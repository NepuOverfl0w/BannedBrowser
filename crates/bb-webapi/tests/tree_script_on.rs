//! The tree-construction tests marked `#script-on` (WPT html/syntax/parsing/resources/*.dat):
//! the parser with the scripting flag on and the documents' scripts running (bb-html's own
//! test runs every other case). Fragment cases parse with scripting on, without scripts.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use bb_url::Url;
use bb_webapi::{Embedder, Window};

struct E(Cell<f64>);

impl Embedder for E {
    fn fetch_script(&self, _url: &Url) -> Result<String, String> {
        Err("no network in tree tests".into())
    }
    fn now_ms(&self) -> f64 {
        self.0.get()
    }
}

struct Case {
    file: String,
    data: String,
    document: String,
}

/// The `#script-on` records of a .dat file (format as bb-html's tree test reads it).
fn cases(file: &str, text: &str) -> Vec<Case> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i] != "#data" {
            i += 1;
            continue;
        }
        i += 1;
        let mut data = Vec::new();
        while i < lines.len() && !lines[i].starts_with('#') {
            data.push(lines[i]);
            i += 1;
        }
        let (mut script_on, mut fragment, mut document) = (false, false, Vec::new());
        while i < lines.len() && lines[i] != "#data" {
            match lines[i] {
                "#script-on" => script_on = true,
                "#document-fragment" => fragment = true,
                "#document" => {
                    i += 1;
                    while i < lines.len() && lines[i] != "#data" && lines[i] != "#description" {
                        if lines[i].is_empty()
                            && (i + 1 >= lines.len()
                                || lines[i + 1] == "#data"
                                || lines[i + 1] == "#description")
                        {
                            break;
                        }
                        document.push(lines[i]);
                        i += 1;
                    }
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        if script_on && !fragment {
            while document.last() == Some(&"") {
                document.pop();
            }
            out.push(Case { file: file.to_string(), data: data.join("\n"), document: document.join("\n") });
        }
    }
    out
}

#[test]
fn tree_construction_script_on() {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/wpt/html/syntax/parsing/resources");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        panic!("{} missing: run scripts/fetch-spec-tests.sh", dir.display());
    };
    let mut files: Vec<PathBuf> =
        rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "dat")).collect();
    files.sort();
    let (mut pass, mut total, mut fails) = (0, 0, Vec::new());
    for f in files {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        for c in cases(&name, &text) {
            total += 1;
            let url = Url::parse("http://web-platform.test/tree.html").expect("url");
            let mut w = Window::new(url, Rc::new(E(Cell::new(0.0))));
            w.load_html(&c.data);
            let got = bb_dom::dump_html5lib(&w.document(), bb_dom::Document::ROOT);
            let got = got.trim_end_matches('\n');
            if got == c.document {
                pass += 1;
            } else {
                fails
                    .push(format!("{} {:?}\n--- expected\n{}\n--- got\n{}", c.file, c.data, c.document, got));
            }
        }
    }
    println!("tree construction #script-on: {pass}/{total}");
    assert!(total > 0, "no #script-on cases found");
    assert!(fails.is_empty(), "{} failing:\n{}", fails.len(), fails.join("\n\n"));
}
