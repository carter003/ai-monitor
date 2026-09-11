#!/bin/sh
set -eu

ai_monitor_source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ai_monitor_install_dir=${AI_MONITOR_INSTALL_DIR:-"$HOME/.local/bin"}
ai_monitor_binary="$ai_monitor_source_dir/target/release/ai-monitor"
if [ ! -x "$ai_monitor_binary" ]; then
    ai_monitor_binary="$ai_monitor_source_dir/ai-monitor"
fi
if [ ! -x "$ai_monitor_binary" ]; then
    printf '%s\n' '未找到已编译程序，请先运行 cargo build --release --locked。' >&2
    exit 1
fi
mkdir -p -- "$ai_monitor_install_dir"
ai_monitor_temp=$(mktemp "$ai_monitor_install_dir/.ai-monitor.XXXXXX")
trap 'rm -f -- "$ai_monitor_temp"' EXIT HUP INT TERM
install -m 755 -- "$ai_monitor_binary" "$ai_monitor_temp"
mv -f -- "$ai_monitor_temp" "$ai_monitor_install_dir/ai-monitor"
printf '已安装：%s/ai-monitor\n' "$ai_monitor_install_dir"
printf '%s\n' '启动：ai-monitor'
