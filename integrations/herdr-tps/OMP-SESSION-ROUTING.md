# OMP 会话路由扩展

OMP 运行时保持原始可执行文件不变。`omp-extension.mjs` 加载两个路由模块和一个独立观察器：

- `lib/omp-antigravity-session-router.mjs`：Antigravity Gemini 新会话路由；
- `lib/omp-api-key-stickiness.mjs`：opencode-go API-key 会话粘性兼容策略；
- `lib/omp-api-key-observer.mjs`：只观察实际 Key 选择，供 request 采集使用，不参与路由。

禁止通过修改、重打包或禁用 OMP Bun 字节码实现这些规则。wrapper 只启动
`.runtime/omp`，不维护或选择 patched/original 双份二进制。

## Antigravity 新会话路由

仅当 session 尚无 user/assistant transcript，并且首次请求选择
`google-antigravity/gemini-*` 时执行一次：

1. 读取全部可用 Antigravity OAuth 账号的 Gemini 5H usage；
2. 排除当前存在 credential block 的账号；
3. 选择 5H `remainingFraction` 最大的账号；
4. 最大值严格小于 `0.15` 时，将整个 session 切到
   `opencode-go/deepseek-v4.1-flash`，thinking level 设为 `high`；
5. 否则把该 session 固定到选中的 Antigravity credential。

正好 15% 保持 Antigravity。任何可用账号缺少可匹配的 5H usage 时 fail-open：保持
Antigravity，不基于不完整数据 fallback。

路由决定作为 `herdr-antigravity-route-v1` custom entry 写入 session，不包含 token 或账号
密钥。已有 transcript 或已有路由记录的 session 不会重新选择。扩展对这些 session 跳过
OMP 的每 request usage-aware preflight；真实请求返回 429 后仍由 OMP 独立的错误恢复路径处理。

## opencode-go 账户观察与粘性

观察器在 OMP 返回登录型 API-key 后将 credential ID 作为 `herdr-api-key-sticky-v1` custom
entry 写入 session；同一 credential 的连续请求不重复写，发生选择变化或 release 才追加记录。
它始终返回 OMP 的原始选择结果，不固定账户，也不改变 block、rotation 或 release 行为。

独立的粘性兼容策略让后续请求和跨进程 resume 复用该账号；credential 被 block、限流、删除、
禁用、rotation 或显式 release 后才重新选择。OMP 原生修复后设置
`HERDR_TPS_OMP_API_KEY_STICKINESS=0` 即可关闭该策略，观察器与 collector 仍继续记录账户。

记录中只保存 credential ID。新 session/branch 必须使用自己的 session ID，不能继承父 session
的 pin。并发首次请求合并为一次原生选择。

## OMP 升级

这些模块不依赖 bundle 字节偏移或 minified 私有方法名。只要新版 OMP 保持以下扩展事件和
`modelRegistry.authStorage` 方法的签名与语义不变，替换 `.runtime/omp` 后无需重新应用任何补丁：

- `session_start`、`session_switch`、`session_branch`、`session_tree`、`before_agent_start`；
- `fetchUsageReports`、`listOAuthAccounts`、`listCredentialBlocks`、
  `pinSessionOAuthAccount`、`getModelUsageHealth`；
- `getApiKey`、`exportSnapshot`、`releaseSessionCredentialForReselection`、
  `markUsageLimitReached`、`rotateSessionCredential`；
- 扩展 API 的 `setModel`、`setThinkingLevel`、`appendEntry`。

升级后的必要动作只有验证，而不是重新 patch：

```bash
npm run herdr:tps:test
```

再创建一个临时 Antigravity Gemini session，确认只产生一条
`herdr-antigravity-route-v1`；恢复该 session，确认模型和 credential 不变。若上述接口发生变化，
扩展应更新适配并重新测试，不能回退到修改 OMP 可执行文件。
