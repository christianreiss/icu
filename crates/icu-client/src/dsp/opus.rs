//! Opus encoder/decoder wrappers with the v1 profile (`PROTOCOL.md` §3,
//! `Client/IMPLEMENTATION.md` §6).

use icu_proto::limits::LOSS_HINT_MAX_PERCENT;

/// Sample rate.
pub const RATE: u32 = 48_000;
/// Largest packet the protocol allows.
pub const MAX_PACKET: usize = icu_proto::header::AUDIO_MAX_PAYLOAD;

/// Voice encoder.
pub struct Encoder {
    inner: opus::Encoder,
    buf: Vec<u8>,
    loss_perc: i32,
}

impl Encoder {
    /// VOIP, mono, 48 kbit/s constrained VBR, FEC on, DTX off, complexity 10.
    pub fn new() -> Result<Encoder, opus::Error> {
        let mut e = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
        e.set_signal(opus::Signal::Voice)?;
        e.set_bitrate(opus::Bitrate::Bits(48_000))?;
        e.set_vbr(true)?;
        e.set_vbr_constraint(true)?;
        e.set_inband_fec(true)?;
        e.set_packet_loss_perc(0)?;
        e.set_dtx(false)?;
        e.set_complexity(10)?;
        Ok(Encoder {
            inner: e,
            buf: vec![0u8; MAX_PACKET],
            loss_perc: 0,
        })
    }

    /// Encodes one frame (480 or 960 samples, ±1.0) and returns the packet.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8], opus::Error> {
        let n = self.inner.encode_float(pcm, &mut self.buf)?;
        Ok(&self.buf[..n])
    }

    /// Applies a `LossHint`: per mille → whole percent, capped at 20.
    pub fn set_loss_permille(&mut self, permille: u16) -> Result<(), opus::Error> {
        let perc = (permille as u32)
            .div_ceil(10)
            .min(LOSS_HINT_MAX_PERCENT as u32) as i32;
        if perc != self.loss_perc {
            self.inner.set_packet_loss_perc(perc)?;
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
    inner: opus::Decoder,
    last_frame: usize,
}

impl Decoder {
    /// Mono 48 kHz decoder.
    pub fn new() -> Result<Decoder, opus::Error> {
        Ok(Decoder {
            inner: opus::Decoder::new(RATE, opus::Channels::Mono)?,
            last_frame: 960,
        })
    }

    /// Decodes a packet into `out`, returning the number of samples.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, opus::Error> {
        let n = self.inner.decode_float(packet, out, false)?;
        self.last_frame = n;
        Ok(n)
    }

    /// Recovers the lost frame `n` from packet `n+1`'s in-band FEC.
    pub fn decode_fec(
        &mut self,
        next_packet: &[u8],
        out: &mut [f32],
    ) -> Result<usize, opus::Error> {
        // The frame length comes from the peer's packet (up to 120 ms);
        // clamped so the slice stays in bounds and libopus decides instead.
        let frame = self
            .inner
            .get_nb_samples(next_packet)
            .unwrap_or(self.last_frame)
            .min(out.len());
        let n = self
            .inner
            .decode_float(next_packet, &mut out[..frame], true)?;
        Ok(n)
    }

    /// Packet loss concealment for one frame.
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<usize, opus::Error> {
        let frame = self.last_frame.min(out.len());
        self.inner.decode_float(&[], &mut out[..frame], false)
    }

    /// Duration in samples of `packet`.
    pub fn nb_samples(&self, packet: &[u8]) -> usize {
        self.inner.get_nb_samples(packet).unwrap_or(self.last_frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::testsignal::{harmonic_voice, pearson, speechlike};

    /// A peer's 60 ms packet must not panic FEC recovery into a buffer
    /// sized for 20 ms.
    #[test]
    fn fec_with_oversized_next_packet_does_not_panic() {
        let mut enc =
            opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        let pkt = enc
            .encode_vec_float(&vec![0.0f32; 2880], MAX_PACKET)
            .unwrap();
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
        assert_eq!(enc.loss_perc(), 0);
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
