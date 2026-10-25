use egui::{Color32, FontFamily, vec2};
use sqlr_core::NodeKind;
use sqlr_ui::theme::{self, Palette, contrast_ratio, icons};

/// Runs `f` inside one frame of a fresh context (fonts exist only once a frame has run).
fn in_frame(ctx: &egui::Context, f: impl FnOnce(&egui::Context)) {
    let mut f = Some(f);
    let mut out = ctx.run_ui(Default::default(), |ui| {
        (f.take().expect("runs once"))(ui.ctx())
    });
    // No renderer consumes the font atlas upload here.
    out.textures_delta.clear();
}

#[test]
fn palettes_meet_contrast() {
    for (name, p) in [("dark", Palette::dark()), ("light", Palette::light())] {
        let text_editor = contrast_ratio(p.text, p.bg_editor);
        let text_panel = contrast_ratio(p.text, p.bg_panel);
        let muted_panel = contrast_ratio(p.text_muted, p.bg_panel);
        let on_accent = contrast_ratio(p.accent_text, p.accent);
        assert!(
            text_editor >= 7.0,
            "{name}: text on bg_editor {text_editor}"
        );
        assert!(text_panel >= 7.0, "{name}: text on bg_panel {text_panel}");
        assert!(
            muted_panel >= 4.5,
            "{name}: text_muted on bg_panel {muted_panel}"
        );
        assert!(
            on_accent >= 4.5,
            "{name}: accent_text on accent {on_accent}"
        );
    }
    // The WCAG formula itself: black on white is 21:1, a color on itself 1:1.
    assert!((contrast_ratio(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
    assert!((contrast_ratio(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
    assert!((contrast_ratio(Color32::GRAY, Color32::GRAY) - 1.0).abs() < 0.001);
}

/// Status text (warnings such as "Driver missing", ER key names, danger menu items) stays
/// readable on every surface it is drawn on, in both themes.
#[test]
fn status_colors_are_readable_on_every_surface() {
    for (name, p) in [("dark", Palette::dark()), ("light", Palette::light())] {
        for (surface, bg) in [
            ("bg_editor", p.bg_editor),
            ("bg_panel", p.bg_panel),
            ("bg_subtle", p.bg_subtle),
        ] {
            for (token, fg) in [
                ("warning", p.warning),
                ("error", p.error),
                ("text_muted", p.text_muted),
                ("icon_key", p.icon_key),
            ] {
                let ratio = contrast_ratio(fg, bg);
                assert!(ratio >= 4.5, "{name}: {token} on {surface} is {ratio:.2}");
            }
        }
    }
}

/// Borders and disabled text are visible against their background, and disabled text still
/// reads dimmer than secondary text.
#[test]
fn borders_and_disabled_text_are_visible() {
    for (name, p) in [("dark", Palette::dark()), ("light", Palette::light())] {
        let card = contrast_ratio(p.border, p.bg_editor);
        assert!(card >= 1.3, "{name}: border on bg_editor is {card:.2}");
        let panel = contrast_ratio(p.border, p.bg_panel);
        assert!(panel >= 1.2, "{name}: border on bg_panel is {panel:.2}");
        let input = contrast_ratio(p.border_input, p.bg_input);
        assert!(
            input >= 1.75,
            "{name}: border_input on bg_input is {input:.2}"
        );
        let disabled = contrast_ratio(p.text_disabled, p.bg_panel);
        assert!(
            disabled >= 2.5,
            "{name}: text_disabled on bg_panel is {disabled:.2}"
        );
        assert!(
            disabled < contrast_ratio(p.text_muted, p.bg_panel),
            "{name}: disabled text must stay dimmer than muted text"
        );
        // The selected navigation entry stands out from the rail it sits on.
        let selected = contrast_ratio(p.accent_soft, p.bg_panel);
        assert!(
            selected >= 1.15,
            "{name}: accent_soft on bg_panel is {selected:.2}"
        );
    }
}

/// A focused text field gets a ring in the accent family (not the text color), and the same
/// color stays readable as the text of a selected label.
#[test]
fn focus_ring_is_an_accent_that_reads_on_the_selection() {
    for base in [Palette::dark(), Palette::light()] {
        for p in [base, base.with_accent([0xE5, 0x39, 0x35])] {
            assert_ne!(p.focus, p.text, "the focus ring is not the text color");
            let on_selection = contrast_ratio(p.focus, p.selection);
            assert!(on_selection >= 4.5, "focus on selection {on_selection:.2}");
            let ring = contrast_ratio(p.focus, p.bg_input);
            assert!(ring >= 3.0, "focus ring on bg_input {ring:.2}");
            // Placeholders read, but clearly dimmer than typed and secondary text.
            let hint = contrast_ratio(p.placeholder, p.bg_input);
            assert!(hint >= 4.5, "placeholder on bg_input {hint:.2}");
            assert!(hint < contrast_ratio(p.text_muted, p.bg_input));
            assert!(hint * 2.0 < contrast_ratio(p.text, p.bg_input));
        }
    }
    let ctx = egui::Context::default();
    in_frame(&ctx, |ctx| theme::apply(ctx, &Palette::light(), 13.0));
    let style = ctx.global_style();
    assert_eq!(style.visuals.selection.stroke.color, Palette::light().focus);
    assert_eq!(
        style.visuals.weak_text_color(),
        Palette::light().placeholder
    );
    // Scroll bars stay visible (dimmed) whenever an area scrolls, not only on hover.
    assert!(style.spacing.scroll.dormant_handle_opacity > 0.0);
}

/// The dark theme uses a near-black editor and panels
/// one step above it. Contrast against black is `1 + luminance / 0.05`, so it measures depth.
#[test]
fn a_custom_accent_keeps_its_text_readable() {
    for base in [Palette::dark(), Palette::light()] {
        // The built-in accent changes nothing.
        assert_eq!(base.with_accent(Palette::DEFAULT_ACCENT), base);
        for rgb in [
            [0xE5, 0x39, 0x35],
            [0xFF, 0xD5, 0x4F],
            [0x20, 0x20, 0x20],
            [0x2E, 0x7D, 0x32],
        ] {
            let p = base.with_accent(rgb);
            assert_eq!(p.accent, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            // Black or white, whichever is better: at least 4.5:1 for these samples.
            let on = contrast_ratio(p.accent_text, p.accent);
            assert!(on >= 4.5, "{rgb:?}: {on}");
            assert_ne!(p.accent_soft, base.accent_soft, "{rgb:?}");
        }
    }
}

#[test]
fn dark_theme_is_deep() {
    let p = Palette::dark();
    let depth = |c: Color32| contrast_ratio(c, Color32::BLACK);
    assert!(
        depth(p.bg_editor) <= 1.20,
        "bg_editor is too light: {:.3}",
        depth(p.bg_editor)
    );
    assert!(
        depth(p.bg_panel) <= 1.37,
        "bg_panel is too light: {:.3}",
        depth(p.bg_panel)
    );
    assert!(depth(p.bg_editor) < depth(p.bg_subtle));
    assert!(depth(p.bg_subtle) < depth(p.bg_panel));
}

#[test]
fn apply_sets_visuals() {
    let ctx = egui::Context::default();
    in_frame(&ctx, |ctx| theme::apply(ctx, &Palette::light(), 13.0));
    let style = ctx.global_style();
    let light = Palette::light();
    assert!(!style.visuals.dark_mode);
    assert_eq!(style.visuals.panel_fill, light.bg_panel);
    assert_eq!(style.spacing.item_spacing, vec2(8.0, 6.0));
    assert_eq!(theme::palette(&ctx), light);
    assert_eq!(theme::monospace(&ctx).size, 13.0);

    in_frame(&ctx, |ctx| theme::apply(ctx, &Palette::dark(), 16.0));
    assert!(ctx.global_style().visuals.dark_mode);
    assert_eq!(theme::palette(&ctx), Palette::dark());
    assert_eq!(theme::monospace(&ctx), egui::FontId::monospace(16.0));
}

#[test]
fn palette_defaults_to_dark() {
    let ctx = egui::Context::default();
    assert_eq!(theme::palette(&ctx), Palette::dark());
}

#[test]
fn fonts_installed() {
    // Any real font file stands in for the app's CJK fallback.
    static EXTRA: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
    let ctx = egui::Context::default();
    theme::install_fonts(&ctx, &[("extra", EXTRA)]);
    // `set_fonts` takes effect at the start of the next pass.
    in_frame(&ctx, |_| {});
    in_frame(&ctx, |ctx| {
        ctx.fonts(|f| {
            let families = &f.definitions().families;
            let proportional = &families[&FontFamily::Proportional];
            let monospace = &families[&FontFamily::Monospace];
            assert_eq!(proportional[0], theme::fonts::INTER);
            assert!(!monospace.is_empty());
            // The fallback is appended to both families.
            assert_eq!(proportional.last().map(String::as_str), Some("extra"));
            assert_eq!(monospace.last().map(String::as_str), Some("extra"));
            // Icons have their own family, Phosphor first: Inter has glyphs at some of
            // Phosphor's private-use code points.
            let icons = &families[&FontFamily::Name(theme::fonts::ICONS.into())];
            assert_eq!(icons[0], theme::fonts::ICONS);
            assert!(!proportional.iter().any(|n| n == theme::fonts::ICONS));
            let semibold = &families[&FontFamily::Name(theme::fonts::INTER_SEMIBOLD.into())];
            assert_eq!(semibold[0], theme::fonts::INTER_SEMIBOLD);
        });
    });
}

#[test]
fn heading_uses_semibold_only_once_installed() {
    let ctx = egui::Context::default();
    in_frame(&ctx, |ctx| theme::apply(ctx, &Palette::dark(), 13.0));
    let heading = ctx.global_style().text_styles[&egui::TextStyle::Heading].clone();
    assert_eq!(
        heading.family,
        FontFamily::Proportional,
        "no semibold font yet"
    );

    theme::install_fonts(&ctx, &[]);
    in_frame(&ctx, |_| {});
    in_frame(&ctx, |ctx| theme::apply(ctx, &Palette::dark(), 13.0));
    let heading = ctx.global_style().text_styles[&egui::TextStyle::Heading].clone();
    assert_eq!(
        heading,
        egui::FontId::new(15.0, FontFamily::Name(theme::fonts::INTER_SEMIBOLD.into()))
    );
}

/// Every `NodeKind`; the match fails to compile when a variant is added and not listed.
fn all_node_kinds() -> Vec<NodeKind> {
    let all = vec![
        NodeKind::Database,
        NodeKind::Schema,
        NodeKind::Folder,
        NodeKind::Table,
        NodeKind::View,
        NodeKind::MaterializedView,
        NodeKind::Column,
        NodeKind::Index,
        NodeKind::Function,
        NodeKind::Procedure,
        NodeKind::Sequence,
        NodeKind::Trigger,
    ];
    for kind in &all {
        match kind {
            NodeKind::Database
            | NodeKind::Schema
            | NodeKind::Folder
            | NodeKind::Table
            | NodeKind::View
            | NodeKind::MaterializedView
            | NodeKind::Column
            | NodeKind::Index
            | NodeKind::Function
            | NodeKind::Procedure
            | NodeKind::Sequence
            | NodeKind::Trigger => {}
        }
    }
    all
}

#[test]
fn icons_for_every_node_kind() {
    let palette = Palette::dark();
    for kind in all_node_kinds() {
        let (glyph, color) = icons::for_node(kind);
        assert!(!glyph.is_empty(), "{kind:?} has no icon");
        assert_ne!(color(&palette), Color32::TRANSPARENT, "{kind:?}");
    }
    assert_eq!(
        icons::for_node(NodeKind::Table).1(&palette),
        palette.icon_table
    );
    assert_eq!(
        icons::for_node(NodeKind::View).1(&palette),
        palette.icon_view
    );
    assert_eq!(
        icons::for_node(NodeKind::Index).1(&palette),
        palette.icon_key
    );
    assert_eq!(
        icons::for_node(NodeKind::Schema).1(&palette),
        palette.icon_schema
    );
    assert_eq!(
        icons::for_node(NodeKind::Function).1(&palette),
        palette.icon_function
    );
}

#[test]
fn database_icons_follow_the_driver() {
    use sqlr_ui::theme::icons::DbIcon;
    assert_eq!(DbIcon::for_driver("postgres"), DbIcon::Postgres);
    assert_eq!(DbIcon::for_driver("postgresql"), DbIcon::Postgres);
    assert_eq!(DbIcon::for_driver("mysql"), DbIcon::MySql);
    assert_eq!(DbIcon::for_driver("sqlite"), DbIcon::Sqlite);
    assert_eq!(DbIcon::for_driver("redis"), DbIcon::Redis);
    assert_eq!(DbIcon::for_driver("mongodb"), DbIcon::Mongo);
    assert_eq!(DbIcon::for_driver("elasticsearch"), DbIcon::Elastic);
    // The installable drivers have their own marks.
    for id in ["hana", "sap-hana"] {
        assert_eq!(DbIcon::for_driver(id), DbIcon::Sap, "{id}");
    }
    for id in ["oracle", "jdbc-oracle"] {
        assert_eq!(DbIcon::for_driver(id), DbIcon::Oracle, "{id}");
    }
    for id in ["mssql", "sqlserver", "jdbc-sqlserver"] {
        assert_eq!(DbIcon::for_driver(id), DbIcon::MsSql, "{id}");
    }
    assert_eq!(DbIcon::for_driver("jdbc-clickhouse"), DbIcon::ClickHouse);
    // The explorer's fake driver and unknown ids use the generic mark.
    assert_eq!(DbIcon::for_driver("fake"), DbIcon::Database);
    assert_eq!(DbIcon::for_driver("mariadb"), DbIcon::Database);
    assert_eq!(DbIcon::for_driver("unknown"), DbIcon::Database);
}

#[test]
fn light_editor_colors_follow_the_brief() {
    use sqlr_core::TokenKind;
    let light = theme::EditorColors::light();
    let hex = |v: u32| Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
    assert_eq!(light.token(TokenKind::Keyword), hex(0x0033B3));
    assert_eq!(light.token(TokenKind::String), hex(0x067D17));
    assert_eq!(light.token(TokenKind::Number), hex(0x1750EB));
    assert_eq!(light.token(TokenKind::Comment), hex(0x8C8C8C));
    assert_eq!(light.token(TokenKind::Function), hex(0x00627A));
    assert_eq!(light.token(TokenKind::Param), hex(0x871094));
    assert_eq!(light.token(TokenKind::Ident), hex(0x080808));
    // Dark keeps the Task 10 colors.
    let dark = theme::EditorColors::dark();
    assert_eq!(dark.token(TokenKind::Keyword), hex(0xCF8E6D));
    assert_eq!(dark.token(TokenKind::Ident), hex(0xA9B7C6));
    assert_eq!(theme::EditorColors::for_palette(&Palette::light()), light);
    assert_eq!(theme::EditorColors::for_palette(&Palette::dark()), dark);
}

#[test]
fn every_database_mark_paints_in_both_themes() {
    // Building each texture checks its bitmap is the 32x32 RGBA the painter expects.
    let drivers = [
        "postgres",
        "mysql",
        "sqlite",
        "redis",
        "mongodb",
        "elasticsearch",
        "hana",
        "jdbc-oracle",
        "jdbc-sqlserver",
        "jdbc-clickhouse",
        "unknown",
    ];
    for dark in [false, true] {
        let mut h = egui_kittest::Harness::builder()
            .with_size((300.0, 60.0))
            .build_ui(|ui| {
                ui.ctx().set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                ui.horizontal(|ui| {
                    for d in drivers {
                        sqlr_ui::theme::icons::paint_database(ui, d);
                    }
                });
            });
        h.run_steps(2);
    }
}

#[test]
fn format_presets_change_the_pattern_value() {
    use egui_kittest::{Harness, kittest::Queryable};
    let def = sqlr_core::setting_defs()
        .iter()
        .find(|d| d.id == "formats.date")
        .unwrap();
    let mut h = Harness::builder().with_size((700.0, 220.0)).build_ui_state(
        |ui, value: &mut sqlr_core::SettingValue| {
            sqlr_ui::setting_editor(ui, def, value);
        },
        sqlr_core::SettingValue::Text(String::new()),
    );
    h.run_steps(2);
    h.get_by_label("Date presets").click();
    h.run_steps(2);
    h.get_by_label("European · 25/10/2026").click();
    h.run_steps(2);
    assert_eq!(h.state().as_str(), "%d/%m/%Y");
}

#[test]
fn properties_editor_adds_a_row_without_requiring_json() {
    use egui_kittest::{Harness, kittest::Queryable};
    let def = sqlr_core::setting_defs()
        .iter()
        .find(|d| d.id == "drivers.postgres.properties")
        .unwrap();
    let mut h = Harness::builder().with_size((700.0, 300.0)).build_ui_state(
        |ui, value: &mut sqlr_core::SettingValue| {
            sqlr_ui::setting_editor(ui, def, value);
        },
        sqlr_core::SettingValue::Text("{}".into()),
    );
    h.run_steps(2);
    h.get_by_label("Add property").click();
    h.run_steps(2);
    assert_eq!(
        h.get_all_by_role(egui::accesskit::Role::TextInput).count(),
        2
    );
}

#[test]
fn disabling_a_row_limit_keeps_the_previous_limit_for_reenable() {
    use egui_kittest::{Harness, kittest::Queryable};
    let def = sqlr_core::setting_defs()
        .iter()
        .find(|d| d.id == "grid.row_cap")
        .unwrap();
    let mut h = Harness::builder().with_size((600.0, 120.0)).build_ui_state(
        |ui, value: &mut sqlr_core::SettingValue| {
            sqlr_ui::setting_editor(ui, def, value);
        },
        sqlr_core::SettingValue::Int(500),
    );
    h.run_steps(2);
    h.get_by_label("No limit").click();
    h.run_steps(2);
    assert_eq!(h.state().as_i64(), 0);
    h.get_by_label("No limit").click();
    h.run_steps(2);
    assert_eq!(h.state().as_i64(), 500);
}
