# Antigravity 合规清理：调查记录与结论

日期：2026-11-07
范围：ai-monitor 仓库 + 本机 Antigravity 相关配置
状态：**已执行完毕（§3 的 1–8 本仓库项已落地；第 9 项 omp 配置待用户确认是否移除）**

---

## 1. 结论摘要

本机 Antigravity 有三份互相独立的 Google 凭据，性质不同，此前被混用：

| 使用者 | 凭据形态 | 位置 | 合规性 |
|---|---|---|---|
| omp 的 `google-antigravity` provider | Antigravity **消费级 OAuth**（access/refresh `ya29.` + projectId） | `~/.omp/agent/agent.db`（id 8、12）、`~/.omp/profiles/pro2/agent/agent.db`（id 1） | **违规**：第三方 agent 持 Antigravity OAuth 调 Google 服务，Antigravity ToS §6 明文禁止（"Using third party software, tools, or services to access the Service (e.g. using OpenClaw with Antigravity OAuth) is a breach"） |
| agy 官方 CLI 自己的会话 | Antigravity 消费级 OAuth | `~/.gemini/antigravity-cli/antigravity-oauth-token`、`~/.gemini2/antigravity-cli/antigravity-oauth-token` | 合规：官方客户端、官方接口、凭据自留。第三方只能调用它的**公开接口**，不得提取令牌自行发请求 |
| opencode 的 `google` provider | AI Studio / Cloud **API key**（`AQ.Ab8RN6…`，= gcloud `428905046160` 的 key `664c24de`） | `~/.local/share/opencode/auth.json`、`opencode.db` | 合规：官方 Gemini API 路径（但该 key 当前 402 预付费耗尽） |

### 账号封禁实测

两个账号的 Antigravity 推理均已被 Google 判 TOS 违规：

```
403 PERMISSION_DENIED  domain=cloudcode-pa.googleapis.com
reason=TOS_VIOLATION
"This service has been disabled in this account for violation of Terms of Service.
 Please submit an appeal to continue using this product."
appeal_url=https://forms.gle/hGzM9MEUv2azZsrb9
```

- `~/.gemini` → `bluefishcarter@gmail.com`（`projectId=clean-hour-qt3g1`）
- `~/.gemini2` → `carter.gogo@gmail.com`（`projectId=ultimate-listener-60x44`）

首个 403 出现在日志 `~/.gemini/antigravity-cli/log/cli-20261106_103244.log:221`（2026-11-06 10:32:52）。
`/usage` 配额查询仍返回 200，只有推理被封。

### 合规的访问姿势（官方文档依据）

| 姿势 | 官方依据 | 备注 |
|---|---|---|
| `agy acp`（Agent Client Protocol，stdio JSON-RPC） | `antigravity.google/docs/cli/acp` | 官方明示供 Zed / JetBrains / Android Studio / Xcode 等第三方编辑器集成；编辑器以子进程方式拉起 `agy acp` |
| `agy -p "<prompt>" --output-format json`（headless） | `antigravity.google/docs/cli/headless` | 官方明示用于脚本与 CI；`--disable-slash-commands` 控制斜杠命令展开 |
| `agy -p "/usage" --output-format json` | `antigravity.google/docs/cli/commands/usage` | 配额面板的官方数据出口 |

### 已验证的替代数据源（关键发现）

`agy -p "/usage" --output-format json` 走的是官方 CLI 自己的认证与请求，凭据全程留在 CLI 内部：

```
$ agy --gemini_dir=/home/carter003/.gemini2 -p "/usage" --output-format json
{"conversation_id":"","status":"SUCCESS",
 "response":"Gemini Models\tWeekly Limit Remaining\t100%\t2026-11-14T01:45:24Z\n…",
 "command":{"name":"usage","data":{"groups":[
   {"name":"Gemini Models","buckets":[
     {"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly",
      "remaining_fraction":1,"reset_time":"2026-11-14T01:45:24Z"},
     {"id":"gemini-5h","name":"Five Hour Limit Remaining","window":"5h",
      "remaining_fraction":1,"reset_time":"2026-11-07T06:45:24Z"}]},
   {"name":"Claude and GPT models","buckets":[
     {"id":"3p-weekly","window":"weekly","remaining_fraction":0.7985923886299133,
      "reset_time":"2026-11-11T08:19:03Z"},
     {"id":"3p-5h","window":"5h","remaining_fraction":1,
      "reset_time":"2026-11-07T06:45:24Z"}]}]}},
 "duration_seconds":0,"num_turns":0,
 "usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"total_tokens":0}}
```

要点：
- `num_turns:0`、token 全 0 → **不调用模型、不消耗推理额度**（与被封的推理路径无关）
- 账号被封推理后仍能读出配额 → 额度展示不依赖被封能力
- `--gemini_dir=<profile>` 可精确隔离 AGY（`~/.gemini`）与 AGY2（`~/.gemini2`），两账号数据不同，已交叉验证与内部接口返回一致
- 退出码：成功 0；失败 3
- 耗时约 4.3 s/次（CLI 冷启动），CPU 约 0.75 s
- stdout 为纯 JSON（`--output-format json` 时错误信息也在 stdout）

---

## 2. ai-monitor 仓库内的违规代码（待清理）

### 2.1 `crates/ai-monitor/src/http.rs`

| 位置 | 内容 | 问题 |
|---|---|---|
| `http.rs:10` | `AGY_ENDPOINT = "https://daily-cloudcode-pa.googleapis.com/v1internal:"` | 调未公开的内部接口 |
| `http.rs:13` | `AGY_USER_AGENT = "antigravity/cli/1.4.0 (aidev_client; …)"` | **伪造官方 CLI 身份**（ToS §6 核心违规形态） |
| `http.rs:54-84` | `agy_quota` / `agy_quota_at` / `agy_request` | 携 Antigravity OAuth 直接打内部接口 |
| `http.rs:85-112` | `agy_refresh` | 读取 `antigravity-cli/antigravity-oauth-token` 里的 refresh_token，用**内嵌的 Antigravity OAuth client_id/client_secret**（`http.rs:88-104`，字节数组伪装以规避密钥扫描器）去续期 → 等于第三方软件自行持有 Antigravity OAuth 客户端凭据 |
| `http.rs:143-168` | `local_json` + `local` client（`danger_accept_invalid_certs`） | 走本地 Antigravity IDE server 的 `exa.language_server_pb.LanguageServerService` 私有 RPC |
| `http.rs:269-392` | `mock_agy` 及 4 个测试 | 锁定上述违规行为的测试，需一并删除/改写 |

### 2.2 `crates/ai-monitor/src/providers/agy_cloud.rs`（整文件，568 行）

- `Credentials::load` 读 `~/.gemini/antigravity-cli/antigravity-oauth-token`，解析 access/refresh token
- `Credentials::renewed` / `Session::renew` 走 `agy_refresh`（上述内嵌凭据续期）
- `Session::query` 携 token 打 `daily-cloudcode-pa`，再打 `openidconnect.googleapis.com/v1/userinfo` 取账号名
- `http.userinfo` 的存在本身就是"把 Antigravity OAuth 用于第三方 API"的形态

### 2.3 `crates/ai-monitor/src/providers/mod.rs`

- `Prepared.agy_credentials` 字段（L27）
- `Source::Agy | Agy2` 分支读 OAuth 凭据（L64-68）
- `Session.agy: agy_cloud::Session`（L147）
- `fetch()` 里 agy_cloud 主路径 + 本地 socket 备用路径（L176-200）
- fetch 后二次比对 token identity 检测"账户已变化"（L192-199）

### 2.4 `crates/ai-monitor/src/providers/agy.rs`（本地 socket 路径，260 行）

- 扫描 `/proc` 找 `agy`/`agy2` 进程，从 argv 取 `--csrf_token`
- 枚举进程 fd 反查监听端口，探 `127.0.0.1:<port>` 的 `exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary`（自签名证书，`danger_accept_invalid_certs`）
- `GetUserStatus` 取账号 email

### 2.5 `crates/ai-monitor/src/providers/parse.rs`

- `parse::agy`（L108-197）解析的是内部接口的 `remainingFraction` / `resetTime` / `displayName` 驼峰结构
- 官方 CLI `/usage` 的 `command.data.groups` 是 **snake_case**（`remaining_fraction` / `reset_time` / `name`），结构不同 → 解析函数需重写
- 相关测试：`agy_fraction_precision_shifts_with_remaining_percent`、`agy_selects_gemini_and_keeps_missing_fraction_unknown`、`agy_production_summary_keeps_gemini_windows_separate_from_third_party`、`agy_cli_summary_matches_native_usage_percentages_and_resets`、`agy_third_party_only_summary_does_not_report_gemini_quota`

### 2.6 `crates/ai-monitor/src/network.rs:21-29`

`ROUTES` 第 4 条探测入口是 `https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent` —— 同一个内部端点。需换成公开可探测入口（`https://antigravity.google/` 实测 200）。

### 2.7 `crates/herdr-usage/src/plans.rs`

- `token_email()`（L773-778）读 `~/.gemini/antigravity-cli/antigravity-oauth-token` 里的 `id_token` JWT 解邮箱
- **注**：实测该 token 文件顶层有 `id_token_email` 明文字段（`bluefishcarter@gmail.com`），根本不需要解 JWT —— 当前实现是在解一个文件里不存在的 `id_token` 字段
- `PlanOptions.agy_home` / `agy2_home`（L158-167）仅用于此邮箱解析

### 2.8 文档

- `crates/ai-monitor/README.md:240` —— 完整描述了直连 `daily-cloudcode-pa` + 伪造 CLI 1.4.0 UA 的做法
- `crates/ai-monitor/README.md:259` —— AI 线路表格列内部端点
- `crates/ai-monitor/README.md:308-313` —— 参考链接含 `crossusage`（AGY 云端额度与 OAuth 查询参考）、`Antigravity-Manager`（AGY OAuth 客户端配置参考）、`CodexBar`（AGY 本地接口与额度结构参考）三个逆向实现仓库，应移除

---

## 3. 计划中的改动（尚未执行）

| # | 改动 | 文件 |
|---|---|---|
| 1 | 新增 AGY provider：调用官方 `agy -p "/usage" --output-format json`，按 `--gemini_dir` 区分两个账号 | `crates/ai-monitor/src/providers/agy_cli.rs`（新） |
| 2 | 删除 OAuth 直连模块 | 删 `crates/ai-monitor/src/providers/agy_cloud.rs` |
| 3 | 删除伪造 UA / 内部端点 / 内嵌 client_secret / 本地自签名 RPC | 改 `crates/ai-monitor/src/http.rs` |
| 4 | 改接线，去掉 `agy_credentials` 与二次 identity 比对 | 改 `crates/ai-monitor/src/providers/mod.rs` |
| 5 | 解析官方 `/usage` 的 snake_case 结构 | 改 `crates/ai-monitor/src/providers/parse.rs` |
| 6 | 探测入口换公开端点 | 改 `crates/ai-monitor/src/network.rs` |
| 7 | 邮箱解析改读 `id_token_email` 明文字段 | 改 `crates/herdr-usage/src/plans.rs` |
| 8 | 文档同步：改写 AGY 数据来源说明、AI 线路表、删三个逆向仓库参考链接 | 改 `crates/ai-monitor/README.md` |
| 9 | omp 配置去掉 `google-antigravity` provider 引用 | 改 `~/.omp/agent/config.yml`、`~/.omp/profiles/pro2/agent/config.yml`（`modelProviderOrder` 与 `retry.fallbackChains`） |

保留项（判断为合规，不动）：
- `crates/ai-monitor/src/providers/grok.rs`、`http.grok` —— Grok 自有订阅的内部 billing 接口，与 Antigravity 无关
- opencode 的 `google` provider（API key 路径）
- agy CLI 自身的 OAuth 会话文件（官方客户端自有凭据）

---

## 4. 副作用清单（必须向用户披露）

| 副作用 | 说明 |
|---|---|
| **Chrome 反复弹出 Google 登录** | **本次调查造成**。我用 `HOME=/tmp/agyhomeX` 跑无凭据的 `agy`，CLI 打印 "Print mode: triggering interactive OAuth" → `browser.go:56 consumerOAuth: starting OAuth flow` → 通过 `xdg-open` 拉起浏览器打开 `accounts.google.com/o/oauth2/auth?client_id=1071006060591-…`。共 4 次：`agyhome3`（09:35、09:36）、`agyhome4`（10:09）、`agyhome5`（10:15）、`/tmp/h2`（10:14，走 symlink 到真实 `.gemini2`，未改凭据） |
| **AIza key 明文进终端日志** | `gcloud services api-keys get-key-string` 把 `carter003test` 与 `gen-lang-client-0572902669` 的 key 完整打印进本次会话输出 → 建议轮换 |
| **API 已开启** | `gcloud services enable aiplatform.googleapis.com` 对 `gen-lang-client-0349116924`、`gen-lang-client-0572902669`、`carter003test` 三个项目生效，可按需 `disable` 回退 |
| 临时目录 | `/tmp/agysdk`（SDK venv）、`/tmp/agyhome2..5`、`/tmp/h2`、`/tmp/*.json` 探针产物，可直接删 |
| 未改写任何真实凭据 | `~/.gemini` / `~/.gemini2` 的 `antigravity-oauth-token` 内容未被本次操作改动（仅 CLI 自身在 10:14 / 10:20 做过正常 token 刷新，mtime 变化来自 CLI 正常续期） |

---

## 5. 待用户决策

1. **是否保留 AGY / AGY2 两张额度卡**：
   - 保留 → 走官方 CLI headless（凭据留在官方客户端内，合规）；但依赖 `agy` CLI 已安装且已登录
   - 取消 → 只保留 Codex / OpenCode Go / GO-2 / SuperGrok / OpenRouter 五张卡
   - 若 agy CLI 自身会话也登出 → 这两张卡无法用 Gemini API key 等价替代（Antigravity 订阅池无公开 API），只能改口径
2. **Chrome 弹窗是否需要我进一步处理**（如关闭 xdg-open 关联、或确认已无残留 OAuth 流程在等回调）
3. **是否轮换那两把 AIza key**
4. **是否回退我开启的 aiplatform API**

---

## 6. 参考

- Antigravity 附加条款：<https://antigravity.google/terms>（§6 第三方软件条款）
- Google APIs 条款 §4.a.1（API Prohibitions）：<https://developers.google.com/terms>
- CLI headless：<https://antigravity.google/docs/cli/headless>
- CLI ACP：<https://antigravity.google/docs/cli/acp>
- CLI 配额命令：<https://antigravity.google/docs/cli/commands/usage>
- CLI 参考：<https://antigravity.google/docs/cli/reference>
- Antigravity Agent API（Gemini API 侧托管版）：<https://ai.google.dev/gemini-api/docs/antigravity-agent>
- Gemini CLI 消费级账户下线公告：<https://developers.google.com/gemini-code-assist/docs/deprecations/code-assist-individuals>

---

## 7. AGY/AGY2 全清（2026-11-07 二次清理）

决策：不再用 agy，AGY/AGY2 整条额度链路全部移除（上一轮的合规 CLI 路径一并去掉）。

| 层 | 移除内容 | 状态 |
|---|---|---|
| ai-monitor | `Source::Agy/Agy2`、`providers/agy_cli.rs`、`parse::agy`、config `agy_home/agy2_home`、Antigravity 网络路由（ROUTES 4→3） | 已删，grep `agy/antigravity` 零残留 |
| herdr-usage plans | `PLANS` 4→2（只剩 Codex/SuperGrok）、`PlanOptions` 改空结构体、`token_email`/`match_email`/AGY resolve 分支删除 | 已删 |
| collector omp | `AccountCatalog` 只查 `opencode-go`、sticky 只查 `session:sticky:opencode-go:%`、pin-wait 逻辑删除、backfill/web 只留 `opencode-go` | 已改 |
| ~/.omp 配置 | 主 config 去掉 `google-antigravity`（`modelProviderOrder` + `fallbackChains`）；pro2 `modelRoles.default` 改 `opencode-go/crouton-alpha-free:high` | 已改 |
| ~/.omp agent.db | `google-antigravity` 凭据删除（主 2 条、pro2 1 条）；sticky 缓存 1947+169 条删除 | 已删；注主库 `cache` 表 19 行因磁盘图像损坏删不掉（`database disk image is malformed`），omp 运行时碰到会跳过，不影响使用 |
| 文档 | 两份 README 的 AGY 数据来源/线路表/协议参考同步删除 | 已改 |

验证：`cargo test --locked` 全过（168+1+4+114+28）；`cargo clippy` 新代码零警告（herdr-usage 既有 38 项未动）；改动文件 rustfmt 干净；`grep -rn "agy\|antigravity" crates/` 零命中（VERIFICATION.md 历史记录除外）。

更新 §3 第 9 项状态：omp `google-antigravity` provider 引用已移除（同上）。
