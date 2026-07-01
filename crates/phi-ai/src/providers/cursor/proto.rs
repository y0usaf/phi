//! Protobuf encode/decode for Cursor's connect+proto wire format.
//! Hand-rolled (no `.proto` files) to avoid adding prost as a dependency.

use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::fmt;
use std::io::Read;
use uuid::Uuid;

/// Build the native request body (connect+proto frame).
pub fn build_native_request_body(prompt: &str, model: &str) -> Vec<u8> {
    let message_id = Uuid::new_v4().to_string();
    let conversation_id = Uuid::new_v4().to_string();
    let request = {
        let mut bytes = Vec::new();
        bytes.extend(encode_field(
            1,
            2,
            encode_message(prompt, 1, &message_id, Some(1)),
        ));
        bytes.extend(encode_field(2, 0, encode_varint_bytes(1)));
        bytes.extend(encode_field(3, 2, Vec::<u8>::new()));
        bytes.extend(encode_field(4, 0, encode_varint_bytes(1)));
        bytes.extend(encode_field(5, 2, encode_model(model)));
        bytes.extend(encode_field(8, 2, Vec::<u8>::new()));
        bytes.extend(encode_field(13, 0, encode_varint_bytes(1)));
        bytes.extend(encode_field(15, 2, encode_cursor_setting()));
        bytes.extend(encode_field(19, 0, encode_varint_bytes(1)));
        bytes.extend(encode_field(23, 2, conversation_id.into_bytes()));
        bytes.extend(encode_field(26, 2, encode_metadata()));
        bytes.extend(encode_field(27, 0, encode_varint_bytes(0)));
        bytes.extend(encode_field(30, 2, encode_message_id(&message_id, 1)));
        bytes.extend(encode_field(35, 0, encode_varint_bytes(0)));
        bytes.extend(encode_field(38, 0, encode_varint_bytes(0)));
        bytes.extend(encode_field(46, 0, encode_varint_bytes(1)));
        bytes.extend(encode_field(47, 2, Vec::<u8>::new()));
        bytes.extend(encode_field(48, 0, encode_varint_bytes(0)));
        bytes.extend(encode_field(49, 0, encode_varint_bytes(0)));
        bytes.extend(encode_field(51, 0, encode_varint_bytes(0)));
        bytes.extend(encode_field(53, 0, encode_varint_bytes(1)));
        bytes.extend(encode_field(54, 2, b"Ask".to_vec()));
        bytes
    };

    let outer = encode_field(1, 2, request);
    let mut body = Vec::with_capacity(outer.len() + 5);
    body.push(0);
    body.extend_from_slice(&(outer.len() as u32).to_be_bytes());
    body.extend_from_slice(&outer);
    body
}

fn encode_message(
    content: &str,
    role: u64,
    message_id: &str,
    chat_mode_enum: Option<u64>,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field(1, 2, content.as_bytes().to_vec()));
    bytes.extend(encode_field(2, 0, encode_varint_bytes(role)));
    bytes.extend(encode_field(13, 2, message_id.as_bytes().to_vec()));
    if let Some(chat_mode_enum) = chat_mode_enum {
        bytes.extend(encode_field(47, 0, encode_varint_bytes(chat_mode_enum)));
    }
    bytes
}

fn encode_model(name: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field(1, 2, name.as_bytes().to_vec()));
    bytes.extend(encode_field(4, 2, Vec::<u8>::new()));
    bytes
}

fn encode_cursor_setting() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field(1, 2, b"cursor\\aisettings".to_vec()));
    bytes.extend(encode_field(3, 2, Vec::<u8>::new()));
    let mut unknown6 = Vec::new();
    unknown6.extend(encode_field(1, 2, Vec::<u8>::new()));
    unknown6.extend(encode_field(2, 2, Vec::<u8>::new()));
    bytes.extend(encode_field(6, 2, unknown6));
    bytes.extend(encode_field(8, 0, encode_varint_bytes(1)));
    bytes.extend(encode_field(9, 0, encode_varint_bytes(1)));
    bytes
}

fn encode_metadata() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field(1, 2, std::env::consts::OS.as_bytes().to_vec()));
    bytes.extend(encode_field(
        2,
        2,
        std::env::consts::ARCH.as_bytes().to_vec(),
    ));
    bytes.extend(encode_field(
        3,
        2,
        std::env::consts::ARCH.as_bytes().to_vec(),
    ));
    bytes.extend(encode_field(4, 2, b"phi".to_vec()));
    bytes.extend(encode_field(
        5,
        2,
        jiff::Timestamp::now().to_string().into_bytes(),
    ));
    bytes
}

fn encode_message_id(message_id: &str, role: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(encode_field(1, 2, message_id.as_bytes().to_vec()));
    bytes.extend(encode_field(3, 0, encode_varint_bytes(role)));
    bytes
}

fn encode_field(field_number: u64, wire_type: u8, value: Vec<u8>) -> Vec<u8> {
    let mut bytes = encode_varint_bytes((field_number << 3) | u64::from(wire_type));
    match wire_type {
        0 => bytes.extend(value),
        2 => {
            bytes.extend(encode_varint_bytes(value.len() as u64));
            bytes.extend(value);
        }
        _ => unreachable!("unsupported wire type"),
    }
    bytes
}

fn encode_varint_bytes(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    while value >= 0x80 {
        bytes.push(((value as u8) & 0x7f) | 0x80);
        value >>= 7;
    }
    bytes.push(value as u8);
    bytes
}

// ── Response decoding ───────────────────────────────────────────────────

/// Decoded connect+proto frame: (frame_type, payload_bytes).
pub fn decode_next_frame(buffer: &[u8]) -> Result<Option<(u8, Vec<u8>, usize)>> {
    if buffer.len() < 5 {
        return Ok(None);
    }
    let frame_type = buffer[0];
    let payload_len = u32::from_be_bytes([buffer[1], buffer[2], buffer[3], buffer[4]]) as usize;
    let consumed = 5 + payload_len;
    if buffer.len() < consumed {
        return Ok(None);
    }
    let payload = &buffer[5..consumed];
    let payload = match frame_type {
        1 | 3 => gunzip(payload)?,
        _ => payload.to_vec(),
    };
    Ok(Some((frame_type, payload, consumed)))
}

fn gunzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = GzDecoder::new(bytes);
    let mut decoded = Vec::new();
    decoder
        .read_to_end(&mut decoded)
        .context("Failed to gunzip Cursor response payload")?;
    Ok(decoded)
}

/// Extract text chunks from a protobuf response payload.
/// Field 2 → message → field 1 → text bytes.
/// Also checks for error frames.
pub fn decode_protobuf_events(payload: &[u8]) -> Result<FrameResult> {
    let mut text_chunks = Vec::new();
    let mut error: Option<String> = None;

    for field in parse_fields(payload)? {
        if field.number == 2
            && let FieldValue::Bytes(inner) = field.value
            && let Ok(inner_fields) = parse_fields(&inner)
        {
            for inner_field in inner_fields {
                if inner_field.number == 1
                    && let FieldValue::Bytes(text) = inner_field.value
                {
                    let text = String::from_utf8_lossy(&text).into_owned();
                    if !text.is_empty() {
                        text_chunks.push(text);
                    }
                }
            }
        }
    }

    // Also check for top-level error fields
    for field in parse_fields(payload)? {
        if field.number == 1
            && let FieldValue::Bytes(ref data) = field.value
            && let Ok(json) = serde_json::from_slice::<serde_json::Value>(data)
            && let Some(err_msg) = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(serde_json::Value::as_str)
        {
            error = Some(err_msg.to_string());
        }
    }

    Ok(FrameResult { text_chunks, error })
}

pub struct FrameResult {
    pub text_chunks: Vec<String>,
    pub error: Option<String>,
}

/// Parse JSON error frame (frame types 2/3 where payload != "{}").
pub fn parse_json_error_frame(payload: &[u8]) -> Result<Option<String>> {
    let json: serde_json::Value =
        serde_json::from_slice(payload).context("Failed to decode Cursor native JSON frame")?;
    if let Some(message) = json
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
    {
        if message.eq_ignore_ascii_case("error") {
            return Ok(Some(format!("Cursor native API stream error: {}", json)));
        }
        return Ok(Some(format!("Cursor native API stream error: {}", message)));
    }
    Ok(Some(format!("Cursor native API stream error: {}", json)))
}

#[derive(Debug)]
struct ProtobufField {
    number: u64,
    value: FieldValue,
}

enum FieldValue {
    Varint(u64),
    Bytes(Vec<u8>),
    #[allow(dead_code)]
    Fixed32([u8; 4]),
    #[allow(dead_code)]
    Fixed64([u8; 8]),
}

impl fmt::Debug for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Varint(value) => f.debug_tuple("Varint").field(value).finish(),
            Self::Bytes(bytes) => f.debug_struct("Bytes").field("len", &bytes.len()).finish(),
            Self::Fixed32(bytes) => f.debug_tuple("Fixed32").field(bytes).finish(),
            Self::Fixed64(bytes) => f.debug_tuple("Fixed64").field(bytes).finish(),
        }
    }
}

fn parse_fields(bytes: &[u8]) -> Result<Vec<ProtobufField>> {
    let mut fields = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let tag = decode_varint(bytes, &mut index)?;
        let number = tag >> 3;
        let wire_type = (tag & 0x07) as u8;
        let value = match wire_type {
            0 => FieldValue::Varint(decode_varint(bytes, &mut index)?),
            1 => {
                let end = index + 8;
                let slice = bytes
                    .get(index..end)
                    .ok_or_else(|| anyhow::anyhow!("Truncated fixed64 protobuf field"))?;
                index = end;
                let mut array = [0u8; 8];
                array.copy_from_slice(slice);
                FieldValue::Fixed64(array)
            }
            2 => {
                let len = decode_varint(bytes, &mut index)? as usize;
                let end = index + len;
                let slice = bytes
                    .get(index..end)
                    .ok_or_else(|| anyhow::anyhow!("Truncated length-delimited protobuf field"))?;
                index = end;
                FieldValue::Bytes(slice.to_vec())
            }
            5 => {
                let end = index + 4;
                let slice = bytes
                    .get(index..end)
                    .ok_or_else(|| anyhow::anyhow!("Truncated fixed32 protobuf field"))?;
                index = end;
                let mut array = [0u8; 4];
                array.copy_from_slice(slice);
                FieldValue::Fixed32(array)
            }
            _ => anyhow::bail!("Unsupported protobuf wire type {}", wire_type),
        };
        fields.push(ProtobufField { number, value });
    }
    Ok(fields)
}

fn decode_varint(bytes: &[u8], index: &mut usize) -> Result<u64> {
    let mut shift = 0u32;
    let mut value = 0u64;
    loop {
        let byte = *bytes
            .get(*index)
            .ok_or_else(|| anyhow::anyhow!("Unexpected EOF while decoding protobuf varint"))?;
        *index += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift >= 64 {
            anyhow::bail!("Protobuf varint too large");
        }
    }
}
