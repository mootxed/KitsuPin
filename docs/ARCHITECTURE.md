# Архитектура KitsuPin

KitsuPin состоит из одного долгоживущего Tauri-процесса, двух webview-окон и
минимального native messaging binary. Все пользовательские данные остаются в
`$XDG_DATA_HOME/kitsupin` (по умолчанию `~/.local/share/kitsupin`).

---

## Слои

| Путь | Назначение |
|---|---|
| `src-tauri/src/domain` | Нормализация, классификация, модели, дедупликация |
| `src-tauri/src/persistence` | SQLite, миграции 1–8, trigram FTS5, short-query page_title fallback, reconciliation |
| `src-tauri/src/clipboard` | XFixes-события только для `CLIPBOARD`, чтение и подавление собственных событий; polling 350 мс — аварийный fallback |
| `src-tauri/src/browser_metadata` | Unix socket, буфер событий Chrome с `event_id: Uuid`, `ClipUpsertReceipt`, `take_matching_receipt`, cleanup по времени |
| `src-tauri/src/jobs` | Очистка устаревшей незакреплённой истории |
| `src-tauri/src/settings` | Настройки (write-fsync-rename), XDG Autostart, consume_invalid_warning |
| `src-tauri/src/migration.rs` | Миграция Pastily → KitsuPin, lock 0600, SQLite Backup API, обработка 4 сценариев (A, B, C, D) |
| `src-tauri/src/lib.rs` | Команды Tauri, окна, tray, регистрация горячих клавиш с обработкой `registered_shortcut = None`, single-instance lock |
| `src/` | TypeScript UI; режим выбирается по query `?mode=popup` |
| `chrome-extension/` | Manifest V3 content script, service worker, status page |
| `scripts/` | Установка native manifest и autostart для текущего пользователя |

---

## Файловая система данных

```text
~/.local/share/
├── .kitsupin-migration.lock # Advisory lock для миграции (0600, вне каталога приложения)
└── kitsupin/
    ├── kitsupin.sqlite3     # База данных (WAL, 0o600)
    ├── kitsupin.sqlite3-wal
    ├── kitsupin.sqlite3-shm
    ├── kitsupin.lock        # Advisory file lock (fs2), удерживается весь срок жизни
    ├── app.sock             # Unix socket для single-instance IPC
    ├── native.sock          # Unix socket для Native Messaging (0600)
    └── settings.json        # Настройки (0o600, write-fsync-rename)
```

---

## Миграция данных Pastily → KitsuPin

Миграция выполняется до запуска базы данных KitsuPin. Использует эксклюзивный lock-файл `$XDG_DATA_HOME/.kitsupin-migration.lock` с правами `0600`.

### Поддерживаемые сценарии

1. **Сценарий A (Только старый каталог)**:
   Атомарно переименовывает `~/.local/share/pastily` в `~/.local/share/kitsupin` и переименовывает файлы базы данных.
2. **Сценарий B (Существует каталог kitsupin без kitsupin.sqlite3)**:
   Переносит старую базу через SQLite Backup API, сбрасывает WAL (`PRAGMA wal_checkpoint(TRUNCATE)`), проверяет `integrity_check` и `foreign_key_check`, после чего создает backup старой базы.
3. **Сценарий C (kitsupin.sqlite3 существует, но фактически пуст)**:
   Перемещает пустую новую базу в `kitsupin.sqlite3.empty.bak`, переносит старую базу через SQLite Backup API с проверкой целостности, удаляет временную пустую базу только после успешного переноса.
4. **Сценарий D (Обе базы содержат пользовательские данные)**:
   Выполняет транзакционный импорт legacy-карточек и категорий из Pastily в KitsuPin. Объединяет дубликаты по `(content_hash, domain_key)`, сохраняя закрепления (`pinned`), категории, наиболшие `copy_count` и свежие метки времени `last_copied_at`. Не перезаписывает более свежие данные старыми.

---

## Поток копирования и Late Reconciliation

```text
Пользователь копирует в браузере Chrome
  │
  ├── Chrome Extension → Native Host → native.sock
  │     └─ BrowserCopyEvent { event_id: Uuid, content_hash, content_length, domain, page_title, timestamp }
  │          └─ Сохраняется в MetadataBuffer
  │
  └── X11 Clipboard Event (или Polling fallback)
        └─ Clipboard Watcher:
             ├─ Попытка take_match (Metadata before Clipboard)
             │    └─ Если найден event: сразу создаёт карточку с доменом и заголовком
             └─ Иначе (Metadata after Clipboard):
                  └─ Создаёт domainless-карточку + генерирует ClipUpsertReceipt
                       └─ Pushes receipt в MetadataBuffer
```

### Прикрепление метаданных (reconciliation)

- Socket callback вызывает `take_matching_receipt(hash, length, timestamp_ms, RECEIPT_MATCH_WINDOW_MS = 2000)`.
- Выбирается receipt с минимальной разницей метки времени в пределах допустимого окна.
- `attach_metadata` проверяет наличие карточки по `receipt.clip_id`, совпадение `content_hash`, `domain_key == ''`, и соответствие текущего состояния DB данным receipt (`copy_count`, `last_copied_at`).
- Если состояние карточки изменилось (было повторное копирование), receipt считается устаревшим и не применяется.
- **Без receipt reconciliation для ранее существовавших карточек с copy_count > 1 ЗАПРЕЩЁН**: partial rollback `copy_count - 1` удалён. Без receipt домен прикрепляется только в однозначном случае для впервые созданной карточки (`copy_count == 1`).
- После успешной синхронизации удаляется строго обработанное событие по `event_id` (`remove_event`).
- Устаревшие events и receipts старше 10 000 мс автоматически очищаются (`cleanup_stale`).

---

## Поиск и FTS5

- Таблица `clips_fts` использует `tokenize='trigram'` над полем `content` и `page_title`.
- Запросы длиной ≥ 3 символов выполняются через FTS5 match.
- **Короткие запросы (< 3 Unicode-символов)**: дополнительно ищут по `kitsupin_lower(c.content) LIKE ?` и `kitsupin_lower(COALESCE(c.page_title, '')) LIKE ?` в пределах последних `SHORT_SEARCH_FALLBACK_LIMIT = 5000` карточек.

---

## Настройки и регистрация горячих клавиш

Управление `registered_shortcut`:
- Если при запуске регистрация комбинации завершилась ошибкой, `registered_shortcut` устанавливается в `None`.
- При последующем сохранении новых настроек код проверяет `registered_shortcut`:
  - `Some(old)` -> регистрирует новый shortcut, снимает `old`.
  - `None` -> регистрирует новый shortcut, старый не снимает.
- Любая ошибка при вызове global shortcut API, autostart или сохранении `settings.json` приводит к каскадному откату без нарушения runtime-состояния.

---

## SQLite-схема и миграции

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

### История миграций DB

- **1–7**: Начальная схема, `domain_key`, FTS5, integer timestamps, merge duplicates, UPDATE triggers.
- **8**: Пересоздание FTS5 с `trigram` токенизатором, индекс `idx_clip_categories_category`.

---

## CI

Workflow: `.github/workflows/ci.yml`

1. `frontend`: `npm ci` · `npm run lint` · `npm test` · `npm run build`
2. `rust-core`:
   - `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check`
   - `cargo clippy --manifest-path src-tauri/core-tests/Cargo.toml --all-targets -- -D warnings`
   - `cargo test --manifest-path src-tauri/core-tests/Cargo.toml`
   - `cargo test --manifest-path src-tauri/Cargo.toml`
3. `tauri-check`: `cargo check --manifest-path src-tauri/Cargo.toml` · `npm run tauri build -- --bundles deb`
