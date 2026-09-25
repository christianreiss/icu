//! The one way this crate takes a mutex.

use std::sync::{Mutex, MutexGuard};

/// Locks `m`, taking the data even when another thread panicked while it
/// held the lock. Everything behind these mutexes is plain state a torn
/// update cannot corrupt beyond what the panic already did, and refusing the
/// lock would only turn one dead thread into a dead client.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
