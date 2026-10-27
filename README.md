# Kyme

开源、AI 原生的中文输入法，先做 macOS。Rust + [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui)。

早期版本，AI 功能还没做。路线图见 [docs/PLAN.md](docs/PLAN.md)，更新记录见 [CHANGELOG.md](CHANGELOG.md)。

## 功能

- 全拼整句、简拼（`rgzn` → 人工智能）
- 手误纠错（`zhnog` → 中），打对的拼音不改
- 拼音显示在输入框里，候选窗一行，每页 5–10 个
- 候选带 emoji（`haha` → 哈哈 😄）
- Shift 切换中英
- 中文标点，引号自动配对
- 不联网，日志不记输入内容

## 安装

需要 macOS 14+、Rust（stable）、Xcode 命令行工具。

```bash
scripts/dev-install.sh
```

首次安装在系统弹窗里点「允许」，再在菜单栏的输入法里选「Kyme 拼音」。

卸载：在「系统设置 › 键盘」里移除 Kyme 拼音，删掉 `~/Library/Input Methods/Kyme.app`。

## 按键

| 按键 | 作用 |
|---|---|
| 空格 | 上屏高亮的候选 |
| `1`–`9`、`0` | 选对应的候选 |
| `-` `=`、↑ ↓、Page Up / Down | 翻页 |
| ← → | 移动高亮 |
| 回车 | 上屏字母 |
| Esc | 取消输入 |
| 退格 | 删字母，删光后退回上一次选的字 |
| `'` | 分隔音节 |
| Shift | 切换中 / 英 |

## 设置

`~/Library/Application Support/Kyme/settings.toml`，首次使用时生成，各项见文件里的注释。保存后切换一下输入框生效。

## 开发

```bash
cargo xtask qa                   # 发版前的整套检查，见 docs/QA.md
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
| `docs` | 计划书、QA 说明 |

## 许可

[GPL-3.0-or-later](LICENSE)。词库和 emoji 表来自[雾凇拼音](https://github.com/iDvel/rime-ice)（GPL-3.0）。
