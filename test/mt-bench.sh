#!/usr/bin/env bash
# Multi-core wall clock, which is the metric that counts (D12).
#
# Single-core numbers are a weak-machine sanity check, not the benchmark: on one
# core we may be slower than the competition and that is accepted. On a modern
# machine we want to be AHEAD. So this measures wall clock with every tool given
# all the cores it will take, and reports ratio beside it — a speed win paid for
# in bytes is not a win.
#
#   test/mt-bench.sh silesia
#   test/mt-bench.sh enwik8 silesia corpus
set -u
cd "$(dirname "$0")/.."

NOVA=target/release/nova.exe
TOOLS="${TOOLS:-D:/Programs/compressors}"
KANZI="$TOOLS/Kanzi64.exe"
ZPAQ="$TOOLS/zpaqfranz.exe"
SEVENZ="${SEVENZ:-/c/Program Files/7-Zip/7z.exe}"
J="${J:-$(nproc 2>/dev/null || echo 8)}"
W=test/mtbench
mkdir -p "$W"

now() { date +%s.%N; }
took() { echo "$2 $1" | awk '{printf "%.1f", $1-$2}'; }

run() { # run <label> <bytes> <comp_s> <dec_s> <raw>
  awk -v l="$1" -v b="$2" -v c="$3" -v d="$4" -v r="$5" 'BEGIN{
    printf "  %-16s %12d  %6.2f%%  %8s s  %8s s\n", l, b, b*100.0/r, c, d
  }'
}

for name in "$@"; do
  case "$name" in
    enwik8)  src=test/enwik8/enwik8 ;;
    silesia) src=test/Silesia-compression-corpus/raw ;;
    corpus)  src=test/corpus ;;
    firefox) src=test/firefox ;;
    precomp) src=test/precomp-web ;;
    *)       src="$name" ;;
  esac
  [ -e "$src" ] || { echo "$name: missing ($src)"; continue; }
  raw=$(du -sb "$src" | awk '{print $1}')
  # Stream compressors need a single file; the tar overhead is charged to them,
  # which is the real cost of using one as an archiver.
  if [ -d "$src" ]; then
    stream="$W/$name.tar"; [ -f "$stream" ] || tar -cf "$stream" -C "$(dirname "$src")" "$(basename "$src")" 2>/dev/null
  else
    stream="$src"
  fi

  echo "== $name — $raw B, $J threads =="
  printf "  %-16s %12s  %7s  %10s  %10s\n" tool bytes "of raw" compress extract

  rm -rf "$W/a.nva" "$W/out"
  t0=$(now); "$NOVA" create "$W/a.nva" "$src" -l max -j "$J" --full >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$NOVA" extract "$W/a.nva" -o "$W/out" -j "$J" --full >/dev/null 2>&1; t3=$(now)
  run "nova max" "$(stat -c%s "$W/a.nva")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")" "$raw"
  rm -rf "$W/out"

  rm -rf "$W/a.7z" "$W/out"
  t0=$(now); "$SEVENZ" a -t7z -mx=9 -mmt="$J" "$W/a.7z" "$src" >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$SEVENZ" x -y -mmt="$J" -o"$W/out" "$W/a.7z" >/dev/null 2>&1; t3=$(now)
  run "7z -mx9" "$(stat -c%s "$W/a.7z")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")" "$raw"
  rm -rf "$W/out"

  # kanzi's default -j is HALF the cores, so it must be given the count
  # explicitly or the comparison silently hands it four threads against our
  # eight (recorded in the roadmap as a bench trap).
  rm -f "$W/a.knz" "$W/a.dec"
  t0=$(now); "$KANZI" -c -i "$stream" -o "$W/a.knz" -l 9 -j "$J" -f >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$KANZI" -d -i "$W/a.knz" -o "$W/a.dec" -j "$J" -f >/dev/null 2>&1; t3=$(now)
  run "kanzi -l9" "$(stat -c%s "$W/a.knz")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")" "$raw"
  rm -f "$W/a.knz" "$W/a.dec"

  # The trailing slash on -to is load-bearing; without it zpaqfranz refuses and
  # the "decode time" is the refusal.
  rm -rf "$W/a.zpaq" "$W/out"
  t0=$(now); "$ZPAQ" a "$W/a.zpaq" "$src" -m5 -t"$J" >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$ZPAQ" x "$W/a.zpaq" -to "$W/out/" -t"$J" >/dev/null 2>&1; t3=$(now)
  run "zpaqfranz -m5" "$(stat -c%s "$W/a.zpaq")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")" "$raw"
  rm -rf "$W/a.zpaq" "$W/out"

  rm -f "$W/a.nva"
  [ -d "$src" ] && rm -f "$stream"
  echo
done
