//! `bbscfg` — a TUI for building and editing `bbs.toml` (#141).
//!
//! Third binary alongside `bbs-rs` and `bbsctl`. Every `[section]` is a screen;
//! settings are edited or picked from their valid values; saving writes the file
//! back **in place**, preserving its comments and anything this tool doesn't
//! model. All of that lives in [`bbs_rs::cfg`]; this file is the terminal.
//!
//! Scope: `bbscfg` edits the config file and nothing else. Runtime state — bans,
//! federation allow/block lists, users — stays in `bbsctl`.

use std::io::{self, Stdout};
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use bbs_rs::cfg::ConfigDoc;
use bbs_rs::cfg::editor::{Action, Editor, Screen};
use bbs_rs::cfg::schema::{DOOR_FIELDS, FieldKind, SECTIONS, SEED_BOARD_FIELDS};
use bbs_rs::cfg::seed::SeedStatus;

#[derive(Parser)]
#[command(
    name = "bbscfg",
    about = "Configure a bbs-rs board",
    long_about = "Edit bbs.toml through a TUI. Reads the existing file if there is one, so it \
                  works for adjusting a running board as well as setting up a new one. Comments \
                  and hand-written sections are preserved."
)]
struct Cli {
    /// Path to the config file.
    #[arg(long, default_value = "bbs.toml")]
    config: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let doc = ConfigDoc::load(&cli.config)
        .with_context(|| format!("loading {}", cli.config.display()))?;
    let mut editor = Editor::new(doc);

    let mut terminal = setup()?;
    let result = run(&mut terminal, &mut editor);
    restore(&mut terminal)?;

    // Restore the terminal before reporting anything, or an error message lands
    // in the alternate screen and vanishes with it.
    result
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, editor: &mut Editor) -> Result<()> {
    loop {
        terminal.draw(|f| draw(f, editor))?;
        if let Event::Key(key) = event::read()? {
            // Windows reports press and release; act on press only.
            if key.kind != KeyEventKind::Release && editor.on_key(key) == Action::Quit {
                return Ok(());
            }
        }
    }
}

fn draw(f: &mut Frame, editor: &Editor) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(f.area());

    title_bar(f, chunks[0], editor);
    match editor.screen {
        Screen::Sections => sections(f, chunks[1], editor),
        Screen::Fields => fields(f, chunks[1], editor),
        Screen::Edit => edit(f, chunks[1], editor),
        Screen::Save => save(f, chunks[1], editor),
        Screen::ConfirmQuit => confirm_quit(f, chunks[1], editor),
        Screen::Doors => doors(f, chunks[1], editor),
        Screen::ArtScreens => art_screens(f, chunks[1], editor),
        Screen::SeedBoards => seed_boards(f, chunks[1], editor),
        Screen::SeedBoardFields => seed_board_fields(f, chunks[1], editor),
        Screen::ConfirmRemoveSeedBoard => confirm_remove_seed_board(f, chunks[1], editor),
        Screen::DoorFields => door_fields(f, chunks[1], editor),
        Screen::ConfirmRemoveDoor => confirm_remove_door(f, chunks[1], editor),
    }
    help_pane(f, chunks[2], editor);
    status_bar(f, chunks[3], editor);
}

fn title_bar(f: &mut Frame, area: Rect, editor: &Editor) {
    let dirty = if editor.doc.is_dirty() {
        " • unsaved"
    } else {
        ""
    };
    let text = format!(" bbscfg · {}{dirty} ", editor.doc.path().display());
    f.render_widget(
        Paragraph::new(text).style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
    );
}

fn sections(f: &mut Frame, area: Rect, editor: &Editor) {
    let (changed, _) = editor.pending();
    let title_width = SECTIONS
        .iter()
        .map(|s| s.title.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let lines: Vec<Line> = SECTIONS
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut spans = vec![Span::raw(format!(
                "{} {:<title_width$}",
                if i == editor.section_sel { ">" } else { " " },
                s.title
            ))];
            spans.push(Span::styled(
                format!("[{}]", s.name),
                Style::default().add_modifier(Modifier::DIM),
            ));
            if changed.contains(&s.name) {
                spans.push(Span::styled(
                    "  changed",
                    Style::default().add_modifier(Modifier::BOLD),
                ));
            }
            if s.restart_only {
                spans.push(Span::styled(
                    "  (restart)",
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
            let style = if i == editor.section_sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(spans).style(style)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Sections ")),
        area,
    );
}

fn fields(f: &mut Frame, area: Rect, editor: &Editor) {
    let section = editor.section();
    // Sized from the section's own labels rather than a fixed number: the
    // longest today is 28 characters, and a hardcoded column silently runs the
    // label into its value the moment someone adds a longer one.
    let label_width = section
        .fields
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let lines: Vec<Line> = section
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let (value, explicit) = editor.shown_value(field.key);
            let shown = match field.kind {
                FieldKind::Str | FieldKind::Path if value.is_empty() => "(blank)".to_string(),
                FieldKind::StrList if value.is_empty() => "(none)".to_string(),
                _ => value,
            };
            let mut spans = vec![
                Span::raw(format!(
                    "{} {:<label_width$}",
                    if i == editor.field_sel { ">" } else { " " },
                    field.label
                )),
                Span::raw(shown),
            ];
            // An unset value is the built-in default showing through, which is
            // worth distinguishing from one the operator chose.
            if !explicit {
                spans.push(Span::styled(
                    "  (default)",
                    Style::default().add_modifier(Modifier::DIM),
                ));
            }
            let style = if i == editor.field_sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(spans).style(style)
        })
        .collect();
    let title = format!(
        " {} [{}]{} ",
        section.title,
        section.name,
        if section.restart_only {
            " · needs a restart"
        } else {
            ""
        }
    );
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn edit(f: &mut Frame, area: Rect, editor: &Editor) {
    let Some(field) = editor.edit_field() else {
        return;
    };
    let hint = match field.kind {
        FieldKind::Int { min, max } => format!("a number from {min} to {max}"),
        FieldKind::StrList => "comma-separated".to_string(),
        FieldKind::Path => "a path".to_string(),
        _ => "text".to_string(),
    };
    let body = vec![
        Line::from(format!("{} ({hint})", field.label)),
        Line::from(""),
        Line::from(vec![
            Span::raw("> "),
            Span::styled(
                editor.input.clone(),
                Style::default().add_modifier(Modifier::REVERSED),
            ),
        ]),
    ];
    f.render_widget(
        Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title(" Edit "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn save(f: &mut Frame, area: Rect, editor: &Editor) {
    let (changed, restart) = editor.pending();
    let mut lines = Vec::new();

    if changed.is_empty() {
        lines.push(Line::from("Nothing has changed."));
    } else {
        lines.push(Line::from(format!("Changed: {}", changed.join(", "))));
        if !restart.is_empty() {
            lines.push(Line::from(Span::styled(
                format!(
                    "Needs a restart to take effect: {} — the listeners, database and \
                     federation are bound at startup.",
                    restart.join(", ")
                ),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }
        if !editor.doc.is_new() {
            lines.push(Line::from(Span::styled(
                "The previous file is kept as bbs.toml.bak.",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
    }

    if !editor.issues.is_empty() {
        lines.push(Line::from(""));
        for issue in &editor.issues {
            let tag = if issue.blocking { "BLOCKS" } else { "warning" };
            let where_ = if issue.section.is_empty() {
                String::new()
            } else {
                format!("[{}] ", issue.section)
            };
            lines.push(Line::from(Span::styled(
                format!("{tag}: {where_}{}", issue.message),
                if issue.blocking {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(if editor.blocking_issues().is_empty() {
        "Write the file?  y = save   n = back"
    } else {
        "Blocking problems must be fixed first.  n = back"
    }));

    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Save "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn seed_boards(f: &mut Frame, area: Rect, editor: &Editor) {
    let mut lines = Vec::new();

    // The whole point of the section: say whether any of this will take effect.
    match &editor.seed_status {
        SeedStatus::WillSeed => lines.push(Line::from(Span::styled(
            "This database has no boards yet, so these settings apply on the next start.",
            Style::default().add_modifier(Modifier::DIM),
        ))),
        SeedStatus::AlreadySeeded { boards } => lines.push(Line::from(Span::styled(
            format!(
                "The database already has {boards} board(s), so seeding is skipped — edits                  here change the file but nothing else until the database is recreated."
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        SeedStatus::Unknown { reason } => lines.push(Line::from(Span::styled(
            format!("Can't tell whether seeding will run ({reason})."),
            Style::default().add_modifier(Modifier::DIM),
        ))),
    }
    lines.push(Line::from(""));

    // Row 0: the guest password (or a note that the default applies).
    let pw_shown = editor
        .doc
        .get("seed", "guest_password")
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(default: guest)".to_string());
    lines.push(seed_row(
        editor.seed_sel == 0,
        "Guest password",
        &pw_shown,
        14,
    ));

    // Then each board.
    let names = editor.doc.seed_board_names();
    if names.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No custom boards — the built-in General + Announcements are used. 'a' adds one.",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for (i, name) in names.iter().enumerate() {
            let write = editor
                .doc
                .seed_board_get(i, "min_write")
                .map(|v| v.display())
                .unwrap_or_default();
            lines.push(seed_row(
                editor.seed_sel == i + 1,
                &format!("Board: {name}"),
                &format!("post: {write}"),
                14,
            ));
        }
    }

    f.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" First-run seeding "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn seed_row(selected: bool, label: &str, value: &str, width: usize) -> Line<'static> {
    let style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(format!(
            "{} {:<width$}",
            if selected { ">" } else { " " },
            label
        )),
        Span::raw(value.to_string()),
    ])
    .style(style)
}

fn seed_board_fields(f: &mut Frame, area: Rect, editor: &Editor) {
    let idx = editor.seed_sel.saturating_sub(1);
    let label_width = SEED_BOARD_FIELDS
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let lines: Vec<Line> = SEED_BOARD_FIELDS
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let value = editor
                .doc
                .seed_board_get(idx, field.key)
                .map(|v| v.display())
                .unwrap_or_default();
            let shown = if value.is_empty() {
                "(blank)".to_string()
            } else {
                value
            };
            let style = if i == editor.seed_field_sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::raw(format!(
                    "{} {:<label_width$}",
                    if i == editor.seed_field_sel { ">" } else { " " },
                    field.label
                )),
                Span::raw(shown),
            ])
            .style(style)
        })
        .collect();
    let name = editor
        .doc
        .seed_board_names()
        .get(idx)
        .cloned()
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Seed board: {name} ")),
        ),
        area,
    );
}

fn confirm_remove_seed_board(f: &mut Frame, area: Rect, editor: &Editor) {
    let name = editor
        .doc
        .seed_board_names()
        .get(editor.seed_sel.saturating_sub(1))
        .cloned()
        .unwrap_or_default();
    let lines = vec![
        Line::from(format!("Remove the seeded board \"{name}\"?")),
        Line::from(""),
        Line::from("This only changes what a fresh database would be seeded with."),
        Line::from(""),
        Line::from("y = remove    any other key = keep"),
    ];
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Remove seed board "),
        ),
        area,
    );
}

fn art_screens(f: &mut Frame, area: Rect, editor: &Editor) {
    let rows = editor.art_rows();
    let width = rows
        .iter()
        .map(|(_, label, _)| label.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    // Which files are missing, so a typo is visible in the list rather than
    // only at save time.
    let missing = editor.doc.missing_art_files();
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, (key, label, file))| {
            let mut spans = vec![
                Span::raw(format!(
                    "{} {:<width$}",
                    if i == editor.art_sel { ">" } else { " " },
                    label
                )),
                Span::raw(if file.is_empty() {
                    "(none)".to_string()
                } else {
                    file.clone()
                }),
            ];
            if missing.iter().any(|(k, _)| k == key) {
                spans.push(Span::styled(
                    "  not found",
                    Style::default().add_modifier(Modifier::BOLD),
                ));
            }
            let style = if i == editor.art_sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(spans).style(style)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Per-screen art "),
        ),
        area,
    );
}

fn doors(f: &mut Frame, area: Rect, editor: &Editor) {
    let names = editor.doc.door_names();
    if names.is_empty() {
        f.render_widget(
            Paragraph::new(vec![
                Line::from("No door games configured."),
                Line::from(""),
                Line::from("Press 'a' to add one."),
            ])
            .block(Block::default().borders(Borders::ALL).title(" Door games ")),
            area,
        );
        return;
    }
    let width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0) + 2;
    let lines: Vec<Line> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let command = editor
                .doc
                .door_get(i, "command")
                .map(|v| v.display())
                .unwrap_or_default();
            let style = if i == editor.door_sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::raw(format!(
                    "{} {:<width$}",
                    if i == editor.door_sel { ">" } else { " " },
                    name
                )),
                Span::styled(command, Style::default().add_modifier(Modifier::DIM)),
            ])
            .style(style)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Door games ({}) ", names.len())),
        ),
        area,
    );
}

fn door_fields(f: &mut Frame, area: Rect, editor: &Editor) {
    let label_width = DOOR_FIELDS
        .iter()
        .map(|f| f.label.chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    let lines: Vec<Line> = DOOR_FIELDS
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let (value, set) = editor.door_shown_value(field.key);
            let shown = if !set || value.is_empty() {
                match field.kind {
                    FieldKind::StrList => "(none)".to_string(),
                    FieldKind::Int { .. } => "0".to_string(),
                    _ => "(blank)".to_string(),
                }
            } else {
                value
            };
            let style = if i == editor.door_field_sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::raw(format!(
                    "{} {:<label_width$}",
                    if i == editor.door_field_sel { ">" } else { " " },
                    field.label
                )),
                Span::raw(shown),
            ])
            .style(style)
        })
        .collect();
    let name = editor
        .doc
        .door_names()
        .get(editor.door_sel)
        .cloned()
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Door: {name} ")),
        ),
        area,
    );
}

fn confirm_remove_door(f: &mut Frame, area: Rect, editor: &Editor) {
    let name = editor
        .doc
        .door_names()
        .get(editor.door_sel)
        .cloned()
        .unwrap_or_default();
    let lines = vec![
        Line::from(format!("Remove the door \"{name}\"?")),
        Line::from(""),
        // Worth saying: bbscfg edits the config and nothing else. Deleting a
        // sysop's game directory because they removed a menu entry would be a
        // genuinely bad surprise.
        Line::from("This removes it from the config only — files on disk are untouched."),
        Line::from(""),
        Line::from("y = remove    any other key = keep"),
    ];
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Remove door "),
        ),
        area,
    );
}

fn confirm_quit(f: &mut Frame, area: Rect, _editor: &Editor) {
    let lines = vec![
        Line::from("You have unsaved changes."),
        Line::from(""),
        Line::from("y = discard and quit    s = save first    any other key = keep editing"),
    ];
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Quit ")),
        area,
    );
}

/// The help pane carries the reasoning that lives in the config's comments —
/// which is the main reason to have a config UI at all.
fn help_pane(f: &mut Frame, area: Rect, editor: &Editor) {
    let text = match editor.screen {
        Screen::Sections => editor.section().help.to_string(),
        Screen::Doors | Screen::ArtScreens => editor.section().help.to_string(),
        Screen::SeedBoards => editor.section().help.to_string(),
        Screen::SeedBoardFields => editor
            .seed_board_field()
            .map(|f| f.help.to_string())
            .unwrap_or_default(),
        Screen::DoorFields => editor
            .door_field()
            .map(|f| f.help.to_string())
            .unwrap_or_default(),
        Screen::Fields => editor
            .field()
            .map(|f| f.help.to_string())
            .unwrap_or_default(),
        // The Edit screen is shared between section fields and door fields.
        Screen::Edit => editor
            .edit_field()
            .map(|f| f.help.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };
    f.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title(" About "))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn status_bar(f: &mut Frame, area: Rect, editor: &Editor) {
    let keys = match editor.screen {
        Screen::Sections => " ↑/↓ move · Enter open · s save · q quit ",
        Screen::Fields => " ↑/↓ move · Enter edit/toggle · u default · s save · Esc back ",
        Screen::Edit => " type · Enter apply · Esc cancel ",
        Screen::Save => " y save · n back ",
        Screen::ConfirmQuit => " y quit · s save · any key back ",
        Screen::Doors => {
            " ↑/↓ move · Enter edit · a add · d remove · K/J reorder · s save · Esc back "
        }
        Screen::DoorFields => " ↑/↓ move · Enter edit · s save · Esc back ",
        Screen::ConfirmRemoveDoor => " y remove · any key keep ",
        Screen::ArtScreens => " ↑/↓ move · Enter set · u clear · s save · Esc back ",
        Screen::SeedBoards => {
            " ↑/↓ move · Enter edit · a add board · d remove · s save · Esc back "
        }
        Screen::SeedBoardFields => " ↑/↓ move · Enter edit · s save · Esc back ",
        Screen::ConfirmRemoveSeedBoard => " y remove · any key keep ",
    };
    let line = format!("{keys}  {}", editor.status);
    f.render_widget(
        Paragraph::new(line).style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}
