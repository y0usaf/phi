//! Outcome construction and run-completion bookkeeping for [`Rlm`].

use std::time::Instant;

use phi_ai::Usage;

use crate::config::{LimitKind, RlmCompletion, RlmEvent, StopCause};
use crate::lm_core::{LmHandler, UsageBreakdown};

use phi_ext::{EventKind, EventPayload, LifecycleEvent};

use super::super::helpers::dispatch_event;
use super::super::subcalls::Outcome;
use super::core::Rlm;

impl Rlm {
    pub(super) fn complete_run(
        &self,
        outcome: Outcome,
        handler: &LmHandler,
        start: Instant,
    ) -> RlmCompletion {
        let duration = start.elapsed();
        dispatch_event(
            &self.runtime,
            LifecycleEvent {
                kind: EventKind::AgentEnd,
                payload: EventPayload::AgentEnd {
                    stop_cause: format!("{:?}", outcome.stop),
                },
            },
        );
        self.events.emit(RlmEvent::RunEnd {
            depth: self.depth,
            stop: outcome.stop,
            iterations: outcome.iterations,
            duration,
        });
        RlmCompletion {
            response: outcome.response,
            usage: handler.usage(),
            usage_by_model: handler.usage_by_model(),
            iterations: outcome.iterations,
            duration,
            stop: outcome.stop,
            error: outcome.error,
        }
    }

    pub fn complete_run_since(
        &self,
        outcome: Outcome,
        handler: &LmHandler,
        usage_before: &Usage,
        usage_by_model_before: &[UsageBreakdown],
        start: Instant,
    ) -> RlmCompletion {
        let duration = start.elapsed();
        dispatch_event(
            &self.runtime,
            LifecycleEvent {
                kind: EventKind::AgentEnd,
                payload: EventPayload::AgentEnd {
                    stop_cause: format!("{:?}", outcome.stop),
                },
            },
        );
        self.events.emit(RlmEvent::RunEnd {
            depth: self.depth,
            stop: outcome.stop,
            iterations: outcome.iterations,
            duration,
        });
        RlmCompletion {
            response: outcome.response,
            usage: handler.usage_since(usage_before),
            usage_by_model: handler.usage_by_model_since(usage_by_model_before),
            iterations: outcome.iterations,
            duration,
            stop: outcome.stop,
            error: outcome.error,
        }
    }

    pub(super) fn limit_outcome(
        &self,
        kind: LimitKind,
        detail: String,
        best_partial: Option<String>,
        iterations: usize,
    ) -> Outcome {
        self.events.emit(RlmEvent::LimitHit {
            depth: self.depth,
            limit: kind,
            detail: detail.clone(),
        });
        Outcome {
            stop: StopCause::Limit(kind),
            response: best_partial.unwrap_or_else(|| format!("Error: {detail}")),
            error: Some(detail),
            iterations,
        }
    }

    pub(super) fn cancelled_outcome(
        &self,
        best_partial: Option<String>,
        iterations: usize,
    ) -> Outcome {
        Outcome {
            stop: StopCause::Cancelled,
            response: best_partial.unwrap_or_else(|| "Execution cancelled".to_owned()),
            error: Some("Execution cancelled".to_owned()),
            iterations,
        }
    }
}
