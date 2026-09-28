//! Admin-issued registration bundles and the first admin
//! (`PROTOCOL.md` §2.3, `Server/IMPLEMENTATION.md` §6.3, §6.4
//! `CreateInviteRequest`).

mod common;

use common::{TestServer, SOON};
use icu_proto::{error as ec, ControlMsg};
use x509_parser::prelude::FromDer;

fn common_name(der: &[u8]) -> String {
    x509_parser::certificate::X509Certificate::from_der(der)
        .unwrap()
        .1
        .subject()
        .iter_common_name()
        .next()
        .and_then(|a| a.as_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// Registers and connects the first user, who is the admin — the starting
/// point of most tests in this file.
async fn admin_client(server: &TestServer) -> common::Client {
    server.client("admin").await
}

async fn mint(client: &mut common::Client, name: &str, ttl_s: u64) -> (String, String) {
    client
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: name.to_string(),
            ttl_s,
        })
        .await;
    match client
        .recv_matching(|m| matches!(m, ControlMsg::CreateInviteResponse { .. }))
        .await
        .expect("CreateInviteResponse")
    {
        ControlMsg::CreateInviteResponse { bundle, expires } => (bundle, expires),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn the_first_registration_is_the_admin_and_the_second_is_not() {
    let server = TestServer::start().await;
    let mut first = server.connect(&server.enroll("first").await).await.unwrap();
    let mut second = server
        .connect(&server.enroll("second").await)
        .await
        .unwrap();
    assert!(matches!(
        first.hello("first").await,
        ControlMsg::Welcome { is_admin: true, .. }
    ));
    assert!(matches!(
        second.hello("second").await,
        ControlMsg::Welcome {
            is_admin: false,
            ..
        }
    ));
    server.shutdown().await;
}

#[tokio::test]
async fn a_bundle_from_an_admin_registers_a_client_with_its_own_name() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    // The label is only the placeholder name; the name in the certificate is
    // the one the invited person picks (`PROTOCOL.md` §2.3).
    let (bundle, _expires) = mint(&mut admin, "invited-person", 0).await;
    let registration = common::identity_from_bundle(&bundle);
    assert_eq!(common_name(&registration.cert), "invited-person");

    let identity = server
        .register(&bundle, "whatever-the-client-picks")
        .await
        .expect("bundle registers");
    assert_eq!(
        common_name(&identity.cert),
        "whatever-the-client-picks",
        "the CN comes from the CSR, the label binds nothing"
    );
    server.shutdown().await;
}

/// The server signs every bundle it issues with its CA key; a client refuses
/// one whose signature doesn't verify (`PROTOCOL.md` §2.3.1). This checks
/// the server's half — the raw bundle carries a valid signature — the
/// client's refusal of a tampered one is covered in icu-client's own tests.
#[tokio::test]
async fn the_issued_bundle_carries_a_verifying_signature() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;
    let (bundle, _expires) = mint(&mut admin, "signed-check", 0).await;

    let passport = icu_proto::passport::Passport::decode(&bundle).expect("bundle decodes");
    assert!(
        !passport.signature.is_empty(),
        "a registration bundle must carry a signature"
    );
    let (_, ca_cert) = x509_parser::certificate::X509Certificate::from_der(&passport.ca_der)
        .expect("ca_der parses");
    let ec_point = ca_cert.public_key().subject_public_key.data.to_vec();
    assert!(
        passport.verify_signature(&ec_point),
        "the signature must verify against the CA's own key"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_bundle_without_a_label_gets_a_placeholder() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (bundle, _expires) = mint(&mut admin, "", 0).await;
    let placeholder = common_name(&common::identity_from_bundle(&bundle).cert);
    assert!(placeholder.starts_with("neu-"), "{placeholder}");

    let identity = server.register(&bundle, "carol").await.expect("registers");
    assert_eq!(common_name(&identity.cert), "carol");
    server.shutdown().await;
}

#[tokio::test]
async fn a_taken_label_is_refused_non_fatally() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;
    admin
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: "ADMIN".into(),
            ttl_s: 0,
        })
        .await;
    match admin
        .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
        .await
    {
        Some(ControlMsg::Error { code, .. }) => assert_eq!(code, ec::NAME_TAKEN),
        other => panic!("expected NAME_TAKEN, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_non_admin_cannot_mint_a_bundle_and_the_connection_stays_open() {
    let server = TestServer::start().await;
    // The first registration becomes admin; register a second, non-admin user.
    server.enroll("admin").await;
    let mut member = server.client("member").await;

    member
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: "someone".into(),
            ttl_s: 0,
        })
        .await;
    match member.recv_msg().await {
        ControlMsg::Error { code, .. } => assert_eq!(code, ec::NOT_ADMIN),
        other => panic!("expected Error(NOT_ADMIN), got {other:?}"),
    }

    // Non-fatal: the connection must still answer a Ping.
    member.send_msg(&ControlMsg::Ping { nonce: 42 }).await;
    match member.recv_timeout(SOON).await {
        Some(ControlMsg::Pong { nonce, .. }) => assert_eq!(nonce, 42),
        other => panic!("expected Pong after NOT_ADMIN, got {other:?}"),
    }
    server.shutdown().await;
}

#[tokio::test]
async fn a_requested_ttl_bounds_the_bundle() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (bundle, _expires) = mint(&mut admin, "short", 3600).await;
    let cert = common::identity_from_bundle(&bundle).cert;
    let (_, cert) = x509_parser::certificate::X509Certificate::from_der(&cert).unwrap();
    let left =
        cert.validity().not_after.timestamp() - time::OffsetDateTime::now_utc().unix_timestamp();
    assert!((3500..=3700).contains(&left), "{left}");
    server.shutdown().await;
}

async fn user_list(client: &mut common::Client) -> Vec<icu_proto::UserSummary> {
    client.send_msg(&ControlMsg::UserListRequest).await;
    match client
        .recv_matching(|m| matches!(m, ControlMsg::UserListResponse { .. }))
        .await
    {
        Some(ControlMsg::UserListResponse { users }) => users,
        other => panic!("expected UserListResponse, got {other:?}"),
    }
}

#[tokio::test]
async fn an_invited_user_is_pending_until_registered() {
    let server = TestServer::start().await;
    let mut admin = admin_client(&server).await;

    let (bundle, _expires) = mint(&mut admin, "guest", 0).await;
    let pending = |users: &[icu_proto::UserSummary], name: &str| {
        users.iter().find(|u| u.name == name).map(|u| u.pending)
    };
    let users = user_list(&mut admin).await;
    assert_eq!(pending(&users, "guest"), Some(true));
    assert_eq!(pending(&users, "admin"), Some(false));

    server.register(&bundle, "guest").await.expect("registers");
    let users = user_list(&mut admin).await;
    assert_eq!(pending(&users, "guest"), Some(false));
    server.shutdown().await;
}

/// Sends `CreateInviteRequest` and returns the `Error` code, `None` when a
/// bundle came back.
async fn invite_code(client: &mut common::Client) -> Option<u32> {
    client
        .send_msg(&ControlMsg::CreateInviteRequest {
            name: String::new(),
            ttl_s: 0,
        })
        .await;
    match client
        .recv_matching(|m| {
            matches!(
                m,
                ControlMsg::CreateInviteResponse { .. } | ControlMsg::Error { .. }
            )
        })
        .await
    {
        Some(ControlMsg::Error { code, .. }) => Some(code),
        Some(_) => None,
        None => panic!("no answer"),
    }
}

/// Every open invitation is a user in `users.toml` until it is confirmed or
/// expires: their number is capped, over every way in.
#[tokio::test]
async fn open_invitations_are_capped() {
    let server = TestServer::start_with(|c| c.limits.max_pending_invites = Some(2)).await;
    let mut admin = admin_client(&server).await;
    assert_eq!(invite_code(&mut admin).await, None);
    assert_eq!(invite_code(&mut admin).await, None);
    assert_eq!(invite_code(&mut admin).await, Some(ec::RATE_LIMITED));
    assert!(!ec::is_fatal(ec::RATE_LIMITED));

    // The host's CLI is held to it as well.
    let answer = server
        .handle
        .admin(icu_server::AdminRequest::Invite {
            name: String::new(),
            ttl_s: None,
        })
        .await;
    assert!(
        matches!(&answer, icu_server::AdminResponse::Error { error, .. } if error == "RATE_LIMITED"),
        "{answer:?}"
    );
    server.shutdown().await;
}

/// An admin mints at most `limits.invites_per_admin_per_hour` bundles
/// in-band; the host's CLI is not limited.
#[tokio::test]
async fn in_band_invitations_are_rate_limited_per_admin() {
    let server = TestServer::start_with(|c| c.limits.invites_per_admin_per_hour = 2).await;
    let mut admin = admin_client(&server).await;
    assert_eq!(invite_code(&mut admin).await, None);
    assert_eq!(invite_code(&mut admin).await, None);
    assert_eq!(invite_code(&mut admin).await, Some(ec::RATE_LIMITED));
    let _ = server.invite("").await;
    server.shutdown().await;
}

/// `limits.invites_per_admin_per_hour = 0` turns in-band invitations off
/// instead of meaning one an hour (external review 2026-09-27); the host's
/// CLI still issues them.
#[tokio::test]
async fn zero_invitations_per_hour_means_none_in_band() {
    let server = TestServer::start_with(|c| c.limits.invites_per_admin_per_hour = 0).await;
    let mut admin = admin_client(&server).await;
    assert_eq!(invite_code(&mut admin).await, Some(ec::RATE_LIMITED));
    let _ = server.invite("").await;
    server.shutdown().await;
}

/// Revoking an invitation nobody used frees its place under
/// `limits.max_pending_invites` right away; it used to hold the place until
/// it expired, up to a week.
///
/// On an empty server, so the invitation is the first bundle and marked
/// admin: withdrawing it used to fail with `LAST_ADMIN` (external review
/// 2026-09-27, S-1), and whoever redeemed a leaked one became the only
/// admin.
#[tokio::test]
async fn a_revoked_invitation_frees_its_place() {
    use icu_server::{AdminRequest, AdminResponse};
    let server = TestServer::start_with(|c| c.limits.max_pending_invites = Some(1)).await;
    let invite = |name: &str| {
        server.handle.admin(AdminRequest::Invite {
            name: name.to_string(),
            ttl_s: None,
        })
    };
    let first = invite("guest").await;
    let AdminResponse::Invite {
        bundle, is_admin, ..
    } = &first
    else {
        panic!("{first:?}")
    };
    assert!(is_admin, "the first bundle of an empty server");
    let full = invite("guest2").await;
    assert!(
        matches!(&full, AdminResponse::Error { error, .. } if error == "RATE_LIMITED"),
        "{full:?}"
    );

    let revoked = server
        .handle
        .admin(AdminRequest::Revoke {
            user: "guest".into(),
        })
        .await;
    assert!(
        matches!(revoked, AdminResponse::Revoke { .. }),
        "{revoked:?}"
    );
    assert!(
        server.register(bundle, "mallory").await.is_err(),
        "the withdrawn bundle is dead"
    );
    let again = invite("guest2").await;
    assert!(matches!(again, AdminResponse::Invite { .. }), "{again:?}");
    server.shutdown().await;
}

/// A refused `CreateInviteRequest` — a taken name here — minted nothing and
/// costs nothing of the admin's hourly budget; the token used to be taken
/// before the bundle was issued, and refusals ate the hour.
#[tokio::test]
async fn a_refused_in_band_invitation_keeps_the_hourly_budget() {
    let server = TestServer::start_with(|c| c.limits.invites_per_admin_per_hour = 1).await;
    let mut admin = admin_client(&server).await;
    for _ in 0..2 {
        admin
            .send_msg(&ControlMsg::CreateInviteRequest {
                name: "ADMIN".into(),
                ttl_s: 0,
            })
            .await;
        match admin
            .recv_matching(|m| matches!(m, ControlMsg::Error { .. }))
            .await
        {
            Some(ControlMsg::Error { code, .. }) => assert_eq!(code, ec::NAME_TAKEN),
            other => panic!("expected NAME_TAKEN, got {other:?}"),
        }
    }
    assert_eq!(invite_code(&mut admin).await, None, "the hour's one bundle");
    assert_eq!(invite_code(&mut admin).await, Some(ec::RATE_LIMITED));
    server.shutdown().await;
}
