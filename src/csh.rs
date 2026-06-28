/*
File: crates/ag-psd/src/csh.rs

Purpose:
чтение custom shapes Photoshop (.csh).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/csh.ts`.
- upstream предоставляет ТОЛЬКО `readCsh` (read-only). `write_csh` добавлен здесь
  как симметричная функция (нужна для round-trip теста); upstream-аналога нет.
- `readVectorMask` / `readBezierKnot` / `booleanOperations` из `additionalInfo.ts`
  ещё не портированы в крейте — портированы локально здесь как `read_vector_mask`
  (DEPENDENCY GAP: должны переехать в `additional_info` при его портировании).

Main responsibilities:
- декодировать/кодировать секции 'cush' с векторными контурами фигур.
*/

use crate::psd::{BezierKnot, BezierPath, BooleanOperation, FillRule, LayerVectorMask};
use crate::reader::{
    check_signature, read_fixed_point_path32, read_int16, read_pascal_string, read_uint16,
    read_uint32, read_unicode_string, skip_bytes, PsdReader, ReadError, ReadResult,
};
use crate::writer::{
    create_writer, get_writer_buffer, write_fixed_point_path32, write_int16, write_pascal_string,
    write_uint16, write_uint32, write_unicode_string, write_zeros, PsdWriter,
};

/// Элемент `Csh.shapes`: `LayerVectorMask & { name, id, width, height }`.
#[derive(Debug, Clone, Default)]
pub struct CshShape {
    pub name: String,
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub mask: LayerVectorMask,
}

/// TS `Csh`.
#[derive(Debug, Clone, Default)]
pub struct Csh {
    pub shapes: Vec<CshShape>,
}

/// `booleanOperations: BooleanOperation[]` из additionalInfo.ts.
fn boolean_operation(index: i16) -> Option<BooleanOperation> {
    match index {
        0 => Some(BooleanOperation::Exclude),
        1 => Some(BooleanOperation::Combine),
        2 => Some(BooleanOperation::Subtract),
        3 => Some(BooleanOperation::Intersect),
        _ => None,
    }
}

/// Порт `readBezierKnot(reader, width, height)`.
fn read_bezier_knot(reader: &mut PsdReader, width: f64, height: f64) -> ReadResult<Vec<f64>> {
    let y0 = read_fixed_point_path32(reader)? * height;
    let x0 = read_fixed_point_path32(reader)? * width;
    let y1 = read_fixed_point_path32(reader)? * height;
    let x1 = read_fixed_point_path32(reader)? * width;
    let y2 = read_fixed_point_path32(reader)? * height;
    let x2 = read_fixed_point_path32(reader)? * width;
    Ok(vec![x0, y0, x1, y1, x2, y2])
}

/// Порт `readVectorMask(reader, vectorMask, width, height, size)`.
///
/// DEPENDENCY GAP: должна жить в `additional_info` (ещё не портирован).
fn read_vector_mask(
    reader: &mut PsdReader,
    vector_mask: &mut LayerVectorMask,
    width: f64,
    height: f64,
    size: usize,
) -> ReadResult<()> {
    let end = reader.offset + size;
    let mut current: Option<usize> = None; // индекс активного path в vector_mask.paths

    while end.saturating_sub(reader.offset) >= 26 {
        let selector = read_uint16(reader)?;

        match selector {
            0 | 3 => {
                // Closed (0) / Open (3) subpath length record
                let _count = read_uint16(reader)?;
                let bool_op = read_int16(reader)?;
                let flags = read_uint16(reader)?; // bit 1 always 1 ?
                skip_bytes(reader, 18);
                let mut path = BezierPath {
                    open: selector == 3,
                    operation: None,
                    knots: Vec::new(),
                    fill_rule: if flags == 2 {
                        FillRule::NonZero
                    } else {
                        FillRule::EvenOdd
                    },
                };
                if bool_op != -1 {
                    path.operation = boolean_operation(bool_op);
                }
                vector_mask.paths.push(path);
                current = Some(vector_mask.paths.len() - 1);
            }
            1 | 2 | 4 | 5 => {
                // Bezier knot, linked (1/4) or unlinked (2/5)
                let points = read_bezier_knot(reader, width, height)?;
                let idx = current
                    .ok_or_else(|| ReadError::StrictViolation("Invalid vmsk section".to_string()))?;
                vector_mask.paths[idx].knots.push(BezierKnot {
                    linked: selector == 1 || selector == 4,
                    points,
                });
            }
            6 => {
                // Path fill rule record
                skip_bytes(reader, 24);
            }
            7 => {
                // Clipboard record
                let top = read_fixed_point_path32(reader)?;
                let left = read_fixed_point_path32(reader)?;
                let bottom = read_fixed_point_path32(reader)?;
                let right = read_fixed_point_path32(reader)?;
                let resolution = read_fixed_point_path32(reader)?;
                skip_bytes(reader, 4);
                vector_mask.clipboard = Some(crate::psd::VectorMaskClipboard {
                    top,
                    left,
                    bottom,
                    right,
                    resolution,
                });
            }
            8 => {
                // Initial fill rule record
                vector_mask.fill_starts_with_all_pixels = Some(read_uint16(reader)? != 0);
                skip_bytes(reader, 22);
            }
            _ => return Err(ReadError::StrictViolation("Invalid vmsk section".to_string())),
        }
    }

    Ok(())
}

/// Порт `readCsh(buffer)`.
pub fn read_csh(buffer: &[u8]) -> ReadResult<Csh> {
    let reader = &mut PsdReader::new(buffer, None, None);
    let mut csh = Csh { shapes: Vec::new() };

    check_signature(reader, "cush", None)?;
    if read_uint32(reader)? != 2 {
        return Err(ReadError::StrictViolation("Invalid version".to_string()));
    }
    let count = read_uint32(reader)?;

    for _ in 0..count {
        let name = read_unicode_string(reader)?;
        while reader.offset % 4 != 0 {
            reader.offset += 1; // pad to 4byte bounds
        }
        if read_uint32(reader)? != 1 {
            return Err(ReadError::StrictViolation("Invalid shape version".to_string()));
        }
        let size = read_uint32(reader)? as usize;
        let end = reader.offset + size;
        let id = read_pascal_string(reader, 1)?;
        // this might not be correct ???
        let y1 = read_uint32(reader)?;
        let x1 = read_uint32(reader)?;
        let y2 = read_uint32(reader)?;
        let x2 = read_uint32(reader)?;
        let width = x2 - x1;
        let height = y2 - y1;
        let mut mask = LayerVectorMask::default();
        read_vector_mask(
            reader,
            &mut mask,
            width as f64,
            height as f64,
            end - reader.offset,
        )?;
        csh.shapes.push(CshShape {
            name,
            id,
            width,
            height,
            mask,
        });

        reader.offset = end;
    }

    Ok(csh)
}

// ===========================================================================
// Writer (симметрия read_csh; upstream-аналога нет)
// ===========================================================================

fn boolean_operation_index(op: BooleanOperation) -> i16 {
    match op {
        BooleanOperation::Exclude => 0,
        BooleanOperation::Combine => 1,
        BooleanOperation::Subtract => 2,
        BooleanOperation::Intersect => 3,
    }
}

fn write_bezier_knot(writer: &mut PsdWriter, points: &[f64], width: f64, height: f64) {
    // обратный порядок read_bezier_knot: x делится на width, y — на height.
    let safe = |v: f64, d: f64| if d != 0.0 { v / d } else { 0.0 };
    write_fixed_point_path32(writer, safe(points[1], height)); // y0
    write_fixed_point_path32(writer, safe(points[0], width)); // x0
    write_fixed_point_path32(writer, safe(points[3], height)); // y1
    write_fixed_point_path32(writer, safe(points[2], width)); // x1
    write_fixed_point_path32(writer, safe(points[5], height)); // y2
    write_fixed_point_path32(writer, safe(points[4], width)); // x2
}

fn write_vector_mask(writer: &mut PsdWriter, mask: &LayerVectorMask, width: f64, height: f64) {
    // initial fill rule record (selector 8) если задано
    if let Some(fill) = mask.fill_starts_with_all_pixels {
        write_uint16(writer, 8);
        write_uint16(writer, if fill { 1 } else { 0 });
        write_zeros(writer, 22);
    }

    if let Some(clip) = &mask.clipboard {
        write_uint16(writer, 7);
        write_fixed_point_path32(writer, clip.top);
        write_fixed_point_path32(writer, clip.left);
        write_fixed_point_path32(writer, clip.bottom);
        write_fixed_point_path32(writer, clip.right);
        write_fixed_point_path32(writer, clip.resolution);
        write_zeros(writer, 4);
    }

    for path in &mask.paths {
        // subpath length record (selector 0 closed / 3 open)
        write_uint16(writer, if path.open { 3 } else { 0 });
        write_uint16(writer, path.knots.len() as u16); // count
        write_int16(
            writer,
            path.operation.map(boolean_operation_index).unwrap_or(-1),
        );
        write_uint16(
            writer,
            match path.fill_rule {
                FillRule::NonZero => 2,
                FillRule::EvenOdd => 0,
            },
        );
        write_zeros(writer, 18);

        for knot in &path.knots {
            // 1 closed-linked / 2 closed-unlinked / 4 open-linked / 5 open-unlinked
            let selector = match (path.open, knot.linked) {
                (false, true) => 1,
                (false, false) => 2,
                (true, true) => 4,
                (true, false) => 5,
            };
            write_uint16(writer, selector);
            write_bezier_knot(writer, &knot.points, width, height);
        }
    }
}

/// Запись custom shapes (симметрия `read_csh`; upstream-аналога нет).
pub fn write_csh(csh: &Csh) -> Vec<u8> {
    let mut writer = create_writer(4096);

    for b in b"cush" {
        crate::writer::write_uint8(&mut writer, *b);
    }
    write_uint32(&mut writer, 2); // version
    write_uint32(&mut writer, csh.shapes.len() as u32);

    for shape in &csh.shapes {
        write_unicode_string(&mut writer, &shape.name);
        while writer.offset % 4 != 0 {
            crate::writer::write_uint8(&mut writer, 0);
        }
        write_uint32(&mut writer, 1); // shape version

        // size placeholder, бэкпатчим после записи тела
        let size_offset = writer.offset;
        write_uint32(&mut writer, 0);
        let body_start = writer.offset;

        write_pascal_string(&mut writer, &shape.id, 1);
        // bounds y1,x1,y2,x2 (предполагаем origin 0,0)
        write_uint32(&mut writer, 0); // y1
        write_uint32(&mut writer, 0); // x1
        write_uint32(&mut writer, shape.height); // y2
        write_uint32(&mut writer, shape.width); // x2

        write_vector_mask(
            &mut writer,
            &shape.mask,
            shape.width as f64,
            shape.height as f64,
        );

        let size = (writer.offset - body_start) as u32;
        writer.buffer[size_offset..size_offset + 4].copy_from_slice(&size.to_be_bytes());
    }

    get_writer_buffer(&writer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-3
    }

    fn sample() -> Csh {
        let mut mask = LayerVectorMask::default();
        mask.paths.push(BezierPath {
            open: false,
            operation: Some(BooleanOperation::Combine),
            knots: vec![
                BezierKnot {
                    linked: true,
                    points: vec![10.0, 20.0, 11.0, 21.0, 12.0, 22.0],
                },
                BezierKnot {
                    linked: false,
                    points: vec![30.0, 40.0, 31.0, 41.0, 32.0, 42.0],
                },
            ],
            fill_rule: FillRule::NonZero,
        });

        Csh {
            shapes: vec![CshShape {
                name: "Square".to_string(),
                id: "abc123".to_string(),
                width: 100,
                height: 80,
                mask,
            }],
        }
    }

    #[test]
    fn csh_round_trip() {
        let csh = sample();
        let bytes = write_csh(&csh);
        let decoded = read_csh(&bytes).expect("read_csh");

        assert_eq!(decoded.shapes.len(), 1);
        let s = &decoded.shapes[0];
        assert_eq!(s.name, "Square");
        assert_eq!(s.id, "abc123");
        assert_eq!(s.width, 100);
        assert_eq!(s.height, 80);
        assert_eq!(s.mask.paths.len(), 1);

        let p = &s.mask.paths[0];
        assert!(!p.open);
        assert_eq!(p.operation, Some(BooleanOperation::Combine));
        assert_eq!(p.fill_rule, FillRule::NonZero);
        assert_eq!(p.knots.len(), 2);
        assert!(p.knots[0].linked);
        assert!(!p.knots[1].linked);

        let orig = &csh.shapes[0].mask.paths[0].knots[0].points;
        let got = &p.knots[0].points;
        for i in 0..6 {
            assert!(approx_eq(orig[i], got[i]), "knot point {} mismatch", i);
        }
    }

    #[test]
    fn csh_decodes_fixture() {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.pop();
        path.pop();
        path.push("test/ag-psd/test/csh-read/animals/src.csh");
        if !path.exists() {
            eprintln!("csh fixture missing, skipping");
            return;
        }
        let data = std::fs::read(&path).unwrap();
        let csh = read_csh(&data).expect("decode csh fixture");
        assert!(!csh.shapes.is_empty());
        let first = &csh.shapes[0];
        assert_eq!(first.name, "Bone");
        assert_eq!(first.id, "26a9b56b-d040-11d5-a39c-fd27718ef272");
        assert_eq!(first.width, 194);
        assert_eq!(first.height, 90);
        assert!(!first.mask.paths.is_empty());
        let p = &first.mask.paths[0];
        assert!(!p.open);
        assert_eq!(p.operation, Some(BooleanOperation::Combine));
        assert!(!p.knots.is_empty());
        // first knot first point ~57.249 (x0)
        assert!((p.knots[0].points[0] - 57.249).abs() < 0.1);
    }

    #[test]
    fn csh_rejects_bad_signature() {
        let bytes = b"XXXX\x00\x00\x00\x02\x00\x00\x00\x00";
        assert!(read_csh(bytes).is_err());
    }
}
