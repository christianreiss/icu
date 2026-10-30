#!/usr/bin/env python3
"""Authenticate a complete, clean stable release and publish its channel last.

    web/tools/publish-release.py                    # sign and publish locally
    web/tools/publish-release.py --prepare-only     # sign dist/<version> only

The legacy latest.json fields remain available to the website. The Windows
client authenticates the exact SSHSIG-signed payload with its embedded key.
ICU_RELEASE_KEY defaults to ~/.ssh/icu-release; no private key belongs in Git.
"""
import argparse
import base64
import fcntl
import os
import subprocess
import tempfile
import hashlib
import json
import re
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
    raise ValueError(f"{name} matches no known package name; extend CLASSES")


REQUIRED = {
    ("client", "windows", "zip"), ("client", "windows", "setup"),
    ("client", "fedora42", "rpm"), ("client", "fedora43", "rpm"),
    ("client", "debian13", "deb"), ("server", "linux-rpm", "rpm"),
    ("server", "linux-deb", "deb"),
}


def stable_version(version: str) -> tuple:
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version):
        raise ValueError("only stable major.minor.patch versions may be published")
    return tuple(map(int, version.split(".")))


def release_manifest(src: Path, version: str) -> tuple[dict, list[str]]:
    """Validate the entire release before changing a channel or signing bytes."""
    stable_version(version)
    sums = src / "SHA256SUMS"
    files, names, commits, infos = [], set(), set(), set()
    for line in sums.read_text().splitlines():
        match = re.fullmatch(r"([a-f0-9]{64}) [ *]([A-Za-z0-9][A-Za-z0-9_.+\-]{0,179})", line)
        if not match:
            raise ValueError("malformed checksum line or unsafe artifact path")
        digest, name = match.groups()
        if name in names:
            raise ValueError(f"duplicate artifact: {name}")
        names.add(name)
        path = src / name
        if path.is_symlink() or not path.is_file() or sha256(path) != digest:
            raise ValueError(f"{name} is missing, a symlink, or does not match SHA256SUMS")
        if name.startswith("BUILDINFO."):
            lines = path.read_text().splitlines()
            if "DIRTY" in lines:
                raise ValueError("a dirty build must never be published as an update")
            ids = [line[7:] for line in lines if line.startswith("commit ")]
            if len(ids) != 1 or not re.fullmatch(r"[a-f0-9]{40}", ids[0]):
                raise ValueError(f"invalid build provenance: {name}")
            commits.add(ids[0]); infos.add(name)
        else:
            size = path.stat().st_size
            if not 0 < size <= 256 * 1024 * 1024:
                raise ValueError(f"invalid artifact size: {name}")
            file = {"name": name, "size": size, "sha256": digest,
                    "arch": "x86_64", **classify(name, version)}
            if file["os"] == "windows":
                file["installation"] = "nsis" if file["format"] == "setup" else "portable"
            files.append(file)
    if (len(commits) != 1 or
            not {"BUILDINFO.linux", "BUILDINFO.windows", "BUILDINFO.windows-installer"} <= infos or
            not REQUIRED <= {(f["kind"], f["os"], f["format"]) for f in files}):
        raise ValueError("release incomplete or built from different commits")
    manifest = {"schema": 1, "version": version, "complete": True,
                "published": datetime.fromtimestamp(sums.stat().st_mtime, timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "commit": commits.pop(), "dirty": False, "base": f"downloads/{version}/",
                "sums": "SHA256SUMS", "sig": "SHA256SUMS.sig", "files": files}
    return manifest, sorted(names)


def sign_bytes(data: bytes, key: Path, namespace: str, root: Path = ROOT) -> str:
    """SSH signs exact bytes; the client does not reserialize signed JSON."""
    with tempfile.TemporaryDirectory(prefix="icu-sign-") as tmp:
        payload = Path(tmp) / "payload"
        payload.write_bytes(data)
        subprocess.run(["ssh-keygen", "-Y", "sign", "-f", str(key), "-n", namespace, str(payload)],
                       check=True, capture_output=True)
        signature = Path(str(payload) + ".sig")
        subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(root / "packaging/allowed_signers"),
                        "-I", "icu-release", "-n", namespace, "-s", str(signature)],
                       input=data, check=True, capture_output=True)
        return signature.read_text()


def authenticate(manifest: dict, key: Path, root: Path = ROOT) -> dict:
    public = subprocess.run(["ssh-keygen", "-y", "-f", str(key)], check=True,
                            capture_output=True, text=True).stdout.split()[:2]
    pinned = (root / "packaging/update-signing.pub").read_text().split()[:2]
    if public != pinned or public[0] != "ssh-ed25519":
        raise ValueError("signing key does not match the public key embedded in the client")
    payload = json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode()
    return {**manifest, "authenticated": {"payload": base64.b64encode(payload).decode(),
            "signature": sign_bytes(payload, key, "icu-update", root)}}


def atomic_write(path: Path, data: bytes) -> None:
    with tempfile.NamedTemporaryFile(dir=path.parent, prefix=".icu-publish-", delete=False) as f:
        tmp = Path(f.name)
        try:
            os.fchmod(f.fileno(), 0o644)
            f.write(data); f.flush(); os.fsync(f.fileno()); f.close()
            tmp.replace(path)
            directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            tmp.unlink(missing_ok=True)


def publish(src: Path, manifest: dict, names: list[str], channel: Path) -> None:
    channel.mkdir(parents=True, exist_ok=True)
    with (channel / ".publish.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        latest = channel / "latest.json"
        if latest.exists() and stable_version(json.loads(latest.read_text())["version"]) > stable_version(manifest["version"]):
            raise ValueError("refusing to downgrade latest.json")
        dest = channel / manifest["version"]
        if dest.is_symlink():
            raise ValueError("version directory must not be a symlink")
        dest.mkdir(exist_ok=True)
        for name in names + ["SHA256SUMS", "SHA256SUMS.sig"]:
            source, target = src / name, dest / name
            if target.is_symlink():
                raise ValueError(f"refusing symlink target: {name}")
            if target.exists():
                if sha256(source) != sha256(target):
                    raise ValueError(f"published artifact is immutable: {name}")
            else:
                atomic_write(target, source.read_bytes())
        encoded = (json.dumps(manifest, indent=2) + "\n").encode()
        atomic_write(dest / "manifest.json", encoded)
        # The channel becomes visible only after every immutable artifact exists.
        atomic_write(latest, encoded)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("version", nargs="?", default=workspace_version())
    ap.add_argument("--prepare-only", action="store_true", help="sign dist/<version>/latest.json without publishing a web channel")
    ap.add_argument("--channel-dir", type=Path, default=DOWNLOADS, help="local channel directory (private acceptance channels use a separate directory)")
    args = ap.parse_args()
    try:
        src = ROOT / "dist" / args.version
        manifest, names = release_manifest(src, args.version)
        head = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], check=True, capture_output=True, text=True).stdout.strip()
        dirty = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain"], check=True, capture_output=True, text=True).stdout.strip()
        if dirty or manifest["commit"] != head:
            raise ValueError("publish only a clean build of the current committed source")
        key = Path(os.environ.get("ICU_RELEASE_KEY", str(Path.home() / ".ssh/icu-release"))).expanduser()
        manifest = authenticate(manifest, key)
        atomic_write(src / "SHA256SUMS.sig", sign_bytes((src / "SHA256SUMS").read_bytes(), key, "icu-release").encode())
        atomic_write(src / "latest.json", (json.dumps(manifest, indent=2) + "\n").encode())
        if not args.prepare_only:
            publish(src, manifest, names, args.channel_dir)
        print(f"signed {args.version}: {len(manifest['files'])} complete packages"
              + (" (prepared only)" if args.prepare_only else f" -> {args.channel_dir}"))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"publish-release: {error}")


if __name__ == "__main__":
    main()
