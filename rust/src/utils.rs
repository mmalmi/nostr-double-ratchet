use crate::{Error, Result, SessionState};
use nostr::PublicKey;
use hkdf::Hkdf;
use sha2::Sha256;

pub fn kdf(input1: &[u8; 32], input2: &[u8], num_outputs: usize) -> Vec<[u8; 32]> {
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
