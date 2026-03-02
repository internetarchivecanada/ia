use crate::error::IaError;
use serde::Deserialize;
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
}
