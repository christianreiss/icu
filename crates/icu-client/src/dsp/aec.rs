//! AEC3 through `webrtc-audio-processing` (`Research/aec.md`).
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
    /// Estimated delay in ms.
    pub delay_ms: Option<u32>,
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

        /// Tells the APM the output is muted (mute/deafen).
        pub fn set_output_muted(&mut self, muted: bool) {
            self.apm.set_output_will_be_muted(muted);
        }

        /// Current statistics.
        pub fn stats(&self) -> AecStats {
            let s = self.apm.get_stats();
            AecStats {
                erl_db: s.echo_return_loss,
                erle_db: s.echo_return_loss_enhancement,
                delay_ms: s.delay_ms,
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
        /// No-op.
        pub fn set_output_muted(&mut self, _muted: bool) {}
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
    //! (`Research/spikes/spike-aec/src/signal.rs`).

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

    fn win(x: &[f32], from_s: f64, to_s: f64) -> &[f32] {
        &x[(from_s * SR as f64) as usize..((to_s * SR as f64) as usize).min(x.len())]
    }

    /// (a) steady-state ERLE ≥ 30 dB in single talk.
    #[test]
    fn steady_state_erle_at_least_30_db() {
        let scene = build_scene(80.0, false);
        let out = run(&scene);
        let erle_early = erle_db(win(&scene.mic, 2.0, 4.0), win(&out, 2.0, 4.0));
        let erle_late = erle_db(win(&scene.mic, 8.0, 10.0), win(&out, 8.0, 10.0));
        assert!(erle_early >= 30.0, "ERLE 2–4 s = {erle_early:.1} dB");
        assert!(erle_late >= 30.0, "ERLE 8–10 s = {erle_late:.1} dB");
    }

    /// (b) ERLE ≥ 25 dB in the near-end pauses during double talk.
    #[test]
    fn double_talk_pause_erle_at_least_25_db() {
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
        assert!(erle >= 25.0, "double-talk pause ERLE = {erle:.1} dB");
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
        assert!(erle < 20.0, "ERLE with missing game audio = {erle:.1} dB");
    }
}
