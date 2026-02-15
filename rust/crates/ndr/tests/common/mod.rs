//! Test utilities

use std::sync::OnceLock;

pub mod ws_relay;
pub use ws_relay::WsRelay;

#[allow(dead_code)]
pub fn ndr_binary() -> &'static std::path::PathBuf {
    static BIN: OnceLock<std::path::PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Some(bin) = option_env!("CARGO_BIN_EXE_ndr") {
            let path = std::path::PathBuf::from(bin);
            if path.exists() {
                return path;
            }
        }

        // Fallback: workspace `target/debug/ndr` (works when running tests from the repo root).
        let mut fallback = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        fallback.pop(); // ndr
        fallback.pop(); // crates
        fallback.push("target");
        fallback.push("debug");
        fallback.push("ndr");
        #[cfg(windows)]
        fallback.set_extension("exe");

        if !fallback.exists() {
            panic!(
                "ndr binary not found (CARGO_BIN_EXE_ndr missing and fallback path does not exist): {}",
                fallback.display()
            );
        }
        fallback
    })
}
