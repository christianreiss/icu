//! Participant list, session id assignment and broadcast
//! (`Server/IMPLEMENTATION.md` §6.4).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use icu_proto::{ControlMsg, Session, UserFlags, UserId, UserInfo};
use tokio::sync::mpsc;

/// Talk spurt tracking for one sender (`PROTOCOL.md` §2.1).
///
/// The server derives `speaking` from the datagram flow: the first datagram
/// after at least `silence` of quiet turns it on, the `END` flag or `silence`
/// without a datagram turns it off.
#[derive(Debug, Default, Clone, Copy)]
pub struct Speaking {
    /// When the last audio datagram arrived.
    pub last_audio: Option<Instant>,
    /// Whether the sender counts as speaking right now.
    pub speaking: bool,
}

impl Speaking {
    /// Records a datagram. Returns `true` when `speaking` flipped to 1.
    pub fn note_audio(&mut self, now: Instant, silence: Duration) -> bool {
        let quiet_before = self
            .last_audio
            .map(|t| now.saturating_duration_since(t) >= silence)
            .unwrap_or(true);
        self.last_audio = Some(now);
        if self.speaking && !quiet_before {
            return false;
        }
        let flipped = !self.speaking;
        self.speaking = true;
        flipped
    }

    /// Ends the talk spurt. Returns `true` when `speaking` flipped to 0.
    pub fn end(&mut self) -> bool {
        if !self.speaking {
            return false;
        }
        self.speaking = false;
        self.last_audio = None;
        true
    }

    /// Ends the talk spurt when quiet for at least `silence`. Returns `true`
    /// when `speaking` flipped to 0.
    pub fn expire(&mut self, now: Instant, silence: Duration) -> bool {
        if !self.speaking {
            return false;
        }
        let quiet = self
            .last_audio
            .map(|t| now.saturating_duration_since(t) >= silence)
            .unwrap_or(true);
        if quiet {
            self.end();
            return true;
        }
        false
    }
}

/// One connected participant.
#[derive(Debug)]
pub struct Member {
    /// Connection scoped id.
    pub session: Session,
    /// Stable identity.
    pub user_id: UserId,
    /// Display name as the server stores it.
    pub name: String,
    /// Current state bits.
    pub flags: UserFlags,
    /// `MUTED`/`DEAFENED` enforced by an admin (`AdminSetRestriction`): kept
    /// set in `flags` whatever the client requests.
    pub forced: UserFlags,
    /// The QUIC connection, for datagrams and for closing.
    pub conn: quinn::Connection,
    /// Outgoing control frames, each shared with every other recipient of a
    /// broadcast. The writer task drains this. Bounded by
    /// [`crate::limits::CONTROL_QUEUE_FRAMES`]: a client that stops reading
    /// is closed rather than buffered for.
    pub out: mpsc::Sender<Bytes>,
    /// Talk spurt tracking.
    pub speak: Speaking,
    /// When the last `KeyframeRequest` was forwarded to this member as a
    /// speaker (`Room::keyframe_request`).
    pub last_keyframe: Option<Instant>,
    /// Set just before the server closes this connection itself, so that
    /// `UserLeft.reason` carries the code the server closed with
    /// (`Server/IMPLEMENTATION.md` §6.4.6).
    pub close_code: Option<u32>,
}

impl Member {
    /// Queues one control frame. A full queue means the client has stopped
    /// reading its control stream; it is closed with `TIMEOUT` instead of
    /// letting its backlog grow in server memory.
    fn push(&mut self, frame: Bytes) -> bool {
        match self.out.try_send(frame) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                if self.close_code.is_none() {
                    tracing::info!(session = self.session, "control queue full, closing");
                    self.close(icu_proto::error::TIMEOUT);
                }
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Closes the connection with `code`, recorded first so that `UserLeft`
    /// carries it. `quinn::Connection::close` is synchronous and never calls
    /// back into the room, so this is safe under the room lock.
    fn close(&mut self, code: u32) {
        self.close_code = Some(code);
        crate::wire::close(&self.conn, code);
    }

    /// This member as it appears on the wire.
    pub fn info(&self) -> UserInfo {
        UserInfo {
            user_id: self.user_id,
            session: self.session,
            name: self.name.clone(),
            flags: self.flags,
        }
    }
}

#[derive(Debug, Default)]
struct RoomState {
    next_session: Session,
    members: BTreeMap<Session, Member>,
}

/// The one room.
#[derive(Debug)]
pub struct Room {
    state: Mutex<RoomState>,
    max_users: u16,
}

/// Why a join was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinError {
    /// `max_users` reached.
    Full,
}

/// The next free session id from `*next` on, advancing it. Monotonic until
/// `u16` runs out, then it wraps to 1 and skips ids still in use: with at
/// most `max_users` members there is always one free, and an id comes back
/// only after 65 534 others have been handed out (`PROTOCOL.md` §1). Without
/// the wrap, a client reconnecting in a loop exhausted the space in about
/// two hours, and the server then turned everybody away until restarted.
fn allocate_session(next: &mut Session, in_use: impl Fn(Session) -> bool) -> Session {
    loop {
        let session = *next;
        *next = session.checked_add(1).unwrap_or(1);
        if session != 0 && !in_use(session) {
            return session;
        }
    }
}

impl Room {
    /// An empty room of at most `max_users` participants.
    pub fn new(max_users: u16) -> Room {
        Room {
            state: Mutex::new(RoomState {
                // Session ids start at 1; 0 means "unknown/me".
                next_session: 1,
                members: BTreeMap::new(),
            }),
            max_users,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RoomState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of participants.
    pub fn len(&self) -> usize {
        self.lock().members.len()
    }

    /// Whether nobody is in the room. Companion to [`Room::len`].
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Adds a participant and returns its session id (see
    /// [`allocate_session`] for when an id can come back).
    ///
    /// `welcome` builds the joiner's `Welcome` from its session, flags and the
    /// other participants. It is queued, and `UserJoined` broadcast, under the
    /// same lock that inserts the member: otherwise a concurrent broadcast
    /// could reach the joiner before its `Welcome`, which the client treats as
    /// a fatal protocol error, and the user list could miss or double a peer.
    #[allow(clippy::too_many_arguments)]
    pub fn join(
        &self,
        user_id: UserId,
        name: String,
        flags: UserFlags,
        is_admin: bool,
        forced: UserFlags,
        conn: quinn::Connection,
        out: mpsc::Sender<Bytes>,
        welcome: impl FnOnce(Session, UserFlags, Vec<UserInfo>) -> ControlMsg,
    ) -> Result<Session, JoinError> {
        let mut state = self.lock();
        if state.members.len() >= usize::from(self.max_users) {
            return Err(JoinError::Full);
        }
        let state = &mut *state;
        let session = allocate_session(&mut state.next_session, |s| state.members.contains_key(&s));
        // A client cannot announce itself as speaking or admin; the admin
        // bit comes from the store, looked up by the caller before join.
        let mut initial = UserFlags::NONE.apply_client_request(flags);
        initial = UserFlags(initial.with_bit(UserFlags::ADMIN, is_admin).0 | forced.0);
        let mut member = Member {
            session,
            user_id,
            name,
            flags: initial,
            forced,
            conn,
            out,
            speak: Speaking::default(),
            last_keyframe: None,
            close_code: None,
        };
        let users = state.members.values().map(Member::info).collect();
        if let Some(frame) = encode(&welcome(session, initial, users)) {
            member.push(frame);
        }
        let joined = ControlMsg::UserJoined {
            user: member.info(),
        };
        if let Some(frame) = encode(&joined) {
            for other in state.members.values_mut() {
                other.push(frame.clone());
            }
        }
        state.members.insert(session, member);
        Ok(session)
    }

    /// Closes a participant's connection with `code` (`kick_user`). Returns
    /// whether it was in the room.
    pub fn close(&self, session: Session, code: u32) -> bool {
        match self.lock().members.get_mut(&session) {
            Some(member) => {
                member.close(code);
                true
            }
            None => false,
        }
    }

    /// Closes every session of `user_id` with `code`, so that their
    /// `UserLeft` carries it. Returns the `stable_id`s of their connections.
    pub fn close_user(&self, user_id: UserId, code: u32) -> Vec<usize> {
        self.lock()
            .members
            .values_mut()
            .filter(|m| m.user_id == user_id)
            .map(|m| {
                m.close(code);
                m.conn.stable_id()
            })
            .collect()
    }

    /// Closes the connection with this `stable_id` if it is in the room, so
    /// that its `UserLeft` carries `code`. Returns whether it was.
    pub fn close_connection(&self, stable_id: usize, code: u32) -> bool {
        match self
            .lock()
            .members
            .values_mut()
            .find(|m| m.conn.stable_id() == stable_id)
        {
            Some(member) => {
                member.close(code);
                true
            }
            None => false,
        }
    }

    /// Closes every connection with `code`, for shutdown.
    pub fn close_all(&self, code: u32) {
        for member in self.lock().members.values_mut() {
            member.close(code);
        }
    }

    /// Removes a participant. The last one out resets the id counter:
    /// nobody is left who could tell a new session 1 from the old one.
    pub fn leave(&self, session: Session) -> Option<Member> {
        let mut state = self.lock();
        let member = state.members.remove(&session);
        if state.members.is_empty() {
            state.next_session = 1;
        }
        member
    }

    /// All sessions belonging to a `user_id` (a user can hold only one
    /// connection in practice, but nothing enforces it).
    pub fn sessions_of_user(&self, user_id: UserId) -> Vec<Session> {
        self.lock()
            .members
            .values()
            .filter(|m| m.user_id == user_id)
            .map(|m| m.session)
            .collect()
    }

    /// The `user_id` of everybody in the room, for the user lists.
    pub fn online_users(&self) -> BTreeSet<UserId> {
        self.lock().members.values().map(|m| m.user_id).collect()
    }

    /// Everything the audio relay needs from the room, in **one** lock: the
    /// sender's flags, and — unless it is muted — the connections to fan the
    /// datagram out to, which is everybody else who is not deafened
    /// (`PROTOCOL.md` §3), plus the talk spurt bookkeeping (`PROTOCOL.md`
    /// §2.1, `Server/IMPLEMENTATION.md` §6.5): `end` closes the spurt, any
    /// other datagram is noted. The second value is the new flags when
    /// `speaking` flipped, for the `UserState` broadcast. Returns `None` when
    /// the sender has already left.
    ///
    /// `out` is owned by the caller and reused across datagrams, so this, the
    /// only truly hot path in the server, allocates nothing.
    pub fn audio_fanout(
        &self,
        sender: Session,
        end: bool,
        silence: Duration,
        out: &mut Vec<(Session, quinn::Connection)>,
    ) -> Option<(UserFlags, Option<UserFlags>)> {
        out.clear();
        let mut state = self.lock();
        let flags = state.members.get(&sender)?.flags;
        if flags.muted() {
            return Some((flags, None));
        }
        out.extend(
            state
                .members
                .values()
                .filter(|m| m.session != sender && !m.flags.deafened())
                .map(|m| (m.session, m.conn.clone())),
        );
        let member = state.members.get_mut(&sender)?;
        let flipped = if end {
            member.speak.end()
        } else {
            member.speak.note_audio(Instant::now(), silence)
        };
        let flipped = flipped.then(|| {
            member.flags = member.flags.with_speaking(!end);
            member.flags
        });
        Some((flags, flipped))
    }

    /// Connections to hand a video frame to: everybody else who is viewing
    /// (`PROTOCOL.md` §4). A user's own deafen only concerns audio; one an
    /// admin enforces cuts video as well (`PROTOCOL.md` §2.2 Nr. 33).
    pub fn video_receivers(&self, sender: Session) -> Vec<(Session, quinn::Connection)> {
        self.lock()
            .members
            .values()
            .filter(|m| m.session != sender && m.flags.viewing() && !m.forced.deafened())
            .map(|m| (m.session, m.conn.clone()))
            .collect()
    }

    /// Applies a client requested state change. Returns the new flags when
    /// something actually changed.
    pub fn apply_client_state(&self, session: Session, requested: UserFlags) -> Option<UserFlags> {
        let mut state = self.lock();
        let member = state.members.get_mut(&session)?;
        let new = UserFlags(member.flags.apply_client_request(requested).0 | member.forced.0);
        if new == member.flags {
            return None;
        }
        member.flags = new;
        Some(new)
    }

    /// Current flags of a participant.
    pub fn flags(&self, session: Session) -> Option<UserFlags> {
        self.lock().members.get(&session).map(|m| m.flags)
    }

    /// Renames a participant.
    pub fn rename(&self, session: Session, name: &str) {
        if let Some(member) = self.lock().members.get_mut(&session) {
            member.name = name.to_string();
        }
    }

    /// Sets or clears `ADMIN` on every live session of `user_id`. Returns the
    /// sessions whose flags actually changed, so the caller can broadcast
    /// `UserState` for each — a user can in principle hold more than one
    /// session (`sessions_of_user`), and every one of them must show the same
    /// badge to everybody else.
    pub fn set_admin_flag(&self, user_id: UserId, is_admin: bool) -> Vec<Session> {
        let mut state = self.lock();
        let mut changed = Vec::new();
        for member in state.members.values_mut() {
            if member.user_id != user_id {
                continue;
            }
            let new = member.flags.with_bit(UserFlags::ADMIN, is_admin);
            if new != member.flags {
                member.flags = new;
                changed.push(member.session);
            }
        }
        changed
    }

    /// Sets the admin-enforced `MUTED`/`DEAFENED` bits on every live session
    /// of `user_id`. Enforcing sets the bits at once; lifting leaves them as
    /// they are, the user unmutes themself. Returns each session whose flags
    /// changed with its new flags, for the `UserState` broadcast.
    pub fn set_forced(&self, user_id: UserId, forced: UserFlags) -> Vec<(Session, UserFlags)> {
        let mut state = self.lock();
        let mut changed = Vec::new();
        for member in state.members.values_mut() {
            if member.user_id != user_id {
                continue;
            }
            member.forced = forced;
            let new = UserFlags(member.flags.0 | forced.0);
            if new != member.flags {
                member.flags = new;
                changed.push((member.session, new));
            }
        }
        changed
    }

    /// One `(session, rtt_ms)` pair per member, from the server's own QUIC
    /// path RTT measurement — not a client self-reported value
    /// (`PROTOCOL.md` §2.2 `UserRtt`). Empty when fewer than two members are
    /// present: a lone participant has nobody to show a ping to.
    pub fn rtt_snapshot(&self) -> Vec<(Session, u16)> {
        let state = self.lock();
        if state.members.len() < 2 {
            return Vec::new();
        }
        state
            .members
            .values()
            .map(|m| {
                let ms = m
                    .conn
                    .stats()
                    .path
                    .rtt
                    .as_millis()
                    .min(u128::from(u16::MAX));
                (m.session, ms as u16)
            })
            .collect()
    }

    /// Clears `speaking` for everybody silent for longer than `silence`.
    pub fn expire_speaking(&self, silence: Duration) -> Vec<(Session, UserFlags)> {
        let now = Instant::now();
        let mut state = self.lock();
        let mut changed = Vec::new();
        for member in state.members.values_mut() {
            if member.speak.expire(now, silence) {
                member.flags = member.flags.with_speaking(false);
                changed.push((member.session, member.flags));
            }
        }
        changed
    }

    /// Forwards a `KeyframeRequest` to `speaker`, at most once per
    /// `min_interval` (§6.5). The speaker learns "someone needs a keyframe",
    /// not who asked.
    pub fn keyframe_request(&self, speaker: Session, min_interval: Duration) -> bool {
        let now = Instant::now();
        let mut state = self.lock();
        let Some(member) = state.members.get_mut(&speaker) else {
            return false;
        };
        if member
            .last_keyframe
            .is_some_and(|last| now.saturating_duration_since(last) < min_interval)
        {
            return false;
        }
        member.last_keyframe = Some(now);
        encode(&ControlMsg::KeyframeRequest { speaker: 0 }).is_some_and(|frame| member.push(frame))
    }

    /// Sends a control message to one participant.
    pub fn send_to(&self, session: Session, msg: &ControlMsg) -> bool {
        let Some(frame) = encode(msg) else {
            return false;
        };
        let mut state = self.lock();
        match state.members.get_mut(&session) {
            Some(member) => member.push(frame),
            None => false,
        }
    }

    /// Sends a control message to everybody, including the originator. One
    /// encoding, shared by refcount with every queue.
    pub fn broadcast(&self, msg: &ControlMsg) {
        let Some(frame) = encode(msg) else {
            return;
        };
        let mut state = self.lock();
        for member in state.members.values_mut() {
            member.push(frame.clone());
        }
    }
}

/// One control frame, logged and `None` when it cannot be encoded.
fn encode(msg: &ControlMsg) -> Option<Bytes> {
    match msg.encode() {
        Ok(frame) => Some(Bytes::from(frame)),
        Err(_) => {
            tracing::error!(msg = msg.variant(), "cannot encode control message");
            None
        }
    }
}

/// Relay and connection counters, exposed through `status`
/// (`Server/IMPLEMENTATION.md` §7).
#[derive(Debug, Default)]
pub struct Counters {
    /// Accepted connections, total.
    pub connections: AtomicU64,
    /// Audio datagrams received from clients.
    pub datagrams_in: AtomicU64,
    /// Audio datagrams handed to receivers.
    pub datagrams_out: AtomicU64,
    /// Dropped: malformed, wrong kind or bad length.
    pub dropped_malformed: AtomicU64,
    /// Dropped: sender over its datagram rate.
    pub dropped_rate: AtomicU64,
    /// Dropped: sender muted.
    pub dropped_muted: AtomicU64,
    /// Send failed: datagram larger than the path allows.
    pub send_too_large: AtomicU64,
    /// Send failed: peer does not support datagrams.
    pub send_unsupported: AtomicU64,
    /// Send failed: datagrams disabled locally.
    pub send_disabled: AtomicU64,
    /// Send failed: connection lost.
    pub send_connection_lost: AtomicU64,
    /// Video frames received.
    pub video_frames_in: AtomicU64,
    /// Video frames forwarded (counted per receiver).
    pub video_frames_out: AtomicU64,
    /// Video frames drained because nobody was listening.
    pub video_dropped_no_receiver: AtomicU64,
    /// Video frames dropped by the frame rate or bitrate limit.
    pub video_dropped_rate: AtomicU64,
    /// Video frames dropped because the receiver was still busy.
    pub video_dropped_slow_receiver: AtomicU64,
    /// Video frames dropped because the sender's camera is off or it is not
    /// active.
    pub video_dropped_not_allowed: AtomicU64,
    /// Video frames whose stream stalled past `timing.video_frame_timeout_ms`.
    pub video_dropped_stalled: AtomicU64,
    /// Control messages received.
    pub control_in: AtomicU64,
    /// `Error(RATE_LIMITED)` sent.
    pub control_rate_limited: AtomicU64,
    /// Registration sessions.
    pub enroll_attempts: AtomicU64,
    /// Completed registrations.
    pub enroll_ok: AtomicU64,
    /// Rejected registration attempts.
    pub enroll_failed: AtomicU64,
}

impl Counters {
    /// Increments a counter by one.
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Adds `n` to a counter; nothing is touched for 0, so a fan-out can
    /// count locally and settle each counter once.
    pub fn add(counter: &AtomicU64, n: u64) {
        if n > 0 {
            counter.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Reads a counter.
    pub fn get(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §10 / `PROTOCOL.md` §2.1: the server derives `speaking` from the
    /// datagram flow. A `quinn::Connection` has no public constructor, so the
    /// room itself is covered by the integration tests; the decision logic is
    /// tested here.
    #[test]
    fn speaking_flips_on_the_first_datagram_and_off_on_end() {
        let silence = Duration::from_millis(500);
        let t0 = Instant::now();
        let mut s = Speaking::default();

        assert!(s.note_audio(t0, silence), "first datagram starts a spurt");
        assert!(s.speaking);
        assert!(
            !s.note_audio(t0 + Duration::from_millis(20), silence),
            "no broadcast while already speaking"
        );
        assert!(s.end(), "END flag ends the spurt");
        assert!(!s.speaking);
        assert!(!s.end(), "a second END changes nothing");
    }

    #[test]
    fn a_gap_of_500_ms_starts_a_new_spurt() {
        let silence = Duration::from_millis(500);
        let t0 = Instant::now();
        let mut s = Speaking::default();
        s.note_audio(t0, silence);
        // Still inside the window: no flip, because it never went off.
        assert!(!s.note_audio(t0 + Duration::from_millis(499), silence));
        // The server never saw an END, so the flag is still 1 and a late
        // datagram does not re-announce it.
        assert!(!s.note_audio(t0 + Duration::from_millis(1500), silence));

        // After an expiry, the next datagram does announce a new spurt.
        assert!(s.expire(t0 + Duration::from_millis(2100), silence));
        assert!(s.note_audio(t0 + Duration::from_millis(2200), silence));
    }

    #[test]
    fn session_ids_wrap_and_skip_ids_in_use() {
        let free = |_| false;
        let mut next = Session::MAX;
        assert_eq!(allocate_session(&mut next, free), Session::MAX);
        assert_eq!(allocate_session(&mut next, free), 1, "wraps past 0");
        // 2 is still connected: skipped, never handed out twice.
        assert_eq!(allocate_session(&mut next, |s| s == 2), 3);
    }

    #[test]
    fn speaking_expires_after_500_ms_of_silence() {
        let silence = Duration::from_millis(500);
        let t0 = Instant::now();
        let mut s = Speaking::default();
        s.note_audio(t0, silence);
        assert!(!s.expire(t0 + Duration::from_millis(400), silence));
        assert!(s.expire(t0 + Duration::from_millis(500), silence));
        assert!(!s.speaking);
        assert!(
            !s.expire(t0 + Duration::from_secs(10), silence),
            "idempotent"
        );
    }
}
