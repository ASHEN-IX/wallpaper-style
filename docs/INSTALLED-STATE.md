# Installed state on this machine

Recorded 2026-11-01 so anything can be put back exactly.

## What was installed

| Item | Path |
|---|---|
| CLI | `~/.local/bin/wallpaper-style-rs` |
| GUI | `~/.local/bin/wallpaper-style-gui` |
| Desktop entry | `~/.local/share/applications/io.github.wallpaperstyle.desktop` |
| Icon | `~/.local/share/icons/hicolor/scalable/apps/io.github.wallpaperstyle.svg` |
| Service unit | `~/.config/systemd/user/wallpaper-style.service` (enabled, running) |
| Settings | `~/.config/wallpaper-style/settings.toml` |
| Snapshots | `~/.local/state/wallpaper-style/snapshots/` |

The CLI is named `wallpaper-style-rs`, **not** `wallpaper-style`, because
`~/.local/bin/wallpaper-style` is the prototype's symlink and comes first on
`PATH`. Overwriting it would have made the prototype's own service
(`ExecStart=%h/.local/bin/wallpaper-style`) execute the new binary, which needs a
subcommand and would fail on every boot.

## What was changed

* **Prototype service disabled and stopped** (`wallpaper-accent.service` is
  `disabled` and `inactive`; the autostart symlink under
  `graphical-session.target.wants/` was removed). It had to be *disabled*, not
  merely stopped: its main loop resets `previous = None` on every start, so its
  first iteration always re-applies the prototype's colour scheme and Kvantum
  theme. Left enabled it would have overwritten the new theme on the first boot
  and then competed with the new service over the same settings.
  Stopping matters too, because two effects setting opacity multiply —
  0.80 × 0.80 = 0.64.
* **Prototype effect unloaded** and `wallpaper-styleEnabled=false` in `kwinrc`.
* New effect `wallpaper-style-transparency` installed and **loaded**
  (`wallpaper-style-transparencyEnabled=true`).
* Plasma colour scheme: `Wallpaper Style Dark 8395f0-181a23`.
* Kvantum theme: `WallpaperStyleDark8395f0`, derived from `WallpaperStyle5F6FDD`.
* Transparency enabled for the same nine presets at the prototype's opacities.

## Previous state, for comparison

```
wallpaper-accent.service      enabled, active
wallpaper-styleEnabled        true
ColorScheme                   WallpaperStyle-5f6fdd
Kvantum theme                 WallpaperStyle5F6FDD
```

Saved verbatim in `~/.local/state/wallpaper-style-backup/prototype-state.txt`.

## Undo

```bash
# 1. Remove everything the new application owns and revert the theme
~/.local/bin/wallpaper-style-rs uninstall

# 2. Restore the prototype exactly as it was
systemctl --user enable --now wallpaper-accent.service
kwriteconfig6 --file kwinrc --group Plugins --key wallpaper-styleEnabled --type bool true
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.loadEffect wallpaper-style
plasma-apply-colorscheme WallpaperStyle-5f6fdd
kwriteconfig6 --file Kvantum/kvantum.kvconfig --group General --key theme WallpaperStyle5F6FDD
```

Step 1 first, so its restore does not fight the prototype for the scheme.

The prototype's own files (`~/.local/share/wallpaper-style/`,
`~/.local/share/kwin/effects/wallpaper-style/`, its Kvantum themes and colour
schemes, `~/.local/bin/wallpaper-style`) were never modified.

## Launcher and panel colours: a real limitation

Your Plasma style is **Mokka**, and its widget artwork is self-contained. The
files under `/usr/share/plasma/desktoptheme/Mokka/widgets/` carry their own
colours rather than referring to the colour scheme:

| File | Colours found |
|---|---|
| `button.svg` | 26× `#ff00ff`, 25× `#ffffff`, 24× `#000000`, 6× `#3daee9` |
| `listitem.svgz` | 14× `#4e9a06`, 10× `#3daee9`, 6× `#232629` |
| `background.svgz` | 8× `#ff00ff`, 8× `#000000`, 6× `#3daee9`, 6× `#31363b` |

`Mokka/colors` contains nothing but an empty `[Colors:Button]` header, so it
supplies no override either.

A Plasma style that pins colours this way ignores whatever colour scheme is
active. The Kickoff launcher's panel, menu items, search field and hover states
all come from Mokka's SVGs, so they keep Mokka's palette no matter what
Wallpaper Style writes. This is a property of the theme, not of the generated
scheme: the scheme's own values are correct and applied
(`Colors:Selection` = `133,157,204`, which matches the wallpaper accent).

What *does* follow the wallpaper:

* every Qt application (Dolphin, Kate, Ark, System Settings) through the
  generated Kvantum theme;
* window decorations and most in-window widgets through the colour scheme;
* the accent used for selection inside Qt apps.

What does not:

* the Plasma shell's widgets — launcher, panel, plasmoids — under a
  self-contained style such as Mokka.

To make the shell follow the wallpaper too, use a style whose artwork refers to
scheme colours (Breeze does), or edit Mokka's SVGs, which is a change to a
system file and outside what this application should do on its own.
