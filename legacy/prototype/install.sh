#!/bin/sh
set -eu
for cmd in python3 magick qdbus6 plasma-apply-colorscheme systemctl kwriteconfig6; do
  command -v "$cmd" >/dev/null || { printf 'Missing dependency: %s\n' "$cmd" >&2; exit 1; }
done
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install -d "$HOME/.local/share/wallpaper-style" "$HOME/.local/bin" "$HOME/.config/systemd/user" "$HOME/.config/wallpaper-style" "$HOME/.local/state/wallpaper-style"
install -m755 "$script_dir/wallpaper_accent.py" "$HOME/.local/share/wallpaper-style/wallpaper_accent.py"
install -m644 "$script_dir/opacity-template.js" "$HOME/.local/share/wallpaper-style/opacity-template.js"
install -m644 "$script_dir/effect-metadata.json" "$HOME/.local/share/wallpaper-style/effect-metadata.json"
ln -sfn ../share/wallpaper-style/wallpaper_accent.py "$HOME/.local/bin/wallpaper-style"
if [ ! -f "$HOME/.config/wallpaper-style/settings.json" ]; then
  install -m644 "$script_dir/settings.example.json" "$HOME/.config/wallpaper-style/settings.json"
fi
if [ ! -f "$HOME/.local/state/wallpaper-style/codex-glass-original" ]; then
  kreadconfig6 --file kwinrc --group Plugins --key codex-glassEnabled --default false > "$HOME/.local/state/wallpaper-style/codex-glass-original"
fi
"$HOME/.local/bin/wallpaper-style" --setup-effect
cat > "$HOME/.config/systemd/user/wallpaper-accent.service" <<'EOF'
[Unit]
Description=Apply wallpaper-inspired Plasma customization
After=graphical-session.target
PartOf=graphical-session.target
[Service]
Type=simple
ExecStart=%h/.local/bin/wallpaper-style
Restart=on-failure
RestartSec=10
[Install]
WantedBy=graphical-session.target
EOF
systemctl --user daemon-reload
systemctl --user enable --now wallpaper-accent.service
systemctl --user restart wallpaper-accent.service
printf '%s\n' 'Installed and enabled Wallpaper Style.'
