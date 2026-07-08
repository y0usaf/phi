//! `core/` — mirrors the spec's `src/core/`. Modules land with their
//! workstreams (WS2.6: auth + models; WS3: sessions/settings; WS7:
//! resource loading).

pub mod auth_guidance;
pub mod auth_storage;
pub mod defaults;
pub mod http_dispatcher;
pub mod model_registry;
pub mod model_resolver;
pub mod resolve_config_value;
pub mod settings_manager;
