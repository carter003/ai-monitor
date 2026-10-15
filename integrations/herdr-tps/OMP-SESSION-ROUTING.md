# OMP 原生认证与 opencode-go session 粘性

OMP 18.11.0 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
扩展只补齐 opencode-go API Key 的 session 粘性，不固定 Antigravity credential、不执行跨
provider fallback，也不覆盖原生 `health.model`。

`omp-extension.mjs` 加载一个局部模块：

- `lib/omp-api-key-observer.mjs`：固定 opencode-go session 的首次实际 Key，并记录 request 路由。

运行时保持原始可执行文件不变；禁止修改、重打包或禁用 OMP Bun 字节码。wrapper 只启动
`.runtime/omp`，不维护或选择 patched/original 双份二进制。

## opencode-go 粘性与账户观察

路由器让新 session 的第一次 `keys.getWithCredential` 使用 OMP 原生 usage 排名，然后在内存中
复用该结果。`/reset` 的 `appendResetBoundary` 会释放当前 pin；`/new` 使用新的 session ID，二者
下一次请求都会重新排名。恢复请求 ID 与日志 session ID 一致的 session 时，从 custom entry 的 credential ID 和
AuthStorage 当前 credential 行恢复同一 Key，不触发排名。block、usage-limit、rotation、显式
release 和认证存储替换会清除 pin，保留 OMP 的失败恢复能力。

credential ID 作为 `herdr-api-key-sticky-v1` custom entry 写入所属 session；同一 credential 的连续
请求不重复写，发生选择变化或 release 才追加记录。记录不包含 API key。
父、子 session 共用 AuthStorage，但各自保留自己的 SessionManager 写入器；记录按 `sessionId`
选择写入器，不能复用最后激活的扩展 API。`session_branch` 会重新绑定新 session 的写入器，
只恢复该 ID 自己的记录，不复制父 session 的 pin。恢复同一子 session 会替换写入器，旧实例退出不会移除新实例。

OMP 18.11.0 的 `/reset` 与 `/fresh` 会生成不同于日志 session ID 的内部 provider routing ID。
公开 ExtensionContext 和请求事件没有提供这一映射，因此这些请求仍可在进程内保持粘性，
但对应 pin 无法可靠写入所属日志，也不能承诺跨进程恢复或 collector 的精确账户归因。
不从请求 payload 的 `prompt_cache_key` 猜测映射；修复需要上游公开有效 routing ID 或认证 hook。

运行期间修改 `auth.broker.url` 或 `auth.broker.token` 导致存储替换时，路由器通过
`credentials.onGeneration` 检测新的认证对象，立即重新安装观察钩子，并记录
`store-replaced` release、清理旧存储的选择缓存。新存储即使复用相同 credential ID，也会
重新记录 pin；旧存储尚未完成的异步调用不会覆盖新存储的观察状态。

OMP 18.11.0 仍然只写 API key 的 session affinity，API-key 选择路径（`CredentialSelector.selectApiKey`）
不读这份 affinity；只有 OAuth 路径（`resolveOAuth`）读。因此本地路由补丁依旧必要，继续位于公开的
`keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，不修改 Bun runtime。
普通 request 不再因额度样本刷新或排序分数变化而换号。

18.8.1 引入的 session restriction（`sessions.restrict`）优先于本地 pin。每次请求先检查
`sessions.isRestricted`；受限 session 清除旧 pin 并委托原生选择，包括空允许池，不能返回缓存
或持久记录中的 Key。`keys.source(provider, { env: 'aliases' })` 表示 runtime/config/env/OAuth
来源时同样委托原生；新增 login Key 优先于旧的非 login pin，保留原生凭据优先级。
读取 pin 前调用原生 `credentials.poll`，采用其它进程的
认证存储变更；generation 通知清理已解析密钥，删除或禁用的 credential 不再被复用。
持久恢复发现 `!command` 或已设置的环境变量引用时委托原生 resolver，不把表达式当密钥。
进程内缓存的是首次原生解析结果；动态引用值变化及自定义 resolver 的刷新不在静态 Key 粘性的保证范围内。

18.8.3 让 request 路径在 OAuth 刷新可重试失败且无其他来源可用时抛 `OAuthRefreshUnavailableError`
（v18.8.1、v18.8.2 均无此符号），可用性探测（`keys.get`）改为吞掉该错误返回 `undefined`。
opencode-go 用 login/stored API key，不经过 OAuth 分支；即便抛出，观察器的 `inflight` 也会在
`finally` 清理、不会写入缓存或 release pin，错误原样交给 OMP 重试。

18.8.5 起 `SessionsApi.inherit` 接受可选 `include` 过滤器（按 provider 与是否显式用户 pin 挑选复制
哪些 affinity），并把 `AuthApiKeyOptions.accountIds` 的匹配键从 `accountId` 泛化为 `oauthAccountKey`
（`accountId ?? email ?? projectId`），使 Antigravity 多账号按能否服务请求的模型排序；`inReserve`
改用 `isWithinUsageReserve` 判定，修正恰好等于 reservePct 的账号仍被选中。这些都只影响 OAuth
账号选择，opencode-go 走 `selectApiKey` 的 API-key 路径，本地 sticky pin 行为不变。
18.8.5 另修正 `oauth.accessById` 带 `forceRefresh` 时返回未刷新 token 的问题（18.8.6 增加
`refreshReason: "auth-recovery"` 转发），同样不经过本扩展。

18.8.7 修好 `/session pin` 在全部账号额度耗尽时被绕过的问题：`select.ts` 的 last-resort blocked
pass 把显式 session pin 排到最早解封的 sibling 之前，并把 OAuth preflight 的候选行改为按
credential ID 捕获（peer 重排行不再错位）。本扩展的粘性 pin 在 `keys.getWithCredential` 之上自行
维护，不写入 OAuth 的显式 pin；`selectApiKey` 仍不读 affinity，opencode-go 换号语义不变。
错误分类上，中文额度耗尽模式从 `使用…上限` 泛化为 `(使用|用量)…上限`（命中 MiniMax Token Plan 的
429），只让这类 429 轮换 sibling；本扩展的 `markReached`/`rotate` 记录语义不变。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级
路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.11.0 的：


- `keys.getWithCredential`、`keys.source`、`sessions.release`、`sessions.isRestricted`、`sessions.permits`、`limits.markReached`、`limits.rotate`；
- `credentials.list`、`credentials.poll`、`sessionManager.getSessionId`、`sessionManager.appendResetBoundary`；
- `credentials.onGeneration`（存储替换时保留订阅并在新对象就位后通知）；
- 扩展 API 的 `appendEntry`；
- TPS 使用 `sessionManager.getBranch` 和公开 SDK 的 `pi.pi.buildSessionContext`；
- 扩展上下文的 `ctx.agent.kind`（18.3.2 新增），用于拒绝子代理 session 驱动 pane 元数据。

Antigravity 账号选择完全由 OMP 原生实现，本扩展不再改写 system prompt；opencode-go session
粘性由本扩展维护。

**`limits.rotate` 的返回类型在 18.4.0 破坏性变更**：由 `Promise<boolean>` 改为
`Promise<CredentialRotation>`（`{ switched, afterSiblingWait? }`）。对象恒为真，只有 `switched`
为真才表示真的换了账号。观察器因此只在 `rotated.switched` 时记录 `rotation` release。
没有可用 sibling 时 rotate 最多等待 5s（`SIBLING_UNBLOCK_WAIT_MAX_MS`）等短暂 block 的
sibling 恢复，随后返回 `switched: false`——此时账号没变，按旧的真值判断会被误记成 rotation。
`packages/ai/src/auth/rotation.ts` 在 18.8.1 至 18.11.0 逐字节相同；18.8.1 起新增 `sessionId` 参数并用
`affinity.allows()` 过滤 sibling，但 `CredentialRotation` 的 `{ switched }` 契约没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

### 2026-10-11 行为复核与优化

接口签名或 bundle 符号未变不足以证明行为正确。本次补齐 restriction 与原生凭据优先级、
跨进程认证刷新、branch 写入器、关闭清理及迟到异步结果的回归。普通 session 退出时释放其本地
选择与已解析 Key；没有扩展写入器的后台 session 最多保留 128 个，使用 LRU，
淘汰只清理内存，不追加 release，也不淘汰仍有写入器的活跃 session。

TPS 在首次 UI 附着、switch/branch/tree 时先重置 observer 和 reporter，再设置模型并预热。
历史只读取 `getBranch()` 的活动路径，优先交给原生公开 `pi.pi.buildSessionContext` 重建消息，
复用 compaction、reset 和 retry 过滤。缺少 helper 时仅按活动路径及 `reset_boundary` 截断；
缺少路径或读取失败时不预热，避免采入其它分支或清空前的消息。
子代理和无 UI 事件不能覆盖已激活 root pane 的状态。

原生恢复会话时在 `session_switch` 之后才应用目标模型，且内部 `model_changed` 没有扩展事件。
切换到不同模型时 metadata 可能暂留旧模型，下一条 assistant 消息会按实际模型纠正；
首次 UI 附着修复不代表已解决这一原生事件时序限制。

| 原生能力 | 本项目处理 |
| --- | --- |
| OAuth affinity、账号排序、reserve、block/rotation | 全部沿用原生；不增加账号选择扩展 |
| API-key affinity / session credential pin | API-key 选择仍不读 affinity，原生日志 pin 只记录 OAuth；保留 API-key 扩展 |
| Herdr `/fork pane/window/tab` | 原生负责创建终端，仍需扩展发布模型、显示名和 TPS metadata |
| 原生 TokenRateMeter | 扩展上下文不公开实例；保留移植算法并复用宿主 Tokenizer |
| 跨 profile daemon 共享 | 原生只共享 global daemon；project daemon 仍需现有 pro2 桥接 |

新增 `test/omp-runtime-contract.test.mjs` 会加载本机 `.runtime/omp`，在临时目录中用假凭据验证
受限 pin、branch JSONL 落盘并重开的恢复、活动路径 TPS、首次 UI switch、reset boundary
和原生 compaction 过滤，以及 runtime override、login 优先级与跨连接认证存储更新；不发送模型请求。
无本地二进制时跳过，可用 `HERDR_TPS_OMP_TEST_BIN` 指定其它安装，显式指定但不存在时失败：

```bash
node --test integrations/herdr-tps/test/omp-runtime-contract.test.mjs
```

该测试调用原生 AuthStorage、SessionManager、上下文重建 helper 和宿主 Tokenizer，事件由 fixture 投递，
不等同于实际 TUI 命令、在线账户恢复或侧栏现场验证。

本轮验证：`npm run herdr:tps:test` 155/155 通过（含本机 18.11.0 的七组契约检查）；
`cargo test -p herdr-usage --offline --locked` 116 + 23 + 3 通过；安装 dry-run 的 config/shell/links
均已就绪，daemon 共享 dry-run 为 `already-shared`，`git diff --check` 通过。
交互进程仍需在任务结束后重启，再观察真实会话切换与侧栏显示。

### 18.8.6

upstream [`v18.8.4...v18.8.6`](https://github.com/can1357/oh-my-pi/compare/v18.8.4...v18.8.6)
含 v18.8.5、v18.8.6 两个 tag；compare API 显示 291 个 commit、243 个文件（分 tag 为
v18.8.4...v18.8.5 183 commit/149 文件，v18.8.5...v18.8.6 108 commit/128 文件），
变更面覆盖 auth-broker/gateway、auth 选择、catalog 计价、session/picker、Tern TUI 与 AgentLoop。

逐字节未变（v18.8.4 与 v18.8.6 两 tag raw 内容 `cmp` 相同，共比对 27 个接缝文件、其中 20 个相同）：
`packages/ai/src/auth/{blocks,rotation,cascade,auth-storage,store,resets,usage,provider-session-state}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`extensibility/extensions/{lifecycle-mirror,runner,wrapper}.ts`、
`session/{session-storage,credential-pin,session-context}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`、`utils/dirs.ts`。
因此事件定义、TPS 分词与 `TokenRateMeter`、认证存储、session 目录解析与 credential pin、采集依赖的
launcher/参数解析全部未变。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/auth/affinity.ts` 与 `auth/types.ts`：`inherit` 新增可选 `include` 过滤器
  （v18.8.5 `feat`），只影响 `sessions.inherit` 复制哪些 affinity；本扩展不调用 inherit，无影响。
- `packages/ai/src/auth/select.ts`：`inReserve` 改用 `isWithinUsageReserve()`（修正恰好等于
  `reservePct` 的账号仍被选中并报 healthy），`enforceAccounts` 的匹配键从 `accountId` 换成
  `oauthAccountKey()`（`accountId ?? email ?? projectId`）——只影响 Antigravity/OAuth 多账号排序，
  `selectApiKey`（API-key 路径）不读 affinity，本地 sticky pin 不变。
- `packages/ai/src/auth/oauth.ts`、`auth/types.ts`：`accessById` 带 `forceRefresh` 时改为真正重新
  mint 该账号并在刷新后重定位行索引；新增 `refreshReason: "auth-recovery"` 与
  `refreshOAuthCredentialMints` 选项。opencode-go 用 login/stored API key，不经过该路径。
- `packages/coding-agent/src/session/session-manager.ts`：新增静态 `isFromRemovedWorktree`/
  `openRelocated`（worktree 模式下被移除目录的 session 可迁移恢复），`listForPicker` 并入仓库其它
  git worktree 的兄弟 session 目录。本扩展消费的
  `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`appendResetBoundary`
  签名与 session JSONL 行形状不变；`getDefaultSessionDir` 未变。
- `packages/coding-agent/src/extensibility/extensions/types.ts`、`docs/extensions.md`：仅措辞补充
  （`deliverAs: "aside"` 会结束运行中的 interruptible wait）；没有新增/删除事件，`shared-events.ts`
  未变。
- 其余为 catalog（prompt-cache lookback、Antigravity 账号发现、Haiku 5.5 计价）、AgentLoop
  （`hasQueuedAsides`、warm-cache 感知裁剪）、coding-agent（`worktree.onStart/onExit` 设置、
  judge 链失效、advisor role 解析、compaction 阈值）、tui 与 natives；18.8.6 修好了编译版二进制中
  扩展导入 `@oh-my-pi/pi-tui/native/*` 的解析（本扩展只导入 `@oh-my-pi/pi-agent-core`，
  18.8.4 上已验证可加载，不受影响亦可受益）。

bundle 接口出现次数 18.8.4 → 18.8.6（`grep -ao` 对两份二进制分别计数）：`getWithCredential` 10 → 10、
`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`appendEntry` 19 → 19、
`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、`message_end` 63 → 63、
`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、`session_tree` 5 → 5、
`tryAcquireSessionLease` 5 → 5、`before_provider_request` 4 → 4 全部一致；`getSessionId` 181 → 183
（worktree picker 新增两处调用），本扩展不消费 picker 路径。

实机验证（`omp-linux-x64` SHA256
`877acdc48384b80fe4c083b1e610433d28922dd9fccde9ea430aaace8765b19b`，`--version` → `omp/18.8.6`）：

- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.8.6 runtime。
- 经 `--mode rpc --no-session --no-ui` 加载临时探针，runtime 发出 `ready`（protocol v1，支持
  v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）；`context.agent.kind === 'main'`、
  `hasUI === false`；20 个事件名全部注册成功；`authStorage` 上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 全部就位，
  `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`、
  `credentials.list`、`credentials.onGeneration`、`blocks.list` 与 sessionManager 的
  `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`getBranch`/`appendResetBoundary`/
  `appendCustomEntry`/`onPersistenceNotice`、`pi.appendEntry` 全部是 function；宿主
  `@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`；usage.db 中 opencode-go 行仍为
  `account_source=session_pin`、账号 `OpenCode Go Key b2e42461`，路由归因未漂移。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test -p herdr-usage --offline --locked`：116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `already-shared`（pro2 daemons 仍共享）。
  本地 sticky pin、TPS reporter 及 herdr-usage 采集链路全部通过，扩展与采集代码无需修改。

### 18.11.0

upstream [`v18.10.4...v18.11.0`](https://github.com/can1357/oh-my-pi/compare/v18.10.4...v18.11.0)
官方 release 日期 2026-10-14。变更主要集中在语音听写与交互体验、会话构建标记、工具审批交互解耦、后台任务指引、扩展事件异步等待状态、浏览器操作可靠性、多项目配置隔离、上下文压缩与模型缓存优化：

- **语音听写与交互支持**：
  - Tern 在 Send 旁新增麦克风按钮，支持点击录制、停止后审阅并提交转录；录制就绪前点击可取消初次模型加载（PR #15759）；
  - 新增本地 Nemotron 流式听写（streaming dictation），边说边出字，录音前等待识别器完全就绪（PR #15759）；
  - Agent Frontmatter 新增 `structured: true`：即使全局 `task.structured` 关闭，带该属性的子代理仍保留 `yield` 工具及 `output` schema 校验（PR #15717）。
- **会话溯源与工具审批**：
  - **会话 Entry 构建标记**：会话 entry 记录写入该 entry 的 OMP 构建版本（版本、commit、branch、未提交 diff hash 及 `OMP_BUILD_LABEL`），跨升级长开的会话也能验证补丁是否生效；`/session` 命令支持列出写入会话的全部构建（PR #15680）；
  - **审批挂起时 Composer 保持可用**：工具审批挂起时输入框保持可用；直接发送消息将拒绝该工具调用并以该消息 steer 轮次，使 Agent 明确知晓拒绝原因（PR #15642）；
  - **输入框审批提示紧凑化**：将待审批命令、路径或内容压缩为单行展示以节省空间，按 `Ctrl+O` 可展开完整详情（PR #15642）；
  - **后台任务指引迁移**：后台任务指引移至 Task 工具 description（及委托给 Task 的同名扩展），不再在每次 spawn acknowledgment 中重复输出（PR #15477）；
  - **设置解耦提示**：明确 Auto-Promote Context 与 Retry Model Fallback 相互独立，关闭 `retry.modelFallback` 可停止 `retry.fallbackChains` 的模型切换（Issue #15636）。
- **扩展与事件语义**：
  - **扩展 agent_end 异步等待标记**：`agent_end` 事件在等待后台作业（background-job wait）时不再报告为计划续轮（scheduled continuation），而是携带 `awaitingAsyncWork: true` 并行附带 `willContinue`（Issue #15348）。本扩展的 TPS pause 与 Herdr 状态监控均兼容该字段。
- **浏览器与自动化修复**：
  - 修复 `tab.click("text/…")` 在可视区域下方或后台标签页时超时的问题（PR #14864, #15496）；
  - 修复页面未实际接收到的点击（如 disabled 控件或 `inert` 子树）误报成功的问题（PR #15496）；
  - `tab.ariaSnapshot()` 隐藏密码框明文值，改为掩码圆点，防止密码进入上下文（PR #15502）；
  - 修复 `tab.text()` 和 `tab.html()` 无选择器时抛出异常的问题，改为读取 page body（PR #15599）；
  - 修复 `page.waitForNetworkIdle` 在 browser `run` 中超时前次 run 启动的请求问题（PR #15495）；
  - 修复 relay 页面通过 `Target.createTarget` 打开的 tab 遗落到无法 attach 的独立 Chrome 窗口的问题（PR #15494）；
  - 修复 macOS `computer` 工具调用挂起后屏幕持续常亮的问题（PR #15414）。
- **配置隔离、安全与插件生态**：
  - 修复在共享临时目录（`/tmp`、`/var/tmp`）或重写 `HOME` 时误加载其他账户或外部 `.omp` 插件、`SYSTEM.md`、规则与技能的问题（PR #15719）；
  - 修复 agent 声明包含空/非法 `tools` 时误赋予全部默认内置工具的问题，改为赋予 `[]`（PR #15718）；
  - 修复在无 Bun 环境下 `omp install`/`omp plugin install` 等命令报 `Executable not found in $PATH: "bun"` 的问题（Issue #15480）；
  - 修复 TUI `/plugins enable` 与 `/plugins disable` 未能即时刷新技能、命令与 MCP 的问题（Issue #15383）；
  - 修复插件市场在 `.claude-plugin/plugin.json` 中的版本优先权与自动更新支持（Issue #15585）；
  - 修复 ACP bash 审批无视 `bash.patterns` 导致高危命令绕过确认的问题（Issue #15473）；
  - 修复项目记忆 / `learn` 误用 Settings 目录导致跨项目污染的问题（Issue #15375）。
- **上下文压缩、缓存与模型交互**：
  - 系统提示词中的 `## MCP Server Instructions` 按 server 名称稳定排序，避免连接延迟乱序破坏 prompt cache（PR #15705）；
  - 修复 `/tan` 将父 `xd://` 工具顶层暴露且丢弃 `ask` 导致丢失父级 prompt cache 的问题（Issue #15578）；
  - 修复含 eval 历史会话在压缩后 fork 或 reload 丢失 prompt cache 的问题（PR #15695）；
  - 修复 eval `agent()` 的 `wait()` 消费过的后台作业超时后被重复注入的问题（Issue #15625）；
  - 修复 OpenAI/Codex 自动压缩在超出上下文窗口时报错失败的问题，改为保留近期轮次并压缩更早历史（PR #15712）；
  - 修复自动压缩无视 `retry.maxDelayMs` 死等超长 Retry-After 的问题（Issue #15311）；
  - 修复 Anthropic 拒绝回退兑换时使用陈旧系统提示词与工具的问题（PR #15702）；
  - 修复技能描述压缩失败重复请求相同 smol 模型的问题（PR #15720）；
  - 修复重试模型回退未跳过子代理账号池 `task.agentAccountPools` 以外模型的问题（Issue #15423）。
- **Git 与文件操作**：
  - `omp commit` 与 `/git` 遵循 Git `commit.gpgsign` 配置进行 GPG 签名，签名失败时保持 HEAD 不变（Issue #14810）；
  - 修复 hashline 编辑静默丢弃被列宽截断的行尾的问题，必须通过 `:raw` 读取完整行后才允许编辑（Issue #15649）；
  - 修复关联 worktree 递归 submodule 在隔离任务下的 pin 丢失问题（Issue #15457）；
  - 修复 HTTP 204 No Content 被 `read` 报为 fetch 失败的问题（Issue #15667）；
  - 修复 `/mcp reauth` 401 挑战仅包含 `Bearer resource_metadata="…"` 时的端点发现（Issue #15690）；
  - 修复 `read skill://<name>` 未知 skill 在 MCP 连接时卡死的问题（Issue #15648）；
  - 修复 `/new` 忽略 `plan.defaultOnStartup` 设置的问题（Issue #15627）；
  - 修复 Cursor 模型 Read 图像返回原始字节的问题（Issue #15630）；
  - 修复 Mermaid 图表中 `<`、`>`、`&`、`"` 被转义为 HTML 实体的问题（Issue #15619）；
  - 修复编译型扩展重复打包 `@oh-my-pi/pi-tui` 子路径导致终端/主题状态未共享的问题（Issue #15659）；
  - 修复 bash service `ready.timeout` 期间阻塞用户/对等节点 steer 消息排队的问题（Issue #15653）。

逐字节未变（两版本比较中核心契约路径继续一致）：
`packages/ai/src/auth/` 全部文件（`auth-storage.ts`、`select.ts`、`affinity.ts`、`rotation.ts`、`blocks.ts`、`cascade.ts`、`types.ts`、`oauth.ts`、`store.ts`、`resets.ts`、`usage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/session/{session-paths.ts}`、
`commands/launch.ts`、`cli/args.ts`。
扩展核心监控事件定义、`TokenRateMeter` 算法、认证底层、SessionManager 核心路径、Session JSONL 结构继续保持一致；“API-key 选择路径不读 session affinity”继续成立，本地 session 粘性路由补丁继续完全适用。

bundle 接口出现次数 18.10.4 → 18.11.0（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`TokenRateMeter` 2 → 2、`willContinue` 37 → 39、`awaitingAsyncWork` 7 → 12、
`plan_review_requested` 3 → 3、`plan_review_resolved` 2 → 2、`structured` 259 → 272。
核心契约逐项保持稳定。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`c23a58ad8a92f4e738aa12dc904f3b0a1ac7035b953b9bf5d39b71a226179f6f`，与 release 构建一致，
`--version` → `omp/18.11.0`）：

- 本次升级命中 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.10.4` 的增量补丁 `omp-linux-x64.patch`（59.5 MB，相比全量 265.8 MB 节省约 77.6% 带宽）；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `c23a58ad8a92f4e738aa12dc904f3b0a1ac7035b953b9bf5d39b71a226179f6f` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.11.0 runtime（输出 `18.11.0`）。
- `omp-with-tps.mjs --print "respond with pong only"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.11.0 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.10.4

upstream [`v18.10.3...v18.10.4`](https://github.com/can1357/oh-my-pi/compare/v18.10.3...v18.10.4)
compare API 显示 105 个 commit、191 个文件。变更主要集中在 `pi-agent-core`（工具审批参数行数感知、loop context 传递与 summarizer 模型支持、跨模型 snapcompact reasoning 剔除）、`pi-ai`（Codex 网络安全策略错误重试保留 Daybreak、移除 toolNamespacesInfo 流式选项与 Codex Code Mode 彻底移除）、`pi-catalog` 与 `snapcompact`（Muse Wonder 订阅定价、Gemini 4 Argon 定价与 2026-12-01 生效排期、支持阶梯费率排期卡片、Devin 缓存写入与长上下文计价修复、Kilo 与 NanoGPT 动态价格与模型元数据拉取、snapcompact 支持 reasoning 剔除归档变体）、`pi-coding-agent`（扩展支持交互式 Plan Review 监听、重新打开与角色 tier 选择、`omp gc --compress` 透明压缩闲置会话、工具审批弹窗整合进输入区）、`pi-natives` 与 `pi-tui`：

- `packages/agent-core` & `packages/ai`：
  - **工具审批参数详情**：`AgentTool.formatApprovalDetails` 接收 `ApprovalDetailsOptions`（`compact: true` 允许紧凑审批框统计行数，PR #15550）；
  - **模型调用钩子上下文**：Before-model-call hooks 接收 loop context 作为第二参数，`buildProviderContext` 接收 summarizer 模型（PR #15436）；
  - **跨模型思考上下文过滤**：修复 Claude 压缩与分支摘要读取 snapcompact 跨模型带 reasoning 的归档导致 `reasoning_extraction` 被拒问题（PR #15436）；
  - **Codex 策略错误处理**：修复 Codex 网络安全策略错误时误重试关闭 Daybreak 模式的问题（PR #15472）；
  - **移除 Codex Code Mode**：移除 `toolNamespacesInfo` 流式选项，不再发送 `tool_namespaces_info` / `code_mode_tool_names` 客户端元数据；彻底移除 Codex Code Mode 及其 `providers.openai-codex.codeMode` / `codeModeDirectTools` 配置与代码模式声明生成器，Codex 模型全面使用统一的直接工具接口。
- `packages/catalog` & `packages/snapcompact`：
  - **新增定价与模型阶梯**：新增 Muse Code 的 Muse Wonder Full（$4/$0.20/$20）与 Wonder contributor（$1.60/$0.08/$8）订阅定价（PR #15522）；新增 Cursor 与 Google Antigravity 上的 Gemini 4 Argon 定价（2026-12-01 统一调整为 $4/$0.20/$20）；
  - **阶梯排期费率卡片**：支持未来生效日期的阶梯费率排期卡片（`effective-rates` card），构建与查询时精准按请求当时生效卡片计价，非周期性生效排期不显示高峰状态标记（PR #15620）；
  - **定价缺陷修复**：修复 Devin 模型的 prompt-cache write 计价（1.25x input）、长上下文阶梯定价（>200K / 272K tokens）及 Fusion 配对规则丢失问题（PR #14891）；修复 Kilo 与 NanoGPT 模型价格、上下文窗口与输出上限丢失问题，改从网关 live model list 动态获取（PR #15623）；
  - **无思考归档视图**：snapcompact 新增 `viewWithoutThinking()`、`textWithoutThinking()`、`readerIncludesThinking()` 及 `Archive.withoutThinking` 归档变体，使不能见 reasoning 的模型安全消费压缩归档（PR #15436）。
- `packages/coding-agent` & `packages/tui`：
  - **交互式计划审批扩展扩展点**：扩展系统可监听并响应交互式 Plan Review（`plan_review_requested`、`plan_review_resolved` 事件），支持提供 refinement 反馈或直接 save-and-quit（PR #14250）；会话空闲时扩展可通过 `ctx.openPlanReview()` 主动重新打开，并通过 `ctx.resolvePlanReview` 选择执行角色 tier（PR #15552）；
  - **闲置会话透明压缩**：新增 `omp gc --compress`（或 `gc.compress` 配置），对空闲超过 `gc.compressAfterDays`（默认 7 天）的会话透明使用文件系统压缩（APFS, NTFS），同时保持可 resume 与全文可搜（PR #15600, #15616）；
  - **工具审批交互重构**：审批弹窗替换 composer 输入区（类似 `ask`），支持逐行展示详情，默认快捷键调整为 `Ctrl+Enter` / `Ctrl+Q`（PR #15548）；
  - **会话与浏览器修复**：修复会话恢复时 late probe 探针倒回对话历史以及陈旧历史扫描降低 parked 子代理开销的问题（PR #15594）；修复独立共享浏览器（headless/headed）崩溃或重启时重新打开前次全部未关闭页面的问题，禁止通过 `proc://<name>/mode` 进行 detach（PR #15561）；
  - **TUI 细节修复**：修复原生终端 `/btw` 复制按钮复制按钮标签文本而非回复内容的缺陷（PR #15547）；修复 tmux 嵌套无 client SIXEL 支持时显示六角图像占位符问题（PR #15513）；修复 `pwsh` 代码块语法高亮缺失问题（PR #15010）。

逐字节未变（两 tag compare 完全不含这些路径）：
`packages/ai/src/auth/` 全部文件（`auth-storage.ts`、`select.ts`、`affinity.ts`、`rotation.ts`、`blocks.ts`、`cascade.ts`、`types.ts`、`oauth.ts`、`store.ts`、`resets.ts`、`usage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/session/{session-paths.ts}`、
`commands/launch.ts`、`cli/args.ts`。
扩展核心监控事件定义、`TokenRateMeter` 算法、认证底层、SessionManager 核心路径、Session JSONL 结构继续逐字节一致；“API-key 选择路径不读 session affinity”继续成立，本地 session 粘性路由补丁继续完全适用。

bundle 接口出现次数 18.10.3 → 18.10.4（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 17（因 Codex Code Mode 彻底移除，清理了 `session/code-mode.ts` 内部调用）、
`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 57（同因 Code Mode 移除减少）、
`session_branch` 8 → 8、`session_switch` 8 → 8、`session_tree` 5 → 5、`isRestricted` 9 → 9、
`permits` 20 → 16、`getCredentialPins` 5 → 5、`getSessionFile` 97 → 92、
`getSessionId` 187 → 173、`selectApiKey` 4 → 4、`resolveOAuth` 8 → 8。核心契约逐项保持稳定。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`ff59fc1b484c1308a7247ad208305005627b3fac62b440737c2b1835ac32074e`，与 release 构建一致，
`--version` → `omp/18.10.4`）：

- 本次升级命中 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.10.3` 的增量补丁 `omp-linux-x64.patch`（44.0 MB，相比全量 264 MB 节省约 83.3% 带宽）；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `ff59fc1b484c1308a7247ad208305005627b3fac62b440737c2b1835ac32074e` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.10.4 runtime（输出 `18.10.4`）。
- `omp-with-tps.mjs --print "respond with pong only"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.10.4 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.10.3

upstream [`v18.10.2...v18.10.3`](https://github.com/can1357/oh-my-pi/compare/v18.10.2...v18.10.3)
compare API 显示变更主要集中在 `pi-agent-core`（分词估算与图像定价统一、思考轮次 token 计量修复）、`pi-ai`（端点判定与 `elidedOutputTokens`）、`pi-catalog` 与 `snapcompact`（模型图像预算策略、Cursor / Antigravity 图像与历史截断修正）、`pi-coding-agent`（子代理 soft request budget、兄弟子代理 prompt cache 共享、`omp --restore` 恢复模式）以及 `pi-tui`：

- `packages/agent-core` & `packages/ai`：
  - **图像 token 计量统一**：移除 `@oh-my-pi/pi-agent-core/image-tokens` 中的 `estimateImageContentTokens()`，统一采用 `Tokenizer.imageTokens(size, detail)` 按 catalog 模型图像定价规则核算；
  - **历史重写与思考轮次统计**：`applyShakeRegion(s)` 支持 replacement 的 token 计数，block rewrite 扣除的 token 计入 `elidedOutputTokens`；修复 Claude 重放思考轮次在重新加载后按输出计费尺寸而非签名尺寸统计的问题（解决 Opus 5.5 输入 context 虚高 ~55%）；
  - **OpenAI Responses 与 Codex 推理统计**：重放的加密思考按报告的 reasoning tokens 计入，会话 compaction 与 snapcompact 精确贴合账单；
  - **端点判定**：`isOfficialCodexApiUrl` 迁移至 `@oh-my-pi/pi-catalog/wire/codex`；新增 `reachesOfficialEndpoint(model)`，遵循 `ANTHROPIC_BASE_URL` 与 `FOUNDRY_BASE_URL` 重定向判定是否直连官方端点。
- `packages/catalog` & `packages/snapcompact`：
  - **图像数量与体积预算规则**：引入 `image-count-budget`（Anthropic 90、Codex 200）与官方端点 override（Anthropic 600、Codex 2000）；Codex 支持 24 MB 行内图像预算；
  - **Cursor 与 Antigravity 限制校正**：Cursor 下 Claude/GPT snapcompact 帧按宿主 `image-max-edge`（1024px）缩放，防止 Cursor 压缩导致不可读；修复 Claude 在 Google Antigravity 超过 5 MiB 单图上限失败问题；
  - **Muse Wonder 模型支持**：新增 Meta / Muse Code 的 `muse-wonder-1.0` 模型定义与 pricing，未分类模型支持 `thinking-unverified` 推理阶梯。
- `packages/coding-agent` & `packages/tui`：
  - **子代理独立请求预算**：新增配置 `task.agentSoftRequestBudget`，允许为命名子代理独立配置 soft request budget；
  - **兄弟子代理 prompt-cache 共享**：在 Muse Code 等兼容提供商上为并行兄弟子代理启用 prompt-cache key 共享（同时保持对话独立），私有 cache key 不泄露至共享会话导出；
  - **会话崩溃恢复命令**：新增 `omp --restore <session>` 命令，在客户端崩溃或被 kill 时不仅恢复会话，还能自动继续未完成的 turn 与子代理执行；
  - **工具输出精简**：移除 bash 毫秒级耗时输出与冗余任务尺寸元数据，降低工具结果 token 占用；
  - **修复**：修复 `snapcompact.toolResults` 重复将已发送结果图片化破坏 prompt cache 和 Anthropic 签名思考的问题；模型选择器对未验证模型提供手动 effort 选择。

逐字节未变（两 tag compare 完全不含这些路径）：
`packages/ai/src/auth/` 全部文件（`auth-storage.ts`、`select.ts`、`affinity.ts`、`rotation.ts`、`blocks.ts`、`cascade.ts`、`types.ts`、`oauth.ts`、`store.ts`、`resets.ts`、`usage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/{types,lifecycle-mirror,runner,wrapper}.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin,session-context,session-paths,session-manager}.ts`、
`commands/launch.ts`、`cli/args.ts`。
因此扩展事件定义、`TokenRateMeter` 算法、认证底层、SessionManager 接口、session 目录与 JSONL 形状、launcher 与参数解析全部未变；“API-key 选择路径不读 session affinity”继续成立，本地 session 粘性路由补丁继续完全适用。

bundle 接口出现次数 18.10.2 → 18.10.3（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5、`isRestricted` 9 → 9、`permits` 20 → 20、`getCredentialPins` 5 → 5；
`getSessionFile` 97 → 97、`getSessionId` 187 → 187。消费的核心契约逐项保持一致。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`366ffcda80db57b825921b6ff4b0e5e623bc62cf18478f2828cd8a322fc72ff9`，与 release 构建一致，
`--version` → `omp/18.10.3`）：

- 本次升级命中 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.10.2` 的增量补丁 `omp-linux-x64.patch`（6.4 MB，相比全量 264 MB 节省约 97.6% 带宽）；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `366ffcda...` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.10.3 runtime（输出 `18.10.3`）。
- `omp-with-tps.mjs --print "respond with pong only"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.10.3 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.10.2

upstream [`v18.10.1...v18.10.2`](https://github.com/can1357/oh-my-pi/compare/v18.10.1...v18.10.2)
compare API 显示 26 个 commit、138 个文件。变更主要分布在 `pi-natives`/`crates/pi-builtins`（GNU coreutils 9.x 兼容内置命令扩充）、`pi-ai` 与 `pi-catalog`（`CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS` 兼容降级）、`pi-coding-agent`（子代理文本返回与任务卡片后台流式输出）及 `pi-tui`（`/tree` 分支切换与视觉组件刷新）：

- `crates/pi-builtins` & `packages/natives`：
  - **GNU coreutils 内置命令扩充**：新增进程内 `env`、`du`、`chmod`、`fold`、`rmdir`、`od`、`df`、`uptime`、`id`、`dd` 命令，对齐 GNU coreutils 9.x 选项与退出码（Unix 环境启用 `chmod` 与 `id`，`dd` 支持通过 `PI_DISABLE_UUTILS_DESTRUCTIVE` 禁用）；
  - **`nl` 行号工具**：从 uutils coreutils 0.8.0 移植，支持页眉/页脚/正文行号样式（含 `pBRE` 正则）、`-l` 空行分组与 `ln`/`rn`/`rz` 编号格式。
- `packages/ai` & `packages/catalog`：
  - **Anthropic 实验性 Beta 降级开关**：支持环境变量 `CLAUDE_CODE_DISABLE_EXPERIMENTAL_BETAS`（对应 catalog 轴 `compat.honorsClaudeCodeBetaKillSwitch`），开启时请求自动剥离 `context_management`、beta 工具字段（`strict`、`defer_loading`）、task budgets、结构化输出、`thinking.block_binding` 与服务端压缩，确保不支持实验性特性的自建或企业 `ANTHROPIC_BASE_URL` 网关正常工作（PR #15458）。
- `packages/coding-agent`：
  - **纯文本子代理与结构化开关**：新增 `task.structured` 配置（默认关闭）。默认情况下，`task` 工具派生的子代理直接将最终消息作为纯文本返回（不再注入 `yield` 工具、输出 schema 或 "Submit Result" 提交框）；eval `agent()` 与 workpool 派生仍保留结构化返回；
  - **后台任务卡片流式输出**：Tern 中后台运行的 `bash` 命令或 `eval` 单元格实时流式输出至卡片（带 live 运行计时器），任务结束或中断时显示对应终态（Cancelled 等）；`wait` 工具输出收敛为单行（类型、ID、标签、耗时）；
  - **修复**：非结构化任务无最终消息时报错；后台任务卡片不再因 turn 结束而提前停止更新；修复透明图像转黑底及缩放边缘黑边问题。
- `packages/tui`：
  - **`/tree` 会话分支快捷跳转**：新增跨可见分支与嵌套 fork 的导航动作 `app.tree.previousBranch` / `app.tree.nextBranch`，可在 `keybindings.yml` 中自由绑定（如 `Shift+Left` / `Shift+Right`）（PR #12623）；
  - **模型选择器品牌徽章**：`/model`、快速切换与 advisor 选择器前缀增加供应商 logo（`mark.icon: "provider:<id>"`）；
  - **后台任务指示器精简**：运行态底栏显示 "In background" 与实时时钟。

逐字节未变（两 tag compare 完全不含这些路径）：
`packages/ai/src/auth/` 全部文件（`auth-storage.ts`、`select.ts`、`affinity.ts`、`rotation.ts`、`blocks.ts`、`cascade.ts`、`types.ts`、`oauth.ts`、`store.ts`、`resets.ts`、`usage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/{types,lifecycle-mirror,runner,wrapper}.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin,session-context,session-paths,session-manager}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`。
因此扩展事件定义、`TokenRateMeter` 算法、认证底层、SessionManager 接口、session 目录与 JSONL 形状、launcher 与参数解析全部未变；“API-key 选择路径不读 session affinity”继续成立，本地 session 粘性路由补丁继续完全适用。

bundle 接口出现次数 18.10.1 → 18.10.2（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5、`isRestricted` 9 → 9、`permits` 20 → 20、`getCredentialPins` 5 → 5；
`getSessionFile` 97 → 97、`getSessionId` 187 → 187。消费的核心契约逐项保持一致。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`3d8f967bb4cf2a154592115edaf4bb723d7a6a6448eec933ac5eb49344938125`，与 release 构建一致，
`--version` → `omp/18.10.2`）：

- 本次升级命中 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.10.1` 的增量补丁 `omp-linux-x64.patch`（56.7 MB，相比全量 263 MB 节省约 78.4% 带宽）；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `3d8f967b...` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.10.2 runtime（输出 `18.10.2`）。
- `omp-with-tps.mjs --print "respond with pong only"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.10.2 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.10.1

upstream [`v18.10.0...v18.10.1`](https://github.com/can1357/oh-my-pi/compare/v18.10.0...v18.10.1)
compare API 显示 9 个 commit、66 个文件。变更主要集中在 `packages/tui` 与 `packages/coding-agent` 的交互与视觉层：

- `packages/tui` & `packages/coding-agent`：
  - **回答复制快捷按钮**：Tern 回答底部 turn footer 首部新增复制按钮（`omp.turn.copy`，替代原时钟图标），支持一键复制回答文本；
  - **用户消息工具栏图标化**：`UserMessageComponent` hover 工具栏（`omp.user.tools`）重构为 Copy 与 Rewind 图标按钮（`copy`, `rewind`）加时间戳显示；
  - **后台任务与 Agent 紧凑合并计数器**：Tern composer 上方原有的两个独立 "N running" 胶囊合并为一个紧凑计数器（分别以图标和数字显示 running agents 与 background jobs，hover 显示文本，点击展开各自管理面板）；
  - **单文件 `grep` 显示折叠归组**：修复 Tern 中对单文件执行 `grep` 时展示为离散框选代码片段且无文件行和高亮的问题，现在统一在所属文件行下方分组展示；
  - **原生倒计时修复**：修复 `CountdownTimer` 在原生端发送 `stopped: 0` 导致全部原生倒计时（扩展输入、选择器、编辑器提示）从一开始就卡死在 0 秒的问题；
  - **提升层 Sheet 焦点修复**：修复原生后端焦点无法穿透到从 dock 组件提升至顶层的 overlay/picker 弹窗（如 MCP 添加向导、login 对话框）内部输入框的问题，恢复实时光标与高亮焦点环；
  - **原生弹窗与选择器重构（命令面板风格规范化）**：
    - `/session pin` 与 `/usage reset` 统一采用与小型选择器相同的 picker sheet（当前生效账号带选中标记，reset 前显示确认提示条，不可用账号灰显并提示原因）；
    - Thinking level 选择器采用量表（meter）展示各思考级别深度；
    - 插件与队列模式选择器改用徽章（badge：版本、已安装状态、生效范围、推荐）与自然语言名称；Theme 选择器移除冗余的 "(current)" 文本；
    - 扩展打开的文本编辑器及 ask 工具自定义回答/备注改为 composer 位置的原位 sheet（顶部标题与详情，文字直接在 sheet 上输入，Open in editor / Cancel / Submit 按钮快捷键移入 tooltip）；
    - `MCPAddWizard` 分步原生向导（阶段进度条、单步表单、字段校验、状态行与审查汇总，保留被拒输入，清理回退状态）；
    - `JobsSheet` 采用 master–detail 分割视图（Running / Recent 分组，各行带状态色彩与持续时间，展示选中任务命令、事实参数与实时输出，task jobs 不再在此处绘制冗余 agent 节点）；
    - `BtwHistoryPanel` 原生邮件式居中阅读器（问题列表 + 线程阅读器 + composer + 底部操作按钮）；
    - `ContextUsageView` 原生布局刷新（展示已用 tokens 与自动压缩前剩余可用空间，单条带刻度堆叠条，按分类行展示色彩块与占比，带 snapcompact 预估）。

逐字节未变（两 tag compare 完全不含这些路径）：
`packages/ai/src/auth/` 全部文件（`auth-storage.ts`、`select.ts`、`affinity.ts`、`rotation.ts`、`blocks.ts`、`cascade.ts`、`types.ts`、`oauth.ts`、`store.ts`、`resets.ts`、`usage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/{types,lifecycle-mirror,runner,wrapper}.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin,session-context,session-paths,session-manager}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`。
因此扩展事件定义、`TokenRateMeter` 算法、认证底层、SessionManager 接口、session 目录与 JSONL 形状、launcher 与参数解析全部未变；“API-key 选择路径不读 session affinity”继续成立，本地 session 粘性路由补丁继续完全适用。

bundle 接口出现次数 18.10.0 → 18.10.1（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5、`isRestricted` 9 → 9、`permits` 20 → 20、`getCredentialPins` 5 → 5；
`getSessionFile` 97 → 97、`getSessionId` 187 → 187。消费的核心契约逐项保持一致。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`9798d22d518b23342b4cadf57672b276ef09e756c1832adc6571b04b26ad4f1b`，与 release 构建一致，
`--version` → `omp/18.10.1`）：

- 本次升级命中 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.10.0` 的增量补丁 `omp-linux-x64.patch`（4.2 MB，相比全量 263 MB 节省约 98.4% 带宽）；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `9798d22d...` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.10.1 runtime（输出 `18.10.1`）。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.10.1 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.10.0

upstream [`v18.9.2...v18.10.0`](https://github.com/can1357/oh-my-pi/compare/v18.9.2...v18.10.0)
compare API 显示 22 个 commit、126 个文件。主要变更集中在 natives（Rust 原生 IPC 传输层重构）、coding-agent（本地 IPC 端点接入原生传输、跨机 session/auth 迁移命令、流式问答 Alt+H 折叠隐藏、Mnemopi 跨 session 共享嵌入服务）、catalog（OpenAI Codex 默认上下文窗口扩增至 500K）以及 tui（问答与 HUD 折叠收缩时的转录回填与多项空行修复）。逐个接缝核对：

逐字节未变（两 tag compare 完全不含这些路径）：
`packages/ai/src/auth/` 全部文件（`auth-storage.ts`、`select.ts`、`affinity.ts`、`rotation.ts`、`blocks.ts`、`cascade.ts`、`types.ts`、`oauth.ts`、`store.ts`、`resets.ts`、`usage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/{types,lifecycle-mirror,runner,wrapper}.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin,session-context,session-paths,session-manager}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`。
因此扩展事件定义、`TokenRateMeter` 算法、认证底层、SessionManager 接口、session 目录与 JSONL 形状、launcher 与参数解析全部未变；“API-key 选择路径不读 session affinity”继续成立，本地 session 粘性路由补丁继续完全适用。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/natives` & `crates/pi-natives/src/ipc/`：新增 Rust 实现的单机原生 IPC 传输（`IpcServer`、`IpcConnection`、`ipcEndpoint`），支持 Unix domain sockets 与 Windows named pipes，具备属主隔离、单实例绑定与陈旧端点接管能力；本地 broker、LSP mux、IDA host、stream、collab 发现和本地模型 worker 接入该传输。本仓库 `share-omp-daemons.mjs` 基于文件系统的 PID 与套接字目录发现不受影响（dry-run 输出 `already-shared`）。
- `packages/coding-agent/src/`：
  - 新增跨机迁移命令：`omp session pack`、`unpack`、`moved`（生成与还原包含 manifest、session.jsonl、artifacts、blobs 的 tar 包；旧副本 resume 时自动 fork 并添加转移标记）以及 `omp auth export` / `import`。采集器只读本地活动 session JSONL，不影响已有解析与归因。
  - 新增交互问答折叠：`Alt+H`（`app.ask.hide`）将挂起的 ask 对话框折叠为单行，不作回答；Enter 或 Show 恢复。
  - Mnemopi 共享嵌入服务：改由共享的后台 embedding server 按需加载并在空闲 15 分钟后退出，下载任务在 session 退出后继续完成。
  - `/fork` 终端增强：支持在 turn 正在流式生成时直接 `/fork` 创建新 Tern pane 并保持原 session 继续生成。
- `packages/catalog`：`openai-codex/gpt-6.1-sol` 与 `gpt-6.1-sol-wm` 的默认上下文窗口由 272K 提高至 500K（扩展上下文保持 922K）。
- `packages/tui`：修复 ask 对话框、任务报告或 HUD 关闭后屏幕留白的问题（转录区域自动重新填满屏幕）；移除 `Composer.pinInputToBottom` 与 `RuntimeChildrenOptions.transient`；修复跨屏顶部分割行丢弃标点符号行及 SIXEL 图像指纹重复提交的问题。

bundle 接口出现次数 18.9.2 → 18.10.0（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 17 → 17、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 57 → 57、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5、`isRestricted` 9 → 9、`permits` 16 → 16、`getCredentialPins` 5 → 5；
`getSessionFile` 88 → 91、`getSessionId` 169 → 170（因新增 session pack/unpack/moved 增加调用点）。消费的核心契约逐项保持一致。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`d5c6f92cabc231b4fb94c47322fc67b3c473c3b107339237a4b2f16505b38342`，与 release 构建一致，
`--version` → `omp/18.10.0`）：

- 本次升级再次命中 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.9.2` 的增量补丁 `omp-linux-x64.patch`（56.9 MB，相比全量 262.4 MB 节省近 80% 带宽）；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `d5c6f92c...` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.10.0 runtime（输出 `18.10.0`）。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.10.0 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.9.2

upstream [`v18.9.1...v18.9.2`](https://github.com/can1357/oh-my-pi/compare/v18.9.1...v18.9.2)
compare API 显示 166 个 commit、98 个文件。变更面覆盖 ai（TypeSafeJudge 重命名为 NativeJudge、OpenAI Decisions API 与 Cloudflare Workers AI 裁决接入及自动分批、Anthropic 提示词缓存内存优化、Devin 切换修复）、coding-agent（长会话中工具后插入文本响应优化、`apply_patch` 重复目标文本歧义修复、`find` 评分分批修复、`omp gc --apply` 临时文件清理、`auth-gateway` OpenRouter 变体路由、`/tan` subagent 开销继承修复、本地模型警告）、natives（Markdown 行内 span/转义回溯卡顿修复）、tui（tmux 内运行 Tern 终端的原生 TSP 渲染支持、长会话渲染流畅度）与 utils（流式 JSON 工具参数解析性能优化）。仍逐个接缝核对。

逐字节未变（两 tag compare 不含这些路径）：
`packages/ai/src/auth/` 全部文件（`blocks`、`rotation`、`cascade`、`affinity`、`types`、`oauth`、`select`、`store`、`resets`、`usage`、`auth-storage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`extensibility/extensions/{types,lifecycle-mirror,runner,wrapper}.ts`、
`session/{session-storage,credential-pin,session-context,session-paths,session-manager}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`。
因此扩展事件定义、`TokenRateMeter` 上游算法、认证存储底层、session 目录与 JSONL 形状、SessionManager 接口、采集依赖的 launcher/参数解析全部未变。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/judge/*`（破坏性重命名）：`TypeSafeJudge` 重命名为 `NativeJudge`，`TypeSafeApiError` 重命名为 `JudgmentApiError`；移除 `JUDGMENT_ROUTES`，统一使用 `isJudgmentApi` 或 `createNativeJudge`。本扩展不调用裁决 API，不受影响。
- `packages/ai/src/providers/`：新增 `openai-decisions` 与 `workers-ai-systemone` 原生裁决路由；新增 `loginCloudflareWorkersAi`。opencode-go 与 Antigravity 原生认证未变。
- `packages/coding-agent/src/`：长会话转录中工具调用后追加 assistant 文本的延迟大幅降低；`apply_patch` 处理无上下文 hunk 时若目标文本重复出现不再误套用；`omp gc --apply` 修复中断保存时遗留临时会话文件的问题；`/tan` fork 不再继承父任务 subagent 开销。本扩展与采集逻辑均不受影响。
- `packages/natives` 与 `packages/utils`：Markdown 工具预览采用无指数回溯的语法树；流式 JSON 解析性能提升。

bundle 接口出现次数 18.9.1 → 18.9.2（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5、`isRestricted` 9 → 9、`permits` 20 → 20、`getCredentialPins` 5 → 5、
`getSessionFile` 94 → 94、`getSessionId` 186 → 186 全部逐项保持一致；消费接口本身未变。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`647237242329897c3a7c0f3591fdd9cafe4a4e13872baa9bbdfe45b84f9d8992`，与 release 构建一致，
`--version` → `omp/18.9.2`）：

- 本次升级再次命中 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.9.1`（sha256 `0ac24a7c...`）的增量补丁 `omp-linux-x64.patch`（64.7 MB，相比全量 262.4 MB 节省约 75% 带宽），补丁 sha256 校验为 `23a34bb5...`；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `64723724...` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.9.2 runtime（输出 `18.9.2`）。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.9.2 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.9.1

upstream [`v18.9.0...v18.9.1`](https://github.com/can1357/oh-my-pi/compare/v18.9.0...v18.9.1)
compare API 显示 15 个 commit、43 个文件。变更面为 Bedrock Converse 的 thinking binding 控制与 forced tool choice 修复、嵌入式 shell 的无 shebang 脚本执行回退修复、以及 TUI 视口变化时的内联图片图形重叠修复。属于补丁版本发布，仍逐个接缝核对。

逐字节未变（两 tag compare 不含这些路径）：
`packages/ai/src/auth/` 全部文件（`blocks`、`rotation`、`cascade`、`affinity`、`types`、`oauth`、`select`、`store`、`resets`、`usage`、`auth-storage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`extensibility/extensions/{types,lifecycle-mirror,runner,wrapper}.ts`、
`session/{session-storage,credential-pin,session-context,session-paths,session-manager}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`。
因此扩展事件定义、`TokenRateMeter` 上游算法、认证存储与 session 边界、SessionManager 接口、采集依赖的 launcher/参数解析全部未变。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/providers/amazon-bedrock.ts` 与 `packages/catalog/src/compat/axes.ts`、`models.json`（PR #15316）：新增 `supportsThinkingBindingControls`（在 `models.yml` 的 `compat` 中设置）与 `supports-thinking-binding-controls` 轴（默认开启）；为拒绝 Claude thinking-binding controls 的 Bedrock 网关或区域提供关闭选项，关闭时不再发送 `thinking.block_binding`、`thinking-binding-controls-2026-08-01` beta 头和 `/input_transformations` 响应路径。并修复 Bedrock Claude Fable 5.1 在 thinking 关闭且强制 tool choice 时与自适应思考冲突的问题，此时 fallback 至 `auto`。
- `crates/pi-shell/src/shell.rs` 与 `crates/vendor/brush-core/src/sys/tokio_process.rs`（PR #15271）：修复嵌入式 shell 在 `posix_spawn` 下执行缺少 `#!`（shebang）的可执行文本脚本时报 ENOEXEC（"Exec format error"）的问题，保留向系统 shell 回退执行文本脚本的能力。
- `packages/tui/src/components/image.ts` 与 `render/composite.ts`（commit `aababbb`）：修复在视口变化、移动、缩放、替换或隐藏时内联图表与图像在文本上方遗留重叠残影的问题，同时保留回滚缓冲区（scrollback）中的图像。

bundle 接口出现次数 18.9.0 → 18.9.1（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5、`isRestricted` 9 → 9、`permits` 20 → 20、`getCredentialPins` 5 → 5、
`getSessionFile` 94 → 94、`getSessionId` 186 → 186 全部完全一致；消费接口与符号无任何漂移。

实机验证与增量补丁升级（`omp-linux-x64` SHA256
`0ac24a7c146dbfb4005ef00e21aa60bf9b1ddf15dae5f92938542c1475824417`，与 release 构建一致，
`--version` → `omp/18.9.1`）：

- 本次升级命中 18.9.0 新增的 Stencil 增量二进制补丁机制：`.runtime/omp update` 检查到版本差异后，自动下载针对 `18.9.0`（sha256 `12b56956...`）的增量补丁 `omp-linux-x64.patch`（仅 55.3 MB，相比全量 266 MB 减少约 79% 流量），补丁 sha256 校验为 `37ca83e6...`；通过原生 `applyBinaryPatch`（HDiffPatch + zstd）就地应用并校验生成目标二进制 sha256 `0ac24a7c...` 后原子替换。
- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.9.1 runtime（输出 `18.9.1`）。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.9.1 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 183 + 0、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.9.0

upstream [`v18.8.9...v18.9.0`](https://github.com/can1357/oh-my-pi/compare/v18.8.9...v18.9.0)
compare API 显示 572 个 commit、300 个文件。变更面覆盖 ai（SessionsApi.permits 破坏性变更、Google stream 签名、Devin CLI 采样配置、Anthropic 额度与 prompt cache 重试）、coding-agent（Stencil 构建与 HDiffPatch 增量二进制补丁升级、Tern 浏览器目标发现与控制、ctx.annotations 扩展接口、composer predictions 幽灵文本预测、结构化异步 job 元数据、omp usage reset 命令行及额度到期预警）、tui（丰富图表类型与智能推荐、Editor.prediction 预测回调、恢复时加速重建长会话）、natives（applyBinaryPatch 单流增量打补丁、嵌入式 shell 启动加速、grep 列截断优化）与 utils（takeLoopPhaseAttribution 破坏性重命名、流式 JSON 性能优化）。仍逐个接缝核对。

逐字节未变（compare 文件列表均不含这些路径）：
`packages/ai/src/auth/{blocks,rotation,cascade,select,store,resets,usage}.ts`、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`extensibility/extensions/{lifecycle-mirror,runner,wrapper}.ts`、
`session/{session-storage,credential-pin,session-context,session-paths,session-manager}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`。
因此扩展事件清单、`TokenRateMeter` 上游算法、认证存储底层、session 目录与 JSONL 形状、SessionManager 接口、采集依赖的 launcher/参数解析全部未变。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/auth/types.ts` 与 `affinity.ts`（v18.9.0 破坏性变更）：`SessionsApi` 接口新增 `permits(provider, sessionId, credentialId): boolean`，自定义 `SessionsApi` 实现必须实现该方法以支持基于账户池的会话权限校验。本扩展仅在 `sessions.release` 上加壳观察，未替换整个 `SessionsApi` 实现；原生 `auth.sessions.permits` 正常生效。测试 fixture 已同步补齐 `permits` 契约断言与 mock。
- `packages/ai/src/auth-storage.ts`、`sqlite-credential-store.ts`、`policy.ts`、`pool.ts`、`refresh.ts`：导出 `matchesReplacementCredential` 与 `isSameOAuthAccount`；`auth.accountPolicies` 对未匹配 OAuth 账号的选择器放宽为报警并忽略，不再中断启动或登录。opencode-go 使用 API key，不经过 OAuth 策略逻辑。
- `packages/coding-agent/src/extensibility/extensions/types.ts`：新增 `ctx.annotations` 用于 `/annotate` 反馈。本扩展不调用该 API，`ctx.ui` 与事件接口未受影响。
- `packages/pi-natives` 与 `coding-agent`（升级补丁机制）：新增 `applyBinaryPatch`（HDiffPatch + zstd），独立安装包升级改用 Stencil 构建与增量 delta 补丁；本地 `.runtime/omp update` 原地校验 sha256 并更新。
- `packages/pi-utils`（v18.9.0 破坏性变更）：`takeRecentLoopPhase()` 更名为 `takeLoopPhaseAttribution()`；新增 `withLoopPhase`。本扩展不直接依赖该工具函数。

bundle 接口出现次数 18.8.9 → 18.9.0（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5 全部一致；`isRestricted` 8 → 9、`permits` 6 → 20（新增方法调用）；消费接口本身未变。

实机验证（`omp-linux-x64` SHA256
`12b56956bdb3b9fcecd7af7106ddbc4910c9561300ce770d8ca5a7eeecee702c`，与 release `SHA256SUMS.txt`
一致，`--version` → `omp/18.9.0`，由 `.runtime/omp` 直接执行 `omp update` 升级，下载即校验 sha256
后原地替换）：

- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.9.0 runtime（输出 `18.9.0`）。
- 经 `--mode rpc --no-session --no-ui` 加载临时探针：20 个事件名全部注册成功；
  `context.agent.kind === 'main'`、`hasUI === false`；`modelRegistry.authStorage` 上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 全部就位，
  `keys.getWithCredential`、`keys.get`、`sessions.release`、`sessions.isRestricted`、`sessions.permits`、
  `limits.markReached`、`limits.rotate`、`credentials.list`、`credentials.onGeneration` 与 sessionManager 的
  `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`getBranch`/`appendResetBoundary`/
  `appendCustomEntry`/`onPersistenceNotice` 以及 `pi.appendEntry` 全部是 function；宿主
  `@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`；
  `omp-with-tps.mjs --print --model "opencode-go/deepseek-v4.1-flash" "say pong"` 正常返回 `Pong.`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：155 passed（含本机 18.9.0 的七组契约检查）；
  `cargo test --offline --locked`：ai-monitor 181 + 1 + 4、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`。

### 18.8.9

upstream [`v18.8.7...v18.8.9`](https://github.com/can1357/oh-my-pi/compare/v18.8.7...v18.8.9)
跨两个补丁位 tag：v18.8.7...v18.8.8 180 commit/173 文件，v18.8.8...v18.8.9 58 commit/124 文件
（compare API 合计 238 commit/267 文件）。18.8.8 变更面为 session dump/anonymize（`/dump anon`、
live subagent 状态与 message times）、compaction 阈值语义（plain 数值阈值改为以模型窗口为 base
折算、`f<tokens>` 固定模式）、`/jobs kill`、浏览器与 natives 修复；18.8.9 增加多路复用器 terminal
launch 与 `/fork pane|window|tab`（tmux/Zellij/Herdr/CMUX/Orca）、表格图表与 Tern composer
git 段，并修复 Responses 空 `.done` 丢弃流式工具参数的回归。仍逐个接缝核对。

逐字节未变（两个窗口的 compare 文件列表均不含这些路径）：`packages/ai/src/auth/` 全部文件
（`blocks`、`rotation`、`cascade`、`affinity`、`types`、`oauth`、`select`、`store`、`resets`、
`usage`、`auth-storage.ts`、`sqlite-credential-store.ts`）、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`extensibility/extensions/{lifecycle-mirror,runner,wrapper}.ts`、
`session/{session-storage,credential-pin,session-context,session-paths}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`。
因此扩展事件清单、`TokenRateMeter` 上游算法、认证存储与 credential pin、session 目录解析、
采集依赖的 launcher/参数解析全部未变（`shared-events.ts` 自 v18.8.0 起保持逐字节相同）。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/coding-agent/src/extensibility/extensions/types.ts`（v18.8.9）：`ExtensionUIContext`
  新增可选 `openTerminal(request)`。可选方法，本扩展不调用，`ctx.ui` 其余契约未变。
- `packages/coding-agent/src/subprocess/terminal-launch/*`（v18.8.9 新增目录，含 `herdr.ts`）：
  多路复用器 terminal launch 实现；Herdr pane 目标默认 `HERDR_PANE_ID`、tab 目标默认
  `HERDR_WORKSPACE_ID`，shell-input 需 `shellGrammar: "posix"`。仅 `/fork` 与
  `ctx.ui.openTerminal` 使用，本扩展不导入。
- `docs/extensions.md`（v18.8.9）：新增 "Launching terminal commands" 一节与 multiplexer
  taxonomy；事件清单未变，仍未新增/删除共享事件。
- `packages/coding-agent/src/session/session-manager.ts`：v18.8.8 仅新增 `loadedMalformedRecords`
  getter（`/dump anon` 用）；v18.8.9 把 fork/process-exit 的未决工具调用配对移到
  `session/exit-diagnostics.ts`（`createInterruptedToolResults(entries, context?)` 新增可选
  `context` 参数、新增 `sessionExitFollowsLastMessage`）。本扩展消费的 `getSessionId`/
  `getEntries`/`getSessionFile`/`getCredentialPins`/`appendResetBoundary`/`appendCustomEntry`
  签名与 session JSONL 行形状未变，实机探针复核全部为 function。
- `packages/coding-agent/src/session/{context-settings,session-advisors,session-maintenance,
  sub-sessions,session-dump-format,model-compaction-threshold}.ts` 与 `exit-diagnostics.ts`
  （v18.8.8）：dump/anonymize 上报、compaction 阈值折算、advisor `modelResolver`。
  均不经过本扩展；advisor 仍以 `{ kind: "sub", name: "advisor" }` 上报，子代理过滤不受影响。
- pi-tui 破坏性变更（v18.8.8/v18.8.9）：移除 `isInsideTmux()`/`isInsideZellij()`/
  `isInsideHerdr()` 导出（改用 `hasTerminalMultiplexerSession()`），
  `ModelHubCallbacks.onCompactionPointChange` 改收 `confirmed` 并返回
  `CompactionPointChangeResult`、`ModelCompactionPoint.percent` 改为 `basis`。只影响导入
  `@oh-my-pi/pi-tui` 的扩展；本扩展只导入 `@oh-my-pi/pi-agent-core`，18.8.4 上已验证可加载，
  不受影响。

bundle 接口出现次数 18.8.7 → 18.8.9（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`message_end` 64 → 64、`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、
`session_tree` 5 → 5 全部一致；消费接口本身未变。

实机验证（`omp-linux-x64` SHA256
`cacc12d5cdd46ff207f3452b36a0d0cfa7c5b8aee0d403aa83c5ab1f46bccb58`，与 release `SHA256SUMS.txt`
一致，`--version` → `omp/18.8.9`，由 `.runtime/omp` 直接执行 `omp update` 升级，下载即校验 sha256
后原地替换）：

- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.8.9 runtime。
- 经 `--mode rpc --no-session --no-ui` 加载临时探针：20 个事件名全部注册成功；
  `context.agent.kind === 'main'`、`hasUI === false`；`modelRegistry.authStorage` 上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 全部就位，
  `keys.getWithCredential`、`keys.get`、`sessions.release`、`limits.markReached`、`limits.rotate`、
  `credentials.list`、`credentials.onGeneration`、`blocks.list` 与 sessionManager 的
  `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`getBranch`/`appendResetBoundary`/
  `appendCustomEntry`/`onPersistenceNotice` 以及 `pi.appendEntry` 全部是 function；宿主
  `@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`；
  `omp-with-tps.mjs --print --model "opencode-go/deepseek-v4.1-flash" "say pong"` 在生产库落成
  `source=omp, provider=opencode-go, account_source=session_pin, account_route_reason=initial`，
  账号 `OpenCode Go Key fba810dd`，路由归因未漂移。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test --offline --locked`：ai-monitor 181 + 1 + 4、herdr-usage 116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`（pro2 daemons 仍共享）。
  以上是升级当时的验证记录；后续行为缺口与修复见「2026-10-11 行为复核与优化」。

### 18.8.7

upstream [`v18.8.6...v18.8.7`](https://github.com/can1357/oh-my-pi/compare/v18.8.6...v18.8.7)
是补丁位发布但变更集不是补丁规模：compare API 显示 190 个 commit、300 个文件
（261 modified / 37 added / 2 removed），变更面覆盖 ai（provider 适配、auth 选择、错误分类）、catalog
（新增 coralbricks provider、model-manager 合并）、coding-agent（自动标题管线重写、`omp update`
修复、browser relay、tts、ast/jq 工具）、tui、natives 与 crates。仍逐个接缝核对。

逐字节未变（两 tag raw 内容 `cmp` 相同，本版本核对 30 个接缝文件、其中 29 个相同）：
`packages/ai/src/auth/{blocks,rotation,cascade,affinity,types,oauth,store,resets,usage}.ts`、
`packages/ai/src/auth-storage.ts`、`packages/ai/src/auth/sqlite-credential-store.ts`、
`packages/ai/src/provider-session-state.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`extensibility/extensions/{types,lifecycle-mirror,runner,wrapper}.ts`、
`session/{session-storage,credential-pin,session-context,session-manager,session-paths}.ts`、
`packages/agent/src/tokenizer.ts`、`commands/launch.ts`、`cli/args.ts`、`docs/extensions.md`
（`packages/utils/src/dirs.ts` 只有 0 行内容变更）。因此扩展事件定义、`TokenRateMeter` 上游算法、
认证存储与 credential pin、session 目录/JSONL 形状、采集依赖的 launcher/参数解析全部未变。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/auth/select.ts`：OAuth 候选的 preflight 改为按 credential ID 捕获排名前的行
  （`credentialIds` 快照），索引对不上记为 preflight 失败；last-resort 的 blocked pass 把用户的
  显式 session pin 排到最早解封的 sibling 之前（修复 `/session pin` 在全部账号额度耗尽时被绕过）。
  只影响 OAuth 路径，`selectApiKey`（API-key 路径）不读 affinity，本地 sticky pin 语义不变；
  `CredentialRotation` 的 `{ switched }` 契约仍然不变（`rotation.ts` 逐字节相同）。
- `packages/ai/src/error/rate-limit.ts`：中文额度耗尽模式从 `使用…上限` 泛化为 `(使用|用量)…上限`，
  命中 MiniMax Token Plan 的 429（`当前已达到 Token Plan 用量上限`），使这类 429 轮换 sibling
  而不是当瞬时限流。只让分类更准；opencode-go 用 login/stored API key，观察器的
  `markReached`/`rotate` 记录语义不变。
- `packages/ai/src/providers/*`（Haiku 5.5 adaptive thinking、routing-session cleanup）与 catalog
  的 coralbricks provider：新接入与既有 provider 适配，本扩展不消费。
- `packages/coding-agent/src/session/agent-session.ts`（+403）：自动标题管线重写——标题在回复首个
  非 thinking 块时启动、`TitleInput` 取代 title fork 状态机、`/rename` 支持 title card。本扩展与
  采集不解析 `title_change` 行；`getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/
  `appendResetBoundary`/`appendCustomEntry` 签名与 session JSONL 行形状未变（`session-manager.ts`
  逐字节相同）。
- `packages/coding-agent/src/cli/update-cli.ts`：修复 `omp update` 忽略 `update.channel`
  （canary 用户此前按 stable 检查、`--canary`/`--stable` 不记忆），并让更新器跳过由 Tern 安装的
  omp。本仓库 `.runtime/omp` 由 `omp update` 就地维护，不受影响；更稳的 channel 记录对后续升级有利。

bundle 接口出现次数 18.8.6 → 18.8.7（`grep -ao` 对两份二进制分别计数）：
`getWithCredential` 10 → 10、`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、
`appendEntry` 19 → 19、`session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
`agent_end` 59 → 59、`session_branch` 8 → 8、`session_switch` 8 → 8、`session_tree` 5 → 5、
`tryAcquireSessionLease` 5 → 5、`getCredentialPins` 5 → 5、`getSessionFile` 91 → 91、
`before_provider_request` 4 → 4 全部一致；`message_end` 63 → 64、`getSessionId` 183 → 182
（runtime 内部调用点增减），消费接口本身未变。

实机验证（`omp-linux-x64` SHA256
`b87f9835a0acdbb81bbbad8273aa2d999b608a208598421a9584cffeb3139a8a`，`--version` → `omp/18.8.7`，
由 `.runtime/omp` 直接执行 `omp update` 升级，下载即校验 sha256 后原地替换）：

- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.8.7 runtime。
- 经 `--mode rpc --no-session --no-ui` 加载临时探针：20 个事件名全部注册成功；
  `context.agent.kind === 'main'`、`hasUI === false`；`modelRegistry.authStorage` 上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 全部就位，
  `keys.getWithCredential`、`keys.get`、`sessions.release`、`limits.markReached`、`limits.rotate`、
  `credentials.list`、`credentials.onGeneration`、`blocks.list` 与 sessionManager 的
  `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`getBranch`/`appendResetBoundary`/
  `appendCustomEntry`/`onPersistenceNotice` 以及 `pi.appendEntry` 全部是 function。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`；
  `omp-with-tps.mjs --print --model "opencode-go/deepseek-v4.1-flash" "say pong"` 在生产库落成
  `source=omp, provider=opencode-go, account_source=session_pin, account_route_reason=initial`，
  账号 `OpenCode Go Key b2e42461`，路由归因未漂移。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test --offline --locked`：ai-monitor 181 + 1 + 4、herdr-usage 116 + 23 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪；
  `share-omp-daemons.mjs --dry-run` → `status: already-shared`（pro2 daemons 仍共享）。
  本地 sticky pin、TPS reporter 及 herdr-usage 采集链路全部通过，扩展与采集代码无需修改。

### 18.8.4

upstream [`v18.8.3...v18.8.4`](https://github.com/can1357/oh-my-pi/compare/v18.8.3...v18.8.4)
共 17 个 PR / commit、81 个文件（主要在 auth-broker/gateway、usage CLI、Cursor/Anthropic 适配器与 Tern TUI）。

逐字节未变（两 tag raw 内容 `cmp` 相同）：
`packages/ai/src/auth/{blocks,affinity,rotation,cascade,select,oauth}.ts`、
`packages/ai/src/auth/auth-storage.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/types.ts`、
`extensions/{lifecycle-mirror,runner,wrapper}.ts`、
`packages/coding-agent/src/session/{session-manager,session-storage,credential-pin,session-context}.ts`、
`packages/agent/src/tokenizer.ts`、`docs/extensions.md`、
`packages/coding-agent/src/commands/launch.ts`、
`packages/coding-agent/src/cli/args.ts`。
因此事件定义与投递形状、TPS 分词与 `TokenRateMeter`、存储、session 目录解析与 credential pin 全部未变。

接缝与变更核对：

- `packages/ai/src/auth/store.ts`（`UsageLedgerStore`）、`resets.ts`、`usage.ts`：
  `invalidateUsageCache(provider?: string, signal?: AbortSignal)` 新增可选 `provider` 首参，
  使单 provider 刷新不再清除所有 provider 的缓存；本扩展未调用该接口，无影响。
- `packages/ai/src/provider-session-state.ts`：session-affinity pins 增加单 provider 最多 256
  个 session 的容量上限防泄漏；本扩展维护独立的本地 sticky pin，无影响。
- bundle 接口出现次数 18.8.3 → 18.8.4：`getWithCredential` 10 → 10、`markReached` 6 → 6、
  `onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`appendEntry` 19 → 19、
  `session_shutdown` 9 → 9、`session_start` 9 → 9、`message_update` 39 → 39、
  `message_end` 63 → 63、`agent_end` 59 → 59 全部逐项保持一致。

实机验证（`omp-linux-x64` SHA256
`b2dba223fbdae27acbd99be2f3e76ba10baae1768f9c57cee30c440f308dea4e`，`--version` → `omp/18.8.4`，
`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.8.4 runtime。
- `node integrations/herdr-tps/omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed。
- `cargo test -p herdr-usage --offline --locked`：116 + 23 + 3 passed。
- `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。
本地 sticky pin、TPS reporter 及 herdr-usage 采集链路全部通过，扩展与采集代码无需修改。

### 18.8.3

upstream [`v18.8.0...v18.8.3`](https://github.com/can1357/oh-my-pi/compare/v18.8.0...v18.8.3)
含 v18.8.1、v18.8.2、v18.8.3 三个 tag，共 61 个 commit、166 个文件，跨 ai/agent/catalog/coding-agent/
natives/tui/edit/vcs/utils/crates。版本号只到补丁位但变更集不是补丁规模，仍逐个接缝核对。

逐字节未变（两 tag raw 内容 `cmp` 相同；括号内为两 tag 相同的 blob SHA 前 8 位）：`packages/ai/src/auth/blocks.ts`
（`d390d8a0`）、`packages/coding-agent/src/utils/token-rate.ts`（`409f3dfa`）、
`packages/coding-agent/src/extensibility/shared-events.ts`（`6317bc3b`）、
`packages/coding-agent/src/extensibility/extensions/types.ts`（`02537394`）、
`extensions/{lifecycle-mirror,runner,wrapper}.ts`（`3184b9ab`/`a667b816`/`d8150f47`）、
`packages/coding-agent/src/session/credential-pin.ts`（`e0d90cc1`）、
`packages/coding-agent/src/session/session-context.ts`（`b7a274ab`）、
`packages/agent/src/tokenizer.ts`、`docs/extensions.md`、
`packages/coding-agent/src/session/session-storage.ts`、`packages/coding-agent/src/commands/launch.ts`、
`packages/coding-agent/src/cli/args.ts`、`packages/catalog/src/model-cache.ts`。
因此事件定义与投递形状（`docs/extensions.md` 两 tag 同为 1084 行、生命周期事件清单一致，没有新增
扩展事件）、TPS 分词与 `TokenRateMeter`、存储、session 目录解析与 credential pin 全部不变。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/auth/affinity.ts`（+82/-10）、`auth-storage.ts`（blob `80cc547b` → `64fdad01`，
  由 v18.8.1 的 `feat(task): restrict subagents to per-agent OAuth account pools` 引入）、`auth/types.ts`、
  `auth/sqlite-credential-store.ts`：新增 `SessionRestrictions` 与 `sessions.restrict`/
  `unrestrict`/`isRestricted`/`allows`，用于把子代理限制在各自的 OAuth 账号池（`restrict` 与
  `select.ts` 的 `affinity.allows` 均在 v18.8.1 首次出现，v18.8.0 无）。`AuthStorage` 持有
  `#restrictions`，`replaceStore` 时随 store 一起迁移（因此限制能跨存储替换存活），身份键由
  `resolveCredentialIdentityKey` 计算。未安装 restriction 的 session 上 `allows()` 恒为真。
- `packages/ai/src/auth/cascade.ts`、`auth/select.ts`、`auth/oauth.ts`、`error/auth.ts`：
  request 路径 `getWithCredential` 在 OAuth 刷新出现可重试失败（网络/超时/5xx）且没有其他来源能
  服务时抛 `OAuthRefreshUnavailableError`（该符号 v18.8.1、v18.8.2 均无，仅 v18.8.3 引入），
  可用性探测 `keys.get` 吞掉它返回 `undefined`；
  `selectApiKey`（API-key 路径）不读 affinity，因此本地 sticky pin 仍必须由扩展维护。
  受限 session 会跳过 runtime override，并在 models.yml 为该 provider 配了 `apiKey` 时按
  fail-closed 抛 `MissingApiKeyError`。
- `packages/ai/src/auth/rotation.ts`（+8/-2）：rotate 与 sibling 选择新增 `sessionId` 参数并追加
  `affinity.allows()` 判断。`CredentialRotation`（`{ switched, afterSiblingWait? }`）形状不变。
- `packages/coding-agent/src/session/session-manager.ts`：v18.8.0 里就存在的 title card 管线
  （`setSessionName` 的 `card` 形参、`getSessionTitleCard`、title-change entry 的 `card` 字段）
  在 v18.8.1 被整体移除（v18.8.1、v18.8.2、v18.8.3 三个 tag 均已无这些符号），净效果与 18.8.0
  对本扩展而言一致。本扩展消费的 `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/
  `appendResetBoundary`/`appendCustomEntry` 签名与 session JSONL 行形状不变；采集侧不解析
  title_change 行。
- `packages/coding-agent/src/config/model-registry.ts`、`config/account-pools.ts`、
  `task/*`、`session/agent-session*.ts`：账号池限制落到 task executor、vibe worker 与
  persisted-revive；`drainConfigWarnings()` 上报 models.yml 未知 compat key。
  `context.modelRegistry.authStorage` 取法不变。
- `packages/utils/src/dirs.ts`：`resolveWorktreeBase` 泛化为 `resolveAbsoluteDir`，并新增
  `PI_NATIVES_DIR` 覆盖原生插件解压目录（相对路径忽略）；默认 `~/.omp/natives` 不变。
- 其余为 catalog 计价与模型目录（Codex Fast 2.5x 倍率、Codex service tier 回显、Copilot 档位
  prompt 限制、Claude Haiku 5.5）、TUI（ask 对话框停靠输入槽、模型选择器去重）、Tern PiP、
  `brush-core` 后台 builtin 持久化与 `kill %N`、`PI_NATIVES_DIR` 文档等，均不触及本扩展接缝。

bundle 接口出现次数 18.8.0 → 18.8.3（同一方法对两份二进制分别计数）：`getWithCredential` 10 → 10、
`markReached` 6 → 6、`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`appendEntry` 19 → 19、
`getSessionId` 181 → 181（此处计数方法为对两份二进制 `grep -ao`，与旧小节的数字口径略有差异，
只看是否变化）、`session_shutdown` 9 → 9、`session_start` 9 → 9、`session_tree` 5 → 5、
`tryAcquireSessionLease` 5 → 5、`getSessionOwnersDir` 2 → 2、`session_branch` 8 → 8、
`session_switch` 8 → 8、`cancel_subagent` 8 → 8、`steer_subagent` 14 → 14、
`predict_word` 15 → 15、`message_update` 39 → 39、`message_end` 63 → 63、`agent_end` 59 → 59、
`before_provider_request` 4 → 4。全部核心接缝逐个保持一致。

实机验证（`omp-linux-x64` SHA256
`8cb6d6a0035a3c5d9c40a58852ca361569eafc2cecbbeef4521e0fefc8ccbc37`，`--version` → `omp/18.8.3`，
`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.8.3 runtime。
- 经 `--mode rpc --no-session --no-ui` 加载临时探针，runtime 发出 `ready`（protocol v1，支持
  v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）；`context.agent.kind === 'main'`、
  `hasUI === false`；20 个事件名全部注册成功；`authStorage` 上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 全部就位，
  `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`、
  `credentials.list`、`credentials.onGeneration`、`blocks.list` 与 sessionManager 的
  `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`getBranch`/`appendResetBoundary`/
  `appendCustomEntry`/`onPersistenceNotice`、`pi.appendEntry` 全部是 function。
- `omp-with-tps.mjs --print "say pong"` 真实请求正常返回 `pong`；usage.db 中 omp 行仍是
  `account_source=session_pin`、`account_route_reason=initial`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test -p herdr-usage --offline --locked`：116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。
  本地 sticky pin、TPS reporter 及 herdr-usage 采集链路全部通过，扩展与采集代码无需修改。

### 18.8.0

upstream [`v18.7.0...v18.8.0`](https://github.com/can1357/oh-my-pi/compare/v18.7.0...v18.8.0)
是补丁版本发布，逐个接缝核对。

逐字节未变（两 tag 相同 blob SHA）：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,blocks,affinity}.ts`、`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/{lifecycle-mirror,runner,wrapper}.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin}.ts`、
`packages/coding-agent/src/commands/launch.ts`、`packages/coding-agent/src/cli/args.ts`、
`packages/catalog/src/model-cache.ts`、`packages/utils/src/dirs.ts`。
因此认证底层、存储、rotation 合约、session affinity 行为与事件定义均未改变；本地 session 粘性补丁继续完全适用。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/`：环境 API-key 辅助函数（`getEnvApiKey`、`getEnvApiKeyName`、
  `listProvidersWithEnvKey`）从 `@oh-my-pi/pi-ai/stream` 移出，改为从 `@oh-my-pi/pi-ai` 或
  `@oh-my-pi/pi-ai/env-api-key` 导入；`TranscriptionRequest.audio` 接受 `Uint8Array | Blob`。
  本扩展不从 `pi-ai/stream` 导入任何符号，也不消费转录请求，因此不受影响。
- `packages/agent/src/tokenizer.ts`：`countTokens` 核心接口与文本分词逻辑未变。
- `packages/coding-agent/src/session/session-manager.ts`：本扩展消费的
  `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`appendResetBoundary`
  签名与 session JSONL 行形状不变。
- `packages/coding-agent/src/extensibility/extensions/types.ts` 与 `docs/extensions.md`：
  未新增扩展生命周期事件，本扩展消费的 7 个事件投递路径与字段形状不变。

bundle 接口出现次数 18.7.0 → 18.8.0：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`session_shutdown` 9 → 9、
`appendEntry` 19 → 19、`getSessionId` 174 → 177、`session_tree` 5 → 5、
`tryAcquireSessionLease` 5 → 5、`getSessionOwnersDir` 2 → 2、`session_start` 9 → 9、
`cancel_subagent` 8 → 8、`steer_subagent` 14 → 14、`predict_word` 15 → 15、
`message_update` 37 → 39、`agent_end` 57 → 58、`before_provider_request` 3 → 4、
`session_branch` 8 → 8。全部核心接缝保持一致或仅内部使用增加。

实机验证（`omp-linux-x64` SHA256
`6d0bd5d624f96b42513859558bc001d523a1eada1d71446ab7ca743ec2aa7de1`，`--version` → `omp/18.8.0`，
`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --version` 加载 wrapper，正确解析到 18.8.0 runtime。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。
  本地 sticky pin、TPS reporter 及 herdr-usage 采集链路全部通过，无需修改代码。

### 18.7.0

upstream [`v18.6.1...v18.7.0`](https://github.com/can1357/oh-my-pi/compare/v18.6.1...v18.7.0)
是 853 个 commit、869 个文件改动（含中间 tag 18.6.2、18.6.3），跨 ai/agent/catalog/coding-agent/natives/builtins/tui/edit/vcs/vfs。次版本发布规模大，逐个接缝核对。

逐字节未变（两 tag 相同 blob SHA）：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,blocks,affinity}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/{lifecycle-mirror,runner,wrapper}.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin}.ts`、
`packages/coding-agent/src/commands/launch.ts`、`packages/coding-agent/src/cli/args.ts`、
`packages/catalog/src/model-cache.ts`、`packages/utils/src/dirs.ts`。
因此认证底层、存储、rotation 合约、session affinity 行为与事件定义均未改变；本地 session 粘性补丁继续完全适用。

被改动但不破坏本扩展/采集契约的接缝：

- `packages/ai/src/auth/cascade.ts` 与 `packages/ai/src/auth/select.ts`：在 `apiKeySelection`、`loginApiKeySelection` 及 OAuth 选择后记录 affinity 时新增 `recordAffinity !== false` 条件判断；`affinity.ts` 逻辑未变且 API key 选择逻辑依然不读取 session affinity，因此本地 sticky pin 仍然必须由扩展管理。
- `packages/agent/src/tokenizer.ts`：构造器入参扩展为可选 `snapcompact.ShapeTarget`，对 snapcompact 消息帧 token 估算由固定值改为按像素尺寸与定价动态估算（`snapcompact.frameTokens`）；`countTokens` 核心接口与文本分词保持一致。
- `packages/coding-agent/src/session/session-manager.ts`：`appendSessionInit` 的 `systemPrompt` 类型由 `string` 扩展为 `string[]` 并增加 `workPoolYieldItems`，`extractSessionInit` 兼容两者；本扩展消费的 `getSessionId`/`getEntries`/`getSessionFile`/`getCredentialPins`/`appendResetBoundary` 均保持不变。
- `packages/coding-agent/src/extensibility/extensions/types.ts` 与 `docs/extensions.md`：`registerProvider` 的 `models` 支持指定 `kind`（如 `openai-images` 对应 `kind: "image"`），未新增扩展生命周期事件。
- `packages/coding-agent/src/tools/read.ts` 与 `crates/pi-builtins/src/read.rs`：`read` 工具原生支持 `?q=<jq-filter>` 进行 JSON/JSONL 查询、分页与流式过滤（PR #14141）。
- `packages/coding-agent/src/tools/{write,edit}.ts`：禁止对 FIFO、设备文件、终端等特殊文件进行写/改操作，遇到非普通文件安全报错而非挂起或耗尽资源（PR #14452、#14462）。
- `crates/pi-natives/src/{sed,jq}.rs`：放弃输入文件 mmap 改为常规读取，彻底解决被外部截断导致的 SIGBUS（PR #14613）。
- `crates/pi-natives/src/tail.rs`：修复文件大小恰为 64 KiB 整倍数时首部数据遗漏的 bug（PR #14264）；修复 macOS 上 `tail -f` 管道下游提前退出时未停止的问题（PR #14617）。
- `crates/pi-builtins/src/sort.rs`：修复 UTF-8 语言环境下 `sort -u` 误丢弃标点相异路径的问题（PR #14606）。
- `crates/pi-natives/src/svg.rs` 与 `packages/tui/`：支持终端行内渲染主题自适应 SVG 图表（PR #14529），数值表格自动生成图表（`tui.autoGraph`）。
- `packages/coding-agent/src/session/session-context.ts`：修复 `.` 与 `..` 等特殊 session ID 可能越界本地存储目录的问题（PR #14385）。
- `packages/coding-agent/src/cli/args.ts`：`--resume <path>` 在路径不存在时显式报错，不再静默退回创建新 session（PR #14404）。
- `packages/catalog/`：新增 Mistral Large 4 支持、Google Antigravity Claude Opus 5.5 / Sonnet 5.5 价格与思考层级、MiniMax Token Plan 转 Anthropic 兼容端点、GPT-6 Astra Ultrafast 6x/8x 计费修复。

bundle 接口出现次数 18.6.1 → 18.7.0：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`session_shutdown` 9 → 9、
`appendEntry` 19 → 19、`getSessionId` 174 → 177、`session_tree` 5 → 5、
`tryAcquireSessionLease` 5 → 5、`getSessionOwnersDir` 2 → 2、`session_start` 9 → 9、
`cancel_subagent` 8 → 8、`steer_subagent` 14 → 14、`predict_word` 15 → 15、
`message_update` 37 → 37、`session_branch` 8 → 8。全部核心接缝保持一致或仅内部使用增加。

实机验证（`omp-linux-x64` SHA256
`03387aceae62386eaefbe96786f6df0e2e2e843ce76926c063a485905b1f4c4f`，`--version` → `omp/18.7.0`，
`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载探针，runtime 正常发出 `ready`
  （protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）；
  `context.agent.kind === 'main'`、`hasUI === false`；`authStorage` 原型及 `sessionManager` 核心方法均在位。
- `omp --print "say pong"` 真实请求正常完成并返回 `pong`。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test -p herdr-usage --locked`：116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。
  本地 sticky pin、TPS reporter 及 herdr-usage 采集链路全部通过，无需修改代码。

### 18.6.1

upstream [`v18.6.0...v18.6.1`](https://github.com/can1357/oh-my-pi/compare/v18.6.0...v18.6.1)
是 42 个 commit、69 个文件改动，跨 ai/agent/coding-agent/natives/tui/edit/wire。属于补丁版本发布，逐个接缝核对。

逐字节未变（两 tag 相同 blob SHA）：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,select,blocks,cascade,affinity}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/lifecycle-mirror.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin}.ts`、
`packages/coding-agent/src/commands/launch.ts`、`packages/coding-agent/src/cli/args.ts`、
`packages/catalog/src/model-cache.ts`、`packages/utils/src/dirs.ts`。
因此认证、存储、rotation 合约、session affinity 行为与事件定义均未改变；本地 session 粘性补丁继续完全适用。

被改动但不影响本扩展/采集契约的接缝：

- `crates/pi-edit/src/modes/hashline/patcher.rs`：新增 `rescue_shifted_anchors` 救援机制（PR #14254）。当前序编辑在上方插入/删除行导致后续行号平移时，若补丁载荷包含了锚点行在当前文件中的内容（首尾 token-bounded 且不产生重复行歧义），允许后续编辑按平移后的行号应用，解决连续 patch 因行号平移误报“never displayed”而被拒的问题，同时依然拒绝真正陈旧的行号引用。
- `packages/agent/src/tokenizer.ts` 与 `packages/agent/src/image-tokens.ts`：将消息分词中固定 1200 的图片 token 估算改为基于尺寸计算的 `estimateImageContentTokens(block)`（PR #14260），解决含多张截图的 session 在 OpenAI 原生 context compaction 时因尺寸高估导致压缩请求被拒的问题；`countTokens` 核心接口及文本分词逻辑完全不变。
- `packages/ai/src/providers/openai-shared.ts`：新增 `withRequiredReasoningText`，为 DeepSeek 等要求每轮必须携带 reasoning 的模型在 warm OpenAI Responses 会话回放时自动补齐缺失的 `reasoning_text`（PR #14294）。
- `packages/ai/src/dialect/deepseek.ts` 与 `packages/ai/src/utils/stream-markup-healing.ts`：保留不完整或未闭合的 `<｜DSML｜tool_calls>` 包裹文本输出（PR #14273），避免吞掉后续正文，交由 agent 层 leak recovery 机制恢复。
- `crates/pi-natives/src/{task,ast,glob,grep}.rs`：将文件系统搜索（`ast_grep`、`ast_edit`、`glob`、`grep`）从 libuv worker 线程池移到 Tokio 专用文件系统任务池（`task::filesystem`），避免并发 `skill://` 搜索阻塞其他异步文件 I/O 或拖慢子代理产物发布；并用 `catch_worker_panic` 捕获工作线程 panic（PR #14298）。
- `packages/coding-agent/src/task/executor.ts`：子代理 IRC wake turn 的输出作为异步 background job 投递，支持被父代理等待与取消，由父代理消息唤醒的 turn 注册跨越整个 turn 的 job（不再重复投递）。
- `packages/coding-agent/src/exec/exec.ts`：修复被超时或信号强杀的进程误报退出码 `0` 的问题，被杀进程统一上报退出码 `-1` 且 `killed: true`（PR #14290）。
- `packages/coding-agent/src/slash-commands/builtin-completions.ts`：`/switch` 命令补全与模型选择器保持一致的相关性排序，支持 `@role` 别名。
- `packages/coding-agent/src/session/{agent-session-types,agent-session,messages}.ts`：`/btw` 临时响应支持 `replyMaxBytes`（`Infinity` 保留长回答，不再硬编码 4 KiB 截断）。
- `packages/coding-agent/src/tools/browser/relay/*`：增加对升级后陈旧 browser relay / connection refused 的诊断与清理提示（PR #14299）。
- `packages/tui/src/apps/git/diff-pane.ts`：修复 Git diff 面板中空格键对选中 hunk 的暂存/取消暂存（PR #14329）；Tern 模式下 BTW 历史面板支持独立滚动。

bundle 接口出现次数 18.6.0 → 18.6.1：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`session_shutdown` 9 → 9、
`appendEntry` 19 → 19、`getSessionId` 174 → 174、`session_tree` 5 → 5、
`tryAcquireSessionLease` 5 → 5、`getSessionOwnersDir` 2 → 2、`session_start` 9 → 9、
`cancel_subagent` 8 → 8、`steer_subagent` 14 → 14、`predict_word` 15 → 15、
`message_update` 37 → 37、`session_branch` 8 → 8。全部保持一致。

实机验证（`omp-linux-x64` SHA256
`c92a6846d02984e84f07c6362d18add3783f528f494ffcf1e0f26e594f327463`，`--version` → `omp/18.6.1`，
`omp update --check` → Already up to date，`.herdr-omp-original` 与仓库 wrapper 两条解析路径各测一次）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载临时探针，runtime 发出
  `ready`（protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）；
  `context.agent.kind === 'main'`、`hasUI === false`；`modelRegistry.authStorage` 原型上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 全部就位；
  `sessionManager` 与 `pi` 核心接口全部是 function。
- 20 个事件名全部注册成功（含本扩展消费的 7 个及 `session_branch`、`session_tree` 等）。
- `omp --print` 真实请求成功完成（`opencode-go/deepseek-v4.1-flash`），assistant 行与 usage 记录完整。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test -p herdr-usage --offline --locked`：116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。
  扩展与采集代码不需要修改。

### 18.6.0

upstream [`v18.5.0...v18.6.0`](https://github.com/can1357/oh-my-pi/compare/v18.5.0...v18.6.0)
是 448 个 commit、516 个文件改动加 17 个删除，跨 ai/agent/catalog/coding-agent/browser-relay/crates，
其中含同日发布的 18.5.1 与 18.6.0 两个 tag。变更集不是补丁规模，因此仍逐个接缝核对。

逐字节未变（两 tag 相同 blob SHA）：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,select,cascade,affinity}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、`packages/agent/src/tokenizer.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/lifecycle-mirror.ts`、
`packages/coding-agent/src/session/{session-storage,credential-pin,agent-session-types}.ts`、
`packages/coding-agent/src/commands/launch.ts`、`packages/coding-agent/src/cli/args.ts`、
`packages/catalog/src/model-cache.ts`、`packages/utils/src/dirs.ts`。
因此 18.5.0 记录的“写了 session affinity 但选择路径不读它”继续成立，本地 session 粘性仍然必要；
`rotation.ts` 未变说明 `limits.rotate` 的 `{ switched }` 契约到本版为止没有再变；`shared-events.ts`
未变说明本版本没有新增扩展事件（`docs/extensions.md` 只多了一句 advisor 身份的说明）。

被改过但语义不变的接缝：

- `packages/ai/src/auth/blocks.ts`：镜像进 store 的 block 多一条判断——持久化行的截止时间短于
  本进程镜像过的截止时间时，说明该行已被 sibling 删除重建，本进程的旧 map 项也一并退休。
  `blocks.list` 与本扩展消费的 block/limit 释放路径形状不变，效果只是少留一个陈旧 backoff 项。
- `extensions/runner.ts` 与 `extensions/wrapper.ts`：`createContext`、`emitToolCall`、
  `emitToolResult` 增加可选的 `ExtensionAgentIdentity` 参数；advisor 复用主 session 的 runner
  时用它把 `ctx.agent` 换成 `{ kind: "sub", name: "advisor" }`。本扩展按
  `ctx.agent.kind === "main"` 拒绝子代理，advisor 的 `tool_call`/`tool_result` 不再冒充主 agent
  （本扩展本来就不注册这两个事件）。runner 另在主 agent 的 `session_start`/`session_switch`/
  `session_branch` 之后调用 `refreshShellConfigCache()`，让扩展注入的环境跟随 session 切换；
  本扩展不写 `process.env`，无影响。
- `session/session-manager.ts`：`repairMissingUsage` 换成 `normalizeLoadedUsage`（顺带补全残缺
  的 usage 字段，不再只是补整块缺失）；release 时清空 `#inMemoryArtifacts`，
  `allocateArtifactPath`/`saveArtifact` 在 release 后直接拒绝。本扩展读取的 `getSessionId`/
  `getEntries`/`getSessionFile`/`getCredentialPins`/`appendResetBoundary`/`appendCustomEntry`
  签名与 session JSONL 行形状不变。
- `session/agent-session.ts`：compaction 的 `deferredHandoff` 并入 `continuationScheduled`，
  `agent_end` 的 `willContinue` 仍只在已排定续轮时为真；`ActiveAgentContinue` 增加 `turnEnded`，
  由本轮 `agent_end` 发起的续接不再被并进已结束的那一轮；延迟自动标题改为累计 40 词后才改写；
  notice entry 增加 `details`；`waitForManualCompactionCleanup` 改名
  `waitForManualMaintenanceCleanup`（同时覆盖 `/handoff`）。本扩展读取的 `message_end`
  （`usage.output`/`duration`/`timestamp`/`content`）与 `agent_end` 形状不变。
- `session/session-paths.ts`：新增只读的 `sessionDirForCwd`，供 `archive` eval prelude 查询项目而
  不创建或迁移 session 目录。目录命名规则不变。
- `session/history-storage.ts`：prompt history 支持按 cwd/session_id 过滤，并新增写入失败监听。
- `modes/rpc/rpc-mode.ts`：新增 `live_start`/`live_stop`/`live_mute`（GPT 实时语音），`live_start`
  归入后台命令，断连时先 `liveBridge.stop()` 再关 session。既有命令与帧格式不变。

bundle 接口出现次数 18.5.0 → 18.6.0：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`session_shutdown` 9 → 9、
`appendEntry` 19 → 19、`getSessionId` 174 → 174、`session_tree` 5 → 5、
`tryAcquireSessionLease` 5 → 5、`getSessionOwnersDir` 2 → 2；`session_start` 8 → 9、
`cancel_subagent` 7 → 8、`steer_subagent` 13 → 14、`predict_word` 13 → 15、
`message_update` 33 → 37、`session_branch` 7 → 8。增长全部来自 live/RPC 与 advisor 相关代码，
没有删改本扩展依赖的任何接口。

实机验证（`omp-linux-x64` SHA256
`844c640c620ec4c152abe4b3736049f963205da50e9e77bd9bdf2a981a6ead55` 与 release
`SHA256SUMS.txt` 一致，`--version` → `omp/18.6.0`，`omp update --check` → Already up to date，
`.herdr-omp-original` 与仓库 wrapper 两条解析路径各测一次）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载临时探针，runtime 发出
  `ready`（protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）；
  `context.agent.kind === 'main'`、`hasUI === false`；`modelRegistry.authStorage` 原型上是
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks`
  （另有 `close`/`getApiKey`/`reload`/`replaceStore`/`setAccountPolicies`），
  `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`、
  `credentials.list`、`credentials.onGeneration`、`blocks.list`、`sessionManager.getSessionId`/
  `getEntries`/`getSessionFile`/`getCredentialPins`/`getBranch`/`onPersistenceNotice`/
  `appendResetBoundary`/`appendCustomEntry`、`pi.appendEntry`/`sendMessage` 全部是 function。
- 20 个事件名全部注册成功：本扩展消费的 7 个（`session_start`、`session_switch`、
  `session_shutdown`、`message_start`、`message_update`、`message_end`、`agent_end`）、
  `turn_start`、`turn_end`、`agent_start`、`queue_update`、`assistant_message`、
  `cache_warming_start`、`cache_warming_end`、`before_provider_request`、`tool_result`、
  `session_before_branch`、`session_branch`、`session_tree`，以及本扩展不消费的 `goal_updated`。
- `omp --print` 真实请求成功完成；`opencode-go/deepseek-v4.1-flash` 的真实 session 在生产库落成
  `source=omp, provider=opencode-go, account_source=session_pin, account_route_reason=initial`，
  解析与回填链路不变。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test -p herdr-usage --offline --locked`：116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。
  扩展与采集代码不需要修改。

### 18.5.0

upstream [`v18.4.12...v18.5.0`](https://github.com/can1357/oh-my-pi/compare/v18.4.12...v18.5.0)
是 67 个 commit、292 个文件（modified/added 混合，新增 `report-panel.ts`、`sub-sessions.ts`、
`agent-tombstone.ts`、`effort.ts`、pi-vcs `filter.rs`，删除 `task-completion-probe.test.ts`）。
版本号从 18.4.x 跳到 18.5.0，但变更集不是补丁规模，因此仍逐个接缝核对。

逐字节未变（两 tag 相同 blob SHA）：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,select,blocks,cascade,affinity}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、`packages/agent/src/tokenizer.ts`、
`packages/coding-agent/src/extensibility/extensions/{runner,wrapper}.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/extensibility/extensions/lifecycle-mirror.ts`、
`packages/coding-agent/src/session/{credential-pin,agent-session-types}.ts`、
`packages/coding-agent/src/commands/launch.ts`、`packages/coding-agent/src/cli/{args,main}.ts`
与 `docs/extensions.md`。因此 18.4.8 记录的“写了 session affinity 但选择路径不读它”继续成立，
本地 session 粘性仍然必要；`shared-events.ts` 与 `docs/extensions.md` 未变说明本版本没有新增扩展
事件，上表已消费事件的投递路径与字段形状不变。

被改过但语义不变的接缝：

- **session 所有权租约换了位置与键（本版本唯一的 session 存储破坏性变更）**：
  `packages/coding-agent/src/session/session-storage.ts` 用
  `claimSession(sessionId, sessionPath)` 取代 `claimSessionFile(sessionPath)`，
  `tryAcquireSessionLease(sessionId)` 取代 `sessionOwnerLeasePath(sessionPath)`；租约从
  session 文件旁的 `.{basename}.jsonl.owner` 移到与 profile 无关的
  `~/.omp/run/session-owners/<sessionId>`（Linux 用 abstract socket，不落文件；本机该目录至今未生成），
  并且 claim 会先读 journal header 的 session id，路径上已是别的 session 就拒绝认领。
  `session-manager.ts` 相应把 `#claimSessionFile`/`#sessionFileOwnedElsewhere` 改名为
  `#claimSession`/`#sessionOwnedElsewhere`，新增 `SessionMoveRefusedError`——`moveTo` 现在可能
  拒绝移动且不动任何文件，重开路径捕获该错误后按“项目没搬动”继续。
  本扩展只用 `getSessionId`/`getEntries`/`getSessionFile`/`appendCustomEntry`/
  `appendResetBoundary`/`getCredentialPins`，全部签名与 JSONL 行形状不变；本仓库也不实现
  `SessionStorage` 后端，因此没有需要迁移的实现。
- `packages/coding-agent/src/session/session-paths.ts`：temp root 优先于 home 判定，同一个 cwd
  同时落在两者之内时改用 `-tmp-…` 目录并把被遮蔽的 home 目录迁移过去。本机 `TMPDIR` 未设置、
  `/tmp` 不在 `$HOME` 之下，session 目录名不变（实测仍是 `-project-ai-monitor`）。
- `packages/coding-agent/src/session/agent-session.ts` 只增不删：新增
  `getAvailableEffortSelectors()` 与 `/dump all` 的 `dumpSessionArchiveToTmpDir()`，
  `dumpLlmRequestToTmpDir()` 抽出 `#formatLlmRequestJson`。本扩展读取的
  `message_end`（`usage.output`/`duration`/`timestamp`/`content`）与 `agent_end` 未变。
- `packages/catalog/src/model-cache.ts` 只新增 `closeModelCache()`；模型身份与显示名形状不变。
- `packages/coding-agent/src/modes/rpc/rpc-mode.ts` 只在 Windows 把 stdout 换成 fd 写流，
  Linux 分支与协议帧完全不变。
- `packages/coding-agent/src/extensibility/plugins/legacy-pi-compat.ts` 让 `?mtime=` 裸路径
  specifier 在 Windows 也生效；POSIX 上原来的分支就是裸路径，行为不变。
- `history-storage.ts` 的 `prepare()` 改 `query()`、`indexed-session-storage.ts` 的
  `listFilesSync` 改用 `directChildKeyName`：都只修 Windows 的文件锁与 `\` 分隔符列举。
- `packages/utils/src/dirs.ts` 新增 `getSessionOwnersDir()`；`registry/agent-tombstone.ts`
  是把 tombstone 路径从 `agent-registry.ts` 拆出，纯重构。

**设置迁移**：`task.completionProbeMs`（毫秒）被 on/off 的 `task.completionProbe` 取代，
旧值自动迁移（`0` → 关，其余 → 开）。本机 `~/.omp/agent/config.yml` 从未设置该键，实跑后
配置文件未被改写，因此无需处理。

bundle 接口出现次数 18.4.9 → 18.5.0：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`session_start` 8 → 8、
`session_shutdown` 9 → 9、`appendEntry` 19 → 19、`cancel_subagent` 7 → 7、
`steer_subagent` 13 → 13、`predict_word` 13 → 13；`message_update` 32 → 33、
`getSessionId` 171 → 174、`session_branch` 7、`session_tree` 5，新增 `tryAcquireSessionLease` 5、
`getSessionOwnersDir` 2。`claimSessionFile`/`sessionOwnerLeasePath` 只剩 CHANGELOG 文本里的迁移说明。

实机验证（`omp-linux-x64` SHA256
`b8413e6e085a423c7a9b1d7bc025c2af794923b6b6c57330a309639b6176c8a1` 与 release
`SHA256SUMS.txt` 一致，`--version` → `omp/18.5.0`，`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载临时探针，runtime 发出
  `ready`（protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）；
  `context.agent.kind === 'main'`、`hasUI === false`；`modelRegistry.authStorage` 上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 都在原型上，
  `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`、
  `credentials.list`、`credentials.onGeneration`、`blocks.list`、`sessionManager.getSessionId`/
  `getEntries`/`getSessionFile`/`getCredentialPins`/`getBranch`/`onPersistenceNotice`/
  `appendResetBoundary`/`appendCustomEntry`、`pi.appendEntry`/`sendMessage` 全部是 function。
- 20 个事件名全部注册成功：本扩展消费的 7 个（`session_start`、`session_switch`、
  `session_shutdown`、`message_start`、`message_update`、`message_end`、`agent_end`）、
  `turn_start`、`turn_end`、`agent_start`、`queue_update`、`assistant_message`、
  `cache_warming_start`、`cache_warming_end`、`before_provider_request`、`tool_result`、
  `session_before_branch`、`session_branch`、`session_tree`，以及本扩展不消费的 `goal_updated`。
- `omp --print` 真实请求成功完成；`opencode-go/deepseek-v4.1-flash` 的真实 session 写入
  `herdr-api-key-sticky-v1`（`credentialId` 7、`reason` initial），collector 在生产库里落成一行
  `source=omp, provider=opencode-go, account_route_reason=initial`，解析与回填链路不变。
- `node --test integrations/herdr-tps/test/*.test.mjs`：120 passed；
  `cargo test -p herdr-usage --locked`：116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。
  扩展与采集代码不需要修改。

### 18.4.12

upstream [`v18.4.10...v18.4.12`](https://github.com/can1357/oh-my-pi/compare/v18.4.10...v18.4.12)
是 130 个 commit、172 个文件（147 modified、25 added），跨 ai/catalog/coding-agent/tui/utils/wire，
因此同样逐个接缝核对。

核心接缝逐字节未变（与 18.4.10 相同的 blob SHA256）：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,select,blocks,cascade,affinity}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、`packages/agent/src/tokenizer.ts`、
`packages/coding-agent/src/extensibility/extensions/runner.ts` 与 `wrapper.ts`、
`packages/coding-agent/src/session/{session-storage,session-paths,credential-pin,agent-session-types}.ts`、
`packages/coding-agent/src/commands/launch.ts`、`packages/coding-agent/src/cli/main.ts`、
`packages/catalog/src/model-cache.ts`。`auth/select.ts` 与 `auth/affinity.ts` 未变说明
18.4.10 记录的“写了 session affinity 但选择路径不读它”继续成立，本地 session 粘性仍然必要；
`rotation.ts` 未变说明 `limits.rotate` 的 `{ switched }` 契约到本版为止没有再变。

被改动但语义不变的接缝：

- `packages/coding-agent/src/extensibility/shared-events.ts` 只给 `session_before_branch` /
  `session_branch` 增加 `reason`（`"branch"`/`"fork"`/`"btw"`）；`docs/extensions.md` 的改动
  也只有这一条。本扩展不注册这两个事件，因此期间没有新增本扩展要消费的扩展事件；
- 新增 `packages/coding-agent/src/extensibility/extensions/lifecycle-mirror.ts`，把
  `agent-session.ts` 里原先内联的“session 事件 → 扩展事件”映射抽出来。**仅当
  `#emitExtensionEvent` 的 `hasHandlers` 门禁已通过时才构造事件**（冒烟已确认），所以
  `message_update` 未注册时不再为它构造 payload；这与 18.4.10 的行为一致（当时是内联的
  per-case 提前返回）。映射出的字段名与形状未变，唯一例外是把 `cloneMessageNotification`
  应用到 `message_end` 的 `message`——18.4.10 已在路径上有
  `cloneMessageEndNotification`（逐字节相同的实现），只是 collab guest 现在也走同一条路。
  本扩展的 `message_end` 只读 `usage.output`、`duration`、`timestamp`、`content`，
  只读 detach 的深拷贝不改变其中任何一个值；`agent_end` 的 `willContinue` 仍是
  `isTerminal === false ? true : undefined`，语义同样不变。
- RPC 侧新加订阅门禁：只有在订阅级别为 `"events"` 时才挂 `TASK_SUBAGENT_EVENT_CHANNEL` 回调，
  未订阅的子代理事件不再被转发处理。本扩展不消费子代理事件，行为无差异。
- `session-manager.ts` 的 `createBranchedSession` 增加可选 `copyArtifacts`，默认值不变；
  `getSessionId`/`getEntries`/`getSessionFile`/`appendEntry`/`appendCustomEntry`/
  `appendResetBoundary`/`rewriteEntries` 的形状与 session JSONL 行形状都不变。
- 18.4.11 改动了本机不接的 auth-broker 路径：`packages/ai/src/auth-broker/remote-store.ts`
  把 credential refresh 的重复代码收进 `#refreshThroughBroker`，并新增分支——若 refresh 期间
  本地副本已变化且与返回行不一致，则以 broker 当前行为准（注销保持注销，较新的 login 保留）。
  本机未配置 `auth.broker`（opencode-go 与 Antigravity 都走原生 AuthStorage），因此不影响本机
  credentials 路径；启用 broker 时最坏情况是多一次额度 snapshot 刷新。

实机验证（`omp-linux-x64` SHA256
`8178466631d09c2165c19c14c92ee7f4e3e68c5f41953e8815adfb1214a64999` 与 release
`SHA256SUMS.txt` 一致，`--version` → `omp/18.4.12`，`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载临时探针，runtime 发出
  `ready`（protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）；
  `context.agent.kind === 'main'`、`hasUI === false`；`modelRegistry.authStorage` 上
  `credentials`/`keys`/`oauth`/`sessions`/`usage`/`health`/`limits`/`resets`/`blocks` 都在原型上，
  `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`、
  `credentials.list`、`credentials.onGeneration`、`sessionManager.getSessionId`/`getEntries`/
  `getSessionFile`/`getCredentialPins`/`appendResetBoundary`、`pi.appendEntry` 全部是 function。
- 18 个事件名全部注册成功：本扩展消费的 7 个（`session_start`、`session_switch`、
  `session_shutdown`、`message_start`、`message_update`、`message_end`、`agent_end`）、
  `turn_start`、`turn_end`、`agent_start`、`queue_update`、`assistant_message`、
  `cache_warming_start`、`cache_warming_end`、`before_provider_request`、`tool_result`、
  `session_before_branch`，以及 18.4.11 新增、本扩展不消费的 `goal_updated`。
- `omp --print --no-extensions -p "…"` 真实请求成功完成。
- `npm run herdr:tps:test`：118 passed；`cargo test -p herdr-usage --locked`：116 + 23 + 3 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。扩展与采集代码不需要修改。

18.4.10 升级时已逐项核对。upstream [`v18.4.9...v18.4.10`](https://github.com/can1357/oh-my-pi/compare/v18.4.9...v18.4.10)
是 214 个 commit、248 个文件（225 modified、21 added、1 renamed、1 removed），横跨
ai/catalog/coding-agent/natives/tui/utils/wire，标题虽是补丁规模但变更集不是，因此同样逐个接缝核对。

核心接缝完全没有被改动：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,select,blocks,cascade}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、`packages/agent/src/tokenizer.ts`、
`packages/coding-agent/src/extensibility/extensions/{runner,wrapper,types}.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/session/{session-manager,session-storage,session-paths,credential-pin}.ts`、
`packages/catalog/src/model-cache.ts`、`packages/coding-agent/src/commands/launch.ts` 与
`packages/coding-agent/src/cli/{args,main}.ts` 都不在变更集内。`auth/select.ts` 未变说明
“API-key 选择路径不读 session affinity”继续成立，本扩展的 session 粘性仍然必要。

被改过但语义不变的接缝：

- `packages/coding-agent/src/session/agent-session-types.ts` 只给 `AsyncJobSnapshotItem`
  增加可选 `command`（后台作业完整命令行，`label` 截断到 120 字符）；`ctx.agent` 形状未变。
- `packages/coding-agent/src/session/agent-session.ts` 新增只读的 `runStartedAt`，dispose 时调用
  `releaseShellSessions(sessionManager.getSessionId())` 回收常驻 shell，skill 图片准备期间按
  prompt generation 丢弃已中止的发布，并扩展 `waitForAdvisorCatchup` 的选项类型。
  extension 事件的投递顺序与字段形状不变；`getSessionId` 的调用点因此 +1（见下面的计数）。
- `packages/coding-agent/src/session/session-listing.ts` 的 `recoverOrphanedBackups` 改为从备份
  路径反推 primary 路径，保留存储自身的目录拼写；session 文件命名与 JSONL 行形状不变。
- `docs/extensions.md` 只在 input hook 的 `source` 表补了 `"rpc"`（`prompt`/`steer`/`follow_up`/
  `abort_and_prompt`）；`shared-events.ts` 未变，因此 18.4.7 至 18.4.10 都没有新增扩展事件。
- `packages/coding-agent/src/ratchet/prelude-definition.ts` 重命名，配合 `src/main.ts` 修复从
  已安装包导入 coding-agent 源码时 `Export named 'createRatchetPrelude' not found`
  （SDK、扩展加载器、bun-global `omp`）。只影响源码安装路径的导入，不改运行时扩展 API。
- catalog 侧更新 `models.json`、`scripts/{generate-models,generated-policies}.ts` 与
  `compat/rules`（MiniMax Token Plan 改按 pay-as-you-go 计价、LiteLLM 命名空间模型的显示名、
  prompt-cache 生命周期重算）。模型身份与显示名形状不变，`omp-profile.mjs` 的推断不受影响；
  改变的只是 OMP 自己写入 JSONL 的 `usage.cost`，collector 用 `import_prices` 自行计价，token 口径不变。
- `packages/coding-agent/src/modes/rpc/*` 重写 prompt 的 input hook 时序：`prompt` 在消息被
  admit 之后才 ack，`abort` 取消尚未 admit 的输入。本扩展不消费 RPC input 帧，wrapper 转发不变。

bundle 接口出现次数 18.4.9 → 18.4.10：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`before_provider_request` 3 → 3、
`session_start` 8 → 8、`message_update` 32 → 32、`session_shutdown` 9 → 9、`appendEntry` 19 → 19、
`.rotate` 17 → 17、`promote_queued_message` 5 → 5、`subagentCost` 8 → 8、
`cancel_subagent` 7、`steer_subagent` 13、`predict_word` 13 全部持平；只有 `getSessionId`
171 → 172，对应 `agent-session.ts` dispose 时新增的那一次 `sessionManager.getSessionId()`，
不是接口变化。

18.4.10 实机验证（`omp-linux-x64` SHA256
`e3f24c475d90b83acec05a26fd4499d2e6dffbf4ca3b0ee9e3e1bc1ab1a4e289` 与 release
`SHA256SUMS.txt` 一致，`--version` → `omp/18.4.10`，`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载生产扩展与临时探针，runtime 发出
  `ready`（protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）
  并响应 `get_state`；`context.agent.kind === 'main'`、`hasUI === false`、
  `keys.getWithCredential`/`sessions.release`/`limits.markReached`/`limits.rotate`、
  `credentials.list`/`credentials.onGeneration`、`sessionManager.getSessionId`/`getEntries`/
  `getSessionFile`/`getCredentialPins`/`appendResetBoundary` 与 `pi.appendEntry` 均在位，
  宿主 `@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载。生产补丁名
  `getWithCredentialWithObservation`、`releaseSessionCredential`、
  `appendResetBoundaryWithCredentialRelease` 由 `lib/omp-api-key-observer.mjs` 安装，测试覆盖其行为。
- 12 个事件名（本扩展消费的 7 个 + `queue_update`、`assistant_message`、`cache_warming_start`、
  `cache_warming_end`、`before_provider_request`）全部注册成功。
- 一次真实 print 模式请求走完整链路：assistant 行仍是 `model`/`duration` 与
  `usage`（`input`/`output`/`cacheRead`/`cacheWrite`/`totalTokens`/`cost`）形状，收尾是
  `session_exit`（dispose/normal）custom 条目，collector 的解析与回填不受影响。
- `npm run herdr:tps:test`：116 passed；`cargo test -p herdr-usage --locked`：23 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。扩展与采集代码不需要修改。

### 18.4.9

升级时 upstream [`v18.4.8...v18.4.9`](https://github.com/can1357/oh-my-pi/compare/v18.4.8...v18.4.9)
是 106 个 commit、187 个文件（171 modified、15 added、1 renamed），不再是补丁规模，因此逐个接缝核对：

核心接缝完全没有被改动：`packages/ai/src/auth-storage.ts`、
`packages/ai/src/auth/{rotation,select,blocks,cascade}.ts`、
`packages/coding-agent/src/utils/token-rate.ts`、`packages/agent/src/tokenizer.ts`、
`packages/coding-agent/src/extensibility/extensions/{runner,wrapper}.ts`、
`packages/coding-agent/src/extensibility/shared-events.ts`、
`packages/coding-agent/src/commands/launch.ts` 与
`packages/coding-agent/src/cli/{args,main}.ts` 都不在变更集内。`auth/select.ts` 未变说明
18.4.8 记录的“API-key 选择路径不读 session affinity”继续成立，本扩展的 session 粘性仍然必要。

被改过但语义不变的接缝：

- `packages/ai/src/auth/affinity.ts`：`SESSION_STICKY_CACHE_PREFIX` 与 `SessionsApi` 形状不变，
  只给持久化 sticky 行补了 30 天 TTL 常量与 60s 同凭据重写节流（内存 sticky 仍精确）。
  `auth/pool.ts` 与 `auth/sqlite-credential-store.ts` 跳过逐字节相同的凭据行重写，
  `auth/types.ts` 给 `OAuthAccountSummary` 增加可选 `lastUsedAtMs`。因为 `auth-storage.ts`
  未变，`credentials.onGeneration` 与 `keys.getWithCredential` 的语义不受这些写入节流影响。
- `packages/coding-agent/src/session/{session-manager,session-storage,session-paths,
  session-loader,blob-store,artifacts}.ts` 改的是持久化竞争：每个 session 文件新增 OS 租约
  `.{basename}.jsonl.owner`（`sessionOwnerLeasePath`）、append 锁只在必要时获取、写冲突读回重试
  （`MAX_WRITE_CONFLICT_RECOVERIES = 3`）、blob 改临时文件 + rename 原子写。`getSessionId`、
  `getEntries`、`getSessionFile`、`appendEntry`、`appendCustomEntry`、`appendResetBoundary`、
  `rewriteEntries` 的签名与 session JSONL 行形状（含 `type:"session"` header 的 `version: 3`）不变。
- `packages/coding-agent/src/session/credential-pin.ts` 只改 OAuth 账户 pin 的恢复判定（同账户且
  sticky 比 pin 更旧时才推进），不涉及 API key 路径。
- `packages/catalog/src/model-cache.ts` 改为写侧 `model_cache_refresh` 表，`writeModelCache` 返回
  `skipped|refreshed|written`，并接受 Azure 这类无 endpoint 的 `baseUrl: ""` 快照。模型身份与
  显示名形状不变，`omp-profile.mjs` 的显示名推断不受影响。
- 扩展 API 只增不改：`extensions/types.ts` 新增 `timedOutAskDialogResult` 与
  `ExtensionAskDialogSubmitResult`（ask 对话框超时兜底），`shared-events.ts` 未变，因此本扩展
  消费的六个事件与 `pi.appendEntry` 不变；新的 RPC 命令 `cancel_subagent`、`steer_subagent`、
  `predict_word` 与 opt-in ask-dialog 模式本扩展都不使用。
- `packages/ai/src/utils/http-inspector.ts` 现在自动清理 `http-400-requests` 下的旧 dump
  （>7 天或总量 >64 MiB）；`packages/utils/src/logger*` 改为批量写 + `OMP_LOG_LEVEL` +
  `logger.flush()` 并清理旧 daily/audit 日志。这两处路径都与 collector 无关——collector 只读
  `~/.omp/agent/sessions/**/*.jsonl`。
- 新增 opt-in 的 `omp gc --stale`（`gc.stale`）清理悬空 session marker、旧 debug report 与
  collab replica。本仓库不调用它；将来若启用，`--blobs` 只清扫无引用 blob，不改 JSONL 行。

**需要留意的一处行为变化**：多进程抢同一 session 文件时，活动 session 会迁到同目录的新文件
（新 session id 与 `parentSession`），旧文件不再被改写，宿主通过新增的
`sessionManager.onPersistenceNotice`（`{ reason, from, to }`）与 SDK 通知获知。collector 按目录
扫描 JSONL，新文件就是一个新 session，解析与回填无需改动；本扩展的 pin 按 session id 索引，
迁移后拿到新 session id，会重新排名并写新 pin，路径与 `/new` 一致。

18.4.2 起 system prompt 已不再包含被 Antigravity 拒绝的句子，已删除的
`lib/omp-antigravity-request-workaround.mjs` 不应恢复；18.4.9 的 provider 改动不要求本地重写请求。

bundle 接口出现次数 18.4.8 → 18.4.9 逐项持平：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`before_provider_request` 3 → 3、
`session_start` 8 → 8、`message_update` 32 → 32、`session_shutdown` 9 → 9、`appendEntry` 19 → 19、
`getSessionId` 171 → 171、`.rotate` 17 → 17、`promote_queued_message` 5 → 5、
`subagentCost` 8 → 8；新增 `cancel_subagent` 7、`steer_subagent` 13、`predict_word` 13。

实机验证（`omp-linux-x64` SHA256
`fe1ec455e3aa4536efc8e28b1023113de94b97e7dd68c726964dadeb488a1753` 与 release
`SHA256SUMS.txt` 一致，`--version` → `omp/18.4.9`，`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载生产扩展与临时探针，runtime 发出
  `ready`（protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）
  并响应 `get_state`；`context.agent.kind === 'main'`、`hasUI === false`、
  `context.modelRegistry.authStorage`、`credentials.list/onGeneration`、
  `limits.markReached/rotate`、`sessionManager.getSessionId/getEntries/getSessionFile/
  getCredentialPins` 和 `pi.appendEntry` 均存在，凭据仍为 10 条 / 8 个 provider。
- 生产补丁实际安装为 `getWithCredentialWithObservation`、`releaseSessionCredential`、
  `appendResetBoundaryWithCredentialRelease`；宿主 `@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载；
  14 个事件名（含 `queue_update`、`assistant_message`、`cache_warming_*`）全部可注册，探针写入的
  自定义条目能从 `getEntries` 读回。
- 一次真实 opencode-go print prompt（`opencode-go/deepseek-v4.1-flash`）走完整链路：assistant 行
  仍是 `model`/`duration` 与 `usage`（`input`/`output`/`cacheRead`/`cacheWrite`/`totalTokens`/
  `cost`）形状，session 内写入 `herdr-api-key-sticky-v1`（`credentialId` 11、`reason` initial），
  收尾是 `session_exit`（dispose/normal），collector 的解析与回填不受影响。
- `npm run herdr:tps:test`：116 passed；`cargo test -p herdr-usage --locked`：23 passed；
  `npm run herdr:tps:install -- --dry-run`：config、shell、links 均已就绪。不需要修改扩展或采集代码。


仍未覆盖的实机验证是 Antigravity Gemini session 的账号选择与恢复。

创建一个临时 opencode-go session，连续请求并确认 credential 不变；执行 `/reset` 后确认重新
排名；对请求 ID 与日志 ID 一致的普通 session，恢复后确认继续使用原 credential。
`/reset`、`/fresh` 后的跨进程 pin 恢复受上文 routing ID 限制。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
