# Herdr Agents 栏模型与 TPS：AI 维护指南

本文用于定位问题、修改扩展和选择验证。安装与日常使用见
[用户指南](./README.md)。实现事实以
[package.json](../../package.json)、当前目录
及本机 CLI 生成的协议为准；升级后先核对实现，不沿用旧版本结论。

## 快速入口

本扩展给 Herdr Agents 栏发布 `model` 和 `tps`，并为 OMP 补充显示名。
`model` 来自当前用户会话的模型设置及可观测 reroute。Codex 的 `tps` 估计可见回答和推理输出；
OMP 的 `tps` 对齐内部 working row，包含工具参数 delta 和 output usage 校正，但不包含工具执行结果或输入。

| 要处理的问题 | 先读本节 | 实现 owner（路径相对当前目录） |
| --- | --- | --- |
| 安装、升级后命令绕过扩展 | [运行与安装](#运行与安装) | `install.mjs`、两个 `*-with-tps.mjs` |
| Codex 模型错误、换会话后 TPS 为 0 | [Codex 协议](#codex-协议) | `lib/codex-tps-observer.mjs`、`codex-with-tps.mjs` |
| OMP 模型、显示名、输出计数 | [OMP 事件](#omp-事件) | `omp-extension.mjs`、`lib/omp-profile.mjs` |
| 速度跳变、结束归零、tokenizer | [TPS 与发布](#tps-与发布) | `lib/live-tps-reporter.mjs`、`lib/token-rate-meter.mjs`、`lib/model-token-counter.mjs` |
| metadata 超时、积压、残留 | [TPS 与发布](#tps-与发布) | `lib/herdr-metadata-publisher.mjs` |
| token 用量统计、计价、采集水位 | [Token 用量统计](#token-用量统计) | 仓库外 `~/.local/share/herdr/usage/` |
| 现场证据与修改收口 | [诊断顺序](#诊断顺序)、[修改与验证](#修改与验证) | `test/*.test.mjs` |

两条输入链复用同一套采样与发布实现：

```text
Codex TUI --remote ↔ WebSocket proxy ↔ Codex App Server
                          ↓
                    CodexTpsObserver ──┐
                                      ├→ LiveTpsReporter → HerdrMetadataPublisher → Herdr socket
OMP + --extension → OmpTpsObserver ─────┘        ↓
                                         TokenRateMeter (OMP 18.2.5 多尺度指数衰减)
                                               ↓
                                        model-token-counter
```

不要把 Codex 或 OMP 的协议逻辑写进通用采样器，也不要把一个适配器的结论套用到另一个。

## 运行与安装

### 启动边界

- 发布要求同时存在 `HERDR_ENV=1`、`HERDR_PANE_ID` 和 `HERDR_SOCKET_PATH`。
- Codex 在 Herdr 外直接运行原始 CLI；在 Herdr 内启动回环地址的 App Server 和代理，再启动
  `--remote <proxy-url>` TUI。两端口动态分配，`/readyz` 默认最多等待 10 秒。
- 代理原样转发 TUI 的文本、二进制帧和子协议；只解析文本 JSON。它会额外发送只读
  `thread/read` 来确认未知会话身份，内部请求的响应不转发给 TUI，详见 [Codex 协议](#codex-协议)。
- OMP wrapper 注入 `--extension` 并转发参数；缺少 `--profile` 时从 `--resume` 的 profile
  会话路径恢复。原始二进制所在目录置于子进程 PATH 首位，保护更新目标。
- `pro2` 启动前由 `share-omp-daemons.mjs` 将其 daemon 目录接到默认 profile；只迁移没有
  活动 client/broker/daemon 的旧目录，并完整保留备份。同项目复用后台服务，auth/settings/sessions
  仍由原 profile 持有。此检查不是热切换；旧客户端须先正常退出。详见用户指南的使用说明。
- wrapper 保留退出码及信号语义并清理子进程；reporter 退出时清除 metadata，异常退出由 TTL 清尾。

### 安装器与命令路径

在仓库根目录执行：

```bash
npm run herdr:tps:install -- --dry-run
npm run herdr:tps:install
```

安装器负责：

1. 校验两个启动脚本仍是 Node wrapper，拒绝损坏目标、冲突链接和残缺 shell 标记。
2. 安装命令链接；把可执行的原始 OMP 固化到受忽略的 `integrations/herdr-tps/.runtime/omp`。
3. 更新受管 shell rc 片段：**仅在 `HERDR_ENV=1` 时**把 Codex shim 目录放到 PATH 首位。
   旧版本的全局受管 PATH 块（`stable Codex dispatcher PATH` 标记）会被迁移移除，标记
   残缺时拒绝改写；受管标记之外残留的 `herdr-tps/bin` 导出仅在输出中警告。
4. 更新受管侧栏 rows；已有 shell rc 和配置发生修改前分别备份，配置校验失败时恢复配置备份。
5. 执行 `herdr config check`、Herdr 交互 shell 的 `command -v codex` 探测及 server config reload。
已包含 `state_icon`、`$model` 与 `$tps` 的自定义 rows 会原样保留；缺少所需 token 的
未知自定义 rows 不会被覆盖，需在原布局中手工补齐。

默认路径：

```text
~/.local/share/herdr-tps/bin/codex → integrations/herdr-tps/codex-with-tps.mjs
~/.local/bin/codex-tps            → integrations/herdr-tps/codex-with-tps.mjs
~/.local/bin/codex                → 原始 Codex（由官方安装器维护）
~/.local/bin/.herdr-codex-original → 原始 Codex

~/.local/bin/omp                  → integrations/herdr-tps/omp-with-tps.mjs
~/.local/bin/omp-tps              → integrations/herdr-tps/omp-with-tps.mjs
~/.local/bin/.herdr-omp-original  → integrations/herdr-tps/.runtime/omp
```

Codex shim 与官方标准 alias 分开，避免更新器覆盖扩展入口。OMP wrapper 优先使用
`HERDR_TPS_OMP_BIN`，其次仓库 `.runtime/omp`，最后 `.herdr-omp-original`；子进程中的
`omp update` 应解析到原始 runtime，而非仓库 wrapper。

### 侧栏与生效条件

受管布局：

```toml
[ui.sidebar.agents]
rows = [
  ["workspace", "tab"],
  ["state_icon", "agent", "$tps"],
  ["$model"],
]
```

`state_icon` 来自 Herdr 的 Agent 状态；本扩展不发布生命周期状态。其外观由 Herdr 的
`ui.status_indicators` 和主题控制，安装器只维护上述 rows，不修改该外观设置。

| 改动 | 如何生效 |
| --- | --- |
| 受管侧栏配置 | 侧栏布局属于客户端本地配置：客户端全局菜单 `reload config` 或重启客户端；`server reload-config` 不刷新已连接客户端的侧栏 |
| shell rc / PATH | 重开 pane，使新 shell 读取 rc |
| Herdr server 自身 PATH | config reload 无法更新进程环境；原生 Agent 恢复是否经过 shim，必须检查 server 实际 PATH 与恢复后的进程链 |

## Codex 协议

### 会话归属

`rememberThread` 保存分类和模型标量，`activateThread` 是活动用户 root 的唯一判定入口。
所有发现路径都必须保留 `parentThreadId`、`source` 和 `threadSource`。

以下会话不能接管 pane：

- `parentThreadId != null` 的子代理；
- `source.subAgent` 标识的子代理；
- `threadSource === "system"` 的内部系统会话。

**没有父会话不等于用户会话。** 系统会话可能同时是 `parentThreadId: null`、
`source: "vscode"`、`ephemeral: true`，仍必须排除。不要按 `ephemeral` 过滤，临时用户会话也合法。
“先显示正确模型、随后被 Luna 覆盖”曾由遗漏 system 分类触发；模型与 TPS 可能同时受影响。

| 入口 | 当前行为 |
| --- | --- |
| client `thread/start`、`thread/resume`、`thread/fork` 成功响应 | 记录会话与模型；无活动 turn 时按原请求顺序尝试激活 |
| `thread/started` 通知 | 仅记录身份，不因通知较新就切换 root |
| client `turn/start` | 保存生效模型，身份已知时尝试激活 |
| server `turn/started` | 激活已确认的用户 root；身份未知时先执行只读查询 |
| `thread/closed`、`thread/deleted` | 清理该会话缓存；若为当前 root，清除模型、turn 和采样状态 |

不能永久绑定首次启动会话，也不能让最近出现的任意会话自动接管。
事件序号用于防止迟到 response、异步查询或旧事件覆盖较新的活动 root；重放保留原序号。

未知会话的处理链：

1. `turn/started` 触发 `thread/read { threadId, includeTurns: false }`，默认超时 2 秒。
2. 等待期间仅缓存受跟踪通知；单会话上限为 1024 条消息、256000 个 delta 字符。
3. 查询成功且 ID 匹配后，保存身份并按原序重放；系统会话和子代理仍走统一过滤。
4. 失败、超限或连接关闭时丢弃待重放事件；不猜身份。后续新的 turn/started 可以重试。

代理用 `herdr-tps:thread-read:<uuid>:<sequence>` 隔离内部请求 ID，吞掉对应响应及迟到响应。
observer 不保留 resume 的整段历史；未知会话的待定模型只保存标量，最多 1024 条。

### 模型与输出事件

模型解析唯一入口是 `effectiveModel(settings)`：

```text
collaborationMode.settings.model ?? model
```

早到 settings / turn 模型必须保留到身份确认；基础快照和迟到响应不得覆盖较新的模型设置。
必须监听 `thread/settings/updated`，不能仅靠 reroute 或下次 turn 才刷新标签。

| 通知 | reporter 行为 / 限制 |
| --- | --- |
| `thread/settings/updated` | 记录生效模型；属于活动 root 时更新标签 |
| `model/rerouted` | 仅接受当前 turn 且未落后于模型设置的 `toModel`；不改写下一轮配置模型 |
| `turn/started` | `start(turn.id, model, emittedAtMs)`，建立 generation |
| `item/agentMessage/delta` | append `agent:<itemId>` |
| `item/reasoning/summaryTextDelta` | append `reasoning-summary:<itemId>:<summaryIndex>` |
| `item/reasoning/textDelta` | append `reasoning:<itemId>:<contentIndex>` |
| `turn/completed` | 忽略明确属于旧 turn 的完成通知；当前 turn pause 并清除 turn ID |
| `thread/tokenUsage/updated` | 忽略；不是结束边界，也不能校准可见输出 TPS |

仅当前 root 的输出进入 reporter；有当前 turn 时，明确属于其它 turn 的 delta 被丢弃。

### 协议升级核对

WebSocket transport 为 experimental。维护命令使用原始 CLI，避免进入交互 wrapper：

```bash
RAW_CODEX_BIN="${HERDR_TPS_CODEX_BIN:-${HOME}/.local/bin/.herdr-codex-original}"
CODEX_PROTOCOL_DIR="$(mktemp -d)"
"$RAW_CODEX_BIN" --version
"$RAW_CODEX_BIN" app-server generate-ts --experimental --out "$CODEX_PROTOCOL_DIR"
```

按改动读取生成类型：`ClientRequest.ts`、`ServerNotificationEnvelope.ts`、`v2/Thread.ts`、
`v2/ThreadStartResponse.ts`、`v2/ThreadResumeResponse.ts`、`v2/ThreadReadResponse.ts`、
`v2/ThreadSettings.ts` 及相关 turn/delta 通知。优先核对会话分类、模型优先级、事件字段与时间戳。
不要把某次 CLI 版本号或 synthetic fixture 当成长期协议合同。

## OMP 事件

OMP 的生命周期状态（working/idle/blocked）由 Herdr 官方集成
`~/.omp/agent/extensions/herdr-omp-agent-state.ts` 发布，本扩展不接管。升级 Herdr 后先
`herdr integration status` 核对版本，必要时 `herdr integration install omp` 同步；0.9.0
随附 v9，`agent_end.willContinue === true` 时官方集成不把已排定续轮的结束当 settle，
`agent wait` 不会在续轮边界提前完成。pro2 profile 的扩展目录链接到默认 profile 同一文件，
一次安装两处生效；运行中的 OMP 不热加载。

生产扩展使用 `requireUi: true`，直到 session context 的 `hasUI === true` 才创建 reporter。

| 事件 | 行为 |
| --- | --- |
| `session_start`、`session_switch` | 确认 UI root，更新 context 模型和显示名 |
| `message_start` | 仅 assistant message 建立 generation |
| `message_update` | 消费 `text_delta` / `thinking_delta` / `toolcall_delta`，不补计快照 |
| `message_end` | 以 timestamp + duration 结束采样，用 usage.output 校正后立即发布 |
| `agent_end` | pause |
| `session_shutdown` | 清除显示刷新 timer、observer 状态并等待 reporter.close |

模型优先级是 `message.model ?? context.model.id ?? context.model.name`。
生产 reporter 为 `OmpTpsReporter`，通过 OMP 自身模块导入 `Tokenizer`，按 context.model.tokenizer 选择分词器。
按与上游相同的消息边界计算，不把旧的 streaming 值保留到 usage 校正之后，不预先取整或放大短响应。
生产路径只处理 delta，不保留全文；message_end 之后忽略迟到 update。observer 的旧 snapshot 路径只保留给兼容 reporter。

显示名优先级：`HERDR_TPS_OMP_DISPLAY_AGENT` → `--profile` → session file 中的
`.omp/profiles/<profile>/agent/sessions/`。`pro2` 显示为 `omp2`，其余为 `omp`。
profile 参数与路径解析统一由 `lib/omp-profile.mjs` 提供，wrapper 的 resume 恢复也复用该 owner。
显示名发布携带 `agent: "omp"`、`applies_to_source: "herdr:omp"` guard，并在 500ms 后补刷一次。

## TPS 与发布

### 采样、平滑和结束

Codex wrapper 启用 `streamingOnly`：以首个可见 delta 作为片段起点，证据门槛为
1 token / 500ms（OMP 默认仍为 200 tokens / 4000ms）。无 delta 达 1500ms 后，
以最后一个 delta 的时间暂停采样并保留速度；同一 turn 的后续输出重新开启片段。
因此工具执行和等待不会持续稀释速率，短推理/回答也不会长期被默认门槛压成零。
运行中的 wrapper 不会热加载修改，须在任务结束后退出并重新运行 Codex 才生效。

| 默认参数 | 值 | owner |
| --- | ---: | --- |
| 衰减半衰期 (TokenRateMeter) | 5s / 20s / 80s | `token-rate-meter.mjs` |
| 采样分块与词边界保留 | 250ms / 32 字符 | `token-rate-meter.mjs` |
| 轮次间残差衰减因子 | 0.8 | `token-rate-meter.mjs` |
| Codex 最短观测 / 更新间隔 | 500ms / 250ms | `live-tps-reporter.mjs` |
| OMP 证据门槛 / 更新间隔 | 200 tokens、4000ms / 250ms | `omp-tps-reporter.mjs` |
| metadata 心跳 / 持久 TTL | 2000ms / 5000ms | `live-tps-reporter.mjs` |
| 非零 TPS TTL | 2500ms | `max(stale, heartbeat) + 2 × sampleInterval` |

生产环境（Codex 与 OMP wrapper）采用 OMP 18.2.5 移植的 `TokenRateMeter` 算法：

1. **多尺度指数衰减**：在 5s、20s、80s 三个半衰期桶内同时维护 tokens 与 time；使用解析积分 `(halfLife / ln2) * (1 - 2^(-t / halfLife))` 计算流式增长与时间权值。
2. **词边界分块分词**：流式 delta 按 250ms 周期分块，并在最后空格/换行符处分割（保留末尾最多 32 字符至下一块），消除跨 delta 分词边界误差与突发抖动。
3. **实际 Output Usage 校准**：在 `message_end` / `turn/completed` 拿到服务商权威 usage output tokens 时，以 0.8 残差衰减因子校正流式累积误差。
4. **轮次间保持可见（Rate Retention）**：轮次结束后保持已显示的速度数字，不会在 1 秒后突兀归零；轮次间维持平滑展示，会话重置、模型切换或长久空闲后归零。
5. **历史会话预热（Seeding）**：会话启动/切换时，若历史中已有含耗时的 assistant 消息，自动以其 usage output 与 duration 预热速率计，实现平滑无缝显示。
### Tokenizer 边界

OMP 使用宿主 `@oh-my-pi/pi-agent-core` 的 `Tokenizer`，由 context.model.tokenizer 决定编码，
不得用下述 Codex 模型名规则替代。

Codex 模型名先去掉供应商路径前缀，再由 `model-token-counter.mjs` 的正则选择：

- 已匹配的 `gpt-4o`、`gpt-4.1`、`gpt-5*`、`gpt-oss`、`o1/o3/o4`、`codex*`：`o200k_base`。
- 旧 `gpt-3.5` / `gpt-4` 命名：`cl100k_base`。
- 未匹配名称：`ceil(UTF-8 字节数 / 4)`；当前 `gpt-6-astra` 也走此 fallback。

编码器按需加载。新增模型系列需修改映射并补测试，不能因显示名正确就宣称已使用精确 tokenizer。

### Metadata 合同

`HerdrMetadataPublisher` 通过 socket 发送单行 JSON RPC：

```json
{
  "id": "example-request",
  "method": "pane.report_metadata",
  "params": {
    "pane_id": "w3:p1",
    "source": "herdr:tps",
    "tokens": { "tps": "42" },
    "seq": 1788696000000001,
    "ttl_ms": 2500
  }
}
```

- 唯一生产 source 是内部固定的 `herdr:tps`；`herdr:tps:codex`、`herdr:tps:omp` 只在启动时清理。
- `model` / `tps` 在 `tokens` 中发布字符串，删除用 `null`；`display_agent` 是独立字段。
- seq 按 source 从 `Date.now() × 1000` 起递增。队列串行发送，同字段集合、TTL、guard 的待发送
  快照只保留最新一份；过期项丢弃，发送前扣除已等待的 TTL。
- 心跳合并 model、零 TPS 和 OMP 显示名；非零 TPS 单独使用短 TTL。模型/速度单独发布不带
  display guard，包含显示名的 snapshot 才带 guard。
- `clearAll` 丢弃尚未发送的生产快照，再排队清除字段，避免退出后重放旧值。
- 默认 socket 超时 2000ms；响应须为单行 JSON、ID 匹配、有 result 且无 API error。
- 发布失败不阻断 Agent；连续相同 source/错误内容在 30 秒内抑制重复报告。Codex 默认仅在
  `HERDR_TPS_DEBUG=1` 时输出诊断；OMP 默认写 `pi.logger.debug`，开启 debug 后另写 stderr。

## 诊断顺序

### 1. 确认目标 pane 和真实进程

先由 workspace/tab 定位 pane，不把当前执行诊断的 pane 当成故障目标：

```bash
herdr --version
herdr workspace list
herdr tab list
herdr pane list
```

确认 ID 后再设置变量并读取：

```bash
TARGET_PANE_ID="<上一步确认的 pane_id>"
herdr pane get "$TARGET_PANE_ID"
herdr pane process-info --pane "$TARGET_PANE_ID"
herdr pane read "$TARGET_PANE_ID" --source visible --lines 10
```

Codex 应同时存在 Node wrapper、`app-server --listen ws://127.0.0.1:...` 和
`--remote ws://127.0.0.1:...` TUI。比对 TUI 模型、`tokens.model`、`agent_session`，再判断根因。

### 2. 按症状定位

| 症状 | 优先核对 |
| --- | --- |
| 模型起初正确，随后变为 Luna | 是否误接纳 `threadSource: "system"`；进程是否已加载修复 |
| 模型错误且生成期间 TPS 为 0 | 活动 root ID、分类元数据和异步 thread/read；不能只看 publisher 心跳 |
| TUI 换模型后标签不变 | `thread/settings/updated`、collaboration 模型优先级、当前 turn 的 reroute |
| TPS 为 0，但模型正确 | 是否确有可见回答/推理 delta；idle、工具执行、未公开推理时为 0 合理 |
| 更新 CLI 后模型/TPS 消失 | shell 的命令解析、原始链接、wrapper 完整性及三个 Codex 进程 |
| OMP 恢复后丢失 omp2 名称 | `--profile` / `--resume`、session file 路径、UI root 门槛、display guard |
| 状态灯消失 | rows 是否含 `state_icon`；不从 TPS 发布逻辑寻找状态灯 |
| 标签残留或速度明显滞后 | 是否有旧进程/旧 source、队列积压、TTL 或 seq 拒绝 |

### 3. 核对安装与发布链

```bash
HERDR_ENV=1 bash -ic 'command -v codex'
readlink -f "${HOME}/.local/share/herdr-tps/bin/codex"
readlink -f "${HOME}/.local/bin/.herdr-codex-original"
readlink -f "${HOME}/.local/bin/.herdr-omp-original"
rg -n -A 6 '^\[ui\.sidebar\.agents\]' "${HOME}/.config/herdr/config.toml"
rg -n 'pane.report_metadata|herdr:tps' "${HOME}/.config/herdr/herdr-server.log" | tail -n 50
```

Herdr shell 应命中独立 Codex shim；普通 shell 不要求命中。zsh 环境用实际 zsh rc 核对。
配置路径有覆盖时以实际环境为准。需要启动诊断日志时使用 `HERDR_TPS_DEBUG=1 codex`。

必须验证 socket/侧栏是否能显示时，只向已确认的目标 pane 发布短期诊断值：

```bash
herdr pane report-metadata "$TARGET_PANE_ID" \
  --source herdr:tps:diagnostic --token tps=42 --ttl-ms 5000
```

**不要手工使用生产 source 或提高其 seq。** 这会使运行中的 publisher 后续写入被拒绝。

### 4. 采集协议证据

用实际 App Server 的 `thread/read` 比对用户与额外会话的 ID、model、parentThreadId、source、
threadSource、ephemeral 和 status；不要仅凭模型名推断会话用途。
需要订阅输出时在内存中汇总事件名、次数和分类标量，断开后一次输出摘要，避免打印正文或历史。
尤其不要把 `item/commandExecution/outputDelta` 逐帧写回正在执行的命令，否则会形成诊断回声。

对已加载会话、不传配置覆盖的 resume 曾能触发显示恢复，但不能据此认定修复完成；
它会重新加入会话并触发同步，不应作为生产修复或定时保活手段。代码是否生效仍由进程启动时间
与重启后的真实场景证明。

## 修改与验证

1. 从快速入口找到 owner，核对输入协议、上下游、缓存/清理和已有测试，再修改。
2. 行为改动补对应回归；只改文档时核对事实、链接及 diff，无需重复已通过的运行时测试。
3. 运行与改动相关的标准入口：

```bash
npm run herdr:tps:test
npm run herdr:tps:install -- --dry-run
git diff --check
```

| 改动面 | 必须保留的回归 |
| --- | --- |
| Codex 会话/模型 | 不同 ID 的占位与活动 root；system、subagent 隔离；response/notification/read；早到模型、迟到响应、关闭清理；用户 delta 仍能产生非零 TPS |
| OMP | 无界面子代理不发布；profile 恢复；原生 tokenizer、工具参数 delta、上游速率 trace、小数和 usage 校正；shutdown |
| 采样/显示 | 多 stream、token 边界、停顿、新 generation、平滑、短响应 hold、新输出取消归零 |
| Publisher | 唯一 source、guard、队列合并/过期、TTL 覆盖心跳、错误收敛、退出清理 |
| Installer/wrapper | 幂等、自定义配置保护、CLI 更新目标、Herdr-only PATH、参数/信号转发和失败清理 |

4. 行为修改后重启对应 Agent 做 live smoke：比对 TUI 与侧栏，观察有持续可见输出时的非零 TPS、
   完成归零、模型/会话切换；假 App Server 的进程测试不能替代实际 CLI 场景。
5. 同步维护指南与用户指南；报告改动范围、验证结果、是否加载新代码及尚未执行的 live 场景。
   某一适配器通过测试不等于另一个已做现场验证。

## Token 用量统计

统计 Herdr 内四个 agent 客户端（`omp`、`codex`、`grok`、`opencode`）的 token 消耗，
写入 sqlite，由 ai-monitor 只读展示。与上文 `model` / `tps` 是两件事：`tps` 是可见输出速率，
本节是**账单口径的用量**，含输入、缓存、输出与思考四分量。

### 落点与载体

代码由本仓库 `crates/herdr-usage/` 持有；数据库
`~/.local/share/herdr/usage.db`（WAL），不进 git。常驻进程是用户级 systemd unit
`~/.config/systemd/user/herdr-usage-collector.service`，`Restart=on-failure` + `RestartSec=10s`。

```text
四源日志 ──► herdr-usage（60s 一轮，只读、不联网）──► usage.db ──► ai-monitor（只读展示）
                                                          ▲
                          import_prices（手动/AI 触发）────┘  唯一写价格表者
```

```sh
systemctl --user status  herdr-usage-collector
journalctl --user -u herdr-usage-collector -f     # 健康判据：每 60s 一轮且行数单调增长
cargo build --release --offline -p herdr-usage
cargo run --release --offline -p herdr-usage --bin import_prices
```

### 采集口径（唯一 owner：`src/event.rs`）

四源的原始字段口径不一致，归一化后才能合并：

| canonical | omp | codex | grok | opencode |
| --- | --- | --- | --- | --- |
| `input_total`（毛值，含缓存） | `input + cacheRead` | `input_tokens` | `prompt_tokens` | `input + cache.read` |
| `cache_read` | `cacheRead` | `cached_input_tokens` | `cached_prompt_tokens` | `cache.read` |
| `cache_write` | `cacheWrite` | `cache_write_input_tokens` | 0（无此字段） | `cache.write` |
| `output_total`（含思考） | `output` | `output_tokens` | `completion_tokens` | `output + reasoning` |
| `reasoning` | `reasoningTokens` | `reasoning_output_tokens` | `reasoning_tokens` | `reasoning` |

两条硬规则，改动归一化时必须同时满足：

- `0 <= cache_read <= input_total`；`0 <= reasoning <= output_total`。违反即解析 bug，入库前钳制并打日志。
- **`reasoning` 是 `output_total` 的组成部分，不是独立分量**：禁止 `output_total + reasoning`。
  opencode 的 `output + reasoning` 是把它原本独立的思考量**并入**输出总量，不是并列求和。

### 去重键与水位

| client | `event_id` | 水位 |
| --- | --- | --- |
| omp | 信封 `o.id` | 每文件字节 offset |
| codex | `文件绝对路径 + 行起始偏移` | 每文件字节 offset |
| grok | `sid + loop_index + ts`（缺 `ts` 会丢 8%） | 单文件字节 offset |
| opencode | `message.rowid` | `rowid > max_rowid − 2000` 重放窗口 |

要点：

- **首次启动只记录当前位置，不回填历史**。因此 `usage_event` 只有 collector 首次运行之后的数据。
- **半行保护**：尾部不完整行不解析，其字节与起始偏移随水位一起持久化；进程在行中间重启后，
  下轮仍能还原完整记录（`FileOffsets.residues`）。这是踩过的坑，勿删。
- **codex 的 model 回溯**：`token_count` 不带 model，取同文件最近一次 `turn_context.model`；
  baseline 跳过的前缀会在 1MB 窗口内补扫一次，避免重启后 codex 事件永久无 model。
- **opencode 先建后填**：行创建时 tokens 全零、之后才回填（实测 p99 lag 905h，尾部 2000 行内约
  3.6% 的行在创建 60s 后仍被回填）。故全零行**不入库**，靠 2000 行重放窗口 + `INSERT OR IGNORE` 补齐。
- **grok 轮转**：文件被重写（`size < offset`）时从 0 重读，并清空 `sid → model` 映射；
  codex 同理清空该文件的 `turn_context`。
- grok 是单模型 CLI，用量事件无 model：先查 `sid` 追踪的 `model changed`，否则回退
  `~/.grok/config.toml` 的 `[models].default`（按 mtime 重读）。注意 **`[ui].fork_secondary_model`
  不是回退值**。回退值不是实测值时，`usage_event.model_source` 记为 `config_fallback`。

### 价格表与计价

价格表由 `import_prices` 独立写入（collector 不联网、不写价格表），源为
`https://openrouter.ai/api/v1/models`。三条易错点：

- **所有价格是 JSON 字符串**（`"0.00001"`），必须 `parse::<f64>()`，否则静默按 0 处理。
- 更新用 `ON CONFLICT DO UPDATE`，且 **`remark` 不在更新列表里** —— 人工写的偏差说明不会被覆盖。
  `remark` 为 NULL = 价格直接来自 OpenRouter 标准字段；非 NULL = 有偏差。
- `pricing.overrides`（分时段定价，实测 65 个模型）**v1 不处理**，一律取基础价，金额会有偏差。

计价公式（唯一 owner：`src/cost.rs`）：

```text
cost = (input_total − cache_read) × prompt
     + cache_read                  × coalesce(cache_read,  prompt)
     + cache_write                 × coalesce(cache_write, prompt)
     + output_total                × completion
```

`reasoning` 不单独计费（它已含在 `output_total` 里）。

**模型解析链**：`model_alias`（含 `ignore`）→ 原名精确匹配 `model_price` → 剥 vendor 前缀按
**末段**匹配 → 失败。末段匹配是必需的：OpenRouter id 是 `vendor/model`，而
`opencode-go/deepseek-v4-flash` 只有靠末段才能对上 `deepseek/deepseek-v4-flash`。

- 命中 → 算 cost。
- `ignore = 1` → `cost_usd = NULL`，**不进** `unresolved_model`，**不计入**覆盖率分母。
  通用规则：`providerID = opencode` 且 `modelID` 以 `-free` 结尾（实测 12 个模型 / 10.2% 事件）。
  **不按 0 计价** —— 0 会进分子并稀释真实付费模型的均价。
- 无价格 → `cost_usd = NULL`，记 `unresolved_model`，**计入**分母，**不阻断采集**。

`cost_coverage = priced_events / (all_events − ignored_events)`。注意 `ignored ⊆ unpriced`，
所以分子里**不能再减一次 `ignored`**。

未计价模型的提醒只扫**最近 7 天**（`last_seen >= now − 7d`）：实测全库 160821 行中窗口内仅 1 个模型、
窗口外 53 个，不设窗口会让废弃模型长期占据提醒首位。三处出口：ai-monitor token 页覆盖率标注、
`unresolved_model` 表（按 `hit_count` 降序）、`import_prices` 收尾打印。

### codex 双事件类型（有意只采一种）

codex 有两种 token 事件：`event_msg` / `token_count`（高频增量流）与顶层
`token_usage_record`（带 `response_id`）。两者字段集完全相同，且实测同时出现的文件中
`SUM(last_token_usage)` **逐字段相等**（36 vs 36 条记录，`input_tokens=876564` 一致），
即后者是前者的子集/汇总。

**决策：只采 `token_count`。** 两个都采会重复计入，且 `token_usage_record` 的 payload
不含 `turn_context`，采了会因 model 缺失变成 `cost = NULL` 并拉低覆盖率。
这里明确记录取舍，便于后续有人发现「有个 `token_usage_record` 没采」时能查到这是有意为之。

### 修改与验证

```sh
cd ~/.local/share/herdr/usage
cargo test --offline                    # 单元 + 端到端（临时 HOME 夹具，不碰真实日志）
cargo build --release --offline
HERDR_USAGE_DB=/tmp/probe.db ./target/release/herdr-usage    # 试跑，不污染生产库
```

回归清单（每条都对应已踩过的坑，改动后必须仍通过）：

- codex `info: null`（首个 `token_count` 即空）跳过而非 panic
- codex `last_token_usage` 求和，绝不使用 `total_token_usage`
- codex 只采 `token_count`，`token_usage_record` 不得计入
- codex 跨文件 timestamp 冲突 → 键必须是 `文件 + offset`
- codex baseline 后 model 仍能从被跳过前缀恢复
- grok 同 `sid + loop_index` 多事件（键必须含 `ts`，否则丢 8%）
- grok model 两级解析 + `config.toml` mtime 变化后重读
- grok / omp / codex 轮转（`size < offset` → 从 0 重读）
- omp 无 `message.id` → 必须用信封 `o.id`
- opencode 全零行不入库；回填行在重放窗口内被补入且不重复计数
- 半行保护：进程在行中间重启后仍能还原完整记录
- 口径断言 `cache_read <= input_total`、`reasoning <= output_total`
- 价格字符串 → f64（防 `"0.00001"` 被当成 0）
- `-free` 通用规则：不进 `unresolved`、不进分母
- 解析失败 → `cost = NULL` 且记 `unresolved_model`，不猜测
- 7 天窗口：`last_seen` 超期的 `unresolved_model` 不出现在提醒清单

## 环境变量

| 变量 | 使用方与含义 |
| --- | --- |
| `HERDR_ENV`、`HERDR_PANE_ID`、`HERDR_SOCKET_PATH` | publisher 启用条件与目标，通常由 Herdr 注入 |
| `HERDR_TPS_DEBUG` | `1` 开启 Codex/OMP stderr 诊断 |
| `HERDR_TPS_TIMEOUT_MS` | metadata socket 超时，默认 2000ms |
| `HERDR_TPS_CODEX_BIN`、`HERDR_TPS_OMP_BIN` | wrapper 的原始可执行文件覆盖 |
| `HERDR_TPS_OMP_DISPLAY_AGENT` | OMP 显示名覆盖 |
| `HERDR_TPS_HOME` | installer home 根目录，默认当前用户 home |
| `HERDR_TPS_CONFIG` | installer 配置路径，默认 `<home>/.config/herdr/config.toml` |
| `HERDR_TPS_BIN_DIR` | installer 别名目录，默认 `<home>/.local/bin` |
| `HERDR_TPS_COMMAND_DIR` | installer Codex shim 目录，默认 `<home>/.local/share/herdr-tps/bin` |
| `HERDR_TPS_SHELL_RC` | installer shell rc，默认按 SHELL 选 `.zshrc` 或 `.bashrc` |
| `HERDR_TPS_RUNTIME_DIR` | installer 的 OMP runtime 目录，默认仓库 `integrations/herdr-tps/.runtime` |
| `HERDR_USAGE_DB` | herdr-usage collector 的 sqlite 路径，默认 `~/.local/share/herdr/usage.db` |

installer 覆盖变量用于隔离安装/测试，不会自动改变 wrapper 的默认路径。自定义 OMP runtime
目录时，需同时核对 wrapper 的二进制解析，必要时设置 `HERDR_TPS_OMP_BIN`。
