# ai-monitor

独立 Rust TUI，用一个命令打开 Debian 系统资源与 AI 额度监控台：

```sh
ai-monitor
```

不提供子命令、JSON 输出或后台守护模式。Herdr、tmux、SSH 和普通终端都可以运行；窗格宽度由终端或 Herdr 调整，程序自动使用窗格的全部空间。

额度页上方显示 CPU、内存、SWAP、默认路由网速、磁盘读写、负载和 CPU 趋势；每个逻辑 CPU 从上到下排列在趋势图左侧，常见的 12 线程处理器可完整显示。

有两个页面，用 `t` 切换整个画面：

- **额度页**（默认）：上半是系统资源，下半是 Codex 常规 GPT、GPT-5.3-Codex-Spark、AGY、AGY2、
  OpenCode Go、SuperGrok 和 OpenRouter 的**云端剩余配额**，末行附本机历史消耗合计。
- **Token 页**：整页只放本机实际烧掉的 token —— 按模型排行（输入/缓存/输出/思考/缓存命中率/金额）、
  当日与当月与本年的柱状图、三个区间合计和历史总计。系统资源不占位置，表格获得完整窗格。

柱状图左侧是带刻度的 Y 轴，按量级自动选择 `K`／`M`／`B` 单位（例如 `375M`），顶端为区间最大值、
底端为 `0`，中间按行边界再标档位；X 轴桶号居中对齐在各自柱子下方。柱子横向铺满窗格，桶数少于可用
列数时自动加宽（区间名会标注每柱跨度），因此 12 个月的年份图在一百列的窗格里也是左右到边的。
`本年`／`当月`／`当日` 的名称放在各自图表**下方**，作为该图的区间标注；窗格够高时柱高会自动增加
（最多 4 行），不够高时先压缩柱高、再按「本年 → 当月 → 当日」的顺序去掉整张图。

两页语义不同，所以 token 页标题是「本地消耗 · Token」而不是「额度」：前者是本机用量，后者是云端余量。
两页各自独立滚动，切换时滚动位置归零。

## 安装

发行包或已经完成本机编译的项目中运行一次：

```sh
sh install.sh
```

安装到 `~/.local/bin/ai-monitor`，之后直接执行 `ai-monitor`。安装脚本不会修改 shell 配置、AI 客户端配置或 Herdr 配置。也可以不安装，直接运行 `./target/release/ai-monitor`。

源码编译需要 Rust 1.88+：

```sh
cargo build --release --locked
sh install.sh
```

Token 页的数据来自 `~/.local/share/herdr/usage.db`，由 Herdr 侧的 `herdr-usage` collector 写入。
数据库不存在时额度页显示「本地消耗  未连接」，不报错。路径可用配置项 `usage_db` 覆盖。

运行只需要 Linux 及交互终端，不需要 Rust、Node、Python 或额外的 TUI 插件。建议窗格至少 32 列、24 行；更小的窗格有降级显示。

## 0.3.2 更新

柱状图补齐读数：Y 轴顶端以下按行边界再加刻度，窗格越高档位越多，柱高可以直接对着旁边刻度读，不必只靠顶端的区间最大值目测；X 轴桶号改为居中对齐在各自柱子下方，末档贴边时钉在图内不再被截断；桶被合并加宽时区间名标注每柱跨度（例如「当月 · 每柱 2」）。底部 `0` 仍只有基线一处。

孤儿进程修复：终端关闭后旧进程不再满核忙等，详见 VERIFICATION.md 的 0.3.2 节。

## 0.3.1 更新

`t`、`r`、`↑`／`↓` 现在也支持鼠标：左键点页脚提示即可切换页面或刷新，滚轮在内容区滚动一行，中键刷新。页脚提示的可点区域由渲染布局直接发布，鼠标与键盘共用同一动作入口。键盘行为完全不变。

柱状图重做：左侧增加带刻度的 Y 轴，按量级自动选用 `K`／`M`／`B` 单位，顶端标区间最大值、底端标 `0`；柱子在横向铺满窗格，桶数不足时按比例加宽，年份图不再挤在左侧；`本年`／`当月`／`当日` 的名称移到各自图表下方；柱高随窗格高度增加，最多 4 行，窗格不够时先降柱高再按「本年 → 当月 → 当日」去掉整张图。

`t` 改为整页切换：Token 页占满窗格，不再在顶部保留 CPU、内存、网速与磁盘区，模型表、柱状图和总计因此获得完整高度；额度页仍是系统资源加云端额度。底部的刷新、切换、退出提示与版本号位置不变。

版本号以 `Cargo.toml` 的 `version` 为唯一来源（当前 `v0.3.2`）：页脚、HTTP User-Agent 和本文件的最新更新小节都由它派生，改动版本只需改 `Cargo.toml` 一处；`the_readme_changelog_matches_the_crate_version` 会在文档与它不一致时失败。

## 0.1.8 更新

修复独立额度池的刷新互相影响：仅当同一来源所有已返回的独立池都耗尽周额度时停轮询，并在最早的周重置时间恢复；未返回订阅的空占位卡不参与判断。GPT 与 Spark 中任一池仍可用时，保持正常刷新。

切换账号和刷新失败会清除等待状态，失败时保留的数据标为旧；正常数据超过配置刷新周期的两倍才标旧。系统网速和磁盘速率按设备分别计算差值，设备集合变化或计数器回退时重新采样；网络和磁盘读取失败互不影响。

各采集线程共享两套 HTTP 客户端的连接池与后台运行线程，账号凭据及 AGY 续期会话仍各自独立。

## 0.1.7 更新

OpenCode Go 官方给整数则保持整数显示（`98%`／`99%`／`100%`），撤销 0.1.6 的保底 1 位；官方给小数时仍保留官方位数。底部右侧显示版本号（例如 `v0.1.7`）。

清理冗余：`from_used` 是小数位上限的唯一收敛点，调用方不再重复限位；`decimals` 去掉与 `is_f64` 重叠的整数判断；`SystemSampler::new` 复用 `Default`。

## 0.1.6 更新

OpenCode Go 固定最少显示 1 位小数（`98.0%`／`99.0%`／`100.0%`，0.1.7 已撤销，整数按整数显示）：Go 后端把整数值按无小数点序列化，文本检测不到精度。

周额度用尽的来源标题显示等待时间（例如 `等1d 23h`），不再标旧；worker 与界面共用同一停轮询规则，到期自动再问，手动按 `r` 仍可立即刷新。

## 0.1.5 更新

额度按官方自带精度显示小数：官方给 1 位小数就显示 1 位，例如 OpenCode Go 的 `100.0%`、`68.8%`；官方给整数的来源保持原样。AGY 的 0~1 小数按乘 100 后的有效位数显示。

周额度用尽的来源停止轮询，直到官方重置时间再问一次（手动按 `r` 仍可立即刷新）。启动后第一次本来就会问一次，不断线重连也能刷新。

## 0.1.4 更新

AGY / AGY2 标题后显示各自 Google 账号名称，名称缺失时使用邮箱。资料来自 Google userinfo，按登录会话缓存在内存中；获取失败不影响额度显示，本地服务备用路径使用已验证账号的邮箱。

网络和磁盘吞吐固定为三位整数与固定宽度单位，例如 `072K/s`、`999K/s`、`001M/s`。超过 999K/s 后切换到 M/s，后续按相同规则切换到 G/s、T/s；首次采样占位符为 `---K/s`，因此刷新时字段位置不会跳动。

配色改为白底（浅色终端）可读：文字与进度条填充使用深色墨，对白底全部达到 WCAG AA 4.5:1 以上；进度条轨道改为中灰，填充对其 ≥3:1。跨过填充边界的进度条文字按列取深色或浅色墨，不会变成白底白字。

## 0.1.3 更新

在 CPU、内存和 SWAP 下方增加实时网络下载／上传速度与磁盘读取／写入速度。网络统计默认路由接口；无默认路由时统计除回环外的接口。磁盘统计顶层块设备，排除分区、RAM、loop 和被上层存储映射持有的设备，避免重复计数。

## 0.1.2 更新

逻辑 CPU 从横向字符条改为趋势图左侧的纵向轴，每行显示 CPU 编号、当前占用率和强度符号。系统区会按逻辑 CPU 数量动态增高，同时为额度区保留可滚动空间。

## 0.1.1 更新

AGY／AGY2 改为云端查询并支持令牌自动续期，关闭客户端也能持续监控。更新后退出旧的监控台，再运行 `ai-monitor` 即可。

## 操作

| 按键 | 操作 |
| --- | --- |
| `r` | 刷新所有额度；重复请求合并，遵守服务端退避时间 |
| `t` | 切换额度页／Token 页（整页切换，滚动位置归零） |
| `↑` / `↓` | 滚动额度区域 |
| `PgUp` / `PgDn` | 按页滚动 |
| `Home` / `End` | 到顶部／底部 |
| `q` / `Esc` / `Ctrl-C` | 退出并恢复终端 |

鼠标（终端需支持并已启用鼠标上报；Herdr、tmux、普通终端均可）：

| 手势 | 操作 |
| --- | --- |
| 左键点页脚的 `r 刷新` / `t 切换token` | 等同于按 `r` / `t` |
| 滚轮上／下 | 在内容区滚动一行 |
| 中键 | 刷新所有额度 |

页脚提示的可点区域由渲染布局直接发布，鼠标与键盘走同一个动作入口，因此不会出现「看到提示却点不动」或点到旁边文字被误触发的情况；`q 退出` 始终只能按键，避免误点退出。

系统指标每秒更新，额度默认每 60 秒更新。每组右侧的时间是距上次成功采集的时间；失败时显示具体连接状态，并把保留的上次数据标为旧数据。重置时间已到但服务端尚未确认新额度时显示“待刷新”，不会自行变成 100%。

## 账户与数据来源

- **Codex**：读取 `~/.codex/auth.json` 中的现有 ChatGPT 登录凭据，查询账户 usage。常规额度按返回的窗口显示；Spark 从独立额度池读取并保留 5H／周两个窗口。此接口属于 Codex 客户端内部接口，可能随服务更新而变化。不会启动 Codex 模型推理或刷新／改写其登录文件。
- **AGY / AGY2**：分别读取 `~/.gemini/antigravity-cli/antigravity-oauth-token` 和 `~/.gemini2/antigravity-cli/antigravity-oauth-token`，直接查询 Google 云端 `retrieveUserQuotaSummary`。**两个客户端都可以关闭，不需要后台 AGY 服务**。显示 Gemini 共享池的 5H／周额度，Pro 和 Flash 不重复列为两份额度。访问令牌接近到期时自动续期；服务端提前返回 401 时最多续期重试一次。新令牌仅保存在各自监控线程内存中，不改写 AGY 登录文件；客户端重新登录后自动读取新凭据。刷新凭据被撤销时需重新登录相应客户端，登录后可再关闭。缺少文件凭据或云端连接／格式异常时，尝试同一配置目录的已运行本地服务并标记“本地服务备用”；登录失效和限流不会被备用路径掩盖。云端接口与 OAuth 客户端配置属于内部协议，可能随 AGY 版本更新而变化。
- **OpenCode Go**：使用 OpenCode 已保存的 Go API Key 查询 `/zen/go/v1/usage`，显示 5H／周／月剩余比例，不抓取浏览器 Cookie。
- **SuperGrok**：使用 `~/.grok/auth.json` 中的订阅登录信息，查询 Grok Build 的订阅 credits 接口，按实际返回的周期解码。登录过期时提示重新登录 Grok。账号的实时接口仍受登录状态、服务端权限及内部协议版本影响，不能保证每个账号都有可查询额度。
- **OpenRouter**：使用具有余额查询权限的 Key 查询 `/api/v1/credits`，显示账户总 credits 减总 usage。读取顺序为下述 Key 文件、`OPENROUTER_MANAGEMENT_KEY`、`OPENROUTER_API_KEY`，最后尝试 OpenCode 保存的 OpenRouter Key。普通模型调用 Key 不保证具有余额查询权限。

OpenRouter 专用 Key 可保存在 `~/.config/ai-monitor/openrouter.key`，文件只包含 Key，建议权限 `600`。不要把 Key 写入源码、截图或聊天消息。

程序只读取这些客户端的登录文件，不保存或打印凭据，不自动充值／切换账户，不向外发送会话记录。网络请求仅发送到对应服务；本地 AGY 的自签名证书例外仅适用于固定的 `127.0.0.1` 地址。数据快照只保留在当前进程内，退出后不留账户额度缓存。

## 可选配置

默认无需配置。需要修改路径时，可在 `~/.config/ai-monitor/config.toml` 放置以下任意字段：

```toml
refresh_seconds = 60
codex_home = "~/.codex"
agy_home = "~/.gemini"
agy2_home = "~/.gemini2"
opencode_home = "~/.local/share/opencode"
grok_home = "~/.grok"
openrouter_key_file = "~/.config/ai-monitor/openrouter.key"
```

支持标准 `XDG_CONFIG_HOME`、`XDG_DATA_HOME`、`CODEX_HOME`、`GROK_HOME` 位置。开发或隔离测试可用 `AI_MONITOR_CONFIG` 指定单独配置文件。配置中不支持任意网络服务器地址，避免登录凭据被误发。

## 开发验证

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

测试覆盖双账户隔离、自动续期及内存复用、令牌轮换、401 有界重试、登录撤销、Spark 多窗口、额度缺失／过期、Go 用量方向、余额计算、Grok protobuf 错误帧、Linux CPU／内存／网络／磁盘指标、重试退避和小窗格渲染。

## 协议参考

- [Codex App Server 的多额度池契约](https://learn.chatgpt.com/docs/app-server)
- [AGY 云端额度与 OAuth 查询参考](https://github.com/barramee27/crossusage/blob/feat/linux-windows-native-support/docs/providers/antigravity-cli.md)
- [AGY OAuth 客户端配置参考](https://github.com/lbjlaq/Antigravity-Manager/blob/main/src-tauri/src/modules/oauth.rs)
- [AGY 原生额度面板](https://www.antigravity.google/docs/cli/commands/usage)
- [OpenCode Go usage 接口源码](https://github.com/anomalyco/opencode/blob/dev/packages/console/app/src/routes/zen/go/v1/usage.ts)
- [OpenRouter credits 接口](https://openrouter.ai/docs/api/api-reference/credits/get-remaining-credits)
- [CodexBar 的 AGY 本地接口与额度结构参考](https://github.com/steipete/CodexBar/tree/main/Sources/CodexBarCore/Providers/Antigravity)
- [SuperGrok 订阅 protobuf 字段参考](https://github.com/aspinojony/grok-credential-manager/blob/main/app/weekly_quota.py)
