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
