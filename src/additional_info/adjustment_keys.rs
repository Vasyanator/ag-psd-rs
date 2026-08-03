/*
File: crates/ag-psd/src/additional_info/adjustment_keys.rs

Purpose:
Group-модуль additional-info ключей. Группа: `Group::Adjustment`.
Корректирующие слои (brit, levl, curv, expA, vibA, hue2, blnc, blwh, phfl, mixr,
clrL, nvrt, post, thrs, grdm, selc, CgEd).

Source compatibility: зеркало одноимённых `addHandler(...)` в
`test/ag-psd/src/additionalInfo.ts` (read + has + write, точная байтовая раскладка
/ дескрипторная структура).

BINARY vs DESCRIPTOR:
- BINARY raw layout: brit, levl, curv (legacy + 'Crv ' block), expA, hue2, blnc,
  phfl, mixr, nvrt, post, thrs, grdm, selc.
- DESCRIPTOR (readVersionAndDescriptor / writeVersionAndDescriptor): vibA, blwh,
  clrL (с uint16(1) префиксом), CgEd.

CONSOLIDATION GAP — локально реализованные дескрипторные хелперы (нет shared слоя):
- `parse_color` / `serialize_color` (DescriptorColor <-> Color) — нужны blwh.tintColor;
- `read_color_binary` (10-байтовый бинарный color) — дублирует приватный `read_color`
  из effects_helpers.rs / image_resources.rs; нужен phfl(v2) и grdm color stops;
- enum-кодеки colorLookupType / LUTFormatType / colorLookupOrder /
  gradientInterpolationMethodType (createEnum decode/encode) реализованы inline.
Все три категории — кандидаты в общий descriptor-helpers слой.
*/

use crate::additional_info::{ReadCtx, WriteCtx};
use crate::descriptor::{
    read_version_and_descriptor, write_version_and_descriptor, Descriptor, DescriptorValue,
};
use crate::psd::{
    AdjustmentLayer, BlackAndWhiteAdjustment, BrightnessAdjustment, ChannelMixerAdjustment,
    ChannelMixerChannel, Cmyk, Color, ColorBalanceAdjustment, ColorBalanceValues, ColorLookupType,
    ColorLookupAdjustment, ColorStop, CurvesAdjustment, CurvesPoint, ExposureAdjustment,
    GradientColorModel, GradientMapAdjustment, GradientMapType,
    HueSaturationAdjustment, HueSaturationAdjustmentChannel, InterpolationMethod, InvertAdjustment,
    Lab, LayerAdditionalInfo, LevelsAdjustment, LevelsAdjustmentChannel, LutFormat, OpacityStop,
    PhotoFilterAdjustment, PosterizeAdjustment, PresetInfo, RgbBgrOrder,
    SelectiveColorAdjustment, SelectiveColorMode, ThresholdAdjustment, VibranceAdjustment,
};
use crate::reader::{
    read_color, read_float32, read_int16, read_int32, read_signature, read_uint16, read_uint32,
    read_uint8, read_unicode_string, skip_bytes, PsdReader, ReadError, ReadResult,
};
use crate::writer::{
    write_color, write_float32, write_int16, write_signature, write_uint16, write_uint32,
    write_uint8, write_unicode_string_with_padding, write_zeros, PsdWriter,
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
        "brit" => read_brit(reader, info, left)?,
        "levl" => read_levl(reader, info, left)?,
        "curv" => read_curv(reader, info, left)?,
        "expA" => read_expa(reader, info, left)?,
        "vibA" => read_viba(reader, info, left)?,
        "hue2" => read_hue2(reader, info, left)?,
        "blnc" => read_blnc(reader, info, left)?,
        "blwh" => read_blwh(reader, info, left)?,
        "phfl" => read_phfl(reader, info, left)?,
        "mixr" => read_mixr(reader, info, left)?,
        "clrL" => read_clrl(reader, info, left)?,
        "nvrt" => {
            info.adjustment = Some(AdjustmentLayer::Invert(InvertAdjustment));
            skip_bytes(reader, left(reader));
        }
        "post" => {
            info.adjustment = Some(AdjustmentLayer::Posterize(PosterizeAdjustment {
                levels: Some(read_uint16(reader)? as f64),
            }));
            skip_bytes(reader, left(reader));
        }
        "thrs" => {
            info.adjustment = Some(AdjustmentLayer::Threshold(ThresholdAdjustment {
                level: Some(read_uint16(reader)? as f64),
            }));
            skip_bytes(reader, left(reader));
        }
        "grdm" => read_grdm(reader, info, left)?,
        "selc" => read_selc(reader, info)?,
        "CgEd" => read_cged(reader, info, left)?,
        _ => return Ok(None),
    }
    Ok(Some(()))
}

// --- brit ------------------------------------------------------------------

fn read_brit(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    // ignore if got one from CgEd block
    if info.adjustment.is_none() {
        let brightness = read_int16(reader)? as f64;
        let contrast = read_int16(reader)? as f64;
        let mean_value = read_int16(reader)? as f64;
        let lab_color_only = read_uint8(reader)? != 0;
        info.adjustment = Some(AdjustmentLayer::Brightness(BrightnessAdjustment {
            brightness: Some(brightness),
            contrast: Some(contrast),
            mean_value: Some(mean_value),
            lab_color_only: Some(lab_color_only),
            use_legacy: Some(true),
            auto: None,
        }));
    }
    skip_bytes(reader, left(reader));
    Ok(())
}

// --- levl ------------------------------------------------------------------

fn read_levels_channel(reader: &mut PsdReader) -> ReadResult<LevelsAdjustmentChannel> {
    let shadow_input = read_int16(reader)? as f64;
    let highlight_input = read_int16(reader)? as f64;
    let shadow_output = read_int16(reader)? as f64;
    let highlight_output = read_int16(reader)? as f64;
    let midtone_input = read_int16(reader)? as f64 / 100.0;
    Ok(LevelsAdjustmentChannel {
        shadow_input,
        highlight_input,
        shadow_output,
        highlight_output,
        midtone_input,
    })
}

fn write_levels_channel(writer: &mut PsdWriter, channel: &LevelsAdjustmentChannel) {
    write_int16(writer, channel.shadow_input as i16);
    write_int16(writer, channel.highlight_input as i16);
    write_int16(writer, channel.shadow_output as i16);
    write_int16(writer, channel.highlight_output as i16);
    write_int16(writer, (channel.midtone_input * 100.0).round() as i16);
}

fn read_levl(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if read_uint16(reader)? != 2 {
        return Err(ReadError::StrictViolation("Invalid levl version".to_string()));
    }

    let preset = existing_preset(info);
    info.adjustment = Some(AdjustmentLayer::Levels(LevelsAdjustment {
        preset,
        rgb: Some(read_levels_channel(reader)?),
        red: Some(read_levels_channel(reader)?),
        green: Some(read_levels_channel(reader)?),
        blue: Some(read_levels_channel(reader)?),
    }));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- curv ------------------------------------------------------------------

fn read_curve_channel(reader: &mut PsdReader) -> ReadResult<Vec<CurvesPoint>> {
    let nodes = read_uint16(reader)?;
    let mut channel = Vec::with_capacity(nodes as usize);
    for _ in 0..nodes {
        let output = read_int16(reader)? as f64;
        let input = read_int16(reader)? as f64;
        channel.push(CurvesPoint { input, output });
    }
    Ok(channel)
}

fn write_curve_channel(writer: &mut PsdWriter, channel: &[CurvesPoint]) {
    write_uint16(writer, channel.len() as u16);
    for n in channel {
        write_uint16(writer, n.output as u16);
        write_uint16(writer, n.input as u16);
    }
}

fn read_curv(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    read_uint8(reader)?;
    if read_uint16(reader)? != 1 {
        return Err(ReadError::StrictViolation("Invalid curv version".to_string()));
    }
    read_uint16(reader)?;
    let channels = read_uint16(reader)?;

    let mut adj = CurvesAdjustment {
        preset: existing_preset(info),
        rgb: None,
        red: None,
        green: None,
        blue: None,
    };

    if channels & 1 != 0 {
        adj.rgb = Some(read_curve_channel(reader)?);
    }
    if channels & 2 != 0 {
        adj.red = Some(read_curve_channel(reader)?);
    }
    if channels & 4 != 0 {
        adj.green = Some(read_curve_channel(reader)?);
    }
    if channels & 8 != 0 {
        adj.blue = Some(read_curve_channel(reader)?);
    }

    info.adjustment = Some(AdjustmentLayer::Curves(adj));

    // ignoring duplicate 'Crv ' block (upstream skips via left())
    skip_bytes(reader, left(reader));
    Ok(())
}

// --- expA ------------------------------------------------------------------

fn read_expa(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if read_uint16(reader)? != 1 {
        return Err(ReadError::StrictViolation("Invalid expA version".to_string()));
    }

    let preset = existing_preset(info);
    info.adjustment = Some(AdjustmentLayer::Exposure(ExposureAdjustment {
        preset,
        exposure: Some(read_float32(reader)? as f64),
        offset: Some(read_float32(reader)? as f64),
        gamma: Some(read_float32(reader)? as f64),
    }));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- vibA ------------------------------------------------------------------

fn read_viba(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    let desc = read_version_and_descriptor(reader)?;
    let mut adj = VibranceAdjustment::default();
    if let Some(v) = desc_double(&desc, "vibrance") {
        adj.vibrance = Some(v);
    }
    if let Some(v) = desc_double(&desc, "Strt") {
        adj.saturation = Some(v);
    }
    info.adjustment = Some(AdjustmentLayer::Vibrance(adj));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- hue2 ------------------------------------------------------------------

fn read_hue_channel(reader: &mut PsdReader) -> ReadResult<HueSaturationAdjustmentChannel> {
    Ok(HueSaturationAdjustmentChannel {
        a: read_int16(reader)? as f64,
        b: read_int16(reader)? as f64,
        c: read_int16(reader)? as f64,
        d: read_int16(reader)? as f64,
        hue: read_int16(reader)? as f64,
        saturation: read_int16(reader)? as f64,
        lightness: read_int16(reader)? as f64,
    })
}

fn write_hue_channel(writer: &mut PsdWriter, channel: Option<&HueSaturationAdjustmentChannel>) {
    let c = channel.cloned().unwrap_or_default();
    write_int16(writer, c.a as i16);
    write_int16(writer, c.b as i16);
    write_int16(writer, c.c as i16);
    write_int16(writer, c.d as i16);
    write_int16(writer, c.hue as i16);
    write_int16(writer, c.saturation as i16);
    write_int16(writer, c.lightness as i16);
}

fn read_hue2(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if read_uint16(reader)? != 2 {
        return Err(ReadError::StrictViolation("Invalid hue2 version".to_string()));
    }

    let preset = existing_preset(info);
    info.adjustment = Some(AdjustmentLayer::HueSaturation(HueSaturationAdjustment {
        preset,
        master: Some(read_hue_channel(reader)?),
        reds: Some(read_hue_channel(reader)?),
        yellows: Some(read_hue_channel(reader)?),
        greens: Some(read_hue_channel(reader)?),
        cyans: Some(read_hue_channel(reader)?),
        blues: Some(read_hue_channel(reader)?),
        magentas: Some(read_hue_channel(reader)?),
    }));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- blnc ------------------------------------------------------------------

fn read_color_balance(reader: &mut PsdReader) -> ReadResult<ColorBalanceValues> {
    Ok(ColorBalanceValues {
        cyan_red: read_int16(reader)? as f64,
        magenta_green: read_int16(reader)? as f64,
        yellow_blue: read_int16(reader)? as f64,
    })
}

fn write_color_balance(writer: &mut PsdWriter, value: Option<&ColorBalanceValues>) {
    let v = value.cloned().unwrap_or_default();
    write_int16(writer, v.cyan_red as i16);
    write_int16(writer, v.magenta_green as i16);
    write_int16(writer, v.yellow_blue as i16);
}

fn read_blnc(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    info.adjustment = Some(AdjustmentLayer::ColorBalance(ColorBalanceAdjustment {
        shadows: Some(read_color_balance(reader)?),
        midtones: Some(read_color_balance(reader)?),
        highlights: Some(read_color_balance(reader)?),
        preserve_luminosity: Some(read_uint8(reader)? != 0),
    }));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- blwh ------------------------------------------------------------------

fn read_blwh(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    let desc = read_version_and_descriptor(reader)?;
    let mut adj = BlackAndWhiteAdjustment {
        preset: PresetInfo::default(),
        reds: desc_double(&desc, "Rd  "),
        yellows: desc_double(&desc, "Yllw"),
        greens: desc_double(&desc, "Grn "),
        cyans: desc_double(&desc, "Cyn "),
        blues: desc_double(&desc, "Bl  "),
        magentas: desc_double(&desc, "Mgnt"),
        use_tint: Some(desc_bool(&desc, "useTint").unwrap_or(false)),
        tint_color: None,
    };
    adj.preset.preset_kind = desc_double(&desc, "bwPresetKind");
    adj.preset.preset_file_name = desc_text(&desc, "blackAndWhitePresetFileName");

    if let Some(DescriptorValue::Descriptor(c)) = desc.get("tintColor") {
        adj.tint_color = Some(parse_color(c)?);
    }

    info.adjustment = Some(AdjustmentLayer::BlackAndWhite(adj));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- phfl ------------------------------------------------------------------

fn read_phfl(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    let version = read_uint16(reader)?;
    if version != 2 && version != 3 {
        return Err(ReadError::StrictViolation("Invalid phfl version".to_string()));
    }

    let color = if version == 2 {
        read_color(reader)?
    } else {
        // version 3 (upstream notes this is probably wrong)
        Color::Lab(Lab {
            l: read_int32(reader)? as f64 / 100.0,
            a: read_int32(reader)? as f64 / 100.0,
            b: read_int32(reader)? as f64 / 100.0,
        })
    };

    info.adjustment = Some(AdjustmentLayer::PhotoFilter(PhotoFilterAdjustment {
        color: Some(color),
        density: Some(read_uint32(reader)? as f64 / 100.0),
        preserve_luminosity: Some(read_uint8(reader)? != 0),
    }));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- mixr ------------------------------------------------------------------

fn read_mixr_channel(reader: &mut PsdReader) -> ReadResult<ChannelMixerChannel> {
    let red = read_int16(reader)? as f64;
    let green = read_int16(reader)? as f64;
    let blue = read_int16(reader)? as f64;
    skip_bytes(reader, 2);
    let constant = read_int16(reader)? as f64;
    Ok(ChannelMixerChannel {
        red,
        green,
        blue,
        constant,
    })
}

fn write_mixr_channel(writer: &mut PsdWriter, channel: Option<&ChannelMixerChannel>) {
    let c = channel.cloned().unwrap_or_default();
    write_int16(writer, c.red as i16);
    write_int16(writer, c.green as i16);
    write_int16(writer, c.blue as i16);
    write_zeros(writer, 2);
    write_int16(writer, c.constant as i16);
}

fn read_mixr(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if read_uint16(reader)? != 1 {
        return Err(ReadError::StrictViolation("Invalid mixr version".to_string()));
    }

    let monochrome = read_uint16(reader)? != 0;
    let mut adj = ChannelMixerAdjustment {
        preset: existing_preset(info),
        monochrome: Some(monochrome),
        red: None,
        green: None,
        blue: None,
        gray: None,
    };

    if !monochrome {
        adj.red = Some(read_mixr_channel(reader)?);
        adj.green = Some(read_mixr_channel(reader)?);
        adj.blue = Some(read_mixr_channel(reader)?);
    }
    adj.gray = Some(read_mixr_channel(reader)?);

    info.adjustment = Some(AdjustmentLayer::ChannelMixer(adj));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- clrL ------------------------------------------------------------------

fn read_clrl(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    if read_uint16(reader)? != 1 {
        return Err(ReadError::StrictViolation("Invalid clrL version".to_string()));
    }

    let desc = read_version_and_descriptor(reader)?;
    let mut adj = ColorLookupAdjustment::default();

    if let Some(s) = desc_enum_value(&desc, "lookupType") {
        adj.lookup_type = color_lookup_type_decode(&s);
    }
    if let Some(s) = desc_text(&desc, "Nm  ") {
        adj.name = Some(s);
    }
    if let Some(b) = desc_bool(&desc, "Dthr") {
        adj.dither = Some(b);
    }
    if let Some(d) = desc_raw(&desc, "profile") {
        adj.profile = Some(d);
    }
    if let Some(s) = desc_enum_value(&desc, "LUTFormat") {
        adj.lut_format = lut_format_decode(&s);
    }
    if let Some(s) = desc_enum_value(&desc, "dataOrder") {
        adj.data_order = color_lookup_order_decode(&s);
    }
    if let Some(s) = desc_enum_value(&desc, "tableOrder") {
        adj.table_order = color_lookup_order_decode(&s);
    }
    if let Some(d) = desc_raw(&desc, "LUT3DFileData") {
        adj.lut3d_file_data = Some(d);
    }
    if let Some(s) = desc_text(&desc, "LUT3DFileName") {
        adj.lut3d_file_name = Some(s);
    }

    info.adjustment = Some(AdjustmentLayer::ColorLookup(adj));

    skip_bytes(reader, left(reader));
    Ok(())
}

// --- grdm ------------------------------------------------------------------

const GRDM_COLOR_MODELS: [&str; 7] = ["", "", "", "rgb", "hsb", "", "lab"];

fn read_grdm(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    let version = read_uint16(reader)?;
    if version != 1 && version != 3 {
        return Err(ReadError::StrictViolation("Invalid grdm version".to_string()));
    }

    let reverse = read_uint8(reader)? != 0;
    let dither = read_uint8(reader)? != 0;

    let has_method = read_uint8(reader)? != 0;
    reader.offset -= 1;
    let method = if has_method {
        let sig = read_signature(reader)?;
        gradient_interpolation_method_decode(&sig)
    } else {
        None
    };

    let name = read_unicode_string(reader)?;
    let mut color_stops: Vec<ColorStop> = Vec::new();
    let mut opacity_stops: Vec<OpacityStop> = Vec::new();

    let stops_count = read_uint16(reader)?;
    for _ in 0..stops_count {
        let location = read_uint32(reader)? as f64;
        let midpoint = read_uint32(reader)? as f64 / 100.0;
        let color = read_color(reader)?;
        color_stops.push(ColorStop {
            color,
            location,
            midpoint,
        });
        skip_bytes(reader, 2);
    }

    let opacity_stops_count = read_uint16(reader)?;
    for _ in 0..opacity_stops_count {
        let location = read_uint32(reader)? as f64;
        let midpoint = read_uint32(reader)? as f64 / 100.0;
        let opacity = read_uint16(reader)? as f64 / 0xff as f64;
        opacity_stops.push(OpacityStop {
            opacity,
            location,
            midpoint,
        });
    }

    let expansion_count = read_uint16(reader)?;
    if expansion_count != 2 {
        return Err(ReadError::StrictViolation(
            "Invalid grdm expansion count".to_string(),
        ));
    }

    let interpolation = read_uint16(reader)?;
    let smoothness = interpolation as f64 / 4096.0;

    let length = read_uint16(reader)?;
    if length != 32 {
        return Err(ReadError::StrictViolation("Invalid grdm length".to_string()));
    }

    let gradient_type = if read_uint16(reader)? != 0 {
        GradientMapType::Noise
    } else {
        GradientMapType::Solid
    };
    let random_seed = read_uint32(reader)? as f64;
    let add_transparency = read_uint16(reader)? != 0;
    let restrict_colors = read_uint16(reader)? != 0;
    let roughness = read_uint32(reader)? as f64 / 4096.0;
    let color_model = grdm_color_model_from_index(read_uint16(reader)? as usize);

    let min = vec![
        read_uint16(reader)? as f64 / 0x8000 as f64,
        read_uint16(reader)? as f64 / 0x8000 as f64,
        read_uint16(reader)? as f64 / 0x8000 as f64,
        read_uint16(reader)? as f64 / 0x8000 as f64,
    ];
    let max = vec![
        read_uint16(reader)? as f64 / 0x8000 as f64,
        read_uint16(reader)? as f64 / 0x8000 as f64,
        read_uint16(reader)? as f64 / 0x8000 as f64,
        read_uint16(reader)? as f64 / 0x8000 as f64,
    ];

    skip_bytes(reader, left(reader));

    // locations are stored scaled by `interpolation`; normalize.
    for s in &mut color_stops {
        s.location /= interpolation as f64;
    }
    for s in &mut opacity_stops {
        s.location /= interpolation as f64;
    }

    info.adjustment = Some(AdjustmentLayer::GradientMap(GradientMapAdjustment {
        name: Some(name),
        gradient_type,
        dither: Some(dither),
        reverse: Some(reverse),
        method,
        smoothness: Some(smoothness),
        color_stops: Some(color_stops),
        opacity_stops: Some(opacity_stops),
        roughness: Some(roughness),
        color_model: Some(color_model),
        random_seed: Some(random_seed),
        restrict_colors: Some(restrict_colors),
        add_transparency: Some(add_transparency),
        min: Some(min),
        max: Some(max),
    }));

    Ok(())
}

// --- selc ------------------------------------------------------------------

fn read_selective_colors(reader: &mut PsdReader) -> ReadResult<Cmyk> {
    Ok(Cmyk {
        c: read_int16(reader)? as f64,
        m: read_int16(reader)? as f64,
        y: read_int16(reader)? as f64,
        k: read_int16(reader)? as f64,
    })
}

fn write_selective_colors(writer: &mut PsdWriter, cmyk: Option<&Cmyk>) {
    let c = cmyk.cloned().unwrap_or_default();
    write_int16(writer, c.c as i16);
    write_int16(writer, c.m as i16);
    write_int16(writer, c.y as i16);
    write_int16(writer, c.k as i16);
}

fn read_selc(reader: &mut PsdReader, info: &mut LayerAdditionalInfo) -> ReadResult<()> {
    if read_uint16(reader)? != 1 {
        return Err(ReadError::StrictViolation("Invalid selc version".to_string()));
    }

    let mode = if read_uint16(reader)? != 0 {
        SelectiveColorMode::Absolute
    } else {
        SelectiveColorMode::Relative
    };
    skip_bytes(reader, 8);

    info.adjustment = Some(AdjustmentLayer::SelectiveColor(SelectiveColorAdjustment {
        mode: Some(mode),
        reds: Some(read_selective_colors(reader)?),
        yellows: Some(read_selective_colors(reader)?),
        greens: Some(read_selective_colors(reader)?),
        cyans: Some(read_selective_colors(reader)?),
        blues: Some(read_selective_colors(reader)?),
        magentas: Some(read_selective_colors(reader)?),
        whites: Some(read_selective_colors(reader)?),
        neutrals: Some(read_selective_colors(reader)?),
        blacks: Some(read_selective_colors(reader)?),
    }));

    Ok(())
}

// --- CgEd ------------------------------------------------------------------

fn read_cged(
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
) -> ReadResult<()> {
    let desc = read_version_and_descriptor(reader)?;
    if desc_double(&desc, "Vrsn") != Some(1.0) {
        return Err(ReadError::StrictViolation("Invalid CgEd version".to_string()));
    }

    if desc.get("presetFileName").is_some() {
        // preset file name for levels / exposure / hue-saturation
        let preset = PresetInfo {
            preset_kind: desc_double(&desc, "presetKind"),
            preset_file_name: desc_text(&desc, "presetFileName"),
        };
        apply_preset(info, preset);
    } else if desc.get("curvesPresetFileName").is_some() {
        let preset = PresetInfo {
            preset_kind: desc_double(&desc, "curvesPresetKind"),
            preset_file_name: desc_text(&desc, "curvesPresetFileName"),
        };
        apply_preset(info, preset);
    } else if desc.get("mixerPresetFileName").is_some() {
        let preset = PresetInfo {
            preset_kind: desc_double(&desc, "mixerPresetKind"),
            preset_file_name: desc_text(&desc, "mixerPresetFileName"),
        };
        apply_preset(info, preset);
    } else {
        info.adjustment = Some(AdjustmentLayer::Brightness(BrightnessAdjustment {
            brightness: desc_double(&desc, "Brgh"),
            contrast: desc_double(&desc, "Cntr"),
            mean_value: desc_double(&desc, "means"),
            use_legacy: Some(desc_bool(&desc, "useLegacy").unwrap_or(false)),
            lab_color_only: Some(desc_bool(&desc, "Lab ").unwrap_or(false)),
            auto: Some(desc_bool(&desc, "Auto").unwrap_or(false)),
        }));
    }

    skip_bytes(reader, left(reader));
    Ok(())
}

/// Накладывает preset (kind + file name) на уже прочитанный adjustment.
/// Зеркало spread `{ ...target.adjustment, presetKind, presetFileName }`.
fn apply_preset(info: &mut LayerAdditionalInfo, preset: PresetInfo) {
    match info.adjustment.as_mut() {
        Some(AdjustmentLayer::Levels(a)) => a.preset = preset,
        Some(AdjustmentLayer::Exposure(a)) => a.preset = preset,
        Some(AdjustmentLayer::HueSaturation(a)) => a.preset = preset,
        Some(AdjustmentLayer::Curves(a)) => a.preset = preset,
        Some(AdjustmentLayer::ChannelMixer(a)) => a.preset = preset,
        _ => {
            // upstream spreads onto whatever adjustment exists; if none yet, it
            // would produce an object with only preset fields — not a valid PSD
            // case in practice. Mirror by leaving adjustment as-is.
        }
    }
}

// ===========================================================================
// HAS
// ===========================================================================

/// См. GROUP-MODULE CONTRACT в mod.rs.
pub fn has(key: &str, info: &LayerAdditionalInfo) -> Option<bool> {
    let owned = matches!(
        key,
        "brit"
            | "levl"
            | "curv"
            | "expA"
            | "vibA"
            | "hue2"
            | "blnc"
            | "blwh"
            | "phfl"
            | "mixr"
            | "clrL"
            | "nvrt"
            | "post"
            | "thrs"
            | "grdm"
            | "selc"
            | "CgEd"
    );
    if !owned {
        return None;
    }

    let a = info.adjustment.as_ref();
    let result = match key {
        "brit" => adj_type_is(a, "brightness/contrast"),
        "levl" => adj_type_is(a, "levels"),
        "curv" => adj_type_is(a, "curves"),
        "expA" => adj_type_is(a, "exposure"),
        "vibA" => adj_type_is(a, "vibrance"),
        "hue2" => adj_type_is(a, "hue/saturation"),
        "blnc" => adj_type_is(a, "color balance"),
        "blwh" => adj_type_is(a, "black & white"),
        "phfl" => adj_type_is(a, "photo filter"),
        "mixr" => adj_type_is(a, "channel mixer"),
        "clrL" => adj_type_is(a, "color lookup"),
        "nvrt" => adj_type_is(a, "invert"),
        "post" => adj_type_is(a, "posterize"),
        "thrs" => adj_type_is(a, "threshold"),
        "grdm" => adj_type_is(a, "gradient map"),
        "selc" => adj_type_is(a, "selective color"),
        "CgEd" => cged_has(a),
        _ => false,
    };
    Some(result)
}

/// Зеркало `adjustmentType(type)`.
fn adj_type_is(a: Option<&AdjustmentLayer>, type_: &str) -> bool {
    adj_type_name(a) == Some(type_)
}

/// Зеркало проверки `CgEd`:
/// brightness/contrast без useLegacy, ИЛИ levels/curves/exposure/channel mixer/
/// hue/saturation с заданным presetFileName.
fn cged_has(a: Option<&AdjustmentLayer>) -> bool {
    match a {
        Some(AdjustmentLayer::Brightness(b)) => b.use_legacy != Some(true),
        Some(AdjustmentLayer::Levels(l)) => l.preset.preset_file_name.is_some(),
        Some(AdjustmentLayer::Curves(c)) => c.preset.preset_file_name.is_some(),
        Some(AdjustmentLayer::Exposure(e)) => e.preset.preset_file_name.is_some(),
        Some(AdjustmentLayer::ChannelMixer(m)) => m.preset.preset_file_name.is_some(),
        Some(AdjustmentLayer::HueSaturation(h)) => h.preset.preset_file_name.is_some(),
        _ => false,
    }
}

fn adj_type_name(a: Option<&AdjustmentLayer>) -> Option<&'static str> {
    Some(match a? {
        AdjustmentLayer::Brightness(_) => "brightness/contrast",
        AdjustmentLayer::Levels(_) => "levels",
        AdjustmentLayer::Curves(_) => "curves",
        AdjustmentLayer::Exposure(_) => "exposure",
        AdjustmentLayer::Vibrance(_) => "vibrance",
        AdjustmentLayer::HueSaturation(_) => "hue/saturation",
        AdjustmentLayer::ColorBalance(_) => "color balance",
        AdjustmentLayer::BlackAndWhite(_) => "black & white",
        AdjustmentLayer::PhotoFilter(_) => "photo filter",
        AdjustmentLayer::ChannelMixer(_) => "channel mixer",
        AdjustmentLayer::ColorLookup(_) => "color lookup",
        AdjustmentLayer::Invert(_) => "invert",
        AdjustmentLayer::Posterize(_) => "posterize",
        AdjustmentLayer::Threshold(_) => "threshold",
        AdjustmentLayer::GradientMap(_) => "gradient map",
        AdjustmentLayer::SelectiveColor(_) => "selective color",
    })
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
    let a = info.adjustment.as_ref();
    match key {
        "brit" => write_brit(writer, a),
        "levl" => write_levl(writer, a),
        "curv" => write_curv(writer, a),
        "expA" => write_expa(writer, a),
        "vibA" => write_viba(writer, a),
        "hue2" => write_hue2(writer, a),
        "blnc" => write_blnc(writer, a),
        "blwh" => write_blwh(writer, a),
        "phfl" => write_phfl(writer, a),
        "mixr" => write_mixr(writer, a),
        "clrL" => write_clrl(writer, a),
        "nvrt" => { /* nothing to write */ }
        "post" => write_post(writer, a),
        "thrs" => write_thrs(writer, a),
        "grdm" => write_grdm(writer, a),
        "selc" => write_selc(writer, a),
        "CgEd" => return Some(write_cged(writer, a)),
        _ => return None,
    }
    Some(Ok(()))
}

fn write_brit(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::Brightness(b)) => b,
        _ => return,
    };
    write_int16(writer, info.brightness.unwrap_or(0.0) as i16);
    write_int16(writer, info.contrast.unwrap_or(0.0) as i16);
    write_int16(writer, info.mean_value.unwrap_or(127.0) as i16);
    write_uint8(writer, if info.lab_color_only == Some(true) { 1 } else { 0 });
    write_zeros(writer, 1);
}

/// Writes the `levl` payload: version 2 followed by 63 fixed-size channel
/// records in the order rgb, red, green, blue and then 59 default records.
///
/// Missing channels are written as the neutral default so the record count
/// stays constant; a non-Levels adjustment is a no-op (the dispatcher only
/// calls this once `has` reported the key).
fn write_levl(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::Levels(l)) => l,
        _ => return,
    };
    let default = LevelsAdjustmentChannel {
        shadow_input: 0.0,
        highlight_input: 255.0,
        shadow_output: 0.0,
        highlight_output: 255.0,
        midtone_input: 1.0,
    };

    write_uint16(writer, 2); // version
    // Channel order is fixed by the format and must match `read_levl`:
    // rgb, red, green, blue. Emitting blue before green silently swaps the two
    // channels on every round trip.
    write_levels_channel(writer, info.rgb.as_ref().unwrap_or(&default));
    write_levels_channel(writer, info.red.as_ref().unwrap_or(&default));
    write_levels_channel(writer, info.green.as_ref().unwrap_or(&default));
    write_levels_channel(writer, info.blue.as_ref().unwrap_or(&default));
    for _ in 0..59 {
        write_levels_channel(writer, &default);
    }
}

fn write_curv(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::Curves(c)) => c,
        _ => return,
    };

    let rgb = info.rgb.as_ref().filter(|c| !c.is_empty());
    let red = info.red.as_ref().filter(|c| !c.is_empty());
    let green = info.green.as_ref().filter(|c| !c.is_empty());
    let blue = info.blue.as_ref().filter(|c| !c.is_empty());

    let mut channels: u16 = 0;
    let mut channel_count: u16 = 0;
    if rgb.is_some() {
        channels |= 1;
        channel_count += 1;
    }
    if red.is_some() {
        channels |= 2;
        channel_count += 1;
    }
    if green.is_some() {
        channels |= 4;
        channel_count += 1;
    }
    if blue.is_some() {
        channels |= 8;
        channel_count += 1;
    }

    write_uint8(writer, 0);
    write_uint16(writer, 1); // version
    write_uint16(writer, 0);
    write_uint16(writer, channels);

    if let Some(c) = rgb {
        write_curve_channel(writer, c);
    }
    if let Some(c) = red {
        write_curve_channel(writer, c);
    }
    if let Some(c) = green {
        write_curve_channel(writer, c);
    }
    if let Some(c) = blue {
        write_curve_channel(writer, c);
    }

    write_signature(writer, "Crv ");
    write_uint16(writer, 4); // version
    write_uint16(writer, 0);
    write_uint16(writer, channel_count);

    if let Some(c) = rgb {
        write_uint16(writer, 0);
        write_curve_channel(writer, c);
    }
    if let Some(c) = red {
        write_uint16(writer, 1);
        write_curve_channel(writer, c);
    }
    if let Some(c) = green {
        write_uint16(writer, 2);
        write_curve_channel(writer, c);
    }
    if let Some(c) = blue {
        write_uint16(writer, 3);
        write_curve_channel(writer, c);
    }
}

fn write_expa(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::Exposure(e)) => e,
        _ => return,
    };
    write_uint16(writer, 1); // version
    write_float32(writer, info.exposure.unwrap_or(0.0) as f32);
    write_float32(writer, info.offset.unwrap_or(0.0) as f32);
    write_float32(writer, info.gamma.unwrap_or(0.0) as f32);
    write_zeros(writer, 2);
}

fn write_viba(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::Vibrance(v)) => v,
        _ => return,
    };
    let mut desc = Descriptor::new("", "null");
    if let Some(v) = info.vibrance {
        desc.set("vibrance", DescriptorValue::Integer(v as i32));
    }
    if let Some(v) = info.saturation {
        desc.set("Strt", DescriptorValue::Integer(v as i32));
    }
    write_version_and_descriptor(writer, &desc);
}

fn write_hue2(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::HueSaturation(h)) => h,
        _ => return,
    };
    write_uint16(writer, 2); // version
    write_hue_channel(writer, info.master.as_ref());
    write_hue_channel(writer, info.reds.as_ref());
    write_hue_channel(writer, info.yellows.as_ref());
    write_hue_channel(writer, info.greens.as_ref());
    write_hue_channel(writer, info.cyans.as_ref());
    write_hue_channel(writer, info.blues.as_ref());
    write_hue_channel(writer, info.magentas.as_ref());
}

fn write_blnc(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::ColorBalance(b)) => b,
        _ => return,
    };
    write_color_balance(writer, info.shadows.as_ref());
    write_color_balance(writer, info.midtones.as_ref());
    write_color_balance(writer, info.highlights.as_ref());
    write_uint8(writer, if info.preserve_luminosity == Some(true) { 1 } else { 0 });
    write_zeros(writer, 1);
}

fn write_blwh(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::BlackAndWhite(b)) => b,
        _ => return,
    };
    let mut desc = Descriptor::new("", "null");
    desc.set("Rd  ", DescriptorValue::Integer(info.reds.unwrap_or(0.0) as i32));
    desc.set("Yllw", DescriptorValue::Integer(info.yellows.unwrap_or(0.0) as i32));
    desc.set("Grn ", DescriptorValue::Integer(info.greens.unwrap_or(0.0) as i32));
    desc.set("Cyn ", DescriptorValue::Integer(info.cyans.unwrap_or(0.0) as i32));
    desc.set("Bl  ", DescriptorValue::Integer(info.blues.unwrap_or(0.0) as i32));
    desc.set("Mgnt", DescriptorValue::Integer(info.magentas.unwrap_or(0.0) as i32));
    desc.set("useTint", DescriptorValue::Boolean(info.use_tint == Some(true)));
    desc.set(
        "tintColor",
        DescriptorValue::Descriptor(serialize_color(info.tint_color.as_ref())),
    );
    desc.set(
        "bwPresetKind",
        DescriptorValue::Integer(info.preset.preset_kind.unwrap_or(0.0) as i32),
    );
    desc.set(
        "blackAndWhitePresetFileName",
        DescriptorValue::Text(info.preset.preset_file_name.clone().unwrap_or_default()),
    );
    write_version_and_descriptor(writer, &desc);
}

fn write_phfl(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::PhotoFilter(p)) => p,
        _ => return,
    };
    write_uint16(writer, 2); // version
    let default = Color::Lab(Lab { l: 0.0, a: 0.0, b: 0.0 });
    write_color(writer, Some(info.color.as_ref().unwrap_or(&default)));
    write_uint32(writer, (info.density.unwrap_or(0.0) * 100.0) as u32);
    write_uint8(writer, if info.preserve_luminosity == Some(true) { 1 } else { 0 });
    write_zeros(writer, 3);
}

fn write_mixr(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::ChannelMixer(m)) => m,
        _ => return,
    };
    write_uint16(writer, 1); // version
    write_uint16(writer, if info.monochrome == Some(true) { 1 } else { 0 });

    if info.monochrome == Some(true) {
        write_mixr_channel(writer, info.gray.as_ref());
        write_zeros(writer, 3 * 5 * 2);
    } else {
        write_mixr_channel(writer, info.red.as_ref());
        write_mixr_channel(writer, info.green.as_ref());
        write_mixr_channel(writer, info.blue.as_ref());
        write_mixr_channel(writer, info.gray.as_ref());
    }
}

fn write_clrl(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::ColorLookup(c)) => c,
        _ => return,
    };
    let mut desc = Descriptor::new("", "null");

    if let Some(t) = info.lookup_type {
        desc.set("lookupType", DescriptorValue::Enum(color_lookup_type_encode(t)));
    }
    if let Some(n) = &info.name {
        desc.set("Nm  ", DescriptorValue::Text(n.clone()));
    }
    if let Some(d) = info.dither {
        desc.set("Dthr", DescriptorValue::Boolean(d));
    }
    if let Some(p) = &info.profile {
        desc.set("profile", DescriptorValue::RawData(p.clone()));
    }
    if let Some(f) = info.lut_format {
        desc.set("LUTFormat", DescriptorValue::Enum(lut_format_encode(f)));
    }
    if let Some(o) = info.data_order {
        desc.set("dataOrder", DescriptorValue::Enum(color_lookup_order_encode(o)));
    }
    if let Some(o) = info.table_order {
        desc.set("tableOrder", DescriptorValue::Enum(color_lookup_order_encode(o)));
    }
    if let Some(d) = &info.lut3d_file_data {
        desc.set("LUT3DFileData", DescriptorValue::RawData(d.clone()));
    }
    if let Some(n) = &info.lut3d_file_name {
        desc.set("LUT3DFileName", DescriptorValue::Text(n.clone()));
    }

    write_uint16(writer, 1); // version
    write_version_and_descriptor(writer, &desc);
}

fn write_post(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::Posterize(p)) => p,
        _ => return,
    };
    write_uint16(writer, info.levels.unwrap_or(4.0) as u16);
    write_zeros(writer, 2);
}

fn write_thrs(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::Threshold(t)) => t,
        _ => return,
    };
    write_uint16(writer, info.level.unwrap_or(128.0) as u16);
    write_zeros(writer, 2);
}

fn write_grdm(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::GradientMap(g)) => g,
        _ => return,
    };

    write_uint16(writer, if info.method.is_some() { 3 } else { 1 }); // version
    write_uint8(writer, if info.reverse == Some(true) { 1 } else { 0 });
    write_uint8(writer, if info.dither == Some(true) { 1 } else { 0 });

    if let Some(m) = info.method {
        write_signature(writer, gradient_interpolation_method_encode(m));
    }

    write_unicode_string_with_padding(writer, info.name.as_deref().unwrap_or(""));

    let empty_color: Vec<ColorStop> = Vec::new();
    let color_stops = info.color_stops.as_ref().unwrap_or(&empty_color);
    write_uint16(writer, color_stops.len() as u16);

    let interpolation = (info.smoothness.unwrap_or(1.0) * 4096.0).round();

    for s in color_stops {
        write_uint32(writer, (s.location * interpolation).round() as u32);
        write_uint32(writer, (s.midpoint * 100.0).round() as u32);
        write_color(writer, Some(&s.color));
        write_zeros(writer, 2);
    }

    let empty_opacity: Vec<OpacityStop> = Vec::new();
    let opacity_stops = info.opacity_stops.as_ref().unwrap_or(&empty_opacity);
    write_uint16(writer, opacity_stops.len() as u16);

    for s in opacity_stops {
        write_uint32(writer, (s.location * interpolation).round() as u32);
        write_uint32(writer, (s.midpoint * 100.0).round() as u32);
        write_uint16(writer, (s.opacity * 0xff as f64).round() as u16);
    }

    write_uint16(writer, 2); // expansion count
    write_uint16(writer, interpolation as u16);
    write_uint16(writer, 32); // length
    write_uint16(writer, if info.gradient_type == GradientMapType::Noise { 1 } else { 0 });
    write_uint32(writer, info.random_seed.unwrap_or(0.0) as u32);
    write_uint16(writer, if info.add_transparency == Some(true) { 1 } else { 0 });
    write_uint16(writer, if info.restrict_colors == Some(true) { 1 } else { 0 });
    write_uint32(writer, (info.roughness.unwrap_or(1.0) * 4096.0).round() as u32);

    let color_model = grdm_color_model_index(info.color_model.unwrap_or(GradientColorModel::Rgb));
    write_uint16(writer, color_model);

    for i in 0..4 {
        let v = info.min.as_ref().and_then(|m| m.get(i)).copied().unwrap_or(0.0);
        write_uint16(writer, (v * 0x8000 as f64).round() as u16);
    }
    for i in 0..4 {
        let v = info.max.as_ref().and_then(|m| m.get(i)).copied().unwrap_or(0.0);
        write_uint16(writer, (v * 0x8000 as f64).round() as u16);
    }

    write_zeros(writer, 4);
}

fn write_selc(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) {
    let info = match a {
        Some(AdjustmentLayer::SelectiveColor(s)) => s,
        _ => return,
    };
    write_uint16(writer, 1); // version
    write_uint16(writer, if info.mode == Some(SelectiveColorMode::Absolute) { 1 } else { 0 });
    write_zeros(writer, 8);
    write_selective_colors(writer, info.reds.as_ref());
    write_selective_colors(writer, info.yellows.as_ref());
    write_selective_colors(writer, info.greens.as_ref());
    write_selective_colors(writer, info.cyans.as_ref());
    write_selective_colors(writer, info.blues.as_ref());
    write_selective_colors(writer, info.magentas.as_ref());
    write_selective_colors(writer, info.whites.as_ref());
    write_selective_colors(writer, info.neutrals.as_ref());
    write_selective_colors(writer, info.blacks.as_ref());
}

fn write_cged(writer: &mut PsdWriter, a: Option<&AdjustmentLayer>) -> ReadResult<()> {
    match a {
        Some(AdjustmentLayer::Levels(l)) => {
            write_preset_descriptor(writer, "presetKind", "presetFileName", &l.preset);
        }
        Some(AdjustmentLayer::Exposure(e)) => {
            write_preset_descriptor(writer, "presetKind", "presetFileName", &e.preset);
        }
        Some(AdjustmentLayer::HueSaturation(h)) => {
            write_preset_descriptor(writer, "presetKind", "presetFileName", &h.preset);
        }
        Some(AdjustmentLayer::Curves(c)) => {
            write_preset_descriptor(writer, "curvesPresetKind", "curvesPresetFileName", &c.preset);
        }
        Some(AdjustmentLayer::ChannelMixer(m)) => {
            write_preset_descriptor(writer, "mixerPresetKind", "mixerPresetFileName", &m.preset);
        }
        Some(AdjustmentLayer::Brightness(b)) => {
            let mut desc = Descriptor::new("", "null");
            desc.set("Vrsn", DescriptorValue::Integer(1));
            desc.set("Brgh", DescriptorValue::Integer(b.brightness.unwrap_or(0.0) as i32));
            desc.set("Cntr", DescriptorValue::Integer(b.contrast.unwrap_or(0.0) as i32));
            desc.set("means", DescriptorValue::Integer(b.mean_value.unwrap_or(127.0) as i32));
            desc.set("Lab ", DescriptorValue::Boolean(b.lab_color_only == Some(true)));
            desc.set("useLegacy", DescriptorValue::Boolean(b.use_legacy == Some(true)));
            desc.set("Auto", DescriptorValue::Boolean(b.auto == Some(true)));
            write_version_and_descriptor(writer, &desc);
        }
        _ => {
            return Err(ReadError::StrictViolation("Unhandled CgEd case".to_string()));
        }
    }
    Ok(())
}

fn write_preset_descriptor(
    writer: &mut PsdWriter,
    kind_key: &str,
    name_key: &str,
    preset: &PresetInfo,
) {
    let mut desc = Descriptor::new("", "null");
    desc.set("Vrsn", DescriptorValue::Integer(1));
    desc.set(kind_key, DescriptorValue::Integer(preset.preset_kind.unwrap_or(1.0) as i32));
    desc.set(
        name_key,
        DescriptorValue::Text(preset.preset_file_name.clone().unwrap_or_default()),
    );
    write_version_and_descriptor(writer, &desc);
}

// ===========================================================================
// Helpers — preset extraction / descriptor accessors
// ===========================================================================

/// Извлекает уже прочитанный `PresetInfo` из текущего adjustment (если он
/// несёт preset) — зеркало spread `...target.adjustment as PresetInfo`.
fn existing_preset(info: &LayerAdditionalInfo) -> PresetInfo {
    match info.adjustment.as_ref() {
        Some(AdjustmentLayer::Levels(a)) => a.preset.clone(),
        Some(AdjustmentLayer::Curves(a)) => a.preset.clone(),
        Some(AdjustmentLayer::Exposure(a)) => a.preset.clone(),
        Some(AdjustmentLayer::HueSaturation(a)) => a.preset.clone(),
        Some(AdjustmentLayer::ChannelMixer(a)) => a.preset.clone(),
        _ => PresetInfo::default(),
    }
}

fn desc_double(desc: &Descriptor, key: &str) -> Option<f64> {
    match desc.get(key)? {
        DescriptorValue::Double(v) => Some(*v),
        DescriptorValue::Integer(v) => Some(*v as f64),
        DescriptorValue::UnitDouble(u) => Some(u.value),
        _ => None,
    }
}

fn desc_bool(desc: &Descriptor, key: &str) -> Option<bool> {
    match desc.get(key)? {
        DescriptorValue::Boolean(b) => Some(*b),
        _ => None,
    }
}

fn desc_text(desc: &Descriptor, key: &str) -> Option<String> {
    match desc.get(key)? {
        DescriptorValue::Text(t) => Some(t.clone()),
        _ => None,
    }
}

fn desc_raw(desc: &Descriptor, key: &str) -> Option<Vec<u8>> {
    match desc.get(key)? {
        DescriptorValue::RawData(d) => Some(d.clone()),
        _ => None,
    }
}

/// Возвращает value-часть `"type.value"` enum-значения дескриптора.
fn desc_enum_value(desc: &Descriptor, key: &str) -> Option<String> {
    match desc.get(key)? {
        DescriptorValue::Enum(s) => Some(s.rsplit('.').next().unwrap_or(s).to_string()),
        _ => None,
    }
}

// ===========================================================================
// CONSOLIDATION GAP — enum codecs (createEnum decode/encode), inline
// ===========================================================================

/// Mirror `colorLookupType.decode`. Accepts the historical code and, since
/// Photoshop 2026, the long-form map key (`3dlut`); unknown values fall back to the
/// `3dlut` default, as upstream's `def` does.
fn color_lookup_type_decode(value: &str) -> Option<ColorLookupType> {
    match value {
        "3DLUT" | "3dlut" => Some(ColorLookupType::Lut3D),
        "abstractProfile" => Some(ColorLookupType::AbstractProfile),
        "deviceLinkProfile" => Some(ColorLookupType::DeviceLinkProfile),
        _ => Some(ColorLookupType::Lut3D), // default '3dlut'
    }
}

fn color_lookup_type_encode(value: ColorLookupType) -> String {
    let v = match value {
        ColorLookupType::Lut3D => "3DLUT",
        ColorLookupType::AbstractProfile => "abstractProfile",
        ColorLookupType::DeviceLinkProfile => "deviceLinkProfile",
    };
    format!("colorLookupType.{v}")
}

/// Mirror `LUTFormatType.decode`; the second arm of each pattern is the long-form
/// map key Photoshop 2026 writes instead of the historical code.
fn lut_format_decode(value: &str) -> Option<LutFormat> {
    match value {
        "LUTFormatLOOK" | "look" => Some(LutFormat::Look),
        "LUTFormatCUBE" | "cube" => Some(LutFormat::Cube),
        "LUTFormat3DL" | "3dl" => Some(LutFormat::ThreeDl),
        _ => Some(LutFormat::Look), // default 'look'
    }
}

fn lut_format_encode(value: LutFormat) -> String {
    let v = match value {
        LutFormat::Look => "LUTFormatLOOK",
        LutFormat::Cube => "LUTFormatCUBE",
        LutFormat::ThreeDl => "LUTFormat3DL",
    };
    format!("LUTFormatType.{v}")
}

/// Mirror `colorLookupOrder.decode`; the second arm of each pattern is the long-form
/// map key Photoshop 2026 writes instead of the historical code.
fn color_lookup_order_decode(value: &str) -> Option<RgbBgrOrder> {
    match value {
        "rgbOrder" | "rgb" => Some(RgbBgrOrder::Rgb),
        "bgrOrder" | "bgr" => Some(RgbBgrOrder::Bgr),
        _ => Some(RgbBgrOrder::Rgb), // default 'rgb'
    }
}

fn color_lookup_order_encode(value: RgbBgrOrder) -> String {
    let v = match value {
        RgbBgrOrder::Rgb => "rgbOrder",
        RgbBgrOrder::Bgr => "bgrOrder",
    };
    format!("colorLookupOrder.{v}")
}

/// Mirror `gradientInterpolationMethodType.decode`; the second arm of each pattern is
/// the long-form map key Photoshop 2026 writes instead of the historical code.
fn gradient_interpolation_method_decode(value: &str) -> Option<InterpolationMethod> {
    match value {
        "Perc" | "perceptual" => Some(InterpolationMethod::Perceptual),
        "Lnr " | "linear" => Some(InterpolationMethod::Linear),
        "Gcls" | "classic" => Some(InterpolationMethod::Classic),
        "Smoo" | "smooth" => Some(InterpolationMethod::Smooth),
        _ => Some(InterpolationMethod::Perceptual), // default 'perceptual'
    }
}

fn gradient_interpolation_method_encode(value: InterpolationMethod) -> &'static str {
    match value {
        InterpolationMethod::Perceptual => "Perc",
        InterpolationMethod::Linear => "Lnr ",
        InterpolationMethod::Classic => "Gcls",
        InterpolationMethod::Smooth => "Smoo",
    }
}

fn grdm_color_model_from_index(index: usize) -> GradientColorModel {
    match GRDM_COLOR_MODELS.get(index).copied().unwrap_or("rgb") {
        "hsb" => GradientColorModel::Hsb,
        "lab" => GradientColorModel::Lab,
        _ => GradientColorModel::Rgb,
    }
}

fn grdm_color_model_index(model: GradientColorModel) -> u16 {
    let name = match model {
        GradientColorModel::Rgb => "rgb",
        GradientColorModel::Hsb => "hsb",
        GradientColorModel::Lab => "lab",
        // `grdm`'s binary color-model table has no `hsl` slot (upstream types the
        // field as 'rgb'|'hsb'|'lab'); the lookup below misses and writes the rgb
        // slot, which is upstream's `indexOf(...) === -1 -> 3` behaviour.
        GradientColorModel::Hsl => "hsl",
    };
    match GRDM_COLOR_MODELS.iter().position(|m| *m == name) {
        Some(i) => i as u16,
        None => 3, // upstream: indexOf === -1 -> 3
    }
}

// descriptor color codec (parseColor / serializeColor) — канонически в descriptor.rs.
use crate::descriptor::{parse_color, serialize_color};

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::Rgb;
    use crate::psd::ReadOptions;
    use crate::reader::PsdReader;
    use crate::writer::{create_writer, get_writer_buffer};

    fn round_trip(key: &str, info: &LayerAdditionalInfo) -> LayerAdditionalInfo {
        let opts = WriteOptionsStub::default_opts();
        let mut wctx = WriteCtx::new(&opts.0, false);
        let mut writer = create_writer(256);
        let r = write(key, &mut writer, info, &mut wctx);
        assert!(r.is_some(), "write returned None for key {key}");
        r.unwrap().expect("write ok");
        let bytes = get_writer_buffer(&writer);

        let ropts = ReadOptions::default();
        let mut rctx = ReadCtx { options: &ropts, large: false };
        let mut reader = PsdReader::new(&bytes, None, None);
        let total = bytes.len();
        let mut out = LayerAdditionalInfo::default();
        let left = move |r: &PsdReader| total - r.offset;
        let handled = read(key, &mut reader, &mut out, &left, &mut rctx).expect("read ok");
        assert_eq!(handled, Some(()), "read not handled for key {key}");
        out
    }

    // small wrapper so we can build a WriteOptions without depending on its
    // exact public constructor surface in the test.
    struct WriteOptionsStub(crate::psd::WriteOptions);
    impl WriteOptionsStub {
        fn default_opts() -> Self {
            WriteOptionsStub(crate::psd::WriteOptions::default())
        }
    }

    #[test]
    fn round_trip_levl_binary() {
        let info = LayerAdditionalInfo {
            adjustment: Some(AdjustmentLayer::Levels(LevelsAdjustment {
                preset: PresetInfo::default(),
                rgb: Some(LevelsAdjustmentChannel {
                    shadow_input: 5.0,
                    highlight_input: 250.0,
                    shadow_output: 10.0,
                    highlight_output: 245.0,
                    midtone_input: 1.2,
                }),
                red: Some(LevelsAdjustmentChannel {
                    shadow_input: 1.0,
                    highlight_input: 254.0,
                    shadow_output: 0.0,
                    highlight_output: 255.0,
                    midtone_input: 0.9,
                }),
                green: None,
                blue: None,
            })),
            ..LayerAdditionalInfo::default()
        };

        let out = round_trip("levl", &info);
        match out.adjustment {
            Some(AdjustmentLayer::Levels(l)) => {
                let rgb = l.rgb.expect("rgb");
                assert_eq!(rgb.shadow_input, 5.0);
                assert_eq!(rgb.highlight_input, 250.0);
                assert_eq!(rgb.shadow_output, 10.0);
                assert_eq!(rgb.highlight_output, 245.0);
                assert!((rgb.midtone_input - 1.2).abs() < 1e-9);
                let red = l.red.expect("red");
                assert!((red.midtone_input - 0.9).abs() < 1e-9);
                // green/blue defaulted on write -> read back as defaults
                let green = l.green.expect("green default");
                assert_eq!(green.highlight_input, 255.0);
                assert_eq!(green.midtone_input, 1.0);
            }
            other => panic!("expected levels, got {other:?}"),
        }
    }

    /// Guards the `levl` channel order: the writer must emit rgb, red, green,
    /// blue in that order, otherwise green and blue are swapped on every round
    /// trip. Each channel carries a distinct marker value so a swap is visible.
    #[test]
    fn round_trip_levl_keeps_channel_order() {
        let channel = |marker: f64| LevelsAdjustmentChannel {
            shadow_input: marker,
            highlight_input: 255.0 - marker,
            shadow_output: marker * 2.0,
            highlight_output: 255.0,
            midtone_input: 1.0,
        };

        let info = LayerAdditionalInfo {
            adjustment: Some(AdjustmentLayer::Levels(LevelsAdjustment {
                preset: PresetInfo::default(),
                rgb: Some(channel(1.0)),
                red: Some(channel(2.0)),
                green: Some(channel(3.0)),
                blue: Some(channel(4.0)),
            })),
            ..LayerAdditionalInfo::default()
        };

        let out = round_trip("levl", &info);
        match out.adjustment {
            Some(AdjustmentLayer::Levels(l)) => {
                assert_eq!(l.rgb.expect("rgb").shadow_input, 1.0);
                assert_eq!(l.red.expect("red").shadow_input, 2.0);
                assert_eq!(l.green.expect("green").shadow_input, 3.0);
                assert_eq!(l.blue.expect("blue").shadow_input, 4.0);
            }
            other => panic!("expected levels, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_brit_binary() {
        let info = LayerAdditionalInfo {
            adjustment: Some(AdjustmentLayer::Brightness(BrightnessAdjustment {
                brightness: Some(20.0),
                contrast: Some(-15.0),
                mean_value: Some(127.0),
                lab_color_only: Some(true),
                use_legacy: Some(true),
                auto: None,
            })),
            ..LayerAdditionalInfo::default()
        };

        let out = round_trip("brit", &info);
        match out.adjustment {
            Some(AdjustmentLayer::Brightness(b)) => {
                assert_eq!(b.brightness, Some(20.0));
                assert_eq!(b.contrast, Some(-15.0));
                assert_eq!(b.mean_value, Some(127.0));
                assert_eq!(b.lab_color_only, Some(true));
                assert_eq!(b.use_legacy, Some(true));
            }
            other => panic!("expected brightness, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_vibance_descriptor() {
        let info = LayerAdditionalInfo {
            adjustment: Some(AdjustmentLayer::Vibrance(VibranceAdjustment {
                vibrance: Some(30.0),
                saturation: Some(-10.0),
            })),
            ..LayerAdditionalInfo::default()
        };

        let out = round_trip("vibA", &info);
        match out.adjustment {
            Some(AdjustmentLayer::Vibrance(v)) => {
                assert_eq!(v.vibrance, Some(30.0));
                assert_eq!(v.saturation, Some(-10.0));
            }
            other => panic!("expected vibrance, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_blwh_descriptor() {
        let adj = BlackAndWhiteAdjustment {
            reds: Some(40.0),
            yellows: Some(60.0),
            greens: Some(40.0),
            cyans: Some(60.0),
            blues: Some(20.0),
            magentas: Some(80.0),
            use_tint: Some(true),
            tint_color: Some(Color::Rgb(Rgb { r: 225.0, g: 211.0, b: 179.0 })),
            preset: PresetInfo {
                preset_kind: Some(1.0),
                preset_file_name: Some(String::new()),
            },
        };
        let info = LayerAdditionalInfo {
            adjustment: Some(AdjustmentLayer::BlackAndWhite(adj)),
            ..LayerAdditionalInfo::default()
        };

        let out = round_trip("blwh", &info);
        match out.adjustment {
            Some(AdjustmentLayer::BlackAndWhite(b)) => {
                assert_eq!(b.reds, Some(40.0));
                assert_eq!(b.magentas, Some(80.0));
                assert_eq!(b.use_tint, Some(true));
                assert_eq!(b.preset.preset_kind, Some(1.0));
                match b.tint_color {
                    Some(Color::Rgb(c)) => {
                        assert_eq!(c.r, 225.0);
                        assert_eq!(c.g, 211.0);
                        assert_eq!(c.b, 179.0);
                    }
                    other => panic!("expected rgb tint, got {other:?}"),
                }
            }
            other => panic!("expected black & white, got {other:?}"),
        }
    }

    #[test]
    fn inline_enum_codecs_accept_photoshop_2026_long_form() {
        // Photoshop 2026 writes the long-form map KEY instead of the historical code;
        // before this these fell through to the default and lost the real value.
        assert_eq!(lut_format_decode("LUTFormatCUBE"), Some(LutFormat::Cube));
        assert_eq!(lut_format_decode("cube"), Some(LutFormat::Cube));
        assert_eq!(lut_format_decode("3dl"), Some(LutFormat::ThreeDl));
        assert_eq!(color_lookup_order_decode("bgrOrder"), Some(RgbBgrOrder::Bgr));
        assert_eq!(color_lookup_order_decode("bgr"), Some(RgbBgrOrder::Bgr));
        assert_eq!(color_lookup_type_decode("3dlut"), Some(ColorLookupType::Lut3D));
        assert_eq!(
            gradient_interpolation_method_decode("classic"),
            Some(InterpolationMethod::Classic)
        );
        assert_eq!(
            gradient_interpolation_method_decode("Gcls"),
            Some(InterpolationMethod::Classic)
        );
    }
}
