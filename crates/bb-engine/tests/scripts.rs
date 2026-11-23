//! Page scripts (milestone E): scripts change what is rendered; timers, click listeners and
//! `location` navigations go through the page.

use bb_engine::{new_client, Page, PageConfig};
use bb_url::Url;

fn page(html: &str) -> Page {
    let client = new_client(Vec::new());
    let url = Url::parse("http://example.test/page.html").unwrap();
    Page::from_html(
        &client,
        html,
        &url,
        PageConfig { viewport: (800.0, 600.0), ..Default::default() },
        200,
        false,
    )
}

fn text(p: &Page, id: &str) -> String {
    let n = p.doc.get_element_by_id(id).unwrap_or_else(|| panic!("no #{id}"));
    p.doc.text_content(n)
}

fn rect(p: &Page, id: &str) -> [f32; 4] {
    p.element_rects_by_id().into_iter().find(|(i, _)| i == id).unwrap_or_else(|| panic!("no #{id}")).1
}

#[test]
fn scripts_change_the_rendered_page() {
    let p = page(
        "<!doctype html><title>Before</title><noscript><p id=ns>no</p></noscript>\
         <p id=a>static</p><div id=box style='height:10px'></div>\
         <script>document.title = 'After';\
           document.getElementById('a').textContent = 'from script ' + (2 + 3);\
           document.getElementById('box').setAttribute('style', 'height: 77px');\
           document.write('<p id=w>written</p>');</script><p id=after>after</p>",
    );
    assert_eq!(p.title, "After");
    assert_eq!(text(&p, "a"), "from script 5");
    assert_eq!(text(&p, "w"), "written");
    assert_eq!(rect(&p, "box")[3], 77.0, "layout sees the script's style");
    // <noscript> is raw text with scripting on, and not rendered.
    assert!(p.element_rects_by_id().iter().all(|(i, _)| i != "ns"));
    // document.write output comes before the rest of the document.
    assert!(rect(&p, "w")[1] < rect(&p, "after")[1]);
}

#[test]
fn timers_run_through_the_page() {
    let mut p = page(
        "<!doctype html><p id=t>0</p><script>var n = 0;\
         setTimeout(function () { n++; document.getElementById('t').textContent = 'fired ' + n; }, 0);</script>",
    );
    assert_eq!(text(&p, "t"), "0");
    assert!(p.script_wakeup().is_some());
    // Wait for the timer to be due, then run it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while text(&p, "t") == "0" && std::time::Instant::now() < deadline {
        if let Some(d) = p.script_wakeup() {
            std::thread::sleep(d);
        }
        p.run_scripts();
    }
    assert_eq!(text(&p, "t"), "fired 1");
    assert!(p.script_repaint);
    assert!(p.script_wakeup().is_none(), "nothing left to run");
}

#[test]
fn click_listeners_run_and_can_cancel() {
    let mut p = page(
        "<!doctype html><a id=l href='/elsewhere' style='display:block;height:40px'\
         onclick='clicks++; if (clicks > 1) event.preventDefault()'>link</a>\
         <p id=c>0</p><script>var clicks = 0;\
         document.getElementById('l').addEventListener('mousedown', function (e) {\
           document.getElementById('c').textContent = e.type + ' ' + e.isTrusted + ' ' + e.clientX;\
         });</script>",
    );
    let [x, y, w, h] = rect(&p, "l");
    let (cx, cy) = ((x + w / 2.0).round(), y + h / 2.0);
    assert!(p.script_click(cx, cy), "the first click isn't canceled");
    assert_eq!(text(&p, "c"), format!("mousedown true {cx}"));
    assert!(!p.script_click(cx, cy), "the second click is canceled");
}

#[test]
fn location_assignment_asks_for_navigation() {
    let mut p = page("<!doctype html><script>location.href = 'next.html?x=1';</script>");
    let to = p.take_script_navigation().expect("a navigation");
    assert_eq!(to.href(), "http://example.test/next.html?x=1");
}

#[test]
fn scripting_off_renders_noscript() {
    let client = new_client(Vec::new());
    let url = Url::parse("http://example.test/").unwrap();
    let cfg = PageConfig { scripting: false, ..Default::default() };
    let p = Page::from_html(
        &client,
        "<noscript><p id=ns>no js</p></noscript><script>document.title='x'</script>",
        &url,
        cfg,
        200,
        false,
    );
    assert!(p.script.is_none());
    assert_eq!(text(&p, "ns"), "no js");
    assert_ne!(p.title, "x");
}

#[test]
fn styles_and_geometry_from_scripts() {
    let p = page(
        "<!doctype html><style>body{margin:0} #b{width:120px;height:30px;padding:5px;border:2px solid red}\
         .x{color:rgb(0, 128, 0)}</style>\
         <div id=b class=x>box</div><p id=out></p><p id=out2></p>\
         <script>var b = document.getElementById('b'), r = b.getBoundingClientRect();\
         var cs = getComputedStyle(b);\
         document.getElementById('out').textContent = [r.width, r.height, b.offsetWidth, b.clientWidth,\
           cs.color, cs.getPropertyValue('display'), cs.width, document.elementFromPoint(10, 10) === b].join(' ');\
         b.style.backgroundColor = 'blue'; b.style.setProperty('margin-left', '10px', 'important');\
         document.getElementById('out2').textContent = b.style.cssText + ' | ' + b.style.marginLeft + ' ' +\
           b.style.getPropertyPriority('margin-left') + ' ' + b.style.length;</script>",
    );
    assert_eq!(text(&p, "out"), "134 44 134 130 rgb(0, 128, 0) block 120px true");
    assert_eq!(text(&p, "out2"), "background-color: blue; margin-left: 10px !important; | 10px important 2");
    assert_eq!(rect(&p, "b")[0], 10.0, "the inline style applies");
}

/// Run the page's tasks until `id`'s text stops being `from` (or 5 s pass).
fn wait_for_change(p: &mut Page, id: &str, from: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while text(p, id) == from && std::time::Instant::now() < deadline {
        if let Some(d) = p.script_wakeup() {
            std::thread::sleep(d.min(std::time::Duration::from_millis(20)));
        }
        p.run_scripts();
    }
}

#[test]
fn fetch_xhr_and_url_from_scripts() {
    let mut p = page(
        "<!doctype html><p id=f>waiting</p><p id=x>waiting</p><p id=u></p>\
         <script>\
         fetch('data:text/plain,hello%20fetch').then(r => r.text().then(t => {\
           document.getElementById('f').textContent = r.status + ' ' + r.ok + ' ' + t;\
           return fetch('data:application/json,{\"a\":[1,2]}');\
         })).then(r => r.json()).then(j => { document.getElementById('f').textContent += ' ' + j.a.length; });\
         var x = new XMLHttpRequest(); x.open('GET', 'data:text/plain,from%20xhr');\
         x.onload = function () { document.getElementById('x').textContent = x.readyState + ' ' + x.status + ' ' + x.responseText; };\
         x.send();\
         var u = new URL('../a/b?c=1#d', 'https://example.com/x/y');\
         u.searchParams.append('e', '2 3'); u.hostname = 'example.org';\
         document.getElementById('u').textContent = u.href + ' ' + new TextEncoder().encode('é').length;\
         </script>",
    );
    assert_eq!(text(&p, "u"), "https://example.org/a/b?c=1&e=2+3#d 2");
    wait_for_change(&mut p, "f", "waiting");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !text(&p, "f").ends_with(" 2") && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
        p.run_scripts();
    }
    assert_eq!(text(&p, "f"), "200 true hello fetch 2");
    wait_for_change(&mut p, "x", "waiting");
    assert_eq!(text(&p, "x"), "4 200 from xhr");
}
