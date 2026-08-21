#!/usr/bin/env bash
# Where does the gap actually live?
#
# A corpus-level "+7.9% against zpaqfranz" says nothing about WHAT to fix. This
# runs nova and the two competitors that beat it PER FILE, so the loss can be
# attributed to a data type rather than to a corpus. Every tool sees the same
# single file, so no packaging or solidity difference is in the way.
#
#   test/weakspots.sh                      # Silesia, file by file
#   test/weakspots.sh test/corpus/some/dir # anything else, file by file
set -u
cd "$(dirname "$0")/.."

NOVA=target/release/nova.exe
TOOLS="${TOOLS:-D:/Programs/compressors}"
KANZI="$TOOLS/Kanzi64.exe"
ZPAQ="$TOOLS/zpaqfranz.exe"
SEVENZ="${SEVENZ:-/c/Program Files/7-Zip/7z.exe}"
SRC="${1:-test/Silesia-compression-corpus/raw}"
W=test/weak
mkdir -p "$W"

now() { date +%s.%N; }
took() { echo "$2 $1" | awk '{printf "%.1f", $1-$2}'; }

printf '%-12s %11s %11s %11s %11s %11s   %s\n' \
  file raw nova kanzi-l9 zpaq-m5 7z-mx9 "worst gap"

tot_raw=0; tot_nova=0; tot_kanzi=0; tot_zpaq=0; tot_7z=0
for f in "$SRC"/*; do
  [ -f "$f" ] || continue
  b=$(basename "$f")
  raw=$(stat -c%s "$f")

  rm -rf "$W/x.nva" "$W/out"; "$NOVA" create "$W/x.nva" "$f" -l max --full >/dev/null 2>&1
  n=$(stat -c%s "$W/x.nva" 2>/dev/null || echo 0)

  rm -f "$W/x.knz"; "$KANZI" -c -i "$f" -o "$W/x.knz" -l 9 -j 8 -f >/dev/null 2>&1
  k=$(stat -c%s "$W/x.knz" 2>/dev/null || echo 0)

  rm -f "$W/x.zpaq"; "$ZPAQ" a "$W/x.zpaq" "$f" -m5 -t8 >/dev/null 2>&1
  z=$(stat -c%s "$W/x.zpaq" 2>/dev/null || echo 0)

  rm -f "$W/x.7z"; "$SEVENZ" a -t7z -mx=9 -mmt=8 "$W/x.7z" "$f" >/dev/null 2>&1
  s=$(stat -c%s "$W/x.7z" 2>/dev/null || echo 0)

  # The gap to whichever competitor did best on THIS file.
  best=$k; [ "$z" -gt 0 ] && [ "$z" -lt "$best" ] && best=$z
  [ "$s" -gt 0 ] && [ "$s" -lt "$best" ] && best=$s
  gap=$(awk -v a="$n" -v b="$best" 'BEGIN{printf "%+.1f%%", (a-b)*100.0/b}')

  # Which codec our tournament chose, so a loss can be tied to a decision.
  cod=$("$NOVA" info "$W/x.nva" --units 2>/dev/null | awk 'NR>9{print $4"/"$6}' | sort -u | tr '\n' ' ')

  printf '%-12s %11d %11d %11d %11d %11d   %7s  %s\n' "$b" "$raw" "$n" "$k" "$z" "$s" "$gap" "$cod"
  tot_raw=$((tot_raw+raw)); tot_nova=$((tot_nova+n))
  tot_kanzi=$((tot_kanzi+k)); tot_zpaq=$((tot_zpaq+z)); tot_7z=$((tot_7z+s))
done

echo
printf '%-12s %11d %11d %11d %11d %11d\n' TOTAL "$tot_raw" "$tot_nova" "$tot_kanzi" "$tot_zpaq" "$tot_7z"
awk -v n="$tot_nova" -v k="$tot_kanzi" -v z="$tot_zpaq" -v s="$tot_7z" 'BEGIN{
  printf "per-file totals: vs kanzi %+.1f%%, vs zpaq %+.1f%%, vs 7z %+.1f%%\n",
    (n-k)*100.0/k, (n-z)*100.0/z, (n-s)*100.0/s
}'
rm -rf "$W/x.nva" "$W/x.knz" "$W/x.zpaq" "$W/x.7z" "$W/out"
