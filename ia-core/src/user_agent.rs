use crate::config::IaConfig;

/// Build the User-Agent string in the same format as the Python library:
/// `ia/{version} ({os} {arch}; N; en) Rust/{rust_version}`
pub fn build_user_agent(config: &IaConfig) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let mut ua = format!("ia/{version} ({os} {arch}; N; en) Rust");

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
        assert!(ua.ends_with("Rust"));
    }
}
