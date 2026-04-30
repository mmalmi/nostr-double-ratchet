use std::io::{self, Read};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde_json::Value;

const MAGIC: &[u8; 4] = b"IRIS";
const COMPRESSED_FLAG: u8 = 0x01;
const COMPRESSION_THRESHOLD: usize = 100;

pub const NEARBY_FRAME_HEADER_BYTES: usize = 13;
pub const NEARBY_MAX_FRAME_BODY_BYTES: usize = 256 * 1024;

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
