# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.0 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
扩展只补齐 opencode-go API Key 的 session 粘性，不固定 Antigravity credential、不执行跨
provider fallback，也不覆盖原生 `health.model`。

`omp-extension.mjs` 加载两个局部模块：

- `lib/omp-antigravity-request-workaround.mjs`：只重写 Antigravity 拒绝的 system prompt 片段；
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

OMP 18.4.0 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.0 的：

- `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`；
- `credentials.list`、`sessionManager.getSessionId`、`sessionManager.appendResetBoundary`；
- `credentials.onGeneration`（存储替换时保留订阅并在新对象就位后通知）；
- 扩展 API 的 `appendEntry`；
- 扩展上下文的 `ctx.agent.kind`（18.3.2 新增），用于拒绝子代理 session 驱动 pane 元数据。

Antigravity request workaround 只依赖 `before_provider_request` 事件和 payload 中的
`requestType`、`userAgent`、`systemInstruction.parts`。Antigravity 账号选择仍由 OMP 原生实现；
opencode-go session 粘性由本扩展维护。

**`limits.rotate` 的返回类型在 18.4.0 破坏性变更**：由 `Promise<boolean>` 改为
`Promise<CredentialRotation>`（`{ switched, afterSiblingWait? }`）。对象恒为真，只有 `switched`
为真才表示真的换了账号。观察器因此只在 `rotated.switched` 时记录 `rotation` release。
没有可用 sibling 时 rotate 最多等待 5s（`SIBLING_UNBLOCK_WAIT_MAX_MS`）等短暂 block 的
sibling 恢复，随后返回 `switched: false`——此时账号没变，按旧的真值判断会被误记成 rotation。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.4.0 升级时已逐项核对。upstream `v18.3.5...v18.4.0` 有 136 个 commit、300+ 文件。
bundle 按 `// packages/...` 注释切分：2750 → 2759 个模块，只新增 9 个、无删除；AST 归一化
（标识符、私有字段名折叠）后 104 个模块有差异。运行时可见且与本扩展相关的改动：

| upstream 模块 | 变化 | 对本扩展的影响 |
| --- | --- | --- |
| `ai/src/auth/rotation.ts` +552 字节 | `rotate` 返回 `CredentialRotation`；无可用 sibling 时最多等 5s 再返回 `afterSiblingWait` | 观察器改判 `.switched`，否则记错 rotation |
| `ai/src/auth/blocks.ts` +1246、`select.ts`、`resets.ts`、`usage.ts`、`usage-cache.ts` | 认证 block 与 quota block 分离（新增 `AUTH_BLOCK_SCOPE`、`ACCOUNT_POLICY_BLOCK_SCOPE`）；修复 Anthropic 额度恢复后仍被 block、并发 usage 刷新反复探测、auth 失败只轮换一个 sibling | 原生行为；观察器的 `markReached`/`rotate` 放行逻辑不变 |
| `ai/src/auth/cascade.ts`、`ai/src/auth-retry.ts` | 上游消费方同步改读 `.switched` | 无 |
| `ai/src/providers/google-shared.ts`、`google-gemini-cli.ts` | 新增 `mapGoogleUsage`，修复 Gemini/Antigravity 缺 prompt token 或 cached > prompt 时的用量与计价（`google-gemini-cli.ts` −652 字节） | Antigravity payload 的 `requestType`/`userAgent`/`systemInstruction.parts` 构造不变，workaround 无需改 |
| `coding-agent/src/session/agent-session.ts` | `PromptDroppedError`、`resetAccountLockKey` 统一 reset 协调、judge 共享缓存、`purpose: "ttsr"` | 消息事件类型不变，observer 不受影响 |
| `coding-agent/src/session/agent-session-types.ts` | 新增 `inheritedSessionAgents`、`runCommands`、`throwOnDrop` | 未使用 |
| `coding-agent/src/extensibility/extensions/wrapper.ts` +399 字节 | `renderCall` 第二参数用 Proxy 同时满足 omp 与 pi 的 `Theme` 契约 | 本扩展不注册工具；事件与 `ExtensionContext` 不变 |
| `utils/dirs.ts` +228 字节 | Windows 8.3 长路径展开、`getJudgmentCacheDbPath` | `run/daemons` 解析逐字节不变 |
| 新增 `stats/src/{rollup,frustration,live}.ts`、`coding-agent/src/judgment/{cache,standalone}.ts`、`telemetry-settings.ts`、`eval/startup-warning.ts`、`natives/native/path.js`、`prompts/tools/wait-no-message.md` | stats 面板重构与 frustration judge、judge 缓存、OTLP 导出开关、telemetry 命名空间 `pi.*` → `omp.*` | 与本扩展无关；collector 不读 OMP 的 stats db |

AST 归一化后逐字节相同的有 `utils/token-rate.ts`（移植算法）、`session/session-manager.ts`
（会话文件格式与 `appendResetBoundary`）、`config/model-registry.ts`、`ai/auth-storage.ts`、
`ai/auth/pool.ts`、`ai/auth/affinity.ts`、`ai/auth/sqlite-credential-store.ts`
（collector 依赖的 `auth_credentials` 与 cache `session:sticky:%`）、扩展 `types.ts` 与
`runner.ts`、`session/agent-storage.ts`。bundle 内 `keys.getWithCredential` 5→5、
`sessions.release` 5→5、`limits.markReached` 5→5、`limits.rotate` 40→40、
`credentials.list` 6→6、`credentials.onGeneration` 6→6、`sessionManager.appendResetBoundary`
2→2、`getSessionId` 164→164、`appendEntry` 15→15、`before_provider_request` 2→2、
`systemInstruction` 10→10、`requestType` 6→6，出现次数与 18.3.5 完全一致。

实机探针扩展在 18.3.5 与 18.4.0 上输出逐项相同：`context.agent.kind === 'main'`、
`context.modelRegistry.authStorage` 存在，上述接口均为函数，`credentials.list()` 返回 10 条
credential，`pi.appendEntry` 可回写，`@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载。
wrapper 链路 `~/.local/bin/omp` 与 `omp-tps` 均解析到 18.4.0；经 wrapper 启动时本扩展的
钩子确实装上（`getWithCredentialWithObservation`、`releaseSessionCredential`、
appendResetBoundaryWithCredentialRelease）。真实 print 会话确认 18.4.0 写出的 session jsonl
仍是 `message`/`custom`/`model_usage` 形状，assistant message 新增 `credentialId` 与
`providerPayload` 两个字段，collector 照常入库。rotate 返回 `{ switched: false,
afterSiblingWait: true }` 时不写 release，由 `test/omp-api-key-observer.test.mjs` 固定
（未打补丁前该用例失败）。`install.mjs --dry-run` 无变更，`npm run herdr:tps:test` 118 passed，
`cargo test -p herdr-usage` 23 passed。

仍未覆盖的实机验证是 Antigravity Gemini session 的账号选择与恢复。

创建一个临时 opencode-go session，连续请求并确认 credential 不变；执行 `/reset` 后确认重新
排名；恢复旧 session 后确认继续使用原 credential。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
