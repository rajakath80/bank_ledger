//! Newline-delimited JSON encoding for records.
//!
//! JSON is chosen over a binary format for this milestone because it makes
//! the forward-compatibility rule (fields added, never removed) and manual
//! log inspection trivial. A denser binary encoding can replace this later
//! behind the same `encode`/`decode` pair, without `ledger-core` or the
//! replay logic ever needing to change.

use crate::record::Record;

#[derive(Debug)]
pub struct EncodeError(pub String);

#[derive(Debug)]
pub struct DecodeError(pub String);

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to encode record: {}", self.0)
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to decode record: {}", self.0)
    }
}

impl std::error::Error for EncodeError {}
impl std::error::Error for DecodeError {}

/// Encode a single record as raw bytes with no delimiter, suitable for
/// handing to `RecordStorage::append` -- the frame's own length prefix
/// delimits records on real storage, so no trailing newline is needed the
/// way [`encode`] adds one for the plain in-memory/newline-delimited log.
pub fn encode_payload(record: &Record) -> Result<Vec<u8>, EncodeError> {
    serde_json::to_vec(record).map_err(|e| EncodeError(e.to_string()))
}

/// Decode a single record from raw payload bytes (the inverse of
/// [`encode_payload`]).
pub fn decode_payload(payload: &[u8]) -> Result<Record, DecodeError> {
    serde_json::from_slice(payload).map_err(|e| DecodeError(e.to_string()))
}

/// Encode a single record as one line of JSON, newline-terminated.
pub fn encode(record: &Record) -> Result<Vec<u8>, EncodeError> {
    let mut bytes = serde_json::to_vec(record).map_err(|e| EncodeError(e.to_string()))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decode a single line (without its trailing newline) back into a record.
pub fn decode(line: &[u8]) -> Result<Record, DecodeError> {
    serde_json::from_slice(line).map_err(|e| DecodeError(e.to_string()))
}

/// Encode a whole log as newline-delimited JSON, in order.
pub fn encode_log(records: &[Record]) -> Result<Vec<u8>, EncodeError> {
    let mut out = Vec::new();
    for record in records {
        out.extend(encode(record)?);
    }
    Ok(out)
}

/// Decode a newline-delimited JSON log back into records, in order. Blank
/// lines are skipped, so a trailing newline (which `encode_log` always
/// produces) does not decode as an extra empty record.
pub fn decode_log(bytes: &[u8]) -> Result<Vec<Record>, DecodeError> {
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(decode)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ledger_core::Command;
    use ledger_types::ids::AccountId;
    use ledger_types::time::LogTime;

    #[test]
    fn round_trips_a_single_record() {
        let record = Record::v1(
            LogTime(3),
            Command::OpenAccount {
                account: AccountId(7),
            },
        );
        let bytes = encode(&record).unwrap();
        assert_eq!(bytes.last(), Some(&b'\n'));
        let decoded = decode(&bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(record, decoded);
    }

    #[test]
    fn round_trips_a_log_and_ignores_trailing_newline() {
        let records = vec![
            Record::v1(
                LogTime(0),
                Command::OpenAccount {
                    account: AccountId(1),
                },
            ),
            Record::v1(
                LogTime(1),
                Command::OpenAccount {
                    account: AccountId(2),
                },
            ),
            Record::v1(LogTime(2), Command::Tick),
        ];
        let bytes = encode_log(&records).unwrap();
        let decoded = decode_log(&bytes).unwrap();
        assert_eq!(records, decoded);
    }
}
