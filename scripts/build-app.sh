#!/usr/bin/env bash
# 构建并组装 target/Kyme.app（ad-hoc 签名），不安装。dev-install.sh 和 cargo xtask qa 都用它。
set -euo pipefail
cd "$(dirname "$0")/.."

APP="target/Kyme.app"

# 词库已是锁定的版本时它会直接退出
scripts/fetch-dict.sh

cargo build --release -p kyme-macos

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/dict"
cp target/release/kyme-macos "$APP/Contents/MacOS/Kyme"
cp crates/kyme-macos/bundle/Info.plist "$APP/Contents/"
cp -R crates/kyme-macos/bundle/Resources/. "$APP/Contents/Resources/"
# 词库清单只在 fetch-dict.sh 里维护，输入法启动时会读 dict/ 下的全部 .dict.yaml
cp data/rime-ice/*.dict.yaml "$APP/Contents/Resources/dict/"
# 许可证全文随包发布；还没有 LICENSE 时 cargo xtask qa --release 会报出来
if [[ -f LICENSE ]]; then
  cp LICENSE "$APP/Contents/Resources/"
fi
# ad-hoc 签名默认按 cdhash 认身份，每编译一次就变一次，系统会把每次重装都当成新的输入法、
# 弹窗请你重新确认。这里把签名要求固定成只认 bundle ID。只适合本机开发，正式发布要用 Developer ID 签名
codesign --force --sign - --requirements '=designated => identifier "dev.kyme.inputmethod.Kyme"' "$APP"
