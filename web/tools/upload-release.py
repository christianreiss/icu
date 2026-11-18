#!/usr/bin/env python3
"""Upload a signed release from dist/<version>/ to the ICU release API.

    ICU_RELEASE_API_TOKEN=... web/tools/upload-release.py [version]
        [--api https://misc.alpha-labs.net/icu/api/v1] [--notes notes.txt] [--no-latest]

Run web/tools/publish-release.py --prepare-only first: it signs SHA256SUMS and
latest.json locally. This tool only transports those bytes; the server
re-verifies both signatures, every checksum and the downgrade rule before it
switches downloads/latest.json, which it does last.
"""
import argparse
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
DEFAULT_API = "https://misc.alpha-labs.net/icu/api/v1"


def workspace_version() -> str:
    in_section = False
    for line in (ROOT / "Cargo.toml").read_text().splitlines():
        if line.startswith("["):
            in_section = line.strip() == "[workspace.package]"
        elif in_section and (m := re.match(r'version\s*=\s*"(.*)"', line)):
            return m.group(1)
    sys.exit("upload-release: no [workspace.package] version in Cargo.toml")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


OPENER = urllib.request.build_opener(NoRedirect)


def call(method: str, url: str, token: str, body=None, length=None, ctype="application/json") -> dict:
    req = urllib.request.Request(url, data=body, method=method)
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Content-Type", ctype)
    if length is not None:
        req.add_header("Content-Length", str(length))
    try:
        with OPENER.open(req, timeout=600) as resp:
            return json.load(resp)
    except urllib.error.HTTPError as error:
        try:
            detail = json.load(error).get("error", "")
        except ValueError:
            detail = ""
        raise SystemExit(f"upload-release: {method} {url}: HTTP {error.code} {detail}".rstrip())


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("version", nargs="?", default=None)
    ap.add_argument("--api", default=os.environ.get("ICU_RELEASE_API", DEFAULT_API))
    ap.add_argument("--dist", type=Path, default=None, help="release directory (default dist/<version>)")
    ap.add_argument("--notes", type=Path, help="plain-text 'Neu in' notes for the page")
    ap.add_argument("--no-latest", action="store_true", help="publish the version without switching latest.json")
    args = ap.parse_args()
    version = args.version or workspace_version()
    src = args.dist or ROOT / "dist" / version
    token = os.environ.get("ICU_RELEASE_API_TOKEN", "")
    if not token:
        sys.exit("upload-release: set ICU_RELEASE_API_TOKEN (fleet secret icu.release-api-token)")
    api = args.api.rstrip("/")
    if not api.startswith("https://") and not api.startswith("http://127.0.0.1"):
        sys.exit("upload-release: the API must be https")

    names = [line.split(maxsplit=1)[1].lstrip("*") for line in (src / "SHA256SUMS").read_text().splitlines()]
    files = names + ["SHA256SUMS", "SHA256SUMS.sig", "latest.json"]
    for name in files:
        if not (src / name).is_file():
            sys.exit(f"upload-release: {src / name} missing; run publish-release.py --prepare-only")

    upload = call("POST", f"{api}/uploads", token, json.dumps({"version": version}).encode())["id"]
    for name in files:
        path = src / name
        size = path.stat().st_size
        with path.open("rb") as body:
            got = call("PUT", f"{api}/uploads/{upload}/{urllib.parse.quote(name)}", token,
                       body, size, "application/octet-stream")
        if got.get("size") != size or got.get("sha256") != sha256(path):
            call("DELETE", f"{api}/uploads/{upload}", token)
            sys.exit(f"upload-release: {name} arrived damaged; upload discarded")
        print(f"  {name} ({size} bytes)")
    result = call("POST", f"{api}/uploads/{upload}/commit", token,
                  json.dumps({"latest": not args.no_latest}).encode())
    if args.notes:
        call("PUT", f"{api}/releases/{version}/notes", token, args.notes.read_bytes(), ctype="text/plain; charset=utf-8")
    print(f"published {result['version']}: {len(result['files'])} packages"
          + ("" if args.no_latest else ", latest.json switched"))


if __name__ == "__main__":
    main()
