//! Control and enrollment messages (`PROTOCOL.md` §2.2, §2.3).
//!
//! Variant order is normative: postcard encodes the discriminant as the varint
//! of the variant index. New variants go at the end, new fields at the end of a
//! variant, each behind a capability bit.

use serde::{Deserialize, Serialize};

use crate::{
    framing::{self, MAX_CONTROL_FRAME, MAX_ENROLL_FRAME},
    AudioParams, ProtoError, Session, UserFlags, UserId, UserInfo, UserSummary,
};

/// Protocol minor version of this implementation. v1 baseline is 1; a minor 0
/// does not exist (`PROTOCOL.md` §2.2, §8).
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
        /// Desired display name. Only taken if the server has none stored for
        /// this `user_id`.
        name: String,
    },
    /// 1, S→C. Complete state.
    Welcome {
        /// Negotiated minor, `min(client, server)`.
        minor: u16,
        /// Server capability bits.
        caps: u32,
        /// The client's own entry, including `user_id` and the stored name.
        you: UserInfo,
        /// Audio parameters for this room.
        audio: AudioParams,
        /// All other participants.
        users: Vec<UserInfo>,
        /// Lowest allowed client version, SemVer.
        min_client: String,
        /// Whether `you` may send `CreateInviteRequest`.
        is_admin: bool,
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
    /// 9, C→S. Certificate renewal over the authenticated connection.
    RenewRequest {
        /// PKCS#10 CSR, DER, at most 2 KiB.
        csr_der: Vec<u8>,
    },
    /// 10, S→C.
    RenewResponse {
        /// The freshly issued leaf certificate, DER.
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
    /// 14, C→S. Rename.
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
    /// otherwise. Non fatal either way.
    CreateInviteRequest {
        /// Name the certificate CN will carry.
        name: String,
        /// Lifetime in seconds. `0` means the default (`limits::INVITE_TTL_S`).
        ttl_s: u64,
    },
    /// 18, S→C.
    CreateInviteResponse {
        /// Self-contained ASCII-armored invite, `ICU1-…`.
        blob: String,
        /// When the underlying invite expires, RFC 3339 UTC.
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
    /// 28, S→C. Rejection of variant 19/22/24/26. `user_id` is `0` for a
    /// `UserListRequest` failure, since it names no single target.
    AdminActionError {
        /// The targeted user, or `0`.
        user_id: UserId,
        /// Error code (`PROTOCOL.md` §6).
        code: u32,
        /// Human readable detail.
        detail: String,
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
                | ControlMsg::SetName { .. }
                | ControlMsg::CreateInviteRequest { .. }
                | ControlMsg::AdminSetRole { .. }
                | ControlMsg::AdminSetDisabled { .. }
                | ControlMsg::AdminRevokeUser { .. }
                | ControlMsg::UserListRequest
        )
    }
}

/// Messages on the enrollment stream (ALPN `icu-enroll/1`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnrollMsg {
    /// 0, C→S.
    EnrollRequest {
        /// Invite token, base64url without padding, 22 characters = 128 bit.
        token: String,
        /// PKCS#10 CSR, DER, at most 2 KiB.
        csr_der: Vec<u8>,
        /// Desired display name.
        name: String,
    },
    /// 1, S→C.
    EnrollResponse {
        /// Issued leaf certificate, DER.
        leaf_der: Vec<u8>,
        /// The server's CA certificate, DER.
        ca_der: Vec<u8>,
        /// SNI to use for the main connection.
        server_name: String,
    },
    /// 2, S→C.
    Error {
        /// Error code (`PROTOCOL.md` §6).
        code: u32,
        /// Human readable detail.
        detail: String,
    },
}

impl EnrollMsg {
    /// Length of an invite token in characters (128 bit, base64url, unpadded).
    pub const TOKEN_LEN: usize = 22;

    /// Encodes the message as a complete enrollment frame.
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        framing::encode_frame(self, MAX_ENROLL_FRAME)
    }

    /// Decodes an enrollment frame body.
    pub fn decode(body: &[u8]) -> Result<EnrollMsg, ProtoError> {
        framing::decode_body(body, MAX_ENROLL_FRAME)
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
                blob: "ICU1-abc".into(),
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
                }],
            },
            ControlMsg::AdminActionError {
                user_id: 5,
                code: crate::error::LAST_ADMIN,
                detail: "cannot demote the last admin".into(),
            },
        ]
    }

    /// §10: roundtrip of every `ControlMsg` variant.
    #[test]
    fn roundtrip_all_control_variants() {
        let variants = all_variants();
        assert_eq!(variants.len(), 29, "v1 has exactly 29 control variants");
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
        for (index, msg) in [
            EnrollMsg::EnrollRequest {
                token: "k3nP8sQ2vL7xR1wY4tZ6bA".into(),
                csr_der: vec![1, 2, 3],
                name: "bob".into(),
            },
            EnrollMsg::EnrollResponse {
                leaf_der: vec![1],
                ca_der: vec![2],
                server_name: "icu.example.org".into(),
            },
            EnrollMsg::Error {
                code: crate::error::ENROLL_BAD_TOKEN,
                detail: "unknown token".into(),
            },
        ]
        .into_iter()
        .enumerate()
        {
            let body = postcard::to_stdvec(&msg).unwrap();
            assert_eq!(body[0] as usize, index);
        }
    }

    /// §10: roundtrip of every `EnrollMsg` variant.
    #[test]
    fn roundtrip_all_enroll_variants() {
        for msg in [
            EnrollMsg::EnrollRequest {
                token: "k3nP8sQ2vL7xR1wY4tZ6bA".into(),
                csr_der: vec![0x30; 218],
                name: "bob".into(),
            },
            EnrollMsg::EnrollResponse {
                leaf_der: vec![0x30; 318],
                ca_der: vec![0x30; 366],
                server_name: "icu.example.org".into(),
            },
            EnrollMsg::Error {
                code: crate::error::ENROLL_BAD_CSR,
                detail: "csr not parsable".into(),
            },
        ] {
            let frame = msg.encode().unwrap();
            let body = framing::split_frame(&frame, MAX_ENROLL_FRAME)
                .unwrap()
                .unwrap();
            assert_eq!(EnrollMsg::decode(body).unwrap(), msg);
        }
    }

    /// §10 / Spike 2b: an unknown variant decodes to `Err`, never a panic.
    #[test]
    fn unknown_variant_is_an_error() {
        // Discriminant 29 does not exist in v1.
        let body = [29u8, 0, 0];
        let err = ControlMsg::decode(&body).unwrap_err();
        assert!(
            matches!(err, ProtoError::UnknownVariant),
            "expected UnknownVariant, got {err:?}"
        );
        assert_eq!(err.error_code(), crate::error::PROTOCOL_VIOLATION);

        let err = EnrollMsg::decode(&[9u8, 0]).unwrap_err();
        assert!(matches!(err, ProtoError::UnknownVariant));
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
            blob: String::new(),
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
        assert!(!ControlMsg::UserAdminChanged { is_admin: true }.is_client_to_server());
        assert!(!ControlMsg::UserListResponse { users: vec![] }.is_client_to_server());
    }
}
