use super::*;
use std::time::Duration;

const INVITE_BOOTSTRAP_EXPIRATION_SECONDS: u64 = 60;
const INVITE_BOOTSTRAP_RETRY_DELAYS_MS: [u64; 3] = [0, 500, 1500];

impl SessionManager {
    pub(super) fn build_bootstrap_messages(&self, owner_pubkey: PublicKey) -> Vec<UnsignedEvent> {
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + INVITE_BOOTSTRAP_EXPIRATION_SECONDS;
        let expiration =
            match Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()]) {
                Ok(tag) => tag,
                Err(_) => return Vec::new(),
            };

        let mut bootstrap_messages = Vec::new();
        for _ in INVITE_BOOTSTRAP_RETRY_DELAYS_MS {
            let Ok(bootstrap) = self.build_message_event(
                owner_pubkey,
                crate::TYPING_KIND,
                "typing".to_string(),
                vec![expiration.clone()],
            ) else {
                break;
            };
            bootstrap_messages.push(bootstrap);
        }

        bootstrap_messages
    }

    pub(super) fn sign_bootstrap_schedule(
        session: &mut crate::Session,
        bootstrap_messages: &[UnsignedEvent],
    ) -> Vec<nostr::Event> {
        let mut bootstrap_events = Vec::new();
        for bootstrap in bootstrap_messages {
            let Ok(signed_bootstrap) = session.send_event(bootstrap.clone()) else {
                break;
            };
            bootstrap_events.push(signed_bootstrap);
        }

        bootstrap_events
    }

    pub(super) fn publish_bootstrap_schedule(&self, bootstrap_events: Vec<nostr::Event>) {
        let Some((initial_event, retry_events)) = bootstrap_events.split_first() else {
            return;
        };

        let _ = self.pubsub.publish_signed(initial_event.clone());

        if retry_events.is_empty() {
            return;
        }

        let scheduled_retries: Vec<(u64, nostr::Event)> = retry_events
            .iter()
            .cloned()
            .zip(INVITE_BOOTSTRAP_RETRY_DELAYS_MS.iter().copied().skip(1))
            .map(|(event, delay_ms)| (delay_ms, event))
            .collect();
        let pubsub = self.pubsub.clone();
        std::thread::spawn(move || {
            for (delay_ms, event) in scheduled_retries {
                std::thread::sleep(Duration::from_millis(delay_ms));
                let _ = pubsub.publish_signed(event);
            }
        });
    }

    pub(super) fn send_link_bootstrap(&self, owner_pubkey: PublicKey, device_id: &str) {
        let bootstrap_messages = self.build_bootstrap_messages(owner_pubkey);
        let bootstrap_events = self.with_user_records({
            let device_id = device_id.to_string();
            let bootstrap_messages = bootstrap_messages.clone();
            move |records| {
                let Some(user_record) = records.get_mut(&owner_pubkey) else {
                    return Vec::new();
                };
                let Some(device_record) = user_record.device_records.get_mut(&device_id) else {
                    return Vec::new();
                };
                let mut signed = Vec::new();
                for bootstrap in bootstrap_messages {
                    let Some(signed_bootstrap) =
                        Self::send_event_with_best_session(device_record, bootstrap)
                    else {
                        break;
                    };
                    signed.push(signed_bootstrap);
                }
                signed
            }
        });

        if !bootstrap_events.is_empty() {
            self.publish_bootstrap_schedule(bootstrap_events);
            let _ = self.store_user_record(&owner_pubkey);
        }
    }
}
