//! `bb-text` — fonts and text: system font discovery (`fontdb`), CSS font
//! matching with generic-family mapping, per-character fallback, shaping
//! (`rustybuzz`), glyph outlines, and UAX #14 line-break opportunities.
//!
//! Font files are loaded once and kept for the life of the process (a font
//! cache that never shrinks), which lets shaped faces borrow `'static` data.

pub mod edit;

use rustc_hash::FxHashMap;
use rustybuzz::ttf_parser;
use std::sync::{Arc, Mutex, OnceLock};

/// Index of a loaded font face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontId(pub u32);

/// Font metrics as fractions of the em size (multiply by px size).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub ascent: f32,
    /// Positive distance below the baseline.
    pub descent: f32,
    pub line_gap: f32,
    pub x_height: f32,
    pub cap_height: f32,
    pub underline_offset: f32,
    pub underline_thickness: f32,
    pub strikeout_offset: f32,
}

impl Metrics {
    /// CSS `line-height: normal` for a given size.
    pub fn normal_line_height(&self, size: f32) -> f32 {
        (self.ascent + self.descent + self.line_gap) * size
    }
}

struct Face {
    data: &'static [u8],
    index: u32,
    hb: rustybuzz::Face<'static>,
    upem: f32,
    metrics: Metrics,
    family: String,
    weight: u16,
    italic: bool,
    has_color_bitmaps: bool,
}

/// One positioned glyph of shaped text (px units).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Glyph {
    pub font: FontId,
    pub id: u16,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    /// Byte offset of the cluster in the shaped string.
    pub cluster: u32,
}

/// Shaped text.
#[derive(Debug, Clone, Default)]
pub struct ShapedText {
    pub glyphs: Vec<Glyph>,
    pub width: f32,
    /// Strong direction of the text: Some(true) = right-to-left, None = neutral only.
    pub rtl: Option<bool>,
    /// Synthesize bold (font lacks the requested weight).
    pub synthetic_bold: bool,
    /// Synthesize oblique (font lacks an italic face).
    pub synthetic_italic: bool,
}

/// A resolved font list: primary face first, then the CSS fallbacks.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontList {
    pub faces: Arc<[FontId]>,
    pub weight: u16,
    pub italic: bool,
}

impl FontList {
    pub fn primary(&self) -> FontId {
        self.faces[0]
    }
}

/// An outline command in font units (y up).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutlineCmd {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    QuadTo(f32, f32, f32, f32),
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// A glyph outline scaled to px (y down, origin at the pen position on the baseline).
#[derive(Debug, Clone, Default)]
pub struct Outline {
    pub cmds: Vec<OutlineCmd>,
}

/// An embedded bitmap glyph (e.g. color emoji), PNG-encoded.
#[derive(Debug, Clone)]
pub struct RasterGlyph {
    pub png: Vec<u8>,
    /// Placement relative to the pen position, in px at the requested size.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

struct OutlineSink {
    cmds: Vec<OutlineCmd>,
    scale: f32,
}

impl ttf_parser::OutlineBuilder for OutlineSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.cmds.push(OutlineCmd::MoveTo(x * self.scale, -y * self.scale));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.cmds.push(OutlineCmd::LineTo(x * self.scale, -y * self.scale));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let s = self.scale;
        self.cmds.push(OutlineCmd::QuadTo(x1 * s, -y1 * s, x * s, -y * s));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let s = self.scale;
        self.cmds.push(OutlineCmd::CubicTo(x1 * s, -y1 * s, x2 * s, -y2 * s, x * s, -y * s));
    }
    fn close(&mut self) {
        self.cmds.push(OutlineCmd::Close);
    }
}

/// Candidate families for CSS generic families and common legacy names, in
/// preference order across Windows, macOS and Linux (first installed wins).
fn generic_candidates(name: &str) -> &'static [&'static str] {
    match name {
        "serif" | "ui-serif" => &[
            "Times New Roman",
            "Times",
            "Liberation Serif",
            "Tinos",
            "DejaVu Serif",
            "Noto Serif",
            "Nimbus Roman",
            "FreeSerif",
        ],
        "sans-serif" | "ui-sans-serif" => &[
            "Arial",
            "Helvetica",
            "Liberation Sans",
            "Arimo",
            "DejaVu Sans",
            "Noto Sans",
            "Nimbus Sans",
            "FreeSans",
        ],
        "monospace" | "ui-monospace" => &[
            "Courier New",
            "Consolas",
            "Menlo",
            "Liberation Mono",
            "Cousine",
            "DejaVu Sans Mono",
            "Noto Sans Mono",
            "Noto Mono",
            "FreeMono",
        ],
        "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-rounded" => &[
            "Segoe UI Variable Text",
            "Segoe UI Variable",
            "Segoe UI",
            "SF Pro Text",
            "SF Pro",
            ".SF NS",
            "Helvetica Neue",
            "Inter",
            "Cantarell",
            "Ubuntu",
            "Noto Sans",
            "DejaVu Sans",
        ],
        "cursive" => &["Comic Sans MS", "Apple Chancery", "URW Chancery L", "DejaVu Sans"],
        "fantasy" => &["Impact", "Papyrus", "DejaVu Sans"],
        "emoji" => &["Segoe UI Emoji", "Apple Color Emoji", "Noto Color Emoji"],
        "math" => &["Cambria Math", "STIX Two Math", "DejaVu Serif"],
        _ => &[],
    }
}

/// Metric-compatible substitutes for classic web fonts (fontconfig-style aliases).
fn alias_candidates(name_lower: &str) -> &'static [&'static str] {
    match name_lower {
        "times new roman" | "times" | "times roman" => {
            &["Liberation Serif", "Tinos", "Nimbus Roman", "DejaVu Serif"]
        }
        "arial" | "helvetica" | "arial narrow" | "helvetica neue" => {
            &["Liberation Sans", "Arimo", "Nimbus Sans", "DejaVu Sans"]
        }
        "courier new" | "courier" => &["Liberation Mono", "Cousine", "Nimbus Mono PS", "DejaVu Sans Mono"],
        "verdana"
        | "tahoma"
        | "trebuchet ms"
        | "lucida grande"
        | "lucida sans unicode"
        | "segoe ui"
        | "geneva" => &["DejaVu Sans", "Liberation Sans", "Noto Sans"],
        "georgia" | "palatino" | "palatino linotype" | "book antiqua" | "garamond" => {
            &["DejaVu Serif", "Liberation Serif", "C059"]
        }
        "calibri" => &["Carlito", "Liberation Sans"],
        "cambria" => &["Caladea", "Liberation Serif"],
        "comic sans ms" | "comic sans" => &["DejaVu Sans"],
        "consolas" | "monaco" | "lucida console" | "andale mono" => &["DejaVu Sans Mono", "Liberation Mono"],
        _ => &[],
    }
}

fn is_generic(name: &str) -> bool {
    !generic_candidates(name).is_empty()
}

/// Characters that never trigger font fallback (they follow the previous font).
fn is_follower(c: char) -> bool {
    let u = c as u32;
    c.is_whitespace()
        || c.is_control()
        || matches!(u, 0x200B..=0x200F | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F | 0x1F3FB..=0x1F3FF)
}

fn is_emoji_presentation(c: char) -> bool {
    matches!(c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0x1F000..=0x1F2FF | 0x2B50 | 0x2B55 | 0x231A | 0x231B | 0x23E9..=0x23F3)
}

/// The process-wide font system.
pub struct FontSystem {
    db: fontdb::Database,
    faces: Vec<Face>,
    by_db_id: FxHashMap<fontdb::ID, FontId>,
    list_cache: FxHashMap<(Vec<String>, u16, bool), FontList>,
    fallback_cache: FxHashMap<(char, u16, bool), Option<FontId>>,
    shape_cache: FxHashMap<(String, Arc<[FontId]>, u32, u16, bool), Arc<ShapedText>>,
    outline_cache: FxHashMap<(FontId, u16), Arc<Outline>>,
    /// Families registered from web fonts (`@font-face`), name → faces.
    web_fonts: FxHashMap<String, Vec<FontId>>,
}

static GLOBAL: OnceLock<Mutex<FontSystem>> = OnceLock::new();

impl FontSystem {
    /// The shared font system (loads system fonts on first use).
    pub fn global() -> &'static Mutex<FontSystem> {
        GLOBAL.get_or_init(|| Mutex::new(FontSystem::new()))
    }

    /// Make the shared font system use **only** the fonts in `dir` (no system fonts), so text
    /// renders identically on every OS. Used by the snapshot tests with the bundled
    /// `tests/fonts/`. Returns false if the shared font system was already in use (then
    /// nothing changes).
    pub fn use_only_fonts_from(dir: &std::path::Path) -> bool {
        let mut fresh = false;
        GLOBAL.get_or_init(|| {
            fresh = true;
            Mutex::new(FontSystem::from_dir(dir))
        });
        fresh
    }

    /// A font system with only the font files in `dir`, loaded in file-name order so that
    /// font matching and fallback are deterministic.
    pub fn from_dir(dir: &std::path::Path) -> FontSystem {
        let mut db = fontdb::Database::new();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
            .unwrap_or_default();
        files.retain(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc"))
        });
        files.sort();
        for f in files {
            if let Err(e) = db.load_font_file(&f) {
                log::warn!("can't load {}: {e}", f.display());
            }
        }
        Self::with_db(db)
    }

    /// Lock the global font system, recovering from a poisoned lock.
    pub fn lock() -> std::sync::MutexGuard<'static, FontSystem> {
        match Self::global().lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        }
    }

    pub fn new() -> FontSystem {
        let mut db = fontdb::Database::new();
        let t0 = std::time::Instant::now();
        db.load_system_fonts();
        // Bundled-font directory for tests / portable installs.
        if let Ok(dir) = std::env::var("BB_FONT_DIR") {
            db.load_fonts_dir(dir);
        }
        log::info!("font database: {} faces in {:?}", db.len(), t0.elapsed());
        Self::with_db(db)
    }

    fn with_db(db: fontdb::Database) -> FontSystem {
        FontSystem {
            db,
            faces: Vec::new(),
            by_db_id: FxHashMap::default(),
            list_cache: FxHashMap::default(),
            fallback_cache: FxHashMap::default(),
            shape_cache: FxHashMap::default(),
            outline_cache: FxHashMap::default(),
            web_fonts: FxHashMap::default(),
        }
    }

    pub fn face_count(&self) -> usize {
        self.db.len()
    }

    fn load(&mut self, id: fontdb::ID) -> Option<FontId> {
        if let Some(f) = self.by_db_id.get(&id) {
            return Some(*f);
        }
        let info = self.db.face(id)?;
        let (family, weight, italic) = (
            info.families.first().map(|f| f.0.clone()).unwrap_or_default(),
            info.weight.0,
            info.style != fontdb::Style::Normal,
        );
        let index = info.index;
        let bytes: Vec<u8> = match &info.source {
            fontdb::Source::Binary(b) => (**b).as_ref().to_vec(),
            fontdb::Source::File(p) => std::fs::read(p).ok()?,
        };
        self.register_bytes(bytes, index, family, weight, italic).inspect(|f| {
            self.by_db_id.insert(id, *f);
        })
    }

    fn register_bytes(
        &mut self,
        bytes: Vec<u8>,
        index: u32,
        family: String,
        weight: u16,
        italic: bool,
    ) -> Option<FontId> {
        // Intentionally leaked: the font cache lives for the whole process.
        let data: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        let hb = rustybuzz::Face::from_slice(data, index)?;
        let upem = hb.units_per_em() as f32;
        let m = |v: i16| v as f32 / upem;
        let ul = hb.underline_metrics();
        let st = hb.strikeout_metrics();
        let ascent = m(hb.ascender());
        let descent = -m(hb.descender());
        let metrics = Metrics {
            ascent,
            descent,
            line_gap: m(hb.line_gap()),
            x_height: hb.x_height().map(m).unwrap_or(ascent * 0.5),
            cap_height: hb.capital_height().map(m).unwrap_or(ascent * 0.7),
            underline_offset: ul.map(|u| -m(u.position)).unwrap_or(0.1),
            underline_thickness: ul.map(|u| m(u.thickness)).unwrap_or(0.05).max(0.04),
            strikeout_offset: st.map(|s| m(s.position)).unwrap_or(ascent * 0.3),
        };
        let has_color_bitmaps = hb.tables().cbdt.is_some() || hb.tables().sbix.is_some();
        let id = FontId(self.faces.len() as u32);
        self.faces.push(Face { data, index, hb, upem, metrics, family, weight, italic, has_color_bitmaps });
        Some(id)
    }

    /// Register a downloaded web font (`@font-face`). TTF/OTF only for now
    /// (TODO(css-fonts-4 §4.3): WOFF/WOFF2 decoding).
    pub fn register_web_font(
        &mut self,
        family: &str,
        bytes: Vec<u8>,
        weight: u16,
        italic: bool,
    ) -> Option<FontId> {
        let id = self.register_bytes(bytes, 0, family.to_string(), weight, italic)?;
        self.web_fonts.entry(family.to_ascii_lowercase()).or_default().push(id);
        self.list_cache.clear();
        self.shape_cache.clear();
        Some(id)
    }

    fn query(&mut self, family: &str, weight: u16, italic: bool) -> Option<FontId> {
        if let Some(faces) = self.web_fonts.get(&family.to_ascii_lowercase()) {
            let faces = faces.clone();
            return faces
                .iter()
                .min_by_key(|f| {
                    let face = &self.faces[f.0 as usize];
                    (face.italic != italic) as i32 * 1000 + (face.weight as i32 - weight as i32).abs()
                })
                .copied();
        }
        let style = if italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
        let q = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: fontdb::Weight(weight),
            stretch: fontdb::Stretch::Normal,
            style,
        };
        let id = self.db.query(&q)?;
        self.load(id)
    }

    /// Resolve a CSS `font-family` list into loaded faces.
    pub fn resolve(&mut self, families: &[String], weight: u16, italic: bool) -> FontList {
        let key = (families.to_vec(), weight, italic);
        if let Some(l) = self.list_cache.get(&key) {
            return l.clone();
        }
        let mut faces = Vec::new();
        let push = |f: Option<FontId>, faces: &mut Vec<FontId>| {
            if let Some(f) = f {
                if !faces.contains(&f) {
                    faces.push(f);
                }
            }
        };
        let mut saw_generic = false;
        for fam in families {
            let lower = fam.to_ascii_lowercase();
            if is_generic(&lower) {
                saw_generic = true;
                for c in generic_candidates(&lower) {
                    if let Some(f) = self.query(c, weight, italic) {
                        push(Some(f), &mut faces);
                        break;
                    }
                }
            } else {
                let direct = self.query(fam, weight, italic);
                if direct.is_some() {
                    push(direct, &mut faces);
                } else {
                    for c in alias_candidates(&lower) {
                        if let Some(f) = self.query(c, weight, italic) {
                            push(Some(f), &mut faces);
                            break;
                        }
                    }
                }
            }
        }
        if !saw_generic || faces.is_empty() {
            // Browser default: serif.
            for c in generic_candidates("serif") {
                if let Some(f) = self.query(c, weight, italic) {
                    push(Some(f), &mut faces);
                    break;
                }
            }
        }
        if faces.is_empty() {
            // Anything at all.
            let any = self.db.faces().next().map(|f| f.id);
            if let Some(id) = any {
                push(self.load(id), &mut faces);
            }
        }
        if faces.is_empty() {
            log::error!("no usable fonts found on this system");
        }
        let list = FontList { faces: faces.into(), weight, italic };
        self.list_cache.insert(key, list.clone());
        list
    }

    pub fn metrics(&self, f: FontId) -> Metrics {
        self.faces.get(f.0 as usize).map(|f| f.metrics).unwrap_or(Metrics {
            ascent: 0.8,
            descent: 0.2,
            line_gap: 0.0,
            x_height: 0.5,
            cap_height: 0.7,
            underline_offset: 0.1,
            underline_thickness: 0.05,
            strikeout_offset: 0.3,
        })
    }

    pub fn family_name(&self, f: FontId) -> &str {
        self.faces.get(f.0 as usize).map(|f| f.family.as_str()).unwrap_or("")
    }

    pub fn has_glyph(&self, f: FontId, c: char) -> bool {
        self.faces.get(f.0 as usize).is_some_and(|face| face.hb.glyph_index(c).is_some_and(|g| g.0 != 0))
    }

    /// Search every installed face for one that covers `c`.
    fn fallback_for(&mut self, c: char, weight: u16, italic: bool) -> Option<FontId> {
        let key = (c, weight, italic);
        if let Some(r) = self.fallback_cache.get(&key) {
            return *r;
        }
        // Prefer emoji fonts for emoji.
        let mut found = None;
        if is_emoji_presentation(c) {
            for name in generic_candidates("emoji") {
                if let Some(f) = self.query(name, 400, false) {
                    if self.has_glyph(f, c) {
                        found = Some(f);
                        break;
                    }
                }
            }
        }
        if found.is_none() {
            // Scan the database, preferring proportional (non-monospace) faces
            // close to the requested weight/style, and regular "Sans" families.
            let mut ordered: Vec<(fontdb::ID, i32)> = self
                .db
                .faces()
                .map(|f| {
                    let fam = f.families.first().map(|x| x.0.to_ascii_lowercase()).unwrap_or_default();
                    let italic_mismatch = (f.style != fontdb::Style::Normal) != italic;
                    let mut score = (f.weight.0 as i32 - weight as i32).abs();
                    score += if italic_mismatch { 1000 } else { 0 };
                    score += if f.monospaced || fam.contains("mono") { 5000 } else { 0 };
                    score += if fam.contains("sans") { 0 } else { 50 };
                    score += if fam.contains("looped") || fam.contains("unjoined") || fam.contains("display")
                    {
                        300
                    } else {
                        0
                    };
                    (f.id, score)
                })
                .collect();
            ordered.sort_by_key(|(_, sc)| *sc);
            for (id, _) in ordered {
                let covers = self.db.with_face_data(id, |data, index| {
                    ttf_parser::Face::parse(data, index)
                        .ok()
                        .and_then(|f| f.glyph_index(c))
                        .is_some_and(|g| g.0 != 0)
                });
                if covers == Some(true) {
                    found = self.load(id);
                    if found.is_some() {
                        break;
                    }
                }
            }
        }
        self.fallback_cache.insert(key, found);
        found
    }

    /// Shape `text` with `list` at `size` px, with per-character fallback.
    pub fn shape(&mut self, text: &str, list: &FontList, size: f32) -> Arc<ShapedText> {
        let key = (text.to_string(), list.faces.clone(), size.to_bits(), list.weight, list.italic);
        if let Some(s) = self.shape_cache.get(&key) {
            return s.clone();
        }
        let shaped = Arc::new(self.shape_uncached(text, list, size));
        if self.shape_cache.len() > 50_000 {
            self.shape_cache.clear();
        }
        self.shape_cache.insert(key, shaped.clone());
        shaped
    }

    fn shape_uncached(&mut self, text: &str, list: &FontList, size: f32) -> ShapedText {
        let mut out = ShapedText::default();
        if text.is_empty() || list.faces.is_empty() {
            return out;
        }
        out.rtl = strong_direction(text);
        // Segment by font.
        let mut segments: Vec<(usize, usize, FontId)> = Vec::new();
        let mut cur: Option<(usize, FontId)> = None;
        for (i, c) in text.char_indices() {
            let font = if is_follower(c) {
                cur.map(|(_, f)| f).unwrap_or(list.faces[0])
            } else {
                let mut f = list.faces.iter().copied().find(|&f| self.has_glyph(f, c));
                if f.is_none() {
                    f = self.fallback_for(c, list.weight, list.italic);
                }
                f.unwrap_or(list.faces[0])
            };
            match cur {
                Some((start, cf)) if cf != font => {
                    segments.push((start, i, cf));
                    cur = Some((i, font));
                }
                None => cur = Some((i, font)),
                _ => {}
            }
        }
        if let Some((start, f)) = cur {
            segments.push((start, text.len(), f));
        }
        let primary = list.faces[0];
        for (start, end, font) in segments {
            let face = &self.faces[font.0 as usize];
            let scale = size / face.upem;
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(&text[start..end]);
            buf.guess_segment_properties();
            let shaped = rustybuzz::shape(&face.hb, &[], buf);
            for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                let g = Glyph {
                    font,
                    id: info.glyph_id as u16,
                    x_advance: pos.x_advance as f32 * scale,
                    x_offset: pos.x_offset as f32 * scale,
                    y_offset: -pos.y_offset as f32 * scale,
                    cluster: info.cluster + start as u32,
                };
                out.width += g.x_advance;
                out.glyphs.push(g);
            }
            if font == primary {
                out.synthetic_bold |= list.weight >= 600 && face.weight < 550;
                out.synthetic_italic |= list.italic && !face.italic;
            }
        }
        out
    }

    /// Glyph outline scaled to `size` px (y down).
    pub fn outline(&mut self, font: FontId, glyph: u16, size: f32) -> Option<Outline> {
        let unit = self.outline_unit(font, glyph)?;
        let s = size;
        let scale = |v: f32| v * s;
        Some(Outline {
            cmds: unit
                .cmds
                .iter()
                .map(|c| match *c {
                    OutlineCmd::MoveTo(x, y) => OutlineCmd::MoveTo(scale(x), scale(y)),
                    OutlineCmd::LineTo(x, y) => OutlineCmd::LineTo(scale(x), scale(y)),
                    OutlineCmd::QuadTo(a, b, x, y) => {
                        OutlineCmd::QuadTo(scale(a), scale(b), scale(x), scale(y))
                    }
                    OutlineCmd::CubicTo(a, b, c2, d, x, y) => {
                        OutlineCmd::CubicTo(scale(a), scale(b), scale(c2), scale(d), scale(x), scale(y))
                    }
                    OutlineCmd::Close => OutlineCmd::Close,
                })
                .collect(),
        })
    }

    /// Outline at 1px em (cached).
    fn outline_unit(&mut self, font: FontId, glyph: u16) -> Option<Arc<Outline>> {
        if let Some(o) = self.outline_cache.get(&(font, glyph)) {
            return Some(o.clone());
        }
        let face = self.faces.get(font.0 as usize)?;
        let mut sink = OutlineSink { cmds: Vec::new(), scale: 1.0 / face.upem };
        face.hb.outline_glyph(ttf_parser::GlyphId(glyph), &mut sink);
        let o = Arc::new(Outline { cmds: sink.cmds });
        self.outline_cache.insert((font, glyph), o.clone());
        Some(o)
    }

    /// Color bitmap glyph (CBDT/sbix), if the font has one.
    pub fn raster_glyph(&self, font: FontId, glyph: u16, size: f32) -> Option<RasterGlyph> {
        let face = self.faces.get(font.0 as usize)?;
        if !face.has_color_bitmaps {
            return None;
        }
        let img = face.hb.glyph_raster_image(ttf_parser::GlyphId(glyph), size.round().max(1.0) as u16)?;
        if img.format != ttf_parser::RasterImageFormat::PNG {
            return None;
        }
        let k = size / img.pixels_per_em as f32;
        Some(RasterGlyph {
            png: img.data.to_vec(),
            x: img.x as f32 * k,
            y: -(img.y as f32 + img.height as f32) * k,
            width: img.width as f32 * k,
            height: img.height as f32 * k,
        })
    }

    /// Whether any installed face covers `c` (independent of the fallback logic).
    pub fn any_face_covers(&self, c: char) -> bool {
        self.db.faces().any(|f| {
            self.db
                .with_face_data(f.id, |data, index| {
                    ttf_parser::Face::parse(data, index)
                        .ok()
                        .and_then(|face| face.glyph_index(c))
                        .is_some_and(|g| g.0 != 0)
                })
                .unwrap_or(false)
        })
    }

    /// Raw font data, for consumers that need it.
    pub fn face_data(&self, font: FontId) -> Option<(&'static [u8], u32)> {
        self.faces.get(font.0 as usize).map(|f| (f.data, f.index))
    }

    /// Advance width of a single character in the primary font (e.g. `ch` units).
    pub fn char_advance(&mut self, list: &FontList, size: f32, c: char) -> f32 {
        let mut b = [0u8; 4];
        self.shape(c.encode_utf8(&mut b), list, size).width
    }
}

/// The first strong bidi direction in `text` (UAX #9 P2): Some(true) for RTL.
pub fn strong_direction(text: &str) -> Option<bool> {
    use unicode_bidi::{bidi_class, BidiClass};
    for c in text.chars() {
        match bidi_class(c) {
            BidiClass::L => return Some(false),
            BidiClass::R | BidiClass::AL => return Some(true),
            _ => {}
        }
    }
    None
}

/// UAX #14 line-break opportunities: byte offsets *after which* a break may
/// occur, with `true` for mandatory breaks.
pub fn break_opportunities(text: &str) -> Vec<(usize, bool)> {
    unicode_linebreak::linebreaks(text)
        .map(|(i, op)| (i, op == unicode_linebreak::BreakOpportunity::Mandatory))
        .collect()
}

/// Grapheme cluster boundaries (byte offsets), for caret movement and wrapping fallbacks.
pub fn grapheme_boundaries(text: &str) -> Vec<usize> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut v: Vec<usize> = text.grapheme_indices(true).map(|(i, _)| i).collect();
    v.push(text.len());
    v
}

/// Word boundaries (byte offsets of word starts), for double-click selection.
pub fn word_at(text: &str, byte: usize) -> (usize, usize) {
    use unicode_segmentation::UnicodeSegmentation;
    for (i, w) in text.split_word_bound_indices() {
        if byte >= i && byte < i + w.len() {
            return (i, i + w.len());
        }
    }
    (byte, byte)
}

/// Resolve bidi embedding levels per byte (UAX #9) for a paragraph. Returns
/// runs of (start, end, rtl).
pub fn bidi_runs(text: &str, base_rtl: bool) -> Vec<(usize, usize, bool)> {
    use unicode_bidi::{BidiInfo, Level};
    let level = if base_rtl { Some(Level::rtl()) } else { Some(Level::ltr()) };
    let info = BidiInfo::new(text, level);
    let mut runs = Vec::new();
    for para in &info.paragraphs {
        let (levels, ranges) = info.visual_runs(para, para.range.clone());
        for r in ranges {
            let rtl = levels[r.start].is_rtl();
            runs.push((r.start, r.end, rtl));
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_generic_families() {
        let mut fs = FontSystem::lock();
        let l = fs.resolve(&["sans-serif".to_string()], 400, false);
        assert!(!l.faces.is_empty());
        let m = fs.metrics(l.primary());
        assert!(m.ascent > 0.5 && m.ascent < 1.2, "{m:?}");
        let bold = fs.resolve(&["serif".to_string()], 700, false);
        assert!(!bold.faces.is_empty());
    }

    #[test]
    fn shapes_and_falls_back() {
        let mut fs = FontSystem::lock();
        let l = fs.resolve(&["serif".to_string()], 400, false);
        let s = fs.shape("Hello", &l, 16.0);
        assert_eq!(s.glyphs.len(), 5);
        assert!(s.width > 20.0 && s.width < 60.0, "{}", s.width);
        // Thai and CJK must not render as .notdef when a suitable font is installed.
        for text in ["สวัสดี", "日本語"] {
            let covered = text.chars().all(|c| fs.any_face_covers(c));
            let shaped = fs.shape(text, &l, 16.0);
            let notdef = shaped.glyphs.iter().filter(|g| g.id == 0).count();
            if covered {
                assert_eq!(notdef, 0, "{text} fell back to tofu although a font covers it");
            }
        }
        let o = fs.outline(s.glyphs[0].font, s.glyphs[0].id, 16.0);
        assert!(o.is_some());
    }

    #[test]
    fn line_breaks() {
        let b = break_opportunities("hello world\nnext");
        assert!(b.contains(&(6, false)));
        assert!(b.contains(&(12, true)));
        assert_eq!(word_at("foo bar", 5), (4, 7));
        assert_eq!(bidi_runs("abc", false), vec![(0, 3, false)]);
    }
}
