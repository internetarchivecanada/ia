//! Parsing of byte counts with a binary suffix: the `--min-speed` rate
//! (download and upload) and the `--multipart` cutoff (upload).

/// Parse a byte count: a whole number, or a whole number followed by `K`,
/// `M`, or `G` (either case) for powers of 1024. Surrounding spaces are
/// ignored. `None` for any other form and for a value that overflows u64.
pub(crate) fn parse_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let (digits, multiplier) = match s.chars().last() {
        Some(c) if c.is_ascii_digit() => (s, 1u64),
        Some('k' | 'K') => (&s[..s.len() - 1], 1u64 << 10),
        Some('m' | 'M') => (&s[..s.len() - 1], 1u64 << 20),
        Some('g' | 'G') => (&s[..s.len() - 1], 1u64 << 30),
        _ => return None,
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(multiplier)
}

/// Parse a `--min-speed` value: bytes per second as a plain number, or a
/// number followed by `K`, `M`, or `G` for powers of 1024. `0` is accepted
/// and turns the check off.
pub(crate) fn parse_rate(s: &str) -> std::result::Result<u64, String> {
    parse_bytes(s).ok_or_else(|| {
        format!(
            "`{}` is not a rate: expected bytes per second as a number, or a number \
             followed by K, M, or G for powers of 1024 (for example 10K or 1M); 0 disables",
            s.trim()
        )
    })
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
