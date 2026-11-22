//! The "About BannedBrowser" window: a small, non-resizable window drawn with `bb-ui`, opened
//! from the ≡ menu. It shows the logo and wordmark, the version and release name, an update
//! check against the GitHub Releases API, links (what's new, credits, license, source) and the
//! unofficial fan-project note. Esc (or Ctrl/Cmd+W) closes it; Tab moves between the links and
//! Enter or Space opens one.
//!
//! The update check is honest: it says "up to date" only after a successful answer, a
//! development build says it's a development build, and any failure says "Couldn't check for
//! updates" with a link to the releases page. Nothing is ever installed.

use bb_paint::{Canvas, Color, DisplayList, Rect, SoftwareCanvas};
use bb_ui::{Painter, Theme};
use bb_url::Url;
use std::num::NonZeroU32;
use std::rc::Rc;
use winit::window::Window;

/// Logical size of the window.
pub const SIZE: (f64, f64) = (500.0, 330.0);

/// The project's repository (from Cargo.toml), e.g. "https://github.com/owner/repo".
fn repository() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_end_matches('/')
}

/// The GitHub Releases API URL for the latest release. `BB_UPDATE_URL` overrides it (tests).
pub fn latest_release_api() -> String {
    if let Ok(u) = std::env::var("BB_UPDATE_URL") {
        return u;
    }
    let path = repository().trim_start_matches("https://github.com/");
    format!("https://api.github.com/repos/{path}/releases/latest")
}

pub fn releases_page() -> String {
    format!("{}/releases", repository())
}

/// "X.Y.Z" → (X, Y, Z), ignoring a leading "v" and any "-pre" suffix.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches('v').split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (it.next()??, it.next()??, it.next()??);
    if it.next().is_some() {
        return None;
    }
    Some(v)
}

/// The release name for this version from CHANGELOG.md (`## [X.Y.Z] — "Name" — date`), if it
/// has been released.
fn release_name() -> Option<String> {
    const CHANGELOG: &str = include_str!("../../../CHANGELOG.md");
    let heading = format!("## [{}]", env!("CARGO_PKG_VERSION"));
    let line = CHANGELOG.lines().find(|l| l.starts_with(&heading))?;
    let start = line.find('"')? + 1;
    let end = start + line[start..].find('"')?;
    Some(line[start..end].to_string())
}

fn is_dev_build() -> bool {
    env!("CARGO_PKG_VERSION").contains('-')
}

/// What the update check found.
#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    Checking,
    UpToDate,
    /// A newer release: its version and page.
    Newer {
        version: String,
        url: String,
    },
    /// This is a development build; `latest` is the newest release.
    DevBuild {
        latest: String,
    },
    /// The check failed (offline, rate-limited, not found, …).
    Failed,
}

impl UpdateStatus {
    /// Short form for the GUI tests' state file.
    pub fn code(&self) -> String {
        match self {
            UpdateStatus::Checking => "checking".into(),
            UpdateStatus::UpToDate => "up_to_date".into(),
            UpdateStatus::Newer { version, .. } => format!("newer:{version}"),
            UpdateStatus::DevBuild { latest } => format!("dev_build:{latest}"),
            UpdateStatus::Failed => "failed".into(),
        }
    }
}

/// Compare this build with the latest release (`tag` like "v0.3.0").
pub fn compare(current: &str, tag: &str, release_url: &str) -> UpdateStatus {
    let (Some(cur), Some(latest)) = (parse_version(current), parse_version(tag)) else {
        return UpdateStatus::Failed;
    };
    let latest_s = format!("{}.{}.{}", latest.0, latest.1, latest.2);
    let dev = current.contains('-');
    // A pre-release (X.Y.Z-dev) comes before the release X.Y.Z.
    if latest > cur || (dev && latest == cur) {
        UpdateStatus::Newer { version: latest_s, url: release_url.to_string() }
    } else if dev {
        UpdateStatus::DevBuild { latest: latest_s }
    } else {
        UpdateStatus::UpToDate
    }
}

/// Ask the Releases API for the latest release (blocking; run it on a worker thread).
pub fn check_for_updates(client: &bb_net::Client) -> UpdateStatus {
    let Ok(url) = Url::parse(&latest_release_api()) else { return UpdateStatus::Failed };
    let mut req = bb_net::Request::get(url);
    req.headers.set("Accept", "application/vnd.github+json");
    let Ok(resp) = client.fetch(req) else { return UpdateStatus::Failed };
    if resp.status != 200 {
        return UpdateStatus::Failed;
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&resp.body) else {
        return UpdateStatus::Failed;
    };
    let (Some(tag), Some(page)) = (json["tag_name"].as_str(), json["html_url"].as_str()) else {
        return UpdateStatus::Failed;
    };
    compare(env!("CARGO_PKG_VERSION"), tag, page)
}

/// Where a link in the window goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub id: &'static str,
    pub label: String,
    pub url: String,
}

pub struct AboutWindow {
    pub window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    canvas: SoftwareCanvas,
    pub theme: Theme,
    pub update: UpdateStatus,
    /// Keyboard focus: index into `links()`.
    pub focus: Option<usize>,
    hover: Option<usize>,
    /// Link hit rects from the last paint (logical px).
    link_rects: Vec<Rect>,
    pub dirty: bool,
}

impl AboutWindow {
    pub fn new(window: Rc<Window>, context: &softbuffer::Context<Rc<Window>>, dark: bool) -> Option<Self> {
        let surface = softbuffer::Surface::new(context, window.clone()).ok()?;
        let size = window.inner_size();
        Some(AboutWindow {
            window,
            surface,
            canvas: SoftwareCanvas::new(size.width.max(1), size.height.max(1)),
            theme: if dark { Theme::dark() } else { Theme::light() },
            update: UpdateStatus::Checking,
            focus: None,
            hover: None,
            link_rects: Vec::new(),
            dirty: true,
        })
    }

    /// The window's links, in Tab order.
    pub fn links(&self) -> Vec<Link> {
        let repo = repository();
        let mut v = Vec::new();
        match &self.update {
            UpdateStatus::Newer { version, url } => {
                v.push(Link { id: "update", label: format!("Get BannedBrowser {version}"), url: url.clone() })
            }
            UpdateStatus::Failed => {
                v.push(Link { id: "update", label: "Releases page".into(), url: releases_page() })
            }
            _ => {}
        }
        let whats_new = if is_dev_build() {
            format!("{repo}/blob/main/CHANGELOG.md")
        } else {
            format!("{repo}/releases/tag/v{}", env!("CARGO_PKG_VERSION"))
        };
        v.push(Link { id: "whats-new", label: "What's new".into(), url: whats_new });
        v.push(Link { id: "credits", label: "Credits".into(), url: "about:bannedbrowser".into() });
        v.push(Link { id: "license", label: "License".into(), url: format!("{repo}#license") });
        v.push(Link { id: "source", label: "Source code".into(), url: repo.to_string() });
        v
    }

    pub fn set_update(&mut self, status: UpdateStatus) {
        self.update = status;
        self.focus = None;
        self.dirty = true;
        self.window.request_redraw();
    }

    /// Tab / Shift+Tab.
    pub fn move_focus(&mut self, back: bool) {
        let n = self.links().len();
        if n == 0 {
            return;
        }
        self.focus = Some(match (self.focus, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        });
        self.dirty = true;
        self.window.request_redraw();
    }

    /// The link under the pointer.
    pub fn hovered(&self) -> Option<usize> {
        self.hover
    }

    /// The link at a point (logical px), if any.
    pub fn link_at(&self, x: f32, y: f32) -> Option<usize> {
        self.link_rects.iter().position(|r| bb_ui::hit(r, x, y))
    }

    pub fn set_hover(&mut self, x: f32, y: f32) -> bool {
        let h = self.link_at(x, y);
        if h != self.hover {
            self.hover = h;
            self.dirty = true;
            self.window.request_redraw();
        }
        h.is_some()
    }

    fn display_list(&mut self) -> DisplayList {
        let th = self.theme;
        let (w, h) = (SIZE.0 as f32, SIZE.1 as f32);
        let links = self.links();
        let mut dl = DisplayList::default();
        let mut rects = Vec::new();
        {
            let mut p = Painter::new(&mut dl);
            p.rect(Rect::new(0.0, 0.0, w, h), th.toolbar);
            // Logo: the pixel "B" on a 16×16 grid.
            let (lx, ly, cell) = (32.0, 34.0, 4.5);
            for (cx, cy, c) in bb_ui::icons::logo_cells() {
                let color = match c {
                    0 => bb_ui::ACCENT,
                    1 => bb_ui::GLITCH,
                    _ => Color::WHITE,
                };
                p.rect(Rect::new(lx + cx as f32 * cell, ly + cy as f32 * cell, cell, cell), color);
            }
            // Wordmark, version, release name.
            let x = 128.0;
            let after = p.text("Banned", x, 62.0, 28.0, 700, th.text);
            p.text("Browser", x + after, 62.0, 28.0, 700, bb_ui::ACCENT);
            let bits = if cfg!(target_pointer_width = "64") { "64-bit" } else { "32-bit" };
            p.text(
                &format!("Version {} ({bits})", env!("CARGO_PKG_VERSION")),
                x,
                88.0,
                13.0,
                400,
                th.text_muted,
            );
            let name = match release_name() {
                Some(n) => format!("\u{201C}{n}\u{201D}"),
                None if is_dev_build() => "Development build".into(),
                None => String::new(),
            };
            p.text(&name, x, 108.0, 13.0, 400, th.text_muted);
            // Update status.
            let status = match &self.update {
                UpdateStatus::Checking => "Checking for updates…".to_string(),
                UpdateStatus::UpToDate => "BannedBrowser is up to date".to_string(),
                UpdateStatus::Newer { version, .. } => format!("Version {version} is available."),
                UpdateStatus::DevBuild { latest } => {
                    format!("This is a development build. The latest release is {latest}.")
                }
                UpdateStatus::Failed => "Couldn't check for updates.".to_string(),
            };
            let mut sx = x + p.text(&status, x, 140.0, 13.0, 600, th.text) + 8.0;
            let mut li = 0;
            if matches!(self.update, UpdateStatus::Newer { .. } | UpdateStatus::Failed) {
                sx += self.link(&mut p, &links[0], sx, 140.0, li, &th, &mut rects);
                li += 1;
            }
            let _ = sx;
            // Links row.
            let mut lx = x;
            for (i, l) in links.iter().enumerate().skip(li) {
                lx += self.link(&mut p, l, lx, 178.0, i, &th, &mut rects) + 18.0;
            }
            // Fan-project note.
            p.rect(Rect::new(32.0, 214.0, w - 64.0, 1.0), th.separator);
            for (i, line) in [
                "BannedBrowser is an unofficial fan-named project and is not affiliated with,",
                "endorsed by, or sponsored by HoYoverse or Mozilla. Honkai: Star Rail and",
                "Silver Wolf are trademarks of their respective owners.",
            ]
            .iter()
            .enumerate()
            {
                p.text(line, 32.0, 240.0 + i as f32 * 18.0, 12.0, 400, th.text_muted);
            }
            p.text("MIT OR Apache-2.0 · press Esc to close", 32.0, 306.0, 12.0, 400, th.text_muted);
        }
        self.link_rects = rects;
        dl
    }

    /// Draw one link; returns its width.
    fn link(
        &self,
        p: &mut Painter,
        l: &Link,
        x: f32,
        baseline: f32,
        index: usize,
        th: &Theme,
        rects: &mut Vec<Rect>,
    ) -> f32 {
        let w = bb_ui::text::measure(&l.label, 13.0, 600);
        let r = Rect::new(x - 4.0, baseline - 15.0, w + 8.0, 21.0);
        if self.focus == Some(index) {
            // Focus ring.
            p.rounded(r, 4.0, th.accent);
            p.rounded(Rect::new(r.x + 2.0, r.y + 2.0, r.w - 4.0, r.h - 4.0), 3.0, th.toolbar);
        }
        let hot = self.hover == Some(index) || self.focus == Some(index);
        p.text(&l.label, x, baseline, 13.0, 600, th.accent);
        if hot {
            p.rect(Rect::new(x, baseline + 2.0, w, 1.0), th.accent);
        }
        rects.push(r);
        w
    }

    pub fn redraw(&mut self) {
        let size = self.window.inner_size();
        let (pw, ph) = (size.width.max(1), size.height.max(1));
        if self.canvas.w != pw || self.canvas.h != ph {
            self.canvas = SoftwareCanvas::new(pw, ph);
        }
        let scale = self.window.scale_factor() as f32;
        let dl = self.display_list();
        self.canvas.clear(self.theme.toolbar);
        bb_paint::render(&dl, &mut self.canvas, (0.0, 0.0), scale);
        let (Some(w), Some(h)) = (NonZeroU32::new(pw), NonZeroU32::new(ph)) else { return };
        if self.surface.resize(w, h).is_err() {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("0.2.0-dev"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert_eq!(parse_version("nope"), None);
    }

    #[test]
    fn comparison_is_honest() {
        let u = "https://example/r";
        assert_eq!(compare("0.1.0", "v0.1.0", u), UpdateStatus::UpToDate);
        assert_eq!(
            compare("0.1.0", "v0.2.0", u),
            UpdateStatus::Newer { version: "0.2.0".into(), url: u.into() }
        );
        // A development build is older than its own release, newer than earlier ones.
        assert_eq!(
            compare("0.2.0-dev", "v0.2.0", u),
            UpdateStatus::Newer { version: "0.2.0".into(), url: u.into() }
        );
        assert_eq!(compare("0.2.0-dev", "v0.1.0", u), UpdateStatus::DevBuild { latest: "0.1.0".into() });
        // Garbage never reads as "up to date".
        assert_eq!(compare("0.2.0", "latest", u), UpdateStatus::Failed);
    }

    #[test]
    fn api_url_comes_from_the_repository() {
        if std::env::var_os("BB_UPDATE_URL").is_none() {
            let api = latest_release_api();
            assert!(api.starts_with("https://api.github.com/repos/"), "{api}");
            assert!(api.ends_with("/releases/latest"), "{api}");
        }
    }
}
