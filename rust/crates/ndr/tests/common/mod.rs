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

        let mut fallback = std::env::var("CARGO_TARGET_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let mut repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
                repo_root.pop(); // ndr
                repo_root.pop(); // crates
                repo_root.pop(); // rust

                let cargo_target = repo_root.join(".cargo-target");
                if cargo_target.exists() {
                    cargo_target
                } else {
                    repo_root.join("rust").join("target")
                }
            });
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
