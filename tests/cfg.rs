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
const COMMENT_LINES: usize = 128;

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
