#!/usr/bin/env bash
set -euo pipefail
extension_id="${1:-}"
if [[ ! "$extension_id" =~ ^[a-p]{32}$ ]]; then
  echo "Использование: $0 <ID расширения из chrome://extensions>" >&2
  exit 2
fi
binary="${KITSUPIN_NATIVE_HOST:-$(command -v kitsupin-native-host || command -v kitsupin || true)}"
if [[ ! -x "$binary" ]]; then
  echo "Native Host не найден. Задайте KITSUPIN_NATIVE_HOST=/полный/путь/kitsupin-native-host" >&2
  exit 1
fi
manifest_dir="${XDG_CONFIG_HOME:-$HOME/.config}/google-chrome/NativeMessagingHosts"
mkdir -p "$manifest_dir"
manifest="$manifest_dir/io.github.mootxed.kitsupin.native.json"
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
sed -e "s|__HOST_PATH__|$binary|g" -e "s|__EXTENSION_ID__|$extension_id|g" "$script_dir/native-host-manifest.template.json" > "$manifest"
chmod 600 "$manifest"
echo "Установлен $manifest"
