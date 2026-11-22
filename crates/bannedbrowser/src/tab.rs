//! Per-tab worker threads. Each tab owns its `Page` (pages are not `Send`)
//! on a dedicated thread; the UI thread talks to it with messages and never
//! blocks on network, parsing or layout.

use bb_engine::{Page, PageConfig};
use bb_net::Client;
use bb_url::Url;
use crossbeam_channel::{Receiver, Sender};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum TabCmd {
    Navigate(Url),
    Render {
        viewport: (f32, f32),
        scale: f32,
        scroll_y: f32,
        dark: bool,
    },
    Hover {
        x: f32,
        y: f32,
    },
    /// `new_tab`: Ctrl/Cmd- or middle-click (open in a background tab).
    Click {
        x: f32,
        y: f32,
        new_tab: bool,
    },
    /// Find in page: `step` 0 = new query (first match at/after `from_y`), ±1 = next/previous.
    Find {
        query: String,
        step: i8,
        from_y: f32,
    },
    FindClose,
    /// Back/Forward between history entries of the same document: bring the frames to this
    /// snapshot (only frames whose URL differs are reloaded).
    RestoreFrames(FrameState),
    /// Wheel over a child frame (page coordinates).
    Wheel {
        x: f32,
        y: f32,
        dy: f32,
    },
    Shutdown,
}

/// A rendered viewport (premultiplied RGBA8, device pixels).
#[derive(Debug, Clone)]
pub struct Frame {
    pub pixels: Arc<Vec<u8>>,
    pub w: u32,
    pub h: u32,
    pub scroll_y: f32,
    /// Child frame rects in page coordinates (wheel events over them go to the tab).
    pub frames: Vec<bb_paint::Rect>,
    /// For internal pages (about:, error pages): rects of elements with an `id`, in page
    /// coordinates, so the GUI tests can find the page's buttons. Empty for web pages.
    pub targets: Vec<(String, [f32; 4])>,
}

/// Where a clicked link should open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOpen {
    Current,
    BackgroundTab,
    ForegroundTab,
}

/// The URL of every child frame: (path of frame indices, URL); see `Page::frame_urls`.
pub type FrameState = Vec<(Vec<usize>, Url)>;

/// A session history entry: the top-level document and its frames' URLs.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub url: Url,
    /// Empty until the document has loaded (then its initial frames are recorded).
    pub frames: FrameState,
}

impl HistoryEntry {
    pub fn new(url: Url) -> Self {
        HistoryEntry { url, frames: Vec::new() }
    }
}

#[derive(Debug, Clone)]
pub enum TabEvent {
    Started,
    Loaded {
        url: Url,
        title: String,
        content_height: f32,
        secure: bool,
        frame_titles: Vec<String>,
        /// The frames' URLs now (for the session history entry).
        frames: FrameState,
    },
    /// A link navigated a frame: a new session history entry with these frame URLs.
    FrameNavigated {
        frames: FrameState,
    },
    Favicon(bb_paint::ImageData),
    Frame(Frame),
    Hover {
        link: Option<String>,
        image: Option<String>,
        pointer: bool,
    },
    Clicked {
        link: Option<Url>,
        open: LinkOpen,
    },
    FindResult {
        total: usize,
        active: Option<usize>,
        rect: Option<bb_paint::Rect>,
    },
    /// No frame could scroll; the main page should.
    WheelUnconsumed {
        dy: f32,
    },
    /// An internal page asked the browser to go back (or home when there's no history).
    GoBack,
}

pub type Notify = Box<dyn Fn(u64, TabEvent) + Send>;

fn loaded_event(p: &Page) -> TabEvent {
    TabEvent::Loaded {
        url: p.url.clone(),
        title: p.title.clone(),
        content_height: p.content_height(),
        secure: p.secure,
        frame_titles: p.frames.iter().map(|f| f.page.title.clone()).collect(),
        frames: p.frame_urls(),
    }
}

/// Scheme of the links internal pages use to ask the browser to do something. They are never
/// fetched; see `browser_action`.
pub const BB_ACTION: &str = "bb-action";

/// Run a `bb-action:` link clicked in the current top-level page.
///   - `bb-action:go-back` — go back, or home when there's no history.
///   - `bb-action:accept-certificate?token=…` — on a certificate error page, trust that exact
///     certificate for this session and load the page again. The token must be this session's
///     secret (`bb_engine::session_token`), which only our error pages contain, and the page
///     must itself be the error page for the certificate being accepted.
fn browser_action(id: u64, p: &Page, client: &Client, url: &Url, notify: &Notify) {
    match url.pathname().as_str() {
        "go-back" => notify(id, TabEvent::GoBack),
        "accept-certificate" => {
            let token = url
                .query()
                .unwrap_or_default()
                .split('&')
                .find_map(|kv| kv.strip_prefix("token="))
                .unwrap_or_default()
                .to_string();
            if token != bb_engine::session_token() {
                log::warn!("tab {id}: ignoring certificate exception request without a valid token");
                return;
            }
            let Some(problem) = p.error.as_ref().and_then(|e| e.certificate.as_deref()) else {
                log::warn!(
                    "tab {id}: ignoring certificate exception request outside a certificate error page"
                );
                return;
            };
            client.add_certificate_exception(problem);
            log::warn!("user accepted the certificate for {} ({})", problem.host, problem.code);
            notify(id, TabEvent::Clicked { link: Some(p.url.clone()), open: LinkOpen::Current });
        }
        other => log::warn!("tab {id}: unknown browser action {other:?}"),
    }
}

pub fn spawn(id: u64, client: Arc<Client>, notify: Notify) -> Sender<TabCmd> {
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name(format!("tab-{id}"))
        .spawn(move || worker(id, client, rx, notify))
        .expect("spawn tab thread");
    tx
}

fn worker(id: u64, client: Arc<Client>, rx: Receiver<TabCmd>, notify: Notify) {
    let mut page: Option<Page> = None;
    let mut last_render: Option<(f32, f32, f32, f32, bool)> = None;
    let render = |page: &mut Page,
                  viewport: (f32, f32),
                  scale: f32,
                  scroll_y: f32,
                  dark: bool|
     -> Option<Frame> {
        page.set_dark(dark);
        page.set_viewport(viewport.0, viewport.1);
        let (w, h) = ((viewport.0 * scale).round() as u32, (viewport.1 * scale).round() as u32);
        if w == 0 || h == 0 {
            return None;
        }
        let max_scroll = (page.content_height() - viewport.1).max(0.0);
        let sy = scroll_y.clamp(0.0, max_scroll);
        let c = page.render(sy, scale, w, h);
        let internal = page.error.is_some() || page.url.scheme() == "about";
        let targets = if internal { page.element_rects_by_id() } else { Vec::new() };
        Some(Frame { pixels: Arc::new(c.pixels), w, h, scroll_y: sy, frames: page.frame_rects(), targets })
    };
    while let Ok(cmd) = rx.recv() {
        // Coalesce queued render requests: only the newest matters.
        let mut cmd = cmd;
        if matches!(cmd, TabCmd::Render { .. } | TabCmd::Hover { .. }) {
            while let Ok(next) = rx.try_recv() {
                let same = std::mem::discriminant(&next) == std::mem::discriminant(&cmd);
                if same {
                    cmd = next;
                } else {
                    // Process the pending one first, then the other.
                    handle(id, &client, &notify, &mut page, &mut last_render, cmd, &render);
                    cmd = next;
                }
            }
        }
        if matches!(cmd, TabCmd::Shutdown) {
            break;
        }
        handle(id, &client, &notify, &mut page, &mut last_render, cmd, &render);
    }
}

type RenderFn<'a> = &'a dyn Fn(&mut Page, (f32, f32), f32, f32, bool) -> Option<Frame>;

fn handle(
    id: u64,
    client: &Client,
    notify: &Notify,
    page: &mut Option<Page>,
    last_render: &mut Option<(f32, f32, f32, f32, bool)>,
    cmd: TabCmd,
    render: RenderFn,
) {
    match cmd {
        TabCmd::Navigate(url) => {
            notify(id, TabEvent::Started);
            let (vw, vh, scale, _, dark) = last_render.unwrap_or((1280.0, 720.0, 1.0, 0.0, false));
            let cfg = PageConfig { viewport: (vw, vh), prefers_dark: dark, ..Default::default() };
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Page::load(client, &url, cfg)));
            let mut p = match result {
                Ok(p) => p,
                Err(_) => {
                    let html = bb_engine::snag_page_html(&url.href(), "internal error while loading");
                    Page::from_html(client, &html, &url, cfg, 0, false)
                }
            };
            notify(id, loaded_event(&p));
            if let Some(f) = render(&mut p, (vw, vh), scale, 0.0, dark) {
                notify(id, TabEvent::Frame(f));
            }
            *last_render = Some((vw, vh, scale, 0.0, dark));
            let fav = p.favicon_url.clone();
            let page_url = p.url.clone();
            *page = Some(p);
            if let Some(fu) = fav {
                if let Some(img) = fetch_favicon(client, &fu, &page_url) {
                    notify(id, TabEvent::Favicon(img));
                }
            }
        }
        TabCmd::Render { viewport, scale, scroll_y, dark } => {
            *last_render = Some((viewport.0, viewport.1, scale, scroll_y, dark));
            if let Some(p) = page.as_mut() {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render(p, viewport, scale, scroll_y, dark)
                }));
                match r {
                    Ok(Some(f)) => notify(id, TabEvent::Frame(f)),
                    Ok(None) => {}
                    Err(_) => log::error!("tab {id}: render panicked"),
                }
            }
        }
        TabCmd::Hover { x, y } => {
            if let Some(p) = page.as_ref() {
                let link = p.link_at(x, y).map(|u| u.href());
                let image = p.image_at(x, y).map(|u| u.href());
                let pointer = link.is_some() || matches!(p.cursor_at(x, y), Some(bb_style::Cursor::Pointer));
                notify(id, TabEvent::Hover { link, image, pointer });
            }
        }
        TabCmd::Click { x, y, new_tab } => {
            let Some(p) = page.as_mut() else { return };
            let Some(hit) = p.link_hit_at(x, y) else {
                // Not a link: the click's activation behaviour (e.g. a <summary> toggles its
                // <details>), then a fresh frame if the page changed.
                if !new_tab && p.activate_at(x, y) {
                    if let Some((vw, vh, scale, sy, dark)) = *last_render {
                        if let Some(f) = render(p, (vw, vh), scale, sy, dark) {
                            notify(id, TabEvent::Frame(f));
                        }
                    }
                }
                notify(id, TabEvent::Clicked { link: None, open: LinkOpen::Current });
                return;
            };
            if hit.url.scheme() == BB_ACTION {
                // Only a top-level internal page's own links, opened in place, are actions.
                if !new_tab && hit.frame_path.is_empty() {
                    browser_action(id, p, client, &hit.url, notify);
                }
                return;
            }
            if new_tab {
                notify(id, TabEvent::Clicked { link: Some(hit.url), open: LinkOpen::BackgroundTab });
                return;
            }
            // Frame targets are resolved here; only top-level navigations go to the UI.
            let frame_target = !hit.frame_path.is_empty() || !hit.target.is_empty();
            if frame_target {
                notify(id, TabEvent::Started);
            }
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.follow_link(client, hit)));
            match r {
                Ok(bb_engine::LinkDisposition::Top(u)) => {
                    notify(id, TabEvent::Clicked { link: Some(u), open: LinkOpen::Current })
                }
                Ok(bb_engine::LinkDisposition::NewTab(u)) => {
                    notify(id, TabEvent::Clicked { link: Some(u), open: LinkOpen::ForegroundTab })
                }
                // A frame navigation is a new session history entry.
                Ok(bb_engine::LinkDisposition::Frame) => {
                    notify(id, TabEvent::FrameNavigated { frames: p.frame_urls() })
                }
                Err(_) => log::error!("tab {id}: frame navigation panicked"),
            }
            if frame_target {
                notify(id, loaded_event(p));
                if let Some((vw, vh, scale, sy, dark)) = *last_render {
                    if let Some(f) = render(p, (vw, vh), scale, sy, dark) {
                        notify(id, TabEvent::Frame(f));
                    }
                }
            }
        }
        TabCmd::Find { query, step, from_y } => {
            let Some(p) = page.as_mut() else { return };
            let (total, active) = if step == 0 || p.find_query != query {
                p.find(&query, from_y)
            } else {
                p.find_step(step > 0)
            };
            let rect = p.find_active_rect();
            if let Some((vw, vh, scale, sy, dark)) = *last_render {
                if let Some(f) = render(p, (vw, vh), scale, sy, dark) {
                    notify(id, TabEvent::Frame(f));
                }
            }
            notify(id, TabEvent::FindResult { total, active, rect });
        }
        TabCmd::RestoreFrames(state) => {
            let Some(p) = page.as_mut() else { return };
            notify(id, TabEvent::Started);
            let r =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.restore_frames(client, &state)));
            if r.is_err() {
                log::error!("tab {id}: restoring frames panicked");
            }
            notify(id, loaded_event(p));
            if let Some((vw, vh, scale, sy, dark)) = *last_render {
                if let Some(f) = render(p, (vw, vh), scale, sy, dark) {
                    notify(id, TabEvent::Frame(f));
                }
            }
        }
        TabCmd::FindClose => {
            let Some(p) = page.as_mut() else { return };
            p.find_clear();
            if let Some((vw, vh, scale, sy, dark)) = *last_render {
                if let Some(f) = render(p, (vw, vh), scale, sy, dark) {
                    notify(id, TabEvent::Frame(f));
                }
            }
        }
        TabCmd::Wheel { x, y, dy } => {
            let Some(p) = page.as_mut() else { return };
            if p.scroll_frame_at(x, y, dy) {
                if let Some((vw, vh, scale, sy, dark)) = *last_render {
                    if let Some(f) = render(p, (vw, vh), scale, sy, dark) {
                        notify(id, TabEvent::Frame(f));
                    }
                }
            } else {
                notify(id, TabEvent::WheelUnconsumed { dy });
            }
        }
        TabCmd::Shutdown => {}
    }
}

fn fetch_favicon(client: &Client, url: &Url, page: &Url) -> Option<bb_paint::ImageData> {
    let mut req = bb_net::Request::get(url.clone());
    req.timeout = std::time::Duration::from_secs(8);
    // A subresource of the page (cookies follow the page's site).
    req.cookie_context = bb_net::cookies::CookieContext {
        site_for_cookies: Some(page.clone()),
        top_level_navigation: false,
        ..Default::default()
    };
    let resp = client.fetch(req).ok().filter(|r| r.ok())?;
    let img = bb_image::decode(&resp.body).ok()?;
    Some(bb_paint::ImageData { width: img.width, height: img.height, pixels: img.frames[0].pixels.clone() })
}
