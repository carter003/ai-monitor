#!/bin/sh
set -eu

workspace_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install_bin_dir=${AI_MONITOR_INSTALL_DIR:-"$HOME/.local/bin"}

echo "==> 编译工作区 (Release)..."
cd "$workspace_dir"
cargo build --release --all-features

mkdir -p -- "$install_bin_dir"

# 安装 ai-monitor 到 ~/.local/bin
ai_monitor_bin="$workspace_dir/target/release/ai-monitor"
if [ -x "$ai_monitor_bin" ]; then
    temp_bin=$(mktemp "$install_bin_dir/.ai-monitor.XXXXXX")
    trap 'rm -f -- "$temp_bin"' EXIT HUP INT TERM
    install -m 755 -- "$ai_monitor_bin" "$temp_bin"
    mv -f -- "$temp_bin" "$install_bin_dir/ai-monitor"
    printf '已安装 TUI 看板: %s/ai-monitor\n' "$install_bin_dir"
fi

# 安装 herdr-usage 与 import_prices 到 ~/.local/bin (可选便利)
herdr_usage_bin="$workspace_dir/target/release/herdr-usage"
if [ -x "$herdr_usage_bin" ]; then
    install -m 755 -- "$herdr_usage_bin" "$install_bin_dir/herdr-usage"
    printf '已安装采集器:   %s/herdr-usage\n' "$install_bin_dir"
fi

import_prices_bin="$workspace_dir/target/release/import_prices"
if [ -x "$import_prices_bin" ]; then
    install -m 755 -- "$import_prices_bin" "$install_bin_dir/import_prices"
    printf '已安装价格同步: %s/import_prices\n' "$install_bin_dir"
fi


printf '\n安装完成！\n'
printf '  • 启动 TUI 监控台: ai-monitor\n'
printf '  • 测试采集器:     HERDR_USAGE_DB=/tmp/probe.db herdr-usage\n'
printf '  • 更新价格库:     import_prices\n'
