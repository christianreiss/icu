//! End-to-end playout quality: A sends the synthetic test tone, B records
//! what its playback callback gets, and the recording is compared with the
//! source window by window (`dsp::testsignal::continuity`). Every gap,
//! concealed frame or skipped/inserted frame shows up as a glitch.
//!
//! Run with `--nocapture` to see the numbers.

mod common;

use std::time::Duration;

use common::*;
use icu_client::{
    audio::fake::FakeBackend,
    dsp::{
        opus::{Decoder, Encoder},
        testsignal::{continuity, harmonic_voice, Continuity},
    },
    engine::{ConnStatus, Engine},
    stats::DebugStats,
};

/// Seconds recorded after the stream settled.
const RECORD: Duration = Duration::from_secs(6);

async fn record(engine_b: &Engine, probe_b: &icu_client::audio::fake::FakeProbe) -> Continuity {
    wait_for(engine_b, SOON, |s| {
        s.conn == ConnStatus::Connected && s.peers.iter().any(|p| p.speaking)
    })
    .await;
    // Let the jitter buffer find its depth first.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let st = engine_b.stats();
    let counters = || {
        [
            &st.frames_plc,
            &st.frames_fec,
            &st.playback_underruns,
            &st.jitter_late,
            &st.jitter_underruns,
            &st.jitter_skipped,
        ]
        .map(DebugStats::get)
    };
    let before = counters();
    probe_b.clear();
    tokio::time::sleep(RECORD).await;
    let played = probe_b.played();
    if let Ok(dir) = std::env::var("ICU_PLAYOUT_DUMP") {
        // Raw f32 LE mono 48 kHz, e.g. `sox -t f32 -r 48000 -c 1 x.f32 x.wav`.
        let name = std::thread::current()
            .name()
            .unwrap_or("playout")
            .replace("::", "_");
        let bytes: Vec<u8> = played.iter().flat_map(|v| v.to_le_bytes()).collect();
        let _ = std::fs::write(format!("{dir}/{name}.f32"), bytes);
    }
    let c = continuity(&played, &codec_reference(), 960);
    let after = counters();
    let d: Vec<u64> = after.iter().zip(before).map(|(a, b)| a - b).collect();
    eprintln!(
        "{c:?} while recording: plc={} fec={} play_underruns={} jb_late={} jb_underruns={} jb_skipped={}",
        d[0], d[1], d[2], d[3], d[4], d[5],
    );
    assert!(c.windows > 100, "too little voiced audio compared: {c:?}");
    c
}

/// The test tone as a clean Opus round trip plays it: the sender encodes
/// the 4 s loop from its first sample on, so after a lap to settle the
/// encoder the decoded third lap is what B should hear, sample for sample.
/// Comparing against this instead of the raw tone leaves only transport
/// and playout faults, not codec colouring.
fn codec_reference() -> Vec<f32> {
    let lap = 48_000 * 4;
    let tone = harmonic_voice(lap, 0.15);
    let mut enc = Encoder::new().unwrap();
    let mut dec = Decoder::new().unwrap();
    let mut out = Vec::with_capacity(3 * lap);
    let mut pcm = vec![0.0f32; 960];
    for frame in tone
        .iter()
        .cycle()
        .take(3 * lap)
        .collect::<Vec<_>>()
        .chunks(960)
    {
        let packet = enc
            .encode(&frame.iter().map(|v| **v).collect::<Vec<_>>())
            .unwrap()
            .to_vec();
        let n = dec.decode(&packet, &mut pcm).unwrap();
        out.extend_from_slice(&pcm[..n]);
    }
    out[2 * lap..3 * lap].to_vec()
}

/// The link of `playout_survives_network_jitter`; override with
/// `ICU_JITTER=<spread ms>,<stall chance>,<stall min ms>,<stall max ms>`.
fn jitter_model() -> Jitter {
    let ms = Duration::from_millis;
    if let Ok(v) = std::env::var("ICU_JITTER") {
        let f: Vec<f32> = v.split(',').map(|x| x.trim().parse().unwrap()).collect();
        return Jitter {
            spread: ms(f[0] as u64),
            stall_chance: f[1],
            stall: (ms(f[2] as u64), ms(f[3] as u64)),
        };
    }
    Jitter {
        spread: ms(15),
        stall_chance: 0.03,
        stall: (ms(30), ms(70)),
    }
}

/// Share of judged windows that glitched, in percent.
fn glitch_pct(c: &Continuity) -> f64 {
    100.0 * c.glitches as f64 / c.windows as f64
}

/// Baseline: loopback, 5 ms quantum — the stream must be clean.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playout_is_clean_on_loopback() {
    let server = TestServer::start().await;
    let (backend_a, _) = FakeBackend::silent();
    let (engine_a, _dir_a) = server.engine("alice", Box::new(backend_a), true).await;
    let (backend_b, probe_b) = FakeBackend::silent();
    let (engine_b, _dir_b) = server.engine("bob", Box::new(backend_b), false).await;
    wait_connected(&engine_a).await;

    let c = record(&engine_b, &probe_b).await;
    assert!(glitch_pct(&c) < 1.0, "{c:?}");

    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// B's graph runs at PipeWire's default quantum (1024 samples, 21.3 ms),
/// more than the mixer used to keep queued: every callback came up short.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playout_is_clean_at_a_large_quantum() {
    let server = TestServer::start().await;
    let (backend_a, _) = FakeBackend::silent();
    let (engine_a, _dir_a) = server.engine("alice", Box::new(backend_a), true).await;
    let (backend_b, probe_b) =
        FakeBackend::with_block(Box::new(|_, out| out.fill(0.0)), false, 1024);
    let (engine_b, _dir_b) = server.engine("bob", Box::new(backend_b), false).await;
    wait_connected(&engine_a).await;

    let c = record(&engine_b, &probe_b).await;
    assert!(glitch_pct(&c) < 1.0, "{c:?}");
    assert!(
        DebugStats::get(&engine_b.stats().playback_underruns) < 5,
        "playback ran dry"
    );

    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// B sits behind a Wi-Fi-like link: up to 15 ms jitter per packet and
/// stalls of 30–70 ms. The first stalls may cost a concealed frame while
/// the buffer deepens; after that the stream has to stay clean.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playout_survives_network_jitter() {
    let server = TestServer::start().await;
    let proxy = JitterProxy::start(server.addr(), jitter_model(), 7).await;
    let (backend_a, _) = FakeBackend::silent();
    let (engine_a, _dir_a) = server.engine("alice", Box::new(backend_a), true).await;
    let (backend_b, probe_b) = FakeBackend::silent();
    let (engine_b, _dir_b) = server
        .engine_via("bob", Box::new(backend_b), false, proxy.addr)
        .await;
    wait_connected(&engine_a).await;

    let c = record(&engine_b, &probe_b).await;
    // Before the jitter buffer waited for late packets, each stall cost an
    // audible gap: 3 glitches per 6 s at this profile.
    assert!(c.glitches <= 1, "{c:?}");

    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}
