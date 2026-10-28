# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Kyme：开源、AI 原生的中文输入法，先做 macOS。全栈 Rust，候选窗用 GPUI。代码注释、文档、提交信息都用中文。路线图和已定的决策在 `docs/PLAN.md`，QA 的细节在 `docs/QA.md`。

## 常用命令

```bash
cargo xtask qa                          # 按改动挑检查：只跑和还没推送的改动有关的几项，平时用它
cargo xtask qa --all                    # 全部自动检查：格式、clippy、单元测试、评测、按键场景、安装包、macOS 接入层，约 1 分钟
cargo xtask qa --only eval,scenarios    # 只跑列出的检查
cargo xtask qa --release                # 发版才跑：全部检查，再加发版要求（已提交、LICENSE）和人工检查清单
cargo xtask ai-eval [服务商]            # 实测 AI 预测选词（eval/context.tsv），用输入法的 AI 设置，联网、要 Key，不在 qa 里；--no-context 不给前文
cargo test -p kyme-core zero_selects    # 跑单个测试，按名字过滤
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all                         # rustfmt.toml：max_width 120
scripts/fetch-dict.sh                   # 下载词库到 data/rime-ice/（不进 git），评测和打包都要它
scripts/build-app.sh                    # 组装 target/app.noindex/Kyme.app（Spotlight 不收录），不安装
scripts/dev-install.sh                  # 装到 ~/Library/Input Methods 并启用，会重启正在用的输入法
swift scripts/make-icons.swift          # 改了图标之后重新生成 AppIcon.icns 和 kyme.pdf
cargo run --release -p kyme-pinyin --example repl -- data/rime-ice/8105.dict.yaml data/rime-ice/base.dict.yaml
```

`cargo xtask` 是 `.cargo/config.toml` 里的别名，用 release 编译。门槛在 `qa/gates.toml`，报告写在 `target/qa/`。

## 架构

一次按键的路径：IMK → `kyme-macos/controller.rs` → `state::handle_event` → `kyme_core::Session::handle_key` → `Response`（接没接、上屏的文字、`View`）。之后 `client.rs` 调 `insertText` / `setMarkedText`，`panel.rs` 用 `kyme-ui` 画候选窗，贴到光标旁边。光标位置每个键都重新问 App。

鼠标点候选窗的路径：`kyme-ui` 等 GPUI 处理完这次鼠标事件再回调 → `state::pick` → `Session::pick`（选字键和翻页键也走它），结果交给最近一次按键所在的输入框。

设置窗口是单独的进程 `Kyme --settings`（`settings_window.rs`），界面是 `kyme-ui` 的 `SettingsView`：左边导航栏，分输入、外观、自定义词库、外部词库、AI 五页。输入法菜单的「Kyme 设置…」（IMK 的 `menu` 和 `showPreferences:`）、输入法在跑时再打开一次 Kyme.app（`on_reopen`）、打开不在「输入法」文件夹里的 Kyme.app，都会拉起它；已经开着就只把它叫到前面（靠 `$TMPDIR/kyme-settings.sock`）。改动用 `SettingsFile::save` 写回，只改值、注释照旧；AI 的 Key、模型和地址写进旁边的 ai.toml（`AiFile`）。「自定义词库」是单独一页（`settings/custom.rs`），在后台读安装包里的主词库，显示每个词会用的读音。

- **`kyme-pinyin`**：引擎。负责音节切分（含简拼和没打完的末尾音节），按一元词频在词图上做 Viterbi 组句，读音一样的几句再看相邻两个词的搭配（`pairs.rs`）换首选，再做手误纠错。另外带一张 emoji 对照表。`Engine::load_dir` 读整个数据目录（`*.dict.yaml` 和 `emoji.txt`），输入法和 xtask 都用它。`Engine::with_local` 在主词库旁边加一层本地词库（`local.rs`：认几种行的写法、没写拼音的按主词库补），共用音节表和代价尺度。
- **`kyme-core`**：和平台无关的输入会话，包括候选、翻页、中英切换、标点、emoji 插入，还有设置文件。
  - 设置文件在 `~/Library/Application Support/Kyme/settings.toml`，每次切到一个输入框时检查有没有改过。`SETTINGS_TEMPLATE` 必须等于默认值，有测试守着。
  - 选词记忆：`kyme-pinyin` 的 `UserDict` 是纯数据，`Engine::convert_with` 带上它组句，只进照着打的读法、不进纠错的读法；`kyme-core` 的 `UserDictFile` 读写同目录的 `userdict.tsv`。会话要挂上用户词库（`set_user_words`）才学词，按键场景和 `Kyme --qa` 都不挂。
  - 本地词库：同目录 `dicts/` 里的 `*.dict.yaml`，`custom.dict.yaml` 是自定义词库，导入的外部词库由 `LocalDicts::import`（`import.rs` 认 .scel 和文本）转成同样的写法放在 `dicts/imported/`。和用户词一样只进照着打的读法。输入法切换输入框时发现有改动，就在后台重建，`Session::set_engine` 换上；评测、按键场景和 `Kyme --qa` 都不读。
  - `SELECT_KEYS` 是选字键、候选窗序号和每页上限的唯一来源。
- **`kyme-ui`**：GPUI 候选窗和设置窗口。尺寸都在 `Metrics` 里按字号等比缩放；平台层先 `measure()`，再摆放窗口。输入框 `input.rs` 照 GPUI 自带的示例改，支持输入法组字，填 Key 时只显示圆点。
- **`kyme-macos`**：同一个可执行文件兼任输入法进程、安装工具（`--install`）、自测模式（`--qa`）和设置窗口（`--settings`）。IMK 回调和 GPUI 都跑在主线程，全局状态放在 thread_local 里。IMK 回调以外（计时器、AI 的回答、鼠标）别占着全局状态向 App 跨进程要东西：等回话时 IMK 会把接着按的键送进来，状态被占着的话这个键只能原样交给 App。
- **AI 预测选词的路径**：输入法里每次组字后 `state::schedule_ai` 等 100ms，输入没变就用 `Session::ai_query` 拿前几名整句（不问时它返回原因，记进日志），在后台问设置里选的那家。光标前的文字用 `client::text_before_cursor` 读，一段输入只读一次；读不到、没有字也问。回来时信得过（`kyme_ai::Pick::trusted`：把握度够，没有前文时要更有把握）、输入还没变，`Session::offer_ai` 把它挑的那句放到第一个（✨），原首选排第二；挑的就是首选也标 ✨。✨ 出现后 0.3 秒内的按键先撤掉 ✨ 再处理。切到输入框、开始打字时，好一阵没连过（`Client::cold`）就由 `warm_ai` 先在后台连上服务商，问的时候不用再握手。会话本身不联网。
- **`kyme-ai`**：AI 预测选词，只有它联网。把光标前的文字和引擎的前几名整句（`Engine::sentences`）交给设置里选的服务商（TypeSafe 的 Jev、Anthropic，和 OpenAI 兼容的几家，预设的地址和模型在 `kyme_ai::preset`；也可以自定义接口），让它挑接得最顺的一句；模型只能从候选里挑，读音不会错。各家的 Key 和模型（自定义的还有地址）存在设置文件旁边的 ai.toml，只有自己能读写，不用钥匙串。
- **`xtask`**：QA。`Kyme --qa` 用一个假的 IMK 输入框，跑真正的会话、接入层和候选窗，每个键都和本地的参照 `Session` 对照。

## 纠错的硬规则

照着打对的拼音永远不能被改。为什么这样设计见 `docs/ENGINE.md`。

- **什么时候用纠错结果**：纠错后的整句要比「照着打」的读法（只用完整音节）好出 `TYPO_MARGIN`，才会被采用。
- **改动落在没打完的末尾音节时**：首选按照着打的读法，纠错结果排第二个候选。
- **显示和理解分开**：输入框里显示的永远是打的字母（`Conversion.preedit`），引擎理解成什么在 `spelling` 里。下面说的「被改」都是指 `spelling` 和打的字母不一样。
- **评测怎么守**：一是把 `eval/pinyin.tsv` 的用例逐个前缀打一遍，`preedit` 都得是打的字母，「本来就对」的那些 `spelling` 也不能变；二是把常用音节两两拼接，检查被改的比例不超过门槛。
- **还做不到的用例**：放进 `eval/pending.tsv`，只报告，不卡发版。

## 测试和调试时要注意

- **别在真实 App 里用合成按键测试**：合成的字母键到输入法时会变成控制字符。用 `Kyme --qa`，也就是 `cargo xtask qa` 的 macos 那一项；各个 App 里的表现走人工检查清单 `qa/manual.toml`。
- **日志不记输入内容**：`~/Library/Logs/Kyme/kyme.log` 只记事件和耗时，绝不记打的内容。
- **别动输入源**：不要切换或「恢复」当前输入源。锁屏时系统报告的输入源是 ABC，不是真正在用的那个。
- **输入法进程没在跑时打字没反应**：进程被关掉后，系统不会因为打字就重新拉起它，要等打开输入法菜单这类动作。`dev-install.sh` 装完会自己拉起来；日志里没有新的「Kyme 启动」就是没起来。
- **授权弹窗**：macOS 26 上调用 `TISEnableInputSource` 会弹授权窗。`--install` 会跳过已经启用的输入源，`build-app.sh` 的签名要求只认 bundle ID，所以重装不会再弹。
- **菜单栏图标**：改了图标并重装后，要 `killall TextInputMenuAgent` 才看得到新图标，系统会自动把它拉起来。
- **候选窗不会出现在截图里**：要查它是否显示、在什么位置，用 `CGWindowListCopyWindowInfo`。
- **看设置窗口**：它是普通窗口，`screencapture -l <窗口号>` 截得到。`HOME=<临时目录> TMPDIR=<临时目录> Kyme --settings` 用临时的设置文件，也不会只去叫你开着的那个设置窗口。

## 数据和发版

- **第三方数据**：先在 `data/README.md` 登记来源和许可。`fetch-dict.sh` 锁定了雾凇拼音的提交，`data/rime-ice/` 整个目录照搬进安装包，安装包检查比对整个目录。
- **发版**：按 `docs/QA.md` 的流程，改 `Cargo.toml` 的 `workspace.package.version` 和 Info.plist 的版本号，把 CHANGELOG 里的「未发布」改成版本号和日期，然后提交、打 tag。README 和 CHANGELOG 都要简洁。
