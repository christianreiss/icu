//! Which channels of an output device carry the mono voice (WASAPI
//! playback, `wasapi.rs`): the front pair — or the center speaker where
//! there is no pair — never LFE or surround. PipeWire maps the mono stream
//! itself.

/// Speaker bits of `WAVEFORMATEXTENSIBLE::dwChannelMask` (`ksmedia.h`).
const SPEAKER_FRONT_LEFT: u32 = 0x1;
const SPEAKER_FRONT_RIGHT: u32 = 0x2;
const SPEAKER_FRONT_CENTER: u32 = 0x4;

/// For each of `channels` interleaved channels, whether the voice is
/// written there (the rest stay silent). `mask` is the format's
/// `dwChannelMask` — only a `WAVEFORMATEXTENSIBLE` has one. Channel `n`
/// is the speaker of the `n`-th bit set in it, lowest first; channels past
/// the mask's bits belong to no speaker. Without a usable mask the first
/// two channels count as the front pair, as they do in every Windows
/// layout.
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

#[cfg(test)]
mod tests {
    use super::*;

    const STEREO: u32 = 0x3;
    /// FL FR FC LFE BL BR.
    const SURROUND_5_1: u32 = 0x3f;
    /// FL FR FC LFE BL BR SL SR.
    const SURROUND_7_1: u32 = 0x63f;

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
