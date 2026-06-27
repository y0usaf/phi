//! OpenAI Codex (ChatGPT) OAuth: browser PKCE flow and device-code flow.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth::callback_server::CallbackServer;
use crate::auth::device_code::{PollOutcome, poll_device_code};
use crate::auth::pkce::{Pkce, random_state};
use crate::auth::{AuthError, LoginCallbacks, OAuthCredentials, parse_authorization_input};
use phi_ai_types::now_ms;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL: &str = "https://auth.openai.com";
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const SCOPE: &str = "openid profile email offline_access";
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";
const DEVICE_CODE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

fn redirect_uri() -> String {
    format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}")
}

pub async fn login_browser(
    callbacks: &dyn LoginCallbacks,
    cancel: &CancellationToken,
) -> Result<OAuthCredentials, AuthError> {
    let pkce = Pkce::generate();
    let state = random_state();
    let mut server = CallbackServer::start(CALLBACK_PORT, CALLBACK_PATH, state.clone()).await?;

    let mut url = url::Url::parse(&format!("{AUTH_BASE_URL}/oauth/authorize"))
        .map_err(|e| AuthError::Protocol(format!("invalid authorize url: {e}")))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri())
        .append_pair("scope", SCOPE)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "phi");
    let url = url.to_string();

    callbacks.on_auth_url(
        &url,
        "A browser window should open. Complete login to finish.",
    );
    let _ = webbrowser::open(&url);

    let code = tokio::select! {
        code = server.wait(cancel) => match code {
            Some(code) => code,
            None => return Err(AuthError::Cancelled),
        },
        manual = callbacks.manual_code() => {
            let input = manual.ok_or(AuthError::Cancelled)?;
            let (code, manual_state) = parse_authorization_input(&input);
            if let Some(manual_state) = manual_state
                && manual_state != state
            {
                return Err(AuthError::Protocol("OAuth state mismatch".into()));
            }
            code.ok_or_else(|| AuthError::Protocol("Missing authorization code".into()))?
        }
    };

    callbacks.on_progress("Exchanging authorization code for tokens...");
    exchange_code(&code, &pkce.verifier, &redirect_uri()).await
}

pub async fn login_device_code(
    callbacks: &dyn LoginCallbacks,
    cancel: &CancellationToken,
) -> Result<OAuthCredentials, AuthError> {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{AUTH_BASE_URL}/api/accounts/deviceauth/usercode"))
        .json(&json!({ "client_id": CLIENT_ID }))
        .send()
        .await?;
    if response.status().as_u16() == 404 {
        return Err(AuthError::Protocol(
            "OpenAI Codex device code login is not enabled for this server. Use browser login."
                .into(),
        ));
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Protocol(format!(
            "OpenAI Codex device code request failed with status {status}: {body}"
        )));
    }
    let data: Value = response.json().await?;
    let (Some(device_auth_id), Some(user_code)) =
        (data["device_auth_id"].as_str(), data["user_code"].as_str())
    else {
        return Err(AuthError::Protocol(format!(
            "Invalid OpenAI Codex device code response: {data}"
        )));
    };
    let device_auth_id = device_auth_id.to_owned();
    let user_code = user_code.to_owned();
    let interval = data["interval"]
        .as_f64()
        .or_else(|| {
            data["interval"]
                .as_str()
                .and_then(|s| s.trim().parse().ok())
        })
        .map(Duration::from_secs_f64);

    callbacks.on_device_code(
        &user_code,
        &format!("{AUTH_BASE_URL}/codex/device"),
        DEVICE_CODE_TIMEOUT,
    );

    let token_url = format!("{AUTH_BASE_URL}/api/accounts/deviceauth/token");
    let (authorization_code, code_verifier) =
        poll_device_code(interval, DEVICE_CODE_TIMEOUT, cancel, || {
            let client = client.clone();
            let token_url = token_url.clone();
            let device_auth_id = device_auth_id.clone();
            let user_code = user_code.clone();
            async move {
                let response = client
                    .post(&token_url)
                    .json(&json!({
                        "device_auth_id": device_auth_id,
                        "user_code": user_code,
                    }))
                    .send()
                    .await?;
                let status = response.status();
                if status.is_success() {
                    let data: Value = response.json().await?;
                    let (Some(code), Some(verifier)) = (
                        data["authorization_code"].as_str(),
                        data["code_verifier"].as_str(),
                    ) else {
                        return Ok(PollOutcome::Failed(format!(
                            "Invalid OpenAI Codex device auth token response: {data}"
                        )));
                    };
                    return Ok(PollOutcome::Complete((
                        code.to_owned(),
                        verifier.to_owned(),
                    )));
                }
                if status.as_u16() == 403 || status.as_u16() == 404 {
                    return Ok(PollOutcome::Pending);
                }
                let body = response.text().await.unwrap_or_default();
                let error_code = serde_json::from_str::<Value>(&body)
                    .ok()
                    .map(|v| match &v["error"] {
                        Value::String(s) => s.clone(),
                        other => other["code"].as_str().unwrap_or_default().to_owned(),
                    })
                    .unwrap_or_default();
                match error_code.as_str() {
                    "deviceauth_authorization_pending" => Ok(PollOutcome::Pending),
                    "slow_down" => Ok(PollOutcome::SlowDown),
                    _ => Ok(PollOutcome::Failed(format!(
                        "OpenAI Codex device auth failed with status {status}: {body}"
                    ))),
                }
            }
        })
        .await?;

    exchange_code(
        &authorization_code,
        &code_verifier,
        &format!("{AUTH_BASE_URL}/deviceauth/callback"),
    )
    .await
}

pub async fn refresh(refresh_token: &str) -> Result<OAuthCredentials, AuthError> {
    token_request(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ])
    .await
}

async fn exchange_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthCredentials, AuthError> {
    token_request(&[
        ("grant_type", "authorization_code"),
        ("client_id", CLIENT_ID),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
    ])
    .await
}

async fn token_request(form: &[(&str, &str)]) -> Result<OAuthCredentials, AuthError> {
    let response = reqwest::Client::new()
        .post(format!("{AUTH_BASE_URL}/oauth/token"))
        .timeout(Duration::from_secs(30))
        .form(form)
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        return Err(AuthError::Protocol(format!(
            "OpenAI Codex token request failed ({status}): {text}"
        )));
    }
    let data: Value = serde_json::from_str(&text)?;
    let (Some(access), Some(refresh), Some(expires_in)) = (
        data["access_token"].as_str(),
        data["refresh_token"].as_str(),
        data["expires_in"].as_i64(),
    ) else {
        return Err(AuthError::Protocol(format!(
            "OpenAI Codex token response missing fields: {text}"
        )));
    };
    let account_id = account_id_from_jwt(access)
        .ok_or_else(|| AuthError::Protocol("Failed to extract accountId from token".into()))?;
    Ok(OAuthCredentials {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires: now_ms() + expires_in * 1000,
        account_id: Some(account_id),
    })
}

/// Decode (without verification) the JWT and pull the ChatGPT account id.
pub fn account_id_from_jwt(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let value: Value = serde_json::from_slice(&decoded).ok()?;
    let account_id = value[JWT_CLAIM_PATH]["chatgpt_account_id"].as_str()?;
    (!account_id.is_empty()).then(|| account_id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_account_id() {
        let payload = json!({ JWT_CLAIM_PATH: { "chatgpt_account_id": "acct_123" } });
        let token = format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(b"{}"),
            URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes()),
            URL_SAFE_NO_PAD.encode(b"sig"),
        );
        assert_eq!(account_id_from_jwt(&token).as_deref(), Some("acct_123"));
        assert_eq!(account_id_from_jwt("not-a-jwt"), None);
    }
}
