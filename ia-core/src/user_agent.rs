use crate::config::IaConfig;

/// Build the User-Agent string in the same format as the Python library.
///
/// With an S3 access key configured (archive.org attributes traffic to an
/// account by this key):
/// `ia/{version} ({os} {arch}; N; en; {access_key}) Rust/{rust_version}`
///
/// Without one:
/// `ia/{version} ({os} {arch}; N; en) Rust/{rust_version}`
///
/// The access key is left out if it is not safe to place in a header value
/// inside the parenthesised platform comment.
pub fn build_user_agent(config: &IaConfig) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let rust_version = env!("IA_RUSTC_VERSION");

    let access_key = config
        .s3_access
        .as_deref()
        .filter(|key| is_header_safe_key(key))
        .map(|key| format!("; {key}"))
        .unwrap_or_default();

    let mut ua = format!("ia/{version} ({os} {arch}; N; en{access_key}) Rust/{rust_version}");

    if let Some(suffix) = &config.general.user_agent_suffix {
        ua.push(' ');
        ua.push_str(suffix);
    }

    ua
}

/// True for a non-empty key of visible ASCII with no characters that would
/// end or split the `(...; ...)` comment.
fn is_header_safe_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_graphic() && !matches!(c, '(' | ')' | ';'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IaConfig;

    fn rust_suffix() -> String {
        format!(" Rust/{}", env!("IA_RUSTC_VERSION"))
    }

    fn keyless_prefix() -> String {
        format!(
            "ia/{} ({} {}; N; en",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }

    #[test]
    fn user_agent_keyless_form_unchanged() {
        let config = IaConfig::default();
        assert_eq!(
            build_user_agent(&config),
            format!("{}){}", keyless_prefix(), rust_suffix())
        );
    }

    #[test]
    fn user_agent_includes_access_key() {
        let mut config = IaConfig::default();
        config.s3_access = Some("AbC123xyz".to_string());
        assert_eq!(
            build_user_agent(&config),
            format!("{}; AbC123xyz){}", keyless_prefix(), rust_suffix())
        );
    }

    #[test]
    fn user_agent_includes_access_key_and_suffix() {
        let mut config = IaConfig::default();
        config.s3_access = Some("AbC123xyz".to_string());
        config.general.user_agent_suffix = Some("TestApp/2.0".to_string());
        assert_eq!(
            build_user_agent(&config),
            format!(
                "{}; AbC123xyz){} TestApp/2.0",
                keyless_prefix(),
                rust_suffix()
            )
        );
    }

    #[test]
    fn user_agent_omits_empty_access_key() {
        let mut config = IaConfig::default();
        config.s3_access = Some(String::new());
        assert_eq!(
            build_user_agent(&config),
            format!("{}){}", keyless_prefix(), rust_suffix())
        );
    }

    #[test]
    fn user_agent_omits_access_key_that_is_not_header_safe() {
        for bad in ["has space", "a;b", "a)b", "line\nbreak", "caf\u{e9}"] {
            let mut config = IaConfig::default();
            config.s3_access = Some(bad.to_string());
            assert_eq!(
                build_user_agent(&config),
                format!("{}){}", keyless_prefix(), rust_suffix()),
                "key {bad:?} must be left out"
            );
        }
    }

    #[test]
    fn user_agent_contains_version() {
        let config = IaConfig::default();
        let ua = build_user_agent(&config);
        assert!(ua.starts_with("ia/"));
        assert!(ua.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn user_agent_contains_os_and_arch() {
        let config = IaConfig::default();
        let ua = build_user_agent(&config);
        assert!(ua.contains(std::env::consts::OS));
        assert!(ua.contains(std::env::consts::ARCH));
    }

    #[test]
    fn user_agent_includes_suffix() {
        let mut config = IaConfig::default();
        config.general.user_agent_suffix = Some("TestApp/2.0".to_string());
        let ua = build_user_agent(&config);
        assert!(ua.ends_with("TestApp/2.0"));
    }

    #[test]
    fn user_agent_no_suffix_by_default() {
        let config = IaConfig::default();
        let ua = build_user_agent(&config);
        let last = ua.rsplit(' ').next().unwrap();
        assert!(
            last.starts_with("Rust/"),
            "UA must end with Rust/{{version}}, got: {ua}"
        );
    }

    #[test]
    fn user_agent_includes_rust_version() {
        let config = IaConfig::default();
        let ua = build_user_agent(&config);
        let rust_part = ua
            .split_whitespace()
            .find(|part| part.starts_with("Rust"))
            .expect("UA must contain a Rust token");
        let version = rust_part
            .strip_prefix("Rust/")
            .unwrap_or_else(|| panic!("expected Rust/{{version}}, got: {rust_part}"));
        assert!(
            version.chars().next().is_some_and(|c| c.is_ascii_digit()),
            "Rust version must start with a digit, got: {version}"
        );
    }
}
