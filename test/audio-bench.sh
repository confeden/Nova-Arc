#!/usr/bin/env bash
# Audio recon bench. Two corpora, same question from both sides:
#   test/audio      — real FLAC files, what every archiver stores verbatim today
#   test/audio-wav  — the same music decoded to PCM, what a WAV->FLAC filter sees
set -uo pipefail
cd "$(dirname "$0")/.."

NOVA=target/release/nova.exe
SZ="D:/Programs/7-Zip/7z.exe"
[ -x "$SZ" ] || SZ="C:/Program Files/7-Zip/7z.exe"
ZPAQ="D:/Programs/compressors/zpaqfranz.exe"
KANZI="D:/Programs/compressors/Kanzi64.exe"
W=test/audio-out
mkdir -p "$W"

now() { date +%s.%N; }
took() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.1f", b-a}'; }
row() { printf '%-16s %-14s %14s %9s %9s\n' "$1" "$2" "$3" "$4" "$5"; }

printf '%-16s %-14s %14s %9s %9s\n' corpus tool bytes pack_s unpack_s
for name in audio audio-wav; do
  src="test/$name"
  raw=$(du -sb "$src" | cut -f1)
  row "$name" raw "$raw" - -

  for tier in normal max; do
    rm -f "$W/$name-$tier.nva"; rm -rf "$W/x-$name-$tier"
    t0=$(now); "$NOVA" create "$W/$name-$tier.nva" "$src" -l "$tier" --full >/dev/null 2>&1; t1=$(now)
    t2=$(now); "$NOVA" extract "$W/$name-$tier.nva" -o "$W/x-$name-$tier" --full >/dev/null 2>&1; t3=$(now)
    row "" "nova $tier" "$(stat -c%s "$W/$name-$tier.nva")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
    rm -rf "$W/x-$name-$tier"
  done

  rm -f "$W/$name.7z"
  t0=$(now); "$SZ" a -t7z -mx9 -mmt=8 "$W/$name.7z" "$src" >/dev/null 2>&1; t1=$(now)
  row "" "7z -mx9" "$(stat -c%s "$W/$name.7z")" "$(took "$t0" "$t1")" -

  rm -f "$W/$name.zpaq"
  t0=$(now); "$ZPAQ" a "$W/$name.zpaq" "$src" -m5 -t8 >/dev/null 2>&1; t1=$(now)
  [ -f "$W/$name.zpaq" ] && row "" "zpaqfranz -m5" "$(stat -c%s "$W/$name.zpaq")" "$(took "$t0" "$t1")" -

  rm -f "$W/$name.knz"
  t0=$(now); "$KANZI" -c -i "$src" -o "$W/$name.knz" -l 9 -j 8 -f >/dev/null 2>&1; t1=$(now)
  [ -f "$W/$name.knz" ] && row "" "kanzi -l9" "$(stat -c%s "$W/$name.knz")" "$(took "$t0" "$t1")" -
done
