/*
File: crates/ag-psd/src/additional_info/text_keys.rs

Purpose:
Group-модуль additional-info ключей. Группа: `Group::Text`.
Текстовые слои (TySh, Txt2).

PORT STATUS: реализованы оба ключа группы — `TySh` (type tool object setting —
текстовый слой) и `Txt2` (text engine data 2). Порт upstream-хэндлеров из
`test/ag-psd/src/additionalInfo.ts` (`addHandler('TySh', ...)` /
`addHandler('Txt2', ...)`).

GROUP-MODULE CONTRACT (см. mod.rs):
- `pub fn read(key, reader, info, left, ctx) -> ReadResult<Option<()>>`
    Ok(Some(())) — ключ обработан; Ok(None) — "не мой ключ".
- `pub fn has(key, info) -> Option<bool>`
    Some(b) — этот group-модуль владеет ключом, b = нужно ли его писать;
    None — "не мой ключ".
- `pub fn write(key, writer, info, ctx) -> Option<ReadResult<()>>`
    Some(Ok(())) — записан; None — "не мой ключ". Вызывается ТОЛЬКО когда
    has(key, info) == Some(true), внутри уже открытой writeSection.

Соответствие upstream (TySh):
- int16 version (=1);
- 6×float64 transform;
- int16 textVersion (=50);
- 'TxLr'-дескриптор (`readVersionAndDescriptor`): 'Txt ', textGridding, Ornt,
  AntA, опц. bounds/boundingBox, TextIndex, EngineData (tdta);
- int16 warpVersion (=1);
- 'warp'-дескриптор: warpStyle/warpValue/warpPerspective/warpPerspectiveOther/
  warpRotate;
- float32 left/top/right/bottom.

EngineData-конвейер: read → parse_engine_data → decode_engine_data → merge в
LayerTextData; write → encode_engine_data → serialize_engine_data → tdta.
Нормализация `\r`<->`\n` повторяет upstream.

Txt2: read читает остаток секции как сырые байты EngineData2 и хранит их как
base64 (`info.engine_data`); write раскодирует base64 обратно в байты.
ВНИМАНИЕ (dependency gap): upstream Txt2.read дополнительно раскодирует
EngineData2 и проставляет `layer.text.textPath` по всем текстовым слоям psd
(нужен полный `Psd` и сортировка слоёв по text.index). В group-модуле такого
контекста нет (ReadCtx несёт только options/large), поэтому textPath здесь не
восстанавливается — это часть оркестрации более высокого уровня.
*/

use crate::additional_info::{ReadCtx, WriteCtx};
use crate::descriptor::{
    read_version_and_descriptor, write_version_and_descriptor, Descriptor, DescriptorValue,
};
use crate::engine_data::{parse_engine_data, serialize_engine_data};
use crate::helpers::{Dict, EnumCodec};
use crate::psd::{
    AntiAlias, LayerAdditionalInfo, LayerTextData, Orientation, TextGridding, UnitsBounds,
    UnitsValue, Warp, WarpStyle,
};
use crate::reader::{
    read_bytes, read_float32, read_float64, read_int16, skip_bytes, PsdReader, ReadError,
    ReadResult,
};
use crate::text::{decode_engine_data, encode_engine_data};
use crate::writer::{write_bytes, write_float32, write_float64, write_int16, PsdWriter};

// ===========================================================================
// Enum codecs (зеркало descriptor.ts: textGridding / Ornt / Annt / warpStyle)
// ===========================================================================

fn dict(pairs: &[(&str, &str)]) -> Dict {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn text_gridding_codec() -> EnumCodec {
    EnumCodec::new("textGridding", "none", dict(&[("none", "None"), ("round", "Rnd ")]))
}

fn ornt_codec() -> EnumCodec {
    EnumCodec::new(
        "Ornt",
        "horizontal",
        dict(&[("horizontal", "Hrzn"), ("vertical", "Vrtc")]),
    )
}

fn annt_codec() -> EnumCodec {
    EnumCodec::new(
        "Annt",
        "sharp",
        dict(&[
            ("none", "Anno"),
            ("sharp", "antiAliasSharp"),
            ("crisp", "AnCr"),
            ("strong", "AnSt"),
            ("smooth", "AnSm"),
            ("platform", "antiAliasPlatformGray"),
            ("platformLCD", "antiAliasPlatformLCD"),
        ]),
    )
}

fn warp_style_codec() -> EnumCodec {
    EnumCodec::new(
        "warpStyle",
        "none",
        dict(&[
            ("none", "warpNone"),
            ("arc", "warpArc"),
            ("arcLower", "warpArcLower"),
            ("arcUpper", "warpArcUpper"),
            ("arch", "warpArch"),
            ("bulge", "warpBulge"),
            ("shellLower", "warpShellLower"),
            ("shellUpper", "warpShellUpper"),
            ("flag", "warpFlag"),
            ("wave", "warpWave"),
            ("fish", "warpFish"),
            ("rise", "warpRise"),
            ("fisheye", "warpFisheye"),
            ("inflate", "warpInflate"),
            ("squeeze", "warpSqueeze"),
            ("twist", "warpTwist"),
            ("cylinder", "warpCylinder"),
            ("custom", "warpCustom"),
        ]),
    )
}

// --- Rust enum <-> TS string-union value conversions -----------------------

fn text_gridding_to_str(v: TextGridding) -> &'static str {
    match v {
        TextGridding::None => "none",
        TextGridding::Round => "round",
    }
}

fn text_gridding_from_str(s: &str) -> TextGridding {
    match s {
        "round" => TextGridding::Round,
        _ => TextGridding::None,
    }
}

fn orientation_to_str(v: Orientation) -> &'static str {
    match v {
        Orientation::Horizontal => "horizontal",
        Orientation::Vertical => "vertical",
    }
}

fn orientation_from_str(s: &str) -> Orientation {
    match s {
        "vertical" => Orientation::Vertical,
        _ => Orientation::Horizontal,
    }
}

fn anti_alias_to_str(v: AntiAlias) -> &'static str {
    match v {
        AntiAlias::None => "none",
        AntiAlias::Sharp => "sharp",
        AntiAlias::Crisp => "crisp",
        AntiAlias::Strong => "strong",
        AntiAlias::Smooth => "smooth",
        AntiAlias::Platform => "platform",
        AntiAlias::PlatformLcd => "platformLCD",
    }
}

fn anti_alias_from_str(s: &str) -> AntiAlias {
    match s {
        "none" => AntiAlias::None,
        "crisp" => AntiAlias::Crisp,
        "strong" => AntiAlias::Strong,
        "smooth" => AntiAlias::Smooth,
        "platform" => AntiAlias::Platform,
        "platformLCD" => AntiAlias::PlatformLcd,
        _ => AntiAlias::Sharp,
    }
}

fn warp_style_to_str(v: WarpStyle) -> &'static str {
    match v {
        WarpStyle::None => "none",
        WarpStyle::Arc => "arc",
        WarpStyle::ArcLower => "arcLower",
        WarpStyle::ArcUpper => "arcUpper",
        WarpStyle::Arch => "arch",
        WarpStyle::Bulge => "bulge",
        WarpStyle::ShellLower => "shellLower",
        WarpStyle::ShellUpper => "shellUpper",
        WarpStyle::Flag => "flag",
        WarpStyle::Wave => "wave",
        WarpStyle::Fish => "fish",
        WarpStyle::Rise => "rise",
        WarpStyle::Fisheye => "fisheye",
        WarpStyle::Inflate => "inflate",
        WarpStyle::Squeeze => "squeeze",
        WarpStyle::Twist => "twist",
        WarpStyle::Custom => "custom",
        WarpStyle::Cylinder => "cylinder",
    }
}

fn warp_style_from_str(s: &str) -> WarpStyle {
    match s {
        "arc" => WarpStyle::Arc,
        "arcLower" => WarpStyle::ArcLower,
        "arcUpper" => WarpStyle::ArcUpper,
        "arch" => WarpStyle::Arch,
        "bulge" => WarpStyle::Bulge,
        "shellLower" => WarpStyle::ShellLower,
        "shellUpper" => WarpStyle::ShellUpper,
        "flag" => WarpStyle::Flag,
        "wave" => WarpStyle::Wave,
        "fish" => WarpStyle::Fish,
        "rise" => WarpStyle::Rise,
        "fisheye" => WarpStyle::Fisheye,
        "inflate" => WarpStyle::Inflate,
        "squeeze" => WarpStyle::Squeeze,
        "twist" => WarpStyle::Twist,
        "custom" => WarpStyle::Custom,
        "cylinder" => WarpStyle::Cylinder,
        _ => WarpStyle::None,
    }
}

// ===========================================================================
// Units (зеркало parseUnits / unitsValue)
// ===========================================================================

// units helpers — канонически в descriptor.rs.
use crate::descriptor::parse_units;

/// Адаптер над `descriptor::units_value` (by-reference, всегда `Some`).
fn units_value(v: &UnitsValue) -> DescriptorValue {
    crate::descriptor::units_value(Some(v))
}

/// Зеркало `boundsToDescBounds(bounds)` — порядок полей: Left, 'Top ', Rght, Btom.
fn bounds_to_desc_bounds(bounds: &UnitsBounds, class_id: &str) -> DescriptorValue {
    let mut desc = Descriptor::new("", class_id);
    desc.set("Left", units_value(&bounds.left));
    desc.set("Top ", units_value(&bounds.top));
    desc.set("Rght", units_value(&bounds.right));
    desc.set("Btom", units_value(&bounds.bottom));
    DescriptorValue::Descriptor(desc)
}

/// Зеркало `descBoundsToBounds(desc)`.
fn desc_bounds_to_bounds(v: &DescriptorValue) -> ReadResult<UnitsBounds> {
    let desc = match v {
        DescriptorValue::Descriptor(d) => d,
        other => {
            return Err(ReadError::StrictViolation(format!(
                "Invalid bounds descriptor: {other:?}"
            )));
        }
    };
    let get = |k: &str| -> ReadResult<UnitsValue> {
        desc.get(k)
            .ok_or_else(|| ReadError::StrictViolation(format!("Missing bounds field: {k}")))
            .and_then(parse_units)
    };
    Ok(UnitsBounds {
        top: get("Top ")?,
        left: get("Left")?,
        right: get("Rght")?,
        bottom: get("Btom")?,
    })
}

// ===========================================================================
// Descriptor field helpers
// ===========================================================================

fn get_text(desc: &Descriptor, key: &str) -> Option<String> {
    match desc.get(key) {
        Some(DescriptorValue::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_enum(desc: &Descriptor, key: &str) -> Option<String> {
    match desc.get(key) {
        Some(DescriptorValue::Enum(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_integer(desc: &Descriptor, key: &str) -> Option<i32> {
    match desc.get(key) {
        Some(DescriptorValue::Integer(v)) => Some(*v),
        _ => None,
    }
}

fn get_double(desc: &Descriptor, key: &str) -> Option<f64> {
    match desc.get(key) {
        Some(DescriptorValue::Double(v)) => Some(*v),
        _ => None,
    }
}

// ===========================================================================
// Warp encode/decode (упрощённый набор полей, как в TySh-блоке upstream)
// ===========================================================================

/// Зеркало (упрощённого) `encodeWarp(warp)` для TySh: пишем
/// warpStyle/warpValue/warpPerspective/warpPerspectiveOther/warpRotate.
fn encode_warp(warp: &Warp) -> Descriptor {
    let mut desc = Descriptor::new("", "warp");
    let style = warp.style.map(warp_style_to_str);
    desc.set(
        "warpStyle",
        DescriptorValue::Enum(warp_style_codec().encode(style).expect("warpStyle encode")),
    );
    desc.set("warpValue", DescriptorValue::Double(warp.value.unwrap_or(0.0)));
    desc.set(
        "warpPerspective",
        DescriptorValue::Double(warp.perspective.unwrap_or(0.0)),
    );
    desc.set(
        "warpPerspectiveOther",
        DescriptorValue::Double(warp.perspective_other.unwrap_or(0.0)),
    );
    let rotate = warp.rotate.map(orientation_to_str);
    desc.set(
        "warpRotate",
        DescriptorValue::Enum(ornt_codec().encode(rotate).expect("warpRotate encode")),
    );
    desc
}

fn decode_warp(desc: &Descriptor) -> ReadResult<Warp> {
    let style_str = warp_style_codec()
        .decode(get_enum(desc, "warpStyle").as_deref().unwrap_or(""))
        .map_err(ReadError::StrictViolation)?;
    let rotate_str = ornt_codec()
        .decode(get_enum(desc, "warpRotate").as_deref().unwrap_or(""))
        .map_err(ReadError::StrictViolation)?;
    Ok(Warp {
        style: Some(warp_style_from_str(&style_str)),
        value: Some(get_double(desc, "warpValue").unwrap_or(0.0)),
        perspective: Some(get_double(desc, "warpPerspective").unwrap_or(0.0)),
        perspective_other: Some(get_double(desc, "warpPerspectiveOther").unwrap_or(0.0)),
        rotate: Some(orientation_from_str(&rotate_str)),
        ..Warp::default()
    })
}

// ===========================================================================
// base64 (зеркало fromByteArray / toByteArray; standard alphabet)
// ===========================================================================

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut chunks = data.chunks_exact(3);
    for c in &mut chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(B64[(n >> 18 & 0x3f) as usize] as char);
        out.push(B64[(n >> 12 & 0x3f) as usize] as char);
        out.push(B64[(n >> 6 & 0x3f) as usize] as char);
        out.push(B64[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(B64[(n >> 18 & 0x3f) as usize] as char);
            out.push(B64[(n >> 12 & 0x3f) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(B64[(n >> 18 & 0x3f) as usize] as char);
            out.push(B64[(n >> 12 & 0x3f) as usize] as char);
            out.push(B64[(n >> 6 & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

fn b64_val(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a' + 26) as u32),
        b'0'..=b'9' => Some((c - b'0' + 52) as u32),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn base64_decode(s: &str) -> ReadResult<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits = 0;
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for &c in s.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let v = b64_val(c)
            .ok_or_else(|| ReadError::StrictViolation(format!("Invalid base64 char: {c}")))?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits & 0xff) as u8);
        }
    }
    Ok(out)
}

// ===========================================================================
// Text normalization (`\r` <-> `\n`)
// ===========================================================================

/// upstream `text['Txt '].replace(/\r/g, '\n')`.
fn cr_to_lf(s: &str) -> String {
    s.replace('\r', "\n")
}

/// upstream `(text.text || '').replace(/\r?\n/g, '\r')`.
fn lf_to_cr(s: &str) -> String {
    s.replace("\r\n", "\r").replace('\n', "\r")
}

// ===========================================================================
// Read / has / write
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn read(
    key: &str,
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
    _ctx: &mut ReadCtx,
) -> ReadResult<Option<()>> {
    match key {
        "TySh" => {
            if read_int16(reader)? != 1 {
                return Err(ReadError::StrictViolation("Invalid TySh version".to_string()));
            }

            let mut transform = Vec::with_capacity(6);
            for _ in 0..6 {
                transform.push(read_float64(reader)?);
            }

            if read_int16(reader)? != 50 {
                return Err(ReadError::StrictViolation(
                    "Invalid TySh text version".to_string(),
                ));
            }
            let text_desc = read_version_and_descriptor(reader)?;

            if read_int16(reader)? != 1 {
                return Err(ReadError::StrictViolation(
                    "Invalid TySh warp version".to_string(),
                ));
            }
            let warp_desc = read_version_and_descriptor(reader)?;

            let raw_text = get_text(&text_desc, "Txt ").unwrap_or_default();
            let gridding = text_gridding_codec()
                .decode(get_enum(&text_desc, "textGridding").as_deref().unwrap_or(""))
                .map_err(ReadError::StrictViolation)?;
            let anti_alias = annt_codec()
                .decode(get_enum(&text_desc, "AntA").as_deref().unwrap_or(""))
                .map_err(ReadError::StrictViolation)?;
            let orientation = ornt_codec()
                .decode(get_enum(&text_desc, "Ornt").as_deref().unwrap_or(""))
                .map_err(ReadError::StrictViolation)?;

            let mut text = LayerTextData {
                transform: Some(transform),
                left: Some(read_float32(reader)? as f64),
                top: Some(read_float32(reader)? as f64),
                right: Some(read_float32(reader)? as f64),
                bottom: Some(read_float32(reader)? as f64),
                text: cr_to_lf(&raw_text),
                index: Some(get_integer(&text_desc, "TextIndex").unwrap_or(0) as f64),
                gridding: Some(text_gridding_from_str(&gridding)),
                anti_alias: Some(anti_alias_from_str(&anti_alias)),
                orientation: Some(orientation_from_str(&orientation)),
                warp: Some(decode_warp(&warp_desc)?),
                ..LayerTextData::default()
            };

            if let Some(b) = text_desc.get("bounds") {
                text.bounds = Some(desc_bounds_to_bounds(b)?);
            }
            if let Some(b) = text_desc.get("boundingBox") {
                text.bounding_box = Some(desc_bounds_to_bounds(b)?);
            }

            if let Some(DescriptorValue::RawData(bytes)) = text_desc.get("EngineData") {
                let engine_data = parse_engine_data(bytes)
                    .map_err(|e| ReadError::StrictViolation(format!("EngineData: {e:?}")))?;
                let text_data = decode_engine_data(&engine_data);
                // merge: { ...target.text, ...textData } — поля textData
                // перекрывают, остальные (transform/left/.../warp) остаются.
                merge_text_data(&mut text, text_data);
            }

            info.text = Some(text);

            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }
        "Txt2" => {
            let n = left(reader);
            let data = read_bytes(reader, n)?;
            info.engine_data = Some(base64_encode(&data));
            // dependency gap: textPath (engineData2 -> layers) не восстанавливается
            // здесь — нет доступа к полному Psd (см. шапку файла).
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }
        _ => Ok(None),
    }
}

/// merge `{ ...target.text, ...textData }`: textData перекрывает только
/// присутствующие в нём поля. encode/decode_engine_data покрывает богатый набор
/// полей; transform/left/top/right/bottom/bounds/boundingBox EngineData не несёт,
/// поэтому копируем именно "стилевые" поля из textData поверх.
fn merge_text_data(target: &mut LayerTextData, src: LayerTextData) {
    target.text = src.text;
    if src.anti_alias.is_some() {
        target.anti_alias = src.anti_alias;
    }
    if src.orientation.is_some() {
        target.orientation = src.orientation;
    }
    if src.gridding.is_some() {
        target.gridding = src.gridding;
    }
    if src.grid_info.is_some() {
        target.grid_info = src.grid_info;
    }
    if src.use_fractional_glyph_widths.is_some() {
        target.use_fractional_glyph_widths = src.use_fractional_glyph_widths;
    }
    if src.style.is_some() {
        target.style = src.style;
    }
    if src.style_runs.is_some() {
        target.style_runs = src.style_runs;
    }
    if src.paragraph_style.is_some() {
        target.paragraph_style = src.paragraph_style;
    }
    if src.paragraph_style_runs.is_some() {
        target.paragraph_style_runs = src.paragraph_style_runs;
    }
    if src.superscript_size.is_some() {
        target.superscript_size = src.superscript_size;
    }
    if src.superscript_position.is_some() {
        target.superscript_position = src.superscript_position;
    }
    if src.subscript_size.is_some() {
        target.subscript_size = src.subscript_size;
    }
    if src.subscript_position.is_some() {
        target.subscript_position = src.subscript_position;
    }
    if src.small_cap_size.is_some() {
        target.small_cap_size = src.small_cap_size;
    }
    if src.shape_type.is_some() {
        target.shape_type = src.shape_type;
    }
    if src.point_base.is_some() {
        target.point_base = src.point_base;
    }
    if src.box_bounds.is_some() {
        target.box_bounds = src.box_bounds;
    }
    if src.text_path.is_some() {
        target.text_path = src.text_path;
    }
}

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn has(key: &str, info: &LayerAdditionalInfo) -> Option<bool> {
    match key {
        "TySh" => Some(info.text.is_some()),
        "Txt2" => Some(info.engine_data.is_some()),
        _ => None,
    }
}

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn write(
    key: &str,
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    _ctx: &mut WriteCtx,
) -> Option<ReadResult<()>> {
    match key {
        "TySh" => {
            let text = match &info.text {
                Some(t) => t,
                None => return Some(Ok(())),
            };
            let default_warp = Warp::default();
            let warp = text.warp.as_ref().unwrap_or(&default_warp);
            let transform = text
                .transform
                .clone()
                .unwrap_or_else(|| vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

            // ---- text 'TxLr' descriptor ----
            let mut td = Descriptor::new("", "TxLr");
            td.set(
                "Txt ",
                DescriptorValue::Text(lf_to_cr(&text.text)),
            );
            let gridding = text.gridding.map(text_gridding_to_str);
            td.set(
                "textGridding",
                DescriptorValue::Enum(
                    text_gridding_codec().encode(gridding).expect("textGridding encode"),
                ),
            );
            let ornt = text.orientation.map(orientation_to_str);
            td.set(
                "Ornt",
                DescriptorValue::Enum(ornt_codec().encode(ornt).expect("Ornt encode")),
            );
            let anta = text.anti_alias.map(anti_alias_to_str);
            td.set(
                "AntA",
                DescriptorValue::Enum(annt_codec().encode(anta).expect("AntA encode")),
            );
            if let Some(b) = &text.bounds {
                td.set("bounds", bounds_to_desc_bounds(b, "bounds"));
            }
            if let Some(b) = &text.bounding_box {
                td.set("boundingBox", bounds_to_desc_bounds(b, "boundingBox"));
            }
            td.set(
                "TextIndex",
                DescriptorValue::Integer(text.index.unwrap_or(0.0) as i32),
            );
            let engine = encode_engine_data(text);
            let engine_bytes = serialize_engine_data(&engine, false);
            td.set("EngineData", DescriptorValue::RawData(engine_bytes));

            write_int16(writer, 1); // version

            for i in 0..6 {
                write_float64(writer, transform.get(i).copied().unwrap_or(0.0));
            }

            write_int16(writer, 50); // text version
            write_version_and_descriptor(writer, &td);

            write_int16(writer, 1); // warp version
            write_version_and_descriptor(writer, &encode_warp(warp));

            write_float32(writer, text.left.unwrap_or(0.0) as f32);
            write_float32(writer, text.top.unwrap_or(0.0) as f32);
            write_float32(writer, text.right.unwrap_or(0.0) as f32);
            write_float32(writer, text.bottom.unwrap_or(0.0) as f32);

            Some(Ok(()))
        }
        "Txt2" => {
            let b64 = match &info.engine_data {
                Some(s) => s,
                None => return Some(Ok(())),
            };
            match base64_decode(b64) {
                Ok(bytes) => {
                    write_bytes(writer, Some(&bytes));
                    Some(Ok(()))
                }
                Err(e) => Some(Err(e)),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::Units;
    use crate::additional_info::{
        read_additional_info_key, write_additional_info, ReadCtx, WriteCtx,
    };
    use crate::psd::{Font, ReadOptions, TextStyle, WriteOptions};
    use crate::reader::{read_section, read_signature, PsdReader};
    use crate::writer::{create_writer, get_writer_buffer};

    fn write_layer(info: &LayerAdditionalInfo) -> Vec<u8> {
        let opts = WriteOptions::default();
        let mut ctx = WriteCtx::new(&opts, false);
        let mut w = create_writer(1024);
        write_additional_info(&mut w, info, &mut ctx);
        get_writer_buffer(&w)
    }

    fn read_layer(bytes: &[u8]) -> LayerAdditionalInfo {
        let opts = ReadOptions::default();
        let mut info = LayerAdditionalInfo::default();
        let mut reader = PsdReader::new(bytes, None, None);

        while reader.offset + 8 <= reader.buffer.len() {
            let sig = read_signature(&mut reader).unwrap();
            assert!(sig == "8BIM" || sig == "8B64", "bad signature {sig}");
            let read_key = read_signature(&mut reader).unwrap();
            let large = sig == "8B64" || crate::additional_info::is_large_key(&read_key);
            let four_bytes = crate::additional_info::HANDLERS
                .iter()
                .find(|h| h.key == read_key)
                .map(|h| h.four_bytes)
                .unwrap_or(false);
            let round = if four_bytes { 4 } else { 2 };

            let mut ctx = ReadCtx { options: &opts, large };
            read_section::<(), _>(
                &mut reader,
                round,
                |r, left| {
                    let handled =
                        read_additional_info_key(&read_key, r, &mut info, left, &mut ctx)?;
                    if !handled || left(r) != 0 {
                        skip_bytes(r, left(r));
                    }
                    Ok(())
                },
                false,
                large,
            )
            .unwrap();
        }

        info
    }

    fn roundtrip(info: LayerAdditionalInfo) -> LayerAdditionalInfo {
        read_layer(&write_layer(&info))
    }

    fn sample_text() -> LayerTextData {
        LayerTextData {
            text: "Hello\nWorld".to_string(),
            transform: Some(vec![1.0, 0.0, 0.0, 1.0, 10.0, 20.0]),
            anti_alias: Some(AntiAlias::Smooth),
            gridding: Some(TextGridding::None),
            orientation: Some(Orientation::Horizontal),
            index: Some(0.0),
            left: Some(5.0),
            top: Some(6.0),
            right: Some(105.0),
            bottom: Some(56.0),
            warp: Some(Warp {
                style: Some(WarpStyle::Arc),
                value: Some(30.0),
                perspective: Some(0.0),
                perspective_other: Some(0.0),
                rotate: Some(Orientation::Horizontal),
                ..Warp::default()
            }),
            bounds: Some(UnitsBounds {
                top: UnitsValue { units: Units::Points, value: 0.0 },
                left: UnitsValue { units: Units::Points, value: 0.0 },
                right: UnitsValue { units: Units::Points, value: 100.0 },
                bottom: UnitsValue { units: Units::Points, value: 50.0 },
            }),
            style: Some(TextStyle {
                font: Some(Font {
                    name: "ArialMT".to_string(),
                    script: Some(0.0),
                    font_type: Some(1.0),
                    synthetic: Some(0.0),
                }),
                font_size: Some(24.0),
                ..TextStyle::default()
            }),
            ..LayerTextData::default()
        }
    }

    #[test]
    fn roundtrip_tysh_basic() {
        let info = LayerAdditionalInfo {
            text: Some(sample_text()),
            ..Default::default()
        };

        let out = roundtrip(info);
        let t = out.text.expect("text");

        // text normalized \n preserved
        assert_eq!(t.text, "Hello\nWorld");
        // transform survives
        assert_eq!(t.transform.as_deref(), Some(&[1.0, 0.0, 0.0, 1.0, 10.0, 20.0][..]));
        // bounds in float32 left/top/right/bottom
        assert_eq!(t.left, Some(5.0));
        assert_eq!(t.top, Some(6.0));
        assert_eq!(t.right, Some(105.0));
        assert_eq!(t.bottom, Some(56.0));
        assert_eq!(t.orientation, Some(Orientation::Horizontal));
        assert_eq!(t.gridding, Some(TextGridding::None));
        assert_eq!(t.anti_alias, Some(AntiAlias::Smooth));
    }

    #[test]
    fn roundtrip_tysh_warp() {
        let info = LayerAdditionalInfo {
            text: Some(sample_text()),
            ..Default::default()
        };

        let out = roundtrip(info);
        let w = out.text.expect("text").warp.expect("warp");
        assert_eq!(w.style, Some(WarpStyle::Arc));
        assert_eq!(w.value, Some(30.0));
        assert_eq!(w.rotate, Some(Orientation::Horizontal));
    }

    #[test]
    fn roundtrip_tysh_units_bounds() {
        let info = LayerAdditionalInfo {
            text: Some(sample_text()),
            ..Default::default()
        };

        let out = roundtrip(info);
        let b = out.text.expect("text").bounds.expect("bounds");
        assert_eq!(b.right.value, 100.0);
        assert_eq!(b.bottom.value, 50.0);
        assert_eq!(b.right.units, Units::Points);
    }

    #[test]
    fn roundtrip_tysh_style_font() {
        let info = LayerAdditionalInfo {
            text: Some(sample_text()),
            ..Default::default()
        };

        let out = roundtrip(info);
        let t = out.text.expect("text");
        let style = t.style.expect("style");
        assert_eq!(style.font.expect("font").name, "ArialMT");
        assert_eq!(style.font_size, Some(24.0));
    }

    #[test]
    fn roundtrip_txt2_engine_data() {
        // Build engine data bytes via the text pipeline, base64 them, round-trip.
        let engine = encode_engine_data(&sample_text());
        let bytes = serialize_engine_data(&engine, false);
        let b64 = base64_encode(&bytes);

        let info = LayerAdditionalInfo {
            engine_data: Some(b64.clone()),
            ..Default::default()
        };

        let out = roundtrip(info);
        assert_eq!(out.engine_data, Some(b64));
    }

    #[test]
    fn base64_round_trip() {
        for input in [
            &b""[..],
            &b"f"[..],
            &b"fo"[..],
            &b"foo"[..],
            &b"foob"[..],
            &b"fooba"[..],
            &b"foobar"[..],
            &[0u8, 255, 128, 1, 2, 3, 4][..],
        ] {
            let enc = base64_encode(input);
            let dec = base64_decode(&enc).expect("decode");
            assert_eq!(dec, input);
        }
    }

    #[test]
    fn every_enum_codec_default_is_a_map_key() {
        // The default must be a map KEY: `encode(None)` resolves through `map[def]`.
        for codec in [
            text_gridding_codec(),
            ornt_codec(),
            annt_codec(),
            warp_style_codec(),
        ] {
            assert!(codec.default_is_valid());
        }
    }

    #[test]
    fn annt_decodes_photoshop_2026_long_form() {
        // Photoshop 2026 writes the map key ('sharp') or its camelCase id instead of
        // the historical code ('antiAliasSharp'); both must resolve.
        let codec = annt_codec();
        assert_eq!(codec.decode("Annt.antiAliasSharp").unwrap(), "sharp");
        assert_eq!(codec.decode("Annt.sharp").unwrap(), "sharp");
        assert_eq!(codec.decode("Annt.platformLCD").unwrap(), "platformLCD");
        assert!(codec.decode("Annt.nonsense").is_err());
    }
}
