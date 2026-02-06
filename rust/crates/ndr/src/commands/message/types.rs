use serde::Serialize;

#[derive(Serialize)]
pub(super) struct MessageSent {
    pub(super) id: String,
    pub(super) chat_id: String,
    pub(super) content: String,
    pub(super) timestamp: u64,
    /// The encrypted nostr event to publish
    pub(super) event: String,
}

#[derive(Serialize)]
pub(super) struct MessageList {
    pub(super) chat_id: String,
    pub(super) messages: Vec<MessageInfo>,
    pub(super) reactions: Vec<ReactionInfo>,
}

#[derive(Serialize)]
pub(super) struct MessageInfo {
    pub(super) id: String,
    pub(super) from_pubkey: String,
    pub(super) content: String,
    pub(super) timestamp: u64,
    pub(super) is_outgoing: bool,
}

#[derive(Serialize)]
pub(super) struct IncomingMessage {
    pub(super) chat_id: String,
    pub(super) message_id: String,
    pub(super) from_pubkey: String,
    pub(super) content: String,
    pub(super) timestamp: u64,
}

#[derive(Serialize)]
pub(super) struct IncomingReaction {
    pub(super) chat_id: String,
    pub(super) from_pubkey: String,
    pub(super) message_id: String,
    pub(super) emoji: String,
    pub(super) timestamp: u64,
}

#[derive(Serialize)]
pub(super) struct ReactionInfo {
    pub(super) id: String,
    pub(super) message_id: String,
    pub(super) from_pubkey: String,
    pub(super) emoji: String,
    pub(super) timestamp: u64,
    pub(super) is_outgoing: bool,
}
