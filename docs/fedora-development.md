# Инструкция по разработке и сборке KitsuPin на Fedora Linux (KDE Plasma)

Данное руководство содержит все необходимые инструкции по настройке окружения разработки, сборке приложения и формированию `.rpm` пакета на Fedora Linux.

---

## 1. Установка зависимостей в Fedora

Перед сборкой установите необходимые пакеты компиляции, библиотек GTK/WebKit, X11 и инструменты пакетирования через `dnf`:

```bash
sudo dnf install -y \
  gcc \
  gcc-c++ \
  make \
  pkg-config \
  patchelf \
  rust \
  cargo \
  nodejs \
  npm \
  webkit2gtk4.1-devel \
  gtk3-devel \
  libayatana-appindicator-gtk3-devel \
  openssl-devel \
  libX11-devel \
  libXi-devel \
  libXtst-devel \
  libXfixes-devel \
  rpm-build
```

---

## 2. Разработка и тестирование

### Установка зависимостей Frontend
```bash
npm install
```

### Запуск тестов
```bash
# Тесты фронтенда
npm test

# Линтинг фронтенда
npm run lint

# Юнит-тесты библиотеки Rust
cargo test --manifest-path src-tauri/Cargo.toml

# Тесты ядера (core-tests)
cargo test --manifest-path src-tauri/core-tests/Cargo.toml
```

### Запуск в режиме разработки
```bash
npm run tauri dev
```

---

## 3. Сборка RPM-пакета

Сборка локального `.rpm` пакета выполняется следующим образом:

1. Подготовка конфигурации стейджинга и ресурсов:
```bash
./scripts/prepare-release-packaging.sh dev
```

2. Компиляция и создание RPM-бандла через Tauri CLI:
```bash
npm run tauri build -- --bundles rpm -c staging/tauri.conf.json
```

3. Готовый пакет находится в директории:
```text
src-tauri/target/release/bundle/rpm/kitsupin-0.1.5-1.x86_64.rpm
```

### Проверка состава и структуры пакета:
```bash
./scripts/verify-rpm-layout.sh src-tauri/target/release/bundle/rpm/kitsupin-*.rpm
```
или через стандартную утилиту Fedora:
```bash
rpm -qpl src-tauri/target/release/bundle/rpm/kitsupin-*.rpm
```

---

## 4. Установка и удаление RPM пакета

### Установка локального пакета:
```bash
sudo dnf install ./src-tauri/target/release/bundle/rpm/kitsupin-*.rpm
```

### Проверка автозапуска и файлов:
```bash
# Проверка наличия бинарных файлов
which kitsupin
kitsupin --version

# Проверка файлов автозапуска и системного моста браузера
ls -la /usr/lib/kitsupin/kitsupin-native-host
ls -la /etc/xdg/autostart/kitsupin.desktop
```

### Удаление пакета:
```bash
sudo dnf remove kitsupin
```

### Удаление пользовательских данных (при необходимости):
```bash
/usr/lib/kitsupin/resources/scripts/uninstall-user-data.sh
rm -rf ~/.local/share/kitsupin
```

---

## 5. Особенности работы под KDE Plasma (X11 vs Wayland)

Для проверки типа вашей текущей графической сессии выполните:
```bash
echo "$XDG_SESSION_TYPE"
```

- **Сессия X11 (`XDG_SESSION_TYPE=x11`)**:
  - Полная поддержка мгновенного фонового отслеживания глобального буфера обмена (через XFixes).
  - Поддержка изображений и текста.
  - Поддержка системного трея и глобальных комбинаций клавиш.

- **Сессия Wayland (`XDG_SESSION_TYPE=wayland`)**:
  - В соответствии с архитектурой безопасности Wayland и KDE Plasma (KWin), прямой пассивный доступ фоновых приложений к буферу обмена заблокирован политиками сессии.
  - KitsuPin запускается в защищённом **ограниченном режиме** (`wayland-limited`). Приложение выполняет роли просмотрщика истории, с предупредительным баннером в GUI.
  - Браузерное расширение передаёт только метаданные копирования (хеш, длину, домен, заголовок), поэтому без системного монитора фоновые карточки автоматического копирования не создаются.

---

## 6. Проверка RPM в CI (Fedora 43 Container)

В GitHub Actions CI для пакетов RPM настроен специализированный этап тестирования в контейнере `fedora:43`:

1. Сборка `.rpm` пакета под Ubuntu 24.04 (для стабильности `glibc`).
2. Передача пакета в контейнер `fedora:43`.
3. Установка пакета через `dnf install -y ./KitsuPin_*.rpm`.
4. Проверка информации и зависимостей пакета (`rpm -qpi`, `rpm -qp --requires`, `rpm -qp --scripts`).
5. Проверка прав исполнения бинарников (`kitsupin` и `kitsupin-native-host`).
6. Выполнение CLI-тестов: `rpm -q kitsupin`, `kitsupin --version`, `kitsupin --help`, `kitsupin --diagnose`.

