"""Build the already-compressed corpus from files anyone can download.

The old `test/precomp` was assembled from whatever was on this disk, so its
numbers could not be checked by anyone else. Every entry here has a permanent,
versioned URL and a recorded SHA-256, so the corpus can be rebuilt byte for
byte and the benchmark repeated.

  python test/fetch-precomp.py --probe    # sizes only, downloads nothing
  python test/fetch-precomp.py            # download into test/precomp-web
"""

import hashlib
import json
import os
import sys
import urllib.request

ROOT = "test/precomp-web"
UA = "NovaPrism-benchmark/0.1 (compression research; contact: repo owner)"

# name, url, what it is
SOURCES = [
    (
        "python-3.12.0-docs-html.zip",
        "https://www.python.org/ftp/python/doc/3.12.0/python-3.12.0-docs-html.zip",
        "zip of the Python 3.12.0 HTML documentation",
    ),
    (
        "binutils-2.42.tar.gz",
        "https://ftp.gnu.org/gnu/binutils/binutils-2.42.tar.gz",
        "gzip of a source tree",
    ),
    (
        "commons-lang3-3.14.0.jar",
        "https://repo1.maven.org/maven2/org/apache/commons/commons-lang3/3.14.0/commons-lang3-3.14.0.jar",
        "zip of compiled Java classes",
    ),
    (
        "pg2701-images-3.epub",
        "https://www.gutenberg.org/cache/epub/2701/pg2701-images-3.epub",
        "zip of XHTML and images (Moby-Dick)",
    ),
    (
        "org.fdroid.fdroid.apk",
        "https://f-droid.org/repo/org.fdroid.fdroid_1021051.apk",
        "zip of mixed application content",
    ),
]

# The Kodak image suite: 24 photographs, PNG, the set image-compression papers
# have quoted since the 1990s. PNG is deflate, so this is the image half of the
# corpus — and unlike a folder of pictures off this disk, anyone can fetch the
# identical bytes and repeat the measurement.
PNGS = [
    f"https://r0k.us/graphics/kodak/kodak/kodim{i:02d}.png" for i in range(1, 25)
]


def req(url, method="GET"):
    return urllib.request.Request(url, headers={"User-Agent": UA}, method=method)


def head(url):
    try:
        with urllib.request.urlopen(req(url, "HEAD"), timeout=30) as r:
            return int(r.headers.get("Content-Length") or 0), r.status
    except Exception as e:  # a dead link must not stop the report
        return 0, str(e)


def get(url, dest):
    with urllib.request.urlopen(req(url), timeout=180) as r:
        data = r.read()
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    with open(dest, "wb") as f:
        f.write(data)
    return data


def main():
    items = [(os.path.basename(u.split("/")[-1]), u, "PNG image") for u in PNGS]
    items = SOURCES + items
    probe = "--probe" in sys.argv

    total = 0
    manifest = []
    for name, url, what in items:
        if probe:
            n, status = head(url)
            total += n
            print(f"{n:>12,}  {status}  {name}  ({what})")
            continue
        dest = os.path.join(ROOT, name)
        if os.path.exists(dest):
            data = open(dest, "rb").read()
        else:
            print(f"  fetching {name} ...", flush=True)
            data = get(url, dest)
        total += len(data)
        manifest.append(
            {
                "name": name,
                "url": url,
                "what": what,
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
            }
        )
        print(f"{len(data):>12,}  {name}")

    print(f"{total:>12,}  TOTAL")
    if not probe:
        # Beside the corpus, never inside it: every tool must see exactly the
        # bytes the manifest describes and nothing else.
        out = ROOT + ".json"
        with open(out, "w", encoding="utf-8") as f:
            json.dump(manifest, f, indent=2, ensure_ascii=False)
        print("wrote", out)


if __name__ == "__main__":
    main()
