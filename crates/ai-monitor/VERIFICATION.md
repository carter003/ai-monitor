# 验证记录

## 0.3.2

日期：2026-09-11，本机 Linux x86_64（WSL2）。

- **修复孤儿进程满核忙等**：终端（pty master）关闭后，旧进程在 crossterm 0.29 的
  `event::poll` 内 100% 占用一个核、永不退出，且 SIGTERM/SIGHUP 仅置位主循环永远读不到的
  shutdown 标志。多会话环境下每次二进制重编译都留下一批孤儿（实测单机 12 个 ≈ 10 核负载）。
- 根因（源码级定位 + pty 复现）：crossterm unix `mio.rs` TTY 分支 `read() == Ok(0)`（EOF）
  既不 break 也不报错，内层 `loop` 空转；EOF 后 epoll 永远就绪，`event::poll` 不再返回，
  主循环永远无法复查 shutdown 标志（crossterm 0.28.1 同样存在）。
- 修复（`src/main.rs`，不改上游依赖）：新增 `terminal_alive()`（`open("/dev/tty")` +
  `TIOCGWINSZ`，挂断后 open 返回 ENXIO）与后台看门狗线程 `watch_terminal()`（1s 探测）。
  检测到挂断即恢复 termios/备用屏幕并 `exit(0)`；探测先于 shutdown 标志检查，避免
  `SIGHUP` 置位标志使看门狗提前退出的竞态。主循环 `event::poll` 错误按终端拆除处理
  （EINTR 继续，其余退出），退出路径 join 看门狗。
- 新增回归 `hangup_tests::a_missing_controlling_terminal_reports_dead`（cargo test 环境
  无控制 tty，确定性覆盖探测失败路径）。
- 真实 PTY 对照（python pty + TIOCSCTTY + DSR 应答，挂断后采样 `/proc/<pid>/stat` ticks）：
  修复前 100.0 ticks/s 且 5 秒后仍存活（含 0.1.8 备份二进制），修复后 ≈ 20 ticks/s（退出
  过渡期）且立即退出；`q` 正常退出 rc=0，termios/备用屏幕恢复正常。
- `cargo test --locked`：93 项通过，1 项联网测试默认忽略；`cargo clippy --all-targets --
  -D warnings`、`cargo fmt --check` 通过；`cargo build --release --locked` 后经 `install.sh`
  安装至 `~/.local/bin/ai-monitor`（sha256 de6fc6cf…）。
- **修复柱状图 Y 轴刻度与 X 轴对齐**（`src/ui.rs`）：Y 轴原先只有顶端一格有数字，且顶格
  打的是行的下边界值（height=1 时甚至打出 `0`）；改为按行**顶**边界打值，顶格恒为区间
  最大值、中间档位随窗格高度最多三档、基线 `0` 唯一（全零数据时只保留基线）。X 轴桶号
  从柱子左边缘改为居中对齐（`offset + span/2 − text.len/2`），末档贴边时钉在图内不截断；
  合并桶时区间名标注每柱跨度（`当月 · 每柱 N`）。死参数 `span_label` 随 cutover 删除。
- 取证：临时 `examples/dump_tokens.rs`（TestBackend）扫 80×24/21/19/17/15/13 与
  60/80/100/140 宽，确认 bars=4/3/2/1 各档刻度无重复 `0`、顶格为最大值；取证后删除。
- 真实 PTY（tmux 110×34，`t` 切 token 页）：当月 `473M/315M/158M/0M`、当日
  `201M/134M/67M/0M`，X 轴 `1/10/20/30` 居中且末档完整；110×24 降为当日单图
  `201M/100M/0M`；110×20 降柱高保总计；`q` 退出 tmux 会话关闭、无残留进程。
- `cargo test --offline`：92 项通过（含 README 版本一致性回归），`cargo clippy --offline
  --all-targets -- -D warnings`、`cargo fmt --check` 通过；`cargo build --release --offline
  --locked` 后经 `install.sh` 安装（sha256 8fa11237…）。

## 0.2.0

日期：2026-09-11，本机 Linux x86_64。

- `cargo test --offline`：85 项通过，1 项真实 Google 联网测试默认忽略（0.1.8 为 54 项）。
- `cargo clippy --offline --all-targets -- -D warnings`、`cargo fmt --check`：通过。
- 新增 token 页（`src/usage.rs` + `ui.rs`）：只读 `~/.local/share/herdr/usage.db`，
  与系统区 1s 采样解耦的独立 60s 线程，经既有 `mpsc::Sender<Update>` 投递 `Update::Usage`。
- 页脚加 `t 切换token`；窄窗格按宽度降级（依次丢弃滚动提示 → 切换提示），不再与版本号重叠。
- 覆盖率口径：`priced / (all − ignored)`，`ignored ⊆ unpriced`，分子不重复扣减。
  memory sqlite 实测 3 事件（2 ignore + 1 已计价）→ 1.00；2 事件（1 未识别）→ 0.50。
- 模型表按 `COALESCE(alias.model_id, event.model)` 归并：`GROUP BY model` 会绑定到基列
  `e.model`，同一模型跨 client 仍会拆成两行 —— 已改为按表达式分组并写回归。
- 柱状图用 `strftime('%H'/'%d'/'%m', …, 'unixepoch','localtime')` 日历分桶：固定桶宽会让
  `2026-03-01` 落进 2 月桶；当月桶数按实际天数（28/29/30/31），空桶补零。
- 高度不足按既定顺序裁减，总计行最后消失（`the_total_line_survives_the_shortest_pane`）。
- 新增回归：切页重置 `scroll`、两页页脚都含切换提示、DB 缺失显示「未连接」不显示 `$0.00`、
  首页与 token 页同一数字、窄窗格丢弃 THINK/CACHE 但保留 IN/OUT/COST 且不被裁成 `COS`、
  表头与数值按**终端列**对齐、`compact()`/`money()` 边界、空桶不画成实心条。
- 真实 PTY 对照（tmux 110×34 与 34×20，隔离配置指向不存在的凭据路径，无真实账号请求）：
  额度页与 token 页均正常渲染，`t` 切换、滚动、`q` 退出及 termios/备用屏幕恢复正常。


## 0.1.8

日期：2026-09-09，本机 Linux x86_64。

- `cargo test --offline --locked`：54 项通过，1 项真实 Google 联网测试默认忽略。
- `cargo clippy --offline --locked --all-targets -- -D warnings`、`cargo fmt --check`：通过。
- `cargo build --release --offline --locked --bin ai-monitor`：通过。
- 新增 7 项回归测试，覆盖 GPT/Spark 独立池轮询、所有池耗尽时最早重置恢复、空订阅占位与未知额度、账号切换清等待、手动刷新失败标旧、配置刷新周期及过期边界、网卡/磁盘集合变化、单设备计数器回退、采集缺失后的恢复及网络/磁盘故障隔离。
- Release 伪终端对照：80×42、20×10、32×24 缩放，刷新、滚动、q 退出及 termios/备用屏幕恢复通过；新版页脚确认 `v0.1.8`。
- 对照测试使用隔离配置，所有凭据路径指向不存在的临时文件，并移除 OpenRouter Key 环境变量，无真实账号请求。启动 2 秒时，旧版 19 线程、新版 9 线程；RSS 单次快照分别为 5752 kB / 5012 kB。线程差异来自共享 HTTP 客户端，RSS 仅为该隔离场景观察值，不代表长期或联网内存上限。
- 本次未执行真实账号接口验证或长期 CPU/内存压力测试。AGY 双账户隔离、续期、凭据轮换和有界重试由既有离线测试覆盖。
- 安装前保留源码与旧版二进制：`../backups/before-0.1.8-20260909-121039/`。

## 0.1.7

- `cargo test --locked`：47 项通过，1 项联网测试默认忽略。
- `cargo clippy --locked --all-targets -- -D warnings` 与 `cargo fmt --check`：通过。
- Go 整数恢复整数显示（`go_keeps_official_precision_without_padding_integers` 重写）；新增 `footer_shows_version_at_bottom_right`（80×24 末行以版本结尾，版本取自 `CARGO_PKG_VERSION` 不随升级腐烂）。
- 全量审计结论：删三处真冗余（调用方重复 `.min(2)`、`decimals` 重叠整数判断、`SystemSampler::new` 手写 `Default`）；其余如 AGY 占位回退、`percent` 渲染限位、protobuf 显式跳过分支均有独立职责，保留。

## 0.1.6

- `cargo test --locked`：46 项通过，1 项联网测试默认忽略。
- `cargo clippy --locked --all-targets -- -D warnings` 与 `cargo fmt --check`：通过。
- 实测官方 Go 接口返回整数 `percent`（`2`／`1`／`0`，Go 序列化整数 float 无小数点），`go_keeps_one_decimal_place_from_official_percent` 改为整数也保底 1 位；新增 `exhausted_weekly_quota_records_hold_until_reset`（用尽记等待、恢复／授权失效清除）、`exhausted_weekly_source_shows_wait_instead_of_stale`（超 120 秒不标旧、标题显示 `等1d 23h`）。
- 启动时 AGY 双账户续期连接失败为瞬时网络抖动：`http.refresh` 的 send 在 12 秒超时内失败，worker 按既有退避 60 秒后重试成功；与 0.1.5／0.1.6 改动无关。

## 0.1.5

- `cargo test --locked`：44 项通过，1 项联网测试默认忽略（0.1.4 为 38 项）。
- `cargo clippy --locked --all-targets -- -D warnings` 与 `cargo fmt --check`：通过。
- 新增 `percentage_keeps_official_decimal_places`（Go 1 位小数：`100.0%`／`68.8%`／`0.0%`，整数官方保持 `82%`）、`go_keeps_one_decimal_place_from_official_percent`、`agy_fraction_precision_shifts_with_remaining_percent`、`exhausted_weekly_quota_waits_for_reset_instead_of_polling`（5H 将先到期也不提前问，等 6 天周重置）、`weekly_wait_needs_a_known_reset_time`（无重置时间／未用尽／月额度耗尽都不停轮询）。

## 0.1.4

- `cargo test --locked`：38 项通过，1 项联网测试默认忽略。
- `cargo clippy --locked --all-targets -- -D warnings` 与 `cargo fmt --check`：通过。
- 吞吐字段固定为六字符：三位整数、一级单位和 `/s`；覆盖 K→M→G→T 边界。
- 44×42 实机伪终端确认 `NET ↓ 037K/s ↑ 024K/s`、`DSK R 000K/s W 000K/s` 对齐稳定；缩放、滚动、刷新、退出及 termios 恢复通过。
- 白底终端配色（0.1.4 修订）：CPU／MEM／SWP／NET／DSK、每核占用率、额度条与状态文字的前景色对白底全部达到 WCAG AA 4.5:1 以上（旧配色最低仅 1.74:1）；进度条填充对轨道 ≥3:1。
- 新增 `inks_clear_wcag_aa_on_a_white_terminal`、`gauge_fill_and_label_stay_readable_on_the_track`、`gauge_label_is_inked_per_column_across_the_fill_boundary`：38 项通过，1 项联网测试默认忽略。将 AMBER 与 TRACK 还原为旧值时断言失败，可用于防回归。

## 0.1.3

- `cargo test --locked`：35 项通过，1 项联网测试默认忽略。
- `cargo clippy --locked --all-targets -- -D warnings`：通过。
- `cargo fmt --check`：通过。
- 真实 `/proc` 与 `/sys` 采样：默认路由接口下载／上传速度和顶层块设备读取／写入速度均成功显示。
- 44×42 伪终端显示、20×10 缩放、32×24 滚动、手动刷新、退出及 termios 恢复：通过。

## 0.1.2

- `cargo test --locked`：31 项通过，1 项联网测试默认忽略。
- `cargo clippy --locked --all-targets -- -D warnings`：通过。
- `cargo fmt --check`：通过。
- 44×42 伪终端实测：CPU01—CPU12 在总 CPU 趋势左侧逐行显示占用率和强度符号；额度区保持可滚动。
- 20×10 缩放、32×24 滚动、手动刷新、退出及 termios 恢复：通过。

## 0.1.1

日期：2026-09-06，本机 Debian / Linux x86_64。

- `cargo test --locked`：30 项通过，1 项联网测试默认忽略。
- `cargo test --locked live_two_profiles_renew_without_client_or_file_writes -- --ignored --nocapture`：通过。使用两个真实账户，在内存中强制令牌过期以验证 Google 自动续期，每个账户连续查询两次；客户端登录文件字节不变。
- `cargo clippy --locked --all-targets -- -D warnings`：通过。
- `cargo fmt --check`：通过。
- `cargo build --release --locked --bin ai-monitor --example check_sources`：通过。
- Release TUI 伪终端验证：没有可见 AGY／语言服务进程，44×42 窗格成功显示两个账户的 5H／周额度；20×10 缩放、32×24 滚动、手动刷新、退出及 termios 恢复通过。
- 实测 Gemini 周剩余：AGY 48.201433%，AGY2 67.2442%；两账户 5H 剩余均为 100%。这些仅是验证时的快照，程序不硬编码额度。

自动化回归覆盖令牌过期、401 后单次重试、服务端拒绝新令牌、续期凭据轮换、多周期续期、账户切换、客户端凭据更新、文件缺失／损坏、限流退避和双账户隔离。网络／格式失败保留原有本地服务备用路径；授权失效和限流不回退。

SuperGrok 与 OpenRouter 的既有凭据问题不在本次 AGY 优化范围内。
