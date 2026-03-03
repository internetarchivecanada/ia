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

    /// Serialize config to JSON, optionally redacting secrets.
    ///
    /// When `redact` is true, S3 keys, cookies, and AI API key are replaced
    /// with `"REDACTED"`. Used by `ia config show` to safely display config.
    pub fn to_json(&self, redact: bool) -> serde_json::Value {
        let redacted = serde_json::json!("REDACTED");

        let s3 = serde_json::json!({
            "access": if redact {
                redacted.clone()
            } else {
                self.s3_access.as_deref()
                    .map(|s| serde_json::Value::String(s.to_owned()))
                    .unwrap_or(serde_json::Value::Null)
            },
            "secret": if redact {
                redacted.clone()
            } else {
                self.s3_secret.as_deref()
                    .map(|s| serde_json::Value::String(s.to_owned()))
                    .unwrap_or(serde_json::Value::Null)
            },
        });

        let cookies: serde_json::Value = if redact {
            let mut map = serde_json::Map::new();
            for key in self.cookies.keys() {
                map.insert(key.clone(), redacted.clone());
            }
            serde_json::Value::Object(map)
        } else {
            serde_json::json!(self.cookies)
        };

        let general = serde_json::json!({
            "host": self.general.host,
            "secure": self.general.secure,
            "screenname": self.general.screenname,
            "user_agent_suffix": self.general.user_agent_suffix,
        });

        let logging = serde_json::json!({
            "level": self.logging.level,
            "file": self.logging.file.as_ref().map(|p| p.display().to_string()),
            "log_to_stdout": self.logging.log_to_stdout,
        });

        let mut obj = serde_json::json!({
            "s3": s3,
            "cookies": cookies,
            "general": general,
            "logging": logging,
        });

        if let Some(ai) = &self.ai {
            let ai_val = if redact {
                serde_json::json!({
                    "base_url": ai.base_url,
                    "api_key": redacted,
                    "model": ai.model,
                    "temperature": ai.temperature,
                    "max_tokens": ai.max_tokens,
                })
            } else {
                serde_json::json!({
                    "base_url": ai.base_url,
                    "api_key": ai.api_key,
                    "model": ai.model,
                    "temperature": ai.temperature,
                    "max_tokens": ai.max_tokens,
                })
            };
            obj["ai"] = ai_val;
        }

        obj
    }

    /// Write authentication credentials to a config file, merging with existing content.
    ///
    /// Creates parent directories (mode 0o700) and sets file permissions to 0o600.
    pub fn write_config_file(auth: &crate::auth::AuthConfig, path: &Path) -> Result<()> {
        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
                }
            }
        }

        // Load existing config or start fresh
        let mut ini = configparser::ini::Ini::new();
        if path.exists() {
            let _ = ini.load(path); // ignore errors on existing file — we'll overwrite
        }

        // Merge auth values
        ini.set("s3", "access", Some(auth.s3_access.clone()));
        ini.set("s3", "secret", Some(auth.s3_secret.clone()));
        ini.set("cookies", "logged-in-user", Some(auth.logged_in_user.clone()));
        ini.set("cookies", "logged-in-sig", Some(auth.logged_in_sig.clone()));
        ini.set("general", "screenname", Some(auth.screenname.clone()));

        // Write the INI file
        ini.write(path).map_err(|e| IaError::Config(format!("failed to write config: {e}")))?;

        // Set file permissions to 0o600 (owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    /// Find the config file to write to, falling back to the XDG default.
    ///
    /// Priority: existing file (same search as `find_config_file`) then XDG default path.
    pub fn find_or_default_config_path() -> PathBuf {
        if let Some(existing) = Self::find_config_file() {
            return existing;
        }

        // Default to XDG location
        let config_home = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|s| !s.is_empty() && PathBuf::from(s).is_absolute())
            .map(PathBuf::from)
            .or_else(|| dirs_path().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from(".config"));

        config_home.join("internetarchive").join("ia.ini")
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

    #[test]
    fn write_config_creates_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("ia.ini");

        let auth = crate::auth::AuthConfig {
            s3_access: "new-access".into(),
            s3_secret: "new-secret".into(),
            logged_in_user: "user%40example.com".into(),
            logged_in_sig: "sig-value".into(),
            screenname: "testuser".into(),
            itemname: Some("@testuser".into()),
        };

        let result = IaConfig::write_config_file(&auth, &ini_path);
        assert!(result.is_ok(), "write_config_file failed: {:?}", result.err());

        // Read it back and verify
        let config = IaConfig::load_from_file(&ini_path).unwrap();
        assert_eq!(config.s3_access.as_deref(), Some("new-access"));
        assert_eq!(config.s3_secret.as_deref(), Some("new-secret"));
        assert_eq!(config.cookies.get("logged-in-user").map(|s| s.as_str()), Some("user%40example.com"));
        assert_eq!(config.cookies.get("logged-in-sig").map(|s| s.as_str()), Some("sig-value"));
        assert_eq!(config.general.screenname.as_deref(), Some("testuser"));
    }

    #[test]
    fn write_config_merges_with_existing() {
        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("ia.ini");

        // Create an existing config with custom settings
        let mut f = std::fs::File::create(&ini_path).unwrap();
        writeln!(f, "[general]").unwrap();
        writeln!(f, "host = custom.archive.org").unwrap();
        writeln!(f, "user_agent_suffix = MyApp/1.0").unwrap();
        writeln!(f, "[logging]").unwrap();
        writeln!(f, "level = debug").unwrap();
        drop(f);

        let auth = crate::auth::AuthConfig {
            s3_access: "merged-access".into(),
            s3_secret: "merged-secret".into(),
            logged_in_user: "user%40example.com".into(),
            logged_in_sig: "sig-value".into(),
            screenname: "testuser".into(),
            itemname: None,
        };

        IaConfig::write_config_file(&auth, &ini_path).unwrap();

        // Verify auth values were written
        let config = IaConfig::load_from_file(&ini_path).unwrap();
        assert_eq!(config.s3_access.as_deref(), Some("merged-access"));
        assert_eq!(config.s3_secret.as_deref(), Some("merged-secret"));

        // Verify existing settings were preserved
        assert_eq!(config.general.host, "custom.archive.org");
        assert_eq!(config.general.user_agent_suffix.as_deref(), Some("MyApp/1.0"));
        assert_eq!(config.logging.level.as_deref(), Some("debug"));
    }

    #[test]
    fn write_config_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("subdir").join("nested").join("ia.ini");

        let auth = crate::auth::AuthConfig {
            s3_access: "access".into(),
            s3_secret: "secret".into(),
            logged_in_user: "user".into(),
            logged_in_sig: "sig".into(),
            screenname: "test".into(),
            itemname: None,
        };

        let result = IaConfig::write_config_file(&auth, &ini_path);
        assert!(result.is_ok());
        assert!(ini_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn write_config_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let ini_path = dir.path().join("ia.ini");

        let auth = crate::auth::AuthConfig {
            s3_access: "access".into(),
            s3_secret: "secret".into(),
            logged_in_user: "user".into(),
            logged_in_sig: "sig".into(),
            screenname: "test".into(),
            itemname: None,
        };

        IaConfig::write_config_file(&auth, &ini_path).unwrap();

        let perms = std::fs::metadata(&ini_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    #[test]
    fn config_to_json_redacts_secrets() {
        let mut config = IaConfig::default();
        config.s3_access = Some("my-access-key".into());
        config.s3_secret = Some("my-secret-key".into());
        config.cookies.insert("logged-in-user".into(), "user%40example.com".into());
        config.cookies.insert("logged-in-sig".into(), "secret-sig".into());
        config.general.screenname = Some("testuser".into());

        let json = config.to_json(true);
        let obj = json.as_object().unwrap();

        // S3 keys should be redacted
        let s3 = obj["s3"].as_object().unwrap();
        assert_eq!(s3["access"], "REDACTED");
        assert_eq!(s3["secret"], "REDACTED");

        // Cookies should be redacted
        let cookies = obj["cookies"].as_object().unwrap();
        assert_eq!(cookies["logged-in-user"], "REDACTED");
        assert_eq!(cookies["logged-in-sig"], "REDACTED");

        // General should NOT be redacted
        let general = obj["general"].as_object().unwrap();
        assert_eq!(general["screenname"], "testuser");
    }

    #[test]
    fn config_to_json_no_redact() {
        let mut config = IaConfig::default();
        config.s3_access = Some("my-access-key".into());
        config.s3_secret = Some("my-secret-key".into());

        let json = config.to_json(false);
        let s3 = json["s3"].as_object().unwrap();
        assert_eq!(s3["access"], "my-access-key");
        assert_eq!(s3["secret"], "my-secret-key");
    }

}
