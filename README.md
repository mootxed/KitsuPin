# KitsuPin

KitsuPin — локальный менеджер текстовой истории обычного X11 Clipboard для Ubuntu 24.04, KDE Plasma и Google Chrome. Приложение сохраняет историю скопированного текста, автоматически определяет типы данных и связывает скопированный текст с доменом и заголовком страницы из Chrome. Приложение не отслеживает PRIMARY selection, не сохраняет полный URL страниц и не отправляет данные в сеть.

---

## Установка

1. Откройте страницу последних выпусков: **[GitHub Releases](../../releases)**.
2. Скачайте пакет `KitsuPin_<version>_amd64.deb`.
3. Установите пакет двойным кликом (через Discover или Центр приложений):
   ```bash
   sudo apt install ./KitsuPin_<version>_amd64.deb
   ```
4. Запустите **KitsuPin** из меню приложений KDE.
5. Перейдите в **«Настройки → Интеграция»** и проверьте состояние работы Chrome и системных компонентов.

---

## Системные требования

* **Операционная система**: Ubuntu 24.04 LTS (x86_64)
* **Рабочее окружение**: KDE Plasma (сеанс **X11**)
* **Браузер**: Google Chrome / Chromium

> **Примечание**: Сеанс Wayland, Firefox, растровые изображения и файлы в текущий релиз не входят.

---

## Первый запуск и интеграция с Chrome

### Production-режим (Chrome Web Store)

Если пакет собран с официальным Chrome Web Store Extension ID, расширение и Native Messaging manifest регистрируются в системе автоматически при установке `.deb`. Пользователю не требуется выполнять никаких ручных действий.

### Alpha / Unpacked fallback-режим

Если Chrome-расширение загружается локально (разработка или раннее alpha-тестирование):

1. Запустите KitsuPin и откройте **«Настройки → Интеграция»**.
2. Нажмите кнопку **«Открыть chrome://extensions»** и включите «Режим разработчика».
3. Нажмите **«Открыть папку расширения»** и выберите предложенную директорию через «Загрузить распакованное расширение» в Chrome.
4. Скопируйте 32-значный ID появившегося расширения (например, `abcdefghijklmnopabcdefghijklmnop`).
5. Вставьте скопированный ID в поле на вкладке **«Интеграция»** и нажмите **«Сохранить ID»**.
6. KitsuPin автоматически создаст пользовательский Native Messaging manifest без использования терминала.

---

## Обновление и Удаление

### Обновление

Скачайте новый `.deb` с GitHub Releases и установите его поверх текущей версии. База данных истории и настройки сохранятся.

### Удаление пакета

```bash
sudo apt remove kitsu-pin
```

При стандартном удалении системные файлы приложения удаляются, а ваша личная база данных истории и категории сохраняются в `~/.local/share/kitsupin/`.

### Полное удаление пользовательских данных

Если вы хотите полностью очистить пользовательские данные KitsuPin:

```bash
/usr/lib/kitsupin/resources/scripts/uninstall-user-data.sh
rm -rf ~/.local/share/kitsupin
```

---

## Диагностика проблем

В KitsuPin встроен автономный инструмент диагностики:

```bash
kitsupin --diagnose
```

Команда выводит подробный отчёт о состоянии ОС, X11-сеанса, наличии исполняемого файла хоста, доступности UNIX socket и корректности манифестов.

Другие полезные CLI-флаги:
* `kitsupin --version` — показать текущую версию
* `kitsupin --help` — показать справку по CLI-опциям

---

## Сборка из исходников

Для сборки из исходного кода потребуется Node.js 20+, Rust stable и системные библиотеки разработки Ubuntu 24.04:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config curl file \
  libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev \
  librsvg2-dev libssl-dev libx11-dev libxi-dev libxtst-dev libxfixes-dev
```

### Запуск в режиме разработки

```bash
npm install
npm run tauri dev
```

### Выполнение тестов и линтеров

```bash
npm run lint
npm test
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/core-tests/Cargo.toml
```

### Локальная сборка `.deb`

```bash
cargo build --release --manifest-path src-tauri/Cargo.toml --bin kitsupin-native-host
./scripts/prepare-release-packaging.sh dev
npm run tauri build -- --bundles deb -c staging/tauri.conf.json
```

Собранный `.deb` появится в `src-tauri/target/release/bundle/deb/`.

---

## Выпуск новой версии

Инструкции по подготовке релизов, установке переменных окружения и управлению тегами описаны в документе **[docs/RELEASING.md](docs/RELEASING.md)**.
