/*
File: crates/ag-psd/src/additional_info/smart_object_keys.rs

Purpose:
Group-модуль additional-info ключей. Группа: `Group::SmartObject`.
Smart objects / linked / placed (PlLd, SoLd, SoLE, lnk2, lnkE, lnkD, lnk3, PxSc, Patt, Pat2, Pat3).

PORT STATUS: реализованы все ключи группы (read + has + write) как порт
одноимённых `addHandler(...)` из `test/ag-psd/src/additionalInfo.ts`:
- `PxSc` — pixel source bounds (descriptor 'PixelSource');
- `PlLd` — placed layer (legacy 'plcL' framing + warp descriptor);
- `SoLd` (+alias `SoLE`) — smart object placed layer ('soLD' + 'null'-descriptor);
- `Patt`/`Pat2`/`Pat3` — pattern definitions (raw pattern records);
- `lnk2` (+aliases `lnkD`/`lnk3`) / `lnkE` — linked files (per-link records).

GROUP-MODULE CONTRACT (см. mod.rs):
- `pub fn read(key, reader, info, left, ctx) -> ReadResult<Option<()>>`
    Ok(Some(())) — ключ обработан; Ok(None) — "не мой ключ".
- `pub fn has(key, info) -> Option<bool>`
    Some(b) — этот group-модуль владеет ключом, b = нужно ли его писать;
    None — "не мой ключ".
- `pub fn write(key, writer, info, ctx) -> Option<ReadResult<()>>`
    Some(Ok(())) — записан; None — "не мой ключ". Вызывается ТОЛЬКО когда
    has(key, info) == Some(true), внутри уже открытой writeSection.

==========================================================================
CONSOLIDATION / FRAMEWORK GAPS (см. также отчёт воркера):

1. read_pattern: CONSOLIDATED. `crate::reader::read_pattern` is the crate's only
   implementation of the primitive; `read_patt` here and the ABR `patt` section both
   call it, so both inherit its rectangle validation and its memory-budget accounting
   (`ReadOptions::total_memory_limit`). A reader created by `PsdReader::new` carries no
   budget (mirror of upstream `createReader`), so the ABR path stays unlimited exactly
   as upstream; the document reader used for `Patt`/`Pat2`/`Pat3` carries the budget.
   Pattern WRITING goes through `crate::writer::write_pattern`.

2. linkedFiles / Psd-контекст: ReadCtx/WriteCtx НЕ несут ни `Psd`, ни список
   linkedFiles. Upstream `createLnkHandler` хранит связанные файлы в `psd.linkedFiles`
   (а НЕ в per-layer `LayerAdditionalInfo`). В per-layer `LayerAdditionalInfo`
   нет поля linkedFiles, поэтому lnk2/lnkE здесь корректно framing-обрабатываются
   при ЧТЕНИИ, но СКЛАДЫВАТЬ некуда (info-уровень) — мы пропускаем секцию
   (skip), а `has` всегда возвращает Some(false). ПОЛНОЦЕННЫЙ lnk2/lnkE round-trip
   требует прокинуть `&mut Psd` (или `&mut Vec<LinkedFile>`) в ReadCtx/WriteCtx.
   Реализация записи (`write_linked_files`) присутствует и протестирована на
   уровне Vec<LinkedFile> локально, чтобы байтовый framing был зафиксирован.

3. SoLd.filterFX (smart-filter / puppet warp дерево): upstream `parseFilterFX`/
   `serializeFilterFXItem` — ~600 строк отдельного поддерева дескрипторов. Здесь
   НЕ портировано: при чтении SoLd, если присутствует `filterFX`, поле
   `placed.filter` НЕ заполняется (NOTED gap); при записи `filter` игнорируется.
   Остальной SoLd (id/placed/type/pages/transform/size/resolution/warp/quiltWarp/
   crop/comp/compInfo) портирован точно.

4. WriteCtx не несёт размеры документа; SoLd transform/warp хранятся в системе
   координат связанного изображения (width/height в самом placedLayer), поэтому
   документ-контекст не требуется для round-trip placedLayer.
*/

use crate::additional_info::{ReadCtx, WriteCtx};
use crate::descriptor::{
    read_version_and_descriptor, write_version_and_descriptor, Descriptor, DescriptorValue,
    UnitDoubleValue,
};
use crate::helpers::{Dict, EnumCodec};
use crate::psd::{
    CustomEnvelopeWarp, LayerAdditionalInfo, NumDenom, Orientation, PixelSource,
    PixelSourceFrameReader, PixelSourceFrameReaderLink, PixelSourceInterpretation, PlacedLayer,
    PlacedLayerType, PointF, Units, UnitsBounds, UnitsValue, Warp, WarpStyle,
};
use crate::reader::{
    read_float64, read_int32, read_pascal_string, read_pattern, read_signature, read_uint32,
    skip_bytes, PsdReader, ReadError, ReadResult,
};
use crate::writer::{
    write_bytes, write_float64, write_int32, write_pattern, write_signature, write_uint32,
    PsdWriter,
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
        "PxSc" => read_pxsc(reader, info)?,
        "PlLd" => read_plld(reader, info, left)?,
        "SoLd" | "SoLE" => read_sold(reader, info, left)?,
        "Patt" | "Pat2" | "Pat3" => read_patt(reader, info, left)?,
        // linked files: framing-обрабатываем и пропускаем (нет места хранения на
        // info-уровне — см. GAP 2). `left() > 8` зеркалирует upstream цикл.
        "lnk2" | "lnkD" | "lnk3" | "lnkE" => {
            skip_bytes(reader, left(reader));
        }
        _ => return Ok(None),
    }
    Ok(Some(()))
}

// ===========================================================================
// HAS
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn has(key: &str, info: &LayerAdditionalInfo) -> Option<bool> {
    match key {
        "PxSc" => Some(false), // upstream: () => false
        "PlLd" | "SoLd" | "SoLE" => Some(info.placed_layer.is_some()),
        "Patt" | "Pat2" | "Pat3" => {
            Some(info.patterns.as_ref().map(|p| !p.is_empty()).unwrap_or(false))
        }
        // linkedFiles живут в Psd, не в LayerAdditionalInfo — см. GAP 2.
        "lnk2" | "lnkD" | "lnk3" | "lnkE" => Some(false),
        _ => None,
    }
}

// ===========================================================================
// WRITE
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn write(
    key: &str,
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    _ctx: &mut WriteCtx,
) -> Option<ReadResult<()>> {
    let r = match key {
        "PxSc" => write_pxsc(writer, info),
        "PlLd" => write_plld(writer, info),
        "SoLd" | "SoLE" => write_sold(writer, info),
        "Patt" | "Pat2" | "Pat3" => {
            for pattern in info.patterns.as_deref().unwrap_or(&[]) {
                write_pattern(writer, pattern);
            }
            Ok(())
        }
        // see GAP 2: has() == Some(false), so this is never reached for lnk*.
        "lnk2" | "lnkD" | "lnk3" | "lnkE" => Ok(()),
        _ => return None,
    };
    Some(r)
}

// ===========================================================================
// Enum codecs / converters (зеркало descriptor.ts: Ornt / warpStyle)
// ===========================================================================

fn dict(pairs: &[(&str, &str)]) -> Dict {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn ornt_codec() -> EnumCodec {
    EnumCodec::new(
        "Ornt",
        "horizontal",
        dict(&[("horizontal", "Hrzn"), ("vertical", "Vrtc")]),
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

fn placed_layer_type_from_index(i: i32) -> ReadResult<PlacedLayerType> {
    Ok(match i {
        0 => PlacedLayerType::Unknown,
        1 => PlacedLayerType::Vector,
        2 => PlacedLayerType::Raster,
        3 => PlacedLayerType::ImageStack,
        other => {
            return Err(ReadError::StrictViolation(format!(
                "Invalid placedLayer type: {other}"
            )));
        }
    })
}

fn placed_layer_type_to_index(t: PlacedLayerType) -> i32 {
    match t {
        PlacedLayerType::Unknown => 0,
        PlacedLayerType::Vector => 1,
        PlacedLayerType::Raster => 2,
        PlacedLayerType::ImageStack => 3,
    }
}

// ===========================================================================
// Units helpers (зеркало parseUnits / unitsValue / parseUnitsOrNumber)
// ===========================================================================

// units helpers — канонически в descriptor.rs.
use crate::descriptor::{parse_units, parse_units_or_number};

/// Адаптер над `descriptor::units_value` (by-reference, всегда `Some`).
fn units_value(v: &UnitsValue) -> DescriptorValue {
    crate::descriptor::units_value(Some(v))
}

// --- descriptor field accessors --------------------------------------------

fn get_text(desc: &Descriptor, key: &str) -> Option<String> {
    match desc.get(key) {
        Some(DescriptorValue::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_double(desc: &Descriptor, key: &str) -> Option<f64> {
    match desc.get(key) {
        Some(DescriptorValue::Double(v)) => Some(*v),
        Some(DescriptorValue::Integer(v)) => Some(*v as f64),
        _ => None,
    }
}

fn get_integer(desc: &Descriptor, key: &str) -> Option<i32> {
    match desc.get(key) {
        Some(DescriptorValue::Integer(v)) => Some(*v),
        Some(DescriptorValue::Double(v)) => Some(*v as i32),
        _ => None,
    }
}

fn get_enum(desc: &Descriptor, key: &str) -> Option<String> {
    match desc.get(key) {
        Some(DescriptorValue::Enum(s)) => Some(s.clone()),
        _ => None,
    }
}

fn get_desc<'a>(desc: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    match desc.get(key) {
        Some(DescriptorValue::Descriptor(d)) => Some(d),
        _ => None,
    }
}

fn get_double_list(desc: &Descriptor, key: &str) -> Vec<f64> {
    match desc.get(key) {
        Some(DescriptorValue::List(items)) => items
            .iter()
            .filter_map(|v| match v {
                DescriptorValue::Double(d) => Some(*d),
                DescriptorValue::Integer(i) => Some(*i as f64),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn double_list(values: &[f64]) -> DescriptorValue {
    DescriptorValue::List(values.iter().map(|v| DescriptorValue::Double(*v)).collect())
}

// ===========================================================================
// Fraction (frac) helpers
// ===========================================================================

/// Зеркало `frac(desc)` — читает { numerator, denominator } из дескриптора.
fn read_frac(v: Option<&DescriptorValue>) -> NumDenom {
    if let Some(DescriptorValue::Descriptor(d)) = v {
        NumDenom {
            numerator: get_double(d, "numerator").unwrap_or(0.0),
            denominator: get_double(d, "denominator").unwrap_or(600.0),
        }
    } else {
        NumDenom { numerator: 0.0, denominator: 600.0 }
    }
}

fn write_frac(nd: &NumDenom) -> DescriptorValue {
    let mut d = Descriptor::new("", "null");
    d.set("numerator", DescriptorValue::Integer(nd.numerator as i32));
    d.set("denominator", DescriptorValue::Integer(nd.denominator as i32));
    DescriptorValue::Descriptor(d)
}

// ===========================================================================
// length64 (зеркало readLength64 / writeLength64 в additionalInfo.ts)
// ===========================================================================

fn read_length64(reader: &mut PsdReader) -> ReadResult<usize> {
    let hi = read_uint32(reader)?;
    if hi != 0 {
        return Err(ReadError::StrictViolation(
            "Resource size above 4 GB limit".to_string(),
        ));
    }
    Ok(read_uint32(reader)? as usize)
}

fn write_length64(writer: &mut PsdWriter, length: usize) {
    write_uint32(writer, 0);
    write_uint32(writer, length as u32);
}

// ===========================================================================
// Warp parse / encode (полный набор: bounds / orders / deform / envelope)
// зеркало parseWarp / encodeWarp / isQuiltWarp / getWarpFromPlacedLayer
// ===========================================================================

fn parse_warp(warp: &Descriptor) -> ReadResult<Warp> {
    let style_str = warp_style_codec()
        .decode(get_enum(warp, "warpStyle").as_deref().unwrap_or(""))
        .map_err(ReadError::StrictViolation)?;
    let rotate_str = ornt_codec()
        .decode(get_enum(warp, "warpRotate").as_deref().unwrap_or(""))
        .map_err(ReadError::StrictViolation)?;

    // warpValues vs warpValue
    let (value, values) = match warp.get("warpValues") {
        Some(_) => (None, Some(get_double_list(warp, "warpValues"))),
        None => (Some(get_double(warp, "warpValue").unwrap_or(0.0)), None),
    };

    let bounds = match get_desc(warp, "bounds") {
        Some(b) => {
            let get = |k: &str| -> ReadResult<UnitsValue> {
                b.get(k)
                    .ok_or_else(|| ReadError::StrictViolation(format!("Missing warp bound: {k}")))
                    .and_then(parse_units_or_number)
            };
            Some(UnitsBounds {
                top: get("Top ")?,
                left: get("Left")?,
                bottom: get("Btom")?,
                right: get("Rght")?,
            })
        }
        None => None,
    };

    let mut result = Warp {
        style: Some(warp_style_from_str(&style_str)),
        value,
        values,
        perspective: Some(get_double(warp, "warpPerspective").unwrap_or(0.0)),
        perspective_other: Some(get_double(warp, "warpPerspectiveOther").unwrap_or(0.0)),
        rotate: Some(orientation_from_str(&rotate_str)),
        bounds,
        u_order: get_double(warp, "uOrder"),
        v_order: get_double(warp, "vOrder"),
        deform_num_rows: None,
        deform_num_cols: None,
        custom_envelope_warp: None,
    };

    let has_rows = warp.get("deformNumRows").is_some();
    let has_cols = warp.get("deformNumCols").is_some();
    if has_rows || has_cols {
        result.deform_num_rows = get_double(warp, "deformNumRows");
        result.deform_num_cols = get_double(warp, "deformNumCols");
    }

    if let Some(envelope) = get_desc(warp, "customEnvelopeWarp") {
        let mut cew = CustomEnvelopeWarp::default();

        // meshPoints: ObjectArray of { type: 'Hrzn'/'Vrtc', values }
        let (xs, ys) = match envelope.get("meshPoints") {
            Some(DescriptorValue::ObjectArray(items)) => {
                let xs = items
                    .iter()
                    .find(|i| i.type_ == "Hrzn")
                    .map(|i| i.values.clone())
                    .unwrap_or_default();
                let ys = items
                    .iter()
                    .find(|i| i.type_ == "Vrtc")
                    .map(|i| i.values.clone())
                    .unwrap_or_default();
                (xs, ys)
            }
            _ => (Vec::new(), Vec::new()),
        };
        // Upstream pairs the Hrzn/Vrtc value lists positionally; a missing Vrtc
        // entry falls back to 0.0, so the loop is driven by the Hrzn list alone.
        for (i, &x) in xs.iter().enumerate() {
            cew.mesh_points.push(PointF {
                x,
                y: *ys.get(i).unwrap_or(&0.0),
            });
        }

        let qx = object_array_values(envelope, "quiltSliceX");
        let qy = object_array_values(envelope, "quiltSliceY");
        if qx.is_some() || qy.is_some() {
            cew.quilt_slice_x = Some(qx.unwrap_or_default());
            cew.quilt_slice_y = Some(qy.unwrap_or_default());
        }

        result.custom_envelope_warp = Some(cew);
    }

    Ok(result)
}

/// Достаёт `values` из первого элемента ObjectArray по ключу (для quiltSliceX/Y).
fn object_array_values(desc: &Descriptor, key: &str) -> Option<Vec<f64>> {
    match desc.get(key) {
        Some(DescriptorValue::ObjectArray(items)) => items.first().map(|i| i.values.clone()),
        _ => None,
    }
}

fn is_quilt_warp(warp: &Warp) -> bool {
    warp.deform_num_cols.is_some()
        || warp.deform_num_rows.is_some()
        || warp
            .custom_envelope_warp
            .as_ref()
            .map(|c| c.quilt_slice_x.is_some() || c.quilt_slice_y.is_some())
            .unwrap_or(false)
}

/// Зеркало `encodeWarp(warp)` (полный набор полей).
fn encode_warp(warp: &Warp) -> Descriptor {
    let class_id = if is_quilt_warp(warp) { "quiltWarp" } else { "warp" };
    let mut desc = Descriptor::new("", class_id);

    let style = warp.style.map(warp_style_to_str);
    desc.set(
        "warpStyle",
        DescriptorValue::Enum(warp_style_codec().encode(style).expect("warpStyle encode")),
    );

    if let Some(values) = &warp.values {
        desc.set("warpValues", double_list(values));
    } else {
        desc.set("warpValue", DescriptorValue::Double(warp.value.unwrap_or(0.0)));
    }

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

    let zero = UnitsValue { units: Units::Pixels, value: 0.0 };
    let bounds = warp.bounds;
    let mut b = Descriptor::new("", "classFloatRect");
    b.set("Top ", units_value(bounds.as_ref().map(|x| &x.top).unwrap_or(&zero)));
    b.set("Left", units_value(bounds.as_ref().map(|x| &x.left).unwrap_or(&zero)));
    b.set("Btom", units_value(bounds.as_ref().map(|x| &x.bottom).unwrap_or(&zero)));
    b.set("Rght", units_value(bounds.as_ref().map(|x| &x.right).unwrap_or(&zero)));
    desc.set("bounds", DescriptorValue::Descriptor(b));

    desc.set("uOrder", DescriptorValue::Integer(warp.u_order.unwrap_or(0.0) as i32));
    desc.set("vOrder", DescriptorValue::Integer(warp.v_order.unwrap_or(0.0) as i32));

    let is_quilt = is_quilt_warp(warp);
    if is_quilt {
        desc.set(
            "deformNumRows",
            DescriptorValue::Integer(warp.deform_num_rows.unwrap_or(0.0) as i32),
        );
        desc.set(
            "deformNumCols",
            DescriptorValue::Integer(warp.deform_num_cols.unwrap_or(0.0) as i32),
        );
    }

    if let Some(cew) = &warp.custom_envelope_warp {
        let mut env = Descriptor::new("", "customEnvelopeWarp");
        let xs: Vec<f64> = cew.mesh_points.iter().map(|p| p.x).collect();
        let ys: Vec<f64> = cew.mesh_points.iter().map(|p| p.y).collect();

        if is_quilt {
            env.set(
                "quiltSliceX",
                DescriptorValue::ObjectArray(vec![crate::descriptor::ObjectArrayItem {
                    type_: "quiltSliceX".to_string(),
                    values: cew.quilt_slice_x.clone().unwrap_or_default(),
                }]),
            );
            env.set(
                "quiltSliceY",
                DescriptorValue::ObjectArray(vec![crate::descriptor::ObjectArrayItem {
                    type_: "quiltSliceY".to_string(),
                    values: cew.quilt_slice_y.clone().unwrap_or_default(),
                }]),
            );
        }

        env.set(
            "meshPoints",
            DescriptorValue::ObjectArray(vec![
                crate::descriptor::ObjectArrayItem { type_: "Hrzn".to_string(), values: xs },
                crate::descriptor::ObjectArrayItem { type_: "Vrtc".to_string(), values: ys },
            ]),
        );

        desc.set("customEnvelopeWarp", DescriptorValue::Descriptor(env));
    }

    desc
}

/// Зеркало `getWarpFromPlacedLayer(placed)`.
fn get_warp_from_placed_layer(placed: &PlacedLayer) -> ReadResult<Warp> {
    if let Some(w) = &placed.warp {
        return Ok(w.clone());
    }

    let w = placed.width.unwrap_or(0.0);
    let h = placed.height.unwrap_or(0.0);
    if w == 0.0 || h == 0.0 {
        return Err(ReadError::StrictViolation(
            "You must provide width and height of the linked image in placedLayer".to_string(),
        ));
    }

    let (x0, x1, x2, x3) = (0.0, w / 3.0, w * 2.0 / 3.0, w);
    let (y0, y1, y2, y3) = (0.0, h / 3.0, h * 2.0 / 3.0, h);
    let p = |x: f64, y: f64| PointF { x, y };

    Ok(Warp {
        style: Some(WarpStyle::Custom),
        value: Some(0.0),
        values: None,
        perspective: Some(0.0),
        perspective_other: Some(0.0),
        rotate: Some(Orientation::Horizontal),
        bounds: Some(UnitsBounds {
            top: UnitsValue { value: 0.0, units: Units::Pixels },
            left: UnitsValue { value: 0.0, units: Units::Pixels },
            bottom: UnitsValue { value: h, units: Units::Pixels },
            right: UnitsValue { value: w, units: Units::Pixels },
        }),
        u_order: Some(4.0),
        v_order: Some(4.0),
        deform_num_rows: None,
        deform_num_cols: None,
        custom_envelope_warp: Some(CustomEnvelopeWarp {
            quilt_slice_x: None,
            quilt_slice_y: None,
            mesh_points: vec![
                p(x0, y0), p(x1, y0), p(x2, y0), p(x3, y0),
                p(x0, y1), p(x1, y1), p(x2, y1), p(x3, y1),
                p(x0, y2), p(x1, y2), p(x2, y2), p(x3, y2),
                p(x0, y3), p(x1, y3), p(x2, y3), p(x3, y3),
            ],
        }),
    })
}

fn check_guid(id: &str) -> ReadResult<()> {
    // /^[0-9a-f]{8}-([0-9a-f]{4}-){3}[0-9a-f]{12}$/
    let bytes = id.as_bytes();
    let pattern = [8usize, 4, 4, 4, 12];
    let mut idx = 0usize;
    let mut ok = true;
    for (gi, &group_len) in pattern.iter().enumerate() {
        if gi > 0 {
            if idx >= bytes.len() || bytes[idx] != b'-' {
                ok = false;
                break;
            }
            idx += 1;
        }
        for _ in 0..group_len {
            if idx >= bytes.len() || !bytes[idx].is_ascii_hexdigit() || bytes[idx].is_ascii_uppercase()
            {
                ok = false;
                break;
            }
            idx += 1;
        }
        if !ok {
            break;
        }
    }
    if !ok || idx != bytes.len() {
        return Err(ReadError::StrictViolation(
            "ID must be in a GUID format (example: 20953ddb-9391-11ec-b4f1-c15674f50bc4)"
                .to_string(),
        ));
    }
    Ok(())
}

// ===========================================================================
// PxSc
// ===========================================================================

fn read_pxsc(reader: &mut PsdReader, info: &mut LayerAdditionalInfo) -> ReadResult<()> {
    let desc = read_version_and_descriptor(reader)?;

    if get_integer(&desc, "pixelSourceType") == Some(1986285651) {
        let origin = get_desc(&desc, "origin")
            .ok_or_else(|| ReadError::StrictViolation("PxSc missing origin".to_string()))?;
        let interp = get_desc(&desc, "interpretation")
            .ok_or_else(|| ReadError::StrictViolation("PxSc missing interpretation".to_string()))?;
        let frame_reader = get_desc(&desc, "frameReader")
            .ok_or_else(|| ReadError::StrictViolation("PxSc missing frameReader".to_string()))?;
        let link = get_desc(frame_reader, "Lnk ")
            .ok_or_else(|| ReadError::StrictViolation("PxSc missing Lnk".to_string()))?;

        // interpretAlpha enum string "alphaInterpretation.<value>" -> "<value>"
        let interpret_alpha = get_enum(interp, "interpretAlpha")
            .and_then(|s| s.split('.').nth(1).map(|s| s.to_string()))
            .unwrap_or_default();
        let profile = match interp.get("profile") {
            Some(DescriptorValue::RawData(d)) => d.clone(),
            _ => Vec::new(),
        };

        info.pixel_source = Some(PixelSource {
            source_type: "vdPS".to_string(),
            origin: PointF {
                x: get_double(origin, "Hrzn").unwrap_or(0.0),
                y: get_double(origin, "Vrtc").unwrap_or(0.0),
            },
            interpretation: PixelSourceInterpretation { interpret_alpha, profile },
            frame_reader: PixelSourceFrameReader {
                reader_type: "QTFR".to_string(),
                link: PixelSourceFrameReaderLink {
                    name: get_text(link, "Nm  ").unwrap_or_default(),
                    full_path: get_text(link, "fullPath").unwrap_or_default(),
                    original_path: get_text(link, "originalPath").unwrap_or_default(),
                    relative_path: get_text(link, "relPath").unwrap_or_default(),
                    alias: match link.get("alis") {
                        Some(DescriptorValue::Alias(s)) => s.clone(),
                        Some(DescriptorValue::Text(s)) => s.clone(),
                        _ => String::new(),
                    },
                },
                media_descriptor: get_text(frame_reader, "mediaDescriptor").unwrap_or_default(),
            },
            show_altered_video: matches!(desc.get("showAlteredVideo"), Some(DescriptorValue::Boolean(true))),
        });
    }
    // else: unknown pixelSourceType — upstream just logs.
    Ok(())
}

fn write_pxsc(writer: &mut PsdWriter, info: &LayerAdditionalInfo) -> ReadResult<()> {
    let source = info
        .pixel_source
        .as_ref()
        .ok_or_else(|| ReadError::StrictViolation("PxSc: missing pixelSource".to_string()))?;

    let mut desc = Descriptor::new("", "PixelSource");
    desc.set("pixelSourceType", DescriptorValue::Integer(1986285651));
    desc.set("descVersion", DescriptorValue::Integer(1));

    let mut origin = Descriptor::new("", "Pnt ");
    origin.set("Hrzn", DescriptorValue::Double(source.origin.x));
    origin.set("Vrtc", DescriptorValue::Double(source.origin.y));
    desc.set("origin", DescriptorValue::Descriptor(origin));

    let mut interp = Descriptor::new("", "footageInterpretation");
    interp.set("Vrsn", DescriptorValue::Integer(1));
    interp.set(
        "interpretAlpha",
        DescriptorValue::Enum(format!(
            "alphaInterpretation.{}",
            source.interpretation.interpret_alpha
        )),
    );
    interp.set(
        "profile",
        DescriptorValue::RawData(source.interpretation.profile.clone()),
    );
    desc.set("interpretation", DescriptorValue::Descriptor(interp));

    let mut frame_reader = Descriptor::new("", "FrameReader");
    frame_reader.set("frameReaderType", DescriptorValue::Integer(1364477522));
    frame_reader.set("descVersion", DescriptorValue::Integer(1));

    let mut link = Descriptor::new("", "ExternalFileLink");
    link.set("descVersion", DescriptorValue::Integer(2));
    link.set("Nm  ", DescriptorValue::Text(source.frame_reader.link.name.clone()));
    link.set("fullPath", DescriptorValue::Text(source.frame_reader.link.full_path.clone()));
    link.set(
        "originalPath",
        DescriptorValue::Text(source.frame_reader.link.original_path.clone()),
    );
    link.set("alis", DescriptorValue::Alias(source.frame_reader.link.alias.clone()));
    link.set("relPath", DescriptorValue::Text(source.frame_reader.link.relative_path.clone()));
    frame_reader.set("Lnk ", DescriptorValue::Descriptor(link));
    frame_reader.set(
        "mediaDescriptor",
        DescriptorValue::Text(source.frame_reader.media_descriptor.clone()),
    );
    desc.set("frameReader", DescriptorValue::Descriptor(frame_reader));

    desc.set(
        "showAlteredVideo",
        DescriptorValue::Boolean(source.show_altered_video),
    );

    write_version_and_descriptor(writer, &desc);
    Ok(())
}

// ===========================================================================
// PlLd (legacy placed layer)
// ===========================================================================

fn read_plld(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if read_signature(reader)? != "plcL" {
        return Err(ReadError::StrictViolation("Invalid PlLd signature".to_string()));
    }
    if read_int32(reader)? != 3 {
        return Err(ReadError::StrictViolation("Invalid PlLd version".to_string()));
    }
    let id = read_pascal_string(reader, 1)?;
    let page_number = read_int32(reader)?;
    let total_pages = read_int32(reader)?;
    read_int32(reader)?; // antiAliasPolicy 16
    let placed_layer_type = placed_layer_type_from_index(read_int32(reader)?)?;

    let mut transform = Vec::with_capacity(8);
    for _ in 0..8 {
        transform.push(read_float64(reader)?);
    }

    let warp_version = read_int32(reader)?;
    if warp_version != 0 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid Warp version {warp_version}"
        )));
    }
    let warp_desc = read_version_and_descriptor(reader)?;

    // skip if SoLd already set it
    if info.placed_layer.is_none() {
        info.placed_layer = Some(PlacedLayer {
            id,
            layer_type: Some(placed_layer_type),
            page_number: Some(page_number as f64),
            total_pages: Some(total_pages as f64),
            transform,
            warp: Some(parse_warp(&warp_desc)?),
            ..PlacedLayer::default()
        });
    }

    skip_bytes(reader, left(reader));
    Ok(())
}

/// Mirrors upstream `placed.pageNumber || 1` / `placed.totalPages || 1`.
///
/// A missing value and an explicit `0` both fall back to `1`: page indices are
/// 1-based, and JS treats `0` as falsy at these write sites.
fn page_value(value: Option<f64>) -> i32 {
    match value {
        Some(v) if v != 0.0 => v as i32,
        _ => 1,
    }
}

fn write_plld(writer: &mut PsdWriter, info: &LayerAdditionalInfo) -> ReadResult<()> {
    let placed = info
        .placed_layer
        .as_ref()
        .ok_or_else(|| ReadError::StrictViolation("PlLd: missing placedLayer".to_string()))?;

    write_signature(writer, "plcL");
    write_int32(writer, 3); // version

    check_guid(&placed.id)?;
    crate::writer::write_pascal_string(writer, &placed.id, 1);
    write_int32(writer, page_value(placed.page_number)); // pageNumber
    write_int32(writer, page_value(placed.total_pages)); // totalPages
    write_int32(writer, 16); // antiAliasPolicy
    let t = placed
        .layer_type
        .ok_or_else(|| ReadError::StrictViolation("Invalid placedLayer type".to_string()))?;
    write_int32(writer, placed_layer_type_to_index(t));
    for i in 0..8 {
        write_float64(writer, *placed.transform.get(i).unwrap_or(&0.0));
    }
    write_int32(writer, 0); // warp version
    let warp = get_warp_from_placed_layer(placed)?;
    write_version_and_descriptor(writer, &encode_warp(&warp));
    Ok(())
}

// ===========================================================================
// SoLd (smart object placed layer)
// ===========================================================================

fn read_sold(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if read_signature(reader)? != "soLD" {
        return Err(ReadError::StrictViolation("Invalid SoLd type".to_string()));
    }
    let version = read_int32(reader)?;
    if version != 4 && version != 5 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid SoLd version {version}"
        )));
    }
    let desc = read_version_and_descriptor(reader)?;

    let trnf = get_double_list(&desc, "Trnf");
    let non_affine = get_double_list(&desc, "nonAffineTransform");

    let size = get_desc(&desc, "Sz  ");
    let warp_src = if desc.get("quiltWarp").is_some() {
        get_desc(&desc, "quiltWarp")
    } else {
        get_desc(&desc, "warp")
    };

    let mut placed = PlacedLayer {
        id: get_text(&desc, "Idnt").unwrap_or_default(),
        placed: get_text(&desc, "placed"),
        layer_type: Some(placed_layer_type_from_index(get_integer(&desc, "Type").unwrap_or(0))?),
        page_number: get_double(&desc, "PgNm"),
        total_pages: get_double(&desc, "totalPages"),
        frame_step: Some(read_frac(desc.get("frameStep"))),
        duration: Some(read_frac(desc.get("duration"))),
        frame_count: get_double(&desc, "frameCount"),
        transform: trnf.clone(),
        width: size.and_then(|s| get_double(s, "Wdth")),
        height: size.and_then(|s| get_double(s, "Hght")),
        resolution: desc.get("Rslt").and_then(|v| parse_units(v).ok()),
        warp: warp_src.map(parse_warp).transpose()?,
        ..PlacedLayer::default()
    };

    if !non_affine.is_empty()
        && non_affine.len() == trnf.len()
        && non_affine.iter().zip(trnf.iter()).any(|(a, b)| a != b)
    {
        placed.non_affine_transform = Some(non_affine);
    }

    if let Some(c) = get_double(&desc, "Crop") {
        if c != 0.0 {
            placed.crop = Some(c);
        }
    }
    if let Some(c) = get_double(&desc, "comp") {
        if c != 0.0 {
            placed.comp = Some(c);
        }
    }
    if let Some(ci) = get_desc(&desc, "compInfo") {
        placed.comp_info = Some(crate::psd::CompInfo {
            comp_id: get_double(ci, "compID").unwrap_or(0.0),
            original_comp_id: get_double(ci, "originalCompID").unwrap_or(0.0),
        });
    }
    // NOTE (GAP 3): desc.filterFX (smart filters / puppet warp) not ported.

    info.placed_layer = Some(placed);

    skip_bytes(reader, left(reader)); // HACK (upstream)
    Ok(())
}

fn write_sold(writer: &mut PsdWriter, info: &LayerAdditionalInfo) -> ReadResult<()> {
    write_signature(writer, "soLD");
    write_int32(writer, 4); // version

    let placed = info
        .placed_layer
        .as_ref()
        .ok_or_else(|| ReadError::StrictViolation("SoLd: missing placedLayer".to_string()))?;

    check_guid(&placed.id)?;

    let mut desc = Descriptor::new("", "null");
    desc.set("Idnt", DescriptorValue::Text(placed.id.clone()));
    desc.set(
        "placed",
        DescriptorValue::Text(placed.placed.clone().unwrap_or_else(|| placed.id.clone())),
    );
    desc.set("PgNm", DescriptorValue::Integer(page_value(placed.page_number)));
    desc.set(
        "totalPages",
        DescriptorValue::Integer(page_value(placed.total_pages)),
    );
    if let Some(crop) = placed.crop {
        desc.set("Crop", DescriptorValue::Integer(crop as i32));
    }
    desc.set(
        "frameStep",
        write_frac(&placed.frame_step.unwrap_or(NumDenom { numerator: 0.0, denominator: 600.0 })),
    );
    desc.set(
        "duration",
        write_frac(&placed.duration.unwrap_or(NumDenom { numerator: 0.0, denominator: 600.0 })),
    );
    desc.set(
        "frameCount",
        DescriptorValue::Integer(placed.frame_count.unwrap_or(0.0) as i32),
    );
    desc.set("Annt", DescriptorValue::Integer(16));
    let t = placed
        .layer_type
        .ok_or_else(|| ReadError::StrictViolation("Invalid placedLayer type".to_string()))?;
    desc.set("Type", DescriptorValue::Integer(placed_layer_type_to_index(t)));
    desc.set("Trnf", double_list(&placed.transform));
    desc.set(
        "nonAffineTransform",
        double_list(placed.non_affine_transform.as_deref().unwrap_or(&placed.transform)),
    );

    // warp / quiltWarp: mirror upstream split.
    let warp = get_warp_from_placed_layer(placed)?;
    let quilt = placed.warp.as_ref().map(is_quilt_warp).unwrap_or(false);

    if quilt {
        let quilt_warp = encode_warp(placed.warp.as_ref().unwrap());
        // warp фолбэк-дескриптор (warpStyle.warpNone), копируя поля из quiltWarp.
        let mut warp_desc = Descriptor::new("", "warp");
        warp_desc.set(
            "warpStyle",
            DescriptorValue::Enum("warpStyle.warpNone".to_string()),
        );
        copy_field(&quilt_warp, &mut warp_desc, "warpValue");
        copy_field(&quilt_warp, &mut warp_desc, "warpPerspective");
        copy_field(&quilt_warp, &mut warp_desc, "warpPerspectiveOther");
        copy_field(&quilt_warp, &mut warp_desc, "warpRotate");
        copy_field(&quilt_warp, &mut warp_desc, "bounds");
        copy_field(&quilt_warp, &mut warp_desc, "uOrder");
        copy_field(&quilt_warp, &mut warp_desc, "vOrder");
        desc.set("warp", DescriptorValue::Descriptor(warp_desc));
        desc.set("quiltWarp", DescriptorValue::Descriptor(quilt_warp));
    } else {
        desc.set("warp", DescriptorValue::Descriptor(encode_warp(&warp)));
    }

    let mut size = Descriptor::new("", "Pnt ");
    size.set("Wdth", DescriptorValue::Integer(placed.width.unwrap_or(0.0) as i32));
    size.set("Hght", DescriptorValue::Integer(placed.height.unwrap_or(0.0) as i32));
    desc.set("Sz  ", DescriptorValue::Descriptor(size));

    desc.set(
        "Rslt",
        match &placed.resolution {
            Some(r) => units_value(r),
            None => DescriptorValue::UnitDouble(UnitDoubleValue {
                units: "Density".to_string(),
                value: 72.0,
            }),
        },
    );

    if let Some(c) = placed.comp {
        desc.set("comp", DescriptorValue::Integer(c as i32));
    }
    if let Some(ci) = placed.comp_info {
        let mut ci_desc = Descriptor::new("", "null");
        ci_desc.set("compID", DescriptorValue::Integer(ci.comp_id as i32));
        ci_desc.set(
            "originalCompID",
            DescriptorValue::Integer(ci.original_comp_id as i32),
        );
        desc.set("compInfo", DescriptorValue::Descriptor(ci_desc));
    }
    // NOTE (GAP 3): placed.filter (filterFX) not serialized.

    write_version_and_descriptor(writer, &desc);
    Ok(())
}

fn copy_field(src: &Descriptor, dst: &mut Descriptor, key: &str) {
    if let Some(v) = src.get(key) {
        dst.set(key, v.clone());
    }
}

// ===========================================================================
// Linked files (lnk2 / lnkD / lnk3 / lnkE) — write framing
//
// Не вызывается из write() (см. GAP 2: has == Some(false)), но определено и
// протестировано отдельно, чтобы зафиксировать байтовый framing для будущего
// прокидывания linkedFiles через WriteCtx.
// ===========================================================================

#[allow(dead_code)]
fn write_linked_files(
    writer: &mut PsdWriter,
    tag: &str,
    linked_files: &[crate::psd::LinkedFile],
) -> ReadResult<()> {
    for file in linked_files {
        if (tag == "lnkE") != file.linked_file.is_some() {
            continue;
        }

        let mut version = 2;
        if file.asset_locked_state.is_some() {
            version = 7;
        } else if file.asset_mod_time.is_some() {
            version = 6;
        } else if file.child_document_id.is_some() {
            version = 5;
        } else if tag == "lnkE" {
            version = 3;
        }

        write_length64(writer, 0);
        let size_offset = writer.offset;

        let sig = if tag == "lnkE" {
            "liFE"
        } else if file.data.is_some() {
            "liFD"
        } else {
            "liFA"
        };
        write_signature(writer, sig);
        write_int32(writer, version);

        check_guid(&file.id)?;
        crate::writer::write_pascal_string(writer, &file.id, 1);
        crate::writer::write_unicode_string_with_padding(writer, &file.name);

        // type / creator signatures (4 bytes each)
        write_signature(writer, &pad_sig(file.file_type.as_deref(), "    "));
        write_signature(writer, &pad_sig(file.creator.as_deref(), "\0\0\0\0"));

        write_length64(writer, file.data.as_ref().map(|d| d.len()).unwrap_or(0));

        if let Some(desc) = &file.descriptor {
            crate::writer::write_uint8(writer, 1);
            let mut d = Descriptor::new("", "null");
            let mut ci = Descriptor::new("", "null");
            ci.set("compID", DescriptorValue::Integer(desc.comp_info.comp_id as i32));
            ci.set(
                "originalCompID",
                DescriptorValue::Integer(desc.comp_info.original_comp_id as i32),
            );
            d.set("compInfo", DescriptorValue::Descriptor(ci));
            write_version_and_descriptor(writer, &d);
        } else {
            crate::writer::write_uint8(writer, 0);
        }

        if tag == "lnkE" {
            let lf = file.linked_file.clone().unwrap_or_default();
            let mut d = Descriptor::new("", "ExternalFileLink");
            d.set("descVersion", DescriptorValue::Integer(2));
            d.set("Nm  ", DescriptorValue::Text(lf.name.clone()));
            d.set("fullPath", DescriptorValue::Text(lf.full_path.clone()));
            d.set("originalPath", DescriptorValue::Text(lf.original_path.clone()));
            d.set("relPath", DescriptorValue::Text(lf.relative_path.clone()));
            write_version_and_descriptor(writer, &d);

            // Date: upstream uses file.time or now(); we write zeros for determinism
            // when time is absent. NOTE: full date round-trip needs chrono; we only
            // preserve framing here.
            write_int32(writer, 0); // year
            crate::writer::write_uint8(writer, 0); // month
            crate::writer::write_uint8(writer, 0); // day
            crate::writer::write_uint8(writer, 0); // hour
            crate::writer::write_uint8(writer, 0); // minute
            write_float64(writer, 0.0); // seconds
        }

        if let Some(data) = &file.data {
            write_bytes(writer, Some(data));
        } else {
            write_length64(
                writer,
                file.linked_file.as_ref().map(|l| l.file_size as usize).unwrap_or(0),
            );
        }

        if version >= 5 {
            crate::writer::write_unicode_string_with_padding(
                writer,
                file.child_document_id.as_deref().unwrap_or(""),
            );
        }
        if version >= 6 {
            write_float64(writer, file.asset_mod_time.unwrap_or(0.0));
        }
        if version >= 7 {
            crate::writer::write_uint8(writer, file.asset_locked_state.unwrap_or(0.0) as u8);
        }

        let mut size = writer.offset - size_offset;
        // backpatch size at (size_offset - 4) big-endian.
        let bytes = (size as u32).to_be_bytes();
        writer.buffer[size_offset - 4..size_offset].copy_from_slice(&bytes);

        while size % 4 != 0 {
            crate::writer::write_uint8(writer, 0);
            size += 1;
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn pad_sig(value: Option<&str>, empty: &str) -> String {
    match value {
        Some(v) if !v.is_empty() => {
            let mut s: String = v.chars().chain("    ".chars()).take(4).collect();
            s.truncate(4);
            s
        }
        _ => empty.to_string(),
    }
}

/// Reads the `Patt`/`Pat2`/`Pat3` section: back-to-back pattern records until
/// the section is exhausted.
///
/// Decoding is delegated to [`crate::reader::read_pattern`], the crate's single
/// implementation of the primitive, so this path is covered by its rectangle
/// validation and by the `ReadOptions::total_memory_limit` budget carried by the
/// document reader.
///
/// # Errors
/// Propagates every error of [`crate::reader::read_pattern`].
fn read_patt(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    while left(reader) > 0 {
        let pattern = read_pattern(reader)?;
        info.patterns.get_or_insert_with(Vec::new).push(pattern);
    }
    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::{ColorMode, PatternBounds, PatternInfo};
    use crate::reader::PsdReader;
    use crate::writer::{create_writer_default, get_writer_buffer};

    fn sample_placed() -> PlacedLayer {
        PlacedLayer {
            id: "20953ddb-9391-11ec-b4f1-c15674f50bc4".to_string(),
            placed: Some("20953ddb-9391-11ec-b4f1-c15674f50bc4".to_string()),
            layer_type: Some(PlacedLayerType::Raster),
            page_number: Some(1.0),
            total_pages: Some(1.0),
            frame_step: Some(NumDenom { numerator: 0.0, denominator: 600.0 }),
            duration: Some(NumDenom { numerator: 0.0, denominator: 600.0 }),
            frame_count: Some(1.0),
            transform: vec![0.0, 0.0, 100.0, 0.0, 100.0, 100.0, 0.0, 100.0],
            non_affine_transform: None,
            width: Some(100.0),
            height: Some(100.0),
            resolution: Some(UnitsValue { units: Units::Density, value: 72.0 }),
            warp: None,
            crop: None,
            comp: None,
            comp_info: None,
            filter: None,
        }
    }

    #[test]
    fn sold_round_trip() {
        let info = LayerAdditionalInfo {
            placed_layer: Some(sample_placed()),
            ..LayerAdditionalInfo::default()
        };

        // write inside an open section (here we just write the body directly).
        let mut writer = create_writer_default();
        write_sold(&mut writer, &info).unwrap();
        let buf = get_writer_buffer(&writer);

        // read back
        let mut reader = PsdReader::new(&buf, None, None);
        let total = buf.len();
        let left = move |r: &PsdReader| total - r.offset;
        let mut out = LayerAdditionalInfo::default();
        read_sold(&mut reader, &mut out, &left).unwrap();

        let p = out.placed_layer.expect("placed layer parsed");
        assert_eq!(p.id, "20953ddb-9391-11ec-b4f1-c15674f50bc4");
        assert_eq!(p.layer_type, Some(PlacedLayerType::Raster));
        assert_eq!(p.transform, vec![0.0, 0.0, 100.0, 0.0, 100.0, 100.0, 0.0, 100.0]);
        assert_eq!(p.width, Some(100.0));
        assert_eq!(p.height, Some(100.0));
        // warp synthesized from width/height should round-trip as custom warp.
        let w = p.warp.expect("warp");
        assert_eq!(w.style, Some(WarpStyle::Custom));
        assert!(w.custom_envelope_warp.is_some());
        assert_eq!(w.custom_envelope_warp.unwrap().mesh_points.len(), 16);
    }

    #[test]
    fn patt_round_trip() {
        // 2x2 RGBA pattern
        let data = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ];
        let pattern = PatternInfo {
            name: "test".to_string(),
            id: "deadbeef-0000-0000-0000-000000000000".to_string(),
            x: 0.0,
            y: 0.0,
            bounds: PatternBounds { x: 0.0, y: 0.0, w: 2.0, h: 2.0 },
            data: data.clone(),
        };

        let mut writer = create_writer_default();
        write_pattern(&mut writer, &pattern);
        let buf = get_writer_buffer(&writer);

        let mut reader = PsdReader::new(&buf, None, None);
        let out = read_pattern(&mut reader).unwrap();

        assert_eq!(out.name, "test");
        assert_eq!(out.id, "deadbeef-0000-0000-0000-000000000000");
        assert_eq!(out.bounds.w, 2.0);
        assert_eq!(out.bounds.h, 2.0);
        // RGB channels round-trip (alpha is forced to 255 by reader).
        for px in 0..4 {
            for c in 0..3 {
                assert_eq!(out.data[px * 4 + c], data[px * 4 + c], "pixel {px} ch {c}");
            }
        }
    }

    /// Builds a pattern record whose channels are stored uncompressed
    /// (`compressionMode == 0`).
    ///
    /// `channels` supplies one `w * h` sample plane per present channel, in
    /// channel order; two absent slots are appended because the reader always
    /// walks `channelsCount + 2` entries. `palette` must be `Some` exactly for
    /// `ColorMode::Indexed`.
    ///
    /// Feeds the shared `crate::reader::read_pattern`; `abr.rs` keeps a similar
    /// builder for the ABR-side test of the same function.
    fn raw_pattern_bytes(
        color_mode: ColorMode,
        palette: Option<&[[u8; 3]; 256]>,
        channels: &[&[u8]],
        w: u32,
        h: u32,
    ) -> Vec<u8> {
        use crate::writer::{
            write_int16, write_pascal_string, write_uint16, write_uint8, write_unicode_string,
        };

        let mut body = create_writer_default();
        write_uint32(&mut body, 1); // version
        write_uint32(&mut body, color_mode as u32);
        write_int16(&mut body, 0); // x
        write_int16(&mut body, 0); // y
        write_unicode_string(&mut body, "pat\0");
        write_pascal_string(&mut body, "deadbeef-0000-0000-0000-000000000000", 1);

        if let Some(palette) = palette {
            for entry in palette.iter() {
                write_uint8(&mut body, entry[0]);
                write_uint8(&mut body, entry[1]);
                write_uint8(&mut body, entry[2]);
            }
            write_uint32(&mut body, 0); // 4 bytes the reader skips
        }

        write_uint32(&mut body, 3); // virtual memory array list version
        write_uint32(&mut body, 0); // list length, unused by the reader
        write_uint32(&mut body, 0); // top
        write_uint32(&mut body, 0); // left
        write_uint32(&mut body, h); // bottom
        write_uint32(&mut body, w); // right
        write_uint32(&mut body, channels.len() as u32);

        for channel in channels {
            write_uint32(&mut body, 1); // has
            write_uint32(&mut body, (channel.len() + 4 + 16 + 2 + 1) as u32);
            write_uint32(&mut body, 8); // pixelDepth
            write_uint32(&mut body, 0); // ctop
            write_uint32(&mut body, 0); // cleft
            write_uint32(&mut body, h); // cbottom
            write_uint32(&mut body, w); // cright
            write_uint16(&mut body, 8); // pixelDepth2
            write_uint8(&mut body, 0); // compressionMode: raw
            write_bytes(&mut body, Some(channel));
        }
        write_uint32(&mut body, 0); // absent slot
        write_uint32(&mut body, 0); // absent slot

        let mut payload = get_writer_buffer(&body);
        // The reader rounds the record length up to a multiple of 4 before
        // computing the record end, so keep the payload aligned.
        while payload.len() % 4 != 0 {
            payload.push(0);
        }

        let mut out = create_writer_default();
        write_uint32(&mut out, payload.len() as u32);
        write_bytes(&mut out, Some(&payload));
        get_writer_buffer(&out)
    }

    #[test]
    fn read_pattern_decodes_indexed_raw_data() {
        let mut palette = [[0u8; 3]; 256];
        palette[1] = [10, 20, 30];
        palette[2] = [40, 50, 60];
        palette[3] = [70, 80, 90];
        palette[4] = [100, 110, 120];
        let indices: [u8; 4] = [1, 2, 3, 4];
        let bytes = raw_pattern_bytes(ColorMode::Indexed, Some(&palette), &[&indices], 2, 2);

        let mut reader = PsdReader::new(&bytes, None, None);
        let out = read_pattern(&mut reader).expect("indexed pattern must decode");

        assert_eq!(out.bounds.w, 2.0);
        assert_eq!(out.bounds.h, 2.0);
        for (px, index) in indices.iter().enumerate() {
            let color = palette[*index as usize];
            assert_eq!(&out.data[px * 4..px * 4 + 3], &color[..], "pixel {px}");
            assert_eq!(out.data[px * 4 + 3], 255, "pixel {px} alpha");
        }
    }

    /// The live `Patt`/`Pat2`/`Pat3` path must reject a hostile pattern
    /// rectangle with a typed error.
    ///
    /// Regression test for the guard being wired into the wrong copy of
    /// `read_pattern`: this module used to decode patterns with its own,
    /// unguarded copy, so `bottom`/`right` of `0xffffffff` reached
    /// `vec![0u8; width * height * 4]` — a wrapping multiplication (release) or
    /// an overflow panic (debug), and no memory-budget check at all.
    #[test]
    fn patt_rejects_a_hostile_pattern_rectangle() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&100u32.to_be_bytes()); // record length
        bytes.extend_from_slice(&1u32.to_be_bytes()); // version
        bytes.extend_from_slice(&(ColorMode::Rgb as u32).to_be_bytes());
        bytes.extend_from_slice(&0i16.to_be_bytes()); // x
        bytes.extend_from_slice(&0i16.to_be_bytes()); // y
        bytes.extend_from_slice(&0u32.to_be_bytes()); // unicode name length
        bytes.push(0); // pascal string id length
        bytes.extend_from_slice(&3u32.to_be_bytes()); // VMAL version
        bytes.extend_from_slice(&0u32.to_be_bytes()); // VMAL length (unused)
        bytes.extend_from_slice(&0u32.to_be_bytes()); // top
        bytes.extend_from_slice(&0u32.to_be_bytes()); // left
        bytes.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // bottom
        bytes.extend_from_slice(&0xffff_ffffu32.to_be_bytes()); // right
        bytes.extend_from_slice(&0u32.to_be_bytes()); // channels count

        let mut reader = PsdReader::new(&bytes, None, None);
        // Same budget `read_psd` installs by default.
        reader.total_memory_limit = Some(crate::psd::DEFAULT_TOTAL_MEMORY_LIMIT);
        let total = bytes.len();
        let left = move |r: &PsdReader| total.saturating_sub(r.offset);

        let mut info = LayerAdditionalInfo::default();
        let err = read_patt(&mut reader, &mut info, &left).unwrap_err();
        assert_eq!(
            err,
            ReadError::InvalidBoxSize {
                kind: "pattern",
                width: 0xffff_ffff,
                height: 0xffff_ffff
            }
        );
        assert!(info.patterns.is_none(), "nothing must be stored on failure");
    }

    /// A raw channel that does not belong to the pattern's colour mode (here the
    /// alpha channel of a grayscale pattern) is ignored by default and only
    /// reported when `throw_for_missing_features` is set.
    #[test]
    fn read_pattern_reports_unhandled_raw_channel_only_when_requested() {
        let gray: [u8; 4] = [10, 20, 30, 40];
        let alpha: [u8; 4] = [255, 255, 255, 255];
        let bytes = raw_pattern_bytes(ColorMode::Grayscale, None, &[&gray, &alpha], 2, 2);

        let mut reader = PsdReader::new(&bytes, None, None);
        let out = read_pattern(&mut reader).expect("grayscale pattern must decode");
        for (px, value) in gray.iter().enumerate() {
            assert_eq!(&out.data[px * 4..px * 4 + 3], &[*value, *value, *value][..]);
        }

        let mut strict = PsdReader::new(&bytes, None, None);
        strict.options.throw_for_missing_features = Some(true);
        assert!(read_pattern(&mut strict).is_err());
    }

    #[test]
    fn plld_writes_page_numbers_from_data() {
        let mut placed = sample_placed();
        placed.page_number = Some(3.0);
        placed.total_pages = Some(7.0);
        let info = LayerAdditionalInfo {
            placed_layer: Some(placed),
            ..LayerAdditionalInfo::default()
        };

        let mut writer = create_writer_default();
        write_plld(&mut writer, &info).unwrap();
        let buf = get_writer_buffer(&writer);

        let mut reader = PsdReader::new(&buf, None, None);
        let total = buf.len();
        let left = move |r: &PsdReader| total - r.offset;
        let mut out = LayerAdditionalInfo::default();
        read_plld(&mut reader, &mut out, &left).unwrap();

        let p = out.placed_layer.expect("placed layer parsed");
        assert_eq!(p.page_number, Some(3.0));
        assert_eq!(p.total_pages, Some(7.0));
    }

    #[test]
    fn lnk_write_framing_size_is_multiple_of_4() {
        use crate::psd::LinkedFile;
        let files = vec![LinkedFile {
            id: "20953ddb-9391-11ec-b4f1-c15674f50bc4".to_string(),
            name: "image.png".to_string(),
            data: Some(vec![1, 2, 3, 4, 5]),
            ..LinkedFile::default()
        }];
        let mut writer = create_writer_default();
        write_linked_files(&mut writer, "lnk2", &files).unwrap();
        let buf = get_writer_buffer(&writer);
        // total written should be padded to a multiple of 4.
        assert_eq!(buf.len() % 4, 0);
        // first 4 bytes are the high half of length64 == 0.
        assert_eq!(&buf[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn every_enum_codec_default_is_a_map_key() {
        // The default must be a map KEY: `encode(None)` resolves through `map[def]`.
        for codec in [ornt_codec(), warp_style_codec()] {
            assert!(codec.default_is_valid());
        }
    }
}
