# Checksum files accept uppercase hex (#99)

**The gap.** Windows tools (`CertUtil`, `Get-FileHash`) write digests in uppercase hex. The checksum-file parsers accept them, but every later comparison is a case-sensitive string comparison against IA's lowercase digests:

- `verify::match_file_against_remote` (`ia-core/src/verify.rs`): `rh == local_hash` in match-names mode, `h == local_hash` in hash-only mode. An intact file is reported `mismatch` or `missing`.
- The upload skip check (`ia-core/src/upload/single.rs`): `remote_md5 == Some(local_md5)`. The file is sent again instead of skipped.

`ia upload --checksum-file` reads its file with `parse_checksums`; `ia verify --checksum-file` reads it with `parse_checksums_multi`. Both are public `ia-core` functions.

**Decision.** A digest is lowercase from the moment it enters the program. The four parse functions (`try_parse_gnu`, `try_parse_bsd`, `try_parse_gnu_multi`, `try_parse_bsd_multi` in `ia-core/src/upload/checksum.rs`) return `hash.to_ascii_lowercase()`. Every comparison downstream then sees the form IA uses; no call site gains a case-insensitive compare. Digests computed from a file (`to_hex`) are already lowercase, so no other constructor of a local hash needs a change. `types.rs` is not touched.

**Validation.** The parsers already reject a digest with a non-hex character, and a digest of the wrong length for MD5 (`parse_checksums`, BSD lines) or for an unforced GNU line (auto-detect from length). A GNU line under an explicit `--checksum-type` takes any hex length by design (`parse_multi_forced_algorithm_overrides_detection`); that is unchanged.

**Out of scope.** Library callers that fill `UploadOpts::checksum_file` or `VerifyOpts::checksums` by hand, bypassing the parsers, keep the digest as given. `md5sum -b` lines with a leading `*` in the file name.

## Tasks
- [x] Plan committed first.
- [x] Red: a unit test in `checksum.rs` that an uppercase GNU and BSD line parses to lowercase in both parsers; wiremock tests for `verify_item` with an uppercase checksum file in match-names and hash-only modes (`Verified`); a wiremock test in `ia-core/tests/upload_single.rs` that an uppercase `checksum_file` digest for a file already on the item is skipped with no PUT.
- [x] Green: lowercase in the four parse functions.
- [x] `cargo test --locked -p ia-core -p ia-cli`, `cargo clippy --locked -p ia-core -p ia-cli -- -D warnings`.
- [ ] Code-reviewer pass; PR.
