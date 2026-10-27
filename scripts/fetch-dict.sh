#!/usr/bin/env bash
# 下载雾凇拼音（rime-ice，GPL-3.0）的词库和 emoji 对照表到 data/rime-ice/。这些文件不进 git。
# 目录里放的正好是 FILES 这一套：已经下好就直接退出；下到一半中断不会留下半套文件，清单里去掉的文件也不会残留。
set -euo pipefail
cd "$(dirname "$0")/.."

COMMIT="da1fbe602e38f26db846fa10120ee64c2b0324c0"
DIR="data/rime-ice"
# 雾凇仓库里的路径，下载后都放在 $DIR 下，只留文件名
FILES=(cn_dicts/8105.dict.yaml cn_dicts/base.dict.yaml opencc/emoji.txt)

# 标记里记着版本和文件清单，两样都对得上就是已经下好了
MARKER="$COMMIT ${FILES[*]}"
if [[ "$(cat "$DIR/COMMIT" 2>/dev/null)" == "$MARKER" ]]; then
  exit 0
fi

# 先全部下到临时目录，再整个换上去
TMP="$DIR.part"
rm -rf "$TMP"
mkdir -p "$TMP"
for file in "${FILES[@]}"; do
  curl -fsSL --retry 3 "https://raw.githubusercontent.com/iDvel/rime-ice/$COMMIT/$file" -o "$TMP/${file##*/}"
done
echo "$MARKER" > "$TMP/COMMIT"
rm -rf "$DIR"
mv "$TMP" "$DIR"
echo "词库已下载到 $DIR/（rime-ice@${COMMIT:0:7}）"
