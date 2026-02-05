use crate::{
    pubsub::{build_filter, NostrPubSub},
    Error, Result, Session, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND,
};
use base64::Engine;
use nostr::nips::nip44::{self, Version};
use nostr::PublicKey;
use nostr::{EventBuilder, Keys, Kind, Tag, Timestamp, UnsignedEvent};
use nostr::types::filter::{Alphabet, SingleLetterTag};

#[derive(Clone)]
pub struct Invite {
    pub inviter_ephemeral_public_key: PublicKey,
    pub shared_secret: [u8; 32],
    pub inviter: PublicKey,
    pub inviter_ephemeral_private_key: Option<[u8; 32]>,
    pub device_id: Option<String>,
    pub max_uses: Option<usize>,
    pub used_by: Vec<PublicKey>,
    pub created_at: u64,
    pub purpose: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

pub struct InviteResponse {
    pub session: Session,
    pub invitee_identity: PublicKey,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

impl Invite {
    pub fn create_new(
        inviter: PublicKey,
        device_id: Option<String>,
        max_uses: Option<usize>,
    ) -> Result<Self> {
        let inviter_ephemeral_keys = Keys::generate();
        let inviter_ephemeral_public_key = inviter_ephemeral_keys.public_key();
        let inviter_ephemeral_private_key = inviter_ephemeral_keys.secret_key().to_secret_bytes();

        let shared_secret = Keys::generate().secret_key().to_secret_bytes();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Ok(Self {
            inviter_ephemeral_public_key,
            shared_secret,
            inviter,
            inviter_ephemeral_private_key: Some(inviter_ephemeral_private_key),
            device_id,
            max_uses,
            used_by: Vec::new(),
            created_at: now,
            purpose: None,
            owner_public_key: None,
        })
    }

    pub fn get_url(&self, root: &str) -> Result<String> {
        let mut data = serde_json::Map::new();
        data.insert(
            "inviter".to_string(),
            serde_json::Value::String(hex::encode(self.inviter.to_bytes())),
        );
        data.insert(
            "ephemeralKey".to_string(),
            serde_json::Value::String(hex::encode(self.inviter_ephemeral_public_key.to_bytes())),
        );
        data.insert(
            "sharedSecret".to_string(),
            serde_json::Value::String(hex::encode(self.shared_secret)),
        );
        if let Some(purpose) = &self.purpose {
            data.insert("purpose".to_string(), serde_json::Value::String(purpose.clone()));
        }
        if let Some(owner_pk) = &self.owner_public_key {
            data.insert(
                "owner".to_string(),
                serde_json::Value::String(hex::encode(owner_pk.to_bytes())),
            );
        }

        let url = format!(
            "{}#{}",
            root,
            urlencoding::encode(&serde_json::Value::Object(data).to_string())
        );
        Ok(url)
    }

    pub fn from_url(url: &str) -> Result<Self> {
        let hash = url
            .split('#')
            .nth(1)
            .ok_or(Error::Invite("No hash in URL".to_string()))?;
        let decoded = urlencoding::decode(hash).map_err(|e| Error::Invite(e.to_string()))?;
        let data: serde_json::Value = serde_json::from_str(&decoded)?;

        let inviter = crate::utils::pubkey_from_hex(
            data["inviter"]
                .as_str()
                .ok_or(Error::Invite("Missing inviter".to_string()))?,
        )?;
        let ephemeral_key_str = data["ephemeralKey"]
            .as_str()
            .or_else(|| data["inviterEphemeralPublicKey"].as_str())
            .ok_or(Error::Invite(
                "Missing ephemeralKey".to_string(),
            ))?;
        let ephemeral_key = crate::utils::pubkey_from_hex(ephemeral_key_str)?;
        let shared_secret_hex = data["sharedSecret"]
            .as_str()
            .ok_or(Error::Invite("Missing sharedSecret".to_string()))?;
        let shared_secret_bytes = hex::decode(shared_secret_hex)?;
        let mut shared_secret = [0u8; 32];
        shared_secret.copy_from_slice(&shared_secret_bytes);

        let purpose = data["purpose"].as_str().map(|s| s.to_string());
        let owner_public_key = data["owner"]
            .as_str()
            .or_else(|| data["ownerPubkey"].as_str())
            .and_then(|s| crate::utils::pubkey_from_hex(s).ok());

        Ok(Self {
            inviter_ephemeral_public_key: ephemeral_key,
            shared_secret,
            inviter,
            inviter_ephemeral_private_key: None,
            device_id: None,
            max_uses: None,
            used_by: Vec::new(),
            created_at: 0,
            purpose,
            owner_public_key,
        })
    }

    pub fn get_event(&self) -> Result<UnsignedEvent> {
        let device_id = self.device_id.as_ref().ok_or(Error::DeviceIdRequired)?;

        let tags = vec![
            Tag::parse(&[
                "ephemeralKey".to_string(),
                hex::encode(self.inviter_ephemeral_public_key.to_bytes()),
            ])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            Tag::parse(&["sharedSecret".to_string(), hex::encode(self.shared_secret)])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            Tag::parse(&[
                "d".to_string(),
                format!("double-ratchet/invites/{}", device_id),
            ])
            .map_err(|e| Error::InvalidEvent(e.to_string()))?,
            Tag::parse(&["l".to_string(), "double-ratchet/invites".to_string()])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        ];

        let event = EventBuilder::new(Kind::from(INVITE_EVENT_KIND as u16), "")
            .tags(tags)
            .custom_created_at(Timestamp::from(self.created_at))
            .build(self.inviter);

        Ok(event)
    }

    pub fn from_event(event: &nostr::Event) -> Result<Self> {
        let inviter = event.pubkey;

        let ephemeral_key = event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("ephemeralKey"))
            .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()))
            .ok_or(Error::Invite("Missing ephemeralKey tag".to_string()))?;

        let shared_secret_hex = event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("sharedSecret"))
            .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()))
            .ok_or(Error::Invite("Missing sharedSecret tag".to_string()))?;

        let device_tag = event
            .tags
            .iter()
            .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("d"))
            .and_then(|t| t.as_slice().get(1).map(|s| s.to_string()));

        let device_id = device_tag.and_then(|d| d.split('/').nth(2).map(String::from));

        let inviter_ephemeral_public_key = crate::utils::pubkey_from_hex(&ephemeral_key)?;
        let shared_secret_bytes = hex::decode(&shared_secret_hex)?;
        let mut shared_secret = [0u8; 32];
        shared_secret.copy_from_slice(&shared_secret_bytes);

        Ok(Self {
            inviter_ephemeral_public_key,
            shared_secret,
            inviter,
            inviter_ephemeral_private_key: None,
            device_id,
            max_uses: None,
            used_by: Vec::new(),
            created_at: event.created_at.as_u64(),
            purpose: None,
            owner_public_key: None,
        })
    }

    pub fn accept(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
    ) -> Result<(Session, nostr::Event)> {
        self.accept_with_owner(invitee_public_key, invitee_private_key, device_id, None)
    }

    pub fn accept_with_owner(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
        owner_public_key: Option<PublicKey>,
    ) -> Result<(Session, nostr::Event)> {
        let invitee_session_keys = Keys::generate();
        let invitee_session_key = invitee_session_keys.secret_key().to_secret_bytes();
        let invitee_session_public_key = invitee_session_keys.public_key();

        let session = Session::init(
            self.inviter_ephemeral_public_key,
            invitee_session_key,
            true,
            self.shared_secret,
            None,
        )?;

        let mut payload = serde_json::Map::new();
        payload.insert(
            "sessionKey".to_string(),
            serde_json::Value::String(hex::encode(invitee_session_public_key.to_bytes())),
        );
        if let Some(device_id) = device_id.clone() {
            payload.insert(
                "deviceId".to_string(),
                serde_json::Value::String(device_id),
            );
        }
        if let Some(owner_pk) = owner_public_key {
            payload.insert(
                "ownerPublicKey".to_string(),
                serde_json::Value::String(hex::encode(owner_pk.to_bytes())),
            );
        }
        let payload = serde_json::Value::Object(payload);

        let invitee_sk = nostr::SecretKey::from_slice(&invitee_private_key)?;
        let dh_encrypted =
            nip44::encrypt(&invitee_sk, &self.inviter, payload.to_string(), Version::V2)?;

        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let encrypted_bytes = nip44::v2::encrypt_to_bytes(&conversation_key, &dh_encrypted)?;
        let inner_encrypted = base64::engine::general_purpose::STANDARD.encode(encrypted_bytes);

        let inner_event = serde_json::json!({
            "pubkey": hex::encode(invitee_public_key.to_bytes()),
            "content": inner_encrypted,
            "created_at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        });

        let random_sender_keys = Keys::generate();
        let random_sender_sk = random_sender_keys.secret_key();

        let envelope_content = nip44::encrypt(
            random_sender_sk,
            &self.inviter_ephemeral_public_key,
            inner_event.to_string(),
            Version::V2,
        )?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let two_days = 2 * 24 * 60 * 60;
        let random_now = now - (rand::random::<u64>() % two_days);

        // Build and sign the event with ephemeral keys
        let unsigned_envelope =
            EventBuilder::new(Kind::from(INVITE_RESPONSE_KIND as u16), envelope_content)
                .tag(
                    Tag::parse(&[
                        "p".to_string(),
                        hex::encode(self.inviter_ephemeral_public_key.to_bytes()),
                    ])
                    .map_err(|e| Error::InvalidEvent(e.to_string()))?,
                )
                .custom_created_at(Timestamp::from(random_now))
                .build(random_sender_keys.public_key());

        // Sign with the ephemeral keys before returning
        let signed_envelope = unsigned_envelope
            .sign_with_keys(&random_sender_keys)
            .map_err(|e| Error::InvalidEvent(e.to_string()))?;

        Ok((session, signed_envelope))
    }

    pub fn serialize(&self) -> Result<String> {
        let data = serde_json::json!({
            "inviterEphemeralPublicKey": hex::encode(self.inviter_ephemeral_public_key.to_bytes()),
            "sharedSecret": hex::encode(self.shared_secret),
            "inviter": hex::encode(self.inviter.to_bytes()),
            "inviterEphemeralPrivateKey": self.inviter_ephemeral_private_key.map(hex::encode),
            "deviceId": self.device_id,
            "maxUses": self.max_uses,
            "usedBy": self.used_by.iter().map(|pk| hex::encode(pk.to_bytes())).collect::<Vec<_>>(),
            "createdAt": self.created_at,
            "purpose": self.purpose.clone(),
            "ownerPublicKey": self
                .owner_public_key
                .as_ref()
                .map(|pk| hex::encode(pk.to_bytes())),
        });
        Ok(data.to_string())
    }

    pub fn deserialize(json: &str) -> Result<Self> {
        let data: serde_json::Value = serde_json::from_str(json)?;

        let inviter_ephemeral_public_key = crate::utils::pubkey_from_hex(
            data["inviterEphemeralPublicKey"]
                .as_str()
                .ok_or(Error::Invite("Missing field".to_string()))?,
        )?;

        let shared_secret_hex = data["sharedSecret"]
            .as_str()
            .ok_or(Error::Invite("Missing sharedSecret".to_string()))?;
        let shared_secret_bytes = hex::decode(shared_secret_hex)?;
        let mut shared_secret = [0u8; 32];
        shared_secret.copy_from_slice(&shared_secret_bytes);

        let inviter = crate::utils::pubkey_from_hex(
            data["inviter"]
                .as_str()
                .ok_or(Error::Invite("Missing inviter".to_string()))?,
        )?;

        let inviter_ephemeral_private_key =
            if let Some(hex_str) = data["inviterEphemeralPrivateKey"].as_str() {
                let bytes = hex::decode(hex_str)?;
                let mut array = [0u8; 32];
                array.copy_from_slice(&bytes);
                Some(array)
            } else {
                None
            };

        let used_by = if let Some(arr) = data["usedBy"].as_array() {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter_map(|s| crate::utils::pubkey_from_hex(s).ok())
                .collect()
        } else {
            Vec::new()
        };

        let purpose = data["purpose"].as_str().map(|s| s.to_string());
        let owner_public_key = data["ownerPublicKey"]
            .as_str()
            .and_then(|s| crate::utils::pubkey_from_hex(s).ok());

        Ok(Self {
            inviter_ephemeral_public_key,
            shared_secret,
            inviter,
            inviter_ephemeral_private_key,
            device_id: data["deviceId"].as_str().map(String::from),
            max_uses: data["maxUses"].as_u64().map(|u| u as usize),
            used_by,
            created_at: data["createdAt"].as_u64().unwrap_or(0),
            purpose,
            owner_public_key,
        })
    }

    pub fn listen_with_pubsub(&self, pubsub: &dyn NostrPubSub) -> Result<String> {
        let filter = build_filter()
            .kinds(vec![INVITE_RESPONSE_KIND as u64])
            .pubkeys(vec![self.inviter_ephemeral_public_key])
            .build();

        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!("invite-response-{}", uuid::Uuid::new_v4());
        pubsub.subscribe(subid.clone(), filter_json)?;
        Ok(subid)
    }

    pub fn listen(
        &self,
        event_tx: &crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) -> Result<()> {
        let _ = self.listen_with_pubsub(event_tx)?;
        Ok(())
    }

    pub fn from_user_with_pubsub(
        user_pubkey: PublicKey,
        pubsub: &dyn crate::NostrPubSub,
    ) -> Result<String> {
        let filter = nostr::Filter::new()
            .kind(Kind::from(INVITE_EVENT_KIND as u16))
            .authors(vec![user_pubkey])
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::L),
                ["double-ratchet/invites"],
            );

        let filter_json = serde_json::to_string(&filter)?;
        let subid = format!("invite-user-{}", uuid::Uuid::new_v4());
        pubsub.subscribe(subid.clone(), filter_json)?;
        Ok(subid)
    }

    pub fn from_user(
        user_pubkey: PublicKey,
        event_tx: &crossbeam_channel::Sender<crate::SessionManagerEvent>,
    ) -> Result<()> {
        let _ = Self::from_user_with_pubsub(user_pubkey, event_tx)?;
        Ok(())
    }

    pub fn process_invite_response(
        &self,
        event: &nostr::Event,
        _inviter_private_key: [u8; 32],
    ) -> Result<Option<InviteResponse>> {
        let inviter_ephemeral_private_key = self
            .inviter_ephemeral_private_key
            .ok_or(Error::Invite("Ephemeral key not available".to_string()))?;

        let inviter_ephemeral_sk = nostr::SecretKey::from_slice(&inviter_ephemeral_private_key)?;
        let sender_pk = event.pubkey;
        let decrypted = nip44::decrypt(&inviter_ephemeral_sk, &sender_pk, &event.content)?;
        let inner_event: serde_json::Value = serde_json::from_str(&decrypted)?;

        let invitee_identity_hex = inner_event["pubkey"]
            .as_str()
            .ok_or(Error::Invite("Missing pubkey".to_string()))?;
        let invitee_identity = crate::utils::pubkey_from_hex(invitee_identity_hex)?;

        let inner_content = inner_event["content"]
            .as_str()
            .ok_or(Error::Invite("Missing content".to_string()))?;

        let conversation_key = nip44::v2::ConversationKey::new(self.shared_secret);
        let ciphertext_bytes = base64::engine::general_purpose::STANDARD
            .decode(inner_content)
            .map_err(|e| Error::Serialization(e.to_string()))?;
        let dh_encrypted_ciphertext = String::from_utf8(nip44::v2::decrypt_to_bytes(
            &conversation_key,
            &ciphertext_bytes,
        )?)
        .map_err(|e| Error::Serialization(e.to_string()))?;

        // Decrypt the DH-encrypted layer using inviter's key
        let inviter_sk = nostr::SecretKey::from_slice(&_inviter_private_key)?;
        let dh_decrypted =
            nip44::decrypt(&inviter_sk, &invitee_identity, &dh_encrypted_ciphertext)?;

        let payload: serde_json::Value = match serde_json::from_str(&dh_decrypted) {
            Ok(p) => p,
            Err(_) => {
                // Fallback: treat as raw hex session key
                let invitee_session_pubkey = crate::utils::pubkey_from_hex(&dh_decrypted)?;
                let session = Session::init(
                    invitee_session_pubkey,
                    inviter_ephemeral_private_key,
                    false, // Inviter is non-initiator, must receive first message to initialize ratchet
                    self.shared_secret,
                    Some(event.id.to_string()),
                )?;
                return Ok(Some(InviteResponse {
                    session,
                    invitee_identity,
                    device_id: None,
                    owner_public_key: None,
                }));
            }
        };

        let invitee_session_key_hex = payload["sessionKey"]
            .as_str()
            .ok_or(Error::Invite("Missing sessionKey".to_string()))?;
        let invitee_session_pubkey = crate::utils::pubkey_from_hex(invitee_session_key_hex)?;
        let device_id = payload["deviceId"].as_str().map(String::from);
        let owner_public_key = payload["ownerPublicKey"]
            .as_str()
            .and_then(|s| crate::utils::pubkey_from_hex(s).ok());

        let session = Session::init(
            invitee_session_pubkey,
            inviter_ephemeral_private_key,
            false, // Inviter is non-initiator, must receive first message to initialize ratchet
            self.shared_secret,
            Some(event.id.to_string()),
        )?;

        Ok(Some(InviteResponse {
            session,
            invitee_identity,
            device_id,
            owner_public_key,
        }))
    }
}
