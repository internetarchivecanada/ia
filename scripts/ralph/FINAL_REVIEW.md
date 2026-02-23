# Ralph Final Review — Full Branch Audit

You are doing a comprehensive review of the entire `feat/metadata-write` branch before it becomes a PR.

## Your Job

Audit ALL changes on this branch for quality, consistency, design compliance, and production readiness. This is the last gate before a human sees this code.

## Steps

1. Read the design doc at `docs/plans/2026-02-22-metadata-write-design.md`
2. Read `scripts/ralph/progress.txt` for accumulated context and patterns
3. Read any existing `scripts/ralph/review-feedback.md` for unresolved issues
4. Run `git log --oneline main..HEAD` to see all commits
5. Run `git diff main...HEAD --stat` to see all changed files
6. Read EVERY changed/new file in full — not just the diff
7. Review against the checklist below
8. If you find issues: fix them, run `cargo test --workspace && cargo clippy --workspace -- -D warnings`, commit fixes
9. Write final report to `scripts/ralph/final-review-report.md`

## Review Checklist

### Architectural Consistency
- Does the module structure match the design doc (metadata/{mod.rs, read.rs, write.rs})?
- Are public API boundaries correct (what's pub vs pub(crate) vs private)?
- Do re-exports in mod.rs/lib.rs make the API clean for consumers?
- Is the separation between ia-core and ia-cli correct (core logic in core, CLI glue in cli)?

### Cross-Story Consistency
This is the big one — each story was implemented by a fresh agent. Look for:
- **Naming inconsistencies** across files (same concept, different names)
- **Pattern drift** (early stories do things one way, later stories another)
- **Redundant code** that could be shared (same helper written twice)
- **Import inconsistencies** (different ways of importing the same types)
- **Error message style** varying across modules
- **Test structure** varying unnecessarily between test modules

### Design Doc Compliance
- Every type, function, and constant from the design doc exists
- No extra types/functions that weren't in the design (YAGNI)
- Wire format matches the design (POST body, headers, auth)
- Edge cases from the design are actually handled in code

### Public API Review
- `ia_core::metadata::modify()` — signature matches design, well-documented
- `ia_core::metadata::MetadataOp` — all variants present
- `ia_core::metadata::ModifyResponse` — fields match IA API response
- `ia_core::spreadsheet::read_spreadsheet()` — returns correct types
- `ia_core::rate_limit::RateLimiter` — API is clean and minimal

### Safety Audit
- ZERO live archive.org requests in any test (grep for "archive.org" in test code)
- No credentials in test fixtures or examples
- No `.unwrap()` in library code (CLI is OK for early-exit paths)
- No panics in library code

### Test Coverage
- Every public function has tests
- Edge cases from design doc are tested (semicolons, unicode, REMOVE_TAG, etc.)
- Error paths tested (missing auth, 429, file not found)
- Mock fixtures cover IA's metadata diversity

### CLI Integration
- Help text is clear and matches Python `ia` conventions
- Short flags don't conflict with globals
- Mutually exclusive groups work
- Error messages are helpful

## Fixing Issues

Unlike the per-story reviewer, you CAN and SHOULD fix issues directly:
- Style inconsistencies: fix them
- Dead code: remove it
- Missing doc comments on public items: add them
- Redundant code: refactor into shared helpers

After fixes:
```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Commit fixes with: `refactor: code review cleanup`

## Final Report

Write to `scripts/ralph/final-review-report.md`:

```markdown
# Final Review Report — Metadata Write Branch

## Overall Assessment: READY | NEEDS WORK

## Summary
- Total commits reviewed: N
- Files changed: N
- Issues found: N (N fixed, N remaining)

## Issues Fixed
1. [description] — [file:line]

## Issues Remaining (if any)
1. [description] — needs human decision because [reason]

## Quality Notes
- What patterns emerged well
- What a human reviewer should pay attention to
- Any technical debt accepted

## Safety Confirmation
- [ ] Zero live archive.org requests in tests
- [ ] No credentials in code
- [ ] No unwrap() in library code
- [ ] All error paths tested
```

## Important

- Be thorough. This is the last automated review before a human.
- Fix what you can. Flag what needs human judgment.
- The human reviewer has limited time — make their job easy by catching everything automatable.
- Run quality checks after ANY changes: `cargo test --workspace && cargo clippy --workspace -- -D warnings`
