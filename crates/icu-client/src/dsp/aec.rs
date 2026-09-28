//! AEC3 through `webrtc-audio-processing` (`docs/research/aec.md`).
//!
//! 480-sample frames in ±1.0. Per tick: render (reference) first, then
//! capture. No `stream_delay_ms` (AEC3 ignores it). Behind feature `aec`; the
//! stub without the feature keeps the engine code identical.

/// Samples per APM frame at 48 kHz.
pub const FRAME: usize = 480;

/// APM statistics the UI diagnoses.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct AecStats {
    /// Echo return loss in dB, if the APM has an estimate.
    pub erl_db: Option<f64>,
    /// Echo return loss enhancement in dB.
    pub erle_db: Option<f64>,
    /// Estimated delay in ms. Near 0 on speakers means the reference
    /// arrives late (`dsp::refalign`).
    pub delay_ms: Option<u32>,
    /// Likelihood (0–1) that residual echo is left in the output.
    pub residual_echo_likelihood: Option<f64>,
}

/// AEC3 reports the echo return loss as `−10·log10(capture / render
/// power)`, starting from its `kMaxErl = 1000` — exactly −30 dB — until a
/// filter has converged on an echo path, and doubling back towards it once
/// the path is gone (`aec3/erl_estimator.cc`). A value at that floor
/// therefore means "no echo path", a positive one "the mic hears the
/// loudspeaker" (measured: 7–20 dB with a room, −30 dB throughout on a
/// headset scene).
pub const ERL_NO_PATH_DB: f64 = -29.0;
/// With an echo path, less than this much cancellation is "not removed":
/// under a quarter of the echo power gone. A wrong reference (game audio
/// missing from it) measures 0–8 dB, a working one 16–28 dB.
pub const ERLE_POOR_DB: f64 = 6.0;
/// Reports come every 2 s: 10 s of poor cancellation, 30 s without a path.
pub const POOR_REPORTS: u32 = 5;
pub const NO_PATH_REPORTS: u32 = 15;

/// What the statistics say to the user (`docs/reference/client.md` §6,
/// `docs/research/aec.md` "Laufende Selbstdiagnose").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Verdict {
    /// Echo detected but not removed: check the reference device.
    pub suspect: bool,
    /// No echo path in 30 s of far-end sound: the canceller may be
    /// switched off.
    pub no_echo: bool,
    /// The far end is heard on the loudspeaker but the reference stream
    /// stays silent: it is not capturing what is played.
    pub reference_silent: bool,
}

/// Folds the periodic statistics into a [`Verdict`]. Reports without
/// far-end sound carry no information and leave the streaks as they are.
#[derive(Debug, Clone, Copy, Default)]
pub struct Diagnosis {
    poor: u32,
    no_path: u32,
    silent: u32,
}

impl Diagnosis {
    /// One report: `far_active` — the reference carried sound since the
    /// last one; `played` — the mixer put sound on the loudspeaker.
    pub fn report(&mut self, far_active: bool, played: bool, stats: &AecStats) -> Verdict {
        if far_active {
            match (stats.erl_db, stats.erle_db) {
                (Some(erl), Some(erle)) if erl > ERL_NO_PATH_DB => {
                    self.no_path = 0;
                    self.poor = if erle < ERLE_POOR_DB {
                        self.poor + 1
                    } else {
                        0
                    };
                }
                (Some(erl), _) if erl <= ERL_NO_PATH_DB => {
                    self.no_path += 1;
                    self.poor = 0;
                }
                _ => {}
            }
        }
        self.silent = if played && !far_active {
            self.silent + 1
        } else {
            0
        };
        Verdict {
            suspect: self.poor >= POOR_REPORTS,
            no_echo: self.no_path >= NO_PATH_REPORTS,
            reference_silent: self.silent >= POOR_REPORTS,
        }
    }

    /// Forgets everything (the canceller was reset or switched).
    pub fn reset(&mut self) {
        *self = Diagnosis::default();
    }
}

#[cfg(test)]
mod diagnosis_tests {
    use super::*;

    fn stats(erl: f64, erle: f64) -> AecStats {
        AecStats {
            erl_db: Some(erl),
            erle_db: Some(erle),
            delay_ms: Some(72),
            residual_echo_likelihood: Some(0.2),
        }
    }

    #[test]
    fn headset_reads_as_no_echo_after_30_s() {
        let mut d = Diagnosis::default();
        for i in 1..=NO_PATH_REPORTS {
            let v = d.report(true, true, &stats(-30.0, 0.18));
            assert_eq!(v.no_echo, i == NO_PATH_REPORTS, "report {i}: {v:?}");
            assert!(!v.suspect, "a headset is never 'echo not removed'");
        }
    }

    #[test]
    fn wrong_reference_reads_as_suspect_after_10_s() {
        let mut d = Diagnosis::default();
        for i in 1..=POOR_REPORTS {
            let v = d.report(true, true, &stats(12.0, 4.5));
            assert_eq!(v.suspect, i == POOR_REPORTS, "report {i}: {v:?}");
            assert!(!v.no_echo);
        }
    }

    #[test]
    fn working_canceller_reads_as_neither() {
        let mut d = Diagnosis::default();
        for _ in 0..40 {
            assert_eq!(d.report(true, true, &stats(18.0, 25.0)), Verdict::default());
        }
    }

    #[test]
    fn double_talk_dips_do_not_add_up() {
        // Near-end speech pulls the ERLE estimate down for a report or two
        // at a time; only a run of poor reports counts.
        let mut d = Diagnosis::default();
        for i in 0..40 {
            let erle = if i % 3 == 0 { 2.0 } else { 20.0 };
            assert!(!d.report(true, true, &stats(15.0, erle)).suspect);
        }
    }

    #[test]
    fn far_end_pauses_hold_the_streaks() {
        let mut d = Diagnosis::default();
        for _ in 0..NO_PATH_REPORTS - 1 {
            d.report(true, true, &stats(-30.0, 0.18));
        }
        for _ in 0..10 {
            assert!(!d.report(false, false, &stats(-30.0, 0.18)).no_echo);
        }
        assert!(d.report(true, true, &stats(-30.0, 0.18)).no_echo);
    }

    #[test]
    fn echo_path_found_resets_the_no_echo_count() {
        let mut d = Diagnosis::default();
        for _ in 0..NO_PATH_REPORTS - 1 {
            d.report(true, true, &stats(-30.0, 0.18));
        }
        d.report(true, true, &stats(10.0, 20.0));
        for _ in 0..NO_PATH_REPORTS - 1 {
            assert!(!d.report(true, true, &stats(-30.0, 0.18)).no_echo);
        }
    }

    #[test]
    fn silent_reference_while_playing_is_flagged() {
        let mut d = Diagnosis::default();
        for i in 1..=POOR_REPORTS {
            let v = d.report(false, true, &AecStats::default());
            assert_eq!(v.reference_silent, i == POOR_REPORTS, "report {i}");
        }
        assert!(!d.report(true, true, &stats(-30.0, 0.18)).reference_silent);
        // Nothing played, nothing to see.
        let mut d = Diagnosis::default();
        for _ in 0..20 {
            assert!(
                !d.report(false, false, &AecStats::default())
                    .reference_silent
            );
        }
    }

    #[test]
    fn missing_statistics_change_nothing() {
        let mut d = Diagnosis::default();
        for _ in 0..40 {
            assert_eq!(
                d.report(true, true, &AecStats::default()),
                Verdict::default()
            );
        }
    }
}

#[cfg(feature = "aec")]
mod real {
    use super::{AecStats, FRAME};
    use webrtc_audio_processing::{Config, Processor};
    use webrtc_audio_processing_config::EchoCanceller;

    /// The echo canceller.
    pub struct Aec {
        apm: Processor,
        render: [Vec<f32>; 1],
        capture: [Vec<f32>; 1],
    }

    impl Aec {
        /// Creates the APM with only the echo canceller enabled.
        pub fn new() -> Result<Aec, String> {
            let apm = Processor::new(48_000).map_err(|e| format!("{e:?}"))?;
            apm.set_config(Config {
                echo_canceller: Some(EchoCanceller::Full {
                    stream_delay_ms: None,
                }),
                high_pass_filter: None,
                noise_suppression: None,
                gain_controller: None,
                ..Default::default()
            });
            Ok(Aec {
                apm,
                render: [vec![0.0; FRAME]],
                capture: [vec![0.0; FRAME]],
            })
        }

        /// Feeds the reference block, then cleans the capture block in place.
        pub fn process(&mut self, reference: &[f32], capture: &mut [f32]) {
            debug_assert_eq!(reference.len(), FRAME);
            debug_assert_eq!(capture.len(), FRAME);
            self.render[0].copy_from_slice(reference);
            let _ = self.apm.process_render_frame(&mut self.render);
            self.capture[0].copy_from_slice(capture);
            if self.apm.process_capture_frame(&mut self.capture).is_ok() {
                capture.copy_from_slice(&self.capture[0]);
            }
        }

        /// Resets the filter after a reference dropout.
        pub fn reinitialize(&mut self) {
            self.apm.reinitialize();
        }

        /// Current statistics.
        pub fn stats(&self) -> AecStats {
            let s = self.apm.get_stats();
            AecStats {
                erl_db: s.echo_return_loss,
                erle_db: s.echo_return_loss_enhancement,
                delay_ms: s.delay_ms,
                residual_echo_likelihood: s.residual_echo_likelihood,
            }
        }
    }
}

#[cfg(not(feature = "aec"))]
mod real {
    use super::AecStats;

    /// Stub: the feature `aec` is off, every call is a no-op.
    pub struct Aec;

    impl Aec {
        /// Always fails, so the engine reports "not available".
        pub fn new() -> Result<Aec, String> {
            Err("Unsupported: built without feature `aec`".into())
        }
        /// No-op.
        pub fn process(&mut self, _reference: &[f32], _capture: &mut [f32]) {}
        /// No-op.
        pub fn reinitialize(&mut self) {}
        /// Empty.
        pub fn stats(&self) -> AecStats {
            AecStats::default()
        }
    }
}

pub use real::Aec;

#[cfg(all(test, feature = "aec"))]
mod tests {
    //! Offline ERLE test lifted from Spike 6
    //! (`experiments/spike-aec/src/signal.rs`).

    use super::*;
    use crate::dsp::testsignal::*;

    const SECS: usize = 10;
    const N: usize = SR * SECS;
    const DT_START: usize = SR * 4;
    const DT_END: usize = SR * 7;
    const MIC_NOISE_RMS: f32 = 0.00316;

    struct Scene {
        far: Vec<f32>,
        mic: Vec<f32>,
        near: Vec<f32>,
    }

    fn build_scene(delay_ms: f32, reference_is_own_playback_only: bool) -> Scene {
        // Far end = the remote voice. In the "own playback only" variant the
        // loudspeaker also plays game audio the reference does not contain.
        let far = speechlike(N, 0xC0FFEE, 3.3, 0.0, 0.1);
        let game = speechlike(N, 0xBADC0DE, 5.1, 0.4, 0.1);
        let played: Vec<f32> = if reference_is_own_playback_only {
            far.iter().zip(game.iter()).map(|(a, b)| a + b).collect()
        } else {
            far.clone()
        };
        let ir = room_ir(delay_ms, 0.5, 80.0, 0xBEEF);
        let echo = convolve(&played, &ir);
        let echo_rms_dt = rms(&echo[DT_START..DT_END]).max(1e-6);
        let near_full = speechlike(N, 0x5EED_1234, 2.7, 1.1, echo_rms_dt);
        let mut near = vec![0.0f32; N];
        let ramp = SR / 50;
        for i in DT_START..DT_END {
            let w = if i < DT_START + ramp {
                (i - DT_START) as f32 / ramp as f32
            } else if i > DT_END - ramp {
                (DT_END - i) as f32 / ramp as f32
            } else {
                1.0
            };
            near[i] = near_full[i] * w;
        }
        let mut rng = Rng::new(0xA5A5);
        let g = MIC_NOISE_RMS * 3f32.sqrt();
        let mic: Vec<f32> = (0..N)
            .map(|i| echo[i] + near[i] + rng.uniform() * g)
            .collect();
        Scene { far, mic, near }
    }

    fn run(scene: &Scene) -> Vec<f32> {
        let mut aec = Aec::new().expect("apm");
        let mut out = vec![0.0f32; N];
        let mut cap = vec![0.0f32; FRAME];
        for k in 0..N / FRAME {
            let a = k * FRAME;
            cap.copy_from_slice(&scene.mic[a..a + FRAME]);
            aec.process(&scene.far[a..a + FRAME], &mut cap);
            out[a..a + FRAME].copy_from_slice(&cap);
        }
        out
    }

    /// Runs the scene with the reference shifted by `shift` samples:
    /// positive = reference arrives late, negative = early.
    fn run_shifted(scene: &Scene, shift: i64) -> Vec<f32> {
        let reference: Vec<f32> = (0..N as i64)
            .map(|i| {
                let j = i - shift;
                if (0..N as i64).contains(&j) {
                    scene.far[j as usize]
                } else {
                    0.0
                }
            })
            .collect();
        let shifted = Scene {
            far: reference,
            mic: scene.mic.clone(),
            near: scene.near.clone(),
        };
        run(&shifted)
    }

    fn win(x: &[f32], from_s: f64, to_s: f64) -> &[f32] {
        &x[(from_s * SR as f64) as usize..((to_s * SR as f64) as usize).min(x.len())]
    }

    /// (a) steady-state ERLE ≥ 25 dB in single talk. Measured 28.9 dB
    /// (2–4 s) and 27.4 dB (8–10 s) on this scene. The 30-dB bar and the
    /// 37–39 dB of Spike 6 came from a noise generator with a DC offset
    /// (`Rng::uniform`, fixed 2026-09-25): the far end was then largely
    /// sub-20-Hz energy, which the APM's own high-pass filter removed from
    /// the capture and the metric counted as echo cancellation.
    #[test]
    fn steady_state_erle_at_least_25_db() {
        let scene = build_scene(80.0, false);
        let out = run(&scene);
        let erle_early = erle_db(win(&scene.mic, 2.0, 4.0), win(&out, 2.0, 4.0));
        let erle_late = erle_db(win(&scene.mic, 8.0, 10.0), win(&out, 8.0, 10.0));
        eprintln!("single talk ERLE 2–4 s {erle_early:.1} dB, 8–10 s {erle_late:.1} dB");
        assert!(erle_early >= 25.0, "ERLE 2–4 s = {erle_early:.1} dB");
        assert!(erle_late >= 25.0, "ERLE 8–10 s = {erle_late:.1} dB");
    }

    /// (g) What is left of the echo must not open the VAD gate: the
    /// residual after AEC3 goes through RNNoise like the live pipeline, and
    /// its voice probability with only the far end speaking has to stay
    /// under the default threshold on nearly every block once the filter
    /// has converged — also with the mic leveler's full +15 dB held from a
    /// quiet user's own voice on it (`dsp::chain`: the leveler sits between
    /// AEC and RNNoise). The far end is the harmonic test voice here, the
    /// only synthetic signal RNNoise reads as speech (the coloured-noise
    /// `speechlike` scores 0.15 on average); without the AEC its echo
    /// reads as speech on a good third of the blocks — that is the speaker
    /// user's own voice coming back at them through a VAD-mode peer.
    #[test]
    fn residual_echo_does_not_read_as_voice() {
        let far = harmonic_voice(N, 0.1);
        let echo = convolve(&far, &room_ir(80.0, 0.5, 80.0, 0xBEEF));
        let mut rng = Rng::new(0xA5A5);
        let g = MIC_NOISE_RMS * 3f32.sqrt();
        let mic: Vec<f32> = (0..N).map(|i| echo[i] + rng.uniform() * g).collect();
        let scene = Scene {
            far,
            mic,
            near: vec![0.0; N],
        };
        let prob_of = |signal: &[f32], gain: f32| -> Vec<f32> {
            let mut dn = crate::dsp::denoise::Denoiser::new();
            let mut cap = vec![0.0f32; FRAME];
            (0..N / FRAME)
                .map(|k| {
                    for (d, s) in cap.iter_mut().zip(&signal[k * FRAME..(k + 1) * FRAME]) {
                        *d = s * gain;
                    }
                    dn.process(&mut cap)
                })
                .collect()
        };
        let out = run(&scene);
        let cleaned = prob_of(&out, 1.0);
        let boosted = prob_of(&out, crate::dsp::meter::from_db(15.0));
        let raw = prob_of(&scene.mic, 1.0);
        // Blocks after 1 s of convergence where the far end is audible.
        let active: Vec<usize> = (100..N / FRAME)
            .filter(|&k| rms(&scene.far[k * FRAME..(k + 1) * FRAME]) > 0.02)
            .collect();
        let share = |p: &[f32]| {
            active.iter().filter(|&&k| p[k] >= 0.5).count() as f64 / active.len() as f64
        };
        let (with_aec, with_boost, without) = (share(&cleaned), share(&boosted), share(&raw));
        eprintln!(
            "blocks read as voice: {:.1} % with AEC, {:.1} % with AEC and +15 dB, {:.1} % without ({} active blocks)",
            100.0 * with_aec,
            100.0 * with_boost,
            100.0 * without,
            active.len()
        );
        assert!(active.len() > 200);
        assert!(
            without > 0.3,
            "echo alone should read as speech: {without:.2}"
        );
        assert!(
            with_aec < 0.05,
            "residual echo opens the gate: {with_aec:.2}"
        );
        assert!(
            with_boost < 0.05,
            "residual echo behind a +15 dB leveler opens the gate: {with_boost:.2}"
        );
    }

    /// A digitally silent microphone (a virtual source, a muted input)
    /// while the far end starts talking: the canceller's comfort-noise
    /// transient scores up to 0.6 in RNNoise although it lies at −96 dBFS
    /// (idle canceller) or up to −63 dBFS (onset right after init). The VAD
    /// must not take it for voice, or every far-end onset holds the gate
    /// open for the whole hangover (live test 2026-09-26).
    #[test]
    fn silent_mic_transient_does_not_read_as_voice() {
        // The lowest threshold `vad_auto` starts from: floor 0.05 + 0.18.
        const AUTO_START: f32 = 0.23;
        let far = voice(SR * 3, 7, 0.1).samples;
        for idle_blocks in [0usize, 100] {
            let mut aec = Aec::new().unwrap();
            let mut dn = crate::dsp::denoise::Denoiser::new();
            let mut cap = [0.0f32; FRAME];
            for _ in 0..idle_blocks {
                cap.fill(0.0);
                aec.process(&[0.0; FRAME], &mut cap);
                dn.process(&mut cap);
            }
            let (mut trusted, mut voiced) = (0, 0);
            for r in far.chunks(FRAME) {
                cap.fill(0.0);
                aec.process(r, &mut cap);
                let p = dn.process(&mut cap);
                trusted += usize::from(p >= AUTO_START);
                let level = crate::dsp::meter::measure(&cap);
                voiced += usize::from(crate::dsp::vad::is_voice(AUTO_START, Some(p), level));
            }
            eprintln!(
                "idle {idle_blocks} blocks: RNNoise ≥ {AUTO_START} on {trusted}, voice on {voiced}"
            );
            if idle_blocks == 0 {
                assert!(
                    trusted > 0,
                    "the transient no longer fools RNNoise; drop this test?"
                );
            }
            assert_eq!(
                voiced, 0,
                "idle {idle_blocks}: the transient opens the gate"
            );
        }
    }

    /// The self-diagnosis on the statistics the APM really reports over
    /// 30 s: a headset (no echo path) ends in "no echo", a reference that
    /// misses the game audio in "echo not removed", a working canceller in
    /// neither.
    #[test]
    fn diagnosis_matches_the_scenes() {
        let secs = 30;
        let n = SR * secs;
        let far = speechlike(n, 0xC0FFEE, 3.3, 0.0, 0.1);
        let game = speechlike(n, 0xBADC0DE, 5.1, 0.4, 0.1);
        let ir = room_ir(80.0, 0.5, 80.0, 0xBEEF);
        let near = speechlike(n, 0x5EED_1234, 2.7, 1.1, 0.05);
        let noise = |seed: u64| {
            let mut rng = Rng::new(seed);
            let g = MIC_NOISE_RMS * 3f32.sqrt();
            (0..n).map(move |_| rng.uniform() * g)
        };
        let echo_of = |played: &[f32]| convolve(played, &ir);
        let with_game: Vec<f32> = far.iter().zip(&game).map(|(a, b)| a + b).collect();
        let scenes: [(&str, Vec<f32>, Verdict); 3] = [
            (
                "headset",
                near.iter().zip(noise(1)).map(|(a, b)| a + b).collect(),
                Verdict {
                    no_echo: true,
                    ..Default::default()
                },
            ),
            (
                "wrong reference",
                echo_of(&with_game)
                    .iter()
                    .zip(noise(2))
                    .map(|(a, b)| a + b)
                    .collect(),
                Verdict {
                    suspect: true,
                    ..Default::default()
                },
            ),
            (
                "working",
                echo_of(&far)
                    .iter()
                    .zip(noise(3))
                    .map(|(a, b)| a + b)
                    .collect(),
                Verdict::default(),
            ),
        ];
        for (name, mic, expected) in scenes {
            let mut aec = Aec::new().expect("apm");
            let mut diagnosis = Diagnosis::default();
            let mut cap = vec![0.0f32; FRAME];
            let mut ever = Verdict::default();
            let mut last = Verdict::default();
            for k in 0..n / FRAME {
                let a = k * FRAME;
                cap.copy_from_slice(&mic[a..a + FRAME]);
                aec.process(&far[a..a + FRAME], &mut cap);
                if (k + 1) % 200 == 0 {
                    last = diagnosis.report(true, true, &aec.stats());
                    ever.suspect |= last.suspect;
                    ever.no_echo |= last.no_echo;
                    ever.reference_silent |= last.reference_silent;
                }
            }
            eprintln!(
                "{name}: last {last:?}, ever {ever:?}, stats {:?}",
                aec.stats()
            );
            assert_eq!(ever.suspect, expected.suspect, "{name}: suspect");
            assert_eq!(ever.no_echo, expected.no_echo, "{name}: no echo");
            assert!(!ever.reference_silent, "{name}");
        }
    }

    /// (b) ERLE ≥ 22 dB in the near-end pauses during double talk
    /// (measured 25.6 dB; see (a) for why the bar moved).
    #[test]
    fn double_talk_pause_erle_at_least_22_db() {
        let scene = build_scene(80.0, false);
        let out = run(&scene);
        let lag = 430usize;
        let blocks: Vec<f32> = (DT_START..DT_END)
            .step_by(FRAME)
            .map(|a| rms(&scene.near[a..(a + FRAME).min(N)]))
            .collect();
        let peak = blocks.iter().cloned().fold(0.0f32, f32::max);
        let thr = peak * 0.10;
        let (mut p_mic, mut p_out) = (Vec::new(), Vec::new());
        for (bi, a) in (DT_START..DT_END).step_by(FRAME).enumerate() {
            let b = (a + FRAME).min(N);
            if a < DT_START + SR / 5 || b > DT_END - SR / 5 || blocks[bi] > thr {
                continue;
            }
            for i in a..b {
                if i + lag < N {
                    p_mic.push(scene.mic[i]);
                    p_out.push(out[i + lag]);
                }
            }
        }
        let erle = erle_db(&p_mic, &p_out);
        eprintln!("double-talk pause ERLE {erle:.1} dB");
        assert!(erle >= 22.0, "double-talk pause ERLE = {erle:.1} dB");
    }

    /// (c) algorithmic delay = 430 samples ± 1 frame.
    #[test]
    fn algorithmic_delay_is_about_430_samples() {
        let mut aec = Aec::new().expect("apm");
        let silence = vec![0.0f32; FRAME];
        let mut out = Vec::new();
        for k in 0..20 {
            let mut cap = vec![0.0f32; FRAME];
            if k == 5 {
                cap[0] = 0.5;
            }
            aec.process(&silence, &mut cap);
            out.extend_from_slice(&cap);
        }
        let (idx, _) =
            out.iter().enumerate().fold(
                (0, 0.0f32),
                |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m },
            );
        let delay = idx as i64 - (5 * FRAME) as i64;
        assert!(
            (delay - 430).abs() <= FRAME as i64,
            "delay {delay} samples, expected 430 ± 480"
        );
    }

    /// (d) scale test through APM → denoiser.
    #[test]
    fn scale_through_apm_and_denoiser() {
        let mut aec = Aec::new().expect("apm");
        let mut dn = crate::dsp::denoise::Denoiser::new();
        let silence = vec![0.0f32; FRAME];
        let mut max_out = 0.0f32;
        for k in 0..40 {
            let mut cap: Vec<f32> = (0..FRAME)
                .map(|i| {
                    let n = (k * FRAME + i) as f32;
                    (n / SR as f32 * 1000.0 * std::f32::consts::TAU).sin() * 0.5
                })
                .collect();
            aec.process(&silence, &mut cap);
            dn.process(&mut cap);
            if k > 10 {
                max_out = max_out.max(cap.iter().fold(0.0f32, |m, s| m.max(s.abs())));
            }
        }
        assert!((0.2..=0.6).contains(&max_out), "peak {max_out}");
    }

    /// (e) reference = own playback only ⇒ ERLE collapses.
    #[test]
    fn own_playback_reference_only_collapses_erle() {
        let scene = build_scene(80.0, true);
        let out = run(&scene);
        let erle = erle_db(win(&scene.mic, 8.0, 10.0), win(&out, 8.0, 10.0));
        eprintln!("own-playback-only reference ERLE {erle:.1} dB");
        assert!(erle < 20.0, "ERLE with missing game audio = {erle:.1} dB");
    }

    /// (f) A reference that arrives later than its echo cannot be
    /// cancelled; one that arrives early can. This is why `dsp::refalign`
    /// never lets the reference lag the capture.
    #[test]
    fn late_reference_breaks_cancellation_early_does_not() {
        // Short acoustic path, as on a laptop: 5 ms.
        let scene = build_scene(5.0, false);
        let erle = |out: &[f32]| erle_db(win(&scene.mic, 8.0, 10.0), win(out, 8.0, 10.0));
        let aligned = erle(&run_shifted(&scene, 0));
        let early = erle(&run_shifted(&scene, -(SR as i64) / 50));
        let late10 = erle(&run_shifted(&scene, (SR as i64) / 100));
        let late20 = erle(&run_shifted(&scene, (SR as i64) / 50));
        eprintln!(
            "ERLE aligned {aligned:.1} early {early:.1} late10 {late10:.1} late20 {late20:.1}"
        );
        // Measured 26.5 / 26.7 dB (see (a) for why the bar is 24, not 30).
        assert!(aligned >= 24.0, "aligned {aligned:.1} dB");
        assert!(early >= 24.0, "20 ms early {early:.1} dB");
        assert!(late10 < aligned - 10.0, "10 ms late {late10:.1} dB");
        assert!(late20 < aligned - 15.0, "20 ms late {late20:.1} dB");
    }
}
