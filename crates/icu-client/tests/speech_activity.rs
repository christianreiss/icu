//! Actual recordings complement the deterministic tone/noise rejection cases.
use icu_client::dsp::normalize::SpeechActivity;

fn wav(bytes: &[u8]) -> Vec<f32> {
    assert_eq!(&bytes[..4], b"RIFF");
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let start = at + 8;
        if &bytes[at..at + 4] == b"data" {
            return bytes[start..start + len]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
                .collect();
        }
        at = start + len + (len % 2);
    }
    panic!("missing PCM16 data");
}

#[test]
fn actual_speech_can_duck_but_real_keyboard_without_voice_cannot() {
    let speech = wav(include_bytes!("fixtures/audio/voices-sp0307.wav"));
    let keyboard = wav(include_bytes!("fixtures/audio/keychron-v1.wav"));
    let mut detector = SpeechActivity::default();
    let voice_blocks = speech
        .chunks(480)
        .filter(|b| detector.observe(b, true))
        .count();
    let mut detector = SpeechActivity::default();
    let typing_blocks = keyboard
        .chunks(480)
        .filter(|b| detector.observe(b, true))
        .count();
    eprintln!(
        "strict activity: VOiCES={voice_blocks}/{} blocks, Keychron={typing_blocks}/{} blocks",
        speech.len().div_ceil(480),
        keyboard.len().div_ceil(480)
    );
    assert!(
        voice_blocks >= 20,
        "real speech must produce sustained evidence"
    );
    assert_eq!(typing_blocks, 0, "keyboard transients cannot start ducking");
}

#[test]
fn stationary_noise_tones_clicks_and_concealment_cannot_start_activity() {
    for kind in 0..4 {
        let mut detector = SpeechActivity::default();
        let mut state = 42u32;
        for block in 0..400 {
            let mut samples = [0.0; 480];
            for (i, s) in samples.iter_mut().enumerate() {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let t = (block * 480 + i) as f32 / 48_000.0;
                *s = match kind {
                    0 => 0.1 * (t * std::f32::consts::TAU * 180.0).sin(),
                    1 => 0.1 * (state as f32 / u32::MAX as f32 - 0.5),
                    2 if (block * 480 + i) % 12_000 < 20 => 0.5,
                    _ => 0.0,
                };
            }
            assert!(!detector.observe(&samples, true), "non-speech kind {kind}");
        }
    }
    let speech = wav(include_bytes!("fixtures/audio/voices-sp0307.wav"));
    let mut detector = SpeechActivity::default();
    for block in speech.chunks(480) {
        assert!(
            !detector.observe(block, false),
            "PLC/FEC never establishes speech"
        );
    }
    for block in speech.chunks(480) {
        detector.observe(block, true);
    }
    assert!(!detector.observe(&[f32::NAN; 480], true));
    assert!(!detector.observe(&[0.0; 480], true));
}
