"""Rebuild the public MP3 corpus from the Internet Archive.

Filter 39 is the first thing any archiver does to an MP3 that is not "store it
verbatim", so the number has to be checkable by someone who is not us. All
thirteen files below are public domain or CC0, with permanent per-file URLs and
the Archive's own SHA-1 pinned in `mp3-web.json`.

The corpus is chosen for the one thing filter 39 depends on — how much of a
file is structure rather than spectral data, which is set by the bitrate:

    64 kbps speech (LibriVox)      ~36 of every ~209 bytes is header + side info
    ~160 kbps VBR piano (Goldberg) ~36 of every ~522
    VBR electronic (netlabel)      in between, and a different encoder

Three encoders, three genres, mono and stereo, CBR and VBR. Nothing here is
synthetic and nothing is ours.

    python test/fetch-mp3.py
    test/mp3-bench.sh test/mp3-pub

Sources and licences:
  babysownaesop_2106_librivox  Public Domain Mark 1.0 (LibriVox)
  OpenGoldbergVariations       CC0 1.0 (Kimiko Ishizaka, Open Goldberg project)
  gt009Ffvii-Cold              CC0 1.0 (netlabel release)
"""

import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request

sys.stdout.reconfigure(encoding="utf-8", errors="replace")

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
MANIFEST = os.path.join(HERE, "mp3-web.json")
OUT_DIR = os.path.join(ROOT, "test", "mp3-pub")
UA = {"User-Agent": "NovaPrism-benchmark/0.1 (github.com/confeden/Nova-Prism)"}


def get(url: str) -> bytes:
    """Download with backoff, and retry 500 as hard as 503.

    `archive.org/download/...` is the permanent URL, but it REDIRECTS to
    whichever storage node currently holds the item, and an individual node
    answers 500 often enough to matter — seen here on the first run, on a node
    that a retry did not go back to. So a 500 is transient routing, not a bad
    URL, and treating it as fatal would make the corpus look unfetchable.
    """
    for attempt in range(8):
        try:
            req = urllib.request.Request(url, headers=UA)
            return urllib.request.urlopen(req, timeout=900).read()
        except urllib.error.HTTPError as e:
            if e.code not in (429, 500, 502, 503) or attempt == 7:
                raise
            wait = 10 * (attempt + 1)
            print(f"    node said {e.code}, waiting {wait}s", flush=True)
            time.sleep(wait)
    raise SystemExit("gave up: every storage node was busy")


def main() -> int:
    with open(MANIFEST, encoding="utf-8") as f:
        manifest = json.load(f)
    os.makedirs(OUT_DIR, exist_ok=True)

    bad = 0
    for i, entry in enumerate(manifest, 1):
        path = os.path.join(OUT_DIR, entry["name"])
        head = f"[{i}/{len(manifest)}] {entry['name']}"

        if os.path.exists(path) and os.path.getsize(path) == entry["bytes"]:
            data = open(path, "rb").read()
        else:
            print(f"{head}  {entry['bytes'] / 1048576:.1f} MiB  {entry['what']}", flush=True)
            data = get(entry["url"])
            with open(path, "wb") as f:
                f.write(data)
            time.sleep(2)

        # The whole point of the manifest: the bytes are pinned, so a corpus
        # rebuilt elsewhere is the same corpus and a published number means
        # something. SHA-1 because that is what the Archive publishes — it is
        # the SOURCE's own checksum, not one we computed after the fact.
        got = hashlib.sha1(data).hexdigest()
        if got != entry["sha1"]:
            print(f"  SHA-1 MISMATCH: expected {entry['sha1']}, got {got}")
            bad += 1

    if bad:
        print(f"\n{bad} file(s) did not match the manifest.")
        return 1

    total = sum(os.path.getsize(os.path.join(OUT_DIR, f)) for f in os.listdir(OUT_DIR))
    print(f"\n{total:,} B in {OUT_DIR}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
