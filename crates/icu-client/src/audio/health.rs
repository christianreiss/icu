//! Typed, sample-free audio health for UI and diagnostics.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum State {
    #[default]
    Learning,
    Adjusting,
    Good,
    Held,
    Conflicting,
    InputClipping,
    SoftwareOverload,
    Limited,
    Disabled,
    LocalOnly,
    Bound,
    NoFeedback,
    NoLocalSpeech,
}
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AudioHealth {
    pub state: State,
    pub reporters: u16,
    pub local_correction_db: Option<f32>,
    pub remote_correction_db: Option<f32>,
    pub software_gain_db: f32,
    pub peer_trim_db: f32,
    pub system_gain_db: f32,
    pub system_status: String,
    pub limiter_reduction_db: f32,
    pub limited_fraction: f32,
    pub profile_saved: bool,
    pub profile_saved_at: Option<u64>,
    pub feedback_age_secs: Option<f32>,
}
impl AudioHealth {
    pub fn text(&self) -> String {
        match self.state {
            State::Learning => "Pegel wird gelernt".into(),
            State::Adjusting => "Pegel wird angepasst".into(),
            State::Good => format!("Bei {} Empfängern passend", self.reporters),
            State::Held => "Guten Pegel während der Pause gehalten".into(),
            State::Conflicting => "Empfänger und lokale Messung widersprechen sich".into(),
            State::InputClipping => "Mikrofon übersteuert".into(),
            State::SoftwareOverload => "Softwareverstärkung wird begrenzt".into(),
            State::Limited => "Ausgang dauerhaft begrenzt".into(),
            State::LocalOnly => "Lokale Pegelregelung aktiv".into(),
            State::Bound => "Regelgrenze erreicht – Mikrofonpegel prüfen".into(),
            State::Disabled => "Pegelautomatik aus".into(),
            State::NoFeedback => "Warte auf Empfangsfeedback".into(),
            State::NoLocalSpeech => "Warte auf Sprachmessung".into(),
        }
    }
    pub fn diagnostic(&self) -> String {
        format!("{} · {} · Empfänger {} · lokal {:?} dB · Empfang {:?} dB · System {:+.1} dB · Software {:+.1} dB · Peer {:+.1} dB · Limiter {:.1} dB / {:.1}% · Profil {} · gespeichert {:?} · Messalter {:?} s",
        self.text(),self.system_status,self.reporters,self.local_correction_db,self.remote_correction_db,self.system_gain_db,self.software_gain_db,self.peer_trim_db,self.limiter_reduction_db,self.limited_fraction*100.0,self.profile_saved,self.profile_saved_at,self.feedback_age_secs)
    }
}
