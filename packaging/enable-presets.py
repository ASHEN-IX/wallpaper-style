#!/usr/bin/env python3
"""Enable the transparency presets the prototype served, in the new settings file.

Run once during installation. Kept in the repo rather than inlined so the exact
change is reviewable, and idempotent so re-running it changes nothing.
"""
import pathlib
import tomllib

APPS = {
    "claude": 0.80,
    "codex": 0.80,
    "vscode": 0.82,
    "cursor": 0.82,
    "browsers": 0.82,
    "garuda-toolbox": 0.85,
    "steam": 0.85,
    "heroic": 0.85,
    "virtualbox-manager": 0.88,
}

path = pathlib.Path.home() / ".config/wallpaper-style/settings.toml"
text = path.read_text()

# Rewrite the whole [transparency.apps] block. Deleting and re-adding is simpler
# and safer than editing in place: the block has no hand-written content in a
# freshly installed file.
lines = text.splitlines()
out, skipping = [], False
for line in lines:
    if line.strip() == "[transparency.apps]":
        skipping = True
        out.append(line)
        for key, value in APPS.items():
            out.append(f'{key} = {{ opacity = {value}, enabled = true }}')
        continue
    if skipping and line.startswith("["):
        skipping = False
    if skipping:
        continue
    out.append(line)

path.write_text("\n".join(out) + "\n")

# Verify by parsing what was written.
data = tomllib.loads(path.read_text())
got = data["transparency"]["apps"]
print(f"transparency.enabled = {data['transparency']['enabled']}")
print(f"themes.kwin_effect  = {data['themes']['kwin_effect']}")
print(f"apps configured     = {len(got)}")
for key in sorted(got):
    print(f"  {key:20} {got[key]['opacity']}")
