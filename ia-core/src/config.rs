use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ai::types::AiConfig;
use crate::error::{IaError, Result};

#[derive(Debug, Clone, Default)]
pub struct IaConfig {
    pub s3_access: Option<String>,
    pub s3_secret: Option<String>,
    pub cookies: HashMap<String, String>,
    pub general: GeneralConfig,
    pub logging: LoggingConfig,
    pub ai: Option<AiConfig>,
}

#[derive(Debug, Clone)]
pub struct GeneralConfig {
    pub host: String,
    pub secure: bool,
    pub user_agent_suffix: Option<String>,
    pub screenname: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LoggingConfig {
    pub level: Option<String>,
    pub file: Option<PathBuf>,
    pub log_to_stdout: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            host: "archive.org".to_string(),
            secure: true,
            user_agent_suffix: None,
            screenname: None,
        }
    }
}

impl IaConfig {
    /// Load config from the default config file location.
    /// Returns default config if no config file is found.
    pub fn load() -> Result<Self> {
        if let Some(path) = Self::find_config_file() {
            Self::load_from_file(&path)
        } else {
            let mut config = Self::default();
            config.apply_env_overrides();
            Ok(config)
        }
    }

    /// Load config from a specific file path.
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let mut ini = configparser::ini::Ini::new();
        ini.load(path).map_err(|e| IaError::Config(e.to_string()))?;

        let mut config = Self {
            s3_access: ini.get("s3", "access"),
            s3_secret: ini.get("s3", "secret"),
            ..Self::default()
        };

        // [cookies] section
        if let Some(cookies) = ini.get_map_ref().get("cookies") {
            for (key, val) in cookies {
                if let Some(v) = val {
                    config.cookies.insert(key.clone(), v.clone());
                }
            }
        }

        // [general] section
        if let Some(host) = ini.get("general", "host") {
            config.general.host = host;
        }
        if let Some(secure) = ini.get("general", "secure") {
            config.general.secure = secure.to_lowercase() == "true";
        }
        config.general.user_agent_suffix = ini.get("general", "user_agent_suffix");
        config.general.screenname = ini.get("general", "screenname");

        // [logging] section
        config.logging.level = ini.get("logging", "level");
        if let Some(file) = ini.get("logging", "file") {
            config.logging.file = Some(PathBuf::from(file));
        }
        if let Some(stdout) = ini.get("logging", "log_to_stdout") {
            config.logging.log_to_stdout = stdout.to_lowercase() == "true";
        }

        // [ai] section
        if ini.get_map_ref().contains_key("ai") {
            let mut ai = AiConfig::default();
            if let Some(url) = ini.get("ai", "base_url") {
                ai.base_url = url;
            }
            ai.api_key = ini.get("ai", "api_key");
            if let Some(model) = ini.get("ai", "model") {
                ai.model = model;
            }
            if let Some(temp) = ini.get("ai", "temperature") {
                if let Ok(t) = temp.parse::<f64>() {
                    ai.temperature = t;
                }
            }
            if let Some(tokens) = ini.get("ai", "max_tokens") {
                if let Ok(t) = tokens.parse::<u64>() {
                    ai.max_tokens = t;
                }
            }
            config.ai = Some(ai);
        }

        config.apply_env_overrides();
        Ok(config)
    }

    /// Apply environment variable overrides.
    fn apply_env_overrides(&mut self) {
        if let Ok(access) = std::env::var("IA_ACCESS_KEY_ID") {
            if let Ok(secret) = std::env::var("IA_SECRET_ACCESS_KEY") {
                self.s3_access = Some(access);
                self.s3_secret = Some(secret);
            }
        }

        // AI env var overrides
        let has_ai_env = std::env::var("IA_AI_BASE_URL").is_ok()
            || std::env::var("IA_AI_API_KEY").is_ok()
            || std::env::var("IA_AI_MODEL").is_ok();

        if has_ai_env {
            let ai = self.ai.get_or_insert_with(AiConfig::default);
            if let Ok(url) = std::env::var("IA_AI_BASE_URL") {
                ai.base_url = url;
            }
            if let Ok(key) = std::env::var("IA_AI_API_KEY") {
                ai.api_key = Some(key);
            }
            if let Ok(model) = std::env::var("IA_AI_MODEL") {
                ai.model = model;
            }
        }
    }

    /// Find the config file, checking locations in priority order.
    pub fn find_config_file() -> Option<PathBuf> {
        // 1. IA_CONFIG_FILE env var
        if let Ok(path) = std::env::var("IA_CONFIG_FILE") {
            let p = PathBuf::from(path);
            if p.exists() {
                return Some(p);
            }
        }

        // 2. $XDG_CONFIG_HOME/internetarchive/ia.ini
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            let p = PathBuf::from(xdg).join("internetarchive").join("ia.ini");
            if p.exists() {
                return Some(p);
            }
        }

        // 3. ~/.config/internetarchive/ia.ini (default XDG)
        if let Some(home) = dirs_path() {
            let p = home.join(".config").join("internetarchive").join("ia.ini");
            if p.exists() {
                return Some(p);
            }

            // 4. ~/.config/ia.ini (legacy)
            let p = home.join(".config").join("ia.ini");
            if p.exists() {
                return Some(p);
            }

            // 5. ~/.ia (legacy)
            let p = home.join(".ia");
            if p.exists() {
                return Some(p);
            }
        }

        None
    }

    /// The protocol to use for requests.
    pub fn protocol(&self) -> &str {
        if self.general.secure { "https" } else { "http" }
    }
}

fn dirs_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn default_config_has_sane_values() {
        let config = IaConfig::default();
        assert_eq!(config.general.host, "archive.org");
        assert!(config.general.secure);
        assert!(config.s3_access.is_none());
        assert!(config.s3_secret.is_none());
        assert_eq!(config.protocol(), "https");
    }

    #[test]
    fn load_from_ini_file() {
        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("ia.ini");
        let mut f = std::fs::File::create(&ini_path).unwrap();
        writeln!(f, "[s3]").unwrap();
        writeln!(f, "access = test_access").unwrap();
        writeln!(f, "secret = test_secret").unwrap();
        writeln!(f, "[general]").unwrap();
        writeln!(f, "host = test.archive.org").unwrap();
        writeln!(f, "secure = false").unwrap();
        writeln!(f, "user_agent_suffix = MyApp/1.0").unwrap();

        let config = IaConfig::load_from_file(&ini_path).unwrap();
        assert_eq!(config.s3_access.as_deref(), Some("test_access"));
        assert_eq!(config.s3_secret.as_deref(), Some("test_secret"));
        assert_eq!(config.general.host, "test.archive.org");
        assert!(!config.general.secure);
        assert_eq!(config.general.user_agent_suffix.as_deref(), Some("MyApp/1.0"));
        assert_eq!(config.protocol(), "http");
    }

    #[test]
    fn default_config_when_no_file() {
        let config = IaConfig::default();
        assert_eq!(config.general.host, "archive.org");
        assert!(config.general.secure);
    }

    #[test]
    fn insecure_protocol() {
        let mut config = IaConfig::default();
        config.general.secure = false;
        assert_eq!(config.protocol(), "http");
    }

    #[test]
    fn default_config_has_no_ai() {
        let config = IaConfig::default();
        assert!(config.ai.is_none());
    }

    #[test]
    fn load_ai_section_from_ini() {
        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("ia.ini");
        let mut f = std::fs::File::create(&ini_path).unwrap();
        writeln!(f, "[ai]").unwrap();
        writeln!(f, "base_url = http://localhost:11434/v1").unwrap();
        writeln!(f, "api_key = sk-test-key").unwrap();
        writeln!(f, "model = llama3").unwrap();
        writeln!(f, "temperature = 0.5").unwrap();
        writeln!(f, "max_tokens = 2048").unwrap();

        let config = IaConfig::load_from_file(&ini_path).unwrap();
        let ai = config.ai.unwrap();
        assert_eq!(ai.base_url, "http://localhost:11434/v1");
        assert_eq!(ai.api_key.as_deref(), Some("sk-test-key"));
        assert_eq!(ai.model, "llama3");
        assert_eq!(ai.temperature, 0.5);
        assert_eq!(ai.max_tokens, 2048);
    }

    #[test]
    fn ai_section_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("ia.ini");
        let mut f = std::fs::File::create(&ini_path).unwrap();
        writeln!(f, "[ai]").unwrap();
        writeln!(f, "api_key = sk-test").unwrap();

        let config = IaConfig::load_from_file(&ini_path).unwrap();
        let ai = config.ai.unwrap();
        assert_eq!(ai.base_url, "https://api.openai.com/v1");
        assert_eq!(ai.model, "gpt-4o-mini");
        assert_eq!(ai.temperature, 0.2);
        assert_eq!(ai.max_tokens, 4096);
    }

    #[test]
    fn missing_ai_section_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("ia.ini");
        let mut f = std::fs::File::create(&ini_path).unwrap();
        writeln!(f, "[general]").unwrap();
        writeln!(f, "host = archive.org").unwrap();

        let config = IaConfig::load_from_file(&ini_path).unwrap();
        assert!(config.ai.is_none());
    }

    #[test]
    fn ai_env_var_overrides() {
        // Use unique env var names to avoid test interference, but we must test
        // the real env var names. Use a serial approach: set, test, clean up.
        // Note: these tests can interfere with parallel tests that call
        // apply_env_overrides(), but the env vars are cleaned up immediately.
        std::env::set_var("IA_AI_BASE_URL", "http://test:8080/v1");
        std::env::set_var("IA_AI_API_KEY", "env-key");
        std::env::set_var("IA_AI_MODEL", "env-model");

        let mut config = IaConfig::default();
        config.apply_env_overrides();

        // Clean up before assertions to minimize window
        std::env::remove_var("IA_AI_BASE_URL");
        std::env::remove_var("IA_AI_API_KEY");
        std::env::remove_var("IA_AI_MODEL");

        let ai = config.ai.unwrap();
        assert_eq!(ai.base_url, "http://test:8080/v1");
        assert_eq!(ai.api_key.as_deref(), Some("env-key"));
        assert_eq!(ai.model, "env-model");
    }

}
