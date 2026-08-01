# Архитектура KitsuPin

KitsuPin состоит из одного долгоживущего Tauri-процесса, двух webview-окон и минимального native messaging binary. Все пользовательские данные остаются в `$XDG_DATA_HOME/kitsupin` (по умолчанию `~/.local/share/kitsupin`).

---

## Слои

| Путь | Назначение |
|---|---|
| `src-tauri/src/domain` | Нормализация, классификация, модели, дедупликация |
| `src-tauri/src/persistence` | SQLite, миграции 1–8, trigram FTS5, short-query fallback, attach_metadata_with_receipt |
| `src-tauri/src/clipboard` | XFixes-события только для `CLIPBOARD`, чтение и подавление собственных событий; polling 350 мс — аварийный fallback |
| `src-tauri/src/browser_metadata` | Unix socket, буфер событий Chrome с `event_id: Uuid`, `ClipUpsertReceipt`, `take_matching_receipt`, cleanup по времени |
| `src-tauri/src/jobs` | Очистка устаревшей незакреплённой истории |
| `src-tauri/src/settings` | Настройки (write-fsync-rename), XDG Autostart, consume_invalid_warning |
| `src-tauri/src/migration.rs` | Миграция Pastily v1 → KitsuPin, schema introspection, parse_legacy_timestamp, canonical ID maps, Backup API, 4 сценария (A, B, C, D) |
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

## Миграция данных Pastily v1 → KitsuPin

Миграция выполняется до запуска основной базы данных KitsuPin. Использует эксклюзивный lock-файл `$XDG_DATA_HOME/.kitsupin-migration.lock` с правами `0600`.

### Схема и импорт
- **Schema Introspection**: проверка наличия колонок (`domain_key`, `normalized_content`, `normalized_name`, `sort_order`) через `PRAGMA table_info` без генерации несуществующих имён в SQL.
- **Внутренние модели**: преобразование legacy-таблиц в `ImportedClip`, `ImportedCategory`, `ImportedCategoryLink`.
- **Парсинг меток времени**: `parse_legacy_timestamp` поддерживает RFC3339, SQLite datetime (`YYYY-MM-DD HH:MM:SS`), Unix seconds, Unix milliseconds и REAL с ведением непубличных логов.
- **Нормализация данных**: заново выполняются `normalize_content`, SHA-256 `content_hash`, классификация типа (`Text`/`Links`/`Email`/`Numbers`), нормализация домена `domain_key` и ограничение `page_title` 500 символами.
- **Каноническое отображение**:
  - Категории объединяются по `normalized_name`, формируя `legacy_category_id -> canonical_category_id`.
  - Карточки объединяются по `(content_hash, domain_key)`, формируя `legacy_clip_id -> canonical_clip_id`.
  - Правила объединения: `created_at` = MIN, `last_copied_at` = MAX, `copy_count` = saturating SUM, `pinned` = OR, `sort_key` = MAX, `page_title` берется от наиболее свежей записи по `last_copied_at`.
  - Связи `clip_user_categories` вставляются по каноническим ID через `INSERT OR IGNORE`.
- **Транзакционность и ошибки**: любые ошибки при чтении или вставке откатывают транзакцию, оставляя исходную и целевую базы нетронутыми и возвращая `ConflictPreserved`.
- **Валидация и бэкап**:
  - Перед диагностикой доступности баз выполняется проверка `inspect_database_data_state` (`Missing`, `Empty`, `ContainsData`, `Unreadable`). Повреждённые или нечитаемые базы отклоняются с `ConflictPreserved` без автоматической замены.
  - Вычисление fingerprint учитывает uncheckpointed WAL-страницы через создание временного snapshot via SQLite Backup API.
  - После восстановления или слияния выполняются `PRAGMA integrity_check` (проверка ответа `"ok"`) и `PRAGMA foreign_key_check` (проверка `stmt.exists([])?`).
  - При восстановлении в `legacy_imports` записывается completed-маркер источника, исключающий повторное удвоение записей.
  - Файл `.empty.bak` вместе со своими sidecar-файлами (`.empty.bak-wal`, `.empty.bak-shm`) сохраняется в качестве диагностического бэкапа.
  - Старая база переименовывается в `.sqlite3.migrated.bak` (с очисткой устаревших WAL/SHM) только после полного успеха транзакции, проверок и повторного открывающего теста целевой базы через `Repository::open`.

### Поддерживаемые сценарии

1. **Сценарий A (Только старый каталог)**:
   Восстанавливает старую базу в `~/.local/share/kitsupin/kitsupin.sqlite3` через SQLite Backup API, заносит реестровую запись в `legacy_imports` и переносит старую базу в backup.
2. **Сценарий B (Существует каталог kitsupin без kitsupin.sqlite3)**:
   Очищает оставшиеся sidecar-файлы (`-wal`, `-shm`), переносит старую базу через SQLite Backup API, заносит запись в `legacy_imports`, проверяет `integrity_check` и `foreign_key_check`, после чего переименовывает старую базу в backup.
3. **Сценарий C (kitsupin.sqlite3 существует, но фактически пуст)**:
   Перемещает пустую новую базу и её sidecar-файлы в `kitsupin.sqlite3.empty.bak*`, переносит старую базу через SQLite Backup API с записью в `legacy_imports` и проверкой целостности, сохраняя `.empty.bak`.
4. **Сценарий D (Обе базы содержат пользовательские данные)**:
   Открывает и мигрирует новую базу до актуальной схемы v1–9 via `Repository::open`, затем выполняет транзакционный merge legacy Pastily v1 с проверкой `legacy_imports` и подсчетом отчета `LegacyMergeReport`.

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
             │    └─ Если найден незарезервированный event: сразу создаёт карточку с доменом и заголовком
             └─ Иначе (Metadata after Clipboard):
                  └─ Создаёт domainless-карточку + генерирует ClipUpsertReceipt
                       └─ Pushes receipt в MetadataBuffer
```

### Прикрепление метаданных (reconciliation)

- `reserve_matching_pair` под единым мьютексом атомарно завышает флаг `reserved = true` и у `BufferedEvent`, и у `ClipUpsertReceipt`.
- Метод `take_match` игнорирует зарезервированные события (`reserved == true`), предотвращая параллельную кражу события в Clipboard watcher.
- Метод `attach_metadata_with_receipt` атомарно проверяет наличие карточки по `receipt.clip_id`, совпадение `content_hash`, `domain_key == ''`, и совпадение текущего состояния DB данным receipt (`copy_count`, `last_copied_at`).
- При успешном прикреплении (`Ok(Some(canonical_id))`) обе записи удаляются из `MetadataBuffer` (`acknowledge_pair`).
- При получении `Ok(None)` (устаревший receipt, не соответствующий текущему состоянию DB) удаляется только несостоятельный receipt (`remove_receipt`), а событие освобождается (`release_event`), оставаясь доступным для свежего receipt или `take_match`.
- При ошибках БД (`Err`) бронирование снимется с обоих объектов (`release_receipt` и `release_event`).
- Устаревшие незарезервированные events и receipts старше 10 000 мс автоматически очищаются (`cleanup_stale`).

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
- **9**: Добавление реестровой таблицы `legacy_imports` для учёта и дедупликации импортированных legacy-баз.

---

## CI

Workflow: `.github/workflows/ci.yml`

1. `frontend`: `npm ci` · `npm run lint` · `npm test` · `npm run build`
2. `rust-core`:
   - `cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check`
   - `cargo clippy --manifest-path src-tauri/core-tests/Cargo.toml --all-targets -- -D warnings`
   - `cargo test --manifest-path src-tauri/core-tests/Cargo.toml`
3. `tauri-check`:
   - Установка системных зависимостей (GTK 3, WebKit2GTK 4.1, AppIndicator3, X11 libraries)
   - `npm ci` · `npm run build`
   - `cargo check --manifest-path src-tauri/Cargo.toml`
   - `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`
   - `cargo test --manifest-path src-tauri/Cargo.toml`
   - `npm run tauri build -- --bundles deb`

---

Статус: ранний MVP, требуется системное тестирование на Ubuntu 24.04 KDE X11.
