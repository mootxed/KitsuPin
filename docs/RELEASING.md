# Регламент выпуска релиза KitsuPin (Release Guide)

В данном документе описан пошаговый процесс подготовки, проверки и публикации новых версий KitsuPin.

---

## 1. Совпадение версий

Перед созданием релиза убедитесь, что версия одинакова во всех 4 файлах проекта:

* `package.json` → `"version": "X.Y.Z"`
* `src-tauri/tauri.conf.json` → `"version": "X.Y.Z"`
* `src-tauri/Cargo.toml` → `version = "X.Y.Z"`
* `chrome-extension/manifest.json` → `"version": "X.Y.Z"`

Если версии расходятся, GitHub Release workflow завершится с ошибкой `::error::Версии не совпадают`.

---

## 2. Настройка Production Chrome Extension ID

Для полноценного production-релиза Chrome-расширение публикуется в Chrome Web Store.
После публикации браузер присваивает расширению постоянный ID (32 символа `a–p`).

### Настройка в GitHub Actions:

1. Перейдите в репозитории: **Settings → Secrets and variables → Actions → Variables**.
2. Создайте переменную: `KITSUPIN_CHROME_EXTENSION_ID`.
3. Укажите в качестве значения 32-значный ID опубликованного расширения (например, `abcdefghijklmnopabcdefghijklmnop`).

Если эта переменная установлена, сборщик автоматически добавит в `.deb`:
* `/etc/opt/chrome/native-messaging-hosts/io.github.mootxed.kitsupin.native.json`
* `/usr/share/google-chrome/extensions/<EXTENSION_ID>.json`

Если переменная отсутствует (Alpha/Dev режим), пакет выйдет в режиме разработки, а пользователь сможет ввести ID в окне `Настройки → Интеграция`.

---

## 3. Проверка перед релизом (Pre-flight checks)

Выполните в терминале перед созданием тега:

```bash
# 1. Проверка frontend
npm ci
npm run lint
npm test
npm run build

# 2. Проверка Rust backend
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/core-tests/Cargo.toml

# 3. Сборка хоста и подготовка packaging
cargo build --release --manifest-path src-tauri/Cargo.toml --bin kitsupin-native-host
./scripts/prepare-release-packaging.sh dev

# 4. Пробная сборка deb пакета
npm run tauri build -- --bundles deb -c staging/tauri.conf.json
```

---

## 4. Публикация нового релиза

1. Закоммитьте все изменения:
   ```bash
   git add .
   git commit -m "chore: bump version to v0.1.0"
   git push origin main
   ```
2. Создайте аннотированный тег версии и отправьте его на GitHub:
   ```bash
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```
3. GitHub Actions автоматически запустит `.github/workflows/release.yml`.
4. После завершения workflow артефакты `KitsuPin_0.1.0_amd64.deb` и `KitsuPin_0.1.0_amd64.deb.sha256` будут прикреплены к созданному GitHub Release.

---

## 5. Проверка установки на чистой Ubuntu 24.04

Скачайте файл `.deb` с GitHub Releases и проверьте его:

```bash
# Проверка состава пакета
dpkg-deb --info KitsuPin_0.1.0_amd64.deb
dpkg-deb --contents KitsuPin_0.1.0_amd64.deb

# Пробная установка
sudo apt install ./KitsuPin_0.1.0_amd64.deb

# Проверка CLI диагностических флагов
kitsupin --version
kitsupin --diagnose
```

---

## 6. Откат неудачного релиза (Rollback)

Если в релизе обнаружена критическая ошибка:

1. Удалите GitHub Release в графическом интерфейсе GitHub.
2. Удалите тег локально и на сервере:
   ```bash
   git tag -d v0.1.0
   git push --delete origin v0.1.0
   ```
3. Исправьте ошибку, повысьте патч-версию (например, `v0.1.1`) и повторите процедуру.

---

## 7. Безопасность и секреты

* **Приватные ключи (`*.pem`, `*.key`, `*.crx`) строго запрещено коммитить в Git**. Они внесены в `.gitignore`.
* В `allowed_origins` манифеста никогда не используется wildcard (`*`).
* Права на бинарные файлы установлены как `0755`, на конфигурационные манифесты — `0644`.
