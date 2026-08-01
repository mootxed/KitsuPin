# Архитектура KitsuPin MVP

KitsuPin состоит из одного долгоживущего Tauri-процесса, двух webview-окон и
минимального native messaging binary. Все пользовательские данные остаются в
`$XDG_DATA_HOME/kitsupin` (или `~/.local/share/kitsupin`).

## Слои

- `src-tauri/src/domain` — нормализация, классификация, модели, дедупликация.
- `src-tauri/src/persistence` — SQLite, миграции, запросы и транзакции.
- `src-tauri/src/clipboard` — события XFixes только для X11 `CLIPBOARD`, чтение и
  подавление собственных событий; polling 350 мс остаётся аварийным fallback.
- `src-tauri/src/browser_metadata` — Unix socket и краткоживущий буфер событий Chrome.
- `src-tauri/src/jobs` — очистка устаревшей незакреплённой истории.
- `src-tauri/src/settings` — настройки и XDG Autostart.
- `src-tauri/src/app` — команды Tauri, окна, tray и глобальная клавиша.
- `src/` — общий TypeScript UI; режим выбирается по label окна.
- `chrome-extension/` — Manifest V3 content script, service worker и status page.
- `scripts/` — установка native manifest и autostart для текущего пользователя.

## Поток копирования

Content script вычисляет SHA-256 нормализованного текста и передаёт только хеш,
длину, hostname и title. Native host валидирует JSON и пересылает событие в Unix
socket. Clipboard watcher замечает новое содержимое, вычисляет тот же хеш и ищет
самое свежее событие в окне 2.5 секунды. Метаданные прикрепляются только при
совпадении хеша и длины. Затем транзакция делает upsert по
`normalized_content + domain`, а UI получает событие `clips-changed`.

PRIMARY selection нигде не запрашивается: XFixes подписан только на atom
`CLIPBOARD`, а `arboard` читает обычный X11 Clipboard.
