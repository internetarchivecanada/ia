use crate::error::IaError;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::AsyncWriteExt;

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
pub fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// Find the release asset matching the given target triple.
/// Looks for `ia-{target}` or `ia-{target}.exe`.
pub fn find_matching_asset<'a>(assets: &'a [GitHubAsset], target: &str) -> Option<&'a GitHubAsset> {
    let name = format!("ia-{target}");
    let name_exe = format!("ia-{target}.exe");
    assets.iter().find(|a| a.name == name || a.name == name_exe)
}

pub const GITHUB_API_BASE: &str = "https://api.github.com";

/// Serializable summary of a single GitHub release for `ia update list`.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseInfo {
    pub version: String,
    pub installed: bool,
    pub has_asset: bool,
}

/// Oldest version that ships the `update` command and can therefore be
/// installed via `ia update install`. Versions below this lack
/// self-update support, so installing them would strand the user.
pub const MIN_INSTALLABLE_VERSION: &str = "0.6.0";

/// Returns `true` if `version` is at or above [`MIN_INSTALLABLE_VERSION`].
///
/// Returns `false` for unparseable version strings.
pub fn is_at_or_above_minimum(version: &str) -> bool {
    match (parse_version(version), parse_version(MIN_INSTALLABLE_VERSION)) {
        (Some(v), Some(min)) => v >= min,
        _ => false,
    }
}

/// Check GitHub Releases for a newer version.
///
/// `api_base` allows overriding the GitHub API URL for testing (pass wiremock URL).
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

    let latest_version = release
        .tag_name
        .strip_prefix('v')
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
        let _ = std::fs::remove_file(&backup);
        std::fs::rename(target, &backup)?;
        if let Err(e) = std::fs::rename(source, target) {
            let _ = std::fs::rename(&backup, target);
            return Err(e.into());
        }
        let _ = std::fs::remove_file(&backup);
    }

    Ok(())
}

/// Result of a successful update.
#[derive(Debug)]
pub struct UpdateResult {
    pub current_version: String,
    pub new_version: String,
}

/// Perform the full update: check -> download -> replace.
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
    let _ = std::fs::remove_file(&temp_path);

    if let Err(e) = download_asset(&asset.browser_download_url, &temp_path, current_version).await
    {
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
        let assets = vec![GitHubAsset {
            name: "ia-x86_64-pc-windows-msvc.exe".into(),
            browser_download_url: "https://example.com/ia-x86_64-pc-windows-msvc.exe".into(),
            size: 300,
        }];
        let result = find_matching_asset(&assets, "x86_64-pc-windows-msvc");
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "ia-x86_64-pc-windows-msvc.exe");
    }

    #[test]
    fn find_asset_returns_none_when_missing() {
        let assets = vec![GitHubAsset {
            name: "ia-x86_64-unknown-linux-musl".into(),
            browser_download_url: "https://example.com/ia-x86_64-unknown-linux-musl".into(),
            size: 200,
        }];
        let result = find_matching_asset(&assets, "aarch64-apple-darwin");
        assert!(result.is_none());
    }

    #[test]
    fn find_asset_empty_list() {
        let assets: Vec<GitHubAsset> = vec![];
        let result = find_matching_asset(&assets, "aarch64-apple-darwin");
        assert!(result.is_none());
    }

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
        assert!(matches!(
            err,
            crate::error::IaError::UpdateApiError { status: 403, .. }
        ));
    }

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
        assert!(!replacement.exists());
    }

    #[tokio::test]
    async fn perform_update_full_flow() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-binary-content";

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
            true, // skip_verify -- can't run --version on fake binary
        )
        .await
        .unwrap();

        assert_eq!(result.new_version, "99.0.0");
        assert_eq!(std::fs::read(&exe_path).unwrap(), fake_binary);
    }

    #[test]
    fn release_info_serializes_to_json() {
        let info = ReleaseInfo {
            version: "0.5.0".into(),
            installed: true,
            has_asset: true,
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["version"], "0.5.0");
        assert_eq!(json["installed"], true);
        assert_eq!(json["has_asset"], true);
    }

    #[test]
    fn is_at_or_above_minimum_filters_correctly() {
        assert!(is_at_or_above_minimum("1.0.0"));
        assert!(is_at_or_above_minimum("99.0.0"));
        assert!(is_at_or_above_minimum(MIN_INSTALLABLE_VERSION));
        assert!(!is_at_or_above_minimum("0.1.0"));
        assert!(!is_at_or_above_minimum("0.0.1"));
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

        let result = perform_update(
            "0.4.3",
            "aarch64-apple-darwin",
            &exe_path,
            &mock_server.uri(),
            false,
        )
        .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateNoAsset { .. }
        ));
    }
}
