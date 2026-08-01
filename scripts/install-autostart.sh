#!/usr/bin/env bash
set -euo pipefail
binary="${1:-$HOME/.local/bin/kitsupin}"
if [[ ! -x "$binary" ]]; then echo "KitsuPin не найден: $binary" >&2; exit 1; fi
directory="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"; mkdir -p "$directory"
sed "s|__KITSUPIN_PATH__|$binary|g" packaging/kitsupin.desktop > "$directory/kitsupin.desktop"
chmod 644 "$directory/kitsupin.desktop"; echo "Автозапуск включён"
