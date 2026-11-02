# Limitations

Everything below is a real, current constraint. Where a feature is limited, the
application detects it, reports it in `wallpaper-style status` and in the control
centre's Availability panel, and disables the corresponding control rather than
offering something that does nothing.

## Platform

**KDE Plasma 6 on Wayland only.** This is first-class support: everything in the
README has been exercised on Plasma 6.8 with KWin on Wayland on Arch.

| Environment | Status |
|---|---|
| KDE Plasma 6, Wayland | Supported |
| KDE Plasma 6, X11 | Untested. The code paths are the same and the effect is scripted, so it is expected to work, but it has not been verified. `status` reports it as degraded rather than supported. |
| KDE Plasma 5 | Not supported. `kwriteconfig5`/`kreadconfig5` are used only as a fallback name; the KWin effect API and Plasma starter differ. |
| GNOME / wlroots / Sway / Hyprland | Not supported. The backend reports every feature unavailable and the GUI explains why. |
| XDG portals | Cannot provide any of this. Portals do not expose the current wallpaper or allow changing another window's opacity. |

## Wallpaper detection

* The wallpaper is read from Plasma's scripting interface (`qdbus6 …
  org.kde.PlasmaShell.evaluateScript`) and, when that is unavailable, from
  `plasma-org.kde.plasma.desktop-appletsrc`.
* **One monitor drives the palette.** The lowest-numbered screen with a usable
  image wins, matching how the prototype behaved. When monitors show different
  images, `status` says which screen was used.
* **Third-party wallpaper plugins work only if they publish an `Image` value.**
  A plugin that renders dynamically without storing a file (some shader-based
  wallpapers) cannot be read; the application says so and offers manual image
  selection.
* **Slideshows** are followed only when the plugin publishes the currently
  displayed frame under `Image`. That is true for `org.kde.slideshow`. When it is
  not, the palette stays on the frame that was current when the wallpaper was
  first detected.
* **Changes are detected by polling** (default every 5 seconds), not by watching
  the configuration file with inotify. A change at the same path is detected by
  hashing the first 64 KiB of the file, so an image replaced with different
  content is picked up even if its size and modification time are unchanged.
* A **missing or unreadable image** leaves the current theme untouched and is
  retried with backoff. It never clears the desktop to a default theme.
* **Video wallpapers** are not supported.

## Palette

* A **monochrome or near-monochrome** image has no hue to extract. A near-grey
  wash is used when it is present; otherwise the built-in fallback accent
  (`#3ddc97`) is used. `status` reports which happened.
* Accents are normalised into a luminance band that works as a UI colour. An
  extremely dark or extremely light wallpaper therefore yields an accent that is
  lighter or darker than the image itself, on purpose, so text on it stays
  readable.
* **Contrast is enforced at 4.5:1 for body text** and 3:1 for muted text.
  Accents that cannot carry readable text in either direction are moved in
  lightness, changing the hue not at all. In the rare case that no lightness
  works, the failure is reported rather than hidden.
* Manual colour overrides replace generated colours but are still contrast
  checked; the report shows the result.

## Theme application

* Colour schemes are written to `$XDG_DATA_HOME/color-schemes` and activated with
  `plasma-apply-colorscheme`. If that binary is missing, the file is still written
  and the user is told to select it in System Settings.
* The scheme name includes the accent and surface colour because Plasma will not
  re-apply a scheme whose name it has already activated, even if the file
  changed. That is why the name changes as the wallpaper does.
* **Kvantum theming recolours an existing base theme** chosen in Kvantum Manager.
  There is no API to generate a theme from nothing. If no base theme is
  configured, Kvantum support is skipped with an explanation.
* Kvantum is a **Qt 5/6 widget style**. It does not affect:
  * Electron applications (Claude, Codex, VS Code, Cursor, Heroic) — they draw
    their own surfaces;
  * GTK applications unless they use a matching GTK theme;
  * browsers, which paint their own UI.
* Applications that cache their style must be **restarted** to pick up a new
  Kvantum theme. `apply` says which ones.
* **Background-only transparency** relies on Kvantum's `translucent_windows`
  support and on the application actually honouring it. It is reported as
  degraded, not supported, for exactly this reason.

## Window transparency

* Implemented as a **scripted KWin effect**, which is the only supported way for
  one process to set another's opacity on Wayland. It requires KWin, a running
  compositor, and Plasma's scripted-effect API.
* Rules match on **window class only**. Two windows of the same application
  cannot be treated differently.
* **Games are excluded** by detecting Steam's `steam_app_<id>` class prefix. A
  game that does not use that prefix and whose class happens to match a rule
  cannot be distinguished from an application; there is no reliable signal for
  "this is a game".
* **VirtualBox guest windows** are excluded by class (`VirtualBox Machine`,
  `VirtualBoxVM`). Guest windows that report a different class are not covered.
* The effect **never moves, resizes, restacks or activates a window.** It only
  sets opacity, and it cancels that opacity the moment a window leaves the
  current desktop, leaves the current activity, is minimised, is hidden by
  show-desktop, or closes. This is verified by a simulated-compositor test
  (`crates/ws-plasma/tests/kwin_effect_lifecycle.js`).
* **Fullscreen windows are skipped by default.** A transparent fullscreen video
  or game is a bug, not a feature. The setting can be turned off.
* Whole-window opacity makes **text translucent too**. For applications that do
  not support background-only transparency there is no way to keep text opaque;
  the Transparency page labels each app accordingly.
* The effect id is `wallpaper-style-transparency`. The prototype's
  `wallpaper-style` and earlier `codex-glass` effects are **not** re-enabled or
  stacked with it; installing this application does not touch them, and its own
  effect is explicitly unloaded before being regenerated.

## Lifecycle

* Snapshots record file contents inline up to 512 KiB. A larger configuration
  file is recorded by path and hash only, so restoring it reports a conflict
  rather than silently failing. In practice every KDE file involved is far
  smaller.
* Restore **refuses to overwrite** anything that changed since the apply and
  reports it as a conflict. `--force` overrides deliberately.
* Restore puts back *values and files*, not running application state. An
  application that already read the old colour scheme keeps it until restarted.
* The systemd unit is generated and owned by the application. A user edit to it is
  reverted by the next `install`. That is intentional: it is a generated file,
  unlike theme files where user edits win.
* `pacman -R` (or removing the binaries another way) leaves applied colours and
  the KWin effect in place, because they live in the user's directories. Run
  `wallpaper-style uninstall` first, or delete the listed directories afterwards.
* Uninstalling does **not** delete settings, snapshots or generated themes, so a
  reinstall picks up where you left off. The uninstall output says where they are.

## Not tested

Recorded honestly rather than claimed:

* X11 sessions.
* Multiple physical monitors with different wallpapers beyond the selection
  policy's unit tests.
* Activities with per-activity wallpapers.
* Plasma versions other than 6.8.1.
* Kvantum base themes other than the Catppuccin/Mokka derivatives present on the
  development machine.
* Installation on distributions other than Arch.
* A live KWin session's compositor behaviour. The effect is validated by placing
  it in a simulated compositor, by syntax-checking the generated JavaScript, and
  by asserting it calls only APIs that exist in KWin 6.8's headers. It has **not**
  been exercised against a running KWin instance: this machine's live desktop was
  deliberately left untouched throughout development.

What *was* run end to end, against a temporary XDG tree on this Plasma 6.8
Wayland machine:

* `install` twice (second reports "unchanged"), `settings`, `preview`, `apply`,
  `snapshot list`, `restore`, `doctor`, `status`, `export-preview`.
* The generated `.colors` file was verified section by section with an
  independent contrast calculation: every section passed 4.5:1.
* The generated KWin effect passed `node --check` and the 24-case simulated
  compositor lifecycle run.
* Automatic mode: an unchanged wallpaper produced "No change"; replacing the
  image at the same path while the watcher was stopped produced a re-apply with a
  new accent, and a restorable snapshot.
* The release build was made from a clean source tarball with `--locked`.
## The Plasma shell's accent

Plasma 6 does **not** take the shell's accent from a colour scheme's
`[Colors:Selection]`. It reads a separate `AccentColor` key in `[General]`, and
`plasma-apply-colorscheme` does not copy that key out of the scheme file into
`kdeglobals`. Without it the shell keeps its previous accent: the Kickoff
selection pill, the category icons and other shell highlights stayed on the old
blue while every Qt application switched correctly.

Wallpaper Style now writes `AccentColor` itself, records it in the ownership
manifest so restore undoes it, and emits it in the generated scheme file for
Plasma's own colour picker.

**Verified on a live Plasma 6.8 Wayland session**: after applying and restarting
plasmashell, the Kickoff selection pill and the category icons changed from the
previous blue-grey to the wallpaper's accent. This was confirmed by the user
looking at the screen, not only by reading configuration files — the pixel
colours before the fix were `82,85,95` and `95,106,133`, unrelated to the
applied `#cf8688`.

Plasma reads the key when it loads its configuration. Changing it while the
session is running takes effect for newly started shell components, and fully
for the whole shell after a Plasma restart (log out and back in, or
`kquitapp6 plasmashell && kstart plasmashell`). This is a limitation of how
Plasma caches its config, not of the value written.

### Plasma styles that pin their own colours

A style such as **Mokka** ships widget artwork with colours baked into the SVGs
(`button.svg` contains 26× `#ff00ff`, `listitem.svgz` 14× `#4e9a06`) and an empty
`colors` file. Where such a style draws a widget from its own artwork, that
widget keeps its own colours.

On the verified session this affected only Mokka's custom-drawn shapes; the
accent-driven elements — the Kickoff selection pill and the category icons —
followed the wallpaper once `AccentColor` was written. So a pinned-colour style
and a working accent are not mutually exclusive: it depends on whether a given
widget is drawn from the theme's SVGs or painted from the accent.
