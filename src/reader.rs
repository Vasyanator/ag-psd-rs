/*
File: crates/ag-psd/src/reader.rs

Purpose:
низкоуровневое чтение байтов из буфера PSD (курсор чтения, примитивы чтения чисел и строк).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/psdReader.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.

Key functions:
- read_psd / read_psd_from_reader: document orchestration;
- get_layer_image_data / get_layer_mask_image_data /
  get_layer_real_mask_image_data / get_composite_image_data / decode_layer_pixels:
  deferred decoding of bitmaps captured with `ReadOptions::use_raw_data`;
- consume_memory / recover_memory / with_scratch_memory /
  create_image_data_bit_depth: the bitmap memory budget
  (`ReadOptions::total_memory_limit`);
- check_box_size / box_extents: rectangle validation at read time and the safe
  conversion of a validated rectangle into `usize` extents;
- read_pattern: the crate's single implementation of the pattern-record
  primitive, also called by `additional_info::smart_object_keys` (`Patt`/`Pat2`/
  `Pat3`) and by `abr` (the `patt` section);
- inflate_channel_stream / has_zlib_header / zip_stream_length: ZIP channel
  decoding. Photoshop, upstream and this crate's writer emit zlib-wrapped
  deflate, but some third-party writers emit bare DEFLATE, so both framings are
  accepted; `zip_stream_length` recovers the byte length of one stream for the
  composite section, which stores ZIP channels back to back with no length
  table, and charges its scratch buffer against the memory budget;
- sample_to_u8: the single place 16/32-bit channel samples are narrowed to the
  crate's RGBA8 model (16-bit keeps the high byte, 32-bit clamps the float to
  `0.0..=1.0` and scales by 255);
- decode_packbits_row: PackBits row decompression, bounded by the row size the
  caller declared. PackBits amplifies by up to 64x, so decoding a row in full
  before clamping it would let a hostile file allocate gigabytes outside the
  memory budget — the bound is a security property, not an optimization.

Notes:
Validation happens as early as possible: rectangles are checked right after they
are read, and every bitmap allocation is checked against the remaining memory
budget, so a malformed file fails with a typed error instead of exhausting RAM.
Scratch allocations are charged through `with_scratch_memory`, which refunds them
on the error path too (a deliberate divergence from upstream, where a throw
between `consumeMemory` and `recoverMemory` shrinks the budget for good).
*/

// PORT STATUS: primitives + document orchestration ported.
//
// The low-level byte/string/section primitives, the readPsd pipeline
// (layer records, mask data, channel image data, additional layer info,
// composite image data) and the readColor/readPattern helpers are ported.
// Known divergences from upstream are documented at each site; browser-only
// pieces (canvas creation, `*Canvas` accessors) have no Rust equivalent.

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
    /// Upstream `Exceeded memory limit`: a bitmap (or scratch buffer) larger than
    /// the remaining [`crate::psd::ReadOptions::total_memory_limit`] budget was
    /// requested. `requested` and `available` are byte counts.
    ExceededMemoryLimit { requested: usize, available: usize },
    /// Upstream `Invalid layer/mask/realMask size`: a declared rectangle is
    /// inverted or larger than the per-format maximum (30000, 300000 for PSB),
    /// or its extents do not fit `usize`.
    ///
    /// `kind` names the rectangle. The full set is `"layer"`, `"mask"` and
    /// `"realMask"` for the layer record, plus `"pattern"`, `"patternChannel"`
    /// and `"patternChannelOffset"` from the shared pattern reader (the `Patt`/
    /// `Pat2`/`Pat3` layer-info keys and the ABR `patt` section);
    /// `"patternChannelOffset"` reports a channel that starts outside its own
    /// pattern rather than an oversized rectangle.
    InvalidBoxSize { kind: &'static str, width: i64, height: i64 },
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::UnexpectedEndOfBuffer => write!(f, "Reading bytes exceeding buffer length"),
            ReadError::ReadingPastEndOfFile => write!(f, "Reading past end of file"),
            ReadError::InvalidSignature { signature, offset } => {
                // The signature is raw file data and is regularly not text at
                // all (four NUL bytes on a truncated file), so it is escaped:
                // an error message goes to logs and terminals, and must not be
                // able to smuggle control characters into them.
                write!(
                    f,
                    "Invalid signature: '{}' at 0x{:x}",
                    signature.escape_debug(),
                    offset
                )
            }
            ReadError::SizeTooLarge => write!(f, "Sizes larger than 4GB are not supported"),
            ReadError::SectionExceedsFileSize => write!(f, "Section exceeds file size"),
            ReadError::StrictViolation(msg) => write!(f, "{}", msg),
            ReadError::ExceededMemoryLimit { requested, available } => write!(
                f,
                "Exceeded memory limit: needed {} bytes, {} bytes left in budget",
                requested, available
            ),
            ReadError::InvalidBoxSize { kind, width, height } => {
                write!(f, "Invalid {} size: {}x{}", kind, width, height)
            }
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
    /// Remaining bitmap memory budget in bytes, or `None` for unlimited.
    ///
    /// Mutable reader state, exactly as upstream mutates `reader.totalMemoryLimit`
    /// while decoding: allocations charge against it and scratch buffers give
    /// their share back when released. Installed by [`read_psd_from_reader`] from
    /// [`ReadOptions::total_memory_limit`]; sub-readers built with
    /// [`PsdReader::new`] start unlimited (mirror of upstream `createReader`,
    /// which yields an object without a `totalMemoryLimit` property).
    pub total_memory_limit: Option<usize>,
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
            // Upstream `createReader` returns a reader without `totalMemoryLimit`,
            // i.e. unlimited; only `readPsd` installs a budget. Sub-readers over
            // channel/pattern buffers therefore must not inherit one.
            total_memory_limit: None,
            options: ReadOptions::default(),
        }
    }
}

/// Charges `size` bytes against the reader's remaining memory budget.
///
/// Mirror of upstream `consumeMemory`. A `None` budget is unlimited and always
/// succeeds.
///
/// # Errors
/// [`ReadError::ExceededMemoryLimit`] if fewer than `size` bytes are left.
fn consume_memory(reader: &mut PsdReader, size: usize) -> ReadResult<()> {
    if let Some(limit) = reader.total_memory_limit {
        if limit < size {
            return Err(ReadError::ExceededMemoryLimit { requested: size, available: limit });
        }
        reader.total_memory_limit = Some(limit - size);
    }
    Ok(())
}

/// Returns `size` bytes to the reader's memory budget (mirror of upstream
/// `recoverMemory`), used when a scratch buffer charged by [`consume_memory`]
/// goes out of scope. Saturates instead of overflowing.
fn recover_memory(reader: &mut PsdReader, size: usize) {
    if let Some(limit) = reader.total_memory_limit {
        reader.total_memory_limit = Some(limit.saturating_add(size));
    }
}

/// Runs `f` with `size` bytes of scratch memory charged against the budget and
/// gives them back afterwards — including when `f` fails.
///
/// Deliberate divergence from upstream: upstream calls `consumeMemory` and
/// `recoverMemory` as plain statements, so any exception thrown in between
/// permanently shrinks `reader.totalMemoryLimit`. Reads are fallible here and a
/// caller may keep using the same reader, so the refund must not be skipped on
/// the error path.
fn with_scratch_memory<T, F>(reader: &mut PsdReader, size: usize, f: F) -> ReadResult<T>
where
    F: FnOnce(&mut PsdReader) -> ReadResult<T>,
{
    consume_memory(reader, size)?;
    let result = f(reader);
    recover_memory(reader, size);
    result
}

/// Byte size upstream would allocate for a `width x height x channels` bitmap at
/// `bit_depth`, used for memory accounting.
///
/// Mirror of upstream `width * height * channels * Math.max(1, bitDepth / 8)`.
/// This port always materializes 8-bit samples (see [`DecodeTarget`]), but the
/// *check* follows upstream's formula so that the same file hits the limit in
/// both implementations. Saturating, so an absurd declared size reports as "too
/// big" instead of wrapping.
fn image_data_size_in_bytes(
    width: usize,
    height: usize,
    channels: usize,
    bit_depth: u32,
) -> usize {
    let bytes_per_sample = (bit_depth.max(8) / 8) as usize;
    width
        .saturating_mul(height)
        .saturating_mul(channels)
        .saturating_mul(bytes_per_sample)
}

/// Allocates a decode target after checking it against `memory_limit`.
///
/// Mirror of upstream `createImageDataBitDepth(width, height, bitDepth, channels,
/// memoryLimit)`: the limit is only *checked* here, charging it is the caller's
/// job (upstream subtracts the size after the call).
///
/// # Errors
/// [`ReadError::ExceededMemoryLimit`] if the bitmap does not fit the budget.
fn create_image_data_bit_depth(
    width: usize,
    height: usize,
    bit_depth: u32,
    channels: usize,
    memory_limit: Option<usize>,
) -> ReadResult<DecodeTarget> {
    let size_in_bytes = image_data_size_in_bytes(width, height, channels, bit_depth);
    if let Some(limit) = memory_limit {
        if size_in_bytes > limit {
            return Err(ReadError::ExceededMemoryLimit {
                requested: size_in_bytes,
                available: limit,
            });
        }
    }
    Ok(DecodeTarget::wide(width, height, channels))
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

/// Mirror of the upstream `supportedColorModes` array, kept in upstream order.
const SUPPORTED_COLOR_MODES: [u16; 4] = [0, 1, 3, 2]; // Bitmap, Grayscale, RGB, Indexed

/// Mirror `supportedColorModes.indexOf(colorMode) !== -1`.
fn is_supported_color_mode(mode: u16) -> bool {
    SUPPORTED_COLOR_MODES.contains(&mode)
}

/// Mirror of the upstream `colorModes` name table, used only for error messages.
///
/// The table is indexed by the raw color-mode number, and codes 5 and 6 are
/// unassigned in the PSD format — upstream keeps two empty slots there so that
/// `multichannel` (7) / `duotone` (8) / `lab` (9) land on their own index.
/// Unassigned or unknown codes return `None` and are reported numerically.
fn color_mode_name(mode: u16) -> Option<&'static str> {
    match mode {
        0 => Some("bitmap"),
        1 => Some("grayscale"),
        2 => Some("indexed"),
        3 => Some("RGB"),
        4 => Some("CMYK"),
        7 => Some("multichannel"),
        8 => Some("duotone"),
        9 => Some("lab"),
        _ => None,
    }
}

/// Mirror of upstream `isValidBoxSize`: a layer/mask rectangle must be
/// non-inverted and no larger than 30000 per side (300000 for PSB / `large`).
///
/// The bounds arrive as `f64` because that is how the document model stores
/// them; they always hold values read with `read_int32`.
fn is_valid_box_size(top: f64, left: f64, bottom: f64, right: f64, large: bool) -> bool {
    let width = right - left;
    let height = bottom - top;
    let max_size = if large { 300000.0 } else { 30000.0 };
    width >= 0.0 && height >= 0.0 && width <= max_size && height <= max_size
}

/// Validates a rectangle read from the file, turning an invalid one into a typed
/// error naming the rectangle (`"layer"`, `"mask"`, `"realMask"`).
fn check_box_size(
    kind: &'static str,
    top: f64,
    left: f64,
    bottom: f64,
    right: f64,
    large: bool,
) -> ReadResult<()> {
    if is_valid_box_size(top, left, bottom, right, large) {
        Ok(())
    } else {
        Err(ReadError::InvalidBoxSize {
            kind,
            width: (right - left) as i64,
            height: (bottom - top) as i64,
        })
    }
}

/// Converts a rectangle that already passed [`check_box_size`] into its
/// `(width, height)` extents.
///
/// `top`/`left`/`bottom`/`right` must originate from 32-bit file reads, so the
/// subtractions cannot overflow `i64`.
///
/// # Errors
/// [`ReadError::InvalidBoxSize`] if an extent is negative or does not fit
/// `usize`. That is unreachable after a successful [`check_box_size`], but it is
/// returned rather than asserted so no unchecked cast is ever needed.
fn box_extents(
    kind: &'static str,
    top: i64,
    left: i64,
    bottom: i64,
    right: i64,
) -> ReadResult<(usize, usize)> {
    let width = right - left;
    let height = bottom - top;
    match (usize::try_from(width), usize::try_from(height)) {
        (Ok(w), Ok(h)) => Ok((w, h)),
        _ => Err(ReadError::InvalidBoxSize { kind, width, height }),
    }
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

/// Hand-written so that a decode target in an error/debug message reports its
/// shape instead of dumping megabytes of pixels.
impl std::fmt::Debug for DecodeTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodeTarget")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("channels", &self.channels)
            .field("data_len", &self.data.len())
            .finish()
    }
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
        // Upstream prints the color mode name when it knows one, the raw number
        // otherwise (`colorModes[colorMode] ?? colorMode`).
        return Err(ReadError::StrictViolation(match color_mode_name(color_mode_raw) {
            Some(name) => format!("Color mode not supported: {}", name),
            None => format!("Color mode not supported: {}", color_mode_raw),
        }));
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
    // Install the bitmap memory budget from the options (upstream assigns the
    // options onto the reader and defaults `totalMemoryLimit` to 2GB here).
    reader.total_memory_limit = reader.options.total_memory_limit;

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
                // The palette is stored plane by plane: all 256 red bytes, then
                // all 256 green, then all 256 blue.
                for entry in &mut pal {
                    entry.g = read_uint8(reader)? as f64;
                }
                for entry in &mut pal {
                    entry.b = read_uint8(reader)? as f64;
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
    // Mirror `if (Object.keys(rest).length) psd.imageResources = rest;` — upstream's
    // guard used to be `if (Object.keys(rest))`, which is always truthy, so an empty
    // bag was assigned unconditionally. `ImageResources` already models exactly `rest`
    // (layersGroup / layerGroupsEnabledId are skipped, never stored).
    if !image_resources.is_empty() {
        psd.image_resources = Some(image_resources);
    }

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
                    // The document is handed over as well, so `Lr16`/`Lr32`
                    // (the layer records of 16/32-bit files) can recurse back
                    // into `read_layer_info`. `additional_info` is taken out
                    // first so the two borrows stay disjoint.
                    let mut info = std::mem::take(&mut psd.additional_info);
                    read_additional_layer_info(reader, &mut info, Some(&mut psd))?;
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

    let has_children = psd.children.as_ref().is_some_and(|c| !c.is_empty());
    let skip_layer = reader.options.skip_layer_image_data == Some(true);
    let skip_composite =
        reader.options.skip_composite_image_data == Some(true) && (skip_layer || has_children);

    if !skip_composite {
        if reader.options.use_raw_data == Some(true) {
            // Capture the composite section undecoded: from the current cursor to
            // the end of the file, exactly like upstream's
            // `new Uint8Array(view.buffer, view.byteOffset + reader.offset)`.
            // The copy is unavoidable here because `Psd` owns its data.
            psd.raw_composite_data =
                Some(reader.buffer[reader.offset.min(reader.buffer.len())..].to_vec());
        } else {
            let image_data = read_image_data(reader, &psd)?;
            if reader.options.use_image_data == Some(true) {
                psd.image_data = Some(image_data);
            } else {
                psd.canvas = Some(image_data_to_canvas(&image_data));
            }
        }
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
    // Upstream unshifts into the existing `psd.children`, so a second
    // `readLayerInfo` pass (the `Lr16`/`Lr32` sections of a 16/32-bit document)
    // prepends its layers rather than replacing what is already there. For the
    // usual single-pass case `children` is empty and this is a plain move.
    let mut merged = root.children;
    merged.append(children);
    *children = merged;
}

// ---------------------------------------------------------------------------
// readLayerRecord
// ---------------------------------------------------------------------------

fn read_layer_record(
    reader: &mut PsdReader,
    _psd: &mut crate::psd::Psd,
) -> ReadResult<(Layer, Vec<ChannelInfo>)> {
    let mut layer = Layer::default();
    let top = read_int32(reader)? as f64;
    let left = read_int32(reader)? as f64;
    let bottom = read_int32(reader)? as f64;
    let right = read_int32(reader)? as f64;
    // Validate before anything downstream sizes a buffer from these numbers.
    check_box_size("layer", top, left, bottom, right, reader.large)?;
    layer.top = Some(top);
    layer.left = Some(left);
    layer.bottom = Some(bottom);
    layer.right = Some(right);

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
                // Layer-level: no document in scope, so a (never observed in
                // practice) nested `Lr16`/`Lr32` here is reported by the group
                // module instead of being silently dropped.
                read_additional_layer_info(reader, &mut info, None)?;
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
            // `box_*` and not `left`/`right`: `left` is the section-remainder
            // closure in this scope.
            let box_top = read_int32(reader)? as f64;
            let box_left = read_int32(reader)? as f64;
            let box_bottom = read_int32(reader)? as f64;
            let box_right = read_int32(reader)? as f64;
            check_box_size("mask", box_top, box_left, box_bottom, box_right, reader.large)?;
            mask.top = Some(box_top);
            mask.left = Some(box_left);
            mask.bottom = Some(box_bottom);
            mask.right = Some(box_right);
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
                let box_top = read_int32(reader)? as f64;
                let box_left = read_int32(reader)? as f64;
                let box_bottom = read_int32(reader)? as f64;
                let box_right = read_int32(reader)? as f64;
                check_box_size(
                    "realMask",
                    box_top,
                    box_left,
                    box_bottom,
                    box_right,
                    reader.large,
                )?;
                real_mask.top = Some(box_top);
                real_mask.left = Some(box_left);
                real_mask.bottom = Some(box_bottom);
                real_mask.right = Some(box_right);
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
        // Upstream passes the reader itself as the options object, so decoded
        // layer bitmaps permanently charge the document-wide budget (they stay
        // alive on the layer, so nothing is given back).
        decode_layer_image_data(
            layer,
            use_image_data,
            throw_missing,
            &mut reader.total_memory_limit,
        )?;
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

/// Numeric compression code as stored in the file, used to phrase the
/// upstream-compatible `Compression not supported: N` errors.
fn compression_code(compression: Compression) -> u16 {
    match compression {
        Compression::RawData => 0,
        Compression::RleCompressed => 1,
        Compression::ZipWithoutPrediction => 2,
        Compression::ZipWithPrediction => 3,
    }
}

/// Builds the unified "compression not supported" error (upstream phrases every
/// such failure as `Compression not supported: N`).
fn compression_not_supported(compression: Compression) -> ReadError {
    ReadError::StrictViolation(format!(
        "Compression not supported: {}",
        compression_code(compression)
    ))
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

/// Which bitmap of a layer [`get_data_from_layer`] should decode.
///
/// Mirror of the upstream `LayerDataType` enum: a layer's captured raw data
/// holds the layer bitmap and up to two mask channels, and each call decodes
/// exactly one of them.
///
/// Crate-internal, exactly like upstream (`psdReader.ts` keeps the enum
/// module-private and exports only the three concrete `getLayer*ImageData`
/// wrappers). A `pub` selector with no `pub` function taking it would be dead
/// public API, and the wrappers already cover every variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LayerDataType {
    /// The layer bitmap itself (colour + transparency channels).
    Layer,
    /// The user mask channel (`ChannelId::UserMask`).
    Mask,
    /// The "real" (vector-derived) mask channel (`ChannelId::RealUserMask`).
    RealMask,
}

/// Decodes the layer bitmap of a layer read with
/// [`crate::psd::ReadOptions::use_raw_data`], leaving the raw data in place.
///
/// Mirror of upstream `getLayerImageData`. Returns `Ok(None)` when the layer
/// carries no raw data (already decoded, or image data was skipped) or has an
/// empty rectangle. Decoding is unlimited memory-wise, as upstream.
pub fn get_layer_image_data(layer: &Layer) -> ReadResult<Option<PixelData>> {
    get_data_from_layer(layer, LayerDataType::Layer, false, None)
}

/// Mask counterpart of [`get_layer_image_data`] (upstream
/// `getLayerMaskImageData`); returns `Ok(None)` if the layer has no mask data.
pub fn get_layer_mask_image_data(layer: &Layer) -> ReadResult<Option<PixelData>> {
    get_data_from_layer(layer, LayerDataType::Mask, false, None)
}

/// Real-mask counterpart of [`get_layer_image_data`] (upstream
/// `getLayerRealMaskImageData`).
pub fn get_layer_real_mask_image_data(layer: &Layer) -> ReadResult<Option<PixelData>> {
    get_data_from_layer(layer, LayerDataType::RealMask, false, None)
}

/// Decodes the composite bitmap of a document read with
/// [`crate::psd::ReadOptions::use_raw_data`].
///
/// Mirror of upstream `getCompositeImageData`; returns `Ok(None)` when
/// [`crate::psd::Psd::raw_composite_data`] is absent.
///
/// Limitation inherited from upstream: the throw-away reader built here starts
/// with `large = false` and `global_alpha = false`, because neither is part of
/// the document model. Deferred decoding therefore only reproduces the eager
/// path for PSD (not PSB) files whose layer count was not negative.
pub fn get_composite_image_data(psd: &crate::psd::Psd) -> ReadResult<Option<PixelData>> {
    let data = match psd.raw_composite_data.as_ref() {
        Some(d) => d,
        None => return Ok(None),
    };
    let mut reader = PsdReader::new(data, None, None);
    read_image_data(&mut reader, psd).map(Some)
}

/// Decodes the bitmaps captured in `layer.raw_data` into
/// `canvas`/`image_data` fields and drops the raw data.
///
/// Mirror of upstream `decodeLayerPixels`: no memory budget is applied (the
/// caller explicitly asked for this work), and `use_image_data` selects between
/// the `image_data` and `canvas` fields, exactly as during reading.
pub fn decode_layer_pixels(layer: &mut Layer, use_image_data: bool) -> ReadResult<()> {
    let mut no_limit = None;
    decode_layer_image_data(layer, use_image_data, false, &mut no_limit)
}

/// Stores a decoded mask bitmap in the field selected by `use_image_data`
/// (mirror of upstream `setImageDataOrCanvas`).
fn set_mask_image_data_or_canvas(
    mask: &mut LayerMaskData,
    decoded: PixelData,
    use_image_data: bool,
) {
    if use_image_data {
        mask.image_data = Some(decoded);
    } else {
        mask.canvas = Some(image_data_to_canvas(&decoded));
    }
}

/// Mirror `decodeLayerImageData`: decodes layer, mask and real-mask bitmaps and
/// clears `layer.raw_data`.
///
/// `memory_limit` is the caller's remaining budget in bytes (`None` =
/// unlimited); every decoded bitmap is charged against it and it is *not*
/// refunded, because the decoded pixels stay alive on the layer.
fn decode_layer_image_data(
    layer: &mut Layer,
    use_image_data: bool,
    throw_for_missing_features: bool,
    memory_limit: &mut Option<usize>,
) -> ReadResult<()> {
    if layer.raw_data.is_none() {
        return Ok(());
    }

    let decoded = get_data_from_layer(
        layer,
        LayerDataType::Layer,
        throw_for_missing_features,
        *memory_limit,
    )?;
    if let Some(pd) = decoded {
        charge_decoded(memory_limit, &pd);
        if use_image_data {
            layer.image_data = Some(pd);
        } else {
            layer.canvas = Some(image_data_to_canvas(&pd));
        }
    }

    if layer.additional_info.mask.is_some() {
        let decoded = get_data_from_layer(
            layer,
            LayerDataType::Mask,
            throw_for_missing_features,
            *memory_limit,
        )?;
        if let Some(pd) = decoded {
            charge_decoded(memory_limit, &pd);
            if let Some(mask) = layer.additional_info.mask.as_mut() {
                set_mask_image_data_or_canvas(mask, pd, use_image_data);
            }
        }
    }

    if layer.additional_info.real_mask.is_some() {
        let decoded = get_data_from_layer(
            layer,
            LayerDataType::RealMask,
            throw_for_missing_features,
            *memory_limit,
        )?;
        if let Some(pd) = decoded {
            charge_decoded(memory_limit, &pd);
            if let Some(mask) = layer.additional_info.real_mask.as_mut() {
                set_mask_image_data_or_canvas(mask, pd, use_image_data);
            }
        }
    }

    layer.raw_data = None;
    Ok(())
}

/// Subtracts the size of a decoded bitmap from the remaining budget, saturating
/// at zero (the allocation itself was already validated against the budget).
///
/// Upstream charges `imageData.data.byteLength`; this port's decode targets are
/// always 8-bit, so the charge is the real allocation, which for >8 bit files is
/// smaller than the amount [`create_image_data_bit_depth`] checked.
fn charge_decoded(memory_limit: &mut Option<usize>, decoded: &PixelData) {
    if let Some(limit) = *memory_limit {
        *memory_limit = Some(limit.saturating_sub(decoded.data.len()));
    }
}

/// Mirror `getDataFromLayer`: decodes one of the bitmaps stored in
/// `layer.raw_data` without consuming it.
///
/// Returns `Ok(None)` when there is no raw data, when the requested rectangle is
/// empty, or when the requested channel is absent.
///
/// # Errors
/// [`ReadError::ExceededMemoryLimit`] when the bitmap does not fit
/// `memory_limit`, and [`ReadError::StrictViolation`] for unsupported channel
/// layouts (only when `throw_for_missing_features` is set) or a mask channel
/// without matching mask metadata.
fn get_data_from_layer(
    layer: &Layer,
    read: LayerDataType,
    throw_for_missing_features: bool,
    memory_limit: Option<usize>,
) -> ReadResult<Option<PixelData>> {
    let raw = match layer.raw_data.as_ref() {
        Some(r) => r,
        None => return Ok(None),
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
    let mut mask_data: Option<DecodeTarget> = None;
    let mut initialized_alpha = false;

    if layer_width != 0 && layer_height != 0 && read == LayerDataType::Layer {
        if cmyk {
            if bits_per_channel != 8 {
                return Err(ReadError::StrictViolation("bitsPerChannel Not supproted".to_string()));
            }
            // CMYK keeps 5 interleaved 8-bit channels. Upstream allocates this one
            // without consulting the budget; we check it too so that no decode
            // path can escape the limit.
            image_data =
                Some(create_image_data_bit_depth(layer_width, layer_height, 8, 5, memory_limit)?);
        } else {
            image_data = Some(create_image_data_bit_depth(
                layer_width,
                layer_height,
                bits_per_channel,
                4,
                memory_limit,
            )?);
        }
    }

    for ch in &raw.channels {
        let data = match &ch.data {
            Some(d) => d,
            None => continue,
        };
        let mut data_reader = PsdReader::new(data, None, None);

        if ch.id == ChannelId::UserMask || ch.id == ChannelId::RealUserMask {
            // Each call decodes exactly one bitmap; skip channels of other kinds.
            if ch.id == ChannelId::UserMask && read != LayerDataType::Mask {
                continue;
            }
            if ch.id == ChannelId::RealUserMask && read != LayerDataType::RealMask {
                continue;
            }

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
            // The rectangle was already validated when the mask record was read,
            // so an inverted box can only come from a hand-built `Layer`; clamp
            // it to empty instead of failing (mirrors upstream's `Math.max(0, ..)`).
            let mw = (mright - mleft).max(0.0) as usize;
            let mh = (mbottom - mtop).max(0.0) as usize;
            if mw != 0 && mh != 0 {
                let mut target =
                    create_image_data_bit_depth(mw, mh, bits_per_channel, 4, memory_limit)?;
                read_data(
                    &mut data_reader,
                    data.len(),
                    Some(&mut target),
                    ch.compression,
                    mw,
                    mh,
                    bits_per_channel,
                    0,
                    large,
                    4,
                )?;
                setup_grayscale(&mut target.data, mw, mh);
                reset_alpha(&mut target, false);
                mask_data = Some(target);
            }
        } else {
            // Colour/transparency channels only contribute to the layer bitmap.
            if read != LayerDataType::Layer {
                continue;
            }

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

    let layer_pixels = image_data.map(|mut img| {
        if !initialized_alpha {
            reset_alpha(&mut img, cmyk);
        }

        if cmyk {
            let mut rgb = create_image_data(img.width as u32, img.height as u32);
            cmyk_to_rgb(&img, &mut rgb, false);
            rgb
        } else {
            img.into_pixel_data()
        }
    });

    Ok(match read {
        LayerDataType::Layer => layer_pixels,
        LayerDataType::Mask | LayerDataType::RealMask => {
            mask_data.map(DecodeTarget::into_pixel_data)
        }
    })
}

// ---------------------------------------------------------------------------
// Channel image-data codecs
// ---------------------------------------------------------------------------

/// Mirror `readData` dispatch.
// The parameter list is upstream's `readData(reader, length, pixels, compression,
// width, height, bitDepth, offset, large, step)` reproduced 1:1. Bundling the
// arguments into a struct would desynchronise this function from the reference
// implementation it is diffed against on every upstream sync, and the callers
// below pass exactly the same tuple upstream passes, so the grouping would be
// artificial.
#[allow(clippy::too_many_arguments)]
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

/// Converts one 32-bit float channel sample to its 8-bit target byte.
///
/// Samples outside `[0, 1]` are clamped before scaling and `NaN` becomes `0`,
/// which is what a store into upstream's `Uint8ClampedArray` does.
//
// `v.max(0.0).min(1.0)` is deliberate and not `v.clamp(0.0, 1.0)`: `clamp`
// propagates `NaN`, whereas `max` followed by `min` turns `NaN` into `0.0`.
// Only the `max`/`min` form reproduces the upstream clamped-array semantics at
// the float level, so the lint's rewrite is not behaviour-preserving here.
#[allow(clippy::manual_clamp)]
#[inline]
fn f32_sample_to_u8(v: f32) -> u8 {
    (v.max(0.0).min(1.0) * 255.0).round() as u8
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
                out.push(f32_sample_to_u8(v));
                i += 4;
            }
            out
        }
        _ => buffer.to_vec(),
    }
}

/// Down-converts one big-endian channel sample to the crate's RGBA8 target byte.
///
/// `bytes` must start at the sample and be at least
/// `helpers::bytes_per_sample(bit_depth)` long; a shorter slice yields 0. A
/// 16-bit sample keeps its most significant byte (the exact inverse of the
/// writer's `sample * 257` expansion), and a 32-bit float is clamped to
/// `0.0..=1.0` and scaled by 255. An unsupported depth yields 0 — callers
/// reject such depths before decoding.
fn sample_to_u8(bytes: &[u8], bit_depth: u32) -> u8 {
    match bit_depth {
        8 | 16 => bytes.first().copied().unwrap_or(0),
        32 => bytes
            .get(..4)
            .and_then(|b| <[u8; 4]>::try_from(b).ok())
            .map_or(0, |b| f32_sample_to_u8(f32::from_be_bytes(b))),
        _ => 0,
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

/// Length in bytes of the compressed channel stream starting at the reader's
/// cursor, found by inflating it and asking the decompressor how much it read.
///
/// The composite image section stores ZIP channels back to back with no length
/// table, so the only way to find where one ends is to decompress it. The
/// cursor is *not* moved; the caller advances it by the returned length.
///
/// `expected_output` is `width * height * bytes_per_sample` — the exact size of
/// one decompressed channel. That scratch buffer is charged against
/// [`crate::psd::ReadOptions::total_memory_limit`] for the duration of the call
/// and refunded afterwards, error paths included, per the crate's memory
/// budget contract.
///
/// Both channel framings are accepted, exactly like [`inflate_channel_stream`].
///
/// # Errors
/// [`ReadError::ExceededMemoryLimit`] if the scratch buffer does not fit the
/// budget, and [`ReadError::StrictViolation`] if no stream ending at exactly
/// `expected_output` bytes decodes here under either framing. The exact-size
/// check is what makes the recovered length trustworthy: a channel of the
/// declared bitmap must produce exactly one bitmap's worth of samples, and a
/// stream that stops short would otherwise hand the caller a cursor advance
/// that lands in the middle of the next channel.
fn zip_stream_length(reader: &mut PsdReader, expected_output: usize) -> ReadResult<usize> {
    use flate2::{Decompress, FlushDecompress, Status};

    // `buffer` is a shared slice reference, so copying it out keeps the scratch
    // closure from holding a borrow of `reader`.
    let start = reader.offset.min(reader.buffer.len());
    let input: &[u8] = &reader.buffer[start..];
    let zlib_first = has_zlib_header(input);

    with_scratch_memory(reader, expected_output, |_| {
        let mut output = vec![0u8; expected_output];
        for attempt in 0..2 {
            let zlib_wrapped = if attempt == 0 { zlib_first } else { !zlib_first };
            let mut decompressor = Decompress::new(zlib_wrapped);
            // A stream longer than `expected_output` fills `output` and yields
            // `Status::Ok` instead of `StreamEnd`, so only the short case has
            // to be rejected explicitly.
            if let Ok(Status::StreamEnd) =
                decompressor.decompress(input, &mut output, FlushDecompress::Finish)
            {
                if u64::try_from(expected_output).is_ok_and(|n| decompressor.total_out() == n) {
                    // `total_in` counts the bytes of this stream only, so it is
                    // bounded by `input.len()` and always fits `usize`.
                    return usize::try_from(decompressor.total_in())
                        .map_err(|_| ReadError::UnexpectedEndOfBuffer);
                }
            }
        }
        Err(ReadError::StrictViolation(format!(
            "Invalid ZIP channel data at 0x{:x}: no zlib or raw DEFLATE stream of {} bytes decodes here",
            start, expected_output
        )))
    })
}

/// Whether a compressed channel stream starts with an RFC 1950 zlib header.
///
/// The header is two bytes: the low nibble of CMF is the compression method (8
/// for deflate) and `CMF * 256 + FLG` is a multiple of 31. A raw RFC 1951
/// stream passes this test only by coincidence, which is why the framing is
/// probed in this order rather than by trying to inflate twice.
fn has_zlib_header(data: &[u8]) -> bool {
    match data {
        [cmf, flg, ..] => {
            (cmf & 0x0f) == 8 && (u16::from(*cmf) * 256 + u16::from(*flg)) % 31 == 0
        }
        _ => false,
    }
}

/// Inflates one compressed channel stream, accepting both framings found in the
/// wild.
///
/// Photoshop, upstream ag-psd and this crate's writer all emit zlib-wrapped
/// deflate (RFC 1950), but some third-party PSD writers emit a bare deflate
/// stream (RFC 1951). The framing suggested by [`has_zlib_header`] is tried
/// first and the other one is the fallback, so neither form is rejected.
///
/// Returns `None` when the data decodes under neither framing.
fn inflate_channel_stream(compressed: &[u8]) -> Option<Vec<u8>> {
    use flate2::read::{DeflateDecoder, ZlibDecoder};
    use std::io::Read;

    let mut zlib_first = has_zlib_header(compressed);
    for _ in 0..2 {
        let mut decompressed: Vec<u8> = Vec::new();
        let read = if zlib_first {
            ZlibDecoder::new(compressed).read_to_end(&mut decompressed)
        } else {
            DeflateDecoder::new(compressed).read_to_end(&mut decompressed)
        };
        if read.is_ok() {
            return Some(decompressed);
        }
        zlib_first = !zlib_first;
    }
    None
}

/// Mirror `readDataZip` (zlib via flate2).
// Upstream exports `readDataZip` with exactly these eight positional
// parameters; this is a published function of the crate, so regrouping them
// would break both the public API and the 1:1 correspondence with the
// reference implementation.
#[allow(clippy::too_many_arguments)]
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
    let mut decompressed = match inflate_channel_stream(compressed) {
        Some(data) => data,
        None => return,
    };

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
                // Photoshop's predicted stream stores each byte plane in a
                // row; reconstruct one float from the four planes.
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
                        pixel_data.data[p] = f32_sample_to_u8(v);
                        p += step;
                    }
                }
            } else {
                // Without prediction, channels are ordinary big-endian
                // float samples, matching the writer's expansion contract.
                // `chunks_exact` yields only full four-byte chunks, but the
                // sample count is file-derived, so the destination index is
                // bounds-checked rather than trusted.
                let mut p = offset;
                for chunk in decompressed.chunks_exact(4).take(width * height) {
                    if p >= pixel_data.data.len() {
                        break;
                    }
                    let sample = match <[u8; 4]>::try_from(chunk) {
                        Ok(bytes) => f32::from_be_bytes(bytes),
                        Err(_) => break,
                    };
                    pixel_data.data[p] = f32_sample_to_u8(sample);
                    p += step;
                }
            }
        }
        _ => {}
    }
}

/// Mirror `readDataRLE` (PackBits), down-converting 16/32-bit samples into the
/// crate's RGBA8 target after each row has been decompressed.
// Upstream exports `readDataRLE` with exactly these eight positional
// parameters; this is a published function of the crate, so regrouping them
// would break both the public API and the 1:1 correspondence with the
// reference implementation.
#[allow(clippy::too_many_arguments)]
pub fn read_data_rle(
    reader: &mut PsdReader,
    mut pixel_data: Option<&mut DecodeTarget>,
    width: usize,
    height: usize,
    bit_depth: u32,
    step: usize,
    offsets: &[usize],
    large: bool,
) -> ReadResult<()> {
    // The line-length table is scratch memory: charge it while it is alive and
    // give it back at the end (also on the error path, hence
    // `with_scratch_memory`).
    //
    // Deliberate divergence from upstream: upstream charges the size of its
    // `Uint16Array`/`Uint32Array` (2 bytes per entry, 4 for PSB), which would
    // under-count here — the table below is a `Vec<u32>`, so the charge is the
    // real allocation size. The point of the budget is bounding real memory, so
    // matching upstream byte-for-byte is the wrong tie-breaker; the same file
    // may therefore hit the limit here slightly earlier than in upstream.
    let entry_size = std::mem::size_of::<u32>();
    let lengths_bytes = offsets.len().saturating_mul(height).saturating_mul(entry_size);
    with_scratch_memory(reader, lengths_bytes, move |reader| {
        // Row byte counts are `u16` (PSD) or `u32` (PSB) in the format itself,
        // so `u32` stores them exactly.
        let mut lengths: Vec<u32> = Vec::with_capacity(offsets.len().saturating_mul(height));
        if large {
            for _ in 0..offsets.len() {
                for _ in 0..height {
                    lengths.push(read_uint32(reader)?);
                }
            }
        } else {
            for _ in 0..offsets.len() {
                for _ in 0..height {
                    lengths.push(u32::from(read_uint16(reader)?));
                }
            }
        }

        // The PackBits stream is a byte stream whatever the depth, but how many
        // of those bytes make one sample decides the down-conversion below.
        let bytes_per_sample = crate::helpers::bytes_per_sample(bit_depth).ok_or_else(|| {
            ReadError::StrictViolation(format!(
                "Unsupported bit depth for RLE channel data: {}",
                bit_depth
            ))
        })?;
        let extra_limit = step.saturating_sub(1);

        let mut li = 0usize;
        for (c, &offset) in offsets.iter().enumerate() {
            let extra = c > extra_limit || offset > extra_limit;

            let have_data = pixel_data.is_some() && !extra;
            if !have_data {
                for _ in 0..height {
                    // u32 -> usize is a widening conversion on every supported
                    // target (32- and 64-bit), so it cannot truncate.
                    let len = lengths[li] as usize;
                    li += 1;
                    skip_bytes(reader, len);
                }
                continue;
            }

            // `have_data` above proved this is `Some`; reborrowing it once
            // here instead of per pixel removes the only place in this
            // function that had to assert the invariant at run time.
            let Some(pd) = pixel_data.as_deref_mut() else {
                continue;
            };

            // The row a PackBits stream is allowed to produce. Decoding stops
            // there, so a hostile row cannot amplify into an unbounded buffer.
            let row_bytes = width.saturating_mul(bytes_per_sample);

            let mut p = offset;
            for _ in 0..height {
                let length = lengths[li] as usize;
                li += 1;
                let buffer = read_bytes(reader, length)?;
                let decoded = decode_packbits_row(&buffer, row_bytes);
                for x in 0..width {
                    let start = x * bytes_per_sample;
                    let Some(sample) = decoded.get(start..start + bytes_per_sample) else {
                        // The row decompressed short: every remaining pixel is
                        // missing too, so stop instead of scanning past it.
                        break;
                    };
                    let value = sample_to_u8(sample, bit_depth);
                    if p < pd.data.len() {
                        pd.data[p] = value;
                    }
                    p += step;
                }
            }
            // assignment of p back happens implicitly via loop continuation; in
            // upstream p resets per channel via offset, which we did at loop top.
            let _ = p;
        }

        Ok(())
    })
}

/// Decompresses one PackBits row, stopping after `max_len` bytes.
///
/// A header byte of `0..=127` introduces a literal run of `header + 1` bytes,
/// `129..=255` a repeat of the following byte `257 - header` times, and `128`
/// is a no-op. A truncated row stops decoding instead of erroring, mirroring
/// upstream's tolerance for short channel data.
///
/// `max_len` is the number of bytes the caller can actually consume — one row,
/// `width * bytes_per_sample`. It is a hard bound, not a post-hoc clamp, and it
/// is the reason this function is safe on untrusted input: PackBits amplifies
/// by up to 64x (a two-byte run header expands to 128 bytes), so decoding a
/// hostile row in full would allocate gigabytes for a bitmap declared to be a
/// few pixels wide, outside the `ReadOptions::total_memory_limit` budget. The
/// returned buffer therefore never exceeds `max_len` bytes.
pub(crate) fn decode_packbits_row(buffer: &[u8], max_len: usize) -> Vec<u8> {
    // Reserve the smaller of the two bounds: what the caller will consume, and
    // the most this input could possibly expand to (64x plus one run). A bogus
    // `max_len` therefore cannot allocate ahead of the data justifying it.
    let capacity = max_len.min(buffer.len().saturating_mul(64).saturating_add(128));
    let mut decoded = Vec::with_capacity(capacity);
    let mut i = 0usize;
    while i < buffer.len() && decoded.len() < max_len {
        let remaining = max_len - decoded.len();
        let header = buffer[i];
        i += 1;
        if header > 128 {
            if i >= buffer.len() {
                break;
            }
            let count = (257usize - usize::from(header)).min(remaining);
            decoded.extend(std::iter::repeat_n(buffer[i], count));
            i += 1;
        } else if header < 128 {
            let count = usize::from(header) + 1;
            let end = (i + count).min(buffer.len());
            let taken = (end - i).min(remaining);
            decoded.extend_from_slice(&buffer[i..i + taken]);
            i = end;
        }
    }
    decoded
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
///
/// `psd` is the document the section belongs to and is `Some` **only** for the
/// document-level additional info blocks. It is what makes the recursive
/// `Lr16`/`Lr32` sections readable: those carry the complete layer-info block of
/// a 16/32-bit document (Photoshop leaves the ordinary `Layr` section empty for
/// those files), and upstream's handler answers them by calling `readLayerInfo`
/// again. The group-module dispatch in `additional_info/` only ever sees a
/// `LayerAdditionalInfo`, so the recursion has to happen here, where the
/// document is in scope. Layer-level sections pass `None`.
///
/// Error handling mirrors upstream's `try { handler.read(..) } catch`: a failing
/// group-module handler is swallowed and the rest of the section skipped unless
/// `ReadOptions::throw_for_missing_features` is set.
///
/// DELIBERATE DIVERGENCE FROM UPSTREAM: the nested `Lr16`/`Lr32` read is *not*
/// covered by that catch-all. Upstream funnels it through the same `try/catch`,
/// so a rejected rectangle or an exhausted `total_memory_limit` inside the
/// section leaves the document with no layers and no word about it. Those are
/// the very guards that make the read path safe on hostile input, and losing a
/// document's layers is not a "missing feature", so the error propagates.
fn read_additional_layer_info(
    reader: &mut PsdReader,
    target: &mut LayerAdditionalInfo,
    psd: Option<&mut crate::psd::Psd>,
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
            // `Lr16`/`Lr32` recurse into a full nested layer-info block instead
            // of going to a group module, and errors from it are propagated
            // rather than swallowed (see the note on this function).
            if let (Some(psd), "Lr16" | "Lr32") = (psd, key.as_str()) {
                read_layer_info(reader, psd)?;
                skip_bytes(reader, left(reader));
                return Ok(());
            }

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

/// Mirror `readImageData`: decodes the composite image section and *returns* the
/// pixels; storing them in `canvas`/`image_data` is the caller's job.
///
/// The composite bitmap is charged against the reader's memory budget and never
/// refunded (it stays alive in the returned document).
///
/// # Errors
/// [`ReadError::ExceededMemoryLimit`] when the composite does not fit the
/// budget, and [`ReadError::StrictViolation`] for unsupported compression,
/// colour mode or bit depth combinations.
fn read_image_data(reader: &mut PsdReader, psd: &crate::psd::Psd) -> ReadResult<PixelData> {
    let compression = compression_from_u16(read_uint16(reader)?);
    let bits_per_channel = psd.bits_per_channel.unwrap_or(8.0) as u32;
    let color_mode = psd.color_mode.unwrap_or(ColorMode::Rgb);

    let width = psd.width as usize;
    let height = psd.height as usize;
    let channels_count = psd.channels.unwrap_or(0.0) as usize;

    // ZIP composites only exist for the RGB/Grayscale branch below; the other
    // colour modes reject them there, with the same wording.
    if !matches!(
        compression,
        Compression::RawData
            | Compression::RleCompressed
            | Compression::ZipWithoutPrediction
            | Compression::ZipWithPrediction
    ) {
        return Err(ReadError::StrictViolation(format!(
            "Compression type not supported: {}",
            compression_code(compression)
        )));
    }

    let mut image_data =
        create_image_data_bit_depth(width, height, bits_per_channel, 4, reader.total_memory_limit)?;
    // The composite stays alive in the document, so the budget is charged and
    // never given back.
    consume_memory(reader, image_data.data.len())?;
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
                    // One bit per pixel, rows padded to whole bytes.
                    read_bytes(reader, width.div_ceil(8) * height)?
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
                // Unreachable in practice (the guard above rejects zip), but the
                // arms stay explicit so a new Compression variant is a compile error.
                Compression::ZipWithoutPrediction | Compression::ZipWithPrediction => {
                    return Err(compression_not_supported(compression))
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
                Compression::ZipWithoutPrediction | Compression::ZipWithPrediction => {
                    // The composite section stores ZIP channels back to back
                    // with no length table, so each stream's length has to be
                    // recovered by decompressing it (`zip_stream_length`).
                    let prediction = compression == Compression::ZipWithPrediction;
                    let sample_bytes = crate::helpers::bytes_per_sample(bits_per_channel)
                        .ok_or_else(|| {
                            ReadError::StrictViolation(format!(
                                "Unsupported bit depth for ZIP channel data: {}",
                                bits_per_channel
                            ))
                        })?;
                    let expected = width
                        .checked_mul(height)
                        .and_then(|pixels| pixels.checked_mul(sample_bytes))
                        .ok_or(ReadError::SizeTooLarge)?;
                    for &c in &channels {
                        let consumed = zip_stream_length(reader, expected)?;
                        let compressed = read_bytes_slice(reader, consumed)?;
                        read_data_zip(
                            compressed,
                            Some(&mut image_data),
                            width,
                            height,
                            bits_per_channel,
                            4,
                            c,
                            prediction,
                        );
                    }
                }
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
                // Upstream leaves raw indexed data unimplemented as well, and now
                // reports it with the same wording as any other bad compression.
                Compression::RawData
                | Compression::ZipWithoutPrediction
                | Compression::ZipWithPrediction => {
                    return Err(compression_not_supported(compression))
                }
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

    Ok(image_data.into_pixel_data())
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

/// Decodes one pattern record (`readPattern`) into an RGBA8 buffer.
///
/// This is the **single** implementation of the primitive in the crate: the
/// `Patt`/`Pat2`/`Pat3` additional-info handler
/// ([`crate::additional_info::smart_object_keys`]) and the ABR `patt` section
/// ([`crate::abr`]) both call it, so the hardening below applies to every path
/// that reads a pattern out of a file.
///
/// Indexed data is resolved through the 256-entry palette that precedes the
/// virtual memory array list. Raw (`compressionMode == 0`) channels are
/// supported for all three colour modes; RLE (`compressionMode == 1`) indexed
/// channels are unsupported and produce an error, as upstream.
///
/// The pattern bitmap is charged against the reader's
/// [`crate::psd::ReadOptions::total_memory_limit`] and is *not* refunded — it is
/// returned to the caller and stays alive. A reader without a budget (any
/// [`PsdReader::new`], hence the whole ABR path) is unlimited, as upstream.
///
/// Deliberate divergence from upstream: upstream derives sizes straight from the
/// unvalidated rectangles and relies on the memory limit alone, which cannot
/// catch an inverted rectangle and, in a language with fixed-width integers,
/// cannot catch a wrapping size computation either. Here the pattern rectangle
/// and every channel rectangle go through [`check_box_size`] first, so a pattern
/// larger than the format maximum (30000, 300000 for PSB) is rejected even when
/// the budget is unlimited.
///
/// # Errors
/// - [`ReadError::InvalidBoxSize`] if the pattern or a channel rectangle is
///   inverted, oversized, or lies outside the pattern rectangle;
/// - [`ReadError::ExceededMemoryLimit`] if the bitmap does not fit the budget;
/// - [`ReadError::StrictViolation`] for an unsupported version, colour mode,
///   pixel depth or compression mode, and — only when
///   `ReadOptions::throw_for_missing_features` is set — for a raw channel that
///   does not belong to the pattern's colour mode.
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
    // The four rectangle values are raw, unvalidated file data. They are read as
    // `i64` and validated BEFORE any arithmetic derived from them, so an
    // inverted or absurd rectangle cannot underflow, wrap a `usize`
    // multiplication into a short buffer, or drive a multi-gigabyte allocation.
    let raw_top = read_uint32(reader)?;
    let raw_left = read_uint32(reader)?;
    let raw_bottom = read_uint32(reader)?;
    let raw_right = read_uint32(reader)?;
    let channels_count = read_uint32(reader)?;
    check_box_size(
        "pattern",
        f64::from(raw_top),
        f64::from(raw_left),
        f64::from(raw_bottom),
        f64::from(raw_right),
        reader.large,
    )?;
    let (top, left) = (i64::from(raw_top), i64::from(raw_left));
    let (bottom, right) = (i64::from(raw_bottom), i64::from(raw_right));
    let (width, height) = box_extents("pattern", top, left, bottom, right)?;
    let size = width.saturating_mul(height).saturating_mul(4);
    // The pattern bitmap is returned to the caller and stays alive, so the
    // budget is charged without a matching recovery.
    consume_memory(reader, size)?;
    let mut data = vec![0u8; size];
    let mut i = 3;
    while i < data.len() {
        data[i] = 255;
        i += 4;
    }

    let mut ch = 0usize;
    // `channels_count` is unvalidated file data; saturate so the loop bound
    // cannot overflow. A bogus count simply runs into the end of the buffer.
    for _ in 0..channels_count.saturating_add(2) {
        let has = read_uint32(reader)?;
        if has == 0 {
            continue;
        }
        let length = read_uint32(reader)? as usize;
        let pixel_depth = read_uint32(reader)?;
        let raw_ctop = read_uint32(reader)?;
        let raw_cleft = read_uint32(reader)?;
        let raw_cbottom = read_uint32(reader)?;
        let raw_cright = read_uint32(reader)?;
        let pixel_depth2 = read_uint16(reader)?;
        let compression_mode = read_uint8(reader)?;
        let data_length = length.saturating_sub(4 + 16 + 2 + 1);
        let cdata = read_bytes(reader, data_length)?;

        // A pattern channel repeats its depth in both a u32 and a u16 field;
        // they must agree, and only the three RGB depths are decodable.
        if u32::from(pixel_depth2) != pixel_depth {
            return Err(ReadError::StrictViolation(format!(
                "Pattern channel depth mismatch: {} != {}",
                pixel_depth, pixel_depth2
            )));
        }
        let channel_sample_bytes =
            crate::helpers::bytes_per_sample(pixel_depth).ok_or_else(|| {
                ReadError::StrictViolation(format!(
                    "Unsupported pixel depth for patterns: {}",
                    pixel_depth
                ))
            })?;
        // A palette index is a byte, so an indexed pattern is 8-bit by
        // definition; anything else would index the palette with half a sample.
        if color_mode == ColorMode::Indexed && pixel_depth != 8 {
            return Err(ReadError::StrictViolation(format!(
                "Indexed patterns require 8-bit channels, got {}",
                pixel_depth
            )));
        }

        // Same treatment as the pattern rectangle: validate first, then derive
        // sizes. The channel must additionally start inside the pattern, or the
        // `ox`/`oy` offsets below would be negative.
        check_box_size(
            "patternChannel",
            f64::from(raw_ctop),
            f64::from(raw_cleft),
            f64::from(raw_cbottom),
            f64::from(raw_cright),
            reader.large,
        )?;
        let (ctop, cleft) = (i64::from(raw_ctop), i64::from(raw_cleft));
        let (cbottom, cright) = (i64::from(raw_cbottom), i64::from(raw_cright));
        let (w, h) = box_extents("patternChannel", ctop, cleft, cbottom, cright)?;
        let (ox, oy) = box_extents("patternChannelOffset", top, left, ctop, cleft)?;

        // Upstream chains these as `else if`: without it, a channel that matched
        // the RGB branch fell through into the grayscale/indexed checks.
        if compression_mode == 0 {
            if color_mode == ColorMode::Rgb && ch < 3 {
                for yy in 0..h {
                    for xx in 0..w {
                        // Samples are `channel_sample_bytes` wide and
                        // big-endian; `sample_to_u8` narrows them to RGBA8.
                        let src = (xx + yy * w) * channel_sample_bytes;
                        let dst = (ox + xx + (yy + oy) * width) * 4;
                        let Some(sample) = cdata.get(src..src + channel_sample_bytes) else {
                            continue;
                        };
                        if dst + ch < data.len() {
                            data[dst + ch] = sample_to_u8(sample, pixel_depth);
                        }
                    }
                }
            } else if color_mode == ColorMode::Grayscale && ch < 1 {
                for yy in 0..h {
                    for xx in 0..w {
                        let src = (xx + yy * w) * channel_sample_bytes;
                        let dst = (ox + xx + (yy + oy) * width) * 4;
                        let Some(sample) = cdata.get(src..src + channel_sample_bytes) else {
                            continue;
                        };
                        if dst + 2 < data.len() {
                            let value = sample_to_u8(sample, pixel_depth);
                            data[dst] = value;
                            data[dst + 1] = value;
                            data[dst + 2] = value;
                        }
                    }
                }
            } else if color_mode == ColorMode::Indexed {
                // Uncompressed indexed data is one palette index per pixel.
                for yy in 0..h {
                    for xx in 0..w {
                        let src = xx + yy * w;
                        let dst = (ox + xx + (yy + oy) * width) * 4;
                        if dst + 2 >= data.len() || src >= cdata.len() {
                            continue;
                        }
                        if let Some(color) = palette.get(cdata[src] as usize) {
                            data[dst] = color.r as u8;
                            data[dst + 1] = color.g as u8;
                            data[dst + 2] = color.b as u8;
                        }
                    }
                }
            } else if reader.options.throw_for_missing_features == Some(true) {
                return Err(ReadError::StrictViolation("Invalid color pattern".to_string()));
            }
        } else if compression_mode == 1 {
            // The temporary single-channel buffer is scratch: charged here and
            // given back when this branch ends, error path included.
            let temp_size = w.saturating_mul(h);
            with_scratch_memory(reader, temp_size, |_reader| {
                let mut temp =
                    DecodeTarget { width: w, height: h, data: vec![0u8; temp_size], channels: 1 };
                // The channel bytes were already read into `cdata`; this
                // sub-reader has no budget of its own, exactly as upstream's
                // `createReader` over the channel buffer.
                let mut cdata_reader = PsdReader::new(&cdata, None, None);
                if color_mode == ColorMode::Rgb && ch < 3 {
                    read_data_rle(&mut cdata_reader, Some(&mut temp), w, h, pixel_depth, 1, &[0], false)?;
                    copy_channel_to_rgba(&temp, &mut data, width, ox, oy, ch);
                }
                if color_mode == ColorMode::Grayscale && ch < 1 {
                    read_data_rle(&mut cdata_reader, Some(&mut temp), w, h, pixel_depth, 1, &[0], false)?;
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
                Ok(())
            })?;
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
    use crate::psd::DEFAULT_TOTAL_MEMORY_LIMIT;

    /// Pins the clamping contract of the 32-bit float sample conversion,
    /// including the `NaN -> 0` case that `f32::clamp` would not reproduce.
    #[test]
    fn f32_samples_clamp_and_map_nan_to_zero() {
        assert_eq!(f32_sample_to_u8(0.0), 0);
        assert_eq!(f32_sample_to_u8(1.0), 255);
        assert_eq!(f32_sample_to_u8(0.5), 128); // 127.5 rounds half away from zero
        assert_eq!(f32_sample_to_u8(-3.0), 0);
        assert_eq!(f32_sample_to_u8(7.5), 255);
        assert_eq!(f32_sample_to_u8(f32::NEG_INFINITY), 0);
        assert_eq!(f32_sample_to_u8(f32::INFINITY), 255);
        assert_eq!(f32_sample_to_u8(f32::NAN), 0);
    }

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
    fn invalid_signature_message_escapes_control_bytes() {
        // Four zero bytes are the common case (truncated or padded file); the
        // message must stay printable instead of embedding raw NULs.
        let buf = [0u8; 4];
        let mut r = PsdReader::new(&buf, None, None);
        let err = check_signature(&mut r, "8BIM", None).unwrap_err();
        let message = err.to_string();
        assert_eq!(message, "Invalid signature: '\\0\\0\\0\\0' at 0x0");
        assert!(
            !message.chars().any(|c| c.is_control()),
            "the message must not contain control characters: {message:?}"
        );

        // A printable signature is still shown verbatim.
        let buf = *b"8BIM";
        let mut r = PsdReader::new(&buf, None, None);
        let err = check_signature(&mut r, "8BPS", None).unwrap_err();
        assert_eq!(err.to_string(), "Invalid signature: '8BIM' at 0x0");
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

    /// Path of the upstream read fixture `rel` (`test/read/<rel>/src.psd`).
    ///
    /// The fixture tree lives outside the crate directory and is therefore
    /// absent from the published package, so callers must treat a missing file
    /// as "skip", not "fail".
    fn fixture_path(rel: &str) -> std::path::PathBuf {
        let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let local = manifest_dir
            .join("test")
            .join("ag-psd")
            .join("test")
            .join("read")
            .join(rel)
            .join("src.psd");
        if local.is_file() {
            local
        } else {
            manifest_dir
                .join("../../test/ag-psd/test/read")
                .join(rel)
                .join("src.psd")
        }
    }

    /// Bytes of the upstream read fixture `rel`, or `None` when the fixture
    /// tree is not checked out. A fixture that exists but cannot be read still
    /// panics — only absence is tolerated.
    fn fixture_bytes(rel: &str) -> Option<Vec<u8>> {
        let path = fixture_path(rel);
        if !path.exists() {
            eprintln!("fixture {} not present, skipping", path.display());
            return None;
        }
        Some(
            std::fs::read(&path)
                .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e)),
        )
    }

    /// Parsed fixture `rel`, or `None` when the fixture tree is not checked
    /// out. A fixture that exists but fails to parse still panics.
    fn read_fixture(rel: &str) -> Option<crate::psd::Psd> {
        let bytes = fixture_bytes(rel)?;
        let opts = ReadOptions::default();
        Some(read_psd(&bytes, &opts).unwrap_or_else(|e| panic!("read_psd {}: {:?}", rel, e)))
    }

    fn count_layers(layers: &[Layer]) -> usize {
        layers.iter().map(|l| 1 + l.children.as_ref().map_or(0, |c| count_layers(c))).sum()
    }

    fn any_layer_has_pixels(layers: &[Layer]) -> bool {
        layers.iter().any(|l| {
            let has = l
                .canvas
                .as_ref()
                .is_some_and(|c| !c.data.is_empty())
                || l.image_data.as_ref().is_some_and(|c| !c.data.is_empty());
            has || l.children.as_ref().is_some_and(|c| any_layer_has_pixels(c))
        })
    }

    #[test]
    fn read_fixture_layers_rgb8() {
        let Some(psd) = read_fixture("layers") else { return };
        assert_eq!(psd.width, 300.0);
        assert_eq!(psd.height, 200.0);
        assert_eq!(psd.color_mode, Some(ColorMode::Rgb));
        assert_eq!(psd.bits_per_channel, Some(8.0));
        let children = psd.children.as_ref().expect("children");
        assert_eq!(children.len(), 3, "top-level children count");
        assert!(any_layer_has_pixels(children), "at least one layer has pixel data");
        // composite image should be present (not skipped)
        assert!(psd.canvas.as_ref().is_some_and(|c| !c.data.is_empty()));
    }

    #[test]
    fn read_fixture_groups_nesting() {
        let Some(psd) = read_fixture("groups") else { return };
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
        let Some(psd) = read_fixture("just-bg") else { return };
        assert_eq!(psd.width, 100.0);
        assert_eq!(psd.height, 100.0);
        assert_eq!(psd.color_mode, Some(ColorMode::Rgb));
        let count = psd.children.as_ref().map_or(0, |c| c.len());
        assert_eq!(count, 0, "background-only document has no layer children");
        assert!(psd.canvas.as_ref().is_some_and(|c| !c.data.is_empty()));
    }

    // -----------------------------------------------------------------------
    // High bit depth: layers live in the recursive `Lr16` / `Lr32` sections.
    //
    // Photoshop leaves the ordinary `Layr` section empty for 16/32-bit files
    // and puts the layer records into an `Lr16`/`Lr32` additional-info section
    // instead. Before that section was parsed these documents read back with
    // `children == None` and every layer silently gone; both assertions on the
    // layer tree below fail on that behaviour.
    // -----------------------------------------------------------------------

    #[test]
    fn read_fixture_16bits_layers_come_from_lr16_section() {
        let Some(psd) = read_fixture("16bits") else { return };
        assert_eq!(psd.width, 500.0);
        assert_eq!(psd.height, 500.0);
        assert_eq!(psd.bits_per_channel, Some(16.0));
        let children = psd.children.as_ref().expect("children (Lr16 layer info)");
        assert_eq!(children.len(), 1, "16-bit document has one layer");
        let layer = &children[0];
        assert_eq!(layer.additional_info.name.as_deref(), Some("Layer 1"));
        // Bounds come from the layer record inside the nested section.
        assert_eq!(
            (layer.top, layer.left, layer.bottom, layer.right),
            (Some(-34.0), Some(-36.0), Some(557.0), Some(533.0))
        );
        // The 16-bit channel data of that layer decodes as well.
        assert!(any_layer_has_pixels(children), "16-bit layer pixels decoded");
    }

    #[test]
    fn read_fixture_32bits_layers_come_from_lr32_section() {
        let Some(psd) = read_fixture("32bits") else { return };
        assert_eq!(psd.width, 300.0);
        assert_eq!(psd.height, 300.0);
        assert_eq!(psd.bits_per_channel, Some(32.0));
        let children = psd.children.as_ref().expect("children (Lr32 layer info)");
        assert_eq!(children.len(), 2, "32-bit document has two layers");
        assert_eq!(children[0].additional_info.name.as_deref(), Some("Layer 1"));
        assert_eq!(children[1].additional_info.name.as_deref(), Some("Layer 0"));
        assert!(any_layer_has_pixels(children), "32-bit layer pixels decoded");
    }

    #[test]
    fn lr16_layers_are_charged_against_the_memory_budget() {
        // The nested section must not be a hole in the budget. The budget below
        // is deliberately picked between the two bitmaps of the fixture: the
        // 500x500 composite needs 500*500*4*2 = 2_000_000 bytes and fits, the
        // single 569x591 16-bit layer needs 2_690_232 and does not. The
        // composite is skipped once the layer tree is non-empty, so the only
        // way to reach the overrun is by reading the layer out of `Lr16`.
        let Some(bytes) = fixture_bytes("16bits") else { return };
        let opts = ReadOptions {
            total_memory_limit: Some(2_500_000),
            skip_composite_image_data: Some(true),
            ..ReadOptions::default()
        };
        let err = read_psd(&bytes, &opts).unwrap_err();
        assert!(
            matches!(err, ReadError::ExceededMemoryLimit { .. }),
            "expected a budget overrun, got {:?}",
            err
        );
    }

    // -----------------------------------------------------------------------
    // Color mode reporting (upstream `colorModes` table).
    // -----------------------------------------------------------------------

    #[test]
    fn color_mode_names_are_not_shifted() {
        // Codes 5 and 6 are unassigned; without the two empty slots upstream
        // reported multichannel/duotone/lab two positions off.
        assert_eq!(color_mode_name(4), Some("CMYK"));
        assert_eq!(color_mode_name(5), None);
        assert_eq!(color_mode_name(6), None);
        assert_eq!(color_mode_name(7), Some("multichannel"));
        assert_eq!(color_mode_name(8), Some("duotone"));
        assert_eq!(color_mode_name(9), Some("lab"));
    }

    /// Minimal 26-byte PSD header with the given color mode code.
    fn header_with_color_mode(color_mode: u16) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"8BPS");
        buf.extend_from_slice(&1u16.to_be_bytes()); // version
        buf.extend_from_slice(&[0u8; 6]); // reserved
        buf.extend_from_slice(&3u16.to_be_bytes()); // channels
        buf.extend_from_slice(&10u32.to_be_bytes()); // height
        buf.extend_from_slice(&10u32.to_be_bytes()); // width
        buf.extend_from_slice(&8u16.to_be_bytes()); // bits per channel
        buf.extend_from_slice(&color_mode.to_be_bytes());
        buf
    }

    #[test]
    fn unsupported_color_mode_is_reported_by_name() {
        let buf = header_with_color_mode(7);
        let err = read_psd(&buf, &ReadOptions::default()).unwrap_err();
        assert_eq!(
            err,
            ReadError::StrictViolation("Color mode not supported: multichannel".to_string())
        );

        // Unassigned code falls back to the number.
        let buf = header_with_color_mode(6);
        let err = read_psd(&buf, &ReadOptions::default()).unwrap_err();
        assert_eq!(
            err,
            ReadError::StrictViolation("Color mode not supported: 6".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Box size validation (upstream `isValidBoxSize`).
    // -----------------------------------------------------------------------

    fn box_bytes(top: i32, left: i32, bottom: i32, right: i32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&top.to_be_bytes());
        buf.extend_from_slice(&left.to_be_bytes());
        buf.extend_from_slice(&bottom.to_be_bytes());
        buf.extend_from_slice(&right.to_be_bytes());
        buf
    }

    #[test]
    fn layer_record_rejects_oversized_rectangle() {
        let buf = box_bytes(0, 0, 100, 40000);
        let mut r = PsdReader::new(&buf, None, None);
        let mut psd = crate::psd::Psd::default();
        let err = read_layer_record(&mut r, &mut psd).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidBoxSize { kind: "layer", width: 40000, height: 100 }
        );
    }

    #[test]
    fn layer_record_rejects_inverted_rectangle() {
        let buf = box_bytes(0, 100, 100, 0);
        let mut r = PsdReader::new(&buf, None, None);
        let mut psd = crate::psd::Psd::default();
        let err = read_layer_record(&mut r, &mut psd).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidBoxSize { kind: "layer", width: -100, height: 100 }
        );
    }

    #[test]
    fn layer_record_allows_psb_sized_rectangle_when_large() {
        let buf = box_bytes(0, 0, 100, 40000);
        let mut r = PsdReader::new(&buf, None, None);
        r.large = true;
        let mut psd = crate::psd::Psd::default();
        // 40000 is within the PSB limit, so the record must fail later (running
        // out of bytes) rather than on the rectangle.
        let err = read_layer_record(&mut r, &mut psd).unwrap_err();
        assert_eq!(err, ReadError::UnexpectedEndOfBuffer);
    }

    /// Wraps mask-section content into the `readSection` length prefix.
    fn mask_section(content: Vec<u8>) -> Vec<u8> {
        let mut buf = (content.len() as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(&content);
        buf
    }

    #[test]
    fn mask_data_rejects_oversized_rectangle() {
        let mut content = box_bytes(0, 0, 40000, 10);
        content.push(0); // default color
        content.push(0); // flags
        let buf = mask_section(content);
        let mut r = PsdReader::new(&buf, None, None);
        let mut info = LayerAdditionalInfo::default();
        let err = read_layer_mask_data(&mut r, &mut info).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidBoxSize { kind: "mask", width: 10, height: 40000 }
        );
    }

    #[test]
    fn real_mask_data_rejects_oversized_rectangle() {
        let mut content = box_bytes(0, 0, 10, 10);
        content.push(0); // mask default color
        content.push(0); // mask flags
        content.push(0); // real mask flags
        content.push(0); // real mask default color
        content.extend_from_slice(&box_bytes(0, 0, 10, 40000));
        let buf = mask_section(content);
        let mut r = PsdReader::new(&buf, None, None);
        let mut info = LayerAdditionalInfo::default();
        let err = read_layer_mask_data(&mut r, &mut info).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidBoxSize { kind: "realMask", width: 40000, height: 10 }
        );
    }

    // -----------------------------------------------------------------------
    // Memory budget (upstream `consumeMemory` / `recoverMemory`).
    // -----------------------------------------------------------------------

    #[test]
    fn default_read_options_carry_2gib_budget() {
        assert_eq!(
            ReadOptions::default().total_memory_limit,
            Some(2 * 1024 * 1024 * 1024)
        );
    }

    #[test]
    fn consume_and_recover_memory_track_the_budget() {
        let buf = [0u8; 4];
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(100);

        consume_memory(&mut r, 60).unwrap();
        assert_eq!(r.total_memory_limit, Some(40));

        let err = consume_memory(&mut r, 41).unwrap_err();
        assert_eq!(err, ReadError::ExceededMemoryLimit { requested: 41, available: 40 });
        assert_eq!(r.total_memory_limit, Some(40), "a failed charge changes nothing");

        recover_memory(&mut r, 60);
        assert_eq!(r.total_memory_limit, Some(100));

        // `None` means unlimited: neither call has any effect.
        r.total_memory_limit = None;
        consume_memory(&mut r, usize::MAX).unwrap();
        recover_memory(&mut r, usize::MAX);
        assert_eq!(r.total_memory_limit, None);
    }

    #[test]
    fn create_image_data_bit_depth_checks_the_budget() {
        // 10x10 RGBA at 8 bit = 400 bytes; at 16 bit upstream counts 800.
        assert!(create_image_data_bit_depth(10, 10, 8, 4, Some(400)).is_ok());
        let err = create_image_data_bit_depth(10, 10, 8, 4, Some(399)).unwrap_err();
        assert_eq!(err, ReadError::ExceededMemoryLimit { requested: 400, available: 399 });
        let err = create_image_data_bit_depth(10, 10, 16, 4, Some(400)).unwrap_err();
        assert_eq!(err, ReadError::ExceededMemoryLimit { requested: 800, available: 400 });
        // No limit: any size is allowed.
        assert!(create_image_data_bit_depth(10, 10, 32, 4, None).is_ok());
    }

    #[test]
    fn rle_line_length_table_is_charged_and_recovered() {
        // One offset, one row: the table is a one-entry `Vec<u32>` = 4 bytes
        // (upstream would charge 2 for its `Uint16Array`; we charge what we
        // really allocate), then a 2-pixel literal run.
        let buf = vec![
            0x00, 0x03, // row byte count = 3
            0x01, 0xAA, 0xBB, // PackBits: copy 2 literal bytes
        ];
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(1000);
        let mut target = DecodeTarget::rgba(2, 1);
        read_data_rle(&mut r, Some(&mut target), 2, 1, 8, 4, &[0], false).unwrap();
        assert_eq!(target.data[0], 0xAA);
        assert_eq!(target.data[4], 0xBB);
        assert_eq!(r.total_memory_limit, Some(1000), "scratch table is given back");

        // The same read fails when the table alone does not fit.
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(1);
        let mut target = DecodeTarget::rgba(2, 1);
        let err = read_data_rle(&mut r, Some(&mut target), 2, 1, 8, 4, &[0], false).unwrap_err();
        assert_eq!(err, ReadError::ExceededMemoryLimit { requested: 4, available: 1 });
    }

    #[test]
    fn rle_line_length_table_charge_matches_the_real_allocation() {
        // Four rows, one offset: the `Vec<u32>` table is 16 bytes. Upstream
        // charges its `Uint16Array` size (8) for a non-PSB file, which would
        // let a table twice the size of the budget through; the charge here is
        // the real allocation, so 8 bytes of budget are not enough.
        let buf = vec![0u8; 64];
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(8);
        let err = read_data_rle(&mut r, None, 2, 4, 8, 4, &[0], false).unwrap_err();
        assert_eq!(err, ReadError::ExceededMemoryLimit { requested: 16, available: 8 });

        // 16 bytes are exactly enough, and they come back afterwards.
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(16);
        read_data_rle(&mut r, None, 2, 4, 8, 4, &[0], false).unwrap();
        assert_eq!(r.total_memory_limit, Some(16));
    }

    #[test]
    fn rle_budget_is_restored_when_the_read_fails() {
        // The table needs two entries but the buffer holds only one, so the
        // read fails between the charge and the refund.
        let buf = vec![0x00, 0x03];
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(1000);
        let err = read_data_rle(&mut r, None, 2, 2, 8, 4, &[0], false).unwrap_err();
        assert_eq!(err, ReadError::UnexpectedEndOfBuffer);
        assert_eq!(
            r.total_memory_limit,
            Some(1000),
            "a failed read must not permanently shrink the budget"
        );

        // A later successful read therefore still sees the full budget.
        let good = vec![0x00, 0x03, 0x01, 0xAA, 0xBB];
        let mut r2 = PsdReader::new(&good, None, None);
        r2.total_memory_limit = r.total_memory_limit;
        let mut target = DecodeTarget::rgba(2, 1);
        read_data_rle(&mut r2, Some(&mut target), 2, 1, 8, 4, &[0], false).unwrap();
        assert_eq!(r2.total_memory_limit, Some(1000));
    }

    /// Compresses `data` with zlib framing (what this crate's writer emits).
    fn zlib_wrap(data: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut e =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// Compresses `data` as a bare DEFLATE stream (what some third-party PSD
    /// writers emit).
    fn deflate_raw(data: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut e =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    #[test]
    fn packbits_rows_cannot_amplify_past_the_declared_row() {
        // Maximum PackBits amplification: every two bytes (`0x81`, value)
        // decode to 128 identical bytes — 64x. One MiB of these expands to
        // 64 MiB, for a bitmap that declares four pixels. An unbounded decoder
        // allocates all of it outside `ReadOptions::total_memory_limit`; a
        // bounded one stops at the row.
        const HOSTILE_BYTES: usize = 1 << 20;
        let hostile: Vec<u8> = std::iter::repeat_n([0x81u8, 0xAB], HOSTILE_BYTES / 2)
            .flatten()
            .collect();

        // A 4-pixel, 8-bit row is 4 bytes.
        const ROW_BYTES: usize = 4;
        let decoded = decode_packbits_row(&hostile, ROW_BYTES);
        assert_eq!(decoded, vec![0xAB; ROW_BYTES]);
        assert!(
            decoded.capacity() < 1024,
            "the decode buffer was reserved for the expansion, not the row: {} bytes",
            decoded.capacity()
        );

        // The same row through the public reader: PSB row lengths are 4 bytes
        // wide, which is what lets a single row declare a megabyte.
        let mut buf = u32::try_from(HOSTILE_BYTES).unwrap().to_be_bytes().to_vec();
        buf.extend_from_slice(&hostile);
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(64 * 1024);
        let mut target = DecodeTarget::rgba(4, 1);
        read_data_rle(&mut r, Some(&mut target), 4, 1, 8, 4, &[0], true).unwrap();
        // Only channel 0 was decoded; `DecodeTarget::rgba` starts zeroed.
        assert_eq!(target.data, vec![0xAB, 0, 0, 0, 0xAB, 0, 0, 0, 0xAB, 0, 0, 0, 0xAB, 0, 0, 0]);
        assert_eq!(
            r.total_memory_limit,
            Some(64 * 1024),
            "the row-length scratch must be refunded, and nothing else charged"
        );
    }

    #[test]
    fn zip_channels_are_read_under_both_framings() {
        // Two 8-bit pixels in one channel.
        let channel = [0x10u8, 0xF0];
        assert!(has_zlib_header(&zlib_wrap(&channel)));
        assert!(!has_zlib_header(&deflate_raw(&channel)));

        for compressed in [zlib_wrap(&channel), deflate_raw(&channel)] {
            let mut target = DecodeTarget::rgba(2, 1);
            read_data_zip(&compressed, Some(&mut target), 2, 1, 8, 4, 0, false);
            assert_eq!(target.data[0], 0x10);
            assert_eq!(target.data[4], 0xF0);
        }

        // 32-bit float samples, unpredicted, go through the same door.
        let floats: Vec<u8> =
            [0.0f32, 1.0].into_iter().flat_map(f32::to_be_bytes).collect();
        for compressed in [zlib_wrap(&floats), deflate_raw(&floats)] {
            let mut target = DecodeTarget::rgba(2, 1);
            read_data_zip(&compressed, Some(&mut target), 2, 1, 32, 4, 0, false);
            assert_eq!((target.data[0], target.data[4]), (0, 255));
        }

        // Data that is neither framing is dropped, not panicked on.
        let mut target = DecodeTarget::rgba(2, 1);
        read_data_zip(&[0xDE, 0xAD, 0xBE, 0xEF], Some(&mut target), 2, 1, 8, 4, 0, false);
        assert_eq!(target.data[0], 0);
    }

    #[test]
    fn zip_stream_length_accepts_both_framings_and_charges_the_budget() {
        let channel = [7u8; 64];
        for compressed in [zlib_wrap(&channel), deflate_raw(&channel)] {
            // A trailing byte stands in for the next channel: the length must
            // stop at the end of this stream.
            let mut buf = compressed.clone();
            buf.push(0xAB);
            let mut r = PsdReader::new(&buf, None, None);
            r.total_memory_limit = Some(1000);
            assert_eq!(zip_stream_length(&mut r, channel.len()).unwrap(), compressed.len());
            assert_eq!(r.offset, 0, "the cursor must not move");
            assert_eq!(r.total_memory_limit, Some(1000), "scratch memory must be refunded");
        }

        // The scratch buffer is charged, so an insufficient budget is an error
        // rather than an unaccounted allocation — and the budget survives it.
        let compressed = zlib_wrap(&channel);
        let mut r = PsdReader::new(&compressed, None, None);
        r.total_memory_limit = Some(8);
        assert_eq!(
            zip_stream_length(&mut r, channel.len()).unwrap_err(),
            ReadError::ExceededMemoryLimit { requested: 64, available: 8 }
        );
        assert_eq!(r.total_memory_limit, Some(8));

        // Undecodable data is reported, and the budget is still refunded.
        let junk = [0xDEu8, 0xAD, 0xBE, 0xEF];
        let mut r = PsdReader::new(&junk, None, None);
        r.total_memory_limit = Some(1000);
        assert!(matches!(
            zip_stream_length(&mut r, 64).unwrap_err(),
            ReadError::StrictViolation(_)
        ));
        assert_eq!(r.total_memory_limit, Some(1000));
    }

    #[test]
    fn composite_zip_channels_are_read_back() {
        use crate::psd::{ColorMode, PixelData, Psd, WriteOptions};
        use crate::writer::write_psd;

        // Write an ordinary 8-bit document, then replace its composite section
        // with the ZIP form. Photoshop never emits one (upstream
        // `psdWriter.ts:314` says so and this crate follows), but third-party
        // writers do, and the reader must not reject those files.
        let pixels = PixelData {
            width: 2,
            height: 1,
            data: vec![0x11, 0x22, 0x33, 255, 0x44, 0x55, 0x66, 255],
        };
        let psd = Psd {
            width: 2.0,
            height: 1.0,
            color_mode: Some(ColorMode::Rgb),
            image_data: Some(pixels.clone()),
            ..Default::default()
        };
        let bytes = write_psd(&psd, &WriteOptions::default());

        // Header, then three length-prefixed sections; the composite follows.
        let section_len = |at: usize, buf: &[u8]| -> usize {
            u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]) as usize
        };
        let mut at = 26usize;
        for _ in 0..3 {
            at += 4 + section_len(at, &bytes);
        }

        for wrap in [zlib_wrap as fn(&[u8]) -> Vec<u8>, deflate_raw] {
            let mut patched = bytes[..at].to_vec();
            patched.extend_from_slice(&(Compression::ZipWithoutPrediction as u16).to_be_bytes());
            for channel in 0..3usize {
                let plane: Vec<u8> =
                    (0..2usize).map(|x| pixels.data[x * 4 + channel]).collect();
                patched.extend_from_slice(&wrap(&plane));
            }

            let again = read_psd(
                &patched,
                &ReadOptions { use_image_data: Some(true), ..Default::default() },
            )
            .expect("composite ZIP channels must be readable");
            assert_eq!(again.image_data.as_ref().expect("composite").data, pixels.data);
        }
    }

    #[test]
    fn read_pattern_charges_its_bitmap_against_the_budget() {
        use crate::writer::{create_writer_default, get_writer_buffer, write_pattern};

        let pattern = PatternInfo {
            name: "test".to_string(),
            id: "deadbeef-0000-0000-0000-000000000000".to_string(),
            x: 0.0,
            y: 0.0,
            bounds: PatternBounds { x: 0.0, y: 0.0, w: 2.0, h: 2.0 },
            data: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255],
        };
        let mut writer = create_writer_default();
        write_pattern(&mut writer, &pattern);
        let buf = get_writer_buffer(&writer);

        // 2x2 RGBA pattern = 16 bytes; any scratch buffer is given back.
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(1000);
        read_pattern(&mut r).unwrap();
        assert_eq!(r.total_memory_limit, Some(1000 - 16));

        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(8);
        let err = read_pattern(&mut r).unwrap_err();
        assert_eq!(err, ReadError::ExceededMemoryLimit { requested: 16, available: 8 });
    }

    /// Builds a pattern record up to and including the virtual-memory-array-list
    /// rectangle, with no channels. Port of the buffer used by upstream's
    /// "rejects a pattern with huge dimensions" regression test.
    fn pattern_record_with_box(top: u32, left: u32, bottom: u32, right: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&100u32.to_be_bytes()); // record length (never reached)
        buf.extend_from_slice(&1u32.to_be_bytes()); // version
        buf.extend_from_slice(&(ColorMode::Rgb as u32).to_be_bytes());
        buf.extend_from_slice(&0i16.to_be_bytes()); // x
        buf.extend_from_slice(&0i16.to_be_bytes()); // y
        buf.extend_from_slice(&0u32.to_be_bytes()); // unicode name length
        buf.push(0); // pascal string id length
        buf.extend_from_slice(&3u32.to_be_bytes()); // VMAL version
        buf.extend_from_slice(&0u32.to_be_bytes()); // VMAL length (unused)
        buf.extend_from_slice(&top.to_be_bytes());
        buf.extend_from_slice(&left.to_be_bytes());
        buf.extend_from_slice(&bottom.to_be_bytes());
        buf.extend_from_slice(&right.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes()); // channels count
        buf
    }

    /// Upstream's "rejects a pattern with huge dimensions": the rectangle is
    /// unvalidated file data, so `bottom`/`right` of `0xffffffff` used to size a
    /// `width * height * 4` allocation. Here the rectangle is rejected before any
    /// arithmetic derived from it, so there is neither a wrapping multiplication
    /// nor a multi-gigabyte allocation.
    #[test]
    fn read_pattern_rejects_a_huge_rectangle() {
        let buf = pattern_record_with_box(0, 0, 0xffff_ffff, 0xffff_ffff);
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(DEFAULT_TOTAL_MEMORY_LIMIT);
        let err = read_pattern(&mut r).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidBoxSize { kind: "pattern", width: 0xffff_ffff, height: 0xffff_ffff }
        );
        // A reader without a budget must reject it just the same: the guard is
        // the rectangle check, not the (optional) memory limit.
        let mut unlimited = PsdReader::new(&buf, None, None);
        assert!(read_pattern(&mut unlimited).is_err());
    }

    #[test]
    fn read_pattern_rejects_an_inverted_rectangle() {
        // right < left would underflow the width computation.
        let buf = pattern_record_with_box(0, 100, 10, 0);
        let mut r = PsdReader::new(&buf, None, None);
        let err = read_pattern(&mut r).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidBoxSize { kind: "pattern", width: -100, height: 10 }
        );
    }

    #[test]
    fn read_pattern_rejects_a_rectangle_that_exceeds_the_budget() {
        // 30000x30000 is a valid rectangle, but its RGBA bitmap is 3.6 GB.
        let buf = pattern_record_with_box(0, 0, 30000, 30000);
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(DEFAULT_TOTAL_MEMORY_LIMIT);
        let err = read_pattern(&mut r).unwrap_err();
        assert_eq!(
            err,
            ReadError::ExceededMemoryLimit {
                requested: 30000 * 30000 * 4,
                available: DEFAULT_TOTAL_MEMORY_LIMIT,
            }
        );
    }

    /// A grayscale pattern with a single RLE channel whose compressed payload is
    /// `cdata`, used to drive the scratch-buffer path of `read_pattern`.
    fn grayscale_rle_pattern_record(w: u32, h: u32, cdata: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u32.to_be_bytes()); // version
        body.extend_from_slice(&(ColorMode::Grayscale as u32).to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes()); // x
        body.extend_from_slice(&0i16.to_be_bytes()); // y
        body.extend_from_slice(&0u32.to_be_bytes()); // unicode name length
        body.push(0); // pascal string id length
        body.extend_from_slice(&3u32.to_be_bytes()); // VMAL version
        body.extend_from_slice(&0u32.to_be_bytes()); // VMAL length (unused)
        body.extend_from_slice(&0u32.to_be_bytes()); // top
        body.extend_from_slice(&0u32.to_be_bytes()); // left
        body.extend_from_slice(&h.to_be_bytes()); // bottom
        body.extend_from_slice(&w.to_be_bytes()); // right
        body.extend_from_slice(&1u32.to_be_bytes()); // channels count

        body.extend_from_slice(&1u32.to_be_bytes()); // has
        let clen = u32::try_from(cdata.len() + 4 + 16 + 2 + 1).expect("test payload fits u32");
        body.extend_from_slice(&clen.to_be_bytes());
        body.extend_from_slice(&8u32.to_be_bytes()); // pixel depth
        body.extend_from_slice(&0u32.to_be_bytes()); // ctop
        body.extend_from_slice(&0u32.to_be_bytes()); // cleft
        body.extend_from_slice(&h.to_be_bytes()); // cbottom
        body.extend_from_slice(&w.to_be_bytes()); // cright
        body.extend_from_slice(&8u16.to_be_bytes()); // pixel depth 2
        body.push(1); // compression mode: RLE
        body.extend_from_slice(cdata);
        body.extend_from_slice(&0u32.to_be_bytes()); // absent slot
        body.extend_from_slice(&0u32.to_be_bytes()); // absent slot
        while body.len() % 4 != 0 {
            body.push(0);
        }

        let mut out = u32::try_from(body.len()).expect("test record fits u32").to_be_bytes().to_vec();
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn read_pattern_restores_the_scratch_budget_when_a_channel_fails() {
        // The channel declares RLE compression but carries no payload, so the
        // row-length table read fails after the scratch buffer was charged.
        let buf = grayscale_rle_pattern_record(2, 2, &[]);
        let mut r = PsdReader::new(&buf, None, None);
        r.total_memory_limit = Some(1000);
        let err = read_pattern(&mut r).unwrap_err();
        assert_eq!(err, ReadError::UnexpectedEndOfBuffer);
        // Only the 2x2 RGBA bitmap (16 bytes) stays charged; the w*h scratch
        // buffer is given back even though the channel read failed.
        assert_eq!(r.total_memory_limit, Some(1000 - 16));
    }

    #[test]
    fn memory_limit_rejects_composite_and_none_disables_it() {
        let Some(bytes) = fixture_bytes("just-bg") else { return };

        // 100x100 RGBA composite needs 40000 bytes.
        let opts = ReadOptions { total_memory_limit: Some(1024), ..Default::default() };
        let err = read_psd(&bytes, &opts).unwrap_err();
        assert_eq!(
            err,
            ReadError::ExceededMemoryLimit { requested: 40000, available: 1024 }
        );

        let opts = ReadOptions { total_memory_limit: None, ..Default::default() };
        assert!(read_psd(&bytes, &opts).is_ok(), "None disables the limit");

        // The 2GB default is not in the way of a normal file.
        assert!(read_psd(&bytes, &ReadOptions::default()).is_ok());
    }

    // -----------------------------------------------------------------------
    // Image resources presence.
    // -----------------------------------------------------------------------

    #[test]
    fn image_resources_are_left_none_when_the_document_has_none() {
        // Mirror `if (Object.keys(rest).length)`; the old unconditional assignment
        // handed callers an all-`None` `ImageResources` for a document with no
        // resource block at all.
        let mut psd = crate::psd::Psd { width: 1.0, height: 1.0, ..Default::default() };
        psd.children = Some(vec![]);
        assert!(psd.image_resources.is_none());

        let bytes = crate::writer::write_psd(&psd, &crate::psd::WriteOptions::default());
        let back = read_psd(&bytes, &ReadOptions::default()).unwrap();
        assert!(
            back.image_resources.is_none(),
            "expected no image resources, got {:?}",
            back.image_resources
        );

        // A real file does carry resources, so the guard must not swallow them.
        let Some(fixture) = fixture_bytes("just-bg") else { return };
        let real = read_psd(&fixture, &ReadOptions::default()).unwrap();
        assert!(real.image_resources.is_some());
    }

    #[test]
    fn image_resources_is_empty_tracks_individual_fields() {
        let mut res = crate::psd::ImageResources::default();
        assert!(res.is_empty());
        res.global_angle = Some(30.0);
        assert!(!res.is_empty());
    }

    // -----------------------------------------------------------------------
    // Compression error wording.
    // -----------------------------------------------------------------------

    #[test]
    fn compression_errors_share_one_wording() {
        assert_eq!(
            compression_not_supported(Compression::ZipWithPrediction),
            ReadError::StrictViolation("Compression not supported: 3".to_string())
        );
        assert_eq!(
            compression_not_supported(Compression::RawData),
            ReadError::StrictViolation("Compression not supported: 0".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Lazy bitmaps (`use_raw_data`).
    // -----------------------------------------------------------------------

    fn first_layer_with_mask(layers: &[Layer]) -> Option<&Layer> {
        layers.iter().find_map(|l| {
            if l.additional_info.mask.is_some() {
                Some(l)
            } else {
                l.children.as_deref().and_then(first_layer_with_mask)
            }
        })
    }

    #[test]
    fn raw_data_defers_composite_decoding() {
        let Some(bytes) = fixture_bytes("just-bg") else { return };
        let opts = ReadOptions { use_raw_data: Some(true), ..Default::default() };
        let psd = read_psd(&bytes, &opts).unwrap();

        assert!(psd.canvas.is_none(), "composite must not be decoded eagerly");
        assert!(psd.image_data.is_none());
        let raw = psd.raw_composite_data.as_ref().expect("raw composite captured");
        assert!(!raw.is_empty());

        let decoded = get_composite_image_data(&psd).unwrap().expect("composite decoded");
        assert_eq!(decoded.width, 100);
        assert_eq!(decoded.height, 100);

        // Same pixels as the eager path.
        let eager = read_psd(&bytes, &ReadOptions::default()).unwrap();
        assert_eq!(decoded.data, eager.canvas.expect("eager canvas").data);
    }

    #[test]
    fn get_composite_image_data_without_raw_data_is_none() {
        let psd = crate::psd::Psd::default();
        assert!(get_composite_image_data(&psd).unwrap().is_none());
    }

    #[test]
    fn raw_data_defers_layer_and_mask_decoding() {
        let Some(bytes) = fixture_bytes("layer-mask") else { return };
        let opts = ReadOptions { use_raw_data: Some(true), ..Default::default() };
        let psd = read_psd(&bytes, &opts).unwrap();
        let children = psd.children.as_deref().expect("children");
        let layer = first_layer_with_mask(children).expect("a layer with a mask");

        assert!(layer.canvas.is_none(), "layer bitmap must not be decoded eagerly");
        assert!(layer.raw_data.is_some(), "raw channel data kept for later");

        let width = (layer.right.unwrap_or(0.0) - layer.left.unwrap_or(0.0)) as u32;
        let height = (layer.bottom.unwrap_or(0.0) - layer.top.unwrap_or(0.0)) as u32;
        let pixels = get_layer_image_data(layer).unwrap().expect("layer pixels");
        assert_eq!((pixels.width, pixels.height), (width, height));

        let mask = layer.additional_info.mask.as_ref().expect("mask");
        let mask_width = (mask.right.unwrap_or(0.0) - mask.left.unwrap_or(0.0)) as u32;
        let mask_height = (mask.bottom.unwrap_or(0.0) - mask.top.unwrap_or(0.0)) as u32;
        let mask_pixels = get_layer_mask_image_data(layer).unwrap().expect("mask pixels");
        assert_eq!((mask_pixels.width, mask_pixels.height), (mask_width, mask_height));
        assert_ne!(
            mask_pixels.data, pixels.data,
            "mask and layer bitmaps must not be the same buffer"
        );

        // No real mask in this fixture, so the real-mask getter yields nothing.
        assert!(get_layer_real_mask_image_data(layer).unwrap().is_none());
    }

    #[test]
    fn decode_layer_pixels_matches_the_eager_path() {
        let Some(bytes) = fixture_bytes("layer-mask") else { return };
        let opts = ReadOptions { use_raw_data: Some(true), ..Default::default() };
        let mut psd = read_psd(&bytes, &opts).unwrap();
        let layer = first_layer_with_mask_mut(psd.children.as_mut().expect("children"))
            .expect("a layer with a mask");

        decode_layer_pixels(layer, true).unwrap();
        assert!(layer.raw_data.is_none(), "raw data dropped after decoding");
        let decoded = layer.image_data.as_ref().expect("layer image data");
        let mask_decoded = layer
            .additional_info
            .mask
            .as_ref()
            .and_then(|m| m.image_data.as_ref())
            .expect("mask image data");

        let eager_opts = ReadOptions { use_image_data: Some(true), ..Default::default() };
        let eager = read_psd(&bytes, &eager_opts).unwrap();
        let eager_layer =
            first_layer_with_mask(eager.children.as_deref().expect("children")).expect("layer");
        assert_eq!(
            decoded.data,
            eager_layer.image_data.as_ref().expect("eager layer data").data
        );
        assert_eq!(
            mask_decoded.data,
            eager_layer
                .additional_info
                .mask
                .as_ref()
                .and_then(|m| m.image_data.as_ref())
                .expect("eager mask data")
                .data
        );
    }

    /// Mutable twin of [`first_layer_with_mask`].
    fn first_layer_with_mask_mut(layers: &mut [Layer]) -> Option<&mut Layer> {
        for layer in layers.iter_mut() {
            if layer.additional_info.mask.is_some() {
                return Some(layer);
            }
            if let Some(found) =
                layer.children.as_mut().and_then(|c| first_layer_with_mask_mut(c))
            {
                return Some(found);
            }
        }
        None
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
