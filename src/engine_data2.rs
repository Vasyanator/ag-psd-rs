/*
File: crates/ag-psd/src/engine_data2.rs

Purpose:
вариант Engine Data v2 (альтернативное/структурное представление текстовых данных).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/engineData2.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.

Mapping TS -> Rust:
- `decodeEngineData2(data)` -> `decode_engine_data2(data: &EngineValue) -> EngineValue`
- внутренняя `decodeObj`     -> `decode_obj(obj, keys) -> EngineValue`
- словари переименования ключей (`keysRoot`, `keysStyleSheet`, ...) -> статически
  построенные `KeysDict` (lazy через `OnceLock`).

Модель:
Upstream `decodeObj` принимает нетипизированный распарсенный EngineData (числовые ключи)
и переименовывает ключи по словарю `KeysDict`, опционально «поднимая» (uproot) вложенный
узел на уровень выше и проставляя `_type` из ключа `99`. Здесь вход и выход —
`crate::engine_data::EngineValue`; результат содержит переименованные `Dict`-ключи,
а `_type` записывается как обычный ключ `"_type"`.

TS-тип `GlobalEngineData` — это loose-структура (`any`/TODO в upstream'е), поэтому
типобезопасной обёртки нет: `decode_engine_data2` возвращает `EngineValue`.
*/

use crate::engine_data::EngineValue;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Запись в словаре переименования ключей (зеркало `KeysDict[key]`).
#[derive(Debug, Clone, Default)]
pub struct KeyEntry {
    pub name: Option<&'static str>,
    pub uproot: bool,
    pub children: Option<KeysDict>,
}

/// Словарь переименования: ключ EngineData -> правило (`KeysDict`).
pub type KeysDict = HashMap<&'static str, KeyEntry>;

fn entry_name(name: &'static str) -> KeyEntry {
    KeyEntry { name: Some(name), uproot: false, children: None }
}

fn entry(name: Option<&'static str>, uproot: bool, children: Option<KeysDict>) -> KeyEntry {
    KeyEntry { name, uproot, children }
}

fn dict(pairs: Vec<(&'static str, KeyEntry)>) -> KeysDict {
    pairs.into_iter().collect()
}

fn keys_color() -> KeysDict {
    dict(vec![(
        "0",
        entry(
            None,
            true,
            Some(dict(vec![
                ("0", entry_name("Type")),
                ("1", entry_name("Values")),
            ])),
        ),
    )])
}

fn keys_style_sheet() -> KeysDict {
    dict(vec![
        ("0", entry_name("Font")),
        ("1", entry_name("FontSize")),
        ("2", entry_name("FauxBold")),
        ("3", entry_name("FauxItalic")),
        ("4", entry_name("AutoLeading")),
        ("5", entry_name("Leading")),
        ("6", entry_name("HorizontalScale")),
        ("7", entry_name("VerticalScale")),
        ("8", entry_name("Tracking")),
        ("9", entry_name("BaselineShift")),
        ("11", entry_name("Kerning?")),
        ("12", entry_name("FontCaps")),
        ("13", entry_name("FontBaseline")),
        ("15", entry_name("Strikethrough?")),
        ("16", entry_name("Underline?")),
        ("18", entry_name("Ligatures")),
        ("19", entry_name("DLigatures")),
        ("23", entry_name("Fractions")),
        ("24", entry_name("Ordinals")),
        ("28", entry_name("StylisticAlternates")),
        ("30", entry_name("OldStyle?")),
        ("35", entry_name("BaselineDirection")),
        ("38", entry_name("Language")),
        ("52", entry_name("NoBreak")),
        ("53", entry(Some("FillColor"), false, Some(keys_color()))),
        ("54", entry(Some("StrokeColor"), false, Some(keys_color()))),
        (
            "55",
            entry(
                None,
                false,
                Some(dict(vec![("99", entry(None, true, None))])),
            ),
        ),
        ("79", entry(None, false, Some(keys_color()))),
    ])
}

fn keys_paragraph() -> KeysDict {
    dict(vec![
        ("0", entry_name("Justification")),
        ("1", entry_name("FirstLineIndent")),
        ("2", entry_name("StartIndent")),
        ("3", entry_name("EndIndent")),
        ("4", entry_name("SpaceBefore")),
        ("5", entry_name("SpaceAfter")),
        ("7", entry_name("AutoLeading")),
        ("9", entry_name("AutoHyphenate")),
        ("10", entry_name("HyphenatedWordSize")),
        ("11", entry_name("PreHyphen")),
        ("12", entry_name("PostHyphen")),
        ("13", entry_name("ConsecutiveHyphens?")),
        ("14", entry_name("Zone")),
        ("15", entry_name("HypenateCapitalizedWords")),
        ("17", entry_name("WordSpacing")),
        ("18", entry_name("LetterSpacing")),
        ("19", entry_name("GlyphSpacing")),
        ("32", entry(Some("StyleSheet"), false, Some(keys_style_sheet()))),
    ])
}

fn keys_style_sheet_data() -> KeyEntry {
    entry(Some("StyleSheetData"), false, Some(keys_style_sheet()))
}

fn keys_root() -> KeysDict {
    dict(vec![
        (
            "0",
            entry(
                Some("ResourceDict"),
                false,
                Some(dict(vec![
                    (
                        "1",
                        entry(
                            Some("FontSet"),
                            false,
                            Some(dict(vec![(
                                "0",
                                entry(
                                    None,
                                    true,
                                    Some(dict(vec![(
                                        "0",
                                        entry(
                                            None,
                                            true,
                                            Some(dict(vec![(
                                                "0",
                                                entry(
                                                    None,
                                                    true,
                                                    Some(dict(vec![
                                                        ("0", entry_name("Name")),
                                                        ("2", entry_name("FontType")),
                                                    ])),
                                                ),
                                            )])),
                                        ),
                                    )])),
                                ),
                            )])),
                        ),
                    ),
                    ("2", entry(Some("2"), false, Some(dict(vec![])))),
                    (
                        "3",
                        entry(
                            Some("MojiKumiSet"),
                            false,
                            Some(dict(vec![(
                                "0",
                                entry(
                                    None,
                                    true,
                                    Some(dict(vec![(
                                        "0",
                                        entry(
                                            None,
                                            true,
                                            Some(dict(vec![("0", entry_name("InternalName"))])),
                                        ),
                                    )])),
                                ),
                            )])),
                        ),
                    ),
                    (
                        "4",
                        entry(
                            Some("KinsokuSet"),
                            false,
                            Some(dict(vec![(
                                "0",
                                entry(
                                    None,
                                    true,
                                    Some(dict(vec![(
                                        "0",
                                        entry(
                                            None,
                                            true,
                                            Some(dict(vec![
                                                ("0", entry_name("Name")),
                                                (
                                                    "5",
                                                    entry(
                                                        None,
                                                        true,
                                                        Some(dict(vec![
                                                            ("0", entry_name("NoStart")),
                                                            ("1", entry_name("NoEnd")),
                                                            ("2", entry_name("Keep")),
                                                            ("3", entry_name("Hanging")),
                                                            ("4", entry_name("Name")),
                                                        ])),
                                                    ),
                                                ),
                                            ])),
                                        ),
                                    )])),
                                ),
                            )])),
                        ),
                    ),
                    (
                        "5",
                        entry(
                            Some("StyleSheetSet"),
                            false,
                            Some(dict(vec![(
                                "0",
                                entry(
                                    None,
                                    true,
                                    Some(dict(vec![(
                                        "0",
                                        entry(
                                            None,
                                            true,
                                            Some(dict(vec![
                                                ("0", entry_name("Name")),
                                                ("6", keys_style_sheet_data()),
                                            ])),
                                        ),
                                    )])),
                                ),
                            )])),
                        ),
                    ),
                    (
                        "6",
                        entry(
                            Some("ParagraphSheetSet"),
                            false,
                            Some(dict(vec![(
                                "0",
                                entry(
                                    None,
                                    true,
                                    Some(dict(vec![(
                                        "0",
                                        entry(
                                            None,
                                            true,
                                            Some(dict(vec![
                                                ("0", entry_name("Name")),
                                                (
                                                    "5",
                                                    entry(
                                                        Some("Properties"),
                                                        false,
                                                        Some(keys_paragraph()),
                                                    ),
                                                ),
                                                ("6", entry_name("DefaultStyleSheet")),
                                            ])),
                                        ),
                                    )])),
                                ),
                            )])),
                        ),
                    ),
                    (
                        "8",
                        entry(
                            Some("TextFrameSet"),
                            false,
                            Some(dict(vec![(
                                "0",
                                entry(
                                    None,
                                    true,
                                    Some(dict(vec![(
                                        "0",
                                        entry(
                                            Some("path"),
                                            false,
                                            Some(dict(vec![
                                                ("0", entry_name("name")),
                                                (
                                                    "1",
                                                    entry(
                                                        Some("bezierCurve"),
                                                        false,
                                                        Some(dict(vec![(
                                                            "0",
                                                            entry_name("controlPoints"),
                                                        )])),
                                                    ),
                                                ),
                                                (
                                                    "2",
                                                    entry(
                                                        Some("data"),
                                                        false,
                                                        Some(dict(vec![
                                                            ("0", entry_name("type")),
                                                            ("1", entry_name("orientation")),
                                                            ("2", entry_name("frameMatrix")),
                                                            ("4", entry_name("4")),
                                                            ("6", entry_name("textRange")),
                                                            ("7", entry_name("rowGutter")),
                                                            ("8", entry_name("columnGutter")),
                                                            ("9", entry_name("9")),
                                                            (
                                                                "10",
                                                                entry(
                                                                    Some("baselineAlignment"),
                                                                    false,
                                                                    Some(dict(vec![
                                                                        ("0", entry_name("flag")),
                                                                        ("1", entry_name("min")),
                                                                    ])),
                                                                ),
                                                            ),
                                                            (
                                                                "11",
                                                                entry(
                                                                    Some("pathData"),
                                                                    false,
                                                                    Some(dict(vec![
                                                                        ("1", entry_name("1")),
                                                                        ("0", entry_name("reversed")),
                                                                        ("2", entry_name("2")),
                                                                        ("3", entry_name("3")),
                                                                        ("4", entry_name("spacing")),
                                                                        ("5", entry_name("5")),
                                                                        ("6", entry_name("6")),
                                                                        ("7", entry_name("7")),
                                                                        ("18", entry_name("18")),
                                                                    ])),
                                                                ),
                                                            ),
                                                            ("12", entry_name("12")),
                                                            ("13", entry_name("13")),
                                                        ])),
                                                    ),
                                                ),
                                                ("3", entry_name("3")),
                                                ("97", entry_name("uuid")),
                                            ])),
                                        ),
                                    )])),
                                ),
                            )])),
                        ),
                    ),
                    (
                        "9",
                        entry(
                            Some("Predefined"),
                            false,
                            Some(dict(vec![
                                (
                                    "0",
                                    entry(
                                        None,
                                        false,
                                        Some(dict(vec![("0", entry(None, true, None))])),
                                    ),
                                ),
                                (
                                    "1",
                                    entry(
                                        None,
                                        false,
                                        Some(dict(vec![("0", entry(None, true, None))])),
                                    ),
                                ),
                            ])),
                        ),
                    ),
                ])),
            ),
        ),
        (
            "1",
            entry(
                Some("EngineDict"),
                false,
                Some(dict(vec![
                    (
                        "0",
                        entry(
                            Some("0"),
                            false,
                            Some(dict(vec![
                                ("3", entry_name("SuperscriptSize")),
                                ("4", entry_name("SuperscriptPosition")),
                                ("5", entry_name("SubscriptSize")),
                                ("6", entry_name("SubscriptPosition")),
                                ("7", entry_name("SmallCapSize")),
                                ("8", entry_name("UseFractionalGlyphWidths")),
                                (
                                    "15",
                                    entry(
                                        None,
                                        false,
                                        Some(dict(vec![("0", entry(None, true, None))])),
                                    ),
                                ),
                            ])),
                        ),
                    ),
                    (
                        "1",
                        entry(
                            Some("Editors?"),
                            false,
                            Some(dict(vec![
                                (
                                    "0",
                                    entry(
                                        Some("Editor"),
                                        false,
                                        Some(dict(vec![
                                            ("0", entry_name("Text")),
                                            (
                                                "5",
                                                entry(
                                                    Some("ParagraphRun"),
                                                    false,
                                                    Some(dict(vec![(
                                                        "0",
                                                        entry(
                                                            Some("RunArray"),
                                                            false,
                                                            Some(dict(vec![
                                                                (
                                                                    "0",
                                                                    entry(
                                                                        Some("ParagraphSheet"),
                                                                        false,
                                                                        Some(dict(vec![(
                                                                            "0",
                                                                            entry(
                                                                                None,
                                                                                true,
                                                                                Some(dict(vec![
                                                                                    ("0", entry_name("0")),
                                                                                    (
                                                                                        "5",
                                                                                        entry(
                                                                                            Some("5"),
                                                                                            false,
                                                                                            Some(keys_paragraph()),
                                                                                        ),
                                                                                    ),
                                                                                    ("6", entry_name("6")),
                                                                                ])),
                                                                            ),
                                                                        )])),
                                                                    ),
                                                                ),
                                                                ("1", entry_name("RunLength")),
                                                            ])),
                                                        ),
                                                    )])),
                                                ),
                                            ),
                                            (
                                                "6",
                                                entry(
                                                    Some("StyleRun"),
                                                    false,
                                                    Some(dict(vec![(
                                                        "0",
                                                        entry(
                                                            Some("RunArray"),
                                                            false,
                                                            Some(dict(vec![
                                                                (
                                                                    "0",
                                                                    entry(
                                                                        Some("StyleSheet"),
                                                                        false,
                                                                        Some(dict(vec![(
                                                                            "0",
                                                                            entry(
                                                                                None,
                                                                                true,
                                                                                Some(dict(vec![(
                                                                                    "6",
                                                                                    keys_style_sheet_data(),
                                                                                )])),
                                                                            ),
                                                                        )])),
                                                                    ),
                                                                ),
                                                                ("1", entry_name("RunLength")),
                                                            ])),
                                                        ),
                                                    )])),
                                                ),
                                            ),
                                        ])),
                                    ),
                                ),
                                ("1", entry_name("FontVectorData ???")),
                            ])),
                        ),
                    ),
                    ("2", entry(Some("StyleSheet"), false, Some(keys_style_sheet()))),
                    ("3", entry(Some("ParagraphSheet"), false, Some(keys_paragraph()))),
                ])),
            ),
        ),
    ])
}

fn keys_root_static() -> &'static KeysDict {
    static ROOT: OnceLock<KeysDict> = OnceLock::new();
    ROOT.get_or_init(keys_root)
}

static EMPTY_KEYS: OnceLock<KeysDict> = OnceLock::new();

fn empty_keys() -> &'static KeysDict {
    EMPTY_KEYS.get_or_init(HashMap::new)
}

// Зеркало `decodeObj`. Переименовывает ключи по `keys`, обрабатывает uproot и `_type`.
fn decode_obj(obj: &EngineValue, keys: &KeysDict) -> EngineValue {
    match obj {
        EngineValue::Null => EngineValue::Null,
        EngineValue::Array(arr) => {
            EngineValue::Array(arr.iter().map(|x| decode_obj(x, keys)).collect())
        }
        // typeof obj !== 'object' -> вернуть как есть (числа, булевы, строки).
        EngineValue::Number(_) | EngineValue::Bool(_) | EngineValue::Str(_) => obj.clone(),
        EngineValue::Dict(map) => {
            let mut result: Vec<(String, EngineValue)> = Vec::new();

            for (key, value) in map {
                if let Some(entry) = keys.get(key.as_str()) {
                    if entry.uproot {
                        // result = decodeObj(obj[key], children) — только если ключ != '99'.
                        if key != "99" {
                            let children = entry.children.as_ref().unwrap_or_else(|| empty_keys());
                            let decoded = decode_obj(value, children);
                            // result := decoded (перезаписываем целиком).
                            result = match decoded {
                                EngineValue::Dict(m) => m,
                                // decodeObj uproot-узла всегда даёт объект, но на случай
                                // не-объекта сохраняем под пустым ключом не требуется —
                                // в upstream'е это присвоение `result = <не-объект>` с
                                // последующим `result._type`/`result[...]`; для не-объекта
                                // ветка недостижима в реальных данных. Используем пусто.
                                other => vec![("".to_string(), other)],
                            };
                        }
                        // if (obj['99']) result._type = obj['99'];
                        if let Some(t) = map.iter().find(|(k, _)| k == "99").map(|(_, v)| v) {
                            if is_truthy(t) {
                                set_kv(&mut result, "_type", t.clone());
                            }
                        }
                        break;
                    } else {
                        let name = entry.name.unwrap_or(key.as_str());
                        let children = entry.children.as_ref().unwrap_or_else(|| empty_keys());
                        set_kv(&mut result, name, decode_obj(value, children));
                    }
                } else if key == "99" {
                    set_kv(&mut result, "_type", value.clone());
                } else {
                    set_kv(&mut result, key, decode_obj(value, empty_keys()));
                }
            }

            EngineValue::Dict(result)
        }
    }
}

// JS-истинность значения для `if (obj['99'])`: null/false/0/"" -> false.
fn is_truthy(v: &EngineValue) -> bool {
    match v {
        EngineValue::Null => false,
        EngineValue::Bool(b) => *b,
        EngineValue::Number(n) => *n != 0.0 && !n.is_nan(),
        EngineValue::Str(s) => !s.is_empty(),
        EngineValue::Array(_) | EngineValue::Dict(_) => true,
    }
}

fn set_kv(map: &mut Vec<(String, EngineValue)>, key: &str, value: EngineValue) {
    if let Some(slot) = map.iter_mut().find(|(k, _)| k == key) {
        slot.1 = value;
    } else {
        map.push((key.to_string(), value));
    }
}

/// Порт `decodeEngineData2`. Возвращает декодированную (переименованную) структуру.
pub fn decode_engine_data2(data: &EngineValue) -> EngineValue {
    decode_obj(data, keys_root_static())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_data::EngineValue;

    fn d(pairs: Vec<(&str, EngineValue)>) -> EngineValue {
        EngineValue::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn get<'a>(v: &'a EngineValue, key: &str) -> Option<&'a EngineValue> {
        if let EngineValue::Dict(m) = v {
            m.iter().find(|(k, _)| k == key).map(|(_, v)| v)
        } else {
            None
        }
    }

    #[test]
    fn renames_top_level_keys() {
        // { "0": {...}, "1": {...} } -> { ResourceDict, EngineDict }
        let input = d(vec![
            ("0", d(vec![])),
            ("1", d(vec![])),
        ]);
        let out = decode_engine_data2(&input);
        assert!(get(&out, "ResourceDict").is_some());
        assert!(get(&out, "EngineDict").is_some());
    }

    #[test]
    fn renames_style_sheet_keys() {
        // EngineDict.StyleSheet (key "2") uses keysStyleSheet: "1" -> FontSize.
        let input = d(vec![(
            "1",
            d(vec![(
                "2",
                d(vec![("1", EngineValue::Number(12.0))]),
            )]),
        )]);
        let out = decode_engine_data2(&input);
        let engine = get(&out, "EngineDict").unwrap();
        let style = get(engine, "StyleSheet").unwrap();
        assert_eq!(get(style, "FontSize"), Some(&EngineValue::Number(12.0)));
    }

    #[test]
    fn uproot_and_type() {
        // keysColor: "0" uproot with children {0:Type,1:Values}; "99" -> _type.
        // Build a FillColor (StyleSheet key "53").
        let input = d(vec![(
            "1", // EngineDict
            d(vec![(
                "2", // EngineDict.StyleSheet
                d(vec![(
                    "53", // FillColor, children = keysColor
                    d(vec![
                        ("99", EngineValue::Str("/SimplePaint".to_string())),
                        (
                            "0",
                            d(vec![
                                ("0", EngineValue::Number(1.0)),
                                ("1", EngineValue::Array(vec![EngineValue::Number(0.0)])),
                            ]),
                        ),
                    ]),
                )]),
            )]),
        )]);
        let out = decode_engine_data2(&input);
        let style = get(get(&out, "EngineDict").unwrap(), "StyleSheet").unwrap();
        let fill = get(style, "FillColor").unwrap();
        // uproot of "0" pulls Type/Values up; "_type" from "99".
        assert_eq!(get(fill, "Type"), Some(&EngineValue::Number(1.0)));
        assert_eq!(get(fill, "_type"), Some(&EngineValue::Str("/SimplePaint".to_string())));
        assert!(get(fill, "Values").is_some());
    }

    #[test]
    fn unknown_keys_passthrough() {
        let input = d(vec![("zzz", EngineValue::Number(7.0))]);
        let out = decode_engine_data2(&input);
        assert_eq!(get(&out, "zzz"), Some(&EngineValue::Number(7.0)));
    }
}
