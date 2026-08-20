#!/usr/bin/env bash
# Как каждый архиватор ведёт себя от 1 потока к 8.
#
# Проверяются ДВЕ вещи, и вторая важнее первой:
#   1. ускорение по времени;
#   2. МЕНЯЕТСЯ ЛИ РАЗМЕР от числа потоков. Многие архиваторы распараллеливают
#      тем, что режут данные на более мелкие независимые блоки — тогда каждый
#      добавленный поток стоит степени сжатия. У nova размер единицы задан
#      геометрией архива и от числа потоков не зависит, так что размер обязан
#      быть БАЙТ В БАЙТ одинаковым. Если это не так — это ошибка.
set -u
NOVA=target/release/nova.exe
SEVENZ="${SEVENZ:-/c/Program Files/7-Zip/7z.exe}"
T="${TOOLS:-D:/Programs/compressors}"
W=test/refbench
mkdir -p "$W"
now(){ date +%s.%N; }
took(){ echo "$2 $1" | awk '{printf "%.2f", $1-$2}'; }

SRC=${1:-test/Silesia-compression-corpus/raw}
TAR="$W/scale.tar"
[ -f "$TAR" ] || tar -cf "$TAR" -C "$(dirname "$SRC")" "$(basename "$SRC")" 2>/dev/null
echo "# корпус $SRC ($(stat -c%s "$TAR") Б в tar)"

for j in 1 2 4 8; do
  rm -f "$W/sc.nva"
  t0=$(now); "$NOVA" create "$W/sc.nva" "$SRC" -l max -j $j >/dev/null 2>&1; t1=$(now)
  printf "nova max|%s|%s|%s\n" "$j" "$(stat -c%s "$W/sc.nva")" "$(took "$t0" "$t1")"
done
rm -f "$W/sc.nva"

for j in 1 2 4 8; do
  rm -f "$W/sc.7z"
  t0=$(now); "$SEVENZ" a -t7z -mx=9 -mmt=$j "$W/sc.7z" "$SRC" >/dev/null 2>&1; t1=$(now)
  printf "7z -mx9|%s|%s|%s\n" "$j" "$(stat -c%s "$W/sc.7z")" "$(took "$t0" "$t1")"
done
rm -f "$W/sc.7z"

for j in 1 2 4 8; do
  rm -f "$W/sc.knz"
  t0=$(now); "$T/Kanzi64.exe" -c -i "$TAR" -o "$W/sc.knz" -l 9 -b 32m -j $j -f >/dev/null 2>&1; t1=$(now)
  printf "kanzi -l9|%s|%s|%s\n" "$j" "$(stat -c%s "$W/sc.knz")" "$(took "$t0" "$t1")"
done
rm -f "$W/sc.knz"

for j in 1 2 4 8; do
  rm -f "$W/sc.xz"
  t0=$(now); xz -9e -T$j -c "$TAR" > "$W/sc.xz" 2>/dev/null; t1=$(now)
  printf "xz -9e|%s|%s|%s\n" "$j" "$(stat -c%s "$W/sc.xz")" "$(took "$t0" "$t1")"
done
rm -f "$W/sc.xz" "$TAR"
