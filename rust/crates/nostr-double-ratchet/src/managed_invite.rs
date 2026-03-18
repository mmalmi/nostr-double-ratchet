use crate::{
    pubsub::{build_filter, NostrPubSub},
    Invite, InviteAcceptInput, InviteProcessResponseInput, InviteProcessResponseResult,
    ManagedSession, Result, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND,
};
use nostr::types::filter::{Alphabet, SingleLetterTag};
use nostr::PublicKey;
use nostr::{Kind, Keys};

pub struct InviteResponse {
    pub session: ManagedSession,
    pub invitee_identity: PublicKey,
    pub device_id: Option<String>,
    pub owner_public_key: Option<PublicKey>,
}

impl InviteResponse {
    pub fn resolved_owner_pubkey(&self) -> PublicKey {
        self.owner_public_key.unwrap_or(self.invitee_identity)
    }

    pub fn has_verified_owner_claim(&self, app_keys: Option<&crate::AppKeys>) -> bool {
        let owner = self.resolved_owner_pubkey();
        if owner == self.invitee_identity {
            return true;
        }
        app_keys
            .and_then(|keys| keys.get_device(&self.invitee_identity))
            .is_some()
    }
}

pub struct ManagedInvite {
    pub invite: Invite,
}

impl ManagedInvite {
    pub fn new(invite: Invite) -> Self {
        Self { invite }
    }

    pub fn accept(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
    ) -> Result<(ManagedSession, nostr::Event)> {
        self.accept_with_owner(invitee_public_key, invitee_private_key, device_id, None)
    }

    pub fn accept_with_owner(
        &self,
        invitee_public_key: PublicKey,
        invitee_private_key: [u8; 32],
        device_id: Option<String>,
        owner_public_key: Option<PublicKey>,
    ) -> Result<(ManagedSession, nostr::Event)> {
        let invitee_session_key = Keys::generate().secret_key().to_secret_bytes();
        let invitee_next_nostr_private_key = Keys::generate().secret_key().to_secret_bytes();
        let envelope_sender_private_key = Keys::generate().secret_key().to_secret_bytes();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let two_days = 2 * 24 * 60 * 60;
        let response_created_at = now - (rand::random::<u64>() % two_days);

        let accepted = self.invite.accept(InviteAcceptInput {
            invitee_public_key,
            invitee_identity_private_key: invitee_private_key,
            invitee_session_private_key: invitee_session_key,
            invitee_next_nostr_private_key,
            envelope_sender_private_key,
            response_created_at,
            device_id,
            owner_public_key,
        })?;

        Ok((
            ManagedSession::from_session(accepted.session, "session".to_string()),
            accepted.response_event,
        ))
    }

    pub fn listen_with_pubsub(&self, pubsub: &dyn NostrPubSub) -> Result<String> {
        let filter = build_filter()
            .kinds(vec![INVITE_RESPONSE_KIND as u64])
            .pubkeys(vec![self.invite.inviter_ephemeral_public_key])
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
        pubsub: &dyn NostrPubSub,
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
        inviter_private_key: [u8; 32],
    ) -> Result<Option<InviteResponse>> {
        let inviter_next_nostr_private_key = Keys::generate().secret_key().to_secret_bytes();

        match self.invite.process_response(InviteProcessResponseInput {
            event: event.clone(),
            inviter_identity_private_key: inviter_private_key,
            inviter_next_nostr_private_key,
        }) {
            InviteProcessResponseResult::NotForThisInvite { .. } => Ok(None),
            InviteProcessResponseResult::Accepted {
                session, meta, ..
            } => Ok(Some(InviteResponse {
                session: ManagedSession::from_session(session, event.id.to_string()),
                invitee_identity: meta.invitee_identity,
                device_id: meta.device_id,
                owner_public_key: meta.owner_public_key,
            })),
            InviteProcessResponseResult::InvalidRelevant { error, .. } => Err(error),
        }
    }
}

impl From<Invite> for ManagedInvite {
    fn from(invite: Invite) -> Self {
        Self::new(invite)
    }
}
