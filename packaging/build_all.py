#!/usr/bin/env python3
"""Build every ICU package with a terminal dashboard; Python 3.11+, no pip."""

import argparse
import codecs
from collections import deque
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import hashlib
import os
from pathlib import Path
import platform
import re
import selectors
import shlex
import shutil
import signal
import subprocess
import sys
import time
import tomllib


ROOT = Path(__file__).resolve().parents[1]
LABEL = "net.icu.build-run"
STAGES = ("Preflight", "Linux server + clients / tests", "Windows client + ZIP",
          "Windows installer", "Artifacts + checksums")
ANSI = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))")


def component(stage, step):
    """Which part of the product a build step belongs to."""
    if stage in (2, 3):
        return "Windows client"
    if step.startswith("smoke:"):
        return "Smoke tests"
    if step.startswith("server:") or step.startswith("nfpm: icu-server"):
        return "Server"
    if step.startswith("client:") or step.startswith("nfpm: icu for"):
        return "Linux clients"
    return "Shared"


class BuildError(Exception):
    pass


def clean(text):
    return "".join(c for c in ANSI.sub("", text) if c == "\t" or ord(c) >= 32)


def elapsed(seconds):
    minutes, seconds = divmod(int(seconds), 60)
    return f"{minutes:02d}:{seconds:02d}"


def capture(command, root, env=None):
    try:
        result = subprocess.run(command, cwd=root, env=env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                timeout=30, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        raise BuildError(f"{shlex.join(command)}: {error}") from error
    if result.returncode:
        raise BuildError(f"{shlex.join(command)} exited {result.returncode}:\n{result.stdout[-3000:]}")
    return result.stdout.strip()


def build_environment(run_id, dirty):
    env = os.environ.copy()
    # These scripts use fixed staging paths; inherited Cargo locations/targets
    # would make them package a different binary or fail after a lengthy build.
    for key in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET"):
        env.pop(key, None)
    env.update(ALLOW_DIRTY="1" if dirty else "0", SKIP_TESTS="0", SKIP_SMOKE="0",
               NO_AEC="0", DISTS="fc42 fc43 debian13", SERVER_DISTS="debian12 alma10",
               ICU_BUILD_RUN_ID=run_id, CARGO_TERM_COLOR="never", NO_COLOR="1")
    return env


def source_digest(root):
    """Include uncommitted/untracked source, excluding ignored build outputs."""
    paths = capture(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], root)
    digest = hashlib.sha256()
    for name in sorted(set(paths.split("\0")) - {""}):
        path = root / name
        digest.update(name.encode() + b"\0")
        if path.is_symlink():
            digest.update(b"link\0" + os.readlink(path).encode())
        elif path.is_file():
            digest.update(b"file\0" + str(path.stat().st_mode & 0o777).encode() + b"\0")
            with path.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(block)
        else:
            digest.update(b"missing\0")
    return digest.hexdigest()


class Dashboard:
    def __init__(self, plain, title):
        self.live = not plain and sys.stdout.isatty() and os.environ.get("TERM", "dumb") != "dumb"
        self.color = self.live and "NO_COLOR" not in os.environ
        self.active = False
        self.title = title
        self.started = time.monotonic()
        self.states = ["pending"] * len(STAGES)
        self.times = [0.0] * len(STAGES)
        self.current = 0
        self.stage_start = self.started
        self.tail = deque(maxlen=6)
        self.step = "Preparing build"
        self.steps = []  # (component, step, seconds)
        self.step_start = self.started

    def close_step(self):
        """Record the running `==>` step; a step ends where the next begins."""
        now = time.monotonic()
        if self.step_name:
            self.steps.append((component(self.current, self.step_name), self.step_name,
                               now - self.step_start))
        self.step_name = None
        self.step_start = now

    step_name = None

    def summary(self):
        """Per-component totals and per-step timings, as printable lines."""
        lines = []
        for group in ("Server", "Linux clients", "Windows client", "Smoke tests", "Shared"):
            rows = [(name, secs) for g, name, secs in self.steps if g == group]
            if rows:
                lines.append(f"  {group:<16} {elapsed(sum(secs for _, secs in rows))}")
                lines += [f"      {elapsed(secs)}  {name}" for name, secs in rows]
        return lines

    def tint(self, text, color):
        return f"\033[{color}m{text}\033[0m" if self.color else text

    def __enter__(self):
        self.active = True
        if self.live:
            print("\033[?1049h\033[?25l", end="", flush=True)
        else:
            print(f"ICU BUILD | {self.title}", flush=True)
        return self

    def __exit__(self, *_):
        self.active = False
        if self.live:
            print("\033[0m\033[?25h\033[?1049l", end="", flush=True)

    def start(self, index):
        self.current = index
        self.states[index] = "running"
        self.stage_start = self.step_start = time.monotonic()
        self.step = STAGES[index]
        self.step_name = None
        self.tail.clear()
        if not self.live:
            print(f"\n[{index + 1}/{len(STAGES)}] {STAGES[index]}", flush=True)
        self.render()

    def finish(self, success=True):
        self.close_step()
        self.times[self.current] = time.monotonic() - self.stage_start
        self.states[self.current] = "passed" if success else "failed"
        if not self.live:
            print(f"{self.states[self.current].upper()}  {elapsed(self.times[self.current])}", flush=True)
        self.render()

    def line(self, line):
        line = clean(line.rstrip("\r\n"))
        if line.startswith("==> "):
            self.close_step()
            self.step = self.step_name = line[4:]
        self.tail.append(line)
        if not self.live:
            print(line, flush=True)

    def render(self):
        if not self.live or not self.active:
            return
        width, height = shutil.get_terminal_size((80, 24))
        width = max(1, width - 1)
        now = time.monotonic()
        spinner = "|/-\\"[int(now * 8) % 4]
        lines = [("ICU / BUILD CONTROL", "1;36"), (self.title, "2"),
                 (f"ALL PLATFORMS  |  {elapsed(now - self.started)} elapsed", "36"), ("", "0")]
        for index, name in enumerate(STAGES):
            state = self.states[index]
            icon = {"pending": ".", "running": spinner, "passed": "+", "failed": "!"}[state]
            duration = now - self.stage_start if state == "running" else self.times[index]
            text = f" {icon} {index + 1}. {name}  {elapsed(duration) if state != 'pending' else '--:--'}"
            lines.append((text, {"passed": "32", "running": "1;36", "failed": "1;31", "pending": "2"}[state]))
        lines += [("", "0"), (f"> {self.step}", "33"), ("", "0")]
        available = max(0, height - len(lines) - 1)
        lines += [(line, "2") for line in list(self.tail)[-available:]] if available else []
        print("\033[H\033[J" + "\n".join(self.tint(text[:width], color)
              for text, color in lines[:max(1, height - 1)]), end="", flush=True)


def stop_group(process):
    # The shell and its compilers share a private process group.
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def run_command(command, root, env, log, ui):
    ui.line("$ " + shlex.join(command))
    with log.open("wb") as output:
        output.write(("$ " + shlex.join(command) + "\n").encode())
        process = subprocess.Popen(command, cwd=root, env=env, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                   start_new_session=True)
        decoder = codecs.getincrementaldecoder("utf-8")("replace")
        pending = ""
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while selector.get_map():
                    if process.poll() not in (None, 0):
                        stop_group(process)
                    for key, _ in selector.select(timeout=0.15):
                        block = os.read(key.fileobj.fileno(), 65536)
                        if not block:
                            selector.unregister(key.fileobj)
                            pending += decoder.decode(b"", final=True)
                            break
                        output.write(block)
                        output.flush()
                        pending += decoder.decode(block)
                        while "\n" in pending:
                            line, pending = pending.split("\n", 1)
                            ui.line(line)
                        if len(pending) > 8192:
                            ui.line(pending)
                            pending = ""
                    ui.render()
            if pending:
                ui.line(pending)
            code = process.wait()
            if code:
                stop_group(process)
                raise BuildError(f"{shlex.join(command)} exited {code}\nLog: {log}")
        except BaseException:
            stop_group(process)
            raise
        finally:
            process.stdout.close()


def preflight(root, env, ui):
    errors = []
    tools = ("bash", "git", "podman", "cargo", "rustc", "makensis", "zip", "file",
             "cmake", "clang", "meson", "ninja", "nasm", "pkg-config",
             "x86_64-w64-mingw32-gcc", "x86_64-w64-mingw32-g++",
             "x86_64-w64-mingw32-objdump", "x86_64-w64-mingw32-windres",
             "x86_64-w64-mingw32-ar")
    missing = [tool for tool in tools if not shutil.which(tool, path=env.get("PATH"))]
    if missing:
        errors.append("Missing tools: " + ", ".join(missing))
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        errors.append("This build needs a Linux x86-64 host.")
    probes = [["podman", "info", "--format", "{{.Host.Arch}}"],
              ["cargo", "deny", "--version"], ["cargo", "about", "--version"],
              ["makensis", "-VERSION"],
              ["pkg-config", "--exists", "libpipewire-0.3", "xkbcommon", "wayland-client"]]
    for command in probes:
        if command[0] not in missing:
            try:
                result = capture(command, root, env)
                ui.line(f"OK {shlex.join(command)} {result}")
            except BuildError as error:
                errors.append(str(error))
    if "rustc" not in missing:
        try:
            libdir = Path(capture(["rustc", "--print", "target-libdir", "--target",
                                   "x86_64-pc-windows-gnu"], root, env))
            if not list(libdir.glob("libstd-*.rlib")):
                errors.append(f"Windows Rust standard library missing: {libdir}")
            sysroot = Path(capture(["rustc", "--print", "sysroot"], root, env))
            host = capture(["rustc", "-vV"], root, env).split("host: ", 1)[1].splitlines()[0]
            if not os.access(sysroot / "lib/rustlib" / host / "bin/rust-objcopy", os.X_OK):
                errors.append("AEC needs rust-objcopy in the Rust host sysroot (see build guide).")
        except BuildError as error:
            errors.append(str(error))
    library = Path("/usr/x86_64-w64-mingw32/sys-root/mingw/lib")
    for name in ("libstdc++.a", "libwinpthread.a"):
        if not (library / name).is_file():
            errors.append(f"Missing static Windows library: {library / name}")
    if errors:
        raise BuildError("Preflight failed:\n- " + "\n- ".join(errors))


def expected_artifacts(version, release):
    name = f"icu-{version}-windows-x86_64"
    return {
        "linux": {f"icu-server-{version}-{release}.x86_64.rpm",
                  f"icu-server_{version}-{release}_amd64.deb",
                  f"icu-{version}-{release}.fc42.x86_64.rpm",
                  f"icu-{version}-{release}.fc43.x86_64.rpm",
                  f"icu_{version}-{release}+deb13_amd64.deb"},
        "windows": {f"{name}.zip"},
        "windows-installer": {f"{name}-setup.exe"},
    }


def verify_artifacts(out, version, release, commit, dirty):
    expected = expected_artifacts(version, release)
    for component, artifacts in expected.items():
        info = out / f"BUILDINFO.{component}"
        if not info.is_file():
            raise BuildError(f"Missing {info}")
        lines = info.read_text().splitlines()
        if [line for line in lines if line.startswith("commit ")] != [f"commit {commit}"]:
            raise BuildError(f"Wrong source commit in {info}")
        if ("DIRTY" in lines) != dirty:
            raise BuildError(f"Wrong source status in {info}")
        listed = [line.removeprefix("artifact ") for line in lines if line.startswith("artifact ")]
        if set(listed) != artifacts or len(listed) != len(artifacts):
            raise BuildError(f"Unexpected artifact list in {info}: {listed}")
        for name in artifacts:
            path = out / name
            if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
                raise BuildError(f"Missing, empty or symlinked artifact: {path}")
    names = set().union(*expected.values()) | {f"BUILDINFO.{key}" for key in expected}
    sums = out / "SHA256SUMS"
    if not sums.is_file():
        raise BuildError(f"Missing {sums}")
    checksums = {}
    for line in sums.read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
        if not match or match[2] in checksums:
            raise BuildError(f"Malformed or duplicate checksum: {line}")
        checksums[match[2]] = match[1]
    if set(checksums) != names:
        raise BuildError("SHA256SUMS does not cover exactly this run's packages and BUILDINFO files")
    for name, expected_hash in checksums.items():
        with (out / name).open("rb") as stream:
            actual = hashlib.file_digest(stream, "sha256").hexdigest()
        if actual != expected_hash:
            raise BuildError(f"Checksum mismatch: {out / name}")
    return sorted(set().union(*expected.values()))


def backup_outputs(root, version, run_id):
    out = root / "dist" / version
    if out.exists() or out.is_symlink():
        if out.is_symlink() or not out.is_dir():
            raise BuildError(f"Expected an ordinary output directory: {out}")
        backup = root / "dist/backups" / f"{version}-{run_id}"
        backup.parent.mkdir(parents=True, exist_ok=True)
        out.rename(backup)
        return backup
    return None


def cleanup_containers(root, env, run_id):
    if not shutil.which("podman", path=env.get("PATH")):
        return
    ids = capture(["podman", "ps", "-aq", "--filter", f"label={LABEL}={run_id}"], root, env).split()
    if ids:
        capture(["podman", "rm", "--force", *ids], root, env)


@contextmanager
def build_lock(root):
    target = root / "target"
    if target.is_symlink():
        raise BuildError(f"Expected an ordinary target directory, not a symlink: {target}")
    target.mkdir(exist_ok=True)
    with (target / "build-all.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise BuildError("Another ./build.sh is already running in this checkout.") from error
        yield


def bump_version(root, version, env, log, ui):
    """Increment the shared patch version, preserving all dependency pins."""
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)(?:[-+][0-9A-Za-z.-]+)?", version)
    if not match:
        raise BuildError(f"Cannot bump version: {version}")
    new_version = f"{int(match[1])}.{int(match[2])}.{int(match[3]) + 1}"
    manifest = root / "Cargo.toml"
    lock = root / "Cargo.lock"
    original_manifest = manifest.read_bytes()
    original_lock = lock.read_bytes()
    text = original_manifest.decode()
    section = re.search(r"(?ms)^\[workspace\.package\][^\n]*\n.*?(?=^\[|\Z)", text)
    if not section:
        raise BuildError("Cannot find [workspace.package] in Cargo.toml")
    updated, count = re.subn(r'(?m)^([ \t]*version[ \t]*=[ \t]*)"' + re.escape(version) + r'"',
                             lambda m: f'{m[1]}"{new_version}"', section[0])
    if count != 1:
        raise BuildError("Cannot locate the shared version line in Cargo.toml")
    text = text[:section.start()] + updated + text[section.end():]
    expected_lock = tomllib.loads(original_lock.decode())
    workspace_names = {"icu-client", "icu-server", "icu-proto"}
    found = set()
    for package in expected_lock["package"]:
        if package["name"] in workspace_names and "source" not in package:
            if package["version"] != version:
                raise BuildError(f"Cargo.lock is out of sync for {package['name']}")
            package["version"] = new_version
            found.add(package["name"])
    if found != workspace_names:
        raise BuildError("Cargo.lock is missing ICU workspace packages")
    try:
        manifest.write_bytes(text.encode())
        run_command(["cargo", "update", "--workspace", "--offline"], root, env, log, ui)
        if tomllib.loads(lock.read_text()) != expected_lock:
            raise BuildError("Version bump changed more than ICU's workspace versions; restored original files")
    except BaseException:
        manifest.write_bytes(original_manifest)
        lock.write_bytes(original_lock)
        raise
    ui.line(f"Version bumped: {version} -> {new_version} (client + server)")
    return new_version


def active_build_processes(root):
    """Find direct Cargo/package builds as well as binaries using this target."""
    active = []
    target = root / "target"
    scripts = {"packaging/build.sh", "packaging/windows/build.sh", "packaging/windows/installer.sh",
               "packaging/smoke.sh", "packaging/build_all.py"}
    for process in Path("/proc").iterdir():
        if not process.name.isdigit() or int(process.name) == os.getpid():
            continue
        try:
            args = (process / "cmdline").read_bytes().decode(errors="replace").split("\0")
            executable = (process / "exe").resolve(strict=True)
            if executable.is_relative_to(target):
                active.append(process.name)
                continue
            building = Path(args[0]).name in {"cargo", "rustc", "rustdoc"} or any(
                arg in scripts or any(arg == str(root / script) for script in scripts) for arg in args)
            if not building:
                continue
            cwd = (process / "cwd").resolve(strict=True)
            # A rootless Podman build sees the same checkout mounted as /src.
            mounted = process / "root/src"
            if cwd.is_relative_to(root) or (mounted.exists() and mounted.samefile(root)):
                active.append(process.name)
        except (OSError, ValueError):
            continue  # Process exited, or belongs to another user.
    return active


def clean_targets(root, log, ui):
    target = root / "target"
    if target.is_symlink() or not target.is_dir():
        raise BuildError(f"Refusing to clean a symlinked or missing target directory: {target}")
    active = active_build_processes(root)
    if active:
        raise BuildError("Cannot clean while this checkout has active builds/binaries (PIDs: " +
                         ", ".join(active) + "); stop them and retry.")
    ui.line("==> clean: all compiled targets and package staging")
    # Keep the lock inode held by build_lock and the diagnostic logs. Do not
    # use cargo clean here: it would delete both along with the target tree.
    with log.open("w") as output:
        for path in sorted(target.iterdir()):
            if path.name in {"build-all.lock", "build-logs"}:
                continue
            ui.line(f"Removing {path.relative_to(root)}")
            ui.render()
            output.write(f"Removing {path.relative_to(root)}\n")
            output.flush()
            if path.is_symlink() or not path.is_dir():
                path.unlink()
            else:
                shutil.rmtree(path)
        message = "Clean complete; packages, backups, local data and build logs preserved."
        output.write(message + "\n")
        ui.line(message)


def execute_clean(root):
    """Reclaim generated build space without requiring any build toolchains."""
    with build_lock(root):
        run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ") + f"-{os.getpid()}"
        logs = root / "target/build-logs" / run_id
        logs.mkdir(parents=True)
        clean_targets(root, logs / "0-clean.log", Dashboard(True, "Cleanup only"))
        print(f"ICU CLEAN COMPLETE | Logs: {logs}")
    return 0


def execute(root, plain, bump=False, clean_build=False):
    with build_lock(root):
        version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
        release = os.environ.get("RELEASE", "1")
        if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", version):
            raise BuildError(f"Unsupported workspace version: {version}")
        if not re.fullmatch(r"[1-9][0-9]*", release):
            raise BuildError("RELEASE must be a positive package iteration number.")
        commit = capture(["git", "rev-parse", "HEAD"], root)
        dirty = bool(capture(["git", "status", "--porcelain"], root))
        run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ") + f"-{os.getpid()}"
        env = build_environment(run_id, dirty)
        logs = root / "target/build-logs" / run_id
        logs.mkdir(parents=True)
        out = root / "dist" / version
        ui = Dashboard(plain, f"v{version} | {commit[:12]} | {'uncommitted changes' if dirty else 'committed source'}")
        backup = None
        cleanup_error = None
        bumped = False
        try:
            with ui:
                ui.start(0)
                preflight(root, env, ui)
                if clean_build:
                    clean_targets(root, logs / "0-clean.log", ui)
                if bump:
                    version = bump_version(root, version, env, logs / "0-version-bump.log", ui)
                    bumped = True
                    dirty = True
                    env = build_environment(run_id, dirty)
                    out = root / "dist" / version
                    ui.title = f"v{version} | {commit[:12]} | uncommitted changes"
                fingerprint = source_digest(root)
                (logs / "source.txt").write_text(f"commit {commit}\nversion {version}\nsource-sha256 {fingerprint}\ndirty {dirty}\n")
                backup = backup_outputs(root, version, run_id)
                ui.line(f"Output: {out}")
                if backup:
                    ui.line(f"Previous output saved: {backup}")
                ui.line(f"Full logs: {logs}")
                ui.finish()
                for index, script in enumerate(("packaging/build.sh", "packaging/windows/build.sh",
                                                 "packaging/windows/installer.sh"), 1):
                    ui.start(index)
                    run_command(["bash", script], root, env, logs / f"{index}-{Path(script).parent.name}.log", ui)
                    ui.finish()
                ui.start(4)
                if capture(["git", "rev-parse", "HEAD"], root) != commit or source_digest(root) != fingerprint:
                    raise BuildError("Source changed during the build; rerun to produce one consistent set of packages.")
                artifacts = verify_artifacts(out, version, release, commit, dirty)
                ui.line("All seven artifacts and ten checksums verified")
                ui.finish()
        except (BuildError, OSError, KeyboardInterrupt) as error:
            ui.finish(False)
            message = "Build cancelled" if isinstance(error, KeyboardInterrupt) else str(error)
            (logs / "failure.txt").write_text(message + "\n" + "\n".join(ui.tail) + "\n")
            (logs / "timings.txt").write_text("\n".join(ui.summary()) + "\n")
            print(f"\nICU BUILD FAILED | {message}", file=sys.stderr)
            if ui.live:
                print("\n".join(ui.tail), file=sys.stderr)
            print(f"Logs: {logs}\nPartial outputs: {out}", file=sys.stderr)
            if backup:
                print(f"Previous output: {backup}", file=sys.stderr)
            if bumped:
                print(f"Version remains {version}; retry ./build.sh without --bump.", file=sys.stderr)
            return 130 if isinstance(error, KeyboardInterrupt) else 1
        finally:
            try:
                cleanup_containers(root, env, run_id)
            except BuildError as error:
                cleanup_error = error
                print(f"Container cleanup failed: {error}", file=sys.stderr)
        if cleanup_error:
            (logs / "failure.txt").write_text(f"Container cleanup failed: {cleanup_error}\n")
            print(f"ICU BUILD FAILED | container cleanup | Logs: {logs}", file=sys.stderr)
            return 1
        print(f"\nICU BUILD COMPLETE | {elapsed(time.monotonic() - ui.started)} | 7 artifacts | checksums verified")
        for index, name in enumerate(STAGES):
            print(f"  PASS {name:<34} {elapsed(ui.times[index])}")
        print("\nTIME BY COMPONENT")
        timings = ui.summary()
        print("\n".join(timings))
        (logs / "timings.txt").write_text("\n".join(timings) + "\n")
        print(f"\n{'ARTIFACT':<58} {'MiB':>9}")
        for name in artifacts:
            print(f"{name:<58} {(out / name).stat().st_size / 1024 ** 2:>9.2f}")
        print(f"\nPackages: {out}\nLogs: {logs}")
        if backup:
            print(f"Previous output: {backup}")
        return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plain", action="store_true", help="stream logs without the live dashboard")
    parser.add_argument("--bump", action="store_true",
                        help="increment the shared client/server patch version before building (e.g. 0.1.0 -> 0.1.1)")
    parser.add_argument("--clean", action="store_true",
                        help="remove all compiled targets and staging before rebuilding; keep packages and logs")
    parser.add_argument("--clean-only", action="store_true",
                        help="remove compiled targets and staging without building; keep packages and logs")
    args = parser.parse_args()
    if args.clean_only and (args.clean or args.bump):
        parser.error("--clean-only cannot be combined with --clean or --bump")

    def interrupted(_signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    try:
        if args.clean_only:
            return execute_clean(ROOT)
        return execute(ROOT, args.plain, args.bump, args.clean)
    except (BuildError, OSError, ValueError, KeyError) as error:
        print(f"ICU build: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
