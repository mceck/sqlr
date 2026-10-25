//! Design tokens: the light and dark palettes, the editor's token colors
//! and the spacing scale.

use egui::Color32;
use sqlr_core::TokenKind;

/// `0xRRGGBB` as an opaque color.
const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// The UI colors widgets draw with; read it with [`super::palette`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg_editor: Color32,
    pub bg_panel: Color32,
    /// Popups and dialogs.
    pub bg_raised: Color32,
    /// The main toolbar and the dialog headers.
    pub bg_toolbar: Color32,
    /// Text fields and other sunken controls.
    pub bg_input: Color32,
    /// Secondary buttons and the left list of a dialog.
    pub bg_subtle: Color32,
    pub border: Color32,
    /// The border of an input or of a raised dialog.
    pub border_input: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    /// Placeholders (hint text) and other weak text such as shortcut hints: still readable
    /// on an input (≥ 4.5:1), clearly dimmer than typed text and than `text_muted`.
    pub placeholder: Color32,
    pub text_disabled: Color32,
    pub accent: Color32,
    /// Text on `accent`.
    pub accent_text: Color32,
    /// A selected row or a toggled toolbar button.
    pub selection: Color32,
    /// The ring of a focused text field and the text of a selected label: the accent,
    /// shifted until it reads on `selection` (≥ 4.5:1) and on `bg_input` (≥ 3:1).
    pub focus: Color32,
    /// The accent over a panel, for selected/toggled surfaces.
    pub accent_soft: Color32,
    /// A barely-there wash behind the statement holding the caret.
    pub statement_tint: Color32,
    pub hover: Color32,
    pub error: Color32,
    pub warning: Color32,
    pub success: Color32,
    pub null_text: Color32,
    pub icon_table: Color32,
    pub icon_view: Color32,
    pub icon_key: Color32,
    pub icon_schema: Color32,
    pub icon_function: Color32,
}

impl Palette {
    /// The built-in accent, which a custom accent color replaces.
    pub const DEFAULT_ACCENT: [u8; 3] = [0x2E, 0x6F, 0xF0];

    /// This palette with `rgb` as its accent: the text on it is white or black, whichever
    /// contrasts more, and the soft accent is the color washed into the panel.
    pub fn with_accent(mut self, rgb: [u8; 3]) -> Self {
        if rgb == Self::DEFAULT_ACCENT {
            return self;
        }
        let accent = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        self.accent = accent;
        // Whichever of black and white contrasts more: at least 4.5:1 on any accent.
        self.accent_text = if super::contrast_ratio(Color32::BLACK, accent)
            > super::contrast_ratio(Color32::WHITE, accent)
        {
            Color32::BLACK
        } else {
            Color32::WHITE
        };
        let wash = if self.is_dark() { 0.22 } else { 0.12 };
        self.accent_soft = self.bg_panel.lerp_to_gamma(accent, wash);
        self.focus = self.readable_accent(accent);
        self
    }

    /// `accent` moved toward the text color in small steps until it reads on the selection
    /// and stands out of an input.
    fn readable_accent(&self, accent: Color32) -> Color32 {
        let readable = |c: Color32| {
            super::contrast_ratio(c, self.selection) >= 4.5
                && super::contrast_ratio(c, self.bg_input) >= 3.0
        };
        (0..=20)
            .map(|step| accent.lerp_to_gamma(self.text, step as f32 / 20.0))
            .find(|&c| readable(c))
            .unwrap_or(self.text)
    }

    /// A dark palette with the editor near black and the panels one
    /// step above it. Two tokens are nudged for WCAG AA (≥ 4.5:1): `text_muted`
    /// (`#868A91` is 3.98:1 on `bg_panel`) and `accent` (`#3574F0` is 4.28:1 under white).
    pub fn dark() -> Self {
        Self {
            bg_editor: hex(0x17181A),
            bg_panel: hex(0x222327),
            bg_raised: hex(0x222327),
            bg_toolbar: hex(0x222327),
            bg_input: hex(0x17181A),
            bg_subtle: hex(0x1D1E21),
            border: hex(0x34363C),
            border_input: hex(0x4A4C54),
            text: hex(0xDFE1E5),
            text_muted: hex(0x90949A),
            placeholder: hex(0x7E828A),
            text_disabled: hex(0x686C73),
            accent: hex(0x2E6FF0),
            accent_text: hex(0xFFFFFF),
            selection: hex(0x293C63),
            focus: hex(0x8FB2FF),
            accent_soft: hex(0x223555),
            statement_tint: hex(0x20232A),
            hover: hex(0x303237),
            error: hex(0xF75464),
            warning: hex(0xF2C55C),
            success: hex(0x5FB865),
            null_text: hex(0x6F737A),
            icon_table: hex(0x548AF7),
            icon_view: hex(0xB189F5),
            icon_key: hex(0xF2C55C),
            icon_schema: hex(0x868A91),
            icon_function: hex(0x5FB865),
        }
    }

    /// Light palette; `accent` is nudged as in [`Palette::dark`]. Borders, secondary and
    /// disabled text are dark enough to separate cards and controls from white, and the
    /// status colors (`warning`, `error`, `success`) are darkened to read as text (≥ 4.5:1)
    /// on the editor, panel and subtle backgrounds.
    pub fn light() -> Self {
        Self {
            bg_editor: hex(0xFFFFFF),
            bg_panel: hex(0xF7F8FA),
            bg_raised: hex(0xFFFFFF),
            bg_toolbar: hex(0xFFFFFF),
            bg_input: hex(0xFFFFFF),
            bg_subtle: hex(0xF0F1F4),
            border: hex(0xD8DBE2),
            border_input: hex(0xBEC3CD),
            text: hex(0x1E1F22),
            text_muted: hex(0x5F6370),
            placeholder: hex(0x6E7380),
            text_disabled: hex(0x8F95A3),
            accent: hex(0x2E6FF0),
            accent_text: hex(0xFFFFFF),
            selection: hex(0xD5E1FF),
            focus: hex(0x1F56C9),
            accent_soft: hex(0xDCE6FC),
            statement_tint: hex(0xEBF1FA),
            hover: hex(0xDFE1E5),
            error: hex(0xC93545),
            warning: hex(0x8F5B00),
            success: hex(0x1D7F37),
            null_text: hex(0x8C8C8C),
            icon_table: hex(0x3574F0),
            icon_view: hex(0x834DF0),
            icon_key: hex(0x8F5B00),
            icon_schema: hex(0x6C707E),
            icon_function: hex(0x1D7F37),
        }
    }

    /// A dark palette has a dark editor background.
    pub fn is_dark(&self) -> bool {
        relative_luminance(self.bg_editor) < 0.5
    }

    /// The grid's odd rows: `bg_editor` 3 % toward white (dark) or black (light).
    pub fn zebra(&self) -> Color32 {
        let toward = if self.is_dark() {
            Color32::WHITE
        } else {
            Color32::BLACK
        };
        self.bg_editor.lerp_to_gamma(toward, 0.03)
    }
}

/// The editor's colors: lexed tokens, the gutter, the caret and find matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EditorColors {
    /// Identifiers, operators and punctuation.
    pub plain: Color32,
    pub keyword: Color32,
    pub string: Color32,
    pub number: Color32,
    pub comment: Color32,
    pub function: Color32,
    pub param: Color32,
    pub gutter: Color32,
    pub caret: Color32,
    pub find_match: Color32,
}

impl EditorColors {
    /// The Task 10 (Darcula-like) colors.
    pub fn dark() -> Self {
        Self {
            plain: hex(0xA9B7C6),
            keyword: hex(0xCF8E6D),
            string: hex(0x6AAB73),
            number: hex(0x2AACB8),
            comment: hex(0x7A7E85),
            function: hex(0x56A8F5),
            param: hex(0xC77DBB),
            gutter: hex(0x6F737A),
            caret: hex(0xCED0D6),
            find_match: hex(0x3B514D),
        }
    }

    /// The same hue order on white.
    pub fn light() -> Self {
        Self {
            plain: hex(0x080808),
            keyword: hex(0x0033B3),
            string: hex(0x067D17),
            number: hex(0x1750EB),
            comment: hex(0x8C8C8C),
            function: hex(0x00627A),
            param: hex(0x871094),
            gutter: hex(0xAEB3C2),
            caret: hex(0x000000),
            find_match: hex(0xF6E9A6),
        }
    }

    pub fn for_palette(palette: &Palette) -> Self {
        if palette.is_dark() {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub fn token(&self, kind: TokenKind) -> Color32 {
        match kind {
            TokenKind::Keyword => self.keyword,
            TokenKind::String => self.string,
            TokenKind::Number => self.number,
            TokenKind::Comment => self.comment,
            TokenKind::Function => self.function,
            TokenKind::Param => self.param,
            TokenKind::Operator
            | TokenKind::Punct
            | TokenKind::Semicolon
            | TokenKind::Ident
            | TokenKind::QuotedIdent => self.plain,
        }
    }
}

/// The spacing scale and the fixed sizes, in points.
pub struct Spacing;

impl Spacing {
    pub const XS: f32 = 2.0;
    pub const S: f32 = 4.0;
    pub const M: f32 = 8.0;
    pub const L: f32 = 12.0;
    pub const XL: f32 = 16.0;
    pub const ROW_TREE: f32 = 24.0;
    pub const ROW_GRID: f32 = 22.0;
    pub const TOOLBAR: f32 = 44.0;
    pub const TAB: f32 = 34.0;
    pub const STATUS: f32 = 26.0;
    /// The path strip above an editor.
    pub const BREADCRUMB: f32 = 26.0;
    /// A toast's width, and its colored bar.
    pub const TOAST_WIDTH: f32 = 320.0;
    pub const TOAST_BAR: f32 = 3.0;
    /// Widgets.
    pub const RADIUS_S: u8 = 4;
    /// Windows, popups and the palette.
    pub const RADIUS_M: u8 = 6;
}

/// WCAG 2 relative luminance of an sRGB color (alpha ignored).
pub(crate) fn relative_luminance(c: Color32) -> f32 {
    let channel = |v: u8| {
        let v = f32::from(v) / 255.0;
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// The WCAG contrast ratio of two colors, from 1 (none) to 21 (black on white).
pub fn contrast_ratio(fg: Color32, bg: Color32) -> f32 {
    let (a, b) = (relative_luminance(fg), relative_luminance(bg));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}
