//! Shared validation for the external send-command timeout.

/// Largest timeout that can be represented by a TOML integer and persisted by
/// the Settings dialog.
pub const MAX_SEND_TIMEOUT_SECONDS: u64 = 9_223_372_036_854_775_807;

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
    Ok(seconds)
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
    fn accepts_the_full_persistable_timeout_range() {
        assert_eq!(parse_send_timeout_seconds(" 1 ").unwrap(), 1);
        assert_eq!(
            parse_send_timeout_seconds(&MAX_SEND_TIMEOUT_SECONDS.to_string()).unwrap(),
            MAX_SEND_TIMEOUT_SECONDS
        );
    }

    #[test]
    fn rejects_zero_negative_nonnumeric_and_overflowing_values() {
        for invalid in [
            "0".to_string(),
            "-0".to_string(),
            "-1".to_string(),
            "not-a-number".to_string(),
            (MAX_SEND_TIMEOUT_SECONDS + 1).to_string(),
            u128::MAX.to_string(),
        ] {
            assert!(
                parse_send_timeout_seconds(&invalid).is_err(),
                "unexpectedly accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn rejects_programmatic_values_that_toml_cannot_persist() {
        let error = validate_send_timeout_seconds(MAX_SEND_TIMEOUT_SECONDS + 1)
            .expect_err("timeout above the TOML integer range must fail");
        assert!(error.to_string().contains("must not exceed"));
    }
}
