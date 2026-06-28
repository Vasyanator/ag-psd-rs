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
  `adjustment_keys.rs`, `misc_keys.rs` — **STUB**, заполняются follow-up
  воркерами.

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

## Добавление нового ключа в группу

1. Найдите ключ в `HANDLERS` (порядок и флаги `four_bytes`/`write_total_length`
   уже заданы — НЕ меняйте порядок).
2. Если ключ должен попасть в вашу группу, но в `HANDLERS` помечен другой
   `Group`, согласуйте смену тега (порядок записи менять нельзя).
3. Реализуйте ветки в `read`/`has`/`write` своего group-модуля.
