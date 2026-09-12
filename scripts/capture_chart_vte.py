#!/usr/bin/env python3
"""Render Rust-exported ANSI fixtures in GTK/VTE; no UI is drawn in Python.

Run under Xvfb, e.g.:
  xvfb-run -a /usr/bin/python3 scripts/capture_chart_vte.py /tmp/chart-previews
Requires python3-gi, gir1.2-vte-2.91, xvfb and a CJK monospace font.
Inputs come from export_real_chart_terminal_fixtures (CHART_PREVIEW_DIR) or
export_real_model_table_terminal_fixtures (MODEL_PREVIEW_DIR).
The synthetic fixtures never read real account credentials.
"""
from pathlib import Path
import re
import sys
import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Vte", "2.91")
from gi.repository import Gdk, GLib, Gtk, Pango, Vte


def rgba(value):
    color = Gdk.RGBA()
    if not color.parse(value):
        raise ValueError(value)
    return color


def capture(source):
    match = re.fullmatch(r"(?:charts|models)-(\d+)x(\d+)-(equal|varied|sample)\.ansi", source.name)
    if not match:
        raise ValueError(f"Unexpected fixture name: {source.name}")
    columns, rows = int(match[1]), int(match[2])
    window = Gtk.Window()
    window.set_decorated(False)
    terminal = Vte.Terminal()
    font_size = 16 if source.name.startswith("models-") else 11
    terminal.set_font(Pango.FontDescription(f"Noto Sans Mono CJK SC {font_size}"))
    terminal.set_color_background(rgba("#FAFAFA"))
    terminal.set_color_foreground(rgba("#111827"))
    terminal.set_scrollback_lines(0)
    terminal.set_size(columns, rows)
    window.add(terminal)
    window.show_all()
    errors = []

    def save():
        try:
            if terminal.get_column_count() != columns or terminal.get_row_count() != rows:
                raise RuntimeError(f"Expected {columns}x{rows}, got "
                                   f"{terminal.get_column_count()}x{terminal.get_row_count()}")
            allocation = terminal.get_allocation()
            pixels = Gdk.pixbuf_get_from_window(
                terminal.get_window(), allocation.x, allocation.y,
                allocation.width, allocation.height)
            if pixels is None:
                raise RuntimeError("VTE screenshot capture failed")
            target = source.with_suffix(".png")
            pixels.savev(str(target), "png", [], [])
            print(f"VTE {columns}x{rows}: {target}", flush=True)
        except Exception as error:
            errors.append(error)
        finally:
            window.destroy()
            Gtk.main_quit()
        return False

    def feed():
        terminal.feed(source.read_bytes())
        GLib.timeout_add(650, save)
        return False

    GLib.timeout_add(250, feed)
    Gtk.main()
    if errors:
        raise errors[0]


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: capture_chart_vte.py PREVIEW_DIRECTORY")
    sources = sorted(Path(sys.argv[1]).glob("*.ansi"))
    if not sources:
        raise SystemExit("No Rust-generated ANSI fixtures found")
    for source in sources:
        capture(source)
