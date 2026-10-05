# Release builds restore a cache that main saves

**Goal (Jake, 2026-10-05):** a release takes about three minutes from tag to published binaries instead of sixteen to thirty.

**Why it was slow.** The v0.21.0 run (37374489401) took 21 min 43 s. Two causes, both in `release.yml`:

1. Every release build was a cold build of 274 crates: all three Build jobs logged "No cache found." GitHub scopes an Actions cache to the ref that wrote it plus the default branch, and a tag's run never reads another tag's cache. The caches on main are no use either: `Swatinem/rust-cache` keys a cache as `v0-rust-<shared-key or key+job>-<OS>-<arch>-<env hash>-<lockfile hash>`, so ci.yml's debug builds live under `v0-rust-test-Linux-x64-…` while the release builds asked for `v0-rust-<target>-build-…`. The cold build steps took 3 min 08 s (musl), 4 min 46 s (Windows) and 1 min 46 s (macOS).
2. The build job had `needs: [check, test]`, so no binary was built until release.yml's four check and test jobs finished: 14 minutes in that run (the Windows test job queued 5 minutes and ran 9). Only the Linux ones repeat work main does: ci.yml tests on ubuntu-latest alone, so the macOS and Windows test jobs in release.yml are the only place those tests run. Main's run is not a gate on tagging today either: the main run for 37092d8 (37373273178) concluded failure, Format cancelled in that evening's runner outage, and v0.21.0 shipped from it.

**Change.**

- `.github/workflows/build.yml`, new: a called workflow (`on: workflow_call`) holding the build matrix and its steps, with `Swatinem/rust-cache@v2` under `shared-key: release-<target>` and `save-if: github.ref == 'refs/heads/main'`. An input `upload-artifacts` (default true) lets the warm-up skip the artifact upload; nothing reads artifacts from a main push, and each would be kept 90 days.
- `.github/workflows/ci.yml`: a job `release-build` that `uses:` build.yml with `if: github.ref == 'refs/heads/main'` and `upload-artifacts: false`. Every push to main (a PR merge or a release-version commit) saves the three release caches. It is not a required check on main (those are Test, Clippy, Format, Doc, Audit) and is skipped on pull requests.
- `.github/workflows/release.yml`: the build job becomes `uses: ./.github/workflows/build.yml` with no `needs`. The check and test jobs stay for the record with `save-if: false` on their caches (a cache saved under a tag ref is never read again); the release job keeps `needs: build`. **Consequence, Jake's call (2026-10-05):** a failing macOS or Windows test no longer stops a release, since those tests run nowhere else. The alternatives are `needs: [build, check, test]` on the release job (builds start at once, publishing waits the 9 to 14 minutes for the Windows test job, which defeats the goal) or adding macOS and Windows to ci.yml's Test job (runner time on every PR; macOS runners queue). Left as instructed: the jobs run for the record.
- `CONTRIBUTING.md` "Releasing": says where the cache comes from and what makes a build cold again.

**One definition, not two copies.** The cache key hashes the job's rustc version, every `CARGO*`/`CC*`/`CXX*`/`RUST*` environment variable, and the manifests and lockfile. Two hand-kept copies of the matrix could drift (one gains an env var or a feature flag) and miss the cache with no error to show for it. A called workflow runs the same steps from both callers, so the keys match by construction. GitHub supports a matrix inside a called workflow, `if:` on the calling job, and artifacts uploaded by the called jobs belong to the caller's run, so `needs: build` and `download-artifact` in the release job work unchanged. The called workflow does not inherit the caller's `env`, so it sets `CARGO_TERM_COLOR` itself (the value is part of the env hash).

**What keeps the key stable.** rust-cache zeroes `package.version` before hashing a member manifest (and zeroes the version and path of path dependencies such as ia-cli's on ia-core), drops workspace crates from the lockfile hash, and never hashes the virtual root manifest, where the workspace version lives. So a release-version commit hits the exact key kept by the previous push to main. A dependency change misses the exact key but matches the restore key (everything up to the env hash), restores the newest cache under it and rebuilds only what changed. A toolchain update (`dtolnay/rust-toolchain@stable` moves with stable) changes the env hash, which is part of the restore key, so the build is fully cold; the same holds for a runner-image update that changes a preinstalled toolchain, since rust-cache hashes every installed toolchain's version. GitHub evicts a cache not accessed for seven days, and by last access once the repository passes 10 GB (6.4 GB in 22 caches today; the three release caches add about 0.84 GB), so a release after a quiet week builds cold. Most pushes to main only restore the cache (rust-cache skips the save on an exact hit, "Cache up-to-date"), which still resets the seven-day clock. A tag pushed before the commit's main run has finished its Release build jobs restores the previous main cache, fine unless that commit changed dependencies or the toolchain; CONTRIBUTING.md says to let the main run finish first.

**Why `save-if` on main only.** A cache saved under a tag ref is never read again, and the post-step save cost 7 to 21 s per job in the v0.21.0 run plus 280 MB of cache storage per target.

**Not changed:** ci.yml's five debug jobs and their keys; the release job's steps. Out of scope, noted for Jake: CONTRIBUTING.md says cargo-release "commits, tags, and pushes", but main accepts only pull requests and v0.21.0 was tagged on a merged PR commit.

**Runner contention:** without `needs`, a tag run starts Test (macos-latest) and Build (aarch64-apple-darwin) at once, so both queue for macOS runners; the first post-merge timing may still show a queue.

## Tasks
- [x] Plan doc; build.yml; ci.yml job; release.yml edits; CONTRIBUTING.md.
- [x] actionlint on the three workflows; `just ci` (green).
- [x] Dispatch release.yml on the branch (37377561458): the three Build jobs started 6 s after the run, in parallel with the checks.
- [x] code-reviewer pass (above). PR #54; `gh pr update-branch`, `gh pr checks --watch --fail-fast`, `gh pr merge --squash`; `scripts/ia-cleanup release-cache` after `gh pr view --json state` says MERGED.
- [ ] After the merge: the main push's `Release build` jobs save the caches; dispatch release.yml on main and read the Build jobs for a restored cache and the build step's time. Record before/after below.

## Review (2026-10-05), closed before merge

Important, taken as a correction of the record: macOS and Windows tests run only in release.yml, so removing the gate means a failing one no longer stops a release; the comments, CONTRIBUTING.md and this plan claimed the checks "already passed on main", which is false for those two and was false for 37092d8's main run. Wording corrected everywhere; the gate stays removed as instructed, and the alternatives are recorded above for Jake.

Suggestions, taken: the fallback claim had the two cases backwards (a toolchain update misses the restore key and builds fully cold; a dependency change restores partially); the 7-day and 10 GB eviction rules; "keeps warm" rather than "saves"; let the main run finish before tagging; `save-if: false` on release.yml's check and test jobs.

Verified correct by the reviewer: both callers produce the same key (dtolnay/rust-toolchain writes CARGO_HOME, CARGO_INCREMENTAL and CARGO_TERM_COLOR to GITHUB_ENV inside the called job; the musl-tools install runs after rust-cache); the version-zeroing; the boolean `inputs.upload-artifacts` step condition; artifacts from called jobs reaching the release job; `github.ref` in `save-if` is the caller's; the keywords on the calling job; Release build is not a required check.

## Timings

Before (v0.21.0, tag 21:13:06 UTC to release 21:34:49 UTC): 21 min 43 s. Build jobs started at 21:27:16 after the Windows test job; build steps 3 min 08 s / 4 min 46 s / 1 min 46 s; the macOS build job waited 5 min for a runner.

After: recorded when measured.
