use crate::{DevicePubkey, Error, Result, SessionState};
use hkdf::Hkdf;
use nostr::PublicKey;
use rand::{CryptoRng, RngCore};
use sha2::Sha256;

pub fn kdf(input1: &[u8], input2: &[u8], num_outputs: usize) -> Vec<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(input2), input1);

    let mut outputs = Vec::with_capacity(num_outputs);
    for i in 1..=num_outputs {
        let mut okm = [0u8; 32];
        hk.expand(&[i as u8], &mut okm)
            .expect("32 bytes is valid length");
        outputs.push(okm);
    }
    outputs
}

pub(crate) fn secret_key_from_bytes(bytes: &[u8; 32]) -> Result<nostr::SecretKey> {
    nostr::SecretKey::from_slice(bytes).map_err(Into::into)
}

pub(crate) fn device_pubkey_from_secret_bytes(bytes: &[u8; 32]) -> Result<DevicePubkey> {
    let secret = secret_key_from_bytes(bytes)?;
    let public = nostr::Keys::new(secret).public_key();
    Ok(DevicePubkey::from_nostr(public))
}

pub(crate) fn random_secret_key_bytes<R>(rng: &mut R) -> Result<[u8; 32]>
where
    R: RngCore + CryptoRng,
{
    loop {
        let mut candidate = [0u8; 32];
        rng.fill_bytes(&mut candidate);
        if nostr::SecretKey::from_slice(&candidate).is_ok() {
            return Ok(candidate);
        }
    }
}

pub fn deep_copy_state(state: &SessionState) -> SessionState {
    state.clone()
}

pub fn serialize_session_state(state: &SessionState) -> Result<String> {
    serde_json::to_string(state).map_err(|e| crate::Error::Serialization(e.to_string()))
}

pub fn deserialize_session_state(data: &str) -> Result<SessionState> {
    serde_json::from_str(data).map_err(|e| crate::Error::Serialization(e.to_string()))
}

pub fn pubkey_from_hex(hex_str: &str) -> Result<PublicKey> {
    let bytes = hex::decode(hex_str)?;
    if bytes.len() != 32 {
        return Err(Error::InvalidEvent("Invalid pubkey length".to_string()));
    }
    PublicKey::from_slice(&bytes).map_err(|e| Error::InvalidEvent(e.to_string()))
}

pub fn resolve_expiration_seconds(
    options: &crate::SendOptions,
    now_seconds: u64,
) -> Result<Option<u64>> {
    let has_expires_at = options.expires_at.is_some();
    let has_ttl = options.ttl_seconds.is_some();
    if has_expires_at && has_ttl {
        return Err(Error::InvalidEvent(
            "Provide either expires_at or ttl_seconds, not both".to_string(),
        ));
    }

    if let Some(expires_at) = options.expires_at {
        return Ok(Some(expires_at));
    }

    if let Some(ttl) = options.ttl_seconds {
        return now_seconds
            .checked_add(ttl)
            .ok_or_else(|| Error::InvalidEvent("ttl_seconds overflow".to_string()))
            .map(Some);
    }

    Ok(None)
}
