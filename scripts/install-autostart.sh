#!/usr/bin/env bash
set -euo pipefail
binary="${1:-$HOME/.local/bin/pastily}"
if [[ ! -x "$binary" ]]; then echo "Pastily не найден: $binary" >&2; exit 1; fi
directory="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"; mkdir -p "$directory"
sed "s|__PASTILY_PATH__|$binary|g" packaging/pastily.desktop > "$directory/pastily.desktop"
chmod 644 "$directory/pastily.desktop"; echo "Автозапуск включён"
