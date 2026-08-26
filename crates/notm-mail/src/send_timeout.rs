//! Shared validation for the external send-command timeout.

use std::time::{Duration, Instant};

/// Largest supported send timeout: 30 fixed 365-day years.
///
/// This deliberately stays within the cross-platform monotonic-clock horizon
/// used by the async timer instead of accepting TOML integers that cannot be
/// represented as timer deadlines.
pub const MAX_SEND_TIMEOUT_SECONDS: u64 = 30 * 365 * 24 * 60 * 60;

/// Validate an already-parsed send timeout.
pub fn validate_send_timeout_seconds(seconds: u64) -> anyhow::Result<u64> {
    anyhow::ensure!(
        seconds > 0,
        "send.timeout_seconds must be greater than zero"
    );
    anyhow::ensure!(
        seconds <= MAX_SEND_TIMEOUT_SECONDS,
        "send.timeout_seconds must not exceed {MAX_SEND_TIMEOUT_SECONDS}"
    );
    let timeout = Duration::from_secs(seconds);
    anyhow::ensure!(
        Instant::now().checked_add(timeout).is_some(),
        "send.timeout_seconds cannot be represented safely by this system's monotonic timer"
    );
    Ok(seconds)
}

/// Validate a send timeout and convert it to the duration used by the timer.
pub fn send_timeout_duration(seconds: u64) -> anyhow::Result<Duration> {
    validate_send_timeout_seconds(seconds)?;
    Ok(Duration::from_secs(seconds))
}

/// Parse and validate the Settings representation of a send timeout.
pub fn parse_send_timeout_seconds(value: &str) -> anyhow::Result<u64> {
    let seconds = value.trim().parse::<u64>().map_err(|_| {
        anyhow::anyhow!(
            "send.timeout_seconds must be a whole number from 1 through \
             {MAX_SEND_TIMEOUT_SECONDS}"
        )
    })?;
    validate_send_timeout_seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_supported_timer_range_and_builds_deadlines() {
        assert_eq!(parse_send_timeout_seconds(" 1 ").unwrap(), 1);
        assert_eq!(
            parse_send_timeout_seconds(&MAX_SEND_TIMEOUT_SECONDS.to_string()).unwrap(),
            MAX_SEND_TIMEOUT_SECONDS
        );
        for seconds in [1, MAX_SEND_TIMEOUT_SECONDS] {
            let timeout = send_timeout_duration(seconds).unwrap();
            assert_eq!(timeout, Duration::from_secs(seconds));
            assert!(Instant::now().checked_add(timeout).is_some());
        }
    }

    #[test]
    fn rejects_zero_negative_nonnumeric_and_overflowing_values() {
        for invalid in [
            "0".to_string(),
            "-0".to_string(),
            "-1".to_string(),
            "not-a-number".to_string(),
            (MAX_SEND_TIMEOUT_SECONDS + 1).to_string(),
            i64::MAX.to_string(),
            u128::MAX.to_string(),
        ] {
            assert!(
                parse_send_timeout_seconds(&invalid).is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn rejects_programmatic_values_above_the_supported_timer_range() {
        for seconds in [MAX_SEND_TIMEOUT_SECONDS + 1, i64::MAX as u64] {
            let error = send_timeout_duration(seconds)
                .expect_err("timeout above the timer range must fail");
            assert!(error.to_string().contains("must not exceed"));
        }
    }
}
