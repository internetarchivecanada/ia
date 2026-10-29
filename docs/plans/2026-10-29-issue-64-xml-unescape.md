# Multipart listings decode XML entities (#64)

**The gap.** `extract_xml_field` in `ia-core/src/upload/multipart.rs` returns the characters between `<Tag>` and `</Tag>` as sent. IA-S3 escapes text in its XML answers, so a file `a&b.bin` is listed as `<Key>a&amp;b.bin</Key>`. Every use of a listed field then works on the escaped text:

- `try_resume` compares the listed key with the local key, finds no candidate, and starts a fresh upload; the old one is left on IA.
- The per-item listing at the start of a run (#21) does not recognise the file as having an unfinished upload, so a rerun sends it as a single PUT.
- `ia upload cleanup` prints the escaped name, and `--abort` builds the abort URL from it (`a%26amp%3Bb.bin`), which names no upload.
- A `NextKeyMarker` with an entity is sent back as the escaped text, so the next listing page starts at the wrong key.

The completion manifest already escapes `&`, `<` and `>` when it is built, so the two directions disagree.

**Decision.** `extract_xml_field` decodes the five predefined entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`) and numeric character references (`&#NN;`, `&#xHH;`) in the text it returns. Every field the parsers return (`Key`, `UploadId`, `Initiated`, `ETag`, and the pagination markers) passes through it, so every comparison, URL and message uses the real text.

The decoding is `quick_xml::escape::unescape_with` with `resolve_xml_entity`, which names XML's five entities only, whatever features other crates enable on `quick-xml` (its `escape-html` feature widens plain `unescape` to every HTML5 entity name). `quick-xml` 0.42 is already compiled into `ia-core` through `calamine` (which reads `.xlsx` metadata spreadsheets); it becomes a direct dependency of `ia-core` with no features (its default set is empty), so while `calamine` requires the same minor version nothing new is built. The two are bumped together. A hand-written decoder would repeat the numeric reference rules (decimal and hex, no sign, no NUL, a valid Unicode scalar value) that the crate already handles and tests.

Whitespace is trimmed before decoding, as today, so a space written as `&#32;` at either end survives. Decoding is one pass: `&amp;amp;` becomes `&amp;`, as XML requires. Text with a malformed or unknown reference (a bare `&`, `&foo;`, `&#0;`) is returned as sent: a server that did not escape a bare `&` most likely sent the real name, and dropping the field would hide the upload entirely.

**Upload IDs in URLs.** The four URLs that carry an upload ID (part PUT, complete, abort, list parts) put it in the query string as is. A decoded ID with `&` would split the query, so the ID is percent-encoded there, as the pagination markers already are. IA's upload IDs are alphanumeric in practice, so this changes no request seen so far.

**`s3_error.rs`.** `parse_s3_error` has an identical copy of `extract_xml_field` for `<Code>` and `<Message>`. `Message` is shown to users (`describe_s3_failure`), and an IA-S3 message that quotes a file name would show `&amp;`. `Code` values are fixed ASCII names, so decoding them changes nothing. The two copies become one: the function in `s3_error.rs` gains the decoding and becomes `pub(crate)`, and `multipart.rs` uses it instead of its own copy.

## Tasks
- [x] Plan committed first.
- [x] Red: unit tests for the decoder (each predefined entity, decimal and hex references, plain text unchanged, one-pass decoding, malformed text returned as sent, trimming before decoding); parser tests for `parse_list_uploads_response` (escaped `Key`, `UploadId`, `Initiated`), `parse_list_parts_response` (escaped `ETag`), `parse_initiate_response`, and the listing's next-page marker; `parse_s3_error` with an escaped `Message`; a hermetic resume test in `ia-core/tests/upload_multipart.rs` for a file whose key is `a&b.bin`, listed as `a&amp;b.bin`, which resumes the existing upload and sends no initiate.
- [x] Green: `quick-xml` dependency, decoding in the shared `extract_xml_field`, the duplicate in `multipart.rs` removed, AGENTS.md crate list.
- [x] `cargo test --locked -p ia-core -p ia-cli`, `cargo clippy --locked -p ia-core -p ia-cli -- -D warnings`.
- [x] Code-reviewer pass; PR.

## Review (2026-10-29), closed before the PR

Taken: upload IDs are percent-encoded in the four URLs that carry one, and the resume test lists its upload ID with an escaped `&`; decoding uses `resolve_xml_entity` so a feature enabled elsewhere cannot widen it to HTML entity names (test: `&nbsp;` stays as sent); the `Cargo.toml` comment says the single copy holds while `calamine` requires the same minor version.

Not changed: one malformed reference leaves the whole field as sent, including any well-formed references in it (`a&amp;b&c` stays as is), as the decoder's comment states; a server that escapes correctly never sends that. `strip_xml`, the fallback message for an error body without `<Code>`, does not decode; it affects only what a reader sees in a message for a body that is not an S3 error, and is outside this change.
