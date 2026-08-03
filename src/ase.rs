/*
File: crates/ag-psd/src/ase.rs

Purpose:
чтение/запись палитр Adobe Swatch Exchange (.ase).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/ase.ts`.
- upstream предоставляет ТОЛЬКО `readAse` (read-only). `write_ase` добавлен здесь
  как симметричная функция (нужна для round-trip теста); upstream-аналога нет.

Main responsibilities:
- декодировать/кодировать блоки ASEF (цвета, группы) с типами RGB/CMYK/Gray/LAB.
*/

use crate::reader::{
    read_float32, read_signature, read_uint16, read_uint32, read_unicode_string_with_length,
    PsdReader, ReadError, ReadResult,
};
use crate::writer::{
    create_writer, get_writer_buffer, write_float32, write_signature, write_uint16, write_uint32,
    PsdWriter,
};

/// TS `AseColorType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AseColorType {
    /// "global"
    Global,
    /// "spot"
    Spot,
    /// "normal"
    Normal,
}

impl AseColorType {
    /// `colorTypes[index]` — индекс из файла -> тип.
    fn from_index(index: u16) -> Option<AseColorType> {
        match index {
            0 => Some(AseColorType::Global),
            1 => Some(AseColorType::Spot),
            2 => Some(AseColorType::Normal),
            _ => None,
        }
    }

    fn to_index(self) -> u16 {
        match self {
            AseColorType::Global => 0,
            AseColorType::Spot => 1,
            AseColorType::Normal => 2,
        }
    }
}

/// TS `AseColor.color` union (RGB / CMYK / Gray / LAB), несёт `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum AseColorValue {
    /// 'RGB '
    Rgb {
        r: f32,
        g: f32,
        b: f32,
        type_: AseColorType,
    },
    /// 'CMYK'
    Cmyk {
        c: f32,
        m: f32,
        y: f32,
        k: f32,
        type_: AseColorType,
    },
    /// 'Gray'
    Gray { k: f32, type_: AseColorType },
    /// 'LAB '
    Lab {
        l: f32,
        a: f32,
        b: f32,
        type_: AseColorType,
    },
}

/// TS `AseColor`.
#[derive(Debug, Clone, PartialEq)]
pub struct AseColor {
    pub name: String,
    pub color: AseColorValue,
}

/// TS `AseGroup`.
#[derive(Debug, Clone, PartialEq)]
pub struct AseGroup {
    pub name: String,
    pub colors: Vec<AseColor>,
}

/// TS `Ase.colors` union элемент (`AseGroup | AseColor`).
#[derive(Debug, Clone, PartialEq)]
pub enum AseEntry {
    Color(AseColor),
    Group(AseGroup),
}

/// TS `Ase`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ase {
    pub colors: Vec<AseEntry>,
}

/// Порт `readAse(buffer)`.
pub fn read_ase(buffer: &[u8]) -> ReadResult<Ase> {
    let reader = &mut PsdReader::new(buffer, None, None);

    let signature = read_signature(reader)?; // ASEF
    if signature != "ASEF" {
        return Err(ReadError::StrictViolation("Invalid signature".to_string()));
    }
    let version_major = read_uint16(reader)?; // 1
    let version_minor = read_uint16(reader)?; // 0
    if version_major != 1 || version_minor != 0 {
        return Err(ReadError::StrictViolation("Invalid version".to_string()));
    }
    let blocks_count = read_uint32(reader)?;

    let mut ase = Ase { colors: Vec::new() };
    // `group` в upstream указывает либо в корень `ase`, либо в последнюю
    // открытую группу. Здесь — индекс открытой группы в `ase.colors` (None = корень).
    let mut current_group: Option<usize> = None;

    for _ in 0..blocks_count {
        let type_ = read_uint16(reader)?;
        let length = read_uint32(reader)? as usize;
        let end = reader.offset + length;

        match type_ {
            0x0001 => {
                // color
                let name_length = read_uint16(reader)? as usize;
                let name = read_unicode_string_with_length(reader, name_length)?;
                let color_mode = read_signature(reader)?;
                let color = match color_mode.as_str() {
                    "RGB " => AseColorValue::Rgb {
                        r: read_float32(reader)?,
                        g: read_float32(reader)?,
                        b: read_float32(reader)?,
                        type_: read_color_type(reader)?,
                    },
                    "CMYK" => AseColorValue::Cmyk {
                        c: read_float32(reader)?,
                        m: read_float32(reader)?,
                        y: read_float32(reader)?,
                        k: read_float32(reader)?,
                        type_: read_color_type(reader)?,
                    },
                    "Gray" => AseColorValue::Gray {
                        k: read_float32(reader)?,
                        type_: read_color_type(reader)?,
                    },
                    "LAB " => AseColorValue::Lab {
                        l: read_float32(reader)?,
                        a: read_float32(reader)?,
                        b: read_float32(reader)?,
                        type_: read_color_type(reader)?,
                    },
                    _ => {
                        return Err(ReadError::StrictViolation("Invalid color mode".to_string()))
                    }
                };
                let entry = AseColor { name, color };
                match current_group {
                    Some(gi) => {
                        if let AseEntry::Group(g) = &mut ase.colors[gi] {
                            g.colors.push(entry);
                        }
                    }
                    None => ase.colors.push(AseEntry::Color(entry)),
                }
            }
            0xC001 => {
                // group start
                let name_length = read_uint16(reader)? as usize;
                let name = read_unicode_string_with_length(reader, name_length)?;
                ase.colors.push(AseEntry::Group(AseGroup {
                    name,
                    colors: Vec::new(),
                }));
                current_group = Some(ase.colors.len() - 1);
            }
            0xC002 => {
                // group end
                current_group = None;
            }
            _ => return Err(ReadError::StrictViolation("Invalid block type".to_string())),
        }

        reader.offset = end;
    }

    Ok(ase)
}

fn read_color_type(reader: &mut PsdReader) -> ReadResult<AseColorType> {
    let index = read_uint16(reader)?;
    AseColorType::from_index(index)
        .ok_or_else(|| ReadError::StrictViolation(format!("Invalid color type: {}", index)))
}

/// Запись палитры в формат ASEF (симметрия `read_ase`; upstream-аналога нет).
///
/// Каждый блок пишется как `uint16 type` + `uint32 length` + payload длины
/// `length`; имена кодируются как `uint16 codeUnitCount` (включая завершающий 0)
/// и затем UTF-16 BE code units (как `readUnicodeStringWithLength`).
pub fn write_ase(ase: &Ase) -> Vec<u8> {
    let mut writer = create_writer(4096);

    write_signature(&mut writer, "ASEF");
    write_uint16(&mut writer, 1); // version major
    write_uint16(&mut writer, 0); // version minor

    // Считаем блоки: каждая группа = group-start + N цветов + group-end.
    let mut blocks_count: u32 = 0;
    for entry in &ase.colors {
        match entry {
            AseEntry::Color(_) => blocks_count += 1,
            AseEntry::Group(g) => blocks_count += 2 + g.colors.len() as u32,
        }
    }
    write_uint32(&mut writer, blocks_count);

    for entry in &ase.colors {
        match entry {
            AseEntry::Color(c) => write_color_block(&mut writer, c),
            AseEntry::Group(g) => {
                write_block(&mut writer, 0xC001, |w| write_name(w, &g.name));
                for c in &g.colors {
                    write_color_block(&mut writer, c);
                }
                write_block(&mut writer, 0xC002, |_| {});
            }
        }
    }

    get_writer_buffer(&writer)
}

fn write_name(writer: &mut PsdWriter, name: &str) {
    // длина в code units включает завершающий ноль (readUnicodeStringWithLength
    // отбрасывает хвостовой \0 на последней позиции).
    let units: Vec<u16> = name.encode_utf16().collect();
    write_uint16(writer, (units.len() + 1) as u16);
    for u in &units {
        write_uint16(writer, *u);
    }
    write_uint16(writer, 0); // trailing null
}

fn write_color_block(writer: &mut PsdWriter, c: &AseColor) {
    write_block(writer, 0x0001, |w| {
        write_name(w, &c.name);
        match &c.color {
            AseColorValue::Rgb { r, g, b, type_ } => {
                write_signature(w, "RGB ");
                write_float32(w, *r);
                write_float32(w, *g);
                write_float32(w, *b);
                write_uint16(w, type_.to_index());
            }
            AseColorValue::Cmyk { c, m, y, k, type_ } => {
                write_signature(w, "CMYK");
                write_float32(w, *c);
                write_float32(w, *m);
                write_float32(w, *y);
                write_float32(w, *k);
                write_uint16(w, type_.to_index());
            }
            AseColorValue::Gray { k, type_ } => {
                write_signature(w, "Gray");
                write_float32(w, *k);
                write_uint16(w, type_.to_index());
            }
            AseColorValue::Lab { l, a, b, type_ } => {
                write_signature(w, "LAB ");
                write_float32(w, *l);
                write_float32(w, *a);
                write_float32(w, *b);
                write_uint16(w, type_.to_index());
            }
        }
    });
}

/// Пишет блок `uint16 type` + `uint32 length` + payload, бэкпатчит длину.
fn write_block<F: FnOnce(&mut PsdWriter)>(writer: &mut PsdWriter, type_: u16, func: F) {
    write_uint16(writer, type_);
    let length_offset = writer.offset;
    write_uint32(writer, 0); // placeholder
    let start = writer.offset;
    func(writer);
    let length = (writer.offset - start) as u32;
    writer.buffer[length_offset..length_offset + 4].copy_from_slice(&length.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Ase {
        Ase {
            colors: vec![
                AseEntry::Color(AseColor {
                    name: "Red".to_string(),
                    color: AseColorValue::Rgb {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                        type_: AseColorType::Global,
                    },
                }),
                AseEntry::Group(AseGroup {
                    name: "Grays".to_string(),
                    colors: vec![
                        AseColor {
                            name: "Mid".to_string(),
                            color: AseColorValue::Gray {
                                k: 0.5,
                                type_: AseColorType::Normal,
                            },
                        },
                        AseColor {
                            name: "Cyanish".to_string(),
                            color: AseColorValue::Cmyk {
                                c: 1.0,
                                m: 0.0,
                                y: 0.0,
                                k: 0.0,
                                type_: AseColorType::Spot,
                            },
                        },
                    ],
                }),
                AseEntry::Color(AseColor {
                    name: "Lab".to_string(),
                    color: AseColorValue::Lab {
                        l: 50.0,
                        a: 10.0,
                        b: -20.0,
                        type_: AseColorType::Normal,
                    },
                }),
            ],
        }
    }

    #[test]
    fn ase_round_trip() {
        let ase = sample();
        let bytes = write_ase(&ase);
        let decoded = read_ase(&bytes).expect("read_ase");
        assert_eq!(ase, decoded);
    }

    /// A colour-type index outside `global`/`spot`/`normal` must be rejected
    /// rather than silently producing an entry without a type.
    #[test]
    fn ase_rejects_invalid_color_type() {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(b"ASEF");
        bytes.extend_from_slice(&1u16.to_be_bytes()); // version major
        bytes.extend_from_slice(&0u16.to_be_bytes()); // version minor
        bytes.extend_from_slice(&1u32.to_be_bytes()); // one block

        let mut block: Vec<u8> = Vec::new();
        block.extend_from_slice(&1u16.to_be_bytes()); // name length: just the terminator
        block.extend_from_slice(&0u16.to_be_bytes()); // trailing null
        block.extend_from_slice(b"Gray");
        block.extend_from_slice(&0.5f32.to_be_bytes());
        block.extend_from_slice(&3u16.to_be_bytes()); // colour type index out of range

        bytes.extend_from_slice(&0x0001u16.to_be_bytes()); // colour block
        bytes.extend_from_slice(&(block.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&block);

        assert!(read_ase(&bytes).is_err());
    }

    #[test]
    fn ase_rejects_bad_signature() {
        let bytes = b"XXXX\x00\x01\x00\x00\x00\x00\x00\x00";
        assert!(read_ase(bytes).is_err());
    }

    fn fixture(sub: &str) -> std::path::PathBuf {
        let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop();
        p.pop();
        p.push("test/ag-psd/test/ase-read");
        p.push(sub);
        p.push("src.ase");
        p
    }

    #[test]
    fn ase_decodes_photoshop_fixture() {
        let path = fixture("from-photoshop");
        if !path.exists() {
            eprintln!("ase fixture missing, skipping");
            return;
        }
        let data = std::fs::read(&path).unwrap();
        let ase = read_ase(&data).expect("decode ase fixture");
        assert!(!ase.colors.is_empty());
        // first entry is an RGB color "#FFCCCC" global
        match &ase.colors[0] {
            AseEntry::Color(c) => {
                assert_eq!(c.name, "#FFCCCC");
                match &c.color {
                    AseColorValue::Rgb { r, g, b, type_ } => {
                        assert_eq!(*r, 1.0);
                        // 0xCC/0xFF stored as f32 == 0.7999878 (13107/16384).
                        assert!((*g - 0.7999878).abs() < 1e-4);
                        assert!((*b - 0.7999878).abs() < 1e-4);
                        assert_eq!(*type_, AseColorType::Global);
                    }
                    other => panic!("expected rgb, got {:?}", other),
                }
            }
            other => panic!("expected color entry, got {:?}", other),
        }
    }

    #[test]
    fn ase_fixture_round_trip() {
        let path = fixture("piratetrousle-dusk");
        if !path.exists() {
            eprintln!("ase fixture missing, skipping");
            return;
        }
        let data = std::fs::read(&path).unwrap();
        let ase = read_ase(&data).expect("decode");
        // re-encode then decode again must match the decoded structure
        let bytes = write_ase(&ase);
        let again = read_ase(&bytes).expect("re-decode");
        assert_eq!(ase, again);
    }

    #[test]
    fn ase_smoke_header() {
        let bytes = write_ase(&Ase { colors: vec![] });
        // ASEF + version(1,0) + blocksCount(0)
        assert_eq!(&bytes[0..4], b"ASEF");
        let decoded = read_ase(&bytes).unwrap();
        assert!(decoded.colors.is_empty());
    }
}
