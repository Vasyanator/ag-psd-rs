/*
File: crates/ag-psd/src/additional_info/metadata_keys.rs

Purpose:
Group-модуль "simple metadata" additional-info ключей (`Group::Metadata`).
REFERENCE IMPLEMENTATION GROUP-MODULE CONTRACT'а — полностью рабочий,
по нему follow-up воркеры заполняют остальные группы.

Реализованные ключи (read + has + write, точная байтовая раскладка):
- `luni` — unicode-имя слоя;
- `lnsr` — источник имени (4-байтовая подпись);
- `lyid` — id слоя (uint32, с дедупликацией при записи);
- `lsct` / алиас `lsdk` — section divider (type + опц. blend key + subType);
- `clbl` — blend clipped elements (bool + 3 паддинга);
- `infx` — blend interior elements;
- `knko` — knockout;
- `lmgm` — layer mask as global mask;
- `lspf` — protected flags (uint32 битовая маска);
- `lclr` — sheet/layer color (uint16 индекс + 6 паддинга);
- `fxrp` — reference point (2x float64);
- `lyvr` — версия слоя (uint32);
- `iOpa` — fill opacity (uint8 / 0xff + 3 паддинга);
- `brst` — channel blending restrictions (массив int32, по `left()`);
- `tsly` — transparency shapes layer (bool + 3 паддинга).

Source compatibility: зеркало одноимённых `addHandler(...)` в
`test/ag-psd/src/additionalInfo.ts`.
*/

use crate::additional_info::{ReadCtx, WriteCtx};
use crate::helpers::LAYER_COLORS;
use crate::psd::{
    LayerAdditionalInfo, PointF, ProtectedInfo, SectionDivider, SectionDividerType,
};
use crate::reader::{
    check_signature, read_float64, read_int32, read_signature, read_uint16, read_uint32,
    read_uint8, read_unicode_string_with_length, skip_bytes, PsdReader, ReadResult,
};
use crate::writer::{
    write_float64, write_int32, write_signature, write_uint16, write_uint32, write_uint8,
    write_unicode_string, write_zeros, PsdWriter,
};

// ===========================================================================
// READ
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
        "luni" => read_luni(reader, info, left)?,
        "lnsr" => info.name_source = Some(read_signature(reader)?),
        "lyid" => info.id = Some(read_uint32(reader)? as f64),
        "lsct" | "lsdk" => read_lsct(reader, info, left)?,
        "clbl" => info.blend_clippend_elements = Some(read_bool_padded(reader)?),
        "infx" => info.blend_interior_elements = Some(read_bool_padded(reader)?),
        "knko" => info.knockout = Some(read_bool_padded(reader)?),
        "lmgm" => info.layer_mask_as_global_mask = Some(read_bool_padded(reader)?),
        "lspf" => read_lspf(reader, info)?,
        "lclr" => read_lclr(reader, info)?,
        "fxrp" => {
            info.reference_point = Some(PointF {
                x: read_float64(reader)?,
                y: read_float64(reader)?,
            });
        }
        "lyvr" => info.version = Some(read_uint32(reader)? as f64),
        "iOpa" => {
            info.fill_opacity = Some(read_uint8(reader)? as f64 / 0xff as f64);
            skip_bytes(reader, 3);
        }
        "brst" => read_brst(reader, info, left)?,
        "tsly" => info.transparency_shapes_layer = Some(read_bool_padded(reader)?),
        _ => return Ok(None),
    }
    Ok(Some(()))
}

/// `readUint8()` → bool, затем `skipBytes(reader, 3)` (общий паттерн
/// clbl/infx/knko/lmgm/tsly).
fn read_bool_padded(reader: &mut PsdReader) -> ReadResult<bool> {
    let value = read_uint8(reader)? != 0;
    skip_bytes(reader, 3);
    Ok(value)
}

fn read_luni(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if left(reader) > 4 {
        let length = read_uint32(reader)? as usize;
        // `if (left() >= length * 2)` — иначе строка слишком длинная, пропускаем.
        if left(reader) >= length * 2 {
            info.name = Some(read_unicode_string_with_length(reader, length)?);
        }
        // (логирование "name in luni section is too long" опускаем)
    }
    // (логирование "empty luni section" опускаем)
    skip_bytes(reader, left(reader));
    Ok(())
}

fn read_lsct(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    let raw_type = read_uint32(reader)?;
    let divider_type = match raw_type {
        0 => SectionDividerType::Other,
        1 => SectionDividerType::OpenFolder,
        2 => SectionDividerType::ClosedFolder,
        3 => SectionDividerType::BoundingSectionDivider,
        // upstream хранит сырой uint32 (const enum), неизвестные значения
        // не встречаются в валидных PSD; маппим как Other (поведение, не данные).
        _ => SectionDividerType::Other,
    };

    let mut divider = SectionDivider {
        divider_type,
        key: None,
        sub_type: None,
    };

    if left(reader) != 0 {
        check_signature(reader, "8BIM", None)?;
        divider.key = Some(read_signature(reader)?);
    }

    if left(reader) != 0 {
        divider.sub_type = Some(read_uint32(reader)? as f64);
    }

    info.section_divider = Some(divider);
    Ok(())
}

fn read_lspf(reader: &mut PsdReader, info: &mut LayerAdditionalInfo) -> ReadResult<()> {
    let flags = read_uint32(reader)?;
    info.protected_info = Some(ProtectedInfo {
        transparency: Some((flags & 0x01) != 0),
        composite: Some((flags & 0x02) != 0),
        position: Some((flags & 0x04) != 0),
        // upstream выставляет artboards только если бит установлен.
        artboards: if (flags & 0x08) != 0 { Some(true) } else { None },
    });
    Ok(())
}

fn read_lclr(reader: &mut PsdReader, info: &mut LayerAdditionalInfo) -> ReadResult<()> {
    let color = read_uint16(reader)? as usize;
    skip_bytes(reader, 6);
    info.layer_color = LAYER_COLORS.get(color).copied();
    Ok(())
}

fn read_brst(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    let mut channels = Vec::new();
    while left(reader) > 4 {
        channels.push(read_int32(reader)? as f64);
    }
    info.channel_blending_restrictions = Some(channels);
    Ok(())
}

// ===========================================================================
// HAS
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn has(key: &str, info: &LayerAdditionalInfo) -> Option<bool> {
    let present = match key {
        "luni" => info.name.is_some(),
        "lnsr" => info.name_source.is_some(),
        "lyid" => info.id.is_some(),
        "lsct" | "lsdk" => info.section_divider.is_some(),
        "clbl" => info.blend_clippend_elements.is_some(),
        "infx" => info.blend_interior_elements.is_some(),
        "knko" => info.knockout.is_some(),
        "lmgm" => info.layer_mask_as_global_mask.is_some(),
        "lspf" => info.protected_info.is_some(),
        "lclr" => info.layer_color.is_some(),
        "fxrp" => info.reference_point.is_some(),
        "lyvr" => info.version.is_some(),
        "iOpa" => info.fill_opacity.is_some(),
        "brst" => info.channel_blending_restrictions.is_some(),
        "tsly" => info.transparency_shapes_layer.is_some(),
        _ => return None,
    };
    Some(present)
}

// ===========================================================================
// WRITE
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs. Вызывается только при has==Some(true),
/// внутри уже открытой writeSection.
pub fn write(
    key: &str,
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    ctx: &mut WriteCtx,
) -> Option<ReadResult<()>> {
    match key {
        "luni" => write_unicode_string(writer, info.name.as_deref().unwrap_or("")),
        "lnsr" => write_signature(writer, info.name_source.as_deref().unwrap_or("")),
        "lyid" => write_lyid(writer, info, ctx),
        "lsct" | "lsdk" => write_lsct(writer, info),
        "clbl" => write_bool_padded(writer, info.blend_clippend_elements),
        "infx" => write_bool_padded(writer, info.blend_interior_elements),
        "knko" => write_bool_padded(writer, info.knockout),
        "lmgm" => write_bool_padded(writer, info.layer_mask_as_global_mask),
        "lspf" => write_lspf(writer, info),
        "lclr" => write_lclr(writer, info),
        "fxrp" => {
            let p = info.reference_point.as_ref().unwrap();
            write_float64(writer, p.x);
            write_float64(writer, p.y);
        }
        "lyvr" => write_uint32(writer, info.version.unwrap() as u32),
        "iOpa" => {
            write_uint8(writer, (info.fill_opacity.unwrap() * 0xff as f64) as u8);
            write_zeros(writer, 3);
        }
        "brst" => {
            for channel in info.channel_blending_restrictions.as_ref().unwrap() {
                write_int32(writer, *channel as i32);
            }
        }
        "tsly" => write_bool_padded(writer, info.transparency_shapes_layer),
        _ => return None,
    }
    Some(Ok(()))
}

fn write_bool_padded(writer: &mut PsdWriter, value: Option<bool>) {
    write_uint8(writer, if value == Some(true) { 1 } else { 0 });
    write_zeros(writer, 3);
}

fn write_lyid(writer: &mut PsdWriter, info: &LayerAdditionalInfo, ctx: &mut WriteCtx) {
    let mut id = info.id.unwrap() as u32;
    // upstream: пока id уже занят — сдвигаем на +100, чтобы избежать дублей.
    while ctx.layer_ids.contains(&id) {
        id += 100;
    }
    write_uint32(writer, id);
    ctx.layer_ids.insert(id);
    // (upstream также пишет options.layerToId.set(target, id); сопоставление
    // слой->id здесь не моделируется, т.к. требует ссылку на слой.)
}

fn write_lsct(writer: &mut PsdWriter, info: &LayerAdditionalInfo) {
    let divider = info.section_divider.as_ref().unwrap();
    write_uint32(writer, divider.divider_type as u32);

    if let Some(key) = &divider.key {
        write_signature(writer, "8BIM");
        write_signature(writer, key);

        if let Some(sub_type) = divider.sub_type {
            write_uint32(writer, sub_type as u32);
        }
    }
}

fn write_lspf(writer: &mut PsdWriter, info: &LayerAdditionalInfo) {
    let p = info.protected_info.as_ref().unwrap();
    let flags = (if p.transparency == Some(true) { 0x01 } else { 0 })
        | (if p.composite == Some(true) { 0x02 } else { 0 })
        | (if p.position == Some(true) { 0x04 } else { 0 })
        | (if p.artboards == Some(true) { 0x08 } else { 0 });
    write_uint32(writer, flags);
}

fn write_lclr(writer: &mut PsdWriter, info: &LayerAdditionalInfo) {
    let color = info.layer_color.unwrap();
    let index = LAYER_COLORS.iter().position(|c| *c == color);
    write_uint16(writer, index.unwrap_or(0) as u16);
    write_zeros(writer, 6);
}

// ===========================================================================
// TESTS
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::additional_info::{read_additional_info_key, write_additional_info, ReadCtx, WriteCtx};
    use crate::psd::{LayerColor, ReadOptions, WriteOptions};
    use crate::reader::{read_section, read_signature, PsdReader};
    use crate::writer::{create_writer, get_writer_buffer};

    /// Записывает один слой `info` целиком (все секции в каноническом порядке)
    /// и возвращает байты.
    fn write_layer(info: &LayerAdditionalInfo) -> Vec<u8> {
        let opts = WriteOptions::default();
        let mut ctx = WriteCtx::new(&opts, false);
        let mut w = create_writer(256);
        write_additional_info(&mut w, info, &mut ctx);
        get_writer_buffer(&w)
    }

    /// Читает поток additional-info секций обратно в `LayerAdditionalInfo`,
    /// зеркаля внешнюю оркестрацию (`readAdditionalLayerInfo`).
    fn read_layer(bytes: &[u8]) -> LayerAdditionalInfo {
        let opts = ReadOptions::default();
        let mut info = LayerAdditionalInfo::default();
        let mut reader = PsdReader::new(bytes, None, None);

        while reader.offset + 8 <= reader.buffer.len() {
            let sig = read_signature(&mut reader).unwrap();
            assert!(sig == "8BIM" || sig == "8B64", "bad signature {sig}");
            let key = read_signature(&mut reader).unwrap();
            let large =
                sig == "8B64" || crate::additional_info::is_large_key(&key);

            let mut ctx = ReadCtx { options: &opts, large };
            read_section::<(), _>(
                &mut reader,
                2,
                |r, left| {
                    let handled =
                        read_additional_info_key(&key, r, &mut info, left, &mut ctx)?;
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

    #[test]
    fn roundtrip_luni_name() {
        let mut info = LayerAdditionalInfo::default();
        info.name = Some("Привет Layer 1".to_string());
        let out = roundtrip(info);
        assert_eq!(out.name.as_deref(), Some("Привет Layer 1"));
    }

    #[test]
    fn roundtrip_lyid() {
        let mut info = LayerAdditionalInfo::default();
        info.id = Some(42.0);
        let out = roundtrip(info);
        assert_eq!(out.id, Some(42.0));
    }

    #[test]
    fn roundtrip_lspf_protected() {
        let mut info = LayerAdditionalInfo::default();
        info.protected_info = Some(ProtectedInfo {
            transparency: Some(true),
            composite: Some(false),
            position: Some(true),
            artboards: Some(true),
        });
        let out = roundtrip(info);
        let p = out.protected_info.expect("protected");
        assert_eq!(p.transparency, Some(true));
        // composite was false → bit unset → read back as Some(false).
        assert_eq!(p.composite, Some(false));
        assert_eq!(p.position, Some(true));
        assert_eq!(p.artboards, Some(true));
    }

    #[test]
    fn roundtrip_lsct_section_divider() {
        let mut info = LayerAdditionalInfo::default();
        info.section_divider = Some(SectionDivider {
            divider_type: SectionDividerType::OpenFolder,
            key: Some("pass".to_string()),
            sub_type: Some(1.0),
        });
        let out = roundtrip(info);
        let d = out.section_divider.expect("section_divider");
        assert_eq!(d.divider_type, SectionDividerType::OpenFolder);
        assert_eq!(d.key.as_deref(), Some("pass"));
        assert_eq!(d.sub_type, Some(1.0));
    }

    #[test]
    fn roundtrip_lsct_type_only() {
        let mut info = LayerAdditionalInfo::default();
        info.section_divider = Some(SectionDivider {
            divider_type: SectionDividerType::BoundingSectionDivider,
            key: None,
            sub_type: None,
        });
        let out = roundtrip(info);
        let d = out.section_divider.expect("section_divider");
        assert_eq!(d.divider_type, SectionDividerType::BoundingSectionDivider);
        assert_eq!(d.key, None);
        assert_eq!(d.sub_type, None);
    }

    #[test]
    fn roundtrip_lclr_color() {
        let mut info = LayerAdditionalInfo::default();
        info.layer_color = Some(LayerColor::Blue);
        let out = roundtrip(info);
        assert_eq!(out.layer_color, Some(LayerColor::Blue));
    }

    #[test]
    fn roundtrip_bool_keys() {
        let mut info = LayerAdditionalInfo::default();
        info.blend_clippend_elements = Some(true);
        info.knockout = Some(true);
        info.transparency_shapes_layer = Some(false);
        let out = roundtrip(info);
        assert_eq!(out.blend_clippend_elements, Some(true));
        assert_eq!(out.knockout, Some(true));
        assert_eq!(out.transparency_shapes_layer, Some(false));
    }

    #[test]
    fn roundtrip_iopa_brst_lyvr_fxrp() {
        let mut info = LayerAdditionalInfo::default();
        info.fill_opacity = Some(128.0 / 255.0);
        info.channel_blending_restrictions = Some(vec![0.0, 1.0, 2.0]);
        info.version = Some(70.0);
        info.reference_point = Some(PointF { x: 1.5, y: -3.25 });
        let out = roundtrip(info);
        // fill_opacity round-trips через u8.
        assert_eq!((out.fill_opacity.unwrap() * 255.0).round() as u8, 128);
        // brst читается через `while left() > 4` (зеркало upstream): для N
        // каналов читается N-1 (последние 4 байта остаются). Это поведение
        // воспроизводится точно — последний канал не возвращается.
        assert_eq!(out.channel_blending_restrictions, Some(vec![0.0, 1.0]));
        assert_eq!(out.version, Some(70.0));
        let rp = out.reference_point.unwrap();
        assert_eq!(rp.x, 1.5);
        assert_eq!(rp.y, -3.25);
    }
}
