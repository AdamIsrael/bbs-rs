//! ratatui rendering. One function per screen, plus shared title/status chrome.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, Paragraph, Wrap};

use crate::app::App;
use crate::app::state::{MenuItem, Screen};
use crate::util::fmt_time;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(f.area());

    render_title(f, chunks[0], app);

    let body = chunks[1];
    match app.screen {
        Screen::MainMenu => render_main_menu(f, body, app),
        Screen::Bulletins => render_bulletins(f, body, app),
        Screen::ReadBulletin => render_read_bulletin(f, body, app),
        Screen::Oneliners => render_oneliners(f, body, app),
        Screen::ComposeOneliner => render_form(f, body, " New Oneliner ", app),
        Screen::BoardList => render_boards(f, body, app),
        Screen::MessageList => render_messages(f, body, app),
        Screen::ReadMessage => render_read_message(f, body, app),
        Screen::ComposePost => render_form(f, body, " New Post ", app),
        Screen::Mailbox => render_mailbox(f, body, app),
        Screen::ReadMail => render_read_mail(f, body, app),
        Screen::ComposeMail => render_form(f, body, " Compose Mail ", app),
        Screen::WhoOnline => render_who(f, body, app),
        Screen::FileAreas => render_file_areas(f, body, app),
        Screen::FileList => render_files(f, body, app),
        Screen::FileDetail => render_file_detail(f, body, app),
        Screen::EditFileDesc => render_form(f, body, " Edit Description ", app),
        Screen::ArchiveList => render_archive_list(f, body, app),
        Screen::FileView => render_file_view(f, body, app),
        Screen::Keys => render_keys(f, body, app),
        Screen::AddKey => render_form(f, body, " Add SSH Key ", app),
        Screen::Register => render_form(f, body, " Register ", app),
        Screen::Help => render_help(f, body, app),
        Screen::AdminUsers => render_admin_users(f, body, app),
        Screen::AdminLogins => render_admin_logins(f, body, app),
    }

    render_status(f, chunks[2], app);
}

fn render_title(f: &mut Frame, area: Rect, app: &App) {
    let title = format!(
        " {}  ·  {} ({})  ·  {} ",
        app.config.bbs.name,
        app.user.username,
        app.user.role,
        screen_name(app.screen)
    );
    let bar = Paragraph::new(title).style(
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    );
    f.render_widget(bar, area);
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let (text, style) = if app.status.is_empty() {
        (
            hints(app.screen, app.user.is_admin(), app.can_edit_current_file()),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (
            format!(" {} ", app.status),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        )
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

fn render_selectable(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line>, selected: usize) {
    let items: Vec<ListItem> = lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let item = ListItem::new(line);
            if i == selected {
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    let list = List::new(items).block(Block::bordered().title(title.to_string()));
    f.render_widget(list, area);
}

fn render_main_menu(f: &mut Frame, area: Rect, app: &App) {
    let bbs = &app.config.bbs;

    // Branding / MOTD banner above the menu.
    let mut banner: Vec<Line> = vec![Line::from(Span::styled(
        bbs.name.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    if !bbs.tagline.is_empty() {
        banner.push(Line::from(Span::styled(
            bbs.tagline.clone(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    if !bbs.welcome.is_empty() {
        banner.push(Line::from(""));
        banner.push(Line::from(bbs.welcome.clone()));
    }
    let banner_h = banner.len() as u16 + 2; // + borders
    let rows = Layout::vertical([Constraint::Length(banner_h), Constraint::Min(1)]).split(area);
    let banner_widget = Paragraph::new(Text::from(banner))
        .block(Block::bordered())
        .wrap(Wrap { trim: false });
    f.render_widget(banner_widget, rows[0]);

    let lines: Vec<Line> = app
        .menu
        .iter()
        .map(|m| {
            let mut label = m.label().to_string();
            if *m == MenuItem::Mail && app.user.is_guest() {
                label.push_str("   (register required)");
            }
            Line::from(label)
        })
        .collect();
    render_selectable(f, rows[1], " Main Menu ", lines, app.menu_sel);
}

fn render_bulletins(f: &mut Frame, area: Rect, app: &App) {
    if app.bulletins.is_empty() {
        return placeholder(f, area, " Bulletins ", "No bulletins.");
    }
    let lines: Vec<Line> = app
        .bulletins
        .iter()
        .map(|b| {
            Line::from(format!(
                "{:<12} {}",
                fmt_time(b.created_at),
                truncate(&b.title, 60)
            ))
        })
        .collect();
    render_selectable(f, area, " Bulletins ", lines, app.bulletin_sel);
}

fn render_read_bulletin(f: &mut Frame, area: Rect, app: &App) {
    let Some(b) = &app.current_bulletin else {
        return placeholder(f, area, " Bulletin ", "Nothing to show.");
    };
    let body = format!("Date: {}\n\n{}", fmt_time(b.created_at), b.body);
    let p = Paragraph::new(body)
        .block(Block::bordered().title(format!(" {} ", truncate(&b.title, 60))))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_oneliners(f: &mut Frame, area: Rect, app: &App) {
    if app.oneliners.is_empty() {
        return placeholder(
            f,
            area,
            " Oneliners ",
            "The wall is empty. Press 'n' to add one.",
        );
    }
    let lines: Vec<Line> = app
        .oneliners
        .iter()
        .map(|o| {
            Line::from(vec![
                Span::styled(
                    format!("{:>12}: ", truncate(&o.author_name, 12)),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(truncate(&o.body, 80)),
            ])
        })
        .collect();
    // A read-only wall: reuse the list renderer with no selection highlight.
    render_selectable(f, area, " Oneliners ", lines, usize::MAX);
}

fn render_boards(f: &mut Frame, area: Rect, app: &App) {
    if app.boards.is_empty() {
        return placeholder(f, area, " Boards ", "No boards.");
    }
    let lines: Vec<Line> = app
        .boards
        .iter()
        .map(|b| {
            let mut flags = String::new();
            if b.locked {
                flags.push_str(" [locked]");
            }
            if b.min_write_role != "user" {
                flags.push_str(&format!(" [{}+ to post]", b.min_write_role));
            }
            if b.min_read_role != "guest" {
                flags.push_str(&format!(" [{}+ to read]", b.min_read_role));
            }
            Line::from(vec![
                Span::raw(format!("{:<16} {}", b.name, b.description)),
                Span::styled(flags, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    render_selectable(f, area, " Boards ", lines, app.board_sel);
}

fn render_messages(f: &mut Frame, area: Rect, app: &App) {
    let name = app
        .current_board
        .as_ref()
        .map(|b| b.name.as_str())
        .unwrap_or("");
    let locked = app.current_board.as_ref().is_some_and(|b| b.locked);
    let title = if locked {
        format!(" {name} [locked] ")
    } else {
        format!(" {name} ")
    };
    if app.messages.is_empty() {
        return placeholder(f, area, &title, "No messages yet. Press 'n' to post.");
    }
    let lines: Vec<Line> = app
        .messages
        .iter()
        .map(|m| {
            let pin = if m.pinned { "📌 " } else { "   " };
            Line::from(format!(
                "{}{:<32} {:<12} {}",
                pin,
                truncate(&m.subject, 32),
                truncate(&m.author_name, 12),
                fmt_time(m.created_at)
            ))
        })
        .collect();
    render_selectable(f, area, &title, lines, app.msg_sel);
}

fn render_read_message(f: &mut Frame, area: Rect, app: &App) {
    let Some(m) = &app.current_message else {
        return placeholder(f, area, " Message ", "Nothing to show.");
    };
    let body = format!(
        "From: {}\nDate: {}\n\n{}",
        m.author_name,
        fmt_time(m.created_at),
        m.body
    );
    let p = Paragraph::new(body)
        .block(Block::bordered().title(format!(" {} ", truncate(&m.subject, 60))))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_mailbox(f: &mut Frame, area: Rect, app: &App) {
    if app.mails.is_empty() {
        return placeholder(f, area, " Mailbox ", "Inbox empty. Press 'n' to compose.");
    }
    let lines: Vec<Line> = app
        .mails
        .iter()
        .map(|m| {
            let flag = if m.read_at.is_none() { "*" } else { " " };
            Line::from(format!(
                "{} {:<32} from {:<12} {}",
                flag,
                truncate(&m.subject, 32),
                truncate(&m.from_name, 12),
                fmt_time(m.created_at)
            ))
        })
        .collect();
    render_selectable(f, area, " Mailbox ", lines, app.mail_sel);
}

fn render_read_mail(f: &mut Frame, area: Rect, app: &App) {
    let Some(m) = &app.current_mail else {
        return placeholder(f, area, " Mail ", "Nothing to show.");
    };
    let body = format!(
        "From: {}\nDate: {}\n\n{}",
        m.from_name,
        fmt_time(m.created_at),
        m.body
    );
    let p = Paragraph::new(body)
        .block(Block::bordered().title(format!(" {} ", truncate(&m.subject, 60))))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_who(f: &mut Frame, area: Rect, app: &App) {
    if app.online.is_empty() {
        return placeholder(f, area, " Who's Online ", "Nobody online.");
    }
    let lines: Vec<Line> = app
        .online
        .iter()
        .map(|u| {
            Line::from(format!(
                "{:<20} online since {}",
                u.username,
                fmt_time(u.since)
            ))
        })
        .collect();
    // Not a selection list, but reuse the renderer with an out-of-range index.
    render_selectable(f, area, " Who's Online ", lines, usize::MAX);
}

fn render_file_areas(f: &mut Frame, area: Rect, app: &App) {
    if app.file_areas.is_empty() {
        return placeholder(f, area, " File Areas ", "No file areas.");
    }
    let lines: Vec<Line> = app
        .file_areas
        .iter()
        .map(|a| {
            let mut flags = String::new();
            if a.min_read_role != "guest" {
                flags.push_str(&format!(" [{}+ to view]", a.min_read_role));
            }
            Line::from(vec![
                Span::raw(format!("{:<16} {}", a.name, a.description)),
                Span::styled(flags, Style::default().fg(Color::DarkGray)),
            ])
        })
        .collect();
    render_selectable(f, area, " File Areas ", lines, app.file_area_sel);
}

fn render_files(f: &mut Frame, area: Rect, app: &App) {
    let name = app
        .current_file_area
        .as_ref()
        .map(|a| a.name.as_str())
        .unwrap_or("");
    let title = format!(" {name} ");
    if app.files.is_empty() {
        return placeholder(f, area, &title, "No files in this area yet.");
    }
    let lines: Vec<Line> = app
        .files
        .iter()
        .map(|file| {
            Line::from(format!(
                "{:<28} {:>10} {:<12} {}",
                truncate(&file.filename, 28),
                human_size(file.size),
                truncate(&file.uploader_name, 12),
                truncate(&file.description, 24)
            ))
        })
        .collect();
    render_selectable(f, area, &title, lines, app.file_sel);
}

fn render_file_detail(f: &mut Frame, area: Rect, app: &App) {
    let Some(file) = &app.current_file else {
        return placeholder(f, area, " File ", "Nothing to show.");
    };
    let area_name = app
        .current_file_area
        .as_ref()
        .map(|a| a.name.as_str())
        .unwrap_or("");
    let net = &app.config.network;
    let body = format!(
        "Name:      {}\nSize:      {} ({} bytes)\nUploaded:  {} by {}\nDownloads: {}\n\n{}\n\n\
         Download over SFTP:\n  sftp -P {} {}@{}\n  sftp> get {}/{}",
        file.filename,
        human_size(file.size),
        file.size,
        fmt_time(file.created_at),
        file.uploader_name,
        file.downloads,
        if file.description.is_empty() {
            "(no description — press 'e' to add one, if it's yours)"
        } else {
            &file.description
        },
        net.port,
        app.user.username,
        net.connect_host(),
        area_name,
        file.filename,
    );
    let p = Paragraph::new(body)
        .block(Block::bordered().title(format!(" {} ", truncate(&file.filename, 50))))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_archive_list(f: &mut Frame, area: Rect, app: &App) {
    let title = app
        .current_file
        .as_ref()
        .map(|file| format!(" {} ", truncate(&file.filename, 40)))
        .unwrap_or_else(|| " Archive ".to_string());
    if app.archive_entries.is_empty() {
        return placeholder(f, area, &title, "Empty archive.");
    }
    let lines: Vec<Line> = app
        .archive_entries
        .iter()
        .map(|e| {
            let size = if e.is_dir {
                "<dir>".to_string()
            } else {
                human_size(e.size as i64)
            };
            Line::from(format!("{:>10}  {}", size, truncate(&e.name, 60)))
        })
        .collect();
    render_selectable(f, area, &title, lines, app.archive_sel);
}

fn render_file_view(f: &mut Frame, area: Rect, app: &App) {
    let mut title = format!(" {} ", truncate(&app.file_view_title, 50));
    if app.file_view_truncated {
        title = format!(" {} [truncated] ", truncate(&app.file_view_title, 40));
    }
    let p = Paragraph::new(app.file_view_body.as_str())
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false })
        .scroll((app.file_view_scroll, 0));
    f.render_widget(p, area);
}

fn render_keys(f: &mut Frame, area: Rect, app: &App) {
    if app.user_keys.is_empty() {
        return placeholder(
            f,
            area,
            " SSH Keys ",
            "No keys registered. Press 'n' to add one (paste your .pub line).",
        );
    }
    let lines: Vec<Line> = app
        .user_keys
        .iter()
        .map(|k| {
            let label = if k.label.is_empty() { "-" } else { &k.label };
            Line::from(format!(
                "{:<12} {:<20} {}",
                truncate(&k.algorithm, 12),
                truncate(label, 20),
                truncate(&k.fingerprint, 50)
            ))
        })
        .collect();
    render_selectable(f, area, " SSH Keys ", lines, app.key_sel);
}

fn render_form(f: &mut Frame, area: Rect, title: &str, app: &App) {
    let lines: Vec<Line> = app
        .form
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let shown = if field.secret {
                "*".repeat(field.value.chars().count())
            } else {
                field.value.clone()
            };
            let marker = if i == app.form.focus { "> " } else { "  " };
            let line = Line::from(format!("{}{}: {}", marker, field.label, shown));
            if i == app.form.focus {
                line.style(Style::default().add_modifier(Modifier::BOLD))
            } else {
                line
            }
        })
        .collect();
    // Wrap so long field input (subject/body/username) stays visible instead of
    // running off the right edge while typing.
    let p = Paragraph::new(Text::from(lines))
        .block(Block::bordered().title(title.to_string()))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_admin_users(f: &mut Frame, area: Rect, app: &App) {
    if app.admin_users.is_empty() {
        return placeholder(f, area, " Admin · Users ", "No users.");
    }
    let lines: Vec<Line> = app
        .admin_users
        .iter()
        .map(|u| {
            let status = if u.is_banned() { "BANNED" } else { "" };
            Line::from(format!(
                "{:<20} {:<7} {:<7} {}",
                truncate(&u.username, 20),
                u.role,
                status,
                fmt_time(u.created_at)
            ))
        })
        .collect();
    render_selectable(f, area, " Admin · Users ", lines, app.admin_user_sel);
}

fn render_admin_logins(f: &mut Frame, area: Rect, app: &App) {
    if app.admin_logins.is_empty() {
        return placeholder(f, area, " Admin · Logins ", "No login attempts recorded.");
    }
    let lines: Vec<Line> = app
        .admin_logins
        .iter()
        .map(|l| {
            let result = if l.success { "ok" } else { "reject" };
            Line::from(format!(
                "{:<17} {:<7} {:<20} {}",
                fmt_time(l.created_at),
                result,
                truncate(&l.username, 20),
                l.ip.as_deref().unwrap_or("-")
            ))
        })
        .collect();
    render_selectable(f, area, " Admin · Logins ", lines, usize::MAX);
}

fn render_help(f: &mut Frame, area: Rect, app: &App) {
    let bbs = &app.config.bbs;
    let mut text = format!("{} — {}\n\n", bbs.name, bbs.tagline);
    text.push_str(
        "\
  • Message Boards : browse boards, read and (registered users) post messages
                     (admins: l lock a board, p pin, d delete a post)
  • Oneliners      : a shared graffiti wall of short public one-liners (press n to add)
  • Private Mail   : send and receive messages with other registered users
  • Who's Online   : see who is currently connected
  • File Areas     : browse files, read text + peek inside archives; transfer over SFTP
  • SSH Keys       : register public keys to log in without a password
  • Register       : create an account, then reconnect over SSH with it

Navigation
  ↑/↓ move    Enter select/open    Esc or ← go back    q quit
  In forms: Tab/↑/↓ switch fields, Enter submits on the last field.

The guest account is read-only: it can browse boards and see who's online,
but cannot post or use mail. Register an account for the full experience.",
    );
    if !bbs.sysop.is_empty() {
        text.push_str(&format!("\n\nSysop: {}", bbs.sysop));
    }
    let p = Paragraph::new(text)
        .block(Block::bordered().title(" Help "))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn placeholder(f: &mut Frame, area: Rect, title: &str, msg: &str) {
    let p = Paragraph::new(msg)
        .alignment(Alignment::Center)
        .block(Block::bordered().title(title.to_string()));
    f.render_widget(p, area);
}

fn screen_name(screen: Screen) -> &'static str {
    match screen {
        Screen::MainMenu => "Main Menu",
        Screen::Bulletins => "Bulletins",
        Screen::ReadBulletin => "Bulletin",
        Screen::Oneliners => "Oneliners",
        Screen::ComposeOneliner => "New Oneliner",
        Screen::BoardList => "Boards",
        Screen::MessageList => "Messages",
        Screen::ReadMessage => "Reading",
        Screen::ComposePost => "New Post",
        Screen::Mailbox => "Mailbox",
        Screen::ReadMail => "Reading Mail",
        Screen::ComposeMail => "Compose Mail",
        Screen::WhoOnline => "Who's Online",
        Screen::FileAreas => "File Areas",
        Screen::FileList => "Files",
        Screen::FileDetail => "File",
        Screen::EditFileDesc => "Edit Description",
        Screen::ArchiveList => "Archive",
        Screen::FileView => "Viewing",
        Screen::Keys => "SSH Keys",
        Screen::AddKey => "Add SSH Key",
        Screen::Register => "Register",
        Screen::Help => "Help",
        Screen::AdminUsers => "Admin · Users",
        Screen::AdminLogins => "Admin · Logins",
    }
}

fn hints(screen: Screen, is_admin: bool, can_edit_file: bool) -> String {
    let base = match screen {
        Screen::MainMenu => " ↑/↓ move · Enter select · q quit ",
        Screen::Bulletins => " ↑/↓ move · Enter read · Esc to menu ",
        Screen::Oneliners => " n new · Esc back ",
        Screen::ComposeOneliner => " type your oneliner · Enter post · Esc cancel ",
        Screen::BoardList => {
            if is_admin {
                " ↑/↓ move · Enter open · l lock/unlock · Esc back "
            } else {
                " ↑/↓ move · Enter open · Esc back "
            }
        }
        Screen::MessageList => {
            if is_admin {
                " ↑/↓ move · Enter read · n post · p pin · d delete · Esc back "
            } else {
                " ↑/↓ move · Enter read · n new post · Esc back "
            }
        }
        Screen::ReadMessage | Screen::ReadMail | Screen::ReadBulletin | Screen::Help => {
            " Esc back "
        }
        Screen::ComposePost | Screen::ComposeMail | Screen::Register => {
            " type to edit · Tab/↑/↓ fields · Enter next/submit · Esc cancel "
        }
        Screen::Mailbox => " ↑/↓ move · Enter read · n compose · Esc back ",
        Screen::WhoOnline => " r refresh · Esc back ",
        Screen::FileAreas => " ↑/↓ move · Enter open · Esc back ",
        Screen::FileList => " ↑/↓ move · Enter details · Esc back ",
        Screen::FileDetail => {
            if can_edit_file {
                " Enter view · e edit description · Esc back "
            } else {
                " Enter view · Esc back "
            }
        }
        Screen::EditFileDesc => " type · Enter save · Esc cancel ",
        Screen::ArchiveList => " ↑/↓ move · Enter open entry · Esc back ",
        Screen::FileView => " ↑/↓ scroll · PgUp/PgDn · Home top · Esc back ",
        Screen::Keys => " ↑/↓ move · n add · d delete · Esc back ",
        Screen::AddKey => " paste your public key · Enter add · Esc cancel ",
        Screen::AdminUsers => " ↑/↓ move · b ban · u unban · l logins · Esc back ",
        Screen::AdminLogins => " Esc back ",
    };
    base.to_string()
}

/// Human-readable byte size, e.g. `1.5 MiB`.
fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes.max(0) as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes.max(0), UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
