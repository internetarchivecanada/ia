# Contributing to ia

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.85+
- [GitHub CLI](https://cli.github.com/) (`gh`) — authenticated
- Git 2.20+

## Setup

Optionally, enable the local pre-commit hook to prevent accidental commits to main:

```sh
git config core.hooksPath scripts/hooks
```

This is a convenience — GitHub branch protection is what actually guards main.

## Project Structure

```
ia/
  ia-core/         Library — client, API types, download/upload engines, search
  ia-cli/          Binary — CLI interface, progress display, TUI dashboards
  scripts/         Workflow helpers and git hooks
  docs/plans/      Design docs and implementation plans
```

`ia-core` is also consumed as a library by [ia-gui](https://github.com/jjjake/ia-gui).

## Workflow

Work happens on feature branches — main is protected by GitHub branch protection.

### Plan

For non-trivial work, consider starting with a design doc or implementation plan in `docs/plans/`.

- **Design docs** (`*-design.md`) — architectural decisions, trade-offs, and rationale.
- **Implementation plans** (`*-implementation-plan.md`) — step-by-step breakdown of complex changes.

Small bug fixes and trivial changes don't need docs — use judgment.

### Branch

Create a feature branch however you like. There are helper scripts for git worktrees if you
prefer that workflow:

```sh
scripts/ia-worktree feat my-feature     # creates feat/my-feature branch in a worktree
scripts/ia-worktree fix search-count    # creates fix/search-count branch in a worktree
```

Types: `fix`, `feat`, `refactor`, `docs`, `chore`

After merge, `scripts/ia-cleanup <slug>` removes the worktree and local branch.
`scripts/ia-status` shows active worktrees.

### Implement

- Write tests alongside your code.
- Commit in logical chunks, not one giant commit at the end.
- Reference issue numbers in commits when applicable.
- Update docs/usage.md if your change affects user-visible behavior.
- Before committing:
  ```sh
  cargo check
  cargo test -p ia-core -p ia-cli
  cargo clippy -p ia-core -p ia-cli -- -D warnings
  ```
- If you have [just](https://github.com/casey/just) installed, `just ci` runs all CI checks
  (format, clippy, test, doc) locally in one command.

### Open a PR

```sh
git push -u origin <branch>
gh pr create
```

Link any related issues with `Closes #N` in the PR body.

## Code Conventions

- **Errors**: `thiserror` in `ia-core`, `anyhow` in `ia-cli`. No `unwrap()`/`expect()` outside tests.
- **Tests**: hermetic — zero outbound network requests. Every HTTP call (read or write) must
  go through a `wiremock` mock.
- **Dependencies**: Don't add crates without discussing first. See the crate stack in
  [`AGENTS.md`](./AGENTS.md).
- **CLI help**: Every flag and subcommand needs accurate help text.
- **JSON output**: Every command supports `--json` for structured output.

## AI-Assisted Development

Nearly all implementation in this repository is written by AI agents (primarily
[Claude Code](https://claude.com/claude-code)). The maintainer writes the designs
(`docs/plans/`), files the issues, sets priorities, and tests the tool hands-on against
real archive.org workflows. Code is not reviewed line-by-line; correctness rests on
process — test-first development, hermetic tests (every HTTP call mocked with
`wiremock`; zero traffic to archive.org), and a strict CI gate on every merge.

The full explanation — why this approach, the complete safeguard list, and an honest
record of what has gone wrong and how it was caught — is in
[How this project is built](docs/ai-development.md).

Agent involvement is recorded per-commit via `Co-Authored-By` trailers. Treat this like
any young codebase: if something looks wrong, it might be — please
[open an issue](https://github.com/jjjake/ia/issues).

## Using AI Coding Tools

This repo includes an [`AGENTS.md`](./AGENTS.md) with project instructions for AI coding
assistants — architecture context, safety rules, API quirks, and workflow guidance. Most
agentic tools will read this automatically or can be configured to.

Claude Code users will also find a [`CLAUDE.md`](./CLAUDE.md) with additional tool-specific notes.

## Releasing

Releases are cut with [cargo-release](https://github.com/crate-ci/cargo-release):

    cargo release <version>    # e.g., cargo release 0.9.0

This bumps the workspace version, commits, tags, and pushes. The GitHub Actions
release workflow then builds binaries and creates the GitHub Release.

To preview without making changes:

    cargo release <version> --dry-run

## Safety

These apply to all contributors — human or AI:

- **Tests must be hermetic.** Zero outbound network requests; mock every HTTP call with `wiremock`.
- **Never send write requests to live archive.org** from automated code paths.
- **Never commit secrets.**
- **Never commit to main.** Use a feature branch.

See the full safety rules in [`AGENTS.md`](./AGENTS.md#safety-rules).
