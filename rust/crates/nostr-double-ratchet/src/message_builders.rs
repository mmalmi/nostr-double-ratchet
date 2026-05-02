use std::time::{SystemTime, UNIX_EPOCH};

use nostr::{EventBuilder, Kind, PublicKey, Tag, Timestamp, UnsignedEvent};

use crate::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct InnerEventBuildOptions {
    pub created_at_seconds: Option<u64>,
    pub ms: Option<u64>,
    pub ensure_ms_tag: bool,
}

impl InnerEventBuildOptions {
    pub fn with_ms_tag() -> Self {
        Self {
            ensure_ms_tag: true,
            ..Self::default()
        }
    }
}

pub fn expiration_tag_for_options(
    options: &crate::SendOptions,
    now_seconds: u64,
) -> Result<Option<Tag>> {
    let Some(expires_at) = crate::utils::resolve_expiration_seconds(options, now_seconds)? else {
        return Ok(None);
    };

    Tag::parse(&[crate::EXPIRATION_TAG.to_string(), expires_at.to_string()])
        .map(Some)
        .map_err(|e| Error::InvalidEvent(e.to_string()))
}

pub fn append_expiration_tag(
    tags: &mut Vec<Tag>,
    options: &crate::SendOptions,
    now_seconds: u64,
) -> Result<()> {
    if let Some(tag) = expiration_tag_for_options(options, now_seconds)? {
        tags.push(tag);
    }
    Ok(())
}

pub fn build_inner_event(
    author_pubkey: PublicKey,
    kind: u32,
    content: impl Into<String>,
    tags: Vec<Tag>,
    options: InnerEventBuildOptions,
) -> Result<UnsignedEvent> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let now_seconds = now.as_secs();
    let now_ms = now.as_millis() as u64;
    let created_at_seconds = options
        .created_at_seconds
        .or_else(|| options.ms.map(|ms| ms / 1_000))
        .unwrap_or(now_seconds);
    let ms = options.ms.unwrap_or(now_ms);
    let tags = if options.ensure_ms_tag {
        ensure_ms_tag(tags, ms)?
    } else {
        tags
    };

    let kind: u16 = kind
        .try_into()
        .map_err(|_| Error::InvalidEvent("kind out of range".to_string()))?;
    let mut event = EventBuilder::new(Kind::from(kind), content.into())
        .tags(tags)
        .custom_created_at(Timestamp::from(created_at_seconds))
        .build(author_pubkey);
    event.ensure_id();
    Ok(event)
}

pub fn build_direct_inner_event(
    author_pubkey: PublicKey,
    recipient_pubkey: PublicKey,
    kind: u32,
    content: impl Into<String>,
    tags: Vec<Tag>,
) -> Result<UnsignedEvent> {
    build_inner_event(
        author_pubkey,
        kind,
        content,
        ensure_recipient_tag(tags, recipient_pubkey)?,
        InnerEventBuildOptions::with_ms_tag(),
    )
}

pub fn build_text_rumor(
    author_pubkey: PublicKey,
    text: impl Into<String>,
    tags: Vec<Tag>,
) -> Result<UnsignedEvent> {
    build_inner_event(
        author_pubkey,
        crate::CHAT_MESSAGE_KIND,
        text,
        tags,
        InnerEventBuildOptions::with_ms_tag(),
    )
}

pub fn build_reply_rumor(
    author_pubkey: PublicKey,
    text: impl Into<String>,
    reply_to: &str,
    mut tags: Vec<Tag>,
) -> Result<UnsignedEvent> {
    tags.push(event_reference_tag(reply_to)?);
    build_text_rumor(author_pubkey, text, tags)
}

pub fn build_reaction_rumor(
    author_pubkey: PublicKey,
    message_id: &str,
    emoji: impl Into<String>,
    mut tags: Vec<Tag>,
) -> Result<UnsignedEvent> {
    tags.push(event_reference_tag(message_id)?);
    build_inner_event(
        author_pubkey,
        crate::REACTION_KIND,
        emoji,
        tags,
        InnerEventBuildOptions::with_ms_tag(),
    )
}

pub fn build_receipt_rumor(
    author_pubkey: PublicKey,
    receipt_type: impl Into<String>,
    message_ids: impl IntoIterator<Item = impl AsRef<str>>,
    mut tags: Vec<Tag>,
) -> Result<UnsignedEvent> {
    for message_id in message_ids {
        tags.push(event_reference_tag(message_id.as_ref())?);
    }
    build_inner_event(
        author_pubkey,
        crate::RECEIPT_KIND,
        receipt_type,
        tags,
        InnerEventBuildOptions::with_ms_tag(),
    )
}

pub fn build_typing_rumor(author_pubkey: PublicKey, tags: Vec<Tag>) -> Result<UnsignedEvent> {
    build_inner_event(
        author_pubkey,
        crate::TYPING_KIND,
        "typing",
        tags,
        InnerEventBuildOptions::with_ms_tag(),
    )
}

pub fn ensure_recipient_tag(mut tags: Vec<Tag>, recipient_pubkey: PublicKey) -> Result<Vec<Tag>> {
    let recipient_hex = recipient_pubkey.to_hex();
    let has_recipient_p_tag = tags.iter().any(|tag| {
        let values = tag.clone().to_vec();
        values.first().map(|value| value.as_str()) == Some("p")
            && values.get(1).map(|value| value.as_str()) == Some(recipient_hex.as_str())
    });

    if !has_recipient_p_tag {
        tags.insert(
            0,
            Tag::parse(&["p".to_string(), recipient_hex])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        );
    }

    Ok(tags)
}

pub fn ensure_ms_tag(mut tags: Vec<Tag>, ms: u64) -> Result<Vec<Tag>> {
    let has_ms_tag = tags.iter().any(|tag| {
        let values = tag.clone().to_vec();
        values.first().map(|value| value.as_str()) == Some("ms")
    });

    if !has_ms_tag {
        tags.push(
            Tag::parse(&["ms".to_string(), ms.to_string()])
                .map_err(|e| Error::InvalidEvent(e.to_string()))?,
        );
    }

    Ok(tags)
}

pub fn event_reference_tag(message_id: &str) -> Result<Tag> {
    Tag::parse(&["e".to_string(), message_id.to_string()])
        .map_err(|e| Error::InvalidEvent(e.to_string()))
}
