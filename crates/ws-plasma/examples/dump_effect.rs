//! Write the generated KWin effect and colour scheme to a directory for manual
//! inspection and syntax checking. Development aid, not shipped.

use ws_core::TransparencyRules;

fn main() {
    let out = std::env::args().nth(1).expect("usage: dump_effect <dir>");
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(out.join("contents/code")).unwrap();
    let rules = TransparencyRules {
        enabled: true,
        default_opacity: 0.8,
        skip_fullscreen: true,
        exclusions: vec!["obs".into()],
        rules: vec![
            ("code".into(), 0.82),
            ("Claude com.anthropic.Claude".into(), 0.80),
            ("steam".into(), 0.85),
            ("VirtualBox Manager".into(), 0.88),
            ("weird\"quote".into(), 0.7),
        ],
    };
    let package = ws_plasma::kwin_effect::generate(&rules);
    std::fs::write(out.join("metadata.json"), &package.metadata).unwrap();
    std::fs::write(out.join("contents/code/main.js"), &package.script).unwrap();
    println!("wrote {}", out.display());
}
