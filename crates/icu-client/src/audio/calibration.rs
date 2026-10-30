//! Machine-local, bounded last-known-good audio profiles. Never stores audio.
use crate::config::{write_atomic, AudioSettings, ConfigError, Paths, VoiceStrength};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    time::{Duration, Instant},
};

const MAX_PROFILES: usize = 16;
const MAX_AGE: u64 = 30 * 24 * 3600;

struct ProfileLease(fs::File);
impl Drop for ProfileLease {
    fn drop(&mut self) {
        // Closing alone can retain flock until a concurrently forked child execs.
        // Release the completed transaction even if it inherited this descriptor.
        let _ = self.0.unlock();
    }
}

/// Processing only: restoring a profile cannot unmute, change PTT, or select devices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessingSettings {
    #[serde(default)]
    policy_version: u16,
    #[serde(default)]
    input_channel: Option<u16>,
    gain_db: f32,
    denoise: bool,
    aec: bool,
    auto_level: bool,
    voice_opt: bool,
    voice_strength: VoiceStrength,
    normalize_outgoing: bool,
    normalize_incoming: bool,
    peer_assistance: bool,
    tune_system_mic: bool,
    vad_auto: bool,
    vad_threshold: f32,
}
impl From<&AudioSettings> for ProcessingSettings {
    fn from(a: &AudioSettings) -> Self {
        Self {
            policy_version: icu_proto::audio_level::POLICY_VERSION,
            input_channel: a.input_channel,
            gain_db: a.gain_db,
            denoise: a.denoise,
            aec: a.aec,
            auto_level: a.auto_level,
            voice_opt: a.voice_opt,
            voice_strength: a.voice_strength,
            normalize_outgoing: a.normalize_outgoing,
            normalize_incoming: a.normalize_incoming,
            peer_assistance: a.peer_assistance,
            tune_system_mic: a.tune_system_mic,
            vad_auto: a.vad_auto,
            vad_threshold: a.vad_threshold,
        }
    }
}
impl ProcessingSettings {
    pub fn restore(&self, a: &mut AudioSettings) {
        a.gain_db = self.gain_db;
        a.denoise = self.denoise;
        a.aec = self.aec;
        a.auto_level = self.auto_level;
        a.voice_opt = self.voice_opt;
        a.voice_strength = self.voice_strength;
        a.normalize_outgoing = self.normalize_outgoing;
        a.normalize_incoming = self.normalize_incoming;
        a.peer_assistance = self.peer_assistance;
        a.tune_system_mic = self.tune_system_mic;
        a.vad_auto = self.vad_auto;
        a.vad_threshold = self.vad_threshold;
    }
    fn valid(&self) -> bool {
        self.policy_version == icu_proto::audio_level::POLICY_VERSION
            && self.input_channel.is_none_or(|channel| channel < 64)
            && self.gain_db.is_finite()
            && (-20.0..=20.0).contains(&self.gain_db)
            && self.vad_threshold.is_finite()
            && (0.0..=1.0).contains(&self.vad_threshold)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub device: String,
    pub saved_at: u64,
    pub settings: ProcessingSettings,
    /// Native baseline before ICU's temporary system changes.
    pub capture_db: f32,
    /// Software gain referred to that baseline, not the temporarily raised input.
    pub mic_gain_db: f32,
    pub peer_trim_db: f32,
}
impl Profile {
    pub fn valid(&self, now: u64) -> bool {
        self.device.len() == 64
            && self.settings.valid()
            && self.capture_db.is_finite()
            && self.mic_gain_db.is_finite()
            && (-15.0..=15.0).contains(&self.mic_gain_db)
            && self.peer_trim_db.is_finite()
            && (-6.0..=6.0).contains(&self.peer_trim_db)
            && self.saved_at <= now
            && now - self.saved_at <= MAX_AGE
    }
}
pub fn device_key(backend: &str, actual_device: &str) -> String {
    device_channel_key(backend, actual_device, None)
}
/// Include the actually selected input, even when automatic channel selection
/// was requested. Unresolved channels must not warm-start another input.
pub fn device_channel_key(backend: &str, actual_device: &str, channel: Option<u16>) -> String {
    let mut h = Sha256::new();
    h.update(backend);
    h.update([0]);
    h.update(actual_device);
    h.update([0]);
    h.update(icu_proto::audio_level::POLICY_VERSION.to_le_bytes());
    h.update(channel.unwrap_or(u16::MAX).to_le_bytes());
    format!("{:x}", h.finalize())
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    version: u32,
    profiles: Vec<Profile>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            version: 1,
            profiles: vec![],
        }
    }
}
impl Store {
    pub fn load(paths: &Paths) -> Self {
        let path = paths.config_dir.join("audio-calibration.toml");
        let loaded = (|| {
            if fs::metadata(&path).ok()?.len() > 65_536 {
                return None;
            }
            let store: Self = toml::from_str(&fs::read_to_string(path).ok()?).ok()?;
            (store.version == 1 && store.profiles.len() <= MAX_PROFILES).then_some(store)
        })();
        loaded.unwrap_or_default()
    }
    pub fn profile(&self, device: &str, now: u64) -> Option<&Profile> {
        self.profiles
            .iter()
            .find(|p| p.device == device && p.valid(now))
    }
    pub fn warm_start(
        &self,
        device: &str,
        audio: &AudioSettings,
        capture_db: f32,
        now: u64,
    ) -> Option<(f32, f32)> {
        let p = self.profile(device, now)?;
        (p.settings == ProcessingSettings::from(audio) && (p.capture_db - capture_db).abs() <= 0.5)
            .then_some((p.mic_gain_db.clamp(-15.0, 6.0), p.peer_trim_db))
    }
    /// Serialize the read/merge/write cycle across processes. Called off the DSP thread.
    pub fn save_profile(
        &mut self,
        paths: &Paths,
        profile: Profile,
        now: u64,
    ) -> Result<(), ConfigError> {
        self.update(paths, Some(profile), None, now)
    }
    pub fn forget(&mut self, paths: &Paths, device: &str, now: u64) -> Result<(), ConfigError> {
        self.update(paths, None, Some(device), now)
    }
    fn update(
        &mut self,
        paths: &Paths,
        profile: Option<Profile>,
        forget: Option<&str>,
        now: u64,
    ) -> Result<(), ConfigError> {
        if profile.as_ref().is_some_and(|p| !p.valid(now)) {
            return Err(ConfigError::Parse(
                "invalid audio calibration profile".into(),
            ));
        }
        fs::create_dir_all(&paths.config_dir).map_err(ConfigError::Io)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lease = options
            .open(paths.config_dir.join("audio-calibration.lock"))
            .map_err(ConfigError::Io)?;
        lease
            .try_lock()
            .map_err(|e| ConfigError::Io(std::io::Error::other(e.to_string())))?;
        let _lease = ProfileLease(lease);
        let mut latest = Self::load(paths);
        latest.profiles.retain(|p| {
            p.valid(now)
                && Some(p.device.as_str()) != forget
                && profile.as_ref().is_none_or(|new| new.device != p.device)
        });
        if let Some(p) = profile.filter(|p| p.valid(now)) {
            latest.profiles.push(p);
        }
        latest
            .profiles
            .sort_by_key(|p| std::cmp::Reverse(p.saved_at));
        latest.profiles.truncate(MAX_PROFILES);
        let text =
            toml::to_string_pretty(&latest).map_err(|e| ConfigError::Parse(e.to_string()))?;
        write_atomic(
            &paths.config_dir.join("audio-calibration.toml"),
            text.as_bytes(),
            true,
        )?;
        *self = latest;
        Ok(())
    }
}
struct Candidate {
    profile: Profile,
    first: Instant,
    last: Instant,
    windows: u32,
}
#[derive(Default)]
pub struct Learner {
    candidate: Option<Candidate>,
    last_window: Option<icu_proto::AudioWindow>,
    last_save: Option<Instant>,
}
impl Learner {
    pub fn reset(&mut self) {
        self.candidate = None;
    }
    /// Count each matched remote observation once, independently of timer ticks.
    pub fn observe_window(
        &mut self,
        profile: Profile,
        good: bool,
        window: icu_proto::AudioWindow,
        now: Instant,
    ) -> Option<Profile> {
        if !window.valid()
            || self.last_window.is_some_and(|old| {
                if window.gain_epoch == old.gain_epoch {
                    !window.newer_hint_than(old)
                } else {
                    window.gain_epoch.wrapping_sub(old.gain_epoch) as i32 <= 0
                }
            })
        {
            return None;
        }
        self.last_window = Some(window);
        self.observe(profile, good, now)
    }
    /// Call only for actual fresh speech windows; silence does not fabricate votes.
    pub fn observe(&mut self, profile: Profile, good: bool, now: Instant) -> Option<Profile> {
        if !good {
            self.reset();
            return None;
        }
        let same = self.candidate.as_ref().is_some_and(|c| {
            now.saturating_duration_since(c.last) <= Duration::from_secs(6)
                && c.profile.device == profile.device
                && c.profile.settings == profile.settings
                && (c.profile.capture_db - profile.capture_db).abs() <= 0.5
                && (c.profile.mic_gain_db - profile.mic_gain_db).abs() <= 1.0
                && (c.profile.peer_trim_db - profile.peer_trim_db).abs() <= 0.5
        });
        if !same {
            self.candidate = Some(Candidate {
                profile: profile.clone(),
                first: now,
                last: now,
                windows: 0,
            });
        }
        let c = self.candidate.as_mut()?;
        c.last = now;
        c.windows += 1;
        if c.windows >= 5
            && now.saturating_duration_since(c.first) >= Duration::from_secs(20)
            && self
                .last_save
                .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(60))
        {
            self.last_save = Some(now);
            return Some(profile);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile(now: u64) -> Profile {
        Profile {
            device: device_key("fake", "mic"),
            saved_at: now,
            settings: ProcessingSettings::from(&AudioSettings::default()),
            capture_db: -20.0,
            mic_gain_db: 10.0,
            peer_trim_db: 2.0,
        }
    }
    #[test]
    fn restart_corruption_device_changes_and_restore_privacy() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().to_path_buf()));
        let now = 1_800_000_000;
        let p = profile(now);
        let key = p.device.clone();
        let mut store = Store::default();
        store.save_profile(&paths, p.clone(), now).unwrap();
        let store = Store::load(&paths);
        let mut a = AudioSettings::default();
        assert_eq!(store.warm_start(&key, &a, -20.0, now), Some((6.0, 2.0)));
        assert!(store.warm_start("other", &a, -20.0, now).is_none());
        let mut invalid = p.clone();
        invalid.mic_gain_db = f32::NAN;
        let mut stale = Store::default();
        assert!(stale.save_profile(&paths, invalid, now).is_err());
        assert!(Store::load(&paths).profile(&key, now).is_some());
        assert!(store.warm_start(&key, &a, -18.0, now).is_none());
        assert!(store.profile(&key, now + MAX_AGE + 1).is_none());
        a.gain_db = 3.0;
        assert!(store.warm_start(&key, &a, -20.0, now).is_none());
        a.muted = true;
        a.mode = crate::config::ModeSetting::Ptt;
        a.input_device = Some("different".into());
        p.settings.restore(&mut a);
        assert!(a.muted);
        assert_eq!(a.mode, crate::config::ModeSetting::Ptt);
        assert_eq!(a.input_device.as_deref(), Some("different"));
        fs::write(paths.config_dir.join("audio-calibration.toml"), "broken [").unwrap();
        assert!(Store::load(&paths).profiles.is_empty());
    }
    #[test]
    fn stable_only_bounded_and_multi_profile_updates() {
        let t = Instant::now();
        let mut l = Learner::default();
        let mut saved = None;
        for i in 0..12 {
            saved = l
                .observe(profile(100), true, t + Duration::from_secs(i * 2))
                .or(saved);
        }
        assert!(saved.is_some());
        l.observe(profile(100), false, t + Duration::from_secs(30));
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().into()));
        let mut store = Store::default();
        for i in 0..20 {
            let mut p = profile(100 + i);
            p.device = device_key("fake", &i.to_string());
            store.save_profile(&paths, p, 120).unwrap();
        }
        assert_eq!(Store::load(&paths).profiles.len(), 16);
        let key = store.profiles[0].device.clone();
        store.forget(&paths, &key, 120).unwrap();
        assert!(store.profile(&key, 120).is_none());
    }
    #[test]
    fn stale_instances_merge_profiles_and_a_busy_lock_preserves_disk() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(Some(dir.path().into()));
        let now = 100;
        let mut first = Store::default();
        let mut second = Store::default();
        let p = profile(now);
        let first_key = p.device.clone();
        first.save_profile(&paths, p, now).unwrap();
        let mut p = profile(now);
        p.device = device_key("fake", "second");
        let second_key = p.device.clone();
        second.save_profile(&paths, p.clone(), now).unwrap();
        assert!(second.profile(&first_key, now).is_some());
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.config_dir.join("audio-calibration.lock"))
            .unwrap();
        lease.try_lock().unwrap();
        assert!(first.save_profile(&paths, p, now).is_err());
        assert!(Store::load(&paths).profile(&second_key, now).is_some());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn profile_lease_releases_with_an_inherited_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        file.try_lock().unwrap();
        // A fork before exec temporarily retains the same open file description.
        let inherited = file.try_clone().unwrap();
        drop(ProfileLease(file));
        let next = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        let acquired = next.try_lock();
        drop(inherited);
        assert!(
            acquired.is_ok(),
            "the completed writer must release its lease"
        );
    }
    #[test]
    fn policy_and_actual_channel_are_part_of_profile_compatibility() {
        assert_ne!(
            device_channel_key("fake", "mic", Some(0)),
            device_channel_key("fake", "mic", Some(1))
        );
        let mut p = profile(100);
        p.settings.policy_version = 0;
        assert!(
            !p.valid(100),
            "old metric profiles cannot warm start policy 2"
        );
        let mut a = AudioSettings::default();
        let first = ProcessingSettings::from(&a);
        a.input_channel = Some(1);
        assert_ne!(first, ProcessingSettings::from(&a));
    }
    #[test]
    fn stale_good_windows_do_not_accumulate_profile_confidence() {
        let mut learner = Learner::default();
        let now = Instant::now();
        for i in 0..20 {
            assert!(learner
                .observe(profile(100), true, now + Duration::from_secs(i * 10))
                .is_none());
        }
    }
    #[test]
    fn replayed_good_window_is_not_new_profile_confidence() {
        let mut learner = Learner::default();
        let now = Instant::now();
        let window = icu_proto::AudioWindow {
            gain_epoch: 1,
            first_ts: 0,
            end_ts: 96_000,
            report_id: 0,
        };
        for tick in 0..60 {
            assert!(learner
                .observe_window(profile(100), true, window, now + Duration::from_secs(tick))
                .is_none());
        }
        assert_eq!(learner.candidate.as_ref().unwrap().windows, 1);
    }
}
