#!/usr/bin/env python3
"""Copies dist/<version>/ into web/downloads/<version>/ and writes
web/downloads/latest.json, which the download section of index.html renders.

    web/tools/publish-release.py           # version from Cargo.toml
    web/tools/publish-release.py 0.1.3     # another dist/<version>/

The file list is dist/<version>/SHA256SUMS, the same list packaging/release.sh
publishes; every listed file is checked against its checksum before it is
copied. Old versions stay in web/downloads/ so pinned links keep working.
web/downloads/ is git-ignored: the packages are ~73 MB per version.
"""
import argparse
import hashlib
import json
import re
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
DOWNLOADS = ROOT / "web" / "downloads"

# (pattern, kind, os, format). First match wins; {v} is the version.
CLASSES = [
    (r"icu-{v}-windows-x86_64-setup\.exe", "client", "windows", "setup"),
    (r"icu-{v}-windows-x86_64\.zip", "client", "windows", "zip"),
    (r"icu(?:-client)?-{v}-\d+\.fc(\d+)\.x86_64\.rpm", "client", "fedora", "rpm"),
    (r"icu_{v}-\d+\+deb(\d+)_amd64\.deb", "client", "debian", "deb"),
    (r"icu-server-{v}-\d+\.x86_64\.rpm", "server", "linux-rpm", "rpm"),
    (r"icu-server_{v}-\d+_amd64\.deb", "server", "linux-deb", "deb"),
]


def workspace_version() -> str:
    """The same lookup as packaging/release.sh: [workspace.package] version."""
    in_section = False
    for line in (ROOT / "Cargo.toml").read_text().splitlines():
        if line.startswith("["):
            in_section = line.strip() == "[workspace.package]"
        elif in_section:
            m = re.match(r'version\s*=\s*"(.*)"', line)
            if m:
                return m.group(1)
    sys.exit("publish-release: no [workspace.package] version in Cargo.toml")


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def classify(name: str, version: str) -> dict:
    v = re.escape(version)
    for pattern, kind, os_, fmt in CLASSES:
        m = re.fullmatch(pattern.format(v=v), name)
        if m:
            return {"kind": kind, "os": os_ + (m.group(1) if m.groups() else ""), "format": fmt}
    sys.exit(f"publish-release: {name} matches no known package name; extend CLASSES")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("version", nargs="?", default=workspace_version())
    version = ap.parse_args().version

    src = ROOT / "dist" / version
    sums = src / "SHA256SUMS"
    if not sums.is_file():
        sys.exit(f"publish-release: {sums} missing, run packaging/build.sh")

    listed = [l.split(None, 1) for l in sums.read_text().splitlines() if l.strip()]
    dest = DOWNLOADS / version
    dest.mkdir(parents=True, exist_ok=True)

    files, dirty, commit = [], False, None
    for digest, name in listed:
        name = name.strip().lstrip("*")
        path = src / name
        if sha256(path) != digest:
            sys.exit(f"publish-release: {name} does not match SHA256SUMS")
        if name.startswith("BUILDINFO."):
            for line in path.read_text().splitlines():
                if line == "DIRTY":
                    dirty = True
                elif line.startswith("commit ") and (name == "BUILDINFO.linux" or commit is None):
                    commit = line.split()[1]
        else:
            files.append({"name": name, "size": path.stat().st_size, "sha256": digest,
                          **classify(name, version)})
        if not (dest / name).is_file() or sha256(dest / name) != digest:
            shutil.copy2(path, dest / name)

    extras = ["SHA256SUMS"] + [p.name for p in sorted(src.glob("SHA256SUMS.sig"))]
    for name in extras:
        shutil.copy2(src / name, dest / name)

    manifest = {
        "version": version,
        "published": datetime.fromtimestamp(sums.stat().st_mtime, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "commit": commit,
        "dirty": dirty,
        "base": f"downloads/{version}/",
        "sums": "SHA256SUMS",
        "sig": "SHA256SUMS.sig" if "SHA256SUMS.sig" in extras else None,
        "files": files,
    }
    tmp = DOWNLOADS / "latest.json.tmp"
    tmp.write_text(json.dumps(manifest, indent=2) + "\n")
    tmp.replace(DOWNLOADS / "latest.json")
    print(f"published {version}: {len(files)} packages -> {dest.relative_to(ROOT)}"
          + ("  (BUILT FROM A DIRTY TREE)" if dirty else ""))


if __name__ == "__main__":
    main()
