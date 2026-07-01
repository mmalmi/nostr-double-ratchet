use crate::{DevicePubkey, Error, Invite, ProtocolContext, Result, UnixSeconds};
use nostr::{Keys, PublicKey, SecretKey};
use rand::rngs::StdRng;
use rand::SeedableRng;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLinkRequest {
    pub request_pubkey: PublicKey,
    pub device_app_key_pubkey: PublicKey,
    pub request_secret: String,
}

pub fn encode_compact_device_link_request(
    device_app_key_pubkey: PublicKey,
    request_secret: &str,
) -> Result<String> {
    let secret = parse_secret_hex(request_secret)?;
    Ok(format!(
        "{}.{}",
        device_app_key_pubkey.to_hex(),
        hex::encode(secret)
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
    if parts.next().is_some() {
        return Err(Error::Parse(
            "Invalid compact device link request".to_string(),
        ));
    }

    let device_app_key_pubkey = PublicKey::parse(device_app_key_pubkey)
        .map_err(|error| Error::Parse(format!("Invalid device app key pubkey: {error}")))?;
    let request_secret_bytes = parse_secret_hex(request_secret)?;
    let request_secret_key = SecretKey::from_slice(&request_secret_bytes)?;
    let request_pubkey = Keys::new(request_secret_key).public_key();

    Ok(DeviceLinkRequest {
        request_pubkey,
        device_app_key_pubkey,
        request_secret: hex::encode(request_secret_bytes),
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
