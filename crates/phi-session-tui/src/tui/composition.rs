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
        let live_blocks = self.current_live_transcript_blocks();

        // Keep the render cache current. It fully rebuilds only when the
        // terminal width or expand toggles change (every cached entry is
        // width-dependent); new entries are rendered incrementally and
        // appended. Without the cache, every draw() re-renders the entire
        // transcript (markdown parsing, syntax highlighting, REPL block
        // construction) on every keystroke — and without the incremental
        // append, every *new entry* would, stalling long sessions on each
        // completed tool call or message.
        let cur_width = self.term.width();
        let cur_expanded = (
            self.expand_tools,
            self.expand_thinking,
            self.tools.tool_summary,
        );
        if self.transcript_cache_width != cur_width
            || self.transcript_cache_expanded != cur_expanded
        {
            self.invalidate_transcript_cache();
            self.transcript_cache_width = cur_width;
            self.transcript_cache_expanded = cur_expanded;
        }
        self.extend_transcript_cache();

        let mut specs = Vec::with_capacity(self.transcript_cache.len() + live_blocks.len());
        for (key, lines) in &self.transcript_cache {
            specs.push(BlockSpec {
                key,
                lines,
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

        let frame = self.transcript.render(self.transcript_generation, &specs);

        let transcript_live_len = frame.live.len();
        let mut live = frame.live;
        live.extend(chrome);
        let cursor = chrome_cursor.map(|mut cursor| {
            cursor.line += transcript_live_len;
            cursor
        });
        self.term.render(&frame.scrollback, &live, cursor)
    }

    /// Drop the per-entry render cache and bump the content generation.
    /// Every invalidation must go through here: the generation is what
    /// tells `Transcript::render` its durable-prefix assembly may be stale.
    pub(super) fn invalidate_transcript_cache(&mut self) {
        self.transcript_cache.clear();
        self.transcript_cache_entries = 0;
        self.repl_run_start = None;
        self.transcript_generation += 1;
    }

    /// Render entries not yet in the cache and append them. Append-only in
    /// the common case, so a new entry costs one entry render — not a
    /// re-render of the whole history. The one exception is the
    /// consolidated tool-summary card of a still-growing ReplBlock run:
    /// its content changes as the run extends, so the stale card is popped
    /// and re-rendered, and the generation is bumped so `Transcript::render`
    /// drops its durable assembly (same key, different content).
    pub(super) fn extend_transcript_cache(&mut self) {
        let total = self.transcript_entries.len();
        let mut i = self.transcript_cache_entries;
        while i < total {
            if self.tools.tool_summary
                && matches!(self.transcript_entries[i], TranscriptEntry::ReplBlock(..))
            {
                // The run may have started before the cache boundary (a
                // turn appends its blocks one flush at a time); re-collect
                // tools from the run's true start.
                let run_start = self.repl_run_start.unwrap_or(i);
                let mut run_end = i + 1;
                while run_end < total
                    && matches!(
                        self.transcript_entries[run_end],
                        TranscriptEntry::ReplBlock(..)
                    )
                {
                    run_end += 1;
                }
                let mut all_tools: Vec<session::ToolCall> = Vec::new();
                let mut repl_count: usize = 0;
                for entry in &self.transcript_entries[run_start..run_end] {
                    if let TranscriptEntry::ReplBlock(data) = entry {
                        all_tools.extend(data.tools.iter().cloned());
                        repl_count += 1;
                    }
                }
                let summary_key = format!("tx:summary:{run_start}");
                if self
                    .transcript_cache
                    .last()
                    .is_some_and(|(key, _)| *key == summary_key)
                {
                    self.transcript_cache.pop();
                    self.transcript_generation += 1;
                }
                let saved = self.block_active;
                self.block_active = false;
                let summary_lines = self.tool_summary_rows(&all_tools, repl_count);
                self.block_active = saved;
                if !summary_lines.is_empty() {
                    self.transcript_cache.push((summary_key, summary_lines));
                }
                self.repl_run_start = Some(run_start);
                i = run_end;
            } else {
                let entry = self.transcript_entries[i].clone();
                let lines = self.render_entry(&entry);
                if !lines.is_empty() {
                    self.transcript_cache.push((format!("tx:{i}"), lines));
                }
                self.repl_run_start = None;
                i += 1;
            }
        }
        self.transcript_cache_entries = total;
    }

    pub(super) fn flush_pending_to_transcript(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        // Appended entries are picked up incrementally by
        // extend_transcript_cache() on the next draw — no invalidation.
        self.transcript_entries.extend(pending);
    }

    pub(super) fn clear_transcript(&mut self) {
        self.transcript.reset();
        self.transcript_entries.clear();
        self.invalidate_transcript_cache();
        self.pending.clear();
        self.block.clear();
        self.after_block.clear();
        self.last_activity = false;
    }

    pub(super) fn render_entry(&mut self, entry: &TranscriptEntry) -> Vec<String> {
        match entry {
            TranscriptEntry::ReplBlock(data) => {
                let saved = self.block_active;
                self.block_active = true;
                let lines = self.render_repl_block_from_data(data);
                self.block_active = saved;
                lines
            }
            TranscriptEntry::UserMessage(text) => self.user_card_lines(text),
            TranscriptEntry::AssistantProse(text) => self.assistant_card_lines(text),
            TranscriptEntry::RawLines(lines) => lines.clone(),
        }
    }

    pub(super) fn render_repl_block_from_data(&self, data: &ReplBlockData) -> Vec<String> {
        // When tool_summary is enabled, individual REPL blocks are suppressed;
        // a single consolidated summary card (with repl count + tool tallies)
        // is rendered after the run of consecutive blocks (see render loop in
        // draw()).
        if self.tools.tool_summary {
            return Vec::new();
        }
        let mut body = Vec::new();
        // Header (same as begin_block)
        if self.chrome.repl_as_tool {
            body.push(self.repl_anchor_row());
        } else {
            body.push(format!("{}repl{}", &self.palette.tool_title_fg, RESET));
            // Skip the blank separator line when REPL code is suppressed
            // (repl_max_lines == 0): the header stands alone.
            if self.tools.repl_max_lines > 0 {
                body.push(String::new());
            }
        }
        // Code
        body.extend(self.repl_code_lines(0, &data.code, self.code_limit()));
        // Tools
        for tool in &data.tools {
            let depth = tool.depth + 1;
            // Replicate ensure_block_gap: if last body row is non-empty, add blank
            // (unless tools.gap == 0).
            if self.tools.gap > 0 && body.last().is_some_and(|row| !row.is_empty()) {
                body.push(String::new());
            }
            let failed = tool.error.is_some();
            let output = tool.error.as_deref().unwrap_or(&tool.result);
            let lang = if failed {
                None
            } else {
                tool_preview_lang(self.tool_render.get(&tool.name), &tool.args)
            };
            body.extend(self.tool_result_rows(
                depth,
                &tool.name,
                &tool.args,
                output,
                self.output_limit(),
                lang.as_deref(),
                failed,
            ));
        }
        // Subcalls
        for subcall in &data.subcalls {
            let depth = subcall.depth;
            if self.tools.gap > 0 && body.last().is_some_and(|row| !row.is_empty()) {
                body.push(String::new());
            }
            let label = subcall_title(&subcall.model, &subcall.prompt_preview, &self.palette);
            let content = subcall
                .error
                .as_deref()
                .or(subcall.result_preview.as_deref());
            body.extend(self.subcall_result_rows(
                depth,
                &label,
                content,
                self.output_limit(),
                subcall.error.is_some(),
            ));
        }
        // Output + stderr. Tool-card chrome (`repl_as_tool`) puts the `⎿`
        // gutter on the first output row of the container — stdout or,
        // when stdout is empty, stderr — and hangs everything after it,
        // mirroring the live path's once-per-container gutter.
        let gutter = format!(
            "{}{}{RESET}",
            &self.palette.text_ghost, self.chrome.result_prefix
        );
        let hang = self.chrome.result_continuation();
        let mut gutter_used = false;
        let filtered = strip_tool_echoes(&data.stdout, &data.tools, 0);
        for text in [filtered.as_str(), data.stderr.as_str()] {
            let preview_rows = self.preview_rows_with_lang(0, text, self.output_limit(), None);
            if preview_rows.is_empty() {
                continue;
            }
            if self.tools.gap > 0 && body.last().is_some_and(|row| !row.is_empty()) {
                body.push(String::new());
            }
            let blank = indent(0);
            for line in &preview_rows {
                if line.is_empty() {
                    body.push(blank.clone());
                } else if !self.chrome.repl_as_tool {
                    body.push(format!("{}{line}", indent(0)));
                } else if gutter_used {
                    body.push(format!("{hang}{line}"));
                } else {
                    gutter_used = true;
                    body.push(format!("{gutter}{line}"));
                }
            }
        }
        // One neutral container surface: failures are signaled by the ✗
        // glyphs and error text inside, not by tinting the whole container.
        // Flat chrome drops the surface (must mirror commit_block exactly —
        // this is the replay/cache path for the same blocks).
        if self.chrome.is_flat() {
            return body;
        }
        render_repl_block(
            &body,
            &self.palette.repl_bg,
            self.panel_width(),
            self.tools.repl_padding,
        )
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
                let mut lines = vec![String::new()];
                if self.chrome.is_flat() {
                    lines.extend(self.block.iter().cloned());
                } else {
                    lines.extend(render_repl_block(
                        &self.block,
                        &self.palette.tool_pending_bg,
                        self.panel_width(),
                        self.tools.repl_padding,
                    ));
                }
                blocks.push(TranscriptBlock {
                    key: "live:repl-block".to_owned(),
                    lines,
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

            // first_block_start always returns stream.len() now (no fences),
            // so the entire stream is visible prose.
            let visible = self.stream.trim();
            if !visible.is_empty() {
                blocks.push(TranscriptBlock {
                    key: "live:stream-prose".to_owned(),
                    lines: self.assistant_card_lines(visible),
                    finalized: false,
                });
            }
        }

        // Thinking animation at the growing edge of the content — directly
        // where the next output will appear, not floating in the footer.
        // The animation lives at the site of the reward: the indicator +
        // expression sit right after the streaming prose (or where prose
        // will materialize). Chrome picks the shape: phi's stock look is
        // the equalizer with the cyan→magenta gradient and a light glint
        // sweep (hue changes reserved for state changes), claude-code's is
        // the accent-coloured spinner glyph with a shimmer sweep over the
        // verb (a uniform pulse while a tool runs).
        if self.running.is_some() {
            let elapsed = self.clock.now_ms();
            let tool_active = !self.open_tool_rows.is_empty() || !self.open_subcall_rows.is_empty();
            let expression =
                self.visual
                    .thinking_expression(elapsed, tool_active, self.run_started_ms);
            let p = &self.palette;
            let effect = |text: &str| match self.chrome.status_effect {
                phi_ext::StatusEffect::Gradient => {
                    gradient_text_animated(text, p.spinner_accent, p.accent_2, p.glint, elapsed)
                }
                phi_ext::StatusEffect::Sweep if tool_active => {
                    shimmer_flash_text(text, p.spinner_accent, p.shimmer_accent, elapsed)
                }
                phi_ext::StatusEffect::Sweep => {
                    shimmer_sweep_text(text, p.spinner_accent, p.shimmer_accent, elapsed)
                }
            };
            let mut line = match self.chrome.status_indicator {
                // The spinner glyph stays plain accent — only the verb
                // carries the effect (claude-code keeps its asterisk flat).
                phi_ext::StatusIndicator::Spinner => format!(
                    "{}{}{RESET} {}",
                    &p.accent_fg,
                    self.visual.spinner_frame(elapsed),
                    effect(&expression)
                ),
                phi_ext::StatusIndicator::Equalizer => {
                    effect(&format!("{} {expression}", equalizer_glyphs(elapsed)))
                }
            };
            // Dim meta suffix (claude-code semantics): elapsed surfaces
            // only past 30s — short turns stay quiet — tokens once > 0.
            if self.chrome.status_meta {
                let run_s = elapsed.saturating_sub(self.run_started_ms) / 1000;
                let mut parts: Vec<String> = Vec::new();
                if run_s > 30 {
                    parts.push(if run_s < 60 {
                        format!("{run_s}s")
                    } else {
                        format!("{}m {}s", run_s / 60, run_s % 60)
                    });
                }
                if self.usage.total_tokens > 0 {
                    parts.push(format!(
                        "\u{2193} {} tokens",
                        format_tokens(self.usage.total_tokens)
                    ));
                }
                if !parts.is_empty() {
                    line.push_str(&format!(
                        " {}({}){RESET}",
                        &p.text_tertiary,
                        parts.join(" \u{00b7} ")
                    ));
                }
            }
            blocks.push(TranscriptBlock {
                key: "live:equalizer".to_owned(),
                lines: vec![line],
                finalized: false,
            });
        }

        // Queued messages are the last live block — waiting to be sent once
        // the current turn completes, shown at the very bottom, just above
        // the separator line.
        if self.running.is_some() && !self.queued.is_empty() {
            let p = &self.palette;
            let mode = "queued";
            let mut lines = Vec::with_capacity(self.queued.len());
            for (i, text) in self.queued.iter().enumerate() {
                let first = text.lines().next().unwrap_or("");
                let more = if text.lines().count() > 1 { " …" } else { "" };
                let prefix = if i == 0 {
                    let count = if self.queued.len() > 1 {
                        format!(" ×{}", self.queued.len())
                    } else {
                        String::new()
                    };
                    format!("{}{}{} > ", &p.text_tertiary, mode, count)
                } else {
                    format!("{}  > ", &p.text_tertiary)
                };
                lines.push(format!("{}{}{}{}", prefix, first, more, RESET));
            }
            blocks.push(TranscriptBlock {
                key: "live:queued".to_owned(),
                lines,
                finalized: false,
            });
        }

        blocks
    }

    /// Chrome segment 1: brand wordmark + usage. The gradient wordmark IS
    /// the brand signal — no agent icon. Tokens and cost recede to the
    /// tertiary level; they're ambient data, not calls to action.
    pub(super) fn render_chrome_agent(&self) -> String {
        let active = self.usage.total_tokens > 0;
        let mut result = gradient_text("phi", self.palette.accent_2, self.palette.spinner_accent);

        if active {
            let tokens = format_tokens(self.usage.total_tokens);
            result.push_str(&format!(" {}{tokens}{RESET}", &self.palette.text_tertiary));
        }

        if active && self.usage.cost.total > 0.0 {
            result.push_str(&format!(
                " {}${:.2}{RESET}",
                &self.palette.text_tertiary, self.usage.cost.total
            ));
        }

        result
    }

    /// Chrome segment 3: model name (the single bold chrome datum) + the
    /// thinking level in its progression color. No icon — the name is the
    /// information.
    pub(super) fn render_chrome_model(&self) -> String {
        let mut result = String::with_capacity(64);

        let model_id = &self.root_lm.model().id;
        let short_model = model_id.rsplit('/').next().unwrap_or(model_id);
        result.push_str(&format!(
            "{}{short_model}{RESET}",
            &self.palette.chrome_model_fg
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

        if self.branch_at.elapsed() > Duration::from_secs(5) {
            self.branch = git_branch(&self.cwd);
            self.branch_at = std::time::Instant::now();
        }
        // Always render the statusbar first (separator + header), whether a
        // selector is open or not — like Pi's permanent footer. The rule is
        // a divider, not content: it uses the theme's border role, felt more
        // than seen. Ruled input chrome (claude-code's two-line box) keeps
        // the top rule but moves the header below the box, so it reads as a
        // footer instead.
        let sep = format!("{}{}{RESET}", &p.chrome_border, "\u{2500}".repeat(width));
        lines.push(sep.clone());
        let input_ruled = self.chrome.input_ruled && self.selector.is_none();

        let current_dir = self
            .cwd
            .file_name()
            .and_then(|name| name.to_str())
            .map(String::from)
            .unwrap_or_else(|| ".".to_string());
        let elapsed = self.clock.now_ms();

        // Three segments: wordmark+usage · dir/branch · model+thinking.
        // No folder/branch icons — the text already says what it is.
        let agent = self.render_chrome_agent();
        let mut dir = format!("{}{current_dir}{RESET}", &self.palette.chrome_dir_fg);
        if let Some(branch) = &self.branch
            && branch != &current_dir
        {
            dir.push_str(&format!(
                " {}·{RESET} {}{branch}{RESET}",
                &self.palette.text_ghost, &self.palette.chrome_dir_fg
            ));
        }
        let model = self.render_chrome_model();

        // The chrome shimmers only while a run is in flight AND the window
        // has focus: idle = calm, working = alive, blurred = still. The
        // shimmer is percentage-based (position = fraction of section
        // width), so all sections sweep in sync.
        let header = if self.running.is_some() && self.clock.is_focused() {
            let shim = self.palette.working_shimmer;
            format!(
                "{}  {}  {}",
                shimmer_ansi(&agent, shim, elapsed),
                shimmer_ansi(&dir, shim, elapsed),
                shimmer_ansi(&model, shim, elapsed)
            )
        } else {
            format!("{agent}  {dir}  {model}")
        };
        if !input_ruled {
            lines.push(header.clone());
        }

        // Render the selector (if open) or the editor underneath the
        // statusbar.  No box borders — minimalist like Pi.
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
                "type to filter \u{00b7} up/down select \u{00b7} enter confirm \u{00b7} esc cancel"
            } else {
                "up/down select \u{00b7} enter confirm \u{00b7} esc cancel"
            };
            lines.push(format!("{tert}{hint}{RESET}", tert = &p.text_tertiary));
        } else {
            let (editor_lines, (row, col)) = self.editor.render();
            // Editor prompt: glyph on row 0, matching-width indent on continuation.
            // When the window is blurred the chevron dims to ghost — a clear
            // signal that keystrokes are going elsewhere. An empty editor
            // shows a ghost placeholder so the input is never a silent void.
            let prompt_fg = if self.clock.is_focused() {
                &self.palette.chrome_dir_fg
            } else {
                &self.palette.text_ghost
            };
            let placeholder = if self.running.is_some() {
                "queue a follow-up…"
            } else {
                "ready"
            };
            // Chrome may swap the prompt glyph (claude-code's `❯`, crush's
            // `:::`); the stock look keeps the Nerd Font chevron / ASCII
            // fallback. The gutter is glyph + one space, so cursor math and
            // continuation indents track the glyph's display width instead
            // of assuming a single cell.
            let prompt_glyph = self
                .chrome
                .input_prompt
                .as_deref()
                .unwrap_or_else(|| success_symbol());
            let prompt_width = visible_width(prompt_glyph) + 1;
            let cont_indent = " ".repeat(prompt_width);
            let editor_top = lines.len();
            let input_inner = width.saturating_sub(prompt_width).max(1);
            let mut input_cursor = None;
            let mut rendered_rows_before_cursor = 0usize;
            for (absolute_row, line) in editor_lines.iter().enumerate() {
                let content = if absolute_row == 0 {
                    if line.is_empty() && self.editor.text().is_empty() {
                        format!(
                            "{prompt_fg}{prompt_glyph}{RESET} {}{placeholder}{RESET}",
                            &self.palette.text_ghost
                        )
                    } else if line.is_empty() {
                        format!("{prompt_fg}{prompt_glyph}{RESET} ")
                    } else {
                        format!("{prompt_fg}{prompt_glyph}{RESET} {line}")
                    }
                } else {
                    format!("{cont_indent}{line}")
                };
                let avail_inner = if absolute_row == 0 {
                    input_inner
                } else {
                    width
                };
                if absolute_row == row {
                    // Editor cols are byte offsets; wrap marks are display
                    // columns — convert through the text's rendered width.
                    let col_cells = visible_width(&line[..col.min(line.len())]);
                    let source_col = if absolute_row == 0 && line.is_empty() {
                        prompt_width
                    } else {
                        prompt_width + col_cells
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
            cursor = input_cursor;

            // Ruled input chrome: close the box with the bottom rule,
            // claude-code style. The autocomplete popup hangs directly off
            // the box (like the ref's suggestion list); the header follows
            // it, re-reading as the footer line.
            if input_ruled {
                lines.push(sep);
            }

            // (Braille spinner removed — the equalizer animation now lives
            // in the live transcript, above the separator line.)

            if let Some(popup) = &self.popup {
                lines.extend(render_popup(popup, &self.palette));
            }

            if input_ruled {
                lines.push(header);
            }

            // Quit confirmation: the first ctrl+c opens a 500ms window; the
            // hint makes that state visible instead of silently arming it.
            if self
                .last_ctrl_c
                .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
            {
                lines.push(format!(
                    "{}press ctrl+c again to quit{RESET}",
                    &p.warning_fg
                ));
            }

            let hints: &[&str] = if self.login.is_some() {
                &[
                    "finish login in the browser, or paste the redirect URL and press enter \u{00b7} esc cancels",
                ]
            } else if self.popup.is_some() {
                &["tab complete \u{00b7} up/down select \u{00b7} enter accept \u{00b7} esc dismiss"]
            } else if self.running.is_some() {
                &["enter to queue \u{00b7} tab to interrupt"]
            } else {
                &[]
            };
            for hint in hints {
                lines.push(format!("{tert}{hint}{RESET}", tert = &p.text_tertiary));
            }
        }
        (lines, cursor)
    }

    pub(super) fn panel_width(&self) -> usize {
        self.term.width().max(1)
    }

    pub(super) fn repl_body_width(&self) -> usize {
        self.panel_width().saturating_sub(2).max(2)
    }

    pub(super) fn card_body_width(&self, depth: usize) -> usize {
        self.repl_body_width()
            .saturating_sub(result_prefix_width(depth))
            .max(1)
    }

    /// Width for a boxed block (pi `Box(1,1,bgFn)`) that sits inside the
    /// REPL block. `render_repl_block` wraps body lines in a `Box(1,1,bgFn)`
    /// with a 1-space left pad, so child cards must be `panel_width - 1`
    /// to fill the remaining width after the outer box's pad.
    pub(super) fn box_width(&self) -> usize {
        self.panel_width().saturating_sub(1).max(1)
    }
}
