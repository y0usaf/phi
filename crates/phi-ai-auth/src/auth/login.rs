//! Top-level login/refresh dispatch.

use tokio_util::sync::CancellationToken;

use crate::auth::callbacks::{LoginCallbacks, LoginMethod};
use crate::auth::error::AuthError;
use crate::auth::types::{OAuthCredentials, OAuthProviderId};

pub async fn login(
    provider: OAuthProviderId,
    method: LoginMethod,
    callbacks: &dyn LoginCallbacks,
    cancel: &CancellationToken,
) -> Result<OAuthCredentials, AuthError> {
    match (provider, method) {
        (OAuthProviderId::Anthropic, _) => crate::auth::anthropic::login(callbacks, cancel).await,
        (OAuthProviderId::OpenAiCodex, LoginMethod::Browser) => {
            crate::auth::codex::login_browser(callbacks, cancel).await
        }
        (OAuthProviderId::OpenAiCodex, LoginMethod::DeviceCode) => {
            crate::auth::codex::login_device_code(callbacks, cancel).await
        }
        (OAuthProviderId::Cursor, _) => crate::auth::cursor::login(callbacks, cancel).await,
    }
}

pub async fn refresh(
    provider: OAuthProviderId,
    refresh_token: &str,
) -> Result<OAuthCredentials, AuthError> {
    match provider {
        OAuthProviderId::Anthropic => crate::auth::anthropic::refresh(refresh_token).await,
        OAuthProviderId::OpenAiCodex => crate::auth::codex::refresh(refresh_token).await,
        OAuthProviderId::Cursor => crate::auth::cursor::refresh(refresh_token).await,
    }
}
