# Ralph Agent Instructions — ia Metadata Write

You are an autonomous coding agent implementing metadata write support for the `ia` Rust CLI (Internet Archive command-line tool).

## ABSOLUTE SAFETY RULES

These are NON-NEGOTIABLE:

1. **NEVER send real HTTP requests to archive.org** — ALL tests use wiremock mocks
2. **NEVER commit secrets, credentials, or .env files**
3. **ALWAYS run `gh auth switch --user jjjake` before any `gh` command or `git push`**
4. **NEVER push to any repo other than `jjjake/ia`**

## Scope

You are ONLY working on metadata write issues (#109-#125, parent #85). Do NOT touch any other issues or features.

## Your Task

1. Read the PRD at `scripts/ralph/prd.json`
2. Read the progress log at `scripts/ralph/progress.txt` (check **Codebase Patterns** section first)
3. **Check for review feedback** at `scripts/ralph/review-feedback.md` — if it exists and contains "FIXME", fix those issues FIRST before doing anything else. After fixing, run quality checks, commit with `refactor: address review feedback for [Story ID]`, and delete the file.
4. Check you're on the `feat/metadata-write` branch. If not, check it out or create from main.
5. Pick the **highest priority** user story where `passes: false`
6. Read the implementation plan at `docs/plans/2026-02-22-metadata-write-implementation-plan.md` for the corresponding task
7. Read the design doc at `docs/plans/2026-02-22-metadata-write-design.md` for context
8. Implement that single user story using TDD:
   a. Write the failing test(s) first
   b. Run tests to verify they fail
   c. Write minimal implementation to pass the tests
   d. Run tests to verify they pass
9. Run quality checks: `cargo test --workspace && cargo clippy --workspace -- -D warnings`
10. If checks pass, commit ALL changes with message: `feat: [Story ID] - [Story Title]`
11. Update the PRD to set `passes: true` for the completed story
12. Close the GitHub issue for the completed story (see below)
13. Append your progress to `scripts/ralph/progress.txt`

## Fixing Review Feedback (Step 3)

If `scripts/ralph/review-feedback.md` exists and contains "FIXME":
- Read the file carefully — it has specific issues with file paths and fixes
- Implement EVERY fix listed
- Run quality checks
- Commit: `refactor: address review feedback for [Story ID]`
- Overwrite `scripts/ralph/review-feedback.md` with "RESOLVED" (do NOT delete it)
- Then continue to the next story

## Closing GitHub Issues

After marking a story as `passes: true` in prd.json, close its GitHub issue:

```bash
gh auth switch --user jjjake 2>/dev/null || true
gh issue close <ISSUE_NUMBER> --comment "Completed: [Story ID] - [Story Title]"
```

The issue number is in the `githubIssue` field of each story in prd.json.

## Quality Checks

Before committing, ALL of these must pass:

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Do NOT commit broken code. If tests or clippy fail, fix the issues before committing.

## Progress Report Format

APPEND to `scripts/ralph/progress.txt` (never replace existing content, always append):

```
## [Date/Time] - [Story ID]
- What was implemented
- Files changed
- **Learnings for future iterations:**
  - Patterns discovered
  - Gotchas encountered
  - Useful context
---
```

If you discover a **reusable pattern**, add it to the `## Codebase Patterns` section at the TOP of progress.txt.

## Key Codebase Context

- **Workspace:** `ia-core` (library) + `ia-cli` (binary with TUI)
- **Core library:** `ia-core/src/` — client.rs, config.rs, error.rs, types.rs, metadata.rs, download.rs, search.rs, joblog.rs, etc.
- **CLI:** `ia-cli/src/` — main.rs (clap), commands/{download,metadata,search,list,status}.rs
- **Tests:** Unit tests inline, integration tests in `ia-core/tests/` and `ia-cli/tests/cli.rs`
- **Design doc:** `docs/plans/2026-02-22-metadata-write-design.md`
- **Implementation plan:** `docs/plans/2026-02-22-metadata-write-implementation-plan.md`

## Important Constraints

- **Rust 1.85** on this machine — pin `wiremock` to 0.6.2 (later versions need let chains)
- **reqwest-middleware** doesn't expose `.json()` — use `.header("content-type", ...).body(...)`
- **Reserved global short flags:** `-c, -l, -d, -i, -H, -j, -q` — subcommands MUST NOT reuse these
- **All test metadata must be FAKE** — do NOT use real archive.org item metadata in tests
- **One story per iteration** — implement one, commit, and exit (unless fixing review feedback first)
- **Scope: metadata write ONLY** — do NOT work on any unrelated issues

## Stop Condition

After completing a user story, check if ALL stories have `passes: true`.

If ALL stories are complete and passing, reply with:
<promise>COMPLETE</promise>

If there are still stories with `passes: false`, end your response normally (another iteration will pick up the next story).
