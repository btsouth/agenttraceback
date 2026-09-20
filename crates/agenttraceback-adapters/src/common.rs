use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use agenttraceback_adapter_sdk::AdapterError;
use chrono::{DateTime, Utc};
use serde_json::Value;

pub(crate) struct JsonlRecord {
    pub(crate) value: Value,
    pub(crate) raw: Vec<u8>,
    pub(crate) end_offset: u64,
}

pub(crate) struct JsonlBatch {
    pub(crate) records: Vec<JsonlRecord>,
    pub(crate) end_offset: u64,
    pub(crate) quarantined: u64,
}

const SOURCE_PREFIX_BYTES: u64 = 64 * 1024;

pub(crate) fn source_prefix_digest(path: &Path, length: u64) -> Result<String, AdapterError> {
    let mut file = File::open(path)?;
    let length = length.min(SOURCE_PREFIX_BYTES);
    let mut bytes = vec![0_u8; usize::try_from(length).unwrap_or(0)];
    file.read_exact(&mut bytes)?;
    Ok(format!("prefix-blake3:{}", blake3::hash(&bytes).to_hex()))
}

pub(crate) fn read_jsonl(
    path: &Path,
    byte_offset: u64,
    max_records: usize,
) -> Result<JsonlBatch, AdapterError> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(byte_offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut records = Vec::new();
    let mut quarantined = 0_u64;
    let mut consumed = 0_usize;
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        if records.len() >= max_records {
            break;
        }
        let has_newline = chunk.ends_with(b"\n");
        let line = chunk.strip_suffix(b"\n").unwrap_or(chunk);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            consumed += chunk.len();
            continue;
        }
        match serde_json::from_slice::<Value>(line) {
            Ok(value) => {
                consumed += chunk.len();
                records.push(JsonlRecord {
                    value,
                    raw: line.to_vec(),
                    end_offset: byte_offset + consumed as u64,
                });
            }
            Err(_) if !has_newline => break,
            Err(_) => {
                consumed += chunk.len();
                quarantined += 1;
            }
        }
    }
    Ok(JsonlBatch {
        records,
        end_offset: byte_offset + consumed as u64,
        quarantined,
    })
}

pub(crate) fn timestamp_us(value: Option<&str>) -> i64 {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp_micros())
        .unwrap_or_else(|| Utc::now().timestamp_micros())
}

pub(crate) fn text_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    part.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| part.get("content").and_then(Value::as_str))
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

pub(crate) fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str()
}

pub(crate) fn deterministic_uuid(value: &str) -> uuid::Uuid {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, value.as_bytes())
}
