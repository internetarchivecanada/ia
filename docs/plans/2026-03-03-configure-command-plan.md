# `ia config` Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `ia config` command with subcommands for authentication setup, config viewing, and credential utilities.

**Architecture:** New `auth.rs` module in ia-core handles IA auth API calls. Existing `config.rs` gains write support. New `commands/config.rs` in ia-cli provides the CLI layer with six subcommands (`login`, `show`, `check`, `whoami`, `print-cookies`, `print-auth`). This is the first command to use sub-subcommands.

**Tech Stack:** `configparser` 3 (INI), `rpassword` (hidden input), `reqwest` (HTTP), `wiremock` (test mocks), `serde_json` (JSON output)

**Design Doc:** `docs/plans/2026-03-03-configure-command-design.md`

---

## Task 1: Add `rpassword` dependency and `auth` module skeleton

**Files:**
- Modify: `ia-core/Cargo.toml`
- Create: `ia-core/src/auth.rs`
- Modify: `ia-core/src/lib.rs`

**Step 1: Add `rpassword` to ia-core dependencies**

In `ia-core/Cargo.toml`, add under `[dependencies]`:

```toml
rpassword = "5"
```

**Step 2: Create the auth module skeleton**

Create `ia-core/src/auth.rs`:

```rust
//! Authentication with the Internet Archive.
//!
//! Handles login via the xauthn API, credential validation, and account info retrieval.

use serde::{Deserialize, Serialize};

use crate::client::IaClient;
use crate::error::{IaError, Result};

/// Credentials and account info returned by a successful login.
#[derive(Debug, Clone, Serialize)]
pub struct AuthConfig {
    pub s3_access: String,
    pub s3_secret: String,
    pub logged_in_user: String,
    pub logged_in_sig: String,
    pub screenname: String,
    pub itemname: Option<String>,
}

/// Account info returned by whoami/check operations.
#[derive(Debug, Clone, Serialize)]
pub struct AccountInfo {
    pub screenname: String,
    pub email: String,
    pub itemname: Option<String>,
}
```

**Step 3: Expose the auth module**

In `ia-core/src/lib.rs`, add `pub mod auth;` in the module list (alphabetical order, before `pub mod client;`).

**Step 4: Verify it compiles**

Run: `cargo check -p ia-core`
Expected: success

**Step 5: Commit**

```
feat(ia-core): add auth module skeleton with AuthConfig and AccountInfo types

New module for Internet Archive authentication. Adds rpassword dependency
for hidden password input during interactive login.
```

---

## Task 2: Implement `auth::login()` — POST to xauthn API

**Files:**
- Modify: `ia-core/src/auth.rs`
- Test: `ia-core/src/auth.rs` (inline tests)

**Step 1: Write the failing test**

Add at the bottom of `ia-core/src/auth.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper: build an IaClient pointing at a mock server.
    async fn test_client(host: &str) -> IaClient {
        let mut config = crate::config::IaConfig::default();
        config.general.host = host.to_string();
        config.general.secure = false; // mock server is HTTP
        IaClient::from_config(config).unwrap()
    }

    #[tokio::test]
    async fn login_success() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": true,
            "values": {
                "s3": {"access": "test-access", "secret": "test-secret"},
                "cookies": {
                    "logged-in-user": "user%40example.com",
                    "logged-in-sig": "test-sig"
                },
                "screenname": "testuser",
                "itemname": "@testuser"
            }
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        // server.uri() returns "http://127.0.0.1:{port}"
        let host = server.uri().replace("http://", "");
        let client = test_client(&host).await;
        let result = login(&client, "user@example.com", "password123").await;

        let auth = result.unwrap();
        assert_eq!(auth.s3_access, "test-access");
        assert_eq!(auth.s3_secret, "test-secret");
        assert_eq!(auth.logged_in_user, "user%40example.com");
        assert_eq!(auth.logged_in_sig, "test-sig");
        assert_eq!(auth.screenname, "testuser");
        assert_eq!(auth.itemname.as_deref(), Some("@testuser"));
    }

    #[tokio::test]
    async fn login_account_not_found() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": false,
            "values": {"reason": "account_not_found"}
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let client = test_client(&host).await;
        let result = login(&client, "bad@example.com", "pass").await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Account not found"), "got: {msg}");
    }

    #[tokio::test]
    async fn login_bad_password() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": false,
            "values": {"reason": "account_bad_password"}
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "login"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let client = test_client(&host).await;
        let result = login(&client, "user@example.com", "wrong").await;

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Incorrect password"), "got: {msg}");
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core auth::tests --no-run 2>&1; cargo test -p ia-core auth::tests 2>&1 | head -30`
Expected: compilation error — `login` function not defined

**Step 3: Implement `login()`**

Add to `ia-core/src/auth.rs`, after the struct definitions:

```rust
/// Response from the IA xauthn login API.
#[derive(Debug, Deserialize)]
struct XauthnResponse {
    success: bool,
    values: Option<XauthnValues>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XauthnValues {
    s3: Option<XauthnS3>,
    cookies: Option<XauthnCookies>,
    screenname: Option<String>,
    itemname: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XauthnS3 {
    access: String,
    secret: String,
}

#[derive(Debug, Deserialize)]
struct XauthnCookies {
    #[serde(rename = "logged-in-user")]
    logged_in_user: String,
    #[serde(rename = "logged-in-sig")]
    logged_in_sig: String,
}

/// Authenticate with archive.org and return credentials.
///
/// POSTs to `/services/xauthn/?op=login` with email and password.
/// On success, returns S3 keys, cookies, and account info.
pub async fn login(client: &IaClient, email: &str, password: &str) -> Result<AuthConfig> {
    let url = format!("{}://{}/services/xauthn/?op=login", client.protocol(), client.host());
    let resp = client
        .http()
        .post(&url)
        .header("user-agent", client.user_agent())
        .form(&[("email", email), ("password", password)])
        .send()
        .await
        .map_err(|e| IaError::Auth(format!("login request failed: {e}")))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| IaError::Auth(format!("failed to read login response: {e}")))?;

    if !status.is_success() {
        return Err(IaError::Auth(format!("login failed with HTTP {status}: {body}")));
    }

    let parsed: XauthnResponse = serde_json::from_str(&body)
        .map_err(|e| IaError::Auth(format!("failed to parse login response: {e}")))?;

    if !parsed.success {
        let message = match parsed.values.as_ref().and_then(|v| v.reason.as_deref()) {
            Some("account_not_found") => "Account not found, check your email".to_string(),
            Some("account_bad_password") => "Incorrect password".to_string(),
            Some(reason) => format!("Login failed: {reason}"),
            None => parsed
                .error
                .unwrap_or_else(|| "Login failed (unknown error)".to_string()),
        };
        return Err(IaError::Auth(message));
    }

    let values = parsed.values.ok_or_else(|| IaError::Auth("login response missing values".into()))?;
    let s3 = values.s3.ok_or_else(|| IaError::Auth("login response missing S3 keys".into()))?;
    let cookies = values.cookies.ok_or_else(|| IaError::Auth("login response missing cookies".into()))?;

    Ok(AuthConfig {
        s3_access: s3.access,
        s3_secret: s3.secret,
        logged_in_user: cookies.logged_in_user,
        logged_in_sig: cookies.logged_in_sig,
        screenname: values.screenname.unwrap_or_default(),
        itemname: values.itemname,
    })
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core auth::tests -- --nocapture`
Expected: all 3 tests pass

**Step 5: Commit**

```
feat(ia-core): implement auth::login() with xauthn API

Authenticates with archive.org via POST /services/xauthn/?op=login.
Handles success, account_not_found, and account_bad_password responses.
All tests use wiremock mocks — no live requests.
```

---

## Task 3: Implement config file writing (`write_config_file`)

**Files:**
- Modify: `ia-core/src/config.rs`

**Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` block in `config.rs`:

```rust
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
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core config::tests --no-run 2>&1 | head -10`
Expected: compilation error — `write_config_file` not defined

**Step 3: Implement `write_config_file` and `find_or_default_config_path`**

Add to the `impl IaConfig` block in `config.rs`:

```rust
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
    /// Priority: existing file (same search as `find_config_file`) → XDG default path.
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
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core config::tests -- --nocapture`
Expected: all tests pass (old + new)

**Step 5: Commit**

```
feat(ia-core): add config file writing with merge and permissions

write_config_file() merges auth credentials into an existing INI config
file, preserving custom sections (logging, AI, general settings).
Creates parent dirs (0o700), sets file permissions to 0o600.
find_or_default_config_path() resolves the write target, falling back
to the XDG default (~/.config/internetarchive/ia.ini).
```

---

## Task 4: Add `config_to_json()` for the `show` subcommand

**Files:**
- Modify: `ia-core/src/config.rs`

**Step 1: Write the failing test**

Add to `config::tests`:

```rust
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
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core config::tests::config_to_json --no-run 2>&1 | head -10`
Expected: compilation error — `to_json` method not defined

**Step 3: Implement `to_json()`**

Add to the `impl IaConfig` block in `config.rs`:

```rust
    /// Serialize config to JSON, optionally redacting secrets.
    pub fn to_json(&self, redact: bool) -> serde_json::Value {
        let redacted = serde_json::json!("REDACTED");

        let s3 = serde_json::json!({
            "access": if redact { redacted.clone() } else { self.s3_access.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null) },
            "secret": if redact { redacted.clone() } else { self.s3_secret.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null) },
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
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core config::tests::config_to_json -- --nocapture`
Expected: both tests pass

**Step 5: Commit**

```
feat(ia-core): add config JSON serialization with secret redaction

IaConfig::to_json(redact) serializes the config to serde_json::Value.
When redact=true, S3 keys, cookies, and AI API key show as "REDACTED".
Used by the `ia config show` command.
```

---

## Task 5: Implement `auth::check_keys()` and `auth::whoami()`

**Files:**
- Modify: `ia-core/src/auth.rs`

**Step 1: Write the failing tests**

Add to `auth::tests`:

```rust
    #[tokio::test]
    async fn check_keys_valid() {
        let server = MockServer::start().await;
        // The Python lib uses the xauthn endpoint with op=authenticate for checking
        let body = serde_json::json!({
            "success": true,
            "values": {
                "screenname": "testuser",
                "itemname": "@testuser"
            }
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let mut config = crate::config::IaConfig::default();
        config.general.host = host;
        config.general.secure = false;
        config.s3_access = Some("test-access".into());
        config.s3_secret = Some("test-secret".into());
        let client = IaClient::from_config(config).unwrap();

        let info = check_keys(&client).await.unwrap();
        assert_eq!(info.screenname, "testuser");
    }

    #[tokio::test]
    async fn check_keys_invalid() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "success": false,
            "error": "invalid credentials"
        });
        Mock::given(method("POST"))
            .and(path("/services/xauthn/"))
            .and(query_param("op", "info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let host = server.uri().replace("http://", "");
        let mut config = crate::config::IaConfig::default();
        config.general.host = host;
        config.general.secure = false;
        config.s3_access = Some("bad-access".into());
        config.s3_secret = Some("bad-secret".into());
        let client = IaClient::from_config(config).unwrap();

        let result = check_keys(&client).await;
        assert!(result.is_err());
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core auth::tests::check_keys --no-run 2>&1 | head -10`
Expected: compilation error — `check_keys` not defined

**Step 3: Implement `check_keys()` and `whoami()`**

Add to `ia-core/src/auth.rs`:

```rust
/// Response from the xauthn info endpoint.
#[derive(Debug, Deserialize)]
struct XauthnInfoResponse {
    success: bool,
    values: Option<XauthnInfoValues>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XauthnInfoValues {
    screenname: Option<String>,
    itemname: Option<String>,
}

/// Validate S3 keys by calling the IA API.
///
/// Uses the xauthn info endpoint to verify credentials are valid.
pub async fn check_keys(client: &IaClient) -> Result<AccountInfo> {
    let (access, secret) = client.require_auth()?;
    let url = format!("{}://{}/services/xauthn/?op=info", client.protocol(), client.host());
    let resp = client
        .http()
        .post(&url)
        .header("user-agent", client.user_agent())
        .header("authorization", format!("LOW {access}:{secret}"))
        .send()
        .await
        .map_err(|e| IaError::Auth(format!("key check request failed: {e}")))?;

    let body = resp
        .text()
        .await
        .map_err(|e| IaError::Auth(format!("failed to read response: {e}")))?;

    let parsed: XauthnInfoResponse = serde_json::from_str(&body)
        .map_err(|e| IaError::Auth(format!("failed to parse response: {e}")))?;

    if !parsed.success {
        let msg = parsed.error.unwrap_or_else(|| "invalid credentials".into());
        return Err(IaError::Auth(msg));
    }

    let values = parsed.values.unwrap_or(XauthnInfoValues {
        screenname: None,
        itemname: None,
    });

    // Extract email from cookies in client config
    let email = client
        .config()
        .cookies
        .get("logged-in-user")
        .cloned()
        .unwrap_or_default();

    Ok(AccountInfo {
        screenname: values.screenname.unwrap_or_default(),
        email,
        itemname: values.itemname,
    })
}

/// Retrieve account info from archive.org (alias for check_keys).
///
/// Uses the same endpoint as `check_keys` but is semantically "who am I?"
pub async fn whoami(client: &IaClient) -> Result<AccountInfo> {
    check_keys(client).await
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core auth::tests -- --nocapture`
Expected: all 5 tests pass

**Step 5: Commit**

```
feat(ia-core): implement auth::check_keys() and whoami()

Validates S3 credentials and retrieves account info via the xauthn
info endpoint. Both functions use require_auth() to ensure credentials
are present before making the API call.
```

---

## Task 6: Add netrc parsing support

**Files:**
- Modify: `ia-core/src/auth.rs`

**Step 1: Write the failing tests**

Add to `auth::tests`:

```rust
    #[test]
    fn parse_netrc_success() {
        let dir = tempfile::tempdir().unwrap();
        let netrc_path = dir.path().join(".netrc");
        std::fs::write(
            &netrc_path,
            "machine archive.org\n  login user@example.com\n  password secret123\n",
        )
        .unwrap();

        let (email, password) = parse_netrc(&netrc_path).unwrap();
        assert_eq!(email, "user@example.com");
        assert_eq!(password, "secret123");
    }

    #[test]
    fn parse_netrc_missing_host() {
        let dir = tempfile::tempdir().unwrap();
        let netrc_path = dir.path().join(".netrc");
        std::fs::write(
            &netrc_path,
            "machine github.com\n  login user\n  password pass\n",
        )
        .unwrap();

        let result = parse_netrc(&netrc_path);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("archive.org"), "got: {msg}");
    }

    #[test]
    fn parse_netrc_file_not_found() {
        let result = parse_netrc(Path::new("/nonexistent/.netrc"));
        assert!(result.is_err());
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core auth::tests::parse_netrc --no-run 2>&1 | head -10`
Expected: compilation error — `parse_netrc` not defined

**Step 3: Implement `parse_netrc()`**

Add to `ia-core/src/auth.rs`:

```rust
use std::path::Path;

/// Parse a netrc file and extract archive.org credentials.
///
/// Looks for a `machine archive.org` entry with `login` and `password` fields.
pub fn parse_netrc(path: &Path) -> Result<(String, String)> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| IaError::Auth(format!("failed to read netrc file: {e}")))?;

    let mut machine_match = false;
    let mut login = None;
    let mut password = None;

    for line in content.lines() {
        let trimmed = line.trim();
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();

        let mut i = 0;
        while i < tokens.len() {
            match tokens[i] {
                "machine" if i + 1 < tokens.len() => {
                    if machine_match && login.is_some() && password.is_some() {
                        // We already found our entry, stop
                        break;
                    }
                    machine_match = tokens[i + 1] == "archive.org";
                    if !machine_match {
                        login = None;
                        password = None;
                    }
                    i += 2;
                }
                "login" if machine_match && i + 1 < tokens.len() => {
                    login = Some(tokens[i + 1].to_string());
                    i += 2;
                }
                "password" if machine_match && i + 1 < tokens.len() => {
                    password = Some(tokens[i + 1].to_string());
                    i += 2;
                }
                _ => {
                    i += 1;
                }
            }
        }
    }

    match (login, password) {
        (Some(l), Some(p)) => Ok((l, p)),
        _ => Err(IaError::Auth("no archive.org entry found in netrc file".into())),
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core auth::tests::parse_netrc -- --nocapture`
Expected: all 3 tests pass

**Step 5: Commit**

```
feat(ia-core): add netrc parsing for archive.org credentials

parse_netrc() reads a .netrc file and extracts the login/password
for the archive.org machine entry. Used by `ia config login --netrc`.
```

---

## Task 7: Create the CLI `config` command with subcommands

**Files:**
- Create: `ia-cli/src/commands/config.rs`
- Modify: `ia-cli/src/commands/mod.rs`
- Modify: `ia-cli/src/main.rs`

**Step 1: Create the config command module**

Create `ia-cli/src/commands/config.rs`:

```rust
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use color_print::cstr;

/// Configure Internet Archive credentials and settings.
#[derive(Debug, Args)]
#[command(
    long_about = "Configure Internet Archive credentials and settings.\n\n\
        Log in to archive.org, view configuration, validate credentials, and \
        retrieve account information.",
    after_long_help = cstr!(
        "<bold><green>Examples:</green></bold>\n  \
         <dim># Interactive login (prompts for email and password)</dim>\n  \
         ia config login\n\n  \
         <dim># Non-interactive login</dim>\n  \
         ia config login -u user@example.com -p mypassword\n\n  \
         <dim># Show current config (secrets redacted)</dim>\n  \
         ia config show\n\n  \
         <dim># Check if stored credentials are valid</dim>\n  \
         ia config check\n\n  \
         <dim># Show account info</dim>\n  \
         ia config whoami"
    )
)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Log in to archive.org and save credentials
    #[command(
        long_about = "Log in to archive.org and save credentials to the config file.\n\n\
            Authenticates with archive.org using your email and password, then \
            writes S3 keys and cookies to the config file. If a config file already \
            exists, existing settings (host, logging, etc.) are preserved.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Interactive login</dim>\n  \
             ia config login\n\n  \
             <dim># Non-interactive login</dim>\n  \
             ia config login -u user@example.com -p mypassword\n\n  \
             <dim># Login using .netrc credentials</dim>\n  \
             ia config login --netrc"
        )
    )]
    Login(LoginArgs),

    /// Print current configuration
    #[command(
        long_about = "Print the current configuration as JSON.\n\n\
            Shows all config sections (s3, cookies, general, logging, ai). \
            Secrets (S3 keys, cookies, AI API key) are redacted by default.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Show config with redacted secrets</dim>\n  \
             ia config show\n\n  \
             <dim># Machine-readable JSON output</dim>\n  \
             ia config show --json"
        )
    )]
    Show(ShowArgs),

    /// Validate stored S3 credentials
    #[command(
        long_about = "Check if the stored S3 credentials are valid by contacting archive.org.\n\n\
            Exits with code 0 if valid, 1 if invalid.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Check credentials</dim>\n  \
             ia config check\n\n  \
             <dim># Check with JSON output</dim>\n  \
             ia config check --json"
        )
    )]
    Check(CheckArgs),

    /// Show account information
    #[command(
        long_about = "Retrieve and display account information from archive.org.\n\n\
            Shows your screenname, email, and itemname.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Show account info</dim>\n  \
             ia config whoami\n\n  \
             <dim># JSON output</dim>\n  \
             ia config whoami --json"
        )
    )]
    Whoami(WhoamiArgs),

    /// Print cookies in Netscape format
    #[command(
        name = "print-cookies",
        long_about = "Print stored cookies in Netscape cookie format.\n\n\
            Outputs cookies suitable for use with curl, wget, or other tools \
            that accept Netscape-format cookie files.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Print cookies</dim>\n  \
             ia config print-cookies\n\n  \
             <dim># Save to cookie file for curl</dim>\n  \
             ia config print-cookies > cookies.txt\n  \
             curl -b cookies.txt https://archive.org/..."
        )
    )]
    PrintCookies(PrintCookiesArgs),

    /// Print the Authorization header
    #[command(
        name = "print-auth",
        long_about = "Print the Authorization header value for S3 API requests.\n\n\
            Outputs the header in the format: Authorization: LOW {access}:{secret}\n\
            Useful for scripting with curl or other HTTP tools.",
        after_long_help = cstr!(
            "<bold><green>Examples:</green></bold>\n  \
             <dim># Print auth header</dim>\n  \
             ia config print-auth\n\n  \
             <dim># Use with curl</dim>\n  \
             curl -H \"$(ia config print-auth)\" https://s3.us.archive.org/..."
        )
    )]
    PrintAuth(PrintAuthArgs),
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Email address for login
    #[arg(short, long)]
    pub username: Option<String>,

    /// Password for login
    #[arg(short, long)]
    pub password: Option<String>,

    /// Read credentials from ~/.netrc
    #[arg(short, long)]
    pub netrc: bool,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Output as JSON (machine-readable, no color)
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct WhoamiArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PrintCookiesArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PrintAuthArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}
```

**Step 2: Register the module**

In `ia-cli/src/commands/mod.rs`, add `pub mod config;` (alphabetical order, after `pub mod completions;`).

**Step 3: Add to Commands enum in main.rs**

In the `Commands` enum in `main.rs`, add after `Completions`:

```rust
    /// Configure credentials and settings
    Config(commands::config::ConfigArgs),
```

**Step 4: Handle config command dispatch**

The `config` command (specifically `login`) needs special handling — it may not need a full `IaClient` (e.g., login creates credentials that don't yet exist). Handle it in the "commands that don't need config/client" early-return block in `main.rs`.

In the first `match` block (lines 116-126 of main.rs), add before the `_ => {}` catch-all:

```rust
        Commands::Config(args) => {
            // Config command handles its own config/client creation
            // because some subcommands (login) don't require existing credentials
            let config = if let Some(path) = &cli.config {
                ia_core::IaConfig::load_from_file(path)?
            } else {
                ia_core::IaConfig::load()?
            };
            return commands::config::run(args, config, cli.config.clone()).await;
        }
```

Also add the `Config(_)` arm to the unreachable match below:

```rust
        Commands::Config(_) => unreachable!("handled above"),
```

**Step 5: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: error — `run` function not defined in config module (that's OK, we'll add it in the next task)

**Step 6: Add a placeholder `run` function**

Add to the bottom of `ia-cli/src/commands/config.rs`:

```rust
/// Run the config command.
pub async fn run(
    args: ConfigArgs,
    config: ia_core::IaConfig,
    config_path: Option<PathBuf>,
) -> Result<()> {
    match args.command {
        ConfigCommand::Login(_) => todo!("login"),
        ConfigCommand::Show(_) => todo!("show"),
        ConfigCommand::Check(_) => todo!("check"),
        ConfigCommand::Whoami(_) => todo!("whoami"),
        ConfigCommand::PrintCookies(_) => todo!("print-cookies"),
        ConfigCommand::PrintAuth(_) => todo!("print-auth"),
    }
}
```

**Step 7: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: success (with warnings about unused variables, OK for now)

**Step 8: Commit**

```
feat(ia-cli): add ia config command skeleton with subcommands

Adds ConfigArgs with six subcommands: login, show, check, whoami,
print-cookies, print-auth. First command to use the sub-subcommand
pattern. Includes full help text with examples for all subcommands.
All handlers are placeholder todo!() — implemented in following commits.
```

---

## Task 8: Implement `config show` subcommand

**Files:**
- Modify: `ia-cli/src/commands/config.rs`

**Step 1: Write a CLI integration test**

Create test in `ia-cli/tests/config_show.rs`:

```rust
use assert_cmd::Command;
use std::io::Write;

#[test]
fn config_show_displays_json() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[s3]").unwrap();
    writeln!(f, "access = test-access").unwrap();
    writeln!(f, "secret = test-secret").unwrap();
    writeln!(f, "[general]").unwrap();
    writeln!(f, "host = archive.org").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "show"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should contain general info but redact secrets
    assert!(stdout.contains("REDACTED"), "secrets should be redacted: {stdout}");
    assert!(stdout.contains("archive.org"), "should show host: {stdout}");
    assert!(!stdout.contains("test-access"), "should NOT show raw access key: {stdout}");
}

#[test]
fn config_show_json_mode() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[general]").unwrap();
    writeln!(f, "host = archive.org").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "show", "--json"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .expect(&format!("should be valid JSON: {stdout}"));
    assert!(parsed.get("general").is_some());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-cli --test config_show`
Expected: FAIL — todo!() panic

**Step 3: Implement the `show` handler**

Replace the `Show` match arm in `run()` in `config.rs`:

```rust
        ConfigCommand::Show(show_args) => {
            let json_value = config.to_json(true);
            if show_args.json {
                println!("{}", serde_json::to_string(&json_value)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&json_value)?);
            }
            Ok(())
        }
```

Add at the top of the file if not present:

```rust
use console::style;
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --test config_show`
Expected: both tests pass

**Step 5: Commit**

```
feat(ia-cli): implement ia config show

Displays current config as pretty-printed JSON with secrets redacted.
Supports --json for machine-readable output.
```

---

## Task 9: Implement `config login` subcommand

**Files:**
- Modify: `ia-cli/src/commands/config.rs`

**Step 1: Write CLI integration test**

Create `ia-cli/tests/config_login.rs`:

```rust
use assert_cmd::Command;

#[test]
fn config_login_missing_password_flag_without_tty() {
    // When not on a TTY and no -p flag, login should error
    // (can't prompt for password in non-interactive mode)
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args([
        "--config-file", ini_path.to_str().unwrap(),
        "config", "login",
        "-u", "user@example.com",
        // no -p flag, not a TTY → should fail
    ]);
    cmd.assert().failure();
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-cli --test config_login`
Expected: FAIL — todo!() panic (not the clean error we want)

**Step 3: Implement the `login` handler**

Replace the `Login` match arm in `run()`:

```rust
        ConfigCommand::Login(login_args) => {
            let (email, password) = if login_args.netrc {
                let netrc_path = dirs_path()
                    .ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?
                    .join(".netrc");
                ia_core::auth::parse_netrc(&netrc_path)?
            } else {
                let email = match login_args.username {
                    Some(u) => u,
                    None => {
                        if !atty::is(atty::Stream::Stdin) {
                            anyhow::bail!(
                                "no username provided and stdin is not a terminal.\n\
                                 Use -u/--username and -p/--password for non-interactive login."
                            );
                        }
                        eprint!("Email address: ");
                        let mut input = String::new();
                        std::io::stdin().read_line(&mut input)?;
                        input.trim().to_string()
                    }
                };
                let password = match login_args.password {
                    Some(p) => p,
                    None => {
                        if !atty::is(atty::Stream::Stdin) {
                            anyhow::bail!(
                                "no password provided and stdin is not a terminal.\n\
                                 Use -u/--username and -p/--password for non-interactive login."
                            );
                        }
                        rpassword::prompt_password("Password: ")?
                    }
                };
                (email, password)
            };

            // Build a minimal client for the login request
            let client = ia_core::IaClient::from_config(config)?;
            let auth = ia_core::auth::login(&client, &email, &password).await?;

            // Determine where to write the config
            let write_path = config_path
                .unwrap_or_else(ia_core::IaConfig::find_or_default_config_path);

            ia_core::IaConfig::write_config_file(&auth, &write_path)?;

            if login_args.json {
                let json = serde_json::json!({
                    "config_file": write_path.display().to_string(),
                    "screenname": auth.screenname,
                });
                println!("{}", serde_json::to_string(&json)?);
            } else {
                eprintln!(
                    "{} Config saved to {}",
                    style("✓").green().bold(),
                    style(write_path.display()).cyan()
                );
            }

            Ok(())
        }
```

Add `rpassword` to `ia-cli/Cargo.toml` dependencies:

```toml
rpassword = "5"
```

Also add `atty` to `ia-cli/Cargo.toml` for TTY detection:

```toml
atty = "0.2"
```

Add the necessary import at the top of `config.rs`:

```rust
fn dirs_path() -> Option<std::path::PathBuf> {
    std::env::var("HOME").ok().map(std::path::PathBuf::from)
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --test config_login`
Expected: pass

**Step 5: Commit**

```
feat(ia-cli): implement ia config login

Supports interactive login (prompts for email + hidden password),
non-interactive (-u/-p flags), and --netrc. Writes credentials to
config file, merging with existing settings. Supports --json output.
```

---

## Task 10: Implement `config check` and `config whoami` subcommands

**Files:**
- Modify: `ia-cli/src/commands/config.rs`

**Step 1: Implement check and whoami handlers**

Replace the `Check` and `Whoami` match arms in `run()`:

```rust
        ConfigCommand::Check(check_args) => {
            let client = ia_core::IaClient::from_config(config)?;
            match ia_core::auth::check_keys(&client).await {
                Ok(info) => {
                    if check_args.json {
                        let json = serde_json::json!({
                            "valid": true,
                            "screenname": info.screenname,
                            "email": info.email,
                            "itemname": info.itemname,
                        });
                        println!("{}", serde_json::to_string(&json)?);
                    } else {
                        eprintln!(
                            "{} Credentials valid ({})",
                            style("✓").green().bold(),
                            style(&info.screenname).cyan()
                        );
                    }
                    Ok(())
                }
                Err(e) => {
                    if check_args.json {
                        let json = serde_json::json!({
                            "valid": false,
                            "error": e.to_string(),
                        });
                        println!("{}", serde_json::to_string(&json)?);
                        std::process::exit(1);
                    } else {
                        eprintln!("{} {}", style("✗").red().bold(), e);
                        std::process::exit(1);
                    }
                }
            }
        }

        ConfigCommand::Whoami(whoami_args) => {
            let client = ia_core::IaClient::from_config(config)?;
            let info = ia_core::auth::whoami(&client).await?;

            if whoami_args.json {
                let json = serde_json::json!({
                    "screenname": info.screenname,
                    "email": info.email,
                    "itemname": info.itemname,
                });
                println!("{}", serde_json::to_string(&json)?);
            } else {
                println!("Screenname: {}", style(&info.screenname).cyan());
                println!("Email:      {}", info.email);
                if let Some(itemname) = &info.itemname {
                    println!("Itemname:   {}", itemname);
                }
            }

            Ok(())
        }
```

**Step 2: Verify it compiles**

Run: `cargo check -p ia-cli`
Expected: success

**Step 3: Commit**

```
feat(ia-cli): implement ia config check and whoami

check validates S3 credentials against the IA API, exits 0/1.
whoami retrieves and displays account info (screenname, email, itemname).
Both support --json output.
```

---

## Task 11: Implement `config print-cookies` and `config print-auth`

**Files:**
- Modify: `ia-cli/src/commands/config.rs`

**Step 1: Write CLI integration tests**

Create `ia-cli/tests/config_print.rs`:

```rust
use assert_cmd::Command;
use std::io::Write;

#[test]
fn print_auth_outputs_header() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[s3]").unwrap();
    writeln!(f, "access = myaccess").unwrap();
    writeln!(f, "secret = mysecret").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-auth"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "Authorization: LOW myaccess:mysecret");
}

#[test]
fn print_auth_json() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[s3]").unwrap();
    writeln!(f, "access = myaccess").unwrap();
    writeln!(f, "secret = mysecret").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-auth", "--json"]);
    let output = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["header"], "Authorization: LOW myaccess:mysecret");
}

#[test]
fn print_cookies_outputs_netscape_format() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[cookies]").unwrap();
    writeln!(f, "logged-in-user = user%40example.com").unwrap();
    writeln!(f, "logged-in-sig = test-sig").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-cookies"]);
    let output = cmd.output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Netscape format: domain, flag, path, secure, expiry, name, value
    assert!(stdout.contains("logged-in-user"), "should contain cookie name: {stdout}");
    assert!(stdout.contains("user%40example.com"), "should contain cookie value: {stdout}");
}

#[test]
fn print_auth_no_credentials_fails() {
    let dir = tempfile::tempdir().unwrap();
    let ini_path = dir.path().join("ia.ini");
    let mut f = std::fs::File::create(&ini_path).unwrap();
    writeln!(f, "[general]").unwrap();
    writeln!(f, "host = archive.org").unwrap();

    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.args(["--config-file", ini_path.to_str().unwrap(), "config", "print-auth"]);
    cmd.assert().failure();
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-cli --test config_print`
Expected: FAIL

**Step 3: Implement print-cookies and print-auth handlers**

Replace the match arms in `run()`:

```rust
        ConfigCommand::PrintCookies(args) => {
            if config.cookies.is_empty() {
                anyhow::bail!("no cookies found in config. Run `ia config login` first.");
            }

            if args.json {
                let json = serde_json::json!(config.cookies);
                println!("{}", serde_json::to_string(&json)?);
            } else {
                // Netscape cookie format:
                // domain  flag  path  secure  expiry  name  value
                for (name, value) in &config.cookies {
                    println!(
                        ".archive.org\tTRUE\t/\tTRUE\t0\t{}\t{}",
                        name, value
                    );
                }
            }
            Ok(())
        }

        ConfigCommand::PrintAuth(args) => {
            let access = config
                .s3_access
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("no S3 access key found in config. Run `ia config login` first."))?;
            let secret = config
                .s3_secret
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("no S3 secret key found in config. Run `ia config login` first."))?;

            let header = format!("Authorization: LOW {access}:{secret}");

            if args.json {
                let json = serde_json::json!({"header": header});
                println!("{}", serde_json::to_string(&json)?);
            } else {
                println!("{header}");
            }
            Ok(())
        }
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-cli --test config_print`
Expected: all tests pass

**Step 5: Commit**

```
feat(ia-cli): implement ia config print-cookies and print-auth

print-cookies outputs cookies in Netscape format for curl/wget.
print-auth outputs the Authorization: LOW header for scripting.
Both support --json output and fail gracefully when credentials
are missing.
```

---

## Task 12: Update `require_auth()` error message and run full test suite

**Files:**
- Modify: `ia-core/src/client.rs` (line 171)

**Step 1: Update the error message**

In `ia-core/src/client.rs`, change the `require_auth()` error message from:

```rust
"S3 credentials required. Run `ia configure` or set \
 IA_ACCESS_KEY_ID/IA_SECRET_ACCESS_KEY environment variables."
```

To:

```rust
"S3 credentials required. Run `ia config login` or set \
 IA_ACCESS_KEY_ID/IA_SECRET_ACCESS_KEY environment variables."
```

**Step 2: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: all tests pass

**Step 3: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: zero warnings

**Step 4: Commit**

```
fix: update require_auth error message to reference ia config login

The configure command was renamed to `ia config login` as part of the
sub-subcommand redesign.
```

---

## Task 13: Update Crate Stack in CLAUDE.md

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Add new dependencies to the Crate Stack section**

Add after the Testing line:

```markdown
- Auth: `rpassword` 5 — hidden password input for interactive login; `atty` 0.2 — TTY detection for interactive/non-interactive mode
```

**Step 2: Commit**

```
docs: update CLAUDE.md crate stack with auth dependencies
```

---

## Task 14: Final verification and cleanup

**Step 1: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: all tests pass

**Step 2: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: zero warnings

**Step 3: Verify help text**

Run: `cargo run -- config --help`
Run: `cargo run -- config login --help`
Run: `cargo run -- config show --help`

Verify all subcommands are listed and help text is readable.

**Step 4: Final commit (if any cleanup needed)**

---

## GitHub Issues to Create

Create these issues on `jjjake/ia` before starting implementation:

1. **Add `ia-core` auth module** — Login via xauthn API, credential validation, netrc parsing (Tasks 1-2, 5-6)
2. **Add config file writing support** — write_config_file() with merge and permissions (Task 3)
3. **Add config JSON serialization** — to_json() with secret redaction for `show` command (Task 4)
4. **Add `ia config` CLI command** — Six subcommands: login, show, check, whoami, print-cookies, print-auth (Tasks 7-11)
5. **Update safety rules for auth support** — Allow auth for read operations, maintain write-request prohibition (Task 12-13)
