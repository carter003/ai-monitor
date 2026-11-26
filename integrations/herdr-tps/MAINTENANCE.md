# 原生扩展维护

入口与运行条件见 [README](README.md)。维护时保留用户已有修改，不重启 Codex Web GPT 服务。

## 模块边界

| 文件 | 职责 |
| --- | --- |
| `omp-subagent-model.js` | 未配置 task 角色与定义匹配的 eval 默认角色继承父模型 |
| `omp-herdr-tps.js` | 根 UI 会话的模型、原生 Tokenizer、用量校正和历史 TPS |
| `lib/herdr-metadata-publisher.mjs` | Unix Socket 发布、串行队列、TTL、错误隔离 |
| `codex-herdr-hook.js` | 根会话校验、原子状态、同面板实例复用、脱离 Hook 管道 |
| `lib/codex-host.mjs` | `/proc` PID/starttime 身份、命令门禁、配置初始模型 |
| `codex-herdr-sidecar.js` | 增量文件读取、会话切换、心跳和宿主退出清理 |
| `lib/codex-rollout-follower.mjs` | UTF8 半行保护、模型事件、权威 output tokens 与生成时长 |
| `install.mjs` | 预检、备份、原子二进制归位、扩展/Hook 安装与卸载 |

模型继承内部路径通过 try/catch 隔离；TPS 只在 Herdr 环境加载公开 Tokenizer。OMP 进入根 UI 后设置 `HERDR_TPS_OMP_TOPLEVEL=String(process.pid)`；同 PID 热重载放行，派生进程 PID 不同则禁止上报。Codex Hook 对任何非空 OMP 标记退出。Publisher 在创建时保存资格，避免根进程自身标记关闭已启动的发布器。

Codex 状态位于 `${XDG_RUNTIME_DIR:-/run/user/$UID}/herdr-tps/codex-<pane hash>.json`，权限 0600。身份为 PID 与 starttime，避免 PID 复用。Hook 用排他锁串行化，状态经临时文件 rename 更新。后台进程必须 `detached: true, stdio: 'ignore'` 并 `unref()`，不得继承 Codex 等待 EOF 的输出管道。

新增 Hook 始终追加独立分组，不挪动官方 SessionStart 首组，不改 `[hooks.state]` 或 `.hook-trust`。用户首次打开 TUI 通过原生信任提示确认。卸载只能移除受管新分组，保留官方条目和原生 CLI。

## 回归

```sh
npm run herdr:tps:test
node integrations/herdr-tps/install.mjs --dry-run
node --test integrations/codex-chatgpt-web-local/upgrade.test.mjs
cargo test --offline --locked
git diff --check
```

OMP 契约测试运行本机真实二进制，验证 `.js` 软链自动发现、公开 Tokenizer、默认 task/eval 继承、显式 eval 保留、同 PID 热重载、跨 PID 阻断和 `--no-tools` 跳过。可用 `HERDR_TPS_OMP_TEST_BIN` 指定被验证的版本，无需 npm install。

Codex 测试使用临时目录、模拟宿主进程与 Unix Socket，覆盖半行字节追加、隐藏 output tokens、模型切换、复用 sidecar、过期结束事件和异常宿主退出。真实 CLI 参数烟雾使用 `codex review --help`，无需调用模型。

Rust 采集口径：优先采集 `token_usage_record`，同文件一旦出现该记录自动忽略 `token_count`；仅无 `token_usage_record` 的旧日志回退 `token_count`。OMP 单 Key 自动归因从健康 auth_credentials 读取，过滤禁用记录；老数据库没有 disabled_cause 列时兼容读取。历史 pin 时间线优先，历史对账保留已采集的 single_credential 来源。

## 安装与恢复

先运行 `--dry-run` 核对路径，再执行安装。普通配置保留同目录 `.bak-herdr-tps-<timestamp>` 备份；软链备份归档至 `~/.local/state/herdr-tps/backups/<timestamp>/`，安装器也会归档 PATH 内已悬空的旧受管备份，并移除空的 dispatcher 目录；写入后的 Shell/TOML 校验失败会恢复本次修改。卸载：

```sh
node integrations/herdr-tps/install.mjs --uninstall
```

卸载后原生 OMP/Codex 仍可使用；需要恢复自定义 Shell 或配置内容时，先对比对应备份再还原。不要恢复已退役的 wrapper 或对正在运行的进程执行强制切换。pro2 sessions 与官方状态灯扩展不属于清理目标。

Codex Web GPT 的升级、Runtime 清单、Native2 补丁继续通过 [独立维护入口](../codex-chatgpt-web-local/README.md)管理。


Codex Sidecar 每次切换先 `resetForAgent(5000)`，即使没有初始 model 也清掉旧来源；单次 tick 读完当前文件尾再统一发布快照。`TokenRateMeter.observe` 按每步服务端 output tokens 与生成时长累加指数桶，历史按 5/20/80 秒的生成时间衰减，不把工具空闲时间加入生成分母。文件截断/替换同时重置 UTF8 半行解码与 response 去重状态。

Agent 定义由 OMP 原生 `task/discovery` 在扩展初始化时只读发现，包含项目、用户及插件优先级；配置修改后重载扩展刷新缓存。默认 model 可能为数组，只有唯一角色选择器与事件相符时才处理 eval。事件目前不包含 perCallModel 来源，内置 Agent 显式传入同一默认角色无法进一步区分，见 README 边界说明。

更换 nvm Node 后重跑安装器，更新 Codex Hook 绝对路径；原生信任由 Codex 提示处理。采集器源码更新后还需构建 Release、原子安装并重启 `herdr-usage-collector`，不能用单元测试通过代替运行中可执行文件哈希核验。
