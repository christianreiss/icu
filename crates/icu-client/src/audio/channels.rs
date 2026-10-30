//! Which channels of an output device carry the mono voice (WASAPI
//! playback, `wasapi.rs`): the front pair — or the center speaker where
//! there is no pair — never LFE or surround. PipeWire maps the mono stream
//! itself.

/// Speaker bits of `WAVEFORMATEXTENSIBLE::dwChannelMask` (`ksmedia.h`).
#[cfg(any(target_os = "windows", feature = "cpal-fallback", test))]
const SPEAKER_FRONT_LEFT: u32 = 0x1;
#[cfg(any(target_os = "windows", feature = "cpal-fallback", test))]
const SPEAKER_FRONT_RIGHT: u32 = 0x2;
#[cfg(any(target_os = "windows", feature = "cpal-fallback", test))]
const SPEAKER_FRONT_CENTER: u32 = 0x4;

/// Capture channel selection without destructive summing of unrelated or
/// opposite-phase microphone inputs. Automatic selection uses an energy
/// margin and 200 ms of consistent evidence before changing an established
/// channel. Changes reset DSP evidence without restarting the native stream.
pub(super) struct CaptureSelector {
    requested: Option<u16>,
    selected: Option<usize>,
    candidate: usize,
    candidate_frames: usize,
    channels: usize,
}
impl CaptureSelector {
    pub(super) fn new(
        requested: Option<u16>,
        channels: usize,
    ) -> Result<Self, super::BackendError> {
        if channels == 0 || channels > 64 || requested.is_some_and(|c| usize::from(c) >= channels) {
            return Err(super::BackendError(format!(
                "Mikrofonkanal nicht verfügbar ({channels} Kanäle)"
            )));
        }
        Ok(Self {
            requested,
            selected: requested.map(usize::from),
            candidate: 0,
            candidate_frames: 0,
            channels,
        })
    }
    pub(super) fn selected(&self) -> Option<u16> {
        self.selected.map(|c| c as u16)
    }
    /// Publish identity before notifying the engine. Keeping this selector
    /// alive avoids repeatedly switching from the initial channel after a
    /// stream reopen. All native backends share the same nonterminal event.
    pub(super) fn update(
        &mut self,
        interleaved: &[f32],
        selected: &std::sync::atomic::AtomicU16,
        events: &std::sync::mpsc::Sender<super::BackendEvent>,
    ) -> bool {
        let changed = self.observe(interleaved);
        selected.store(
            self.selected().unwrap_or(u16::MAX),
            std::sync::atomic::Ordering::Relaxed,
        );
        if changed {
            let _ = events.send(super::BackendEvent::CaptureChannelChanged {
                channel: self.channel() as u16,
            });
        }
        changed
    }
    /// Returns true only when an established selection changes.
    pub(super) fn observe(&mut self, interleaved: &[f32]) -> bool {
        if self.requested.is_some() {
            return false;
        }
        let frames = interleaved.len() / self.channels;
        if frames == 0 {
            return false;
        }
        let mut energy = [0.0f64; 64];
        for frame in interleaved.chunks_exact(self.channels) {
            for (e, sample) in energy.iter_mut().zip(frame) {
                if sample.is_finite() {
                    *e += f64::from(*sample).powi(2);
                }
            }
        }
        let strongest = (0..self.channels)
            .max_by(|&a, &b| energy[a].total_cmp(&energy[b]))
            .unwrap_or(0);
        if energy[strongest] / (frames as f64) < 1e-10 {
            return false;
        }
        let Some(current) = self.selected else {
            self.selected = Some(strongest);
            return false;
        };
        if strongest == current || energy[strongest] < energy[current] * 4.0 {
            self.candidate_frames = 0;
            return false;
        }
        if self.candidate != strongest {
            self.candidate = strongest;
            self.candidate_frames = 0;
        }
        self.candidate_frames += frames;
        if self.candidate_frames >= 9_600 {
            self.selected = Some(strongest);
            self.candidate_frames = 0;
            true
        } else {
            false
        }
    }
    pub(super) fn channel(&self) -> usize {
        self.selected.unwrap_or(0)
    }
}

/// For each of `channels` interleaved channels, whether the voice is
/// written there (the rest stay silent). `mask` is the format's
/// `dwChannelMask` — only a `WAVEFORMATEXTENSIBLE` has one. Channel `n`
/// is the speaker of the `n`-th bit set in it, lowest first; channels past
/// the mask's bits belong to no speaker. Without a usable mask the first
/// two channels count as the front pair, as they do in every Windows
/// layout.
#[cfg(any(target_os = "windows", feature = "cpal-fallback", test))]
pub fn voice_channels(channels: usize, mask: Option<u32>) -> Vec<bool> {
    let speaker = |bit: u32| -> Option<usize> {
        let mask = mask.filter(|m| m & bit != 0)?;
        // Channels are in the order of the mask's bits.
        let index = (mask & (bit - 1)).count_ones() as usize;
        (index < channels).then_some(index)
    };
    let front: Vec<usize> = [SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT]
        .into_iter()
        .filter_map(speaker)
        .collect();
    let chosen = if !front.is_empty() {
        front
    } else if let Some(center) = speaker(SPEAKER_FRONT_CENTER) {
        vec![center]
    } else {
        (0..channels.min(2)).collect()
    };
    (0..channels).map(|c| chosen.contains(&c)).collect()
}

/// Render one endpoint request using preallocated mono scratch, then map it
/// to the audible channels. The callback must see the complete request so
/// the mixer can keep enough audio queued for the device's actual period.
#[cfg(any(target_os = "windows", test))]
pub fn render_voice(
    mono: &mut [f32],
    out: &mut [f32],
    voice: &[bool],
    callback: &mut dyn FnMut(&mut [f32]),
) {
    debug_assert_eq!(out.len(), mono.len() * voice.len());
    mono.fill(0.0);
    callback(mono);
    for (frame, sample) in out.chunks_exact_mut(voice.len()).zip(mono) {
        for (output, enabled) in frame.iter_mut().zip(voice) {
            *output = if *enabled { *sample } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO: u32 = 0x3;
    /// FL FR FC LFE BL BR.
    const SURROUND_5_1: u32 = 0x3f;
    /// FL FR FC LFE BL BR SL SR.
    const SURROUND_7_1: u32 = 0x63f;

    #[test]
    fn microphone_selects_non_silent_input_and_respects_explicit_choice() {
        let stereo: Vec<f32> = (0..480).flat_map(|_| [0.0, 0.2]).collect();
        let mut automatic = CaptureSelector::new(None, 2).unwrap();
        assert!(!automatic.observe(&stereo));
        assert_eq!(automatic.channel(), 1);
        let mut explicit = CaptureSelector::new(Some(0), 2).unwrap();
        explicit.observe(&stereo);
        assert_eq!(explicit.channel(), 0);
        assert!(CaptureSelector::new(Some(2), 2).is_err());
        // Duplicated or antiphase signals are selected, never averaged away.
        let antiphase: Vec<f32> = (0..480).flat_map(|_| [0.2, -0.2]).collect();
        automatic.observe(&antiphase);
        assert_eq!(automatic.channel(), 1);
    }

    #[test]
    fn an_impulse_cannot_move_an_established_microphone_channel() {
        let first: Vec<f32> = (0..480).flat_map(|_| [0.1, 0.0]).collect();
        let second: Vec<f32> = (0..480).flat_map(|_| [0.0, 0.2]).collect();
        let mut selector = CaptureSelector::new(None, 2).unwrap();
        selector.observe(&first);
        assert!(!selector.observe(&second));
        assert_eq!(selector.channel(), 0);
        selector.observe(&first);
        for _ in 0..19 {
            assert!(!selector.observe(&second));
        }
        assert!(selector.observe(&second));
        assert_eq!(selector.channel(), 1);
    }

    #[test]
    fn switching_channels_notifies_once_without_restarting_the_selector() {
        use std::sync::{
            atomic::{AtomicU16, Ordering},
            mpsc,
        };
        let (events, receiver) = mpsc::channel();
        let selected = AtomicU16::new(u16::MAX);
        let first: Vec<f32> = (0..480).flat_map(|_| [0.1, 0.0]).collect();
        let second: Vec<f32> = (0..480).flat_map(|_| [0.0, 0.2]).collect();
        let mut selector = CaptureSelector::new(None, 2).unwrap();
        assert!(!selector.update(&first, &selected, &events));
        for _ in 0..19 {
            assert!(!selector.update(&second, &selected, &events));
        }
        assert!(selector.update(&second, &selected, &events));
        assert_eq!(selected.load(Ordering::Relaxed), 1);
        assert!(matches!(
            receiver.try_recv().unwrap(),
            super::super::BackendEvent::CaptureChannelChanged { channel: 1 }
        ));
        // Continue using the established native stream after the engine
        // resets its DSP epoch. A reopen would throw away this selection.
        for _ in 0..200 {
            assert!(!selector.update(&second, &selected, &events));
            assert_eq!(selector.channel(), 1);
        }
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn large_endpoint_request_reaches_mixer_whole_and_preserves_channel_routing() {
        for frames in [240, 480, 1440, 1920] {
            let mut mono = vec![9.0; frames];
            let mut out = vec![9.0; frames * 6];
            let voice = voice_channels(6, Some(SURROUND_5_1));
            let mut calls = vec![];
            render_voice(&mut mono, &mut out, &voice, &mut |samples| {
                calls.push(samples.len());
                assert!(samples.iter().all(|sample| *sample == 0.0));
                samples.fill(0.25);
            });
            assert_eq!(
                calls,
                [frames],
                "device demand must not be hidden by chunking"
            );
            for frame in out.as_chunks::<6>().0 {
                assert_eq!(*frame, [0.25, 0.25, 0.0, 0.0, 0.0, 0.0]);
            }
        }
    }

    #[test]
    fn the_voice_goes_to_the_front_pair_only() {
        assert_eq!(voice_channels(1, None), [true]);
        assert_eq!(voice_channels(2, None), [true, true]);
        assert_eq!(voice_channels(2, Some(STEREO)), [true, true]);
        assert_eq!(
            voice_channels(6, Some(SURROUND_5_1)),
            [true, true, false, false, false, false],
            "not into the subwoofer or the rear"
        );
        assert_eq!(
            voice_channels(8, Some(SURROUND_7_1)),
            [true, true, false, false, false, false, false, false]
        );
    }

    #[test]
    fn without_a_usable_mask_the_first_two_channels_are_the_front() {
        assert_eq!(
            voice_channels(6, None),
            [true, true, false, false, false, false]
        );
        assert_eq!(voice_channels(4, Some(0)), [true, true, false, false]);
        // Neither front nor center in the mask (BL BR only).
        assert_eq!(voice_channels(2, Some(0x30)), [true, true]);
    }

    #[test]
    fn a_center_only_layout_uses_the_center() {
        // FC alone, and FC + LFE: the voice once, in the center.
        assert_eq!(voice_channels(1, Some(SPEAKER_FRONT_CENTER)), [true]);
        assert_eq!(voice_channels(2, Some(0x4 | 0x8)), [true, false]);
    }

    #[test]
    fn channels_follow_the_order_of_the_mask_bits() {
        // FR alone (bit 1 is the only one set): channel 0 is the right
        // speaker; the second channel belongs to no speaker.
        assert_eq!(voice_channels(2, Some(SPEAKER_FRONT_RIGHT)), [true, false]);
        // A mask describing more speakers than there are channels.
        assert_eq!(voice_channels(1, Some(SURROUND_5_1)), [true]);
    }
}
