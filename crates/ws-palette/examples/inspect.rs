//! Print the derived palette for an image. Used for manual verification:
//! `cargo run -p ws-palette --example inspect -- path/to/wallpaper.jpg`

fn main() {
    let path = std::env::args().nth(1).expect("usage: inspect <image>");
    let sampled = match ws_palette::sample_path(std::path::Path::new(&path)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    println!("{}: {}x{}", path, sampled.width, sampled.height);
    println!(
        "  samples={} opaque={:.2} monochrome={}",
        sampled.samples.len(),
        sampled.opaque_fraction,
        sampled.is_monochrome
    );
    let d = ws_palette::derive(
        &sampled,
        None,
        255,
        &ws_palette::PaletteOverrides::default(),
    );
    println!("  accent={} source={:?}", d.accent, d.source);
    for (i, c) in d.clusters.iter().take(4).enumerate() {
        println!(
            "    cluster {i}: hue={:.0} chroma={:.3} l={:.3} share={:.2}",
            c.hue, c.chroma, c.lightness, c.weight
        );
    }
    for (name, rep) in [("dark", &d.dark), ("light", &d.light)] {
        let p = &rep.palette;
        println!("  {name}: accent={} bg={} surface={} button={} hover={} selection={} text={} warning={}",
            p.accent, p.background, p.surface, p.button, p.hover, p.selection, p.text, p.warning);
        println!(
            "        worst text ratio {:.2}; failures: {}",
            rep.worst_text_ratio(),
            rep.failures()
                .map(|c| format!(
                    "{} on {} = {:.2}",
                    c.foreground_href, c.background_href, c.ratio
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}
