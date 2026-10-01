# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.8 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
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

OMP 18.4.8 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.8 的：

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
`packages/ai/src/auth/rotation.ts` 在 18.4.1 至 18.4.8 逐字节相同，这个契约到本版为止没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.4.8 升级时已逐项核对。upstream [`v18.4.6...v18.4.8`](https://github.com/can1357/oh-my-pi/compare/v18.4.6...v18.4.8)
只有 5 个 commit、135 个文件（134 modified、1 removed、0 added），两个补丁都不触及本扩展：
18.4.7 是 pi-natives 的 Apple silicon macOS <27 启动段错误修复（Apple Foundation Models 只在
macOS 27+ 加载，删掉 `crates/pi-natives/src/applefm/stub.c`），以及全部内置主题新增可选的
`terminal` 配色段落（`background`/`foreground`/`chrome`/`widget`/16 个 `ansi`，供 Tern 这类
自己绘制终端的宿主参考）；18.4.8 只修 native-terminal（TSP）在未确认 credit 阻塞、5 秒 stall
兜底过期后仍要等一次无关渲染才出帧的问题。改动面限于 `crates/pi-natives`、`packages/tui`、
各包版本号、CI 与 lock 文件。

路由器与 collector 的核心接缝保持兼容：

- `packages/ai/src/auth-storage.ts`、`packages/ai/src/auth/{rotation,pool,affinity,blocks,select,
  cascade,types,sqlite-credential-store}.ts`、`packages/coding-agent/src/session/session-storage.ts`、
  `packages/coding-agent/src/extensibility/extensions/{runner,wrapper}.ts`、
  `packages/coding-agent/src/extensibility/shared-events.ts`、
  `packages/coding-agent/src/utils/token-rate.ts`、`packages/agent/src/tokenizer.ts`、
  `packages/catalog/src/model-cache.ts`、`packages/coding-agent/src/commands/launch.ts` 与
  `packages/coding-agent/src/cli/*` 完全不在变更集内，逐字节相同；18.4.0 的
  `CredentialRotation` 契约与 18.4.6 的 `UsageStatistics.subagentCost`、session JSONL 行形状继续成立。
- extension context 的 `agent.kind`、`hasUI`、`modelRegistry`、六个已消费事件和 `pi.appendEntry`
  不变；`terminal` 主题段落只是主题元数据，`packages/tui/src/theme/schema-validation.ts` 为它补上
  校验且整段可选（缺省仍合法），两个补丁都没有新增扩展事件或 RPC 命令。
- `packages/tui/src/native/backend.ts` 把 `NativeBackendOptions.now` 换成 `scheduler`
  （`RenderScheduler`）并新增 unref 的 stall 定时器，`tui.ts` 只多传一个调度器参数；这是 TUI
  内部渲染接口，扩展与 CLI 都不引用，不影响 delta 投递、hook 安装和退出码语义。
- CLI `--extension` 解析与 `packages/coding-agent/src/commands/launch.ts` 仍逐字节兼容。

18.4.2 起 system prompt 已不再包含被 Antigravity 拒绝的句子，已删除的
`lib/omp-antigravity-request-workaround.mjs` 不应恢复；18.4.8 的 natives/TUI 改动不要求本地重写请求。

bundle 接口出现次数 18.4.6 → 18.4.8 逐项持平：`getWithCredential` 10 → 10、`markReached` 6 → 6、
`onGeneration` 8 → 8、`appendResetBoundary` 3 → 3、`before_provider_request` 3 → 3、
`session_start` 8 → 8、`message_update` 32 → 32、`session_shutdown` 9 → 9、`appendEntry` 19 → 19、
`getSessionId` 171 → 171、`.rotate` 17 → 17、`promote_queued_message` 5 → 5、
`subagentCost` 8 → 8。

实机验证（`omp-linux-x64` SHA256
`1b88f7a0da3f61edda915d836e11f63f20f2b56aeac2ddfa4ece0faa726f31c2` 与 release
`SHA256SUMS.txt` 一致，`--version` → `omp/18.4.8`，`omp update --check` → Already up to date）：

- 经 `omp-with-tps.mjs --mode rpc --no-session --no-ui` 加载生产扩展与临时探针，runtime 发出
  `ready`（protocol v1，支持 v1/v2，maxFrameBytes 1048576，maxReassembledFrameBytes 67108864）
  并响应 `get_state`；`context.agent.kind === 'main'`、`hasUI === false`，
  `context.modelRegistry.authStorage`、`credentials.list/onGeneration`、
  `limits.markReached/rotate`、`sessionManager.getSessionId/getEntries/getSessionFile` 和
  `pi.appendEntry` 均存在，凭据仍为 10 条 / 8 个 provider。
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
