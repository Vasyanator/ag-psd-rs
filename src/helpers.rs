/*
File: crates/ag-psd/src/helpers.rs

Purpose:
общие хелперы крейта: числовые/цветовые утилиты, таблицы соответствия blend mode
<-> 4-символьный ключ, layerColors, упаковка/распаковка данных каналов
(raw / RLE / zip-without-prediction), а также заглушки canvas-уровня.

Channel bit depths:
The `*_bit_depth` writers (`write_data_raw_bit_depth`, `write_data_rle_bit_depth`,
`write_data_zip_without_prediction_bit_depth`) expand the crate's RGBA8 model to
the 8/16/32-bit big-endian samples a PSD channel stores; `expand_channel_samples`
is the single place that mapping lives. `write_data_rle_bit_depth` reports
unrepresentable input as a typed `RleEncodeError` instead of emitting a shorter,
silently corrupt channel, and its high-depth path allocates its own exactly
sized output rather than borrowing the shared writer scratch buffer.
ZIP output is always zlib-wrapped, matching upstream's pako `deflate`; the
reader (`reader::inflate_channel_stream`) additionally tolerates bare DEFLATE.

Source compatibility:
- порт upstream-файла `test/ag-psd/src/helpers.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.

Descriptor enum decoding (`EnumCodec`, `enum_long_form_to_key`):
Photoshop 2026 writes descriptor enum values in long form (the map KEY, e.g.
`BlnM.normal`, camelCased when the key is multi-word: `BlnM.colorBurn`) instead of
the historical 4-character code (`BlnM.Nrml`). `EnumCodec::decode` accepts code,
key and camelCased key, in that order, before erroring; `enum_long_form_to_key`
is the shared camelCase -> spaced-lowercase normalizer, also used by the typed
enum tables in `additional_info::effects_keys`.

PORT STATUS: ported except browser-canvas concerns
  (create_canvas / create_image_data / image_data_to_canvas / create_canvas_from_data /
   initialize_canvas стабированы под модель PixelData, см. пометки ниже).
*/

use std::collections::HashMap;

use flate2::{write::ZlibEncoder, Compression as FlateCompression};
use std::io::Write as _;

use crate::psd::{BlendMode, ChannelId, Compression, Layer, LayerColor, PixelData};

/// upstream: `export const MOCK_HANDLERS = false;`
pub const MOCK_HANDLERS: bool = false;
/// upstream: `export const RAW_IMAGE_DATA = false;`
pub const RAW_IMAGE_DATA: bool = false;

// ===========================================================================
// Blend mode <-> 4-char key mapping tables
// ===========================================================================

/// upstream `toBlendMode`: 4-символьный ключ -> BlendMode.
/// Сохранены ВСЕ записи и точные ключи (включая пробелы), это критично для формата.
pub fn to_blend_mode(key: &str) -> Option<BlendMode> {
    Some(match key {
        "pass" => BlendMode::PassThrough,
        "norm" => BlendMode::Normal,
        "diss" => BlendMode::Dissolve,
        "dark" => BlendMode::Darken,
        "mul " => BlendMode::Multiply,
        "idiv" => BlendMode::ColorBurn,
        "lbrn" => BlendMode::LinearBurn,
        "dkCl" => BlendMode::DarkerColor,
        "lite" => BlendMode::Lighten,
        "scrn" => BlendMode::Screen,
        "div " => BlendMode::ColorDodge,
        "lddg" => BlendMode::LinearDodge,
        "lgCl" => BlendMode::LighterColor,
        "over" => BlendMode::Overlay,
        "sLit" => BlendMode::SoftLight,
        "hLit" => BlendMode::HardLight,
        "vLit" => BlendMode::VividLight,
        "lLit" => BlendMode::LinearLight,
        "pLit" => BlendMode::PinLight,
        "hMix" => BlendMode::HardMix,
        "diff" => BlendMode::Difference,
        "smud" => BlendMode::Exclusion,
        "fsub" => BlendMode::Subtract,
        "fdiv" => BlendMode::Divide,
        "hue " => BlendMode::Hue,
        "sat " => BlendMode::Saturation,
        "colr" => BlendMode::Color,
        "lum " => BlendMode::Luminosity,
        _ => return None,
    })
}

/// upstream `fromBlendMode` (построен через
/// `Object.keys(toBlendMode).forEach(key => fromBlendMode[toBlendMode[key]] = key)`):
/// BlendMode -> 4-символьный ключ. Это обратное отображение `to_blend_mode`.
///
/// Returns `None` for the descriptor-only modes (`linear height`, `height`,
/// `subtraction`), which have no entry in the legacy signature table — the JS
/// dictionary lookup yields `undefined` there and every call site substitutes its own
/// default (`'norm'`, or `'pass'` for a section divider).
#[must_use]
pub fn from_blend_mode(mode: BlendMode) -> Option<&'static str> {
    Some(match mode {
        BlendMode::PassThrough => "pass",
        BlendMode::Normal => "norm",
        BlendMode::Dissolve => "diss",
        BlendMode::Darken => "dark",
        BlendMode::Multiply => "mul ",
        BlendMode::ColorBurn => "idiv",
        BlendMode::LinearBurn => "lbrn",
        BlendMode::DarkerColor => "dkCl",
        BlendMode::Lighten => "lite",
        BlendMode::Screen => "scrn",
        BlendMode::ColorDodge => "div ",
        BlendMode::LinearDodge => "lddg",
        BlendMode::LighterColor => "lgCl",
        BlendMode::Overlay => "over",
        BlendMode::SoftLight => "sLit",
        BlendMode::HardLight => "hLit",
        BlendMode::VividLight => "vLit",
        BlendMode::LinearLight => "lLit",
        BlendMode::PinLight => "pLit",
        BlendMode::HardMix => "hMix",
        BlendMode::Difference => "diff",
        BlendMode::Exclusion => "smud",
        BlendMode::Subtract => "fsub",
        BlendMode::Divide => "fdiv",
        BlendMode::Hue => "hue ",
        BlendMode::Saturation => "sat ",
        BlendMode::Color => "colr",
        BlendMode::Luminosity => "lum ",
        // Not present in upstream `toBlendMode`, therefore absent from `fromBlendMode`.
        BlendMode::LinearHeight | BlendMode::Height | BlendMode::Subtraction => return None,
    })
}

/// upstream `layerColors`.
pub const LAYER_COLORS: [LayerColor; 8] = [
    LayerColor::None,
    LayerColor::Red,
    LayerColor::Orange,
    LayerColor::Yellow,
    LayerColor::Green,
    LayerColor::Blue,
    LayerColor::Violet,
    LayerColor::Gray,
];

/// upstream `largeAdditionalInfoKeys`.
pub const LARGE_ADDITIONAL_INFO_KEYS: [&str; 14] = [
    // from documentation
    "LMsk", "Lr16", "Lr32", "Layr", "Mt16", "Mt32", "Mtrn", "Alph", "FMsk", "lnk2", "FEid",
    "FXid", "PxSD", // from guessing
    "cinf",
];

// ===========================================================================
// Dict / enum descriptor helpers
// ===========================================================================

/// upstream `Dict` = `{ [key: string]: string }`.
pub type Dict = HashMap<String, String>;

/// upstream `revMap`: меняет местами ключи и значения словаря.
pub fn rev_map(map: &Dict) -> Dict {
    let mut result = Dict::new();
    for (key, value) in map {
        result.insert(value.clone(), key.clone());
    }
    result
}

/// Normalizes a Photoshop 2026 long-form enum id into the space-separated map-key
/// spelling used by the descriptor enum tables: `colorBurn` -> `color burn`.
///
/// Faithful port of upstream `value.replace(/([A-Z])/g, ' $1').toLowerCase()`: a space
/// is inserted before every *ASCII* uppercase letter (so a leading capital yields a
/// leading space, exactly as upstream), and the whole string is then lowercased.
#[must_use]
pub fn enum_long_form_to_key(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    for ch in value.chars() {
        if ch.is_ascii_uppercase() {
            out.push(' ');
        }
        // `to_lowercase` (not `to_ascii_lowercase`) mirrors JS `toLowerCase()`.
        out.extend(ch.to_lowercase());
    }
    out
}

/// upstream `createEnum<T>`: возвращает пару (decode, encode) для дескрипторного
/// enum вида `prefix.value`. Так как в Rust замыкания неудобно возвращать парой,
/// предоставляем структуру с теми же decode/encode.
pub struct EnumCodec {
    prefix: String,
    def: String,
    map: Dict,
    rev: Dict,
}

impl EnumCodec {
    /// upstream `createEnum(prefix, def, map)`.
    ///
    /// `def` MUST be a *key* of `map`, never one of its values: [`EnumCodec::encode`]
    /// resolves `None` through `map[def]`, so a default that is not a key silently
    /// emits an empty code. Upstream enforces this through the type system
    /// (`createEnum<T extends string>(prefix, def: T, map: { [K in T]: string })`);
    /// Rust has no equivalent for a runtime `HashMap`, so the invariant is checked with
    /// a debug assertion — a contract violation is a programming error in the codec
    /// table, not a condition callers can recover from.
    pub fn new(prefix: &str, def: &str, map: Dict) -> Self {
        debug_assert!(
            map.contains_key(def),
            "EnumCodec '{prefix}': default '{def}' is not a key of the map"
        );
        let rev = rev_map(&map);
        EnumCodec {
            prefix: prefix.to_string(),
            def: def.to_string(),
            map,
            rev,
        }
    }

    /// True when the configured default is a valid key of the map.
    ///
    /// Mirrors the invariant asserted in [`EnumCodec::new`]; exposed so that modules
    /// owning codec tables can prove it for every codec they construct in a test.
    #[must_use]
    pub fn default_is_valid(&self) -> bool {
        self.map.contains_key(&self.def)
    }

    /// upstream `decode(val)`: `val.split('.')[1]` -> reverse-lookup -> def.
    /// Бросает (Err) при нераспознанном непустом значении.
    ///
    /// Photoshop 2026 stopped writing the historical 4-character code (the map *value*,
    /// e.g. `BlnM.Nrml`) and writes the long-form id instead. Two shapes occur:
    /// single-word values use the map *key* verbatim (`BlnM.normal`), multi-word values
    /// use a camelCase id whose map key is space-separated (`BlnM.colorBurn` ->
    /// `color burn`). Both are accepted here, in that order, before erroring — without
    /// this, every descriptor enum in a file saved by Photoshop 2026 fails to read.
    pub fn decode(&self, val: &str) -> Result<String, String> {
        // val.split('.')[1] — второй сегмент (может отсутствовать => "").
        let value = val.split('.').nth(1).unwrap_or("");
        if !value.is_empty() && !self.rev.contains_key(value) {
            if self.map.contains_key(value) {
                return Ok(value.to_string());
            }
            let spaced = enum_long_form_to_key(value);
            if self.map.contains_key(&spaced) {
                return Ok(spaced);
            }
            return Err(format!("Unrecognized value for enum: '{val}'"));
        }
        Ok(self
            .rev
            .get(value)
            .cloned()
            .unwrap_or_else(|| self.def.clone()))
    }

    /// upstream `encode(val)`: `${prefix}.${map[val] || map[def]}`.
    /// `val == None` зеркалирует `undefined` в TS.
    /// Бросает (Err) при невалидном непустом значении.
    pub fn encode(&self, val: Option<&str>) -> Result<String, String> {
        if let Some(v) = val {
            if !self.map.contains_key(v) {
                return Err(format!("Invalid value for enum: '{v}'"));
            }
        }
        let mapped = val
            .and_then(|v| self.map.get(v))
            .or_else(|| self.map.get(&self.def))
            .cloned()
            .unwrap_or_default();
        Ok(format!("{}.{}", self.prefix, mapped))
    }
}

// ===========================================================================
// Numeric const enums (port of TS `const enum`)
// ===========================================================================

/// upstream `const enum ColorSpace`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    Rgb = 0,
    Hsb = 1,
    Cmyk = 2,
    Lab = 7,
    Grayscale = 8,
}

/// upstream `const enum LayerMaskFlags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerMaskFlags {
    PositionRelativeToLayer = 1,
    LayerMaskDisabled = 2,
    /// obsolete
    InvertLayerMaskWhenBlending = 4,
    LayerMaskFromRenderingOtherData = 8,
    MaskHasParametersAppliedToIt = 16,
}

/// upstream `const enum MaskParams`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskParams {
    UserMaskDensity = 1,
    UserMaskFeather = 2,
    VectorMaskDensity = 4,
    VectorMaskFeather = 8,
}

// ===========================================================================
// Channel / bounds data shapes
// ===========================================================================

/// upstream `ChannelData`.
///
/// Field names follow upstream v31, which renamed `channelId -> id` and
/// `buffer -> data`.
#[derive(Debug, Clone)]
pub struct ChannelData {
    pub id: ChannelId,
    pub compression: Compression,
    /// Encoded channel payload, `None` when the channel has no bytes to write.
    pub data: Option<Vec<u8>>,
    pub length: usize,
}

/// upstream `Bounds`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bounds {
    pub top: i32,
    pub left: i32,
    pub right: i32,
    pub bottom: i32,
}

/// upstream `LayerChannelData`.
pub struct LayerChannelData {
    pub layer: Layer,
    pub channels: Vec<ChannelData>,
    pub top: i32,
    pub left: i32,
    pub right: i32,
    pub bottom: i32,
    pub mask: Option<Bounds>,
    pub real_mask: Option<Bounds>,
}

// ===========================================================================
// Pure numeric helpers
// ===========================================================================

/// upstream `offsetForChannel(channelId, cmyk)`.
/// В TS работает с числовыми значениями enum; здесь повторяем арифметику через i32.
pub fn offset_for_channel(channel_id: ChannelId, cmyk: bool) -> i32 {
    let id = channel_id as i32;
    match channel_id {
        ChannelId::Color0 => 0,
        ChannelId::Color1 => 1,
        ChannelId::Color2 => 2,
        ChannelId::Color3 => {
            if cmyk {
                3
            } else {
                id + 1
            }
        }
        ChannelId::Transparency => {
            if cmyk {
                4
            } else {
                3
            }
        }
        _ => id + 1,
    }
}

/// upstream `clamp(value, min, max)`.
pub fn clamp(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// upstream `hasAlpha(data)`: true, если есть пиксель с alpha != 255.
pub fn has_alpha(data: &PixelData) -> bool {
    let size = (data.width as usize) * (data.height as usize) * 4;
    let mut i = 3usize;
    while i < size {
        if data.data[i] != 255 {
            return true;
        }
        i += 4;
    }
    false
}

/// upstream `resetImageData({ data })`.
/// В оригинале alpha зависит от типа массива (Float32/Uint16/Uint8); наша модель
/// PixelData хранит байты RGBA8, поэтому alpha = 0xff.
pub fn reset_image_data(data: &mut PixelData) {
    let buf = &mut data.data;
    let alpha = 0xffu8;
    let size = buf.len();
    let mut p = 0usize;
    while p < size {
        buf[p] = 0;
        buf[p + 1] = 0;
        buf[p + 2] = 0;
        buf[p + 3] = alpha;
        p += 4;
    }
}

/// upstream `decodeBitmap(input, output, width, height)`.
/// Распаковывает 1-битное изображение в RGBA8: бит=1 -> чёрный (0), бит=0 -> белый (255).
pub fn decode_bitmap(input: &[u8], output: &mut [u8], width: usize, height: usize) {
    let mut p = 0usize;
    let mut o = 0usize;
    for _y in 0..height {
        let mut x = 0usize;
        while x < width {
            let mut b = input[o];
            o += 1;
            let mut i = 0;
            while i < 8 && x < width {
                let v: u8 = if b & 0x80 != 0 { 0 } else { 255 };
                b <<= 1;
                output[p] = v;
                output[p + 1] = v;
                output[p + 2] = v;
                output[p + 3] = 255;
                i += 1;
                x += 1;
                p += 4;
            }
        }
    }
}

// ===========================================================================
// Channel data writers (raw / RLE / zip)
// ===========================================================================

/// upstream `writeDataRaw(data, offset, width, height)`.
/// Извлекает один канал (по offset) в плотный массив длиной width*height.
///
/// `offset` is the channel's byte index inside each RGBA quadruple (`0..=3`).
/// Returns `None` for an empty bitmap or when `data.data` is shorter than
/// `width * height * 4`; upstream reads past the end of its typed array and
/// gets zeros, but an out-of-range index panics in Rust, so the caller is told
/// instead (crate contract: a public API never panics on a bad buffer length).
#[must_use]
pub fn write_data_raw(data: &PixelData, offset: usize, width: usize, height: usize) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || offset >= 4 {
        return None;
    }
    let needed = width.checked_mul(height)?.checked_mul(4)?;
    if data.data.len() < needed {
        return None;
    }
    let mut array = vec![0u8; width * height];
    for (i, slot) in array.iter_mut().enumerate() {
        *slot = data.data[i * 4 + offset];
    }
    Some(array)
}

/// Bytes one channel sample occupies in the file at `bit_depth`.
///
/// `bit_depth` is a PSD channel depth in bits; only 8, 16 and 32 exist for the
/// RGB modes this crate writes, and anything else returns `None` rather than
/// guessing a width.
#[must_use]
pub fn bytes_per_sample(bit_depth: u32) -> Option<usize> {
    match bit_depth {
        8 => Some(1),
        16 => Some(2),
        32 => Some(4),
        _ => None,
    }
}

/// Expands RGBA8 channel samples into the byte representation an RGB PSD/PSB
/// channel uses at `bit_depth`.
///
/// `samples` holds one byte per pixel, `0..=255`. The returned buffer is
/// big-endian and `samples.len() * bytes_per_sample(bit_depth)` bytes long:
///
/// - 8 bits — the samples verbatim;
/// - 16 bits — `sample * 257`, which maps `0..=255` onto the full `0..=65535`
///   range with `0xff` becoming `0xffff` rather than `0xff00`;
/// - 32 bits — `sample / 255.0` as an IEEE-754 `f32` in `0.0..=1.0`, the
///   normalized form Photoshop stores for 32-bit documents.
///
/// Returns `None` if `bit_depth` is not 8, 16 or 32.
#[must_use]
pub fn expand_channel_samples(samples: &[u8], bit_depth: u32) -> Option<Vec<u8>> {
    match bit_depth {
        8 => Some(samples.to_vec()),
        16 => Some(
            samples
                .iter()
                .flat_map(|&sample| (u16::from(sample) * 257).to_be_bytes())
                .collect(),
        ),
        32 => Some(
            samples
                .iter()
                // u8 -> f32 is exact, so the division is the only rounding step.
                .flat_map(|&sample| (f32::from(sample) / 255.0).to_be_bytes())
                .collect(),
        ),
        _ => None,
    }
}

/// Extracts one RGBA8 channel and expands it to an uncompressed PSD channel
/// payload at `bit_depth`.
///
/// `offset` is the byte index of the channel inside each RGBA quadruple
/// (`0..=3`), `width` x `height` the bitmap size in pixels. Returns
/// `width * height * bytes_per_sample(bit_depth)` big-endian bytes, or `None`
/// for an empty bitmap, an unsupported `bit_depth`, or a `data` buffer too
/// short for the declared size (see [`write_data_raw`]).
#[must_use]
pub fn write_data_raw_bit_depth(
    data: &PixelData,
    offset: usize,
    width: usize,
    height: usize,
    bit_depth: u32,
) -> Option<Vec<u8>> {
    expand_channel_samples(&write_data_raw(data, offset, width, height)?, bit_depth)
}

/// upstream `writeDataRLE(buffer, { data, width, height }, offsets, large)`.
/// Сжимает каналы по PackBits, как в оригинале (включая раскладку length-таблицы
/// в начале буфера). Возвращает срез использованной части буфера.
///
/// `buffer` must hold `offsets.len() * (height * entry + 2 * width * height)`
/// bytes, where `entry` is 4 when `large` (PSB row lengths) and 2 otherwise —
/// see `writer::rle_scratch_size`. A shorter buffer does not error: writes past
/// its end are dropped and the result is truncated, mirroring upstream's
/// `Uint8Array`.
pub fn write_data_rle(
    buffer: &mut [u8],
    data_pixels: &PixelData,
    offsets: &[usize],
    large: bool,
) -> Option<Vec<u8>> {
    let width = data_pixels.width as i64;
    let height = data_pixels.height as i64;
    if width == 0 || height == 0 {
        return None;
    }
    let data = &data_pixels.data;
    let stride = 4 * width;

    let mut ol: i64 = 0;
    let mut o: i64 = (offsets.len() as i64) * (if large { 4 } else { 2 }) * height;

    let get = |idx: i64| -> i64 { data[idx as usize] as i64 };

    // upstream writes into a `Uint8Array`; writing past its end is a silent
    // no-op in JS (the value is simply dropped), and `buffer.slice(0, o)` later
    // returns only the bytes that fit. Mirroring those TypedArray semantics
    // keeps a caller-supplied short buffer from panicking; `o`/`ol` still
    // advance so the returned length matches TS. Note that this makes an
    // undersized buffer produce silently truncated output, so callers inside
    // this crate must size it from `writer::rle_scratch_size`, which is a
    // proven bound for both PSD and PSB row-length tables.
    macro_rules! set {
        ($buf:expr, $idx:expr, $val:expr) => {{
            let idx = $idx as usize;
            if idx < $buf.len() {
                $buf[idx] = $val;
            }
        }};
    }

    for &offset in offsets {
        let offset = offset as i64;
        for y in 0..height {
            let stride_start = y * stride;
            let stride_end = stride_start + stride;
            let last_index = stride_end + offset - 4;
            let last_index2 = last_index - 4;
            let start_offset = o;

            let mut p = stride_start + offset;
            while p < stride_end {
                if p < last_index2 {
                    let mut value1 = get(p);
                    p += 4;
                    let mut value2 = get(p);
                    p += 4;
                    let mut value3 = get(p);

                    if value1 == value2 && value1 == value3 {
                        let mut count: i64 = 3;
                        while count < 128 && p < last_index && get(p + 4) == value1 {
                            count += 1;
                            p += 4;
                        }
                        set!(buffer, o, (1 - count) as u8);
                        o += 1;
                        set!(buffer, o, value1 as u8);
                        o += 1;
                    } else {
                        let count_index = o;
                        let mut write_last = true;
                        let mut count: i64 = 1;
                        set!(buffer, o, 0);
                        o += 1;
                        set!(buffer, o, value1 as u8);
                        o += 1;

                        while p < last_index && count < 128 {
                            p += 4;
                            value1 = value2;
                            value2 = value3;
                            value3 = get(p);

                            if value1 == value2 && value1 == value3 {
                                p -= 12;
                                write_last = false;
                                break;
                            } else {
                                count += 1;
                                set!(buffer, o, value1 as u8);
                                o += 1;
                            }
                        }

                        if write_last {
                            if count < 127 {
                                set!(buffer, o, value2 as u8);
                                o += 1;
                                set!(buffer, o, value3 as u8);
                                o += 1;
                                count += 2;
                            } else if count < 128 {
                                set!(buffer, o, value2 as u8);
                                o += 1;
                                count += 1;
                                p -= 4;
                            } else {
                                p -= 8;
                            }
                        }

                        set!(buffer, count_index, (count - 1) as u8);
                    }
                } else if p == last_index {
                    set!(buffer, o, 0);
                    o += 1;
                    set!(buffer, o, get(p) as u8);
                    o += 1;
                } else {
                    // p === lastIndex2
                    set!(buffer, o, 1);
                    o += 1;
                    set!(buffer, o, get(p) as u8);
                    o += 1;
                    p += 4;
                    set!(buffer, o, get(p) as u8);
                    o += 1;
                }

                p += 4;
            }

            let length = o - start_offset;

            if large {
                set!(buffer, ol, ((length >> 24) & 0xff) as u8);
                ol += 1;
                set!(buffer, ol, ((length >> 16) & 0xff) as u8);
                ol += 1;
            }

            set!(buffer, ol, ((length >> 8) & 0xff) as u8);
            ol += 1;
            set!(buffer, ol, (length & 0xff) as u8);
            ol += 1;
        }
    }

    // mirror `buffer.slice(0, o)`: clamp to the buffer length so we never read
    // past the end when the size estimate fell short (out-of-bounds writes above
    // were dropped, so those positions hold zero/stale bytes which TS omits too).
    let end = (o as usize).min(buffer.len());
    Some(buffer[..end].to_vec())
}

/// Why [`write_data_rle_bit_depth`] could not produce a valid PackBits channel.
///
/// Every variant describes input the PSD/PSB container cannot represent, so a
/// caller must reject the document rather than emit a shorter channel: an
/// undersized channel is structurally valid and silently corrupt, which is the
/// failure mode this type exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RleEncodeError {
    /// `bit_depth` was not one of the PSD channel depths 8, 16 or 32.
    UnsupportedBitDepth {
        /// The rejected depth, in bits.
        bit_depth: u32,
    },
    /// The bitmap has a zero side, no channel offsets were given, an offset is
    /// not a valid RGBA component index (`0..=3`), or `data` is shorter than
    /// `width * height * 4` bytes.
    InvalidBitmap {
        /// Bitmap width in pixels.
        width: usize,
        /// Bitmap height in pixels.
        height: usize,
        /// Number of channel offsets requested.
        channels: usize,
        /// Length of the RGBA8 source buffer, in bytes.
        data_len: usize,
    },
    /// One compressed row is longer than its row-length table entry can
    /// address. PSD stores that entry in two bytes (65535 max) and PSB in four,
    /// so a wide 32-bit bitmap can overflow the PSD form; writing it as PSB is
    /// the fix.
    RowLengthOverflow {
        /// Zero-based row index inside the channel.
        row: usize,
        /// Length of the PackBits-compressed row, in bytes.
        encoded_len: usize,
        /// Largest value the row-length entry can hold, in bytes.
        max_len: usize,
    },
}

impl std::fmt::Display for RleEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RleEncodeError::UnsupportedBitDepth { bit_depth } => {
                write!(f, "Unsupported channel bit depth: {} (expected 8, 16 or 32)", bit_depth)
            }
            RleEncodeError::InvalidBitmap { width, height, channels, data_len } => write!(
                f,
                "Invalid bitmap for RLE encoding: {}x{}, {} channel offset(s), {} bytes of RGBA8 data",
                width, height, channels, data_len
            ),
            RleEncodeError::RowLengthOverflow { row, encoded_len, max_len } => write!(
                f,
                "Compressed row {} is {} bytes, more than the {} a row-length entry can address \
                 (use the PSB format for bitmaps this wide)",
                row, encoded_len, max_len
            ),
        }
    }
}

impl std::error::Error for RleEncodeError {}

/// Compresses one or more channels with PackBits after expanding RGBA8 samples
/// to `bit_depth`, mirroring the layout [`write_data_rle`] produces for 8-bit
/// data: a per-row length table for every channel first, then the rows.
///
/// `offsets` lists the channels to encode as byte indices inside each RGBA
/// quadruple (`0..=3`). `large` selects the PSB row-length entry width (4
/// bytes, against 2 for PSD). `bit_depth` is a PSD channel depth in bits and
/// must be 8, 16 or 32.
///
/// `buffer` is the shared writer scratch buffer and is used *only* for the
/// 8-bit path, which delegates to [`write_data_rle`]; the high-depth path
/// allocates its own exactly-sized output, so it is never truncated by a short
/// scratch buffer. Sizing for the 8-bit path is `writer::rle_scratch_size`.
///
/// # Errors
/// [`RleEncodeError`] — see that type; every variant means the document cannot
/// be written faithfully, never that a shorter result was produced.
pub fn write_data_rle_bit_depth(
    buffer: &mut [u8],
    data_pixels: &PixelData,
    offsets: &[usize],
    large: bool,
    bit_depth: u32,
) -> Result<Vec<u8>, RleEncodeError> {
    let width = data_pixels.width as usize;
    let height = data_pixels.height as usize;
    let invalid = || RleEncodeError::InvalidBitmap {
        width,
        height,
        channels: offsets.len(),
        data_len: data_pixels.data.len(),
    };

    if bit_depth == 8 {
        return write_data_rle(buffer, data_pixels, offsets, large).ok_or_else(invalid);
    }
    let sample_bytes = bytes_per_sample(bit_depth)
        .ok_or(RleEncodeError::UnsupportedBitDepth { bit_depth })?;

    let pixels = width
        .checked_mul(height)
        .filter(|&pixels| pixels != 0)
        .ok_or_else(invalid)?;
    if offsets.is_empty()
        || offsets.iter().any(|&offset| offset >= 4)
        || data_pixels.data.len() < pixels * 4
    {
        return Err(invalid());
    }

    // The row-length entry is written before the row it measures, so the whole
    // table is reserved up front and filled in as the rows are compressed.
    let entry_size = if large { 4 } else { 2 };
    // Both supported targets are 64-bit, so `u32::MAX` fits `usize`; `try_from`
    // keeps the bound honest on any other target instead of truncating it.
    let max_row_len = if large {
        usize::try_from(u32::MAX).unwrap_or(usize::MAX)
    } else {
        usize::from(u16::MAX)
    };
    let mut output = vec![0u8; offsets.len() * height * entry_size];
    let mut table_offset = 0usize;

    for &offset in offsets {
        let channel: Vec<u8> = (0..pixels).map(|pixel| data_pixels.data[pixel * 4 + offset]).collect();
        let expanded = expand_channel_samples(&channel, bit_depth)
            .ok_or(RleEncodeError::UnsupportedBitDepth { bit_depth })?;

        for row in 0..height {
            let row_start = row * width * sample_bytes;
            let encoded = packbits_encode(&expanded[row_start..row_start + width * sample_bytes]);
            if encoded.len() > max_row_len {
                return Err(RleEncodeError::RowLengthOverflow {
                    row,
                    encoded_len: encoded.len(),
                    max_len: max_row_len,
                });
            }
            // The overflow check above proves both conversions are exact.
            let entry = &mut output[table_offset..table_offset + entry_size];
            if large {
                entry.copy_from_slice(&(encoded.len() as u32).to_be_bytes());
            } else {
                entry.copy_from_slice(&(encoded.len() as u16).to_be_bytes());
            }
            table_offset += entry_size;
            output.extend_from_slice(&encoded);
        }
    }
    Ok(output)
}

/// PackBits-compresses one row of bytes.
///
/// Emits Photoshop's byte-oriented variant: a header byte of `0..=127` starts a
/// literal run of `header + 1` bytes, `129..=255` a repeat of the next byte
/// `257 - header` times, and `128` is never produced (decoders treat it as a
/// no-op). Both run kinds are capped at 128 bytes, so the result never exceeds
/// `row.len() + row.len() / 128 + 1` bytes.
fn packbits_encode(row: &[u8]) -> Vec<u8> {
    /// Longest run or literal a single PackBits header byte can describe.
    const MAX_RUN: usize = 128;

    let mut encoded = Vec::with_capacity(row.len() + row.len() / MAX_RUN + 1);
    let mut i = 0usize;
    while i < row.len() {
        let run = repeat_len(row, i, MAX_RUN);
        if run >= 3 {
            // `run` is 3..=128, so `1 - run` is -127..=-2: the two's-complement
            // byte is 129..=254 and can never collide with the 128 no-op.
            encoded.push(1u8.wrapping_sub(run_as_u8(run)));
            encoded.push(row[i]);
            i += run;
            continue;
        }

        // Literal run: keep taking bytes until a run of three or more starts,
        // the row ends, or the 128-byte header limit is reached. The limit is
        // checked against the *next* step's length, because overshooting it
        // would emit a header of 128, which decoders read as a no-op.
        let literal_start = i;
        i += run;
        while i < row.len() {
            let next_run = repeat_len(row, i, MAX_RUN);
            if next_run >= 3 || i - literal_start + next_run > MAX_RUN {
                break;
            }
            i += next_run;
        }
        let literal_len = i - literal_start;
        encoded.push(run_as_u8(literal_len) - 1);
        encoded.extend_from_slice(&row[literal_start..i]);
    }
    encoded
}

/// Length of the run of equal bytes starting at `start`, capped at `max`.
fn repeat_len(row: &[u8], start: usize, max: usize) -> usize {
    let value = row[start];
    let mut len = 1usize;
    while len < max && start + len < row.len() && row[start + len] == value {
        len += 1;
    }
    len
}

/// Narrows a PackBits run length to `u8`.
///
/// `packbits_encode` caps every run at 128 before calling this, so the value
/// always fits; the saturating fallback exists only so the conversion cannot
/// panic or wrap if that invariant is ever broken by a later edit.
fn run_as_u8(len: usize) -> u8 {
    u8::try_from(len).unwrap_or(u8::MAX)
}

/// upstream `writeDataZipWithoutPrediction({ data, width, height }, offsets)`.
/// Извлекает каждый канал и сжимает zlib/deflate, конкатенируя результаты.
pub fn write_data_zip_without_prediction(data_pixels: &PixelData, offsets: &[usize]) -> Option<Vec<u8>> {
    let size = (data_pixels.width as usize) * (data_pixels.height as usize);
    let data = &data_pixels.data;
    let mut channel = vec![0u8; size];
    let mut buffers: Vec<Vec<u8>> = Vec::new();
    let mut total_length = 0usize;

    for &offset in offsets {
        let mut o = offset;
        for slot in channel.iter_mut().take(size) {
            *slot = data[o];
            o += 4;
        }

        let buffer = deflate_sync(&channel);
        total_length += buffer.len();
        buffers.push(buffer);
    }

    if !buffers.is_empty() {
        let mut buffer = Vec::with_capacity(total_length);
        for b in &buffers {
            buffer.extend_from_slice(b);
        }
        Some(buffer)
    } else {
        // upstream возвращает buffers[0] (undefined при пустом списке).
        None
    }
}

/// Encodes channels with ZIP (zlib-wrapped deflate, matching upstream's `pako`
/// `deflate`) after expanding RGBA8 samples to `bit_depth`.
///
/// `offsets` lists the channels as byte indices inside each RGBA quadruple
/// (`0..=3`); the compressed channels are concatenated in that order, with no
/// per-channel length table — the enclosing record supplies the lengths.
/// `bit_depth` is a PSD channel depth in bits and must be 8, 16 or 32.
///
/// Returns `None` for an unsupported `bit_depth`, an empty bitmap or channel
/// list, an offset outside `0..=3`, or a `data` buffer shorter than
/// `width * height * 4`.
#[must_use]
pub fn write_data_zip_without_prediction_bit_depth(
    data_pixels: &PixelData,
    offsets: &[usize],
    bit_depth: u32,
) -> Option<Vec<u8>> {
    if bit_depth == 8 {
        return write_data_zip_without_prediction(data_pixels, offsets);
    }
    bytes_per_sample(bit_depth)?;
    let size = (data_pixels.width as usize).checked_mul(data_pixels.height as usize)?;
    if size == 0
        || offsets.is_empty()
        || offsets.iter().any(|&offset| offset >= 4)
        || data_pixels.data.len() < size.checked_mul(4)?
    {
        return None;
    }
    let mut buffers = Vec::with_capacity(offsets.len());
    for &offset in offsets {
        let samples = (0..size)
            .map(|pixel| data_pixels.data[pixel * 4 + offset])
            .collect::<Vec<_>>();
        buffers.push(deflate_sync(&expand_channel_samples(&samples, bit_depth)?));
    }
    let total_length = buffers.iter().map(Vec::len).sum();
    let mut output = Vec::with_capacity(total_length);
    for buffer in buffers {
        output.extend_from_slice(&buffer);
    }
    Some(output)
}

/// Эквивалент `deflate` из `pako` (zlib-обёрнутый deflate).
fn deflate_sync(input: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), FlateCompression::default());
    encoder.write_all(input).expect("zlib write");
    encoder.finish().expect("zlib finish")
}

// ===========================================================================
// Canvas-уровень — заглушки под модель PixelData
// ===========================================================================

/// upstream `imageDataToCanvas(pixelData)`.
/// TODO: browser-canvas concern. Наша модель уже хранит RGBA8 в PixelData, так что
/// "канвас" — это и есть копия PixelData. Гамма/битность-конверсии оригинала
/// (Float32 pow(1/2.2), Uint16 >>8) не нужны для байтовой модели.
pub fn image_data_to_canvas(pixel_data: &PixelData) -> PixelData {
    pixel_data.clone()
}

/// upstream `createCanvasFromData(data)` — декодирование JPEG в канвас.
/// TODO: browser-canvas concern; зависит от не-портированного `decode_jpeg`.
/// Стаб: возвращает пустой PixelData 100x100, как и стартовый канвас в оригинале.
pub fn create_canvas_from_data(_data: &[u8]) -> PixelData {
    create_canvas(100, 100)
}

/// upstream `createCanvas(width, height)`.
/// TODO: browser-canvas concern, not needed for byte IO. Возвращаем нулевой
/// RGBA8-буфер нужного размера вместо HTMLCanvasElement.
pub fn create_canvas(width: u32, height: u32) -> PixelData {
    PixelData {
        width,
        height,
        data: vec![0u8; (width as usize) * (height as usize) * 4],
    }
}

/// upstream `createImageData(width, height)`.
/// TODO: browser-canvas concern, not needed for byte IO.
pub fn create_image_data(width: u32, height: u32) -> PixelData {
    create_canvas(width, height)
}

/// upstream `initializeCanvas(createCanvasMethod, createImageDataMethod?)`.
/// TODO: browser-canvas concern — установка глобальной фабрики канваса.
/// В байтовой модели не требуется; намеренный no-op.
pub fn initialize_canvas() {
    // no-op
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_mode_round_trip_all_entries() {
        let all = [
            BlendMode::PassThrough,
            BlendMode::Normal,
            BlendMode::Dissolve,
            BlendMode::Darken,
            BlendMode::Multiply,
            BlendMode::ColorBurn,
            BlendMode::LinearBurn,
            BlendMode::DarkerColor,
            BlendMode::Lighten,
            BlendMode::Screen,
            BlendMode::ColorDodge,
            BlendMode::LinearDodge,
            BlendMode::LighterColor,
            BlendMode::Overlay,
            BlendMode::SoftLight,
            BlendMode::HardLight,
            BlendMode::VividLight,
            BlendMode::LinearLight,
            BlendMode::PinLight,
            BlendMode::HardMix,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Subtract,
            BlendMode::Divide,
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ];
        for mode in all {
            // `expect` takes a plain &str and would print the braces literally,
            // so format the mode explicitly.
            let key = from_blend_mode(mode)
                .unwrap_or_else(|| panic!("legacy signature exists for {mode:?}"));
            assert_eq!(key.len(), 4, "key must be 4 chars: {key:?}");
            assert_eq!(to_blend_mode(key), Some(mode), "round trip failed for {key:?}");
        }
    }

    #[test]
    fn descriptor_only_blend_modes_have_no_legacy_signature() {
        // Upstream `toBlendMode` has no code for these, so `fromBlendMode[mode]` is
        // `undefined` and every call site substitutes its own default.
        assert_eq!(from_blend_mode(BlendMode::LinearHeight), None);
        assert_eq!(from_blend_mode(BlendMode::Height), None);
        assert_eq!(from_blend_mode(BlendMode::Subtraction), None);
    }

    #[test]
    fn blend_mode_spacey_keys() {
        assert_eq!(to_blend_mode("mul "), Some(BlendMode::Multiply));
        assert_eq!(to_blend_mode("div "), Some(BlendMode::ColorDodge));
        assert_eq!(from_blend_mode(BlendMode::Luminosity), Some("lum "));
        assert_eq!(to_blend_mode("nope"), None);
    }

    #[test]
    fn clamp_edges() {
        assert_eq!(clamp(-1.0, 0.0, 10.0), 0.0);
        assert_eq!(clamp(11.0, 0.0, 10.0), 10.0);
        assert_eq!(clamp(5.0, 0.0, 10.0), 5.0);
        assert_eq!(clamp(0.0, 0.0, 10.0), 0.0);
        assert_eq!(clamp(10.0, 0.0, 10.0), 10.0);
    }

    #[test]
    fn offset_for_channel_rgb_and_cmyk() {
        assert_eq!(offset_for_channel(ChannelId::Color0, false), 0);
        assert_eq!(offset_for_channel(ChannelId::Color1, false), 1);
        assert_eq!(offset_for_channel(ChannelId::Color2, false), 2);
        // Color3 == 3: rgb branch -> id+1 == 4; cmyk -> 3.
        assert_eq!(offset_for_channel(ChannelId::Color3, false), 4);
        assert_eq!(offset_for_channel(ChannelId::Color3, true), 3);
        // Transparency == -1.
        assert_eq!(offset_for_channel(ChannelId::Transparency, false), 3);
        assert_eq!(offset_for_channel(ChannelId::Transparency, true), 4);
        // default: UserMask == -2 -> id+1 == -1.
        assert_eq!(offset_for_channel(ChannelId::UserMask, false), -1);
        assert_eq!(offset_for_channel(ChannelId::RealUserMask, true), -2);
    }

    #[test]
    fn has_alpha_detects_non_opaque() {
        let opaque = PixelData {
            width: 2,
            height: 1,
            data: vec![1, 2, 3, 255, 4, 5, 6, 255],
        };
        assert!(!has_alpha(&opaque));

        let translucent = PixelData {
            width: 2,
            height: 1,
            data: vec![1, 2, 3, 255, 4, 5, 6, 128],
        };
        assert!(has_alpha(&translucent));
    }

    #[test]
    fn reset_image_data_sets_black_opaque() {
        let mut pd = PixelData {
            width: 2,
            height: 1,
            data: vec![9, 9, 9, 9, 9, 9, 9, 9],
        };
        reset_image_data(&mut pd);
        assert_eq!(pd.data, vec![0, 0, 0, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn decode_bitmap_packs_bits() {
        // One byte 0b10100000 over width 8: bits -> black,white,black,white,...
        let input = [0b1010_0000u8];
        let mut output = vec![0u8; 8 * 4];
        decode_bitmap(&input, &mut output, 8, 1);
        // pixel0 bit set -> 0; pixel1 bit clear -> 255; pixel2 -> 0; pixel3 -> 255 ...
        assert_eq!(output[0], 0);
        assert_eq!(output[4], 255);
        assert_eq!(output[8], 0);
        assert_eq!(output[12], 255);
        // alpha always 255
        assert_eq!(output[3], 255);
    }

    #[test]
    fn write_data_raw_extracts_channel() {
        // 2x1 RGBA: [r0 g0 b0 a0, r1 g1 b1 a1]
        let pd = PixelData {
            width: 2,
            height: 1,
            data: vec![10, 20, 30, 40, 50, 60, 70, 80],
        };
        assert_eq!(write_data_raw(&pd, 0, 2, 1), Some(vec![10, 50])); // red
        assert_eq!(write_data_raw(&pd, 3, 2, 1), Some(vec![40, 80])); // alpha
        assert_eq!(write_data_raw(&pd, 0, 0, 1), None);
    }

    #[test]
    fn expands_rgba8_samples_for_high_bit_depths() {
        assert_eq!(
            expand_channel_samples(&[0, 1, 127, 255], 16),
            Some(vec![0, 0, 1, 1, 127, 127, 255, 255])
        );
        let expanded = expand_channel_samples(&[0, 1, 127, 255], 32).unwrap();
        let values = expanded
            .chunks_exact(4)
            .map(|bytes| f32::from_be_bytes(bytes.try_into().unwrap()))
            .collect::<Vec<_>>();
        assert_eq!(values, vec![0.0, 1.0 / 255.0, 127.0 / 255.0, 1.0]);
        assert_eq!(expand_channel_samples(&[1], 12), None);
        let pd = PixelData {
            width: 2,
            height: 1,
            data: vec![1, 2, 3, 4, 255, 6, 7, 8],
        };
        assert_eq!(write_data_raw_bit_depth(&pd, 0, 2, 1, 16), Some(vec![1, 1, 255, 255]));

        // The ZIP channel writer emits zlib-wrapped deflate, exactly like
        // upstream's `pako` `deflate` — decoding it as raw DEFLATE must fail.
        let compressed = write_data_zip_without_prediction_bit_depth(&pd, &[0], 32).unwrap();
        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut decoded = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();
        assert_eq!(decoded, [1.0f32 / 255.0, 1.0].into_iter().flat_map(f32::to_be_bytes).collect::<Vec<_>>());

        // Rejected inputs, rather than a silently shorter channel.
        assert_eq!(write_data_zip_without_prediction_bit_depth(&pd, &[0], 12), None);
        assert_eq!(write_data_zip_without_prediction_bit_depth(&pd, &[], 16), None);
        assert_eq!(write_data_zip_without_prediction_bit_depth(&pd, &[4], 16), None);
        assert_eq!(write_data_raw_bit_depth(&pd, 0, 3, 1, 16), None);
    }

    #[test]
    fn packbits_never_emits_the_no_op_header() {
        // A 200-byte literal stretch with no run of three: the encoder must
        // split it into two literals of at most 128 bytes each and never emit
        // header 128, which decoders skip.
        let row: Vec<u8> = (0..200u16).map(|i| if i % 2 == 0 { 0 } else { 1 }).collect();
        let encoded = packbits_encode(&row);
        assert!(!encoded.is_empty());
        assert_eq!(encoded[0], 127, "first literal must be capped at 128 bytes");
        assert_eq!(encoded[129], 71, "remaining 72 bytes form the second literal");
        assert_eq!(encoded.len(), 202);

        // Runs longer than 128 are split too, and a run header is 129..=254.
        let encoded = packbits_encode(&[7u8; 300]);
        assert_eq!(encoded, vec![129, 7, 129, 7, 213, 7]);

        // Round trip through the reader's decoder for a mixed row.
        let row = [1u8, 1, 1, 1, 2, 3, 4, 4, 4, 4, 4, 9];
        assert_eq!(
            crate::reader::decode_packbits_row(&packbits_encode(&row), row.len()),
            row.to_vec()
        );
    }

    #[test]
    fn rle_bit_depth_reports_input_it_cannot_encode() {
        let pd = PixelData { width: 2, height: 1, data: vec![1, 2, 3, 4, 255, 6, 7, 8] };
        let mut scratch = [0u8; 64];

        assert_eq!(
            write_data_rle_bit_depth(&mut scratch, &pd, &[0], false, 12),
            Err(RleEncodeError::UnsupportedBitDepth { bit_depth: 12 })
        );
        assert!(matches!(
            write_data_rle_bit_depth(&mut scratch, &pd, &[], false, 16),
            Err(RleEncodeError::InvalidBitmap { .. })
        ));
        assert!(matches!(
            write_data_rle_bit_depth(&mut scratch, &pd, &[4], false, 16),
            Err(RleEncodeError::InvalidBitmap { .. })
        ));

        // A short scratch buffer must NOT truncate the high-depth result: the
        // encoder allocates its own exactly-sized output. `writer.rs` relies on
        // exactly this to size the shared scratch buffer for 8-bit only
        // (`SCRATCH_DEPTH`), so this assertion guards that decision too.
        let mut tiny = [0u8; 1];
        let full = write_data_rle_bit_depth(&mut scratch, &pd, &[0], false, 16).unwrap();
        assert_eq!(write_data_rle_bit_depth(&mut tiny, &pd, &[0], false, 16).unwrap(), full);
        // Two-byte row length table + the PackBits form of [1, 1, 255, 255].
        assert_eq!(full.len(), 2 + 5);
        assert_eq!(&full[..2], &[0, 5]);

        // A row too long for a PSD row-length entry is an error, not a
        // truncated channel; the same bitmap encodes fine as PSB.
        const WIDE: u32 = 40000;
        let wide = PixelData {
            width: WIDE,
            height: 1,
            // Cycling samples defeat run compression, so the encoded row is
            // longer than the 65535 a PSD row-length entry can address.
            data: (0..WIDE)
                .flat_map(|x| [u8::try_from(x % 251).unwrap_or(0), 0, 0, 255])
                .collect(),
        };
        assert!(matches!(
            write_data_rle_bit_depth(&mut scratch, &wide, &[0], false, 16),
            Err(RleEncodeError::RowLengthOverflow { row: 0, .. })
        ));
        assert!(write_data_rle_bit_depth(&mut scratch, &wide, &[0], true, 16).is_ok());
    }

    #[test]
    fn zip_without_prediction_round_trips() {
        use flate2::read::ZlibDecoder;
        use std::io::Read;

        let pd = PixelData {
            width: 4,
            height: 1,
            data: vec![
                1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0,
            ],
        };
        let out = write_data_zip_without_prediction(&pd, &[0]).unwrap();
        let mut decoder = ZlibDecoder::new(&out[..]);
        let mut decoded = Vec::new();
        decoder.read_to_end(&mut decoded).unwrap();
        assert_eq!(decoded, vec![1, 2, 3, 4]);
    }

    #[test]
    fn rev_map_swaps() {
        let mut m = Dict::new();
        m.insert("a".into(), "1".into());
        m.insert("b".into(), "2".into());
        let r = rev_map(&m);
        assert_eq!(r.get("1"), Some(&"a".to_string()));
        assert_eq!(r.get("2"), Some(&"b".to_string()));
    }

    #[test]
    fn enum_codec_encode_decode() {
        let mut map = Dict::new();
        map.insert("alpha".into(), "Alph".into());
        map.insert("beta".into(), "Beta".into());
        let codec = EnumCodec::new("Enum", "alpha", map);

        assert_eq!(codec.encode(Some("beta")).unwrap(), "Enum.Beta");
        assert_eq!(codec.encode(None).unwrap(), "Enum.Alph"); // falls back to def
        assert!(codec.encode(Some("gamma")).is_err());

        assert_eq!(codec.decode("Enum.Beta").unwrap(), "beta");
        assert_eq!(codec.decode("Enum.Alph").unwrap(), "alpha");
        // empty second segment -> default
        assert_eq!(codec.decode("Enum").unwrap(), "alpha");
        assert!(codec.decode("Enum.Zzzz").is_err());
    }

    /// A `BlnM`-shaped codec: single-word and multi-word keys, historical codes.
    fn bln_m_like() -> EnumCodec {
        let map: Dict = [
            ("normal", "Nrml"),
            ("color burn", "CBrn"),
            ("linear burn", "linearBurn"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
        EnumCodec::new("BlnM", "normal", map)
    }

    #[test]
    fn enum_codec_decodes_historical_four_char_code() {
        // Every Photoshop up to 2025 writes the map VALUE; this must keep working.
        let codec = bln_m_like();
        assert_eq!(codec.decode("BlnM.Nrml").unwrap(), "normal");
        assert_eq!(codec.decode("BlnM.CBrn").unwrap(), "color burn");
        assert_eq!(codec.decode("BlnM.linearBurn").unwrap(), "linear burn");
    }

    #[test]
    fn enum_codec_decodes_photoshop_2026_long_form_key() {
        // Photoshop 2026 writes the map KEY verbatim for single-word values.
        let codec = bln_m_like();
        assert_eq!(codec.decode("BlnM.normal").unwrap(), "normal");
    }

    #[test]
    fn enum_codec_decodes_photoshop_2026_camel_case_long_form() {
        // Multi-word values arrive camelCased: 'colorBurn' -> 'color burn'.
        let codec = bln_m_like();
        assert_eq!(codec.decode("BlnM.colorBurn").unwrap(), "color burn");
    }

    #[test]
    fn enum_codec_still_rejects_a_genuinely_unknown_value() {
        let codec = bln_m_like();
        let err = codec.decode("BlnM.wibbleWobble").unwrap_err();
        assert!(err.contains("Unrecognized value for enum"), "{err}");
        // A camelCase id whose normalized form is still unknown must not be accepted.
        assert!(codec.decode("BlnM.Zzzz").is_err());
    }

    #[test]
    fn enum_codec_reports_an_invalid_default() {
        // `EnumCodec::new` debug-asserts this; `default_is_valid` lets codec-owning
        // modules prove the invariant for their own tables without a panic.
        assert!(bln_m_like().default_is_valid());
    }

    #[test]
    fn enum_long_form_to_key_matches_the_upstream_regex() {
        assert_eq!(enum_long_form_to_key("colorBurn"), "color burn");
        assert_eq!(enum_long_form_to_key("normal"), "normal");
        // A leading capital yields a leading space, exactly like `' $1'` upstream.
        assert_eq!(enum_long_form_to_key("ColorBurn"), " color burn");
        assert_eq!(enum_long_form_to_key(""), "");
    }
}
