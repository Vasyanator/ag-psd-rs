# Changelog

All notable changes to this crate are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - Unreleased

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
