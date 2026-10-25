//! Phosphor "regular" icon glyphs (from `egui-phosphor`) and helpers that keep accessible
//! labels plain: an icon is painted as its own glyph, never mixed into a widget's label.

use egui::{
    Align2, Button, Color32, CornerRadius, InnerResponse, Response, RichText, Sense, Stroke, Ui,
    WidgetInfo, WidgetType, vec2,
};
use egui_phosphor::regular as ph;
use sqlr_core::{NodeKind, ObjectKind, SqlTemplate};

use super::{Palette, Spacing};

// Explorer.
pub const DATABASE: &str = ph::DATABASE;
pub const SCHEMA: &str = ph::FOLDER_SIMPLE;
pub const FOLDER: &str = ph::FOLDER;
pub const FOLDER_PLUS: &str = ph::FOLDER_PLUS;
pub const TABLE: &str = ph::TABLE;
pub const VIEW: &str = ph::EYE;
pub const MATERIALIZED_VIEW: &str = ph::STACK;
pub const COLUMN: &str = ph::COLUMNS;
pub const KEY: &str = ph::KEY;
pub const FUNCTION: &str = ph::FUNCTION;
pub const SEQUENCE: &str = ph::LIST_NUMBERS;
pub const PROCEDURE: &str = ph::GEAR_SIX;
pub const TRIGGER: &str = ph::LIGHTNING_A;
pub const CARET_RIGHT: &str = ph::CARET_RIGHT;
pub const CARET_DOWN: &str = ph::CARET_DOWN;
/// Connection state marks: a checked circle while live, a ring while offline, a half circle
/// while connecting.
pub const CONNECTED: &str = ph::CHECK_CIRCLE;
pub const DISCONNECTED: &str = ph::CIRCLE;
pub const CONNECTING: &str = ph::CIRCLE_HALF;
// Toolbar.
pub const RUN: &str = ph::PLAY;
pub const RUN_SCRIPT: &str = ph::PLAY_CIRCLE;
pub const STOP: &str = ph::STOP;
pub const REFRESH: &str = ph::ARROWS_CLOCKWISE;
pub const ADD: &str = ph::PLUS;
pub const CLOSE: &str = ph::X;
pub const SETTINGS: &str = ph::GEAR;
pub const PANEL: &str = ph::SIDEBAR_SIMPLE;
pub const COMMIT: &str = ph::CHECK;
pub const PREVIEW: &str = ph::EYE;
pub const ROLLBACK: &str = ph::ARROW_COUNTER_CLOCKWISE;
pub const SAVEPOINT: &str = ph::BOOKMARK_SIMPLE;
pub const ROLLBACK_TO: &str = ph::ARROW_U_UP_LEFT;
pub const READ_ONLY: &str = ph::LOCK_SIMPLE;
pub const DATA_SOURCE: &str = ph::DATABASE;
pub const APPEARANCE: &str = ph::PALETTE;
pub const QUERY: &str = ph::CODE;
/// The visual explain plan.
pub const PLAN: &str = ph::TREE_STRUCTURE;
pub const CONNECTION: &str = ph::PLUGS_CONNECTED;
/// Agent access (the MCP server).
pub const MCP: &str = ph::ROBOT;
// Commands (the palette and menus).
pub const TERMINAL: &str = ph::TERMINAL_WINDOW;
pub const TRANSFER: &str = ph::ARROWS_LEFT_RIGHT;
pub const COMPARE: &str = ph::GIT_DIFF;
pub const PROFILES: &str = ph::SHIELD_CHECK;
pub const TASKS: &str = ph::LIST_CHECKS;
pub const UPDATES: &str = ph::ARROW_CIRCLE_UP;
pub const DRIVERS: &str = ph::PUZZLE_PIECE;
pub const QUIT: &str = ph::SIGN_OUT;
pub const SOFT_WRAP: &str = ph::TEXT_ALIGN_LEFT;
pub const FORMAT: &str = ph::MAGIC_WAND;
pub const GO_TO: &str = ph::LIST_MAGNIFYING_GLASS;
// Status bar and outcomes.
pub const DOT: &str = ph::CIRCLE;
pub const ROWS: &str = ph::ROWS;
pub const TIMER: &str = ph::TIMER;
pub const WARNING: &str = ph::WARNING;
pub const SUCCESS: &str = ph::CHECK_CIRCLE;
pub const FAILED: &str = ph::X_CIRCLE;
pub const CANCELLED: &str = ph::PROHIBIT;
pub const AFFECTED: &str = ph::PENCIL_SIMPLE;
pub const EDIT: &str = ph::PENCIL_SIMPLE;
pub const DELETE: &str = ph::TRASH;
pub const MOVE_UP: &str = ph::ARROW_UP;
pub const MOVE_DOWN: &str = ph::ARROW_DOWN;
// Palette groups and panels.
pub const COMMAND: &str = ph::LIGHTNING;
pub const SNIPPET: &str = ph::CODE_BLOCK;
pub const HISTORY: &str = ph::CLOCK_COUNTER_CLOCKWISE;
pub const SEARCH: &str = ph::MAGNIFYING_GLASS;
/// The toolbar's compact menu, when the window is too narrow for File … Help.
pub const MENU: &str = ph::LIST;
/// The toolbar's overflow, for controls that do not fit.
pub const MORE: &str = ph::DOTS_THREE;
pub const KEYBOARD: &str = ph::KEYBOARD;
// Menu actions.
/// The mark of the checked choice in a menu.
pub const CHECK: &str = ph::CHECK;
pub const COPY: &str = ph::COPY;
pub const DUPLICATE: &str = ph::COPY_SIMPLE;
/// Copy in another format (the "Copy as" submenu).
pub const COPY_AS: &str = ph::CLIPBOARD_TEXT;
/// Paste clipboard rows as new rows.
pub const PASTE_ROWS: &str = ph::ROWS_PLUS_BOTTOM;
/// Copy every row of a result.
pub const COPY_ALL: &str = ph::FILES;
/// Copy a link or URL (a connection URL), distinct from copying text.
pub const LINK: &str = ph::LINK;
/// Disconnect a live connection.
pub const DISCONNECT: &str = ph::PLUGS;
/// Live server sessions and locks.
pub const SESSIONS: &str = ph::PULSE;
pub const OPEN: &str = ph::ARROW_SQUARE_OUT;
pub const INSERT: &str = ph::ARROW_SQUARE_IN;
pub const EXPORT: &str = ph::EXPORT;
pub const IMPORT: &str = ph::DOWNLOAD_SIMPLE;
/// A driver that is fetched before use.
pub const DOWNLOAD: &str = ph::CLOUD_ARROW_DOWN;
/// Bringing something in from elsewhere (connections from another client).
pub const ARRIVE: &str = ph::ARROW_SQUARE_IN;
pub const FILTER: &str = ph::FUNNEL;
pub const NULL: &str = ph::MINUS_CIRCLE;
pub const TRUNCATE: &str = ph::BROOM;
pub const CREATE: &str = ph::FILE_PLUS;
pub const DROP: &str = ph::FILE_MINUS;
pub const CSV: &str = ph::FILE_CSV;
pub const JSON: &str = ph::BRACKETS_CURLY;
pub const SQL: &str = ph::FILE_SQL;
pub const MARKDOWN: &str = ph::MARKDOWN_LOGO;
pub const ROLE: &str = ph::USERS;
// Text fields.
pub const UNDO: &str = ph::ARROW_COUNTER_CLOCKWISE;
pub const REDO: &str = ph::ARROW_CLOCKWISE;
pub const CUT: &str = ph::SCISSORS;
pub const PASTE: &str = ph::CLIPBOARD;
pub const SELECT_ALL: &str = ph::SELECTION_ALL;

/// The brand mark of a connection's database engine. Unknown drivers fall back to the
/// generic database icon from the bundled icon set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DbIcon {
    Postgres,
    MySql,
    Sqlite,
    Redis,
    Mongo,
    Elastic,
    Sap,
    Oracle,
    MsSql,
    ClickHouse,
    Snowflake,
    DuckDb,
    Database,
}

impl DbIcon {
    /// The icon for a driver id (`postgres`, `mysql`, `sqlite`, `redis`, `mongodb`,
    /// `elasticsearch`). Engines without a driver keep the generic mark.
    pub fn for_driver(driver: &str) -> Self {
        match driver {
            "postgres" | "postgresql" => Self::Postgres,
            "mysql" => Self::MySql,
            "sqlite" | "sqlite3" => Self::Sqlite,
            "redis" => Self::Redis,
            "mongodb" | "mongo" => Self::Mongo,
            "elasticsearch" | "elastic" => Self::Elastic,
            // The installable drivers: SAP HANA's own, and the JDBC bridge's presets.
            "hana" | "sap-hana" | "sap" => Self::Sap,
            "oracle" | "jdbc-oracle" => Self::Oracle,
            "mssql" | "sqlserver" | "jdbc-sqlserver" => Self::MsSql,
            "clickhouse" | "jdbc-clickhouse" => Self::ClickHouse,
            "snowflake" => Self::Snowflake,
            "duckdb" => Self::DuckDb,
            _ => Self::Database,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Postgres => "postgresql",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
            Self::Redis => "redis",
            Self::Mongo => "mongodb",
            Self::Elastic => "elasticsearch",
            Self::Sap => "sap",
            Self::Oracle => "oracle",
            Self::MsSql => "mssql",
            Self::ClickHouse => "clickhouse",
            Self::Snowflake => "snowflake",
            Self::DuckDb => "duckdb",
            Self::Database => "database",
        }
    }

    /// The brand marks are full color, so both themes share one bitmap except where the
    /// brand color is too dark for the dark theme.
    fn bytes(self, dark: bool) -> &'static [u8] {
        match (self, dark) {
            // MySQL's navy #00618a disappears on the dark palette: the dark bitmap is the
            // same artwork with the fill lightened to #1aa9e8.
            (Self::MySql, true) => include_bytes!("../../assets/icons/mysql_dark.rgba"),
            (Self::Postgres, _) => include_bytes!("../../assets/icons/postgresql.rgba"),
            (Self::MySql, false) => include_bytes!("../../assets/icons/mysql.rgba"),
            (Self::Sqlite, _) => include_bytes!("../../assets/icons/sqlite.rgba"),
            (Self::Redis, _) => include_bytes!("../../assets/icons/redis.rgba"),
            (Self::Mongo, _) => include_bytes!("../../assets/icons/mongodb.rgba"),
            (Self::Elastic, _) => include_bytes!("../../assets/icons/elasticsearch.rgba"),
            (Self::Sap, _) => include_bytes!("../../assets/icons/sap.rgba"),
            (Self::Oracle, _) => include_bytes!("../../assets/icons/oracle.rgba"),
            (Self::MsSql, _) => include_bytes!("../../assets/icons/mssql.rgba"),
            (Self::ClickHouse, _) => include_bytes!("../../assets/icons/clickhouse.rgba"),
            (Self::Snowflake, _) => include_bytes!("../../assets/icons/snowflake.rgba"),
            (Self::DuckDb, _) => include_bytes!("../../assets/icons/duckdb.rgba"),
            (Self::Database, _) => include_bytes!("../../assets/icons/database.rgba"),
        }
    }
}

/// The bitmap side: 16 px at 2×, like the bundled icon set.
const DB_ICON_PX: usize = 32;

/// Paints the driver's database icon in the same square as [`paint`]. Like [`paint`] it only
/// senses hover, so a click falls through to the row underneath.
pub fn paint_database(ui: &mut Ui, driver: &str) -> Response {
    let height = ui.spacing().interact_size.y;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(SIZE + Spacing::XS, height), Sense::hover());
    paint_database_at(ui, rect, driver);
    response
}

/// Paints the driver's database icon at the center of `rect` (which should be square).
pub fn paint_database_at(ui: &Ui, rect: egui::Rect, driver: &str) {
    let side = rect.width().min(rect.height());
    let image_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    let id = database_texture(ui.ctx(), DbIcon::for_driver(driver), ui.visuals().dark_mode);
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    ui.painter().image(id, image_rect, uv, Color32::WHITE);
}

fn database_texture(ctx: &egui::Context, icon: DbIcon, dark: bool) -> egui::TextureId {
    let id = egui::Id::new(("sqlr-database-icon", icon, dark));
    if let Some(handle) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return handle.id();
    }
    let bytes = icon.bytes(dark);
    debug_assert_eq!(bytes.len(), DB_ICON_PX * DB_ICON_PX * 4);
    let image = egui::ColorImage::from_rgba_premultiplied([DB_ICON_PX, DB_ICON_PX], bytes);
    let name = format!(
        "sqlr-{}-{}",
        icon.name(),
        if dark { "dark" } else { "light" }
    );
    let handle = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
    let texture = handle.id();
    ctx.data_mut(|d| d.insert_temp(id, handle));
    texture
}

/// The explorer icon of a node kind and the palette color it is drawn in.
pub fn for_node(kind: NodeKind) -> (&'static str, fn(&Palette) -> Color32) {
    match kind {
        NodeKind::Database => (DATABASE, |p| p.icon_table),
        NodeKind::Schema => (SCHEMA, |p| p.icon_schema),
        NodeKind::Folder => (FOLDER, |p| p.icon_schema),
        NodeKind::Table => (TABLE, |p| p.icon_table),
        NodeKind::View => (VIEW, |p| p.icon_view),
        NodeKind::MaterializedView => (MATERIALIZED_VIEW, |p| p.icon_view),
        NodeKind::Column => (COLUMN, |p| p.text_muted),
        NodeKind::Index => (KEY, |p| p.icon_key),
        NodeKind::Function => (FUNCTION, |p| p.icon_function),
        NodeKind::Procedure => (PROCEDURE, |p| p.icon_function),
        NodeKind::Sequence => (SEQUENCE, |p| p.icon_schema),
        NodeKind::Trigger => (TRIGGER, |p| p.icon_function),
    }
}

/// The menu icon of an object kind (the "New …" submenus).
pub fn for_object(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Database => DATABASE,
        ObjectKind::Schema => SCHEMA,
        ObjectKind::Table => TABLE,
        ObjectKind::View => VIEW,
        ObjectKind::MaterializedView => MATERIALIZED_VIEW,
        ObjectKind::Index => KEY,
        ObjectKind::Sequence => SEQUENCE,
        ObjectKind::Function => FUNCTION,
        ObjectKind::Procedure => PROCEDURE,
        ObjectKind::Trigger => TRIGGER,
        ObjectKind::Role => ROLE,
    }
}

/// The menu icon of a "Generate SQL" template.
pub fn for_template(template: SqlTemplate) -> &'static str {
    match template {
        SqlTemplate::Select => SEARCH,
        SqlTemplate::Insert => ADD,
        SqlTemplate::Update => EDIT,
        SqlTemplate::Delete => DELETE,
        SqlTemplate::Create => CREATE,
        SqlTemplate::Drop => DROP,
    }
}

/// The size icons are drawn at next to body text.
pub const SIZE: f32 = 14.0;

/// The icon font at `size`.
pub fn font(ui: &Ui, size: f32) -> egui::FontId {
    super::fonts::icons(ui.ctx(), size)
}

/// `icon` as text in the icon font.
pub fn text(ui: &Ui, icon: &str, size: f32) -> RichText {
    RichText::new(icon).font(font(ui, size))
}

/// Paints `icon` in a square of the row's height, with no accessibility node of its own.
pub fn paint(ui: &mut Ui, icon: &str, color: Color32) -> Response {
    paint_with_sense(ui, icon, color, Sense::hover())
}

fn paint_with_sense(ui: &mut Ui, icon: &str, color: Color32, sense: Sense) -> Response {
    let height = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(SIZE + Spacing::XS, height), sense);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        font(ui, SIZE),
        color,
    );
    response
}

/// A context-menu entry with `icon` before `label`; `label` is its accessible name. See
/// [`super::menu`] for shortcuts, danger entries and choices.
pub fn menu_item(ui: &mut Ui, icon: &str, label: &str) -> Response {
    super::menu::MenuItem::new(label).icon(icon).ui(ui)
}

/// [`menu_item`], enabled or grayed out.
pub fn menu_item_enabled(ui: &mut Ui, enabled: bool, icon: &str, label: &str) -> Response {
    super::menu::MenuItem::new(label)
        .icon(icon)
        .enabled(enabled)
        .ui(ui)
}

/// [`menu_item`] with its keyboard shortcut shown on the right; `label` stays its
/// accessible name.
pub fn menu_item_shortcut(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    shortcut: Option<String>,
) -> Response {
    super::menu::MenuItem::new(label)
        .icon(icon)
        .shortcut(shortcut)
        .ui(ui)
}

/// A submenu entry with `icon` before `label`; `label` is its accessible name, followed by
/// egui's menu arrow.
pub fn menu_button<R>(
    ui: &mut Ui,
    icon: &str,
    label: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<Option<R>> {
    super::menu::submenu(ui, Some(icon), label, add_contents)
}

/// A toolbar button: `icon` (in `color` while enabled, else the text color) before `label`,
/// flat until hovered. Its accessible label is `label` alone.
pub fn button(
    ui: &mut Ui,
    enabled: bool,
    icon: &str,
    color: Option<Color32>,
    label: &str,
) -> Response {
    let mut glyph = text(ui, icon, SIZE);
    if let Some(color) = color.filter(|_| enabled) {
        glyph = glyph.color(color);
    }
    let button = Button::new((glyph, label)).frame_when_inactive(false);
    let response = ui.add_enabled(enabled, button);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    response
}

/// [`button`] showing `text` (short, to save space) while `label` stays the accessible name
/// and the tooltip.
pub fn short_button(
    ui: &mut Ui,
    enabled: bool,
    icon: &str,
    color: Option<Color32>,
    text: &str,
    label: &str,
) -> Response {
    let mut glyph = text_icon(ui, icon, color.filter(|_| enabled));
    if !enabled {
        glyph = glyph.color(super::palette(ui.ctx()).text_disabled);
    }
    let response = ui.add_enabled(
        enabled,
        Button::new((glyph, text)).frame_when_inactive(false),
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    response.on_hover_text(label)
}

fn text_icon(ui: &Ui, icon: &str, color: Option<Color32>) -> RichText {
    let glyph = text(ui, icon, SIZE);
    match color {
        Some(c) => glyph.color(c),
        None => glyph,
    }
}

/// "Add" with a green plus; `label` names what is added.
pub fn add_button(ui: &mut Ui, label: &str) -> Response {
    let color = super::palette(ui.ctx()).success;
    short_button(ui, true, ADD, Some(color), "Add", label)
}

/// "Delete" with a trash icon; `label` names what is deleted.
pub fn delete_button(ui: &mut Ui, enabled: bool, label: &str) -> Response {
    let color = super::palette(ui.ctx()).error;
    short_button(ui, enabled, DELETE, Some(color), "Delete", label)
}

/// A frameless icon-only button labeled `label` (also its tooltip).
pub fn icon_button(ui: &mut Ui, icon: &str, color: Option<Color32>, label: &str) -> Response {
    let mut glyph = text(ui, icon, SIZE);
    if let Some(color) = color {
        glyph = glyph.color(color);
    }
    let response = ui.add(Button::new(glyph).frame(false));
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response.on_hover_text(label)
}

/// The side of a toolbar button.
pub const TOOLBAR_SIZE: egui::Vec2 = vec2(28.0, 26.0);

/// A compact, flat toolbar button: the icon alone, a subtle rounded background on hover,
/// an accent-soft one while `selected`. `label` is the accessible name; callers attach the
/// tooltip, so one button never stacks two.
pub fn toolbar_button(
    ui: &mut Ui,
    enabled: bool,
    icon: &str,
    color: Option<Color32>,
    selected: bool,
    label: &str,
) -> Response {
    let palette = super::palette(ui.ctx());
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(TOOLBAR_SIZE, sense);
    let fill = if selected {
        palette.accent_soft
    } else if enabled && response.hovered() {
        palette.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(5), fill);
    let fg = if !enabled {
        palette.text_disabled
    } else if let Some(color) = color {
        color
    } else if selected {
        palette.accent
    } else {
        palette.text
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        font(ui, 15.0),
        fg,
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.shrink(1.0),
            CornerRadius::same(4),
            Stroke::new(1.0, palette.accent),
            egui::StrokeKind::Inside,
        );
    }
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, selected, label));
    response
}

/// A compact flat icon button for a list row: `20×20`, with a hover background. `label` is
/// the accessible name and the tooltip.
pub fn row_button(ui: &mut Ui, icon: &str, label: &str) -> Response {
    let palette = super::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(4), palette.hover);
    }
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        font(ui, 13.0),
        palette.text_muted,
    );
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label));
    response.on_hover_text(label)
}

/// The app mark: a rounded accent square with the database glyph. `label` is its tooltip.
pub fn app_mark(ui: &mut Ui, label: &str) -> Response {
    let palette = super::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(6), palette.accent);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        DATA_SOURCE,
        font(ui, 14.0),
        palette.accent_text,
    );
    response.on_hover_text(label)
}

/// A 1 px vertical divider for a toolbar.
pub fn toolbar_divider(ui: &mut Ui) {
    let palette = super::palette(ui.ctx());
    let height = 18.0;
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, height), Sense::hover());
    let x = rect.center().x;
    ui.painter().vline(
        x,
        (rect.center().y - height / 2.0)..=(rect.center().y + height / 2.0),
        Stroke::new(1.0, palette.border),
    );
}
