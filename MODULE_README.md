# Crate: ag-psd (Rust port)

## Назначение
Самостоятельный workspace-крейт с переносом TypeScript-библиотеки `ag-psd`
(чтение/запись PSD-файлов Photoshop) на Rust «с нуля». Сейчас крейт находится в
стадии каркаса: публичный API и разбиение на модули объявлены, но реальная
логика ещё не портирована.

Крейт намеренно standalone и НЕ подключён как зависимость корневого бинарника
`manhwastudio_rs`. Подключение произойдёт позже, когда reader/writer будут
готовы.

## Эталон (spec)
Эталоном является оригинальная TS-библиотека в `test/ag-psd` (только для чтения):
- исходники-спецификация: `test/ag-psd/src/*.ts`;
- фикстуры для тестов: `test/ag-psd/test` (пары `read/<name>/src.psd` + `data.json`).

## Политика портирования
- **Порт по необходимости** (port on demand): модули наполняются логикой только
  тогда, когда они реально нужны следующему шагу, а не все сразу.
- **Зеркалирование разбиения 1:1**: каждому исходному файлу `*.ts` соответствует
  ровно один `*.rs`-модуль с тем же набором ответственности. Имена приводятся к
  `snake_case`.
- **Сначала запись, потом чтение** (write path first, then reader): порт начинаем
  с пути записи (`writer.rs` + сериализация в `psd.rs`/секции), затем делаем
  обратный путь чтения. Это даёт возможность генерировать валидные PSD и
  проверять их сторонними инструментами до готовности reader.

## Карта модулей (TS -> Rust)
| Upstream `*.ts`        | Rust модуль            | Назначение |
| ---------------------- | ---------------------- | ---------- |
| `psdWriter.ts`         | `writer.rs`            | низкоуровневая запись байтов в буфер PSD |
| `psdReader.ts`         | `reader.rs`            | низкоуровневое чтение байтов из буфера PSD |
| `descriptor.ts`        | `descriptor.rs`        | дескрипторы Photoshop (Action Descriptor) |
| `engineData.ts`        | `engine_data.rs`       | парсер/сериализатор Engine Data (текст) |
| `engineData2.ts`       | `engine_data2.rs`      | вариант Engine Data v2 |
| `text.ts`              | `text.rs`              | работа с текстовыми слоями |
| `additionalInfo.ts`    | `additional_info.rs`   | additional layer information блоки |
| `imageResources.ts`    | `image_resources.rs`   | image resource блоки |
| `effectsHelpers.ts`    | `effects_helpers.rs`   | вспомогательные функции эффектов слоёв |
| `helpers.ts`           | `helpers.rs`           | общие хелперы (в т.ч. `initialize_canvas`) |
| `initializeCanvas.ts`  | `initialize_canvas.rs` | инициализация canvas-фабрики (зеркало upstream) |
| `utf8.ts`              | `utf8.rs`              | кодирование/декодирование UTF-8 |
| `psd.ts`               | `psd.rs`               | главные типы документа `Psd` + оркестрация чтения/записи |
| `abr.ts`               | `abr.rs`               | чтение кистей Photoshop (.abr) |
| `csh.ts`               | `csh.rs`               | чтение custom shapes (.csh) |
| `ase.ts`               | `ase.rs`               | палитры Adobe Swatch Exchange (.ase) |
| `jpeg.ts`              | `jpeg.rs`              | работа с JPEG-данными внутри PSD |
| `index.ts`             | `lib.rs`               | публичные re-export'ы и точки входа |

## Файлы
- `lib.rs`: корень крейта; объявляет `pub mod ...;` для всех модулей и сводит
  публичные re-export'ы (зеркало `index.ts`).
- `src/*.rs`: модули по таблице выше; каждый начинается с FILE HEADER-комментария
  и пометки `// PORT STATUS: stub — not yet ported`.
- `tests/fixtures.rs`: каркас интеграционного теста, который позднее будет
  round-trip-проверять `src.psd` против `data.json` из `test/ag-psd/test`.

## Тестирование
- `cargo build -p ag-psd` — крейт должен собираться на любом этапе.
- `cargo test -p ag-psd` — фикстурные тесты (пока `#[ignore]`).
