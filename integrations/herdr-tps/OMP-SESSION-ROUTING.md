# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.9 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
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

credential ID 作为 `herdr-api-key-sticky-v1` custom entry 写入 session；同一 credential 的连续
请求不重复写，发生选择变化或 release 才追加记录。记录不包含 API key。

运行期间修改 `auth.broker.url` 或 `auth.broker.token` 导致存储替换时，路由器通过
`credentials.onGeneration` 检测新的认证对象，立即重新安装观察钩子，并记录
`store-replaced` release、清理旧存储的选择缓存。新存储即使复用相同 credential ID，也会
重新记录 pin；旧存储尚未完成的异步调用不会覆盖新存储的观察状态。

OMP 18.4.9 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.9 的：

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
`packages/ai/src/auth/rotation.ts` 在 18.4.1 至 18.4.9 逐字节相同，这个契约到本版为止没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.4.9 升级时已逐项核对。upstream [`v18.4.8...v18.4.9`](https://github.com/can1357/oh-my-pi/compare/v18.4.8...v18.4.9)
是 106 个 commit、187 个文件（171 modified、15 added、1 renamed），横跨
ai/catalog/coding-agent/natives/tui/utils/wire，不再是补丁规模，因此逐个接缝核对。

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
