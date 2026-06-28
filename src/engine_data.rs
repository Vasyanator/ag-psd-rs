/*
File: crates/ag-psd/src/engine_data.rs

Purpose:
парсер и сериализатор Engine Data (структура текстовых движков Photoshop).

Source compatibility:
- порт upstream-файла `test/ag-psd/src/engineData.ts` (разбиение 1:1).

Main responsibilities:
- зеркалировать соответствующий upstream-модуль при портировании;
- держать публичный контракт этого участка в одном месте.

Mapping TS -> Rust:
- `parseEngineData(data)`            -> `parse_engine_data(data: &[u8]) -> Result<EngineValue, EngineDataError>`
- `serializeEngineData(data, cond?)` -> `serialize_engine_data(data: &EngineValue, condensed: bool) -> Vec<u8>`

Модель значения EngineData (`EngineValue`):
Upstream хранит распарсенные данные как нетипизированный JS-объект. Значениями могут быть:
`null`, число (всегда `number`), булево, строка (как обычный текст в `( )`, так и
«имя» вида `/Name` — в JS это просто строка, начинающаяся с `/`), массив и словарь.
Словари ДОЛЖНЫ сохранять порядок ключей вставки, потому что сериализатор обходит
`Object.keys()` в порядке вставки (с одной поправкой `getKeys`, переносящей `99`/`98`
в начало) и побайтовый вывод сверяется Photoshop'ом. Поэтому `Dict` хранится как
`Vec<(String, EngineValue)>` (insertion-ordered), а не как `HashMap`.

Строки моделируются единым вариантом `EngineValue::Str(String)`: «имена» (`/Name`)
в upstream'е — это обычные строки, начинающиеся с `/`, и сериализатор отличает их
только по первому символу `/` (для ключей `98`/`99`). Отдельный вариант `Name` создал
бы расхождение с TS-семантикой, поэтому мы держим строку «как есть», включая ведущий `/`.

Замечание о точности строк:
Upstream работает на UTF-16 code units (`charCodeAt`/`String.fromCharCode`). Здесь текст
хранится как Rust `String` и конвертируется через `encode_utf16`/`from_utf16`, что
побайтово эквивалентно для любого валидного Unicode (типичный случай PSD-текста).
Одиночные суррогаты в `&str` невозможны.
*/

use std::fmt;

/// Ошибка парсинга EngineData (зеркало `throw new Error(...)` в upstream'е).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineDataError(pub String);

impl fmt::Display for EngineDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for EngineDataError {}

/// Распарсенное значение EngineData. См. обоснование модели в шапке файла.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineValue {
    /// `null` (в upstream'е — JS `null`).
    Null,
    /// Число. Upstream всегда хранит `number` (f64).
    Number(f64),
    /// Булево значение.
    Bool(bool),
    /// Строка. «Имена» хранятся как строка с ведущим `/` (например `/Name`).
    Str(String),
    /// Массив значений.
    Array(Vec<EngineValue>),
    /// Словарь `<< /Key value >>` с сохранением порядка ключей.
    Dict(Vec<(String, EngineValue)>),
}

impl EngineValue {
    pub fn is_object(&self) -> bool {
        // Зеркало `typeof value === 'object'` в JS для null/array/dict
        // (используется только там, где это влияет на ветвление).
        matches!(self, EngineValue::Dict(_) | EngineValue::Array(_) | EngineValue::Null)
    }
}

// ' ', '\n', '\r', '\t'
fn is_whitespace(char: u8) -> bool {
    char == 32 || char == 10 || char == 13 || char == 9
}

// 0123456789.-
fn is_number(char: u8) -> bool {
    (48..=57).contains(&char) || char == 46 || char == 45
}

// Внутренний узел стека парсера: либо контейнер (значение), либо имя ключа (строка).
enum StackItem {
    Value(EngineValue),
    Key(String),
}

/// Порт `parseEngineData`. Принимает байты блоба, возвращает корневое значение.
pub fn parse_engine_data(data: &[u8]) -> Result<EngineValue, EngineDataError> {
    let mut index: usize = 0;

    // --- helpers, оперирующие над `data`/`index` ---

    fn skip_whitespace(data: &[u8], index: &mut usize) {
        while *index < data.len() && is_whitespace(data[*index]) {
            *index += 1;
        }
    }

    fn get_text_byte(data: &[u8], index: &mut usize) -> u8 {
        let mut byte = data[*index];
        *index += 1;

        if byte == 92 {
            // \
            byte = data[*index];
            *index += 1;
        }

        byte
    }

    fn get_text(data: &[u8], index: &mut usize) -> Result<String, EngineDataError> {
        let mut units: Vec<u16> = Vec::new();

        if data[*index] == 41 {
            // )
            *index += 1;
            return Ok(String::new());
        }

        // Strings start with utf-16 BOM
        if data.get(*index) != Some(&0xFE) || data.get(*index + 1) != Some(&0xFF) {
            return Err(EngineDataError("Invalid utf-16 BOM".to_string()));
        }

        *index += 2;

        // ), ( and \ characters are escaped in ascii manner, remove the escapes before
        // interpreting the bytes as utf-16
        while *index < data.len() && data[*index] != 41 {
            // )
            let high = get_text_byte(data, index) as u16;
            let low = get_text_byte(data, index) as u16;
            let char = (high << 8) | low;
            units.push(char);
        }

        *index += 1;
        Ok(String::from_utf16_lossy(&units))
    }

    // Корень и стек. В отличие от TS (где объекты — ссылки, и контейнер одновременно
    // лежит в родителе и на стеке), здесь контейнеры ЖИВУТ на стеке до закрытия, а при
    // `pop` присоединяются к родителю. Семантически это эквивалентно: TS присоединяет
    // контейнер в родителя в момент открытия и затем мутирует через алиас, мы — в момент
    // закрытия. Итоговая структура та же.
    let mut root: Option<EngineValue> = None;
    let mut stack: Vec<StackItem> = Vec::new();

    fn set_dict(map: &mut Vec<(String, EngineValue)>, key: &str, value: EngineValue) {
        // Семантика `obj[key] = value`: перезапись по существующему ключу с
        // сохранением позиции, иначе вставка в конец.
        if let Some(slot) = map.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            map.push((key.to_string(), value));
        }
    }

    // pushValue: помещает значение в текущий контекст (объект-ключ или массив).
    fn push_value(
        stack: &mut Vec<StackItem>,
        _root: &mut Option<EngineValue>,
        value: EngineValue,
    ) -> Result<(), EngineDataError> {
        let top = stack.last().ok_or_else(|| EngineDataError("Invalid data".to_string()))?;

        match top {
            StackItem::Key(_) => {
                // top — строка-ключ; снимаем её и кладём значение в объект под ней.
                let key = match stack.pop().unwrap() {
                    StackItem::Key(k) => k,
                    _ => unreachable!(),
                };
                match stack.last_mut() {
                    Some(StackItem::Value(EngineValue::Dict(map))) => {
                        set_dict(map, &key, value);
                        Ok(())
                    }
                    _ => Err(EngineDataError("Invalid data".to_string())),
                }
            }
            StackItem::Value(EngineValue::Array(_)) => {
                if let Some(StackItem::Value(EngineValue::Array(arr))) = stack.last_mut() {
                    arr.push(value);
                }
                Ok(())
            }
            _ => Err(EngineDataError("Invalid data".to_string())),
        }
    }

    // pushContainer: открывает новый контейнер. Кладём его кадром на стек; присоединение
    // к родителю произойдёт в `pop`.
    fn push_container(
        stack: &mut Vec<StackItem>,
        _root: &mut Option<EngineValue>,
        value: EngineValue,
    ) {
        stack.push(StackItem::Value(value));
    }

    // pushProperty
    fn push_property(
        stack: &mut Vec<StackItem>,
        root: &mut Option<EngineValue>,
        name: &str,
    ) -> Result<(), EngineDataError> {
        if stack.is_empty() {
            push_container(stack, root, EngineValue::Dict(Vec::new()));
        }

        match stack.last() {
            Some(StackItem::Key(_)) => {
                // top — строка: трактуем имя как значение.
                if name == "nil" {
                    push_value(stack, root, EngineValue::Null)
                } else {
                    push_value(stack, root, EngineValue::Str(format!("/{}", name)))
                }
            }
            Some(StackItem::Value(_)) => {
                // top — объект/массив: имя становится ключом.
                stack.push(StackItem::Key(name.to_string()));
                Ok(())
            }
            None => Err(EngineDataError("Invalid data".to_string())),
        }
    }

    // pop: закрывает текущий контейнер. Снимает кадр со стека и присоединяет его значение
    // к родителю (либо делает его корнем, если стек опустел).
    fn pop(
        stack: &mut Vec<StackItem>,
        root: &mut Option<EngineValue>,
    ) -> Result<(), EngineDataError> {
        let item = stack.pop().ok_or_else(|| EngineDataError("Invalid data".to_string()))?;
        let value = match item {
            StackItem::Value(v) => v,
            StackItem::Key(_) => return Err(EngineDataError("Invalid data".to_string())),
        };

        if stack.is_empty() {
            *root = Some(value);
            Ok(())
        } else {
            push_value(stack, root, value)
        }
    }

    skip_whitespace(data, &mut index);

    let mut data_length = data.len();

    while data_length > 0 && data[data_length - 1] == 0 {
        data_length -= 1; // trim 0 bytes from end
    }

    while index < data_length {
        let i = index;
        let char = data[i];

        if char == 60 && data.get(i + 1) == Some(&60) {
            // <<
            index += 2;
            push_container(&mut stack, &mut root, EngineValue::Dict(Vec::new()));
        } else if char == 62 && data.get(i + 1) == Some(&62) {
            // >>
            index += 2;
            pop(&mut stack, &mut root)?;
        } else if char == 47 {
            // /
            index += 1;
            let start = index;

            while index < data.len() && !is_whitespace(data[index]) {
                index += 1;
            }

            let mut name = String::new();
            for j in start..index {
                name.push(data[j] as char);
            }

            push_property(&mut stack, &mut root, &name)?;
        } else if char == 40 {
            // (
            index += 1;
            let text = get_text(data, &mut index)?;
            push_value(&mut stack, &mut root, EngineValue::Str(text))?;
        } else if char == 91 {
            // [
            index += 1;
            push_container(&mut stack, &mut root, EngineValue::Array(Vec::new()));
        } else if char == 93 {
            // ]
            index += 1;
            pop(&mut stack, &mut root)?;
        } else if char == 110
            && data.get(i + 1) == Some(&117)
            && data.get(i + 2) == Some(&108)
            && data.get(i + 3) == Some(&108)
        {
            // null
            index += 4;
            push_value(&mut stack, &mut root, EngineValue::Null)?;
        } else if char == 116
            && data.get(i + 1) == Some(&114)
            && data.get(i + 2) == Some(&117)
            && data.get(i + 3) == Some(&101)
        {
            // true
            index += 4;
            push_value(&mut stack, &mut root, EngineValue::Bool(true))?;
        } else if char == 102
            && data.get(i + 1) == Some(&97)
            && data.get(i + 2) == Some(&108)
            && data.get(i + 3) == Some(&115)
            && data.get(i + 4) == Some(&101)
        {
            // false
            index += 5;
            push_value(&mut stack, &mut root, EngineValue::Bool(false))?;
        } else if is_number(char) {
            let mut value = String::new();

            while index < data.len() && is_number(data[index]) {
                value.push(data[index] as char);
                index += 1;
            }

            let parsed = parse_float(&value);
            push_value(&mut stack, &mut root, EngineValue::Number(parsed))?;
        } else {
            index += 1;
            // Зеркало `console.log` (невалидный токен пропускается, не бросается).
            eprintln!(
                "Invalid token '{}' ({}) at {}",
                char as char, char, index
            );
        }

        skip_whitespace(data, &mut index);
    }

    // В TS корень фиксируется при первом pushContainer. У нас контейнеры присоединяются
    // при `pop`, поэтому верхнеуровневый словарь condensed-формата (без обёртки `<< >>`)
    // остаётся на дне стека незакрытым — берём его как корень.
    if root.is_none() {
        if let Some(StackItem::Value(v)) = stack.into_iter().next() {
            root = Some(v);
        }
    }

    Ok(root.unwrap_or(EngineValue::Null))
}

// Зеркало JS `parseFloat`: парсит ведущий числовой префикс, иначе NaN.
fn parse_float(value: &str) -> f64 {
    let s = value.trim_start();
    // Найти максимальный валидный префикс f64.
    let mut end = 0;
    let bytes = s.as_bytes();
    let mut seen_dot = false;
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            end = i + 1;
            i += 1;
        } else if c == b'.' && !seen_dot {
            seen_dot = true;
            i += 1;
        } else {
            break;
        }
    }
    if end == 0 {
        f64::NAN
    } else {
        s[..end].parse::<f64>().unwrap_or(f64::NAN)
    }
}

const FLOAT_KEYS: &[&str] = &[
    "Axis", "XY", "Zone", "WordSpacing", "FirstLineIndent", "GlyphSpacing", "StartIndent",
    "EndIndent", "SpaceBefore", "SpaceAfter", "LetterSpacing", "Values", "GridSize",
    "GridLeading", "PointBase", "BoxBounds", "TransformPoint0", "TransformPoint1",
    "TransformPoint2", "FontSize", "Leading", "HorizontalScale", "VerticalScale",
    "BaselineShift", "Tsume", "OutlineWidth", "AutoLeading",
];

const INT_ARRAYS: &[&str] = &["RunLengthArray"];

// serializeInt
fn serialize_int(value: f64) -> String {
    // JS `value.toString()` для целочисленных значений.
    js_number_to_string(value)
}

// serializeFloat: toFixed(5) + три regex-замены.
fn serialize_float(value: f64) -> String {
    let mut s = format!("{:.5}", value);

    // .replace(/(\d)0+$/g, '$1') — убрать хвостовые нули после ненулевой цифры.
    s = replace_trailing_zeros(&s);
    // .replace(/^0+\.([1-9])/g, '.$1')
    s = replace_leading_zero_dot(&s);
    // .replace(/^-0+\.0(\d)/g, '-.0$1')
    s = replace_neg_zero_dot(&s);

    s
}

// /(\d)0+$/ -> $1 : находит САМУЮ ЛЕВУЮ цифру `p`, после которой до конца строки идут
// только нули (>=1), и оставляет строку по `p` включительно (отбрасывает хвост нулей).
// Это в точности семантика глобальной замены leftmost-greedy у движка JS-regex.
fn replace_trailing_zeros(s: &str) -> String {
    let bytes = s.as_bytes();
    let n = bytes.len();
    for p in 0..n {
        if bytes[p].is_ascii_digit() && p + 1 < n && bytes[p + 1..].iter().all(|&c| c == b'0') {
            return s[..=p].to_string();
        }
    }
    s.to_string()
}

// /^0+\.([1-9])/ -> .$1 : ведущие нули + точка + цифра 1-9 -> точка + цифра.
fn replace_leading_zero_dot(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b'0' {
        i += 1;
    }
    // Нужен хотя бы один ведущий ноль, далее '.', далее [1-9].
    if i >= 1
        && i + 1 < bytes.len()
        && bytes[i] == b'.'
        && (b'1'..=b'9').contains(&bytes[i + 1])
    {
        format!(".{}", &s[i + 1..])
    } else {
        s.to_string()
    }
}

// /^-0+\.0(\d)/ -> -.0$1 : '-' + нули + ".0" + цифра -> "-.0" + цифра.
fn replace_neg_zero_dot(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'-') {
        return s.to_string();
    }
    let mut i = 1;
    while i < bytes.len() && bytes[i] == b'0' {
        i += 1;
    }
    // Нужен хотя бы один ноль после '-', затем ".0", затем цифра.
    if i >= 2
        && i + 2 < bytes.len()
        && bytes[i] == b'.'
        && bytes[i + 1] == b'0'
        && bytes[i + 2].is_ascii_digit()
    {
        format!("-.0{}", &s[i + 2..])
    } else {
        s.to_string()
    }
}

// serializeNumber
fn serialize_number(value: f64, key: Option<&str>) -> String {
    // (key && floatKeys.indexOf(key) !== -1) || (value | 0) !== value
    let is_float = key.map(|k| FLOAT_KEYS.contains(&k)).unwrap_or(false) || (to_int32(value) as f64) != value;
    if is_float {
        serialize_float(value)
    } else {
        serialize_int(value)
    }
}

// JS `value | 0` — приведение к int32 через ToInt32.
fn to_int32(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let n = value.trunc();
    let m = n.rem_euclid(4294967296.0); // 2^32
    let u = m as u64 as u32;
    u as i32
}

// JS Number.toString() для значений, которые мы выводим как целые.
fn js_number_to_string(value: f64) -> String {
    if value == value.trunc() && value.is_finite() && value.abs() < 1e21 {
        // целое
        format!("{}", value as i64)
    } else {
        // не должно достигаться для serialize_int (вызывается только для целых),
        // но оставим разумный fallback.
        let s = format!("{}", value);
        s
    }
}

// getKeys: воспроизводит порядок ключей upstream'а, включая баг с двойным переносом '99'.
fn get_keys(map: &[(String, EngineValue)]) -> Vec<String> {
    let mut keys: Vec<String> = map.iter().map(|(k, _)| k.clone()).collect();

    // if (keys.indexOf('98') !== -1) keys.unshift(...keys.splice(keys.indexOf('99'), 1));
    if keys.iter().any(|k| k == "98") {
        if let Some(pos) = keys.iter().position(|k| k == "99") {
            let removed = keys.remove(pos);
            keys.insert(0, removed);
        }
        // Если '99' нет — splice(-1, 1) удалил бы последний элемент и перенёс его в начало.
        else if !keys.is_empty() {
            let removed = keys.remove(keys.len() - 1);
            keys.insert(0, removed);
        }
    }

    // if (keys.indexOf('99') !== -1) keys.unshift(...keys.splice(keys.indexOf('99'), 1));
    if let Some(pos) = keys.iter().position(|k| k == "99") {
        let removed = keys.remove(pos);
        keys.insert(0, removed);
    }

    keys
}

/// Порт `serializeEngineData`. Возвращает байты блоба.
pub fn serialize_engine_data(data: &EngineValue, condensed: bool) -> Vec<u8> {
    let mut buffer: Vec<u8> = Vec::with_capacity(1024);
    let mut indent: usize = 0;

    serialize_engine_data_inner(data, condensed, &mut buffer, &mut indent);

    buffer
}

fn write_str(buffer: &mut Vec<u8>, value: &str) {
    // writeString: пишет младший байт каждой UTF-16 code unit (charCodeAt & 0xff
    // фактически — но upstream пишет charCodeAt целиком в Uint8Array, что усекает
    // до байта). Для ASCII-литералов это эквивалентно прямой записи байтов.
    for unit in value.encode_utf16() {
        buffer.push((unit & 0xff) as u8);
    }
}

fn write_indent(buffer: &mut Vec<u8>, condensed: bool, indent: usize) {
    if condensed {
        write_str(buffer, " ");
    } else {
        for _ in 0..indent {
            write_str(buffer, "\t");
        }
    }
}

fn write_string_byte(buffer: &mut Vec<u8>, value: u8) {
    if value == 40 || value == 41 || value == 92 {
        // ( ) \
        buffer.push(92); // \
    }
    buffer.push(value);
}

fn serialize_engine_data_inner(
    data: &EngineValue,
    condensed: bool,
    buffer: &mut Vec<u8>,
    indent: &mut usize,
) {
    if condensed {
        if let EngineValue::Dict(map) = data {
            for key in get_keys(map) {
                let value = map.iter().find(|(k, _)| *k == key).map(|(_, v)| v).unwrap();
                write_property(&key, value, condensed, buffer, indent);
            }
        }
        // (если data — массив/число/строка в condensed-режиме, upstream ничего не пишет,
        //  т.к. `typeof data === 'object'` для массива true — но тогда for...in по
        //  числовым индексам; на практике корень EngineData всегда объект.)
        // Для точности: массив в condensed обрабатывается как объект с числовыми ключами.
        else if let EngineValue::Array(arr) = data {
            // Object.keys массива — это строковые индексы "0", "1", ...
            for (i, value) in arr.iter().enumerate() {
                let key = i.to_string();
                write_property(&key, value, condensed, buffer, indent);
            }
        }
    } else {
        write_str(buffer, "\n\n");
        write_value(data, None, false, condensed, buffer, indent);
    }
}

fn write_property(
    key: &str,
    value: &EngineValue,
    condensed: bool,
    buffer: &mut Vec<u8>,
    indent: &mut usize,
) {
    write_indent(buffer, condensed, *indent);
    write_str(buffer, &format!("/{}", key));
    write_value(value, Some(key), true, condensed, buffer, indent);
    if !condensed {
        write_str(buffer, "\n");
    }
}

fn write_value(
    value: &EngineValue,
    key: Option<&str>,
    in_property: bool,
    condensed: bool,
    buffer: &mut Vec<u8>,
    indent: &mut usize,
) {
    // writePrefix
    fn write_prefix(in_property: bool, condensed: bool, buffer: &mut Vec<u8>, indent: usize) {
        if in_property {
            write_str(buffer, " ");
        } else {
            write_indent(buffer, condensed, indent);
        }
    }

    match value {
        EngineValue::Null => {
            write_prefix(in_property, condensed, buffer, *indent);
            write_str(buffer, if condensed { "/nil" } else { "null" });
        }
        EngineValue::Number(n) => {
            write_prefix(in_property, condensed, buffer, *indent);
            write_str(buffer, &serialize_number(*n, key));
        }
        EngineValue::Bool(b) => {
            write_prefix(in_property, condensed, buffer, *indent);
            write_str(buffer, if *b { "true" } else { "false" });
        }
        EngineValue::Str(s) => {
            write_prefix(in_property, condensed, buffer, *indent);

            let is_name = (key == Some("99") || key == Some("98")) && s.starts_with('/');
            if is_name {
                write_str(buffer, s);
            } else {
                write_str(buffer, "(");
                buffer.push(0xfe);
                buffer.push(0xff);

                for code in s.encode_utf16() {
                    write_string_byte(buffer, ((code >> 8) & 0xff) as u8);
                    write_string_byte(buffer, (code & 0xff) as u8);
                }

                write_str(buffer, ")");
            }
        }
        EngineValue::Array(arr) => {
            write_prefix(in_property, condensed, buffer, *indent);

            let all_numbers = arr.iter().all(|x| matches!(x, EngineValue::Number(_)));
            if all_numbers {
                write_str(buffer, "[");

                let int_array = key.map(|k| INT_ARRAYS.contains(&k)).unwrap_or(false);

                for x in arr {
                    if let EngineValue::Number(n) = x {
                        write_str(buffer, " ");
                        let s = if int_array {
                            serialize_number(*n, None)
                        } else {
                            serialize_float(*n)
                        };
                        write_str(buffer, &s);
                    }
                }

                write_str(buffer, " ]");
            } else {
                write_str(buffer, "[");
                if !condensed {
                    write_str(buffer, "\n");
                }

                for x in arr {
                    write_value(x, key, false, condensed, buffer, indent);
                    if !condensed {
                        write_str(buffer, "\n");
                    }
                }

                write_indent(buffer, condensed, *indent);
                write_str(buffer, "]");
            }
        }
        EngineValue::Dict(map) => {
            if in_property && !condensed {
                write_str(buffer, "\n");
            }

            write_indent(buffer, condensed, *indent);
            write_str(buffer, "<<");

            if !condensed {
                write_str(buffer, "\n");
            }

            *indent += 1;

            for k in get_keys(map) {
                let v = map.iter().find(|(kk, _)| *kk == k).map(|(_, v)| v).unwrap();
                write_property(&k, v, condensed, buffer, indent);
            }

            *indent -= 1;
            write_indent(buffer, condensed, *indent);
            write_str(buffer, ">>");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: Vec<(&str, EngineValue)>) -> EngineValue {
        EngineValue::Dict(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn sample() -> EngineValue {
        dict(vec![
            (
                "EngineDict",
                dict(vec![
                    (
                        "Editor",
                        dict(vec![("Text", EngineValue::Str("Привет ❤".to_string()))]),
                    ),
                    (
                        "RunLengthArray",
                        EngineValue::Array(vec![
                            EngineValue::Number(3.0),
                            EngineValue::Number(5.0),
                        ]),
                    ),
                    (
                        "Values",
                        EngineValue::Array(vec![
                            EngineValue::Number(0.0),
                            EngineValue::Number(0.5),
                            EngineValue::Number(1.0),
                        ]),
                    ),
                    ("FontSize", EngineValue::Number(12.0)),
                    ("AntiAlias", EngineValue::Number(4.0)),
                    ("UseFractionalGlyphWidths", EngineValue::Bool(true)),
                    ("Nothing", EngineValue::Null),
                ]),
            ),
            (
                "ResourceDict",
                dict(vec![(
                    "Nested",
                    dict(vec![("Flag", EngineValue::Bool(false))]),
                )]),
            ),
        ])
    }

    #[test]
    fn round_trip_full() {
        let value = sample();
        let bytes = serialize_engine_data(&value, false);
        let parsed = parse_engine_data(&bytes).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn round_trip_condensed() {
        let value = sample();
        let bytes = serialize_engine_data(&value, true);
        let parsed = parse_engine_data(&bytes).unwrap();
        assert_eq!(parsed, value);
    }

    #[test]
    fn condensed_byte_exact() {
        // Маленький проверяемый вручную случай.
        let value = dict(vec![
            ("A", EngineValue::Number(1.0)),
            ("B", EngineValue::Bool(true)),
            ("C", EngineValue::Number(12.0)),
            ("D", EngineValue::Null),
        ]);
        let bytes = serialize_engine_data(&value, true);
        // condensed: " /A 1 /B true /C 12 /D /nil"
        let expected = b" /A 1 /B true /C 12 /D /nil".to_vec();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn condensed_string_byte_exact() {
        // Строка "AB" в condensed: " /T (<FE><FF>\0A\0B)"
        let value = dict(vec![("T", EngineValue::Str("AB".to_string()))]);
        let bytes = serialize_engine_data(&value, true);
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b" /T (");
        expected.push(0xfe);
        expected.push(0xff);
        expected.extend_from_slice(&[0x00, b'A', 0x00, b'B']);
        expected.push(b')');
        assert_eq!(bytes, expected);
    }

    #[test]
    fn condensed_string_escapes() {
        // Скобки и обратный слэш в строке должны экранироваться.
        let value = dict(vec![("T", EngineValue::Str("()\\".to_string()))]);
        let bytes = serialize_engine_data(&value, true);
        let mut expected: Vec<u8> = Vec::new();
        expected.extend_from_slice(b" /T (");
        expected.push(0xfe);
        expected.push(0xff);
        // '(' = 0x28 high 0x00 low 0x28 -> 0x00, '\(' (escaped 0x28)
        expected.extend_from_slice(&[0x00, 0x5c, 0x28]); // 0x00 then \(
        expected.extend_from_slice(&[0x00, 0x5c, 0x29]); // 0x00 then \)
        expected.extend_from_slice(&[0x00, 0x5c, 0x5c]); // 0x00 then \\
        expected.push(b')');
        assert_eq!(bytes, expected);
    }

    #[test]
    fn non_condensed_byte_exact() {
        // { A: 1, B: { C: 2 } } в не-condensed форме.
        let value = dict(vec![
            ("A", EngineValue::Number(1.0)),
            ("B", dict(vec![("C", EngineValue::Number(2.0))])),
        ]);
        let bytes = serialize_engine_data(&value, false);
        // \n\n<<\n\t/A 1\n\t/B\n\t<<\n\t\t/C 2\n\t>>\n>>
        let expected = "\n\n<<\n\t/A 1\n\t/B\n\t<<\n\t\t/C 2\n\t>>\n>>";
        assert_eq!(String::from_utf8(bytes).unwrap(), expected);
    }

    #[test]
    fn float_formatting() {
        assert_eq!(serialize_float(0.5), ".5");
        assert_eq!(serialize_float(1.0), "1.0");
        assert_eq!(serialize_float(12.25), "12.25");
        assert_eq!(serialize_float(-0.05), "-.05");
        assert_eq!(serialize_float(100.0), "100.0");
    }

    #[test]
    fn number_int_vs_float() {
        // FontSize в floatKeys -> float-формат ("12.00000" -> "12.0").
        assert_eq!(serialize_number(12.0, Some("FontSize")), "12.0");
        // Обычный ключ, целое значение -> int.
        assert_eq!(serialize_number(12.0, Some("AntiAlias")), "12");
        // Нецелое без ключа -> float.
        assert_eq!(serialize_number(0.5, None), ".5");
    }

    #[test]
    fn get_keys_99_first() {
        let map = vec![
            ("0".to_string(), EngineValue::Number(1.0)),
            ("99".to_string(), EngineValue::Str("/Type".to_string())),
            ("1".to_string(), EngineValue::Number(2.0)),
        ];
        assert_eq!(get_keys(&map), vec!["99", "0", "1"]);
    }
}
