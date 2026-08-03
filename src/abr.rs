/*
File: crates/ag-psd/src/abr.rs

Purpose:
чтение кистей Photoshop (.abr).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/abr.ts`. Upstream READ-ONLY (нет `writeAbr`),
  поэтому здесь портирован только `read_abr`.

Dependency gaps (портированы локально здесь, должны переехать в свои модули):
- `read_data_rle` (8-бит, uint16-длины) — из `psdReader.ts`; локальная копия нужна
  потому, что ABR-`samp` пишет в заимствованный `&mut [u8]`, а `reader::read_data_rle`
  работает с владеющим `DecodeTarget`. `read_pattern` — НЕ копия: ABR-секция `patt`
  вызывает единственную реализацию `crate::reader::read_pattern` (ридер, созданный
  `PsdReader::new`, не несёт бюджета памяти — как upstream `createReader`).
- descriptor-хелперы `parsePercent` / `parseAngle` / `parseUnitsToNumber` и enum
  `BlnM` (descriptor.ts) — здесь как `parse_percent` / `parse_angle` /
  `parse_units_to_number` / `blnm_decode`.
- `crate::descriptor::read_version_and_descriptor` всегда читает class id
  дескриптора (эквивалент upstream `includeClass = true`), поэтому отдельного
  флага не требуется.

Main responsibilities:
- разбор ABR версий 6/7/9/10 (minor 1/2): секции '8BIM' samp/desc/patt/phry.
*/

use crate::additional_info::effects_keys::bln_m_decode;
use crate::descriptor::{Descriptor, DescriptorValue, UnitDoubleValue};
use crate::psd::{BlendMode, PatternInfo};
use crate::reader::{
    check_signature, read_bytes, read_int16, read_int32, read_pascal_string, read_pattern,
    read_signature, read_uint16, read_uint32, read_uint8, skip_bytes, PsdReader, ReadError,
    ReadResult,
};

// ===========================================================================
// Data model (зеркало интерфейсов abr.ts)
// ===========================================================================

/// TS `Abr`.
#[derive(Debug, Clone, Default)]
pub struct Abr {
    pub brushes: Vec<Brush>,
    pub samples: Vec<SampleInfo>,
    pub patterns: Vec<PatternInfo>,
}

/// TS `SampleInfo`.
#[derive(Debug, Clone)]
pub struct SampleInfo {
    pub id: String,
    pub bounds: SampleBounds,
    pub alpha: Vec<u8>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SampleBounds {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// TS `BrushDynamics`.
#[derive(Debug, Clone)]
pub struct BrushDynamics {
    /// control: 'off' | 'fade' | 'pen pressure' | ...
    pub control: String,
    pub steps: f64,
    pub jitter: f64,
    pub minimum: f64,
}

const DYNAMICS_CONTROL: &[&str] = &[
    "off",
    "fade",
    "pen pressure",
    "pen tilt",
    "stylus wheel",
    "initial direction",
    "direction",
    "initial rotation",
    "rotation",
];

const DYNAMIC_BRUSH_SHAPE_SHAPES: &[&str] = &[
    "round point",
    "round blunt",
    "round curve",
    "round angle",
    "round fan",
    "flat point",
    "flat blunt",
    "flat curve",
    "flat angle",
    "flat fan",
];

const TIPS_BRUSH_SHAPE_SHAPES: &[&str] = &[
    "erodible point",
    "erodible flat",
    "erodible round",
    "erodible square",
    "erodible triangle",
    "custom",
];

/// TS `BrushShape` union.
#[derive(Debug, Clone)]
pub enum BrushShape {
    Computed {
        size: f64,
        angle: f64,
        roundness: f64,
        hardness: f64,
        spacing_on: bool,
        spacing: f64,
        flip_x: bool,
        flip_y: bool,
    },
    Sampled {
        name: String,
        size: f64,
        angle: f64,
        roundness: f64,
        spacing_on: bool,
        spacing: f64,
        flip_x: bool,
        flip_y: bool,
        sampled_data: String,
    },
    Tips {
        angle: f64,
        size: f64,
        shape: String,
        physics: bool,
        spacing: f64,
        spacing_on: bool,
        flip_x: bool,
        flip_y: bool,
        tips_type: String,
        tips_length_ratio: f64,
        tips_hardness: f64,
        tips_grid_size: Option<f64>,
        tips_erodible_tip_height_map: Option<Vec<u8>>,
        tips_airbrush_cutoff_angle: f64,
        tips_airbrush_granularity: f64,
        tips_airbrush_streakiness: f64,
        tips_airbrush_splat_size: f64,
        tips_airbrush_splat_count: f64,
    },
    Dynamic {
        size: f64,
        angle: f64,
        shape: String,
        density: f64,
        length: f64,
        clumping: f64,
        thickness: f64,
        stiffness: f64,
        physics: bool,
        spacing: f64,
        spacing_on: bool,
        flip_x: bool,
        flip_y: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ShapeDynamics {
    pub size_dynamics: BrushDynamics,
    pub minimum_diameter: f64,
    pub tilt_scale: f64,
    pub angle_dynamics: BrushDynamics,
    pub roundness_dynamics: BrushDynamics,
    pub minimum_roundness: f64,
    pub flip_x: bool,
    pub flip_y: bool,
    pub brush_projection: bool,
}

#[derive(Debug, Clone)]
pub struct Scatter {
    pub both_axes: bool,
    pub scatter_dynamics: BrushDynamics,
    pub count_dynamics: BrushDynamics,
    pub count: f64,
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub id: String,
    pub name: String,
    pub invert: bool,
    pub scale: f64,
    pub brightness: f64,
    pub contrast: f64,
    pub blend_mode: BlendMode,
    pub depth: f64,
    pub depth_minimum: f64,
    pub depth_dynamics: BrushDynamics,
    pub texture_each_tip: bool,
}

#[derive(Debug, Clone)]
pub struct DualBrush {
    pub flip: bool,
    pub shape: BrushShape,
    pub blend_mode: BlendMode,
    pub use_scatter: bool,
    pub spacing: f64,
    pub count: f64,
    pub both_axes: bool,
    pub count_dynamics: BrushDynamics,
    pub scatter_dynamics: BrushDynamics,
}

#[derive(Debug, Clone)]
pub struct ColorDynamics {
    pub foreground_background: BrushDynamics,
    pub hue: f64,
    pub saturation: f64,
    pub brightness: f64,
    pub purity: f64,
    pub per_tip: bool,
}

#[derive(Debug, Clone)]
pub struct Transfer {
    pub flow_dynamics: BrushDynamics,
    pub opacity_dynamics: BrushDynamics,
    pub wetness_dynamics: BrushDynamics,
    pub mix_dynamics: BrushDynamics,
}

#[derive(Debug, Clone)]
pub struct BrushPose {
    pub override_angle: bool,
    pub override_tilt_x: bool,
    pub override_tilt_y: bool,
    pub override_pressure: bool,
    pub pressure: f64,
    pub tilt_x: f64,
    pub tilt_y: f64,
    pub angle: f64,
}

/// TS `Brush.toolOptions.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolType {
    Brush,
    MixerBrush,
    SmudgeBrush,
}

#[derive(Debug, Clone)]
pub struct ToolOptions {
    pub type_: ToolType,
    pub brush_preset: bool,
    pub flow: f64,
    pub wetness: Option<f64>,
    pub dryness: Option<f64>,
    pub mix: Option<f64>,
    pub smooth: f64,
    pub mode: BlendMode,
    pub opacity: f64,
    pub smoothing: bool,
    pub smoothing_value: f64,
    pub smoothing_radius_mode: bool,
    pub smoothing_catchup: bool,
    pub smoothing_catchup_at_end: bool,
    pub smoothing_zoom_compensation: bool,
    pub pressure_smoothing: bool,
    pub use_pressure_overrides_size: bool,
    pub use_pressure_overrides_opacity: bool,
    pub use_legacy: bool,
    pub auto_fill: Option<bool>,
    pub auto_clean: Option<bool>,
    pub load_solid_color_only: Option<bool>,
    pub sample_all_layers: Option<bool>,
    pub flow_dynamics: Option<BrushDynamics>,
    pub opacity_dynamics: Option<BrushDynamics>,
    pub size_dynamics: Option<BrushDynamics>,
    pub smudge_finger_painting: Option<bool>,
    pub smudge_sample_all_layers: Option<bool>,
    pub strength: Option<f64>,
}

/// TS `Brush`.
#[derive(Debug, Clone)]
pub struct Brush {
    pub name: String,
    pub shape: BrushShape,
    pub shape_dynamics: Option<ShapeDynamics>,
    pub scatter: Option<Scatter>,
    pub texture: Option<Texture>,
    pub dual_brush: Option<DualBrush>,
    pub color_dynamics: Option<ColorDynamics>,
    pub transfer: Option<Transfer>,
    pub brush_pose: Option<BrushPose>,
    pub noise: bool,
    pub wet_edges: bool,
    pub protect_texture: Option<bool>,
    pub spacing: f64,
    pub interpretation: Option<bool>,
    pub use_brush_size: bool,
    pub tool_options: Option<ToolOptions>,
}

// ===========================================================================
// Descriptor accessors (тонкая обёртка над типизированным деревом)
// ===========================================================================

fn dget<'a>(d: &'a Descriptor, key: &str) -> Option<&'a DescriptorValue> {
    d.get(key)
}

fn as_bool(v: Option<&DescriptorValue>) -> bool {
    matches!(v, Some(DescriptorValue::Boolean(true)))
}

fn as_opt_bool(v: Option<&DescriptorValue>) -> Option<bool> {
    match v {
        Some(DescriptorValue::Boolean(b)) => Some(*b),
        _ => None,
    }
}

fn as_number(v: Option<&DescriptorValue>) -> f64 {
    match v {
        Some(DescriptorValue::Integer(i)) => *i as f64,
        Some(DescriptorValue::Double(d)) => *d,
        Some(DescriptorValue::UnitDouble(u)) => u.value,
        _ => 0.0,
    }
}

fn as_opt_number(v: Option<&DescriptorValue>) -> Option<f64> {
    match v {
        Some(DescriptorValue::Integer(i)) => Some(*i as f64),
        Some(DescriptorValue::Double(d)) => Some(*d),
        Some(DescriptorValue::UnitDouble(u)) => Some(u.value),
        _ => None,
    }
}

fn as_text(v: Option<&DescriptorValue>) -> String {
    match v {
        Some(DescriptorValue::Text(s)) => s.clone(),
        Some(DescriptorValue::Enum(s)) => s.clone(),
        _ => String::new(),
    }
}

fn as_descriptor(v: Option<&DescriptorValue>) -> Option<&Descriptor> {
    match v {
        Some(DescriptorValue::Descriptor(d)) => Some(d),
        _ => None,
    }
}

fn as_units(v: Option<&DescriptorValue>) -> Option<&UnitDoubleValue> {
    match v {
        Some(DescriptorValue::UnitDouble(u)) => Some(u),
        _ => None,
    }
}

fn as_raw(v: Option<&DescriptorValue>) -> Option<&[u8]> {
    match v {
        Some(DescriptorValue::RawData(b)) => Some(b),
        _ => None,
    }
}

// ===========================================================================
// Parse helpers (зеркало descriptor.ts; DEPENDENCY GAP)
// ===========================================================================

/// Зеркало `parseAngle(x)`: 0 если undefined; иначе требует units == 'Angle'.
fn parse_angle(v: Option<&DescriptorValue>) -> ReadResult<f64> {
    match as_units(v) {
        None => Ok(0.0),
        Some(u) => {
            if u.units != "Angle" {
                return Err(ReadError::StrictViolation(format!(
                    "Invalid units: {}",
                    u.units
                )));
            }
            Ok(u.value)
        }
    }
}

/// Зеркало `parsePercent(x)`: 1 если undefined; иначе units == 'Percent', value/100.
fn parse_percent(v: Option<&DescriptorValue>) -> ReadResult<f64> {
    match as_units(v) {
        None => Ok(1.0),
        Some(u) => {
            if u.units != "Percent" {
                return Err(ReadError::StrictViolation(format!(
                    "Invalid units: {}",
                    u.units
                )));
            }
            Ok(u.value / 100.0)
        }
    }
}

/// Зеркало `parseUnitsToNumber(x, expectedUnits)`: требует совпадение units.
fn parse_units_to_number(v: Option<&DescriptorValue>, expected: &str) -> ReadResult<f64> {
    match as_units(v) {
        Some(u) if u.units == expected => Ok(u.value),
        Some(u) => Err(ReadError::StrictViolation(format!(
            "Invalid units: {}",
            u.units
        ))),
        None => Err(ReadError::StrictViolation(format!(
            "Invalid units: missing (expected {})",
            expected
        ))),
    }
}

/// Зеркало `BlnM.decode(code)`: ABR-код режима наложения -> [`BlendMode`].
/// Маппинг — обратный к `BlnM` enum в descriptor.ts (только коды, релевантные ABR).
fn blnm_decode(code: &str) -> BlendMode {
    // code может приходить как "BlnM.Nrml" — берём сегмент после точки.
    let key = code.split('.').nth(1).unwrap_or(code);
    match key {
        "Nrml" => BlendMode::Normal,
        "Dslv" => BlendMode::Dissolve,
        "Drkn" => BlendMode::Darken,
        "Mltp" => BlendMode::Multiply,
        "CBrn" => BlendMode::ColorBurn,
        "linearBurn" => BlendMode::LinearBurn,
        "darkerColor" => BlendMode::DarkerColor,
        "Lghn" => BlendMode::Lighten,
        "Scrn" => BlendMode::Screen,
        "CDdg" => BlendMode::ColorDodge,
        "linearDodge" => BlendMode::LinearDodge,
        "lighterColor" => BlendMode::LighterColor,
        "Ovrl" => BlendMode::Overlay,
        "SftL" => BlendMode::SoftLight,
        "HrdL" => BlendMode::HardLight,
        "vividLight" => BlendMode::VividLight,
        "linearLight" => BlendMode::LinearLight,
        "pinLight" => BlendMode::PinLight,
        "hardMix" => BlendMode::HardMix,
        "Dfrn" => BlendMode::Difference,
        "Xclu" => BlendMode::Exclusion,
        "blendSubtraction" => BlendMode::Subtract,
        "blendDivide" => BlendMode::Divide,
        "H   " => BlendMode::Hue,
        "Strt" => BlendMode::Saturation,
        "Clr " => BlendMode::Color,
        "Lmns" => BlendMode::Luminosity,
        // ABR-only codes; they gained `BlendMode` variants in upstream v31.
        "linearHeight" => BlendMode::LinearHeight,
        "Hght" => BlendMode::Height,
        "Sbtr" => BlendMode::Subtraction,
        // Photoshop 2026 writes the long-form id (`BlnM.normal`, `BlnM.colorBurn`)
        // instead of the historical code. Delegate to the shared `BlnM` codec, which
        // implements upstream's key/camelCase fallback chain, rather than repeating the
        // whole name table here.
        other => bln_m_decode(&format!("BlnM.{other}")),
    }
}

fn parse_dynamics(desc: &Descriptor) -> ReadResult<BrushDynamics> {
    let control_index = as_number(dget(desc, "bVTy")) as usize;
    Ok(BrushDynamics {
        control: DYNAMICS_CONTROL
            .get(control_index)
            .copied()
            .unwrap_or("off")
            .to_string(),
        steps: as_number(dget(desc, "fStp")),
        jitter: parse_percent(dget(desc, "jitter"))?,
        minimum: parse_percent(dget(desc, "Mnm "))?,
    })
}

fn parse_dynamics_opt(v: Option<&DescriptorValue>) -> ReadResult<Option<BrushDynamics>> {
    match as_descriptor(v) {
        Some(d) => Ok(Some(parse_dynamics(d)?)),
        None => Ok(None),
    }
}

fn parse_heightmap(array: &[u8]) -> Vec<u8> {
    array.to_vec()
}

fn parse_brush_shape(desc: &Descriptor) -> ReadResult<BrushShape> {
    match desc.class_id.as_str() {
        "computedBrush" => Ok(BrushShape::Computed {
            size: parse_units_to_number(dget(desc, "Dmtr"), "Pixels")?,
            angle: parse_angle(dget(desc, "Angl"))?,
            roundness: parse_percent(dget(desc, "Rndn"))?,
            spacing_on: as_bool(dget(desc, "Intr")),
            spacing: parse_percent(dget(desc, "Spcn"))?,
            flip_x: as_bool(dget(desc, "flipX")),
            flip_y: as_bool(dget(desc, "flipY")),
            hardness: parse_percent(dget(desc, "Hrdn"))?,
        }),
        "sampledBrush" => Ok(BrushShape::Sampled {
            size: parse_units_to_number(dget(desc, "Dmtr"), "Pixels")?,
            angle: parse_angle(dget(desc, "Angl"))?,
            roundness: parse_percent(dget(desc, "Rndn"))?,
            spacing_on: as_bool(dget(desc, "Intr")),
            spacing: parse_percent(dget(desc, "Spcn"))?,
            flip_x: as_bool(dget(desc, "flipX")),
            flip_y: as_bool(dget(desc, "flipY")),
            name: as_text(dget(desc, "Nm  ")),
            sampled_data: as_text(dget(desc, "sampledData")),
        }),
        "dBrush" => Ok(BrushShape::Dynamic {
            shape: shape_name(DYNAMIC_BRUSH_SHAPE_SHAPES, as_number(dget(desc, "Shp "))),
            angle: parse_angle(dget(desc, "Angl"))?,
            size: parse_units_to_number(dget(desc, "Dmtr"), "Pixels")?,
            density: parse_percent(dget(desc, "Dnst"))?,
            length: parse_percent(dget(desc, "Lngt"))?,
            clumping: parse_percent(dget(desc, "clumping"))?,
            thickness: parse_percent(dget(desc, "thickness"))?,
            stiffness: parse_percent(dget(desc, "stiffness"))?,
            physics: as_bool(dget(desc, "physics")),
            spacing: parse_percent(dget(desc, "Spcn"))?,
            spacing_on: as_bool(dget(desc, "Intr")),
            flip_x: as_bool(dget(desc, "flipX")),
            flip_y: as_bool(dget(desc, "flipY")),
        }),
        "dTips" => {
            let grid_size = as_number(dget(desc, "dtipsGridSize"));
            let height_map = as_raw(dget(desc, "dtipsErodibleTipHeightMap"));
            // Both fields are emitted only when the grid size is non-zero AND the
            // height map is present, mirroring upstream's combined condition.
            let (tips_grid_size, tips_erodible_tip_height_map) = match height_map {
                Some(map) if grid_size != 0.0 => (Some(grid_size), Some(parse_heightmap(map))),
                _ => (None, None),
            };
            Ok(BrushShape::Tips {
                angle: parse_angle(dget(desc, "Angl"))?,
                size: parse_units_to_number(dget(desc, "Dmtr"), "Pixels")?,
                shape: shape_name(DYNAMIC_BRUSH_SHAPE_SHAPES, as_number(dget(desc, "Shp "))),
                physics: as_bool(dget(desc, "physics")),
                spacing: parse_percent(dget(desc, "Spcn"))?,
                spacing_on: as_bool(dget(desc, "Intr")),
                flip_x: as_bool(dget(desc, "flipX")),
                flip_y: as_bool(dget(desc, "flipY")),
                tips_type: shape_name(TIPS_BRUSH_SHAPE_SHAPES, as_number(dget(desc, "dtipsType"))),
                tips_length_ratio: parse_percent(dget(desc, "dtipsLengthRatio"))?,
                tips_hardness: parse_percent(dget(desc, "dtipsHardness"))?,
                tips_grid_size,
                tips_erodible_tip_height_map,
                tips_airbrush_cutoff_angle: as_number(dget(desc, "dtipsAirbrushCutoffAngle")),
                tips_airbrush_granularity: parse_percent(dget(desc, "dtipsAirbrushGranularity"))?,
                tips_airbrush_streakiness: parse_percent(dget(desc, "dtipsAirbrushStreakiness"))?,
                tips_airbrush_splat_size: parse_percent(dget(desc, "dtipsAirbrushSplatSize"))?,
                tips_airbrush_splat_count: as_number(dget(desc, "dtipsAirbrushSplatCount")),
            })
        }
        other => Err(ReadError::StrictViolation(format!(
            "Unknown brush classId: {}",
            other
        ))),
    }
}

fn shape_name(table: &[&str], index: f64) -> String {
    table
        .get(index as usize)
        .copied()
        .unwrap_or("")
        .to_string()
}

const TO_BRUSH_TYPE: &[(&str, ToolType)] = &[
    ("_", ToolType::Brush),
    ("MixB", ToolType::MixerBrush),
    ("SmTl", ToolType::SmudgeBrush),
];

fn to_brush_type(class_id: &str) -> ToolType {
    TO_BRUSH_TYPE
        .iter()
        .find(|(k, _)| *k == class_id)
        .map(|(_, t)| *t)
        .unwrap_or(ToolType::Brush)
}

// ===========================================================================
// RLE + pattern decoding (зеркало psdReader.ts; DEPENDENCY GAP)
// ===========================================================================

struct PixelData<'a> {
    data: &'a mut [u8],
    width: usize,
    #[allow(dead_code)]
    height: usize,
}

/// Зеркало `readDataRLE(reader, pixelData, width, height, _bitDepth, step, offsets, large)`
/// для случая `large = false` (uint16-длины).
fn read_data_rle(
    reader: &mut PsdReader,
    pixel_data: Option<&mut PixelData>,
    width: usize,
    height: usize,
    _bit_depth: i32,
    step: usize,
    offsets: &[usize],
) -> ReadResult<()> {
    let mut lengths: Vec<u16> = vec![0; offsets.len() * height];
    let mut li = 0usize;
    for _ in 0..offsets.len() {
        for _ in 0..height {
            lengths[li] = read_uint16(reader)?;
            li += 1;
        }
    }

    let extra_limit = step.wrapping_sub(1);

    let has_data = pixel_data.is_some();
    // Чтобы избежать борьбы с заимствованиями, держим Option<&mut [u8]>.
    let mut data: Option<&mut [u8]> = pixel_data.map(|p| &mut *p.data);

    li = 0;
    for (c, &offset) in offsets.iter().enumerate() {
        let extra = c > extra_limit || offset > extra_limit;

        if !has_data || extra {
            for _ in 0..height {
                skip_bytes(reader, lengths[li] as usize);
                li += 1;
            }
        } else {
            let mut p = offset;
            for _ in 0..height {
                let length = lengths[li] as usize;
                let buffer = read_bytes(reader, length)?;
                li += 1;

                let buf = data.as_deref_mut().unwrap();
                let mut i = 0usize;
                let mut x = 0usize;
                while i < length {
                    let mut header = buffer[i] as i32;
                    if header > 128 {
                        i += 1;
                        let value = buffer[i];
                        header = 256 - header;
                        let mut j = 0;
                        while j <= header && x < width {
                            buf[p] = value;
                            p += step;
                            j += 1;
                            x += 1;
                        }
                    } else if header < 128 {
                        let mut j = 0;
                        while j <= header && x < width {
                            i += 1;
                            buf[p] = buffer[i];
                            p += step;
                            j += 1;
                            x += 1;
                        }
                    }
                    // header == 128: ignore
                    i += 1;
                }
            }
        }
    }
    Ok(())
}

// ===========================================================================
// Brush descriptor -> Brush
// ===========================================================================

fn parse_brush(brush: &Descriptor) -> ReadResult<Brush> {
    let shape_desc = as_descriptor(dget(brush, "Brsh"))
        .ok_or_else(|| ReadError::StrictViolation("Missing brush shape descriptor".to_string()))?;

    let mut b = Brush {
        name: as_text(dget(brush, "Nm  ")),
        shape: parse_brush_shape(shape_desc)?,
        spacing: parse_percent(dget(brush, "Spcn"))?,
        wet_edges: as_bool(dget(brush, "Wtdg")),
        noise: as_bool(dget(brush, "Nose")),
        use_brush_size: as_bool(dget(brush, "useBrushSize")),
        shape_dynamics: None,
        scatter: None,
        texture: None,
        dual_brush: None,
        color_dynamics: None,
        transfer: None,
        brush_pose: None,
        protect_texture: None,
        interpretation: None,
        tool_options: None,
    };

    if let Some(v) = as_opt_bool(dget(brush, "interpretation")) {
        b.interpretation = Some(v);
    }
    if let Some(v) = as_opt_bool(dget(brush, "protectTexture")) {
        b.protect_texture = Some(v);
    }

    if as_bool(dget(brush, "useTipDynamics")) {
        b.shape_dynamics = Some(ShapeDynamics {
            tilt_scale: parse_percent(dget(brush, "tiltScale"))?,
            size_dynamics: parse_dynamics_desc(brush, "szVr")?,
            angle_dynamics: parse_dynamics_desc(brush, "angleDynamics")?,
            roundness_dynamics: parse_dynamics_desc(brush, "roundnessDynamics")?,
            flip_x: as_bool(dget(brush, "flipX")),
            flip_y: as_bool(dget(brush, "flipY")),
            brush_projection: as_bool(dget(brush, "brushProjection")),
            minimum_diameter: parse_percent(dget(brush, "minimumDiameter"))?,
            minimum_roundness: parse_percent(dget(brush, "minimumRoundness"))?,
        });
    }

    if as_bool(dget(brush, "useScatter")) {
        b.scatter = Some(Scatter {
            count: as_number(dget(brush, "Cnt ")),
            both_axes: as_bool(dget(brush, "bothAxes")),
            count_dynamics: parse_dynamics_desc(brush, "countDynamics")?,
            scatter_dynamics: parse_dynamics_desc(brush, "scatterDynamics")?,
        });
    }

    if as_bool(dget(brush, "useTexture")) {
        if let Some(txtr) = as_descriptor(dget(brush, "Txtr")) {
            b.texture = Some(Texture {
                id: as_text(dget(txtr, "Idnt")),
                name: as_text(dget(txtr, "Nm  ")),
                blend_mode: blnm_decode(&as_text(dget(brush, "textureBlendMode"))),
                depth: parse_percent(dget(brush, "textureDepth"))?,
                depth_minimum: parse_percent(dget(brush, "minimumDepth"))?,
                depth_dynamics: parse_dynamics_desc(brush, "textureDepthDynamics")?,
                scale: parse_percent(dget(brush, "textureScale"))?,
                invert: as_bool(dget(brush, "InvT")),
                brightness: as_number(dget(brush, "textureBrightness")),
                contrast: as_number(dget(brush, "textureContrast")),
                texture_each_tip: as_bool(dget(brush, "TxtC")),
            });
        }
    }

    if let Some(db) = as_descriptor(dget(brush, "dualBrush")) {
        if as_bool(dget(db, "useDualBrush")) {
            let db_shape = as_descriptor(dget(db, "Brsh")).ok_or_else(|| {
                ReadError::StrictViolation("Missing dual brush shape".to_string())
            })?;
            b.dual_brush = Some(DualBrush {
                flip: as_bool(dget(db, "Flip")),
                shape: parse_brush_shape(db_shape)?,
                blend_mode: blnm_decode(&as_text(dget(db, "BlnM"))),
                use_scatter: as_bool(dget(db, "useScatter")),
                spacing: parse_percent(dget(db, "Spcn"))?,
                count: as_number(dget(db, "Cnt ")),
                both_axes: as_bool(dget(db, "bothAxes")),
                count_dynamics: parse_dynamics_desc(db, "countDynamics")?,
                scatter_dynamics: parse_dynamics_desc(db, "scatterDynamics")?,
            });
        }
    }

    if as_bool(dget(brush, "useColorDynamics")) {
        b.color_dynamics = Some(ColorDynamics {
            foreground_background: parse_dynamics_desc(brush, "clVr")?,
            hue: parse_percent(dget(brush, "H   "))?,
            saturation: parse_percent(dget(brush, "Strt"))?,
            brightness: parse_percent(dget(brush, "Brgh"))?,
            purity: parse_percent(dget(brush, "purity"))?,
            per_tip: as_bool(dget(brush, "colorDynamicsPerTip")),
        });
    }

    if as_bool(dget(brush, "usePaintDynamics")) {
        b.transfer = Some(Transfer {
            flow_dynamics: parse_dynamics_desc(brush, "prVr")?,
            opacity_dynamics: parse_dynamics_desc(brush, "opVr")?,
            wetness_dynamics: parse_dynamics_desc(brush, "wtVr")?,
            mix_dynamics: parse_dynamics_desc(brush, "mxVr")?,
        });
    }

    if as_bool(dget(brush, "useBrushPose")) {
        b.brush_pose = Some(BrushPose {
            override_angle: as_bool(dget(brush, "overridePoseAngle")),
            override_tilt_x: as_bool(dget(brush, "overridePoseTiltX")),
            override_tilt_y: as_bool(dget(brush, "overridePoseTiltY")),
            override_pressure: as_bool(dget(brush, "overridePosePressure")),
            pressure: parse_percent(dget(brush, "brushPosePressure"))?,
            tilt_x: as_number(dget(brush, "brushPoseTiltX")),
            tilt_y: as_number(dget(brush, "brushPoseTiltY")),
            angle: as_number(dget(brush, "brushPoseAngle")),
        });
    }

    if let Some(to) = as_descriptor(dget(brush, "toolOptions")) {
        let mut opts = ToolOptions {
            type_: to_brush_type(&to.class_id),
            brush_preset: as_bool(dget(to, "brushPreset")),
            flow: as_opt_number(dget(to, "flow")).unwrap_or(100.0),
            smooth: as_opt_number(dget(to, "Smoo")).unwrap_or(0.0),
            mode: blnm_decode(&{
                let m = as_text(dget(to, "Md  "));
                if m.is_empty() {
                    "BlnM.Nrml".to_string()
                } else {
                    m
                }
            }),
            opacity: as_opt_number(dget(to, "Opct")).unwrap_or(100.0),
            smoothing: as_bool(dget(to, "smoothing")),
            smoothing_value: as_opt_number(dget(to, "smoothingValue")).unwrap_or(0.0),
            smoothing_radius_mode: as_bool(dget(to, "smoothingRadiusMode")),
            smoothing_catchup: as_bool(dget(to, "smoothingCatchup")),
            smoothing_catchup_at_end: as_bool(dget(to, "smoothingCatchupAtEnd")),
            smoothing_zoom_compensation: as_bool(dget(to, "smoothingZoomCompensation")),
            pressure_smoothing: as_bool(dget(to, "pressureSmoothing")),
            use_pressure_overrides_size: as_bool(dget(to, "usePressureOverridesSize")),
            use_pressure_overrides_opacity: as_bool(dget(to, "usePressureOverridesOpacity")),
            use_legacy: as_bool(dget(to, "useLegacy")),
            wetness: None,
            dryness: None,
            mix: None,
            auto_fill: None,
            auto_clean: None,
            load_solid_color_only: None,
            sample_all_layers: None,
            flow_dynamics: None,
            opacity_dynamics: None,
            size_dynamics: None,
            smudge_finger_painting: None,
            smudge_sample_all_layers: None,
            strength: None,
        };

        opts.flow_dynamics = parse_dynamics_opt(dget(to, "prVr"))?;
        opts.opacity_dynamics = parse_dynamics_opt(dget(to, "opVr"))?;
        opts.size_dynamics = parse_dynamics_opt(dget(to, "szVr"))?;
        if let Some(v) = as_opt_number(dget(to, "wetness")) {
            opts.wetness = Some(v);
        }
        if let Some(v) = as_opt_number(dget(to, "dryness")) {
            opts.dryness = Some(v);
        }
        if let Some(v) = as_opt_number(dget(to, "mix")) {
            opts.mix = Some(v);
        }
        if let Some(v) = as_opt_bool(dget(to, "autoFill")) {
            opts.auto_fill = Some(v);
        }
        if let Some(v) = as_opt_bool(dget(to, "autoClean")) {
            opts.auto_clean = Some(v);
        }
        if let Some(v) = as_opt_bool(dget(to, "loadSolidColorOnly")) {
            opts.load_solid_color_only = Some(v);
        }
        if let Some(v) = as_opt_bool(dget(to, "sampleAllLayers")) {
            opts.sample_all_layers = Some(v);
        }
        if let Some(v) = as_opt_bool(dget(to, "SmdF")) {
            opts.smudge_finger_painting = Some(v);
        }
        if let Some(v) = as_opt_bool(dget(to, "SmdS")) {
            opts.smudge_sample_all_layers = Some(v);
        }
        if let Some(v) = as_opt_number(dget(to, "Prs ")) {
            opts.strength = Some(v);
        }

        b.tool_options = Some(opts);
    }

    Ok(b)
}

fn parse_dynamics_desc(parent: &Descriptor, key: &str) -> ReadResult<BrushDynamics> {
    match as_descriptor(dget(parent, key)) {
        Some(d) => parse_dynamics(d),
        None => Err(ReadError::StrictViolation(format!(
            "Missing dynamics descriptor: {}",
            key
        ))),
    }
}

// ===========================================================================
// Main reader
// ===========================================================================

/// Опции `readAbr`.
#[derive(Debug, Clone, Default)]
pub struct ReadAbrOptions {
    pub log_missing_features: bool,
}

/// Порт `readAbr(buffer, options)`.
pub fn read_abr(buffer: &[u8], _options: &ReadAbrOptions) -> ReadResult<Abr> {
    let reader = &mut PsdReader::new(buffer, None, None);
    let version = read_int16(reader)?;
    let mut samples: Vec<SampleInfo> = Vec::new();
    let mut brushes: Vec<Brush> = Vec::new();
    let mut patterns: Vec<PatternInfo> = Vec::new();

    if version == 1 || version == 2 {
        return Err(ReadError::StrictViolation(format!(
            "Unsupported ABR version ({})",
            version
        )));
    } else if version == 6 || version == 7 || version == 9 || version == 10 {
        let minor_version = read_int16(reader)?;
        if minor_version != 1 && minor_version != 2 {
            return Err(ReadError::StrictViolation(
                "Unsupported ABR minor version".to_string(),
            ));
        }

        while reader.offset < reader.buffer.len() {
            check_signature(reader, "8BIM", None)?;
            let type_ = read_signature(reader)?;
            let mut size = read_uint32(reader)? as usize;
            let end = reader.offset + size;

            match type_.as_str() {
                "samp" => {
                    while reader.offset < end {
                        let mut brush_length = read_uint32(reader)? as usize;
                        while brush_length & 0b11 != 0 {
                            brush_length += 1; // pad to 4 byte alignment
                        }
                        let brush_end = reader.offset + brush_length;

                        let id = read_pascal_string(reader, 1)?;

                        // v1 - skip Int16 bounds + unknown Int16 (10 bytes)
                        // v2 - skip unknown 264 bytes
                        skip_bytes(reader, if minor_version == 1 { 10 } else { 264 });

                        let y = read_int32(reader)?;
                        let x = read_int32(reader)?;
                        let h = read_int32(reader)? - y;
                        let w = read_int32(reader)? - x;
                        if w <= 0 || h <= 0 {
                            return Err(ReadError::StrictViolation("Invalid bounds".to_string()));
                        }

                        let bit_depth = read_int16(reader)?;
                        let compression = read_uint8(reader)?; // 0 - raw, 1 - RLE
                        let mut alpha = vec![0u8; (w * h) as usize];

                        if bit_depth == 8 {
                            if compression == 0 {
                                let bytes = read_bytes(reader, alpha.len())?;
                                alpha.copy_from_slice(&bytes);
                            } else if compression == 1 {
                                let mut pd = PixelData {
                                    data: &mut alpha,
                                    width: w as usize,
                                    height: h as usize,
                                };
                                read_data_rle(
                                    reader,
                                    Some(&mut pd),
                                    w as usize,
                                    h as usize,
                                    bit_depth as i32,
                                    1,
                                    &[0],
                                )?;
                            } else {
                                return Err(ReadError::StrictViolation(
                                    "Invalid compression".to_string(),
                                ));
                            }
                        } else if bit_depth == 16 {
                            if compression == 0 {
                                for sample in &mut alpha {
                                    *sample = (read_uint16(reader)? >> 8) as u8; // -> 8bit
                                }
                            } else if compression == 1 {
                                return Err(ReadError::StrictViolation(
                                    "not implemented (16bit RLE)".to_string(),
                                ));
                            } else {
                                return Err(ReadError::StrictViolation(
                                    "Invalid compression".to_string(),
                                ));
                            }
                        } else {
                            return Err(ReadError::StrictViolation("Invalid depth".to_string()));
                        }

                        samples.push(SampleInfo {
                            id,
                            bounds: SampleBounds { x, y, w, h },
                            alpha,
                        });
                        reader.offset = brush_end;
                    }
                }
                "desc" => {
                    let desc = crate::descriptor::read_version_and_descriptor(reader)?;
                    if let Some(DescriptorValue::List(list)) = dget(&desc, "Brsh") {
                        for item in list {
                            if let DescriptorValue::Descriptor(brush) = item {
                                brushes.push(parse_brush(brush)?);
                            }
                        }
                    }
                }
                "patt" => {
                    while reader.offset < end {
                        patterns.push(read_pattern(reader)?);
                    }
                    reader.offset = end;
                }
                "phry" => {
                    // TODO: what is this ? — читаем дескриптор и игнорируем.
                    let _desc = crate::descriptor::read_version_and_descriptor(reader)?;
                }
                other => {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid brush type: {}",
                        other
                    )));
                }
            }

            // align to 4 bytes
            while size % 4 != 0 {
                reader.offset += 1;
                size += 1;
            }
        }
    } else {
        return Err(ReadError::StrictViolation(format!(
            "Unsupported ABR version ({})",
            version
        )));
    }

    Ok(Abr {
        samples,
        patterns,
        brushes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::ColorMode;
    use std::path::PathBuf;

    fn fixture_dir() -> PathBuf {
        // crates/ag-psd -> repo root -> test/ag-psd/test/abr-read
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop(); // crates
        p.pop(); // repo root
        p.push("test/ag-psd/test/abr-read");
        p
    }

    fn find_abr(dir: &std::path::Path) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        for e in entries.flatten() {
            let path = e.path();
            if path.extension().map(|x| x == "abr").unwrap_or(false) {
                return Some(path);
            }
        }
        None
    }

    #[test]
    fn abr_rejects_v1() {
        // version 1 -> unsupported
        let bytes = [0u8, 1u8];
        assert!(read_abr(&bytes, &ReadAbrOptions::default()).is_err());
    }

    #[test]
    fn abr_rejects_unknown_version() {
        let bytes = [0u8, 99u8];
        assert!(read_abr(&bytes, &ReadAbrOptions::default()).is_err());
    }

    #[test]
    fn abr_decodes_fixtures_if_present() {
        let base = fixture_dir();
        if !base.exists() {
            eprintln!("abr fixtures not present, skipping");
            return;
        }

        let mut decoded_any = false;
        for sub in ["simple", "sample-and-pattern", "tilt", "special"] {
            let dir = base.join(sub);
            if let Some(abr_path) = find_abr(&dir) {
                let data = std::fs::read(&abr_path).expect("read fixture");
                match read_abr(&data, &ReadAbrOptions::default()) {
                    Ok(abr) => {
                        decoded_any = true;
                        // sanity: sample bounds positive, brush names valid utf8 (always)
                        for s in &abr.samples {
                            assert!(s.bounds.w > 0 && s.bounds.h > 0);
                            assert_eq!(s.alpha.len(), (s.bounds.w * s.bounds.h) as usize);
                        }
                        eprintln!(
                            "decoded {:?}: {} brushes, {} samples, {} patterns",
                            abr_path.file_name().unwrap(),
                            abr.brushes.len(),
                            abr.samples.len(),
                            abr.patterns.len()
                        );
                    }
                    Err(e) => {
                        // Surface decode errors so the test is meaningful.
                        panic!("failed to decode {:?}: {:?}", abr_path, e);
                    }
                }
            }
        }

        if !decoded_any {
            eprintln!("no .abr fixture files found, smoke-only");
        }
    }

    #[test]
    fn abr_simple_fixture_content() {
        let path = fixture_dir().join("simple/src.abr");
        if !path.exists() {
            eprintln!("simple fixture missing, skipping");
            return;
        }
        let data = std::fs::read(&path).unwrap();
        let abr = read_abr(&data, &ReadAbrOptions::default()).expect("decode simple");
        assert_eq!(abr.brushes.len(), 1);
        let b = &abr.brushes[0];
        assert_eq!(b.name, "Soft Round");
        assert_eq!(b.spacing, 1.0);
        assert!(!b.wet_edges);
        assert!(!b.noise);
        assert!(b.use_brush_size);
        match &b.shape {
            BrushShape::Computed {
                size,
                angle,
                roundness,
                spacing_on,
                spacing,
                hardness,
                ..
            } => {
                assert_eq!(*size, 30.0);
                assert_eq!(*angle, 0.0);
                assert_eq!(*roundness, 1.0);
                assert!(*spacing_on);
                assert_eq!(*spacing, 0.25);
                assert_eq!(*hardness, 0.0);
            }
            other => panic!("expected computed brush, got {:?}", other),
        }
    }

    /// Builds an indexed-mode pattern record whose single channel is stored
    /// uncompressed (`compressionMode == 0`).
    ///
    /// Kept here because this is the ABR-side regression test for the shared
    /// `crate::reader::read_pattern`: it proves the ABR `patt` section still
    /// decodes indexed patterns through the consolidated implementation.
    fn indexed_raw_pattern_bytes(palette: &[[u8; 3]; 256], indices: &[u8], w: u32, h: u32) -> Vec<u8> {
        use crate::writer::{
            create_writer_default, get_writer_buffer, write_bytes, write_int16, write_pascal_string,
            write_uint16, write_uint32, write_uint8, write_unicode_string,
        };

        let mut body = create_writer_default();
        write_uint32(&mut body, 1); // version
        write_uint32(&mut body, ColorMode::Indexed as u32);
        write_int16(&mut body, 0); // x
        write_int16(&mut body, 0); // y
        write_unicode_string(&mut body, "pat\0");
        write_pascal_string(&mut body, "deadbeef-0000-0000-0000-000000000000", 1);
        for entry in palette.iter() {
            write_uint8(&mut body, entry[0]);
            write_uint8(&mut body, entry[1]);
            write_uint8(&mut body, entry[2]);
        }
        write_uint32(&mut body, 0); // 4 bytes the reader skips

        write_uint32(&mut body, 3); // virtual memory array list version
        write_uint32(&mut body, 0); // list length, unused by the reader
        write_uint32(&mut body, 0); // top
        write_uint32(&mut body, 0); // left
        write_uint32(&mut body, h); // bottom
        write_uint32(&mut body, w); // right
        write_uint32(&mut body, 1); // channels count

        write_uint32(&mut body, 1); // has
        write_uint32(&mut body, (indices.len() + 4 + 16 + 2 + 1) as u32);
        write_uint32(&mut body, 8); // pixelDepth
        write_uint32(&mut body, 0); // ctop
        write_uint32(&mut body, 0); // cleft
        write_uint32(&mut body, h); // cbottom
        write_uint32(&mut body, w); // cright
        write_uint16(&mut body, 8); // pixelDepth2
        write_uint8(&mut body, 0); // compressionMode: raw
        write_bytes(&mut body, Some(indices));
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
        let bytes = indexed_raw_pattern_bytes(&palette, &indices, 2, 2);

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

    #[test]
    fn blnm_decode_handles_codes_long_form_and_abr_only_modes() {
        // Historical 4-char codes.
        assert_eq!(blnm_decode("BlnM.Nrml"), BlendMode::Normal);
        assert_eq!(blnm_decode("BlnM.CBrn"), BlendMode::ColorBurn);
        // ABR-only codes; before v31 these silently became `normal`.
        assert_eq!(blnm_decode("BlnM.linearHeight"), BlendMode::LinearHeight);
        assert_eq!(blnm_decode("BlnM.Hght"), BlendMode::Height);
        assert_eq!(blnm_decode("BlnM.Sbtr"), BlendMode::Subtraction);
        // Photoshop 2026 long form, delegated to the shared BlnM codec.
        assert_eq!(blnm_decode("BlnM.colorBurn"), BlendMode::ColorBurn);
        assert_eq!(blnm_decode("BlnM.linearHeight"), BlendMode::LinearHeight);
        // Bare code without the `BlnM.` prefix (some ABR payloads).
        assert_eq!(blnm_decode("Mltp"), BlendMode::Multiply);
        // Unknown -> default.
        assert_eq!(blnm_decode("BlnM.wibble"), BlendMode::Normal);
    }
}
