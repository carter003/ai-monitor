# Codex Web GPT 本地补丁维护

入口为 `upgrade.mjs`，补丁为本目录的 `local-fixes.patch`。适用于本机 Linux AppImage、Node.js 22+、用户级 `codex-web-gpt.service`。脚本不选择 latest，不 reset、stash 或覆盖已有 dirty source；所有子进程使用 `spawn(executable, argv)`，不拼接 shell 命令。

**当前状态（2026-11-27）：按用户要求已停用。** Codex 已通过 `route disconnect` 解除本地 bridge 路由，`codex-web-gpt.service` 已停止并禁用，GNOME autostart 项 `~/.config/autostart/dev.codexwebgpt.launcher.desktop` 已改名为 `.disabled`。安装包、receipt、Runtime、源码和补丁均保留；此时 `status` 因 `/healthz` 不可达返回非零并显示 `drift: "fetch failed"`，这是服务未运行的预期结果，不是安装损坏。

**`route disconnect` 只改 `config.toml`，不清模型缓存。** bridge 的模型列表写在 `~/.codex/models_cache.json`，停服务后必须备份并删除该文件，否则 `/model` 继续显示缓存内容。已删除，备份为 `~/.codex/models_cache.json.bak-before-codex-web-gpt-off`。同目录的 `models_cache wm.json` 是 2026-08 的旧文件，与 bridge 无关，未改动。

**停服务后还必须重启 Codex 常驻进程。** `~/.codex/app-server-daemon` 的 `app-server --managed-daemon` 和所有已打开的 TUI 都在内存里持有旧的 `openai_base_url`，会持续重试 `http://127.0.0.1:17841/v1/models`，表现为进入 Codex 卡顿。处理方式：结束全部旧 TUI，`SIGTERM` 守护进程本体；保留 `app-server daemon pid-update-loop`，它负责在下次启动 Codex 时按新配置重新拉起守护进程。

直连验证（守护进程重启后）：`codex login status` 为 `Logged in using ChatGPT`，实机 `codex exec` 7.7 秒返回 `provider: openai` 和 `OK`；重启后日志中 `17841` 引用 0 次、模型刷新失败 0 次（重启前最后两分钟 73 次）。`service_tier = "fast"` 直连可用，未改动。新拉取的 `models_cache.json` 为 12 条、11 个可见，`etag` 与 bridge 写入的那份相同，说明这是账号本身的官方目录，不是 web 专属模型。

重新启用：把 `.disabled` 改回原名，执行 `systemctl --user enable --now codex-web-gpt`， Launcher 完成登录后执行 `~/.local/bin/codex-chatgpt-web route connect`，再完全重启 Codex。重新启用前当前 Agent 的通信不依赖该服务；停服务约束针对的是通信依赖它的会话。

## 修复内容与源码基线

首次修复基于 [miuuyy/codex-chatgpt-web v6.1.19](https://github.com/miuuyy/codex-chatgpt-web/tree/v6.1.19)，提交 `8e9fac038dcd998bf23716df2b08379a15854ec9`。当前维护版本为 [v6.2.0](https://github.com/miuuyy/codex-chatgpt-web/releases/tag/v6.2.0)，提交 `fc701f6fbcff9fb65f679445389aca0c60410271`，补丁 SHA-256 `930c48063ff56e4ef9c95a8336a68a8231df0532e0da7e35ea5ea8cb5de9dd91`；目标源码使用 `.state/sources/6.2.0-930c48063ff5-qZgs5V` 的独立 clone。本机工作区 `~/.local/src/codex-chatgpt-web` 的 `carter/native2-literal-transport-v6.1.19` 分支及其既有修改保持原样；升级时以目标 tag 和升级凭据为准，不能继续假定旧提交是最新版本。

- 输入：`browser-worker.ts` 在 Native2 mention 后插入 `<p><br></p>` 边界与真正的语义代码块，使用 `textContent` 转义内容，并校验完整文本与节点。`browser-user-message.ts` 精确匹配真实前端渲染出的 mention 段落内尾随空格 `<span>`、mention 段落与 `PRE` 之间的单个 `blank-lines` 节点及唯一代码块正文；前导/尾随空行、额外文本、错误 mention 和多块内容仍拒绝。
- 工具参数：Native2 的请求末尾单列 token 的末四字符供复制核对。Broker 拒绝未知 token 后只提示调用方自己提交的末四字符；只有完整签发值可以进入原有鉴权流程。
- 输出：`raw-final-json.ts` 只在内存中收集当前请求的 assistant/final 原始文本，要求完整成功结束、DONE 和合法 JSON。`stream-diagnostics.ts` 管理请求与页面归属；长工具回合的初始 POST 可能在工具执行期间结束，worker 还监听同一页主 frame 的会话 GET。它比对页面会话、已绑定 turn 的用户与最终消息 ID、服务端当前节点和最近用户祖先，验证 assistant/final 已成功结束并解析 JSON。只在 DOM 完成检查及 Broker 完成栅栏通过后输出原始文本。显式 schema 与完整 JSON 保留原始文本；普通回复以完整 JSON 开头再接解释时可走 Markdown 路径。
- 回归：覆盖字面内容插入、焦点保护、消息归属、Activity 临时分组无初始用户气泡时的收尾折叠气泡展开重绑、SSE 分帧与覆盖、页面重绑、网络失败、内容大小限制及原始 JSON 的引号、反斜杠和反引号。另修正上游测试设施：导航 fixture 的子视图退出清理；实际需约 74 秒的 18 组 Think 组合测试的 120 秒预算；并行浏览器回归下 Bun 1.4.0 `rejects.toMatchObject` 同步符号化阻塞对应的测试超时预算；离屏视口测试先完成缩放绘制，再显式检查按钮可见、可用和菜单已打开，避免依赖离屏动画帧。全部原断言保留。

Broker token 校验、工具权限、任务归属与完成栅栏没有放宽。完整原始浏览器记录不纳入补丁。

## 命令

从 ai-monitor 根目录执行。已有补丁工作区可以直接验证外部源码；后续升级先 prepare。

```bash
node integrations/codex-chatgpt-web-local/upgrade.mjs status

# 首次：源码与已导出的补丁必须完全一致。
node integrations/codex-chatgpt-web-local/upgrade.mjs verify \
  --source /home/carter003/.local/src/codex-chatgpt-web \
  --browser /absolute/path/to/chromium \
  --electron /absolute/path/to/electron

node integrations/codex-chatgpt-web-local/upgrade.mjs build

# 预置下一次启动的包；不停止当前通信，也不覆盖运行中的 Runtime。
node integrations/codex-chatgpt-web-local/upgrade.mjs stage
node integrations/codex-chatgpt-web-local/upgrade.mjs status

# 用户正常退出并重开 Launcher 后，再执行：
node integrations/codex-chatgpt-web-local/upgrade.mjs finalize

# 未激活时只取消预置；已安装后的回滚会停服务，须遵守通信约束。
node integrations/codex-chatgpt-web-local/upgrade.mjs rollback

# 指定凭据，也适用于安装中途失败后的恢复。
node integrations/codex-chatgpt-web-local/upgrade.mjs rollback \
  --receipt /absolute/path/to/receipts/ID/receipt.json
```

`--browser` / `--electron` 也可分别用 `CHATGPT_DOM_TEST_BROWSER` / `LAUNCHER_TEST_ELECTRON` 提供。Electron 必须是与上游 launcher 依赖版本一致的、完整发行目录中的可执行文件；不要传 AppImage 文件。脚本优先复用工作区已有的完整 Electron dist，否则将指定发行目录复制到该工作区的 `launcher/node_modules/electron/dist`，避免上游安装器写入原 Electron 目录。浏览器与 Electron 缺失会明确失败，不能将相关测试 skipped 视为验证成功。

`verify` 在 root 和 launcher 分别执行 `bun install --frozen-lockfile`，再执行上游完整 `bun run verify`。Bun 版本严格取自上游 `packageManager`：当前为 **1.4.0**；先找配置/已安装 runtime 的 bundled Bun，再找本机匹配版本。全局 1.4.4 不会替代 1.4.0。选择结果见 `status` 和 `.state/current.json`。

`build` 要求已成功 verify 且源码/补丁没有变化；重新 frozen install，执行上游 `app:package`，解包实际生成的 AppImage，以上游 `validateRuntimeBundle` 核验文件哈希和 manifest，再执行上游隔离的 `app:smoke`。产物路径当前为：

```text
/home/carter003/project/ai-monitor/integrations/codex-chatgpt-web-local/.state/sources/6.2.0-930c48063ff5-qZgs5V/launcher/artifacts/codex-web-gpt-6.2.0-linux-x64.AppImage
```

Linux 打包还需要上游 CI 的兼容 `libnotify` 准备步骤；直接使用 electron-builder 下载工具集中的旧库会被最终 ABI smoke 拒绝。脚本先检查已有库的 SHA-256、原生 ELF 架构及所需符号，再运行上游 `prepare-linux-appimage-tools.cjs`，只修改工作区自有工具副本。最终安装包仍必须通过 `smoke-linux-appimage-symbols.sh`，不跳过符号或依赖检查。

本机首次使用原官方 v6.1.19 AppImage 中的 `libnotify 0.8.7`，原包 SHA-256 为 `23473a9156147282b3e516e8210c386a43d41f556be674e8b62285832880180c`，库 SHA-256 为 `fe960926dd4681a35a3ebb9b730cd29d2a50763519fa37a5f86a5ca5698c8ae4`；LGPL-2.1 许可随原包及新 Runtime 的 LICENSES 保留。库按内容摘要存入 `.state/linux-inputs/`，`current.json` 在 prepare 新版本后仍可复用；每次重建重新校验，路径和摘要写入构建及安装凭据。

新机器没有缓存时，脚本执行上游 `scripts/prepare-linux-libnotify.sh`，从固定源码地址按固定摘要构建；需要 `meson`、`ninja`、`pkg-config`、GLib/GDK Pixbuf 开发依赖和 `binutils`。也可显式提供 `CODEX_WEB_GPT_LINUX_LIBNOTIFY=/absolute/path/to/libnotify.so.4`，仍须通过相同检查。目标 Electron 或上游准备脚本变化时重新评估 ABI，不能因为缓存曾可用就忽略最终 smoke。

## 后续升级

```bash
# 使用已有的上游源码 clone；没有时先用 git clone 取得。
# --version 必填，示例版本需由维护者明确选定。
node integrations/codex-chatgpt-web-local/upgrade.mjs prepare --version 6.2.0
node integrations/codex-chatgpt-web-local/upgrade.mjs verify \
  --browser /absolute/path/to/chromium --electron /absolute/path/to/electron
node integrations/codex-chatgpt-web-local/upgrade.mjs build
node integrations/codex-chatgpt-web-local/upgrade.mjs stage
node integrations/codex-chatgpt-web-local/upgrade.mjs status
# 用户正常退出并重开后：
node integrations/codex-chatgpt-web-local/upgrade.mjs finalize
```

`prepare` 校验显式版本后，在 `--source` clone 中只执行 `git fetch --no-tags origin refs/tags/vVERSION:refs/tags/vVERSION`，不强制更新已有 tag，也不改工作树。随后创建独立的干净 clone、checkout 该 tag 的确切 commit，先 `git apply --check`，再 `git apply --index`。不会使用三方合并、忽略冲突或强行 fuzzy 修补。源码 clone 不存在时明确报错，需先用 `git clone` 取得。失败的独立 clone 留在 `.state/sources/` 供审查。

补丁不能应用时，必须在独立 clone 中审查上游改动、迁移补丁并运行回归。主工作树和其他用户修改保持原样。直接验证修改过的 source 时，脚本要求下面命令的输出与 `local-fixes.patch` 字节完全一致；新增回归文件需先逐个审查并 `git add -N PATH`，使它们进入 diff。所有未被忽略的未跟踪文件都会被拒绝。

```bash
# 在外部 source 中执行；导出到 ai-monitor 的绝对补丁路径。
git diff --binary --no-ext-diff --no-textconv v6.2.0 -- > /absolute/path/to/ai-monitor/integrations/codex-chatgpt-web-local/local-fixes.patch
```

更新补丁或源码后，重新 `verify --source PATH` 再 `build`。prepare/verify/build 的版本、upstream commit、source HEAD/diff 摘要、patch SHA-256、Bun、验证结果及 AppImage 哈希保存在 `.state/current.json`。失败的 verify/build 会清除旧成功标记，防止误用旧产物。

## 安装、停服务与凭据

上游 v6.2.0 的 `main.cjs` 在 Runtime 安装前取得单实例锁（已核实 `main.cjs:1136` 的 `requestSingleInstanceLock()` 先于 `runtimeRootProvider()`），因此旧 GUI 还开着时再启动只会唤起旧 GUI；**必须正常退出，再重新打开**，仅关闭窗口、最小化或重复点图标不等于完成切换。

重开后，上游 Launcher 从新 AppImage 安装合法 Runtime。`finalize` 只核对实际进程、目标 `bundleId`、完整 manifest 文件哈希、AppImage 和 wrapper 摘要，保存安装完成凭据；它也不停止或启动服务。若还在运行旧包，明确拒绝确认。`status` 显示 `awaiting-finalize` 时再执行该命令。未激活阶段的 `rollback` 只恢复旧入口并取消预置，保留当前进程与配置。

下面的 `install` 是**会停止并重启服务的操作**，仅适用于通信已独立于该服务的维护终端；不要在本次自依赖对话中调用。GNOME 会把 Electron 和 daemon 移到单独 app scope，`systemctl stop` 可能只结束入口而没有结束真实 daemon；脚本会拒绝覆盖这种仍在运行的 Runtime，不能改成跨 scope 强杀。

`install` 先检查 `/healthz` 的 `active_http_turns === 0` 和 `active_browser_turns === 0`；计数缺失、繁忙或健康不可达均失败。备份当前入口、完整 runtime、私有 config 后，通过带控制凭据的 `/admin/drain` 禁止新回合，再次确认空闲和进程身份，才执行 `systemctl --user stop codex-web-gpt`。旧 daemon 必须确实退出；否则不换入口、不替换 runtime，也不额外 kill 任何任务。

AppImage 安装至不可变目录：

```text
~/.local/lib/codex-web-gpt/local/<version>-<完整patch-sha256>/Codex Web GPT.AppImage
~/.local/lib/codex-web-gpt/local/<version>-<完整patch-sha256>/receipts/<ID>/receipt.json
~/.local/lib/codex-web-gpt/local/active-receipt.json
```

receipt 记录 upstream commit、patch SHA-256、artifact SHA-256、完整 runtime manifest / bundleId、旧 runtime 身份、入口/配置备份及运行状态。原官方 AppImage 和原 `run-appimage` 保留。`~/.local/bin/codex-web-gpt` 原子替换为指向自定义 AppImage 的 wrapper；`~/.local/bin/codex-chatgpt-web` 原子替换为动态 wrapper，每次读取 config.releaseVersion，使用对应版本目录里的 bundled Bun 和 `app/cli.js`，不再固定在旧 6.1.18 软链接上。

随后 `systemctl --user start codex-web-gpt`。上游 launcher 负责从自定义 AppImage 安装合法 runtime；本脚本不修改 runtime 内的 JS。**同版本包也会替换同一路径的 runtime，因此必须先停旧服务，不能只复制 JS 或凭版本号判定成功。** 安装成功要求健康接受新回合、daemon 为新进程、`/proc/PID/cmdline` 和 config 指向正确入口，并且实际 runtime 的完整文件哈希和 `bundleId` 与构建 manifest 一致。版本相同但 bundleId 不同会失败。

`status` 只读检查入口、活动 receipt、artifact 哈希、实际 runtime 身份及健康计数；存在活动 receipt 且发生漂移时返回非零。无本地安装凭据时也会展示可取得的当前 runtime bundleId。锁文件阻止并发维护；仅在确认其中 PID 已退出后手工删除遗留 `.upgrade.lock`。

## 回滚与安装失败恢复

正常 `rollback` 要求当前入口、私有 config 和实际 runtime 符合活动 receipt，健康空闲后 drain/stop，确认相关进程已退出，再恢复备份。恢复完整 config 是为了让旧 launcher 接受旧 releaseVersion；安装后用户已修改配置时，正常回滚会拒绝覆盖，需先审查合并。旧软链接按原 target 恢复，普通 wrapper 按内容和 mode 恢复。旧 runtime 从经过 manifest 校验的备份恢复，随后启动旧服务，并再次核对健康、新进程和旧 bundleId。移开的 runtime 保留为 `.displaced-*` 供恢复检查。

若安装停在 `switching` / `starting` / `failed-after-stop`（或回滚停在 `rolling-back` / `recovery-failed`），同一条 **`rollback --receipt PATH` 就是恢复命令**，不依赖新包已经成功启动：

- 每个入口分别允许处于原备份状态或本次预期新 wrapper 状态，因而支持只切了一半的入口；其他用户编辑会被拒绝。
- 若 health 可达，仍必须确认空闲、身份属于本次旧/新 bundle，经过 drain 才停服务。
- 若 health 不可达，必须收到连接拒绝、`systemctl --user is-active codex-web-gpt` 明确为 inactive/failed、receipt 记录的 daemon 已消失，而且 `/proc` 中没有该 core-home 的 runtime/launcher；超时、活跃服务、未知状态或仍存活进程均拒绝恢复。离线路径不执行 stop/kill。
- 恢复前校验全部备份；中断升级产生的 config 先另存为私有 `config-before-recovery-*.json`，再恢复旧配置、runtime 和入口，启动旧包并核验 bundleId。

失败时脚本打印 receipt 路径。没有自动杀任务/重启循环；如服务仍 active 但 health 不可达，恢复会明确拒绝，须先排查进程状态，不能以“health 不通”推定没有任务。原配置备份可能含凭据，只存于本机权限受限的安装备份目录，不得复制进仓库。

## 路径覆盖与本地检查

所有命令支持 `--state-dir PATH`、`--core-home PATH`、`--install-root PATH`、`--bin-dir PATH`、`--source PATH`、`--patch PATH`；回滚使用与安装相同的路径参数。默认 source 为 `~/.local/src/codex-chatgpt-web`，core-home 为 `CODEX_CHATGPT_WEB_HOME` 或 `~/.codex-chatgpt-web`，状态目录为本目录 `.state/`（已忽略）。build/install 使用已验证 state；显式指定的 source/version 必须与其匹配。不要把恢复备份、控制 token、原始 POST/SSE 或账号数据提交到仓库。

```bash
node --check integrations/codex-chatgpt-web-local/upgrade.mjs
node --test integrations/codex-chatgpt-web-local/upgrade.test.mjs
```

维护脚本测试只使用临时 Git 仓库、假 Bun、假 systemctl 和测试 daemon，覆盖 dirty source 隔离、补丁冲突、版本锁定、活动任务拒绝、入口恢复和中断安装后的离线恢复；另验证忙碌时预置不改变进程或 Runtime、取消预置不停止进程，以及真正换包后才可 finalize，共 13 项。它们不操作真实服务，不替代上游 verify、AppImage smoke 或实际安装后的 Native2 调用验收。

安装后的真实模型验收通过 `live-smoke.ts` 执行：

```text
<已安装runtime/bun> run integrations/codex-chatgpt-web-local/live-smoke.ts [sourcepath]
```

例如使用 `~/.codex-chatgpt-web/versions/6.2.0-linux-x64/runtime/bun`；省略 sourcepath 时使用外部源码 checkout。它使用安装中的 helper，默认以 GPT-6 Sol Instant（`low`）真实调用一次 inventory（不执行 shell），检查真实尾 `_` token 的 POST 字节和 raw JSON，将脱敏证据写入 `.state/live-smoke.json`。需要 Pro 时用 `CHATGPT_WEB_SMOKE_REASONING=max` 显式指定。**这一步会真实调用模型，在已授权的升级任务中执行；它不是单元测试。**

再运行真实 Codex CLI 审查回归：

```bash
node integrations/codex-chatgpt-web-local/review-smoke.mjs
# 默认 GPT-6 Sol Instant（low）；仅在用户明确要求时切到 Pro：
# CODEX_REVIEW_MODEL='chatgpt-web/gpt-6-pro' CODEX_REVIEW_REASONING=max node integrations/codex-chatgpt-web-local/review-smoke.mjs
```

脚本在权限受限的临时目录创建独立 Git 仓库，将乘法改成错误的加法，默认使用 GPT-6 Sol Instant（`low`）发起只读 `review --uncommitted` 审查指令；可用 `CODEX_REVIEW_MODEL` / `CODEX_REVIEW_REASONING` 显式指定 Pro。它继承用户现有 CLI 权限配置，不强制覆盖沙箱或审批设置；在当前 CLI/bridge 组合中额外指定只读沙箱会触发可信环境缺少网络字段的独立兼容问题，该问题不属于本次序列化修复。

**Codex CLI 会把合法 review JSON 渲染成审查文本，`-o` 文件不应按 JSON 检验。** 脚本为这一次子进程建立临时 loopback 转发，保持 HTTP 字节和认证头不变，只在 bridge 的 `response.completed` 中读取最终 assistant 文本，验证原始 JSON 及 `overall_correctness` 字段；不记录请求或认证内容，也不改全局路由。验收还要求至少一次成功的命令读取、审查发现该文件的已知缺陷、CLI 正常展示审查，以及夹具源码未被审查过程改写。转发仅用于测试，关闭时不影响真实服务。

脱敏结果写入 `.state/review-smoke.json`；失败时仅打印私有诊断目录，原始输出和 CLI 事件不进入仓库。可用 `CODEX_BIN` 指定 CLI 可执行文件，`CODEX_REVIEW_MODEL` / `CODEX_REVIEW_REASONING` 指定模型与推理档位。
Herdr TPS 已迁移为原生 Hooks 与只读 Rollout 观察器；`codex` 使用官方原生入口。`review-smoke.mjs` 默认运行标准 `codex`，仍可用 `CODEX_BIN` 覆盖。

**官方内置自动更新不会携带本地 patch，可能替换 AppImage、wrapper 或同版本 runtime。** 更新后必须运行 `status` 检查 bundleId 漂移；以后通过本流程迁移补丁、验证并重新安装。不要仅凭 `/healthz` 返回 6.2.0 就认定本地修复仍在运行。
