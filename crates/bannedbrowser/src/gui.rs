//! The windowed browser: winit event loop, softbuffer presentation,
//! Proton-style chrome drawn with bb-ui, tabs backed by worker threads.

use crate::cli::{fixup_url, Args};
use crate::menu::{primary_label, Action, Entry, Menu};
use crate::tab::{self, Frame, HistoryEntry, LinkOpen, TabCmd, TabEvent};
use bb_engine::{FocusKind, KeyMods, NavRequest, PageKey, SelectPopup};
use bb_net::cache::CacheMode;
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
    Tab {
        tab: u64,
        ev: Box<TabEvent>,
    },
    /// The About window's update check finished.
    Update(crate::about::UpdateStatus),
}

struct Tab {
    id: u64,
    tx: Sender<TabCmd>,
    title: String,
    url: Option<Url>,
    history: Vec<HistoryEntry>,
    index: usize,
    loading: bool,
    load_started: Instant,
    scroll_y: f32,
    content_height: f32,
    frame: Option<Frame>,
    favicon: Option<ImageData>,
    secure: bool,
    hover_link: Option<String>,
    /// Image (`<img src>`) under the pointer, for the image context menu.
    hover_image: Option<String>,
    pointer: bool,
    /// Frames received from the worker (test hook for frame scrolling/navigation).
    frame_renders: u64,
    frame_titles: Vec<String>,
    /// What has keyboard focus inside the page (decides which keys go to the page).
    focus: Option<FocusKind>,
    /// The page-has-keyboard-focus state last sent to the worker.
    window_focus_sent: Option<bool>,
}

/// Height of the find bar at the bottom of the window (logical px).
const FIND_H: f32 = 40.0;

/// Find-in-page bar state.
struct FindBar {
    field: TextField,
    focused: bool,
    total: usize,
    active: Option<usize>,
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
    FindField,
    FindPrev,
    FindNext,
    FindClose,
    Content,
}

struct BrowserWindow {
    window: Rc<Window>,
    /// The window's inner size in physical pixels, from `Resized` events. (winit's X11
    /// `inner_size()` asks the X server every time and panics if that fails.)
    size: winit::dpi::PhysicalSize<u32>,
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
    /// The find bar (Ctrl+F), when open.
    find: Option<FindBar>,
    /// The open popup menu (≡ app menu or a context menu).
    menu: Option<Menu>,
    /// A blocked submission's validation message under its control (page rect, text); gone
    /// at the next input.
    validation: Option<(Rect, String)>,
    /// Tabs requested by link clicks: (url, open in foreground).
    pending_background: Vec<(NavRequest, bool)>,
    /// The `<select>` whose drop-down is open (the menu of kind "select").
    select_popup: Option<SelectPopup>,
    /// When the page was last scrolled (scrollbar visibility).
    last_scroll: Instant,
    /// Dragging the scrollbar thumb: grab offset within the thumb (logical px).
    drag_thumb: Option<f32>,
}

struct ClosedTab {
    history: Vec<HistoryEntry>,
    index: usize,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    client: Arc<bb_net::Client>,
    /// Private windows' network client: its own cookie jar (and later cache), never saved.
    private_client: Arc<bb_net::Client>,
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
    /// The About window, while open.
    about: Option<crate::about::AboutWindow>,
    about_modifiers: ModifiersState,
    /// The profile (persistent cookies, prefs); `None` when it can't be used (see the status).
    profile: Option<Arc<bb_profile::Profile>>,
    profile_status: bb_profile::OpenStatus,
    cookie_saver: Option<bb_profile::CookieSaver>,
    /// The last normal window's size and maximized state, saved as prefs at exit.
    last_geometry: Option<((u32, u32), bool)>,
    /// Maximize the first window (restored from the prefs).
    start_maximized: bool,
}

/// The profile directory: `--profile`, else `$BB_PROFILE`, else the platform's data directory
/// (`~/.local/share/BannedBrowser/profile`, `~/Library/Application Support/...`,
/// `%APPDATA%\BannedBrowser\profile`).
fn profile_dir(args: &Args) -> Option<std::path::PathBuf> {
    args.profile
        .clone()
        .or_else(|| std::env::var_os("BB_PROFILE").map(Into::into))
        .or_else(|| dirs::data_dir().map(|d| d.join("BannedBrowser").join("profile")))
}

/// The disk HTTP cache's size limit.
const HTTP_CACHE_BYTES: u64 = 256 * 1024 * 1024;
/// The memory-only caches' limit (private windows, or no usable profile).
const MEMORY_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// "1280x800" → (1280, 800), within sane bounds.
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let (w, h) = s.split_once('x')?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    ((360..=16384).contains(&w) && (240..=16384).contains(&h)).then_some((w, h))
}

/// Cmd on macOS, Ctrl elsewhere.
fn primary(m: ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        m.super_key()
    } else {
        m.control_key()
    }
}

/// Keep the Public Suffix List current (cookies depend on it): load the cached copy and, at most
/// once a day, download the current one, on a background thread. `BB_PSL_UPDATE=0` turns it off
/// (the GUI tests do, so they never touch the network or the user's cache).
fn refresh_public_suffix_list(client: &Arc<bb_net::Client>) {
    if std::env::var("BB_PSL_UPDATE").is_ok_and(|v| v == "0") {
        return;
    }
    let Some(dir) = dirs::cache_dir().map(|d| d.join("BannedBrowser").join("psl")) else { return };
    let client = client.clone();
    let spawned = std::thread::Builder::new().name("psl-refresh".into()).spawn(move || {
        let outcome = bb_net::psl::refresh(&client, &dir);
        log::info!("public suffix list: {outcome:?} (in use: {})", bb_net::psl::version());
    });
    if let Err(e) = spawned {
        log::warn!("couldn't start the public suffix list refresh: {e}");
    }
}

pub fn run(args: Args) -> anyhow::Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let (profile, profile_status) = match profile_dir(&args) {
        Some(dir) => bb_profile::Profile::open(&dir),
        None => (None, bb_profile::OpenStatus::Failed("no data directory".into())),
    };
    // The HTTP cache: on disk next to the profile (an explicit --profile / $BB_PROFILE keeps it
    // inside that directory), in memory only when the profile isn't usable and for private
    // windows.
    let explicit = args.profile.is_some() || std::env::var_os("BB_PROFILE").is_some();
    let cache_dir = match &profile {
        Some(p) if explicit => Some(p.dir().join("cache")),
        Some(_) => dirs::cache_dir().map(|d| d.join("BannedBrowser").join("http-cache")),
        None => None,
    };
    let cache = Arc::new(match cache_dir {
        Some(d) => bb_net::cache::HttpCache::on_disk(&d, HTTP_CACHE_BYTES),
        None => bb_net::cache::HttpCache::in_memory(MEMORY_CACHE_BYTES),
    });
    let private_cache = Arc::new(bb_net::cache::HttpCache::in_memory(MEMORY_CACHE_BYTES));
    let client = Arc::new(bb_engine::new_client_with_cache(
        args.extra_ca.clone(),
        args.user_agent.clone(),
        Some(cache),
    ));
    let private_client = Arc::new(bb_engine::new_client_with_cache(
        args.extra_ca.clone(),
        args.user_agent.clone(),
        Some(private_cache),
    ));
    refresh_public_suffix_list(&client);
    match &profile_status {
        bb_profile::OpenStatus::Opened | bb_profile::OpenStatus::Created => {}
        other => log::warn!("profile: {other:?}; cookies and prefs won't be saved this session"),
    }
    let profile = profile.map(Arc::new);
    if let Some(p) = &profile {
        client.cookies().load(p.load_cookies());
        client.storage().load(p.load_storage());
    }
    let cookie_saver = profile.clone().map(|p| bb_profile::CookieSaver::start(p, client.clone()));
    let saved_size = profile.as_ref().and_then(|p| p.pref("window.size")).and_then(|s| parse_size(&s));
    let start_maximized = args.window_size.is_none()
        && profile.as_ref().and_then(|p| p.pref("window.maximized")).as_deref() == Some("1");
    let mut initial: Vec<Url> =
        args.urls.iter().chain(args.new_tab.iter()).filter_map(|u| fixup_url(u)).collect();
    if initial.is_empty() {
        initial.push(Url::parse(HOME).expect("home URL"));
    }
    let mut app = App {
        proxy: event_loop.create_proxy(),
        client,
        private_client,
        windows: HashMap::new(),
        next_tab: 1,
        initial,
        initial_private: args.private_window,
        window_size: args.window_size.or(saved_size).unwrap_or((1280, 800)),
        test_state: std::env::var_os("BB_TEST_STATE").map(Into::into),
        closed: Vec::new(),
        context: None,
        force_dark: args.dark,
        about: None,
        about_modifiers: ModifiersState::empty(),
        profile,
        profile_status,
        cookie_saver,
        last_geometry: None,
        start_maximized,
    };
    event_loop.run_app(&mut app)?;
    app.save_profile();
    Ok(())
}

impl App {
    /// Save the prefs and the cookies one last time (at exit).
    fn save_profile(&mut self) {
        if let (Some(p), Some(((w, h), maximized))) = (&self.profile, self.last_geometry) {
            let r = p
                .set_pref("window.size", &format!("{w}x{h}"))
                .and_then(|_| p.set_pref("window.maximized", if maximized { "1" } else { "0" }));
            if let Err(e) = r {
                log::warn!("couldn't save the window size: {e}");
            }
        }
        if let Some(s) = self.cookie_saver.take() {
            s.finish();
        }
    }

    /// Remember a normal window's geometry (private windows don't count).
    fn note_geometry(&mut self, bw: &BrowserWindow) {
        if bw.private {
            return;
        }
        let maximized = bw.window.is_maximized();
        let (w, h) = bw.logical_size();
        let size = if maximized {
            // Keep the size it had before it was maximized.
            self.last_geometry.map_or((w.round() as u32, h.round() as u32), |g| g.0)
        } else {
            (w.round() as u32, h.round() as u32)
        };
        self.last_geometry = Some((size, maximized));
    }
}

impl App {
    /// Open the About window (or bring it to the front), and start its update check.
    fn open_about(&mut self, el: &ActiveEventLoop) {
        if let Some(a) = &self.about {
            a.window.focus_window();
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("About BannedBrowser")
            .with_inner_size(LogicalSize::new(crate::about::SIZE.0, crate::about::SIZE.1))
            .with_resizable(false);
        let window = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                log::error!("cannot create the About window: {e}");
                return;
            }
        };
        if self.context.is_none() {
            self.context = softbuffer::Context::new(window.clone()).ok();
        }
        let Some(context) = self.context.as_ref() else { return };
        let dark = self.force_dark.unwrap_or(matches!(window.theme(), Some(winit::window::Theme::Dark)));
        let Some(about) = crate::about::AboutWindow::new(window, context, dark) else { return };
        about.window.request_redraw();
        self.about = Some(about);
        // The check runs off the UI thread and reports back as a user event.
        let (client, proxy) = (self.client.clone(), self.proxy.clone());
        let _ = std::thread::Builder::new().name("update-check".into()).spawn(move || {
            let status = crate::about::check_for_updates(&client);
            let _ = proxy.send_event(UserEvent::Update(status));
        });
        self.write_state();
    }

    /// Events for the About window.
    fn about_event(&mut self, el: &ActiveEventLoop, event: WindowEvent) {
        let Some(a) = self.about.as_mut() else { return };
        let mut open_link: Option<usize> = None;
        let mut close = false;
        match event {
            WindowEvent::CloseRequested => close = true,
            WindowEvent::RedrawRequested => a.redraw(),
            WindowEvent::Focused(_) => {}
            WindowEvent::ThemeChanged(t) => {
                let dark = self.force_dark.unwrap_or(t == winit::window::Theme::Dark);
                a.theme = if dark { Theme::dark() } else { Theme::light() };
                a.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.about_modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let k = a.window.scale_factor() as f32;
                let over = a.set_hover(position.x as f32 / k, position.y as f32 / k);
                a.window.set_cursor(if over { CursorIcon::Pointer } else { CursorIcon::Default });
            }
            WindowEvent::CursorLeft { .. } => {
                a.set_hover(-1.0, -1.0);
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                // The hovered link (set_hover tracks the pointer).
                open_link = a.hovered();
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. }
                if event.state == ElementState::Pressed =>
            {
                let m = self.about_modifiers;
                match &event.logical_key {
                    Key::Named(NamedKey::Escape) => close = true,
                    Key::Named(NamedKey::Tab) => a.move_focus(m.shift_key()),
                    Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => open_link = a.focus,
                    Key::Character(c) if primary(m) && c.eq_ignore_ascii_case("w") => close = true,
                    _ => {}
                }
            }
            _ => {}
        }
        if let Some(i) = open_link {
            if let Some(link) = a.links().get(i).cloned() {
                self.open_link_from_about(el, &link.url);
            }
        }
        if close {
            self.about = None;
        }
        self.write_state();
    }

    /// Open a link from the About window in a new tab of a browser window (a new window if
    /// there is none).
    fn open_link_from_about(&mut self, el: &ActiveEventLoop, url: &str) {
        let Ok(u) = Url::parse(url) else { return };
        let target = self
            .windows
            .iter()
            .find(|(_, w)| w.window.has_focus())
            .map(|(id, _)| *id)
            .or_else(|| self.windows.iter().find(|(_, w)| !w.private).map(|(id, _)| *id));
        match target.and_then(|id| self.windows.remove(&id).map(|bw| (id, bw))) {
            Some((id, mut bw)) => {
                let t = self.new_tab(&mut bw, Some(u));
                bw.tabs.push(t);
                bw.active = bw.tabs.len() - 1;
                bw.sync_url_field();
                bw.dirty = true;
                bw.window.request_redraw();
                bw.window.focus_window();
                self.windows.insert(id, bw);
            }
            None => {
                self.open_window(el, vec![u], false);
            }
        }
    }

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
            size,
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
            select_popup: None,
            find: None,
            menu: None,
            validation: None,
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
        self.new_tab_request(bw, url.map(NavRequest::get))
    }

    fn new_tab_request(&mut self, bw: &mut BrowserWindow, req: Option<NavRequest>) -> Tab {
        let id = self.next_tab;
        self.next_tab += 1;
        let proxy = self.proxy.clone();
        let client = if bw.private { self.private_client.clone() } else { self.client.clone() };
        let tx = tab::spawn(
            id,
            client,
            Box::new(move |tab, ev| {
                let _ = proxy.send_event(UserEvent::Tab { tab, ev: Box::new(ev) });
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
            hover_image: None,
            pointer: false,
            frame_renders: 0,
            frame_titles: Vec::new(),
            focus: None,
            window_focus_sent: None,
        };
        let (vw, vh) = bw.viewport();
        let _ = t.tx.send(TabCmd::Render {
            viewport: (vw, vh),
            scale: bw.scale(),
            scroll_y: 0.0,
            dark: bw.page_dark(),
        });
        if let Some(req) = req {
            t.history.push(HistoryEntry { url: req.url.clone(), frames: Vec::new(), post: req.post.clone() });
            t.index = 0;
            t.url = Some(req.url.clone());
            t.loading = true;
            let _ = t.tx.send(TabCmd::Navigate(Box::new(req)));
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
                v["menu"] = match &w.menu {
                    Some(m) => serde_json::json!({ "kind": m.kind, "items": m.labels(), "hover": m.hover }),
                    None => serde_json::Value::Null,
                };
                v["hover_image"] = serde_json::json!(t.hover_image);
                v["page_focus"] = serde_json::json!(t.focus.map(FocusKind::name));
                v["find_open"] = serde_json::json!(w.find.is_some());
                v["find_total"] = serde_json::json!(w.find.as_ref().map_or(0, |f| f.total));
                v["find_active"] = serde_json::json!(w.find.as_ref().and_then(|f| f.active));
                v["frames"] = serde_json::json!(t.frame.as_ref().map_or(0, |f| f.frames.len()));
                v["frame_titles"] = serde_json::json!(t.frame_titles);
                v["frame_renders"] = serde_json::json!(t.frame_renders);
                v["scrollbar_visible"] = serde_json::json!(w.scrollbar().is_some() && w.scrollbar_visible());
                v["scrollbar_dragging"] = serde_json::json!(w.drag_thumb.is_some());
                // Centres of internal pages' id'd elements, in window content coordinates.
                let mut targets = serde_json::Map::new();
                if let Some(f) = &t.frame {
                    for (id, [x, y, bw_, bh]) in &f.targets {
                        let cy = y + bh / 2.0 + CHROME_H - t.scroll_y;
                        targets.insert(id.clone(), serde_json::json!([(x + bw_ / 2.0).round(), cy.round()]));
                    }
                }
                v["targets"] = serde_json::Value::Object(targets);
            }
            // Window geometry for the GUI test drivers, which send real OS input in screen
            // coordinates: the content area's position and size in logical pixels.
            let k = w.window.scale_factor();
            if let Ok(pos) = w.window.inner_position() {
                v["win_x"] = serde_json::json!((pos.x as f64 / k).round());
                v["win_y"] = serde_json::json!((pos.y as f64 / k).round());
            }
            let (lw, lh) = w.logical_size();
            v["win_w"] = serde_json::json!(lw.round());
            v["win_h"] = serde_json::json!(lh.round());
            v["scale"] = serde_json::json!(k);
            v["maximized"] = serde_json::json!(w.window.is_maximized());
            v["has_focus"] = serde_json::json!(w.window.has_focus());
        }
        v["profile"] = serde_json::json!(self.profile_status.code());
        v["about"] = match &self.about {
            Some(a) => {
                let links: Vec<&str> = a.links().iter().map(|l| l.id).collect();
                serde_json::json!({
                    "update": a.update.code(),
                    "focus": a.focus.and_then(|i| links.get(i).copied()),
                    "links": links,
                    "has_focus": a.window.has_focus(),
                })
            }
            None => serde_json::Value::Null,
        };
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
        let s = self.size;
        let k = self.scale();
        (s.width as f32 / k, s.height as f32 / k)
    }

    fn viewport(&self) -> (f32, f32) {
        let c = self.content_rect();
        (c.w.max(1.0), c.h.max(1.0))
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
        self.navigate_request(NavRequest::get(url));
    }

    /// Navigate the active tab: a new session history entry (a form POST keeps its body).
    fn navigate_request(&mut self, req: NavRequest) {
        let url = req.url.clone();
        if let Some(t) = self.tab_mut() {
            t.history.truncate(t.index + 1);
            let entry = HistoryEntry { url: url.clone(), frames: Vec::new(), post: req.post.clone() };
            if req.post.is_some() || t.history.last().map(|e| &e.url) != Some(&url) || t.history.is_empty() {
                t.history.push(entry);
            }
            t.index = t.history.len() - 1;
            t.url = Some(url.clone());
            t.loading = true;
            t.load_started = Instant::now();
            t.scroll_y = 0.0;
            t.favicon = None;
            t.focus = None;
            let _ = t.tx.send(TabCmd::Navigate(Box::new(req)));
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
            // Entries of the same (loaded) document differ only in their frames: restore those
            // without reloading the top-level page.
            let same_document = !t.loading && t.url.as_ref() == Some(&t.history[ni as usize].url);
            t.index = ni as usize;
            let entry = t.history[t.index].clone();
            t.url = Some(entry.url.clone());
            t.loading = true;
            t.load_started = Instant::now();
            if !same_document {
                t.scroll_y = 0.0;
                // History navigation prefers the cache, even stale entries.
                let mut req = entry.request();
                req.cache_mode = CacheMode::ForceCache;
                let _ = t.tx.send(TabCmd::Navigate(Box::new(req)));
            }
            if !entry.frames.is_empty() {
                let _ = t.tx.send(TabCmd::RestoreFrames(entry.frames));
            } else if same_document {
                t.loading = false;
            }
        }
        self.sync_url_field();
        self.dirty = true;
    }

    /// Reload: revalidate everything with the server; `hard` bypasses the HTTP cache.
    fn reload(&mut self, hard: bool) {
        log::debug!("reload (hard: {hard})");
        if let Some(t) = self.tab_mut() {
            if let Some(h) = t.history.get(t.index).cloned() {
                t.loading = true;
                t.load_started = Instant::now();
                let mut req = h.request();
                req.cache_mode = if hard { CacheMode::Reload } else { CacheMode::NoCache };
                let _ = t.tx.send(TabCmd::Navigate(Box::new(req)));
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

    /// Scroll from the wheel or keyboard: child frames under the pointer
    /// get the scroll first (the tab worker hands it back if they can't).
    fn scroll_content(&mut self, dy: f32, wheel: bool) {
        let (mx, my) = self.mouse;
        let vh = self.viewport().1;
        let Some(t) = self.tab() else { return };
        let frames = t.frame.as_ref().map(|f| f.frames.as_slice()).unwrap_or(&[]);
        if frames.is_empty() || (!wheel && t.content_height > vh + 0.5) {
            self.scroll_by(dy);
            return;
        }
        let (px, py) = (mx, my - CHROME_H + t.scroll_y);
        let over = |r: &Rect| px >= r.x && py >= r.y && px < r.right() && py < r.bottom();
        let point = if my >= CHROME_H && frames.iter().any(over) {
            Some((px, py))
        } else if !wheel {
            // Keyboard in a frameset: scroll the largest frame.
            frames
                .iter()
                .max_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h)))
                .map(|r| (r.x + r.w / 2.0, r.y + r.h / 2.0))
        } else {
            None
        };
        match point {
            Some((x, y)) => {
                let _ = t.tx.send(TabCmd::Wheel { x, y, dy });
            }
            None => self.scroll_by(dy),
        }
    }

    /// Re-evaluate what's under the pointer without a mouse move (e.g. after a
    /// popup menu closes, since moves aren't passed to the page while it's open).
    fn refresh_hover(&mut self) {
        let (x, y) = self.mouse;
        self.hover = self.target_at(x, y);
        if self.hover == Target::Content {
            if let Some(t) = self.tab() {
                let _ = t.tx.send(TabCmd::Hover { x, y: y - CHROME_H + t.scroll_y });
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

    /// Show a `<select>`'s options as a menu under the select (window coordinates).
    fn open_select_popup(&mut self, popup: SelectPopup) {
        let sy = self.tab().map(|t| t.scroll_y).unwrap_or(0.0);
        let at = (popup.rect.x, popup.rect.bottom() - sy + CHROME_H);
        let mut entries = Vec::new();
        let mut current = None;
        for it in &popup.items {
            if it.group {
                entries.push(Entry::item(&it.label, None, Action::ChooseOption(usize::MAX), false));
                continue;
            }
            let Some(i) = it.option else { continue };
            if it.selected && current.is_none() {
                current = Some(entries.len());
            }
            let indent = if popup.items.iter().any(|g| g.group) { "    " } else { "" };
            let mark = if it.selected { "✓ " } else { "   " };
            entries.push(Entry::item(
                &format!("{mark}{indent}{}", it.label),
                None,
                Action::ChooseOption(i),
                it.enabled,
            ));
        }
        if entries.is_empty() {
            return;
        }
        let win = self.logical_size();
        let mut m = Menu::new("select", entries, at, win);
        m.rect.w = m.rect.w.max(popup.rect.w);
        // Below the select if it fits, else above it, else as high as the window allows.
        let top = popup.rect.y - sy + CHROME_H;
        m.rect.y = if at.1 + m.rect.h <= win.1 - 4.0 {
            at.1
        } else if top - m.rect.h >= CHROME_H {
            top - m.rect.h
        } else {
            (win.1 - m.rect.h - 4.0).max(4.0)
        };
        m.hover = current;
        self.menu = Some(m);
        self.select_popup = Some(popup);
        self.dirty = true;
    }

    /// Tell the active tab whether the page has keyboard focus (caret and focus ring).
    fn sync_window_focus(&mut self) {
        let on = !self.url_focused
            && !self.find.as_ref().is_some_and(|f| f.focused)
            && self.menu.is_none()
            && self.window.has_focus();
        if let Some(t) = self.tab_mut() {
            if t.window_focus_sent != Some(on) {
                t.window_focus_sent = Some(on);
                let _ = t.tx.send(TabCmd::WindowFocus(on));
            }
        }
    }

    fn content_rect(&self) -> Rect {
        let (w, h) = self.logical_size();
        let bar = if self.find.is_some() { FIND_H } else { 0.0 };
        Rect::new(0.0, CHROME_H, w, (h - CHROME_H - bar).max(0.0))
    }

    fn find_bar_rect(&self) -> Rect {
        let (w, h) = self.logical_size();
        Rect::new(0.0, h - FIND_H, w, FIND_H)
    }

    fn find_field_rect(&self) -> Rect {
        let r = self.find_bar_rect();
        Rect::new(8.0, r.y + 6.0, 260.0, 28.0)
    }

    /// Find bar buttons: 0 = previous, 1 = next, 2 = close.
    fn find_button(&self, i: usize) -> Rect {
        let r = self.find_bar_rect();
        if i == 2 {
            return Rect::new(r.right() - 36.0, r.y + 6.0, 28.0, 28.0);
        }
        Rect::new(272.0 + i as f32 * 30.0, r.y + 6.0, 28.0, 28.0)
    }

    /// Open (or focus) the find bar.
    fn open_find(&mut self) {
        let bar = self.find.get_or_insert_with(|| FindBar {
            field: TextField::new(""),
            focused: true,
            total: 0,
            active: None,
        });
        bar.focused = true;
        bar.field.select_all();
        self.url_focused = false;
        self.request_render();
    }

    fn close_find(&mut self) {
        if self.find.take().is_some() {
            if let Some(t) = self.tab() {
                let _ = t.tx.send(TabCmd::FindClose);
            }
            self.request_render();
        }
    }

    /// Send the find query (step 0 = new search from the top of the viewport).
    fn send_find(&mut self, step: i8) {
        let Some(bar) = &self.find else { return };
        let query = bar.field.text.clone();
        let Some(t) = self.tab() else { return };
        if query.is_empty() {
            let _ = t.tx.send(TabCmd::FindClose);
            if let Some(b) = self.find.as_mut() {
                b.total = 0;
                b.active = None;
            }
            return;
        }
        let _ = t.tx.send(TabCmd::Find { query, step, from_y: t.scroll_y });
    }

    fn target_at(&self, x: f32, y: f32) -> Target {
        if self.find.is_some() && y >= self.find_bar_rect().y {
            if self.find_field_rect().contains(x, y) {
                return Target::FindField;
            }
            for (i, t) in [Target::FindPrev, Target::FindNext, Target::FindClose].into_iter().enumerate() {
                if self.find_button(i).contains(x, y) {
                    return t;
                }
            }
            return Target::None;
        }
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
        // Find bar.
        if let Some(bar) = &self.find {
            let r = self.find_bar_rect();
            p.rect(r, th.toolbar);
            p.rect(Rect::new(0.0, r.y, w, 1.0), th.separator);
            let fr = self.find_field_rect();
            if bar.focused {
                p.rounded(
                    Rect::new(fr.x - 2.0, fr.y - 2.0, fr.w + 4.0, fr.h + 4.0),
                    6.0,
                    th.accent.with_alpha(0.55),
                );
            }
            let not_found = bar.total == 0 && !bar.field.text.is_empty();
            let field_bg = if not_found { Color::rgba(0xE0, 0x30, 0x30, 0x40) } else { th.field };
            p.rounded(fr, 4.0, th.field);
            p.rounded(fr, 4.0, field_bg);
            p.path(bb_ui::icons::path(Icon::Search, fr.x + 8.0, fr.y + 6.0, 16.0), th.text_muted);
            let tx = fr.x + 30.0;
            let size = 13.5;
            let base = bb_ui::text::centered_baseline(fr.y, fr.h, size);
            p.clip(Rect::new(tx, fr.y, fr.w - 36.0, fr.h));
            let b = bb_ui::text::boundaries(&bar.field.text, size);
            let xof = |i: usize| b.iter().find(|(j, _)| *j >= i).map(|(_, x)| *x).unwrap_or(0.0);
            let caret_x = xof(bar.field.caret);
            let scroll = (caret_x - (fr.w - 50.0)).max(0.0);
            if bar.focused {
                if let Some((a, bb)) = bar.field.selection() {
                    p.rect(
                        Rect::new(tx + xof(a) - scroll, fr.y + 6.0, xof(bb) - xof(a), fr.h - 12.0),
                        th.accent.with_alpha(0.35),
                    );
                }
            }
            if bar.field.text.is_empty() {
                p.text("Find in page", tx, base, size, 400, th.text_muted);
            } else {
                p.text(&bar.field.text, tx - scroll, base, size, 400, th.text);
            }
            if bar.focused {
                p.rect(Rect::new(tx + caret_x - scroll, fr.y + 7.0, 1.5, fr.h - 14.0), th.text);
            }
            p.unclip();
            let en = bar.total > 0;
            let prev = self.find_button(0);
            p.icon_button(
                prev,
                Icon::ChevronUp,
                th,
                self.hover == Target::FindPrev,
                self.pressed == Target::FindPrev,
                en,
            );
            let next = self.find_button(1);
            p.icon_button(
                next,
                Icon::ChevronDown,
                th,
                self.hover == Target::FindNext,
                self.pressed == Target::FindNext,
                en,
            );
            let label = match (bar.total, bar.active) {
                (0, _) if bar.field.text.is_empty() => String::new(),
                (0, _) => "Phrase not found".to_string(),
                (1, _) => "1 of 1 match".to_string(),
                (n, Some(a)) => format!("{} of {n} matches", a + 1),
                (n, None) => format!("{n} matches"),
            };
            let lb = bb_ui::text::centered_baseline(r.y, r.h, 12.5);
            p.text(
                &label,
                next.right() + 12.0,
                lb,
                12.5,
                400,
                if not_found { th.accent } else { th.text_muted },
            );
            let cr = self.find_button(2);
            if self.hover == Target::FindClose {
                p.rounded(cr, 4.0, th.button_hover);
            }
            p.path(bb_ui::icons::path(Icon::Close, cr.x + 7.0, cr.y + 7.0, 14.0), th.text);
        }
        // Status panel (hovered link).
        if let Some(link) = t.and_then(|t| t.hover_link.clone()) {
            let h = self.content_rect().bottom();
            let tw = bb_ui::text::measure(&link, 12.0, 400).min(w * 0.6);
            let r = Rect::new(2.0, h - 26.0, tw + 16.0, 24.0);
            p.shadow(r, 4.0, 4.0, th.shadow.with_alpha(0.3));
            p.rounded(r, 4.0, th.popup);
            let base = bb_ui::text::centered_baseline(r.y, r.h, 12.0);
            p.text_faded(&link, r.x + 8.0, base, 12.0, tw, th.text, th.popup);
        }
        // A validation message under its control (HTML §4.10.21.2 "report the problems").
        if let (Some((r, msg)), Some(t)) = (&self.validation, t) {
            let tw = bb_ui::text::measure(msg, 13.0, 400).min(w * 0.6);
            let x = r.x.clamp(4.0, (w - tw - 28.0).max(4.0));
            let y = r.bottom() - t.scroll_y + CHROME_H + 6.0;
            let b = Rect::new(x, y, tw + 24.0, 30.0);
            p.shadow(b, 6.0, 8.0, th.shadow.with_alpha(0.35));
            p.rounded(b, 6.0, th.popup);
            p.dl.push(Item::Border {
                rect: b,
                radii: bb_paint::Radii::uniform(6.0),
                sides: [Some(bb_paint::BorderSide {
                    width: 1.0,
                    color: th.separator,
                    style: bb_paint::BorderStyle::Solid,
                }); 4],
            });
            let base = bb_ui::text::centered_baseline(b.y, b.h, 13.0);
            p.text_faded(msg, b.x + 12.0, base, 13.0, tw, th.text, th.popup);
        }
        // Popup menu on top of everything.
        if let Some(m) = &self.menu {
            m.paint(&mut p, th);
        }
        dl
    }

    fn redraw(&mut self) {
        let size = self.size;
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
            if let Some(id) = self.open_window(el, urls, private) {
                if std::mem::take(&mut self.start_maximized) {
                    if let Some(bw) = self.windows.get(&id) {
                        bw.window.set_maximized(true);
                    }
                }
            }
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        self.save_profile();
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, event: UserEvent) {
        let (tab, ev) = match event {
            UserEvent::Tab { tab, ev } => (tab, *ev),
            UserEvent::Update(status) => {
                if let Some(a) = self.about.as_mut() {
                    a.set_update(status);
                }
                self.write_state();
                return;
            }
        };
        let mut state_changed = false;
        for bw in self.windows.values_mut() {
            let Some(ti) = bw.tabs.iter().position(|t| t.id == tab) else { continue };
            let is_active = ti == bw.active;
            match ev {
                TabEvent::GoBack => {
                    if is_active {
                        if bw.tab().is_some_and(|t| t.index > 0) {
                            bw.go_history(-1);
                        } else if let Ok(u) = Url::parse(HOME) {
                            bw.navigate(u);
                        }
                        state_changed = true;
                    }
                }
                TabEvent::Started => {
                    let t = &mut bw.tabs[ti];
                    t.loading = true;
                    t.load_started = Instant::now();
                    t.hover_link = None;
                    state_changed = true;
                }
                TabEvent::FrameNavigated { frames } => {
                    let t = &mut bw.tabs[ti];
                    if let Some(url) = t.url.clone() {
                        t.history.truncate(t.index + 1);
                        t.history.push(HistoryEntry { url, frames, post: None });
                        t.index = t.history.len() - 1;
                        state_changed = true;
                    }
                }
                TabEvent::Loaded { url, title, content_height, secure, frame_titles, frames } => {
                    let t = &mut bw.tabs[ti];
                    t.loading = false;
                    t.frame_titles = frame_titles;
                    t.title = title;
                    t.content_height = content_height;
                    t.secure = secure;
                    // Redirects: replace the current history entry (a POST answered with a
                    // redirect is now a GET of the new URL).
                    if t.url.as_ref() != Some(&url) {
                        if let Some(h) = t.history.get_mut(t.index) {
                            *h = HistoryEntry::new(url.clone());
                        }
                        t.url = Some(url);
                    }
                    // Record the document's initial frames in its history entry.
                    if let Some(h) = t.history.get_mut(t.index) {
                        if h.frames.is_empty() {
                            h.frames = frames;
                        }
                    }
                    if is_active {
                        if !bw.url_focused {
                            bw.sync_url_field();
                        }
                        bw.update_title();
                        if bw.find.as_ref().is_some_and(|f| !f.field.text.is_empty()) {
                            bw.send_find(0);
                        }
                    }
                    state_changed = true;
                }
                TabEvent::Favicon(img) => bw.tabs[ti].favicon = Some(img),
                TabEvent::Frame(f) => {
                    let t = &mut bw.tabs[ti];
                    t.frame = Some(f);
                    t.frame_renders += 1;
                    state_changed = true;
                }
                TabEvent::Hover { link, image, pointer } => {
                    let t = &mut bw.tabs[ti];
                    if t.hover_link != link || t.hover_image != image {
                        state_changed = true;
                    }
                    t.hover_link = link;
                    t.hover_image = image;
                    t.pointer = pointer;
                    if is_active {
                        bw.window.set_cursor(if pointer { CursorIcon::Pointer } else { CursorIcon::Default });
                    }
                }
                TabEvent::Clicked { link, open } => {
                    if let Some(u) = link {
                        if is_active {
                            match open {
                                LinkOpen::Current => bw.navigate_request(u),
                                LinkOpen::BackgroundTab => bw.pending_background.push((u, false)),
                                LinkOpen::ForegroundTab => bw.pending_background.push((u, true)),
                            }
                            state_changed = true;
                        }
                    }
                }
                TabEvent::Focus { kind, rect } => {
                    let t = &mut bw.tabs[ti];
                    t.focus = kind;
                    // Keep the focused element in view (Tab, or a failed `required` check).
                    if let (Some(r), true) = (rect, is_active) {
                        let vh = bw.viewport().1;
                        let sy = bw.tabs[ti].scroll_y;
                        if r.y < sy || r.bottom() > sy + vh {
                            bw.scroll_to((r.y - vh / 3.0).max(0.0));
                        }
                    }
                    state_changed = true;
                }
                TabEvent::Popup(popup) => {
                    if is_active {
                        bw.open_select_popup(popup);
                        state_changed = true;
                    }
                }
                TabEvent::Validation { rect, message } => {
                    if is_active {
                        bw.validation = Some((rect, message));
                        state_changed = true;
                    }
                }
                TabEvent::Copy(text) => {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(text);
                    }
                }
                TabEvent::FocusExit => {
                    // Tab past the page's last element: on to the address bar (like Firefox's
                    // toolbar), which then hands focus back on the next Tab.
                    if is_active {
                        bw.tabs[ti].focus = None;
                        bw.url_focused = true;
                        bw.url.select_all();
                        state_changed = true;
                    }
                }
                TabEvent::FindResult { total, active, rect } => {
                    if is_active {
                        if let Some(f) = bw.find.as_mut() {
                            f.total = total;
                            f.active = active;
                        }
                        // Scroll the active match into view (a third from the top).
                        if let (Some(r), Some(t)) = (rect, bw.tabs.get(ti)) {
                            let vh = bw.viewport().1;
                            if r.y < t.scroll_y || r.bottom() > t.scroll_y + vh {
                                bw.scroll_to((r.y - vh / 3.0).max(0.0));
                            }
                        }
                        state_changed = true;
                    }
                }
                TabEvent::WheelUnconsumed { dy } => {
                    if is_active {
                        bw.scroll_by(dy);
                    }
                }
            }
            bw.dirty = true;
            bw.window.request_redraw();
            break;
        }
        // Focus may have moved between the page and the chrome (e.g. Tab past the last element).
        for w in self.windows.values_mut() {
            w.sync_window_focus();
        }
        // Open any background tabs requested by Ctrl+click.
        let ids: Vec<WindowId> = self.windows.keys().copied().collect();
        for id in ids {
            let pending = self
                .windows
                .get_mut(&id)
                .map(|w| std::mem::take(&mut w.pending_background))
                .unwrap_or_default();
            for (u, foreground) in pending {
                if let Some(mut bw) = self.windows.remove(&id) {
                    let t = self.new_tab_request(&mut bw, Some(u));
                    bw.tabs.insert(bw.active + 1, t);
                    if foreground {
                        bw.active += 1;
                        bw.sync_url_field();
                        bw.update_title();
                        bw.request_render();
                    }
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
        if self.about.as_ref().is_some_and(|a| a.window.id() == id) {
            self.about_event(el, event);
            return;
        }
        let Some(mut bw) = self.windows.remove(&id) else { return };
        let mut keep = true;
        let mut state_changed = false;
        match event {
            WindowEvent::CloseRequested => keep = false,
            WindowEvent::Resized(sz) => {
                bw.size = sz;
                self.note_geometry(&bw);
                bw.request_render();
                bw.dirty = true;
                state_changed = true;
            }
            // A Resized event with the new physical size follows.
            WindowEvent::ScaleFactorChanged { .. } => {
                self.note_geometry(&bw);
                bw.request_render();
                bw.dirty = true;
                state_changed = true;
            }
            // The test state reports window position and focus.
            WindowEvent::Moved(_) | WindowEvent::Focused(_) => state_changed = true,
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
                if let Some(m) = bw.menu.as_mut() {
                    let h = if m.contains(x, y) { m.item_at(x, y) } else { m.hover };
                    if h != m.hover {
                        m.hover = h;
                        bw.window.request_redraw();
                        self.windows.insert(id, bw);
                        self.write_state();
                        return;
                    }
                    self.windows.insert(id, bw);
                    return;
                }
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
            WindowEvent::MouseInput { state, .. } if bw.menu.is_some() => {
                let (x, y) = bw.mouse;
                let inside = bw.menu.as_ref().is_some_and(|m| m.contains(x, y));
                match (state, inside) {
                    // Choose an item on release (Firefox-style: press-drag-release works too).
                    (ElementState::Released, true) => {
                        let action = bw
                            .menu
                            .as_ref()
                            .and_then(|m| m.item_at(x, y))
                            .and_then(|i| bw.menu.as_ref().and_then(|m| m.action(i)));
                        if let Some(a) = action {
                            bw.menu = None;
                            self.run_action(&mut bw, el, a);
                        }
                    }
                    // A press outside closes the menu and is not passed on.
                    (ElementState::Pressed, false) => bw.menu = None,
                    _ => {}
                }
                bw.pressed = Target::None;
                if bw.menu.is_none() {
                    bw.refresh_hover();
                }
                bw.dirty = true;
                state_changed = true;
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if state == ElementState::Pressed && bw.validation.take().is_some() {
                    bw.dirty = true;
                }
                let (x, y) = bw.mouse;
                let tgt = bw.target_at(x, y);
                match (state, button) {
                    (ElementState::Pressed, MouseButton::Right) => {
                        // Like Firefox: right-clicking an unfocused URL bar focuses it
                        // and selects the address, so Cut/Copy/Delete apply to it.
                        if tgt == Target::UrlBar && !bw.url_focused {
                            if let Some(f) = bw.find.as_mut() {
                                f.focused = false;
                            }
                            bw.url_focused = true;
                            bw.url.select_all();
                        }
                        bw.menu = self.context_menu(&bw, tgt, (x, y));
                        bw.dirty = true;
                        state_changed = true;
                    }
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
                            if let Some(f) = bw.find.as_mut() {
                                f.focused = false;
                            }
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
                                let _ = t.tx.send(TabCmd::Click { x, y: y - CHROME_H + sy, new_tab: true });
                            }
                        }
                        bw.dirty = true;
                    }
                    (ElementState::Released, MouseButton::Back) => bw.go_history(-1),
                    (ElementState::Released, MouseButton::Forward) => bw.go_history(1),
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { .. } if bw.menu.is_some() => {
                bw.menu = None;
                bw.refresh_hover();
                bw.dirty = true;
                state_changed = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if bw.validation.take().is_some() {
                    bw.dirty = true;
                }
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y * 48.0,
                    MouseScrollDelta::PixelDelta(PhysicalPosition { y, .. }) => -(y as f32) / bw.scale(),
                };
                bw.scroll_content(dy, true);
                state_changed = true;
            }
            WindowEvent::Ime(Ime::Commit(s))
                if !bw.url_focused
                    && !bw.find.as_ref().is_some_and(|f| f.focused)
                    && bw
                        .tab()
                        .is_some_and(|t| matches!(t.focus, Some(FocusKind::Text | FocusKind::TextArea))) =>
            {
                if let Some(t) = bw.tab() {
                    let _ = t.tx.send(TabCmd::Text(s));
                }
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
                bw.validation = None;
                state_changed |= self.key(&mut bw, el, &event.logical_key, event.text.as_deref());
                bw.dirty = true;
            }
            _ => {}
        }
        if keep && !bw.tabs.is_empty() {
            if bw.menu.is_none() {
                bw.select_popup = None;
            }
            bw.sync_window_focus();
            if bw.dirty {
                bw.window.request_redraw();
            }
            self.windows.insert(id, bw);
        } else {
            for t in &bw.tabs {
                let _ = t.tx.send(TabCmd::Shutdown);
            }
            // The private session ends with the last private window: forget its cookies and cache.
            if bw.private && !self.windows.values().any(|w| w.private) {
                self.private_client.cookies().clear();
                if let Some(c) = self.private_client.cache() {
                    c.clear();
                }
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
                bw.reload(bw.modifiers.shift_key());
                true
            }
            Target::Home => {
                if let Ok(u) = Url::parse(HOME) {
                    bw.navigate(u);
                }
                true
            }
            Target::FindField => {
                if let Some(f) = bw.find.as_mut() {
                    f.focused = true;
                }
                bw.url_focused = false;
                bw.sync_url_field();
                true
            }
            Target::FindPrev | Target::FindNext => {
                bw.send_find(if tgt == Target::FindNext { 1 } else { -1 });
                true
            }
            Target::FindClose => {
                bw.close_find();
                true
            }
            Target::Menu => {
                let _ = el;
                bw.menu = Some(self.app_menu(bw));
                true
            }
            Target::Content => {
                let (x, y) = bw.mouse;
                let sy = bw.tab().map(|t| t.scroll_y).unwrap_or(0.0);
                if let Some(t) = bw.tab() {
                    let new_tab = primary(bw.modifiers);
                    let _ = t.tx.send(TabCmd::Click { x, y: y - CHROME_H + sy, new_tab });
                }
                false
            }
            _ => false,
        }
    }

    /// The ≡ app menu, right-aligned under the menu button.
    fn app_menu(&self, bw: &BrowserWindow) -> Menu {
        let pk = primary_label();
        let sc = |k: &str| format!("{pk}+{k}");
        let entries = vec![
            Entry::item("New Tab", Some(&sc("T")), Action::NewTab, true),
            Entry::item("New Window", Some(&sc("N")), Action::NewWindow, true),
            Entry::item("New Private Window", Some(&sc("Shift+P")), Action::NewPrivateWindow, true),
            Entry::Separator,
            Entry::item("Find in Page…", Some(&sc("F")), Action::FindInPage, true),
            Entry::Separator,
            Entry::item("About BannedBrowser", None, Action::About, true),
            Entry::item(
                if cfg!(target_os = "macos") { "Quit" } else { "Exit" },
                Some(&sc("Q")),
                Action::Quit,
                true,
            ),
        ];
        let mr = bw.menu_rect();
        let mut m = Menu::new("app", entries, (0.0, mr.bottom() + 4.0), bw.logical_size());
        m.rect.x = (mr.right() - m.rect.w).max(4.0);
        m
    }

    /// Context menu for what's under the pointer (`None` if nothing applies).
    fn context_menu(&self, bw: &BrowserWindow, tgt: Target, at: (f32, f32)) -> Option<Menu> {
        let pk = primary_label();
        let win = bw.logical_size();
        let (kind, entries) = match tgt {
            Target::Tab(i) | Target::TabClose(i) => (
                "tab",
                vec![
                    Entry::item("New Tab", Some(&format!("{pk}+T")), Action::NewTab, true),
                    Entry::item("Reload Tab", None, Action::TabReload(i), bw.tabs[i].url.is_some()),
                    Entry::item("Duplicate Tab", None, Action::TabDuplicate(i), bw.tabs[i].url.is_some()),
                    Entry::Separator,
                    Entry::item("Close Tab", Some(&format!("{pk}+W")), Action::TabClose(i), true),
                    Entry::item("Close Other Tabs", None, Action::TabCloseOthers(i), bw.tabs.len() > 1),
                    Entry::item(
                        "Reopen Closed Tab",
                        Some(&format!("{pk}+Shift+T")),
                        Action::TabReopen,
                        !self.closed.is_empty(),
                    ),
                ],
            ),
            Target::UrlBar => {
                let sel = bw.url.selection().is_some_and(|(a, b)| b > a);
                (
                    "urlbar",
                    vec![
                        Entry::item("Undo", Some(&format!("{pk}+Z")), Action::UrlUndo, true),
                        Entry::Separator,
                        Entry::item("Cut", Some(&format!("{pk}+X")), Action::UrlCut, sel),
                        Entry::item("Copy", Some(&format!("{pk}+C")), Action::UrlCopy, sel),
                        Entry::item("Paste", Some(&format!("{pk}+V")), Action::UrlPaste, true),
                        Entry::item("Paste & Go", None, Action::UrlPasteAndGo, true),
                        Entry::item("Delete", None, Action::UrlDelete, sel),
                        Entry::Separator,
                        Entry::item("Select All", Some(&format!("{pk}+A")), Action::UrlSelectAll, true),
                    ],
                )
            }
            Target::Content => {
                let t = bw.tab()?;
                let link = t.hover_link.as_deref().and_then(|l| Url::parse(l).ok());
                let image = t.hover_image.as_deref().and_then(|l| Url::parse(l).ok());
                let mut e = Vec::new();
                if let Some(u) = &link {
                    e.push(Entry::item("Open Link in New Tab", None, Action::OpenInNewTab(u.clone()), true));
                    if !bw.private {
                        e.push(Entry::item(
                            "Open Link in New Window",
                            None,
                            Action::OpenInNewWindow(u.clone()),
                            true,
                        ));
                    }
                    e.push(Entry::item(
                        "Open Link in New Private Window",
                        None,
                        Action::OpenInPrivateWindow(u.clone()),
                        true,
                    ));
                    e.push(Entry::Separator);
                    e.push(Entry::item("Copy Link", None, Action::CopyText(u.href()), true));
                }
                if let Some(u) = &image {
                    if !e.is_empty() {
                        e.push(Entry::Separator);
                    }
                    e.push(Entry::item("Open Image in New Tab", None, Action::OpenInNewTab(u.clone()), true));
                    e.push(Entry::item("Copy Image Link", None, Action::CopyText(u.href()), true));
                }
                if !e.is_empty() {
                    (if link.is_some() { "link" } else { "image" }, e)
                } else {
                    (
                        "page",
                        vec![
                            Entry::item("Back", None, Action::Back, t.index > 0),
                            Entry::item("Forward", None, Action::Forward, t.index + 1 < t.history.len()),
                            Entry::item("Reload", Some(&format!("{pk}+R")), Action::Reload, t.url.is_some()),
                            Entry::Separator,
                            Entry::item("Find in Page…", Some(&format!("{pk}+F")), Action::FindInPage, true),
                        ],
                    )
                }
            }
            _ => return None,
        };
        Some(Menu::new(kind, entries, at, win))
    }

    /// Carry out a menu action.
    fn run_action(&mut self, bw: &mut BrowserWindow, el: &ActiveEventLoop, action: Action) {
        let home = || Url::parse(HOME).ok();
        match action {
            Action::ChooseOption(index) => {
                if let (Some(p), Some(t)) = (bw.select_popup.take(), bw.tab()) {
                    let _ =
                        t.tx.send(TabCmd::ChooseOption { frame_path: p.frame_path, select: p.select, index });
                }
            }
            Action::NewTab => self.open_tab(bw, home()),
            Action::NewWindow => {
                self.open_window(el, home().into_iter().collect(), false);
            }
            Action::NewPrivateWindow => {
                self.open_window(el, home().into_iter().collect(), true);
            }
            Action::FindInPage => bw.open_find(),
            Action::About => self.open_about(el),
            Action::Quit => el.exit(),
            Action::Back => bw.go_history(-1),
            Action::Forward => bw.go_history(1),
            Action::Reload => bw.reload(false),
            Action::OpenInNewTab(u) => {
                // Background tab right after the current one (Firefox's default).
                let t = self.new_tab(bw, Some(u));
                bw.tabs.insert(bw.active + 1, t);
            }
            Action::OpenInNewWindow(u) => {
                self.open_window(el, vec![u], false);
            }
            Action::OpenInPrivateWindow(u) => {
                self.open_window(el, vec![u], true);
            }
            Action::CopyText(s) => {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(s);
                }
            }
            Action::TabReload(i) => {
                if let Some(t) = bw.tabs.get_mut(i) {
                    if let Some(h) = t.history.get(t.index).cloned() {
                        t.loading = true;
                        t.load_started = Instant::now();
                        let mut req = h.request();
                        req.cache_mode = CacheMode::NoCache;
                        let _ = t.tx.send(TabCmd::Navigate(Box::new(req)));
                    }
                }
            }
            Action::TabDuplicate(i) => {
                if let Some((history, index)) = bw.tabs.get(i).map(|t| (t.history.clone(), t.index)) {
                    let entry = history.get(index).cloned();
                    let mut t = self.new_tab(bw, entry.as_ref().map(|e| e.url.clone()));
                    if let Some(e) = entry.filter(|e| !e.frames.is_empty()) {
                        let _ = t.tx.send(TabCmd::RestoreFrames(e.frames));
                    }
                    t.history = history;
                    t.index = index;
                    bw.tabs.insert(i + 1, t);
                    bw.active = i + 1;
                    bw.sync_url_field();
                    bw.update_title();
                }
            }
            Action::TabClose(i) => self.close_tab(bw, i),
            Action::TabCloseOthers(i) => {
                for j in (0..bw.tabs.len()).rev() {
                    if j != i {
                        self.close_tab(bw, j);
                    }
                }
                bw.active = 0;
                bw.sync_url_field();
                bw.request_render();
                bw.update_title();
            }
            Action::TabReopen => self.reopen_tab(bw),
            Action::UrlUndo => bw.url.undo(),
            Action::UrlCut | Action::UrlCopy => {
                if let Some(sel) = bw.url.selected_text().map(str::to_string) {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(sel);
                    }
                    if action == Action::UrlCut {
                        bw.url.backspace();
                    }
                }
            }
            Action::UrlPaste | Action::UrlPasteAndGo => {
                let text = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok());
                if let Some(t) = text {
                    bw.url.insert(&t.replace(['\n', '\r'], " "));
                    if action == Action::UrlPasteAndGo {
                        if let Some(u) = fixup_url(&bw.url.text.clone()) {
                            bw.navigate(u);
                        }
                    }
                }
            }
            Action::UrlDelete => bw.url.delete(),
            Action::UrlSelectAll => bw.url.select_all(),
        }
        bw.dirty = true;
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
            let entry = c.history.get(c.index).cloned();
            let mut t = self.new_tab(bw, entry.as_ref().map(|e| e.url.clone()));
            if let Some(e) = entry.filter(|e| !e.frames.is_empty()) {
                let _ = t.tx.send(TabCmd::RestoreFrames(e.frames));
            }
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
        log::debug!("key {key:?} text {text:?} modifiers {:?}", bw.modifiers);
        let m = bw.modifiers;
        let p = primary(m);
        let shift = m.shift_key();
        let alt = m.alt_key();
        let ch = |s: &str| matches!(key, Key::Character(c) if c.eq_ignore_ascii_case(s));
        if let Some(m) = bw.menu.as_mut() {
            match key {
                Key::Named(NamedKey::ArrowDown) => m.step(true),
                Key::Named(NamedKey::ArrowUp) => m.step(false),
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                    if let Some(a) = m.hover.and_then(|i| m.action(i)) {
                        bw.menu = None;
                        self.run_action(bw, el, a);
                    }
                }
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Tab) | Key::Named(NamedKey::Alt) => {
                    bw.menu = None
                }
                _ => {}
            }
            if bw.menu.is_none() {
                bw.refresh_hover();
            }
            return true;
        }
        // Context-menu key / Shift+F10: page context menu at the pointer.
        if matches!(key, Key::Named(NamedKey::ContextMenu))
            || (shift && matches!(key, Key::Named(NamedKey::F10)))
        {
            let at = bw.mouse;
            let tgt = if bw.url_focused { Target::UrlBar } else { Target::Content };
            bw.menu = self.context_menu(bw, tgt, at);
            if let Some(m) = bw.menu.as_mut() {
                m.step(true);
            }
            return true;
        }
        // Global shortcuts (Firefox-style; Cmd on macOS, Ctrl elsewhere).
        if (p && ch("l")) || (alt && ch("d")) || matches!(key, Key::Named(NamedKey::F6)) {
            if let Some(f) = bw.find.as_mut() {
                f.focused = false;
            }
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
            // Ctrl+Shift+R, Ctrl+F5 and Shift+F5 reload past the cache.
            bw.reload(shift || (p && matches!(key, Key::Named(NamedKey::F5))));
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
        if p && ch("f") && !shift {
            bw.open_find();
            return true;
        }
        if matches!(key, Key::Named(NamedKey::F3)) || (p && ch("g")) {
            if bw.find.is_none() {
                bw.open_find();
            } else {
                bw.send_find(if shift { -1 } else { 1 });
            }
            return true;
        }
        if bw.find.as_ref().is_some_and(|f| f.focused) {
            return self.find_key(bw, key, text);
        }
        if bw.url_focused {
            return self.url_key(bw, key, text);
        }
        if bw.find.is_some() && matches!(key, Key::Named(NamedKey::Escape)) {
            bw.close_find();
            return true;
        }
        // Keys for the focused element in the page.
        if self.page_key(bw, key, text) {
            return true;
        }
        // Page scrolling.
        let vh = bw.viewport().1;
        match key {
            Key::Named(NamedKey::ArrowDown) => bw.scroll_content(48.0, false),
            Key::Named(NamedKey::ArrowUp) => bw.scroll_content(-48.0, false),
            Key::Named(NamedKey::PageDown) => bw.scroll_content(vh * 0.85, false),
            Key::Named(NamedKey::PageUp) => bw.scroll_content(-vh * 0.85, false),
            Key::Named(NamedKey::Space) => {
                bw.scroll_content(if shift { -vh * 0.85 } else { vh * 0.85 }, false)
            }
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

    /// Route a key to the page when its focused element uses it (text editing, Space on a
    /// checkbox, arrows in a select, ...). Tab always goes to the page (focus navigation).
    /// Returns whether the key was sent.
    fn page_key(&mut self, bw: &mut BrowserWindow, key: &Key, text: Option<&str>) -> bool {
        let m = bw.modifiers;
        let p = primary(m);
        let mac = cfg!(target_os = "macos");
        let mods = KeyMods {
            shift: m.shift_key(),
            word: if mac { m.alt_key() } else { m.control_key() },
            primary: p,
        };
        let Some(t) = bw.tab() else { return false };
        let send = |cmd: TabCmd| {
            let _ = t.tx.send(cmd);
            true
        };
        if matches!(key, Key::Named(NamedKey::Tab)) && !p {
            return send(TabCmd::Key { key: PageKey::Tab, mods });
        }
        let Some(kind) = t.focus else { return false };
        let ch = |s: &str| matches!(key, Key::Character(c) if c.eq_ignore_ascii_case(s));
        let editing = matches!(kind, FocusKind::Text | FocusKind::TextArea);
        let pk = match key {
            Key::Named(NamedKey::Enter) => Some(PageKey::Enter),
            Key::Named(NamedKey::Space) => Some(PageKey::Space),
            Key::Named(NamedKey::Backspace) => Some(PageKey::Backspace),
            Key::Named(NamedKey::Delete) => Some(PageKey::Delete),
            Key::Named(NamedKey::ArrowLeft) => Some(PageKey::Left),
            Key::Named(NamedKey::ArrowRight) => Some(PageKey::Right),
            Key::Named(NamedKey::ArrowUp) => Some(PageKey::Up),
            Key::Named(NamedKey::ArrowDown) if m.alt_key() => Some(PageKey::Open),
            Key::Named(NamedKey::ArrowDown) => Some(PageKey::Down),
            Key::Named(NamedKey::Home) => Some(PageKey::Home),
            Key::Named(NamedKey::End) => Some(PageKey::End),
            _ if p && ch("a") => Some(PageKey::SelectAll),
            _ if p && ch("z") && m.shift_key() => Some(PageKey::Redo),
            _ if p && (ch("z")) => Some(PageKey::Undo),
            _ if p && ch("y") && !mac => Some(PageKey::Redo),
            _ if p && ch("c") => Some(PageKey::Copy),
            _ if p && ch("x") => Some(PageKey::Cut),
            _ => None,
        };
        if editing {
            if p && ch("v") {
                let clip = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok());
                return match clip {
                    Some(c) => send(TabCmd::Text(c)),
                    None => true,
                };
            }
            // Printable text (incl. Space) is inserted; Enter etc. are keys.
            let printable = text.filter(|t| !t.is_empty() && !t.chars().any(char::is_control));
            if let (Some(t), false) = (printable, p || (m.control_key() && !mac)) {
                return send(TabCmd::Text(t.to_string()));
            }
            let wanted = match pk {
                Some(PageKey::Up | PageKey::Down) => kind == FocusKind::TextArea,
                Some(PageKey::Space | PageKey::Open) => false,
                Some(_) => true,
                None => false,
            };
            return wanted && send(TabCmd::Key { key: pk.unwrap_or(PageKey::Escape), mods });
        }
        let wanted = match (kind, pk) {
            (_, None) => false,
            (FocusKind::Select, Some(k)) => matches!(
                k,
                PageKey::Space
                    | PageKey::Enter
                    | PageKey::Open
                    | PageKey::Up
                    | PageKey::Down
                    | PageKey::Left
                    | PageKey::Right
                    | PageKey::Home
                    | PageKey::End
            ),
            (FocusKind::Radio, Some(k)) => {
                matches!(k, PageKey::Space | PageKey::Up | PageKey::Down | PageKey::Left | PageKey::Right)
            }
            (FocusKind::Checkbox, Some(k)) => k == PageKey::Space,
            (FocusKind::Button | FocusKind::Summary, Some(k)) => matches!(k, PageKey::Space | PageKey::Enter),
            (FocusKind::Link, Some(k)) => k == PageKey::Enter,
            _ => false,
        };
        wanted && pk.is_some_and(|k| send(TabCmd::Key { key: k, mods }))
    }

    fn find_key(&mut self, bw: &mut BrowserWindow, key: &Key, text: Option<&str>) -> bool {
        let m = bw.modifiers;
        match key {
            Key::Named(NamedKey::Enter) => bw.send_find(if m.shift_key() { -1 } else { 1 }),
            Key::Named(NamedKey::Escape) => bw.close_find(),
            _ => {
                let changed = bw.find.as_mut().is_some_and(|f| edit_field(&mut f.field, m, key, text));
                if changed {
                    bw.send_find(0);
                }
            }
        }
        true
    }

    fn url_key(&mut self, bw: &mut BrowserWindow, key: &Key, text: Option<&str>) -> bool {
        let m = bw.modifiers;
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
            // Tab from the address bar moves into the page (its first focusable element).
            Key::Named(NamedKey::Tab) if !m.shift_key() && !primary(m) => {
                bw.url_focused = false;
                bw.sync_url_field();
                if let Some(t) = bw.tab() {
                    let mods = KeyMods::default();
                    let _ = t.tx.send(TabCmd::Key { key: PageKey::Tab, mods });
                }
                return true;
            }
            _ => {
                edit_field(&mut bw.url, m, key, text);
            }
        }
        false
    }
}

/// Shared text-field editing keys (URL bar, find bar). Returns whether the text changed.
fn edit_field(field: &mut TextField, m: ModifiersState, key: &Key, text: Option<&str>) -> bool {
    let p = primary(m);
    let shift = m.shift_key();
    let word = if cfg!(target_os = "macos") { m.alt_key() } else { m.control_key() };
    let before = field.text.clone();
    match key {
        Key::Named(NamedKey::Backspace) => field.backspace(),
        Key::Named(NamedKey::Delete) => field.delete(),
        Key::Named(NamedKey::ArrowLeft) => field.left(word, shift),
        Key::Named(NamedKey::ArrowRight) => field.right(word, shift),
        Key::Named(NamedKey::Home) => field.home(shift),
        Key::Named(NamedKey::End) => field.end(shift),
        Key::Character(c) if p && c.eq_ignore_ascii_case("a") => field.select_all(),
        Key::Character(c) if p && c.eq_ignore_ascii_case("z") => {
            if shift {
                field.redo()
            } else {
                field.undo()
            }
        }
        Key::Character(c) if p && (c.eq_ignore_ascii_case("c") || c.eq_ignore_ascii_case("x")) => {
            if let Some(sel) = field.selected_text().map(str::to_string) {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(sel);
                }
                if c.eq_ignore_ascii_case("x") {
                    field.backspace();
                }
            }
        }
        Key::Character(c) if p && c.eq_ignore_ascii_case("v") => {
            if let Ok(mut cb) = arboard::Clipboard::new() {
                if let Ok(t) = cb.get_text() {
                    field.insert(&t.replace(['\n', '\r'], " "));
                }
            }
        }
        _ => {
            if !p {
                if let Some(t) = text {
                    field.insert(t);
                }
            }
        }
    }
    field.text != before
}
