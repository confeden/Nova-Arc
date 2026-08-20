#!/usr/bin/env bash
# Отвечает на один вопрос: стоит ли целиться в результаты bsc и kanzi.
#
# Числа LTCB для bsc сняты БЛОКОМ В 1000 МБ на 5 ГБ памяти. У nova единица
# сжатия — 32 МиБ, потому что её размер и есть цена правки одного файла.
# BWT очень чувствителен к размеру блока, поэтому сравнивать надо на нашей
# геометрии, а не на их.
set -u
T="${TOOLS:-D:/Programs/compressors}"
W=test/refbench
mkdir -p "$W"
now(){ date +%s.%N; }
took(){ echo "$2 $1" | awk '{printf "%.2f", $1-$2}'; }
say(){ printf "%s|%s|%s|%s|%s\n" "$1" "$2" "$3" "$4" "$5"; }

sweep(){  # sweep <имя> <файл>
  local name="$1" f="$2"
  [ -f "$f" ] || { echo "# $name: НЕТ"; return; }
  local sz; sz=$(stat -c%s "$f")
  echo "# $name $sz Б"

  # bsc: развёртка по размеру блока. -e2 — лучший энтропийный кодер.
  for b in 8 16 32 64 128 999; do
    local t0 t1 t2 t3
    rm -f "$W/s.bsc" "$W/s.out"
    t0=$(now); "$T/bsc.exe" e "$f" "$W/s.bsc" -b$b -e2 >/dev/null 2>&1; t1=$(now)
    [ -f "$W/s.bsc" ] || continue
    t2=$(now); "$T/bsc.exe" d "$W/s.bsc" "$W/s.out" >/dev/null 2>&1; t3=$(now)
    cmp -s "$f" "$W/s.out" || echo "# ВНИМАНИЕ: bsc -b$b не восстановил $name"
    say "$name" "bsc -b$b -e2" "$(stat -c%s "$W/s.bsc")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
    rm -f "$W/s.bsc" "$W/s.out"
  done

  # kanzi: те же уровни, но с ЯВНЫМ размером блока и всеми ядрами.
  # По умолчанию kanzi берёт половину ядер — прошлые замеры шли на четырёх
  # против восьми у nova, что было не в его пользу.
  for spec in "5:BWT+ANS0" "7:BWT+CM" "8:TPAQ" "9:TPAQX"; do
    local lvl="${spec%%:*}" tag="${spec##*:}"
    rm -f "$W/s.knz" "$W/s.out"
    t0=$(now); "$T/Kanzi64.exe" -c -i "$f" -o "$W/s.knz" -l $lvl -b 32m -j 8 -f >/dev/null 2>&1; t1=$(now)
    [ -f "$W/s.knz" ] || continue
    t2=$(now); "$T/Kanzi64.exe" -d -i "$W/s.knz" -o "$W/s.out" -j 8 -f >/dev/null 2>&1; t3=$(now)
    cmp -s "$f" "$W/s.out" || echo "# ВНИМАНИЕ: kanzi -l$lvl не восстановил $name"
    say "$name" "kanzi -l$lvl ($tag) 32m" "$(stat -c%s "$W/s.knz")" "$(took "$t0" "$t1")" "$(took "$t2" "$t3")"
    rm -f "$W/s.knz" "$W/s.out"
  done
}

for a in "$@"; do
  case "$a" in
    enwik8) sweep enwik8 test/enwik8/enwik8 ;;
    corpus) tar -cf "$W/corpus.tar" -C test corpus 2>/dev/null; sweep corpus "$W/corpus.tar" ;;
    silesia) tar -cf "$W/sil.tar" -C test/Silesia-compression-corpus raw 2>/dev/null; sweep silesia "$W/sil.tar" ;;
  esac
done
