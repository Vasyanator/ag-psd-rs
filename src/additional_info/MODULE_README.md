# additional_info — additional layer information (8BIM/8B64 sections)

Порт upstream `test/ag-psd/src/additionalInfo.ts` (~5445 строк).

## Осознанное расхождение с upstream

Upstream — один файл. Здесь — **directory module**: размер файла плюс
параллельное портирование несколькими воркерами требуют разбиения. `mod.rs`
владеет каркасом и каноническим порядком ключей; каждая логическая группа
ключей живёт в своём файле и реализует общий GROUP-MODULE CONTRACT.

## Файлы

- `mod.rs` — каркас: `HANDLERS` (CANONICAL ORDERED registry), `ALIASES`,
  `Group`, контекст-структуры `ReadCtx`/`WriteCtx`, публичные точки входа
  `read_additional_info_key` / `write_additional_info`, маршрутизация в группы.
- `metadata_keys.rs` — **REFERENCE IMPLEMENTATION**: простые scalar/string/enum
  ключи (полностью рабочие + тесты round-trip).
- `text_keys.rs`, `effects_keys.rs`, `smart_object_keys.rs`, `vector_keys.rs`,
  `adjustment_keys.rs`, `misc_keys.rs` — ключи своих групп (read + has + write);
  локальные ограничения и пробелы описаны в шапке каждого файла.

## Shared primitives (do not fork them)

Pattern records (`Patt`/`Pat2`/`Pat3` in `smart_object_keys.rs`) are decoded by
`crate::reader::read_pattern` — the crate's single implementation of the
primitive, shared with `crate::abr` (the `patt` section). It owns the rectangle
validation (`check_box_size`) and the `ReadOptions::total_memory_limit`
accounting, so a local copy here would silently drop both guards on the live
document path, which is exactly the path a hostile file arrives on. Write side:
`crate::writer::write_pattern`, likewise single. Both are `pub` in their module
for this reason: call them, do not re-implement them.

## GROUP-MODULE CONTRACT

Каждый group-модуль экспортирует ровно три функции:

```rust
pub fn read(
    key: &str,
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
    ctx: &mut ReadCtx,
) -> ReadResult<Option<()>>;   // Ok(Some(())) обработан; Ok(None) "не мой ключ"

pub fn has(key: &str, info: &LayerAdditionalInfo) -> Option<bool>;
//   Some(true)  владею ключом и его надо писать
//   Some(false) владею ключом, но писать не надо
//   None        "не мой ключ"

pub fn write(
    key: &str,
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    ctx: &mut WriteCtx,
) -> Option<ReadResult<()>>;   // Some(Ok(())) записан; None "не мой ключ"
```

Правила:

1. `read`/`write`/`has` ветвятся по `key` (group-модуль может владеть несколькими
   ключами; учитывайте алиасы, см. `ALIASES` в mod.rs — например `lsct`/`lsdk`).
2. `write` вызывается ТОЛЬКО когда `has(key, info) == Some(true)` и УЖЕ внутри
   открытой `write_section` (подпись `8BIM`/`8B64`, ключ и framing пишет
   диспетчер по флагам из `HANDLERS`). Тело пишет только полезную нагрузку.
3. `read` вызывается УЖЕ внутри открытой `read_section`; используйте `left`
   (остаток байт) точно как upstream; хвост секции диспетчер не доскипывает
   за вас внутри group-модуля — это делает внешняя оркестрация после возврата.
4. Big-endian; точные padding/framing; `throw` → `ReadError`.

## Exception to the contract: `Lr16` / `Lr32`

These two keys are the only registry entries whose read body does **not** live in
the owning group module. Their payload is a complete nested layer-info block —
the layer records of a 16/32-bit document, which Photoshop puts here instead of
in the ordinary `Layr` section — so reading it means running the whole
`read_layer_info` pipeline, which needs the whole `Psd`. A group module only ever
receives a `LayerAdditionalInfo`, so the recursion happens one level up, in
`crate::reader::read_additional_layer_info`, which routes the key before the
group dispatch is reached.

Consequences to respect when editing:

- `misc_keys.rs` still owns the keys in `HANDLERS` (`Group::Misc`) and keeps
  their `has`/`write` (write is a no-op, as upstream). Its `read` branch is
  reachable only for the case that has no document to attach layers to — an
  `Lr16`/`Lr32` nested inside a *layer* — and returns an error there rather than
  consuming the body and losing the layers silently.
- Errors from the nested read are **not** swallowed by the per-key
  `try`/`catch`-equivalent in `read_additional_layer_info`; they propagate. This
  is a deliberate divergence from upstream (recorded in `CHANGELOG.md`): a
  rejected rectangle or an exhausted memory budget must not degrade into a
  document that reads "successfully" with no layers.
- `read_layer_info` may therefore run twice for one document. It merges into the
  existing `psd.children` instead of replacing it, mirroring upstream's `unshift`.

## Добавление нового ключа в группу

1. Найдите ключ в `HANDLERS` (порядок и флаги `four_bytes`/`write_total_length`
   уже заданы — НЕ меняйте порядок).
2. Если ключ должен попасть в вашу группу, но в `HANDLERS` помечен другой
   `Group`, согласуйте смену тега (порядок записи менять нельзя).
3. Реализуйте ветки в `read`/`has`/`write` своего group-модуля.
