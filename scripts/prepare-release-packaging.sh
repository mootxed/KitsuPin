#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-dev}" # "prod" or "dev"

echo "=== Preparing Release Packaging (mode: $MODE) ==="

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"

# 1. Version Check
PKG_VER=$(jq -r '.version' "$ROOT_DIR/package.json")
TAURI_VER=$(jq -r '.version' "$ROOT_DIR/src-tauri/tauri.conf.json")
CARGO_VER=$(grep '^version = ' "$ROOT_DIR/src-tauri/Cargo.toml" | head -n1 | cut -d '"' -f2)
EXT_VER=$(jq -r '.version' "$ROOT_DIR/chrome-extension/manifest.json")

echo "package.json version: $PKG_VER"
echo "tauri.conf.json version: $TAURI_VER"
echo "Cargo.toml version: $CARGO_VER"
echo "chrome-extension manifest version: $EXT_VER"

if [[ "$PKG_VER" != "$TAURI_VER" || "$PKG_VER" != "$CARGO_VER" || "$PKG_VER" != "$EXT_VER" ]]; then
  echo " ОШИБКА: Версии не совпадают между манифестами!" >&2
  exit 1
fi

STAGING_DIR="$ROOT_DIR/staging"
rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR"

# Copy Chrome extension unpacked files to staging resources
EXT_RES_DIR="$STAGING_DIR/usr/lib/kitsupin/resources/chrome-extension"
mkdir -p "$EXT_RES_DIR"
cp -r "$ROOT_DIR/chrome-extension/"* "$EXT_RES_DIR/"
chmod -R 755 "$STAGING_DIR/usr/lib/kitsupin"
find "$EXT_RES_DIR" -type f -exec chmod 644 {} +

EXTENSION_ID="${KITSUPIN_CHROME_EXTENSION_ID:-}"

if [[ "$MODE" == "prod" || -n "$EXTENSION_ID" ]]; then
  if [[ ! "$EXTENSION_ID" =~ ^[a-p]{32}$ ]]; then
    echo " ОШИБКА: KITSUPIN_CHROME_EXTENSION_ID не задан или имеет неверный формат (требуется 32 символа a-p)!" >&2
    echo "Получено: '$EXTENSION_ID'" >&2
    exit 1
  fi
  echo "Production extension ID: $EXTENSION_ID"

  # System Native Messaging Manifest
  SYS_NATIVE_DIR="$STAGING_DIR/etc/opt/chrome/native-messaging-hosts"
  mkdir -p "$SYS_NATIVE_DIR"
  SYS_MANIFEST="$SYS_NATIVE_DIR/io.github.mootxed.kitsupin.native.json"

  cat <<EOF > "$SYS_MANIFEST"
{
  "name": "io.github.mootxed.kitsupin.native",
  "description": "KitsuPin Native Messaging Host",
  "path": "/usr/lib/kitsupin/kitsupin-native-host",
  "type": "stdio",
  "allowed_origins": [
    "chrome-extension://$EXTENSION_ID/"
  ]
}
EOF
  chmod 644 "$SYS_MANIFEST"

  # Chrome External Extension Auto-Install Manifest
  EXT_SYS_DIR="$STAGING_DIR/usr/share/google-chrome/extensions"
  mkdir -p "$EXT_SYS_DIR"
  EXT_SYS_MANIFEST="$EXT_SYS_DIR/${EXTENSION_ID}.json"

  cat <<EOF > "$EXT_SYS_MANIFEST"
{
  "external_update_url": "https://clients2.google.com/service/update2/crx"
}
EOF
  chmod 644 "$EXT_SYS_MANIFEST"
  echo " Подготовлены системные манифесты Chrome для Extension ID: $EXTENSION_ID"
else
  echo " Development / Alpha packaging mode (KITSUPIN_CHROME_EXTENSION_ID не задан)."
fi

TAURI_OVERRIDE_CONF="$STAGING_DIR/tauri.conf.json"
if [[ -n "$EXTENSION_ID" ]]; then
  cat <<EOF > "$TAURI_OVERRIDE_CONF"
{
  "bundle": {
    "targets": ["deb", "rpm"],
    "resources": {},
    "linux": {
      "deb": {
        "files": {
          "/usr/lib/kitsupin/kitsupin-native-host": "target/release/kitsupin-native-host",
          "/etc/xdg/autostart/kitsupin.desktop": "../packaging/kitsupin-autostart.desktop",
          "/usr/lib/kitsupin/resources/chrome-extension": "../chrome-extension",
          "/usr/lib/kitsupin/resources/scripts/uninstall-user-data.sh": "../scripts/uninstall-user-data.sh",
          "/etc/opt/chrome/native-messaging-hosts/io.github.mootxed.kitsupin.native.json": "../staging/etc/opt/chrome/native-messaging-hosts/io.github.mootxed.kitsupin.native.json",
          "/usr/share/google-chrome/extensions/${EXTENSION_ID}.json": "../staging/usr/share/google-chrome/extensions/${EXTENSION_ID}.json"
        },
        "preInstallScript": "../packaging/preinst"
      },
      "rpm": {
        "files": {
          "/usr/lib/kitsupin/kitsupin-native-host": "target/release/kitsupin-native-host",
          "/etc/xdg/autostart/kitsupin.desktop": "../packaging/kitsupin-autostart.desktop",
          "/usr/lib/kitsupin/resources/chrome-extension": "../chrome-extension",
          "/usr/lib/kitsupin/resources/scripts/uninstall-user-data.sh": "../scripts/uninstall-user-data.sh",
          "/etc/opt/chrome/native-messaging-hosts/io.github.mootxed.kitsupin.native.json": "../staging/etc/opt/chrome/native-messaging-hosts/io.github.mootxed.kitsupin.native.json",
          "/usr/share/google-chrome/extensions/${EXTENSION_ID}.json": "../staging/usr/share/google-chrome/extensions/${EXTENSION_ID}.json"
        }
      }
    }
  }
}
EOF
else
  cat <<EOF > "$TAURI_OVERRIDE_CONF"
{
  "bundle": {
    "targets": ["deb", "rpm"],
    "resources": {},
    "linux": {
      "deb": {
        "files": {
          "/usr/lib/kitsupin/kitsupin-native-host": "target/release/kitsupin-native-host",
          "/etc/xdg/autostart/kitsupin.desktop": "../packaging/kitsupin-autostart.desktop",
          "/usr/lib/kitsupin/resources/chrome-extension": "../chrome-extension",
          "/usr/lib/kitsupin/resources/scripts/uninstall-user-data.sh": "../scripts/uninstall-user-data.sh"
        },
        "preInstallScript": "../packaging/preinst"
      },
      "rpm": {
        "files": {
          "/usr/lib/kitsupin/kitsupin-native-host": "target/release/kitsupin-native-host",
          "/etc/xdg/autostart/kitsupin.desktop": "../packaging/kitsupin-autostart.desktop",
          "/usr/lib/kitsupin/resources/chrome-extension": "../chrome-extension",
          "/usr/lib/kitsupin/resources/scripts/uninstall-user-data.sh": "../scripts/uninstall-user-data.sh"
        }
      }
    }
  }
}
EOF
fi

echo " Packaging staging directory and tauri config override successfully created at $STAGING_DIR"
