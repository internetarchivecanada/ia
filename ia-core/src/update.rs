use crate::error::IaError;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, DefaultRetryableStrategy};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// The GitHub repository self-update queries for releases.
///
/// Single source of truth: the three API call sites below and every test
/// fixture build their paths from this, so moving the repo is a one-line
/// change here rather than a find-and-replace across the file.
pub const GITHUB_REPO: &str = "internetarchivecanada/ia";

/// Build a configured HTTP client for GitHub API requests.
///
/// Includes the crate's retry middleware: three retries on a 5xx, a 429,
/// a 408 or a connection failure, waiting a random time up to a cap that
/// doubles from 1 s to 30 s, or what a `Retry-After` header says.
fn github_client(current_version: &str) -> crate::Result<ClientWithMiddleware> {
    let raw = crate::client::configure_transport(
        reqwest::Client::builder(),
        crate::client::CONNECT_TIMEOUT,
        Some(crate::client::READ_TIMEOUT),
    )
    .user_agent(format!("ia/{current_version}"))
    .build()
    .map_err(|e| IaError::Config(format!("failed to build HTTP client: {e}")))?;

    let retry_policy = ExponentialBackoff::builder()
        .retry_bounds(
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(30),
        )
        .build_with_max_retries(3);

    Ok(ClientBuilder::new(raw)
        .with(crate::retry::RetryMiddleware::new(
            retry_policy,
            DefaultRetryableStrategy,
        ))
        .build())
}

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

/// First release whose assets come with a `SHA256SUMS` file. From this
/// version on, a release without the file, or without a line for this
/// platform's asset, refuses to install; older releases were published
/// without it and install with a warning.
pub const MIN_CHECKSUMMED_VERSION: &str = "0.23.0";

/// Name of the checksum file attached to each release.
pub const SHA256SUMS_ASSET: &str = "SHA256SUMS";

/// Returns `true` if `version` is at or above [`MIN_INSTALLABLE_VERSION`].
///
/// Returns `false` for unparseable version strings.
pub fn is_at_or_above_minimum(version: &str) -> bool {
    match (
        parse_version(version),
        parse_version(MIN_INSTALLABLE_VERSION),
    ) {
        (Some(v), Some(min)) => v >= min,
        _ => false,
    }
}

/// Parse the GitHub `Link` header to extract the `rel="next"` URL.
///
/// GitHub uses RFC 5988 `Link` headers for pagination, e.g.:
/// `<https://api.github.com/repos/…?page=2>; rel="next", <…>; rel="last"`
fn parse_next_link(link_header: &str) -> Option<String> {
    for part in link_header.split(',') {
        let part = part.trim();
        if part.contains("rel=\"next\"") {
            if let Some(url) = part.split(';').next() {
                let url = url.trim().trim_start_matches('<').trim_end_matches('>');
                return Some(url.to_string());
            }
        }
    }
    None
}

/// Fetch all GitHub releases, filtered to [`MIN_INSTALLABLE_VERSION`] and above,
/// sorted oldest-to-newest by semver.
///
/// Each release is converted to a [`ReleaseInfo`] with:
/// - `installed`: `true` if the version matches `current_version`
/// - `has_asset`: `true` if a matching binary asset exists for `target`
///
/// `api_base` allows overriding the GitHub API URL for testing (pass wiremock URL).
pub async fn list_releases(
    api_base: &str,
    current_version: &str,
    target: &str,
) -> crate::Result<Vec<ReleaseInfo>> {
    let client = github_client(current_version)?;
    let mut url = format!("{api_base}/repos/{GITHUB_REPO}/releases?page=1");
    let mut all_releases: Vec<GitHubRelease> = Vec::new();

    loop {
        let response = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
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

        // Extract the Link header before consuming the response body.
        let next_link = response
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_next_link);

        let page: Vec<GitHubRelease> =
            response.json().await.map_err(|e| IaError::UpdateApiError {
                status: 0,
                message: format!("failed to parse releases JSON: {e}"),
            })?;

        if page.is_empty() {
            break;
        }

        all_releases.extend(page);

        match next_link {
            Some(next) => url = next,
            None => break,
        }
    }

    let mut results: Vec<ReleaseInfo> = all_releases
        .iter()
        .filter_map(|release| {
            let version = release
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&release.tag_name)
                .to_string();

            if !is_at_or_above_minimum(&version) {
                return None;
            }

            let has_asset = find_matching_asset(&release.assets, target).is_some();
            let installed = version == current_version;

            Some(ReleaseInfo {
                version,
                installed,
                has_asset,
            })
        })
        .collect();

    // Sort oldest-to-newest by semver.
    results.sort_by(|a, b| {
        let va = parse_version(&a.version).unwrap_or((0, 0, 0));
        let vb = parse_version(&b.version).unwrap_or((0, 0, 0));
        va.cmp(&vb)
    });

    Ok(results)
}

/// Fetch a single GitHub release by its version tag.
///
/// Adds a `v` prefix if not already present (e.g. `"1.2.3"` -> `"v1.2.3"`).
///
/// Returns `IaError::UpdateVersionNotFound` on 404, `IaError::UpdateApiError`
/// on other HTTP errors.
pub async fn fetch_release_by_tag(
    version: &str,
    current_version: &str,
    api_base: &str,
) -> crate::Result<GitHubRelease> {
    let tag = if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    };

    let url = format!("{api_base}/repos/{GITHUB_REPO}/releases/tags/{tag}");
    let client = github_client(current_version)?;
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| IaError::UpdateApiError {
            status: 0,
            message: e.to_string(),
        })?;

    if response.status().as_u16() == 404 {
        return Err(IaError::UpdateVersionNotFound {
            version: version.to_string(),
        });
    }

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

    Ok(release)
}

/// Check GitHub Releases for a newer version.
///
/// `api_base` allows overriding the GitHub API URL for testing (pass wiremock URL).
pub async fn check_for_update(current_version: &str, api_base: &str) -> crate::Result<UpdateCheck> {
    let url = format!("{api_base}/repos/{GITHUB_REPO}/releases/latest");
    let client = github_client(current_version)?;
    let response = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
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
        release: if update_available {
            Some(release)
        } else {
            None
        },
    })
}

/// Download a release asset to a local file path without a checksum check.
///
/// Kept for library callers; `ia update` itself goes through
/// [`download_asset_checked`].
pub async fn download_asset(url: &str, dest: &Path, current_version: &str) -> crate::Result<()> {
    download_asset_checked(url, dest, current_version, None).await
}

/// The SHA-256 a release's `SHA256SUMS` lists for one asset.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExpectedSha256 {
    pub asset: String,
    /// 64 hex digits, any case.
    pub hex: String,
}

impl ExpectedSha256 {
    /// An expectation that `asset` hashes to `hex`.
    pub fn new(asset: impl Into<String>, hex: impl Into<String>) -> Self {
        Self {
            asset: asset.into(),
            hex: hex.into(),
        }
    }
}

/// Download a release asset to `dest`, refusing it if its SHA-256 does not
/// match `expected`.
///
/// The body is hashed in memory before anything is written, so a bad
/// download never reaches disk. `None` skips the check.
pub async fn download_asset_checked(
    url: &str,
    dest: &Path,
    current_version: &str,
    expected: Option<&ExpectedSha256>,
) -> crate::Result<()> {
    let bytes = fetch_bytes(url, current_version, "download").await?;

    if let Some(expected) = expected {
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(&expected.hex) {
            return Err(IaError::UpdateChecksumMismatch {
                asset: expected.asset.clone(),
                expected: expected.hex.to_ascii_lowercase(),
                actual,
            });
        }
    }

    let mut file = tokio::fs::File::create(dest).await?;
    file.write_all(&bytes).await?;
    file.flush().await?;

    Ok(())
}

/// GET `url` through the GitHub client and return the body. `what` names
/// the request in error messages.
async fn fetch_bytes(url: &str, current_version: &str, what: &str) -> crate::Result<bytes::Bytes> {
    let client = github_client(current_version)?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| IaError::UpdateApiError {
            status: 0,
            message: format!("{what} failed: {e}"),
        })?;

    if !response.status().is_success() {
        return Err(IaError::UpdateApiError {
            status: response.status().as_u16(),
            message: format!("{what} returned {}", response.status()),
        });
    }

    response.bytes().await.map_err(|e| IaError::UpdateApiError {
        status: 0,
        message: format!("failed to read {what} body: {e}"),
    })
}

/// Lowercase hex SHA-256 of `data`.
fn sha256_hex(data: &[u8]) -> String {
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Parse the `sha256sum` output format: one `<hex>  <name>` line per file.
///
/// Accepts the two-space separator and the ` *` binary-mode marker, trims
/// surrounding whitespace (so CRLF files parse), and skips blank lines,
/// lines without both parts, and lines whose digest is not 64 hex digits,
/// so a corrupt file reads as "no entry" rather than as a mismatch against
/// garbage. Returns `(hex, name)` pairs in file order.
pub fn parse_sha256sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (hex, name) = line.trim().split_once(' ')?;
            let name = name.trim_start_matches([' ', '*']);
            let is_sha256 = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
            if !is_sha256 || name.is_empty() {
                return None;
            }
            Some((hex.to_string(), name.to_string()))
        })
        .collect()
}

/// The SHA-256 the release's `SHA256SUMS` lists for `asset_name`.
///
/// `Ok(None)` for a release older than [`MIN_CHECKSUMMED_VERSION`] that
/// has no `SHA256SUMS`: it was published without one, and a warning is
/// logged. A release that has the file is always checked against it.
/// From [`MIN_CHECKSUMMED_VERSION`] on, a missing file or a missing line
/// is [`IaError::UpdateChecksumMissing`].
pub async fn expected_checksum_for(
    release: &GitHubRelease,
    asset_name: &str,
    current_version: &str,
) -> crate::Result<Option<ExpectedSha256>> {
    let version = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name)
        .to_string();
    // An unparseable tag is treated as new: fail closed.
    let must_have_sums = match (
        parse_version(&version),
        parse_version(MIN_CHECKSUMMED_VERSION),
    ) {
        (Some(v), Some(min)) => v >= min,
        _ => true,
    };
    let missing = || IaError::UpdateChecksumMissing {
        version: version.clone(),
        asset: asset_name.to_string(),
    };

    let Some(sums_asset) = release.assets.iter().find(|a| a.name == SHA256SUMS_ASSET) else {
        if must_have_sums {
            return Err(missing());
        }
        tracing::warn!(
            "release v{version} predates SHA256SUMS; installing without checksum verification"
        );
        return Ok(None);
    };

    let body = fetch_bytes(
        &sums_asset.browser_download_url,
        current_version,
        "SHA256SUMS download",
    )
    .await?;
    let text = String::from_utf8_lossy(&body);
    match parse_sha256sums(&text)
        .into_iter()
        .find(|(_, name)| name == asset_name)
    {
        Some((hex, _)) => Ok(Some(ExpectedSha256::new(asset_name, hex))),
        None => Err(missing()),
    }
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

/// Download a release asset, atomically replace the binary, and optionally
/// verify the new binary by running `--version`.
///
/// This is the shared implementation for both `install_version` and `perform_update`.
async fn download_and_replace(
    release: &GitHubRelease,
    asset: &GitHubAsset,
    current_exe: &Path,
    current_version: &str,
    expected_version: &str,
    skip_verify: bool,
) -> crate::Result<()> {
    // Look the checksum up before downloading: a release that should have
    // one and does not is refused without fetching the asset at all.
    let expected = expected_checksum_for(release, &asset.name, current_version).await?;

    let temp_path = current_exe.with_extension("update-tmp");
    let _ = std::fs::remove_file(&temp_path);

    if let Err(e) = download_asset_checked(
        &asset.browser_download_url,
        &temp_path,
        current_version,
        expected.as_ref(),
    )
    .await
    {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    if let Err(e) = replace_binary(&temp_path, current_exe) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(e);
    }

    if !skip_verify {
        let output = tokio::process::Command::new(current_exe)
            .arg("--version")
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                let version_output = String::from_utf8_lossy(&out.stdout);
                if !version_output.contains(expected_version) {
                    return Err(IaError::UpdateVerifyFailed {
                        expected: expected_version.to_string(),
                        actual: version_output.trim().to_string(),
                    });
                }
            }
            Ok(out) => {
                return Err(IaError::UpdateVerifyFailed {
                    expected: expected_version.to_string(),
                    actual: format!("exit code {}", out.status),
                });
            }
            Err(e) => {
                return Err(IaError::UpdateVerifyFailed {
                    expected: expected_version.to_string(),
                    actual: format!("failed to run: {e}"),
                });
            }
        }
    }

    Ok(())
}

/// Install a specific version by tag.
///
/// Downloads and replaces the current binary with the specified version.
/// Returns `IaError::UpdateBelowMinimum` if the version is below [`MIN_INSTALLABLE_VERSION`],
/// `IaError::UpdateVersionNotFound` if the release doesn't exist on GitHub,
/// `IaError::UpdateNoAsset` if there's no matching asset for the target platform.
///
/// If `skip_verify` is false, runs `current_exe --version` after replacing to verify the install.
pub async fn install_version(
    version: &str,
    current_version: &str,
    target: &str,
    current_exe: &Path,
    api_base: &str,
    skip_verify: bool,
) -> crate::Result<UpdateResult> {
    let clean_version = version.strip_prefix('v').unwrap_or(version);

    if !is_at_or_above_minimum(clean_version) {
        return Err(IaError::UpdateBelowMinimum {
            version: clean_version.to_string(),
            minimum: MIN_INSTALLABLE_VERSION.to_string(),
        });
    }

    let release = fetch_release_by_tag(clean_version, current_version, api_base).await?;

    let asset =
        find_matching_asset(&release.assets, target).ok_or_else(|| IaError::UpdateNoAsset {
            target: target.to_string(),
        })?;

    download_and_replace(
        &release,
        asset,
        current_exe,
        current_version,
        clean_version,
        skip_verify,
    )
    .await?;

    Ok(UpdateResult {
        current_version: current_version.to_string(),
        new_version: clean_version.to_string(),
    })
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
    let asset =
        find_matching_asset(&release.assets, target).ok_or_else(|| IaError::UpdateNoAsset {
            target: target.to_string(),
        })?;

    download_and_replace(
        release,
        asset,
        current_exe,
        current_version,
        &check.latest_version,
        skip_verify,
    )
    .await?;

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
                    "browser_download_url": "https://github.com/internetarchivecanada/ia/releases/download/v0.4.4/ia-aarch64-apple-darwin",
                    "size": 12345678
                },
                {
                    "name": "ia-x86_64-unknown-linux-musl",
                    "browser_download_url": "https://github.com/internetarchivecanada/ia/releases/download/v0.4.4/ia-x86_64-unknown-linux-musl",
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
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/latest"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v99.0.0",
                    "assets": [{
                        "name": "ia-aarch64-apple-darwin",
                        "browser_download_url": "https://example.com/ia-aarch64-apple-darwin",
                        "size": 100
                    }]
                })),
            )
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
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/latest"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v0.4.3",
                    "assets": []
                })),
            )
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
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/latest"
            )))
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
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/latest"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(release_json(
                    &mock_server.uri(),
                    "v99.0.0",
                    fake_binary.len(),
                    true,
                )),
            )
            .mount(&mock_server)
            .await;

        mount_asset(&mock_server, "/download/ia-test-target", fake_binary).await;
        mount_asset(
            &mock_server,
            "/download/SHA256SUMS",
            format!("{NEW_BINARY_CONTENT_SHA256}  ia-test-target\n").as_bytes(),
        )
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

    #[test]
    fn parse_next_link_extracts_next_url() {
        let header = r#"<https://api.github.com/repos/internetarchivecanada/ia/releases?page=2>; rel="next", <https://api.github.com/repos/internetarchivecanada/ia/releases?page=5>; rel="last""#;
        assert_eq!(
            parse_next_link(header),
            Some(
                "https://api.github.com/repos/internetarchivecanada/ia/releases?page=2".to_string()
            )
        );
    }

    #[test]
    fn parse_next_link_returns_none_without_next() {
        let header = r#"<https://api.github.com/repos/internetarchivecanada/ia/releases?page=1>; rel="prev", <https://api.github.com/repos/internetarchivecanada/ia/releases?page=5>; rel="last""#;
        assert_eq!(parse_next_link(header), None);
    }

    #[test]
    fn parse_next_link_returns_none_for_empty() {
        assert_eq!(parse_next_link(""), None);
    }

    #[tokio::test]
    async fn list_releases_returns_sorted_versions() {
        let mock_server = wiremock::MockServer::start().await;
        let page2_url = format!("{}/repos/{GITHUB_REPO}/releases?page=2", mock_server.uri());
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!("/repos/{GITHUB_REPO}/releases")))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {"tag_name": "v99.0.0", "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/1", "size": 100}]},
                        {"tag_name": "v98.0.0", "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/2", "size": 100}]}
                    ]))
                    .insert_header("Link", format!("<{page2_url}>; rel=\"next\"")),
            )
            .mount(&mock_server)
            .await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!("/repos/{GITHUB_REPO}/releases")))
            .and(wiremock::matchers::query_param("page", "2"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {"tag_name": "v97.0.0", "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/3", "size": 100}]}
                    ])),
            )
            .mount(&mock_server)
            .await;

        let releases = list_releases(&mock_server.uri(), "98.0.0", "test-target")
            .await
            .unwrap();
        assert_eq!(releases.len(), 3);
        assert_eq!(releases[0].version, "97.0.0");
        assert_eq!(releases[1].version, "98.0.0");
        assert_eq!(releases[2].version, "99.0.0");
        assert!(!releases[0].installed);
        assert!(releases[1].installed);
        assert!(!releases[2].installed);
        assert!(releases.iter().all(|r| r.has_asset));
    }

    #[tokio::test]
    async fn list_releases_filters_below_minimum() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!("/repos/{GITHUB_REPO}/releases")))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {"tag_name": "v0.1.0", "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/1", "size": 100}]},
                        {"tag_name": "v99.0.0", "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/2", "size": 100}]}
                    ])),
            )
            .mount(&mock_server)
            .await;

        let releases = list_releases(&mock_server.uri(), "99.0.0", "test-target")
            .await
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].version, "99.0.0");
    }

    #[tokio::test]
    async fn list_releases_detects_missing_asset() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!("/repos/{GITHUB_REPO}/releases")))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {"tag_name": "v99.0.0", "assets": [{"name": "ia-other-target", "browser_download_url": "https://example.com/1", "size": 100}]}
                    ])),
            )
            .mount(&mock_server)
            .await;

        let releases = list_releases(&mock_server.uri(), "99.0.0", "test-target")
            .await
            .unwrap();
        assert_eq!(releases.len(), 1);
        assert!(!releases[0].has_asset);
    }

    #[tokio::test]
    async fn fetch_release_by_tag_success() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!("/repos/{GITHUB_REPO}/releases/tags/v1.2.3")))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"tag_name": "v1.2.3", "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/ia-test", "size": 100}]}),
            ))
            .mount(&mock_server)
            .await;

        let release = fetch_release_by_tag("1.2.3", "0.5.0", &mock_server.uri())
            .await
            .unwrap();
        assert_eq!(release.tag_name, "v1.2.3");
        assert_eq!(release.assets.len(), 1);
    }

    #[tokio::test]
    async fn fetch_release_by_tag_not_found() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/tags/v99.99.99"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({"message": "Not Found"})),
            )
            .mount(&mock_server)
            .await;

        let result = fetch_release_by_tag("99.99.99", "0.5.0", &mock_server.uri()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateVersionNotFound { .. }
        ));
    }

    #[tokio::test]
    async fn perform_update_no_matching_asset() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/latest"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v99.0.0",
                    "assets": [{
                        "name": "ia-some-other-target",
                        "browser_download_url": "https://example.com/ia-other",
                        "size": 100
                    }]
                })),
            )
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

    #[tokio::test]
    async fn install_version_below_minimum_returns_error() {
        let result = install_version(
            "0.1.0",
            "0.5.0",
            "test-target",
            Path::new("/tmp/fake"),
            "https://unused.example.com",
            true,
        )
        .await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateBelowMinimum { .. }
        ));
    }

    #[tokio::test]
    async fn install_version_full_flow() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";

        mount_release_by_tag(&mock_server, "v99.0.0", fake_binary.len(), true).await;
        mount_asset(&mock_server, "/download/ia-test-target", fake_binary).await;
        mount_asset(
            &mock_server,
            "/download/SHA256SUMS",
            format!("{NEW_VERSION_BINARY_SHA256}  ia-test-target\n").as_bytes(),
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let result = install_version(
            "99.0.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap();
        assert_eq!(result.new_version, "99.0.0");
        assert_eq!(result.current_version, "0.5.0");
        assert_eq!(std::fs::read(&exe_path).unwrap(), fake_binary);
    }

    // SHA-256 of the fixtures above, computed with `shasum -a 256`.
    const NEW_BINARY_CONTENT_SHA256: &str =
        "14e726e382bdd84e4915710951640438c2960ce6eec4acdf0fdb0958ad5b7035";
    const NEW_VERSION_BINARY_SHA256: &str =
        "314a972e8bec759671a3a2585ecad004202b803fc64a32394d950f8936517f7d";

    /// A release JSON body with the `ia-test-target` asset and, when
    /// `with_sums` is set, a `SHA256SUMS` asset, both served by `base`.
    fn release_json(base: &str, tag: &str, size: usize, with_sums: bool) -> serde_json::Value {
        let mut assets = vec![serde_json::json!({
            "name": "ia-test-target",
            "browser_download_url": format!("{base}/download/ia-test-target"),
            "size": size
        })];
        if with_sums {
            assets.push(serde_json::json!({
                "name": "SHA256SUMS",
                "browser_download_url": format!("{base}/download/SHA256SUMS"),
                "size": 0
            }));
        }
        serde_json::json!({ "tag_name": tag, "assets": assets })
    }

    async fn mount_release_by_tag(
        server: &wiremock::MockServer,
        tag: &str,
        size: usize,
        with_sums: bool,
    ) {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/tags/{tag}"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(release_json(
                    &server.uri(),
                    tag,
                    size,
                    with_sums,
                )),
            )
            .mount(server)
            .await;
    }

    async fn mount_asset(server: &wiremock::MockServer, path: &str, body: &[u8]) {
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(path))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
            .mount(server)
            .await;
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parse_sha256sums_two_space_and_asterisk_forms() {
        let a = NEW_BINARY_CONTENT_SHA256;
        let b = NEW_VERSION_BINARY_SHA256;
        let c = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let text = format!(
            "{a}  ia-x86_64-unknown-linux-musl\n{b} *ia-aarch64-apple-darwin\n\n  {c}  ia-cli-man.tar.gz  \n"
        );
        assert_eq!(
            parse_sha256sums(&text),
            vec![
                (a.to_string(), "ia-x86_64-unknown-linux-musl".to_string()),
                (b.to_string(), "ia-aarch64-apple-darwin".to_string()),
                (c.to_string(), "ia-cli-man.tar.gz".to_string()),
            ]
        );
    }

    #[test]
    fn parse_sha256sums_skips_lines_without_both_parts() {
        assert_eq!(
            parse_sha256sums(&format!("{NEW_BINARY_CONTENT_SHA256}\n   \n")),
            Vec::new()
        );
    }

    #[test]
    fn parse_sha256sums_skips_a_digest_that_is_not_64_hex_digits() {
        let text = format!(
            "notahash  ia-x\n{NEW_BINARY_CONTENT_SHA256}0  ia-y\nzz{}  ia-z\n",
            &NEW_BINARY_CONTENT_SHA256[2..]
        );
        assert_eq!(parse_sha256sums(&text), Vec::new());
    }

    #[test]
    fn parse_sha256sums_handles_crlf_line_endings() {
        let text =
            format!("{NEW_BINARY_CONTENT_SHA256}  ia-x\r\n{NEW_VERSION_BINARY_SHA256}  ia-y\r\n");
        assert_eq!(
            parse_sha256sums(&text),
            vec![
                (NEW_BINARY_CONTENT_SHA256.to_string(), "ia-x".to_string()),
                (NEW_VERSION_BINARY_SHA256.to_string(), "ia-y".to_string()),
            ]
        );
    }

    #[test]
    fn parse_sha256sums_keeps_spaces_inside_a_name() {
        let text = format!("{NEW_BINARY_CONTENT_SHA256}  ia cli man.tar.gz\n");
        assert_eq!(
            parse_sha256sums(&text),
            vec![(
                NEW_BINARY_CONTENT_SHA256.to_string(),
                "ia cli man.tar.gz".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn expected_checksum_for_fails_closed_on_an_unparseable_tag() {
        // No assets, so nothing is fetched: the decision alone is under test.
        let release = GitHubRelease {
            tag_name: "nightly".into(),
            assets: vec![],
        };
        let err = expected_checksum_for(&release, "ia-test-target", "0.5.0")
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                IaError::UpdateChecksumMissing { ref version, ref asset }
                    if version == "nightly" && asset == "ia-test-target"
            ),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn install_refuses_a_mismatching_checksum_and_leaves_the_binary_alone() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";
        mount_release_by_tag(&mock_server, "v99.0.0", fake_binary.len(), true).await;
        mount_asset(&mock_server, "/download/ia-test-target", fake_binary).await;
        mount_asset(
            &mock_server,
            "/download/SHA256SUMS",
            format!("{NEW_BINARY_CONTENT_SHA256}  ia-test-target\n").as_bytes(),
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let err = install_version(
            "99.0.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap_err();

        match err {
            IaError::UpdateChecksumMismatch {
                asset,
                expected,
                actual,
            } => {
                assert_eq!(asset, "ia-test-target");
                assert_eq!(expected, NEW_BINARY_CONTENT_SHA256);
                assert_eq!(actual, NEW_VERSION_BINARY_SHA256);
            }
            other => panic!("expected UpdateChecksumMismatch, got {other:?}"),
        }
        assert_eq!(std::fs::read(&exe_path).unwrap(), b"old-binary");
        assert!(!exe_path.with_extension("update-tmp").exists());
    }

    #[tokio::test]
    async fn install_accepts_an_uppercase_checksum() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";
        mount_release_by_tag(&mock_server, "v99.0.0", fake_binary.len(), true).await;
        mount_asset(&mock_server, "/download/ia-test-target", fake_binary).await;
        mount_asset(
            &mock_server,
            "/download/SHA256SUMS",
            format!(
                "{}  ia-test-target\n",
                NEW_VERSION_BINARY_SHA256.to_ascii_uppercase()
            )
            .as_bytes(),
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        install_version(
            "99.0.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&exe_path).unwrap(), fake_binary);
    }

    #[tokio::test]
    async fn install_refuses_a_new_release_without_sha256sums() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";
        mount_release_by_tag(&mock_server, "v99.0.0", fake_binary.len(), false).await;
        // No asset mock: the install must stop before downloading.

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let err = install_version(
            "99.0.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(
                err,
                IaError::UpdateChecksumMissing { ref version, ref asset }
                    if version == "99.0.0" && asset == "ia-test-target"
            ),
            "got {err:?}"
        );
        assert_eq!(std::fs::read(&exe_path).unwrap(), b"old-binary");
    }

    #[tokio::test]
    async fn install_refuses_a_new_release_whose_sha256sums_lacks_this_asset() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";
        mount_release_by_tag(&mock_server, "v99.0.0", fake_binary.len(), true).await;
        mount_asset(
            &mock_server,
            "/download/SHA256SUMS",
            format!("{NEW_VERSION_BINARY_SHA256}  ia-other-target\n").as_bytes(),
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let err = install_version(
            "99.0.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, IaError::UpdateChecksumMissing { .. }),
            "got {err:?}"
        );
        assert_eq!(std::fs::read(&exe_path).unwrap(), b"old-binary");
    }

    #[tokio::test]
    async fn install_accepts_a_pre_sums_release_without_sha256sums() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";
        // 0.7.0 is installable (>= 0.6.0) and predates SHA256SUMS (< 0.23.0).
        mount_release_by_tag(&mock_server, "v0.7.0", fake_binary.len(), false).await;
        mount_asset(&mock_server, "/download/ia-test-target", fake_binary).await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let result = install_version(
            "0.7.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap();
        assert_eq!(result.new_version, "0.7.0");
        assert_eq!(std::fs::read(&exe_path).unwrap(), fake_binary);
    }

    #[tokio::test]
    async fn install_checks_a_pre_sums_release_that_does_have_sha256sums() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";
        mount_release_by_tag(&mock_server, "v0.7.0", fake_binary.len(), true).await;
        mount_asset(&mock_server, "/download/ia-test-target", fake_binary).await;
        mount_asset(
            &mock_server,
            "/download/SHA256SUMS",
            format!("{NEW_BINARY_CONTENT_SHA256}  ia-test-target\n").as_bytes(),
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let err = install_version(
            "0.7.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, IaError::UpdateChecksumMismatch { .. }),
            "got {err:?}"
        );
        assert_eq!(std::fs::read(&exe_path).unwrap(), b"old-binary");
    }

    #[tokio::test]
    async fn install_refuses_a_pre_sums_release_whose_sha256sums_lacks_this_asset() {
        let mock_server = wiremock::MockServer::start().await;
        let fake_binary = b"new-version-binary";
        mount_release_by_tag(&mock_server, "v0.7.0", fake_binary.len(), true).await;
        mount_asset(
            &mock_server,
            "/download/SHA256SUMS",
            format!("{NEW_VERSION_BINARY_SHA256}  ia-other-target\n").as_bytes(),
        )
        .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old-binary").unwrap();

        let err = install_version(
            "0.7.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, IaError::UpdateChecksumMissing { .. }),
            "got {err:?}"
        );
        assert_eq!(std::fs::read(&exe_path).unwrap(), b"old-binary");
    }

    #[tokio::test]
    async fn install_version_no_matching_asset() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/tags/v99.0.0"
            )))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"tag_name": "v99.0.0", "assets": [{"name": "ia-other-target", "browser_download_url": "https://example.com/other", "size": 100}]}),
            ))
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old").unwrap();

        let result = install_version(
            "99.0.0",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await;
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateNoAsset { .. }
        ));
    }

    #[tokio::test]
    async fn install_version_not_found() {
        let mock_server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path(&format!(
                "/repos/{GITHUB_REPO}/releases/tags/v99.99.99"
            )))
            .respond_with(
                wiremock::ResponseTemplate::new(404)
                    .set_body_json(serde_json::json!({"message": "Not Found"})),
            )
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old").unwrap();

        let result = install_version(
            "99.99.99",
            "0.5.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await;
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateVersionNotFound { .. }
        ));
    }
}
