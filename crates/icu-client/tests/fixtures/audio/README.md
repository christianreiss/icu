# Audio fixtures

Downloaded 2026-10-30. These are public test recordings, never ICU conversations.
Both files were converted with FFmpeg to 48 kHz, mono, signed 16-bit PCM WAV;
metadata removed. No gain, denoising or other processing was applied. Conversion
command: `ffmpeg -i ORIGINAL -ar 48000 -ac 1 -c:a pcm_s16le -map_metadata -1 OUTPUT.wav`.
These fixtures are **CC BY 4.0**, separate from the source-code license.

* `voices-sp0307.wav`: 3.4 s of real read speech,
  `Lab41-SRI-VOiCES-src-sp0307-ch127535-sg0042.wav`, from **Lab41 and SRI
  International**, Voices Obscured in Complex Environmental Settings (VOiCES).
  [Dataset and license](https://iqtlabs.github.io/voices/Lab41-SRI-VOiCES_README/),
  [unaltered upstream example](https://download.pytorch.org/torchaudio/tutorial-assets/Lab41-SRI-VOiCES-src-sp0307-ch127535-sg0042.wav).
  SHA-256: `89c975a660fc1c764dcc49f1da330628e9af1415f7a4b4b330c85673ee0f70bf`.
* `keychron-v1.wav`: real typing, **C40115**, “Typing on Keychron V1 Ultra
  (Red Linear Switch)”, recorded 2026-06-05 using a MacBook Air.
  [Source and attribution](https://commons.wikimedia.org/wiki/File:Typing_on_Keychron_V1_Ultra_(Red_Linear_Switch).wav).
  The file description's author-granted license is CC BY 4.0 (its structured
  data carries an inconsistent BY-SA tag; attribution follows the explicit
  license statement on the file page).
  SHA-256: `7fe13b4318ede259ab8f7f7f42432b3ed32527622a8027c281c762c7c2d2782e`.

[Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/).
Preserve attribution and identify changes when redistributing these recordings
or generated comparison clips. No endorsement by the authors is implied.

The real recordings complement seeded synthetic speech and click/noise cases.
One voice and one keyboard cannot establish population-wide quality, whisper,
laughter, breath, hardware-AEC or subjective naturalness acceptance.
