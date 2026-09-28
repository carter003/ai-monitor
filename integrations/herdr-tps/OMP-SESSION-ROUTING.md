# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.1 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
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

OMP 18.4.1 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.1 的：

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
`ai/src/auth/rotation.ts` 在 18.4.1 逐字节相同，这个契约到本版为止没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.4.1 升级时已逐项核对。upstream `v18.4.0...v18.4.1` 有 340 个 commit、300+ 文件，
是一个以修复为主的 patch 版；`ai/src/auth/rotation.ts`、`auth-storage.ts`、`auth/pool.ts`、
`auth/affinity.ts`、`auth/sqlite-credential-store.ts` 逐字节相同，18.4.0 的 rotate 契约继续成立。
bundle 按 `// packages/...` 注释切分：2759 → 2765 个模块，删除 6 个、无新增；AST 归一化
（标识符、私有字段名折叠）后 148 个模块有差异。运行时可见且与本扩展相关的改动：

| upstream 模块 | 变化 | 对本扩展的影响 |
| --- | --- | --- |
| `ai/src/auth/types.ts`、`auth/select.ts`、`auth/refresh.ts` −1400、`auth-retry.ts`、`auth/cascade.ts` | 401 恢复复用近期签发的 token（`OAuthRefreshByIdOptions.reuseRecentMint`、`refreshReason: "auth-recovery"`），不再每次 401 都重签 | 原生行为；`keys.getWithCredential` 语义不变 |
| `ai/src/auth/usage/zai.ts`、`usage/claude.ts`、`error/rate-limit.ts` | 修复 Z.AI 额度恢复后仍被旧 block 钉在 fallback 模型、Claude saved resets 被限流探针清空、滚动 TPM/RPM 429 被当成额度耗尽 | 原生 block 行为，观察器的 `markReached`/`rotate` 放行逻辑不变 |
| `coding-agent/src/session/session-manager.ts` | `repairMissingUsage`：assistant message 缺 usage 时补零值并告警；`close()` 的 deferred publish 与权威重写拆分（修复 dispose 丢最后一条消息） | 采集口径需核对（见下） |
| `coding-agent/src/session/agent-storage.ts` | SQLite 语句改用 `using` 作用域，close 后不再挂住 `agent.db` | schema 未变；collector 不读该库 |
| `coding-agent/src/extensibility/extensions/types.ts` | 仅注释：`pi.sendMessage` 空闲时的即时渲染语义 | 事件与 `ExtensionContext` 不变 |
| `coding-agent/src/extensibility/extensions/runner.ts`、`wrapper.ts` | 新增 `ToolCallPreflight` 与 loop dispatch 标记（规则可在执行前拦截工具调用、别名归一） | 只影响工具调用路径，本扩展不注册工具 |
| `coding-agent/src/config/model-registry.ts` | keyless provider 判定、`getDiscoveryProviderId`、`disabledProviders` 屏蔽 fallback | `authStorage` 暴露方式不变 |
| `utils/dirs.ts` | composer 缓存目录 `cache/composer/` 改为单个 `cache/composer.db` | `run/daemons` 解析不变 |
| 删除 `tui/src/status-line/startup.ts`、`tui/src/prompt/composer-cache.ts`、`export/html/vendor/{highlight,marked}.min.js`、`prompts/system/stream-stall-continue.md`、`prompts/tools/salvaged-child-hint.md`、`slash-commands/helpers/usage-accounts.ts` | 状态栏首帧重绘、HTML 导出内联 vendor、stream 卡住后续跑提示词 | 与本扩展无关 |

`repairMissingUsage` 是唯一触及采集口径的改动：18.4.1 起，缺 usage 的 assistant message 会写入
全零 `usage`，而 collector 对 `usage` 缺失的记录是跳过的。本机 142064 条 assistant message 中
缺 usage 的为 0 条、已有 976 条 usage 全零的记录在 18.4.0 就已入库，因此实际行为不变；若以后
出现真正的 usage-less 消息，采集侧会多出一条零 token 记录，届时再决定是否过滤。

AST 归一化后逐字节相同的有 `utils/token-rate.ts`（移植算法）、`ai/auth-storage.ts`、
`ai/auth/rotation.ts`、`ai/auth/pool.ts`、`ai/auth/affinity.ts`、
`ai/auth/sqlite-credential-store.ts`（collector 依赖的 `auth_credentials` 与 cache
`session:sticky:%`）、扩展 `types.ts`、`ai/providers/google-gemini-cli.ts`、
`ai/providers/google-shared.ts`、`stats/src/db.ts`。bundle 内 `keys.getWithCredential` 5→5、
`limits.markReached` 5→5、`limits.rotate` 40→40、`credentials.onGeneration` 6→6、
`sessionManager.appendResetBoundary` 2→2、`appendEntry` 15→15、
`before_provider_request` 2→2、`systemInstruction` 10→10、`requestType` 6→6 出现次数与 18.4.0
完全一致（`getSessionId` 166→164，来自被删的 `status-line/startup.ts`，与接口无关）。

实机探针扩展在 18.4.0 与 18.4.1 上除 `ctx.model`（会话遗留模型，本次为
`gemini-3.8-flash`）外逐项相同：`context.agent.kind === 'main'`、
`context.modelRegistry.authStorage` 存在，上述接口均为函数，`credentials.list()` 返回 10 条
credential，`pi.appendEntry` 可回写，`@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载。
wrapper 链路 `~/.local/bin/omp` 与 `omp-tps` 均解析到 18.4.1；经 wrapper 启动时本扩展的
钩子确实装上（`getWithCredentialWithObservation`、`releaseSessionCredential`、
appendResetBoundaryWithCredentialRelease）。真实 print 会话确认 18.4.1 写出的 session jsonl
仍是 `message`/`custom`/`credential_pin` 形状，collector 照常入库并从 `credential_pin` 解析出
账户（`gemini-3.8-flash` / `google-antigravity` / `carter.gogo@gmail.com`）。
`install.mjs --dry-run` 无变更，`npm run herdr:tps:test` 118 passed，
`cargo test -p herdr-usage` 23 passed。本版不需要改扩展或采集代码。

仍未覆盖的实机验证是 Antigravity Gemini session 的账号选择与恢复。

创建一个临时 opencode-go session，连续请求并确认 credential 不变；执行 `/reset` 后确认重新
排名；恢复旧 session 后确认继续使用原 credential。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
