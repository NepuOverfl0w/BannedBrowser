//! The windowed browser: winit event loop, softbuffer presentation,
//! Proton-style chrome drawn with bb-ui, tabs backed by worker threads.

use crate::cli::{fixup_url, Args};
use crate::tab::{self, Frame, TabCmd, TabEvent};
use bb_paint::{Canvas, Color, DisplayList, ImageData, Item, Rect, SoftwareCanvas};
use bb_ui::icons::Icon;
use bb_ui::textfield::TextField;
use bb_ui::{Painter, Theme};
use bb_url::Url;
use crossbeam_channel::Sender;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Window, WindowId};

const TAB_STRIP_H: f32 = 40.0;
const TOOLBAR_H: f32 = 40.0;
const CHROME_H: f32 = TAB_STRIP_H + TOOLBAR_H;
const HOME: &str = "about:newtab";

#[derive(Debug)]
pub enum UserEvent {
    Tab { tab: u64, ev: TabEvent },
}

struct Tab {
    id: u64,
    tx: Sender<TabCmd>,
    title: String,
    url: Option<Url>,
    history: Vec<Url>,
    index: usize,
    loading: bool,
    load_started: Instant,
    scroll_y: f32,
    content_height: f32,
    frame: Option<Frame>,
    favicon: Option<ImageData>,
    secure: bool,
    hover_link: Option<String>,
    pointer: bool,
}

/// Which chrome element the mouse is over.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Target {
    None,
    Tab(usize),
    TabClose(usize),
    NewTab,
    Back,
    Forward,
    Reload,
    Home,
    UrlBar,
    Menu,
    Scrollbar,
    Content,
}

struct BrowserWindow {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    tabs: Vec<Tab>,
    active: usize,
    private: bool,
    theme: Theme,
    os_dark: bool,
    url: TextField,
    url_focused: bool,
    mouse: (f32, f32),
    hover: Target,
    pressed: Target,
    modifiers: ModifiersState,
    canvas: SoftwareCanvas,
    dirty: bool,
    /// Links to open in background tabs (Ctrl/Cmd+click, middle-click).
    pending_background: Vec<Url>,
    /// The next content click should open in a background tab.
    click_background: bool,
    /// When the page was last scrolled (scrollbar visibility).
    last_scroll: Instant,
    /// Dragging the scrollbar thumb: grab offset within the thumb (logical px).
    drag_thumb: Option<f32>,
}

struct ClosedTab {
    history: Vec<Url>,
    index: usize,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    client: Arc<bb_net::Client>,
    windows: HashMap<WindowId, BrowserWindow>,
    next_tab: u64,
    initial: Vec<Url>,
    initial_private: bool,
    window_size: (u32, u32),
    test_state: Option<std::path::PathBuf>,
    closed: Vec<ClosedTab>,
    context: Option<softbuffer::Context<Rc<Window>>>,
    /// `--dark` / `--light` override of the OS theme.
    force_dark: Option<bool>,
}

/// Cmd on macOS, Ctrl elsewhere.
fn primary(m: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        m.super_key()
    } else {
        m.control_key()
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let client = Arc::new(bb_engine::new_client(args.extra_ca.clone()));
    let mut initial: Vec<Url> =
        args.urls.iter().chain(args.new_tab.iter()).filter_map(|u| fixup_url(u)).collect();
    if initial.is_empty() {
        initial.push(Url::parse(HOME).expect("home URL"));
    }
    let mut app = App {
        proxy: event_loop.create_proxy(),
        client,
        windows: HashMap::new(),
        next_tab: 1,
        initial,
        initial_private: args.private_window,
        window_size: args.window_size.unwrap_or((1280, 800)),
        test_state: std::env::var_os("BB_TEST_STATE").map(Into::into),
        closed: Vec::new(),
        context: None,
        force_dark: args.dark,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

impl App {
    fn open_window(&mut self, el: &ActiveEventLoop, urls: Vec<Url>, private: bool) -> Option<WindowId> {
        let title = if private { "BannedBrowser Private Browsing" } else { "BannedBrowser" };
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(LogicalSize::new(self.window_size.0 as f64, self.window_size.1 as f64))
            .with_min_inner_size(LogicalSize::new(360.0, 240.0));
        let window = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                log::error!("cannot create window: {e}");
                return None;
            }
        };
        if self.context.is_none() {
            self.context = softbuffer::Context::new(window.clone()).ok();
        }
        let context = self.context.as_ref()?;
        let surface = softbuffer::Surface::new(context, window.clone()).ok()?;
        window.set_ime_allowed(true);
        let os_dark = self.force_dark.unwrap_or(matches!(window.theme(), Some(winit::window::Theme::Dark)));
        let theme = if private {
            Theme::private()
        } else if os_dark {
            Theme::dark()
        } else {
            Theme::light()
        };
        let id = window.id();
        let size = window.inner_size();
        let mut bw = BrowserWindow {
            window,
            surface,
            tabs: Vec::new(),
            active: 0,
            private,
            theme,
            os_dark,
            url: TextField::default(),
            url_focused: false,
            mouse: (0.0, 0.0),
            hover: Target::None,
            pressed: Target::None,
            modifiers: ModifiersState::empty(),
            canvas: SoftwareCanvas::new(size.width.max(1), size.height.max(1)),
            dirty: true,
            pending_background: Vec::new(),
            click_background: false,
            last_scroll: Instant::now() - Duration::from_secs(10),
            drag_thumb: None,
        };
        for u in urls {
            let t = self.new_tab(&mut bw, Some(u));
            bw.tabs.push(t);
        }
        if bw.tabs.is_empty() {
            let t = self.new_tab(&mut bw, Some(Url::parse(HOME).expect("home")));
            bw.tabs.push(t);
        }
        bw.active = 0;
        bw.sync_url_field();
        self.windows.insert(id, bw);
        self.write_state();
        Some(id)
    }

    fn new_tab(&mut self, bw: &mut BrowserWindow, url: Option<Url>) -> Tab {
        let id = self.next_tab;
        self.next_tab += 1;
        let proxy = self.proxy.clone();
        let tx = tab::spawn(
            id,
            self.client.clone(),
            Box::new(move |tab, ev| {
                let _ = proxy.send_event(UserEvent::Tab { tab, ev });
            }),
        );
        let mut t = Tab {
            id,
            tx,
            title: "New Tab".into(),
            url: None,
            history: Vec::new(),
            index: 0,
            loading: false,
            load_started: Instant::now(),
            scroll_y: 0.0,
            content_height: 0.0,
            frame: None,
            favicon: None,
            secure: false,
            hover_link: None,
            pointer: false,
        };
        let (vw, vh) = bw.viewport();
        let _ = t.tx.send(TabCmd::Render {
            viewport: (vw, vh),
            scale: bw.scale(),
            scroll_y: 0.0,
            dark: bw.page_dark(),
        });
        if let Some(u) = url {
            t.history.push(u.clone());
            t.index = 0;
            t.url = Some(u.clone());
            t.loading = true;
            let _ = t.tx.send(TabCmd::Navigate(u));
        }
        t
    }

    fn write_state(&self) {
        let Some(path) = &self.test_state else { return };
        // Pick the most recently focused (or any) window.
        let mut wins: Vec<&BrowserWindow> = self.windows.values().collect();
        wins.sort_by_key(|w| !w.window.has_focus());
        let tabs: usize = self.windows.values().map(|w| w.tabs.len()).sum();
        let mut v = serde_json::json!({
            "windows": self.windows.len(),
            "tabs": tabs,
            "bookmarks_count": 0,
            "open_panel": serde_json::Value::Null,
        });
        if let Some(w) = wins.first() {
            if let Some(t) = w.tabs.get(w.active) {
                v["active_tab"] = serde_json::json!(w.active);
                v["window_tabs"] = serde_json::json!(w.tabs.len());
                v["url"] = serde_json::json!(t.url.as_ref().map(|u| u.href()).unwrap_or_default());
                v["title"] = serde_json::json!(t.title);
                v["history_len"] = serde_json::json!(t.history.len());
                v["history_index"] = serde_json::json!(t.index);
                v["loading"] = serde_json::json!(t.loading);
                v["scroll_y"] = serde_json::json!(t.scroll_y.round());
                v["content_height"] = serde_json::json!(t.content_height.round());
                v["focused"] = serde_json::json!(if w.url_focused { "urlbar" } else { "content" });
                v["private"] = serde_json::json!(w.private);
                v["hover_link"] = serde_json::json!(t.hover_link);
                v["url_text"] = serde_json::json!(w.url.text);
                v["scrollbar_visible"] = serde_json::json!(w.scrollbar().is_some() && w.scrollbar_visible());
                v["scrollbar_dragging"] = serde_json::json!(w.drag_thumb.is_some());
            }
        }
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, v.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

impl BrowserWindow {
    fn scale(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    fn logical_size(&self) -> (f32, f32) {
        let s = self.window.inner_size();
        let k = self.scale();
        (s.width as f32 / k, s.height as f32 / k)
    }

    fn viewport(&self) -> (f32, f32) {
        let (w, h) = self.logical_size();
        (w.max(1.0), (h - CHROME_H).max(1.0))
    }

    fn page_dark(&self) -> bool {
        self.theme.dark
    }

    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    fn sync_url_field(&mut self) {
        let text = self.tab().and_then(|t| t.url.as_ref()).map(display_url).unwrap_or_default();
        self.url.set_text(&text);
    }

    fn request_render(&mut self) {
        let (vp, scale, dark) = (self.viewport(), self.scale(), self.page_dark());
        if let Some(t) = self.tab_mut() {
            let _ = t.tx.send(TabCmd::Render { viewport: vp, scale, scroll_y: t.scroll_y, dark });
        }
    }

    fn navigate(&mut self, url: Url) {
        if let Some(t) = self.tab_mut() {
            t.history.truncate(t.index + 1);
            if t.history.last() != Some(&url) || t.history.is_empty() {
                t.history.push(url.clone());
            }
            t.index = t.history.len() - 1;
            t.url = Some(url.clone());
            t.loading = true;
            t.load_started = Instant::now();
            t.scroll_y = 0.0;
            t.favicon = None;
            let _ = t.tx.send(TabCmd::Navigate(url));
        }
        self.url_focused = false;
        self.sync_url_field();
        self.dirty = true;
    }

    fn go_history(&mut self, delta: isize) {
        if let Some(t) = self.tab_mut() {
            let ni = t.index as isize + delta;
            if ni < 0 || ni as usize >= t.history.len() {
                return;
            }
            t.index = ni as usize;
            let url = t.history[t.index].clone();
            t.url = Some(url.clone());
            t.loading = true;
            t.load_started = Instant::now();
            t.scroll_y = 0.0;
            let _ = t.tx.send(TabCmd::Navigate(url));
        }
        self.sync_url_field();
        self.dirty = true;
    }

    fn reload(&mut self) {
        if let Some(t) = self.tab_mut() {
            if let Some(u) = t.url.clone() {
                t.loading = true;
                t.load_started = Instant::now();
                let _ = t.tx.send(TabCmd::Navigate(u));
            }
        }
        self.dirty = true;
    }

    fn scroll_by(&mut self, dy: f32) {
        let vh = self.viewport().1;
        if let Some(t) = self.tab_mut() {
            let max = (t.content_height - vh).max(0.0);
            let ny = (t.scroll_y + dy).clamp(0.0, max);
            if (ny - t.scroll_y).abs() > 0.1 {
                t.scroll_y = ny;
                self.last_scroll = Instant::now();
                self.request_render();
                self.dirty = true;
            }
        }
    }

    fn scroll_to(&mut self, y: f32) {
        let cur = self.tab().map(|t| t.scroll_y).unwrap_or(0.0);
        self.scroll_by(y - cur);
    }

    // ---------------- chrome geometry ----------------

    fn tab_rects(&self) -> Vec<Rect> {
        let (w, _) = self.logical_size();
        let n = self.tabs.len().max(1) as f32;
        let start = 8.0;
        let avail = (w - start - 48.0).max(80.0);
        let tw = (avail / n).clamp(76.0, 240.0);
        (0..self.tabs.len()).map(|i| Rect::new(start + i as f32 * (tw + 2.0), 4.0, tw, 32.0)).collect()
    }

    fn close_rect(tab: &Rect) -> Rect {
        Rect::new(tab.right() - 28.0, tab.y + 6.0, 20.0, 20.0)
    }

    fn new_tab_rect(&self) -> Rect {
        let rs = self.tab_rects();
        let x = rs.last().map(|r| r.right() + 4.0).unwrap_or(8.0);
        Rect::new(x, 6.0, 28.0, 28.0)
    }

    fn toolbar_button(&self, i: usize) -> Rect {
        Rect::new(8.0 + i as f32 * 36.0, TAB_STRIP_H + 4.0, 32.0, 32.0)
    }

    fn menu_rect(&self) -> Rect {
        let (w, _) = self.logical_size();
        Rect::new(w - 40.0, TAB_STRIP_H + 4.0, 32.0, 32.0)
    }

    fn url_rect(&self) -> Rect {
        let (w, _) = self.logical_size();
        let x = 8.0 + 4.0 * 36.0 + 8.0;
        Rect::new(x, TAB_STRIP_H + 4.0, (w - x - 56.0).max(60.0), 32.0)
    }

    /// Scrollbar track and thumb rects (logical px), if the page scrolls.
    fn scrollbar(&self) -> Option<(Rect, Rect)> {
        let t = self.tab()?;
        let c = self.content_rect();
        if t.content_height <= c.h + 0.5 || c.h <= 0.0 {
            return None;
        }
        let wide = matches!(self.hover, Target::Scrollbar) || self.drag_thumb.is_some();
        let w = if wide { 10.0 } else { 6.0 };
        let track = Rect::new(c.right() - w - 2.0, c.y + 2.0, w, c.h - 4.0);
        let th = (track.h * c.h / t.content_height).max(24.0).min(track.h);
        let max = (t.content_height - c.h).max(1.0);
        let ty = track.y + (track.h - th) * (t.scroll_y / max).clamp(0.0, 1.0);
        Some((track, Rect::new(track.x, ty, w, th)))
    }

    fn scrollbar_visible(&self) -> bool {
        self.drag_thumb.is_some()
            || matches!(self.hover, Target::Scrollbar)
            || self.last_scroll.elapsed() < Duration::from_millis(1200)
    }

    fn content_rect(&self) -> Rect {
        let (w, h) = self.logical_size();
        Rect::new(0.0, CHROME_H, w, (h - CHROME_H).max(0.0))
    }

    fn target_at(&self, x: f32, y: f32) -> Target {
        if y >= CHROME_H {
            if let Some((track, _)) = self.scrollbar() {
                if x >= track.x - 6.0 {
                    return Target::Scrollbar;
                }
            }
            return Target::Content;
        }
        for (i, r) in self.tab_rects().iter().enumerate() {
            if r.contains(x, y) {
                if Self::close_rect(r).contains(x, y)
                    && (i == self.active || self.hover == Target::Tab(i) || self.hover == Target::TabClose(i))
                {
                    return Target::TabClose(i);
                }
                return Target::Tab(i);
            }
        }
        if self.new_tab_rect().contains(x, y) {
            return Target::NewTab;
        }
        let buttons = [Target::Back, Target::Forward, Target::Reload, Target::Home];
        for (i, t) in buttons.iter().enumerate() {
            if self.toolbar_button(i).contains(x, y) {
                return *t;
            }
        }
        if self.url_rect().contains(x, y) {
            return Target::UrlBar;
        }
        if self.menu_rect().contains(x, y) {
            return Target::Menu;
        }
        Target::None
    }

    // ---------------- painting ----------------

    fn chrome_display_list(&self) -> DisplayList {
        let th = &self.theme;
        let (w, _) = self.logical_size();
        let mut dl = DisplayList::default();
        let mut p = Painter::new(&mut dl);
        p.rect(Rect::new(0.0, 0.0, w, TAB_STRIP_H), th.frame);
        // Tabs.
        let rects = self.tab_rects();
        for (i, r) in rects.iter().enumerate() {
            let t = &self.tabs[i];
            let selected = i == self.active;
            let hovered = matches!(self.hover, Target::Tab(j) | Target::TabClose(j) if j == i);
            if selected {
                p.shadow(*r, 6.0, 4.0, th.shadow.with_alpha(0.35));
                p.rounded(*r, 6.0, th.toolbar);
                // Brand accent line on the selected tab.
                p.rounded(Rect::new(r.x + 8.0, r.y, r.w - 16.0, 2.0), 1.0, th.accent);
            } else if hovered {
                p.rounded(*r, 6.0, th.tab_hover);
            }
            // Favicon / throbber.
            let ix = r.x + 10.0;
            let iy = r.y + 8.0;
            if t.loading {
                let phase = t.load_started.elapsed().as_secs_f32() * 6.0;
                let mut pts = Vec::new();
                for k in 0..=16 {
                    let a = phase + k as f32 / 16.0 * 4.2;
                    pts.push(bb_paint::Point::new(ix + 8.0 + 6.0 * a.cos(), iy + 8.0 + 6.0 * a.sin()));
                }
                let mut path = bb_paint::Path::new();
                for (k, pt) in pts.iter().enumerate() {
                    if k == 0 {
                        path.move_to(pt.x, pt.y);
                    } else {
                        path.line_to(pt.x, pt.y);
                    }
                }
                p.path(path.stroke(2.0), th.accent);
            } else if let Some(f) = &t.favicon {
                let dst = Rect::new(ix, iy, 16.0, 16.0);
                p.dl.push(Item::Image { image: f.clone(), dst, clip: dst, opacity: 1.0, smooth: true });
            } else {
                p.path(bb_ui::icons::path(Icon::Globe, ix, iy, 16.0), th.text_muted);
            }
            let title = if t.title.trim().is_empty() {
                t.url.as_ref().map(display_url).unwrap_or_else(|| "New Tab".into())
            } else {
                t.title.clone()
            };
            let bg = if selected { th.toolbar } else { th.frame };
            let show_close = selected || hovered;
            let max_w = r.w - 34.0 - if show_close { 28.0 } else { 8.0 };
            let base = bb_ui::text::centered_baseline(r.y, r.h, 12.5);
            p.text_faded(
                &title,
                ix + 24.0,
                base,
                12.5,
                max_w.max(0.0),
                th.text,
                if hovered && !selected { th.frame } else { bg },
            );
            if show_close {
                let cr = Self::close_rect(r);
                if self.hover == Target::TabClose(i) {
                    p.rounded(cr, 4.0, th.button_hover);
                }
                p.path(bb_ui::icons::path(Icon::Close, cr.x + 4.0, cr.y + 4.0, 12.0), th.text);
            }
        }
        let nt = self.new_tab_rect();
        p.icon_button(nt, Icon::Plus, th, self.hover == Target::NewTab, self.pressed == Target::NewTab, true);
        if self.private {
            p.path(bb_ui::icons::path(Icon::Mask, w - 34.0, 12.0, 18.0), th.accent);
        }
        // Toolbar.
        p.rect(Rect::new(0.0, TAB_STRIP_H, w, TOOLBAR_H), th.toolbar);
        let t = self.tab();
        let can_back = t.is_some_and(|t| t.index > 0);
        let can_fwd = t.is_some_and(|t| t.index + 1 < t.history.len());
        let loading = t.is_some_and(|t| t.loading);
        let btns = [
            (Target::Back, Icon::Back, can_back),
            (Target::Forward, Icon::Forward, can_fwd),
            (Target::Reload, if loading { Icon::Stop } else { Icon::Reload }, true),
            (Target::Home, Icon::Home, true),
        ];
        for (i, (tg, icon, en)) in btns.iter().enumerate() {
            p.icon_button(self.toolbar_button(i), *icon, th, self.hover == *tg, self.pressed == *tg, *en);
        }
        // URL bar.
        let ur = self.url_rect();
        if self.url_focused {
            p.rounded(
                Rect::new(ur.x - 2.0, ur.y - 2.0, ur.w + 4.0, ur.h + 4.0),
                6.0,
                th.accent.with_alpha(0.55),
            );
        }
        p.rounded(ur, 4.0, th.field);
        if !self.url_focused {
            p.dl.push(Item::Border {
                rect: ur,
                radii: bb_paint::Radii::uniform(4.0),
                sides: [Some(bb_paint::BorderSide {
                    width: 1.0,
                    color: th.field_border,
                    style: bb_paint::BorderStyle::Solid,
                }); 4],
            });
        }
        let secure = t.is_some_and(|t| t.secure);
        let is_https = t.and_then(|t| t.url.as_ref()).is_some_and(|u| u.scheme() == "https");
        let icon = if is_https && secure {
            Icon::Lock
        } else if t.and_then(|t| t.url.as_ref()).is_some_and(|u| u.scheme() == "http") {
            Icon::LockBroken
        } else {
            Icon::Info
        };
        p.path(bb_ui::icons::path(icon, ur.x + 10.0, ur.y + 8.0, 16.0), th.text_muted);
        let tx = ur.x + 34.0;
        let size = 14.0;
        let base = bb_ui::text::centered_baseline(ur.y, ur.h, size);
        p.clip(Rect::new(tx, ur.y, ur.w - 44.0, ur.h));
        if self.url_focused {
            let b = bb_ui::text::boundaries(&self.url.text, size);
            let xof = |i: usize| b.iter().find(|(j, _)| *j >= i).map(|(_, x)| *x).unwrap_or(0.0);
            let caret_x = xof(self.url.caret);
            let scroll = (caret_x - (ur.w - 60.0)).max(0.0);
            if let Some((a, bb)) = self.url.selection() {
                p.rect(
                    Rect::new(tx + xof(a) - scroll, ur.y + 7.0, xof(bb) - xof(a), ur.h - 14.0),
                    th.accent.with_alpha(0.35),
                );
            }
            p.text(&self.url.text, tx - scroll, base, size, 400, th.text);
            if !self.url.preedit.is_empty() {
                let pw = p.text(&self.url.preedit, tx + caret_x - scroll, base, size, 400, th.accent);
                p.rect(Rect::new(tx + caret_x - scroll, base + 2.0, pw, 1.0), th.accent);
            }
            p.rect(Rect::new(tx + caret_x - scroll, ur.y + 8.0, 1.5, ur.h - 16.0), th.text);
        } else if self.url.text.is_empty() {
            p.text("Search or enter address", tx, base, size, 400, th.text_muted);
        } else {
            // Emphasize the host.
            draw_url_with_host(&mut p, &self.url.text, tx, base, size, th);
        }
        p.unclip();
        p.icon_button(
            self.menu_rect(),
            Icon::Menu,
            th,
            self.hover == Target::Menu,
            self.pressed == Target::Menu,
            true,
        );
        // Loading progress line.
        if loading {
            let e = t.map(|t| t.load_started.elapsed().as_secs_f32()).unwrap_or(0.0);
            let frac = 1.0 - (-e / 1.5).exp();
            p.rect(Rect::new(0.0, CHROME_H - 2.0, w * frac * 0.9, 2.0), th.glitch);
        }
        p.rect(Rect::new(0.0, CHROME_H - 1.0, w, 1.0), th.separator);
        // Overlay scrollbar.
        if self.scrollbar_visible() {
            if let Some((track, thumb)) = self.scrollbar() {
                if matches!(self.hover, Target::Scrollbar) || self.drag_thumb.is_some() {
                    p.rounded(track, track.w / 2.0, th.text.with_alpha(0.08));
                }
                let c = if self.drag_thumb.is_some() { th.accent } else { th.text.with_alpha(0.5) };
                // A halo in the toolbar color keeps the thumb visible on any page color.
                let halo = Rect::new(thumb.x - 1.0, thumb.y - 1.0, thumb.w + 2.0, thumb.h + 2.0);
                p.rounded(halo, halo.w / 2.0, th.toolbar.with_alpha(0.6));
                p.rounded(thumb, thumb.w / 2.0, c);
            }
        }
        // Status panel (hovered link).
        if let Some(link) = t.and_then(|t| t.hover_link.clone()) {
            let (_, h) = self.logical_size();
            let tw = bb_ui::text::measure(&link, 12.0, 400).min(w * 0.6);
            let r = Rect::new(2.0, h - 26.0, tw + 16.0, 24.0);
            p.shadow(r, 4.0, 4.0, th.shadow.with_alpha(0.3));
            p.rounded(r, 4.0, th.popup);
            let base = bb_ui::text::centered_baseline(r.y, r.h, 12.0);
            p.text_faded(&link, r.x + 8.0, base, 12.0, tw, th.text, th.popup);
        }
        dl
    }

    fn redraw(&mut self) {
        let size = self.window.inner_size();
        let (pw, ph) = (size.width.max(1), size.height.max(1));
        if self.canvas.w != pw || self.canvas.h != ph {
            self.canvas = SoftwareCanvas::new(pw, ph);
        }
        let scale = self.scale();
        self.canvas.clear(self.theme.toolbar);
        // Content frame (shifted for scroll offsets not yet re-rendered).
        let content = self.content_rect().scale(scale);
        if let Some(t) = self.tabs.get(self.active) {
            if let Some(f) = &t.frame {
                let dy = ((f.scroll_y - t.scroll_y) * scale).round() as i32;
                blit(&mut self.canvas, f, content.x as i32, content.y as i32 + dy, content);
            } else {
                self.canvas
                    .fill_rect(content, if self.theme.dark { Color::hex(0x1C1B22) } else { Color::WHITE });
            }
        }
        let dl = self.chrome_display_list();
        bb_paint::render(&dl, &mut self.canvas, (0.0, 0.0), scale);
        if self
            .surface
            .resize(
                NonZeroU32::new(pw).unwrap_or(NonZeroU32::MIN),
                NonZeroU32::new(ph).unwrap_or(NonZeroU32::MIN),
            )
            .is_err()
        {
            return;
        }
        if let Ok(mut buf) = self.surface.buffer_mut() {
            if buf.len() == (pw * ph) as usize {
                self.canvas.to_0rgb(&mut buf);
                let _ = buf.present();
            }
        }
        self.dirty = false;
    }

    fn update_title(&self) {
        let suffix = if self.private { "BannedBrowser Private Browsing" } else { "BannedBrowser" };
        let t = self.tab().map(|t| t.title.trim().to_string()).unwrap_or_default();
        let title = if t.is_empty() { suffix.to_string() } else { format!("{t} — {suffix}") };
        self.window.set_title(&title);
    }
}

fn display_url(u: &Url) -> String {
    let s = u.href();
    if s == "about:newtab" || s == "about:home" {
        String::new()
    } else {
        s
    }
}

fn draw_url_with_host(p: &mut Painter, text: &str, x: f32, base: f32, size: f32, th: &Theme) {
    if let Ok(u) = Url::parse(text) {
        let host = u.host_with_port();
        if !host.is_empty() {
            if let Some(i) = text.find(&host) {
                let pre = &text[..i];
                let post = &text[i + host.len()..];
                let mut cx = x;
                cx += p.text(pre, cx, base, size, 400, th.text_muted);
                cx += p.text(&host, cx, base, size, 400, th.text);
                p.text(post, cx, base, size, 400, th.text_muted);
                return;
            }
        }
    }
    p.text(text, x, base, size, 400, th.text);
}

/// Copy a frame's pixels into the canvas at (x, y), clipped to `clip`.
fn blit(c: &mut SoftwareCanvas, f: &Frame, x: i32, y: i32, clip: Rect) {
    let (cw, ch) = (c.w as i32, c.h as i32);
    let (cx0, cy0, cx1, cy1) = (clip.x as i32, clip.y as i32, clip.right() as i32, clip.bottom() as i32);
    for row in 0..f.h as i32 {
        let dy = y + row;
        if dy < cy0.max(0) || dy >= cy1.min(ch) {
            continue;
        }
        let x0 = x.max(cx0).max(0);
        let x1 = (x + f.w as i32).min(cx1).min(cw);
        if x0 >= x1 {
            continue;
        }
        let src = ((row * f.w as i32 + (x0 - x)) * 4) as usize;
        let dst = ((dy * cw + x0) * 4) as usize;
        let n = ((x1 - x0) * 4) as usize;
        c.pixels[dst..dst + n].copy_from_slice(&f.pixels[src..src + n]);
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.windows.is_empty() {
            let urls = std::mem::take(&mut self.initial);
            let private = self.initial_private;
            self.open_window(el, urls, private);
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: UserEvent) {
        let UserEvent::Tab { tab, ev } = event;
        let mut state_changed = false;
        for bw in self.windows.values_mut() {
            let Some(ti) = bw.tabs.iter().position(|t| t.id == tab) else { continue };
            let is_active = ti == bw.active;
            match ev {
                TabEvent::Started => {
                    let t = &mut bw.tabs[ti];
                    t.loading = true;
                    t.load_started = Instant::now();
                    t.hover_link = None;
                    state_changed = true;
                }
                TabEvent::Loaded { url, title, content_height, secure } => {
                    let t = &mut bw.tabs[ti];
                    t.loading = false;
                    t.title = title;
                    t.content_height = content_height;
                    t.secure = secure;
                    // Redirects: replace the current history entry.
                    if t.url.as_ref() != Some(&url) {
                        if let Some(h) = t.history.get_mut(t.index) {
                            *h = url.clone();
                        }
                        t.url = Some(url);
                    }
                    if is_active {
                        if !bw.url_focused {
                            bw.sync_url_field();
                        }
                        bw.update_title();
                    }
                    state_changed = true;
                }
                TabEvent::Favicon(img) => bw.tabs[ti].favicon = Some(img),
                TabEvent::Frame(f) => {
                    let t = &mut bw.tabs[ti];
                    t.frame = Some(f);
                }
                TabEvent::Hover { link, pointer } => {
                    let t = &mut bw.tabs[ti];
                    if t.hover_link != link {
                        state_changed = true;
                    }
                    t.hover_link = link;
                    t.pointer = pointer;
                    if is_active {
                        bw.window.set_cursor(if pointer { CursorIcon::Pointer } else { CursorIcon::Default });
                    }
                }
                TabEvent::Clicked { link } => {
                    if let Some(u) = link {
                        if is_active {
                            if std::mem::take(&mut bw.click_background) || primary(bw.modifiers) {
                                bw.pending_background.push(u);
                            } else {
                                bw.navigate(u);
                            }
                            state_changed = true;
                        }
                    }
                }
            }
            bw.dirty = true;
            bw.window.request_redraw();
            break;
        }
        // Open any background tabs requested by Ctrl+click.
        let ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for id in ids {
            let pending = self
                .windows
                .get_mut(&id)
                .map(|w| std::mem::take(&mut w.pending_background))
                .unwrap_or_default();
            for u in pending {
                if let Some(mut bw) = self.windows.remove(&id) {
                    let t = self.new_tab(&mut bw, Some(u));
                    bw.tabs.insert(bw.active + 1, t);
                    bw.dirty = true;
                    self.windows.insert(id, bw);
                }
            }
        }
        if state_changed {
            self.write_state();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(mut bw) = self.windows.remove(&id) else { return };
        let mut keep = true;
        let mut state_changed = false;
        match event {
            WindowEvent::CloseRequested => keep = false,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                bw.request_render();
                bw.dirty = true;
                state_changed = true;
            }
            WindowEvent::ThemeChanged(t) => {
                bw.os_dark = self.force_dark.unwrap_or(t == winit::window::Theme::Dark);
                if !bw.private {
                    bw.theme = if bw.os_dark { Theme::dark() } else { Theme::light() };
                }
                bw.request_render();
                bw.dirty = true;
            }
            WindowEvent::RedrawRequested => {
                bw.redraw();
            }
            WindowEvent::ModifiersChanged(m) => bw.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let k = bw.scale();
                let (x, y) = (position.x as f32 / k, position.y as f32 / k);
                bw.mouse = (x, y);
                if let Some(grab) = bw.drag_thumb {
                    if let (Some((track, thumb)), Some(t)) = (bw.scrollbar(), bw.tab()) {
                        let max = (t.content_height - bw.content_rect().h).max(0.0);
                        let frac = ((y - grab - track.y) / (track.h - thumb.h).max(1.0)).clamp(0.0, 1.0);
                        bw.scroll_to(frac * max);
                    }
                    bw.window.request_redraw();
                    self.windows.insert(id, bw);
                    return;
                }
                let tgt = bw.target_at(x, y);
                if tgt != bw.hover {
                    bw.hover = tgt;
                    bw.dirty = true;
                }
                if tgt == Target::Content {
                    let sy = bw.tab().map(|t| t.scroll_y).unwrap_or(0.0);
                    if let Some(t) = bw.tab() {
                        let _ = t.tx.send(TabCmd::Hover { x, y: y - CHROME_H + sy });
                    }
                } else {
                    let cur = if tgt == Target::UrlBar { CursorIcon::Text } else { CursorIcon::Default };
                    bw.window.set_cursor(cur);
                    if let Some(t) = bw.tab_mut() {
                        if t.hover_link.take().is_some() {
                            bw.dirty = true;
                        }
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                bw.hover = Target::None;
                bw.dirty = true;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = bw.mouse;
                let tgt = bw.target_at(x, y);
                match (state, button) {
                    (ElementState::Pressed, MouseButton::Left) if tgt == Target::Scrollbar => {
                        if let Some((_, thumb)) = bw.scrollbar() {
                            if y >= thumb.y && y < thumb.bottom() {
                                bw.drag_thumb = Some(y - thumb.y);
                            } else {
                                let page = bw.viewport().1 * 0.85;
                                bw.scroll_by(if y < thumb.y { -page } else { page });
                            }
                        }
                        bw.dirty = true;
                        state_changed = true;
                    }
                    (ElementState::Released, MouseButton::Left) if bw.drag_thumb.is_some() => {
                        bw.drag_thumb = None;
                        bw.last_scroll = Instant::now();
                        bw.dirty = true;
                        state_changed = true;
                    }
                    (ElementState::Pressed, MouseButton::Left) => {
                        bw.pressed = tgt;
                        if tgt == Target::UrlBar {
                            if !bw.url_focused {
                                bw.url_focused = true;
                                bw.url.select_all();
                            } else {
                                let ur = bw.url_rect();
                                let b = bb_ui::text::boundaries(&bw.url.text, 14.0);
                                let lx = x - ur.x - 34.0;
                                let at = b
                                    .iter()
                                    .min_by(|a, c| (a.1 - lx).abs().total_cmp(&(c.1 - lx).abs()))
                                    .map(|p| p.0)
                                    .unwrap_or(0);
                                let extend = bw.modifiers.shift_key();
                                bw.url.move_caret(at, extend);
                            }
                            state_changed = true;
                        } else if bw.url_focused {
                            bw.url_focused = false;
                            bw.sync_url_field();
                            state_changed = true;
                        }
                        bw.dirty = true;
                    }
                    (ElementState::Released, MouseButton::Left) => {
                        let pressed = std::mem::replace(&mut bw.pressed, Target::None);
                        if pressed == tgt {
                            state_changed |= self.activate(&mut bw, el, tgt);
                        }
                        bw.dirty = true;
                    }
                    (ElementState::Released, MouseButton::Middle) => {
                        if let Target::Tab(i) | Target::TabClose(i) = tgt {
                            self.close_tab(&mut bw, i);
                            state_changed = true;
                        } else if tgt == Target::Content {
                            let sy = bw.tab().map(|t| t.scroll_y).unwrap_or(0.0);
                            if let Some(t) = bw.tab() {
                                let _ = t.tx.send(TabCmd::Click { x, y: y - CHROME_H + sy });
                            }
                            bw.click_background = true;
                        }
                        bw.dirty = true;
                    }
                    (ElementState::Released, MouseButton::Back) => bw.go_history(-1),
                    (ElementState::Released, MouseButton::Forward) => bw.go_history(1),
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 48.0,
                    MouseScrollDelta::PixelDelta(PhysicalPosition { y, .. }) => -(y as f32) / bw.scale(),
                };
                bw.scroll_by(dy);
                state_changed = true;
            }
            WindowEvent::Ime(ime) => {
                if bw.url_focused {
                    match ime {
                        Ime::Preedit(s, _) => bw.url.preedit = s,
                        Ime::Commit(s) => {
                            bw.url.preedit.clear();
                            bw.url.insert(&s);
                        }
                        _ => {}
                    }
                    bw.dirty = true;
                }
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. }
                if event.state == ElementState::Pressed =>
            {
                state_changed |= self.key(&mut bw, el, &event.logical_key, event.text.as_deref());
                bw.dirty = true;
            }
            _ => {}
        }
        if keep && !bw.tabs.is_empty() {
            if bw.dirty {
                bw.window.request_redraw();
            }
            self.windows.insert(id, bw);
        } else {
            for t in &bw.tabs {
                let _ = t.tx.send(TabCmd::Shutdown);
            }
            if self.windows.is_empty() {
                el.exit();
            }
            state_changed = true;
        }
        if state_changed {
            self.write_state();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // Animate only while something is loading (idle = 0% CPU).
        let mut animating = false;
        let mut wake: Option<Instant> = None;
        let now = Instant::now();
        for bw in self.windows.values() {
            if bw.tabs.iter().any(|t| t.loading) {
                animating = true;
                bw.window.request_redraw();
            }
            // Hide the overlay scrollbar once it goes idle.
            let hide_at = bw.last_scroll + Duration::from_millis(1250);
            if hide_at > now {
                wake = Some(wake.map_or(hide_at, |w| w.min(hide_at)));
            } else if hide_at + Duration::from_millis(300) > now {
                bw.window.request_redraw();
            }
        }
        el.set_control_flow(if animating {
            ControlFlow::WaitUntil(now + Duration::from_millis(33))
        } else if let Some(w) = wake {
            ControlFlow::WaitUntil(w + Duration::from_millis(60))
        } else {
            ControlFlow::Wait
        });
    }
}

impl App {
    /// Handle a click on a chrome target. Returns whether state changed.
    fn activate(&mut self, bw: &mut BrowserWindow, el: &ActiveEventLoop, tgt: Target) -> bool {
        match tgt {
            Target::Tab(i) => {
                if bw.active != i {
                    bw.active = i;
                    bw.sync_url_field();
                    bw.request_render();
                    bw.update_title();
                }
                true
            }
            Target::TabClose(i) => {
                self.close_tab(bw, i);
                true
            }
            Target::NewTab => {
                self.open_tab(bw, Url::parse(HOME).ok());
                true
            }
            Target::Back => {
                bw.go_history(-1);
                true
            }
            Target::Forward => {
                bw.go_history(1);
                true
            }
            Target::Reload => {
                bw.reload();
                true
            }
            Target::Home => {
                if let Ok(u) = Url::parse(HOME) {
                    bw.navigate(u);
                }
                true
            }
            Target::Menu => {
                // TODO(M6): ≡ app menu panel. For now: open a new window.
                let _ = el;
                false
            }
            Target::Content => {
                let (x, y) = bw.mouse;
                let sy = bw.tab().map(|t| t.scroll_y).unwrap_or(0.0);
                if let Some(t) = bw.tab() {
                    let _ = t.tx.send(TabCmd::Click { x, y: y - CHROME_H + sy });
                }
                false
            }
            _ => false,
        }
    }

    fn open_tab(&mut self, bw: &mut BrowserWindow, url: Option<Url>) {
        let t = self.new_tab(bw, url.clone());
        bw.tabs.push(t);
        bw.active = bw.tabs.len() - 1;
        bw.sync_url_field();
        bw.url_focused = url.as_ref().is_none_or(|u| u.href() == HOME);
        if bw.url_focused {
            bw.url.select_all();
        }
        bw.update_title();
    }

    fn close_tab(&mut self, bw: &mut BrowserWindow, i: usize) {
        if i >= bw.tabs.len() {
            return;
        }
        let t = bw.tabs.remove(i);
        let _ = t.tx.send(TabCmd::Shutdown);
        if !t.history.is_empty() && !bw.private {
            self.closed.push(ClosedTab { history: t.history, index: t.index });
        }
        if bw.tabs.is_empty() {
            return; // window closes
        }
        if bw.active >= bw.tabs.len() || bw.active > i {
            bw.active = bw.active.saturating_sub(1);
        }
        bw.active = bw.active.min(bw.tabs.len() - 1);
        bw.sync_url_field();
        bw.request_render();
        bw.update_title();
    }

    fn reopen_tab(&mut self, bw: &mut BrowserWindow) {
        if let Some(c) = self.closed.pop() {
            let url = c.history.get(c.index).cloned();
            let mut t = self.new_tab(bw, url);
            t.history = c.history;
            t.index = c.index;
            bw.tabs.push(t);
            bw.active = bw.tabs.len() - 1;
            bw.sync_url_field();
            bw.update_title();
        }
    }

    /// Keyboard handling. Returns whether state changed.
    fn key(&mut self, bw: &mut BrowserWindow, el: &ActiveEventLoop, key: &Key, text: Option<&str>) -> bool {
        let m = bw.modifiers;
        let p = primary(m);
        let shift = m.shift_key();
        let alt = m.alt_key();
        let ch = |s: &str| matches!(key, Key::Character(c) if c.eq_ignore_ascii_case(s));
        // Global shortcuts (CLAUDE.md §3.4).
        if (p && ch("l")) || (alt && ch("d")) || matches!(key, Key::Named(NamedKey::F6)) {
            bw.url_focused = true;
            bw.url.select_all();
            return true;
        }
        if p && ch("t") && !shift {
            self.open_tab(bw, Url::parse(HOME).ok());
            return true;
        }
        if p && shift && ch("t") {
            self.reopen_tab(bw);
            return true;
        }
        if (p && ch("w") && !shift) || (p && matches!(key, Key::Named(NamedKey::F4))) {
            let a = bw.active;
            self.close_tab(bw, a);
            return true;
        }
        if p && ch("n") && !shift {
            self.open_window(el, vec![Url::parse(HOME).expect("home")], false);
            return true;
        }
        if p && shift && ch("p") {
            self.open_window(el, vec![Url::parse(HOME).expect("home")], true);
            return true;
        }
        if p && ch("q") {
            el.exit();
            return true;
        }
        if p && matches!(key, Key::Named(NamedKey::Tab)) {
            let n = bw.tabs.len();
            if n > 1 {
                bw.active = if shift { (bw.active + n - 1) % n } else { (bw.active + 1) % n };
                bw.sync_url_field();
                bw.request_render();
                bw.update_title();
            }
            return true;
        }
        if (p || (alt && !cfg!(target_os = "macos"))) && !shift {
            if let Key::Character(c) = key {
                if let Some(d) = c.chars().next().and_then(|c| c.to_digit(10)) {
                    if (1..=9).contains(&d) && (p || alt) {
                        let n = bw.tabs.len();
                        bw.active = if d == 9 { n - 1 } else { (d as usize - 1).min(n - 1) };
                        bw.sync_url_field();
                        bw.request_render();
                        bw.update_title();
                        return true;
                    }
                }
            }
        }
        if (p && ch("r")) || matches!(key, Key::Named(NamedKey::F5)) {
            bw.reload();
            return true;
        }
        if (alt && matches!(key, Key::Named(NamedKey::ArrowLeft)))
            || (cfg!(target_os = "macos") && p && ch("["))
        {
            bw.go_history(-1);
            return true;
        }
        if (alt && matches!(key, Key::Named(NamedKey::ArrowRight)))
            || (cfg!(target_os = "macos") && p && ch("]"))
        {
            bw.go_history(1);
            return true;
        }
        if alt && matches!(key, Key::Named(NamedKey::Home)) {
            if let Ok(u) = Url::parse(HOME) {
                bw.navigate(u);
            }
            return true;
        }
        if bw.url_focused {
            return self.url_key(bw, key, text);
        }
        // Page scrolling.
        let vh = bw.viewport().1;
        match key {
            Key::Named(NamedKey::ArrowDown) => bw.scroll_by(48.0),
            Key::Named(NamedKey::ArrowUp) => bw.scroll_by(-48.0),
            Key::Named(NamedKey::PageDown) => bw.scroll_by(vh * 0.85),
            Key::Named(NamedKey::PageUp) => bw.scroll_by(-vh * 0.85),
            Key::Named(NamedKey::Space) => bw.scroll_by(if shift { -vh * 0.85 } else { vh * 0.85 }),
            Key::Named(NamedKey::Home) => bw.scroll_to(0.0),
            Key::Named(NamedKey::End) => bw.scroll_to(f32::MAX / 4.0),
            Key::Named(NamedKey::Escape) => {
                if let Some(t) = bw.tab_mut() {
                    t.loading = false;
                }
            }
            _ => return false,
        }
        true
    }

    fn url_key(&mut self, bw: &mut BrowserWindow, key: &Key, text: Option<&str>) -> bool {
        let m = bw.modifiers;
        let p = primary(m);
        let shift = m.shift_key();
        let word = if cfg!(target_os = "macos") { m.alt_key() } else { m.control_key() };
        match key {
            Key::Named(NamedKey::Enter) => {
                let input = bw.url.text.clone();
                if let Some(u) = fixup_url(&input) {
                    bw.navigate(u);
                }
                return true;
            }
            Key::Named(NamedKey::Escape) => {
                bw.url_focused = false;
                bw.sync_url_field();
                return true;
            }
            Key::Named(NamedKey::Backspace) => bw.url.backspace(),
            Key::Named(NamedKey::Delete) => bw.url.delete(),
            Key::Named(NamedKey::ArrowLeft) => bw.url.left(word, shift),
            Key::Named(NamedKey::ArrowRight) => bw.url.right(word, shift),
            Key::Named(NamedKey::Home) => bw.url.home(shift),
            Key::Named(NamedKey::End) => bw.url.end(shift),
            Key::Character(c) if p && c.eq_ignore_ascii_case("a") => bw.url.select_all(),
            Key::Character(c) if p && c.eq_ignore_ascii_case("z") => {
                if shift {
                    bw.url.redo()
                } else {
                    bw.url.undo()
                }
            }
            Key::Character(c) if p && (c.eq_ignore_ascii_case("c") || c.eq_ignore_ascii_case("x")) => {
                if let Some(sel) = bw.url.selected_text().map(str::to_string) {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(sel);
                    }
                    if c.eq_ignore_ascii_case("x") {
                        bw.url.backspace();
                    }
                }
            }
            Key::Character(c) if p && c.eq_ignore_ascii_case("v") => {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if let Ok(t) = cb.get_text() {
                        bw.url.insert(&t.replace(['\n', '\r'], " "));
                    }
                }
            }
            _ => {
                if !p {
                    if let Some(t) = text {
                        bw.url.insert(t);
                    }
                }
            }
        }
        false
    }
}
