# Update Version Listing & Installation — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Expand `ia update` with `list` and `install` subcommands for version browsing and specific version installation.

**Architecture:** Optional subcommand enum on `UpdateArgs` preserves backward compat. New core functions (`list_releases`, `install_version`) handle GitHub API pagination and per-tag fetching. A hardcoded `MIN_INSTALLABLE_VERSION` floor prevents installing pre-feature versions.

**Tech Stack:** reqwest (existing), serde/serde_json (existing), wiremock (testing), clap subcommands

---

### Task 1: Add `UpdateBelowMinimum` error variant

**Files:**
- Modify: `ia-core/src/error.rs:66-67` (add variant after `UpdateVerifyFailed`)
- Modify: `ia-core/src/error.rs:117-118` (add retry logic)
- Modify: `ia-core/src/error.rs:190-194` (add JSON serialization)

**Step 1: Write the failing test**

Add at end of `mod tests` in `ia-core/src/error.rs` (before the closing `}`):

```rust
    #[test]
    fn update_below_minimum_displays_versions() {
        let err = IaError::UpdateBelowMinimum {
            version: "0.3.0".into(),
            minimum: "0.6.0".into(),
        };
        assert!(err.to_string().contains("0.3.0"));
        assert!(err.to_string().contains("0.6.0"));
    }

    #[test]
    fn json_update_below_minimum() {
        let err = IaError::UpdateBelowMinimum {
            version: "0.3.0".into(),
            minimum: "0.6.0".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_below_minimum");
        assert_eq!(v["error"]["version"], "0.3.0");
        assert_eq!(v["error"]["minimum"], "0.6.0");
    }

    #[test]
    fn update_below_minimum_is_not_retryable() {
        let err = IaError::UpdateBelowMinimum {
            version: "0.3.0".into(),
            minimum: "0.6.0".into(),
        };
        assert!(!err.is_retryable());
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core update_below_minimum`
Expected: FAIL — `UpdateBelowMinimum` variant doesn't exist yet.

**Step 3: Write minimal implementation**

Add to `IaError` enum in `ia-core/src/error.rs` after `UpdateVerifyFailed` (line 67):

```rust
    #[error("version {version} is below minimum installable version ({minimum})")]
    UpdateBelowMinimum { version: String, minimum: String },
```

Add to `is_retryable` match (after `UpdateVerifyFailed` arm, around line 118):

```rust
            IaError::UpdateBelowMinimum { .. } => false,
```

Add to `to_json_error` match (after `UpdateVerifyFailed` arm, around line 194):

```rust
            IaError::UpdateBelowMinimum { version, minimum } => {
                extra.insert("version".into(), version.clone().into());
                extra.insert("minimum".into(), minimum.clone().into());
                "update_below_minimum"
            }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core update_below_minimum`
Expected: 3 tests PASS.

**Step 5: Commit**

```bash
git add ia-core/src/error.rs
git commit -m "feat(ia-core): add UpdateBelowMinimum error variant

For the version floor enforcement in ia update install.
Non-retryable, JSON code: update_below_minimum."
```

---

### Task 2: Add `UpdateVersionNotFound` error variant

**Files:**
- Modify: `ia-core/src/error.rs` (same locations as Task 1, immediately after the new variant)

**Step 1: Write the failing test**

Add at end of `mod tests` in `ia-core/src/error.rs`:

```rust
    #[test]
    fn update_version_not_found_displays_version() {
        let err = IaError::UpdateVersionNotFound {
            version: "99.99.99".into(),
        };
        assert!(err.to_string().contains("99.99.99"));
    }

    #[test]
    fn json_update_version_not_found() {
        let err = IaError::UpdateVersionNotFound {
            version: "99.99.99".into(),
        };
        let v = parse_json_error(&err);
        assert_eq!(v["error"]["code"], "update_version_not_found");
        assert_eq!(v["error"]["version"], "99.99.99");
    }

    #[test]
    fn update_version_not_found_is_not_retryable() {
        let err = IaError::UpdateVersionNotFound {
            version: "99.99.99".into(),
        };
        assert!(!err.is_retryable());
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core update_version_not_found`
Expected: FAIL — variant doesn't exist.

**Step 3: Write minimal implementation**

Add to `IaError` enum after `UpdateBelowMinimum`:

```rust
    #[error("version {version} not found")]
    UpdateVersionNotFound { version: String },
```

Add to `is_retryable`:

```rust
            IaError::UpdateVersionNotFound { .. } => false,
```

Add to `to_json_error`:

```rust
            IaError::UpdateVersionNotFound { version } => {
                extra.insert("version".into(), version.clone().into());
                "update_version_not_found"
            }
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core update_version_not_found`
Expected: 3 tests PASS.

**Step 5: Run full ia-core test suite**

Run: `cargo test -p ia-core`
Expected: All tests pass (existing + 6 new).

**Step 6: Commit**

```bash
git add ia-core/src/error.rs
git commit -m "feat(ia-core): add UpdateVersionNotFound error variant

For when a requested version tag doesn't exist on GitHub.
Non-retryable, JSON code: update_version_not_found."
```

---

### Task 3: Add `ReleaseInfo`, `MIN_INSTALLABLE_VERSION`, and version-comparison helper

**Files:**
- Modify: `ia-core/src/update.rs:1-10` (add imports, types, const)

**Step 1: Write the failing test**

Add in `mod tests` in `ia-core/src/update.rs`:

```rust
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
        // Above minimum
        assert!(is_at_or_above_minimum("1.0.0"));
        assert!(is_at_or_above_minimum("99.0.0"));
        // Equal to minimum
        assert!(is_at_or_above_minimum(MIN_INSTALLABLE_VERSION));
        // Below minimum
        assert!(!is_at_or_above_minimum("0.1.0"));
        assert!(!is_at_or_above_minimum("0.0.1"));
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core release_info_serializes -- && cargo test -p ia-core is_at_or_above_minimum`
Expected: FAIL — types and functions don't exist.

**Step 3: Write minimal implementation**

Add to top of `ia-core/src/update.rs` (after existing imports, before `GitHubRelease`):

```rust
use serde::Serialize;
```

Add after `GITHUB_API_BASE` constant (line 61):

```rust
/// Minimum version that can be installed via `ia update install`.
/// Versions below this lack the update command, so installing them
/// would leave the user unable to update again.
pub const MIN_INSTALLABLE_VERSION: &str = "0.6.0";

/// Summary info about a release for display in `ia update list`.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseInfo {
    pub version: String,
    pub installed: bool,
    pub has_asset: bool,
}

/// Returns true if the given version is at or above `MIN_INSTALLABLE_VERSION`.
pub fn is_at_or_above_minimum(version: &str) -> bool {
    match (parse_version(version), parse_version(MIN_INSTALLABLE_VERSION)) {
        (Some(v), Some(min)) => v >= min,
        _ => false,
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core release_info_serializes is_at_or_above_minimum`
Expected: 2 tests PASS.

**Step 5: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat(ia-core): add ReleaseInfo, MIN_INSTALLABLE_VERSION, and floor check

ReleaseInfo is the display model for ia update list.
MIN_INSTALLABLE_VERSION prevents installing versions that
lack the update command (would strand the user)."
```

---

### Task 4: Implement `list_releases` with pagination

**Files:**
- Modify: `ia-core/src/update.rs` (add function after `is_at_or_above_minimum`)

**Step 1: Write the failing tests**

Add in `mod tests` in `ia-core/src/update.rs`:

```rust
    #[tokio::test]
    async fn list_releases_returns_sorted_versions() {
        let mock_server = wiremock::MockServer::start().await;

        // Page 1 — returns Link header pointing to page 2
        let page2_url = format!("{}/repos/jjjake/ia/releases?page=2", mock_server.uri());
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases"))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {
                            "tag_name": "v99.0.0",
                            "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/1", "size": 100}]
                        },
                        {
                            "tag_name": "v98.0.0",
                            "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/2", "size": 100}]
                        }
                    ]))
                    .insert_header("Link", format!("<{page2_url}>; rel=\"next\"")),
            )
            .mount(&mock_server)
            .await;

        // Page 2 — no Link header (last page)
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases"))
            .and(wiremock::matchers::query_param("page", "2"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {
                            "tag_name": "v97.0.0",
                            "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/3", "size": 100}]
                        }
                    ])),
            )
            .mount(&mock_server)
            .await;

        let releases = list_releases(&mock_server.uri(), "98.0.0", "test-target")
            .await
            .unwrap();

        // Should be sorted oldest → newest
        assert_eq!(releases.len(), 3);
        assert_eq!(releases[0].version, "97.0.0");
        assert_eq!(releases[1].version, "98.0.0");
        assert_eq!(releases[2].version, "99.0.0");

        // Installed flag
        assert!(!releases[0].installed);
        assert!(releases[1].installed);
        assert!(!releases[2].installed);

        // All have matching assets
        assert!(releases.iter().all(|r| r.has_asset));
    }

    #[tokio::test]
    async fn list_releases_filters_below_minimum() {
        let mock_server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases"))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {
                            "tag_name": "v0.1.0",
                            "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/1", "size": 100}]
                        },
                        {
                            "tag_name": "v99.0.0",
                            "assets": [{"name": "ia-test-target", "browser_download_url": "https://example.com/2", "size": 100}]
                        }
                    ])),
            )
            .mount(&mock_server)
            .await;

        let releases = list_releases(&mock_server.uri(), "99.0.0", "test-target")
            .await
            .unwrap();

        // Only v99.0.0 should remain (v0.1.0 is below MIN_INSTALLABLE_VERSION)
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].version, "99.0.0");
    }

    #[tokio::test]
    async fn list_releases_detects_missing_asset() {
        let mock_server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases"))
            .and(wiremock::matchers::query_param("page", "1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([
                        {
                            "tag_name": "v99.0.0",
                            "assets": [{"name": "ia-other-target", "browser_download_url": "https://example.com/1", "size": 100}]
                        }
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
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core list_releases`
Expected: FAIL — function doesn't exist.

**Step 3: Write minimal implementation**

Add after `is_at_or_above_minimum` in `ia-core/src/update.rs`:

```rust
/// Parse the `Link` header to extract the URL for `rel="next"`.
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

/// Fetch all releases from GitHub (paginated), filtered to versions at or
/// above `MIN_INSTALLABLE_VERSION`. Returns sorted oldest → newest.
pub async fn list_releases(
    api_base: &str,
    current_version: &str,
    target: &str,
) -> crate::Result<Vec<ReleaseInfo>> {
    let client = reqwest::Client::new();
    let mut all_releases: Vec<GitHubRelease> = Vec::new();
    let mut url = format!("{api_base}/repos/jjjake/ia/releases?page=1");

    loop {
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

        // Check for next page before consuming the response body
        let next_url = response
            .headers()
            .get("link")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_next_link);

        let page: Vec<GitHubRelease> = response.json().await.map_err(|e| IaError::UpdateApiError {
            status: 0,
            message: format!("failed to parse releases JSON: {e}"),
        })?;

        if page.is_empty() {
            break;
        }

        all_releases.extend(page);

        match next_url {
            Some(next) => url = next,
            None => break,
        }
    }

    // Convert to ReleaseInfo, filter by minimum, sort by semver
    let current = current_version.strip_prefix('v').unwrap_or(current_version);
    let mut infos: Vec<ReleaseInfo> = all_releases
        .into_iter()
        .filter_map(|r| {
            let version = r
                .tag_name
                .strip_prefix('v')
                .unwrap_or(&r.tag_name)
                .to_string();
            if !is_at_or_above_minimum(&version) {
                return None;
            }
            let has_asset = find_matching_asset(&r.assets, target).is_some();
            let installed = version == current;
            Some(ReleaseInfo {
                version,
                installed,
                has_asset,
            })
        })
        .collect();

    infos.sort_by(|a, b| {
        let va = parse_version(&a.version);
        let vb = parse_version(&b.version);
        va.cmp(&vb)
    });

    Ok(infos)
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core list_releases`
Expected: 3 tests PASS.

**Step 5: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat(ia-core): implement list_releases with GitHub pagination

Fetches all releases, follows Link rel=next headers,
filters by MIN_INSTALLABLE_VERSION, sorts oldest→newest.
Marks installed version and platform asset availability."
```

---

### Task 5: Implement `fetch_release_by_tag`

**Files:**
- Modify: `ia-core/src/update.rs` (add function after `list_releases`)

**Step 1: Write the failing tests**

Add in `mod tests`:

```rust
    #[tokio::test]
    async fn fetch_release_by_tag_success() {
        let mock_server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/tags/v1.2.3"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "tag_name": "v1.2.3",
                    "assets": [{
                        "name": "ia-test-target",
                        "browser_download_url": "https://example.com/ia-test",
                        "size": 100
                    }]
                }),
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
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/tags/v99.99.99"))
            .respond_with(wiremock::ResponseTemplate::new(404).set_body_json(
                serde_json::json!({"message": "Not Found"}),
            ))
            .mount(&mock_server)
            .await;

        let result = fetch_release_by_tag("99.99.99", "0.5.0", &mock_server.uri()).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateVersionNotFound { .. }
        ));
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core fetch_release_by_tag`
Expected: FAIL — function doesn't exist.

**Step 3: Write minimal implementation**

Add after `list_releases` in `ia-core/src/update.rs`:

```rust
/// Fetch a single release by its version tag from GitHub.
///
/// The version string should NOT have a `v` prefix (e.g., "1.2.3" not "v1.2.3").
/// Returns `UpdateVersionNotFound` if the tag doesn't exist.
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
    let url = format!("{api_base}/repos/jjjake/ia/releases/tags/{tag}");
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

    response.json().await.map_err(|e| IaError::UpdateApiError {
        status: 0,
        message: format!("failed to parse release JSON: {e}"),
    })
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core fetch_release_by_tag`
Expected: 2 tests PASS.

**Step 5: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat(ia-core): implement fetch_release_by_tag

Fetches a single GitHub release by tag name.
Returns UpdateVersionNotFound on 404."
```

---

### Task 6: Implement `install_version`

**Files:**
- Modify: `ia-core/src/update.rs` (add function after `fetch_release_by_tag`)

**Step 1: Write the failing tests**

Add in `mod tests`:

```rust
    #[tokio::test]
    async fn install_version_below_minimum_returns_error() {
        let result = install_version(
            "0.1.0",
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

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/tags/v99.0.0"))
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

        let result = install_version(
            "99.0.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true, // skip_verify
        )
        .await
        .unwrap();

        assert_eq!(result.new_version, "99.0.0");
        assert_eq!(std::fs::read(&exe_path).unwrap(), fake_binary);
    }

    #[tokio::test]
    async fn install_version_no_matching_asset() {
        let mock_server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/tags/v99.0.0"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({
                    "tag_name": "v99.0.0",
                    "assets": [{
                        "name": "ia-other-target",
                        "browser_download_url": "https://example.com/other",
                        "size": 100
                    }]
                }),
            ))
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old").unwrap();

        let result = install_version(
            "99.0.0",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateNoAsset { .. }
        ));
    }

    #[tokio::test]
    async fn install_version_not_found() {
        let mock_server = wiremock::MockServer::start().await;

        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/repos/jjjake/ia/releases/tags/v99.99.99"))
            .respond_with(wiremock::ResponseTemplate::new(404).set_body_json(
                serde_json::json!({"message": "Not Found"}),
            ))
            .mount(&mock_server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let exe_path = dir.path().join("ia");
        std::fs::write(&exe_path, b"old").unwrap();

        let result = install_version(
            "99.99.99",
            "test-target",
            &exe_path,
            &mock_server.uri(),
            true,
        )
        .await;

        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            crate::error::IaError::UpdateVersionNotFound { .. }
        ));
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p ia-core install_version`
Expected: FAIL — function doesn't exist.

**Step 3: Write minimal implementation**

Add after `fetch_release_by_tag` in `ia-core/src/update.rs`:

```rust
/// Install a specific version by tag.
///
/// Fetches the release, downloads the matching asset, atomically replaces
/// the binary, and verifies. Rejects versions below `MIN_INSTALLABLE_VERSION`.
pub async fn install_version(
    version: &str,
    target: &str,
    current_exe: &Path,
    api_base: &str,
    skip_verify: bool,
) -> crate::Result<UpdateResult> {
    let version_clean = version.strip_prefix('v').unwrap_or(version);
    let current_version = crate::version();

    // Enforce minimum version floor
    if !is_at_or_above_minimum(version_clean) {
        return Err(IaError::UpdateBelowMinimum {
            version: version_clean.to_string(),
            minimum: MIN_INSTALLABLE_VERSION.to_string(),
        });
    }

    let release = fetch_release_by_tag(version_clean, current_version, api_base).await?;

    let asset = find_matching_asset(&release.assets, target).ok_or_else(|| {
        IaError::UpdateNoAsset {
            target: target.to_string(),
        }
    })?;

    // Download to temp file in same directory (for atomic rename)
    let temp_path = current_exe.with_extension("update-tmp");
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
                if !version_output.contains(version_clean) {
                    return Err(IaError::UpdateVerifyFailed {
                        expected: version_clean.to_string(),
                        actual: version_output.trim().to_string(),
                    });
                }
            }
            Ok(out) => {
                return Err(IaError::UpdateVerifyFailed {
                    expected: version_clean.to_string(),
                    actual: format!("exit code {}", out.status),
                });
            }
            Err(e) => {
                return Err(IaError::UpdateVerifyFailed {
                    expected: version_clean.to_string(),
                    actual: format!("failed to run: {e}"),
                });
            }
        }
    }

    Ok(UpdateResult {
        current_version: current_version.to_string(),
        new_version: version_clean.to_string(),
    })
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test -p ia-core install_version`
Expected: 4 tests PASS.

**Step 5: Run full ia-core test suite**

Run: `cargo test -p ia-core`
Expected: All tests pass.

**Step 6: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "feat(ia-core): implement install_version

Installs a specific version by fetching the tagged release,
downloading the platform asset, and atomically replacing the binary.
Enforces MIN_INSTALLABLE_VERSION floor. Verifies post-install."
```

---

### Task 7: Restructure CLI with optional subcommands

**Files:**
- Modify: `ia-cli/src/commands/update.rs` (restructure args, add subcommand enum)

**Step 1: Write the failing test**

This is a refactor — existing behavior must be preserved. The test is the existing integration test.

Run: `cargo test -p ia-cli`
Expected: PASS (baseline).

**Step 2: Restructure `UpdateArgs` with optional subcommand**

Replace the full contents of `ia-cli/src/commands/update.rs`:

```rust
use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use color_print::cstr;
use console::style;

#[derive(Args)]
#[command(
    about = "Update ia to a specific or latest version",
    long_about = "Check for updates, list available versions, or install a specific version of ia.\n\n\
        This command is only available in standalone release builds. If you installed ia via \
        cargo install or a package manager, use that tool to update instead.",
    after_long_help = cstr!(
        "<bold><underline>Examples:</underline></bold>\n\
         \n  <dim># Update to the latest version</dim>\n  <bold>$ ia update</bold>\
         \n\n  <dim># Check for updates without installing</dim>\n  <bold>$ ia update --check</bold>\
         \n\n  <dim># List available versions</dim>\n  <bold>$ ia update list</bold>\
         \n\n  <dim># Install a specific version</dim>\n  <bold>$ ia update install 0.5.1</bold>\
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

    #[command(subcommand)]
    pub subcommand: Option<UpdateSubcommand>,
}

#[derive(Subcommand)]
pub enum UpdateSubcommand {
    /// List available versions
    #[command(
        long_about = "List available versions of ia from GitHub Releases.\n\n\
            Shows the 5 most recent versions by default. Use --all to see all versions \
            above the minimum installable version.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># List recent versions</dim>\n  <bold>$ ia update list</bold>\
             \n\n  <dim># List all versions</dim>\n  <bold>$ ia update list --all</bold>\
             \n\n  <dim># JSON output</dim>\n  <bold>$ ia update list --json</bold>\n"
        ),
    )]
    List(ListArgs),

    /// Install a specific version
    #[command(
        long_about = "Install a specific version of ia by version number.\n\n\
            Downloads the release from GitHub and replaces the current binary. \
            Cannot install versions below the minimum supported version.",
        after_long_help = cstr!(
            "<bold><underline>Examples:</underline></bold>\n\
             \n  <dim># Install a specific version</dim>\n  <bold>$ ia update install 0.5.1</bold>\
             \n\n  <dim># JSON output</dim>\n  <bold>$ ia update install 0.5.1 --json</bold>\n"
        ),
    )]
    Install(InstallArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// Show all versions (not just the 5 most recent)
    #[arg(long)]
    pub all: bool,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct InstallArgs {
    /// Version to install (e.g., 0.5.1)
    pub version: String,

    /// Output results as JSON
    #[arg(long)]
    pub json: bool,
}

pub async fn run(args: UpdateArgs) -> Result<()> {
    match args.subcommand {
        Some(UpdateSubcommand::List(list_args)) => run_list(list_args).await,
        Some(UpdateSubcommand::Install(install_args)) => run_install(install_args).await,
        None => run_default(args).await,
    }
}

/// Original behavior: update to latest or check for updates.
async fn run_default(args: UpdateArgs) -> Result<()> {
    let current_version = ia_core::version();
    let target = env!("IA_TARGET");
    let current_exe =
        std::env::current_exe().context("failed to determine current executable path")?;

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
                || !ia_core::update::is_newer(
                    &update_result.current_version,
                    &update_result.new_version,
                )
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
                        style("\u{2713}").green(),
                        update_result.current_version,
                    );
                }
            } else if args.json {
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
                    style("\u{2713}").green(),
                    style(&update_result.current_version).dim(),
                    style(&update_result.new_version).green().bold(),
                );
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
    let check =
        ia_core::update::check_for_update(current_version, ia_core::update::GITHUB_API_BASE).await;

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
                println!(
                    "Latest version:  {}",
                    style(&info.latest_version).green().bold()
                );
                println!(
                    "\nUpdate available! Run {} to install.",
                    style("ia update").cyan()
                );
            } else {
                println!(
                    "  {} ia {} is already the latest version.",
                    style("\u{2713}").green(),
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

async fn run_list(args: ListArgs) -> Result<()> {
    let current_version = ia_core::version();
    let target = env!("IA_TARGET");

    let releases =
        ia_core::update::list_releases(ia_core::update::GITHUB_API_BASE, current_version, target)
            .await;

    match releases {
        Ok(mut releases) => {
            // Default: show only the 5 most recent
            if !args.all && releases.len() > 5 {
                let start = releases.len() - 5;
                releases = releases.split_off(start);
            }

            if args.json {
                println!("{}", serde_json::to_string(&releases)?);
            } else {
                for release in &releases {
                    if release.installed {
                        println!(
                            "{} {} {}",
                            style("\u{2192}").green(),
                            style(&release.version).green().bold(),
                            style("(installed)").dim(),
                        );
                    } else {
                        println!("  {}", release.version);
                    }
                }
                if releases.is_empty() {
                    println!("No versions available.");
                }
            }
        }
        Err(e) => {
            if args.json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("failed to list versions");
            }
        }
    }

    Ok(())
}

async fn run_install(args: InstallArgs) -> Result<()> {
    let current_version = ia_core::version();
    let target = env!("IA_TARGET");
    let current_exe =
        std::env::current_exe().context("failed to determine current executable path")?;

    if !args.json {
        println!("Installing ia {}...", style(&args.version).bold());
    }

    let result = ia_core::update::install_version(
        &args.version,
        target,
        &current_exe,
        ia_core::update::GITHUB_API_BASE,
        false,
    )
    .await;

    match result {
        Ok(update_result) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "installed",
                        "previous_version": update_result.current_version,
                        "installed_version": update_result.new_version,
                    })
                );
            } else {
                println!(
                    "  {} Installed ia {} (was {})",
                    style("\u{2713}").green(),
                    style(&update_result.new_version).green().bold(),
                    style(&update_result.current_version).dim(),
                );
            }
        }
        Err(e) => {
            if args.json {
                ia_core::write_json_error(&e);
                std::process::exit(1);
            } else {
                return Err(e).context("install failed");
            }
        }
    }

    Ok(())
}
```

**Step 3: Run test to verify existing behavior is preserved**

Run: `cargo test -p ia-cli`
Expected: All tests pass (including the feature-gating integration test).

**Step 4: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: No warnings.

**Step 5: Commit**

```bash
git add ia-cli/src/commands/update.rs
git commit -m "feat(ia-cli): add list and install subcommands to ia update

Restructures UpdateArgs with an optional subcommand enum.
Bare 'ia update' and '--check' behavior preserved.
New: 'ia update list [--all] [--json]' and 'ia update install <version> [--json]'.
Both subcommands support --json for machine-readable output."
```

---

### Task 8: Add integration tests for new subcommands

**Files:**
- Modify: `ia-cli/tests/update.rs`

**Step 1: Add integration tests**

Since the `self-update` feature is disabled in test builds, the new subcommands won't be available for assert_cmd testing. The existing test already covers feature-gating. The substantive behavior is tested via the ia-core unit tests (Tasks 4-6).

Extend the existing integration test to also verify the subcommands are hidden:

```rust
#[cfg(not(feature = "self-update"))]
mod without_feature {
    use assert_cmd::Command;
    use predicates::prelude::*;

    fn ia() -> Command {
        assert_cmd::cargo_bin_cmd!("ia")
    }

    /// When built without the self-update feature (the default for tests),
    /// `ia update` should not be a recognized subcommand.
    #[test]
    fn update_command_not_available_without_feature() {
        ia().arg("update")
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }

    #[test]
    fn update_list_not_available_without_feature() {
        ia().args(["update", "list"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }

    #[test]
    fn update_install_not_available_without_feature() {
        ia().args(["update", "install", "0.5.0"])
            .assert()
            .failure()
            .stderr(predicate::str::contains("unrecognized subcommand"));
    }
}
```

**Step 2: Run test to verify it passes**

Run: `cargo test -p ia-cli update`
Expected: 3 tests PASS.

**Step 3: Commit**

```bash
git add ia-cli/tests/update.rs
git commit -m "test(ia-cli): add integration tests for update subcommands

Verify list and install subcommands are also gated behind
the self-update feature flag."
```

---

### Task 9: Add `parse_next_link` unit test

**Files:**
- Modify: `ia-core/src/update.rs` (add test in `mod tests`)

**Step 1: Write the test**

Add in `mod tests`:

```rust
    #[test]
    fn parse_next_link_extracts_url() {
        let header = r#"<https://api.github.com/repos/jjjake/ia/releases?page=2>; rel="next", <https://api.github.com/repos/jjjake/ia/releases?page=5>; rel="last""#;
        let next = parse_next_link(header);
        assert_eq!(
            next,
            Some("https://api.github.com/repos/jjjake/ia/releases?page=2".to_string())
        );
    }

    #[test]
    fn parse_next_link_returns_none_when_no_next() {
        let header = r#"<https://api.github.com/repos/jjjake/ia/releases?page=1>; rel="prev", <https://api.github.com/repos/jjjake/ia/releases?page=5>; rel="last""#;
        let next = parse_next_link(header);
        assert!(next.is_none());
    }

    #[test]
    fn parse_next_link_empty_string() {
        assert!(parse_next_link("").is_none());
    }
```

**Step 2: Run test to verify it passes**

Run: `cargo test -p ia-core parse_next_link`
Expected: 3 tests PASS (implementation already exists from Task 4).

**Step 3: Commit**

```bash
git add ia-core/src/update.rs
git commit -m "test(ia-core): add unit tests for parse_next_link

Cover happy path, missing rel=next, and empty input."
```

---

### Task 10: Final verification and cleanup

**Step 1: Run full test suite**

Run: `cargo test -p ia-core -p ia-cli`
Expected: All tests pass.

**Step 2: Run clippy**

Run: `cargo clippy -p ia-core -p ia-cli -- -D warnings`
Expected: Zero warnings.

**Step 3: Verify cargo check**

Run: `cargo check -p ia-core -p ia-cli`
Expected: Success.

**Step 4: Commit design and plan docs (if not already committed)**

```bash
git add docs/plans/2026-03-03-update-versions-design.md docs/plans/2026-03-03-update-versions-implementation-plan.md
git commit -m "docs: add design and implementation plan for update versions feature

Design doc: docs/plans/2026-03-03-update-versions-design.md
Implementation plan: docs/plans/2026-03-03-update-versions-implementation-plan.md"
```

---

## Summary

| Task | What | Files |
|------|------|-------|
| 1 | `UpdateBelowMinimum` error variant | `ia-core/src/error.rs` |
| 2 | `UpdateVersionNotFound` error variant | `ia-core/src/error.rs` |
| 3 | `ReleaseInfo`, `MIN_INSTALLABLE_VERSION`, `is_at_or_above_minimum` | `ia-core/src/update.rs` |
| 4 | `list_releases` with pagination | `ia-core/src/update.rs` |
| 5 | `fetch_release_by_tag` | `ia-core/src/update.rs` |
| 6 | `install_version` | `ia-core/src/update.rs` |
| 7 | CLI restructure with subcommands | `ia-cli/src/commands/update.rs` |
| 8 | Integration tests | `ia-cli/tests/update.rs` |
| 9 | `parse_next_link` tests | `ia-core/src/update.rs` |
| 10 | Final verification | All |
