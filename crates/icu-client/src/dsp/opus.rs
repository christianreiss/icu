//! Opus encoder/decoder wrappers with the v1 profile (`docs/reference/protocol.md` §3,
//! `docs/reference/client.md` §6), directly over `opusic-sys`: the `opus`
//! crate has no decoder complexity control, and Deep PLC needs one.

use std::ffi::{c_int, CStr};
use std::fmt;

use icu_proto::limits::LOSS_HINT_MAX_PERCENT;
use opusic_sys as ffi;

/// Sample rate.
pub const RATE: u32 = crate::audio::SAMPLE_RATE;
/// Largest packet the protocol allows.
pub const MAX_PACKET: usize = icu_proto::header::AUDIO_MAX_PAYLOAD;
/// `PACKET_LOSS_PERC` never goes below this: libopus codes no in-band FEC
/// at 0 % (`decide_fec`), so without a floor the first loss burst before a
/// `LossReport` round trip would always fall back to concealment.
pub const FEC_FLOOR_PERCENT: i32 = 3;
/// Decoder complexity: ≥ 5 turns on Deep PLC (neural concealment). 6/7
/// (LACE/NoLACE) only enhance low-rate SILK and are not built.
pub const DECODER_COMPLEXITY: i32 = 5;
const _: () = assert!(DECODER_COMPLEXITY >= 5, "below 5 there is no Deep PLC");

/// A libopus error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SAFETY: opus_strerror returns a static NUL-terminated string.
        let s = unsafe { CStr::from_ptr(ffi::opus_strerror(self.0)) };
        write!(f, "opus: {}", s.to_string_lossy())
    }
}

impl std::error::Error for Error {}

fn check(ret: c_int) -> Result<c_int, Error> {
    if ret < 0 {
        Err(Error(ret))
    } else {
        Ok(ret)
    }
}

/// Samples in `packet` at 48 kHz.
fn packet_samples(packet: &[u8]) -> Result<usize, Error> {
    // SAFETY: pointer and length describe `packet`.
    check(unsafe {
        ffi::opus_packet_get_nb_samples(packet.as_ptr(), packet.len() as i32, RATE as i32)
    })
    .map(|n| n as usize)
}

/// Owned `OpusEncoder`.
struct RawEncoder(*mut ffi::OpusEncoder);

// SAFETY: the state is only touched through `&mut self`.
unsafe impl Send for RawEncoder {}

impl Drop for RawEncoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_encoder_create, destroyed once.
        unsafe { ffi::opus_encoder_destroy(self.0) }
    }
}

impl RawEncoder {
    fn ctl(&mut self, request: c_int, value: i32) -> Result<(), Error> {
        // SAFETY: every request used here takes one opus_int32.
        check(unsafe { ffi::opus_encoder_ctl(self.0, request, value) }).map(|_| ())
    }
}

/// Owned `OpusDecoder`.
struct RawDecoder(*mut ffi::OpusDecoder);

// SAFETY: the state is only touched through `&mut self`.
unsafe impl Send for RawDecoder {}

impl Drop for RawDecoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_decoder_create, destroyed once.
        unsafe { ffi::opus_decoder_destroy(self.0) }
    }
}

impl RawDecoder {
    /// Decodes `data` (empty: conceal) into `out`; `out.len()` is the frame size.
    fn decode_float(&mut self, data: &[u8], out: &mut [f32], fec: bool) -> Result<usize, Error> {
        let ptr = if data.is_empty() {
            std::ptr::null()
        } else {
            data.as_ptr()
        };
        // SAFETY: pointers and lengths describe the slices; mono, so the
        // frame size is the output length.
        check(unsafe {
            ffi::opus_decode_float(
                self.0,
                ptr,
                data.len() as i32,
                out.as_mut_ptr(),
                out.len() as c_int,
                fec as c_int,
            )
        })
        .map(|n| n as usize)
    }
}

/// Voice encoder.
pub struct Encoder {
    inner: RawEncoder,
    buf: Vec<u8>,
    loss_perc: i32,
}

impl Encoder {
    /// VOIP, mono, 48 kbit/s constrained VBR, FEC on (floor
    /// [`FEC_FLOOR_PERCENT`]), DTX off, complexity 10. Stays below 64 kbit/s
    /// on purpose: mono voice goes CELT-only there, without FEC or Deep PLC.
    pub fn new() -> Result<Encoder, Error> {
        let mut err = 0;
        // SAFETY: plain constructor; `err` receives the status.
        let ptr = unsafe {
            ffi::opus_encoder_create(RATE as i32, 1, ffi::OPUS_APPLICATION_VOIP, &mut err)
        };
        check(err)?;
        if ptr.is_null() {
            return Err(Error(ffi::OPUS_ALLOC_FAIL));
        }
        let mut e = RawEncoder(ptr);
        e.ctl(ffi::OPUS_SET_SIGNAL_REQUEST, ffi::OPUS_SIGNAL_VOICE)?;
        e.ctl(ffi::OPUS_SET_BITRATE_REQUEST, 48_000)?;
        e.ctl(ffi::OPUS_SET_VBR_REQUEST, 1)?;
        e.ctl(ffi::OPUS_SET_VBR_CONSTRAINT_REQUEST, 1)?;
        e.ctl(ffi::OPUS_SET_INBAND_FEC_REQUEST, 1)?;
        e.ctl(ffi::OPUS_SET_PACKET_LOSS_PERC_REQUEST, FEC_FLOOR_PERCENT)?;
        e.ctl(ffi::OPUS_SET_DTX_REQUEST, 0)?;
        e.ctl(ffi::OPUS_SET_COMPLEXITY_REQUEST, 10)?;
        Ok(Encoder {
            inner: e,
            buf: vec![0u8; MAX_PACKET],
            loss_perc: FEC_FLOOR_PERCENT,
        })
    }

    /// Encodes one frame (480 or 960 samples, ±1.0) and returns the packet.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8], Error> {
        // SAFETY: pointers and lengths describe `pcm` and `buf`; mono.
        let n = check(unsafe {
            ffi::opus_encode_float(
                self.inner.0,
                pcm.as_ptr(),
                pcm.len() as c_int,
                self.buf.as_mut_ptr(),
                self.buf.len() as i32,
            )
        })?;
        Ok(&self.buf[..n as usize])
    }

    /// Applies a `LossHint`: per mille → whole percent, at least
    /// [`FEC_FLOOR_PERCENT`], capped at 20.
    pub fn set_loss_permille(&mut self, permille: u16) -> Result<(), Error> {
        let perc = ((permille as u32).div_ceil(10) as i32)
            .clamp(FEC_FLOOR_PERCENT, LOSS_HINT_MAX_PERCENT as i32);
        if perc != self.loss_perc {
            self.inner
                .ctl(ffi::OPUS_SET_PACKET_LOSS_PERC_REQUEST, perc)?;
            self.loss_perc = perc;
        }
        Ok(())
    }

    /// Current `PACKET_LOSS_PERC`.
    pub fn loss_perc(&self) -> i32 {
        self.loss_perc
    }
}

/// Per speaker decoder with PLC and FEC.
pub struct Decoder {
    inner: RawDecoder,
    last_frame: usize,
}

impl Decoder {
    /// Mono 48 kHz decoder at [`DECODER_COMPLEXITY`].
    pub fn new() -> Result<Decoder, Error> {
        Self::with_complexity(DECODER_COMPLEXITY)
    }

    /// Mono 48 kHz decoder at `complexity` (0 = classic PLC).
    pub fn with_complexity(complexity: i32) -> Result<Decoder, Error> {
        let mut err = 0;
        // SAFETY: plain constructor; `err` receives the status.
        let ptr = unsafe { ffi::opus_decoder_create(RATE as i32, 1, &mut err) };
        check(err)?;
        if ptr.is_null() {
            return Err(Error(ffi::OPUS_ALLOC_FAIL));
        }
        let inner = RawDecoder(ptr);
        // SAFETY: SET_COMPLEXITY takes one opus_int32.
        check(unsafe {
            ffi::opus_decoder_ctl(inner.0, ffi::OPUS_SET_COMPLEXITY_REQUEST, complexity)
        })?;
        Ok(Decoder {
            inner,
            last_frame: 960,
        })
    }

    /// The decoder's `OPUS_GET_COMPLEXITY`.
    pub fn complexity(&mut self) -> Result<i32, Error> {
        let mut v: i32 = 0;
        // SAFETY: GET_COMPLEXITY writes one opus_int32.
        check(unsafe {
            ffi::opus_decoder_ctl(
                self.inner.0,
                ffi::OPUS_GET_COMPLEXITY_REQUEST,
                &mut v as *mut i32,
            )
        })?;
        Ok(v)
    }

    /// Decodes a packet into `out`, returning the number of samples.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, Error> {
        let n = self.inner.decode_float(packet, out, false)?;
        self.last_frame = n;
        Ok(n)
    }

    /// Recovers the lost frame `n` from packet `n+1`'s in-band FEC.
    pub fn decode_fec(&mut self, next_packet: &[u8], out: &mut [f32]) -> Result<usize, Error> {
        // The frame length comes from the peer's packet (up to 120 ms);
        // clamped so the slice stays in bounds and libopus decides instead.
        let frame = packet_samples(next_packet)
            .unwrap_or(self.last_frame)
            .min(out.len());
        self.inner
            .decode_float(next_packet, &mut out[..frame], true)
    }

    /// Packet loss concealment for one frame.
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<usize, Error> {
        let frame = self.last_frame.min(out.len());
        self.inner.decode_float(&[], &mut out[..frame], false)
    }

    /// Duration in samples of `packet`.
    pub fn nb_samples(&self, packet: &[u8]) -> usize {
        packet_samples(packet).unwrap_or(self.last_frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testsignal::{harmonic_voice, pearson, rms, speechlike};

    /// A peer's 60 ms packet must not panic FEC recovery into a buffer
    /// sized for 20 ms.
    #[test]
    fn fec_with_oversized_next_packet_does_not_panic() {
        let mut enc = Encoder::new().unwrap();
        let pkt = enc.encode(&vec![0.0f32; 2880]).unwrap().to_vec();
        let mut dec = Decoder::new().unwrap();
        let mut out = vec![0.0f32; 1920];
        if let Ok(n) = dec.decode_fec(&pkt, &mut out) {
            assert!(n <= out.len());
        }
    }

    #[test]
    fn encode_decode_roundtrip_correlates() {
        let mut enc = Encoder::new().unwrap();
        let mut dec = Decoder::new().unwrap();
        let sig = harmonic_voice(48_000, 0.1);
        let mut out = Vec::new();
        let mut pcm = vec![0.0f32; 960];
        for frame in sig.as_chunks::<960>().0 {
            let pkt = enc.encode(frame).unwrap().to_vec();
            assert!(!pkt.is_empty() && pkt.len() <= MAX_PACKET);
            let n = dec.decode(&pkt, &mut pcm).unwrap();
            assert_eq!(n, 960);
            out.extend_from_slice(&pcm[..n]);
        }
        // Opus has ~6.5 ms lookahead; correlate with a small lag search.
        let lag = crate::dsp::testsignal::best_lag(&sig[9600..38400], &out[9600..], 600, 1);
        let c = pearson(&sig[9600..38400], &out[9600 + lag..]);
        assert!(c > 0.8, "correlation {c} at lag {lag}");
    }

    #[test]
    fn loss_hint_maps_to_percent_capped_at_20() {
        let mut enc = Encoder::new().unwrap();
        enc.set_loss_permille(123).unwrap();
        assert_eq!(enc.loss_perc(), 13);
        enc.set_loss_permille(900).unwrap();
        assert_eq!(enc.loss_perc(), 20);
        enc.set_loss_permille(0).unwrap();
        assert_eq!(enc.loss_perc(), FEC_FLOOR_PERCENT);
        enc.set_loss_permille(5).unwrap();
        assert_eq!(enc.loss_perc(), FEC_FLOOR_PERCENT);
    }

    /// Any loss percentage forces SILK-only for voice (`opus_encoder.c`,
    /// `useInBandFEC && packetLossPercentage > …`); above wideband that is
    /// promoted back to hybrid. The floor must not cost fullband.
    #[test]
    fn fec_floor_stays_fullband_hybrid() {
        let voice = crate::dsp::testsignal::voice(48_000 * 2, 5, 0.1).samples;
        let mut enc = Encoder::new().unwrap();
        let mut voiced = 0;
        for frame in voice.as_chunks::<960>().0 {
            let pkt = enc.encode(frame).unwrap();
            if pkt.len() < 20 {
                continue;
            }
            // SAFETY: pointer to a live packet's TOC byte.
            let bw = unsafe { ffi::opus_packet_get_bandwidth(pkt.as_ptr()) };
            let config = pkt[0] >> 3;
            assert_eq!(bw, ffi::OPUS_BANDWIDTH_FULLBAND, "TOC config {config}");
            assert!(
                (14..=15).contains(&config),
                "not hybrid FB: config {config}"
            );
            voiced += 1;
        }
        assert!(voiced > 50);
    }

    #[test]
    fn new_encoder_starts_at_fec_floor() {
        assert_eq!(Encoder::new().unwrap().loss_perc(), FEC_FLOOR_PERCENT);
    }

    #[test]
    fn decoder_runs_at_deep_plc_complexity() {
        let mut dec = Decoder::new().unwrap();
        assert_eq!(dec.complexity().unwrap(), DECODER_COMPLEXITY);
    }

    /// The 10 ms option (`frame_ms = 10`, `docs/reference/protocol.md` §3): 480-sample
    /// frames encode, carry their duration in the TOC, and both FEC and PLC
    /// recover a 10 ms frame.
    #[test]
    fn ten_ms_frames_with_fec_and_plc() {
        let mut enc = Encoder::new().unwrap();
        enc.set_loss_permille(100).unwrap();
        let mut dec = Decoder::new().unwrap();
        let sig = speechlike(48_000, 9, 3.0, 0.0, 0.1);
        let packets: Vec<Vec<u8>> = sig
            .as_chunks::<480>()
            .0
            .iter()
            .map(|f| enc.encode(f).unwrap().to_vec())
            .collect();
        assert!(packets
            .iter()
            .all(|p| !p.is_empty() && p.len() <= MAX_PACKET));
        let mut out = vec![0.0f32; 960];
        for p in &packets[..20] {
            assert_eq!(dec.nb_samples(p), 480);
            assert_eq!(dec.decode(p, &mut out).unwrap(), 480);
        }
        // Packet 20 lost: FEC from 21, then PLC for a lost 22.
        assert_eq!(dec.decode_fec(&packets[21], &mut out).unwrap(), 480);
        assert_eq!(dec.decode(&packets[21], &mut out).unwrap(), 480);
        assert_eq!(dec.conceal(&mut out).unwrap(), 480);
        assert_eq!(dec.decode(&packets[23], &mut out).unwrap(), 480);
    }

    /// A `LossHint` makes the encoder spend more bits on in-band FEC
    /// (LBRR) than the floor does, and a lost frame recovered from its
    /// successor is much closer to the original than what concealment can
    /// extrapolate.
    #[test]
    fn loss_hint_buys_fec_that_beats_plc() {
        let voice = crate::dsp::testsignal::voice(48_000 * 4, 5, 0.1).samples;
        let frames = voice.as_chunks::<960>().0;
        let size_at = |permille: u16| -> (f64, Vec<Vec<u8>>) {
            let mut enc = Encoder::new().unwrap();
            enc.set_loss_permille(permille).unwrap();
            let packets: Vec<Vec<u8>> = frames
                .iter()
                .map(|f| enc.encode(f).unwrap().to_vec())
                .collect();
            let mean = packets.iter().map(|p| p.len()).sum::<usize>() as f64 / packets.len() as f64;
            (mean, packets)
        };
        let (plain, _) = size_at(0);
        let (with_fec, packets) = size_at(200);
        eprintln!(
            "mean packet {plain:.0} B at the FEC floor, {with_fec:.0} B with a 20 % loss hint"
        );
        assert!(
            with_fec >= plain,
            "loss hint shrank packets: {plain:.0} → {with_fec:.0} B"
        );
        // Three decoders in the same state up to frame k−1: the truth, the
        // FEC recovery of frame k, and its concealment. After a lost frame
        // all three consume k+1 to get back in step, so the loop skips it.
        let mut truth = Decoder::new().unwrap();
        let mut fec = Decoder::new().unwrap();
        let mut plc = Decoder::new().unwrap();
        let mut sum = (0.0f64, 0.0f64);
        let mut worst_fec = 1.0f64;
        let mut n = 0;
        let mut out = vec![0.0f32; 960];
        let mut k = 0;
        while k + 1 < packets.len() {
            let mut want = vec![0.0f32; 960];
            truth.decode(&packets[k], &mut want).unwrap();
            // Only voiced frames after some history say anything.
            if k < 5 || rms(&want) < 0.05 {
                fec.decode(&packets[k], &mut out).unwrap();
                plc.decode(&packets[k], &mut out).unwrap();
                k += 1;
                continue;
            }
            let mut got_fec = vec![0.0f32; 960];
            fec.decode_fec(&packets[k + 1], &mut got_fec).unwrap();
            let mut got_plc = vec![0.0f32; 960];
            plc.conceal(&mut got_plc).unwrap();
            let c_fec = pearson(&want, &got_fec);
            let c_plc = pearson(&want, &got_plc);
            assert!(
                c_fec.is_finite() && c_plc.is_finite(),
                "frame {k}: silent output"
            );
            sum.0 += c_fec;
            sum.1 += c_plc;
            worst_fec = worst_fec.min(c_fec);
            n += 1;
            fec.decode(&packets[k + 1], &mut out).unwrap();
            plc.decode(&packets[k + 1], &mut out).unwrap();
            truth.decode(&packets[k + 1], &mut out).unwrap();
            k += 2;
        }
        let (mean_fec, mean_plc) = (sum.0 / n as f64, sum.1 / n as f64);
        eprintln!(
            "{n} voiced frames: correlation with the original {mean_fec:.2} via FEC (worst {worst_fec:.2}), {mean_plc:.2} via PLC"
        );
        assert!(n > 50);
        assert!(mean_fec > 0.7, "FEC recovery correlates {mean_fec:.2}");
        assert!(
            mean_fec > mean_plc + 0.15,
            "FEC ({mean_fec:.2}) hardly beats PLC ({mean_plc:.2})"
        );
    }

    /// Voiced-frame correlation of FEC recovery vs. concealment for single
    /// lost frames, with the encoder left at its initial loss setting.
    fn fec_vs_plc_at_default_hint() -> (f64, f64, usize) {
        let voice = crate::dsp::testsignal::voice(48_000 * 4, 5, 0.1).samples;
        let mut enc = Encoder::new().unwrap();
        let packets: Vec<Vec<u8>> = voice
            .as_chunks::<960>()
            .0
            .iter()
            .map(|f| enc.encode(f).unwrap().to_vec())
            .collect();
        let (mut truth, mut fec, mut plc) = (
            Decoder::new().unwrap(),
            Decoder::new().unwrap(),
            Decoder::new().unwrap(),
        );
        let (mut sum_fec, mut sum_plc, mut n) = (0.0, 0.0, 0);
        let mut out = vec![0.0f32; 960];
        let mut k = 0;
        while k + 1 < packets.len() {
            let mut want = vec![0.0f32; 960];
            truth.decode(&packets[k], &mut want).unwrap();
            if k < 5 || rms(&want) < 0.05 {
                fec.decode(&packets[k], &mut out).unwrap();
                plc.decode(&packets[k], &mut out).unwrap();
                k += 1;
                continue;
            }
            let mut got = vec![0.0f32; 960];
            fec.decode_fec(&packets[k + 1], &mut got).unwrap();
            sum_fec += pearson(&want, &got);
            plc.conceal(&mut got).unwrap();
            sum_plc += pearson(&want, &got);
            n += 1;
            for d in [&mut fec, &mut plc, &mut truth] {
                d.decode(&packets[k + 1], &mut out).unwrap();
            }
            k += 2;
        }
        (sum_fec / n as f64, sum_plc / n as f64, n)
    }

    /// Without any `LossHint` yet, the FEC floor already puts LBRR into
    /// every packet: the first loss is recovered, not extrapolated.
    #[test]
    fn fec_floor_recovers_first_loss_without_hint() {
        let (fec, plc, n) = fec_vs_plc_at_default_hint();
        eprintln!("{n} frames at hint 0: FEC {fec:.2}, PLC {plc:.2}");
        assert!(n > 50);
        assert!(
            fec > plc + 0.15,
            "FEC ({fec:.2}) hardly beats PLC ({plc:.2})"
        );
    }

    /// Mean RMS of three consecutive concealed frames relative to the
    /// original over voiced bursts, at decoder `complexity`.
    fn burst_energy_retention(complexity: i32) -> f64 {
        let voice = crate::dsp::testsignal::voice(48_000 * 4, 5, 0.1).samples;
        let frames = voice.as_chunks::<960>().0;
        let mut enc = Encoder::new().unwrap();
        let packets: Vec<Vec<u8>> = frames
            .iter()
            .map(|f| enc.encode(f).unwrap().to_vec())
            .collect();
        let mut truth = Decoder::with_complexity(complexity).unwrap();
        let mut dec = Decoder::with_complexity(complexity).unwrap();
        let (mut sum, mut n) = (0.0, 0);
        let mut out = vec![0.0f32; 960];
        let mut k = 0;
        while k + 4 < packets.len() {
            let mut want = Vec::new();
            for p in &packets[k..k + 3] {
                truth.decode(p, &mut out).unwrap();
                want.extend_from_slice(&out);
            }
            if k < 10 || rms(&want) < 0.05 {
                for p in &packets[k..k + 3] {
                    dec.decode(p, &mut out).unwrap();
                }
                k += 3;
                continue;
            }
            let mut got = Vec::new();
            for _ in 0..3 {
                dec.conceal(&mut out).unwrap();
                got.extend_from_slice(&out);
            }
            sum += (rms(&got) / rms(&want)).min(1.5) as f64;
            n += 1;
            // Resynchronise both on the next packets.
            for p in &packets[k + 3..k + 5] {
                dec.decode(p, &mut out).unwrap();
                truth.decode(p, &mut out).unwrap();
            }
            k += 5;
        }
        assert!(n > 10, "only {n} voiced bursts");
        sum / n as f64
    }

    /// Deep PLC (complexity ≥ 5) keeps talking through a 60 ms burst where
    /// classic concealment fades out. Generative, so energy, not SNR.
    #[test]
    fn deep_plc_keeps_energy_over_a_burst() {
        let classic = burst_energy_retention(0);
        let deep = burst_energy_retention(DECODER_COMPLEXITY);
        eprintln!("energy kept over 3 lost frames: classic {classic:.2}, deep {deep:.2}");
        assert!(
            (deep - 1.0).abs() < (classic - 1.0).abs(),
            "deep PLC {deep:.2} no closer to the source than classic {classic:.2}"
        );
    }

    /// Cost per frame by decoder complexity, normal vs. concealed:
    /// `cargo test --release -p icu-client decoder_complexity_bench -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn decoder_complexity_bench() {
        let voice = crate::dsp::testsignal::voice(48_000 * 10, 5, 0.1).samples;
        let mut enc = Encoder::new().unwrap();
        let packets: Vec<Vec<u8>> = voice
            .as_chunks::<960>()
            .0
            .iter()
            .map(|f| enc.encode(f).unwrap().to_vec())
            .collect();
        for c in [0, 5, 6, 7] {
            let mut dec = Decoder::with_complexity(c).unwrap();
            let mut out = vec![0.0f32; 960];
            let (mut t_dec, mut t_plc, mut n_dec, mut n_plc) = (
                std::time::Duration::ZERO,
                std::time::Duration::ZERO,
                0u32,
                0u32,
            );
            for (i, p) in packets.iter().enumerate() {
                let t = std::time::Instant::now();
                if i % 10 == 9 {
                    dec.conceal(&mut out).unwrap();
                    t_plc += t.elapsed();
                    n_plc += 1;
                } else {
                    dec.decode(p, &mut out).unwrap();
                    t_dec += t.elapsed();
                    n_dec += 1;
                }
            }
            eprintln!(
                "complexity {c}: decode {:.1} µs/frame, conceal {:.1} µs/frame",
                t_dec.as_secs_f64() * 1e6 / n_dec as f64,
                t_plc.as_secs_f64() * 1e6 / n_plc as f64
            );
        }
    }

    #[test]
    fn fec_and_plc_produce_full_frames() {
        let mut enc = Encoder::new().unwrap();
        enc.set_loss_permille(100).unwrap();
        let mut dec = Decoder::new().unwrap();
        let sig = speechlike(48_000, 9, 3.0, 0.0, 0.1);
        let packets: Vec<Vec<u8>> = sig
            .as_chunks::<960>()
            .0
            .iter()
            .map(|f| enc.encode(f).unwrap().to_vec())
            .collect();
        let mut out = vec![0.0f32; 960];
        for p in &packets[..10] {
            dec.decode(p, &mut out).unwrap();
        }
        // Packet 10 lost, recover from 11.
        let n = dec.decode_fec(&packets[11], &mut out).unwrap();
        assert_eq!(n, 960);
        dec.decode(&packets[11], &mut out).unwrap();
        let n = dec.conceal(&mut out).unwrap();
        assert_eq!(n, 960);
        assert_eq!(dec.nb_samples(&packets[0]), 960);
    }
}
