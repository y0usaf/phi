//! Built-in overlay extension: registers a simple "about" overlay
//! that demonstrates the extension overlay pipeline.
//!
//! This proves that extensions can register custom overlays that the
//! mux TUI renders on top of the normal chrome. The overlay shows
//! a simple info panel and closes on Esc/Enter/q.

use std::any::Any;
use std::sync::Arc;

use anyhow::Result;
use phi_ext::{
    DrawContext, DrawRect, Extension, ExtensionHost, ExtensionManifest, OverlayKeyCode,
    OverlayOutcome, OverlaySpec, Rgb,
};

/// The built-in overlay extension.
///
/// Registers:
/// - `phi:about` overlay — a simple info panel toggled by pressing
///   `Ctrl+Shift+/` (a.k.a. `Ctrl+?`), showing version info and closing
///   on Esc/Enter/q.
pub struct DefaultOverlayExtension;

/// State for the about overlay.
#[derive(Default)]
struct AboutState {}

impl Extension for DefaultOverlayExtension {
    fn manifest(&self) -> ExtensionManifest {
        ExtensionManifest {
            id: "phi:default-overlay".to_string(),
            name: "Phi Default Overlay".to_string(),
            version: "0.1.0".to_string(),
            description: "Built-in default overlay (about panel)".to_string(),
        }
    }

    fn register(&self, host: &mut dyn ExtensionHost) -> Result<()> {
        host.register_overlay(OverlaySpec::new(
            "phi:about",
            "About Phi — version and build info",
            "ctrl+?", // toggle key: Ctrl+Shift+/ (a.k.a. Ctrl+?)
            // init_state
            Arc::new(|| Box::new(AboutState::default()) as Box<dyn Any + Send + Sync>),
            // render
            Arc::new(
                |ctx: &mut dyn DrawContext, _state: &mut Box<dyn Any + Send + Sync>| {
                    let size = ctx.size();
                    let width = size.cols.min(60).max(20);
                    let height = size.rows.min(12).max(6);
                    let col = (size.cols - width) / 2;
                    let row = (size.rows - height) / 2;
                    let rect = DrawRect::new(col, row, width, height);

                    let border = Rgb(100, 100, 100);
                    let fg = Rgb(220, 220, 220);
                    let bg = Rgb(30, 30, 30);
                    let accent = Rgb(100, 200, 255);

                    ctx.draw_box(rect, fg, bg, border);
                    let inner = rect.inset(2, 1);

                    ctx.put_text_bold(inner.col, inner.row, inner.cols, " Phi ", accent, bg);

                    ctx.put_text(
                        inner.col,
                        inner.row + 2,
                        inner.cols,
                        "An AI coding agent with an extensible TUI.",
                        fg,
                        bg,
                    );
                    ctx.put_text(
                        inner.col,
                        inner.row + 3,
                        inner.cols,
                        "Extensions can register overlays,",
                        fg,
                        bg,
                    );
                    ctx.put_text(
                        inner.col,
                        inner.row + 4,
                        inner.cols,
                        "themes, animations, and more.",
                        fg,
                        bg,
                    );

                    ctx.put_text(
                        inner.col,
                        inner.row + 6,
                        inner.cols,
                        "Press Esc/Enter/q to close.",
                        Rgb(120, 120, 120),
                        bg,
                    );
                },
            ),
            // handle_key
            Arc::new(
                |_state: &mut Box<dyn Any + Send + Sync>,
                 key: &phi_ext::OverlayKey,
                 _cols: u16,
                 _rows: u16| {
                    // Close on Esc, Enter, or q
                    match key.code {
                        OverlayKeyCode::Esc | OverlayKeyCode::Enter => OverlayOutcome::Close,
                        OverlayKeyCode::Char('q') if !key.modifiers.control => {
                            OverlayOutcome::Close
                        }
                        OverlayKeyCode::Char('Q') if !key.modifiers.control => {
                            OverlayOutcome::Close
                        }
                        _ => OverlayOutcome::None,
                    }
                },
            ),
        ))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_ext::{Extension, ExtensionHost, OverlayKeyModifiers};
    use std::collections::BTreeMap;

    // A minimal test host that collects registrations.
    struct TestHost {
        overlays: BTreeMap<String, OverlaySpec>,
    }
    impl Default for TestHost {
        fn default() -> Self {
            Self {
                overlays: BTreeMap::new(),
            }
        }
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
        fn register_overlay(&mut self, overlay: OverlaySpec) -> Result<()> {
            let name = overlay.name.clone();
            self.overlays.insert(name, overlay);
            Ok(())
        }
    }

    #[test]
    fn default_overlay_extension_registers_about_overlay() {
        let mut host = TestHost::default();
        DefaultOverlayExtension.register(&mut host).unwrap();
        let overlay = host
            .overlays
            .get("phi:about")
            .expect("about overlay registered");
        assert_eq!(overlay.name, "phi:about");
        assert_eq!(overlay.toggle_key, "ctrl+?");
        assert!(overlay.description.contains("version"));
    }

    #[test]
    fn about_overlay_handle_key_closes_on_esc() {
        let mut host = TestHost::default();
        DefaultOverlayExtension.register(&mut host).unwrap();
        let overlay = host.overlays.get("phi:about").unwrap();
        let mut state = (overlay.init_state)();

        let esc_key = phi_ext::OverlayKey {
            code: OverlayKeyCode::Esc,
            modifiers: OverlayKeyModifiers::default(),
        };
        let outcome = (overlay.handle_key)(&mut state, &esc_key, 80, 24);
        assert_eq!(outcome, OverlayOutcome::Close);

        let enter_key = phi_ext::OverlayKey {
            code: OverlayKeyCode::Enter,
            modifiers: OverlayKeyModifiers::default(),
        };
        let outcome = (overlay.handle_key)(&mut state, &enter_key, 80, 24);
        assert_eq!(outcome, OverlayOutcome::Close);
    }

    #[test]
    fn about_overlay_handle_key_none_on_random_char() {
        let mut host = TestHost::default();
        DefaultOverlayExtension.register(&mut host).unwrap();
        let overlay = host.overlays.get("phi:about").unwrap();
        let mut state = (overlay.init_state)();

        let key = phi_ext::OverlayKey {
            code: OverlayKeyCode::Char('x'),
            modifiers: OverlayKeyModifiers::default(),
        };
        let outcome = (overlay.handle_key)(&mut state, &key, 80, 24);
        assert_eq!(outcome, OverlayOutcome::None);
    }
}
