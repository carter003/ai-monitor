# 本仓库的 Agent 工作约定

保留用户已有的未提交修改。Rust 看板、用量采集器及各本地集成按各自目录维护；不要为了一个集成的变更重写其他模块。

## 全局 Codex CLI 升级

收到“升级 Codex CLI”或“迁移 `/clear`、Plan 模型保留补丁”的请求时，按[全局 Codex CLI 升级报告](integrations/codex-cli-local/README.md)操作。`codex update` 更新官方基线；若用户已经执行过，从核对已安装版本开始，随后迁移补丁、构建、验证并安装到当前用户的全局入口。

## Codex Web GPT 本地修复与升级

用户指定本仓库作为 Codex Web GPT 本地补丁和后续升级的维护入口。收到“升级 Codex Web GPT”“更新本地 Native2 修复”等请求时，按本节执行完整的准备、验证、打包、安装和健康检查；不要直接换成未携带补丁的官方包。

默认使用 `stage` 预置更新，保留当前进程和 Runtime；常规路径是由用户正常退出 Launcher 并重开后，再执行 `finalize` 和实机验收。**只有用户明确提出重启 `codex-web-gpt` 时，才可按其指令执行重启**（优先正常退出并重开；需要命令行时用 `systemctl --user restart codex-web-gpt`），并在重启后核对实际 bundleId、健康状态和文件哈希。无论当前 Agent 是否通过该服务通信，未经用户明确指令都不得自行 stop、restart、drain、shutdown 或 kill 来切换它；健康计数为零并不能证明可以安全中断。GNOME 还可能把 Electron/daemon 移到独立 app scope，仅停 systemd 入口不等于它们已经退出。不得通过反复停止、跨 scope 杀进程或直接覆盖正在运行的 Runtime 来绕过。

### 文件与职责

- 维护入口：`integrations/codex-chatgpt-web-local/README.md`、`upgrade.mjs`。
- 可迁移源码补丁：`integrations/codex-chatgpt-web-local/local-fixes.patch`。这是修复内容的持久来源，不能只在外部源码工作区保留修改。
- 安装后的真实验收：同目录 `live-smoke.ts` 检查真实 POST 和原始 JSON，`review-smoke.mjs` 检查 Codex CLI 审查、实际工具读取和已知缺陷。两项默认使用 GPT-6 Sol Instant（`low`）；只有显式指定或用户要求时才用 GPT-6 Pro / GPT-5.6 Pro。调用方式见维护入口；两项都会真实调用模型。本机 `review-smoke.mjs` 默认使用标准 `codex` 原生入口，可用 `CODEX_BIN` 覆盖。
- 本机源码工作区：`~/.local/src/codex-chatgpt-web`；升级前检查分支和工作树，不得用 `reset --hard`、强制 checkout 等方式丢弃修改。
- 根因与脱敏实验记录：`docs/codex-native2-turn-token-root-cause.md` 的 §10–§11，以及 `docs/codex-native2-turn-token-verified-evidence.json`。§1–§9 是被更正的初版调查，不应据此重新认定“编辑器已排除”或实施模糊 token 匹配。
- 自定义安装包、来源和校验信息以升级脚本生成的安装凭据为准。检查具体路径的方式见维护入口的 `status` 命令；不能只凭 `/healthz` 的版本号认定补丁已经运行。
- 当前修复的继续检查点：`integrations/codex-chatgpt-web-local/verification.json`。首次包、异步 review 修复包及 Native2 参数末字符复制核对补丁（`bundleId=371e97f693093f090daeee175be89a33c3e57a9fb5761ab2bf1b91595e459c8f`，见报告 §11.1–§11.5）均已完成 `installed-and-verified`。Activity 收尾换 key 字面包装重绑修复包（`patchSha256=d82aa743ea9d3db4c48f6e7112fbd8250483cece3500fa9eedceb0a04f3cab31`，`bundleId=4343f6d3afc2a7e8a5a00bad75abcaa95eacd414b4615c175b4f7ff69122cf60`，见报告 §11.6）也曾完成全量验证与两项实机回归。当前版本升级到上游 v6.2.0（commit `fc701f6fbcff9fb65f679445389aca0c60410271`），保留本地修复（`patchSha256=930c48063ff56e4ef9c95a8336a68a8231df0532e0da7e35ea5ea8cb5de9dd91`，`bundleId=db3cbf7d21ea3cb541537edaba135d8a43c60141eed44f80dfb151968d23e89f`），已完成全量验证、打包、`stage`、在用户明确授权后 `systemctl --user restart codex-web-gpt`、`finalize` 和两项实机回归，状态为 **`installed-and-verified`**。收到状态查询或继续指令时先核对实际状态，不重复构建、不丢弃现有源码。
- **2026-11-27 起该服务已按用户要求停用，Codex 直连官方 API。** 已执行 `route disconnect`、`systemctl --user stop` 和 `disable codex-web-gpt`，autostart 项改名为 `.disabled`。6.2.0 的安装包、活动 receipt、Runtime、源码和补丁保留；6.1.19 的历史安装目录、旧 Runtime 和旧源码副本已按用户确认清理，范围见维护入口“停用后的清理”。服务未运行时 `status` 因 `/healthz` 不可达返回非零，属于预期，不得据此重新构建、`stage`、`install` 或自行启动服务。用户要求恢复时按维护入口“当前状态”一段重新启用，并在 Launcher 登录完成后执行 `route connect`。实机回归和 `live-smoke.ts`、`review-smoke.mjs` 都需要该服务运行，停用期间不要执行。`verification.json` 的 `history` 中 6.1.19 条目的本地凭据已删除，不要再尝试对它们执行 `rollback --receipt`。
- **停用该服务时，`route disconnect` 不够。** 它只还原 `config.toml`，不清 `~/.codex/models_cache.json`，也不重启 `~/.codex/app-server-daemon` 的常驻进程和已打开的 TUI；旧进程会持续重试 `127.0.0.1:17841`，表现为 Codex 卡顿。必须同时删除 `models_cache.json`、结束旧 TUI 并重启守护进程，再验证直连。详细步骤见维护入口“当前状态”一段。

### 必须保留的修复行为

1. Native2 连接器 mention 保留为独立节点；在 Markdown 编辑器中把完整 prepared prompt 放入真正的 `code_block`。仅插入字面三反引号、内联 `<code>` 或普通粘贴不能视为修复。
2. 插入通过 DOM 的 `textContent` 转义协议数据，在 PRE 前加入真实空白段落作为插入边界，避免 Chromium 把首行合并到 mention 段落；校验代码块节点和完整文本。回归 fixture 必须包含真实段落中的 mention。同步适配提交后的包装识别，不能放宽消息归属或忽略额外文本。
3. 验收必须覆盖实际 POST：签发的 `turn_token` 逐字符一致，嵌套 context JSON 能直接解析，首尾/连续下划线、连字符、引号、反斜杠和内嵌围栏均保持原样。DOM readback 一致不能替代发送边界的验证。
4. 结构化 JSON 输出保留最终原始生成文本，避免 HTML → Markdown 转换增加反斜杠或丢失内联代码字符；不得全局删除反斜杠来“修复”JSON。Codex CLI 会把合法 review JSON 渲染成人类可读审查文本，JSON 验收应检查 bridge 渲染前的原始输出，不能把正常的 CLI 文本渲染误认成 JSON 损坏。
5. 保留 Broker 的精确 token 匹配、严格格式校验、任务归属和完成栅栏。不得引入猜测 token、编辑距离匹配、延长已结束回合的权限等绕过办法。

### 升级流程

1. 阅读维护入口并运行 `status`，确认当前官方基线、补丁摘要、安装包和运行时清单。用户指定版本时使用指定版本；未指定时先查上游正式 release，记录实际选定的版本和提交。
2. 用维护脚本准备目标版本，先执行补丁兼容性检查。出现冲突时，阅读目标版本源码迁移补丁；若上游已实现等效修复，用回归测试证明后删除已被替代的补丁部分。不得强行忽略补丁失败。
3. 使用目标版本 `packageManager` 锁定的 Bun 和两份 lockfile 安装依赖，运行相关回归、上游验证、Runtime/Launcher 构建与打包 smoke。浏览器回归应使用可用的 Chromium，不能把 skipped 当成已验证。Linux 打包必须运行上游兼容 libnotify / 自有 AppImage 工具准备，并通过最终安装包 ABI 检查；库缓存来源和摘要见维护入口与构建凭据，不能直接使用下载工具集中的旧库。
4. 重新导出并检查 `local-fixes.patch`，使其可在干净的目标版本上应用。更新安装来源记录和维护说明。
5. 用户已要求升级或安装时，完成检查后执行 `stage`，无需重复索取同一次任务的授权。它备份入口、配置和 Runtime，将下次启动入口指向不可变自定义 AppImage，不停止当前进程，也不替换正在运行的 Runtime。检查 `status` 应明确显示 `awaiting-user-restart`，不能把预置当作已生效。
6. 常规情况下告知用户需要正常“退出并重开” Launcher。**若用户明确要求由 Agent 重启 `codex-web-gpt`，可执行 `systemctl --user restart codex-web-gpt`**，但仍须在重启后核对 bundleId、健康状态和文件哈希；未经要求不得自行停服务。收到用户恢复后的继续指令，先运行 `status`；观察到目标 bundle 后运行 `finalize`，核对运行进程、健康状态、完整文件哈希及 CLI 入口，再执行隔离的 Native2 调用和审查回归。若还是旧 bundle 且用户未要求重启，不得自行停服务。上游目标版本必须保留“单实例锁先于 Runtime 安装”的顺序；迁移时核实这一条件。未激活时 `rollback` 仅取消预置；激活后的回滚另需遵守相同通信约束。实机回归默认使用 GPT-6 Sol Instant（`low`），避免消耗 Pro 次数；只有显式指定或用户要求时才用 Pro。
7. 最终说明上游版本、补丁标识、验证结果、安装位置及回滚方式，并更新本节引用的维护记录。

不要直接修改 `~/.codex-chatgpt-web/versions/*/app/`、绕过 Runtime 清单校验，或依赖指向旧版本的静态 CLI 软链接。官方内置更新不会自动携带本地源码补丁；后续升级继续通过本仓库维护入口执行。

原始浏览器 POST/SSE、账号配置、控制凭据和真实 token 不进入仓库。持久诊断记录只保存脱敏摘要、字符长度和必要的哈希。
