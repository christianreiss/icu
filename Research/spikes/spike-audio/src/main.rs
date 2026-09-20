//! Spike 3: Capture -> (RNNoise) -> Opus encode -> decode -> Playback, lokale Pipeline-Latenz.
use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::{Consumer, Producer, Split, Observer};
use std::sync::{atomic::{AtomicU64, Ordering}, Arc, Mutex};
use std::time::{Duration, Instant};

const RATE: u32 = 48_000;
const FRAME: usize = 480; // 10 ms
static DROPS: AtomicU64 = AtomicU64::new(0);

fn main() -> Result<()> {
    let host = cpal::default_host();
    println!("host: {:?}", host.id());
    let inp = host.default_input_device().ok_or(anyhow!("no input"))?;
    let out = host.default_output_device().ok_or(anyhow!("no output"))?;
    println!("input : {}", inp.name()?);
    println!("output: {}", out.name()?);
    let dic = inp.default_input_config()?;
    let doc = out.default_output_config()?;
    println!("default input cfg : {:?}", dic);
    println!("default output cfg: {:?}", doc);
    for c in inp.supported_input_configs()? { println!("  in  supports {:?}", c); }
    for c in out.supported_output_configs()? { println!("  out supports {:?}", c); }

    let mut icfg: cpal::StreamConfig = dic.clone().into();
    icfg.sample_rate = cpal::SampleRate(RATE); icfg.buffer_size = cpal::BufferSize::Fixed(FRAME as u32);
    let mut ocfg: cpal::StreamConfig = doc.clone().into();
    ocfg.sample_rate = cpal::SampleRate(RATE); ocfg.buffer_size = cpal::BufferSize::Fixed(FRAME as u32);
    let in_ch = icfg.channels as usize; let out_ch = ocfg.channels as usize;
    println!("requested: in {} ch, out {} ch, {} Hz, buffer Fixed({})", in_ch, out_ch, RATE, FRAME);

    // capture -> ring (mono f32 + capture timestamp per frame)
    let (mut prod, mut cons) = ringbuf::HeapRb::<(f32, u64)>::new(RATE as usize).split();
    let t0 = Instant::now();
    let cb_intervals = Arc::new(Mutex::new(Vec::<f64>::new()));
    let cbi = cb_intervals.clone();
    let last = Arc::new(AtomicU64::new(0));
    let in_stream = inp.build_input_stream(&icfg, move |data: &[f32], _| {
        let now = t0.elapsed().as_micros() as u64;
        let l = last.swap(now, Ordering::Relaxed);
        if l != 0 { cbi.lock().unwrap().push((now - l) as f64 / 1000.0); }
        for fr in data.chunks(in_ch) { let _ = prod.try_push((fr[0], now)); }
    }, |e| eprintln!("in err {e}"), None)?;

    // worker: frame -> rnnoise -> opus enc -> dec -> ring2
    let (mut prod2, mut cons2) = ringbuf::HeapRb::<(f32, u64)>::new(RATE as usize).split();
    let stats = Arc::new(Mutex::new(Stats::default()));
    let st = stats.clone();
    std::thread::spawn(move || {
        let mut enc = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip).unwrap();
        enc.set_bitrate(opus::Bitrate::Bits(64_000)).unwrap();
        enc.set_inband_fec(true).unwrap();
        enc.set_packet_loss_perc(5).unwrap();
        let mut dec = opus::Decoder::new(RATE, opus::Channels::Mono).unwrap();
        let mut dn = nnnoiseless::DenoiseState::new();
        let mut buf = vec![0f32; FRAME]; let mut ts = vec![0u64; FRAME];
        let mut dnout = vec![0f32; FRAME]; let mut pkt = vec![0u8; 1500]; let mut pcm = vec![0f32; FRAME];
        loop {
            let mut n = 0;
            while n < FRAME { if let Some((s, t)) = cons.try_pop() { buf[n] = s; ts[n] = t; n += 1; } else { std::thread::sleep(Duration::from_micros(500)); } }
            let a = Instant::now();
            let scaled: Vec<f32> = buf.iter().map(|x| x * 32768.0).collect();
            dn.process_frame(&mut dnout, &scaled);
            let dn_t = a.elapsed();
            let b = Instant::now();
            let inp: Vec<f32> = dnout.iter().map(|x| x / 32768.0).collect();
            let len = enc.encode_float(&inp, &mut pkt).unwrap();
            let enc_t = b.elapsed();
            let c = Instant::now();
            let got = dec.decode_float(&pkt[..len], &mut pcm, false).unwrap();
            let dec_t = c.elapsed();
            { let mut s = st.lock().unwrap(); s.frames += 1; s.bytes += len; s.dn += dn_t; s.enc += enc_t; s.dec += dec_t; s.max_enc = s.max_enc.max(enc_t); }
            for i in 0..got { let _ = prod2.try_push((pcm[i], ts[i])); }
        }
    });

    let e2e = Arc::new(Mutex::new(Vec::<f64>::new()));
    let e2 = e2e.clone();
    let out_stream = out.build_output_stream(&ocfg, move |data: &mut [f32], _| {
        let now = t0.elapsed().as_micros() as u64;
        // Minimaler Playout-Puffer: alles über 2 Frames Rückstand verwerfen (ersetzt in v1 den Jitter-Buffer)
        let backlog = cons2.occupied_len();
        if backlog > 2 * FRAME { let drop = backlog - FRAME; for _ in 0..drop { let _ = cons2.try_pop(); } DROPS.fetch_add(drop as u64, Ordering::Relaxed); }
        for fr in data.chunks_mut(out_ch) {
            let (s, t) = cons2.try_pop().unwrap_or((0.0, now));
            if t != now { e2.lock().unwrap().push((now - t) as f64 / 1000.0); }
            for x in fr.iter_mut() { *x = s * 0.0; } // stumm ausgeben (kein Feedback), Timing zählt
        }
    }, |e| eprintln!("out err {e}"), None)?;

    in_stream.play()?; out_stream.play()?;
    std::thread::sleep(Duration::from_secs(4));
    drop(in_stream); drop(out_stream);

    let s = stats.lock().unwrap();
    let n = s.frames.max(1) as u32;
    println!("frames={} avg_pkt={} B -> {:.1} kbps | rnnoise {:?}/frame | opus enc {:?}/frame (max {:?}) | dec {:?}/frame",
        s.frames, s.bytes / n as usize, (s.bytes as f64 * 8.0 / n as f64) / 10.0, s.dn / n, s.enc / n, s.max_enc, s.dec / n);
    let mut iv = cb_intervals.lock().unwrap().clone(); iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !iv.is_empty() { println!("capture callback interval ms: min {:.2} p50 {:.2} p99 {:.2} max {:.2} (n={})", iv[0], iv[iv.len()/2], iv[iv.len()*99/100], iv[iv.len()-1], iv.len()); }
    let mut e = e2e.lock().unwrap().clone(); e.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("dropped samples total: {}", DROPS.load(Ordering::Relaxed));
    if !e.is_empty() { println!("capture-cb -> playback-cb pipeline ms: min {:.2} p50 {:.2} p99 {:.2} (n={})", e[0], e[e.len()/2], e[e.len()*99/100], e.len()); }
    Ok(())
}
#[derive(Default)] struct Stats { frames: u64, bytes: usize, dn: Duration, enc: Duration, dec: Duration, max_enc: Duration }
