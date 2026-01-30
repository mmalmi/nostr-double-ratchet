use anyhow::Result;
use serde::Serialize;

use crate::config::Config;
use crate::output::Output;
use crate::storage::{Storage, StoredGroup};

#[derive(Serialize)]
struct GroupList {
    groups: Vec<GroupInfo>,
}

#[derive(Serialize)]
struct GroupInfo {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    picture: Option<String>,
    members: Vec<String>,
    admins: Vec<String>,
    created_at: u64,
}

impl From<&nostr_double_ratchet::group::GroupData> for GroupInfo {
    fn from(g: &nostr_double_ratchet::group::GroupData) -> Self {
        GroupInfo {
            id: g.id.clone(),
            name: g.name.clone(),
            description: g.description.clone(),
            picture: g.picture.clone(),
            members: g.members.clone(),
            admins: g.admins.clone(),
            created_at: g.created_at,
        }
    }
}

pub async fn create(
    name: &str,
    members: &[String],
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let my_pubkey = config.public_key()?;
    let member_refs: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
    let group_data = nostr_double_ratchet::group::create_group_data(name, &my_pubkey, &member_refs);

    let stored = StoredGroup {
        data: group_data,
    };
    storage.save_group(&stored)?;

    output.success("group.create", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn list(storage: &Storage, output: &Output) -> Result<()> {
    let groups = storage.list_groups()?;
    let infos: Vec<GroupInfo> = groups.iter().map(|g| GroupInfo::from(&g.data)).collect();
    output.success("group.list", GroupList { groups: infos });
    Ok(())
}

pub async fn show(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;
    output.success("group.show", GroupInfo::from(&group.data));
    Ok(())
}

pub async fn delete(id: &str, storage: &Storage, output: &Output) -> Result<()> {
    if storage.delete_group(id)? {
        output.success_message("group.delete", &format!("Deleted group {}", id));
    } else {
        anyhow::bail!("Group not found: {}", id);
    }
    Ok(())
}

pub async fn update(
    id: &str,
    name: Option<&str>,
    description: Option<&str>,
    picture: Option<&str>,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updates = nostr_double_ratchet::group::GroupUpdate {
        name: name.map(|s| s.to_string()),
        description: description.map(|s| s.to_string()),
        picture: picture.map(|s| s.to_string()),
    };

    let updated = nostr_double_ratchet::group::update_group_data(&group.data, &updates, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Permission denied: not an admin"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    output.success("group.update", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn add_member(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::add_group_member(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot add member: not admin or already a member"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    output.success("group.add-member", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn remove_member(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::remove_group_member(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot remove member: not admin, not a member, or trying to remove self"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    output.success("group.remove-member", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn add_admin(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::add_group_admin(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot add admin: not admin, not a member, or already an admin"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    output.success("group.add-admin", GroupInfo::from(&stored.data));
    Ok(())
}

pub async fn remove_admin(
    id: &str,
    pubkey: &str,
    config: &Config,
    storage: &Storage,
    output: &Output,
) -> Result<()> {
    let group = storage
        .get_group(id)?
        .ok_or_else(|| anyhow::anyhow!("Group not found: {}", id))?;

    let my_pubkey = config.public_key()?;
    let updated = nostr_double_ratchet::group::remove_group_admin(&group.data, pubkey, &my_pubkey)
        .ok_or_else(|| anyhow::anyhow!("Cannot remove admin: not admin, target not admin, or would remove last admin"))?;

    let stored = StoredGroup { data: updated };
    storage.save_group(&stored)?;

    output.success("group.remove-admin", GroupInfo::from(&stored.data));
    Ok(())
}
