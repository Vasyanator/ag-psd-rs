# ag-psd

[![crates.io](https://img.shields.io/crates/v/ag-psd.svg)](https://crates.io/crates/ag-psd)
[![docs.rs](https://img.shields.io/docsrs/ag-psd)](https://docs.rs/ag-psd)
[![license](https://img.shields.io/crates/l/ag-psd.svg)](./LICENSE)

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

## Installation

```toml
[dependencies]
ag-psd = "0.1"
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
full tour of the options, see **[`docs/usage.md`](./docs/usage.md)**.

## Public API at a glance

The crate root re-exports the main entry points:

| Symbol | Purpose |
| --- | --- |
| `read_psd(&[u8], &ReadOptions) -> Result<Psd, ReadError>` | Parse a PSD/PSB from memory |
| `write_psd(&Psd, &WriteOptions) -> Vec<u8>` | Serialize a PSD/PSB to bytes |
| `write_psd_to_writer(&mut PsdWriter, &Psd, &WriteOptions)` | Serialize into an existing writer |
| `read_abr`, `read_csh` / `write_csh`, `read_ase` / `write_ase` | Companion Adobe formats |
| `parse_engine_data`, `serialize_engine_data`, `decode_engine_data2` | Text Engine Data |

The document model itself lives in the [`ag_psd::psd`] module: `Psd`, `Layer`,
`LayerAdditionalInfo`, `PixelData`, `BlendMode`, `ColorMode`, `ReadOptions`,
`WriteOptions`, and the many supporting types.

## Status & limitations

The bulk of the upstream library is ported and exercised against the original
test fixtures:

- **Reads** essentially all of the upstream read fixtures.
- **Round-trips** (read → write → read) the large majority of fixtures with a
  structurally stable result.

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
cargo test -p ag-psd        # 150+ unit tests
```

A fixture harness (`tests/fixtures.rs`) additionally reads and round-trips the
upstream `ag-psd` `.psd` fixtures when they are available on disk; it reports
coverage rather than asserting byte-exactness.

## Credits & license

- Ported from [`ag-psd`](https://github.com/Agamnentzar/ag-psd) by Agamnentzar.
- Original PSD format reverse-engineering and structure courtesy of the upstream
  project and the Adobe Photoshop File Format specification.
- Rust port: vibe-coded by Claude (Anthropic), maintained by Vasyanator.

Licensed under the **MIT License**, the same as upstream. The original
copyright © 2016 Agamnentzar is preserved; see [`LICENSE`](./LICENSE).
