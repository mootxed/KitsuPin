#!/usr/bin/env bash
set -euo pipefail

RPM_FILE="${1:-}"

if [[ -z "$RPM_FILE" || ! -f "$RPM_FILE" ]]; then
  echo "ОШИБКА: Укажите существующий .rpm файл для проверки." >&2
  echo "Использование: $0 <path-to-rpm>" >&2
  exit 1
fi

echo "=== Проверка структуры и метаданных RPM-пакета: $RPM_FILE ==="

if command -v rpm >/dev/null 2>&1; then
  echo "--- 1. Метаданные пакета (rpm -qpi) ---"
  rpm -qpi "$RPM_FILE"

  echo "--- 2. Зависимости пакета (rpm -qp --requires) ---"
  rpm -qp --requires "$RPM_FILE"

  echo "--- 3. Скрипты пакета (rpm -qp --scripts) ---"
  rpm -qp --scripts "$RPM_FILE" || true

  CONTENTS=$(rpm -qpl "$RPM_FILE")
  VERBOSE_CONTENTS=$(rpm -qplv "$RPM_FILE")
elif command -v rpm2cpio >/dev/null 2>&1 && command -v cpio >/dev/null 2>&1; then
  CONTENTS=$(rpm2cpio "$RPM_FILE" | cpio -t 2>/dev/null)
  VERBOSE_CONTENTS=$(rpm2cpio "$RPM_FILE" | cpio -tv 2>/dev/null)
else
  echo "::warning::Утилита rpm/rpm2cpio не найдена, пропускается чтение содержимого."
  exit 0
fi

# Helper function to assert a regex pattern is present in rpm contents
require_path() {
  local pattern="$1"
  local description="$2"
  if ! printf '%s\n' "$CONTENTS" | grep -Eq "$pattern"; then
    echo "::error::ОШИБКА: $description" >&2
    exit 1
  fi
}

# Helper function to assert a regex pattern is ABSENT in rpm contents
forbidden_path() {
  local pattern="$1"
  local description="$2"
  if printf '%s\n' "$CONTENTS" | grep -Eq "$pattern"; then
    echo "::error::ОШИБКА: $description" >&2
    exit 1
  fi
}

# 4. Обязательные файлы и каталоги
require_path \
  '(^|[[:space:]])(\./|/)?usr/bin/kitsupin$' \
  'Основной бинарник /usr/bin/kitsupin отсутствует в RPM пакете!'

require_path \
  '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/kitsupin-native-host$' \
  'Native host /usr/lib/kitsupin/kitsupin-native-host отсутствует в RPM пакете!'

require_path \
  '(^|[[:space:]])(\./|/)?etc/xdg/autostart/kitsupin\.desktop$' \
  'Autostart-файл /etc/xdg/autostart/kitsupin.desktop отсутствует в RPM пакете!'

require_path \
  '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/resources/chrome-extension/manifest\.json$' \
  'Chrome extension manifest.json в /usr/lib/kitsupin/resources/chrome-extension/ отсутствует!'

require_path \
  '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/resources/scripts/uninstall-user-data\.sh$' \
  'Скрипт uninstall-user-data.sh в /usr/lib/kitsupin/resources/scripts/ отсутствует!'

# 5. Запрещённый дублирующий каталог /usr/lib/KitsuPin
forbidden_path \
  '(^|[[:space:]])(\./|/)?usr/lib/KitsuPin(/|$)' \
  'RPM Пакет содержит запрещённый дублирующий каталог /usr/lib/KitsuPin!'

# 6. Единственность Chrome extension manifest
EXT_COUNT=$(printf '%s\n' "$CONTENTS" | grep -Ec '/chrome-extension/manifest\.json$')
if [[ "$EXT_COUNT" -ne 1 ]]; then
  echo "::error::ОШИБКА: Ожидался ровно 1 Chrome extension manifest, найдено: $EXT_COUNT" >&2
  exit 1
fi

# 7. Единственность Native Host binary в /usr/lib/kitsupin/
HOST_COUNT=$(printf '%s\n' "$CONTENTS" | grep -Ec '(^|[[:space:]])(\./|/)?usr/lib/kitsupin/kitsupin-native-host$')
if [[ "$HOST_COUNT" -ne 1 ]]; then
  echo "::error::ОШИБКА: Ожидался ровно 1 native host бинарник в /usr/lib/kitsupin/, найдено: $HOST_COUNT" >&2
  exit 1
fi

# 8. Проверка прав на исполнение бинарников (x bit)
if [[ -n "${VERBOSE_CONTENTS:-}" ]]; then
  echo "--- 4. Проверка прав на исполнение бинарников ---"
  KITSUPIN_PERM=$(printf '%s\n' "$VERBOSE_CONTENTS" | grep -E 'usr/bin/kitsupin$' || true)
  NATIVE_HOST_PERM=$(printf '%s\n' "$VERBOSE_CONTENTS" | grep -E 'usr/lib/kitsupin/kitsupin-native-host$' || true)

  if [[ -n "$KITSUPIN_PERM" ]] && ! echo "$KITSUPIN_PERM" | grep -qE '^[-rwx]{3,10}x'; then
    echo "::error::ОШИБКА: /usr/bin/kitsupin не имеет флага исполнения: $KITSUPIN_PERM" >&2
    exit 1
  fi

  if [[ -n "$NATIVE_HOST_PERM" ]] && ! echo "$NATIVE_HOST_PERM" | grep -qE '^[-rwx]{3,10}x'; then
    echo "::error::ОШИБКА: /usr/lib/kitsupin/kitsupin-native-host не имеет флага исполнения: $NATIVE_HOST_PERM" >&2
    exit 1
  fi
  echo " Права на исполнение подтверждены для kitsupin и kitsupin-native-host."
fi

echo " УСПЕХ: Структура и метаданные .rpm пакета полностью соответствуют требованиям!"
