/*
File: crates/ag-psd/src/reader.rs

Purpose:
низкоуровневое чтение байтов из буфера PSD (курсор чтения, примитивы чтения чисел и строк).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/psdReader.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.
*/

// PORT STATUS: primitives ported; document orchestration pending
//
// Ported: the low-level byte/string/section reader primitives that operate only
// on the reader buffer/offset plus scalar/length args. Deferred (require the full
// Psd/Layer document shape, descriptor/additionalInfo/imageResources handlers):
//   readPsd, readLayerInfo, readLayerRecord, readLayerMaskData,
//   readLayerBlendingRanges, readLayerChannelImageData, decodeLayerImageData,
//   readData, readDataRaw/Zip/RLE, readGlobalLayerMaskInfo,
//   readAdditionalLayerInfo, readImageData, readColor, readPattern,
//   createImageDataBitDepth and the cmyk/indexed/grayscale pixel helpers.
// See `// TODO: orchestration, later task` markers below.

//! # Endianness
//!
//! PSD is **big-endian**. Upstream calls `DataView.getInt16/getUint16/getInt32/
//! getUint32/getFloat32/getFloat64` with the `littleEndian` argument either
//! omitted or explicitly `false` (e.g. `getInt16(off, false)`), which means
//! big-endian. The few `*LE` variants pass `true`. This port reproduces that:
//! all default readers use `from_be_bytes`, the `_le` variants use
//! `from_le_bytes`.
//!
//! # Error strategy
//!
//! Upstream throws `Error` in a handful of places (`checkSignature`,
//! `readSection` overflow, `warnOrThrow` when `strict`, the >100MB guard in
//! `readBytes`). It also reads past the end of a slice in some "broken file"
//! recovery paths. We model fallible operations as `Result<T, ReadError>` with
//! a small crate-local [`ReadError`] enum, returned consistently from every
//! primitive that can fail. This is preferred over panicking because callers
//! (the future document orchestration) need to distinguish recoverable from
//! fatal conditions, mirroring upstream's `strict`/`warnOrThrow` split.

use crate::psd::ReadOptions;

/// Ошибки низкоуровневого ридера (зеркало `throw new Error(...)` из upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// Чтение за пределами буфера (зеркало `Reading bytes exceeding buffer length`
    /// в strict-режиме, и общая защита границ в Rust-порте).
    UnexpectedEndOfBuffer,
    /// Защита `Reading past end of file` (length > 100MB).
    ReadingPastEndOfFile,
    /// `checkSignature`: подпись не совпала ни с `a`, ни с `b`.
    InvalidSignature { signature: String, offset: usize },
    /// `readSection`: длина > 4GB при чтении 8-байтового размера.
    SizeTooLarge,
    /// `readSection`: секция выходит за пределы буфера.
    SectionExceedsFileSize,
    /// `warnOrThrow` в strict-режиме (`Exceeded section limits` / `Unread section data`).
    StrictViolation(String),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::UnexpectedEndOfBuffer => write!(f, "Reading bytes exceeding buffer length"),
            ReadError::ReadingPastEndOfFile => write!(f, "Reading past end of file"),
            ReadError::InvalidSignature { signature, offset } => {
                write!(f, "Invalid signature: '{}' at 0x{:x}", signature, offset)
            }
            ReadError::SizeTooLarge => write!(f, "Sizes larger than 4GB are not supported"),
            ReadError::SectionExceedsFileSize => write!(f, "Section exceeds file size"),
            ReadError::StrictViolation(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for ReadError {}

/// Результат операций ридера.
pub type ReadResult<T> = Result<T, ReadError>;

/// Состояние низкоуровневого ридера (зеркало TS `interface PsdReader extends ReadOptions`).
///
/// ## Borrow vs owned
///
/// Буфер хранится как **заимствованный срез** `&'a [u8]`. Upstream держит
/// `DataView` поверх существующего `ArrayBuffer` и читает строго вперёд по
/// `offset`; данные он не модифицирует и владения ими не требует. Заимствованный
/// срез даёт ту же zero-copy семантику (`readBytes` возвращает под-срез исходного
/// буфера, как `new Uint8Array(buffer, start, length)` в upstream), без аллокаций
/// и без лишнего клонирования. Поэтому `&'a [u8]` предпочтительнее `Vec<u8>`.
///
/// TS `DataView(buffer, offset, length)` сдвигает базу представления; здесь это
/// учтено тем, что вызывающий передаёт уже подрезанный срез (как
/// `createReader(buffer, offset, length)` создаёт view на под-диапазон). Поле
/// `offset` — позиция курсора внутри `buffer`, ровно как `reader.offset`.
#[derive(Debug)]
pub struct PsdReader<'a> {
    pub buffer: &'a [u8],
    pub offset: usize,
    // зеркало ReadOptions-полей, которые upstream подмешивает в reader.
    pub strict: bool,
    pub debug: bool,
    pub large: bool,
    pub global_alpha: bool,
    pub options: ReadOptions,
}

impl<'a> PsdReader<'a> {
    /// Зеркало `createReader(buffer, offset?, length?)`.
    ///
    /// В upstream `offset`/`length` задают окно `DataView`; здесь это выражается
    /// под-срезом `buffer[offset..offset+length]`. Если `length` не задан — до
    /// конца буфера. Курсор (`offset`-поле) всегда начинается с 0 относительно
    /// окна, как в upstream (`offset: 0`).
    pub fn new(buffer: &'a [u8], offset: Option<usize>, length: Option<usize>) -> PsdReader<'a> {
        let start = offset.unwrap_or(0);
        let end = match length {
            Some(len) => start + len,
            None => buffer.len(),
        };
        PsdReader {
            buffer: &buffer[start..end],
            offset: 0,
            strict: false,
            debug: false,
            large: false,
            global_alpha: false,
            options: ReadOptions::default(),
        }
    }
}

/// Зеркало `warnOrThrow(reader, message)`.
///
/// В strict-режиме upstream бросает исключение — здесь возвращаем `Err`. Вне
/// strict (с `debug`) — просто логирование, которое мы опускаем как поведение,
/// не данные; возвращаем `Ok(())`.
pub fn warn_or_throw(reader: &PsdReader, message: &str) -> ReadResult<()> {
    if reader.strict {
        return Err(ReadError::StrictViolation(message.to_string()));
    }
    // `if (reader.debug) reader.log(message);` — лог опускаем.
    Ok(())
}

// ===========================================================================
// Scalar readers (big-endian, кроме *_le)
// ===========================================================================

#[inline]
fn ensure(reader: &PsdReader, len: usize) -> ReadResult<usize> {
    let start = reader.offset;
    if start + len > reader.buffer.len() {
        return Err(ReadError::UnexpectedEndOfBuffer);
    }
    Ok(start)
}

pub fn read_uint8(reader: &mut PsdReader) -> ReadResult<u8> {
    let start = ensure(reader, 1)?;
    reader.offset += 1;
    Ok(reader.buffer[start])
}

/// Зеркало `peekUint8` — читает без сдвига курсора.
pub fn peek_uint8(reader: &PsdReader) -> ReadResult<u8> {
    let start = ensure(reader, 1)?;
    Ok(reader.buffer[start])
}

/// Upstream имеет `readInt8`? Нет отдельной функции, но задание просит её —
/// реализуем через интерпретацию байта как знакового (DataView.getInt8).
pub fn read_int8(reader: &mut PsdReader) -> ReadResult<i8> {
    Ok(read_uint8(reader)? as i8)
}

pub fn read_int16(reader: &mut PsdReader) -> ReadResult<i16> {
    let start = ensure(reader, 2)?;
    reader.offset += 2;
    Ok(i16::from_be_bytes([reader.buffer[start], reader.buffer[start + 1]]))
}

pub fn read_uint16(reader: &mut PsdReader) -> ReadResult<u16> {
    let start = ensure(reader, 2)?;
    reader.offset += 2;
    Ok(u16::from_be_bytes([reader.buffer[start], reader.buffer[start + 1]]))
}

/// Зеркало `readUint16LE` (little-endian).
pub fn read_uint16_le(reader: &mut PsdReader) -> ReadResult<u16> {
    let start = ensure(reader, 2)?;
    reader.offset += 2;
    Ok(u16::from_le_bytes([reader.buffer[start], reader.buffer[start + 1]]))
}

pub fn read_int32(reader: &mut PsdReader) -> ReadResult<i32> {
    let start = ensure(reader, 4)?;
    reader.offset += 4;
    Ok(i32::from_be_bytes([
        reader.buffer[start],
        reader.buffer[start + 1],
        reader.buffer[start + 2],
        reader.buffer[start + 3],
    ]))
}

/// Зеркало `readInt32LE` (little-endian).
pub fn read_int32_le(reader: &mut PsdReader) -> ReadResult<i32> {
    let start = ensure(reader, 4)?;
    reader.offset += 4;
    Ok(i32::from_le_bytes([
        reader.buffer[start],
        reader.buffer[start + 1],
        reader.buffer[start + 2],
        reader.buffer[start + 3],
    ]))
}

pub fn read_uint32(reader: &mut PsdReader) -> ReadResult<u32> {
    let start = ensure(reader, 4)?;
    reader.offset += 4;
    Ok(u32::from_be_bytes([
        reader.buffer[start],
        reader.buffer[start + 1],
        reader.buffer[start + 2],
        reader.buffer[start + 3],
    ]))
}

pub fn read_float32(reader: &mut PsdReader) -> ReadResult<f32> {
    let start = ensure(reader, 4)?;
    reader.offset += 4;
    Ok(f32::from_be_bytes([
        reader.buffer[start],
        reader.buffer[start + 1],
        reader.buffer[start + 2],
        reader.buffer[start + 3],
    ]))
}

pub fn read_float64(reader: &mut PsdReader) -> ReadResult<f64> {
    let start = ensure(reader, 8)?;
    reader.offset += 8;
    Ok(f64::from_be_bytes([
        reader.buffer[start],
        reader.buffer[start + 1],
        reader.buffer[start + 2],
        reader.buffer[start + 3],
        reader.buffer[start + 4],
        reader.buffer[start + 5],
        reader.buffer[start + 6],
        reader.buffer[start + 7],
    ]))
}

/// Зеркало `readFixedPoint32` — 32-битное число с фиксированной точкой 16.16.
pub fn read_fixed_point32(reader: &mut PsdReader) -> ReadResult<f64> {
    Ok(read_int32(reader)? as f64 / (1i64 << 16) as f64)
}

/// Зеркало `readFixedPointPath32` — 32-битное число с фиксированной точкой 8.24.
pub fn read_fixed_point_path32(reader: &mut PsdReader) -> ReadResult<f64> {
    Ok(read_int32(reader)? as f64 / (1i64 << 24) as f64)
}

/// Зеркало `readBytes(reader, length)`.
///
/// Upstream при выходе за конец буфера выдаёт предупреждение (или бросает в
/// strict), затем возвращает нулевой буфер нужной длины, частично заполненный
/// доступными байтами (фикс для битых PSD). Защита: length > 100MB → throw.
///
/// Возвращает `Vec<u8>`, а не срез: в обычном случае это `buffer[start..start+len]`
/// (как zero-copy под-Uint8Array в upstream), но ветка восстановления требует
/// собственного буфера, поэтому для единообразия сигнатуры возвращаем владеющий
/// `Vec`. Для zero-copy подреза есть отдельный [`read_bytes_slice`].
pub fn read_bytes(reader: &mut PsdReader, length: usize) -> ReadResult<Vec<u8>> {
    let start = reader.offset;
    reader.offset += length;

    if start + length > reader.buffer.len() {
        // фикс для битых PSD, где не хватает части файла в конце.
        warn_or_throw(reader, "Reading bytes exceeding buffer length")?;
        if length > 100 * 1024 * 1024 {
            return Err(ReadError::ReadingPastEndOfFile);
        }
        let mut result = vec![0u8; length];
        let avail = reader.buffer.len().saturating_sub(start);
        let len = length.min(avail);
        if len > 0 {
            result[..len].copy_from_slice(&reader.buffer[start..start + len]);
        }
        Ok(result)
    } else {
        Ok(reader.buffer[start..start + length].to_vec())
    }
}

/// Zero-copy вариант чтения байтов: возвращает под-срез исходного буфера.
///
/// Эквивалент успешной (не-восстановительной) ветки upstream'а
/// `new Uint8Array(reader.view.buffer, start, length)`. Ошибается, если выходит
/// за пределы буфера (восстановительной ветки тут нет — она требует аллокации).
pub fn read_bytes_slice<'a>(reader: &mut PsdReader<'a>, length: usize) -> ReadResult<&'a [u8]> {
    let start = ensure(reader, length)?;
    reader.offset += length;
    Ok(&reader.buffer[start..start + length])
}

/// Зеркало `skipBytes(reader, count)`.
pub fn skip_bytes(reader: &mut PsdReader, count: usize) {
    reader.offset += count;
}

// ===========================================================================
// String readers
// ===========================================================================

/// Зеркало приватной `readShortString(reader, length)`.
///
/// Upstream строит строку через `String.fromCharCode(byte)` для каждого байта —
/// то есть **каждый байт 0..=255 становится UTF-16 code unit'ом** (Latin-1-подобно),
/// это НЕ UTF-8-декодирование. Воспроизводим точно: каждый байт → `char`.
pub fn read_short_string(reader: &mut PsdReader, length: usize) -> ReadResult<String> {
    let buffer = read_bytes(reader, length)?;
    let mut result = String::with_capacity(buffer.len());
    for &b in &buffer {
        result.push(b as char); // char::from(u8) == fromCharCode для 0..=255
    }
    Ok(result)
}

/// Зеркало `readAsciiString(reader, length)`.
pub fn read_ascii_string(reader: &mut PsdReader, length: usize) -> ReadResult<String> {
    let mut result = String::with_capacity(length);
    for _ in 0..length {
        result.push(read_uint8(reader)? as char);
    }
    Ok(result)
}

/// Зеркало `readSignature(reader)` — 4-байтовая подпись.
pub fn read_signature(reader: &mut PsdReader) -> ReadResult<String> {
    read_short_string(reader, 4)
}

/// Зеркало `validSignatureAt(reader, offset)` — `8BIM`/`8B64` по абсолютному offset.
pub fn valid_signature_at(reader: &PsdReader, offset: usize) -> bool {
    if offset + 4 > reader.buffer.len() {
        return false;
    }
    let sig = &reader.buffer[offset..offset + 4];
    sig == b"8BIM" || sig == b"8B64"
}

/// Зеркало `readPascalString(reader, padTo)`.
///
/// Layout: 1 байт длины, затем `length` байт текста, затем padding так, чтобы
/// `(length + 1)` (счёт включает байт длины) был кратен `padTo`.
pub fn read_pascal_string(reader: &mut PsdReader, pad_to: usize) -> ReadResult<String> {
    let mut length = read_uint8(reader)? as usize;
    let text = if length != 0 {
        read_short_string(reader, length)?
    } else {
        String::new()
    };

    // `while (++length % padTo) reader.offset++;`
    loop {
        length += 1;
        if length % pad_to == 0 {
            break;
        }
        reader.offset += 1;
    }

    Ok(text)
}

/// Зеркало `readUnicodeString(reader)` — uint32 длина (в code unit'ах), затем строка.
pub fn read_unicode_string(reader: &mut PsdReader) -> ReadResult<String> {
    let length = read_uint32(reader)? as usize;
    read_unicode_string_with_length(reader, length)
}

/// Зеркало `readUnicodeStringWithLength(reader, length)` (big-endian uint16 code units).
///
/// Каждый code unit читается как `readUint16` и добавляется через
/// `String.fromCharCode`; финальный `\0` (значение 0 на последней позиции)
/// отбрасывается. См. [`push_code_unit`] о суррогатах.
pub fn read_unicode_string_with_length(
    reader: &mut PsdReader,
    length: usize,
) -> ReadResult<String> {
    let mut units: Vec<u16> = Vec::with_capacity(length);
    let mut remaining = length;
    while remaining > 0 {
        remaining -= 1;
        let value = read_uint16(reader)?;
        // `if (value || length > 0)` — убираем хвостовой \0 (последняя итерация).
        if value != 0 || remaining > 0 {
            units.push(value);
        }
    }
    Ok(utf16_units_to_string(&units))
}

/// Зеркало `readUnicodeStringWithLengthLE` (little-endian uint16 code units).
pub fn read_unicode_string_with_length_le(
    reader: &mut PsdReader,
    length: usize,
) -> ReadResult<String> {
    let mut units: Vec<u16> = Vec::with_capacity(length);
    let mut remaining = length;
    while remaining > 0 {
        remaining -= 1;
        let value = read_uint16_le(reader)?;
        if value != 0 || remaining > 0 {
            units.push(value);
        }
    }
    Ok(utf16_units_to_string(&units))
}

/// Сборка строки из UTF-16 code unit'ов.
///
/// Upstream аккумулирует JS-строку напрямую из `fromCharCode(unit)`, что
/// допускает одиночные суррогаты. Rust `String` хранит только валидные scalar
/// values, поэтому для битых/одиночных суррогатов используем
/// `decode_utf16` с заменой на U+FFFD — для всех корректных PSD-строк результат
/// идентичен upstream'у.
fn utf16_units_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// Зеркало `checkSignature(reader, a, b?)`.
///
/// Читает 4-байтовую подпись; если она не равна ни `a`, ни (опционально) `b` —
/// возвращает `Err(InvalidSignature)` (upstream `throw`).
pub fn check_signature(reader: &mut PsdReader, a: &str, b: Option<&str>) -> ReadResult<()> {
    let offset = reader.offset;
    let signature = read_signature(reader)?;

    if signature != a && Some(signature.as_str()) != b {
        return Err(ReadError::InvalidSignature { signature, offset });
    }
    Ok(())
}

// ===========================================================================
// Section helper
// ===========================================================================

/// Зеркало `readSection<T>(reader, round, func, skipEmpty = true, eightBytes = false)`.
///
/// Читает length-prefixed секцию, вызывает `func` с замыканием `left()`
/// (сколько байт осталось до конца секции), затем выравнивает курсор на конец
/// секции, округлённый так, чтобы `length` стал кратен `round`.
///
/// Логика округления воспроизведена ровно: `while (length % round) { length++; end++; }`.
///
/// `func` принимает `&mut PsdReader` и `&dyn Fn(&PsdReader) -> usize` (вычисление
/// `left()`). Поскольку Rust не даёт замыканию захватить `reader`, который
/// одновременно передаётся мутабельно в `func`, `left` принимает текущий ридер
/// явным аргументом — это эквивалент upstream'а, где `left` читает `reader.offset`.
pub fn read_section<T, F>(
    reader: &mut PsdReader,
    round: usize,
    func: F,
    skip_empty: bool,
    eight_bytes: bool,
) -> ReadResult<Option<T>>
where
    F: FnOnce(&mut PsdReader, &dyn Fn(&PsdReader) -> usize) -> ReadResult<T>,
{
    let mut length = read_uint32(reader)? as usize;

    if eight_bytes {
        if length != 0 {
            return Err(ReadError::SizeTooLarge);
        }
        length = read_uint32(reader)? as usize;
    }

    // `if (length <= 0 && skipEmpty) return undefined;` (length unsigned → == 0)
    if length == 0 && skip_empty {
        return Ok(None);
    }

    let mut end = reader.offset + length;
    if end > reader.buffer.len() {
        return Err(ReadError::SectionExceedsFileSize);
    }

    let left = move |r: &PsdReader| end_minus_offset(end, r);
    let result = func(reader, &left)?;

    if reader.offset != end {
        if reader.offset > end {
            warn_or_throw(reader, "Exceeded section limits")?;
        } else {
            warn_or_throw(reader, "Unread section data")?;
        }
    }

    // `while (length % round) { length++; end++; }`
    while length % round != 0 {
        length += 1;
        end += 1;
    }

    reader.offset = end;

    Ok(Some(result))
}

#[inline]
fn end_minus_offset(end: usize, reader: &PsdReader) -> usize {
    end.saturating_sub(reader.offset)
}

/// Хелпер `peekUint32` — задача упоминает его наличие; в upstream отдельной
/// функции нет, но peek-семантика (чтение без сдвига курсора) полезна и
/// согласуется с `peekUint8`. Big-endian.
pub fn peek_uint32(reader: &PsdReader) -> ReadResult<u32> {
    let start = ensure(reader, 4)?;
    Ok(u32::from_be_bytes([
        reader.buffer[start],
        reader.buffer[start + 1],
        reader.buffer[start + 2],
        reader.buffer[start + 3],
    ]))
}

// ===========================================================================
// Document orchestration (port of psdReader.ts readPsd & friends)
// ===========================================================================

use crate::additional_info::{read_additional_info_key, ReadCtx};
use crate::helpers::{
    create_image_data, decode_bitmap, image_data_to_canvas, offset_for_channel,
    to_blend_mode, ColorSpace, LayerMaskFlags, MaskParams,
};
use crate::image_resources::read_image_resource;
use crate::psd::{
    Color, ColorMode, Compression, GlobalLayerMaskInfo, ImageResources, Layer, LayerAdditionalInfo,
    LayerMaskData, LayerRawData, LayerRawDataChannel, PatternInfo, PixelData, Cmyk, Grayscale, Hsb,
    Lab, PatternBounds, Rgb, ChannelId, SectionDividerType,
};

/// Internal per-channel `{ id, length }` (mirror of TS `ChannelInfo`).
#[derive(Debug, Clone, Copy)]
struct ChannelInfo {
    id: i16,
    length: usize,
}

/// Mirror `supportedColorModes`.
fn is_supported_color_mode(mode: u16) -> bool {
    matches!(mode, 0 | 1 | 3 | 2) // Bitmap, Grayscale, RGB, Indexed
}

fn color_mode_from_u16(mode: u16) -> Option<ColorMode> {
    Some(match mode {
        0 => ColorMode::Bitmap,
        1 => ColorMode::Grayscale,
        2 => ColorMode::Indexed,
        3 => ColorMode::Rgb,
        4 => ColorMode::Cmyk,
        7 => ColorMode::Multichannel,
        8 => ColorMode::Duotone,
        9 => ColorMode::Lab,
        _ => return None,
    })
}

fn channel_id_from_i16(id: i16) -> ChannelId {
    match id {
        0 => ChannelId::Color0,
        1 => ChannelId::Color1,
        2 => ChannelId::Color2,
        3 => ChannelId::Color3,
        -2 => ChannelId::UserMask,
        -3 => ChannelId::RealUserMask,
        // -1 transparency, and any unknown extra color channels (>3): treat as
        // transparency-like (offset_for_channel guards what actually lands).
        _ => ChannelId::Transparency,
    }
}

/// Pixel storage backing a `PixelData` during decode, tracking bit depth so the
/// channel codecs can write at the correct stride.
///
/// Upstream uses `Uint8ClampedArray` / `Uint16Array` / `Float32Array` views.
/// Here we keep a `Vec<u8>` of RGBA8 always (PixelData is RGBA8 in this port);
/// 16/32-bit source samples are down-converted to 8-bit on store so the public
/// `PixelData` stays RGBA8 (matching how this crate models pixels).
pub struct DecodeTarget {
    pub width: usize,
    pub height: usize,
    /// RGBA8 (or `channels`-wide) byte buffer.
    pub data: Vec<u8>,
    pub channels: usize,
}

impl DecodeTarget {
    pub fn rgba(width: usize, height: usize) -> DecodeTarget {
        DecodeTarget { width, height, data: vec![0u8; width * height * 4], channels: 4 }
    }
    pub fn wide(width: usize, height: usize, channels: usize) -> DecodeTarget {
        DecodeTarget { width, height, data: vec![0u8; width * height * channels], channels }
    }
    pub fn into_pixel_data(self) -> PixelData {
        PixelData { width: self.width as u32, height: self.height as u32, data: self.data }
    }
}

// ---------------------------------------------------------------------------
// readPsd
// ---------------------------------------------------------------------------

/// High-level entry point. Mirror of upstream `readPsd(reader, readOptions)`,
/// but takes a byte slice + options and builds a [`PsdReader`] internally.
pub fn read_psd(buffer: &[u8], options: &ReadOptions) -> ReadResult<crate::psd::Psd> {
    let mut reader = PsdReader::new(buffer, None, None);
    reader.options = options.clone();
    reader.strict = options.strict.unwrap_or(false);
    reader.debug = options.debug.unwrap_or(false);
    read_psd_from_reader(&mut reader)
}

/// Mirror of upstream `readPsd` operating on an existing reader (options must
/// already be set on the reader, as upstream does via `Object.assign`).
pub fn read_psd_from_reader(reader: &mut PsdReader) -> ReadResult<crate::psd::Psd> {
    // header
    check_signature(reader, "8BPS", None)?;
    let version = read_uint16(reader)?;
    if version != 1 && version != 2 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid PSD file version: {}",
            version
        )));
    }

    skip_bytes(reader, 6);
    let channels = read_uint16(reader)?;
    let height = read_uint32(reader)?;
    let width = read_uint32(reader)?;
    let bits_per_channel = read_uint16(reader)?;
    let color_mode_raw = read_uint16(reader)?;
    let max_size: u32 = if version == 1 { 30000 } else { 300000 };

    if width > max_size || height > max_size {
        return Err(ReadError::StrictViolation(format!(
            "Invalid size: {}x{}",
            width, height
        )));
    }
    if channels > 16 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid channel count: {}",
            channels
        )));
    }
    if ![1, 8, 16, 32].contains(&bits_per_channel) {
        return Err(ReadError::StrictViolation(format!(
            "Invalid bitsPerChannel: {}",
            bits_per_channel
        )));
    }
    if !is_supported_color_mode(color_mode_raw) {
        return Err(ReadError::StrictViolation(format!(
            "Color mode not supported: {}",
            color_mode_raw
        )));
    }

    let color_mode = color_mode_from_u16(color_mode_raw);

    let mut psd = crate::psd::Psd {
        width: width as f64,
        height: height as f64,
        channels: Some(channels as f64),
        bits_per_channel: Some(bits_per_channel as f64),
        color_mode,
        ..Default::default()
    };

    reader.large = version == 2;
    reader.global_alpha = false;

    // color mode data
    let palette = read_section(
        reader,
        1,
        |reader, left| {
            if left(reader) == 0 {
                return Ok(None);
            }
            let mut palette: Option<Vec<Rgb>> = None;
            if color_mode == Some(ColorMode::Indexed) {
                if left(reader) != 768 {
                    return Err(ReadError::StrictViolation(
                        "Invalid color palette size".to_string(),
                    ));
                }
                let mut pal: Vec<Rgb> = Vec::with_capacity(256);
                for _ in 0..256 {
                    pal.push(Rgb { r: read_uint8(reader)? as f64, g: 0.0, b: 0.0 });
                }
                for i in 0..256 {
                    pal[i].g = read_uint8(reader)? as f64;
                }
                for i in 0..256 {
                    pal[i].b = read_uint8(reader)? as f64;
                }
                palette = Some(pal);
            }
            skip_bytes(reader, left(reader));
            Ok(palette)
        },
        true,
        false,
    )?;
    if let Some(Some(p)) = palette {
        psd.palette = Some(p);
    }

    // image resources
    let mut image_resources = ImageResources::default();
    read_section(
        reader,
        1,
        |reader, left| {
            while left(reader) > 0 {
                realign_with_signature(reader, is_valid_image_resource_signature)?;
                let id = read_uint16(reader)?;
                read_pascal_string(reader, 2)?; // name

                read_section(
                    reader,
                    2,
                    |reader, left| {
                        let skip = id == 1036 && reader.options.skip_thumbnail == Some(true);
                        let throw_for_missing =
                            reader.options.throw_for_missing_features == Some(true);
                        let block_len = left(reader);
                        if !skip {
                            match read_image_resource(id, reader, &mut image_resources, block_len) {
                                Ok(()) => {}
                                Err(e) => {
                                    if throw_for_missing {
                                        return Err(e);
                                    }
                                    skip_bytes(reader, left(reader));
                                }
                            }
                        } else {
                            skip_bytes(reader, left(reader));
                        }
                        Ok(())
                    },
                    false,
                    false,
                )?;
            }
            Ok(())
        },
        true,
        false,
    )?;
    psd.image_resources = Some(image_resources);

    // layer and mask info
    read_section(
        reader,
        1,
        |reader, left| {
            read_section(
                reader,
                2,
                |reader, left| {
                    read_layer_info(reader, &mut psd)?;
                    skip_bytes(reader, left(reader));
                    Ok(())
                },
                true,
                reader.large,
            )?;

            // SAI does not include this section
            if left(reader) > 0 {
                if let Some(info) = read_global_layer_mask_info(reader)? {
                    psd.global_layer_mask_info = Some(info);
                }
            } else {
                skip_bytes(reader, left(reader));
            }

            while left(reader) > 0 {
                // sometimes there are empty bytes here
                while left(reader) > 0 && peek_uint8(reader)? == 0 {
                    skip_bytes(reader, 1);
                }

                if left(reader) >= 12 {
                    // additional layer info applied to the whole document.
                    let mut info = std::mem::take(&mut psd.additional_info);
                    read_additional_layer_info(reader, &mut info)?;
                    psd.additional_info = info;
                } else {
                    skip_bytes(reader, left(reader));
                    break;
                }
            }
            Ok(())
        },
        true,
        reader.large,
    )?;

    let has_children = psd.children.as_ref().map_or(false, |c| !c.is_empty());
    let skip_layer = reader.options.skip_layer_image_data == Some(true);
    let skip_composite =
        reader.options.skip_composite_image_data == Some(true) && (skip_layer || has_children);

    if !skip_composite {
        read_image_data(reader, &mut psd)?;
    }

    Ok(psd)
}

fn is_valid_image_resource_signature(sig: &str) -> bool {
    sig == "8BIM" || sig == "MeSa" || sig == "AgHg" || sig == "PHUT" || sig == "DCSR"
}

// ---------------------------------------------------------------------------
// readLayerInfo
// ---------------------------------------------------------------------------

fn read_layer_info(reader: &mut PsdReader, psd: &mut crate::psd::Psd) -> ReadResult<()> {
    let mut layer_count = read_int16(reader)? as i32;

    if layer_count < 0 {
        reader.global_alpha = true;
        layer_count = -layer_count;
    }
    let layer_count = layer_count as usize;

    let mut layers: Vec<Layer> = Vec::with_capacity(layer_count);
    let mut layer_channels: Vec<Vec<ChannelInfo>> = Vec::with_capacity(layer_count);

    for _ in 0..layer_count {
        let (layer, channels) = read_layer_record(reader, psd)?;
        layers.push(layer);
        layer_channels.push(channels);
    }

    for i in 0..layer_count {
        read_layer_channel_image_data(reader, psd, &mut layers[i], &layer_channels[i])?;
    }

    if psd.children.is_none() {
        psd.children = Some(Vec::new());
    }

    // Build the tree. We mirror upstream's stack-based unshift algorithm, but
    // since Rust ownership makes a stack of mutable references hard, we collect
    // into a nesting structure by tracking a path of indices.
    build_layer_tree(psd, layers);

    Ok(())
}

/// Mirror of the upstream stack/unshift folder-nesting algorithm.
fn build_layer_tree(psd: &mut crate::psd::Psd, mut layers: Vec<Layer>) {
    // Pre-process: apply opened/children/blendMode for folders.
    // We process from the end (as upstream loops i = len-1 .. 0) building nested
    // vectors. `stack` holds the children-list under construction; each entry is
    // a Vec<Layer>. When we open a folder we push a new list; when we hit a
    // bounding divider we pop and attach to the parent's last-unshifted folder.
    //
    // Because upstream unshifts (prepends) and we iterate end->start, the final
    // order is preserved by pushing to front of each list.

    // Stack of (children list, optional folder layer awaiting its children).
    struct Frame {
        children: Vec<Layer>,
        folder: Option<Layer>,
    }

    let mut stack: Vec<Frame> = vec![Frame { children: Vec::new(), folder: None }];

    for i in (0..layers.len()).rev() {
        let l = std::mem::take(&mut layers[i]);
        let ty = l
            .additional_info
            .section_divider
            .as_ref()
            .map(|d| d.divider_type)
            .unwrap_or(SectionDividerType::Other);

        match ty {
            SectionDividerType::OpenFolder | SectionDividerType::ClosedFolder => {
                let mut folder = l;
                folder.opened = Some(ty == SectionDividerType::OpenFolder);
                folder.children = Some(Vec::new());
                if let Some(div) = &folder.additional_info.section_divider {
                    if let Some(key) = &div.key {
                        if let Some(bm) = to_blend_mode(key) {
                            folder.blend_mode = Some(bm);
                        }
                    }
                }
                // push the folder frame; its children come from subsequent
                // (deeper-in-file, earlier-in-loop) layers between this and the
                // bounding divider.
                stack.push(Frame { children: Vec::new(), folder: Some(folder) });
            }
            SectionDividerType::BoundingSectionDivider => {
                // close current frame: attach collected children to folder, then
                // unshift folder into parent.
                let frame = stack.pop().unwrap_or(Frame { children: Vec::new(), folder: None });
                if let Some(mut folder) = frame.folder {
                    folder.children = Some(frame.children);
                    if let Some(parent) = stack.last_mut() {
                        parent.children.insert(0, folder);
                    }
                } else {
                    // bounding divider without matching folder; ignore body.
                    if let Some(parent) = stack.last_mut() {
                        for layer in frame.children.into_iter().rev() {
                            parent.children.insert(0, layer);
                        }
                    }
                }
            }
            _ => {
                if let Some(top) = stack.last_mut() {
                    top.children.insert(0, l);
                }
            }
        }
    }

    // Drain any unterminated folders (defensive — well-formed files end clean).
    while stack.len() > 1 {
        let frame = stack.pop().unwrap();
        if let Some(mut folder) = frame.folder {
            folder.children = Some(frame.children);
            if let Some(parent) = stack.last_mut() {
                parent.children.insert(0, folder);
            }
        } else if let Some(parent) = stack.last_mut() {
            for layer in frame.children.into_iter().rev() {
                parent.children.insert(0, layer);
            }
        }
    }

    let root = stack.pop().unwrap();
    let children = psd.children.get_or_insert_with(Vec::new);
    *children = root.children;
}

// ---------------------------------------------------------------------------
// readLayerRecord
// ---------------------------------------------------------------------------

fn read_layer_record(
    reader: &mut PsdReader,
    _psd: &mut crate::psd::Psd,
) -> ReadResult<(Layer, Vec<ChannelInfo>)> {
    let mut layer = Layer::default();
    layer.top = Some(read_int32(reader)? as f64);
    layer.left = Some(read_int32(reader)? as f64);
    layer.bottom = Some(read_int32(reader)? as f64);
    layer.right = Some(read_int32(reader)? as f64);

    let channel_count = read_uint16(reader)?;
    let mut channels: Vec<ChannelInfo> = Vec::with_capacity(channel_count as usize);

    for _ in 0..channel_count {
        let id = read_int16(reader)?;
        let mut length = read_uint32(reader)? as usize;
        if reader.large {
            if length != 0 {
                return Err(ReadError::StrictViolation(
                    "Sizes larger than 4GB are not supported".to_string(),
                ));
            }
            length = read_uint32(reader)? as usize;
        }
        channels.push(ChannelInfo { id, length });
    }

    check_signature(reader, "8BIM", None)?;
    let blend_mode = read_signature(reader)?;
    match to_blend_mode(&blend_mode) {
        Some(bm) => layer.blend_mode = Some(bm),
        None => {
            return Err(ReadError::StrictViolation(format!(
                "Invalid blend mode: '{}'",
                blend_mode
            )))
        }
    }

    layer.opacity = Some(read_uint8(reader)? as f64 / 0xff as f64);
    layer.clipping = Some(read_uint8(reader)? == 1);

    let flags = read_uint8(reader)?;
    layer.transparency_protected = Some((flags & 0x01) != 0);
    layer.hidden = Some((flags & 0x02) != 0);
    if flags & 0x20 != 0 {
        layer.effects_open = Some(true);
    }

    skip_bytes(reader, 1);

    // extra data section
    let large = reader.large;
    let mut info = std::mem::take(&mut layer.additional_info);
    read_section(
        reader,
        1,
        |reader, left| {
            read_layer_mask_data(reader, &mut info)?;

            if let Some(ranges) = read_layer_blending_ranges(reader)? {
                info.blending_ranges = Some(ranges);
            }
            info.name = Some(read_pascal_string(reader, 1)?);

            // HACK: skip junk until a valid signature
            while left(reader) > 4 && !valid_signature_at(reader, reader.offset) {
                reader.offset += 1;
            }

            while left(reader) >= 12 {
                read_additional_layer_info(reader, &mut info)?;
            }

            skip_bytes(reader, left(reader));
            Ok(())
        },
        true,
        false,
    )?;
    let _ = large;
    layer.additional_info = info;

    Ok((layer, channels))
}

fn read_layer_mask_data(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
) -> ReadResult<()> {
    read_section(
        reader,
        1,
        |reader, left| {
            if left(reader) == 0 {
                return Ok(());
            }
            let mut mask = LayerMaskData::default();
            mask.top = Some(read_int32(reader)? as f64);
            mask.left = Some(read_int32(reader)? as f64);
            mask.bottom = Some(read_int32(reader)? as f64);
            mask.right = Some(read_int32(reader)? as f64);
            mask.default_color = Some(read_uint8(reader)? as f64);

            let flags = read_uint8(reader)?;
            mask.position_relative_to_layer =
                Some((flags & LayerMaskFlags::PositionRelativeToLayer as u8) != 0);
            mask.disabled = Some((flags & LayerMaskFlags::LayerMaskDisabled as u8) != 0);
            mask.from_vector_data =
                Some((flags & LayerMaskFlags::LayerMaskFromRenderingOtherData as u8) != 0);

            if left(reader) >= 18 {
                let mut real_mask = LayerMaskData::default();
                let real_flags = read_uint8(reader)?;
                real_mask.position_relative_to_layer =
                    Some((real_flags & LayerMaskFlags::PositionRelativeToLayer as u8) != 0);
                real_mask.disabled =
                    Some((real_flags & LayerMaskFlags::LayerMaskDisabled as u8) != 0);
                real_mask.from_vector_data = Some(
                    (real_flags & LayerMaskFlags::LayerMaskFromRenderingOtherData as u8) != 0,
                );
                real_mask.default_color = Some(read_uint8(reader)? as f64);
                real_mask.top = Some(read_int32(reader)? as f64);
                real_mask.left = Some(read_int32(reader)? as f64);
                real_mask.bottom = Some(read_int32(reader)? as f64);
                real_mask.right = Some(read_int32(reader)? as f64);
                info.real_mask = Some(real_mask);
            }

            if flags & LayerMaskFlags::MaskHasParametersAppliedToIt as u8 != 0 {
                let params = read_uint8(reader)?;
                if params & MaskParams::UserMaskDensity as u8 != 0 {
                    mask.user_mask_density = Some(read_uint8(reader)? as f64 / 0xff as f64);
                }
                if params & MaskParams::UserMaskFeather as u8 != 0 {
                    mask.user_mask_feather = Some(read_float64(reader)?);
                }
                if params & MaskParams::VectorMaskDensity as u8 != 0 {
                    mask.vector_mask_density = Some(read_uint8(reader)? as f64 / 0xff as f64);
                }
                if params & MaskParams::VectorMaskFeather as u8 != 0 {
                    mask.vector_mask_feather = Some(read_float64(reader)?);
                }
            }

            info.mask = Some(mask);
            skip_bytes(reader, left(reader));
            Ok(())
        },
        true,
        false,
    )?;
    Ok(())
}

fn read_blending_range(reader: &mut PsdReader) -> ReadResult<Vec<f64>> {
    Ok(vec![
        read_uint8(reader)? as f64,
        read_uint8(reader)? as f64,
        read_uint8(reader)? as f64,
        read_uint8(reader)? as f64,
    ])
}

fn read_layer_blending_ranges(
    reader: &mut PsdReader,
) -> ReadResult<Option<crate::psd::BlendingRanges>> {
    let res = read_section(
        reader,
        1,
        |reader, left| {
            let composite_gray_blend_source = read_blending_range(reader)?;
            let composite_graph_blend_destination_range = read_blending_range(reader)?;
            let mut ranges: Vec<crate::psd::BlendingRange> = Vec::new();
            while left(reader) > 0 {
                let source_range = read_blending_range(reader)?;
                let dest_range = read_blending_range(reader)?;
                ranges.push(crate::psd::BlendingRange { source_range, dest_range });
            }
            Ok(crate::psd::BlendingRanges {
                composite_gray_blend_source,
                composite_graph_blend_destination_range,
                ranges,
            })
        },
        true,
        false,
    )?;
    Ok(res)
}

// ---------------------------------------------------------------------------
// readLayerChannelImageData
// ---------------------------------------------------------------------------

fn read_layer_channel_image_data(
    reader: &mut PsdReader,
    psd: &crate::psd::Psd,
    layer: &mut Layer,
    channels: &[ChannelInfo],
) -> ReadResult<()> {
    if reader.options.skip_layer_image_data == Some(true) {
        return Ok(());
    }

    let color_mode = psd.color_mode.unwrap_or(ColorMode::Rgb);
    let bits_per_channel = psd.bits_per_channel.unwrap_or(8.0);
    let large = reader.large;

    let mut raw_channels: Vec<LayerRawDataChannel> = Vec::with_capacity(channels.len());

    for channel in channels {
        let start = reader.offset;
        let mut compression = Compression::RawData;
        let mut data: Option<Vec<u8>> = None;

        if channel.length == 1 {
            return Err(ReadError::StrictViolation("Invalid channel length".to_string()));
        }
        if channel.length != 0 {
            let mut comp = read_uint16(reader)?;
            if comp > 3 {
                reader.offset -= 1;
                comp = read_uint16(reader)?;
            }
            if comp > 3 {
                reader.offset -= 3;
                comp = read_uint16(reader)?;
            }
            if comp > 3 {
                return Err(ReadError::StrictViolation(format!(
                    "Invalid compression: {}",
                    comp
                )));
            }
            compression = compression_from_u16(comp);
            if channel.length > 2 {
                data = Some(read_bytes(reader, channel.length - 2)?);
            }
        }

        reader.offset = start + channel.length;
        raw_channels.push(LayerRawDataChannel {
            id: channel_id_from_i16(channel.id),
            compression,
            data,
        });
    }

    layer.raw_data = Some(LayerRawData {
        color_mode,
        bits_per_channel,
        channels: raw_channels,
        large,
    });

    if reader.options.use_raw_data != Some(true) {
        let use_image_data = reader.options.use_image_data == Some(true);
        let throw_missing = reader.options.throw_for_missing_features == Some(true);
        decode_layer_image_data(layer, use_image_data, throw_missing)?;
    }

    Ok(())
}

fn compression_from_u16(v: u16) -> Compression {
    match v {
        0 => Compression::RawData,
        1 => Compression::RleCompressed,
        2 => Compression::ZipWithoutPrediction,
        _ => Compression::ZipWithPrediction,
    }
}

fn setup_grayscale(data: &mut [u8], width: usize, height: usize) {
    let size = width * height * 4;
    let mut i = 0;
    while i < size {
        let c = data[i];
        data[i + 1] = c;
        data[i + 2] = c;
        i += 4;
    }
}

fn reset_alpha(target: &mut DecodeTarget, cmyk: bool) {
    let alpha = 0xffu8;
    let offset = if cmyk { 4 } else { 3 };
    let step = if cmyk { 5 } else { 4 };
    let length = target.data.len();
    let mut p = offset;
    while p < length {
        target.data[p] = alpha;
        p += step;
    }
}

/// Mirror `decodeLayerImageData`.
fn decode_layer_image_data(
    layer: &mut Layer,
    use_image_data: bool,
    throw_for_missing_features: bool,
) -> ReadResult<()> {
    let raw = match layer.raw_data.take() {
        Some(r) => r,
        None => return Ok(()),
    };

    let color_mode = raw.color_mode;
    let bits_per_channel = raw.bits_per_channel as u32;
    let large = raw.large;
    let layer_width =
        (layer.right.unwrap_or(0.0) - layer.left.unwrap_or(0.0)).max(0.0) as usize;
    let layer_height =
        (layer.bottom.unwrap_or(0.0) - layer.top.unwrap_or(0.0)).max(0.0) as usize;
    let cmyk = color_mode == ColorMode::Cmyk;

    let mut image_data: Option<DecodeTarget> = None;
    let mut initialized_alpha = false;

    if layer_width != 0 && layer_height != 0 {
        if cmyk {
            if bits_per_channel != 8 {
                return Err(ReadError::StrictViolation("bitsPerChannel Not supproted".to_string()));
            }
            image_data = Some(DecodeTarget::wide(layer_width, layer_height, 5));
        } else {
            image_data = Some(DecodeTarget::rgba(layer_width, layer_height));
        }
    }

    for ch in &raw.channels {
        let data = match &ch.data {
            Some(d) => d,
            None => continue,
        };
        let mut data_reader = PsdReader::new(data, None, None);

        if ch.id == ChannelId::UserMask || ch.id == ChannelId::RealUserMask {
            let mask_ref = if ch.id == ChannelId::UserMask {
                layer.additional_info.mask.as_ref()
            } else {
                layer.additional_info.real_mask.as_ref()
            };
            let (mtop, mleft, mbottom, mright) = match mask_ref {
                Some(m) => (
                    m.top.unwrap_or(0.0),
                    m.left.unwrap_or(0.0),
                    m.bottom.unwrap_or(0.0),
                    m.right.unwrap_or(0.0),
                ),
                None => {
                    return Err(ReadError::StrictViolation(format!(
                        "Missing layer {} data",
                        if ch.id == ChannelId::UserMask { "mask" } else { "real mask" }
                    )))
                }
            };
            let mask_width = (mright - mleft) as i64;
            let mask_height = (mbottom - mtop) as i64;
            if !(0..=30000).contains(&mask_width) || !(0..=30000).contains(&mask_height) {
                return Err(ReadError::StrictViolation("Invalid mask size".to_string()));
            }
            let mw = mask_width as usize;
            let mh = mask_height as usize;
            if mw != 0 && mh != 0 {
                let mut mask_data = DecodeTarget::rgba(mw, mh);
                read_data(
                    &mut data_reader,
                    data.len(),
                    Some(&mut mask_data),
                    ch.compression,
                    mw,
                    mh,
                    bits_per_channel,
                    0,
                    large,
                    4,
                )?;
                setup_grayscale(&mut mask_data.data, mw, mh);
                reset_alpha(&mut mask_data, false);
                let pd = mask_data.into_pixel_data();
                let mask = if ch.id == ChannelId::UserMask {
                    layer.additional_info.mask.as_mut()
                } else {
                    layer.additional_info.real_mask.as_mut()
                };
                if let Some(mask) = mask {
                    if use_image_data {
                        mask.image_data = Some(pd);
                    } else {
                        mask.canvas = Some(image_data_to_canvas(&pd));
                    }
                }
            }
        } else {
            let offset = offset_for_channel(ch.id, cmyk);
            let target = if offset < 0 {
                if throw_for_missing_features {
                    return Err(ReadError::StrictViolation(format!(
                        "Channel not supported: {}",
                        ch.id as i32
                    )));
                }
                None
            } else {
                image_data.as_mut()
            };

            let step = if cmyk { 5 } else { 4 };
            read_data(
                &mut data_reader,
                data.len(),
                target,
                ch.compression,
                layer_width,
                layer_height,
                bits_per_channel,
                offset.max(0) as usize,
                large,
                step,
            )?;

            if offset >= 0 && color_mode == ColorMode::Grayscale {
                if let Some(t) = image_data.as_mut() {
                    setup_grayscale(&mut t.data, t.width, t.height);
                }
            }
        }

        if ch.id == ChannelId::Transparency {
            initialized_alpha = true;
        }
    }

    if let Some(mut img) = image_data {
        if !initialized_alpha {
            reset_alpha(&mut img, cmyk);
        }

        let final_pd = if cmyk {
            let mut rgb = create_image_data(img.width as u32, img.height as u32);
            cmyk_to_rgb(&img, &mut rgb, false);
            rgb
        } else {
            img.into_pixel_data()
        };

        if use_image_data {
            layer.image_data = Some(final_pd);
        } else {
            layer.canvas = Some(image_data_to_canvas(&final_pd));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Channel image-data codecs
// ---------------------------------------------------------------------------

/// Mirror `readData` dispatch.
fn read_data(
    reader: &mut PsdReader,
    length: usize,
    pixels: Option<&mut DecodeTarget>,
    compression: Compression,
    width: usize,
    height: usize,
    bit_depth: u32,
    offset: usize,
    large: bool,
    step: usize,
) -> ReadResult<()> {
    if length == 0 {
        return Ok(());
    }
    match compression {
        Compression::RawData => {
            let data = read_bytes(reader, length)?;
            read_data_raw(&data, pixels, bit_depth, step, offset);
            Ok(())
        }
        Compression::RleCompressed => {
            read_data_rle(reader, pixels, width, height, bit_depth, step, &[offset], large)
        }
        Compression::ZipWithoutPrediction => {
            let data = read_bytes(reader, length)?;
            read_data_zip(&data, pixels, width, height, bit_depth, step, offset, false);
            Ok(())
        }
        Compression::ZipWithPrediction => {
            let data = read_bytes(reader, length)?;
            read_data_zip(&data, pixels, width, height, bit_depth, step, offset, true);
            Ok(())
        }
    }
}

fn copy_channel_to_pixel_data(target: &mut DecodeTarget, channel: &[u8], offset: usize, step: usize) {
    let size = target.width * target.height;
    let mut p = offset;
    for i in 0..size {
        if i >= channel.len() || p >= target.data.len() {
            break;
        }
        target.data[p] = channel[i];
        p += step;
    }
}

/// Mirror `readDataRaw`. Down-converts 16/32-bit samples to 8-bit (top byte).
pub fn read_data_raw(
    buffer: &[u8],
    pixel_data: Option<&mut DecodeTarget>,
    bit_depth: u32,
    step: usize,
    offset: usize,
) {
    let pixel_data = match pixel_data {
        Some(p) => p,
        None => return,
    };
    if offset >= step {
        return;
    }
    let bytes = bytes_to_u8_channel(buffer, bit_depth);
    copy_channel_to_pixel_data(pixel_data, &bytes, offset, step);
}

/// Convert a big-endian channel byte buffer to an 8-bit sample-per-element Vec.
/// For 16/32-bit, takes the most significant byte (matching down-conversion to
/// RGBA8 used elsewhere in this crate).
fn bytes_to_u8_channel(buffer: &[u8], bit_depth: u32) -> Vec<u8> {
    match bit_depth {
        8 => buffer.to_vec(),
        16 => {
            // big-endian: MSB first
            let mut out = Vec::with_capacity(buffer.len() / 2);
            let mut i = 0;
            while i + 1 < buffer.len() {
                out.push(buffer[i]);
                i += 2;
            }
            out
        }
        32 => {
            // 32-bit float channel; clamp [0,1] -> [0,255].
            let mut out = Vec::with_capacity(buffer.len() / 4);
            let mut i = 0;
            while i + 3 < buffer.len() {
                let v = f32::from_be_bytes([
                    buffer[i],
                    buffer[i + 1],
                    buffer[i + 2],
                    buffer[i + 3],
                ]);
                let c = (v.max(0.0).min(1.0) * 255.0).round() as u8;
                out.push(c);
                i += 4;
            }
            out
        }
        _ => buffer.to_vec(),
    }
}

fn decode_predicted_u8(data: &mut [u8], width: usize, height: usize) {
    for y in 0..height {
        let offset = y * width;
        for x in 1..width {
            let o = offset + x;
            data[o] = data[o - 1].wrapping_add(data[o]);
        }
    }
}

fn decode_predicted_u16(data: &mut [u16], width: usize, height: usize) {
    for y in 0..height {
        let offset = y * width;
        for x in 1..width {
            let o = offset + x;
            data[o] = data[o - 1].wrapping_add(data[o]);
        }
    }
}

/// Mirror `readDataZip` (zlib via flate2).
pub fn read_data_zip(
    compressed: &[u8],
    pixel_data: Option<&mut DecodeTarget>,
    width: usize,
    height: usize,
    bit_depth: u32,
    step: usize,
    offset: usize,
    prediction: bool,
) {
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    let mut decoder = ZlibDecoder::new(compressed);
    let mut decompressed: Vec<u8> = Vec::new();
    if decoder.read_to_end(&mut decompressed).is_err() {
        return;
    }

    let pixel_data = match pixel_data {
        Some(p) => p,
        None => return,
    };
    if offset >= step {
        return;
    }

    match bit_depth {
        8 => {
            if prediction {
                decode_predicted_u8(&mut decompressed, width, height);
            }
            copy_channel_to_pixel_data(pixel_data, &decompressed, offset, step);
        }
        16 => {
            // big-endian u16 samples
            let mut samples: Vec<u16> = Vec::with_capacity(decompressed.len() / 2);
            let mut i = 0;
            while i + 1 < decompressed.len() {
                samples.push(u16::from_be_bytes([decompressed[i], decompressed[i + 1]]));
                i += 2;
            }
            if prediction {
                decode_predicted_u16(&mut samples, width, height);
            }
            // down-convert to MSB byte
            let bytes: Vec<u8> = samples.iter().map(|&s| (s >> 8) as u8).collect();
            copy_channel_to_pixel_data(pixel_data, &bytes, offset, step);
        }
        32 => {
            // 32-bit float, optionally byte-predicted across width*4 bytes.
            if prediction {
                decode_predicted_u8(&mut decompressed, width * 4, height);
            }
            // Photoshop stores planar bytes: reconstruct big-endian floats.
            let mut p = offset;
            for y in 0..height {
                let a0 = width * 4 * y;
                for x in 0..width {
                    let a = a0 + x;
                    let b = a + width;
                    let c = b + width;
                    let d = c + width;
                    if d >= decompressed.len() || p >= pixel_data.data.len() {
                        break;
                    }
                    let v = f32::from_be_bytes([
                        decompressed[a],
                        decompressed[b],
                        decompressed[c],
                        decompressed[d],
                    ]);
                    pixel_data.data[p] = (v.max(0.0).min(1.0) * 255.0).round() as u8;
                    p += step;
                }
            }
        }
        _ => {}
    }
}

/// Mirror `readDataRLE` (PackBits). Writes one byte per sample to the 8-bit
/// RGBA target. For >8 bit depths the source is still byte-stream PackBits, so
/// we keep upstream's byte semantics (the upstream RLE path also writes bytes).
pub fn read_data_rle(
    reader: &mut PsdReader,
    mut pixel_data: Option<&mut DecodeTarget>,
    width: usize,
    height: usize,
    _bit_depth: u32,
    step: usize,
    offsets: &[usize],
    large: bool,
) -> ReadResult<()> {
    let mut lengths: Vec<usize> = Vec::with_capacity(offsets.len() * height);
    if large {
        for _ in 0..offsets.len() {
            for _ in 0..height {
                lengths.push(read_uint32(reader)? as usize);
            }
        }
    } else {
        for _ in 0..offsets.len() {
            for _ in 0..height {
                lengths.push(read_uint16(reader)? as usize);
            }
        }
    }

    let extra_limit = step.saturating_sub(1);

    let mut li = 0usize;
    for c in 0..offsets.len() {
        let offset = offsets[c];
        let extra = c > extra_limit || offset > extra_limit;

        let have_data = pixel_data.is_some() && !extra;
        if !have_data {
            for _ in 0..height {
                let len = lengths[li];
                li += 1;
                skip_bytes(reader, len);
            }
            continue;
        }

        let mut p = offset;
        for _ in 0..height {
            let length = lengths[li];
            li += 1;
            let buffer = read_bytes(reader, length)?;

            let mut i = 0usize;
            let mut x = 0usize;
            while i < length {
                let header = buffer[i];
                if header > 128 {
                    i += 1;
                    if i >= buffer.len() {
                        break;
                    }
                    let value = buffer[i];
                    let count = (256 - header as usize) as usize;
                    let mut j = 0;
                    while j <= count && x < width {
                        let pd = pixel_data.as_deref_mut_unchecked();
                        if p < pd.data.len() {
                            pd.data[p] = value;
                        }
                        p += step;
                        j += 1;
                        x += 1;
                    }
                } else if header < 128 {
                    let count = header as usize;
                    let mut j = 0;
                    while j <= count && x < width {
                        i += 1;
                        if i >= buffer.len() {
                            break;
                        }
                        let value = buffer[i];
                        let pd = pixel_data.as_deref_mut_unchecked();
                        if p < pd.data.len() {
                            pd.data[p] = value;
                        }
                        p += step;
                        j += 1;
                        x += 1;
                    }
                }
                i += 1;
            }
        }
        // assignment of p back happens implicitly via loop continuation; in
        // upstream p resets per channel via offset, which we did at loop top.
        let _ = p;
    }

    Ok(())
}

// Helper trait to reborrow Option<&mut T> inside the RLE inner loops without
// fighting the borrow checker over the per-iteration mutable access.
trait OptMutHelper {
    fn as_deref_mut_unchecked(&mut self) -> &mut DecodeTarget;
}
impl OptMutHelper for Option<&mut DecodeTarget> {
    #[inline]
    fn as_deref_mut_unchecked(&mut self) -> &mut DecodeTarget {
        self.as_deref_mut().expect("pixel_data present in RLE write path")
    }
}

// ---------------------------------------------------------------------------
// readGlobalLayerMaskInfo
// ---------------------------------------------------------------------------

fn read_global_layer_mask_info(
    reader: &mut PsdReader,
) -> ReadResult<Option<GlobalLayerMaskInfo>> {
    let res = read_section(
        reader,
        1,
        |reader, left| {
            if left(reader) == 0 {
                return Ok(None);
            }
            let overlay_color_space = read_uint16(reader)? as f64;
            let color_space1 = read_uint16(reader)? as f64;
            let color_space2 = read_uint16(reader)? as f64;
            let color_space3 = read_uint16(reader)? as f64;
            let color_space4 = read_uint16(reader)? as f64;
            let opacity = read_uint16(reader)? as f64 / 0xff as f64;
            let kind = read_uint8(reader)? as f64;
            skip_bytes(reader, left(reader));
            Ok(Some(GlobalLayerMaskInfo {
                overlay_color_space,
                color_space1,
                color_space2,
                color_space3,
                color_space4,
                opacity,
                kind,
            }))
        },
        true,
        false,
    )?;
    Ok(res.flatten())
}

// ---------------------------------------------------------------------------
// realignWithSignature & readAdditionalLayerInfo
// ---------------------------------------------------------------------------

const FIX_OFFSETS: [i32; 9] = [0, 1, -1, 2, -2, 3, -3, 4, -4];

/// Mirror `realignWithSignature`.
fn realign_with_signature(
    reader: &mut PsdReader,
    is_valid: fn(&str) -> bool,
) -> ReadResult<String> {
    let sig_offset = reader.offset as i64;
    let mut sig = String::new();

    for &off in FIX_OFFSETS.iter() {
        let new_off = sig_offset + off as i64;
        if new_off < 0 || (new_off as usize) + 4 > reader.buffer.len() {
            continue;
        }
        reader.offset = new_off as usize;
        if let Ok(s) = read_signature(reader) {
            sig = s;
        }
        if is_valid(&sig) {
            break;
        }
    }

    if !is_valid(&sig) {
        return Err(ReadError::InvalidSignature {
            signature: sig,
            offset: sig_offset as usize,
        });
    }
    Ok(sig)
}

fn is_valid_additional_info_signature(sig: &str) -> bool {
    sig == "8BIM" || sig == "8B64"
}

/// Mirror `readAdditionalLayerInfo`.
fn read_additional_layer_info(
    reader: &mut PsdReader,
    target: &mut LayerAdditionalInfo,
) -> ReadResult<()> {
    let sig = realign_with_signature(reader, is_valid_additional_info_signature)?;
    let key = read_signature(reader)?;

    let large = reader.large;
    let u64_size = sig == "8B64"
        || (large && crate::additional_info::is_large_key(&key));

    let options = reader.options.clone();
    let throw_for_missing = options.throw_for_missing_features == Some(true);

    read_section(
        reader,
        2,
        |reader, left| {
            let mut ctx = ReadCtx { options: &options, large };
            match read_additional_info_key(&key, reader, target, &left_fn_wrap(left), &mut ctx) {
                Ok(handled) => {
                    if !handled {
                        skip_bytes(reader, left(reader));
                    }
                }
                Err(e) => {
                    if throw_for_missing {
                        return Err(e);
                    }
                    // swallow and skip remaining
                }
            }
            if left(reader) > 0 {
                skip_bytes(reader, left(reader));
            }
            Ok(())
        },
        false,
        u64_size,
    )?;
    Ok(())
}

/// `read_additional_info_key` expects `&dyn Fn(&PsdReader)->usize`; the section
/// closure already provides one (`left`). This wrapper just re-types it.
fn left_fn_wrap<'a>(left: &'a dyn Fn(&PsdReader) -> usize) -> impl Fn(&PsdReader) -> usize + 'a {
    move |r: &PsdReader| left(r)
}

// ---------------------------------------------------------------------------
// readImageData (composite)
// ---------------------------------------------------------------------------

fn read_image_data(reader: &mut PsdReader, psd: &mut crate::psd::Psd) -> ReadResult<()> {
    let compression = compression_from_u16(read_uint16(reader)?);
    let bits_per_channel = psd.bits_per_channel.unwrap_or(8.0) as u32;
    let color_mode = psd.color_mode.unwrap_or(ColorMode::Rgb);

    let width = psd.width as usize;
    let height = psd.height as usize;
    let channels_count = psd.channels.unwrap_or(0.0) as usize;

    if compression != Compression::RawData && compression != Compression::RleCompressed {
        return Err(ReadError::StrictViolation(format!(
            "Compression type not supported: {:?}",
            compression
        )));
    }

    let mut image_data = DecodeTarget::rgba(width, height);
    {
        // resetImageData: black, opaque.
        let buf = &mut image_data.data;
        let mut p = 0;
        while p < buf.len() {
            buf[p] = 0;
            buf[p + 1] = 0;
            buf[p + 2] = 0;
            buf[p + 3] = 0xff;
            p += 4;
        }
    }

    match color_mode {
        ColorMode::Bitmap => {
            if bits_per_channel != 1 {
                return Err(ReadError::StrictViolation(
                    "Invalid bitsPerChannel for bitmap color mode".to_string(),
                ));
            }
            let bytes: Vec<u8> = match compression {
                Compression::RawData => {
                    read_bytes(reader, ((width + 7) / 8) * height)?
                }
                Compression::RleCompressed => {
                    let mut tgt = DecodeTarget {
                        width,
                        height,
                        data: vec![0u8; width * height],
                        channels: 1,
                    };
                    read_data_rle(
                        reader,
                        Some(&mut tgt),
                        width,
                        height,
                        8,
                        1,
                        &[0],
                        reader.large,
                    )?;
                    tgt.data
                }
                _ => {
                    return Err(ReadError::StrictViolation(
                        "Bitmap compression not supported".to_string(),
                    ))
                }
            };
            decode_bitmap(&bytes, &mut image_data.data, width, height);
        }
        ColorMode::Rgb | ColorMode::Grayscale => {
            let mut channels: Vec<usize> =
                if color_mode == ColorMode::Grayscale { vec![0] } else { vec![0, 1, 2] };

            if channels_count > 3 {
                for i in 3..channels_count {
                    channels.push(i);
                }
            } else if reader.global_alpha {
                channels.push(3);
            }

            match compression {
                Compression::RawData => {
                    for &c in &channels {
                        let data =
                            read_bytes(reader, width * height * (bits_per_channel as usize / 8))?;
                        read_data_raw(&data, Some(&mut image_data), bits_per_channel, 4, c);
                    }
                }
                Compression::RleCompressed => {
                    read_data_rle(
                        reader,
                        Some(&mut image_data),
                        width,
                        height,
                        bits_per_channel,
                        4,
                        &channels,
                        reader.large,
                    )?;
                }
                _ => {}
            }

            if color_mode == ColorMode::Grayscale {
                setup_grayscale(&mut image_data.data, width, height);
            }
        }
        ColorMode::Indexed => {
            if bits_per_channel != 8 {
                return Err(ReadError::StrictViolation("bitsPerChannel Not supproted".to_string()));
            }
            if channels_count != 1 {
                return Err(ReadError::StrictViolation("Invalid channel count".to_string()));
            }
            let palette = psd
                .palette
                .clone()
                .ok_or_else(|| ReadError::StrictViolation("Missing color palette".to_string()))?;

            match compression {
                Compression::RleCompressed => {
                    let mut indexed = DecodeTarget {
                        width,
                        height,
                        data: vec![0u8; width * height],
                        channels: 1,
                    };
                    read_data_rle(
                        reader,
                        Some(&mut indexed),
                        width,
                        height,
                        bits_per_channel,
                        1,
                        &[0],
                        reader.large,
                    )?;
                    indexed_to_rgb(&indexed, &mut image_data, &palette);
                }
                _ => return Err(ReadError::StrictViolation("Not implemented".to_string())),
            }
        }
        _ => {
            return Err(ReadError::StrictViolation(format!(
                "Color mode not supported: {:?}",
                color_mode
            )))
        }
    }

    // remove weird white matte
    if reader.global_alpha && bits_per_channel == 8 {
        let p = &mut image_data.data;
        let size = width * height * 4;
        let mut i = 0;
        while i < size {
            let pa = p[i + 3];
            if pa != 0 && pa != 255 {
                let a = pa as f64 / 255.0;
                let ra = 1.0 / a;
                let inv_a = 255.0 * (1.0 - ra);
                p[i] = (p[i] as f64 * ra + inv_a) as u8;
                p[i + 1] = (p[i + 1] as f64 * ra + inv_a) as u8;
                p[i + 2] = (p[i + 2] as f64 * ra + inv_a) as u8;
            }
            i += 4;
        }
    }

    let pd = image_data.into_pixel_data();
    if reader.options.use_image_data == Some(true) {
        psd.image_data = Some(pd);
    } else {
        psd.canvas = Some(image_data_to_canvas(&pd));
    }

    Ok(())
}

fn cmyk_to_rgb(cmyk: &DecodeTarget, rgb: &mut PixelData, reverse_alpha: bool) {
    let size = (rgb.width as usize) * (rgb.height as usize) * 4;
    let src = &cmyk.data;
    let dst = &mut rgb.data;
    let mut s = 0usize;
    let mut d = 0usize;
    while d < size && s + 4 < src.len() {
        let c = src[s] as u32;
        let m = src[s + 1] as u32;
        let y = src[s + 2] as u32;
        let k = src[s + 3] as u32;
        dst[d] = ((c * k) / 255) as u8;
        dst[d + 1] = ((m * k) / 255) as u8;
        dst[d + 2] = ((y * k) / 255) as u8;
        dst[d + 3] = if reverse_alpha { 255 - src[s + 4] } else { src[s + 4] };
        s += 5;
        d += 4;
    }
}

fn indexed_to_rgb(indexed: &DecodeTarget, rgb: &mut DecodeTarget, palette: &[Rgb]) {
    let size = indexed.width * indexed.height;
    let mut d = 0usize;
    for s in 0..size {
        let idx = indexed.data[s] as usize;
        if let Some(c) = palette.get(idx) {
            rgb.data[d] = c.r as u8;
            rgb.data[d + 1] = c.g as u8;
            rgb.data[d + 2] = c.b as u8;
            rgb.data[d + 3] = 255;
        }
        d += 4;
    }
}

// ---------------------------------------------------------------------------
// readColor (consolidated) & readPattern
// ---------------------------------------------------------------------------

/// Consolidated `readColor`. NOTE (see report): `effects_helpers`, `image_resources`,
/// `additional_info::adjustment_keys`, and `additional_info::misc_keys` each keep
/// a LOCAL copy of this function (they cannot reach reader-internal helpers).
/// Those should switch to this in a later cleanup task; not edited now.
pub fn read_color(reader: &mut PsdReader) -> ReadResult<Color> {
    let color_space = read_uint16(reader)?;
    if color_space == ColorSpace::Rgb as u16 {
        let r = read_uint16(reader)? as f64 / 257.0;
        let g = read_uint16(reader)? as f64 / 257.0;
        let b = read_uint16(reader)? as f64 / 257.0;
        skip_bytes(reader, 2);
        Ok(Color::Rgb(Rgb { r, g, b }))
    } else if color_space == ColorSpace::Hsb as u16 {
        let h = read_uint16(reader)? as f64 / 0xffff as f64;
        let s = read_uint16(reader)? as f64 / 0xffff as f64;
        let b = read_uint16(reader)? as f64 / 0xffff as f64;
        skip_bytes(reader, 2);
        Ok(Color::Hsb(Hsb { h, s, b }))
    } else if color_space == ColorSpace::Cmyk as u16 {
        let c = read_uint16(reader)? as f64 / 257.0;
        let m = read_uint16(reader)? as f64 / 257.0;
        let y = read_uint16(reader)? as f64 / 257.0;
        let k = read_uint16(reader)? as f64 / 257.0;
        Ok(Color::Cmyk(Cmyk { c, m, y, k }))
    } else if color_space == ColorSpace::Lab as u16 {
        let l = read_int16(reader)? as f64 / 10000.0;
        let ta = read_int16(reader)? as f64;
        let tb = read_int16(reader)? as f64;
        let a = if ta < 0.0 { ta / 12800.0 } else { ta / 12700.0 };
        let b = if tb < 0.0 { tb / 12800.0 } else { tb / 12700.0 };
        skip_bytes(reader, 2);
        Ok(Color::Lab(Lab { l, a, b }))
    } else if color_space == ColorSpace::Grayscale as u16 {
        let k = read_uint16(reader)? as f64 * 255.0 / 10000.0;
        skip_bytes(reader, 6);
        Ok(Color::Grayscale(Grayscale { k }))
    } else {
        Err(ReadError::StrictViolation("Invalid color space".to_string()))
    }
}

/// Consolidated `readPattern`. NOTE (see report): `abr.rs` and
/// `smart_object_keys.rs` hold local copies; they should switch to this later.
pub fn read_pattern(reader: &mut PsdReader) -> ReadResult<PatternInfo> {
    let mut length = read_uint32(reader)? as usize;
    while length % 4 != 0 {
        length += 1;
    }
    let end = reader.offset + length;
    let version = read_uint32(reader)?;
    if version != 1 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid pattern version: {}",
            version
        )));
    }

    let color_mode_raw = read_uint32(reader)?;
    let color_mode = color_mode_from_u16(color_mode_raw as u16);
    let x = read_int16(reader)? as f64;
    let y = read_int16(reader)? as f64;

    if !matches!(
        color_mode,
        Some(ColorMode::Rgb) | Some(ColorMode::Grayscale) | Some(ColorMode::Indexed)
    ) {
        return Err(ReadError::StrictViolation(format!(
            "Unsupported pattern color mode: {}",
            color_mode_raw
        )));
    }
    let color_mode = color_mode.unwrap();

    let name = read_unicode_string(reader)?;
    let id = read_pascal_string(reader, 1)?;

    let mut palette: Vec<Rgb> = Vec::new();
    if color_mode == ColorMode::Indexed {
        for _ in 0..256 {
            palette.push(Rgb {
                r: read_uint8(reader)? as f64,
                g: read_uint8(reader)? as f64,
                b: read_uint8(reader)? as f64,
            });
        }
        skip_bytes(reader, 4);
    }

    let version2 = read_uint32(reader)?;
    if version2 != 3 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid pattern VMAL version: {}",
            version2
        )));
    }

    read_uint32(reader)?; // length
    let top = read_uint32(reader)? as i64;
    let left = read_uint32(reader)? as i64;
    let bottom = read_uint32(reader)? as i64;
    let right = read_uint32(reader)? as i64;
    let channels_count = read_uint32(reader)? as usize;
    let width = (right - left) as usize;
    let height = (bottom - top) as usize;
    let mut data = vec![0u8; width * height * 4];
    let mut i = 3;
    while i < data.len() {
        data[i] = 255;
        i += 4;
    }

    let mut ch = 0usize;
    for _ in 0..(channels_count + 2) {
        let has = read_uint32(reader)?;
        if has == 0 {
            continue;
        }
        let length = read_uint32(reader)? as usize;
        let pixel_depth = read_uint32(reader)?;
        let ctop = read_uint32(reader)? as i64;
        let cleft = read_uint32(reader)? as i64;
        let cbottom = read_uint32(reader)? as i64;
        let cright = read_uint32(reader)? as i64;
        let pixel_depth2 = read_uint16(reader)?;
        let compression_mode = read_uint8(reader)?;
        let data_length = length.saturating_sub(4 + 16 + 2 + 1);
        let cdata = read_bytes(reader, data_length)?;

        if pixel_depth != 8 || pixel_depth2 != 8 {
            return Err(ReadError::StrictViolation(
                "16bit pixel depth not supported for patterns".to_string(),
            ));
        }

        let w = (cright - cleft) as usize;
        let h = (cbottom - ctop) as usize;
        let ox = (cleft - left) as usize;
        let oy = (ctop - top) as usize;

        if compression_mode == 0 {
            if color_mode == ColorMode::Rgb && ch < 3 {
                for yy in 0..h {
                    for xx in 0..w {
                        let src = xx + yy * w;
                        let dst = (ox + xx + (yy + oy) * width) * 4;
                        if dst + ch < data.len() && src < cdata.len() {
                            data[dst + ch] = cdata[src];
                        }
                    }
                }
            }
            if color_mode == ColorMode::Grayscale && ch < 1 {
                for yy in 0..h {
                    for xx in 0..w {
                        let src = xx + yy * w;
                        let dst = (ox + xx + (yy + oy) * width) * 4;
                        if dst + 2 < data.len() && src < cdata.len() {
                            let value = cdata[src];
                            data[dst] = value;
                            data[dst + 1] = value;
                            data[dst + 2] = value;
                        }
                    }
                }
            }
            if color_mode == ColorMode::Indexed {
                return Err(ReadError::StrictViolation(
                    "Indexed pattern color mode not implemented".to_string(),
                ));
            }
        } else if compression_mode == 1 {
            let mut temp = DecodeTarget { width: w, height: h, data: vec![0u8; w * h], channels: 1 };
            let mut cdata_reader = PsdReader::new(&cdata, None, None);
            if color_mode == ColorMode::Rgb && ch < 3 {
                read_data_rle(&mut cdata_reader, Some(&mut temp), w, h, 8, 1, &[0], false)?;
                copy_channel_to_rgba(&temp, &mut data, width, ox, oy, ch);
            }
            if color_mode == ColorMode::Grayscale && ch < 1 {
                read_data_rle(&mut cdata_reader, Some(&mut temp), w, h, 8, 1, &[0], false)?;
                copy_channel_to_rgba(&temp, &mut data, width, ox, oy, 0);
                // setup grayscale on the destination region is approximated by
                // copying channel 0 into 1 and 2 in copy step below.
                copy_channel_to_rgba(&temp, &mut data, width, ox, oy, 1);
                copy_channel_to_rgba(&temp, &mut data, width, ox, oy, 2);
            }
            if color_mode == ColorMode::Indexed {
                return Err(ReadError::StrictViolation(
                    "Indexed pattern color mode not implemented".to_string(),
                ));
            }
        } else {
            return Err(ReadError::StrictViolation(
                "Invalid pattern compression mode".to_string(),
            ));
        }

        ch += 1;
    }

    reader.offset = end;

    Ok(PatternInfo {
        id,
        name,
        x,
        y,
        bounds: PatternBounds {
            x: left as f64,
            y: top as f64,
            w: width as f64,
            h: height as f64,
        },
        data,
    })
}

fn copy_channel_to_rgba(
    src: &DecodeTarget,
    dst: &mut [u8],
    dst_width: usize,
    ox: usize,
    oy: usize,
    offset: usize,
) {
    let w = src.width;
    let h = src.height;
    for y in 0..h {
        for x in 0..w {
            let s = x + y * w;
            let d = (ox + x + (y + oy) * dst_width) * 4;
            if d + offset < dst.len() && s < src.data.len() {
                dst[d + offset] = src.data[s];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_round_trip_big_endian() {
        // Hand-crafted big-endian bytes.
        // u8=0x12, i8=-1(0xFF), i16=-2(0xFFFE), u16=0x0102, i32=-3, u32=0x01020304
        let buf: Vec<u8> = vec![
            0x12, // u8
            0xFF, // i8 = -1
            0xFF, 0xFE, // i16 = -2
            0x01, 0x02, // u16 = 0x0102
            0xFF, 0xFF, 0xFF, 0xFD, // i32 = -3
            0x01, 0x02, 0x03, 0x04, // u32 = 0x01020304
        ];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_uint8(&mut r).unwrap(), 0x12);
        assert_eq!(read_int8(&mut r).unwrap(), -1);
        assert_eq!(read_int16(&mut r).unwrap(), -2);
        assert_eq!(read_uint16(&mut r).unwrap(), 0x0102);
        assert_eq!(read_int32(&mut r).unwrap(), -3);
        assert_eq!(read_uint32(&mut r).unwrap(), 0x0102_0304);
        assert_eq!(r.offset, buf.len());
    }

    #[test]
    fn float_round_trip_big_endian() {
        let f32v: f32 = 3.5;
        let f64v: f64 = -1234.5678;
        let mut buf = Vec::new();
        buf.extend_from_slice(&f32v.to_be_bytes());
        buf.extend_from_slice(&f64v.to_be_bytes());
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_float32(&mut r).unwrap(), f32v);
        assert_eq!(read_float64(&mut r).unwrap(), f64v);
    }

    #[test]
    fn uint16_le_differs_from_be() {
        let buf = vec![0x01, 0x02];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_uint16_le(&mut r).unwrap(), 0x0201);
    }

    #[test]
    fn fixed_point() {
        // 16.16: value 1.5 -> int32 = 1.5 * 65536 = 98304 = 0x00018000
        let buf = vec![0x00, 0x01, 0x80, 0x00];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_fixed_point32(&mut r).unwrap(), 1.5);
    }

    #[test]
    fn signature_and_check() {
        let buf = b"8BIM".to_vec();
        let mut r = PsdReader::new(&buf, None, None);
        assert!(valid_signature_at(&r, 0));
        check_signature(&mut r, "8BIM", None).unwrap();

        let mut r2 = PsdReader::new(&buf, None, None);
        let err = check_signature(&mut r2, "8BPS", None).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidSignature {
                signature: "8BIM".to_string(),
                offset: 0
            }
        );
    }

    #[test]
    fn pascal_string_pad_to_2() {
        // length=3, "abc", padTo=2: bytes consumed = 1(len)+3(text)=4, already
        // multiple of 2 -> no extra padding. count starts at length+1=4.
        let buf = vec![0x03, b'a', b'b', b'c'];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_pascal_string(&mut r, 2).unwrap(), "abc");
        assert_eq!(r.offset, 4);
    }

    #[test]
    fn pascal_string_with_padding() {
        // length=2, "ab", padTo=4: total with len byte = 3, must pad to 4 -> +1.
        let buf = vec![0x02, b'a', b'b', 0x00, 0xFF];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_pascal_string(&mut r, 4).unwrap(), "ab");
        // consumed: len(1) + text(2) + pad(1) = 4
        assert_eq!(r.offset, 4);
    }

    #[test]
    fn pascal_string_empty() {
        // length=0, padTo=4: count starts at 1, pads to 4 -> 3 extra offset bumps.
        let buf = vec![0x00, 0x00, 0x00, 0x00];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_pascal_string(&mut r, 4).unwrap(), "");
        // len byte read (offset 1) + 3 pad bumps = 4
        assert_eq!(r.offset, 4);
    }

    #[test]
    fn unicode_string_with_length() {
        // "Hi" + trailing \0 -> length 3 code units, big-endian uint16 each.
        let buf = vec![
            0x00, 0x00, 0x00, 0x03, // uint32 length = 3
            0x00, 0x48, // 'H'
            0x00, 0x69, // 'i'
            0x00, 0x00, // trailing \0 (dropped)
        ];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_unicode_string(&mut r).unwrap(), "Hi");
    }

    #[test]
    fn unicode_string_non_ascii() {
        // Cyrillic 'Я' = U+042F
        let buf = vec![
            0x00, 0x00, 0x00, 0x01, // length 1
            0x04, 0x2F, // U+042F
        ];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_unicode_string(&mut r).unwrap(), "Я");
    }

    #[test]
    fn unicode_string_surrogate_pair() {
        // U+1F600 emoji -> surrogate pair D83D DE00
        let buf = vec![
            0x00, 0x00, 0x00, 0x02, // length 2 units
            0xD8, 0x3D, // high surrogate
            0xDE, 0x00, // low surrogate
        ];
        let mut r = PsdReader::new(&buf, None, None);
        assert_eq!(read_unicode_string(&mut r).unwrap(), "😀");
    }

    #[test]
    fn signature_str_is_latin1_codeunits() {
        // bytes > 127 must map to their code point, not be UTF-8 decoded.
        let buf = vec![0xFF, 0x00, b'A', b'B'];
        let mut r = PsdReader::new(&buf, None, None);
        let sig = read_signature(&mut r).unwrap();
        let chars: Vec<u32> = sig.chars().map(|c| c as u32).collect();
        assert_eq!(chars, vec![0xFF, 0x00, 0x41, 0x42]);
    }

    #[test]
    fn read_bytes_recovery_past_end() {
        let buf = vec![0x01, 0x02];
        let mut r = PsdReader::new(&buf, None, None);
        // ask for 4 bytes; only 2 available, not strict -> zero-filled result.
        let out = read_bytes(&mut r, 4).unwrap();
        assert_eq!(out, vec![0x01, 0x02, 0x00, 0x00]);
        assert_eq!(r.offset, 4);
    }

    #[test]
    fn read_bytes_strict_errors() {
        let buf = vec![0x01, 0x02];
        let mut r = PsdReader::new(&buf, None, None);
        r.strict = true;
        // strict mode routes through warn_or_throw -> StrictViolation (upstream `throw`).
        let err = read_bytes(&mut r, 4).unwrap_err();
        assert_eq!(
            err,
            ReadError::StrictViolation("Reading bytes exceeding buffer length".to_string())
        );
    }

    #[test]
    fn section_rounding() {
        // length prefix = 3 (uint32 BE), then 3 payload bytes, round=4.
        // Payload: read 3 bytes via func. After func offset == end (4+3=7).
        // Rounding: length 3 -> 4, end 7 -> 8. Final offset must be 8.
        let buf = vec![
            0x00, 0x00, 0x00, 0x03, // length = 3
            0xAA, 0xBB, 0xCC, // payload (3 bytes)
            0xEE, // padding byte to reach rounded end
        ];
        let mut r = PsdReader::new(&buf, None, None);
        let collected: Vec<u8> = read_section(
            &mut r,
            4,
            |reader, left| {
                assert_eq!(left(reader), 3);
                let a = read_uint8(reader)?;
                let b = read_uint8(reader)?;
                let c = read_uint8(reader)?;
                assert_eq!(left(reader), 0);
                Ok(vec![a, b, c])
            },
            true,
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(collected, vec![0xAA, 0xBB, 0xCC]);
        // end was 7, rounded up to 8.
        assert_eq!(r.offset, 8);
    }

    #[test]
    fn section_empty_skipped() {
        let buf = vec![0x00, 0x00, 0x00, 0x00];
        let mut r = PsdReader::new(&buf, None, None);
        let res: Option<()> =
            read_section(&mut r, 4, |_r, _left| Ok(()), true, false).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn section_eight_bytes() {
        // eightBytes: first uint32 must be 0, then real uint32 length.
        let buf = vec![
            0x00, 0x00, 0x00, 0x00, // high u32 = 0
            0x00, 0x00, 0x00, 0x02, // low u32 = 2 (length)
            0x11, 0x22, // payload
        ];
        let mut r = PsdReader::new(&buf, None, None);
        let res: Option<u16> = read_section(
            &mut r,
            1,
            |reader, _left| read_uint16(reader),
            true,
            true,
        )
        .unwrap();
        assert_eq!(res, Some(0x1122));
    }

    #[test]
    fn section_exceeds_file() {
        let buf = vec![0x00, 0x00, 0x00, 0x10]; // claims 16 bytes but none follow
        let mut r = PsdReader::new(&buf, None, None);
        let err = read_section::<(), _>(&mut r, 1, |_r, _l| Ok(()), true, false).unwrap_err();
        assert_eq!(err, ReadError::SectionExceedsFileSize);
    }

    // -----------------------------------------------------------------------
    // Real-fixture end-to-end pipeline tests (read_psd).
    // -----------------------------------------------------------------------

    fn read_fixture(rel: &str) -> crate::psd::Psd {
        let path = format!(
            "{}/../../test/ag-psd/test/read/{}/src.psd",
            env!("CARGO_MANIFEST_DIR"),
            rel
        );
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {}", path, e));
        let opts = ReadOptions::default();
        read_psd(&bytes, &opts).unwrap_or_else(|e| panic!("read_psd {}: {:?}", rel, e))
    }

    fn count_layers(layers: &[Layer]) -> usize {
        layers.iter().map(|l| 1 + l.children.as_ref().map_or(0, |c| count_layers(c))).sum()
    }

    fn any_layer_has_pixels(layers: &[Layer]) -> bool {
        layers.iter().any(|l| {
            let has = l
                .canvas
                .as_ref()
                .map_or(false, |c| !c.data.is_empty())
                || l.image_data.as_ref().map_or(false, |c| !c.data.is_empty());
            has || l.children.as_ref().map_or(false, |c| any_layer_has_pixels(c))
        })
    }

    #[test]
    fn read_fixture_layers_rgb8() {
        let psd = read_fixture("layers");
        assert_eq!(psd.width, 300.0);
        assert_eq!(psd.height, 200.0);
        assert_eq!(psd.color_mode, Some(ColorMode::Rgb));
        assert_eq!(psd.bits_per_channel, Some(8.0));
        let children = psd.children.as_ref().expect("children");
        assert_eq!(children.len(), 3, "top-level children count");
        assert!(any_layer_has_pixels(children), "at least one layer has pixel data");
        // composite image should be present (not skipped)
        assert!(psd.canvas.as_ref().map_or(false, |c| !c.data.is_empty()));
    }

    #[test]
    fn read_fixture_groups_nesting() {
        let psd = read_fixture("groups");
        assert_eq!(psd.width, 300.0);
        assert_eq!(psd.height, 200.0);
        assert_eq!(psd.color_mode, Some(ColorMode::Rgb));
        let children = psd.children.as_ref().expect("children");
        assert_eq!(children.len(), 2, "top-level children count (2 incl. group)");
        // total layers across the tree should exceed top-level count (nesting).
        assert!(count_layers(children) >= 3);
        assert!(any_layer_has_pixels(children));
    }

    #[test]
    fn read_fixture_just_bg_no_layers() {
        let psd = read_fixture("just-bg");
        assert_eq!(psd.width, 100.0);
        assert_eq!(psd.height, 100.0);
        assert_eq!(psd.color_mode, Some(ColorMode::Rgb));
        let count = psd.children.as_ref().map_or(0, |c| c.len());
        assert_eq!(count, 0, "background-only document has no layer children");
        assert!(psd.canvas.as_ref().map_or(false, |c| !c.data.is_empty()));
    }

    #[test]
    fn new_with_offset_window() {
        let buf = vec![0x00, 0x11, 0x22, 0x33, 0x44];
        let mut r = PsdReader::new(&buf, Some(1), Some(2));
        // window is [0x11, 0x22]; cursor starts at 0 relative to window.
        assert_eq!(read_uint8(&mut r).unwrap(), 0x11);
        assert_eq!(read_uint8(&mut r).unwrap(), 0x22);
        assert!(read_uint8(&mut r).is_err());
    }
}
