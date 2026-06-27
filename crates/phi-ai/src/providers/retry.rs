//! Retry logic: transient error classification, backoff, and retry-after parsing.

use std::time::Duration;

pub(crate) const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// A fresh, unpooled client for mid-stream retry attempts. After a transient
/// transport failure the pooled connection that broke may have poisoned other
/// idle connections on the same path (e.g. a TLS BadRecordMac from a corrupting
/// middlebox). Disabling idle pooling forces a new TLS handshake per retry.
pub(crate) fn fresh_transport_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(0)
        .build()
}

pub(crate) fn retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 408 || status.as_u16() == 429 || status.is_server_error()
}

/// Exponential backoff with +/-20% jitter to spread retries during correlated
/// upstream outages (thundering herd).
pub(crate) fn retry_delay(attempt: u32, retry_after: Option<Duration>) -> Duration {
    let base = retry_after
        .unwrap_or_else(|| Duration::from_millis(1000u64.saturating_mul(1 << attempt.min(10))));
    let jitter = rand::random_range(0.8f32..1.2f32);
    let millis = (base.as_millis() as f32 * jitter) as u64;
    Duration::from_millis(millis).min(MAX_RETRY_DELAY)
}

/// Classifies a reqwest transport error as transient (safe to retry) vs
/// terminal. Mid-stream "error decoding response body" failures (truncated
/// content-length mismatch, HTTP/2 stream errors, connection resets) are
/// transient -- the request can be reissued from the top.
pub(crate) fn is_transient_transport_error(error: &reqwest::Error) -> bool {
    let msg = error.to_string().to_lowercase();
    const TRANSIENT_MARKERS: &[&str] = &[
        "error decoding",
        "unexpected eof",
        "decoding body",
        "http2 error",
        "http/2",
        "stream error",
        "connection reset",
        "broken pipe",
        "timed out",
        "timeout",
        "dns",
        "name resolution",
        "incomplete message",
        "protocol error",
        "end of file before",
        "request body write was interrupted",
    ];
    TRANSIENT_MARKERS.iter().any(|m| msg.contains(m))
}

pub(crate) fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_applies_jitter() {
        // The jittered delay should stay within +/-20% of the base, bounded
        // by MAX_RETRY_DELAY. Run several samples to confirm the range.
        for attempt in 0..5u32 {
            let base = Duration::from_millis(1000u64.saturating_mul(1 << attempt.min(10)));
            for _ in 0..20 {
                let d = retry_delay(attempt, None);
                let min = Duration::from_millis((base.as_millis() as f32 * 0.8) as u64);
                let max = Duration::from_millis((base.as_millis() as f32 * 1.2) as u64);
                assert!(
                    d >= min && d <= max.min(MAX_RETRY_DELAY),
                    "attempt={attempt} d={d:?} base={base:?}"
                );
            }
        }
    }
}
