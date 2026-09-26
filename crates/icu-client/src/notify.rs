//! Desktop notifications when someone comes or goes
//! (`Research/feature-freeze.md`, Join/Leave-Hinweis: tone and
//! notification, each switched on its own). On Linux through
//! `org.freedesktop.Notifications` on the session bus, which KDE, GNOME and
//! the notification daemons of the smaller desktops all serve; elsewhere
//! nothing is shown yet.

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// The one line shown.
    pub text: String,
    /// Category from the Desktop Notifications Specification, so a desktop
    /// can group or silence presence changes.
    pub category: &'static str,
}

impl Note {
    /// `name` came into the room.
    pub fn joined(name: &str) -> Note {
        Note {
            text: format!("{name} ist dazugekommen"),
            category: "presence.online",
        }
    }

    /// `name` left the room.
    pub fn left(name: &str) -> Note {
        Note {
            text: format!("{name} ist gegangen"),
            category: "presence.offline",
        }
    }
}

/// Hands notes to a task of its own, started with the first one. Each note
/// replaces the one before, so a room filling up again after a server
/// restart leaves one notification instead of a stack.
#[derive(Debug, Default)]
pub struct Notifier {
    tx: Option<tokio::sync::mpsc::UnboundedSender<Note>>,
}

impl Notifier {
    /// Shows `note`. Must be called inside a Tokio runtime; never blocks. A
    /// desktop without a notification service is logged once.
    pub fn show(&mut self, note: Note) {
        let tx = self.tx.get_or_insert_with(|| {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(run(rx));
            tx
        });
        let _ = tx.send(note);
    }
}

#[cfg(target_os = "linux")]
async fn run(mut rx: tokio::sync::mpsc::UnboundedReceiver<Note>) {
    let mut conn: Option<zbus::Connection> = None;
    let mut last_id = 0u32;
    let mut warned = false;
    while let Some(note) = rx.recv().await {
        if conn.is_none() {
            match zbus::Connection::session().await {
                Ok(c) => conn = Some(c),
                Err(e) => {
                    if !std::mem::replace(&mut warned, true) {
                        tracing::warn!(error = %e, "no session bus, notifications are off");
                    }
                    continue;
                }
            }
        }
        let Some(c) = &conn else { continue };
        match notify(c, last_id, &note).await {
            Ok(id) => last_id = id,
            Err(e) => {
                if !std::mem::replace(&mut warned, true) {
                    tracing::warn!(error = %e, "cannot show a notification");
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
async fn run(mut rx: tokio::sync::mpsc::UnboundedReceiver<Note>) {
    while rx.recv().await.is_some() {}
}

/// `Notify` of the Desktop Notifications Specification; returns the id the
/// next note replaces.
#[cfg(target_os = "linux")]
async fn notify(conn: &zbus::Connection, replaces: u32, note: &Note) -> zbus::Result<u32> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let hints: HashMap<&str, Value> = HashMap::from([
        ("category", Value::from(note.category)),
        // Lets the desktop take name and icon from `icu.desktop`.
        ("desktop-entry", Value::from("icu")),
        // Low: someone joining is no reason to break into a full-screen game.
        ("urgency", Value::U8(0)),
    ]);
    let reply = conn
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "ICU",
                replaces,
                "icu",
                note.text.as_str(),
                "",
                Vec::<&str>::new(),
                hints,
                -1i32,
            ),
        )
        .await?;
    reply.body().deserialize::<u32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_name_the_person_and_the_direction() {
        assert_eq!(Note::joined("Anna").text, "Anna ist dazugekommen");
        assert_eq!(Note::left("Anna").text, "Anna ist gegangen");
        assert_ne!(Note::joined("A").category, Note::left("A").category);
    }

    /// Shows a real notification on the desktop running the test, and
    /// replaces it: `cargo test -p icu-client --lib notify -- --ignored`.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "shows a notification on the desktop"]
    async fn a_note_reaches_the_desktop_and_is_replaced() {
        let conn = zbus::Connection::session().await.expect("session bus");
        let id = notify(&conn, 0, &Note::joined("ICU-Test"))
            .await
            .expect("Notify");
        assert_ne!(id, 0);
        let again = notify(&conn, id, &Note::left("ICU-Test"))
            .await
            .expect("Notify");
        assert_eq!(again, id, "the second note replaces the first");
    }
}
