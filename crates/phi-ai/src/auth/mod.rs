//! Credential storage and OAuth flows (Anthropic Pro/Max, OpenAI Codex).

pub mod anthropic;
mod callback_server;
pub mod codex;
mod device_code;
mod pkce;
mod storage;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

pub use storage::AuthStorage;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Protocol(String),
    #[error("login cancelled")]
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub access: String,
    pub refresh: String,
    /// Unix milliseconds at which the access token should be considered expired.
    pub expires: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthCredential {
    ApiKey { key: String },
    Oauth(OAuthCredentials),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OAuthProviderId {
    Anthropic,
    OpenAiCodex,
}

impl OAuthProviderId {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "openai" | "openai-codex" => Some(Self::OpenAiCodex),
            _ => None,
        }
    }

    /// Key under which credentials are stored in auth.json (matches the
    /// model `provider` field).
    pub fn storage_key(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCodex => "openai",
        }
    }
}

/// UI hooks for login flows. Frontends print/show; headless callers can rely
/// on the callback server alone.
pub trait LoginCallbacks: Send + Sync {
    fn on_auth_url(&self, _url: &str, _instructions: &str) {}
    fn on_device_code(&self, _user_code: &str, _verification_uri: &str, _expires_in: Duration) {}
    fn on_progress(&self, _message: &str) {}
    /// Resolves when the user pastes a code/redirect URL manually. The
    /// default never resolves, leaving only the callback-server path.
    fn manual_code(&self) -> std::pin::Pin<Box<dyn Future<Output = Option<String>> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

/// Print-to-stdout callbacks for headless `--login`.
pub struct PrintCallbacks;

impl LoginCallbacks for PrintCallbacks {
    fn on_auth_url(&self, url: &str, instructions: &str) {
        println!("{instructions}\n\n  {url}\n");
    }

    fn on_device_code(&self, user_code: &str, verification_uri: &str, expires_in: Duration) {
        println!(
            "Visit {verification_uri} and enter code: {user_code} (expires in {} minutes)",
            expires_in.as_secs() / 60
        );
    }

    fn on_progress(&self, message: &str) {
        println!("{message}");
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LoginMethod {
    #[default]
    Browser,
    DeviceCode,
}

pub async fn login(
    provider: OAuthProviderId,
    method: LoginMethod,
    callbacks: &dyn LoginCallbacks,
    cancel: &CancellationToken,
) -> Result<OAuthCredentials, AuthError> {
    match (provider, method) {
        (OAuthProviderId::Anthropic, _) => anthropic::login(callbacks, cancel).await,
        (OAuthProviderId::OpenAiCodex, LoginMethod::Browser) => {
            codex::login_browser(callbacks, cancel).await
        }
        (OAuthProviderId::OpenAiCodex, LoginMethod::DeviceCode) => {
            codex::login_device_code(callbacks, cancel).await
        }
    }
}

pub async fn refresh(
    provider: OAuthProviderId,
    refresh_token: &str,
) -> Result<OAuthCredentials, AuthError> {
    match provider {
        OAuthProviderId::Anthropic => anthropic::refresh(refresh_token).await,
        OAuthProviderId::OpenAiCodex => codex::refresh(refresh_token).await,
    }
}

/// Accepts a raw code, a full redirect URL, `code#state`, or a query string.
pub(crate) fn parse_authorization_input(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    if let Ok(url) = url::Url::parse(value) {
        let mut code = None;
        let mut state = None;
        for (key, val) in url.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(val.into_owned()),
                "state" => state = Some(val.into_owned()),
                _ => {}
            }
        }
        return (code, state);
    }
    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_owned()), Some(state.to_owned()));
    }
    if value.contains("code=") {
        let mut code = None;
        let mut state = None;
        for pair in value.split('&') {
            if let Some((key, val)) = pair.split_once('=') {
                match key {
                    "code" => code = Some(val.to_owned()),
                    "state" => state = Some(val.to_owned()),
                    _ => {}
                }
            }
        }
        return (code, state);
    }
    (Some(value.to_owned()), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_authorization_inputs() {
        assert_eq!(
            parse_authorization_input("http://localhost:1455/auth/callback?code=abc&state=xyz"),
            (Some("abc".into()), Some("xyz".into()))
        );
        assert_eq!(
            parse_authorization_input("abc#xyz"),
            (Some("abc".into()), Some("xyz".into()))
        );
        assert_eq!(
            parse_authorization_input("code=abc&state=xyz"),
            (Some("abc".into()), Some("xyz".into()))
        );
        assert_eq!(parse_authorization_input("abc"), (Some("abc".into()), None));
        assert_eq!(parse_authorization_input("  "), (None, None));
    }
}
