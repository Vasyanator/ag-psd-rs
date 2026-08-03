/*
File: crates/ag-psd/src/utf8.rs

Purpose:
UTF-8 encoding and decoding for PSD string payloads.

Source compatibility:
- port of the upstream file `test/ag-psd/src/utf8.ts` (1:1 module split).

Main responsibilities:
- mirror the upstream module's public contract;
- keep the encode/decode contract of this area in one place.

Mapping TS -> Rust:
- `charLengthInBytes(code)`        -> `char_length_in_bytes(code: u32) -> usize`
- `stringLengthInBytes(value)`     -> `string_length_in_bytes(value: &str) -> usize`
- `writeCharacter(buf, off, code)` -> `write_character(buffer: &mut [u8], offset: usize, code: u32) -> usize`
- `encodeStringTo(buf, off, val)`  -> `encode_string_to(buffer: &mut [u8], offset: usize, value: &str) -> usize`
- `encodeString(value)`            -> `encode_string(value: &str) -> Vec<u8>`
- `decodeString(value)`            -> `decode_string(value: &[u8]) -> String`
- `codePointAt(value, i)`          -> no equivalent, see below

Notes on faithfulness:
- Upstream iterates UTF-16 code units (`charCodeAt`) and assembles surrogate pairs by
  hand before encoding to UTF-8. Its `codePointAt` helper exists to map an *unpaired*
  surrogate to U+FFFD, because a JS string may contain one. Rust `&str` is guaranteed
  well-formed UTF-8 and `chars()` yields Unicode scalar values only, so an unpaired
  surrogate is unrepresentable and the helper has no meaning here: iterating `chars()`
  already produces exactly the byte sequence upstream's fixed encoder produces.
- `decode_string` follows the WHATWG Encoding Standard's non-fatal error mode (the
  behaviour of `TextDecoder`), which is exactly what `String::from_utf8_lossy`
  implements: every maximal malformed subsequence becomes a single U+FFFD instead of
  raising an error. The hand-written decoder that used to live here rejected malformed
  input; upstream deliberately stopped doing that, because a PSD produced by another
  tool may carry slightly broken UTF-8 in a string resource and must still be readable.
- Both upstream fast paths (`TextEncoder`/`TextDecoder` for inputs over 1000 units)
  are irrelevant here: there is a single code path, so the length-dependent divergence
  that upstream was fixing cannot occur.
*/

// PORT STATUS: ported

/// Byte length of the UTF-8 encoding of `code` (mirror of `charLengthInBytes`).
fn char_length_in_bytes(code: u32) -> usize {
    if (code & 0xffff_ff80) == 0 {
        1
    } else if (code & 0xffff_f800) == 0 {
        2
    } else if (code & 0xffff_0000) == 0 {
        3
    } else {
        4
    }
}

/// Number of bytes `value` occupies when UTF-8 encoded (mirror of `stringLengthInBytes`).
///
/// Guaranteed to equal `encode_string(value).len()`: `chars()` yields scalar values, so
/// unlike the JS original this pre-pass cannot disagree with the encoder over an
/// unpaired surrogate.
#[must_use]
pub fn string_length_in_bytes(value: &str) -> usize {
    let mut result = 0;
    for c in value.chars() {
        result += char_length_in_bytes(c as u32);
    }
    result
}

/// Write one code point into `buffer` starting at `offset`; returns the bytes written
/// (mirror of `writeCharacter`).
///
/// # Panics
/// Panics if `buffer` has fewer than `char_length_in_bytes(code)` bytes left after
/// `offset`. Callers size the buffer with `string_length_in_bytes` first.
fn write_character(buffer: &mut [u8], offset: usize, code: u32) -> usize {
    let length = char_length_in_bytes(code);

    match length {
        1 => {
            buffer[offset] = code as u8;
        }
        2 => {
            buffer[offset] = (((code >> 6) & 0x1f) | 0xc0) as u8;
            buffer[offset + 1] = ((code & 0x3f) | 0x80) as u8;
        }
        3 => {
            buffer[offset] = (((code >> 12) & 0x0f) | 0xe0) as u8;
            buffer[offset + 1] = (((code >> 6) & 0x3f) | 0x80) as u8;
            buffer[offset + 2] = ((code & 0x3f) | 0x80) as u8;
        }
        _ => {
            buffer[offset] = (((code >> 18) & 0x07) | 0xf0) as u8;
            buffer[offset + 1] = (((code >> 12) & 0x3f) | 0x80) as u8;
            buffer[offset + 2] = (((code >> 6) & 0x3f) | 0x80) as u8;
            buffer[offset + 3] = ((code & 0x3f) | 0x80) as u8;
        }
    }

    length
}

/// Encode `value` into `buffer` starting at `offset`; returns the offset past the last
/// byte written (mirror of `encodeStringTo`).
///
/// # Panics
/// Panics if `buffer` cannot hold `string_length_in_bytes(value)` bytes at `offset`.
#[must_use]
pub fn encode_string_to(buffer: &mut [u8], offset: usize, value: &str) -> usize {
    let mut offset = offset;
    for c in value.chars() {
        offset += write_character(buffer, offset, c as u32);
    }
    offset
}

/// Encode `value` into a freshly allocated `Vec<u8>` (mirror of `encodeString`).
///
/// The result is always identical to `value.as_bytes()`; the explicit encoder is kept
/// to mirror the upstream module structure and to keep `write_character` exercised.
#[must_use]
pub fn encode_string(value: &str) -> Vec<u8> {
    let mut buffer = vec![0u8; string_length_in_bytes(value)];
    let end = encode_string_to(&mut buffer, 0, value);
    // The invariant upstream's unpaired-surrogate bug used to break: the pre-pass that
    // sizes the buffer and the encoder that fills it must agree on the byte count.
    debug_assert_eq!(end, buffer.len());
    buffer
}

/// Decode UTF-8 bytes into a `String` (mirror of `decodeString`).
///
/// Malformed input never fails: following the WHATWG Encoding Standard's non-fatal
/// error mode, every maximal malformed subsequence — invalid lead byte, out-of-range
/// continuation byte, overlong form, encoded surrogate, out-of-range scalar value, and
/// a sequence truncated by the end of input — is replaced by U+FFFD. This matches
/// `TextDecoder` and therefore upstream's decoder exactly.
#[must_use]
pub fn decode_string(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- string_length_in_bytes (ported from upstream `utf8.spec.ts`) ----------------

    #[test]
    fn string_length_in_bytes_counts_ascii_as_one_byte() {
        assert_eq!(string_length_in_bytes(""), 0);
        assert_eq!(string_length_in_bytes("Hello"), 5);
    }

    #[test]
    fn string_length_in_bytes_counts_polish_diacritics_as_two_bytes() {
        assert_eq!(string_length_in_bytes("ą"), 2);
        assert_eq!(string_length_in_bytes("ąćęłńóśźż"), 9 * 2);
    }

    #[test]
    fn string_length_in_bytes_counts_cjk_as_three_bytes() {
        assert_eq!(string_length_in_bytes("你好世界"), 4 * 3);
        assert_eq!(string_length_in_bytes("こんにちは"), 5 * 3);
        assert_eq!(string_length_in_bytes("カタカナ"), 4 * 3);
        assert_eq!(string_length_in_bytes("漢字"), 2 * 3);
    }

    #[test]
    fn string_length_in_bytes_counts_emoji_as_four_bytes() {
        assert_eq!(string_length_in_bytes("😀"), 4);
        assert_eq!(string_length_in_bytes("😀🎉👍"), 3 * 4);
    }

    #[test]
    fn string_length_in_bytes_matches_encode_string() {
        // Upstream's regression guard: the pre-pass that sizes the buffer must agree
        // with the encoder that fills it, for every sample.
        let samples = [
            "Hello",
            "Zażółć gęślą jaźń",
            "你好，世界",
            "こんにちは世界",
            "😀🎉👨‍👩‍👧‍👦",
        ];

        for sample in samples {
            assert_eq!(string_length_in_bytes(sample), encode_string(sample).len(), "{sample}");
            assert_eq!(string_length_in_bytes(sample), sample.len(), "{sample}");
        }
    }

    // -- encode/decode round trips ---------------------------------------------------

    #[test]
    fn round_trips_upstream_samples() {
        let samples = [
            ("empty string", ""),
            ("ascii", "The quick brown fox jumps over the lazy dog."),
            ("polish", "Zażółć gęślą jaźń"),
            ("chinese", "你好，世界！这是一段中文文本。"),
            ("japanese", "こんにちは世界、これは日本語のテキストです。"),
            ("emoji", "😀🎉👍🍕🚀"),
            ("emoji with skin tone modifier", "👍🏽"),
            ("emoji with ZWJ sequence (family)", "👨‍👩‍👧‍👦"),
            ("mixed scripts and emoji", "Hello Zażółć 你好 こんにちは 😀"),
        ];

        for (name, value) in samples {
            let encoded = encode_string(value);
            // The Rust encoder must agree with the standard UTF-8 representation, which
            // is what upstream checks against `TextEncoder`.
            assert_eq!(encoded, value.as_bytes(), "{name}");
            assert_eq!(decode_string(&encoded), value, "{name}");
        }
    }

    #[test]
    fn ascii_round_trip() {
        let s = "Hello, World!";
        let bytes = encode_string(s);
        assert_eq!(bytes, s.as_bytes());
        assert_eq!(decode_string(&bytes), s);
    }

    #[test]
    fn empty_round_trip() {
        let s = "";
        let bytes = encode_string(s);
        assert!(bytes.is_empty());
        assert_eq!(string_length_in_bytes(s), 0);
        assert_eq!(decode_string(&bytes), s);
    }

    #[test]
    fn cyrillic_round_trip() {
        let s = "Привет";
        let bytes = encode_string(s);
        // Each Cyrillic letter is 2 bytes in UTF-8 -> 6 chars * 2 = 12 bytes.
        assert_eq!(bytes.len(), 12);
        assert_eq!(string_length_in_bytes(s), 12);
        // "Пр" = D0 9F D1 80
        assert_eq!(&bytes[0..4], &[0xD0, 0x9F, 0xD1, 0x80]);
        assert_eq!(bytes, s.as_bytes());
        assert_eq!(decode_string(&bytes), s);
    }

    #[test]
    fn three_byte_round_trip() {
        let s = "あ"; // U+3042, 3 bytes
        let bytes = encode_string(s);
        assert_eq!(bytes, vec![0xE3, 0x81, 0x82]);
        assert_eq!(string_length_in_bytes(s), 3);
        assert_eq!(decode_string(&bytes), s);
    }

    #[test]
    fn emoji_surrogate_pair_round_trip() {
        let s = "😀"; // U+1F600, 4 bytes, surrogate pair in UTF-16
        let bytes = encode_string(s);
        assert_eq!(bytes, vec![0xF0, 0x9F, 0x98, 0x80]);
        assert_eq!(string_length_in_bytes(s), 4);
        assert_eq!(decode_string(&bytes), s);
    }

    #[test]
    fn mixed_round_trip() {
        let s = "aЯ あ😀z";
        let bytes = encode_string(s);
        assert_eq!(bytes, s.as_bytes());
        assert_eq!(string_length_in_bytes(s), s.len());
        assert_eq!(decode_string(&bytes), s);
    }

    #[test]
    fn encode_string_to_writes_at_offset_and_returns_new_offset() {
        let mut buffer = [0xffu8; 20];
        let offset = encode_string_to(&mut buffer, 3, "ą😀");

        // 'ą' -> 2 bytes, '😀' -> 4 bytes.
        assert_eq!(offset, 3 + 2 + 4);
        assert_eq!(&buffer[0..3], &[0xff, 0xff, 0xff]);
        assert_eq!(&buffer[3..9], "ą😀".as_bytes());
        assert_eq!(&buffer[9..], &[0xffu8; 11]);
    }

    // -- malformed input: WHATWG non-fatal error mode ---------------------------------
    //
    // Every expectation below was cross-checked against Node's `TextDecoder`, the same
    // oracle upstream's spec uses. These replaced the previous tests, which asserted
    // that the decoder rejects malformed input; upstream deliberately changed that
    // contract, so the old expectations no longer describe correct behaviour.

    #[test]
    fn decode_replaces_invalid_lead_byte() {
        assert_eq!(decode_string(&[0x61, 0xff, 0x62]), "a\u{FFFD}b");
    }

    #[test]
    fn decode_replaces_lone_continuation_byte() {
        assert_eq!(decode_string(&[0x80]), "\u{FFFD}");
    }

    #[test]
    fn decode_replaces_invalid_continuation_byte_and_resyncs() {
        // The out-of-range byte is re-processed as a fresh sequence start, so '(' survives.
        assert_eq!(decode_string(&[0x61, 0xe2, 0x28, 0xa1]), "a\u{FFFD}(\u{FFFD}");
        assert_eq!(decode_string(&[0xc2, 0x20]), "\u{FFFD} ");
    }

    #[test]
    fn decode_replaces_truncated_sequence_at_end_of_input() {
        assert_eq!(decode_string(&[0x61, 0xe2, 0x82]), "a\u{FFFD}");
        assert_eq!(decode_string(&[0xE3, 0x81]), "\u{FFFD}");
    }

    #[test]
    fn decode_replaces_overlong_two_byte_sequence() {
        // C0 80 is the overlong encoding of NUL; C0 is not a valid lead byte at all.
        assert_eq!(decode_string(&[0xc0, 0x80]), "\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn decode_replaces_overlong_four_byte_sequence() {
        // F0 has lower boundary 0x90, so 80 is rejected and each byte becomes U+FFFD.
        assert_eq!(decode_string(&[0xf0, 0x80, 0x80, 0x80]), "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn decode_replaces_encoded_surrogate() {
        // ED A0 80 encodes U+D800, which is not a scalar value; ED caps at 0x9f.
        assert_eq!(decode_string(&[0xED, 0xA0, 0x80]), "\u{FFFD}\u{FFFD}\u{FFFD}");
    }

    #[test]
    fn decode_replaces_out_of_range_four_byte_sequence() {
        // F4 90 80 80 encodes U+110000, past the last scalar value; F4 caps at 0x8f.
        assert_eq!(
            decode_string(&[0xf4, 0x90, 0x80, 0x80]),
            "\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}"
        );
    }

    #[test]
    fn decode_long_malformed_buffer_matches_short_one() {
        // Upstream had a >1000-byte `TextDecoder` fast path that disagreed with its
        // hand-written decoder. This port has one code path; the test pins that down so
        // no length-dependent shortcut can be reintroduced unnoticed.
        let short = decode_string(&[0x61, 0xff, 0x62]);
        let mut long_bytes = "x".repeat(2000).into_bytes();
        long_bytes.extend_from_slice(&[0x61, 0xff, 0x62]);
        let long = decode_string(&long_bytes);

        assert_eq!(short, "a\u{FFFD}b");
        assert_eq!(&long[2000..], "a\u{FFFD}b");
    }
}
