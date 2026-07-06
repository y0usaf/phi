use super::*;

impl Ui {
    pub(super) fn assistant_card_lines(&self, text: &str) -> Vec<String> {
        // pi-style by default: no header, no background — assistant prose is
        // bare text separated from siblings by a spacer provided by the
        // caller. Chrome may add a prefix on the first content line
        // (claude-code's `⏺ `); the body hangs at the prefix width.
        let prefix = self.chrome.assistant_prefix.as_deref().unwrap_or("");
        let hang = " ".repeat(prefix.chars().count());
        let rendered = render_markdown(
            text,
            self.markdown_width().saturating_sub(hang.len()),
            &self.palette.markdown,
        );
        let mut first_content = true;
        rendered
            .into_iter()
            .map(|line| {
                if line.is_empty() {
                    String::new()
                } else {
                    let head = if first_content { prefix } else { hang.as_str() };
                    first_content = false;
                    format!("{fg}{head}{line}{RESET}", fg = &self.palette.assistant_fg)
                }
            })
            .collect()
    }

    pub(super) fn user_card_lines(&self, text: &str) -> Vec<String> {
        // Two chrome shapes for the user turn. Stock (crush-style): a
        // colored left-gutter bar on every line instead of a background
        // wash — the accent bar is the role signal, the reading surface
        // stays flat. claude-code-style: a ghost `❯ ` prefix on the first
        // row, continuations flowing flush beneath it (matching ink's
        // inline wrap), optionally washed full-width with the theme's
        // user_message_bg. `user_padding` extends the bar/wash above and
        // below the text. OSC 133 zone markers wrap first/last lines.
        let prefix = self.chrome.user_prefix.as_deref();
        let prefix_w = prefix.map(|p| p.chars().count()).unwrap_or(0);
        let rendered = render_markdown(
            text,
            self.markdown_width().saturating_sub(prefix_w),
            &self.palette.markdown,
        );
        let pad_row = match prefix {
            Some(_) => String::new(),
            None => format!("{}\u{258C}{RESET}", &self.palette.accent_fg),
        };
        let mut body: Vec<String> = Vec::with_capacity(rendered.len() + 2);
        for _ in 0..self.tools.user_padding {
            body.push(pad_row.clone());
        }
        for (i, line) in rendered.into_iter().enumerate() {
            body.push(match prefix {
                Some(p) if i == 0 => format!(
                    "{ghost}{p}{RESET}{fg}{line}{RESET}",
                    ghost = &self.palette.text_ghost,
                    fg = &self.palette.user_fg
                ),
                Some(_) if line.is_empty() => String::new(),
                Some(_) => format!("{fg}{line}{RESET}", fg = &self.palette.user_fg),
                None if line.is_empty() => pad_row.clone(),
                None => format!("{pad_row} {fg}{line}{RESET}", fg = &self.palette.user_fg),
            });
        }
        for _ in 0..self.tools.user_padding {
            body.push(pad_row.clone());
        }
        if self.chrome.user_background {
            let width = self.panel_width();
            body = body
                .iter()
                .map(|row| fill_bg(row, &self.palette.user_bg, width))
                .collect();
        }
        // OSC 133 zones bracket the CONTENT lines, not the padding bars —
        // shell integrations locate the semantic start of user input from
        // the A marker, which must sit on the first text line.
        let pad = self.tools.user_padding;
        if body.len() > pad * 2 {
            body[pad] = format!("{OSC133_ZONE_START}{}", body[pad]);
            let last_content = body.len() - 1 - pad;
            body[last_content] =
                format!("{}{OSC133_ZONE_END}{OSC133_ZONE_FINAL}", body[last_content]);
        } else if !body.is_empty() {
            body[0] = format!("{OSC133_ZONE_START}{}", body[0]);
            let last = body.len() - 1;
            body[last] = format!("{}{OSC133_ZONE_END}{OSC133_ZONE_FINAL}", body[last]);
        }
        body
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
                    "{}{}{RESET}",
                    &self.palette.muted_fg,
                    self.chrome.format_more_lines(output_rows.len() - max)
                ));
            }
        }
        rows
    }

    pub(super) fn repl_code_lines(&self, depth: usize, body: &str, max: usize) -> Vec<String> {
        // Tool-card chrome hangs the code 2 cells under the container's
        // glyph anchor, exactly like a bash card's multi-line body.
        let hang = if self.block_active && self.chrome.repl_as_tool {
            2
        } else {
            0
        };
        let trimmed = body.trim_end();
        let mut rows = Vec::new();
        if !trimmed.is_empty() && max > 0 {
            let wrap_width = if self.block_active {
                self.box_width()
                    .saturating_sub(2)
                    .saturating_sub(indent_width(depth))
                    .saturating_sub(hang)
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
                rows.push(format!(
                    "{}{}{RESET}",
                    &self.palette.text_tertiary,
                    self.chrome.format_more_lines(hidden)
                ));
            }
        }
        let prefix = if self.block_active {
            format!("{}{}", indent(depth), " ".repeat(hang))
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
                &format!("{}thinking{RESET}", &self.palette.text_tertiary),
                &self.thinking,
                usize::MAX,
            )
        } else {
            Vec::new()
        }
    }

    pub(super) fn flush_thinking(&mut self) {
        // Committing must see the full text, not the paced prefix.
        if !self.thinking_backlog.is_empty() {
            let backlog = std::mem::take(&mut self.thinking_backlog);
            self.thinking.push_str(&backlog);
        }
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
        // Flat chrome: the status line lies bare in the transcript, no
        // tinted box (claude-code's in-flight rows are flat).
        if self.chrome.is_flat() {
            return wrapped;
        }
        render_boxed_padded(&wrapped, bg, self.box_width(), self.tools.tool_padding)
    }

    /// Rows for an in-flight tool/subcall, shaped by the chrome's pending
    /// indicator. Stock: spinner glyph + shimmer-swept label in the tinted
    /// pending box. claude-code (`blink_glyph`): the status glyph blinking
    /// dim on a 600ms interval (ToolUseLoader's useBlink — glyph ↔ blank,
    /// no color animation) over the plain label, plus the optional dim
    /// result-gutter note (`⎿  Running…`).
    pub(super) fn running_status_rows(
        &self,
        depth: usize,
        label: &str,
        now_ms: u64,
    ) -> Vec<String> {
        match self.chrome.pending_indicator {
            phi_ext::PendingIndicator::SpinnerShimmer => self.status_rows(
                depth,
                &running_status_text_shimmer(
                    label,
                    &self.visual.spinner_frame(now_ms),
                    &self.palette.working_fg,
                    self.palette.working_shimmer,
                    now_ms,
                ),
            ),
            phi_ext::PendingIndicator::BlinkGlyph => {
                let content_width = self.box_width().saturating_sub(2).max(1);
                let glyph_w = self.chrome.glyph_ok.chars().count().max(1);
                let glyph = if (now_ms / BLINK_INTERVAL_MS) % 2 == 0 {
                    self.chrome.glyph_ok.clone()
                } else {
                    " ".repeat(glyph_w)
                };
                let hang = " ".repeat(glyph_w + 1);
                let anchor_width = content_width.saturating_sub(glyph_w + 1).max(1);
                let mut rows: Vec<String> = Vec::new();
                for (i, line) in wrap_ansi(label, anchor_width).into_iter().enumerate() {
                    if i == 0 {
                        rows.push(format!("{}{glyph}{RESET} {line}", &self.palette.muted_fg));
                    } else {
                        rows.push(format!("{hang}{line}"));
                    }
                }
                if let Some(note) = &self.chrome.pending_note {
                    rows.push(format!(
                        "{}{}{RESET}{}{note}{RESET}",
                        &self.palette.text_ghost,
                        self.chrome.result_prefix,
                        &self.palette.text_tertiary
                    ));
                }
                if self.chrome.is_flat() {
                    return rows;
                }
                let bg = if self.block_active {
                    &self.palette.tool_pending_bg_nested
                } else {
                    &self.palette.tool_pending_bg
                };
                render_boxed_padded(&rows, bg, self.box_width(), self.tools.tool_padding)
            }
        }
    }

    pub(super) fn push_running_status(
        &mut self,
        depth: usize,
        label: &str,
        now_ms: u64,
    ) -> Option<(usize, usize)> {
        if self.block_active {
            self.ensure_block_gap();
        } else if depth == 0 {
            self.ensure_pending_gap();
        }
        let rows = self.running_status_rows(depth, label, now_ms);
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
        for flash in self.flash_rows.iter_mut() {
            if flash.start >= after {
                flash.start = ((flash.start as isize) + delta) as usize;
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
        // State arc (stock chrome): magenta while working, green on
        // completion — the hue cross at the finish line is what makes
        // "done" legible at a glance. Blink-glyph chrome animates by
        // visibility instead; both resolve inside `running_status_rows`.
        let now_ms = self.clock.now_ms();

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
                self.running_status_rows(row.depth, &row.label, now_ms),
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
                self.running_status_rows(row.depth, &row.label, now_ms),
            );
            if let Some(open) = self.open_subcall_rows.get_mut(idx) {
                open.len = new_len;
            }
        }

        self.refresh_completion_flashes(now_ms);
    }

    /// Drive the completion pulses: while a flash is live, the card's anchor
    /// line sweeps with the success (green) shimmer; on expiry the plain
    /// rows are restored and the flash dropped. The magenta→green hue cross
    /// is the completion celebration.
    fn refresh_completion_flashes(&mut self, now_ms: u64) {
        if self.flash_rows.is_empty() {
            return;
        }
        let success = self.palette.shimmer_accent;
        let mut flashes = std::mem::take(&mut self.flash_rows);
        // Process from the bottom up so earlier replacements can't shift
        // later starts mid-loop (lengths are equal, but stay defensive).
        flashes.sort_by_key(|f| std::cmp::Reverse(f.start));
        let mut keep: Vec<FlashRow> = Vec::with_capacity(flashes.len());
        for flash in flashes {
            if now_ms >= flash.expires_ms {
                self.replace_block_rows(flash.start, flash.len, flash.rows.clone());
                continue;
            }
            let mut rows = flash.rows.clone();
            if let Some(first) = rows.first_mut() {
                *first = shimmer_ansi(first, success, now_ms);
            }
            self.replace_block_rows(flash.start, flash.len, rows);
            keep.push(flash);
        }
        // Invariant: self.flash_rows is empty here — it was taken at the top
        // and nothing on the rendering path (replace_block_rows,
        // shift_open_row_starts) pushes to it; handle_tool_end is the only
        // pusher and it runs on the event-dispatch path, not during render.
        debug_assert!(
            self.flash_rows.is_empty(),
            "flash_rows must be empty mid-render"
        );
        self.flash_rows = keep;
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
            let rows = self.failed_rows(&row.label);
            self.replace_block_rows(row.start, row.len, rows);
        }
        while let Some(row) = self.open_subcall_rows.pop_back() {
            let rows = self.failed_rows(&row.label);
            self.replace_block_rows(row.start, row.len, rows);
        }
    }

    /// Flat failure line(s) for an interrupted/failed activity row: the red
    /// ✗ status text, wrapped to the content width. No background tint —
    /// the glyph and color carry the state.
    fn failed_rows(&self, label: &str) -> Vec<String> {
        let content_width = self.box_width().saturating_sub(2).max(1);
        wrap_ansi(
            &failed_status_text(label, None, None, &self.chrome, &self.palette),
            content_width,
        )
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
        self.flash_rows.clear();
        self.block_errored = false;
        self.block_active = true;
        self.tool_block = false;
        if self.chrome.repl_as_tool {
            // Tool-card chrome: the container header is a glyph + verb
            // anchor like any tool card; the code hangs directly under it.
            let row = self.repl_anchor_row();
            self.push_row(row);
        } else {
            self.push_row(format!("{}repl{}", &self.palette.tool_title_fg, RESET));
            // Skip the blank separator line when REPL code is suppressed
            // (repl_max_lines == 0): the header stands alone.
            if self.tools.repl_max_lines > 0 {
                self.push_row(String::new());
            }
        }
        self.block_out_gutter = false;
    }

    /// Anchor row for the repl container under tool-card chrome
    /// (`repl_as_tool`): status glyph + the repl render spec's verb
    /// (`repl` stock; a pack may rename it). The glyph carries the success
    /// color from the start — the container is neutral surface, failures
    /// are signaled by the glyphs and error text inside (same doctrine as
    /// the card surface).
    pub(super) fn repl_anchor_row(&self) -> String {
        let verb = self
            .tool_render
            .get("repl")
            .and_then(|s| s.verb.as_deref())
            .unwrap_or("repl");
        format!(
            "{}{}{RESET} {}{verb}{RESET}",
            &self.palette.success_fg, self.chrome.glyph_ok, &self.palette.tool_title_fg
        )
    }

    /// Open a bare live container for a tool that runs outside any repl
    /// block (native/registry dispatch, or a stray event after its block
    /// closed). No `repl` header — the tool card carries its own identity —
    /// but the same live machinery: spinner statuses splice in place instead
    /// of freezing into committed scrollback, and the card gets the block
    /// surface instead of floating as bare flat rows. `handle_tool_end`
    /// commits it once the last open activity row resolves.
    pub(super) fn begin_tool_block(&mut self) {
        self.begin_block();
        self.block.clear();
        self.tool_block = true;
    }

    pub(super) fn commit_block(&mut self) {
        if !self.block_active {
            return;
        }
        // Restore any still-pulsing completion flashes to their plain rows
        // so a frozen mid-pulse frame is never committed to scrollback.
        let flashes = std::mem::take(&mut self.flash_rows);
        for flash in flashes {
            self.replace_block_rows(flash.start, flash.len, flash.rows.clone());
        }
        self.block_active = false;
        self.tool_block = false;
        let body = std::mem::take(&mut self.block);
        // One neutral container surface for committed REPL blocks: failures
        // are signaled by the ✗ glyphs and error text inside, not by tinting
        // the whole container. Three surfaces total (terminal, repl, pending)
        // instead of six. Flat chrome drops the surface entirely — the
        // gutter glyphs are the only structure (claude-code shape).
        let rows = if self.chrome.is_flat() {
            body
        } else {
            render_repl_block(
                &body,
                &self.palette.repl_bg,
                self.panel_width(),
                self.tools.repl_padding,
            )
        };
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
        let rows = render_flat(
            &format!("{}{text}{RESET}", &self.palette.text_tertiary),
            depth,
            &[],
        );
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
        // output aligns with child card widths (fix #5). Tool-card chrome
        // hangs repl output behind the result gutter, so the gutter width
        // comes off the wrap budget.
        let wrap_width = if self.block_active {
            self.box_width()
                .saturating_sub(2)
                .saturating_sub(indent_width(depth))
                .saturating_sub(if self.repl_output_gutter(depth) {
                    self.chrome.result_continuation().len()
                } else {
                    0
                })
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
            rows.push(format!(
                "{}{}{RESET}",
                &self.palette.text_tertiary,
                self.chrome.format_more_lines(hidden)
            ));
        }
        rows
    }

    /// Whether repl-level output rows (depth 0, inside a repl container,
    /// not a bare tool container) carry the tool-card result gutter —
    /// the `repl_as_tool` chrome's `⎿` shape.
    pub(super) fn repl_output_gutter(&self, depth: usize) -> bool {
        self.block_active && depth == 0 && !self.tool_block && self.chrome.repl_as_tool
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
        if self.repl_output_gutter(depth) {
            // First output row of the block carries the `⎿` gutter;
            // everything after (including later streamed chunks) hangs at
            // the same width, so the gutter appears once per container.
            let gutter = format!(
                "{}{}{RESET}",
                &self.palette.text_ghost, self.chrome.result_prefix
            );
            let hang = self.chrome.result_continuation();
            for line in &rows {
                let row = if line.is_empty() {
                    String::new()
                } else if self.block_out_gutter {
                    format!("{hang}{line}")
                } else {
                    self.block_out_gutter = true;
                    format!("{gutter}{line}")
                };
                self.push_row(row);
            }
            return;
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
        let identity = tool_identity(
            self.tool_render.get(name),
            name,
            args,
            &self.chrome,
            &self.palette,
        );

        // tool_max_lines = 0: one-line summary `glyph tool ╱ detail`, mirroring
        // pi-compact's compact tools: `✓ bash ╱ git status`.
        if self.tools.tool_max_lines == 0 {
            return self.compact_tool_rows(&identity, output, failed);
        }

        // Full card: flat, crush-style — no background tint. The colored
        // status glyph (✓ bright green / ✗ red stock; chrome-supplied per
        // style) is the only state signal; the anchor carries the identity,
        // and the output folds under it behind the chrome's result prefix
        // (stock: a 2-space indent; claude-code: `  ⎿  `). Removing the
        // per-state background washes keeps the transcript a single flat
        // surface where hue is reserved for meaning. (`tool_padding` applies
        // to the pending tinted box only — flat cards carry no padding rows.)
        let glyph = if failed {
            format!(
                "{}{}{RESET}",
                &self.palette.error_fg, self.chrome.glyph_failed
            )
        } else {
            format!(
                "{}{}{RESET}",
                &self.palette.success_fg, self.chrome.glyph_ok
            )
        };
        let content_width = self.box_width().saturating_sub(2).max(1);
        let anchor_width = content_width.saturating_sub(2).max(1);
        let mut content: Vec<String> = Vec::new();
        // Anchor line: glyph + identity; wrapped continuations hang under it.
        for (i, line) in wrap_ansi(&identity.anchor, anchor_width)
            .into_iter()
            .enumerate()
        {
            if i == 0 {
                content.push(format!("{glyph} {line}"));
            } else {
                content.push(format!("  {line}"));
            }
        }
        // Identity body (multi-line commands, etc.) hangs under the anchor.
        let body_width = content_width.saturating_sub(2).max(1);
        for line in &identity.body {
            for wrapped in wrap_ansi(line, body_width) {
                content.push(format!("  {wrapped}"));
            }
        }
        let trimmed = output.trim_end();
        if !trimmed.is_empty() {
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
            // The first result row carries the chrome's gutter prefix;
            // continuations (and the hidden-lines marker) hang at a blank
            // indent of the same width.
            let gutter = format!(
                "{}{}{RESET}",
                &self.palette.text_ghost, self.chrome.result_prefix
            );
            let hang = self.chrome.result_continuation();
            let result_width = content_width.saturating_sub(hang.len()).max(1);
            let output_rows: Vec<String> = base_rows
                .into_iter()
                .flat_map(|line| {
                    let colored = format!("{}{line}{RESET}", &self.palette.tool_output_fg);
                    wrap_ansi(&colored, result_width)
                })
                .collect();
            let shown = output_rows.len().min(max);
            for (i, row) in output_rows[..shown].iter().enumerate() {
                if i == 0 {
                    content.push(format!("{gutter}{row}"));
                } else {
                    content.push(format!("{hang}{row}"));
                }
            }
            if output_rows.len() > max {
                content.push(format!(
                    "{hang}{}{}{RESET}",
                    &self.palette.text_tertiary,
                    self.chrome.format_more_lines(output_rows.len() - max)
                ));
            }
        }
        content
    }

    /// One-line compact tool summary: `{glyph} {anchor} ╱ {detail}`.
    /// Mirrors pi-compact's compact tools: `✓ bash ╱ git status`.
    /// The anchor carries the tool verb + canonical argument (already styled
    /// by `tool_identity`); the detail is a plain truncated first line of the
    /// result output. Flat — the colored glyph is the only state signal.
    fn compact_tool_rows(
        &self,
        identity: &super::tool_display::Identity,
        output: &str,
        failed: bool,
    ) -> Vec<String> {
        let glyph = if failed {
            format!(
                "{}{}{RESET}",
                &self.palette.error_fg, self.chrome.glyph_failed
            )
        } else {
            format!(
                "{}{}{RESET}",
                &self.palette.success_fg, self.chrome.glyph_ok
            )
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
            format!(
                "{glyph} {anchor_plain} {}╱{RESET} {}{detail_trunc}{RESET}",
                &self.palette.text_ghost, &self.palette.text_secondary
            )
        };
        vec![line]
    }

    /// Consolidated summary card for all REPL blocks and tool calls in a turn.
    /// Renders the repl count followed by per-category tool tallies with
    /// ✓/✗ glyphs in a single boxed card. Used when `tool_summary` is enabled
    /// instead of rendering individual REPL blocks and tool cards.
    pub(super) fn tool_summary_rows(
        &self,
        tools: &[session::ToolCall],
        repl_count: usize,
    ) -> Vec<String> {
        if repl_count == 0 && tools.is_empty() {
            return Vec::new();
        }
        let ck = format!(
            "{}{}{RESET}",
            &self.palette.success_fg, self.chrome.glyph_ok
        );
        let nr = format!(
            "{}{}{RESET}",
            &self.palette.error_fg, self.chrome.glyph_failed
        );

        // Categories come from the registered tool render specs; tools
        // without a spec tally as "other".
        let tallies = phi_ext::tally(
            tools
                .iter()
                .map(|t| (self.tool_render.category(&t.name), t.error.is_some())),
        );
        let any_failed = tallies.iter().any(|(_, _, failed)| *failed > 0);

        // Flat summary lines inside one neutral REPL container: the colored
        // ✓/✗ glyphs carry the state; indentation carries the nesting.
        let panel_width = self.panel_width();
        let content_width = self.box_width().saturating_sub(2).max(1);
        let mut body: Vec<String> = Vec::new();

        if repl_count > 0 {
            let noun = if repl_count == 1 { "repl" } else { "repls" };
            body.push(format!("{ck} {repl_count} {noun}"));
        }

        let total = tools.len();
        if total > 0 || any_failed {
            if total > 0 {
                let noun = if total == 1 { "tool" } else { "tools" };
                let line = format!("{ck} {total} {noun}");
                body.extend(wrap_ansi(&line, content_width));
            }
            for (cat, ok, failed) in tallies {
                if ok == 0 && failed == 0 {
                    continue;
                }
                if ok > 0 {
                    let noun = if ok == 1 {
                        cat.singular()
                    } else {
                        cat.plural()
                    };
                    let line = format!("  {ck} {ok} {noun}");
                    body.extend(wrap_ansi(&line, content_width));
                }
                if failed > 0 {
                    let line = format!("  {nr} {failed}");
                    body.extend(wrap_ansi(&line, content_width));
                }
            }
        }

        // One neutral container surface — error state lives in the glyphs.
        // Flat chrome: no surface (same degrade as commit_block).
        if self.chrome.is_flat() {
            return body;
        }
        render_repl_block(
            &body,
            &self.palette.repl_bg,
            panel_width,
            self.tools.repl_padding,
        )
    }

    pub(super) fn subcall_result_rows(
        &self,
        _depth: usize,
        label: &str,
        content: Option<&str>,
        max: usize,
        failed: bool,
    ) -> Vec<String> {
        // Flat, like tool cards: colored status glyph + label anchor, with
        // the output folded under behind the chrome's result prefix. No
        // background tint.
        let glyph = if failed {
            format!(
                "{}{}{RESET}",
                &self.palette.error_fg, self.chrome.glyph_failed
            )
        } else {
            format!(
                "{}{}{RESET}",
                &self.palette.success_fg, self.chrome.glyph_ok
            )
        };
        let content_width = self.box_width().saturating_sub(2).max(1);
        let anchor_width = content_width.saturating_sub(2).max(1);
        let mut rows: Vec<String> = Vec::new();
        for (i, line) in wrap_ansi(label, anchor_width).into_iter().enumerate() {
            if i == 0 {
                rows.push(format!("{glyph} {line}"));
            } else {
                rows.push(format!("  {line}"));
            }
        }
        if let Some(text) = content {
            let trimmed = text.trim_end();
            if !trimmed.is_empty() {
                let gutter = format!(
                    "{}{}{RESET}",
                    &self.palette.text_ghost, self.chrome.result_prefix
                );
                let hang = self.chrome.result_continuation();
                let result_width = content_width.saturating_sub(hang.len()).max(1);
                let output_rows: Vec<String> = trimmed
                    .lines()
                    .flat_map(|line| {
                        let colored = format!("{}{line}{RESET}", &self.palette.tool_output_fg);
                        wrap_ansi(&colored, result_width)
                    })
                    .collect();
                let shown = output_rows.len().min(max);
                for (i, row) in output_rows[..shown].iter().enumerate() {
                    if i == 0 {
                        rows.push(format!("{gutter}{row}"));
                    } else {
                        rows.push(format!("{hang}{row}"));
                    }
                }
                if output_rows.len() > max {
                    rows.push(format!(
                        "{hang}{}{}{RESET}",
                        &self.palette.text_tertiary,
                        self.chrome.format_more_lines(output_rows.len() - max)
                    ));
                }
            }
        }
        rows
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
            // The repl-as-tool container (`repl_as_tool` chrome) is one
            // card: the `● Repl` header pushed by `begin_block` and the
            // code that hangs directly under it. A blank separator here
            // would split the card — the header is the block's only row
            // right after `begin_block`, so skip the gap in that case.
            // Nested repls (depth > 0) push into an already-populated
            // block and still want the separator from prior content.
            let just_opened = self.chrome.repl_as_tool && self.block.len() == 1;
            if !just_opened {
                self.ensure_block_gap();
            }
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
        // Accumulate intermediate assistant prose for durable persistence.
        // Drained into ReplRecord.prose at the next CodeExecStart so the
        // model's reasoning between steps survives session resume.
        if !self.pending_prose.is_empty() {
            self.pending_prose.push_str(
                "

",
            );
        }
        self.pending_prose.push_str(text);
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
        prose: &str,
        code: &str,
        stdout: &str,
        stderr: &str,
        _duration_secs: f64,
        tools: &[session::ToolCall],
        subcalls: &[session::SubcallRecord],
    ) {
        // Intermediate prose that preceded this repl block during the live
        // turn. Render it as an assistant prose card so resumed sessions
        // show the same interleaving (breaking up consecutive ReplBlocks).
        // NOTE: do not call `push_prose` here — that would accumulate into
        // `pending_prose`, which must stay empty during replay (it is
        // only populated during live model execution).
        if !prose.trim().is_empty() {
            self.ensure_pending_gap();
            self.pending
                .push(TranscriptEntry::AssistantProse(prose.to_owned()));
            self.last_activity = false;
        }
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
