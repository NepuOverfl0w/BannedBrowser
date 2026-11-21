//! Computed value types and the [`ComputedStyle`] struct.

use bb_css::calc::Calc;
use bb_css::Rgba;
use std::rc::Rc;

/// `<length-percentage>` with lengths already resolved to px.
#[derive(Debug, Clone, PartialEq)]
pub enum LP {
    Px(f32),
    Pct(f32),
    /// calc() with length leaves resolved to px numbers.
    Calc(Rc<Calc>),
}

impl LP {
    pub const ZERO: LP = LP::Px(0.0);

    /// Resolve against a percentage basis.
    pub fn resolve(&self, base: f32) -> f32 {
        match self {
            LP::Px(v) => *v,
            LP::Pct(p) => p / 100.0 * base,
            LP::Calc(c) => c.eval_px(&PxOnly, base),
        }
    }

    pub fn is_zero(&self) -> bool {
        matches!(self, LP::Px(v) if *v == 0.0)
    }

    pub fn has_percent(&self) -> bool {
        match self {
            LP::Pct(_) => true,
            LP::Calc(c) => c.has_percent(),
            LP::Px(_) => false,
        }
    }
}

pub(crate) struct PxOnly;
impl bb_css::calc::LengthResolver for PxOnly {
    fn length_px(&self, l: &bb_css::Length) -> f32 {
        l.to_px_static(16.0).unwrap_or(0.0)
    }
}

/// `<length-percentage> | auto`.
#[derive(Debug, Clone, PartialEq)]
pub enum LPA {
    LP(LP),
    Auto,
}

impl LPA {
    pub const ZERO: LPA = LPA::LP(LP::Px(0.0));
    pub fn is_auto(&self) -> bool {
        matches!(self, LPA::Auto)
    }
    pub fn resolve(&self, base: f32) -> Option<f32> {
        match self {
            LPA::LP(lp) => Some(lp.resolve(base)),
            LPA::Auto => None,
        }
    }
    pub fn has_percent(&self) -> bool {
        matches!(self, LPA::LP(lp) if lp.has_percent())
    }
}

/// `max-width` / `max-height`: `none` or a length-percentage.
pub type MaxSize = Option<LP>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Display {
    None,
    Contents,
    Inline,
    Block,
    InlineBlock,
    FlowRoot,
    ListItem,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableColumn,
    TableColumnGroup,
    TableCaption,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Display::Inline
                | Display::InlineBlock
                | Display::InlineTable
                | Display::InlineFlex
                | Display::InlineGrid
        )
    }

    /// CSS 2 §9.7 blockification.
    pub fn blockify(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineTable => Display::Table,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableCell
            | Display::TableColumn
            | Display::TableColumnGroup
            | Display::TableCaption => Display::Block,
            other => other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Hidden,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

impl BorderStyle {
    pub fn is_visible(self) -> bool {
        !matches!(self, BorderStyle::None | BorderStyle::Hidden)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Hidden,
    Collapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Px(f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
    Justify,
    /// `-webkit-center` / `-moz-center` / HTML `align=center`: also centers block children.
    LegacyCenter,
    LegacyLeft,
    LegacyRight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Pre,
    Nowrap,
    PreWrap,
    PreLine,
    BreakSpaces,
}

impl WhiteSpace {
    pub fn preserves_spaces(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces)
    }
    pub fn preserves_newlines(self) -> bool {
        !matches!(self, WhiteSpace::Normal | WhiteSpace::Nowrap)
    }
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::Pre | WhiteSpace::Nowrap)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerticalAlign {
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Middle,
    Top,
    Bottom,
    Length(LP),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextDecorationLine {
    pub underline: bool,
    pub overline: bool,
    pub line_through: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStyleType {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    LowerGreek,
    DisclosureOpen,
    DisclosureClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStylePosition {
    Outside,
    Inside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderCollapse {
    Separate,
    Collapse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableLayout {
    Auto,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionSide {
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    Auto,
    Default,
    Pointer,
    Text,
    Move,
    NotAllowed,
    Wait,
    Help,
    Crosshair,
    Progress,
    Grab,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GradientStop {
    pub color: Rgba,
    pub pos: Option<LP>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Image {
    Url(String),
    /// Angle in degrees (CSS convention: 180deg = to bottom).
    LinearGradient {
        angle: f32,
        stops: Vec<GradientStop>,
        repeating: bool,
    },
    RadialGradient {
        stops: Vec<GradientStop>,
        repeating: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgRepeat {
    Repeat,
    NoRepeat,
    Space,
    Round,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BgSize {
    Auto(LPA, LPA),
    Cover,
    Contain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BgLayer {
    pub image: Option<Image>,
    pub repeat_x: BgRepeat,
    pub repeat_y: BgRepeat,
    pub pos_x: LP,
    pub pos_y: LP,
    pub size: BgSize,
    pub fixed: bool,
}

impl Default for BgLayer {
    fn default() -> Self {
        BgLayer {
            image: None,
            repeat_x: BgRepeat::Repeat,
            repeat_y: BgRepeat::Repeat,
            pos_x: LP::Pct(0.0),
            pos_y: LP::Pct(0.0),
            size: BgSize::Auto(LPA::Auto, LPA::Auto),
            fixed: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
    pub inset: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContentItem {
    Text(String),
    Attr(String),
    OpenQuote,
    CloseQuote,
    Counter(String, ListStyleType),
    Url(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

/// justify-content / align-* keyword subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Auto,
    Normal,
    Stretch,
    Start,
    End,
    Center,
    Baseline,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

/// All computed properties for one element (or pseudo-element).
#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    // ---- inherited ----
    pub color: Rgba,
    pub font_family: Rc<Vec<String>>,
    pub font_size: f32,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub small_caps: bool,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub text_indent: LP,
    pub text_transform: TextTransform,
    pub white_space: WhiteSpace,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub visibility: Visibility,
    pub list_style_type: ListStyleType,
    pub list_style_position: ListStylePosition,
    pub list_style_image: Option<String>,
    pub border_collapse: BorderCollapse,
    pub border_spacing: (f32, f32),
    pub caption_side: CaptionSide,
    pub empty_cells_show: bool,
    pub cursor: Cursor,
    pub direction_rtl: bool,
    pub color_scheme_dark: bool,
    /// Whether `color-scheme` allows dark (used for light-dark()).
    pub color_scheme_allows_dark: bool,
    pub custom: Rc<Vec<(String, Rc<Vec<bb_css::Cv>>)>>,
    /// Inherited decorations from ancestors (CSS Text Decoration §3: propagated).
    pub quotes_depth: u8,

    // ---- not inherited ----
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: Clear,
    pub top: LPA,
    pub right: LPA,
    pub bottom: LPA,
    pub left: LPA,
    pub width: LPA,
    pub height: LPA,
    pub min_width: LPA,
    pub min_height: LPA,
    pub max_width: MaxSize,
    pub max_height: MaxSize,
    pub margin: [LPA; 4],
    pub padding: [LP; 4],
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [Rgba; 4],
    pub border_radius: [LP; 4],
    pub box_sizing: BoxSizing,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub z_index: Option<i32>,
    pub opacity: f32,
    pub background_color: Rgba,
    pub background: Vec<BgLayer>,
    pub box_shadow: Vec<Shadow>,
    pub text_decoration_line: TextDecorationLine,
    pub text_decoration_color: Option<Rgba>,
    pub vertical_align: VerticalAlign,
    pub table_layout: TableLayout,
    pub content: Option<Vec<ContentItem>>,
    pub outline_width: f32,
    pub outline_style: BorderStyle,
    pub outline_color: Option<Rgba>,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: LPA,
    pub justify_content: Align,
    pub align_items: Align,
    pub align_self: Align,
    pub align_content: Align,
    pub row_gap: LP,
    pub column_gap: LP,
    pub order: i32,
    pub object_fit: ObjectFit,
    pub counter_reset: Vec<(String, i32)>,
    pub counter_increment: Vec<(String, i32)>,
    /// `transform: translate(...)` subset (px, px).
    pub translate: (LP, LP),
}

impl ComputedStyle {
    /// Initial values for every property (CSS "initial").
    pub fn initial() -> ComputedStyle {
        ComputedStyle {
            color: Rgba::BLACK,
            font_family: Rc::new(vec!["serif".to_string()]),
            font_size: 16.0,
            font_weight: 400,
            font_style: FontStyle::Normal,
            small_caps: false,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Start,
            text_indent: LP::ZERO,
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            visibility: Visibility::Visible,
            list_style_type: ListStyleType::Disc,
            list_style_position: ListStylePosition::Outside,
            list_style_image: None,
            border_collapse: BorderCollapse::Separate,
            border_spacing: (0.0, 0.0),
            caption_side: CaptionSide::Top,
            empty_cells_show: true,
            cursor: Cursor::Auto,
            direction_rtl: false,
            color_scheme_dark: false,
            color_scheme_allows_dark: false,
            custom: Rc::new(Vec::new()),
            quotes_depth: 0,
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            top: LPA::Auto,
            right: LPA::Auto,
            bottom: LPA::Auto,
            left: LPA::Auto,
            width: LPA::Auto,
            height: LPA::Auto,
            min_width: LPA::Auto,
            min_height: LPA::Auto,
            max_width: None,
            max_height: None,
            margin: [LPA::ZERO, LPA::ZERO, LPA::ZERO, LPA::ZERO],
            padding: [LP::ZERO, LP::ZERO, LP::ZERO, LP::ZERO],
            border_width: [3.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [Rgba::BLACK; 4],
            border_radius: [LP::ZERO, LP::ZERO, LP::ZERO, LP::ZERO],
            box_sizing: BoxSizing::ContentBox,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            z_index: None,
            opacity: 1.0,
            background_color: Rgba::TRANSPARENT,
            background: Vec::new(),
            box_shadow: Vec::new(),
            text_decoration_line: TextDecorationLine::default(),
            text_decoration_color: None,
            vertical_align: VerticalAlign::Baseline,
            table_layout: TableLayout::Auto,
            content: None,
            outline_width: 3.0,
            outline_style: BorderStyle::None,
            outline_color: None,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: LPA::Auto,
            justify_content: Align::Normal,
            align_items: Align::Normal,
            align_self: Align::Auto,
            align_content: Align::Normal,
            row_gap: LP::ZERO,
            column_gap: LP::ZERO,
            order: 0,
            object_fit: ObjectFit::Fill,
            counter_reset: Vec::new(),
            counter_increment: Vec::new(),
            translate: (LP::ZERO, LP::ZERO),
        }
    }

    /// A style that inherits inherited properties from `parent` and has
    /// initial values for the rest.
    pub fn inherit_from(parent: &ComputedStyle) -> ComputedStyle {
        let init = Self::initial();
        ComputedStyle {
            color: parent.color,
            font_family: parent.font_family.clone(),
            font_size: parent.font_size,
            font_weight: parent.font_weight,
            font_style: parent.font_style,
            small_caps: parent.small_caps,
            line_height: parent.line_height,
            text_align: parent.text_align,
            text_indent: parent.text_indent.clone(),
            text_transform: parent.text_transform,
            white_space: parent.white_space,
            letter_spacing: parent.letter_spacing,
            word_spacing: parent.word_spacing,
            visibility: parent.visibility,
            list_style_type: parent.list_style_type,
            list_style_position: parent.list_style_position,
            list_style_image: parent.list_style_image.clone(),
            border_collapse: parent.border_collapse,
            border_spacing: parent.border_spacing,
            caption_side: parent.caption_side,
            empty_cells_show: parent.empty_cells_show,
            cursor: parent.cursor,
            direction_rtl: parent.direction_rtl,
            color_scheme_dark: parent.color_scheme_dark,
            color_scheme_allows_dark: parent.color_scheme_allows_dark,
            custom: parent.custom.clone(),
            quotes_depth: parent.quotes_depth,
            ..init
        }
    }

    pub fn is_positioned(&self) -> bool {
        self.position != Position::Static
    }

    pub fn is_out_of_flow(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed) || self.float != Float::None
    }

    /// Used line height in px given the font's "normal" line height.
    pub fn line_height_px(&self, normal: f32) -> f32 {
        match self.line_height {
            LineHeight::Normal => normal,
            LineHeight::Number(n) => n * self.font_size,
            LineHeight::Px(px) => px,
        }
    }

    /// Whether the used border on side `i` (0=top,1=right,2=bottom,3=left) is drawn.
    pub fn border_px(&self, i: usize) -> f32 {
        if self.border_style[i].is_visible() {
            self.border_width[i]
        } else {
            0.0
        }
    }

    pub fn custom_property(&self, name: &str) -> Option<&Rc<Vec<bb_css::Cv>>> {
        self.custom.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}
