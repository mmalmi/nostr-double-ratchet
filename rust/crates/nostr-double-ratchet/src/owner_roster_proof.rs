use crate::{DevicePubkey, DeviceRoster, DomainError, OwnerPubkey, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OwnerRosterProof {
    pub owner_pubkey: OwnerPubkey,
    pub roster: DeviceRoster,
    pub raw_proof: String,
}

impl OwnerRosterProof {
    pub fn new(owner_pubkey: OwnerPubkey, roster: DeviceRoster, raw_proof: String) -> Self {
        Self {
            owner_pubkey,
            roster,
            raw_proof,
        }
    }

    pub fn ensure_authorizes_device(&self, device_pubkey: DevicePubkey) -> Result<()> {
        if self.roster.get_device(&device_pubkey).is_none() {
            return Err(DomainError::InvalidState(
                "owner roster proof does not authorize device".to_string(),
            )
            .into());
        }
        Ok(())
    }
}
