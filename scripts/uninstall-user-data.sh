#!/usr/bin/env bash
set -euo pipefail
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/pastily.desktop"
rm -f "${XDG_CONFIG_HOME:-$HOME/.config}/google-chrome/NativeMessagingHosts/app.pastily.native.json"
echo "Интеграции удалены. Данные оставлены в ${XDG_DATA_HOME:-$HOME/.local/share}/pastily"
echo "Для удаления данных вручную закройте Pastily и удалите этот конкретный каталог."
