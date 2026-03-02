# Self-Updater (`ia update`) Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `ia update` command that checks GitHub Releases for new versions and replaces the binary in-place, gated behind a compile-time `self-update` feature flag.

**Architecture:** Build script emits the target triple as `IA_TARGET`. Core update logic lives in `ia-core/src/update.rs` (types, version check, asset matching, download, replace). CLI command in `ia-cli/src/commands/update.rs` handles args and output. Feature flag `self-update` gates the entire subcommand.

**Tech Stack:** `reqwest` (plain, not middleware — no IA retry needed for GitHub API), `serde`/`serde_json`, `indicatif`, `std::fs` for atomic replace.

---

### Task 1: Build Script and Feature Flag

**Files:**
- Create: `ia-cli/build.rs`
- Modify: `ia-cli/Cargo.toml:29-31`

**Step 1: Create the build script**

Create `ia-cli/build.rs`:

```rust
fn main() {
    // Emit the Rust target triple so the binary knows which release asset to download.
    // Used by the self-update feature to find the matching GitHub Release asset.
    println!(
        "cargo:rustc-env=IA_TARGET={}",
        std::env::var("TARGET").unwrap()
    );
}
```

**Step 2: Add the `self-update` feature to Cargo.toml**

In `ia-cli/Cargo.toml`, after the existing `tui` feature (line 31), add:

```toml
self-update = []
```

So the `[features]` section becomes:
```toml
[features]
default = ["tui"]
tui = ["dep:ratatui", "dep:crossterm"]
self-update = []
```

**Step 3: Verify build script works**

Run: `cargo build -p ia-cli 2>&1 | head -5`
Expected: Compiles successfully. No errors.

Verify the env var is set by temporarily adding to any file:
```rust
// let _ = env!("IA_TARGET");  // would fail at compile time if not set
```

**Step 4: Commit**

```bash
git add ia-cli/build.rs ia-cli/Cargo.toml
git commit -m "feat: add build script for IA_TARGET and self-update feature flag

The build script emits the Rust target triple as IA_TARGET so the
self-update command can find the correct GitHub Release asset.
The self-update feature flag gates the update subcommand — it's only
enabled in release workflow builds."
```

---

### Task 2: Update Error Variants

**Files:**
- Modify: `ia-core/src/error.rs:20-67` (IaError enum)
- Modify: `ia-core/src/error.rs:78-103` (is_retryable)
- Modify: `ia-core/src/error.rs:106-167` (to_json_error)

**Step 1: Write tests for the new error variants**

Add these tests at the bottom of `ia-core/src/error.rs` `mod tests`, before the closing `}`:

```rust
    #[test]
    fn update_no_asset_displays_target() {
        let err = IaError::UpdateNoAsset {
            target: "aarch64-apple-darwin".into(),
        };
        assert!(err.to_string().contains("aarch64-apple-darwin"));
    }

    #[test]
    fn update_api_error_displays_status() {
        let err = IaError::UpdateApiError {
            status: 403,
            message: "rate limited".into(),
        };
        assert!(err.to_string().contains("403"));
    }

    #[test]
    fn update_verify_failed_displays_versions() {
        let err = IaError::UpdateVerifyFailed {
            expected: "0.4.4".into(),
            actual: "0.4.3".into(),
        };
        assert!(err.to_string().contains("0.4.4"));
        assert!(err.to_string().contains("0.4.3"));
    }

    #[test]
    fn json_update_no_asset() {
        let err = IaError::UpdateNoAsset {
            target: "aarch64-apple-darwin".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_no_asset");
        assert_eq!(v["error"]["target"], "aarch64-apple-darwin");
    }

    #[test]
    fn json_update_api_error() {
        let err = IaError::UpdateApiError {
            status: 403,
            message: "rate limited".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_api_error");
        assert_eq!(v["error"]["status"], 403);
    }

    #[test]
    fn json_update_verify_failed() {
        let err = IaError::UpdateVerifyFailed {
            expected: "0.4.4".into(),
            actual: "0.4.3".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_verify_failed");
        assert_eq!(v["error"]["expected"], "0.4.4");
        assert_eq!(v["error"]["actual"], "0.4.3");
    }

    #[test]
    fn update_errors_are_not_retryable() {
        assert!(!IaError::UpdateNoAsset { target: "x".into() }.is_retryable());
        assert!(!IaError::UpdateVerifyFailed {
            expected: "a".into(),
            actual: "b".into(),
        }
        .is_retryable());
    }

    #[test]
    fn update_api_error_retryable_on_5xx() {
        assert!(IaError::UpdateApiError {
            status: 500,
            message: "error".into(),
        }
        .is_retryable());
        assert!(!IaError::UpdateApiError {
            status: 403,
            message: "forbidden".into(),
        }
        .is_retryable());
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- error::tests::update`
Expected: FAIL — `IaError` doesn't have the `Update*` variants yet.

**Step 3: Add error variants to the IaError enum**

In `ia-core/src/error.rs`, add these variants to the `IaError` enum (after the `LlmApi` variant, before `Network`):

```rust
    #[error("no release asset found for target {target}")]
    UpdateNoAsset { target: String },

    #[error("update API error ({status}): {message}")]
    UpdateApiError { status: u16, message: String },

    #[error("update verification failed: expected {expected}, got {actual}")]
    UpdateVerifyFailed { expected: String, actual: String },
```

**Step 4: Add is_retryable arms**

In the `is_retryable` match, add (before the `// Permanent` section):

```rust
            // Update errors: API errors retry on 5xx, others are permanent
            IaError::UpdateApiError { status, .. } => {
                *status == 429 || *status >= 500
            }
            IaError::UpdateNoAsset { .. } => false,
            IaError::UpdateVerifyFailed { .. } => false,
```

**Step 5: Add to_json_error arms**

In the `to_json_error` match, add (before `IaError::Network`):

```rust
            IaError::UpdateNoAsset { target } => {
                extra.insert("target".into(), target.clone().into());
                "update_no_asset"
            }
            IaError::UpdateApiError { status, .. } => {
                extra.insert("status".into(), (*status).into());
                "update_api_error"
            }
            IaError::UpdateVerifyFailed { expected, actual } => {
                extra.insert("expected".into(), expected.clone().into());
                extra.insert("actual".into(), actual.clone().into());
                "update_verify_failed"
            }
```

**Step 6: Run tests to verify they pass**

Run: `cargo test -p ia-core -- error::tests`
Expected: ALL PASS (including all existing tests — the new variants must not break existing match exhaustiveness).

**Step 7: Commit**

```bash
git add ia-core/src/error.rs
git commit -m "feat: add update error variants to IaError

Add UpdateNoAsset, UpdateApiError, and UpdateVerifyFailed variants
with JSON error codes, retryability rules, and display formatting.
API errors retry on 5xx; the rest are permanent failures."
```

---

### Task 3: Core Types and Version Comparison

**Files:**
- Create: `ia-core/src/update.rs`
- Modify: `ia-core/src/lib.rs:1-14` (add `pub mod update;`)

**Step 1: Write tests for version parsing and comparison**

Create `ia-core/src/update.rs` with tests first:

```rust
use serde::Deserialize;

/// A GitHub release from the releases API.
#[derive(Debug, Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub assets: Vec<GitHubAsset>,
}

/// A single asset attached to a GitHub release.
#[derive(Debug, Deserialize)]
pub struct GitHubAsset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// Result of checking for an update.
#[derive(Debug)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release: Option<GitHubRelease>,
}

/// Parse a version string like "0.4.3" into a comparable tuple.
/// Strips leading 'v' if present.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

/// Returns true if `latest` is newer than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_simple() {
        assert_eq!(parse_version("0.4.3"), Some((0, 4, 3)));
    }

    #[test]
    fn parse_version_strips_v_prefix() {
        assert_eq!(parse_version("v0.4.3"), Some((0, 4, 3)));
    }

    #[test]
    fn parse_version_invalid() {
        assert_eq!(parse_version("not-a-version"), None);
        assert_eq!(parse_version("0.4"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn is_newer_basic() {
        assert!(is_newer("0.4.3", "0.4.4"));
        assert!(is_newer("0.4.3", "0.5.0"));
        assert!(is_newer("0.4.3", "1.0.0"));
    }

    #[test]
    fn is_newer_same_version() {
        assert!(!is_newer("0.4.3", "0.4.3"));
    }

    #[test]
    fn is_newer_older_version() {
        assert!(!is_newer("0.4.4", "0.4.3"));
        assert!(!is_newer("1.0.0", "0.9.9"));
    }

    #[test]
    fn is_newer_handles_v_prefix() {
        assert!(is_newer("0.4.3", "v0.4.4"));
        assert!(is_newer("v0.4.3", "0.4.4"));
    }

    #[test]
    fn is_newer_numeric_comparison_not_lexicographic() {
        // "0.4.10" > "0.4.9" numerically, but "10" < "9" lexicographically
        assert!(is_newer("0.4.9", "0.4.10"));
        assert!(!is_newer("0.4.10", "0.4.9"));
    }

    #[test]
    fn github_release_deserialize() {
        let json = r#"{
            "tag_name": "v0.4.4",
            "assets": [
                {
                    "name": "ia-aarch64-apple-darwin",
                    "browser_download_url": "https://github.com/jjjake/ia/releases/download/v0.4.4/ia-aarch64-apple-darwin",
                    "size": 12345678
                },
                {
                    "name": "ia-x86_64-unknown-linux-musl",
                    "browser_download_url": "https://github.com/jjjake/ia/releases/download/v0.4.4/ia-x86_64-unknown-linux-musl",
                    "size": 23456789
                }
            ]
        }"#;
        let release: GitHubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v0.4.4");
        assert_eq!(release.assets.len(), 2);
        assert_eq!(release.assets[0].name, "ia-aarch64-apple-darwin");
    }
}
```

**Step 2: Register the module**

In `ia-core/src/lib.rs`, add after line 13 (`pub mod user_agent;`):

```rust
pub mod update;
```

**Step 3: Run tests to verify they pass**

Run: `cargo test -p ia-core -- update::tests`
Expected: ALL PASS.

**Step 4: Commit**

```bash
git add ia-core/src/update.rs ia-core/src/lib.rs
git commit -m "feat: add update module with types and version comparison

Adds GitHubRelease/GitHubAsset serde types, version parsing
(handles v-prefix, numeric comparison), and is_newer comparison.
Tests cover edge cases: v-prefix, numeric vs lexicographic ordering."
```

---

### Task 4: Asset Matching

**Files:**
- Modify: `ia-core/src/update.rs`

**Step 1: Write tests for asset matching**

Add these tests to the `mod tests` block in `ia-core/src/update.rs`:

```rust
    #[test]
    fn find_asset_matches_target() {
        let assets = vec![
            GitHubAsset {
                name: "ia-aarch64-apple-darwin".into(),
                browser_download_url: "https://example.com/ia-aarch64-apple-darwin".into(),
                size: 100,
            },
            GitHubAsset {
                name: "ia-x86_64-unknown-linux-musl".into(),
                browser_download_url: "https://example.com/ia-x86_64-unknown-linux-musl".into(),
                size: 200,
            },
        ];
        let result = find_matching_asset(&assets, "aarch64-apple-darwin");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "ia-aarch64-apple-darwin");
    }

    #[test]
    fn find_asset_matches_windows() {
        let assets = vec![
            GitHubAsset {
                name: "ia-x86_64-pc-windows-msvc.exe".into(),
                browser_download_url: "https://example.com/ia-x86_64-pc-windows-msvc.exe".into(),
                size: 300,
            },
        ];
        let result = find_matching_asset(&assets, "x86_64-pc-windows-msvc");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "ia-x86_64-pc-windows-msvc.exe");
    }

    #[test]
    fn find_asset_returns_none_when_missing() {
        let assets = vec![
            GitHubAsset {
                name: "ia-x86_64-unknown-linux-musl".into(),
                browser_download_url: "https://example.com/ia-x86_64-unknown-linux-musl".into(),
                size: 200,
            },
        ];
        let result = find_matching_asset(&assets, "aarch64-apple-darwin");
        assert!(result.is_none());
    }

    #[test]
    fn find_asset_empty_list() {
        let assets: Vec<GitHubAsset> = vec![];
        let result = find_matching_asset(&assets, "aarch64-apple-darwin");
        assert!(result.is_none());
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- update::tests::find_asset`
Expected: FAIL — `find_matching_asset` doesn't exist yet.

**Step 3: Implement find_matching_asset**

Add this function to `ia-core/src/update.rs` (after `is_newer`, before `#[cfg(test)]`):

```rust
/// Find the release asset matching the given target triple.
/// Looks for `ia-{target}` or `ia-{target}.exe`.
pub fn find_matching_asset<'a>(assets: &'a [GitHubAsset], target: &str) -> Option<&'a GitHubAsset> {
    let name = format!("ia-{target}");
    let name_exe = format!("ia-{target}.exe");
    assets.iter().find(|a| a.name == name || a.name == name_exe)
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- update::tests`
Expected: ALL PASS.

**Step 5: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat: add find_matching_asset for release asset lookup

Matches assets by ia-{target} or ia-{target}.exe pattern.
Handles Linux, macOS, and Windows naming conventions."
```

---

### Task 5: Check for Update (HTTP)

**Files:**
- Modify: `ia-core/src/update.rs`

**Step 1: Write integration test with wiremock**

Add to the `#[cfg(test)] mod tests` block:

```rust
    #[tokio::test]
    async fn check_for_update_newer_version() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/latest"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "tag_name": "v99.0.0",
                    "assets": [{
                        "name": "ia-aarch64-apple-darwin",
                        "browser_download_url": "https://example.com/ia-aarch64-apple-darwin",
                        "size": 100
                    }]
                }),
            ))
            .mount(&mock_server)
            .await;

        let check = check_for_update("0.4.3", &mock_server.uri()).await.unwrap();
        assert!(check.update_available);
        assert_eq!(check.latest_version, "99.0.0");
        assert_eq!(check.current_version, "0.4.3");
        assert!(check.release.is_some());
    }

    #[tokio::test]
    async fn check_for_update_already_current() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/latest"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "tag_name": "v0.4.3",
                    "assets": []
                }),
            ))
            .mount(&mock_server)
            .await;

        let check = check_for_update("0.4.3", &mock_server.uri()).await.unwrap();
        assert!(!check.update_available);
        assert_eq!(check.latest_version, "0.4.3");
    }

    #[tokio::test]
    async fn check_for_update_api_error() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/latest"))
            .respond_with(wiremock::ResponseTemplate::new(403))
            .mount(&mock_server)
            .await;

        let result = check_for_update("0.4.3", &mock_server.uri()).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, crate::error::IaError::UpdateApiError { status: 403, .. }));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- update::tests::check_for_update`
Expected: FAIL — `check_for_update` doesn't exist yet.

**Step 3: Implement check_for_update**

Add to `ia-core/src/update.rs`, after `find_matching_asset`:

```rust
use crate::error::IaError;

const GITHUB_API_BASE: &str = "https://api.github.com";

/// Check GitHub Releases for a newer version.
///
/// `api_base` allows overriding the GitHub API URL for testing (pass wiremock URL).
/// In production, pass `GITHUB_API_BASE`.
pub async fn check_for_update(current_version: &str, api_base: &str) -> crate::Result<UpdateCheck> {
    let url = format!("{api_base}/repos/jjjake/ia/releases/latest");
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", format!("ia/{current_version}"))
        .send()
        .await
        .map_err(|e| IaError::UpdateApiError {
            status: 0,
            message: e.to_string(),
        })?;

    if !response.status().is_success() {
        return Err(IaError::UpdateApiError {
            status: response.status().as_u16(),
            message: format!("GitHub API returned {}", response.status()),
        });
    }

    let release: GitHubRelease = response.json().await.map_err(|e| IaError::UpdateApiError {
        status: 0,
        message: format!("failed to parse release JSON: {e}"),
    })?;

    let latest_version = release.tag_name.strip_prefix('v')
        .unwrap_or(&release.tag_name)
        .to_string();

    let update_available = is_newer(current_version, &latest_version);

    Ok(UpdateCheck {
        current_version: current_version.to_string(),
        latest_version,
        update_available,
        release: if update_available { Some(release) } else { None },
    })
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- update::tests`
Expected: ALL PASS.

**Step 5: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat: add check_for_update with GitHub Releases API

Fetches latest release from GitHub API, compares versions, and
returns UpdateCheck with availability info. API base URL is
injectable for testing with wiremock."
```

---

### Task 6: Download and Replace Logic

**Files:**
- Modify: `ia-core/src/update.rs`

This task is harder to test fully in unit tests because it involves filesystem operations and binary replacement. We'll test the pieces we can and leave the full E2E flow for integration tests.

**Step 1: Write tests for the download and replace helpers**

Add to the `mod tests` block:

```rust
    #[tokio::test]
    async fn download_asset_to_temp_file() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"#!/bin/sh\necho hello";
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/download/ia-test"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_bytes(fake_binary.to_vec())
                    .insert_header("content-length", fake_binary.len().to_string()),
            )
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let url = format!("{}/download/ia-test", mock_server.uri());
        let dest = dir.path().join("ia-new");
        download_asset(&url, &dest, "0.4.3").await.unwrap();

        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), fake_binary);
    }

    #[test]
    fn atomic_replace_swaps_files() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("ia");
        let replacement = dir.path().join("ia-new");

        std::fs::write(&original, b"old").unwrap();
        std::fs::write(&replacement, b"new").unwrap();

        replace_binary(&replacement, &original).unwrap();

        assert_eq!(std::fs::read(&original).unwrap(), b"new");
        // replacement file is gone (renamed)
        assert!(!replacement.exists());
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- update::tests::download_asset update::tests::atomic_replace`
Expected: FAIL — functions don't exist yet.

**Step 3: Implement download_asset and replace_binary**

Add to `ia-core/src/update.rs`:

```rust
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// Download a release asset to a local file path.
pub async fn download_asset(url: &str, dest: &Path, current_version: &str) -> crate::Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header("User-Agent", format!("ia/{current_version}"))
        .send()
        .await
        .map_err(|e| IaError::UpdateApiError {
            status: 0,
            message: format!("download failed: {e}"),
        })?;

    if !response.status().is_success() {
        return Err(IaError::UpdateApiError {
            status: response.status().as_u16(),
            message: format!("download returned {}", response.status()),
        });
    }

    let bytes = response.bytes().await.map_err(|e| IaError::UpdateApiError {
        status: 0,
        message: format!("failed to read download body: {e}"),
    })?;

    let mut file = tokio::fs::File::create(dest).await?;
    file.write_all(&bytes).await?;
    file.flush().await?;

    Ok(())
}

/// Atomically replace the binary at `target` with the file at `source`.
///
/// On Unix: atomic rename (same filesystem required).
/// On Windows: rename target to .old first, then rename source to target.
pub fn replace_binary(source: &Path, target: &Path) -> crate::Result<()> {
    #[cfg(unix)]
    {
        // Copy permissions from original binary
        if let Ok(metadata) = std::fs::metadata(target) {
            let permissions = metadata.permissions();
            std::fs::set_permissions(source, permissions)?;
        } else {
            // If we can't read the original, at least make executable
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(source, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(source, target)?;
    }

    #[cfg(windows)]
    {
        let backup = target.with_extension("old.exe");
        // Remove stale backup if it exists
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(target, &backup)?;
        if let Err(e) = std::fs::rename(source, target) {
            // Rollback: restore the backup
            let _ = std::fs::rename(&backup, target);
            return Err(e.into());
        }
        // Clean up backup
        let _ = std::fs::remove_file(&backup);
    }

    Ok(())
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- update::tests`
Expected: ALL PASS.

**Step 5: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat: add download_asset and replace_binary for self-update

download_asset streams the release asset to a local file.
replace_binary does atomic rename on Unix with permission
preservation, and rename-with-backup on Windows."
```

---

### Task 7: Public perform_update Function

**Files:**
- Modify: `ia-core/src/update.rs`

**Step 1: Write test for perform_update**

Add to the `mod tests` block:

```rust
    #[tokio::test]
    async fn perform_update_full_flow() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-binary-content";

        // Mock the release API
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/latest"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "tag_name": "v99.0.0",
                    "assets": [{
                        "name": "ia-test-target",
                        "browser_download_url": format!("{}/download/ia-test-target", mock_server.uri()),
                        "size": fake_binary.len()
                    }]
                }),
            ))
            .mount(&mock_server)
            .await;

        // Mock the asset download
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/download/ia-test-target"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_bytes(fake_binary.to_vec()),
            )
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let result = perform_update(
            "0.4.3",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            false, // skip_verify — can't run --version on fake binary
        ).await.unwrap();

        assert_eq!(result.new_version, "99.0.0");
        assert_eq!(std::fs::read(&exe_path).unwrap(), fake_binary);
    }

    #[tokio::test]
    async fn perform_update_no_matching_asset() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/latest"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "tag_name": "v99.0.0",
                    "assets": [{
                        "name": "ia-some-other-target",
                        "browser_download_url": "https://example.com/ia-other",
                        "size": 100
                    }]
                }),
            ))
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old").unwrap();

        let result = perform_update("0.4.3", "aarch64-apple-darwin", &exe_path, &mock_server.uri(), false).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IaError::UpdateNoAsset { .. }));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p ia-core -- update::tests::perform_update`
Expected: FAIL — `perform_update` doesn't exist yet.

**Step 3: Implement perform_update**

Add the `UpdateResult` struct and `perform_update` function:

```rust
/// Result of a successful update.
#[derive(Debug)]
pub struct UpdateResult {
    pub current_version: String,
    pub new_version: String,
}

/// Perform the full update: check → download → replace.
///
/// `current_exe` is the path to the running binary (use `std::env::current_exe()`).
/// `skip_verify` skips the post-replace --version check (for testing with non-executable content).
pub async fn perform_update(
    current_version: &str,
    target: &str,
    current_exe: &Path,
    api_base: &str,
    skip_verify: bool,
) -> crate::Result<UpdateResult> {
    let check = check_for_update(current_version, api_base).await?;

    if !check.update_available {
        return Ok(UpdateResult {
            current_version: check.current_version,
            new_version: check.latest_version,
        });
    }

    let release = check.release.as_ref().unwrap();
    let asset = find_matching_asset(&release.assets, target).ok_or_else(|| {
        IaError::UpdateNoAsset {
            target: target.to_string(),
        }
    })?;

    // Download to a temp file in the same directory (for atomic rename)
    let temp_path = current_exe.with_extension("update-tmp");
    // Clean up any stale temp file from a previous failed attempt
    let _ = std::fs::remove_file(&temp_path);

    if let Err(e) = download_asset(&asset.browser_download_url, &temp_path, current_version).await {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    if let Err(e) = replace_binary(&temp_path, current_exe) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    // Verify the new binary works
    if !skip_verify {
        let output = std::process::Command::new(current_exe)
            .arg("--version")
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let version_output = String::from_utf8_lossy(&out.stdout);
                if !version_output.contains(&check.latest_version) {
                    return Err(IaError::UpdateVerifyFailed {
                        expected: check.latest_version,
                        actual: version_output.trim().to_string(),
                    });
                }
            }
            Ok(out) => {
                return Err(IaError::UpdateVerifyFailed {
                    expected: check.latest_version,
                    actual: format!("exit code {}", out.status),
                });
            }
            Err(e) => {
                return Err(IaError::UpdateVerifyFailed {
                    expected: check.latest_version,
                    actual: format!("failed to run: {e}"),
                });
            }
        }
    }

    Ok(UpdateResult {
        current_version: check.current_version,
        new_version: check.latest_version,
    })
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test -p ia-core -- update::tests`
Expected: ALL PASS.

**Step 5: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat: add perform_update for full check-download-replace flow

Orchestrates the entire update pipeline: checks GitHub for a newer
version, finds the matching platform asset, downloads to temp file,
atomically replaces the binary, and optionally verifies the new
binary runs. Cleans up temp files on failure."
```

---

### Task 8: CLI Command (`ia update`)

**Files:**
- Create: `ia-cli/src/commands/update.rs`
- Modify: `ia-cli/src/commands/mod.rs:1-7`
- Modify: `ia-cli/src/main.rs:89-106` (Commands enum)
- Modify: `ia-cli/src/main.rs:108-177` (main fn)

**Step 1: Create the update command**

Create `ia-cli/src/commands/update.rs`:

```rust
use anyhow::{Context, Result};
use clap::Args;
use color_print::cstr;
use console::style;

#[derive(Args)]
#[command(
    about = "Update ia to the latest version",
    long_about = "Check for a newer version of ia on GitHub Releases and update the binary in-place.\n\n\
        This command is only available in standalone release builds. If you installed ia via \
        cargo install or a package manager, use that tool to update instead.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Update to the latest version</dim>\n  <bold>$ ia update</bold>\
         \n\n  <dim># Check for updates without installing</dim>\n  <bold>$ ia update --check</bold>\
         \n\n  <dim># Machine-readable output</dim>\n  <bold>$ ia update --check --json</bold>\n"
    ),
)]
pub struct UpdateArgs {
    /// Only check for updates, don't install
    #[arg(long)]
    pub check: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: UpdateArgs) -> Result<()> {
    let current_version = ia_core::version();
    let target = env!("IA_TARGET");
    let current_exe = std::env::current_exe().context("failed to determine current executable path")?;

    if args.check {
        return run_check(current_version, &args).await;
    }

    if !args.json {
        println!("Checking for updates...");
    }

    let result = ia_core::update::perform_update(
        current_version,
        target,
        &current_exe,
        ia_core::update::GITHUB_API_BASE,
        false,
    )
    .await;

    match result {
        Ok(update_result) => {
            if update_result.current_version == update_result.new_version
                || !ia_core::update::is_newer(&update_result.current_version, &update_result.new_version)
            {
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "up_to_date",
                            "current_version": update_result.current_version,
                        })
                    );
                } else {
                    println!(
                        "  {} ia {} is already the latest version.",
                        style("✓").green(),
                        update_result.current_version,
                    );
                }
            } else {
                if args.json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "updated",
                            "current_version": update_result.current_version,
                            "new_version": update_result.new_version,
                        })
                    );
                } else {
                    println!(
                        "  {} Updated ia from {} to {}",
                        style("✓").green(),
                        style(&update_result.current_version).dim(),
                        style(&update_result.new_version).green().bold(),
                    );
                }
            }
        }
        Err(e) => {
            if args.json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("update failed");
            }
        }
    }

    Ok(())
}

async fn run_check(current_version: &str, args: &UpdateArgs) -> Result<()> {
    let check = ia_core::update::check_for_update(
        current_version,
        ia_core::update::GITHUB_API_BASE,
    )
    .await;

    match check {
        Ok(info) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "current_version": info.current_version,
                        "latest_version": info.latest_version,
                        "update_available": info.update_available,
                    })
                );
            } else if info.update_available {
                println!("Current version: {}", info.current_version);
                println!("Latest version:  {}", style(&info.latest_version).green().bold());
                println!(
                    "\nUpdate available! Run {} to install.",
                    style("ia update").cyan()
                );
            } else {
                println!(
                    "  {} ia {} is already the latest version.",
                    style("✓").green(),
                    info.current_version,
                );
            }
        }
        Err(e) => {
            if args.json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("failed to check for updates");
            }
        }
    }

    Ok(())
}
```

**Step 2: Register in commands/mod.rs**

Add to `ia-cli/src/commands/mod.rs`, after the existing modules:

```rust
#[cfg(feature = "self-update")]
pub mod update;
```

**Step 3: Wire into main.rs Commands enum**

In `ia-cli/src/main.rs`, add to the `Commands` enum (after `Completions`):

```rust
    /// Update ia to the latest version
    #[cfg(feature = "self-update")]
    Update(commands::update::UpdateArgs),
```

**Step 4: Wire into main.rs match**

In `ia-cli/src/main.rs`, the `Update` command doesn't need config or client (it only talks to GitHub). Add it as an early return alongside `Completions`. Change the completions early-return section (lines 113-116) to:

```rust
    // Handle commands that don't need IA config/client
    match cli.command {
        Commands::Completions(args) => {
            let mut cmd = Cli::command();
            return commands::completions::run(args, &mut cmd);
        }
        #[cfg(feature = "self-update")]
        Commands::Update(args) => {
            return commands::update::run(args).await;
        }
        _ => {}
    }
```

Then update the main match at the bottom to add:

```rust
        #[cfg(feature = "self-update")]
        Commands::Update(_) => unreachable!("handled above"),
```

**Step 5: Verify it compiles with the feature**

Run: `cargo build -p ia-cli --features self-update`
Expected: Compiles successfully.

**Step 6: Verify it compiles without the feature**

Run: `cargo build -p ia-cli`
Expected: Compiles successfully. `ia update` is not a recognized command.

**Step 7: Commit**

```bash
git add ia-cli/src/commands/update.rs ia-cli/src/commands/mod.rs ia-cli/src/main.rs
git commit -m "feat: add ia update CLI command behind self-update feature flag

Adds the update subcommand with --check and --json flags. The
command is gated behind cfg(feature = \"self-update\") so it only
exists in release builds. Handles version check, download, replace,
and structured JSON output."
```

---

### Task 9: Make is_newer and GITHUB_API_BASE Public

**Files:**
- Modify: `ia-core/src/update.rs`

The CLI command references `ia_core::update::is_newer` and `ia_core::update::GITHUB_API_BASE`, so these need `pub` visibility.

**Step 1: Verify `is_newer` and `GITHUB_API_BASE` are `pub`**

Check that `is_newer` is `pub fn is_newer(...)` and `GITHUB_API_BASE` is `pub const GITHUB_API_BASE`. If they were written as private (`fn is_newer`, `const GITHUB_API_BASE`), change them.

**Step 2: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: ALL PASS.

**Step 3: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli --features self-update -- -D warnings`
Expected: No warnings.

**Step 4: Commit (if changes needed)**

```bash
git add ia-core/src/update.rs
git commit -m "fix: make is_newer and GITHUB_API_BASE public for CLI usage"
```

---

### Task 10: E2E Tests

**Files:**
- Modify or create: `ia-cli/tests/update.rs`

**Step 1: Write E2E test for feature gate**

Create `ia-cli/tests/update.rs`:

```rust
use assert_cmd::Command;

/// When built without the self-update feature (the default for tests),
/// `ia update` should not be a recognized subcommand.
#[test]
fn update_command_not_available_without_feature() {
    let mut cmd = Command::cargo_bin("ia").unwrap();
    cmd.arg("update");
    cmd.assert()
        .failure()
        .stderr(predicates::str::contains("unrecognized subcommand"));
}
```

Note: By default, `cargo test` doesn't enable `self-update`, so the update command won't exist. This tests the feature gate.

**Step 2: Run to verify**

Run: `cargo test -p ia-cli -- update_command_not_available`
Expected: PASS.

**Step 3: Commit**

```bash
git add ia-cli/tests/update.rs
git commit -m "test: add E2E test verifying update command requires self-update feature

Confirms that ia update is not a recognized subcommand when built
without the self-update feature flag."
```

---

### Task 11: Update Release Workflow

**Files:**
- Modify: `.github/workflows/release.yml:44`

**Step 1: Add --features self-update to the release build**

In `.github/workflows/release.yml`, change line 44:

From:
```yaml
        run: cargo build -p ia-cli --release --target ${{ matrix.target }}
```

To:
```yaml
        run: cargo build -p ia-cli --release --target ${{ matrix.target }} --features self-update
```

**Step 2: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: enable self-update feature in release builds

Release binaries now include the ia update command. Non-release
builds (cargo install, local dev) don't get it."
```

---

### Task 12: Help Text Verification

**Files:**
- Modify: `ia-cli/src/commands/update.rs` (if needed)

**Step 1: Verify short help**

Run: `cargo run -p ia-cli --features self-update -- update -h`
Expected: Shows terse one-liner about and flags.

**Step 2: Verify long help**

Run: `cargo run -p ia-cli --features self-update -- update --help`
Expected: Shows full description with examples.

**Step 3: Verify update appears in main help**

Run: `cargo run -p ia-cli --features self-update -- --help`
Expected: `update` appears in the subcommand list.

**Step 4: Commit any help text fixes**

If help text needs adjustments, fix and commit.

---

### Task 13: Documentation Updates

**Files:**
- Modify: `CLAUDE.md`
- Modify: `/Users/jake/.claude/projects/-Users-jake-github-jjjake-ia/memory/MEMORY.md`

**Step 1: Update CLAUDE.md**

Add `ia update` to the project documentation. Under the "Crate Stack" or relevant section, note:

- `self-update` feature flag gates `ia update` command (release builds only)
- No new dependencies added

**Step 2: Update MEMORY.md**

Add an entry for the update command under "Key CLI Files" and "Implementation Status".

**Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document ia update command and self-update feature flag"
```

---

### Task 14: Final Verification

**Step 1: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: ALL PASS.

**Step 2: Run tests with self-update feature**

Run: `cargo test -p ia-cli --features self-update`
Expected: ALL PASS.

**Step 3: Run clippy on both configurations**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Run: `cargo clippy -p ia-core -p ia-cli --features self-update -- -D warnings`
Expected: No warnings.

**Step 4: Manual smoke test**

Run: `cargo run -p ia-cli --features self-update -- update --check`
Expected: Shows current version and latest version from GitHub (or "up to date" if current).

Run: `cargo run -p ia-cli --features self-update -- update --check --json`
Expected: JSON output with `current_version`, `latest_version`, `update_available`.
