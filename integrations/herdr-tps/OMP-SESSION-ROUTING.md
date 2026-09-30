# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.4 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
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

OMP 18.4.4 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.4 的：

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
`ai/src/auth/rotation.ts` 在 18.4.1、18.4.2、18.4.3、18.4.4 逐字节相同，这个契约到本版为止没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.4.4 升级时已逐项核对。upstream `v18.4.3...v18.4.4` 有 169 个 commit、714 个文件，以 GPT-6.1 Sol
与 ultrafast service tier、bare slash 命令、ask 粘贴图片、Tern surface protocol 与原生 TUI、
skills 同名命名空间冲突、mnemopi 多音召回和一批 dead-code 清理为主。路由器与 collector 依赖的接缝
全部逐字节相同：`ai/src/auth-storage.ts`、`ai/src/auth/rotation.ts`（18.4.0 rotate 契约）、
`ai/src/auth/pool.ts`、`ai/src/auth/affinity.ts`、`ai/src/auth/sqlite-credential-store.ts`、
`coding-agent/src/session/session-manager.ts`、`session-storage.ts`、
`coding-agent/src/utils/token-rate.ts`、`agent/src/tokenizer.ts`、`catalog/src/model-cache.ts`。
`cli/args.ts`、`main.ts`、`commands/launch.ts` 也未改，wrapper 注入 `--extension`、参数转发和退出码
透传的语义不变。

bundle 按 `// packages/...` 注释切分：2768 → 2817 个模块（新增 49、删除 0），新增集中在 Tern/TSP
原生层、browser、mnemopi、skills 和 auth-broker 协议拆分。接口出现次数逐项保持或上升：
`getWithCredential` 6 → 6、`markReached` 6 → 6、`onGeneration` 8 → 8、
`appendResetBoundary` 3 → 3、`before_provider_request` 3 → 3、`session_start` 9 → 9、
`message_update` 30 → 30、`session_shutdown` 9 → 9、`text_delta`/`thinking_delta`/`toolcall_delta`
不变，`appendEntry` 19 → 20、`getSessionId` 151 → 171、`.rotate` 12 → 18。运行时可见且与本扩展
相关的改动：

| upstream 模块 | 变化 | 对本扩展的影响 |
| --- | --- | --- |
| `coding-agent/src/extensibility/extensions/{types,runner,wrapper}.ts`、`extensibility/shared-events.ts`、`agent/src/types.ts` | 新增 `assistant_message` 事件（改写已完成的 assistant 消息，只能替换 text block 的 text）与 `tool_result.additionalContext`（handler 在工具失败时也能注入上下文） | 新增可选事件，本扩展既不注册也不使用；既有事件的投递路径与 `MessageEndEvent` 形状不变 |
| `coding-agent/src/session/agent-session.ts`、`agent/src/agent.ts`、`session/queued-messages.ts`、`session/agent-session-events.ts` | 新增 `queue_update` 会话事件；keyword 提示改为随队列消息前置投递；`#queuedMessageRawText` 让 `removeQueuedMessage` 能按用户原始输入匹配；队列分组回调 | 只影响队列展示与 RPC 移除路径，不改 assistant 消息事件，也不改 session jsonl 的行形状 |
| `coding-agent/src/session/attachment-source-notice.ts`、`prompts/system/image-attachment.md` | 图片/视频来源提示抽成独立模块，图片提示新增 ask answer 分支 | 只影响用户附件提示的措辞；本扩展不读该 custom 消息 |
| `ai/src/usage/{shared,claude,cursor,synthetic,umans,zai,cline-pass,openai-codex}.ts` | `buildUsageAmount`/`usageStatus` 收敛到 `usage/shared.ts`；Cursor 丢弃未封顶且零请求的 legacy bucket | 纯重构加展示过滤，usage 报告字段与排序语义不变；`opencode-go` 的 usage provider 未改 |
| `ai/src/auth-broker/protocol.ts`（新）、`auth-broker/{client,remote-store,server,snapshot-cache}.ts` | ETag 解析与 block 快照排序抽到共享协议模块，`#raceWithSignal` 换成 `raceSignal` 工具 | 纯重构；broker 协议与快照顺序不变 |
| `coding-agent/src/session/{sql,indexed}-session-storage.ts`、`session-storage-errors.ts`（新） | `enoent` 构造器提取到共享模块 | 纯重构；session 文件写入与错误形状不变 |
| `coding-agent/src/extensibility/skills.ts`、`docs/skills.md` | 同名不同源的 skill 统一加来源命名空间，覆盖优先级确定化 | 只影响 skill 解析；本扩展不读 skill |
| `ai/src/providers/{anthropic,amazon-bedrock,bedrock-anthropic,bedrock-request-metadata,openai-*,cursor,google-shared,xai-base-url}.ts`、`catalog/src/models.json` | GPT-6.1 Sol 定价与 ultrafast service tier、Bedrock/Anthropic 与 xAI base URL 调整 | 原生 provider 行为；Antigravity / opencode-go 请求路径不受影响 |
| `coding-agent/src/tools/browser/**`、`tui/src/native/**`、`wire/src/tsp.ts`（新） | Tern 原生 surface protocol、browser 画中画与新工具模块 | 只影响 browser/Tern；本扩展不注册工具 |
| `ai/src/compaction/**`、`agent/src/compaction/*`、`mnemopi/src/core/*` | Bedrock/Azure/OpenAI 压缩端点、mnemopi 多音召回与查询缓存 | 原生行为 |

18.4.2 起 system prompt 已不再包含被 Antigravity 拒绝的句子，`lib/omp-antigravity-request-workaround.mjs`
及其测试一并删除，扩展不再改写 `before_provider_request` 的 payload；`prompts/system/system-prompt.md`
与 `prompts/advisor/system.md` 不在 `v18.4.3...v18.4.4` 的改动里（首行仍是 `RFC 2119 keywords: …`），
这个删除在 18.4.4 上依然正确。

实机验证（`omp-linux-x64` SHA256 `24c830fc…` 与 release `SHA256SUMS.txt` 一致，`--version` →
`omp/18.4.4`，`omp update --check` → Already up to date）：

- 探针扩展逐项与 18.4.3 相同——`context.agent.kind === 'main'`、`hasUI === false`（print 模式）、
  `context.modelRegistry.authStorage` 存在，`keys.getWithCredential`、`sessions.release`、
  `limits.markReached`、`limits.rotate`、`credentials.list`、`credentials.onGeneration`、
  `sessionManager.getSessionId`、`sessionManager.appendResetBoundary`、`sessionManager.getEntries`
  均为函数，`credentials.list()` 返回 10 条 credential（8 个 provider，与 18.4.3 集合相同），
  `pi.appendEntry` 可回写，`@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载并计数，
  事件面 `session_start`/`message_start`/`message_update`/`message_end`/`agent_end`/
  `session_shutdown` 全部到达，delta 为 `text_start`/`text_delta`/`text_end`，
  `message_end` 的 usage 字段形状不变（`cacheRead`/`cacheWrite`/`cost`/`input`/`output`/
  `reasoningTokens`/`totalTokens`），`errors: []`。
- 与生产扩展同时加载时，本扩展的三个钩子确实装上：`getWithCredentialWithObservation`、
  `releaseSessionCredential`、`appendResetBoundaryWithCredentialRelease`。
- 真实 TUI 会话（经 `~/.local/bin/omp` 启动、`HERDR_*` 指向假 socket）发布出 `model`
  （`gemini-3.8-flash`）、`display_agent`（`omp`）与非零 `tps`（27 个样本，82.5 → 78.7 tok/s），
  Herdr 官方集成同时给出 `idle` → `working` → `idle`。
- 真实 opencode-go print 会话写出的 session jsonl 仍是 `title`/`session`/`model_change`/
  `thinking_level_change`/`message`/`custom` 这些行，`custom` 仍是
  `herdr-api-key-sticky-v1`（`action: pin`，`provider: opencode-go`，`credentialId: 7`，
  `reason: initial`），常驻 collector 照常入库并解析出账户
  （`OpenCode Go Key 97520118` / `opencode-go` / `session_pin` / `initial`）。
- `install.mjs --dry-run` 无变更，`npm run herdr:tps:test` 116 passed，
  `cargo test -p herdr-usage` 23 passed。不需要改扩展或采集代码。

仍未覆盖的实机验证是 Antigravity Gemini session 的账号选择与恢复。

创建一个临时 opencode-go session，连续请求并确认 credential 不变；执行 `/reset` 后确认重新
排名；恢复旧 session 后确认继续使用原 credential。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
