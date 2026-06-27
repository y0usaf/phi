use super::*;


impl Ui {
    pub(super) fn handle_popup_key(&mut self, key: phi_tui::crossterm::event::KeyEvent) -> PopupOutcome {
        let Some(popup) = &mut self.popup else {
            return PopupOutcome::Pass;
        };
        match key.code {
            KeyCode::Esc => {
                self.popup = None;
                PopupOutcome::Handled
            }
            KeyCode::Up | KeyCode::BackTab => {
                popup.selected = popup
                    .selected
                    .checked_sub(1)
                    .unwrap_or(popup.items.len() - 1);
                PopupOutcome::Handled
            }
            KeyCode::Down => {
                popup.selected = (popup.selected + 1) % popup.items.len();
                PopupOutcome::Handled
            }
            KeyCode::Tab => {
                self.apply_completion();
                PopupOutcome::Handled
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
                let kind = popup.kind;
                self.apply_completion();
                match kind {
                    PopupKind::Command => PopupOutcome::Pass,
                    PopupKind::Argument | PopupKind::File => {
                        PopupOutcome::Handled
                    }
                }
            }
            _ => PopupOutcome::Pass,
        }
    }

    pub(super) fn apply_completion(&mut self) {
        let Some(popup) = self.popup.take() else {
            return;
        };
        let item = &popup.items[popup.selected];
        // Directory completions stay open for further completion (pi-style);
        // file completions get a trailing space to end the token.
        let is_dir = item.value.trim_end_matches('"').ends_with('/');
        let replacement = match popup.kind {
            PopupKind::Command => format!("/{} ", item.value),
            PopupKind::Argument => item.value.clone(),
            PopupKind::File if !is_dir => format!("{} ", item.value),
            PopupKind::File => item.value.clone(),
        };
        let kind = popup.kind;
        self.editor.replace_prefix(&popup.prefix, &replacement);
        if is_dir && kind == PopupKind::File {
            self.refresh_popup();
        }
    }

    pub(super) fn refresh_popup(&mut self) {
        self.popup = None;
        if self.login.is_some() {
            return;
        }
        let before = self.editor.before_cursor().to_owned();
        if self.editor.cursor_row() == 0
            && let Some(popup) = complete(
                &before,
                &self.model_names,
                &self.runtime.list_extension_commands(),
            )
        {
            self.popup = Some(popup);
            return;
        }
        let Some(token) = extract_at_token(&before) else {
            return;
        };
        let query = token.trim_start_matches('@').trim_start_matches('"');
        let items: Vec<Suggestion> = self
            .files
            .search(query)
            .into_iter()
            .map(|c| Suggestion {
                value: c.value,
                label: c.label,
                desc: c.desc,
            })
            .collect();
        if !items.is_empty() {
            self.popup = Some(Popup {
                items,
                selected: 0,
                prefix: token,
                kind: PopupKind::File,
            });
        }
    }

    pub(super) fn restore_queued(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        let mut text = self.queued.join("\n");
        let draft = self.editor.text();
        if !draft.trim().is_empty() {
            text.push('\n');
            text.push_str(&draft);
        }
        self.queued.clear();
        self.editor.set_text(&text);
    }
}
