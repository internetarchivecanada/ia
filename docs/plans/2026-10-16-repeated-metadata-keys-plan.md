# Repeated and indexed keys under `metadata modify` (#65)

**The bug.** `ia metadata ITEM -m collection:a -m collection:b` writes `collection: "b"`. `prepare_metadata` in `ia-core/src/metadata/write.rs` applies `MetadataOp::Set` one `(key, value)` pair at a time with `obj.insert`, so a repeated key overwrites itself and only the last value survives. The same arm inserts an indexed key such as `collection[0]` as a literal field name, so `-m "collection[0]:a" -m "collection[1]:b"` adds two new fields called `collection[0]` and `collection[1]` instead of building the list. `insert` and the spreadsheet import already handle `field[N]`; `modify`, the bare `-m` shorthand, and `+ modify` continuations do not.

**Decision.** Match the Python `internetarchive` client, which this project ports. Its CLI groups repeated `-m` keys into a list (`cli_utils.get_args_dict`: values accumulate per key, identical values are kept once, a single value stays a scalar), and its `iarequest.prepare_metadata` treats `field[N]:value` under modify as "take the item's current list for `field`, overwrite position N, keep the rest"; `REMOVE_TAG` at a position deletes that element. No flag and no new syntax.

**Where.** The fix lives in `prepare_metadata`, so every caller is covered: the three CLI paths above, the dry run, and any library consumer that passes repeated keys to `compute_patch` or `modify`. The `Set` arm becomes a two-pass function over the whole change list:

1. Bare keys, in order of first appearance. A key named once is written exactly as given: a scalar, or a library caller's JSON array as is, duplicates included (the existing `compute_patch_no_changes_on_duplicate_values_item` test pins this); `REMOVE_TAG` deletes the field. A key named more than once becomes one list: array values contribute their elements, each value is kept once, `REMOVE_TAG` entries are skipped, a single survivor is written as a scalar, and a group left with nothing deletes the field (`collection` errors instead, as below).
2. Indexed keys, grouped per field and applied in index order (a position named twice takes its later value). The working list is the field's value after pass 1 (array as is, scalar wrapped, missing empty). A position inside the list is overwritten, or marked for deletion on `REMOVE_TAG`; a position past the end appends, so the list is never padded: the first draft padded with `null` up to N, which the review showed panics on `subject[18446744073709551615]` (overflow in debug, out-of-bounds in release) and would try to allocate gigabytes for `subject[4000000000]`. Marked slots are dropped at the end; an empty result deletes the field.

**The collection guard.** `remove` refuses to empty `collection`. `Set` now does the same, judged on the whole batch: if the item had a collection before and would have none after, the batch errors. Judging it per pass (the first draft) wrongly refused `-m collection:REMOVE_TAG -m "collection[0]:x"`, and judging it without looking at the source wrongly refused `collection:REMOVE_TAG` on a file-level target that never had the field. A bare `-m collection:REMOVE_TAG` on an item previously deleted every collection without complaint; it now errors. Across `+` groups the guard is still per group, as for `remove`.

A bare key followed by an indexed key for the same field (`-m collection:a -m "collection[1]:b"`) therefore gives `["a", "b"]`. This is a deliberate difference from Python, whose indexed pass seeds from the item's current list and discards the bare value (`['old1', 'b', 'old3']` for that input, verified during review); a value the user gave should not vanish. A scalar `Set` on a field that is currently a list still replaces the whole list; that is the documented "replaces existing values" behavior and the fix leaves it alone.

**Other ops.** `append`, `append-list`, and `remove` already apply each pair against the running value, so repeated keys accumulate correctly there; `insert` has its own per-index groups. Their bodies move verbatim into one helper per operation (`apply_append`, `apply_append_list`, `apply_insert`, `apply_remove`) so `prepare_metadata` dispatches once on the operation instead of once per pair; behavior is unchanged.

**`parse_indexed_key`.** Now requires the key to end with `]`: `subject[1]zzz` used to parse as `subject[1]` and drop the tail. It is public; `--expect` and spreadsheet columns go through it too, and none of them had a reason to accept trailing text.

**User-visible text.** The `modify` help and `docs/usage.md` gain a multi-value example (`-m subject:a -m subject:b` sets the list) and a line on `field[N]:value` under modify.

## Tasks
- [x] Plan committed first.
- [x] Red: unit tests on `prepare_metadata` and `compute_patch` for repeated bare keys (list, dedup, single stays scalar, REMOVE_TAG mixed in, array value passthrough), indexed keys (overwrite in place, extend past the end, create from nothing, REMOVE_TAG at a position, last-collection guard), and bare-then-indexed mixing; hermetic CLI tests (wiremock) that run `ia metadata ITEM -m collection:a -m collection:b --dry-run --json` and the indexed form, and one that captures the real POST and decodes `-patch`.
- [x] Green: the two-pass `Set` in `write.rs`.
- [x] Docs: `modify` help, top-level `metadata` help, `docs/usage.md`.
- [ ] `cargo test -p ia-core -p ia-cli`, `cargo clippy -p ia-core -p ia-cli -- -D warnings`; code-reviewer pass; PR referencing #65.

## Review (2026-10-16), closed before merge

Important, taken: a huge index panicked (padding; now append-past-end, test `prepare_set_huge_index_appends_without_panicking`); the bare-then-indexed mix was claimed to match Python and does not (wording corrected above); the collection guard fired on intermediate state and on items with no `collection` field (now judged on the whole batch against the source; tests `prepare_set_collection_remove_tag_then_indexed_value_succeeds`, `prepare_set_collection_guard_ignores_an_item_without_collection`). Suggestions, taken: strict `parse_indexed_key`; `MetadataOp::Set` and `REMOVE_TAG` doc comments; a CLI test for the refusal message (`metadata_modify_remove_tag_on_collection_is_refused`); tests for out-of-order and repeated indexes.

The six CLI tests were run against the unfixed `write.rs` (the core change stashed, the tests kept) and all six failed; with the change restored all six pass.
