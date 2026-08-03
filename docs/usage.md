# ag-psd — Usage Guide

This is the detailed guide for the `ag-psd` Rust crate. For a high-level
overview, capabilities, and status, see the [README](../README.md).

- [Mental model](#mental-model)
- [Reading a PSD](#reading-a-psd)
  - [ReadOptions](#readoptions)
  - [Where the pixels go: `canvas` vs `image_data`](#where-the-pixels-go-canvas-vs-image_data)
  - [Memory limits](#memory-limits)
  - [Lazy bitmaps: decoding on demand](#lazy-bitmaps-decoding-on-demand)
  - [Walking the layer tree](#walking-the-layer-tree)
  - [Error handling](#error-handling)
- [Writing a PSD](#writing-a-psd)
  - [WriteOptions](#writeoptions)
  - [Building a document from scratch](#building-a-document-from-scratch)
  - [Pixel data layout](#pixel-data-layout)
- [Text layers](#text-layers)
- [Companion formats: ABR / CSH / ASE](#companion-formats-abr--csh--ase)
- [Engine Data](#engine-data)
- [Limitations & gotchas](#limitations--gotchas)

---

## Mental model

The crate mirrors the upstream TypeScript [`ag-psd`](https://github.com/Agamnentzar/ag-psd)
data model. There are two entry points and one big document struct:

- `read_psd(&[u8], &ReadOptions) -> Result<Psd, ReadError>`
- `write_psd(&Psd, &WriteOptions) -> Vec<u8>`
- `ag_psd::psd::Psd` — the whole document.

A `Psd` is a tree:

```text
Psd
├─ width / height / color_mode / channels / bits_per_channel
├─ canvas | image_data           (composite pixels, RGBA8)
├─ raw_composite_data            (undecoded composite, with `use_raw_data`)
├─ image_resources               (ICC profile, guides, slices, …)
└─ children: Vec<Layer>
   └─ Layer
      ├─ additional_info          (name, text, effects, vector data, …)
      ├─ top / left / bottom / right
      ├─ blend_mode / opacity / hidden / clipping
      ├─ canvas | image_data      (this layer's pixels, RGBA8)
      ├─ raw_data                 (undecoded channels, with `use_raw_data`)
      └─ children: Vec<Layer>     (present when this layer is a group)
```

Almost every scalar field is an `Option<…>` so that "absent" is distinct from a
default value, exactly like the optional fields in the TS library. Most structs
derive `Default`, so you build them with `..Default::default()`.

> Note on numbers: to stay faithful to the JS source, numeric fields are `f64`
> (e.g. `width: f64`, `opacity: f64`). Opacity is in the **0.0–1.0** range.

The document types live in the `ag_psd::psd` module; the functions are
re-exported from the crate root.

---

## Reading a PSD

```rust
use ag_psd::read_psd;
use ag_psd::psd::ReadOptions;

let bytes = std::fs::read("input.psd")?;
let psd = read_psd(&bytes, &ReadOptions::default())
    .expect("failed to parse PSD");

println!("{} x {}", psd.width, psd.height);
println!("color mode: {:?}", psd.color_mode);
println!("bits/channel: {:?}", psd.bits_per_channel);
# Ok::<(), std::io::Error>(())
```

### ReadOptions

Every flag is an `Option<bool>` that defaults to "off" (`None`). The one
exception is `total_memory_limit`, whose default is **not** `None` — see
[Memory limits](#memory-limits). Set the ones you need:

| Field | Effect |
| --- | --- |
| `skip_layer_image_data` | Don't decode per-layer pixels (faster, metadata-only). |
| `skip_composite_image_data` | Don't decode the flattened composite image. |
| `skip_thumbnail` | Don't decode the embedded thumbnail. |
| `skip_linked_files_data` | Don't load smart-object linked file payloads. |
| `total_memory_limit` | `Option<usize>`: cumulative byte budget for decoded bitmaps. `None` = unlimited; the default is 2 GiB. |
| `use_image_data` | Put decoded pixels into `image_data` instead of `canvas`. |
| `use_raw_data` | Keep raw, undecoded channel bytes (`Layer::raw_data`, `Psd::raw_composite_data`) and decode later. |
| `use_raw_thumbnail` | Keep the thumbnail as raw bytes instead of decoding it. |
| `throw_for_missing_features` | Return an error when an unsupported feature is found. |
| `log_missing_features` / `log_dev_features` | Diagnostic logging flags. |
| `strict` / `debug` | Development-only strictness/diagnostics. |

Example — read metadata only, as fast as possible:

```rust
use ag_psd::read_psd;
use ag_psd::psd::ReadOptions;

# let bytes: Vec<u8> = Vec::new();
let opts = ReadOptions {
    skip_layer_image_data: Some(true),
    skip_composite_image_data: Some(true),
    skip_thumbnail: Some(true),
    ..Default::default()
};
let psd = read_psd(&bytes, &opts).unwrap();
```

### Where the pixels go: `canvas` vs `image_data`

Both `Psd` and `Layer` have two pixel slots, and both hold `PixelData`:

```rust
pub struct PixelData {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>, // RGBA8, length == width * height * 4
}
```

- **Default** (`use_image_data` unset): decoded pixels are stored in `canvas`.
- **`use_image_data: Some(true)`**: decoded pixels are stored in `image_data`.

So to read pixels, pick one and stick with it:

```rust
use ag_psd::read_psd;
use ag_psd::psd::ReadOptions;

# let bytes: Vec<u8> = Vec::new();
let opts = ReadOptions { use_image_data: Some(true), ..Default::default() };
let psd = read_psd(&bytes, &opts).unwrap();

if let Some(px) = &psd.image_data {
    // px.data is RGBA8, row-major, top-to-bottom.
    let (w, h) = (px.width, px.height);
    let first_pixel = &px.data[0..4]; // [r, g, b, a]
    println!("{w}x{h}, first pixel = {first_pixel:?}");
}
```

### Memory limits

A PSD canvas may declare up to 300000×300000 pixels with no limit on layer
count, and a file can declare huge layers without containing any matching data.
Decoding such a document naively needs terabytes of RAM. To make that survivable,
`ReadOptions` carries a **cumulative byte budget for decoded bitmaps and decode
scratch buffers**:

```rust
use ag_psd::DEFAULT_TOTAL_MEMORY_LIMIT;
use ag_psd::psd::ReadOptions;

// The default: 2 GiB, i.e. `Some(DEFAULT_TOTAL_MEMORY_LIMIT)`.
let bounded = ReadOptions::default();

// A tighter ceiling.
let tight = ReadOptions {
    total_memory_limit: Some(256 * 1024 * 1024),
    ..Default::default()
};

// No limit at all — only for files you produced or otherwise trust.
let unlimited = ReadOptions { total_memory_limit: None, ..Default::default() };
# let _ = (bounded, tight, unlimited, DEFAULT_TOTAL_MEMORY_LIMIT);
```

Consequences worth knowing:

- `ReadOptions::default()` is **not** an all-`None` value. If you match on it or
  construct it field by field, remember this one field.
- Exceeding the budget aborts the read with `ReadError::ExceededMemoryLimit`. A
  genuinely large document that used to read fine can now fail; raise the limit
  or set it to `None`.
- Layer, mask, real-mask and pattern rectangles are validated as they are read:
  inverted rectangles, and sides above 30000 (300000 for PSB), give
  `ReadError::InvalidBoxSize`.
- The budget applies to the eager read path only. Bitmaps you decode yourself
  through the lazy API below are not charged against it — you are in control of
  the pacing there.

### Lazy bitmaps: decoding on demand

With `use_raw_data`, the reader parses the full document structure but keeps the
compressed channel bytes instead of decoding them: they land in
`Layer::raw_data` and `Psd::raw_composite_data`. Five free functions then decode
one bitmap at a time:

| Function | Decodes |
| --- | --- |
| `get_layer_image_data(&Layer)` | the layer's own bitmap |
| `get_layer_mask_image_data(&Layer)` | the layer's user mask |
| `get_layer_real_mask_image_data(&Layer)` | the layer's vector-derived mask |
| `get_composite_image_data(&Psd)` | the flattened composite |
| `decode_layer_pixels(&mut Layer, use_image_data)` | all of a layer's bitmaps, in place, dropping `raw_data` |

The first four borrow immutably and return a fresh `PixelData` (or `Ok(None)`
when there is nothing to decode), so the peak cost is one bitmap rather than the
whole document.

This is the recommended way to handle **untrusted, user-provided files**: read
the structure only, validate the declared sizes against your own limits, then
decode layer by layer.

```rust
use ag_psd::{read_psd, get_layer_image_data};
use ag_psd::psd::{Layer, ReadOptions};
use ag_psd::ReadError;

# fn run(bytes: &[u8]) -> Result<(), String> {
// 1. Structure only — no bitmap is decoded here.
let opts = ReadOptions {
    use_raw_data: Some(true),
    use_raw_thumbnail: Some(true),
    ..Default::default()
};
let psd = read_psd(bytes, &opts).map_err(|e| e.to_string())?;

// 2. Check the document against your own environment limits.
if psd.width > 10000.0 || psd.height > 10000.0 {
    return Err("document too large".to_string());
}
if psd.bits_per_channel.unwrap_or(8.0) > 8.0 {
    return Err("only 8-bit color data is supported".to_string());
}

// 3. Walk the tree, validate each layer, and decode one bitmap at a time.
fn process(layer: &Layer, total: &mut usize) -> Result<(), String> {
    *total += 1;
    if *total > 100 {
        return Err("too many layers".to_string());
    }

    // Layer bounds are independent of the document size and can exceed it.
    let width = layer.right.unwrap_or(0.0) - layer.left.unwrap_or(0.0);
    let height = layer.bottom.unwrap_or(0.0) - layer.top.unwrap_or(0.0);
    if width > 10000.0 || height > 10000.0 {
        return Err("layer too large".to_string());
    }

    match get_layer_image_data(layer) {
        // Use the pixels, then let them drop: only one layer bitmap is alive
        // at a time.
        Ok(Some(px)) => println!("{}x{} decoded", px.width, px.height),
        Ok(None) => {}
        Err(ReadError::ExceededMemoryLimit { .. }) => return Err("too big".to_string()),
        Err(e) => return Err(e.to_string()),
    }

    for child in layer.children.iter().flatten() {
        process(child, total)?;
    }
    Ok(())
}

let mut total = 0usize;
for layer in psd.children.iter().flatten() {
    process(layer, &mut total)?;
}
# Ok(())
# }
```

Two further precautions, inherited from upstream's production guidance:

- Queue documents rather than decoding many in parallel; each one can still cost
  a lot of memory and time.
- Reading is synchronous and can be slow. Run it off your UI/main thread.

### Walking the layer tree

Layers are recursive: a group layer carries its members in `children`.

```rust
use ag_psd::psd::Layer;

fn print_tree(layers: &[Layer], depth: usize) {
    for layer in layers {
        let name = layer.additional_info.name.as_deref().unwrap_or("<unnamed>");
        println!("{:indent$}{name}", "", indent = depth * 2);
        if let Some(children) = &layer.children {
            print_tree(children, depth + 1);
        }
    }
}

# let psd: ag_psd::psd::Psd = Default::default();
if let Some(children) = &psd.children {
    print_tree(children, 0);
}
```

Useful per-layer fields:

- `additional_info.name: Option<String>`
- `top` / `left` / `bottom` / `right: Option<f64>` — layer bounds.
- `blend_mode: Option<BlendMode>` — e.g. `BlendMode::Normal`, `Multiply`, `PassThrough`.
- `opacity: Option<f64>` — 0.0–1.0.
- `hidden: Option<bool>`, `clipping: Option<bool>`, `transparency_protected: Option<bool>`.
- `canvas` / `image_data: Option<PixelData>`.

### Error handling

`read_psd` returns `Result<Psd, ReadError>` (`ag_psd::ReadError`, re-exported
from the `reader` module). Match on it for robust handling:

```rust
use ag_psd::read_psd;
use ag_psd::psd::ReadOptions;
use ag_psd::ReadError;

# let bytes: Vec<u8> = Vec::new();
match read_psd(&bytes, &ReadOptions::default()) {
    Ok(psd) => { /* … */ }
    Err(ReadError::ExceededMemoryLimit { requested, available }) => {
        eprintln!("needs {requested} bytes, only {available} left in the budget");
    }
    Err(ReadError::InvalidBoxSize { kind, width, height }) => {
        eprintln!("malformed {kind} rectangle: {width}x{height}");
    }
    Err(e) => eprintln!("could not read PSD: {e}"),
}
```

The variants are:

| Variant | Meaning |
| --- | --- |
| `UnexpectedEndOfBuffer` | A read ran past the end of the input. |
| `ReadingPastEndOfFile` | A declared length is implausibly large (>100 MB guard). |
| `InvalidSignature { .. }` | A section signature did not match what the format requires. |
| `SizeTooLarge` | A section declares more than 4 GB. |
| `SectionExceedsFileSize` | A section reaches past the end of the file. |
| `StrictViolation(..)` | An unsupported feature, or a strict-mode consistency check. |
| `ExceededMemoryLimit { .. }` | The bitmap budget ran out — see [Memory limits](#memory-limits). |
| `InvalidBoxSize { .. }` | A layer/mask/real-mask/pattern rectangle is inverted or too large. |

`ReadError` implements `Display` and `std::error::Error`, so `{e}` gives a
human-readable message and it composes with `Box<dyn Error>` / `anyhow`.

CMYK documents are rejected at the header; 16/32-bit, grayscale, indexed and
bitmap modes read fine.

---

## Writing a PSD

```rust
use ag_psd::write_psd;
use ag_psd::psd::WriteOptions;

# let psd: ag_psd::psd::Psd = Default::default();
let bytes = write_psd(&psd, &WriteOptions::default());
std::fs::write("output.psd", bytes)?;
# Ok::<(), std::io::Error>(())
```

### WriteOptions

| Field | Effect |
| --- | --- |
| `generate_thumbnail` | Generate a thumbnail from the composite (best-effort). |
| `trim_image_data` | Trim transparent borders from layer pixel data. |
| `invalidate_text_layers` | Force Photoshop to re-render text layers on open. |
| `no_background` | Treat the bottom layer as a normal layer, not a background. |
| `psb` | Write a PSB (Large Document Format) file instead of PSD. |
| `compress` | Use ZIP compression for channel data. |
| `log_missing_features` | Diagnostic logging. |

> **`invalidate_text_layers`**: if you have rendered text-layer pixels yourself
> and want Photoshop to display *your* pixels (not re-rasterize from the text
> engine), leave this `None`/`Some(false)`. Set it to `Some(true)` to force a
> redraw.

### Building a document from scratch

A minimal opaque RGBA document with one full-frame layer:

```rust
use ag_psd::write_psd;
use ag_psd::psd::{
    BlendMode, ColorMode, Layer, LayerAdditionalInfo, PixelData, Psd, WriteOptions,
};

let (w, h) = (256u32, 128u32);

// A solid red RGBA8 buffer.
let mut data = vec![0u8; (w * h * 4) as usize];
for px in data.chunks_exact_mut(4) {
    px.copy_from_slice(&[255, 0, 0, 255]); // R, G, B, A
}

let layer = Layer {
    additional_info: LayerAdditionalInfo {
        name: Some("Background".to_string()),
        ..Default::default()
    },
    top: Some(0.0),
    left: Some(0.0),
    bottom: Some(h as f64),
    right: Some(w as f64),
    blend_mode: Some(BlendMode::Normal),
    opacity: Some(1.0),
    hidden: Some(false),
    image_data: Some(PixelData { width: w, height: h, data: data.clone() }),
    ..Default::default()
};

let psd = Psd {
    width: w as f64,
    height: h as f64,
    color_mode: Some(ColorMode::Rgb),
    channels: Some(4.0),
    bits_per_channel: Some(8.0),
    children: Some(vec![layer]),
    // composite image (what apps that ignore layers will show):
    image_data: Some(PixelData { width: w, height: h, data }),
    ..Default::default()
};

let bytes = write_psd(&psd, &WriteOptions::default());
# let _ = bytes;
```

Grouping layers: a group is simply a `Layer` with `children: Some(vec![…])` and
typically `blend_mode: Some(BlendMode::PassThrough)` and `opened: Some(true)`.

```rust
use ag_psd::psd::{BlendMode, Layer, LayerAdditionalInfo};

# let inner_layers: Vec<Layer> = Vec::new();
let group = Layer {
    additional_info: LayerAdditionalInfo {
        name: Some("My Group".to_string()),
        ..Default::default()
    },
    blend_mode: Some(BlendMode::PassThrough),
    opacity: Some(1.0),
    hidden: Some(false),
    opened: Some(true),
    children: Some(inner_layers),
    ..Default::default()
};
# let _ = group;
```

### Pixel data layout

- `PixelData.data` is **RGBA8**: 4 bytes per pixel, `[R, G, B, A]`, row-major,
  top row first. Length must equal `width * height * 4`.
- You can supply pixels via either `image_data` or `canvas` on a `Layer`/`Psd`;
  both are `PixelData`.
- For the document to display correctly in apps that only read the flattened
  image, set `Psd.image_data` (or `Psd.canvas`) to your composite.
- **The writer is 8-bit RGB only.** Set `color_mode: Some(ColorMode::Rgb)`,
  `bits_per_channel: Some(8.0)`. Other modes/depths can be read but not written
  in their original form (same constraint as upstream `ag-psd`).

---

## Text layers

Editable type layers are represented through `LayerAdditionalInfo.text`
(`Option<LayerTextData>`), which carries the text string, the type-tool transform
matrix, and styling. Photoshop stores text styling in an "Engine Data" blob; the
crate models the structured pieces (`TextStyle`, `ParagraphStyle`, alignment,
font, size, color via `Rgb`).

The common pattern, used in production by the project this crate was extracted
from, is:

1. Render the text to pixels yourself and place them in the layer's
   `image_data` (so any viewer shows the right thing).
2. Also fill `additional_info.text` with the editable text + transform, so
   Photoshop can still edit it.
3. Write with `invalidate_text_layers` left unset, so Photoshop trusts your
   pixels instead of re-rasterizing.

Caveats inherited from the format / the use case:

- Font is identified by name; Photoshop may substitute if the font is missing.
- Font size in pixels is treated as ≈ points.
- Non-affine deformations (mesh warps) cannot be expressed as an editable text
  transform — bake those to a raster layer and optionally keep a hidden editable
  text layer alongside.

See the inline docs on `LayerTextData`, `TextStyle` and `ParagraphStyle` in the
`ag_psd::psd` module for the exact fields.

---

## Companion formats: ABR / CSH / ASE

The port includes the auxiliary Adobe parsers that ship with upstream `ag-psd`:

```rust
use ag_psd::{read_abr, read_csh, write_csh, read_ase, write_ase};
use ag_psd::ReadAbrOptions; // re-exported at the crate root, from the `abr` module

// Brushes (.abr) — read only
let abr = read_abr(&std::fs::read("brushes.abr")?, &ReadAbrOptions::default()).unwrap();

// Custom shapes (.csh) — read + write
let csh = read_csh(&std::fs::read("shapes.csh")?).unwrap();
let csh_out = write_csh(&csh);

// Swatches (.ase) — read + write
let ase = read_ase(&std::fs::read("palette.ase")?).unwrap();
let ase_out = write_ase(&ase);
# Ok::<(), std::io::Error>(())
```

(Exact option/return types are in the `abr`, `csh`, and `ase` modules.)

---

## Engine Data

Photoshop's text engine serializes its state into a token format ("Engine Data").
You can parse and re-serialize it directly:

```rust
use ag_psd::{parse_engine_data, serialize_engine_data};

# let raw: Vec<u8> = Vec::new();
let value = parse_engine_data(&raw)?;               // -> EngineValue tree
let bytes = serialize_engine_data(&value, false);   // -> Vec<u8>
# Ok::<(), ag_psd::EngineDataError>(())
```

The second argument of `serialize_engine_data` selects the condensed
(single-line) layout; pass `false` for the indented form Photoshop writes.

There is also `decode_engine_data2` for the v2 variant. This is a low-level API;
most users will interact with text through `LayerAdditionalInfo.text` instead.

---

## Limitations & gotchas

- **Writer = 8-bit RGB only** (faithful to upstream). 16/32-bit, grayscale,
  indexed, bitmap, duotone documents read but don't re-emit in original form.
- **CMYK read is rejected** at the header.
- Some advanced subtrees are partial/stubbed: vector gradient/pattern content
  (`Grad`/`Ptrn`), `vstk` stroke units, `vogk`/`pths` paths, `Lr16`/`Lr32`
  nested layers, `Psd.linked_files` storage, smart-object `SoLd` filter-FX,
  `shmd` timeline/comps, thumbnail generation, link groups, `Txt2` text paths.
  These weren't needed for the original use case; open an issue if you need them.
- Opacity is **0.0–1.0**, not 0–255.
- **`ReadOptions::default()` carries a 2 GiB bitmap budget**, so it is not an
  all-`None` value and a very large document can fail with
  `ReadError::ExceededMemoryLimit`. Set `total_memory_limit: None` to opt out,
  or use the [lazy bitmap API](#lazy-bitmaps-decoding-on-demand) to stay bounded
  without a hard ceiling.
- This is a vibe-coded port (see the README): well tested against fixtures, but
  not line-by-line human-audited. Verify critical output in real Photoshop.
