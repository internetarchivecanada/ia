# Spreadsheet reader: value types, identifier key, repeated headers (#154)

`ia_core::spreadsheet::read_spreadsheet` feeds `ia upload --spreadsheet`,
`ia metadata --spreadsheet`, `ia tasks submit --spreadsheet` and
`ia verify --spreadsheet`; `read_identifiers_from_file` (built on it) feeds the file arguments and
`--itemlist` of `ia metadata export`.
Three of its rules change input without a message. This plan gives each a
decided behaviour, implemented once in the reader so every command gets it.

## Rules

### 1. JSONL values are read by type

The rules of `ia collection create --spreadsheet` (#147,
`ia_core::collection::read_jsonl_rows`) become the rules of the shared reader,
except for numbers and booleans, so one JSONL file means the same thing to
`ia upload`, `ia metadata`, `ia tasks submit` and `ia verify`:

| Value | Result |
|-------|--------|
| string | the value, trimmed (as the CSV reader trims every cell); blank is no value |
| `null` | no value, so a `-m` default for the field still applies |
| number, boolean | the value as written in the file (`1.50`, `1e3`, `true`) |
| array of strings | one value per element, as indexed columns `key[0]`, `key[1]`, … |
| object, array holding anything but strings | refused |

An array gives indexed keys because that is how every consumer already
receives several values for one field from a CSV (`subject[0]`,
`subject[1]`): `ia upload` places them with `fill_positions` and `ia metadata`
merges them with `merge_indexed_columns`. An array under a key that is
already indexed (`"subject[0]": ["a"]`) is refused, since it has no position
to expand into.

Numbers and booleans are sent as text, since files that use them work
today and must keep working. Earlier versions sent serde_json's rendering
(`1.50` as `1.5`); the reader now keeps each value's raw JSON text
(serde_json's `raw_value` feature), so the value sent is the one written in
the file. Arrays still take strings only: an array was never split into
values before this change, so no file relies on numbers inside one. A
numeric `identifier` was skipped before and is refused now.
`ia collection create` keeps refusing numbers and booleans. The Python
client has no JSONL spreadsheet input, so there is nothing there to match.

### 2. The `identifier` key is case-insensitive

Every other key is lowercased, and CSV, TSV, XLSX and ODS headers are
compared lowercased, `identifier` included. JSONL now matches the identifier
key the same way. A JSONL line is refused, not skipped, when it has no
identifier, an identifier that is not a string, a blank identifier, or the
key twice (`identifier` and `Identifier`). Any other key given twice in
different case (`title` and `Title`) is refused too, since one value would
be dropped. Blank lines are still skipped, and a line that is not a JSON
object is reported with its line number.

### 3. A repeated header is refused

In CSV, TSV, XLSX and ODS, two columns whose headers are equal after
trimming and lowercasing are refused, naming the header. The reader keys a
row by header, so one of the two cells is always lost; the Python client
(`csv.DictReader`) keeps the last cell even when it is empty. Several
values for one field are written `subject[0]`, `subject[1]`, which the error
says. Blank headers are not compared: a trailing delimiter in the header row
gives blank headers, and refusing those would refuse files that read
correctly today.

### Reporting

Every problem in the file is collected and reported together as one
`IaError::InvalidArgument`, one problem per line, each naming the line (JSONL)
or header, before any command makes a request. This is how
`ia collection create` reports its rows.

### Identifier lists

`read_identifiers_from_file` needs only the identifier, so it applies rules
2 and 3 to the identifier only (key case, type, presence, a repeated
`identifier` header) and ignores the other values, so a JSONL file whose other fields hold numbers
(a download count, for example) still works as an item list.

## Export read-back

`ia metadata export -o FILE.csv` (or `.tsv`, `.xlsx`) with `--joblog`, when
resumed, reads the rows already in its file and rewrites the file with them
and the new items. Two changes follow from the header rule:

- A read error used to print a warning and start fresh, so the rewrite
  dropped the rows of every item the joblog skipped. The export now reads
  the file before any request and stops with an error naming the file and
  the reason, leaving the file as it is.
- The export writes keys in the case IA returns (`Title` and `title` are
  separate keys on IA). Read back through the normalising reader, `Title`
  became `title`, so a later resume met both and was refused. The read-back
  uses `read_exported_spreadsheet`, the same reader with header case kept;
  `identifier` is still matched in any case. Every user-input path keeps
  the normalising reader, where `Title` and `title` are one field.

A file that an earlier version resumed may hold both `title` and `Title`
columns. It reads, and is rewritten as it is; `--no-resume` exports every
item again in the case IA returns.

## Out of scope

- A CSV row whose `identifier` cell is blank is still skipped. The Python
  client differs per command (`ia upload` continues the previous row's item;
  `ia metadata` skips the row), so it needs its own decision.
- `ia_core::collection::read_jsonl_rows` keeps its own reader; moving it onto
  the shared reader is a follow-up once both are merged.
- A cell under a blank header is still kept under the empty key.

## Review findings

- The export read-back was the main finding; resolved as above.
- An identifier given as an array is reported as "an array"; JSONL keys
  are not trimmed, as in `ia collection create`.
- An item whose metadata holds an `Identifier` key (other than the
  `identifier` one) is written as its own column, which the read-back
  matches as a second `identifier`; the resume then stops with an error.
  Not handled; no such item is known.

## Tests

`ia-core/src/spreadsheet.rs` unit tests, written first and failing:

- JSONL: `null` is no value; an array of strings gives `key[0]`, `key[1]`;
  numbers and booleans are kept as written (`1.50`, `1e3`, `-5`, a 30-digit
  number, `true`), indexed keys included; an object and a mixed array are
  each refused, all in one error with line numbers; an array under an indexed key is refused;
  `Identifier` is read as the identifier; a numeric, blank or missing
  identifier is refused; a key given twice in different case is refused;
  string values are trimmed; a line that is not an object is reported with
  its line number.
- CSV, TSV and XLSX: a repeated header (`title,Title`) is refused naming the
  header; blank headers are not.
- `read_identifiers_from_file` on JSONL with numeric values returns the
  identifiers, and reads `Identifier`.

`ia-cli/tests/spreadsheet_values.rs`: for each of `ia upload`,
`ia metadata`, `ia tasks submit` and `ia verify`, a JSONL file with number and
boolean values passes the reader and a CSV with a repeated header fails with
the reader's message; `ia upload --dry-run` shows `1.50` and `1e3` as written,
with the host pointed at a closed local port so no request can leave the
machine.
