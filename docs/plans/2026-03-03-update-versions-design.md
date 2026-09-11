# Design: Version Listing and Installation for `ia update`

**Date:** 2026-03-03
**Status:** Approved

## Overview

Expand the `ia update` command to support listing available versions and installing specific versions. This turns `ia update` from a simple "install latest" tool into a lightweight version manager with rollback capability.

## Motivation

Users need to:
- **Roll back** after a bad update breaks something
- **Install a specific version** for testing or pinning in scripts
- **Browse available versions** to understand what's available

## CLI Interface

### Existing Behavior (Unchanged)

```
ia update              # install latest version
ia update --check      # check if update available
ia update --json       # JSON output for either mode
```

### New Subcommands

```
ia update list                   # show 5 most recent versions (above floor)
ia update list --all             # show all versions above floor
ia update list --json            # JSON array of versions
ia update install <version>      # install specific version
ia update install <version> --json  # JSON output
```

### Output Examples

```
$ ia update list
  0.4.4
  0.5.0
  0.5.1
→ 0.5.2 (installed)
  0.5.3

$ ia update list --all
  0.4.0
  0.4.1
  ...
→ 0.5.2 (installed)
  0.5.3

$ ia update install 0.4.0
error: version 0.4.0 is below the minimum supported version (0.6.0)

$ ia update install 0.5.1
✓ Installed ia 0.5.1 (was 0.5.2)

$ ia update list --json
[
  {"version": "0.4.4", "installed": false, "has_asset": true},
  {"version": "0.5.0", "installed": false, "has_asset": true},
  {"version": "0.5.1", "installed": false, "has_asset": true},
  {"version": "0.5.2", "installed": true, "has_asset": true},
  {"version": "0.5.3", "installed": false, "has_asset": true}
]
```

## Minimum Version Floor

A hardcoded constant prevents installing versions older than the first release that ships this feature:

```rust
pub const MIN_INSTALLABLE_VERSION: &str = "0.6.0";  // first release with version management
```

Without this, a user could install an old binary that lacks `ia update`, leaving them stuck. Enforced in both `list_releases` (filters output) and `install_version` (rejects with error).

## Architecture

### Approach: Optional Subcommands (Backward Compatible)

`UpdateArgs` gains an optional subcommand enum. When no subcommand is given, existing behavior is preserved (`--check` flag determines mode). This follows the sub-subcommand pattern established by `ia config`.

### Core API (ia-core/src/update.rs)

**New types:**

```rust
pub struct ReleaseInfo {
    pub version: String,
    pub installed: bool,
    pub has_asset: bool,
}
```

**New constant:**

```rust
pub const MIN_INSTALLABLE_VERSION: &str = "0.6.0";
```

**New functions:**

```rust
/// Fetch all releases from GitHub (paginated), filtered to versions at or above
/// MIN_INSTALLABLE_VERSION. Returns sorted oldest → newest.
pub async fn list_releases(
    api_base: &str,
    current_version: &str,
    target: &str,
) -> Result<Vec<ReleaseInfo>>

/// Install a specific version. Fetches the release by tag, downloads the matching
/// asset, atomically replaces the binary, and verifies. Rejects versions below floor.
pub async fn install_version(
    version: &str,
    target: &str,
    current_exe: &Path,
    api_base: &str,
    skip_verify: bool,
) -> Result<UpdateResult>
```

### GitHub API Usage

- **List:** `GET /repos/internetarchivecanada/ia/releases` — paginated (30/page), follow `Link: rel="next"` header. Parse all pages, filter by floor, sort by semver.
- **Install:** `GET /repos/internetarchivecanada/ia/releases/tags/v{version}` — single release by tag. Avoids fetching all releases to find one. 404 → `UpdateVersionNotFound`.
- **Rate limits:** 60 req/hr unauthenticated. Listing takes 2-3 requests, installing takes 2. Well within limits.

### New Error Variants (ia-core/src/error.rs)

```rust
#[error("version {version} is below minimum installable version ({minimum})")]
UpdateBelowMinimum { version: String, minimum: String },

#[error("version {version} not found")]
UpdateVersionNotFound { version: String },
```

Both non-retryable. JSON codes: `"update_below_minimum"`, `"update_version_not_found"`.

### CLI Changes (ia-cli/src/commands/update.rs)

Optional subcommand enum:

```rust
#[derive(Subcommand)]
enum UpdateSubcommand {
    List(ListArgs),
    Install(InstallArgs),
}

struct ListArgs {
    #[arg(long)]
    all: bool,
    #[arg(long)]
    json: bool,
}

struct InstallArgs {
    version: String,
    #[arg(long)]
    json: bool,
}
```

When no subcommand is given, fall through to existing `--check` / install-latest logic.

## Changes by File

| File | Changes |
|------|---------|
| `ia-core/src/update.rs` | `ReleaseInfo`, `MIN_INSTALLABLE_VERSION`, `list_releases()`, `install_version()`, pagination helper, unit tests |
| `ia-core/src/error.rs` | `UpdateBelowMinimum`, `UpdateVersionNotFound` variants + JSON codes + retry logic |
| `ia-cli/src/commands/update.rs` | Optional subcommand enum, `ListArgs`, `InstallArgs`, output formatting |
| `ia-cli/tests/update.rs` | Integration tests for new subcommands |

**No new dependencies.** No changes to build script, feature flags, or main.rs routing.

## Testing Strategy

### Unit Tests (ia-core, wiremock)

- `list_releases` with mocked paginated responses (2 pages)
- `list_releases` filters out versions below `MIN_INSTALLABLE_VERSION`
- `list_releases` marks installed version correctly
- `install_version` with mocked single-release endpoint → download → replace (skip_verify)
- `install_version` below minimum floor → `UpdateBelowMinimum` error
- `install_version` with nonexistent tag → `UpdateVersionNotFound` error
- `install_version` with no matching platform asset → existing `UpdateNoAsset` error

### Integration Tests (ia-cli, assert_cmd)

- `ia update list` output format (arrow marker, "(installed)" label)
- `ia update list --json` output structure
- `ia update list --all` shows more than default 5
- `ia update install` below floor → error message
- Feature-gating: subcommands unavailable without `self-update` feature
