//! Spike 4a: (1) evdev-Zugriff ohne root, (2) XDG GlobalShortcuts-Portal Session+Bind, Activated/Deactivated.
use anyhow::Result;
use futures_util::StreamExt;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // (1) evdev
    let mut kbd = 0; let mut mice = 0; let mut denied = 0;
    for (path, dev) in evdev::enumerate() {
        let keys = dev.supported_keys();
        let has_kbd = keys.map(|k| k.contains(evdev::KeyCode::KEY_A)).unwrap_or(false);
        let has_mouse = keys.map(|k| k.contains(evdev::KeyCode::BTN_LEFT)).unwrap_or(false);
        if has_kbd { kbd += 1; println!("[evdev] keyboard {:?} {}", path, dev.name().unwrap_or("?")); }
        if has_mouse { mice += 1; println!("[evdev] mouse    {:?} {} side-buttons={}", path, dev.name().unwrap_or("?"), keys.map(|k| k.contains(evdev::KeyCode::BTN_SIDE)).unwrap_or(false)); }
    }
    let n_nodes = std::fs::read_dir("/dev/input")?.filter(|e| e.as_ref().map(|e| e.file_name().to_string_lossy().starts_with("event")).unwrap_or(false)).count();
    for e in std::fs::read_dir("/dev/input")? { let e = e?; if e.file_name().to_string_lossy().starts_with("event") { if std::fs::File::open(e.path()).is_err() { denied += 1; } } }
    println!("[evdev] event nodes={} keyboards={} mice={} open-denied={} (uid={})", n_nodes, kbd, mice, denied, unsafe { libc_getuid() });

    // (2) Portal
    let portal = ashpd::desktop::global_shortcuts::GlobalShortcuts::new().await?;
    let session = portal.create_session().await?;
    println!("[portal] session created");
    let sc = ashpd::desktop::global_shortcuts::NewShortcut::new("ptt", "ICU Push-to-Talk").preferred_trigger("CTRL+SHIFT+F12");
    println!("[portal] sending bind request; waiting up to 20 s (KDE zeigt evtl. Dialog)…");
    match tokio::time::timeout(Duration::from_secs(20), async { portal.bind_shortcuts(&session, &[sc], None).await?.response() }).await {
        Ok(Ok(resp)) => {
            for s in resp.shortcuts() { println!("[portal] bound: id={} desc={} trigger={:?}", s.id(), s.description(), s.trigger_description()); }
            let mut act = portal.receive_activated().await?;
            let mut deact = portal.receive_deactivated().await?;
            println!("[portal] listening 15 s for Activated/Deactivated (press CTRL+SHIFT+F12)…");
            let deadline = tokio::time::sleep(Duration::from_secs(15));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    Some(a) = act.next() => println!("[portal] Activated id={} ts={:?}", a.shortcut_id(), a.timestamp()),
                    Some(d) = deact.next() => println!("[portal] Deactivated id={} ts={:?}", d.shortcut_id(), d.timestamp()),
                    _ = &mut deadline => break,
                }
            }
        }
        Ok(Err(e)) => println!("[portal] bind response error: {e}"),
        Err(_) => println!("[portal] bind: keine Antwort in 20 s (Dialog unbeantwortet?)"),
    }
    Ok(())
}
extern "C" { fn getuid() -> u32; }
unsafe fn libc_getuid() -> u32 { getuid() }
