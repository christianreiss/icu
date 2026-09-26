//! What the user reads when something goes wrong.
//!
//! Every error that reaches the window goes through here: one or two plain
//! German sentences saying what happened and what to do, no code names, no
//! library jargon. The technical text the error carried is kept alongside
//! for the log and the "Technische Details" in the Verbindung window, so
//! support still gets it.

use icu_proto::{error as codes, passport::PassportError, types::NAME_MAX_LEN, NameError};

/// A user-facing problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// What the user reads.
    pub text: String,
    /// What a developer needs; `None` when `text` already says everything.
    pub technical: Option<String>,
}

impl Problem {
    /// A problem whose text is all there is.
    pub fn plain(text: impl Into<String>) -> Problem {
        Problem {
            text: text.into(),
            technical: None,
        }
    }

    /// A problem with the technical detail kept for support.
    pub fn new(text: impl Into<String>, technical: impl Into<String>) -> Problem {
        Problem {
            text: text.into(),
            technical: Some(technical.into()),
        }
    }

    /// The text with the technical detail in parentheses, for places that
    /// only have one line (log-adjacent UI, tests).
    pub fn full(&self) -> String {
        match &self.technical {
            Some(t) => format!("{} ({t})", self.text),
            None => self.text.clone(),
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// The server does not know or no longer trusts our credentials.
pub const CREDENTIALS_STALE: &str = "Deine Zugangsdaten passen nicht (mehr) zu diesem Server – \
     vermutlich wurde er neu eingerichtet. Bitte beim Admin eine neue Einladung anfordern.";

/// Our certificate was revoked. Since 2026-09-25 that is what a second
/// device holding the same passport sees once the other one renewed or
/// renamed and logged in with the new certificate: that login revokes every
/// older one (`PROTOCOL.md` §2.2 Nr. 10). A new invitation would create a
/// second account, so the text names the passport as the way back, and the
/// admin only for a real revocation.
pub const SUPERSEDED_REVOKED: &str = "Dieses Zertifikat gilt nicht mehr. Wurde ICU auf einem \
     anderen Gerät erneuert oder umbenannt, dort den Passport speichern und hier einspielen – \
     sonst den Admin fragen.";

/// The server does not know our certificate: the same multi-device case once
/// the replaced certificate has expired and left `users.toml`.
pub const SUPERSEDED_UNKNOWN: &str = "Der Server kennt dieses Zertifikat nicht (mehr). Wurde \
     ICU auf einem anderen Gerät erneuert oder umbenannt, dort den Passport speichern und hier \
     einspielen – sonst den Admin fragen.";

/// The server cannot be reached.
pub const UNREACHABLE: &str = "Der Server ist nicht erreichbar. Läuft er, stimmt die Adresse \
     und besteht eine Internetverbindung? ICU versucht es weiter.";

/// The server answered in a way this version does not understand.
pub const UPDATE_NEEDED: &str =
    "Der Server antwortet unerwartet. Bitte ICU auf die neueste Version aktualisieren.";

/// German text for a protocol code (`PROTOCOL.md` §6).
pub fn code_text(code: u32) -> String {
    let s = match code {
        codes::NO_ERROR => "Die Verbindung wurde beendet.",
        codes::PROTOCOL_VIOLATION => UPDATE_NEEDED,
        codes::VERSION_UNSUPPORTED => {
            "Diese ICU-Version passt nicht zum Server. Bitte ICU aktualisieren."
        }
        codes::AUTH_UNKNOWN_CERT => SUPERSEDED_UNKNOWN,
        codes::AUTH_REVOKED => SUPERSEDED_REVOKED,
        codes::SERVER_FULL => "Der Server ist voll. ICU versucht es gleich noch einmal.",
        codes::KICKED => "Ein Admin hat dich vom Server geworfen.",
        codes::HELLO_TIMEOUT | codes::TIMEOUT => {
            "Die Verbindung ist eingeschlafen. ICU verbindet sich neu."
        }
        codes::SHUTDOWN => "Der Server wird gerade neu gestartet. ICU verbindet sich gleich neu.",
        codes::RATE_LIMITED => "Zu viele Aktionen in kurzer Zeit – bitte einen Moment warten.",
        codes::RATE_LIMITED_FATAL => {
            "Zu viele Aktionen in kurzer Zeit – der Server hat die Verbindung getrennt. Bitte \
             ICU neu starten."
        }
        codes::AUTH_REQUIRED => {
            "Der Server verlangt eine Anmeldung, aber es sind keine Zugangsdaten eingerichtet. \
             Bitte eine Einladung öffnen."
        }
        codes::NAME_TAKEN => "Dieser Name ist schon vergeben. Bitte einen anderen wählen.",
        codes::ENROLL_BAD_TOKEN | codes::ENROLL_BOOTSTRAP_CLOSED => {
            "Diese Einladung gilt nicht (mehr). Bitte beim Admin eine neue anfordern."
        }
        codes::ENROLL_BAD_CSR => {
            "Die Anmeldung wurde abgelehnt. Bitte einen anderen Namen versuchen oder beim Admin \
             eine neue Einladung anfordern."
        }
        codes::ENROLL_RATE_LIMITED => {
            "Zu viele Anmeldeversuche – bitte in einer Minute noch einmal versuchen."
        }
        codes::RENEW_REJECTED => "Die Änderung wurde vom Server abgelehnt.",
        codes::NOT_ADMIN => "Das dürfen nur Admins.",
        codes::LAST_ADMIN => "Das geht nicht: Es muss immer mindestens ein Admin übrig bleiben.",
        codes::AUTH_DISABLED => {
            "Dein Zugang ist deaktiviert. Wende dich an den Admin, wenn das ein Irrtum ist."
        }
        codes::NO_SUCH_USER => "Diesen Nutzer gibt es nicht (mehr).",
        codes::SESSION_REPLACED => {
            "Mit denselben Zugangsdaten ist ein anderes Gerät verbunden, deshalb wurde hier \
             getrennt. „Neu verbinden“ holt die Sitzung hierher zurück – dann wird das andere \
             Gerät getrennt."
        }
        codes::REGISTRATION_STORED => "Registrierung abgeschlossen.",
        codes::RENEW_REQUIRED => "Das eigene Zertifikat ist abgelaufen und wird erneuert …",
        other => return format!("Unerwarteter Fehler (Code {other})."),
    };
    s.to_string()
}

/// A code the server sent, with its detail kept for support. Renewal
/// rejections say why when the reason is one we know.
pub fn from_code(code: u32, detail: &str) -> Problem {
    let text = if code == codes::RENEW_REJECTED {
        renew_text(detail)
    } else {
        code_text(code)
    };
    let technical = format!("{} ({code}): {detail}", codes::name(code));
    Problem::new(text, technical)
}

fn renew_text(detail: &str) -> String {
    if detail.contains("per day") || detail.contains("caught up today") {
        "Der Name lässt sich nur einmal am Tag ändern. Bitte morgen noch einmal versuchen.".into()
    } else if detail.contains("renewal opens") {
        "Das Zertifikat ist noch lange gültig und muss noch nicht erneuert werden.".into()
    } else if detail.contains("bad name") {
        "Dieser Name ist nicht erlaubt. Bitte einen anderen wählen.".into()
    } else if detail.contains("unknown user") {
        code_text(codes::AUTH_UNKNOWN_CERT)
    } else {
        code_text(codes::RENEW_REJECTED)
    }
}

/// German for the name rules.
pub fn name_error_text(e: &NameError) -> String {
    match e {
        NameError::Empty => "Bitte einen Namen eingeben.".into(),
        NameError::TooLong => format!(
            "Der Name ist zu lang (höchstens {} Bytes, bei Umlauten also etwas weniger \
             Zeichen).",
            NAME_MAX_LEN
        ),
        NameError::ControlChar | NameError::InvisibleChar => {
            "Der Name enthält unsichtbare Sonderzeichen. Bitte neu eintippen.".into()
        }
        NameError::Whitespace => {
            "Der Name darf nicht mit Leerzeichen beginnen oder enden und keine doppelten \
             Leerzeichen enthalten."
                .into()
        }
        NameError::NotNormalized | NameError::LeadingMark => {
            "Der Name enthält ungewöhnliche Zeichen. Bitte neu eintippen.".into()
        }
        NameError::MixedScript => {
            "Der Name mischt verschiedene Schriften (z. B. lateinische und kyrillische \
             Buchstaben). Bitte nur eine verwenden."
                .into()
        }
    }
}

/// A name that failed the rules.
pub fn from_name_error(e: &NameError) -> Problem {
    Problem::new(name_error_text(e), e.to_string())
}

/// German for a passport or invitation that does not read.
pub fn passport_text(e: &PassportError) -> String {
    match e {
        PassportError::BadArmor | PassportError::BadBase64 | PassportError::Malformed => {
            "Das ist keine gültige ICU-Einladung und kein ICU-Passport. Bitte den Text \
             vollständig kopieren oder die Datei neu anfordern."
                .into()
        }
        PassportError::TooLarge => "Die Datei ist zu groß für eine ICU-Einladung.".into(),
        PassportError::UnsupportedVersion(_) => {
            "Diese Einladung stammt von einer neueren ICU-Version. Bitte ICU aktualisieren.".into()
        }
    }
}

/// A file could not be written or read.
pub fn storage(what: &str, e: &dyn std::fmt::Display) -> Problem {
    Problem::new(
        format!(
            "{what} konnte nicht gespeichert werden. Ist genug Speicherplatz frei und darf ICU \
             in seinen Einstellungsordner schreiben?"
        ),
        e.to_string(),
    )
}

/// An audio device that would not open or broke down.
pub fn audio(device: &str, e: &dyn std::fmt::Display) -> Problem {
    Problem::new(
        format!(
            "{device} lässt sich nicht verwenden. Ist das Gerät angeschlossen und in den \
             Einstellungen unter Audio ausgewählt?"
        ),
        e.to_string(),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Code names, library prefixes and Debug names are what a non-techie
    /// cannot read; none may reach `text`.
    pub(crate) fn assert_readable(p: &Problem) {
        let t = &p.text;
        assert!(!t.is_empty());
        for bad in [
            "tls:",
            "transport:",
            "connection:",
            "protocol:",
            "Unknown",
            "Error",
        ] {
            assert!(!t.contains(bad), "{bad:?} in {t:?}");
        }
        let shouting = t
            .split(|c: char| !(c.is_ascii_uppercase() || c == '_'))
            .any(|w| w.len() >= 4);
        assert!(!shouting, "code name in {t:?}");
    }

    #[test]
    fn every_code_has_a_german_text() {
        for &code in codes::ALL_CODES {
            let p = from_code(code, "detail");
            assert_readable(&p);
            assert!(!p.text.contains("Unerwarteter"), "code {code} unmapped");
            assert!(p.technical.unwrap().contains(codes::name(code)));
        }
        assert!(code_text(0xEE).contains("0xEE") || code_text(0xEE).contains("238"));
    }

    /// A second device whose passport went stale is told how to get back
    /// in — not to request an invitation, which would make a second account.
    #[test]
    fn a_superseded_certificate_points_to_the_passport() {
        for code in [codes::AUTH_REVOKED, codes::AUTH_UNKNOWN_CERT] {
            let text = code_text(code);
            assert!(text.contains("Passport"), "{text}");
            assert!(text.contains("anderen Gerät"), "{text}");
            assert!(!text.contains("Einladung"), "{text}");
        }
        let replaced = code_text(codes::SESSION_REPLACED);
        assert!(replaced.contains("Neu verbinden"), "{replaced}");
        assert!(replaced.contains("andere"), "{replaced}");
    }

    #[test]
    fn renewal_reasons_are_told_apart() {
        let per_day = from_code(codes::RENEW_REJECTED, "at most one renewal per day");
        assert!(per_day.text.contains("einmal am Tag"));
        let window = from_code(codes::RENEW_REJECTED, "renewal opens 30 days before expiry");
        assert!(window.text.contains("noch lange gültig"));
        assert_readable(&from_code(codes::RENEW_REJECTED, "something new"));
    }

    #[test]
    fn name_and_passport_texts_are_readable() {
        use NameError::*;
        for e in [
            Empty,
            TooLong,
            ControlChar,
            InvisibleChar,
            Whitespace,
            NotNormalized,
            LeadingMark,
            MixedScript,
        ] {
            assert_readable(&from_name_error(&e));
        }
        for e in [
            PassportError::BadArmor,
            PassportError::BadBase64,
            PassportError::TooLarge,
            PassportError::UnsupportedVersion(9),
            PassportError::Malformed,
        ] {
            assert_readable(&Problem::plain(passport_text(&e)));
        }
    }
}
