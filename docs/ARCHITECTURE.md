# Архитектура KitsuPin

> **Статус:** Ранний MVP. Требуется системное тестирование на целевой среде (Ubuntu 24.04 · KDE Plasma · X11).

KitsuPin состоит из одного долгоживущего Tauri-процесса, двух webview-окон и
минимального native messaging binary. Все пользовательские данные остаются в
`$XDG_DATA_HOME/kitsupin` (по умолчанию `~/.local/share/kitsupin`).

---

## Слои

| Путь | Назначение |
|---|---|
| `src-tauri/src/domain` | Нормализация, классификация, модели, дедупликация |
| `src-tauri/src/persistence` | SQLite, миграции 1–7, запросы, транзакции, reconciliation |
| `src-tauri/src/clipboard` | XFixes-события только для `CLIPBOARD`, чтение и подавление собственных событий; polling 350 мс — аварийный fallback |
| `src-tauri/src/browser_metadata` | Unix socket, краткоживущий буфер событий Chrome, late reconciliation callback |
| `src-tauri/src/jobs` | Очистка устаревшей незакреплённой истории |
| `src-tauri/src/settings` | Настройки (write-fsync-rename), XDG Autostart, consume_invalid_warning |
| `src-tauri/src/lib.rs` | Команды Tauri, окна, tray, глобальная клавиша, single-instance lock |
| `src/` | TypeScript UI; режим выбирается по query `?mode=popup` |
| `chrome-extension/` | Manifest V3 content script, service worker, status page |
| `scripts/` | Установка native manifest и autostart для текущего пользователя |

---

## Файловая система данных

```
~/.local/share/kitsupin/
├── kitsupin.sqlite3         # База данных (WAL, 0o600)
├── kitsupin.sqlite3-wal
├── kitsupin.sqlite3-shm
├── kitsupin.lock            # Advisory file lock (fs2), удерживается весь срок жизни
├── app.sock                 # Unix socket для single-instance IPC
├── native.sock              # Unix socket для Native Messaging
└── settings.json            # Настройки (0o600, write-fsync-rename)
```

---

## Single-instance (атомарный)

Перед открытием базы и запуском Tauri приложение пытается получить **exclusive
advisory file lock** (`kitsupin.lock`, через crate `fs2`).

- Если lock свободен — процесс запускается в штатном режиме.
- Если lock занят — второй процесс подключается к `app.sock`, отправляет
  `show_main\n` (с retry 3×100 мс) и завершается. База данных **не открывается**.

После захвата lock первичный процесс создаёт `app.sock` (предварительно удалив
stale-файл, если таковой остался после краша). Listener принимает только команду
`show_main`; любые другие строки игнорируются.

**Native Messaging socket (`native.sock`)** перед bind пробует `connect()`:
- Соединение успешно → другой сервер активен, возвращает ошибку.
- Соединение неудачно → stale socket, удаляет файл и делает `bind`.

---

## Поток копирования

```
Пользователь нажимает Ctrl+C
  → XFixes уведомление (или poll 350 мс)
    → clipboard::start читает текст
      → normalize_content + content_hash
        → MetadataBuffer::take_match (immediate, 0–2.5 сек)
          → Repository::upsert_clip(content, domain?, title?, now)
            → clips-changed
```

### Late reconciliation (Chrome metadata)

Content script вычисляет SHA-256 нормализованного текста и передаёт только хеш,
длину UTF-8 в байтах, hostname и title. Native host пересылает событие в `native.sock`.

Если metadata поступают **после** того, как clipboard watcher уже сохранил карточку
без домена, socket server вызывает `Repository::attach_metadata`:

1. Ищет карточку с `content_hash = ?` AND `domain_key = ''` AND
   `last_copied_at >= now - 5000 мс`.
2. Проверяет `length(content)` совпадает с `content_length_bytes` из Chrome.
3. Если существует карточка с `(content_hash, domain_key)` — **merge**:
   сохраняет закрепление, объединяет категории, суммирует `copy_count`.
4. Если нет — обновляет `domain`, `domain_key`, `page_title` на месте.
5. Отправляет `clips-changed`.

**Metadata не прикрепляются** если:
- hash не совпадает,
- длина контента не совпадает,
- разница времени превышает `METADATA_RECONCILE_WINDOW_MS = 5000 мс`.

---

## Дедупликация

Уникальность clips: **SHA-256 нормализованного содержимого + `domain_key`**.

`domain_key` = нормализованный домен (без `www.`, lowercase, trim) либо пустая
строка `''` для карточек без источника.

> **Примечание:** SHA-256 теоретически допускает коллизии. При совпадении хеша
> `attach_metadata` дополнительно проверяет `length(content) == content_length_bytes`
> из Chrome, что делает случайное ложное прикрепление практически невозможным.
> Тем не менее архитектура не полагается слепо на неколлизионность хеша.

---

## Транзакция копирования карточки

Команда `copy_clip` выполняет операции в безопасном порядке:

1. `get_clip_content(id)` — читает содержимое (без изменений).
2. `set_clipboard(content)` — записывает в системный Clipboard.
3. `mark_clip_copied(id, now)` — только при успехе п.2: обновляет `copy_count`,
   `last_copied_at`, `sort_key`.
4. `emit("clips-changed")`.
5. Скрыть popup — только при полном успехе.

Если clipboard-запись завершилась ошибкой — база не изменяется, popup остаётся открытым.

---

## Настройки и rollback

`save_settings` применяет изменения в следующем порядке с полным rollback:

1. Валидация (`shortcut` не пустой, `retention_days` 1–3650, `excluded_apps` ≤ 100).
2. Регистрация новой горячей клавиши — при ошибке возврат без изменений.
3. Снятие старой горячей клавиши.
4. Применение autostart — при ошибке откат shortcut.
5. Запись `settings.json` (write-fsync-rename) — при ошибке откат autostart и shortcut.
6. Обновление runtime-состояния (paused, tray).

Файл записывается как `settings.tmp`, затем `fsync`, затем `rename` (атомарная замена).

---

## SQLite-схема

### Таблица `clips`

| Колонка | Тип | Описание |
|---|---|---|
| `id` | TEXT PK | UUID v4 |
| `content` | TEXT | Полный текст |
| `content_hash` | TEXT | SHA-256 нормализованного содержимого |
| `content_type` | TEXT | `Text\|Links\|Email\|Numbers` |
| `domain` | TEXT? | Нормализованный домен |
| `domain_key` | TEXT | `domain` или `''` (никогда NULL) |
| `page_title` | TEXT? | Заголовок страницы ≤ 500 символов |
| `created_at` | INTEGER | Unix milliseconds |
| `last_copied_at` | INTEGER | Unix milliseconds |
| `copy_count` | INTEGER | ≥ 1 |
| `pinned` | INTEGER | 0 или 1 |
| `sort_key` | INTEGER | Дополнительный критерий сортировки |

UNIQUE constraint: `(content_hash, domain_key)`.

### FTS5 (`clips_fts`)

Виртуальная таблица `external-content` над `clips`, индексирует `content` и
`page_title`. Triggers:

- `clips_ai` — AFTER INSERT
- `clips_ad` — AFTER DELETE
- `clips_au` — **AFTER UPDATE OF content, page_title** (намеренно не срабатывает
  при изменении `copy_count`, `last_copied_at`, `pinned`, `sort_key`)

---

## Миграции

| Версия | Описание |
|---|---|
| 1 | Начальная схема |
| 2 | Колонка `domain_key`, UNIQUE индекс по `(content_hash, domain_key)` |
| 3 | Попытка `DROP COLUMN normalized_content` (может не сработать при UNIQUE constraint) |
| 4 | FTS5 + triggers (все UPDATE) |
| 5 | TEXT timestamps → INTEGER ms (только RFC3339) |
| 6 | **Безопасный полный rebuild** таблицы `clips` если `normalized_content` ещё присутствует; нормализация INTEGER-секунд → ms; merge дубликатов без потери `pinned`/категорий |
| 7 | Замена `clips_au` trigger на `AFTER UPDATE OF content, page_title` |

Каждая миграция транзакционна. Migration 6 использует `PRAGMA foreign_keys=OFF`
внутри транзакции и восстанавливает с `PRAGMA foreign_key_check`.

---

## CI

Файл: `.github/workflows/ci.yml`

| Job | Runner | Шаги |
|---|---|---|
| `frontend` | ubuntu-24.04 | `npm ci` · `npm run lint` · `npm test` · `npm run build` |
| `rust-core` | ubuntu-24.04 | `cargo fmt --check` · `cargo clippy -D warnings` · `cargo test` (core-tests) |
| `tauri-check` | ubuntu-24.04 | Системные зависимости GTK/WebKit · `cargo check` · `npm run tauri build --bundles deb` |

Все jobs используют `concurrency.cancel-in-progress: true`.

---

## Локальные команды

```bash
# Frontend
npm ci
npm run lint
npm test
npm run build

# Rust core tests (включает FTS5 через bundled-full)
cargo test --manifest-path src-tauri/core-tests/Cargo.toml

# Format check
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check

# Clippy
cargo clippy --manifest-path src-tauri/core-tests/Cargo.toml --all-targets -- -D warnings

# Tauri check (без сборки бинаря)
cargo check --manifest-path src-tauri/Cargo.toml

# Полная сборка .deb
npm run tauri build -- --bundles deb
```

---

## Ограничения текущей версии

- Только X11; Wayland не поддерживается.
- Только Google Chrome через Native Messaging; Firefox — нет.
- Синхронизация между устройствами не реализована.
- Поддержка изображений и файлов не реализована.
- `excluded_apps` в настройках подготовлено архитектурно, но отключено:
  X11 не всегда надёжно сообщает источник Clipboard.
- PRIMARY selection не отслеживается (только `CLIPBOARD` atom).
