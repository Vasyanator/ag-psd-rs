/*
File: crates/ag-psd/src/image_resources.rs

Purpose:
image resource блоки PSD (ресурсы уровня документа).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/imageResources.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.

PORT STATUS: ported.

ARCHITECTURE — handler registry
================================
Upstream models per-resource handlers as an array `resourceHandlers[]` plus a
`resourceHandlersMap[key]`, each entry `{ key, has, read, write }`. In Rust a
`Vec<{ read: closure, write: closure }>` is awkward: the closures borrow the
typed `ImageResources` model mutably with differing capture sets and lifetimes,
and `read` needs the `left()` section-length callback. Instead the registry is
expressed as three free functions dispatching on the resource id:

  - [`has_image_resource`]  — mirror of each handler's `has` predicate (which
    resources are present, in upstream order — see [`RESOURCE_IDS`]);
  - [`read_image_resource`] — `match id { .. }` over the per-id read body;
  - [`write_image_resource`] — `match id { .. }` over the per-id write body.

This is the idiomatic, zero-cost Rust analog of `resourceHandlersMap[key]`: the
exact id numbers, signatures, padding and field math are preserved; only the
dispatch shape changes (a `match` instead of a closure table).

Upstream's `MOCK_HANDLERS && addHandler(...)` blocks are gated on
`helpers::MOCK_HANDLERS` which is `false`, so they are never registered. They are
left unported (they only stash raw bytes into `_irNNNN` debug fields that do not
exist on the typed model). The real (always-registered) handlers are all ported.

DEPENDENCY GAPS (cannot be closed without editing other modules)
================================================================
- `reader::read_color` is not yet ported (deferred in reader.rs). A minimal local
  [`read_color`] mirroring upstream `readColor` is provided here for id 1010.
- id 1075 (Timeline Information) needs `parseTrackList`/`serializeTrackList` and
  the `TimelineTrackDescriptor`/`FractionDescriptor` shape converters from
  descriptor.ts, none of which are ported in descriptor.rs. Porting them would
  require touching descriptor.rs, so 1075 is left as a TODO stub (see below).
- id 1036 thumbnail JPEG payload: jpeg.rs is a stub, so the compressed bytes are
  kept/emitted raw (`thumbnail_raw`); no encode/decode is performed.
*/

#![allow(clippy::too_many_lines)]

use crate::descriptor::{
    read_version_and_descriptor, write_version_and_descriptor, Descriptor, DescriptorValue,
};
use crate::helpers::{EnumCodec, MOCK_HANDLERS};
use crate::psd::{
    AnimationDispose, AnimationFrameInfo, AnimationInfo, Animations, CountInformation,
    GridAndGuidesInformation, GridInfo, GuideDirection, GuideInfo,
    ImageResources, LayerCompCapturedInfo, LayerCompListItem, LayerCompsResource, LtrbBounds,
    OnionSkins, PixelAspectRatio, PointF, PrintFlags, PrintInformation, PrintScale,
    PrintScaleStyle, ProofSetup, RenderingIntent, ResolutionInfo, ResolutionUnit, DimensionUnit,
    Rgb, Rgba, Slice, SliceAlignment, SliceBackgroundColorType, SliceGroup, SliceOrigin, SliceType,
    SheetDisclosure, SheetTimelineOption, ThumbnailRaw, UrlListItem, VersionInfo, BlendMode,
};
use crate::reader::{
    check_signature, read_ascii_string, read_bytes, read_color, read_float32, read_float64,
    read_int16, read_int32, read_fixed_point32, read_section, read_signature, read_uint16,
    read_uint32, read_uint8, read_unicode_string, skip_bytes, PsdReader, ReadError, ReadResult,
};
use crate::utf8::{decode_string, encode_string};
use crate::writer::{
    write_ascii_string, write_bytes, write_color, write_fixed_point32, write_float32,
    write_float64, write_int16, write_int32, write_section, write_signature, write_uint16,
    write_uint32, write_uint8, write_unicode_string, write_unicode_string_with_padding, PsdWriter,
};

// ===========================================================================
// Tables / small helpers (mirror module-level consts in imageResources.ts)
// ===========================================================================

/// Mirror `RESOLUTION_UNITS = [undefined, 'PPI', 'PPCM']` (1-based).
const RESOLUTION_UNITS: [Option<ResolutionUnit>; 3] =
    [None, Some(ResolutionUnit::Ppi), Some(ResolutionUnit::Ppcm)];

/// Mirror `MEASUREMENT_UNITS = [undefined, 'Inches', 'Centimeters', 'Points', 'Picas', 'Columns']`.
const MEASUREMENT_UNITS: [Option<DimensionUnit>; 6] = [
    None,
    Some(DimensionUnit::Inches),
    Some(DimensionUnit::Centimeters),
    Some(DimensionUnit::Points),
    Some(DimensionUnit::Picas),
    Some(DimensionUnit::Columns),
];

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Mirror `charToNibble`: ASCII hex digit -> its 0-15 value.
///
/// Accepts `'0'`-`'9'`, `'a'`-`'f'` and `'A'`-`'F'`. Any other byte is outside the
/// contract of the callers (they only ever pass bytes from a hex string produced by
/// [`HEX`] or read back from one) and yields a meaningless nibble.
fn char_to_nibble(code: u8) -> u8 {
    if code <= b'9' {
        code - b'0'
    } else if code >= b'a' {
        code - 87
    } else {
        // 'A'-'F': 0x41 - 55 == 10.
        code - 55
    }
}

/// Mirror `byteAt(value, index)`.
fn byte_at(value: &str, index: usize) -> u8 {
    let bytes = value.as_bytes();
    (char_to_nibble(bytes[index]) << 4) | char_to_nibble(bytes[index + 1])
}

/// Mirror `readUtf8String(reader, length)`.
fn read_utf8_string(reader: &mut PsdReader, length: usize) -> ReadResult<String> {
    let buffer = read_bytes(reader, length)?;
    Ok(decode_string(&buffer))
}

/// Mirror `writeUtf8String(writer, value)`.
fn write_utf8_string(writer: &mut PsdWriter, value: &str) {
    let buffer = encode_string(value);
    write_bytes(writer, Some(&buffer));
}

/// Mirror `readEncodedString(reader)`.
///
/// Reads a uint8 length, then `length` bytes. When a byte has the high bit set the
/// payload is legacy GBK; upstream tries `new TextDecoder('gbk')` and, since v31,
/// falls back to a plain UTF-8 decode when that decoder is unavailable in the host
/// runtime. The Rust port has no GBK decoder at all, so it always takes upstream's
/// fallback path — the same result a browser without the `gbk` label produces.
/// Pure-ASCII payloads (the common case) are byte-identical either way.
fn read_encoded_string(reader: &mut PsdReader) -> ReadResult<String> {
    let length = read_uint8(reader)? as usize;
    let buffer = read_bytes(reader, length)?;
    Ok(decode_string(&buffer))
}

/// Mirror `writeEncodedString(writer, value)`.
fn write_encoded_string(writer: &mut PsdWriter, value: &str) {
    // Replace any code point > 0x7f with '?'.
    let ascii: String = value
        .chars()
        .map(|c| if (c as u32) > 0x7f { '?' } else { c })
        .collect();
    let buffer = encode_string(&ascii);
    write_uint8(writer, buffer.len() as u8);
    write_bytes(writer, Some(&buffer));
}

// ===========================================================================
// EnumCodec instances (mirror createEnum<...> module-level consts)
// ===========================================================================

fn dict(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Mirror `Inte = createEnum<RenderingIntent>('Inte', 'perceptual', {...})`.
fn inte_codec() -> EnumCodec {
    EnumCodec::new(
        "Inte",
        "perceptual",
        dict(&[
            ("perceptual", "Img "),
            ("saturation", "Grp "),
            ("relative colorimetric", "Clrm"),
            ("absolute colorimetric", "AClr"),
        ]),
    )
}

/// Mirror `FrmD = createEnum<'auto'|'none'|'dispose'>('FrmD', 'auto', {...})`.
///
/// The default is the map KEY `'auto'`, not `''`: upstream's original `''` was not a
/// key, so `encode(None)` resolved `map['']` to `undefined` and emitted `"FrmD."`.
fn frmd_codec() -> EnumCodec {
    EnumCodec::new(
        "FrmD",
        "auto",
        dict(&[("auto", "Auto"), ("none", "None"), ("dispose", "Disp")]),
    )
}

// Slice enums (mirror ESlice* from descriptor.ts, defined locally as a gap).
fn eslice_type_codec() -> EnumCodec {
    EnumCodec::new(
        "ESliceType",
        "image",
        dict(&[("image", "Img "), ("noImage", "Nor ")]),
    )
}
fn eslice_horz_codec() -> EnumCodec {
    EnumCodec::new("ESliceHorzAlign", "default", dict(&[("default", "Dflt")]))
}
fn eslice_vert_codec() -> EnumCodec {
    EnumCodec::new("ESliceVertAlign", "default", dict(&[("default", "Dflt")]))
}
fn eslice_origin_codec() -> EnumCodec {
    EnumCodec::new(
        "ESliceOrigin",
        "userGenerated",
        dict(&[
            ("userGenerated", "userGenerated"),
            ("autoGenerated", "autoGenerated"),
            ("layer", "layer"),
        ]),
    )
}
fn eslice_bg_codec() -> EnumCodec {
    EnumCodec::new(
        "ESliceBGColorType",
        "none",
        dict(&[("none", "None"), ("matte", "Matt"), ("color", "Clr ")]),
    )
}

// ===========================================================================
// RenderingIntent <-> string helpers (model enum <-> EnumCodec string)
// ===========================================================================

fn rendering_intent_to_str(intent: RenderingIntent) -> &'static str {
    match intent {
        RenderingIntent::Perceptual => "perceptual",
        RenderingIntent::Saturation => "saturation",
        RenderingIntent::RelativeColorimetric => "relative colorimetric",
        RenderingIntent::AbsoluteColorimetric => "absolute colorimetric",
    }
}

fn rendering_intent_from_str(s: &str) -> RenderingIntent {
    match s {
        "saturation" => RenderingIntent::Saturation,
        "relative colorimetric" => RenderingIntent::RelativeColorimetric,
        "absolute colorimetric" => RenderingIntent::AbsoluteColorimetric,
        _ => RenderingIntent::Perceptual,
    }
}

// ===========================================================================
// Descriptor field accessors (typed-tree convenience)
// ===========================================================================

fn get_bool(desc: &Descriptor, key: &str) -> Option<bool> {
    match desc.get(key) {
        Some(DescriptorValue::Boolean(b)) => Some(*b),
        _ => None,
    }
}
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
fn get_int(desc: &Descriptor, key: &str) -> Option<i32> {
    match desc.get(key) {
        Some(DescriptorValue::Integer(i)) => Some(*i),
        _ => None,
    }
}
fn get_double(desc: &Descriptor, key: &str) -> Option<f64> {
    match desc.get(key) {
        Some(DescriptorValue::Double(d)) => Some(*d),
        Some(DescriptorValue::Integer(i)) => Some(*i as f64),
        _ => None,
    }
}
fn get_descriptor<'a>(desc: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    match desc.get(key) {
        Some(DescriptorValue::Descriptor(d)) => Some(d),
        _ => None,
    }
}
fn get_list<'a>(desc: &'a Descriptor, key: &str) -> Option<&'a Vec<DescriptorValue>> {
    match desc.get(key) {
        Some(DescriptorValue::List(l)) => Some(l),
        _ => None,
    }
}

// ===========================================================================
// Registry surface (ordered list of always-registered resource ids)
// ===========================================================================

/// Resource ids in upstream registration order (real, non-MOCK handlers).
///
/// Mirror of the order in which `addHandler(...)` runs for non-mock entries.
pub const RESOURCE_IDS: &[u16] = &[
    1061, // captionDigest
    1060, // xmpMetadata
    1082, // printInformation
    1005, // resolutionInfo
    1062, // printScale
    1006, // alphaChannelNames (encoded)
    1045, // alphaChannelNames (unicode)
    1053, // alphaIdentifiers
    1010, // backgroundColor
    1037, // globalAngle
    1049, // globalAltitude
    1011, // printFlags
    1034, // copyrighted
    1035, // url
    1080, // countInformation
    1024, // layerState
    1026, // layersGroup
    1072, // layerGroupsEnabledId
    1069, // layerSelectionIds
    1032, // gridAndGuidesInformation
    1065, // layerComps
    1078, // onionSkins
    1075, // timelineInformation (stub — see header)
    1076, // sheetDisclosure
    1054, // urlsList
    1050, // slices
    1064, // pixelAspectRatio
    1041, // iccUntaggedProfile
    1044, // idsSeedNumber
    1036, // thumbnail
    1057, // versionInfo
    7000, // imageReadyVariables
    7001, // imageReadyDataSets
    1088, // pathSelectionState
    4000, // animations
];

/// Mirror of each handler's `has(target)` predicate. Returns whether the resource
/// should be written, plus a count for `slices` (matches upstream returning a
/// number there). For most resources the count is 1.
pub fn has_image_resource(id: u16, target: &ImageResources) -> usize {
    let present = match id {
        1061 => target.caption_digest.is_some(),
        1060 => target.xmp_metadata.is_some(),
        1082 => target.print_information.is_some(),
        1005 => target.resolution_info.is_some(),
        1062 => target.print_scale.is_some(),
        1006 | 1045 => target.alpha_channel_names.is_some(),
        1053 => target.alpha_identifiers.is_some(),
        1010 => target.background_color.is_some(),
        1037 => target.global_angle.is_some(),
        1049 => target.global_altitude.is_some(),
        1011 => target.print_flags.is_some(),
        1034 => target.copyrighted.is_some(),
        1035 => target.url.is_some(),
        1080 => target.count_information.is_some(),
        1024 => target.layer_state.is_some(),
        1069 => target.layer_selection_ids.is_some(),
        1032 => target.grid_and_guides_information.is_some(),
        1065 => target.layer_comps.is_some(),
        1078 => target.onion_skins.is_some(),
        1075 => target.timeline_information.is_some(),
        1076 => target.sheet_disclosure.is_some(),
        1054 => target.urls_list.is_some(),
        1050 => return target.slices.as_ref().map_or(0, |s| s.len()),
        1064 => target.pixel_aspect_ratio.is_some(),
        1041 => target.icc_untagged_profile.is_some(),
        1044 => target.ids_seed_number.is_some(),
        1036 => target.thumbnail.is_some() || target.thumbnail_raw.is_some(),
        1057 => target.version_info.is_some(),
        7000 => target.image_ready_variables.is_some(),
        7001 => target.image_ready_data_sets.is_some(),
        1088 => target.path_selection_state.is_some(),
        4000 => target.animations.is_some(),
        // layersGroup / layerGroupsEnabledId are InternalImageResources-only
        // (1026 / 1072) and not on the public model.
        _ => false,
    };
    usize::from(present)
}

// ===========================================================================
// Read dispatch
// ===========================================================================

/// Mirror `resourceHandlersMap[key].read(reader, target, left)`.
///
/// `left` is the number of bytes remaining in the resource block (mirror of the
/// `left()` callback). Unknown ids are ignored (caller skips the block).
pub fn read_image_resource(
    id: u16,
    reader: &mut PsdReader,
    target: &mut ImageResources,
    left: usize,
) -> ReadResult<()> {
    match id {
        1061 => {
            let mut caption_digest = String::new();
            for _ in 0..16 {
                let byte = read_uint8(reader)?;
                caption_digest.push(HEX[(byte >> 4) as usize] as char);
                caption_digest.push(HEX[(byte & 0xf) as usize] as char);
            }
            target.caption_digest = Some(caption_digest);
        }
        1060 => {
            target.xmp_metadata = Some(read_utf8_string(reader, left)?);
        }
        1082 => {
            let desc = read_version_and_descriptor(reader)?;
            let intent = inte_codec()
                .decode(&get_enum(&desc, "Inte").unwrap_or_else(|| "Inte.Img ".to_string()))
                .unwrap_or_else(|_| "perceptual".to_string());

            let mut info = PrintInformation {
                printer_name: Some(get_text(&desc, "printerName").unwrap_or_default()),
                rendering_intent: Some(rendering_intent_from_str(&intent)),
                ..Default::default()
            };

            if let Some(v) = get_bool(&desc, "PstS") {
                info.printer_manages_colors = Some(v);
            }
            if let Some(v) = get_text(&desc, "Nm  ") {
                info.printer_profile = Some(v);
            }
            if let Some(v) = get_bool(&desc, "MpBl") {
                info.black_point_compensation = Some(v);
            }
            if let Some(v) = get_bool(&desc, "printSixteenBit") {
                info.print_sixteen_bit = Some(v);
            }
            if let Some(v) = get_bool(&desc, "hardProof") {
                info.hard_proof = Some(v);
            }
            if let Some(setup) = get_descriptor(&desc, "printProofSetup") {
                if let Some(DescriptorValue::Enum(bltn)) = setup.get("Bltn") {
                    let builtin = bltn.split('.').nth(1).unwrap_or("").to_string();
                    info.proof_setup = Some(ProofSetup::Builtin { builtin });
                } else if let Some(DescriptorValue::Text(bltn)) = setup.get("Bltn") {
                    let builtin = bltn.split('.').nth(1).unwrap_or("").to_string();
                    info.proof_setup = Some(ProofSetup::Builtin { builtin });
                } else {
                    let intent = inte_codec()
                        .decode(
                            &get_enum(setup, "Inte").unwrap_or_else(|| "Inte.Img ".to_string()),
                        )
                        .unwrap_or_else(|_| "perceptual".to_string());
                    info.proof_setup = Some(ProofSetup::Profile {
                        profile: get_text(setup, "profile").unwrap_or_default(),
                        rendering_intent: Some(rendering_intent_from_str(&intent)),
                        black_point_compensation: Some(get_bool(setup, "MpBl").unwrap_or(false)),
                        paper_white: Some(get_bool(setup, "paperWhite").unwrap_or(false)),
                    });
                }
            }

            target.print_information = Some(info);
        }
        1005 => {
            let horizontal_resolution = read_fixed_point32(reader)?;
            let horizontal_resolution_unit = read_uint16(reader)? as usize;
            let width_unit = read_uint16(reader)? as usize;
            let vertical_resolution = read_fixed_point32(reader)?;
            let vertical_resolution_unit = read_uint16(reader)? as usize;
            let height_unit = read_uint16(reader)? as usize;

            target.resolution_info = Some(ResolutionInfo {
                horizontal_resolution,
                horizontal_resolution_unit: RESOLUTION_UNITS
                    .get(horizontal_resolution_unit)
                    .copied()
                    .flatten()
                    .unwrap_or(ResolutionUnit::Ppi),
                width_unit: MEASUREMENT_UNITS
                    .get(width_unit)
                    .copied()
                    .flatten()
                    .unwrap_or(DimensionUnit::Inches),
                vertical_resolution,
                vertical_resolution_unit: RESOLUTION_UNITS
                    .get(vertical_resolution_unit)
                    .copied()
                    .flatten()
                    .unwrap_or(ResolutionUnit::Ppi),
                height_unit: MEASUREMENT_UNITS
                    .get(height_unit)
                    .copied()
                    .flatten()
                    .unwrap_or(DimensionUnit::Inches),
            });
        }
        1062 => {
            let style_index = read_int16(reader)?;
            let style = match style_index {
                0 => Some(PrintScaleStyle::Centered),
                1 => Some(PrintScaleStyle::SizeToFit),
                2 => Some(PrintScaleStyle::UserDefined),
                _ => None,
            };
            target.print_scale = Some(PrintScale {
                style,
                x: Some(read_float32(reader)? as f64),
                y: Some(read_float32(reader)? as f64),
                scale: Some(read_float32(reader)? as f64),
            });
        }
        1006 => {
            // skip if the unicode versions are already read
            if target.alpha_channel_names.is_none() {
                let mut names = Vec::new();
                let end = reader.offset + left;
                while reader.offset < end {
                    names.push(read_encoded_string(reader)?);
                }
                target.alpha_channel_names = Some(names);
            } else {
                skip_bytes(reader, left);
            }
        }
        1045 => {
            let mut names = Vec::new();
            let end = reader.offset + left;
            while reader.offset < end {
                names.push(read_unicode_string(reader)?);
            }
            target.alpha_channel_names = Some(names);
        }
        1053 => {
            let mut ids = Vec::new();
            let end = reader.offset + left;
            while end.saturating_sub(reader.offset) >= 4 {
                ids.push(read_uint32(reader)? as f64);
            }
            target.alpha_identifiers = Some(ids);
        }
        1010 => {
            target.background_color = Some(read_color(reader)?);
        }
        1037 => {
            target.global_angle = Some(read_int32(reader)? as f64);
        }
        1049 => {
            target.global_altitude = Some(read_uint32(reader)? as f64);
        }
        1011 => {
            target.print_flags = Some(PrintFlags {
                labels: Some(read_uint8(reader)? != 0),
                crop_marks: Some(read_uint8(reader)? != 0),
                color_bars: Some(read_uint8(reader)? != 0),
                registration_marks: Some(read_uint8(reader)? != 0),
                negative: Some(read_uint8(reader)? != 0),
                flip: Some(read_uint8(reader)? != 0),
                interpolate: Some(read_uint8(reader)? != 0),
                caption: Some(read_uint8(reader)? != 0),
                print_flags: Some(read_uint8(reader)? != 0),
            });
        }
        1034 => {
            target.copyrighted = Some(read_uint8(reader)? != 0);
        }
        1035 => {
            target.url = Some(read_ascii_string(reader, left)?);
        }
        1080 => {
            let desc = read_version_and_descriptor(reader)?;
            let mut groups = Vec::new();
            if let Some(list) = get_list(&desc, "countGroupList") {
                for item in list {
                    if let DescriptorValue::Descriptor(g) = item {
                        let mut points = Vec::new();
                        if let Some(plist) = get_list(g, "countObjectList") {
                            for p in plist {
                                if let DescriptorValue::Descriptor(pd) = p {
                                    points.push(PointF {
                                        x: get_double(pd, "X   ").unwrap_or(0.0),
                                        y: get_double(pd, "Y   ").unwrap_or(0.0),
                                    });
                                }
                            }
                        }
                        groups.push(CountInformation {
                            color: Rgb {
                                r: get_double(g, "Rd  ").unwrap_or(0.0),
                                g: get_double(g, "Grn ").unwrap_or(0.0),
                                b: get_double(g, "Bl  ").unwrap_or(0.0),
                            },
                            name: get_text(g, "Nm  ").unwrap_or_default(),
                            size: get_double(g, "Rds ").unwrap_or(0.0),
                            font_size: get_double(g, "fontSize").unwrap_or(0.0),
                            visible: get_bool(g, "Vsbl").unwrap_or(false),
                            points,
                        });
                    }
                }
            }
            target.count_information = Some(groups);
        }
        1024 => {
            target.layer_state = Some(read_uint16(reader)? as f64);
        }
        1026 => {
            // InternalImageResources.layersGroup — not on public model; skip.
            skip_bytes(reader, left);
        }
        1072 => {
            // InternalImageResources.layerGroupsEnabledId — not on public model; skip.
            skip_bytes(reader, left);
        }
        1069 => {
            let mut count = read_uint16(reader)?;
            let mut ids = Vec::new();
            while count > 0 {
                count -= 1;
                ids.push(read_uint32(reader)? as f64);
            }
            target.layer_selection_ids = Some(ids);
        }
        1032 => {
            let version = read_uint32(reader)?;
            let horizontal = read_uint32(reader)? as f64;
            let vertical = read_uint32(reader)? as f64;
            let count = read_uint32(reader)?;

            if version != 1 {
                return Err(ReadError::StrictViolation(format!(
                    "Invalid 1032 resource version: {version}"
                )));
            }

            let mut guides = Vec::new();
            for _ in 0..count {
                let location = read_uint32(reader)? as f64 / 32.0;
                let direction = if read_uint8(reader)? != 0 {
                    GuideDirection::Horizontal
                } else {
                    GuideDirection::Vertical
                };
                guides.push(GuideInfo {
                    location,
                    direction,
                });
            }

            target.grid_and_guides_information = Some(GridAndGuidesInformation {
                grid: Some(GridInfo {
                    horizontal,
                    vertical,
                }),
                guides: Some(guides),
            });
        }
        1065 => {
            let desc = read_version_and_descriptor(reader)?;
            let mut list = Vec::new();
            if let Some(items) = get_list(&desc, "list") {
                for item in items {
                    if let DescriptorValue::Descriptor(it) = item {
                        let comment = get_text(it, "comment");
                        list.push(LayerCompListItem {
                            id: get_double(it, "compID").unwrap_or(0.0),
                            name: get_text(it, "Nm  ").unwrap_or_default(),
                            comment,
                            captured_info: captured_info_from_num(
                                get_double(it, "capturedInfo").unwrap_or(0.0),
                            ),
                        });
                    }
                }
            }
            let last_applied = get_double(&desc, "lastAppliedComp");
            target.layer_comps = Some(LayerCompsResource {
                list,
                last_applied,
            });
        }
        1078 => {
            let desc = read_version_and_descriptor(reader)?;
            let blnm = get_int(&desc, "BlnM").unwrap_or(0);
            target.onion_skins = Some(OnionSkins {
                enabled: get_bool(&desc, "enab").unwrap_or(false),
                frames_before: get_double(&desc, "numBefore").unwrap_or(0.0),
                frames_after: get_double(&desc, "numAfter").unwrap_or(0.0),
                frame_spacing: get_double(&desc, "Spcn").unwrap_or(0.0),
                min_opacity: get_double(&desc, "minOpacity").unwrap_or(0.0) / 100.0,
                max_opacity: get_double(&desc, "maxOpacity").unwrap_or(0.0) / 100.0,
                blend_mode: onion_skin_blend_mode(blnm),
            });
        }
        1075 => {
            // TODO: needs parseTrackList/serializeTrackList + TimelineTrackDescriptor
            // shape converters from descriptor.ts (not ported in descriptor.rs).
            // Skipping payload to keep the rest of the block framing intact.
            skip_bytes(reader, left);
        }
        1076 => {
            let desc = read_version_and_descriptor(reader)?;
            let mut disclosure = SheetDisclosure::default();
            if let Some(list) = get_list(&desc, "sheetTimelineOptions") {
                let mut options = Vec::new();
                for item in list {
                    if let DescriptorValue::Descriptor(o) = item {
                        options.push(SheetTimelineOption {
                            sheet_id: get_double(o, "sheetID").unwrap_or(0.0),
                            sheet_disclosed: get_bool(o, "sheetDisclosed").unwrap_or(false),
                            lights_disclosed: get_bool(o, "lightsDisclosed").unwrap_or(false),
                            meshes_disclosed: get_bool(o, "meshesDisclosed").unwrap_or(false),
                            materials_disclosed: get_bool(o, "materialsDisclosed").unwrap_or(false),
                        });
                    }
                }
                disclosure.sheet_timeline_options = Some(options);
            }
            target.sheet_disclosure = Some(disclosure);
        }
        1054 => {
            let count = read_uint32(reader)?;
            let mut list = Vec::new();
            for _ in 0..count {
                let long = read_signature(reader)?;
                if long != "slic" && reader.options.throw_for_missing_features == Some(true) {
                    return Err(ReadError::StrictViolation("Unknown long".to_string()));
                }
                let id = read_uint32(reader)? as f64;
                let url = read_unicode_string(reader)?;
                list.push(UrlListItem {
                    id,
                    url,
                    r#ref: "slice".to_string(),
                });
            }
            target.urls_list = Some(list);
        }
        1050 => {
            read_slices(reader, target)?;
        }
        1064 => {
            if read_uint32(reader)? > 2 {
                return Err(ReadError::StrictViolation(
                    "Invalid pixelAspectRatio version".to_string(),
                ));
            }
            target.pixel_aspect_ratio = Some(PixelAspectRatio {
                aspect: read_float64(reader)?,
            });
        }
        1041 => {
            target.icc_untagged_profile = Some(read_uint8(reader)? != 0);
        }
        1044 => {
            target.ids_seed_number = Some(read_uint32(reader)? as f64);
        }
        1036 => {
            read_thumbnail(reader, target, left)?;
        }
        1057 => {
            let version = read_uint32(reader)?;
            if version != 1 {
                return Err(ReadError::StrictViolation(
                    "Invalid versionInfo version".to_string(),
                ));
            }
            target.version_info = Some(VersionInfo {
                has_real_merged_data: read_uint8(reader)? != 0,
                writer_name: read_unicode_string(reader)?,
                reader_name: read_unicode_string(reader)?,
                file_version: read_uint32(reader)? as f64,
            });
            // skipBytes(reader, left()) — already consumed; remaining skipped by caller.
            let consumed_end = reader.offset;
            let _ = consumed_end;
            skip_bytes(reader, 0);
        }
        7000 => {
            target.image_ready_variables = Some(read_utf8_string(reader, left)?);
        }
        7001 => {
            target.image_ready_data_sets = Some(read_utf8_string(reader, left)?);
        }
        1088 => {
            let desc = read_version_and_descriptor(reader)?;
            let mut paths = Vec::new();
            if let Some(list) = get_list(&desc, "null") {
                for item in list {
                    if let DescriptorValue::Text(s) = item {
                        paths.push(s.clone());
                    }
                }
            }
            target.path_selection_state = Some(paths);
        }
        4000 => {
            read_animations(reader, target, left)?;
        }
        _ => {
            // Unknown / MOCK-only id — caller skips the block.
        }
    }
    Ok(())
}

// ===========================================================================
// Write dispatch
// ===========================================================================

/// Mirror `resourceHandlersMap[key].write(writer, target, index)`.
pub fn write_image_resource(
    id: u16,
    writer: &mut PsdWriter,
    target: &ImageResources,
    index: usize,
) -> ReadResult<()> {
    match id {
        1061 => {
            let digest = target.caption_digest.as_deref().unwrap_or("");
            for i in 0..16 {
                write_uint8(writer, byte_at(digest, i * 2));
            }
        }
        1060 => {
            write_utf8_string(writer, target.xmp_metadata.as_deref().unwrap_or(""));
        }
        1082 => {
            let info = target.print_information.as_ref().unwrap();
            let mut desc = Descriptor::new("", "printOutput");

            if info.printer_manages_colors == Some(true) {
                desc.set("PstS", DescriptorValue::Boolean(true));
            } else {
                if let Some(hp) = info.hard_proof {
                    desc.set("hardProof", DescriptorValue::Boolean(hp));
                }
                desc.set("ClrS", DescriptorValue::Enum("ClrS.RGBC".to_string()));
                desc.set(
                    "Nm  ",
                    DescriptorValue::Text(
                        info.printer_profile
                            .clone()
                            .unwrap_or_else(|| "CIE RGB".to_string()),
                    ),
                );
            }

            let intent = info.rendering_intent.unwrap_or(RenderingIntent::Perceptual);
            desc.set(
                "Inte",
                DescriptorValue::Enum(
                    inte_codec()
                        .encode(Some(rendering_intent_to_str(intent)))
                        .unwrap(),
                ),
            );

            if info.printer_manages_colors != Some(true) {
                desc.set(
                    "MpBl",
                    DescriptorValue::Boolean(info.black_point_compensation.unwrap_or(false)),
                );
            }

            desc.set(
                "printSixteenBit",
                DescriptorValue::Boolean(info.print_sixteen_bit.unwrap_or(false)),
            );
            desc.set(
                "printerName",
                DescriptorValue::Text(info.printer_name.clone().unwrap_or_default()),
            );

            match &info.proof_setup {
                Some(ProofSetup::Profile {
                    profile,
                    rendering_intent,
                    black_point_compensation,
                    paper_white,
                }) => {
                    let mut sub = Descriptor::new("", "prfP");
                    sub.set("profile", DescriptorValue::Text(profile.clone()));
                    let pi = rendering_intent.unwrap_or(RenderingIntent::Perceptual);
                    sub.set(
                        "Inte",
                        DescriptorValue::Enum(
                            inte_codec().encode(Some(rendering_intent_to_str(pi))).unwrap(),
                        ),
                    );
                    sub.set(
                        "MpBl",
                        DescriptorValue::Boolean(black_point_compensation.unwrap_or(false)),
                    );
                    sub.set(
                        "paperWhite",
                        DescriptorValue::Boolean(paper_white.unwrap_or(false)),
                    );
                    desc.set("printProofSetup", DescriptorValue::Descriptor(sub));
                }
                other => {
                    let builtin = match other {
                        Some(ProofSetup::Builtin { builtin }) if !builtin.is_empty() => {
                            format!("builtinProof.{builtin}")
                        }
                        _ => "builtinProof.proofCMYK".to_string(),
                    };
                    let mut sub = Descriptor::new("", "prfP");
                    sub.set("Bltn", DescriptorValue::Enum(builtin));
                    desc.set("printProofSetup", DescriptorValue::Descriptor(sub));
                }
            }

            write_version_and_descriptor(writer, &desc);
        }
        1005 => {
            let info = target.resolution_info.as_ref().unwrap();
            write_fixed_point32(writer, info.horizontal_resolution);
            write_uint16(writer, resolution_unit_index(info.horizontal_resolution_unit));
            write_uint16(writer, measurement_unit_index(info.width_unit));
            write_fixed_point32(writer, info.vertical_resolution);
            write_uint16(writer, resolution_unit_index(info.vertical_resolution_unit));
            write_uint16(writer, measurement_unit_index(info.height_unit));
        }
        1062 => {
            let ps = target.print_scale.as_ref().unwrap();
            let style_index = match ps.style {
                Some(PrintScaleStyle::Centered) => 0,
                Some(PrintScaleStyle::SizeToFit) => 1,
                Some(PrintScaleStyle::UserDefined) => 2,
                None => 0,
            };
            write_int16(writer, style_index);
            write_float32(writer, ps.x.unwrap_or(0.0) as f32);
            write_float32(writer, ps.y.unwrap_or(0.0) as f32);
            write_float32(writer, ps.scale.unwrap_or(0.0) as f32);
        }
        1006 => {
            for name in target.alpha_channel_names.as_ref().unwrap() {
                write_encoded_string(writer, name);
            }
        }
        1045 => {
            for name in target.alpha_channel_names.as_ref().unwrap() {
                write_unicode_string_with_padding(writer, name);
            }
        }
        1053 => {
            for id in target.alpha_identifiers.as_ref().unwrap() {
                write_uint32(writer, *id as u32);
            }
        }
        1010 => {
            write_color(writer, target.background_color.as_ref());
        }
        1037 => {
            write_int32(writer, target.global_angle.unwrap() as i32);
        }
        1049 => {
            write_uint32(writer, target.global_altitude.unwrap() as u32);
        }
        1011 => {
            let f = target.print_flags.as_ref().unwrap();
            for b in [
                f.labels,
                f.crop_marks,
                f.color_bars,
                f.registration_marks,
                f.negative,
                f.flip,
                f.interpolate,
                f.caption,
                f.print_flags,
            ] {
                write_uint8(writer, u8::from(b.unwrap_or(false)));
            }
        }
        1034 => {
            write_uint8(writer, u8::from(target.copyrighted.unwrap_or(false)));
        }
        1035 => {
            write_ascii_string(writer, target.url.as_deref().unwrap());
        }
        1080 => {
            let mut desc = Descriptor::new("", "Cnt ");
            desc.set("Vrsn", DescriptorValue::Integer(1));
            let mut group_list = Vec::new();
            for g in target.count_information.as_ref().unwrap() {
                let mut gd = Descriptor::new("", "cntG");
                gd.set("Rd  ", DescriptorValue::Integer(g.color.r as i32));
                gd.set("Grn ", DescriptorValue::Integer(g.color.g as i32));
                gd.set("Bl  ", DescriptorValue::Integer(g.color.b as i32));
                gd.set("Nm  ", DescriptorValue::Text(g.name.clone()));
                gd.set("Rds ", DescriptorValue::Integer(g.size as i32));
                gd.set("fontSize", DescriptorValue::Integer(g.font_size as i32));
                gd.set("Vsbl", DescriptorValue::Boolean(g.visible));
                let mut points = Vec::new();
                for p in &g.points {
                    let mut pd = Descriptor::new("", "cntO");
                    pd.set("X   ", DescriptorValue::Integer(p.x as i32));
                    pd.set("Y   ", DescriptorValue::Integer(p.y as i32));
                    points.push(DescriptorValue::Descriptor(pd));
                }
                gd.set("countObjectList", DescriptorValue::List(points));
                group_list.push(DescriptorValue::Descriptor(gd));
            }
            desc.set("countGroupList", DescriptorValue::List(group_list));
            write_version_and_descriptor(writer, &desc);
        }
        1024 => {
            write_uint16(writer, target.layer_state.unwrap() as u16);
        }
        1069 => {
            let ids = target.layer_selection_ids.as_ref().unwrap();
            write_uint16(writer, ids.len() as u16);
            for id in ids {
                write_uint32(writer, *id as u32);
            }
        }
        1032 => {
            let info = target.grid_and_guides_information.as_ref().unwrap();
            let grid = info.grid.unwrap_or(GridInfo {
                horizontal: 18.0 * 32.0,
                vertical: 18.0 * 32.0,
            });
            let empty = Vec::new();
            let guides = info.guides.as_ref().unwrap_or(&empty);
            write_uint32(writer, 1);
            write_uint32(writer, grid.horizontal as u32);
            write_uint32(writer, grid.vertical as u32);
            write_uint32(writer, guides.len() as u32);
            for g in guides {
                write_uint32(writer, (g.location * 32.0) as u32);
                write_uint8(
                    writer,
                    u8::from(g.direction == GuideDirection::Horizontal),
                );
            }
        }
        1065 => {
            let lc = target.layer_comps.as_ref().unwrap();
            let mut desc = Descriptor::new("", "CompList");
            let mut list = Vec::new();
            for item in &lc.list {
                let mut t = Descriptor::new("", "Comp");
                t.set("Nm  ", DescriptorValue::Text(item.name.clone()));
                if let Some(comment) = &item.comment {
                    t.set("comment", DescriptorValue::Text(comment.clone()));
                }
                t.set("compID", DescriptorValue::Integer(item.id as i32));
                t.set(
                    "capturedInfo",
                    DescriptorValue::Integer(item.captured_info as i32),
                );
                list.push(DescriptorValue::Descriptor(t));
            }
            desc.set("list", DescriptorValue::List(list));
            if let Some(last) = lc.last_applied {
                desc.set("lastAppliedComp", DescriptorValue::Integer(last as i32));
            }
            write_version_and_descriptor(writer, &desc);
        }
        1078 => {
            let os = target.onion_skins.as_ref().unwrap();
            let mut desc = Descriptor::new("", "null");
            desc.set("Vrsn", DescriptorValue::Integer(1));
            desc.set("enab", DescriptorValue::Boolean(os.enabled));
            desc.set("numBefore", DescriptorValue::Integer(os.frames_before as i32));
            desc.set("numAfter", DescriptorValue::Integer(os.frames_after as i32));
            desc.set("Spcn", DescriptorValue::Integer(os.frame_spacing as i32));
            desc.set(
                "minOpacity",
                DescriptorValue::Integer((os.min_opacity * 100.0) as i32),
            );
            desc.set(
                "maxOpacity",
                DescriptorValue::Integer((os.max_opacity * 100.0) as i32),
            );
            desc.set(
                "BlnM",
                DescriptorValue::Integer(onion_skin_blend_index(os.blend_mode)),
            );
            write_version_and_descriptor(writer, &desc);
        }
        1075 => {
            // TODO: needs serializeTrackList + TimelineTrackDescriptor (not ported).
            // Nothing emitted (caller must avoid registering 1075 for now).
        }
        1076 => {
            let d = target.sheet_disclosure.as_ref().unwrap();
            let mut desc = Descriptor::new("", "null");
            desc.set("Vrsn", DescriptorValue::Integer(1));
            if let Some(opts) = &d.sheet_timeline_options {
                let mut list = Vec::new();
                for o in opts {
                    let mut od = Descriptor::new("", "shtT");
                    od.set("Vrsn", DescriptorValue::Integer(2));
                    od.set("sheetID", DescriptorValue::Integer(o.sheet_id as i32));
                    od.set("sheetDisclosed", DescriptorValue::Boolean(o.sheet_disclosed));
                    od.set(
                        "lightsDisclosed",
                        DescriptorValue::Boolean(o.lights_disclosed),
                    );
                    od.set(
                        "meshesDisclosed",
                        DescriptorValue::Boolean(o.meshes_disclosed),
                    );
                    od.set(
                        "materialsDisclosed",
                        DescriptorValue::Boolean(o.materials_disclosed),
                    );
                    list.push(DescriptorValue::Descriptor(od));
                }
                desc.set("sheetTimelineOptions", DescriptorValue::List(list));
            }
            write_version_and_descriptor(writer, &desc);
        }
        1054 => {
            let list = target.urls_list.as_ref().unwrap();
            write_uint32(writer, list.len() as u32);
            for item in list {
                write_signature(writer, "slic");
                write_uint32(writer, item.id as u32);
                write_unicode_string(writer, &item.url);
            }
        }
        1050 => {
            write_slices(writer, target, index);
        }
        1064 => {
            write_uint32(writer, 2); // version
            write_float64(writer, target.pixel_aspect_ratio.as_ref().unwrap().aspect);
        }
        1041 => {
            write_uint8(
                writer,
                u8::from(target.icc_untagged_profile.unwrap_or(false)),
            );
        }
        1044 => {
            write_uint32(writer, target.ids_seed_number.unwrap() as u32);
        }
        1036 => {
            write_thumbnail(writer, target);
        }
        1057 => {
            let vi = target.version_info.as_ref().unwrap();
            write_uint32(writer, 1);
            write_uint8(writer, u8::from(vi.has_real_merged_data));
            write_unicode_string(writer, &vi.writer_name);
            write_unicode_string(writer, &vi.reader_name);
            write_uint32(writer, vi.file_version as u32);
        }
        7000 => {
            write_utf8_string(writer, target.image_ready_variables.as_deref().unwrap());
        }
        7001 => {
            write_utf8_string(writer, target.image_ready_data_sets.as_deref().unwrap());
        }
        1088 => {
            let mut desc = Descriptor::new("", "null");
            let paths = target.path_selection_state.as_ref().unwrap();
            let list = paths
                .iter()
                .map(|s| DescriptorValue::Text(s.clone()))
                .collect();
            desc.set("null", DescriptorValue::List(list));
            write_version_and_descriptor(writer, &desc);
        }
        4000 => {
            write_animations(writer, target);
        }
        _ => {}
    }
    Ok(())
}

// ===========================================================================
// Unit-index helpers (mirror Math.max(1, ARRAY.indexOf(x)))
// ===========================================================================

fn resolution_unit_index(u: ResolutionUnit) -> u16 {
    match u {
        ResolutionUnit::Ppi => 1,
        ResolutionUnit::Ppcm => 2,
    }
}

fn measurement_unit_index(u: DimensionUnit) -> u16 {
    match u {
        DimensionUnit::Inches => 1,
        DimensionUnit::Centimeters => 2,
        DimensionUnit::Points => 3,
        DimensionUnit::Picas => 4,
        DimensionUnit::Columns => 5,
    }
}

// ===========================================================================
// LayerCompCapturedInfo <-> number
// ===========================================================================

fn captured_info_from_num(n: f64) -> LayerCompCapturedInfo {
    match n as i32 {
        1 => LayerCompCapturedInfo::Visibility,
        2 => LayerCompCapturedInfo::Position,
        4 => LayerCompCapturedInfo::Appearance,
        _ => LayerCompCapturedInfo::None,
    }
}

// ===========================================================================
// Onion-skin blend mode table (mirror onionSkinsBlendModes)
// ===========================================================================

fn onion_skin_blend_mode(index: i32) -> BlendMode {
    match index {
        7 => BlendMode::Multiply,
        8 => BlendMode::Screen,
        23 => BlendMode::Difference,
        _ => BlendMode::Normal,
    }
}

fn onion_skin_blend_index(mode: BlendMode) -> i32 {
    match mode {
        BlendMode::Multiply => 7,
        BlendMode::Screen => 8,
        BlendMode::Difference => 23,
        BlendMode::Normal => 0,
        _ => 0,
    }
}

// ===========================================================================
// Slices (id 1050)
// ===========================================================================

fn ltrb_from_bounds_desc(desc: &Descriptor) -> LtrbBounds {
    LtrbBounds {
        top: get_double(desc, "Top ").unwrap_or(0.0),
        left: get_double(desc, "Left").unwrap_or(0.0),
        bottom: get_double(desc, "Btom").unwrap_or(0.0),
        right: get_double(desc, "Rght").unwrap_or(0.0),
    }
}

fn bounds_desc_from_ltrb(b: &LtrbBounds) -> Descriptor {
    let mut d = Descriptor::new("", "Rct1");
    d.set("Top ", DescriptorValue::Integer(b.top as i32));
    d.set("Left", DescriptorValue::Integer(b.left as i32));
    d.set("Btom", DescriptorValue::Integer(b.bottom as i32));
    d.set("Rght", DescriptorValue::Integer(b.right as i32));
    d
}

fn slice_origin_from_index(i: u32) -> SliceOrigin {
    // ['autoGenerated', 'layer', 'userGenerated'], clamped.
    match i {
        0 => SliceOrigin::AutoGenerated,
        1 => SliceOrigin::Layer,
        _ => SliceOrigin::UserGenerated,
    }
}

fn slice_origin_index(o: SliceOrigin) -> u32 {
    match o {
        SliceOrigin::AutoGenerated => 0,
        SliceOrigin::Layer => 1,
        SliceOrigin::UserGenerated => 2,
    }
}

fn slice_type_from_index(i: u32) -> SliceType {
    // ['noImage', 'image'], clamped.
    if i == 0 {
        SliceType::NoImage
    } else {
        SliceType::Image
    }
}

fn slice_type_index(t: SliceType) -> u32 {
    match t {
        SliceType::NoImage => 0,
        SliceType::Image => 1,
    }
}

fn read_slices(reader: &mut PsdReader, target: &mut ImageResources) -> ReadResult<()> {
    let version = read_uint32(reader)?;

    if version == 6 {
        if target.slices.is_none() {
            target.slices = Some(Vec::new());
        }
        let top = read_int32(reader)? as f64;
        let left = read_int32(reader)? as f64;
        let bottom = read_int32(reader)? as f64;
        let right = read_int32(reader)? as f64;
        let group_name = read_unicode_string(reader)?;
        let count = read_uint32(reader)?;

        let mut slices = Vec::new();
        for _ in 0..count {
            let id = read_uint32(reader)? as f64;
            let group_id = read_uint32(reader)? as f64;
            let origin = slice_origin_from_index(read_uint32(reader)?);
            let associated_layer_id = if origin == SliceOrigin::Layer {
                read_uint32(reader)? as f64
            } else {
                0.0
            };
            let name = read_unicode_string(reader)?;
            let slice_type = slice_type_from_index(read_uint32(reader)?);
            let s_left = read_int32(reader)? as f64;
            let s_top = read_int32(reader)? as f64;
            let s_right = read_int32(reader)? as f64;
            let s_bottom = read_int32(reader)? as f64;
            let url = read_unicode_string(reader)?;
            let s_target = read_unicode_string(reader)?;
            let message = read_unicode_string(reader)?;
            let alt_tag = read_unicode_string(reader)?;
            let cell_text_is_html = read_uint8(reader)? != 0;
            let cell_text = read_unicode_string(reader)?;
            let _horz = read_uint32(reader)?; // clamped to 'default'
            let _vert = read_uint32(reader)?;
            let a = read_uint8(reader)? as f64;
            let r = read_uint8(reader)? as f64;
            let g = read_uint8(reader)? as f64;
            let b = read_uint8(reader)? as f64;
            let background_color_type = if (a + r + g + b) == 0.0 {
                SliceBackgroundColorType::None
            } else if a == 0.0 {
                SliceBackgroundColorType::Matte
            } else {
                SliceBackgroundColorType::Color
            };
            slices.push(Slice {
                id,
                group_id,
                origin: Some(origin),
                associated_layer_id,
                name: Some(name),
                slice_type: Some(slice_type),
                bounds: LtrbBounds {
                    top: s_top,
                    left: s_left,
                    bottom: s_bottom,
                    right: s_right,
                },
                url,
                target: s_target,
                message,
                alt_tag,
                cell_text_is_html,
                cell_text,
                horizontal_alignment: Some(SliceAlignment::Default),
                vertical_alignment: Some(SliceAlignment::Default),
                background_color_type: Some(background_color_type),
                background_color: Rgba { r, g, b, a },
                top_outset: None,
                left_outset: None,
                bottom_outset: None,
                right_outset: None,
            });
        }

        let desc = read_version_and_descriptor(reader)?;
        if let Some(slice_list) = get_list(&desc, "slices") {
            for d in slice_list {
                if let DescriptorValue::Descriptor(d) = d {
                    let slice_id = get_double(d, "sliceID").unwrap_or(0.0);
                    if let Some(slice) = slices.iter_mut().find(|s| s.id == slice_id) {
                        slice.top_outset = get_double(d, "topOutset");
                        slice.left_outset = get_double(d, "leftOutset");
                        slice.bottom_outset = get_double(d, "bottomOutset");
                        slice.right_outset = get_double(d, "rightOutset");
                    }
                }
            }
        }

        target.slices.as_mut().unwrap().push(SliceGroup {
            bounds: LtrbBounds {
                top,
                left,
                bottom,
                right,
            },
            group_name,
            slices,
        });
    } else if version == 7 || version == 8 {
        let desc = read_version_and_descriptor(reader)?;
        if target.slices.is_none() {
            target.slices = Some(Vec::new());
        }
        let bounds = get_descriptor(&desc, "bounds")
            .map(ltrb_from_bounds_desc)
            .unwrap_or_default();
        let mut slices = Vec::new();
        if let Some(list) = get_list(&desc, "slices") {
            for item in list {
                if let DescriptorValue::Descriptor(s) = item {
                    let origin = eslice_origin_codec()
                        .decode(&get_enum(s, "origin").unwrap_or_default())
                        .ok();
                    let slice_type = eslice_type_codec()
                        .decode(&get_enum(s, "Type").unwrap_or_default())
                        .ok();
                    let bg = get_descriptor(s, "bgColor");
                    let background_color = bg
                        .map(|c| Rgba {
                            r: get_double(c, "Rd  ").unwrap_or(0.0),
                            g: get_double(c, "Grn ").unwrap_or(0.0),
                            b: get_double(c, "Bl  ").unwrap_or(0.0),
                            a: get_double(c, "alpha").unwrap_or(0.0),
                        })
                        .unwrap_or(Rgba {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 0.0,
                        });
                    slices.push(Slice {
                        name: get_text(s, "Nm  "),
                        id: get_double(s, "sliceID").unwrap_or(0.0),
                        group_id: get_double(s, "groupID").unwrap_or(0.0),
                        associated_layer_id: 0.0,
                        origin: origin.map(|o| match o.as_str() {
                            "autoGenerated" => SliceOrigin::AutoGenerated,
                            "layer" => SliceOrigin::Layer,
                            _ => SliceOrigin::UserGenerated,
                        }),
                        slice_type: slice_type.map(|t| {
                            if t == "noImage" {
                                SliceType::NoImage
                            } else {
                                SliceType::Image
                            }
                        }),
                        bounds: get_descriptor(s, "bounds")
                            .map(ltrb_from_bounds_desc)
                            .unwrap_or_default(),
                        url: get_text(s, "url").unwrap_or_default(),
                        target: get_text(s, "null").unwrap_or_default(),
                        message: get_text(s, "Msge").unwrap_or_default(),
                        alt_tag: get_text(s, "altTag").unwrap_or_default(),
                        cell_text_is_html: get_bool(s, "cellTextIsHTML").unwrap_or(false),
                        cell_text: get_text(s, "cellText").unwrap_or_default(),
                        horizontal_alignment: Some(SliceAlignment::Default),
                        vertical_alignment: Some(SliceAlignment::Default),
                        background_color_type: eslice_bg_codec()
                            .decode(&get_enum(s, "bgColorType").unwrap_or_default())
                            .ok()
                            .map(|t| match t.as_str() {
                                "matte" => SliceBackgroundColorType::Matte,
                                "color" => SliceBackgroundColorType::Color,
                                _ => SliceBackgroundColorType::None,
                            }),
                        background_color,
                        top_outset: Some(get_double(s, "topOutset").unwrap_or(0.0)),
                        left_outset: Some(get_double(s, "leftOutset").unwrap_or(0.0)),
                        bottom_outset: Some(get_double(s, "bottomOutset").unwrap_or(0.0)),
                        right_outset: Some(get_double(s, "rightOutset").unwrap_or(0.0)),
                    });
                }
            }
        }
        target.slices.as_mut().unwrap().push(SliceGroup {
            group_name: get_text(&desc, "baseName").unwrap_or_default(),
            bounds,
            slices,
        });
    } else {
        return Err(ReadError::StrictViolation(format!(
            "Invalid slices version ({version})"
        )));
    }
    Ok(())
}

fn write_slices(writer: &mut PsdWriter, target: &ImageResources, index: usize) {
    let group = &target.slices.as_ref().unwrap()[index];
    let bounds = &group.bounds;

    write_uint32(writer, 6); // version
    write_int32(writer, bounds.top as i32);
    write_int32(writer, bounds.left as i32);
    write_int32(writer, bounds.bottom as i32);
    write_int32(writer, bounds.right as i32);
    write_unicode_string(writer, &group.group_name);
    write_uint32(writer, group.slices.len() as u32);

    for slice in &group.slices {
        let (mut a, mut r, mut g, mut b) = (
            slice.background_color.a,
            slice.background_color.r,
            slice.background_color.g,
            slice.background_color.b,
        );
        match slice.background_color_type {
            Some(SliceBackgroundColorType::None) => {
                a = 0.0;
                r = 0.0;
                g = 0.0;
                b = 0.0;
            }
            Some(SliceBackgroundColorType::Matte) => {
                a = 0.0;
                r = 255.0;
                g = 255.0;
                b = 255.0;
            }
            _ => {}
        }

        write_uint32(writer, slice.id as u32);
        write_uint32(writer, slice.group_id as u32);
        let origin = slice.origin.unwrap_or(SliceOrigin::UserGenerated);
        write_uint32(writer, slice_origin_index(origin));
        if origin == SliceOrigin::Layer {
            write_uint32(writer, slice.associated_layer_id as u32);
        }
        write_unicode_string(writer, slice.name.as_deref().unwrap_or(""));
        write_uint32(
            writer,
            slice_type_index(slice.slice_type.unwrap_or(SliceType::Image)),
        );
        write_int32(writer, slice.bounds.left as i32);
        write_int32(writer, slice.bounds.top as i32);
        write_int32(writer, slice.bounds.right as i32);
        write_int32(writer, slice.bounds.bottom as i32);
        write_unicode_string(writer, &slice.url);
        write_unicode_string(writer, &slice.target);
        write_unicode_string(writer, &slice.message);
        write_unicode_string(writer, &slice.alt_tag);
        write_uint8(writer, u8::from(slice.cell_text_is_html));
        write_unicode_string(writer, &slice.cell_text);
        write_uint32(writer, 0); // horizontalAlignment -> 'default'
        write_uint32(writer, 0); // verticalAlignment -> 'default'
        write_uint8(writer, a as u8);
        write_uint8(writer, r as u8);
        write_uint8(writer, g as u8);
        write_uint8(writer, b as u8);
    }

    let mut desc = Descriptor::new("", "null");
    desc.set(
        "bounds",
        DescriptorValue::Descriptor(bounds_desc_from_ltrb(bounds)),
    );
    let mut slice_list = Vec::new();
    for s in &group.slices {
        let mut sd = Descriptor::new("", "slcD");
        sd.set("sliceID", DescriptorValue::Integer(s.id as i32));
        sd.set("groupID", DescriptorValue::Integer(s.group_id as i32));
        sd.set(
            "origin",
            DescriptorValue::Enum(
                eslice_origin_codec()
                    .encode(Some(match s.origin.unwrap_or(SliceOrigin::UserGenerated) {
                        SliceOrigin::AutoGenerated => "autoGenerated",
                        SliceOrigin::Layer => "layer",
                        SliceOrigin::UserGenerated => "userGenerated",
                    }))
                    .unwrap(),
            ),
        );
        sd.set(
            "Type",
            DescriptorValue::Enum(
                eslice_type_codec()
                    .encode(Some(match s.slice_type.unwrap_or(SliceType::Image) {
                        SliceType::NoImage => "noImage",
                        SliceType::Image => "image",
                    }))
                    .unwrap(),
            ),
        );
        sd.set(
            "bounds",
            DescriptorValue::Descriptor(bounds_desc_from_ltrb(&s.bounds)),
        );
        if let Some(name) = &s.name {
            sd.set("Nm  ", DescriptorValue::Text(name.clone()));
        }
        sd.set("url", DescriptorValue::Text(s.url.clone()));
        sd.set("null", DescriptorValue::Text(s.target.clone()));
        sd.set("Msge", DescriptorValue::Text(s.message.clone()));
        sd.set("altTag", DescriptorValue::Text(s.alt_tag.clone()));
        sd.set("cellTextIsHTML", DescriptorValue::Boolean(s.cell_text_is_html));
        sd.set("cellText", DescriptorValue::Text(s.cell_text.clone()));
        sd.set(
            "horzAlign",
            DescriptorValue::Enum(eslice_horz_codec().encode(Some("default")).unwrap()),
        );
        sd.set(
            "vertAlign",
            DescriptorValue::Enum(eslice_vert_codec().encode(Some("default")).unwrap()),
        );
        sd.set(
            "bgColorType",
            DescriptorValue::Enum(
                eslice_bg_codec()
                    .encode(Some(
                        match s.background_color_type.unwrap_or(SliceBackgroundColorType::None) {
                            SliceBackgroundColorType::None => "none",
                            SliceBackgroundColorType::Matte => "matte",
                            SliceBackgroundColorType::Color => "color",
                        },
                    ))
                    .unwrap(),
            ),
        );
        if s.background_color_type == Some(SliceBackgroundColorType::Color) {
            let mut bg = Descriptor::new("", "RGBC");
            bg.set("Rd  ", DescriptorValue::Integer(s.background_color.r as i32));
            bg.set("Grn ", DescriptorValue::Integer(s.background_color.g as i32));
            bg.set("Bl  ", DescriptorValue::Integer(s.background_color.b as i32));
            bg.set("alpha", DescriptorValue::Integer(s.background_color.a as i32));
            sd.set("bgColor", DescriptorValue::Descriptor(bg));
        }
        sd.set("topOutset", DescriptorValue::Integer(s.top_outset.unwrap_or(0.0) as i32));
        sd.set("leftOutset", DescriptorValue::Integer(s.left_outset.unwrap_or(0.0) as i32));
        sd.set(
            "bottomOutset",
            DescriptorValue::Integer(s.bottom_outset.unwrap_or(0.0) as i32),
        );
        sd.set(
            "rightOutset",
            DescriptorValue::Integer(s.right_outset.unwrap_or(0.0) as i32),
        );
        slice_list.push(DescriptorValue::Descriptor(sd));
    }
    desc.set("slices", DescriptorValue::List(slice_list));
    write_version_and_descriptor(writer, &desc);
}

// ===========================================================================
// Thumbnail (ids 1033 / 1036)
//
// JPEG encode/decode is NOT ported (jpeg.rs is a stub), so the compressed JPEG
// payload is kept/emitted as raw bytes via `thumbnail_raw`.
// TODO: jpeg.rs encode/decode when ported.
// ===========================================================================

fn read_thumbnail(
    reader: &mut PsdReader,
    target: &mut ImageResources,
    left: usize,
) -> ReadResult<()> {
    let start = reader.offset;
    let format = read_uint32(reader)?; // 1 = kJpegRGB, 0 = kRawRGB
    let width = read_uint32(reader)? as f64;
    let height = read_uint32(reader)? as f64;
    let _width_bytes = read_uint32(reader)?;
    let _total_size = read_uint32(reader)?;
    let _size_after_compression = read_uint32(reader)?;
    let bits_per_pixel = read_uint16(reader)?; // 24
    let planes = read_uint16(reader)?; // 1

    let consumed = reader.offset - start;
    let remaining = left.saturating_sub(consumed);

    if format != 1 || bits_per_pixel != 24 || planes != 1 {
        skip_bytes(reader, remaining);
        return Ok(());
    }

    let data = read_bytes(reader, remaining)?;
    // TODO: jpeg.rs decode when ported — keep raw compressed bytes for now.
    target.thumbnail_raw = Some(ThumbnailRaw {
        width,
        height,
        data,
    });
    Ok(())
}

fn write_thumbnail(writer: &mut PsdWriter, target: &ImageResources) {
    let mut width = 0.0;
    let mut height = 0.0;
    let mut data: Vec<u8> = Vec::new();

    if let Some(raw) = &target.thumbnail_raw {
        width = raw.width;
        height = raw.height;
        data = raw.data.clone();
    }
    // TODO: jpeg.rs encode when ported — would encode `target.thumbnail` canvas.

    let bits_per_pixel = 24.0_f64;
    let width_bytes = ((width * bits_per_pixel + 31.0) / 32.0).floor() * 4.0;
    let planes = 1.0_f64;
    let total_size = width_bytes * height * planes;
    let size_after_compression = data.len() as f64;

    write_uint32(writer, 1); // 1 = kJpegRGB
    write_uint32(writer, width as u32);
    write_uint32(writer, height as u32);
    write_uint32(writer, width_bytes as u32);
    write_uint32(writer, total_size as u32);
    write_uint32(writer, size_after_compression as u32);
    write_uint16(writer, bits_per_pixel as u16);
    write_uint16(writer, planes as u16);
    write_bytes(writer, Some(&data));
}

// ===========================================================================
// Animations (id 4000)
// ===========================================================================

fn read_animations(
    reader: &mut PsdReader,
    target: &mut ImageResources,
    left: usize,
) -> ReadResult<()> {
    let key = read_signature(reader)?;

    if key == "mani" {
        check_signature(reader, "IRFR", None)?;
        read_section(
            reader,
            1,
            |reader, sect_left| {
                while sect_left(reader) > 0 {
                    check_signature(reader, "8BIM", None)?;
                    let sub_key = read_signature(reader)?;
                    read_section(
                        reader,
                        1,
                        |reader, inner_left| {
                            if sub_key == "AnDs" {
                                let desc = read_version_and_descriptor(reader)?;
                                target.animations = Some(parse_animations(&desc));
                            } else {
                                // 'Roll' or unhandled — skip bytes.
                                let n = inner_left(reader);
                                skip_bytes(reader, n);
                            }
                            Ok(())
                        },
                        true,
                        false,
                    )?;
                }
                Ok(())
            },
            true,
            false,
        )?;
    } else {
        // 'mopt' or unhandled — skip the remaining bytes of the block.
        let consumed = 4; // signature
        skip_bytes(reader, left.saturating_sub(consumed));
    }
    Ok(())
}

fn parse_animations(desc: &Descriptor) -> Animations {
    let mut frames = Vec::new();
    if let Some(list) = get_list(desc, "FrIn") {
        for item in list {
            if let DescriptorValue::Descriptor(x) = item {
                let dispose = get_enum(x, "FrDs")
                    .and_then(|s| frmd_codec().decode(&s).ok())
                    .map(|d| match d.as_str() {
                        "none" => AnimationDispose::None,
                        "dispose" => AnimationDispose::Dispose,
                        _ => AnimationDispose::Auto,
                    })
                    .unwrap_or(AnimationDispose::Auto);
                frames.push(AnimationFrameInfo {
                    id: get_double(x, "FrID").unwrap_or(0.0),
                    delay: get_double(x, "FrDl").unwrap_or(0.0) / 100.0,
                    dispose: Some(dispose),
                });
            }
        }
    }
    let mut animations = Vec::new();
    if let Some(list) = get_list(desc, "FSts") {
        for item in list {
            if let DescriptorValue::Descriptor(x) = item {
                let mut fr = Vec::new();
                if let Some(fslist) = get_list(x, "FsFr") {
                    for f in fslist {
                        if let DescriptorValue::Integer(i) = f {
                            fr.push(*i as f64);
                        }
                    }
                }
                animations.push(AnimationInfo {
                    id: get_double(x, "FsID").unwrap_or(0.0),
                    frames: fr,
                    repeats: Some(get_double(x, "LCnt").unwrap_or(0.0)),
                    active_frame: Some(get_double(x, "AFrm").unwrap_or(0.0)),
                });
            }
        }
    }
    Animations { frames, animations }
}

fn write_animations(writer: &mut PsdWriter, target: &ImageResources) {
    let Some(animations) = &target.animations else {
        return;
    };
    write_signature(writer, "mani");
    write_signature(writer, "IRFR");
    write_section(
        writer,
        1,
        |writer| {
            write_signature(writer, "8BIM");
            write_signature(writer, "AnDs");
            write_section(
                writer,
                1,
                |writer| {
                    let mut desc = Descriptor::new("", "null");
                    let mut fr_in = Vec::new();
                    for f in &animations.frames {
                        let mut frame = Descriptor::new("", "AnFr");
                        frame.set("FrID", DescriptorValue::Integer(f.id as i32));
                        if f.delay != 0.0 {
                            frame.set(
                                "FrDl",
                                DescriptorValue::Integer((f.delay * 100.0) as i32),
                            );
                        }
                        frame.set(
                            "FrDs",
                            DescriptorValue::Enum(
                                frmd_codec()
                                    .encode(Some(match f.dispose.unwrap_or(AnimationDispose::Auto) {
                                        AnimationDispose::Auto => "auto",
                                        AnimationDispose::None => "none",
                                        AnimationDispose::Dispose => "dispose",
                                    }))
                                    .unwrap(),
                            ),
                        );
                        fr_in.push(DescriptorValue::Descriptor(frame));
                    }
                    desc.set("FrIn", DescriptorValue::List(fr_in));

                    let mut f_sts = Vec::new();
                    for a in &animations.animations {
                        let mut anim = Descriptor::new("", "AnSt");
                        anim.set("FsID", DescriptorValue::Integer(a.id as i32));
                        anim.set(
                            "AFrm",
                            DescriptorValue::Integer(a.active_frame.unwrap_or(0.0) as i32),
                        );
                        let frames = a
                            .frames
                            .iter()
                            .map(|f| DescriptorValue::Integer(*f as i32))
                            .collect();
                        anim.set("FsFr", DescriptorValue::List(frames));
                        anim.set(
                            "LCnt",
                            DescriptorValue::Integer(a.repeats.unwrap_or(0.0) as i32),
                        );
                        f_sts.push(DescriptorValue::Descriptor(anim));
                    }
                    desc.set("FSts", DescriptorValue::List(f_sts));

                    write_version_and_descriptor(writer, &desc);
                },
                false,
                false,
            );
        },
        false,
        false,
    );
}

// Reference MOCK_HANDLERS so the import is considered used even though the
// gated handlers are not ported (helpers::MOCK_HANDLERS == false).
#[allow(dead_code)]
const _MOCK: bool = MOCK_HANDLERS;

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::PsdReader;
    use crate::writer::{create_writer, get_writer_buffer};

    /// Mirror the resource-block framing: signature '8BIM', id (u16), pascal name,
    /// size (u32), then padded-to-even payload. Used to test framing round-trip.
    fn write_block(id: u16, name: &str, target: &ImageResources, index: usize) -> Vec<u8> {
        let mut w = create_writer(256);
        write_signature(&mut w, "8BIM");
        write_uint16(&mut w, id);
        // pascal name padded to 2 (mirror writePascalString(name, 2)).
        crate::writer::write_pascal_string(&mut w, name, 2);
        write_section(
            &mut w,
            2,
            |w| {
                write_image_resource(id, w, target, index).unwrap();
            },
            false,
            false,
        );
        get_writer_buffer(&w)
    }

    /// Read back a framed block, returning (id, remaining payload consumed via handler).
    fn read_block(bytes: &[u8]) -> ImageResources {
        let mut r = PsdReader::new(bytes, None, None);
        check_signature(&mut r, "8BIM", None).unwrap();
        let id = read_uint16(&mut r).unwrap();
        let _name = crate::reader::read_pascal_string(&mut r, 2).unwrap();
        let mut target = ImageResources::default();
        // read the section length, then dispatch with `left`.
        let len = read_uint32(&mut r).unwrap() as usize;
        read_image_resource(id, &mut r, &mut target, len).unwrap();
        target
    }

    #[test]
    fn resolution_info_round_trip() {
        let target = ImageResources {
            resolution_info: Some(ResolutionInfo {
                horizontal_resolution: 300.0,
                horizontal_resolution_unit: ResolutionUnit::Ppi,
                width_unit: DimensionUnit::Inches,
                vertical_resolution: 300.0,
                vertical_resolution_unit: ResolutionUnit::Ppcm,
                height_unit: DimensionUnit::Centimeters,
            }),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1005, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1005, &mut r, &mut out, bytes.len()).unwrap();

        let a = target.resolution_info.unwrap();
        let b = out.resolution_info.unwrap();
        assert_eq!(a.horizontal_resolution, b.horizontal_resolution);
        assert_eq!(a.horizontal_resolution_unit, b.horizontal_resolution_unit);
        assert_eq!(a.width_unit, b.width_unit);
        assert_eq!(a.vertical_resolution, b.vertical_resolution);
        assert_eq!(a.vertical_resolution_unit, b.vertical_resolution_unit);
        assert_eq!(a.height_unit, b.height_unit);
    }

    #[test]
    fn xmp_metadata_string_round_trip() {
        let target = ImageResources {
            xmp_metadata: Some("<x:xmpmeta>data</x:xmpmeta>".to_string()),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1060, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1060, &mut r, &mut out, bytes.len()).unwrap();
        assert_eq!(out.xmp_metadata, target.xmp_metadata);
    }

    #[test]
    fn caption_digest_round_trip() {
        let target = ImageResources {
            caption_digest: Some("0123456789abcdef0123456789abcdef".to_string()),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1061, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);
        assert_eq!(bytes.len(), 16);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1061, &mut r, &mut out, bytes.len()).unwrap();
        assert_eq!(out.caption_digest, target.caption_digest);
    }

    #[test]
    fn framing_odd_length_pad() {
        // url resource: ASCII string of odd length forces pad-to-even on the block.
        let target = ImageResources {
            url: Some("abc".to_string()), // 3 bytes -> needs 1 pad byte
            ..Default::default()
        };

        let bytes = write_block(1035, "", &target, 0);
        // Find the section length and assert payload is padded to even.
        // Layout: 4 (8BIM) + 2 (id) + pascal name (1 len byte + 0 chars + 1 pad = 2)
        //         + 4 (size) + payload(3) + pad(1)
        let header = 4 + 2 + 2 + 4;
        let payload_len = 3usize;
        assert_eq!(bytes.len(), header + payload_len + 1); // +1 pad to even

        let out = read_block(&bytes);
        assert_eq!(out.url, target.url);
    }

    #[test]
    fn print_scale_round_trip() {
        let target = ImageResources {
            print_scale: Some(PrintScale {
                style: Some(PrintScaleStyle::UserDefined),
                x: Some(1.5),
                y: Some(2.5),
                scale: Some(0.75),
            }),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1062, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1062, &mut r, &mut out, bytes.len()).unwrap();
        let p = out.print_scale.unwrap();
        assert_eq!(p.style, Some(PrintScaleStyle::UserDefined));
        assert_eq!(p.x, Some(1.5));
        assert_eq!(p.y, Some(2.5));
        assert!((p.scale.unwrap() - 0.75).abs() < 1e-6);
    }

    #[test]
    fn version_info_round_trip() {
        let target = ImageResources {
            version_info: Some(VersionInfo {
                has_real_merged_data: true,
                writer_name: "ag-psd".to_string(),
                reader_name: "ag-psd".to_string(),
                file_version: 1.0,
            }),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1057, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1057, &mut r, &mut out, bytes.len()).unwrap();
        let v = out.version_info.unwrap();
        assert!(v.has_real_merged_data);
        assert_eq!(v.writer_name, "ag-psd");
        assert_eq!(v.reader_name, "ag-psd");
        assert_eq!(v.file_version, 1.0);
    }

    #[test]
    fn grid_and_guides_round_trip() {
        let target = ImageResources {
            grid_and_guides_information: Some(GridAndGuidesInformation {
                grid: Some(GridInfo {
                    horizontal: 576.0,
                    vertical: 576.0,
                }),
                guides: Some(vec![
                    GuideInfo {
                        location: 10.0,
                        direction: GuideDirection::Horizontal,
                    },
                    GuideInfo {
                        location: 20.0,
                        direction: GuideDirection::Vertical,
                    },
                ]),
            }),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1032, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1032, &mut r, &mut out, bytes.len()).unwrap();
        let info = out.grid_and_guides_information.unwrap();
        let grid = info.grid.unwrap();
        assert_eq!(grid.horizontal, 576.0);
        assert_eq!(grid.vertical, 576.0);
        let guides = info.guides.unwrap();
        assert_eq!(guides.len(), 2);
        assert_eq!(guides[0].location, 10.0);
        assert_eq!(guides[0].direction, GuideDirection::Horizontal);
        assert_eq!(guides[1].direction, GuideDirection::Vertical);
    }

    #[test]
    fn alpha_identifiers_round_trip() {
        let target = ImageResources {
            alpha_identifiers: Some(vec![1.0, 2.0, 3.0]),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1053, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1053, &mut r, &mut out, bytes.len()).unwrap();
        assert_eq!(out.alpha_identifiers, Some(vec![1.0, 2.0, 3.0]));
    }

    #[test]
    fn url_list_round_trip() {
        let target = ImageResources {
            urls_list: Some(vec![UrlListItem {
                id: 7.0,
                r#ref: "slice".to_string(),
                url: "http://example.com".to_string(),
            }]),
            ..Default::default()
        };

        let mut w = create_writer(128);
        write_image_resource(1054, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1054, &mut r, &mut out, bytes.len()).unwrap();
        let list = out.urls_list.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, 7.0);
        assert_eq!(list[0].url, "http://example.com");
        assert_eq!(list[0].r#ref, "slice");
    }

    #[test]
    fn pixel_aspect_ratio_round_trip() {
        let target = ImageResources {
            pixel_aspect_ratio: Some(PixelAspectRatio { aspect: 1.25 }),
            ..Default::default()
        };

        let mut w = create_writer(64);
        write_image_resource(1064, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1064, &mut r, &mut out, bytes.len()).unwrap();
        assert_eq!(out.pixel_aspect_ratio.unwrap().aspect, 1.25);
    }

    #[test]
    fn animations_round_trip() {
        let target = ImageResources {
            animations: Some(Animations {
                frames: vec![
                    AnimationFrameInfo {
                        id: 1.0,
                        delay: 0.1,
                        dispose: Some(AnimationDispose::Auto),
                    },
                    AnimationFrameInfo {
                        id: 2.0,
                        delay: 0.0,
                        dispose: Some(AnimationDispose::None),
                    },
                ],
                animations: vec![AnimationInfo {
                    id: 10.0,
                    frames: vec![1.0, 2.0],
                    repeats: Some(3.0),
                    active_frame: Some(1.0),
                }],
            }),
            ..Default::default()
        };

        let mut w = create_writer(512);
        write_image_resource(4000, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(4000, &mut r, &mut out, bytes.len()).unwrap();
        let a = out.animations.unwrap();
        assert_eq!(a.frames.len(), 2);
        assert_eq!(a.frames[0].id, 1.0);
        assert!((a.frames[0].delay - 0.1).abs() < 1e-6);
        assert_eq!(a.frames[1].dispose, Some(AnimationDispose::None));
        assert_eq!(a.animations.len(), 1);
        assert_eq!(a.animations[0].id, 10.0);
        assert_eq!(a.animations[0].frames, vec![1.0, 2.0]);
        assert_eq!(a.animations[0].repeats, Some(3.0));
    }

    #[test]
    fn slices_round_trip() {
        let target = ImageResources {
            slices: Some(vec![SliceGroup {
                bounds: LtrbBounds {
                    left: 0.0,
                    top: 0.0,
                    right: 100.0,
                    bottom: 80.0,
                },
                group_name: "group".to_string(),
                slices: vec![Slice {
                    id: 1.0,
                    group_id: 0.0,
                    origin: Some(SliceOrigin::UserGenerated),
                    associated_layer_id: 0.0,
                    name: Some("slice 1".to_string()),
                    slice_type: Some(SliceType::Image),
                    bounds: LtrbBounds {
                        left: 1.0,
                        top: 2.0,
                        right: 3.0,
                        bottom: 4.0,
                    },
                    url: "u".to_string(),
                    target: "t".to_string(),
                    message: "m".to_string(),
                    alt_tag: "a".to_string(),
                    cell_text_is_html: true,
                    cell_text: "c".to_string(),
                    horizontal_alignment: Some(SliceAlignment::Default),
                    vertical_alignment: Some(SliceAlignment::Default),
                    background_color_type: Some(SliceBackgroundColorType::None),
                    background_color: Rgba {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 0.0,
                    },
                    top_outset: None,
                    left_outset: None,
                    bottom_outset: None,
                    right_outset: None,
                }],
            }]),
            ..Default::default()
        };

        let mut w = create_writer(1024);
        write_image_resource(1050, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1050, &mut r, &mut out, bytes.len()).unwrap();
        let groups = out.slices.unwrap();
        assert_eq!(groups.len(), 1);
        let g = &groups[0];
        assert_eq!(g.group_name, "group");
        assert_eq!(g.bounds.right, 100.0);
        assert_eq!(g.slices.len(), 1);
        let s = &g.slices[0];
        assert_eq!(s.id, 1.0);
        assert_eq!(s.name.as_deref(), Some("slice 1"));
        assert_eq!(s.slice_type, Some(SliceType::Image));
        assert_eq!(s.url, "u");
        assert!(s.cell_text_is_html);
        assert_eq!(s.bounds.left, 1.0);
        assert_eq!(s.bounds.bottom, 4.0);
    }

    #[test]
    fn print_information_round_trip() {
        let target = ImageResources {
            print_information: Some(PrintInformation {
                printer_manages_colors: None,
                printer_name: Some("My Printer".to_string()),
                printer_profile: Some("sRGB".to_string()),
                print_sixteen_bit: Some(false),
                rendering_intent: Some(RenderingIntent::RelativeColorimetric),
                hard_proof: Some(true),
                black_point_compensation: Some(true),
                proof_setup: Some(ProofSetup::Builtin {
                    builtin: "proofCMYK".to_string(),
                }),
            }),
            ..Default::default()
        };

        let mut w = create_writer(512);
        write_image_resource(1082, &mut w, &target, 0).unwrap();
        let bytes = get_writer_buffer(&w);

        let mut r = PsdReader::new(&bytes, None, None);
        let mut out = ImageResources::default();
        read_image_resource(1082, &mut r, &mut out, bytes.len()).unwrap();
        let info = out.print_information.unwrap();
        assert_eq!(info.printer_name.as_deref(), Some("My Printer"));
        assert_eq!(info.printer_profile.as_deref(), Some("sRGB"));
        assert_eq!(info.rendering_intent, Some(RenderingIntent::RelativeColorimetric));
        assert_eq!(info.black_point_compensation, Some(true));
        match info.proof_setup {
            Some(ProofSetup::Builtin { builtin }) => assert_eq!(builtin, "proofCMYK"),
            _ => panic!("expected builtin proof setup"),
        }
    }

    // -- charToNibble / byteAt --------------------------------------------------

    #[test]
    fn char_to_nibble_decodes_all_three_hex_ranges() {
        for (i, c) in b"0123456789".iter().enumerate() {
            assert_eq!(char_to_nibble(*c), u8::try_from(i).unwrap());
        }
        for (i, c) in b"abcdef".iter().enumerate() {
            assert_eq!(char_to_nibble(*c), u8::try_from(i).unwrap() + 10);
        }
        // Uppercase used to fall into the lowercase branch and underflow/garble.
        for (i, c) in b"ABCDEF".iter().enumerate() {
            assert_eq!(char_to_nibble(*c), u8::try_from(i).unwrap() + 10);
        }
    }

    #[test]
    fn byte_at_accepts_uppercase_hex() {
        assert_eq!(byte_at("FF", 0), 0xff);
        assert_eq!(byte_at("ff", 0), 0xff);
        assert_eq!(byte_at("A0", 0), 0xa0);
        assert_eq!(byte_at("00ff", 2), 0xff);
    }

    // -- enum codec defaults ----------------------------------------------------

    #[test]
    fn every_enum_codec_default_is_a_map_key() {
        // A default that is one of the map VALUES makes `encode(None)` emit an empty
        // code. Upstream caught this class with types; here it is a test plus the
        // debug assertion in `EnumCodec::new`.
        for codec in [
            inte_codec(),
            frmd_codec(),
            eslice_type_codec(),
            eslice_horz_codec(),
            eslice_vert_codec(),
            eslice_origin_codec(),
            eslice_bg_codec(),
        ] {
            assert!(codec.default_is_valid());
        }
    }

    #[test]
    fn frmd_default_encodes_to_auto() {
        // Was `''`, which is not a map key, so `encode(None)` produced "FrmD.".
        assert_eq!(frmd_codec().encode(None).unwrap(), "FrmD.Auto");
        assert_eq!(frmd_codec().decode("FrmD").unwrap(), "auto");
    }

    #[test]
    fn frmd_decodes_photoshop_2026_long_form() {
        assert_eq!(frmd_codec().decode("FrmD.Disp").unwrap(), "dispose");
        assert_eq!(frmd_codec().decode("FrmD.dispose").unwrap(), "dispose");
    }

    // -- readEncodedString ------------------------------------------------------

    #[test]
    fn read_encoded_string_reads_ascii_and_utf8() {
        let mut bytes = vec![5u8];
        bytes.extend_from_slice(b"hello");
        let mut r = PsdReader::new(&bytes, None, None);
        assert_eq!(read_encoded_string(&mut r).unwrap(), "hello");

        // High-bit payload: no GBK decoder here, so upstream's fallback (UTF-8) applies.
        let text = "\u{0142}\u{0105}"; // 4 UTF-8 bytes
        let mut bytes = vec![u8::try_from(text.len()).unwrap()];
        bytes.extend_from_slice(text.as_bytes());
        let mut r = PsdReader::new(&bytes, None, None);
        assert_eq!(read_encoded_string(&mut r).unwrap(), text);
    }
}
