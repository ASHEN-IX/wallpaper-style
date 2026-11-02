# Architecture

## Shape of the solution

```
        ┌──────────────────────────────────────────────────────────┐
        │ ws-palette      image decode → OKLab → clusters → palette │
        │                 pure; no filesystem, no desktop           │
        └───────────────────────────┬──────────────────────────────┘
                                    │ PaletteReport
        ┌───────────────────────────▼──────────────────────────────┐
        │ ws-core         settings + migration, presets, wallpaper  │
        │                 selection policy, ownership, snapshots    │
        └───────────────────────────┬──────────────────────────────┘
                                    │ traits in ws-backend
        ┌───────────────────────────▼──────────────────────────────┐
        │ ws-plasma       Plasma 6 / KWin / Kvantum / systemd       │
        └───────────────────────────┬──────────────────────────────┘
                                    │
        ┌──────────────────┬────────┴─────────┐
        │ ws-service       │ ws-cli           │ ws-gui
        │ watcher loop     │ automation       │ control centre
        └──────────────────┴──────────────────┘
```

The dependency arrows only ever point downwards. `ws-palette` and `ws-core` do
not depend on any desktop crate, which is what makes the palette maths and the
policy unit-testable without a display server, and what would let a second
backend be added without touching the domain.

## Why the boundaries sit where they do

**Wallpaper *selection* is in `ws-core`, wallpaper *reading* is in the backend.**
Which monitor wins, what to do about a slideshow, and whether a missing file is
worth retrying are product decisions, not Plasma facts. The backend reports what
each monitor shows; the core decides. That is why `select()` is a pure function
taking an `exists` predicate.

**Colour maths is a separate crate with no dependencies on the other crates.**
It takes bytes and returns colours. This is the part most likely to need tuning,
and being able to run `cargo test -p ws-palette` on its own, plus
`cargo run -p ws-palette --example inspect -- wallpaper.jpg`, made the tuning
practical.

**Window matching lives in `ws-core`, not in the generated JavaScript.** The
JavaScript is generated *from* the resolved rule set. The same `decide()` policy
is unit-tested in Rust, and the generated script mirrors it; a
simulated-compositor test checks that the script agrees with the policy on the
cases the specification names (virtual desktops, minimisation, activities,
fullscreen, VirtualMachine guests, games).

**Snapshots are data, not code.** A snapshot is a JSON document of captured
values plus a status for what was applied. Restore is a fold over that document
with an explicit conflict rule. Making it data meant the "don't clobber the
user's later change" behaviour could be tested with three temporary files.

## Palette pipeline

1. **Decode** with the `image` crate, natively, in process.
2. **Downscale** to a 128px thumbnail with an area filter. This averages, so the
   result is independent of the source resolution — a 8K wallpaper and a 400px
   one of the same picture produce the same palette.
3. **Convert to OKLab/OKLCH**, not HSL. HSL lightness is not perceptual:
   saturated blues collapse to black far faster than yellows at the same "L", and
   the prototype's surfaces were visibly muddy because of it.
4. **Weight** each sample by area, saturation and a lightness window, so a large
   flat grey border cannot outvote a small vivid subject, and near-white
   highlights do not dominate by area alone.
5. **Cluster hues** on the colour circle with merge-on-closest-pair, so a cluster
   straddling 0° stays whole and a 15° step cannot chain into a 40° one.
6. **Choose the accent** from the leading cluster, lifting its chroma to a usable
   UI level and normalising its lightness into a band that works on both
   variants.
7. **Build surfaces** as the accent's hue at very low chroma, spaced so each
   neighbouring pair clears a measured luminance step.
8. **Repair contrast.** Body text must reach 4.5:1 on every surface it sits on. If
   the accent cannot host readable text at any lightness, it is moved along the
   lightness axis keeping its hue exactly, and the text colour direction is
   chosen by measurement rather than by assumption.
9. **Report.** Every pairing is checked and returned, so the GUI and
   `wallpaper-style preview` show the same numbers the tests assert.

## The transparency effect

Wayland gives no way for one client to change another's opacity, and KWin does not
expose `wl_surface.set_opacity` to third parties. The only supported mechanism is
a script running inside the compositor, which is what the specification asks for
when it says "scoped KWin compositor effect".

The effect applies opacity with a persistent `set({duration: 1})` animation. The
prototype discovered the hard part: such an animation keeps KWin painting the
window. If it is left in place when the window moves to another virtual desktop or
is minimised, the window can be composited where it does not belong.

The generated effect therefore **cancels** the animation — rather than merely
ignoring the window — whenever `onCurrentDesktop`, `onCurrentActivity`,
`minimized`, `hiddenByShowDesktop` or `visible` stops holding, on every
per-window signal that can change them, and on every global desktop/activity
switch. It never calls a window-manipulating API; the simulation used in tests
simply does not provide them, so reaching for one would fail loudly.

`crates/ws-plasma/tests/kwin_effect_lifecycle.js` runs the *generated* script
against a simulation of the KWin scripting host and asserts 24 behaviours. This
is the closest thing to a compositor test that can run in CI.

## Recovery model

Before any apply, the application records the previous state of everything it is
about to touch:

* **File** → previous content (inline, ≤512 KiB) or "absent".
* **Config key** → previous value or "not present".
* **systemd unit / effect package** → previous content or "absent".

For each item it also records what it wrote. Restore then compares three states:

| current | action |
|---|---|
| equals what we wrote | restore the previous state |
| equals the previous state | nothing to do |
| anything else | **conflict** — leave it, report it |

Repeated applies never overwrite "the original" with "what we wrote last time";
`Ownership::record` keeps the first-seen original for each item.

## Library choices

Chosen after reading the current documentation for each, and pinned in
`Cargo.lock`.

| Crate | Why |
|---|---|
| `image` 0.25 | Pure-Rust decoding for JPEG/PNG/WebP/GIF/BMP/TIFF, EXIF orientation, area-filter downscaling. No ImageMagick at runtime, which the prototype required. |
| `serde` + `toml` + `serde_json` | Settings in TOML (hand-editable, as required), snapshots and the ownership manifest in JSON (diffable, machine-readable). |
| `clap` 4 (derive) | Subcommand CLI; shell completion via `clap_complete` later. |
| `thiserror` / `anyhow` | Library errors are typed; the CLI's errors carry context chains. |
| `eframe`/`egui` 0.36 | Native GPU-rendered GUI with a Wayland backend and no web engine. GTK4 cannot be used: `gtk4-rs` needs a newer `glib` than Arch ships alongside the other system libraries here, and an embedded web view would drag in a browser engine for a settings window. |
| `rfd` 0.17 (`xdg-portal`) | Native file chooser through the desktop portal, so it looks and behaves like the rest of Plasma and works under Wayland. |
| `sha2` | Content fingerprints to detect wallpaper changes and restore conflicts. |
| `nix` (signal) | SIGINT/SIGTERM handling for the foreground watcher without a signal crate. |
| `tempfile` | Atomic writes: write beside the target, `fsync`, rename. A crash mid-write cannot leave a half-written theme file. |

**Deliberately not used:**

* `image` features for AVIF/HEIC (rare as wallpapers, large dependency trees).
* Any palette crate: the OKLab implementation is ~150 lines, is covered by
  reference-value tests, and is the part of the product most likely to need
  tuning. A general-purpose library would hide the one thing that matters.
* `tokio`: the watcher is a sleep-and-work loop; an async runtime would add
  weight for no benefit.
* A tray library: `ksni` and `tray-icon` do not build from the vendored
  dependency set here (`tray-icon` needs `libxdo`), and the specification makes the
  tray optional. The setting exists in the schema and is honoured as "do not show"
  until a tray backend is added.

## Determinism and idempotence

* Palette extraction is deterministic: clusters sort with hue as a final
  tie-break, and rules serialise from a `BTreeMap`.
* Generated names include the palette, so Plasma and Kvantum reload instead of
  serving a cache, while repeated applies of the same palette produce identical
  bytes.
* `install` rewrites only files whose content differs and reports "unchanged"
  otherwise.
* The Kvantum config is regenerated with all of this application's mappings
  removed before the current one is added, so repeated applies converge instead of
  accumulating stale entries.

## Testing strategy

| Layer | How |
|---|---|
| Palette | Reference-value tests for OKLab, WCAG reference points for contrast, property-style round trips, real wallpapers via an example binary. |
| Policy | Table tests for presets and matching, including the cases that broke during development (multi-word classes, substring false positives, VirtualMachine guests). |
| Settings | Round trips, migration from unversioned files, import of the prototype's JSON, rejection of unknown keys and future schemas. |
| Snapshots | Three-state restore table, idempotence, corrupt and future-version rejection. |
| KWin effect | Node syntax check + 24 lifecycle assertions against a simulated compositor. |
| Apply pipeline | End-to-end against a temporary XDG tree: real files, real snapshot, real restore, plus a sandbox-escape check. |
| GUI | Headless `egui::Context::run_ui` renders every widget group, counts painted shapes, and verifies the preview's own text contrast. |

Nothing in the test suite writes outside a `tempfile::TempDir`, and the
production runner is behind a `Runner` trait so reads and writes are recorded
rather than executed.
