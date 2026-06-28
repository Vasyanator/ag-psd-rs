/*
File: crates/ag-psd/src/additional_info/vector_keys.rs

Purpose:
Group-модуль additional-info ключей. Группа: `Group::Vector`.
Векторные маски/заливки/штрихи (vmsk, vsms, vscg, vstk, vogk, vowv, SoCo, GdFl,
PtFl, pths). Порт соответствующих `addHandler(...)` блоков из
`test/ag-psd/src/additionalInfo.ts`.

CANONICAL HOME для парсинга векторной маски / bezier-кривых:
`read_vector_mask` / `read_bezier_knot` / `boolean_operations` живут ЗДЕСЬ
(используются vmsk-хэндлером). Локальная копия существует также в `csh.rs`
(она появилась раньше этого модуля); её следует позже переключить на вызовы
сюда. См. отчёт.

DESCRIPTOR-BASED KEYS: SoCo / GdFl / PtFl / vscg / vstk / vogk используют
descriptor-дерево (`read_version_and_descriptor`). Vector-content/color строятся
явными вариантами `DescriptorValue` через локально-портированные
`parse_color` / `serialize_color` / `parse_vector_content` /
`serialize_vector_content` (upstream-аналоги живут в `descriptor.ts`, ещё не
портированы в крейтовый `descriptor.rs` — DEPENDENCY GAP, см. отчёт).

GROUP-MODULE CONTRACT (см. mod.rs):
- `pub fn read(key, reader, info, left, ctx) -> ReadResult<Option<()>>`
    Ok(Some(())) — ключ обработан; Ok(None) — "не мой ключ".
- `pub fn has(key, info) -> Option<bool>`
    Some(b) — этот group-модуль владеет ключом, b = нужно ли его писать;
    None — "не мой ключ".
- `pub fn write(key, writer, info, ctx) -> Option<ReadResult<()>>`
    Some(Ok(())) — записан; None — "не мой ключ". Вызывается ТОЛЬКО когда
    has(key, info) == Some(true), внутри уже открытой writeSection.
*/

use crate::additional_info::{ReadCtx, WriteCtx};
use crate::descriptor::{
    read_version_and_descriptor, write_version_and_descriptor, Descriptor, DescriptorValue,
};
use crate::psd::{
    BezierKnot, BezierPath, BooleanOperation, Color, FillRule, LayerAdditionalInfo,
    LayerVectorMask, VectorContent, VectorMaskClipboard,
};
use crate::psd::Rgb;
use crate::reader::{
    read_fixed_point_path32, read_int16, read_int32, read_signature, read_uint16, read_uint32,
    skip_bytes, PsdReader, ReadError, ReadResult,
};
use crate::writer::{
    write_fixed_point_path32, write_int16, write_int32, write_signature, write_uint16, write_uint32,
    write_zeros, PsdWriter,
};

// ===========================================================================
// Shared vector-mask / bezier-path parsing (CANONICAL HOME)
// ===========================================================================

/// `booleanOperations: BooleanOperation[]` из additionalInfo.ts (index -> op).
fn boolean_operation(index: i16) -> Option<BooleanOperation> {
    match index {
        0 => Some(BooleanOperation::Exclude),
        1 => Some(BooleanOperation::Combine),
        2 => Some(BooleanOperation::Subtract),
        3 => Some(BooleanOperation::Intersect),
        _ => None,
    }
}

/// `booleanOperations.indexOf(op)`.
fn boolean_operation_index(op: BooleanOperation) -> i16 {
    match op {
        BooleanOperation::Exclude => 0,
        BooleanOperation::Combine => 1,
        BooleanOperation::Subtract => 2,
        BooleanOperation::Intersect => 3,
    }
}

/// Порт `readBezierKnot(reader, width, height)` — точки 8.24 fixed-point,
/// порядок (y0,x0,y1,x1,y2,x2), возвращает `[x0,y0,x1,y1,x2,y2]`.
pub fn read_bezier_knot(reader: &mut PsdReader, width: f64, height: f64) -> ReadResult<Vec<f64>> {
    let y0 = read_fixed_point_path32(reader)? * height;
    let x0 = read_fixed_point_path32(reader)? * width;
    let y1 = read_fixed_point_path32(reader)? * height;
    let x1 = read_fixed_point_path32(reader)? * width;
    let y2 = read_fixed_point_path32(reader)? * height;
    let x2 = read_fixed_point_path32(reader)? * width;
    Ok(vec![x0, y0, x1, y1, x2, y2])
}

/// Порт `writeBezierKnot(writer, points, width, height)`.
fn write_bezier_knot(writer: &mut PsdWriter, points: &[f64], width: f64, height: f64) {
    let safe = |v: f64, d: f64| if d != 0.0 { v / d } else { 0.0 };
    write_fixed_point_path32(writer, safe(points[1], height)); // y0
    write_fixed_point_path32(writer, safe(points[0], width)); // x0
    write_fixed_point_path32(writer, safe(points[3], height)); // y1
    write_fixed_point_path32(writer, safe(points[2], width)); // x1
    write_fixed_point_path32(writer, safe(points[5], height)); // y2
    write_fixed_point_path32(writer, safe(points[4], width)); // x2
}

/// Порт `readVectorMask(reader, vectorMask, width, height, size)`.
///
/// CANONICAL HOME — `csh.rs` держит дубликат, который позже должен звать сюда.
pub fn read_vector_mask(
    reader: &mut PsdReader,
    vector_mask: &mut LayerVectorMask,
    width: f64,
    height: f64,
    size: usize,
) -> ReadResult<()> {
    let end = reader.offset + size;
    let mut current: Option<usize> = None;

    while end.saturating_sub(reader.offset) >= 26 {
        let selector = read_uint16(reader)?;

        match selector {
            // Closed (0) / Open (3) subpath length record
            0 | 3 => {
                let _count = read_uint16(reader)?;
                let bool_op = read_int16(reader)?;
                let flags = read_uint16(reader)?; // bit 1 always 1 ?
                skip_bytes(reader, 18);
                let mut path = BezierPath {
                    open: selector == 3,
                    operation: None,
                    knots: Vec::new(),
                    fill_rule: if flags == 2 {
                        FillRule::NonZero
                    } else {
                        FillRule::EvenOdd
                    },
                };
                if bool_op != -1 {
                    path.operation = boolean_operation(bool_op);
                }
                vector_mask.paths.push(path);
                current = Some(vector_mask.paths.len() - 1);
            }
            // Bezier knot: closed-linked(1)/closed-unlinked(2)/open-linked(4)/open-unlinked(5)
            1 | 2 | 4 | 5 => {
                let points = read_bezier_knot(reader, width, height)?;
                let idx = current
                    .ok_or_else(|| ReadError::StrictViolation("Invalid vmsk section".to_string()))?;
                vector_mask.paths[idx].knots.push(BezierKnot {
                    linked: selector == 1 || selector == 4,
                    points,
                });
            }
            // Path fill rule record
            6 => {
                skip_bytes(reader, 24);
            }
            // Clipboard record
            7 => {
                let top = read_fixed_point_path32(reader)?;
                let left = read_fixed_point_path32(reader)?;
                let bottom = read_fixed_point_path32(reader)?;
                let right = read_fixed_point_path32(reader)?;
                let resolution = read_fixed_point_path32(reader)?;
                skip_bytes(reader, 4);
                vector_mask.clipboard = Some(VectorMaskClipboard {
                    top,
                    left,
                    bottom,
                    right,
                    resolution,
                });
            }
            // Initial fill rule record
            8 => {
                vector_mask.fill_starts_with_all_pixels = Some(read_uint16(reader)? != 0);
                skip_bytes(reader, 22);
            }
            _ => return Err(ReadError::StrictViolation("Invalid vmsk section".to_string())),
        }
    }

    Ok(())
}

/// Порт тела writer-блока `vmsk` (path records). Точное зеркало upstream:
/// initial entry (selector 6 + 24 zeros), optional clipboard (7), initial fill
/// rule (8), затем по path: subpath length record + knots.
fn write_vector_mask_paths(writer: &mut PsdWriter, mask: &LayerVectorMask, width: f64, height: f64) {
    // initial entry
    write_uint16(writer, 6);
    write_zeros(writer, 24);

    if let Some(clip) = &mask.clipboard {
        write_uint16(writer, 7);
        write_fixed_point_path32(writer, clip.top);
        write_fixed_point_path32(writer, clip.left);
        write_fixed_point_path32(writer, clip.bottom);
        write_fixed_point_path32(writer, clip.right);
        write_fixed_point_path32(writer, clip.resolution);
        write_zeros(writer, 4);
    }

    write_uint16(writer, 8);
    write_uint16(
        writer,
        if mask.fill_starts_with_all_pixels == Some(true) {
            1
        } else {
            0
        },
    );
    write_zeros(writer, 22);

    for path in &mask.paths {
        write_uint16(writer, if path.open { 3 } else { 0 });
        write_uint16(writer, path.knots.len() as u16); // count
        write_int16(
            writer,
            path.operation.map(boolean_operation_index).unwrap_or(-1),
        ); // -1 for undefined
        write_uint16(
            writer,
            match path.fill_rule {
                FillRule::NonZero => 2,
                FillRule::EvenOdd => 1,
            },
        );
        write_zeros(writer, 18); // TODO: these are sometimes non-zero

        let linked_knot: u16 = if path.open { 4 } else { 1 };
        let unlinked_knot: u16 = if path.open { 5 } else { 2 };

        for knot in &path.knots {
            write_uint16(writer, if knot.linked { linked_knot } else { unlinked_knot });
            write_bezier_knot(writer, &knot.points, width, height);
        }
    }
}

// ===========================================================================
// Color / vector-content descriptor helpers (LOCAL PORT)
//
// Порт upstream `parseColor`/`serializeColor`/`parseVectorContent`/
// `serializeVectorContent` из `descriptor.ts`. Только color-content полностью
// поддержан здесь (gradient/pattern требуют непортированных `serializeGradient`
// и пр. — DEPENDENCY GAP, см. отчёт).
// ===========================================================================

fn dv_f64(v: &DescriptorValue) -> Option<f64> {
    match v {
        DescriptorValue::Double(x) => Some(*x),
        DescriptorValue::Integer(x) => Some(*x as f64),
        DescriptorValue::UnitDouble(u) => Some(u.value),
        _ => None,
    }
}

// parseColor / serializeColor — теперь канонически в descriptor.rs.
use crate::descriptor::{parse_color, serialize_color};

/// Порт `parseVectorContent(descriptor)`. Поддержан только color-case;
/// gradient/pattern требуют непортированных хелперов (DEPENDENCY GAP).
fn parse_vector_content(d: &Descriptor) -> ReadResult<VectorContent> {
    if d.get("Grad").is_some() {
        return Err(ReadError::StrictViolation(
            "vector_keys: gradient vector content not yet supported (parseGradientContent unported)"
                .to_string(),
        ));
    }
    if d.get("Ptrn").is_some() {
        return Err(ReadError::StrictViolation(
            "vector_keys: pattern vector content not yet supported (parsePatternContent unported)"
                .to_string(),
        ));
    }
    if let Some(DescriptorValue::Descriptor(clr)) = d.get("Clr ") {
        return Ok(VectorContent::Color(parse_color(clr)?));
    }
    Err(ReadError::StrictViolation(
        "Invalid vector content".to_string(),
    ))
}

/// Порт `serializeVectorContent(content)` -> (descriptor, key). Color-case
/// полностью поддержан; gradient/pattern — DEPENDENCY GAP.
fn serialize_vector_content(content: &VectorContent) -> ReadResult<(Descriptor, &'static str)> {
    match content {
        VectorContent::Color(color) => {
            let mut d = Descriptor::new("", "null");
            d.set("Clr ", DescriptorValue::Descriptor(serialize_color(Some(color))));
            Ok((d, "SoCo"))
        }
        VectorContent::SolidGradient { .. } | VectorContent::NoiseGradient { .. } => {
            Err(ReadError::StrictViolation(
                "vector_keys: gradient vector content serialization not yet supported".to_string(),
            ))
        }
        VectorContent::Pattern { .. } => Err(ReadError::StrictViolation(
            "vector_keys: pattern vector content serialization not yet supported".to_string(),
        )),
    }
}

// ===========================================================================
// READ
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn read(
    key: &str,
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
    ctx: &mut ReadCtx,
) -> ReadResult<Option<()>> {
    match key {
        // -- vmsk / vsms: vector mask --------------------------------------
        "vmsk" | "vsms" => {
            if read_uint32(reader)? != 3 {
                return Err(ReadError::StrictViolation("Invalid vmsk version".to_string()));
            }

            let mut vector_mask = LayerVectorMask::default();
            let flags = read_uint32(reader)?;
            vector_mask.invert = Some((flags & 1) != 0);
            vector_mask.not_link = Some((flags & 2) != 0);
            vector_mask.disable = Some((flags & 4) != 0);

            let (width, height) = doc_size(ctx);
            read_vector_mask(reader, &mut vector_mask, width, height, left(reader))?;

            info.vector_mask = Some(vector_mask);
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }

        // -- vowv: something with vectors (always 2 ?) ---------------------
        "vowv" => {
            info.vowv = Some(read_uint32(reader)? as f64);
            Ok(Some(()))
        }

        // -- SoCo / PtFl: descriptor vector fill ---------------------------
        "SoCo" | "PtFl" => {
            let desc = read_version_and_descriptor(reader)?;
            info.vector_fill = Some(parse_vector_content(&desc)?);
            Ok(Some(()))
        }

        // -- GdFl: descriptor vector fill (has trailing bytes) -------------
        "GdFl" => {
            let desc = read_version_and_descriptor(reader)?;
            info.vector_fill = Some(parse_vector_content(&desc)?);
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }

        // -- vscg: vector stroke content -----------------------------------
        "vscg" => {
            let _key = read_signature(reader)?; // key (color class)
            let desc = read_version_and_descriptor(reader)?;
            info.vector_fill = Some(parse_vector_content(&desc)?);
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }

        // -- vstk: vector stroke data --------------------------------------
        "vstk" => {
            let desc = read_version_and_descriptor(reader)?;
            info.vector_stroke = Some(parse_vector_stroke(&desc)?);
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }

        // -- vogk: vector origination --------------------------------------
        "vogk" => {
            if read_int32(reader)? != 1 {
                return Err(ReadError::StrictViolation("Invalid vogk version".to_string()));
            }
            let _desc = read_version_and_descriptor(reader)?;
            // NOTE: keyDescriptorList parsing requires unported descriptor unit
            // helpers (parseUnits/parseUnitsOrNumber). We preserve presence with
            // an empty list so `has`/round-trip framing matches upstream shape.
            info.vector_origination =
                Some(crate::psd::VectorOrigination { key_descriptor_list: Vec::new() });
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }

        // -- pths: paths ---------------------------------------------------
        "pths" => {
            let _desc = read_version_and_descriptor(reader)?;
            // TODO upstream: read paths. Upstream sets pathList = [] (no paths
            // read yet); mirror that.
            info.path_list = Some(Vec::new());
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }

        _ => Ok(None),
    }
}

/// Размер документа (width, height) для масштабирования bezier-точек.
///
/// FRAMEWORK GAP: upstream берёт `{ width, height }` из `psd` (vmsk-хэндлер
/// получает их 4-м аргументом). `ReadCtx`/`WriteCtx` в этом порте НЕ несут
/// размеров документа, поэтому используем единичный масштаб (1.0×1.0) —
/// bezier-точки тогда читаются/пишутся как identity (8.24 fixed-point напрямую).
/// Это сохраняет round-trip, но даёт ненормализованные координаты против PS.
/// См. отчёт: ctx должен получить width/height.
fn doc_size(_ctx: &ReadCtx) -> (f64, f64) {
    (1.0, 1.0)
}

/// Частичный порт vstk-чтения. Полностью поддержан content (vector-content)
/// и булевы/скалярные поля; единицы и enum-кодеки штриха — DEPENDENCY GAP.
fn parse_vector_stroke(desc: &Descriptor) -> ReadResult<crate::psd::VectorStroke> {
    let mut stroke = crate::psd::VectorStroke::default();
    if let Some(DescriptorValue::Boolean(b)) = desc.get("strokeEnabled") {
        stroke.stroke_enabled = Some(*b);
    }
    if let Some(DescriptorValue::Boolean(b)) = desc.get("fillEnabled") {
        stroke.fill_enabled = Some(*b);
    }
    if let Some(v) = desc.get("strokeStyleMiterLimit").and_then(dv_f64) {
        stroke.miter_limit = Some(v);
    }
    if let Some(v) = desc.get("strokeStyleResolution").and_then(dv_f64) {
        stroke.resolution = Some(v);
    }
    if let Some(DescriptorValue::Descriptor(content)) = desc.get("strokeStyleContent") {
        if let Ok(c) = parse_vector_content(content) {
            stroke.content = Some(c);
        }
    }
    Ok(stroke)
}

// ===========================================================================
// HAS
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs. Зеркало upstream-предикатов хэндлеров.
pub fn has(key: &str, info: &LayerAdditionalInfo) -> Option<bool> {
    match key {
        "vmsk" | "vsms" => Some(info.vector_mask.is_some()),
        "vowv" => Some(info.vowv.is_some()),
        "vogk" => Some(info.vector_origination.is_some()),
        "vstk" => Some(info.vector_stroke.is_some()),
        "pths" => Some(info.path_list.is_some()),
        // SoCo: color fill, no stroke
        "SoCo" => Some(
            info.vector_stroke.is_none()
                && matches!(info.vector_fill, Some(VectorContent::Color(_))),
        ),
        // GdFl: solid/noise gradient fill, no stroke
        "GdFl" => Some(
            info.vector_stroke.is_none()
                && matches!(
                    info.vector_fill,
                    Some(VectorContent::SolidGradient { .. })
                        | Some(VectorContent::NoiseGradient { .. })
                ),
        ),
        // PtFl: pattern fill, no stroke
        "PtFl" => Some(
            info.vector_stroke.is_none()
                && matches!(info.vector_fill, Some(VectorContent::Pattern { .. })),
        ),
        // vscg: stroke content (fill + stroke both present)
        "vscg" => Some(info.vector_fill.is_some() && info.vector_stroke.is_some()),
        _ => None,
    }
}

// ===========================================================================
// WRITE
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs. Вызывается только если has==Some(true).
pub fn write(
    key: &str,
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    ctx: &mut WriteCtx,
) -> Option<ReadResult<()>> {
    match key {
        "vmsk" | "vsms" => {
            let mask = info.vector_mask.as_ref()?;
            let flags = (if mask.invert == Some(true) { 1 } else { 0 })
                | (if mask.not_link == Some(true) { 2 } else { 0 })
                | (if mask.disable == Some(true) { 4 } else { 0 });

            write_uint32(writer, 3); // version
            write_uint32(writer, flags);

            let (width, height) = write_doc_size(ctx);
            write_vector_mask_paths(writer, mask, width, height);
            Some(Ok(()))
        }

        "vowv" => {
            write_uint32(writer, info.vowv.unwrap_or(0.0) as u32);
            Some(Ok(()))
        }

        "SoCo" | "GdFl" | "PtFl" => {
            let fill = info.vector_fill.as_ref()?;
            Some(match serialize_vector_content(fill) {
                Ok((desc, _key)) => {
                    write_version_and_descriptor(writer, &desc);
                    Ok(())
                }
                Err(e) => Err(e),
            })
        }

        "vscg" => {
            let fill = info.vector_fill.as_ref()?;
            Some(match serialize_vector_content(fill) {
                Ok((desc, content_key)) => {
                    write_signature(writer, content_key);
                    write_version_and_descriptor(writer, &desc);
                    Ok(())
                }
                Err(e) => Err(e),
            })
        }

        "vstk" => Some(write_vector_stroke(writer, info)),

        "vogk" => {
            // Mirror upstream framing: version + descriptor. keyDescriptorList
            // serialization is a DEPENDENCY GAP (unported unit helpers); emit an
            // empty list descriptor.
            write_int32(writer, 1); // version
            let mut desc = Descriptor::new("", "null");
            desc.set("keyDescriptorList", DescriptorValue::List(Vec::new()));
            write_version_and_descriptor(writer, &desc);
            Some(Ok(()))
        }

        "pths" => {
            let mut desc = Descriptor::new("", "pathsDataClass");
            desc.set("pathList", DescriptorValue::List(Vec::new()));
            write_version_and_descriptor(writer, &desc);
            Some(Ok(()))
        }

        _ => None,
    }
}

fn write_doc_size(_ctx: &WriteCtx) -> (f64, f64) {
    // FRAMEWORK GAP — см. `doc_size`. Единичный масштаб для round-trip.
    (1.0, 1.0)
}

/// Частичный порт vstk-записи (см. `parse_vector_stroke` — DEPENDENCY GAP по
/// единицам/enum-кодекам штриха).
fn write_vector_stroke(writer: &mut PsdWriter, info: &LayerAdditionalInfo) -> ReadResult<()> {
    let stroke = match info.vector_stroke.as_ref() {
        Some(s) => s,
        None => return Ok(()),
    };
    let mut desc = Descriptor::new("", "strokeStyle");
    desc.set("strokeStyleVersion", DescriptorValue::Integer(2));
    desc.set(
        "strokeEnabled",
        DescriptorValue::Boolean(stroke.stroke_enabled.unwrap_or(false)),
    );
    desc.set(
        "fillEnabled",
        DescriptorValue::Boolean(stroke.fill_enabled.unwrap_or(false)),
    );
    desc.set(
        "strokeStyleMiterLimit",
        DescriptorValue::Double(stroke.miter_limit.unwrap_or(100.0)),
    );
    desc.set(
        "strokeStyleResolution",
        DescriptorValue::Double(stroke.resolution.unwrap_or(72.0)),
    );
    let content = stroke
        .content
        .clone()
        .unwrap_or(VectorContent::Color(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 0.0 })));
    let (content_desc, _) = serialize_vector_content(&content)?;
    desc.set(
        "strokeStyleContent",
        DescriptorValue::Descriptor(content_desc),
    );
    write_version_and_descriptor(writer, &desc);
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::{ReadOptions, WriteOptions};
    use crate::reader::PsdReader;
    use crate::writer::{create_writer, get_writer_buffer};

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-2
    }

    fn read_ctx(opts: &ReadOptions) -> ReadCtx<'_> {
        ReadCtx { options: opts, large: false }
    }

    #[test]
    fn vmsk_round_trip() {
        let mut mask = LayerVectorMask::default();
        mask.invert = Some(true);
        mask.disable = Some(false);
        mask.fill_starts_with_all_pixels = Some(true);
        mask.paths.push(BezierPath {
            open: false,
            operation: Some(BooleanOperation::Combine),
            knots: vec![
                BezierKnot { linked: true, points: vec![10.0, 20.0, 11.0, 21.0, 12.0, 22.0] },
                BezierKnot { linked: false, points: vec![30.0, 40.0, 31.0, 41.0, 32.0, 42.0] },
            ],
            fill_rule: FillRule::NonZero,
        });

        let mut info = LayerAdditionalInfo::default();
        info.vector_mask = Some(mask);

        // write
        let w_opts = WriteOptions::default();
        let mut ctx = WriteCtx::new(&w_opts, false);
        let mut writer = create_writer(256);
        let res = write("vmsk", &mut writer, &info, &mut ctx).expect("vmsk write");
        res.expect("vmsk write ok");
        let bytes = get_writer_buffer(&writer);

        // read
        let r_opts = ReadOptions::default();
        let len = bytes.len();
        let mut reader = PsdReader::new(&bytes, None, None);
        let mut out = LayerAdditionalInfo::default();
        let mut rctx = read_ctx(&r_opts);
        let left = move |r: &PsdReader| len.saturating_sub(r.offset);
        read("vmsk", &mut reader, &mut out, &left, &mut rctx)
            .expect("vmsk read")
            .expect("vmsk handled");

        let m = out.vector_mask.expect("mask present");
        assert_eq!(m.invert, Some(true));
        assert_eq!(m.disable, Some(false));
        assert_eq!(m.fill_starts_with_all_pixels, Some(true));
        assert_eq!(m.paths.len(), 1);
        let p = &m.paths[0];
        assert!(!p.open);
        assert_eq!(p.operation, Some(BooleanOperation::Combine));
        assert_eq!(p.fill_rule, FillRule::NonZero);
        assert_eq!(p.knots.len(), 2);
        assert!(p.knots[0].linked);
        assert!(!p.knots[1].linked);
        for i in 0..6 {
            assert!(
                approx_eq(p.knots[0].points[i], [10.0, 20.0, 11.0, 21.0, 12.0, 22.0][i]),
                "knot point {i}"
            );
        }
    }

    #[test]
    fn soco_round_trip() {
        let mut info = LayerAdditionalInfo::default();
        info.vector_fill = Some(VectorContent::Color(Color::Rgb(Rgb {
            r: 12.0,
            g: 34.0,
            b: 56.0,
        })));

        assert_eq!(has("SoCo", &info), Some(true));

        let w_opts = WriteOptions::default();
        let mut ctx = WriteCtx::new(&w_opts, false);
        let mut writer = create_writer(256);
        write("SoCo", &mut writer, &info, &mut ctx)
            .expect("SoCo write")
            .expect("SoCo write ok");
        let bytes = get_writer_buffer(&writer);

        let r_opts = ReadOptions::default();
        let len = bytes.len();
        let mut reader = PsdReader::new(&bytes, None, None);
        let mut out = LayerAdditionalInfo::default();
        let mut rctx = read_ctx(&r_opts);
        let left = move |r: &PsdReader| len.saturating_sub(r.offset);
        read("SoCo", &mut reader, &mut out, &left, &mut rctx)
            .expect("SoCo read")
            .expect("SoCo handled");

        match out.vector_fill {
            Some(VectorContent::Color(Color::Rgb(c))) => {
                assert!(approx_eq(c.r, 12.0));
                assert!(approx_eq(c.g, 34.0));
                assert!(approx_eq(c.b, 56.0));
            }
            other => panic!("expected rgb color fill, got {other:?}"),
        }
    }
}
