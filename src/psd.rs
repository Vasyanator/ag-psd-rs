/*
File: crates/ag-psd/src/psd.rs

Purpose:
главные типы документа Psd (общая модель данных), опции чтения/записи.
Это центральная shared-модель, на которую ссылаются все остальные модули порта.

Source compatibility:
- порт upstream-файла `test/ag-psd/src/psd.ts` (разбиение 1:1).
- портированы ТОЛЬКО объявления модели данных (interface/type/enum/union).
  Функции `readPsd`/`writePsd` и любая оркестрация чтения/записи здесь НЕ
  портированы — их портирует отдельная задача в этот же файл.

Соглашения порта (становятся конвенциями проекта):
- TS optional `field?: T`  -> `field: Option<T>`.
- TS string-union (BlendMode = 'normal' | ...) -> Rust `enum`
  с `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`; точные строковые значения
  сохранены в doc-комментариях для будущего слоя (де)сериализации.
- TS numeric enum -> Rust enum с явными дискриминантами, совпадающими с TS.
- camelCase -> snake_case. Для 4-char PSD-ключей и неочевидных имён в
  doc-комментарии указано оригинальное TS-имя.
- структуры derive `Debug, Clone` (+ `Default`, где есть осмысленный default).
  Exception: `ReadOptions` implements `Default` by hand, because its default is
  not all-`None` — it carries the 2 GiB `total_memory_limit` budget that upstream
  `readPsd` installs when the option is absent.
- массивы -> `Vec<T>`; `Option<Box<T>>` только для разрыва рекурсии.

Маппинг canvas / imageData:
- В TS поля `canvas: HTMLCanvasElement` и `imageData: PixelData` (где PixelData
  оборачивает типизированный массив). Здесь и то, и другое моделируется одним
  типом `PixelData { width, height, data: Vec<u8> /* RGBA8 */ }`. Поля,
  бывшие `canvas?: HTMLCanvasElement`, становятся `canvas: Option<PixelData>`,
  чтобы сохранить раздельность полей оригинала. Крейт `image` не подключаем.
- TS `Uint8Array` / `PixelArray` -> `Vec<u8>` (для PixelArray теряем сведения о
  битности, как и просили — буфер сырых байт).

Размещение типов:
- Все типы документа определены здесь (это общая модель). Типы, которые в TS
  жили бы в других модулях, но являются частью публичной формы документа,
  тоже определены здесь. Никакие ещё-stub-модули не трогаются.
*/

// PORT STATUS: ported. This file holds the document model only; the read/write
// orchestration that upstream keeps in `psd.ts` lives in `reader.rs`/`writer.rs`.

// ===========================================================================
// Canvas / pixel data mapping
// ===========================================================================

/// Замена для TS `HTMLCanvasElement` и `PixelData`.
/// `data` — сырые пиксели RGBA8 (4 байта на пиксель), длина = width*height*4.
/// (В оригинале тип массива зависит от битности документа — здесь храним байты.)
#[derive(Debug, Clone, Default)]
pub struct PixelData {
    pub width: u32,
    pub height: u32,
    /// RGBA8, либо сырые байты канала(ов).
    pub data: Vec<u8>,
}

// ===========================================================================
// Blend mode (string union)
// ===========================================================================

/// TS `BlendMode` string-union. Строковые значения см. в doc-комментариях.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// "pass through"
    PassThrough,
    /// "normal"
    Normal,
    /// "dissolve"
    Dissolve,
    /// "darken"
    Darken,
    /// "multiply"
    Multiply,
    /// "color burn"
    ColorBurn,
    /// "linear burn"
    LinearBurn,
    /// "darker color"
    DarkerColor,
    /// "lighten"
    Lighten,
    /// "screen"
    Screen,
    /// "color dodge"
    ColorDodge,
    /// "linear dodge"
    LinearDodge,
    /// "lighter color"
    LighterColor,
    /// "overlay"
    Overlay,
    /// "soft light"
    SoftLight,
    /// "hard light"
    HardLight,
    /// "vivid light"
    VividLight,
    /// "linear light"
    LinearLight,
    /// "pin light"
    PinLight,
    /// "hard mix"
    HardMix,
    /// "difference"
    Difference,
    /// "exclusion"
    Exclusion,
    /// "subtract"
    Subtract,
    /// "divide"
    Divide,
    /// "hue"
    Hue,
    /// "saturation"
    Saturation,
    /// "color"
    Color,
    /// "luminosity"
    Luminosity,
    /// "linear height" — descriptor-only (`BlnM.linearHeight`), used in ABR brushes.
    /// The legacy layer-record signature table has no code for it.
    LinearHeight,
    /// "height" — descriptor-only (`BlnM.Hght`), used in ABR brushes.
    Height,
    /// "subtraction" — descriptor-only (`BlnM.Sbtr`), a second encoding of subtract.
    Subtraction,
}

// ===========================================================================
// Numeric enums
// ===========================================================================

/// TS `const enum ColorMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Bitmap = 0,
    Grayscale = 1,
    Indexed = 2,
    Rgb = 3,
    Cmyk = 4,
    Multichannel = 7,
    Duotone = 8,
    Lab = 9,
}

/// TS `const enum SectionDividerType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionDividerType {
    Other = 0,
    OpenFolder = 1,
    ClosedFolder = 2,
    BoundingSectionDivider = 3,
}

/// TS `enum LayerCompCapturedInfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerCompCapturedInfo {
    None = 0,
    Visibility = 1,
    Position = 2,
    Appearance = 4,
}

/// TS `const enum ChannelID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelId {
    /// red (rgb) / cyan (cmyk)
    Color0 = 0,
    /// green (rgb) / magenta (cmyk)
    Color1 = 1,
    /// blue (rgb) / yellow (cmyk)
    Color2 = 2,
    /// - (rgb) / black (cmyk)
    Color3 = 3,
    Transparency = -1,
    UserMask = -2,
    RealUserMask = -3,
}

/// TS `const enum Compression`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    RawData = 0,
    RleCompressed = 1,
    ZipWithoutPrediction = 2,
    ZipWithPrediction = 3,
}

// ===========================================================================
// Color variants
// ===========================================================================

/// TS `RGBA` — values from 0 to 255.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// TS `RGB` — values from 0 to 255.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

/// TS `FRGB` — values from 0 to 1 (can be above 1, can be negative).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Frgb {
    pub fr: f64,
    pub fg: f64,
    pub fb: f64,
}

/// TS `HSB` — values from 0 to 1.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hsb {
    pub h: f64,
    pub s: f64,
    pub b: f64,
}

/// TS `CMYK` — values from 0 to 255.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cmyk {
    pub c: f64,
    pub m: f64,
    pub y: f64,
    pub k: f64,
}

/// TS `LAB` — `l` from 0 to 1; `a` and `b` from -1 to 1.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Lab {
    pub l: f64,
    pub a: f64,
    pub b: f64,
}

/// TS `Grayscale` — values from 0 to 255.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Grayscale {
    pub k: f64,
}

/// TS `Color = RGBA | RGB | FRGB | HSB | CMYK | LAB | Grayscale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    Rgba(Rgba),
    Rgb(Rgb),
    Frgb(Frgb),
    Hsb(Hsb),
    Cmyk(Cmyk),
    Lab(Lab),
    Grayscale(Grayscale),
}

// ===========================================================================
// Units / generic small shapes
// ===========================================================================

/// TS `Units` string-union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Units {
    /// "Pixels"
    Pixels,
    /// "Points"
    Points,
    /// "Picas"
    Picas,
    /// "Millimeters"
    Millimeters,
    /// "Centimeters"
    Centimeters,
    /// "Inches"
    Inches,
    /// "None"
    None,
    /// "Density"
    Density,
}

/// TS `UnitsValue`.
#[derive(Debug, Clone, Copy)]
pub struct UnitsValue {
    pub units: Units,
    pub value: f64,
}

/// TS `UnitsBounds`.
#[derive(Debug, Clone, Copy)]
pub struct UnitsBounds {
    pub top: UnitsValue,
    pub left: UnitsValue,
    pub right: UnitsValue,
    pub bottom: UnitsValue,
}

/// Generic `{ x: number; y: number; }` point.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PointF {
    pub x: f64,
    pub y: f64,
}

/// Generic `{ x: UnitsValue; y: UnitsValue; }`.
#[derive(Debug, Clone, Copy)]
pub struct UnitsPoint {
    pub x: UnitsValue,
    pub y: UnitsValue,
}

/// Generic `{ horizontal: number; vertical: number; }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HorizontalVertical {
    pub horizontal: f64,
    pub vertical: f64,
}

/// Generic integer rect `{ top; left; bottom; right; }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bounds {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

/// Generic `{ left; top; right; bottom; }` (order as it appears in slices).
#[derive(Debug, Clone, Copy, Default)]
pub struct LtrbBounds {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// TS `Fraction`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fraction {
    pub numerator: f64,
    pub denominator: f64,
}

// ===========================================================================
// String-union helper enums
// ===========================================================================

/// TS `TextGridding = 'none' | 'round'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextGridding {
    /// "none"
    None,
    /// "round"
    Round,
}

/// TS `Orientation = 'horizontal' | 'vertical'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// "horizontal"
    Horizontal,
    /// "vertical"
    Vertical,
}

/// TS `AntiAlias`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntiAlias {
    /// "none"
    None,
    /// "sharp"
    Sharp,
    /// "crisp"
    Crisp,
    /// "strong"
    Strong,
    /// "smooth"
    Smooth,
    /// "platform"
    Platform,
    /// "platformLCD"
    PlatformLcd,
}

/// TS `WarpStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpStyle {
    /// "none"
    None,
    /// "arc"
    Arc,
    /// "arcLower"
    ArcLower,
    /// "arcUpper"
    ArcUpper,
    /// "arch"
    Arch,
    /// "bulge"
    Bulge,
    /// "shellLower"
    ShellLower,
    /// "shellUpper"
    ShellUpper,
    /// "flag"
    Flag,
    /// "wave"
    Wave,
    /// "fish"
    Fish,
    /// "rise"
    Rise,
    /// "fisheye"
    Fisheye,
    /// "inflate"
    Inflate,
    /// "squeeze"
    Squeeze,
    /// "twist"
    Twist,
    /// "custom"
    Custom,
    /// "cylinder"
    Cylinder,
}

/// TS `BevelStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BevelStyle {
    /// "outer bevel"
    OuterBevel,
    /// "inner bevel"
    InnerBevel,
    /// "emboss"
    Emboss,
    /// "pillow emboss"
    PillowEmboss,
    /// "stroke emboss"
    StrokeEmboss,
}

/// TS `BevelTechnique`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BevelTechnique {
    /// "smooth"
    Smooth,
    /// "chisel hard"
    ChiselHard,
    /// "chisel soft"
    ChiselSoft,
}

/// TS `BevelDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BevelDirection {
    /// "up"
    Up,
    /// "down"
    Down,
}

/// TS `GlowTechnique`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlowTechnique {
    /// "softer"
    Softer,
    /// "precise"
    Precise,
}

/// TS `GlowSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlowSource {
    /// "edge"
    Edge,
    /// "center"
    Center,
}

/// TS `GradientStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientStyle {
    /// "linear"
    Linear,
    /// "radial"
    Radial,
    /// "angle"
    Angle,
    /// "reflected"
    Reflected,
    /// "diamond"
    Diamond,
}

/// TS `Justification`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Justification {
    /// "left"
    Left,
    /// "right"
    Right,
    /// "center"
    Center,
    /// "justify-left"
    JustifyLeft,
    /// "justify-right"
    JustifyRight,
    /// "justify-center"
    JustifyCenter,
    /// "justify-all"
    JustifyAll,
}

/// TS `LineCapType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineCapType {
    /// "butt"
    Butt,
    /// "round"
    Round,
    /// "square"
    Square,
}

/// TS `LineJoinType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoinType {
    /// "miter"
    Miter,
    /// "round"
    Round,
    /// "bevel"
    Bevel,
}

/// TS `LineAlignment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineAlignment {
    /// "inside"
    Inside,
    /// "center"
    Center,
    /// "outside"
    Outside,
}

/// TS `InterpolationMethod`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterpolationMethod {
    /// "classic"
    Classic,
    /// "perceptual"
    Perceptual,
    /// "linear"
    Linear,
    /// "smooth"
    Smooth,
}

/// TS `RenderingIntent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderingIntent {
    /// "perceptual"
    Perceptual,
    /// "saturation"
    Saturation,
    /// "relative colorimetric"
    RelativeColorimetric,
    /// "absolute colorimetric"
    AbsoluteColorimetric,
}

/// TS `BooleanOperation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanOperation {
    /// "exclude"
    Exclude,
    /// "combine"
    Combine,
    /// "subtract"
    Subtract,
    /// "intersect"
    Intersect,
}

/// TS `LayerColor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerColor {
    /// "none"
    None,
    /// "red"
    Red,
    /// "orange"
    Orange,
    /// "yellow"
    Yellow,
    /// "green"
    Green,
    /// "blue"
    Blue,
    /// "violet"
    Violet,
    /// "gray"
    Gray,
}

/// TS `PlacedLayerType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacedLayerType {
    /// "unknown"
    Unknown,
    /// "vector"
    Vector,
    /// "raster"
    Raster,
    /// "image stack"
    ImageStack,
}

/// TS `TimelineKeyInterpolation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineKeyInterpolation {
    /// "linear"
    Linear,
    /// "hold"
    Hold,
}

/// TS `TimelineTrackType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineTrackType {
    /// "opacity"
    Opacity,
    /// "style"
    Style,
    /// "sheetTransform"
    SheetTransform,
    /// "sheetPosition"
    SheetPosition,
    /// "globalLighting"
    GlobalLighting,
}

/// TS `LayerEffectStroke.position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokePosition {
    /// "inside"
    Inside,
    /// "center"
    Center,
    /// "outside"
    Outside,
}

/// TS `LayerEffectStroke.fillType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrokeFillType {
    /// "color"
    Color,
    /// "gradient"
    Gradient,
    /// "pattern"
    Pattern,
}

/// TS `EffectNoiseGradient.colorModel` and similar `'rgb' | 'hsb' | 'lab' | 'hsl'`.
///
/// `Hsl` exists only in the descriptor enum `ClrS` (upstream v31). The `grdm`
/// adjustment reuses this type but its binary color-model table has no slot for
/// `hsl`; writing it there falls back to the rgb slot, mirroring upstream's
/// `indexOf(...) === -1 -> 3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientColorModel {
    /// "rgb"
    Rgb,
    /// "hsb"
    Hsb,
    /// "lab"
    Lab,
    /// "hsl"
    Hsl,
}

/// TS `BezierPath.fillRule = 'even-odd' | 'non-zero'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillRule {
    /// "even-odd"
    EvenOdd,
    /// "non-zero"
    NonZero,
}

// ===========================================================================
// Effects
// ===========================================================================

/// TS `EffectContour`.
#[derive(Debug, Clone, Default)]
pub struct EffectContour {
    pub name: String,
    /// curve points `{ x; y; }[]`
    pub curve: Vec<PointF>,
}

/// TS `EffectPattern` (TODO: add fields upstream).
#[derive(Debug, Clone, Default)]
pub struct EffectPattern {
    pub name: String,
    pub id: String,
}

/// TS `ColorStop`.
#[derive(Debug, Clone)]
pub struct ColorStop {
    pub color: Color,
    pub location: f64,
    pub midpoint: f64,
}

/// TS `OpacityStop`.
#[derive(Debug, Clone, Default)]
pub struct OpacityStop {
    pub opacity: f64,
    pub location: f64,
    pub midpoint: f64,
}

/// TS `EffectSolidGradient` (`type: 'solid'`).
#[derive(Debug, Clone, Default)]
pub struct EffectSolidGradient {
    pub name: String,
    pub smoothness: Option<f64>,
    pub color_stops: Vec<ColorStop>,
    pub opacity_stops: Vec<OpacityStop>,
}

/// TS `EffectNoiseGradient` (`type: 'noise'`).
#[derive(Debug, Clone, Default)]
pub struct EffectNoiseGradient {
    pub name: String,
    pub roughness: Option<f64>,
    pub color_model: Option<GradientColorModel>,
    pub random_seed: Option<f64>,
    pub restrict_colors: Option<bool>,
    pub add_transparency: Option<bool>,
    pub min: Vec<f64>,
    pub max: Vec<f64>,
}

/// TS union `EffectSolidGradient | EffectNoiseGradient`.
#[derive(Debug, Clone)]
pub enum EffectGradient {
    Solid(EffectSolidGradient),
    Noise(EffectNoiseGradient),
}

/// TS `ExtraGradientInfo` (intersected with gradient unions in several places).
#[derive(Debug, Clone, Default)]
pub struct ExtraGradientInfo {
    pub style: Option<GradientStyle>,
    pub scale: Option<f64>,
    pub angle: Option<f64>,
    pub dither: Option<bool>,
    pub interpolation_method: Option<InterpolationMethod>,
    pub reverse: Option<bool>,
    pub align: Option<bool>,
    pub offset: Option<PointF>,
}

/// TS `ExtraPatternInfo`.
#[derive(Debug, Clone, Default)]
pub struct ExtraPatternInfo {
    pub linked: Option<bool>,
    pub phase: Option<PointF>,
}

/// TS `(EffectSolidGradient | EffectNoiseGradient) & ExtraGradientInfo`.
#[derive(Debug, Clone)]
pub struct GradientWithExtra {
    pub gradient: EffectGradient,
    pub extra: ExtraGradientInfo,
}

/// TS `LayerEffectShadow` (drop & inner shadow).
#[derive(Debug, Clone, Default)]
pub struct LayerEffectShadow {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub size: Option<UnitsValue>,
    pub angle: Option<f64>,
    pub distance: Option<UnitsValue>,
    pub color: Option<Color>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f64>,
    pub use_global_light: Option<bool>,
    pub antialiased: Option<bool>,
    pub contour: Option<EffectContour>,
    /// spread
    pub choke: Option<UnitsValue>,
    /// only drop shadow
    pub layer_conceals: Option<bool>,
}

/// TS `LayerEffectsOuterGlow`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectsOuterGlow {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub size: Option<UnitsValue>,
    pub color: Option<Color>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f64>,
    pub source: Option<GlowSource>,
    pub antialiased: Option<bool>,
    pub noise: Option<f64>,
    pub range: Option<f64>,
    pub choke: Option<UnitsValue>,
    pub jitter: Option<f64>,
    pub contour: Option<EffectContour>,
}

/// TS `LayerEffectInnerGlow`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectInnerGlow {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub size: Option<UnitsValue>,
    pub color: Option<Color>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f64>,
    pub source: Option<GlowSource>,
    pub technique: Option<GlowTechnique>,
    pub antialiased: Option<bool>,
    pub noise: Option<f64>,
    pub range: Option<f64>,
    /// spread
    pub choke: Option<UnitsValue>,
    pub jitter: Option<f64>,
    pub contour: Option<EffectContour>,
}

/// TS `LayerEffectBevel`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectBevel {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub size: Option<UnitsValue>,
    pub angle: Option<f64>,
    /// depth
    pub strength: Option<f64>,
    pub highlight_blend_mode: Option<BlendMode>,
    pub shadow_blend_mode: Option<BlendMode>,
    pub highlight_color: Option<Color>,
    pub shadow_color: Option<Color>,
    pub style: Option<BevelStyle>,
    pub highlight_opacity: Option<f64>,
    pub shadow_opacity: Option<f64>,
    pub soften: Option<UnitsValue>,
    pub use_global_light: Option<bool>,
    pub altitude: Option<f64>,
    pub technique: Option<BevelTechnique>,
    pub direction: Option<BevelDirection>,
    pub use_texture: Option<bool>,
    pub use_shape: Option<bool>,
    pub antialias_gloss: Option<bool>,
    pub contour: Option<EffectContour>,
}

/// TS `LayerEffectSolidFill`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectSolidFill {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub blend_mode: Option<BlendMode>,
    pub color: Option<Color>,
    pub opacity: Option<f64>,
}

/// TS `LayerEffectStroke`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectStroke {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub overprint: Option<bool>,
    pub size: Option<UnitsValue>,
    pub position: Option<StrokePosition>,
    pub fill_type: Option<StrokeFillType>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f64>,
    pub color: Option<Color>,
    /// `(EffectSolidGradient | EffectNoiseGradient) & ExtraGradientInfo`
    pub gradient: Option<GradientWithExtra>,
    /// `EffectPattern & {}` (TODO: additional pattern info upstream)
    pub pattern: Option<EffectPattern>,
}

/// TS `LayerEffectSatin`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectSatin {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub size: Option<UnitsValue>,
    pub blend_mode: Option<BlendMode>,
    pub color: Option<Color>,
    pub antialiased: Option<bool>,
    pub opacity: Option<f64>,
    pub distance: Option<UnitsValue>,
    pub invert: Option<bool>,
    pub angle: Option<f64>,
    pub contour: Option<EffectContour>,
}

/// TS `LayerEffectPatternOverlay` (not supported yet upstream — `Patt` section).
#[derive(Debug, Clone, Default)]
pub struct LayerEffectPatternOverlay {
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f64>,
    pub scale: Option<f64>,
    pub pattern: Option<EffectPattern>,
    pub phase: Option<PointF>,
    pub align: Option<bool>,
}

/// TS `LayerEffectGradientOverlay`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectGradientOverlay {
    /// NOTE: in TS this is `string`, not `BlendMode`.
    pub blend_mode: Option<String>,
    pub present: Option<bool>,
    pub show_in_dialog: Option<bool>,
    pub enabled: Option<bool>,
    pub opacity: Option<f64>,
    pub align: Option<bool>,
    pub scale: Option<f64>,
    pub dither: Option<bool>,
    pub reverse: Option<bool>,
    /// TS field `type`
    pub gradient_type: Option<GradientStyle>,
    pub offset: Option<PointF>,
    pub gradient: Option<EffectGradient>,
    pub interpolation_method: Option<InterpolationMethod>,
    /// degrees
    pub angle: Option<f64>,
}

/// TS `LayerEffectsInfo`.
#[derive(Debug, Clone, Default)]
pub struct LayerEffectsInfo {
    pub disabled: Option<bool>,
    pub scale: Option<f64>,
    pub drop_shadow: Option<Vec<LayerEffectShadow>>,
    pub inner_shadow: Option<Vec<LayerEffectShadow>>,
    pub outer_glow: Option<LayerEffectsOuterGlow>,
    pub inner_glow: Option<LayerEffectInnerGlow>,
    pub bevel: Option<LayerEffectBevel>,
    pub solid_fill: Option<Vec<LayerEffectSolidFill>>,
    pub satin: Option<LayerEffectSatin>,
    pub stroke: Option<Vec<LayerEffectStroke>>,
    pub gradient_overlay: Option<Vec<LayerEffectGradientOverlay>>,
    /// not supported yet upstream because of `Patt` section
    pub pattern_overlay: Option<LayerEffectPatternOverlay>,
}

// ===========================================================================
// Mask data
// ===========================================================================

/// TS `LayerMaskData`.
#[derive(Debug, Clone, Default)]
pub struct LayerMaskData {
    pub top: Option<f64>,
    pub left: Option<f64>,
    pub bottom: Option<f64>,
    pub right: Option<f64>,
    pub default_color: Option<f64>,
    pub disabled: Option<bool>,
    pub position_relative_to_layer: Option<bool>,
    /// true if mask is generated from vector data, false if bitmap from user.
    pub from_vector_data: Option<bool>,
    pub user_mask_density: Option<f64>,
    /// px
    pub user_mask_feather: Option<f64>,
    pub vector_mask_density: Option<f64>,
    pub vector_mask_feather: Option<f64>,
    /// TS `canvas?: HTMLCanvasElement` -> raw pixels.
    pub canvas: Option<PixelData>,
    pub image_data: Option<PixelData>,
}

// ===========================================================================
// Warp / animations / fonts / text
// ===========================================================================

/// TS `Warp.customEnvelopeWarp`.
#[derive(Debug, Clone, Default)]
pub struct CustomEnvelopeWarp {
    pub quilt_slice_x: Option<Vec<f64>>,
    pub quilt_slice_y: Option<Vec<f64>>,
    /// 16 points top-left to bottom-right, rows first, relative to first point.
    pub mesh_points: Vec<PointF>,
}

/// TS `Warp`.
#[derive(Debug, Clone, Default)]
pub struct Warp {
    pub style: Option<WarpStyle>,
    pub value: Option<f64>,
    pub values: Option<Vec<f64>>,
    pub perspective: Option<f64>,
    pub perspective_other: Option<f64>,
    pub rotate: Option<Orientation>,
    /// for custom warps
    pub bounds: Option<UnitsBounds>,
    pub u_order: Option<f64>,
    pub v_order: Option<f64>,
    pub deform_num_rows: Option<f64>,
    pub deform_num_cols: Option<f64>,
    pub custom_envelope_warp: Option<CustomEnvelopeWarp>,
}

/// TS `Animations.frames[]` element.
#[derive(Debug, Clone, Default)]
pub struct AnimationFrameInfo {
    pub id: f64,
    pub delay: f64,
    /// 'auto' | 'none' | 'dispose'
    pub dispose: Option<AnimationDispose>,
}

/// TS `Animations.frames[].dispose` string-union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimationDispose {
    /// "auto"
    Auto,
    /// "none"
    None,
    /// "dispose"
    Dispose,
}

/// TS `Animations.animations[]` element.
#[derive(Debug, Clone, Default)]
pub struct AnimationInfo {
    pub id: f64,
    pub frames: Vec<f64>,
    pub repeats: Option<f64>,
    pub active_frame: Option<f64>,
}

/// TS `Animations`.
#[derive(Debug, Clone, Default)]
pub struct Animations {
    pub frames: Vec<AnimationFrameInfo>,
    pub animations: Vec<AnimationInfo>,
}

/// TS `Font`.
#[derive(Debug, Clone, Default)]
pub struct Font {
    pub name: String,
    pub script: Option<f64>,
    /// TS field `type`
    pub font_type: Option<f64>,
    pub synthetic: Option<f64>,
}

/// TS `ParagraphStyle`.
#[derive(Debug, Clone, Default)]
pub struct ParagraphStyle {
    pub justification: Option<Justification>,
    pub first_line_indent: Option<f64>,
    pub start_indent: Option<f64>,
    pub end_indent: Option<f64>,
    pub space_before: Option<f64>,
    pub space_after: Option<f64>,
    pub auto_hyphenate: Option<bool>,
    pub hyphenated_word_size: Option<f64>,
    pub pre_hyphen: Option<f64>,
    pub post_hyphen: Option<f64>,
    pub consecutive_hyphens: Option<f64>,
    pub zone: Option<f64>,
    pub word_spacing: Option<Vec<f64>>,
    pub letter_spacing: Option<Vec<f64>>,
    pub glyph_spacing: Option<Vec<f64>>,
    pub auto_leading: Option<f64>,
    pub leading_type: Option<f64>,
    pub hanging: Option<bool>,
    pub burasagari: Option<bool>,
    pub kinsoku_order: Option<f64>,
    pub every_line_composer: Option<bool>,
}

/// TS `ParagraphStyleRun`.
#[derive(Debug, Clone, Default)]
pub struct ParagraphStyleRun {
    pub length: f64,
    pub style: ParagraphStyle,
}

/// TS `TextStyle`.
#[derive(Debug, Clone, Default)]
pub struct TextStyle {
    pub font: Option<Font>,
    pub font_size: Option<f64>,
    pub faux_bold: Option<bool>,
    pub faux_italic: Option<bool>,
    pub auto_leading: Option<bool>,
    pub leading: Option<f64>,
    pub horizontal_scale: Option<f64>,
    pub vertical_scale: Option<f64>,
    pub tracking: Option<f64>,
    pub auto_kerning: Option<bool>,
    pub kerning: Option<f64>,
    pub baseline_shift: Option<f64>,
    /// 0 - none, 1 - small caps, 2 - all caps
    pub font_caps: Option<f64>,
    /// 0 - normal, 1 - superscript, 2 - subscript
    pub font_baseline: Option<f64>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    pub ligatures: Option<bool>,
    pub d_ligatures: Option<bool>,
    pub baseline_direction: Option<f64>,
    pub tsume: Option<f64>,
    pub style_run_alignment: Option<f64>,
    pub language: Option<f64>,
    pub no_break: Option<bool>,
    pub fill_color: Option<Color>,
    pub stroke_color: Option<Color>,
    pub fill_flag: Option<bool>,
    pub stroke_flag: Option<bool>,
    pub fill_first: Option<bool>,
    pub y_underline: Option<f64>,
    pub outline_width: Option<f64>,
    pub character_direction: Option<f64>,
    pub hindi_numbers: Option<bool>,
    pub kashida: Option<f64>,
    pub diacritic_pos: Option<f64>,
}

/// TS `TextStyleRun`.
#[derive(Debug, Clone, Default)]
pub struct TextStyleRun {
    pub length: f64,
    pub style: TextStyle,
}

/// TS `TextGridInfo`.
#[derive(Debug, Clone, Default)]
pub struct TextGridInfo {
    pub is_on: Option<bool>,
    pub show: Option<bool>,
    pub size: Option<f64>,
    pub leading: Option<f64>,
    pub color: Option<Color>,
    pub leading_fill_color: Option<Color>,
    pub align_line_height_to_grid_flags: Option<bool>,
}

/// TS `TextPath.bezierCurve`.
#[derive(Debug, Clone, Default)]
pub struct TextPathBezierCurve {
    /// 8 values per bezier curve
    pub control_points: Vec<f64>,
}

/// TS `TextPath.data.BaselineAlignment`.
#[derive(Debug, Clone, Default)]
pub struct TextPathBaselineAlignment {
    pub flag: Option<f64>,
    pub min: Option<f64>,
}

/// TS `TextPath.data.pathData`.
#[derive(Debug, Clone, Default)]
pub struct TextPathPathData {
    pub reversed: Option<bool>,
    pub spacing: Option<f64>,
}

/// TS `TextPath.data`.
#[derive(Debug, Clone, Default)]
pub struct TextPathData {
    /// TS field `type`
    pub path_type: Option<f64>,
    pub orientation: Option<f64>,
    pub frame_matrix: Vec<f64>,
    pub text_range: Vec<f64>,
    pub row_gutter: Option<f64>,
    pub column_gutter: Option<f64>,
    pub baseline_alignment: Option<TextPathBaselineAlignment>,
    pub path_data: TextPathPathData,
}

/// TS `TextPath`.
#[derive(Debug, Clone, Default)]
pub struct TextPath {
    /// TODO: this is probably not a name (upstream note)
    pub name: Option<Vec<f64>>,
    pub bezier_curve: Option<TextPathBezierCurve>,
    pub data: TextPathData,
    pub uuid: Option<String>,
}

/// TS `LayerTextData.shapeType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextShapeType {
    /// "point"
    Point,
    /// "box"
    Box,
}

/// TS `LayerTextData`.
#[derive(Debug, Clone, Default)]
pub struct LayerTextData {
    pub text: String,
    /// 2d transform matrix [xx, xy, yx, yy, tx, ty]
    pub transform: Option<Vec<f64>>,
    pub anti_alias: Option<AntiAlias>,
    pub gridding: Option<TextGridding>,
    pub orientation: Option<Orientation>,
    /// index of Editor in extra editor data related to this layer
    pub index: Option<f64>,
    pub warp: Option<Warp>,
    pub top: Option<f64>,
    pub left: Option<f64>,
    pub bottom: Option<f64>,
    pub right: Option<f64>,
    pub grid_info: Option<TextGridInfo>,
    pub use_fractional_glyph_widths: Option<bool>,
    /// base style
    pub style: Option<TextStyle>,
    /// spans of different style
    pub style_runs: Option<Vec<TextStyleRun>>,
    /// base paragraph style
    pub paragraph_style: Option<ParagraphStyle>,
    /// style for each line
    pub paragraph_style_runs: Option<Vec<ParagraphStyleRun>>,
    pub superscript_size: Option<f64>,
    pub superscript_position: Option<f64>,
    pub subscript_size: Option<f64>,
    pub subscript_position: Option<f64>,
    pub small_cap_size: Option<f64>,
    pub shape_type: Option<TextShapeType>,
    pub point_base: Option<Vec<f64>>,
    pub box_bounds: Option<Vec<f64>>,
    pub bounds: Option<UnitsBounds>,
    pub bounding_box: Option<UnitsBounds>,
    /// This is a read-only field; any changes will not be saved.
    pub text_path: Option<TextPath>,
}

// ===========================================================================
// Patterns / paths / vector content
// ===========================================================================

/// TS `PatternInfo`.
#[derive(Debug, Clone, Default)]
pub struct PatternInfo {
    pub name: String,
    pub id: String,
    pub x: f64,
    pub y: f64,
    /// `{ x; y; w; h; }`
    pub bounds: PatternBounds,
    pub data: Vec<u8>,
}

/// TS `PatternInfo.bounds` shape `{ x; y; w; h; }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PatternBounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// TS `BezierKnot`.
#[derive(Debug, Clone, Default)]
pub struct BezierKnot {
    pub linked: bool,
    /// x0, y0, x1, y1, x2, y2
    pub points: Vec<f64>,
}

/// TS `BezierPath`.
#[derive(Debug, Clone)]
pub struct BezierPath {
    pub open: bool,
    pub operation: Option<BooleanOperation>,
    pub knots: Vec<BezierKnot>,
    pub fill_rule: FillRule,
}

/// TS `VectorContent` union.
#[derive(Debug, Clone)]
pub enum VectorContent {
    /// `{ type: 'color'; color: Color; }`
    Color(Color),
    /// `EffectSolidGradient & ExtraGradientInfo`
    SolidGradient {
        gradient: EffectSolidGradient,
        extra: ExtraGradientInfo,
    },
    /// `EffectNoiseGradient & ExtraGradientInfo`
    NoiseGradient {
        gradient: EffectNoiseGradient,
        extra: ExtraGradientInfo,
    },
    /// `EffectPattern & { type: 'pattern'; } & ExtraPatternInfo`
    Pattern {
        pattern: EffectPattern,
        extra: ExtraPatternInfo,
    },
}

// ===========================================================================
// Adjustments
// ===========================================================================

/// TS `PresetInfo` (mixed into several adjustments).
#[derive(Debug, Clone, Default)]
pub struct PresetInfo {
    pub preset_kind: Option<f64>,
    pub preset_file_name: Option<String>,
}

/// TS `BrightnessAdjustment` (`type: 'brightness/contrast'`).
#[derive(Debug, Clone, Default)]
pub struct BrightnessAdjustment {
    pub brightness: Option<f64>,
    pub contrast: Option<f64>,
    pub mean_value: Option<f64>,
    pub use_legacy: Option<bool>,
    pub lab_color_only: Option<bool>,
    pub auto: Option<bool>,
}

/// TS `LevelsAdjustmentChannel`.
#[derive(Debug, Clone, Default)]
pub struct LevelsAdjustmentChannel {
    pub shadow_input: f64,
    pub highlight_input: f64,
    pub shadow_output: f64,
    pub highlight_output: f64,
    pub midtone_input: f64,
}

/// TS `LevelsAdjustment extends PresetInfo` (`type: 'levels'`).
#[derive(Debug, Clone, Default)]
pub struct LevelsAdjustment {
    pub preset: PresetInfo,
    pub rgb: Option<LevelsAdjustmentChannel>,
    pub red: Option<LevelsAdjustmentChannel>,
    pub green: Option<LevelsAdjustmentChannel>,
    pub blue: Option<LevelsAdjustmentChannel>,
}

/// TS `CurvesAdjustmentChannel = { input; output; }[]`.
pub type CurvesAdjustmentChannel = Vec<CurvesPoint>;

/// Element of `CurvesAdjustmentChannel`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CurvesPoint {
    pub input: f64,
    pub output: f64,
}

/// TS `CurvesAdjustment extends PresetInfo` (`type: 'curves'`).
#[derive(Debug, Clone, Default)]
pub struct CurvesAdjustment {
    pub preset: PresetInfo,
    pub rgb: Option<CurvesAdjustmentChannel>,
    pub red: Option<CurvesAdjustmentChannel>,
    pub green: Option<CurvesAdjustmentChannel>,
    pub blue: Option<CurvesAdjustmentChannel>,
}

/// TS `ExposureAdjustment extends PresetInfo` (`type: 'exposure'`).
#[derive(Debug, Clone, Default)]
pub struct ExposureAdjustment {
    pub preset: PresetInfo,
    pub exposure: Option<f64>,
    pub offset: Option<f64>,
    pub gamma: Option<f64>,
}

/// TS `VibranceAdjustment` (`type: 'vibrance'`).
#[derive(Debug, Clone, Default)]
pub struct VibranceAdjustment {
    pub vibrance: Option<f64>,
    pub saturation: Option<f64>,
}

/// TS `HueSaturationAdjustmentChannel`.
#[derive(Debug, Clone, Default)]
pub struct HueSaturationAdjustmentChannel {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub hue: f64,
    pub saturation: f64,
    pub lightness: f64,
}

/// TS `HueSaturationAdjustment extends PresetInfo` (`type: 'hue/saturation'`).
#[derive(Debug, Clone, Default)]
pub struct HueSaturationAdjustment {
    pub preset: PresetInfo,
    pub master: Option<HueSaturationAdjustmentChannel>,
    pub reds: Option<HueSaturationAdjustmentChannel>,
    pub yellows: Option<HueSaturationAdjustmentChannel>,
    pub greens: Option<HueSaturationAdjustmentChannel>,
    pub cyans: Option<HueSaturationAdjustmentChannel>,
    pub blues: Option<HueSaturationAdjustmentChannel>,
    pub magentas: Option<HueSaturationAdjustmentChannel>,
}

/// TS `ColorBalanceValues`.
#[derive(Debug, Clone, Default)]
pub struct ColorBalanceValues {
    pub cyan_red: f64,
    pub magenta_green: f64,
    pub yellow_blue: f64,
}

/// TS `ColorBalanceAdjustment` (`type: 'color balance'`).
#[derive(Debug, Clone, Default)]
pub struct ColorBalanceAdjustment {
    pub shadows: Option<ColorBalanceValues>,
    pub midtones: Option<ColorBalanceValues>,
    pub highlights: Option<ColorBalanceValues>,
    pub preserve_luminosity: Option<bool>,
}

/// TS `BlackAndWhiteAdjustment extends PresetInfo` (`type: 'black & white'`).
#[derive(Debug, Clone, Default)]
pub struct BlackAndWhiteAdjustment {
    pub preset: PresetInfo,
    pub reds: Option<f64>,
    pub yellows: Option<f64>,
    pub greens: Option<f64>,
    pub cyans: Option<f64>,
    pub blues: Option<f64>,
    pub magentas: Option<f64>,
    pub use_tint: Option<bool>,
    pub tint_color: Option<Color>,
}

/// TS `PhotoFilterAdjustment` (`type: 'photo filter'`).
#[derive(Debug, Clone, Default)]
pub struct PhotoFilterAdjustment {
    pub color: Option<Color>,
    pub density: Option<f64>,
    pub preserve_luminosity: Option<bool>,
}

/// TS `ChannelMixerChannel`.
#[derive(Debug, Clone, Default)]
pub struct ChannelMixerChannel {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub constant: f64,
}

/// TS `ChannelMixerAdjustment extends PresetInfo` (`type: 'channel mixer'`).
#[derive(Debug, Clone, Default)]
pub struct ChannelMixerAdjustment {
    pub preset: PresetInfo,
    pub monochrome: Option<bool>,
    pub red: Option<ChannelMixerChannel>,
    pub green: Option<ChannelMixerChannel>,
    pub blue: Option<ChannelMixerChannel>,
    pub gray: Option<ChannelMixerChannel>,
}

/// TS `ColorLookupAdjustment.lookupType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLookupType {
    /// "3dlut"
    Lut3D,
    /// "abstractProfile"
    AbstractProfile,
    /// "deviceLinkProfile"
    DeviceLinkProfile,
}

/// TS `ColorLookupAdjustment.lutFormat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LutFormat {
    /// "look"
    Look,
    /// "cube"
    Cube,
    /// "3dl"
    ThreeDl,
}

/// TS `'rgb' | 'bgr'` (data/table order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbBgrOrder {
    /// "rgb"
    Rgb,
    /// "bgr"
    Bgr,
}

/// TS `ColorLookupAdjustment` (`type: 'color lookup'`).
#[derive(Debug, Clone, Default)]
pub struct ColorLookupAdjustment {
    pub lookup_type: Option<ColorLookupType>,
    pub name: Option<String>,
    pub dither: Option<bool>,
    pub profile: Option<Vec<u8>>,
    pub lut_format: Option<LutFormat>,
    pub data_order: Option<RgbBgrOrder>,
    pub table_order: Option<RgbBgrOrder>,
    pub lut3d_file_data: Option<Vec<u8>>,
    pub lut3d_file_name: Option<String>,
}

/// TS `InvertAdjustment` (`type: 'invert'`).
#[derive(Debug, Clone, Default)]
pub struct InvertAdjustment;

/// TS `PosterizeAdjustment` (`type: 'posterize'`).
#[derive(Debug, Clone, Default)]
pub struct PosterizeAdjustment {
    pub levels: Option<f64>,
}

/// TS `ThresholdAdjustment` (`type: 'threshold'`).
#[derive(Debug, Clone, Default)]
pub struct ThresholdAdjustment {
    pub level: Option<f64>,
}

/// TS `GradientMapAdjustment.gradientType = 'solid' | 'noise'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GradientMapType {
    /// "solid"
    Solid,
    /// "noise"
    Noise,
}

/// TS `GradientMapAdjustment` (`type: 'gradient map'`).
#[derive(Debug, Clone)]
pub struct GradientMapAdjustment {
    pub name: Option<String>,
    pub gradient_type: GradientMapType,
    pub dither: Option<bool>,
    pub reverse: Option<bool>,
    pub method: Option<InterpolationMethod>,
    // solid
    pub smoothness: Option<f64>,
    pub color_stops: Option<Vec<ColorStop>>,
    pub opacity_stops: Option<Vec<OpacityStop>>,
    // noise
    pub roughness: Option<f64>,
    pub color_model: Option<GradientColorModel>,
    pub random_seed: Option<f64>,
    pub restrict_colors: Option<bool>,
    pub add_transparency: Option<bool>,
    pub min: Option<Vec<f64>>,
    pub max: Option<Vec<f64>>,
}

/// TS `SelectiveColorAdjustment.mode = 'relative' | 'absolute'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectiveColorMode {
    /// "relative"
    Relative,
    /// "absolute"
    Absolute,
}

/// TS `SelectiveColorAdjustment` (`type: 'selective color'`).
#[derive(Debug, Clone, Default)]
pub struct SelectiveColorAdjustment {
    pub mode: Option<SelectiveColorMode>,
    pub reds: Option<Cmyk>,
    pub yellows: Option<Cmyk>,
    pub greens: Option<Cmyk>,
    pub cyans: Option<Cmyk>,
    pub blues: Option<Cmyk>,
    pub magentas: Option<Cmyk>,
    pub whites: Option<Cmyk>,
    pub neutrals: Option<Cmyk>,
    pub blacks: Option<Cmyk>,
}

/// TS `AdjustmentLayer` union (the `type` tag is encoded by the variant).
#[derive(Debug, Clone)]
pub enum AdjustmentLayer {
    /// "brightness/contrast"
    Brightness(BrightnessAdjustment),
    /// "levels"
    Levels(LevelsAdjustment),
    /// "curves"
    Curves(CurvesAdjustment),
    /// "exposure"
    Exposure(ExposureAdjustment),
    /// "vibrance"
    Vibrance(VibranceAdjustment),
    /// "hue/saturation"
    HueSaturation(HueSaturationAdjustment),
    /// "color balance"
    ColorBalance(ColorBalanceAdjustment),
    /// "black & white"
    BlackAndWhite(BlackAndWhiteAdjustment),
    /// "photo filter"
    PhotoFilter(PhotoFilterAdjustment),
    /// "channel mixer"
    ChannelMixer(ChannelMixerAdjustment),
    /// "color lookup"
    ColorLookup(ColorLookupAdjustment),
    /// "invert"
    Invert(InvertAdjustment),
    /// "posterize"
    Posterize(PosterizeAdjustment),
    /// "threshold"
    Threshold(ThresholdAdjustment),
    /// "gradient map"
    GradientMap(GradientMapAdjustment),
    /// "selective color"
    SelectiveColor(SelectiveColorAdjustment),
}

// ===========================================================================
// Linked files
// ===========================================================================

/// TS `LinkedFile.descriptor.compInfo` and `PlacedLayer.compInfo`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompInfo {
    pub comp_id: f64,
    pub original_comp_id: f64,
}

/// TS `LinkedFile.descriptor`.
#[derive(Debug, Clone, Default)]
pub struct LinkedFileDescriptor {
    pub comp_info: CompInfo,
}

/// TS `LinkedFile.linkedFile` (external files).
#[derive(Debug, Clone, Default)]
pub struct ExternalLinkedFile {
    pub file_size: f64,
    pub name: String,
    pub full_path: String,
    pub original_path: String,
    pub relative_path: String,
}

/// TS `LinkedFile`.
#[derive(Debug, Clone, Default)]
pub struct LinkedFile {
    /// GUID format, e.g. 20953ddb-9391-11ec-b4f1-c15674f50bc4
    pub id: String,
    pub name: String,
    /// TS field `type`
    pub file_type: Option<String>,
    pub creator: Option<String>,
    pub data: Option<Vec<u8>>,
    /// for external files
    pub time: Option<String>,
    pub descriptor: Option<LinkedFileDescriptor>,
    pub child_document_id: Option<String>,
    pub asset_mod_time: Option<f64>,
    pub asset_locked_state: Option<f64>,
    pub linked_file: Option<ExternalLinkedFile>,
}

// ===========================================================================
// Smart filters (FilterVariant + Filter)
// ===========================================================================

/// Generic `{ radius: UnitsValue }` filter parameter set.
#[derive(Debug, Clone, Copy)]
pub struct RadiusFilter {
    pub radius: UnitsValue,
}

/// TS `FilterVariant` union. Each variant carries the TS `type` tag and its
/// `filter` payload (where present). The exact string tags are in doc-comments.
#[derive(Debug, Clone)]
pub enum FilterVariant {
    /// "average"
    Average,
    /// "blur"
    Blur,
    /// "blur more"
    BlurMore,
    /// "box blur"
    BoxBlur(RadiusFilter),
    /// "gaussian blur"
    GaussianBlur(RadiusFilter),
    /// "motion blur"
    MotionBlur {
        /// in degrees
        angle: f64,
        distance: UnitsValue,
    },
    /// "radial blur"
    RadialBlur {
        amount: f64,
        method: RadialBlurMethod,
        quality: RadialBlurQuality,
    },
    /// "shape blur"
    ShapeBlur {
        radius: UnitsValue,
        custom_shape: NamedId,
    },
    /// "smart blur"
    SmartBlur {
        radius: f64,
        threshold: f64,
        quality: LowMediumHigh,
        mode: SmartBlurMode,
    },
    /// "surface blur"
    SurfaceBlur { radius: UnitsValue, threshold: f64 },
    /// "displace"
    Displace {
        horizontal_scale: f64,
        vertical_scale: f64,
        displacement_map: DisplacementMap,
        undefined_areas: WrapOrRepeat,
        displacement_file: DisplacementFile,
    },
    /// "pinch"
    Pinch { amount: f64 },
    /// "polar coordinates"
    PolarCoordinates { conversion: PolarConversion },
    /// "ripple"
    Ripple {
        amount: f64,
        size: SmallMediumLarge,
    },
    /// "shear"
    Shear {
        shear_points: Vec<PointF>,
        shear_start: f64,
        shear_end: f64,
        undefined_areas: WrapOrRepeat,
    },
    /// "spherize"
    Spherize {
        amount: f64,
        mode: SpherizeMode,
    },
    /// "twirl"
    Twirl {
        /// degrees
        angle: f64,
    },
    /// "wave"
    Wave {
        number_of_generators: f64,
        /// TS field `type`
        wave_type: WaveType,
        wavelength: MinMax,
        amplitude: MinMax,
        scale: PointF,
        random_seed: f64,
        undefined_areas: WrapOrRepeat,
    },
    /// "zigzag"
    ZigZag {
        amount: f64,
        ridges: f64,
        style: ZigZagStyle,
    },
    /// "add noise"
    AddNoise {
        /// 0..1
        amount: f64,
        distribution: NoiseDistribution,
        monochromatic: bool,
        random_seed: f64,
    },
    /// "despeckle"
    Despeckle,
    /// "dust and scratches"
    DustAndScratches {
        /// pixels
        radius: f64,
        /// levels
        threshold: f64,
    },
    /// "median"
    Median(RadiusFilter),
    /// "reduce noise"
    ReduceNoise {
        preset: String,
        remove_jpeg_artifact: bool,
        /// 0..1
        reduce_color_noise: f64,
        /// 0..1
        sharpen_details: f64,
        channel_denoise: Vec<ChannelDenoise>,
    },
    /// "color halftone"
    ColorHalftone {
        /// pixels
        radius: f64,
        /// degrees
        angle1: f64,
        angle2: f64,
        angle3: f64,
        angle4: f64,
    },
    /// "crystallize"
    Crystallize { cell_size: f64, random_seed: f64 },
    /// "facet"
    Facet,
    /// "fragment"
    Fragment,
    /// "mezzotint"
    Mezzotint {
        /// TS field `type`
        mezzotint_type: MezzotintType,
        random_seed: f64,
    },
    /// "mosaic"
    Mosaic { cell_size: UnitsValue },
    /// "pointillize"
    Pointillize { cell_size: f64, random_seed: f64 },
    /// "clouds"
    Clouds { random_seed: f64 },
    /// "difference clouds"
    DifferenceClouds { random_seed: f64 },
    /// "fibers"
    Fibers {
        variance: f64,
        strength: f64,
        random_seed: f64,
    },
    /// "lens flare"
    LensFlare {
        /// percent
        brightness: f64,
        position: PointF,
        lens_type: LensType,
    },
    /// "sharpen"
    Sharpen,
    /// "sharpen edges"
    SharpenEdges,
    /// "sharpen more"
    SharpenMore,
    /// "smart sharpen"
    SmartSharpen {
        /// 0..1
        amount: f64,
        radius: UnitsValue,
        threshold: f64,
        /// degrees
        angle: f64,
        more_accurate: bool,
        blur: SmartSharpenBlur,
        preset: String,
        shadow: SmartSharpenTone,
        highlight: SmartSharpenTone,
    },
    /// "unsharp mask"
    UnsharpMask {
        /// 0..1
        amount: f64,
        radius: UnitsValue,
        /// levels
        threshold: f64,
    },
    /// "diffuse"
    Diffuse {
        mode: DiffuseMode,
        random_seed: f64,
    },
    /// "emboss"
    Emboss {
        /// degrees
        angle: f64,
        /// pixels
        height: f64,
        /// percent
        amount: f64,
    },
    /// "extrude"
    Extrude {
        /// TS field `type`
        extrude_type: ExtrudeType,
        /// pixels
        size: f64,
        depth: f64,
        depth_mode: ExtrudeDepthMode,
        random_seed: f64,
        solid_front_faces: bool,
        mask_incomplete_blocks: bool,
    },
    /// "find edges"
    FindEdges,
    /// "solarize"
    Solarize,
    /// "tiles"
    Tiles {
        number_of_tiles: f64,
        /// percent
        maximum_offset: f64,
        fill_empty_area_with: TilesFill,
        random_seed: f64,
    },
    /// "trace contour"
    TraceContour { level: f64, edge: LowerUpper },
    /// "wind"
    Wind {
        method: WindMethod,
        direction: LeftRight,
    },
    /// "de-interlace"
    DeInterlace {
        eliminate: DeInterlaceEliminate,
        new_fields_by: DeInterlaceNewFields,
    },
    /// "ntsc colors"
    NtscColors,
    /// "custom"
    Custom {
        scale: f64,
        offset: f64,
        matrix: Vec<f64>,
    },
    /// "high pass"
    HighPass(RadiusFilter),
    /// "maximum"
    Maximum(RadiusFilter),
    /// "minimum"
    Minimum(RadiusFilter),
    /// "offset"
    Offset {
        /// pixels
        horizontal: f64,
        /// pixels
        vertical: f64,
        undefined_areas: OffsetUndefinedAreas,
    },
    /// "puppet"
    Puppet {
        rigid_type: bool,
        bounds: Vec<PointF>,
        puppet_shape_list: Vec<PuppetShape>,
    },
    /// "oil paint plugin"
    OilPaintPlugin {
        name: String,
        gpu: bool,
        lighting: bool,
        parameters: Vec<NamedValue>,
    },
    /// "hsb/hsl"
    HsbHsl {
        input_mode: RgbHsbHsl,
        row_order: RgbHsbHsl,
    },
    /// "oil paint"
    OilPaint {
        lighting_on: bool,
        stylization: f64,
        cleanliness: f64,
        brush_scale: f64,
        micro_brush: f64,
        /// degrees
        light_direction: f64,
        specularity: f64,
    },
    /// "liquify"
    Liquify { liquify_mesh: Vec<u8> },
    /// "perspective warp"
    PerspectiveWarp {
        /// quad indices
        quads: Vec<Vec<f64>>,
        vertices: Vec<UnitsPoint>,
        warped_vertices: Vec<UnitsPoint>,
    },
    /// "curves"
    Curves {
        preset_kind: CurvesPresetKind,
        adjustments: Option<Vec<CurvesFilterAdjustment>>,
    },
    /// "invert"
    Invert,
    /// "brightness/contrast"
    BrightnessContrast {
        brightness: f64,
        contrast: f64,
        use_legacy: bool,
    },
}

/// `{ name: string; id: string }` (filter custom shape).
#[derive(Debug, Clone, Default)]
pub struct NamedId {
    pub name: String,
    pub id: String,
}

/// `{ name: string; value: number }` (oil paint plugin parameter).
#[derive(Debug, Clone, Default)]
pub struct NamedValue {
    pub name: String,
    pub value: f64,
}

/// `{ min: number; max: number }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct MinMax {
    pub min: f64,
    pub max: f64,
}

/// "spin" | "zoom"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadialBlurMethod {
    /// "spin"
    Spin,
    /// "zoom"
    Zoom,
}

/// "draft" | "good" | "best"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadialBlurQuality {
    /// "draft"
    Draft,
    /// "good"
    Good,
    /// "best"
    Best,
}

/// "low" | "medium" | "high"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowMediumHigh {
    /// "low"
    Low,
    /// "medium"
    Medium,
    /// "high"
    High,
}

/// smart blur mode: "normal" | "edge only" | "overlay edge"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartBlurMode {
    /// "normal"
    Normal,
    /// "edge only"
    EdgeOnly,
    /// "overlay edge"
    OverlayEdge,
}

/// displace displacementMap: "stretch to fit" | "tile"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplacementMap {
    /// "stretch to fit"
    StretchToFit,
    /// "tile"
    Tile,
}

/// "wrap around" | "repeat edge pixels"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapOrRepeat {
    /// "wrap around"
    WrapAround,
    /// "repeat edge pixels"
    RepeatEdgePixels,
}

/// displace displacementFile `{ signature; path; }`.
#[derive(Debug, Clone, Default)]
pub struct DisplacementFile {
    pub signature: String,
    pub path: String,
}

/// "rectangular to polar" | "polar to rectangular"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolarConversion {
    /// "rectangular to polar"
    RectangularToPolar,
    /// "polar to rectangular"
    PolarToRectangular,
}

/// "small" | "medium" | "large"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmallMediumLarge {
    /// "small"
    Small,
    /// "medium"
    Medium,
    /// "large"
    Large,
}

/// spherize mode: "normal" | "horizontal only" | "vertical only"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpherizeMode {
    /// "normal"
    Normal,
    /// "horizontal only"
    HorizontalOnly,
    /// "vertical only"
    VerticalOnly,
}

/// wave type: "sine" | "triangle" | "square"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveType {
    /// "sine"
    Sine,
    /// "triangle"
    Triangle,
    /// "square"
    Square,
}

/// zigzag style: "around center" | "out from center" | "pond ripples"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZigZagStyle {
    /// "around center"
    AroundCenter,
    /// "out from center"
    OutFromCenter,
    /// "pond ripples"
    PondRipples,
}

/// "uniform" | "gaussian"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseDistribution {
    /// "uniform"
    Uniform,
    /// "gaussian"
    Gaussian,
}

/// reduce noise channelDenoise channel: "red" | "green" | "blue" | "composite"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenoiseChannel {
    /// "red"
    Red,
    /// "green"
    Green,
    /// "blue"
    Blue,
    /// "composite"
    Composite,
}

/// reduce noise `channelDenoise[]` element.
#[derive(Debug, Clone, Default)]
pub struct ChannelDenoise {
    pub channels: Vec<DenoiseChannel>,
    pub amount: f64,
    /// percent
    pub preserve_details: Option<f64>,
}

/// mezzotint type union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MezzotintType {
    /// "fine dots"
    FineDots,
    /// "medium dots"
    MediumDots,
    /// "grainy dots"
    GrainyDots,
    /// "coarse dots"
    CoarseDots,
    /// "short lines"
    ShortLines,
    /// "medium lines"
    MediumLines,
    /// "long lines"
    LongLines,
    /// "short strokes"
    ShortStrokes,
    /// "medium strokes"
    MediumStrokes,
    /// "long strokes"
    LongStrokes,
}

/// lens flare lensType.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LensType {
    /// "50-300mm zoom"
    Zoom50To300,
    /// "32mm prime"
    Prime32,
    /// "105mm prime"
    Prime105,
    /// "movie prime"
    MoviePrime,
}

/// smart sharpen blur: "gaussian blur" | "lens blur" | "motion blur"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartSharpenBlur {
    /// "gaussian blur"
    GaussianBlur,
    /// "lens blur"
    LensBlur,
    /// "motion blur"
    MotionBlur,
}

/// smart sharpen shadow/highlight tone.
#[derive(Debug, Clone, Copy, Default)]
pub struct SmartSharpenTone {
    /// 0..1
    pub fade_amount: f64,
    /// 0..1
    pub tonal_width: f64,
    /// px
    pub radius: f64,
}

/// diffuse mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffuseMode {
    /// "normal"
    Normal,
    /// "darken only"
    DarkenOnly,
    /// "lighten only"
    LightenOnly,
    /// "anisotropic"
    Anisotropic,
}

/// extrude type: "blocks" | "pyramids"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtrudeType {
    /// "blocks"
    Blocks,
    /// "pyramids"
    Pyramids,
}

/// extrude depthMode: "random" | "level-based"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtrudeDepthMode {
    /// "random"
    Random,
    /// "level-based"
    LevelBased,
}

/// tiles fill: "background color" | "foreground color" | "inverse image" | "unaltered image"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilesFill {
    /// "background color"
    BackgroundColor,
    /// "foreground color"
    ForegroundColor,
    /// "inverse image"
    InverseImage,
    /// "unaltered image"
    UnalteredImage,
}

/// trace contour edge: "lower" | "upper"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowerUpper {
    /// "lower"
    Lower,
    /// "upper"
    Upper,
}

/// wind method: "wind" | "blast" | "stagger"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindMethod {
    /// "wind"
    Wind,
    /// "blast"
    Blast,
    /// "stagger"
    Stagger,
}

/// "left" | "right"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftRight {
    /// "left"
    Left,
    /// "right"
    Right,
}

/// de-interlace eliminate: "odd lines" | "even lines"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeInterlaceEliminate {
    /// "odd lines"
    OddLines,
    /// "even lines"
    EvenLines,
}

/// de-interlace newFieldsBy: "duplication" | "interpolation"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeInterlaceNewFields {
    /// "duplication"
    Duplication,
    /// "interpolation"
    Interpolation,
}

/// offset undefinedAreas: "set to transparent" | "repeat edge pixels" | "wrap around"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetUndefinedAreas {
    /// "set to transparent"
    SetToTransparent,
    /// "repeat edge pixels"
    RepeatEdgePixels,
    /// "wrap around"
    WrapAround,
}

/// "rgb" | "hsb" | "hsl"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbHsbHsl {
    /// "rgb"
    Rgb,
    /// "hsb"
    Hsb,
    /// "hsl"
    Hsl,
}

/// curves filter presetKind: "custom" | "default"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurvesPresetKind {
    /// "custom"
    Custom,
    /// "default"
    Default,
}

/// curves filter channel: "composite" | "red" | "green" | "blue"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurvesFilterChannel {
    /// "composite"
    Composite,
    /// "red"
    Red,
    /// "green"
    Green,
    /// "blue"
    Blue,
}

/// curves filter point `{ x; y; curved?; }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CurvesFilterPoint {
    pub x: f64,
    pub y: f64,
    pub curved: Option<bool>,
}

/// curves filter adjustment (union of curve-points vs raw values forms).
#[derive(Debug, Clone)]
pub enum CurvesFilterAdjustment {
    Curve {
        channels: Vec<CurvesFilterChannel>,
        curve: Vec<CurvesFilterPoint>,
    },
    Values {
        channels: Vec<CurvesFilterChannel>,
        values: Vec<f64>,
    },
}

/// puppet shape mesh boundary point.
#[derive(Debug, Clone, Copy)]
pub struct PuppetMeshPoint {
    pub anchor: UnitsPoint,
    pub forward: UnitsPoint,
    pub backward: UnitsPoint,
    pub smooth: bool,
}

/// puppet mesh boundary path.
#[derive(Debug, Clone, Default)]
pub struct PuppetMeshPath {
    pub closed: bool,
    pub points: Vec<PuppetMeshPoint>,
}

/// puppet mesh boundary path component.
#[derive(Debug, Clone, Default)]
pub struct PuppetPathComponent {
    pub shape_operation: String,
    pub paths: Vec<PuppetMeshPath>,
}

/// puppet shape entry.
#[derive(Debug, Clone, Default)]
pub struct PuppetShape {
    pub rigid_type: bool,
    pub original_vertex_array: Vec<PointF>,
    pub deformed_vertex_array: Vec<PointF>,
    pub index_array: Vec<f64>,
    pub pin_offsets: Vec<PointF>,
    pub pos_final_pins: Vec<PointF>,
    pub pin_vertex_indices: Vec<f64>,
    pub selected_pin: Vec<f64>,
    pub pin_position: Vec<PointF>,
    /// in degrees
    pub pin_rotation: Vec<f64>,
    pub pin_overlay: Vec<bool>,
    pub pin_depth: Vec<f64>,
    pub mesh_quality: f64,
    pub mesh_expansion: f64,
    pub mesh_rigidity: f64,
    pub image_resolution: f64,
    /// `{ pathComponents: [...] }`
    pub mesh_boundary_path: Vec<PuppetPathComponent>,
}

/// TS `Filter = FilterVariant & { ...common fields... }`.
#[derive(Debug, Clone)]
pub struct Filter {
    pub variant: FilterVariant,
    pub name: String,
    pub opacity: f64,
    pub blend_mode: BlendMode,
    pub enabled: bool,
    pub has_options: bool,
    pub foreground_color: Color,
    pub background_color: Color,
}

/// TS `PlacedLayerFilter`.
#[derive(Debug, Clone, Default)]
pub struct PlacedLayerFilter {
    pub enabled: bool,
    pub valid_at_position: bool,
    pub mask_enabled: bool,
    pub mask_linked: bool,
    pub mask_extend_with_white: bool,
    pub list: Vec<Filter>,
}

/// TS `PlacedLayer.frameStep` / `duration` `{ numerator; denominator; }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct NumDenom {
    pub numerator: f64,
    pub denominator: f64,
}

/// TS `PlacedLayer`.
#[derive(Debug, Clone, Default)]
pub struct PlacedLayer {
    /// id of linked image file (psd.linkedFiles), GUID format
    pub id: String,
    /// unique id
    pub placed: Option<String>,
    /// TS field `type`
    pub layer_type: Option<PlacedLayerType>,
    pub page_number: Option<f64>,
    pub total_pages: Option<f64>,
    pub frame_step: Option<NumDenom>,
    pub duration: Option<NumDenom>,
    pub frame_count: Option<f64>,
    /// x, y of 4 corners of the transform
    pub transform: Vec<f64>,
    /// x, y of 4 corners of the transform
    pub non_affine_transform: Option<Vec<f64>>,
    /// width of the linked image
    pub width: Option<f64>,
    /// height of the linked image
    pub height: Option<f64>,
    pub resolution: Option<UnitsValue>,
    /// warp coordinates are relative to the linked image size
    pub warp: Option<Warp>,
    pub crop: Option<f64>,
    pub comp: Option<f64>,
    pub comp_info: Option<CompInfo>,
    pub filter: Option<PlacedLayerFilter>,
}

// ===========================================================================
// Vector origination / vector mask / timeline / animation
// ===========================================================================

/// TS `KeyDescriptorItem.keyOriginRRectRadii`.
#[derive(Debug, Clone, Copy)]
pub struct RRectRadii {
    pub top_right: UnitsValue,
    pub top_left: UnitsValue,
    pub bottom_left: UnitsValue,
    pub bottom_right: UnitsValue,
}

/// TS `KeyDescriptorItem`.
#[derive(Debug, Clone, Default)]
pub struct KeyDescriptorItem {
    pub key_shape_invalidated: Option<bool>,
    pub key_origin_type: Option<f64>,
    pub key_origin_resolution: Option<f64>,
    pub key_origin_r_rect_radii: Option<RRectRadii>,
    pub key_origin_shape_bounding_box: Option<UnitsBounds>,
    pub key_origin_box_corners: Option<Vec<PointF>>,
    /// 2d transform matrix [xx, xy, yx, yy, tx, ty]
    pub transform: Option<Vec<f64>>,
}

/// TS `LayerVectorMask.clipboard`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VectorMaskClipboard {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub resolution: f64,
}

/// TS `LayerVectorMask`.
#[derive(Debug, Clone, Default)]
pub struct LayerVectorMask {
    pub invert: Option<bool>,
    pub not_link: Option<bool>,
    pub disable: Option<bool>,
    pub fill_starts_with_all_pixels: Option<bool>,
    pub clipboard: Option<VectorMaskClipboard>,
    pub paths: Vec<BezierPath>,
}

/// TS `AnimationFrame`.
#[derive(Debug, Clone, Default)]
pub struct AnimationFrame {
    /// IDs of frames that this modifier applies to
    pub frames: Vec<f64>,
    pub enable: Option<bool>,
    pub offset: Option<PointF>,
    pub reference_point: Option<PointF>,
    pub opacity: Option<f64>,
    pub effects: Option<LayerEffectsInfo>,
}

/// TS `TimelineKey` payload (the `type`-tagged second half of the union).
// `Style` embeds a whole `LayerEffectsInfo` (~1 KiB), so the enum is much larger
// than its other variants. Boxing that field is deliberately NOT done: this enum is
// part of the crate's published API, and `Box<Option<LayerEffectsInfo>>` would change
// the shape every downstream construction/match site sees. Timeline keys are held one
// per keyframe (never in bulk buffers), so the padding is not a measurable cost.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum TimelineKeyData {
    /// "opacity"
    Opacity { value: f64 },
    /// "position"
    Position { x: f64, y: f64 },
    /// "transform"
    Transform {
        scale: PointF,
        skew: PointF,
        rotation: f64,
        translation: PointF,
    },
    /// "style"
    Style { style: Option<LayerEffectsInfo> },
    /// "globalLighting"
    GlobalLighting {
        global_angle: f64,
        global_altitude: f64,
    },
}

/// TS `TimelineKey` (common fields intersected with the tagged union).
#[derive(Debug, Clone)]
pub struct TimelineKey {
    pub interpolation: TimelineKeyInterpolation,
    pub time: Fraction,
    pub selected: Option<bool>,
    pub data: TimelineKeyData,
}

/// TS `TimelineTrack.effectParams`.
#[derive(Debug, Clone, Default)]
pub struct TimelineEffectParams {
    pub keys: Vec<TimelineKey>,
    pub fill_canvas: bool,
    pub zoom_origin: f64,
}

/// TS `TimelineTrack`.
#[derive(Debug, Clone)]
pub struct TimelineTrack {
    /// TS field `type`
    pub track_type: TimelineTrackType,
    pub enabled: Option<bool>,
    pub effect_params: Option<TimelineEffectParams>,
    pub keys: Vec<TimelineKey>,
}

/// TS `Timeline`.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    pub start: Fraction,
    pub duration: Fraction,
    pub in_time: Fraction,
    pub out_time: Fraction,
    pub auto_scope: bool,
    pub audio_level: f64,
    pub tracks: Option<Vec<TimelineTrack>>,
}

// ===========================================================================
// LayerAdditionalInfo sub-shapes
// ===========================================================================

/// TS `LayerAdditionalInfo.protected`.
#[derive(Debug, Clone, Default)]
pub struct ProtectedInfo {
    pub transparency: Option<bool>,
    pub composite: Option<bool>,
    pub position: Option<bool>,
    pub artboards: Option<bool>,
}

/// TS `LayerAdditionalInfo.sectionDivider`.
#[derive(Debug, Clone)]
pub struct SectionDivider {
    /// TS field `type`
    pub divider_type: SectionDividerType,
    pub key: Option<String>,
    /// 0 = normal, 1 = scene group, affects animation timeline.
    pub sub_type: Option<f64>,
}

/// TS `LayerAdditionalInfo.filterMask` and `.userMask`.
#[derive(Debug, Clone)]
pub struct ColorSpaceMask {
    pub color_space: Color,
    pub opacity: f64,
}

/// TS `LayerAdditionalInfo.vectorStroke`.
#[derive(Debug, Clone, Default)]
pub struct VectorStroke {
    pub stroke_enabled: Option<bool>,
    pub fill_enabled: Option<bool>,
    pub line_width: Option<UnitsValue>,
    pub line_dash_offset: Option<UnitsValue>,
    pub miter_limit: Option<f64>,
    pub line_cap_type: Option<LineCapType>,
    pub line_join_type: Option<LineJoinType>,
    pub line_alignment: Option<LineAlignment>,
    pub scale_lock: Option<bool>,
    pub stroke_adjust: Option<bool>,
    pub line_dash_set: Option<Vec<UnitsValue>>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f64>,
    pub content: Option<VectorContent>,
    pub resolution: Option<f64>,
}

/// TS `LayerAdditionalInfo.vectorOrigination`.
#[derive(Debug, Clone, Default)]
pub struct VectorOrigination {
    pub key_descriptor_list: Vec<KeyDescriptorItem>,
}

/// TS version triple `{ major; minor; fix; }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VersionTriple {
    pub major: f64,
    pub minor: f64,
    pub fix: f64,
}

/// TS `LayerAdditionalInfo.compositorUsed`.
#[derive(Debug, Clone, Default)]
pub struct CompositorUsed {
    pub version: Option<VersionTriple>,
    pub photoshop_version: Option<VersionTriple>,
    pub description: String,
    pub reason: String,
    pub engine: String,
    pub enable_comp_core: Option<String>,
    pub enable_comp_core_gpu: Option<String>,
    pub enable_comp_core_threads: Option<String>,
    pub comp_core_support: Option<String>,
    pub comp_core_gpu_support: Option<String>,
}

/// TS `LayerAdditionalInfo.artboard`.
#[derive(Debug, Clone, Default)]
pub struct LayerArtboard {
    pub rect: Bounds,
    /// TS `any[]` — kept as opaque count of entries is not modeled; raw f64s.
    pub guide_indices: Option<Vec<f64>>,
    pub preset_name: Option<String>,
    pub color: Option<Color>,
    pub background_type: Option<f64>,
}

/// TS `LayerAdditionalInfo.animationFrameFlags`.
#[derive(Debug, Clone, Default)]
pub struct AnimationFrameFlags {
    pub propagate_frame_one: Option<bool>,
    pub unify_layer_position: Option<bool>,
    pub unify_layer_style: Option<bool>,
    pub unify_layer_visibility: Option<bool>,
}

/// TS `filterEffectsMasks[].channels[]` element (may be `undefined` in TS).
#[derive(Debug, Clone, Default)]
pub struct FilterEffectsChannel {
    pub compression_mode: f64,
    pub data: Vec<u8>,
}

/// TS `filterEffectsMasks[].extra`.
#[derive(Debug, Clone, Default)]
pub struct FilterEffectsExtra {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub compression_mode: f64,
    pub data: Vec<u8>,
}

/// TS `LayerAdditionalInfo.filterEffectsMasks[]` element.
#[derive(Debug, Clone, Default)]
pub struct FilterEffectsMask {
    pub id: String,
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    pub depth: f64,
    /// `(channel | undefined)[]`
    pub channels: Vec<Option<FilterEffectsChannel>>,
    pub extra: Option<FilterEffectsExtra>,
}

/// TS `comps.settings[]` element.
#[derive(Debug, Clone, Default)]
pub struct LayerCompSettings {
    pub enabled: Option<bool>,
    pub comp_list: Vec<f64>,
    pub offset: Option<PointF>,
    pub effects_reference_point: Option<PointF>,
}

/// TS `LayerAdditionalInfo.comps`.
#[derive(Debug, Clone, Default)]
pub struct LayerComps {
    pub original_effects_reference_point: Option<PointF>,
    pub settings: Vec<LayerCompSettings>,
}

/// TS `blendingRanges.ranges[]` element.
#[derive(Debug, Clone, Default)]
pub struct BlendingRange {
    pub source_range: Vec<f64>,
    pub dest_range: Vec<f64>,
}

/// TS `LayerAdditionalInfo.blendingRanges`.
#[derive(Debug, Clone, Default)]
pub struct BlendingRanges {
    pub composite_gray_blend_source: Vec<f64>,
    pub composite_graph_blend_destination_range: Vec<f64>,
    pub ranges: Vec<BlendingRange>,
}

/// TS `pixelSource.interpretation`.
#[derive(Debug, Clone, Default)]
pub struct PixelSourceInterpretation {
    /// 'straight' | ...
    pub interpret_alpha: String,
    pub profile: Vec<u8>,
}

/// TS `pixelSource.frameReader.link`.
#[derive(Debug, Clone, Default)]
pub struct PixelSourceFrameReaderLink {
    pub name: String,
    pub full_path: String,
    pub original_path: String,
    pub relative_path: String,
    pub alias: String,
}

/// TS `pixelSource.frameReader`.
#[derive(Debug, Clone, Default)]
pub struct PixelSourceFrameReader {
    /// TS field `type` = 'QTFR'
    pub reader_type: String,
    pub link: PixelSourceFrameReaderLink,
    pub media_descriptor: String,
}

/// TS `LayerAdditionalInfo.pixelSource`.
#[derive(Debug, Clone, Default)]
pub struct PixelSource {
    /// TS field `type` = 'vdPS'
    pub source_type: String,
    pub origin: PointF,
    pub interpretation: PixelSourceInterpretation,
    pub frame_reader: PixelSourceFrameReader,
    pub show_altered_video: bool,
}

/// TS `LayerAdditionalInfo`.
#[derive(Debug, Clone, Default)]
pub struct LayerAdditionalInfo {
    /// layer name
    pub name: Option<String>,
    /// layer name source
    pub name_source: Option<String>,
    /// layer id
    pub id: Option<f64>,
    /// layer version
    pub version: Option<f64>,
    pub mask: Option<LayerMaskData>,
    pub real_mask: Option<LayerMaskData>,
    /// must be `true` when using `color burn` blend mode.
    pub blend_clippend_elements: Option<bool>,
    pub blend_interior_elements: Option<bool>,
    pub knockout: Option<bool>,
    pub layer_mask_as_global_mask: Option<bool>,
    /// TS field `protected`
    pub protected_info: Option<ProtectedInfo>,
    pub layer_color: Option<LayerColor>,
    pub reference_point: Option<PointF>,
    pub section_divider: Option<SectionDivider>,
    pub filter_mask: Option<ColorSpaceMask>,
    pub effects: Option<LayerEffectsInfo>,
    pub text: Option<LayerTextData>,
    /// not supported yet upstream
    pub patterns: Option<Vec<PatternInfo>>,
    pub vector_fill: Option<VectorContent>,
    pub vector_stroke: Option<VectorStroke>,
    pub vector_mask: Option<LayerVectorMask>,
    pub using_aligned_rendering: Option<bool>,
    /// seconds
    pub timestamp: Option<f64>,
    /// TS `pathList?: {}[]` — opaque entries; count preserved as empty structs.
    pub path_list: Option<Vec<PathListItem>>,
    pub adjustment: Option<AdjustmentLayer>,
    pub placed_layer: Option<PlacedLayer>,
    pub vector_origination: Option<VectorOrigination>,
    pub compositor_used: Option<CompositorUsed>,
    pub artboard: Option<LayerArtboard>,
    pub fill_opacity: Option<f64>,
    pub transparency_shapes_layer: Option<bool>,
    pub channel_blending_restrictions: Option<Vec<f64>>,
    pub animation_frames: Option<Vec<AnimationFrame>>,
    pub animation_frame_flags: Option<AnimationFrameFlags>,
    pub timeline: Option<Timeline>,
    pub filter_effects_masks: Option<Vec<FilterEffectsMask>>,
    pub comps: Option<LayerComps>,
    pub user_mask: Option<ColorSpaceMask>,
    pub blending_ranges: Option<BlendingRanges>,
    /// ??? (upstream comment)
    pub vowv: Option<f64>,
    pub pixel_source: Option<PixelSource>,
    /// Base64 encoded raw EngineData, kept in original state.
    pub engine_data: Option<String>,
}

/// TS `pathList[]` element (`{}` with TODO upstream).
#[derive(Debug, Clone, Default)]
pub struct PathListItem;

// ===========================================================================
// Image resources
// ===========================================================================

/// TS `ImageResources.versionInfo`.
#[derive(Debug, Clone, Default)]
pub struct VersionInfo {
    pub has_real_merged_data: bool,
    pub writer_name: String,
    pub reader_name: String,
    pub file_version: f64,
}

/// TS `ImageResources.urlsList[]` element.
#[derive(Debug, Clone, Default)]
pub struct UrlListItem {
    pub id: f64,
    /// 'slice'
    pub r#ref: String,
    pub url: String,
}

/// TS `gridAndGuidesInformation.grid`.
#[derive(Debug, Clone, Copy, Default)]
pub struct GridInfo {
    pub horizontal: f64,
    pub vertical: f64,
}

/// guide direction: "horizontal" | "vertical"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideDirection {
    /// "horizontal"
    Horizontal,
    /// "vertical"
    Vertical,
}

/// TS `gridAndGuidesInformation.guides[]` element.
#[derive(Debug, Clone, Copy)]
pub struct GuideInfo {
    pub location: f64,
    pub direction: GuideDirection,
}

/// TS `ImageResources.gridAndGuidesInformation`.
#[derive(Debug, Clone, Default)]
pub struct GridAndGuidesInformation {
    pub grid: Option<GridInfo>,
    pub guides: Option<Vec<GuideInfo>>,
}

/// "PPI" | "PPCM"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionUnit {
    /// "PPI"
    Ppi,
    /// "PPCM"
    Ppcm,
}

/// width/height unit: "Inches" | "Centimeters" | "Points" | "Picas" | "Columns"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DimensionUnit {
    /// "Inches"
    Inches,
    /// "Centimeters"
    Centimeters,
    /// "Points"
    Points,
    /// "Picas"
    Picas,
    /// "Columns"
    Columns,
}

/// TS `ImageResources.resolutionInfo`.
#[derive(Debug, Clone, Copy)]
pub struct ResolutionInfo {
    pub horizontal_resolution: f64,
    pub horizontal_resolution_unit: ResolutionUnit,
    pub width_unit: DimensionUnit,
    pub vertical_resolution: f64,
    pub vertical_resolution_unit: ResolutionUnit,
    pub height_unit: DimensionUnit,
}

/// TS `ImageResources.thumbnailRaw`.
#[derive(Debug, Clone, Default)]
pub struct ThumbnailRaw {
    pub width: f64,
    pub height: f64,
    pub data: Vec<u8>,
}

/// print scale style: "centered" | "size to fit" | "user defined"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintScaleStyle {
    /// "centered"
    Centered,
    /// "size to fit"
    SizeToFit,
    /// "user defined"
    UserDefined,
}

/// TS `ImageResources.printScale`.
#[derive(Debug, Clone, Default)]
pub struct PrintScale {
    pub style: Option<PrintScaleStyle>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub scale: Option<f64>,
}

/// TS `printInformation.proofSetup` union.
#[derive(Debug, Clone)]
pub enum ProofSetup {
    /// `{ builtin: string; }`
    Builtin { builtin: String },
    /// `{ profile; renderingIntent?; blackPointCompensation?; paperWhite?; }`
    Profile {
        profile: String,
        rendering_intent: Option<RenderingIntent>,
        black_point_compensation: Option<bool>,
        paper_white: Option<bool>,
    },
}

/// TS `ImageResources.printInformation`.
#[derive(Debug, Clone, Default)]
pub struct PrintInformation {
    pub printer_manages_colors: Option<bool>,
    pub printer_name: Option<String>,
    pub printer_profile: Option<String>,
    pub print_sixteen_bit: Option<bool>,
    pub rendering_intent: Option<RenderingIntent>,
    pub hard_proof: Option<bool>,
    pub black_point_compensation: Option<bool>,
    pub proof_setup: Option<ProofSetup>,
}

/// TS `ImageResources.printFlags`.
#[derive(Debug, Clone, Default)]
pub struct PrintFlags {
    pub labels: Option<bool>,
    pub crop_marks: Option<bool>,
    pub color_bars: Option<bool>,
    pub registration_marks: Option<bool>,
    pub negative: Option<bool>,
    pub flip: Option<bool>,
    pub interpolate: Option<bool>,
    pub caption: Option<bool>,
    /// nested field also named `printFlags`
    pub print_flags: Option<bool>,
}

/// TS `ImageResources.onionSkins`.
#[derive(Debug, Clone)]
pub struct OnionSkins {
    pub enabled: bool,
    pub frames_before: f64,
    pub frames_after: f64,
    pub frame_spacing: f64,
    pub min_opacity: f64,
    pub max_opacity: f64,
    pub blend_mode: BlendMode,
}

/// TS `timelineInformation.audioClipGroups[].audioClips[].frameReader.link`.
#[derive(Debug, Clone, Default)]
pub struct AudioClipFrameReaderLink {
    pub name: String,
    pub full_path: String,
    pub relative_path: String,
}

/// TS `timelineInformation.audioClipGroups[].audioClips[].frameReader`.
#[derive(Debug, Clone, Default)]
pub struct AudioClipFrameReader {
    /// TS field `type`
    pub reader_type: f64,
    pub media_descriptor: String,
    pub link: AudioClipFrameReaderLink,
}

/// TS `timelineInformation.audioClipGroups[].audioClips[]` element.
#[derive(Debug, Clone, Default)]
pub struct AudioClip {
    pub id: String,
    pub start: Fraction,
    pub duration: Fraction,
    pub in_time: Fraction,
    pub out_time: Fraction,
    pub muted: bool,
    pub audio_level: f64,
    pub frame_reader: AudioClipFrameReader,
}

/// TS `timelineInformation.audioClipGroups[]` element.
#[derive(Debug, Clone, Default)]
pub struct AudioClipGroup {
    pub id: String,
    pub muted: bool,
    pub audio_clips: Vec<AudioClip>,
}

/// TS `ImageResources.timelineInformation`.
#[derive(Debug, Clone, Default)]
pub struct TimelineInformation {
    pub enabled: bool,
    pub frame_step: Fraction,
    pub frame_rate: f64,
    pub time: Fraction,
    pub duration: Fraction,
    pub work_in_time: Fraction,
    pub work_out_time: Fraction,
    pub repeats: f64,
    pub has_motion: bool,
    pub global_tracks: Vec<TimelineTrack>,
    pub audio_clip_groups: Option<Vec<AudioClipGroup>>,
}

/// TS `sheetDisclosure.sheetTimelineOptions[]` element.
#[derive(Debug, Clone, Copy, Default)]
pub struct SheetTimelineOption {
    pub sheet_id: f64,
    pub sheet_disclosed: bool,
    pub lights_disclosed: bool,
    pub meshes_disclosed: bool,
    pub materials_disclosed: bool,
}

/// TS `ImageResources.sheetDisclosure`.
#[derive(Debug, Clone, Default)]
pub struct SheetDisclosure {
    pub sheet_timeline_options: Option<Vec<SheetTimelineOption>>,
}

/// TS `ImageResources.countInformation[]` element.
#[derive(Debug, Clone, Default)]
pub struct CountInformation {
    pub color: Rgb,
    pub name: String,
    pub size: f64,
    pub font_size: f64,
    pub visible: bool,
    pub points: Vec<PointF>,
}

/// slice origin: "userGenerated" | "autoGenerated" | "layer"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceOrigin {
    /// "userGenerated"
    UserGenerated,
    /// "autoGenerated"
    AutoGenerated,
    /// "layer"
    Layer,
}

/// slice type: "image" | "noImage"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    /// "image"
    Image,
    /// "noImage"
    NoImage,
}

/// slice alignment (only "default" observed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceAlignment {
    /// "default"
    Default,
}

/// slice background color type: "none" | "matte" | "color"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceBackgroundColorType {
    /// "none"
    None,
    /// "matte"
    Matte,
    /// "color"
    Color,
}

/// TS `slices[].slices[]` element.
#[derive(Debug, Clone, Default)]
pub struct Slice {
    pub id: f64,
    pub group_id: f64,
    pub origin: Option<SliceOrigin>,
    pub associated_layer_id: f64,
    pub name: Option<String>,
    /// TS field `type`
    pub slice_type: Option<SliceType>,
    pub bounds: LtrbBounds,
    pub url: String,
    pub target: String,
    pub message: String,
    pub alt_tag: String,
    pub cell_text_is_html: bool,
    pub cell_text: String,
    pub horizontal_alignment: Option<SliceAlignment>,
    pub vertical_alignment: Option<SliceAlignment>,
    pub background_color_type: Option<SliceBackgroundColorType>,
    pub background_color: Rgba,
    pub top_outset: Option<f64>,
    pub left_outset: Option<f64>,
    pub bottom_outset: Option<f64>,
    pub right_outset: Option<f64>,
}

/// TS `ImageResources.slices[]` element.
#[derive(Debug, Clone, Default)]
pub struct SliceGroup {
    pub bounds: LtrbBounds,
    pub group_name: String,
    pub slices: Vec<Slice>,
}

/// TS `layerComps.list[]` element.
#[derive(Debug, Clone)]
pub struct LayerCompListItem {
    pub id: f64,
    pub name: String,
    pub comment: Option<String>,
    pub captured_info: LayerCompCapturedInfo,
}

/// TS `ImageResources.layerComps`.
#[derive(Debug, Clone, Default)]
pub struct LayerCompsResource {
    pub list: Vec<LayerCompListItem>,
    pub last_applied: Option<f64>,
}

/// TS `ImageResources.pixelAspectRatio`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PixelAspectRatio {
    pub aspect: f64,
}

/// TS `ImageResources`.
#[derive(Debug, Clone, Default)]
pub struct ImageResources {
    pub layer_state: Option<f64>,
    pub layer_selection_ids: Option<Vec<f64>>,
    pub version_info: Option<VersionInfo>,
    pub alpha_identifiers: Option<Vec<f64>>,
    pub alpha_channel_names: Option<Vec<String>>,
    pub global_angle: Option<f64>,
    pub global_altitude: Option<f64>,
    pub pixel_aspect_ratio: Option<PixelAspectRatio>,
    pub urls_list: Option<Vec<UrlListItem>>,
    pub grid_and_guides_information: Option<GridAndGuidesInformation>,
    pub resolution_info: Option<ResolutionInfo>,
    /// TS `thumbnail?: HTMLCanvasElement` -> raw pixels.
    pub thumbnail: Option<PixelData>,
    pub thumbnail_raw: Option<ThumbnailRaw>,
    pub caption_digest: Option<String>,
    pub xmp_metadata: Option<String>,
    pub print_scale: Option<PrintScale>,
    pub print_information: Option<PrintInformation>,
    pub background_color: Option<Color>,
    pub ids_seed_number: Option<f64>,
    pub print_flags: Option<PrintFlags>,
    pub icc_untagged_profile: Option<bool>,
    pub path_selection_state: Option<Vec<String>>,
    pub image_ready_variables: Option<String>,
    pub image_ready_data_sets: Option<String>,
    pub animations: Option<Animations>,
    pub onion_skins: Option<OnionSkins>,
    pub timeline_information: Option<TimelineInformation>,
    pub sheet_disclosure: Option<SheetDisclosure>,
    pub count_information: Option<Vec<CountInformation>>,
    pub slices: Option<Vec<SliceGroup>>,
    pub layer_comps: Option<LayerCompsResource>,
    pub copyrighted: Option<bool>,
    pub url: Option<String>,
}

impl ImageResources {
    /// True when no image resource has been decoded into this struct.
    ///
    /// Mirrors upstream's `Object.keys(rest).length` guard, where `rest` is the
    /// image-resource bag minus `layersGroup`/`layerGroupsEnabledId` — exactly the
    /// fields modelled here. The reader uses it to leave `Psd::image_resources` at
    /// `None` for a document that carries no resources at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        // Destructured exhaustively on purpose: adding a field to `ImageResources`
        // must force this emptiness check to be reconsidered rather than silently
        // ignoring the new field.
        let ImageResources {
            layer_state,
            layer_selection_ids,
            version_info,
            alpha_identifiers,
            alpha_channel_names,
            global_angle,
            global_altitude,
            pixel_aspect_ratio,
            urls_list,
            grid_and_guides_information,
            resolution_info,
            thumbnail,
            thumbnail_raw,
            caption_digest,
            xmp_metadata,
            print_scale,
            print_information,
            background_color,
            ids_seed_number,
            print_flags,
            icc_untagged_profile,
            path_selection_state,
            image_ready_variables,
            image_ready_data_sets,
            animations,
            onion_skins,
            timeline_information,
            sheet_disclosure,
            count_information,
            slices,
            layer_comps,
            copyrighted,
            url,
        } = self;

        layer_state.is_none()
            && layer_selection_ids.is_none()
            && version_info.is_none()
            && alpha_identifiers.is_none()
            && alpha_channel_names.is_none()
            && global_angle.is_none()
            && global_altitude.is_none()
            && pixel_aspect_ratio.is_none()
            && urls_list.is_none()
            && grid_and_guides_information.is_none()
            && resolution_info.is_none()
            && thumbnail.is_none()
            && thumbnail_raw.is_none()
            && caption_digest.is_none()
            && xmp_metadata.is_none()
            && print_scale.is_none()
            && print_information.is_none()
            && background_color.is_none()
            && ids_seed_number.is_none()
            && print_flags.is_none()
            && icc_untagged_profile.is_none()
            && path_selection_state.is_none()
            && image_ready_variables.is_none()
            && image_ready_data_sets.is_none()
            && animations.is_none()
            && onion_skins.is_none()
            && timeline_information.is_none()
            && sheet_disclosure.is_none()
            && count_information.is_none()
            && slices.is_none()
            && layer_comps.is_none()
            && copyrighted.is_none()
            && url.is_none()
    }
}

// ===========================================================================
// Global mask info / annotations
// ===========================================================================

/// TS `GlobalLayerMaskInfo`.
#[derive(Debug, Clone, Default)]
pub struct GlobalLayerMaskInfo {
    pub overlay_color_space: f64,
    pub color_space1: f64,
    pub color_space2: f64,
    pub color_space3: f64,
    pub color_space4: f64,
    pub opacity: f64,
    pub kind: f64,
}

/// annotation type: "text" | "sound"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationType {
    /// "text"
    Text,
    /// "sound"
    Sound,
}

/// TS `Annotation.data = string | Uint8Array`.
#[derive(Debug, Clone)]
pub enum AnnotationData {
    Text(String),
    Binary(Vec<u8>),
}

/// TS `Annotation`.
#[derive(Debug, Clone)]
pub struct Annotation {
    /// TS field `type`
    pub annotation_type: AnnotationType,
    pub open: bool,
    pub icon_location: LtrbBounds,
    pub popup_location: LtrbBounds,
    pub color: Color,
    pub author: String,
    pub name: String,
    pub date: String,
    pub data: AnnotationData,
}

// ===========================================================================
// Raw channel data
// ===========================================================================

/// TS `LayerRawDataChannel`.
#[derive(Debug, Clone)]
pub struct LayerRawDataChannel {
    pub id: ChannelId,
    pub compression: Compression,
    pub data: Option<Vec<u8>>,
}

/// TS `LayerRawData`.
#[derive(Debug, Clone)]
pub struct LayerRawData {
    pub color_mode: ColorMode,
    pub bits_per_channel: f64,
    pub channels: Vec<LayerRawDataChannel>,
    pub large: bool,
}

// ===========================================================================
// Layer / Psd
// ===========================================================================

/// TS `Layer extends LayerAdditionalInfo`.
///
/// `children` is `Vec<Layer>` (recursive); since it lives behind a `Vec`, no
/// `Box` is needed to break the recursion.
#[derive(Debug, Clone, Default)]
pub struct Layer {
    /// flattened `LayerAdditionalInfo` base.
    pub additional_info: LayerAdditionalInfo,

    pub top: Option<f64>,
    pub left: Option<f64>,
    pub bottom: Option<f64>,
    pub right: Option<f64>,
    pub blend_mode: Option<BlendMode>,
    pub opacity: Option<f64>,
    pub transparency_protected: Option<bool>,
    /// effects/filters panel is expanded
    pub effects_open: Option<bool>,
    pub hidden: Option<bool>,
    pub clipping: Option<bool>,
    /// TS `canvas?: HTMLCanvasElement` -> raw pixels.
    pub canvas: Option<PixelData>,
    pub image_data: Option<PixelData>,
    pub raw_data: Option<LayerRawData>,
    pub children: Option<Vec<Layer>>,
    /// Applies only for layer groups.
    pub opened: Option<bool>,
    pub link_group: Option<f64>,
    pub link_group_enabled: Option<bool>,
}

/// TS `Psd.artboards`.
#[derive(Debug, Clone, Default)]
pub struct PsdArtboards {
    /// number of artboards in the document
    pub count: f64,
    pub auto_expand_offset: Option<HorizontalVertical>,
    pub origin: Option<HorizontalVertical>,
    pub auto_expand_enabled: Option<bool>,
    pub auto_nest_enabled: Option<bool>,
    pub auto_position_enabled: Option<bool>,
    pub shrinkwrap_on_save_enabled: Option<bool>,
    pub doc_default_new_artboard_background_color: Option<Color>,
    pub doc_default_new_artboard_background_type: Option<f64>,
}

/// TS `Psd extends LayerAdditionalInfo`.
#[derive(Debug, Clone, Default)]
pub struct Psd {
    /// flattened `LayerAdditionalInfo` base.
    pub additional_info: LayerAdditionalInfo,

    pub width: f64,
    pub height: f64,
    pub channels: Option<f64>,
    pub bits_per_channel: Option<f64>,
    pub color_mode: Option<ColorMode>,
    /// colors for indexed color mode
    pub palette: Option<Vec<Rgb>>,
    pub children: Option<Vec<Layer>>,
    /// TS `canvas?: HTMLCanvasElement` -> raw pixels.
    pub canvas: Option<PixelData>,
    pub image_data: Option<PixelData>,
    pub image_resources: Option<ImageResources>,
    /// used in smart objects
    pub linked_files: Option<Vec<LinkedFile>>,
    pub artboards: Option<PsdArtboards>,
    pub global_layer_mask_info: Option<GlobalLayerMaskInfo>,
    pub annotations: Option<Vec<Annotation>>,
    /// TS `rawCompositeData?: Uint8Array` — undecoded composite image section.
    ///
    /// Filled instead of `canvas`/`image_data` when [`ReadOptions::use_raw_data`]
    /// is set: it holds the raw bytes of the composite image data section (from
    /// its first byte to the end of the file), so decoding can be deferred to
    /// `reader::get_composite_image_data`.
    pub raw_composite_data: Option<Vec<u8>>,
}

// ===========================================================================
// Read / Write options
// ===========================================================================

/// TS `ReadOptions`.
///
/// The development-only `log?: (...args) => void` callback is not modeled here
/// (it is behaviour, not data); other dev flags are kept.
///
/// Note that [`ReadOptions::default()`] is **not** an all-`None` value: it
/// carries the upstream default memory limit (see
/// [`ReadOptions::total_memory_limit`]).
#[derive(Debug, Clone)]
pub struct ReadOptions {
    /// Does not load layer image data.
    pub skip_layer_image_data: Option<bool>,
    /// Does not load composite image data.
    pub skip_composite_image_data: Option<bool>,
    /// Does not load thumbnail.
    pub skip_thumbnail: Option<bool>,
    /// Does not load linked files (used in smart-objects).
    pub skip_linked_files_data: Option<bool>,
    /// Total memory budget, in bytes, for bitmaps decoded while reading a file.
    ///
    /// `None` means unlimited; [`ReadOptions::default()`] carries
    /// `Some(2 GiB)`. Exceeding the budget aborts the read with
    /// `reader::ReadError::ExceededMemoryLimit`.
    ///
    /// Difference from JS: upstream distinguishes an *absent* `totalMemoryLimit`
    /// property (which is replaced by the 2 GiB default) from an explicitly
    /// `undefined` one (which disables the limit). Rust has no such distinction,
    /// so the 2 GiB default lives in `Default` and `None` is the explicit
    /// "unlimited" request.
    pub total_memory_limit: Option<usize>,
    /// Throws exception if features are missing.
    pub throw_for_missing_features: Option<bool>,
    /// Logs if features are missing.
    pub log_missing_features: Option<bool>,
    /// Keep image data as byte array instead of canvas.
    pub use_image_data: Option<bool>,
    /// Skips decoding layer and composite bitmaps; they can be decoded later
    /// with the `reader::get_*_image_data` helpers.
    pub use_raw_data: Option<bool>,
    /// Loads thumbnail raw data instead of decoding into canvas.
    pub use_raw_thumbnail: Option<bool>,
    /// Used only for development.
    pub log_dev_features: Option<bool>,
    /// Used only for development.
    pub strict: Option<bool>,
    /// Used only for development.
    pub debug: Option<bool>,
    // TS `log?: (...args: any[]) => void;` — поведение, не данные; не портируем.
}

/// Upstream default: every flag off, but the bitmap memory budget set to 2 GiB
/// (`readPsd` installs that value when `totalMemoryLimit` is not present in the
/// options object). Written by hand because `#[derive(Default)]` cannot express
/// a non-`None` default for a single field.
impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            skip_layer_image_data: None,
            skip_composite_image_data: None,
            skip_thumbnail: None,
            skip_linked_files_data: None,
            total_memory_limit: Some(DEFAULT_TOTAL_MEMORY_LIMIT),
            throw_for_missing_features: None,
            log_missing_features: None,
            use_image_data: None,
            use_raw_data: None,
            use_raw_thumbnail: None,
            log_dev_features: None,
            strict: None,
            debug: None,
        }
    }
}

/// Default bitmap memory budget used by [`ReadOptions::default()`]: 2 GiB,
/// mirroring upstream `readPsd`.
pub const DEFAULT_TOTAL_MEMORY_LIMIT: usize = 2 * 1024 * 1024 * 1024;

/// TS `WriteOptions`.
#[derive(Debug, Clone, Default)]
pub struct WriteOptions {
    /// Automatically generates thumbnail from composite image.
    pub generate_thumbnail: Option<bool>,
    /// Trims transparent pixels from layer image data.
    pub trim_image_data: Option<bool>,
    /// Invalidates text layer data, forcing Photoshop to redraw on load.
    pub invalidate_text_layers: Option<bool>,
    /// Logs if features are missing.
    pub log_missing_features: Option<bool>,
    /// Forces bottom layer to be treated as layer and not background.
    pub no_background: Option<bool>,
    /// Saves document as PSB (Large Document Format) file.
    pub psb: Option<bool>,
    /// Uses zip compression when writing PSD file.
    pub compress: Option<bool>,
}
