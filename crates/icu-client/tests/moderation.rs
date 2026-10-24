//! Fresh moderation data must be distinguishable even when its rows did not change.
#![cfg(unix)]

mod common;

use common::*;
use icu_client::{audio::fake::FakeBackend, engine::Command};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_user_list_responses_advance_the_moderation_generation() {
    let server = TestServer::start().await;
    let (engine, _dir) = server
        .engine("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;
    server
        .handle
        .admin(icu_server::AdminRequest::Promote {
            user: "alice".into(),
        })
        .await;
    let before = wait_for(&engine, SOON, |s| s.is_admin).await;

    engine.command(Command::RequestUserList);
    let first = wait_for(&engine, SOON, |s| {
        s.user_list_generation != before.user_list_generation
    })
    .await;
    assert!(!first.user_list.is_empty());
    engine.command(Command::RequestUserList);
    let second = wait_for(&engine, SOON, |s| {
        s.user_list_generation != first.user_list_generation
    })
    .await;
    assert_eq!(second.user_list, first.user_list);
    assert_eq!(
        second.user_list_generation,
        first.user_list_generation.wrapping_add(1)
    );

    engine.shutdown();
    server.handle.shutdown().await;
}
