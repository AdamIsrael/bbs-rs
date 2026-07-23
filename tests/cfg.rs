//! `bbscfg`'s config-editing core (#141).
//!
//! The first test here is the load-bearing one: if a round trip isn't
//! byte-identical, the whole approach is wrong and we'd be silently destroying
//! the operator's file every time they saved.
//!
//! Everything is exercised against [`DEFAULT_CONFIG_TOML`] — the annotated
//! config the binary itself writes on first run, which is committed in
//! `src/config.rs`. Deliberately **not** the `bbs.toml` in the working
//! directory: that file is gitignored (a runtime artifact), so it's absent in
//! CI and, on a developer's machine, silently drifts from the shipped default
//! as settings are added. Testing against it means testing whatever happens to
//! be on disk.

use bbs_rs::cfg::{ConfigDoc, FieldValue, SECTIONS};
use bbs_rs::config::DEFAULT_CONFIG_TOML;
use std::path::PathBuf;

/// How many lines of the shipped config are comments. Asserted rather than
/// computed so the point survives: this file is *mostly documentation*, which
/// is the entire reason we edit it in place instead of regenerating it.
const COMMENT_LINES: usize = 206;

/// Real `[[doors]]` entries, ignoring the commented-out example the shipped
/// config carries — a plain `contains("[[doors]]")` matches that comment and
/// passes for the wrong reason.
fn uncommented_doors(text: &str) -> usize {
    text.lines()
        .filter(|l| l.trim_start().starts_with("[[doors]]"))
        .count()
}

/// Real `[art.screens]` headers, ignoring the commented-out example the shipped
/// config carries — same trap as [`uncommented_doors`].
fn uncommented_art_screens(text: &str) -> usize {
    text.lines()
        .filter(|l| l.trim_start().starts_with("[art.screens]"))
        .count()
}

fn comment_lines(text: &str) -> usize {
    text.lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count()
}

/// A scratch copy of the shipped default config, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("bbscfg-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("bbs.toml");
        std::fs::write(&path, DEFAULT_CONFIG_TOML).unwrap();
        Scratch(path)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn text(&self) -> String {
        std::fs::read_to_string(&self.0).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

/// **The load-bearing test.** Load the shipped config, save it untouched, and
/// get back exactly the same bytes.
///
/// 125 of `bbs.toml`'s ~200 lines are comments and ~25 settings ship commented
/// out. Serializing `Settings` back to TOML would produce a *valid* file that
/// had lost all of that — and it would look fine in review. This is what proves
/// we edit rather than regenerate.
#[test]
fn saving_an_unchanged_config_rewrites_it_byte_for_byte() {
    let scratch = Scratch::new("roundtrip");
    let before = scratch.text();

    let mut doc = ConfigDoc::load(scratch.path()).unwrap();
    assert!(!doc.is_dirty(), "loading alone changes nothing");
    doc.save().unwrap();

    assert_eq!(scratch.text(), before, "round trip must be byte-identical");
    assert_eq!(
        comment_lines(&before),
        COMMENT_LINES,
        "and the file really is mostly comments — if this number moves, \
         the test above is protecting something different than it was"
    );
}

/// Changing one setting changes exactly one line.
#[test]
fn editing_one_value_touches_only_that_line() {
    let scratch = Scratch::new("oneline");
    let before = scratch.text();

    let mut doc = ConfigDoc::load(scratch.path()).unwrap();
    doc.set("network", "port", FieldValue::Int(2323));
    assert!(doc.is_dirty());
    doc.save().unwrap();

    let after = scratch.text();
    let changed: Vec<(&str, &str)> = before
        .lines()
        .zip(after.lines())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(
        changed.len(),
        1,
        "expected one changed line, got {changed:?}"
    );
    assert_eq!(changed[0], ("port = 2222", "port = 2323"));
    assert_eq!(before.lines().count(), after.lines().count());
}

/// Setting a key that ships commented out adds a real entry and keeps the
/// comment, which is the explanation of what the setting does.
#[test]
fn setting_a_commented_out_key_keeps_the_comment() {
    let scratch = Scratch::new("commented");
    let before = scratch.text();
    assert!(
        before.contains("# tls_cert"),
        "precondition: it ships commented"
    );

    let mut doc = ConfigDoc::load(scratch.path()).unwrap();
    assert_eq!(doc.get("web", "tls_cert"), None, "not actually set");
    doc.set(
        "web",
        "tls_cert",
        FieldValue::Str("/etc/ssl/bbs.pem".into()),
    );
    doc.save().unwrap();

    let after = scratch.text();
    assert!(
        after.contains("# tls_cert"),
        "the explanatory comment survives"
    );
    assert!(after.contains("tls_cert = \"/etc/ssl/bbs.pem\""));
    assert_eq!(comment_lines(&after), COMMENT_LINES, "no comments lost");
}

/// Shapes the field editor doesn't model must survive untouched. This is the
/// property that makes "we don't support editing doors yet" harmless instead of
/// destructive.
#[test]
fn unmodelled_shapes_are_preserved() {
    let scratch = Scratch::new("unmodelled");
    let extra = "\n[[doors]]\nname = \"Adventure\"\ncommand = \"/usr/games/adventure\"\nargs = []\n\
                 \n[art.screens]\nboard_list = \"boards.ans\"\n";
    let mut original = scratch.text();
    original.push_str(extra);
    std::fs::write(scratch.path(), &original).unwrap();

    let mut doc = ConfigDoc::load(scratch.path()).unwrap();
    doc.set("bbs", "name", FieldValue::Str("Renamed".into()));
    doc.save().unwrap();

    let after = scratch.text();
    assert!(after.contains("[[doors]]"), "door definitions survive");
    assert!(after.contains("command = \"/usr/games/adventure\""));
    assert!(after.contains("[art.screens]"), "nested tables survive");
    assert!(after.contains("board_list = \"boards.ans\""));
    assert!(after.contains("name = \"Renamed\""));
}

/// Saving backs up what was there, because the operator may have hand-edited it
/// for years.
#[test]
fn saving_backs_up_the_previous_file() {
    let scratch = Scratch::new("backup");
    let before = scratch.text();

    let mut doc = ConfigDoc::load(scratch.path()).unwrap();
    doc.set("bbs", "name", FieldValue::Str("New Name".into()));
    doc.save().unwrap();

    let backup = scratch.path().with_extension("toml.bak");
    assert!(backup.exists(), "a .bak was written");
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        before,
        "and it holds the pre-edit contents"
    );
}

/// Every value kind survives a write/read cycle as itself.
#[test]
fn each_field_kind_round_trips() {
    let scratch = Scratch::new("kinds");
    let mut doc = ConfigDoc::load(scratch.path()).unwrap();

    doc.set("features", "guest", FieldValue::Bool(false));
    doc.set("limits", "max_posts", FieldValue::Int(42));
    doc.set("bbs", "sysop", FieldValue::Str("Adam".into()));
    doc.set(
        "accounts",
        "reserved_usernames",
        FieldValue::List(vec!["root".into(), "sysop".into()]),
    );
    doc.save().unwrap();

    let reread = ConfigDoc::load(scratch.path()).unwrap();
    assert_eq!(
        reread.get("features", "guest"),
        Some(FieldValue::Bool(false))
    );
    assert_eq!(reread.get("limits", "max_posts"), Some(FieldValue::Int(42)));
    assert_eq!(
        reread.get("bbs", "sysop"),
        Some(FieldValue::Str("Adam".into()))
    );
    assert_eq!(
        reread.get("accounts", "reserved_usernames"),
        Some(FieldValue::List(vec!["root".into(), "sysop".into()]))
    );
}

/// Unsetting removes the key so the built-in default applies again.
#[test]
fn unsetting_falls_back_to_the_default() {
    let scratch = Scratch::new("unset");
    let mut doc = ConfigDoc::load(scratch.path()).unwrap();

    doc.set("limits", "max_posts", FieldValue::Int(99));
    assert_eq!(doc.get("limits", "max_posts"), Some(FieldValue::Int(99)));
    assert!(doc.unset("limits", "max_posts"));
    assert_eq!(doc.get("limits", "max_posts"), None, "no longer set");
    assert_eq!(
        doc.effective("limits", "max_posts"),
        Some(FieldValue::Int(5)),
        "but the shipped default is what actually applies"
    );
}

/// A missing file starts from the annotated default, not a bare skeleton — a new
/// board deserves the same self-documenting config as an existing one.
#[test]
fn a_missing_config_starts_from_the_documented_default() {
    let dir = std::env::temp_dir().join(format!("bbscfg-new-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("bbs.toml");
    let _ = std::fs::remove_file(&path);

    let mut doc = ConfigDoc::load(&path).unwrap();
    assert!(doc.is_new());
    doc.set("bbs", "name", FieldValue::Str("Fresh Board".into()));
    doc.save().unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("name = \"Fresh Board\""));
    assert!(
        text.lines()
            .filter(|l| l.trim_start().starts_with('#'))
            .count()
            > 50,
        "the new file is documented too"
    );
    assert!(
        !path.with_extension("toml.bak").exists(),
        "nothing to back up"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Changing a startup-bound section is reported as needing a restart, and a
/// hot-reloadable one isn't. This mirrors `reload::warn_restart_only`.
#[test]
fn restart_only_sections_are_reported() {
    let scratch = Scratch::new("restart");
    let mut doc = ConfigDoc::load(scratch.path()).unwrap();

    doc.set("bbs", "name", FieldValue::Str("Renamed".into()));
    assert!(
        doc.restart_needed().is_empty(),
        "[bbs] reaches new sessions on the next connect"
    );

    doc.set("network", "port", FieldValue::Int(2323));
    assert_eq!(doc.restart_needed(), vec!["network"]);
    assert_eq!(doc.changed_sections(), vec!["bbs", "network"]);
}

/// Federation's constraints are checked while editing, using the same validator
/// the server runs at startup — so the operator learns now instead of on a
/// failed boot.
#[test]
fn federation_is_validated_the_way_startup_would() {
    let scratch = Scratch::new("validate");
    let mut doc = ConfigDoc::load(scratch.path()).unwrap();
    assert!(doc.validate().is_empty(), "the shipped config is clean");

    // Enabled with no origin: rejected, exactly as at startup.
    doc.set("federation", "enabled", FieldValue::Bool(true));
    let issues = doc.validate();
    assert!(
        issues
            .iter()
            .any(|i| i.section == "federation" && i.blocking),
        "an empty origin blocks: {issues:?}"
    );

    // A good origin still needs the web frontend, which serves every AP endpoint.
    doc.set(
        "federation",
        "origin",
        FieldValue::Str("https://bbs.example.com".into()),
    );
    let issues = doc.validate();
    assert!(
        issues.iter().any(|i| i.message.contains("[web] enabled")),
        "must flag that federation needs the web frontend: {issues:?}"
    );

    // With web on but not on 443, warn rather than block — a reverse proxy is a
    // legitimate way to satisfy this and we can't see it from here.
    doc.set("web", "enabled", FieldValue::Bool(true));
    let issues = doc.validate();
    assert!(
        issues.iter().all(|i| !i.blocking),
        "nothing blocking remains: {issues:?}"
    );
    assert!(
        issues
            .iter()
            .any(|i| i.message.contains("443") && !i.blocking),
        "port 443 is a warning, not a refusal: {issues:?}"
    );
}

/// An unparseable config fails to load, loudly. Deliberately the opposite of
/// the bug bbsctl had (#138), where a broken file silently became defaults.
#[test]
fn an_unparseable_config_fails_to_load() {
    let scratch = Scratch::new("broken");
    std::fs::write(scratch.path(), "this is not = = toml [[[\n").unwrap();
    let err = match ConfigDoc::load(scratch.path()) {
        Ok(_) => panic!("a broken config must not load"),
        Err(e) => e,
    };
    assert!(
        format!("{err:#}").contains("parsing"),
        "the error names what went wrong and where: {err:#}"
    );
}

/// Every setting in the shipped config is described by the schema. A setting
/// the editor can't see is one an operator can't configure — and it would be
/// invisible in review, since nothing else references the schema.
#[test]
fn the_schema_covers_the_shipped_config() {
    let doc: toml_edit::DocumentMut = DEFAULT_CONFIG_TOML.parse().unwrap();

    for section in SECTIONS {
        let Some(table) = doc.get(section.name).and_then(|i| i.as_table()) else {
            continue; // section absent from the shipped file is fine
        };
        for (key, _) in table.iter() {
            assert!(
                section.field(key).is_some(),
                "[{}] {key} is in the shipped config but missing from the schema — a setting \
                 the editor can't see is one an operator can't configure",
                section.name
            );
        }
    }
}

// ---- #141 Slice B: the editor state machine ---------------------------------

mod editor {
    use super::*;
    use bbs_rs::cfg::editor::{Action, Editor, Screen};
    use crossterm::event::{KeyCode, KeyEvent};

    fn ed(scratch: &Scratch) -> Editor {
        Editor::new(ConfigDoc::load(scratch.path()).unwrap())
    }

    fn press(e: &mut Editor, code: KeyCode) -> Action {
        e.on_key(KeyEvent::from(code))
    }

    fn typed(e: &mut Editor, text: &str) {
        for c in text.chars() {
            press(e, KeyCode::Char(c));
        }
    }

    /// Navigate to a section by name, from the section list.
    fn goto(e: &mut Editor, section: &str) {
        e.screen = Screen::Sections;
        e.section_sel = 0;
        while e.section().name != section {
            press(e, KeyCode::Down);
        }
        press(e, KeyCode::Enter);
    }

    fn goto_field(e: &mut Editor, section: &str, key: &str) {
        goto(e, section);
        while e.field().is_some_and(|f| f.key != key) {
            press(e, KeyCode::Down);
        }
    }

    /// Typing a value into a text field writes it to the document.
    #[test]
    fn editing_a_string_field_updates_the_document() {
        let scratch = Scratch::new("ed-str");
        let mut e = ed(&scratch);
        goto_field(&mut e, "bbs", "name");

        press(&mut e, KeyCode::Enter);
        assert_eq!(e.screen, Screen::Edit);
        for _ in 0..40 {
            press(&mut e, KeyCode::Backspace);
        }
        typed(&mut e, "Adam's Board");
        press(&mut e, KeyCode::Enter);

        assert_eq!(e.screen, Screen::Fields, "back to the field list");
        assert_eq!(
            e.doc.get("bbs", "name"),
            Some(FieldValue::Str("Adam's Board".into()))
        );
    }

    /// Escape leaves the value alone.
    #[test]
    fn cancelling_an_edit_changes_nothing() {
        let scratch = Scratch::new("ed-cancel");
        let mut e = ed(&scratch);
        let before = e.doc.get("bbs", "name");
        goto_field(&mut e, "bbs", "name");

        press(&mut e, KeyCode::Enter);
        typed(&mut e, "discarded");
        press(&mut e, KeyCode::Esc);

        assert_eq!(e.doc.get("bbs", "name"), before);
        assert!(!e.doc.is_dirty());
    }

    /// Booleans toggle in place rather than opening an editor to type "true".
    #[test]
    fn a_boolean_toggles_without_an_edit_screen() {
        let scratch = Scratch::new("ed-bool");
        let mut e = ed(&scratch);
        goto_field(&mut e, "features", "guest");

        press(&mut e, KeyCode::Enter);
        assert_eq!(e.screen, Screen::Fields, "no edit screen for a toggle");
        assert_eq!(
            e.doc.get("features", "guest"),
            Some(FieldValue::Bool(false))
        );

        press(&mut e, KeyCode::Enter);
        assert_eq!(e.doc.get("features", "guest"), Some(FieldValue::Bool(true)));
    }

    /// Enums cycle through their valid values, so an invalid one can't be typed.
    #[test]
    fn an_enum_cycles_through_its_options() {
        let scratch = Scratch::new("ed-enum");
        let mut e = ed(&scratch);
        goto_field(&mut e, "theme", "preset");

        press(&mut e, KeyCode::Enter);
        assert_eq!(
            e.doc.get("theme", "preset"),
            Some(FieldValue::Str("mono".into()))
        );
        press(&mut e, KeyCode::Enter);
        assert_eq!(
            e.doc.get("theme", "preset"),
            Some(FieldValue::Str("amber".into()))
        );
    }

    /// A number outside the schema's range is refused with an explanation,
    /// rather than written for the server to reject at boot.
    #[test]
    fn an_out_of_range_number_is_refused() {
        let scratch = Scratch::new("ed-range");
        let mut e = ed(&scratch);
        goto_field(&mut e, "network", "port");

        press(&mut e, KeyCode::Enter);
        for _ in 0..10 {
            press(&mut e, KeyCode::Backspace);
        }
        typed(&mut e, "99999");
        press(&mut e, KeyCode::Enter);

        assert_eq!(e.screen, Screen::Edit, "still editing — not accepted");
        assert!(e.status.contains("65535"), "says the range: {}", e.status);
        assert_eq!(
            e.doc.get("network", "port"),
            Some(FieldValue::Int(2222)),
            "the document is untouched"
        );
    }

    /// Text that isn't a number at all is refused the same way.
    #[test]
    fn a_non_numeric_entry_is_refused() {
        let scratch = Scratch::new("ed-nan");
        let mut e = ed(&scratch);
        goto_field(&mut e, "limits", "max_posts");

        press(&mut e, KeyCode::Enter);
        for _ in 0..6 {
            press(&mut e, KeyCode::Backspace);
        }
        typed(&mut e, "lots");
        press(&mut e, KeyCode::Enter);

        assert_eq!(e.screen, Screen::Edit);
        assert!(e.status.contains("not a number"), "{}", e.status);
    }

    /// A list is entered comma-separated and stored as an array.
    #[test]
    fn a_list_is_entered_comma_separated() {
        let scratch = Scratch::new("ed-list");
        let mut e = ed(&scratch);
        goto_field(&mut e, "accounts", "reserved_usernames");

        press(&mut e, KeyCode::Enter);
        for _ in 0..40 {
            press(&mut e, KeyCode::Backspace);
        }
        typed(&mut e, "root, admin , sysop");
        press(&mut e, KeyCode::Enter);

        assert_eq!(
            e.doc.get("accounts", "reserved_usernames"),
            Some(FieldValue::List(vec![
                "root".into(),
                "admin".into(),
                "sysop".into()
            ])),
            "whitespace trimmed, empties dropped"
        );
    }

    /// `u` removes the setting so the built-in default applies again — which is
    /// a different thing from setting it to an empty value.
    #[test]
    fn u_resets_a_field_to_its_default() {
        let scratch = Scratch::new("ed-unset");
        let mut e = ed(&scratch);
        goto_field(&mut e, "limits", "max_posts");

        press(&mut e, KeyCode::Enter);
        for _ in 0..6 {
            press(&mut e, KeyCode::Backspace);
        }
        typed(&mut e, "99");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.doc.get("limits", "max_posts"), Some(FieldValue::Int(99)));

        press(&mut e, KeyCode::Char('u'));
        assert_eq!(e.doc.get("limits", "max_posts"), None, "no longer set");
        let (shown, explicit) = e.shown_value("max_posts");
        assert_eq!(shown, "5", "the default shows through");
        assert!(!explicit, "and is marked as a default, not a choice");
    }

    /// The save screen names what changed and what needs a restart.
    #[test]
    fn the_save_screen_reports_restart_only_changes() {
        let scratch = Scratch::new("ed-save");
        let mut e = ed(&scratch);

        goto_field(&mut e, "network", "port");
        press(&mut e, KeyCode::Enter);
        for _ in 0..10 {
            press(&mut e, KeyCode::Backspace);
        }
        typed(&mut e, "2323");
        press(&mut e, KeyCode::Enter);

        press(&mut e, KeyCode::Char('s'));
        assert_eq!(e.screen, Screen::Save);
        let (changed, restart) = e.pending();
        assert_eq!(changed, vec!["network"]);
        assert_eq!(restart, vec!["network"], "listeners are bound at startup");

        press(&mut e, KeyCode::Char('y'));
        assert_eq!(e.screen, Screen::Sections);
        assert!(e.status.starts_with("Saved"), "{}", e.status);
        assert!(scratch.text().contains("port = 2323"));
        assert!(!e.doc.is_dirty(), "saving clears the dirty flag");
    }

    /// A config that wouldn't start is not written. Catching the mistake and
    /// then saving it anyway would be worse than not checking at all.
    #[test]
    fn a_blocking_problem_refuses_the_save() {
        let scratch = Scratch::new("ed-block");
        let before = scratch.text();
        let mut e = ed(&scratch);

        // Federation on with no origin — rejected fail-closed at startup.
        goto_field(&mut e, "federation", "enabled");
        press(&mut e, KeyCode::Enter);
        assert_eq!(
            e.doc.get("federation", "enabled"),
            Some(FieldValue::Bool(true))
        );

        press(&mut e, KeyCode::Char('s'));
        assert!(!e.blocking_issues().is_empty(), "the problem is found");

        press(&mut e, KeyCode::Char('y'));
        assert_eq!(e.screen, Screen::Save, "still on the save screen");
        assert!(e.status.contains("would not start"), "{}", e.status);
        assert_eq!(scratch.text(), before, "and nothing was written");
    }

    /// Quitting with unsaved work asks first.
    #[test]
    fn quitting_dirty_asks_before_discarding() {
        let scratch = Scratch::new("ed-quit");
        let mut e = ed(&scratch);

        assert_eq!(
            press(&mut e, KeyCode::Char('q')),
            Action::Quit,
            "clean quits"
        );

        let mut e = ed(&scratch);
        goto_field(&mut e, "features", "guest");
        press(&mut e, KeyCode::Enter); // toggle -> dirty
        e.screen = Screen::Sections;

        assert_eq!(press(&mut e, KeyCode::Char('q')), Action::None);
        assert_eq!(e.screen, Screen::ConfirmQuit);

        // Any other key goes back to editing.
        assert_eq!(press(&mut e, KeyCode::Char('x')), Action::None);
        assert_eq!(e.screen, Screen::Sections);

        press(&mut e, KeyCode::Char('q'));
        assert_eq!(
            press(&mut e, KeyCode::Char('y')),
            Action::Quit,
            "y discards"
        );
    }

    /// Editing through the UI preserves comments, exactly as the core does —
    /// the property is worth asserting at this level too, since it's the whole
    /// point and a UI is where it would be quietly lost.
    #[test]
    fn a_full_edit_session_preserves_the_comments() {
        let scratch = Scratch::new("ed-comments");
        let mut e = ed(&scratch);

        goto_field(&mut e, "bbs", "sysop");
        press(&mut e, KeyCode::Enter);
        typed(&mut e, "Adam");
        press(&mut e, KeyCode::Enter);

        goto_field(&mut e, "features", "oneliners");
        press(&mut e, KeyCode::Enter);

        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));

        let after = scratch.text();
        assert_eq!(comment_lines(&after), COMMENT_LINES);
        assert!(after.contains("sysop = \"Adam\""));
        assert!(after.contains("oneliners = false"));
    }
}

// ---- #145: door games (an array of tables) ----------------------------------

mod doors {
    use super::*;
    use bbs_rs::cfg::editor::{Editor, Screen};
    use crossterm::event::{KeyCode, KeyEvent};

    fn ed(scratch: &Scratch) -> Editor {
        Editor::new(ConfigDoc::load(scratch.path()).unwrap())
    }
    fn press(e: &mut Editor, code: KeyCode) {
        e.on_key(KeyEvent::from(code));
    }
    fn typed(e: &mut Editor, text: &str) {
        for c in text.chars() {
            press(e, KeyCode::Char(c));
        }
    }
    /// Open the Doors section from the section list.
    fn open_doors(e: &mut Editor) {
        e.screen = Screen::Sections;
        e.section_sel = 0;
        while e.section().name != "doors" {
            press(e, KeyCode::Down);
        }
        press(e, KeyCode::Enter);
        assert_eq!(
            e.screen,
            Screen::Doors,
            "the doors section is a list, not fields"
        );
    }
    /// Move to a door field by key and open it.
    fn open_door_field(e: &mut Editor, key: &str) {
        while e.door_field().is_some_and(|f| f.key != key) {
            press(e, KeyCode::Down);
        }
        press(e, KeyCode::Enter);
    }
    fn retype(e: &mut Editor, text: &str) {
        for _ in 0..60 {
            press(e, KeyCode::Backspace);
        }
        typed(e, text);
        press(e, KeyCode::Enter);
    }

    /// Adding a door writes a complete, parseable entry immediately — an
    /// operator who adds one and walks away must not be left with a config
    /// that won't load.
    #[test]
    fn adding_a_door_writes_a_valid_entry() {
        let scratch = Scratch::new("door-add");
        let mut e = ed(&scratch);
        open_doors(&mut e);

        press(&mut e, KeyCode::Char('a'));
        assert_eq!(e.screen, Screen::DoorFields, "drops you into its settings");
        assert_eq!(e.doc.door_count(), 1);

        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));

        let text = scratch.text();
        assert!(
            uncommented_doors(&text) == 1,
            "wrote exactly one real [[doors]] entry (the shipped config also has \
             a commented-out example, which a substring check would match)"
        );
        let parsed: bbs_rs::config::Settings = toml::from_str(&text).expect("must parse");
        assert_eq!(parsed.doors.len(), 1);
        assert_eq!(parsed.doors[0].name, "New door");
    }

    /// Editing a door's fields writes into that entry — not into the section,
    /// which has no fields of its own. The `Edit` screen is shared between the
    /// two, so committing to the wrong target would silently write nothing.
    #[test]
    fn editing_a_door_field_writes_into_that_entry() {
        let scratch = Scratch::new("door-edit");
        let mut e = ed(&scratch);
        open_doors(&mut e);
        press(&mut e, KeyCode::Char('a'));

        open_door_field(&mut e, "name");
        assert_eq!(e.screen, Screen::Edit);
        retype(&mut e, "Adventure");
        assert_eq!(
            e.screen,
            Screen::DoorFields,
            "returns to the door, not the section"
        );

        open_door_field(&mut e, "command");
        retype(&mut e, "/usr/games/adventure");
        open_door_field(&mut e, "args");
        retype(&mut e, "-q, --no-color");
        open_door_field(&mut e, "time_limit_secs");
        retype(&mut e, "900");

        assert_eq!(
            e.doc.door_get(0, "name"),
            Some(FieldValue::Str("Adventure".into()))
        );
        assert_eq!(
            e.doc.door_get(0, "args"),
            Some(FieldValue::List(vec!["-q".into(), "--no-color".into()]))
        );
        assert_eq!(
            e.doc.door_get(0, "time_limit_secs"),
            Some(FieldValue::Int(900))
        );

        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));
        let parsed: bbs_rs::config::Settings = toml::from_str(&scratch.text()).unwrap();
        assert_eq!(parsed.doors[0].command, "/usr/games/adventure");
        assert_eq!(parsed.doors[0].time_limit_secs, 900);
    }

    /// The drop file cycles through its valid values, blank included.
    #[test]
    fn the_drop_file_cycles_including_blank() {
        let scratch = Scratch::new("door-drop");
        let mut e = ed(&scratch);
        open_doors(&mut e);
        press(&mut e, KeyCode::Char('a'));

        open_door_field(&mut e, "drop_file");
        assert_eq!(
            e.doc.door_get(0, "drop_file"),
            Some(FieldValue::Str("door.sys".into()))
        );
        press(&mut e, KeyCode::Enter);
        assert_eq!(
            e.doc.door_get(0, "drop_file"),
            Some(FieldValue::Str("dorinfo1.def".into()))
        );
        press(&mut e, KeyCode::Enter);
        assert_eq!(
            e.doc.door_get(0, "drop_file"),
            Some(FieldValue::Str(String::new())),
            "wraps back to blank — writing no drop file is a real choice"
        );
    }

    /// Removing a door asks first, and takes only that entry.
    #[test]
    fn removing_a_door_leaves_the_others_intact() {
        let scratch = Scratch::new("door-remove");
        let mut e = ed(&scratch);
        open_doors(&mut e);

        for name in ["First", "Second", "Third"] {
            press(&mut e, KeyCode::Char('a'));
            open_door_field(&mut e, "name");
            retype(&mut e, name);
            press(&mut e, KeyCode::Esc); // back to the list
        }
        assert_eq!(e.doc.door_names(), vec!["First", "Second", "Third"]);

        e.door_sel = 1;
        press(&mut e, KeyCode::Char('d'));
        assert_eq!(e.screen, Screen::ConfirmRemoveDoor, "asks before removing");

        press(&mut e, KeyCode::Char('n'));
        assert_eq!(e.doc.door_count(), 3, "any other key keeps it");

        press(&mut e, KeyCode::Char('d'));
        press(&mut e, KeyCode::Char('y'));
        assert_eq!(e.doc.door_names(), vec!["First", "Third"]);
        assert!(
            e.status.contains("files on disk are untouched"),
            "says what it did and didn't do: {}",
            e.status
        );
    }

    /// Removing the last door leaves no `[[doors]]` header behind, and the
    /// result reparses as a board with no doors.
    ///
    /// Note this holds because an empty `ArrayOfTables` serializes to nothing,
    /// not because of any cleanup we do — a mutation test proved an explicit
    /// removal changed nothing, so that code is gone.
    #[test]
    fn removing_the_last_door_leaves_no_trace() {
        let scratch = Scratch::new("door-last");
        let mut e = ed(&scratch);
        open_doors(&mut e);
        press(&mut e, KeyCode::Char('a'));
        press(&mut e, KeyCode::Esc);

        press(&mut e, KeyCode::Char('d'));
        press(&mut e, KeyCode::Char('y'));
        assert_eq!(e.doc.door_count(), 0);

        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));
        let text = scratch.text();
        assert_eq!(uncommented_doors(&text), 0, "no empty array left behind");
        let parsed: bbs_rs::config::Settings = toml::from_str(&text).unwrap();
        assert!(parsed.doors.is_empty(), "and it reparses with no doors");
    }

    /// Menu order is what callers see, so it can be changed without deleting
    /// and re-adding.
    #[test]
    fn doors_can_be_reordered() {
        let scratch = Scratch::new("door-order");
        let mut e = ed(&scratch);
        open_doors(&mut e);

        for name in ["A", "B", "C"] {
            press(&mut e, KeyCode::Char('a'));
            open_door_field(&mut e, "name");
            retype(&mut e, name);
            press(&mut e, KeyCode::Esc);
        }

        e.door_sel = 2;
        press(&mut e, KeyCode::Char('K')); // move C up
        assert_eq!(e.doc.door_names(), vec!["A", "C", "B"]);
        assert_eq!(e.door_sel, 1, "selection follows the door");

        press(&mut e, KeyCode::Char('K'));
        assert_eq!(e.doc.door_names(), vec!["C", "A", "B"]);
        press(&mut e, KeyCode::Char('K'));
        assert_eq!(
            e.doc.door_names(),
            vec!["C", "A", "B"],
            "can't go past the top"
        );

        press(&mut e, KeyCode::Char('J'));
        assert_eq!(e.doc.door_names(), vec!["A", "C", "B"]);
    }

    /// A hand-written door keeps its own comments through an unrelated edit,
    /// and through an edit to a *different* door.
    #[test]
    fn hand_written_doors_keep_their_comments() {
        let scratch = Scratch::new("door-comments");
        let mut text = scratch.text();
        text.push_str(
            "\n# The good one\n[[doors]]\nname = \"Adventure\"\ncommand = \"/usr/games/adventure\"\n\
             \n# Needs the old terminal\n[[doors]]\nname = \"Trade Wars\"\ncommand = \"/opt/tw2002\"\n",
        );
        std::fs::write(scratch.path(), &text).unwrap();

        let mut e = ed(&scratch);
        open_doors(&mut e);
        assert_eq!(e.doc.door_names(), vec!["Adventure", "Trade Wars"]);

        // Edit the second door only.
        e.door_sel = 1;
        press(&mut e, KeyCode::Enter);
        open_door_field(&mut e, "time_limit_secs");
        retype(&mut e, "1200");
        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));

        let after = scratch.text();
        assert!(
            after.contains("# The good one"),
            "the first door's comment survives"
        );
        assert!(after.contains("# Needs the old terminal"));
        assert!(after.contains("time_limit_secs = 1200"));
        assert_eq!(
            comment_lines(&after),
            COMMENT_LINES + 2,
            "only the two added comments, nothing lost"
        );
    }

    /// Round trip with doors present: load, save unchanged, byte-identical.
    /// The Slice A test only proved doors survive an *unrelated* edit.
    #[test]
    fn a_config_with_doors_round_trips() {
        let scratch = Scratch::new("door-roundtrip");
        let mut text = scratch.text();
        text.push_str(
            "\n[[doors]]\nname = \"Adventure\"\ncommand = \"/usr/games/adventure\"\n\
             args = [\"-q\"]\ntime_limit_secs = 900\ndrop_file = \"dorinfo1.def\"\n",
        );
        std::fs::write(scratch.path(), &text).unwrap();

        let mut doc = ConfigDoc::load(scratch.path()).unwrap();
        assert!(!doc.is_dirty());
        doc.save().unwrap();
        assert_eq!(scratch.text(), text, "byte-identical with doors present");
    }

    /// The door schema covers every field of the Door config struct — the same
    /// guarantee the section schema has, so a new door setting can't be
    /// silently unconfigurable.
    #[test]
    fn the_door_schema_covers_every_door_field() {
        let sample = "[[doors]]\nname = \"x\"\ncommand = \"y\"\nargs = []\ncwd = \"/tmp\"\n\
                      time_limit_secs = 60\ndrop_file = \"door.sys\"\n";
        // Proves the sample names real fields and nothing more.
        let parsed: bbs_rs::config::Settings = toml::from_str(sample).unwrap();
        assert_eq!(parsed.doors.len(), 1);

        let doc: toml_edit::DocumentMut = sample.parse().unwrap();
        let table = doc["doors"].as_array_of_tables().unwrap().get(0).unwrap();
        for (key, _) in table.iter() {
            assert!(
                bbs_rs::cfg::schema::DOOR_FIELDS
                    .iter()
                    .any(|f| f.key == key),
                "door field {key:?} is missing from DOOR_FIELDS"
            );
        }
    }
}

// ---- #146: per-screen art (a nested table) ----------------------------------

mod art_screens {
    use super::*;
    use bbs_rs::cfg::editor::{Editor, Screen};
    use crossterm::event::{KeyCode, KeyEvent};

    fn ed(scratch: &Scratch) -> Editor {
        Editor::new(ConfigDoc::load(scratch.path()).unwrap())
    }
    fn press(e: &mut Editor, code: KeyCode) {
        e.on_key(KeyEvent::from(code));
    }
    fn typed(e: &mut Editor, text: &str) {
        for c in text.chars() {
            press(e, KeyCode::Char(c));
        }
    }
    fn open_art(e: &mut Editor) {
        e.screen = Screen::Sections;
        e.section_sel = 0;
        while e.section().name != "art.screens" {
            press(e, KeyCode::Down);
        }
        press(e, KeyCode::Enter);
        assert_eq!(e.screen, Screen::ArtScreens);
    }
    fn select(e: &mut Editor, key: &str) {
        e.art_sel = 0;
        let keys = bbs_rs::app::ART_SCREEN_KEYS;
        while keys[e.art_sel].0 != key {
            press(e, KeyCode::Down);
        }
    }

    /// Setting a screen's art writes `[art.screens]` as a nested table under
    /// `[art]`, and it reparses into the Art config.
    #[test]
    fn setting_art_writes_a_nested_table() {
        let scratch = Scratch::new("art-set");
        let mut e = ed(&scratch);
        open_art(&mut e);
        select(&mut e, "board_list");

        press(&mut e, KeyCode::Enter);
        assert_eq!(e.screen, Screen::Edit);
        typed(&mut e, "boards.ans");
        press(&mut e, KeyCode::Enter);
        assert_eq!(
            e.screen,
            Screen::ArtScreens,
            "back to the art list, not a section"
        );

        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));

        let text = scratch.text();
        assert_eq!(
            uncommented_art_screens(&text),
            1,
            "wrote a real [art.screens] header (not the commented-out example)"
        );
        assert!(text.contains("board_list = \"boards.ans\""));
        let parsed: bbs_rs::config::Settings = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed.art.screens.get("board_list").map(String::as_str),
            Some("boards.ans")
        );
    }

    /// The rows offered are exactly the keys the server matches — a UII can't
    /// offer one that gets silently ignored.
    #[test]
    fn the_rows_are_exactly_the_servers_keys() {
        let scratch = Scratch::new("art-keys");
        let e = ed(&scratch);
        for (key, _, _) in e.art_rows() {
            assert!(
                bbs_rs::app::screen_from_art_key(key).is_some(),
                "art row {key:?} is not a key the server understands"
            );
        }
    }

    /// Clearing with 'u' removes the entry, and removing the last one drops the
    /// `[art.screens]` table rather than leaving an empty header.
    #[test]
    fn clearing_the_last_entry_drops_the_table() {
        let scratch = Scratch::new("art-clear");
        let mut e = ed(&scratch);
        open_art(&mut e);
        select(&mut e, "help");
        press(&mut e, KeyCode::Enter);
        typed(&mut e, "help.ans");
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.doc.art_screen_get("help").as_deref(), Some("help.ans"));

        press(&mut e, KeyCode::Char('u'));
        assert_eq!(e.doc.art_screen_get("help"), None, "cleared");

        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));
        let text = scratch.text();
        assert_eq!(
            uncommented_art_screens(&text),
            0,
            "no empty table header left behind"
        );
        // And it reparses with no per-screen art.
        let parsed: bbs_rs::config::Settings = toml::from_str(&text).unwrap();
        assert!(parsed.art.screens.is_empty());
    }

    /// A blank value clears rather than writing an empty filename the loader
    /// would try to open.
    #[test]
    fn a_blank_value_clears_the_entry() {
        let scratch = Scratch::new("art-blank");
        let mut e = ed(&scratch);
        e.doc.art_screen_set("stats", "stats.ans");
        assert_eq!(e.doc.art_screen_get("stats").as_deref(), Some("stats.ans"));

        e.doc.art_screen_set("stats", "   ");
        assert_eq!(
            e.doc.art_screen_get("stats"),
            None,
            "blank clears, not empty-string"
        );
    }

    /// A referenced file that isn't on disk is reported as a warning when saving
    /// — the failure a config UI is best placed to catch, since at runtime it's
    /// silent.
    #[test]
    fn a_missing_art_file_is_flagged() {
        let dir = std::env::temp_dir().join(format!("bbscfg-art-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("art")).unwrap();
        let path = dir.join("bbs.toml");
        std::fs::write(&path, DEFAULT_CONFIG_TOML).unwrap();

        let mut doc = ConfigDoc::load(&path).unwrap();
        // One file that exists, one that doesn't.
        std::fs::write(dir.join("art").join("real.ans"), b"art").unwrap();
        doc.art_screen_set("main_menu", "real.ans");
        doc.art_screen_set("help", "typo.ans");

        let issues = doc.validate();
        let art: Vec<_> = issues.iter().filter(|i| i.section == "art").collect();
        assert_eq!(art.len(), 1, "only the missing one is flagged: {art:?}");
        assert!(art[0].message.contains("typo.ans"));
        assert!(!art[0].blocking, "missing art is a warning, not a refusal");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A hand-written [art.screens] with comments round-trips and survives an
    /// edit to a *different* key.
    #[test]
    fn hand_written_art_survives_an_edit() {
        let scratch = Scratch::new("art-handwritten");
        let mut text = scratch.text();
        text.push_str(
            "\n[art.screens]\n# the good backdrop\nboard_list = \"boards.ans\"\nhelp = \"help.ans\"\n",
        );
        std::fs::write(scratch.path(), &text).unwrap();

        let mut e = ed(&scratch);
        open_art(&mut e);
        assert_eq!(
            e.doc.art_screen_get("board_list").as_deref(),
            Some("boards.ans")
        );

        // Edit a third key; the first two must be untouched.
        select(&mut e, "mailbox");
        press(&mut e, KeyCode::Enter);
        typed(&mut e, "mail.ans");
        press(&mut e, KeyCode::Enter);
        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));

        let after = scratch.text();
        assert!(after.contains("# the good backdrop"), "comment survives");
        assert!(after.contains("board_list = \"boards.ans\""));
        assert!(after.contains("mailbox = \"mail.ans\""));
    }
}

// ---- #147: seeded boards + first-run detection ------------------------------

mod seed_boards {
    use super::*;
    use bbs_rs::cfg::editor::{Editor, Screen};
    use bbs_rs::cfg::seed::{self, SeedStatus};
    use crossterm::event::{KeyCode, KeyEvent};

    fn ed(scratch: &Scratch) -> Editor {
        Editor::new(ConfigDoc::load(scratch.path()).unwrap())
    }
    fn press(e: &mut Editor, code: KeyCode) {
        e.on_key(KeyEvent::from(code));
    }
    fn typed(e: &mut Editor, text: &str) {
        for c in text.chars() {
            press(e, KeyCode::Char(c));
        }
    }
    fn open_seed(e: &mut Editor) {
        e.screen = Screen::Sections;
        e.section_sel = 0;
        while e.section().name != "seed" {
            press(e, KeyCode::Down);
        }
        press(e, KeyCode::Enter);
        assert_eq!(e.screen, Screen::SeedBoards);
    }
    fn retype(e: &mut Editor, text: &str) {
        for _ in 0..40 {
            press(e, KeyCode::Backspace);
        }
        typed(e, text);
        press(e, KeyCode::Enter);
    }

    /// Adding a board writes an inline table into `[seed] boards` and reparses.
    #[test]
    fn adding_a_seed_board_writes_an_inline_table() {
        let scratch = Scratch::new("seed-add");
        let mut e = ed(&scratch);
        open_seed(&mut e);

        press(&mut e, KeyCode::Char('a'));
        assert_eq!(e.screen, Screen::SeedBoardFields);
        // Name it (field 0): open the editor, then type.
        press(&mut e, KeyCode::Enter);
        assert_eq!(e.screen, Screen::Edit);
        retype(&mut e, "News");
        assert_eq!(e.screen, Screen::SeedBoardFields);
        // Set min_write (field 3) to admin — an announcement board.
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Down);
        press(&mut e, KeyCode::Down);
        assert_eq!(e.seed_board_field().unwrap().key, "min_write");
        press(&mut e, KeyCode::Enter); // enum cycles user -> admin
        assert_eq!(
            e.doc.seed_board_get(0, "min_write"),
            Some(FieldValue::Str("admin".into()))
        );
        assert_eq!(
            e.doc.seed_board_get(0, "name"),
            Some(FieldValue::Str("News".into()))
        );

        press(&mut e, KeyCode::Char('s'));
        press(&mut e, KeyCode::Char('y'));

        let parsed: bbs_rs::config::Settings = toml::from_str(&scratch.text()).unwrap();
        let boards = parsed.seed.boards.expect("boards set");
        assert_eq!(boards.len(), 1);
        assert_eq!(boards[0].name, "News");
        assert_eq!(boards[0].min_write, "admin");
    }

    /// An empty boards list is meaningful — "seed no boards", distinct from the
    /// absent key that means "use the built-in defaults" — so removing the last
    /// board leaves `boards = []`, not an absent key.
    #[test]
    fn removing_the_last_seed_board_leaves_an_empty_list() {
        let scratch = Scratch::new("seed-empty");
        let mut e = ed(&scratch);
        open_seed(&mut e);

        press(&mut e, KeyCode::Char('a'));
        press(&mut e, KeyCode::Esc); // back to the list; board is row 1
        assert_eq!(e.doc.seed_board_count(), Some(1));

        e.seed_sel = 1;
        e.seed_on_password = false;
        press(&mut e, KeyCode::Char('d'));
        assert_eq!(e.screen, Screen::ConfirmRemoveSeedBoard);
        press(&mut e, KeyCode::Char('y'));

        assert_eq!(
            e.doc.seed_board_count(),
            Some(0),
            "explicit empty list, not the absent key"
        );
        let parsed: bbs_rs::config::Settings = toml::from_str(&e.doc.to_text()).unwrap();
        assert_eq!(parsed.seed.boards, Some(vec![]), "seeds no boards");
    }

    /// A config that never mentions seed boards leaves the key absent, so the
    /// built-in defaults still apply.
    #[test]
    fn an_untouched_config_leaves_seed_boards_unset() {
        let scratch = Scratch::new("seed-untouched");
        let mut e = ed(&scratch);
        open_seed(&mut e);
        press(&mut e, KeyCode::Esc);

        assert!(!e.doc.is_dirty(), "opening the seed screen changes nothing");
        let parsed: bbs_rs::config::Settings = toml::from_str(&e.doc.to_text()).unwrap();
        assert_eq!(parsed.seed.boards, None, "defaults still apply");
    }

    // ---- the first-run detection: the substance of this issue ---------------

    /// A database that doesn't exist yet: seeding will run.
    #[test]
    fn a_missing_database_will_seed() {
        let dir = std::env::temp_dir().join(format!("seed-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let status = seed::status("sqlite://nope.db?mode=rwc", &dir);
        assert_eq!(status, SeedStatus::WillSeed);
        // And the check must NOT have created it — bbscfg opens configs, not DBs.
        assert!(
            !dir.join("nope.db").exists(),
            "checking must not create the database"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A database that already has boards: seeding is skipped, and we report the
    /// count so the operator understands why their edit does nothing.
    #[test]
    fn a_database_with_boards_is_reported_as_already_seeded() {
        let dir = std::env::temp_dir().join(format!("seed-has-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("bbs.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());

        // Build a real migrated database with two seed boards, in a throwaway
        // runtime that's gone before the sync check runs (seed::status uses
        // block_on internally, so it can't be called from inside a runtime).
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = bbs_rs::db::connect(&url).await.unwrap();
            bbs_rs::db::run_migrations(&pool).await.unwrap();
            bbs_rs::services::boards::ensure_default_boards(
                &pool,
                &[seed_board("General"), seed_board("Announcements")],
            )
            .await
            .unwrap();
            pool.close().await;
        });
        drop(rt);

        assert_eq!(
            seed::status(&url, &dir),
            SeedStatus::AlreadySeeded { boards: 2 }
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A migrated but board-less database still counts as "will seed" — that's
    /// exactly the state a fresh `bbsctl migrate` leaves it in.
    #[test]
    fn a_migrated_but_empty_database_will_seed() {
        let dir = std::env::temp_dir().join(format!("seed-empty-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("bbs.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let pool = bbs_rs::db::connect(&url).await.unwrap();
            bbs_rs::db::run_migrations(&pool).await.unwrap();
            pool.close().await;
        });
        drop(rt);
        assert_eq!(seed::status(&url, &dir), SeedStatus::WillSeed);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An in-memory or otherwise non-file URL can't be inspected; we say so
    /// rather than guess either way.
    #[test]
    fn a_non_file_database_is_unknown() {
        let dir = std::env::temp_dir();
        assert!(matches!(
            seed::status("sqlite::memory:", &dir),
            SeedStatus::Unknown { .. }
        ));
        assert!(matches!(
            seed::status("postgres://localhost/bbs", &dir),
            SeedStatus::Unknown { .. }
        ));
    }

    fn seed_board(name: &str) -> bbs_rs::config::SeedBoard {
        bbs_rs::config::SeedBoard {
            name: name.into(),
            description: String::new(),
            min_read: "guest".into(),
            min_write: "user".into(),
        }
    }
}
