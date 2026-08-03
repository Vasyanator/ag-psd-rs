/*
File: crates/ag-psd/src/text.rs

Purpose:
работа с текстовыми слоями (связывание текста со слоями и Engine Data).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/text.ts` (разбиение 1:1).

Main responsibilities:
- `decodeEngineData(engineData)` -> `decode_engine_data(&EngineValue) -> LayerTextData`
- `encodeEngineData(data)`       -> `encode_engine_data(&LayerTextData) -> EngineValue`
- хелперы encode/decode стилей, цветов, шрифтов, дефолтные листы стилей.

Архитектурное замечание:
upstream `text.ts` оперирует ИСКЛЮЧИТЕЛЬНО над EngineData (нетипизированный
JS-объект, у нас — `crate::engine_data::EngineValue`), а НЕ над дескрипторами.
Поэтому `crate::descriptor` здесь не используется, и его «явная типизация
вариантов» к этому файлу не относится. Все значения строятся как `EngineValue`.

Friendly-типы (`LayerTextData`, `TextStyle`, `ParagraphStyle`, `Font`,
`TextGridInfo`, `Color`, `AntiAlias`, `Justification`, `Orientation`,
`TextShapeType`, `TextStyleRun`, `ParagraphStyleRun`) переиспользуются из
`crate::psd` без локального переопределения. Структуры, специфичные для самого
EngineData (FontSet/ParagraphSheet/StyleSheet и т.п.), не выделяются в типы —
они существуют только как форма `EngineValue::Dict`, как и в upstream'е.

Порядок ключей в строящихся словарях ВАЖЕН: сериализатор EngineData обходит
ключи в порядке вставки, а вывод сверяется Photoshop'ом побайтово. Поэтому
порядок ключей здесь в точности повторяет порядок object-literal'ов TS.

Точность чисел: upstream хранит всё как JS `number` (f64). Здесь — `f64`.
*/

use crate::engine_data::EngineValue;
use crate::psd::{
    AntiAlias, Color, Cmyk, Font, Grayscale, Justification, LayerTextData, Orientation,
    ParagraphStyle, ParagraphStyleRun, Rgb, Rgba, TextGridInfo, TextShapeType, TextStyle,
    TextStyleRun,
};

// ===========================================================================
// EngineValue access helpers (mirror loose JS property access)
// ===========================================================================

fn dict(v: &EngineValue) -> Option<&[(String, EngineValue)]> {
    match v {
        EngineValue::Dict(m) => Some(m),
        _ => None,
    }
}

/// `obj[key]` over a Dict. Returns `None` when not present (mirror `undefined`).
fn get<'a>(v: &'a EngineValue, key: &str) -> Option<&'a EngineValue> {
    dict(v).and_then(|m| m.iter().find(|(k, _)| k == key).map(|(_, val)| val))
}

fn as_number(v: &EngineValue) -> Option<f64> {
    match v {
        EngineValue::Number(n) => Some(*n),
        _ => None,
    }
}

fn as_bool(v: &EngineValue) -> Option<bool> {
    match v {
        EngineValue::Bool(b) => Some(*b),
        _ => None,
    }
}

fn as_str(v: &EngineValue) -> Option<&str> {
    match v {
        EngineValue::Str(s) => Some(s.as_str()),
        _ => None,
    }
}

fn as_array(v: &EngineValue) -> Option<&[EngineValue]> {
    match v {
        EngineValue::Array(a) => Some(a),
        _ => None,
    }
}

/// `!!value` truthiness for booleans coming out of EngineData.
fn truthy_bool(v: Option<&EngineValue>) -> bool {
    match v {
        Some(EngineValue::Bool(b)) => *b,
        Some(EngineValue::Number(n)) => *n != 0.0,
        Some(EngineValue::Str(s)) => !s.is_empty(),
        _ => false,
    }
}

fn num_array(v: &EngineValue) -> Vec<f64> {
    as_array(v)
        .map(|a| a.iter().filter_map(as_number).collect())
        .unwrap_or_default()
}

fn n(value: f64) -> EngineValue {
    EngineValue::Number(value)
}

fn b(value: bool) -> EngineValue {
    EngineValue::Bool(value)
}

fn num_arr(values: &[f64]) -> EngineValue {
    EngineValue::Array(values.iter().map(|x| EngineValue::Number(*x)).collect())
}

fn d(pairs: Vec<(&str, EngineValue)>) -> EngineValue {
    EngineValue::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

// ===========================================================================
// Lookup tables (mirror `antialias` / `justification` arrays)
// ===========================================================================

const ANTIALIAS: [AntiAlias; 5] = [
    AntiAlias::None,   // 0
    AntiAlias::Crisp,  // 1
    AntiAlias::Strong, // 2
    AntiAlias::Smooth, // 3
    AntiAlias::Sharp,  // 4
];

fn antialias_index(value: AntiAlias) -> i32 {
    // mirror `antialias.indexOf(...)`; not-found is -1.
    ANTIALIAS.iter().position(|&a| a == value).map(|i| i as i32).unwrap_or(-1)
}

fn antialias_at(index: f64) -> AntiAlias {
    let i = index as i64;
    if i >= 0 && (i as usize) < ANTIALIAS.len() {
        ANTIALIAS[i as usize]
    } else {
        AntiAlias::Smooth // `?? 'smooth'`
    }
}

const JUSTIFICATION: [Justification; 7] = [
    Justification::Left,          // 0
    Justification::Right,         // 1
    Justification::Center,        // 2
    Justification::JustifyLeft,   // 3
    Justification::JustifyRight,  // 4
    Justification::JustifyCenter, // 5
    Justification::JustifyAll,    // 6
];

fn justification_index(value: Justification) -> f64 {
    JUSTIFICATION
        .iter()
        .position(|&j| j == value)
        .map(|i| i as f64)
        .unwrap_or(-1.0)
}

fn justification_at(index: f64) -> Option<Justification> {
    let i = index as i64;
    if i >= 0 && (i as usize) < JUSTIFICATION.len() {
        Some(JUSTIFICATION[i as usize])
    } else {
        None
    }
}

// ===========================================================================
// Default sheets (mirror defaultFont / defaultParagraphStyle / defaultStyle /
// defaultGridInfo). Values reproduced exactly.
// ===========================================================================

fn default_font() -> Font {
    Font {
        name: "MyriadPro-Regular".to_string(),
        script: Some(0.0),
        font_type: Some(0.0),
        synthetic: Some(0.0),
    }
}

fn default_paragraph_style() -> ParagraphStyle {
    ParagraphStyle {
        justification: Some(Justification::Left),
        first_line_indent: Some(0.0),
        start_indent: Some(0.0),
        end_indent: Some(0.0),
        space_before: Some(0.0),
        space_after: Some(0.0),
        auto_hyphenate: Some(true),
        hyphenated_word_size: Some(6.0),
        pre_hyphen: Some(2.0),
        post_hyphen: Some(2.0),
        consecutive_hyphens: Some(8.0),
        zone: Some(36.0),
        word_spacing: Some(vec![0.8, 1.0, 1.33]),
        letter_spacing: Some(vec![0.0, 0.0, 0.0]),
        glyph_spacing: Some(vec![1.0, 1.0, 1.0]),
        auto_leading: Some(1.2),
        leading_type: Some(0.0),
        hanging: Some(false),
        burasagari: Some(false),
        kinsoku_order: Some(0.0),
        every_line_composer: Some(false),
    }
}

fn default_style() -> TextStyle {
    TextStyle {
        font: Some(default_font()),
        font_size: Some(12.0),
        faux_bold: Some(false),
        faux_italic: Some(false),
        auto_leading: Some(true),
        leading: Some(0.0),
        horizontal_scale: Some(1.0),
        vertical_scale: Some(1.0),
        tracking: Some(0.0),
        auto_kerning: Some(true),
        kerning: Some(0.0),
        baseline_shift: Some(0.0),
        font_caps: Some(0.0),
        font_baseline: Some(0.0),
        underline: Some(false),
        strikethrough: Some(false),
        ligatures: Some(true),
        d_ligatures: Some(false),
        baseline_direction: Some(2.0),
        tsume: Some(0.0),
        style_run_alignment: Some(2.0),
        language: Some(0.0),
        no_break: Some(false),
        fill_color: Some(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 0.0 })),
        stroke_color: Some(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 0.0 })),
        fill_flag: Some(true),
        stroke_flag: Some(false),
        fill_first: Some(true),
        y_underline: Some(1.0),
        outline_width: Some(1.0),
        character_direction: Some(0.0),
        hindi_numbers: Some(false),
        kashida: Some(1.0),
        diacritic_pos: Some(2.0),
    }
}

fn default_grid_info() -> TextGridInfo {
    TextGridInfo {
        is_on: Some(false),
        show: Some(false),
        size: Some(18.0),
        leading: Some(22.0),
        color: Some(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 255.0 })),
        leading_fill_color: Some(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 255.0 })),
        align_line_height_to_grid_flags: Some(false),
    }
}

// ===========================================================================
// Color encode / decode (mirror decodeColor / encodeColor)
// ===========================================================================

/// Mirror `decodeColor`. `color` is the `TypeValues` dict `{ Type, Values }`.
fn decode_color(color: &EngineValue) -> Result<Color, String> {
    let ty = get(color, "Type").and_then(as_number).unwrap_or(0.0);
    let c: Vec<f64> = get(color, "Values").map(num_array).unwrap_or_default();
    let at = |i: usize| c.get(i).copied().unwrap_or(0.0);
    match ty as i64 {
        0 => Ok(Color::Grayscale(Grayscale { k: at(1) * 255.0 })),
        1 => {
            if at(0) == 1.0 {
                Ok(Color::Rgb(Rgb {
                    r: at(1) * 255.0,
                    g: at(2) * 255.0,
                    b: at(3) * 255.0,
                }))
            } else {
                Ok(Color::Rgba(Rgba {
                    r: at(1) * 255.0,
                    g: at(2) * 255.0,
                    b: at(3) * 255.0,
                    a: at(0) * 255.0,
                }))
            }
        }
        2 => Ok(Color::Cmyk(Cmyk {
            c: at(1) * 255.0,
            m: at(2) * 255.0,
            y: at(3) * 255.0,
            k: at(4) * 255.0,
        })),
        _ => Err("Unknown color type in text layer".to_string()),
    }
}

/// Mirror `encodeColor`. Returns the `TypeValues` dict `{ Type, Values }`.
fn encode_color(color: Option<&Color>) -> EngineValue {
    match color {
        None => d(vec![("Type", n(1.0)), ("Values", num_arr(&[0.0, 0.0, 0.0, 0.0]))]),
        Some(Color::Rgb(c)) => d(vec![
            ("Type", n(1.0)),
            ("Values", num_arr(&[1.0, c.r / 255.0, c.g / 255.0, c.b / 255.0])),
        ]),
        Some(Color::Rgba(c)) => d(vec![
            ("Type", n(1.0)),
            ("Values", num_arr(&[c.a / 255.0, c.r / 255.0, c.g / 255.0, c.b / 255.0])),
        ]),
        Some(Color::Cmyk(c)) => d(vec![
            ("Type", n(2.0)),
            ("Values", num_arr(&[1.0, c.c / 255.0, c.m / 255.0, c.y / 255.0, c.k / 255.0])),
        ]),
        Some(Color::Grayscale(c)) => {
            d(vec![("Type", n(0.0)), ("Values", num_arr(&[1.0, c.k / 255.0]))])
        }
        // upstream throws 'Invalid color type in text layer' for unsupported
        // color kinds (FRGB/HSB/LAB don't appear in text layers). Mirror as the
        // default empty RGB rather than panicking in a library.
        Some(_) => d(vec![("Type", n(1.0)), ("Values", num_arr(&[0.0, 0.0, 0.0, 0.0]))]),
    }
}

// ===========================================================================
// Font dedup / indexing (mirror findOrAddFont)
// ===========================================================================

fn find_or_add_font(fonts: &mut Vec<Font>, font: &Font) -> f64 {
    for (i, f) in fonts.iter().enumerate() {
        if f.name == font.name {
            return i as f64;
        }
    }
    fonts.push(font.clone());
    (fonts.len() - 1) as f64
}

// ===========================================================================
// Style encode / decode
//
// upstream uses a generic `decodeObject`/`encodeObject` keyed by the field name
// list + upperFirst. With typed Rust structs we map each field explicitly,
// preserving the SAME key order as `styleKeys` / `paragraphStyleKeys`.
// ===========================================================================

fn decode_paragraph_style(obj: &EngineValue) -> ParagraphStyle {
    let mut s = ParagraphStyle::default();
    if let Some(v) = get(obj, "Justification").and_then(as_number) {
        s.justification = justification_at(v);
    }
    s.first_line_indent = get(obj, "FirstLineIndent").and_then(as_number);
    s.start_indent = get(obj, "StartIndent").and_then(as_number);
    s.end_indent = get(obj, "EndIndent").and_then(as_number);
    s.space_before = get(obj, "SpaceBefore").and_then(as_number);
    s.space_after = get(obj, "SpaceAfter").and_then(as_number);
    s.auto_hyphenate = get(obj, "AutoHyphenate").and_then(as_bool);
    s.hyphenated_word_size = get(obj, "HyphenatedWordSize").and_then(as_number);
    s.pre_hyphen = get(obj, "PreHyphen").and_then(as_number);
    s.post_hyphen = get(obj, "PostHyphen").and_then(as_number);
    s.consecutive_hyphens = get(obj, "ConsecutiveHyphens").and_then(as_number);
    s.zone = get(obj, "Zone").and_then(as_number);
    s.word_spacing = get(obj, "WordSpacing").map(num_array);
    s.letter_spacing = get(obj, "LetterSpacing").map(num_array);
    s.glyph_spacing = get(obj, "GlyphSpacing").map(num_array);
    s.auto_leading = get(obj, "AutoLeading").and_then(as_number);
    s.leading_type = get(obj, "LeadingType").and_then(as_number);
    s.hanging = get(obj, "Hanging").and_then(as_bool);
    s.burasagari = get(obj, "Burasagari").and_then(as_bool);
    s.kinsoku_order = get(obj, "KinsokuOrder").and_then(as_number);
    s.every_line_composer = get(obj, "EveryLineComposer").and_then(as_bool);
    s
}

/// Mirror `decodeStyle`. `fonts` indexes the FontSet so `Font` references resolve.
fn decode_style(obj: &EngineValue, fonts: &[Font]) -> TextStyle {
    let mut s = TextStyle::default();
    if let Some(v) = get(obj, "Font").and_then(as_number) {
        let idx = v as i64;
        if idx >= 0 && (idx as usize) < fonts.len() {
            s.font = Some(fonts[idx as usize].clone());
        }
    }
    s.font_size = get(obj, "FontSize").and_then(as_number);
    s.faux_bold = get(obj, "FauxBold").and_then(as_bool);
    s.faux_italic = get(obj, "FauxItalic").and_then(as_bool);
    s.auto_leading = get(obj, "AutoLeading").and_then(as_bool);
    s.leading = get(obj, "Leading").and_then(as_number);
    s.horizontal_scale = get(obj, "HorizontalScale").and_then(as_number);
    s.vertical_scale = get(obj, "VerticalScale").and_then(as_number);
    s.tracking = get(obj, "Tracking").and_then(as_number);
    s.auto_kerning = get(obj, "AutoKerning").and_then(as_bool);
    s.kerning = get(obj, "Kerning").and_then(as_number);
    s.baseline_shift = get(obj, "BaselineShift").and_then(as_number);
    s.font_caps = get(obj, "FontCaps").and_then(as_number);
    s.font_baseline = get(obj, "FontBaseline").and_then(as_number);
    s.underline = get(obj, "Underline").and_then(as_bool);
    s.strikethrough = get(obj, "Strikethrough").and_then(as_bool);
    s.ligatures = get(obj, "Ligatures").and_then(as_bool);
    s.d_ligatures = get(obj, "DLigatures").and_then(as_bool);
    s.baseline_direction = get(obj, "BaselineDirection").and_then(as_number);
    s.tsume = get(obj, "Tsume").and_then(as_number);
    s.style_run_alignment = get(obj, "StyleRunAlignment").and_then(as_number);
    s.language = get(obj, "Language").and_then(as_number);
    s.no_break = get(obj, "NoBreak").and_then(as_bool);
    if let Some(c) = get(obj, "FillColor") {
        s.fill_color = decode_color(c).ok();
    }
    if let Some(c) = get(obj, "StrokeColor") {
        s.stroke_color = decode_color(c).ok();
    }
    s.fill_flag = get(obj, "FillFlag").and_then(as_bool);
    s.stroke_flag = get(obj, "StrokeFlag").and_then(as_bool);
    s.fill_first = get(obj, "FillFirst").and_then(as_bool);
    s.y_underline = get(obj, "YUnderline").and_then(as_number);
    s.outline_width = get(obj, "OutlineWidth").and_then(as_number);
    s.character_direction = get(obj, "CharacterDirection").and_then(as_number);
    s.hindi_numbers = get(obj, "HindiNumbers").and_then(as_bool);
    s.kashida = get(obj, "Kashida").and_then(as_number);
    s.diacritic_pos = get(obj, "DiacriticPos").and_then(as_number);
    s
}

/// Mirror `encodeParagraphStyle`. Emits keys in `paragraphStyleKeys` order,
/// skipping fields that are `None` (mirror `obj[key] === undefined`).
fn encode_paragraph_style(s: &ParagraphStyle) -> EngineValue {
    let mut out: Vec<(String, EngineValue)> = Vec::new();
    macro_rules! put {
        ($key:expr, $opt:expr, $f:expr) => {
            if let Some(v) = $opt {
                out.push(($key.to_string(), $f(v)));
            }
        };
    }
    // 'justification'
    if let Some(j) = s.justification {
        out.push(("Justification".to_string(), n(justification_index(j))));
    }
    put!("FirstLineIndent", s.first_line_indent, n);
    put!("StartIndent", s.start_indent, n);
    put!("EndIndent", s.end_indent, n);
    put!("SpaceBefore", s.space_before, n);
    put!("SpaceAfter", s.space_after, n);
    put!("AutoHyphenate", s.auto_hyphenate, b);
    put!("HyphenatedWordSize", s.hyphenated_word_size, n);
    put!("PreHyphen", s.pre_hyphen, n);
    put!("PostHyphen", s.post_hyphen, n);
    put!("ConsecutiveHyphens", s.consecutive_hyphens, n);
    put!("Zone", s.zone, n);
    put!("WordSpacing", s.word_spacing.as_ref(), |v: &Vec<f64>| num_arr(v));
    put!("LetterSpacing", s.letter_spacing.as_ref(), |v: &Vec<f64>| num_arr(v));
    put!("GlyphSpacing", s.glyph_spacing.as_ref(), |v: &Vec<f64>| num_arr(v));
    put!("AutoLeading", s.auto_leading, n);
    put!("LeadingType", s.leading_type, n);
    put!("Hanging", s.hanging, b);
    put!("Burasagari", s.burasagari, b);
    put!("KinsokuOrder", s.kinsoku_order, n);
    put!("EveryLineComposer", s.every_line_composer, b);
    EngineValue::Dict(out)
}

/// Mirror `encodeStyle`. Emits keys in `styleKeys` order, skipping `None`.
fn encode_style(s: &TextStyle, fonts: &mut Vec<Font>) -> EngineValue {
    let mut out: Vec<(String, EngineValue)> = Vec::new();
    macro_rules! put {
        ($key:expr, $opt:expr, $f:expr) => {
            if let Some(v) = $opt {
                out.push(($key.to_string(), $f(v)));
            }
        };
    }
    // 'font'
    if let Some(f) = s.font.as_ref() {
        out.push(("Font".to_string(), n(find_or_add_font(fonts, f))));
    }
    put!("FontSize", s.font_size, n);
    put!("FauxBold", s.faux_bold, b);
    put!("FauxItalic", s.faux_italic, b);
    put!("AutoLeading", s.auto_leading, b);
    put!("Leading", s.leading, n);
    put!("HorizontalScale", s.horizontal_scale, n);
    put!("VerticalScale", s.vertical_scale, n);
    put!("Tracking", s.tracking, n);
    put!("AutoKerning", s.auto_kerning, b);
    put!("Kerning", s.kerning, n);
    put!("BaselineShift", s.baseline_shift, n);
    put!("FontCaps", s.font_caps, n);
    put!("FontBaseline", s.font_baseline, n);
    put!("Underline", s.underline, b);
    put!("Strikethrough", s.strikethrough, b);
    put!("Ligatures", s.ligatures, b);
    put!("DLigatures", s.d_ligatures, b);
    put!("BaselineDirection", s.baseline_direction, n);
    put!("Tsume", s.tsume, n);
    put!("StyleRunAlignment", s.style_run_alignment, n);
    put!("Language", s.language, n);
    put!("NoBreak", s.no_break, b);
    // 'fillColor'
    if let Some(c) = s.fill_color.as_ref() {
        out.push(("FillColor".to_string(), encode_color(Some(c))));
    }
    if let Some(c) = s.stroke_color.as_ref() {
        out.push(("StrokeColor".to_string(), encode_color(Some(c))));
    }
    put!("FillFlag", s.fill_flag, b);
    put!("StrokeFlag", s.stroke_flag, b);
    put!("FillFirst", s.fill_first, b);
    put!("YUnderline", s.y_underline, n);
    put!("OutlineWidth", s.outline_width, n);
    put!("CharacterDirection", s.character_direction, n);
    put!("HindiNumbers", s.hindi_numbers, b);
    put!("Kashida", s.kashida, n);
    put!("DiacriticPos", s.diacritic_pos, n);
    EngineValue::Dict(out)
}

// ===========================================================================
// Merge helpers (mirror the `{ ...defaults, ...overrides }` spreads)
// ===========================================================================

fn merge_paragraph(base: &ParagraphStyle, over: &ParagraphStyle) -> ParagraphStyle {
    let mut r = base.clone();
    macro_rules! m {
        ($field:ident) => {
            if over.$field.is_some() {
                r.$field = over.$field.clone();
            }
        };
    }
    m!(justification);
    m!(first_line_indent);
    m!(start_indent);
    m!(end_indent);
    m!(space_before);
    m!(space_after);
    m!(auto_hyphenate);
    m!(hyphenated_word_size);
    m!(pre_hyphen);
    m!(post_hyphen);
    m!(consecutive_hyphens);
    m!(zone);
    m!(word_spacing);
    m!(letter_spacing);
    m!(glyph_spacing);
    m!(auto_leading);
    m!(leading_type);
    m!(hanging);
    m!(burasagari);
    m!(kinsoku_order);
    m!(every_line_composer);
    r
}

fn merge_style(base: &TextStyle, over: &TextStyle) -> TextStyle {
    let mut r = base.clone();
    macro_rules! m {
        ($field:ident) => {
            if over.$field.is_some() {
                r.$field = over.$field.clone();
            }
        };
    }
    m!(font);
    m!(font_size);
    m!(faux_bold);
    m!(faux_italic);
    m!(auto_leading);
    m!(leading);
    m!(horizontal_scale);
    m!(vertical_scale);
    m!(tracking);
    m!(auto_kerning);
    m!(kerning);
    m!(baseline_shift);
    m!(font_caps);
    m!(font_baseline);
    m!(underline);
    m!(strikethrough);
    m!(ligatures);
    m!(d_ligatures);
    m!(baseline_direction);
    m!(tsume);
    m!(style_run_alignment);
    m!(language);
    m!(no_break);
    m!(fill_color);
    m!(stroke_color);
    m!(fill_flag);
    m!(stroke_flag);
    m!(fill_first);
    m!(y_underline);
    m!(outline_width);
    m!(character_direction);
    m!(hindi_numbers);
    m!(kashida);
    m!(diacritic_pos);
    r
}

// ===========================================================================
// decodeEngineData
// ===========================================================================

/// Port of `decodeEngineData`. Converts a parsed EngineData `EngineValue` into a
/// friendly `LayerTextData`.
pub fn decode_engine_data(engine_data: &EngineValue) -> LayerTextData {
    let engine_dict = get(engine_data, "EngineDict").cloned().unwrap_or(EngineValue::Null);
    let resource_dict = get(engine_data, "ResourceDict").cloned().unwrap_or(EngineValue::Null);

    // fonts
    let fonts: Vec<Font> = get(&resource_dict, "FontSet")
        .and_then(as_array)
        .map(|arr| {
            arr.iter()
                .map(|f| Font {
                    name: get(f, "Name").and_then(as_str).unwrap_or("").to_string(),
                    script: get(f, "Script").and_then(as_number),
                    font_type: get(f, "FontType").and_then(as_number),
                    synthetic: get(f, "Synthetic").and_then(as_number),
                })
                .collect()
        })
        .unwrap_or_default();

    // text: Editor.Text with \r -> \n, then trim trailing \n counting removals.
    let raw_text = get(&engine_dict, "Editor")
        .and_then(|e| get(e, "Text"))
        .and_then(as_str)
        .unwrap_or("")
        .to_string();
    // operate on UTF-16 units to mirror JS .length/charCodeAt semantics
    let mut units: Vec<u16> = raw_text.encode_utf16().map(|u| if u == 13 { 10 } else { u }).collect();
    let mut removed_characters: usize = 0;
    while units.last() == Some(&10) {
        units.pop();
        removed_characters += 1;
    }
    let text = String::from_utf16_lossy(&units);

    let mut result = LayerTextData {
        text,
        anti_alias: Some(antialias_at(
            get(&engine_dict, "AntiAlias").and_then(as_number).unwrap_or(0.0),
        )),
        use_fractional_glyph_widths: Some(truthy_bool(get(&engine_dict, "UseFractionalGlyphWidths"))),
        superscript_size: get(&resource_dict, "SuperscriptSize").and_then(as_number),
        superscript_position: get(&resource_dict, "SuperscriptPosition").and_then(as_number),
        subscript_size: get(&resource_dict, "SubscriptSize").and_then(as_number),
        subscript_position: get(&resource_dict, "SubscriptPosition").and_then(as_number),
        small_cap_size: get(&resource_dict, "SmallCapSize").and_then(as_number),
        ..Default::default()
    };

    // shape
    let photoshop = get(&engine_dict, "Rendered")
        .and_then(|r| get(r, "Shapes"))
        .and_then(|s| get(s, "Children"))
        .and_then(as_array)
        .and_then(|c| c.first())
        .and_then(|c0| get(c0, "Cookie"))
        .and_then(|ck| get(ck, "Photoshop"))
        .cloned();

    if let Some(ps) = photoshop {
        let shape_type = get(&ps, "ShapeType").and_then(as_number).unwrap_or(0.0);
        result.shape_type = Some(if shape_type == 1.0 {
            TextShapeType::Box
        } else {
            TextShapeType::Point
        });
        if let Some(pb) = get(&ps, "PointBase") {
            result.point_base = Some(num_array(pb));
        }
        if let Some(bb) = get(&ps, "BoxBounds") {
            result.box_bounds = Some(num_array(bb));
        }
    }

    // paragraph style
    let paragraph_run = get(&engine_dict, "ParagraphRun").cloned().unwrap_or(EngineValue::Null);
    result.paragraph_style = Some(ParagraphStyle::default());
    let mut paragraph_style_runs: Vec<ParagraphStyleRun> = Vec::new();

    let p_run_array = get(&paragraph_run, "RunArray").and_then(as_array).map(|a| a.to_vec()).unwrap_or_default();
    let p_len_array: Vec<f64> = get(&paragraph_run, "RunLengthArray").map(num_array).unwrap_or_default();
    for (i, run) in p_run_array.iter().enumerate() {
        let length = p_len_array.get(i).copied().unwrap_or(0.0);
        let props = get(run, "ParagraphSheet")
            .and_then(|sheet| get(sheet, "Properties"))
            .cloned()
            .unwrap_or(EngineValue::Dict(Vec::new()));
        let style = decode_paragraph_style(&props);
        paragraph_style_runs.push(ParagraphStyleRun { length, style });
    }

    // trim removed trailing characters off the last run(s)
    let mut counter = removed_characters;
    while !paragraph_style_runs.is_empty() && counter > 0 {
        let last = paragraph_style_runs.len() - 1;
        paragraph_style_runs[last].length -= 1.0;
        if paragraph_style_runs[last].length == 0.0 {
            paragraph_style_runs.pop();
        }
        counter -= 1;
    }

    deduplicate_paragraph(result.paragraph_style.as_mut().unwrap(), &mut paragraph_style_runs);
    result.paragraph_style_runs = if paragraph_style_runs.is_empty() {
        None
    } else {
        Some(paragraph_style_runs)
    };

    // style
    let style_run = get(&engine_dict, "StyleRun").cloned().unwrap_or(EngineValue::Null);
    result.style = Some(TextStyle::default());
    let mut style_runs: Vec<TextStyleRun> = Vec::new();

    let s_run_array = get(&style_run, "RunArray").and_then(as_array).map(|a| a.to_vec()).unwrap_or_default();
    let s_len_array: Vec<f64> = get(&style_run, "RunLengthArray").map(num_array).unwrap_or_default();
    for (i, run) in s_run_array.iter().enumerate() {
        let length = s_len_array.get(i).copied().unwrap_or(0.0);
        let data = get(run, "StyleSheet")
            .and_then(|ss| get(ss, "StyleSheetData"))
            .cloned()
            .unwrap_or(EngineValue::Dict(Vec::new()));
        let mut style = decode_style(&data, &fonts);
        if style.font.is_none() {
            style.font = fonts.first().cloned();
        }
        style_runs.push(TextStyleRun { length, style });
    }

    let mut counter = removed_characters;
    while !style_runs.is_empty() && counter > 0 {
        let last = style_runs.len() - 1;
        style_runs[last].length -= 1.0;
        if style_runs[last].length == 0.0 {
            style_runs.pop();
        }
        counter -= 1;
    }

    deduplicate_style(result.style.as_mut().unwrap(), &mut style_runs);
    result.style_runs = if style_runs.is_empty() { None } else { Some(style_runs) };

    result
}

// ===========================================================================
// deduplicateValues — specialized per style kind.
//
// upstream: for each key, if all runs share run[0]'s value, copy it to `base`
// and delete it from each run that matches `base`. If every run ends up empty,
// clear the run list. We mirror exactly, per-field.
// ===========================================================================

macro_rules! dedup_field {
    ($base:expr, $runs:expr, $field:ident) => {{
        if let Some(first) = $runs.first() {
            let value = first.style.$field.clone();
            if value.is_some() {
                let identical = $runs.iter().all(|r| r.style.$field == value);
                if identical {
                    $base.$field = value.clone();
                }
            }
            if $base.$field.is_some() {
                let bv = $base.$field.clone();
                for r in $runs.iter_mut() {
                    if r.style.$field == bv {
                        r.style.$field = None;
                    }
                }
            }
        }
    }};
}

fn paragraph_style_is_empty(s: &ParagraphStyle) -> bool {
    s.justification.is_none()
        && s.first_line_indent.is_none()
        && s.start_indent.is_none()
        && s.end_indent.is_none()
        && s.space_before.is_none()
        && s.space_after.is_none()
        && s.auto_hyphenate.is_none()
        && s.hyphenated_word_size.is_none()
        && s.pre_hyphen.is_none()
        && s.post_hyphen.is_none()
        && s.consecutive_hyphens.is_none()
        && s.zone.is_none()
        && s.word_spacing.is_none()
        && s.letter_spacing.is_none()
        && s.glyph_spacing.is_none()
        && s.auto_leading.is_none()
        && s.leading_type.is_none()
        && s.hanging.is_none()
        && s.burasagari.is_none()
        && s.kinsoku_order.is_none()
        && s.every_line_composer.is_none()
}

fn deduplicate_paragraph(base: &mut ParagraphStyle, runs: &mut Vec<ParagraphStyleRun>) {
    if runs.is_empty() {
        return;
    }
    dedup_field!(base, runs, justification);
    dedup_field!(base, runs, first_line_indent);
    dedup_field!(base, runs, start_indent);
    dedup_field!(base, runs, end_indent);
    dedup_field!(base, runs, space_before);
    dedup_field!(base, runs, space_after);
    dedup_field!(base, runs, auto_hyphenate);
    dedup_field!(base, runs, hyphenated_word_size);
    dedup_field!(base, runs, pre_hyphen);
    dedup_field!(base, runs, post_hyphen);
    dedup_field!(base, runs, consecutive_hyphens);
    dedup_field!(base, runs, zone);
    dedup_field!(base, runs, word_spacing);
    dedup_field!(base, runs, letter_spacing);
    dedup_field!(base, runs, glyph_spacing);
    dedup_field!(base, runs, auto_leading);
    dedup_field!(base, runs, leading_type);
    dedup_field!(base, runs, hanging);
    dedup_field!(base, runs, burasagari);
    dedup_field!(base, runs, kinsoku_order);
    dedup_field!(base, runs, every_line_composer);

    if runs.iter().all(|r| paragraph_style_is_empty(&r.style)) {
        runs.clear();
    }
}

fn style_is_empty(s: &TextStyle) -> bool {
    s.font.is_none()
        && s.font_size.is_none()
        && s.faux_bold.is_none()
        && s.faux_italic.is_none()
        && s.auto_leading.is_none()
        && s.leading.is_none()
        && s.horizontal_scale.is_none()
        && s.vertical_scale.is_none()
        && s.tracking.is_none()
        && s.auto_kerning.is_none()
        && s.kerning.is_none()
        && s.baseline_shift.is_none()
        && s.font_caps.is_none()
        && s.font_baseline.is_none()
        && s.underline.is_none()
        && s.strikethrough.is_none()
        && s.ligatures.is_none()
        && s.d_ligatures.is_none()
        && s.baseline_direction.is_none()
        && s.tsume.is_none()
        && s.style_run_alignment.is_none()
        && s.language.is_none()
        && s.no_break.is_none()
        && s.fill_color.is_none()
        && s.stroke_color.is_none()
        && s.fill_flag.is_none()
        && s.stroke_flag.is_none()
        && s.fill_first.is_none()
        && s.y_underline.is_none()
        && s.outline_width.is_none()
        && s.character_direction.is_none()
        && s.hindi_numbers.is_none()
        && s.kashida.is_none()
        && s.diacritic_pos.is_none()
}

fn deduplicate_style(base: &mut TextStyle, runs: &mut Vec<TextStyleRun>) {
    if runs.is_empty() {
        return;
    }
    // `font` compares by structural equality; Font isn't PartialEq, so compare by name+fields.
    {
        if let Some(first) = runs.first() {
            let value = first.style.font.clone();
            if value.is_some() {
                let identical = runs.iter().all(|r| font_eq(&r.style.font, &value));
                if identical {
                    base.font = value.clone();
                }
            }
            if base.font.is_some() {
                let bv = base.font.clone();
                for r in runs.iter_mut() {
                    if font_eq(&r.style.font, &bv) {
                        r.style.font = None;
                    }
                }
            }
        }
    }
    dedup_field!(base, runs, font_size);
    dedup_field!(base, runs, faux_bold);
    dedup_field!(base, runs, faux_italic);
    dedup_field!(base, runs, auto_leading);
    dedup_field!(base, runs, leading);
    dedup_field!(base, runs, horizontal_scale);
    dedup_field!(base, runs, vertical_scale);
    dedup_field!(base, runs, tracking);
    dedup_field!(base, runs, auto_kerning);
    dedup_field!(base, runs, kerning);
    dedup_field!(base, runs, baseline_shift);
    dedup_field!(base, runs, font_caps);
    dedup_field!(base, runs, font_baseline);
    dedup_field!(base, runs, underline);
    dedup_field!(base, runs, strikethrough);
    dedup_field!(base, runs, ligatures);
    dedup_field!(base, runs, d_ligatures);
    dedup_field!(base, runs, baseline_direction);
    dedup_field!(base, runs, tsume);
    dedup_field!(base, runs, style_run_alignment);
    dedup_field!(base, runs, language);
    dedup_field!(base, runs, no_break);
    dedup_field!(base, runs, fill_color);
    dedup_field!(base, runs, stroke_color);
    dedup_field!(base, runs, fill_flag);
    dedup_field!(base, runs, stroke_flag);
    dedup_field!(base, runs, fill_first);
    dedup_field!(base, runs, y_underline);
    dedup_field!(base, runs, outline_width);
    dedup_field!(base, runs, character_direction);
    dedup_field!(base, runs, hindi_numbers);
    dedup_field!(base, runs, kashida);
    dedup_field!(base, runs, diacritic_pos);

    if runs.iter().all(|r| style_is_empty(&r.style)) {
        runs.clear();
    }
}

fn font_eq(a: &Option<Font>, b: &Option<Font>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => {
            x.name == y.name
                && x.script == y.script
                && x.font_type == y.font_type
                && x.synthetic == y.synthetic
        }
        (None, None) => true,
        _ => false,
    }
}

// ===========================================================================
// encodeEngineData
// ===========================================================================

/// Port of `encodeEngineData`. Builds the EngineData `EngineValue` from friendly
/// `LayerTextData`. Key order matches the TS object-literals exactly.
pub fn encode_engine_data(data: &LayerTextData) -> EngineValue {
    // text = `${(text||'').replace(/\r?\n/g, '\r')}\r`
    let text = normalize_text(&data.text);
    let text_len = text.encode_utf16().count() as f64;
    let text_units: Vec<u16> = text.encode_utf16().collect();

    // fonts list starts with AdobeInvisFont
    let mut fonts: Vec<Font> = vec![Font {
        name: "AdobeInvisFont".to_string(),
        script: Some(0.0),
        font_type: Some(0.0),
        synthetic: Some(0.0),
    }];

    // def font: data.style.font || first styleRun with font || defaultFont
    let def_font = data
        .style
        .as_ref()
        .and_then(|s| s.font.clone())
        .or_else(|| {
            data.style_runs
                .as_ref()
                .and_then(|runs| runs.iter().find_map(|r| r.style.font.clone()))
        })
        .unwrap_or_else(default_font);

    let data_paragraph_style = data.paragraph_style.clone().unwrap_or_default();
    let default_para = default_paragraph_style();

    // ---- paragraph runs ----
    let mut paragraph_run_array: Vec<EngineValue> = Vec::new();
    let mut paragraph_run_length_array: Vec<f64> = Vec::new();

    let para_sheet = |props: EngineValue| {
        d(vec![
            ("DefaultStyleSheet", n(0.0)),
            ("Properties", props),
        ])
    };
    let para_run_entry = |props: EngineValue| {
        d(vec![
            ("ParagraphSheet", para_sheet(props)),
            (
                "Adjustments",
                d(vec![("Axis", num_arr(&[1.0, 0.0, 1.0])), ("XY", num_arr(&[0.0, 0.0]))]),
            ),
        ])
    };

    let has_para_runs = data
        .paragraph_style_runs
        .as_ref()
        .map(|r| !r.is_empty())
        .unwrap_or(false);

    if has_para_runs {
        let runs = data.paragraph_style_runs.as_ref().unwrap();
        let mut left_length = text_len;
        let last_idx = runs.len() - 1;

        for (i, run) in runs.iter().enumerate() {
            let mut run_length = run.length.min(left_length);
            left_length -= run_length;

            if run_length == 0.0 {
                continue;
            }

            // extend last run if it's only for trailing \r
            if left_length == 1.0 && i == last_idx {
                run_length += 1.0;
                left_length -= 1.0;
            }

            paragraph_run_length_array.push(run_length);
            let merged = merge_paragraph(&merge_paragraph(&default_para, &data_paragraph_style), &run.style);
            paragraph_run_array.push(para_run_entry(encode_paragraph_style(&merged)));
        }

        if left_length != 0.0 {
            paragraph_run_length_array.push(left_length);
            let merged = merge_paragraph(&default_para, &data_paragraph_style);
            paragraph_run_array.push(para_run_entry(encode_paragraph_style(&merged)));
        }
    } else {
        let mut last = 0usize;
        for (i, unit) in text_units.iter().enumerate() {
            if *unit == 13 {
                // \r
                paragraph_run_length_array.push((i - last + 1) as f64);
                let merged = merge_paragraph(&default_para, &data_paragraph_style);
                paragraph_run_array.push(para_run_entry(encode_paragraph_style(&merged)));
                last = i + 1;
            }
        }
    }

    // ---- style sheet + style runs ----
    let mut style_sheet_base = default_style();
    style_sheet_base.font = Some(def_font.clone());
    let style_sheet_data = encode_style(&style_sheet_base, &mut fonts);

    let data_style = data.style.clone().unwrap_or_default();

    // styleRuns default = [{ length: text.length, style: data.style || {} }]
    let style_runs: Vec<TextStyleRun> = match data.style_runs.as_ref() {
        Some(r) => r.clone(),
        None => vec![TextStyleRun {
            length: text_len,
            style: data_style.clone(),
        }],
    };

    let mut style_run_array: Vec<EngineValue> = Vec::new();
    let mut style_run_length_array: Vec<f64> = Vec::new();

    // base for each run: { kerning:0, autoKerning:true, fillColor:{0,0,0}, ...data.style, ...run.style }
    let run_base = || {
        let mut s = TextStyle {
            kerning: Some(0.0),
            auto_kerning: Some(true),
            fill_color: Some(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 0.0 })),
            ..Default::default()
        };
        s = merge_style(&s, &data_style);
        s
    };

    let mut left_length = text_len;
    let last_idx = if style_runs.is_empty() { 0 } else { style_runs.len() - 1 };

    for (i, run) in style_runs.iter().enumerate() {
        let mut run_length = run.length.min(left_length);
        left_length -= run_length;

        if run_length == 0.0 {
            continue;
        }

        if left_length == 1.0 && i == last_idx {
            run_length += 1.0;
            left_length -= 1.0;
        }

        style_run_length_array.push(run_length);
        let merged = merge_style(&run_base(), &run.style);
        style_run_array.push(d(vec![(
            "StyleSheet",
            d(vec![("StyleSheetData", encode_style(&merged, &mut fonts))]),
        )]));
    }

    // add extra run to the end if existing ones didn't fill it up
    if left_length != 0.0 && !style_runs.is_empty() {
        style_run_length_array.push(left_length);
        let merged = run_base();
        style_run_array.push(d(vec![(
            "StyleSheet",
            d(vec![("StyleSheetData", encode_style(&merged, &mut fonts))]),
        )]));
    }

    // ---- grid info / shape ----
    let grid_info = merge_grid(&default_grid_info(), data.grid_info.as_ref());
    let writing_direction = if data.orientation == Some(Orientation::Vertical) { 2.0 } else { 0.0 };
    let procession = if data.orientation == Some(Orientation::Vertical) { 1.0 } else { 0.0 };
    let shape_type = if data.shape_type == Some(TextShapeType::Box) { 1.0 } else { 0.0 };

    // Photoshop node, properties in exact order: ShapeType, (PointBase|BoxBounds), Base
    let mut photoshop_pairs: Vec<(&str, EngineValue)> = vec![("ShapeType", n(shape_type))];
    if shape_type == 0.0 {
        let pb = data.point_base.clone().unwrap_or_else(|| vec![0.0, 0.0]);
        photoshop_pairs.push(("PointBase", num_arr(&pb)));
    } else {
        let bb = data.box_bounds.clone().unwrap_or_else(|| vec![0.0, 0.0, 0.0, 0.0]);
        photoshop_pairs.push(("BoxBounds", num_arr(&bb)));
    }
    photoshop_pairs.push((
        "Base",
        d(vec![
            ("ShapeType", n(shape_type)),
            ("TransformPoint0", num_arr(&[1.0, 0.0])),
            ("TransformPoint1", num_arr(&[0.0, 1.0])),
            ("TransformPoint2", num_arr(&[0.0, 0.0])),
        ]),
    ));
    let photoshop = d(photoshop_pairs);

    // ---- default resources ----
    let build_resources = || -> EngineValue {
        let kinsoku_set = EngineValue::Array(vec![
            d(vec![
                ("Name", EngineValue::Str("PhotoshopKinsokuHard".to_string())),
                ("NoStart", EngineValue::Str("、。，．・：；？！ー―’”）〕］｝〉》」』】ヽヾゝゞ々ぁぃぅぇぉっゃゅょゎァィゥェォッャュョヮヵヶ゛゜?!)]},.:;℃℉¢％‰".to_string())),
                ("NoEnd", EngineValue::Str("‘“（〔［｛〈《「『【([{￥＄£＠§〒＃".to_string())),
                ("Keep", EngineValue::Str("―‥".to_string())),
                ("Hanging", EngineValue::Str("、。.,".to_string())),
            ]),
            d(vec![
                ("Name", EngineValue::Str("PhotoshopKinsokuSoft".to_string())),
                ("NoStart", EngineValue::Str("、。，．・：；？！’”）〕］｝〉》」』】ヽヾゝゞ々".to_string())),
                ("NoEnd", EngineValue::Str("‘“（〔［｛〈《「『【".to_string())),
                ("Keep", EngineValue::Str("―‥".to_string())),
                ("Hanging", EngineValue::Str("、。.,".to_string())),
            ]),
        ]);
        let mojikumi_set = EngineValue::Array(vec![
            d(vec![("InternalName", EngineValue::Str("Photoshop6MojiKumiSet1".to_string()))]),
            d(vec![("InternalName", EngineValue::Str("Photoshop6MojiKumiSet2".to_string()))]),
            d(vec![("InternalName", EngineValue::Str("Photoshop6MojiKumiSet3".to_string()))]),
            d(vec![("InternalName", EngineValue::Str("Photoshop6MojiKumiSet4".to_string()))]),
        ]);

        let paragraph_sheet_set = EngineValue::Array(vec![d(vec![
            ("Name", EngineValue::Str("Normal RGB".to_string())),
            ("DefaultStyleSheet", n(0.0)),
            (
                "Properties",
                encode_paragraph_style(&merge_paragraph(&default_para, &data_paragraph_style)),
            ),
        ])]);

        let style_sheet_set = EngineValue::Array(vec![d(vec![
            ("Name", EngineValue::Str("Normal RGB".to_string())),
            ("StyleSheetData", style_sheet_data.clone()),
        ])]);

        let font_set = EngineValue::Array(
            fonts
                .iter()
                .map(|f| {
                    d(vec![
                        ("Name", EngineValue::Str(f.name.clone())),
                        ("Script", n(f.script.unwrap_or(0.0))),
                        ("FontType", n(f.font_type.unwrap_or(0.0))),
                        ("Synthetic", n(f.synthetic.unwrap_or(0.0))),
                    ])
                })
                .collect(),
        );

        d(vec![
            ("KinsokuSet", kinsoku_set),
            ("MojiKumiSet", mojikumi_set),
            ("TheNormalStyleSheet", n(0.0)),
            ("TheNormalParagraphSheet", n(0.0)),
            ("ParagraphSheetSet", paragraph_sheet_set),
            ("StyleSheetSet", style_sheet_set),
            ("FontSet", font_set),
            ("SuperscriptSize", n(data.superscript_size.unwrap_or(0.583))),
            ("SuperscriptPosition", n(data.superscript_position.unwrap_or(0.333))),
            ("SubscriptSize", n(data.subscript_size.unwrap_or(0.583))),
            ("SubscriptPosition", n(data.subscript_position.unwrap_or(0.333))),
            ("SmallCapSize", n(data.small_cap_size.unwrap_or(0.7))),
        ])
    };

    let resource_dict = build_resources();
    let document_resources = build_resources();

    // ---- engine dict ----
    let engine_dict = d(vec![
        ("Editor", d(vec![("Text", EngineValue::Str(text.clone()))])),
        (
            "ParagraphRun",
            d(vec![
                (
                    "DefaultRunData",
                    d(vec![
                        (
                            "ParagraphSheet",
                            d(vec![
                                ("DefaultStyleSheet", n(0.0)),
                                ("Properties", EngineValue::Dict(Vec::new())),
                            ]),
                        ),
                        (
                            "Adjustments",
                            d(vec![("Axis", num_arr(&[1.0, 0.0, 1.0])), ("XY", num_arr(&[0.0, 0.0]))]),
                        ),
                    ]),
                ),
                ("RunArray", EngineValue::Array(paragraph_run_array)),
                ("RunLengthArray", num_arr(&paragraph_run_length_array)),
                ("IsJoinable", n(1.0)),
            ]),
        ),
        (
            "StyleRun",
            d(vec![
                (
                    "DefaultRunData",
                    d(vec![(
                        "StyleSheet",
                        d(vec![("StyleSheetData", EngineValue::Dict(Vec::new()))]),
                    )]),
                ),
                ("RunArray", EngineValue::Array(style_run_array)),
                ("RunLengthArray", num_arr(&style_run_length_array)),
                ("IsJoinable", n(2.0)),
            ]),
        ),
        (
            "GridInfo",
            d(vec![
                ("GridIsOn", b(grid_info.is_on.unwrap_or(false))),
                ("ShowGrid", b(grid_info.show.unwrap_or(false))),
                ("GridSize", n(grid_info.size.unwrap_or(18.0))),
                ("GridLeading", n(grid_info.leading.unwrap_or(22.0))),
                ("GridColor", encode_color(grid_info.color.as_ref())),
                ("GridLeadingFillColor", encode_color(grid_info.color.as_ref())),
                ("AlignLineHeightToGridFlags", b(grid_info.align_line_height_to_grid_flags.unwrap_or(false))),
            ]),
        ),
        (
            "AntiAlias",
            n(antialias_index(data.anti_alias.unwrap_or(AntiAlias::Sharp)) as f64),
        ),
        (
            "UseFractionalGlyphWidths",
            b(data.use_fractional_glyph_widths.unwrap_or(true)),
        ),
        (
            "Rendered",
            d(vec![
                ("Version", n(1.0)),
                (
                    "Shapes",
                    d(vec![
                        ("WritingDirection", n(writing_direction)),
                        (
                            "Children",
                            EngineValue::Array(vec![d(vec![
                                ("ShapeType", n(shape_type)),
                                ("Procession", n(procession)),
                                (
                                    "Lines",
                                    d(vec![
                                        ("WritingDirection", n(writing_direction)),
                                        ("Children", EngineValue::Array(Vec::new())),
                                    ]),
                                ),
                                ("Cookie", d(vec![("Photoshop", photoshop)])),
                            ])]),
                        ),
                    ]),
                ),
            ]),
        ),
    ]);

    d(vec![
        ("EngineDict", engine_dict),
        ("ResourceDict", resource_dict),
        ("DocumentResources", document_resources),
    ])
}

/// `(text || '').replace(/\r?\n/g, '\r') + '\r'`
fn normalize_text(text: &str) -> String {
    // replace \r\n and \n with \r, leave standalone \r as-is
    let mut out = String::with_capacity(text.len() + 1);
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '\r' {
            if i + 1 < bytes.len() && bytes[i + 1] == '\n' {
                // \r\n -> \r
                out.push('\r');
                i += 2;
            } else {
                // standalone \r — regex /\r?\n/ would not match it (no \n), kept as-is
                out.push('\r');
                i += 1;
            }
        } else if c == '\n' {
            out.push('\r');
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out.push('\r');
    out
}

fn merge_grid(base: &TextGridInfo, over: Option<&TextGridInfo>) -> TextGridInfo {
    let mut r = base.clone();
    if let Some(o) = over {
        if o.is_on.is_some() {
            r.is_on = o.is_on;
        }
        if o.show.is_some() {
            r.show = o.show;
        }
        if o.size.is_some() {
            r.size = o.size;
        }
        if o.leading.is_some() {
            r.leading = o.leading;
        }
        if o.color.is_some() {
            r.color = o.color;
        }
        if o.leading_fill_color.is_some() {
            r.leading_fill_color = o.leading_fill_color;
        }
        if o.align_line_height_to_grid_flags.is_some() {
            r.align_line_height_to_grid_flags = o.align_line_height_to_grid_flags;
        }
    }
    r
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_data::{parse_engine_data, serialize_engine_data};

    fn keys_of(v: &EngineValue) -> Vec<String> {
        match v {
            EngineValue::Dict(m) => m.iter().map(|(k, _)| k.clone()).collect(),
            _ => vec![],
        }
    }

    #[test]
    fn round_trip_two_style_runs() {
        let data = LayerTextData {
            text: "Hello\nWorld".to_string(),
            anti_alias: Some(AntiAlias::Smooth),
            orientation: Some(Orientation::Horizontal),
            paragraph_style: Some(ParagraphStyle {
                justification: Some(Justification::Center),
                ..Default::default()
            }),
            style_runs: Some(vec![
                TextStyleRun {
                    length: 5.0,
                    style: TextStyle {
                        font: Some(Font {
                            name: "Arial".to_string(),
                            script: Some(0.0),
                            font_type: Some(0.0),
                            synthetic: Some(0.0),
                        }),
                        font_size: Some(24.0),
                        fill_color: Some(Color::Rgb(Rgb { r: 255.0, g: 0.0, b: 0.0 })),
                        ..Default::default()
                    },
                },
                TextStyleRun {
                    length: 6.0,
                    style: TextStyle {
                        font: Some(Font {
                            name: "Arial".to_string(),
                            script: Some(0.0),
                            font_type: Some(0.0),
                            synthetic: Some(0.0),
                        }),
                        font_size: Some(24.0),
                        fill_color: Some(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 255.0 })),
                        ..Default::default()
                    },
                },
            ]),
            ..Default::default()
        };

        let encoded = encode_engine_data(&data);
        // serialize then parse to ensure bytes survive the round trip
        let bytes = serialize_engine_data(&encoded, false);
        let parsed = parse_engine_data(&bytes).unwrap();
        assert_eq!(parsed, encoded, "serialize/parse round-trip must be stable");

        let decoded = decode_engine_data(&parsed);

        // text: \n preserved (was \r in EngineData, decoded back to \n)
        assert_eq!(decoded.text, "Hello\nWorld");
        assert_eq!(decoded.anti_alias, Some(AntiAlias::Smooth));

        // style runs survived
        let runs = decoded.style_runs.expect("style runs present");
        assert_eq!(runs.len(), 2);
        // lengths: 5 and 6 (trailing \r folded into the last run during encode,
        // then trimmed back on decode)
        assert_eq!(runs[0].length, 5.0);
        assert_eq!(runs[1].length, 6.0);

        // distinguishing field: fill color differs between runs, so it stays per-run
        assert_eq!(
            runs[0].style.fill_color,
            Some(Color::Rgb(Rgb { r: 255.0, g: 0.0, b: 0.0 }))
        );
        assert_eq!(
            runs[1].style.fill_color,
            Some(Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 255.0 }))
        );

        // shared font dedups into base style
        assert_eq!(decoded.style.as_ref().unwrap().font.as_ref().unwrap().name, "Arial");
        assert_eq!(decoded.style.as_ref().unwrap().font_size, Some(24.0));

        // paragraph: justification center round-trips
        let pruns = decoded.paragraph_style_runs.as_ref();
        let just = decoded
            .paragraph_style
            .as_ref()
            .and_then(|p| p.justification)
            .or_else(|| pruns.and_then(|r| r.first()).and_then(|r| r.style.justification));
        assert_eq!(just, Some(Justification::Center));
    }

    #[test]
    fn default_sheets_key_sets() {
        // empty data exercises the default sheets path
        let data = LayerTextData {
            text: "x".to_string(),
            ..Default::default()
        };
        let encoded = encode_engine_data(&data);

        // top-level keys
        assert_eq!(
            keys_of(&encoded),
            vec!["EngineDict", "ResourceDict", "DocumentResources"]
        );

        // ResourceDict key set + order
        let rd = get(&encoded, "ResourceDict").unwrap();
        assert_eq!(
            keys_of(rd),
            vec![
                "KinsokuSet",
                "MojiKumiSet",
                "TheNormalStyleSheet",
                "TheNormalParagraphSheet",
                "ParagraphSheetSet",
                "StyleSheetSet",
                "FontSet",
                "SuperscriptSize",
                "SuperscriptPosition",
                "SubscriptSize",
                "SubscriptPosition",
                "SmallCapSize",
            ]
        );

        // EngineDict key set + order
        let ed = get(&encoded, "EngineDict").unwrap();
        assert_eq!(
            keys_of(ed),
            vec![
                "Editor",
                "ParagraphRun",
                "StyleRun",
                "GridInfo",
                "AntiAlias",
                "UseFractionalGlyphWidths",
                "Rendered",
            ]
        );

        // default paragraph sheet Properties must carry all 21 default keys in order
        let props = get(rd, "ParagraphSheetSet")
            .and_then(as_array)
            .and_then(|a| a.first())
            .and_then(|s| get(s, "Properties"))
            .unwrap();
        assert_eq!(
            keys_of(props),
            vec![
                "Justification",
                "FirstLineIndent",
                "StartIndent",
                "EndIndent",
                "SpaceBefore",
                "SpaceAfter",
                "AutoHyphenate",
                "HyphenatedWordSize",
                "PreHyphen",
                "PostHyphen",
                "ConsecutiveHyphens",
                "Zone",
                "WordSpacing",
                "LetterSpacing",
                "GlyphSpacing",
                "AutoLeading",
                "LeadingType",
                "Hanging",
                "Burasagari",
                "KinsokuOrder",
                "EveryLineComposer",
            ]
        );

        // default style sheet data: first key is Font, includes FillColor/StrokeColor
        let ssd = get(rd, "StyleSheetSet")
            .and_then(as_array)
            .and_then(|a| a.first())
            .and_then(|s| get(s, "StyleSheetData"))
            .unwrap();
        let ssd_keys = keys_of(ssd);
        assert_eq!(ssd_keys.first().map(|s| s.as_str()), Some("Font"));
        assert!(ssd_keys.contains(&"FillColor".to_string()));
        assert!(ssd_keys.contains(&"DiacriticPos".to_string()));

        // FontSet: AdobeInvisFont first, then the default MyriadPro-Regular
        let font_set = get(rd, "FontSet").and_then(as_array).unwrap();
        assert_eq!(
            get(&font_set[0], "Name").and_then(as_str),
            Some("AdobeInvisFont")
        );
        assert_eq!(
            get(&font_set[1], "Name").and_then(as_str),
            Some("MyriadPro-Regular")
        );
    }

    #[test]
    fn color_round_trip() {
        // rgb
        let c = encode_color(Some(&Color::Rgb(Rgb { r: 255.0, g: 128.0, b: 0.0 })));
        match decode_color(&c).unwrap() {
            Color::Rgb(rgb) => {
                assert!((rgb.r - 255.0).abs() < 1e-6);
                assert!((rgb.g - 128.0).abs() < 1e-6);
                assert!((rgb.b - 0.0).abs() < 1e-6);
            }
            _ => panic!("expected rgb"),
        }
        // grayscale
        let c = encode_color(Some(&Color::Grayscale(Grayscale { k: 200.0 })));
        match decode_color(&c).unwrap() {
            Color::Grayscale(g) => assert!((g.k - 200.0).abs() < 1e-6),
            _ => panic!("expected grayscale"),
        }
        // none -> default
        let c = encode_color(None);
        assert_eq!(get(&c, "Type").and_then(as_number), Some(1.0));
    }

    #[test]
    fn paragraph_runs_from_newlines() {
        // No paragraph_style_runs supplied: encode splits on \r boundaries.
        let data = LayerTextData {
            text: "a\nb\nc".to_string(),
            ..Default::default()
        };
        let encoded = encode_engine_data(&data);
        let pr = get(&encoded, "EngineDict").and_then(|e| get(e, "ParagraphRun")).unwrap();
        let lens = get(pr, "RunLengthArray").map(num_array).unwrap();
        // text becomes "a\rb\rc\r" (len 6): runs end at each \r -> [2,2,2]
        assert_eq!(lens, vec![2.0, 2.0, 2.0]);
    }
}
