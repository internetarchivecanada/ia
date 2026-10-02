//! Parsing of a `--min-speed` rate, shared by download and upload.

/// Parse a `--min-speed` value: bytes per second as a plain number, or a
/// number followed by `K`, `M`, or `G` for powers of 1024. `0` is accepted
/// and turns the check off.
pub(crate) fn parse_rate(s: &str) -> std::result::Result<u64, String> {
    let s = s.trim();
    let err = || {
        format!(
            "`{s}` is not a rate: expected bytes per second as a number, or a number \
             followed by K, M, or G for powers of 1024 (for example 10K or 1M); 0 disables"
        )
    };
    let (digits, multiplier) = match s.chars().last() {
        Some(c) if c.is_ascii_digit() => (s, 1u64),
        Some('k' | 'K') => (&s[..s.len() - 1], 1u64 << 10),
        Some('m' | 'M') => (&s[..s.len() - 1], 1u64 << 20),
        Some('g' | 'G') => (&s[..s.len() - 1], 1u64 << 30),
        _ => return Err(err()),
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(err());
    }
    let n: u64 = digits.parse().map_err(|_| err())?;
    n.checked_mul(multiplier).ok_or_else(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rate_accepts_plain_bytes_and_binary_suffixes() {
        assert_eq!(parse_rate("500"), Ok(500));
        assert_eq!(parse_rate("0"), Ok(0));
        assert_eq!(parse_rate("10K"), Ok(10 * 1024));
        assert_eq!(parse_rate("10k"), Ok(10 * 1024));
        assert_eq!(parse_rate("1M"), Ok(1024 * 1024));
        assert_eq!(parse_rate("1G"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse_rate(" 2M "), Ok(2 * 1024 * 1024));
    }

    #[test]
    fn parse_rate_rejects_other_forms_and_names_the_accepted_ones() {
        for bad in ["10KB", "1.5M", "abc", "", "-1", "K", "10 K", "1T"] {
            let err = parse_rate(bad).unwrap_err();
            assert!(err.contains("10K"), "{bad:?}: {err}");
            assert!(err.contains("1M"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn parse_rate_rejects_overflow() {
        assert!(parse_rate("99999999999999999999").is_err());
        assert!(parse_rate("18446744073709551615G").is_err());
    }
}
