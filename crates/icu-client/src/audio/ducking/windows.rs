//! WASAPI session ducking, exclusively on the guardian's MTA thread.
//!
//! Session master volume persists across process exits. Every native write has
//! a durable compare-and-restore record; losing an endpoint never deletes it.
//! No endpoint gain, mute, channel gain or foreign application's duck preference
//! is changed. The session peak meter supplies audibility, never RMS/LUFS.
use super::{same, Background, Gain};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{implement, Interface, BOOL, GUID, HSTRING, PCWSTR, PWSTR},
    Win32::{
        Foundation::CloseHandle,
        Media::Audio::{
            eConsole, eRender, AudioSessionDisconnectReason, AudioSessionState,
            AudioSessionStateActive, Endpoints::IAudioMeterInformation, IAudioSessionControl2,
            IAudioSessionEvents, IAudioSessionEvents_Impl, IAudioSessionManager2, IMMDevice,
            IMMDeviceEnumerator, ISimpleAudioVolume, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
        },
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
                COINIT_MULTITHREADED,
            },
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
};

const CONTEXT: GUID = GUID::from_u128(0x20b914e6_ae73_443a_a63c_5b53d248817d);
const MAX_SESSIONS: usize = 128;
const MAX_JOURNAL: u64 = 256 * 1024;

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    route: String,
    session: String,
    instance: String,
    gain: Gain,
    /// Both sides of an interrupted write are valid recovery observations.
    pending: bool,
}
impl Record {
    fn key(&self) -> String {
        key(&self.route, &self.instance)
    }
    fn valid(&self) -> bool {
        [&self.route, &self.session, &self.instance]
            .into_iter()
            .all(|s| !s.is_empty() && s.len() <= 4096 && !s.contains('\0'))
            && [&self.gain.baseline, &self.gain.last, &self.gain.previous]
                .into_iter()
                .all(|v| v.len() == 1 && v[0].is_finite() && (0.0..=1.0).contains(&v[0]))
    }
    fn owns(&self, value: f32) -> bool {
        self.gain.owns(&[value])
            || (!self.gain.yielded && self.pending && same(&[value], &self.gain.previous))
    }
    fn stage(&mut self, current: f32, next: Vec<f32>) {
        self.gain.written(next);
        // A failed previous attempt may never have reached its target. Preserve
        // the current readback, including across repeated failed restorations.
        self.gain.previous = vec![current];
        self.pending = true;
    }
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    records: Vec<Record>,
}
fn key(route: &str, instance: &str) -> String {
    format!("{route}\0{instance}")
}

fn load(path: &std::path::Path) -> Result<HashMap<String, Record>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e.to_string()),
    };
    if metadata.len() > MAX_JOURNAL {
        return Err("Ducking-Wiederherstellung: Journal zu groß".into());
    }
    let journal: Journal = serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|_| {
            "Ducking-Wiederherstellung: Journal beschädigt; keine neuen Pegeländerungen".to_string()
        })?;
    if journal.version != 1
        || journal.records.len() > MAX_SESSIONS
        || journal.records.iter().any(|r| !r.valid())
    {
        return Err("Ducking-Wiederherstellung: ungültiges Journal".into());
    }
    let count = journal.records.len();
    let records: HashMap<_, _> = journal.records.into_iter().map(|r| (r.key(), r)).collect();
    if records.len() != count {
        return Err("Ducking-Wiederherstellung: doppelte Sitzungskennung".into());
    }
    Ok(records)
}

#[derive(Default)]
struct Changes {
    manual: AtomicBool,
    volume: AtomicU32,
}
impl Changes {
    fn volume(&self, value: f32, own: bool) {
        let previous = f32::from_bits(self.volume.swap(value.to_bits(), Ordering::AcqRel));
        // A mute-only event has the same volume. A foreign volume change stays
        // latched even if the user subsequently returns to our last value.
        if !own && !same(&[previous], &[value]) {
            self.manual.store(true, Ordering::Release);
        }
    }
}
#[implement(IAudioSessionEvents)]
struct Events {
    changes: Arc<Changes>,
}
#[allow(non_snake_case)]
impl IAudioSessionEvents_Impl for Events_Impl {
    fn OnSimpleVolumeChanged(
        &self,
        value: f32,
        _: BOOL,
        context: *const GUID,
    ) -> windows::core::Result<()> {
        let own = !context.is_null() && unsafe { *context == CONTEXT };
        self.changes.volume(value, own);
        Ok(())
    }
    fn OnDisplayNameChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnIconPathChanged(&self, _: &PCWSTR, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnChannelVolumeChanged(
        &self,
        _: u32,
        _: *const f32,
        _: u32,
        _: *const GUID,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnGroupingParamChanged(&self, _: *const GUID, _: *const GUID) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnStateChanged(&self, _: AudioSessionState) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnSessionDisconnected(&self, _: AudioSessionDisconnectReason) -> windows::core::Result<()> {
        Ok(())
    }
}
struct Live {
    route: String,
    session: String,
    instance: String,
    control: IAudioSessionControl2,
    volume: ISimpleAudioVolume,
    meter: Option<IAudioMeterInformation>,
    events: IAudioSessionEvents,
    changes: Arc<Changes>,
    active: bool,
}
impl Drop for Live {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .control
                .UnregisterAudioSessionNotification(&self.events);
        }
    }
}

pub struct Native {
    enumerator: IMMDeviceEnumerator,
    live: HashMap<String, Live>,
    records: HashMap<String, Record>,
    recovering: HashSet<String>,
    path: PathBuf,
    last_saved: RefCell<Vec<u8>>,
    route: Option<String>,
    last_scan: Option<Instant>,
    own_exe: String,
    // Declared last so all COM references are released before CoUninitialize.
    _com: Com,
}
impl Native {
    pub fn new() -> Result<Self, String> {
        Self::with_journal(super::worker::state_dir()?.join("windows-sessions-v1.json"))
    }
    fn with_journal(path: PathBuf) -> Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|e| e.to_string())?;
        let com = Com;
        let enumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
            .map_err(|e| e.to_string())?;
        let records = load(&path)?;
        let recovering = records.keys().cloned().collect();
        Ok(Self {
            enumerator,
            live: HashMap::new(),
            records,
            recovering,
            path,
            last_saved: RefCell::new(Vec::new()),
            route: None,
            last_scan: None,
            own_exe: std::env::current_exe()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .to_lowercase(),
            _com: com,
        })
    }
    fn save(&self) -> Result<(), String> {
        let journal = Journal {
            version: 1,
            records: self.records.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_JOURNAL {
            return Err("Ducking-Wiederherstellung: Journal voll".into());
        }
        if *self.last_saved.borrow() == bytes {
            return Ok(());
        }
        crate::config::write_atomic(&self.path, &bytes, true).map_err(|e| e.to_string())?;
        *self.last_saved.borrow_mut() = bytes;
        Ok(())
    }
    fn endpoint(&self, route: Option<&str>) -> Result<IMMDevice, String> {
        let device = unsafe {
            match route {
                Some(route) => self.enumerator.GetDevice(&HSTRING::from(route)),
                None => self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole),
            }
        }
        .map_err(|e| format!("Ausgabegerät nicht verfügbar: {e}"))?;
        if unsafe { device.GetState() }.map_err(|e| e.to_string())? != DEVICE_STATE_ACTIVE {
            return Err("Ausgabegerät nicht aktiv; Wiederherstellung wartet".into());
        }
        Ok(device)
    }
    fn scan(&mut self, route: &str, device: &IMMDevice) -> Result<(), String> {
        let manager: IAudioSessionManager2 =
            unsafe { device.Activate(CLSCTX_ALL, None) }.map_err(|e| e.to_string())?;
        let sessions = unsafe { manager.GetSessionEnumerator() }.map_err(|e| e.to_string())?;
        let count = unsafe { sessions.GetCount() }.map_err(|e| e.to_string())?;
        if !(0..=MAX_SESSIONS as i32).contains(&count) {
            return Err("Zu viele Audiositzungen für sichere Absenkung".into());
        }
        for session in self.live.values_mut().filter(|s| s.route == route) {
            session.active = false;
        }
        for i in 0..count {
            let Ok(control) =
                (unsafe { sessions.GetSession(i) }).and_then(|s| s.cast::<IAudioSessionControl2>())
            else {
                continue;
            };
            let Ok(pid) = (unsafe { control.GetProcessId() }) else {
                continue;
            };
            // Unknown identity, system notifications and all ICU processes are
            // excluded, including another ICU instance in another directory.
            if pid == 0
                || pid == std::process::id()
                || unsafe { control.IsSystemSoundsSession() }.0 == 0
            {
                continue;
            }
            let Some(exe) = process_exe(pid) else {
                continue;
            };
            if is_icu(&exe, &self.own_exe) {
                continue;
            }
            let Ok(session) = (unsafe { control.GetSessionIdentifier() }).and_then(take_string)
            else {
                continue;
            };
            let Ok(instance) =
                (unsafe { control.GetSessionInstanceIdentifier() }).and_then(take_string)
            else {
                continue;
            };
            if session.len() > 4096 || instance.len() > 4096 {
                continue;
            }
            let id = key(route, &instance);
            let active = unsafe { control.GetState() }.is_ok_and(|s| s == AudioSessionStateActive);
            if let Some(existing) = self.live.get_mut(&id) {
                existing.active = active;
                continue;
            }
            if self.live.len() >= MAX_SESSIONS {
                continue;
            }
            let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
                continue;
            };
            let Ok(value) = (unsafe { volume.GetMasterVolume() }) else {
                continue;
            };
            let changes = Arc::new(Changes {
                volume: AtomicU32::new(value.to_bits()),
                ..Changes::default()
            });
            let events: IAudioSessionEvents = Events {
                changes: changes.clone(),
            }
            .into();
            if unsafe { control.RegisterAudioSessionNotification(&events) }.is_err() {
                continue;
            }
            let meter = control.cast::<IAudioMeterInformation>().ok();
            self.live.insert(
                id,
                Live {
                    route: route.into(),
                    session,
                    instance,
                    control,
                    volume,
                    meter,
                    events,
                    changes,
                    active,
                },
            );
        }
        Ok(())
    }
    fn recovery_live(&self, record: &Record) -> Option<String> {
        let id = record.key();
        if self.live.get(&id).is_some_and(|s| s.active) {
            return Some(id);
        }
        let mut active = self
            .live
            .iter()
            .filter(|(_, s)| s.active && s.route == record.route && s.session == record.session);
        if let Some(first) = active.next() {
            return active.next().is_none().then(|| first.0.clone());
        }
        if self.live.contains_key(&id) {
            return Some(id);
        }
        // Windows persists a session's gain into a new process instance. Never
        // adopt that attenuated value as its new baseline. Reattach only when
        // there is one unambiguous instance of the same stable session.
        let mut candidates = self
            .live
            .iter()
            .filter(|(_, s)| s.route == record.route && s.session == record.session);
        let first = candidates.next()?;
        candidates.next().is_none().then(|| first.0.clone())
    }
    fn restore_selected(&mut self, all: bool) -> bool {
        let ids: Vec<_> =
            self.records
                .iter()
                .filter(|(id, record)| {
                    all || self.recovering.contains(*id)
                        || self.recovery_live(record).is_none_or(|live| {
                            live != record.key()
                                || self.live.get(&live).is_none_or(|s| {
                                    !s.active || Some(&s.route) != self.route.as_ref()
                                })
                        })
                })
                .map(|(id, _)| id.clone())
                .collect();
        for id in ids {
            let Some(record) = self.records.get(&id).cloned() else {
                continue;
            };
            let Some(live_id) = self.recovery_live(&record) else {
                continue;
            };
            let session = &self.live[&live_id];
            let Ok(current) = (unsafe { session.volume.GetMasterVolume() }) else {
                continue;
            };
            if session.changes.manual.load(Ordering::Acquire) || !record.owns(current) {
                // A user's absolute application gain is authoritative, even if
                // they returned to a number we had written earlier.
                session.changes.manual.store(true, Ordering::Release);
                self.records.remove(&id);
                self.recovering.remove(&id);
                continue;
            }
            if same(&[current], &record.gain.baseline) {
                self.records.remove(&id);
                self.recovering.remove(&id);
                continue;
            }
            let mut pending = record.clone();
            pending.stage(current, record.gain.baseline.clone());
            self.records.insert(id.clone(), pending);
            // Durability precedes restoration too: a second crash must accept
            // both the attenuated and restored sides of this exact write.
            if self.save().is_err() {
                return false;
            }
            let session = &self.live[&live_id];
            if session.changes.manual.load(Ordering::Acquire) {
                continue;
            }
            if unsafe {
                session
                    .volume
                    .SetMasterVolume(record.gain.baseline[0], &CONTEXT)
            }
            .is_ok()
                && unsafe { session.volume.GetMasterVolume() }
                    .is_ok_and(|v| same(&[v], &record.gain.baseline))
            {
                self.records.remove(&id);
                self.recovering.remove(&id);
            }
        }
        if self.save().is_err() {
            return false;
        }
        self.records.is_empty()
    }
    pub fn active_route(&self) -> Option<String> {
        self.route.clone()
    }

    pub fn measure(&mut self, route: Option<&str>) -> Result<Background, String> {
        let device = self.endpoint(route)?;
        let actual = (unsafe { device.GetId() })
            .and_then(take_string)
            .map_err(|e| e.to_string())?;
        if self.route.as_ref() != Some(&actual) {
            self.restore();
            self.route = Some(actual.clone());
            self.last_scan = None;
        }
        if self
            .last_scan
            .is_none_or(|t| t.elapsed() >= Duration::from_millis(250))
        {
            self.scan(&actual, &device)?;
            // Retry unavailable old endpoints independently of the selected one.
            let routes: HashSet<_> = self
                .records
                .values()
                .map(|r| r.route.clone())
                .filter(|r| *r != actual)
                .collect();
            for old in routes {
                if let Ok(endpoint) = self.endpoint(Some(&old)) {
                    let _ = self.scan(&old, &endpoint);
                }
            }
            self.last_scan = Some(Instant::now());
        }
        self.restore_selected(false);
        self.live.retain(|id, s| {
            s.active
                || self.records.contains_key(id)
                || self
                    .records
                    .values()
                    .any(|r| r.route == s.route && r.session == s.session)
        });
        let mut audible = false;
        let mut unknown = false;
        for (id, session) in self
            .live
            .iter()
            .filter(|(_, s)| s.active && s.route == actual)
        {
            if unsafe { session.volume.GetMute() }.is_ok_and(|mute| mute.as_bool()) {
                continue;
            }
            let peak = session
                .meter
                .as_ref()
                .and_then(|m| unsafe { m.GetPeakValue() }.ok());
            if let Some(peak) = peak.filter(|p| p.is_finite()) {
                let undo = self
                    .records
                    .get(id)
                    .map_or(1.0, |r| r.gain.baseline[0] / r.gain.last[0].max(1e-6));
                audible |= peak * undo > 0.0001;
            } else {
                unknown = true;
            }
        }
        Ok(Background {
            rms_db: None,
            audible: if audible {
                Some(true)
            } else if unknown {
                None
            } else {
                Some(false)
            },
        })
    }
    pub fn apply(&mut self, db: f32) -> Result<usize, String> {
        if !db.is_finite() {
            return Err("Ungültige Absenkung".into());
        }
        if db <= 0.0 {
            return if self.restore() {
                Ok(0)
            } else {
                Err("Wiederherstellung wartet auf Audiositzung".into())
            };
        }
        let mut writes = Vec::new();
        let mut dirty = false;
        for (id, session) in &self.live {
            if !session.active || Some(&session.route) != self.route.as_ref() {
                continue;
            }
            if session.changes.manual.load(Ordering::Acquire) {
                dirty |= self.records.remove(id).is_some();
                continue;
            }
            // Pending recovery for an old instance has priority over new gain.
            if self.records.iter().any(|(key, r)| {
                (self.recovering.contains(key) || key != id)
                    && r.route == session.route
                    && r.session == session.session
            }) {
                continue;
            }
            let current = unsafe { session.volume.GetMasterVolume() }.map_err(|e| e.to_string())?;
            if !self.records.contains_key(id) {
                if self.records.len() >= MAX_SESSIONS {
                    return Err("Ducking-Wiederherstellung: Journal voll".into());
                }
                let gain = Gain::new(vec![current])
                    .filter(|g| g.baseline[0] <= 1.0)
                    .ok_or("Ungültiger Sitzungspegel")?;
                self.records.insert(
                    id.clone(),
                    Record {
                        route: session.route.clone(),
                        session: session.session.clone(),
                        instance: session.instance.clone(),
                        gain,
                        pending: false,
                    },
                );
            }
            let record = self.records.get_mut(id).unwrap();
            if !record.owns(current) {
                session.changes.manual.store(true, Ordering::Release);
                self.records.remove(id);
                dirty = true;
                continue;
            }
            let next = record.gain.target(db);
            if same(&[current], &next) {
                continue;
            }
            record.stage(current, next.clone());
            writes.push((id.clone(), next[0]));
            dirty = true;
        }
        if dirty {
            self.save()?;
        }
        for (id, value) in writes {
            let session = &self.live[&id];
            if session.changes.manual.load(Ordering::Acquire) {
                self.records.remove(&id);
                continue;
            }
            unsafe { session.volume.SetMasterVolume(value, &CONTEXT) }
                .map_err(|e| e.to_string())?;
            let readback =
                unsafe { session.volume.GetMasterVolume() }.map_err(|e| e.to_string())?;
            if !same(&[value], &[readback]) {
                return Err("Sitzungspegel wurde gleichzeitig geändert".into());
            }
            self.records.get_mut(&id).unwrap().pending = false;
        }
        if dirty {
            self.save()?;
        }
        Ok(self
            .records
            .iter()
            .filter(|(id, r)| {
                !r.pending
                    && self
                        .live
                        .get(*id)
                        .is_some_and(|s| s.active && Some(&s.route) == self.route.as_ref())
            })
            .count())
    }
    pub fn restore(&mut self) -> bool {
        let routes: HashSet<_> = self.records.values().map(|r| r.route.clone()).collect();
        for route in routes {
            if let Ok(device) = self.endpoint(Some(&route)) {
                let _ = self.scan(&route, &device);
            }
        }
        self.restore_selected(true)
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        self.restore();
    }
}
fn take_string(value: PWSTR) -> windows::core::Result<String> {
    let result = unsafe { value.to_string() }.map_err(|_| {
        windows::core::Error::from_hresult(windows::core::HRESULT(0x8007000du32 as i32))
    });
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    result
}
fn process_exe(pid: u32) -> Option<String> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = vec![0u16; 32768];
    let mut size = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };
    unsafe {
        let _ = CloseHandle(handle);
    }
    result.ok()?;
    Some(String::from_utf16_lossy(&buffer[..size as usize]).to_lowercase())
}
fn is_icu(exe: &str, own_exe: &str) -> bool {
    let base = exe.rsplit(['\\', '/']).next().unwrap_or(exe);
    exe.eq_ignore_ascii_case(own_exe)
        || ["icu.exe", "icu-client.exe", "icu_client.exe"]
            .iter()
            .any(|n| base.eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> Record {
        Record {
            route: "endpoint".into(),
            session: "stable".into(),
            instance: "instance".into(),
            gain: Gain::new(vec![0.8]).unwrap(),
            pending: false,
        }
    }
    #[test]
    fn journal_preserves_both_sides_only_until_write_is_confirmed() {
        let mut r = record();
        r.gain.written(vec![0.2]);
        r.pending = true;
        assert!(r.owns(0.8) && r.owns(0.2));
        assert!(!r.owns(0.4));
        r.pending = false;
        assert!(!r.owns(0.8));
        assert!(r.owns(0.2));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.json");
        let data = serde_json::to_vec(&Journal {
            version: 1,
            records: vec![r.clone()],
        })
        .unwrap();
        std::fs::write(&path, data).unwrap();
        assert_eq!(load(&path).unwrap()[&r.key()].gain.baseline, [0.8]);
        std::fs::write(&path, b"{broken").unwrap();
        assert!(load(&path).is_err());
        let mut invalid = r.clone();
        invalid.gain.baseline = vec![2.0];
        std::fs::write(
            &path,
            serde_json::to_vec(&Journal {
                version: 1,
                records: vec![invalid],
            })
            .unwrap(),
        )
        .unwrap();
        assert!(load(&path).is_err());
    }
    #[test]
    fn external_round_trip_is_detected_and_own_notifications_do_not_yield() {
        let c = Changes {
            volume: AtomicU32::new(0.8f32.to_bits()),
            ..Changes::default()
        };
        c.volume(0.2, true);
        c.volume(0.2, false);
        assert!(!c.manual.load(Ordering::Acquire));
        c.volume(0.4, false);
        c.volume(0.2, false);
        assert!(c.manual.load(Ordering::Acquire));
        c.volume(0.8, true);
        assert!(c.manual.load(Ordering::Acquire));
    }
    #[test]
    fn every_known_icu_binary_is_excluded_independently_of_install_directory() {
        assert!(is_icu(r"C:\other\ICU.EXE", r"C:\installed\icu.exe"));
        assert!(is_icu(r"D:\dev\icu-client.exe", r"C:\installed\icu.exe"));
        assert!(!is_icu(r"C:\games\game.exe", r"C:\installed\icu.exe"));
    }
    #[test]
    fn repeated_failed_restore_keeps_the_observed_attenuated_value() {
        let mut r = record();
        r.stage(0.8, vec![0.2]);
        r.pending = false;
        for _ in 0..3 {
            r.stage(0.2, r.gain.baseline.clone());
            assert!(
                r.owns(0.2),
                "an unsuccessful native restore keeps ownership"
            );
            assert!(
                r.owns(0.8),
                "a successful write before journal confirmation is recoverable"
            );
        }
    }
    /// Child fixture copied under a foreign executable name by the live test.
    #[test]
    #[ignore = "internal live-audio fixture"]
    fn background_fixture() {
        if std::env::var_os("ICU_DUCKING_FIXTURE").is_none() {
            return;
        }
        use crate::audio::{wasapi::WasapiBackend, AudioBackend};
        let mut backend = WasapiBackend::new().unwrap();
        let mut phase = 0.0f32;
        backend
            .open_playback(
                None,
                Box::new(move |samples| {
                    for sample in samples {
                        *sample = phase.sin() * 0.01;
                        phase = (phase + std::f32::consts::TAU * 220.0 / 48_000.0)
                            % std::f32::consts::TAU;
                    }
                }),
            )
            .unwrap();
        std::thread::sleep(Duration::from_secs(20));
    }
    /// Native Windows acceptance, deliberately opt-in and confined to a copied
    /// synthetic background process. Never modifies another application's gain.
    #[test]
    #[ignore = "needs a native Windows output endpoint; plays a quiet synthetic tone"]
    fn live_native_session_restore_and_manual_override() {
        use std::{
            mem::ManuallyDrop,
            process::{Command, Stdio},
        };
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let foreign = directory.path().join("icu-ducking-background-fixture.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &foreign).unwrap();
        let mut child = Child(
            Command::new(&foreign)
                .args([
                    "--exact",
                    "audio::ducking::windows::tests::background_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .env("ICU_DUCKING_FIXTURE", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let journal = directory.path().join("session.json");
        let mut native = Native::with_journal(journal.clone()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        let fixture = loop {
            native.measure(None).unwrap();
            if let Some((id, _)) = native.live.iter().find(|(_, s)| {
                unsafe { s.control.GetProcessId() }.ok() == Some(child.0.id()) && s.active
            }) {
                break id.clone();
            }
            assert!(
                Instant::now() < deadline,
                "synthetic background session did not start"
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        native.live.retain(|id, _| *id == fixture);
        let volume = native.live[&fixture].volume.clone();
        let baseline = unsafe { volume.GetMasterVolume() }.unwrap();
        let muted = unsafe { volume.GetMute() }.unwrap().as_bool();
        assert!(baseline > 0.01, "fresh fixture needs a nonzero baseline");
        assert_eq!(native.apply(12.0).unwrap(), 1);
        let attenuated = unsafe { volume.GetMasterVolume() }.unwrap();
        assert!((attenuated / baseline - 10.0f32.powf(-12.0 / 20.0)).abs() < 0.0001);
        native.apply(12.0).unwrap();
        assert!(
            same(
                &[unsafe { volume.GetMasterVolume() }.unwrap()],
                &[attenuated]
            ),
            "repeat does not compound"
        );
        assert_eq!(unsafe { volume.GetMute() }.unwrap().as_bool(), muted);
        assert!(native.restore());
        assert!(same(
            &[unsafe { volume.GetMasterVolume() }.unwrap()],
            &[baseline]
        ));
        native.live.retain(|id, _| *id == fixture);
        native.apply(6.0).unwrap();
        let manual = (baseline * 0.7).min(0.9);
        unsafe { volume.SetMasterVolume(manual, &GUID::zeroed()) }.unwrap();
        std::thread::sleep(Duration::from_millis(100));
        native.apply(12.0).unwrap();
        assert!(native.restore());
        assert!(
            same(&[unsafe { volume.GetMasterVolume() }.unwrap()], &[manual]),
            "manual slider wins"
        );
        unsafe { volume.SetMasterVolume(baseline, &GUID::zeroed()) }.unwrap();
        drop(native);
        // Simulate loss after a completed native write, before normal Drop.
        let mut before_crash = Native::with_journal(journal.clone()).unwrap();
        before_crash.measure(None).unwrap();
        before_crash.live.retain(|id, _| *id == fixture);
        before_crash.apply(12.0).unwrap();
        let mut abandoned = ManuallyDrop::new(before_crash);
        let mut restarted = Native::with_journal(journal).unwrap();
        restarted.measure(None).unwrap();
        assert!(
            same(&[unsafe { volume.GetMasterVolume() }.unwrap()], &[baseline]),
            "durable recovery must not adopt the attenuated baseline"
        );
        assert!(restarted.records.is_empty());
        abandoned.records.clear();
        unsafe {
            ManuallyDrop::drop(&mut abandoned);
        }
        // Exit the foreign app while attenuated. Its next instance must never
        // inherit our temporary attenuation as a fresh baseline.
        restarted.live.retain(|id, _| *id == fixture);
        restarted.apply(12.0).unwrap();
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        restarted.restore(); // May remain pending until the endpoint responds.
        child.0 = Command::new(&foreign)
            .args([
                "--exact",
                "audio::ducking::windows::tests::background_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("ICU_DUCKING_FIXTURE", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            restarted.measure(None).unwrap();
            if let Some(session) = restarted.live.values().find(|s| {
                unsafe { s.control.GetProcessId() }.ok() == Some(child.0.id()) && s.active
            }) {
                let actual = unsafe { session.volume.GetMasterVolume() }.unwrap();
                assert!(
                    same(&[actual], &[baseline]),
                    "application restart must restore its original volume: {actual} vs {baseline}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "restarted background session did not start"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(restarted.restore());
        drop(child);
    }
}
