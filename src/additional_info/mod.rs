/*
File: crates/ag-psd/src/additional_info/mod.rs

Purpose:
Дополнительная информация слоёв PSD (8BIM/8B64-секции "additional layer
information"). Порт upstream-файла `test/ag-psd/src/additionalInfo.ts`.

DELIBERATE DIVERGENCE FROM UPSTREAM LAYOUT:
upstream держит всё в одном файле `additionalInfo.ts` (~5445 строк). Здесь это
осознанно разбито на DIRECTORY MODULE: `mod.rs` (фреймворк + диспетчер) плюс
по одному файлу на логическую группу ключей (`metadata_keys.rs`,
`text_keys.rs`, ...). Причина — размер файла и то, что группы будут
заполняться НЕСКОЛЬКИМИ параллельными воркерами независимо друг от друга.
mod.rs владеет каноническим порядком ключей (важен для записи — Photoshop
пишет ключи в порядке регистрации хэндлеров, а не в порядке данных) и
маршрутизирует чтение/запись в group-модули.

Main responsibilities:
- CANONICAL ORDERED key registry (зеркало порядка вызовов `addHandler`);
- публичная точка входа чтения `read_additional_info_key`;
- публичная точка входа записи `write_additional_info`;
- GROUP-MODULE CONTRACT, который реализует каждый group-модуль.

Source compatibility:
- зеркало `test/ag-psd/src/additionalInfo.ts` (инфраструктура `infoHandlers` /
  `infoHandlersMap`, `addHandler` / `addHandlerAlias`, framing в
  `readAdditionalLayerInfo` / `writeAdditionalLayerInfo`).
*/

use crate::helpers::LARGE_ADDITIONAL_INFO_KEYS;
use crate::psd::{LayerAdditionalInfo, ReadOptions, WriteOptions};
use crate::reader::{PsdReader, ReadResult};
use crate::writer::{write_section, write_signature, PsdWriter};

pub mod metadata_keys;

// Stub group modules (filled by follow-up parallel workers; see GROUP-MODULE
// CONTRACT below). They currently report "not mine" for every key.
pub mod adjustment_keys;
pub mod effects_keys;
pub mod misc_keys;
pub mod smart_object_keys;
pub mod text_keys;
pub mod vector_keys;

// ===========================================================================
// Group tags
// ===========================================================================

/// Логическая группа ключа. Диспетчер маршрутизирует чтение/запись в
/// соответствующий group-модуль по этому тегу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Простые скалярные/строковые/enum-ключи (реализовано: `metadata_keys.rs`).
    Metadata,
    /// Текстовые слои (`TySh`, `Txt2`) — `text_keys.rs`.
    Text,
    /// Эффекты слоя (`lmfx`, `lrFX`, `lfxs`, `lfx2`) — `effects_keys.rs`.
    Effects,
    /// Smart objects / linked / placed (`PlLd`, `SoLd`, `SoLE`, `lnk2`, `lnkE`,
    /// `PxSc`, `Patt`, ...) — `smart_object_keys.rs`.
    SmartObject,
    /// Векторные маски / заливки / штрихи (`vmsk`, `vsms`, `vscg`, `vstk`,
    /// `vogk`, `vowv`, `SoCo`, `GdFl`, `PtFl`, `pths`) — `vector_keys.rs`.
    Vector,
    /// Корректирующие слои (`brit`, `levl`, `curv`, ...) — `adjustment_keys.rs`.
    Adjustment,
    /// Всё прочее, что не вписалось в группы выше (`Lr16`, `Lr32`, `LMsk`,
    /// `FMsk`, `FEid`, `Anno`, `shmd`, `artb`, `artd`, `cinf`, `extn`, `CAI `,
    /// `OCIO`, `GenI`, `sn2P`, `lfxs`-сосед, ...) — `misc_keys.rs`.
    Misc,
}

// ===========================================================================
// CANONICAL ORDERED key registry
// ===========================================================================

/// Запись канонического реестра ключей.
#[derive(Debug, Clone, Copy)]
pub struct KeyHandler {
    /// 4-символьный ключ секции (как в файле; например `"luni"`, `"CAI "`).
    pub key: &'static str,
    /// Логическая группа (определяет group-модуль).
    pub group: Group,
    /// `writeSection` round для записи: некоторые ключи пишут 4-байтовую
    /// длину секции (round=4) вместо 2-байтовой (round=2). Зеркало `fourBytes`
    /// в `writeAdditionalLayerInfo`.
    pub four_bytes: bool,
    /// `writeTotalLength` для `writeSection` — зеркало одноимённого флага.
    /// Для большинства ключей `true`; `false` для `Txt2`/`cinf`/`extn`/`CAI `/`OCIO`.
    pub write_total_length: bool,
}

const fn h(key: &'static str, group: Group) -> KeyHandler {
    KeyHandler { key, group, four_bytes: false, write_total_length: true }
}

const fn h4(key: &'static str, group: Group) -> KeyHandler {
    KeyHandler { key, group, four_bytes: true, write_total_length: true }
}

const fn h4_no_total(key: &'static str, group: Group) -> KeyHandler {
    KeyHandler { key, group, four_bytes: true, write_total_length: false }
}

const fn h_no_total(key: &'static str, group: Group) -> KeyHandler {
    KeyHandler { key, group, four_bytes: false, write_total_length: false }
}

/// CANONICAL ORDERED registry — ТОТ ЖЕ ПОРЯДОК, что и вызовы `addHandler` в
/// upstream (важно для записи: PS пишет ключи в порядке регистрации хэндлеров).
///
/// Алиасы (`addHandlerAlias`) НЕ добавляют новых записей в порядок записи —
/// они лишь делают ключ распознаваемым при чтении (см. [`alias_target`]).
///
/// `fourBytes` / `writeTotalLength` повторяют флаги из upstream
/// `writeAdditionalLayerInfo`.
pub const HANDLERS: &[KeyHandler] = &[
    h("TySh", Group::Text), // NOT in fourBytes list upstream
    h("SoCo", Group::Vector),
    h4("GdFl", Group::Vector),
    h("PtFl", Group::Vector),
    h4("vscg", Group::Vector),
    h4("vmsk", Group::Vector),
    h("vowv", Group::Vector), // NOT in fourBytes list upstream
    h4("vogk", Group::Vector),
    h4("lmfx", Group::Effects),
    h4("lrFX", Group::Effects),
    h4("luni", Group::Metadata),
    h("lnsr", Group::Metadata),
    h("lyid", Group::Metadata),
    h("lsct", Group::Metadata),
    h("clbl", Group::Metadata),
    h("infx", Group::Metadata),
    h("knko", Group::Metadata),
    h("lmgm", Group::Metadata),
    h("lspf", Group::Metadata),
    h("lclr", Group::Metadata),
    h("shmd", Group::Misc),
    h("PxSc", Group::SmartObject),
    h("vstk", Group::Vector),
    h4("artb", Group::Misc),
    h("sn2P", Group::Misc),
    h4("PlLd", Group::SmartObject),
    h4("SoLd", Group::SmartObject),
    h("fxrp", Group::Metadata),
    h("Lr16", Group::Misc),
    h("Lr32", Group::Misc),
    h("LMsk", Group::Misc),
    h("Patt", Group::SmartObject),
    h("Patt", Group::SmartObject), // second Patt handler (upstream registers twice)
    h4_no_total("CAI ", Group::Misc),
    h4_no_total("CAI ", Group::Misc), // second CAI handler
    h4_no_total("OCIO", Group::Misc),
    h4("GenI", Group::Misc),
    h4("Anno", Group::Misc),
    h4("lnk2", Group::SmartObject), // createLnkHandler('lnk2') (in fourBytes list)
    h("lnkE", Group::SmartObject), // createLnkHandler('lnkE') (NOT in fourBytes list upstream)
    h("pths", Group::Vector),
    h("lyvr", Group::Metadata),
    h("lfxs", Group::Effects),
    h("brit", Group::Adjustment),
    h("levl", Group::Adjustment),
    h4("curv", Group::Adjustment),
    h("expA", Group::Adjustment),
    h4("vibA", Group::Adjustment),
    h("hue2", Group::Adjustment),
    h("blnc", Group::Adjustment),
    h4("blwh", Group::Adjustment),
    h("phfl", Group::Adjustment),
    h("mixr", Group::Adjustment),
    h("clrL", Group::Adjustment),
    h("nvrt", Group::Adjustment),
    h("post", Group::Adjustment),
    h("thrs", Group::Adjustment),
    h4("grdm", Group::Adjustment),
    h("selc", Group::Adjustment),
    h4("CgEd", Group::Adjustment),
    h4_no_total("Txt2", Group::Text), // four_bytes + writeTotalLength=false
    h4("FEid", Group::Misc),
    h("FMsk", Group::Misc),
    h("artd", Group::Misc),
    h("lfx2", Group::Effects),
    h4_no_total("cinf", Group::Misc),
    h_no_total("extn", Group::Misc),
    h("iOpa", Group::Metadata),
    h("brst", Group::Metadata),
    h("tsly", Group::Metadata),
];

/// Алиасы чтения (`addHandlerAlias(key, target)`): ключ в файле → ключ-цель,
/// чей хэндлер обрабатывает данные. Запись по этим ключам НЕ производится.
pub const ALIASES: &[(&str, &str)] = &[
    ("vsms", "vmsk"),
    ("vmsk", "vsms"), // upstream registers both directions
    ("lsdk", "lsct"),
    ("SoLE", "SoLd"),
    ("Pat2", "Patt"),
    ("Pat3", "Patt"),
    ("lnkD", "lnk2"),
    ("lnk3", "lnk2"),
    ("FXid", "FEid"),
];

/// Возвращает ключ-цель для алиаса (или сам ключ, если не алиас).
///
/// Используется только при ЧТЕНИИ для маршрутизации: данные ключа-алиаса
/// читаются хэндлером ключа-цели. ВНИМАНИЕ: при чтении `lsdk`/`lsct` группа
/// определяется по ключу-цели, но фактический ключ передаётся в group-модуль
/// без изменений (group-модули, обрабатывающие алиасы, должны учитывать оба).
pub fn alias_target(key: &str) -> &str {
    for (alias, target) in ALIASES {
        if *alias == key {
            return target;
        }
    }
    key
}

/// Группа, к которой относится ключ (учитывая алиасы). `None`, если ключ
/// неизвестен.
pub fn group_for_key(key: &str) -> Option<Group> {
    let target = alias_target(key);
    HANDLERS.iter().find(|h| h.key == target).map(|h| h.group)
}

/// Использует ли ключ "большой" (8-байтовый) размер секции. Зеркало
/// `largeAdditionalInfoKeys.indexOf(key) !== -1`.
pub fn is_large_key(key: &str) -> bool {
    LARGE_ADDITIONAL_INFO_KEYS.contains(&key)
}

// ===========================================================================
// Context structs (orchestration glue)
// ===========================================================================

/// Контекст чтения, прокидываемый в group-модули. Зеркало хвостовых аргументов
/// upstream `ReadMethod` (`psd`, `imageResources`) плюс опции ридера.
///
/// Поля сделаны опциональными ссылками, чтобы метаданные-группа (которой
/// контекст не нужен) могла вызываться и без полностью собранного `Psd`.
/// Группы, которым нужен `psd`/ресурсы (smart objects, linked files), будут
/// требовать соответствующие поля — это часть GROUP-MODULE CONTRACT.
pub struct ReadCtx<'a> {
    pub options: &'a ReadOptions,
    pub large: bool,
}

/// Контекст записи. Зеркало `ExtendedWriteOptions` (опции + `layerIds` /
/// `layerToId` для дедупликации id в `lyid`).
pub struct WriteCtx<'a> {
    pub options: &'a WriteOptions,
    pub psb: bool,
    /// уже использованные id слоёв (см. `lyid`-хэндлер: дубликаты сдвигаются +100).
    pub layer_ids: std::collections::HashSet<u32>,
}

impl<'a> WriteCtx<'a> {
    pub fn new(options: &'a WriteOptions, psb: bool) -> Self {
        WriteCtx { options, psb, layer_ids: std::collections::HashSet::new() }
    }
}

// ===========================================================================
// PUBLIC READ ENTRY POINT
// ===========================================================================

/// Прочитать тело одного additional-info ключа В ПРЕДЕЛАХ уже открытой секции.
///
/// Зеркало внутренностей `readAdditionalLayerInfo`: вызывающая оркестрация
/// (`readAdditionalLayerInfo` в reader.rs, ещё не портирована) сама читает
/// подпись `8BIM`/`8B64` и открывает `readSection`, затем зовёт эту функцию
/// с `key` и замыканием `left` (сколько байт осталось в секции).
///
/// Возвращает `Ok(true)`, если ключ распознан и обработан; `Ok(false)`, если
/// ключ неизвестен (вызывающий должен `skip_bytes(reader, left())`).
///
/// СТАБИЛЬНАЯ СИГНАТУРА — на неё опирается оркестрация и group-модули.
pub fn read_additional_info_key(
    key: &str,
    reader: &mut PsdReader,
    info: &mut LayerAdditionalInfo,
    left: &dyn Fn(&PsdReader) -> usize,
    ctx: &mut ReadCtx,
) -> ReadResult<bool> {
    let group = match group_for_key(key) {
        Some(g) => g,
        None => return Ok(false),
    };

    let result = match group {
        Group::Metadata => metadata_keys::read(key, reader, info, left, ctx)?,
        Group::Text => text_keys::read(key, reader, info, left, ctx)?,
        Group::Effects => effects_keys::read(key, reader, info, left, ctx)?,
        Group::SmartObject => smart_object_keys::read(key, reader, info, left, ctx)?,
        Group::Vector => vector_keys::read(key, reader, info, left, ctx)?,
        Group::Adjustment => adjustment_keys::read(key, reader, info, left, ctx)?,
        Group::Misc => misc_keys::read(key, reader, info, left, ctx)?,
    };

    Ok(result.is_some())
}

// ===========================================================================
// PUBLIC WRITE ENTRY POINT
// ===========================================================================

/// Записать все additional-info секции слоя в каноническом порядке.
///
/// Зеркало `writeAdditionalLayerInfo`: итерирует [`HANDLERS`] по порядку,
/// для каждого ключа спрашивает у его group-модуля `has(key, info)`; если
/// `Some(true)` — пишет подпись (`8BIM`/`8B64`) + ключ + секцию, делегируя
/// тело записи `write(key, ...)` group-модуля.
///
/// Особые случаи из upstream:
/// - `Txt2` пропускается при `options.invalidate_text_layers`;
/// - `vmsk` при `psb` пишется как `vsms`;
/// - `large` (8B64 / 8-байтовая длина) включается при `psb && is_large_key`.
///
/// СТАБИЛЬНАЯ СИГНАТУРА.
pub fn write_additional_info(
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    ctx: &mut WriteCtx,
) {
    for handler in HANDLERS {
        let mut key = handler.key;

        // upstream: skip Txt2 when invalidating text layers.
        if key == "Txt2" && ctx.options.invalidate_text_layers == Some(true) {
            continue;
        }
        // upstream: vmsk -> vsms in PSB.
        if key == "vmsk" && ctx.psb {
            key = "vsms";
        }

        let has = group_has(handler.group, key, info);
        if has != Some(true) {
            continue;
        }

        let large = ctx.psb && is_large_key(key);
        let round = if handler.four_bytes { 4 } else { 2 };

        write_signature(writer, if large { "8B64" } else { "8BIM" });
        write_signature(writer, key);
        let write_total_length = handler.write_total_length;
        write_section(
            writer,
            round,
            |w| {
                // Ошибки записи в Rust-порте отражаются как паника/в group-модуле;
                // write-контракт group-модуля не возвращает Result (см. контракт).
                group_write(handler.group, key, w, info, ctx);
            },
            write_total_length,
            large,
        );
    }
}

// --- internal routing helpers ---------------------------------------------

fn group_has(group: Group, key: &str, info: &LayerAdditionalInfo) -> Option<bool> {
    match group {
        Group::Metadata => metadata_keys::has(key, info),
        Group::Text => text_keys::has(key, info),
        Group::Effects => effects_keys::has(key, info),
        Group::SmartObject => smart_object_keys::has(key, info),
        Group::Vector => vector_keys::has(key, info),
        Group::Adjustment => adjustment_keys::has(key, info),
        Group::Misc => misc_keys::has(key, info),
    }
}

fn group_write(
    group: Group,
    key: &str,
    writer: &mut PsdWriter,
    info: &LayerAdditionalInfo,
    ctx: &mut WriteCtx,
) {
    let handled = match group {
        Group::Metadata => metadata_keys::write(key, writer, info, ctx),
        Group::Text => text_keys::write(key, writer, info, ctx),
        Group::Effects => effects_keys::write(key, writer, info, ctx),
        Group::SmartObject => smart_object_keys::write(key, writer, info, ctx),
        Group::Vector => vector_keys::write(key, writer, info, ctx),
        Group::Adjustment => adjustment_keys::write(key, writer, info, ctx),
        Group::Misc => misc_keys::write(key, writer, info, ctx),
    };
    debug_assert!(handled.is_some(), "group did not handle write for key {key}");
}
