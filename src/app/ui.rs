//! ratatui rendering. One function per screen, plus shared title/status chrome.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
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
        Screen::BoardList => render_boards(f, body, app),
        Screen::MessageList => render_messages(f, body, app),
        Screen::ReadMessage => render_read_message(f, body, app),
        Screen::ComposePost => render_form(f, body, " New Post ", app),
        Screen::Mailbox => render_mailbox(f, body, app),
        Screen::ReadMail => render_read_mail(f, body, app),
        Screen::ComposeMail => render_form(f, body, " Compose Mail ", app),
        Screen::WhoOnline => render_who(f, body, app),
        Screen::Register => render_form(f, body, " Register ", app),
        Screen::Help => render_help(f, body),
        Screen::AdminUsers => render_admin_users(f, body, app),
        Screen::AdminLogins => render_admin_logins(f, body, app),
    }

    render_status(f, chunks[2], app);
}

fn render_title(f: &mut Frame, area: Rect, app: &App) {
    let title = format!(
        " sshtui BBS  ·  {} ({})  ·  {} ",
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
            hints(app.screen).to_string(),
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
    render_selectable(f, area, " Main Menu ", lines, app.menu_sel);
}

fn render_boards(f: &mut Frame, area: Rect, app: &App) {
    if app.boards.is_empty() {
        return placeholder(f, area, " Boards ", "No boards.");
    }
    let lines: Vec<Line> = app
        .boards
        .iter()
        .map(|b| Line::from(format!("{:<16} {}", b.name, b.description)))
        .collect();
    render_selectable(f, area, " Boards ", lines, app.board_sel);
}

fn render_messages(f: &mut Frame, area: Rect, app: &App) {
    let title = format!(" {} ", app.current_board_name);
    if app.messages.is_empty() {
        return placeholder(f, area, &title, "No messages yet. Press 'n' to post.");
    }
    let lines: Vec<Line> = app
        .messages
        .iter()
        .map(|m| {
            Line::from(format!(
                "{:<32} {:<12} {}",
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
    let p = Paragraph::new(Text::from(lines)).block(Block::bordered().title(title.to_string()));
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

fn render_help(f: &mut Frame, area: Rect) {
    let text = "\
sshtui — a tiny bulletin board over SSH

  • Message Boards : browse boards, read and (registered users) post messages
  • Private Mail   : send and receive messages with other registered users
  • Who's Online   : see who is currently connected
  • Register       : create an account, then reconnect over SSH with it

Navigation
  ↑/↓ move    Enter select/open    Esc or ← go back    q quit
  In forms: Tab/↑/↓ switch fields, Enter submits on the last field.

The guest account is read-only: it can browse boards and see who's online,
but cannot post or use mail. Register an account for the full experience.";
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
        Screen::BoardList => "Boards",
        Screen::MessageList => "Messages",
        Screen::ReadMessage => "Reading",
        Screen::ComposePost => "New Post",
        Screen::Mailbox => "Mailbox",
        Screen::ReadMail => "Reading Mail",
        Screen::ComposeMail => "Compose Mail",
        Screen::WhoOnline => "Who's Online",
        Screen::Register => "Register",
        Screen::Help => "Help",
        Screen::AdminUsers => "Admin · Users",
        Screen::AdminLogins => "Admin · Logins",
    }
}

fn hints(screen: Screen) -> &'static str {
    match screen {
        Screen::MainMenu => " ↑/↓ move · Enter select · q quit ",
        Screen::BoardList => " ↑/↓ move · Enter open · Esc back ",
        Screen::MessageList => " ↑/↓ move · Enter read · n new post · Esc back ",
        Screen::ReadMessage | Screen::ReadMail | Screen::Help => " Esc back ",
        Screen::ComposePost | Screen::ComposeMail | Screen::Register => {
            " type to edit · Tab/↑/↓ fields · Enter next/submit · Esc cancel "
        }
        Screen::Mailbox => " ↑/↓ move · Enter read · n compose · Esc back ",
        Screen::WhoOnline => " r refresh · Esc back ",
        Screen::AdminUsers => " ↑/↓ move · b ban · u unban · l logins · Esc back ",
        Screen::AdminLogins => " Esc back ",
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
