//! Built-in overlay extension: the mux's usage table, model picker, and
//! archived-session browser — the overlays that need host-side data
//! (`phi_session` usage reports, model registry, archived sessions) and
//! can't be expressed in Lua.
//!
//! The `phi:about` and `phi:help` overlays are Lua defaults
//! (`crates/phi-builtins/defaults/about_overlay.lua`,
//! `help_overlay.lua`) — they're pure rendering and serve as the tutorial
//! overlays a user reads to learn the extension API. This extension keeps
//! only the data-dependent ones.
//!
//! Per The Rule (PLAN.md): these are extension-shaped features, so they
//! live here and go through `phi_ext::OverlaySpec`. The host opens them
//! by name (`phi_ext::OVERLAY_USAGE` / `OVERLAY_MODEL` / `OVERLAY_ARCHIVE`)
//! with a typed payload.

mod archive;
mod dialog;
mod model;
mod usage;

use anyhow::Result;
use phi_ext::{Extension, ExtensionHost, ExtensionManifest};

/// The built-in overlay extension (data-dependent overlays only).
///
/// Registers:
/// - `phi:usage` — usage totals table (opened via `:usage` / `/usage`).
/// - `phi:model` — model picker (opened with a [`phi_ext::ModelPickerPayload`]).
/// - `phi:archive` — archived-session browser/restorer (opened via
///   `:archive`).
///
/// `phi:about` and `phi:help` are Lua defaults
/// (`crates/phi-builtins/defaults/`).
pub struct DefaultOverlayExtension;

impl Extension for DefaultOverlayExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:default-overlay".to_string(),
            name: "Phi Default Overlay".to_string(),
            version: "0.1.0".to_string(),
            description: "Built-in overlays: usage, model picker, archive".to_string(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        host.register_overlay(usage::spec())?;
        host.register_overlay(model::spec())?;
        host.register_overlay(archive::spec())?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_host {
    use super::*;
    use phi_ext::OverlaySpec;
    use std::collections::BTreeMap;

    // A minimal test host that collects registrations.
    #[derive(Default)]
    pub(crate) struct TestHost {
        pub(crate) overlays: BTreeMap<String, OverlaySpec>,
    }

    impl ExtensionHost for TestHost {
        fn register_tool(&mut self, _t: phi_ext::ToolSpec) -> Result<()> {
            Ok(())
        }
        fn register_command(&mut self, _c: phi_ext::CommandSpec) -> Result<()> {
            Ok(())
        }
        fn register_shortcut(&mut self, _s: phi_ext::ShortcutSpec) -> Result<()> {
            Ok(())
        }
        fn register_flag(&mut self, _f: phi_ext::FlagSpec) -> Result<()> {
            Ok(())
        }
        fn subscribe(&mut self, _h: phi_ext::EventHandlerRegistration) -> Result<()> {
            Ok(())
        }
        fn register_message_renderer(&mut self, _r: phi_ext::MessageRendererSpec) -> Result<()> {
            Ok(())
        }
        fn register_tool_renderer(&mut self, _s: phi_ext::ToolRenderSpec) -> Result<()> {
            Ok(())
        }
        fn register_provider(&mut self, _c: phi_ext::ProviderConfig) -> Result<()> {
            Ok(())
        }
        fn unregister_provider(&mut self, _n: &str) -> Result<()> {
            Ok(())
        }
        fn register_theme(&mut self, _t: phi_ext::ThemeSpec) -> Result<()> {
            Ok(())
        }
        fn register_spinner(&mut self, _s: phi_ext::SpinnerSpec) -> Result<()> {
            Ok(())
        }
        fn register_thinking_expression(
            &mut self,
            _e: phi_ext::ThinkingExpressionSpec,
        ) -> Result<()> {
            Ok(())
        }
        fn register_text_effect(&mut self, _e: phi_ext::TextEffectSpec) -> Result<()> {
            Ok(())
        }
        fn register_style(&mut self, _s: phi_ext::StyleSpec) -> Result<()> {
            Ok(())
        }
        fn register_overlay(&mut self, overlay: OverlaySpec) -> Result<()> {
            let name = overlay.name.clone();
            self.overlays.insert(name, overlay);
            Ok(())
        }
    }

    pub(crate) fn registered_overlays() -> BTreeMap<String, OverlaySpec> {
        let mut host = TestHost::default();
        DefaultOverlayExtension.register(&mut host).unwrap();
        host.overlays
    }
}

#[cfg(test)]
mod tests {
    use super::test_host::registered_overlays;
    use phi_ext::{OVERLAY_ARCHIVE, OVERLAY_MODEL, OVERLAY_USAGE};

    #[test]
    fn registers_data_dependent_overlays() {
        let overlays = registered_overlays();
        assert!(overlays.contains_key(OVERLAY_USAGE));
        assert!(overlays.contains_key(OVERLAY_MODEL));
        assert!(overlays.contains_key(OVERLAY_ARCHIVE));
        // about and help are Lua defaults now — not registered here.
        assert!(!overlays.contains_key("phi:about"));
        assert!(!overlays.contains_key(phi_ext::OVERLAY_HELP));
    }

    #[test]
    fn data_overlays_have_no_toggle_key() {
        let overlays = registered_overlays();
        assert_eq!(overlays[OVERLAY_USAGE].toggle_key, None);
        assert_eq!(overlays[OVERLAY_MODEL].toggle_key, None);
        assert_eq!(overlays[OVERLAY_ARCHIVE].toggle_key, None);
    }
}
