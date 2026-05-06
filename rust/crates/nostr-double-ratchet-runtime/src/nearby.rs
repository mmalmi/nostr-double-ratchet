use std::io::{self, Read};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAGIC: &[u8; 4] = b"IRIS";
const COMPRESSED_FLAG: u8 = 0x01;
const COMPRESSION_THRESHOLD: usize = 100;

pub const NEARBY_FRAME_HEADER_BYTES: usize = 13;
pub const NEARBY_MAX_FRAME_BODY_BYTES: usize = 256 * 1024;
pub const NEARBY_ENVELOPE_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NearbyInventoryItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub kind: u64,
    pub created_at: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NearbyEnvelope {
    #[serde(rename = "hello")]
    Hello {
        v: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        nonce: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    #[serde(rename = "inv")]
    Inv {
        v: u8,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        author: Option<String>,
        kind: u64,
        created_at: u64,
        size: u64,
    },
    #[serde(rename = "want")]
    Want { v: u8, id: String },
    #[serde(rename = "event")]
    Event { v: u8, event_json: String },
}

impl NearbyEnvelope {
    pub fn hello(nonce: Option<String>, name: Option<String>) -> Self {
        Self::Hello {
            v: NEARBY_ENVELOPE_VERSION,
            nonce,
            name,
        }
    }

    pub fn inv(item: NearbyInventoryItem) -> Self {
        Self::Inv {
            v: NEARBY_ENVELOPE_VERSION,
            id: item.id,
            author: item.author,
            kind: item.kind,
            created_at: item.created_at,
            size: item.size,
        }
    }

    pub fn want(id: impl Into<String>) -> Self {
        Self::Want {
            v: NEARBY_ENVELOPE_VERSION,
            id: id.into(),
        }
    }

    pub fn event(event_json: impl Into<String>) -> Self {
        Self::Event {
            v: NEARBY_ENVELOPE_VERSION,
            event_json: event_json.into(),
        }
    }

    fn version(&self) -> u8 {
        match self {
            Self::Hello { v, .. }
            | Self::Inv { v, .. }
            | Self::Want { v, .. }
            | Self::Event { v, .. } => *v,
        }
    }
}

pub fn encode_nearby_envelope_json(envelope: &NearbyEnvelope) -> Option<String> {
    if !validate_nearby_envelope(envelope) {
        return None;
    }
    serde_json::to_string(envelope).ok()
}

pub fn decode_nearby_envelope_json(envelope_json: &str) -> Option<NearbyEnvelope> {
    let value: Value = serde_json::from_str(envelope_json).ok()?;
    if value.get("peer_id").is_some() {
        return None;
    }
    let envelope: NearbyEnvelope = serde_json::from_value(value).ok()?;
    validate_nearby_envelope(&envelope).then_some(envelope)
}

pub fn encode_nearby_envelope_frame(envelope: &NearbyEnvelope) -> Option<Vec<u8>> {
    encode_nearby_frame_json(&encode_nearby_envelope_json(envelope)?)
}

pub fn decode_nearby_envelope_frame(frame: &[u8]) -> Option<NearbyEnvelope> {
    decode_nearby_envelope_json(&decode_nearby_frame_json(frame)?)
}

fn validate_nearby_envelope(envelope: &NearbyEnvelope) -> bool {
    if envelope.version() != NEARBY_ENVELOPE_VERSION {
        return false;
    }
    match envelope {
        NearbyEnvelope::Hello { .. } => true,
        NearbyEnvelope::Inv {
            id, author, size, ..
        } => {
            is_hex_id(id)
                && author.as_ref().is_none_or(|author| is_hex_id(author))
                && (1..=NEARBY_MAX_FRAME_BODY_BYTES as u64).contains(size)
        }
        NearbyEnvelope::Want { id, .. } => is_hex_id(id),
        NearbyEnvelope::Event { event_json, .. } => {
            !event_json.is_empty() && event_json.len() <= NEARBY_MAX_FRAME_BODY_BYTES
        }
    }
}

fn is_hex_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn encode_nearby_frame_json(envelope_json: &str) -> Option<Vec<u8>> {
    let envelope: Value = serde_json::from_str(envelope_json).ok()?;
    if !envelope.is_object() {
        return None;
    }
    let payload = serde_json::to_vec(&envelope).ok()?;
    if payload.is_empty() || payload.len() > NEARBY_MAX_FRAME_BODY_BYTES {
        return None;
    }

    let compressed = compress_if_beneficial(&payload);
    let body = compressed.as_deref().unwrap_or(&payload);
    if body.len() > NEARBY_MAX_FRAME_BODY_BYTES {
        return None;
    }

    let mut frame = Vec::with_capacity(NEARBY_FRAME_HEADER_BYTES + body.len());
    frame.extend_from_slice(MAGIC);
    frame.push(if compressed.is_some() {
        COMPRESSED_FLAG
    } else {
        0
    });
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    Some(frame)
}

pub fn decode_nearby_frame_json(frame: &[u8]) -> Option<String> {
    if frame.len() < NEARBY_FRAME_HEADER_BYTES || &frame[..4] != MAGIC {
        return None;
    }
    let flags = frame[4];
    if flags & !COMPRESSED_FLAG != 0 {
        return None;
    }

    let body_len = u32::from_be_bytes(frame[5..9].try_into().ok()?) as usize;
    let original_len = u32::from_be_bytes(frame[9..13].try_into().ok()?) as usize;
    if body_len == 0
        || original_len == 0
        || body_len > NEARBY_MAX_FRAME_BODY_BYTES
        || original_len > NEARBY_MAX_FRAME_BODY_BYTES
        || frame.len() != NEARBY_FRAME_HEADER_BYTES + body_len
    {
        return None;
    }

    let body = &frame[NEARBY_FRAME_HEADER_BYTES..];
    let payload = if flags & COMPRESSED_FLAG != 0 {
        decompress(body, original_len)?
    } else {
        if body_len != original_len {
            return None;
        }
        body.to_vec()
    };

    let envelope: Value = serde_json::from_slice(&payload).ok()?;
    if !envelope.is_object() {
        return None;
    }
    serde_json::to_string(&envelope).ok()
}

pub fn nearby_frame_body_len_from_header(header: &[u8]) -> Option<usize> {
    if header.len() < NEARBY_FRAME_HEADER_BYTES || &header[..4] != MAGIC {
        return None;
    }
    let body_len = u32::from_be_bytes(header[5..9].try_into().ok()?) as usize;
    if body_len == 0 || body_len > NEARBY_MAX_FRAME_BODY_BYTES {
        return None;
    }
    Some(body_len)
}

pub fn read_nearby_frame<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; NEARBY_FRAME_HEADER_BYTES];
    if !read_exact_or_eof(reader, &mut header)? {
        return Ok(None);
    }
    let body_len = nearby_frame_body_len_from_header(&header)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid nearby frame header"))?;
    let mut frame = Vec::with_capacity(NEARBY_FRAME_HEADER_BYTES + body_len);
    frame.extend_from_slice(&header);
    let start = frame.len();
    frame.resize(start + body_len, 0);
    reader.read_exact(&mut frame[start..])?;
    Ok(Some(frame))
}

#[derive(Debug)]
pub struct NearbyFrameAssembler {
    buffer: Vec<u8>,
}

impl NearbyFrameAssembler {
    pub fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub fn append(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while self.buffer.len() >= NEARBY_FRAME_HEADER_BYTES {
            let Some(body_len) =
                nearby_frame_body_len_from_header(&self.buffer[..NEARBY_FRAME_HEADER_BYTES])
            else {
                self.buffer.remove(0);
                continue;
            };
            let frame_len = NEARBY_FRAME_HEADER_BYTES + body_len;
            if self.buffer.len() < frame_len {
                break;
            }
            frames.push(self.buffer.drain(..frame_len).collect());
        }
        frames
    }
}

impl Default for NearbyFrameAssembler {
    fn default() -> Self {
        Self::new()
    }
}

fn read_exact_or_eof<R: Read>(reader: &mut R, buffer: &mut [u8]) -> io::Result<bool> {
    let mut offset = 0;
    while offset < buffer.len() {
        match reader.read(&mut buffer[offset..]) {
            Ok(0) if offset == 0 => return Ok(false),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "partial nearby frame header",
                ))
            }
            Ok(read) => offset += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(true)
}

fn compress_if_beneficial(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < COMPRESSION_THRESHOLD {
        return None;
    }
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    use std::io::Write;
    encoder.write_all(data).ok()?;
    let compressed = encoder.finish().ok()?;
    if compressed.is_empty() || compressed.len() >= data.len() {
        return None;
    }
    Some(compressed)
}

fn decompress(data: &[u8], original_len: usize) -> Option<Vec<u8>> {
    let mut decoder = DeflateDecoder::new(data);
    let mut output = Vec::with_capacity(original_len);
    decoder.read_to_end(&mut output).ok()?;
    if output.len() != original_len || output.len() > NEARBY_MAX_FRAME_BODY_BYTES {
        return None;
    }
    Some(output)
}
