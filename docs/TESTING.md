# What was tested

Everything here was run on **KDE Plasma 6.8.1 / KWin 6.8.1, Wayland, Arch Linux**,
on 2026-11-01. Nothing was run against the live desktop: every apply, service tick
and restore used a temporary XDG tree, and the command runner records calls rather
than executing them.

## Automated

```bash
cargo test --workspace     # 296 tests
cargo clippy --all-targets # no warnings
cargo fmt --all --check
```

| Suite | Tests | Covers |
|---|---|---|
| `ws-palette` | 50 | OKLab reference values, WCAG reference points, gamut mapping, clustering across the hue circle, monochrome and transparent images, resolution independence, truncated/garbage files |
| `ws-core` | 99 + 1 doc | Settings round trip and migration, prototype JSON import, rejection of unknown keys and future schemas, the `Default` regression that produced transparent backgrounds, float formatting, INI editing, ownership merge rules, three-state restore |
| `ws-plasma` | 105 | Colour-scheme section completeness and per-section contrast, Kvantum recolouring without touching the base theme, hex-token safety, effect generation and escaping, Plasma config parsing, environment/availability reporting, installer idempotence |
| `ws-plasma` integration | 11 | Apply → snapshot → restore against a real temporary tree; conflict preservation; sandbox-escape check; KWin effect lifecycle in a simulated compositor; JS syntax |
| `ws-service` | 18 | Automatic mode off, first apply, no-op on unchanged input, same-path content change, restart detection, undecodable image, no wallpaper, apply failure, backoff, snapshot creation |
| `ws-gui` | 8 | Headless render of cards, pills, swatches and the window preview; a full page layout; preview text contrast |
| `ws-backend` | 3 | Availability states, feature catalogue |

## Manual, against the temporary tree

```
install --no-enable     → writes unit + marker; second run reports "unchanged"
settings --set …        → writes schema-versioned TOML; values read back correctly
preview --image …       → prints accent, clusters, palette, contrast, plan; writes nothing
apply --image …         → snapshot 20261101T154614-0000, 4 files, restorable
service once            → "Applied #8395f0" then "No change"
change image, restart   → "Applied #cf8688" (accent follows the new image)
restore                 → "Restored 4 change(s)"; scheme file removed
doctor / status         → correct environment and feature reporting
export-preview          → SVG specimen, rendered to docs/preview.png
```

The generated `.colors` file was additionally verified with an independent
contrast calculation outside the codebase: all seven sections ≥ 4.5:1.

## Bugs found and fixed during this work

Recorded because each one is now covered by a regression test:

1. `PaletteSettings` derived `Default`, so `background_alpha` defaulted to 0 and
   every generated theme would have had a fully transparent background. The
   serde `default = "opaque"` attribute only applied when reading a file.
2. `f32` opacity 0.8 serialised as `0.800000011920929` because `toml` widens to
   `f64`; the file is required to be human-readable.
3. Hue clustering scaled vote weight by chroma, so a legitimate majority hue
   could never reach the "dominant" threshold.
4. Window-class matching split on whitespace, making the multi-word
   `VirtualBox Manager` preset unmatchable.
5. Kvantum cleanup removed only the listed applications from a stale mapping,
   stranding the rest on a theme that no longer existed.
6. The service built an empty snapshot, so automatic applies could not be undone.
7. The service stored only the wallpaper path, so a restarted watcher could not
   tell that the image had changed while it was down.
8. The service ignored a manually chosen image saved from the GUI.
9. Restore left an empty effect directory, so `status` still reported the effect
   as installed.

## Found by running it for real

Installing into a live Plasma 6.8 Wayland session exposed what no simulation had:

**`window.addRepaintFull()` does not exist.** KWin exposes `addRepaintFull` on the
effects handler, not on individual windows. The generated effect threw on the
first window it managed, so `loadEffect` returned `false` and the effect never
loaded — while `isEffectSupported` still reported `true`. The simulated compositor
had been *too permissive*: it provided `window.addRepaintFull`, so it certified a
script that could never run.

Two changes followed:

1. The effect calls `effects.addRepaintFull()` inside a guarded helper, so a
   future API difference degrades a repaint hint instead of killing the effect.
2. The simulation no longer provides `window.addRepaintFull`. Re-running it
   against the old script produces 20 failures, which is what should have
   happened the first time.

Two further bugs surfaced during installation:

* The GUI passed its own executable to the installer, producing a systemd unit
  running `wallpaper-style-gui service run` — a CLI subcommand that does not
  exist there. The unit would have failed on every boot.
* The desktop entry used `Exec=wallpaper-style-gui` and bare
  `wallpaper-style apply` actions. `~/.local/bin` is first in `PATH` and holds
  the *prototype's* `wallpaper-style`, so the actions launched the old script.
  The install therefore uses `wallpaper-style-rs` for the CLI, and the entry
  names the action binary explicitly.

Verified afterwards on the live session: `isEffectLoaded → true`, the effect is
in `activeEffects`, and the KWin journal shows no errors since loading.

## Found by listening to the user

Three more issues surfaced only because the user reported what they saw:

**The transparency was not applied.** The effect had been fixed, rebuilt and
installed, and every command reported success — yet nothing changed. The service
was still running the *previous* binary, started at 17:14:33; the fix was
installed at 17:20:54. It kept regenerating the broken effect from memory on
every poll, silently undoing the fix. `install` used `systemctl enable --now`,
which starts a stopped unit and does nothing to a running one. It now restarts.

**The shell accent did not follow the wallpaper.** Kickoff's selection pill and
the category icons stayed blue while Qt applications switched correctly. Sampling
the screenshot gave `82,85,95` and `95,106,133` — blue-grey, unrelated to the
applied `#cf8688`, so the shell was applying *an* accent, just not ours. The cause:
Plasma 6 reads `AccentColor` from `[General]`, not `[Colors:Selection]`, and
`plasma-apply-colorscheme` does not copy that key into `kdeglobals`. Apply now
writes it. Confirmed by the user after restarting plasmashell.

**The prototype service would have reverted the theme at boot.** Its loop resets
its change signature on every start, so its first iteration always re-applies.
Stopping it was not enough; it had to be disabled.

The lesson repeated across all three: a command exiting successfully is not
evidence that the system changed. Only checking the system, or asking the person
looking at it, is.

## Not tested

See [LIMITATIONS.md](LIMITATIONS.md#not-tested). In particular: no live KWin
session, no X11, no multiple physical monitors beyond unit tests, no Plasma
version other than 6.8.1.
