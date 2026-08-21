#!/usr/bin/env bash
# MP3 bench. The one audio format every archiver, nova included until now,
# stores at 100%: filter 39 separates the frame headers and side info from the
# spectral data, so the ~8% of an MP3 that is structure stops being interleaved
# with the 92% that is noise.
#
# Point it at a directory of .mp3 files:
#     test/mp3-bench.sh test/mp3-pub
#
# The comparison that matters is not "nova vs 7-Zip" — everyone lands at 100%
# — but how much of the structural part each one recovers. Anything at or above
# raw means the split bought nothing.
set -uo pipefail
cd "$(dirname "$0")/.."

SRC="${1:-test/mp3-pub}"
if [ ! -d "$SRC" ]; then
  echo "no corpus at $SRC — pass a directory of .mp3 files" >&2
  exit 1
fi

NOVA=target/release/nova.exe
SZ="D:/Programs/7-Zip/7z.exe"
[ -x "$SZ" ] || SZ="C:/Program Files/7-Zip/7z.exe"
ZPAQ="D:/Programs/compressors/zpaqfranz.exe"
KANZI="D:/Programs/compressors/Kanzi64.exe"
W=test/mp3-out
mkdir -p "$W"

now() { date +%s.%N; }
took() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.1f", b-a}'; }
raw=$(du -sb "$SRC" | cut -f1)
row() { # row <tool> <bytes> <pack_s> <unpack_s>
  awk -v t="$1" -v n="$2" -v p="$3" -v u="$4" -v r="$raw" \
    'BEGIN{printf "%-16s %14s %8.2f%% %9s %9s\n", t, n, n*100/r, p, u}'
}

printf '%-16s %14s %9s %9s %9s\n' tool bytes "of raw" pack_s unpack_s
row raw "$raw" - -

for tier in fast normal max; do
  rm -f "$W/m-$tier.nva"; rm -rf "$W/x-$tier"
  t0=$(now); "$NOVA" create "$W/m-$tier.nva" "$SRC" -l "$tier" --full >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$NOVA" extract "$W/m-$tier.nva" -o "$W/x-$tier" --full >/dev/null 2>&1; t3=$(now)
  row "nova $tier" "$(stat -c%s "$W/m-$tier.nva")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
  # Bit-exactness is not optional and not assumed: diff every file.
  if ! diff -r -q "$SRC" "$W/x-$tier/$(basename "$SRC")" >/dev/null 2>&1; then
    echo "  !! $tier DID NOT ROUND-TRIP" >&2
  fi
  rm -rf "$W/x-$tier"
done

rm -f "$W/m.7z"
t0=$(now); "$SZ" a -t7z -mx9 -mmt=8 "$W/m.7z" "$SRC" >/dev/null 2>&1; t1=$(now)
[ -f "$W/m.7z" ] && row "7z -mx9" "$(stat -c%s "$W/m.7z")" "$(took "$t0" "$t1")" -

rm -f "$W/m.zpaq"
t0=$(now); "$ZPAQ" a "$W/m.zpaq" "$SRC" -m5 -t8 >/dev/null 2>&1; t1=$(now)
[ -f "$W/m.zpaq" ] && row "zpaqfranz -m5" "$(stat -c%s "$W/m.zpaq")" "$(took "$t0" "$t1")" -

rm -f "$W/m.knz"
t0=$(now); "$KANZI" -c -i "$SRC" -o "$W/m.knz" -l 9 -j 8 -f >/dev/null 2>&1; t1=$(now)
[ -f "$W/m.knz" ] && row "kanzi -l9" "$(stat -c%s "$W/m.knz")" "$(took "$t0" "$t1")" -

echo
echo "--- per-unit verdicts (which units the filter actually took) ---"
"$NOVA" info "$W/m-max.nva" --units 2>/dev/null | head -30
