//! Windows Raw Input hotkeys: M3. Every entry point reports `Unsupported`.

/// Not implemented before M3.
pub fn start() -> Result<(), String> {
    Err("Unsupported: Raw Input hotkeys not implemented (M3)".into())
}
