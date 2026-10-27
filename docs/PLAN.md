# kyme 计划书

> 开源 · 本地优先 · AI 原生的跨平台中文输入法，先做 macOS。
> 草案 v0.2（2026-10-26，桌面端改为 Rust + GPUI）｜状态：待确认，见第 10 节「待你拍板」

## 1. 定位与原则

**打字像微信输入法一样快、干净、准；需要时，AI 读得懂上下文，能帮你写、帮你改、帮你说。**

五条原则，冲突时按顺序取舍：

1. **打字是底线**。AI 永远不能拖慢基础打字：经典路径单次按键 < 10ms，AI 只做异步增强；AI 进程挂了，照样能打字。
2. **本地优先**。组句、续写、个人数据默认只在本机。云端只服务你主动发起的指令，且自带 Key（BYOK）。
3. **AI 可替换**。本地模型、Apple 系统模型、任意 OpenAI 兼容 API、Claude，都是可插拔的 provider。
4. **无广告、无遥测**。默认零上报。
5. **可审计**。输入法能看到你打的每一个字，开源是信任的前提。

## 2. 现状（2026-10 调研）

**商业产品（体验基线）**

| 产品 | AI 能力 | 对我们的启示 |
|---|---|---|
| 微信输入法 Mac（WeType） | 「问 AI」：输入 `=` 唤起，混元 / DeepSeek；干净无广告 | 你现在用的就是它，它就是体验基线 |
| 豆包输入法（已有 Mac 版） | 大模型语音识别，整句语义理解强 | 语音 + AI 是主战场 |
| 讯飞输入法 | 「AI 键」：帮写 / 润色 / 翻译；离线语音、方言 | 一键 AI 的交互方式 |
| Typeless / Wispr Flow | AI 语音听写：去口癖、自动整理格式 | 「语音 → 能直接发出去的文字」 |

**开源项目（可复用 / 可借鉴）**

| 项目 | 许可 | 是什么 | 借鉴点 |
|---|---|---|---|
| [librime](https://github.com/rime/librime) | BSD-3 | Rime 核心引擎（C++） | 经典引擎底座：方案齐全，用户词学习成熟 |
| [鼠须管 Squirrel](https://github.com/rime/squirrel) | GPL-3.0 | Rime 的 macOS 前端（Swift） | 大量 IMK 兼容性坑的现成处理 |
| [fcitx5-macos](https://github.com/fcitx/fcitx5-macos) | GPL-3.0 | 小企鹅输入法 macOS 版 | 另一套 IMK 前端实现 |
| [雾凇拼音](https://github.com/iDvel/rime-ice) / [万象拼音](https://github.com/amzxyz/rime-wanxiang) | GPL-3.0 / CC-BY-4.0 | Rime 词库；万象另带语言模型 | 词库与语法模型数据 |
| [azooKey-Desktop](https://github.com/azooKey/azooKey-Desktop) | MIT | 日文输入法，用 Zenzai 神经网络做假名汉字转换 | 最接近的「macOS 上的神经网络输入法」范例 |
| [LIME](https://github.com/xushengfeng/lime) | GPL-3.0 | Rime + Qwen3-0.6B，用拼音约束 LLM 逐字采样 | 思路可行；作者自述的问题：长句不稳、打快会漏字母、没有生词记录 |
| [DS Input](https://github.com/madeye/ds-input) | MIT | Rust core + Swift / C++ 薄前端，用云端 LLM 做整句转换 | 分层方式可参考；但整句拼音都发到云端 |

**结论**：经典派（Rime 系）打字扎实但没有 AI；AI 派（LIME、DS Input）证明了方向，但还是原型，要么基本功不稳，要么输入全上云。**「扎实的打字 + 本地 AI + 产品化体验」这个位置目前是空的**，kyme 就做这个。

## 3. 「AI 原生」具体指什么

不是在输入法里塞一个聊天框。输入法的位置很特殊：它存在于每一个输入框，知道你在哪个 App，看得到光标前的文字，而且在你「想表达」的那一刻就在场。按这个思路分五层：

| 层 | 能力 | 用户感受 | 默认运行位置 | 阶段 |
|---|---|---|---|---|
| L0 经典 | 全拼 / 双拼、模糊音、中英混输、词频学习、emoji、繁简 | 跟微信输入法一样顺手 | 本地 | P1 |
| L1 隐形 AI | **上下文感知组句**（参考光标前的文字）；之后加错拼纠正、简拼理解 | 首选更准，AI 给出的首选带 ✨ | 本地小模型 | P2 / P3 |
| L2 预测 | **续写**（Copilot 式，Tab 接受）、智能联想 | 少打很多字 | 本地小模型 | P3 |
| L3 指令 | 润色 / 改写 / 纠错 / 缩写扩写 / 换语气、翻译输入、用拼音写指令、问 AI | 不用再复制到 ChatGPT 改完再粘回来 | 本地中模型或云端 BYOK | P2 |
| L4 语音 | 按住说话 → 本地 ASR → LLM 整理（去掉「嗯 / 那个」、同音纠错、补标点、整理成分段或列表） | 说一段，得到能直接发出去的文字 | 本地 ASR + 本地 / 云端 LLM | P3 |
| L5 代理（探索） | 场景感知（终端里默认英文、邮件里用正式语气）、上屏后纠错提示、智能回复、MCP 工具调用 | 输入法成为全局 AI 入口 | 视情况 | P6 |

**macOS 上的关键交互**

- **✨ 智能首选**：AI 只更新首选这一个位置，不打乱其余候选。空格提交的永远是屏幕上正显示的那一个，不会被异步结果「抢」掉。
- **指令模式**：按快捷键（默认 ⌥Space，可改）→ 用拼音打出指令，比如「帮我改正式一点」→ 回车 → 弹出结果卡片（插入 / 替换 / 复制 / 重试）。作用对象是选中的文字，没选中就用上一段。
- **问 AI**：沿用微信输入法「空输入时按 `=` 唤起」的习惯。
- **语音**：按住右 ⌥（可改）说话，松开上屏；可在「原话 / 整理后」之间切换。
- **硬规则**：AI 生成的内容一律先预览、确认后再上屏；数据要离开本机时，界面上显示 ☁️ 和目标服务名。

## 4. 总体架构

```
Kyme.app ── 输入法 bundle（由系统拉起并常驻：必须轻、稳、快；整个进程都是 Rust）
│
├─ 平台接入层（Rust + objc2，只有这一层是 macOS 专属）
│   ├─ IMKInputController：按键、marked text、上屏
│   └─ 上下文采集：IMK client → AX API → 上屏历史
│
├─ 界面层 kyme-ui（GPUI，三个桌面端共用）
│   ├─ 候选窗：PopUp 窗口（不抢焦点的 NSPanel）
│   └─ 指令面板 / 结果卡片
│
└─ kyme-core（Rust）
    ├─ 会话状态机 · 经典引擎适配（librime）· 拼音 lattice
    ├─ AI 编排：路由 / 去抖 / 取消 / 候选合并
    └─ 用户数据（SQLite）· 配置
          │
          │  Unix domain socket（launchd 按需拉起）
          ▼
kyme-brain ── 独立后台进程（Rust；可崩溃、可重启，不影响打字）
    ├─ 本地 LLM：llama.cpp（Metal）
    ├─ 语音：sherpa-onnx（SenseVoice）
    ├─ 云端：OpenAI 兼容（DeepSeek / Qwen / Kimi / GLM / 豆包 / OpenRouter / Ollama…）· Anthropic
    └─ 模型管理：下载（HF / ModelScope）· 校验 · 空闲卸载

Kyme Settings.app（GPUI）── 方案 · 外观 · 词库导入 · 模型 · API Key（钥匙串）· 隐私规则
```

**为什么这样分**

- **IME 进程只管打字**。它由系统拉起、常驻，一崩用户就打不了字。模型动辄几百 MB 到几 GB，绝不放进这个进程。
- **AI 进程独立**。内存、崩溃、升级都和打字隔离；AI 不可用时自动退回纯经典输入。IPC 一个来回不到 1ms，跟推理耗时比可以忽略。
- **全栈 Rust，桌面三端共用 core 和 UI**。Windows（TSF）、Linux（fcitx5）只需要再写一层薄薄的输入框架接入。TSF 输入法是加载进每个 App 的 DLL，fcitx5 插件跑在 fcitx5 进程里，GPUI 在这两处都跑不起来，所以这两个平台的候选窗放在 kyme 自己的主进程里，DLL / 插件只负责转发按键（小狼毫在 Windows 上就是这个结构）。移动端没有 GPUI，界面要原生写，再用 UniFFI 绑定 core。
- **Apple 系统能力只做可选项**。Foundation Models、SpeechAnalyzer 只有 Swift API，需要时加一层很薄的 Swift shim 接入。国行 Mac 上 Apple Intelligence 何时可用还不确定，不能当默认依赖。

**一次按键的数据流**

```
按键 → IMK → core.process_key()
  ├─ 快路径（同步，< 10ms）：librime 出候选 → 立刻画候选窗
  └─ 慢路径（异步，去抖约 80ms）：core → brain
        输入：光标前文字（≤ 128 token）+ 拼音音节 lattice + 经典候选
        brain：拼音约束 beam search（光标前文字的 KV cache 跨按键复用）
        输出：AI 整句 + 置信度 → core 合并 → 若更优且用户还没动 → 更新首选位 ✨
```

**上下文来源（依次降级）**

1. IMK client 的 `selectedRange()` / `attributedSubstring(from:)`：原生 Cocoa App 支持得好
2. Accessibility API：需要用户授权，能覆盖浏览器和部分 Electron App
3. 自己的上屏历史：同一个 App 里最近提交的文字，任何情况下都可用
4. App bundle ID：判断场景（终端 / IDE / 聊天 / 邮件）

## 5. 技术选型

| # | 决策 | 建议 | 理由 / 备选 |
|---|---|---|---|
| T1 | 经典引擎 | **librime 起步**，通过接口隔离 | 打字基本功是底线；librime 打磨了十多年，BSD-3 许可，方案齐全。局限是它只给一个整句候选、不提供 N-best，所以神经组句需要自建 Rust 拼音 lattice（和它共用词库）。这正好是日后逐步自研引擎的切入点；要不要完全替换 librime，看评测数据再定 |
| T2 | 词库 / 语法模型 | Phase 0 用评测集比较 **万象**（CC-BY-4.0，带语言模型）和 **雾凇**（GPL-3.0） | 用数据选，不凭感觉 |
| T3 | 语言 | **全部 Rust**；桌面端不需要绑定层，做移动端时再用 UniFFI 生成 Swift / Kotlin 绑定 | 一种语言、一套构建（cargo） |
| T4 | 桌面 UI | **GPUI**（Zed 的 GPU UI 框架）：候选窗、AI 面板、设置都用它，三个桌面端共用；macOS 上用 objc2 接 InputMethodKit | GPUI 的 PopUp 窗口在 macOS 上就是不抢焦点、位于弹出层级的 NSPanel，满足候选窗的要求。短板：没有公开的移动窗口 API（改用 raw handle 调 `setFrameOrigin:`）、crates.io 版本停在 0.2.2、做不出 Liquid Glass。Phase 0 spike 不通过就退回「Swift 写 IMK 和候选窗」 |
| T5 | 本地推理 | **llama.cpp**（GGUF，Metal） | 跨平台、量化成熟；Mac 上可拿 MLX 做对比 |
| T6 | 组句模型 | 先用现成的 **Qwen3.5-0.8B** 加拼音约束解码做验证，再**微调专用小模型**（参考 azooKey 的 zenz） | 训练数据可自动合成：开放语料 → pypinyin 转拼音，再加简拼和错拼扰动。已知坑：现成 LLM 处理简拼很差（PinyinGPT 论文的结论），迟早要微调 |
| T7 | 指令 / 改写模型 | 16GB 以上的机器用本地 Qwen3.5-2B / 4B，否则走云端 BYOK | 8GB 机器默认走云端，或者关闭 |
| T8 | 语音 | 默认 sherpa-onnx + SenseVoice-Small（中 / 英 / 粤 / 日 / 韩）；Apple SpeechAnalyzer 和云端 ASR 作为可选 | Phase 3 开始前再做一次 ASR 选型评测 |
| T9 | IPC | Unix domain socket + 长度前缀的 msgpack 消息；launchd 按需拉起 brain | 到 Windows 换成 named pipe，协议不变 |
| T10 | 存储 | 用户数据 / 记忆 / 历史用 SQLite；API Key 存钥匙串 | |
| T11 | 分发 | Developer ID 签名 + 公证、.pkg 安装包、Sparkle 2 自动更新、Homebrew cask | 需要 Apple 开发者账号（$99 / 年）。模型按需下载（HF + ModelScope 镜像，校验 sha256） |
| T12 | 系统要求 | macOS 14+；本地 AI 需要 Apple Silicon（Intel 机器只有经典输入 + 云端） | |

## 6. 性能预算（以 M1 8GB 为最低基准）

| 路径 | 目标 |
|---|---|
| 按键 → 候选窗渲染 | p95 < 16ms（一帧之内） |
| AI 首选更新 | 去抖 80ms，推理 p95 < 150ms |
| 续写 | 停顿 300ms 后出现，p95 < 400ms |
| 指令 / 改写 | 首 token < 1s，流式输出 |
| 语音 | 松开按键后 < 1s 上屏（含整理） |
| 内存 | IME 进程 < 150MB；brain 按模型档位 0.6–3GB，空闲时卸载模型 |
| 电量 | 用电池或开了低电量模式时自动降级（关续写、换小模型） |

## 7. 隐私与安全

- 密码框（secure input）里自动禁用所有 AI 和上下文读取。
- 按 App 配置三档：允许读上下文 / 允许云端 / 完全禁用 AI。密码管理器、银行类 App 默认禁用。
- 无痕模式：一键关掉学习和 AI。
- 只有你主动触发的指令才会访问云端，按键流永远不上云；面板上标明 ☁️ 和服务名。
- 上屏历史只放内存或短期保留，仅供上下文使用，可一键清除。
- 默认零遥测；崩溃报告需要手动开启。

## 8. 路线图

时长按「1 人全职 + AI 辅助」粗估，只用于排期参考。

### Phase 0 · 地基与验证（1–2 周）

- [x] 初始化 monorepo：git、cargo workspace、打包脚本（组装 .app、签名、安装）；GitHub Actions 待加
- [x] **IMK + GPUI spike**：Rust + objc2 写 `IMKInputController`，GPUI 画候选窗，先配一个内置的简易拼音引擎；`scripts/dev-install.sh` 一键安装并重启输入法进程。要验证三件事：
  - IMKServer 和 GPUI 能在同一进程共存，在 IMK 回调里更新 GPUI 状态不卡、不崩
  - 候选窗跟随光标：每次按键重新定位并重绘，p95 < 16ms；多显示器、全屏 App 下位置正确
  - 常驻内存 < 150MB

  任一项不过，就退回「Swift 写 IMK 和候选窗，GPUI 只做设置和 AI 面板」

  **结果（2026-10-26，M1 Pro，在「文本编辑」里实测）**：三项都通过。IMK 和 GPUI 同进程正常工作；候选窗贴在输入行下方、随光标移动，按键处理 p50 4.6ms、p95 9.4ms；物理内存 52MB（峰值 82MB，出现在加载词库时）。还没测多显示器、全屏 App、Electron / 浏览器 / 微信。遗留问题：启动时 GPUI 打出一条 `RefCell already borrowed`，暂未发现影响；候选窗暂不支持鼠标点选。
- [ ] 接入 librime 预编译库和词库，打通「拼音 → 候选 → 上屏」
- [ ] **评测框架** `eval/`：开放语料 → 带上下文的拼音测试集（含简拼、错拼子集）；指标为整句首选准确率、字错误率、top-5 命中率、延迟
  - 已有手写的小评测集 `eval/pinyin.tsv`（90 条：本来就对的 54 条，加上打反、漏字母、连按两次、按错相邻键、跨音节打反、元音开头的简拼六类错误），用 `cargo xtask qa --only eval` 跑，每一条都必须对
- [ ] **神经组句 spike**：Qwen3.5-0.8B + 拼音约束 beam search + 光标前文字，和 librime 基线对比
- [x] **QA 框架** `cargo xtask qa`（2026-10-27）：格式、clippy、单元测试、评测、按键场景、安装包检查；用 `Kyme --qa` 自测模式在 macOS 接入层逐键对照；`--release` 再加人工检查清单和发版要求。每次发版都要跑，见 [QA.md](QA.md)
- [ ] 兼容性冒烟测试：备忘录、Safari、Chrome、VS Code、微信、飞书、终端、JetBrains。已写进人工检查清单 `qa/manual.toml`，还没逐个实测

**出口**：你自己能用它打字；评测报告回答「神经组句能提升多少、延迟多少」，据此定下 Phase 2 的方案。

### Phase 1 · macOS 可日用的 MVP（3–5 周）

- 全拼、双拼（小鹤 / 自然码 / 微软 / 搜狗）、模糊音、Shift 切中英、中英文标点、emoji、繁简转换（OpenCC）
- 用户词学习、自定义短语、横排 / 竖排候选、主题
- 设置界面 v1（GPUI）；**词库导入**（搜狗 / 微信 / Rime 格式），这是别人愿意切过来的前提
- `kyme-cli`：在终端里敲拼音、看候选和 AI 结果，调试时不用安装输入法

**出口**：你关掉微信输入法，连续用 kyme 一周，不想切回去。

### Phase 2 · AI 原生 v1（4–6 周）

- kyme-brain 进程 + llama.cpp + 模型下载与管理
- Rust 拼音 lattice + **✨ 上下文感知组句**（要解决的用例在 `eval/pending.tsv`，比如 `haonanhe` 该出「好难喝」）
- **指令模式、选中改写、翻译输入、问 AI**；provider 支持本地、OpenAI 兼容、Anthropic
- 隐私控制面板：按 App 的规则、无痕模式、☁️ 标识

**出口**：评测集上整句首选准确率明显高于 librime 基线，且 p95 延迟达标。

### Phase 3 · 语音、续写、个性化（4–6 周）

- 按住说话：本地 ASR + LLM 整理，可切换「原话 / 整理后」
- 续写：先做跟随光标的浮层提示，内联灰字按 App 逐个适配
- 个人记忆：常用信息、快捷短语，AI 生成时可以引用
- 专用组句模型 v1 微调（前面的评测数据支持的话）

### Phase 4 · 公测（2–3 周）

- 签名公证、安装包、Sparkle 自动更新、Homebrew cask
- 首次启动引导（解释为什么需要这些权限）、文档站、贡献指南
- 在 GitHub 发布公测版

### Phase 5 · 跨平台

建议顺序：**Windows（TSF）→ Linux（fcitx5 插件）→ Android → iOS**

- 桌面端复用 kyme-core、kyme-ui 和 brain；Windows / Linux 的候选窗放在 kyme 主进程里，TSF DLL / fcitx5 插件只负责转发按键
- 移动端界面要原生写：Android 能跑本地小模型；iOS 键盘扩展的内存上限只有几十 MB，AI 要么交给主 App，要么走云端
- 用户词库和设置的端到端加密同步

### Phase 6 · 探索

- 场景感知、上屏后纠错提示、智能回复（需按 App 显式开启）、MCP 工具调用、语音指令

## 9. 风险

| 风险 | 后果 | 应对 |
|---|---|---|
| 基础打字不如微信输入法 | 用户装上就卸 | librime 打底 + 评测集回归 + 自己每天用 |
| 本地模型在 8GB Mac 上慢、吃内存、耗电 | AI 体验差 | 模型分档、异步去抖、空闲卸载、电池模式降级、可选云端 |
| IMK 兼容性坑（Electron、终端、浏览器、微信） | 有的 App 里候选窗错位、读不到上下文 | 兼容性矩阵 + 参考鼠须管 / fcitx5-macos 的现成处理 + 上屏历史兜底 |
| GPUI 依赖不稳（API 常变、没有正式版本） | 升级成本高，遇到 bug 要自己修 | 锁定版本；界面隔离在 kyme-ui；spike 不过就退回 Swift / AppKit |
| 隐私信任 | 开源输入法的命门 | 本地优先、零遥测、代码可审计、☁️ 标识 |
| 模型分发（国内访问 HF 不稳定） | 下载失败 | ModelScope 镜像、断点续传 |
| 大厂 AI 输入法免费又强 | 拉新难 | 差异化：开源、隐私、BYOK、可定制、无广告；先服务开发者和重度写作者 |
| 许可证混用 | 法律风险 | 尽早定项目许可证；`data/` 下逐项登记来源和许可 |

## 10. 待你拍板

**已定**：

- 桌面端走 Rust + GPUI（2026-10-26），以 Phase 0 spike 的结果为准。
- 许可证用 **GPL-3.0-or-later**（2026-10-27），全文见仓库根目录的 LICENSE。别人对外发布改过的版本必须开源，没法闭源 fork 后塞广告、加追踪。你写的代码以后仍然可以另卖闭源授权，要留住这条路：外部贡献先签 CLA；鼠须管、fcitx5-macos 这类 GPL 项目只参考思路、不拷代码；词库保持可替换（雾凇词库的版权不是我们的）。

1. **经典引擎**：建议 **librime 起步**，神经组句建在它上面，要不要自研引擎看评测结果。备选：一开始就纯 Rust 自研，更「原生」，但追上基本功大概要多花 2–3 个月。
2. **AI 默认策略**：建议 **本地优先 + 云端可选（BYOK）**。备选：云端优先（DS Input 的做法），效果上限高、上手快，但每句话都要上云。
3. **首批用户**：建议先服务**开发者和重度写作者**。他们重视隐私、愿意折腾，用语音写 prompt 又是刚需。先不做大众产品。

## 11. 仓库结构（Phase 0 创建）

```
kyme/
├── crates/                # cargo workspace
│   ├── kyme-core/         # 会话状态机、引擎适配、AI 编排、用户数据
│   ├── kyme-pinyin/       # 拼音解析、双拼映射、lattice
│   ├── kyme-ui/           # GPUI 界面：候选窗、AI 面板、设置（桌面三端共用）
│   ├── kyme-macos/        # macOS 输入法进程：objc2 接 IMK、窗口定位、打包
│   ├── kyme-brain/        # AI 后台进程（llama.cpp、ASR、云端 provider）
│   └── kyme-cli/          # 终端调试 REPL
├── xtask/                 # cargo xtask qa：发版前的检查（见 docs/QA.md）
├── qa/                    # 按键场景、人工检查清单、发版门槛
├── scripts/               # 打包、安装、词库下载
├── data/                  # 词库来源 / 许可登记（不放大文件）
├── eval/                  # 评测集与评测脚本
├── models/                # 训练 / 量化脚本（不放权重）
└── docs/
```

## 附：参考来源

- 微信输入法「问 AI」：https://www.ifanr.com/1590165
- 豆包输入法 Mac 版体验：https://www.zhihu.com/tardis/jm/art/2042013121229968287
- 讯飞输入法：https://sj.qq.com/appdetail/com.iflytek.inputmethod
- Apple Intelligence 国行进展：https://9to5mac.com/2026/03/30/apple-intelligence-rolling-out-now-in-china-per-user-reports/
- Qwen3.5 小模型系列（2026-03）：https://blog.mean.ceo/?p=2699 （参数与许可以官方 model card 为准）
- sherpa-onnx：https://github.com/k2-fsa/sherpa-onnx
- Tan et al., *Exploring and Adapting Chinese GPT to Pinyin Input Method*, ACL 2022（PinyinGPT）
- 开源项目链接见第 2 节表格
