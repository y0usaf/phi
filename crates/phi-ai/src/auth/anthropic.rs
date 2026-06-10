//! Anthropic OAuth (Claude Pro/Max): authorization code + PKCE.

use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::auth::callback_server::CallbackServer;
use crate::auth::pkce::Pkce;
use crate::auth::{AuthError, LoginCallbacks, OAuthCredentials, parse_authorization_input};
use crate::types::now_ms;

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CALLBACK_PORT: u16 = 53692;
const CALLBACK_PATH: &str = "/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Treat tokens as expired 5 minutes early to absorb clock skew.
const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;

fn redirect_uri() -> String {
    format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}")
}

pub async fn login(
    callbacks: &dyn LoginCallbacks,
    cancel: &CancellationToken,
) -> Result<OAuthCredentials, AuthError> {
    let pkce = Pkce::generate();
    // Upstream uses the PKCE verifier as the OAuth state.
    let state = pkce.verifier.clone();
    let mut server = CallbackServer::start(CALLBACK_PORT, CALLBACK_PATH, state.clone()).await?;

    let mut url = url::Url::parse(AUTHORIZE_URL).expect("authorize url");
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri())
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);
    let url = url.to_string();

    callbacks.on_auth_url(
        &url,
        "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here.",
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
    exchange(json!({
        "grant_type": "authorization_code",
        "client_id": CLIENT_ID,
        "code": code,
        "state": state,
        "redirect_uri": redirect_uri(),
        "code_verifier": pkce.verifier,
    }))
    .await
}

pub async fn refresh(refresh_token: &str) -> Result<OAuthCredentials, AuthError> {
    exchange(json!({
        "grant_type": "refresh_token",
        "client_id": CLIENT_ID,
        "refresh_token": refresh_token,
    }))
    .await
}

async fn exchange(body: serde_json::Value) -> Result<OAuthCredentials, AuthError> {
    let response = reqwest::Client::new()
        .post(TOKEN_URL)
        .timeout(std::time::Duration::from_secs(30))
        .json(&body)
        .send()
        .await?;
    let status = response.status();
    let text = response.text().await?;
    if !status.is_success() {
        return Err(AuthError::Protocol(format!(
            "Anthropic token request failed. status={status}; body={text}"
        )));
    }
    let data: serde_json::Value = serde_json::from_str(&text)?;
    let (Some(access), Some(refresh), Some(expires_in)) = (
        data["access_token"].as_str(),
        data["refresh_token"].as_str(),
        data["expires_in"].as_i64(),
    ) else {
        return Err(AuthError::Protocol(format!(
            "Anthropic token response missing fields: {text}"
        )));
    };
    Ok(OAuthCredentials {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires: now_ms() + expires_in * 1000 - EXPIRY_SKEW_MS,
        account_id: None,
    })
}
