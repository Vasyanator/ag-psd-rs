/*
File: crates/ag-psd/src/utf8.rs

Purpose:
кодирование и декодирование UTF-8 строк.

Source compatibility:
- порт upstream-файла `test/ag-psd/src/utf8.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.

Mapping TS -> Rust:
- `charLengthInBytes(code)`        -> `char_length_in_bytes(code: u32) -> usize`
- `stringLengthInBytes(value)`     -> `string_length_in_bytes(value: &str) -> usize`
- `writeCharacter(buf, off, code)` -> `write_character(buffer: &mut [u8], offset: usize, code: u32) -> usize`
- `encodeStringTo(buf, off, val)`  -> `encode_string_to(buffer: &mut [u8], offset: usize, value: &str) -> usize`
- `encodeString(value)`            -> `encode_string(value: &str) -> Vec<u8>`
- `decodeString(value)`            -> `decode_string(value: &[u8]) -> Result<String, Utf8DecodeError>`

Notes on faithfulness:
- Upstream работает на UTF-16 code units (`charCodeAt`) и вручную собирает суррогатные
  пары перед кодированием в UTF-8. В Rust `&str` уже хранит валидный UTF-8, поэтому
  ручная посимвольная сборка по `char`-ам (scalar values) даёт ровно те же байты, что и
  ветвь `charLengthInBytes`/`writeCharacter` для любой валидной строки. Суррогаты в `&str`
  невозможны, так что ветка склейки high/low surrogate автоматически выполняется самим Rust.
- `decodeString` повторяет ручной UTF-8-декодер upstream'а, включая отклонение overlong-
  последовательностей, одиночных суррогатов и кодов вне диапазона. Вместо `String::from_utf8`
  оставлен ручной разбор, чтобы воспроизвести точную семантику ошибок upstream'а
  (одиночные суррогаты, проверка границ continuation-байтов и т.п.).
*/

// PORT STATUS: ported

/// Ошибка ручного UTF-8-декодера (зеркало `throw Error(...)` из upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Utf8DecodeError {
    InvalidByteIndex,
    InvalidContinuationByte,
    LoneSurrogate(u32),
    InvalidUtf8Detected,
}

impl std::fmt::Display for Utf8DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Utf8DecodeError::InvalidByteIndex => write!(f, "Invalid byte index"),
            Utf8DecodeError::InvalidContinuationByte => write!(f, "Invalid continuation byte"),
            Utf8DecodeError::LoneSurrogate(code) => write!(
                f,
                "Lone surrogate U+{} is not a scalar value",
                format!("{:X}", code)
            ),
            Utf8DecodeError::InvalidUtf8Detected => write!(f, "Invalid UTF-8 detected"),
        }
    }
}

impl std::error::Error for Utf8DecodeError {}

/// Длина в байтах для UTF-8-кодирования code point'а (зеркало `charLengthInBytes`).
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

/// Сколько байт займёт строка в UTF-8 (зеркало `stringLengthInBytes`).
///
/// В TS итерация идёт по UTF-16 code unit'ам со сборкой суррогатов; в Rust `chars()`
/// уже отдаёт scalar values, поэтому `char_length_in_bytes(c as u32)` эквивалентно.
pub fn string_length_in_bytes(value: &str) -> usize {
    let mut result = 0;
    for c in value.chars() {
        result += char_length_in_bytes(c as u32);
    }
    result
}

/// Записать один code point в `buffer` начиная с `offset`, вернуть число записанных байт
/// (зеркало `writeCharacter`).
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

/// Закодировать строку в `buffer` начиная с `offset`, вернуть новый offset
/// (зеркало `encodeStringTo`).
pub fn encode_string_to(buffer: &mut [u8], offset: usize, value: &str) -> usize {
    let mut offset = offset;
    for c in value.chars() {
        offset += write_character(buffer, offset, c as u32);
    }
    offset
}

/// Закодировать строку в новый `Vec<u8>` (зеркало `encodeString`).
///
/// Upstream для длинных строк (>1000) делегирует в `TextEncoder` (стандартный UTF-8);
/// ручная ветка для валидных строк даёт ровно те же байты, так что результат идентичен
/// вне зависимости от длины.
pub fn encode_string(value: &str) -> Vec<u8> {
    let mut buffer = vec![0u8; string_length_in_bytes(value)];
    encode_string_to(&mut buffer, 0, value);
    buffer
}

/// Прочитать continuation-байт по индексу `index` (зеркало `continuationByte`).
fn continuation_byte(buffer: &[u8], index: usize) -> Result<u32, Utf8DecodeError> {
    if index >= buffer.len() {
        return Err(Utf8DecodeError::InvalidByteIndex);
    }

    let continuation_byte = buffer[index];

    if (continuation_byte & 0xC0) == 0x80 {
        Ok((continuation_byte & 0x3F) as u32)
    } else {
        Err(Utf8DecodeError::InvalidContinuationByte)
    }
}

/// Декодировать UTF-8 байты в строку (зеркало `decodeString`).
///
/// Ручной разбор сохранён намеренно (вместо `String::from_utf8`), чтобы повторить точную
/// семантику ошибок upstream'а.
pub fn decode_string(value: &[u8]) -> Result<String, Utf8DecodeError> {
    let mut result = String::new();
    let mut i = 0usize;

    while i < value.len() {
        let byte1 = value[i] as u32;
        i += 1;
        let code: u32;

        if (byte1 & 0x80) == 0 {
            code = byte1;
        } else if (byte1 & 0xe0) == 0xc0 {
            let byte2 = continuation_byte(value, i)?;
            i += 1;
            code = ((byte1 & 0x1f) << 6) | byte2;

            if code < 0x80 {
                return Err(Utf8DecodeError::InvalidContinuationByte);
            }
        } else if (byte1 & 0xf0) == 0xe0 {
            let byte2 = continuation_byte(value, i)?;
            i += 1;
            let byte3 = continuation_byte(value, i)?;
            i += 1;
            code = ((byte1 & 0x0f) << 12) | (byte2 << 6) | byte3;

            if code < 0x0800 {
                return Err(Utf8DecodeError::InvalidContinuationByte);
            }

            if (0xd800..=0xdfff).contains(&code) {
                return Err(Utf8DecodeError::LoneSurrogate(code));
            }
        } else if (byte1 & 0xf8) == 0xf0 {
            let byte2 = continuation_byte(value, i)?;
            i += 1;
            let byte3 = continuation_byte(value, i)?;
            i += 1;
            let byte4 = continuation_byte(value, i)?;
            i += 1;
            code = ((byte1 & 0x0f) << 0x12) | (byte2 << 0x0c) | (byte3 << 0x06) | byte4;

            if code < 0x01_0000 || code > 0x10_ffff {
                return Err(Utf8DecodeError::InvalidContinuationByte);
            }
        } else {
            return Err(Utf8DecodeError::InvalidUtf8Detected);
        }

        // Upstream собирает UTF-16-строку (включая суррогатную пару для code > 0xFFFF).
        // В Rust строка хранит scalar values, поэтому достаточно `char::from_u32`.
        // Код к этому моменту уже провалидирован как валидный scalar value.
        if let Some(ch) = char::from_u32(code) {
            result.push(ch);
        } else {
            // Недостижимо для прошедших проверки кодов, но на всякий случай.
            return Err(Utf8DecodeError::InvalidUtf8Detected);
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_round_trip() {
        let s = "Hello, World!";
        let bytes = encode_string(s);
        assert_eq!(bytes, s.as_bytes());
        assert_eq!(decode_string(&bytes).unwrap(), s);
    }

    #[test]
    fn empty_round_trip() {
        let s = "";
        let bytes = encode_string(s);
        assert!(bytes.is_empty());
        assert_eq!(string_length_in_bytes(s), 0);
        assert_eq!(decode_string(&bytes).unwrap(), s);
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
        assert_eq!(decode_string(&bytes).unwrap(), s);
    }

    #[test]
    fn three_byte_round_trip() {
        let s = "あ"; // U+3042, 3 bytes
        let bytes = encode_string(s);
        assert_eq!(bytes, vec![0xE3, 0x81, 0x82]);
        assert_eq!(string_length_in_bytes(s), 3);
        assert_eq!(decode_string(&bytes).unwrap(), s);
    }

    #[test]
    fn emoji_surrogate_pair_round_trip() {
        let s = "😀"; // U+1F600, 4 bytes, surrogate pair in UTF-16
        let bytes = encode_string(s);
        assert_eq!(bytes, vec![0xF0, 0x9F, 0x98, 0x80]);
        assert_eq!(string_length_in_bytes(s), 4);
        assert_eq!(decode_string(&bytes).unwrap(), s);
    }

    #[test]
    fn mixed_round_trip() {
        let s = "aЯ あ😀z";
        let bytes = encode_string(s);
        assert_eq!(bytes, s.as_bytes());
        assert_eq!(string_length_in_bytes(s), s.len());
        assert_eq!(decode_string(&bytes).unwrap(), s);
    }

    #[test]
    fn decode_rejects_lone_surrogate() {
        // ED A0 80 -> U+D800, a lone surrogate (upstream throws).
        let err = decode_string(&[0xED, 0xA0, 0x80]).unwrap_err();
        assert_eq!(err, Utf8DecodeError::LoneSurrogate(0xD800));
    }

    #[test]
    fn decode_rejects_overlong_two_byte() {
        // C0 80 would decode to U+0000 (overlong) -> code < 0x80.
        let err = decode_string(&[0xC0, 0x80]).unwrap_err();
        assert_eq!(err, Utf8DecodeError::InvalidContinuationByte);
    }

    #[test]
    fn decode_rejects_bad_continuation() {
        // C2 followed by a non-continuation byte.
        let err = decode_string(&[0xC2, 0x20]).unwrap_err();
        assert_eq!(err, Utf8DecodeError::InvalidContinuationByte);
    }

    #[test]
    fn decode_rejects_truncated() {
        // E3 81 missing third byte -> index out of range.
        let err = decode_string(&[0xE3, 0x81]).unwrap_err();
        assert_eq!(err, Utf8DecodeError::InvalidByteIndex);
    }

    #[test]
    fn decode_rejects_invalid_lead() {
        let err = decode_string(&[0xFF]).unwrap_err();
        assert_eq!(err, Utf8DecodeError::InvalidUtf8Detected);
    }
}
