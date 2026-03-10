# Contributing to ia

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.85+
- [GitHub CLI](https://cli.github.com/) (`gh`) — authenticated
- Git 2.20+

## Setup

After cloning, enable the commit hook that prevents direct commits to main:

```sh
git config core.hooksPath scripts/hooks
```

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

All work happens on feature branches in git worktrees. Main is protected — a pre-commit hook
rejects direct commits, and branch protection blocks pushes.

### 1. Plan

For non-trivial work, start with a design doc or implementation plan in `docs/plans/`. This is
where you think through the approach before writing code. Commit it to your feature branch as the
first commit.

- **Design docs** (`*-design.md`) — architectural decisions, trade-offs, and rationale. These are
  durable reference material.
- **Implementation plans** (`*-implementation-plan.md`) — step-by-step breakdown of complex
  changes. Useful during development and for debugging later.

Small bug fixes and trivial changes don't need docs — use judgment.

### 2. Create a Worktree

```sh
scripts/ia-worktree feat my-feature     # creates feat/my-feature branch
scripts/ia-worktree fix search-count    # creates fix/search-count branch
```

Types: `fix`, `feat`, `refactor`, `docs`, `chore`

This creates a worktree at `../worktrees/<slug>` with a branch `<type>/<slug>` from main.
Then `cd` into it and start working.

### 3. Implement

- Write tests alongside your code.
- Commit in logical chunks, not one giant commit at the end.
- Reference issue numbers in commits when applicable.
- Before committing:
  ```sh
  cargo check
  cargo test -p ia-core -p ia-cli
  cargo clippy -p ia-core -p ia-cli -- -D warnings
  ```

### 4. Open a PR

```sh
git push -u origin <branch>
gh pr create
```

Link any related issues with `Closes #N` in the PR body.

### 5. After Merge

From outside the worktree (e.g., the main checkout):

```sh
scripts/ia-cleanup <slug>
```

This removes the worktree, deletes the local branch, and prunes stale references.

To see all active worktrees:

```sh
scripts/ia-status
```

## Code Conventions

- **Errors**: `thiserror` in `ia-core`, `anyhow` in `ia-cli`. No `unwrap()`/`expect()` outside tests.
- **Tests**: `wiremock` for HTTP mocks. Write-operation tests must always use mocks — never hit
  live archive.org.
- **Dependencies**: Don't add crates without discussing first. See the crate stack in
  [`AGENTS.md`](./AGENTS.md).
- **CLI help**: Every flag and subcommand needs accurate help text.
- **JSON output**: Every command supports `--json` for structured output.

## Using AI Coding Tools

This repo includes an [`AGENTS.md`](./AGENTS.md) with project instructions for AI coding
assistants — architecture context, safety rules, API quirks, and workflow guidance. Most
agentic tools will read this automatically or can be configured to.

Claude Code users will also find a [`CLAUDE.md`](./CLAUDE.md) with additional tool-specific notes.

## Safety

These apply to all contributors — human or AI:

- **Never send write requests to live archive.org** in automated tests. Use mocks.
- **Never commit secrets.**
- **Never commit to main.** Use a worktree.

See the full safety rules in [`AGENTS.md`](./AGENTS.md#safety-rules).
