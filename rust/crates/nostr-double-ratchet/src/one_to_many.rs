use base64::Engine;

use crate::{Error, Result, SenderKeyState, MESSAGE_EVENT_KIND};

/// A lightweight helper for "one-to-many" publishing:
///
/// - Outer Nostr event is authored by a sender-controlled pubkey (eg per-group sender keypair).
/// - Outer content is a compact base64 payload: `key_id_be || msg_num_be || nip44_ciphertext_bytes`.
/// - Ciphertext bytes are produced/consumed by [`SenderKeyState`].
#[derive(Debug, Clone)]
pub struct OneToManyChannel {
    outer_kind: u32,
}

impl Default for OneToManyChannel {
    fn default() -> Self {
        Self {
            outer_kind: MESSAGE_EVENT_KIND,
        }
    }
}

/// Parsed one-to-many outer content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneToManyMessage {
    pub key_id: u32,
    pub message_number: u32,
    pub ciphertext: Vec<u8>,
}

impl OneToManyMessage {
    pub fn decrypt(&self, state: &mut SenderKeyState) -> Result<String> {
        state.decrypt_from_bytes(self.message_number, &self.ciphertext)
    }
}

impl OneToManyChannel {
    pub fn new(outer_kind: u32) -> Self {
        Self { outer_kind }
    }

    pub fn outer_kind(&self) -> u32 {
        self.outer_kind
    }

    pub fn build_outer_content(
        &self,
        key_id: u32,
        message_number: u32,
        ciphertext_bytes: &[u8],
    ) -> String {
        let mut payload: Vec<u8> = Vec::with_capacity(8 + ciphertext_bytes.len());
        payload.extend_from_slice(&key_id.to_be_bytes());
        payload.extend_from_slice(&message_number.to_be_bytes());
        payload.extend_from_slice(ciphertext_bytes);
        base64::engine::general_purpose::STANDARD.encode(payload)
    }

    pub fn parse_outer_content(&self, content: &str) -> Result<OneToManyMessage> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;
        if bytes.len() < 8 {
            return Err(Error::InvalidEvent(
                "one-to-many payload too short".to_string(),
            ));
        }
        let key_id = u32::from_be_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| Error::InvalidEvent("invalid key_id bytes".to_string()))?,
        );
        let message_number = u32::from_be_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| Error::InvalidEvent("invalid message_number bytes".to_string()))?,
        );
        Ok(OneToManyMessage {
            key_id,
            message_number,
            ciphertext: bytes[8..].to_vec(),
        })
    }

    pub fn encrypt_to_outer_event(
        &self,
        sender_event_keys: &nostr::Keys,
        sender_key: &mut SenderKeyState,
        inner_plaintext: &str,
        created_at: nostr::Timestamp,
    ) -> Result<nostr::Event> {
        let (n, ciphertext_bytes) = sender_key.encrypt_to_bytes(inner_plaintext)?;
        let content = self.build_outer_content(sender_key.key_id, n, ciphertext_bytes.as_slice());

        let unsigned =
            nostr::EventBuilder::new(nostr::Kind::Custom(self.outer_kind as u16), &content)
                .custom_created_at(created_at)
                .build(sender_event_keys.public_key());

        let signed = unsigned.sign_with_keys(sender_event_keys)?;
        Ok(signed)
    }
}
