//! Credential storage and OAuth flows (Anthropic Pro/Max, OpenAI Codex).

mod anthropic;
mod callback_server;
mod codex;
mod cursor;
mod device_code;
mod pkce;
mod storage;

mod callbacks;
mod error;
mod login;
mod types;
mod util;

pub use callbacks::{LoginCallbacks, LoginMethod, PrintCallbacks};
pub use error::AuthError;
pub use login::{login, refresh};
pub use storage::AuthStorage;
pub use types::{AuthCredential, OAuthCredentials, OAuthProviderId};
pub(crate) use util::parse_authorization_input;
