# ia Rust Port — Implementation Plan

**Goal:** Build a concurrent, beautifully-designed Rust CLI for downloading from the Internet Archive, backed by a clean library crate.

**Architecture:** Cargo workspace with `ia-core` (library) and `ia-cli` (binary). `IaClient` wraps `reqwest::Client`; operation modules provide typed functions. See `docs/plans/2026-02-20-ia-rust-port-design.md`.

**Tech Stack:** clap 4, tokio 1, reqwest 0.12, serde 1, indicatif 0.17, tracing 0.1, thiserror 2, anyhow 1

**Safety:** Read-only operations only. No auth. No writes to archive.org. GitHub user `jjjake` only.

---

## Milestone 1: Core Infrastructure

### Task 1: Workspace Scaffold

Set up the Cargo workspace with both crates compiling and a passing "hello world" test.

**Files:**
- Create: `Cargo.toml` (workspace root)
- Create: `ia-core/Cargo.toml`
- Create: `ia-core/src/lib.rs`
- Create: `ia-cli/Cargo.toml`
- Create: `ia-cli/src/main.rs`

**Step 1: Create workspace root Cargo.toml**

```toml
[workspace]
members = ["ia-core", "ia-cli"]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
rust-version = "1.75"
license = "AGPL-3.0"
repository = "https://github.com/internetarchivecanada/ia"
```

**Step 2: Create ia-core/Cargo.toml**

```toml
[package]
name = "ia-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json", "stream", "rustls-tls"] }
reqwest-middleware = "0.4"
reqwest-retry = "0.7"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "fs", "io-util", "sync", "signal"] }
futures = "0.3"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
configparser = "3"
thiserror = "2"
tracing = "0.1"
backon = "1"
md-5 = "0.10"
globset = "0.4"
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
wiremock = "0.6"
tempfile = "3"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "test-util"] }
```

**Step 3: Create ia-core/src/lib.rs**

```rust
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_set() {
        assert_eq!(super::version(), "0.1.0");
    }
}
```

**Step 4: Create ia-cli/Cargo.toml**

```toml
[package]
name = "ia-cli"
version.workspace = true
edition.workspace = true
rust-version.workspace = true

[[bin]]
name = "ia"
path = "src/main.rs"

[dependencies]
ia-core = { path = "../ia-core" }
clap = { version = "4", features = ["derive", "env", "string"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
anyhow = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
indicatif = "0.17"
console = "0.15"
comfy-table = "7"

[dev-dependencies]
assert_cmd = "2"
predicates = "3"
```

**Step 5: Create ia-cli/src/main.rs**

```rust
fn main() {
    println!("ia {}", ia_core::version());
}
```

**Step 6: Verify it compiles and tests pass**

Run: `cargo test --workspace`
Expected: 1 test passes, both crates compile.

Run: `cargo run -p ia-cli`
Expected: Prints `ia 0.1.0`

**Step 7: Commit**

```bash
git add Cargo.toml ia-core/ ia-cli/
git commit -m "feat: scaffold cargo workspace with ia-core and ia-cli"
```

---

### Task 2: Error Types

**Files:**
- Create: `ia-core/src/error.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write tests for error types**

Add to `ia-core/src/error.rs`:

```rust
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum IaError {
    #[error("item not found: {0}")]
    NotFound(String),

    #[error("HTTP error {status}: {message}")]
    Http { status: u16, message: String },

    #[error("rate limited (retry after {retry_after}s)")]
    RateLimited { retry_after: u64 },

    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    #[error("disk full: {}", path.display())]
    DiskFull { path: PathBuf },

    #[error("no disk in pool has {needed} bytes free")]
    NoDiskSpace { needed: u64 },

    #[error("download resume failed for {file}: {reason}")]
    ResumeFailed { file: String, reason: String },

    #[error("config error: {0}")]
    Config(String),

    #[error(transparent)]
    Network(#[from] reqwest::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, IaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_found_displays_identifier() {
        let err = IaError::NotFound("nasa".to_string());
        assert_eq!(err.to_string(), "item not found: nasa");
    }

    #[test]
    fn http_error_displays_status_and_message() {
        let err = IaError::Http {
            status: 503,
            message: "Service Unavailable".to_string(),
        };
        assert_eq!(err.to_string(), "HTTP error 503: Service Unavailable");
    }

    #[test]
    fn checksum_mismatch_displays_details() {
        let err = IaError::ChecksumMismatch {
            file: "photo.jpg".to_string(),
            expected: "abc123".to_string(),
            actual: "def456".to_string(),
        };
        assert!(err.to_string().contains("photo.jpg"));
        assert!(err.to_string().contains("abc123"));
    }

    #[test]
    fn io_error_converts() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let ia_err: IaError = io_err.into();
        assert!(matches!(ia_err, IaError::Io(_)));
    }
}
```

**Step 2: Export from lib.rs**

```rust
pub mod error;

pub use error::{IaError, Result};
```

**Step 3: Run tests**

Run: `cargo test -p ia-core`
Expected: All error tests pass.

**Step 4: Commit**

```bash
git add ia-core/src/error.rs ia-core/src/lib.rs
git commit -m "feat: add error types with thiserror"
```

---

### Task 3: Configuration

INI config loading compatible with Python's `ia.ini` format.

**Files:**
- Create: `ia-core/src/config.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write the config module with tests**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{IaError, Result};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: None,
            file: None,
            log_to_stdout: false,
        }
    }
}

impl Default for IaConfig {
    fn default() -> Self {
        Self {
            s3_access: None,
            s3_secret: None,
            cookies: HashMap::new(),
            general: GeneralConfig::default(),
            logging: LoggingConfig::default(),
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

        let mut config = Self::default();

        // [s3] section
        config.s3_access = ini.get("s3", "access");
        config.s3_secret = ini.get("s3", "secret");

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
```

**Step 2: Export from lib.rs**

Add `pub mod config;` and `pub use config::IaConfig;`

**Step 3: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add ia-core/src/config.rs ia-core/src/lib.rs
git commit -m "feat: add INI config loading compatible with Python ia.ini"
```

---

### Task 4: User-Agent Construction

**Files:**
- Create: `ia-core/src/user_agent.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write user_agent module with tests**

```rust
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
```

**Step 2: Export from lib.rs**

Add `pub mod user_agent;`

**Step 3: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add ia-core/src/user_agent.rs ia-core/src/lib.rs
git commit -m "feat: add user-agent string construction matching Python format"
```

---

### Task 5: IaClient

The central HTTP client wrapping reqwest with IA-specific config, retry middleware, and URL construction.

**Files:**
- Create: `ia-core/src/client.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write the client module**

```rust
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};

use crate::config::IaConfig;
use crate::error::Result;
use crate::user_agent::build_user_agent;

/// The central HTTP client for interacting with the Internet Archive.
///
/// Wraps a `reqwest::Client` with IA-specific configuration:
/// - Connection pooling and keep-alive (unlike Python's Connection: close)
/// - Automatic retry with exponential backoff for 429/5xx
/// - Retry-After header respect
/// - Proper User-Agent identification
#[derive(Clone)]
pub struct IaClient {
    http: ClientWithMiddleware,
    config: IaConfig,
    user_agent: String,
}

impl IaClient {
    /// Create a new client with auto-discovered config.
    pub fn new() -> Result<Self> {
        let config = IaConfig::load()?;
        Self::from_config(config)
    }

    /// Create a new client with the provided config.
    pub fn from_config(config: IaConfig) -> Result<Self> {
        let user_agent = build_user_agent(&config);

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_str(&user_agent).unwrap());

        let raw_client = reqwest::Client::builder()
            .default_headers(headers)
            .pool_max_idle_per_host(10)
            .build()
            .map_err(|e| crate::error::IaError::Config(format!("failed to build HTTP client: {e}")))?;

        let retry_policy = ExponentialBackoff::builder()
            .retry_bounds(
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(60),
            )
            .build_with_max_retries(3);

        let http = ClientBuilder::new(raw_client)
            .with(RetryTransientMiddleware::new_with_policy(retry_policy))
            .build();

        Ok(Self {
            http,
            config,
            user_agent,
        })
    }

    /// The configured host (default: "archive.org").
    pub fn host(&self) -> &str {
        &self.config.general.host
    }

    /// The protocol ("https" or "http").
    pub fn protocol(&self) -> &str {
        self.config.protocol()
    }

    /// Build a full URL from a path on the main archive.org host.
    pub fn url(&self, path: &str) -> String {
        format!("{}://{}{}", self.protocol(), self.host(), path)
    }

    /// The User-Agent string being sent with requests.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// The underlying HTTP client (for operation modules).
    pub fn http(&self) -> &ClientWithMiddleware {
        &self.http
    }

    /// The current configuration.
    pub fn config(&self) -> &IaConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_from_default_config() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        assert_eq!(client.host(), "archive.org");
        assert_eq!(client.protocol(), "https");
        assert!(client.user_agent().starts_with("ia/"));
    }

    #[test]
    fn client_url_construction() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        assert_eq!(
            client.url("/metadata/nasa"),
            "https://archive.org/metadata/nasa"
        );
    }

    #[test]
    fn client_custom_host() {
        let mut config = IaConfig::default();
        config.general.host = "test.archive.org".to_string();
        let client = IaClient::from_config(config).unwrap();
        assert_eq!(client.host(), "test.archive.org");
        assert_eq!(
            client.url("/metadata/test"),
            "https://test.archive.org/metadata/test"
        );
    }

    #[test]
    fn client_insecure_mode() {
        let mut config = IaConfig::default();
        config.general.secure = false;
        let client = IaClient::from_config(config).unwrap();
        assert_eq!(client.protocol(), "http");
        assert!(client.url("/test").starts_with("http://"));
    }

    #[test]
    fn client_is_clone() {
        let client = IaClient::from_config(IaConfig::default()).unwrap();
        let _clone = client.clone(); // Should compile — reqwest::Client is Arc-based
    }
}
```

**Step 2: Export from lib.rs**

Add `pub mod client;` and `pub use client::IaClient;`

**Step 3: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add ia-core/src/client.rs ia-core/src/lib.rs
git commit -m "feat: add IaClient with reqwest, retry middleware, and connection pooling"
```

---

### Task 6: Core Types

Item and file metadata types that match the IA JSON API response format.

**Files:**
- Create: `ia-core/src/types.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write types with deserialization tests**

The IA `/metadata/{id}` endpoint returns JSON with metadata and files as nested objects. File metadata fields like `size` and `mtime` come as strings from the API.

```rust
use serde::Deserialize;
use std::collections::HashMap;

/// Full response from GET /metadata/{identifier}
#[derive(Debug, Clone, Deserialize)]
pub struct ItemMetadata {
    /// The item's metadata fields.
    #[serde(default)]
    pub metadata: MetadataFields,

    /// Files contained in the item.
    #[serde(default)]
    pub files: Vec<FileMetadata>,

    /// Server hosting the item (e.g., "ia802304.us.archive.org").
    pub server: Option<String>,

    /// Primary directory server.
    pub d1: Option<String>,

    /// Secondary directory server.
    pub d2: Option<String>,

    /// Directory path on server.
    pub dir: Option<String>,

    /// Number of files that have been cached.
    pub files_count: Option<u64>,

    /// Total size of the item in bytes.
    pub item_size: Option<u64>,

    /// Whether the item is "dark" (hidden).
    #[serde(default)]
    pub is_dark: bool,
}

/// Item-level metadata fields.
/// Uses a mix of typed common fields and a catch-all HashMap.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct MetadataFields {
    pub identifier: Option<String>,
    pub title: Option<StringOrVec>,
    pub description: Option<StringOrVec>,
    pub mediatype: Option<String>,
    pub collection: Option<StringOrVec>,
    pub creator: Option<StringOrVec>,
    pub date: Option<String>,
    pub subject: Option<StringOrVec>,
    pub language: Option<StringOrVec>,
    pub publicdate: Option<String>,
    pub addeddate: Option<String>,
    pub uploader: Option<String>,

    /// All other metadata fields not captured above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// IA metadata fields can be a single string or a vec of strings.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    Single(String),
    Multiple(Vec<String>),
}

impl StringOrVec {
    /// Get the first value (or only value).
    pub fn first(&self) -> &str {
        match self {
            StringOrVec::Single(s) => s,
            StringOrVec::Multiple(v) => v.first().map(|s| s.as_str()).unwrap_or(""),
        }
    }

    /// Get all values as a vec.
    pub fn to_vec(&self) -> Vec<&str> {
        match self {
            StringOrVec::Single(s) => vec![s.as_str()],
            StringOrVec::Multiple(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

/// Metadata for a single file within an item.
/// Note: IA returns size and mtime as strings, not numbers.
#[derive(Debug, Clone, Deserialize)]
pub struct FileMetadata {
    /// File name (relative path within the item).
    pub name: String,

    /// File source: "original", "derivative", or "metadata".
    pub source: Option<String>,

    /// File format (e.g., "MPEG4", "JPEG", "Text").
    pub format: Option<String>,

    /// MD5 checksum.
    pub md5: Option<String>,

    /// File size in bytes (IA returns as string).
    #[serde(default, deserialize_with = "deserialize_optional_string_u64")]
    pub size: Option<u64>,

    /// Modification time as Unix timestamp (IA returns as string).
    #[serde(default, deserialize_with = "deserialize_optional_string_u64")]
    pub mtime: Option<u64>,

    /// SHA1 checksum.
    pub sha1: Option<String>,

    /// CRC32 checksum.
    pub crc32: Option<String>,

    /// For derivatives: the original file this was derived from.
    pub original: Option<String>,

    /// Rotation (for images/video).
    pub rotation: Option<String>,

    /// All other file metadata fields.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Custom deserializer for fields that IA returns as strings but are actually u64.
fn deserialize_optional_string_u64<'de, D>(deserializer: D) -> std::result::Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNum {
        Str(String),
        Num(u64),
    }

    match Option::<StringOrNum>::deserialize(deserializer)? {
        Some(StringOrNum::Str(s)) => s
            .parse::<u64>()
            .map(Some)
            .map_err(de::Error::custom),
        Some(StringOrNum::Num(n)) => Ok(Some(n)),
        None => Ok(None),
    }
}

/// File source filter for downloads.
#[derive(Debug, Clone, PartialEq)]
pub enum FileSource {
    Original,
    Derivative,
    Metadata,
}

impl FileSource {
    pub fn as_str(&self) -> &str {
        match self {
            FileSource::Original => "original",
            FileSource::Derivative => "derivative",
            FileSource::Metadata => "metadata",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_item_metadata() {
        let json = r#"{
            "metadata": {
                "identifier": "nasa",
                "title": "NASA Images",
                "mediatype": "image",
                "collection": ["nasa_collection", "other_collection"],
                "subject": "space"
            },
            "files": [
                {
                    "name": "photo.jpg",
                    "source": "original",
                    "format": "JPEG",
                    "md5": "abc123",
                    "size": "4200000",
                    "mtime": "1700000000"
                },
                {
                    "name": "photo_thumb.jpg",
                    "source": "derivative",
                    "original": "photo.jpg",
                    "size": "50000"
                }
            ],
            "server": "ia802304.us.archive.org",
            "d1": "ia802304.us.archive.org",
            "d2": "ia902304.us.archive.org",
            "dir": "/7/items/nasa"
        }"#;

        let item: ItemMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(item.metadata.identifier.as_deref(), Some("nasa"));
        assert_eq!(item.metadata.title.as_ref().unwrap().first(), "NASA Images");
        assert_eq!(item.files.len(), 2);
        assert_eq!(item.files[0].name, "photo.jpg");
        assert_eq!(item.files[0].size, Some(4200000));
        assert_eq!(item.files[0].mtime, Some(1700000000));
        assert_eq!(item.files[1].source.as_deref(), Some("derivative"));
        assert_eq!(item.files[1].original.as_deref(), Some("photo.jpg"));
    }

    #[test]
    fn string_or_vec_single() {
        let json = r#""hello""#;
        let v: StringOrVec = serde_json::from_str(json).unwrap();
        assert_eq!(v.first(), "hello");
        assert_eq!(v.to_vec(), vec!["hello"]);
    }

    #[test]
    fn string_or_vec_multiple() {
        let json = r#"["a", "b", "c"]"#;
        let v: StringOrVec = serde_json::from_str(json).unwrap();
        assert_eq!(v.first(), "a");
        assert_eq!(v.to_vec(), vec!["a", "b", "c"]);
    }

    #[test]
    fn file_size_as_string() {
        let json = r#"{"name": "test.txt", "size": "12345"}"#;
        let f: FileMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(f.size, Some(12345));
    }

    #[test]
    fn file_size_as_number() {
        let json = r#"{"name": "test.txt", "size": 12345}"#;
        let f: FileMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(f.size, Some(12345));
    }

    #[test]
    fn file_size_missing() {
        let json = r#"{"name": "test.txt"}"#;
        let f: FileMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(f.size, None);
    }

    #[test]
    fn collection_as_single_string() {
        let json = r#"{"collection": "nasa"}"#;
        let m: MetadataFields = serde_json::from_str(json).unwrap();
        assert_eq!(m.collection.unwrap().first(), "nasa");
    }

    #[test]
    fn collection_as_array() {
        let json = r#"{"collection": ["nasa", "images"]}"#;
        let m: MetadataFields = serde_json::from_str(json).unwrap();
        assert_eq!(m.collection.unwrap().to_vec(), vec!["nasa", "images"]);
    }
}
```

**Step 2: Export from lib.rs**

Add `pub mod types;`

**Step 3: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add ia-core/src/types.rs ia-core/src/lib.rs
git commit -m "feat: add IA item and file metadata types with serde deserialization"
```

---

## Milestone 2: Download MVP

### Task 7: Metadata Read API

Fetch item metadata from `GET /metadata/{identifier}`.

**Files:**
- Create: `ia-core/src/metadata.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write the metadata module**

```rust
use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::types::ItemMetadata;

/// Fetch full metadata for an item.
pub async fn get(client: &IaClient, identifier: &str) -> Result<ItemMetadata> {
    let url = client.url(&format!("/metadata/{identifier}"));

    let response = client
        .http()
        .get(&url)
        .send()
        .await
        .map_err(|e| IaError::Network(e.into()))?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(IaError::NotFound(identifier.to_string()));
    }
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status: status.as_u16(),
            message: body,
        });
    }

    let item: ItemMetadata = response
        .json()
        .await
        .map_err(|e| IaError::Network(e.into()))?;

    Ok(item)
}

/// Check if an item exists.
pub async fn exists(client: &IaClient, identifier: &str) -> Result<bool> {
    match get(client, identifier).await {
        Ok(_) => Ok(true),
        Err(IaError::NotFound(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        // Strip protocol prefix for host
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    #[tokio::test]
    async fn get_item_metadata() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/test-item"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {
                    "identifier": "test-item",
                    "title": "Test Item",
                    "mediatype": "texts"
                },
                "files": [
                    {"name": "test.pdf", "size": "1000", "source": "original", "md5": "abc123"}
                ],
                "server": "ia000000.us.archive.org"
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let item = get(&client, "test-item").await.unwrap();

        assert_eq!(item.metadata.identifier.as_deref(), Some("test-item"));
        assert_eq!(item.files.len(), 1);
        assert_eq!(item.files[0].name, "test.pdf");
        assert_eq!(item.files[0].size, Some(1000));
    }

    #[tokio::test]
    async fn get_item_not_found() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nonexistent"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let result = get(&client, "nonexistent").await;

        assert!(matches!(result, Err(IaError::NotFound(_))));
    }

    #[tokio::test]
    async fn exists_returns_true_for_existing_item() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nasa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metadata": {"identifier": "nasa"},
                "files": []
            })))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        assert!(exists(&client, "nasa").await.unwrap());
    }

    #[tokio::test]
    async fn exists_returns_false_for_missing_item() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/metadata/nope"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        assert!(!exists(&client, "nope").await.unwrap());
    }
}
```

**Step 2: Add convenience methods to IaClient**

In `client.rs`, add:
```rust
impl IaClient {
    pub async fn get_item(&self, identifier: &str) -> Result<crate::types::ItemMetadata> {
        crate::metadata::get(self, identifier).await
    }

    pub async fn item_exists(&self, identifier: &str) -> Result<bool> {
        crate::metadata::exists(self, identifier).await
    }
}
```

**Step 3: Export from lib.rs**

Add `pub mod metadata;`

**Step 4: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass including wiremock integration tests.

**Step 5: Commit**

```bash
git add ia-core/src/metadata.rs ia-core/src/client.rs ia-core/src/lib.rs
git commit -m "feat: add metadata read API with GET /metadata/{id}"
```

---

### Task 8: File Listing & Filtering

Filter files from item metadata by glob, format, source, and name.

**Files:**
- Create: `ia-core/src/files.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write the files module with filtering logic**

```rust
use globset::{Glob, GlobMatcher};

use crate::types::{FileMetadata, FileSource, ItemMetadata};

/// Options for filtering files.
#[derive(Debug, Clone, Default)]
pub struct FileFilter {
    pub glob: Option<String>,
    pub exclude: Option<String>,
    pub formats: Vec<String>,
    pub source: Option<FileSource>,
    pub exclude_source: Option<FileSource>,
    pub names: Vec<String>,
}

/// List files from an item, applying filters.
pub fn list(item: &ItemMetadata, filter: &FileFilter) -> Vec<&FileMetadata> {
    let glob_matcher = filter.glob.as_ref().and_then(|g| {
        // Support pipe-separated globs like Python: "*.mp4|*.webm"
        // We match if ANY sub-glob matches
        let patterns: Vec<GlobMatcher> = g
            .split('|')
            .filter_map(|p| Glob::new(p.trim()).ok().map(|g| g.compile_matcher()))
            .collect();
        if patterns.is_empty() { None } else { Some(patterns) }
    });

    let exclude_matcher = filter.exclude.as_ref().and_then(|g| {
        let patterns: Vec<GlobMatcher> = g
            .split('|')
            .filter_map(|p| Glob::new(p.trim()).ok().map(|g| g.compile_matcher()))
            .collect();
        if patterns.is_empty() { None } else { Some(patterns) }
    });

    item.files
        .iter()
        .filter(|f| {
            // Filter by specific file names
            if !filter.names.is_empty() {
                return filter.names.iter().any(|n| n == &f.name);
            }

            // Filter by glob pattern
            if let Some(matchers) = &glob_matcher {
                if !matchers.iter().any(|m| m.is_match(&f.name)) {
                    return false;
                }
            }

            // Filter by exclude pattern
            if let Some(matchers) = &exclude_matcher {
                if matchers.iter().any(|m| m.is_match(&f.name)) {
                    return false;
                }
            }

            // Filter by format
            if !filter.formats.is_empty() {
                if let Some(fmt) = &f.format {
                    if !filter.formats.iter().any(|ff| ff.eq_ignore_ascii_case(fmt)) {
                        return false;
                    }
                } else {
                    return false;
                }
            }

            // Filter by source
            if let Some(src) = &filter.source {
                if f.source.as_deref() != Some(src.as_str()) {
                    return false;
                }
            }

            // Filter by excluded source
            if let Some(src) = &filter.exclude_source {
                if f.source.as_deref() == Some(src.as_str()) {
                    return false;
                }
            }

            true
        })
        .collect()
}

/// Calculate total size of a set of files.
pub fn total_size(files: &[&FileMetadata]) -> u64 {
    files.iter().filter_map(|f| f.size).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FileMetadata, ItemMetadata, MetadataFields};
    use std::collections::HashMap;

    fn make_item(files: Vec<FileMetadata>) -> ItemMetadata {
        ItemMetadata {
            metadata: MetadataFields::default(),
            files,
            server: None, d1: None, d2: None, dir: None,
            files_count: None, item_size: None, is_dark: false,
        }
    }

    fn make_file(name: &str, source: &str, format: &str, size: u64) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: Some(source.to_string()),
            format: Some(format.to_string()),
            size: Some(size),
            md5: None, mtime: None, sha1: None, crc32: None,
            original: None, rotation: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn list_all_files() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
        ]);
        let result = list(&item, &FileFilter::default());
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_by_glob() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
            make_file("c.jpg", "original", "JPEG", 150),
        ]);
        let result = list(&item, &FileFilter {
            glob: Some("*.jpg".to_string()),
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|f| f.name.ends_with(".jpg")));
    }

    #[test]
    fn filter_by_pipe_separated_glob() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
            make_file("c.png", "original", "PNG", 150),
        ]);
        let result = list(&item, &FileFilter {
            glob: Some("*.jpg|*.png".to_string()),
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_by_exclude() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("a_thumb.jpg", "derivative", "JPEG", 10),
            make_file("b.jpg", "original", "JPEG", 200),
        ]);
        let result = list(&item, &FileFilter {
            exclude: Some("*_thumb*".to_string()),
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_by_source() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("a_thumb.jpg", "derivative", "JPEG", 10),
        ]);
        let result = list(&item, &FileFilter {
            source: Some(FileSource::Original),
            ..Default::default()
        });
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "a.jpg");
    }

    #[test]
    fn filter_by_format() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
        ]);
        let result = list(&item, &FileFilter {
            formats: vec!["JPEG".to_string()],
            ..Default::default()
        });
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_by_specific_names() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
            make_file("c.jpg", "original", "JPEG", 150),
        ]);
        let result = list(&item, &FileFilter {
            names: vec!["a.jpg".to_string(), "c.jpg".to_string()],
            ..Default::default()
        });
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn total_size_calculation() {
        let item = make_item(vec![
            make_file("a.jpg", "original", "JPEG", 100),
            make_file("b.mp4", "original", "MPEG4", 200),
        ]);
        let files = list(&item, &FileFilter::default());
        assert_eq!(total_size(&files), 300);
    }
}
```

**Step 2: Export from lib.rs**

Add `pub mod files;`

**Step 3: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add ia-core/src/files.rs ia-core/src/lib.rs
git commit -m "feat: add file listing with glob, format, and source filtering"
```

---

### Task 9: Single File Download

Core download logic: streaming to .part file, resume with Range header, mtime setting, skip logic.

**Files:**
- Create: `ia-core/src/download.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Write the download module**

This is the largest single module. It handles:
- Skip detection (mtime + size match)
- Partial file resume (Range header)
- Streaming to .part file
- Checksum verification
- Finalizing (rename .part, set mtime)
- Progress callback for UI integration

```rust
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use crate::client::IaClient;
use crate::error::{IaError, Result};
use crate::files::FileFilter;
use crate::types::{FileMetadata, ItemMetadata};

/// Options for downloading files.
#[derive(Debug, Clone)]
pub struct DownloadOpts {
    /// Concurrent file downloads per item.
    pub jobs: usize,
    /// Destination directory.
    pub destdir: PathBuf,
    /// Whether to create item subdirectory.
    pub no_directories: bool,
    /// Verify checksums (opt-in).
    pub checksum: bool,
    /// Maximum retries per file.
    pub retries: usize,
    /// Don't set file mtime from server.
    pub no_timestamps: bool,
    /// Dry run (don't download, just report).
    pub dry_run: bool,
    /// File filter.
    pub filter: FileFilter,
}

impl Default for DownloadOpts {
    fn default() -> Self {
        Self {
            jobs: 4,
            destdir: PathBuf::from("."),
            no_directories: false,
            checksum: false,
            retries: 5,
            no_timestamps: false,
            dry_run: false,
            filter: FileFilter::default(),
        }
    }
}

/// Progress information for a single file download.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub file_name: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub status: DownloadStatus,
}

/// Status of a file download.
#[derive(Debug, Clone, PartialEq)]
pub enum DownloadStatus {
    Starting,
    Downloading,
    Verifying,
    Complete,
    Skipped(String),
    Failed(String),
}

/// Result of a single file download.
#[derive(Debug)]
pub struct FileDownloadResult {
    pub file_name: String,
    pub bytes: u64,
    pub status: DownloadStatus,
    pub elapsed: Duration,
}

/// Download a single file from an item.
pub async fn download_file(
    client: &IaClient,
    identifier: &str,
    file: &FileMetadata,
    dest_dir: &Path,
    opts: &DownloadOpts,
    progress: Option<&dyn Fn(DownloadProgress) + Send + Sync>,
) -> Result<FileDownloadResult> {
    let start = std::time::Instant::now();
    let file_path = dest_dir.join(&file.name);

    // Ensure parent directory exists
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).await?;
    }

    // Skip check: existing file with matching size + mtime
    if !opts.checksum {
        if let Some(skip_reason) = should_skip(&file_path, file).await {
            debug!(file = %file.name, reason = %skip_reason, "skipping file");
            if let Some(p) = progress {
                p(DownloadProgress {
                    file_name: file.name.clone(),
                    bytes_downloaded: 0,
                    total_bytes: file.size,
                    status: DownloadStatus::Skipped(skip_reason.clone()),
                });
            }
            return Ok(FileDownloadResult {
                file_name: file.name.clone(),
                bytes: 0,
                status: DownloadStatus::Skipped(skip_reason),
                elapsed: start.elapsed(),
            });
        }
    }

    // Checksum skip: compute local MD5 and compare
    if opts.checksum {
        if let Some(skip_reason) = should_skip_checksum(&file_path, file).await {
            debug!(file = %file.name, reason = %skip_reason, "skipping file (checksum match)");
            if let Some(p) = progress {
                p(DownloadProgress {
                    file_name: file.name.clone(),
                    bytes_downloaded: 0,
                    total_bytes: file.size,
                    status: DownloadStatus::Skipped(skip_reason.clone()),
                });
            }
            return Ok(FileDownloadResult {
                file_name: file.name.clone(),
                bytes: 0,
                status: DownloadStatus::Skipped(skip_reason),
                elapsed: start.elapsed(),
            });
        }
    }

    if opts.dry_run {
        return Ok(FileDownloadResult {
            file_name: file.name.clone(),
            bytes: file.size.unwrap_or(0),
            status: DownloadStatus::Skipped("dry run".to_string()),
            elapsed: start.elapsed(),
        });
    }

    // Download URL
    let encoded_name = urlencoding::encode(&file.name);
    let url = client.url(&format!("/download/{identifier}/{encoded_name}"));

    // Check for partial file (.part) for resume
    let part_path = PathBuf::from(format!("{}.part", file_path.display()));
    let resume_from = if part_path.exists() {
        let meta = fs::metadata(&part_path).await?;
        Some(meta.len())
    } else {
        None
    };

    // Build request
    let mut req = client.http().get(&url);
    if let Some(offset) = resume_from {
        debug!(file = %file.name, offset, "resuming download");
        req = req.header("Range", format!("bytes={offset}-"));
    }

    if let Some(p) = progress {
        p(DownloadProgress {
            file_name: file.name.clone(),
            bytes_downloaded: resume_from.unwrap_or(0),
            total_bytes: file.size,
            status: DownloadStatus::Starting,
        });
    }

    let response = req.send().await.map_err(|e| IaError::Network(e.into()))?;
    let status = response.status();

    if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
        let body = response.text().await.unwrap_or_default();
        return Err(IaError::Http {
            status: status.as_u16(),
            message: body,
        });
    }

    // Parse Last-Modified for mtime
    let last_modified = response
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok());

    // Stream to .part file
    let mut output = if resume_from.is_some() {
        fs::OpenOptions::new()
            .append(true)
            .open(&part_path)
            .await?
    } else {
        fs::File::create(&part_path).await?
    };

    let mut bytes_downloaded = resume_from.unwrap_or(0);
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| IaError::Network(e.into()))?;
        output.write_all(&chunk).await?;
        bytes_downloaded += chunk.len() as u64;

        if let Some(p) = progress {
            p(DownloadProgress {
                file_name: file.name.clone(),
                bytes_downloaded,
                total_bytes: file.size,
                status: DownloadStatus::Downloading,
            });
        }
    }

    output.flush().await?;
    drop(output);

    // Checksum verification if requested
    if opts.checksum {
        if let Some(expected_md5) = &file.md5 {
            if let Some(p) = progress {
                p(DownloadProgress {
                    file_name: file.name.clone(),
                    bytes_downloaded,
                    total_bytes: file.size,
                    status: DownloadStatus::Verifying,
                });
            }

            let actual_md5 = compute_md5(&part_path).await?;
            if &actual_md5 != expected_md5 {
                // Delete the bad file
                let _ = fs::remove_file(&part_path).await;
                return Err(IaError::ChecksumMismatch {
                    file: file.name.clone(),
                    expected: expected_md5.clone(),
                    actual: actual_md5,
                });
            }
        }
    }

    // Finalize: rename .part to final name
    fs::rename(&part_path, &file_path).await?;

    // Set mtime from Last-Modified header
    if !opts.no_timestamps {
        if let Some(mtime) = last_modified.or_else(|| {
            file.mtime.map(|t| UNIX_EPOCH + Duration::from_secs(t))
        }) {
            let _ = filetime::set_file_mtime(
                &file_path,
                filetime::FileTime::from_system_time(mtime),
            );
        }
    }

    info!(file = %file.name, bytes = bytes_downloaded, "download complete");

    if let Some(p) = progress {
        p(DownloadProgress {
            file_name: file.name.clone(),
            bytes_downloaded,
            total_bytes: file.size,
            status: DownloadStatus::Complete,
        });
    }

    Ok(FileDownloadResult {
        file_name: file.name.clone(),
        bytes: bytes_downloaded,
        status: DownloadStatus::Complete,
        elapsed: start.elapsed(),
    })
}

/// Check if a file should be skipped based on size + mtime.
async fn should_skip(path: &Path, file: &FileMetadata) -> Option<String> {
    let meta = fs::metadata(path).await.ok()?;
    let local_size = meta.len();

    // Size must match
    if let Some(remote_size) = file.size {
        if local_size != remote_size {
            return None;
        }
    } else {
        return None; // Can't compare without remote size
    }

    // mtime must match (if available)
    if let Some(remote_mtime) = file.mtime {
        if let Ok(local_mtime) = meta.modified() {
            let local_ts = local_mtime
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if local_ts == remote_mtime {
                return Some("size+mtime match".to_string());
            }
        }
    }

    // If we have matching size but no mtime to compare, skip based on size alone
    if file.mtime.is_none() {
        return Some("size match (no remote mtime)".to_string());
    }

    None
}

/// Check if a file should be skipped based on MD5 checksum.
async fn should_skip_checksum(path: &Path, file: &FileMetadata) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let expected_md5 = file.md5.as_ref()?;
    let actual_md5 = compute_md5(path).await.ok()?;
    if &actual_md5 == expected_md5 {
        Some("checksum match".to_string())
    } else {
        None
    }
}

/// Compute MD5 hash of a file.
async fn compute_md5(path: &Path) -> Result<String> {
    use md5::{Md5, Digest};
    use tokio::io::AsyncReadExt;

    let mut file = fs::File::open(path).await?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB buffer

    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_config(server_uri: &str) -> crate::config::IaConfig {
        let mut config = crate::config::IaConfig::default();
        let host = server_uri
            .strip_prefix("http://")
            .or_else(|| server_uri.strip_prefix("https://"))
            .unwrap_or(server_uri);
        config.general.host = host.to_string();
        config.general.secure = false;
        config
    }

    fn test_file_meta(name: &str, size: u64) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            source: Some("original".to_string()),
            format: None,
            md5: Some("d41d8cd98f00b204e9800998ecf8427e".to_string()),
            size: Some(size),
            mtime: Some(1700000000),
            sha1: None, crc32: None, original: None, rotation: None,
            extra: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn download_single_file() {
        let mock_server = MockServer::start().await;
        let body = b"hello world test content";

        Mock::given(method("GET"))
            .and(path("/download/test-item/test.txt"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(body.to_vec())
                    .insert_header("Last-Modified", "Thu, 01 Jan 2024 00:00:00 GMT"),
            )
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("test.txt", body.len() as u64);

        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert_eq!(result.bytes, body.len() as u64);

        let content = std::fs::read_to_string(dir.path().join("test.txt")).unwrap();
        assert_eq!(content, "hello world test content");
    }

    #[tokio::test]
    async fn skip_existing_file_with_matching_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("existing.txt");
        std::fs::write(&file_path, "content").unwrap();

        let file = FileMetadata {
            name: "existing.txt".to_string(),
            size: Some(7), // "content".len()
            mtime: None,   // No mtime means skip on size alone
            ..test_file_meta("existing.txt", 7)
        };

        // We don't even need a real server for skip
        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let result = download_file(
            &client,
            "test-item",
            &file,
            dir.path(),
            &DownloadOpts::default(),
            None,
        )
        .await
        .unwrap();

        assert!(matches!(result.status, DownloadStatus::Skipped(_)));
    }

    #[tokio::test]
    async fn dry_run_does_not_download() {
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("test.txt", 1000);

        let client = IaClient::from_config(crate::config::IaConfig::default()).unwrap();
        let opts = DownloadOpts {
            dry_run: true,
            ..Default::default()
        };

        let result = download_file(&client, "test-item", &file, dir.path(), &opts, None)
            .await
            .unwrap();

        assert!(matches!(result.status, DownloadStatus::Skipped(_)));
        assert!(!dir.path().join("test.txt").exists());
    }

    #[tokio::test]
    async fn creates_subdirectories_for_nested_files() {
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/download/test-item/subdir%2Ftest.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
            .mount(&mock_server)
            .await;

        let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = test_file_meta("subdir/test.txt", 4);

        let result = download_file(
            &client, "test-item", &file, dir.path(),
            &DownloadOpts::default(), None,
        )
        .await
        .unwrap();

        assert_eq!(result.status, DownloadStatus::Complete);
        assert!(dir.path().join("subdir/test.txt").exists());
    }
}
```

**Step 2: Add dependencies to ia-core/Cargo.toml**

Add:
```toml
urlencoding = "2"
httpdate = "1"
filetime = "0.2"
```

**Step 3: Export from lib.rs**

Add `pub mod download;`

**Step 4: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 5: Commit**

```bash
git add ia-core/src/download.rs ia-core/src/lib.rs ia-core/Cargo.toml
git commit -m "feat: add single file download with resume, skip, and mtime support"
```

---

### Task 10: Concurrent Download Engine

Add item-level download orchestration with parallel file downloads.

**Files:**
- Modify: `ia-core/src/download.rs` (add `download_item` function)

**Step 1: Add the concurrent download_item function**

Add to `download.rs`:

```rust
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Result of downloading an entire item.
#[derive(Debug)]
pub struct ItemDownloadResult {
    pub identifier: String,
    pub files_total: usize,
    pub files_downloaded: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub bytes_total: u64,
    pub elapsed: Duration,
    pub results: Vec<FileDownloadResult>,
}

/// Download all matching files from an item concurrently.
pub async fn download_item(
    client: &IaClient,
    identifier: &str,
    opts: &DownloadOpts,
    progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>>,
) -> Result<ItemDownloadResult> {
    let start = std::time::Instant::now();

    // Fetch item metadata
    let item = crate::metadata::get(client, identifier).await?;

    // Filter files
    let files = crate::files::list(&item, &opts.filter);
    let files_total = files.len();

    if files.is_empty() {
        return Ok(ItemDownloadResult {
            identifier: identifier.to_string(),
            files_total: 0,
            files_downloaded: 0,
            files_skipped: 0,
            files_failed: 0,
            bytes_total: 0,
            elapsed: start.elapsed(),
            results: vec![],
        });
    }

    // Determine destination directory
    let dest_dir = if opts.no_directories {
        opts.destdir.clone()
    } else {
        opts.destdir.join(identifier)
    };

    // Clone file metadata for owned access in tasks
    let files_owned: Vec<FileMetadata> = files.into_iter().cloned().collect();

    // Concurrent download with semaphore
    let semaphore = Arc::new(Semaphore::new(opts.jobs));
    let mut handles = Vec::new();

    for file in files_owned {
        let client = client.clone();
        let identifier = identifier.to_string();
        let dest_dir = dest_dir.clone();
        let opts = opts.clone();
        let sem = semaphore.clone();
        let progress = progress.clone();

        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let prog_ref = progress.as_deref();
            let mut last_err = None;

            for attempt in 0..=opts.retries {
                if attempt > 0 {
                    let delay = Duration::from_secs(2u64.pow(attempt as u32).min(60));
                    warn!(file = %file.name, attempt, "retrying after {:?}", delay);
                    tokio::time::sleep(delay).await;
                }

                match download_file(
                    &client,
                    &identifier,
                    &file,
                    &dest_dir,
                    &opts,
                    prog_ref,
                )
                .await
                {
                    Ok(result) => return result,
                    Err(e) => {
                        warn!(file = %file.name, attempt, error = %e, "download failed");
                        last_err = Some(e);
                    }
                }
            }

            FileDownloadResult {
                file_name: file.name.clone(),
                bytes: 0,
                status: DownloadStatus::Failed(
                    last_err.map(|e| e.to_string()).unwrap_or_else(|| "unknown error".to_string()),
                ),
                elapsed: start.elapsed(),
            }
        });

        handles.push(handle);
    }

    // Collect results
    let mut results = Vec::new();
    for handle in handles {
        match handle.await {
            Ok(result) => results.push(result),
            Err(e) => results.push(FileDownloadResult {
                file_name: "unknown".to_string(),
                bytes: 0,
                status: DownloadStatus::Failed(format!("task panic: {e}")),
                elapsed: start.elapsed(),
            }),
        }
    }

    let files_downloaded = results.iter().filter(|r| r.status == DownloadStatus::Complete).count();
    let files_skipped = results.iter().filter(|r| matches!(r.status, DownloadStatus::Skipped(_))).count();
    let files_failed = results.iter().filter(|r| matches!(r.status, DownloadStatus::Failed(_))).count();
    let bytes_total = results.iter().map(|r| r.bytes).sum();

    Ok(ItemDownloadResult {
        identifier: identifier.to_string(),
        files_total,
        files_downloaded,
        files_skipped,
        files_failed,
        bytes_total,
        elapsed: start.elapsed(),
        results,
    })
}
```

**Step 2: Add integration test**

```rust
#[tokio::test]
async fn download_item_concurrently() {
    let mock_server = MockServer::start().await;

    // Mock metadata endpoint
    Mock::given(method("GET"))
        .and(path("/metadata/test-item"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "metadata": {"identifier": "test-item"},
            "files": [
                {"name": "a.txt", "size": "5", "source": "original"},
                {"name": "b.txt", "size": "5", "source": "original"},
                {"name": "c.txt", "size": "5", "source": "original"}
            ]
        })))
        .mount(&mock_server)
        .await;

    // Mock file downloads
    for name in &["a.txt", "b.txt", "c.txt"] {
        Mock::given(method("GET"))
            .and(path(format!("/download/test-item/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello".to_vec()))
            .mount(&mock_server)
            .await;
    }

    let client = IaClient::from_config(mock_config(&mock_server.uri())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let opts = DownloadOpts {
        destdir: dir.path().to_path_buf(),
        jobs: 2,
        ..Default::default()
    };

    let result = download_item(&client, "test-item", &opts, None).await.unwrap();

    assert_eq!(result.files_total, 3);
    assert_eq!(result.files_downloaded, 3);
    assert_eq!(result.files_failed, 0);
    assert!(dir.path().join("test-item/a.txt").exists());
    assert!(dir.path().join("test-item/b.txt").exists());
    assert!(dir.path().join("test-item/c.txt").exists());
}
```

**Step 3: Run tests**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add ia-core/src/download.rs
git commit -m "feat: add concurrent item download with semaphore-based parallelism"
```

---

### Task 11: CLI Scaffold & Download Command

Wire up the CLI with clap, global options, and the download subcommand with beautiful console output.

**Files:**
- Rewrite: `ia-cli/src/main.rs`
- Create: `ia-cli/src/commands/mod.rs`
- Create: `ia-cli/src/commands/download.rs`
- Create: `ia-cli/src/output.rs`

**Step 1: Write main.rs with clap**

```rust
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

mod commands;
mod output;

#[derive(Parser)]
#[command(name = "ia", version, about = "Internet Archive command-line tool")]
struct Cli {
    /// Path to config file
    #[arg(short = 'c', long, global = true)]
    config: Option<PathBuf>,

    /// Enable logging
    #[arg(short = 'l', long, global = true)]
    log: bool,

    /// Enable debug output
    #[arg(short = 'd', long, global = true)]
    debug: bool,

    /// Path to job log file
    #[arg(long, global = true)]
    joblog: Option<PathBuf>,

    /// Retry failed operations from job log
    #[arg(long, global = true)]
    retry_failed: bool,

    /// Suppress output (repeat for more quiet: -q summary only, -qq silent)
    #[arg(short = 'q', long, global = true, action = clap::ArgAction::Count)]
    quiet: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Download files from an item
    Download(commands::download::DownloadArgs),
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    let log_level = if cli.debug {
        "debug"
    } else if cli.log {
        "info"
    } else {
        "warn"
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .with_writer(std::io::stderr)
        .init();

    // Load config
    let config = if let Some(path) = &cli.config {
        ia_core::IaConfig::load_from_file(path)?
    } else {
        ia_core::IaConfig::load()?
    };

    let client = ia_core::IaClient::from_config(config)?;

    match cli.command {
        Commands::Download(args) => commands::download::run(&client, args, cli.quiet).await?,
    }

    Ok(())
}
```

**Step 2: Write commands/mod.rs**

```rust
pub mod download;
```

**Step 3: Write commands/download.rs**

```rust
use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;

use ia_core::download::{DownloadOpts, DownloadProgress, DownloadStatus};
use ia_core::files::FileFilter;
use ia_core::types::FileSource;
use ia_core::IaClient;

use crate::output::DownloadDisplay;

#[derive(Args)]
pub struct DownloadArgs {
    /// Item identifier to download
    identifier: String,

    /// Specific files to download (optional)
    files: Vec<String>,

    /// Filter files by glob pattern (pipe-separated: "*.mp4|*.webm")
    #[arg(short = 'g', long)]
    glob: Option<String>,

    /// Exclude files matching pattern
    #[arg(short = 'e', long)]
    exclude: Option<String>,

    /// Filter by file format
    #[arg(short = 'f', long)]
    format: Vec<String>,

    /// Filter by source type
    #[arg(long, value_parser = parse_source)]
    source: Option<FileSource>,

    /// Exclude by source type
    #[arg(long, value_parser = parse_source)]
    exclude_source: Option<FileSource>,

    /// Concurrent downloads per item
    #[arg(short = 'j', long, default_value = "4")]
    jobs: usize,

    /// Destination directory (repeatable for disk pool)
    #[arg(long, default_value = ".")]
    destdir: Vec<PathBuf>,

    /// Don't create item subdirectory
    #[arg(long)]
    no_directories: bool,

    /// Verify checksums (slower, reads every local file)
    #[arg(short = 'C', long)]
    checksum: bool,

    /// Max retries per file
    #[arg(short = 'R', long, default_value = "5")]
    retries: usize,

    /// Don't set file modification times
    #[arg(long)]
    no_timestamps: bool,

    /// Show what would be downloaded without downloading
    #[arg(long)]
    dry_run: bool,
}

fn parse_source(s: &str) -> std::result::Result<FileSource, String> {
    match s.to_lowercase().as_str() {
        "original" => Ok(FileSource::Original),
        "derivative" => Ok(FileSource::Derivative),
        "metadata" => Ok(FileSource::Metadata),
        _ => Err(format!("unknown source: {s} (expected: original, derivative, metadata)")),
    }
}

pub async fn run(client: &IaClient, args: DownloadArgs, quiet: u8) -> Result<()> {
    let opts = DownloadOpts {
        jobs: args.jobs,
        destdir: args.destdir.first().cloned().unwrap_or_else(|| PathBuf::from(".")),
        no_directories: args.no_directories,
        checksum: args.checksum,
        retries: args.retries,
        no_timestamps: args.no_timestamps,
        dry_run: args.dry_run,
        filter: FileFilter {
            glob: args.glob,
            exclude: args.exclude,
            formats: args.format,
            source: args.source,
            exclude_source: args.exclude_source,
            names: args.files,
        },
    };

    let display = if quiet == 0 {
        Some(Arc::new(DownloadDisplay::new(&args.identifier)))
    } else {
        None
    };

    let progress: Option<Arc<dyn Fn(DownloadProgress) + Send + Sync>> =
        display.clone().map(|d| -> Arc<dyn Fn(DownloadProgress) + Send + Sync> {
            Arc::new(move |p| d.update(p))
        });

    let result = ia_core::download::download_item(
        client,
        &args.identifier,
        &opts,
        progress,
    )
    .await
    .context(format!("failed to download {}", args.identifier))?;

    if let Some(d) = display {
        d.finish(&result);
    }

    // Summary for -q mode
    if quiet == 1 {
        eprintln!(
            "{}  {} files ({}) in {:.1}s",
            args.identifier,
            result.files_downloaded,
            format_bytes(result.bytes_total),
            result.elapsed.as_secs_f64(),
        );
    }

    if result.files_failed > 0 {
        std::process::exit(1);
    }

    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
```

**Step 4: Write output.rs (progress bars)**

```rust
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::Mutex;

use ia_core::download::{DownloadProgress, DownloadStatus, ItemDownloadResult};

pub struct DownloadDisplay {
    identifier: String,
    multi: MultiProgress,
    bars: Mutex<HashMap<String, ProgressBar>>,
    header: ProgressBar,
}

impl DownloadDisplay {
    pub fn new(identifier: &str) -> Self {
        let multi = MultiProgress::new();
        let header = multi.add(ProgressBar::new_spinner());
        header.set_message(format!(
            "{}  Resolving...",
            style(identifier).bold()
        ));
        header.enable_steady_tick(std::time::Duration::from_millis(100));

        Self {
            identifier: identifier.to_string(),
            multi,
            bars: Mutex::new(HashMap::new()),
            header,
        }
    }

    pub fn update(&self, progress: DownloadProgress) {
        let mut bars = self.bars.lock().unwrap();

        match &progress.status {
            DownloadStatus::Starting | DownloadStatus::Downloading => {
                let bar = bars.entry(progress.file_name.clone()).or_insert_with(|| {
                    let pb = self.multi.add(ProgressBar::new(
                        progress.total_bytes.unwrap_or(0),
                    ));
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("  {prefix:.dim} {bar:20.cyan/dim} {bytes}/{total_bytes} {bytes_per_sec:.dim}")
                            .unwrap()
                            .progress_chars("━╸─"),
                    );
                    pb.set_prefix(progress.file_name.clone());
                    pb
                });
                bar.set_position(progress.bytes_downloaded);
            }
            DownloadStatus::Complete => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style("done").green()
                    ));
                }
            }
            DownloadStatus::Skipped(reason) => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style(format!("skipped ({reason})")).yellow()
                    ));
                }
            }
            DownloadStatus::Failed(err) => {
                if let Some(bar) = bars.remove(&progress.file_name) {
                    bar.finish_with_message(format!(
                        "  {} {}",
                        style(&progress.file_name).dim(),
                        style(format!("FAILED: {err}")).red()
                    ));
                }
            }
            DownloadStatus::Verifying => {}
        }
    }

    pub fn finish(&self, result: &ItemDownloadResult) {
        self.header.finish_and_clear();

        let summary = format!(
            "\n{}  Downloaded {} files ({}) in {:.1}s",
            style(&self.identifier).bold(),
            result.files_downloaded,
            format_bytes(result.bytes_total),
            result.elapsed.as_secs_f64(),
        );

        let stats = format!(
            "  {} {} succeeded, {} failed, {} skipped",
            style("✓").green(),
            result.files_downloaded,
            if result.files_failed > 0 {
                style(result.files_failed.to_string()).red().to_string()
            } else {
                "0".to_string()
            },
            result.files_skipped,
        );

        eprintln!("{summary}");
        eprintln!("{stats}");
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}
```

**Step 5: Verify it compiles and runs**

Run: `cargo build --workspace`
Expected: Compiles without errors.

Run: `cargo run -p ia-cli -- --help`
Expected: Shows help with download subcommand.

Run: `cargo run -p ia-cli -- download --help`
Expected: Shows download help with all flags.

**Step 6: Run all tests**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 7: Commit**

```bash
git add ia-cli/src/
git commit -m "feat: add CLI with download command, progress bars, and colored output"
```

---

## Milestone 3: Full Read-Only CLI

### Task 12: Joblog

**Files:**
- Create: `ia-core/src/joblog.rs`
- Modify: `ia-core/src/lib.rs`
- Modify: `ia-cli/src/commands/download.rs` (wire up joblog)

Implement JSONL append-only joblog: write entries after each file operation, read entries for `--retry-failed`. Each entry is a JSON object with `ts`, `op`, `item`, `file`, `status`, `bytes`, `elapsed_ms`, and optional `error`/`reason` fields.

**Commit:** `feat: add JSONL joblog with --retry-failed support`

---

### Task 13: Search Module

**Files:**
- Create: `ia-core/src/search.rs`
- Modify: `ia-core/src/lib.rs`
- Modify: `ia-core/src/client.rs` (add convenience methods)

Implement all three search backends:
1. **Scrape API** (default): POST to `/services/search/v1/scrape` with cursor pagination. Returns async iterator/stream of results.
2. **Advanced search**: GET `/advancedsearch.php` with page-based pagination.
3. **Full-text search**: POST to `be-api.us.archive.org/ia-pub-fts-api` with scroll_id pagination.

Each returns a `Stream<Item = Result<SearchResult>>` for lazy iteration.

**Commit:** `feat: add search module with scrape, advanced, and FTS backends`

---

### Task 14: Search CLI Command

**Files:**
- Create: `ia-cli/src/commands/search.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

Flags: `query`, `--itemlist` (output identifiers only), `--num-found`, `--sort`, `--field` (repeatable), `--fts`, `--parameters`, `--timeout`.

**Commit:** `feat: add search CLI command`

---

### Task 15: List CLI Command

**Files:**
- Create: `ia-cli/src/commands/list.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

Fetch item metadata and display file listing. Flags: `--columns`, `--glob`, `--location` (full URLs), `--all`, `--verbose`.

**Commit:** `feat: add list CLI command`

---

### Task 16: Metadata CLI Command

**Files:**
- Create: `ia-cli/src/commands/metadata.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

Display item metadata as JSON. Flags: `--exists` (check existence), `--formats` (list file formats). Pipe-friendly JSON output.

**Commit:** `feat: add metadata CLI command`

---

### Task 17: Batch Downloads (--itemlist, --search)

**Files:**
- Modify: `ia-core/src/download.rs` (add `download_batch` function)
- Modify: `ia-cli/src/commands/download.rs` (add --itemlist and --search flags)

Add `download_batch` that takes a list of identifiers or a search query, downloads items with item-level concurrency (controlled by `--items` flag), and maintains item affinity.

**Commit:** `feat: add batch downloads with --itemlist and --search`

---

### Task 18: Status Command

**Files:**
- Create: `ia-cli/src/commands/status.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

Read a joblog file and display summary: total ops, success/fail/skip counts, list of failures with error messages. Show the retry command.

**Commit:** `feat: add status command for joblog inspection`

---

## Milestone 4: Advanced Features

### Task 19: Disk Pool

**Files:**
- Create: `ia-core/src/disk_pool.rs`
- Modify: `ia-core/src/download.rs`

Implement `DiskPool` struct: assign items to disks (most free space), handle disk-full by cleaning up partial items and reassigning to next disk. Wire into download pipeline.

**Commit:** `feat: add multi-disk pool support for downloads`

---

### Task 20: TUI Mode

**Files:**
- Create: `ia-cli/src/tui/mod.rs`
- Create: `ia-cli/src/tui/app.rs`
- Create: `ia-cli/src/tui/ui.rs`
- Modify: `ia-cli/Cargo.toml` (add ratatui dependency)

Interactive terminal UI with ratatui: overall progress bar, per-worker file progress, queue status, keyboard controls (pause, retry, adjust concurrency, quit).

Feature-gated behind `tui` cargo feature.

**Commit:** `feat: add interactive TUI mode for download management`

---

## Implementation Notes

### Testing Strategy

- **Unit tests**: In each module file. Test types, filtering, config parsing, URL construction.
- **Integration tests**: Use `wiremock` to mock IA API responses. Test full download flow, search pagination, metadata fetching.
- **CLI tests**: Use `assert_cmd` to test the binary end-to-end with mock servers.
- **No live API tests**: Never hit real archive.org in automated tests. All tests use mocks.

### Commit Strategy

- One commit per task step (or logical group of steps).
- Run `cargo test --workspace && cargo clippy --workspace` before every commit.
- Prefix: `feat:` for new features, `fix:` for bugs, `test:` for test additions, `refactor:` for restructuring.

### Safety Reminders

- **NEVER** implement write operations (upload, delete, metadata modify)
- **NEVER** read or use IA credentials
- **ALWAYS** use `gh auth switch --user jjjake` before GitHub operations
- **ALWAYS** include User-Agent in HTTP requests
