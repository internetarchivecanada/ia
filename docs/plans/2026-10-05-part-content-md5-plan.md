# Content-MD5 on multipart part PUTs

**Source:** Jake's live probe, 2026-10-05, against `s3.us.archive.org` with a fresh `test_collection` item (script kept out of the repo): a part PUT with a wrong `Content-MD5` got `400 BadDigest` ("The Content-MD5 you specified did not match what we received"); the same part with the right one got `200` and an `etag` header holding the body's md5. So IA checks `Content-MD5` on part PUTs, and IA does return an ETag on a part PUT, contrary to the comment on `upload_part`. The probe's abort got `404 NoSuchBucket` eight seconds after the initiate had created the item (observation, below).

**Change:** `upload_part_with_retry` sends `Content-MD5` (base64 of the part's md5, the same digest that already goes into the manifest as the ETag) unless `opts.verify` is off, as the single PUT does under `--no-verify`. A part whose bytes arrive corrupted is refused at the PUT instead of at completion, and the error says which part. The `upload_part` doc comment says IA returns an ETag and why the manifest carries the local md5 anyway (PR #44). `base64_encode` in `single.rs` becomes crate-visible for the second call site.

**Not changed, flagged in the PR:** `BadDigest` is a non-retryable S3 code in `s3_error::is_retryable_code` (pre-existing, pinned by tests; the same for the single PUT). On a part PUT the body is in memory, so a `BadDigest` means the bytes changed on the wire, and a re-send would fix it; today it fails the part for good ("refused by IA (BadDigest ...)"), the upload is kept, and a rerun re-sends that part. Making `BadDigest` retryable is a separate decision for both paths.

**Observation, no change:** an abort issued seconds after an initiate that created the item can get `404 NoSuchBucket` from IA while the new item propagates. `ia upload cleanup` on a brand-new item may see the same; nothing in the code path depends on it.

## Tasks
- [x] Red (`ia-core/tests/upload_multipart.rs`): `upload_part` sends `Content-MD5` equal to the base64 md5 of the body; through `upload_file` the part carries it by default; with `verify: false` no part PUT carries it. Run: the first two failed, the third passed already (it pins the `--no-verify` case).
- [x] Green: the header (`upload_part_with_retry` takes `content_md5: bool`; `upload_part` passes true, the part loop `opts.verify`), `base64_encode` crate-visible, the doc comment; `--no-verify` help and usage.md say the header is skipped on parts too; "Completing a multipart upload" says each part is checked on receipt.
- [ ] `just ci`; code-reviewer pass; PR; merge after checks; `scripts/ia-cleanup part-content-md5` after a confirmed merge.
