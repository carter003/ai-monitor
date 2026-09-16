# Herdr 实时模型速度

该工具在 Herdr 左侧 Agents 区域使用三行紧凑布局：

```text
workspace tab
● codex 42
gpt-5.6
```

其中 `●` 是 Herdr 的 `state_icon` 状态指示灯：官方默认
`status_indicators = "dots"` 使用紧凑彩色圆点，具体颜色由主题决定。Herdr 0.9.0 官方源码中
工作中为黄色，完成但尚未读到（`done`）为 teal，已读空闲（`idle`）为绿色；需要无障碍的
形状区分时，可将 `status_indicators` 改为 `"symbols"`。
定义详见 Herdr 官方 [Configuration](https://herdr.dev/docs/configuration/) 与
[Config reference](https://herdr.dev/docs/config-reference/)；颜色映射见
[0.9.0 `src/ui/status.rs`](https://github.com/herdrdev/herdr/blob/v0.9.0/src/ui/status.rs)。

目前只接入 Codex 和 OMP。内部每 250ms 采样，使用最近约 2 秒的滚动窗口；侧栏数字最多每秒更新一次。Agent
存活时速度数字固定展示，连续 1.5 秒没有新的可观测 token、响应结束或等待用户输入时
显示 `0`。响应结束时将最后一次显示速度保留 1 秒；有足够观测时长的短响应也会保留一个非零帧，
随后自动归零；若期间开始新的输出则取消归零计时。它不是会话平均值，也不包含输入、
历史上下文、工具调用参数和工具输出。

现代 OpenAI 模型使用 `o200k_base` tokenizer，旧 GPT 模型使用 `cl100k_base`；未知模型
回退到 UTF-8 字节估算。Codex 同时采集回答、reasoning summary 与可用的 raw reasoning，
OMP 只采集 text 与 thinking。结束后的总 output usage 可能混入工具调用参数，因此不参与
校准。OMP 优先使用真实 text/thinking delta，避免 OMP 18 已被后续流更新改写的 partial
快照把整段输出压成单次采样；若 provider 没有提供可用 delta，则在 message_end 从最终
消息补齐缺失的 text/thinking。若 provider 将整段回复缓冲为单个事件，
最终速度帧按已观测的 text/thinking token 与消息生成时长计算；工具调用内容仍会排除。
显示区域只输出速度数字，不附加单位文字；数字语义仍为 `t/s`。模型、空闲速度和 OMP
显示名使用 5 秒 TTL，并每 2 秒刷新一次；wrapper 即使被强制终止，遗留 metadata 也会自动
过期。空闲心跳会把模型、零速度和 OMP 显示名合并为一次 snapshot，非零速度仍使用更短的
2.5 秒 TTL（覆盖 2 秒心跳并留出调度余量）。`herdr:tps` 是所有 Agent 的唯一生产 source，调用方不能覆盖；Codex/OMP 每次启动
都会先清除两个旧 metadata source，再由统一 owner 发布新值，因此同一 pane 在不同 Agent
间复用不会带回旧模型。

总 usage 只在响应阶段结束后可用，并可能包含本工具明确排除的其它 token，因此不用于
计数。运行中的数字只来自可观测的 thinking/reasoning 与正常回答流；`0` 表示当前没有
这两类 token 流，模型未公开的隐藏推理无法实时反映。

Codex 只展示当前用户主会话，排除子代理与 `threadSource: "system"` 的内部系统会话。
系统会话也可能没有父会话；旧扩展因此可能在启动后把正确模型覆盖成 Luna。更新此修复后，
需退出并重新运行对应 Codex，运行中的 wrapper 不会热加载代码。

会话分类规则与验证边界见
[维护指南：会话归属](./MAINTENANCE.md#会话归属)。

## 安装

在仓库根目录执行（`gpt-tokenizer` 由根 `package.json` / `package-lock.json` 管理，
`herdr-tps` 不是独立 npm 包；首次准备工作区时用 `npm ci --include=dev` 安装锁定依赖）：

```bash
npm run herdr:tps:install
```

计数器延迟加载 `gpt-tokenizer/cjs/encoding/*` 的公开 CommonJS 导出。OMP 18.1.18
内嵌 Bun 1.4.2 无法解析同一已安装包的双模式 `gpt-tokenizer/encoding/*` 导出；
不要用重新安装 OMP 或字节估算掩盖该解析错误。精确 tokenizer 缺失或计数失败时仍报错，
只有未知模型允许估算。修改后须重启 OMP，已有进程不会热加载。

安装器会：

- 备份并更新 `~/.config/herdr/config.toml`：第一行显示 workspace 和 tab，第二行显示
  状态指示灯、agent CLI 名称与速度，第三行显示模型；
- 在接管命令前校验 Codex/OMP wrapper 仍是 Node 启动脚本，避免误把原始 CLI 二进制
  当成 wrapper 后静默绕过实时采样；
- 在 `~/.local/share/herdr-tps/bin` 创建 Herdr 专用 `codex` 命令，并在 shell rc 中加入仅当
  `HERDR_ENV=1` 时生效的受管 PATH；Codex standalone 更新器改写标准 alias 时不会覆盖这个
  独立 shim；安装器会在交互 shell 中执行 live route probe，确认 `codex` 确实解析到 shim，
  修改已有 shell rc 前会先创建时间戳备份。旧版本写入的全局受管 PATH 块
  （`herdr-tps stable Codex dispatcher PATH` 标记）会被自动迁移移除，仅保留 Herdr 条件
  接管与 Herdr 外直通；标记残缺时拒绝改写。
- 在 `~/.local/bin` 保留显式 `codex-tps`、`omp-tps` 别名；OMP 标准命令仍由 wrapper 接管；
- 安装器把原始 OMP 固化到受忽略的 `integrations/herdr-tps/.runtime/omp`；OMP 子进程优先
  从该目录解析 `omp`，因此 `omp update` 只替换 runtime binary，不会沿标准命令链接
  覆盖本仓库 wrapper；
- 校验配置并通知正在运行的 Herdr 服务器重载。注意 Herdr 0.9.0 起 UI 渲染在客户端本地：
  侧栏布局属于客户端本地配置，`herdr server reload-config` 不会刷新已连接客户端的侧栏；
  侧栏 rows 变更需在客户端全局菜单执行 `reload config`（或重启客户端）后生效。

如果已有自定义 `ui.sidebar.agents.rows`，且已包含 `state_icon`、`$model` 与 `$tps`，
安装器会原样保留；缺少这些字段时拒绝覆盖，需要手工补齐。可先运行
`npm run herdr:tps:install -- --dry-run` 检查；
dry-run 会报告旧 dispatcher PATH 是否需要迁移，以及受管标记之外残留的 `herdr-tps/bin`
PATH 导出（仅警告，需先人工核对用途）。

## 使用

在 Herdr 新 pane 中继续使用原命令：

```bash
codex
omp
```

原命令参数无需改变，例如 `codex resume --last`。安装后必须重新打开 Herdr pane，使新的
shell rc PATH 生效；已有正在运行的进程不能热切换到流式观测，需要退出后重新运行一次。
`codex-tps`、`omp-tps` 仍作为显式别名保留。

OMP 生命周期状态（working/idle/blocked）由 Herdr 官方集成
`~/.omp/agent/extensions/herdr-omp-agent-state.ts` 负责，本工具不接管。升级 Herdr 后用
`herdr integration status` 核对版本，必要时执行 `herdr integration install omp` 同步
（0.9.0 随附 v9，含 `agent_end.willContinue` 判断：已排定续轮的结束不会误报 idle、
`agent wait` 不会提前完成）。pro2 profile 的扩展目录链接到默认 profile 同一文件，一次
安装两处生效；运行中的 OMP 进程不热加载，需退出重启后生效。
OMP 的 `pro2` profile 会自动显示为 `omp2`。Herdr 冷重启只使用
`omp --resume=<session-path>` 恢复会话；wrapper 会从 profile session 路径补回
`--profile=pro2`，因此实际 OMP 配置和显示名都不会退回 `omp`。其它 OMP profile 默认仍显示
`omp`；可通过 `HERDR_TPS_OMP_DISPLAY_AGENT` 显式指定显示名称。`omp2` shell 函数只负责
立即重命名 pane 与启动 `--profile=pro2`；侧栏显示名和 metadata 由扩展统一发布并在退出
时清理，shell 侧不再重复发送。

`omp2` 与默认 `omp` 共用后台服务：wrapper 在 `pro2` 首次启动前，将
`~/.omp/profiles/pro2/run/daemons` 备份并链接到 `~/.omp/run/daemons`。
相同项目共用 broker、浏览器及 LSP mux；项目之间仍按 OMP 原有项目 hash 隔离。
认证、模型设置和会话目录保留各 profile 的独立配置；两个交互客户端仍各自占用内存。
浏览器登录状态和后台进程名称在同项目内共享；LSP 底层实例仍由命令、cwd、参数和 env 决定，
不同 LSP 配置不强制合并。

首次切换前须正常退出所有旧 `omp2`，等待其 broker 和后台服务退出，再重新运行 `omp2`。
发现活动 client/broker/daemon PID 时，wrapper 拒绝切换并退出，不自动终止任务。
可用 `node integrations/herdr-tps/share-omp-daemons.mjs --dry-run` 单独检查，
或退出旧客户端后用 `--apply` 单独执行；默认仅检查。原始 runtime 二进制不会运行此启动检查，
完成链接后它也会使用共享目录。当前本机 OMP 18.1.11 已核对目录解析；启用 XDG state 的布局需另行适配。
回滚时先退出全部 OMP 客户端并等待 broker 退出，再移除 pro2 的 daemon 符号链接、
将输出的 `daemons.before-share-*` 备份恢复原名，并撤销 wrapper 的共享启动接线。

OMP 扩展只观察带 UI 的根 session。scout/reviewer 等无 UI 子代理即使使用不同模型，也不会
覆盖父 pane 的模型或 TPS；同时运行的多个 OMP pane 各自向自己的 `HERDR_PANE_ID` 发布。

Codex 包装器在本机回环地址启动 App Server 和透明 WebSocket 代理，再启动远程 TUI；
代理观察 App Server 发往 TUI 的 agent-message 与 reasoning 增量；token-usage 事件仅
作为 usage 通知忽略，不把其中的总 token 数加入速度，也不把它误判为响应结束；响应
边界以 `turn/completed` 为准。模型标签同时跟随 thread settings、turn override 与运行时
reroute；Default/Plan collaboration mode 指定模型时，其优先于同一事件中的基础模型。该
WebSocket transport 目前属于 Codex experimental 接口，只建议本机使用。

设置 `HERDR_TPS_DEBUG=1` 可输出代理连接错误与 metadata 发布异常日志；可用 `HERDR_TPS_TIMEOUT_MS`
调整单次 socket 发布超时（默认 2000ms）；可用 `HERDR_TPS_CODEX_BIN` 或 `HERDR_TPS_OMP_BIN`
指定原始可执行文件路径。OMP 下 metadata 异常默认收敛至 `pi.logger.debug`，Codex 下收敛至调试日志，
避免后台超时或网络抖动污染交互终端输入框；开启 `HERDR_TPS_DEBUG=1` 时按 30 秒限频输出到 stderr。

## Codex 活动会话绑定修复（2026-09-05）

现场 Codex CLI 0.153.4 同时加载了 GPT-6（`gpt-6-astra`）实际主会话和 Luna 占位会话。
旧观察器只通过 start/resume response 设置 root，后续实际会话的模型事件与输出被过滤，
导致侧栏固定 Luna、生成期间 TPS 为 0。

观察器现在登记 `thread/started` 与 start/resume/fork response 的 thread 信息，在主会话
`turn/started` 或客户端 `turn/start` 时更新活动绑定。未知会话的 `turn/started` 会触发
同一连接上的只读 `thread/read`，核对 `parentThreadId` / subAgent source 并读取模型；读取
期间缓存该会话通知并按顺序回放。内部读取响应不会转发给 TUI，读取失败或 2 秒超时会
释放缓存，后续 turn 可重试。不会发送 resume 或改变会话模型设置。

定向回归覆盖不同 ID 的占位/实际会话、子代理隔离、迟到启动响应、读取失败重试，
进程级 E2E 验证 GPT-6 标签和非零 TPS。已有 wrapper 不支持热加载，必须退出 Codex 后
重新运行 `codex resume <会话ID>` 才会加载修复；真实 pane 重启后的显示仍需现场验证。
本次修改限于 Codex，OMP 通过既有回归测试，未做 OMP 现场验证。

### 模型同步顺序与清理补齐（2026-09-05）

活动绑定修复还需覆盖模型设置先于会话登记、读取完成顺序与事件到达顺序不同等情况：

- `turn/start` 与 `thread/settings/updated` 的模型先按 thread ID 保存，在协议元数据确认
  主会话身份后发布；尚未确认身份的缓存只保留模型和序号，最多 1024 个 thread。
  基础 thread 快照不会覆盖已观察到的有效模型，子代理的提前设置也不会影响侧栏。
- 通知缓存回放使用原始接收序号。较早事件触发的异步读取、迟到启动响应不能覆盖较新的
  活动绑定；子代理事件不推进主会话的绑定序号。
- `model/rerouted` 必须匹配当前 `turnId`；旧轮次和已完成轮次的迟到事件不更新模型。
  重路由仅影响当前轮次的显示，不覆盖下一轮使用的配置模型。
- 关闭或删除活动 thread 时，reporter 清空模型、累计文本与结束保留计时，并通过 publisher
  显式发送 `tokens.model: null`。后续心跳不再续期旧模型，新会话可正常发布相同模型。
  单独清模型不清除 OMP 显示名；publisher 的生产 source 仍统一为 `herdr:tps`。

回归同时覆盖通知/只读查询两种身份确认路径、查询完成顺序互换、子代理隔离、迟到响应、
轮次校验和跨心跳清理。进程级 E2E 使用“提前收到 Astra 设置、thread/read 返回 Luna 基础快照”
的序列，验证 Astra 标签、非零 TPS 和 thread 关闭后的模型删除。

这些是代码与隔离回归验证；安装环境仍须退出并重新启动 Codex，才能加载更新并验证真实侧栏。
重启前不要把显示修复标记为现场验收通过。

## 速度口径与稳定显示

侧栏速度是**近似可见输出吞吐**：仅统计当前主会话的回答与可观测推理文本，不包含输入、
历史、工具参数/输出或未公开的隐藏推理。网络缓冲、分批送达和文本分词方法都会影响数值；
它不适合作为服务端真实解码速度、账单 token 或跨供应商性能排名。
当前模型映射尚未覆盖 GPT-6，`gpt-6-astra` 与其它未知模型使用 UTF-8 字节数 / 4 估算。
在取得匹配 tokenizer 的依据前，不把另一模型的 tokenizer 宣称为 GPT-6 的精确计数器。

Codex 与 OMP 复用以下显示规则：

- 原始吞吐仍按 2 秒滚动窗口、250ms 采样计算；首个实时数值等待至少 500ms 观察时间。
- 显示层采用时间常数 1 秒的指数平滑，正数最多每秒变化一次。
- 相对当前显示值，小于 `max(2 t/s, 5%)` 的变化暂不更新；因而稳定显示允许少量偏差。
- 连续 1.5 秒无可见输出时，在下一个采样点归零，不用平滑拖出虚假的持续输出。
- 新一轮、恢复输出、模型切换清空平滑状态；模型切换也清空上一模型的文本统计。
- 完成时保留最近已显示值 1 秒，不额外闪出末尾突发峰值；没有已显示值时，只有至少一个
  采样周期的观测时长才计算末帧。OMP 提供有效响应时长时，仍可用其可见输出 / 时长作回退。
- 完成保留期间若出现新输出，取消归零计时；metadata 心跳和 TTL 独立于数字变化频率。

这套配置优先稳定易读，持续加速/减速会有几秒的收敛时间。整条输出流重新分词仍保留，
避免逐 delta 独立分词造成边界误差；超长单流的 CPU 开销仍随累计文本长度增长。
更改脚本后需退出并重新启动相应 Codex/OMP 进程，运行中的 wrapper/extension 不会热加载。

## OMP 本地补丁

OMP Antigravity 429 与账号轮换平衡补丁由同级 `sol-omp` 仓库维护，不属于实时监控集成。
升级或重装 OMP 后，按 `sol-omp/scripts/omp-patches/` 中的说明检查并重新应用补丁。

## 生命周期与性能维护约束

- 原始 CLI 必须存在且不能经符号链接指回 wrapper；缺失时明确报错，不再回退到可能递归
  解析自身的同名 PATH 命令。安装器查找 Codex 原始命令时也跳过自身 dispatcher。
- Codex 非 Herdr 直通路径不加载 reporter；两套 tokenizer 均只在对应编码首次实际计数时加载，
  字节估算模型不会加载 tokenizer 词表。首次使用某个 tokenizer 仍有一次初始化开销。
- Codex 服务端 JSON 帧只解析一次。thread 缓存只保留 ID、父级、来源与模型，
  不保留 resume 的完整历史。旧 turn 的迟到 delta/completed 不影响当前 turn。
- 未知 thread 的只读查询期间只缓存统计相关通知，排除工具输出；单 thread 上限为
  1024 条通知或 256000 个 delta 字符。超限或读取失败时丢弃本次观察，后续 turn 重试，
  原始 TUI 消息仍按原样转发。连接关闭会清理 observer，迟到读取不能恢复已经关闭的观察器。
- metadata 串行发送；同一字段组合、TTL 和 guard 的未发送值只保留最新一份，避免慢 socket
  积压无限旧帧。已过期值丢弃，发送时扣除排队消耗的 TTL；退出清理会移除待发送的生产值。
  清理仍可能等待一个正在发送的 socket 请求完成或超时。
- reporter 可在未收到 model/text 时安全结束；close 后忽略迟到回调并释放累计文本。
  非零 TTL 为 `max(stale, heartbeat) + 2 × sample interval`，默认 2500ms。
- Codex readiness 探测有单次超时，TUI spawn 错误会进入清理路径；shutdown 关闭代理连接。
  OMP 转发 SIGINT/SIGTERM/SIGHUP，等待子进程退出。两处 profile 解析统一由
  `lib/omp-profile.mjs` 持有。

以上不改变统计口径：GPT-6 tokenizer 匹配和超长单流的重复分词成本仍是明确的后续优化点。
验证使用 `npm run herdr:tps:test` 和安装 dry-run；
真实安装环境的退出/恢复及侧栏显示仍需重新启动 Codex/OMP 后观察。
