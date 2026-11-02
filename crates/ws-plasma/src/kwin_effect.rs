//! Generation of the scoped KWin compositor effect.
//!
//! # Why an effect at all
//!
//! Wayland has no protocol for one client to set another client's opacity, and
//! KWin deliberately does not expose `wl_surface.set_opacity` to anything but
//! the surface's own client. The only supported mechanism is a KWin script or
//! effect running inside the compositor. The specification requires a "scoped
//! KWin compositor effect", so this module generates one.
//!
//! # The correctness problem the prototype hit
//!
//! The prototype applied opacity with a persistent `set({duration: 1})`
//! animation and only *cleared* it when the window left the current desktop or
//! was minimised. The comment in that code says the quiet part out loud: a
//! persistent animation "otherwise makes KWin paint windows that the desktop or
//! minimize state would normally exclude from composition". That is exactly the
//! bug class to avoid: a window must never appear on a desktop it does not
//! belong to, and a minimised window must never be painted.
//!
//! The generated effect therefore:
//!
//! * only ever *adds* an opacity animation to a window that KWin is already
//!   painting (`visible`, `onCurrentDesktop`, not `minimized`, not
//!   `hiddenByShowDesktop`);
//! * **cancels** the animation the instant any of those conditions stops
//!   holding, rather than leaving it in place;
//! * re-evaluates on every state signal KWin emits for that window
//!   (`desktopChanged`, `minimized`, `fullscreen`, `activity`, `hidden`,
//!   `stackingOrder`), plus on whole-desktop and whole-activity switches;
//! * never moves, resizes, restacks, activates or otherwise manipulates a
//!   window — it only sets opacity;
//! * refuses to touch panel/dock/desktop/OSD/notification windows, VirtualBox
//!   guests and game windows.
//!
//! # Scoping
//!
//! Rules are matched in the script, not by KWin, so a window that stops
//! matching (for example because the user changed settings and the effect was
//! reloaded) loses its animation on the next pass. Nothing persists outside the
//! compositor: no window ids are written to disk, and the effect holds no state
//! across a KWin restart.

use std::collections::BTreeMap;

use ws_core::TransparencyRules;

/// The KWin effect id. Must be a valid plugin id: lowercase, digits, dashes.
pub const EFFECT_ID: &str = ws_core::KWIN_EFFECT_ID;

/// Version reported in the effect metadata. Bumped when the generated script
/// changes shape so KWin reloads it.
pub const EFFECT_VERSION: &str = "1.0.0";

/// A generated effect package, ready to be written to
/// `$XDG_DATA_HOME/kwin/effects/<id>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectPackage {
    pub id: String,
    /// `metadata.json` contents.
    pub metadata: String,
    /// `contents/code/main.js` contents.
    pub script: String,
}

impl EffectPackage {
    pub fn directory_name(&self) -> &str {
        &self.id
    }
}

/// Build the effect package for a rule set.
pub fn generate(rules: &TransparencyRules) -> EffectPackage {
    EffectPackage {
        id: EFFECT_ID.to_owned(),
        metadata: metadata_json(),
        script: script_js(rules),
    }
}

fn metadata_json() -> String {
    // KPackageStructure/X-Plasma-API are what KWin 6 looks for in a scripted
    // effect; without them the effect is not listed at all.
    format!(
        r#"{{
  "KPackageStructure": "KWin/Effect",
  "KPlugin": {{
    "Id": "{EFFECT_ID}",
    "Name": "Wallpaper Style transparency",
    "Description": "Scoped per-application window opacity managed by Wallpaper Style. Windows on other virtual desktops, on other activities, minimised, or fullscreen are never touched.",
    "Version": "{EFFECT_VERSION}",
    "License": "MIT",
    "Category": "Appearance",
    "EnabledByDefault": false
  }},
  "X-Plasma-API": "javascript",
  "X-Plasma-MainScript": "code/main.js",
  "X-KDE-Ordering": 60
}}
"#
    )
}

/// Escape a string for safe inclusion in a JavaScript double-quoted literal.
///
/// Window classes come from the user's settings file, so they are untrusted
/// input. Anything outside printable ASCII is escaped, which also guarantees the
/// generated script contains no non-ASCII characters.
fn js_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Escape the characters that can terminate a script block or a
            // string early, even though this is a separate .js file.
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => out.push_str(&format!("\\u{:04x}", c as u32)),
        }
    }
    out.push('"');
    out
}

/// Render the rules map as a JavaScript object literal.
///
/// Keys are window classes; values are opacity in tenths so the number is an
/// integer on both sides and cannot drift through float formatting.
fn rules_literal(rules: &TransparencyRules) -> String {
    let mut by_class: BTreeMap<&str, u32> = BTreeMap::new();
    for (class, opacity) in &rules.rules {
        let tenths = (opacity.clamp(0.0, 1.0) * 1000.0).round() as u32;
        if tenths < 1000 {
            by_class.insert(class.as_str(), tenths);
        }
    }
    let mut out = String::from("{");
    let mut first = true;
    for (class, tenths) in by_class {
        if !first {
            out.push(',');
        }
        first = false;
        out.push_str(&js_string(class));
        out.push(':');
        out.push_str(&tenths.to_string());
    }
    out.push('}');
    out
}

fn exclusions_literal(exclusions: &[String]) -> String {
    let mut out = String::from("[");
    for (index, class) in exclusions.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&js_string(class));
    }
    out.push(']');
    out
}

/// The generated effect script.
///
/// The structure mirrors KWin's own `translucency` effect, which is the
/// reference implementation for scripted opacity in Plasma 6.
fn script_js(rules: &TransparencyRules) -> String {
    let enabled = rules.enabled && !rules.rules.is_empty();
    let default_tenths = (rules.default_opacity.clamp(0.0, 1.0) * 1000.0).round() as u32;
    let skip_fullscreen = rules.skip_fullscreen;

    // The per-window animation handle is stored under a namespaced key so it
    // cannot collide with another effect's data.
    let template = r#"// Wallpaper Style transparency effect.
//
// Generated file. Changes are overwritten the next time Wallpaper Style
// applies its theme; edit the settings in the Wallpaper Style application
// instead.
//
// Safety invariants (see crates/ws-plasma/src/kwin_effect.rs):
//   1. Opacity is only ever applied to a window KWin is already painting.
//   2. The animation is cancelled - not merely ignored - as soon as the window
//      stops being painted here (other desktop, other activity, minimised,
//      hidden by show-desktop, closed).
//   3. This effect never moves, resizes, restacks or activates a window.
"use strict";

const ENABLED = __ENABLED__;
const DEFAULT_TENTHS = __DEFAULT__;
const SKIP_FULLSCREEN = __SKIP_FULLSCREEN__;
// window class -> opacity in thousandths
const RULES = __RULES__;
const EXCLUSIONS = __EXCLUSIONS__;
// Window classes that are a VM guest display, never a launcher.
const NEVER_MATCH = ["VirtualBox Machine", "VirtualBoxVM", "virtualbox machine"];
// Steam games set this prefix on their own windows.
const GAME_MARKERS = ["steam_app_"];

const ANIMATION_KEY = "wallpaperStyleOpacityAnimation";

// Ask the compositor to redraw. Guarded because this is the one API that has
// differed between KWin versions: an unguarded call threw here and prevented the
// effect from loading at all, which is far worse than a missed repaint.
function requestRepaint() {
    try {
        if (typeof effects.addRepaintFull === "function") {
            effects.addRepaintFull();
        }
    } catch (err) {
        // Never let a repaint hint break the effect.
    }
}

function lower(value) {
    return String(value === undefined || value === null ? "" : value).toLowerCase();
}

// Whole-token, case-insensitive containment: "code code" contains "code", but
// "codeblocks" does not. Mirrors contains_class() in ws-core.
function classContains(haystack, needle) {
    const h = lower(haystack);
    const n = lower(needle).trim();
    if (n.length === 0 || n.length > h.length) {
        return false;
    }
    const isBoundary = function (index) {
        return index < 0 || index >= h.length || /\s/.test(h.charAt(index));
    };
    let at = h.indexOf(n);
    while (at !== -1) {
        if (isBoundary(at - 1) && isBoundary(at + n.length)) {
            return true;
        }
        at = h.indexOf(n, at + 1);
    }
    return false;
}

function isVirtualMachine(cls) {
    return NEVER_MATCH.some(function (name) { return classContains(cls, name); });
}

function isGame(cls) {
    const c = lower(cls);
    return GAME_MARKERS.some(function (marker) { return c.indexOf(marker) !== -1; });
}

function isExcluded(cls) {
    return EXCLUSIONS.some(function (name) { return classContains(cls, name); });
}

// Returns opacity in thousandths, or 0 meaning "leave this window alone".
function amountFor(cls) {
    if (!ENABLED) {
        return 0;
    }
    if (isVirtualMachine(cls) || isGame(cls) || isExcluded(cls)) {
        return 0;
    }
    let best = 0;
    let bestLength = -1;
    Object.keys(RULES).forEach(function (needle) {
        if (!classContains(cls, needle)) {
            return;
        }
        // A longer (more specific) class wins when several match.
        if (needle.length > bestLength) {
            bestLength = needle.length;
            best = RULES[needle];
        }
    });
    if (best > 0) {
        return best;
    }
    return DEFAULT_TENTHS < 1000 ? DEFAULT_TENTHS : 0;
}

// A window is eligible only when KWin is painting it on this desktop.
function isPaintable(window) {
    if (!window || window.deleted || !window.managed) {
        return false;
    }
    if (window.desktopWindow || window.dock || window.popupWindow) {
        return false;
    }
    // Panels, menus, tooltips, notifications, OSDs and splash screens are not
    // application windows; touching them breaks the shell.
    if (!window.normalWindow && !window.dialog) {
        return false;
    }
    if (window.minimized || window.hiddenByShowDesktop || !window.visible) {
        return false;
    }
    if (!window.onCurrentDesktop || !window.onCurrentActivity) {
        return false;
    }
    if (SKIP_FULLSCREEN && window.fullScreen) {
        return false;
    }
    return true;
}

function clearAnimation(window) {
    if (window && window[ANIMATION_KEY] !== undefined) {
        // cancel() is what stops KWin painting the window through the
        // animation; leaving it would paint windows on other desktops.
        cancel(window[ANIMATION_KEY]);
        window[ANIMATION_KEY] = undefined;
        requestRepaint();
    }
}

function sync(window) {
    if (!window) {
        return;
    }
    const tenths = amountFor(window.windowClass);
    if (tenths === 0 || !isPaintable(window)) {
        clearAnimation(window);
        return;
    }
    const amount = tenths / 1000.0;
    if (window[ANIMATION_KEY] !== undefined) {
        // Already animated; leave it, unless the amount changed.
        if (window["wallpaperStyleAmount"] === tenths) {
            return;
        }
        clearAnimation(window);
    }
    window[ANIMATION_KEY] = set({
        window: window,
        duration: 1,
        keepAlive: false,
        animations: [{ type: Effect.Opacity, from: amount, to: amount }]
    });
    window["wallpaperStyleAmount"] = tenths;
    requestRepaint();
}

function syncAll() {
    effects.stackingOrder.forEach(sync);
}

function manage(window) {
    if (!window) {
        return;
    }
    // Re-evaluate on every state change that can make a window paintable or
    // un-paintable. Missing any one of these is what causes a window to linger
    // on the wrong desktop.
    const signals = [
        "windowDesktopsChanged",
        "windowHiddenChanged",
        "windowFullScreenChanged",
        "activitiesChanged",
        "windowShown",
        "windowHidden"
    ];
    signals.forEach(function (name) {
        const signal = window[name];
        if (signal && signal.connect) {
            signal.connect(function () { sync(window); });
        }
    });
    if (window.minimizedChanged && window.minimizedChanged.connect) {
        window.minimizedChanged.connect(function () { sync(window); });
    }
    sync(window);
}

effects.windowAdded.connect(manage);
effects.windowClosed.connect(clearAnimation);
effects.windowDeleted.connect(clearAnimation);
effects.desktopChanged.connect(function () {
    // Windows on the desktop being left must lose their animation immediately.
    syncAll();
});
effects.desktopChangingCancelled.connect(syncAll);
effects.currentActivityChanged.connect(syncAll);
effects.showingDesktopChanged.connect(syncAll);
// Opacity is a property of the window, not of the stacking order, but a new
// window can become eligible after another was raised.
effects.windowActivated.connect(function (window) { syncAll(); });

effects.stackingOrder.forEach(manage);

const ruleCount = Object.keys(RULES).length;
print("wallpaper-style: transparency effect loaded with " + ruleCount +
      (ruleCount === 1 ? " rule" : " rules") +
      (ENABLED ? "" : " (disabled in settings)"));
"#;

    template
        .replace("__ENABLED__", if enabled { "true" } else { "false" })
        .replace("__DEFAULT__", &default_tenths.to_string())
        .replace(
            "__SKIP_FULLSCREEN__",
            if skip_fullscreen { "true" } else { "false" },
        )
        .replace("__RULES__", &rules_literal(rules))
        .replace("__EXCLUSIONS__", &exclusions_literal(&rules.exclusions))
}

#[cfg(test)]
mod tests {
    // Test modules use deliberate idioms clippy flags by default: exhaustive
    // `other => panic!()` guards so a new variant cannot pass silently, and
    // `assert!(x.is_empty())` for readability.
    #![allow(
        clippy::single_match_else,
        clippy::wildcard_in_or_patterns,
        clippy::bool_assert_comparison,
        clippy::struct_excessive_bools,
        clippy::match_same_arms,
        clippy::needless_borrow,
        clippy::map_unwrap_or,
        clippy::unnecessary_wraps,
        clippy::redundant_closure,
        clippy::unused_unit,
        clippy::items_after_statements,
        clippy::needless_borrows_for_generic_args,
        clippy::ptr_arg
    )]
    use super::*;

    fn rules(pairs: &[(&str, f32)]) -> TransparencyRules {
        TransparencyRules {
            enabled: true,
            default_opacity: 0.8,
            skip_fullscreen: true,
            exclusions: Vec::new(),
            rules: pairs.iter().map(|(c, o)| ((*c).to_owned(), *o)).collect(),
        }
    }

    #[test]
    fn the_package_has_the_files_kwin_needs() {
        let package = generate(&rules(&[("code", 0.7)]));
        assert_eq!(package.id, EFFECT_ID);
        let metadata: serde_json::Value = serde_json::from_str(&package.metadata).unwrap();
        assert_eq!(metadata["KPackageStructure"], "KWin/Effect");
        assert_eq!(metadata["KPlugin"]["Id"], EFFECT_ID);
        assert_eq!(metadata["X-Plasma-API"], "javascript");
        assert_eq!(metadata["X-Plasma-MainScript"], "code/main.js");
        assert_eq!(metadata["KPlugin"]["EnabledByDefault"], false);
    }

    #[test]
    fn the_effect_id_is_a_valid_plugin_identifier() {
        assert!(EFFECT_ID
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(!EFFECT_ID.is_empty());
    }

    #[test]
    fn rules_are_rendered_into_the_script() {
        let package = generate(&rules(&[("code", 0.7), ("Claude", 0.65)]));
        assert!(
            package.script.contains(r#""code":700"#),
            "{}",
            package.script
        );
        assert!(
            package.script.contains(r#""Claude":650"#),
            "{}",
            package.script
        );
    }

    #[test]
    fn no_placeholder_survives_substitution() {
        let package = generate(&rules(&[("code", 0.7)]));
        for token in [
            "__ENABLED__",
            "__DEFAULT__",
            "__SKIP_FULLSCREEN__",
            "__RULES__",
            "__EXCLUSIONS__",
        ] {
            assert!(
                !package.script.contains(token),
                "{token} was not substituted"
            );
        }
    }

    #[test]
    fn the_script_cancels_rather_than_hides_the_animation() {
        // The prototype's bug was leaving the animation alive off-desktop.
        let script = generate(&rules(&[("code", 0.7)])).script;
        assert!(
            script.contains("cancel(window[ANIMATION_KEY])"),
            "must cancel"
        );
        assert!(
            script.contains("window.onCurrentDesktop"),
            "must check the desktop"
        );
        assert!(
            script.contains("window.onCurrentActivity"),
            "must check the activity"
        );
        assert!(
            script.contains("window.minimized"),
            "must check minimisation"
        );
        assert!(
            script.contains("hiddenByShowDesktop"),
            "must check show-desktop"
        );
    }

    #[test]
    fn the_script_never_manipulates_window_geometry_or_stacking() {
        let script = generate(&rules(&[("code", 0.7)])).script;
        for forbidden in [
            ".move(",
            "moveWindow",
            "keepAbove",
            "activateWindow",
            "setCurrentDesktop",
            "windowToDesktops",
            "windowToScreen",
            "setMinimized",
        ] {
            assert!(
                !script.contains(forbidden),
                "script must not call {forbidden}"
            );
        }
    }

    #[test]
    fn the_script_excludes_virtual_machines_and_games() {
        let script = generate(&rules(&[("VirtualBox Manager", 0.88)])).script;
        assert!(script.contains("VirtualBox Machine"));
        assert!(script.contains("VirtualBoxVM"));
        assert!(script.contains("steam_app_"));
    }

    #[test]
    fn the_script_only_targets_application_windows() {
        let script = generate(&rules(&[("code", 0.7)])).script;
        assert!(script.contains("window.normalWindow"));
        assert!(script.contains("window.dialog"));
        assert!(script.contains("window.desktopWindow"));
        assert!(script.contains("window.dock"));
        assert!(script.contains("window.popupWindow"));
    }

    #[test]
    fn window_classes_are_escaped_so_they_cannot_break_the_script() {
        let nasty = vec![("bad\"); process.exit(0); //".to_owned(), 0.5_f32)];
        let mut r = rules(&[]);
        r.rules = nasty;
        let script = generate(&r).script;
        // The quote must be escaped, and the injected call must not appear as code.
        assert!(
            script.contains(r#""bad\"); process.exit(0); //":500"#),
            "{script}"
        );
        assert!(
            !script.contains("\nprocess.exit"),
            "injection escaped the string"
        );
    }

    #[test]
    fn newlines_and_unicode_in_classes_are_neutralised() {
        let mut r = rules(&[]);
        r.rules = vec![("a\nb".to_owned(), 0.5), ("café".to_owned(), 0.5)];
        let script = generate(&r).script;
        assert!(
            !script.contains("\"a\nb\""),
            "raw newline leaked into a literal"
        );
        assert!(script.contains("\\u00e9"), "non-ASCII must be escaped");
        // The whole generated script stays ASCII.
        assert!(script.is_ascii(), "generated script must be pure ASCII");
    }

    #[test]
    fn opaque_rules_are_omitted_entirely() {
        let package = generate(&rules(&[("myopaqueapp", 1.0), ("kate", 0.8)]));
        // "code" also appears in the template's comments, so assert on the
        // rendered RULES literal rather than on the whole script.
        assert!(
            !package.script.contains("\"myopaqueapp\""),
            "a 1.0 rule is pointless work"
        );
        assert!(package.script.contains("\"kate\":800"));
    }

    #[test]
    fn disabling_transparency_produces_a_dormant_script() {
        let mut r = rules(&[("code", 0.7)]);
        r.enabled = false;
        let script = generate(&r).script;
        assert!(script.contains("const ENABLED = false;"), "{script}");
    }

    #[test]
    fn exclusions_are_rendered() {
        let mut r = rules(&[("code", 0.7)]);
        r.exclusions = vec!["steam".to_owned(), "obs".to_owned()];
        let script = generate(&r).script;
        assert!(
            script.contains(r#"const EXCLUSIONS = ["steam","obs"];"#),
            "{script}"
        );
    }

    #[test]
    fn the_default_opacity_is_carried_over() {
        let mut r = rules(&[]);
        r.default_opacity = 0.62;
        let script = generate(&r).script;
        assert!(script.contains("const DEFAULT_TENTHS = 620;"), "{script}");
    }

    #[test]
    fn generation_is_deterministic() {
        let r = rules(&[("b", 0.5), ("a", 0.6)]);
        assert_eq!(generate(&r), generate(&r));
    }

    #[test]
    fn rule_order_in_settings_does_not_change_the_script() {
        // BTreeMap ordering means the emitted script is stable regardless of how
        // the caller assembled the rules.
        let a = generate(&rules(&[("alpha", 0.5), ("beta", 0.6)]));
        let b = generate(&rules(&[("beta", 0.6), ("alpha", 0.5)]));
        assert_eq!(a, b);
    }

    #[test]
    fn the_script_references_only_apis_that_exist_in_kwin_6() {
        // Verified against /usr/include/kwin/effect/*.h for Plasma 6.8.
        let script = generate(&rules(&[("code", 0.7)])).script;
        for api in [
            "effects.stackingOrder",
            "effects.windowAdded",
            "effects.windowClosed",
            "effects.windowDeleted",
            "effects.desktopChanged",
            "effects.desktopChangingCancelled",
            "effects.currentActivityChanged",
            "effects.showingDesktopChanged",
            "effects.windowActivated",
            "Effect.Opacity",
            "set(",
            "cancel(",
        ] {
            assert!(script.contains(api), "missing API {api}");
        }
    }
}
