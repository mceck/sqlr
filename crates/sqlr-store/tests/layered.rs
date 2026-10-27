use sqlr_core::{Level, SettingValue, ThemePref};
use sqlr_store::{LayeredSettings, Settings};

fn dir_with(text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("settings.toml"), text).unwrap();
    dir
}

type Shape = (&'static str, &'static str, Box<dyn Fn(&Settings)>);

fn int(v: i64) -> SettingValue {
    SettingValue::Int(v)
}

/// Every shape of `settings.toml` written by older versions: the keys from one format
/// are a subset of the next one's, and each loads with its values kept.
#[test]
fn settings_migration_matrix() {
    let shapes: &[Shape] = &[
        (
            "M1: page size and script flag",
            "page_size = 200\nstop_script_on_error = false\n",
            Box::new(|s| {
                assert_eq!(s.page_size, 200);
                assert!(!s.stop_script_on_error);
                assert_eq!(s.theme, ThemePref::System);
            }),
        ),
        (
            "M2: appearance",
            "page_size = 1000\ntheme = \"Light\"\nmono_font_size = 16.0\nsession_per_tab = false\nconnect_timeout_s = 30\n",
            Box::new(|s| {
                assert_eq!(s.theme, ThemePref::Light);
                assert_eq!(s.mono_font_size, 16.0);
                assert!(!s.session_per_tab);
                assert_eq!(s.connect_timeout_s, 30);
            }),
        ),
        (
            "M3b: editor, zoom, cap and drivers",
            "theme = \"Dark\"\nui_scale = 1.25\nline_numbers = false\nsoft_wrap = true\nrow_cap = 0\nsidecar_idle_minutes = 20\n",
            Box::new(|s| {
                assert_eq!(s.theme, ThemePref::Dark);
                assert_eq!(s.ui_scale, 1.25);
                assert!(!s.line_numbers && s.soft_wrap);
                assert_eq!(s.row_cap, 0);
                assert_eq!(s.sidecar_idle_minutes, 20);
            }),
        ),
        (
            "out of range and mistyped values",
            "page_size = 0\nconnect_timeout_s = 99999\nmono_font_size = nan\nui_scale = 9\nline_numbers = \"yes\"\nunknown_key = 1\n",
            Box::new(|s| {
                assert_eq!(s.page_size, 1);
                assert_eq!(s.connect_timeout_s, 600);
                assert_eq!(s.mono_font_size, 13.0);
                assert_eq!(s.ui_scale, 2.0);
                // A wrong type keeps the default, and the other keys still load.
                assert!(s.line_numbers);
            }),
        ),
        (
            "empty file",
            "",
            Box::new(|s| assert_eq!(*s, Settings::default())),
        ),
    ];
    for (name, text, check) in shapes {
        let dir = dir_with(text);
        let settings = Settings::load(dir.path()).unwrap();
        check(&settings);

        // Saving writes the new shape, loses nothing, and backs the old file up once.
        let (layered, notice) = LayeredSettings::load(dir.path());
        assert_eq!(notice.corrupt, None, "{name}");
        layered.save(dir.path()).unwrap();
        assert_eq!(Settings::load(dir.path()).unwrap(), settings, "{name}");
        let backup = dir.path().join("settings.toml.bak-m9");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), *text, "{name}");
        let new = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        assert!(new.contains("version = 2"), "{name}: {new}");

        // A second save does not overwrite the backup with the new shape.
        layered.save(dir.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), *text, "{name}");
    }
}

#[test]
fn a_missing_file_writes_no_backup() {
    let dir = tempfile::tempdir().unwrap();
    LayeredSettings::default().save(dir.path()).unwrap();
    assert!(!dir.path().join("settings.toml.bak-m9").exists());
}

#[test]
fn corrupt_settings_fallback() {
    let dir = dir_with("page_size = [unterminated\n");
    let (layered, notice) = LayeredSettings::load(dir.path());
    let message = notice.corrupt.expect("a notice for the toast");
    assert!(message.contains("settings.toml.corrupt"), "{message}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("settings.toml.corrupt")).unwrap(),
        "page_size = [unterminated\n"
    );
    assert_eq!(
        layered.get("grid.page_size", None),
        (int(500), Level::Global)
    );
    assert_eq!(
        Settings::load(dir.path()).unwrap(),
        Settings::default(),
        "Settings::load falls back too"
    );
    // The next save replaces the unreadable file.
    layered.save(dir.path()).unwrap();
    assert_eq!(LayeredSettings::load(dir.path()).1.corrupt, None);
}

#[test]
fn layered_resolution_order() {
    let mut s = LayeredSettings::default();
    let conn = Some(("c1", "production"));
    assert_eq!(s.get("grid.page_size", conn), (int(500), Level::Global));

    s.set("grid.page_size", &Level::Global, Some(int(100)))
        .unwrap();
    assert_eq!(s.get("grid.page_size", conn), (int(100), Level::Global));

    s.set(
        "grid.page_size",
        &Level::Profile("production".into()),
        Some(int(50)),
    )
    .unwrap();
    assert_eq!(
        s.get("grid.page_size", conn),
        (int(50), Level::Profile("production".into()))
    );
    // Another connection on another profile, and the global view, are not affected.
    assert_eq!(
        s.get("grid.page_size", Some(("c2", "development"))),
        (int(100), Level::Global)
    );
    assert_eq!(s.get("grid.page_size", None), (int(100), Level::Global));

    s.set(
        "grid.page_size",
        &Level::Connection("c1".into()),
        Some(int(10)),
    )
    .unwrap();
    assert_eq!(
        s.get("grid.page_size", conn),
        (int(10), Level::Connection("c1".into()))
    );

    // Clearing an override uncovers the level below.
    s.set("grid.page_size", &Level::Connection("c1".into()), None)
        .unwrap();
    assert_eq!(
        s.get("grid.page_size", conn),
        (int(50), Level::Profile("production".into()))
    );

    // Invalid values and levels are refused.
    assert!(
        s.set("grid.page_size", &Level::Global, Some(int(0)))
            .is_err()
    );
    assert!(
        s.set(
            "grid.page_size",
            &Level::Global,
            Some(SettingValue::Bool(true))
        )
        .is_err()
    );
    assert!(
        s.set(
            "appearance.theme",
            &Level::Profile("p".into()),
            Some(SettingValue::Choice("dark".into()))
        )
        .is_err()
    );
    assert!(s.set("no.such", &Level::Global, Some(int(1))).is_err());

    // The overrides survive a round trip through the files.
    let dir = tempfile::tempdir().unwrap();
    s.save(dir.path()).unwrap();
    let (back, notice) = LayeredSettings::load(dir.path());
    assert_eq!(notice.corrupt, None);
    assert_eq!(back, s);
}

#[test]
fn flat_settings_save_keeps_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = LayeredSettings::default();
    s.set(
        "grid.row_cap",
        &Level::Connection("c1".into()),
        Some(int(7)),
    )
    .unwrap();
    s.save(dir.path()).unwrap();
    Settings {
        page_size: 42,
        ..Settings::default()
    }
    .save(dir.path())
    .unwrap();
    let (back, _) = LayeredSettings::load(dir.path());
    assert_eq!(back.get("grid.page_size", None).0, int(42));
    assert_eq!(
        back.get("grid.row_cap", Some(("c1", "development"))).0,
        int(7)
    );
}

#[test]
fn safety_flags_only_tighten() {
    let mut s = LayeredSettings::default();
    let conn = Some(("c1", "production"));
    let on = SettingValue::Bool(true);
    let off = SettingValue::Bool(false);

    // The profile switches read-only on; a connection cannot switch it back off.
    s.set(
        "safety.read_only",
        &Level::Profile("production".into()),
        Some(on.clone()),
    )
    .unwrap();
    assert!(
        s.set(
            "safety.read_only",
            &Level::Connection("c1".into()),
            Some(off.clone())
        )
        .is_err()
    );
    assert_eq!(
        s.get("safety.read_only", conn),
        (on.clone(), Level::Profile("production".into()))
    );
    // Not even a global "off" loosens a profile's "on".
    s.set("safety.read_only", &Level::Global, Some(off.clone()))
        .unwrap();
    assert_eq!(s.get("safety.read_only", conn).0, on);

    // A connection can tighten what the profile left open.
    assert_eq!(s.get("safety.confirm_writes", conn).0, off);
    s.set(
        "safety.confirm_writes",
        &Level::Connection("c1".into()),
        Some(on.clone()),
    )
    .unwrap();
    assert_eq!(
        s.get("safety.confirm_writes", conn),
        (on, Level::Connection("c1".into()))
    );
    // Another connection is untouched.
    assert_eq!(
        s.get("safety.confirm_writes", Some(("c2", "production"))).0,
        off
    );

    // A hand-edited overrides file cannot loosen a flag or set a global-only key either.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("overrides.toml"),
        "[connection.c1]\n\"safety.read_only\" = false\n\"appearance.theme\" = \"dark\"\n\"grid.page_size\" = 9\n",
    )
    .unwrap();
    let (loaded, _) = LayeredSettings::load(dir.path());
    assert_eq!(
        loaded.value_at("safety.read_only", &Level::Connection("c1".into())),
        None
    );
    assert_eq!(
        loaded.value_at("appearance.theme", &Level::Connection("c1".into())),
        None
    );
    assert_eq!(
        loaded.get("grid.page_size", Some(("c1", "development"))).0,
        int(9)
    );
}

#[test]
fn keymap_toml_round_trip() {
    use sqlr_core::{ActionId, Chord, Keymap};
    use sqlr_store::StoredKeymap;

    let dir = tempfile::tempdir().unwrap();
    // No file: the sqlr preset.
    let loaded = StoredKeymap::load(dir.path());
    assert_eq!(loaded.preset, "sqlr");
    assert_eq!(loaded.keymap, Keymap::preset("sqlr").unwrap());
    assert_eq!(loaded.corrupt, None);

    // A customized keymap writes the preset and its difference, and reads back whole.
    let mut keymap = Keymap::preset("vscode").unwrap();
    keymap.set_chords(ActionId::RunStatement, vec![Chord::parse("Alt+R").unwrap()]);
    keymap.unbind(ActionId::Find);
    let stored = StoredKeymap {
        keymap,
        preset: "vscode".into(),
        corrupt: None,
    };
    stored.save(dir.path()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).unwrap();
    assert!(text.contains("preset = \"vscode\""), "{text}");
    assert!(text.contains("run_statement = [\"Alt+R\"]"), "{text}");
    assert!(text.contains("find = []"), "{text}");
    assert!(!text.contains("undo"), "{text}");
    assert_eq!(StoredKeymap::load(dir.path()), stored);

    // The default keymap writes only its name.
    let empty = tempfile::tempdir().unwrap();
    StoredKeymap::load(empty.path()).save(dir.path()).unwrap();
    let text = std::fs::read_to_string(dir.path().join("keymap.toml")).unwrap();
    assert_eq!(text.trim(), "preset = \"sqlr\"");
}

#[test]
fn corrupt_keymap_falls_back_and_keeps_the_file() {
    use sqlr_store::StoredKeymap;

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("keymap.toml"), "run = = nope\n").unwrap();
    let loaded = StoredKeymap::load(dir.path());
    assert_eq!(loaded.preset, "sqlr");
    let notice = loaded.corrupt.expect("a notice for the toast");
    assert!(notice.contains("keymap.toml.corrupt"), "{notice}");
    assert!(dir.path().join("keymap.toml.corrupt").exists());
}
