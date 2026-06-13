//! System clipboard glue (port of pi-harness backend clipboard code):
//! lazily-initialized arboard handle, writing both Clipboard and Primary on
//! Linux so mouse-selection copy behaves like a terminal.

use arboard::Clipboard;

#[derive(Default)]
pub struct MuxClipboard {
    clipboard: Option<Clipboard>,
}

impl MuxClipboard {
    fn handle(&mut self) -> Option<&mut Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }

    /// Returns an error note when the clipboard is unavailable.
    pub fn copy_text(&mut self, text: &str) -> Result<(), &'static str> {
        let Some(clipboard) = self.handle() else {
            return Err("clipboard unavailable");
        };

        #[cfg(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        ))]
        {
            use arboard::{LinuxClipboardKind, SetExtLinux};
            if clipboard
                .set()
                .clipboard(LinuxClipboardKind::Clipboard)
                .text(text)
                .is_err()
            {
                return Err("clipboard unavailable");
            }
            let _ = clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text);
            Ok(())
        }

        #[cfg(not(all(
            unix,
            not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
        )))]
        {
            clipboard
                .set_text(text)
                .map_err(|_| "clipboard unavailable")
        }
    }

    pub fn read_text(&mut self) -> Option<String> {
        self.handle()?
            .get_text()
            .ok()
            .filter(|text| !text.is_empty())
    }
}
