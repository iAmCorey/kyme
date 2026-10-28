#!/usr/bin/env bash
# 构建 Kyme.app，装到 ~/Library/Input Methods，并向系统注册、启用。
# 用法：scripts/dev-install.sh [--select]    加 --select 会顺便切换到 Kyme 拼音。
set -euo pipefail
cd "$(dirname "$0")/.."

APP="target/app.noindex/Kyme.app"
DEST="$HOME/Library/Input Methods/Kyme.app"

scripts/build-app.sh

pkill -x Kyme || true
rm -rf "$DEST"
cp -R "$APP" "$DEST"
"$DEST/Contents/MacOS/Kyme" --install "$@"
# 旧进程关掉后，系统不会因为打字就把输入法重新拉起来，要等打开输入法菜单这类动作，
# 在那之前打什么都没反应。装好就先拉起来。已经被系统拉起来了就不再开：再开一次是打开设置窗口
pgrep -x Kyme > /dev/null || open -g "$DEST"
echo "已安装：$DEST"
