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
    /// Outgoing control frames. The writer task drains this.
    pub out: mpsc::UnboundedSender<Vec<u8>>,
    /// Talk spurt tracking.
    pub speak: Speaking,
}

impl Member {
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
    /// Session ids exhausted (2^16 connections in one server run).
    SessionSpaceExhausted,
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

    /// Whether the room is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Adds a participant and returns its session id. Session ids are
    /// monotonic and never reused within a server run.
    pub fn join(
        &self,
        user_id: UserId,
        name: String,
        flags: UserFlags,
        conn: quinn::Connection,
        out: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Result<Session, JoinError> {
        let mut state = self.lock();
        if state.members.len() >= usize::from(self.max_users) {
            return Err(JoinError::Full);
        }
        let session = state.next_session;
        if session == Session::MAX {
            return Err(JoinError::SessionSpaceExhausted);
        }
        state.next_session += 1;
        state.members.insert(
            session,
            Member {
                session,
                user_id,
                name,
                // A client cannot announce itself as speaking.
                flags: UserFlags(flags.0 & UserFlags::CLIENT_SETTABLE),
                conn,
                out,
                speak: Speaking::default(),
            },
        );
        Ok(session)
    }

    /// Removes a participant.
    pub fn leave(&self, session: Session) -> Option<Member> {
        self.lock().members.remove(&session)
    }

    /// One participant, cloned.
    pub fn member(&self, session: Session) -> Option<Member> {
        self.lock().members.get(&session).cloned()
    }

    /// All participants as wire records.
    pub fn users(&self) -> Vec<UserInfo> {
        self.lock().members.values().map(Member::info).collect()
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

    /// Connections to hand an audio datagram to: everybody else who is not
    /// deafened (`PROTOCOL.md` §3).
    pub fn audio_receivers(&self, sender: Session) -> Vec<(Session, quinn::Connection)> {
        self.lock()
            .members
            .values()
            .filter(|m| m.session != sender && !m.flags.deafened())
            .map(|m| (m.session, m.conn.clone()))
            .collect()
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
        let state = self.lock();
        match state.members.get(&session) {
            Some(member) => member.out.send(frame).is_ok(),
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
        let state = self.lock();
        for member in state.members.values() {
            if Some(member.session) == except {
                continue;
            }
            let _ = member.out.send(frame.clone());
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
    /// Enrollment attempts.
    pub enroll_attempts: AtomicU64,
    /// Successful enrollments.
    pub enroll_ok: AtomicU64,
    /// Rejected enrollments.
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
