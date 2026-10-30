# Kyme

开源、AI 原生的中文输入法，首先支持 macOS。Rust + [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)。

早期版本。路线图见 [docs/PLAN.md](docs/PLAN.md)，引擎设计见 [docs/ENGINE.md](docs/ENGINE.md)，更新记录见 [CHANGELOG.md](CHANGELOG.md)。

## 功能

- 全拼整句、简拼（`rgzn` → 人工智能）
- 手误纠错（`zhnog` → 中），不改动正确输入的拼音
- 拼音内嵌于输入框；单行候选窗，每页 5–10 个候选
- Emoji 候选（`haha` → 😄）
- 选词记忆：常用候选自动前移，数据仅保存在本机
- 自定义词库；导入外部词库（搜狗细胞词库、Rime、文本）；可在设置中下载雾凇扩展词库和腾讯词库
- AI 预测选词（默认关闭）：停止输入时由模型结合光标前的文字选出首选，以 ✨ 标记；支持 TypeSafe、Anthropic、OpenAI、DeepSeek、通义千问、Kimi、智谱、OpenRouter 及自定义接口
- Shift 切换中英
- 中文标点，引号自动配对
- 除在设置中下载词库外，未开启 AI 预测选词时不联网；日志不记录输入内容

## 安装

需要 macOS 14+、Rust（stable）、Xcode 命令行工具。

```bash
scripts/dev-install.sh
```

首次安装时在系统弹窗中点击「允许」，然后在菜单栏的输入法菜单中选择「Kyme 拼音」。

卸载：在「系统设置 › 键盘」中移除 Kyme 拼音，并删除 `~/Library/Input Methods/Kyme.app`。

## 按键

| 按键 | 作用 |
|---|---|
| 空格 | 上屏高亮的候选 |
| `1`–`9`、`0` | 选择对应的候选 |
| `-` `=`、↑ ↓、Page Up / Down | 翻页 |
| ← → | 移动高亮 |
| 回车 | 上屏输入的字母 |
| Esc | 取消输入 |
| 退格 | 删除字母；字母删完后撤销上一次选择的字 |
| `'` | 分隔音节 |
| Shift | 切换中 / 英 |
| 鼠标点击候选 | 上屏所点的候选；点击 ‹ › 翻页 |

## 设置

在输入法菜单中选择「Kyme 设置…」，或通过 Spotlight、启动台打开 Kyme。

也可直接编辑 `~/Library/Application Support/Kyme/settings.toml`，保存后切换输入框即可生效。自定义词库和导入的外部词库位于同目录的 `dicts/` 中。

## 开发

```bash
cargo xtask qa                   # 发版前的完整检查，见 docs/QA.md
scripts/fetch-dict.sh            # 下载词库
swift scripts/make-icons.swift   # 重新生成图标
cargo run --release -p kyme-pinyin --example repl -- data/rime-ice/8105.dict.yaml data/rime-ice/base.dict.yaml
```

| 目录 | 内容 |
|---|---|
| `crates/kyme-pinyin` | 拼音切分、组句、纠错 |
| `crates/kyme-core` | 输入会话、标点、设置 |
| `crates/kyme-ui` | 候选窗 |
| `crates/kyme-macos` | macOS 输入法 |
| `xtask` | QA |
| `eval`、`qa` | 评测用例、按键场景、门槛、人工检查清单 |
| `scripts` | 下载词库、打包、安装、生成图标 |
| `data` | 数据来源登记 |
| `docs` | 计划书、引擎设计、QA 说明 |

## 许可

[GPL-3.0-or-later](LICENSE)。词库和 emoji 表来自[雾凇拼音](https://github.com/iDvel/rime-ice)（GPL-3.0）。
