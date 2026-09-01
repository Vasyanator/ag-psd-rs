/*
File: crates/ag-psd/tests/fixtures.rs

Purpose:
Integration harness over the fixture corpus of the original ag-psd library
(`test/ag-psd/test`). It sweeps every `src.psd` and, where upstream ships a
`data.json` dump of its own read result, compares the ported reader against that
ground truth.

What is checked:
- `read_all_fixtures`: every `src.psd` reads without an error or a panic. The set of
  failing fixtures must equal `EXPECTED_READ_FAILURES` exactly.
- `round_trip_all_fixtures`: read -> `write_psd` -> read is structurally stable
  (width/height/colorMode, child counts, recursive layer count, per-layer
  name/opacity/blendMode/bounds). The set of failing fixtures must equal
  `EXPECTED_ROUND_TRIP_FAILURES` exactly.
- `photoshop_2026_blend_modes_match_ground_truth`: the `read/2026-blend-modes` fixture
  (29 layers, one per blend mode, saved by Photoshop 2026) decodes to exactly the blend
  modes recorded in its `data.json` — both layer blend modes and layer *effect* blend
  modes. The two travel different decode paths (layer records store the historical
  4-character code, effects store a descriptor enum), and only the descriptor path is
  affected by Photoshop 2026's long-form enum ids.
- `structure_matches_ground_truth`: for every `read/` fixture that ships a `data.json`,
  the document header (width/height/channels/bitsPerChannel/colorMode) and the layer
  tree (name, bounds, blendMode) match the dump. The set of diverging fixtures must
  equal `EXPECTED_GROUND_TRUTH_FAILURES` exactly.

All three failure sets are pinned as allowlists instead of as a success ratio: a newly
failing fixture *and* a newly passing one both fail the suite, so the lists cannot rot
into meaninglessness.

Key items:
- `json`: a minimal JSON reader — the crate deliberately has no JSON dependency, and
  none is added for tests.
- `EXPECTED_READ_FAILURES` / `EXPECTED_ROUND_TRIP_FAILURES` /
  `EXPECTED_GROUND_TRUTH_FAILURES`: the pinned failure sets.
- `compare_psd` / `compare_to_ground_truth`: self-consistency vs. ground-truth checks.

Notes:
The corpus lives outside the crate (`$CARGO_MANIFEST_DIR/../../test/ag-psd/test`) and is
not part of the published package, so every test here skips silently when it is absent.
*/

use std::collections::BTreeMap;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use ag_psd::psd::{
    BlendMode, ColorMode, Layer, LayerEffectsInfo, Psd, ReadOptions, WriteOptions,
};
use ag_psd::reader::ReadError;
use ag_psd::{read_psd, write_psd};

use json::Json;

// --- Pinned expected-failure sets -------------------------------------------
//
// These replace the old success-ratio thresholds, which were slack enough
// (50% read / 90% round-trip against 99% / 93% actual) that half the corpus could
// break without turning the suite red.

/// Fixtures that are expected to fail `read_psd`, by fixture name.
///
/// `read/cmyk` is faithful to upstream: the JS test suite skips it as well
/// (`psdReader.spec.ts` filters `/cmyk/`), because CMYK cannot be converted to RGB.
/// Any other read failure is a regression; any entry here that starts passing must be
/// removed rather than left to rot.
const EXPECTED_READ_FAILURES: &[&str] = &["read/cmyk"];

/// Fixtures that are expected to fail the read -> write -> read round trip, by name.
///
/// These are the remaining upstream *writer* mode limitations.
///   - `read/bitmap`, `read/bitmap-rle` — bitmap mode is not RGB;
///   - `read/grayscale`, `read/grayscale-alpha` — colorMode Grayscale is written as RGB;
///   - `read/indexed` — colorMode Indexed is written as RGB.
const EXPECTED_ROUND_TRIP_FAILURES: &[&str] = &[
    "read/bitmap",
    "read/bitmap-rle",
    "read/grayscale",
    "read/grayscale-alpha",
    "read/indexed",
];

/// Fixtures that are expected to diverge from their `data.json` ground truth, by name.
///
/// Empty: every fixture that ships a `data.json` matches it. The list is kept (and the
/// harness keeps comparing against it) so that a future divergence has to be recorded
/// here deliberately instead of passing unnoticed.
const EXPECTED_GROUND_TRUTH_FAILURES: &[&str] = &[];

/// Upper bound on divergences reported per fixture, so one badly broken fixture cannot
/// bury the rest of the report in output.
const MAX_DIFFS_PER_FIXTURE: usize = 20;

// ===========================================================================
// Minimal JSON reader
// ===========================================================================

/// A minimal JSON reader, sufficient for upstream's `data.json` ground-truth dumps.
///
/// The crate has no JSON dependency and none is added for tests, so this parses the
/// full JSON grammar (including `\uXXXX` escapes and surrogate pairs) in ~150 lines.
/// Input is trusted fixture data: parsing is recursive and has no depth limit.
mod json {
    /// A parsed JSON value. Object fields keep source order; the dumps never repeat a key.
    #[derive(Debug, Clone, PartialEq)]
    pub enum Json {
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array(Vec<Json>),
        Object(Vec<(String, Json)>),
    }

    impl Json {
        /// Field lookup on an object; `None` for a missing key or a non-object value.
        #[must_use]
        pub fn get(&self, key: &str) -> Option<&Json> {
            match self {
                Json::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                Json::Null
                | Json::Bool(_)
                | Json::Number(_)
                | Json::String(_)
                | Json::Array(_) => None,
            }
        }

        /// The string payload, or `None` for any other kind.
        #[must_use]
        pub fn as_str(&self) -> Option<&str> {
            match self {
                Json::String(s) => Some(s),
                Json::Null
                | Json::Bool(_)
                | Json::Number(_)
                | Json::Array(_)
                | Json::Object(_) => None,
            }
        }

        /// The numeric payload, or `None` for any other kind.
        #[must_use]
        pub fn as_f64(&self) -> Option<f64> {
            match self {
                Json::Number(n) => Some(*n),
                Json::Null
                | Json::Bool(_)
                | Json::String(_)
                | Json::Array(_)
                | Json::Object(_) => None,
            }
        }

        /// The array payload, or `None` for any other kind.
        #[must_use]
        pub fn as_array(&self) -> Option<&[Json]> {
            match self {
                Json::Array(items) => Some(items),
                Json::Null
                | Json::Bool(_)
                | Json::Number(_)
                | Json::String(_)
                | Json::Object(_) => None,
            }
        }
    }

    /// Parses a complete JSON document.
    ///
    /// # Errors
    /// Returns a human-readable message with the offending byte offset for malformed
    /// input or for trailing data after the top-level value.
    pub fn parse(input: &str) -> Result<Json, String> {
        let mut parser = Parser {
            bytes: input.as_bytes(),
            pos: 0,
        };
        parser.skip_ws();
        let value = parser.value()?;
        parser.skip_ws();
        if parser.pos != parser.bytes.len() {
            return Err(format!("trailing data at byte {}", parser.pos));
        }
        Ok(value)
    }

    /// Byte cursor over the document. Positions in error messages are byte offsets.
    struct Parser<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Parser<'_> {
        fn skip_ws(&mut self) {
            while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.pos += 1;
            }
        }

        fn peek(&self) -> Option<u8> {
            self.bytes.get(self.pos).copied()
        }

        fn expect(&mut self, byte: u8) -> Result<(), String> {
            if self.peek() == Some(byte) {
                self.pos += 1;
                Ok(())
            } else {
                Err(format!(
                    "expected '{}' at byte {}",
                    char::from(byte),
                    self.pos
                ))
            }
        }

        fn value(&mut self) -> Result<Json, String> {
            match self.peek() {
                Some(b'{') => self.object(),
                Some(b'[') => self.array(),
                Some(b'"') => self.string().map(Json::String),
                Some(b't') => self.literal("true", Json::Bool(true)),
                Some(b'f') => self.literal("false", Json::Bool(false)),
                Some(b'n') => self.literal("null", Json::Null),
                Some(_) => self.number(),
                None => Err("unexpected end of input".to_string()),
            }
        }

        fn literal(&mut self, text: &str, value: Json) -> Result<Json, String> {
            if self.bytes[self.pos..].starts_with(text.as_bytes()) {
                self.pos += text.len();
                Ok(value)
            } else {
                Err(format!("expected '{text}' at byte {}", self.pos))
            }
        }

        fn number(&mut self) -> Result<Json, String> {
            let start = self.pos;
            while let Some(byte) = self.peek() {
                if byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.pos += 1;
                } else {
                    break;
                }
            }
            // The slice is ASCII by construction, so `from_utf8` cannot fail here.
            let text = std::str::from_utf8(&self.bytes[start..self.pos])
                .map_err(|e| format!("invalid utf-8 in number at byte {start}: {e}"))?;
            text.parse::<f64>()
                .map(Json::Number)
                .map_err(|e| format!("invalid number '{text}' at byte {start}: {e}"))
        }

        fn string(&mut self) -> Result<String, String> {
            self.expect(b'"')?;
            let mut out: Vec<u8> = Vec::new();
            loop {
                let byte = self
                    .peek()
                    .ok_or_else(|| format!("unterminated string at byte {}", self.pos))?;
                self.pos += 1;
                match byte {
                    b'"' => break,
                    b'\\' => {
                        let escape = self
                            .peek()
                            .ok_or_else(|| format!("unterminated escape at byte {}", self.pos))?;
                        self.pos += 1;
                        match escape {
                            b'"' => out.push(b'"'),
                            b'\\' => out.push(b'\\'),
                            b'/' => out.push(b'/'),
                            b'b' => out.push(0x08),
                            b'f' => out.push(0x0c),
                            b'n' => out.push(b'\n'),
                            b'r' => out.push(b'\r'),
                            b't' => out.push(b'\t'),
                            b'u' => {
                                let ch = self.unicode_escape()?;
                                let mut buf = [0u8; 4];
                                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                            }
                            other => {
                                return Err(format!(
                                    "invalid escape '\\{}' at byte {}",
                                    char::from(other),
                                    self.pos
                                ));
                            }
                        }
                    }
                    other => out.push(other),
                }
            }
            String::from_utf8(out).map_err(|e| format!("invalid utf-8 in string: {e}"))
        }

        /// Reads exactly four hex digits and returns them as a UTF-16 code unit.
        fn hex4(&mut self) -> Result<u32, String> {
            let start = self.pos;
            let end = start + 4;
            if end > self.bytes.len() {
                return Err(format!("truncated \\u escape at byte {start}"));
            }
            let text = std::str::from_utf8(&self.bytes[start..end])
                .map_err(|e| format!("invalid utf-8 in \\u escape at byte {start}: {e}"))?;
            let unit = u32::from_str_radix(text, 16)
                .map_err(|e| format!("invalid \\u escape '{text}' at byte {start}: {e}"))?;
            self.pos = end;
            Ok(unit)
        }

        /// Decodes a `\uXXXX` escape, joining a UTF-16 surrogate pair when present.
        fn unicode_escape(&mut self) -> Result<char, String> {
            let first = self.hex4()?;
            // A high surrogate is only valid when immediately followed by `\uDC00..\uDFFF`;
            // the two halves together encode one code point above the BMP.
            if (0xD800..0xDC00).contains(&first) {
                self.expect(b'\\')?;
                self.expect(b'u')?;
                let second = self.hex4()?;
                if !(0xDC00..0xE000).contains(&second) {
                    return Err(format!(
                        "unpaired high surrogate {first:#06x} at byte {}",
                        self.pos
                    ));
                }
                let code_point = 0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                return char::from_u32(code_point)
                    .ok_or_else(|| format!("invalid code point {code_point:#x}"));
            }
            char::from_u32(first).ok_or_else(|| format!("invalid code point {first:#x}"))
        }

        fn array(&mut self) -> Result<Json, String> {
            self.expect(b'[')?;
            let mut items = Vec::new();
            self.skip_ws();
            if self.peek() == Some(b']') {
                self.pos += 1;
                return Ok(Json::Array(items));
            }
            loop {
                self.skip_ws();
                items.push(self.value()?);
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b']') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Err(format!("expected ',' or ']' at byte {}", self.pos)),
                }
            }
            Ok(Json::Array(items))
        }

        fn object(&mut self) -> Result<Json, String> {
            self.expect(b'{')?;
            let mut fields = Vec::new();
            self.skip_ws();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                return Ok(Json::Object(fields));
            }
            loop {
                self.skip_ws();
                let key = self.string()?;
                self.skip_ws();
                self.expect(b':')?;
                self.skip_ws();
                let value = self.value()?;
                fields.push((key, value));
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b'}') => {
                        self.pos += 1;
                        break;
                    }
                    _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
                }
            }
            Ok(Json::Object(fields))
        }
    }
}

// ===========================================================================
// Fixture discovery
// ===========================================================================

/// Корень эталонных фикстур (read-only). `crates/ag-psd` -> корень репо -> test.
fn fixtures_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test/ag-psd/test"))
}

/// Рекурсивно собрать все `src.psd`. Только std::fs, без внешних крейтов.
fn discover_fixtures(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("src.psd") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Человекочитаемое имя фикстуры: путь относительно корня без `/src.psd`.
///
/// Separators are normalized to `/` so that the expected-failure allowlists are a
/// single platform-independent spelling.
fn fixture_name(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let s = rel.to_string_lossy().replace('\\', "/");
    s.strip_suffix("/src.psd").unwrap_or(&s).to_string()
}

/// Обёртка: поймать панику и привести её к строке сообщения.
fn catch_panic_msg<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    let prev = panic::take_hook();
    panic::set_hook(Box::new(|_| {})); // глушим стандартный вывод паники
    let res = panic::catch_unwind(AssertUnwindSafe(f));
    panic::set_hook(prev);
    res.map_err(|e| {
        if let Some(s) = e.downcast_ref::<&str>() {
            format!("panic: {s}")
        } else if let Some(s) = e.downcast_ref::<String>() {
            format!("panic: {s}")
        } else {
            "panic: <non-string payload>".to_string()
        }
    })
}

fn default_read_options() -> ReadOptions {
    ReadOptions::default()
}

/// Грубая классификация причины падения для группировки в сводке.
fn classify(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    if m.starts_with("panic") {
        if m.contains("index out of") || m.contains("range end index") || m.contains("slice") {
            "panic: index/slice out of bounds"
        } else if m.contains("unwrap") || m.contains("none") || m.contains("unwrap_none") {
            "panic: unwrap on None/Err"
        } else if m.contains("overflow") {
            "panic: arithmetic overflow"
        } else if m.contains("utf-8") || m.contains("utf8") {
            "panic: invalid utf-8"
        } else if m.contains("not yet") || m.contains("todo") || m.contains("unimplemented") {
            "panic: unimplemented/todo"
        } else {
            "panic: other"
        }
    } else if m.contains("invalid signature") {
        "read error: invalid signature"
    } else if m.contains("exceeding buffer") || m.contains("exceeds file") || m.contains("past end")
    {
        "read error: buffer/section bounds"
    } else if m.contains("4gb") {
        "read error: size too large"
    } else {
        "read error: other"
    }
}

/// Compares an observed failure set against a pinned allowlist.
///
/// `failures` are the fixtures that failed this sweep, `exercised` every fixture the
/// sweep actually ran. Returns one actionable message per problem: a `REGRESSION` for a
/// failure that is not allowlisted, a `FIXED` entry for an allowlisted fixture that now
/// passes (so the list gets trimmed instead of rotting) and a `STALE` entry for an
/// allowlisted fixture that no longer exists in the corpus. An empty result means the
/// observed set matches the allowlist exactly.
fn allowlist_problems(
    what: &str,
    failures: &[(String, String)],
    allowlist: &[&str],
    exercised: &[String],
) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, msg) in failures {
        if !allowlist.contains(&name.as_str()) {
            problems.push(format!("REGRESSION: {name} now fails {what}: {msg}"));
        }
    }
    for expected in allowlist {
        if failures.iter().any(|(name, _)| name == expected) {
            continue;
        }
        if exercised.iter().any(|name| name == expected) {
            problems.push(format!(
                "FIXED: {expected} no longer fails {what} — remove it from the allowlist"
            ));
        } else {
            problems.push(format!(
                "STALE: {expected} is allowlisted for {what} but no such fixture was exercised — \
                 remove it from the allowlist"
            ));
        }
    }
    problems
}

// ===========================================================================
// Structural self-consistency (read -> write -> read)
// ===========================================================================

/// Рекурсивно подсчитать общее число слоёв (включая группы и их детей).
fn count_layers(children: &Option<Vec<Layer>>) -> usize {
    match children {
        None => 0,
        Some(layers) => layers.iter().map(|l| 1 + count_layers(&l.children)).sum(),
    }
}

/// Структурная сверка одного дерева слоёв. Возвращает первое расхождение.
fn compare_layers(a: &[Layer], b: &[Layer], path: &str) -> Result<(), String> {
    if a.len() != b.len() {
        return Err(format!("{path}: children count {} != {}", a.len(), b.len()));
    }
    for (i, (la, lb)) in a.iter().zip(b.iter()).enumerate() {
        let here = format!("{path}[{i}]");
        let na = la.additional_info.name.as_deref();
        let nb = lb.additional_info.name.as_deref();
        if na != nb {
            return Err(format!("{here}: name {na:?} != {nb:?}"));
        }
        if la.opacity != lb.opacity {
            return Err(format!(
                "{here} ({na:?}): opacity {:?} != {:?}",
                la.opacity, lb.opacity
            ));
        }
        if la.blend_mode != lb.blend_mode {
            return Err(format!(
                "{here} ({na:?}): blendMode {:?} != {:?}",
                la.blend_mode, lb.blend_mode
            ));
        }
        let bounds_a = (la.top, la.left, la.bottom, la.right);
        let bounds_b = (lb.top, lb.left, lb.bottom, lb.right);
        if bounds_a != bounds_b {
            return Err(format!("{here} ({na:?}): bounds {bounds_a:?} != {bounds_b:?}"));
        }
        match (&la.children, &lb.children) {
            (Some(ca), Some(cb)) => compare_layers(ca, cb, &here)?,
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                return Err(format!("{here} ({na:?}): children presence differs"));
            }
        }
    }
    Ok(())
}

/// Структурная стабильность двух Psd. Возвращает первое расхождение.
fn compare_psd(a: &Psd, b: &Psd) -> Result<(), String> {
    if a.width != b.width {
        return Err(format!("width {} != {}", a.width, b.width));
    }
    if a.height != b.height {
        return Err(format!("height {} != {}", a.height, b.height));
    }
    let cma: Option<ColorMode> = a.color_mode;
    let cmb: Option<ColorMode> = b.color_mode;
    if cma != cmb {
        return Err(format!("colorMode {cma:?} != {cmb:?}"));
    }
    let ta = count_layers(&a.children);
    let tb = count_layers(&b.children);
    if ta != tb {
        return Err(format!("total layer count {ta} != {tb}"));
    }
    let empty: Vec<Layer> = Vec::new();
    let ca = a.children.as_ref().unwrap_or(&empty);
    let cb = b.children.as_ref().unwrap_or(&empty);
    compare_layers(ca, cb, "root")
}

// ===========================================================================
// Ground-truth comparison against `data.json`
// ===========================================================================

/// The blend-mode spelling upstream writes into `data.json` (the TS string union).
fn blend_mode_name(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::PassThrough => "pass through",
        BlendMode::Normal => "normal",
        BlendMode::Dissolve => "dissolve",
        BlendMode::Darken => "darken",
        BlendMode::Multiply => "multiply",
        BlendMode::ColorBurn => "color burn",
        BlendMode::LinearBurn => "linear burn",
        BlendMode::DarkerColor => "darker color",
        BlendMode::Lighten => "lighten",
        BlendMode::Screen => "screen",
        BlendMode::ColorDodge => "color dodge",
        BlendMode::LinearDodge => "linear dodge",
        BlendMode::LighterColor => "lighter color",
        BlendMode::Overlay => "overlay",
        BlendMode::SoftLight => "soft light",
        BlendMode::HardLight => "hard light",
        BlendMode::VividLight => "vivid light",
        BlendMode::LinearLight => "linear light",
        BlendMode::PinLight => "pin light",
        BlendMode::HardMix => "hard mix",
        BlendMode::Difference => "difference",
        BlendMode::Exclusion => "exclusion",
        BlendMode::Subtract => "subtract",
        BlendMode::Divide => "divide",
        BlendMode::Hue => "hue",
        BlendMode::Saturation => "saturation",
        BlendMode::Color => "color",
        BlendMode::Luminosity => "luminosity",
        BlendMode::LinearHeight => "linear height",
        BlendMode::Height => "height",
        BlendMode::Subtraction => "subtraction",
    }
}

/// The numeric colour-mode code stored in the PSD header and dumped to `data.json`.
fn color_mode_code(mode: ColorMode) -> u16 {
    match mode {
        ColorMode::Bitmap => 0,
        ColorMode::Grayscale => 1,
        ColorMode::Indexed => 2,
        ColorMode::Rgb => 3,
        ColorMode::Cmyk => 4,
        ColorMode::Multichannel => 7,
        ColorMode::Duotone => 8,
        ColorMode::Lab => 9,
    }
}

/// The `effects` sub-objects that carry a plain `blendMode` field, in dump order.
///
/// The flag marks the slots upstream serializes as an array (Photoshop allows several
/// instances of those effects) as opposed to a single object. `bevel` is deliberately
/// absent: it carries `highlightBlendMode`/`shadowBlendMode` instead.
const EFFECT_BLEND_MODE_SLOTS: &[(&str, bool)] = &[
    ("dropShadow", true),
    ("innerShadow", true),
    ("outerGlow", false),
    ("innerGlow", false),
    ("solidFill", true),
    ("gradientOverlay", true),
    ("satin", false),
    ("stroke", true),
];

/// Blend modes carried by a layer's effects as `(slot, index, mode)` triples.
///
/// Emitted in `EFFECT_BLEND_MODE_SLOTS` order so the result lines up element-wise with
/// [`json_effect_blend_modes`] over the same layer's `data.json` entry.
fn effect_blend_modes(effects: &LayerEffectsInfo) -> Vec<(&'static str, usize, Option<String>)> {
    let mut out = Vec::new();
    let named = |mode: Option<BlendMode>| mode.map(|m| blend_mode_name(m).to_string());

    for (i, e) in effects.drop_shadow.iter().flatten().enumerate() {
        out.push(("dropShadow", i, named(e.blend_mode)));
    }
    for (i, e) in effects.inner_shadow.iter().flatten().enumerate() {
        out.push(("innerShadow", i, named(e.blend_mode)));
    }
    if let Some(e) = &effects.outer_glow {
        out.push(("outerGlow", 0, named(e.blend_mode)));
    }
    if let Some(e) = &effects.inner_glow {
        out.push(("innerGlow", 0, named(e.blend_mode)));
    }
    for (i, e) in effects.solid_fill.iter().flatten().enumerate() {
        out.push(("solidFill", i, named(e.blend_mode)));
    }
    // Gradient overlay stores its blend mode as a raw string upstream, and so does the port.
    for (i, e) in effects.gradient_overlay.iter().flatten().enumerate() {
        out.push(("gradientOverlay", i, e.blend_mode.clone()));
    }
    if let Some(e) = &effects.satin {
        out.push(("satin", 0, named(e.blend_mode)));
    }
    for (i, e) in effects.stroke.iter().flatten().enumerate() {
        out.push(("stroke", i, named(e.blend_mode)));
    }
    out
}

/// The same `(slot, index, mode)` triples read out of a `data.json` `effects` object.
fn json_effect_blend_modes(effects: &Json) -> Vec<(&'static str, usize, Option<String>)> {
    let mut out = Vec::new();
    for (slot, is_array) in EFFECT_BLEND_MODE_SLOTS {
        let blend_mode = |value: &Json| {
            value
                .get("blendMode")
                .and_then(Json::as_str)
                .map(str::to_string)
        };
        match effects.get(slot) {
            None | Some(Json::Null) => {}
            Some(value) if *is_array => {
                for (i, item) in value.as_array().unwrap_or(&[]).iter().enumerate() {
                    out.push((*slot, i, blend_mode(item)));
                }
            }
            Some(value) => out.push((*slot, 0, blend_mode(value))),
        }
    }
    out
}

/// Loads and parses the `data.json` next to a fixture.
///
/// Returns `None` when the fixture ships no ground truth (upstream `read-write/*`
/// fixtures compare against a binary `expected.psd` instead), `Some(Err)` when the file
/// exists but cannot be read or parsed.
fn load_ground_truth(fixture_dir: &Path) -> Option<Result<Json, String>> {
    let path = fixture_dir.join("data.json");
    if !path.is_file() {
        return None;
    }
    Some(
        fs::read_to_string(&path)
            .map_err(|e| format!("io: {e}"))
            .and_then(|text| json::parse(&text)),
    )
}

/// Records a divergence when an integral ground-truth number disagrees with the parsed
/// value. Absent on both sides is a match; the compared fields are all integral, so the
/// comparison is exact by design.
fn compare_number(diffs: &mut Vec<String>, path: &str, expected: Option<f64>, actual: Option<f64>) {
    if expected != actual {
        diffs.push(format!("{path}: expected {expected:?}, got {actual:?}"));
    }
}

/// Records a divergence when a ground-truth string disagrees with the parsed value.
fn compare_str(diffs: &mut Vec<String>, path: &str, expected: Option<&str>, actual: Option<&str>) {
    if expected != actual {
        diffs.push(format!("{path}: expected {expected:?}, got {actual:?}"));
    }
}

/// Compares a layer tree against the `children` array of a `data.json` dump on the
/// fields that carry the same meaning on both sides: name, bounds and blend mode.
///
/// Stops adding messages once `MAX_DIFFS_PER_FIXTURE` is reached.
fn compare_layer_tree(diffs: &mut Vec<String>, path: &str, expected: &[Json], actual: &[Layer]) {
    if expected.len() != actual.len() {
        diffs.push(format!(
            "{path}: layer count {} != {}",
            expected.len(),
            actual.len()
        ));
        return;
    }
    for (i, (exp, act)) in expected.iter().zip(actual.iter()).enumerate() {
        if diffs.len() >= MAX_DIFFS_PER_FIXTURE {
            diffs.push("... further differences suppressed".to_string());
            return;
        }
        let here = format!("{path}[{i}]");
        compare_str(
            diffs,
            &format!("{here}.name"),
            exp.get("name").and_then(Json::as_str),
            act.additional_info.name.as_deref(),
        );
        for (key, value) in [
            ("top", act.top),
            ("left", act.left),
            ("bottom", act.bottom),
            ("right", act.right),
        ] {
            compare_number(
                diffs,
                &format!("{here}.{key}"),
                exp.get(key).and_then(Json::as_f64),
                value,
            );
        }
        compare_str(
            diffs,
            &format!("{here}.blendMode"),
            exp.get("blendMode").and_then(Json::as_str),
            act.blend_mode.map(blend_mode_name),
        );
        match (exp.get("children").and_then(Json::as_array), &act.children) {
            (Some(ec), Some(ac)) => compare_layer_tree(diffs, &here, ec, ac),
            (None, None) => {}
            (Some(ec), None) => diffs.push(format!(
                "{here}: expected {} children, got no children array",
                ec.len()
            )),
            (None, Some(ac)) => diffs.push(format!(
                "{here}: expected no children array, got {}",
                ac.len()
            )),
        }
    }
}

/// Compares a parsed document against upstream's `data.json` dump on the fields whose
/// meaning is identical across the JS/Rust boundary: the document header and the layer
/// tree's names, bounds and blend modes. Returns one message per divergence.
fn compare_to_ground_truth(psd: &Psd, expected: &Json) -> Vec<String> {
    let mut diffs = Vec::new();
    compare_number(
        &mut diffs,
        "width",
        expected.get("width").and_then(Json::as_f64),
        Some(psd.width),
    );
    compare_number(
        &mut diffs,
        "height",
        expected.get("height").and_then(Json::as_f64),
        Some(psd.height),
    );
    compare_number(
        &mut diffs,
        "channels",
        expected.get("channels").and_then(Json::as_f64),
        psd.channels,
    );
    compare_number(
        &mut diffs,
        "bitsPerChannel",
        expected.get("bitsPerChannel").and_then(Json::as_f64),
        psd.bits_per_channel,
    );
    compare_number(
        &mut diffs,
        "colorMode",
        expected.get("colorMode").and_then(Json::as_f64),
        psd.color_mode.map(|m| f64::from(color_mode_code(m))),
    );

    let no_children: Vec<Json> = Vec::new();
    let expected_children = expected
        .get("children")
        .and_then(Json::as_array)
        .unwrap_or(&no_children);
    let empty: Vec<Layer> = Vec::new();
    let actual_children = psd.children.as_ref().unwrap_or(&empty);
    compare_layer_tree(&mut diffs, "children", expected_children, actual_children);
    diffs
}

// ===========================================================================
// Tests
// ===========================================================================

#[test]
fn read_all_fixtures() {
    let root = fixtures_root();
    // Эталонные фикстуры лежат в монорепе (test/ag-psd/test) и НЕ входят в
    // отдельный git/crates.io-репозиторий крейта. Если их нет — это
    // standalone-сборка: тихо пропускаем (unit-тесты в src/ остаются).
    if !root.is_dir() {
        eprintln!(
            "skipping fixture harness: каталог фикстур не найден ({})",
            root.display()
        );
        return;
    }

    let fixtures = discover_fixtures(&root);
    let total = fixtures.len();
    assert!(total > 0, "не найдено ни одного src.psd под {}", root.display());

    let mut ok = 0usize;
    let mut exercised: Vec<String> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();

    for path in &fixtures {
        let name = fixture_name(&root, path);
        exercised.push(name.clone());
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                failures.push((name, format!("read error: io: {e}")));
                continue;
            }
        };
        let opts = default_read_options();
        let res = catch_panic_msg(|| read_psd(&bytes, &opts));
        match res {
            Ok(Ok(_psd)) => ok += 1,
            Ok(Err(e)) => failures.push((name, format!("read error: {e}"))),
            Err(panic_msg) => failures.push((name, panic_msg)),
        }
    }

    print_read_summary(total, ok, &failures);

    let problems = allowlist_problems("reading", &failures, EXPECTED_READ_FAILURES, &exercised);
    assert!(
        problems.is_empty(),
        "read sweep no longer matches EXPECTED_READ_FAILURES ({ok}/{total} read OK):\n{}",
        problems.join("\n")
    );
}

#[test]
fn round_trip_all_fixtures() {
    let root = fixtures_root();
    if !root.is_dir() {
        eprintln!(
            "skipping fixture harness: каталог фикстур не найден ({})",
            root.display()
        );
        return;
    }
    let fixtures = discover_fixtures(&root);
    assert!(!fixtures.is_empty(), "не найдено ни одного src.psd");

    let mut ok = 0usize; // прошли round-trip структурно
    let mut exercised: Vec<String> = Vec::new(); // фикстуры, прочитанные на первом проходе
    let mut mismatches: Vec<(String, String)> = Vec::new();

    for path in &fixtures {
        let name = fixture_name(&root, path);
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let opts = default_read_options();

        // Первый проход. Если не читается — это забота read_all_fixtures.
        let first = catch_panic_msg(|| read_psd(&bytes, &opts));
        let psd1 = match first {
            Ok(Ok(p)) => p,
            _ => continue,
        };
        exercised.push(name.clone());

        // write -> read -> compare.
        let result = catch_panic_msg(|| {
            let wopts = WriteOptions::default();
            let written = write_psd(&psd1, &wopts);
            let psd2 = read_psd(&written, &opts)?;
            Ok::<Psd, ReadError>(psd2)
        });

        match result {
            Ok(Ok(psd2)) => match compare_psd(&psd1, &psd2) {
                Ok(()) => ok += 1,
                Err(div) => mismatches.push((name, format!("structure: {div}"))),
            },
            Ok(Err(e)) => mismatches.push((name, format!("re-read error: {e}"))),
            Err(panic_msg) => mismatches.push((name, panic_msg)),
        }
    }

    print_round_trip_summary(exercised.len(), ok, &mismatches);

    assert!(
        !exercised.is_empty(),
        "ни одна фикстура не прочиталась — нечего round-trip'ить"
    );

    let problems = allowlist_problems(
        "the round trip",
        &mismatches,
        EXPECTED_ROUND_TRIP_FAILURES,
        &exercised,
    );
    assert!(
        problems.is_empty(),
        "round trip no longer matches EXPECTED_ROUND_TRIP_FAILURES ({ok}/{} stable):\n{}",
        exercised.len(),
        problems.join("\n")
    );
}

/// Every layer of the Photoshop 2026 fixture decodes to the blend mode recorded in its
/// `data.json`, both for the layer itself and for each of its effects.
///
/// This is the only test that pins the Photoshop 2026 descriptor decoding: the read
/// sweep and the round trip are both blind to a systematically wrong `BlnM` decode
/// (reading still succeeds, and a wrong-but-consistent value survives write -> read).
#[test]
fn photoshop_2026_blend_modes_match_ground_truth() {
    let dir = fixtures_root().join("read").join("2026-blend-modes");
    if !dir.is_dir() {
        eprintln!(
            "skipping Photoshop 2026 blend-mode check: фикстура не найдена ({})",
            dir.display()
        );
        return;
    }

    let psd_path = dir.join("src.psd");
    let bytes = match fs::read(&psd_path) {
        Ok(b) => b,
        Err(e) => panic!("cannot read {}: {e}", psd_path.display()),
    };
    let psd = match read_psd(&bytes, &default_read_options()) {
        Ok(p) => p,
        Err(e) => panic!("cannot parse {}: {e}", psd_path.display()),
    };
    let expected = match load_ground_truth(&dir) {
        Some(Ok(v)) => v,
        Some(Err(e)) => panic!("cannot load {}/data.json: {e}", dir.display()),
        None => panic!("{}/data.json is missing", dir.display()),
    };

    let expected_children = expected
        .get("children")
        .and_then(Json::as_array)
        .unwrap_or_default();
    let empty: Vec<Layer> = Vec::new();
    let actual_children = psd.children.as_ref().unwrap_or(&empty);
    assert_eq!(
        expected_children.len(),
        actual_children.len(),
        "layer count differs from data.json"
    );
    // The fixture exists to cover one layer per blend mode; a shrunken corpus would
    // silently weaken the check.
    assert!(
        expected_children.len() >= 29,
        "expected at least 29 layers in the blend-mode fixture, data.json has {}",
        expected_children.len()
    );

    let mut diffs: Vec<String> = Vec::new();
    for (i, (exp, act)) in expected_children
        .iter()
        .zip(actual_children.iter())
        .enumerate()
    {
        // In this fixture each layer is named after its own blend mode, so the name is
        // part of the ground truth being asserted, not just a label for messages.
        let label = exp.get("name").and_then(Json::as_str).unwrap_or("<unnamed>");
        compare_str(
            &mut diffs,
            &format!("children[{i}].name"),
            exp.get("name").and_then(Json::as_str),
            act.additional_info.name.as_deref(),
        );
        compare_str(
            &mut diffs,
            &format!("children[{i}] ({label}).blendMode"),
            exp.get("blendMode").and_then(Json::as_str),
            act.blend_mode.map(blend_mode_name),
        );

        // Layer effects decode their blend mode through the descriptor enum path
        // (`effects_keys::decode_enum`), which is the one Photoshop 2026 broke.
        let expected_effects = exp
            .get("effects")
            .map(json_effect_blend_modes)
            .unwrap_or_default();
        let actual_effects = act
            .additional_info
            .effects
            .as_ref()
            .map(effect_blend_modes)
            .unwrap_or_default();
        if expected_effects.len() == actual_effects.len() {
            for (e, a) in expected_effects.iter().zip(actual_effects.iter()) {
                if e != a {
                    diffs.push(format!(
                        "children[{i}] ({label}).effects: expected {e:?}, got {a:?}"
                    ));
                }
            }
        } else {
            diffs.push(format!(
                "children[{i}] ({label}).effects: expected {expected_effects:?}, got {actual_effects:?}"
            ));
        }
    }

    assert!(
        diffs.is_empty(),
        "read/2026-blend-modes does not match its data.json ground truth:\n{}",
        diffs.join("\n")
    );
}

/// Every `read/*` fixture that ships a `data.json` matches it on the document header and
/// on the layer tree's names, bounds and blend modes.
///
/// Deliberately narrow: only fields whose meaning is identical on both sides of the
/// JS/Rust boundary are compared, so a mismatch is a real defect rather than a
/// formatting or representation difference. Fixtures that fail to read are ignored here
/// — `read_all_fixtures` owns that signal.
#[test]
fn structure_matches_ground_truth() {
    let root = fixtures_root();
    if !root.is_dir() {
        eprintln!(
            "skipping ground-truth comparison: каталог фикстур не найден ({})",
            root.display()
        );
        return;
    }

    let mut checked: Vec<String> = Vec::new();
    let mut mismatches: Vec<(String, String)> = Vec::new();

    for path in discover_fixtures(&root) {
        let name = fixture_name(&root, &path);
        // Only `read/*` ships a dump of a *read* result. `write/*/data.json` is writer
        // input, and `read-write/*` compares against a binary `expected.psd` instead.
        if !name.starts_with("read/") {
            continue;
        }
        let Some(dir) = path.parent() else { continue };
        let expected = match load_ground_truth(dir) {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                checked.push(name.clone());
                mismatches.push((name, format!("data.json: {e}")));
                continue;
            }
            None => continue,
        };
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                checked.push(name.clone());
                mismatches.push((name, format!("io: {e}")));
                continue;
            }
        };
        let opts = default_read_options();
        let psd = match catch_panic_msg(|| read_psd(&bytes, &opts)) {
            Ok(Ok(p)) => p,
            // Unreadable fixtures are read_all_fixtures' business, not this test's.
            Ok(Err(_)) | Err(_) => continue,
        };

        checked.push(name.clone());
        let diffs = compare_to_ground_truth(&psd, &expected);
        if !diffs.is_empty() {
            mismatches.push((name, diffs.join("; ")));
        }
    }

    eprintln!("\n========= GROUND-TRUTH SUMMARY =========");
    eprintln!("fixtures with data.json : {}", checked.len());
    eprintln!("mismatching             : {}", mismatches.len());
    if !mismatches.is_empty() {
        eprintln!("\n-- mismatches (fixture -> divergences) --");
        for (name, diffs) in &mismatches {
            eprintln!("  {name}\n       {diffs}");
        }
    }
    eprintln!("========================================\n");

    assert!(
        !checked.is_empty(),
        "не найдено ни одной read-фикстуры с data.json"
    );

    let problems = allowlist_problems(
        "the ground-truth comparison",
        &mismatches,
        EXPECTED_GROUND_TRUTH_FAILURES,
        &checked,
    );
    assert!(
        problems.is_empty(),
        "ground-truth comparison no longer matches EXPECTED_GROUND_TRUTH_FAILURES \
         ({}/{} fixtures match):\n{}",
        checked.len() - mismatches.len(),
        checked.len(),
        problems.join("\n")
    );
}

fn print_read_summary(total: usize, ok: usize, failures: &[(String, String)]) {
    eprintln!("\n================ READ SWEEP SUMMARY ================");
    eprintln!("total fixtures : {total}");
    eprintln!("read OK        : {ok}");
    eprintln!("read FAILED    : {}", failures.len());
    if total > 0 {
        eprintln!("success rate   : {:.1}%", ok as f64 / total as f64 * 100.0);
    }

    if !failures.is_empty() {
        let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
        for (_, msg) in failures {
            *by_kind.entry(classify(msg)).or_default() += 1;
        }
        eprintln!("\n-- failures grouped by kind --");
        for (kind, n) in &by_kind {
            eprintln!("  {n:>3}  {kind}");
        }
        eprintln!("\n-- failures (fixture -> error) --");
        for (name, msg) in failures {
            eprintln!("  {name}\n       {msg}");
        }
    }
    eprintln!("====================================================\n");
}

fn print_round_trip_summary(readable: usize, ok: usize, mismatches: &[(String, String)]) {
    eprintln!("\n============= ROUND-TRIP SUMMARY =============");
    eprintln!("readable fixtures : {readable}");
    eprintln!("round-trip OK     : {ok}");
    eprintln!("round-trip BAD    : {}", mismatches.len());
    if readable > 0 {
        eprintln!(
            "stability rate    : {:.1}%",
            ok as f64 / readable as f64 * 100.0
        );
    }

    if !mismatches.is_empty() {
        let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
        for (_, msg) in mismatches {
            *by_kind.entry(classify(msg)).or_default() += 1;
        }
        eprintln!("\n-- mismatches grouped by kind --");
        for (kind, n) in &by_kind {
            eprintln!("  {n:>3}  {kind}");
        }
        eprintln!("\n-- mismatches (fixture -> first divergence) --");
        for (name, msg) in mismatches {
            eprintln!("  {name}\n       {msg}");
        }
    }
    eprintln!("=============================================\n");
}
