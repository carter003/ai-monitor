# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.12 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
扩展只补齐 opencode-go API Key 的 session 粘性，不固定 Antigravity credential、不执行跨
provider fallback，也不覆盖原生 `health.model`。

`omp-extension.mjs` 加载一个局部模块：

- `lib/omp-api-key-observer.mjs`：固定 opencode-go session 的首次实际 Key，并记录 request 路由。

运行时保持原始可执行文件不变；禁止修改、重打包或禁用 OMP Bun 字节码。wrapper 只启动
`.runtime/omp`，不维护或选择 patched/original 双份二进制。

## opencode-go 粘性与账户观察

路由器让新 session 的第一次 `keys.getWithCredential` 使用 OMP 原生 usage 排名，然后在内存中
复用该结果。`/reset` 的 `appendResetBoundary` 会释放当前 pin；`/new` 使用新的 session ID，二者
下一次请求都会重新排名。恢复已有 session 时，从 session custom entry 的 credential ID 和
AuthStorage 当前 credential 行恢复同一 Key，不触发排名。block、usage-limit、rotation、显式
release 和认证存储替换会清除 pin，保留 OMP 的失败恢复能力。

credential ID 作为 `herdr-api-key-sticky-v1` custom entry 写入所属 session；同一 credential 的连续
请求不重复写，发生选择变化或 release 才追加记录。记录不包含 API key。
父、子 session 共用 AuthStorage，但各自保留自己的 SessionManager 写入器；记录按 `sessionId`
选择写入器，不能复用最后激活的扩展 API。恢复同一子 session 会替换写入器，旧实例退出不会移除新实例。

运行期间修改 `auth.broker.url` 或 `auth.broker.token` 导致存储替换时，路由器通过
`credentials.onGeneration` 检测新的认证对象，立即重新安装观察钩子，并记录
`store-replaced` release、清理旧存储的选择缓存。新存储即使复用相同 credential ID，也会
重新记录 pin；旧存储尚未完成的异步调用不会覆盖新存储的观察状态。

OMP 18.4.12 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.12 的：

- `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`；
- `credentials.list`、`sessionManager.getSessionId`、`sessionManager.appendResetBoundary`；
- `credentials.onGeneration`（存储替换时保留订阅并在新对象就位后通知）；
- 扩展 API 的 `appendEntry`；
- 扩展上下文的 `ctx.agent.kind`（18.3.2 新增），用于拒绝子代理 session 驱动 pane 元数据。

Antigravity 账号选择完全由 OMP 原生实现，本扩展不再改写 system prompt；opencode-go session
粘性由本扩展维护。

**`limits.rotate` 的返回类型在 18.4.0 破坏性变更**：由 `Promise<boolean>` 改为
`Promise<CredentialRotation>`（`{ switched, afterSiblingWait? }`）。对象恒为真，只有 `switched`
为真才表示真的换了账号。观察器因此只在 `rotated.switched` 时记录 `rotation` release。
没有可用 sibling 时 rotate 最多等待 5s（`SIBLING_UNBLOCK_WAIT_MAX_MS`）等短暂 block 的
sibling 恢复，随后返回 `switched: false`——此时账号没变，按旧的真值判断会被误记成 rotation。
`packages/ai/src/auth/rotation.ts` 在 18.4.1 至 18.4.12 逐字节相同，这个契约到本版为止没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

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
排名；恢复旧 session 后确认继续使用原 credential。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
