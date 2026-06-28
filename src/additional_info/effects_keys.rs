/*
File: crates/ag-psd/src/additional_info/effects_keys.rs

Purpose:
Group-модуль additional-info ключей. Группа: `Group::Effects`.
Эффекты слоя (lmfx, lrFX, lfxs, lfx2).

PORT STATUS: реализованы все четыре ключа (`lfx2`, `lrFX`, `lmfx`, `lfxs`).
- `lrFX` — legacy signature-block формат: делегируется в
  `crate::effects_helpers::{read_effects, write_effects}`.
- `lfx2` / `lmfx` / `lfxs` — descriptor-based формат: здесь портированы
  `parseEffects` / `serializeEffects` (+ `parseEffectObject` /
  `serializeEffectObject`, gradient/pattern/contour/color мапперы) из upstream
  `test/ag-psd/src/descriptor.ts`, отображающие descriptor <-> LayerEffectsInfo.

ПРИЧИНА ПОРТА ЗДЕСЬ (а не реюз `crate::effects_helpers`):
`crate::effects_helpers` несёт ТОЛЬКО legacy lrFX-хелперы (read_effects /
write_effects). Descriptor-side мапперы (`parseEffects`/`serializeEffects` и
их зависимости из `descriptor.ts`) в Rust-крейте ещё нигде не портированы —
их нет ни в `descriptor.rs` (там только типизированное дерево), ни в
`effects_helpers.rs`. Поэтому они портированы локально в этом файле. См. отчёт
о зависимостях (DEPENDENCY GAPS).

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
    parse_angle, parse_percent, parse_units, read_version_and_descriptor, units_angle,
    write_version_and_descriptor, Descriptor, DescriptorValue, UnitDoubleValue,
};
use crate::effects_helpers::{read_effects, write_effects};
use crate::psd::{
    BevelDirection, BevelStyle, BevelTechnique, BlendMode, Color, ColorStop, EffectContour,
    EffectGradient, EffectNoiseGradient, EffectPattern, EffectSolidGradient, ExtraGradientInfo,
    GlowSource, GlowTechnique, GradientColorModel, GradientStyle, GradientWithExtra,
    InterpolationMethod, LayerAdditionalInfo, LayerEffectBevel, LayerEffectGradientOverlay,
    LayerEffectInnerGlow, LayerEffectPatternOverlay, LayerEffectSatin, LayerEffectShadow,
    LayerEffectSolidFill, LayerEffectStroke, LayerEffectsInfo, LayerEffectsOuterGlow, OpacityStop,
    PointF, StrokeFillType, StrokePosition, UnitsValue,
};
use crate::reader::{read_uint32, skip_bytes, PsdReader, ReadError, ReadResult};
use crate::writer::{write_uint32, PsdWriter};

// ===========================================================================
// Public group-module contract entry points
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
        "lrFX" => {
            // upstream: if (!target.effects) target.effects = readEffects(reader);
            if info.effects.is_none() {
                info.effects = Some(read_effects(reader)?);
            }
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }
        "lmfx" => {
            let version = read_uint32(reader)?;
            if version != 0 {
                return Err(ReadError::StrictViolation("Invalid lmfx version".to_string()));
            }
            let desc = read_version_and_descriptor(reader)?;
            info.effects = Some(parse_effects(&desc)?);
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }
        "lfxs" => {
            let version = read_uint32(reader)?;
            if version != 0 {
                return Err(ReadError::StrictViolation("Invalid lfxs version".to_string()));
            }
            let desc = read_version_and_descriptor(reader)?;
            info.effects = Some(parse_effects(&desc)?);
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }
        "lfx2" => {
            let version = read_uint32(reader)?;
            if version != 0 {
                return Err(ReadError::StrictViolation("Invalid lfx2 version".to_string()));
            }
            let desc = read_version_and_descriptor(reader)?;
            info.effects = Some(parse_effects(&desc)?);
            skip_bytes(reader, left(reader));
            Ok(Some(()))
        }
        _ => Ok(None),
    }
}

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn has(key: &str, info: &LayerAdditionalInfo) -> Option<bool> {
    match key {
        // upstream lmfx: target.effects !== undefined && hasMultiEffects(target.effects)
        "lmfx" => Some(info.effects.as_ref().map(has_multi_effects).unwrap_or(false)),
        // upstream lrFX: hasKey('effects')
        "lrFX" => Some(info.effects.is_some()),
        // upstream lfxs: () => false
        "lfxs" => Some(false),
        // upstream lfx2: target.effects !== undefined && !hasMultiEffects(target.effects)
        "lfx2" => Some(
            info.effects
                .as_ref()
                .map(|e| !has_multi_effects(e))
                .unwrap_or(false),
        ),
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
        "lrFX" => {
            if let Some(effects) = info.effects.as_ref() {
                write_effects(writer, effects);
            }
            Some(Ok(()))
        }
        "lmfx" | "lfxs" | "lfx2" => {
            // upstream serializeEffects(target.effects!, log, true)
            let effects = info.effects.as_ref();
            let result = match effects {
                Some(e) => serialize_effects(e),
                // has() guarantees effects.is_some() for these keys, but be safe.
                None => Ok(Descriptor::new("", "null")),
            };
            match result {
                Ok(desc) => {
                    write_uint32(writer, 0); // version
                    write_version_and_descriptor(writer, &desc);
                    Some(Ok(()))
                }
                Err(e) => Some(Err(e)),
            }
        }
        _ => None,
    }
}

/// Зеркало `hasMultiEffects(effects)`: есть ли массив эффектов длиной > 1.
pub fn has_multi_effects(e: &LayerEffectsInfo) -> bool {
    fn multi<T>(v: &Option<Vec<T>>) -> bool {
        v.as_ref().map(|a| a.len() > 1).unwrap_or(false)
    }
    multi(&e.drop_shadow)
        || multi(&e.inner_shadow)
        || multi(&e.solid_fill)
        || multi(&e.stroke)
        || multi(&e.gradient_overlay)
}

// ===========================================================================
// Enum codecs (зеркала createEnum<...> из descriptor.ts)
// ===========================================================================
//
// createEnum.decode(val): берёт часть после '.', ищет в reverse-map, иначе def.
// createEnum.encode(val): `${prefix}.${code}`. Здесь — типизированные пары.

/// Общий декодер: `"prefix.code"` -> значение по таблице (или default).
fn decode_enum<T: Copy>(val: &str, table: &[(&str, T)], default: T) -> T {
    let code = val.split('.').nth(1).unwrap_or("");
    if code.is_empty() {
        return default;
    }
    table
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, v)| *v)
        .unwrap_or(default)
}

/// Общий энкодер: значение -> `"prefix.code"`.
fn encode_enum<T: PartialEq>(prefix: &str, val: T, table: &[(&str, T)], default_code: &str) -> String {
    let code = table
        .iter()
        .find(|(_, v)| *v == val)
        .map(|(c, _)| *c)
        .unwrap_or(default_code);
    format!("{}.{}", prefix, code)
}

// --- BlnM (blend mode) -----------------------------------------------------

const BLN_M: &[(&str, BlendMode)] = &[
    ("Nrml", BlendMode::Normal),
    ("Dslv", BlendMode::Dissolve),
    ("Drkn", BlendMode::Darken),
    ("Mltp", BlendMode::Multiply),
    ("CBrn", BlendMode::ColorBurn),
    ("linearBurn", BlendMode::LinearBurn),
    ("darkerColor", BlendMode::DarkerColor),
    ("Lghn", BlendMode::Lighten),
    ("Scrn", BlendMode::Screen),
    ("CDdg", BlendMode::ColorDodge),
    ("linearDodge", BlendMode::LinearDodge),
    ("lighterColor", BlendMode::LighterColor),
    ("Ovrl", BlendMode::Overlay),
    ("SftL", BlendMode::SoftLight),
    ("HrdL", BlendMode::HardLight),
    ("vividLight", BlendMode::VividLight),
    ("linearLight", BlendMode::LinearLight),
    ("pinLight", BlendMode::PinLight),
    ("hardMix", BlendMode::HardMix),
    ("Dfrn", BlendMode::Difference),
    ("Xclu", BlendMode::Exclusion),
    ("blendSubtraction", BlendMode::Subtract),
    ("blendDivide", BlendMode::Divide),
    ("H   ", BlendMode::Hue),
    ("Strt", BlendMode::Saturation),
    ("Clr ", BlendMode::Color),
    ("Lmns", BlendMode::Luminosity),
];

fn bln_m_decode(val: &str) -> BlendMode {
    decode_enum(val, BLN_M, BlendMode::Normal)
}
fn bln_m_encode(val: BlendMode) -> String {
    encode_enum("BlnM", val, BLN_M, "Nrml")
}

// --- FStl (stroke position) ------------------------------------------------

const F_STL: &[(&str, StrokePosition)] = &[
    ("OutF", StrokePosition::Outside),
    ("CtrF", StrokePosition::Center),
    ("InsF", StrokePosition::Inside),
];

fn f_stl_decode(val: &str) -> StrokePosition {
    decode_enum(val, F_STL, StrokePosition::Outside)
}
fn f_stl_encode(val: StrokePosition) -> String {
    encode_enum("FStl", val, F_STL, "OutF")
}

// --- FrFl (stroke fill type) -----------------------------------------------

const FR_FL: &[(&str, StrokeFillType)] = &[
    ("SClr", StrokeFillType::Color),
    ("GrFl", StrokeFillType::Gradient),
    ("Ptrn", StrokeFillType::Pattern),
];

fn fr_fl_decode(val: &str) -> StrokeFillType {
    decode_enum(val, FR_FL, StrokeFillType::Color)
}
fn fr_fl_encode(val: StrokeFillType) -> String {
    encode_enum("FrFl", val, FR_FL, "SClr")
}

// --- BESl (bevel style) ----------------------------------------------------

const BE_SL: &[(&str, BevelStyle)] = &[
    ("InrB", BevelStyle::InnerBevel),
    ("OtrB", BevelStyle::OuterBevel),
    ("Embs", BevelStyle::Emboss),
    ("PlEb", BevelStyle::PillowEmboss),
    ("strokeEmboss", BevelStyle::StrokeEmboss),
];

fn be_sl_decode(val: &str) -> BevelStyle {
    decode_enum(val, BE_SL, BevelStyle::InnerBevel)
}
fn be_sl_encode(val: BevelStyle) -> String {
    encode_enum("BESl", val, BE_SL, "InrB")
}

// --- bvlT (bevel technique) ------------------------------------------------

const BVL_T: &[(&str, BevelTechnique)] = &[
    ("SfBL", BevelTechnique::Smooth),
    ("PrBL", BevelTechnique::ChiselHard),
    ("Slmt", BevelTechnique::ChiselSoft),
];

fn bvl_t_decode(val: &str) -> BevelTechnique {
    decode_enum(val, BVL_T, BevelTechnique::Smooth)
}
fn bvl_t_encode(val: BevelTechnique) -> String {
    encode_enum("bvlT", val, BVL_T, "SfBL")
}

// --- BESs (bevel direction) ------------------------------------------------

const BE_SS: &[(&str, BevelDirection)] =
    &[("In  ", BevelDirection::Up), ("Out ", BevelDirection::Down)];

fn be_ss_decode(val: &str) -> BevelDirection {
    decode_enum(val, BE_SS, BevelDirection::Up)
}
fn be_ss_encode(val: BevelDirection) -> String {
    encode_enum("BESs", val, BE_SS, "In  ")
}

// --- BETE (glow technique) -------------------------------------------------

const BE_TE: &[(&str, GlowTechnique)] =
    &[("SfBL", GlowTechnique::Softer), ("PrBL", GlowTechnique::Precise)];

fn be_te_decode(val: &str) -> GlowTechnique {
    decode_enum(val, BE_TE, GlowTechnique::Softer)
}
fn be_te_encode(val: GlowTechnique) -> String {
    encode_enum("BETE", val, BE_TE, "SfBL")
}

// --- IGSr (glow source) ----------------------------------------------------

const IG_SR: &[(&str, GlowSource)] = &[("SrcE", GlowSource::Edge), ("SrcC", GlowSource::Center)];

fn ig_sr_decode(val: &str) -> GlowSource {
    decode_enum(val, IG_SR, GlowSource::Edge)
}
fn ig_sr_encode(val: GlowSource) -> String {
    encode_enum("IGSr", val, IG_SR, "SrcE")
}

// --- GrdT (gradient style) -------------------------------------------------

const GRD_T: &[(&str, GradientStyle)] = &[
    ("Lnr ", GradientStyle::Linear),
    ("Rdl ", GradientStyle::Radial),
    ("Angl", GradientStyle::Angle),
    ("Rflc", GradientStyle::Reflected),
    ("Dmnd", GradientStyle::Diamond),
];

fn grd_t_decode(val: &str) -> GradientStyle {
    decode_enum(val, GRD_T, GradientStyle::Linear)
}
fn grd_t_encode(val: GradientStyle) -> String {
    encode_enum("GrdT", val, GRD_T, "Lnr ")
}

// --- gradientInterpolationMethodType ---------------------------------------

const GS99: &[(&str, InterpolationMethod)] = &[
    ("Perc", InterpolationMethod::Perceptual),
    ("Lnr ", InterpolationMethod::Linear),
    ("Gcls", InterpolationMethod::Classic),
    ("Smoo", InterpolationMethod::Smooth),
];

fn gs99_decode(val: &str) -> InterpolationMethod {
    decode_enum(val, GS99, InterpolationMethod::Perceptual)
}
fn gs99_encode(val: InterpolationMethod) -> String {
    encode_enum("gradientInterpolationMethodType", val, GS99, "Perc")
}

// --- ClrS (gradient color model) -------------------------------------------

const CLR_S: &[(&str, GradientColorModel)] = &[
    ("RGBC", GradientColorModel::Rgb),
    ("HSBl", GradientColorModel::Hsb),
    ("LbCl", GradientColorModel::Lab),
];

fn clr_s_decode(val: &str) -> GradientColorModel {
    decode_enum(val, CLR_S, GradientColorModel::Rgb)
}
fn clr_s_encode(val: GradientColorModel) -> String {
    encode_enum("ClrS", val, CLR_S, "RGBC")
}

// ===========================================================================
// Units helpers (зеркала descriptor.ts)
// ===========================================================================

/// `unitsPercent(value)` -> `{ units: 'Percent', value: round(value*100) }`.
fn units_percent(value: f64) -> DescriptorValue {
    DescriptorValue::UnitDouble(UnitDoubleValue {
        units: "Percent".to_string(),
        value: (value * 100.0).round(),
    })
}

/// `unitsPercentF(value)` -> `{ units: 'Percent', value: value*100 }` (без округления).
fn units_percent_f(value: f64) -> DescriptorValue {
    DescriptorValue::UnitDouble(UnitDoubleValue {
        units: "Percent".to_string(),
        value: value * 100.0,
    })
}

/// `unitsValue(x, key)` (by-value адаптер над `descriptor::units_value`).
fn units_value(x: Option<UnitsValue>) -> DescriptorValue {
    crate::descriptor::units_value(x.as_ref())
}

// ===========================================================================
// Color (DescriptorValue::Descriptor <-> Color); адаптеры над descriptor.rs
// ===========================================================================

fn desc_double(d: &Descriptor, key: &str) -> f64 {
    match d.get(key) {
        Some(DescriptorValue::Double(v)) => *v,
        Some(DescriptorValue::Integer(v)) => *v as f64,
        _ => 0.0,
    }
}

/// Адаптер над `descriptor::parse_color`: распаковывает `DescriptorValue::Descriptor`.
fn parse_color(v: &DescriptorValue) -> ReadResult<Color> {
    match v {
        DescriptorValue::Descriptor(d) => crate::descriptor::parse_color(d),
        _ => Err(ReadError::StrictViolation(
            "Unsupported color descriptor".to_string(),
        )),
    }
}

/// Адаптер над `descriptor::serialize_color`: оборачивает в `DescriptorValue::Descriptor`.
fn serialize_color(color: Option<&Color>) -> DescriptorValue {
    DescriptorValue::Descriptor(crate::descriptor::serialize_color(color))
}

// ===========================================================================
// Contour (TrnS / MpgS); зеркало parse/serialize в parseEffectObject
// ===========================================================================

fn parse_contour(v: &DescriptorValue) -> EffectContour {
    let d = match v {
        DescriptorValue::Descriptor(d) => d,
        _ => return EffectContour::default(),
    };
    let name = match d.get("Nm  ") {
        Some(DescriptorValue::Text(s)) => s.clone(),
        _ => String::new(),
    };
    let curve = match d.get("Crv ") {
        Some(DescriptorValue::List(items)) => items
            .iter()
            .filter_map(|it| match it {
                DescriptorValue::Descriptor(p) => {
                    Some(PointF { x: desc_double(p, "Hrzn"), y: desc_double(p, "Vrtc") })
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    EffectContour { name, curve }
}

fn serialize_contour(c: &EffectContour) -> DescriptorValue {
    let mut d = Descriptor::new("", "ShpC");
    d.set("Nm  ", DescriptorValue::Text(c.name.clone()));
    let curve = c
        .curve
        .iter()
        .map(|p| {
            let mut pt = Descriptor::new("", "Pnt ");
            pt.set("Hrzn", DescriptorValue::Double(p.x));
            pt.set("Vrtc", DescriptorValue::Double(p.y));
            DescriptorValue::Descriptor(pt)
        })
        .collect();
    d.set("Crv ", DescriptorValue::List(curve));
    DescriptorValue::Descriptor(d)
}

/// Пустой default-контур TrnS для dropShadow (зеркало `{ 'Nm  ': '', 'Crv ': [] }`).
fn empty_contour() -> DescriptorValue {
    let mut d = Descriptor::new("", "ShpC");
    d.set("Nm  ", DescriptorValue::Text(String::new()));
    d.set("Crv ", DescriptorValue::List(Vec::new()));
    DescriptorValue::Descriptor(d)
}

// ===========================================================================
// Gradient (Grad descriptor); зеркала parseGradient/serializeGradient
// ===========================================================================

fn parse_gradient(v: &DescriptorValue) -> ReadResult<EffectGradient> {
    let d = match v {
        DescriptorValue::Descriptor(d) => d,
        _ => return Err(ReadError::StrictViolation("Invalid gradient".to_string())),
    };
    let grd_f = match d.get("GrdF") {
        Some(DescriptorValue::Enum(s)) => s.clone(),
        _ => String::new(),
    };
    let name = match d.get("Nm  ") {
        Some(DescriptorValue::Text(s)) => s.clone(),
        _ => String::new(),
    };
    if grd_f == "GrdF.CstS" {
        let intr = desc_double(d, "Intr");
        let samples = if intr != 0.0 { intr } else { 4096.0 };
        let color_stops = match d.get("Clrs") {
            Some(DescriptorValue::List(items)) => items
                .iter()
                .map(|it| {
                    let s = match it {
                        DescriptorValue::Descriptor(s) => s,
                        _ => return Err(ReadError::StrictViolation("Invalid color stop".to_string())),
                    };
                    Ok(ColorStop {
                        color: parse_color(
                            s.get("Clr ")
                                .ok_or_else(|| ReadError::StrictViolation("Missing Clr".to_string()))?,
                        )?,
                        location: desc_double(s, "Lctn") / samples,
                        midpoint: desc_double(s, "Mdpn") / 100.0,
                    })
                })
                .collect::<ReadResult<Vec<_>>>()?,
            _ => Vec::new(),
        };
        let opacity_stops = match d.get("Trns") {
            Some(DescriptorValue::List(items)) => items
                .iter()
                .map(|it| {
                    let s = match it {
                        DescriptorValue::Descriptor(s) => s,
                        _ => return Err(ReadError::StrictViolation("Invalid opacity stop".to_string())),
                    };
                    Ok(OpacityStop {
                        opacity: s
                            .get("Opct")
                            .map(parse_percent)
                            .transpose()?
                            .unwrap_or(1.0),
                        location: desc_double(s, "Lctn") / samples,
                        midpoint: desc_double(s, "Mdpn") / 100.0,
                    })
                })
                .collect::<ReadResult<Vec<_>>>()?,
            _ => Vec::new(),
        };
        Ok(EffectGradient::Solid(EffectSolidGradient {
            name,
            smoothness: Some(intr / 4096.0),
            color_stops,
            opacity_stops,
        }))
    } else {
        let map_arr = |v: Option<&DescriptorValue>| -> Vec<f64> {
            match v {
                Some(DescriptorValue::List(items)) => items
                    .iter()
                    .map(|it| match it {
                        DescriptorValue::Double(x) => *x / 100.0,
                        DescriptorValue::Integer(x) => *x as f64 / 100.0,
                        _ => 0.0,
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        Ok(EffectGradient::Noise(EffectNoiseGradient {
            name,
            roughness: Some(desc_double(d, "Smth") / 4096.0),
            color_model: Some(match d.get("ClrS") {
                Some(DescriptorValue::Enum(s)) => clr_s_decode(s),
                _ => GradientColorModel::Rgb,
            }),
            random_seed: Some(desc_double(d, "RndS")),
            restrict_colors: Some(get_bool(d, "VctC")),
            add_transparency: Some(get_bool(d, "ShTr")),
            min: map_arr(d.get("Mnm ")),
            max: map_arr(d.get("Mxm ")),
        }))
    }
}

fn serialize_gradient(grad: &EffectGradient) -> DescriptorValue {
    match grad {
        EffectGradient::Solid(g) => {
            let samples = ((g.smoothness.unwrap_or(1.0)) * 4096.0).round();
            let mut d = Descriptor::new("", "Grdn");
            d.set("Nm  ", DescriptorValue::Text(g.name.clone()));
            d.set("GrdF", DescriptorValue::Enum("GrdF.CstS".to_string()));
            d.set("Intr", DescriptorValue::Double(samples));
            let clrs = g
                .color_stops
                .iter()
                .map(|s| {
                    let mut cs = Descriptor::new("", "Clrt");
                    cs.set("Clr ", serialize_color(Some(&s.color)));
                    cs.set("Type", DescriptorValue::Enum("Clry.UsrS".to_string()));
                    cs.set("Lctn", DescriptorValue::Integer((s.location * samples).round() as i32));
                    cs.set("Mdpn", DescriptorValue::Integer((s.midpoint * 100.0).round() as i32));
                    DescriptorValue::Descriptor(cs)
                })
                .collect();
            d.set("Clrs", DescriptorValue::List(clrs));
            let trns = g
                .opacity_stops
                .iter()
                .map(|s| {
                    let mut ts = Descriptor::new("", "TrnS");
                    ts.set("Opct", units_percent(s.opacity));
                    ts.set("Lctn", DescriptorValue::Integer((s.location * samples).round() as i32));
                    ts.set("Mdpn", DescriptorValue::Integer((s.midpoint * 100.0).round() as i32));
                    DescriptorValue::Descriptor(ts)
                })
                .collect();
            d.set("Trns", DescriptorValue::List(trns));
            DescriptorValue::Descriptor(d)
        }
        EffectGradient::Noise(g) => {
            let mut d = Descriptor::new("", "Grdn");
            d.set("GrdF", DescriptorValue::Enum("GrdF.ClNs".to_string()));
            d.set("Nm  ", DescriptorValue::Text(g.name.clone()));
            d.set("ShTr", DescriptorValue::Boolean(g.add_transparency.unwrap_or(false)));
            d.set("VctC", DescriptorValue::Boolean(g.restrict_colors.unwrap_or(false)));
            d.set("ClrS", DescriptorValue::Enum(clr_s_encode(g.color_model.unwrap_or(GradientColorModel::Rgb))));
            d.set("RndS", DescriptorValue::Double(g.random_seed.unwrap_or(0.0)));
            d.set("Smth", DescriptorValue::Double((g.roughness.unwrap_or(1.0) * 4096.0).round()));
            let min = if g.min.is_empty() { vec![0.0, 0.0, 0.0, 0.0] } else { g.min.clone() };
            let max = if g.max.is_empty() { vec![1.0, 1.0, 1.0, 1.0] } else { g.max.clone() };
            d.set(
                "Mnm ",
                DescriptorValue::List(min.iter().map(|x| DescriptorValue::Double(x * 100.0)).collect()),
            );
            d.set(
                "Mxm ",
                DescriptorValue::List(max.iter().map(|x| DescriptorValue::Double(x * 100.0)).collect()),
            );
            DescriptorValue::Descriptor(d)
        }
    }
}

// ===========================================================================
// Small accessors
// ===========================================================================

fn get_bool(d: &Descriptor, key: &str) -> bool {
    matches!(d.get(key), Some(DescriptorValue::Boolean(true)))
}

fn opt_bool(d: &Descriptor, key: &str) -> Option<bool> {
    match d.get(key) {
        Some(DescriptorValue::Boolean(b)) => Some(*b),
        _ => None,
    }
}

fn opt_text(d: &Descriptor, key: &str) -> Option<String> {
    match d.get(key) {
        Some(DescriptorValue::Text(s)) => Some(s.clone()),
        _ => None,
    }
}

fn pattern_from(d: &Descriptor) -> EffectPattern {
    let name = opt_text(d, "Nm  ").unwrap_or_default();
    let id = opt_text(d, "Idnt").unwrap_or_default();
    EffectPattern { name, id }
}

fn point_percent(d: &Descriptor) -> ReadResult<PointF> {
    let x = d.get("Hrzn").map(parse_percent).transpose()?.unwrap_or(1.0);
    let y = d.get("Vrtc").map(parse_percent).transpose()?.unwrap_or(1.0);
    Ok(PointF { x, y })
}

// ===========================================================================
// Per-effect parsers (descriptor -> struct); зеркала ветвей parseEffectObject
// ===========================================================================

fn parse_shadow(d: &Descriptor) -> ReadResult<LayerEffectShadow> {
    let mut s = LayerEffectShadow::default();
    s.enabled = opt_bool(d, "enab");
    s.use_global_light = opt_bool(d, "uglg");
    s.antialiased = opt_bool(d, "AntA");
    s.layer_conceals = opt_bool(d, "layerConceals");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(v) = d.get("Clr ") {
        s.color = Some(parse_color(v)?);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Md  ") {
        s.blend_mode = Some(bln_m_decode(v));
    }
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("lagl") {
        s.angle = Some(parse_angle(v)?);
    }
    if let Some(v) = d.get("blur") {
        s.size = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("Ckmt") {
        s.choke = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("Dstn") {
        s.distance = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("TrnS") {
        s.contour = Some(parse_contour(v));
    }
    Ok(s)
}

fn parse_outer_glow(d: &Descriptor) -> ReadResult<LayerEffectsOuterGlow> {
    let mut s = LayerEffectsOuterGlow::default();
    s.enabled = opt_bool(d, "enab");
    s.antialiased = opt_bool(d, "AntA");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(v) = d.get("Clr ") {
        s.color = Some(parse_color(v)?);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Md  ") {
        s.blend_mode = Some(bln_m_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("glwS") {
        s.source = Some(ig_sr_decode(v));
    }
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Nose") {
        s.noise = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Inpr") {
        s.range = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("ShdN") {
        s.jitter = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("blur") {
        s.size = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("Ckmt") {
        s.choke = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("TrnS") {
        s.contour = Some(parse_contour(v));
    }
    Ok(s)
}

fn parse_inner_glow(d: &Descriptor) -> ReadResult<LayerEffectInnerGlow> {
    let mut s = LayerEffectInnerGlow::default();
    s.enabled = opt_bool(d, "enab");
    s.antialiased = opt_bool(d, "AntA");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(v) = d.get("Clr ") {
        s.color = Some(parse_color(v)?);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Md  ") {
        s.blend_mode = Some(bln_m_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("glwS") {
        s.source = Some(ig_sr_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("GlwT") {
        s.technique = Some(be_te_decode(v));
    }
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Nose") {
        s.noise = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Inpr") {
        s.range = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("ShdN") {
        s.jitter = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("blur") {
        s.size = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("Ckmt") {
        s.choke = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("TrnS") {
        s.contour = Some(parse_contour(v));
    }
    Ok(s)
}

fn parse_bevel(d: &Descriptor) -> ReadResult<LayerEffectBevel> {
    let mut s = LayerEffectBevel::default();
    s.enabled = opt_bool(d, "enab");
    s.use_global_light = opt_bool(d, "uglg");
    s.antialias_gloss = opt_bool(d, "antialiasGloss");
    s.use_texture = opt_bool(d, "useTexture");
    s.use_shape = opt_bool(d, "useShape");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(v) = d.get("hglC") {
        s.highlight_color = Some(parse_color(v)?);
    }
    if let Some(v) = d.get("sdwC") {
        s.shadow_color = Some(parse_color(v)?);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("hglM") {
        s.highlight_blend_mode = Some(bln_m_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("sdwM") {
        s.shadow_blend_mode = Some(bln_m_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("bvlS") {
        s.style = Some(be_sl_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("bvlD") {
        s.direction = Some(be_ss_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("bvlT") {
        s.technique = Some(bvl_t_decode(v));
    }
    if let Some(v) = d.get("hglO") {
        s.highlight_opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("sdwO") {
        s.shadow_opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("lagl") {
        s.angle = Some(parse_angle(v)?);
    }
    if let Some(v) = d.get("Lald") {
        s.altitude = Some(parse_angle(v)?);
    }
    if let Some(v) = d.get("Sftn") {
        s.soften = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("srgR") {
        s.strength = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("blur") {
        s.size = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("TrnS") {
        s.contour = Some(parse_contour(v));
    }
    Ok(s)
}

fn parse_solid_fill(d: &Descriptor) -> ReadResult<LayerEffectSolidFill> {
    let mut s = LayerEffectSolidFill::default();
    s.enabled = opt_bool(d, "enab");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(v) = d.get("Clr ") {
        s.color = Some(parse_color(v)?);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Md  ") {
        s.blend_mode = Some(bln_m_decode(v));
    }
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    Ok(s)
}

fn parse_satin(d: &Descriptor) -> ReadResult<LayerEffectSatin> {
    let mut s = LayerEffectSatin::default();
    s.enabled = opt_bool(d, "enab");
    s.antialiased = opt_bool(d, "AntA");
    s.invert = opt_bool(d, "Invr");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(v) = d.get("Clr ") {
        s.color = Some(parse_color(v)?);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Md  ") {
        s.blend_mode = Some(bln_m_decode(v));
    }
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("lagl") {
        s.angle = Some(parse_angle(v)?);
    }
    if let Some(v) = d.get("blur") {
        s.size = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("Dstn") {
        s.distance = Some(parse_units(v)?);
    }
    if let Some(v) = d.get("MpgS") {
        s.contour = Some(parse_contour(v));
    }
    Ok(s)
}

fn parse_gradient_overlay(d: &Descriptor) -> ReadResult<LayerEffectGradientOverlay> {
    let mut s = LayerEffectGradientOverlay::default();
    s.enabled = opt_bool(d, "enab");
    s.dither = opt_bool(d, "Dthr");
    s.reverse = opt_bool(d, "Rvrs");
    s.align = opt_bool(d, "Algn");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Md  ") {
        // gradient overlay stores blend mode as String upstream.
        s.blend_mode = Some(blend_mode_string(bln_m_decode(v)));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Type") {
        s.gradient_type = Some(grd_t_decode(v));
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("gs99") {
        s.interpolation_method = Some(gs99_decode(v));
    }
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Scl ") {
        s.scale = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Angl") {
        s.angle = Some(parse_angle(v)?);
    }
    if let Some(DescriptorValue::Descriptor(p)) = d.get("Ofst") {
        s.offset = Some(point_percent(p)?);
    }
    if let Some(v) = d.get("Grad") {
        s.gradient = Some(parse_gradient(v)?);
    }
    Ok(s)
}

fn parse_pattern_overlay(d: &Descriptor) -> ReadResult<LayerEffectPatternOverlay> {
    let mut s = LayerEffectPatternOverlay::default();
    s.enabled = opt_bool(d, "enab");
    s.align = opt_bool(d, "Algn");
    if let Some(v) = d.get("present") {
        s.present = bool_of(v);
    }
    if let Some(v) = d.get("showInDialog") {
        s.show_in_dialog = bool_of(v);
    }
    if let Some(DescriptorValue::Enum(v)) = d.get("Md  ") {
        s.blend_mode = Some(bln_m_decode(v));
    }
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Scl ") {
        s.scale = Some(parse_percent(v)?);
    }
    if let Some(DescriptorValue::Descriptor(p)) = d.get("Ptrn") {
        s.pattern = Some(pattern_from(p));
    }
    if let Some(DescriptorValue::Descriptor(p)) = d.get("phase") {
        s.phase = Some(PointF { x: desc_double(p, "Hrzn"), y: desc_double(p, "Vrtc") });
    }
    Ok(s)
}

/// Зеркало parseFxObject (stroke / FrFX).
fn parse_fx_object(d: &Descriptor) -> ReadResult<LayerEffectStroke> {
    let mut s = LayerEffectStroke::default();
    s.enabled = Some(get_bool(d, "enab"));
    s.position = Some(match d.get("Styl") {
        Some(DescriptorValue::Enum(v)) => f_stl_decode(v),
        _ => StrokePosition::Outside,
    });
    s.fill_type = Some(match d.get("PntT") {
        Some(DescriptorValue::Enum(v)) => fr_fl_decode(v),
        _ => StrokeFillType::Color,
    });
    s.blend_mode = Some(match d.get("Md  ") {
        Some(DescriptorValue::Enum(v)) => bln_m_decode(v),
        _ => BlendMode::Normal,
    });
    if let Some(v) = d.get("Opct") {
        s.opacity = Some(parse_percent(v)?);
    }
    if let Some(v) = d.get("Sz  ") {
        s.size = Some(parse_units(v)?);
    }
    s.present = opt_bool(d, "present");
    s.show_in_dialog = opt_bool(d, "showInDialog");
    s.overprint = opt_bool(d, "overprint");
    if let Some(v) = d.get("Clr ") {
        s.color = Some(parse_color(v)?);
    }
    if let Some(v) = d.get("Grad") {
        s.gradient = Some(parse_gradient_content(d, v)?);
    }
    if let Some(DescriptorValue::Descriptor(p)) = d.get("Ptrn") {
        s.pattern = Some(pattern_from(p));
    }
    Ok(s)
}

/// Зеркало parseGradientContent (для stroke.gradient).
fn parse_gradient_content(d: &Descriptor, grad: &DescriptorValue) -> ReadResult<GradientWithExtra> {
    let gradient = parse_gradient(grad)?;
    let mut extra = ExtraGradientInfo::default();
    if let Some(DescriptorValue::Enum(v)) = d.get("Type") {
        extra.style = Some(grd_t_decode(v));
    }
    extra.dither = opt_bool(d, "Dthr");
    if let Some(DescriptorValue::Enum(v)) = d.get("gradientsInterpolationMethod") {
        extra.interpolation_method = Some(gs99_decode(v));
    }
    extra.reverse = opt_bool(d, "Rvrs");
    if let Some(v) = d.get("Angl") {
        extra.angle = Some(parse_angle(v)?);
    }
    if let Some(v) = d.get("Scl ") {
        extra.scale = Some(parse_percent(v)?);
    }
    extra.align = opt_bool(d, "Algn");
    if let Some(DescriptorValue::Descriptor(p)) = d.get("Ofst") {
        extra.offset = Some(point_percent(p)?);
    }
    Ok(GradientWithExtra { gradient, extra })
}

fn bool_of(v: &DescriptorValue) -> Option<bool> {
    match v {
        DescriptorValue::Boolean(b) => Some(*b),
        _ => None,
    }
}

// ===========================================================================
// Per-effect serializers (struct -> descriptor); зеркала serializeEffectObject
// ===========================================================================

fn ser_shadow(s: &LayerEffectShadow, is_drop: bool) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if is_drop {
        d.set("TrnS", empty_contour());
    }
    if let Some(v) = s.use_global_light {
        d.set("uglg", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.antialiased {
        d.set("AntA", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.color.as_ref() {
        d.set("Clr ", serialize_color(Some(v)));
    }
    if let Some(v) = s.blend_mode {
        d.set("Md  ", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.opacity {
        d.set("Opct", units_percent(v));
    }
    if let Some(v) = s.angle {
        d.set("lagl", units_angle(v));
    }
    if let Some(v) = s.size {
        d.set("blur", units_value(Some(v)));
    }
    if let Some(v) = s.choke {
        d.set("Ckmt", units_value(Some(v)));
    }
    if let Some(v) = s.distance {
        d.set("Dstn", units_value(Some(v)));
    }
    if let Some(v) = s.layer_conceals {
        d.set("layerConceals", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    if let Some(c) = s.contour.as_ref() {
        d.set("TrnS", serialize_contour(c));
    }
    d
}

fn ser_outer_glow(s: &LayerEffectsOuterGlow) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.antialiased {
        d.set("AntA", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.color.as_ref() {
        d.set("Clr ", serialize_color(Some(v)));
    }
    if let Some(v) = s.blend_mode {
        d.set("Md  ", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.source {
        d.set("glwS", DescriptorValue::Enum(ig_sr_encode(v)));
    }
    if let Some(v) = s.opacity {
        d.set("Opct", units_percent(v));
    }
    if let Some(v) = s.noise {
        d.set("Nose", units_percent(v));
    }
    if let Some(v) = s.range {
        d.set("Inpr", units_percent(v));
    }
    if let Some(v) = s.jitter {
        d.set("ShdN", units_percent(v));
    }
    if let Some(v) = s.size {
        d.set("blur", units_value(Some(v)));
    }
    if let Some(v) = s.choke {
        d.set("Ckmt", units_value(Some(v)));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    if let Some(c) = s.contour.as_ref() {
        d.set("TrnS", serialize_contour(c));
    }
    d
}

fn ser_inner_glow(s: &LayerEffectInnerGlow) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.antialiased {
        d.set("AntA", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.color.as_ref() {
        d.set("Clr ", serialize_color(Some(v)));
    }
    if let Some(v) = s.blend_mode {
        d.set("Md  ", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.source {
        d.set("glwS", DescriptorValue::Enum(ig_sr_encode(v)));
    }
    if let Some(v) = s.technique {
        d.set("GlwT", DescriptorValue::Enum(be_te_encode(v)));
    }
    if let Some(v) = s.opacity {
        d.set("Opct", units_percent(v));
    }
    if let Some(v) = s.noise {
        d.set("Nose", units_percent(v));
    }
    if let Some(v) = s.range {
        d.set("Inpr", units_percent(v));
    }
    if let Some(v) = s.jitter {
        d.set("ShdN", units_percent(v));
    }
    if let Some(v) = s.size {
        d.set("blur", units_value(Some(v)));
    }
    if let Some(v) = s.choke {
        d.set("Ckmt", units_value(Some(v)));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    if let Some(c) = s.contour.as_ref() {
        d.set("TrnS", serialize_contour(c));
    }
    d
}

fn ser_bevel(s: &LayerEffectBevel) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.use_global_light {
        d.set("uglg", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.highlight_color.as_ref() {
        d.set("hglC", serialize_color(Some(v)));
    }
    if let Some(v) = s.shadow_color.as_ref() {
        d.set("sdwC", serialize_color(Some(v)));
    }
    if let Some(v) = s.highlight_blend_mode {
        d.set("hglM", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.shadow_blend_mode {
        d.set("sdwM", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.style {
        d.set("bvlS", DescriptorValue::Enum(be_sl_encode(v)));
    }
    if let Some(v) = s.direction {
        d.set("bvlD", DescriptorValue::Enum(be_ss_encode(v)));
    }
    if let Some(v) = s.technique {
        d.set("bvlT", DescriptorValue::Enum(bvl_t_encode(v)));
    }
    if let Some(v) = s.highlight_opacity {
        d.set("hglO", units_percent(v));
    }
    if let Some(v) = s.shadow_opacity {
        d.set("sdwO", units_percent(v));
    }
    if let Some(v) = s.angle {
        d.set("lagl", units_angle(v));
    }
    if let Some(v) = s.altitude {
        d.set("Lald", units_angle(v));
    }
    if let Some(v) = s.soften {
        d.set("Sftn", units_value(Some(v)));
    }
    if let Some(v) = s.strength {
        d.set("srgR", units_percent(v));
    }
    if let Some(v) = s.size {
        d.set("blur", units_value(Some(v)));
    }
    if let Some(v) = s.use_texture {
        d.set("useTexture", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.use_shape {
        d.set("useShape", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.antialias_gloss {
        d.set("antialiasGloss", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    if let Some(c) = s.contour.as_ref() {
        d.set("TrnS", serialize_contour(c));
    }
    d
}

fn ser_solid_fill(s: &LayerEffectSolidFill) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.color.as_ref() {
        d.set("Clr ", serialize_color(Some(v)));
    }
    if let Some(v) = s.blend_mode {
        d.set("Md  ", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.opacity {
        d.set("Opct", units_percent(v));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    d
}

fn ser_satin(s: &LayerEffectSatin) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.antialiased {
        d.set("AntA", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.invert {
        d.set("Invr", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.color.as_ref() {
        d.set("Clr ", serialize_color(Some(v)));
    }
    if let Some(v) = s.blend_mode {
        d.set("Md  ", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.opacity {
        d.set("Opct", units_percent(v));
    }
    if let Some(v) = s.angle {
        d.set("lagl", units_angle(v));
    }
    if let Some(v) = s.size {
        d.set("blur", units_value(Some(v)));
    }
    if let Some(v) = s.distance {
        d.set("Dstn", units_value(Some(v)));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    if let Some(c) = s.contour.as_ref() {
        d.set("MpgS", serialize_contour(c));
    }
    d
}

fn ser_gradient_overlay(s: &LayerEffectGradientOverlay) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.dither {
        d.set("Dthr", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.reverse {
        d.set("Rvrs", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.blend_mode.as_ref() {
        d.set("Md  ", DescriptorValue::Enum(bln_m_encode(blend_mode_from_string(v))));
    }
    if let Some(v) = s.gradient_type {
        d.set("Type", DescriptorValue::Enum(grd_t_encode(v)));
    }
    if let Some(v) = s.interpolation_method {
        d.set("gs99", DescriptorValue::Enum(gs99_encode(v)));
    }
    if let Some(v) = s.opacity {
        d.set("Opct", units_percent(v));
    }
    // gradientOverlay angle uses Angl (объект gradientOverlay в serializeEffectObject).
    if let Some(v) = s.angle {
        d.set("Angl", units_angle(v));
    }
    if let Some(v) = s.scale {
        d.set("Scl ", units_percent(v));
    }
    if let Some(v) = s.align {
        d.set("Algn", DescriptorValue::Boolean(v));
    }
    if let Some(p) = s.offset.as_ref() {
        let mut off = Descriptor::new("", "Pnt ");
        off.set("Hrzn", units_percent(p.x));
        off.set("Vrtc", units_percent(p.y));
        d.set("Ofst", DescriptorValue::Descriptor(off));
    }
    if let Some(g) = s.gradient.as_ref() {
        d.set("Grad", serialize_gradient(g));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    d
}

fn ser_pattern_overlay(s: &LayerEffectPatternOverlay) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.blend_mode {
        d.set("Md  ", DescriptorValue::Enum(bln_m_encode(v)));
    }
    if let Some(v) = s.opacity {
        d.set("Opct", units_percent(v));
    }
    if let Some(v) = s.scale {
        // patternFill angle path uses Angl in serializeEffectObject, but pattern
        // overlay has no angle field; scale uses Scl '.
        d.set("Scl ", units_percent(v));
    }
    if let Some(p) = s.pattern.as_ref() {
        let mut pat = Descriptor::new("", "Ptrn");
        pat.set("Nm  ", DescriptorValue::Text(p.name.clone()));
        pat.set("Idnt", DescriptorValue::Text(p.id.clone()));
        d.set("Ptrn", DescriptorValue::Descriptor(pat));
    }
    if let Some(p) = s.phase.as_ref() {
        let mut ph = Descriptor::new("", "Pnt ");
        ph.set("Hrzn", DescriptorValue::Double(p.x));
        ph.set("Vrtc", DescriptorValue::Double(p.y));
        d.set("phase", DescriptorValue::Descriptor(ph));
    }
    if let Some(v) = s.align {
        d.set("Algn", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    d
}

/// Зеркало serializeFxObject (stroke / FrFX).
fn ser_fx_object(s: &LayerEffectStroke) -> Descriptor {
    let mut d = Descriptor::new("", "null");
    d.set("enab", DescriptorValue::Boolean(s.enabled.unwrap_or(false)));
    if let Some(v) = s.present {
        d.set("present", DescriptorValue::Boolean(v));
    }
    if let Some(v) = s.show_in_dialog {
        d.set("showInDialog", DescriptorValue::Boolean(v));
    }
    d.set("Styl", DescriptorValue::Enum(f_stl_encode(s.position.unwrap_or(StrokePosition::Outside))));
    d.set("PntT", DescriptorValue::Enum(fr_fl_encode(s.fill_type.unwrap_or(StrokeFillType::Color))));
    d.set("Md  ", DescriptorValue::Enum(bln_m_encode(s.blend_mode.unwrap_or(BlendMode::Normal))));
    d.set("Opct", units_percent(s.opacity.unwrap_or(0.0)));
    d.set("Sz  ", units_value(s.size));
    if let Some(v) = s.color.as_ref() {
        d.set("Clr ", serialize_color(Some(v)));
    }
    if let Some(g) = s.gradient.as_ref() {
        // spread serializeGradientContent fields onto FrFX.
        ser_gradient_content_into(&mut d, g);
    }
    if let Some(p) = s.pattern.as_ref() {
        ser_pattern_content_into(&mut d, p, None, None);
    }
    if let Some(v) = s.overprint {
        d.set("overprint", DescriptorValue::Boolean(v));
    }
    d
}

/// Зеркало serializeGradientContent (раскладывается в FrFX).
fn ser_gradient_content_into(d: &mut Descriptor, g: &GradientWithExtra) {
    let e = &g.extra;
    if let Some(v) = e.dither {
        d.set("Dthr", DescriptorValue::Boolean(v));
    }
    if let Some(v) = e.interpolation_method {
        d.set("gradientsInterpolationMethod", DescriptorValue::Enum(gs99_encode(v)));
    }
    if let Some(v) = e.reverse {
        d.set("Rvrs", DescriptorValue::Boolean(v));
    }
    if let Some(v) = e.angle {
        d.set("Angl", units_angle(v));
    }
    d.set("Type", DescriptorValue::Enum(grd_t_encode(e.style.unwrap_or(GradientStyle::Linear))));
    if let Some(v) = e.align {
        d.set("Algn", DescriptorValue::Boolean(v));
    }
    if let Some(v) = e.scale {
        d.set("Scl ", units_percent(v));
    }
    if let Some(p) = e.offset.as_ref() {
        let mut off = Descriptor::new("", "Pnt ");
        off.set("Hrzn", units_percent(p.x));
        off.set("Vrtc", units_percent(p.y));
        d.set("Ofst", DescriptorValue::Descriptor(off));
    }
    d.set("Grad", serialize_gradient(&g.gradient));
}

/// Зеркало serializePatternContent (раскладывается в FrFX).
fn ser_pattern_content_into(
    d: &mut Descriptor,
    p: &EffectPattern,
    linked: Option<bool>,
    phase: Option<PointF>,
) {
    let mut pat = Descriptor::new("", "Ptrn");
    pat.set("Nm  ", DescriptorValue::Text(p.name.clone()));
    pat.set("Idnt", DescriptorValue::Text(p.id.clone()));
    d.set("Ptrn", DescriptorValue::Descriptor(pat));
    if let Some(v) = linked {
        d.set("Lnkd", DescriptorValue::Boolean(v));
    }
    if let Some(p) = phase {
        let mut ph = Descriptor::new("", "Pnt ");
        ph.set("Hrzn", DescriptorValue::Double(p.x));
        ph.set("Vrtc", DescriptorValue::Double(p.y));
        d.set("phase", DescriptorValue::Descriptor(ph));
    }
}

// ===========================================================================
// BlendMode <-> String (gradient overlay stores blend mode as String)
// ===========================================================================

fn blend_mode_string(m: BlendMode) -> String {
    blend_mode_to_str(m).to_string()
}

fn blend_mode_from_string(s: &str) -> BlendMode {
    BLEND_MODE_STR
        .iter()
        .find(|(name, _)| *name == s)
        .map(|(_, m)| *m)
        .unwrap_or(BlendMode::Normal)
}

fn blend_mode_to_str(m: BlendMode) -> &'static str {
    BLEND_MODE_STR
        .iter()
        .find(|(_, v)| *v == m)
        .map(|(name, _)| *name)
        .unwrap_or("normal")
}

const BLEND_MODE_STR: &[(&str, BlendMode)] = &[
    ("pass through", BlendMode::PassThrough),
    ("normal", BlendMode::Normal),
    ("dissolve", BlendMode::Dissolve),
    ("darken", BlendMode::Darken),
    ("multiply", BlendMode::Multiply),
    ("color burn", BlendMode::ColorBurn),
    ("linear burn", BlendMode::LinearBurn),
    ("darker color", BlendMode::DarkerColor),
    ("lighten", BlendMode::Lighten),
    ("screen", BlendMode::Screen),
    ("color dodge", BlendMode::ColorDodge),
    ("linear dodge", BlendMode::LinearDodge),
    ("lighter color", BlendMode::LighterColor),
    ("overlay", BlendMode::Overlay),
    ("soft light", BlendMode::SoftLight),
    ("hard light", BlendMode::HardLight),
    ("vivid light", BlendMode::VividLight),
    ("linear light", BlendMode::LinearLight),
    ("pin light", BlendMode::PinLight),
    ("hard mix", BlendMode::HardMix),
    ("difference", BlendMode::Difference),
    ("exclusion", BlendMode::Exclusion),
    ("subtract", BlendMode::Subtract),
    ("divide", BlendMode::Divide),
    ("hue", BlendMode::Hue),
    ("saturation", BlendMode::Saturation),
    ("color", BlendMode::Color),
    ("luminosity", BlendMode::Luminosity),
];

// ===========================================================================
// parseEffects / serializeEffects
// ===========================================================================

/// Зеркало `parseEffects(info, log)`.
fn parse_effects(info: &Descriptor) -> ReadResult<LayerEffectsInfo> {
    let mut e = LayerEffectsInfo::default();

    // masterFXSwitch: if (!masterFXSwitch) effects.disabled = true;
    let master = get_bool(info, "masterFXSwitch");
    if !master {
        e.disabled = Some(true);
    }
    if let Some(v) = info.get("Scl ") {
        e.scale = Some(parse_percent(v)?);
    }

    let get_desc = |key: &str| -> Option<&Descriptor> {
        match info.get(key) {
            Some(DescriptorValue::Descriptor(d)) => Some(d),
            _ => None,
        }
    };
    let get_list = |key: &str| -> Option<&Vec<DescriptorValue>> {
        match info.get(key) {
            Some(DescriptorValue::List(l)) => Some(l),
            _ => None,
        }
    };

    if let Some(d) = get_desc("DrSh") {
        e.drop_shadow = Some(vec![parse_shadow(d)?]);
    }
    if let Some(l) = get_list("dropShadowMulti") {
        e.drop_shadow = Some(map_desc_list(l, parse_shadow)?);
    }
    if let Some(d) = get_desc("IrSh") {
        e.inner_shadow = Some(vec![parse_shadow(d)?]);
    }
    if let Some(l) = get_list("innerShadowMulti") {
        e.inner_shadow = Some(map_desc_list(l, parse_shadow)?);
    }
    if let Some(d) = get_desc("OrGl") {
        e.outer_glow = Some(parse_outer_glow(d)?);
    }
    if let Some(d) = get_desc("IrGl") {
        e.inner_glow = Some(parse_inner_glow(d)?);
    }
    if let Some(d) = get_desc("ebbl") {
        e.bevel = Some(parse_bevel(d)?);
    }
    if let Some(d) = get_desc("SoFi") {
        e.solid_fill = Some(vec![parse_solid_fill(d)?]);
    }
    if let Some(l) = get_list("solidFillMulti") {
        e.solid_fill = Some(map_desc_list(l, parse_solid_fill)?);
    }
    if let Some(d) = get_desc("patternFill") {
        e.pattern_overlay = Some(parse_pattern_overlay(d)?);
    }
    if let Some(d) = get_desc("GrFl") {
        e.gradient_overlay = Some(vec![parse_gradient_overlay(d)?]);
    }
    if let Some(l) = get_list("gradientFillMulti") {
        e.gradient_overlay = Some(map_desc_list(l, parse_gradient_overlay)?);
    }
    if let Some(d) = get_desc("ChFX") {
        e.satin = Some(parse_satin(d)?);
    }
    if let Some(d) = get_desc("FrFX") {
        e.stroke = Some(vec![parse_fx_object(d)?]);
    }
    if let Some(l) = get_list("frameFXMulti") {
        e.stroke = Some(map_desc_list(l, parse_fx_object)?);
    }

    Ok(e)
}

fn map_desc_list<T>(
    list: &[DescriptorValue],
    f: impl Fn(&Descriptor) -> ReadResult<T>,
) -> ReadResult<Vec<T>> {
    list.iter()
        .map(|it| match it {
            DescriptorValue::Descriptor(d) => f(d),
            _ => Err(ReadError::StrictViolation("Expected descriptor in list".to_string())),
        })
        .collect()
}

/// Зеркало `serializeEffects(e, log, multi=true)`.
fn serialize_effects(e: &LayerEffectsInfo) -> ReadResult<Descriptor> {
    // multi == true: { 'Scl ', masterFXSwitch }.
    let mut info = Descriptor::new("", "null");
    info.set("Scl ", units_percent_f(e.scale.unwrap_or(1.0)));
    info.set("masterFXSwitch", DescriptorValue::Boolean(!e.disabled.unwrap_or(false)));

    fn use_multi<T>(arr: &Option<Vec<T>>) -> bool {
        arr.as_ref().map(|a| a.len() > 1).unwrap_or(false)
    }
    fn use_single<T>(arr: &Option<Vec<T>>) -> bool {
        arr.as_ref().map(|a| !a.is_empty()).unwrap_or(false)
    }

    // dropShadow
    if use_single(&e.drop_shadow) && !use_multi(&e.drop_shadow) {
        let s = &e.drop_shadow.as_ref().unwrap()[0];
        info.set("DrSh", DescriptorValue::Descriptor(ser_shadow(s, true)));
    }
    if use_multi(&e.drop_shadow) {
        let list = e
            .drop_shadow
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| DescriptorValue::Descriptor(ser_shadow(s, true)))
            .collect();
        info.set("dropShadowMulti", DescriptorValue::List(list));
    }
    // innerShadow
    if use_single(&e.inner_shadow) && !use_multi(&e.inner_shadow) {
        let s = &e.inner_shadow.as_ref().unwrap()[0];
        info.set("IrSh", DescriptorValue::Descriptor(ser_shadow(s, false)));
    }
    if use_multi(&e.inner_shadow) {
        let list = e
            .inner_shadow
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| DescriptorValue::Descriptor(ser_shadow(s, false)))
            .collect();
        info.set("innerShadowMulti", DescriptorValue::List(list));
    }
    // outerGlow
    if let Some(s) = e.outer_glow.as_ref() {
        info.set("OrGl", DescriptorValue::Descriptor(ser_outer_glow(s)));
    }
    // solidFill multi
    if use_multi(&e.solid_fill) {
        let list = e
            .solid_fill
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| DescriptorValue::Descriptor(ser_solid_fill(s)))
            .collect();
        info.set("solidFillMulti", DescriptorValue::List(list));
    }
    // gradientOverlay multi
    if use_multi(&e.gradient_overlay) {
        let list = e
            .gradient_overlay
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| DescriptorValue::Descriptor(ser_gradient_overlay(s)))
            .collect();
        info.set("gradientFillMulti", DescriptorValue::List(list));
    }
    // stroke multi
    if use_multi(&e.stroke) {
        let list = e
            .stroke
            .as_ref()
            .unwrap()
            .iter()
            .map(|s| DescriptorValue::Descriptor(ser_fx_object(s)))
            .collect();
        info.set("frameFXMulti", DescriptorValue::List(list));
    }
    // innerGlow
    if let Some(s) = e.inner_glow.as_ref() {
        info.set("IrGl", DescriptorValue::Descriptor(ser_inner_glow(s)));
    }
    // bevel
    if let Some(s) = e.bevel.as_ref() {
        info.set("ebbl", DescriptorValue::Descriptor(ser_bevel(s)));
    }
    // solidFill single
    if use_single(&e.solid_fill) && !use_multi(&e.solid_fill) {
        let s = &e.solid_fill.as_ref().unwrap()[0];
        info.set("SoFi", DescriptorValue::Descriptor(ser_solid_fill(s)));
    }
    // patternOverlay
    if let Some(s) = e.pattern_overlay.as_ref() {
        info.set("patternFill", DescriptorValue::Descriptor(ser_pattern_overlay(s)));
    }
    // gradientOverlay single
    if use_single(&e.gradient_overlay) && !use_multi(&e.gradient_overlay) {
        let s = &e.gradient_overlay.as_ref().unwrap()[0];
        info.set("GrFl", DescriptorValue::Descriptor(ser_gradient_overlay(s)));
    }
    // satin
    if let Some(s) = e.satin.as_ref() {
        info.set("ChFX", DescriptorValue::Descriptor(ser_satin(s)));
    }
    // stroke single
    if use_single(&e.stroke) && !use_multi(&e.stroke) {
        let s = &e.stroke.as_ref().unwrap()[0];
        info.set("FrFX", DescriptorValue::Descriptor(ser_fx_object(s)));
    }

    // multi == true: numModifyingFX = count of enabled effects.
    let mut num: i32 = 0;
    num += count_enabled_vec(&e.drop_shadow, |s: &LayerEffectShadow| s.enabled.unwrap_or(false));
    num += count_enabled_vec(&e.inner_shadow, |s: &LayerEffectShadow| s.enabled.unwrap_or(false));
    num += count_enabled_vec(&e.solid_fill, |s: &LayerEffectSolidFill| s.enabled.unwrap_or(false));
    num += count_enabled_vec(&e.stroke, |s: &LayerEffectStroke| s.enabled.unwrap_or(false));
    num += count_enabled_vec(&e.gradient_overlay, |s: &LayerEffectGradientOverlay| {
        s.enabled.unwrap_or(false)
    });
    if e.outer_glow.as_ref().map(|s| s.enabled.unwrap_or(false)).unwrap_or(false) {
        num += 1;
    }
    if e.inner_glow.as_ref().map(|s| s.enabled.unwrap_or(false)).unwrap_or(false) {
        num += 1;
    }
    if e.bevel.as_ref().map(|s| s.enabled.unwrap_or(false)).unwrap_or(false) {
        num += 1;
    }
    if e.satin.as_ref().map(|s| s.enabled.unwrap_or(false)).unwrap_or(false) {
        num += 1;
    }
    if e.pattern_overlay.as_ref().map(|s| s.enabled.unwrap_or(false)).unwrap_or(false) {
        num += 1;
    }
    info.set("numModifyingFX", DescriptorValue::Integer(num));

    Ok(info)
}

fn count_enabled_vec<T>(v: &Option<Vec<T>>, enabled: impl Fn(&T) -> bool) -> i32 {
    v.as_ref()
        .map(|a| a.iter().filter(|x| enabled(x)).count() as i32)
        .unwrap_or(0)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::{Rgb, Units};
    use crate::reader::PsdReader;
    use crate::writer::{create_writer_default, get_writer_buffer};

    fn rgb(r: f64, g: f64, b: f64) -> Color {
        Color::Rgb(Rgb { r, g, b })
    }

    fn px(v: f64) -> UnitsValue {
        UnitsValue { units: Units::Pixels, value: v }
    }

    fn round_trip_descriptor(e: &LayerEffectsInfo) -> LayerEffectsInfo {
        let desc = serialize_effects(e).expect("serialize");
        let mut w = create_writer_default();
        write_uint32(&mut w, 0);
        write_version_and_descriptor(&mut w, &desc);
        let buf = get_writer_buffer(&w);
        let mut r = PsdReader::new(&buf, None, None);
        let version = read_uint32(&mut r).unwrap();
        assert_eq!(version, 0);
        let read_desc = read_version_and_descriptor(&mut r).expect("read desc");
        parse_effects(&read_desc).expect("parse")
    }

    #[test]
    fn lfx2_drop_shadow_and_solid_fill_round_trip() {
        let e = LayerEffectsInfo {
            scale: Some(1.0),
            drop_shadow: Some(vec![LayerEffectShadow {
                enabled: Some(true),
                use_global_light: Some(true),
                size: Some(px(5.0)),
                distance: Some(px(3.0)),
                angle: Some(120.0),
                color: Some(rgb(10.0, 20.0, 30.0)),
                blend_mode: Some(BlendMode::Multiply),
                opacity: Some(0.5),
                ..LayerEffectShadow::default()
            }]),
            solid_fill: Some(vec![LayerEffectSolidFill {
                enabled: Some(true),
                blend_mode: Some(BlendMode::Normal),
                color: Some(rgb(255.0, 128.0, 0.0)),
                opacity: Some(1.0),
                ..LayerEffectSolidFill::default()
            }]),
            ..LayerEffectsInfo::default()
        };

        let out = round_trip_descriptor(&e);

        // disabled should remain unset (masterFXSwitch true).
        assert_eq!(out.disabled, None);
        assert!((out.scale.unwrap() - 1.0).abs() < 1e-9);

        let ds = &out.drop_shadow.as_ref().unwrap()[0];
        assert_eq!(ds.enabled, Some(true));
        assert_eq!(ds.use_global_light, Some(true));
        assert_eq!(ds.size.unwrap().value, 5.0);
        assert_eq!(ds.distance.unwrap().value, 3.0);
        assert_eq!(ds.angle.unwrap(), 120.0);
        assert_eq!(ds.blend_mode, Some(BlendMode::Multiply));
        // opacity 0.5 -> units_percent rounds 50 -> 0.5
        assert!((ds.opacity.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(ds.color, Some(rgb(10.0, 20.0, 30.0)));
        // default empty contour written for drop shadow.
        assert!(ds.contour.is_some());

        let sf = &out.solid_fill.as_ref().unwrap()[0];
        assert_eq!(sf.enabled, Some(true));
        assert_eq!(sf.blend_mode, Some(BlendMode::Normal));
        assert_eq!(sf.color, Some(rgb(255.0, 128.0, 0.0)));
        assert!((sf.opacity.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn lfx2_disabled_and_outer_glow() {
        let e = LayerEffectsInfo {
            disabled: Some(true),
            outer_glow: Some(LayerEffectsOuterGlow {
                enabled: Some(true),
                size: Some(px(7.0)),
                color: Some(rgb(1.0, 2.0, 3.0)),
                blend_mode: Some(BlendMode::Screen),
                opacity: Some(0.75),
                source: Some(GlowSource::Edge),
                ..LayerEffectsOuterGlow::default()
            }),
            ..LayerEffectsInfo::default()
        };

        let out = round_trip_descriptor(&e);
        assert_eq!(out.disabled, Some(true));
        let og = out.outer_glow.as_ref().unwrap();
        assert_eq!(og.enabled, Some(true));
        assert_eq!(og.size.unwrap().value, 7.0);
        assert_eq!(og.blend_mode, Some(BlendMode::Screen));
        assert!((og.opacity.unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(og.source, Some(GlowSource::Edge));
        assert_eq!(og.color, Some(rgb(1.0, 2.0, 3.0)));
    }

    #[test]
    fn lr_fx_legacy_round_trip() {
        // lrFX delegates to effects_helpers::{write_effects, read_effects}.
        let mut info = LayerAdditionalInfo::default();
        info.effects = Some(LayerEffectsInfo {
            drop_shadow: Some(vec![LayerEffectShadow {
                enabled: Some(true),
                use_global_light: Some(true),
                size: Some(px(5.0)),
                distance: Some(px(3.0)),
                angle: Some(90.0),
                color: Some(rgb(10.0, 20.0, 30.0)),
                blend_mode: Some(BlendMode::Multiply),
                opacity: Some(0.5),
                ..LayerEffectShadow::default()
            }]),
            ..LayerEffectsInfo::default()
        });

        // has(lrFX) should be true.
        assert_eq!(has("lrFX", &info), Some(true));

        let mut w = create_writer_default();
        let mut wctx = WriteCtx::new(default_write_options(), false);
        let res = write("lrFX", &mut w, &info, &mut wctx);
        assert!(matches!(res, Some(Ok(()))));
        let buf = get_writer_buffer(&w);

        let mut r = PsdReader::new(&buf, None, None);
        let mut out = LayerAdditionalInfo::default();
        let len = buf.len();
        let left = move |reader: &PsdReader| len - reader.offset;
        let opts = default_read_options();
        let mut rctx = ReadCtx { options: &opts, large: false };
        let handled = read("lrFX", &mut r, &mut out, &left, &mut rctx).expect("read");
        assert!(handled.is_some());

        let ds = &out.effects.as_ref().unwrap().drop_shadow.as_ref().unwrap()[0];
        assert_eq!(ds.size.unwrap().value, 5.0);
        assert_eq!(ds.angle.unwrap(), 90.0);
        assert_eq!(ds.blend_mode, Some(BlendMode::Multiply));
        assert_eq!(ds.enabled, Some(true));
    }

    #[test]
    fn has_multi_effects_detection() {
        let mut e = LayerEffectsInfo::default();
        e.drop_shadow = Some(vec![LayerEffectShadow::default()]);
        assert!(!has_multi_effects(&e));
        e.drop_shadow = Some(vec![LayerEffectShadow::default(), LayerEffectShadow::default()]);
        assert!(has_multi_effects(&e));
    }

    fn default_write_options() -> &'static crate::psd::WriteOptions {
        use std::sync::OnceLock;
        static OPTS: OnceLock<crate::psd::WriteOptions> = OnceLock::new();
        OPTS.get_or_init(crate::psd::WriteOptions::default)
    }

    fn default_read_options() -> crate::psd::ReadOptions {
        crate::psd::ReadOptions::default()
    }
}
