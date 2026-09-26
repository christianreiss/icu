# Windows release gate — 2026-09-26

## Scope and verdict

Windows feature parity was checked against the Linux client at `180fa55`.
The fixes below cover defects found in code review and on the supplied
Windows VM. The automated and VM checks pass. This is a tested release
candidate, with **production approval still conditional on the physical
device and graphics checks below**.

Target: `10.2.0.201`, Windows 10 Pro N 22H2, build 19045.6456, eight virtual
CPUs, QEMU HDA audio, 1024×768 desktop. Tests ran in Buffies' interactive
desktop session through scheduled tasks; SSH-only execution was used for
unit tests. The installed Media Foundation components were available.
The VM uses software graphics (D3D/WARP). A Logitech C270 camera and its
USB microphone were temporarily passed through for the hardware check below;
no physical speaker/headset acceptance has been completed.

An isolated native Linux server listened on `10.2.0.10:4713`, with new test
identities and state under `.scratch/windows-gate`. The Linux comparison
peer used fake capture with a test tone and a synthetic camera. Existing
server configuration and identities were kept separate. No containers
were needed.

## Fixed defects

| Defect | Result |
| --- | --- |
| Windows ignored portal-only key combinations imported in a Linux passport. | Raw Input recognizes CTRL/SHIFT/ALT/LOGO combinations when no physical key is saved. Releasing any required key ends PTT; a saved physical binding remains authoritative. Labels show the actual combination. |
| WASAPI reopened individual streams without resetting the complete audio pipeline. | Default-device changes and stream errors reach the engine, which reopens capture, playback and reference together and resets AEC. Initial open failures also enter bounded retry. |
| Failed playback could leave the DSP waiting for a silent loopback reference. | The reference stream and reference flag are cleared when playback cannot open. |
| A recovered audio device left its old error banner visible. | Reopening replaces an audio error with the current outcome while preserving unrelated problems. |
| A silent named-pipe client could block handoff; acknowledgement handling could lose or repeat requests. | Async I/O, eight active clients maximum, two-second request timeout, three-second handoff deadline, exclusive first pipe, receipt acknowledgement and no request replay after sending. Dropping the listener cancels pending I/O. |
| Toast workers did not balance COM initialization. | Every successful initialization, including `S_FALSE`, is paired with `CoUninitialize`. |
| Main, setup, settings and invitation windows could extend below the taskbar. | Windows windows are fitted to the usable monitor rectangle, with title-bar margins and capped minimum sizes. Subsequent growth respects available space. |
| Close-to-tray kept rendering an invisible Win32 window. | Hidden Windows windows are also minimized so eframe skips rendering; restore explicitly removes minimization. Connected hidden CPU fell from about two cores to near idle in the VM. |
| Minimized enrollment did not process handoff/completion; an idle invitation repainted continuously. | Handoff and enrollment completion run in `App::logic`, with wake callbacks. Only pending registration needs periodic polling. Idle invitation CPU fell from several cores to near idle. |
| Attaching to a Windows parent console discarded inherited output redirection. | Preserve redirected stdin/stdout/stderr while attaching; CLI help, errors and pipes reach their intended destination. Native regression fails on the prior candidate and passes with the fix. |
| Windows packaging could omit third-party notices and reuse Linux-only attribution. | Packaging requires `cargo deny` and generates notices for the actual Windows target/features, including native audio/video dependencies and the embedded Inter font. Missing notices fail the build. |

## Verification

- Linux: `cargo test --workspace --locked` — **593 passed, 0 failed,
  5 ignored**, across 26 suites. Two playout continuity tests failed during
  a concurrent release build; the complete rerun without build load passed.
  Their assertions and thresholds were not weakened. The final Windows-only
  main-window bounds change also passed all nine UI layout tests.
- Windows: cross-built client library tests executed natively on the VM
  with `--test-threads=2` — **231 passed, 0 failed, 3 ignored**.
- Strict Clippy: Linux workspace and Windows client, `--all-targets --
  -D warnings`, passed. The existing cross-linker COMDAT warnings from
  prefixed AEC symbols remain; native AEC tests pass.
- Windows package: release build with AEC, GUI, camera, tray and overlay;
  system-DLL import allowlist and dependency/license checks pass.
- Real WASAPI probe: three-second quiet tone, approximately 145,680 capture
  samples and 144,960 reference samples, 143,792 nonzero reference samples.
  Capture, playback and loopback all ran in the interactive Windows session.
- Real Raw Input probe: F12 and Pause each produced press and release
  transitions. Imported CTRL+SHIFT+F12 worked with another app focused;
  bare F12 did not transmit, and releasing CTRL stopped transmission.
- Linux ↔ Windows voice and synthetic video worked through the isolated
  server. The Linux receiver decoded 1,361 Windows voice packets with zero
  bad packets in the PTT test. Native Windows overlay showed the peer.
- Physical camera: Logitech C270 over live USB passthrough, real `--camera 0`
  on Windows, with a Linux decoder probe: **1,856 nonuniform decoded frames,
  320×240, approximately 15 fps over 150 seconds**, including an 18-second
  USB disconnect. Frames resumed automatically after reattachment; the
  session stayed connected. Camera images were not saved. The microphone
  switched to C270, fell back to HDA when unplugged and reopened on device
  changes. This establishes input hotplug, not acoustic AEC quality. The
  camera was returned to Linux and temporary tasks/server removed.
- Native CLI regression: `windows_console` verifies redirected help, errors,
  NUL and piped version output under a real `cmd.exe` console. The previous
  installed release failed (all output empty); the fixed executable passed.
- Audio-service restart: Audiosrv stopped for five seconds and restarted;
  capture/reference/playback recovered, the stale banner cleared and the
  network session stayed connected.
- Passport export/import worked through the GUI, with settings; the saved
  Windows identity used the DPAPI envelope. Import into another test
  configuration replaced the earlier session as expected.
- Setup completed at 1024×768 with the final action visible. Tray mute
  changed the actual client state. Close-to-tray and a separate second
  launch restored the same process without another connection. A minimized
  invitation accepted a passport handoff and restored its preview.
- A native Windows join toast was visible on the desktop.

Repeatable commands and interactive probe instructions are in
[`Client/LIVETEST.md`](../Client/LIVETEST.md), section 7. Local logs and
screenshots are retained under `.scratch/windows-gate`; they are excluded
from Git because that directory also contains test credentials.

## Delivery and rollback

The local branch is `fix/windows-release-gate`; no shared branch was pushed.
The portable package is
`dist/0.1.0/icu-0.1.0-windows-x86_64.zip`, with its digest in the adjacent
`SHA256SUMS`. The installed executable on `10.2.0.201` is
`C:\Users\Buffies\icu-0.1.0-windows-x86_64\icu.exe` and matches the built
executable:

```text
a463f662a128a4f34d0fd8953a2ec910cdb5f7609a9fdb3de29609907ead9750
```

The previous package is preserved at
`C:\Users\Buffies\icu-gate\original-20260926`; its executable SHA-256 is
`223956869b86b4d2bebb3c9b2072dcdaba8792702f958f31ec61925dc70ecbd5`.
To roll back, quit ICU, copy that backup's files over the installed package,
then launch ICU again. The existing user's identity and settings were not
replaced. Test identities and settings were confined to `icu-gate`.
The normal VM profile retains its existing identity and server at
`10.2.0.10:4712` (display name `localhost`); parity tests used the separate
LAN profile above. The
isolated server/peers and temporary `ICU-Gate*` scheduled tasks were stopped
or removed after testing; Audiosrv is running. Large test executables and
intermediate ZIPs were removed; logs, the candidate and rollback copy remain.

For hands-on review, the existing VM backend was restarted as the transient
user service `icu-windows-vm-gate` on this Linux host, bound to
`10.2.0.10:4712`. Its test CA/users remain in `/home/chris/icu-live-win/server`;
the original configuration was preserved. The LAN-only configuration is
`.scratch/windows-gate/vm-review-server.toml`. Stop it with
`systemctl --user stop icu-windows-vm-gate`; it is not a production service.

## Remaining production checks

1. A physical Windows microphone/speaker or headset pair: acoustic echo
   cancellation, double-talk, ten-minute clock drift and unplug/replug or
   switching between distinct real endpoints. VM loopback proves plumbing,
   not acoustic performance.
2. A normal hardware graphics adapter and camera preview in the GUI. Physical
   camera capture/encode/network/decode and hotplug passed, but active GUI rendering on this software-rendered VM can still
   consume several CPU cores; hidden and idle invitation rendering is fixed.
   Measure voice continuity with camera/overlay enabled on target hardware.
3. Global PTT and click-through/topmost overlay over an actual borderless
   game, including gaming mouse buttons and elevated applications; actual
   mixed-DPI/multi-monitor use. Geometry is unit-tested, but the live desktop
   check was at 100% scaling.
4. Windows 11 and first-run installation/SmartScreen on a fresh normal
   workstation. The portable executable remains unsigned. This session did
   not publish a release or deploy to production.

Do not describe these remaining checks as passed on the strength of the VM
or the synthetic AEC/video tests.
