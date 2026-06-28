/*
File: crates/ag-psd/src/effects_helpers.rs

Purpose:
вспомогательные функции для legacy-эффектов слоёв (формат подписи 'lrFX':
drop shadow, inner shadow, outer/inner glow, bevel, solid fill, плюс общий
заголовок и проверки версий).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/effectsHelpers.ts` (разбиение 1:1).

Main responsibilities:
- читать/писать побайтовые раскладки каждого типа эффекта;
- сохранять точный порядок полей, единицы измерения и арифметику.

DEPENDENCY GAPS (см. отчёт):
- `read_color` в `crate::reader` ещё не портирован — здесь реализован локальный
  `read_color`, зеркало `readColor` из `psdReader.ts`. Когда reader получит
  публичный `read_color`, локальную копию можно удалить.
*/

use crate::helpers::{from_blend_mode, to_blend_mode};
use crate::psd::{
    BevelDirection, BevelStyle, LayerEffectBevel,
    LayerEffectInnerGlow, LayerEffectShadow, LayerEffectSolidFill, LayerEffectsInfo,
    LayerEffectsOuterGlow, Units, UnitsValue,
};
use crate::reader::{
    check_signature, read_color, read_fixed_point32, read_signature, read_uint16,
    read_uint32, read_uint8, skip_bytes, PsdReader, ReadError, ReadResult,
};
use crate::writer::{
    write_color, write_fixed_point32, write_signature, write_uint16, write_uint32, write_uint8,
    write_zeros, PsdWriter,
};

/// Зеркало `const bevelStyles: BevelStyle[]` — индекс 0 «пустой» (`undefined`).
fn bevel_style_from_index(index: u8) -> Option<BevelStyle> {
    match index {
        1 => Some(BevelStyle::OuterBevel),
        2 => Some(BevelStyle::InnerBevel),
        3 => Some(BevelStyle::Emboss),
        4 => Some(BevelStyle::PillowEmboss),
        5 => Some(BevelStyle::StrokeEmboss),
        _ => None,
    }
}

/// Зеркало `bevelStyles.indexOf(style)`; для отсутствующего возвращает -1.
fn bevel_style_index(style: Option<BevelStyle>) -> i32 {
    match style {
        Some(BevelStyle::OuterBevel) => 1,
        Some(BevelStyle::InnerBevel) => 2,
        Some(BevelStyle::Emboss) => 3,
        Some(BevelStyle::PillowEmboss) => 4,
        Some(BevelStyle::StrokeEmboss) => 5,
        None => -1,
    }
}

/// Зеркало `readBlendMode` — `8BIM` + 4-байтовый ключ, дефолт `'normal'`.
fn read_blend_mode(reader: &mut PsdReader) -> ReadResult<crate::psd::BlendMode> {
    check_signature(reader, "8BIM", None)?;
    let sig = read_signature(reader)?;
    Ok(to_blend_mode(&sig).unwrap_or(crate::psd::BlendMode::Normal))
}

/// Зеркало `writeBlendMode` — `8BIM` + ключ режима, дефолт `'norm'`.
fn write_blend_mode(writer: &mut PsdWriter, mode: Option<crate::psd::BlendMode>) {
    write_signature(writer, "8BIM");
    match mode {
        Some(m) => write_signature(writer, from_blend_mode(m)),
        None => write_signature(writer, "norm"),
    }
}

/// Зеркало `readFixedPoint8` — `readUint8 / 0xff`.
fn read_fixed_point8(reader: &mut PsdReader) -> ReadResult<f64> {
    Ok(read_uint8(reader)? as f64 / 0xff as f64)
}

/// Зеркало `writeFixedPoint8` — `Math.round(value * 0xff) | 0`.
fn write_fixed_point8(writer: &mut PsdWriter, value: f64) {
    // `| 0` в JS — приведение к int32; round даёт безопасное значение для u8.
    write_uint8(writer, ((value * 0xff as f64).round() as i64 as i32) as u8);
}

/// Зеркало `readEffects(reader)`.
pub fn read_effects(reader: &mut PsdReader) -> ReadResult<LayerEffectsInfo> {
    let version = read_uint16(reader)?;
    if version != 0 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid effects layer version: {}",
            version
        )));
    }

    let effects_count = read_uint16(reader)?;
    let mut effects = LayerEffectsInfo::default();

    for _ in 0..effects_count {
        check_signature(reader, "8BIM", None)?;
        let type_ = read_signature(reader)?;

        match type_.as_str() {
            "cmnS" => {
                // common state
                let size = read_uint32(reader)?;
                let version = read_uint32(reader)?;
                let visible = read_uint8(reader)? != 0;
                skip_bytes(reader, 2);

                if size != 7 || version != 0 || !visible {
                    return Err(ReadError::StrictViolation(
                        "Invalid effects common state".to_string(),
                    ));
                }
            }
            "dsdw" | "isdw" => {
                // drop shadow / inner shadow
                let block_size = read_uint32(reader)?;
                let version = read_uint32(reader)?;

                if block_size != 41 && block_size != 51 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid shadow size: {}",
                        block_size
                    )));
                }
                if version != 0 && version != 2 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid shadow version: {}",
                        version
                    )));
                }

                let size = read_fixed_point32(reader)?;
                read_fixed_point32(reader)?; // intensity
                let angle = read_fixed_point32(reader)?;
                let distance = read_fixed_point32(reader)?;
                let color = read_color(reader)?;
                let blend_mode = read_blend_mode(reader)?;
                let enabled = read_uint8(reader)? != 0;
                let use_global_light = read_uint8(reader)? != 0;
                let opacity = read_fixed_point8(reader)?;
                if block_size >= 51 {
                    read_color(reader)?; // native color
                }

                let shadow_info = LayerEffectShadow {
                    size: Some(UnitsValue {
                        units: Units::Pixels,
                        value: size,
                    }),
                    distance: Some(UnitsValue {
                        units: Units::Pixels,
                        value: distance,
                    }),
                    angle: Some(angle),
                    color: Some(color),
                    blend_mode: Some(blend_mode),
                    enabled: Some(enabled),
                    use_global_light: Some(use_global_light),
                    opacity: Some(opacity),
                    ..LayerEffectShadow::default()
                };

                if type_ == "dsdw" {
                    effects.drop_shadow = Some(vec![shadow_info]);
                } else {
                    effects.inner_shadow = Some(vec![shadow_info]);
                }
            }
            "oglw" => {
                // outer glow
                let block_size = read_uint32(reader)?;
                let version = read_uint32(reader)?;

                if block_size != 32 && block_size != 42 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid outer glow size: {}",
                        block_size
                    )));
                }
                if version != 0 && version != 2 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid outer glow version: {}",
                        version
                    )));
                }

                let size = read_fixed_point32(reader)?;
                read_fixed_point32(reader)?; // intensity
                let color = read_color(reader)?;
                let blend_mode = read_blend_mode(reader)?;
                let enabled = read_uint8(reader)? != 0;
                let opacity = read_fixed_point8(reader)?;
                if block_size >= 42 {
                    read_color(reader)?; // native color
                }

                effects.outer_glow = Some(LayerEffectsOuterGlow {
                    size: Some(UnitsValue {
                        units: Units::Pixels,
                        value: size,
                    }),
                    color: Some(color),
                    blend_mode: Some(blend_mode),
                    enabled: Some(enabled),
                    opacity: Some(opacity),
                    ..LayerEffectsOuterGlow::default()
                });
            }
            "iglw" => {
                // inner glow
                let block_size = read_uint32(reader)?;
                let version = read_uint32(reader)?;

                if block_size != 32 && block_size != 43 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid inner glow size: {}",
                        block_size
                    )));
                }
                if version != 0 && version != 2 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid inner glow version: {}",
                        version
                    )));
                }

                let size = read_fixed_point32(reader)?;
                read_fixed_point32(reader)?; // intensity
                let color = read_color(reader)?;
                let blend_mode = read_blend_mode(reader)?;
                let enabled = read_uint8(reader)? != 0;
                let opacity = read_fixed_point8(reader)?;

                if block_size >= 43 {
                    read_uint8(reader)?; // inverted
                    read_color(reader)?; // native color
                }

                effects.inner_glow = Some(LayerEffectInnerGlow {
                    size: Some(UnitsValue {
                        units: Units::Pixels,
                        value: size,
                    }),
                    color: Some(color),
                    blend_mode: Some(blend_mode),
                    enabled: Some(enabled),
                    opacity: Some(opacity),
                    ..LayerEffectInnerGlow::default()
                });
            }
            "bevl" => {
                // bevel
                let block_size = read_uint32(reader)?;
                let version = read_uint32(reader)?;

                if block_size != 58 && block_size != 78 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid bevel size: {}",
                        block_size
                    )));
                }
                if version != 0 && version != 2 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid bevel version: {}",
                        version
                    )));
                }

                let angle = read_fixed_point32(reader)?;
                let strength = read_fixed_point32(reader)?;
                let size = read_fixed_point32(reader)?;
                let highlight_blend_mode = read_blend_mode(reader)?;
                let shadow_blend_mode = read_blend_mode(reader)?;
                let highlight_color = read_color(reader)?;
                let shadow_color = read_color(reader)?;
                // `bevelStyles[...] || 'inner bevel'`
                let style = bevel_style_from_index(read_uint8(reader)?)
                    .unwrap_or(BevelStyle::InnerBevel);
                let highlight_opacity = read_fixed_point8(reader)?;
                let shadow_opacity = read_fixed_point8(reader)?;
                let enabled = read_uint8(reader)? != 0;
                let use_global_light = read_uint8(reader)? != 0;
                let direction = if read_uint8(reader)? != 0 {
                    BevelDirection::Down
                } else {
                    BevelDirection::Up
                };

                if block_size >= 78 {
                    read_color(reader)?; // real highlight color
                    read_color(reader)?; // real shadow color
                }

                effects.bevel = Some(LayerEffectBevel {
                    size: Some(UnitsValue {
                        units: Units::Pixels,
                        value: size,
                    }),
                    angle: Some(angle),
                    strength: Some(strength),
                    highlight_blend_mode: Some(highlight_blend_mode),
                    shadow_blend_mode: Some(shadow_blend_mode),
                    highlight_color: Some(highlight_color),
                    shadow_color: Some(shadow_color),
                    style: Some(style),
                    highlight_opacity: Some(highlight_opacity),
                    shadow_opacity: Some(shadow_opacity),
                    enabled: Some(enabled),
                    use_global_light: Some(use_global_light),
                    direction: Some(direction),
                    ..LayerEffectBevel::default()
                });
            }
            "sofi" => {
                // solid fill (Photoshop 7.0)
                let size = read_uint32(reader)?;
                let version = read_uint32(reader)?;

                if size != 34 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid effects solid fill info size: {}",
                        size
                    )));
                }
                if version != 2 {
                    return Err(ReadError::StrictViolation(format!(
                        "Invalid effects solid fill info version: {}",
                        version
                    )));
                }

                let blend_mode = read_blend_mode(reader)?;
                let color = read_color(reader)?;
                let opacity = read_fixed_point8(reader)?;
                let enabled = read_uint8(reader)? != 0;
                read_color(reader)?; // native color

                effects.solid_fill = Some(vec![LayerEffectSolidFill {
                    blend_mode: Some(blend_mode),
                    color: Some(color),
                    opacity: Some(opacity),
                    enabled: Some(enabled),
                    ..LayerEffectSolidFill::default()
                }]);
            }
            other => {
                return Err(ReadError::StrictViolation(format!(
                    "Invalid effect type: '{}'",
                    other
                )));
            }
        }
    }

    Ok(effects)
}

/// Зеркало `writeShadowInfo(writer, shadow)`.
fn write_shadow_info(writer: &mut PsdWriter, shadow: &LayerEffectShadow) {
    write_uint32(writer, 51);
    write_uint32(writer, 2);
    write_fixed_point32(writer, shadow.size.map(|s| s.value).unwrap_or(0.0));
    write_fixed_point32(writer, 0.0); // intensity
    write_fixed_point32(writer, shadow.angle.unwrap_or(0.0));
    write_fixed_point32(writer, shadow.distance.map(|d| d.value).unwrap_or(0.0));
    write_color(writer, shadow.color.as_ref());
    write_blend_mode(writer, shadow.blend_mode);
    write_uint8(writer, if shadow.enabled.unwrap_or(false) { 1 } else { 0 });
    write_uint8(
        writer,
        if shadow.use_global_light.unwrap_or(false) {
            1
        } else {
            0
        },
    );
    // `shadow.opacity ?? 1`
    write_fixed_point8(writer, shadow.opacity.unwrap_or(1.0));
    write_color(writer, shadow.color.as_ref()); // native color
}

/// Зеркало `writeEffects(writer, effects)`.
pub fn write_effects(writer: &mut PsdWriter, effects: &LayerEffectsInfo) {
    let drop_shadow = effects.drop_shadow.as_ref().and_then(|v| v.first());
    let inner_shadow = effects.inner_shadow.as_ref().and_then(|v| v.first());
    let outer_glow = effects.outer_glow.as_ref();
    let inner_glow = effects.inner_glow.as_ref();
    let bevel = effects.bevel.as_ref();
    let solid_fill = effects.solid_fill.as_ref().and_then(|v| v.first());

    let mut count = 1u16;
    if drop_shadow.is_some() {
        count += 1;
    }
    if inner_shadow.is_some() {
        count += 1;
    }
    if outer_glow.is_some() {
        count += 1;
    }
    if inner_glow.is_some() {
        count += 1;
    }
    if bevel.is_some() {
        count += 1;
    }
    if solid_fill.is_some() {
        count += 1;
    }

    write_uint16(writer, 0);
    write_uint16(writer, count);

    write_signature(writer, "8BIM");
    write_signature(writer, "cmnS");
    write_uint32(writer, 7); // size
    write_uint32(writer, 0); // version
    write_uint8(writer, 1); // visible
    write_zeros(writer, 2);

    if let Some(drop_shadow) = drop_shadow {
        write_signature(writer, "8BIM");
        write_signature(writer, "dsdw");
        write_shadow_info(writer, drop_shadow);
    }

    if let Some(inner_shadow) = inner_shadow {
        write_signature(writer, "8BIM");
        write_signature(writer, "isdw");
        write_shadow_info(writer, inner_shadow);
    }

    if let Some(outer_glow) = outer_glow {
        write_signature(writer, "8BIM");
        write_signature(writer, "oglw");
        write_uint32(writer, 42);
        write_uint32(writer, 2);
        write_fixed_point32(writer, outer_glow.size.map(|s| s.value).unwrap_or(0.0));
        write_fixed_point32(writer, 0.0); // intensity
        write_color(writer, outer_glow.color.as_ref());
        write_blend_mode(writer, outer_glow.blend_mode);
        write_uint8(
            writer,
            if outer_glow.enabled.unwrap_or(false) { 1 } else { 0 },
        );
        write_fixed_point8(writer, outer_glow.opacity.unwrap_or(0.0));
        write_color(writer, outer_glow.color.as_ref());
    }

    if let Some(inner_glow) = inner_glow {
        write_signature(writer, "8BIM");
        write_signature(writer, "iglw");
        write_uint32(writer, 43);
        write_uint32(writer, 2);
        write_fixed_point32(writer, inner_glow.size.map(|s| s.value).unwrap_or(0.0));
        write_fixed_point32(writer, 0.0); // intensity
        write_color(writer, inner_glow.color.as_ref());
        write_blend_mode(writer, inner_glow.blend_mode);
        write_uint8(
            writer,
            if inner_glow.enabled.unwrap_or(false) { 1 } else { 0 },
        );
        write_fixed_point8(writer, inner_glow.opacity.unwrap_or(0.0));
        write_uint8(writer, 0); // inverted
        write_color(writer, inner_glow.color.as_ref());
    }

    if let Some(bevel) = bevel {
        write_signature(writer, "8BIM");
        write_signature(writer, "bevl");
        write_uint32(writer, 78);
        write_uint32(writer, 2);
        write_fixed_point32(writer, bevel.angle.unwrap_or(0.0));
        write_fixed_point32(writer, bevel.strength.unwrap_or(0.0));
        write_fixed_point32(writer, bevel.size.map(|s| s.value).unwrap_or(0.0));
        write_blend_mode(writer, bevel.highlight_blend_mode);
        write_blend_mode(writer, bevel.shadow_blend_mode);
        write_color(writer, bevel.highlight_color.as_ref());
        write_color(writer, bevel.shadow_color.as_ref());
        // `const style = bevelStyles.indexOf(...); writeUint8(style <= 0 ? 1 : style)`
        let style = bevel_style_index(bevel.style);
        write_uint8(writer, if style <= 0 { 1 } else { style as u8 });
        write_fixed_point8(writer, bevel.highlight_opacity.unwrap_or(0.0));
        write_fixed_point8(writer, bevel.shadow_opacity.unwrap_or(0.0));
        write_uint8(writer, if bevel.enabled.unwrap_or(false) { 1 } else { 0 });
        write_uint8(
            writer,
            if bevel.use_global_light.unwrap_or(false) {
                1
            } else {
                0
            },
        );
        write_uint8(
            writer,
            if bevel.direction == Some(BevelDirection::Down) {
                1
            } else {
                0
            },
        );
        write_color(writer, bevel.highlight_color.as_ref());
        write_color(writer, bevel.shadow_color.as_ref());
    }

    if let Some(solid_fill) = solid_fill {
        write_signature(writer, "8BIM");
        write_signature(writer, "sofi");
        write_uint32(writer, 34);
        write_uint32(writer, 2);
        write_blend_mode(writer, solid_fill.blend_mode);
        write_color(writer, solid_fill.color.as_ref());
        write_fixed_point8(writer, solid_fill.opacity.unwrap_or(0.0));
        write_uint8(
            writer,
            if solid_fill.enabled.unwrap_or(false) { 1 } else { 0 },
        );
        write_color(writer, solid_fill.color.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::psd::{BlendMode, Color, Rgb};
    use crate::reader::PsdReader;
    use crate::writer::{create_writer_default, get_writer_buffer};

    fn round_trip(effects: &LayerEffectsInfo) -> LayerEffectsInfo {
        let mut w = create_writer_default();
        write_effects(&mut w, effects);
        let buf = get_writer_buffer(&w);
        let mut r = PsdReader::new(&buf, None, None);
        read_effects(&mut r).expect("read_effects failed")
    }

    // colors are quantized to /257 on read and *257 on write; assert within 1 step.
    fn assert_rgb_close(a: &Color, r: f64, g: f64, b: f64) {
        match a {
            Color::Rgb(c) => {
                assert!((c.r - r).abs() < 1.0, "r {} vs {}", c.r, r);
                assert!((c.g - g).abs() < 1.0, "g {} vs {}", c.g, g);
                assert!((c.b - b).abs() < 1.0, "b {} vs {}", c.b, b);
            }
            other => panic!("expected Rgb, got {:?}", other),
        }
    }

    #[test]
    fn drop_shadow_round_trip() {
        let effects = LayerEffectsInfo {
            drop_shadow: Some(vec![LayerEffectShadow {
                size: Some(UnitsValue {
                    units: Units::Pixels,
                    value: 5.0,
                }),
                distance: Some(UnitsValue {
                    units: Units::Pixels,
                    value: 3.0,
                }),
                angle: Some(120.0),
                color: Some(Color::Rgb(Rgb {
                    r: 10.0,
                    g: 20.0,
                    b: 30.0,
                })),
                blend_mode: Some(BlendMode::Multiply),
                enabled: Some(true),
                use_global_light: Some(true),
                opacity: Some(0.5),
                ..LayerEffectShadow::default()
            }]),
            ..LayerEffectsInfo::default()
        };

        let out = round_trip(&effects);
        let ds = &out.drop_shadow.as_ref().unwrap()[0];
        assert_eq!(ds.size.unwrap().value, 5.0);
        assert_eq!(ds.distance.unwrap().value, 3.0);
        assert_eq!(ds.angle.unwrap(), 120.0);
        assert_eq!(ds.blend_mode, Some(BlendMode::Multiply));
        assert_eq!(ds.enabled, Some(true));
        assert_eq!(ds.use_global_light, Some(true));
        // 0.5 * 0xff = 127.5 -> round 128 -> 128/255
        assert!((ds.opacity.unwrap() - (128.0 / 255.0)).abs() < 1e-9);
        assert_rgb_close(ds.color.as_ref().unwrap(), 10.0, 20.0, 30.0);
        assert!(out.inner_shadow.is_none());
    }

    #[test]
    fn solid_fill_round_trip() {
        let effects = LayerEffectsInfo {
            solid_fill: Some(vec![LayerEffectSolidFill {
                blend_mode: Some(BlendMode::Normal),
                color: Some(Color::Rgb(Rgb {
                    r: 255.0,
                    g: 128.0,
                    b: 0.0,
                })),
                opacity: Some(1.0),
                enabled: Some(true),
                ..LayerEffectSolidFill::default()
            }]),
            ..LayerEffectsInfo::default()
        };

        let out = round_trip(&effects);
        let sf = &out.solid_fill.as_ref().unwrap()[0];
        assert_eq!(sf.blend_mode, Some(BlendMode::Normal));
        assert_eq!(sf.enabled, Some(true));
        assert!((sf.opacity.unwrap() - 1.0).abs() < 1e-9);
        assert_rgb_close(sf.color.as_ref().unwrap(), 255.0, 128.0, 0.0);
    }

    #[test]
    fn solid_fill_invalid_version_errors() {
        // hand-build a buffer with sofi version != 2.
        let mut w = create_writer_default();
        write_uint16(&mut w, 0); // version
        write_uint16(&mut w, 2); // count (cmnS + sofi)
        write_signature(&mut w, "8BIM");
        write_signature(&mut w, "cmnS");
        write_uint32(&mut w, 7);
        write_uint32(&mut w, 0);
        write_uint8(&mut w, 1);
        write_zeros(&mut w, 2);
        write_signature(&mut w, "8BIM");
        write_signature(&mut w, "sofi");
        write_uint32(&mut w, 34);
        write_uint32(&mut w, 1); // invalid version
        let buf = get_writer_buffer(&w);
        let mut r = PsdReader::new(&buf, None, None);
        assert!(read_effects(&mut r).is_err());
    }
}
