#!/usr/bin/env bash
set -euo pipefail
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/kitsupin.desktop"
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/google-chrome/NativeMessagingHosts/io.github.mootxed.kitsupin.native.json"
echo "Интеграции удалены. Данные оставлены в ${XDG_DATA_HOME:-$HOME/.local/share}/kitsupin"
echo "Для удаления данных вручную закройте KitsuPin и удалите этот конкретный каталог."
