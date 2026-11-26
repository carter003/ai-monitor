# ChatGPT Web GPT 上下文窗口实测记录

日期：2026-11-24  
范围：本机 `chatgpt-codex-web` v6.2.0 安装包（Pro 账号，`experimentalBiggerContext=true`，launcher 临时会话）。本文区分官方模型规格、Codex/Web 产品默认值和本机 bridge 实测值；三者不是同一个数。§2.5 为本次新增的 GPT-6.2 Sol medium 实测。

## 1. 测试方法

### 1.1 测试入口

直接使用**安装包源码**里的 `ChatGptBrowserWorker`，不经过 Codex review 流程、不调用 broker、不跑本地工具：

- 源码：`integrations/codex-chatgpt-web-local/.state/sources/6.2.0-930c48063ff5-qZgs5V/`
- runtime：`/home/carter003/.codex-chatgpt-web/versions/6.2.0-linux-x64/runtime/bun`
- 浏览器宿主：`~/.codex-chatgpt-web/runtime/launcher-browser.json`

调用参数：

- `modelId: "gpt-5.6-sol"`（worker 只接受 backend model ID）
- `modelFamily: "6.2"`
- `reasoning: "low"`

该组合在 bridge 里对应面板的 **GPT-6.2 Sol Instant / low**。安装包路由表中 `chatgpt-web/gpt-6.2-sol` 只支持 medium/high/xhigh；low 会映射到 `gpt-6.2-sol-instant`。

### 1.2 输入构造

- 用 bridge 自带 `src/lib/token-estimate.ts`（o200k_base）精确计量 token。
- 惰性 filler：`synthetic-context-fixture alpha beta gamma ... 0123456789`，明确声明是数据不是指令。
- 在文本开头、中间、结尾放唯一 marker；最终问题只要求按序输出 marker，避免测试内容受推理质量影响。
- 单条消息测试：逐步提高 filler token，观察 bridge preflight 接受/拒绝以及 marker 召回。
- 多轮测试：使用 64 位 hex `conversationKey` + `retainConversation: true`，后续轮提供 `prepareResume`；每轮发送一段 filler 并要求只回复 `OK<n>`，最后统一要求召回全部 marker。

### 1.3 约束

- 未停止、重启或覆盖正在运行的服务。
- 未修改仓库源码或配置；临时脚本和测试日志在测试后删除。
- 每个尺寸只做有限次数采样；filler 是合成数据，不等价于真实代码审查质量。

## 2. 已知上下文窗口清单

### 2.1 官方模型规格

| 模型 | context window | 最大输入 | 最大输出 | 来源 |
|---|---:|---:|---:|---|
| GPT-6.2 Sol | 1,050,000 | 922,000 | 128,000 | [OpenAI API](https://platform.openai.com/docs/models/gpt-6.2-sol) |
| GPT-6 Sol | 1,050,000 | 922,000 | 128,000 | [OpenAI API](https://platform.openai.com/docs/models/gpt-6-sol) |
| GPT-5.6 Sol | 1,050,000 | 922,000 | 128,000 | [OpenAI API](https://platform.openai.com/docs/models/gpt-5.6-sol) |
| GPT-6.1 Sol | 1,050,000 | 922,000 | 128,000 | [OpenAI API](https://platform.openai.com/docs/models/gpt-6.1-sol) |

官方 API 页面对 GPT-6.1 Sol 还明确写了：在 Codex 和 ChatGPT Work 中默认窗口是 **500K**，说明同一模型的 API 与产品界面窗口并不相同。

### 2.2 官方 ChatGPT / Codex 产品默认值

| 项目 | 数值 | 来源 |
|---|---:|---|
| GPT-6.2 Sol 在 Codex 的默认上下文 | 500,000 tokens | [Models / ChatGPT Learn](https://learn.chatgpt.com/docs/models) |
| GPT-6.2 Sol 可通过 `model_context_window` 提到 | up to 1,000,000 tokens | [Models / ChatGPT Learn](https://learn.chatgpt.com/docs/models) |
| GPT-6.1 Sol 在 Codex / ChatGPT Work 的默认上下文 | 500,000 tokens | [GPT-6.1 Sol API 页](https://platform.openai.com/docs/models/gpt-6.1-sol)、[ChatGPT Work and Codex](https://help.openai.com/en/articles/20001275-chatgpt-work-and-codex) |

### 2.3 本机 bridge 声明值（安装包源码，非本次重新实测）

本机配置：Pro 账号 + Bigger Context 开启。

| 模型 / effort | bridge 原始窗口 | 压缩阈值 | Codex 报告窗口 |
|---|---:|---:|---:|
| GPT-6.2 Sol Instant / low | 111,193 | 95,000 | 94,514 |
| GPT-6.2 Sol / medium、high、xhigh | 240,000 | 220,000 | 220,800 |
| GPT-5.6 Sol Instant / low | 333,579 | 285,000 | 283,542 |
| GPT-5.6 Sol / medium、high、xhigh | 333,579 | 285,000 | 283,542 |
| GPT-5.6 Pro / max | 336,579 | 285,000 | 286,092 |
| GPT-6 Pro / max | 336,579 | 285,000 | 286,092 |

源码依据：`integrations/codex-chatgpt-web-local/.state/sources/6.2.0-930c48063ff5-qZgs5V/src/chatgpt-web-models.ts`。

### 2.4 本次实测：GPT-6.2 Sol low / Instant

#### 单条消息

| 实测 token | 结果 |
|---:|---|
| 100,002 | 接受，marker 全部召回 |
| 102,004 | 接受，marker 全部召回 |
| 102,997 | 接受，marker 全部召回 |
| 103,002 | 拒绝：`exceeds the measured 103,000-token ChatGPT browser message boundary` |

结论：**单条消息上限约 103,000 token**。最后一次成功请求的 bridge 计量：`estimatedInputTokens=111,189`，窗口 `111,193`。

#### 保留会话的多轮上下文

| 轮次 × 每轮 token | 累计 token | 召回结果 |
|---|---:|---|
| 3 × 90k | ~270k | 全部 marker 召回（重复两次一致） |
| 4 × 50k | ~200k | 全部 8 个召回 |
| 4 × 60k | ~240k | 全部 8 个召回 |
| 4 × 62.5k | ~250k | 全部 8 个召回 |
| 4 × 63.75k | ~255k | 只剩 T3/T4 |
| 4 × 65k | ~260k | 只剩 T4 |
| 4 × 67k | ~268k | 只剩 T4 |
| 4 × 70k | ~280k | 只剩 T4 |
| 4 × 80k | ~320k | 只剩 T4 |
| 6 × 90k | ~360k | 第 4 轮浏览器读回失败；召回完全丢失 |

结论：

- 多轮保留上下文的**实用上限约 250,000 token**。
- 255k 起明显丢失早期轮次；260k 基本只剩最后一轮。
- 3 × 90k（270k）两次全召回属于分布相关例外，可能与 ChatGPT 自身摘要有关；规划时不要依赖它。
- 这是 bridge + ChatGPT Web 保留会话的实测值，不等于 API 的 1.05M，也不等于 bridge 单轮窗口 111,193 或 Codex 报告窗口 94,514。

### 2.5 本次实测：GPT-6.2 Sol / medium

入口与 §1 相同，参数改为 `modelId: "gpt-5.6-sol"`、`modelFamily: "6.2"`、`reasoning: "medium"`。六段事务由 `compileChatGptWebPrompt` 的 `experimentalMultipartParts: 6` 生成，事务 ID 使用 worker 下发的 `ctx_<uuid>`。回答中 Markdown 会把 marker 的下划线转义为 `CTXMED\_A7F3`，判定前统一还原。未停止、重启或覆盖运行中的服务；未修改仓库源码或配置。

#### 单条消息（inline）

| 实测 token | 结果 |
|---:|---|
| 100,000 | 接受，3 个 marker 按序召回 |
| 102,300 | 接受，3 个 marker 按序召回 |
| 102,400 | 接受，3 个 marker 按序召回 |
| 102,800 | 拒绝：`103,340 visible message tokens` 超过 `103,000-token` 边界 |
| 103,000 | 拒绝：`103,540 visible message tokens` 超过同一边界 |

结论：**medium 与 low 共用约 103,000 可见消息 token 的单条消息边界**。拒绝信息里的 `111,193` 是 inline 路径未带 bigger-context family 时报告的标准窗口，不是 medium 的 bridge 窗口；bridge 声明值仍是 240,000，压缩阈值 220,000。

#### Bigger Context 六段事务（单轮）

离线编译校准：200k filler 得 `estimatedInputTokens=221,703`，通过；215k filler 得 `251,766`，被 `240,000-token six-part ceiling` 拒绝。因此事务入口上限是 **240,000 估算输入 token**，最大 filler 约 205k。单条记录不可跨段拆分，所以 filler 由多条 20k–30k token 的消息组成。

| filler 可见 token | 样本 | marker 召回 |
|---:|---:|---|
| 100,000 | 2 | 两次均 3 个按序 |
| 120,000 | 2 | 两次均 3 个按序 |
| 125,000 | 1 | 首个丢失 |
| 130,000 | 2 | 一次 3 个按序，一次首个丢失 |
| 140,000 | 1 | 首个丢失 |
| 150,000 | 2 | 一次首个字符错乱，一次丢失 |
| 200,000 / 205,000 | 各 1 | 首个丢失 |

六段全部实际提交并收到 acknowledgement，`compactionTrimmedMessages=0`；丢失发生在 ChatGPT Web 侧，不是 bridge 裁剪。结论：**事务预算可用到约 240k 估算输入，但可靠召回只到约 120k 可见 token（约 131k 估算输入）**；125k 起首个 marker 经常丢失。

#### 保留会话的多轮上下文

同一 64 位 hex `conversationKey` + `retainConversation: true`，每轮一条 60k–100k token 消息，最后统一召回。下表区分首次样本与随后重复样本：

| 累计 token | 首次样本 | 重复样本 |
|---:|---|---|
| 120k（2 × 60k） | 2 个全召回 | — |
| 180k（3 × 60k） | 全召回 | 首 marker 丢失 / 只剩后两轮 |
| 240k（4 × 60k） | 全召回 | 只剩 T3/T4 |
| 280k（4 × 70k） | 全召回 | — |
| 350k（5 × 70k） | 全召回 | 只剩 T5 |
| 360k（4 × 90k） | 全召回 | — |
| 400k（4 × 100k） | 全召回 | 只剩 T4 |
| 500k（5 × 100k） | 全召回 | — |
| 600k（6 × 100k） | 全召回 | 只剩 T6 |
| 800k（8 × 100k） | 全召回 | 只剩 T8 / 回答 `None.` |
| 1.0M（10 × 100k） | 全召回 | 只剩 T10 |
| 1.1M（11 × 100k） | 全召回 | 只剩 T11 |
| 1.4M（14 × 100k） | — | 只剩 T14 |
| 1.5M（15 × 100k） | 第 15 轮浏览器读回失败：`accepted the message but did not expose its assistant turn in the DOM` | — |

结论：

- 单次样本曾到 **1.1M 累计 token 全召回**，但重复样本从 180k 起就频繁只剩最后一轮；这不是稳定上限，不能据此规划。
- 按重复样本保守取值，**可靠多轮保留约 120k 累计 token**；180k 以上应视为不稳定，1.2M 以上基本只剩最后一轮。
- 波动来自 ChatGPT Web 临时会话自身的摘要/运行状态，不是 bridge 的 240k 窗口或 220k 压缩阈值。与 §2.4 的 low（实用约 250k）不能直接等同：两轮测试期间模型行为有明显差异，marker-only 召回也不等价于真实代码审查质量。

### 2.6 与历史慢 review Pane 的对应

- `w4:p4`：GPT-6.2 Sol xhigh，Codex 报告窗口 220,800；21 次压缩累计约 96.7 分钟。
- `w4:pV`：GPT-5.6 Pro max，Codex 报告窗口 286,092；峰值 156,181，未触发压缩，10.8 分钟完成。
- 两个差异中，220.8k vs 286.1k 是本机 bridge 预算差异；模型原生 API 窗口两者同为 1.05M。
