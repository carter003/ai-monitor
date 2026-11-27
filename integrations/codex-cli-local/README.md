# 全局 Codex CLI 升级报告

本补丁基于 OpenAI Codex 的 `rust-v0.172.0`，提交 `3d86e43b075adeba2193ec5c22ac92f55afd82e6`。它适用于当前 Linux 用户在各目录启动的原生交互式 CLI。

- 手动执行 `/clear` 或 `/clear 名称` 时，TUI 将当前线程实际生效的模型和推理强度交给新线程。数据只来自触发命令的会话。`/new` 和批准计划时的“清空并执行”继续使用原有流程。Cloud 线程的启动设置由服务端决定。
- 用 `/plan` 或模式切换键进入 Plan 时保留当前模型。Plan 的推理强度继续按原有配置解析。在 Plan 中手动清空会继承清空时生效的强度。

## 如何升级

1. **更新官方 CLI。** 在终端运行 `codex update`。若用户已经更新，直接读取已安装的官方版本。旧补丁绑定旧版，官方升级后需为新版重新安装补丁。
2. **准备匹配的源码。** 从本仓库开始执行下面的命令。按官方二进制的实际版本选择 tag，并使用独立目录保留旧工作区。

```sh
integration="$(git rev-parse --show-toplevel)/integrations/codex-cli-local"
official="$HOME/.codex/packages/standalone/current/bin/codex"
version=$("$official" --version | awk '{print $NF}')
mkdir -p "$HOME/.local/src"
source_dir=$(mktemp -d "$HOME/.local/src/codex-cli-$version.XXXXXX")
git clone --depth 1 --branch "rust-v$version" https://github.com/openai/codex.git "$source_dir"
cd "$source_dir"
git rev-parse HEAD
git apply --check "$integration/local-fixes.patch"
```

3. **迁移、构建和测试。** 兼容性检查通过后，按下面的命令应用补丁。如果检查失败，按下方代码位置迁移到新实现，用两个回归用例验证，再从目标 tag 导出更新后的补丁。使用 `codex-rs/rust-toolchain.toml` 指定的工具链和上游 Linux 构建依赖。发布 tag 有时只改了 workspace 的版本号；遇到锁文件报错时，核对并同步本地 workspace 包版本，再使用 `--locked` 构建。

```sh
git apply "$integration/local-fixes.patch"
cd codex-rs
cargo build --release --locked -p codex-cli --bin codex
RUST_MIN_STACK=33554432 cargo test --release --locked -p codex-tui --lib replacement_uses_server_defaults_and_preserves_explicit_launch_settings
RUST_MIN_STACK=33554432 cargo test --release --locked -p codex-tui --lib entering_plan_keeps_selected_model_over_catalog_preset
```

4. **安装并验收全局入口。** 确认两个测试均实际运行并通过后，在同一目录执行以下命令。然后从全局入口新开两个会话，按文末的双会话场景核对清空后首个用户请求及 Plan 切换，记录新 tag、提交、补丁和二进制 SHA-256。

```sh
package=$("$integration/install.sh" target/release/codex)
test "$(readlink -f "$(command -v codex)")" = "$package/launch"
test "$package/upstream" -ef "$official"
sha256sum "$integration/local-fixes.patch" "$package/bin/codex"
```

## 实现和代码位置

| 位置 | 维护内容 |
| --- | --- |
| [local-fixes.patch](local-fixes.patch) | 仓库中可迁移的 Rust 源码和回归用例。以下源码路径相对于上游 `codex-rs/tui/src/`。 |
| `app/session_lifecycle.rs` | `start_fresh_session` 识别手动清空；`prepare_fresh_session` 在创建线程前，将当前会话的模型和有效推理强度写入本次请求配置。 |
| `chatwidget/settings.rs` | `set_collaboration_mask_from_user_action` 在用户进入 Plan 时沿用当前模型，推理强度交给既有 Plan 配置解析。 |
| `app/tests/new_session_tests.rs`、`chatwidget/tests/plan_mode.rs` | 分别验证 `thread/start` 实际参数和 Plan 预设指定另一模型时的行为。 |
| [install.sh](install.sh)、[launch](launch) | 安装器校验版本和产物哈希并切换全局入口；启动器按官方基线选择可执行文件，并把 `codex update` 交给官方程序。 |

## 全局入口与回滚

安装器为当前用户的 standalone 安装保留一份版本匹配的资源和辅助程序，通过原子重命名切换 `~/.local/bin/codex`。在匹配的官方版本上，新启动的 CLI 使用补丁。执行 `codex update` 时入口转交官方安装器。若官方 `current` 已切到新版本，入口会使用新官方版本，待补丁迁移后重新安装。现有进程在下次启动时使用新入口。

查看当前入口、补丁包位置及官方基线可以执行以下命令。

```sh
command -v codex
readlink -f ~/.local/bin/codex
readlink -f ~/.codex/packages/standalone/current/bin/codex
```

以下命令可恢复本用户原有的官方动态入口。

```sh
ln -sfnT "$HOME/.codex/packages/standalone/current/bin/codex" "$HOME/.local/bin/codex"
```

## 0.172.0 本机验收

2026-11-27 使用全局入口启动两个隔离终端，将模型请求发往本机模拟服务。A 选 Sol/medium 后，B 完成 Astra/low 回合。A 执行 `/clear` 的首个用户请求仍为 Sol/medium。通过 `/plan` 和 Shift+Tab 进入 Plan 时实际请求为 Sol/high，与测试配置的 Plan 强度一致。在 Plan 中执行 `/clear fresh` 后，请求沿用 Sol/high。B 继续使用 Astra/low。内嵌服务端及通过 Unix 套接字连接同版本官方 app-server 均通过验证。

补丁 SHA-256 为 `6a7e8b3b9f7157004968b987f0a935bab87946686eb79ef0433cc59570af2422`。本机 Linux GNU 可执行文件 SHA-256 为 `22dee81da43df2980fbe6c8195a46deee95851e7a007efc708b089face962a84`。

Rust 回归覆盖 62 项 Plan 用例和 31 项新会话用例。本机测试进程使用 `RUST_MIN_STACK=33554432`。原样运行时有一份上游界面快照预期开发版标题 `v0.0.0`，实际为发布版 `v0.172.0`。仅临时对齐快照中的版本号后，31 项全部通过。原快照已还原，版本号调整未进入补丁。
