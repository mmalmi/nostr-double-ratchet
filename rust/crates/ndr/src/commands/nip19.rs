//! Helpers for parsing NIP-19 identifiers and Iris-style chat links.
//!
//! Iris chat links often look like:
//! - https://chat.iris.to/#npub1...
//! - https://chat.iris.to/#/npub1... (hash-routing style)
//! - nostr:npub1...
//! - nprofile1...

use nostr::PublicKey;

fn first_token(s: &str) -> &str {
    s.split(&['/', '?', '&'][..]).next().unwrap_or(s)
}

fn extract_nip19_candidate(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    // Support `nostr:npub1...` and `nostr:nprofile1...`
    let without_scheme = trimmed
        .strip_prefix("nostr:")
        .map(|s| s.trim())
        .unwrap_or(trimmed);

    let looks_like_nip19 = |s: &str| s.starts_with("npub1") || s.starts_with("nprofile1");

    // Raw nip19 token (optionally followed by a path/query)
    let candidate = without_scheme.trim_start_matches('/');
    if looks_like_nip19(candidate) {
        return Some(first_token(candidate).to_string());
    }

    // Full URL or any string with a hash fragment.
    if let Some((_, hash)) = without_scheme.rsplit_once('#') {
        let hash = hash.trim();
        let hash = hash.trim_start_matches('/');
        if looks_like_nip19(hash) {
            return Some(first_token(hash).to_string());
        }
    }

    None
}

pub(super) fn parse_pubkey(input: &str) -> Option<PublicKey> {
    use nostr::nips::nip19::{FromBech32, Nip19};

    let candidate = extract_nip19_candidate(input)?;
    match Nip19::from_bech32(candidate).ok()? {
        Nip19::Pubkey(pk) => Some(pk),
        Nip19::Profile(profile) => Some(profile.public_key),
        _ => None,
    }
}
