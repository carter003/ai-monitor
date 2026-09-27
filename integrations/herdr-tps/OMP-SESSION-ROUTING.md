# OMP 原生认证与 opencode-go session 粘性

OMP 18.3.5 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
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

OMP 18.3.5 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.3.5 的：

- `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`；
- `credentials.list`、`sessionManager.getSessionId`、`sessionManager.appendResetBoundary`；
- `credentials.onGeneration`（存储替换时保留订阅并在新对象就位后通知）；
- 扩展 API 的 `appendEntry`；
- 扩展上下文的 `ctx.agent.kind`（18.3.2 新增），用于拒绝子代理 session 驱动 pane 元数据。

Antigravity request workaround 只依赖 `before_provider_request` 事件和 payload 中的
`requestType`、`userAgent`、`systemInstruction.parts`。Antigravity 账号选择仍由 OMP 原生实现；
opencode-go session 粘性由本扩展维护。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.3.5 升级时已逐项核对。upstream `v18.3.4...v18.3.5` 有 24 个 commit；运行时可见的改动是
prompt-cache 预热（新增 `session/cache-warmer.ts` +599 字节、`providers.cacheWarming` 设置默认
`idle`、扩展事件 `cache_warming_decision`、`session/agent-session.ts` +103 字节、
`sdk.ts` +35/4 字节、`session/settings.ts` +31/4 字节）、OpenAI 计费 web search（新增
`web/search/providers/openai.ts` +265 字节，catalog 新增 `web-search "openai"` 轴）、
Anthropic keep-alive 刷新从 `ai/src/stream.ts`（-284 字节）与 `ai/src/providers/anthropic.ts`
（-110 字节）移到新的预热器，以及语法高亮切换到 Oniguruma（vendored addon、tarball）。
bundle 层面按 `// packages/...` 注释切分模块：2748 → 2750，只新增 cache-warmer 与 openai
web search 两个模块，无删除；AST 归一化（标识符、私有字段名折叠）后仅 33 个模块有差异，全部
对应上面这份 upstream 改动，其余是重新生成的产物（`catalog/src/models.json`、
`catalog/src/compat/rules.json`、`internal-urls/docs-index.ts`、`natives` 内嵌 addon、
各 `package.json` 版本号）。
`utils/token-rate.ts`、`session/session-manager.ts`、`utils/dirs.ts`（`run/daemons` 目录解析）
逐字节相同；`config/model-registry.ts`、`ai/auth-storage.ts`、`ai/auth/pool.ts`、
`ai/auth/sqlite-credential-store.ts`、`ai/auth/affinity.ts` 与
`ai/providers/google-shared.ts`、`ai/providers/google-gemini-cli.ts`（Antigravity
`before_provider_request` payload 的构造处）归一化后相同。移植算法、目录解析、AuthStorage
命名空间与 Antigravity workaround 的输入面都无需改动。bundle 内
`keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`、
`credentials.list`、`credentials.onGeneration`、`sessionManager.appendResetBoundary`、
`appendEntry`、`before_provider_request` 的出现次数与 18.3.4 完全一致。

新增的 cache warming 不影响本扩展：`promptCache` 只对直连 Anthropic 模型填值，预热请求不进
transcript，扩展的消息事件不会因预热多计；预热用量写入 `model_usage`（`purpose` 为
`cache-warm`），而 herdr-usage 的 OMP 采集只解析 `type: "message"` 且 `role: "assistant"`
的记录，这些新记录被忽略，采集侧无需改动。若以后把 `promptCache` 打开到
opencode-go/Antigravity 模型，需要重新核对这一点。

实机探针扩展在 18.3.4 与 18.3.5 上输出完全相同：`context.agent.kind === 'main'`、
`context.modelRegistry.authStorage` 存在，`keys.getWithCredential`、`sessions.release`、
`limits.markReached`、`limits.rotate`、`credentials.list`、`credentials.onGeneration` 均为函数，
`credentials.list()` 返回 10 条 credential，`pi.appendEntry` 可回写，
`@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载。wrapper 链路 `~/.local/bin/omp` 与 `omp-tps`
均解析到 18.3.5；经 wrapper 启动时本扩展的钩子确实装上
（`getWithCredentialWithObservation`、`releaseSessionCredential`、
`appendResetBoundaryWithCredentialRelease`）。真实 TUI 启动后 `display_agent: "omp"`、
`tokens.model: stealth/space-bunny-alpha` 正常发布，流式期间 `tokens.tps` 从 38.7 升到 70.3。
opencode-go 粘性实测：新建 session 首次选择写入一条 `herdr-api-key-sticky-v1` pin
（`credentialId: 7`），`--continue` 恢复后不追加新 entry，即按持久 credential ID 复用而没有
重新排名。`install.mjs --dry-run` 无变更，`npm run herdr:tps:test` 117 passed，
`cargo test -p herdr-usage` 23 passed。

仍未覆盖的实机验证是 Antigravity Gemini session 的账号选择与恢复。

创建一个临时 opencode-go session，连续请求并确认 credential 不变；执行 `/reset` 后确认重新
排名；恢复旧 session 后确认继续使用原 credential。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
