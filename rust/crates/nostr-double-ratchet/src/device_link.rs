use crate::{DevicePubkey, Error, Invite, ProtocolContext, Result, UnixSeconds};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use nostr::{Keys, PublicKey, SecretKey};
use rand::rngs::StdRng;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLinkRequest {
    pub request_pubkey: PublicKey,
    pub device_app_key_pubkey: PublicKey,
    pub request_secret: String,
    pub requested_at: Option<u64>,
    pub device_label: Option<String>,
    pub client_label: Option<String>,
}

pub fn encode_compact_device_link_request(
    device_app_key_pubkey: PublicKey,
    request_secret: &str,
    device_label: Option<&str>,
    client_label: Option<&str>,
    requested_at: Option<u64>,
) -> Result<String> {
    let secret = parse_secret_hex(request_secret)?;
    let metadata = CompactDeviceLinkMetadata {
        v: 1,
        requested_at,
        device_label: normalize_label(device_label),
        client_label: normalize_label(client_label),
    };
    let metadata = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&metadata)?);
    Ok(format!(
        "{}.{}.{}",
        device_app_key_pubkey.to_hex(),
        hex::encode(secret),
        metadata
    ))
}

pub fn parse_compact_device_link_request(input: &str) -> Result<DeviceLinkRequest> {
    let mut parts = input.trim().split('.');
    let Some(device_app_key_pubkey) = parts.next() else {
        return Err(Error::Parse("Missing device app key pubkey".to_string()));
    };
    let Some(request_secret) = parts.next() else {
        return Err(Error::Parse("Missing device link secret".to_string()));
    };
    let Some(metadata) = parts.next() else {
        return Err(Error::Parse("Missing device link metadata".to_string()));
    };
    if parts.next().is_some() {
        return Err(Error::Parse(
            "Invalid compact device link request".to_string(),
        ));
    }

    let device_app_key_pubkey = PublicKey::parse(device_app_key_pubkey)
        .map_err(|error| Error::Parse(format!("Invalid device app key pubkey: {error}")))?;
    let request_secret_bytes = parse_secret_hex(request_secret)?;
    let metadata = parse_device_link_metadata(metadata)?;
    let request_secret_key = SecretKey::from_slice(&request_secret_bytes)?;
    let request_pubkey = Keys::new(request_secret_key).public_key();

    Ok(DeviceLinkRequest {
        request_pubkey,
        device_app_key_pubkey,
        request_secret: hex::encode(request_secret_bytes),
        requested_at: metadata.requested_at,
        device_label: metadata.device_label,
        client_label: metadata.client_label,
    })
}

pub fn deterministic_link_invite_for_device_link_request(
    request: &DeviceLinkRequest,
) -> Result<Invite> {
    deterministic_link_invite_for_device(
        request.device_app_key_pubkey,
        request.request_secret.as_str(),
    )
}

pub fn deterministic_link_invite_for_device(
    device_app_key_pubkey: PublicKey,
    request_secret: &str,
) -> Result<Invite> {
    let seed = parse_secret_hex(request_secret)?;
    let mut rng = StdRng::from_seed(seed);
    let mut ctx = ProtocolContext::new(UnixSeconds(0), &mut rng);
    let mut invite = Invite::create_new_with_context(
        &mut ctx,
        DevicePubkey::from_bytes(device_app_key_pubkey.to_bytes()),
        None,
        Some(1),
    )?;
    invite.purpose = Some("link".to_string());
    invite.device_id = Some(device_app_key_pubkey.to_hex());
    Ok(invite)
}

fn parse_secret_hex(secret: &str) -> Result<[u8; 32]> {
    let secret = secret.trim().to_ascii_lowercase();
    if secret.len() != 64 {
        return Err(Error::Parse(
            "Device link secret must be 32 bytes".to_string(),
        ));
    }
    let bytes = hex::decode(secret)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::Parse("Device link secret must be 32 bytes".to_string()))?;
    SecretKey::from_slice(&bytes)?;
    Ok(bytes)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactDeviceLinkMetadata {
    v: u8,
    #[serde(
        rename = "requestedAt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    requested_at: Option<u64>,
    #[serde(
        rename = "deviceLabel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    device_label: Option<String>,
    #[serde(
        rename = "clientLabel",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    client_label: Option<String>,
}

fn parse_device_link_metadata(input: &str) -> Result<CompactDeviceLinkMetadata> {
    if input.is_empty()
        || !input
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(Error::Parse("Invalid device link metadata".to_string()));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|error| Error::Parse(format!("Invalid device link metadata: {error}")))?;
    let metadata: CompactDeviceLinkMetadata = serde_json::from_slice(&bytes)?;
    if metadata.v != 1 {
        return Err(Error::Parse("Unsupported device link metadata".to_string()));
    }
    Ok(CompactDeviceLinkMetadata {
        v: 1,
        requested_at: metadata.requested_at,
        device_label: normalize_label(metadata.device_label.as_deref()),
        client_label: normalize_label(metadata.client_label.as_deref()),
    })
}

fn normalize_label(label: Option<&str>) -> Option<String> {
    let normalized = label?.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized.chars().take(96).collect())
}
