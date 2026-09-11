# ai-monitor

[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

一个纯 Rust 编写的超轻量 Linux 终端监控看板与多 Agent 本地 Token 审计审计系统。

它同时整合了 **系统资源监测**、**云端 AI 额度追踪** 以及 **多 Coding Agent 本地 Token 消耗审计（基于 SQLite WAL 架构）**。

---

## 目录与项目结构

本项目采用 Cargo Workspace 组织，共享统一依赖与编译缓存：

```text
ai-monitor/
├── Cargo.toml                    # Workspace 根配置
├── install.sh                    # 一键编译安装脚本
├── systemd/
│   └── herdr-usage-collector.service  # 用户级后台采集守护进程配置
└── crates/
    ├── ai-monitor/               # 前台 TUI 终端监控看板 (Ratatui)
    │   ├── src/
    │   │   ├── main.rs           # 事件循环与输入路由
    │   │   ├── ui.rs             # 自适应布局、响应式柱状图渲染
    │   │   ├── usage.rs          # SQLite 只读查询引擎与日历分桶
    │   │   ├── system.rs         # CPU/内存/SWAP/网速/磁盘 IO 采集
    │   │   ├── worker.rs         # 轮询调度器
    │   │   ├── config.rs         # 配置加载与路径解析
    │   │   └── providers/        # 云端厂商配额抓取 (Codex, Grok, AGY, OpenCode)
    │   └── tests/
    │
    └── herdr-usage/              # 后台日志采集守护进程与价格工具
        ├── migrations/
        │   └── schema.sql        # SQLite 表结构唯一 SSOT
        ├── src/
        │   ├── main.rs           # 60 秒常驻增量采集主循环
        │   ├── bin/import_prices.rs  # OpenRouter 价格表同步 CLI
        │   ├── event.rs          # 5 维标准 Token 规范化口径 (Canonical Accounting)
        │   ├── tail.rs           # 字节水位、半行残留保护、文件轮转判定
        │   ├── db.rs             # WAL 模式连接、幂等批量写入、水位持久化
        │   ├── cost.rs           # 模型匹配算法与成本计算
        │   └── sources/          # OMP / Codex / Grok / OpenCode 四源解析器
        └── tests/
            └── collect.rs        # 增量、重放、轮转、断点续读端到端测试
```

---

## 核心架构与数据流

```text
┌─────────────────────────────────────────────────────────────┐
│                       AI Agent 客户端日志                     │
│   • omp (~/.omp/agent/sessions/*.json)                      │
│   • codex (~/.codex/sessions/*.jsonl)                       │
│   • grok (~/.grok/logs/unified.jsonl)                       │
│   • opencode (~/.local/share/opencode/opencode.db)          │
└──────────────────────────────┬──────────────────────────────┘
                               │
                               ▼
┌─────────────────────────────────────────────────────────────┐
│              herdr-usage (常驻增量采集守护进程)                │
│  • 60s 一轮，纯只读、不联网                                    │
│  • 字节水位 + 半行保护 (FileOffsets.residues)                 │
│  • Codex 同文件上下文 model 回溯                                │
│  • OpenCode 2000 行滑动窗口重放 + 吸收回填                     │
│  • Grok 日志文件重写/轮转归零判定                               │
└──────────────────────────────┬──────────────────────────────┘
                               │ 幂等批量写入
                               ▼
┌─────────────────────────────────────────────────────────────┐
│                 usage.db (本地 SQLite 存储层)                │
│  • PRAGMA journal_mode = WAL; synchronous = NORMAL;         │
│  • usage_event (PRIMARY KEY(source, event_id) WITHOUT ROWID)│
│  • collect_offset / model_price / model_alias / unresolved  │
└──────────────────────────────┬──────────────────────────────┘
                               ▲
                               │ 只读无锁查询 (SQLITE_OPEN_READ_ONLY)
┌──────────────────────────────┴──────────────────────────────┐
│                    ai-monitor (TUI 终端看板)                 │
│  • 一页两栏：左栏系统监控 (CPU/Mem/Disk/Net) + 云端厂商配额   │
│  • 右栏：本地模型用量排行 + 日/周/月柱状图 + 历史总计          │
│  • Calendar-Aware 日历感知分桶，避免跨月错位                   │
│  • 纯键盘 + 鼠标双模交互                                      │
└─────────────────────────────────────────────────────────────┘
```

---

## 核心设计与数据口径

### 1. 5 维标准 Token 模型 (Canonical Accounting)
各 AI 工具导出的 Token 字段互不统一，本项目在 `event.rs` 中定义了统一的 5 维原子口径：

| 标准字段 (Canonical) | 含义 | 约束规则 |
| :--- | :--- | :--- |
| `input_total` | 输入毛值（包含缓存命中部分） | $0 \le \text{cache\_read} \le \text{input\_total}$ |
| `cache_read` | 命中并读取的上下文缓存量 | 计入 prompt 折扣 |
| `cache_write` | 写入上下文缓存量 | 仅部分提供商支持 |
| `output_total` | 输出总量（**包含思考量**） | $0 \le \text{reasoning} \le \text{output\_total}$ |
| `reasoning` | 思考/推演 Token | **是 `output_total` 的组成部分，绝不并列累加** |

### 2. 健壮日志采集机制
- **半行保护**：文件写入截断在半行时，该残行不解析，随字节水位一并暂存至 `FileOffsets.residues`，下轮无缝拼接。
- **OpenCode 先建后填**：行创建时 tokens 全零、数十小时后异步回填。采集器对全零行**不入库**，利用 2000 行重放窗口结合 `INSERT OR IGNORE` 自动补齐。
- **Codex 严格去重**：只采集增量流式 `token_count`，主动忽略可能导致二次计量的 `token_usage_record`。
- **价格表解耦**：采集进程不联网；通过 `import_prices` CLI 单独向 `model_price` 同步 OpenRouter 价格；未计价模型记录至 7 天滑动窗口的 `unresolved_model` 表。

---

## 快速安装与使用

### 1. 编译与安装

系统要求：Linux，Rust 1.88+ (2024 edition)

```sh
# 克隆仓库后，运行根目录安装脚本
sh install.sh
```

脚本将自动执行 Release 编译，并将以下程序安装到 `~/.local/bin`：
- `ai-monitor`：终端 TUI 看板
- `herdr-usage`：Token 采集守护程序
- `import_prices`：价格表同步工具

安装脚本使用 `--all-features` 编译；若只需采集器，可单独执行
`cargo build --release -p herdr-usage`（跳过价格同步工具的 HTTP 依赖编译）。

### 2. 运行 TUI 监控看板

```sh
ai-monitor
```

**交互按键**：
- `r`：立即刷新所有指标与数据库查询。
- `↑` / `↓` / 滚轮：滚动浏览内容（系统资源与云端配额在左栏，Token 统计在右栏）。
- `PgUp` / `PgDn`：按页翻滚。
- `Home` / `End`：回到顶部或底部。
- `q` / `Esc` / `Ctrl+C`：安全退出并恢复终端状态。
- **鼠标支持**：左键可直接点击底部页脚的刷新按钮，中键刷新。

### 3. 后台采集守护进程 (Systemd)

启用用户级 systemd 服务以自动采集用量：

```sh
# 复制服务定义
mkdir -p ~/.config/systemd/user
cp systemd/herdr-usage-collector.service ~/.config/systemd/user/

# 重新加载并启动服务
systemctl --user daemon-reload
systemctl --user enable --now herdr-usage-collector

# 查看运行状态与日志
systemctl --user status herdr-usage-collector
journalctl --user -u herdr-usage-collector -f
```

### 4. 价格表更新

```sh
import_prices
```

工具将从 OpenRouter 获取最新官方模型计价并写入 SQLite。对人工指定的别名或备注（`remark`）会自动保留。

---

## 运行自动化测试

项目内置完整的单元测试与端到端集成测试（包括模拟日志轮转、断点续读、并发重放等）：

```sh
cargo test
```

---

## 许可证

本项目基于 [MIT 许可证](LICENSE) 开源。
