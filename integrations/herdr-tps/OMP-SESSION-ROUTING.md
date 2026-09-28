# OMP 原生认证与 opencode-go session 粘性

OMP 18.4.2 的认证、额度报告、reserve、block、rotation 和 429 恢复继续使用原生 AuthStorage。
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

OMP 18.4.2 会记录 API key 的 session affinity，但 API-key 选择路径没有读取这份 affinity；
本地路由补丁因此位于公开的 `keys.getWithCredential`、`sessions.release` 和 reset boundary 接口，
不修改 Bun runtime。普通 request 不再因额度样本刷新或排序分数变化而换号。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

路由器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.4.2 的：

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
`ai/src/auth/rotation.ts` 在 18.4.1、18.4.2 逐字节相同，这个契约到本版为止没有再变。

升级后必须验证这些公开接口与 reset boundary 行为：

```bash
npm run herdr:tps:test
```

18.4.2 升级时已逐项核对。upstream `v18.4.1...v18.4.2` 有 79 个 commit、142 个文件，以性能
优化和修复为主；`ai/src/auth/rotation.ts`、`auth-storage.ts`、`auth/pool.ts`、
`auth/affinity.ts`、`auth/sqlite-credential-store.ts`、`session/session-manager.ts` 逐字节相同，
18.4.0 的 rotate 契约继续成立。bundle 按 `// packages/...` 注释切分：2765 → 2765 个模块，
无新增无删除；AST 归一化（标识符、私有字段名折叠）后 77 个模块有差异。运行时可见且与本扩展
相关的改动：

| upstream 模块 | 变化 | 对本扩展的影响 |
| --- | --- | --- |
| `prompts/system/system-prompt.md`、`prompts/advisor/system.md` | 首行改为 `RFC 2119 keywords: …`，upstream 修掉了 Antigravity 对该句的 429（#13379） | 本地 workaround 目标串已不存在，已删除 |
| `packages/utils/src/index.ts` 新增 `cloneJsonTree`；`agent/src/agent-loop.ts` | 工具调用的流式参数快照改用容器深拷贝（修掉参数被 provider 原地改写）；新增 `tool_execution_end`，工具结果消息按调用顺序落库而非完成顺序 | 只影响工具调用路径，本扩展不注册工具；事件面新增一个，本扩展未监听 |
| `agent/src/tokenizer.ts` | 精确 token 计数加模型级有界 LRU 缓存，>16k 字符与 ≥16 片段仍走原生批量路径 | 返回值不变，reporter 的分词口径不变 |
| `coding-agent/src/utils/token-rate.ts` | `#countPending` 对 pending 分块计数做 memo（每流、每次 append 失效） | 纯性能，返回值不变，移植算法无需跟进 |
| `coding-agent/src/session/session-storage.ts` | 写入前用一次 `fstat` 同时做 inode 身份校验与回滚点，锁文件改用 `fstat`/`stat` 比对 dev+ino | session 文件格式与写入语义不变，collector 口径不变 |
| `coding-agent/src/session/agent-storage.ts`、`catalog/src/model-cache.ts` | `model_usage` 查询去掉未用列，模型缓存改深比较失效 | schema 未变；collector 不读该库 |
| `coding-agent/src/session/agent-session.ts`、`eval/*` | 子代理 eval kernel 与父会话隔离，`getEvalSessionId()` 不再可空 | 与本扩展无关 |
| `ai/src/error/flags.ts`、`ai/src/stream.ts` | 溢出判定优先用 `contextTokens`；thinking budget 按模型输出上限夹紧 | 原生行为；Antigravity / opencode-go 路径不受影响 |
| `coding-agent/src/tools/grep.ts`、`jfind`、natives | grep 流式搜索的背压与取消、`find` 工具 20s 超时 | 原生行为 |

18.4.2 起 system prompt 已不再包含被 Antigravity 拒绝的句子，`lib/omp-antigravity-request-workaround.mjs`
及其测试一并删除，扩展不再改写 `before_provider_request` 的 payload。实机在 18.4.2 上抓到的
provider 请求里该行为 `RFC 2119 keywords: MUST, REQUIRED, …`，workaround 的匹配串
`RFC 2119: MUST, REQUIRED, …` 已不存在（`prompts/` 两个文件都已改写），因此删除不会让 429 复发。

AST 归一化后逐字节相同的有 `ai/auth-storage.ts`、`ai/auth/rotation.ts`、`ai/auth/pool.ts`、
`ai/auth/affinity.ts`、`ai/auth/sqlite-credential-store.ts`（collector 依赖的 `auth_credentials` 与
cache `session:sticky:%`）、`session/session-manager.ts`、扩展 `types.ts`/`runner.ts`/`wrapper.ts`、
`ai/providers/google-gemini-cli.ts`、`ai/providers/google-shared.ts`。bundle 内
`keys.getWithCredential` 5→5、`limits.markReached` 5→5、`limits.rotate` 40→40、
`credentials.onGeneration` 6→6、`sessionManager.appendResetBoundary` 2→2、
`sessionManager.getSessionId` 166→166、`appendEntry` 15→15、`before_provider_request` 2→2、
`systemInstruction` 10→10、`requestType` 6→6、`userAgent` 221→221，与 18.4.1 完全一致。

实机验证（`omp-linux-x64` SHA256 与 release `SHA256SUMS.txt` 一致，`--version` → `omp/18.4.2`）：
探针扩展在 18.4.2 上逐项与 18.4.1 相同——`context.agent.kind === 'main'`、
`context.modelRegistry.authStorage` 存在，上述接口均为函数，`credentials.list()` 返回 10 条
credential（provider 集合不变），`pi.appendEntry` 可回写，`@oh-my-pi/pi-agent-core` 的
`Tokenizer` 可加载，`errors: []`。wrapper 链路 `~/.local/bin/omp` 与 `omp-tps` 均解析到 18.4.2；
经 wrapper 启动时本扩展的钩子确实装上（`getWithCredentialWithObservation`、
`releaseSessionCredential`、`appendResetBoundaryWithCredentialRelease`）。真实 print 会话确认
18.4.2 写出的 session jsonl 仍是 `message`/`custom`/`credential_pin` 形状，collector 照常入库并从
`credential_pin` 解析出账户（`space-bunny-free` / `opencode-go` / `session_pin`）。
`install.mjs --dry-run` 无变更，`npm run herdr:tps:test` 116 passed（删掉 workaround 的 2 条），
`cargo test -p herdr-usage` 23 passed。除删除 workaround 外不需要改扩展或采集代码。

仍未覆盖的实机验证是 Antigravity Gemini session 的账号选择与恢复。

创建一个临时 opencode-go session，连续请求并确认 credential 不变；执行 `/reset` 后确认重新
排名；恢复旧 session 后确认继续使用原 credential。再创建 Antigravity Gemini session，确认其
账号仍由原生 usage ranking 和 session affinity 选择。若公开接口发生变化，应升级适配扩展，
不能回退到修改 OMP 可执行文件。
