//! What a received access unit may ask of the decoder, read from its SPS
//! before the decoder sees it (`PROTOCOL.md` §4). The encoder of a modified
//! client is not bound to the 320×240 profile; openh264 itself accepts up
//! to level 5.2 (about 9.4 Mpx), and a frame that size costs every viewer
//! tens of MB and a stalled decode thread. Only the fields up to
//! `frame_mbs_only_flag` are read — enough for the picture size and the
//! reference count.

use icu_proto::header::{VIDEO_MAX_HEIGHT, VIDEO_MAX_REF_FRAMES, VIDEO_MAX_WIDTH};

/// Why an access unit is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reject {
    /// An SVC/MVC NAL (14, 15, 20): a subset SPS carries its own size.
    Extension(u8),
    /// Not Baseline or Main: the protocol sends Constrained Baseline, and
    /// the High profiles put scaling lists before the size.
    Profile(u8),
    /// The SPS ends before the size fields.
    Truncated,
    /// A picture larger than [`VIDEO_MAX_WIDTH`] × [`VIDEO_MAX_HEIGHT`].
    TooLarge { width: u64, height: u64 },
    /// More reference frames than [`VIDEO_MAX_REF_FRAMES`].
    TooManyRefs(u64),
}

/// `Ok` when every SPS in the Annex-B access unit `data` stays within the
/// receiver ceiling and no NAL needs an extension decoder.
pub fn check(data: &[u8]) -> Result<(), Reject> {
    for nal in nal_units(data) {
        let Some(&first) = nal.first() else { continue };
        match first & 0x1f {
            t @ (14 | 15 | 20) => return Err(Reject::Extension(t)),
            7 => check_sps(&unescape(&nal[1..]))?,
            _ => {}
        }
    }
    Ok(())
}

/// The NAL units between Annex-B start codes (`00 00 01`, also as the tail
/// of `00 00 00 01`). Trailing zero bytes belong to the next start code and
/// are left on the unit — harmless for the header byte and the SPS fields.
fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|&s| s - 3)
        .chain(std::iter::once(data.len()))
        .collect();
    starts
        .into_iter()
        .zip(ends)
        .map(move |(s, e)| &data[s..e.max(s)])
}

/// The RBSP of a NAL payload: every `00 00 03` loses its `03`.
fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn check_sps(rbsp: &[u8]) -> Result<(), Reject> {
    let mut r = Bits { data: rbsp, pos: 0 };
    let profile = r.bits(8)? as u8;
    if !matches!(profile, 66 | 77) {
        return Err(Reject::Profile(profile));
    }
    r.bits(16)?; // constraint flags, level_idc
    r.ue()?; // seq_parameter_set_id
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
        }
        1 => {
            r.bits(1)?; // delta_pic_order_always_zero_flag
            r.ue()?; // offset_for_non_ref_pic (se)
            r.ue()?; // offset_for_top_to_bottom_field (se)
            let cycle = r.ue()?;
            if cycle > 255 {
                return Err(Reject::Truncated);
            }
            for _ in 0..cycle {
                r.ue()?; // offset_for_ref_frame (se)
            }
        }
        _ => {}
    }
    let refs = r.ue()?;
    r.bits(1)?; // gaps_in_frame_num_value_allowed_flag
    let width = (r.ue()? + 1) * 16;
    let map_units = r.ue()? + 1;
    let frame_mbs_only = r.bits(1)?;
    let height = map_units * (2 - frame_mbs_only) * 16;
    if width > VIDEO_MAX_WIDTH as u64 || height > VIDEO_MAX_HEIGHT as u64 {
        return Err(Reject::TooLarge { width, height });
    }
    if refs > u64::from(VIDEO_MAX_REF_FRAMES) {
        return Err(Reject::TooManyRefs(refs));
    }
    Ok(())
}

/// A big-endian bit reader over an RBSP.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u64, Reject> {
        let byte = self.data.get(self.pos / 8).ok_or(Reject::Truncated)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Ok(u64::from(bit))
    }

    fn bits(&mut self, n: u32) -> Result<u64, Reject> {
        (0..n).try_fold(0, |acc, _| Ok((acc << 1) | self.bit()?))
    }

    /// Exp-Golomb `ue(v)`; an `se(v)` has the same length, its value is
    /// never needed here. More than 31 leading zeros is not a valid code.
    fn ue(&mut self) -> Result<u64, Reject> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return Err(Reject::Truncated);
            }
        }
        Ok((1 << zeros) - 1 + self.bits(zeros)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes an SPS RBSP bit by bit.
    #[derive(Default)]
    struct Writer {
        bits: Vec<bool>,
    }

    impl Writer {
        fn bits(&mut self, v: u64, n: u32) -> &mut Self {
            for i in (0..n).rev() {
                self.bits.push((v >> i) & 1 == 1);
            }
            self
        }
        fn ue(&mut self, v: u64) -> &mut Self {
            let len = 64 - (v + 1).leading_zeros();
            self.bits(0, len - 1).bits(v + 1, len)
        }
        fn bytes(&self) -> Vec<u8> {
            let mut out: Vec<u8> = self
                .bits
                .chunks(8)
                .map(|c| c.iter().enumerate().fold(0, |a, (i, &b)| a | (u8::from(b) << (7 - i))))
                .collect();
            out.push(0x80); // rbsp stop bit
            out
        }
    }

    /// An access unit of one SPS for a `mbs_w` × `mbs_h` frame picture
    /// with `refs` reference frames, escaped the way an encoder does.
    fn sps(profile: u64, mbs_w: u64, mbs_h: u64, refs: u64) -> Vec<u8> {
        let rbsp = Writer::default()
            .bits(profile, 8)
            .bits(0, 16)
            .ue(0)
            .ue(0)
            .ue(2) // poc type 2
            .ue(refs)
            .bits(0, 1)
            .ue(mbs_w - 1)
            .ue(mbs_h - 1)
            .bits(1, 1)
            .bytes();
        let mut au = vec![0, 0, 0, 1, 0x67];
        let mut zeros = 0;
        for b in rbsp {
            if zeros >= 2 && b <= 3 {
                au.push(3);
                zeros = 0;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            au.push(b);
        }
        au.extend([0, 0, 1, 0x65, 0x88]);
        au
    }

    #[test]
    fn the_profile_size_passes() {
        assert_eq!(check(&sps(66, 20, 15, 1)), Ok(()));
        assert_eq!(check(&sps(66, 40, 30, 4)), Ok(()), "the ceiling itself");
    }

    #[test]
    fn a_larger_picture_is_refused() {
        assert_eq!(
            check(&sps(66, 80, 45, 1)),
            Err(Reject::TooLarge {
                width: 1280,
                height: 720
            })
        );
        assert!(check(&sps(66, 41, 15, 1)).is_err(), "one MB too wide");
        assert!(check(&sps(66, 20, 31, 1)).is_err(), "one MB too high");
    }

    #[test]
    fn too_many_references_are_refused() {
        assert_eq!(check(&sps(66, 20, 15, 5)), Err(Reject::TooManyRefs(5)));
    }

    #[test]
    fn high_profiles_and_extensions_are_refused() {
        assert_eq!(check(&sps(100, 20, 15, 1)), Err(Reject::Profile(100)));
        assert_eq!(
            check(&[0, 0, 0, 1, 0x6f, 0x42]),
            Err(Reject::Extension(15))
        );
        assert_eq!(check(&[0, 0, 1, 0x74, 0]), Err(Reject::Extension(20)));
    }

    #[test]
    fn a_truncated_sps_is_refused() {
        let au = sps(66, 20, 15, 1);
        let cut = &au[..7];
        assert_eq!(check(cut), Err(Reject::Truncated));
    }

    #[test]
    fn emulation_prevention_is_removed() {
        assert_eq!(unescape(&[1, 0, 0, 3, 0, 0, 0, 3, 1]), vec![1, 0, 0, 0, 0, 0, 1]);
        // Zero constraint flags and level put `00 00` after the profile.
        assert_eq!(check(&sps(66, 1, 1, 0)), Ok(()));
    }

    #[test]
    fn slices_without_an_sps_pass() {
        assert_eq!(check(&[0, 0, 0, 1, 0x65]), Ok(()));
        assert_eq!(check(&[]), Ok(()));
    }
}
