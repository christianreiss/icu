#!/usr/bin/env python3
"""Native ducking acceptance with an isolated PipeWire null sink.

Requires user PipeWire/WirePlumber, pactl, pw-play, pw-dump, pw-cli and Cargo.
Never changes the default sink or real application volumes. Fixtures are removed.
"""
import argparse
import json
import math
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import time
import wave


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def worker_checks(binary, env, node, sink):
    def levels():
        objects = json.loads(run("pw-dump"))
        entry = next(o for o in objects if o["id"] == node)
        return entry["info"]["params"]["Props"][0]

    baseline = levels()["channelVolumes"]
    packet = (json.dumps(dict(mode="medium", route=sink, online=True,
                             transmitting_voice=True, speech_db=None, noisy_room=False)) + "\n").encode()

    logs = []
    def start():
        log = tempfile.TemporaryFile(mode="a+")
        logs.append(log)
        worker = subprocess.Popen([str(binary), "--ducking-worker"], stdin=subprocess.PIPE,
                                  stdout=log, stderr=subprocess.PIPE, env=env)
        worker.status_log = log
        return worker

    def duck(worker):
        for _ in range(25):
            worker.stdin.write(packet)
            worker.stdin.flush()
            time.sleep(0.04)
        worker.status_log.seek(0)
        reports = [json.loads(line) for line in worker.status_log]
        assert reports and reports[-1]["sessions"] == 1, reports
        assert abs(reports[-1]["attenuation_db"] - 12) < .1, reports
        props = levels()
        assert props["channelVolumes"] == baseline, props
        for value, base in zip(props["softVolumes"], baseline):
            assert abs(value - base * 10 ** (-12 / 20)) < 0.001, props

    def restored():
        props = levels()
        assert props["channelVolumes"] == baseline, props
        assert all(abs(a-b) < 0.001 for a,b in zip(props["softVolumes"], baseline)), props

    worker = start()
    sender = None
    try:
        duck(worker)
        # A stalled parent cannot retain attenuation while its pipe stays open.
        time.sleep(1.7)
        restored()
        duck(worker)
        worker.stdin.close()  # Exactly the EOF caused by a killed parent process.
        worker.wait(timeout=5)
        assert worker.returncode == 0
        restored()
        # A real sender-process SIGKILL closes the guardian pipe without any
        # shutdown hook; the parent Python process holds no writer descriptor.
        read_fd, write_fd = os.pipe()
        worker = subprocess.Popen([str(binary), "--ducking-worker"], stdin=read_fd,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=env)
        os.close(read_fd)
        sender_code = "import sys,time\np=" + repr(packet) + "\nwhile True:\n sys.stdout.buffer.write(p);sys.stdout.buffer.flush();time.sleep(.04)\n"
        sender = subprocess.Popen([sys.executable, "-c", sender_code], stdout=write_fd)
        os.close(write_fd)
        time.sleep(1.2)
        props = levels()
        assert all(abs(a-b*10**(-12/20)) < .001 for a,b in zip(props["softVolumes"],baseline)), props
        sender.kill()
        sender.wait(timeout=3)
        worker.wait(timeout=5)
        assert worker.returncode == 0
        restored()
        worker = start()
        duck(worker)
        worker.kill()  # Kill the guardian itself: restart must consume its journal.
        worker.wait(timeout=3)
        worker.stdin.close()
        worker = start()
        time.sleep(1.0)
        restored()
        worker.stdin.close()
        worker.wait(timeout=5)
        assert worker.returncode == 0
        print("Native guardian: heartbeat expiry, parent EOF/SIGKILL, guardian SIGKILL + journal restart PASS")
    finally:
        if sender is not None and sender.poll() is None:
            sender.kill()
            sender.wait(timeout=3)
        if worker.poll() is None:
            if worker.stdin and not worker.stdin.closed:
                worker.stdin.close()
            try:
                worker.wait(timeout=5)
            except subprocess.TimeoutExpired:
                worker.kill()
                worker.wait(timeout=3)
        for log in logs:
            log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker-binary", type=Path, help="also test crash restoration with this built icu binary")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    sink = f"icu_ducking_acceptance_{os.getpid()}"
    name = f"ducking-acceptance-background-{os.getpid()}"
    module = None
    player = None
    with tempfile.TemporaryDirectory(prefix="icu-ducking-native-") as scratch:
        try:
            module = run("pactl", "load-module", "module-null-sink", f"sink_name={sink}",
                         f"sink_properties=device.description={sink}", "channels=2")
            path = Path(scratch) / "voice.wav"
            one_second = b"".join(struct.pack("<hh", int(3500 * math.sin(2 * math.pi * 440 * n / 48000)),
                                                      int(3500 * math.sin(2 * math.pi * 660 * n / 48000)))
                                  for n in range(48000))
            with wave.open(str(path), "wb") as audio:
                audio.setnchannels(2)
                audio.setsampwidth(2)
                audio.setframerate(48000)
                for _ in range(180):
                    audio.writeframesraw(one_second)
            play_command = ["pw-play", f"--target={sink}", "--properties",
                            f"{{ application.name=DuckingAcceptanceBackground node.name={name} }}", str(path)]
            player = subprocess.Popen(play_command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            node = None
            for _ in range(100):
                objects = json.loads(run("pw-dump"))
                node = next((o["id"] for o in objects if o.get("info", {}).get("props", {}).get("node.name") == name), None)
                if node is not None:
                    break
                time.sleep(0.1)
            if node is None:
                raise RuntimeError("Synthetic playback node did not appear")
            run("pw-cli", "set-param", str(node), "Props", "{ channelVolumes: [ 0.63, 0.315 ] }")
            time.sleep(0.3)
            env = dict(os.environ, ICU_DUCKING_TEST_SINK=sink, ICU_DUCKING_TEST_NODE=str(node),
                       XDG_DATA_HOME=str(Path(scratch) / "data"))
            subprocess.run(["cargo", "test", "--locked", "-p", "icu-client", "--lib",
                            "native_null_sink_attenuation_restore_and_manual_override", "--", "--ignored", "--nocapture"],
                           cwd=root, env=env, check=True, timeout=150)
            print("Native PipeWire: all five modes, measured RMS, channel balance, restore, manual override PASS")
            if args.worker_binary:
                worker_checks(args.worker_binary.resolve(), env, node, sink)
                old = next(o for o in json.loads(run("pw-dump")) if o["id"] == node)
                baseline = old["info"]["params"]["Props"][0]["channelVolumes"]
                old_serial = old["info"]["props"]["object.serial"]
                # Kill the foreign application while it is actually attenuated;
                # restarting only after an orderly restore could hide persistence.
                guardian = subprocess.Popen([str(args.worker_binary.resolve()), "--ducking-worker"],
                                            stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                            stderr=subprocess.PIPE, env=env)
                try:
                    packet = (json.dumps(dict(mode="medium", route=sink, online=True,
                                              transmitting_voice=True, speech_db=None, noisy_room=False)) + "\n").encode()
                    for _ in range(25):
                        guardian.stdin.write(packet)
                        guardian.stdin.flush()
                        time.sleep(.04)
                    active = next(o for o in json.loads(run("pw-dump")) if o["id"] == node)
                    props = active["info"]["params"]["Props"][0]
                    assert props["channelVolumes"] == baseline
                    assert all(abs(a-b*10**(-12/20)) < .001 for a,b in zip(props["softVolumes"],baseline))
                    player.kill()
                    player.wait(timeout=3)
                finally:
                    guardian.stdin.close()
                    try:
                        guardian.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        guardian.kill()
                        guardian.wait(timeout=3)
                time.sleep(.4)
                player = subprocess.Popen(play_command, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                restarted = None
                for _ in range(30):
                    restarted = next((o for o in json.loads(run("pw-dump")) if
                                      o.get("info", {}).get("props", {}).get("node.name") == name and
                                      o["info"]["props"]["object.serial"] != old_serial), None)
                    if restarted is not None:
                        break
                    time.sleep(.1)
                assert restarted is not None
                time.sleep(.5)
                restarted = next(o for o in json.loads(run("pw-dump")) if o["id"] == restarted["id"])
                channels = restarted["info"]["params"]["Props"][0]["channelVolumes"]
                assert all(abs(a-b) < .001 for a,b in zip(channels, baseline)), (channels, baseline)
                print("Native application restart: persistent channel baseline retained PASS")
        finally:
            if player is not None:
                player.terminate()
                try:
                    player.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    player.kill()
                    player.wait(timeout=3)
            if module is not None:
                subprocess.run(["pactl", "unload-module", module], check=True)


if __name__ == "__main__":
    main()
