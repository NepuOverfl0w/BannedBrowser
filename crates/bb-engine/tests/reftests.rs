//! Reftests: every `tests/reftests/*.html` with `<link rel="match" href="ref.html">`
//! is rendered headless at 800×600 and compared with its reference.
//!
//! Fuzz: channel differences ≤ 3 are ignored by default; a test may widen this
//! with `<meta name="fuzzy" content="maxDifference=N;totalPixels=M">`.
//! On failure, `tests/reftests/out/<name>-{test,ref,diff}.png` are written.

use bb_engine::{Page, PageConfig};
use bb_url::Url;
use std::path::{Path, PathBuf};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/reftests").canonicalize().unwrap()
}

fn attr(html: &str, tag_pat: &str, attr: &str) -> Option<String> {
    let i = html.find(tag_pat)?;
    let rest = &html[i..];
    let end = rest.find('>')?;
    let tag = &rest[..end];
    let a = tag.find(&format!("{attr}=\""))? + attr.len() + 2;
    let b = tag[a..].find('"')?;
    Some(tag[a..a + b].to_string())
}

fn render(client: &bb_net::Client, path: &Path) -> bb_paint::SoftwareCanvas {
    let url = Url::from_file_path(&path.to_string_lossy()).unwrap();
    let cfg = PageConfig { viewport: (800.0, 600.0), ..Default::default() };
    let page = Page::load(client, &url, cfg);
    assert!(page.error.is_none(), "{}: {:?}", path.display(), page.error);
    page.render(0.0, 1.0, 800, 600)
}

#[test]
fn reftests() {
    let d = dir();
    let client = bb_engine::new_client(Vec::new());
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&d)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "html"))
        .collect();
    entries.sort();
    let (mut pass, mut total) = (0, 0);
    let mut failures = Vec::new();
    for test in entries {
        let html = std::fs::read_to_string(&test).unwrap();
        let Some(reference) = attr(&html, "<link rel=\"match\"", "href") else { continue };
        total += 1;
        let (max_diff, max_pixels) = attr(&html, "<meta name=\"fuzzy\"", "content")
            .map(|c| {
                let get = |k: &str| {
                    c.split(';')
                        .find_map(|kv| kv.trim().strip_prefix(&format!("{k}=")))
                        .and_then(|v| v.rsplit('-').next()?.parse().ok())
                };
                (get("maxDifference").unwrap_or(3u32), get("totalPixels").unwrap_or(0u32))
            })
            .unwrap_or((3, 0));
        let a = render(&client, &test);
        let b = render(&client, &d.join(&reference));
        let mut bad = 0u32;
        let mut worst = 0u32;
        let mut diff = vec![0u8; a.pixels.len()];
        for (i, (pa, pb)) in a.pixels.chunks_exact(4).zip(b.pixels.chunks_exact(4)).enumerate() {
            let dmax = (0..4).map(|k| (pa[k] as i32 - pb[k] as i32).unsigned_abs()).max().unwrap();
            worst = worst.max(dmax);
            if dmax > max_diff {
                bad += 1;
                diff[i * 4..i * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            } else {
                diff[i * 4..i * 4 + 4].copy_from_slice(&[
                    pa[0] / 3 + 170,
                    pa[1] / 3 + 170,
                    pa[2] / 3 + 170,
                    255,
                ]);
            }
        }
        let name = test.file_stem().unwrap().to_string_lossy().into_owned();
        if bad <= max_pixels {
            pass += 1;
        } else {
            let out = d.join("out");
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join(format!("{name}-test.png")), a.encode_png()).unwrap();
            std::fs::write(out.join(format!("{name}-ref.png")), b.encode_png()).unwrap();
            let mut dc = bb_paint::SoftwareCanvas::new(800, 600);
            dc.pixels = diff;
            std::fs::write(out.join(format!("{name}-diff.png")), dc.encode_png()).unwrap();
            failures.push(format!("{name}: {bad} pixels differ (worst channel diff {worst})"));
        }
    }
    for f in &failures {
        println!("FAIL {f}");
    }
    println!("reftests: {pass}/{total} passed");
    assert!(total > 0, "no reftests found");
    assert!(failures.is_empty(), "{} reftest(s) failed", failures.len());
}
