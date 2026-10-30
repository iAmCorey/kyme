# 数据来源

词库文件不进 git，用 `scripts/fetch-dict.sh` 下载。

| 目录 | 来源 | 许可 | 用途 |
|---|---|---|---|
| `rime-ice/` | [雾凇拼音](https://github.com/iDvel/rime-ice) 的 `cn_dicts/8105.dict.yaml`、`cn_dicts/base.dict.yaml`、`opencc/emoji.txt`，版本记在 `rime-ice.commit` | GPL-3.0 | 单字与基础词库（带词频）；词语对应的 emoji |
| `rime-ice-extra/` | 雾凇拼音的 `cn_dicts/ext.dict.yaml`、`cn_dicts/tencent.dict.yaml`，与上一行同一版本。腾讯词库是雾凇从[腾讯 AI Lab 中文词向量](https://ai.tencent.com/ailab/nlp/en/embedding.html)的词表整理的 | 雾凇以 GPL-3.0 发布；腾讯 AI Lab 对词向量语料的说明是仅供研究、CC BY 3.0 | 只给评测用，不进安装包。用户在设置窗口里自己从雾凇仓库下载同一版本 |

新增任何数据前，先在这张表里登记来源和许可。
