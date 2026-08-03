/*
File: crates/ag-psd/src/lib.rs

Purpose:
Корень крейта `ag-psd` (Rust-порт одноимённой TS-библиотеки). Зеркалирует
`index.ts`: объявляет все модули и собирает публичные re-export'ы.

Main responsibilities:
- объявить `pub mod ...;` для каждого порта upstream-файла (разбиение 1:1);
- свести будущий публичный API крейта в одну точку входа (как `index.ts`);
- быть единственным местом, где задаётся видимость модулей наружу.

Source compatibility:
- зеркало `test/ag-psd/src/index.ts`.

PORT STATUS: ported — модули объявлены и наполнены; re-export'ы держат публичный
API крейта (read_psd / write_psd + lazy-bitmap хелперы), зеркалируя `index.ts`.
Помимо функций, корень крейта обязан называть все типы из сигнатур публичного
API (`ReadError`, `ReadResult`, `PixelData`) и `DEFAULT_TOTAL_MEMORY_LIMIT`;
это зафиксировано тестом `crate_root_names_the_whole_read_api`.
*/

//! # ag-psd
//!
//! Read and write Adobe Photoshop (`.psd` / `.psb`) files in pure Rust.
//!
//! This crate is a from-scratch Rust port of the
//! [`ag-psd`](https://github.com/Agamnentzar/ag-psd) TypeScript library. The
//! data model mirrors the upstream structures, so anyone familiar with the JS
//! API will recognize [`psd::Psd`], [`psd::Layer`], [`psd::ReadOptions`] and
//! [`psd::WriteOptions`].
//!
//! ## Quick start
//!
//! ```no_run
//! use ag_psd::{read_psd, write_psd};
//! use ag_psd::psd::{ReadOptions, WriteOptions};
//!
//! // Read
//! let bytes = std::fs::read("input.psd").unwrap();
//! let psd = read_psd(&bytes, &ReadOptions::default()).unwrap();
//! println!("{}x{}, {} top-level layers", psd.width, psd.height,
//!          psd.children.as_ref().map_or(0, |c| c.len()));
//!
//! // Write
//! let out = write_psd(&psd, &WriteOptions::default());
//! std::fs::write("output.psd", out).unwrap();
//! ```
//!
//! The most important entry points are [`read_psd`] and [`write_psd`]; the
//! document/layer types live in the [`psd`] module. See the `README.md` and
//! `docs/usage.md` in the repository for a detailed guide.
//!
//! ## Vibe-coded port
//!
//! This port was written almost entirely by **Claude** (Anthropic), using the
//! upstream TypeScript source as the specification and its test fixtures as the
//! oracle. See the README for the full status and known limitations.

// dead_code is expected: not every ported symbol is wired into the public API.
#![allow(dead_code)]

pub mod abr;
pub mod additional_info;
pub mod ase;
pub mod csh;
pub mod descriptor;
pub mod effects_helpers;
pub mod engine_data;
pub mod engine_data2;
pub mod helpers;
pub mod image_resources;
pub mod initialize_canvas;
pub mod jpeg;
pub mod psd;
pub mod reader;
pub mod text;
pub mod utf8;
pub mod writer;

// Публичные re-export'ы (зеркало index.ts) добавляются по мере портирования,
// например `pub use psd::*;`, `pub use abr::*;`, `pub use csh::*;`.
pub use engine_data::{
    parse_engine_data, serialize_engine_data, EngineDataError, EngineValue,
};
pub use engine_data2::decode_engine_data2;
pub use abr::{read_abr, Abr, Brush, BrushShape, ReadAbrOptions, SampleInfo};
pub use ase::{read_ase, write_ase, Ase, AseColor, AseColorValue, AseEntry, AseGroup};
pub use csh::{read_csh, write_csh, Csh, CshShape};
pub use reader::read_psd;
// The read entry point returns `Result<Psd, ReadError>` and the document model
// hands out `PixelData`, so both must be nameable without a module path, as must
// the budget constant a caller needs in order to tune `total_memory_limit`.
pub use psd::{PixelData, DEFAULT_TOTAL_MEMORY_LIMIT};
pub use reader::{ReadError, ReadResult};
// Lazy-bitmap API (upstream `index.ts` exports these next to `readPsd`): with
// `ReadOptions::use_raw_data` the reader keeps compressed channel bytes instead of
// decoding them, and these free functions decode a single layer/mask/composite on
// demand. The upstream canvas variants have no Rust equivalent.
pub use reader::{
    decode_layer_pixels, get_composite_image_data, get_layer_image_data,
    get_layer_mask_image_data, get_layer_real_mask_image_data,
};
pub use writer::{write_psd, write_psd_to_writer};

#[cfg(test)]
mod tests {
    /// Everything the signature of the public read API mentions must be
    /// nameable from the crate root — this test does not compile otherwise.
    #[test]
    fn crate_root_names_the_whole_read_api() {
        let read: fn(&[u8], &crate::psd::ReadOptions) -> crate::ReadResult<crate::psd::Psd> =
            crate::read_psd;
        let _ = read;
        let _: crate::ReadError = crate::ReadError::UnexpectedEndOfBuffer;
        let _: usize = crate::DEFAULT_TOTAL_MEMORY_LIMIT;
        let _: crate::PixelData =
            crate::PixelData { width: 0, height: 0, data: Vec::new() };
    }
}
