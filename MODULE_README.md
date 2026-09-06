# Module: crates/ag-psd

## Purpose

Standalone Rust port of the TypeScript library
[`ag-psd`](https://github.com/Agamnentzar/ag-psd): reading and writing Adobe
Photoshop `.psd`/`.psb` documents, plus the companion Adobe formats that ship
with upstream (`.abr`, `.csh`, `.ase`, Engine Data).

The crate is **published on crates.io as `ag-psd`** and consumed from there by
the surrounding application as an ordinary registry dependency. It is not a
member of that application's cargo workspace: it lives inside its checkout for
convenience only, is its own git repository
(`https://github.com/Vasyanator/ag-psd-rs`), and carries an empty `[workspace]`
table in `Cargo.toml` to opt out of any parent workspace. Build and test it from
inside this directory.

## Reference specification

The upstream TypeScript library in `test/ag-psd` (READ-ONLY) is the
specification:

- `test/ag-psd/src/*.ts` — behavioural spec, ported module-for-module;
- `test/ag-psd/test/` — fixtures (`read/<name>/src.psd` + `data.json`) used as
  the oracle by `tests/fixtures.rs`.

The port mirrors the upstream module split 1:1, with names converted to
`snake_case`. Any deliberate divergence from upstream must be commented at the
site where it happens and recorded in `CHANGELOG.md`.

## Architecture

```text
lib.rs                       crate root: module declarations + public re-exports
  psd.rs                     shared document model (Psd/Layer/ReadOptions/…)
  reader.rs   writer.rs      byte-level IO + whole-document orchestration
  additional_info/           8BIM/8B64 layer-info sections (directory module)
  image_resources.rs  descriptor.rs  engine_data*.rs  text.rs
  effects_helpers.rs  helpers.rs  utf8.rs  jpeg.rs
  abr.rs  csh.rs  ase.rs      companion Adobe formats
```

Data flow on read: `read_psd` drives `reader.rs`, which decodes the header,
layer records, channel bitmaps and every section handler, filling one shared
`psd::Psd` value. Writing is the exact mirror through `writer.rs`. Every section
decoder depends on `psd.rs` for its types and on `reader.rs`/`writer.rs` for
primitives; nothing depends on `lib.rs`. The per-file map lives in
`src/MODULE_README.md`.

## Files and submodules

- `src/` — the library itself; see `src/MODULE_README.md`.
- `tests/fixtures.rs` — integration harness that reads and round-trips the
  upstream fixture corpus when it is present on disk, and compares the parse
  against the `data.json` ground truth upstream ships with it. Expected failures
  are pinned as explicit allowlists, so both a new failure and a newly passing
  allowlisted fixture turn the suite red.
- `docs/usage.md` — the user-facing guide. Excluded from the published package
  by `Cargo.toml` `exclude`, so links to it from `README.md` must be absolute
  URLs into the GitHub repository.
- `README.md` — crates.io / docs.rs front page.
- `CHANGELOG.md` — release history, including the upstream version this port is
  synced to.

## Contracts and invariants

- **Memory budget.** `psd::ReadOptions::total_memory_limit` is a byte budget for
  bitmaps and decode scratch buffers; `None` means unlimited and
  `ReadOptions::default()` carries `psd::DEFAULT_TOTAL_MEMORY_LIMIT` (2 GiB).
  Every allocation on the read path charges against the remaining budget and an
  overrun returns `reader::ReadError::ExceededMemoryLimit`. Because of this,
  `ReadOptions` implements `Default` by hand — it is *not* an all-`None` value.
  Scratch charges are refunded when the buffer dies, error path included.
  A decoder that can amplify its input must additionally be *bounded*, not
  merely charged: `reader::decode_packbits_row` takes the row size it is
  allowed to produce, because PackBits expands by up to 64x and an unbounded
  decode would allocate outside the budget entirely.
- **Box validation.** Layer, mask, real-mask and pattern rectangles are validated
  immediately after they are read; an inverted rectangle or a side above 30000
  (300000 for PSB) returns `reader::ReadError::InvalidBoxSize`. Patterns go
  through one shared reader (`reader::read_pattern`) so that this check and the
  memory budget cover every caller — the document keys and ABR alike.
- **Lazy bitmaps.** With `ReadOptions::use_raw_data` the reader keeps undecoded
  channel bytes in `Layer::raw_data` / `Psd::raw_composite_data` instead of
  decoding them. The free functions `reader::{get_layer_image_data,
  get_layer_mask_image_data, get_layer_real_mask_image_data,
  get_composite_image_data, decode_layer_pixels}` decode one bitmap at a time.
  This is the intended path for untrusted input: read structure, validate sizes,
  then decode layer by layer.
- **Errors.** The read path returns the typed `reader::ReadError` and must never
  panic on malformed input; index and size arithmetic is checked. The write path
  is infallible by design and reproduces upstream's silently-truncating buffer
  semantics for 8-bit RLE, so writer scratch buffers must be sized correctly
  rather than guarded at use time: `writer::rle_scratch_size` is the single
  source of that size, and its doc comment explains the PSB row-length entry
  width — 4 bytes, where upstream hardcodes 2 — which is a deliberate
  divergence. The high-depth encoders do not inherit those semantics: they
  allocate exactly, and report input the container cannot represent as
  `helpers::RleEncodeError`, which `writer::encode_channel` turns into a panic
  with the encoder's diagnostic (the write path has no other way to say no).
- **Byte order and framing.** PSD is big-endian; padding, section framing and
  key ordering are byte-exact against upstream, with one recorded exception:
  animation image resource #4000 follows Photoshop rather than upstream and
  writes three keys upstream leaves commented out (see `CHANGELOG.md`,
  *Deliberate divergences*). ZIP channel streams are written
  zlib-wrapped, matching upstream's pako `deflate`; the reader additionally
  accepts bare DEFLATE, which some third-party writers emit.
- **Writer scope.** Writing is RGB at 8, 16 or 32 bits per channel; 16- and
  32-bit documents carry their layer records in a document-level `Lr16`/`Lr32`
  block, as Photoshop does. This extends upstream, which writes 8-bit only.
  Other colour modes can be read but are re-emitted as RGB.
- **Public API.** The crate root mirrors upstream `index.ts`. Symbols with no
  upstream counterpart (`write_csh`, `write_ase`) are marked as such at their
  declaration.
- **Toolchain.** Edition 2024, MSRV 1.85, `flate2` as the only runtime
  dependency. Adding a dependency is an architectural decision, not a
  convenience.
- **Never run `cargo fmt` / `rustfmt` in this repository.** The layout is
  hand-maintained; a formatting pass produces an unreviewable diff.
- **Tests must survive packaging.** The upstream fixture tree is not part of the
  published crate, so any test that needs it has to skip gracefully when it is
  absent instead of failing or panicking.

## Editing map

- To change the document model, or add a field visible to users, see `src/psd.rs`
  — then update both the reader and the writer, and `CHANGELOG.md`.
- To change decoding/encoding of a specific PSD section, find its module in
  `src/MODULE_README.md`; layer-info keys live in `src/additional_info/`.
- To change the public surface or the crate-level docs, see `src/lib.rs`.
- To sync a new upstream release, diff `test/ag-psd/src/` between the two
  upstream tags and apply module by module; record the sync in `CHANGELOG.md`.
- To change user-facing documentation, see `README.md` (overview, API table,
  status) and `docs/usage.md` (guide, examples).
