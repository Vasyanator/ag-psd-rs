# Changelog

All notable changes to this crate are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-09-06

### Added

- **Writing 16- and 32-bit RGB documents (PSD and PSB).** `Psd::bits_per_channel`
  may now be 16 or 32 as well as 8. RGBA8 samples are widened on the way out —
  16-bit as `sample * 257`, 32-bit as `sample / 255.0` in big-endian IEEE-754 —
  and layer records go into a document-level `Lr16`/`Lr32` tagged block, which
  is where Photoshop reads them from; the ordinary layer-info section is left
  empty for such documents. This extends upstream ag-psd, which writes 8-bit
  only. Consequently `read/16bits` and `read/32bits` are no longer expected
  round-trip failures in the fixture suite.
- `helpers::write_data_raw_bit_depth`, `helpers::write_data_rle_bit_depth`,
  `helpers::write_data_zip_without_prediction_bit_depth`,
  `helpers::expand_channel_samples` and `helpers::bytes_per_sample`: the
  bit-depth-aware channel writers.
- `helpers::RleEncodeError`: the typed reason a high-depth PackBits channel
  cannot be produced — an unsupported depth, an invalid bitmap, or a row longer
  than a PSD row-length entry can address (use PSB). It is reported instead of
  emitting a shorter, silently corrupt channel.
- The reader accepts ZIP channel streams in **both** framings: zlib-wrapped
  deflate (what Photoshop, upstream and this crate's writer emit) and bare
  DEFLATE (emitted by some third-party writers). The writer is unchanged and
  stays zlib-wrapped, matching upstream's pako `deflate`.
- The reader decodes ZIP composite image data and 16/32-bit pattern channels,
  both of which previously errored.

### Fixed

- **Frame animation descriptors were written with the wrong class id.** The
  nested frame and animation-set descriptors of image resource #4000 were built
  as `AnFr` and `AnSt`; upstream maps `FrIn` and `FSts` to `nullType`, so the
  class id must be `null`. (`AnSt` is upstream's *antialias* enum value, which
  is most likely where the porting mistake came from.) Every animated document
  this crate wrote carried the defect. The read path was never affected — it
  does not inspect the class id, which is why a round-trip test could not see
  the bug. After the fix, resource #4000 is reproduced byte-for-byte for three
  of the five animated fixtures in the upstream corpus.
- `writer::add_children` and `writer::get_largest_layer_size` walk the layer
  tree with an explicit stack instead of recursion, and a group's closing folder
  record no longer deep-copies the subtree it discards. A deeply nested document
  no longer overflows the stack or costs quadratic work.
- `helpers::write_data_raw` returns `None` instead of panicking when the pixel
  buffer is shorter than the declared `width * height * 4`.
- PackBits encoding no longer emits a literal header of 128 (a decoder no-op)
  for literal runs that overshot the 128-byte limit.
- The ZIP scratch buffer used to find a composite channel's compressed length is
  charged against `ReadOptions::total_memory_limit` and refunded, error paths
  included, like every other read-path allocation. That length is also checked
  to decompress to exactly one bitmap's worth of samples, so a short stream
  cannot advance the cursor into the middle of the next channel.
- **PackBits row decompression is bounded by the declared row size.** PackBits
  amplifies by up to 64x, so a hostile row could otherwise decode into an
  unbounded buffer outside `ReadOptions::total_memory_limit` — a 1 MiB row
  expanded to 64 MiB for a bitmap declaring four pixels. `decode_packbits_row`
  now stops at `width * bytes_per_sample`.

### Deliberate divergences from upstream

- **Animation image resource #4000 gains three keys upstream omits.** Every
  animated `.psd` written by Photoshop in the upstream fixture corpus (five
  files) begins the resource with `AFSt` (a long, 0) and carries an `8BIM`/`Roll`
  block of eight zero bytes; upstream has code for both, commented out. Both are
  now written. Photoshop also omits `FrDs` for frames using the automatic
  disposal method, and upstream's own reader documents a missing `FrDs` as
  automatic, so it is omitted rather than written explicitly.
- **The `AnDs` payload is not padded to a four-byte boundary**, matching
  upstream's `writeSection(round = 1)`. This is the one animation decision the
  corpus cannot settle: a payload holding a root plus `N` frame/set descriptors
  is a multiple of four exactly when `N` is odd, and all five Photoshop samples
  have an odd `N` (3, 3, 3, 9, 17), so padding and no padding produce identical
  bytes for every file available to test against. Photoshop's behaviour for an
  even `N` is unknown. Padding would also leave bytes our own reader stops short
  of, making it report unread section data.
- **The reader still registers image resource #4000 only**, not the whole
  `4000..=4999` range: that is Adobe's generic "Plug-In resources" range, and the
  same fixtures carry an unrelated `mfri` blob at 4001.
- **16- and 32-bit writing extends upstream**, which writes 8-bit only; see
  *Added* above. The composite image remains PackBits at every depth, because
  upstream records that Photoshop does not support ZIP-compressed composite data.

## [0.2.0] - 2026-08-03

Synchronises the port with upstream [`ag-psd`](https://github.com/Agamnentzar/ag-psd)
**v31.0.2** (previously v30.2.0). The two headline changes are Photoshop 2026
compatibility and a memory budget on the read path.

### Breaking

- **`ReadOptions::default()` is no longer an all-`None` value.** It now carries
  `total_memory_limit: Some(DEFAULT_TOTAL_MEMORY_LIMIT)` — a cumulative **2 GiB**
  budget for decoded bitmaps and decode scratch buffers. A large document that
  read successfully with 0.1.0 can now fail with
  `ReadError::ExceededMemoryLimit`. **The budget is opt-out:** pass
  `total_memory_limit: None` for the previous unlimited behaviour, or set your
  own ceiling. `ReadOptions` no longer derives `Default`; the hand-written impl
  is what supplies the non-`None` field.
- `ReadOptions` gains the field `total_memory_limit: Option<usize>` and `Psd`
  gains `raw_composite_data: Option<Vec<u8>>`; exhaustive struct literals for
  either type need updating (`..Default::default()` keeps working).
- `ReadError` gains the variants `ExceededMemoryLimit { requested, available }`
  and `InvalidBoxSize { kind, width, height }`.
- `BlendMode` gains `LinearHeight`, `Height` and `Subtraction`;
  `GradientColorModel` gains `Hsl`. Exhaustive `match` expressions over either
  enum need a new arm.
- `helpers::from_blend_mode` now returns `Option<&'static str>` instead of
  `&'static str`: the three new descriptor-only blend modes have no legacy
  4-character layer-record signature and yield `None`.
- `helpers::ChannelData` fields renamed to match upstream: `channel_id` -> `id`,
  `buffer` -> `data`.
- `utf8::decode_string` now returns `String` instead of
  `Result<String, Utf8DecodeError>`. Malformed input produces `U+FFFD` following
  the WHATWG Encoding Standard's non-fatal error mode instead of failing.

### Removed

- `utf8::Utf8DecodeError` — decoding no longer fails (see above).
- The lossy latin-1 fallback that `image_resources::read_encoded_string` used for
  non-ASCII payloads. Upstream falls back to UTF-8 when its GBK decoder is
  unavailable, and so does this port now.

### Added

- **Memory budget on read.** `ReadOptions::total_memory_limit` and the constant
  `psd::DEFAULT_TOTAL_MEMORY_LIMIT` (2 GiB). Bitmap allocations and RLE
  row-length scratch tables are charged against the remaining budget, and an
  overrun returns `ReadError::ExceededMemoryLimit` rather than attempting the
  allocation. Scratch charges are refunded when the buffer dies, on the error
  path as well as the success path, and are sized by what this port really
  allocates rather than by what upstream's typed arrays would have taken — see
  *Deliberate divergences* below.
- **Rectangle validation.** Layer, mask, real-mask and pattern rectangles are
  checked as soon as they are read: an inverted rectangle, or a side above 30000
  (300000 for PSB), returns `ReadError::InvalidBoxSize`. Pattern rectangles are
  a deliberate divergence — upstream does not check them at all.
- **Lazy bitmap API.** With `ReadOptions::use_raw_data` the reader now also keeps
  the undecoded composite in `Psd::raw_composite_data`, and five functions
  re-exported from the crate root decode one bitmap at a time:
  `get_layer_image_data`, `get_layer_mask_image_data`,
  `get_layer_real_mask_image_data`, `get_composite_image_data` and
  `decode_layer_pixels`. This keeps peak memory at one layer instead of the whole
  document and is the recommended path for untrusted files; see
  `docs/usage.md`. The upstream canvas variants have no Rust equivalent.
- **Photoshop 2026 descriptor compatibility.** Photoshop 2026 writes descriptor
  enum values in long form (`BlnM.normal`) and camelCase for multi-word values
  (`BlnM.colorBurn`) instead of the historical 4-character codes (`BlnM.Nrml`,
  `BlnM.CBrn`). Enum decoding now accepts the code, the map key, and the
  camelCased key, in that order. New helper `helpers::enum_long_form_to_key`.
- New blend modes `linear height`, `height` and `subtraction` (descriptor-only,
  used by ABR brushes), and `pass through` in the `BlnM` descriptor table.
- `hsl` added to the `ClrS` colour-space enum and to
  `EffectNoiseGradient.colorModel`.
- Indexed pattern data (`compressionMode == 0`) is decoded through the pattern's
  palette instead of being rejected — in the shared pattern reader, so on the
  ABR and the smart-object/document paths alike.
- JPEG thumbnails with 2 components (grayscale + alpha) are decoded instead of
  being rejected.
- The writer can now emit channels straight from `Layer::raw_data` without
  decoding them first, so a document read with `use_raw_data` round-trips
  verbatim.
- `ImageResources::is_empty`.
- Crate-root re-exports for names that the public signatures already mention:
  `ReadError`, `ReadResult`, `PixelData` and `DEFAULT_TOTAL_MEMORY_LIMIT`.
- **Verification against upstream's own ground truth.** The fixture harness now
  compares every parsed document against the `data.json` dump that upstream
  ships next to each fixture (header fields plus the layer tree's names, bounds
  and blend modes) — currently 68 fixtures, 0 mismatches — and asserts every
  layer and effect blend mode of the Photoshop 2026 fixture. Read, round-trip
  and ground-truth failures are pinned as explicit allowlists instead of
  pass-rate thresholds, so an allowlisted fixture that starts passing turns the
  suite red just like a new failure does.

### Fixed

- **16-bit and 32-bit documents lost every layer on read.** Photoshop stores the
  layer records of a high-bit-depth document in the `Lr16`/`Lr32` additional-info
  section and leaves the ordinary `Layr` section empty; that section was skipped,
  so such a document parsed "successfully" with no layers at all. This affects
  the published 0.1.0: anyone reading 16/32-bit files with it has been getting
  empty documents without an error. Those sections are now parsed through the
  same layer-info reader as the 8-bit path, and errors raised inside them
  propagate (see *Deliberate divergences*).
- **Pattern reading was unguarded on the live path.** `Patt`/`Pat2`/`Pat3` and
  ABR patterns each went through their own copy of `read_pattern`, none of which
  validated the declared rectangle or consulted the memory budget: a hostile file
  could request a multi-gigabyte allocation, underflow the width computation, or
  wrap the buffer-size multiplication. The three copies are now one shared
  implementation in `reader`, so the document path is covered by
  `total_memory_limit` and by the same rectangle check as layers and masks.
- **`levl` (Levels) adjustment layers were corrupted on write.** The writer
  emitted channels in the order rgb, red, blue, green while the reader expects
  rgb, red, green, blue, silently swapping the green and blue curves on every
  round trip.
- **The writer's shared RLE scratch buffer could be undersized**, and because it
  mirrors upstream's typed-array semantics (out-of-range writes are dropped), the
  result was a structurally valid but silently truncated file. Three causes are
  fixed: layer masks larger than their layer were not measured at all; PSB
  row-length tables use 4-byte entries where 2 were budgeted, which **cost PSB
  output its pixel data** whenever the encoded channel no longer fit the short
  buffer (a tall, narrow channel, where the table dominates, does so at any
  size); and the composite estimate charged the row-length table once for the
  whole bitmap instead of once per channel. The last two are deliberate
  divergences from upstream, which still has both bugs.
- **`write_pattern`'s per-channel buffer was not a bound.** It followed
  upstream's `width * height + 2 * height + 2 * width + 16` formula, which does
  not cover the run headers of a tall, narrow channel and overflowed `u32` on a
  large pattern; it is now sized by the writer's proven `rle_scratch_size` bound
  in saturating `usize`.
- `ReadError::InvalidSignature` escapes control characters in its message: the
  signature is raw file data (four NUL bytes on a truncated file) and must not be
  able to smuggle escape sequences into a log or a terminal.
- Uppercase hexadecimal (`A`–`F`) in image-resource strings decoded to garbage
  (`char_to_nibble` only handled digits and lowercase).
- Engine Data key ordering: the reserved key `'98'` was never hoisted to the
  front unless `'99'` was also present.
- Unsupported colour modes were named two slots off (`multichannel`, `duotone`
  and `lab` were misreported) because the name table was missing two entries.
- `GlobalLayerMaskInfo.opacity` was truncated instead of rounded on write.
- `Psd::image_resources` was always `Some(..)`, even for a document carrying no
  image resources at all; it is now `None` in that case.
- ASE palettes with an out-of-range colour type are rejected instead of
  producing an entry with no type.
- UTF-8 decoding is now conformant with the WHATWG Encoding Standard: overlong
  sequences, surrogates, out-of-range code points, invalid lead bytes and
  truncated trailing sequences all produce `U+FFFD` rather than being
  mis-decoded.
- "Compression not supported" errors report the offending mode consistently
  across the reader.

### Deliberate divergences from upstream

Behaviour that intentionally differs from `ag-psd` v31.0.2. Each one is also
commented at its site in the source; **a future upstream sync must not "restore"
them**.

- **PSB RLE row-length entries are 4 bytes wide** (`writer::rle_scratch_size`).
  Upstream hardcodes 2 bytes and ignores the PSB flag, so its scratch buffer is
  short by `2 * height` per channel and `writeDataRLE` silently truncates the
  output. Shipping known corruption is worse than deviating.
- **The composite scratch estimate charges the row-length table per channel**
  (`writer::write_psd_to_writer`). Upstream's
  `4 * 2 * w * h + 2 * h` charges it once although the composite is encoded in a
  single call over all its channels.
- **`write_pattern` sizes its channel buffer with `rle_scratch_size`** instead of
  upstream's `w * h + 2 * h + 2 * w + 16`, which is neither a bound for tall,
  narrow channels nor overflow-free.
- **Pattern and pattern-channel rectangles are box-checked**
  (`reader::read_pattern`). Upstream validates no pattern rectangle at all and
  relies on the memory limit alone, which cannot catch an inverted rectangle and,
  with fixed-width integers, cannot catch a wrapping size computation either. A
  pattern above the format maximum is therefore rejected here even when the
  budget is unlimited.
- **Errors from a nested `Lr16`/`Lr32` section propagate**
  (`reader::read_additional_layer_info`). Upstream funnels them through the same
  per-handler `try`/`catch` as every other key, which turns a rejected rectangle
  or an exhausted `total_memory_limit` into a successful read of a document with
  no layers. Losing a document's layers is not a "missing feature".
- **Scratch memory is refunded on the error path** (`reader::with_scratch_memory`).
  Upstream calls `consumeMemory`/`recoverMemory` as plain statements, so a throw
  in between shrinks the budget for the rest of the read.
- **The RLE row-length table is charged at its real allocation size**
  (`reader::read_data_rle`): 4 bytes per entry, the width of the `Vec<u32>` this
  port actually allocates, rather than upstream's typed-array width (2 bytes for
  PSD). The budget exists to bound real memory, so the same file may hit the
  limit slightly earlier here than in upstream.
- *(since 0.1.0, listed here for completeness)* **`additional_info` is a
  directory module**, not upstream's single ~5400-line `additionalInfo.ts`. It is
  a layout divergence only: key ordering, framing and behaviour are unchanged.

## [0.1.0] - 2026-06-28

### Added

- Initial public release of the Rust port of [`ag-psd`](https://github.com/Agamnentzar/ag-psd).
- `read_psd` / `write_psd` / `write_psd_to_writer` for PSD and PSB documents.
- Typed document model under `ag_psd::psd` (`Psd`, `Layer`, `LayerAdditionalInfo`,
  `PixelData`, `BlendMode`, `ColorMode`, `ReadOptions`, `WriteOptions`, …).
- Layer trees and groups, blend modes, opacity, bounds, visibility, clipping,
  layer masks.
- Composite and per-layer pixel data with PackBits/RLE and ZIP compression.
- Editable text layers (Engine Data, type-tool transform, styles).
- Layer effects, vector/shape data, adjustment layers, smart-object metadata,
  image resources, annotations, artboards.
- Companion Adobe formats: `.abr` (read), `.csh` (read/write), `.ase`
  (read/write), and an Engine Data parser/serializer.

### Known limitations

- Writing is 8-bit RGB only (faithful to upstream); other modes/depths read but
  do not re-emit in their original form.
- CMYK read is rejected at the header.
- Some advanced subtrees are partial/stubbed; see the README and `docs/usage.md`.
