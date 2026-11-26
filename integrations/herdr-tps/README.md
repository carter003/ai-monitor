# Herdr 原生模型与 TPS 集成

OMP 与 Codex 均直接运行官方 CLI。集成使用 Node.js 22 内置模块，无 npm 外部依赖、WebSocket 代理或二进制副本。

```sh
node integrations/herdr-tps/install.mjs --dry-run
node integrations/herdr-tps/install.mjs
npm run herdr:tps:test
# 移除新扩展和 Hook，保留原生 CLI
node integrations/herdr-tps/install.mjs --uninstall
```

安装器备份并清理旧 Shell 受管块和 wrapper 软链，原子归位 OMP 二进制，将两个 `.js` 扩展软链到 `~/.omp/agent/extensions/`，在 Codex 配置末尾追加独立 SessionStart/SessionEnd 分组。它不更改已有 Hook 的信任状态，也不停止运行中的会话。配置语法校验失败会恢复本次修改。

安装后新开 Codex TUI，按 Codex 原生提示确认新增 Hook 的信任；新开 OMP 会自动加载扩展。既有进程继续运行旧会话逻辑，直至正常退出。

OMP 的 `omp-subagent-model.js` 让未配置的 task 角色继承当前父模型；eval 则核对实际发现的 Agent 定义，仅默认角色匹配时继承，保留无默认角色的 m1、不同默认值的显式角色以及用户配置。OMP 派发事件不提供显式参数来源：对同一个 reviewer 显式指定与其默认值相同的 `@slow`，事件无法区分，也会按默认角色继承。内部模块变更时记录警告并退回原生行为。`omp-herdr-tps.js` 独立使用宿主公开 Tokenizer 与原生用量校正，保持 OMP 工作行速率口径。`--no-tools` 使用原生扩展跳过规则。

Codex Hook 只接收 Herdr 根交互会话：检查环境、会话标识、真实 transcript 和 `/proc` 宿主身份，排除 exec/review/mcp 与嵌套 OMP。每面板一个只读观察器，不监听端口，不拦截模型请求。会话切换更新同一个观察器；退出清理 metadata，异常宿主退出后约 1 秒清理。Herdr metadata TTL 为 5 秒。

`$model` 从 Hook 或顶层配置初始化，随后跟随 Rollout 的模型变更。`$tps` 使用服务端 `token_usage_record.usage.output_tokens`（含隐藏推理），按每个推理步/工具步刷新，保留一位小数。优先使用生成项时间戳，缺失时回退请求间隔或任务时长；不估算文本 token。长回复中途的速率更新频率取决于原生日志刷盘。

单 `opencode-go` Key 由 Rust 采集器直接归因为 `single_credential`。历史 `herdr-api-key-sticky-v1` 日志仍可读取，已停用旧凭据不参与新请求自动归因，不再读取 `agent.db` 的 cache 表。pro2 历史会话保留。

详见 [维护指南](MAINTENANCE.md) 与 [实施计划](../../docs/custom-extensions-optimization-plan.md)。


根 OMP 标记现在保存进程 PID，同一 PID 的 `/reload-plugins` 可以继续发布，子进程会被拦截。迁移前已写入旧值 `1` 的会话需正常退出并重开一次；不放宽嵌套保护来猜测归属。

Codex Hook 使用安装时的 Node 绝对路径，以支持无完整 PATH 的 GUI 启动。卸载该 nvm Node 版本前，先在保留的新 Node 环境重跑 `npm run herdr:tps:install`；安装器不会自行写入信任索引，命令变更后按 Codex 原生信任提示确认。
