//! Byte framing for a single record on durable storage:
//! `[len: u32 LE][crc32: u32 LE][payload; len bytes]`.
//!
//! This is where "assume fsync may lie; checksum everything" (design
//! section 9) becomes concrete code: a checksum is the only thing that can
//! tell recovery whether a record read back from disk is the one that was
//! actually written, rather than a torn or bit-flipped remnant of it.

const HEADER_LEN: usize = 8; // 4 bytes length + 4 bytes crc32

/// Frame `payload` for durable storage.
pub fn encode_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32(payload).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Decode every complete, checksum-valid frame from the front of `bytes`,
/// in order, stopping at the first problem: not enough bytes left for a
/// full header, not enough bytes left for the payload the header claims
/// (a torn write), or a checksum mismatch (a bit flip, or corruption of
/// any other kind).
///
/// Returns the decoded payloads and the number of bytes consumed by them.
/// `bytes.len() - consumed` is exactly the size of the torn/corrupt tail
/// that recovery must discard.
pub fn decode_frames(bytes: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let mut frames = Vec::new();
    let mut offset = 0;

    loop {
        if bytes.len() - offset < HEADER_LEN {
            break; // not even a full header left: torn write, or clean EOF
        }
        let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let expected_crc = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());

        let payload_start = offset + HEADER_LEN;
        let payload_end = payload_start + len;
        if payload_end > bytes.len() {
            break; // header claims more payload than is actually present: torn write
        }

        let payload = &bytes[payload_start..payload_end];
        if crc32(payload) != expected_crc {
            break; // bit flip or other corruption: don't trust this frame or anything after it
        }

        frames.push(payload.to_vec());
        offset = payload_end;
    }

    (frames, offset)
}

/// CRC-32 (IEEE 802.3 polynomial), computed byte-at-a-time. Hand-rolled
/// rather than pulled in as a dependency: it's ~15 lines, carries no
/// dependency-resolution risk, and this crate is not performance-critical
/// on the checksum path -- throughput is bound by fsync round-trips, not
/// checksum computation (design section 9).
fn crc32(data: &[u8]) -> u32 {
    const POLY: u32 = 0xEDB8_8320;
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (POLY & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_single_frame() {
        let payload = b"hello ledger";
        let framed = encode_frame(payload);
        let (frames, consumed) = decode_frames(&framed);
        assert_eq!(frames, vec![payload.to_vec()]);
        assert_eq!(consumed, framed.len());
    }

    #[test]
    fn round_trips_several_frames_concatenated() {
        let mut bytes = Vec::new();
        bytes.extend(encode_frame(b"one"));
        bytes.extend(encode_frame(b"two"));
        bytes.extend(encode_frame(b"three"));
        let (frames, consumed) = decode_frames(&bytes);
        assert_eq!(frames, vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn empty_input_decodes_to_nothing() {
        let (frames, consumed) = decode_frames(&[]);
        assert!(frames.is_empty());
        assert_eq!(consumed, 0);
    }

    #[test]
    fn stops_at_a_torn_tail() {
        let mut bytes = Vec::new();
        bytes.extend(encode_frame(b"complete"));
        let torn = encode_frame(b"incomplete-record");
        bytes.extend_from_slice(&torn[..torn.len() - 3]); // simulate a write cut short
        let (frames, consumed) = decode_frames(&bytes);
        assert_eq!(frames, vec![b"complete".to_vec()]);
        assert_eq!(consumed, encode_frame(b"complete").len());
        assert!(consumed < bytes.len());
    }

    #[test]
    fn stops_at_a_bit_flip() {
        let mut bytes = encode_frame(b"good");
        let mut corrupted = encode_frame(b"corrupted-payload");
        corrupted[10] ^= 0x01; // flip one bit inside the payload
        bytes.extend(corrupted);
        let (frames, consumed) = decode_frames(&bytes);
        assert_eq!(frames, vec![b"good".to_vec()]);
        assert_eq!(consumed, encode_frame(b"good").len());
    }

    #[test]
    fn crc32_matches_known_test_vector() {
        // The canonical CRC-32/IEEE-802.3 test vector.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
