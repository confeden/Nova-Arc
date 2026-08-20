"""Typical family-album JPEGs, not gallery panoramas.

The first bench pulled Commons featured pictures at full size — 8 to 84 MB
each, one of them beyond what any JPEG recompressor accepts. A family archive
is made of camera and phone shots of a few megabytes, so this asks Commons for
renderings at ordinary widths instead. They are real JPEGs from a real encoder.
"""
import json, os, sys, time, urllib.parse, urllib.request

sys.stdout.reconfigure(encoding="utf-8", errors="replace")
UA = "NovaPrism-benchmark/0.1 (local compression research)"
OUT = "test/photos"
os.makedirs(OUT, exist_ok=True)


def get(url):
    return urllib.request.urlopen(
        urllib.request.Request(url, headers={"User-Agent": UA}), timeout=90
    ).read()


total = n = 0
for cat, want in (("Featured_pictures_of_birds", 10), ("Featured_pictures_of_landscapes", 10),
                  ("Featured_pictures_of_people", 10), ("Featured_pictures_of_plants", 10),
                  ("Featured_pictures_of_vehicles", 10)):
    q = urllib.parse.urlencode({
        "action": "query", "generator": "categorymembers",
        "gcmtitle": f"Category:{cat}", "gcmlimit": "40", "gcmtype": "file",
        "prop": "imageinfo", "iiprop": "url|size|mime", "iiurlwidth": "2400",
        "format": "json",
    })
    try:
        d = json.loads(get(f"https://commons.wikimedia.org/w/api.php?{q}"))
    except Exception as e:
        print(f"{cat}: {e}")
        continue
    got = 0
    for p in sorted(d.get("query", {}).get("pages", {}).values(), key=lambda p: p.get("title", "")):
        ii = (p.get("imageinfo") or [{}])[0]
        url = ii.get("thumburl") or ii.get("url")
        if ii.get("mime") != "image/jpeg" or not url:
            continue
        name = "".join(c if c.isalnum() or c in "._-" else "_"
                       for c in urllib.parse.unquote(url.rsplit("/", 1)[-1]))[:60]
        if not name.lower().endswith(".jpg"):
            name += ".jpg"
        dest = os.path.join(OUT, name)
        if not os.path.exists(dest):
            try:
                data = get(url)
            except Exception as e:
                print(f"  skip {name}: {e}")
                continue
            open(dest, "wb").write(data)
            time.sleep(2.0)  # Commons rate-limits hard; be a polite client
        total += os.path.getsize(dest)
        n += 1
        got += 1
        if got >= want:
            break
print(f"{n} photos, {total} B = {total/2**20:.1f} MiB")
