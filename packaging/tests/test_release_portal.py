"""Release API and download page (web/portal) against a real PHP server.

Releases are signed with a throwaway key through the same publish-release.py
code as production. Skipped when php or ssh-keygen is missing.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import socket
import sys
import subprocess
import tempfile
import time
import unittest
import urllib.error
import urllib.request

REPO = Path(__file__).parents[2]
PORTAL = REPO / "web/portal"
SPEC = importlib.util.spec_from_file_location("publish_release", REPO / "web/tools/publish-release.py")
publish = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(publish)

ROUTER = """<?php
$uri = parse_url($_SERVER['REQUEST_URI'], PHP_URL_PATH);
if (preg_match('#^/api/v1/?(.*)$#', $uri, $m)) { $_GET['path'] = rawurldecode($m[1]); require __DIR__ . '/api.php'; return true; }
if ($uri === '/' ) { require __DIR__ . '/index.php'; return true; }
return false;
"""
TOKEN = "t" * 40
PACKAGES = ["icu-{v}-windows-x86_64.zip", "icu-{v}-windows-x86_64-setup.exe",
            "icu-client-{v}-1.fc42.x86_64.rpm", "icu-client-{v}-1.fc43.x86_64.rpm",
            "icu_{v}-1+deb13_amd64.deb", "icu-server-{v}-1.x86_64.rpm", "icu-server_{v}-1_amd64.deb"]


@unittest.skipUnless(shutil.which("php") and shutil.which("ssh-keygen"), "needs php and ssh-keygen")
class PortalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="icu-portal-test-")
        root = cls.root = Path(cls.temp.name)
        cls.web = root / "web/icu"
        shutil.copytree(PORTAL, cls.web)
        (cls.web / "router.php").write_text(ROUTER)
        cls.downloads = cls.web / "downloads"
        cls.downloads.mkdir()
        cls.data = root / "private/icu-portal"
        cls.data.mkdir(parents=True)
        cls.key = cls.keypair("release")
        cls.other = cls.keypair("other")
        (root / "packaging").mkdir()
        public = cls.public(cls.key)
        (root / "packaging/update-signing.pub").write_text(public + "\n")
        signers = 'icu-release namespaces="icu-release,icu-update" ' + public + "\n"
        (root / "packaging/allowed_signers").write_text(signers)
        (cls.data / "allowed_signers").write_text(signers)
        (cls.data / "config.php").write_text(
            "<?php return ['token_sha256' => '%s', 'downloads' => '%s', 'server_note' => 'Server: test'];\n"
            % (hashlib.sha256(TOKEN.encode()).hexdigest(), cls.downloads))
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            cls.port = s.getsockname()[1]
        env = {**os.environ, "ICU_PORTAL_CONFIG": str(cls.data / "config.php")}
        cls.server = subprocess.Popen(["php", "-S", f"127.0.0.1:{cls.port}", "-t", str(cls.web), str(cls.web / "router.php")],
                                      env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", cls.port), 0.1).close()
                break
            except OSError:
                time.sleep(0.05)

    @classmethod
    def tearDownClass(cls):
        cls.server.terminate()
        cls.server.wait()
        cls.temp.cleanup()

    @classmethod
    def keypair(cls, name):
        key = cls.root / name
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        return key

    @staticmethod
    def public(key):
        return " ".join(Path(str(key) + ".pub").read_text().split()[:2])

    def release(self, version, key=None, names=None):
        """A complete signed dist directory, built like publish-release.py --prepare-only."""
        src = self.root / f"dist-{version}-{time.monotonic_ns()}"
        src.mkdir()
        for name in names or [p.format(v=version) for p in PACKAGES]:
            (src / name).write_bytes(f"{name} payload".encode())
        for name in ["BUILDINFO.linux", "BUILDINFO.windows", "BUILDINFO.windows-installer"]:
            (src / name).write_text("commit " + "c" * 40 + "\n")
        lines = [f"{publish.sha256(p)}  {p.name}" for p in sorted(src.iterdir())]
        (src / "SHA256SUMS").write_text("\n".join(lines) + "\n")
        if names is None:
            manifest, _ = publish.release_manifest(src, version)
            manifest = publish.authenticate(manifest, self.key, self.root)
        else:
            manifest = {"version": version}
        signer = key or self.key
        (src / "SHA256SUMS.sig").write_text(self.sign((src / "SHA256SUMS").read_bytes(), signer, "icu-release"))
        (src / "latest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        return src

    def sign(self, data, key, namespace):
        payload = self.root / f"payload-{time.monotonic_ns()}"
        payload.write_bytes(data)
        subprocess.run(["ssh-keygen", "-Y", "sign", "-f", str(key), "-n", namespace, str(payload)], check=True, capture_output=True)
        return Path(str(payload) + ".sig").read_text()

    def call(self, method, path, body=None, token=TOKEN, ctype="application/json"):
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", data=body, method=method)
        if token:
            req.add_header("Authorization", f"Bearer {token}")
        req.add_header("Content-Type", ctype)
        try:
            with urllib.request.urlopen(req) as resp:
                return resp.status, resp.read()
        except urllib.error.HTTPError as error:
            with error:
                return error.code, error.read()

    def upload(self, src, version, files=None, latest=True):
        status, body = self.call("POST", "/api/v1/uploads", json.dumps({"version": version}).encode())
        self.assertEqual(status, 201, body)
        upload = json.loads(body)["id"]
        for name in files or [p.name for p in src.iterdir()]:
            status, body = self.call("PUT", f"/api/v1/uploads/{upload}/{name}", (src / name).read_bytes(), ctype="application/octet-stream")
            if status != 200:
                return status, json.loads(body)
        status, body = self.call("POST", f"/api/v1/uploads/{upload}/commit", json.dumps({"latest": latest}).encode())
        return status, json.loads(body)

    def latest(self):
        path = self.downloads / "latest.json"
        return path.read_bytes() if path.exists() else None

    def test_1_publish_latest_last_and_idempotent(self):
        src = self.release("1.4.0")
        status, body = self.upload(src, "1.4.0")
        self.assertEqual(status, 200, body)
        self.assertEqual(self.latest(), (src / "latest.json").read_bytes())
        self.assertEqual((self.downloads / "1.4.0/manifest.json").read_bytes(), (src / "latest.json").read_bytes())
        for name in PACKAGES:
            n = name.format(v="1.4.0")
            self.assertEqual((self.downloads / "1.4.0" / n).read_bytes(), (src / n).read_bytes())
        self.assertFalse((self.downloads / "1.4.0/latest.json").exists())
        status, body = self.upload(src, "1.4.0")
        self.assertEqual(status, 200, body)

        status, body = self.call("GET", "/api/v1/releases/latest", token=None)
        info = json.loads(body)
        self.assertEqual((status, info["version"], len(info["files"])), (200, "1.4.0", 7))
        self.assertTrue(all(f["version_matches"] and f["present"] for f in info["files"]))

    def test_2_downgrade_refused_but_backfill_allowed(self):
        if not (self.downloads / "1.4.0").exists():
            self.test_1_publish_latest_last_and_idempotent()
        before = self.latest()
        src = self.release("1.3.0")
        status, body = self.upload(src, "1.3.0")
        self.assertEqual(status, 409, body)
        self.assertIn("downgrade", body["error"])
        status, body = self.upload(src, "1.3.0", latest=False)
        self.assertEqual(status, 200, body)
        self.assertEqual(self.latest(), before)

    def test_3_rejections_never_touch_the_channel(self):
        before = self.latest()
        # Signed by a key that is not in allowed_signers.
        status, body = self.upload(self.release("2.0.0", key=self.other), "2.0.0")
        self.assertEqual(status, 422, body)
        self.assertIn("icu-release signature", body["error"])
        # A package changed after signing.
        src = self.release("2.0.1")
        (src / "icu-2.0.1-windows-x86_64.zip").write_bytes(b"evil")
        status, body = self.upload(src, "2.0.1")
        self.assertEqual(status, 422, body)
        # latest.json visible fields edited after signing.
        src = self.release("2.0.2")
        data = json.loads((src / "latest.json").read_text())
        data["files"][0]["size"] += 1
        (src / "latest.json").write_text(json.dumps(data))
        status, body = self.upload(src, "2.0.2")
        self.assertEqual(status, 422, body)
        self.assertIn("signed payload", body["error"])
        # Upload version differs from the signed release.
        status, body = self.upload(self.release("2.0.3"), "2.0.4")
        self.assertEqual(status, 422, body)
        # Unknown or traversing file names are refused at upload.
        status, body = self.call("POST", "/api/v1/uploads", b'{"version":"2.0.5"}')
        upload = json.loads(body)["id"]
        for name in ["index.php", "..%2Fx", ".htaccess", "icu-2.0.5-windows-x86_64.zip.php"]:
            status, _ = self.call("PUT", f"/api/v1/uploads/{upload}/{name}", b"x", ctype="application/octet-stream")
            self.assertIn(status, (404, 422), name)
        self.assertEqual(self.latest(), before)
        self.assertFalse(any((self.downloads / v).exists() for v in ["2.0.0", "2.0.1", "2.0.2", "2.0.3", "2.0.4"]))

    def test_4_token_required_for_writes(self):
        for token in [None, "wrong" * 10]:
            status, _ = self.call("POST", "/api/v1/uploads", b'{"version":"3.0.0"}', token=token)
            self.assertEqual(status, 401)
            status, _ = self.call("PUT", "/api/v1/releases/1.4.0/notes", b"x", token=token, ctype="text/plain")
            self.assertEqual(status, 401)
        self.assertEqual(self.call("GET", "/api/v1/releases", token=None)[0], 200)

    def test_5_page_shows_versions_notes_and_deviating_packages(self):
        if not (self.downloads / "1.4.0").exists():
            self.test_1_publish_latest_last_and_idempotent()
        status, _ = self.call("PUT", "/api/v1/releases/1.4.0/notes", "Neu: <b>Kamera</b> & Ton".encode(), ctype="text/plain")
        self.assertEqual(status, 200)
        # A hand-placed 0.1.28-style directory with latest.json and one deviating package.
        old = self.downloads / "0.9.0"
        old.mkdir()
        files = [{"name": "icu-0.8.9-windows-x86_64.zip", "size": 3, "sha256": "0" * 64},
                 {"name": "icu-client-0.9.0-1.fc43.x86_64.rpm", "size": 3, "sha256": "0" * 64}]
        (old / files[0]["name"]).write_bytes(b"abc")
        (old / "latest.json").write_text(json.dumps({"version": "0.9.0", "files": files}))
        status, page = self.call("GET", "/", token=None)
        page = page.decode()
        self.assertEqual(status, 200)
        self.assertIn('<span class="version">1.4.0', page)
        self.assertIn("Neu: &lt;b&gt;Kamera&lt;/b&gt; &amp; Ton", page)
        self.assertIn("downloads/1.4.0/icu_1.4.0-1%2Bdeb13_amd64.deb", page)
        self.assertIn("Server-Pakete", page)
        self.assertIn("Version 0.8.9", page)
        self.assertIn("fehlt", page)  # fc43 rpm of 0.9.0 is not on disk
        self.assertIn("Server: test", page)
        status, body = self.call("GET", "/api/v1/releases/0.9.0", token=None)
        zip_ = [f for f in json.loads(body)["files"] if f["os"] == "windows"][0]
        self.assertEqual((zip_["version"], zip_["version_matches"]), ("0.8.9", False))

    def test_6_upload_tool_end_to_end(self):
        src = self.release("5.0.0")
        notes = self.root / "notes.txt"
        notes.write_text("Neu in fünf\n")
        out = subprocess.run([sys.executable, str(REPO / "web/tools/upload-release.py"), "5.0.0", "--dist", str(src),
                              "--api", f"http://127.0.0.1:{self.port}/api/v1", "--notes", str(notes)],
                             env={**os.environ, "ICU_RELEASE_API_TOKEN": TOKEN}, capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(self.latest(), (src / "latest.json").read_bytes())
        status, body = self.call("GET", "/api/v1/releases/5.0.0", token=None)
        self.assertEqual(json.loads(body)["notes"], "Neu in fünf")


@unittest.skipUnless(shutil.which("php") and shutil.which("ssh-keygen"), "needs php and ssh-keygen")
class RealReleaseTests(unittest.TestCase):
    def test_published_release_in_dist_verifies(self):
        """The real signed dist/<latest> passes the server-side checks (read-only)."""
        dist = Path(os.environ.get("ICU_TEST_DIST", REPO / "dist"))
        dists = sorted((p for p in dist.glob("*.*.*") if (p / "latest.json").is_file()
                        and (p / "SHA256SUMS.sig").is_file()), key=lambda p: p.stat().st_mtime) if dist.is_dir() else []
        if not dists:
            self.skipTest("no signed dist/<version>")
        src = dists[-1]
        version = json.loads((src / "latest.json").read_text())["version"]
        script = (f"require '{PORTAL}/_lib/release.php'; "
                  f"[$raw,,$names] = icu_verify_staging($argv[1], $argv[2]); echo count($names);")
        with tempfile.TemporaryDirectory() as tmp:
            config = Path(tmp) / "config.php"
            shutil.copy(REPO / "packaging/allowed_signers", Path(tmp) / "allowed_signers")
            config.write_text("<?php return [];\n")
            out = subprocess.run(["php", "-r", script, str(src), version], capture_output=True, text=True,
                                 env={**os.environ, "ICU_PORTAL_CONFIG": str(config)})
        self.assertEqual(out.returncode, 0, out.stdout + out.stderr)
        self.assertEqual(out.stdout, "10")


if __name__ == "__main__":
    unittest.main()
