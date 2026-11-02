# Prototype (archived)

The original Python implementation of Wallpaper Style, kept for reference.

This is **not** the product and is not installed or run by anything here. The Rust
workspace in `crates/` replaces it. The files are preserved because the Rust
version was written by reading this code: several of its behaviours (the Kvantum
recolour approach, the desktop-scoped opacity animation, the colour-specific
scheme name) are documented in `docs/ARCHITECTURE.md` as lessons taken from it.

## Why it was replaced

* Required `python3` and ImageMagick at runtime.
* Single accent, no contrast checking, no light variant.
* No snapshot or restore, so changes could not be undone.
* The KWin effect could leave a window composited on the wrong virtual desktop.

## If you still have it installed

The prototype may be present under `~/.local/share/wallpaper-style/` with a
service called `wallpaper-accent.service`. The new application deliberately does
not touch it. To retire it:

```bash
systemctl --user disable --now wallpaper-accent.service
qdbus6 org.kde.KWin /Effects org.kde.kwin.Effects.unloadEffect wallpaper-style
kwriteconfig6 --file kwinrc --group Plugins --key wallpaper-styleEnabled --type bool false
```

Its own files, themes and colour schemes are left in place.

## Licence

Same as the project: MIT.
