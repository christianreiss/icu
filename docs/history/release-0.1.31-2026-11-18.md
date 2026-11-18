# ICU 0.1.31

Cameras with a fixed capture delay can send moving video and keep their original
timestamps. A stalled read now returns control to the camera supervisor. The
receiver preserves recovery frames when packets change order or the decoder
falls behind. A stable network route can regain full frame rate after an RTT
change.

The viewer uses PipeWire or WASAPI playback timing to align each speaker's
picture with their voice. Changing output devices clears previous timing
observations. Estimated output timing gradually releases an existing hold.

On Fedora, a physical C270 delivered 239 decoded pictures through a local
relay in 20 seconds. A simulated 300 ms output delay produced a 321 ms picture
hold compared with 19 ms on direct output. The Windows 11 virtual audio endpoint
returned 566 measured presentation times in a three-second run. These tests
measure capture, decode and device clock behavior. Physical display-to-speaker
lip sync requires a clap test at the receiving device.

The preceding stable release is 0.1.30. Its installer and Linux packages remain
available under [downloads](https://misc.alpha-labs.net/icu/downloads/0.1.30/).
