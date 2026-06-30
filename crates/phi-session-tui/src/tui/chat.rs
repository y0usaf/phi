use super::*;

impl Ui {
    pub(super) fn assistant_card_lines(&self, text: &str) -> Vec<String> {
        let rendered = render_markdown(text, self.markdown_width(), &self.palette.markdown);
        // pi-style: no header, no background — assistant prose is bare text
        // separated from siblings by a spacer provided by the caller.
        rendered
            .into_iter()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{fg}{line}{RESET}", fg = &self.palette.assistant_fg)
                }
            })
            .collect()
    }

    pub(super) fn user_card_lines(&self, text: &str) -> Vec<String> {
        let rendered = render_markdown(text, self.markdown_width(), &self.palette.markdown);
        let width = self.panel_width();
        let bg = &self.palette.user_message_bg;
        // pi-style: full-width background fill with 1-cell padding (Box(1,1,bgFn)),
        // no header label. OSC 133 zone markers wrap first/last lines.
        let mut body: Vec<String> = rendered
            .into_iter()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    format!("{fg}{line}{RESET}", fg = &self.palette.user_fg)
                }
            })
            .collect();
        if !body.is_empty() {
            body[0] = format!("{OSC133_ZONE_START}{}", body[0]);
            let last = body.len() - 1;
            body[last] = format!("{}{OSC133_ZONE_END}{OSC133_ZONE_FINAL}", body[last]);
        }
        render_boxed_padded(&body, bg, width, self.tools.user_padding)
    }

    pub(super) fn tool_lines(
        &self,
        _depth: usize,
        title: &str,
        body: &str,
        max: usize,
    ) -> Vec<String> {
        // Thinking/prelude block: flat lines, no depth indentation.
        // Title on line 0, body follows. Matches pi's assistant thinking
        // rendering: italic markdown, no box chrome.
        let content_width = self.panel_width().max(1);
        let mut rows = Vec::new();
        rows.extend(wrap_ansi(title, content_width));
        let trimmed = body.trim_end();
        if !trimmed.is_empty() {
            let output_rows: Vec<String> = trimmed
                .lines()
                .flat_map(|line| wrap_ansi(line, content_width))
                .collect();
            let shown = output_rows.len().min(max);
            for row in &output_rows[..shown] {
                rows.push(format!("{}{row}{RESET}", &self.palette.muted_fg));
            }
            if output_rows.len() > max {
                rows.push(format!(
                    "{}… ({} more lines){RESET}",
                    &self.palette.muted_fg,
                    output_rows.len() - max
                ));
            }
        }
        rows
    }

    pub(super) fn repl_code_lines(&self, depth: usize, body: &str, max: usize) -> Vec<String> {
        let trimmed = body.trim_end();
        let mut rows = Vec::new();
        if !trimmed.is_empty() && max > 0 {
            let wrap_width = if self.block_active {
                self.box_width()
                    .saturating_sub(2)
                    .saturating_sub(indent_width(depth))
                    .max(1)
            } else {
                self.card_body_width(depth)
            };
            let display_code = collapse_answer_assignment(trimmed);
            rows = highlight_code(&display_code, Some("repl"), &self.palette.markdown)
                .into_iter()
                .flat_map(|line| wrap_ansi(&line, wrap_width))
                .collect();
            if rows.len() > max {
                let hidden = rows.len() - max;
                rows.truncate(max);
                rows.push(format!("{DIM}… ({hidden} more lines){RESET}"));
            }
        }
        let prefix = if self.block_active {
            indent(depth)
        } else {
            result_prefix(depth)
        };
        let blank = indent(depth);
        rows.into_iter()
            .map(|line| {
                if line.is_empty() {
                    blank.clone()
                } else {
                    format!("{prefix}{line}")
                }
            })
            .collect()
    }

    pub(super) fn live_thinking_lines(&self) -> Vec<String> {
        if self.thinking.trim().is_empty() {
            return Vec::new();
        }
        if self.expand_thinking {
            self.tool_lines(
                0,
                &format!("{DIM}thinking{RESET}"),
                &self.thinking,
                usize::MAX,
            )
        } else {
            Vec::new()
        }
    }

    pub(super) fn flush_thinking(&mut self) {
        let thinking = std::mem::take(&mut self.thinking);
        if thinking.trim().is_empty() {
            return;
        }
        if self.expand_thinking {
            self.push_dot(0, "thinking".to_owned());
            self.push_preview(0, &thinking, usize::MAX);
        }
    }

    pub(super) fn status_rows(&self, depth: usize, text: &str) -> Vec<String> {
        let bg = if self.block_active {
            &self.palette.tool_pending_bg_nested
        } else {
            &self.palette.tool_pending_bg
        };
        self.status_rows_bg(depth, text, bg)
    }

    /// Status rows with an explicit background color (pending vs. failed),
    /// rendered as a pi-style boxed block with 1-cell padding. The pending
    /// spinner/label or failure indicator is the only content — no depth
    /// indentation (the box IS the visual structure, matching pi's
    /// ToolExecutionComponent at `isPartial: true`).
    pub(super) fn status_rows_bg(&self, _depth: usize, text: &str, bg: &str) -> Vec<String> {
        let content_width = self.box_width().saturating_sub(2).max(1);
        let wrapped = wrap_ansi(text, content_width);
        render_boxed_padded(&wrapped, bg, self.box_width(), self.tools.tool_padding)
    }

    pub(super) fn push_status(&mut self, depth: usize, text: String) -> Option<(usize, usize)> {
        if self.block_active {
            self.ensure_block_gap();
        } else if depth == 0 {
            self.ensure_pending_gap();
        }
        let rows = self.status_rows(depth, &text);
        let start = if self.block_active {
            Some(self.block.len())
        } else {
            None
        };
        let len = rows.len();
        for row in rows {
            self.push_row(row);
        }
        self.last_activity = true;
        start.map(|start| (start, len))
    }

    pub(super) fn shift_open_row_starts(&mut self, after: usize, delta: isize) {
        if delta == 0 {
            return;
        }
        for row in self.open_tool_rows.values_mut() {
            if row.start >= after {
                row.start = ((row.start as isize) + delta) as usize;
            }
        }
        for row in self.open_subcall_rows.iter_mut() {
            if row.start >= after {
                row.start = ((row.start as isize) + delta) as usize;
            }
        }
    }

    pub(super) fn replace_block_rows(
        &mut self,
        start: usize,
        len: usize,
        rows: Vec<String>,
    ) -> usize {
        if !self.block_active || start > self.block.len() {
            return rows.len();
        }
        let end = (start + len).min(self.block.len());
        let old_len = end.saturating_sub(start);
        let new_len = rows.len();
        self.block.splice(start..end, rows);
        self.shift_open_row_starts(end, new_len as isize - old_len as isize);
        new_len
    }

    pub(super) fn refresh_open_activity_rows(&mut self) {
        if !self.block_active {
            return;
        }
        let now_ms = self.anim_start.elapsed().as_millis() as u64;
        let spinner = self.visual.spinner_frame(now_ms);
        let elapsed = now_ms;
        let shimmer = self.palette.shimmer_accent;

        let mut tool_ids: Vec<((usize, u64), usize)> = self
            .open_tool_rows
            .iter()
            .map(|(&id, row)| (id, row.start))
            .collect();
        tool_ids.sort_by_key(|(_, start)| std::cmp::Reverse(*start));
        for (id, _) in tool_ids {
            let Some(row) = self.open_tool_rows.get(&id).cloned() else {
                continue;
            };
            let new_len = self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows(
                    row.depth,
                    &running_status_text_shimmer(&row.label, &spinner, shimmer, elapsed),
                ),
            );
            if let Some(open) = self.open_tool_rows.get_mut(&id) {
                open.len = new_len;
            }
        }

        let mut sub_idxs: Vec<(usize, usize)> = self
            .open_subcall_rows
            .iter()
            .enumerate()
            .map(|(idx, row)| (idx, row.start))
            .collect();
        sub_idxs.sort_by_key(|(_, start)| std::cmp::Reverse(*start));
        for (idx, _) in sub_idxs {
            let Some(row) = self.open_subcall_rows.get(idx).cloned() else {
                continue;
            };
            let new_len = self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows(
                    row.depth,
                    &running_status_text_shimmer(&row.label, &spinner, shimmer, elapsed),
                ),
            );
            if let Some(open) = self.open_subcall_rows.get_mut(idx) {
                open.len = new_len;
            }
        }
    }

    pub(super) fn fail_open_activity_rows(&mut self) {
        self.block_errored = true;
        let mut tool_ids: Vec<((usize, u64), usize)> = self
            .open_tool_rows
            .iter()
            .map(|(&id, row)| (id, row.start))
            .collect();
        tool_ids.sort_by_key(|(_, start)| std::cmp::Reverse(*start));
        for (id, _) in tool_ids {
            let Some(row) = self.open_tool_rows.remove(&id) else {
                continue;
            };
            self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows_bg(
                    row.depth,
                    &failed_status_text(&row.label, None, None, &self.palette),
                    &self.palette.tool_error_bg_nested,
                ),
            );
        }
        while let Some(row) = self.open_subcall_rows.pop_back() {
            self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows_bg(
                    row.depth,
                    &failed_status_text(&row.label, None, None, &self.palette),
                    &self.palette.tool_error_bg_nested,
                ),
            );
        }
    }

    pub(super) fn push_row(&mut self, row: String) {
        if self.block_active {
            self.block.push(row);
        } else {
            self.pending.push(TranscriptEntry::RawLines(vec![row]));
        }
    }

    /// Ensure a one-line gap before the next block pushed into the active
    /// REPL block's child rows. If the last row in `self.block` is non-empty
    /// (i.e. a prior child card or flat line is still touching), a blank line
    /// is inserted so consecutive child blocks never visually touch.
    pub(super) fn ensure_block_gap(&mut self) {
        if !self.block_active || self.tools.gap == 0 {
            return;
        }
        if self.block.last().is_some_and(|row| !row.is_empty()) {
            self.block.push(String::new());
        }
    }

    /// Ensure a one-line gap before the next block pushed into the top-level
    /// pending transcript. If the last row in `self.pending` is non-empty,
    /// a blank line is inserted so consecutive top-level blocks (user
    /// messages, REPL blocks, tool cards, assistant prose) never visually
    /// touch.
    pub(super) fn ensure_pending_gap(&mut self) {
        if self.tools.gap == 0 {
            return;
        }
        let needs_gap = match self.pending.last() {
            Some(TranscriptEntry::RawLines(lines)) => lines.last().is_some_and(|l| !l.is_empty()),
            Some(_) => true,
            None => false,
        };
        if needs_gap {
            self.pending
                .push(TranscriptEntry::RawLines(vec![String::new()]));
        }
    }

    pub(super) fn begin_block(&mut self) {
        self.ensure_pending_gap();
        self.block.clear();
        self.open_tool_rows.clear();
        self.open_subcall_rows.clear();
        self.block_errored = false;
        self.block_active = true;
        self.push_row(format!("{}repl{}", &self.palette.tool_title_fg, RESET));
        // Skip the blank separator line when REPL code is suppressed
        // (repl_max_lines == 0): the header stands alone.
        if self.tools.repl_max_lines > 0 {
            self.push_row(String::new());
        }
    }

    pub(super) fn commit_block(&mut self) {
        if !self.block_active {
            return;
        }
        self.block_active = false;
        let body = std::mem::take(&mut self.block);
        let bg = if self.block_errored {
            &self.palette.tool_error_bg
        } else {
            &self.palette.tool_success_bg
        };
        let rows = render_repl_block(&body, bg, self.panel_width(), self.tools.repl_padding);
        self.ensure_pending_gap();
        self.pending.push(TranscriptEntry::RawLines(rows));
        self.open_tool_rows.clear();
        self.open_subcall_rows.clear();
        let after = std::mem::take(&mut self.after_block);
        if !after.is_empty() {
            self.pending.push(TranscriptEntry::RawLines(after));
        }
    }

    pub(super) fn push_dot(&mut self, depth: usize, text: String) {
        if self.block_active {
            self.ensure_block_gap();
        } else if depth == 0 {
            self.ensure_pending_gap();
        }
        self.push_row(forge_title(depth, &text));
        self.last_activity = true;
    }

    pub(super) fn push_bar(&mut self, depth: usize, text: String) {
        if self.block_active {
            self.ensure_block_gap();
        } else if depth == 0 {
            self.ensure_pending_gap();
        }
        let rows = render_flat(&format!("{DIM}{text}{RESET}"), depth, &[]);
        for row in rows {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    pub(super) fn push_preview(&mut self, depth: usize, text: &str, max: usize) {
        self.push_preview_with_lang(depth, text, max, None);
    }

    pub(super) fn preview_rows_with_lang(
        &self,
        depth: usize,
        text: &str,
        max: usize,
        lang: Option<&str>,
    ) -> Vec<String> {
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let base_rows: Vec<String> = match lang {
            Some(lang) => highlight_code(trimmed, Some(lang), &self.palette.markdown),
            None => trimmed
                .lines()
                .map(|line| {
                    highlight_diff_line(line, &self.palette.markdown)
                        .unwrap_or_else(|| highlight_output(line, &self.palette.markdown))
                })
                .collect(),
        };
        // For non-code output (stdout/stderr), apply tool_output_fg to match
        // the styling used by tool_result_rows for tool output (fix #4).
        // Inside an active REPL block, use the box inner content width so flat
        // output aligns with child card widths (fix #5).
        let wrap_width = if self.block_active {
            self.box_width()
                .saturating_sub(2)
                .saturating_sub(indent_width(depth))
                .max(1)
        } else {
            self.card_body_width(depth)
        };
        let mut rows: Vec<String> = base_rows
            .into_iter()
            .map(|line| {
                if lang.is_none() {
                    format!("{}{line}{RESET}", &self.palette.tool_output_fg)
                } else {
                    line
                }
            })
            .flat_map(|line| wrap_ansi(&line, wrap_width))
            .collect();
        if rows.len() > max {
            let hidden = rows.len() - max;
            rows.truncate(max);
            rows.push(format!("{DIM}… ({hidden} more lines){RESET}"));
        }
        rows
    }

    pub(super) fn push_preview_with_lang(
        &mut self,
        depth: usize,
        text: &str,
        max: usize,
        lang: Option<&str>,
    ) {
        let rows = self.preview_rows_with_lang(depth, text, max, lang);
        if rows.is_empty() {
            return;
        }
        if self.block_active {
            self.ensure_block_gap();
        } else if depth == 0 {
            self.ensure_pending_gap();
        }
        let prefix = if self.block_active {
            indent(depth)
        } else {
            result_prefix(depth)
        };
        let blank = indent(depth);
        for line in &rows {
            self.push_row(if line.is_empty() {
                blank.clone()
            } else {
                format!("{prefix}{line}")
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn tool_result_rows(
        &self,
        _depth: usize,
        name: &str,
        args: &serde_json::Value,
        output: &str,
        max: usize,
        lang: Option<&str>,
        failed: bool,
    ) -> Vec<String> {
        let identity = tool_identity(name, args, &self.palette);
        let padding = self.tools.tool_padding;

        // tool_max_lines = 0: one-line summary `glyph tool ╱ detail`, mirroring
        // pi-compact's compact tools: `✓ bash ╱ git status`.
        if self.tools.tool_max_lines == 0 {
            return self.compact_tool_rows(&identity, output, failed, padding);
        }

        // Full card: pi-style tool card with configurable padding. Content is
        // flat — no depth indentation, no gutter. The call (anchor) is the
        // first line; the result (output) follows, each line in toolOutput
        // color. Matches pi's ToolExecutionComponent:
        //   renderCall → Text(bold toolTitle + colored args)
        //   renderResult → Text(toolOutput output)
        let content_width = self.box_width().saturating_sub(2).max(1);
        let mut content: Vec<String> = Vec::new();
        // Call line (anchor)
        content.extend(wrap_ansi(&identity.anchor, content_width));
        // Identity body (multi-line commands, etc.)
        for line in &identity.body {
            content.extend(wrap_ansi(line, content_width));
        }
        // pi-style: a blank line separates the call header from the
        // result output. pi's renderResult implementations start their
        // text with a leading \n (e.g. formatReadResult returns
        // `\n${...}`, rebuildBashResultRenderComponent uses
        // new Text(`\n${styledOutput}`, 0, 0)). Since the call and
        // result are separate children in the same Box, that \n renders
        // as an empty line between them.
        let trimmed = output.trim_end();
        if !trimmed.is_empty() {
            content.push(String::new());
            let base_rows: Vec<String> = match lang {
                Some(lang) => highlight_code(trimmed, Some(lang), &self.palette.markdown),
                None => trimmed
                    .lines()
                    .map(|line| {
                        highlight_diff_line(line, &self.palette.markdown)
                            .unwrap_or_else(|| highlight_output(line, &self.palette.markdown))
                    })
                    .collect(),
            };
            let output_rows: Vec<String> = base_rows
                .into_iter()
                .flat_map(|line| {
                    let colored = format!("{}{line}{RESET}", &self.palette.tool_output_fg);
                    wrap_ansi(&colored, content_width)
                })
                .collect();
            let shown = output_rows.len().min(max);
            content.extend(output_rows[..shown].to_vec());
            if output_rows.len() > max {
                content.push(format!(
                    "{}… ({} more lines){RESET}",
                    &self.palette.muted_fg,
                    output_rows.len() - max
                ));
            }
        }
        // State-based background (pending → success/error). When the
        // tool card is rendered inside an active REPL block, use the
        // slightly-brighter "nested" variant so the inner card stands out
        // from the enclosing container (which uses the same base bg).
        let bg = if failed {
            if self.block_active {
                &self.palette.tool_error_bg_nested
            } else {
                &self.palette.tool_error_bg
            }
        } else if self.block_active {
            &self.palette.tool_success_bg_nested
        } else {
            &self.palette.tool_success_bg
        };
        render_boxed_padded(&content, bg, self.box_width(), padding)
    }

    /// One-line compact tool summary: `{glyph} {anchor} {DIM}╱{RESET} {detail}`.
    /// Mirrors pi-compact's compact tools: `✓ bash ╱ git status`.
    /// The anchor carries the tool verb + canonical argument (already styled
    /// by `tool_identity`); the detail is a plain truncated first line of the
    /// result output. Wrapped in `render_boxed_padded` so the background colour
    /// (success/error) is preserved and padding is configurable.
    fn compact_tool_rows(
        &self,
        identity: &super::tool_display::Identity,
        output: &str,
        failed: bool,
        padding: usize,
    ) -> Vec<String> {
        let glyph = if failed {
            format!("{}✗{RESET}", &self.palette.error_fg)
        } else {
            format!("{}✓{RESET}", &self.palette.tool_title_fg)
        };
        let anchor_plain = strip_ansi(&identity.anchor);
        let detail = output
            .trim()
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        // Reserve space for `glyph ╱ ` prefix; cap the detail so the
        // whole line fits the content width.
        let prefix_width = visible_width(&strip_ansi(&format!("{glyph} {anchor_plain} ╱ ")));
        let content_width = self.box_width().saturating_sub(2).max(1);
        let detail_budget = content_width.saturating_sub(prefix_width).max(1);
        let detail_trunc = if detail.chars().count() <= detail_budget {
            detail.to_owned()
        } else {
            let keep = detail_budget.saturating_sub(1);
            let mut short: String = detail.chars().take(keep).collect();
            short.push('…');
            short
        };
        let line = if detail_trunc.is_empty() {
            format!("{glyph} {anchor_plain}")
        } else {
            format!("{glyph} {anchor_plain} {DIM}╱{RESET} {detail_trunc}")
        };
        let bg = if failed {
            if self.block_active {
                &self.palette.tool_error_bg_nested
            } else {
                &self.palette.tool_error_bg
            }
        } else if self.block_active {
            &self.palette.tool_success_bg_nested
        } else {
            &self.palette.tool_success_bg
        };
        render_boxed_padded(&[line], bg, self.box_width(), padding)
    }

    pub(super) fn subcall_result_rows(
        &self,
        _depth: usize,
        label: &str,
        content: Option<&str>,
        max: usize,
        failed: bool,
    ) -> Vec<String> {
        // pi-style: flat content (label + output) in a Box(1,1,bgFn).
        // No depth indentation — the box is the visual structure.
        let content_width = self.box_width().saturating_sub(2).max(1);
        let mut rows: Vec<String> = Vec::new();
        rows.extend(wrap_ansi(label, content_width));
        if let Some(text) = content {
            let trimmed = text.trim_end();
            if !trimmed.is_empty() {
                // pi-style blank line separating header from output
                // (mirrors the \n prefix in pi's renderResult implementations).
                rows.push(String::new());
                let output_rows: Vec<String> = trimmed
                    .lines()
                    .flat_map(|line| {
                        let colored = format!("{}{line}{RESET}", &self.palette.tool_output_fg);
                        wrap_ansi(&colored, content_width)
                    })
                    .collect();
                let shown = output_rows.len().min(max);
                rows.extend(output_rows[..shown].to_vec());
                if output_rows.len() > max {
                    rows.push(format!(
                        "{}… ({} more lines){RESET}",
                        &self.palette.muted_fg,
                        output_rows.len() - max
                    ));
                }
            }
        }
        // Use the slightly-brighter nested variant when rendered inside an
        // active REPL block so the subcall card stands out from the
        // enclosing container.
        let bg = if failed {
            if self.block_active {
                &self.palette.tool_error_bg_nested
            } else {
                &self.palette.tool_error_bg
            }
        } else if self.block_active {
            &self.palette.tool_success_bg_nested
        } else {
            &self.palette.tool_success_bg
        };
        render_boxed_padded(&rows, bg, self.box_width(), self.tools.tool_padding)
    }

    #[allow(clippy::too_many_arguments, dead_code)]
    pub(super) fn push_tool_result(
        &mut self,
        depth: usize,
        name: &str,
        args: &serde_json::Value,
        output: &str,
        max: usize,
        lang: Option<&str>,
        failed: bool,
    ) {
        if self.block_active {
            self.ensure_block_gap();
        } else if depth == 0 {
            self.ensure_pending_gap();
        }
        let rows = self.tool_result_rows(depth, name, args, output, max, lang, failed);
        for row in rows {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    pub(super) fn push_repl_block(&mut self, depth: usize, text: &str, max: usize) {
        if self.block_active {
            self.ensure_block_gap();
        } else if depth == 0 {
            self.ensure_pending_gap();
        }
        for row in self.repl_code_lines(depth, text, max) {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    pub(super) fn push_user_message(&mut self, text: &str) {
        self.ensure_pending_gap();
        self.pending
            .push(TranscriptEntry::UserMessage(text.to_owned()));
        self.last_activity = false;
    }

    pub(super) fn push_prose(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if self.block_active {
            // Prose flushed while a block is active goes to after_block,
            // committed after the block closes. Ensure a gap from any prior
            // after_block content.
            if self.after_block.last().is_some_and(|row| !row.is_empty()) {
                self.after_block.push(String::new());
            }
            let card = self.assistant_card_lines(text);
            self.after_block.extend(card);
        } else {
            self.ensure_pending_gap();
            self.pending
                .push(TranscriptEntry::AssistantProse(text.to_owned()));
        }
        self.last_activity = false;
    }

    pub(super) fn push_assistant_message(&mut self, text: &str) {
        let (thinking, text) = split_assistant_display_text(text);
        if let Some(thinking) = thinking
            && self.expand_thinking
        {
            self.push_dot(0, "thinking".to_owned());
            self.push_preview(0, thinking, usize::MAX);
        }

        // With native tool calls, the assistant message text is pure prose.
        // Code blocks are replayed separately via `push_repl_replay`, which
        // is called by the session-replay path for each persisted REPL
        // execution interleaved between assistant messages.
        self.push_prose(text);
    }

    pub(super) fn push_repl_replay(
        &mut self,
        code: &str,
        stdout: &str,
        stderr: &str,
        _duration_secs: f64,
        tools: &[session::ToolCall],
        subcalls: &[session::SubcallRecord],
    ) {
        self.ensure_pending_gap();
        self.pending.push(TranscriptEntry::ReplBlock(ReplBlockData {
            code: code.to_owned(),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            tools: tools.to_owned(),
            subcalls: subcalls.to_owned(),
        }));
    }

    pub(super) fn cycle_thinking(&mut self) {
        let current = self.root_lm.thinking();
        let idx = ThinkingLevel::ALL
            .iter()
            .position(|l| *l == current)
            .unwrap_or(0);
        let next = ThinkingLevel::ALL[(idx + 1) % ThinkingLevel::ALL.len()];
        let prev_str = format!("{:?}", current);
        self.root_lm.set_thinking(next);

        // ── Extension: ThinkingLevelSelect ──────────────────────────────
        let rt = self.runtime.clone();
        let next_str = format!("{:?}", next);
        tokio::spawn(async move {
            let _ = rt.on_thinking_level_select(&next_str, &prev_str).await;
        });
    }

    pub(super) fn replay_transcript_if_idle(&mut self) {
        if self.running.is_none() && !self.block_active {
            self.replay_branch();
        }
    }

    pub(super) fn toggle_thinking(&mut self) {
        self.expand_thinking = !self.expand_thinking;
        self.replay_transcript_if_idle();
    }

    pub(super) fn toggle_tools(&mut self) {
        self.expand_tools = !self.expand_tools;
        self.replay_transcript_if_idle();
    }

    pub(super) fn code_limit(&self) -> usize {
        if self.expand_tools {
            usize::MAX
        } else {
            self.tools.repl_max_lines
        }
    }

    pub(super) fn output_limit(&self) -> usize {
        if self.expand_tools {
            usize::MAX
        } else {
            self.tools.tool_max_lines
        }
    }
}
