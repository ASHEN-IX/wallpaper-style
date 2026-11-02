#!/bin/sh
set -eu
systemctl --user disable --now wallpaper-accent.service 2>/dev/null || true
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.unloadEffect wallpaper-style >/dev/null 2>&1 || true
kwriteconfig6 --file kwinrc --group Plugins --key wallpaper-styleEnabled --type bool false
if [ "$(cat "$HOME/.local/state/wallpaper-style/codex-glass-original" 2>/dev/null || true)" = true ]; then
  kwriteconfig6 --file kwinrc --group Plugins --key codex-glassEnabled --type bool true
  qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.loadEffect codex-glass >/dev/null 2>&1 || true
fi
rm -f "$HOME/.config/systemd/user/wallpaper-accent.service" "$HOME/.local/bin/wallpaper-style"
systemctl --user daemon-reload
printf '%s\n' 'Stopped Wallpaper Style; current color and Kvantum themes remain selected. Settings and generated files were retained.'
