//! Page scripts (milestone E): scripts change what is rendered; timers, click listeners and
//! `location` navigations go through the page.

use bb_dom::Document;
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

#[test]
fn dedicated_workers_and_structured_clone() {
    let mut p = page(
        "<!doctype html><p id=out>wait</p><script>\
           var src = 'onmessage = function (e) {' +\
             ' var m = e.data; m.map.set(\"seen\", self.name);' +\
             ' setTimeout(function () { postMessage({ map: m.map, sum: m.bytes.reduce(function (a, b) { return a + b; }, 0) }); }, 1);' +\
             ' if (m.fail) { throw new Error(\"boom\"); }' +\
             '};';\
           var w = new Worker(URL.createObjectURL(new Blob([src])), { name: 'w1' });\
           var parts = [];\
           var copy = structuredClone({ d: new Date(5), r: /a/g, s: new Set([1]) });\
           parts.push(copy.d.getTime() + copy.r.flags + copy.s.has(1));\
           w.onerror = function (e) { parts.push('error:' + /boom/.test(e.message)); e.preventDefault(); done(); };\
           w.onmessage = function (e) { parts.push(e.data.map.get('seen') + e.data.sum); done(); };\
           var n = 0;\
           function done() { if (++n == 2) document.getElementById('out').textContent = parts.join('|'); }\
           w.postMessage({ map: new Map([['a', 1]]), bytes: new Uint8Array([1, 2, 3]), fail: true });\
         </script>",
    );
    wait_for_change(&mut p, "out", "wait");
    let t = text(&p, "out");
    assert!(t.starts_with("5gtrue|"), "{t}");
    assert!(t.contains("w16") && t.contains("error:true"), "{t}");
}

#[test]
fn message_channels_and_ports_in_workers() {
    let mut p = page(
        "<!doctype html><p id=out>wait</p><script>\
           var parts = [];\
           var ch = new MessageChannel();\
           ch.port1.onmessage = function (e) { parts.push('local:' + e.data); next(); };\
           ch.port2.postMessage('tick');\
           var w = new Worker(URL.createObjectURL(new Blob([\
             'onmessage = function (e) { e.ports[0].postMessage(\"via port \" + e.data); };'])));\
           var ch2 = new MessageChannel();\
           ch2.port1.onmessage = function (e) { parts.push(e.data); next(); };\
           w.postMessage('hello', [ch2.port2]);\
           var n = 0;\
           function next() { if (++n == 2) document.getElementById('out').textContent = parts.sort().join('|'); }\
         </script>",
    );
    wait_for_change(&mut p, "out", "wait");
    assert_eq!(text(&p, "out"), "local:tick|via port hello");
}

#[test]
fn canvas_drawings_are_painted() {
    let p = page(
        "<!doctype html><style>body{margin:0}</style><canvas id=c width=100 height=50></canvas>\
         <p id=out></p><script>\
           var ctx = document.getElementById('c').getContext('2d');\
           ctx.fillStyle = 'rgb(255, 0, 0)';\
           ctx.fillRect(0, 0, 50, 50);\
           ctx.fillStyle = '#00f';\
           ctx.beginPath(); ctx.arc(75, 25, 20, 0, Math.PI * 2); ctx.fill();\
           var d = ctx.getImageData(75, 25, 1, 1).data;\
           out.textContent = [d[0], d[1], d[2], d[3], c.toDataURL().slice(0, 22)].join(',');\
         </script>",
    );
    assert_eq!(text(&p, "out"), "0,0,255,255,data:image/png;base64,");
    let r = p.render(0.0, 1.0, 120, 60);
    let at = |x: usize, y: usize| {
        let i = (y * r.w as usize + x) * 4;
        [r.pixels[i], r.pixels[i + 1], r.pixels[i + 2]]
    };
    assert_eq!(at(10, 10), [255, 0, 0], "the canvas is painted");
    assert_eq!(at(75, 25), [0, 0, 255]);
    assert_eq!(at(110, 10), [255, 255, 255], "outside the canvas");
}

#[test]
fn frames_have_their_own_windows() {
    // A same-origin srcdoc frame and an opaque-origin data: frame.
    let child = "<p id=c>child</p><script>\
        parent.document.getElementById('a').textContent = 'from frame ' + frameElement.id + ' ' + \
          (parent === top) + ' ' + (window !== parent);\
        document.getElementById('c').textContent = 'child ran';</script>";
    let data = "data:text/html,<script>var r = []; try { parent.document; r.push('read') } \
        catch (e) { r.push(e.name) } parent.postMessage(r.join() + ' ' + typeof parent.length, '*');</script>";
    let html = format!(
        "<!doctype html><p id=a>-</p><p id=b>-</p><p id=m>-</p>\
         <iframe id=f1 srcdoc=\"{child}\" width=200 height=50></iframe>\
         <iframe id=f2 src=\"{data}\" width=200 height=50></iframe>\
         <script>onmessage = function (e) {{\
           document.getElementById('m').textContent = e.data + ' ' + (e.source === frames[1]) + ' ' + e.origin; }};\
         onload = function () {{\
           var r = [length, frames[0].document.getElementById('c').textContent,\
             document.getElementById('f2').contentDocument];\
           try {{ frames[1].document; r.push('leaked') }} catch (e) {{ r.push(e.name) }}\
           try {{ frames[1].someGlobal = 1; r.push('set') }} catch (e) {{ r.push(e.name) }}\
           r.push(typeof frames[1].postMessage, frames[0] instanceof Object);\
           document.getElementById('b').textContent = r.join();\
         }};</script>",
        child = child.replace('"', "&quot;"),
        data = data.replace('"', "&quot;"),
    );
    let mut p = page(&html);
    assert_eq!(text(&p, "a"), "from frame f1 true true");
    // A frame's realm has its own intrinsics: its window isn't an instance of ours.
    assert_eq!(text(&p, "b"), "2,child ran,,SecurityError,SecurityError,function,false");
    // The frames render their own documents (changed by their scripts).
    let c = p.frames[0].page.doc.get_element_by_id("c").expect("the frame's document");
    assert_eq!(p.frames[0].page.doc.text_content(c), "child ran");
    wait_for_change(&mut p, "m", "-");
    assert_eq!(text(&p, "m"), "SecurityError number true null");
}

#[test]
fn removed_frames_stop_running() {
    let mut p = page(
        "<!doctype html><p id=t>-</p><iframe id=f srcdoc='x'></iframe><script>try {\
         var w = frames[0], f = w.Function('return 1 + 1');\
         var r = [f()];\
         document.getElementById('f').remove();\
         try { f(); r.push('ran') } catch (e) { r.push(e.name) }\
         try { w.document; r.push('ran') } catch (e) { r.push(e.name) }\
         r.push(length);\
         var i = document.createElement('iframe'); document.body.appendChild(i);\
         i.contentDocument.open(); i.contentDocument.write('<b id=x>written</b>'); i.contentDocument.close();\
         r.push(i.contentDocument.getElementById('x').textContent);\
         document.getElementById('t').textContent = r.join();\
         } catch (e) { document.getElementById('t').textContent = 'threw ' + e + ' ' + r; }</script>",
    );
    assert_eq!(text(&p, "t"), "2,TypeError,TypeError,0,written");
    p.run_scripts();
}

#[test]
fn module_scripts_import() {
    let mut p = page(
        "<!doctype html><p id=m>-</p><p id=d>-</p><p id=b>-</p>\
         <script type=module>import { v } from 'data:text/javascript,export const v = 40 + 2';\
         document.getElementById('m').textContent = 'static ' + v;\
         import('data:text/javascript,export default 7').then(function (x) {\
           document.getElementById('d').textContent = 'dynamic ' + x.default; });\
         import('bare').catch(function (e) { document.getElementById('b').textContent = e.name; });</script>\
         <p id=s>-</p><script id=late></script><script>\
         document.getElementById('late').src = \"data:text/javascript,document.getElementById('s').textContent = 'src set'\";</script>",
    );
    assert_eq!(text(&p, "m"), "static 42");
    wait_for_change(&mut p, "d", "-");
    assert_eq!(text(&p, "d"), "dynamic 7");
    // Bare specifiers need import maps, which aren't supported.
    assert_eq!(text(&p, "b"), "TypeError");
    // Setting src on a connected script that never ran runs it.
    wait_for_change(&mut p, "s", "-");
    assert_eq!(text(&p, "s"), "src set");
}

#[test]
fn custom_elements() {
    let p = page(
        "<!doctype html><x-early id=e></x-early><p id=log></p><script>\
         var log = [];\
         class XEl extends HTMLElement {\
           static observedAttributes = ['v'];\
           constructor() { super(); log.push('ctor:' + (this.id || 'new')); }\
           connectedCallback() { log.push('connected:' + this.id); }\
           attributeChangedCallback(n, o, v) { log.push('attr:' + n + '=' + v); }\
         }\
         customElements.define('x-early', XEl);\
         log.push(document.getElementById('e') instanceof XEl);\
         var made = document.createElement('x-early');\
         made.id = 'm'; made.setAttribute('v', '1');\
         document.body.appendChild(made);\
         customElements.whenDefined('x-early').then(function (c) { log.push('defined:' + (c === XEl)); });\
         log.push(customElements.get('x-early') === XEl, customElements.getName(XEl));\
         try { customElements.define('nodash', class extends HTMLElement {}); } catch (e) { log.push(e.name); }\
         </script><x-early id=late v=2></x-early><script>\
         document.getElementById('log').textContent = log.join(' ');</script>",
    );
    assert_eq!(
        text(&p, "log"),
        "ctor:e connected:e true ctor:new attr:v=1 connected:m true x-early SyntaxError defined:true ctor:late attr:v=2 connected:late"
    );
}

#[test]
fn streams() {
    let mut p = page(
        "<!doctype html><p id=log>-</p><script>\
         (async function () {\
           var log = [];\
           var r = new Blob(['hello ', 'streams']).stream().pipeThrough(new TextDecoderStream()).getReader();\
           var s = ''; for (;;) { var x = await r.read(); if (x.done) break; s += x.value; }\
           log.push(s);\
           var res = new Response('body text');\
           var reader = res.body.getReader();\
           var first = await reader.read();\
           log.push(first.value.length, res.bodyUsed);\
           var up = new TransformStream({ transform(c, ctl) { ctl.enqueue(c.toUpperCase()); } });\
           var w = up.writable.getWriter(); w.write('a'); w.write('b'); w.close();\
           var collected = [];\
           await up.readable.pipeTo(new WritableStream({ write(c) { collected.push(c); } }));\
           log.push(collected.join(''));\
           var text = await new Response(ReadableStream.from(['x', 'y'].map(c => new TextEncoder().encode(c)))).text();\
           log.push(text);\
           document.getElementById('log').textContent = log.join(' ');\
         })().catch(function (e) { document.getElementById('log').textContent = 'error ' + e; });</script>",
    );
    wait_for_change(&mut p, "log", "-");
    assert_eq!(text(&p, "log"), "hello streams 9 true AB xy");
}

#[test]
fn shadow_trees_render_and_scope_styles() {
    let mut p = page(
        "<!doctype html><style>p { color: rgb(0, 0, 255) }</style>\
         <div id=host><template shadowrootmode=open>\
         <style>p { color: rgb(255, 0, 0) } :host { display: block; padding-left: 50px }\
         ::slotted(span) { font-weight: bold }</style>\
         <p>Shadow text</p><slot name=a></slot><slot>fallback</slot></template>\
         <span id=s slot=a>slotted</span></div>\
         <p id=outside>outside</p><div id=imperative></div><p id=log></p><script>\
         var log = [];\
         var host = document.getElementById('host');\
         log.push(host.shadowRoot.mode, host.shadowRoot.querySelector('p').textContent.length);\
         log.push(document.getElementById('s').assignedSlot.name);\
         var sr = document.getElementById('imperative').attachShadow({ mode: 'closed' });\
         sr.innerHTML = '<b id=inner>in shadow</b>';\
         log.push(String(document.getElementById('inner')), sr.getElementById('inner').textContent.length);\
         var path = [];\
         sr.firstChild.addEventListener('ping', function (e) { path.push(e.composedPath().length); });\
         document.body.addEventListener('ping', function (e) { path.push(e.target.id); });\
         sr.firstChild.dispatchEvent(new Event('ping', { bubbles: true, composed: true }));\
         log.push(path.join(','));\
         document.getElementById('log').textContent = log.join(' ');</script>",
    );
    assert_eq!(text(&p, "log"), "open 11 a null 9 7,imperative");
    // The shadow tree is rendered (its own <style> applies inside it only), the slotted span
    // is placed in its slot, and :host styles the host.
    let shadow_p = p
        .doc
        .shadow_including_descendants(Document::ROOT)
        .into_iter()
        .find(|&n| p.doc.is_html(n, "p") && p.doc.containing_shadow_root(n).is_some());
    let shadow_p = shadow_p.expect("the shadow tree's <p>");
    assert_eq!(p.styles.get(shadow_p).unwrap().color, bb_css::Rgba { r: 255, g: 0, b: 0, a: 255 });
    let outside = p.doc.get_element_by_id("outside").unwrap();
    assert_eq!(p.styles.get(outside).unwrap().color, bb_css::Rgba { r: 0, g: 0, b: 255, a: 255 });
    let s = p.doc.get_element_by_id("s").unwrap();
    assert_eq!(p.styles.get(s).unwrap().font_weight, 700);
    assert!(rect(&p, "s")[0] >= 50.0, "the host's padding comes from :host");
    assert_eq!(p.find("Shadow text", 0.0).0, 1);
    assert_eq!(p.find("fallback", 0.0).0, 1);
    assert_eq!(p.find("in shadow", 0.0).0, 1);
}
