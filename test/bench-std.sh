#!/usr/bin/env bash
# Benchmark on STANDARD corpora, so the numbers can be put next to published
# ones (LTCB, Silesia). Every tool runs on this machine, so the times are
# comparable with each other; sizes are comparable with anyone's.
#
#   bash test/bench-std.sh enwik8 silesia ...
#
# Archivers (nova, 7z, zpaqfranz) take the input directly. Stream compressors
# (xz, brotli, kanzi, paq8px) take a tar when the input is a directory — the tar
# header overhead is charged to them, which is the real cost of using a stream
# compressor as an archiver.
set -u
NOVA=target/release/nova.exe
SEVENZ="${SEVENZ:-/c/Program Files/7-Zip/7z.exe}"
# Where the competitor binaries live. Override to run this anywhere else.
TOOLS="${TOOLS:-D:/Programs/compressors}"
KANZI="$TOOLS/Kanzi64.exe"
ZPAQ="$TOOLS/zpaqfranz.exe"
PAQ="$TOOLS/paq8px.exe"
W=test/refbench
mkdir -p "$W"

now() { date +%s.%N; }
took() { echo "$2 $1" | awk '{printf "%.2f", $1-$2}'; }
say() { printf "%s|%s|%s|%s|%s\n" "$1" "$2" "$3" "$4" "$5"; }   # corpus|tool|bytes|comp_s|dec_s

bench() {
  local name="$1" src="$2" want="${3:-all}"
  [ -e "$src" ] || { echo "# $name: НЕТ ($src)"; return; }
  local raw stream t0 t1 t2 t3 sz
  if [ -d "$src" ]; then
    raw=$(du -sb "$src" | awk '{print $1}')
    stream="$W/$name.tar"
    [ -f "$stream" ] || tar -cf "$stream" -C "$(dirname "$src")" "$(basename "$src")" 2>/dev/null
  else
    raw=$(stat -c%s "$src")
    stream="$src"
  fi
  echo "# $name raw=$raw stream=$(stat -c%s "$stream")"

  # --- nova max -------------------------------------------------------------
  rm -rf "$W/$name.nva" "$W/$name.out"
  t0=$(now); "$NOVA" create "$W/$name.nva" "$src" -l max >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$NOVA" extract "$W/$name.nva" -o "$W/$name.out" >/dev/null 2>&1; t3=$(now)
  say "$name" "nova max" "$(stat -c%s "$W/$name.nva")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
  rm -rf "$W/$name.out"

  # --- 7-Zip ----------------------------------------------------------------
  rm -rf "$W/$name.7z" "$W/$name.out"
  t0=$(now); "$SEVENZ" a -t7z -mx=9 "$W/$name.7z" "$src" >/dev/null 2>&1; t1=$(now)
  t2=$(now); "$SEVENZ" x -y -o"$W/$name.out" "$W/$name.7z" >/dev/null 2>&1; t3=$(now)
  say "$name" "7z -mx9" "$(stat -c%s "$W/$name.7z")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
  rm -rf "$W/$name.out"

  # --- xz -------------------------------------------------------------------
  rm -f "$W/$name.xz"
  t0=$(now); xz -9e -T0 -c "$stream" > "$W/$name.xz" 2>/dev/null; t1=$(now)
  t2=$(now); xz -d -T0 -c "$W/$name.xz" > /dev/null 2>&1; t3=$(now)
  say "$name" "xz -9e" "$(stat -c%s "$W/$name.xz")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
  rm -f "$W/$name.xz"

  # --- kanzi (context mixing, the speed band nova can afford) ---------------
  # Decoded to a real file: these are native Windows binaries and do not know
  # what /dev/null is — pointed at it they fail instantly and the "decode time"
  # is the time to print an error.
  local levels="7 9"; [ "$want" = "big" ] && levels="9"
  for lvl in $levels; do
    rm -f "$W/$name.knz" "$W/$name.dec"
    t0=$(now); "$KANZI" -c -i "$stream" -o "$W/$name.knz" -l $lvl -f >/dev/null 2>&1; t1=$(now)
    t2=$(now); "$KANZI" -d -i "$W/$name.knz" -o "$W/$name.dec" -f >/dev/null 2>&1; t3=$(now)
    cmp -s "$stream" "$W/$name.dec" || echo "# ВНИМАНИЕ: kanzi -l$lvl не восстановил $name"
    say "$name" "kanzi -l$lvl" "$(stat -c%s "$W/$name.knz")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
    rm -f "$W/$name.knz" "$W/$name.dec"
  done

  # --- zpaqfranz ------------------------------------------------------------
  local methods="4 5"; [ "$want" = "big" ] && methods="5"
  for m in $methods; do
    rm -rf "$W/$name-m$m.zpaq" "$W/$name.out"
    t0=$(now); "$ZPAQ" a "$W/$name-m$m.zpaq" "$src" -m$m >/dev/null 2>&1; t1=$(now)
    # The trailing slash is load-bearing: without it zpaqfranz reads -to as a
    # single output FILE, refuses, and the "decode time" is the refusal.
    t2=$(now); "$ZPAQ" x "$W/$name-m$m.zpaq" -to "$W/$name.out/" >/dev/null 2>&1; t3=$(now)
    local got; got=$(du -sb "$W/$name.out" 2>/dev/null | awk '{print $1}')
    [ "${got:-0}" -gt 0 ] || echo "# ВНИМАНИЕ: zpaqfranz -m$m ничего не распаковал для $name"
    say "$name" "zpaqfranz -m$m" "$(stat -c%s "$W/$name-m$m.zpaq")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
    rm -rf "$W/$name-m$m.zpaq" "$W/$name.out"
  done

  # --- brotli (only where it will not take an hour) -------------------------
  if [ "$want" != "big" ]; then
    rm -f "$W/$name.br"
    t0=$(now); brotli -q 11 -c "$stream" > "$W/$name.br" 2>/dev/null; t1=$(now)
    t2=$(now); brotli -d -c "$W/$name.br" > "$W/$name.dec" 2>&1; t3=$(now); rm -f "$W/$name.dec"
    say "$name" "brotli -q11" "$(stat -c%s "$W/$name.br")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
    rm -f "$W/$name.br"
  fi

  [ -d "$src" ] && rm -f "$stream"
  return 0
}

for a in "$@"; do
  case "$a" in
    enwik8)  bench enwik8  test/enwik8/enwik8 ;;
    enwik9)  bench enwik9  test/enwik9/enwik9 big ;;
    silesia) bench silesia test/Silesia-compression-corpus/raw ;;
    corpus)  bench corpus  test/corpus ;;
    pdfs)    bench pdfs    test/pdfs ;;
    photos)  bench photos  test/photos ;;
    precomp) bench precomp test/precomp ;;
  esac
done
