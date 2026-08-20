#!/usr/bin/env bash
# The already-compressed corpus, rebuilt from files anyone can download.
# Sources and SHA-256 in test/precomp-web.json (test/fetch-precomp.py).
set -uo pipefail
cd "$(dirname "$0")/.."

SRC=test/precomp-web
W=test/precomp-out
mkdir -p "$W"
NOVA=target/release/nova.exe
SZ="${SEVENZ:-C:/Program Files/7-Zip/7z.exe}"
CMP="${TOOLS:-D:/Programs/compressors}"
XZ=$(command -v xz)
BR=$(command -v brotli)

# The manifest is ours, not part of the corpus.
RAW=$(find "$SRC" -type f ! -name SOURCES.json -printf '%s\n' | awk '{s+=$1} END{print s}')
now() { date +%s.%N; }
took() { awk -v a="$1" -v b="$2" 'BEGIN{printf "%.2f", b-a}'; }
row() { printf '%-16s %13s %7s %9s %9s\n' "$1" "$2" "$3" "$4" "$5"; }

printf '%-16s %13s %7s %9s %9s\n' tool bytes '%raw' pack_s unpack_s
row raw "$RAW" 100.0 - -

for tier in fast normal max; do
  rm -f "$W/n-$tier.nva"; rm -rf "$W/x"
  t0=$(now); "$NOVA" create "$W/n-$tier.nva" "$SRC" -l "$tier" --full >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$NOVA" extract "$W/n-$tier.nva" -o "$W/x" --full >/dev/null 2>&1; t3=$(now)
  b=$(stat -c%s "$W/n-$tier.nva")
  row "nova $tier" "$b" "$(awk -v b="$b" -v r="$RAW" 'BEGIN{printf "%.1f",100*b/r}')" \
      "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
  rm -rf "$W/x"
done

# One tar so the stream compressors see the same bytes as the archivers.
[ -f "$W/all.tar" ] || tar -cf "$W/all.tar" -C "$SRC" .

bench_arc() { # name, output, command...
  local name=$1 out=$2; shift 2
  rm -f "$out"
  local t0 t1; t0=$(now); "$@" >/dev/null 2>&1; t1=$(now)
  [ -f "$out" ] && row "$name" "$(stat -c%s "$out")" \
    "$(awk -v b="$(stat -c%s "$out")" -v r="$RAW" 'BEGIN{printf "%.1f",100*b/r}')" \
    "$(took "$t0" "$t1")" -
}

bench_arc "7z -mx9"       "$W/a.7z"    "$SZ" a -t7z -mx9 -mmt=8 "$W/a.7z" "$SRC"
bench_arc "zpaqfranz -m5" "$W/a.zpaq"  "$CMP/zpaqfranz.exe" a "$W/a.zpaq" "$SRC" -m5 -t8
bench_arc "zpaqfranz -m4" "$W/a4.zpaq" "$CMP/zpaqfranz.exe" a "$W/a4.zpaq" "$SRC" -m4 -t8
# kanzi writes one output per input file, so like xz and brotli it gets the tar.
bench_arc "kanzi -l9"     "$W/all.tar.knz"  "$CMP/Kanzi64.exe" -c -i "$W/all.tar" -o "$W/all.tar.knz" -l 9 -j 8 -f
bench_arc "kanzi -l7"     "$W/all7.tar.knz" "$CMP/Kanzi64.exe" -c -i "$W/all.tar" -o "$W/all7.tar.knz" -l 7 -j 8 -f
[ -n "$XZ" ] && bench_arc "xz -9e"     "$W/all.tar.xz" "$XZ" -9e -k -f -T8 "$W/all.tar"
[ -n "$BR" ] && bench_arc "brotli -q11" "$W/all.tar.br" "$BR" -q 11 -f -o "$W/all.tar.br" "$W/all.tar"
