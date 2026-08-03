# Module: crates/ag-psd/src

## Purpose

The whole `ag-psd` library. This directory is a module-for-module port of the
upstream TypeScript sources in `test/ag-psd/src` (READ-ONLY specification), with
names converted to `snake_case`. Crate-level constraints, publication and build
rules live in the parent `../MODULE_README.md`.

## Architecture

Three layers, bottom-up:

1. **Byte IO and orchestration** — `reader.rs` / `writer.rs`. They own the read
   cursor and write buffer, the PSD primitives (numbers, strings, sections,
   RLE/ZIP channel data), and the whole-document pipelines `read_psd` /
   `write_psd`.
2. **Section decoders** — `additional_info/`, `image_resources.rs`,
   `descriptor.rs`, `engine_data.rs`, `engine_data2.rs`, `text.rs`,
   `effects_helpers.rs`. Each decodes one family of PSD structures using the
   layer-1 primitives and writes into the shared model.
3. **Shared model** — `psd.rs`. Every layer above depends on it; it depends on
   nothing in the crate.

`helpers.rs`, `utf8.rs` and `jpeg.rs` are cross-cutting utilities.
`abr.rs`, `csh.rs`, `ase.rs` are self-contained parsers for companion Adobe
formats that reuse the same primitives. `lib.rs` only declares modules and
re-exports; it holds no logic.

## Upstream module map

| Upstream `*.ts` | Rust | Role |
| --- | --- | --- |
| `index.ts` | `lib.rs` | module declarations and public re-exports |
| `psd.ts` | `psd.rs` | document model: `Psd`, `Layer`, options, all value types |
| `psdReader.ts` | `reader.rs` | read primitives, memory budget, `read_psd` |
| `psdWriter.ts` | `writer.rs` | write primitives, `write_psd` |
| `additionalInfo.ts` | `additional_info/` | directory module, see below |
| `imageResources.ts` | `image_resources.rs` | image resource blocks |
| `descriptor.ts` | `descriptor.rs` | Action Descriptor read/write, enum codecs |
| `engineData.ts` | `engine_data.rs` | Engine Data token format |
| `engineData2.ts` | `engine_data2.rs` | Engine Data v2 variant |
| `text.ts` | `text.rs` | Engine Data <-> typed text-layer model |
| `effectsHelpers.ts` | `effects_helpers.rs` | legacy `lrFX` effect layouts |
| `helpers.ts` | `helpers.rs` | blend-mode tables, `EnumCodec`, channel packing |
| `utf8.ts` | `utf8.rs` | WHATWG-conformant UTF-8 encode/decode |
| `jpeg.ts` | `jpeg.rs` | baseline/progressive JPEG decoder for thumbnails |
| `abr.ts` | `abr.rs` | Photoshop brushes (read only, as upstream) |
| `csh.ts` | `csh.rs` | custom shapes (read; `write_csh` has no upstream twin) |
| `ase.ts` | `ase.rs` | swatches (read; `write_ase` has no upstream twin) |
| `initializeCanvas.ts` | `initialize_canvas.rs` | browser canvas factory — intentionally an empty stub, there is nothing to port |

## `additional_info/` (directory module)

Upstream keeps all 8BIM/8B64 layer-information keys in one ~5400-line file. Here
it is a directory: `mod.rs` owns the canonically ordered handler registry, the
aliases and the read/write dispatch, and each logical key family lives in its own
module (`adjustment_keys.rs`, `effects_keys.rs`, `metadata_keys.rs`,
`misc_keys.rs`, `smart_object_keys.rs`, `text_keys.rs`, `vector_keys.rs`), all
implementing the same three-function group contract. See
`additional_info/MODULE_README.md`.

One key pair escapes that contract: `Lr16`/`Lr32` carry a full nested layer-info
block (the layers of a 16/32-bit document), so `reader::read_additional_layer_info`
handles them itself — a group module only sees a `LayerAdditionalInfo`, and this
needs the whole `Psd`. Errors from that nested read propagate instead of being
swallowed like other handler errors.

## Contracts and invariants

- **Big-endian.** All default numeric readers/writers use `from_be_bytes` /
  `to_be_bytes`; the `_le` variants exist only where upstream passes
  `littleEndian = true`.
- **Typed errors, no panics on input.** Read paths return
  `reader::ReadError`; malformed sizes, signatures, rectangles and budget
  overruns are errors, never panics or unchecked allocations. Do not introduce
  `unwrap()`/`expect()` on values derived from file contents.
- **Memory budget.** Any new allocation whose size comes from the file must go
  through the reader's `consume_memory` / `create_image_data_bit_depth` path so
  it is charged against `ReadOptions::total_memory_limit`. Scratch buffers must
  give the charge back through `with_scratch_memory`, which refunds on the error
  path too. Charge the size this port really allocates, not the size upstream's
  typed array would have taken. Pattern bitmaps are on this path as well: they
  are charged (and kept, so never refunded) inside `reader::read_pattern`, the
  crate's single pattern implementation, which also runs the same
  `check_box_size` validation as layers and masks.
- **Writer buffer semantics.** `write_data_rle` mirrors upstream's typed-array
  behaviour: out-of-range writes are dropped rather than reported. The shared RLE
  scratch buffer must therefore be sized by `rle_scratch_size` for the widest
  case in the document (every layer plus its mask and real mask, and the
  composite, the latter for all of its channels), otherwise output is silently
  truncated. `rle_scratch_size` is also the only correct source of that size for
  local buffers such as `write_pattern`'s. Note its PSB rule: a row-length entry
  is 4 bytes on PSB and 2 on PSD, which is a deliberate divergence from
  upstream's hardcoded 2 — see the doc comment on `writer::rle_scratch_size`.
- **Section framing is owned by the dispatcher.** Handlers inside
  `additional_info/` run within an already-opened section and must not write
  their own signature, key or length.
- **Enum codecs.** `helpers::EnumCodec::decode` accepts the historical 4-char
  code, the long-form map key, and the camelCased key (Photoshop 2026 writes the
  latter two). Every codec's default must be a map *key*, never a map *value*.
- **Port fidelity.** Keep the 1:1 module mapping; when behaviour must diverge
  from upstream, say so in a comment at the site and in `CHANGELOG.md`.

## Editing map

- To add or change a document field, start in `psd.rs`, then update the
  corresponding read site and write site — they are always a pair.
- To add or change a layer-info key, see `additional_info/mod.rs` for the
  registry entry and the owning key-group module for the payload.
- To change the read pipeline, bitmap decoding, the memory budget or rectangle
  validation, see `reader.rs`.
- To change channel encoding, layer records or the scratch-buffer sizing, see
  `writer.rs`.
- To change the public surface or the crate-level docs, see `lib.rs`.
- To change descriptor enum decoding, see `helpers.rs` (`EnumCodec`) and
  `descriptor.rs`.
