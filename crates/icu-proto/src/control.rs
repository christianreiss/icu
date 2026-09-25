//! Control messages (`PROTOCOL.md` §2.2, §2.3).
//!
//! Variant order is normative: postcard encodes the discriminant as the varint
//! of the variant index. New variants go at the end, new fields at the end of a
//! variant, each behind a capability bit.

use serde::{Deserialize, Serialize};

use crate::{
    framing::{self, MAX_CONTROL_FRAME},
    AudioParams, ProtoError, Session, UserFlags, UserId, UserInfo, UserSummary,
};

/// Protocol minor version of this implementation. v1 baseline is 1; a minor 0
/// does not exist (`PROTOCOL.md` §2.2, §8). New mandatory variants (like
/// `UserRtt`) ship without a minor bump: client and server are always
/// deployed in lockstep in this project (`PROTOCOL.md` §2.2).
pub const MINOR: u16 = 1;

/// Capability bits. All zero in v1, reserved for later extensions.
pub const CAPS: u32 = 0;

/// Messages on the control stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMsg {
    /// 0, C→S. First message after the handshake, within 5 s.
    Hello {
        /// Highest minor the client speaks.
        minor: u16,
        /// Client capability bits.
        caps: u32,
        /// `<name>/<semver> <os>`, at most 64 bytes.
        client: String,
        /// Initial state bits.
        initial: UserFlags,
        /// Obsolete. The display name lives in the certificate's subject CN
        /// (`PROTOCOL.md` §2.1); the server ignores this field. Kept because
        /// postcard encodes fields positionally.
        name: String,
    },
    /// 1, S→C. Complete state.
    Welcome {
        /// Negotiated minor, `min(client, server)`.
        minor: u16,
        /// Server capability bits.
        caps: u32,
        /// The client's own entry, including `user_id` and the stored name.
        /// A client whose own leaf carries a different CN asks for a new
        /// certificate carrying `you.name` (`PROTOCOL.md` §2.2).
        you: UserInfo,
        /// Audio parameters for this room.
        audio: AudioParams,
        /// All other participants.
        users: Vec<UserInfo>,
        /// Lowest allowed client version, SemVer.
        min_client: String,
        /// Whether `you` may send `CreateInviteRequest`.
        is_admin: bool,
        /// The client authenticated with a registration certificate
        /// (`PROTOCOL.md` §2.3). `users` is then empty, the session is not in
        /// the room, and the only message the server accepts besides `Ping` is
        /// a `RenewRequest` with a fresh key.
        registration: bool,
    },
    /// 2, S→C.
    UserJoined {
        /// The participant that joined.
        user: UserInfo,
    },
    /// 3, S→C.
    UserLeft {
        /// Session of the participant that left.
        session: Session,
        /// Error code < 256 (`PROTOCOL.md` §6).
        reason: u8,
    },
    /// 4, S→C. Broadcast after every change, including to the originator.
    UserState {
        /// Whose state changed.
        session: Session,
        /// New state bits.
        flags: UserFlags,
    },
    /// 5, C→S. At most 20/s per client.
    SetState {
        /// Requested state bits; `SPEAKING` and reserved bits are ignored.
        flags: UserFlags,
    },
    /// 6, C→S. Application RTT, every 5 s.
    Ping {
        /// Client chosen nonce.
        nonce: u64,
    },
    /// 7, S→C.
    Pong {
        /// Echo of [`ControlMsg::Ping::nonce`].
        nonce: u64,
        /// Unix time in milliseconds, UTC.
        server_time_ms: u64,
    },
    /// 8, S→C. Non fatal; fatal errors travel in `CONNECTION_CLOSE`.
    Error {
        /// Error code (`PROTOCOL.md` §6).
        code: u32,
        /// Human readable detail.
        detail: String,
    },
    /// 9, C→S. Certificate renewal over the authenticated connection, and the
    /// only way to change a display name: the CSR's subject CN is the name the
    /// client asks for (`PROTOCOL.md` §2.2).
    RenewRequest {
        /// PKCS#10 CSR, DER, at most 2 KiB. Exactly one subject CN, which must
        /// pass `validate_name` and be free server-wide.
        csr_der: Vec<u8>,
    },
    /// 10, S→C.
    RenewResponse {
        /// The freshly issued leaf certificate, DER. Its CN is the name the
        /// CSR asked for.
        leaf_der: Vec<u8>,
    },
    /// 11, C→S. Loss measured by a receiver over the last 2 s.
    LossReport {
        /// Whose audio stream the loss was measured on.
        speaker: Session,
        /// Loss in per mille.
        loss_permille: u16,
    },
    /// 12, S→C. Maximum of all reports for this speaker.
    LossHint {
        /// Loss in per mille.
        loss_permille: u16,
    },
    /// 13, C→S, and S→C with `speaker = 0`.
    KeyframeRequest {
        /// Whose camera should emit an IDR. `0` towards the speaker.
        speaker: Session,
    },
    /// 14, obsolete. Renaming runs through `RenewRequest`, because the name
    /// lives in the certificate.
    ///
    /// The variant number stays reserved so the numbering remains normative;
    /// no client sends it, and a server that receives it closes with
    /// `PROTOCOL_VIOLATION`.
    SetName {
        /// The new display name.
        name: String,
    },
    /// 15, S→C.
    UserRenamed {
        /// Who was renamed.
        session: Session,
        /// The new display name.
        name: String,
    },
    /// 16, S→C. Worst video-drop rate among this sender's receivers, measured
    /// over the last 2 s. Mirrors `LossHint`'s max-across-reporters rule.
    VideoCongestionHint {
        /// Per mille of frames displaced in a receiver's slot before delivery.
        drop_permille: u16,
    },
    /// 17, C→S. Admin only (`Welcome.is_admin`); rejected with `Error{NOT_ADMIN}`
    /// otherwise. Non fatal either way. Issues a registration certificate.
    CreateInviteRequest {
        /// Placeholder name the registration certificate carries until the
        /// invited user picks their own. Empty means a generated one. Must be
        /// free server-wide (`NAME_TAKEN` otherwise).
        name: String,
        /// Lifetime in seconds. `0` means the default (`limits::INVITE_TTL_S`).
        ttl_s: u64,
    },
    /// 18, S→C.
    CreateInviteResponse {
        /// Registration bundle: an ASCII-armored passport carrying the
        /// one-time key, the registration certificate and the CA.
        bundle: String,
        /// When the registration certificate expires, RFC 3339 UTC.
        expires: String,
    },
    /// 19, C→S. Promote/demote another user. Admin only, rejected with
    /// `AdminActionError{NOT_ADMIN}` otherwise; rejected with
    /// `AdminActionError{LAST_ADMIN}` if this would demote the last
    /// remaining functional admin. Non fatal either way.
    AdminSetRole {
        /// The user whose role changes.
        user_id: UserId,
        /// The new role.
        is_admin: bool,
    },
    /// 20, S→C. Ack to the acting admin.
    AdminSetRoleOk {
        /// The user whose role changed.
        user_id: UserId,
        /// The new role.
        is_admin: bool,
    },
    /// 21, S→C. Unicast to every session of the affected user (not a room
    /// broadcast): their own cached role changed, live, without a reconnect.
    UserAdminChanged {
        /// The new role.
        is_admin: bool,
    },
    /// 22, C→S. Reversible login block, independent of certificate revoke.
    /// Admin only; same rejection shape as `AdminSetRole`.
    AdminSetDisabled {
        /// The affected user.
        user_id: UserId,
        /// The new disabled state.
        disabled: bool,
    },
    /// 23, S→C. Ack to the acting admin.
    AdminSetDisabledOk {
        /// The affected user.
        user_id: UserId,
        /// The new disabled state.
        disabled: bool,
    },
    /// 24, C→S. Revokes every certificate of `user_id` and kicks it. Admin
    /// only; same rejection shape as `AdminSetRole`.
    AdminRevokeUser {
        /// The affected user.
        user_id: UserId,
    },
    /// 25, S→C. Ack to the acting admin.
    AdminRevokeUserOk {
        /// The affected user.
        user_id: UserId,
        /// How many live connections were closed.
        closed: u32,
    },
    /// 26, C→S. Admin only; same rejection shape as `AdminSetRole`.
    UserListRequest,
    /// 27, S→C. Every registered user, online or not.
    UserListResponse {
        /// One row per registered user.
        users: Vec<UserSummary>,
    },
    /// 28, S→C. Rejection of variant 19/22/24/26/31/33. `user_id` is `0` for a
    /// `UserListRequest` failure, since it names no single target.
    AdminActionError {
        /// The targeted user, or `0`.
        user_id: UserId,
        /// Error code (`PROTOCOL.md` §6).
        code: u32,
        /// Human readable detail.
        detail: String,
    },
    /// 29, S→C. Broadcast periodically (every ~2 s) while the room has at
    /// least two participants. One entry per online participant with its
    /// current server-measured QUIC path RTT to the server, in milliseconds
    /// (saturating). This is the server's own view of the connection, not a
    /// client's self-reported `Ping`/`Pong` round trip, so every viewer sees
    /// the same number for a given peer.
    UserRtt {
        /// `(session, rtt_ms)` pairs, one per online participant.
        rtts: Vec<(Session, u16)>,
    },
    /// 30, S→C. The server's current CRL, DER, signed by its CA
    /// (`PROTOCOL.md` §1, §2.2). Sent right after `Welcome` and to everyone
    /// whenever it changes. The client keeps the one with the highest CRL
    /// number whose signature its CA verifies and checks the server leaf
    /// against it on every connect.
    CrlUpdate {
        /// The CRL, DER.
        crl_der: Vec<u8>,
    },
    /// 31, C→S. Closes every live session of `user_id` with `KICKED`
    /// without touching the store: the user may connect again. Admin only;
    /// same rejection shape as `AdminSetRole`.
    AdminKick {
        /// The affected user.
        user_id: UserId,
    },
    /// 32, S→C. Ack to the acting admin.
    AdminKickOk {
        /// The affected user.
        user_id: UserId,
        /// How many live connections were closed.
        closed: u32,
    },
    /// 33, C→S. Server-enforced mute and deafen of `user_id`, absolute and
    /// persisted: while set, bit0/bit1 of the user's `UserFlags` stay set
    /// whatever the client sends. Admin only; same rejection shape as
    /// `AdminSetRole`.
    AdminSetRestriction {
        /// The affected user.
        user_id: UserId,
        /// Enforced mute.
        muted: bool,
        /// Enforced deafen.
        deafened: bool,
    },
    /// 34, S→C. Ack to the acting admin.
    AdminSetRestrictionOk {
        /// The affected user.
        user_id: UserId,
        /// Enforced mute.
        muted: bool,
        /// Enforced deafen.
        deafened: bool,
    },
    /// 35, S→C. Unicast to every session of the affected user, on change and
    /// right after `Welcome` while any restriction is set: which of their
    /// own mute/deafen an admin enforces.
    UserRestrictionChanged {
        /// Enforced mute.
        muted: bool,
        /// Enforced deafen.
        deafened: bool,
    },
}

impl ControlMsg {
    /// Maximum length of `Hello.client` in bytes (`PROTOCOL.md` §2.2).
    pub const MAX_CLIENT_STRING: usize = 64;
    /// Maximum CSR size in bytes (`PROTOCOL.md` §2.2, §2.3).
    pub const MAX_CSR_DER: usize = 2 * 1024;

    /// Encodes the message as a complete control frame.
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        framing::encode_frame(self, MAX_CONTROL_FRAME)
    }

    /// Decodes a control frame body.
    pub fn decode(body: &[u8]) -> Result<ControlMsg, ProtoError> {
        framing::decode_body(body, MAX_CONTROL_FRAME)
    }

    /// Stable variant name, for logs.
    pub fn variant(&self) -> &'static str {
        match self {
            ControlMsg::Hello { .. } => "Hello",
            ControlMsg::Welcome { .. } => "Welcome",
            ControlMsg::UserJoined { .. } => "UserJoined",
            ControlMsg::UserLeft { .. } => "UserLeft",
            ControlMsg::UserState { .. } => "UserState",
            ControlMsg::SetState { .. } => "SetState",
            ControlMsg::Ping { .. } => "Ping",
            ControlMsg::Pong { .. } => "Pong",
            ControlMsg::Error { .. } => "Error",
            ControlMsg::RenewRequest { .. } => "RenewRequest",
            ControlMsg::RenewResponse { .. } => "RenewResponse",
            ControlMsg::LossReport { .. } => "LossReport",
            ControlMsg::LossHint { .. } => "LossHint",
            ControlMsg::KeyframeRequest { .. } => "KeyframeRequest",
            ControlMsg::SetName { .. } => "SetName",
            ControlMsg::UserRenamed { .. } => "UserRenamed",
            ControlMsg::VideoCongestionHint { .. } => "VideoCongestionHint",
            ControlMsg::CreateInviteRequest { .. } => "CreateInviteRequest",
            ControlMsg::CreateInviteResponse { .. } => "CreateInviteResponse",
            ControlMsg::AdminSetRole { .. } => "AdminSetRole",
            ControlMsg::AdminSetRoleOk { .. } => "AdminSetRoleOk",
            ControlMsg::UserAdminChanged { .. } => "UserAdminChanged",
            ControlMsg::AdminSetDisabled { .. } => "AdminSetDisabled",
            ControlMsg::AdminSetDisabledOk { .. } => "AdminSetDisabledOk",
            ControlMsg::AdminRevokeUser { .. } => "AdminRevokeUser",
            ControlMsg::AdminRevokeUserOk { .. } => "AdminRevokeUserOk",
            ControlMsg::UserListRequest => "UserListRequest",
            ControlMsg::UserListResponse { .. } => "UserListResponse",
            ControlMsg::AdminActionError { .. } => "AdminActionError",
            ControlMsg::UserRtt { .. } => "UserRtt",
            ControlMsg::CrlUpdate { .. } => "CrlUpdate",
            ControlMsg::AdminKick { .. } => "AdminKick",
            ControlMsg::AdminKickOk { .. } => "AdminKickOk",
            ControlMsg::AdminSetRestriction { .. } => "AdminSetRestriction",
            ControlMsg::AdminSetRestrictionOk { .. } => "AdminSetRestrictionOk",
            ControlMsg::UserRestrictionChanged { .. } => "UserRestrictionChanged",
        }
    }

    /// Whether this variant may legitimately arrive at the server.
    pub fn is_client_to_server(&self) -> bool {
        matches!(
            self,
            ControlMsg::Hello { .. }
                | ControlMsg::SetState { .. }
                | ControlMsg::Ping { .. }
                | ControlMsg::RenewRequest { .. }
                | ControlMsg::LossReport { .. }
                | ControlMsg::KeyframeRequest { .. }
                | ControlMsg::CreateInviteRequest { .. }
                | ControlMsg::AdminSetRole { .. }
                | ControlMsg::AdminSetDisabled { .. }
                | ControlMsg::AdminRevokeUser { .. }
                | ControlMsg::UserListRequest
                | ControlMsg::AdminKick { .. }
                | ControlMsg::AdminSetRestriction { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UserId;

    fn user(id: UserId, session: Session, name: &str) -> UserInfo {
        UserInfo {
            user_id: id,
            session,
            name: name.to_string(),
            flags: UserFlags(UserFlags::MUTED),
        }
    }

    fn all_variants() -> Vec<ControlMsg> {
        vec![
            ControlMsg::Hello {
                minor: MINOR,
                caps: CAPS,
                client: "icu/0.1.0 linux".into(),
                initial: UserFlags(UserFlags::MUTED),
                name: "alice".into(),
            },
            ControlMsg::Welcome {
                minor: MINOR,
                caps: CAPS,
                you: user(1, 1, "alice"),
                audio: AudioParams::default(),
                users: vec![user(2, 2, "bob"), user(3, 3, "carol")],
                min_client: "0.1.0".into(),
                is_admin: true,
                registration: false,
            },
            ControlMsg::UserJoined {
                user: user(4, 4, "dave"),
            },
            ControlMsg::UserLeft {
                session: 4,
                reason: crate::error::TIMEOUT as u8,
            },
            ControlMsg::UserState {
                session: 2,
                flags: UserFlags(UserFlags::SPEAKING),
            },
            ControlMsg::SetState {
                flags: UserFlags(UserFlags::DEAFENED),
            },
            ControlMsg::Ping { nonce: u64::MAX },
            ControlMsg::Pong {
                nonce: 7,
                server_time_ms: 1_789_000_000_000,
            },
            ControlMsg::Error {
                code: crate::error::NAME_TAKEN,
                detail: "name taken".into(),
            },
            ControlMsg::RenewRequest {
                csr_der: vec![0x30, 0x82, 0x01],
            },
            ControlMsg::RenewResponse {
                leaf_der: vec![0x30, 0x82, 0x02],
            },
            ControlMsg::LossReport {
                speaker: 2,
                loss_permille: 123,
            },
            ControlMsg::LossHint { loss_permille: 0 },
            ControlMsg::KeyframeRequest { speaker: 0 },
            ControlMsg::SetName {
                name: "alice2".into(),
            },
            ControlMsg::UserRenamed {
                session: 1,
                name: "alice2".into(),
            },
            ControlMsg::VideoCongestionHint { drop_permille: 50 },
            ControlMsg::CreateInviteRequest {
                name: "eve".into(),
                ttl_s: 3600,
            },
            ControlMsg::CreateInviteResponse {
                bundle: "-----BEGIN ICU PASSPORT-----".into(),
                expires: "2026-09-22T00:00:00Z".into(),
            },
            ControlMsg::AdminSetRole {
                user_id: 5,
                is_admin: true,
            },
            ControlMsg::AdminSetRoleOk {
                user_id: 5,
                is_admin: true,
            },
            ControlMsg::UserAdminChanged { is_admin: true },
            ControlMsg::AdminSetDisabled {
                user_id: 5,
                disabled: true,
            },
            ControlMsg::AdminSetDisabledOk {
                user_id: 5,
                disabled: true,
            },
            ControlMsg::AdminRevokeUser { user_id: 5 },
            ControlMsg::AdminRevokeUserOk {
                user_id: 5,
                closed: 1,
            },
            ControlMsg::UserListRequest,
            ControlMsg::UserListResponse {
                users: vec![UserSummary {
                    user_id: 5,
                    name: "eve".into(),
                    created: "2026-09-21T00:00:00Z".into(),
                    is_admin: false,
                    disabled: true,
                    revoked: false,
                    online: true,
                    pending: false,
                    muted: true,
                    deafened: false,
                }],
            },
            ControlMsg::AdminActionError {
                user_id: 5,
                code: crate::error::LAST_ADMIN,
                detail: "cannot demote the last admin".into(),
            },
            ControlMsg::UserRtt {
                rtts: vec![(2, 34), (3, 128)],
            },
            ControlMsg::CrlUpdate {
                crl_der: vec![0x30, 0x03, 0x02, 0x01, 0x01],
            },
            ControlMsg::AdminKick { user_id: 5 },
            ControlMsg::AdminKickOk {
                user_id: 5,
                closed: 1,
            },
            ControlMsg::AdminSetRestriction {
                user_id: 5,
                muted: true,
                deafened: false,
            },
            ControlMsg::AdminSetRestrictionOk {
                user_id: 5,
                muted: true,
                deafened: false,
            },
            ControlMsg::UserRestrictionChanged {
                muted: true,
                deafened: true,
            },
        ]
    }

    /// §10: roundtrip of every `ControlMsg` variant.
    #[test]
    fn roundtrip_all_control_variants() {
        let variants = all_variants();
        assert_eq!(variants.len(), 36, "v1 has exactly 36 control variants");
        for msg in variants {
            let frame = msg.encode().unwrap();
            let body = framing::split_frame(&frame, MAX_CONTROL_FRAME)
                .unwrap()
                .unwrap();
            let back = ControlMsg::decode(body).unwrap();
            assert_eq!(back, msg, "roundtrip failed for {}", msg.variant());
        }
    }

    /// The discriminant is the variant index as a varint; the numbering in
    /// `PROTOCOL.md` §2.2 is therefore normative wire format.
    #[test]
    fn variant_discriminants_match_protocol_numbering() {
        for (index, msg) in all_variants().into_iter().enumerate() {
            let body = postcard::to_stdvec(&msg).unwrap();
            assert_eq!(
                body[0] as usize,
                index,
                "{} must have discriminant {index}",
                msg.variant()
            );
        }
    }

    /// §10 / Spike 2b: an unknown variant decodes to `Err`, never a panic.
    #[test]
    fn unknown_variant_is_an_error() {
        // Discriminant 36 does not exist yet.
        let body = [36u8, 0, 0];
        let err = ControlMsg::decode(&body).unwrap_err();
        assert!(
            matches!(err, ProtoError::UnknownVariant),
            "expected UnknownVariant, got {err:?}"
        );
        assert_eq!(err.error_code(), crate::error::PROTOCOL_VIOLATION);
    }

    /// A future minor's message must not be mistaken for a known one.
    #[test]
    fn truncated_body_is_an_error() {
        let err = ControlMsg::decode(&[0u8]).unwrap_err();
        assert!(matches!(err, ProtoError::Decode(_)));
        assert_eq!(err.error_code(), crate::error::PROTOCOL_VIOLATION);
    }

    #[test]
    fn direction_table() {
        assert!(ControlMsg::Ping { nonce: 1 }.is_client_to_server());
        assert!(!ControlMsg::Pong {
            nonce: 1,
            server_time_ms: 0
        }
        .is_client_to_server());
        assert!(!ControlMsg::LossHint { loss_permille: 0 }.is_client_to_server());
        assert!(ControlMsg::LossReport {
            speaker: 1,
            loss_permille: 0
        }
        .is_client_to_server());
        assert!(ControlMsg::CreateInviteRequest {
            name: "eve".into(),
            ttl_s: 0
        }
        .is_client_to_server());
        assert!(!ControlMsg::CreateInviteResponse {
            bundle: String::new(),
            expires: String::new()
        }
        .is_client_to_server());
        assert!(ControlMsg::AdminSetRole {
            user_id: 1,
            is_admin: true
        }
        .is_client_to_server());
        assert!(ControlMsg::AdminSetDisabled {
            user_id: 1,
            disabled: true
        }
        .is_client_to_server());
        assert!(ControlMsg::AdminRevokeUser { user_id: 1 }.is_client_to_server());
        assert!(ControlMsg::UserListRequest.is_client_to_server());
        assert!(ControlMsg::AdminKick { user_id: 1 }.is_client_to_server());
        assert!(ControlMsg::AdminSetRestriction {
            user_id: 1,
            muted: true,
            deafened: true
        }
        .is_client_to_server());
        assert!(!ControlMsg::UserRestrictionChanged {
            muted: true,
            deafened: false
        }
        .is_client_to_server());
        assert!(!ControlMsg::UserAdminChanged { is_admin: true }.is_client_to_server());
        assert!(!ControlMsg::UserListResponse { users: vec![] }.is_client_to_server());
        assert!(!ControlMsg::UserRtt { rtts: vec![] }.is_client_to_server());
        assert!(!ControlMsg::CrlUpdate { crl_der: vec![] }.is_client_to_server());
        // Obsolete: renaming runs through `RenewRequest`, so a server that
        // sees variant 14 treats it as a protocol violation.
        assert!(!ControlMsg::SetName {
            name: "alice".into()
        }
        .is_client_to_server());
    }
}
