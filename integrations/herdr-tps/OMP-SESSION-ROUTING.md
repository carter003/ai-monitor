# OMP 原生认证与账户观察

OMP 18.3.2 的 Antigravity 与 opencode-go 多账号选择全部使用原生 AuthStorage：模型对应
counter 的额度报告、session affinity、reserve、block、rotation、429 恢复，以及标题和子代理
credential 继承均由 OMP 管理。扩展不预选账号、不固定 Antigravity credential、不执行跨 provider
fallback，也不覆盖原生 `health.model`。

`omp-extension.mjs` 只加载不改变账号选择的辅助模块：

- `lib/omp-antigravity-request-workaround.mjs`：只重写 Antigravity 拒绝的 system prompt 片段；
- `lib/omp-api-key-observer.mjs`：只观察实际 Key 选择，供 request 采集使用。

运行时保持原始可执行文件不变；禁止修改、重打包或禁用 OMP Bun 字节码。wrapper 只启动
`.runtime/omp`，不维护或选择 patched/original 双份二进制。

## opencode-go 账户观察

观察器通过 OMP 18.3.2 的 `keys.getWithCredential` 读取实际选择，将 credential ID 作为
`herdr-api-key-sticky-v1` custom entry 写入 session；同一 credential 的连续请求不重复写，
发生选择变化或 release 才追加记录。记录不包含 API key，观察器不改变 block、rotation、
release 或其它认证行为。

OMP 18.3.2 已原生提供持久化的 session-to-credential affinity，包括 API key 粘性、额度
保留、限流恢复和跨进程 resume。旧的本地粘性 monkey-patch 与 Antigravity session router
均已删除，避免与原生选择器重复路由；观察器与 collector 继续记录账户。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。父 session、标题和子代理的 credential 继承由 OMP 原生 `sessions.inherit` 与 title
generator 管理。

## OMP 升级

观察器不依赖 bundle 字节偏移或 minified 私有方法名，只依赖 OMP 18.3.2 的：

- `keys.getWithCredential`、`sessions.release`、`limits.markReached`、`limits.rotate`；
- 扩展 API 的 `appendEntry`；
- 扩展上下文的 `ctx.agent.kind`（18.3.2 新增），用于拒绝子代理 session 驱动 pane 元数据。

Antigravity request workaround 只依赖 `before_provider_request` 事件和 payload 中的
`requestType`、`userAgent`、`systemInstruction.parts`。账号选择契约完全由 OMP 原生实现维护。

升级后的必要动作只有验证，而不是重新 patch：

```bash
npm run herdr:tps:test
```

18.3.2 升级时已逐项核对：`before_provider_request` payload 形状、
`keys.getWithCredential`/`appendEntry` 调用路径、`run/daemons` 目录解析与
`utils/token-rate.ts`（与 18.2.5 逐字节相同，移植算法无需改动）均未变；TUI 实测模型、
显示名、实时 TPS 与 `herdr-api-key-sticky-v1` pin entry 正常。仍未覆盖的实机验证是
Antigravity Gemini session 的账号选择与恢复。

创建一个临时 Antigravity Gemini session，确认账号由原生 usage ranking 和 session affinity
选择；恢复该 session，确认模型与 credential 保持稳定。若原生认证接口发生变化，应升级 OMP
并重新验证，不能回退到修改 OMP 可执行文件。
