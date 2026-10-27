#!/usr/bin/env bash
# 构建并组装 Kyme.app（ad-hoc 签名），不安装。dev-install.sh 和 cargo xtask qa 都用它。
# 放在 .noindex 目录里：Spotlight 不收录，搜「Kyme」只会找到装好的那份。
set -euo pipefail
cd "$(dirname "$0")/.."

APP="target/app.noindex/Kyme.app"

# 词库已是锁定的版本时它会直接退出
scripts/fetch-dict.sh

cargo build --release -p kyme-macos

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/dict"
cp target/release/kyme-macos "$APP/Contents/MacOS/Kyme"
cp crates/kyme-macos/bundle/Info.plist "$APP/Contents/"
cp -R crates/kyme-macos/bundle/Resources/. "$APP/Contents/Resources/"
# 数据文件的清单只在 fetch-dict.sh 里维护，这里整个目录照搬（COMMIT 记着是哪个版本）
cp data/rime-ice/* "$APP/Contents/Resources/dict/"
# 许可证全文随包发布；还没有 LICENSE 时 cargo xtask qa --release 会报出来
if [[ -f LICENSE ]]; then
  cp LICENSE "$APP/Contents/Resources/"
fi
# ad-hoc 签名默认按 cdhash 认身份，每编译一次就变一次，系统会把每次重装都当成新的输入法、
# 弹窗请你重新确认。这里把签名要求固定成只认 bundle ID。只适合本机开发，正式发布要用 Developer ID 签名
codesign --force --sign - --requirements '=designated => identifier "dev.kyme.inputmethod.Kyme"' "$APP"
