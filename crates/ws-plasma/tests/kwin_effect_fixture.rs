//! Integration test: drive the *generated* KWin effect through a simulated
//! compositor.
//!
//! Window-lifecycle behaviour (leaving a virtual desktop, switching activity,
//! minimising, fullscreen, closing) cannot be observed from outside a live
//! compositor, and running a real KWin instance in a test is neither safe nor
//! hermetic. Instead the generated JavaScript is executed against a small
//! simulation of the KWin scripting host (`tests/support/kwin_sim.js`) that
//! implements exactly the API surface the effect is allowed to use.
//!
//! The simulation is deliberately *closed*: it provides `effects`, `Effect`,
//! `set` and `cancel` and nothing else. An effect that reached for a
//! window-moving API would throw rather than silently succeed, which is how the
//! "never restack or move a window" rule is enforced here.
//!
//! Skipped with a clear message when `node` is unavailable, so the Rust test
//! suite still passes on a machine without a JavaScript runtime.

use std::path::PathBuf;
use std::process::Command;

use ws_core::TransparencyRules;

fn project_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/ws-plasma.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The rule set used by the simulation. It intentionally covers every preset
/// family and the exclusion rules the specification calls out.
fn fixture_rules() -> TransparencyRules {
    TransparencyRules {
        enabled: true,
        default_opacity: 0.8,
        skip_fullscreen: true,
        exclusions: vec!["obs".into()],
        rules: vec![
            ("code".into(), 0.82),
            ("Claude com.anthropic.Claude".into(), 0.80),
            ("steam".into(), 0.85),
            ("VirtualBox Manager".into(), 0.88),
        ],
    }
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn generated_effect_respects_window_lifecycle_rules() {
    if !node_available() {
        eprintln!("SKIP: node is not installed; KWin effect lifecycle simulation not run");
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    let package = ws_plasma::kwin_effect::generate(&fixture_rules());
    let script_dir = dir.path().join("effect/contents/code");
    std::fs::create_dir_all(&script_dir).expect("create effect dir");
    let script_path = script_dir.join("main.js");
    std::fs::write(&script_path, &package.script).expect("write effect");

    // A syntax check first: a broken script must fail loudly rather than be
    // discovered when a user's desktop stops compositing correctly.
    let syntax = Command::new("node")
        .arg("--check")
        .arg(&script_path)
        .output()
        .expect("run node --check");
    assert!(
        syntax.status.success(),
        "generated effect is not valid JavaScript:\n{}",
        String::from_utf8_lossy(&syntax.stderr)
    );

    let runner = project_root().join("tests/kwin_effect_lifecycle.js");
    let output = Command::new("node")
        .arg(&runner)
        .env("WS_EFFECT_JS", &script_path)
        .output()
        .expect("run simulation");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "lifecycle simulation failed\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("RESULT "),
        "simulation produced no summary:\n{stdout}"
    );
    assert!(
        !stdout.contains("FAIL"),
        "at least one lifecycle check failed:\n{stdout}"
    );
}

#[test]
fn the_generated_script_is_pure_ascii_and_free_of_placeholders() {
    let package = ws_plasma::kwin_effect::generate(&fixture_rules());
    assert!(
        package.script.is_ascii(),
        "KWin loads scripts as UTF-8 but ASCII avoids surprises"
    );
    for token in [
        "__RULES__",
        "__ENABLED__",
        "__DEFAULT__",
        "__SKIP_FULLSCREEN__",
        "__EXCLUSIONS__",
    ] {
        assert!(!package.script.contains(token), "unsubstituted {token}");
    }
}

#[test]
fn disabling_transparency_makes_the_effect_dormant_but_valid() {
    if !node_available() {
        eprintln!("SKIP: node is not installed");
        return;
    }
    let mut rules = fixture_rules();
    rules.enabled = false;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.js");
    std::fs::write(&path, ws_plasma::kwin_effect::generate(&rules).script).unwrap();
    let check = Command::new("node")
        .arg("--check")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
}
