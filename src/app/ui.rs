//! ratatui rendering. One function per screen, plus shared title/status chrome.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};

use crate::app::App;
use crate::app::state::{MenuAction, MenuItem, Screen};
use crate::transport::Transport;
use crate::util::fmt_time;

/// The "other way in" for this session: browser users get the SSH command, SSH
/// users get the web URL. `None` when the operator opted out, or when the other
/// transport isn't available (the web frontend is off).
///
/// Addresses come from the configured public hostnames — with a stock config
/// (blank `hostname`, wildcard bind) these resolve to `localhost`, which is
/// only useful locally.
fn other_transport_hint(app: &App) -> Option<String> {
    if !app.config.features.advertise_transports {
        return None;
    }
    match app.transport {
        // Mirrors the SFTP hint's shape: omit -p on the default SSH port.
        Transport::Web => {
            let net = &app.config.network;
            let host = net.connect_host();
            let user = &app.user.username;
            Some(if net.port == 22 {
                format!("ssh {user}@{host}")
            } else {
                format!("ssh -p {} {user}@{host}", net.port)
            })
        }
        Transport::Ssh => app.config.web.enabled.then(|| app.config.web.connect_url()),
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(f.area());

    render_title(f, chunks[0], app);

    // The main menu with a full ANSI backdrop (#85) owns the whole area and
    // draws its own art; every other screen gets the art (if any) as a capped
    // header band above its body.
    let body = if app.screen == Screen::MainMenu && menu_canvas_active(app) {
        chunks[1]
    } else {
        render_art_header(f, chunks[1], app)
    };
    match app.screen {
        Screen::MainMenu => render_main_menu(f, body, app),
        Screen::Bulletins => render_bulletins(f, body, app),
        Screen::ReadBulletin => render_read_bulletin(f, body, app),
        Screen::Oneliners => render_oneliners(f, body, app),
        Screen::Timeline => render_timeline(f, body, app),
        Screen::FollowRemote => render_form(f, body, " Follow Remote Account ", app),
        Screen::RemoteBoards => render_remote_boards(f, body, app),
        Screen::RemoteBoardPosts => render_remote_board_posts(f, body, app),
        Screen::ComposeRemotePost => render_form(f, body, " Post to Remote Board ", app),
        Screen::ComposeOneliner => render_form(f, body, " New Oneliner ", app),
        Screen::BoardList => render_boards(f, body, app),
        Screen::MessageList => render_messages(f, body, app),
        Screen::ReadMessage => render_read_message(f, body, app),
        Screen::ComposePost => {
            let title = if app.is_editing_post() {
                " Edit Post "
            } else {
                " New Post "
            };
            render_compose(f, body, title, app)
        }
        Screen::Mailbox => render_mailbox(f, body, app),
        Screen::MailSearchInput => render_form(f, body, " Search Mail ", app),
        Screen::MailSearchResults => render_mail_search_results(f, body, app),
        Screen::ReadMail => render_read_mail(f, body, app),
        Screen::ConfirmDeleteMail => render_confirm_delete_mail(f, body, app),
        Screen::ComposeMail => render_compose(f, body, " Compose Mail ", app),
        Screen::WhoOnline => render_who(f, body, app),
        Screen::ComposePage => {
            let title = format!(" Page {} ", app.page_target().unwrap_or("user"));
            render_form(f, body, &title, app)
        }
        Screen::Profile => render_profile(f, body, app),
        Screen::IgnoreList => render_ignore_list(f, body, app),
        Screen::EditProfile => render_form(f, body, " Edit Profile ", app),
        Screen::Stats => render_stats(f, body, app),
        Screen::SearchInput => render_form(f, body, " Search Messages ", app),
        Screen::SearchResults => render_search_results(f, body, app),
        Screen::Doors => render_doors(f, body, app),
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
        Screen::ComposeBroadcast => render_form(f, body, " Broadcast to all sessions ", app),
        Screen::AdminLogins => render_admin_logins(f, body, app),
        Screen::AdminAudit => render_admin_audit(f, body, app),
        Screen::AdminFederation => render_admin_federation(f, body, app),
        Screen::ComposeFederation => render_form(f, body, " Add federation domain ", app),
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
            .fg(app.theme.title_fg)
            .bg(app.theme.title_bg)
            .add_modifier(Modifier::BOLD),
    );
    f.render_widget(bar, area);
}

/// Draw the configured art for `app.screen` as a header band at the top of
/// `area`, returning the remaining area for the screen's own content. Returns
/// `area` unchanged when there's no art (or no room for it).
fn render_art_header(f: &mut Frame, area: Rect, app: &App) -> Rect {
    /// Never let art take more than this many rows (leave room for content).
    const MAX_ART_ROWS: u16 = 16;
    let Some(art) = app.art.get(&app.screen) else {
        return area;
    };
    // Leave at least one row for the screen body.
    let want = art.lines.len() as u16;
    let art_h = want.min(MAX_ART_ROWS).min(area.height.saturating_sub(1));
    if art_h == 0 {
        return area;
    }
    let rows = Layout::vertical([Constraint::Length(art_h), Constraint::Min(0)]).split(area);
    f.render_widget(Paragraph::new(art.clone()), rows[0]);
    rows[1]
}

fn render_status(f: &mut Frame, area: Rect, app: &App) {
    let (text, style) = if app.status.is_empty() {
        (
            hints(
                app.screen,
                app.user.is_admin(),
                app.can_edit_current_file(),
                app.can_edit_current_profile(),
                app.can_block_current_profile(),
            ),
            Style::default().fg(app.theme.dim),
        )
    } else {
        (
            format!(" {} ", app.status),
            Style::default()
                .fg(app.theme.warning_fg)
                .bg(app.theme.warning_bg),
        )
    };
    f.render_widget(Paragraph::new(text).style(style), area);
}

/// Render a bordered list. When `selected` is a valid index the row is
/// highlighted and a stateful `List` auto-scrolls to keep it visible; a
/// sentinel `usize::MAX` means "no cursor" (read-only walls), leaving the list
/// top-anchored.
fn render_selectable(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line>, selected: usize) {
    let items: Vec<ListItem> = lines.into_iter().map(ListItem::new).collect();
    let len = items.len();
    let list = List::new(items)
        .block(Block::bordered().title(title.to_string()))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    if selected < len {
        let mut state = ListState::default();
        state.select(Some(selected));
        f.render_stateful_widget(list, area, &mut state);
    } else {
        f.render_widget(list, area);
    }
}

/// The menu label for `entry`, including the Mail badge — shared by the list and
/// canvas renderers so they never drift.
fn menu_label(app: &App, entry: &crate::app::state::MenuEntry) -> String {
    let mut label = entry.label.clone();
    if matches!(entry.action, MenuAction::Builtin(MenuItem::Mail)) {
        if app.user.is_guest() {
            label.push_str("   (register required)");
        } else if app.mail_unread > 0 {
            label.push_str(&format!("   ({} new)", app.mail_unread));
        }
    }
    label
}

/// Whether the main menu should render as an ANSI canvas (#85): a main-menu
/// backdrop is configured *and* every shown entry has a placement. All-or-none,
/// so a partial layout can never hide an item.
fn menu_canvas_active(app: &App) -> bool {
    // Only the top-level menu uses the backdrop; a submenu (#86) always renders
    // as the bordered list, so its title breadcrumb is visible.
    app.menu_stack.is_empty()
        && app.art.contains_key(&Screen::MainMenu)
        && !app.menu.is_empty()
        && app.menu.iter().all(|e| e.row.is_some() && e.col.is_some())
}

/// Whether the placed items fit within `area` (else we fall back to the list so
/// nothing is clipped off the bottom or right).
fn menu_canvas_fits(app: &App, area: Rect) -> bool {
    app.menu.iter().all(|e| match (e.row, e.col) {
        (Some(row), Some(col)) => {
            let w = menu_label(app, e).chars().count() as u16;
            row < area.height && col.saturating_add(w) <= area.width
        }
        _ => false,
    })
}

/// Draw the menu as labels placed over the ANSI backdrop at operator-chosen
/// coordinates (#85). The selected entry is highlighted; arrow keys still move
/// the selection in menu order.
fn render_menu_canvas(f: &mut Frame, area: Rect, app: &App) {
    if let Some(art) = app.art.get(&Screen::MainMenu) {
        f.render_widget(Paragraph::new(art.clone()), area);
    }
    for (i, entry) in app.menu.iter().enumerate() {
        let (Some(row), Some(col)) = (entry.row, entry.col) else {
            continue;
        };
        let label = menu_label(app, entry);
        let x = area.x + col;
        let y = area.y + row;
        if y >= area.bottom() || x >= area.right() {
            continue;
        }
        let w = (label.chars().count() as u16).min(area.right() - x);
        let cell = Rect::new(x, y, w, 1);
        // Selected item pops via reverse-video; the rest use the terminal
        // default so they sit naturally on the operator's art.
        let style = if i == app.menu_sel {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        f.render_widget(Paragraph::new(Span::styled(label, style)), cell);
    }
}

fn render_main_menu(f: &mut Frame, area: Rect, app: &App) {
    // ANSI-canvas layout (#85) when the operator placed every item and it fits;
    // otherwise the classic bordered list below.
    if menu_canvas_active(app) && menu_canvas_fits(app, area) {
        render_menu_canvas(f, area, app);
        return;
    }

    let bbs = &app.config.bbs;

    // When welcome art is configured it heads the screen (drawn as the art
    // header), so skip the redundant text branding banner here.
    let mut area = area;
    if !app.art.contains_key(&Screen::MainMenu) {
        // Branding / MOTD banner above the menu.
        let mut banner: Vec<Line> = vec![Line::from(Span::styled(
            bbs.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        if !bbs.tagline.is_empty() {
            banner.push(Line::from(Span::styled(
                bbs.tagline.clone(),
                Style::default().fg(app.theme.dim),
            )));
        }
        if !bbs.welcome.is_empty() {
            banner.push(Line::from(""));
            banner.push(Line::from(bbs.welcome.clone()));
        }
        // Point users at the other way in (browser ↔ SSH).
        if let Some(hint) = other_transport_hint(app) {
            let label = match app.transport {
                Transport::Web => "Also on SSH:",
                Transport::Ssh => "Also in a browser:",
            };
            banner.push(Line::from(Span::styled(
                format!("{label} {hint}"),
                Style::default().fg(app.theme.dim),
            )));
        }
        let banner_h = banner.len() as u16 + 2; // + borders
        let rows = Layout::vertical([Constraint::Length(banner_h), Constraint::Min(1)]).split(area);
        let banner_widget = Paragraph::new(Text::from(banner))
            .block(Block::bordered())
            .wrap(Wrap { trim: false });
        f.render_widget(banner_widget, rows[0]);
        area = rows[1];
    }

    let lines: Vec<Line> = app
        .menu
        .iter()
        .map(|m| {
            // A leading "[k] " hotkey hint (classic command menu, #84), then the
            // operator's label (+ any Mail badge).
            let key = m
                .key
                .map(|c| format!("[{c}] "))
                .unwrap_or_else(|| "    ".to_string());
            Line::from(format!("{key}{}", menu_label(app, m)))
        })
        .collect();
    // A submenu (#86) shows its breadcrumb title; the top level is "Main Menu".
    let title = match &app.menu_title {
        Some(t) => format!(" {t} "),
        None => " Main Menu ".to_string(),
    };
    render_selectable(f, area, &title, lines, app.menu_sel);
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
    // `author:` prefix is a fixed 14-column gutter (`{:>12}: `); wrap the body
    // to the remaining width and hang-indent continuation lines under it so a
    // long oneliner flows onto the next row instead of running off screen.
    const GUTTER: usize = 14;
    let inner = area.width.saturating_sub(2) as usize; // minus borders
    let body_width = inner.saturating_sub(GUTTER).max(1);
    let mut lines: Vec<Line> = Vec::new();
    for o in &app.oneliners {
        let wrapped = wrap_text(&o.body, body_width);
        let prefix = Span::styled(
            format!("{:>12}: ", truncate(&o.author_name, 12)),
            Style::default().fg(app.theme.accent),
        );
        lines.push(Line::from(vec![
            prefix,
            Span::raw(wrapped.first().cloned().unwrap_or_default()),
        ]));
        for cont in wrapped.into_iter().skip(1) {
            lines.push(Line::from(format!("{:GUTTER$}{cont}", "")));
        }
    }
    // A read-only wall: reuse the list renderer with no selection highlight.
    render_selectable(f, area, " Oneliners ", lines, usize::MAX);
}

fn render_timeline(f: &mut Frame, area: Rect, app: &App) {
    if app.timeline.is_empty() {
        return placeholder(
            f,
            area,
            " Timeline ",
            "No statuses yet. Press 'f' to follow an account (e.g. alice@mastodon.social).",
        );
    }
    // Each status is a header line (handle · time) followed by its wrapped,
    // already-degraded text, then a blank spacer. A leading marker per status
    // lets the selection highlight land on the header row.
    let inner = area.width.saturating_sub(2) as usize;
    let width = inner.max(1);
    let mut lines: Vec<Line> = Vec::new();
    let mut header_rows: Vec<usize> = Vec::new();
    for e in &app.timeline {
        header_rows.push(lines.len());
        lines.push(Line::from(vec![
            Span::styled(
                format!("@{}", e.author_handle),
                Style::default().fg(app.theme.accent),
            ),
            Span::styled(
                format!("  · {}", fmt_time(e.published)),
                Style::default().fg(app.theme.dim),
            ),
        ]));
        for para in e.content.lines() {
            if para.is_empty() {
                lines.push(Line::from(""));
            } else {
                for row in wrap_text(para, width) {
                    lines.push(Line::from(row));
                }
            }
        }
        lines.push(Line::from(""));
    }
    // Map the selected status to its header row so ↑/↓ steps status-by-status.
    let selected = header_rows
        .get(app.timeline_sel)
        .copied()
        .unwrap_or(usize::MAX);
    render_selectable(f, area, " Timeline ", lines, selected);
}

/// Remote boards we subscribe to. These are *someone else's* boards, cached —
/// the title and the per-row marker say so, because nothing else on this screen
/// distinguishes them from local boards at a glance.
fn render_remote_boards(f: &mut Frame, area: Rect, app: &App) {
    if app.remote_boards.is_empty() {
        return placeholder(
            f,
            area,
            " Remote Boards ",
            "Not subscribed to any remote boards. An operator can subscribe with \
             `bbsctl ap-follow <board@host>`.",
        );
    }
    let lines: Vec<Line> = app
        .remote_boards
        .iter()
        .map(|b| {
            let mut spans = vec![Span::raw(format!("{:<32}", truncate(&b.handle, 32)))];
            // A board followed but not yet accepted is legitimately empty.
            // Saying so keeps that from reading as a bug.
            if b.state != "accepted" {
                spans.push(Span::styled(
                    format!("  [{} — no posts until accepted]", b.state),
                    Style::default().fg(app.theme.warning_fg),
                ));
            } else if b.posts == 0 {
                spans.push(Span::styled(
                    "  (nothing mirrored yet)",
                    Style::default().fg(app.theme.dim),
                ));
            } else {
                spans.push(Span::styled(
                    format!(
                        "  {} post{}  · latest {}",
                        b.posts,
                        if b.posts == 1 { "" } else { "s" },
                        b.latest.map(fmt_time).unwrap_or_default()
                    ),
                    Style::default().fg(app.theme.dim),
                ));
            }
            Line::from(spans)
        })
        .collect();
    render_selectable(
        f,
        area,
        " Remote Boards (mirrored) ",
        lines,
        app.remote_board_sel,
    );
}

/// Mirrored posts of one remote board. Same header-plus-body shape as the
/// timeline, since both render already-degraded remote text.
fn render_remote_board_posts(f: &mut Frame, area: Rect, app: &App) {
    let title = match app.current_remote_board.as_ref() {
        Some(b) => format!(" {} · mirrored copy ", truncate(&b.handle, 40)),
        None => " Remote Board ".to_string(),
    };
    if app.mirror_rows.is_empty() {
        let unaccepted = app
            .current_remote_board
            .as_ref()
            .is_some_and(|b| b.state != "accepted");
        return placeholder(
            f,
            area,
            &title,
            if unaccepted {
                "This subscription hasn't been accepted by the remote server yet. \
                 Posts appear once it is."
            } else {
                "Nothing mirrored from this board yet. Posts arrive as the board announces them."
            },
        );
    }
    let inner = area.width.saturating_sub(2) as usize;
    let width = inner.max(1);
    let mut lines: Vec<Line> = Vec::new();
    let mut header_rows: Vec<usize> = Vec::new();
    for row in &app.mirror_rows {
        header_rows.push(lines.len());
        // Indent replies like a local thread, so a mirrored board reads the same
        // as one of ours (#139). Body lines carry the same indent as their
        // header — otherwise a nested reply's text runs back to the margin and
        // the nesting stops being legible past the first line.
        let indent = "  ".repeat(row.depth as usize);
        let lead = if row.depth > 0 { "↳ " } else { "" };
        let mut spans = vec![
            Span::raw(format!("{indent}{lead}")),
            Span::styled(row.subject.clone(), Style::default().fg(app.theme.accent)),
            Span::styled(
                format!("  · @{} · {}", row.author_handle, fmt_time(row.published)),
                Style::default().fg(app.theme.dim),
            ),
        ];
        // Our own submission, not yet published by the board — we don't get to
        // call it published, so it says so.
        if row.pending {
            spans.push(Span::styled(
                "  [sent — awaiting the board]",
                Style::default().fg(app.theme.warning_fg),
            ));
        }
        lines.push(Line::from(spans));

        let body_width = width.saturating_sub(indent.len()).max(1);
        for para in row.body.lines() {
            if para.is_empty() {
                lines.push(Line::from(""));
            } else {
                for wrapped in wrap_text(para, body_width) {
                    lines.push(Line::from(format!("{indent}{wrapped}")));
                }
            }
        }
        lines.push(Line::from(""));
    }
    let selected = header_rows
        .get(app.mirror_sel)
        .copied()
        .unwrap_or(usize::MAX);
    render_selectable(f, area, &title, lines, selected);
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
            let mut spans = vec![
                Span::raw(format!("{:<16} {}", b.name, b.description)),
                Span::styled(flags, Style::default().fg(app.theme.dim)),
            ];
            if let Some(&n) = app.board_unread.get(&b.id).filter(|&&n| n > 0) {
                spans.push(Span::styled(
                    format!("  ({n} new)"),
                    Style::default()
                        .fg(app.theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Line::from(spans)
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
        .map(|item| {
            let m = &item.message;
            // Indent replies; a reply gets a "↳" marker in place of the pin.
            let indent = "  ".repeat(item.depth as usize);
            let lead = if item.depth > 0 {
                "↳ "
            } else if m.pinned {
                "📌 "
            } else {
                "  "
            };
            let subj_width = 34usize.saturating_sub(indent.len() + lead.chars().count());
            // "New since last call": posted after the watermark and not by the
            // viewer. Flagged with a leading dot and a green subject.
            let is_new = m.created_at > app.msg_seen_threshold && m.author_id != app.user.id;
            let marker = if is_new { "•" } else { " " };
            let row = format!(
                "{}{}{}{:<width$} {:<12} {}",
                marker,
                indent,
                lead,
                truncate(&m.subject, subj_width.max(8)),
                truncate(&m.author_name, 12),
                fmt_time(m.created_at),
                width = subj_width.max(8),
            );
            if is_new {
                Line::from(Span::styled(
                    row,
                    Style::default()
                        .fg(app.theme.highlight)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(row)
            }
        })
        .collect();
    render_selectable(f, area, &title, lines, app.msg_sel);
}

fn render_read_message(f: &mut Frame, area: Rect, app: &App) {
    let Some(m) = &app.current_message else {
        return placeholder(f, area, " Message ", "Nothing to show.");
    };
    let edited = m
        .edited_at
        .map(|t| format!("  (edited {})", fmt_time(t)))
        .unwrap_or_default();
    let mut body = format!(
        "From: {}\nDate: {}{}\n\n{}",
        m.author_name,
        fmt_time(m.created_at),
        edited,
        m.body
    );
    // Append the author's signature (usenet-style), if they have one.
    if !app.current_msg_signature.is_empty() {
        body.push_str(&format!("\n\n-- \n{}", app.current_msg_signature));
    }
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
            let mut spans = vec![Span::raw(format!(
                "{} {:<32} from {:<20} {}",
                flag,
                truncate(&m.subject, 32),
                // Remote senders show as `user@host`, so allow the wider field.
                truncate(&m.from_name, 20),
                fmt_time(m.created_at)
            ))];
            // A `@` in the sender means a remote fediverse DM — not private.
            if m.from_name.contains('@') {
                spans.push(Span::styled(
                    "  [fedi · not private]",
                    Style::default().fg(app.theme.warning_fg),
                ));
            }
            Line::from(spans)
        })
        .collect();
    render_selectable(f, area, " Mailbox ", lines, app.mail_sel);
}

fn render_mail_search_results(f: &mut Frame, area: Rect, app: &App) {
    let title = format!(
        " Mail matching \"{}\" ",
        truncate(&app.mail_search_query, 30)
    );
    if app.mail_search.is_empty() {
        return placeholder(
            f,
            area,
            &title,
            "No matching mail. Press / to search again.",
        );
    }
    let lines: Vec<Line> = app
        .mail_search
        .iter()
        .map(|m| {
            let flag = if m.read_at.is_none() { "*" } else { " " };
            Line::from(format!(
                "{} {:<32} from {:<20} {}",
                flag,
                truncate(&m.subject, 32),
                truncate(&m.from_name, 20),
                fmt_time(m.created_at)
            ))
        })
        .collect();
    render_selectable(f, area, &title, lines, app.mail_search_sel);
}

fn render_read_mail(f: &mut Frame, area: Rect, app: &App) {
    let Some(m) = &app.current_mail else {
        return placeholder(f, area, " Mail ", "Nothing to show.");
    };
    let mut lines: Vec<Line> = Vec::new();
    // A remote (fediverse) DM is not private — say so, up top, before the body.
    if m.from_name.contains('@') {
        lines.push(Line::from(Span::styled(
            "⚠  A fediverse message — it passed through remote servers and is NOT private.",
            Style::default()
                .fg(app.theme.warning_fg)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
    }
    lines.push(Line::from(format!("From: {}", m.from_name)));
    lines.push(Line::from(format!("Date: {}", fmt_time(m.created_at))));
    lines.push(Line::from(""));
    for line in m.body.lines() {
        lines.push(Line::from(line.to_string()));
    }
    let p = Paragraph::new(Text::from(lines))
        .block(Block::bordered().title(format!(" {} ", truncate(&m.subject, 60))))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn render_confirm_delete_mail(f: &mut Frame, area: Rect, app: &App) {
    let subject = app
        .current_mail
        .as_ref()
        .map(|m| truncate(&m.subject, 50))
        .unwrap_or_default();
    let lines = vec![
        Line::from(format!("Delete \"{subject}\"?")),
        Line::from(""),
        // Mail is one row, one recipient — deleting is the only copy gone.
        Line::from("This permanently removes it from your mailbox."),
        Line::from(""),
        Line::from("y = delete    any other key = keep"),
    ];
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(Block::bordered().title(" Delete mail ")),
        area,
    );
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
    // Selectable so Enter can open the highlighted user's profile.
    render_selectable(f, area, " Who's Online ", lines, app.who_sel);
}

fn render_profile(f: &mut Frame, area: Rect, app: &App) {
    let Some(p) = &app.current_profile else {
        return placeholder(f, area, " Profile ", "Nothing to show.");
    };
    let dash = |s: &str| {
        if s.is_empty() {
            "—".to_string()
        } else {
            s.to_string()
        }
    };
    let last_on = p
        .last_login
        .map(fmt_time)
        .unwrap_or_else(|| "—".to_string());
    let dim = app.theme.dim;
    let mut lines = vec![
        field_line("User", &format!("{} ({})", p.username, p.role), dim),
        field_line("Real name", &dash(&p.real_name), dim),
        field_line("Location", &dash(&p.location), dim),
        field_line("Tagline", &dash(&p.tagline), dim),
        Line::from(""),
        field_line("Member since", &fmt_time(p.created_at), dim),
        field_line("Last on", &last_on, dim),
        field_line("Posts", &p.post_count.to_string(), dim),
    ];
    if !p.signature.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Signature",
            Style::default().fg(dim),
        )));
        lines.push(Line::from(format!("-- {}", p.signature)));
    }
    // On your own profile, show (and let you toggle) finger visibility (#77).
    if p.user_id == app.user.id {
        lines.push(Line::from(""));
        lines.push(field_line(
            "Finger",
            if p.finger_optout {
                "hidden (press f to list)"
            } else {
                "listed (press f to hide)"
            },
            dim,
        ));
    }
    // Blocked marker (#97): the reader has this user on their ignore list.
    if app.current_profile_blocked {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "\u{1F6AB} You have blocked this user (press b to unblock).",
            Style::default().fg(app.theme.warning_fg),
        )));
    }
    let title = format!(" Profile · {} ", truncate(&p.username, 24));
    let para = Paragraph::new(Text::from(lines))
        .block(Block::bordered().title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn render_ignore_list(f: &mut Frame, area: Rect, app: &App) {
    if app.ignored.is_empty() {
        return placeholder(
            f,
            area,
            " Ignored Users ",
            "You haven't blocked anyone. Block from a user's profile (b).",
        );
    }
    let lines: Vec<Line> = app
        .ignored
        .iter()
        .map(|(_, name)| Line::from(name.clone()))
        .collect();
    render_selectable(f, area, " Ignored Users ", lines, app.ignored_sel);
}

/// A `label: value` line with a dim label, for the profile/stats views.
fn field_line(label: &str, value: &str, dim: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:>13}: "), Style::default().fg(dim)),
        Span::raw(value.to_string()),
    ])
}

fn render_stats(f: &mut Frame, area: Rect, app: &App) {
    let Some(s) = &app.stats else {
        return placeholder(f, area, " Stats ", "No stats yet.");
    };
    let dim = app.theme.dim;
    let accent = app.theme.accent;
    let heading = |text: &str| {
        Line::from(Span::styled(
            text.to_string(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ))
    };
    let mut lines = vec![
        field_line("Users", &s.total_users.to_string(), dim),
        field_line("Posts", &s.total_posts.to_string(), dim),
        field_line("Calls", &s.total_calls.to_string(), dim),
        Line::from(""),
        heading("Top posters"),
    ];
    if s.top_posters.is_empty() {
        lines.push(Line::from("  (none yet)"));
    } else {
        for (i, p) in s.top_posters.iter().enumerate() {
            let posts = if p.posts == 1 { "post" } else { "posts" };
            lines.push(Line::from(format!(
                "  {:>2}. {:<20} {} {}",
                i + 1,
                truncate(&p.username, 20),
                p.posts,
                posts
            )));
        }
    }
    lines.push(Line::from(""));
    lines.push(heading("Recent callers"));
    if s.recent_callers.is_empty() {
        lines.push(Line::from("  (none yet)"));
    } else {
        for c in &s.recent_callers {
            lines.push(Line::from(format!(
                "  {:<20} {}",
                truncate(&c.username, 20),
                fmt_time(c.at)
            )));
        }
    }
    let para = Paragraph::new(Text::from(lines))
        .block(Block::bordered().title(" Stats "))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

fn render_doors(f: &mut Frame, area: Rect, app: &App) {
    if app.config.doors.is_empty() {
        return placeholder(f, area, " Door Games ", "No doors configured.");
    }
    let lines: Vec<Line> = app
        .config
        .doors
        .iter()
        .map(|d| {
            let limit = match d.time_limit_secs {
                0 => String::new(),
                s if s % 60 == 0 => format!("  ({} min limit)", s / 60),
                s => format!("  ({s}s limit)"),
            };
            Line::from(vec![
                Span::raw(format!("{:<20} ", truncate(&d.name, 20))),
                Span::styled(limit, Style::default().fg(app.theme.dim)),
            ])
        })
        .collect();
    render_selectable(f, area, " Door Games ", lines, app.door_sel);
}

fn render_search_results(f: &mut Frame, area: Rect, app: &App) {
    let title = format!(" Search: {} ", truncate(&app.search_query, 40));
    if app.search_results.is_empty() {
        return placeholder(f, area, &title, "No matches. Press '/' to search again.");
    }
    let lines: Vec<Line> = app
        .search_results
        .iter()
        .map(|h| {
            Line::from(vec![
                Span::styled(
                    format!("[{}] ", truncate(&h.board_name, 12)),
                    Style::default().fg(app.theme.accent),
                ),
                Span::raw(format!("{:<32} ", truncate(&h.subject, 32))),
                Span::styled(
                    format!(
                        "{:<12} {}",
                        truncate(&h.author_name, 12),
                        fmt_time(h.created_at)
                    ),
                    Style::default().fg(app.theme.dim),
                ),
            ])
        })
        .collect();
    render_selectable(f, area, &title, lines, app.search_sel);
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
    let mut lines: Vec<Line> = app
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
    // Composing mail to a `user@host` recipient is a remote fediverse DM — warn,
    // loudly, that it leaves the BBS and is not private (fediverse DMs are
    // plaintext on every server they touch).
    if app.screen == Screen::ComposeMail && app.form.fields[0].value.contains('@') {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "⚠  Remote recipient: this message LEAVES the BBS and is NOT private.",
            Style::default()
                .fg(app.theme.warning_fg)
                .add_modifier(Modifier::BOLD),
        )));
    }
    // Wrap so long field input (subject/body/username) stays visible instead of
    // running off the right edge while typing.
    let p = Paragraph::new(Text::from(lines))
        .block(Block::bordered().title(title.to_string()))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

/// The multi-line compose editor (#96): single-line header fields, then a
/// bordered body area with soft word-wrap and a visible cursor.
fn render_compose(f: &mut Frame, area: Rect, title: &str, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};

    // A remote (`user@host`) recipient means a fediverse DM — the same
    // not-private warning the old form showed, kept here.
    let remote_warn = app.screen == Screen::ComposeMail
        && app
            .form
            .fields
            .first()
            .is_some_and(|fld| fld.value.contains('@'));

    let header_lines = app.form.fields.len() as u16 + if remote_warn { 2 } else { 0 };
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_lines + 2), Constraint::Min(3)])
        .split(area);

    // ---- header fields ----
    let mut lines: Vec<Line> = app
        .form
        .fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let focused = !app.body_focused && i == app.form.focus;
            let marker = if focused { "> " } else { "  " };
            let line = Line::from(format!("{}{}: {}", marker, field.label, field.value));
            if focused {
                line.style(Style::default().add_modifier(Modifier::BOLD))
            } else {
                line
            }
        })
        .collect();
    if remote_warn {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "⚠  Remote recipient: this message LEAVES the BBS and is NOT private.",
            Style::default()
                .fg(app.theme.warning_fg)
                .add_modifier(Modifier::BOLD),
        )));
    }
    f.render_widget(
        Paragraph::new(Text::from(lines)).block(Block::bordered().title(title.to_string())),
        outer[0],
    );

    // ---- body ----
    let body_title = if app.body_focused {
        " Body — ^D send · Esc cancel "
    } else {
        " Body — Tab/Enter to edit "
    };
    let block = Block::bordered().title(body_title);
    let inner = block.inner(outer[1]);
    f.render_widget(block, outer[1]);

    let width = inner.width.max(1) as usize;
    let (rows, (cur_row, cur_col)) = app.body.display(width);

    // Scroll so the cursor row stays visible in a tall body.
    let height = inner.height.max(1) as usize;
    let top = cur_row.saturating_sub(height.saturating_sub(1));
    let visible: Vec<Line> = rows
        .iter()
        .skip(top)
        .take(height)
        .map(|r| Line::from(r.clone()))
        .collect();
    f.render_widget(Paragraph::new(Text::from(visible)), inner);

    // Place the terminal cursor when the body has focus, so it blinks where
    // typing will land.
    if app.body_focused {
        let x = inner.x + cur_col as u16;
        let y = inner.y + (cur_row - top) as u16;
        f.set_cursor_position((x, y));
    }
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
    render_selectable(f, area, " Admin · Logins ", lines, app.admin_login_sel);
}

fn render_admin_audit(f: &mut Frame, area: Rect, app: &App) {
    if app.admin_audit.is_empty() {
        return placeholder(f, area, " Admin · Audit ", "No moderator actions recorded.");
    }
    let lines: Vec<Line> = app
        .admin_audit
        .iter()
        .map(|e| {
            let detail = e
                .detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            Line::from(format!(
                "{:<17} {:<12} {:<12} {}{}",
                fmt_time(e.created_at),
                truncate(&e.actor, 12),
                truncate(&e.action, 12),
                truncate(&e.target, 24),
                truncate(&detail, 40)
            ))
        })
        .collect();
    render_selectable(f, area, " Admin · Audit ", lines, app.admin_audit_sel);
}

fn render_admin_federation(f: &mut Frame, area: Rect, app: &App) {
    if app.fed_policy.is_empty() {
        return placeholder(
            f,
            area,
            " Admin · Federation ",
            "No allow/block entries. a allow · b block · s silence a domain.",
        );
    }
    let lines: Vec<Line> = app
        .fed_policy
        .iter()
        .map(|(kind, domain, reason, severity)| {
            // Show the block severity; an allow entry has no severity to show.
            let tag = if kind == "block" {
                format!("[{severity}] ")
            } else {
                String::new()
            };
            let reason = if reason.is_empty() {
                String::new()
            } else {
                format!(" — {reason}")
            };
            Line::from(format!(
                "{:<7} {:<32} {}{}",
                kind,
                truncate(domain, 32),
                tag,
                truncate(&reason, 30)
            ))
        })
        .collect();
    render_selectable(f, area, " Admin · Federation ", lines, app.fed_sel);
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
  • SSH Keys       : register public keys to log in over SSH without a password
",
    );
    // How you get back in after registering depends on how you got here.
    text.push_str(match app.transport {
        Transport::Ssh => {
            "  • Register       : create an account, then reconnect over SSH with it\n"
        }
        Transport::Web => "  • Register       : create an account, then sign in again with it\n",
    });
    text.push_str(
        "
Navigation
  ↑/↓ move    Enter select/open    Esc or ← go back    q quit
  In forms: Tab/↑/↓ switch fields, Enter submits on the last field.

The guest account is read-only: it can browse boards and see who's online,
but cannot post or use mail. Register an account for the full experience.",
    );
    if let Some(hint) = other_transport_hint(app) {
        let label = match app.transport {
            Transport::Web => "This board is also reachable over SSH:",
            Transport::Ssh => "This board is also reachable in a browser:",
        };
        text.push_str(&format!("\n\n{label}\n  {hint}"));
    }
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
        Screen::Timeline => "Timeline",
        Screen::RemoteBoards => "Remote Boards",
        Screen::RemoteBoardPosts => "Remote Board",
        Screen::ComposeRemotePost => "Post",
        Screen::FollowRemote => "Follow",
        Screen::BoardList => "Boards",
        Screen::MessageList => "Messages",
        Screen::ReadMessage => "Reading",
        Screen::ComposePost => "New Post",
        Screen::Mailbox => "Mailbox",
        Screen::MailSearchInput => "Search Mail",
        Screen::MailSearchResults => "Mail Search",
        Screen::ReadMail => "Reading Mail",
        Screen::ConfirmDeleteMail => "Delete Mail",
        Screen::ComposeMail => "Compose Mail",
        Screen::WhoOnline => "Who's Online",
        Screen::ComposePage => "Page User",
        Screen::Profile => "Profile",
        Screen::IgnoreList => "Ignored Users",
        Screen::EditProfile => "Edit Profile",
        Screen::Stats => "Stats",
        Screen::SearchInput => "Search",
        Screen::SearchResults => "Search Results",
        Screen::Doors => "Door Games",
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
        Screen::AdminAudit => "Admin · Audit",
        Screen::AdminFederation => "Admin · Federation",
        Screen::ComposeFederation => "Add Domain",
        Screen::ComposeBroadcast => "Broadcast",
        Screen::AdminLogins => "Admin · Logins",
    }
}

fn hints(
    screen: Screen,
    is_admin: bool,
    can_edit_file: bool,
    can_edit_profile: bool,
    can_block: bool,
) -> String {
    let base = match screen {
        Screen::MainMenu => " ↑/↓ or [hotkey] · Enter select · Esc quit ",
        Screen::Bulletins => " ↑/↓ move · Enter read · Esc to menu ",
        Screen::Oneliners => " n new · Esc back ",
        Screen::ComposeOneliner => " type your oneliner · Enter post · Esc cancel ",
        Screen::Timeline => " ↑/↓ scroll · f follow · r refresh · Esc back ",
        Screen::RemoteBoards => " ↑/↓ select · Enter open · r refresh · Esc back ",
        Screen::RemoteBoardPosts => " ↑/↓ scroll · p post · r reply · R refresh · Esc back ",
        Screen::ComposeRemotePost => " Tab/Enter next field · Enter on Body sends · Esc cancel ",
        Screen::FollowRemote => " type user@host · Enter follow · Esc cancel ",
        Screen::BoardList => {
            if is_admin {
                " ↑/↓ move · Enter open · l lock/unlock · Esc back "
            } else {
                " ↑/↓ move · Enter open · Esc back "
            }
        }
        Screen::MessageList => {
            if is_admin {
                " ↑/↓ · Enter read · n post · r reply · e edit · p pin · d delete · Esc back "
            } else {
                " ↑/↓ · Enter read · n post · r reply · e/d edit·delete own · Esc back "
            }
        }
        Screen::ReadMessage => {
            if is_admin {
                " r reply · e edit · d delete · Esc back "
            } else {
                " r reply · e edit own · d delete own · Esc back "
            }
        }
        Screen::ReadMail => " r reply · f forward · d delete · Esc back ",
        Screen::ReadBulletin | Screen::Help => " Esc back ",
        Screen::ComposePost | Screen::ComposeMail => {
            " Tab/↑/↓ move · type body · Enter newline · ^D send · Esc cancel "
        }
        Screen::Register => " type to edit · Tab/↑/↓ fields · Enter next/submit · Esc cancel ",
        Screen::Mailbox => " ↑/↓ move · Enter read · n compose · / search · d delete · Esc back ",
        Screen::MailSearchInput => " type a query · Enter search · Esc cancel ",
        Screen::MailSearchResults => " ↑/↓ move · Enter read · / refine · Esc back ",
        Screen::ConfirmDeleteMail => " y delete · any key keep ",
        Screen::WhoOnline => " ↑/↓ move · Enter profile · p page · r refresh · Esc back ",
        Screen::ComposePage => " type your message · Enter send · Esc cancel ",
        Screen::Profile => {
            if can_edit_profile {
                " e edit · i ignored · f finger · Esc back "
            } else if can_block {
                " b block/unblock · Esc back "
            } else {
                " Esc back "
            }
        }
        Screen::IgnoreList => " ↑/↓ move · u/Enter unblock · Esc back ",
        Screen::EditProfile => " type · Tab/↑/↓ fields · Enter next/save · Esc cancel ",
        Screen::Stats => " r refresh · Esc back ",
        Screen::SearchInput => " type a query · Enter search · Esc cancel ",
        Screen::SearchResults => " ↑/↓ move · Enter open · / new search · Esc back ",
        Screen::Doors => " ↑/↓ move · Enter launch · Esc back ",
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
        Screen::AdminUsers => {
            " ↑/↓ · b ban · u unban · w broadcast · l logins · a audit · f federation · Esc back "
        }
        Screen::AdminAudit => " ↑/↓ move · PgUp/PgDn · Home/End · Esc back ",
        Screen::AdminFederation => " ↑/↓ · a allow · b block · s silence · d remove · Esc back ",
        Screen::ComposeFederation => " type a domain · Enter apply · Esc cancel ",
        Screen::ComposeBroadcast => " type your message · Enter send to all · Esc cancel ",
        Screen::AdminLogins => " ↑/↓ move · PgUp/PgDn · Home/End · Esc back ",
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

/// Word-wrap `s` to `width` columns (char-counted), breaking on whitespace and
/// hard-splitting any single word longer than `width`. Returns at least one
/// line (empty input yields `[""]`). Used for the read-only oneliners wall.
fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut cur_len = 0usize;
    for word in s.split_whitespace() {
        let wlen = word.chars().count();
        // A word wider than the line: flush, then hard-split it.
        if wlen > width {
            if cur_len > 0 {
                lines.push(std::mem::take(&mut cur));
                cur_len = 0;
            }
            let mut chunk = String::new();
            for c in word.chars() {
                chunk.push(c);
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
            }
            if !chunk.is_empty() {
                cur = chunk;
                cur_len = cur.chars().count();
            }
            continue;
        }
        let sep = if cur_len == 0 { 0 } else { 1 };
        if cur_len + sep + wlen > width {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
            cur_len = wlen;
        } else {
            if sep == 1 {
                cur.push(' ');
            }
            cur.push_str(word);
            cur_len += sep + wlen;
        }
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
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

#[cfg(test)]
mod tests {
    use super::wrap_text;

    #[test]
    fn wraps_on_word_boundaries() {
        assert_eq!(
            wrap_text("the quick brown fox", 9),
            vec!["the quick", "brown fox"]
        );
    }

    #[test]
    fn short_input_is_single_line() {
        assert_eq!(wrap_text("hello", 80), vec!["hello"]);
    }

    #[test]
    fn empty_input_yields_one_empty_line() {
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn hard_splits_overlong_word() {
        // A 10-char word with width 4 splits into 4 + 4 + 2.
        assert_eq!(wrap_text("aaaabbbbcc", 4), vec!["aaaa", "bbbb", "cc"]);
    }

    #[test]
    fn overlong_word_after_text_flushes_first() {
        assert_eq!(wrap_text("hi aaaabbbb", 4), vec!["hi", "aaaa", "bbbb"]);
    }

    #[test]
    fn never_exceeds_width() {
        let s = "supercalifragilistic expialidocious and some more words here";
        for line in wrap_text(s, 12) {
            assert!(line.chars().count() <= 12, "line too wide: {line:?}");
        }
    }
}
