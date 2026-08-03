/*
File: crates/ag-psd/src/jpeg.rs

Purpose:
работа с JPEG-данными, встроенными в PSD (декодирование thumbnail-ресурсов).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/jpeg.ts` (разбиение 1:1).
  Сам `jpeg.ts` основан на https://github.com/jpeg-js/jpeg-js.

Main responsibilities:
- baseline + progressive JPEG decoder: разбор маркеров, таблицы квантования,
  таблицы Хаффмана, обратное DCT, апсемплинг компонент, YCbCr->RGB;
- результат — `PixelData` (RGBA8), как `decodeJpeg` в TS возвращает ImageData.

PORT NOTES:
- Экспортируется только декодер (`decode_jpeg`); кодировщика в `jpeg.ts` нет.
  Запись thumbnail остаётся raw-passthrough в image_resources.rs.
- camelCase -> snake_case; функции и таблицы публичные по необходимости.
*/

#![allow(clippy::needless_range_loop)]

use crate::psd::PixelData;

// based on https://github.com/jpeg-js/jpeg-js
/*
   Copyright 2011 notmasteryet

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

pub type JpegResult<T> = Result<T, String>;

pub static DCT_ZIG_ZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const DCT_COS1: i32 = 4017; // cos(pi/16)
const DCT_SIN1: i32 = 799; // sin(pi/16)
const DCT_COS3: i32 = 3406; // cos(3*pi/16)
const DCT_SIN3: i32 = 2276; // sin(3*pi/16)
const DCT_COS6: i32 = 1567; // cos(6*pi/16)
const DCT_SIN6: i32 = 3784; // sin(6*pi/16)
const DCT_SQRT2: i32 = 5793; // sqrt(2)
const DCT_SQRT1D2: i32 = 2896; // sqrt(2) / 2

const MAX_RESOLUTION_IN_MP: usize = 100; // Don't decode more than 100 megapixels
const MAX_MEMORY_USAGE_BYTES: usize = 64 * 1024 * 1024; // 64MB cap on untrusted content

// ===========================================================================
// Memory guard (mirrors requestMemoryAllocation / totalBytesAllocated module state)
// ===========================================================================

struct MemoryGuard {
    total_bytes_allocated: usize,
}

impl MemoryGuard {
    fn new() -> Self {
        MemoryGuard {
            total_bytes_allocated: 0,
        }
    }

    fn request(&mut self, increase_amount: usize) -> JpegResult<()> {
        let total = self.total_bytes_allocated + increase_amount;
        if total > MAX_MEMORY_USAGE_BYTES {
            let exceeded = (total - MAX_MEMORY_USAGE_BYTES).div_ceil(1024 * 1024);
            return Err(format!("Max memory limit exceeded by at least {exceeded}MB"));
        }
        self.total_bytes_allocated = total;
        Ok(())
    }
}

// ===========================================================================
// Huffman table: tree of nodes; mirrors the nested-array structure of the TS port.
// ===========================================================================

/// A Huffman tree node: either a leaf value, or an index into the arena of branches.
#[derive(Clone, Copy, Debug)]
enum HuffNode {
    Empty,
    Value(i32),
    Branch(usize), // index into HuffmanTable.nodes
}

#[derive(Clone, Debug, Default)]
struct HuffmanTable {
    /// Each entry is a 2-element children array `[child0, child1]` in arena form.
    nodes: Vec<[HuffNode; 2]>,
}

impl HuffmanTable {
    fn root(&self) -> usize {
        0
    }
}

/// Port of `buildHuffmanTable`.
///
/// The TS implementation builds a nested-array tree where interior nodes are
/// `number[]` and leaves are `number`. We replicate the exact construction
/// algorithm using an arena of branch nodes.
fn build_huffman_table(code_lengths: &[u8; 16], values: &[u8]) -> HuffmanTable {
    let mut length: usize = 16;
    while length > 0 && code_lengths[length - 1] == 0 {
        length -= 1;
    }

    // Arena of branch nodes. Node 0 is the root (code[0].children in TS).
    // `code` is a stack of (node_index, next_child_index) pairs.
    let mut table = HuffmanTable { nodes: vec![[HuffNode::Empty, HuffNode::Empty]] };

    // Stack mirrors TS `code: Code[]`, each Code = { node_index, index }.
    let mut stack: Vec<(usize, usize)> = vec![(0, 0)];
    let mut k = 0usize;

    let new_branch = |table: &mut HuffmanTable| -> usize {
        table.nodes.push([HuffNode::Empty, HuffNode::Empty]);
        table.nodes.len() - 1
    };

    for i in 0..length {
        for _ in 0..code_lengths[i] {
            // p = code.pop()
            let mut p = stack.pop().expect("huffman stack underflow");
            // p.children[p.index] = values[k]
            table.nodes[p.0][p.1] = HuffNode::Value(values[k] as i32);
            // while (p.index > 0) { ... p = code.pop() }
            while p.1 > 0 {
                p = stack.pop().expect("Could not recreate Huffman Table");
            }
            // p.index++
            p.1 += 1;
            // code.push(p)
            stack.push(p);
            // while (code.length <= i) { q = new; code.push(q); p.children[p.index] = q.children; p = q; }
            while stack.len() <= i {
                let q = new_branch(&mut table);
                stack.push((q, 0));
                // p.children[p.index] = q.children  → branch link
                let p_ref = stack[stack.len() - 2];
                table.nodes[p_ref.0][p_ref.1] = HuffNode::Branch(q);
            }
            k += 1;
        }
        if i + 1 < length {
            // p here points to last code: p = code[code.length-1]
            let q = new_branch(&mut table);
            let last = stack.len() - 1;
            let p_ref = stack[last];
            table.nodes[p_ref.0][p_ref.1] = HuffNode::Branch(q);
            stack.push((q, 0));
        }
    }

    table
}

// ===========================================================================
// Component / Frame data
// ===========================================================================

#[derive(Clone)]
struct Component {
    h: usize,
    v: usize,
    blocks_per_line: usize,
    blocks_per_column: usize,
    /// blocks[blockRow][blockCol] = [i32; 64]
    blocks: Vec<Vec<[i32; 64]>>,
    pred: i32,
    quantization_idx: usize,
    quantization_table: Option<[i32; 64]>,
    huffman_table_dc: Option<usize>, // index into Frame's DC tables snapshot
    huffman_table_ac: Option<usize>,
}

struct Frame {
    progressive: bool,
    scan_lines: usize,
    samples_per_line: usize,
    /// components keyed by componentId (1..=255). Stored sparse via Vec<Option<>>.
    components: Vec<Option<Component>>,
    components_order: Vec<usize>,
    max_h: usize,
    max_v: usize,
    mcus_per_line: usize,
    mcus_per_column: usize,
}

struct DecodedComponent {
    lines: Vec<Vec<u8>>,
    scale_x: f64,
    scale_y: f64,
}

struct Decoded {
    width: usize,
    height: usize,
    adobe_transform_code: Option<u8>,
    components: Vec<DecodedComponent>,
}

// ===========================================================================
// Scan decoding (port of decodeScan)
// ===========================================================================

struct BitReader<'a> {
    data: &'a [u8],
    offset: usize,
    bits_data: u32,
    bits_count: i32,
}

impl<'a> BitReader<'a> {
    fn read_bit(&mut self) -> JpegResult<Option<u32>> {
        if self.bits_count > 0 {
            self.bits_count -= 1;
            return Ok(Some((self.bits_data >> self.bits_count) & 1));
        }

        if self.offset >= self.data.len() {
            // TS would read `undefined`; treat as end-of-data (None).
            return Ok(None);
        }
        self.bits_data = self.data[self.offset] as u32;
        self.offset += 1;

        if self.bits_data == 0xFF {
            let next_byte = if self.offset < self.data.len() {
                let b = self.data[self.offset];
                self.offset += 1;
                b
            } else {
                0
            };
            if next_byte != 0 {
                return Err(format!(
                    "unexpected marker: {:x}",
                    (self.bits_data << 8) | next_byte as u32
                ));
            }
            // unstuff 0
        }

        self.bits_count = 7;
        Ok(Some(self.bits_data >> 7))
    }
}

/// Tables snapshot passed to the scan: AC/DC huffman tables indexed by selector.
struct HuffmanTables<'a> {
    dc: &'a [Option<HuffmanTable>],
    ac: &'a [Option<HuffmanTable>],
}

#[derive(Clone, Copy, PartialEq)]
enum DecodeMode {
    Baseline,
    DcFirst,
    DcSuccessive,
    AcFirst,
    AcSuccessive,
}

#[allow(clippy::too_many_arguments)]
fn decode_scan(
    data: &[u8],
    offset: usize,
    frame: &mut Frame,
    component_ids: &[usize],
    tables: &HuffmanTables,
    mut reset_interval: usize,
    spectral_start: usize,
    spectral_end: usize,
    successive_prev: i32,
    successive: i32,
) -> JpegResult<usize> {
    let mcus_per_line = frame.mcus_per_line;
    let mcus_per_column = frame.mcus_per_column;
    let progressive = frame.progressive;
    let start_offset = offset;

    let mut reader = BitReader {
        data,
        offset,
        bits_data: 0,
        bits_count: 0,
    };

    // --- bit helpers ---
    fn decode_huffman(reader: &mut BitReader, table: &HuffmanTable) -> JpegResult<i32> {
        let mut node_idx = table.root();
        loop {
            let bit = match reader.read_bit()? {
                Some(b) => b as usize,
                None => return Err("invalid huffman sequence".to_string()),
            };
            match table.nodes[node_idx][bit] {
                HuffNode::Value(v) => return Ok(v),
                HuffNode::Branch(n) => node_idx = n,
                HuffNode::Empty => return Err("invalid huffman sequence".to_string()),
            }
        }
    }

    fn receive(reader: &mut BitReader, mut length: i32) -> JpegResult<i32> {
        let mut n: i32 = 0;
        while length > 0 {
            let bit = reader.read_bit()?.unwrap_or(0);
            n = (n << 1) | bit as i32;
            length -= 1;
        }
        Ok(n)
    }

    fn receive_and_extend(reader: &mut BitReader, length: i32) -> JpegResult<i32> {
        let n = receive(reader, length)?;
        if n >= (1 << (length - 1)) {
            Ok(n)
        } else {
            Ok(n + (-1 << length) + 1)
        }
    }

    // Progressive-state machine variables (shared across blocks).
    // (Reset at the top of each reset-interval below, mirroring the TS code.)
    #[allow(unused_assignments)]
    let mut eobrun: i32 = 0;
    #[allow(unused_assignments)]
    let mut successive_ac_state: i32 = 0;
    let mut successive_ac_next_value: i32 = 0;

    // Decode a single block (zz coefficients) according to mode.
    // Returns possibly updated `pred`.
    let decode_block_coeffs = |reader: &mut BitReader,
                               mode: DecodeMode,
                               zz: &mut [i32; 64],
                               pred: &mut i32,
                               dc: &Option<HuffmanTable>,
                               ac: &Option<HuffmanTable>,
                               eobrun: &mut i32,
                               sa_state: &mut i32,
                               sa_next: &mut i32|
     -> JpegResult<()> {
        match mode {
            DecodeMode::Baseline => {
                let dc = dc.as_ref().ok_or("missing DC table")?;
                let t = decode_huffman(reader, dc)?;
                let diff = if t == 0 { 0 } else { receive_and_extend(reader, t)? };
                *pred += diff;
                zz[0] = *pred;
                let mut k = 1usize;
                while k < 64 {
                    let ac = ac.as_ref().ok_or("missing AC table")?;
                    let rs = decode_huffman(reader, ac)?;
                    let s = rs & 15;
                    let r = rs >> 4;
                    if s == 0 {
                        if r < 15 {
                            break;
                        }
                        k += 16;
                        continue;
                    }
                    k += r as usize;
                    let z = DCT_ZIG_ZAG[k];
                    zz[z] = receive_and_extend(reader, s)?;
                    k += 1;
                }
            }
            DecodeMode::DcFirst => {
                let dc = dc.as_ref().ok_or("missing DC table")?;
                let t = decode_huffman(reader, dc)?;
                let diff = if t == 0 {
                    0
                } else {
                    receive_and_extend(reader, t)? << successive
                };
                *pred += diff;
                zz[0] = *pred;
            }
            DecodeMode::DcSuccessive => {
                let bit = reader.read_bit()?.unwrap_or(0) as i32;
                zz[0] |= bit << successive;
            }
            DecodeMode::AcFirst => {
                if *eobrun > 0 {
                    *eobrun -= 1;
                    return Ok(());
                }
                let mut k = spectral_start;
                let e = spectral_end;
                while k <= e {
                    let ac = ac.as_ref().ok_or("missing AC table")?;
                    let rs = decode_huffman(reader, ac)?;
                    let s = rs & 15;
                    let r = rs >> 4;
                    if s == 0 {
                        if r < 15 {
                            *eobrun = receive(reader, r)? + (1 << r) - 1;
                            break;
                        }
                        k += 16;
                        continue;
                    }
                    k += r as usize;
                    let z = DCT_ZIG_ZAG[k];
                    zz[z] = receive_and_extend(reader, s)? * (1 << successive);
                    k += 1;
                }
            }
            DecodeMode::AcSuccessive => {
                let mut k = spectral_start;
                let e = spectral_end;
                let mut r = 0i32;
                while k <= e {
                    let z = DCT_ZIG_ZAG[k];
                    let direction = if zz[z] < 0 { -1 } else { 1 };

                    match *sa_state {
                        0 => {
                            let ac = ac.as_ref().ok_or("missing AC table")?;
                            let rs = decode_huffman(reader, ac)?;
                            let s = rs & 15;
                            r = rs >> 4;
                            if s == 0 {
                                if r < 15 {
                                    *eobrun = receive(reader, r)? + (1 << r);
                                    *sa_state = 4;
                                } else {
                                    r = 16;
                                    *sa_state = 1;
                                }
                            } else {
                                if s != 1 {
                                    return Err("invalid ACn encoding".to_string());
                                }
                                *sa_next = receive_and_extend(reader, s)?;
                                *sa_state = if r != 0 { 2 } else { 3 };
                            }
                            continue; // does not advance k
                        }
                        1 | 2 => {
                            if zz[z] != 0 {
                                let bit = reader.read_bit()?.unwrap_or(0) as i32;
                                zz[z] += (bit << successive) * direction;
                            } else {
                                r -= 1;
                                if r == 0 {
                                    *sa_state = if *sa_state == 2 { 3 } else { 0 };
                                }
                            }
                        }
                        3 => {
                            if zz[z] != 0 {
                                let bit = reader.read_bit()?.unwrap_or(0) as i32;
                                zz[z] += (bit << successive) * direction;
                            } else {
                                zz[z] = *sa_next << successive;
                                *sa_state = 0;
                            }
                        }
                        // Deliberately a nested `if` rather than a match guard: states
                        // 1|2, 3 and 4 all open with the same `if zz[z] != 0` test, and
                        // that symmetry mirrors upstream's `switch (successiveACState)`
                        // in jpeg.ts. Collapsing only this arm into `4 if zz[z] != 0`
                        // would hide the shared shape (semantics are identical because
                        // the trailing `_ => {}` arm is a no-op).
                        #[allow(clippy::collapsible_match)]
                        4 => {
                            if zz[z] != 0 {
                                let bit = reader.read_bit()?.unwrap_or(0) as i32;
                                zz[z] += (bit << successive) * direction;
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }

                if *sa_state == 4 {
                    *eobrun -= 1;
                    if *eobrun == 0 {
                        *sa_state = 0;
                    }
                }
            }
        }
        Ok(())
    };

    let mode = if progressive {
        if spectral_start == 0 {
            if successive_prev == 0 {
                DecodeMode::DcFirst
            } else {
                DecodeMode::DcSuccessive
            }
        } else if successive_prev == 0 {
            DecodeMode::AcFirst
        } else {
            DecodeMode::AcSuccessive
        }
    } else {
        DecodeMode::Baseline
    };

    let components_length = component_ids.len();

    let mcu_expected = if components_length == 1 {
        let c = frame.components[component_ids[0]].as_ref().unwrap();
        c.blocks_per_line * c.blocks_per_column
    } else {
        mcus_per_line * mcus_per_column
    };

    if reset_interval == 0 {
        reset_interval = mcu_expected;
    }

    let mut mcu = 0usize;

    while mcu < mcu_expected {
        // reset interval stuff
        for &cid in component_ids {
            frame.components[cid].as_mut().unwrap().pred = 0;
        }
        eobrun = 0;
        successive_ac_state = 0;

        if components_length == 1 {
            let cid = component_ids[0];
            for _ in 0..reset_interval {
                if mcu >= mcu_expected {
                    break;
                }
                // decodeBlock
                let (blocks_per_line, dc, ac) = {
                    let c = frame.components[cid].as_ref().unwrap();
                    (c.blocks_per_line, c.huffman_table_dc, c.huffman_table_ac)
                };
                let block_row = mcu / blocks_per_line;
                let block_col = mcu % blocks_per_line;
                let dc_tbl = dc.and_then(|i| tables.dc[i].clone());
                let ac_tbl = ac.and_then(|i| tables.ac[i].clone());
                let mut pred = frame.components[cid].as_ref().unwrap().pred;
                let component = frame.components[cid].as_mut().unwrap();
                if block_row < component.blocks.len() {
                    let mut zz = component.blocks[block_row][block_col];
                    decode_block_coeffs(
                        &mut reader,
                        mode,
                        &mut zz,
                        &mut pred,
                        &dc_tbl,
                        &ac_tbl,
                        &mut eobrun,
                        &mut successive_ac_state,
                        &mut successive_ac_next_value,
                    )?;
                    component.blocks[block_row][block_col] = zz;
                    component.pred = pred;
                }
                mcu += 1;
            }
        } else {
            for _ in 0..reset_interval {
                for &cid in component_ids {
                    let (h, v, dc, ac) = {
                        let c = frame.components[cid].as_ref().unwrap();
                        (c.h, c.v, c.huffman_table_dc, c.huffman_table_ac)
                    };
                    let dc_tbl = dc.and_then(|i| tables.dc[i].clone());
                    let ac_tbl = ac.and_then(|i| tables.ac[i].clone());
                    let mcu_row = mcu / mcus_per_line;
                    let mcu_col = mcu % mcus_per_line;
                    for j in 0..v {
                        for k in 0..h {
                            let block_row = mcu_row * v + j;
                            let block_col = mcu_col * h + k;
                            let mut pred = frame.components[cid].as_ref().unwrap().pred;
                            let component = frame.components[cid].as_mut().unwrap();
                            if block_row >= component.blocks.len() {
                                continue;
                            }
                            let mut zz = component.blocks[block_row][block_col];
                            decode_block_coeffs(
                                &mut reader,
                                mode,
                                &mut zz,
                                &mut pred,
                                &dc_tbl,
                                &ac_tbl,
                                &mut eobrun,
                                &mut successive_ac_state,
                                &mut successive_ac_next_value,
                            )?;
                            component.blocks[block_row][block_col] = zz;
                            component.pred = pred;
                        }
                    }
                }
                mcu += 1;
                if mcu == mcu_expected {
                    break;
                }
            }
        }

        if mcu == mcu_expected {
            // Skip trailing bytes at the end of the scan - until the next marker.
            while reader.offset < reader.data.len().saturating_sub(2) {
                if reader.data[reader.offset] == 0xFF && reader.data[reader.offset + 1] != 0x00 {
                    break;
                }
                reader.offset += 1;
            }
        }

        // find marker
        reader.bits_count = 0;
        let marker = if reader.offset + 1 < reader.data.len() {
            ((reader.data[reader.offset] as u32) << 8) | reader.data[reader.offset + 1] as u32
        } else {
            0
        };

        if marker < 0xFF00 {
            return Err("marker was not found".to_string());
        }

        if (0xFFD0..=0xFFD7).contains(&marker) {
            // RSTx
            reader.offset += 2;
        } else {
            break;
        }
    }

    Ok(reader.offset - start_offset)
}

// ===========================================================================
// IDCT + component data build (port of buildComponentData / quantizeAndInverse)
// ===========================================================================

fn clamp8(sample: i32) -> u8 {
    if sample < 0 {
        0
    } else if sample > 0xFF {
        0xFF
    } else {
        sample as u8
    }
}

fn quantize_and_inverse(zz: &[i32; 64], quant: &[i32; 64], data_out: &mut [u8; 64]) {
    let mut p = [0i32; 64];

    // dequant
    for i in 0..64 {
        p[i] = zz[i] * quant[i];
    }

    // inverse DCT on rows
    for i in 0..8 {
        let row = 8 * i;

        if p[1 + row] == 0
            && p[2 + row] == 0
            && p[3 + row] == 0
            && p[4 + row] == 0
            && p[5 + row] == 0
            && p[6 + row] == 0
            && p[7 + row] == 0
        {
            let t = (DCT_SQRT2 * p[row] + 512) >> 10;
            for j in 0..8 {
                p[j + row] = t;
            }
            continue;
        }

        // stage 4
        let mut v0 = (DCT_SQRT2 * p[row] + 128) >> 8;
        let mut v1 = (DCT_SQRT2 * p[4 + row] + 128) >> 8;
        let mut v2 = p[2 + row];
        let mut v3 = p[6 + row];
        let mut v4 = (DCT_SQRT1D2 * (p[1 + row] - p[7 + row]) + 128) >> 8;
        let mut v7 = (DCT_SQRT1D2 * (p[1 + row] + p[7 + row]) + 128) >> 8;
        let mut v5 = p[3 + row] << 4;
        let mut v6 = p[5 + row] << 4;

        // stage 3
        let mut t = (v0 - v1 + 1) >> 1;
        v0 = (v0 + v1 + 1) >> 1;
        v1 = t;
        t = (v2 * DCT_SIN6 + v3 * DCT_COS6 + 128) >> 8;
        v2 = (v2 * DCT_COS6 - v3 * DCT_SIN6 + 128) >> 8;
        v3 = t;
        t = (v4 - v6 + 1) >> 1;
        v4 = (v4 + v6 + 1) >> 1;
        v6 = t;
        t = (v7 + v5 + 1) >> 1;
        v5 = (v7 - v5 + 1) >> 1;
        v7 = t;

        // stage 2
        t = (v0 - v3 + 1) >> 1;
        v0 = (v0 + v3 + 1) >> 1;
        v3 = t;
        t = (v1 - v2 + 1) >> 1;
        v1 = (v1 + v2 + 1) >> 1;
        v2 = t;
        t = (v4 * DCT_SIN3 + v7 * DCT_COS3 + 2048) >> 12;
        v4 = (v4 * DCT_COS3 - v7 * DCT_SIN3 + 2048) >> 12;
        v7 = t;
        t = (v5 * DCT_SIN1 + v6 * DCT_COS1 + 2048) >> 12;
        v5 = (v5 * DCT_COS1 - v6 * DCT_SIN1 + 2048) >> 12;
        v6 = t;

        // stage 1
        p[row] = v0 + v7;
        p[7 + row] = v0 - v7;
        p[1 + row] = v1 + v6;
        p[6 + row] = v1 - v6;
        p[2 + row] = v2 + v5;
        p[5 + row] = v2 - v5;
        p[3 + row] = v3 + v4;
        p[4 + row] = v3 - v4;
    }

    // inverse DCT on columns
    for i in 0..8 {
        let col = i;

        if p[8 + col] == 0
            && p[16 + col] == 0
            && p[24 + col] == 0
            && p[32 + col] == 0
            && p[40 + col] == 0
            && p[48 + col] == 0
            && p[56 + col] == 0
        {
            // NOTE: the TS uses dataIn[i + 0] here (the original i-th element),
            // which at this point equals p[col] (already overwritten by row pass,
            // but col == i so p[col] == p[i]). Match TS exactly: use p[i].
            let t = (DCT_SQRT2 * p[i] + 8192) >> 14;
            p[col] = t;
            p[8 + col] = t;
            p[16 + col] = t;
            p[24 + col] = t;
            p[32 + col] = t;
            p[40 + col] = t;
            p[48 + col] = t;
            p[56 + col] = t;
            continue;
        }

        // stage 4
        let mut v0 = (DCT_SQRT2 * p[col] + 2048) >> 12;
        let mut v1 = (DCT_SQRT2 * p[32 + col] + 2048) >> 12;
        let mut v2 = p[16 + col];
        let mut v3 = p[48 + col];
        let mut v4 = (DCT_SQRT1D2 * (p[8 + col] - p[56 + col]) + 2048) >> 12;
        let mut v7 = (DCT_SQRT1D2 * (p[8 + col] + p[56 + col]) + 2048) >> 12;
        let mut v5 = p[24 + col];
        let mut v6 = p[40 + col];

        // stage 3
        let mut t = (v0 - v1 + 1) >> 1;
        v0 = (v0 + v1 + 1) >> 1;
        v1 = t;
        t = (v2 * DCT_SIN6 + v3 * DCT_COS6 + 2048) >> 12;
        v2 = (v2 * DCT_COS6 - v3 * DCT_SIN6 + 2048) >> 12;
        v3 = t;
        t = (v4 - v6 + 1) >> 1;
        v4 = (v4 + v6 + 1) >> 1;
        v6 = t;
        t = (v7 + v5 + 1) >> 1;
        v5 = (v7 - v5 + 1) >> 1;
        v7 = t;

        // stage 2
        t = (v0 - v3 + 1) >> 1;
        v0 = (v0 + v3 + 1) >> 1;
        v3 = t;
        t = (v1 - v2 + 1) >> 1;
        v1 = (v1 + v2 + 1) >> 1;
        v2 = t;
        t = (v4 * DCT_SIN3 + v7 * DCT_COS3 + 2048) >> 12;
        v4 = (v4 * DCT_COS3 - v7 * DCT_SIN3 + 2048) >> 12;
        v7 = t;
        t = (v5 * DCT_SIN1 + v6 * DCT_COS1 + 2048) >> 12;
        v5 = (v5 * DCT_COS1 - v6 * DCT_SIN1 + 2048) >> 12;
        v6 = t;

        // stage 1
        p[col] = v0 + v7;
        p[56 + col] = v0 - v7;
        p[8 + col] = v1 + v6;
        p[48 + col] = v1 - v6;
        p[16 + col] = v2 + v5;
        p[40 + col] = v2 - v5;
        p[24 + col] = v3 + v4;
        p[32 + col] = v3 - v4;
    }

    // convert to 8-bit integers
    for i in 0..64 {
        let sample = 128 + ((p[i] + 8) >> 4);
        data_out[i] = clamp8(sample);
    }
}

fn build_component_data(component: &Component, mem: &mut MemoryGuard) -> JpegResult<Vec<Vec<u8>>> {
    let blocks_per_line = component.blocks_per_line;
    let blocks_per_column = component.blocks_per_column;
    let samples_per_line = blocks_per_line << 3;
    let quant = component
        .quantization_table
        .ok_or("missing quantization table")?;

    mem.request(samples_per_line * blocks_per_column * 8)?;

    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut r = [0u8; 64];

    for block_row in 0..blocks_per_column {
        let scan_line = block_row << 3;
        for _ in 0..8 {
            lines.push(vec![0u8; samples_per_line]);
        }

        for block_col in 0..blocks_per_line {
            quantize_and_inverse(&component.blocks[block_row][block_col], &quant, &mut r);

            let mut offset = 0;
            let sample = block_col << 3;
            for j in 0..8 {
                let line = &mut lines[scan_line + j];
                for i in 0..8 {
                    line[sample + i] = r[offset];
                    offset += 1;
                }
            }
        }
    }

    Ok(lines)
}

// ===========================================================================
// parse() — marker loop
// ===========================================================================

struct Parser<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Parser<'a> {
    fn read_uint16(&mut self) -> usize {
        let value = ((self.data[self.offset] as usize) << 8) | self.data[self.offset + 1] as usize;
        self.offset += 2;
        value
    }

    fn read_data_block(&mut self) -> &'a [u8] {
        let length = self.read_uint16();
        let end = (self.offset + length - 2).min(self.data.len());
        let array = &self.data[self.offset..end];
        self.offset += array.len();
        array
    }
}

fn prepare_components(frame: &mut Frame, mem: &mut MemoryGuard) -> JpegResult<()> {
    let mut max_h = 0usize;
    let mut max_v = 0usize;
    for &cid in &frame.components_order {
        let c = frame.components[cid].as_ref().unwrap();
        if max_h < c.h {
            max_h = c.h;
        }
        if max_v < c.v {
            max_v = c.v;
        }
    }

    let mcus_per_line = (frame.samples_per_line as f64 / 8.0 / max_h as f64).ceil() as usize;
    let mcus_per_column = (frame.scan_lines as f64 / 8.0 / max_v as f64).ceil() as usize;

    for &cid in &frame.components_order {
        let (h, v) = {
            let c = frame.components[cid].as_ref().unwrap();
            (c.h, c.v)
        };
        let blocks_per_line =
            ((frame.samples_per_line as f64 / 8.0).ceil() * h as f64 / max_h as f64).ceil() as usize;
        let blocks_per_column =
            ((frame.scan_lines as f64 / 8.0).ceil() * v as f64 / max_v as f64).ceil() as usize;
        let blocks_per_line_for_mcu = mcus_per_line * h;
        let blocks_per_column_for_mcu = mcus_per_column * v;
        let blocks_to_allocate = blocks_per_column_for_mcu * blocks_per_line_for_mcu;

        mem.request(blocks_to_allocate * 256)?;

        let mut blocks: Vec<Vec<[i32; 64]>> = Vec::with_capacity(blocks_per_column_for_mcu);
        for _ in 0..blocks_per_column_for_mcu {
            let mut rowv: Vec<[i32; 64]> = Vec::with_capacity(blocks_per_line_for_mcu);
            for _ in 0..blocks_per_line_for_mcu {
                rowv.push([0i32; 64]);
            }
            blocks.push(rowv);
        }

        let c = frame.components[cid].as_mut().unwrap();
        c.blocks_per_line = blocks_per_line;
        c.blocks_per_column = blocks_per_column;
        c.blocks = blocks;
    }

    frame.max_h = max_h;
    frame.max_v = max_v;
    frame.mcus_per_line = mcus_per_line;
    frame.mcus_per_column = mcus_per_column;
    Ok(())
}

fn parse(data: &[u8]) -> JpegResult<Decoded> {
    let mut mem = MemoryGuard::new();
    let max_resolution_in_pixels = MAX_RESOLUTION_IN_MP * 1000 * 1000;

    let mut p = Parser { data, offset: 0 };

    let mut adobe_transform_code: Option<u8> = None;
    let mut reset_interval = 0usize;
    // quantization tables indexed by id (0..15)
    let mut quantization_tables: Vec<Option<[i32; 64]>> = vec![None; 16];
    let mut huffman_tables_ac: Vec<Option<HuffmanTable>> = vec![None; 16];
    let mut huffman_tables_dc: Vec<Option<HuffmanTable>> = vec![None; 16];

    let mut frame: Option<Frame> = None;
    let mut frame_count = 0usize;

    let mut file_marker = p.read_uint16();
    if file_marker != 0xFFD8 {
        return Err("SOI not found".to_string());
    }

    file_marker = p.read_uint16();
    while file_marker != 0xFFD9 {
        match file_marker {
            0xFF00 => {}
            0xFFE0..=0xFFEF | 0xFFFE => {
                let app_data = p.read_data_block();
                if file_marker == 0xFFEE
                    && app_data.len() >= 12
                    && app_data[0] == 0x41
                    && app_data[1] == 0x64
                    && app_data[2] == 0x6F
                    && app_data[3] == 0x62
                    && app_data[4] == 0x65
                    && app_data[5] == 0
                {
                    // Adobe APP14
                    adobe_transform_code = Some(app_data[11]);
                }
                // JFIF/EXIF/comments are parsed in TS but unused for pixel output.
            }
            0xFFDB => {
                // DQT
                let length = p.read_uint16();
                let end = length + p.offset - 2;
                while p.offset < end {
                    let spec = data[p.offset] as usize;
                    p.offset += 1;
                    mem.request(64 * 4)?;
                    let mut table = [0i32; 64];
                    if (spec >> 4) == 0 {
                        // 8 bit
                        for j in 0..64 {
                            let z = DCT_ZIG_ZAG[j];
                            table[z] = data[p.offset] as i32;
                            p.offset += 1;
                        }
                    } else if (spec >> 4) == 1 {
                        // 16 bit
                        for j in 0..64 {
                            let z = DCT_ZIG_ZAG[j];
                            table[z] = p.read_uint16() as i32;
                        }
                    } else {
                        return Err("DQT: invalid table spec".to_string());
                    }
                    quantization_tables[spec & 15] = Some(table);
                }
            }
            // SOF0 (0xFFC0), SOF1 (0xFFC1), SOF2 (0xFFC2) — contiguous marker range.
            0xFFC0..=0xFFC2 => {
                // SOF
                p.read_uint16(); // length
                let progressive = file_marker == 0xFFC2;
                let _precision = data[p.offset];
                p.offset += 1;
                let scan_lines = p.read_uint16();
                let samples_per_line = p.read_uint16();

                let pixels_in_frame = scan_lines * samples_per_line;
                if pixels_in_frame > max_resolution_in_pixels {
                    let exceeded = (pixels_in_frame - max_resolution_in_pixels).div_ceil(1_000_000);
                    return Err(format!("maxResolutionInMP limit exceeded by {exceeded}MP"));
                }

                let mut f = Frame {
                    progressive,
                    scan_lines,
                    samples_per_line,
                    components: vec![None; 256],
                    components_order: Vec::new(),
                    max_h: 0,
                    max_v: 0,
                    mcus_per_line: 0,
                    mcus_per_column: 0,
                };

                let components_count = data[p.offset] as usize;
                p.offset += 1;
                for _ in 0..components_count {
                    let component_id = data[p.offset] as usize;
                    let h = (data[p.offset + 1] >> 4) as usize;
                    let v = (data[p.offset + 1] & 15) as usize;
                    let q_id = data[p.offset + 2] as usize;
                    f.components_order.push(component_id);
                    f.components[component_id] = Some(Component {
                        h,
                        v,
                        blocks_per_line: 0,
                        blocks_per_column: 0,
                        blocks: Vec::new(),
                        pred: 0,
                        quantization_idx: q_id,
                        quantization_table: None,
                        huffman_table_dc: None,
                        huffman_table_ac: None,
                    });
                    p.offset += 3;
                }
                prepare_components(&mut f, &mut mem)?;
                frame = Some(f);
                frame_count += 1;
            }
            0xFFC4 => {
                // DHT
                let huffman_length = p.read_uint16();
                let mut i = 2;
                while i < huffman_length {
                    let spec = data[p.offset] as usize;
                    p.offset += 1;
                    let mut code_lengths = [0u8; 16];
                    let mut code_length_sum = 0usize;
                    for j in 0..16 {
                        code_lengths[j] = data[p.offset];
                        code_length_sum += code_lengths[j] as usize;
                        p.offset += 1;
                    }
                    mem.request(16 + code_length_sum)?;
                    let mut values = vec![0u8; code_length_sum];
                    for j in 0..code_length_sum {
                        values[j] = data[p.offset];
                        p.offset += 1;
                    }
                    i += 17 + code_length_sum;
                    let index = spec & 15;
                    let table = build_huffman_table(&code_lengths, &values);
                    if (spec >> 4) == 0 {
                        huffman_tables_dc[index] = Some(table);
                    } else {
                        huffman_tables_ac[index] = Some(table);
                    }
                }
            }
            0xFFDD => {
                // DRI
                p.read_uint16();
                reset_interval = p.read_uint16();
            }
            0xFFDC => {
                // Number of Lines
                p.read_uint16();
                p.read_uint16();
            }
            0xFFDA => {
                // SOS
                p.read_uint16(); // length
                let selectors_count = data[p.offset] as usize;
                p.offset += 1;
                let f = frame.as_mut().ok_or("SOS before SOF")?;
                let mut component_ids: Vec<usize> = Vec::with_capacity(selectors_count);
                for _ in 0..selectors_count {
                    let cid = data[p.offset] as usize;
                    p.offset += 1;
                    let table_spec = data[p.offset] as usize;
                    p.offset += 1;
                    let c = f.components[cid].as_mut().ok_or("unknown component in SOS")?;
                    c.huffman_table_dc = Some(table_spec >> 4);
                    c.huffman_table_ac = Some(table_spec & 15);
                    component_ids.push(cid);
                }
                let spectral_start = data[p.offset] as usize;
                p.offset += 1;
                let spectral_end = data[p.offset] as usize;
                p.offset += 1;
                let successive_approximation = data[p.offset] as i32;
                p.offset += 1;

                let tables = HuffmanTables {
                    dc: &huffman_tables_dc,
                    ac: &huffman_tables_ac,
                };
                let processed = decode_scan(
                    data,
                    p.offset,
                    f,
                    &component_ids,
                    &tables,
                    reset_interval,
                    spectral_start,
                    spectral_end,
                    successive_approximation >> 4,
                    successive_approximation & 15,
                )?;
                p.offset += processed;
            }
            0xFFFF => {
                // Fill bytes
                if data[p.offset] != 0xFF {
                    p.offset -= 1;
                }
            }
            _ => {
                if p.offset >= 3
                    && data[p.offset - 3] == 0xFF
                    && data[p.offset - 2] >= 0xC0
                    && data[p.offset - 2] <= 0xFE
                {
                    p.offset -= 3;
                } else {
                    return Err(format!("unknown JPEG marker {file_marker:x}"));
                }
            }
        }

        file_marker = p.read_uint16();
    }

    if frame_count != 1 {
        return Err("only single frame JPEGs supported".to_string());
    }

    let mut frame = frame.unwrap();

    // assign quantization tables
    for &cid in &frame.components_order {
        let q_idx = frame.components[cid].as_ref().unwrap().quantization_idx;
        let qt = quantization_tables[q_idx];
        frame.components[cid].as_mut().unwrap().quantization_table = qt;
    }

    let width = frame.samples_per_line;
    let height = frame.scan_lines;
    let max_h = frame.max_h;
    let max_v = frame.max_v;

    let mut components: Vec<DecodedComponent> = Vec::new();
    for &cid in &frame.components_order.clone() {
        let component = frame.components[cid].as_ref().unwrap();
        let scale_x = component.h as f64 / max_h as f64;
        let scale_y = component.v as f64 / max_v as f64;
        let lines = build_component_data(component, &mut mem)?;
        components.push(DecodedComponent {
            lines,
            scale_x,
            scale_y,
        });
    }

    Ok(Decoded {
        width,
        height,
        adobe_transform_code,
        components,
    })
}

// ===========================================================================
// getData — produce interleaved component samples (port of getData)
// ===========================================================================

/// Clamps a sample to the 0..=255 range, mirroring upstream `clampTo8bit`.
///
/// `f64::clamp` matches the upstream ternary exactly, NaN included (NaN is
/// neither `< 0.0` nor `> 255.0`, so it passes through unchanged).
fn clamp_to_8bit(a: f64) -> f64 {
    a.clamp(0.0, 255.0)
}

fn get_data(decoded: &Decoded) -> Vec<u8> {
    let width = decoded.width;
    let height = decoded.height;
    let n = decoded.components.len();
    let mut data = vec![0u8; width * height * n];
    let mut offset = 0usize;

    match n {
        1 => {
            let c1 = &decoded.components[0];
            for y in 0..height {
                let line = &c1.lines[(y as f64 * c1.scale_y) as usize];
                for x in 0..width {
                    data[offset] = line[(x as f64 * c1.scale_x) as usize];
                    offset += 1;
                }
            }
        }
        2 => {
            let c1 = &decoded.components[0];
            let c2 = &decoded.components[1];
            for y in 0..height {
                let l1 = &c1.lines[(y as f64 * c1.scale_y) as usize];
                let l2 = &c2.lines[(y as f64 * c2.scale_y) as usize];
                for x in 0..width {
                    data[offset] = l1[(x as f64 * c1.scale_x) as usize];
                    offset += 1;
                    data[offset] = l2[(x as f64 * c2.scale_x) as usize];
                    offset += 1;
                }
            }
        }
        3 => {
            // colorTransform defaults to true for 3 components.
            let color_transform = true;
            let c1 = &decoded.components[0];
            let c2 = &decoded.components[1];
            let c3 = &decoded.components[2];
            for y in 0..height {
                let l1 = &c1.lines[(y as f64 * c1.scale_y) as usize];
                let l2 = &c2.lines[(y as f64 * c2.scale_y) as usize];
                let l3 = &c3.lines[(y as f64 * c3.scale_y) as usize];
                for x in 0..width {
                    let (r, g, b);
                    if !color_transform {
                        r = l1[(x as f64 * c1.scale_x) as usize];
                        g = l2[(x as f64 * c2.scale_x) as usize];
                        b = l3[(x as f64 * c3.scale_x) as usize];
                    } else {
                        let yc = l1[(x as f64 * c1.scale_x) as usize] as f64;
                        let cb = l2[(x as f64 * c2.scale_x) as usize] as f64;
                        let cr = l3[(x as f64 * c3.scale_x) as usize] as f64;
                        r = clamp_to_8bit(yc + 1.402 * (cr - 128.0)) as u8;
                        g = clamp_to_8bit(yc - 0.3441363 * (cb - 128.0) - 0.71413636 * (cr - 128.0))
                            as u8;
                        b = clamp_to_8bit(yc + 1.772 * (cb - 128.0)) as u8;
                    }
                    data[offset] = r;
                    offset += 1;
                    data[offset] = g;
                    offset += 1;
                    data[offset] = b;
                    offset += 1;
                }
            }
        }
        4 => {
            // The adobe transform marker overrides; default false.
            let color_transform = decoded
                .adobe_transform_code
                .map(|t| t != 0)
                .unwrap_or(false);
            let c1 = &decoded.components[0];
            let c2 = &decoded.components[1];
            let c3 = &decoded.components[2];
            let c4 = &decoded.components[3];
            for y in 0..height {
                let l1 = &c1.lines[(y as f64 * c1.scale_y) as usize];
                let l2 = &c2.lines[(y as f64 * c2.scale_y) as usize];
                let l3 = &c3.lines[(y as f64 * c3.scale_y) as usize];
                let l4 = &c4.lines[(y as f64 * c4.scale_y) as usize];
                for x in 0..width {
                    let (cc, mm, ye, k);
                    if !color_transform {
                        cc = l1[(x as f64 * c1.scale_x) as usize] as f64;
                        mm = l2[(x as f64 * c2.scale_x) as usize] as f64;
                        ye = l3[(x as f64 * c3.scale_x) as usize] as f64;
                        k = l4[(x as f64 * c4.scale_x) as usize] as f64;
                    } else {
                        let yc = l1[(x as f64 * c1.scale_x) as usize] as f64;
                        let cb = l2[(x as f64 * c2.scale_x) as usize] as f64;
                        let cr = l3[(x as f64 * c3.scale_x) as usize] as f64;
                        k = l4[(x as f64 * c4.scale_x) as usize] as f64;
                        cc = 255.0 - clamp_to_8bit(yc + 1.402 * (cr - 128.0));
                        mm = 255.0
                            - clamp_to_8bit(
                                yc - 0.3441363 * (cb - 128.0) - 0.71413636 * (cr - 128.0),
                            );
                        ye = 255.0 - clamp_to_8bit(yc + 1.772 * (cb - 128.0));
                    }
                    data[offset] = (255.0 - cc) as u8;
                    offset += 1;
                    data[offset] = (255.0 - mm) as u8;
                    offset += 1;
                    data[offset] = (255.0 - ye) as u8;
                    offset += 1;
                    data[offset] = (255.0 - k) as u8;
                    offset += 1;
                }
            }
        }
        _ => {}
    }

    data
}

// ===========================================================================
// Public API: decode_jpeg (port of decodeJpeg)
// ===========================================================================

/// Decode a baseline/progressive JPEG into RGBA8 `PixelData`.
///
/// Port of TS `decodeJpeg(encoded, createImageData)`: the TS callback created an
/// `ImageData`; here we directly allocate the RGBA8 buffer in `PixelData`.
///
/// Supported component counts are 1 (grayscale), 2 (grayscale + alpha),
/// 3 (YCbCr) and 4 (CMYK/YCCK).
///
/// # Errors
/// Returns an error string for an empty buffer, a malformed stream, or a
/// component count outside the supported set.
pub fn decode_jpeg(encoded: &[u8]) -> JpegResult<PixelData> {
    if encoded.is_empty() {
        return Err("Empty jpeg buffer".to_string());
    }

    let decoded = parse(encoded)?;
    let data = get_data(&decoded);

    let width = decoded.width;
    let height = decoded.height;
    let mut out = vec![0u8; width * height * 4];

    let mut i = 0usize;
    let mut j = 0usize;

    match decoded.components.len() {
        1 => {
            for _ in 0..(width * height) {
                let yv = data[i];
                i += 1;
                out[j] = yv;
                out[j + 1] = yv;
                out[j + 2] = yv;
                out[j + 3] = 255;
                j += 4;
            }
        }
        2 => {
            // Grayscale + alpha: the luminance sample is replicated across RGB
            // and the second component becomes the alpha channel.
            for _ in 0..(width * height) {
                let yv = data[i];
                let a = data[i + 1];
                i += 2;
                out[j] = yv;
                out[j + 1] = yv;
                out[j + 2] = yv;
                out[j + 3] = a;
                j += 4;
            }
        }
        3 => {
            for _ in 0..(width * height) {
                out[j] = data[i];
                out[j + 1] = data[i + 1];
                out[j + 2] = data[i + 2];
                out[j + 3] = 255;
                i += 3;
                j += 4;
            }
        }
        4 => {
            for _ in 0..(width * height) {
                let c = data[i] as f64;
                let m = data[i + 1] as f64;
                let yv = data[i + 2] as f64;
                let k = data[i + 3] as f64;
                i += 4;
                let r = 255.0 - clamp_to_8bit(c * (1.0 - k / 255.0) + k);
                let g = 255.0 - clamp_to_8bit(m * (1.0 - k / 255.0) + k);
                let b = 255.0 - clamp_to_8bit(yv * (1.0 - k / 255.0) + k);
                out[j] = r as u8;
                out[j + 1] = g as u8;
                out[j + 2] = b as u8;
                out[j + 3] = 255;
                j += 4;
            }
        }
        _ => return Err("Unsupported color mode".to_string()),
    }

    Ok(PixelData {
        width: width as u32,
        height: height as u32,
        data: out,
    })
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid baseline grayscale 8x8 JPEG (single DC coefficient).
    /// Produced from a known-good encoder; decodes to an 8x8 solid-ish image.
    /// We construct it inline to avoid relying on external fixtures.
    fn tiny_gray_jpeg() -> Vec<u8> {
        // 8x8, single component, baseline. Built from a standard minimal JPEG.
        // SOI
        let mut v: Vec<u8> = vec![0xFF, 0xD8];
        // DQT (id 0, all 1s for simplicity) - length 67 = 2 + 1 + 64
        v.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
        v.extend(std::iter::repeat_n(0x01u8, 64));
        // SOF0: length 11, precision 8, height 8, width 8, 1 component, id 1, h/v=0x11, qtable 0
        v.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x08, 0x00, 0x08, 0x01, 0x01, 0x11, 0x00,
        ]);
        // DHT DC table 0: one code of length 2 -> value 0
        // counts: lengths[1]=0,... lengths[2]=1 ... rest 0
        let mut dht_dc: Vec<u8> = vec![0x00]; // Tc=0,Th=0
        let mut counts = [0u8; 16];
        counts[1] = 1; // one code of length 2
        dht_dc.extend_from_slice(&counts);
        dht_dc.push(0x00); // value 0
        let dht_dc_len = (dht_dc.len() + 2) as u16;
        v.extend_from_slice(&[0xFF, 0xC4]);
        v.extend_from_slice(&dht_dc_len.to_be_bytes());
        v.extend_from_slice(&dht_dc);
        // DHT AC table 0: one code of length 2 -> value 0 (EOB)
        let mut dht_ac: Vec<u8> = vec![0x10]; // Tc=1,Th=0
        let mut counts_ac = [0u8; 16];
        counts_ac[1] = 1;
        dht_ac.extend_from_slice(&counts_ac);
        dht_ac.push(0x00); // value 0 (EOB)
        let dht_ac_len = (dht_ac.len() + 2) as u16;
        v.extend_from_slice(&[0xFF, 0xC4]);
        v.extend_from_slice(&dht_ac_len.to_be_bytes());
        v.extend_from_slice(&dht_ac);
        // SOS: length 8, 1 component, id 1, tables 0/0, Ss=0 Se=63 Ah/Al=0
        v.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
        // Scan data: DC code (length-2 code is bit "0"), t=0 so diff=0, then AC EOB
        // DC table: single code "00" -> value 0. With value 0, diff=0.
        // The single huffman code for one symbol of length 2: code is "00".
        // We need: DC symbol (2 bits "00"), then AC EOB symbol (2 bits "00").
        // bits: 00 00 = 0x00 ... pad with 1s. Byte: 0000 0011 -> but padding is 1s.
        // Actually DC "00" then AC "00": four bits 0000, pad remaining 4 bits with 1: 0000_1111 = 0x0F
        v.push(0x0F);
        // EOI
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn decodes_tiny_gray_jpeg_dimensions() {
        let jpeg = tiny_gray_jpeg();
        let result = decode_jpeg(&jpeg).expect("should decode");
        assert_eq!(result.width, 8);
        assert_eq!(result.height, 8);
        assert_eq!(result.data.len(), 8 * 8 * 4);
        // grayscale: R==G==B and alpha 255 everywhere
        for px in result.data.chunks_exact(4) {
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
            assert_eq!(px[3], 255);
        }
    }

    /// Same minimal baseline stream as `tiny_gray_jpeg`, but with two
    /// components (luminance + alpha), both 1x1 sampled and sharing the single
    /// quantization/Huffman tables.
    fn tiny_gray_alpha_jpeg() -> Vec<u8> {
        // SOI
        let mut v: Vec<u8> = vec![0xFF, 0xD8];
        // DQT (id 0, all 1s)
        v.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, 0x00]);
        v.extend(std::iter::repeat_n(0x01u8, 64));
        // SOF0: length 14 = 8 + 3*2 components
        v.extend_from_slice(&[
            0xFF, 0xC0, 0x00, 0x0E, 0x08, 0x00, 0x08, 0x00, 0x08, 0x02, 0x01, 0x11, 0x00, 0x02,
            0x11, 0x00,
        ]);
        // DHT DC table 0: one code of length 2 -> value 0
        let mut dht_dc: Vec<u8> = vec![0x00];
        let mut counts = [0u8; 16];
        counts[1] = 1;
        dht_dc.extend_from_slice(&counts);
        dht_dc.push(0x00);
        let dht_dc_len = (dht_dc.len() + 2) as u16;
        v.extend_from_slice(&[0xFF, 0xC4]);
        v.extend_from_slice(&dht_dc_len.to_be_bytes());
        v.extend_from_slice(&dht_dc);
        // DHT AC table 0: one code of length 2 -> value 0 (EOB)
        let mut dht_ac: Vec<u8> = vec![0x10];
        let mut counts_ac = [0u8; 16];
        counts_ac[1] = 1;
        dht_ac.extend_from_slice(&counts_ac);
        dht_ac.push(0x00);
        let dht_ac_len = (dht_ac.len() + 2) as u16;
        v.extend_from_slice(&[0xFF, 0xC4]);
        v.extend_from_slice(&dht_ac_len.to_be_bytes());
        v.extend_from_slice(&dht_ac);
        // SOS: length 10 = 6 + 2*2, both components use tables 0/0
        v.extend_from_slice(&[
            0xFF, 0xDA, 0x00, 0x0A, 0x02, 0x01, 0x00, 0x02, 0x00, 0x00, 0x3F, 0x00,
        ]);
        // One MCU: DC("00") AC("00") for each component -> 8 zero bits.
        v.push(0x00);
        // EOI
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    /// Two-component streams must expand to RGBA with the luminance replicated
    /// across RGB and the second component used as alpha, instead of being
    /// rejected as an unsupported colour mode.
    #[test]
    fn decodes_two_component_jpeg_as_gray_plus_alpha() {
        let jpeg = tiny_gray_alpha_jpeg();
        let result = decode_jpeg(&jpeg).expect("should decode");
        assert_eq!(result.width, 8);
        assert_eq!(result.height, 8);
        assert_eq!(result.data.len(), 8 * 8 * 4);
        for px in result.data.chunks_exact(4) {
            assert_eq!(px[0], px[1]);
            assert_eq!(px[1], px[2]);
            // Both components decode to the same flat value here, so alpha must
            // equal the luminance rather than the hardcoded 255.
            assert_eq!(px[3], px[0]);
        }
    }

    #[test]
    fn rejects_empty_buffer() {
        assert!(decode_jpeg(&[]).is_err());
    }

    #[test]
    fn rejects_missing_soi() {
        // Starts with garbage, not 0xFFD8.
        assert!(decode_jpeg(&[0x00, 0x01, 0x02, 0x03]).is_err());
    }

    #[test]
    fn build_huffman_table_single_code() {
        let mut counts = [0u8; 16];
        counts[1] = 1; // one 2-bit code
        let table = build_huffman_table(&counts, &[42]);
        // root -> branch (bit 0) -> value 42 at [0]
        match table.nodes[0][0] {
            HuffNode::Branch(n) => match table.nodes[n][0] {
                HuffNode::Value(v) => assert_eq!(v, 42),
                _ => panic!("expected value leaf"),
            },
            _ => panic!("expected branch at root[0]"),
        }
    }

    #[test]
    fn dct_zigzag_is_permutation() {
        let mut seen = [false; 64];
        for &z in DCT_ZIG_ZAG.iter() {
            assert!(!seen[z], "duplicate index {z}");
            seen[z] = true;
        }
        assert!(seen.iter().all(|&b| b));
    }
}
