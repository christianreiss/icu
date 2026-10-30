#!/usr/bin/env python3
"""Build/run the same independent probe against an already-built baseline rlib.

python3 experiments/audio-quality.py --baseline /path/to/unchanged/checkout
The baseline must first pass `cargo test --locked --workspace` using its own
target directory. This script never edits it and never shares Cargo targets.
Current production code is run with Cargo. WAVs and CSVs stay under target/.
"""
import argparse
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--baseline", type=Path, required=True)
parser.add_argument("--output", type=Path, default=ROOT / "target/audio-quality")
args = parser.parse_args()
base = args.baseline.resolve()
target = base / "target/debug"
libs = list((target / "deps").glob("libicu_client-*.rlib"))
if len(libs) != 1:
    raise SystemExit("Baseline needs exactly one built icu_client rlib; use a dedicated target.")
args.output.mkdir(parents=True, exist_ok=True)
binary = args.output.resolve() / "baseline-probe"
cmd = ["rustc", "--edition=2021", "--cfg", "icu_legacy_audio", "--extern",
       f"icu_client={libs[0]}", "-L", f"dependency={target / 'deps'}", "-o", str(binary),
       str(ROOT / "crates/icu-client/examples/audio_quality.rs")]
for directory in sorted({p.parent for p in (target / "build").rglob("*.a")}):
    cmd += ["-L", f"native={directory}"]
subprocess.run(cmd, cwd=ROOT, check=True)
subprocess.run([str(binary), str(args.output.resolve() / "before")], check=True)
subprocess.run(["cargo", "run", "--locked", "-p", "icu-client", "--example", "audio_quality",
                "--", str(args.output.resolve() / "after")], cwd=ROOT, check=True)
print(f"Comparison CSVs and float WAVs: {args.output.resolve()}")
print("Offline masked RMS/envelope lag are not LUFS, a listening score or mouth-to-ear latency.")
