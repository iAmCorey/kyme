#!/usr/bin/env bash
# 下载雾凇拼音（rime-ice，GPL-3.0）的数据，这些文件都不进 git：
# - data/rime-ice/：单字和基础词库、emoji 对照表，随安装包分发；
# - data/rime-ice-extra/：扩展词库，只给评测用，不进安装包（用户在设置窗口里自己下载）。
# 版本锁定在 data/rime-ice.commit，kyme-core 的 extra.rs 也读它；改版本时扩展词库的校验值要跟着改，评测会核对。
set -euo pipefail
cd "$(dirname "$0")/.."

COMMIT="$(< data/rime-ice.commit)"

# 把雾凇仓库里的这几个文件下到目录 $1，只留文件名。目录里放的正好是这一套：已经下好就跳过；
# 下到一半中断不会留下半套文件，清单里去掉的文件也不会残留
fetch_dir() {
  local dir="$1"
  shift
  # 标记里记着版本和文件清单，两样都对得上就是已经下好了
  local marker="$COMMIT $*"
  if [[ "$(cat "$dir/COMMIT" 2>/dev/null)" == "$marker" ]]; then
    return
  fi
  # 先全部下到临时目录，再整个换上去
  local tmp="$dir.part"
  rm -rf "$tmp"
  mkdir -p "$tmp"
  for file in "$@"; do
    curl -fsSL --retry 3 --retry-all-errors "https://raw.githubusercontent.com/iDvel/rime-ice/$COMMIT/$file" -o "$tmp/${file##*/}"
  done
  echo "$marker" > "$tmp/COMMIT"
  rm -rf "$dir"
  mv "$tmp" "$dir"
  echo "已下载到 $dir/（rime-ice@${COMMIT:0:7}）"
}

fetch_dir data/rime-ice cn_dicts/8105.dict.yaml cn_dicts/base.dict.yaml opencc/emoji.txt
fetch_dir data/rime-ice-extra cn_dicts/ext.dict.yaml cn_dicts/tencent.dict.yaml
