//! Real QUIC control delivery and legacy capability isolation.
mod common;
use common::{Client, TestServer};
use icu_proto::{AudioHeader, AudioLevel, AudioWindow, ControlMsg, CAP_AUDIO_FEEDBACK};
use std::time::Duration;

async fn capable(s: &TestServer, name: &str) -> Client {
    let id = s.enroll(name).await;
    let mut c = s.connect(&id).await.unwrap();
    c.hello_caps(name, CAP_AUDIO_FEEDBACK).await.unwrap();
    c
}
async fn hint(c: &mut Client, value: Option<i16>, reporters: u16) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(c.recv_msg().await, ControlMsg::AudioLevelHint { correction_cdb, reporters:r,.. } if correction_cdb==value && r==reporters) {
                break;
            }
        }
    })
    .await
    .expect("expected consensus hint");
}
#[tokio::test]
async fn two_receivers_agree_then_deafen_excludes_one_and_old_client_gets_no_hints() {
    let s = TestServer::start().await;
    let mut a = capable(&s, "a").await;
    let mut b = capable(&s, "b").await;
    let mut c = capable(&s, "c").await;
    let mut old = s.client("old").await;
    let level = AudioLevel {
        level_cdb: -2800,
        correction_cdb: 400,
        peak_cdb: -600,
        near_full_permille: 0,
        speech_ms: 1500,
    };
    let window = AudioWindow {
        gain_epoch: 1,
        first_ts: 0,
        end_ts: 96_000,
        report_id: 0,
    };
    a.conn
        .send_datagram(
            AudioHeader {
                gain_epoch: 1,
                ts: 96_000,
                ..Default::default()
            }
            .datagram(&[0x78])
            .unwrap()
            .into(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    for client in [&mut b, &mut c] {
        client
            .send_msg(&ControlMsg::AudioLevelReport {
                speaker: a.session,
                window,
                level,
            })
            .await;
    }
    hint(&mut a, Some(400), 2).await;
    c.send_msg(&ControlMsg::SetState {
        flags: icu_proto::UserFlags(icu_proto::UserFlags::DEAFENED),
    })
    .await;
    // The remaining receiver must supply a fresh window. Server timer
    // ticks and membership changes cannot turn its old report into one.
    tokio::time::sleep(Duration::from_millis(2100)).await;
    let window = AudioWindow {
        gain_epoch: 1,
        first_ts: 96_000,
        end_ts: 192_000,
        report_id: 1,
    };
    a.conn
        .send_datagram(
            AudioHeader {
                gain_epoch: 1,
                ts: 192_000,
                ..Default::default()
            }
            .datagram(&[0x78])
            .unwrap()
            .into(),
        )
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    b.send_msg(&ControlMsg::AudioLevelReport {
        speaker: a.session,
        window,
        level,
    })
    .await;
    hint(&mut a, Some(400), 1).await;
    // Unsupported peers may send a fabricated report, but cannot influence consensus.
    old.send_msg(&ControlMsg::AudioLevelReport {
        speaker: a.session,
        window,
        level: AudioLevel {
            correction_cdb: -1200,
            ..level
        },
    })
    .await;
    old.send_msg(&ControlMsg::Ping { nonce: 991 }).await;
    loop {
        match old.recv_msg().await {
            ControlMsg::AudioLevelHint { .. } => panic!("legacy client received unknown variant"),
            ControlMsg::Pong { nonce: 991, .. } => break,
            _ => {}
        }
    }
    s.shutdown().await;
}
