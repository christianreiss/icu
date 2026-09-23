//! Participant list, session id assignment and broadcast
//! (`Server/IMPLEMENTATION.md` §6.4).

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

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
#[derive(Debug, Clone)]
pub struct Member {
    /// Connection scoped id.
    pub session: Session,
    /// Stable identity.
    pub user_id: UserId,
    /// Display name as the server stores it.
    pub name: String,
    /// Current state bits.
    pub flags: UserFlags,
    /// The QUIC connection, for datagrams and for closing.
    pub conn: quinn::Connection,
    /// Outgoing control frames. The writer task drains this. Bounded by
    /// [`crate::limits::CONTROL_QUEUE_FRAMES`]: a client that stops reading
    /// is closed rather than buffered for.
    pub out: mpsc::Sender<Vec<u8>>,
    /// Talk spurt tracking.
    pub speak: Speaking,
    /// Set just before the server closes this connection itself, so that
    /// `UserLeft.reason` carries the code the server closed with
    /// (`Server/IMPLEMENTATION.md` §6.4.6).
    pub close_code: Option<u32>,
}

impl Member {
    /// Queues one control frame. A full queue means the client has stopped
    /// reading its control stream; it is closed with `TIMEOUT` instead of
    /// letting its backlog grow in server memory. `quinn::Connection::close`
    /// is synchronous and never calls back into the room, so this is safe
    /// under the room lock.
    fn push(&mut self, frame: Vec<u8>) -> bool {
        match self.out.try_send(frame) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                if self.close_code.is_none() {
                    tracing::info!(session = self.session, "control queue full, closing");
                    self.close_code = Some(icu_proto::error::TIMEOUT);
                    let code = icu_proto::error::TIMEOUT;
                    self.conn.close(
                        quinn::VarInt::from_u32(code),
                        icu_proto::error::name(code).as_bytes(),
                    );
                }
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
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
        conn: quinn::Connection,
        out: mpsc::Sender<Vec<u8>>,
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
        initial = initial.with_bit(UserFlags::ADMIN, is_admin);
        let mut member = Member {
            session,
            user_id,
            name,
            flags: initial,
            conn,
            out,
            speak: Speaking::default(),
            close_code: None,
        };
        let users = state.members.values().map(Member::info).collect();
        let welcome = welcome(session, initial, users);
        match welcome.encode() {
            Ok(frame) => {
                member.push(frame);
            }
            Err(_) => tracing::error!(msg = welcome.variant(), "cannot encode control message"),
        }
        let joined = ControlMsg::UserJoined {
            user: member.info(),
        };
        match joined.encode() {
            Ok(frame) => {
                for other in state.members.values_mut() {
                    other.push(frame.clone());
                }
            }
            Err(_) => tracing::error!("cannot encode UserJoined"),
        }
        state.members.insert(session, member);
        Ok(session)
    }

    /// Records the code the server is about to close this connection with.
    pub fn mark_closing(&self, session: Session, code: u32) {
        if let Some(member) = self.lock().members.get_mut(&session) {
            member.close_code = Some(code);
        }
    }

    /// Records the close code for every participant, for shutdown.
    pub fn mark_all_closing(&self, code: u32) {
        for member in self.lock().members.values_mut() {
            member.close_code = Some(code);
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

    /// One participant, cloned.
    pub fn member(&self, session: Session) -> Option<Member> {
        self.lock().members.get(&session).cloned()
    }

    /// All participants except `session`.
    pub fn users_except(&self, session: Session) -> Vec<UserInfo> {
        self.lock()
            .members
            .values()
            .filter(|m| m.session != session)
            .map(Member::info)
            .collect()
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

    /// Everything the audio relay needs from the room, in **one** lock: the
    /// sender's flags, and — unless it is muted — the connections to fan the
    /// datagram out to, which is everybody else who is not deafened
    /// (`PROTOCOL.md` §3). Returns `None` when the sender has already left.
    ///
    /// `out` is owned by the caller and reused across datagrams, so this, the
    /// only truly hot path in the server, allocates nothing.
    pub fn audio_fanout(
        &self,
        sender: Session,
        out: &mut Vec<(Session, quinn::Connection)>,
    ) -> Option<UserFlags> {
        out.clear();
        let state = self.lock();
        let flags = state.members.get(&sender)?.flags;
        if !flags.muted() {
            out.extend(
                state
                    .members
                    .values()
                    .filter(|m| m.session != sender && !m.flags.deafened())
                    .map(|m| (m.session, m.conn.clone())),
            );
        }
        Some(flags)
    }

    /// Connections to hand a video frame to: everybody else. Deafened only
    /// concerns audio.
    pub fn video_receivers(&self, sender: Session) -> Vec<(Session, quinn::Connection)> {
        self.lock()
            .members
            .values()
            .filter(|m| m.session != sender)
            .map(|m| (m.session, m.conn.clone()))
            .collect()
    }

    /// Applies a client requested state change. Returns the new flags when
    /// something actually changed.
    pub fn apply_client_state(&self, session: Session, requested: UserFlags) -> Option<UserFlags> {
        let mut state = self.lock();
        let member = state.members.get_mut(&session)?;
        let new = member.flags.apply_client_request(requested);
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
                let ms = m.conn.stats().path.rtt.as_millis().min(u128::from(u16::MAX));
                (m.session, ms as u16)
            })
            .collect()
    }

    /// Records an incoming audio datagram and derives the `speaking` bit
    /// (`PROTOCOL.md` §2.1, `Server/IMPLEMENTATION.md` §6.5).
    ///
    /// Returns the new flags when `speaking` flipped to 1.
    pub fn note_audio(&self, session: Session, silence: Duration) -> Option<UserFlags> {
        let now = Instant::now();
        let mut state = self.lock();
        let member = state.members.get_mut(&session)?;
        if !member.speak.note_audio(now, silence) {
            return None;
        }
        member.flags = member.flags.with_speaking(true);
        Some(member.flags)
    }

    /// Ends a talk spurt explicitly (the `END` flag). Returns the new flags
    /// when `speaking` flipped to 0.
    pub fn end_talk_spurt(&self, session: Session) -> Option<UserFlags> {
        let mut state = self.lock();
        let member = state.members.get_mut(&session)?;
        if !member.speak.end() {
            return None;
        }
        member.flags = member.flags.with_speaking(false);
        Some(member.flags)
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

    /// Sends a control message to one participant.
    pub fn send_to(&self, session: Session, msg: &ControlMsg) -> bool {
        let Ok(frame) = msg.encode() else {
            tracing::error!(msg = msg.variant(), "cannot encode control message");
            return false;
        };
        let mut state = self.lock();
        match state.members.get_mut(&session) {
            Some(member) => member.push(frame),
            None => false,
        }
    }

    /// Sends a control message to everybody, including the originator.
    pub fn broadcast(&self, msg: &ControlMsg) {
        self.broadcast_inner(msg, None)
    }

    /// Sends a control message to everybody but `except`.
    pub fn broadcast_except(&self, except: Session, msg: &ControlMsg) {
        self.broadcast_inner(msg, Some(except))
    }

    fn broadcast_inner(&self, msg: &ControlMsg, except: Option<Session>) {
        let Ok(frame) = msg.encode() else {
            tracing::error!(msg = msg.variant(), "cannot encode control message");
            return;
        };
        let mut state = self.lock();
        for member in state.members.values_mut() {
            if Some(member.session) == except {
                continue;
            }
            member.push(frame.clone());
        }
    }

    /// All connections, for shutdown.
    pub fn connections(&self) -> Vec<quinn::Connection> {
        self.lock()
            .members
            .values()
            .map(|m| m.conn.clone())
            .collect()
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
