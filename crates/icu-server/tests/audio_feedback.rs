//! Real QUIC control delivery and legacy capability isolation.
mod common;
use common::{Client, TestServer};
use icu_proto::{AudioLevel, ControlMsg, CAP_AUDIO_FEEDBACK};
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
            if c.recv_msg().await
                == (ControlMsg::AudioLevelHint {
                    correction_cdb: value,
                    reporters,
                })
            {
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
    for client in [&mut b, &mut c] {
        client
            .send_msg(&ControlMsg::AudioLevelReport {
                speaker: a.session,
                level,
            })
            .await;
    }
    hint(&mut a, Some(400), 2).await;
    c.send_msg(&ControlMsg::SetState {
        flags: icu_proto::UserFlags(icu_proto::UserFlags::DEAFENED),
    })
    .await;
    hint(&mut a, Some(400), 1).await;
    // Unsupported peers may send a fabricated report, but cannot influence consensus.
    old.send_msg(&ControlMsg::AudioLevelReport {
        speaker: a.session,
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
