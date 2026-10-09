"""Exercise the build driver with tiny builders, never the real toolchains."""
from contextlib import redirect_stderr, redirect_stdout
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("build_all", Path(__file__).parents[1] / "build_all.py")
build = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build)


class BuildTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="icu-build-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n'
            '[workspace.package]\nversion = "0.1.0" # shared version\n')
        for name in ("icu-client", "icu-server", "icu-proto"):
            crate = self.root / "crates" / name
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text(f'[package]\nname = "{name}"\nversion.workspace = true\nedition = "2021"\n')
            (crate / "src/main.rs").write_text('fn main() { println!("{}", env!("CARGO_PKG_VERSION")); }\n')
        subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=self.root, check=True, capture_output=True)
        (self.root / ".gitignore").write_text("target/\ndist/\n")
        (self.root / "packaging/windows").mkdir(parents=True)
        expected = build.expected_artifacts("0.1.0", "1")
        helper = '''import hashlib, json, os, pathlib, subprocess, sys, tomllib
root = pathlib.Path.cwd()
version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
out = root / "dist" / version
out.mkdir(parents=True, exist_ok=True)
component = sys.argv[1]
with (root / "target/order").open("a") as stream:
    stream.write(component + "\\n")
if os.environ.get("FAIL_COMPONENT") == component:
    print("intentional failure", flush=True)
    sys.exit(7)
assert os.environ["SKIP_TESTS"] == os.environ["SKIP_SMOKE"] == os.environ["NO_AEC"] == "0"
assert os.environ["DISTS"] == "fc42 fc43 debian13"
assert "CARGO_TARGET_DIR" not in os.environ
artifacts = [name.replace("0.1.0", version) for name in json.loads((root / "expected.json").read_text())[component]]
for name in artifacts:
    (out / name).write_bytes(("payload " + name).encode())
commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
info = "commit " + commit + "\\n"
if os.environ["ALLOW_DIRTY"] == "1": info += "DIRTY\\n"
info += "".join("artifact " + name + "\\n" for name in artifacts)
(out / ("BUILDINFO." + component)).write_text(info)
names = set()
for path in out.glob("BUILDINFO.*"):
    names.add(path.name)
    names.update(line[9:] for line in path.read_text().splitlines() if line.startswith("artifact "))
(out / "SHA256SUMS").write_text("".join(hashlib.sha256((out/name).read_bytes()).hexdigest() + "  " + name + "\\n" for name in sorted(names)))
if os.environ.get("CHANGE_SOURCE") == component:
    (root / "Cargo.toml").write_text((root / "Cargo.toml").read_text() + "# changed\\n")
print("==> built " + component)
'''
        (self.root / "helper.py").write_text(helper)
        (self.root / "expected.json").write_text(json.dumps({k: sorted(v) for k, v in expected.items()}))
        for script, component in (("packaging/build.sh", "linux"),
                                  ("packaging/windows/build.sh", "windows"),
                                  ("packaging/windows/installer.sh", "windows-installer")):
            (self.root / script).write_text(f"#!/bin/bash\nexec {sys.executable} -B helper.py {component}\n")
        for args in (["init", "-q"], ["add", "Cargo.toml", "Cargo.lock", "crates", ".gitignore", "packaging", "helper.py", "expected.json"],
                     ["-c", "user.name=Test", "-c", "user.email=test@localhost", "-c", "commit.gpgsign=false", "commit", "-qm", "fixture"]):
            subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True)
        self.commit = build.capture(["git", "rev-parse", "HEAD"], self.root)

    def execute(self, env=None, bump=False, clean_build=False):
        output = io.StringIO()
        with patch.dict(os.environ, env or {}), patch.object(build, "preflight"), \
                patch.object(build, "cleanup_containers"), redirect_stdout(output), redirect_stderr(output):
            code = build.execute(self.root, True, bump, clean_build)
        return code, output.getvalue()

    def test_bump_builds_new_version_and_both_binaries_inherit_it(self):
        old_out = self.root / "dist/0.1.0"
        old_out.mkdir(parents=True)
        (old_out / "previous").write_text("keep")
        code, output = self.execute(bump=True)
        self.assertEqual(code, 0, output)
        self.assertIn("0.1.0 -> 0.1.1", output)
        self.assertIn('version = "0.1.1" # shared version', (self.root / "Cargo.toml").read_text())
        self.assertEqual((old_out / "previous").read_text(), "keep")
        self.assertEqual(len(build.verify_artifacts(self.root / "dist/0.1.1", "0.1.1", "1", self.commit, True)), 7)
        for name in ("icu-client", "icu-server"):
            version = build.capture(["cargo", "run", "--quiet", "--locked", "--offline", "-p", name], self.root)
            self.assertEqual(version, "0.1.1")

    def test_failed_build_keeps_bump_and_plain_retry_reuses_it(self):
        code, output = self.execute({"FAIL_COMPONENT": "linux"}, bump=True)
        self.assertEqual(code, 1)
        self.assertIn("Version remains 0.1.1; retry ./build.sh without --bump.", output)
        code, output = self.execute()
        self.assertEqual(code, 0, output)
        self.assertTrue((self.root / "dist/0.1.1/SHA256SUMS").is_file())
        self.assertFalse((self.root / "dist/0.1.2").exists())

    def test_failed_preflight_does_not_bump(self):
        original = (self.root / "Cargo.toml").read_bytes()
        with patch.object(build, "preflight", side_effect=build.BuildError("missing tool")), \
                patch.object(build, "cleanup_containers"), redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            self.assertEqual(build.execute(self.root, True, True), 1)
        self.assertEqual((self.root / "Cargo.toml").read_bytes(), original)

    def test_failed_lock_update_restores_original_files(self):
        originals = {name: (self.root / name).read_bytes() for name in ("Cargo.toml", "Cargo.lock")}
        def failed_update(*_):
            (self.root / "Cargo.lock").write_text("partial lock update")
            raise build.BuildError("cargo exited 7")
        with patch.object(build, "run_command", side_effect=failed_update):
            with self.assertRaisesRegex(build.BuildError, "cargo exited 7"):
                build.bump_version(self.root, "0.1.0", {}, self.root / "bump.log", build.Dashboard(True, "test"))
        for name, original in originals.items():
            self.assertEqual((self.root / name).read_bytes(), original)

    def test_lock_update_cannot_change_other_dependencies(self):
        original = (self.root / "Cargo.lock").read_bytes()
        def wrong_update(*_):
            (self.root / "Cargo.lock").write_bytes(original.replace(b'"0.1.0"', b'"0.1.1"') +
                                                    b'\n[[package]]\nname = "unrelated"\nversion = "9.0.0"\n')
        with patch.object(build, "run_command", side_effect=wrong_update):
            with self.assertRaisesRegex(build.BuildError, "changed more than"):
                build.bump_version(self.root, "0.1.0", {}, self.root / "bump.log", build.Dashboard(True, "test"))
        self.assertEqual((self.root / "Cargo.lock").read_bytes(), original)

    def test_bump_flag_reaches_runner(self):
        previous = signal.getsignal(signal.SIGTERM)
        self.addCleanup(signal.signal, signal.SIGTERM, previous)
        with patch.object(sys, "argv", ["build.sh", "--plain", "--bump"]), \
                patch.object(build, "execute", return_value=0) as execute:
            self.assertEqual(build.main(), 0)
        execute.assert_called_once_with(build.ROOT, True, True, False)

    def test_clean_flag_combines_with_bump_and_plain(self):
        previous = signal.getsignal(signal.SIGTERM)
        self.addCleanup(signal.signal, signal.SIGTERM, previous)
        with patch.object(sys, "argv", ["build.sh", "--clean", "--plain", "--bump"]), \
                patch.object(build, "execute", return_value=0) as execute:
            self.assertEqual(build.main(), 0)
        execute.assert_called_once_with(build.ROOT, True, True, True)

    def test_clean_only_cli_removes_outputs_without_build_requirements(self):
        stale = self.root / "target/debug/stale"
        stale.parent.mkdir(parents=True)
        stale.write_text("compiled output")
        package = self.root / "dist/0.1.0/keep.rpm"
        package.parent.mkdir(parents=True)
        package.write_text("package")
        originals = {name: (self.root / name).read_bytes() for name in ("Cargo.toml", "Cargo.lock")}
        previous = signal.getsignal(signal.SIGTERM)
        self.addCleanup(signal.signal, signal.SIGTERM, previous)
        with patch.object(sys, "argv", ["build.sh", "--clean-only", "--plain"]), \
                patch.object(build, "ROOT", self.root), \
                patch.object(build, "execute", side_effect=AssertionError("unexpected build")), \
                patch.object(build, "capture", side_effect=AssertionError("unexpected tool invocation")), \
                patch.object(build, "preflight", side_effect=AssertionError("unexpected preflight")), \
                redirect_stdout(io.StringIO()) as output:
            self.assertEqual(build.main(), 0)
            self.assertEqual(build.main(), 0)  # Repeated cleanup is safe.
        self.assertFalse(stale.exists())
        self.assertEqual(package.read_text(), "package")
        for name, original in originals.items():
            self.assertEqual((self.root / name).read_bytes(), original)
        self.assertIn("ICU CLEAN COMPLETE", output.getvalue())
        self.assertEqual(len(list((self.root / "target/build-logs").glob("*/0-clean.log"))), 2)

    def test_clean_only_rejects_build_options_before_deleting(self):
        for option in ("--clean", "--bump"):
            with self.subTest(option=option), \
                    patch.object(sys, "argv", ["build.sh", "--clean-only", option]), \
                    patch.object(build, "execute_clean") as cleanup, redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    build.main()
                self.assertEqual(error.exception.code, 2)
                cleanup.assert_not_called()

    def test_clean_only_respects_existing_build_lock(self):
        with build.build_lock(self.root):
            with self.assertRaisesRegex(build.BuildError, "already running"):
                build.execute_clean(self.root)

    def test_clean_scrubs_every_target_preserving_lock_logs_and_user_data(self):
        target = self.root / "target"
        for name in ("debug", "release", "release-server", "musl", "pkg", "pkg-fc42", "pkg-fc43",
                     "pkg-debian13", "x86_64-pc-windows-gnu", "windows-static", "experiments", "doc"):
            (target / name).mkdir(parents=True)
            (target / name / "stale").write_text("compiled output")
        (target / ".rustc_info.json").write_text("stale")
        protected = ("dist/0.1.0/old.rpm", "dist/backups/old/file", ".local/state",
                     "target/build-logs/previous/log", "outside-target/keep")
        for name in protected:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("keep")
        (target / "external-link").symlink_to(self.root / "outside-target", target_is_directory=True)
        (target / "dangling-link").symlink_to(self.root / "missing")
        with build.build_lock(self.root), redirect_stdout(io.StringIO()):
            inode = (target / "build-all.lock").stat().st_ino
            build.clean_targets(self.root, target / "build-logs/clean.log", build.Dashboard(True, "test"))
            self.assertEqual({p.name for p in target.iterdir()}, {"build-all.lock", "build-logs"})
            self.assertEqual((target / "build-all.lock").stat().st_ino, inode)
            with self.assertRaisesRegex(build.BuildError, "already running"):
                with build.build_lock(self.root):
                    self.fail("clean removed active lock")
        for name in protected:
            self.assertEqual((self.root / name).read_text(), "keep")

    def test_clean_then_bump_then_full_pipeline(self):
        stale = self.root / "target/debug/stale"
        stale.parent.mkdir(parents=True)
        stale.write_text("old binary")
        code, output = self.execute(bump=True, clean_build=True)
        self.assertEqual(code, 0, output)
        self.assertFalse(stale.exists())
        self.assertLess(output.index("Clean complete"), output.index("Version bumped"))
        self.assertEqual(len(build.verify_artifacts(self.root / "dist/0.1.1", "0.1.1", "1", self.commit, True)), 7)

    def test_plain_build_retains_compiler_cache(self):
        stale = self.root / "target/debug/cached"
        stale.parent.mkdir(parents=True)
        stale.write_text("cache")
        code, output = self.execute()
        self.assertEqual(code, 0, output)
        self.assertTrue(stale.exists())

    def test_clean_refuses_active_cargo_before_deleting_anything(self):
        target = self.root / "target"
        target.mkdir()
        sentinel = target / "keep"
        sentinel.write_text("active build")
        process = subprocess.Popen(["bash", "-c", "exec -a cargo sleep 60"], cwd=self.root)
        try:
            deadline = time.monotonic() + 3
            while str(process.pid) not in build.active_build_processes(self.root) and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertIn(str(process.pid), build.active_build_processes(self.root))
            with self.assertRaisesRegex(build.BuildError, "active builds/binaries"):
                build.clean_targets(self.root, self.root / "clean.log", build.Dashboard(True, "test"))
            self.assertTrue(sentinel.exists())
        finally:
            process.terminate()
            process.wait()

    def test_clean_refuses_symlinked_target(self):
        external = self.root / "external"
        external.mkdir()
        (external / "keep").write_text("keep")
        (self.root / "target").symlink_to(external, target_is_directory=True)
        with self.assertRaisesRegex(build.BuildError, "symlinked"):
            build.clean_targets(self.root, self.root / "clean.log", build.Dashboard(True, "test"))
        with self.assertRaisesRegex(build.BuildError, "not a symlink"):
            with build.build_lock(self.root):
                self.fail("lock followed a target symlink")
        self.assertEqual((external / "keep").read_text(), "keep")
        self.assertFalse((external / "build-all.lock").exists())

    def test_bump_increments_numeric_patch_and_removes_suffix(self):
        for before, after in (("2.3.9", "2.3.10"), ("2.3.9-beta.1", "2.3.10")):
            with self.subTest(version=before):
                for name in ("Cargo.toml", "Cargo.lock"):
                    path = self.root / name
                    text = path.read_text()
                    import re
                    text = re.sub(r'(?m)^(version\s*=\s*)"[^"]+"', lambda m: f'{m[1]}"{before}"', text)
                    path.write_text(text)
                with redirect_stdout(io.StringIO()):
                    actual = build.bump_version(self.root, before, os.environ.copy(), self.root / "bump.log",
                                                build.Dashboard(True, "test"))
                self.assertEqual(actual, after)

    def test_full_build_and_backup(self):
        out = self.root / "dist/0.1.0"
        out.mkdir(parents=True)
        (out / "old-package").write_bytes(b"keep me")
        code, output = self.execute({"SKIP_TESTS": "1", "SKIP_SMOKE": "1", "NO_AEC": "1",
                                     "DISTS": "fc43", "CARGO_TARGET_DIR": "/wrong"})
        self.assertEqual(code, 0, output)
        self.assertEqual((self.root / "target/order").read_text().splitlines(),
                         ["linux", "windows", "windows-installer"])
        self.assertEqual(len(build.verify_artifacts(out, "0.1.0", "1", self.commit, False)), 7)
        self.assertEqual(next((self.root / "dist/backups").glob("*/old-package")).read_bytes(), b"keep me")
        self.assertNotIn("\x1b", output)
        self.assertIn("BUILD COMPLETE", output)

    def test_uncommitted_code_is_accepted_and_recorded(self):
        (self.root / "current-code").write_text("new source")
        code, output = self.execute()
        self.assertEqual(code, 0, output)
        self.assertIn("DIRTY", (self.root / "dist/0.1.0/BUILDINFO.linux").read_text())

    def test_failed_builder_stops_pipeline_and_retains_log(self):
        code, output = self.execute({"FAIL_COMPONENT": "linux"})
        self.assertEqual(code, 1)
        self.assertIn("exited 7", output)
        self.assertNotIn("BUILD COMPLETE", output)
        self.assertEqual((self.root / "target/order").read_text(), "linux\n")
        self.assertIn("intentional failure", next((self.root / "target/build-logs").glob("*/*.log")).read_text())

    def test_mid_build_source_change_fails(self):
        code, output = self.execute({"CHANGE_SOURCE": "windows-installer"})
        self.assertEqual(code, 1)
        self.assertIn("Source changed", output)

    def test_missing_stale_and_corrupt_artifacts_fail(self):
        code, output = self.execute()
        self.assertEqual(code, 0, output)
        out = self.root / "dist/0.1.0"
        artifact = out / "icu-0.1.0-windows-x86_64-setup.exe"
        original = artifact.read_bytes()
        artifact.unlink()
        with self.assertRaisesRegex(build.BuildError, "Missing, empty"):
            build.verify_artifacts(out, "0.1.0", "1", self.commit, False)
        artifact.write_bytes(original + b"corrupt")
        with self.assertRaisesRegex(build.BuildError, "Checksum mismatch"):
            build.verify_artifacts(out, "0.1.0", "1", self.commit, False)
        artifact.write_bytes(original)
        info = out / "BUILDINFO.windows"
        info.write_text(info.read_text().replace(self.commit, "stale-commit"))
        with self.assertRaisesRegex(build.BuildError, "Wrong source commit"):
            build.verify_artifacts(out, "0.1.0", "1", self.commit, False)

    def test_lock_excludes_second_runner(self):
        with build.build_lock(self.root):
            with self.assertRaisesRegex(build.BuildError, "already running"):
                with build.build_lock(self.root):
                    self.fail("second lock acquired")

    def test_preflight_aggregates_missing_tools(self):
        with patch.object(build.shutil, "which", return_value=None):
            with self.assertRaisesRegex(build.BuildError, "Missing tools: bash, git, podman, cargo"):
                build.preflight(self.root, {}, build.Dashboard(True, "test"))

    def test_cleanup_only_selects_current_run(self):
        with patch.object(build.shutil, "which", return_value="/usr/bin/podman"), \
                patch.object(build, "capture", side_effect=["abc\ndef", ""]) as capture:
            build.cleanup_containers(self.root, {}, "test-run")
        self.assertEqual(capture.call_args_list[0].args[0],
                         ["podman", "ps", "-aq", "--filter", "label=net.icu.build-run=test-run"])
        self.assertEqual(capture.call_args_list[1].args[0], ["podman", "rm", "--force", "abc", "def"])

    def test_failed_shell_terminates_descendants(self):
        # The shell waits for a grandchild. Signal our runner once the PID is
        # available; run_command must propagate cancellation and reap the group.
        pidfile = self.root / "pid"
        driver = self.root / "driver.py"
        driver.write_text(f'''import importlib.util, pathlib, sys
s = importlib.util.spec_from_file_location("build", {str(Path(build.__file__))!r})
b = importlib.util.module_from_spec(s); s.loader.exec_module(b)
try:
    b.run_command(["bash", "-c", "sleep 60 & echo $! > pid; wait"], pathlib.Path({str(self.root)!r}), None,
                  pathlib.Path({str(self.root / "child.log")!r}), b.Dashboard(True, "test"))
except KeyboardInterrupt:
    sys.exit(130)
''')
        process = subprocess.Popen([sys.executable, "-B", str(driver)], stdout=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 5
            while not pidfile.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(pidfile.exists())
            child_pid = int(pidfile.read_text())
            process.send_signal(signal.SIGINT)
            self.assertEqual(process.wait(timeout=6), 130)
            proc = Path(f"/proc/{child_pid}/stat")
            self.assertTrue(not proc.exists() or proc.read_text().split()[2] == "Z")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    def test_dashboard_narrow_terminal_and_restoration(self):
        stream = io.StringIO()
        with patch.object(stream, "isatty", return_value=True), redirect_stdout(stream), \
                patch.dict(os.environ, {"TERM": "xterm", "NO_COLOR": "1"}), \
                patch.object(build.shutil, "get_terminal_size", return_value=(32, 12)):
            with build.Dashboard(False, "test " * 20) as ui:
                ui.start(1)
                ui.line("==> compiling")
                ui.render()
                ui.finish()
        output = stream.getvalue()
        self.assertIn("\x1b[?1049h\x1b[?25l", output)
        self.assertTrue(output.endswith("\x1b[0m\x1b[?25h\x1b[?1049l"))
        self.assertNotIn("\x1b[1;36m", output)


if __name__ == "__main__":
    unittest.main()
