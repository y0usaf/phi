//! Cursor OAuth login: PKCE + browser + poll-based device flow.
//!
//! Unlike Anthropic (redirect-based) and OpenAI (device code), Cursor uses
//! a poll-based flow: generate PKCE + UUID, open the browser, then poll
//! `api2.cursor.sh/auth/poll` until the user completes login.

use std::time::Duration;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::auth::pkce::Pkce;
use crate::auth::{AuthError, LoginCallbacks, OAuthCredentials, open_browser};
use phi_ai_types::now_ms;

const CURSOR_LOGIN_URL: &str = "https://cursor.com/loginDeepControl";
const CURSOR_POLL_URL: &str = "https://api2.cursor.sh/auth/poll";
const CURSOR_REFRESH_URL: &str = "https://api2.cursor.sh/auth/exchange_user_api_key";

const POLL_MAX_ATTEMPTS: u32 = 150;
const POLL_BASE_DELAY: Duration = Duration::from_millis(1000);
const POLL_MAX_DELAY: Duration = Duration::from_millis(10_000);
const POLL_BACKOFF_MULTIPLIER: f64 = 1.2;

/// Treat tokens as expired 5 minutes early to absorb clock skew.
const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;

struct CursorAuthParams {
    verifier: String,
    #[allow(dead_code)]
    challenge: String,
    uuid: String,
    login_url: String,
}

fn generate_auth_params() -> CursorAuthParams {
    let pkce = Pkce::generate();
    let uuid = uuid::Uuid::new_v4().to_string();

    let login_url = format!(
        "{CURSOR_LOGIN_URL}?challenge={}&uuid={}&mode=login&redirectTarget=cli",
        pkce.challenge, uuid,
    );

    CursorAuthParams {
        verifier: pkce.verifier,
        challenge: pkce.challenge,
        uuid,
        login_url,
    }
}

pub async fn login(
    callbacks: &dyn LoginCallbacks,
    cancel: &CancellationToken,
) -> Result<OAuthCredentials, AuthError> {
    let params = generate_auth_params();

    callbacks.on_auth_url(
        &params.login_url,
        "Complete login in your browser. If the browser is on another machine, open the URL manually.",
    );
    let _ = open_browser(&params.login_url);
    callbacks.on_progress("Waiting for Cursor login to complete...");

    let tokens = poll_auth(&params.uuid, &params.verifier, cancel).await?;
    let expires = token_expiry_ms(&tokens.access_token);

    Ok(OAuthCredentials {
        access: tokens.access_token,
        refresh: tokens.refresh_token.unwrap_or_default(),
        expires,
        account_id: None,
    })
}

pub async fn refresh(refresh_token: &str) -> Result<OAuthCredentials, AuthError> {
    let response = reqwest::Client::new()
        .post(CURSOR_REFRESH_URL)
        .timeout(Duration::from_secs(30))
        .header("Authorization", format!("Bearer {refresh_token}"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await?;

    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        return Err(AuthError::Protocol(format!(
            "Cursor token refresh failed ({status}): {text}"
        )));
    }

    let data: PollResponse = serde_json::from_str(&text)?;
    let expires = token_expiry_ms(&data.access_token);

    Ok(OAuthCredentials {
        access: data.access_token,
        refresh: data
            .refresh_token
            .unwrap_or_else(|| refresh_token.to_owned()),
        expires,
        account_id: None,
    })
}

#[derive(Deserialize)]
struct PollResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
}

async fn poll_auth(
    uuid: &str,
    verifier: &str,
    cancel: &CancellationToken,
) -> Result<PollResponse, AuthError> {
    let client = reqwest::Client::new();
    let mut delay = POLL_BASE_DELAY;
    let mut consecutive_errors = 0u32;

    let url = format!("{CURSOR_POLL_URL}?uuid={uuid}&verifier={verifier}");

    for _attempt in 0..POLL_MAX_ATTEMPTS {
        // Check for cancellation before each attempt.
        if cancel.is_cancelled() {
            return Err(AuthError::Cancelled);
        }

        tokio::select! {
            _ = cancel.cancelled() => return Err(AuthError::Cancelled),
            _ = tokio::time::sleep(delay) => {}
        }

        let response = client
            .get(&url)
            .timeout(Duration::from_secs(15))
            .send()
            .await;

        match response {
            Ok(response) => {
                let status = response.status();
                if status.as_u16() == 404 {
                    consecutive_errors = 0;
                    delay = Duration::from_millis(
                        ((delay.as_millis() as f64 * POLL_BACKOFF_MULTIPLIER) as u64)
                            .min(POLL_MAX_DELAY.as_millis() as u64),
                    );
                    continue;
                }
                if status.is_success() {
                    let data: PollResponse = response.json().await?;
                    return Ok(data);
                }
                // Non-404 error status.
                consecutive_errors += 1;
                if consecutive_errors >= 3 {
                    let text = response.text().await.unwrap_or_default();
                    return Err(AuthError::Protocol(format!(
                        "Cursor poll failed ({status}): {text}"
                    )));
                }
            }
            Err(_) => {
                consecutive_errors += 1;
                if consecutive_errors >= 3 {
                    return Err(AuthError::Protocol(
                        "Too many consecutive errors during Cursor auth polling".into(),
                    ));
                }
            }
        }
    }

    Err(AuthError::Protocol(
        "Cursor authentication polling timeout — login did not complete in time".into(),
    ))
}

/// Extract the `exp` claim from a JWT and return the expiry in Unix
/// milliseconds (minus 5min skew). Falls back to now + 1h if parsing fails.
fn token_expiry_ms(token: &str) -> i64 {
    if let Some(exp) = token_expiry_epoch_secs(token) {
        return exp as i64 * 1000 - EXPIRY_SKEW_MS;
    }
    now_ms() + 3600 * 1000
}

fn token_expiry_epoch_secs(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    #[derive(Deserialize)]
    struct JwtClaims {
        exp: Option<u64>,
    }
    serde_json::from_slice::<JwtClaims>(&decoded).ok()?.exp
}

/// Check whether a cursor access token (JWT) will expire within the given
/// threshold (seconds). Used by the cursor provider to proactively refresh.
#[allow(dead_code)]
pub fn token_is_expiring_soon(token: &str, threshold_secs: u64) -> bool {
    let Some(exp) = token_expiry_epoch_secs(token) else {
        return false;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    exp <= now.saturating_add(threshold_secs)
}

/// Compute SHA-256 hex of a string (used by the cursor provider for
/// client-key headers and checksums).
#[allow(dead_code)]
pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    let mut out = String::with_capacity(result.len() * 2);
    for byte in result {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_expiry_parses_valid_jwt() {
        // A JWT with exp=9999999999 (year 2286)
        // Header: {"alg":"HS256","typ":"JWT"}
        // Payload: {"exp":9999999999}
        // Signature: "sig" (can be anything for our parsing)
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJleHAiOjk5OTk5OTk5OTl9.sig";
        let exp = token_expiry_epoch_secs(token);
        assert_eq!(exp, Some(9999999999));
    }

    #[test]
    fn token_expiry_returns_none_for_non_jwt() {
        let token = "not-a-jwt";
        assert_eq!(token_expiry_epoch_secs(token), None);
    }

    #[test]
    fn token_is_not_expiring_for_far_future() {
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJleHAiOjk5OTk5OTk5OTl9.sig";
        assert!(!token_is_expiring_soon(token, 300));
    }

    #[test]
    fn sha256_hex_matches_known_value() {
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn generate_auth_params_produces_valid_urls() {
        let params = generate_auth_params();
        assert!(
            params
                .login_url
                .starts_with("https://cursor.com/loginDeepControl?")
        );
        assert!(params.login_url.contains("challenge="));
        assert!(params.login_url.contains("uuid="));
        assert!(params.login_url.contains("mode=login"));
        assert!(params.login_url.contains("redirectTarget=cli"));
        assert!(!params.verifier.is_empty());
        assert!(!params.uuid.is_empty());
    }
}
