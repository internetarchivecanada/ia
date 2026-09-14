# Showcase

A short tour of the things `ia` does that the Python `ia` can't, or does better. This is
a shop window, not a manual — for the full reference see [usage.md](./usage.md).

Every command below is real. Read examples run against public items; write examples use a
disposable test item and `--dry-run`, so nothing is mutated. Output blocks are shown only
where the output itself is the point; long output is trimmed with a `# …` marker.

---

## 1. Know the schema before you touch it

`ia` fetches the Internet Archive metadata schema from the `ia-metadata` item and shows
it in the terminal — every user-facing field, whether it's required, repeatable, and who
controls it.

```
$ ia metadata schema
 FIELD                      LABEL                            REQUIRED     REPEATABLE
 addeddate                  Date Added to Public Search      Yes          No
 collection                 Collections                      No           Yes
 creator                    Creator                          No           Yes
 date                       Publication Date                 No           No
 description                Item Description                 Recommended  Yes
 identifier                 Item Identifier                  Yes          No
 isbn                       ISBN                             No           Yes
 language                   Language                         No           Yes
 licenseurl                 License URL                      No           No
 mediatype                  Type of Media                    Yes          No
 publisher                  Publisher                        No           No
 rights                     Rights                           No           No
 subject                    Subject                          No           Yes
 title                      Title                            Recommended  No
 # … (many more fields)
```

Drill into any field for the authoritative definition, accepted values, and an example:

```
$ ia metadata schema addeddate
addeddate
  Label:           Date Added to Public Search
  Required:        Yes
  Repeatable:      No
  Internal:        No
  Defined by:      IA software
  Edit access:     not editable
  Definition:      2019-12 and later dates: represents time item was added to
                   public search engine. # …
  Accepted values: YYYY-MM-DD HH:MM:SS
                   YYYY-MM-DD
  Example:         2017-03-28 22:05:46
```

Python `ia` has no equivalent — you'd be guessing field names or reading wiki pages.

---

## 2. Many edits, one request

Compound `+` chaining composes several metadata operations into a single atomic patch —
fewer round-trips, no half-applied state. `--dry-run` previews the exact JSON-patch first.

```
$ ia metadata modify jj-test-2020-07-21 --dry-run -m "title:New Title" + remove -m "subject:foo"
Dry run -- no changes will be applied

  jj-test-2020-07-21:
    /subject/0: replace -> "baz"
    /subject/1: replace -> "bar"
    /subject/2: remove
    /title: replace -> "New Title"

1 item(s), 4 change(s)
```

Repeatable fields are diffed element by element, so removing one value from a list
reindexes the rest correctly.

---

## 3. Search and act, concurrency built in

Search and parallel download in one command — with resume and a multi-disk destination
pool. No glue scripts, no manual parallelism.

```
$ ia download --search "identifier:(jj-test-*)" --jobs 16
```

A `--dry-run` previews the set and total size before anything transfers, then a static
summary reports what completed, throughput, free space, and HTTP timing:

```
────────────────────────────────────────────────────
15/15 items (15 done)
.: 19.13 GiB free
HTTP: 17 requests · p50 366 ms · p95 748 ms
```

---

## 4. Compose across commands — concurrent read *and* write

The same inputs (`--search`, `--itemlist`, stdin) drive both export and edit, and both
fan out concurrently across items. Identifiers pipe straight through — `ia search --itemlist`
emits one per line, `ia metadata export` reads them from stdin.

```
# Export the metadata of a whole collection to a spreadsheet
$ ia search "collection:mybooks" --itemlist | ia metadata export -o out.xlsx

# Or edit every match in one fanned-out, atomic-per-item pass
$ ia metadata --search "collection:mybooks" -m "rights:public domain" --dry-run
```

Unix-style composition end to end, parallel by default rather than a serial loop.

---

## 5. ZIP surgery — one page, transcoded, without the whole archive

Pull a single member out of a remote ZIP *and* convert it on the way down, without
downloading the multi-gigabyte archive.

```
# List members of a remote ZIP
$ ia download journalofhorticu3441unse --zip-list journalofhorticu3441unse_jp2.zip

# Pull one page and transcode JP2 → JPEG in flight
$ ia download journalofhorticu3441unse \
    --zip-member journalofhorticu3441unse_jp2.zip/journalofhorticu3441unse_0001.jp2 \
    --zip-convert jpg
```

Python `ia` has nothing like it.

---

## 6. Mirror a whole collection locally

The multi-disk pool spreads files across volumes by free space; auto-resume means
re-running the same command picks up exactly where an interrupted run left off.

```
$ ia download --search "collection:mycollection" --destdir /vol/a --destdir /vol/b --joblog dl.jsonl
$ ia status --joblog dl.jsonl   # see results and any failures
```

---

## 7. Bulk-fix metadata from a spreadsheet

Round-trip metadata through a spreadsheet and preview every change before writing.

```
$ ia metadata export --search "collection:mycollection" -o data.xlsx
# edit data.xlsx
$ ia metadata --spreadsheet data.xlsx --dry-run   # then drop --dry-run to apply
```

---

## 8. Resumable batch upload

Interrupt-safe batch upload (same joblog / auto-resume model as download), multipart for
large files, streaming progress.

```
$ ia upload template ./photos -o items.csv   # edit to add metadata
$ ia upload --spreadsheet items.csv
# interrupt, re-run the same command — completed files skip automatically
```

---

## 9. Re-derive a set of items

Submit catalog tasks across every search match in one command.

```
$ ia tasks submit --search "collection:mycollection" --cmd derive --dry-run
```

---

## 10. Verify a local backup against archive.org

Confirm local files exist remotely with matching checksums (MD5 / SHA-1 / CRC32), no
re-download.

```
$ ia verify --spreadsheet items.csv
```

---

## See also

- **AI QA (alpha):** `ia ai qa <id>` — vision-based LLM verification of extracted
  metadata. Feature-gated, needs an API key. See [usage.md](./usage.md).
- **Full-text search:** `ia search fts "…"`, or `--dsl` for raw Elasticsearch queries.
- **Privacy by default:** downloads send `cnt=0` so they don't inflate public view
  counts (`--count-views` to opt in).
- **`--json` everywhere:** every command emits structured JSON/JSONL for scripts, agents,
  and MCP servers.
