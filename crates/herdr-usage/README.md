# herdr-usage

Herdr 内四个 agent 客户端（`omp`、`codex`、`grok`、`opencode`）的 token 用量 collector。
常驻进程，60 秒一轮，**只读**源日志与只读价格表，**不联网**。

口径、去重键、水位语义、价格表维护方式与回归清单见仓库
`.agents/herdr/README.md` 的「Token 用量统计」一节 —— 那里是唯一 owner，本文件只写操作方式。

## 运行

```sh
cargo build --release --offline      # 依赖已在本机 cargo 缓存中
cargo test --offline                 # 单元 + 端到端（临时 HOME 夹具）
```

常驻（已启用）：

```sh
systemctl --user status  herdr-usage-collector
journalctl --user -u herdr-usage-collector -f
```

手动试跑，不污染生产库：

```sh
HERDR_USAGE_DB=/tmp/probe.db ./target/release/herdr-usage
```

## 价格表

`import_prices` 是 `model_price` 的**唯一**写入者。collector 从不写入，也不联网。

```sh
cargo run --release --offline --bin import_prices
```

它会打印写入/跳过数量，并按 `hit_count` 降序列出近 7 天未计价的模型，供人工补别名或确认忽略。
更新时**不清空 `remark`**：人工写的偏差说明会被保留。

新增别名（人工或 AI 维护，SQL 直写库）：

```sql
INSERT INTO model_alias(raw_model, model_id, ignore, resolved_by, remark)
VALUES ('opencode-go/ox-alpha-free', NULL, 1, 'ignore', '免费通道不计价')
ON CONFLICT(raw_model) DO UPDATE SET
    model_id = excluded.model_id, ignore = excluded.ignore,
    resolved_by = excluded.resolved_by, remark = excluded.remark;
```

`model_price` / `model_alias` 由 collector **每轮重读**，改完 60 秒内自动生效，无需重启。

## 目录

```text
src/event.rs             canonical 用量与四源归一化（口径唯一 owner）
src/sources/{omp,codex,grok,opencode}.rs   四源解析；codex/grok 额外做 model 回溯
src/tail.rs              字节水位、半行保护、轮转判定
src/db.rs                schema、幂等写入、水位持久化
src/cost.rs              计价公式、别名解析、未计价台账
src/main.rs              60s 主循环
src/bin/import_prices.rs 价格表导入
migrations/schema.sql    schema（唯一 owner）
tests/collect.rs         端到端：baseline、增量、重放、轮转、重启续读
```

## 已知偏差

- `pricing.overrides`（分时段定价，实测 65 个模型）不处理，一律取基础价，金额会有偏差。
- grok 的 model 在无 `model changed` 事件时回退 `config.toml [models].default`；
  若在 collector 未运行期间切换过模型，那批事件会记到 default 上（`model_source = config_fallback`）。
- 不回填历史：`usage_event` 只有 collector 首次运行之后的数据。

## 本地网页统计

```sh
cargo run --offline -p ai-monitor
# 或安装后运行 ai-monitor
# 浏览器打开 http://127.0.0.1:19999
```

- **总览 `/`**：全部模型的今日、本周（周一开始）、本月、累计 Token 与金额，近 30 天趋势和全部历史模型排行。
- **独立查询页 `/query`**：日期范围、全部模型或单模型筛选；可切换「每天合计」「每天按模型」，每天合计补齐无记录日期。明细每页 50 行。
- Token 数量达到 10 亿使用 B，其余使用 M（1 B = 10 亿，1 M = 100 万），较小数值保留必要小数。
- 点击趋势柱可跳转到查询页查看当天模型明细；点击排行中的模型可查看该模型每天的数据。支持今天、近 7 / 30 天、本月、全部历史快捷查询，单次最多 3661 天。
- 网页仅监听 `127.0.0.1`，资源全部内嵌，无需 Node、前端构建或 CDN。点击「刷新」重新读取 collector 已采集的数据。
- 数据库以只读方式打开，不创建数据库，不修改采集和价格数据。未运行 collector 时显示连接错误；空库显示零用量。
- 日期按**服务进程本地时区**划分，与终端统计一致；可通过 `TZ=Asia/Shanghai` 指定。金额为已记录的 USD 估算费用，不是账单实付金额；部分未计价时仅累计已知费用并标注条数，全部未计价显示「未计价」。
- 模型按 `COALESCE(model_alias.model_id, usage_event.model)` 合并；为使明细与合计一致，网页保留未识别和忽略计价的模型。

JSON 接口：`GET /api/usage?start=2026-09-01&end=2026-09-12&model=vendor%2Fmodel`。
日期包含首尾两天，省略日期默认近 30 天；省略 `model` 表示全部模型，`unknown=1` 查询未识别模型。
响应包含全局 `overview`、筛选范围 `total` / `daily` / `details` / `ranking`，以及模型列表和时区。

网页由 `ai-monitor` 在同一进程内启动，按 `q` / Esc / Ctrl+C、接收退出信号、关闭终端或强制结束进程时一同关闭，不产生网页子进程。正常退出会停止监听、关闭活动连接并回收网页线程。不再使用独立 systemd 网页常驻服务；collector 的采集生命周期保持独立。

在 `~/.config/ai-monitor/config.toml` 中配置（网页和终端共用 `usage_db`）：

```toml
web_port = 19999
usage_db = "~/.local/share/herdr/usage.db"
```

端口已被占用时在进入终端界面前报错，不复用或终止其他进程的服务；多个实例使用不同 `web_port`，或设为 `0` 自动分配端口（启动时打印地址）。

`usage-web` 二进制仅保留为手动调试入口，直接运行它时生命周期独立，需自行关闭；日常使用直接运行 `ai-monitor`。
