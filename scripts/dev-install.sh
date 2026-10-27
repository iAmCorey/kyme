#!/usr/bin/env bash
# 构建 Kyme.app，装到 ~/Library/Input Methods，并向系统注册、启用。
# 用法：scripts/dev-install.sh [--select]    加 --select 会顺便切换到 Kyme 拼音。
set -euo pipefail
cd "$(dirname "$0")/.."

APP="target/app.noindex/Kyme.app"
DEST="$HOME/Library/Input Methods/Kyme.app"

scripts/build-app.sh

# 系统会在需要时重新拉起输入法进程
pkill -x Kyme || true
rm -rf "$DEST"
cp -R "$APP" "$DEST"
"$DEST/Contents/MacOS/Kyme" --install "$@"
echo "已安装：$DEST"
