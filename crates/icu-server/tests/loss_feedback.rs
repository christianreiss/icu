//! Loss feedback is accepted only from current recipients of a speaker's audio.
mod common;

use common::{Client, TestServer};
use icu_proto::{ControlMsg, UserFlags};
use icu_server::AdminRequest;
use std::time::Duration;

async fn report(reporter: &mut Client, speaker: u16, loss_permille: u16) {
    reporter
        .send_msg(&ControlMsg::LossReport {
            speaker,
            loss_permille,
        })
        .await;
    // Ordered control processing: the report has been handled before Pong.
    reporter.send_msg(&ControlMsg::Ping { nonce: 77 }).await;
    reporter
        .recv_matching(|m| matches!(m, ControlMsg::Pong { nonce: 77, .. }))
        .await
        .expect("report processed");
}

async fn expect_hint(speaker: &mut Client, expected: u16) {
    let got = speaker
        .recv_matching(|m| matches!(m, ControlMsg::LossHint { .. }))
        .await;
    assert_eq!(
        got,
        Some(ControlMsg::LossHint {
            loss_permille: expected
        })
    );
}

async fn expect_no_hint(speaker: &mut Client) {
    let until = tokio::time::Instant::now() + Duration::from_millis(350);
    while let Some(msg) = speaker
        .recv_timeout(until.saturating_duration_since(tokio::time::Instant::now()))
        .await
    {
        assert!(
            !matches!(msg, ControlMsg::LossHint { .. }),
            "ineligible report generated {msg:?}"
        );
    }
}

#[tokio::test]
async fn deafened_non_listener_cannot_inject_loss_into_two_healthy_listeners() {
    let s = TestServer::start().await;
    let mut speaker = s.client("speaker").await;
    let _healthy_one = s.client("healthy-one").await;
    let _healthy_two = s.client("healthy-two").await;
    let mut attacker = s.client("attacker").await;
    attacker
        .send_msg(&ControlMsg::SetState {
            flags: UserFlags(UserFlags::DEAFENED),
        })
        .await;
    let attacker_session = attacker.session;
    attacker
        .recv_matching(|m| matches!(m, ControlMsg::UserState { session, flags } if *session == attacker_session && flags.deafened()))
        .await
        .expect("deafened");
    report(&mut attacker, speaker.session, 1000).await;
    expect_no_hint(&mut speaker).await;
    s.shutdown().await;
}

async fn cached_report_is_removed_on_deafen(admin: bool) {
    let s = TestServer::start().await;
    let mut speaker = s.client("speaker").await;
    let mut reporter = s.client("reporter").await;
    let _healthy = s.client("healthy").await;
    report(&mut reporter, speaker.session, 1000).await;
    expect_hint(&mut speaker, 1000).await;
    if admin {
        s.handle
            .admin(AdminRequest::Deafen {
                user: "reporter".into(),
            })
            .await;
    } else {
        reporter
            .send_msg(&ControlMsg::SetState {
                flags: UserFlags(UserFlags::DEAFENED),
            })
            .await;
    }
    // Recovery must arrive before the ordinary six-second report expiry.
    tokio::time::timeout(Duration::from_secs(1), expect_hint(&mut speaker, 0))
        .await
        .expect("deafen promptly invalidates the cached loss report");
    report(&mut reporter, speaker.session, 1000).await;
    expect_no_hint(&mut speaker).await;
    s.shutdown().await;
}

#[tokio::test]
async fn self_deafen_removes_cached_loss_and_rejects_further_reports() {
    cached_report_is_removed_on_deafen(false).await;
}

#[tokio::test]
async fn admin_deafen_removes_cached_loss_and_rejects_further_reports() {
    cached_report_is_removed_on_deafen(true).await;
}

#[tokio::test]
async fn loss_above_one_thousand_is_ignored_without_losing_valid_feedback() {
    let s = TestServer::start().await;
    let mut speaker = s.client("speaker").await;
    let mut reporter = s.client("reporter").await;
    report(&mut reporter, speaker.session, 1001).await;
    expect_no_hint(&mut speaker).await;
    report(&mut reporter, speaker.session, 80).await;
    expect_hint(&mut speaker, 80).await;
    s.shutdown().await;
}
