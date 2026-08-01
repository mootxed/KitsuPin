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

## 2. Первый alpha-релиз (v0.1.0)

Первый релиз KitsuPin собирается в **alpha/dev packaging mode**, так как Chrome-расширение ещё не опубликовано в Chrome Web Store.

### Особенности alpha-релиза:
* Chrome Web Store Extension ID **не требуется** (`KITSUPIN_CHROME_EXTENSION_ID` не нужен).
* Пассивные системные манифесты Chrome в `/etc/opt/chrome/native-messaging-hosts/` не создаются.
* Chrome-расширение поставляется в виде распакованного каталога в `/usr/lib/kitsupin/resources/chrome-extension/`.
* Установка и привязка Chrome-расширения выполняется пользователем через экран диагностики / alpha fallback в интерфейсе приложения (ручной ввод extension ID).
* Workflow автоматически создает GitHub Release с флагом `prerelease: true`.
* Кнопку **Create a new release** в графическом интерфейсе GitHub вручную нажимать **не требуется**.

### Команды для публикации первого alpha-релиза v0.1.0:

```bash
git switch main
git pull --ff-only
git status

git tag -a v0.1.0 -m "KitsuPin v0.1.0 alpha"
git push origin v0.1.0
```

После отправки тега GitHub Actions автоматически запустит `.github/workflows/release.yml`, соберет `.deb` пакет и создаст prerelease.

---

## 3. Переход на Stable / Production релизы в будущем

После публикации Chrome-расширения в Chrome Web Store необходимо перевести workflow в **production packaging mode**:

1. **Опубликовать расширение в Chrome Web Store** и получить постоянный 32-символьный ID (буквы `a–p`).
2. **Добавить переменную в GitHub**:
   * Перейти в **Settings → Secrets and variables → Actions → Variables**.
   * Создать переменную `KITSUPIN_CHROME_EXTENSION_ID` с полученным 32-значным ID.
3. **Обновить workflow (`.github/workflows/release.yml`)**:
   * Переключить подготовку пакета с dev на prod:
     ```yaml
     - name: Prepare packaging files
       run: ./scripts/prepare-release-packaging.sh prod
       env:
         KITSUPIN_CHROME_EXTENSION_ID: ${{ vars.KITSUPIN_CHROME_EXTENSION_ID }}
     ```
   * Изменить флаг `prerelease` с `true` на `false`:
     ```yaml
     prerelease: false
     ```
4. Закоммитить изменения workflow в ветку `main`.

---

## 4. Проверка перед релизом (Pre-flight checks)

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

# 3. Сборка хоста и подготовка packaging (dev mode)
cargo build --release --manifest-path src-tauri/Cargo.toml --bin kitsupin-native-host
./scripts/prepare-release-packaging.sh dev

# 4. Пробная сборка deb пакета
npm run tauri build -- --bundles deb -c staging/tauri.conf.json
```

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
kitsupin --help
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
* При обновлении или повторной установке пакета пользовательские данные не удаляются.

