#!/usr/bin/env python3
"""Exercise real PipeWire channel negotiation using only temporary virtual nodes.

Run from any directory: python3 crates/icu-client/tests/native_channels.py
Requires pw-loopback, pw-cat and pw-dump. No device volumes or defaults change.
An existing Cargo test executable can be supplied with --test-binary PATH.
"""
import argparse
import array
import json
import math
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
TEST = "audio::pipewire::tests::native_stereo_capture_and_reference_keep_independent_channels"
SWITCH_TEST = "audio::pipewire::tests::native_automatic_channel_change_keeps_capture_running"


def nodes():
    return json.loads(subprocess.check_output(["pw-dump"], text=True))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test-binary", type=Path)
    args = parser.parse_args()
    binary = args.test_binary
    if binary is None:
        build = subprocess.run(
            ["cargo", "test", "--locked", "-p", "icu-client", "--lib", "--no-run", "--message-format=json"],
            cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True,
        )
        for line in build.stdout.splitlines():
            artifact = json.loads(line)
            if artifact.get("reason") == "compiler-artifact" and artifact.get("target", {}).get("name") == "icu_client" and artifact.get("executable"):
                binary = Path(artifact["executable"])
        if binary is None:
            raise RuntimeError("Cargo did not return the client unit-test executable")
    binary = binary.resolve()
    prefix = f"icu-voice-test-{os.getpid()}"
    source, sink = f"{prefix}-source", f"{prefix}-sink"
    children = []
    with tempfile.TemporaryDirectory(prefix="icu-native-channels-") as directory:
        directory = Path(directory)
        try:
            def properties(name, kind):
                return json.dumps({"node.name": name, "media.class": kind, "node.virtual": True, "priority.session": 0, "node.autoconnect": False})

            with (directory / "loopback.log").open("w+") as loop_log:
                children.append(subprocess.Popen([
                    "pw-loopback", f"--name={prefix}", "--channels=2",
                    "--capture-props=" + properties(sink, "Audio/Sink"),
                    "--playback-props=" + properties(source, "Audio/Source"),
                ], stdout=loop_log, stderr=subprocess.STDOUT))
                deadline = time.monotonic() + 5
                while True:
                    names = {node.get("info", {}).get("props", {}).get("node.name") for node in nodes()}
                    if source in names and sink in names:
                        break
                    if time.monotonic() >= deadline:
                        loop_log.seek(0)
                        raise RuntimeError("Virtual nodes did not appear: " + loop_log.read())
                    time.sleep(0.05)
                signal = array.array("f")
                left = array.array("f")
                right = array.array("f")
                for i in range(48_000):
                    sample = 0.2 * math.sin(2 * math.pi * 997 * i / 48_000)
                    signal.extend((sample, -sample))
                    left.extend((sample, 0.0))
                    right.extend((0.0, sample))
                raw = directory / "antiphase.f32"
                with raw.open("wb") as output:
                    for second in range(15):
                        (signal if second < 2 else left if second < 4 else right).tofile(output)
                with raw.open("rb") as audio, (directory / "playback.log").open("w+") as play_log:
                    children.append(subprocess.Popen([
                        "pw-cat", "--raw", "--playback", "--format=f32", "--rate=48000",
                        "--channels=2", f"--target={sink}", "-",
                    ], stdin=audio, stdout=play_log, stderr=subprocess.STDOUT))
                    time.sleep(0.2)
                    environment = dict(os.environ, ICU_CAPTURE_TEST_SOURCE=source, ICU_REFERENCE_TEST_SINK=sink)
                    subprocess.run([str(binary), "--ignored", "--exact", TEST, "--nocapture"], cwd=ROOT, env=environment, timeout=15, check=True)
                    subprocess.run([str(binary), "--ignored", "--exact", SWITCH_TEST, "--nocapture"], cwd=ROOT, env=environment, timeout=15, check=True)
        finally:
            for child in reversed(children):
                if child.poll() is not None:
                    continue
                child.terminate()
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
    remaining = [node.get("info", {}).get("props", {}).get("node.name", "") for node in nodes()]
    if any(name.startswith(prefix) for name in remaining):
        raise RuntimeError("Temporary PipeWire nodes were not removed")
    print("Native capture/reference channel regression passed; temporary nodes removed.")


if __name__ == "__main__":
    main()
