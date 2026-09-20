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

pub(crate) async fn read_jsonl(
    path: &Path,
    byte_offset: u64,
    max_records: usize,
) -> Result<JsonlBatch, AdapterError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || read_jsonl_sync(&path, byte_offset, max_records))
        .await
        .map_err(|error| AdapterError::Parse(error.to_string()))?
}

fn read_jsonl_sync(
    path: &Path,
    byte_offset: u64,
    max_records: usize,
) -> Result<JsonlBatch, AdapterError> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(byte_offset))?;
    let mut bytes = Vec::new();
    const MAX_BATCH_BYTES: u64 = 8 * 1024 * 1024;
    file.take(MAX_BATCH_BYTES).read_to_end(&mut bytes)?;
    if bytes.len() as u64 == MAX_BATCH_BYTES && !bytes.contains(&b'\n') {
        return Err(AdapterError::UnsupportedSource(
            "JSONL record exceeds the 8 MiB limit".to_owned(),
        ));
    }
    let mut records = Vec::new();
    let mut quarantined = 0_u64;
    let mut consumed = 0_usize;
    for chunk in bytes.split_inclusive(|byte| *byte == b'\n') {
        if records.len() >= max_records {
            break;
        }
        let has_newline = chunk.ends_with(b"\n");
        if !has_newline && bytes.len() as u64 == MAX_BATCH_BYTES {
            break;
        }
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

#[cfg(test)]
mod tests {
    use super::read_jsonl_sync;

    #[test]
    fn oversized_jsonl_record_is_rejected_without_reading_whole_file() {
        let file = tempfile::NamedTempFile::new().expect("file");
        file.as_file()
            .set_len(1024 * 1024 * 1024)
            .expect("sparse source");
        assert!(read_jsonl_sync(file.path(), 0, 1000).is_err());
    }

    #[test]
    fn batch_cursor_stops_at_complete_record() {
        let file = tempfile::NamedTempFile::new().expect("file");
        std::fs::write(file.path(), b"{\"a\":1}\n{\"b\":2}\n{\"incomplete\":").expect("source");
        let first = read_jsonl_sync(file.path(), 0, 1).expect("first");
        assert_eq!(first.end_offset, 8);
        let second = read_jsonl_sync(file.path(), first.end_offset, 1000).expect("second");
        assert_eq!(second.records.len(), 1);
        assert_eq!(second.end_offset, 16);
    }
}
