use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persistent GUI settings stored as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuiSettings {
    /// archive.org host (default: archive.org)
    #[serde(default = "default_host")]
    pub host: String,

    /// User-agent suffix appended to UA string
    #[serde(default)]
    pub user_agent_suffix: String,

    /// Allow insecure (HTTP) connections
    #[serde(default)]
    pub insecure: bool,

    /// Default download directory
    #[serde(default = "default_download_dir")]
    pub download_dir: String,

    /// Concurrent download jobs
    #[serde(default = "default_jobs")]
    pub jobs: u32,

    /// Enable job log
    #[serde(default = "default_true")]
    pub joblog_enabled: bool,
}

fn default_host() -> String {
    "archive.org".to_string()
}

fn default_download_dir() -> String {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .to_string_lossy()
        .to_string()
}

fn default_jobs() -> u32 {
    2
}

fn default_true() -> bool {
    true
}

impl Default for GuiSettings {
    fn default() -> Self {
        Self {
            host: default_host(),
            user_agent_suffix: String::new(),
            insecure: false,
            download_dir: default_download_dir(),
            jobs: default_jobs(),
            joblog_enabled: default_true(),
        }
    }
}

impl GuiSettings {
    /// Default settings file path.
    pub fn default_path() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ia")
            .join("gui-settings.json")
    }

    /// Load settings from file, or return defaults.
    pub fn load() -> Self {
        let path = Self::default_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save settings to file.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::default_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)?;
        std::fs::write(path, content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_settings() {
        let settings = GuiSettings::default();
        assert_eq!(settings.host, "archive.org");
        assert_eq!(settings.user_agent_suffix, "");
        assert!(!settings.insecure);
        assert_eq!(settings.jobs, 2);
        assert!(settings.joblog_enabled);
    }

    #[test]
    fn test_serialize_deserialize() {
        let settings = GuiSettings {
            host: "test.archive.org".to_string(),
            user_agent_suffix: "test-app".to_string(),
            insecure: true,
            download_dir: "/tmp/downloads".to_string(),
            jobs: 4,
            joblog_enabled: false,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let loaded: GuiSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.host, "test.archive.org");
        assert_eq!(loaded.user_agent_suffix, "test-app");
        assert!(loaded.insecure);
        assert_eq!(loaded.download_dir, "/tmp/downloads");
        assert_eq!(loaded.jobs, 4);
        assert!(!loaded.joblog_enabled);
    }

    #[test]
    fn test_load_missing_file() {
        // Should return defaults when file doesn't exist
        let settings = GuiSettings::default();
        assert_eq!(settings.host, "archive.org");
    }

    #[test]
    fn test_partial_json() {
        let json = r#"{"host": "custom.archive.org"}"#;
        let settings: GuiSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.host, "custom.archive.org");
        // Other fields should be defaults
        assert_eq!(settings.jobs, 2);
        assert!(settings.joblog_enabled);
    }
}
