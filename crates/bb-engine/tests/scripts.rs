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

#[test]
fn inserted_scripts_and_images() {
    // A 2×3 PNG.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAIAAAADCAIAAAA2iEnWAAAAEElEQVR4nGP4z8AARAwoFABE0AX7pM/egAAAAABJRU5ErkJggg==";
    let html = format!(
        "<!doctype html><p id=a>no</p><p id=b>no</p>\
         <script>var s = document.createElement('script'); s.textContent = \"document.getElementById('a').textContent = 'inline ran'\";\
         document.body.appendChild(s);\
         var e = document.createElement('script'); e.src = \"data:text/javascript,document.getElementById('b').textContent = 'external ran'\";\
         document.body.appendChild(e);\
         var i = document.createElement('img'); i.id = 'img'; i.src = 'data:image/png;base64,{png}'; document.body.appendChild(i);\
         setTimeout(function () {{ var j = new Image(); j.id = 'img2'; j.src = i.src + '#later'; document.body.appendChild(j);\
           document.getElementById('a').textContent = 'timer ran'; }}, 0);</script>"
    );
    let mut p = page(&html);
    assert_eq!(text(&p, "a"), "inline ran", "an inserted inline script runs at once");
    wait_for_change(&mut p, "b", "no");
    assert_eq!(text(&p, "b"), "external ran", "an inserted external script runs as a task");
    let [_, _, w, h] = rect(&p, "img");
    assert_eq!((w, h), (2.0, 3.0), "the inserted image was loaded");
    wait_for_change(&mut p, "a", "inline ran");
    let [_, _, w, h] = rect(&p, "img2");
    assert_eq!((w, h), (2.0, 3.0), "an image inserted later loads too");
}

#[test]
fn storage_and_cookies() {
    let client = new_client(Vec::new());
    let url = Url::parse("http://example.test/page.html").unwrap();
    let cfg = PageConfig { viewport: (800.0, 600.0), ..Default::default() };
    let first = "<!doctype html><p id=o></p><script>localStorage.setItem('a', '1'); localStorage.b = '2';\
                 sessionStorage.s = 'x'; document.cookie = 'c=3; path=/';\
                 document.getElementById('o').textContent = localStorage.length + ' ' + localStorage.getItem('b') +\
                   ' ' + Object.keys(localStorage).join(',') + ' ' + document.cookie;</script>";
    let p = Page::from_html(&client, first, &url, cfg, 200, false);
    assert_eq!(text(&p, "o"), "2 2 a,b c=3");
    // Another page of the same origin, with the same client: localStorage and the cookie stay;
    // sessionStorage is the window's.
    let second = "<!doctype html><p id=o></p><script>delete localStorage.a;\
                  document.getElementById('o').textContent = JSON.stringify(localStorage) + ' ' +\
                    sessionStorage.length + ' ' + document.cookie;</script>";
    let p2 = Page::from_html(&client, second, &url, cfg, 200, false);
    assert_eq!(text(&p2, "o"), "{\"b\":\"2\"} 0 c=3");
    assert_eq!(client.storage().area("http://example.test"), vec![("b".to_string(), "2".to_string())]);
    // Another origin sees none of it.
    let other = Url::parse("http://other.test/").unwrap();
    let p3 = Page::from_html(&client, second, &other, cfg, 200, false);
    assert_eq!(text(&p3, "o"), "{} 0 ");
}

#[test]
fn mutation_observers_ranges_and_traversal() {
    let p = page(
        "<!doctype html><ul id=list><li>one</li></ul><p id=log></p><p id=r>Hello brave world</p>\
         <p id=walk><b>x</b><i>y</i></p><p id=named></p>\
         <script>\
           var log = [];\
           var mo = new MutationObserver(function (records) {\
             records.forEach(function (r) { log.push(r.type + ':' + (r.attributeName || r.addedNodes.length)); });\
             document.getElementById('log').textContent = log.join(',');\
           });\
           mo.observe(list, { childList: true, attributes: true, subtree: true });\
           var li = document.createElement('li'); li.textContent = 'two'; list.appendChild(li);\
           list.setAttribute('data-x', '1');\
           var range = document.createRange();\
           var t = document.getElementById('r').firstChild;\
           range.setStart(t, 6); range.setEnd(t, 11);\
           var picked = range.toString();\
           range.deleteContents();\
           var w = document.createTreeWalker(document.getElementById('walk'), NodeFilter.SHOW_ELEMENT);\
           var tags = []; while (w.nextNode()) tags.push(w.currentNode.tagName);\
           getSelection().selectAllChildren(document.getElementById('walk'));\
           named.textContent = picked + '|' + tags.join('') + '|' + getSelection().toString();\
         </script>",
    );
    // The observer ran at the microtask checkpoint after the script.
    assert_eq!(text(&p, "log"), "childList:1,attributes:data-x");
    assert_eq!(text(&p, "r"), "Hello  world");
    // `named` is the element with that id (named access on the Window).
    assert_eq!(text(&p, "named"), "brave|BI|xy");
}

#[test]
fn dom_parser_and_xml_serializer() {
    let p = page(
        "<!doctype html><p id=out></p><script>\
           var d = new DOMParser().parseFromString(\
             '<feed xmlns=\"urn:f\"><e n=\"1\">a &amp; b<![CDATA[<x>]]></e></feed>', 'application/xml');\
           var e = d.documentElement.firstChild;\
           var bad = new DOMParser().parseFromString('<a><b></a>', 'text/xml');\
           var html = new DOMParser().parseFromString('<p>hi<script>window.ran = 1</scr' + 'ipt>', 'text/html');\
           document.body.appendChild(document.adoptNode(html.body.firstChild));\
           out.textContent = [d.documentElement.namespaceURI, e.getAttribute('n'), e.textContent,\
             e.lastChild.nodeType, bad.documentElement.localName,\
             new XMLSerializer().serializeToString(d), typeof window.ran].join('|');\
         </script>",
    );
    assert_eq!(
        text(&p, "out"),
        "urn:f|1|a & b<x>|4|parsererror|<feed xmlns=\"urn:f\"><e n=\"1\">a &amp; b<![CDATA[<x>]]></e></feed>|undefined"
    );
}

#[test]
fn blobs_files_form_data_and_crypto() {
    let mut p = page(
        "<!doctype html><form id=f><input name=a value=1><input name=b value=2></form><p id=out>wait</p><script>\
           var parts = [];\
           var b = new Blob(['hello ', new Uint8Array([119, 111, 114, 108, 100])], { type: 'Text/Plain' });\
           parts.push(b.size + b.type + new File(['x'], 'n.txt').name);\
           var fd = new FormData(document.getElementById('f'));\
           fd.append('file', b, 'h.txt');\
           parts.push([...fd.keys()].join(','));\
           var r = new FileReader();\
           r.onload = function () {\
             parts.push(r.result);\
             var u = new Uint8Array(16); crypto.getRandomValues(u);\
             parts.push(/^[0-9a-f-]{36}$/.test(crypto.randomUUID()) && u.some(function (x) { return x; }));\
             fetch(URL.createObjectURL(b.slice(0, 5))).then(function (res) { return res.text(); }).then(function (t) {\
               parts.push(t);\
               return new Response(fd).formData();\
             }).then(function (back) {\
               parts.push(back.get('a') + back.get('file').name + back.get('file').size);\
               document.getElementById('out').textContent = parts.join('|');\
             });\
           };\
           r.readAsText(b);\
         </script>",
    );
    wait_for_change(&mut p, "out", "wait");
    assert_eq!(text(&p, "out"), "11text/plainn.txt|a,b,file|hello world|true|hello|1h.txt11");
}
