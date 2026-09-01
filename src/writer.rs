/*
File: crates/ag-psd/src/writer.rs

Purpose:
низкоуровневая запись байтов в буфер PSD (структуры/курсор записи, примитивы записи чисел и строк).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/psdWriter.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.

Notes:
- The RLE scratch buffer (`PsdWriter::temp_buffer`) is shared by every channel
  encode in the document. It is sized once in `write_psd_to_writer` from
  `get_largest_layer_size` (which measures every layer plus its `mask` and
  `real_mask`) and the composite estimate. `write_data_rle` silently drops
  out-of-bounds writes, so undersizing this buffer corrupts output instead of
  failing — see `rle_scratch_size`, which also documents why the PSB sizing
  deliberately diverges from upstream.
- Every RLE row-length table entry is 4 bytes in PSB and 2 in PSD, so all
  scratch sizing must be told whether the document is PSB. Paths that encode
  through the shared buffer: the layer bitmap (`get_layer_channels`), the mask
  and real mask (`get_mask_channels`) and the composite (`write_image_data`).
  `write_pattern` encodes into its own local buffer, and the `Layer::raw_data`
  verbatim path does not encode at all.
- `get_channels` has a verbatim fast path for `Layer::raw_data` (channels read
  with `ReadOptions::use_raw_data`): they are written back without decoding.
*/

// PORT STATUS: primitives ported; document orchestration ported.

use crate::additional_info::{write_additional_info, WriteCtx};
use crate::helpers::{
    clamp, from_blend_mode, has_alpha, offset_for_channel, write_data_rle,
    write_data_rle_bit_depth, write_data_zip_without_prediction_bit_depth,
    Bounds as ChannelBounds, ChannelData,
    ColorSpace, LayerChannelData, LayerMaskFlags, MaskParams, RAW_IMAGE_DATA,
};
use crate::image_resources::{has_image_resource, write_image_resource, RESOURCE_IDS};
use crate::psd::{
    BlendMode, ChannelId, Color, ColorMode, Compression, GlobalLayerMaskInfo, Layer,
    LayerAdditionalInfo, LayerMaskData, PatternInfo, PixelData, Psd, SectionDividerType,
    WriteOptions,
};

/// Порт TS-интерфейса `PsdWriter`.
///
/// В upstream-е используется заранее аллоцированный `ArrayBuffer` фиксированного
/// размера + `DataView` + курсор `offset`, буфер растёт удвоением (`resizeBuffer`).
/// Здесь `buffer: Vec<u8>` хранит «вместимость» (capacity), заполненную нулями,
/// а реально записано `offset` байт. Это байт-в-байт повторяет upstream:
/// `getWriterBuffer` отдаёт срез `[0, offset)`, а `ensureSize`/`resizeBuffer`
/// воспроизводят логику удвоения. `tempBuffer` относится к оркестрации документа
/// (буфер для RLE) и здесь не используется примитивами, но поле сохранено для
/// зеркальности.
#[derive(Debug, Clone)]
pub struct PsdWriter {
    /// Аналог `ArrayBuffer` фиксированной вместимости (заполнен нулями до конца).
    pub buffer: Vec<u8>,
    /// Курсор записи (число реально записанных байт).
    pub offset: usize,
    /// Временный буфер для RLE-сжатия (используется оркестрацией документа).
    pub temp_buffer: Option<Vec<u8>>,
}

/// Порт `createWriter(size = 4096)`.
pub fn create_writer(size: usize) -> PsdWriter {
    PsdWriter {
        buffer: vec![0u8; size],
        offset: 0,
        temp_buffer: None,
    }
}

/// Порт `createWriter()` с дефолтным размером 4096.
pub fn create_writer_default() -> PsdWriter {
    create_writer(4096)
}

/// Порт `getWriterBuffer(writer)` — `buffer.slice(0, offset)` (копия).
pub fn get_writer_buffer(writer: &PsdWriter) -> Vec<u8> {
    writer.buffer[..writer.offset].to_vec()
}

/// Порт `getWriterBufferNoCopy(writer)` — `Uint8Array(buffer, 0, offset)` (без копии).
pub fn get_writer_buffer_no_copy(writer: &PsdWriter) -> &[u8] {
    &writer.buffer[..writer.offset]
}

// ===========================================================================
// Buffer growth (resizeBuffer / ensureSize / addSize)
// ===========================================================================

/// Порт `resizeBuffer(writer, size)`.
fn resize_buffer(writer: &mut PsdWriter, size: usize) {
    let mut new_length = writer.buffer.len();

    // do { newLength *= 2; } while (size > newLength);
    loop {
        new_length *= 2;
        if size <= new_length {
            break;
        }
    }

    writer.buffer.resize(new_length, 0);
}

/// Порт `ensureSize(writer, size)`.
fn ensure_size(writer: &mut PsdWriter, size: usize) {
    if size > writer.buffer.len() {
        resize_buffer(writer, size);
    }
}

/// Порт `addSize(writer, size)` — возвращает прежний `offset`, продвигает курсор.
fn add_size(writer: &mut PsdWriter, size: usize) -> usize {
    let offset = writer.offset;
    writer.offset += size;
    ensure_size(writer, writer.offset);
    offset
}

// ===========================================================================
// Big-endian / little-endian helpers (mirror DataView.setXxx)
//
// Endianness: в upstream все `view.setInt16/Uint16/Int32/Uint32/Float32/Float64`
// вызываются с `littleEndian = false`, т.е. PSD — big-endian. Исключения — явные
// `*LE`-варианты (`setUint16(..., true)`, `setInt32(..., true)`).
// ===========================================================================

#[inline]
fn set_bytes_be(writer: &mut PsdWriter, offset: usize, bytes: &[u8]) {
    writer.buffer[offset..offset + bytes.len()].copy_from_slice(bytes);
}

// ===========================================================================
// Scalar writers
// ===========================================================================

/// Порт `writeUint8`.
pub fn write_uint8(writer: &mut PsdWriter, value: u8) {
    let offset = add_size(writer, 1);
    writer.buffer[offset] = value;
}

/// Порт `writeInt16` (big-endian).
pub fn write_int16(writer: &mut PsdWriter, value: i16) {
    let offset = add_size(writer, 2);
    set_bytes_be(writer, offset, &value.to_be_bytes());
}

/// Порт `writeUint16` (big-endian).
pub fn write_uint16(writer: &mut PsdWriter, value: u16) {
    let offset = add_size(writer, 2);
    set_bytes_be(writer, offset, &value.to_be_bytes());
}

/// Порт `writeUint16LE` (little-endian).
pub fn write_uint16_le(writer: &mut PsdWriter, value: u16) {
    let offset = add_size(writer, 2);
    set_bytes_be(writer, offset, &value.to_le_bytes());
}

/// Порт `writeInt32` (big-endian).
pub fn write_int32(writer: &mut PsdWriter, value: i32) {
    let offset = add_size(writer, 4);
    set_bytes_be(writer, offset, &value.to_be_bytes());
}

/// Порт `writeInt32LE` (little-endian).
pub fn write_int32_le(writer: &mut PsdWriter, value: i32) {
    let offset = add_size(writer, 4);
    set_bytes_be(writer, offset, &value.to_le_bytes());
}

/// Порт `writeUint32` (big-endian).
pub fn write_uint32(writer: &mut PsdWriter, value: u32) {
    let offset = add_size(writer, 4);
    set_bytes_be(writer, offset, &value.to_be_bytes());
}

/// Порт `writeFloat32` (big-endian).
pub fn write_float32(writer: &mut PsdWriter, value: f32) {
    let offset = add_size(writer, 4);
    set_bytes_be(writer, offset, &value.to_be_bytes());
}

/// Порт `writeFloat64` (big-endian).
pub fn write_float64(writer: &mut PsdWriter, value: f64) {
    let offset = add_size(writer, 8);
    set_bytes_be(writer, offset, &value.to_be_bytes());
}

/// Порт `writeFixedPoint32` — 32-битное число с фиксированной точкой 16.16.
pub fn write_fixed_point32(writer: &mut PsdWriter, value: f64) {
    write_int32(writer, (value * (1i64 << 16) as f64) as i32);
}

/// Порт `writeFixedPointPath32` — 32-битное число с фиксированной точкой 8.24.
pub fn write_fixed_point_path32(writer: &mut PsdWriter, value: f64) {
    write_int32(writer, (value * (1i64 << 24) as f64) as i32);
}

/// Порт `writeBytes`. В Rust пустой срез эквивалентен `undefined`/пустому буферу.
pub fn write_bytes(writer: &mut PsdWriter, buffer: Option<&[u8]>) {
    if let Some(buffer) = buffer {
        ensure_size(writer, writer.offset + buffer.len());
        let offset = writer.offset;
        writer.buffer[offset..offset + buffer.len()].copy_from_slice(buffer);
        writer.offset += buffer.len();
    }
}

/// Порт `writeZeros(writer, count)`.
pub fn write_zeros(writer: &mut PsdWriter, count: usize) {
    for _ in 0..count {
        write_uint8(writer, 0);
    }
}

/// Порт `writeSignature(writer, signature)` — ровно 4 ASCII-символа.
pub fn write_signature(writer: &mut PsdWriter, signature: &str) {
    if signature.len() != 4 {
        panic!("Invalid signature: '{}'", signature);
    }

    for b in signature.bytes() {
        write_uint8(writer, b);
    }
}

/// Порт `writeAsciiString(writer, text)` — по одному char-code на байт.
///
/// Upstream использует `charCodeAt` (UTF-16 code unit); для не-ASCII строк
/// результат усекается до младшего байта. Здесь зеркалируем через коды
/// символов (`char as u32 as u8`), что совпадает для ASCII.
pub fn write_ascii_string(writer: &mut PsdWriter, text: &str) {
    for ch in text.chars() {
        write_uint8(writer, (ch as u32) as u8);
    }
}

/// Порт `writePascalString(writer, text, padTo)`.
pub fn write_pascal_string(writer: &mut PsdWriter, text: &str, pad_to: usize) {
    let chars: Vec<char> = text.chars().collect();
    let mut length = chars.len();
    if length > 255 {
        panic!("String too long");
    }

    write_uint8(writer, length as u8);

    for &ch in &chars {
        let code = ch as u32;
        // code < 128 ? code : '?'
        write_uint8(writer, if code < 128 { code as u8 } else { b'?' });
    }

    // while (++length % padTo) writeUint8(0)
    length += 1;
    while length % pad_to != 0 {
        write_uint8(writer, 0);
        length += 1;
    }
}

/// Порт `writeUnicodeStringWithoutLength` — UTF-16 BE code units, без префикса длины.
pub fn write_unicode_string_without_length(writer: &mut PsdWriter, text: &str) {
    for unit in text.encode_utf16() {
        write_uint16(writer, unit);
    }
}

/// Порт `writeUnicodeStringWithoutLengthLE` — UTF-16 LE code units, без префикса длины.
pub fn write_unicode_string_without_length_le(writer: &mut PsdWriter, text: &str) {
    for unit in text.encode_utf16() {
        write_uint16_le(writer, unit);
    }
}

/// Порт `writeUnicodeString` — префикс длины (в UTF-16 code units) + строка BE.
pub fn write_unicode_string(writer: &mut PsdWriter, text: &str) {
    let len = text.encode_utf16().count();
    write_uint32(writer, len as u32);
    write_unicode_string_without_length(writer, text);
}

/// Порт `writeUnicodeStringWithPadding` — длина+1, строка BE, завершающий 0.
pub fn write_unicode_string_with_padding(writer: &mut PsdWriter, text: &str) {
    let len = text.encode_utf16().count();
    write_uint32(writer, (len + 1) as u32);

    for unit in text.encode_utf16() {
        write_uint16(writer, unit);
    }

    write_uint16(writer, 0);
}

// ===========================================================================
// Section helper
// ===========================================================================

/// Порт `writeSection(writer, round, func, writeTotalLength = false, large = false)`.
///
/// Пишет длину-префикс (4 байта BE; при `large` — два 4-байтовых слова),
/// выполняет `func`, затем добавляет паддинг до кратности `round` и
/// бэкпатчит длину секции.
pub fn write_section<F: FnOnce(&mut PsdWriter)>(
    writer: &mut PsdWriter,
    round: usize,
    func: F,
    write_total_length: bool,
    large: bool,
) {
    if large {
        write_uint32(writer, 0);
    }
    let offset = writer.offset;
    write_uint32(writer, 0);

    func(writer);

    let mut length = writer.offset - offset - 4;
    let mut len = length;

    while len % round != 0 {
        write_uint8(writer, 0);
        len += 1;
    }

    if write_total_length {
        length = len;
    }

    // writer.view.setUint32(offset, length, false)
    set_bytes_be(writer, offset, &(length as u32).to_be_bytes());
}

// ===========================================================================
// Color / Pattern generic helpers
// ===========================================================================

/// Порт `writeColor(writer, color)`.
pub fn write_color(writer: &mut PsdWriter, color: Option<&Color>) {
    match color {
        None => {
            write_uint16(writer, ColorSpace::Rgb as u16);
            write_zeros(writer, 8);
        }
        Some(Color::Rgba(c)) => {
            // 'r' in color
            write_uint16(writer, ColorSpace::Rgb as u16);
            write_uint16(writer, (c.r * 257.0).round() as u16);
            write_uint16(writer, (c.g * 257.0).round() as u16);
            write_uint16(writer, (c.b * 257.0).round() as u16);
            write_uint16(writer, 0);
        }
        Some(Color::Rgb(c)) => {
            // 'r' in color
            write_uint16(writer, ColorSpace::Rgb as u16);
            write_uint16(writer, (c.r * 257.0).round() as u16);
            write_uint16(writer, (c.g * 257.0).round() as u16);
            write_uint16(writer, (c.b * 257.0).round() as u16);
            write_uint16(writer, 0);
        }
        Some(Color::Frgb(c)) => {
            // 'fr' in color
            write_uint16(writer, ColorSpace::Rgb as u16);
            write_uint16(writer, (c.fr * 255.0 * 257.0).round() as u16);
            write_uint16(writer, (c.fg * 255.0 * 257.0).round() as u16);
            write_uint16(writer, (c.fb * 255.0 * 257.0).round() as u16);
            write_uint16(writer, 0);
        }
        Some(Color::Lab(c)) => {
            // 'l' in color
            write_uint16(writer, ColorSpace::Lab as u16);
            write_int16(writer, (c.l * 10000.0).round() as i16);
            write_int16(
                writer,
                (if c.a < 0.0 { c.a * 12800.0 } else { c.a * 12700.0 }).round() as i16,
            );
            write_int16(
                writer,
                (if c.b < 0.0 { c.b * 12800.0 } else { c.b * 12700.0 }).round() as i16,
            );
            write_uint16(writer, 0);
        }
        Some(Color::Hsb(c)) => {
            // 'h' in color
            write_uint16(writer, ColorSpace::Hsb as u16);
            write_uint16(writer, (c.h * 0xffff as f64).round() as u16);
            write_uint16(writer, (c.s * 0xffff as f64).round() as u16);
            write_uint16(writer, (c.b * 0xffff as f64).round() as u16);
            write_uint16(writer, 0);
        }
        Some(Color::Cmyk(c)) => {
            // 'c' in color
            write_uint16(writer, ColorSpace::Cmyk as u16);
            write_uint16(writer, (c.c * 257.0).round() as u16);
            write_uint16(writer, (c.m * 257.0).round() as u16);
            write_uint16(writer, (c.y * 257.0).round() as u16);
            write_uint16(writer, (c.k * 257.0).round() as u16);
        }
        Some(Color::Grayscale(c)) => {
            // else
            write_uint16(writer, ColorSpace::Grayscale as u16);
            write_uint16(writer, (c.k * 10000.0 / 255.0).round() as u16);
            write_zeros(writer, 6);
        }
    }
}

/// Порт `writePattern(writer, pattern)`.
///
/// Оперирует только `PatternInfo` и примитивом `write_data_rle`, без знания о
/// форме Psd/Layer, поэтому остаётся в слое примитивов.
pub fn write_pattern(writer: &mut PsdWriter, pattern: &PatternInfo) {
    let width = pattern.bounds.w as u32;
    let height = pattern.bounds.h as u32;
    let pixel_data = PixelData {
        width,
        height,
        data: pattern.data.clone(),
    };

    write_uint32(writer, 0); // length, fixed up below
    let patts_offset = writer.offset;

    write_uint32(writer, 1); // version
    write_uint32(writer, ColorMode::Rgb as u32); // color mode - rgb only

    write_int16(writer, pattern.x as i16);
    write_int16(writer, pattern.y as i16);

    write_unicode_string(writer, &format!("{}\0", pattern.name)); // name
    write_pascal_string(writer, &pattern.id, 1); // id

    // virtual memory array list
    write_uint32(writer, 3); // version
    write_uint32(writer, 0); // length, fixed up below
    let vl_offset = writer.offset;

    let top = pattern.bounds.y as u32;
    let left = pattern.bounds.x as u32;
    let bottom = top + height;
    let right = left + width;

    write_uint32(writer, top);
    write_uint32(writer, left);
    write_uint32(writer, bottom);
    write_uint32(writer, right);

    write_uint32(writer, 24); // channels count

    // channels: RGB at indices 0,1,2 and alpha at index 25
    for i in 0..(24 + 2) {
        let offset: i32 = if i < 3 {
            i
        } else if i == 25 {
            3
        } else {
            -1
        };

        if offset < 0 {
            write_uint32(writer, 0); // has
            continue;
        }

        // Worst-case RLE size for a single channel. Patterns always use the
        // short (PSD) row-length table, hence `large = false`. Upstream sizes
        // this as `width * height + 2 * height + 2 * width + 16`, which is not a
        // bound for tall, narrow channels (the `2 * width + 16` slack does not
        // cover the ~`height / 128` run headers) and overflows `u32` on large
        // patterns; `rle_scratch_size` is a proven bound in saturating usize.
        let mut buffer = vec![0u8; rle_scratch_size(width, height, 1, false, 8)];
        let data = write_data_rle(&mut buffer, &pixel_data, &[offset as usize], false)
            .expect("write_data_rle returned None for pattern channel");

        write_uint32(writer, 1); // has
        write_uint32(writer, (data.len() + 4 + 16 + 2 + 1) as u32); // length
        write_uint32(writer, 8); // pixelDepth
        write_uint32(writer, top);
        write_uint32(writer, left);
        write_uint32(writer, bottom);
        write_uint32(writer, right);
        write_uint16(writer, 8); // pixelDepth2
        write_uint8(writer, 1); // compressionMode - rle
        write_bytes(writer, Some(&data));
    }

    let vl_length = writer.offset - vl_offset;
    let mut patts_length = writer.offset - patts_offset;

    while patts_length % 4 != 0 {
        write_zeros(writer, 1);
        patts_length += 1;
    }

    set_bytes_be(writer, vl_offset - 4, &(vl_length as u32).to_be_bytes());
    set_bytes_be(writer, patts_offset - 4, &(patts_length as u32).to_be_bytes());
}

// ===========================================================================
// Document orchestration (port of psdWriter.ts writePsd & friends)
// ===========================================================================

/// Byte width of one entry of the RLE per-row length table.
///
/// PSB (`large`) stores row lengths as `u32`, PSD as `u16`; this must match what
/// `helpers::write_data_rle` emits for the same `large` flag.
const fn rle_row_length_entry_size(large: bool) -> usize {
    if large {
        4
    } else {
        2
    }
}

/// Worst-case size of the RLE scratch buffer for `channel_count` channels of a
/// `width` x `height` bitmap, encoded with `large` row lengths (PSB).
///
/// Two terms per channel: `height * entry_size` for the per-row length table and
/// `2 * width * height * bytes_per_sample` for a pathological, fully
/// incompressible channel (RLE never expands a row past ~`width * 129 / 128`,
/// so twice the row is a safe bound). Saturating arithmetic keeps an absurd
/// bitmap from wrapping the size instead of failing loudly at allocation time.
///
/// DELIBERATE DIVERGENCE FROM UPSTREAM — do not "restore" on the next sync.
/// Upstream (`getLargestLayerSize` / `writePsd` in `psdWriter.ts`) hardcodes
/// `2 * height` for the length table and ignores the PSB flag, so every PSB
/// scratch buffer is short by `2 * height` bytes per channel. Upstream does not
/// notice because `writeDataRLE` writes into a `Uint8Array`: out-of-bounds
/// writes are silently dropped and the result is truncated, which turns the
/// shortfall into structurally valid but incomplete channel data instead of an
/// error. Shipping known output corruption is worse than a documented deviation,
/// so the entry size is computed from `large` here.
fn rle_scratch_size(
    width: u32,
    height: u32,
    channel_count: usize,
    large: bool,
    bits_per_channel: u32,
) -> usize {
    // u32 -> usize is a widening conversion on both supported targets
    // (x86_64-unknown-linux-gnu / x86_64-pc-windows-gnu), so nothing is lost.
    let w = width as usize;
    let h = height as usize;
    let table = h.saturating_mul(rle_row_length_entry_size(large));
    let bytes_per_sample = match bits_per_channel {
        16 => 2,
        32 => 4,
        _ => 1,
    };
    let data = 2usize
        .saturating_mul(w)
        .saturating_mul(h)
        .saturating_mul(bytes_per_sample);
    table.saturating_add(data).saturating_mul(channel_count)
}

/// Порт `getLargestLayerSize(layers)`.
///
/// Returns the largest single-channel RLE scratch size required by any layer in
/// the tree. Every layer is measured — including layers with no bitmap of their
/// own — together with its `mask` and `real_mask`, because those are encoded
/// through the same shared scratch buffer and may be larger than the layer.
/// `large` is the PSB flag of the document being written; it selects the width
/// of the per-row length table entries (see `rle_scratch_size`).
fn get_largest_layer_size(layers: Option<&[Layer]>, large: bool, bits_per_channel: u32) -> usize {
    let mut max = 0usize;
    let layers = match layers {
        Some(l) => l,
        None => return 0,
    };
    for layer in layers {
        let (width, height) =
            get_layer_dimensions(layer.canvas.as_ref(), layer.image_data.as_ref());
        // Layer bitmaps and masks are encoded one channel at a time
        // (`get_layer_channels` / `get_mask_channels` pass a single offset).
        max = max.max(rle_scratch_size(width, height, 1, large, bits_per_channel));

        if let Some(mask) = &layer.additional_info.mask {
            let (width, height) = get_layer_dimensions(mask.canvas.as_ref(), mask.image_data.as_ref());
            max = max.max(rle_scratch_size(width, height, 1, large, bits_per_channel));
        }

        if let Some(real_mask) = &layer.additional_info.real_mask {
            let (width, height) =
                get_layer_dimensions(real_mask.canvas.as_ref(), real_mask.image_data.as_ref());
            max = max.max(rle_scratch_size(width, height, 1, large, bits_per_channel));
        }

        if let Some(children) = &layer.children {
            max = max.max(get_largest_layer_size(Some(children), large, bits_per_channel));
        }
    }
    max
}

/// Порт `getLayerDimentions({ canvas, imageData })`.
/// imageData берёт приоритет над canvas (как в upstream).
fn get_layer_dimensions(canvas: Option<&PixelData>, image_data: Option<&PixelData>) -> (u32, u32) {
    if let Some(d) = image_data {
        (d.width, d.height)
    } else if let Some(c) = canvas {
        (c.width, c.height)
    } else {
        (0, 0)
    }
}

/// Порт `verifyBitCount(target)`. В байтовой модели PixelData всегда 8-битный
/// (нет Uint16Array/Uint32Array), поэтому проверка тривиально проходит. Оставлено
/// для зеркальности (no-op рекурсия).
fn verify_bit_count(_target_children: Option<&[Layer]>) {
    // PixelData is always RGBA8 in this port; nothing to verify.
}

/// Публичная точка входа: построить байты `.psd` из документа.
/// Порт связки `writePsd` (psd.ts) + `writePsd(writer, ...)` (psdWriter.ts).
pub fn write_psd(psd: &Psd, options: &WriteOptions) -> Vec<u8> {
    let mut writer = create_writer_default();
    write_psd_to_writer(&mut writer, psd, options);
    get_writer_buffer(&writer)
}

/// Порт `writePsd(writer, psd, options)`.
pub fn write_psd_to_writer(writer: &mut PsdWriter, psd: &Psd, options: &WriteOptions) {
    if !(psd.width > 0.0 && psd.height > 0.0) {
        panic!("Invalid document size");
    }

    let psb = options.psb == Some(true);

    if (psd.width > 30000.0 || psd.height > 30000.0) && !psb {
        panic!("Document size is too large (max is 30000x30000, use PSB format instead)");
    }

    let bits_per_channel = psd.bits_per_channel.unwrap_or(8.0);
    if !matches!(bits_per_channel, 8.0 | 16.0 | 32.0) {
        panic!("bitsPerChannel must be 8, 16, or 32 for writing");
    }

    verify_bit_count(psd.children.as_deref());

    // imageResources: { ...psd.imageResources }. generateThumbnail would set
    // imageResources.thumbnail, but thumbnail generation requires canvas
    // scaling (createThumbnail) which is a browser-canvas concern not available
    // here; see report.
    let image_resources = psd.image_resources.clone().unwrap_or_default();

    // imageData (composite). Our model stores both image_data and canvas as
    // PixelData; image_data takes priority, falling back to canvas.
    let image_data: Option<&PixelData> = psd.image_data.as_ref().or(psd.canvas.as_ref());

    if let Some(id) = image_data {
        if psd.width as u32 != id.width || psd.height as u32 != id.height {
            panic!("Document canvas must have the same size as document");
        }
    }

    let global_alpha = image_data.map(has_alpha).unwrap_or(false);

    // The composite is encoded in one `write_data_rle` call over all of its
    // channels (3, or 4 with global alpha), so its scratch requirement scales
    // with the channel count — including the per-row length table, which
    // upstream's estimate charges only once. Size for 4 channels
    // unconditionally: it is the maximum the composite writer can ask for.
    const COMPOSITE_MAX_CHANNELS: usize = 4;
    let composite_size = rle_scratch_size(
        psd.width as u32,
        psd.height as u32,
        COMPOSITE_MAX_CHANNELS,
        psb,
        bits_per_channel as u32,
    );
    let max_buffer_size = get_largest_layer_size(
        psd.children.as_deref(),
        psb,
        bits_per_channel as u32,
    )
    .max(composite_size);
    writer.temp_buffer = Some(vec![0u8; max_buffer_size]);

    // header
    write_signature(writer, "8BPS");
    write_uint16(writer, if psb { 2 } else { 1 }); // version
    write_zeros(writer, 6);
    write_uint16(writer, if global_alpha { 4 } else { 3 }); // channels
    write_uint32(writer, psd.height as u32);
    write_uint32(writer, psd.width as u32);
    write_uint16(writer, bits_per_channel as u16);
    write_uint16(writer, ColorMode::Rgb as u16); // we only support saving RGB

    // color mode data
    let palette = psd.palette.clone();
    write_section(
        writer,
        1,
        |w| {
            if let Some(palette) = &palette {
                for i in 0..256 {
                    w_palette_byte(w, palette.get(i).map(|c| c.r));
                }
                for i in 0..256 {
                    w_palette_byte(w, palette.get(i).map(|c| c.g));
                }
                for i in 0..256 {
                    w_palette_byte(w, palette.get(i).map(|c| c.b));
                }
            }
        },
        false,
        false,
    );

    // layers (flattened with section dividers)
    //
    // upstream unconditionally does `if (!layers.length) layers.push({})`, which
    // materialises a single empty placeholder layer even for documents that have
    // no layers section at all (e.g. a background-only PSD where `psd.children`
    // is absent). That makes read(write(x)) gain a spurious child for such files.
    // We only add the placeholder when the document actually carries a layers
    // section (`children` present), so a background-only / no-layer document
    // round-trips to the same (zero) child count it was read with. Documents with
    // an explicit (possibly empty) children list still get the placeholder, as
    // upstream requires for a valid layer section.
    let has_layer_section = psd.children.is_some();
    let mut layers: Vec<Layer> = Vec::new();
    add_children(&mut layers, psd.children.as_deref());
    if layers.is_empty() && has_layer_section {
        layers.push(Layer::default());
    }

    // image resources
    //
    // upstream additionally sets imageResources.layersGroup /
    // layerGroupsEnabledId here (resource ids 1026/1072). Those are
    // InternalImageResources-only and are NOT modeled on the public
    // ImageResources struct (the reader skips them), so they are not written.
    // See report for this dependency gap.
    write_section(
        writer,
        1,
        |w| {
            for &id in RESOURCE_IDS {
                let count = has_image_resource(id, &image_resources);
                for i in 0..count {
                    write_signature(w, "8BIM");
                    write_uint16(w, id);
                    write_pascal_string(w, "", 2);
                    write_section(
                        w,
                        2,
                        |w| {
                            // write_image_resource returns ReadResult<()>; errors
                            // here mean a malformed resource model — surface as panic
                            // to mirror upstream throwing.
                            write_image_resource(id, w, &image_resources, i)
                                .expect("write_image_resource failed");
                        },
                        false,
                        false,
                    );
                }
            }
        },
        false,
        false,
    );

    // layer and mask info
    write_section(
        writer,
        2,
        |w| {
            write_layer_info(w, &layers, psd, global_alpha, options, psb);
            write_global_layer_mask_info(w, psd.global_layer_mask_info.as_ref());
            // document-level additional layer info
            let mut ctx = WriteCtx::new(options, psb);
            write_additional_info(w, &psd.additional_info, &mut ctx);
        },
        false,
        psb,
    );

    // image data (composite)
    let channels: Vec<usize> = if global_alpha {
        vec![0, 1, 2, 3]
    } else {
        vec![0, 1, 2]
    };
    let width = image_data.map(|d| d.width).unwrap_or(psd.width as u32);
    let height = image_data.map(|d| d.height).unwrap_or(psd.height as u32);
    let mut data = PixelData {
        width,
        height,
        data: vec![0u8; (width as usize) * (height as usize) * 4],
    };

    write_uint16(writer, Compression::RleCompressed as u16);

    if let Some(id) = image_data {
        data.data[..id.data.len()].copy_from_slice(&id.data);

        // add weird white matte
        if global_alpha {
            let size = (data.width as usize) * (data.height as usize) * 4;
            let p = &mut data.data;
            let mut i = 0;
            while i < size {
                let pa = p[i + 3];
                if pa != 0 && pa != 255 {
                    let a = pa as f64 / 255.0;
                    let ra = 255.0 * (1.0 - a);
                    p[i] = (p[i] as f64 * a + ra) as u8;
                    p[i + 1] = (p[i + 1] as f64 * a + ra) as u8;
                    p[i + 2] = (p[i + 2] as f64 * a + ra) as u8;
                }
                i += 4;
            }
        }
    }

    let mut temp = writer.temp_buffer.take().unwrap();
    let rle = write_data_rle_bit_depth(
        &mut temp,
        &data,
        &channels,
        psb,
        bits_per_channel as u32,
    );
    writer.temp_buffer = Some(temp);
    write_bytes(writer, rle.as_deref());
}

/// Записать один байт палитры (0 при отсутствии цвета).
fn w_palette_byte(writer: &mut PsdWriter, value: Option<f64>) {
    write_uint8(writer, value.unwrap_or(0.0) as u8);
}

/// Порт `writeLayerInfo(writer, layers, psd, globalAlpha, options)`.
fn write_layer_info(
    writer: &mut PsdWriter,
    layers: &[Layer],
    psd: &Psd,
    global_alpha: bool,
    options: &WriteOptions,
    psb: bool,
) {
    write_section(
        writer,
        4,
        |w| {
            write_int16(
                w,
                if global_alpha {
                    -(layers.len() as i16)
                } else {
                    layers.len() as i16
                },
            );

            // extract channels for every layer
            let mut temp = w.temp_buffer.take().unwrap();
            let mut layers_data: Vec<LayerChannelData> = layers
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    get_channels(&mut temp, l, i == 0, options, psb, psd.bits_per_channel)
                })
                .collect();
            w.temp_buffer = Some(temp);

            // layer records
            let mut ctx = WriteCtx::new(options, psb);
            for layer_data in &layers_data {
                let layer = &layer_data.layer;
                write_int32(w, layer_data.top);
                write_int32(w, layer_data.left);
                write_int32(w, layer_data.bottom);
                write_int32(w, layer_data.right);
                write_uint16(w, layer_data.channels.len() as u16);

                for c in &layer_data.channels {
                    write_int16(w, c.id as i16);
                    if psb {
                        write_uint32(w, 0);
                    }
                    write_uint32(w, c.length as u32);
                }

                write_signature(w, "8BIM");
                // Mirror `fromBlendMode[layer.blendMode!] || 'norm'`: descriptor-only
                // modes have no legacy signature and fall back to 'norm'.
                let blend = layer.blend_mode.and_then(from_blend_mode).unwrap_or("norm");
                write_signature(w, blend);
                write_uint8(w, (clamp(layer.opacity.unwrap_or(1.0), 0.0, 1.0) * 255.0).round() as u8);
                write_uint8(w, if layer.clipping == Some(true) { 1 } else { 0 });

                let mut flags: u8 = 0x08;
                if layer.transparency_protected == Some(true) {
                    flags |= 0x01;
                }
                if layer.hidden == Some(true) {
                    flags |= 0x02;
                }
                let info = &layer.additional_info;
                let section_irrelevant = info.section_divider.as_ref().is_some_and(|sd| {
                    sd.divider_type != SectionDividerType::Other
                });
                if info.vector_mask.is_some() || section_irrelevant || info.adjustment.is_some() {
                    flags |= 0x10;
                }
                if layer.effects_open == Some(true) {
                    flags |= 0x20;
                }

                write_uint8(w, flags);
                write_uint8(w, 0); // filler

                write_section(
                    w,
                    1,
                    |w| {
                        write_layer_mask_data(w, info, layer_data);
                        write_layer_blending_ranges(w, info);
                        let name = info.name.clone().unwrap_or_default();
                        let name: String = name.chars().take(255).collect();
                        write_pascal_string(w, &name, 4);
                        write_additional_info(w, info, &mut ctx);
                    },
                    false,
                    false,
                );
            }

            // layer channel image data
            for layer_data in &mut layers_data {
                for channel in &layer_data.channels {
                    write_uint16(w, channel.compression as u16);
                    if let Some(buffer) = &channel.data {
                        write_bytes(w, Some(buffer));
                    }
                }
            }
        },
        true,
        psb,
    );
}

/// Порт `writeLayerMaskData(writer, { mask, realMask }, layerData)`.
fn write_layer_mask_data(
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    layer_data: &LayerChannelData,
) {
    let mask = info.mask.as_ref();
    let real_mask = info.real_mask.as_ref();
    write_section(
        writer,
        1,
        |w| {
            if mask.is_none() && real_mask.is_none() {
                return;
            }

            let mut params: u8 = 0;
            let mut flags: u8 = 0;
            let mut real_flags: u8 = 0;

            if let Some(mask) = mask {
                if mask.user_mask_density.is_some() {
                    params |= MaskParams::UserMaskDensity as u8;
                }
                if mask.user_mask_feather.is_some() {
                    params |= MaskParams::UserMaskFeather as u8;
                }
                if mask.vector_mask_density.is_some() {
                    params |= MaskParams::VectorMaskDensity as u8;
                }
                if mask.vector_mask_feather.is_some() {
                    params |= MaskParams::VectorMaskFeather as u8;
                }

                if mask.disabled == Some(true) {
                    flags |= LayerMaskFlags::LayerMaskDisabled as u8;
                }
                if mask.position_relative_to_layer == Some(true) {
                    flags |= LayerMaskFlags::PositionRelativeToLayer as u8;
                }
                if mask.from_vector_data == Some(true) {
                    flags |= LayerMaskFlags::LayerMaskFromRenderingOtherData as u8;
                }
                if params != 0 {
                    flags |= LayerMaskFlags::MaskHasParametersAppliedToIt as u8;
                }
            }

            let m = layer_data.mask.unwrap_or_default();
            write_int32(w, m.top);
            write_int32(w, m.left);
            write_int32(w, m.bottom);
            write_int32(w, m.right);
            write_uint8(w, mask.and_then(|m| m.default_color).unwrap_or(0.0) as u8);
            write_uint8(w, flags);

            if let Some(real_mask) = real_mask {
                if real_mask.disabled == Some(true) {
                    real_flags |= LayerMaskFlags::LayerMaskDisabled as u8;
                }
                if real_mask.position_relative_to_layer == Some(true) {
                    real_flags |= LayerMaskFlags::PositionRelativeToLayer as u8;
                }
                if real_mask.from_vector_data == Some(true) {
                    real_flags |= LayerMaskFlags::LayerMaskFromRenderingOtherData as u8;
                }

                let r = layer_data.real_mask.unwrap_or_default();
                write_uint8(w, real_flags);
                write_uint8(w, real_mask.default_color.unwrap_or(0.0) as u8);
                write_int32(w, r.top);
                write_int32(w, r.left);
                write_int32(w, r.bottom);
                write_int32(w, r.right);
            }

            if params != 0 {
                if let Some(mask) = mask {
                    write_uint8(w, params);
                    if let Some(v) = mask.user_mask_density {
                        write_uint8(w, (v * 0xff as f64).round() as u8);
                    }
                    if let Some(v) = mask.user_mask_feather {
                        write_float64(w, v);
                    }
                    if let Some(v) = mask.vector_mask_density {
                        write_uint8(w, (v * 0xff as f64).round() as u8);
                    }
                    if let Some(v) = mask.vector_mask_feather {
                        write_float64(w, v);
                    }
                }
            }

            write_zeros(w, 2);
        },
        false,
        false,
    );
}

/// Порт `writerBlendingRange`.
fn write_blending_range(writer: &mut PsdWriter, range: &[f64]) {
    write_uint8(writer, range[0] as u8);
    write_uint8(writer, range[1] as u8);
    write_uint8(writer, range[2] as u8);
    write_uint8(writer, range[3] as u8);
}

/// Порт `writeLayerBlendingRanges(writer, layer)`.
fn write_layer_blending_ranges(writer: &mut PsdWriter, info: &LayerAdditionalInfo) {
    let ranges = info.blending_ranges.clone();
    write_section(
        writer,
        1,
        |w| {
            if let Some(ranges) = &ranges {
                write_blending_range(w, &ranges.composite_gray_blend_source);
                write_blending_range(w, &ranges.composite_graph_blend_destination_range);
                for r in &ranges.ranges {
                    write_blending_range(w, &r.source_range);
                    write_blending_range(w, &r.dest_range);
                }
            }
        },
        false,
        false,
    );
}

/// Порт `writeGlobalLayerMaskInfo(writer, info)`.
fn write_global_layer_mask_info(writer: &mut PsdWriter, info: Option<&GlobalLayerMaskInfo>) {
    let info = info.cloned();
    write_section(
        writer,
        1,
        |w| {
            if let Some(info) = &info {
                write_uint16(w, info.overlay_color_space as u16);
                write_uint16(w, info.color_space1 as u16);
                write_uint16(w, info.color_space2 as u16);
                write_uint16(w, info.color_space3 as u16);
                write_uint16(w, info.color_space4 as u16);
                // `round`, not truncation: the reader divides by 0xff, so
                // truncating turns 0.5 into 127/255 and breaks the round trip.
                write_uint16(w, (info.opacity * 0xff as f64).round() as u16);
                write_uint8(w, info.kind as u8);
                write_zeros(w, 3);
            }
        },
        false,
        false,
    );
}

/// Порт `addChildren(layers, children)`.
fn add_children(layers: &mut Vec<Layer>, children: Option<&[Layer]>) {
    let children = match children {
        Some(c) => c,
        None => return,
    };

    for c in children {
        if c.children.is_some() && c.canvas.is_some() {
            panic!("Invalid layer, cannot have both 'canvas' and 'children' properties");
        }
        if c.children.is_some() && c.image_data.is_some() {
            panic!("Invalid layer, cannot have both 'imageData' and 'children' properties");
        }

        if c.children.is_some() {
            // bounding section divider
            let mut open_layer = Layer::default();
            open_layer.additional_info.name = Some("</Layer group>".to_string());
            open_layer.additional_info.section_divider = Some(crate::psd::SectionDivider {
                divider_type: SectionDividerType::BoundingSectionDivider,
                key: None,
                sub_type: None,
            });
            layers.push(open_layer);

            add_children(layers, c.children.as_deref());

            // closing folder layer: copy of `c` with adjusted blend mode + divider
            let mut folder = c.clone();
            folder.children = None;
            if folder.blend_mode == Some(BlendMode::PassThrough) {
                folder.blend_mode = Some(BlendMode::Normal);
            }
            // Mirror `fromBlendMode[c.blendMode!] || 'pass'`.
            let key = c
                .blend_mode
                .and_then(from_blend_mode)
                .unwrap_or("pass")
                .to_string();
            folder.additional_info.section_divider = Some(crate::psd::SectionDivider {
                divider_type: if c.opened == Some(false) {
                    SectionDividerType::ClosedFolder
                } else {
                    SectionDividerType::OpenFolder
                },
                key: Some(key),
                sub_type: Some(0.0),
            });
            layers.push(folder);
        } else {
            layers.push(c.clone());
        }
    }
}

/// Порт `bounds(obj)` — прямоугольник из необязательных координат; отсутствующая
/// координата трактуется как 0 (upstream `obj.top || 0`).
fn bounds_or_zero(
    top: Option<f64>,
    left: Option<f64>,
    bottom: Option<f64>,
    right: Option<f64>,
) -> ChannelBounds {
    ChannelBounds {
        top: top.unwrap_or(0.0) as i32,
        left: left.unwrap_or(0.0) as i32,
        right: right.unwrap_or(0.0) as i32,
        bottom: bottom.unwrap_or(0.0) as i32,
    }
}

/// Порт `getChannels(tempBuffer, layer, background, options)`.
///
/// If the layer still carries undecoded channel payloads (`Layer::raw_data`,
/// produced by the reader under `ReadOptions::use_raw_data`), they are written
/// back verbatim: no decode, no re-encode, and bounds are taken from the layer
/// and its masks as-is. Otherwise the layer bitmap and masks are encoded through
/// `temp_buffer`.
fn get_channels(
    temp_buffer: &mut [u8],
    layer: &Layer,
    background: bool,
    options: &WriteOptions,
    psb: bool,
    bits_per_channel: Option<f64>,
) -> LayerChannelData {
    let bits_per_channel = bits_per_channel.unwrap_or(8.0) as u32;
    if let Some(raw) = &layer.raw_data {
        if raw.bits_per_channel == bits_per_channel as f64 {
            // Verbatim path: `length` is recomputed (2 compression bytes + payload)
            // because the record header must match what we are about to emit.
            let channels = raw
                .channels
                .iter()
                .map(|c| ChannelData {
                    id: c.id,
                    compression: c.compression,
                    data: c.data.clone(),
                    length: 2 + c.data.as_ref().map_or(0, Vec::len),
                })
                .collect();

            let b = bounds_or_zero(layer.top, layer.left, layer.bottom, layer.right);
            return LayerChannelData {
                layer: layer.clone(),
                channels,
                top: b.top,
                left: b.left,
                right: b.right,
                bottom: b.bottom,
                mask: layer
                    .additional_info
                    .mask
                    .as_ref()
                    .map(|m| bounds_or_zero(m.top, m.left, m.bottom, m.right)),
                real_mask: layer
                    .additional_info
                    .real_mask
                    .as_ref()
                    .map(|m| bounds_or_zero(m.top, m.left, m.bottom, m.right)),
            };
        }
    }

    let mut layer_data =
        get_layer_channels(temp_buffer, layer, background, options, psb, bits_per_channel);
    if let Some(mask) = &layer.additional_info.mask {
        get_mask_channels(temp_buffer, &mut layer_data, mask, options, psb, false, bits_per_channel);
    }
    if let Some(real_mask) = &layer.additional_info.real_mask {
        get_mask_channels(temp_buffer, &mut layer_data, real_mask, options, psb, true, bits_per_channel);
    }
    layer_data
}

/// Порт `getMaskChannels(...)`.
fn get_mask_channels(
    temp_buffer: &mut [u8],
    layer_data: &mut LayerChannelData,
    mask: &LayerMaskData,
    options: &WriteOptions,
    psb: bool,
    real_mask: bool,
    bits_per_channel: u32,
) {
    let top = mask.top.unwrap_or(0.0) as i32;
    let left = mask.left.unwrap_or(0.0) as i32;
    let (width, height) = get_layer_dimensions(mask.canvas.as_ref(), mask.image_data.as_ref());

    let image_data = mask.image_data.as_ref().or(mask.canvas.as_ref());

    if let Some(id) = image_data {
        if id.width != width || id.height != height {
            panic!("Invalid imageData dimentions");
        }
    }

    let right = left + width as i32;
    let bottom = top + height as i32;

    let (buffer, compression): (Vec<u8>, Compression) = match image_data {
        None => (Vec::new(), Compression::RleCompressed),
        Some(id) if options.compress == Some(true) => (
            write_data_zip_without_prediction_bit_depth(id, &[0], bits_per_channel)
                .unwrap_or_default(),
            Compression::ZipWithoutPrediction,
        ),
        Some(id) => (
            write_data_rle_bit_depth(temp_buffer, id, &[0], psb, bits_per_channel)
                .unwrap_or_default(),
            Compression::RleCompressed,
        ),
    };

    let length = 2 + buffer.len();
    layer_data.channels.push(ChannelData {
        id: if real_mask {
            ChannelId::RealUserMask
        } else {
            ChannelId::UserMask
        },
        compression,
        data: Some(buffer),
        length,
    });

    let bounds = ChannelBounds { top, left, right, bottom };
    if real_mask {
        layer_data.real_mask = Some(bounds);
    } else {
        layer_data.mask = Some(bounds);
    }
}

/// Порт `cropImageData(data, left, top, width, height)`.
fn crop_image_data(data: &PixelData, left: usize, top: usize, width: usize, height: usize) -> PixelData {
    let mut dst = vec![0u8; width * height * 4];
    let src = &data.data;
    let dw = data.width as usize;
    for y in 0..height {
        for x in 0..width {
            let s = ((x + left) + (y + top) * dw) * 4;
            let d = (x + y * width) * 4;
            dst[d] = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
            dst[d + 3] = src[s + 3];
        }
    }
    PixelData {
        width: width as u32,
        height: height as u32,
        data: dst,
    }
}

/// Порт `getLayerChannels(tempBuffer, layer, background, options)`.
fn get_layer_channels(
    temp_buffer: &mut [u8],
    layer: &Layer,
    background: bool,
    options: &WriteOptions,
    psb: bool,
    bits_per_channel: u32,
) -> LayerChannelData {
    let mut top = layer.top.unwrap_or(0.0) as i32;
    let mut left = layer.left.unwrap_or(0.0) as i32;
    #[allow(unused_assignments)]
    let mut right = layer.right.unwrap_or(0.0) as i32;
    #[allow(unused_assignments)]
    let mut bottom = layer.bottom.unwrap_or(0.0) as i32;

    // default empty channel set (Transparency, Color0..2), all length=2.
    let default_channels = || {
        vec![
            ChannelData { id: ChannelId::Transparency, compression: Compression::RawData, data: None, length: 2 },
            ChannelData { id: ChannelId::Color0, compression: Compression::RawData, data: None, length: 2 },
            ChannelData { id: ChannelId::Color1, compression: Compression::RawData, data: None, length: 2 },
            ChannelData { id: ChannelId::Color2, compression: Compression::RawData, data: None, length: 2 },
        ]
    };

    let (mut width, mut height) = get_layer_dimensions(layer.canvas.as_ref(), layer.image_data.as_ref());

    if (layer.canvas.is_none() && layer.image_data.is_none()) || width == 0 || height == 0 {
        right = left;
        bottom = top;
        return LayerChannelData {
            layer: layer.clone(),
            channels: default_channels(),
            top,
            left,
            right,
            bottom,
            mask: None,
            real_mask: None,
        };
    }

    right = left + width as i32;
    bottom = top + height as i32;

    let mut data: PixelData = layer
        .image_data
        .clone()
        .or_else(|| layer.canvas.clone())
        .unwrap();

    if options.trim_image_data == Some(true) {
        let trimmed = trim_data(&data);
        if trimmed.left != 0
            || trimmed.top != 0
            || trimmed.right != data.width as i32
            || trimmed.bottom != data.height as i32
        {
            left += trimmed.left;
            top += trimmed.top;
            right -= data.width as i32 - trimmed.right;
            bottom -= data.height as i32 - trimmed.bottom;
            width = (right - left) as u32;
            height = (bottom - top) as u32;

            if width == 0 || height == 0 {
                return LayerChannelData {
                    layer: layer.clone(),
                    channels: default_channels(),
                    top,
                    left,
                    right,
                    bottom,
                    mask: None,
                    real_mask: None,
                };
            }

            data = crop_image_data(
                &data,
                trimmed.left as usize,
                trimmed.top as usize,
                width as usize,
                height as usize,
            );
        }
    }

    let mut channel_ids = vec![ChannelId::Color0, ChannelId::Color1, ChannelId::Color2];

    if !background
        || options.no_background == Some(true)
        || layer.additional_info.mask.is_some()
        || has_alpha(&data)
    {
        channel_ids.insert(0, ChannelId::Transparency);
    }

    let channels: Vec<ChannelData> = channel_ids
        .into_iter()
        .map(|channel_id| {
            let offset = offset_for_channel(channel_id, false) as usize;
            let (buffer, compression): (Vec<u8>, Compression) = if options.compress == Some(true) {
                (
                    write_data_zip_without_prediction_bit_depth(&data, &[offset], bits_per_channel)
                        .unwrap_or_default(),
                    Compression::ZipWithoutPrediction,
                )
            } else {
                (
                    write_data_rle_bit_depth(temp_buffer, &data, &[offset], psb, bits_per_channel)
                        .unwrap_or_default(),
                    Compression::RleCompressed,
                )
            };
            let length = 2 + buffer.len();
            ChannelData { id: channel_id, compression, data: Some(buffer), length }
        })
        .collect();

    let _ = RAW_IMAGE_DATA; // raw image data path not modeled (RAW_IMAGE_DATA == false)

    LayerChannelData {
        layer: layer.clone(),
        channels,
        top,
        left,
        right,
        bottom,
        mask: None,
        real_mask: None,
    }
}

/// Порт `isRowEmpty`.
fn is_row_empty(data: &PixelData, y: usize, left: usize, right: usize) -> bool {
    let width = data.width as usize;
    let start = (y * width + left) * 4 + 3;
    let end = start + (right - left) * 4;
    let mut i = start;
    while i < end {
        if data.data[i] != 0 {
            return false;
        }
        i += 4;
    }
    true
}

/// Порт `isColEmpty`.
fn is_col_empty(data: &PixelData, x: usize, top: usize, bottom: usize) -> bool {
    let width = data.width as usize;
    let stride = width * 4;
    let start = top * stride + x * 4 + 3;
    let mut y = top;
    let mut i = start;
    while y < bottom {
        if data.data[i] != 0 {
            return false;
        }
        y += 1;
        i += stride;
    }
    true
}

/// Порт `trimData(data)`. Возвращает обрезанные границы в координатах данных.
fn trim_data(data: &PixelData) -> ChannelBounds {
    let mut top = 0i32;
    let mut left = 0i32;
    let mut right = data.width as i32;
    let mut bottom = data.height as i32;

    while top < bottom && is_row_empty(data, top as usize, left as usize, right as usize) {
        top += 1;
    }
    while bottom > top && is_row_empty(data, (bottom - 1) as usize, left as usize, right as usize) {
        bottom -= 1;
    }
    while left < right && is_col_empty(data, left as usize, top as usize, bottom as usize) {
        left += 1;
    }
    while right > left && is_col_empty(data, (right - 1) as usize, top as usize, bottom as usize) {
        right -= 1;
    }

    ChannelBounds { top, left, right, bottom }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::{Cmyk, Grayscale, Hsb, Lab, Rgb};

    #[test]
    fn scalars_big_endian() {
        let mut w = create_writer_default();
        write_uint8(&mut w, 0x12);
        write_uint16(&mut w, 0x1234);
        write_int16(&mut w, -2);
        write_uint32(&mut w, 0x12345678);
        write_int32(&mut w, -1);
        let buf = get_writer_buffer(&w);
        assert_eq!(
            buf,
            vec![
                0x12, // u8
                0x12, 0x34, // u16 BE
                0xff, 0xfe, // i16 BE (-2)
                0x12, 0x34, 0x56, 0x78, // u32 BE
                0xff, 0xff, 0xff, 0xff, // i32 BE (-1)
            ]
        );
    }

    #[test]
    fn le_variants() {
        let mut w = create_writer_default();
        write_uint16_le(&mut w, 0x1234);
        write_int32_le(&mut w, 0x12345678);
        assert_eq!(
            get_writer_buffer(&w),
            vec![0x34, 0x12, 0x78, 0x56, 0x34, 0x12]
        );
    }

    #[test]
    fn floats_big_endian() {
        let mut w = create_writer_default();
        write_float32(&mut w, 1.0_f32);
        write_float64(&mut w, 1.0_f64);
        assert_eq!(
            get_writer_buffer(&w),
            vec![
                0x3f, 0x80, 0x00, 0x00, // f32 1.0 BE
                0x3f, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // f64 1.0 BE
            ]
        );
    }

    #[test]
    fn signature_and_zeros() {
        let mut w = create_writer_default();
        write_signature(&mut w, "8BPS");
        write_zeros(&mut w, 3);
        assert_eq!(get_writer_buffer(&w), vec![b'8', b'B', b'P', b'S', 0, 0, 0]);
    }

    #[test]
    fn pascal_string_padding() {
        // "ab" -> [len=2, 'a', 'b'] then pad: length becomes 3, 3%4!=0 -> write 0 (4),
        // 4%4==0 stop. Total bytes: 1 + 2 + 1 = 4.
        let mut w = create_writer_default();
        write_pascal_string(&mut w, "ab", 4);
        assert_eq!(get_writer_buffer(&w), vec![2, b'a', b'b', 0]);
    }

    #[test]
    fn pascal_string_empty_pad2() {
        // "" -> [0], length 0 -> ++length=1, 1%2!=0 -> write 0 (2), 2%2==0 stop.
        let mut w = create_writer_default();
        write_pascal_string(&mut w, "", 2);
        assert_eq!(get_writer_buffer(&w), vec![0, 0]);
    }

    #[test]
    fn pascal_string_non_ascii_becomes_question() {
        let mut w = create_writer_default();
        write_pascal_string(&mut w, "é", 1); // 1 char, code > 127 -> '?'
        // len=1, '?'; ++length=2, 2%1==0 stop.
        assert_eq!(get_writer_buffer(&w), vec![1, b'?']);
    }

    #[test]
    fn unicode_string_layout() {
        let mut w = create_writer_default();
        write_unicode_string(&mut w, "AB");
        assert_eq!(
            get_writer_buffer(&w),
            vec![
                0x00, 0x00, 0x00, 0x02, // length = 2 (u32 BE)
                0x00, b'A', 0x00, b'B', // UTF-16 BE
            ]
        );
    }

    #[test]
    fn unicode_string_with_padding_layout() {
        let mut w = create_writer_default();
        write_unicode_string_with_padding(&mut w, "A");
        assert_eq!(
            get_writer_buffer(&w),
            vec![
                0x00, 0x00, 0x00, 0x02, // length+1 = 2
                0x00, b'A', // UTF-16 BE
                0x00, 0x00, // terminating 0
            ]
        );
    }

    #[test]
    fn section_backpatch_and_rounding() {
        // round=4, body = 3 bytes -> length 3, padded to 4. writeTotalLength=false
        // so the patched length is the un-padded value (3).
        let mut w = create_writer_default();
        write_section(
            &mut w,
            4,
            |w| {
                write_uint8(w, 0xaa);
                write_uint8(w, 0xbb);
                write_uint8(w, 0xcc);
            },
            false,
            false,
        );
        assert_eq!(
            get_writer_buffer(&w),
            vec![
                0x00, 0x00, 0x00, 0x03, // length = 3 (un-padded, writeTotalLength=false)
                0xaa, 0xbb, 0xcc, // body
                0x00, // padding to round=4
            ]
        );
    }

    #[test]
    fn section_total_length() {
        // writeTotalLength=true -> patched length includes padding (4).
        let mut w = create_writer_default();
        write_section(
            &mut w,
            4,
            |w| {
                write_uint8(w, 0xaa);
                write_uint8(w, 0xbb);
                write_uint8(w, 0xcc);
            },
            true,
            false,
        );
        assert_eq!(
            get_writer_buffer(&w),
            vec![0x00, 0x00, 0x00, 0x04, 0xaa, 0xbb, 0xcc, 0x00]
        );
    }

    #[test]
    fn section_large() {
        // large=true -> leading extra u32(0), then the length word. body=1 byte, round=2.
        let mut w = create_writer_default();
        write_section(&mut w, 2, |w| write_uint8(w, 0x7f), false, true);
        assert_eq!(
            get_writer_buffer(&w),
            vec![
                0x00, 0x00, 0x00, 0x00, // large leading u32
                0x00, 0x00, 0x00, 0x01, // length = 1 (un-padded)
                0x7f, // body
                0x00, // padding to round=2
            ]
        );
    }

    #[test]
    fn buffer_growth_doubling() {
        // Start at size 4, write 10 bytes -> 4 -> 8 -> 16.
        let mut w = create_writer(4);
        for i in 0..10u8 {
            write_uint8(&mut w, i);
        }
        assert_eq!(w.offset, 10);
        assert_eq!(w.buffer.len(), 16);
        assert_eq!(
            get_writer_buffer(&w),
            vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
        );
    }

    #[test]
    fn write_bytes_grows_and_appends() {
        let mut w = create_writer(2);
        write_bytes(&mut w, Some(&[1, 2, 3, 4, 5]));
        write_bytes(&mut w, None);
        assert_eq!(get_writer_buffer(&w), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn color_none_is_rgb_zeros() {
        let mut w = create_writer_default();
        write_color(&mut w, None);
        assert_eq!(
            get_writer_buffer(&w),
            vec![0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0] // ColorSpace::Rgb (0) + 8 zeros
        );
    }

    #[test]
    fn color_rgb() {
        let mut w = create_writer_default();
        let c = Color::Rgb(Rgb {
            r: 255.0,
            g: 0.0,
            b: 128.0,
        });
        write_color(&mut w, Some(&c));
        // 255*257 = 65535 = 0xffff, 0, 128*257 = 32896 = 0x8080, then 0
        assert_eq!(
            get_writer_buffer(&w),
            vec![
                0x00, 0x00, // ColorSpace::Rgb
                0xff, 0xff, // r
                0x00, 0x00, // g
                0x80, 0x80, // b
                0x00, 0x00, // pad
            ]
        );
    }

    #[test]
    fn color_cmyk_lab_hsb_grayscale_color_space_codes() {
        let mut w = create_writer_default();
        write_color(&mut w, Some(&Color::Cmyk(Cmyk { c: 0.0, m: 0.0, y: 0.0, k: 0.0 })));
        write_color(&mut w, Some(&Color::Lab(Lab { l: 0.0, a: 0.0, b: 0.0 })));
        write_color(&mut w, Some(&Color::Hsb(Hsb { h: 0.0, s: 0.0, b: 0.0 })));
        write_color(&mut w, Some(&Color::Grayscale(Grayscale { k: 0.0 })));
        let buf = get_writer_buffer(&w);
        // First u16 of each block is the ColorSpace code.
        assert_eq!(&buf[0..2], &[0x00, 0x02]); // Cmyk = 2
        assert_eq!(&buf[10..12], &[0x00, 0x07]); // Lab = 7
        assert_eq!(&buf[20..22], &[0x00, 0x01]); // Hsb = 1
        assert_eq!(&buf[30..32], &[0x00, 0x08]); // Grayscale = 8
    }

    // -----------------------------------------------------------------------
    // Round-trip tests: read a real fixture -> write_psd -> read again,
    // assert structural equality (the bar for this task).
    // -----------------------------------------------------------------------

    use crate::psd::{BlendMode, Layer, Psd, ReadOptions};
    use crate::reader::read_psd;

    /// Path of the upstream read fixture `rel` (`test/read/<rel>/src.psd`).
    ///
    /// The fixture tree lives outside the crate directory and is therefore
    /// absent from the published package, so callers must treat a missing file
    /// as "skip", not "fail".
    fn fixture_path(rel: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(format!(
            "{}/../../test/ag-psd/test/read/{}/src.psd",
            env!("CARGO_MANIFEST_DIR"),
            rel
        ))
    }

    /// Read fixture `rel`, or `None` when the fixture tree is not checked out.
    ///
    /// Reads with `use_image_data` so layers carry `image_data` (byte planes)
    /// for pixel comparison instead of the (cloned) canvas. A fixture that
    /// exists but fails to parse still panics — only absence is tolerated.
    fn read_fixture(rel: &str) -> Option<Psd> {
        let path = fixture_path(rel);
        if !path.exists() {
            eprintln!("fixture {} not present, skipping", path.display());
            return None;
        }
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        let opts = ReadOptions { use_image_data: Some(true), ..Default::default() };
        Some(read_psd(&bytes, &opts).unwrap_or_else(|e| panic!("read_psd {}: {:?}", rel, e)))
    }

    fn count_layers(layers: &[Layer]) -> usize {
        layers
            .iter()
            .map(|l| 1 + l.children.as_ref().map_or(0, |c| count_layers(c)))
            .sum()
    }

    /// Flatten the tree depth-first into (name, top, left, bottom, right,
    /// opacity, blend_mode) tuples for structural comparison. Folders are
    /// included; group structure is preserved via recursion order.
    fn flatten(layers: &[Layer], out: &mut Vec<(String, i64, i64, i64, i64, u8, BlendMode)>) {
        for l in layers {
            out.push((
                l.additional_info.name.clone().unwrap_or_default(),
                l.top.unwrap_or(0.0) as i64,
                l.left.unwrap_or(0.0) as i64,
                l.bottom.unwrap_or(0.0) as i64,
                l.right.unwrap_or(0.0) as i64,
                (l.opacity.unwrap_or(1.0) * 255.0).round() as u8,
                l.blend_mode.unwrap_or(BlendMode::Normal),
            ));
            if let Some(children) = &l.children {
                flatten(children, out);
            }
        }
    }

    fn round_trip(psd: &Psd) -> Psd {
        let bytes = write_psd(psd, &WriteOptions::default());
        let opts = ReadOptions { use_image_data: Some(true), ..Default::default() };
        read_psd(&bytes, &opts).expect("re-read written psd")
    }

    fn assert_structural_eq(a: &Psd, b: &Psd) {
        assert_eq!(a.width, b.width, "width");
        assert_eq!(a.height, b.height, "height");
        assert_eq!(a.color_mode, b.color_mode, "color_mode");

        let ca = a.children.as_deref().unwrap_or(&[]);
        let cb = b.children.as_deref().unwrap_or(&[]);
        assert_eq!(ca.len(), cb.len(), "top-level children count");
        assert_eq!(count_layers(ca), count_layers(cb), "total layer count");

        let mut fa = Vec::new();
        let mut fb = Vec::new();
        flatten(ca, &mut fa);
        flatten(cb, &mut fb);
        assert_eq!(fa, fb, "per-layer name/bounds/opacity/blendMode");
    }

    /// Total bytes of layer image data across the tree (for survival check).
    fn total_pixel_bytes(layers: &[Layer]) -> usize {
        layers
            .iter()
            .map(|l| {
                let own = l.image_data.as_ref().map_or(0, |d| d.data.len());
                own + l.children.as_ref().map_or(0, |c| total_pixel_bytes(c))
            })
            .sum()
    }

    #[test]
    fn round_trip_layers_fixture() {
        let Some(orig) = read_fixture("layers") else { return };
        let again = round_trip(&orig);
        assert_structural_eq(&orig, &again);

        // Layer pixel data must survive the round trip.
        let ca = orig.children.as_deref().unwrap_or(&[]);
        let cb = again.children.as_deref().unwrap_or(&[]);
        assert!(total_pixel_bytes(ca) > 0, "fixture should have layer pixels");
        assert_eq!(
            total_pixel_bytes(ca),
            total_pixel_bytes(cb),
            "layer pixel byte totals survive round trip"
        );
    }

    #[test]
    fn round_trip_groups_fixture() {
        let Some(orig) = read_fixture("groups") else { return };
        let again = round_trip(&orig);
        assert_structural_eq(&orig, &again);

        let ca = orig.children.as_deref().unwrap_or(&[]);
        let cb = again.children.as_deref().unwrap_or(&[]);
        assert_eq!(
            total_pixel_bytes(ca),
            total_pixel_bytes(cb),
            "layer pixel byte totals survive round trip"
        );
    }

    /// Build an RGBA `PixelData` whose R/G/B carry `value(x, y)` and A is 255.
    ///
    /// Masks are single-channel in the file format; the reader materialises them
    /// as grayscale RGBA (`setup_grayscale` + `reset_alpha`), so a mask built
    /// this way compares byte-for-byte after a round trip.
    fn gray_pattern(w: u32, h: u32, value: impl Fn(u32, u32) -> u8) -> PixelData {
        let mut data = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in 0..h {
            for x in 0..w {
                let v = value(x, y);
                let i = ((y as usize) * (w as usize) + (x as usize)) * 4;
                data[i] = v;
                data[i + 1] = v;
                data[i + 2] = v;
                data[i + 3] = 255;
            }
        }
        PixelData { width: w, height: h, data }
    }

    #[test]
    fn round_trip_mask_larger_than_layer_bitmap() {
        // Regression: the RLE scratch buffer used to be sized from layer
        // bitmaps only (and only for layers that had one), so a mask bigger
        // than both its layer and the composite silently produced truncated
        // RLE data. Document 8x8, layer bitmap 2x2, mask 64x64.
        let layer_bitmap = gray_pattern(2, 2, |x, y| (x * 40 + y * 80) as u8);
        // A high-entropy pattern so RLE cannot compress the mask down into the
        // (previously undersized) scratch buffer.
        let mask_bitmap = gray_pattern(64, 64, |x, y| ((x * 7 + y * 13) % 251) as u8);

        let mut layer = Layer::default();
        layer.additional_info.name = Some("masked".to_string());
        layer.top = Some(0.0);
        layer.left = Some(0.0);
        layer.bottom = Some(2.0);
        layer.right = Some(2.0);
        layer.image_data = Some(layer_bitmap);
        layer.additional_info.mask = Some(LayerMaskData {
            top: Some(0.0),
            left: Some(0.0),
            bottom: Some(64.0),
            right: Some(64.0),
            image_data: Some(mask_bitmap.clone()),
            ..Default::default()
        });

        let psd = Psd {
            width: 8.0,
            height: 8.0,
            color_mode: Some(ColorMode::Rgb),
            bits_per_channel: Some(8.0),
            children: Some(vec![layer]),
            ..Default::default()
        };

        let again = round_trip(&psd);
        let children = again.children.as_ref().expect("children");
        assert_eq!(children.len(), 1, "one layer survives");
        let mask = children[0]
            .additional_info
            .mask
            .as_ref()
            .expect("mask survives the round trip");
        assert_eq!(mask.right.map(|v| v - mask.left.unwrap_or(0.0)), Some(64.0));
        assert_eq!(mask.bottom.map(|v| v - mask.top.unwrap_or(0.0)), Some(64.0));
        let got = mask.image_data.as_ref().expect("mask image data");
        assert_eq!((got.width, got.height), (64, 64), "mask dimensions");
        assert_eq!(got.data, mask_bitmap.data, "mask pixels survive round trip");
    }

    /// Round trip a document through `write_psd` with the PSB flag set.
    fn round_trip_psb(psd: &Psd) -> Psd {
        let bytes = write_psd(psd, &WriteOptions { psb: Some(true), ..Default::default() });
        // The reader picks PSB up from the file header version, so no read-side flag.
        let opts = ReadOptions { use_image_data: Some(true), ..Default::default() };
        read_psd(&bytes, &opts).expect("re-read written psb")
    }

    #[test]
    fn psb_layer_and_mask_channels_are_not_truncated() {
        // Regression for the PSB RLE scratch shortfall: the per-row length table
        // is 4 bytes per row in PSB but the estimate reserved 2, so a tall,
        // narrow channel (where the table dominates the payload) overflowed the
        // shared scratch buffer. `write_data_rle` drops out-of-bounds writes, so
        // the result was a structurally valid PSB with the pixel payload missing.
        //
        // 1x256 layer + 1x256 mask need 256 * (4 + 2) = 1536 bytes each; the old
        // estimate gave 256 * (2 + 2) = 1024. The document is kept at 1x1 so the
        // composite estimate (10 bytes then, 24 now) cannot mask the shortfall.
        let layer_bitmap = gray_pattern(1, 256, |_, y| (y % 251) as u8);
        let mask_bitmap = gray_pattern(1, 256, |_, y| ((y * 7 + 3) % 251) as u8);

        let mut layer = Layer::default();
        layer.additional_info.name = Some("tall".to_string());
        layer.top = Some(0.0);
        layer.left = Some(0.0);
        layer.bottom = Some(256.0);
        layer.right = Some(1.0);
        layer.image_data = Some(layer_bitmap.clone());
        layer.additional_info.mask = Some(LayerMaskData {
            top: Some(0.0),
            left: Some(0.0),
            bottom: Some(256.0),
            right: Some(1.0),
            image_data: Some(mask_bitmap.clone()),
            ..Default::default()
        });

        let psd = Psd {
            width: 1.0,
            height: 1.0,
            color_mode: Some(ColorMode::Rgb),
            bits_per_channel: Some(8.0),
            children: Some(vec![layer]),
            ..Default::default()
        };

        let again = round_trip_psb(&psd);
        let children = again.children.as_ref().expect("children");
        assert_eq!(children.len(), 1, "one layer survives");

        let got = children[0].image_data.as_ref().expect("layer image data");
        assert_eq!((got.width, got.height), (1, 256), "layer dimensions");
        // Only RGB survives: the layer has no alpha channel of its own here, and
        // the writer emits one only when alpha is non-opaque, so compare colors.
        for y in 0..256usize {
            assert_eq!(
                &got.data[y * 4..y * 4 + 3],
                &layer_bitmap.data[y * 4..y * 4 + 3],
                "layer pixel row {y} survives the psb round trip"
            );
        }

        let mask = children[0]
            .additional_info
            .mask
            .as_ref()
            .expect("mask survives the round trip");
        let got_mask = mask.image_data.as_ref().expect("mask image data");
        assert_eq!((got_mask.width, got_mask.height), (1, 256), "mask dimensions");
        assert_eq!(got_mask.data, mask_bitmap.data, "mask pixels survive the psb round trip");
    }

    #[test]
    fn psb_composite_channels_are_not_truncated() {
        // Same shortfall on the composite path, which encodes all channels in a
        // single `write_data_rle` call: 3 channels of a 1x256 document need
        // 3 * 256 * (4 + 2) = 4608 bytes, the old estimate gave 2560. No layers,
        // so nothing else can size the scratch buffer up.
        let composite = gray_pattern(1, 256, |_, y| ((y * 11 + 5) % 251) as u8);

        let psd = Psd {
            width: 1.0,
            height: 256.0,
            color_mode: Some(ColorMode::Rgb),
            bits_per_channel: Some(8.0),
            image_data: Some(composite.clone()),
            ..Default::default()
        };

        let again = round_trip_psb(&psd);
        let got = again.image_data.as_ref().expect("composite image data");
        assert_eq!((got.width, got.height), (1, 256), "composite dimensions");
        assert_eq!(got.data, composite.data, "composite pixels survive the psb round trip");
    }

    #[test]
    fn largest_layer_size_measures_masks_and_bitmapless_layers() {
        // A layer with no bitmap of its own but a 64x64 mask must still size the
        // scratch buffer; previously such a layer was skipped entirely.
        let mut layer = Layer::default();
        layer.additional_info.mask = Some(LayerMaskData {
            image_data: Some(gray_pattern(64, 64, |_, _| 0)),
            ..Default::default()
        });
        // 2 * 64 + 2 * 64 * 64
        assert_eq!(get_largest_layer_size(Some(&[layer.clone()]), false, 8), 8320);
        // PSB row lengths are 4 bytes wide: 4 * 64 + 2 * 64 * 64
        assert_eq!(get_largest_layer_size(Some(&[layer]), true, 8), 8448);

        // real_mask counts as well, and the maximum wins over the layer bitmap.
        let layer = Layer {
            image_data: Some(gray_pattern(4, 4, |_, _| 0)),
            additional_info: LayerAdditionalInfo {
                real_mask: Some(LayerMaskData {
                    image_data: Some(gray_pattern(32, 16, |_, _| 0)),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        // max(2*4 + 2*4*4, 2*16 + 2*32*16)
        assert_eq!(get_largest_layer_size(Some(&[layer]), false, 8), 1056);

        // The recursion into groups still applies.
        let child = Layer {
            image_data: Some(gray_pattern(10, 10, |_, _| 0)),
            ..Default::default()
        };
        let group = Layer {
            children: Some(vec![child]),
            ..Default::default()
        };
        assert_eq!(get_largest_layer_size(Some(&[group]), false, 8), 220);

        assert_eq!(get_largest_layer_size(None, false, 8), 0);
        assert_eq!(get_largest_layer_size(None, true, 8), 0);
    }

    #[test]
    fn rle_scratch_size_covers_the_psb_row_length_table() {
        // A 1x1 PSB channel needs a 4-byte row length plus 2 bytes of encoded
        // data. Upstream's `2 * height + 2 * width * height` yields 4 and
        // truncates the channel; ours must not.
        assert_eq!(rle_scratch_size(1, 1, 1, true, 8), 6);
        assert_eq!(rle_scratch_size(1, 1, 1, false, 8), 4);

        // A 1x1 three-channel PSB composite needs 3 * (4 + 2) = 18 bytes;
        // upstream's `4 * 2 * w * h + 2 * h` yields 10.
        assert_eq!(rle_scratch_size(1, 1, 3, true, 8), 18);

        // Saturating: an absurd bitmap must not wrap the size.
        assert_eq!(rle_scratch_size(u32::MAX, u32::MAX, 4, true, 8), usize::MAX);
    }

    #[test]
    fn global_layer_mask_info_opacity_is_rounded() {
        // 0.5 * 0xff == 127.5; truncation would emit 127 and break the round
        // trip through the reader (which divides by 0xff again).
        let mut w = create_writer_default();
        let info = GlobalLayerMaskInfo {
            overlay_color_space: 0.0,
            color_space1: 0.0,
            color_space2: 0.0,
            color_space3: 0.0,
            color_space4: 0.0,
            opacity: 0.5,
            kind: 0.0,
        };
        write_global_layer_mask_info(&mut w, Some(&info));
        let buf = get_writer_buffer(&w);
        // 4 bytes section length + 5 color-space u16 -> opacity u16 at [14..16].
        assert_eq!(&buf[14..16], &[0x00, 128], "opacity is rounded, not truncated");
    }

    #[test]
    fn raw_data_channels_are_written_verbatim() {
        // Reading with `use_raw_data` leaves the undecoded channel payloads on
        // the layers; the writer must emit them as-is instead of treating the
        // layers as empty (which is what happened before the fast path existed).
        let path = fixture_path("layers");
        if !path.exists() {
            eprintln!("fixture {} not present, skipping", path.display());
            return;
        }
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
        let raw = read_psd(
            &bytes,
            &ReadOptions { use_raw_data: Some(true), ..Default::default() },
        )
        .expect("read with use_raw_data");

        let raw_children = raw.children.as_deref().expect("children");
        assert!(
            raw_children.iter().any(|l| l.raw_data.is_some()),
            "fixture layers should carry raw_data"
        );

        let written = write_psd(&raw, &WriteOptions::default());
        let again = read_psd(
            &written,
            &ReadOptions { use_image_data: Some(true), ..Default::default() },
        )
        .expect("re-read verbatim-written psd");

        // The fixture was read above, so its absence was already handled.
        let orig = read_fixture("layers").expect("fixture present");
        assert_structural_eq(&orig, &again);

        let ca = orig.children.as_deref().unwrap_or(&[]);
        let cb = again.children.as_deref().unwrap_or(&[]);
        assert!(total_pixel_bytes(ca) > 0, "fixture should have layer pixels");
        assert_eq!(
            total_pixel_bytes(ca),
            total_pixel_bytes(cb),
            "verbatim raw channels decode to the same pixel volume"
        );

        // Pixel-exact comparison: the verbatim path must not alter a single byte.
        let mut pa = Vec::new();
        let mut pb = Vec::new();
        collect_pixels(ca, &mut pa);
        collect_pixels(cb, &mut pb);
        assert_eq!(pa, pb, "verbatim raw channels decode to identical pixels");
    }

    /// Depth-first collect of every layer's decoded pixel bytes.
    fn collect_pixels(layers: &[Layer], out: &mut Vec<u8>) {
        for l in layers {
            if let Some(d) = &l.image_data {
                out.extend_from_slice(&d.data);
            }
            if let Some(children) = &l.children {
                collect_pixels(children, out);
            }
        }
    }

    #[test]
    fn round_trip_synthetic_two_solid_layers() {
        // Build a small Psd: 4x4 document, 2 solid-color layers each 4x4.
        fn solid(w: u32, h: u32, rgba: [u8; 4]) -> PixelData {
            let mut data = vec![0u8; (w * h * 4) as usize];
            for px in data.chunks_mut(4) {
                px.copy_from_slice(&rgba);
            }
            PixelData { width: w, height: h, data }
        }

        let mut red = Layer::default();
        red.additional_info.name = Some("red".to_string());
        red.top = Some(0.0);
        red.left = Some(0.0);
        red.bottom = Some(4.0);
        red.right = Some(4.0);
        red.opacity = Some(1.0);
        red.blend_mode = Some(BlendMode::Normal);
        red.image_data = Some(solid(4, 4, [255, 0, 0, 255]));

        let mut blue = Layer::default();
        blue.additional_info.name = Some("blue".to_string());
        blue.top = Some(0.0);
        blue.left = Some(0.0);
        blue.bottom = Some(4.0);
        blue.right = Some(4.0);
        blue.opacity = Some(0.5);
        blue.blend_mode = Some(BlendMode::Multiply);
        blue.image_data = Some(solid(4, 4, [0, 0, 255, 200]));

        let psd = Psd {
            width: 4.0,
            height: 4.0,
            color_mode: Some(ColorMode::Rgb),
            bits_per_channel: Some(8.0),
            children: Some(vec![red, blue]),
            ..Default::default()
        };

        let again = round_trip(&psd);

        assert_eq!(again.width, 4.0);
        assert_eq!(again.height, 4.0);
        assert_eq!(again.color_mode, Some(ColorMode::Rgb));

        let children = again.children.as_ref().expect("children");
        assert_eq!(children.len(), 2, "two layers survive");

        let red_back = &children[0];
        assert_eq!(red_back.additional_info.name.as_deref(), Some("red"));
        assert_eq!(red_back.blend_mode, Some(BlendMode::Normal));
        assert_eq!(red_back.opacity.map(|o| (o * 255.0).round() as u8), Some(255));
        assert_eq!(red_back.bottom, Some(4.0));
        assert_eq!(red_back.right, Some(4.0));
        // first pixel should be red, fully opaque
        let rd = red_back.image_data.as_ref().expect("red image data");
        assert_eq!(&rd.data[0..4], &[255, 0, 0, 255]);

        let blue_back = &children[1];
        assert_eq!(blue_back.additional_info.name.as_deref(), Some("blue"));
        assert_eq!(blue_back.blend_mode, Some(BlendMode::Multiply));
        assert_eq!(blue_back.opacity.map(|o| (o * 255.0).round() as u8), Some(128));
        let bd = blue_back.image_data.as_ref().expect("blue image data");
        assert_eq!(&bd.data[0..4], &[0, 0, 255, 200]);
    }

    #[test]
    fn round_trip_rgb_high_bit_depths_and_compressions() {
        let samples = [0u8, 1, 127, 255];
        let pixels = PixelData {
            width: 4,
            height: 1,
            data: samples
                .into_iter()
                .flat_map(|v| [v, v, v, 255])
                .collect(),
        };
        for depth in [16.0, 32.0] {
            for (psb, compress) in [(false, false), (true, false), (false, true)] {
                let mut layer = Layer::default();
                layer.top = Some(0.0);
                layer.left = Some(0.0);
                layer.bottom = Some(1.0);
                layer.right = Some(4.0);
                layer.image_data = Some(pixels.clone());
                layer.additional_info.mask = Some(LayerMaskData {
                    top: Some(0.0),
                    left: Some(0.0),
                    bottom: Some(1.0),
                    right: Some(4.0),
                    image_data: Some(pixels.clone()),
                    ..Default::default()
                });
                layer.additional_info.real_mask = Some(LayerMaskData {
                    top: Some(0.0),
                    left: Some(0.0),
                    bottom: Some(1.0),
                    right: Some(4.0),
                    image_data: Some(pixels.clone()),
                    ..Default::default()
                });
                let psd = Psd {
                    width: 4.0,
                    height: 1.0,
                    color_mode: Some(ColorMode::Rgb),
                    bits_per_channel: Some(depth),
                    image_data: Some(pixels.clone()),
                    children: Some(vec![layer]),
                    ..Default::default()
                };
                let bytes = write_psd(
                    &psd,
                    &WriteOptions {
                        psb: Some(psb),
                        compress: Some(compress),
                        ..Default::default()
                    },
                );
                let again = read_psd(
                    &bytes,
                    &ReadOptions { use_image_data: Some(true), ..Default::default() },
                )
                .expect("high-bit PSD/PSB round trip");
                assert_eq!(again.bits_per_channel, Some(depth));
                assert_eq!(again.width, 4.0);
                assert_eq!(again.height, 1.0);
                assert_eq!(again.image_data.as_ref().unwrap().data, pixels.data);
                let child = &again.children.as_ref().unwrap()[0];
                assert_eq!(child.image_data.as_ref().unwrap().data, pixels.data);
                assert_eq!(child.additional_info.mask.as_ref().unwrap().image_data.as_ref().unwrap().data, pixels.data);
                assert_eq!(child.additional_info.real_mask.as_ref().unwrap().image_data.as_ref().unwrap().data, pixels.data);
            }
        }
    }

    #[test]
    fn high_bit_raw_layer_channels_are_written_verbatim() {
        let pixels = gray_pattern(2, 1, |x, _| if x == 0 { 17 } else { 231 });
        for depth in [16.0, 32.0] {
            let mut layer = Layer::default();
            layer.top = Some(0.0);
            layer.left = Some(0.0);
            layer.bottom = Some(1.0);
            layer.right = Some(2.0);
            layer.image_data = Some(pixels.clone());
            let psd = Psd {
                width: 2.0,
                height: 1.0,
                color_mode: Some(ColorMode::Rgb),
                bits_per_channel: Some(depth),
                image_data: Some(pixels.clone()),
                children: Some(vec![layer]),
                ..Default::default()
            };
            let bytes = write_psd(&psd, &WriteOptions { compress: Some(false), ..Default::default() });
            let raw = read_psd(
                &bytes,
                &ReadOptions { use_raw_data: Some(true), ..Default::default() },
            )
            .expect("read high-bit layer as raw data");
            let original = raw.children.as_ref().unwrap()[0].raw_data.as_ref().unwrap();
            let rewritten = write_psd(
                &raw,
                &WriteOptions {
                    compress: Some(true),
                    ..Default::default()
                },
            );
            let reread = read_psd(
                &rewritten,
                &ReadOptions { use_raw_data: Some(true), ..Default::default() },
            )
            .expect("re-read verbatim high-bit layer");
            let roundtripped = reread.children.as_ref().unwrap()[0].raw_data.as_ref().unwrap();
            assert_eq!(original.bits_per_channel, depth);
            assert_eq!(original.channels.len(), roundtripped.channels.len());
            for (a, b) in original.channels.iter().zip(&roundtripped.channels) {
                assert_eq!(a.id, b.id);
                assert_eq!(a.compression, b.compression);
                assert_eq!(a.data, b.data);
            }
        }
    }

    #[test]
    #[should_panic(expected = "bitsPerChannel must be 8, 16, or 32")]
    fn writer_rejects_unsupported_bit_depth() {
        let psd = Psd {
            width: 1.0,
            height: 1.0,
            bits_per_channel: Some(12.0),
            ..Default::default()
        };
        write_psd(&psd, &WriteOptions::default());
    }
}
