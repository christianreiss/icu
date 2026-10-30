"""Signed channel publication: no partial, dirty, downgraded or mutable release."""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("publish_release", Path(__file__).parents[2] / "web/tools/publish-release.py")
publish = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publish)


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="icu-publish-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.src = self.root / "dist"
        self.src.mkdir()
        self.version = "1.2.3"
        names = ["icu-1.2.3-windows-x86_64.zip", "icu-1.2.3-windows-x86_64-setup.exe",
                 "icu-client-1.2.3-1.fc42.x86_64.rpm", "icu-client-1.2.3-1.fc43.x86_64.rpm",
                 "icu_1.2.3-1+deb13_amd64.deb", "icu-server-1.2.3-1.x86_64.rpm", "icu-server_1.2.3-1_amd64.deb"]
        for name in names:
            (self.src / name).write_bytes(name.encode())
        for name in ["BUILDINFO.linux", "BUILDINFO.windows", "BUILDINFO.windows-installer"]:
            (self.src / name).write_text("commit " + "a" * 40 + "\n")
        self.sums()

    def sums(self):
        lines = [f"{publish.sha256(p)}  {p.name}" for p in sorted(self.src.iterdir()) if p.name not in ("SHA256SUMS", "SHA256SUMS.sig")]
        (self.src / "SHA256SUMS").write_text("\n".join(lines) + "\n")

    def test_complete_manifest_keeps_legacy_fields_and_selectors(self):
        manifest, names = publish.release_manifest(self.src, self.version)
        self.assertEqual(len(manifest["files"]), 7)
        self.assertEqual(len(names), 10)
        self.assertEqual(manifest["base"], "downloads/1.2.3/")
        self.assertEqual({f["installation"] for f in manifest["files"] if f["os"] == "windows"}, {"nsis", "portable"})
        for bad in ["1.2", "01.2.3", "1.2.3-rc.1", "../1.2.3", "1.2.3+build"]:
            with self.assertRaises(ValueError):
                publish.stable_version(bad)

    def test_dirty_partial_and_mixed_builds_are_rejected(self):
        info = self.src / "BUILDINFO.windows"
        for text in ["commit " + "a" * 40 + "\nDIRTY\n", "commit " + "b" * 40 + "\n"]:
            info.write_text(text); self.sums()
            with self.assertRaises(ValueError):
                publish.release_manifest(self.src, self.version)
        info.write_text("commit " + "a" * 40 + "\n")
        (self.src / "icu-1.2.3-windows-x86_64-setup.exe").unlink(); self.sums()
        with self.assertRaises(ValueError):
            publish.release_manifest(self.src, self.version)

    def test_traversal_and_hash_mismatch_are_rejected_before_publication(self):
        sums = self.src / "SHA256SUMS"
        sums.write_text("a" * 64 + "  ../outside\n")
        with self.assertRaises(ValueError):
            publish.release_manifest(self.src, self.version)
        self.sums(); (self.src / "icu-1.2.3-windows-x86_64.zip").write_bytes(b"corrupt")
        with self.assertRaises(ValueError):
            publish.release_manifest(self.src, self.version)

    def test_sign_exact_payload_and_publish_latest_last(self):
        packaging = self.root / "packaging"; packaging.mkdir()
        key = self.root / "test-key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        public = " ".join(Path(str(key) + ".pub").read_text().split()[:2])
        (packaging / "update-signing.pub").write_text(public + "\n")
        (packaging / "allowed_signers").write_text('icu-release namespaces="icu-release,icu-update" ' + public + "\n")
        manifest, names = publish.release_manifest(self.src, self.version)
        authenticated = publish.authenticate(manifest, key, self.root)
        self.assertEqual(json.loads(base64.b64decode(authenticated["authenticated"]["payload"])), manifest)
        (self.src / "SHA256SUMS.sig").write_text(publish.sign_bytes((self.src / "SHA256SUMS").read_bytes(), key, "icu-release", self.root))
        channel = self.root / "channel"
        publish.publish(self.src, authenticated, names, channel)
        self.assertEqual(json.loads((channel / "latest.json").read_text()), authenticated)
        for name in names:
            self.assertEqual(publish.sha256(self.src / name), publish.sha256(channel / self.version / name))
        # A preexisting immutable payload is never silently replaced, and
        # failed publication leaves the previous channel pointer unchanged.
        before = (channel / "latest.json").read_bytes()
        (channel / self.version / names[0]).write_bytes(b"other")
        with self.assertRaises(ValueError):
            publish.publish(self.src, authenticated, names, channel)
        self.assertEqual((channel / "latest.json").read_bytes(), before)
        newer = {**authenticated, "version": "2.0.0"}
        (channel / "latest.json").write_text(json.dumps(newer))
        with self.assertRaises(ValueError):
            publish.publish(self.src, authenticated, names, channel)


if __name__ == "__main__":
    unittest.main()
