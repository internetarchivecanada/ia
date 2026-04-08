#!/usr/bin/env bash
#
# Manual integration test for multi-drive download resume.
# Uses temp directories as virtual drives and real archive.org items.
#
# Requirements:
#   - ia binary built (cargo build --release)
#   - Archive.org credentials configured (ia config login)
#   - ~300 MiB disk space for test downloads
#
# Usage:
#   ./scripts/test-multi-drive.sh [path-to-ia-binary]
#
set -euo pipefail

IA="${1:-./target/release/ia}"
TESTDIR=$(mktemp -d)
DRIVE1="$TESTDIR/VirtualDrive1"
DRIVE2="$TESTDIR/VirtualDrive2"
DRIVE3="$TESTDIR/VirtualDrive3"
ITEMS="$TESTDIR/items.txt"

# Small items from us-supreme-court (~50-70 MiB each with PDF+meta filter)
TEST_ITEMS=(
    "micro_IA40385001_0821"
    "micro_IA40385017_1925"
    "micro_IA40385006_0036"
    "micro_IA40386003_0386"
)

pass=0
fail=0

check() {
    local name="$1"
    shift
    if "$@"; then
        echo "  ✓ $name"
        ((pass++))
    else
        echo "  ✗ $name"
        ((fail++))
    fi
}

cleanup() {
    echo ""
    echo "Cleaning up $TESTDIR ..."
    rm -rf "$TESTDIR"
    echo ""
    echo "Results: $pass passed, $fail failed"
    if [ "$fail" -gt 0 ]; then
        exit 1
    fi
}
trap cleanup EXIT

echo "=== Multi-Drive Download Manual Tests ==="
echo "Binary: $IA"
echo "Version: $($IA --version)"
echo "Test dir: $TESTDIR"
echo ""

# Setup
mkdir -p "$DRIVE1" "$DRIVE2" "$DRIVE3"
printf '%s\n' "${TEST_ITEMS[@]}" > "$ITEMS"

# ────────────────────────────────────────────────────
echo "── Test 1: Single-item download with --glob and --checksum ──"
$IA download "${TEST_ITEMS[0]}" \
    --glob '*.pdf|*_meta.xml' \
    --destdir "$DRIVE1" \
    --checksum

check "item directory created" test -d "$DRIVE1/${TEST_ITEMS[0]}"
check "has PDF files" test -n "$(ls "$DRIVE1/${TEST_ITEMS[0]}"/*.pdf 2>/dev/null)"
check "has meta XML" test -f "$DRIVE1/${TEST_ITEMS[0]}/${TEST_ITEMS[0]}_meta.xml"

# ────────────────────────────────────────────────────
echo ""
echo "── Test 2: Checksum skip on re-run ──"
output=$($IA download "${TEST_ITEMS[0]}" \
    --glob '*.pdf|*_meta.xml' \
    --destdir "$DRIVE1" \
    --checksum 2>&1)

check "reports skipped files" grep -q "skipped" <<< "$output"
check "reports 0 downloaded" grep -q "0 files (0 B)" <<< "$output"

# Clean up for multi-disk tests
rm -rf "$DRIVE1"/*

# ────────────────────────────────────────────────────
echo ""
echo "── Test 3: Multi-drive batch download ──"
$IA download \
    --itemlist "$ITEMS" \
    --glob '*.pdf|*_meta.xml' \
    --destdir "$DRIVE1" \
    --destdir "$DRIVE2" \
    --checksum

d1_count=$(ls -d "$DRIVE1"/*/ 2>/dev/null | wc -l | tr -d ' ')
d2_count=$(ls -d "$DRIVE2"/*/ 2>/dev/null | wc -l | tr -d ' ')
total=$((d1_count + d2_count))

check "items distributed across drives" test "$d1_count" -gt 0 -a "$d2_count" -gt 0
check "all 4 items downloaded" [ "$total" -eq 4 ]

# Record which items are on which drives for resume test
d1_items=$(ls "$DRIVE1")
d2_items=$(ls "$DRIVE2")

# ────────────────────────────────────────────────────
echo ""
echo "── Test 4: Resume — items stay on same drives ──"
# Delete some files to force re-download
first_item=$(ls "$DRIVE1" | head -1)
rm -f "$DRIVE1/$first_item"/*.pdf

output=$($IA download \
    --itemlist "$ITEMS" \
    --glob '*.pdf|*_meta.xml' \
    --destdir "$DRIVE1" \
    --destdir "$DRIVE2" \
    --checksum 2>&1)

d1_items_after=$(ls "$DRIVE1")
d2_items_after=$(ls "$DRIVE2")

check "drive 1 items unchanged" [ "$d1_items" = "$d1_items_after" ]
check "drive 2 items unchanged" [ "$d2_items" = "$d2_items_after" ]
check "reports some skipped" grep -q "skipped" <<< "$output"

# ────────────────────────────────────────────────────
echo ""
echo "── Test 5: Missing destdir error in multi-disk mode ──"
set +e
output=$($IA download \
    --itemlist "$ITEMS" \
    --destdir "$DRIVE1" \
    --destdir "$TESTDIR/NonexistentDrive" \
    --checksum 2>&1)
missing_exit=$?
set -e

check "exits non-zero for missing drive" [ "$missing_exit" -ne 0 ]
check "error mentions 'does not exist'" grep -qi "does not exist" <<< "$output"

# ────────────────────────────────────────────────────
echo ""
echo "── Test 6: download --search with --parameters ──"
output=$($IA download \
    --search 'collection:us-supreme-court AND identifier:micro_IA40386003_0386' \
    --glob '*.pdf|*_meta.xml' \
    --destdir "$DRIVE3" \
    --checksum 2>&1)

check "search download succeeds" test -d "$DRIVE3/micro_IA40386003_0386"
