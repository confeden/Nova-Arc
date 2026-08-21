"""Rebuild the public audio corpus from Wikimedia Commons.

The WAV benchmark is nova's largest claimed win, so it has to be checkable by
someone who is not us. Commons is the only source of openly licensed lossless
music with permanent per-file URLs; it has almost no WAV (a few dozen
pronunciation clips), so the corpus arrives as FLAC and is decoded here.

Two directories come out of this:

    test/audio-pub      the FLAC as downloaded — what every archiver stores
                        verbatim today, and the control for "nobody compresses
                        FLAC, including us"
    test/audio-pub-wav  the same music as PCM — what filter 38 actually sees

Decoding uses this repo's own `flac2wav` example rather than ffmpeg, so the
recipe needs nothing installed. See its header for why that does not flatter
the benchmark.

Commons rate-limits hard: expect this to pause. It resumes, so re-running
after an interruption only fetches what is missing.

    python test/fetch-audio.py
"""

import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
MANIFEST = os.path.join(HERE, "audio-web.json")
FLAC_DIR = os.path.join(ROOT, "test", "audio-pub")
WAV_DIR = os.path.join(ROOT, "test", "audio-pub-wav")
UA = {"User-Agent": "NovaPrism-benchmark/0.1 (github.com/confeden/Nova-Prism)"}


def get(url: str) -> bytes:
    """Download with backoff. Commons answers 429 freely and recovers."""
    for attempt in range(6):
        try:
            req = urllib.request.Request(url, headers=UA)
            return urllib.request.urlopen(req, timeout=900).read()
        except urllib.error.HTTPError as e:
            if e.code != 429 or attempt == 5:
                raise
            wait = 20 * (attempt + 1)
            print(f"    rate-limited, waiting {wait}s", flush=True)
            time.sleep(wait)
    raise SystemExit("gave up: rate-limited six times")


def main() -> int:
    with open(MANIFEST, encoding="utf-8") as f:
        manifest = json.load(f)
    os.makedirs(FLAC_DIR, exist_ok=True)
    os.makedirs(WAV_DIR, exist_ok=True)

    # The decoder is built once, in release: a debug build of claxon turns a
    # few minutes of decoding into a lot more than that.
    print("building flac2wav", flush=True)
    subprocess.run(
        ["cargo", "build", "--release", "-p", "nova-core", "--example", "flac2wav"],
        cwd=ROOT,
        check=True,
    )
    decoder = os.path.join(ROOT, "target", "release", "examples", "flac2wav")
    if os.name == "nt":
        decoder += ".exe"

    bad = 0
    for i, entry in enumerate(manifest, 1):
        flac = os.path.join(FLAC_DIR, entry["name"])
        head = f"[{i}/{len(manifest)}] {entry['name'][:56]}"

        if os.path.exists(flac) and os.path.getsize(flac) == entry["bytes"]:
            data = open(flac, "rb").read()
        else:
            print(f"{head}  {entry['bytes'] / 1048576:.1f} MiB", flush=True)
            data = get(entry["url"])
            with open(flac, "wb") as f:
                f.write(data)
            time.sleep(3)

        # The whole point of the manifest: the bytes are pinned, so a corpus
        # rebuilt elsewhere is the same corpus and the published number means
        # something.
        got = hashlib.sha256(data).hexdigest()
        if got != entry["sha256"]:
            print(f"  SHA-256 MISMATCH: expected {entry['sha256']}, got {got}")
            bad += 1
            continue

        wav = os.path.join(WAV_DIR, os.path.splitext(entry["name"])[0] + ".wav")
        if not os.path.exists(wav):
            subprocess.run([decoder, flac, wav], check=True)

    if bad:
        print(f"\n{bad} file(s) did not match the manifest.")
        return 1

    def total(d):
        return sum(os.path.getsize(os.path.join(d, f)) for f in os.listdir(d))

    print(f"\nFLAC {total(FLAC_DIR):,} B in {FLAC_DIR}")
    print(f"WAV  {total(WAV_DIR):,} B in {WAV_DIR}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
