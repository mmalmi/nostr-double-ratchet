use crate::{
    device_pubkey_from_secret_bytes, random_secret_key_bytes, DevicePubkey, DomainError,
    GroupCreateResult, GroupIncomingEvent, GroupManagerSnapshot, GroupPairwiseCommand,
    GroupPayloadCodec, GroupPendingFanout, GroupPreparedPublish, GroupPreparedSend, GroupProtocol,
    GroupReceivedMessage, GroupSenderKeyHandleResult, GroupSenderKeyMessage,
    GroupSenderKeyMessageEnvelope, GroupSenderKeyPlaintext, GroupSenderKeyRecordSnapshot,
    GroupSnapshot, OwnerPubkey, ProtocolContext, Result, SenderEventPubkey, SenderKeyDistribution,
    SenderKeyMessageContent, SenderKeyState, SessionManager, UnixSeconds,
};
use rand::{CryptoRng, RngCore};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct GroupManager<C> {
    payload_codec: C,
    local_owner_pubkey: OwnerPubkey,
    groups: BTreeMap<String, GroupRecord>,
    sender_keys: BTreeMap<SenderKeyRecordId, SenderKeyRecord>,
    sender_event_index: BTreeMap<SenderEventPubkey, SenderKeyRecordId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupRecord {
    group_id: String,
    protocol: GroupProtocol,
    name: String,
    created_by: OwnerPubkey,
    members: BTreeSet<OwnerPubkey>,
    admins: BTreeSet<OwnerPubkey>,
    revision: u64,
    created_at: UnixSeconds,
    updated_at: UnixSeconds,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SenderKeyRecordId {
    group_id: String,
    sender_owner: OwnerPubkey,
    sender_device: DevicePubkey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SenderKeyRecord {
    group_id: String,
    sender_owner: OwnerPubkey,
    sender_device: DevicePubkey,
    sender_event_pubkey: SenderEventPubkey,
    sender_event_secret_key: Option<[u8; 32]>,
    latest_key_id: Option<u32>,
    states: BTreeMap<u32, SenderKeyState>,
}

impl<C> GroupManager<C>
where
    C: GroupPayloadCodec,
{
    pub fn new_with_payload_codec(local_owner_pubkey: OwnerPubkey, payload_codec: C) -> Self {
        Self {
            payload_codec,
            local_owner_pubkey,
            groups: BTreeMap::new(),
            sender_keys: BTreeMap::new(),
            sender_event_index: BTreeMap::new(),
        }
    }

    pub fn is_pairwise_payload(&self, payload: &[u8]) -> bool {
        self.payload_codec.is_pairwise_payload(payload)
    }

    pub fn from_snapshot_with_payload_codec(
        snapshot: GroupManagerSnapshot,
        payload_codec: C,
    ) -> Result<Self> {
        let mut groups = BTreeMap::new();
        for group in snapshot.groups {
            let record = GroupRecord::from_snapshot(group)?;
            if groups.insert(record.group_id.clone(), record).is_some() {
                return Err(group_error("duplicate group id in snapshot"));
            }
        }
        let mut sender_keys = BTreeMap::new();
        let mut sender_event_index = BTreeMap::new();
        for snapshot in snapshot.sender_keys {
            let record = SenderKeyRecord::from_snapshot(snapshot)?;
            let id = record.id();
            if sender_keys.insert(id.clone(), record.clone()).is_some() {
                return Err(group_error("duplicate sender-key record in snapshot"));
            }
            if sender_event_index
                .insert(record.sender_event_pubkey, id)
                .is_some()
            {
                return Err(group_error("duplicate sender-event pubkey in snapshot"));
            }
        }
        Ok(Self {
            payload_codec,
            local_owner_pubkey: snapshot.local_owner_pubkey,
            groups,
            sender_keys,
            sender_event_index,
        })
    }

    pub fn snapshot(&self) -> GroupManagerSnapshot {
        GroupManagerSnapshot {
            local_owner_pubkey: self.local_owner_pubkey,
            groups: self.groups.values().map(GroupRecord::snapshot).collect(),
            sender_keys: self
                .sender_keys
                .values()
                .map(SenderKeyRecord::snapshot)
                .collect(),
        }
    }

    pub fn group(&self, group_id: &str) -> Option<GroupSnapshot> {
        self.groups.get(group_id).map(GroupRecord::snapshot)
    }

    pub fn groups(&self) -> Vec<GroupSnapshot> {
        self.groups.values().map(GroupRecord::snapshot).collect()
    }

    pub fn known_sender_event_pubkeys(&self) -> Vec<SenderEventPubkey> {
        self.sender_event_index.keys().copied().collect()
    }

    pub fn create_group<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        name: String,
        initial_members: Vec<OwnerPubkey>,
    ) -> Result<GroupCreateResult>
    where
        R: RngCore + CryptoRng,
    {
        self.create_group_with_protocol(
            session_manager,
            ctx,
            name,
            initial_members,
            GroupProtocol::pairwise_fanout_v1(),
        )
    }

    pub fn create_group_with_protocol<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        name: String,
        initial_members: Vec<OwnerPubkey>,
        protocol: GroupProtocol,
    ) -> Result<GroupCreateResult>
    where
        R: RngCore + CryptoRng,
    {
        let member_set = validate_unique_owners(&initial_members, "initial members")?;
        if member_set.contains(&self.local_owner_pubkey) {
            return Err(group_error("local owner is added automatically"));
        }
        validate_supported_protocol(protocol)?;

        let group_id = random_group_id(ctx);
        let mut members = member_set;
        members.insert(self.local_owner_pubkey);

        let mut admins = BTreeSet::new();
        admins.insert(self.local_owner_pubkey);

        let record = GroupRecord {
            group_id: group_id.clone(),
            protocol,
            name,
            created_by: self.local_owner_pubkey,
            members,
            admins,
            revision: 1,
            created_at: ctx.now,
            updated_at: ctx.now,
        };
        let payload = record.create_payload();
        let recipients = record.remote_members(self.local_owner_pubkey);
        let prepared = GroupPreparedSend {
            group_id: group_id.clone(),
            remote: self.fanout_payload(session_manager, ctx, &group_id, recipients, &payload)?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &record)?,
        };
        let prepared = if protocol.is_sender_key_v1() {
            self.prepare_sender_key_bootstrap(session_manager, ctx, &record, prepared)?
        } else {
            prepared
        };
        let snapshot = record.snapshot();

        self.groups.insert(group_id, record);

        Ok(GroupCreateResult {
            group: snapshot,
            prepared,
        })
    }

    pub fn retry_create_group<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        recipients: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let record = self.group_record(group_id)?.clone();
        record.ensure_admin(self.local_owner_pubkey)?;

        let recipients = validate_unique_owners(&recipients, "recipients")?
            .into_iter()
            .filter(|owner| *owner != self.local_owner_pubkey)
            .collect::<Vec<_>>();
        for recipient in &recipients {
            record.ensure_member(*recipient)?;
        }

        let prepared = GroupPreparedSend {
            group_id: record.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &record.group_id,
                recipients,
                &record.create_payload(),
            )?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &record)?,
        };
        if record.protocol.is_sender_key_v1() {
            self.prepare_sender_key_bootstrap(session_manager, ctx, &record, prepared)
        } else {
            Ok(prepared)
        }
    }

    pub fn send_message<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        body: Vec<u8>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let record = self.group_record(group_id)?.clone();
        record.ensure_member(self.local_owner_pubkey)?;
        if record.protocol.is_sender_key_v1() {
            return self.send_sender_key_message(session_manager, ctx, &record, body);
        }
        let payload = GroupPairwiseCommand::GroupMessage {
            group_id: record.group_id.clone(),
            revision: record.revision,
            body,
        };

        let mut local_sibling = self.local_sibling_sync(session_manager, ctx, &record)?;
        let sibling_message =
            self.local_sibling_payload(session_manager, ctx, &record.group_id, &payload)?;
        merge_group_prepared_publish(&mut local_sibling, sibling_message);

        Ok(GroupPreparedSend {
            group_id: record.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &record.group_id,
                record.remote_members(self.local_owner_pubkey),
                &payload,
            )?,
            local_sibling,
        })
    }

    pub fn update_name<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        name: String,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_rename(
            self.local_owner_pubkey,
            name.clone(),
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let payload = GroupPairwiseCommand::RenameGroup {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            name,
        };

        let prepared = GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &current.group_id,
                next.remote_members(self.local_owner_pubkey),
                &payload,
            )?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &next)?,
        };
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn retry_update_name<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let current = self.group_record(group_id)?.clone();
        current.ensure_admin(self.local_owner_pubkey)?;
        let (base_revision, new_revision) = current.retry_delta_revisions("rename")?;

        let payload = GroupPairwiseCommand::RenameGroup {
            group_id: current.group_id.clone(),
            base_revision,
            new_revision,
            name: current.name.clone(),
        };

        Ok(GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &current.group_id,
                current.remote_members(self.local_owner_pubkey),
                &payload,
            )?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &current)?,
        })
    }

    pub fn add_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let additions = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_add_members(
            self.local_owner_pubkey,
            &additions,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let delta_payload = GroupPairwiseCommand::AddMembers {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            members: additions.iter().copied().collect(),
        };
        let bootstrap_payload = next.create_payload();

        let existing_recipients: Vec<_> = current
            .remote_members(self.local_owner_pubkey)
            .into_iter()
            .filter(|owner| !additions.contains(owner))
            .collect();
        let new_recipients: Vec<_> = additions
            .iter()
            .copied()
            .filter(|owner| *owner != self.local_owner_pubkey)
            .collect();

        let mut remote = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            existing_recipients,
            &delta_payload,
        )?;
        let bootstrapped = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            new_recipients,
            &bootstrap_payload,
        )?;
        merge_group_prepared_publish(&mut remote, bootstrapped);

        let mut prepared = GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &next)?,
        };
        if next.protocol.is_sender_key_v1() {
            prepared = self.prepare_sender_key_bootstrap(session_manager, ctx, &next, prepared)?;
        }
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn retry_add_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let additions = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        current.ensure_admin(self.local_owner_pubkey)?;
        let (base_revision, new_revision) = current.retry_delta_revisions("add members")?;
        for owner in &additions {
            current.ensure_member(*owner)?;
        }

        let delta_payload = GroupPairwiseCommand::AddMembers {
            group_id: current.group_id.clone(),
            base_revision,
            new_revision,
            members: additions.iter().copied().collect(),
        };
        let bootstrap_payload = current.create_payload();

        let existing_recipients: Vec<_> = current
            .remote_members(self.local_owner_pubkey)
            .into_iter()
            .filter(|owner| !additions.contains(owner))
            .collect();
        let new_recipients: Vec<_> = additions
            .iter()
            .copied()
            .filter(|owner| *owner != self.local_owner_pubkey)
            .collect();

        let mut remote = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            existing_recipients,
            &delta_payload,
        )?;
        let bootstrapped = self.fanout_payload(
            session_manager,
            ctx,
            &current.group_id,
            new_recipients,
            &bootstrap_payload,
        )?;
        merge_group_prepared_publish(&mut remote, bootstrapped);
        let prepared = GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &current)?,
        };
        if current.protocol.is_sender_key_v1() {
            self.prepare_sender_key_bootstrap(session_manager, ctx, &current, prepared)
        } else {
            Ok(prepared)
        }
    }

    pub fn remove_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let removals = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_remove_members(
            self.local_owner_pubkey,
            &removals,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let payload = GroupPairwiseCommand::RemoveMembers {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            members: removals.iter().copied().collect(),
        };

        let mut prepared = GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &current.group_id,
                current.remote_members(self.local_owner_pubkey),
                &payload,
            )?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &next)?,
        };
        if next.protocol.is_sender_key_v1() {
            prepared = self.prepare_sender_key_rotation(session_manager, ctx, &next, prepared)?;
        }
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn retry_remove_members<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        members: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let removals = validate_unique_owners(&members, "members")?;
        let current = self.group_record(group_id)?.clone();
        current.ensure_admin(self.local_owner_pubkey)?;
        let (base_revision, new_revision) = current.retry_delta_revisions("remove members")?;
        for owner in &removals {
            if current.members.contains(owner) {
                return Err(group_error(format!(
                    "owner {owner} should already be removed before retrying removal"
                )));
            }
        }

        let payload = GroupPairwiseCommand::RemoveMembers {
            group_id: current.group_id.clone(),
            base_revision,
            new_revision,
            members: removals.iter().copied().collect(),
        };

        let mut recipients = current
            .remote_members(self.local_owner_pubkey)
            .into_iter()
            .collect::<BTreeSet<_>>();
        recipients.extend(
            removals
                .iter()
                .copied()
                .filter(|owner| *owner != self.local_owner_pubkey),
        );

        let prepared = GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &current.group_id,
                recipients.into_iter().collect(),
                &payload,
            )?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &current)?,
        };
        if current.protocol.is_sender_key_v1() {
            self.prepare_sender_key_bootstrap(session_manager, ctx, &current, prepared)
        } else {
            Ok(prepared)
        }
    }

    pub fn add_admins<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        admins: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let additions = validate_unique_owners(&admins, "admins")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_add_admins(
            self.local_owner_pubkey,
            &additions,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let payload = GroupPairwiseCommand::AddAdmins {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            admins: additions.iter().copied().collect(),
        };

        let prepared = GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &current.group_id,
                next.remote_members(self.local_owner_pubkey),
                &payload,
            )?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &next)?,
        };
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn remove_admins<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        group_id: &str,
        admins: Vec<OwnerPubkey>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let removals = validate_unique_owners(&admins, "admins")?;
        let current = self.group_record(group_id)?.clone();
        let mut next = current.clone();
        next.apply_remove_admins(
            self.local_owner_pubkey,
            &removals,
            current.revision,
            current.revision + 1,
            ctx.now,
        )?;

        let payload = GroupPairwiseCommand::RemoveAdmins {
            group_id: current.group_id.clone(),
            base_revision: current.revision,
            new_revision: next.revision,
            admins: removals.iter().copied().collect(),
        };

        let prepared = GroupPreparedSend {
            group_id: current.group_id.clone(),
            remote: self.fanout_payload(
                session_manager,
                ctx,
                &current.group_id,
                next.remote_members(self.local_owner_pubkey),
                &payload,
            )?,
            local_sibling: self.local_sibling_sync(session_manager, ctx, &next)?,
        };
        self.groups.insert(current.group_id.clone(), next);
        Ok(prepared)
    }

    pub fn handle_incoming(
        &mut self,
        sender_owner: OwnerPubkey,
        payload: &[u8],
    ) -> Result<Option<GroupIncomingEvent>> {
        self.handle_pairwise_payload_inner(sender_owner, None, payload)
    }

    pub fn handle_pairwise_payload(
        &mut self,
        sender_owner: OwnerPubkey,
        sender_device: DevicePubkey,
        payload: &[u8],
    ) -> Result<Option<GroupIncomingEvent>> {
        self.handle_pairwise_payload_inner(sender_owner, Some(sender_device), payload)
    }

    fn handle_pairwise_payload_inner(
        &mut self,
        sender_owner: OwnerPubkey,
        sender_device: Option<DevicePubkey>,
        payload: &[u8],
    ) -> Result<Option<GroupIncomingEvent>> {
        let Some(command) = self.payload_codec.decode_pairwise_command(payload)? else {
            return Ok(None);
        };

        let event = match command {
            GroupPairwiseCommand::CreateGroup {
                group_id,
                protocol,
                base_revision,
                new_revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            } => {
                if validate_supported_protocol(protocol).is_err() {
                    return Ok(None);
                }
                let record = GroupRecord::from_create_payload(
                    group_id,
                    protocol,
                    name,
                    created_by,
                    members,
                    admins,
                    new_revision,
                    created_at,
                    updated_at,
                    sender_owner,
                )?;
                if let Some(existing) = self.groups.get(&record.group_id) {
                    if existing.protocol != record.protocol {
                        return Err(group_error(format!(
                            "group `{}` protocol mismatch: expected {:?}, got {:?}",
                            record.group_id, existing.protocol, record.protocol
                        )));
                    }
                    if existing == &record {
                        GroupIncomingEvent::MetadataUpdated(existing.snapshot())
                    } else {
                        return Err(group_error(format!(
                            "group `{}` already exists",
                            record.group_id
                        )));
                    }
                } else {
                    if base_revision != 0 {
                        return Err(group_error("create group base revision must be 0"));
                    }
                    let snapshot = record.snapshot();
                    self.groups.insert(record.group_id.clone(), record);
                    GroupIncomingEvent::MetadataUpdated(snapshot)
                }
            }
            GroupPairwiseCommand::SyncGroup {
                group_id,
                protocol,
                revision,
                name,
                created_by,
                members,
                admins,
                created_at,
                updated_at,
            } => {
                if validate_supported_protocol(protocol).is_err() {
                    return Ok(None);
                }
                let record = GroupRecord::from_sync_payload(
                    group_id,
                    protocol,
                    name,
                    created_by,
                    members,
                    admins,
                    revision,
                    created_at,
                    updated_at,
                    sender_owner,
                    self.local_owner_pubkey,
                )?;
                if let Some(existing) = self.groups.get(&record.group_id) {
                    if existing.protocol != record.protocol {
                        return Err(group_error(format!(
                            "group `{}` protocol mismatch: expected {:?}, got {:?}",
                            record.group_id, existing.protocol, record.protocol
                        )));
                    }
                    if existing == &record || existing.revision > record.revision {
                        GroupIncomingEvent::MetadataUpdated(existing.snapshot())
                    } else {
                        let snapshot = record.snapshot();
                        self.groups.insert(record.group_id.clone(), record);
                        GroupIncomingEvent::MetadataUpdated(snapshot)
                    }
                } else {
                    let snapshot = record.snapshot();
                    self.groups.insert(record.group_id.clone(), record);
                    GroupIncomingEvent::MetadataUpdated(snapshot)
                }
            }
            GroupPairwiseCommand::RenameGroup {
                group_id,
                base_revision,
                new_revision,
                name,
            } => {
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_rename(&name, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else if !group.should_apply_delta_revision(base_revision)? {
                    return Ok(None);
                } else {
                    group.apply_rename(
                        sender_owner,
                        name,
                        base_revision,
                        new_revision,
                        group.updated_at,
                    )?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwiseCommand::AddMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => {
                let additions = validate_unique_owners(&members, "members")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_added_members(&additions, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else if !group.should_apply_delta_revision(base_revision)? {
                    return Ok(None);
                } else {
                    group.apply_add_members(
                        sender_owner,
                        &additions,
                        base_revision,
                        new_revision,
                        group.updated_at,
                    )?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwiseCommand::RemoveMembers {
                group_id,
                base_revision,
                new_revision,
                members,
            } => {
                let removals = validate_unique_owners(&members, "members")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_removed_members(&removals, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else if !group.should_apply_delta_revision(base_revision)? {
                    return Ok(None);
                } else {
                    group.apply_remove_members(
                        sender_owner,
                        &removals,
                        base_revision,
                        new_revision,
                        group.updated_at,
                    )?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwiseCommand::AddAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => {
                let additions = validate_unique_owners(&admins, "admins")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_added_admins(&additions, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else if !group.should_apply_delta_revision(base_revision)? {
                    return Ok(None);
                } else {
                    group.apply_add_admins(
                        sender_owner,
                        &additions,
                        base_revision,
                        new_revision,
                        group.updated_at,
                    )?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwiseCommand::RemoveAdmins {
                group_id,
                base_revision,
                new_revision,
                admins,
            } => {
                let removals = validate_unique_owners(&admins, "admins")?;
                let group = self.group_record_mut(&group_id)?;
                if group.reflects_removed_admins(&removals, new_revision) {
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                } else if !group.should_apply_delta_revision(base_revision)? {
                    return Ok(None);
                } else {
                    group.apply_remove_admins(
                        sender_owner,
                        &removals,
                        base_revision,
                        new_revision,
                        group.updated_at,
                    )?;
                    GroupIncomingEvent::MetadataUpdated(group.snapshot())
                }
            }
            GroupPairwiseCommand::GroupMessage {
                group_id,
                revision,
                body,
            } => {
                let group = self.group_record(&group_id)?;
                group.ensure_member(sender_owner)?;
                if revision > group.revision {
                    return Err(pending_group_revision_error(
                        group_id,
                        group.revision,
                        revision,
                    ));
                }
                if revision < group.revision {
                    return Ok(None);
                }
                GroupIncomingEvent::Message(GroupReceivedMessage {
                    group_id,
                    sender_owner,
                    sender_device,
                    body,
                    revision,
                })
            }
            GroupPairwiseCommand::SenderKeyDistribution { distribution } => {
                let Some(sender_device) = sender_device else {
                    return Err(group_error(
                        "sender-key distribution requires authenticated sender device",
                    ));
                };
                let group_id = distribution.group_id.clone();
                self.observe_sender_key_distribution(sender_owner, sender_device, distribution)?;
                let snapshot = self.group_record(&group_id)?.snapshot();
                GroupIncomingEvent::MetadataUpdated(snapshot)
            }
        };

        Ok(Some(event))
    }

    pub fn handle_sender_key_message(
        &mut self,
        message: GroupSenderKeyMessage,
    ) -> Result<GroupSenderKeyHandleResult> {
        let content = SenderKeyMessageContent {
            key_id: message.key_id,
            message_number: message.message_number,
            ciphertext: message.ciphertext,
        };
        let Some(id) = self
            .sender_event_index
            .get(&message.sender_event_pubkey)
            .cloned()
        else {
            return Ok(GroupSenderKeyHandleResult::PendingDistribution {
                group_id: message.group_id,
                sender_event_pubkey: message.sender_event_pubkey,
                key_id: message.key_id,
            });
        };
        if id.group_id != message.group_id {
            return Ok(GroupSenderKeyHandleResult::Ignored);
        }

        let group = self.group_record(&id.group_id)?.clone();
        if !group.protocol.is_sender_key_v1() || !group.members.contains(&id.sender_owner) {
            return Ok(GroupSenderKeyHandleResult::Ignored);
        }

        let record = self
            .sender_keys
            .get_mut(&id)
            .ok_or_else(|| group_error("sender-key index points to missing state"))?;
        let Some(state) = record.states.get_mut(&message.key_id) else {
            return Ok(GroupSenderKeyHandleResult::PendingDistribution {
                group_id: message.group_id,
                sender_event_pubkey: message.sender_event_pubkey,
                key_id: message.key_id,
            });
        };
        let plan = state.plan_decrypt(&content)?;
        let plaintext = plan.plaintext.clone();

        let Some(plaintext) = self.payload_codec.decode_sender_key_plaintext(&plaintext)? else {
            return Ok(GroupSenderKeyHandleResult::Ignored);
        };
        if plaintext.group_id != group.group_id {
            return Ok(GroupSenderKeyHandleResult::Ignored);
        }
        if plaintext.revision > group.revision {
            return Ok(GroupSenderKeyHandleResult::PendingRevision {
                group_id: group.group_id,
                current_revision: group.revision,
                required_revision: plaintext.revision,
            });
        }
        if plaintext.revision < group.revision {
            return Ok(GroupSenderKeyHandleResult::Ignored);
        }

        state.apply_decrypt(plan);

        Ok(GroupSenderKeyHandleResult::Event(
            GroupIncomingEvent::Message(GroupReceivedMessage {
                group_id: plaintext.group_id,
                sender_owner: id.sender_owner,
                sender_device: Some(id.sender_device),
                body: plaintext.body,
                revision: plaintext.revision,
            }),
        ))
    }

    fn local_sibling_sync<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        record: &GroupRecord,
    ) -> Result<GroupPreparedPublish>
    where
        R: RngCore + CryptoRng,
    {
        if !session_manager.has_authorized_local_siblings() {
            return Ok(GroupPreparedPublish::empty());
        }
        let payload = self
            .payload_codec
            .encode_pairwise_command(&record.sync_payload())?;
        self.local_sibling_payload_bytes(session_manager, ctx, payload)
    }

    fn local_sibling_payload<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        _group_id: &str,
        payload: &GroupPairwiseCommand,
    ) -> Result<GroupPreparedPublish>
    where
        R: RngCore + CryptoRng,
    {
        if !session_manager.has_authorized_local_siblings() {
            return Ok(GroupPreparedPublish::empty());
        }
        self.local_sibling_payload_bytes(
            session_manager,
            ctx,
            self.payload_codec.encode_pairwise_command(payload)?,
        )
    }

    fn local_sibling_payload_bytes<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        payload: Vec<u8>,
    ) -> Result<GroupPreparedPublish>
    where
        R: RngCore + CryptoRng,
    {
        let prepared =
            session_manager.prepare_local_sibling_send_reusing_sessions(ctx, payload.clone())?;
        let pending_fanouts = if prepared.relay_gaps.is_empty() {
            Vec::new()
        } else {
            vec![GroupPendingFanout::LocalSiblings { payload }]
        };
        Ok(GroupPreparedPublish {
            deliveries: prepared.deliveries,
            invite_responses: prepared.invite_responses,
            sender_key_messages: Vec::new(),
            relay_gaps: prepared.relay_gaps,
            pending_fanouts,
        })
    }

    fn fanout_payload<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        _group_id: &str,
        recipients: Vec<OwnerPubkey>,
        payload: &GroupPairwiseCommand,
    ) -> Result<GroupPreparedPublish>
    where
        R: RngCore + CryptoRng,
    {
        let mut prepared = GroupPreparedPublish::empty();
        let payload_bytes = self.payload_codec.encode_pairwise_command(payload)?;

        for recipient in recipients {
            let next =
                session_manager.prepare_remote_send(ctx, recipient, payload_bytes.clone())?;
            prepared.deliveries.extend(next.deliveries);
            prepared.invite_responses.extend(next.invite_responses);
            if !next.relay_gaps.is_empty() {
                prepared.pending_fanouts.push(GroupPendingFanout::Remote {
                    recipient_owner: recipient,
                    payload: payload_bytes.clone(),
                });
            }
            prepared.relay_gaps.extend(next.relay_gaps);
        }

        prepared.relay_gaps.sort();
        prepared.relay_gaps.dedup();
        Ok(prepared)
    }

    fn send_sender_key_message<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        record: &GroupRecord,
        body: Vec<u8>,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let mut remote = GroupPreparedPublish::empty();
        let mut local_sibling = self.local_sibling_sync(session_manager, ctx, record)?;
        let local_device = session_manager.local_device_pubkey();
        let (distribution, created) =
            self.ensure_local_sender_key_record(ctx, record, local_device, false)?;

        if created {
            remote = self.fanout_sender_key_distribution(
                session_manager,
                ctx,
                record.remote_members(self.local_owner_pubkey),
                &distribution,
            )?;
        }
        let sibling_distribution =
            self.local_sibling_sender_key_distribution(session_manager, ctx, &distribution)?;
        merge_group_prepared_publish(&mut local_sibling, sibling_distribution);

        let id = SenderKeyRecordId::new(
            record.group_id.clone(),
            self.local_owner_pubkey,
            local_device,
        );
        let sender_record = self
            .sender_keys
            .get_mut(&id)
            .ok_or_else(|| group_error("missing local sender-key record"))?;
        let key_id = sender_record
            .latest_key_id
            .ok_or_else(|| group_error("missing local sender-key id"))?;
        let state = sender_record
            .states
            .get_mut(&key_id)
            .ok_or_else(|| group_error("missing local sender-key state"))?;
        let plaintext =
            self.payload_codec
                .encode_sender_key_plaintext(&GroupSenderKeyPlaintext {
                    group_id: record.group_id.clone(),
                    revision: record.revision,
                    body,
                })?;
        let plan = state.plan_encrypt(&plaintext)?;
        let message_number = plan.message_number;
        let ciphertext = plan.ciphertext.clone();
        state.apply_encrypt(plan);
        let signer_secret_key = sender_record
            .sender_event_secret_key
            .ok_or_else(|| group_error("missing local sender-event secret key"))?;

        remote
            .sender_key_messages
            .push(GroupSenderKeyMessageEnvelope {
                group_id: record.group_id.clone(),
                sender_event_pubkey: sender_record.sender_event_pubkey,
                signer_secret_key,
                key_id,
                message_number,
                created_at: ctx.now,
                ciphertext,
            });

        Ok(GroupPreparedSend {
            group_id: record.group_id.clone(),
            remote,
            local_sibling,
        })
    }

    fn prepare_sender_key_bootstrap<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        record: &GroupRecord,
        mut prepared: GroupPreparedSend,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let local_device = session_manager.local_device_pubkey();
        let (distribution, _) =
            self.ensure_local_sender_key_record(ctx, record, local_device, false)?;
        let remote = self.fanout_sender_key_distribution(
            session_manager,
            ctx,
            record.remote_members(self.local_owner_pubkey),
            &distribution,
        )?;
        merge_group_prepared_publish(&mut prepared.remote, remote);
        let local =
            self.local_sibling_sender_key_distribution(session_manager, ctx, &distribution)?;
        merge_group_prepared_publish(&mut prepared.local_sibling, local);
        Ok(prepared)
    }

    fn prepare_sender_key_rotation<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        record: &GroupRecord,
        mut prepared: GroupPreparedSend,
    ) -> Result<GroupPreparedSend>
    where
        R: RngCore + CryptoRng,
    {
        let local_device = session_manager.local_device_pubkey();
        let (distribution, _) =
            self.ensure_local_sender_key_record(ctx, record, local_device, true)?;
        let remote = self.fanout_sender_key_distribution(
            session_manager,
            ctx,
            record.remote_members(self.local_owner_pubkey),
            &distribution,
        )?;
        merge_group_prepared_publish(&mut prepared.remote, remote);
        let local =
            self.local_sibling_sender_key_distribution(session_manager, ctx, &distribution)?;
        merge_group_prepared_publish(&mut prepared.local_sibling, local);
        Ok(prepared)
    }

    fn ensure_local_sender_key_record<R>(
        &mut self,
        ctx: &mut ProtocolContext<'_, R>,
        record: &GroupRecord,
        local_device: DevicePubkey,
        force_rotate: bool,
    ) -> Result<(SenderKeyDistribution, bool)>
    where
        R: RngCore + CryptoRng,
    {
        let id = SenderKeyRecordId::new(
            record.group_id.clone(),
            self.local_owner_pubkey,
            local_device,
        );
        let mut created_or_rotated = force_rotate;
        if !self.sender_keys.contains_key(&id) {
            let sender_event_secret_key = random_secret_key_bytes(ctx.rng)?;
            let sender_event_pubkey = device_pubkey_from_secret_bytes(&sender_event_secret_key)?;
            let sender_record = SenderKeyRecord {
                group_id: record.group_id.clone(),
                sender_owner: self.local_owner_pubkey,
                sender_device: local_device,
                sender_event_pubkey,
                sender_event_secret_key: Some(sender_event_secret_key),
                latest_key_id: None,
                states: BTreeMap::new(),
            };
            self.sender_event_index
                .insert(sender_event_pubkey, sender_record.id());
            self.sender_keys.insert(id.clone(), sender_record);
            created_or_rotated = true;
        }

        let sender_record = self
            .sender_keys
            .get_mut(&id)
            .ok_or_else(|| group_error("missing local sender-key record"))?;
        if force_rotate || sender_record.latest_key_id.is_none() {
            let key_id = random_key_id(ctx);
            let mut chain_key = [0u8; 32];
            ctx.rng.fill_bytes(&mut chain_key);
            sender_record
                .states
                .insert(key_id, SenderKeyState::new(key_id, chain_key, 0));
            sender_record.latest_key_id = Some(key_id);
            created_or_rotated = true;
        }

        let key_id = sender_record
            .latest_key_id
            .ok_or_else(|| group_error("missing local sender-key id"))?;
        let state = sender_record
            .states
            .get(&key_id)
            .ok_or_else(|| group_error("missing local sender-key state"))?;
        Ok((
            SenderKeyDistribution {
                group_id: record.group_id.clone(),
                key_id,
                sender_event_pubkey: sender_record.sender_event_pubkey,
                chain_key: state.chain_key(),
                iteration: state.iteration(),
                created_at: ctx.now,
            },
            created_or_rotated,
        ))
    }

    fn fanout_sender_key_distribution<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        recipients: Vec<OwnerPubkey>,
        distribution: &SenderKeyDistribution,
    ) -> Result<GroupPreparedPublish>
    where
        R: RngCore + CryptoRng,
    {
        self.fanout_payload(
            session_manager,
            ctx,
            &distribution.group_id,
            recipients,
            &GroupPairwiseCommand::SenderKeyDistribution {
                distribution: distribution.clone(),
            },
        )
    }

    fn local_sibling_sender_key_distribution<R>(
        &mut self,
        session_manager: &mut SessionManager,
        ctx: &mut ProtocolContext<'_, R>,
        distribution: &SenderKeyDistribution,
    ) -> Result<GroupPreparedPublish>
    where
        R: RngCore + CryptoRng,
    {
        self.local_sibling_payload(
            session_manager,
            ctx,
            &distribution.group_id,
            &GroupPairwiseCommand::SenderKeyDistribution {
                distribution: distribution.clone(),
            },
        )
    }

    fn observe_sender_key_distribution(
        &mut self,
        sender_owner: OwnerPubkey,
        sender_device: DevicePubkey,
        distribution: SenderKeyDistribution,
    ) -> Result<()> {
        let group = self.group_record(&distribution.group_id)?.clone();
        if !group.protocol.is_sender_key_v1() {
            return Ok(());
        }
        group.ensure_member(sender_owner)?;

        let id = SenderKeyRecordId::new(distribution.group_id.clone(), sender_owner, sender_device);
        let record = self
            .sender_keys
            .entry(id.clone())
            .or_insert_with(|| SenderKeyRecord {
                group_id: distribution.group_id.clone(),
                sender_owner,
                sender_device,
                sender_event_pubkey: distribution.sender_event_pubkey,
                sender_event_secret_key: None,
                latest_key_id: None,
                states: BTreeMap::new(),
            });
        if record.sender_event_pubkey != distribution.sender_event_pubkey {
            self.sender_event_index.remove(&record.sender_event_pubkey);
            record.sender_event_pubkey = distribution.sender_event_pubkey;
        }
        self.sender_event_index
            .insert(distribution.sender_event_pubkey, id);
        record.latest_key_id = Some(distribution.key_id);
        record.states.entry(distribution.key_id).or_insert_with(|| {
            SenderKeyState::new(
                distribution.key_id,
                distribution.chain_key,
                distribution.iteration,
            )
        });
        Ok(())
    }

    fn group_record(&self, group_id: &str) -> Result<&GroupRecord> {
        self.groups
            .get(group_id)
            .ok_or_else(|| group_error(format!("unknown group `{group_id}`")))
    }

    fn group_record_mut(&mut self, group_id: &str) -> Result<&mut GroupRecord> {
        self.groups
            .get_mut(group_id)
            .ok_or_else(|| group_error(format!("unknown group `{group_id}`")))
    }
}

impl<C> GroupManager<C>
where
    C: GroupPayloadCodec + Default,
{
    pub fn new(local_owner_pubkey: OwnerPubkey) -> Self {
        Self::new_with_payload_codec(local_owner_pubkey, C::default())
    }

    pub fn from_snapshot(snapshot: GroupManagerSnapshot) -> Result<Self> {
        Self::from_snapshot_with_payload_codec(snapshot, C::default())
    }
}

impl GroupRecord {
    fn from_snapshot(snapshot: GroupSnapshot) -> Result<Self> {
        let members = validate_unique_owners(&snapshot.members, "members")?;
        let admins = validate_unique_owners(&snapshot.admins, "admins")?;
        validate_supported_protocol(snapshot.protocol)?;
        validate_group_invariants(&members, &admins)?;

        Ok(Self {
            group_id: snapshot.group_id,
            protocol: snapshot.protocol,
            name: snapshot.name,
            created_by: snapshot.created_by,
            members,
            admins,
            revision: snapshot.revision,
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn from_create_payload(
        group_id: String,
        protocol: GroupProtocol,
        name: String,
        created_by: OwnerPubkey,
        members: Vec<OwnerPubkey>,
        admins: Vec<OwnerPubkey>,
        new_revision: u64,
        created_at: UnixSeconds,
        updated_at: UnixSeconds,
        sender_owner: OwnerPubkey,
    ) -> Result<Self> {
        let member_set = validate_unique_owners(&members, "members")?;
        let admin_set = validate_unique_owners(&admins, "admins")?;
        validate_supported_protocol(protocol)?;
        if created_by != sender_owner {
            return Err(group_error("create group sender must match created_by"));
        }
        if new_revision == 0 {
            return Err(group_error("create group revision must be at least 1"));
        }
        if !admin_set.contains(&sender_owner) {
            return Err(group_error("create group sender must be an admin"));
        }
        validate_group_invariants(&member_set, &admin_set)?;

        Ok(Self {
            group_id,
            protocol,
            name,
            created_by,
            members: member_set,
            admins: admin_set,
            revision: new_revision,
            created_at,
            updated_at,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn from_sync_payload(
        group_id: String,
        protocol: GroupProtocol,
        name: String,
        created_by: OwnerPubkey,
        members: Vec<OwnerPubkey>,
        admins: Vec<OwnerPubkey>,
        revision: u64,
        created_at: UnixSeconds,
        updated_at: UnixSeconds,
        sender_owner: OwnerPubkey,
        local_owner: OwnerPubkey,
    ) -> Result<Self> {
        let member_set = validate_unique_owners(&members, "members")?;
        let admin_set = validate_unique_owners(&admins, "admins")?;
        validate_supported_protocol(protocol)?;
        if sender_owner != local_owner {
            return Err(group_error("sync group sender must match local owner"));
        }
        if !member_set.contains(&sender_owner) {
            return Err(group_error("sync group sender must be a member"));
        }
        if revision == 0 {
            return Err(group_error("sync group revision must be at least 1"));
        }
        validate_group_invariants(&member_set, &admin_set)?;

        Ok(Self {
            group_id,
            protocol,
            name,
            created_by,
            members: member_set,
            admins: admin_set,
            revision,
            created_at,
            updated_at,
        })
    }

    fn snapshot(&self) -> GroupSnapshot {
        GroupSnapshot {
            group_id: self.group_id.clone(),
            protocol: self.protocol,
            name: self.name.clone(),
            created_by: self.created_by,
            members: self.members.iter().copied().collect(),
            admins: self.admins.iter().copied().collect(),
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn create_payload(&self) -> GroupPairwiseCommand {
        GroupPairwiseCommand::CreateGroup {
            group_id: self.group_id.clone(),
            protocol: self.protocol,
            base_revision: 0,
            new_revision: self.revision,
            name: self.name.clone(),
            created_by: self.created_by,
            members: self.members.iter().copied().collect(),
            admins: self.admins.iter().copied().collect(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn sync_payload(&self) -> GroupPairwiseCommand {
        GroupPairwiseCommand::SyncGroup {
            group_id: self.group_id.clone(),
            protocol: self.protocol,
            revision: self.revision,
            name: self.name.clone(),
            created_by: self.created_by,
            members: self.members.iter().copied().collect(),
            admins: self.admins.iter().copied().collect(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }

    fn remote_members(&self, local_owner_pubkey: OwnerPubkey) -> Vec<OwnerPubkey> {
        self.members
            .iter()
            .copied()
            .filter(|owner| *owner != local_owner_pubkey)
            .collect()
    }

    fn ensure_admin(&self, owner: OwnerPubkey) -> Result<()> {
        if !self.admins.contains(&owner) {
            return Err(group_error(format!(
                "owner {owner} is not an admin of group `{}`",
                self.group_id
            )));
        }
        Ok(())
    }

    fn ensure_member(&self, owner: OwnerPubkey) -> Result<()> {
        if !self.members.contains(&owner) {
            return Err(group_error(format!(
                "owner {owner} is not a member of group `{}`",
                self.group_id
            )));
        }
        Ok(())
    }

    fn ensure_revision(&self, base_revision: u64, new_revision: u64) -> Result<()> {
        if base_revision != self.revision {
            return Err(group_error(format!(
                "stale group revision for `{}`: expected {}, got {}",
                self.group_id, self.revision, base_revision
            )));
        }
        if new_revision != base_revision + 1 {
            return Err(group_error(format!(
                "invalid next revision for `{}`: expected {}, got {}",
                self.group_id,
                base_revision + 1,
                new_revision
            )));
        }
        Ok(())
    }

    fn retry_delta_revisions(&self, action: &str) -> Result<(u64, u64)> {
        if self.revision < 2 {
            return Err(group_error(format!(
                "{action} retry requires an already-applied revision"
            )));
        }
        Ok((self.revision - 1, self.revision))
    }

    fn reflects_rename(&self, name: &str, new_revision: u64) -> bool {
        self.revision >= new_revision && self.name == name
    }

    fn reflects_added_members(&self, additions: &BTreeSet<OwnerPubkey>, new_revision: u64) -> bool {
        self.revision >= new_revision && additions.iter().all(|owner| self.members.contains(owner))
    }

    fn reflects_removed_members(
        &self,
        removals: &BTreeSet<OwnerPubkey>,
        new_revision: u64,
    ) -> bool {
        self.revision >= new_revision && removals.iter().all(|owner| !self.members.contains(owner))
    }

    fn reflects_added_admins(&self, additions: &BTreeSet<OwnerPubkey>, new_revision: u64) -> bool {
        self.revision >= new_revision && additions.iter().all(|owner| self.admins.contains(owner))
    }

    fn reflects_removed_admins(&self, removals: &BTreeSet<OwnerPubkey>, new_revision: u64) -> bool {
        self.revision >= new_revision && removals.iter().all(|owner| !self.admins.contains(owner))
    }

    fn should_apply_delta_revision(&self, base_revision: u64) -> Result<bool> {
        if base_revision > self.revision {
            return Err(pending_group_revision_error(
                self.group_id.clone(),
                self.revision,
                base_revision,
            ));
        }
        Ok(base_revision == self.revision)
    }

    fn apply_rename(
        &mut self,
        actor: OwnerPubkey,
        name: String,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        self.name = name;
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_add_members(
        &mut self,
        actor: OwnerPubkey,
        additions: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if additions.is_empty() {
            return Err(group_error("members list must not be empty"));
        }
        for owner in additions {
            if self.members.contains(owner) {
                return Err(group_error(format!("owner {owner} is already a member")));
            }
        }
        self.members.extend(additions.iter().copied());
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_remove_members(
        &mut self,
        actor: OwnerPubkey,
        removals: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if removals.is_empty() {
            return Err(group_error("members list must not be empty"));
        }
        if removals.contains(&actor) {
            return Err(group_error("self-removal is not allowed"));
        }
        for owner in removals {
            if !self.members.contains(owner) {
                return Err(group_error(format!("owner {owner} is not a member")));
            }
        }
        for owner in removals {
            self.members.remove(owner);
            self.admins.remove(owner);
        }
        validate_group_invariants(&self.members, &self.admins)?;
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_add_admins(
        &mut self,
        actor: OwnerPubkey,
        additions: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if additions.is_empty() {
            return Err(group_error("admins list must not be empty"));
        }
        for owner in additions {
            if !self.members.contains(owner) {
                return Err(group_error(format!(
                    "owner {owner} must be a member before promotion"
                )));
            }
            if self.admins.contains(owner) {
                return Err(group_error(format!("owner {owner} is already an admin")));
            }
        }
        self.admins.extend(additions.iter().copied());
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }

    fn apply_remove_admins(
        &mut self,
        actor: OwnerPubkey,
        removals: &BTreeSet<OwnerPubkey>,
        base_revision: u64,
        new_revision: u64,
        updated_at: UnixSeconds,
    ) -> Result<()> {
        self.ensure_admin(actor)?;
        self.ensure_revision(base_revision, new_revision)?;
        if removals.is_empty() {
            return Err(group_error("admins list must not be empty"));
        }
        for owner in removals {
            if !self.admins.contains(owner) {
                return Err(group_error(format!("owner {owner} is not an admin")));
            }
        }
        if self.admins.len() == removals.len() {
            return Err(group_error("cannot remove the last admin"));
        }
        for owner in removals {
            self.admins.remove(owner);
        }
        validate_group_invariants(&self.members, &self.admins)?;
        self.revision = new_revision;
        self.updated_at = updated_at;
        Ok(())
    }
}

impl SenderKeyRecordId {
    fn new(group_id: String, sender_owner: OwnerPubkey, sender_device: DevicePubkey) -> Self {
        Self {
            group_id,
            sender_owner,
            sender_device,
        }
    }
}

impl SenderKeyRecord {
    fn id(&self) -> SenderKeyRecordId {
        SenderKeyRecordId::new(self.group_id.clone(), self.sender_owner, self.sender_device)
    }

    fn from_snapshot(snapshot: GroupSenderKeyRecordSnapshot) -> Result<Self> {
        let mut states = BTreeMap::new();
        for state in snapshot.states {
            if states.insert(state.key_id(), state).is_some() {
                return Err(group_error("duplicate sender-key state in snapshot"));
            }
        }
        if let Some(latest_key_id) = snapshot.latest_key_id {
            if !states.contains_key(&latest_key_id) {
                return Err(group_error("sender-key latest key id missing from states"));
            }
        }
        Ok(Self {
            group_id: snapshot.group_id,
            sender_owner: snapshot.sender_owner,
            sender_device: snapshot.sender_device,
            sender_event_pubkey: snapshot.sender_event_pubkey,
            sender_event_secret_key: snapshot.sender_event_secret_key,
            latest_key_id: snapshot.latest_key_id,
            states,
        })
    }

    fn snapshot(&self) -> GroupSenderKeyRecordSnapshot {
        GroupSenderKeyRecordSnapshot {
            group_id: self.group_id.clone(),
            sender_owner: self.sender_owner,
            sender_device: self.sender_device,
            sender_event_pubkey: self.sender_event_pubkey,
            sender_event_secret_key: self.sender_event_secret_key,
            latest_key_id: self.latest_key_id,
            states: self.states.values().cloned().collect(),
        }
    }
}

fn random_group_id<R>(ctx: &mut ProtocolContext<'_, R>) -> String
where
    R: RngCore + CryptoRng,
{
    let mut bytes = [0u8; 16];
    ctx.rng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn random_key_id<R>(ctx: &mut ProtocolContext<'_, R>) -> u32
where
    R: RngCore + CryptoRng,
{
    loop {
        let id = ctx.rng.next_u32();
        if id != 0 {
            return id;
        }
    }
}

fn validate_supported_protocol(protocol: GroupProtocol) -> Result<()> {
    if protocol.is_pairwise_fanout_v1() || protocol.is_sender_key_v1() {
        Ok(())
    } else {
        Err(group_error(format!(
            "unsupported group protocol {:?}/{}",
            protocol.strategy, protocol.version
        )))
    }
}

fn validate_unique_owners(values: &[OwnerPubkey], label: &str) -> Result<BTreeSet<OwnerPubkey>> {
    let set: BTreeSet<_> = values.iter().copied().collect();
    if set.len() != values.len() {
        return Err(group_error(format!("duplicate {label} are not allowed")));
    }
    Ok(set)
}

fn validate_group_invariants(
    members: &BTreeSet<OwnerPubkey>,
    admins: &BTreeSet<OwnerPubkey>,
) -> Result<()> {
    if members.is_empty() {
        return Err(group_error("group must have at least one member"));
    }
    if admins.is_empty() {
        return Err(group_error("group must have at least one admin"));
    }
    if !admins.is_subset(members) {
        return Err(group_error("all admins must also be members"));
    }
    Ok(())
}

fn merge_group_prepared_publish(into: &mut GroupPreparedPublish, next: GroupPreparedPublish) {
    into.deliveries.extend(next.deliveries);
    into.invite_responses.extend(next.invite_responses);
    into.sender_key_messages.extend(next.sender_key_messages);
    into.relay_gaps.extend(next.relay_gaps);
    into.relay_gaps.sort();
    into.relay_gaps.dedup();
    for fanout in next.pending_fanouts {
        if !into.pending_fanouts.contains(&fanout) {
            into.pending_fanouts.push(fanout);
        }
    }
}

fn group_error(message: impl Into<String>) -> crate::Error {
    DomainError::InvalidGroupOperation(message.into()).into()
}

fn pending_group_revision_error(
    group_id: impl Into<String>,
    current_revision: u64,
    required_revision: u64,
) -> crate::Error {
    DomainError::PendingGroupRevision {
        group_id: group_id.into(),
        current_revision,
        required_revision,
    }
    .into()
}
