//! Credential data models and provider identifiers.

use serde::{Deserialize, Serialize};

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
    Cursor,
}

impl OAuthProviderId {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "openai" | "openai-codex" => Some(Self::OpenAiCodex),
            "cursor" => Some(Self::Cursor),
            _ => None,
        }
    }

    /// Key under which credentials are stored in auth.json (matches the
    /// model `provider` field).
    pub fn storage_key(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAiCodex => "openai",
            Self::Cursor => "cursor",
        }
    }
}
