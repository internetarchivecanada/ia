# Self-Updater Design (`ia update`)

**Date:** 2026-03-02
**Status:** Approved

## Overview

Add an `ia update` subcommand that checks for new releases on GitHub and replaces
the running binary in-place. Only available in standalone release builds (gated by
a compile-time feature flag). No new dependencies.

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Detection method | Compile-time feature flag (`self-update`) | Clean: command doesn't exist in non-release builds |
| Command name | `ia update` | Simple, familiar |
| Auto-check | No | Explicit only — no surprise network requests |
| Platform identification | Compile-time target triple | Embedded via build script, matches release asset names |
| Approach | Hand-rolled with existing deps | Zero new dependencies, full UX control, ~300 lines |

## Feature Gate

A cargo feature `self-update` controls whether `ia update` exists:

- **`ia-cli/Cargo.toml`**: `self-update = []` (no extra deps, pure gate)
- **Release workflow** (`.github/workflows/release.yml`): Build with `--features self-update`
- **`ia-cli/build.rs`**: Emit `cargo:rustc-env=IA_TARGET={target}` for platform identification
- **Command registration**: `#[cfg(feature = "self-update")] Update(UpdateArgs)` in `Commands` enum
- When feature is disabled, `ia update` simply doesn't appear as a subcommand

## Version Check

1. `GET https://api.github.com/repos/jjjake/ia/releases/latest`
   - Headers: `Accept: application/vnd.github+json`, `User-Agent: ia/{version}`
   - No authentication needed (60 req/hr unauthenticated rate limit is sufficient)
2. Parse `tag_name` (e.g. `"v0.4.4"`), strip `v` prefix, parse as semver
3. Compare with `env!("CARGO_PKG_VERSION")`
4. If latest > current → update available; if equal → up to date

### `--check` flag

Print version info without downloading:

```
Current version: 0.4.3
Latest version:  0.4.4
Update available! Run `ia update` to install.
```

### `--json` support

```json
{"current_version": "0.4.3", "latest_version": "0.4.4", "update_available": true}
```

## Download & Replace

When an update is available and user runs `ia update` (without `--check`):

1. **Find asset** — Match `ia-{IA_TARGET}` in the release's `assets[]` array
2. **Download** — Stream from `browser_download_url` to a temp file in the same
   directory as the current binary (ensures same filesystem for atomic rename).
   Show progress bar via indicatif.
3. **Set permissions** — On Unix, copy permissions from the original binary to the
   new file (`chmod +x` at minimum)
4. **Atomic replace** — `std::fs::rename(temp_file, current_exe())`
   - Unix: atomic (single syscall)
   - Windows: rename current to `ia.old.exe`, rename new to `ia.exe`
5. **Verify** — Run the new binary with `--version`, confirm output matches
   expected version. If verification fails, roll back (restore old binary).
6. **Cleanup** — Remove backup/temp files

### Output

```
Updated ia from 0.4.3 to 0.4.4
```

JSON mode:
```json
{"current_version": "0.4.3", "new_version": "0.4.4", "status": "updated"}
```

## Error Handling

| Error | Code | Context |
|---|---|---|
| No matching asset for platform | `update_no_asset` | `target` field |
| Download failure | `update_download_failed` | `url`, `status` fields |
| Permission denied (can't write) | `update_permission_denied` | `path` field |
| GitHub API error | `update_api_error` | `status` field |
| Verification failed (rollback) | `update_verify_failed` | `version` field |

All errors follow the standard `{"error": {"code": "...", "message": "...", ...}}` format.

New `IaError` variants:
- `UpdateNoAsset { target: String }`
- `UpdateDownloadFailed { url: String, source: ... }`
- `UpdatePermissionDenied { path: PathBuf, source: ... }`
- `UpdateApiError { status: u16, message: String }`
- `UpdateVerifyFailed { expected: String, actual: String }`

## Architecture

| Component | Location | Purpose |
|---|---|---|
| Update module | `ia-core/src/update.rs` | Version check, asset matching, download, replace |
| CLI command | `ia-cli/src/commands/update.rs` | Args (`--check`, `--json`), command handler |
| Build script | `ia-cli/build.rs` | Emit `IA_TARGET` env var |
| Feature flag | `ia-cli/Cargo.toml` | `self-update = []` |
| Release workflow | `.github/workflows/release.yml` | Add `--features self-update` |
| Error variants | `ia-core/src/error.rs` | Update-specific error types |

### Data Flow

```
ia update
  → fetch GitHub API (latest release)
  → compare versions (semver)
  → find matching asset (by compile-time target triple)
  → download to temp file (with progress bar)
  → set permissions (preserve original)
  → atomic rename (replace binary)
  → verify new binary (--version check)
  → print success / rollback on failure
```

## Testing Strategy

### Unit Tests (ia-core)

- Version parsing and semver comparison (edge cases: `0.4.3` vs `0.4.10`, pre-release)
- Asset name matching (`find_matching_asset` with various asset lists)
- GitHub API response deserialization (mock JSON payloads)

### Integration Tests (wiremock)

- Mock API returning newer version → correct asset selected
- Mock API returning same version → "up to date" output
- Mock API returning no matching asset → error with `update_no_asset` code
- Mock API returning 404/500 → error handling
- `--check` flag → no download attempt
- `--json` flag → structured output validation

### E2E Tests (assert_cmd)

- Build without `self-update` feature → `ia update` is not a recognized subcommand
- Build with `self-update` feature → `ia update --check` works against mock

### Manual Testing (PR checklist)

- Test on macOS with a real GitHub release
- Verify rollback (corrupt downloaded binary, confirm restoration)
- Verify permission preservation

## Rust Port Benefit

Self-update is a genuine advantage of the Rust port. Python's `ia` requires
`pip install --upgrade internetarchive` (depends on pip, Python, virtualenvs)
or pex binary rebuilds. Rust's `ia` is a single statically-linked binary that
can replace itself in-place — `ia update` and done. No package manager, no
interpreter, no virtual environment.

This should be documented in `docs/why-rust.md` (when created per the README
redesign plan) under the "Single binary distribution" benefit.

## No New Dependencies

Everything uses existing crates:
- `reqwest` — HTTP client (GitHub API + asset download)
- `serde` / `serde_json` — JSON deserialization
- `indicatif` — Progress bar during download
- `std::fs` — Atomic file replacement
- `std::process::Command` — Verification (`--version` check)
