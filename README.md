# ag-psd

[![crates.io](https://img.shields.io/crates/v/ag-psd.svg)](https://crates.io/crates/ag-psd)
[![docs.rs](https://img.shields.io/docsrs/ag-psd)](https://docs.rs/ag-psd)
[![license](https://img.shields.io/crates/l/ag-psd.svg)](https://github.com/Vasyanator/ag-psd-rs/blob/main/LICENSE)

Read and write Adobe Photoshop (`.psd` / `.psb`) files in **pure Rust**, with no
native Photoshop or system dependencies.

This crate is a from-scratch Rust port of the excellent
[`ag-psd`](https://github.com/Agamnentzar/ag-psd) TypeScript library by
Agamnentzar. The public data model deliberately mirrors the upstream library, so
the structures (`Psd`, `Layer`, `ReadOptions`, `WriteOptions`, …) will feel
familiar if you've used the JavaScript version.

> ### 🤖 This is a vibe-coded port
>
> The overwhelming majority of this code was written by **Claude** (Anthropic),
> driven from the original TypeScript source as the specification and the
> upstream test fixtures as the oracle. It is used in production in a manhwa
> typesetting tool (PSD export of source / clean / editable text layers), but it
> has **not** been hand-audited line by line. Treat it accordingly: it is well
> tested against real fixtures, but it is not a battle-hardened, human-reviewed
> codebase. Bug reports and PRs are very welcome.

## Features

- **Read** PSD/PSB documents into a rich, typed object model.
- **Write** PSD/PSB documents from that same model (round-trippable).
- **Layers & groups** — nested layer trees, names, opacity, blend modes, bounds,
  visibility, clipping, layer masks.
- **Pixel data** — composite image and per-layer RGBA pixels, with PackBits/RLE
  and ZIP compression supported on read and write.
- **Text layers** — editable type layers, including the Engine Data text engine
  blob (font, size, alignment, paragraph/character styles) and the type-tool
  transform matrix.
- **Layer effects** — drop shadow, stroke, glow, overlays, bevel, etc.
- **Vector / shape** data, adjustment layers, smart-object metadata, image
  resources, annotations, artboards, global layer mask info.
- **Companion Adobe formats**, ported alongside the core library:
  - `.abr` — Photoshop brushes (read)
  - `.csh` — custom shapes (read/write)
  - `.ase` — Adobe Swatch Exchange palettes (read/write)
  - Engine Data parser/serializer (text engine)
- **Photoshop 2026 compatible.** Photoshop 2026 writes descriptor enum values in
  long form (`BlnM.normal`, `BlnM.colorBurn`) instead of the historical 4-character
  codes (`BlnM.Nrml`, `BlnM.CBrn`). Both spellings are accepted.
- **Bounded memory on read.** Reading enforces a byte budget for decoded bitmaps
  and validates layer, mask and pattern rectangles, so a malformed or hostile
  file fails with an error instead of exhausting memory — see
  [Memory limits](#memory-limits).

## Installation

```toml
[dependencies]
ag-psd = "0.2"
```

Requires Rust **1.85+** (the crate uses the 2024 edition). The only runtime
dependency is [`flate2`](https://crates.io/crates/flate2) for ZIP-compressed
channel data.

## Quick start

```rust
use ag_psd::{read_psd, write_psd};
use ag_psd::psd::{ReadOptions, WriteOptions};

fn main() -> std::io::Result<()> {
    // --- Read ---
    let bytes = std::fs::read("input.psd")?;
    let psd = read_psd(&bytes, &ReadOptions::default())
        .expect("failed to parse PSD");

    println!("{}x{} document", psd.width, psd.height);
    if let Some(children) = &psd.children {
        for layer in children {
            println!("layer: {:?}", layer.additional_info.name);
        }
    }

    // --- Write ---
    let out = write_psd(&psd, &WriteOptions::default());
    std::fs::write("output.psd", out)?;
    Ok(())
}
```

For building documents from scratch, reading pixels, creating text layers, and a
full tour of the options, see the
**[usage guide](https://github.com/Vasyanator/ag-psd-rs/blob/main/docs/usage.md)**.

## Public API at a glance

The crate root re-exports the main entry points:

| Symbol | Purpose |
| --- | --- |
| `read_psd(&[u8], &ReadOptions) -> Result<Psd, ReadError>` | Parse a PSD/PSB from memory |
| `write_psd(&Psd, &WriteOptions) -> Vec<u8>` | Serialize a PSD/PSB to bytes |
| `write_psd_to_writer(&mut PsdWriter, &Psd, &WriteOptions)` | Serialize into an existing writer |
| `get_layer_image_data(&Layer) -> Result<Option<PixelData>, ReadError>` | Decode one layer's bitmap on demand |
| `get_layer_mask_image_data(&Layer) -> Result<Option<PixelData>, ReadError>` | Decode one layer's mask on demand |
| `get_layer_real_mask_image_data(&Layer) -> Result<Option<PixelData>, ReadError>` | Decode one layer's vector-derived mask |
| `get_composite_image_data(&Psd) -> Result<Option<PixelData>, ReadError>` | Decode the flattened composite on demand |
| `decode_layer_pixels(&mut Layer, use_image_data: bool) -> Result<(), ReadError>` | Decode a layer's raw data in place and drop it |
| `read_abr`, `read_csh` / `write_csh`, `read_ase` / `write_ase` | Companion Adobe formats |
| `parse_engine_data`, `serialize_engine_data`, `decode_engine_data2` | Text Engine Data |

The five decode-on-demand functions are the *lazy bitmap* API: with
`ReadOptions::use_raw_data` the reader keeps undecoded channel bytes in
`Layer::raw_data` / `Psd::raw_composite_data`, and you turn one bitmap at a time
into pixels — so peak memory is one layer, not the whole document.

`ReadError`, `ReadResult`, `PixelData` and `DEFAULT_TOTAL_MEMORY_LIMIT` are
re-exported from the crate root too, so the types that appear in those
signatures are nameable without a module path. The rest of the document model
lives in the `ag_psd::psd` module: `Psd`, `Layer`, `LayerAdditionalInfo`,
`BlendMode`, `ColorMode`, `ReadOptions`, `WriteOptions`, and the many supporting
types.

## Memory limits

`ReadOptions` carries a **cumulative budget for decoded bitmaps**:

```rust
use ag_psd::psd::ReadOptions;

// The default is 2 GiB (`ag_psd::DEFAULT_TOTAL_MEMORY_LIMIT`).
let bounded = ReadOptions::default();

// Opt out entirely — only for files you trust.
let unlimited = ReadOptions { total_memory_limit: None, ..Default::default() };

// Or pick your own ceiling.
let tight = ReadOptions {
    total_memory_limit: Some(256 * 1024 * 1024),
    ..Default::default()
};
```

Exceeding the budget aborts the read with `ReadError::ExceededMemoryLimit`, and
an inverted or absurdly large layer, mask or pattern rectangle is rejected with
`ReadError::InvalidBoxSize`. Note that `ReadOptions::default()` is therefore
**not** an all-`None` value; if a genuinely large document used to read fine and
now fails, set `total_memory_limit: None` or raise it.

For untrusted, user-provided files, prefer the lazy-bitmap path: read the
structure with `use_raw_data`, check the document and layer dimensions against
your own limits, and only then decode layers one at a time. The
[usage guide](https://github.com/Vasyanator/ag-psd-rs/blob/main/docs/usage.md#lazy-bitmaps-decoding-on-demand)
has a worked example.

## Status & limitations

The bulk of the upstream library is ported and exercised against the original
test fixtures:

- **Reads** every upstream read fixture except the CMYK one, which is rejected
  by design (upstream's own test suite skips it too).
- **Matches** the `data.json` ground truth that ships with those fixtures,
  except for the 16-bit and 32-bit documents whose layers live in an
  `Lr16`/`Lr32` section this port does not yet parse.
- **Round-trips** (read → write → read) with a structurally stable result
  everywhere the 8-bit-RGB writer limitation below does not apply.

The writer intentionally inherits the same constraints as upstream `ag-psd`:

- **Writing is 8-bit RGB only.** 16/32-bit, grayscale, indexed, bitmap and
  duotone documents can be *read*, but are not re-emitted in their original
  depth/mode. This matches upstream behavior and is the reason a handful of
  fixtures do not byte-round-trip.
- **CMYK** documents are rejected at the header on read.

Known partial / stubbed areas (not required for the typesetting use case that
drove the port, but relevant if you need "100% complete"):

- Vector gradient/pattern content (`Grad`/`Ptrn`), `vstk` stroke units,
  `vogk`/`pths` path lists.
- `Lr16`/`Lr32` nested layers, `Psd.linked_files` storage, the smart-object
  `SoLd` filter-FX subtree, `shmd` timeline/comps.
- Thumbnail generation on write, link-group resources, `Txt2` text-path
  restoration.

If you hit one of these, please open an issue — they are tracked and can be
filled in on demand.

## Testing

```sh
cargo test
```

Several hundred unit tests cover the primitives, the section decoders and the
round-trip behavior of individual keys. A fixture harness (`tests/fixtures.rs`,
not shipped in the published crate) additionally reads the upstream `ag-psd`
`.psd` fixtures when they are available on disk, compares the result against the
`data.json` ground truth that ships with them, and checks that read → write →
read is structurally stable. Its known-failure sets are pinned, so a regression
shows up as a new name rather than a slipped percentage.

## Credits & license

- Ported from [`ag-psd`](https://github.com/Agamnentzar/ag-psd) by Agamnentzar.
- Original PSD format reverse-engineering and structure courtesy of the upstream
  project and the Adobe Photoshop File Format specification.
- Rust port: vibe-coded by Claude (Anthropic), maintained by Vasyanator.

Licensed under the **MIT License**, the same as upstream. The original
copyright © 2016 Agamnentzar is preserved; see
[`LICENSE`](https://github.com/Vasyanator/ag-psd-rs/blob/main/LICENSE).
