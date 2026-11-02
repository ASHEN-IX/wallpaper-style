//! Platform-independent settings and domain logic for Wallpaper Style.
//!
//! Nothing in this crate knows about KDE, KWin, Kvantum or a display server. It
//! owns the settings schema and its migration chain, the preset catalogue and
//! window-matching policy, wallpaper *selection* policy, and the snapshot and
//! ownership model used for safe restore.
//!
//! ```
//! use ws_core::settings::Settings;
//! let settings = Settings::default();
//! assert!(!settings.general.automatic, "automatic mode is opt-in");
//! ```

#![deny(missing_debug_implementations)]
#![warn(clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::float_cmp,
    clippy::manual_midpoint,
    clippy::items_after_statements,
    clippy::new_ret_no_self,
    clippy::needless_pass_by_value,
    clippy::return_self_not_must_use,
    clippy::self_named_constructors,
    // The lints below fire only in test modules in this crate, where the flagged
    // forms are deliberate: exhaustive `other => panic!()` guards so a new enum
    // variant cannot pass silently, and `assert!(x.is_empty())` for readability.
    clippy::assert_is_empty,
    clippy::match_wildcard_for_single_variants,
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
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::too_many_lines,
    clippy::uninlined_format_args,
)]

pub mod ownership;
pub mod paths;
pub mod presets;
pub mod settings;
pub mod snapshot;
pub mod wallpaper;

pub use ownership::{OwnedItem, Ownership, RestoreEntry, RestoreOutcome};
pub use paths::{PathError, Paths};
pub use presets::{preset, Decision, Preset, SkipReason, TransparencyKind, PRESETS};
pub use settings::{
    AppOpacity, Settings, APP_NAME, CURRENT_SCHEMA_VERSION, KWIN_EFFECT_ID, MIN_OPACITY,
};
pub use snapshot::{Snapshot, SnapshotStore, SnapshotSummary};
pub use wallpaper::{MonitorWallpaper, PluginKind, Selection, WallpaperFingerprint};

/// Flat rules the transparency backend consumes.
///
/// Built from settings here so both the Rust `decide` policy and the generated
/// KWin effect are fed from one place.
#[derive(Debug, Clone, PartialEq)]
pub struct TransparencyRules {
    pub enabled: bool,
    pub default_opacity: f32,
    pub skip_fullscreen: bool,
    pub exclusions: Vec<String>,
    /// `(window class, opacity)` sorted by class for deterministic output.
    pub rules: Vec<(String, f32)>,
}

/// Collapse settings into the flat rule set the compositor effect understands.
///
/// Preset entries are expanded into their individual window classes; disabled
/// presets and disabled custom entries are dropped, and anything at 1.0 is
/// omitted because it would only add work. Custom entries win over presets when
/// they name the same class, so a user can always override a preset by hand.
pub fn transparency_rules(settings: &Settings) -> TransparencyRules {
    use std::collections::BTreeMap;

    let mut merged: BTreeMap<String, f32> = BTreeMap::new();

    for (preset_id, app) in &settings.transparency.apps {
        if !app.enabled {
            continue;
        }
        let Some(preset) = presets::preset(preset_id) else {
            log::warn!("settings reference unknown preset {preset_id:?}; ignoring it");
            continue;
        };
        for class in preset.window_classes {
            merged.insert((*class).to_owned(), app.opacity);
        }
    }

    for (class, app) in &settings.transparency.custom {
        if !app.enabled {
            merged.remove(class);
            continue;
        }
        merged.insert(class.clone(), app.opacity);
    }

    let rules: Vec<(String, f32)> = merged
        .into_iter()
        .map(|(class, opacity)| (class, opacity.clamp(settings::MIN_OPACITY, 1.0)))
        .filter(|(_, opacity)| *opacity < 1.0)
        .collect();

    TransparencyRules {
        enabled: settings.transparency.enabled,
        default_opacity: settings
            .transparency
            .default_opacity
            .clamp(settings::MIN_OPACITY, 1.0),
        skip_fullscreen: settings.transparency.skip_fullscreen,
        exclusions: settings.transparency.exclusions.clone(),
        rules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn disabling_a_preset_removes_its_classes() {
        let mut settings = Settings::default();
        settings
            .transparency
            .apps
            .insert("claude".into(), AppOpacity::new(0.7));
        let rules = transparency_rules(&settings);
        assert!(rules.rules.iter().any(|(c, _)| c == "Claude"));

        settings
            .transparency
            .apps
            .get_mut("claude")
            .unwrap()
            .enabled = false;
        let rules = transparency_rules(&settings);
        assert!(!rules.rules.iter().any(|(c, _)| c == "Claude"));
    }

    #[test]
    fn a_custom_rule_overrides_a_preset_for_the_same_class() {
        let mut settings = Settings::default();
        settings
            .transparency
            .apps
            .insert("claude".into(), AppOpacity::new(0.70));
        settings
            .transparency
            .custom
            .insert("Claude".into(), AppOpacity::new(0.95));
        let rules = transparency_rules(&settings);
        let value = rules
            .rules
            .iter()
            .find(|(c, _)| c == "Claude")
            .map(|(_, o)| *o);
        assert_eq!(value, Some(0.95));
    }

    #[test]
    fn fully_opaque_entries_are_dropped() {
        let mut settings = Settings::default();
        settings.transparency.custom = BTreeMap::from([("x".to_owned(), AppOpacity::new(1.0))]);
        assert!(transparency_rules(&settings).rules.is_empty());
    }

    #[test]
    fn rules_are_sorted_for_deterministic_output() {
        let mut settings = Settings::default();
        settings.transparency.custom = BTreeMap::from([
            ("zebra".to_owned(), AppOpacity::new(0.5)),
            ("alpha".to_owned(), AppOpacity::new(0.5)),
        ]);
        let rules = transparency_rules(&settings);
        let classes: Vec<&str> = rules.rules.iter().map(|(c, _)| c.as_str()).collect();
        let mut sorted = classes.clone();
        sorted.sort_unstable();
        assert_eq!(classes, sorted);
    }

    #[test]
    fn unknown_preset_ids_are_ignored_not_fatal() {
        let mut settings = Settings::default();
        settings
            .transparency
            .apps
            .insert("does-not-exist".into(), AppOpacity::new(0.5));
        let rules = transparency_rules(&settings);
        assert!(rules.rules.is_empty());
    }
}
