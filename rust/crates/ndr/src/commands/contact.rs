use anyhow::Result;

use crate::output::Output;
use crate::storage::Storage;

pub async fn add(npub_or_hex: &str, name: &str, storage: &Storage, output: &Output) -> Result<()> {
    // Accept hex pubkey and convert to npub
    let npub = if npub_or_hex.starts_with("npub1") {
        npub_or_hex.to_string()
    } else if npub_or_hex.len() == 64 && npub_or_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let pk = nostr::PublicKey::from_hex(npub_or_hex)?;
        nostr::ToBech32::to_bech32(&pk)?
    } else {
        anyhow::bail!("Invalid pubkey: must be npub or 64-char hex");
    };

    storage.add_contact(&npub, name)?;
    output.success(
        "contact_add",
        serde_json::json!({
            "npub": npub,
            "name": name,
        }),
    );
    Ok(())
}

pub async fn list(storage: &Storage, output: &Output) -> Result<()> {
    let contacts = storage.list_contacts()?;
    output.success(
        "contact_list",
        serde_json::json!({
            "contacts": contacts.iter().map(|(npub, name)| serde_json::json!({
                "npub": npub,
                "name": name,
            })).collect::<Vec<_>>(),
        }),
    );
    Ok(())
}

pub async fn remove(name: &str, storage: &Storage, output: &Output) -> Result<()> {
    if storage.remove_contact(name)? {
        output.success_message("contact_remove", &format!("Removed contact '{}'", name));
    } else {
        anyhow::bail!("Contact '{}' not found", name);
    }
    Ok(())
}
