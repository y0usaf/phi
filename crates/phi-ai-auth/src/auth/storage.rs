//! `auth.json` credential storage: 0600 permissions, fd-lock for
//! cross-process safety (concurrent instances refreshing the same token).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

use crate::auth::{
    AuthCredential, AuthError, LoginCallbacks, LoginMethod, OAuthCredentials, OAuthProviderId,
};
use phi_ai_types::{Auth, now_ms};

type AuthData = BTreeMap<String, AuthCredential>;

pub struct AuthStorage {
    path: PathBuf,
    runtime: Mutex<BTreeMap<String, String>>,
}

impl AuthStorage {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            runtime: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Runtime override, never persisted.
    pub fn set_runtime_api_key(&self, provider: &str, key: &str) {
        self.runtime
            .lock()
            .unwrap()
            .insert(provider.to_owned(), key.to_owned());
    }

    fn with_lock<T>(
        &self,
        f: impl FnOnce(&mut AuthData) -> Result<(T, bool), AuthError>,
    ) -> Result<T, AuthError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&self.path)?;
        let mut lock = fd_lock::RwLock::new(file);
        let mut guard = lock.write()?;
        let mut content = String::new();
        guard.read_to_string(&mut content)?;
        let mut data: AuthData = if content.trim().is_empty() {
            AuthData::new()
        } else {
            serde_json::from_str(&content)?
        };
        let (result, dirty) = f(&mut data)?;
        if dirty {
            let serialized = serde_json::to_string_pretty(&data)?;
            // Atomic write: write to a temp file then rename over the
            // original. A crash between truncate and write completion would
            // otherwise leave auth.json empty (total credential loss).
            let temp_path = self.path.with_extension("json.tmp");
            {
                let mut temp = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&temp_path)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    temp.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
                temp.write_all(serialized.as_bytes())?;
                temp.flush()?;
            }
            // Drop the file lock/guard before the rename since the guard
            // holds the original fd, which rename replaces.
            drop(guard);
            std::fs::rename(&temp_path, &self.path)?;
        }
        Ok(result)
    }

    pub fn get(&self, provider: &str) -> Result<Option<AuthCredential>, AuthError> {
        self.with_lock(|data| Ok((data.get(provider).cloned(), false)))
    }

    pub fn set(&self, provider: &str, credential: AuthCredential) -> Result<(), AuthError> {
        self.with_lock(|data| {
            data.insert(provider.to_owned(), credential);
            Ok(((), true))
        })
    }

    pub fn remove(&self, provider: &str) -> Result<(), AuthError> {
        self.with_lock(|data| {
            data.remove(provider);
            Ok(((), true))
        })
    }

    pub fn list(&self) -> Result<Vec<String>, AuthError> {
        self.with_lock(|data| Ok((data.keys().cloned().collect(), false)))
    }

    pub async fn login(
        &self,
        provider: OAuthProviderId,
        method: LoginMethod,
        callbacks: &dyn LoginCallbacks,
        cancel: &CancellationToken,
    ) -> Result<(), AuthError> {
        let credentials = crate::auth::login(provider, method, callbacks, cancel).await?;
        self.set(provider.storage_key(), AuthCredential::Oauth(credentials))
    }

    pub fn logout(&self, provider: &str) -> Result<(), AuthError> {
        self.remove(provider)
    }

    /// Resolve auth for a provider. Priority: runtime override > stored
    /// api_key > stored oauth (auto-refresh under the file lock) > env.
    pub async fn resolve(&self, provider: &str) -> Result<Option<Auth>, AuthError> {
        if let Some(key) = self.runtime.lock().unwrap().get(provider) {
            return Ok(Some(Auth::ApiKey(key.clone())));
        }

        match self.get(provider)? {
            Some(AuthCredential::ApiKey { key }) => return Ok(Some(Auth::ApiKey(key))),
            Some(AuthCredential::Oauth(credentials)) => {
                let Some(oauth_provider) = OAuthProviderId::parse(provider) else {
                    return Ok(None);
                };
                let credentials = if now_ms() >= credentials.expires {
                    self.refresh_locked(provider, oauth_provider).await?
                } else {
                    credentials
                };
                return Ok(Some(auth_from_credentials(oauth_provider, credentials)));
            }
            None => {}
        }

        Ok(env_auth(provider))
    }

    /// Refresh under the file lock so concurrent instances don't both burn
    /// the (rotating) refresh token. If another process already refreshed,
    /// its credentials are used as-is.
    async fn refresh_locked(
        &self,
        provider: &str,
        oauth_provider: OAuthProviderId,
    ) -> Result<OAuthCredentials, AuthError> {
        let handle = tokio::runtime::Handle::current();
        let path = self.path.clone();
        let provider = provider.to_owned();
        tokio::task::spawn_blocking(move || {
            let storage = AuthStorage::new(path);
            // Read current credentials under the lock, then release it
            // before calling block_on so the refresh future can schedule
            // work onto the runtime without deadlocking.
            let current = storage.with_lock(|data| {
                let Some(AuthCredential::Oauth(current)) = data.get(&provider).cloned() else {
                    return Err(AuthError::Protocol(format!(
                        "no oauth credentials stored for {provider}"
                    )));
                };
                Ok((current, false))
            })?;
            if now_ms() < current.expires {
                return Ok(current);
            }
            let refreshed =
                handle.block_on(crate::auth::refresh(oauth_provider, &current.refresh))?;
            // Re-acquire the lock to persist the refreshed credentials.
            // If another process already refreshed, use its credentials.
            storage.with_lock(|data| {
                if let Some(AuthCredential::Oauth(existing)) = data.get(&provider).cloned()
                    && now_ms() < existing.expires
                {
                    return Ok((existing, false));
                }
                data.insert(provider.clone(), AuthCredential::Oauth(refreshed.clone()));
                Ok((refreshed, true))
            })
        })
        .await
        .map_err(|e| AuthError::Protocol(format!("refresh task failed: {e}")))?
    }
}

fn auth_from_credentials(provider: OAuthProviderId, credentials: OAuthCredentials) -> Auth {
    match provider {
        OAuthProviderId::Anthropic => Auth::OAuthToken(credentials.access),
        OAuthProviderId::OpenAiCodex => Auth::ChatGptOAuth {
            account_id: credentials.account_id.unwrap_or_default(),
            access: credentials.access,
        },
    }
}

fn env_auth(provider: &str) -> Option<Auth> {
    if provider == "anthropic" {
        if let Ok(token) = std::env::var("ANTHROPIC_OAUTH_TOKEN")
            && !token.is_empty()
        {
            return Some(Auth::OAuthToken(token));
        }
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY")
            && !key.is_empty()
        {
            return Some(Auth::ApiKey(key));
        }
        if let Ok(key) = std::env::var("PHI_API_KEY")
            && !key.is_empty()
        {
            return Some(Auth::ApiKey(key));
        }
        return None;
    }
    if provider == "vercel-ai-gateway" {
        return std::env::var("AI_GATEWAY_API_KEY")
            .ok()
            .filter(|key| !key.is_empty())
            .map(Auth::ApiKey);
    }
    let env_name = format!(
        "{}_API_KEY",
        provider.to_uppercase().replace(['-', '.'], "_")
    );
    if let Ok(key) = std::env::var(env_name)
        && !key.is_empty()
    {
        return Some(Auth::ApiKey(key));
    }
    // Universal fallback for any provider.
    if let Ok(key) = std::env::var("PHI_API_KEY")
        && !key.is_empty()
    {
        return Some(Auth::ApiKey(key));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_storage() -> (AuthStorage, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (AuthStorage::new(dir.path().join("auth.json")), dir)
    }

    #[test]
    fn roundtrips_credentials() {
        let (storage, _dir) = temp_storage();
        storage
            .set(
                "anthropic",
                AuthCredential::Oauth(OAuthCredentials {
                    access: "a".into(),
                    refresh: "r".into(),
                    expires: 123,
                    account_id: None,
                }),
            )
            .unwrap();
        storage
            .set("openai", AuthCredential::ApiKey { key: "sk-x".into() })
            .unwrap();

        assert_eq!(storage.list().unwrap(), vec!["anthropic", "openai"]);
        match storage.get("anthropic").unwrap() {
            Some(AuthCredential::Oauth(c)) => assert_eq!(c.expires, 123),
            other => panic!("unexpected: {other:?}"),
        }
        storage.remove("anthropic").unwrap();
        assert!(storage.get("anthropic").unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn auth_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (storage, _dir) = temp_storage();
        storage
            .set("openai", AuthCredential::ApiKey { key: "k".into() })
            .unwrap();
        let mode = std::fs::metadata(storage.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn runtime_override_wins() {
        let (storage, _dir) = temp_storage();
        storage
            .set(
                "openai",
                AuthCredential::ApiKey {
                    key: "stored".into(),
                },
            )
            .unwrap();
        storage.set_runtime_api_key("openai", "runtime");
        match storage.resolve("openai").await.unwrap() {
            Some(Auth::ApiKey(key)) => assert_eq!(key, "runtime"),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
