//! State files get their documented modes whatever the umask (external
//! review 2026-09-27, S-2). Alone in its test binary on purpose: the umask is
//! process wide, and every other test creating a file beside it would get
//! this one.

use std::os::unix::fs::PermissionsExt;

use icu_server::{ca::Ca, store};

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Root on a CIS-hardened host runs with umask 027. The offline CLI wrote
/// `users.toml` and `ca.crt` 0640 then — root-owned, so the service could no
/// longer read them and did not start. The owner half needs root to test;
/// the mode half is this.
#[test]
fn state_files_keep_their_modes_under_a_restrictive_umask() {
    let dir = tempfile::tempdir().unwrap();
    let config = icu_server::Config::for_test(dir.path());
    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o027));

    drop(store::Store::open(dir.path()).unwrap());
    Ca::load_or_create(dir.path(), &config.tls, "icu.home.arpa").unwrap();
    store::write_atomic(&dir.path().join("public"), "x").unwrap();
    store::write_private(&dir.path().join("secret"), "x").unwrap();
    store::write_private_for_caller(&dir.path().join("bundle.txt"), "x").unwrap();

    rustix::process::umask(old);
    for (name, want) in [
        (store::USERS_FILE, 0o644),
        ("ca.crt", 0o644),
        ("server.crt", 0o644),
        ("public", 0o644),
        ("ca.key", 0o600),
        ("server.key", 0o600),
        ("secret", 0o600),
        ("bundle.txt", 0o600),
    ] {
        assert_eq!(mode(&dir.path().join(name)), want, "{name} under umask 027");
    }
}
