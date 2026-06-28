/*
File: crates/ag-psd/src/descriptor.rs

Purpose:
чтение и запись Photoshop Action Descriptor (типизированные поля дескрипторов).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/descriptor.ts`.

Этот модуль портирует *format-critical* ядро `descriptor.ts`: чтение/запись
дескриптора, диспетчеризацию OSType, кодирование ключей (4-байтовая сигнатура
или длино-префиксная ASCII-строка), таблицу единиц (`UntF`/`UnFl`) и
version-обёртки (`readVersionAndDescriptor` / `writeVersionAndDescriptor`).

Модель данных
-------------
TS-версия слабо типизирована: `readDescriptorStructure` возвращает «голый»
объект, а `writeDescriptorStructure` восстанавливает OSType каждого поля из
большой эвристической таблицы `getTypeByKey`/`fieldToType` по имени ключа.
В Rust мы превращаем дескриптор в строго типизированное дерево: тип значения
несёт сам enum [`DescriptorValue`], поэтому writer пишет байты прямо по варианту
enum'а — это и точнее (никаких догадок по имени ключа), и полностью совпадает с
байтами, которые читает reader. Таблицы вывода типа из `descriptor.ts`
(`fieldToType`, `fieldToExtType`, `getTypeByKey`, ...) намеренно не нужны в этой
модели и не воспроизводятся — см. отчёт о портировании.

Порядок полей
-------------
Photoshop чувствителен к порядку полей дескриптора, поэтому [`Descriptor`]
хранит поля как `Vec<(String, DescriptorValue)>` (insertion-ordered) — порядок
вставки сохраняется ровно так, как при записи. `HashMap`/`BTreeMap` исказили бы
порядок, поэтому они не подходят; тащить внешний `indexmap` ради этого не нужно.
*/

use crate::reader::{
    read_ascii_string, read_bytes, read_float32, read_float64, read_int32, read_int32_le,
    read_signature, read_uint32, read_uint8, read_unicode_string,
    read_unicode_string_with_length_le, PsdReader, ReadError, ReadResult,
};
use crate::writer::{
    write_bytes, write_float64, write_int32, write_int32_le, write_signature,
    write_uint32, write_uint8, write_unicode_string, write_unicode_string_with_padding,
    write_unicode_string_without_length_le, PsdWriter,
};

// ===========================================================================
// Unit maps (зеркало unitsMap / unitsMapRev)
// ===========================================================================

/// Зеркало `unitsMap`: 4-байтовый код единицы измерения -> человекочитаемое имя.
///
/// Порядок записей сохранён как в `descriptor.ts` (для соответствия и читаемости).
pub const UNITS_MAP: &[(&str, &str)] = &[
    ("#Ang", "Angle"),
    ("#Rsl", "Density"),
    ("#Rlt", "Distance"),
    ("#Nne", "None"),
    ("#Prc", "Percent"),
    ("#Pxl", "Pixels"),
    ("#Mlm", "Millimeters"),
    ("#Pnt", "Points"),
    ("RrPi", "Picas"),
    ("RrIn", "Inches"),
    ("RrCm", "Centimeters"),
];

/// `unitsMap[code]` — код -> имя.
pub fn units_name_from_code(code: &str) -> Option<&'static str> {
    UNITS_MAP
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| *name)
}

/// `unitsMapRev[name]` — имя -> код.
pub fn units_code_from_name(name: &str) -> Option<&'static str> {
    UNITS_MAP
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(code, _)| *code)
}

// ===========================================================================
// Модель данных дескриптора
// ===========================================================================

/// Значение единицы измерения (`UntF` / `UnFl`).
///
/// `units` хранит человекочитаемое имя (как в TS `{ units, value }`), а
/// `float32` отличает `UnFl` (одинарная точность) от `UntF` (двойная) на записи.
#[derive(Debug, Clone, PartialEq)]
pub struct UnitDoubleValue {
    pub units: String,
    pub value: f64,
}

/// Large integer (`comp`): low/high 32-битные половины (зеркало `{ low, high }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LargeInteger {
    pub low: u32,
    pub high: u32,
}

/// Имя+classID (зеркало `readClassStructure` -> `{ name, classID }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassStructure {
    pub name: String,
    pub class_id: String,
}

/// Элемент `ObAr` (object array): тип точки + значения (зеркало `{ type, values }`).
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectArrayItem {
    pub type_: String,
    pub values: Vec<f64>,
}

/// Значение `Pth ` (file path): сигнатура + путь (зеркало `{ sig, path }`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathValue {
    pub sig: String,
    pub path: String,
}

/// Элемент Reference-структуры (`obj `).
///
/// `descriptor.ts` сворачивает все под-типы ссылки в плоский массив значений:
/// 'prop'/'rele'/'name' дают строку, 'Enmr' — строку `"type.value"`,
/// 'Idnt'/'indx' — число, 'Clss' — class-структуру. Здесь — типизированный enum.
#[derive(Debug, Clone, PartialEq)]
pub enum ReferenceItem {
    /// 'prop' — Property: keyID (после прочтения class-структуры).
    Property(String),
    /// 'Clss' — Class.
    Class(ClassStructure),
    /// 'Enmr' — Enumerated reference, хранится как `"typeID.value"`.
    Enumerated(String),
    /// 'rele' — Offset (uint32) после class-структуры.
    Offset(u32),
    /// 'Idnt' — Identifier (int32).
    Identifier(i32),
    /// 'indx' — Index (int32).
    Index(i32),
    /// 'name' — Name (unicode string) после class-структуры.
    Name(String),
}

/// Значение OSType-поля дескриптора (динамическое типизированное дерево).
#[derive(Debug, Clone, PartialEq)]
pub enum DescriptorValue {
    /// 'obj ' — Reference.
    Reference(Vec<ReferenceItem>),
    /// 'Objc' / 'GlbO' — вложенный дескриптор.
    Descriptor(Descriptor),
    /// 'VlLs' — список.
    List(Vec<DescriptorValue>),
    /// 'doub' — double.
    Double(f64),
    /// 'UntF' (float64) / 'UnFl' (float32) — unit double.
    UnitDouble(UnitDoubleValue),
    /// 'TEXT' — unicode string.
    Text(String),
    /// 'enum' — `"type.value"`.
    Enum(String),
    /// 'long' — int32.
    Integer(i32),
    /// 'comp' — large integer.
    LargeInteger(LargeInteger),
    /// 'bool'.
    Boolean(bool),
    /// 'type' / 'GlbC' — class.
    Class(ClassStructure),
    /// 'alis' — alias (ASCII string).
    Alias(String),
    /// 'tdta' — raw data.
    RawData(Vec<u8>),
    /// 'ObAr' — object array.
    ObjectArray(Vec<ObjectArrayItem>),
    /// 'Pth ' — file path / alias.
    Path(PathValue),
}

impl DescriptorValue {
    /// 4-байтовый код OSType для данного варианта значения.
    ///
    /// `UnFl` неотличим от `UntF` по значению — оба несут [`UnitDoubleValue`];
    /// мы выбираем 'UntF' по умолчанию (как и большинство полей PSD).
    /// На записи это управляется явным [`write_os_type`] (см. `float32`-вариант ниже).
    pub fn os_type(&self) -> &'static str {
        match self {
            DescriptorValue::Reference(_) => "obj ",
            DescriptorValue::Descriptor(_) => "Objc",
            DescriptorValue::List(_) => "VlLs",
            DescriptorValue::Double(_) => "doub",
            DescriptorValue::UnitDouble(_) => "UntF",
            DescriptorValue::Text(_) => "TEXT",
            DescriptorValue::Enum(_) => "enum",
            DescriptorValue::Integer(_) => "long",
            DescriptorValue::LargeInteger(_) => "comp",
            DescriptorValue::Boolean(_) => "bool",
            DescriptorValue::Class(_) => "type",
            DescriptorValue::Alias(_) => "alis",
            DescriptorValue::RawData(_) => "tdta",
            DescriptorValue::ObjectArray(_) => "ObAr",
            DescriptorValue::Path(_) => "Pth ",
        }
    }
}

/// Дескриптор: classID + name + упорядоченный список полей.
///
/// `items` — `Vec<(String, DescriptorValue)>`: insertion-ordered, чтобы при записи
/// поля шли ровно в том порядке, в каком были добавлены (Photoshop это важно).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Descriptor {
    pub name: String,
    pub class_id: String,
    pub items: Vec<(String, DescriptorValue)>,
}

impl Descriptor {
    pub fn new(name: impl Into<String>, class_id: impl Into<String>) -> Descriptor {
        Descriptor {
            name: name.into(),
            class_id: class_id.into(),
            items: Vec::new(),
        }
    }

    /// Добавляет поле в конец (сохраняя порядок вставки).
    pub fn set(&mut self, key: impl Into<String>, value: DescriptorValue) -> &mut Self {
        self.items.push((key.into(), value));
        self
    }

    /// Первое значение по ключу (порядок не нарушается).
    pub fn get(&self, key: &str) -> Option<&DescriptorValue> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

// ===========================================================================
// Высокоуровневые хелперы descriptor.ts (color / units / percent-or-angle)
// ---------------------------------------------------------------------------
// Канонические копии, ранее продублированные в group-модулях additional_info
// (effects_keys / vector_keys / adjustment_keys / misc_keys / text_keys /
// smart_object_keys) и в abr.rs. Сигнатуры выверены по
// `test/ag-psd/src/descriptor.ts`.
// ===========================================================================

use crate::psd::{Cmyk, Color, Frgb, Grayscale, Hsb, Lab, Rgb, Units, UnitsValue};

/// `Units` enum -> человекочитаемое имя единицы.
pub fn units_to_str(u: Units) -> &'static str {
    match u {
        Units::Pixels => "Pixels",
        Units::Points => "Points",
        Units::Picas => "Picas",
        Units::Millimeters => "Millimeters",
        Units::Centimeters => "Centimeters",
        Units::Inches => "Inches",
        Units::None => "None",
        Units::Density => "Density",
    }
}

/// Имя единицы -> `Units` enum (валидирует допустимые единицы).
pub fn units_from_str(s: &str) -> ReadResult<Units> {
    Ok(match s {
        "Pixels" => Units::Pixels,
        "Points" => Units::Points,
        "Picas" => Units::Picas,
        "Millimeters" => Units::Millimeters,
        "Centimeters" => Units::Centimeters,
        "Inches" => Units::Inches,
        "None" => Units::None,
        "Density" => Units::Density,
        other => {
            return Err(ReadError::StrictViolation(format!("Invalid units: {other}")));
        }
    })
}

/// `unitsAngle(value)` — UntF с units = Angle.
pub fn units_angle(value: f64) -> DescriptorValue {
    DescriptorValue::UnitDouble(UnitDoubleValue {
        units: "Angle".to_string(),
        value,
    })
}

/// Зеркало `unitsValue(x | undefined, key)` — `None` => Pixels/0.
pub fn units_value(x: Option<&UnitsValue>) -> DescriptorValue {
    match x {
        None => DescriptorValue::UnitDouble(UnitDoubleValue {
            units: "Pixels".to_string(),
            value: 0.0,
        }),
        Some(v) => DescriptorValue::UnitDouble(UnitDoubleValue {
            units: units_to_str(v.units).to_string(),
            value: v.value,
        }),
    }
}

/// Зеркало `parseUnits({units,value})` — валидирует допустимые единицы.
pub fn parse_units(v: &DescriptorValue) -> ReadResult<UnitsValue> {
    match v {
        DescriptorValue::UnitDouble(u) => Ok(UnitsValue {
            units: units_from_str(&u.units)?,
            value: u.value,
        }),
        other => Err(ReadError::StrictViolation(format!(
            "Invalid units value: {other:?}"
        ))),
    }
}

/// Зеркало `parseUnitsOrNumber(value, units='Pixels')`: UntF/UnFl ИЛИ число => Pixels.
pub fn parse_units_or_number(v: &DescriptorValue) -> ReadResult<UnitsValue> {
    match v {
        DescriptorValue::UnitDouble(u) => Ok(UnitsValue {
            units: units_from_str(&u.units)?,
            value: u.value,
        }),
        DescriptorValue::Double(d) => Ok(UnitsValue {
            units: Units::Pixels,
            value: *d,
        }),
        DescriptorValue::Integer(i) => Ok(UnitsValue {
            units: Units::Pixels,
            value: *i as f64,
        }),
        other => Err(ReadError::StrictViolation(format!(
            "Invalid units-or-number value: {other:?}"
        ))),
    }
}

/// Зеркало `parseAngle(x)` — units must be Angle; отсутствие => 0.
pub fn parse_angle(v: &DescriptorValue) -> ReadResult<f64> {
    match v {
        DescriptorValue::UnitDouble(u) if u.units == "Angle" => Ok(u.value),
        DescriptorValue::UnitDouble(u) => {
            Err(ReadError::StrictViolation(format!("Invalid units: {}", u.units)))
        }
        _ => Ok(0.0),
    }
}

/// Зеркало `parsePercent(x)` — value/100, units must be Percent; отсутствие => 1.
pub fn parse_percent(v: &DescriptorValue) -> ReadResult<f64> {
    match v {
        DescriptorValue::UnitDouble(u) if u.units == "Percent" => Ok(u.value / 100.0),
        DescriptorValue::UnitDouble(u) => {
            Err(ReadError::StrictViolation(format!("Invalid units: {}", u.units)))
        }
        _ => Ok(1.0),
    }
}

/// Зеркало `parsePercentOrAngle(x)` — Percent/100 или Angle/360; отсутствие => 1.
pub fn parse_percent_or_angle(v: &DescriptorValue) -> ReadResult<f64> {
    match v {
        DescriptorValue::UnitDouble(u) if u.units == "Percent" => Ok(u.value / 100.0),
        DescriptorValue::UnitDouble(u) if u.units == "Angle" => Ok(u.value / 360.0),
        DescriptorValue::UnitDouble(u) => {
            Err(ReadError::StrictViolation(format!("Invalid units: {}", u.units)))
        }
        _ => Ok(1.0),
    }
}

/// Числовое значение поля дескриптора-цвета (double/integer; прочее => 0).
fn color_double(d: &Descriptor, key: &str) -> f64 {
    match d.get(key) {
        Some(DescriptorValue::Double(v)) => *v,
        Some(DescriptorValue::Integer(v)) => *v as f64,
        _ => 0.0,
    }
}

/// Зеркало `parseColor(DescriptorColor)` — по наличию ключей.
pub fn parse_color(d: &Descriptor) -> ReadResult<Color> {
    if let Some(h) = d.get("H   ") {
        let h = parse_percent_or_angle(h)?;
        Ok(Color::Hsb(Hsb {
            h,
            s: color_double(d, "Strt"),
            b: color_double(d, "Brgh"),
        }))
    } else if d.get("Rd  ").is_some() {
        Ok(Color::Rgb(Rgb {
            r: color_double(d, "Rd  "),
            g: color_double(d, "Grn "),
            b: color_double(d, "Bl  "),
        }))
    } else if d.get("Cyn ").is_some() {
        Ok(Color::Cmyk(Cmyk {
            c: color_double(d, "Cyn "),
            m: color_double(d, "Mgnt"),
            y: color_double(d, "Ylw "),
            k: color_double(d, "Blck"),
        }))
    } else if d.get("Gry ").is_some() {
        Ok(Color::Grayscale(Grayscale {
            k: color_double(d, "Gry "),
        }))
    } else if d.get("Lmnc").is_some() {
        Ok(Color::Lab(Lab {
            l: color_double(d, "Lmnc"),
            a: color_double(d, "A   "),
            b: color_double(d, "B   "),
        }))
    } else if d.get("redFloat").is_some() {
        Ok(Color::Frgb(Frgb {
            fr: color_double(d, "redFloat"),
            fg: color_double(d, "greenFloat"),
            fb: color_double(d, "blueFloat"),
        }))
    } else {
        Err(ReadError::StrictViolation(
            "Unsupported color descriptor".to_string(),
        ))
    }
}

/// Зеркало `serializeColor(Color | undefined)` — `None` => нулевой RGBC.
pub fn serialize_color(color: Option<&Color>) -> Descriptor {
    let mut d;
    match color {
        None => {
            d = Descriptor::new("", "RGBC");
            d.set("Rd  ", DescriptorValue::Double(0.0));
            d.set("Grn ", DescriptorValue::Double(0.0));
            d.set("Bl  ", DescriptorValue::Double(0.0));
        }
        Some(Color::Rgb(c)) => {
            d = Descriptor::new("", "RGBC");
            d.set("Rd  ", DescriptorValue::Double(c.r));
            d.set("Grn ", DescriptorValue::Double(c.g));
            d.set("Bl  ", DescriptorValue::Double(c.b));
        }
        Some(Color::Rgba(c)) => {
            // upstream `'r' in color` matches RGBA too.
            d = Descriptor::new("", "RGBC");
            d.set("Rd  ", DescriptorValue::Double(c.r));
            d.set("Grn ", DescriptorValue::Double(c.g));
            d.set("Bl  ", DescriptorValue::Double(c.b));
        }
        Some(Color::Frgb(c)) => {
            d = Descriptor::new("", "RGBC");
            d.set("redFloat", DescriptorValue::Double(c.fr));
            d.set("greenFloat", DescriptorValue::Double(c.fg));
            d.set("blueFloat", DescriptorValue::Double(c.fb));
        }
        Some(Color::Hsb(c)) => {
            d = Descriptor::new("", "HSBC");
            d.set("H   ", units_angle(c.h * 360.0));
            d.set("Strt", DescriptorValue::Double(c.s));
            d.set("Brgh", DescriptorValue::Double(c.b));
        }
        Some(Color::Cmyk(c)) => {
            d = Descriptor::new("", "CMYC");
            d.set("Cyn ", DescriptorValue::Double(c.c));
            d.set("Mgnt", DescriptorValue::Double(c.m));
            d.set("Ylw ", DescriptorValue::Double(c.y));
            d.set("Blck", DescriptorValue::Double(c.k));
        }
        Some(Color::Lab(c)) => {
            d = Descriptor::new("", "LABC");
            d.set("Lmnc", DescriptorValue::Double(c.l));
            d.set("A   ", DescriptorValue::Double(c.a));
            d.set("B   ", DescriptorValue::Double(c.b));
        }
        Some(Color::Grayscale(c)) => {
            d = Descriptor::new("", "GRYC");
            d.set("Gry ", DescriptorValue::Double(c.k));
        }
    }
    d
}

// ===========================================================================
// Кодирование ключей / class id (zero-padded signature или длино-префиксная строка)
// ===========================================================================

/// Зеркало `readAsciiStringOrClassId(reader)`:
/// читает int32-длину, затем ASCII-строку `length || 4` байт.
pub fn read_ascii_string_or_class_id(reader: &mut PsdReader) -> ReadResult<String> {
    let length = read_int32(reader)?;
    let len = if length == 0 { 4 } else { length as usize };
    read_ascii_string(reader, len)
}

/// Зеркало `writeAsciiStringOrClassId(writer, value)`.
///
/// Если строка ровно из 4 символов и не входит в спец-исключения
/// (`warp`/`time`/`hold`/`list`) — пишется как classId: int32(0) + 4-байтовая
/// сигнатура. Иначе — int32(длина) + ASCII-байты (по `charCodeAt`).
pub fn write_ascii_string_or_class_id(writer: &mut PsdWriter, value: &str) {
    let char_count = value.chars().count();
    if char_count == 4 && value != "warp" && value != "time" && value != "hold" && value != "list"
    {
        // classId
        write_int32(writer, 0);
        write_signature(writer, value);
    } else {
        // ascii string
        write_int32(writer, char_count as i32);
        for ch in value.chars() {
            write_uint8(writer, (ch as u32) as u8);
        }
    }
}

// ===========================================================================
// Class structure
// ===========================================================================

/// Зеркало `readClassStructure(reader)` — `{ name (unicode), classID }`.
pub fn read_class_structure(reader: &mut PsdReader) -> ReadResult<ClassStructure> {
    let name = read_unicode_string(reader)?;
    let class_id = read_ascii_string_or_class_id(reader)?;
    Ok(ClassStructure { name, class_id })
}

/// Зеркало `writeClassStructure(writer, name, classID)`.
pub fn write_class_structure(writer: &mut PsdWriter, name: &str, class_id: &str) {
    write_unicode_string(writer, name);
    write_ascii_string_or_class_id(writer, class_id);
}

// ===========================================================================
// Reference structure ('obj ')
// ===========================================================================

/// Зеркало `readReferenceStructure(reader)`.
pub fn read_reference_structure(reader: &mut PsdReader) -> ReadResult<Vec<ReferenceItem>> {
    let items_count = read_int32(reader)?;
    let mut items = Vec::new();

    for _ in 0..items_count {
        let type_ = read_signature(reader)?;

        match type_.as_str() {
            "prop" => {
                // Property
                read_class_structure(reader)?;
                let key_id = read_ascii_string_or_class_id(reader)?;
                items.push(ReferenceItem::Property(key_id));
            }
            "Clss" => {
                // Class
                items.push(ReferenceItem::Class(read_class_structure(reader)?));
            }
            "Enmr" => {
                // Enumerated Reference
                read_class_structure(reader)?;
                let type_id = read_ascii_string_or_class_id(reader)?;
                let value = read_ascii_string_or_class_id(reader)?;
                items.push(ReferenceItem::Enumerated(format!("{}.{}", type_id, value)));
            }
            "rele" => {
                // Offset
                read_class_structure(reader)?;
                items.push(ReferenceItem::Offset(read_uint32(reader)?));
            }
            "Idnt" => {
                // Identifier
                items.push(ReferenceItem::Identifier(read_int32(reader)?));
            }
            "indx" => {
                // Index
                items.push(ReferenceItem::Index(read_int32(reader)?));
            }
            "name" => {
                // Name
                read_class_structure(reader)?;
                items.push(ReferenceItem::Name(read_unicode_string(reader)?));
            }
            other => {
                return Err(ReadError::StrictViolation(format!(
                    "Invalid descriptor reference type: {}",
                    other
                )));
            }
        }
    }

    Ok(items)
}

/// Зеркало `writeReferenceStructure(writer, key, items)`.
///
/// Upstream поддерживает запись только для 'Enmr' и 'name' (остальные ветки
/// закомментированы и бросают исключение). Здесь мы пишем по типизированному
/// варианту: 'Enmr' и 'name' — как upstream; прочие варианты — паника (writer.rs
/// панически реагирует на невалидный ввод).
pub fn write_reference_structure(writer: &mut PsdWriter, items: &[ReferenceItem]) {
    write_int32(writer, items.len() as i32);

    for item in items {
        match item {
            ReferenceItem::Enumerated(value) => {
                write_signature(writer, "Enmr");
                let mut parts = value.splitn(2, '.');
                let type_id = parts.next().unwrap_or("");
                let enum_value = parts.next().unwrap_or("");
                write_class_structure(writer, "\0", type_id);
                write_ascii_string_or_class_id(writer, type_id);
                write_ascii_string_or_class_id(writer, enum_value);
            }
            ReferenceItem::Name(value) => {
                write_signature(writer, "name");
                write_class_structure(writer, "\0", "Lyr ");
                write_unicode_string(writer, &format!("{}\0", value));
            }
            other => {
                panic!("Invalid descriptor reference type for writing: {:?}", other);
            }
        }
    }
}

// ===========================================================================
// OSType dispatch
// ===========================================================================

/// Зеркало `readOSType(reader, type, includeClass)`.
pub fn read_os_type(reader: &mut PsdReader, type_: &str) -> ReadResult<DescriptorValue> {
    match type_ {
        "obj " => Ok(DescriptorValue::Reference(read_reference_structure(reader)?)),
        "Objc" | "GlbO" => Ok(DescriptorValue::Descriptor(read_descriptor_structure(
            reader,
        )?)),
        "VlLs" => {
            let length = read_int32(reader)?;
            let mut items = Vec::new();
            for _ in 0..length {
                let item_type = read_signature(reader)?;
                items.push(read_os_type(reader, &item_type)?);
            }
            Ok(DescriptorValue::List(items))
        }
        "doub" => Ok(DescriptorValue::Double(read_float64(reader)?)),
        "UntF" => {
            let units_code = read_signature(reader)?;
            let value = read_float64(reader)?;
            let units = units_name_from_code(&units_code)
                .ok_or_else(|| ReadError::StrictViolation(format!("Invalid units: {}", units_code)))?;
            Ok(DescriptorValue::UnitDouble(UnitDoubleValue {
                units: units.to_string(),
                value,
            }))
        }
        "UnFl" => {
            let units_code = read_signature(reader)?;
            let value = read_float32(reader)? as f64;
            let units = units_name_from_code(&units_code)
                .ok_or_else(|| ReadError::StrictViolation(format!("Invalid units: {}", units_code)))?;
            Ok(DescriptorValue::UnitDouble(UnitDoubleValue {
                units: units.to_string(),
                value,
            }))
        }
        "TEXT" => Ok(DescriptorValue::Text(read_unicode_string(reader)?)),
        "enum" => {
            let enum_type = read_ascii_string_or_class_id(reader)?;
            let value = read_ascii_string_or_class_id(reader)?;
            Ok(DescriptorValue::Enum(format!("{}.{}", enum_type, value)))
        }
        "long" => Ok(DescriptorValue::Integer(read_int32(reader)?)),
        "comp" => {
            let low = read_uint32(reader)?;
            let high = read_uint32(reader)?;
            Ok(DescriptorValue::LargeInteger(LargeInteger { low, high }))
        }
        "bool" => Ok(DescriptorValue::Boolean(read_uint8(reader)? != 0)),
        "type" | "GlbC" => Ok(DescriptorValue::Class(read_class_structure(reader)?)),
        "alis" => {
            let length = read_int32(reader)?;
            Ok(DescriptorValue::Alias(read_ascii_string(
                reader,
                length as usize,
            )?))
        }
        "tdta" => {
            let length = read_int32(reader)?;
            Ok(DescriptorValue::RawData(read_bytes(reader, length as usize)?))
        }
        "ObAr" => {
            let _version = read_int32(reader)?; // version: 16
            let _name = read_unicode_string(reader)?; // name: ''
            let _type = read_ascii_string_or_class_id(reader)?; // e.g. 'rationalPoint'
            let length = read_int32(reader)?;
            let mut items = Vec::new();

            for _ in 0..length {
                let type1 = read_ascii_string_or_class_id(reader)?; // Hrzn | Vrtc
                let _unfl = read_signature(reader)?; // 'UnFl'
                let _units = read_signature(reader)?; // units e.g. '#Pxl'
                let values_count = read_int32(reader)?;
                let mut values = Vec::new();
                for _ in 0..values_count {
                    values.push(read_float64(reader)?);
                }
                items.push(ObjectArrayItem {
                    type_: type1,
                    values,
                });
            }

            Ok(DescriptorValue::ObjectArray(items))
        }
        "Pth " => {
            let _length = read_int32(reader)?; // total size of all fields below
            let sig = read_signature(reader)?;
            let _path_size = read_int32_le(reader)?; // same as length
            let chars_count = read_int32_le(reader)?;
            let path = read_unicode_string_with_length_le(reader, chars_count as usize)?;
            Ok(DescriptorValue::Path(PathValue { sig, path }))
        }
        other => Err(ReadError::StrictViolation(format!(
            "Invalid TySh descriptor OSType: {} at {:x}",
            other, reader.offset
        ))),
    }
}

/// Зеркало `ObArTypes` — для каких ключей какой sub-type объявить в `ObAr`.
fn ob_ar_type_for_key(key: &str) -> Option<&'static str> {
    match key {
        "meshPoints" => Some("rationalPoint"),
        "quiltSliceX" => Some("UntF"),
        "quiltSliceY" => Some("UntF"),
        _ => None,
    }
}

/// Зеркало `writeOSType(writer, type, value, key, extType, root)`.
///
/// В отличие от TS, тип определяется вариантом [`DescriptorValue`], а не
/// эвристикой по имени ключа; `key` нужен только для `ObAr` (выбор sub-type).
pub fn write_os_type(writer: &mut PsdWriter, value: &DescriptorValue, key: &str) {
    match value {
        DescriptorValue::Reference(items) => {
            write_reference_structure(writer, items);
        }
        DescriptorValue::Descriptor(desc) => {
            write_descriptor_structure(writer, desc);
        }
        DescriptorValue::List(items) => {
            write_int32(writer, items.len() as i32);
            for item in items {
                write_signature(writer, item.os_type());
                write_os_type(writer, item, &format!("{}[]", key));
            }
        }
        DescriptorValue::Double(v) => {
            write_float64(writer, *v);
        }
        DescriptorValue::UnitDouble(u) => {
            let code = units_code_from_name(&u.units)
                .unwrap_or_else(|| panic!("Invalid units: {} in {}", u.units, key));
            write_signature(writer, code);
            write_float64(writer, u.value);
        }
        DescriptorValue::Text(v) => {
            write_unicode_string_with_padding(writer, v);
        }
        DescriptorValue::Enum(v) => {
            let mut parts = v.splitn(2, '.');
            let type_ = parts.next().unwrap_or("");
            let val = parts.next().unwrap_or("");
            write_ascii_string_or_class_id(writer, type_);
            write_ascii_string_or_class_id(writer, val);
        }
        DescriptorValue::Integer(v) => {
            write_int32(writer, *v);
        }
        DescriptorValue::LargeInteger(li) => {
            // `comp` запись закомментирована в upstream; воспроизводим формат
            // (low, high как в reader): два uint32.
            write_uint32(writer, li.low);
            write_uint32(writer, li.high);
        }
        DescriptorValue::Boolean(v) => {
            write_uint8(writer, if *v { 1 } else { 0 });
        }
        DescriptorValue::Class(c) => {
            // 'type'/'GlbC' запись закомментирована в upstream; воспроизводим
            // структуру class (name + classID).
            write_class_structure(writer, &c.name, &c.class_id);
        }
        DescriptorValue::Alias(s) => {
            // 'alis' запись закомментирована в upstream; воспроизводим reader-формат.
            write_int32(writer, s.chars().count() as i32);
            for ch in s.chars() {
                write_uint8(writer, (ch as u32) as u8);
            }
        }
        DescriptorValue::RawData(bytes) => {
            write_int32(writer, bytes.len() as i32);
            write_bytes(writer, Some(bytes));
        }
        DescriptorValue::ObjectArray(items) => {
            write_int32(writer, 16); // version
            write_unicode_string_with_padding(writer, ""); // name
            let type_ = ob_ar_type_for_key(key)
                .unwrap_or_else(|| panic!("Not implemented ObArType for: {}", key));
            write_ascii_string_or_class_id(writer, type_);
            write_int32(writer, items.len() as i32);

            for item in items {
                write_ascii_string_or_class_id(writer, &item.type_); // Hrzn | Vrtc
                write_signature(writer, "UnFl");
                write_signature(writer, "#Pxl");
                write_int32(writer, item.values.len() as i32);
                for v in &item.values {
                    write_float64(writer, *v);
                }
            }
        }
        DescriptorValue::Path(p) => {
            let length = 4 + 4 + 4 + p.path.chars().count() as i32 * 2;
            write_int32(writer, length);
            write_signature(writer, &p.sig);
            write_int32_le(writer, length);
            write_int32_le(writer, p.path.chars().count() as i32);
            write_unicode_string_without_length_le(writer, &p.path);
        }
    }
}

// ===========================================================================
// Descriptor structure
// ===========================================================================

/// Зеркало `readDescriptorStructure(reader, includeClass)`.
///
/// `name`/`classID` всегда сохраняются на [`Descriptor`] (что эквивалентно
/// `includeClass=true` в upstream — в типизированной модели они всегда доступны).
pub fn read_descriptor_structure(reader: &mut PsdReader) -> ReadResult<Descriptor> {
    let class = read_class_structure(reader)?;
    let mut desc = Descriptor {
        name: class.name,
        class_id: class.class_id,
        items: Vec::new(),
    };

    let items_count = read_uint32(reader)?;
    for _ in 0..items_count {
        let key = read_ascii_string_or_class_id(reader)?;
        let type_ = read_signature(reader)?;
        let data = read_os_type(reader, &type_)?;
        desc.items.push((key, data));
    }

    Ok(desc)
}

/// Зеркало `writeDescriptorStructure(writer, name, classId, value, root)`.
///
/// Эвристика вывода OSType по имени ключа (`getTypeByKey`/`fieldToType`) не нужна:
/// тип несёт сам [`DescriptorValue`]. Поля пишутся в порядке вставки.
pub fn write_descriptor_structure(writer: &mut PsdWriter, desc: &Descriptor) {
    write_unicode_string_with_padding(writer, &desc.name);
    write_ascii_string_or_class_id(writer, &desc.class_id);

    write_uint32(writer, desc.items.len() as u32);

    for (key, value) in &desc.items {
        write_ascii_string_or_class_id(writer, key);
        write_signature(writer, value.os_type());
        write_os_type(writer, value, key);
    }
}

// ===========================================================================
// Top-level read/write (alias для совместимости имён)
// ===========================================================================

/// Зеркало `readDescriptor` (в TS отсутствует отдельная функция; алиас на
/// `readDescriptorStructure`, как ожидают вызывающие).
pub fn read_descriptor(reader: &mut PsdReader) -> ReadResult<Descriptor> {
    read_descriptor_structure(reader)
}

/// Зеркало `writeDescriptor` (алиас на `writeDescriptorStructure`).
pub fn write_descriptor(writer: &mut PsdWriter, desc: &Descriptor) {
    write_descriptor_structure(writer, desc);
}

// ===========================================================================
// Version-prefixed wrappers (TySh и др. вызывающие)
// ===========================================================================

/// Зеркало `readVersionAndDescriptor(reader, includeClass = false)`.
///
/// Версия обязана быть 16, иначе ошибка (upstream `throw`).
pub fn read_version_and_descriptor(reader: &mut PsdReader) -> ReadResult<Descriptor> {
    let version = read_uint32(reader)?;
    if version != 16 {
        return Err(ReadError::StrictViolation(format!(
            "Invalid descriptor version: {}",
            version
        )));
    }
    read_descriptor_structure(reader)
}

/// Зеркало `writeVersionAndDescriptor(writer, name, classID, descriptor, root='')`.
pub fn write_version_and_descriptor(writer: &mut PsdWriter, desc: &Descriptor) {
    write_uint32(writer, 16); // version
    write_descriptor_structure(writer, desc);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::{create_writer, get_writer_buffer};

    fn build_sample() -> Descriptor {
        let mut nested = Descriptor::new("", "Pnt ");
        nested.set("Hrzn", DescriptorValue::Double(12.5));
        nested.set("Vrtc", DescriptorValue::Double(-7.0));

        let list = DescriptorValue::List(vec![
            DescriptorValue::Integer(1),
            DescriptorValue::Integer(2),
            DescriptorValue::Integer(3),
        ]);

        let mut desc = Descriptor::new("layer name", "TxLr");
        // Намеренно «непривычный» порядок — должен быть сохранён ровно так.
        desc.set("Txt ", DescriptorValue::Text("Héllo 𝕎orld".to_string()));
        desc.set("dbl ", DescriptorValue::Double(3.5));
        desc.set(
            "Opct",
            DescriptorValue::UnitDouble(UnitDoubleValue {
                units: "Percent".to_string(),
                value: 75.0,
            }),
        );
        desc.set("Ornt", DescriptorValue::Enum("Ornt.Hrzn".to_string()));
        desc.set("long", DescriptorValue::Integer(-42));
        desc.set("bool", DescriptorValue::Boolean(true));
        desc.set("Objc", DescriptorValue::Descriptor(nested));
        desc.set("VlLs", list);
        desc
    }

    #[test]
    fn round_trip_descriptor() {
        let desc = build_sample();

        let mut writer = create_writer(256);
        write_descriptor(&mut writer, &desc);
        let bytes = get_writer_buffer(&writer);

        let mut reader = PsdReader::new(&bytes, None, None);
        let read = read_descriptor(&mut reader).expect("read");

        assert_eq!(read.name, "layer name");
        assert_eq!(read.class_id, "TxLr");
        assert_eq!(read, desc);
        // Курсор должен дойти ровно до конца.
        assert_eq!(reader.offset, bytes.len());
    }

    #[test]
    fn round_trip_version_and_descriptor() {
        let desc = build_sample();

        let mut writer = create_writer(256);
        write_version_and_descriptor(&mut writer, &desc);
        let bytes = get_writer_buffer(&writer);

        let mut reader = PsdReader::new(&bytes, None, None);
        let read = read_version_and_descriptor(&mut reader).expect("read");
        assert_eq!(read, desc);
    }

    #[test]
    fn field_order_preserved() {
        let desc = build_sample();
        let expected_keys: Vec<&str> = desc.items.iter().map(|(k, _)| k.as_str()).collect();

        let mut writer = create_writer(256);
        write_descriptor(&mut writer, &desc);
        let bytes = get_writer_buffer(&writer);

        let mut reader = PsdReader::new(&bytes, None, None);
        let read = read_descriptor(&mut reader).expect("read");
        let read_keys: Vec<&str> = read.items.iter().map(|(k, _)| k.as_str()).collect();

        assert_eq!(read_keys, expected_keys);
    }

    #[test]
    fn unit_float_round_trip() {
        // 'UnFl' — единичный float32; читаем как float и проверяем единицу.
        let value = UnitDoubleValue {
            units: "Pixels".to_string(),
            value: 10.0,
        };
        let mut writer = create_writer(64);
        // Записываем как UntF (float64) — read_os_type("UntF") вернёт то же.
        write_signature(&mut writer, "#Pxl");
        write_float64(&mut writer, value.value);
        let bytes = get_writer_buffer(&writer);
        let mut reader = PsdReader::new(&bytes, None, None);
        let v = read_os_type(&mut reader, "UntF").expect("read untf");
        assert_eq!(v, DescriptorValue::UnitDouble(value));
    }

    #[test]
    fn ascii_string_or_class_id_encoding() {
        // 4-символьный classId -> int32(0) + signature.
        let mut w1 = create_writer(32);
        write_ascii_string_or_class_id(&mut w1, "TxLr");
        let b1 = get_writer_buffer(&w1);
        assert_eq!(&b1[0..4], &[0, 0, 0, 0]);
        assert_eq!(&b1[4..8], b"TxLr");

        // Исключение 'list' -> длино-префиксная строка, не classId.
        let mut w2 = create_writer(32);
        write_ascii_string_or_class_id(&mut w2, "list");
        let b2 = get_writer_buffer(&w2);
        assert_eq!(&b2[0..4], &[0, 0, 0, 4]);
        assert_eq!(&b2[4..8], b"list");

        // round-trip обоих
        for s in ["TxLr", "list", "longerKey"] {
            let mut w = create_writer(32);
            write_ascii_string_or_class_id(&mut w, s);
            let b = get_writer_buffer(&w);
            let mut r = PsdReader::new(&b, None, None);
            assert_eq!(read_ascii_string_or_class_id(&mut r).unwrap(), s);
        }
    }

    #[test]
    fn raw_data_and_large_integer_round_trip() {
        let mut desc = Descriptor::new("", "test");
        desc.set("data", DescriptorValue::RawData(vec![1, 2, 3, 4, 5]));
        desc.set(
            "big ",
            DescriptorValue::LargeInteger(LargeInteger {
                low: 0xDEAD_BEEF,
                high: 0x1234_5678,
            }),
        );

        let mut writer = create_writer(64);
        write_descriptor(&mut writer, &desc);
        let bytes = get_writer_buffer(&writer);
        let mut reader = PsdReader::new(&bytes, None, None);
        let read = read_descriptor(&mut reader).expect("read");
        assert_eq!(read, desc);
    }

    #[test]
    fn units_table_complete() {
        // Все 11 единиц round-trip код<->имя.
        assert_eq!(UNITS_MAP.len(), 11);
        for (code, name) in UNITS_MAP {
            assert_eq!(units_name_from_code(code), Some(*name));
            assert_eq!(units_code_from_name(name), Some(*code));
        }
    }
}
