//! Control messages (`docs/reference/protocol.md` §2.2, §2.3).
//!
//! Variant order is normative: postcard encodes the discriminant as the varint
//! of the variant index. New variants go at the end, new fields at the end of a
//! variant, each behind a capability bit.

use serde::{Deserialize, Serialize};

use crate::{
    framing::{self, MAX_CONTROL_FRAME},
    AudioParams, ProtoError, Rgb, Session, UserFlags, UserId, UserInfo, UserSummary,
};

/// Protocol minor version of this implementation: the client sends it in
/// `Hello.minor`, the server in `Welcome.server_minor` (`docs/reference/protocol.md` §8).
/// A minor 0 does not exist. New mandatory variants (like `UserRtt`) ship
/// without a minor bump: client and server are always deployed in lockstep in
/// this project (`docs/reference/protocol.md` §2.2). Minor 2 appended
/// `Welcome.server_minor` (2026-09-26). Minor 3 adds peer certificate lookup
/// and requires a coordinated client/server update. Minor 4 adds video
/// display reports and likewise requires a coordinated update. Minor 5 adds
/// each user's own corona color (`UserInfo.corona_color`, `SetCoronaColor`,
/// `UserCoronaColor`); `UserInfo` sits inside `Welcome`, so again a
/// coordinated update. Minor 6 attributes audio and feedback to applied gain
/// generations and extends the audio header to 14 bytes; both ends update together.
/// Minor 7 makes feedback policy 2 mandatory: short speech-only RMS windows,
/// raw target error, independently consumed reporter windows and matched hints.
/// Minor 8 adds shared capture time and clock epochs to both media headers.
/// Minor 9 adds optional administrative status and single-flag moderation.
pub const MINOR: u16 = 9;

/// Lowest protocol minor this build talks to, in both directions: the server
/// closes a `Hello` below it with `VERSION_UNSUPPORTED`, the client a
/// `Welcome` whose `server_minor` is below it (`docs/reference/protocol.md` §8).
pub const MIN_PEER_MINOR: u16 = 8;

/// Peer-assisted audio measurements and hints (bit 0).
pub const CAP_AUDIO_FEEDBACK: u32 = 1;
/// Capabilities supported by this implementation.
pub const CAPS: u32 = CAP_AUDIO_FEEDBACK;

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
        /// (`docs/reference/protocol.md` §2.1); the server ignores this field. Kept because
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
        /// certificate carrying `you.name` (`docs/reference/protocol.md` §2.2).
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
        /// (`docs/reference/protocol.md` §2.3). `users` is then empty, the session is not in
        /// the room, and the only message the server accepts besides `Ping` is
        /// a `RenewRequest` with a fresh key.
        registration: bool,
        /// The server's own protocol minor ([`MINOR`]), since minor 2. The
        /// client closes with `VERSION_UNSUPPORTED` below its
        /// [`MIN_PEER_MINOR`].
        server_minor: u16,
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
        /// Error code < 256 (`docs/reference/protocol.md` §6).
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
        /// Error code (`docs/reference/protocol.md` §6).
        code: u32,
        /// Human readable detail.
        detail: String,
    },
    /// 9, C→S. Certificate renewal over the authenticated connection, and the
    /// only way to change a display name: the CSR's subject CN is the name the
    /// client asks for (`docs/reference/protocol.md` §2.2).
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
    /// 12, S→C. The largest loss more than a third of this speaker's
    /// listeners reported at least; up to two listeners the maximum.
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
    /// 16, S→C. Video-drop rate more than a third of this sender's receivers
    /// agree on, measured over the last 2 s; the rule of `LossHint`.
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
        /// Error code (`docs/reference/protocol.md` §6).
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
    /// (`docs/reference/protocol.md` §1, §2.2). Sent right after `Welcome` and to everyone
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
    /// 36, C→S. Pre-playback speech RMS; at most every two seconds, capability bit 0.
    AudioLevelReport {
        /// Current speaker session.
        speaker: Session,
        /// Measurement before receiver processing.
        level: crate::AudioLevel,
        /// Gain generation and independent sender-clock measurement window.
        window: crate::AudioWindow,
    },
    /// 37, S→C. None means expired/conflicting/insufficient reports.
    AudioLevelHint {
        /// Consensus correction in centi-dB, absent when insufficient/conflicting.
        correction_cdb: Option<i16>,
        /// Number of fresh eligible reporters.
        reporters: u16,
        /// Gain generation and independent sender-clock measurement window.
        window: crate::AudioWindow,
        /// Reporters within the two-dB consensus interval (not all reporters).
        agreeing: u16,
        /// Minimum suitable speech duration among agreeing reporters.
        speech_ms: u16,
    },
    /// 38, C→S. Public certificate of an online room session, on demand.
    PeerCertificateRequest {
        /// Exact online session whose public leaf is requested.
        session: Session,
    },
    /// 39, S→C. The leaf used to authenticate that session; `None` if gone
    /// or unavailable. Never contains a private key or a registration leaf.
    PeerCertificateResponse {
        /// Echoes the requested session.
        session: Session,
        /// Public leaf DER; at most `MAX_CERTIFICATE_DER` bytes.
        certificate_der: Option<Vec<u8>>,
    },
    /// 40, C→S. A fresh frame from `speaker` was presented on this client.
    VideoDisplayed {
        /// Exact session whose frame was presented.
        speaker: Session,
    },
    /// 41, S→C. The authenticated `viewer` presented a frame from us.
    VideoViewer {
        /// Authenticated reporting session.
        viewer: Session,
    },
    /// 42, C→S. The sender's own corona color, stored for their user and
    /// shown for them on every client; `None` = back to the default.
    SetCoronaColor {
        /// The chosen color.
        color: Option<Rgb>,
    },
    /// 43, S→C. A user's corona color changed, sent once per session of
    /// that user (also to the user's own sessions).
    UserCoronaColor {
        /// Whose color.
        session: Session,
        /// The new color; `None` = default.
        color: Option<Rgb>,
    },
    /// 44, C→S. Request the server's administrative status snapshot.
    AdminStatusRequest,
    /// 45, S→C. Same JSON object as the local status command.
    AdminStatusResponse {
        /// Encoded bounded status object.
        json: String,
    },
    /// 46, C→S. Change one restriction under the store lock.
    AdminChangeRestriction {
        /// Target account.
        user_id: UserId,
        /// Select deafen (true) or mute (false).
        deafen: bool,
        /// New value of the selected restriction.
        enabled: bool,
    },
}

impl ControlMsg {
    /// Maximum length of `Hello.client` in bytes (`docs/reference/protocol.md` §2.2).
    pub const MAX_CLIENT_STRING: usize = 64;
    /// Maximum CSR size in bytes (`docs/reference/protocol.md` §2.2, §2.3).
    pub const MAX_CSR_DER: usize = 2 * 1024;

    /// Certificate payload bound, leaving room in the 16 KiB control frame.
    pub const MAX_CERTIFICATE_DER: usize = 8 * 1024;

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
            Self::AdminStatusRequest => "AdminStatusRequest",
            Self::AdminStatusResponse { .. } => "AdminStatusResponse",
            Self::AdminChangeRestriction { .. } => "AdminChangeRestriction",
            ControlMsg::AudioLevelReport { .. } => "AudioLevelReport",
            ControlMsg::AudioLevelHint { .. } => "AudioLevelHint",
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
            ControlMsg::PeerCertificateRequest { .. } => "PeerCertificateRequest",
            ControlMsg::PeerCertificateResponse { .. } => "PeerCertificateResponse",
            ControlMsg::VideoDisplayed { .. } => "VideoDisplayed",
            ControlMsg::VideoViewer { .. } => "VideoViewer",
            ControlMsg::SetCoronaColor { .. } => "SetCoronaColor",
            ControlMsg::UserCoronaColor { .. } => "UserCoronaColor",
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
                | ControlMsg::AudioLevelReport { .. }
                | ControlMsg::LossReport { .. }
                | ControlMsg::KeyframeRequest { .. }
                | ControlMsg::CreateInviteRequest { .. }
                | ControlMsg::AdminSetRole { .. }
                | ControlMsg::AdminSetDisabled { .. }
                | ControlMsg::AdminRevokeUser { .. }
                | ControlMsg::PeerCertificateRequest { .. }
                | ControlMsg::VideoDisplayed { .. }
                | Self::AdminStatusRequest
                | Self::AdminChangeRestriction { .. }
                | ControlMsg::UserListRequest
                | ControlMsg::AdminKick { .. }
                | ControlMsg::AdminSetRestriction { .. }
                | ControlMsg::SetCoronaColor { .. }
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
            corona_color: Some([255, 105, 180]),
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
                server_minor: MINOR,
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
            ControlMsg::AudioLevelReport {
                window: crate::AudioWindow {
                    gain_epoch: 1,
                    first_ts: 0,
                    end_ts: 96_000,
                    report_id: 1,
                },
                speaker: 2,
                level: crate::AudioLevel {
                    level_cdb: -2600,
                    correction_cdb: 200,
                    peak_cdb: -600,
                    near_full_permille: 0,
                    speech_ms: 1200,
                },
            },
            ControlMsg::AudioLevelHint {
                window: crate::AudioWindow {
                    gain_epoch: 1,
                    first_ts: 0,
                    end_ts: 96_000,
                    report_id: 1,
                },
                correction_cdb: Some(200),
                reporters: 2,
                agreeing: 2,
                speech_ms: 1500,
            },
            ControlMsg::PeerCertificateRequest { session: 7 },
            ControlMsg::PeerCertificateResponse {
                session: 7,
                certificate_der: Some(vec![1, 2, 3]),
            },
            ControlMsg::VideoDisplayed { speaker: 7 },
            ControlMsg::VideoViewer { viewer: 8 },
            ControlMsg::SetCoronaColor {
                color: Some([255, 105, 180]),
            },
            ControlMsg::UserCoronaColor {
                session: 7,
                color: None,
            },
            ControlMsg::AdminStatusRequest,
            ControlMsg::AdminStatusResponse { json: "{}".into() },
            ControlMsg::AdminChangeRestriction {
                user_id: 3,
                deafen: true,
                enabled: false,
            },
        ]
    }

    /// §10: roundtrip of every `ControlMsg` variant.
    #[test]
    fn roundtrip_all_control_variants() {
        let variants = all_variants();
        assert_eq!(variants.len(), 47, "47 append-only control variants");
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
    /// `docs/reference/protocol.md` §2.2 is therefore normative wire format.
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
        // Discriminant 47 does not exist yet.
        let body = [47u8, 0, 0];
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

    /// postcard ignores trailing bytes: a minor-1 reader decodes a minor-2
    /// `Welcome` without seeing `server_minor`, so the server, not the old
    /// client, has to turn it away (`Hello.minor` below `MIN_PEER_MINOR`).
    /// The other way round, a minor-1 `Welcome` is too short for this build.
    #[test]
    fn welcome_server_minor_is_appended() {
        let welcome = all_variants().remove(1);
        let body = postcard::to_stdvec(&welcome).unwrap();
        assert_eq!(*body.last().unwrap(), MINOR as u8);
        assert!(ControlMsg::decode(&body[..body.len() - 1]).is_err());
        let mut longer = body.clone();
        longer.push(7);
        assert_eq!(ControlMsg::decode(&longer).unwrap(), welcome);
    }

    #[test]
    fn direction_table() {
        assert!(ControlMsg::Ping { nonce: 1 }.is_client_to_server());
        assert!(ControlMsg::SetCoronaColor { color: None }.is_client_to_server());
        assert!(!ControlMsg::UserCoronaColor {
            session: 1,
            color: None
        }
        .is_client_to_server());
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

#[cfg(test)]
mod certificate_tests {
    use super::*;
    #[test]
    fn certificate_frames_are_bounded_and_directional() {
        assert!(ControlMsg::PeerCertificateRequest { session: 7 }.is_client_to_server());
        for certificate_der in [None, Some(vec![0; ControlMsg::MAX_CERTIFICATE_DER])] {
            let msg = ControlMsg::PeerCertificateResponse {
                session: 7,
                certificate_der,
            };
            assert!(!msg.is_client_to_server());
            let frame = msg.encode().unwrap();
            assert!(frame.len() <= MAX_CONTROL_FRAME + 2);
            assert_eq!(ControlMsg::decode(&frame[2..]).unwrap(), msg);
        }
    }
}
