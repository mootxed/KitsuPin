#!/usr/bin/env bash
set -euo pipefail

DEB_FILE="${1:-}"

if [[ -z "$DEB_FILE" || ! -f "$DEB_FILE" ]]; then
  echo "ОШИБКА: Укажите существующий .deb файл для проверки." >&2
  echo "Использование: $0 <path-to-deb>" >&2
  exit 1
fi

echo "=== Проверка структуры Debian-пакета: $DEB_FILE ==="

CONTENTS=$(dpkg-deb --contents "$DEB_FILE")

# Helper function to assert a regex pattern is present in dpkg-deb contents
require_path() {
  local pattern="$1"
  local description="$2"
  if ! printf '%s\n' "$CONTENTS" | grep -Eq "$pattern"; then
    echo "::error::ОШИБКА: $description" >&2
    exit 1
  fi
}

# Helper function to assert a regex pattern is ABSENT in dpkg-deb contents
forbidden_path() {
  local pattern="$1"
  local description="$2"
  if printf '%s\n' "$CONTENTS" | grep -Eq "$pattern"; then
    echo "::error::ОШИБКА: $description" >&2
    exit 1
  fi
}

# 1. Обязательные файлы и каталоги
require_path \
  '(^|[[:space:]])(\./|/)?usr/bin/kitsupin$' \
  'Основной бинарник /usr/bin/kitsupin отсутствует в пакете!'

require_path \
  '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/kitsupin-native-host$' \
  'Native host /usr/lib/kitsupin/kitsupin-native-host отсутствует в пакете!'

require_path \
  '(^|[[:space:]])(\./|/)?etc/xdg/autostart/kitsupin\.desktop$' \
  'Autostart-файл /etc/xdg/autostart/kitsupin.desktop отсутствует в пакете!'

require_path \
  '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/resources/chrome-extension/manifest\.json$' \
  'Chrome extension manifest.json в /usr/lib/kitsupin/resources/chrome-extension/ отсутствует!'

require_path \
  '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/resources/scripts/uninstall-user-data\.sh$' \
  'Скрипт uninstall-user-data.sh в /usr/lib/kitsupin/resources/scripts/ отсутствует!'

# 2. Запрещённый дублирующий каталог /usr/lib/KitsuPin
forbidden_path \
  '(^|[[:space:]])(\./|/)?usr/lib/KitsuPin(/|$)' \
  'Пакет содержит запрещённый дублирующий каталог /usr/lib/KitsuPin!'

# 3. Единственность Chrome extension manifest
EXT_COUNT=$(printf '%s\n' "$CONTENTS" | grep -Ec '/chrome-extension/manifest\.json$')
if [[ "$EXT_COUNT" -ne 1 ]]; then
  echo "::error::ОШИБКА: Ожидался ровно 1 Chrome extension manifest, найдено: $EXT_COUNT" >&2
  exit 1
fi

# 4. Единственность Native Host binary в /usr/lib/kitsupin/
HOST_COUNT=$(printf '%s\n' "$CONTENTS" | grep -Ec '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/kitsupin-native-host$')
if [[ "$HOST_COUNT" -ne 1 ]]; then
  echo "::error::ОШИБКА: Ожидался ровно 1 native host бинарник в /usr/lib/kitsupin/, найдено: $HOST_COUNT" >&2
  exit 1
fi

# 5. Отсутствие старых dev-скриптов и шаблонов
forbidden_path \
  'install-native-host\.sh' \
  'Пакет не должен содержать dev-скрипт install-native-host.sh!'

forbidden_path \
  'native-host-manifest\.template\.json' \
  'Пакет не должен содержать dev-шаблон native-host-manifest.template.json!'

# 6. Отсутствие placeholder-путей
forbidden_path \
  '__KITSUPIN_PATH__' \
  'Пакет содержит неразвёрнутый placeholder __KITSUPIN_PATH__!'

echo " УСПЕХ: Структура .deb пакета полностью соответствует требованиям!"
