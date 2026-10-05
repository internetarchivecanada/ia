# Release builds restore a cache that main saves

**Goal (Jake, 2026-10-05):** a release takes about three minutes from tag to published binaries instead of sixteen to thirty.

**Why it was slow.** The v0.21.0 run (37374489401) took 21 min 43 s. Two causes, both in `release.yml`:

1. Every release build was a cold build of 274 crates: all three Build jobs logged "No cache found." GitHub scopes an Actions cache to the ref that wrote it plus the default branch, and a tag's run never reads another tag's cache. The caches on main are no use either: `Swatinem/rust-cache` keys a cache as `v0-rust-<shared-key or key+job>-<OS>-<arch>-<env hash>-<lockfile hash>`, so ci.yml's debug builds live under `v0-rust-test-Linux-x64-…` while the release builds asked for `v0-rust-<target>-build-…`. The cold build steps took 3 min 08 s (musl), 4 min 46 s (Windows) and 1 min 46 s (macOS).
2. The build job had `needs: [check, test]`, so no binary was built until the five CI jobs that had already run on main for the same commit finished again: 14 minutes in that run (the Windows test job queued 5 minutes and ran 9).

**Change.**

- `.github/workflows/build.yml`, new: a called workflow (`on: workflow_call`) holding the build matrix and its steps, with `Swatinem/rust-cache@v2` under `shared-key: release-<target>` and `save-if: github.ref == 'refs/heads/main'`. An input `upload-artifacts` (default true) lets the warm-up skip the artifact upload; nothing reads artifacts from a main push, and each would be kept 90 days.
- `.github/workflows/ci.yml`: a job `release-build` that `uses:` build.yml with `if: github.ref == 'refs/heads/main'` and `upload-artifacts: false`. Every push to main (a PR merge or a release-version commit) saves the three release caches. It is not a required check on main (those are Test, Clippy, Format, Doc, Audit) and is skipped on pull requests.
- `.github/workflows/release.yml`: the build job becomes `uses: ./.github/workflows/build.yml` with no `needs`. The check and test jobs stay for the record; the release job keeps `needs: build`.
- `CONTRIBUTING.md` "Releasing": says where the cache comes from and what makes a build cold again.

**One definition, not two copies.** The cache key hashes the job's rustc version, every `CARGO*`/`CC*`/`CXX*`/`RUST*` environment variable, and the manifests and lockfile. Two hand-kept copies of the matrix could drift (one gains an env var or a feature flag) and miss the cache with no error to show for it. A called workflow runs the same steps from both callers, so the keys match by construction. GitHub supports a matrix inside a called workflow, `if:` on the calling job, and artifacts uploaded by the called jobs belong to the caller's run, so `needs: build` and `download-artifact` in the release job work unchanged. The called workflow does not inherit the caller's `env`, so it sets `CARGO_TERM_COLOR` itself (the value is part of the env hash).

**What keeps the key stable.** rust-cache zeroes `package.version` before hashing a member manifest and drops workspace crates from the lockfile hash, and the workspace version lives in the root manifest, which is not a member. So a release-version commit hits the exact key saved by the previous push to main. A toolchain update (`dtolnay/rust-toolchain@stable` moves with stable; the rustc version is in the key) or a dependency change misses the exact key, falls back to the newest cache under the same prefix, and the next push to main saves a fresh one.

**Why `save-if` on main only.** A cache saved under a tag ref is never read again, and the post-step save cost 7 to 21 s per job in the v0.21.0 run plus 280 MB of cache storage per target.

**Not changed:** the check and test jobs in release.yml (they still run on every tag); ci.yml's five debug jobs and their keys; the release job's steps.

## Tasks
- [x] Plan doc; build.yml; ci.yml job; release.yml edits; CONTRIBUTING.md.
- [x] actionlint on the three workflows; `just ci`.
- [ ] Dispatch release.yml on the branch: proves the call wiring and gives cold timings with no cache written (the branch is not main).
- [ ] code-reviewer pass; PR; `gh pr update-branch`, `gh pr checks --watch --fail-fast`, `gh pr merge --squash`; `scripts/ia-cleanup release-cache` after `gh pr view --json state` says MERGED.
- [ ] After the merge: the main push's `Release build` jobs save the caches; dispatch release.yml on main and read the Build jobs for a restored cache and the build step's time. Record before/after below.

## Timings

Before (v0.21.0, tag 21:13:06 UTC to release 21:34:49 UTC): 21 min 43 s. Build jobs started at 21:27:16 after the Windows test job; build steps 3 min 08 s / 4 min 46 s / 1 min 46 s; the macOS build job waited 5 min for a runner.

After: recorded when measured.
