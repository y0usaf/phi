//! Per-session status derived from depth-0 engine events; drives the
//! sidebar spinner and statusline glyphs.

use phi_rlm::RlmEvent;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    /// No run in flight; the session is waiting for input.
    Idle,
    /// A run is in flight, waiting on the model or executing code.
    Thinking,
    /// The root model is streaming text.
    Outputting,
    /// A prelude tool (shell, edit_file, ...) is executing.
    Tool { name: String },
    /// The session task panicked; it can be restarted in place.
    Crashed,
    /// The session task exited cleanly.
    Done,
}

impl SessionStatus {
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Thinking | Self::Outputting | Self::Tool { .. })
    }
}

/// Status implied by an engine event. Only depth-0 events count: nested
/// (subagent) activity stays under the root run's status.
pub fn status_for_event(event: &RlmEvent) -> Option<SessionStatus> {
    Some(match event {
        RlmEvent::RunStart { depth: 0 } => SessionStatus::Thinking,
        RlmEvent::RootDelta { depth: 0, .. }
        | RlmEvent::RootThinkingDelta { depth: 0, .. }
        | RlmEvent::RootText { depth: 0, .. } => SessionStatus::Outputting,
        RlmEvent::ToolStart { depth: 0, name, .. } => SessionStatus::Tool { name: name.clone() },
        RlmEvent::ToolEnd { depth: 0, .. } => SessionStatus::Thinking,
        RlmEvent::RunEnd { depth: 0, .. } => SessionStatus::Idle,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use phi_rlm::StopCause;
    use std::time::Duration;

    #[test]
    fn depth_zero_events_map_to_statuses() {
        assert_eq!(
            status_for_event(&RlmEvent::RunStart { depth: 0 }),
            Some(SessionStatus::Thinking)
        );
        assert_eq!(
            status_for_event(&RlmEvent::RootDelta {
                depth: 0,
                iteration: 0,
                delta: "hi".into()
            }),
            Some(SessionStatus::Outputting)
        );
        assert_eq!(
            status_for_event(&RlmEvent::ToolStart {
                depth: 0,
                call_id: 1,
                name: "shell".into(),
                args: serde_json::Value::Null
            }),
            Some(SessionStatus::Tool {
                name: "shell".into()
            })
        );
        assert_eq!(
            status_for_event(&RlmEvent::RunEnd {
                depth: 0,
                stop: StopCause::Answer,
                iterations: 1,
                duration: Duration::ZERO
            }),
            Some(SessionStatus::Idle)
        );
    }

    #[test]
    fn nested_events_are_ignored() {
        assert_eq!(status_for_event(&RlmEvent::RunStart { depth: 1 }), None);
        assert_eq!(
            status_for_event(&RlmEvent::ToolStart {
                depth: 2,
                call_id: 1,
                name: "shell".into(),
                args: serde_json::Value::Null
            }),
            None
        );
    }
}
