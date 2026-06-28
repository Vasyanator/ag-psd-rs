/*
File: crates/ag-psd/tests/fixtures.rs

Purpose:
Интеграционный harness на фикстурах оригинальной библиотеки ag-psd. Проходит по
ВСЕМ найденным `src.psd` и эмпирически измеряет покрытие портированного ридера/
райтера. Это инструмент диагностики, а не byte-exact сверка с `data.json`.

Что проверяется:
- `read_all_fixtures`: каждый `src.psd` читается без ошибки/паники;
- `round_trip_all_fixtures`: read -> write_psd -> read даёт структурно стабильный
  результат (width/height/colorMode, число детей, рекурсивное число слоёв,
  per-layer name/opacity/blendMode/bounds).

Падения собираются по-фикстурно (паники ловятся через catch_unwind), сводки
печатаются в stderr. Тесты ассертят лишь разумный порог успеха через явные
константы — чтобы отчёт был виден без «красного» прогона.

Source compatibility:
- фикстуры: `test/ag-psd/test/**/src.psd`.
*/

use std::collections::BTreeMap;
use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use ag_psd::psd::{ColorMode, Layer, Psd, ReadOptions, WriteOptions};
use ag_psd::reader::ReadError;
use ag_psd::{read_psd, write_psd};

// --- Пороги успеха (легко ужесточить позже) ---------------------------------
/// Минимальная доля фикстур, которые должны читаться без ошибки/паники.
const READ_SUCCESS_THRESHOLD: f64 = 0.50;
/// Минимальная доля читаемых фикстур, переживающих round-trip структурно.
///
/// Текущее реальное значение — 93/100 (93.0%). Оставшиеся 7 расхождений
/// «верны» (faithful): они воспроизводят ограничение upstream-райтера ag-psd,
/// который умеет писать только 8-бит RGB. Эти фикстуры НЕ должны «чиниться»:
///   - read/16bits, read/32bits — bitsPerChannel != 8 (paника write по дизайну);
///   - read/bitmap, read/bitmap-rle — bitmap-режим тоже пишется как !=8 бит;
///   - read/grayscale, read/grayscale-alpha — colorMode Grayscale -> RGB;
///   - read/indexed — colorMode Indexed -> RGB.
/// Порог выставлен чуть ниже достижимого (0.90 < 0.93), чтобы тест ловил
/// регрессии, но не «краснел» из-за известных faithful-ограничений.
const ROUND_TRIP_SUCCESS_THRESHOLD: f64 = 0.90;

/// Корень эталонных фикстур (read-only). `crates/ag-psd` -> корень репо -> test.
fn fixtures_root() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../test/ag-psd/test"))
}

/// Рекурсивно собрать все `src.psd`. Только std::fs, без внешних крейтов.
fn discover_fixtures(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("src.psd") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Человекочитаемое имя фикстуры: путь относительно корня без `/src.psd`.
fn fixture_name(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let s = rel.to_string_lossy();
    s.strip_suffix("/src.psd")
        .or_else(|| s.strip_suffix("\\src.psd"))
        .unwrap_or(&s)
        .to_string()
}

/// Обёртка: поймать панику и привести её к строке сообщения.
fn catch_panic_msg<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    let prev = panic::take_hook();
    panic::set_hook(Box::new(|_| {})); // глушим стандартный вывод паники
    let res = panic::catch_unwind(AssertUnwindSafe(f));
    panic::set_hook(prev);
    res.map_err(|e| {
        if let Some(s) = e.downcast_ref::<&str>() {
            format!("panic: {s}")
        } else if let Some(s) = e.downcast_ref::<String>() {
            format!("panic: {s}")
        } else {
            "panic: <non-string payload>".to_string()
        }
    })
}

fn default_read_options() -> ReadOptions {
    ReadOptions::default()
}

/// Грубая классификация причины падения для группировки в сводке.
fn classify(msg: &str) -> &'static str {
    let m = msg.to_ascii_lowercase();
    if m.starts_with("panic") {
        if m.contains("index out of") || m.contains("range end index") || m.contains("slice") {
            "panic: index/slice out of bounds"
        } else if m.contains("unwrap") || m.contains("none") || m.contains("unwrap_none") {
            "panic: unwrap on None/Err"
        } else if m.contains("overflow") {
            "panic: arithmetic overflow"
        } else if m.contains("utf-8") || m.contains("utf8") {
            "panic: invalid utf-8"
        } else if m.contains("not yet") || m.contains("todo") || m.contains("unimplemented") {
            "panic: unimplemented/todo"
        } else {
            "panic: other"
        }
    } else if m.contains("invalid signature") {
        "read error: invalid signature"
    } else if m.contains("exceeding buffer") || m.contains("exceeds file") || m.contains("past end")
    {
        "read error: buffer/section bounds"
    } else if m.contains("4gb") {
        "read error: size too large"
    } else {
        "read error: other"
    }
}

/// Рекурсивно подсчитать общее число слоёв (включая группы и их детей).
fn count_layers(children: &Option<Vec<Layer>>) -> usize {
    match children {
        None => 0,
        Some(layers) => layers.iter().map(|l| 1 + count_layers(&l.children)).sum(),
    }
}

/// Структурная сверка одного дерева слоёв. Возвращает первое расхождение.
fn compare_layers(a: &[Layer], b: &[Layer], path: &str) -> Result<(), String> {
    if a.len() != b.len() {
        return Err(format!("{path}: children count {} != {}", a.len(), b.len()));
    }
    for (i, (la, lb)) in a.iter().zip(b.iter()).enumerate() {
        let here = format!("{path}[{i}]");
        let na = la.additional_info.name.as_deref();
        let nb = lb.additional_info.name.as_deref();
        if na != nb {
            return Err(format!("{here}: name {na:?} != {nb:?}"));
        }
        if la.opacity != lb.opacity {
            return Err(format!(
                "{here} ({na:?}): opacity {:?} != {:?}",
                la.opacity, lb.opacity
            ));
        }
        if la.blend_mode != lb.blend_mode {
            return Err(format!(
                "{here} ({na:?}): blendMode {:?} != {:?}",
                la.blend_mode, lb.blend_mode
            ));
        }
        let bounds_a = (la.top, la.left, la.bottom, la.right);
        let bounds_b = (lb.top, lb.left, lb.bottom, lb.right);
        if bounds_a != bounds_b {
            return Err(format!("{here} ({na:?}): bounds {bounds_a:?} != {bounds_b:?}"));
        }
        match (&la.children, &lb.children) {
            (Some(ca), Some(cb)) => compare_layers(ca, cb, &here)?,
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                return Err(format!("{here} ({na:?}): children presence differs"));
            }
        }
    }
    Ok(())
}

/// Структурная стабильность двух Psd. Возвращает первое расхождение.
fn compare_psd(a: &Psd, b: &Psd) -> Result<(), String> {
    if a.width != b.width {
        return Err(format!("width {} != {}", a.width, b.width));
    }
    if a.height != b.height {
        return Err(format!("height {} != {}", a.height, b.height));
    }
    let cma: Option<ColorMode> = a.color_mode;
    let cmb: Option<ColorMode> = b.color_mode;
    if cma != cmb {
        return Err(format!("colorMode {cma:?} != {cmb:?}"));
    }
    let ta = count_layers(&a.children);
    let tb = count_layers(&b.children);
    if ta != tb {
        return Err(format!("total layer count {ta} != {tb}"));
    }
    let empty: Vec<Layer> = Vec::new();
    let ca = a.children.as_ref().unwrap_or(&empty);
    let cb = b.children.as_ref().unwrap_or(&empty);
    compare_layers(ca, cb, "root")
}

#[test]
fn read_all_fixtures() {
    let root = fixtures_root();
    // Эталонные фикстуры лежат в монорепе (test/ag-psd/test) и НЕ входят в
    // отдельный git/crates.io-репозиторий крейта. Если их нет — это
    // standalone-сборка: тихо пропускаем (unit-тесты в src/ остаются).
    if !root.is_dir() {
        eprintln!(
            "skipping fixture harness: каталог фикстур не найден ({})",
            root.display()
        );
        return;
    }

    let fixtures = discover_fixtures(&root);
    let total = fixtures.len();
    assert!(total > 0, "не найдено ни одного src.psd под {}", root.display());

    let mut ok = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();

    for path in &fixtures {
        let name = fixture_name(&root, path);
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                failures.push((name, format!("read error: io: {e}")));
                continue;
            }
        };
        let opts = default_read_options();
        let res = catch_panic_msg(|| read_psd(&bytes, &opts));
        match res {
            Ok(Ok(_psd)) => ok += 1,
            Ok(Err(e)) => failures.push((name, format!("read error: {e}"))),
            Err(panic_msg) => failures.push((name, panic_msg)),
        }
    }

    print_read_summary(total, ok, &failures);

    let ratio = ok as f64 / total as f64;
    assert!(
        ratio >= READ_SUCCESS_THRESHOLD,
        "read success rate {:.1}% ниже порога {:.0}% ({ok}/{total})",
        ratio * 100.0,
        READ_SUCCESS_THRESHOLD * 100.0
    );
}

#[test]
fn round_trip_all_fixtures() {
    let root = fixtures_root();
    if !root.is_dir() {
        eprintln!(
            "skipping fixture harness: каталог фикстур не найден ({})",
            root.display()
        );
        return;
    }
    let fixtures = discover_fixtures(&root);
    assert!(!fixtures.is_empty(), "не найдено ни одного src.psd");

    let mut readable = 0usize; // фикстуры, прочитанные на первом проходе
    let mut ok = 0usize; // прошли round-trip структурно
    let mut mismatches: Vec<(String, String)> = Vec::new();

    for path in &fixtures {
        let name = fixture_name(&root, path);
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let opts = default_read_options();

        // Первый проход. Если не читается — это забота read_all_fixtures.
        let first = catch_panic_msg(|| read_psd(&bytes, &opts));
        let psd1 = match first {
            Ok(Ok(p)) => p,
            _ => continue,
        };
        readable += 1;

        // write -> read -> compare.
        let result = catch_panic_msg(|| {
            let wopts = WriteOptions::default();
            let written = write_psd(&psd1, &wopts);
            let psd2 = read_psd(&written, &opts)?;
            Ok::<Psd, ReadError>(psd2)
        });

        match result {
            Ok(Ok(psd2)) => match compare_psd(&psd1, &psd2) {
                Ok(()) => ok += 1,
                Err(div) => mismatches.push((name, format!("structure: {div}"))),
            },
            Ok(Err(e)) => mismatches.push((name, format!("re-read error: {e}"))),
            Err(panic_msg) => mismatches.push((name, panic_msg)),
        }
    }

    print_round_trip_summary(readable, ok, &mismatches);

    if readable == 0 {
        panic!("ни одна фикстура не прочиталась — нечего round-trip'ить");
    }
    let ratio = ok as f64 / readable as f64;
    assert!(
        ratio >= ROUND_TRIP_SUCCESS_THRESHOLD,
        "round-trip success rate {:.1}% ниже порога {:.0}% ({ok}/{readable})",
        ratio * 100.0,
        ROUND_TRIP_SUCCESS_THRESHOLD * 100.0
    );
}

fn print_read_summary(total: usize, ok: usize, failures: &[(String, String)]) {
    eprintln!("\n================ READ SWEEP SUMMARY ================");
    eprintln!("total fixtures : {total}");
    eprintln!("read OK        : {ok}");
    eprintln!("read FAILED    : {}", failures.len());
    if total > 0 {
        eprintln!("success rate   : {:.1}%", ok as f64 / total as f64 * 100.0);
    }

    if !failures.is_empty() {
        let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
        for (_, msg) in failures {
            *by_kind.entry(classify(msg)).or_default() += 1;
        }
        eprintln!("\n-- failures grouped by kind --");
        for (kind, n) in &by_kind {
            eprintln!("  {n:>3}  {kind}");
        }
        eprintln!("\n-- failures (fixture -> error) --");
        for (name, msg) in failures {
            eprintln!("  {name}\n       {msg}");
        }
    }
    eprintln!("====================================================\n");
}

fn print_round_trip_summary(readable: usize, ok: usize, mismatches: &[(String, String)]) {
    eprintln!("\n============= ROUND-TRIP SUMMARY =============");
    eprintln!("readable fixtures : {readable}");
    eprintln!("round-trip OK     : {ok}");
    eprintln!("round-trip BAD    : {}", mismatches.len());
    if readable > 0 {
        eprintln!(
            "stability rate    : {:.1}%",
            ok as f64 / readable as f64 * 100.0
        );
    }

    if !mismatches.is_empty() {
        let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
        for (_, msg) in mismatches {
            *by_kind.entry(classify(msg)).or_default() += 1;
        }
        eprintln!("\n-- mismatches grouped by kind --");
        for (kind, n) in &by_kind {
            eprintln!("  {n:>3}  {kind}");
        }
        eprintln!("\n-- mismatches (fixture -> first divergence) --");
        for (name, msg) in mismatches {
            eprintln!("  {name}\n       {msg}");
        }
    }
    eprintln!("=============================================\n");
}
