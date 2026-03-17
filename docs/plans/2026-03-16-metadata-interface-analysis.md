# Metadata Command Interface Analysis

## 1. Command Shape & Taxonomy: 9 Subcommands — Reasonable but Confusing

**Grouping:**
- **Read**: `export`, `schema` (bare read is implicit on main command)
- **Write**: `modify`, `append`, `append-list`, `insert`, `remove`
- **Batch write**: `import`

**Problem**: The read/write split isn't visible in naming. Users don't know whether to use `export` (bulk read) or bare `ia metadata ID` (single read). Export takes `files` (identifiers), not metadata — semantically it's "export FROM items TO file", but the positional arg position suggests "export FROM file". This is backwards from the actual data flow.

**Verdict**: 9 subcommands is acceptable. The conceptual grouping is clear, but **the semantics of "export" are inverted** — it should be named or positioned to make clear that you're reading FROM items, not reading FROM a file. Compare: `ia metadata export --search "..."` (intuitive) vs `ia metadata export items.csv` (confusing — is this a file you're exporting, or a file of identifiers?).

---

## 2. Export vs Import Asymmetry: Confusing Dual Semantics

| Command | Input | Output | Positional Arg |
|---------|-------|--------|-----------------|
| `export` | Items (via files/search/stdin) | File or JSONL | `files` (identifiers) |
| `import` | Spreadsheet file | Items (write to archive.org) | `file` (metadata) |

**The problem**: Both take a positional `file`/`files` arg, but with opposite meanings:
- `ia metadata export items.csv -o out.csv` — read FROM items identified IN items.csv
- `ia metadata import data.csv` — read FROM data.csv, write TO items

A user might reasonably expect these to be inverse operations, but the positional args tell different stories. It's not obvious that export reads identifiers from a file, while import reads metadata from a file.

**Verdict**: This is confusing. Either:
- Rename export to `export-ids` or `pull` to clarify the direction
- Add `--source` / `--from-file` for export to make explicit that files are input sources, not output
- Or lean into the asymmetry and add examples showing the workflow: `export --search ... -o data.xlsx && import data.xlsx`

---

## 3. Batch Expression: Two Mechanisms

**Write operations** (modify/append/etc):
```bash
ia metadata modify ID -m field:value
ia metadata modify --itemlist ids.txt -m field:value
ia metadata modify --search "collection:test" -m field:value
```
Batch is via **options** on per-operation subcommands.

**Import operation**:
```bash
ia metadata import data.csv
```
Batch is the **entire subcommand**. The input file structure (one row = one item's changes) defines batching.

**Problem**: These are conceptually the same (modify multiple items), but expressed differently. A user might not realize that `modify --search "..."` is equivalent to "apply the same change to all results," while `import data.csv` is "apply different changes to different items."

**Verdict**: The two mechanisms work, but don't feel unified. This is acceptable IF documented, but could confuse power users. **No action needed** — the mental model is: `modify/append/etc` are for uniform batch changes, `import` is for heterogeneous batch changes.

---

## 4. Compound Operations (`+`): Not Discoverable, But Useful

The `+` syntax allows chaining multiple operations:
```bash
ia metadata modify ID -m "title:New" + remove -m "subject:old" + append-list -m "subject:new"
```

**Issues:**
- Not mentioned in `ia metadata -h` (only in `modify` help)
- No other `ia` command has this syntax
- Users won't discover it without reading docs
- The grammar is unusual for CLI conventions

**Discover-ability fix**: Add `+` to the top-level help text: "Chain operations with + for single-request compound edits."

**Precedent**: No other command uses this. It's novel, powerful, but requires documentation and discovery. Acceptable for power users, but needs better visibility.

**Verdict**: Keep it, but **improve discoverability** in help text and CONTRIBUTING.md.

---

## 5. Comparison to Other Commands

| Comparison | Pattern | Assessment |
|------------|---------|------------|
| `ia metadata import <file>` vs `ia upload import <file>` | Same naming, different semantics | **Different enough**: metadata import applies field changes per-row; upload import groups rows by identifier into single uploads. Same word, fundamentally different behavior. Could be confusing. |
| `ia metadata modify --itemlist` vs `ia tasks submit --itemlist` | Same flag, same concept | ✓ Consistent. Both read identifiers from a file and apply the same operation to all. |
| `ia metadata export` vs `ia tasks list` | No `--itemlist` on export | ✗ Asymmetric. Export supports `--search` but not `--itemlist`, while modify does. Why? Export can read IDs from a file (positional `files`), so `--itemlist` would be redundant, but it's still inconsistent. |

**Verdict**: The naming parallelism between `metadata import` and `upload import` is **misleading**. They're too different in behavior (field-per-row vs file-per-row grouping). Consider renaming metadata's import to `batch-modify` or `apply`. But this is a pre-release consideration — too late to change now.

---

## 6. Naming Oddities: `append` vs `append-list`

The distinction is semantic:
- `append` — append text to a string field (e.g., append to description)
- `append-list` — append value to a list field (e.g., add a subject)

**Why it's confusing**:
- Users don't think about "is this field a string or a list?" — they think about operations
- The hyphen (`append-list`) is unusual compared to `append-to-list` or `append --to-list`
- Docs and examples are needed to explain the distinction

**Better names** (in priority order):
1. `append-value` (for lists) vs `append-text` (for strings) — clearer intent
2. `append --to-list` flag — more discoverable than hyphenated command
3. Keep as-is — it's fine once documented

**Verdict**: The current naming is **defensible but confusing**. The real issue is that users need to understand whether a field is a string or list to choose the right operation. This is a documentation problem more than a naming problem. The hyphen is acceptable; the lack of context in short help is the real gap.

---

## Summary: UX Friction Points

| Issue | Severity | Fix |
|-------|----------|-----|
| Export's positional `files` semantic (reads FROM, not TO) | Medium | Better help text or rename to clarify direction |
| `import` vs `export` asymmetry | Medium | Document the workflow explicitly in examples |
| Compound `+` syntax undiscoverable | Low | Add to top-level help; link from examples |
| `append` vs `append-list` distinction unclear | Low | Improve short help or use `--to-list` flag |
| `metadata import` vs `upload import` naming clash | Low | Too late to change; document the difference |
| `modify --itemlist` but `export` reads files positionally | Low | Acceptable — export's positional makes sense; modify's batch opts make sense |

## Recommendations

**High priority (improve usability):**
1. Add explicit examples showing `export | import` workflow
2. Clarify that export reads identifiers FROM files, not metadata
3. Highlight compound operations in main help text

**Medium priority (documentation):**
1. Explain `append` vs `append-list` in schema/field help
2. Document the difference between `modify --itemlist` (uniform batch) and `import` (heterogeneous batch)

**Low priority (could be future redesign):**
1. Consider `append --to-list` flag instead of `append-list` subcommand
2. Rename `metadata import` to something that doesn't echo `upload import`'s semantics

The interface is **fundamentally sound** — the issues are **discoverability and naming precedent**, not structural flaws.
