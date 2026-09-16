# OMP Antigravity 账号轮换平衡补丁

补丁脚本:`patch-omp-antigravity-balance.mjs`(对 omp 二进制做四处**等长字节补丁**)。

结论先说:omp 对多 OAuth 账号的默认排序是"**快过期的窗口优先塞流量**"(use-it-or-lose-it),
曾实测把一个账号从 0 快速消耗并触发限流。本补丁把决策改成用户确认的三条规则，并加固会话粘性。

## 1. 用户确认的三条规则 (2026-09-13)

1. **谁的 5H 剩余多,下一个新建会话就用谁**。
2. **新会话只剩 ≤15% 时自动降级**:同模型全部账号进入 reserve 后，不再等待硬 429，
   由 OMP 原生 usage-aware preflight 路由到 `opencode-go/deepseek-v4.1-flash:high`。
3. **会话粘性保持**:已有会话不换账号、不换模型;只有新建会话的首启选择遵循规则 1/2。

## 2. 机制与根因 (源码:`pi-ai/src/auth-storage.ts`)

omp 对 google-antigravity 的账号排序在 `pi-ai/src/auth-storage.ts`:

- `#computeWindowRequiredDrain` 返回 `headroom / remainingHours` —— 注释明确是
  "use it or lose it":谁剩余额度即将作废就先烧谁。这是正反馈:越塞 → 剩余越少、距重置越近 → drain 越高 → 塞得更狠 → 撞突发限流。
- antigravity 策略(`usage/google-antigravity.ts`)把"剩余最少的窗口"当 primary。
  多账号的 primary 因此常是 weekly(剩 91%),大量 5h 剩余根本没参与决策。
- OAuth 凭据还有会话级钉扎(`session:sticky:<provider>:<sessionId>`,30 天 TTL，
  resume 时从 transcript 的 `credential_pin` 恢复；某些会话创建路径还会预先继承父会话账号)。
  对 API Key 来说，这种「新 session 尚无 request 就已经有 pin」不能证明会话已经开始，Site D
  会按 session 创建时间把它与当前会话自己写入的 pin 区分开。
- 多 API-key 路径不同:`#selectApiKeyCredential` 每次调用都重新做 usage 排名，
  选完后才覆盖 `session:sticky`，没有在排名前读取 sticky。因此同一个会话会随额度排名从账号 A 切到 B。Site D 补齐该缺口。

## 3. 四处字节补丁 (均严格等长)
支持的构建:minified 符号按版本分支——18.2.1(`#lt/#ct/#wt`、`async#X`、`async#he/#te`、迭代器 `f`/结果 `d`)、18.2(`#mt/#yt/#lt`、`#z`、`#Q/#oe`)、18.1(`#Ot/#it/#gt`、`#B`、`#ne/#Q`)。Site B 按 span 内提取的 selector/limits/scope 符号重发，不硬编码旧符号。
### Site A — 仅为 Antigravity 去掉"重置时钟"项

`auth-storage` `#computeWindowRequiredDrain` 的完整 344 字节函数 span 被压缩并等长回填。
Site B 给 Antigravity 排名专用的 limit 副本标记 `id: "ag5h"`;Site A 只对这个标记返回
纯 headroom，其他 provider 仍执行上游的 `headroom / remainingHours`。

```javascript
if (limit.id === "ag5h") return headroom;
return headroom / remainingHours;
```

Antigravity 排序变为"**剩余多者优先**"(负反馈:用得越多,新会话越快离开它)。

### Site B — antigravity primary 固定为该模型族的 5h 窗口

span 内重排代码为等长压缩形式:

```javascript
findWindowLimits(e,t){
const s=eei(e,t),w=s.find(w=>w.id.endsWith("5h"))??s[0];
return{primary:w&&{...w,id:"ag5h"}}},
scopeLimits:MX,blockScope(e){return`counter:${jz(e?.modelId)??"unknown"}`},
```

效果:primary 固定为 5h 窗口，无 5h 时回退列表头。只影响排名，429 自动 block、stale-block heal 等走独立的 `scopeLimits`。

### Site C — usage-aware fallback 仅限空 transcript

OMP 原生 usage-aware preflight 默认在每个 turn 前运行，会让已有会话在进入 reserve 后换模型。
Site C 在该 preflight 入口增加 `agent.state.messages.length` 守卫：只有 transcript 为空的新会话
参与 15% reserve 路由；已有会话保持账号与模型粘性。

### Site D — 多 API Key 会话账号粘性与新会话边界

完整替换 API-key selector span。OMP 的 UUIDv7 session id 前 12 个十六进制字符就是会话创建时间；
只有 sticky 的 `lastUsedAtMs` 不早于该时间，才说明这个 pin 是当前会话自己首个 request 选出的。
匹配且未 block 时直接复用，不再读取 usage 并重排。

继承或恢复出来的旧 pin 早于新 session 创建时间，不能作为「当前会话已经有 request」的证据，
因此会被忽略，首个 request 仍查询全部 API Key 并重新排名。排名结果随后由 OMP 原生路径写成
当前 session 的新 sticky，后续 request 才保持账号粘性。非 UUID session id 保留上游兼容行为。

账号排名只在候选凭据可用时生效。若额度更充足的账号被 OMP 的跨进程 credential block
暂时封锁，或首个推理请求返回 403/429，OMP 会回退到下一个可用账号，并把成功账号写入
本会话 sticky；这不是旧 sticky 泄漏。排查时应同时核对 `auth_credential_blocks` 和推理端响应，
不能只看 usage 接口返回的周额度。

2026-09-16 实测的 `opencode-go/deepseek-v4.1-flash` 案例中，周用量 6% 的 credential 11
返回 HTTP 403 `RegionError`，要求在对应 OpenCode Go workspace 显式同意中国区托管；周用量
24% 的 credential 7 对同一带 `x-opencode-session` 的最小请求返回 HTTP 200。因此新会话最终
固定到 credential 7 是失败回退，不是 Site D 没有释放新会话粘性。完成账号侧 opt-in 后，
新会话才能按额度排名实际使用 credential 11。

## 4. 配套配置(`~/.omp/agent/config.yml`)

```yaml
retry:
  fallbackChains:
    "google-antigravity/*":
      - "opencode-go/deepseek-v4.1-flash:high"
  modelFallback: true        # 开启 fallback
  usageAwareFallback: true   # 在硬 429 前读取可靠额度报告
  usageReservePct: 15
  usageReservePolicy: auto   # 新会话无需交互确认，直接 fallback
```
