mod common;
mod listen;
mod receive;
mod send;
mod types;

pub use listen::listen;
pub use receive::receive;
pub use send::{read, react, receipt, send, typing};

pub fn resolve_target(
    target: &str,
    storage: &crate::storage::Storage,
) -> anyhow::Result<crate::storage::StoredChat> {
    common::resolve_target(target, storage)
}

#[cfg(test)]
mod tests;
