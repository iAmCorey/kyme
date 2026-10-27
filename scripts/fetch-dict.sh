#!/usr/bin/env bash
# 下载雾凇拼音（rime-ice，GPL-3.0）的词库到 data/rime-ice/。词库文件不进 git。
# 已经下好同一版本就直接退出；下到一半中断也不会留下看起来完整的半个文件。
set -euo pipefail
cd "$(dirname "$0")/.."

COMMIT="da1fbe602e38f26db846fa10120ee64c2b0324c0"
DIR="data/rime-ice"

# COMMIT 标记在所有文件都下完之后才写，它和锁定的版本一致，就说明词库是完整的
if [[ "$(cat "$DIR/COMMIT" 2>/dev/null)" == "$COMMIT" ]]; then
  exit 0
fi

mkdir -p "$DIR"
# 先删旧标记：这次中途失败的话，不会被当成旧版本的完整词库
rm -f "$DIR/COMMIT"
for name in 8105 base; do
  # 先下到 .part，下完再改名，*.dict.yaml 永远是完整文件
  curl -fsSL --retry 3 \
    "https://raw.githubusercontent.com/iDvel/rime-ice/$COMMIT/cn_dicts/$name.dict.yaml" \
    -o "$DIR/$name.dict.yaml.part"
  mv "$DIR/$name.dict.yaml.part" "$DIR/$name.dict.yaml"
done
echo "$COMMIT" > "$DIR/COMMIT"
echo "词库已下载到 $DIR/（rime-ice@${COMMIT:0:7}）"
