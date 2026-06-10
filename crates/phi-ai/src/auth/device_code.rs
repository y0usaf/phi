//! Generic RFC 8628 device-code polling engine.

use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::auth::AuthError;

const MINIMUM_INTERVAL: Duration = Duration::from_secs(1);
/// RFC 8628 section 3.2: default to 5 s when the server omits `interval`.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// RFC 8628 section 3.5: `slow_down` increases the interval by 5 s.
const SLOW_DOWN_INCREMENT: Duration = Duration::from_secs(5);

pub enum PollOutcome<T> {
    Pending,
    SlowDown,
    Failed(String),
    Complete(T),
}

pub async fn poll_device_code<T, F, Fut>(
    interval: Option<Duration>,
    expires_in: Duration,
    cancel: &CancellationToken,
    mut poll: F,
) -> Result<T, AuthError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<PollOutcome<T>, AuthError>>,
{
    let deadline = Instant::now() + expires_in;
    let mut interval = interval
        .unwrap_or(DEFAULT_POLL_INTERVAL)
        .max(MINIMUM_INTERVAL);
    let mut slow_downs = 0u32;

    while Instant::now() < deadline {
        if cancel.is_cancelled() {
            return Err(AuthError::Cancelled);
        }

        match poll().await? {
            PollOutcome::Complete(value) => return Ok(value),
            PollOutcome::Failed(message) => return Err(AuthError::Protocol(message)),
            PollOutcome::SlowDown => {
                slow_downs += 1;
                interval = (interval + SLOW_DOWN_INCREMENT).max(MINIMUM_INTERVAL);
            }
            PollOutcome::Pending => {}
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            () = cancel.cancelled() => return Err(AuthError::Cancelled),
            () = tokio::time::sleep(interval.min(remaining)) => {}
        }
    }

    Err(AuthError::Protocol(if slow_downs > 0 {
        "Device flow timed out after one or more slow_down responses. This is often caused by \
         clock drift in WSL or VM environments. Please sync or restart the VM clock and try again."
            .to_owned()
    } else {
        "Device flow timed out".to_owned()
    }))
}
