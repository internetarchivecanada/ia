use crate::config::IaConfig;

/// Build the User-Agent string in the same format as the Python library:
/// `ia/{version} ({os} {arch}; N; en) Rust/{rust_version}`
pub fn build_user_agent(config: &IaConfig) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let rust_version = env!("IA_RUSTC_VERSION");

    let mut ua = format!("ia/{version} ({os} {arch}; N; en) Rust/{rust_version}");

    if let Some(suffix) = &config.general.user_agent_suffix {
        ua.push(' ');
        ua.push_str(suffix);
    }

    ua
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::IaConfig;

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
