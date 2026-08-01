# KitsuPin

KitsuPin — локальный менеджер текстовой истории обычного X11 Clipboard для
Ubuntu 24.04, KDE Plasma и Google Chrome. Приложение не отслеживает PRIMARY
selection, не сохраняет полный URL и не отправляет данные в интернет.

> Статус: рабочий MVP. Поддерживаются только X11, KDE Plasma и Google Chrome.
> Wayland, Firefox, изображения, файлы и HTML намеренно не входят в MVP.

## Возможности

- фоновое наблюдение за обычным X11 Clipboard;
- SQLite-история в постоянном XDG data directory;
- автоматические типы Text, Links, Email и Numbers;
- дедупликация по `нормализованный текст + домен`;
- поиск по содержимому, домену, заголовку и пользовательским категориям;
- несколько цветных пользовательских категорий на одной карточке;
- закрепление, удаление и очистка незакреплённой истории;
- основное окно и быстрый popup с клавиатурной навигацией;
- KDE System Tray и глобальная клавиша `Super+V`;
- Chrome Extension MV3 + Native Messaging без полного URL;
- XDG Autostart и настраиваемое хранение (90 дней по умолчанию).

## Требования

- Ubuntu 24.04;
- сеанс KDE Plasma **X11** (`echo $XDG_SESSION_TYPE` должен вывести `x11`);
- Google Chrome;
- Node.js 20+ и Rust stable для сборки.

Системные зависимости Tauri:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config curl file \
  libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libssl-dev libxdo-dev
```

## Запуск для разработки

```bash
npm install
npm run tauri dev
```

Без desktop shell интерфейс можно проверить в браузере (показываются явно
локальные демонстрационные карточки, системный Clipboard watcher в этом режиме
не работает):

```bash
npm run dev
```

## Сборка и тесты

```bash
npm run lint
npm test
npm run build
cargo test --manifest-path src-tauri/core-tests/Cargo.toml
npm run tauri build
```

Последняя команда создаёт `.deb` в `src-tauri/target/release/bundle/deb/`.
Rust core вынесен в отдельный test harness, поэтому доменную логику и SQLite
можно проверять даже на build-машине без GTK/WebKit headers.

## Chrome Extension и Native Messaging

1. Соберите приложение и установите `.deb`.
2. Откройте `chrome://extensions`, включите «Режим разработчика».
3. Нажмите «Загрузить распакованное расширение» и выберите
   `chrome-extension/` из репозитория либо `/usr/lib/KitsuPin/chrome-extension/`
   после установки `.deb`.
4. Скопируйте ID появившегося расширения.
5. Убедитесь, что `kitsupin-native-host` доступен в `PATH` (либо задайте
   `KITSUPIN_NATIVE_HOST=/полный/путь/kitsupin-native-host`).
6. Выполните:

```bash
chmod +x scripts/install-native-host.sh
./scripts/install-native-host.sh ID_ИЗ_CHROME
```

После установки `.deb` тот же скрипт находится в
`/usr/lib/KitsuPin/scripts/install-native-host.sh`.

Manifest устанавливается только для текущего пользователя в
`~/.config/google-chrome/NativeMessagingHosts/io.github.mootxed.kitsupin.native.json`.
`allowed_origins` содержит точный ID расширения без wildcard.

При копировании content script нормализует текст, вычисляет SHA-256 и передаёт
native host только хеш, байтовую длину, нормализованный hostname, `document.title`
и timestamp. Фоновый процесс прикрепляет metadata лишь при совпадении хеша и
длины в окне 2,5 секунды. При сомнении карточка сохраняется без источника.

## Автозапуск и горячая клавиша

Автозапуск включается в настройках или вручную:

```bash
chmod +x scripts/install-autostart.sh
./scripts/install-autostart.sh /полный/путь/к/kitsupin
```

Пакет устанавливает `/etc/xdg/autostart/kitsupin.desktop`, который автоматически
удаляется вместе с `.deb`. При отключении настройки KitsuPin создаёт стандартный
пользовательский override `~/.config/autostart/kitsupin.desktop` с `Hidden=true`.
В dev-режиме используется обычный пользовательский autostart-файл. Горячая
клавиша по умолчанию — `Super+V`; её можно изменить в настройках. Если комбинация
занята, KitsuPin продолжит работать и запишет ошибку в локальный лог.

## Данные, логи и удаление

- база: `${XDG_DATA_HOME:-~/.local/share}/kitsupin/kitsupin.sqlite3`;
- настройки: `${XDG_DATA_HOME:-~/.local/share}/kitsupin/settings.json`;
- socket: `${XDG_DATA_HOME:-~/.local/share}/kitsupin/native.sock`;
- логи: стандартный XDG log directory плагина Tauri (`KitsuPin/logs`).

Удалить пользовательские интеграции, оставив данные:

```bash
chmod +x scripts/uninstall-user-data.sh
./scripts/uninstall-user-data.sh
```

После завершения KitsuPin данные можно удалить вручную, удалив только конкретный
каталог `${XDG_DATA_HOME:-$HOME/.local/share}/kitsupin`.

## Модель безопасности и ограничения

- содержимое Clipboard никогда не пишется в лог;
- Native Host принимает только `copy` protocol v1 и status probe, имеет лимит
  16 КБ и не поддерживает команды выполнения;
- Unix socket доступен владельцу (`0600`);
- расширение не имеет сетевых API, не хранит содержимое и не получает полный URL;
- password field нельзя надёжно распознать по системному X11 Clipboard;
- blacklist приложений оставлен в модели настроек как точка расширения, но не
  включён: определить приложение-источник X11 Clipboard надёжно во всех случаях
  без чрезмерной сложности нельзя;
- popup центрируется на активном экране: точное позиционирование у KDE tray
  неодинаково между конфигурациями панели;
- наблюдение получает события смены владельца `CLIPBOARD` через XFixes; интервал
  350 мс используется только как fallback, если XFixes недоступен;
- запросы не перечитывают базу целиком, а popup ограничен 16 результатами.

Подробная структура и поток данных описаны в [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
