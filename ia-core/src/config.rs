use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{IaError, Result};

#[derive(Debug, Clone, Default)]
pub struct IaConfig {
    pub s3_access: Option<String>,
    pub s3_secret: Option<String>,
    pub cookies: HashMap<String, String>,
    pub general: GeneralConfig,
    pub logging: LoggingConfig,
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
}
