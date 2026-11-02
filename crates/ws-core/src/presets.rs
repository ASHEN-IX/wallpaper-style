//! Built-in application presets and window-class matching.
//!
//! A "preset" is a named group of window classes plus a default opacity. The
//! effect itself only ever sees a flat map of `window class → opacity`, so all
//! the interesting policy lives here where it can be unit-tested without a
//! compositor.
//!
//! Matching rules, learned from the prototype's failures:
//!
//! * Window classes are matched **case-insensitively** by whole token. The
//!   prototype used a word-boundary regex which was correct, but a plain
//!   substring check would make `code` match `codeblocks`.
//! * A class may carry a reversed-DNS domain (`com.anthropic.Claude`), a plain
//!   name (`steam`), or several space-separated values (KWin concatenates
//!   `resourceClass` and `resourceName`). All are compared separately.
//! * **VirtualBox guest windows are never matched.** A guest runs arbitrary
//!   software inside a VM window; making it translucent hides the guest's own
//!   UI and was the reason the prototype needed a special case.
//! * **Game windows are excluded from launcher rules.** Steam's overlay and
//!   Heroic both spawn games whose class is unrelated to the launcher, but the
//!   leftover `steamwebhelper` process can match. Games are skipped entirely.

use serde::{Deserialize, Serialize};

/// Window classes the prototype applied its generic browser opacity to.
///
/// Kept as a list rather than a regex so the migration can be a plain loop and
/// so the UI can show exactly which applications a rule covers.
pub const BROWSER_WINDOW_CLASSES: &[&str] = &["brave-browser", "brave", "firedragon", "FireDragon"];

/// Window classes the prototype applied its launcher opacity to.
pub const LAUNCHER_WINDOW_CLASSES: &[&str] =
    &["VirtualBox Manager", "steam", "Steam", "heroic", "Heroic"];

/// Window classes that must never receive opacity, whatever else matches.
///
/// `VirtualBox Machine` and `VirtualBoxVM` are the guest display windows; the
/// guest's own windows live inside them and must keep their own rendering.
pub const NEVER_MATCH: &[&str] = &["VirtualBox Machine", "VirtualBoxVM", "VirtualBox machine"];

/// Something that is a game rather than a launcher UI.
///
/// Steam sets `steam_app_<id>` on the game process and uses `steamwebhelper`
/// for its own UI, so the two can be told apart by class alone.
const GAME_MARKERS: &[&str] = &["steam_app_"];

/// How to treat a window whose class matches a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransparencyKind {
    /// The compositor lowers the opacity of the whole window, decoration
    /// included. Works for every application, but text also becomes translucent.
    WholeWindow,
    /// The theme paints translucent surfaces while text stays opaque. Requires
    /// a Kvantum theme and an application that honours it.
    BackgroundOnly,
}

/// A built-in group of applications sharing an opacity and a treatment.
///
/// Not serialised: the catalogue is compile-time data and is referenced from
/// settings by [`Preset::id`] only, so renaming a field cannot break an old
/// configuration file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Preset {
    /// Stable identifier stored in the settings file; never renamed.
    pub id: &'static str,
    /// Human-readable name shown in the UI.
    pub name: &'static str,
    /// Window classes covered by this preset.
    pub window_classes: &'static [&'static str],
    /// Default opacity when the preset is enabled.
    pub default_opacity: f32,
    /// Which transparency technique the product can offer for these apps.
    pub kind: TransparencyKind,
    /// One line explaining a limitation the user would otherwise discover by
    /// accident.
    pub note: &'static str,
}

/// The preset catalogue promised by the specification.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "codex",
        name: "Codex",
        window_classes: &["codex", "chatgpt", "com.openai.chatgpt", "com.openai.codex"],
        default_opacity: 0.80,
        kind: TransparencyKind::WholeWindow,
        note: "Electron application: it draws its own surfaces, so only whole-window \
               transparency applies and text dims with the window.",
    },
    Preset {
        id: "claude",
        name: "Claude",
        window_classes: &["Claude", "claude-desktop", "com.anthropic.Claude"],
        default_opacity: 0.80,
        kind: TransparencyKind::WholeWindow,
        note: "Electron application: uses whole-window transparency; a restart is needed \
               after changing its opacity.",
    },
    Preset {
        id: "vscode",
        name: "Visual Studio Code",
        window_classes: &[
            "code",
            "code-oss",
            "com.microsoft.VSCode",
            "com.visualstudio.code",
        ],
        default_opacity: 0.82,
        kind: TransparencyKind::WholeWindow,
        note: "Electron application: uses whole-window transparency. VS Code also has its \
               own built-in background transparency setting.",
    },
    Preset {
        id: "cursor",
        name: "Cursor",
        window_classes: &["Cursor", "co.anysphere.cursor"],
        default_opacity: 0.82,
        kind: TransparencyKind::WholeWindow,
        note: "Electron application: uses whole-window transparency.",
    },
    Preset {
        id: "browsers",
        name: "Web browsers",
        window_classes: BROWSER_WINDOW_CLASSES,
        default_opacity: 0.82,
        kind: TransparencyKind::WholeWindow,
        note: "Browsers do not follow the Qt theme, so per-site colours are unaffected; \
               only the window frame and page backdrop become translucent.",
    },
    Preset {
        id: "garuda-toolbox",
        name: "Garuda Toolbox",
        window_classes: &["garuda-toolbox", "garuda-system-maintenance"],
        default_opacity: 0.85,
        kind: TransparencyKind::WholeWindow,
        note: "A Qt application: it supports background-only transparency from the Kvantum \
               theme when the glass variant is enabled.",
    },
    Preset {
        id: "steam",
        name: "Steam",
        window_classes: &["steam", "Steam", "steamwebhelper"],
        default_opacity: 0.85,
        kind: TransparencyKind::WholeWindow,
        note: "Only the Steam client window is affected; game windows are never matched.",
    },
    Preset {
        id: "heroic",
        name: "Heroic Games Launcher",
        window_classes: &["heroic", "Heroic", "com.heroicgameslauncher.hgl"],
        default_opacity: 0.85,
        kind: TransparencyKind::WholeWindow,
        note: "Only the launcher window is affected. Heroic is Electron-based; a restart is \
               needed after changing its opacity.",
    },
    Preset {
        id: "virtualbox-manager",
        name: "VirtualBox Manager",
        window_classes: &["VirtualBox Manager", "virtualbox manager"],
        default_opacity: 0.88,
        kind: TransparencyKind::WholeWindow,
        note: "Only the Manager window. Running guest machines are never made translucent.",
    },
];

/// Look a preset up by its stable id.
pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The nearest-usable preset for a window class, for the "identify an
/// application" button in the GUI.
pub fn preset_for_class(window_class: &str) -> Option<&'static Preset> {
    PRESETS
        .iter()
        .find(|p| class_matches(window_class, p.window_classes))
}

/// Split a KWin `windowClass` into its space-delimited words.
///
/// KWin's scripted `windowClass` property concatenates `res_name` and
/// `res_class` with a space, so a window reports `"code code"` or
/// `"Claude com.anthropic.Claude"`. Some class names additionally contain
/// spaces themselves (`"VirtualBox Manager"`), which is why matching is done
/// against the whole string rather than against individual words. This helper
/// is for display and for suggesting a rule, not for matching.
pub fn class_tokens(window_class: &str) -> Vec<String> {
    window_class.split_whitespace().map(str::to_owned).collect()
}

/// True when `candidate` appears in `window_class` delimited by whitespace (or
/// the string ends), compared case-insensitively.
///
/// This is the rule the prototype's regex used and it is the one that gets all
/// the cases right:
///
/// * `"code code"` matches `code`;
/// * `"Claude com.anthropic.Claude"` matches both `Claude` and the reversed
///   domain;
/// * `"VirtualBox Manager"` matches the whole multi-word name;
/// * `"codeblocks"` does **not** match `code`, because the match is followed by
///   more word characters rather than a boundary;
/// * `"steam_app_440"` does not match `steam`, so games are not caught by a
///   launcher rule even before the explicit game check runs.
fn contains_class(window_class: &str, candidate: &str) -> bool {
    let needle = candidate.trim();
    if needle.is_empty() {
        return false;
    }
    let haystack = window_class.as_bytes();
    let needle_lower = needle.to_ascii_lowercase();
    let needle_bytes = needle_lower.as_bytes();

    if needle_bytes.len() > haystack.len() {
        return false;
    }
    let is_space = |b: u8| b.is_ascii_whitespace();
    let mut start = 0;
    while start + needle_bytes.len() <= haystack.len() {
        if haystack[start..start + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes)
            && (start == 0 || is_space(haystack[start - 1]))
            && (start + needle_bytes.len() == haystack.len()
                || is_space(haystack[start + needle_bytes.len()]))
        {
            return true;
        }
        start += 1;
    }
    false
}

/// True when any of `candidates` matches the window class.
fn any_matches(window_class: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| contains_class(window_class, candidate))
}

/// True when any of `candidates` matches the window class.
pub fn class_matches(window_class: &str, candidates: &[&str]) -> bool {
    any_matches(window_class, candidates)
}

/// Whether this window is a VirtualBox guest display, which is never touched.
pub fn is_virtual_machine_window(window_class: &str) -> bool {
    class_matches(window_class, NEVER_MATCH)
}

/// Whether this window looks like a game rather than a launcher UI.
pub fn is_game_window(window_class: &str) -> bool {
    let tokens = class_tokens(window_class);
    if tokens.is_empty() {
        return false;
    }
    tokens
        .iter()
        .any(|token| GAME_MARKERS.iter().any(|marker| token.contains(marker)))
}

/// Outcome of deciding what opacity, if any, applies to a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Decision {
    /// Leave the window exactly as it is.
    Skip { reason: SkipReason },
    /// Apply this opacity to the whole window.
    Opacity {
        opacity_tenths: u16,
        matched: String,
    },
}

/// Why a window was left alone. Surfaced in diagnostics so a user can
/// understand why an application is not becoming translucent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The master switch is off.
    Disabled,
    /// Not a normal window or dialog (panels, menus, tooltips, OSDs).
    WrongWindowType,
    /// A VirtualBox guest display.
    VirtualMachine,
    /// A game window.
    Game,
    /// Matches a user exclusion.
    Excluded,
    /// Fullscreen and the user asked to leave fullscreen alone.
    Fullscreen,
    /// No rule matched.
    NoMatch,
    /// The rule exists but its opacity is 1.0, so there is nothing to do.
    FullyOpaque,
}

impl SkipReason {
    pub fn describe(self) -> &'static str {
        match self {
            SkipReason::Disabled => "transparency is disabled",
            SkipReason::WrongWindowType => "not an application window",
            SkipReason::VirtualMachine => "VirtualBox guest windows are never modified",
            SkipReason::Game => "game windows are never modified",
            SkipReason::Excluded => "matched an exclusion rule",
            SkipReason::Fullscreen => "fullscreen windows are left alone",
            SkipReason::NoMatch => "no rule matched this application",
            SkipReason::FullyOpaque => "the matching rule is fully opaque",
        }
    }
}

/// Everything needed to decide a window without touching the compositor.
///
/// This mirrors exactly what the generated KWin effect evaluates, and the unit
/// tests below assert that the Rust policy and the shipped JavaScript agree.
#[derive(Debug, Clone)]
pub struct WindowFacts<'a> {
    pub window_class: &'a str,
    pub is_normal_or_dialog: bool,
    pub fullscreen: bool,
    /// Rules keyed by window class, already merged from presets and custom
    /// entries.
    pub rules: &'a [(String, f32)],
    pub exclusions: &'a [String],
    pub default_opacity: f32,
    pub enabled: bool,
    pub skip_fullscreen: bool,
}

/// Resolve a window against the rules.
pub fn decide(facts: &WindowFacts<'_>) -> Decision {
    if !facts.enabled {
        return Decision::Skip {
            reason: SkipReason::Disabled,
        };
    }
    if !facts.is_normal_or_dialog {
        return Decision::Skip {
            reason: SkipReason::WrongWindowType,
        };
    }
    if is_virtual_machine_window(facts.window_class) {
        return Decision::Skip {
            reason: SkipReason::VirtualMachine,
        };
    }
    if is_game_window(facts.window_class) {
        return Decision::Skip {
            reason: SkipReason::Game,
        };
    }
    if facts.fullscreen && facts.skip_fullscreen {
        return Decision::Skip {
            reason: SkipReason::Fullscreen,
        };
    }
    if matches_exclusion(facts.window_class, facts.exclusions) {
        return Decision::Skip {
            reason: SkipReason::Excluded,
        };
    }

    // Most specific rule wins: a rule whose class matches exactly is preferred
    // over a rule that only matched because the class contains several tokens.
    let mut best: Option<(&str, f32, usize)> = None;
    for (class, opacity) in facts.rules {
        if !contains_class(facts.window_class, class) {
            continue;
        }
        // Prefer the rule naming the longest class; that is the more specific
        // one when several apply.
        let specificity = class.len();
        if best.is_none_or(|(_, _, s)| specificity > s) {
            best = Some((class, *opacity, specificity));
        }
    }

    match best {
        Some((class, opacity, _)) => {
            let clamped = opacity.clamp(crate::settings::MIN_OPACITY, 1.0);
            if clamped >= 1.0 {
                Decision::Skip {
                    reason: SkipReason::FullyOpaque,
                }
            } else {
                Decision::Opacity {
                    opacity_tenths: (clamped * 1000.0).round() as u16,
                    matched: class.to_owned(),
                }
            }
        }
        None => {
            let clamped = facts
                .default_opacity
                .clamp(crate::settings::MIN_OPACITY, 1.0);
            if clamped >= 1.0 {
                Decision::Skip {
                    reason: SkipReason::NoMatch,
                }
            } else {
                Decision::Opacity {
                    opacity_tenths: (clamped * 1000.0).round() as u16,
                    matched: "<default>".to_owned(),
                }
            }
        }
    }
}

/// Exclusions match as whole tokens, like rules, so excluding `steam` does not
/// also exclude an unrelated `steamthing`.
pub fn matches_exclusion(window_class: &str, exclusions: &[String]) -> bool {
    exclusions
        .iter()
        .any(|excluded| contains_class(window_class, excluded))
}

/// Take a raw list of window classes from the compositor, filtered to those that
/// could plausibly be applications. Used by the "identify an application" tool.
pub fn identify(window_class: &str) -> Identification {
    let tokens = class_tokens(window_class);
    Identification {
        window_class: window_class.to_owned(),
        tokens,
        excluded_reason: if is_virtual_machine_window(window_class) {
            Some(SkipReason::VirtualMachine)
        } else if is_game_window(window_class) {
            Some(SkipReason::Game)
        } else {
            None
        },
        preset: preset_for_class(window_class).map(|p| p.id),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identification {
    pub window_class: String,
    pub tokens: Vec<String>,
    pub excluded_reason: Option<SkipReason>,
    pub preset: Option<&'static str>,
}

impl Identification {
    /// A class a user could reasonably type into their own rules.
    pub fn suggested_rule_class(&self) -> Option<&str> {
        // Prefer a reversed-DNS token, which is the most stable identifier.
        self.tokens
            .iter()
            .find(|token| token.contains('.') && !token.starts_with("com.google.chrome.app"))
            .map(String::as_str)
            .or_else(|| self.tokens.first().map(String::as_str))
    }
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

    fn facts<'a>(
        window_class: &'a str,
        rules: &'a [(String, f32)],
        exclusions: &'a [String],
    ) -> WindowFacts<'a> {
        WindowFacts {
            window_class,
            is_normal_or_dialog: true,
            fullscreen: false,
            rules,
            exclusions,
            default_opacity: 0.8,
            enabled: true,
            skip_fullscreen: true,
        }
    }

    fn rules(pairs: &[(&str, f32)]) -> Vec<(String, f32)> {
        pairs.iter().map(|(c, o)| ((*c).to_owned(), *o)).collect()
    }

    #[test]
    fn every_documented_preset_exists() {
        for id in [
            "codex",
            "claude",
            "vscode",
            "cursor",
            "browsers",
            "garuda-toolbox",
            "steam",
            "heroic",
            "virtualbox-manager",
        ] {
            assert!(preset(id).is_some(), "missing preset {id}");
        }
        assert_eq!(PRESETS.len(), 9);
    }

    #[test]
    fn preset_ids_are_unique_and_opacities_are_valid() {
        let mut ids: Vec<_> = PRESETS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "duplicate preset id");
        for p in PRESETS {
            assert!(
                (crate::settings::MIN_OPACITY..1.0).contains(&p.default_opacity),
                "{} has invalid opacity {}",
                p.id,
                p.default_opacity
            );
            assert!(!p.window_classes.is_empty(), "{} has no classes", p.id);
            assert!(!p.note.is_empty(), "{} has no note", p.id);
        }
    }

    #[test]
    fn window_class_matching_is_case_insensitive_and_segment_aware() {
        assert!(class_matches("Claude", &["claude"]));
        assert!(class_matches("claude-desktop", &["claude-desktop"]));
        assert!(class_matches(
            "com.anthropic.Claude",
            &["com.anthropic.Claude"]
        ));
        // KWin joins res_name and res_class with a space.
        assert!(class_matches("code code", &["code"]));
        assert!(class_matches("Claude com.anthropic.Claude", &["Claude"]));
        assert!(class_matches("Claude com.anthropic.Claude", &["claude"]));
        // Substrings must not match: this was a real prototype hazard.
        assert!(!class_matches("codeblocks", &["code"]));
        assert!(!class_matches("steamthing", &["steam"]));
    }

    #[test]
    fn multi_word_class_names_are_matchable() {
        // "VirtualBox Manager" contains a space; splitting the haystack on
        // whitespace silently made this preset dead. Matching is done against
        // the whole string with whitespace boundaries instead.
        for class in ["VirtualBox Manager", "virtualbox manager"] {
            assert!(class_matches(class, &["VirtualBox Manager"]), "{class}");
        }
    }

    #[test]
    fn boundary_matching_distinguishes_joined_names_from_single_words() {
        // KWin joins res_name and res_class with a space.
        assert!(class_matches(
            "Claude com.anthropic.Claude",
            &["com.anthropic.Claude"]
        ));
        assert!(class_matches("code code", &["code"]));
        // A shorter candidate inside a longer *segment* must not match.
        assert!(!class_matches("steam_app_440", &["steam"]));
        assert!(!class_matches("org.kde.plasma.folder", &["plasma"]));
    }

    #[test]
    fn virtualbox_guest_windows_are_never_matched() {
        for class in ["VirtualBox Machine", "VirtualBoxVM", "virtualbox machine"] {
            assert!(
                is_virtual_machine_window(class),
                "{class} should be excluded"
            );
            let r = rules(&[("VirtualBox Manager", 0.85), ("virtualbox", 0.5)]);
            let decision = decide(&facts(class, &r, &[]));
            assert_eq!(
                decision,
                Decision::Skip {
                    reason: SkipReason::VirtualMachine
                },
                "{class} must be skipped even when a rule matches"
            );
        }
    }

    #[test]
    fn the_virtualbox_manager_window_is_still_transparent() {
        let r = rules(&[("VirtualBox Manager", 0.88)]);
        match decide(&facts("VirtualBox Manager", &r, &[])) {
            Decision::Opacity { opacity_tenths, .. } => assert_eq!(opacity_tenths, 880),
            other => panic!("expected opacity, got {other:?}"),
        }
    }

    #[test]
    fn game_windows_are_never_matched() {
        let r = rules(&[("steam", 0.85)]);
        let decision = decide(&facts("steam_app_12345", &r, &[]));
        assert_eq!(
            decision,
            Decision::Skip {
                reason: SkipReason::Game
            }
        );
    }

    #[test]
    fn steam_launcher_itself_is_transparent() {
        let r = rules(&[("steam", 0.85), ("steamwebhelper", 0.85)]);
        for class in ["steam", "Steam", "steamwebhelper"] {
            match decide(&facts(class, &r, &[])) {
                Decision::Opacity { opacity_tenths, .. } => assert_eq!(opacity_tenths, 850),
                other => panic!("{class}: expected opacity, got {other:?}"),
            }
        }
    }

    #[test]
    fn non_application_windows_are_skipped() {
        let r = rules(&[("plasma-desktop", 0.5)]);
        let mut f = facts("plasma-desktop", &r, &[]);
        f.is_normal_or_dialog = false;
        assert_eq!(
            decide(&f),
            Decision::Skip {
                reason: SkipReason::WrongWindowType
            }
        );
    }

    #[test]
    fn fullscreen_is_skipped_only_when_requested() {
        let r = rules(&[("mpv", 0.6)]);
        let mut f = facts("mpv", &r, &[]);
        f.fullscreen = true;
        assert_eq!(
            decide(&f),
            Decision::Skip {
                reason: SkipReason::Fullscreen
            }
        );
        f.skip_fullscreen = false;
        assert!(matches!(decide(&f), Decision::Opacity { .. }));
    }

    #[test]
    fn exclusions_win_over_rules() {
        let r = rules(&[("code", 0.7)]);
        let excluded = vec!["code".to_owned()];
        assert_eq!(
            decide(&facts("code code", &r, &excluded)),
            Decision::Skip {
                reason: SkipReason::Excluded
            }
        );
    }

    #[test]
    fn exclusions_do_not_over_match_substrings() {
        let r = rules(&[("codeblocks", 0.7)]);
        let excluded = vec!["code".to_owned()];
        assert!(matches!(
            decide(&facts("codeblocks", &r, &excluded)),
            Decision::Opacity { .. }
        ));
    }

    #[test]
    fn unlisted_applications_fall_back_to_the_default_opacity() {
        let r = rules(&[]);
        match decide(&facts("unknown-app", &r, &[])) {
            Decision::Opacity {
                opacity_tenths,
                matched,
            } => {
                assert_eq!(opacity_tenths, 800);
                assert_eq!(matched, "<default>");
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn a_fully_opaque_default_means_no_work() {
        let r = rules(&[]);
        let mut f = facts("unknown-app", &r, &[]);
        f.default_opacity = 1.0;
        assert_eq!(
            decide(&f),
            Decision::Skip {
                reason: SkipReason::NoMatch
            }
        );
    }

    #[test]
    fn the_most_specific_rule_wins() {
        let r = rules(&[("code", 0.9), ("com.microsoft.vscode", 0.6)]);
        // The window carries both tokens; the longer class must win.
        match decide(&facts("code com.microsoft.VSCode", &r, &[])) {
            Decision::Opacity {
                opacity_tenths,
                matched,
            } => {
                assert_eq!(matched, "com.microsoft.vscode");
                assert_eq!(opacity_tenths, 600);
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn disabled_switch_skips_everything() {
        let r = rules(&[("code", 0.5)]);
        let mut f = facts("code", &r, &[]);
        f.enabled = false;
        assert_eq!(
            decide(&f),
            Decision::Skip {
                reason: SkipReason::Disabled
            }
        );
    }

    #[test]
    fn identification_finds_presets_and_explains_exclusions() {
        let id = identify("com.anthropic.Claude");
        assert_eq!(id.preset, Some("claude"));
        assert_eq!(id.excluded_reason, None);
        assert_eq!(id.suggested_rule_class(), Some("com.anthropic.Claude"));

        let guest = identify("VirtualBoxVM");
        assert_eq!(guest.excluded_reason, Some(SkipReason::VirtualMachine));

        let game = identify("steam_app_440");
        assert_eq!(game.excluded_reason, Some(SkipReason::Game));

        let unknown = identify("some-app");
        assert_eq!(unknown.preset, None);
        assert_eq!(unknown.excluded_reason, None);
        assert_eq!(unknown.suggested_rule_class(), Some("some-app"));
    }

    #[test]
    fn identification_handles_empty_and_odd_input() {
        let id = identify("");
        assert!(id.tokens.is_empty());
        assert_eq!(id.suggested_rule_class(), None);
        assert_eq!(id.excluded_reason, None);
    }

    #[test]
    fn every_skip_reason_has_an_explanation() {
        for reason in [
            SkipReason::Disabled,
            SkipReason::WrongWindowType,
            SkipReason::VirtualMachine,
            SkipReason::Game,
            SkipReason::Excluded,
            SkipReason::Fullscreen,
            SkipReason::NoMatch,
            SkipReason::FullyOpaque,
        ] {
            assert!(!reason.describe().is_empty());
        }
    }

    #[test]
    fn decisions_serialise_with_a_stable_shape() {
        let d = Decision::Opacity {
            opacity_tenths: 820,
            matched: "code".into(),
        };
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"opacity","opacity_tenths":820,"matched":"code"}"#
        );
        let back: Decision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }
}
