# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.3 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
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

OMP 18.4.3 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.3 的：

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
`ai/src/auth/rotation.ts` 在 18.4.1、18.4.2、18.4.3 逐字节相同，这个契约到本版为止没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.4.3 升级时已逐项核对。upstream `v18.4.2...v18.4.3` 有 77 个 commit、220 个文件，以 Command Code
provider 刷新、task 推测执行和 TUI/perf 为主。路由器与 collector 依赖的接缝全部未改：
`ai/src/auth/rotation.ts`（18.4.0 rotate 契约）、`auth-storage.ts`、`auth/pool.ts`、
`auth/affinity.ts`、`auth/sqlite-credential-store.ts`、`coding-agent/src/session/session-manager.ts`、
`session-storage.ts`、扩展 `extensibility/extensions/{types,runner,wrapper}.ts`、
`coding-agent/src/utils/token-rate.ts` 和 `agent/src/tokenizer.ts` 都不在改动文件里。

bundle 按 `// packages/...` 注释切分：2765 → 2768 个模块（新增 5、删除 2）。原始字节比对在这两个
版本之间不可用——同一模块内标识符被打散重命名，逐字节差异会覆盖全部模块——因此这一版的证据是
三层：upstream 源文件 diff、bundle 内接口出现次数、实机探针。接口出现次数逐项一致
（`keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`、
`credentials.list`、`credentials.onGeneration`、`sessionManager.getSessionId`、
`sessionManager.appendResetBoundary`、`appendEntry`、`before_provider_request`、`userAgent`、
以及 `session_start`/`message_start`/`message_end`/`agent_end`/`session_shutdown` 与
`text_delta`/`thinking_delta`/`toolcall_delta`），只有 `message_update` 28 → 30。运行时可见且与
本扩展相关的改动：

| upstream 模块 | 变化 | 对本扩展的影响 |
| --- | --- | --- |
| `ai/src/usage/commandcode.ts`、`usage/registry.ts`、`catalog/src/compat/rules/providers/commandcode.kdl` | 新增 Command Code provider、usage 报告与排序策略 | 新增 provider，不在本扩展的观察列表（仅 `opencode-go`）内，Antigravity/opencode-go 路径不变 |
| `ai/src/registry/engine/api-key.ts`、`catalog/.../auth/commandcode.kdl` | 可选登录探测支持 `trustForbidden`（Command Code 允许 403） | 只影响登录校验；不改变已存储 credential 的选择 |
| `coding-agent/src/session/agent-session.ts` | `message_update` 的扩展投递改为先 `hasHandlers` 再入队（per-delta 热路径） | 纯性能；本扩展注册了 `message_update`，delta 仍然逐条到达（实机核对过） |
| `coding-agent/src/extensibility/shared-events.ts`、`agent/src/types.ts`、`docs/extensions.md`、`docs/hooks.md` | `tool_call` 的 `additionalContext` 去重（同一次调用与同一批次内重复值只保留一份） | 本扩展不用 `tool_call` 的 `additionalContext` |
| `agent/src/agent-loop.ts`、`agent.ts`、`types.ts`、`speculation/host.ts`、`coding-agent/src/task/{index,spawn-run,speculative-launch}.ts` | 批量 task 调用的推测执行：流式参数预授权启动子任务，新增 `transformAssistantMessagePreservesToolCalls` | 只影响 tool/task 路径；本扩展不注册工具，子代理仍以 `ctx.agent.kind === 'sub'` 拒绝 |
| `coding-agent/src/cli/args.ts`、`cli/flag-tables.ts`、`main.ts`、`commands/launch.ts` | 内置枚举 flag（`--mode`/`--thinking`/`--approval-mode`）取值校验、退出码 2；`--no-ui` 要求 `--mode rpc`；非 TTY 的自动 print 判定改到扩展 flag 解析之后；ACP 改为按 session 绑定扩展；`--export` 也校验枚举 | wrapper 注入的 `--extension` 语义不变；参数转发与退出码透传照旧，误用 flag 现在以 usage error 退出而不是静默 |
| `ai/src/utils/event-stream.ts`、`utils/src/stream.ts`、`ai/src/providers/google-{shared,gemini-cli}.ts` | 事件队列改用 head 游标代替 `shift()`；SSE 无诊断监听者时不再挂 observer，raw 复用冻结的空数组 | 纯性能，事件顺序与协议不变 |
| `coding-agent/src/tools/output-meta.ts` | 工具结果溢出 artifact 的阈值先用 UTF-16 长度快速判定 | 只影响超长工具输出的落盘方式 |
| `coding-agent/src/sdk.ts` | 松散 edit 恢复声明 `transformAssistantMessagePreservesToolCalls` | 原生行为 |

18.4.2 起 system prompt 已不再包含被 Antigravity 拒绝的句子，`lib/omp-antigravity-request-workaround.mjs`
及其测试一并删除，扩展不再改写 `before_provider_request` 的 payload；`prompts/` 不在
`v18.4.2...v18.4.3` 的改动里，这个删除在 18.4.3 上依然正确。

实机验证（`omp-linux-x64` SHA256 与 release `SHA256SUMS.txt` 一致，`--version` → `omp/18.4.3`，
`omp update --check` → Already up to date）：

- 探针扩展逐项与 18.4.2 相同——`context.agent.kind === 'main'`、`hasUI === true`、
  `context.modelRegistry.authStorage` 存在，`keys.getWithCredential`、`sessions.release`、
  `limits.markReached`、`limits.rotate`、`credentials.list`、`credentials.onGeneration`、
  `sessionManager.getSessionId`、`sessionManager.appendResetBoundary` 均为函数，
  `credentials.list()` 返回 10 条 credential（8 个 provider，与 18.4.2 集合相同），
  `pi.appendEntry` 可回写，`@oh-my-pi/pi-agent-core` 的 `Tokenizer` 可加载并计数，`errors: []`。
- 经 wrapper 启动时本扩展的钩子确实装上（`getWithCredentialWithObservation`、
  `releaseSessionCredential`、`appendResetBoundaryWithCredentialRelease`）。
- 真实 TUI 会话（经 wrapper 启动、`HERDR_*` 指向假 socket）发布出 `model`、`display_agent`
  与非零 `tps`（44 个非零样本，52.8 → 135.4 tok/s，结束后归零），Herdr 官方集成同时给出
  `idle` → `working` → `idle`；事件面 `session_start`/`message_start`/`message_update`/
  `message_end`/`agent_end`/`session_shutdown` 全部到达，`message_end` 的 usage 字段形状不变。
- 真实 opencode-go print 会话写出的 session jsonl 仍是 `message`/`custom`/`session` 等形状，
  `custom` 仍是 `herdr-api-key-sticky-v1`（`action: pin`，`credentialId: 7`），collector 照常入库
  并解析出账户（`OpenCode Go Key 97520118` / `opencode-go` / `session_pin` / `initial`）。
- `install.mjs --dry-run` 无变更，`npm run herdr:tps:test` 116 passed，
  `cargo test -p herdr-usage` 23 passed。不需要改扩展或采集代码。

仍未覆盖的实机验证是 Antigravity Gemini session 的账号选择与恢复。

创建一个临时 opencode-go session，连续请求并确认 credential 不变；执行 `/reset` 后确认重新
排名；恢复旧 session 后确认继续使用原 credential。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
