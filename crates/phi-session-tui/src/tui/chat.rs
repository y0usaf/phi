use super::*;


impl Ui {
    pub(super) fn assistant_card_lines(&self, text: &str) -> Vec<String> {
        let rendered = render_markdown(text, self.markdown_width(), &self.palette.markdown);
        let mut out = vec![forge_title(0, None, "Assistant")];
        for line in rendered {
            out.push(if line.is_empty() {
                String::new()
            } else {
                format!("{fg}{line}{RESET}", fg = &self.palette.assistant_fg)
            });
        }
        out
    }

    pub(super) fn user_card_lines(&self, text: &str) -> Vec<String> {
        let rendered = render_markdown(text, self.markdown_width(), &self.palette.markdown);
        let mut out = vec![forge_title(0, None, "You")];
        for line in rendered {
            out.push(if line.is_empty() {
                String::new()
            } else {
                format!("{fg}{line}{RESET}", fg = &self.palette.user_fg)
            });
        }
        out
    }

    pub(super) fn tool_lines(&self, depth: usize, title: &str, body: &str, max: usize) -> Vec<String> {
        let mut rows = Vec::new();
        let trimmed = body.trim_end();
        if !trimmed.is_empty() {
            let output_rows: Vec<String> = trimmed
                .lines()
                .flat_map(|line| wrap_ansi(line, self.card_body_width(depth)))
                .collect();
            let shown = output_rows.len().min(max);
            for row in &output_rows[..shown] {
                rows.push(format!("{DIM}{row}{RESET}"));
            }
            if output_rows.len() > max {
                rows.push(format!(
                    "{DIM}… ({} more lines){RESET}",
                    output_rows.len() - max
                ));
            }
        }

        render_flat(None, title, depth, &rows)
    }

    pub(super) fn repl_code_lines(&self, depth: usize, body: &str, max: usize) -> Vec<String> {
        let trimmed = body.trim_end();
        let mut rows = Vec::new();
        if !trimmed.is_empty() {
            rows = highlight_code(trimmed, Some("repl"), &self.palette.markdown)
                .into_iter()
                .flat_map(|line| wrap_ansi(&line, self.card_body_width(depth)))
                .collect();
            if rows.len() > max {
                let hidden = rows.len() - max;
                rows.truncate(max);
                rows.push(format!("{DIM}… ({hidden} more lines){RESET}"));
            }
        }
        let prefix = result_prefix(depth);
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
        vec![format!("{}{text}", indent(depth))]
    }

    pub(super) fn push_status(&mut self, depth: usize, text: String) -> Option<(usize, usize)> {
        if depth == 0 && !self.last_activity && !self.block_active {
            self.pending.push(String::new());
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

    pub(super) fn replace_block_rows(&mut self, start: usize, len: usize, rows: Vec<String>) -> usize {
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
        let spinner = self.spinner.frame().to_owned();

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
                self.status_rows(row.depth, &running_status_text(&row.label, &spinner)),
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
                self.status_rows(row.depth, &running_status_text(&row.label, &spinner)),
            );
            if let Some(open) = self.open_subcall_rows.get_mut(idx) {
                open.len = new_len;
            }
        }
    }

    pub(super) fn fail_open_activity_rows(&mut self) {
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
                self.status_rows(row.depth, &failed_status_text(&row.label, None, None, &self.palette)),
            );
        }
        while let Some(row) = self.open_subcall_rows.pop_back() {
            self.replace_block_rows(
                row.start,
                row.len,
                self.status_rows(row.depth, &failed_status_text(&row.label, None, None, &self.palette)),
            );
        }
    }

    pub(super) fn push_row(&mut self, row: String) {
        if self.block_active {
            self.block.push(row);
        } else {
            self.pending.push(row);
        }
    }

    pub(super) fn begin_block(&mut self) {
        if !self.last_activity {
            self.pending.push(String::new());
        }
        self.block.clear();
        self.open_tool_rows.clear();
        self.open_subcall_rows.clear();
        self.block_active = true;
    }

    pub(super) fn commit_block(&mut self) {
        if !self.block_active {
            return;
        }
        self.block_active = false;
        let body = std::mem::take(&mut self.block);
        // The `repl · turn N` label belongs to the *live* action block built
        // during an iteration (running is Some). Persisted assistant messages
        // re-rendered as prose containers (`push_assistant_message`, during
        // restore or after a run completes) are a different artifact with no
        // meaningful per-block turn, so they stay at a plain `repl` label.
        let turn = self.running.is_some().then_some(self.turn);
        self.pending.extend(render_repl_block(&body, turn));
        self.open_tool_rows.clear();
        self.open_subcall_rows.clear();
        let after = std::mem::take(&mut self.after_block);
        self.pending.extend(after);
    }

    pub(super) fn push_dot(&mut self, depth: usize, text: String) {
        if depth == 0 && !self.last_activity && !self.block_active {
            self.pending.push(String::new());
        }
        self.push_row(forge_title(
            depth,
            self.committed_ts().as_deref(),
            &text,
        ));
        self.last_activity = true;
    }

    pub(super) fn push_bar(&mut self, depth: usize, text: String) {
        let rows = render_flat(None, &format!("{DIM}{text}{RESET}"), depth, &[]);
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
        let mut rows: Vec<String> = base_rows
            .into_iter()
            .flat_map(|line| wrap_ansi(&line, self.card_body_width(depth)))
            .collect();
        if rows.len() > max {
            let hidden = rows.len() - max;
            rows.truncate(max);
            rows.push(format!("{DIM}… ({hidden} more lines){RESET}"));
        }
        rows
    }

    pub(super) fn push_preview_with_lang(&mut self, depth: usize, text: &str, max: usize, lang: Option<&str>) {
        let rows = self.preview_rows_with_lang(depth, text, max, lang);
        if rows.is_empty() {
            return;
        }
        let prefix = result_prefix(depth);
        let blank = indent(depth);
        for line in &rows {
            self.push_row(if line.is_empty() {
                blank.clone()
            } else {
                format!("{prefix}{line}")
            });
        }
    }

    pub(super) fn committed_ts(&self) -> Option<String> {
        self.running.is_some().then(now_hms)
    }

    pub(super) fn tool_result_rows(
        &self,
        depth: usize,
        name: &str,
        args: &serde_json::Value,
        output: &str,
        max: usize,
        lang: Option<&str>,
        _failed: bool,
        ts: Option<&str>,
    ) -> Vec<String> {
        let identity = tool_identity(name, args, &self.palette);
        // The colored `●` carries success/failure (green/red); the in-flight
        // spinner label uses the same anchor via `tool_title`, so running and
        // committed render the same identity (no flicker on done). The header
        // carries the canonical identity (path/command/pattern) once; the body
        // folds multi-line detail (full bash command / repl code) under it. For
        // tools whose identity IS the anchor, the body is empty and only the
        // output preview shows.
        let width = self.card_body_width(depth);
        let mut body: Vec<String> = identity
            .body
            .into_iter()
            .flat_map(|line| wrap_ansi(&line, width))
            .collect();
        body.extend(self.preview_rows_with_lang(depth, output, max, lang));
        render_flat(ts, &identity.anchor, depth, &body)
    }

    pub(super) fn subcall_result_rows(
        &self,
        depth: usize,
        label: &str,
        content: Option<&str>,
        max: usize,
        _failed: bool,
        ts: Option<&str>,
    ) -> Vec<String> {
        let body = content
            .map(|text| self.preview_rows_with_lang(depth, text, max, None))
            .unwrap_or_default();
        render_flat(ts, label, depth, &body)
    }

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
        if depth == 0 && !self.last_activity && !self.block_active {
            self.pending.push(String::new());
        }
        let rows = self.tool_result_rows(
            depth,
            name,
            args,
            output,
            max,
            lang,
            failed,
            self.committed_ts().as_deref(),
        );
        for row in rows {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    pub(super) fn push_repl_block(&mut self, depth: usize, text: &str, max: usize) {
        for row in self.repl_code_lines(depth, text, max) {
            self.push_row(row);
        }
        self.last_activity = true;
    }

    pub(super) fn push_user_message(&mut self, text: &str) {
        self.pending.push(String::new());
        self.pending.extend(self.user_card_lines(text));
        self.last_activity = false;
    }

    pub(super) fn push_prose(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let card = self.assistant_card_lines(text);
        let sink = if self.block_active {
            &mut self.after_block
        } else {
            &mut self.pending
        };
        sink.push(String::new());
        sink.extend(card);
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

        let blocks = find_code_blocks(text);
        let segments = split_prose(text);
        for (i, segment) in segments.iter().enumerate() {
            self.push_prose(segment);
            if let Some(code) = blocks.get(i) {
                self.begin_block();
                self.push_repl_block(0, code, self.code_limit());
                self.commit_block();
            }
        }
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
        self.begin_block();
        self.push_repl_block(0, code, self.code_limit());
        // Replay the persisted tool calls and sub-LLM calls nested in the
        // block, mirroring the live ToolStart/ToolEnd rendering so a
        // resumed session looks like the one the user actually saw.
        for tool in tools {
            let depth = tool.depth + 1;
            match &tool.error {
                Some(error) => {
                    self.push_tool_result(
                        depth,
                        &tool.name,
                        &tool.args,
                        error,
                        self.output_limit(),
                        None,
                        true,
                    );
                }
                None => {
                    self.push_tool_result(
                        depth,
                        &tool.name,
                        &tool.args,
                        &tool.result,
                        self.output_limit(),
                        tool_preview_lang(&tool.name, &tool.args),
                        false,
                    );
                }
            }
        }
        for subcall in subcalls {
            let depth = subcall.depth;
            let label = subcall_title(&subcall.model, &subcall.prompt_preview, &self.palette);
            let content = subcall
                .error
                .as_deref()
                .or(subcall.result_preview.as_deref());
            let rows = self.subcall_result_rows(
                depth,
                &label,
                content,
                self.output_limit(),
                subcall.error.is_some(),
                None,
            );
            for row in rows {
                self.push_row(row);
            }
            self.last_activity = true;
        }
        self.last_activity = true;
        let filtered = strip_tool_echoes(stdout, tools, 0);
        self.push_preview(0, &filtered, self.output_limit());
        self.push_preview(0, stderr, self.output_limit());
        self.commit_block();
    }

    pub(super) fn cycle_thinking(&mut self) {
        let current = self.root_lm.thinking();
        let idx = ThinkingLevel::ALL
            .iter()
            .position(|l| *l == current)
            .unwrap_or(0);
        let next = ThinkingLevel::ALL[(idx + 1) % ThinkingLevel::ALL.len()];
        self.root_lm.set_thinking(next);
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
        self.pending.push(format!(
            "{DIM}[tool output {} throughout transcript]{RESET}",
            if self.expand_tools {
                "expanded"
            } else {
                "collapsed"
            }
        ));
    }

    pub(super) fn code_limit(&self) -> usize {
        if self.expand_tools {
            usize::MAX
        } else {
            CODE_PREVIEW_LINES
        }
    }

    pub(super) fn output_limit(&self) -> usize {
        if self.expand_tools {
            usize::MAX
        } else {
            OUTPUT_PREVIEW_LINES
        }
    }
}
