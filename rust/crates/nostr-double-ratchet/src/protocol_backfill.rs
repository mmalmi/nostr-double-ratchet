use std::collections::HashSet;

use nostr::{Alphabet, Filter, Kind, PublicKey, SingleLetterTag, Timestamp};

use crate::{
    NdrRuntime, APP_KEYS_EVENT_KIND, INVITE_EVENT_KIND, INVITE_RESPONSE_KIND, MESSAGE_EVENT_KIND,
};

pub const DEFAULT_INVITE_BACKFILL_LOOKBACK_SECS: u64 = 30 * 24 * 60 * 60;
pub const DEFAULT_MESSAGE_BACKFILL_LOOKBACK_SECS: u64 = 30;

#[derive(Debug, Clone)]
pub struct NdrProtocolBackfillOptions {
    pub now_seconds: u64,
    pub owner_pubkeys: Vec<PublicKey>,
    pub invite_author_pubkeys: Vec<PublicKey>,
    pub message_author_pubkeys: Vec<PublicKey>,
    pub invite_lookback_seconds: u64,
    pub message_lookback_seconds: u64,
}

impl NdrProtocolBackfillOptions {
    pub fn new(now_seconds: u64) -> Self {
        Self {
            now_seconds,
            owner_pubkeys: Vec::new(),
            invite_author_pubkeys: Vec::new(),
            message_author_pubkeys: Vec::new(),
            invite_lookback_seconds: DEFAULT_INVITE_BACKFILL_LOOKBACK_SECS,
            message_lookback_seconds: DEFAULT_MESSAGE_BACKFILL_LOOKBACK_SECS,
        }
    }
}

impl NdrRuntime {
    pub fn protocol_backfill_filters(&self, options: NdrProtocolBackfillOptions) -> Vec<Filter> {
        let owners = self.protocol_backfill_owner_pubkeys(options.owner_pubkeys);
        let invite_authors =
            self.protocol_backfill_invite_authors(&owners, options.invite_author_pubkeys);
        let invite_response_pubkeys = self
            .current_device_invite_response_pubkey()
            .into_iter()
            .collect::<Vec<_>>();
        let message_authors =
            self.protocol_backfill_message_authors(options.message_author_pubkeys);

        build_protocol_backfill_filters(ProtocolBackfillFilterInputs {
            owner_pubkeys: owners,
            invite_author_pubkeys: invite_authors,
            invite_response_pubkeys,
            message_author_pubkeys: message_authors,
            now_seconds: options.now_seconds,
            invite_lookback_seconds: options.invite_lookback_seconds,
            message_lookback_seconds: options.message_lookback_seconds,
        })
    }

    fn protocol_backfill_owner_pubkeys(
        &self,
        extra_owner_pubkeys: Vec<PublicKey>,
    ) -> Vec<PublicKey> {
        dedupe_pubkeys(
            extra_owner_pubkeys
                .into_iter()
                .chain(std::iter::once(self.get_owner_pubkey()))
                .chain(self.session_manager().known_peer_owner_pubkeys())
                .chain(self.pending_invite_response_owner_pubkeys()),
        )
    }

    fn protocol_backfill_invite_authors(
        &self,
        owner_pubkeys: &[PublicKey],
        extra_invite_author_pubkeys: Vec<PublicKey>,
    ) -> Vec<PublicKey> {
        dedupe_pubkeys(
            extra_invite_author_pubkeys
                .into_iter()
                .chain(std::iter::once(self.get_our_pubkey()))
                .chain(
                    self.session_manager()
                        .known_device_identity_pubkeys_for_owners(owner_pubkeys.iter().copied()),
                ),
        )
    }

    fn protocol_backfill_message_authors(
        &self,
        extra_message_author_pubkeys: Vec<PublicKey>,
    ) -> Vec<PublicKey> {
        dedupe_pubkeys(
            extra_message_author_pubkeys
                .into_iter()
                .chain(self.get_all_message_push_author_pubkeys())
                .chain(self.group_known_sender_event_pubkeys()),
        )
    }
}

struct ProtocolBackfillFilterInputs {
    owner_pubkeys: Vec<PublicKey>,
    invite_author_pubkeys: Vec<PublicKey>,
    invite_response_pubkeys: Vec<PublicKey>,
    message_author_pubkeys: Vec<PublicKey>,
    now_seconds: u64,
    invite_lookback_seconds: u64,
    message_lookback_seconds: u64,
}

fn build_protocol_backfill_filters(inputs: ProtocolBackfillFilterInputs) -> Vec<Filter> {
    let mut filters = Vec::new();

    if !inputs.owner_pubkeys.is_empty() {
        filters.push(
            Filter::new()
                .kind(Kind::from(APP_KEYS_EVENT_KIND as u16))
                .authors(inputs.owner_pubkeys.clone()),
        );
    }

    if !inputs.invite_author_pubkeys.is_empty() {
        filters.push(
            Filter::new()
                .kind(Kind::from(INVITE_EVENT_KIND as u16))
                .authors(inputs.invite_author_pubkeys)
                .custom_tag(
                    SingleLetterTag::lowercase(Alphabet::L),
                    "double-ratchet/invites",
                )
                .since(Timestamp::from(
                    inputs
                        .now_seconds
                        .saturating_sub(inputs.invite_lookback_seconds),
                )),
        );
    }

    if !inputs.invite_response_pubkeys.is_empty() {
        filters.push(
            Filter::new()
                .kind(Kind::from(INVITE_RESPONSE_KIND as u16))
                .pubkeys(inputs.invite_response_pubkeys)
                .since(Timestamp::from(
                    inputs
                        .now_seconds
                        .saturating_sub(inputs.invite_lookback_seconds),
                )),
        );
    }

    if !inputs.message_author_pubkeys.is_empty() {
        filters.push(
            Filter::new()
                .kind(Kind::from(MESSAGE_EVENT_KIND as u16))
                .authors(inputs.message_author_pubkeys)
                .since(Timestamp::from(
                    inputs
                        .now_seconds
                        .saturating_sub(inputs.message_lookback_seconds),
                )),
        );
    }

    filters
}

fn dedupe_pubkeys(values: impl IntoIterator<Item = PublicKey>) -> Vec<PublicKey> {
    let mut seen = HashSet::new();
    let mut output = values
        .into_iter()
        .filter(|pubkey| seen.insert(pubkey.to_hex()))
        .collect::<Vec<_>>();
    output.sort_by_key(PublicKey::to_hex);
    output
}

#[cfg(test)]
mod tests {
    use nostr::Keys;

    use crate::{AppKeys, DeviceEntry, Invite, NdrProtocolBackfillOptions, NdrRuntime};

    fn filter_with_kind(filters: &[nostr::Filter], kind: u64) -> serde_json::Value {
        filters
            .iter()
            .map(|filter| serde_json::to_value(filter).expect("filter json"))
            .find(|filter| {
                filter
                    .get("kinds")
                    .and_then(|kinds| kinds.as_array())
                    .is_some_and(|kinds| kinds.iter().any(|value| value.as_u64() == Some(kind)))
            })
            .expect("filter kind")
    }

    fn invite_filter(filters: &[nostr::Filter]) -> serde_json::Value {
        filters
            .iter()
            .map(|filter| serde_json::to_value(filter).expect("filter json"))
            .find(|filter| {
                filter
                    .get("kinds")
                    .and_then(|kinds| kinds.as_array())
                    .is_some_and(|kinds| {
                        kinds
                            .iter()
                            .any(|value| value.as_u64() == Some(crate::INVITE_EVENT_KIND as u64))
                    })
                    && filter
                        .get("#l")
                        .and_then(|labels| labels.as_array())
                        .is_some_and(|labels| {
                            labels
                                .iter()
                                .any(|label| label.as_str() == Some("double-ratchet/invites"))
                        })
            })
            .expect("invite filter")
    }

    #[test]
    fn runtime_backfill_filters_include_current_device_invite_responses() {
        let owner = Keys::generate();
        let device = Keys::generate();
        let invite = Invite::create_new(
            device.public_key(),
            Some(device.public_key().to_hex()),
            Some(1),
        )
        .expect("invite");
        let invite_response_pubkey = invite.inviter_ephemeral_public_key;
        let runtime = NdrRuntime::new(
            device.public_key(),
            device.secret_key().secret_bytes(),
            device.public_key().to_hex(),
            owner.public_key(),
            None,
            Some(invite),
        );
        runtime.init().expect("runtime init");

        let filters =
            runtime.protocol_backfill_filters(NdrProtocolBackfillOptions::new(1_777_159_500));

        let response_filter = filter_with_kind(&filters, crate::INVITE_RESPONSE_KIND as u64);
        assert_eq!(
            response_filter
                .get("#p")
                .and_then(|pubkeys| pubkeys.as_array())
                .and_then(|pubkeys| pubkeys.first())
                .and_then(|pubkey| pubkey.as_str()),
            Some(invite_response_pubkey.to_hex().as_str())
        );
        assert_eq!(
            response_filter
                .get("since")
                .and_then(|since| since.as_u64()),
            Some(1_777_159_500 - crate::DEFAULT_INVITE_BACKFILL_LOOKBACK_SECS)
        );
    }

    #[test]
    fn runtime_backfill_filters_include_app_keys_devices_for_invites() {
        let owner = Keys::generate();
        let local_device = Keys::generate();
        let peer_owner = Keys::generate();
        let peer_device = Keys::generate();
        let runtime = NdrRuntime::new(
            local_device.public_key(),
            local_device.secret_key().secret_bytes(),
            local_device.public_key().to_hex(),
            owner.public_key(),
            None,
            None,
        );
        assert_eq!(runtime.get_our_pubkey(), local_device.public_key());
        runtime.ingest_app_keys_snapshot(
            peer_owner.public_key(),
            AppKeys::new(vec![DeviceEntry::new(peer_device.public_key(), 1)]),
            1,
        );

        let mut options = NdrProtocolBackfillOptions::new(1_777_159_500);
        options.owner_pubkeys.push(peer_owner.public_key());
        let filters = runtime.protocol_backfill_filters(options);

        let invite_filter = invite_filter(&filters);
        let authors = invite_filter
            .get("authors")
            .and_then(|authors| authors.as_array())
            .expect("authors")
            .iter()
            .filter_map(|author| author.as_str())
            .collect::<Vec<_>>();
        let local_device_hex = local_device.public_key().to_hex();
        let peer_device_hex = peer_device.public_key().to_hex();
        assert!(
            authors.iter().any(|author| *author == local_device_hex),
            "missing local device author {local_device_hex} in {authors:?}"
        );
        assert!(
            authors.iter().any(|author| *author == peer_device_hex),
            "missing peer device author {peer_device_hex} in {authors:?}"
        );
    }
}
