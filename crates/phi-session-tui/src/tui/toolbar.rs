use super::*;

impl Ui {
    pub(super) fn request_host_model_modal(&mut self, root: bool) -> bool {
        let Some(host_modal_tx) = &self.host_modal_tx else {
            return false;
        };
        let current = {
            let m = if root {
                self.root_lm.model()
            } else {
                self.sub_lm.model()
            };
            format!("{}/{}", m.provider, m.id)
        };
        let kind = HostModalKind::Model {
            root,
            current: current.clone(),
        };
        let (response_tx, response_rx) = oneshot::channel();
        if host_modal_tx
            .send(HostModalRequest {
                kind: kind.clone(),
                response_tx,
            })
            .is_err()
        {
            return false;
        }
        self.pending_host_modal = Some(PendingHostModal { kind, response_rx });
        true
    }

    pub(super) fn request_host_usage_modal(&mut self) -> bool {
        let Some(host_modal_tx) = &self.host_modal_tx else {
            return false;
        };
        let kind = HostModalKind::Usage;
        let (response_tx, response_rx) = oneshot::channel();
        if host_modal_tx
            .send(HostModalRequest {
                kind: kind.clone(),
                response_tx,
            })
            .is_err()
        {
            return false;
        }
        self.pending_host_modal = Some(PendingHostModal { kind, response_rx });
        true
    }

    pub(super) fn open_usage_selector(&mut self) {
        let report = load_usage_report();
        let mut items = vec![SelectorItem::new(
            "__usage_total__",
            format!(
                "entries {} · files {} · total {} tok · ${:.4}",
                report.entries,
                report.files_scanned,
                format_tokens(report.totals.total_tokens()),
                report.totals.total_cost
            ),
        )];
        for (index, line) in usage_overlay_lines(&report).into_iter().enumerate() {
            items.push(SelectorItem::new(format!("usage-{index}"), line));
        }
        let widget = Selector::new(format!("Usage · {} entries", report.entries), items, false)
            .with_style(self.selector_style());
        self.open_selector(SelectorKind::Usage, widget);
    }

    pub(super) fn open_model_selector(&mut self, root: bool) {
        if self.model_names.is_empty() {
            return;
        }
        let current = {
            let m = if root {
                self.root_lm.model()
            } else {
                self.sub_lm.model()
            };
            format!("{}/{}", m.provider, m.id)
        };
        let items = self
            .model_names
            .iter()
            .map(|name| {
                let (provider, id) = name.split_once('/').unwrap_or(("", name));
                let detail = self
                    .registry
                    .find(name)
                    .map(|model| format!("[{provider}] {}", format_model_pricing(model)))
                    .unwrap_or_else(|| format!("[{provider}]"));
                SelectorItem::new(name.clone(), id.to_owned())
                    .detail(detail)
                    .current(*name == current)
            })
            .collect();
        let title = if root {
            "Switch Model"
        } else {
            "Switch Sub Model"
        };
        let mut widget = Selector::new(title, items, true).with_style(self.selector_style());
        widget.preselect(&current);
        self.open_selector(SelectorKind::Model { root }, widget);
    }

    pub(super) fn cycle_model(&mut self, back: bool) {
        if self.model_names.is_empty() {
            return;
        }
        let current = {
            let m = self.root_lm.model();
            format!("{}/{}", m.provider, m.id)
        };
        let len = self.model_names.len();
        let next = match self.model_names.iter().position(|n| *n == current) {
            Some(i) if back => (i + len - 1) % len,
            Some(i) => (i + 1) % len,
            None => 0,
        };
        let name = self.model_names[next].clone();
        self.switch_model(&name, true);
    }

    pub(super) fn switch_model(&mut self, query: &str, root: bool) {
        let Some(model) = self.registry.find(query).cloned() else {
            self.pending
                .push(TranscriptEntry::RawLines(vec![self.err(&format!(
                    "unknown model '{query}' (tab after /model lists models)"
                ))]));
            return;
        };
        if !self.registry.is_supported(&model) {
            self.pending
                .push(TranscriptEntry::RawLines(vec![self.err(&format!(
                    "model '{}/{}' is not supported",
                    model.provider, model.id
                ))]));
            return;
        }
        let (target, which) = if root {
            (&self.root_lm, "model")
        } else {
            (&self.sub_lm, "sub model")
        };
        let previous = target.model();
        let new_provider = model.provider.clone();
        let new_id = model.id.clone();
        target.set_model(model.clone());
        self.model_label = model_label(&self.root_lm.model(), &self.sub_lm.model());
        self.pending.push(TranscriptEntry::RawLines(vec![
            self.note(&format!("[{which} → {}/{new_id}]", new_provider)),
        ]));
        self.warn_if_missing_credentials(new_provider.clone());

        // ── Extension: ModelSelect ──────────────────────────────────────
        let rt = self.runtime.clone();
        let new_full = format!("{}/{new_id}", new_provider);
        let prev_full = format!("{}/{}", previous.provider, previous.id);
        let source = if root { "model" } else { "sub_model" };
        tokio::spawn(async move {
            let _ = rt
                .on_model_select(&new_full, Some(&prev_full), source)
                .await;
        });
    }
}
