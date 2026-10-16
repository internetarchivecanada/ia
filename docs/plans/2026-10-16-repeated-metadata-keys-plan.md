# Repeated and indexed keys under `metadata modify` (#65)

**The bug.** `ia metadata ITEM -m collection:a -m collection:b` writes `collection: "b"`. `prepare_metadata` in `ia-core/src/metadata/write.rs` applies `MetadataOp::Set` one `(key, value)` pair at a time with `obj.insert`, so a repeated key overwrites itself and only the last value survives. The same arm inserts an indexed key such as `collection[0]` as a literal field name, so `-m "collection[0]:a" -m "collection[1]:b"` adds two new fields called `collection[0]` and `collection[1]` instead of building the list. `insert` and the spreadsheet import already handle `field[N]`; `modify`, the bare `-m` shorthand, and `+ modify` continuations do not.

**Decision.** Match the Python `internetarchive` client, which this project ports. Its CLI groups repeated `-m` keys into a list (`cli_utils.get_args_dict`: values accumulate per key, identical values are kept once, a single value stays a scalar), and its `iarequest.prepare_metadata` treats `field[N]:value` under modify as "take the item's current list for `field`, overwrite position N, keep the rest"; `REMOVE_TAG` at a position deletes that element. No flag and no new syntax.

**Where.** The fix lives in `prepare_metadata`, so every caller is covered: the three CLI paths above, the dry run, and any library consumer that passes repeated keys to `compute_patch` or `modify`. The `Set` arm becomes a two-pass function over the whole change list:

1. Bare keys, in order of first appearance. Values for one key accumulate, duplicates dropped; `REMOVE_TAG` as the only value deletes the field, otherwise it is skipped; one value is written as a scalar, several as an array. A value that is already a JSON array (a library caller) is written as is when it is the only value for its key, and flattened into the accumulated list otherwise.
2. Indexed keys, in order. The working list is the field's value after pass 1 (array as is, scalar wrapped, missing empty), padded with `null` up to N; the value is written at N, or N is marked for deletion on `REMOVE_TAG`. Marked and padding `null` slots are dropped at the end. An empty result deletes the field, except `collection`, which errors as `remove` already does ("cannot remove last collection").

A bare key followed by an indexed key for the same field (`-m collection:a -m "collection[1]:b"`) therefore gives `["a", "b"]`, as in Python. A scalar `Set` on a field that is currently a list still replaces the whole list; that is the documented "replaces existing values" behavior and the fix leaves it alone.

**Other ops.** `append`, `append-list`, and `remove` already apply each pair against the running value, so repeated keys accumulate correctly there; `insert` has its own per-index groups. They are not changed.

**User-visible text.** The `modify` help and `docs/usage.md` gain a multi-value example (`-m subject:a -m subject:b` sets the list) and a line on `field[N]:value` under modify.

## Tasks
- [ ] Plan committed first.
- [ ] Red: unit tests on `prepare_metadata` and `compute_patch` for repeated bare keys (list, dedup, single stays scalar, REMOVE_TAG mixed in, array value passthrough), indexed keys (overwrite in place, extend past the end, create from nothing, REMOVE_TAG at a position, last-collection guard), and bare-then-indexed mixing; hermetic CLI tests (wiremock) that run `ia metadata ITEM -m collection:a -m collection:b --dry-run --json` and the indexed form, and one that captures the real POST and decodes `-patch`.
- [ ] Green: the two-pass `Set` in `write.rs`.
- [ ] Docs: `modify` help, top-level `metadata` help, `docs/usage.md`.
- [ ] `cargo test -p ia-core -p ia-cli`, `cargo clippy -p ia-core -p ia-cli -- -D warnings`; code-reviewer pass; PR referencing #65.
