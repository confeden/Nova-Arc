#!/usr/bin/env bash
# Price a decode-only nova, per feature and per profile (doc 17 §3).
#
# Builds the SAME binary once per feature set, with the settings a shipped SFX
# stub would use (opt-level=z, fat LTO, panic=abort, stripped), and prints the
# stripped size. `floor` is a Rust std binary with no decoder in it, so every
# other row minus floor is what that decoder really costs.
set -u
cd "$(dirname "$0")/size-probe" || exit 1

TARGET=target/release/size-probe.exe
[ -e "$TARGET" ] || TARGET=target/release/size-probe

row() { # row <label> <features...>
    local label="$1"; shift
    local feats="$*"
    if [ -z "$feats" ]; then
        cargo build --release --quiet 2>/dev/null
    else
        cargo build --release --quiet --features "$feats" 2>/dev/null
    fi
    if [ ! -e "$TARGET" ]; then
        printf '%-12s BUILD FAILED\n' "$label"
        return
    fi
    local n
    n=$(stat -c %s "$TARGET")
    printf '%-12s %10d  %7.1f KiB\n' "$label" "$n" "$(echo "$n" | awk '{print $1/1024}')"
    rm -f "$TARGET"
}

echo "=== per feature (subtract floor for the decoder's own cost) ==="
row floor
row lzma      f-lzma
row ppmd      f-ppmd
row zstd      f-zstd
row bsc       f-bsc
row preflate  f-preflate
row lepton    f-lepton
row claxon    f-claxon
row container f-container

echo
echo "=== profiles ==="
row core   p-core
row media  p-media
row max    p-max

echo
echo "=== ceiling: nova-core as it stands, encoders and all, no CLI ==="
row engine f-engine
