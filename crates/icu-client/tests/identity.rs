//! The name lives in the certificate (`PROTOCOL.md` §2.1): renaming issues a
//! new certificate, a device that missed a rename catches up by itself, and a
//! passport moves the whole identity to another installation.
//!
//! Unix only: the in-process server (`common`) is.
#![cfg(unix)]

mod common;

use std::time::Duration;

use common::{wait_connected, wait_for, wait_until, TestServer, SOON};
use icu_client::{
    audio::fake::FakeBackend,
    config::{Identity, ModeSetting, Paths, Settings},
    conninfo::CertInfo,
    engine::Command,
};

fn leaf_cn(paths: &Paths) -> String {
    let id = Identity::load(paths)
        .expect("identity readable")
        .expect("identity present");
    CertInfo::from_der(&id.leaf_der).subject
}

/// A rename must not orphan the key: a passport exported before it still has
/// to match the certificate afterwards.
#[tokio::test]
async fn rename_issues_a_new_certificate_and_keeps_the_key() {
    let server = TestServer::start().await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    let before = Identity::load(&paths).unwrap().unwrap();
    assert_eq!(CertInfo::from_der(&before.leaf_der).subject, "alice");

    engine.command(Command::RequestRename("alicia".into()));
    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("alicia")
    })
    .await;
    assert!(!state.rename_pending);

    assert!(
        wait_until(SOON, || leaf_cn(&paths) == "alicia").await,
        "the new certificate reaches the disk"
    );
    let after = Identity::load(&paths).unwrap().unwrap();
    assert_eq!(after.key_der, before.key_der, "the key is reused");
    assert_ne!(after.leaf_der, before.leaf_der, "a new leaf was issued");
    assert_eq!(after.server.name, "alicia");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// A taken name comes back as `NAME_TAKEN` and changes nothing.
#[tokio::test]
async fn renaming_onto_a_taken_name_reports_it_under_the_field() {
    let server = TestServer::start().await;
    let (_bob, _bob_paths, _bob_dir) = server.enroll("bob").await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    engine.command(Command::RequestRename("BOB".into()));
    let state = wait_for(&engine, SOON, |s| s.name_error.is_some()).await;
    assert!(!state.rename_pending, "the field is usable again");
    assert_eq!(state.me.unwrap().name, "alice");
    assert_eq!(leaf_cn(&paths), "alice", "the certificate is untouched");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Two devices share one certificate (passport). One renames; the other picks
/// the new name up on its next start, silently, and ends up with a
/// certificate that carries it.
#[tokio::test]
async fn a_stale_certificate_catches_up_on_the_next_start() {
    let server = TestServer::start().await;
    let (stale, paths_a, _dir_a) = server.enroll("alice").await;

    // Device A renames and goes away.
    let engine_a = server
        .engine_from(
            stale.clone(),
            paths_a.clone(),
            Box::new(FakeBackend::silent().0),
            false,
        )
        .expect("engine a");
    wait_connected(&engine_a).await;
    engine_a.command(Command::RequestRename("alicia".into()));
    wait_for(&engine_a, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("alicia")
    })
    .await;
    engine_a.shutdown();

    // Device B still holds the pre-rename certificate.
    let dir_b = tempfile::tempdir().expect("tempdir");
    let paths_b = Paths::new(Some(dir_b.path().to_path_buf()));
    stale.save(&paths_b).expect("seed device b");
    let engine_b = server
        .engine_from(
            stale,
            paths_b.clone(),
            Box::new(FakeBackend::silent().0),
            false,
        )
        .expect("engine b");

    let state = wait_for(&engine_b, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("alicia")
    })
    .await;
    assert!(
        state.name_error.is_none() && !state.rename_pending,
        "a catch-up is silent, it is not something the user asked for"
    );
    assert!(
        wait_until(SOON, || leaf_cn(&paths_b) == "alicia").await,
        "device b renews itself onto the current name"
    );

    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// The passport round trip, end to end: export on one installation, import
/// into an empty one, connect as the same user.
#[tokio::test]
async fn a_passport_moves_an_identity_to_a_second_installation() {
    let server = TestServer::start().await;
    let (identity, paths_a, _dir_a) = server.enroll("alice").await;

    let armored = identity.to_passport();
    let imported = Identity::from_passport(&armored).expect("passport decodes");
    assert_eq!(imported, identity, "nothing is lost in the armor");

    let dir_b = tempfile::tempdir().expect("tempdir");
    let paths_b = Paths::new(Some(dir_b.path().to_path_buf()));
    imported.save(&paths_b).expect("save imported");
    assert_eq!(leaf_cn(&paths_b), leaf_cn(&paths_a));

    let engine = server
        .engine_from(imported, paths_b, Box::new(FakeBackend::silent().0), false)
        .expect("engine on the imported identity");
    let state = wait_connected(&engine).await;
    assert_eq!(state.me.unwrap().name, "alice");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Importing a passport while connected swaps the identity and reconnects
/// with it — the way out of a revoked or broken certificate, too.
#[tokio::test]
async fn replacing_the_identity_reconnects_as_the_other_user() {
    let server = TestServer::start().await;
    let (bob, _bob_paths, _bob_dir) = server.enroll("bob").await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    let state = wait_connected(&engine).await;
    assert_eq!(state.me.unwrap().name, "alice");

    engine.command(Command::ReplaceIdentity(Box::new(bob), None));
    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("bob")
    })
    .await;
    assert!(matches!(
        state.conn,
        icu_client::engine::ConnStatus::Connected
    ));
    assert!(
        wait_until(SOON, || leaf_cn(&paths) == "bob").await,
        "the imported identity is on disk"
    );
    assert!(
        wait_until(SOON, || !paths
            .identity_dir()
            .with_extension("old")
            .exists())
        .await,
        "after its first Welcome the replaced set goes"
    );

    engine.shutdown();
    server.handle.shutdown().await;
}

/// A passport that ends fatally before its first `Welcome` — here revoked —
/// does not cost the identity that worked: it is put back on disk and in
/// force, with its settings, and connects again; the window says why
/// (review 2026-09-27, C-2).
#[tokio::test]
async fn a_failing_passport_import_gives_way_to_the_identity_that_worked() {
    let server = TestServer::start().await;
    // Alice first: the first user is the admin, and the last admin cannot
    // be revoked.
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;
    let (bob, _bob_paths, _bob_dir) = server.enroll("bob").await;
    let revoked = server
        .handle
        .admin(icu_server::AdminRequest::Revoke { user: "bob".into() })
        .await;
    assert!(
        matches!(revoked, icu_server::AdminResponse::Revoke { .. }),
        "{revoked:?}"
    );

    let mut settings = Settings::default();
    settings.audio.mode = ModeSetting::Ptt;
    engine.command(Command::ReplaceIdentity(
        Box::new(bob),
        Some(Box::new(settings)),
    ));
    let state = wait_for(&engine, SOON, |s| {
        s.banner
            .as_ref()
            .is_some_and(|b| b.text.contains("Passport ließ sich nicht verwenden"))
    })
    .await;
    assert!(
        !matches!(state.conn, icu_client::engine::ConnStatus::Fatal(_)),
        "{:?}",
        state.conn
    );
    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("alice")
            && matches!(s.conn, icu_client::engine::ConnStatus::Connected)
    })
    .await;
    assert_eq!(state.settings.audio.mode, ModeSetting::Vad, "settings back");
    assert_eq!(leaf_cn(&paths), "alice");
    assert!(!paths.identity_dir().with_extension("old").exists());

    engine.shutdown();
    server.handle.shutdown().await;
}

/// An invitation that was already redeemed, imported into a running
/// client, is never written over the identity in force: the server refuses
/// it, and the old identity reconnects (review 2026-09-27, C-2).
#[tokio::test]
async fn a_spent_invitation_import_gives_way_to_the_identity_that_worked() {
    use icu_client::net::register::register;
    let server = TestServer::start().await;
    let bundle = Identity::from_passport(&server.invite_bundle("carol").await).unwrap();
    let spent = tempfile::tempdir().unwrap();
    register(
        &bundle,
        "carol",
        "icu-test/0.1.0 linux",
        Some(server.addr()),
        &Paths::new(Some(spent.path().to_path_buf())),
    )
    .await
    .expect("first redemption");
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    engine.command(Command::ReplaceIdentity(Box::new(bundle), None));
    wait_for(&engine, SOON, |s| {
        s.banner
            .as_ref()
            .is_some_and(|b| b.text.contains("Einladung ließ sich nicht verwenden"))
    })
    .await;
    wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("alice")
            && matches!(s.conn, icu_client::engine::ConnStatus::Connected)
    })
    .await;
    assert_eq!(leaf_cn(&paths), "alice");
    assert!(!Identity::load(&paths).unwrap().unwrap().is_registration());

    engine.shutdown();
    server.handle.shutdown().await;
}

/// A malformed passport is refused before anything is written.
#[test]
fn a_broken_passport_is_refused() {
    assert!(Identity::from_passport("nicht mal armor").is_err());
    assert!(Identity::from_passport(&format!(
        "{}\nAAAA\n{}\n",
        icu_proto::passport::ARMOR_BEGIN,
        icu_proto::passport::ARMOR_END
    ))
    .is_err());
}

/// The key inside a passport is a real, reloadable key — the property the
/// whole renewal path rests on.
#[tokio::test]
async fn a_passport_carries_a_usable_key() {
    let server = TestServer::start().await;
    let (identity, _paths, _dir) = server.enroll("alice").await;
    let armored = identity.to_passport();
    let back = Identity::from_passport(&armored).expect("decode");
    assert!(
        rcgen::KeyPair::try_from(back.key_der.as_slice()).is_ok(),
        "the key survives the armor"
    );
    tokio::time::sleep(Duration::from_millis(1)).await;
    server.handle.shutdown().await;
}

/// A registration bundle imported into a running client (settings) is
/// registered by the engine itself, under its placeholder name, and the
/// bundle is spent.
#[tokio::test]
async fn an_imported_bundle_registers_itself() {
    let server = TestServer::start().await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    let bundle = Identity::from_passport(&server.invite_bundle("carol").await).unwrap();
    engine.command(Command::ReplaceIdentity(Box::new(bundle.clone()), None));
    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("carol")
            && matches!(s.conn, icu_client::engine::ConnStatus::Connected)
    })
    .await;
    assert!(!state.is_admin);
    let on_disk = Identity::load(&paths).unwrap().unwrap();
    assert!(!on_disk.is_registration(), "the registration was swapped");
    assert_ne!(on_disk.key_der, bundle.key_der, "on a key of its own");
    assert_eq!(leaf_cn(&paths), "carol");

    engine.shutdown();
    server.handle.shutdown().await;
}

/// Review item 1: after a fatal close (here: revoked), importing a passport
/// reconnects at once instead of waiting for a restart.
#[tokio::test]
async fn a_passport_import_recovers_from_a_fatal_state() {
    let server = TestServer::start().await;
    let (alice, _alice_paths, _alice_dir) = server.enroll("alice").await;
    let (engine, _paths, _dir) = server
        .engine_with_paths("bob", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    server
        .handle
        .admin(icu_server::AdminRequest::Revoke { user: "bob".into() })
        .await;
    wait_for(&engine, SOON, |s| {
        matches!(s.conn, icu_client::engine::ConnStatus::Fatal(_))
    })
    .await;

    engine.command(Command::ReplaceIdentity(Box::new(alice), None));
    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("alice")
            && matches!(s.conn, icu_client::engine::ConnStatus::Connected)
    })
    .await;
    assert!(state.me.is_some());

    engine.shutdown();
    server.handle.shutdown().await;
}

/// The clean close after `RenewResponse` is the receipt that revokes the
/// bundle, so it is sent only once the identity is on disk: a failed save
/// leaves the bundle usable for another attempt (`PROTOCOL.md` §2.3).
#[tokio::test]
async fn a_failed_save_does_not_burn_the_bundle() {
    use icu_client::net::register::register;
    let server = TestServer::start().await;
    let bundle = Identity::from_passport(&server.invite_bundle("").await).unwrap();

    // `identity.new` is a file, so the new set cannot be written.
    let broken = tempfile::tempdir().unwrap();
    std::fs::write(broken.path().join("identity.new"), b"").unwrap();
    let broken_paths = Paths::new(Some(broken.path().to_path_buf()));
    let client = "icu-test/0.1.0 linux";
    let err = register(&bundle, "dora", client, Some(server.addr()), &broken_paths)
        .await
        .expect_err("saving must fail");
    assert!(err.problem().text.contains("gespeichert"), "{err}");

    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(Some(dir.path().to_path_buf()));
    register(&bundle, "dora", client, Some(server.addr()), &paths)
        .await
        .expect("the bundle is still valid");
    assert_eq!(leaf_cn(&paths), "dora");
}

/// Two devices share one passport. Device A renames and logs in with the new
/// certificate, which revokes every older one (`PROTOCOL.md` §2.2 Nr. 10).
/// Device B, still on the old certificate, is refused — and told to fetch the
/// passport from the other device, not to ask for an invitation (which would
/// create a second account).
#[tokio::test]
async fn a_device_left_behind_by_a_renewal_is_pointed_to_the_passport() {
    let server = TestServer::start().await;
    let (stale, paths_a, _dir_a) = server.enroll("alice").await;
    let dir_b = tempfile::tempdir().expect("tempdir");
    let paths_b = Paths::new(Some(dir_b.path().to_path_buf()));
    stale.save(&paths_b).expect("seed device b");

    // Device A renames: a new certificate, stored on disk.
    let engine_a = server
        .engine_from(
            stale.clone(),
            paths_a.clone(),
            Box::new(FakeBackend::silent().0),
            false,
        )
        .expect("engine a");
    wait_connected(&engine_a).await;
    engine_a.command(Command::RequestRename("alicia".into()));
    assert!(
        wait_until(SOON, || leaf_cn(&paths_a) == "alicia").await,
        "device a stores the renamed certificate"
    );
    engine_a.shutdown();

    // ... and logs in with it: that login retires the old certificate.
    let renewed = Identity::load(&paths_a).unwrap().unwrap();
    let engine_a = server
        .engine_from(
            renewed,
            paths_a.clone(),
            Box::new(FakeBackend::silent().0),
            false,
        )
        .expect("engine a, renewed");
    let state = wait_connected(&engine_a).await;
    assert_eq!(state.me.unwrap().name, "alicia");

    // Device B still holds the old certificate.
    let engine_b = server
        .engine_from(stale, paths_b, Box::new(FakeBackend::silent().0), false)
        .expect("engine b");
    let state = wait_for(&engine_b, SOON, |s| {
        matches!(s.conn, icu_client::engine::ConnStatus::Fatal(_))
    })
    .await;
    let icu_client::engine::ConnStatus::Fatal(problem) = state.conn else {
        unreachable!("the predicate waited for Fatal");
    };
    assert!(problem.text.contains("Passport"), "{}", problem.text);
    assert!(problem.text.contains("anderen Gerät"), "{}", problem.text);
    assert!(!problem.text.contains("Einladung"), "{}", problem.text);

    engine_a.shutdown();
    engine_b.shutdown();
    server.handle.shutdown().await;
}

/// A passport is stored before it is taken into use. When it cannot be
/// written, the identity in force stays — in memory, on disk and on the
/// connection — and the window says so.
#[tokio::test]
async fn a_passport_that_cannot_be_stored_is_not_taken_over() {
    let server = TestServer::start().await;
    let (bob, _bob_paths, _bob_dir) = server.enroll("bob").await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    // `identity.new` is a file, so the new set cannot be written.
    std::fs::write(paths.identity_dir().with_extension("new"), b"").unwrap();
    let mut settings = Settings::default();
    settings.audio.mode = ModeSetting::Ptt;
    engine.command(Command::ReplaceIdentity(
        Box::new(bob),
        Some(Box::new(settings)),
    ));
    let state = wait_for(&engine, SOON, |s| s.banner.is_some()).await;
    assert!(
        state.banner.unwrap().text.contains("gespeichert"),
        "a storage problem"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let state = engine.snapshot();
    assert_eq!(state.me.unwrap().name, "alice");
    assert!(matches!(
        state.conn,
        icu_client::engine::ConnStatus::Connected
    ));
    assert_eq!(leaf_cn(&paths), "alice");
    assert_eq!(
        state.settings.audio.mode,
        ModeSetting::Vad,
        "the settings stay with the identity in force"
    );

    engine.shutdown();
    server.handle.shutdown().await;
}

/// A passport with settings, taken into a running client: the person's
/// settings arrive with the identity, this machine keeps its devices, and
/// `settings.toml` follows.
#[tokio::test]
async fn a_passport_brings_the_settings_but_not_the_devices() {
    let server = TestServer::start().await;
    let (bob, _bob_paths, _bob_dir) = server.enroll("bob").await;
    let (engine, paths, _dir) = server
        .engine_with_paths("alice", Box::new(FakeBackend::silent().0), false)
        .await;
    wait_connected(&engine).await;

    // This machine's microphone.
    let mut here = engine.snapshot().settings;
    here.audio.input_device = Some("mic-here".into());
    engine.command(Command::UpdateSettings(Box::new(here)));
    wait_for(&engine, SOON, |s| {
        s.settings.audio.input_device.as_deref() == Some("mic-here")
    })
    .await;

    // Bob's other device: push to talk on a mouse button, dark, someone
    // louder — and a microphone of its own.
    let mut there = Settings::default();
    there.audio.mode = ModeSetting::Ptt;
    there.audio.input_device = Some("mic-there".into());
    there.hotkeys.ptt.key = "BTN_SIDE".into();
    there.ui.theme = "dark".into();
    there.set_volume(42, 150.0);
    let (passport, with_settings) = bob.to_passport_with_settings(&there);
    assert!(with_settings);
    let (identity, settings) = Identity::from_passport_with_settings(&passport).unwrap();
    engine.command(Command::ReplaceIdentity(
        Box::new(identity),
        settings.map(Box::new),
    ));

    let state = wait_for(&engine, SOON, |s| {
        s.me.as_ref().map(|m| m.name.as_str()) == Some("bob")
    })
    .await;
    assert_eq!(state.settings.audio.mode, ModeSetting::Ptt);
    assert_eq!(state.settings.hotkeys.ptt.key, "BTN_SIDE");
    assert_eq!(state.settings.ui.theme, "dark");
    assert_eq!(state.settings.volume(42), 150.0);
    assert_eq!(
        state.settings.audio.input_device.as_deref(),
        Some("mic-here"),
        "the device stays this machine's"
    );
    assert!(
        wait_until(SOON, || Settings::load(&paths)
            .is_ok_and(|s| s.audio.mode == ModeSetting::Ptt
                && s.audio.input_device.as_deref() == Some("mic-here")))
        .await,
        "settings.toml follows"
    );

    engine.shutdown();
    server.handle.shutdown().await;
}
