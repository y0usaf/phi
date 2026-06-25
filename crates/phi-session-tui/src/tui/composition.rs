use super::*;

fn git_branch(cwd: &std::path::Path) -> Option<String> {
    let dotgit = cwd.join(".git");
    let git_dir = if dotgit.is_file() {
        let redirect = std::fs::read_to_string(&dotgit).ok()?;
        let path = redirect.trim().strip_prefix("gitdir: ")?.to_owned();
        let path = std::path::PathBuf::from(path);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    } else {
        dotgit
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: ") {
        Some(reference) => Some(reference.rsplit('/').next()?.to_owned()),
        None => Some(head.get(..8)?.to_owned()),
    }
}


impl Ui {
    pub(super) fn draw(&mut self) -> std::io::Result<()> {
        self.flush_pending_to_transcript();

        let (chrome, chrome_cursor) = self.chrome_lines();
        let transcript_height = self.term.height().saturating_sub(chrome.len()).max(1);
        let live_blocks = self.current_live_transcript_blocks();

        let mut specs = Vec::with_capacity(self.transcript_blocks.len() + live_blocks.len());
        for block in &self.transcript_blocks {
            specs.push(BlockSpec {
                key: &block.key,
                lines: &block.lines,
                finalized: true,
            });
        }
        for block in &live_blocks {
            specs.push(BlockSpec {
                key: &block.key,
                lines: &block.lines,
                finalized: block.finalized,
            });
        }

        let frame = self
            .transcript
            .render(transcript_height, &specs);

        let transcript_live_len = frame.live.len();
        let mut live = frame.live;
        live.extend(chrome);
        let cursor = chrome_cursor.map(|mut cursor| {
            cursor.line += transcript_live_len;
            cursor
        });
        self.term.render(&frame.scrollback, &live, cursor)
    }

    pub(super) fn flush_pending_to_transcript(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        self.transcript_blocks.push(TranscriptBlock {
            key: format!("tx:{}", self.next_transcript_block_id),
            lines: pending,
            finalized: true,
        });
        self.next_transcript_block_id += 1;
    }

    pub(super) fn clear_transcript(&mut self) {
        self.transcript.reset();
        self.transcript_blocks.clear();
        self.next_transcript_block_id = 0;
        self.pending.clear();
        self.block.clear();
        self.after_block.clear();
        self.last_activity = false;
    }

    pub(super) fn current_live_transcript_blocks(&self) -> Vec<TranscriptBlock> {
        let mut blocks = Vec::new();

        if self.running.is_some() {
            let thinking = self.live_thinking_lines();
            if !thinking.is_empty() {
                blocks.push(TranscriptBlock {
                    key: "live:thinking".to_owned(),
                    lines: thinking,
                    finalized: false,
                });
            }

            if self.block_active {
                blocks.push(TranscriptBlock {
                    key: "live:repl-block".to_owned(),
                    lines: render_repl_block(&self.block, Some(self.turn)),
                    finalized: false,
                });

                if !self.after_block.is_empty() {
                    blocks.push(TranscriptBlock {
                        key: "live:after-block".to_owned(),
                        lines: self.after_block.clone(),
                        finalized: true,
                    });
                }
            }

            let visible = self.stream[..first_block_start(&self.stream)].trim();
            if !visible.is_empty() {
                blocks.push(TranscriptBlock {
                    key: "live:stream-prose".to_owned(),
                    lines: self.assistant_card_lines(visible),
                    finalized: false,
                });
            }

            if let Some(block) = live_block_body(&self.stream) {
                blocks.push(TranscriptBlock {
                    key: "live:preview-repl".to_owned(),
                    lines: render_repl_block(
                        &self.repl_code_lines(0, block, self.code_limit()),
                        None,
                    ),
                    finalized: false,
                });
            }
        }

        blocks
    }

    pub(super) fn render_chrome_agent(&self) -> String {
        let active = self.usage.total_tokens > 0;
        let mut result = String::with_capacity(64);

        let agent_str = format!("{} PHI", agent_symbol());
        result.push_str(&format!("{}{agent_str}{RESET}", &self.palette.chrome_agent_fg));

        if active {
            let tokens = format_tokens(self.usage.total_tokens);
            result.push_str(&format!(" {}{tokens}{RESET}", &self.palette.chrome_agent_fg));
        }

        if active && self.usage.cost.total > 0.0 {
            result.push_str(&format!(
                " {}{}{:.2}{RESET}",
                &self.palette.chrome_cost_fg,
                cost_symbol(),
                self.usage.cost.total
            ));
        }

        result
    }

    pub(super) fn render_chrome_model(&self) -> String {
        let mut result = String::with_capacity(64);

        let model_id = &self.root_lm.model().id;
        let short_model = model_id.rsplit('/').next().unwrap_or(model_id);
        result.push_str(&format!(
            "{}{} {short_model}{RESET}",
            &self.palette.chrome_model_fg,
            model_symbol()
        ));

        if let Some(effort) = effort_label(self.root_lm.thinking(), self.term.width()) {
            let thinking_fg = thinking_level_color(self.root_lm.thinking(), &self.palette);
            result.push_str(&format!(" {}{effort}{RESET}", thinking_fg));
        }

        result
    }

    pub(super) fn chrome_lines(&mut self) -> (Vec<String>, Option<LiveCursor>) {
        let mut lines: Vec<String> = Vec::new();
        let width = self.term.width().max(12);
        let p = &self.palette;

        if self.running.is_some() {
            lines.push(format!(
                "{CYAN}{}{RESET} {DIM}{} · turn {}{RESET}",
                self.spinner.frame(),
                self.status,
                self.turn
            ));
        }

        if self.branch_at.elapsed() > Duration::from_secs(5) {
            self.branch = git_branch(&self.cwd);
            self.branch_at = std::time::Instant::now();
        }
        let pwd = display_path(&self.cwd);

        let framed = self.selector.is_some();
        if framed {
            let header_plain = "φ";
            let header_fill = width
                .saturating_sub(header_plain.chars().count() + 5)
                .max(1);
            lines.push(format!(
                "{chrome_border}┌─ {BOLD}φ{RESET} {}┐{RESET}",
                "─".repeat(header_fill),
                chrome_border = &p.chrome_border,
            ));
        }

        let cursor;
        if let Some((_, widget)) = &self.selector {
            let avail = self.term.height().saturating_sub(lines.len() + 2).max(1);
            let (sel_lines, sel_cursor) = widget.render(width, avail);
            let top = lines.len();
            cursor = sel_cursor.map(|(line, col)| LiveCursor {
                line: top + line,
                col,
            });
            lines.extend(sel_lines);
            let hint = if widget.searchable() {
                "type to filter · up/down select · enter confirm · esc cancel"
            } else {
                "up/down select · enter confirm · esc cancel"
            };
            lines.push(format!("{muted_fg}{hint}{RESET}", muted_fg = &p.muted_fg));
        } else {
            let (editor_lines, (row, col)) = self.editor.render();
            let max_visible = (self.term.height() * 3 / 10).max(5);
            if row < self.editor_scroll {
                self.editor_scroll = row;
            } else if row + 1 > self.editor_scroll + max_visible {
                self.editor_scroll = row + 1 - max_visible;
            }
            self.editor_scroll = self
                .editor_scroll
                .min(editor_lines.len().saturating_sub(max_visible));
            let end = (self.editor_scroll + max_visible).min(editor_lines.len());

            // Separator line with the same bg inversion as phi-mux's statusbar.
            let sep_width = width;
            let sep = "\u{2500}".repeat(sep_width);
            lines.push(format!("{}{sep}{RESET}", &p.statusbar_bg));

            // Forgecode-style header: all left-aligned.
            //   PHI  dir  branch  model  effort
            let current_dir = self
                .cwd
                .file_name()
                .and_then(|name| name.to_str())
                .map(String::from)
                .unwrap_or_else(|| ".".to_string());
            let mut header = self.render_chrome_agent();
            header.push_str(&format!(
                "  {}{} {current_dir}{RESET}",
                &self.palette.chrome_dir_fg,
                dir_symbol()
            ));
            if let Some(branch) = &self.branch
                && branch != &current_dir
            {
                header.push_str(&format!(
                    "  {}{} {branch}{RESET}",
                    &self.palette.chrome_dir_fg,
                    branch_symbol()
                ));
            }
            header.push_str(&format!("  {}", self.render_chrome_model()));
            lines.push(header);

            // Editor prompt: chevron on row 0, 2-space indent on continuation.
            let editor_top = lines.len();
            let input_inner = width.saturating_sub(2).max(1);
            let mut input_cursor = None;
            let mut rendered_rows_before_cursor = 0usize;
            for (i, line) in editor_lines[self.editor_scroll..end].iter().enumerate() {
                let absolute_row = i + self.editor_scroll;
                let content = if absolute_row == 0 {
                    if line.is_empty() {
                        format!("{}{}{RESET} ", &self.palette.chrome_dir_fg, success_symbol())
                    } else {
                        format!("{}{}{RESET} {line}", &self.palette.chrome_dir_fg, success_symbol())
                    }
                } else {
                    format!("  {line}")
                };
                let avail_inner = if absolute_row == 0 { input_inner } else { width };
                if absolute_row == row {
                    let source_col = if absolute_row == 0 {
                        if line.is_empty() { 2 } else { 2 + col }
                    } else {
                        2 + col
                    };
                    let (wrapped, (cursor_row, cursor_col)) =
                        wrap_ansi_locate(&content, avail_inner, source_col);
                    input_cursor = Some(LiveCursor {
                        line: editor_top + rendered_rows_before_cursor + cursor_row,
                        col: cursor_col,
                    });
                    rendered_rows_before_cursor += wrapped.len();
                    for r in wrapped {
                        lines.push(r);
                    }
                } else {
                    let wrapped = wrap_ansi(&content, avail_inner);
                    rendered_rows_before_cursor += wrapped.len();
                    for r in wrapped {
                        lines.push(r);
                    }
                }
            }
            if end < editor_lines.len() {
                let below = editor_lines.len() - end;
                lines.push(format!("{muted_fg}  \u{2193} {below} more{RESET}", muted_fg = &p.muted_fg));
            }
            cursor = input_cursor;

            if let Some(text) = self.queued.first() {
                let first = text.lines().next().unwrap_or("");
                let more = if text.lines().count() > 1 { " \u{2026}" } else { "" };
                let count = if self.queued.len() > 1 {
                    format!(" \u{00d7}{}", self.queued.len())
                } else {
                    String::new()
                };
                lines.push(format!("{muted_fg}queued{count} > {first}{more}{RESET}", muted_fg = &p.muted_fg));
            }

            if let Some(popup) = &self.popup {
                lines.extend(render_popup(popup));
            }

            let hints: &[&str] = if self.login.is_some() {
                &[
                    "finish login in the browser, or paste the redirect URL and press enter \u{00b7} esc cancels",
                ]
            } else if self.popup.is_some() {
                &["tab complete \u{00b7} up/down select \u{00b7} enter accept \u{00b7} esc dismiss"]
            } else if self.running.is_some() {
                &["enter queue follow-up", "alt+up edit queued \u{00b7} esc cancel"]
            } else {
                &[]
            };
            for hint in hints {
                lines.push(format!("{muted_fg}{hint}{RESET}", muted_fg = &p.muted_fg));
            }
        }


        // Footer: only rendered in framed (selector) mode.  The non-framed
        // REPL path puts all metadata in the forgecode-style header above.
        let footer = if !framed {
            String::new()
        } else {
            let left = if self.usage.total_tokens > 0 {
                format!(
                    "\u{2191}{} \u{2193}{} ${:.4}",
                    format_tokens(self.usage.input),
                    format_tokens(self.usage.output),
                    self.usage.cost.total
                )
            } else {
                String::new()
            };
            let mut right = format!(
                "phi \u{00b7} {} \u{00b7} thinking:{}",
                self.model_label,
                self.root_lm.thinking().as_str()
            );
            right.push_str(" \u{00b7} ");
            right.push_str(&pwd);
            if let Some(branch) = &self.branch {
                right.push_str(" \u{00b7} ");
                right.push_str(branch);
            }
            match (left.is_empty(), right.is_empty()) {
                (true, true) => String::new(),
                (true, false) => right,
                (false, true) => left,
                (false, false) => format!("{left} \u{00b7} {right}"),
            }
        };
        if framed {
            let footer_fill = width.saturating_sub(footer.chars().count() + 5).max(1);
            lines.push(format!(
                "{chrome_border}\u{2514}\u{2500} {muted_fg}{footer}{RESET} {}\u{2518}{RESET}",
                "\u{2500}".repeat(footer_fill),
                chrome_border = &p.chrome_border,
                muted_fg = &p.muted_fg,
            ));
        }
        (lines, cursor)
    }

    pub(super) fn panel_width(&self) -> usize {
        self.term.width().max(1)
    }

    pub(super) fn repl_body_width(&self) -> usize {
        self.panel_width().saturating_sub(4).max(4)
    }

    pub(super) fn card_body_width(&self, depth: usize) -> usize {
        self.repl_body_width()
            .saturating_sub(result_prefix_width(depth))
            .max(1)
    }
}
