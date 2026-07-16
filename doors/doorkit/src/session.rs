//! The calling user's session, resolved from a drop file and/or environment.

use std::path::Path;
use std::time::{Duration, Instant};

/// Everything a door needs to know about who's calling and their limits.
#[derive(Debug, Clone)]
pub struct Session {
    /// The user's name (from the drop file or `BBS_USER`).
    pub username: String,
    /// Node / line number.
    pub node: u32,
    /// Security level (higher = more privileged).
    pub security: u32,
    /// Terminal width / height in cells.
    pub cols: u16,
    pub rows: u16,
    /// Total time allotted for this door (if the BBS imposes one).
    pub time_limit: Option<Duration>,
    start: Instant,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            username: "Player".into(),
            node: 1,
            security: 10,
            cols: 80,
            rows: 24,
            time_limit: None,
            start: Instant::now(),
        }
    }
}

impl Session {
    /// Resolve the session from the environment: a drop file in `dir` (or the
    /// current directory) provides the baseline; bbs-rs environment variables
    /// override individual fields when present.
    pub fn load() -> Self {
        Self::load_from(Path::new("."))
    }

    /// Like [`Session::load`], but looking for the drop file under `dir`.
    pub fn load_from(dir: &Path) -> Self {
        let mut s = Session::default();
        if let Some(df) = read_drop_file(dir) {
            if !df.name.is_empty() {
                s.username = df.name;
            }
            if let Some(sec) = df.security {
                s.security = sec;
            }
            if let Some(mins) = df.minutes_left {
                s.time_limit = Some(Duration::from_secs(mins * 60));
            }
        }
        // Environment overrides (set by bbs-rs, and easy for any BBS to provide).
        if let Ok(u) = std::env::var("BBS_USER")
            && !u.is_empty()
        {
            s.username = u;
        }
        if let Some(n) = env_parse("BBS_NODE") {
            s.node = n;
        }
        if let Some(c) = env_parse("BBS_COLS") {
            s.cols = c;
        }
        if let Some(r) = env_parse("BBS_ROWS") {
            s.rows = r;
        }
        if let Some(secs) = env_parse::<u64>("BBS_TIME_LEFT_SECS")
            && secs > 0
        {
            s.time_limit = Some(Duration::from_secs(secs));
        }
        s.start = Instant::now();
        s
    }

    /// Time remaining before the limit, or `None` if unlimited.
    pub fn time_left(&self) -> Option<Duration> {
        self.time_limit
            .map(|d| d.saturating_sub(self.start.elapsed()))
    }

    /// Whether the allotted time has run out.
    pub fn time_up(&self) -> bool {
        self.time_left().map(|d| d.is_zero()).unwrap_or(false)
    }
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok()?.trim().parse().ok()
}

/// The subset of drop-file fields a door typically needs.
struct DropFile {
    name: String,
    security: Option<u32>,
    minutes_left: Option<u64>,
}

/// Look for a `DORINFO1.DEF` or `DOOR.SYS` (any case) in `dir` and parse it.
fn read_drop_file(dir: &Path) -> Option<DropFile> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let lower = name.to_string_lossy().to_ascii_lowercase();
        let text = std::fs::read_to_string(entry.path()).ok();
        match (lower.as_str(), text) {
            ("dorinfo1.def", Some(t)) => return Some(parse_dorinfo(&t)),
            ("door.sys", Some(t)) => return Some(parse_door_sys(&t)),
            _ => {}
        }
    }
    None
}

/// `DORINFO1.DEF`: user first/last on lines 7–8, security on 11, minutes on 12.
fn parse_dorinfo(text: &str) -> DropFile {
    let lines: Vec<&str> = text.lines().collect();
    let get = |i: usize| lines.get(i).map(|s| s.trim()).unwrap_or("");
    let name = format!("{} {}", get(6), get(7)).trim().to_string();
    DropFile {
        name,
        security: get(10).parse().ok(),
        minutes_left: get(11).parse().ok(),
    }
}

/// `DOOR.SYS`: full name on line 10, security on 15, minutes on 19.
fn parse_door_sys(text: &str) -> DropFile {
    let lines: Vec<&str> = text.lines().collect();
    let get = |i: usize| lines.get(i).map(|s| s.trim()).unwrap_or("");
    DropFile {
        name: get(9).to_string(),
        security: get(14).parse().ok(),
        minutes_left: get(18).parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dorinfo() {
        let df =
            parse_dorinfo("BBS\nSy\nsop\nCOM0\n0 BAUD\n0\nAlice\nSmith\nTown\n1\n40\n25\n-1\n");
        assert_eq!(df.name, "Alice Smith");
        assert_eq!(df.security, Some(40));
        assert_eq!(df.minutes_left, Some(25));
    }

    #[test]
    fn parses_door_sys() {
        let mut lines = vec![""; 20];
        lines[9] = "Bob Jones";
        lines[14] = "30";
        lines[18] = "60";
        let df = parse_door_sys(&lines.join("\r\n"));
        assert_eq!(df.name, "Bob Jones");
        assert_eq!(df.security, Some(30));
        assert_eq!(df.minutes_left, Some(60));
    }
}
