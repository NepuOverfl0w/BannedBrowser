//! Offline regression tests: render saved real-world pages
//! (`tests/pages/snapshots/`) and compare with approved screenshots.
//!
//! Text rendering depends on the installed fonts, so the approved images are
//! tied to a font set; `approved/FONTS.txt` records which families the generic
//! names resolved to. On a machine with a different font set the comparison is
//! reported as skipped (it can't be meaningful). Set `BB_UPDATE_SNAPSHOTS=1`
//! to (re)approve after checking the new renders by eye.

use bb_engine::{Page, PageConfig};
use bb_url::Url;
use std::path::PathBuf;

const PAGES: &[(&str, &str)] = &[
    ("example.com", "example.com.html"),
    ("example.com-postjs", "example.com-postjs.html"),
    ("theoldnet", "theoldnet/index.html"),
    ("theoldnet-lite", "theoldnet-lite/index.html"),
    ("myip.wtf", "myip.wtf/index.html"),
];

fn font_fingerprint() -> String {
    let mut fs = bb_text::FontSystem::lock();
    ["serif", "sans-serif", "monospace", "system-ui"]
        .iter()
        .map(|g| {
            let l = fs.resolve(&[g.to_string()], 400, false);
            format!("{g}={}", fs.family_name(l.primary()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn snapshot_regressions() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/pages/snapshots");
    let approved = root.join("approved");
    let update = std::env::var_os("BB_UPDATE_SNAPSHOTS").is_some();
    let fp = font_fingerprint();
    let fp_file = approved.join("FONTS.txt");
    if update {
        std::fs::create_dir_all(&approved).unwrap();
        std::fs::write(&fp_file, &fp).unwrap();
    } else if std::fs::read_to_string(&fp_file).map(|s| s.trim() != fp.trim()).unwrap_or(true) {
        println!("snapshot comparison skipped: font set differs from the approved one\n{fp}");
        return;
    }
    let client = bb_engine::new_client(Vec::new());
    let mut failures = Vec::new();
    for (name, file) in PAGES {
        let path = root.join(file).canonicalize().unwrap();
        let url = Url::from_file_path(&path.to_string_lossy()).unwrap();
        let page = Page::load(&client, &url, PageConfig { viewport: (1024.0, 768.0), ..Default::default() });
        let canvas = page.render(0.0, 1.0, 1024, 768);
        let png = approved.join(format!("{name}.png"));
        if update {
            std::fs::write(&png, canvas.encode_png()).unwrap();
            continue;
        }
        let reference = std::fs::read(&png).unwrap_or_else(|_| panic!("missing {}", png.display()));
        let img = bb_image::decode(&reference).unwrap();
        // Fuzz: tolerate small AA differences on up to 0.5% of pixels.
        let mut bad = 0usize;
        for (a, b) in canvas.pixels.chunks_exact(4).zip(img.pixels().chunks_exact(4)) {
            if (0..3).any(|k| (a[k] as i32 - b[k] as i32).abs() > 24) {
                bad += 1;
            }
        }
        let limit = 1024 * 768 / 200;
        if bad > limit {
            let out = root.join("out");
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join(format!("{name}.png")), canvas.encode_png()).unwrap();
            failures.push(format!("{name}: {bad} pixels differ (limit {limit})"));
        }
    }
    assert!(failures.is_empty(), "snapshot regressions:\n{}", failures.join("\n"));
}
