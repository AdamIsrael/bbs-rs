# Door games

External "door" programs for bbs-rs (and other BBSes). These are a **Cargo
workspace** of their own, deliberately kept free of any dependency on the
`bbs-rs` crate — a door only needs a **drop file** and a **terminal**, so these
binaries run under any BBS that speaks that contract and could be split into a
separate repo unchanged.

- [`doorkit`](doorkit) — a small, BBS-agnostic library: parse the drop file
  (`DOOR.SYS` / `DORINFO1.DEF`) with environment-variable overrides, and manage
  the terminal (raw mode with restore-on-exit, key input, ANSI helpers). This is
  the framework new doors build on.
- [`loid`](loid) — **Legend of the Indigo Dragon**, a small
  [LORD](https://en.wikipedia.org/wiki/Legend_of_the_Red_Dragon)-inspired RPG:
  the town of Emberhold, forest combat, leveling, weapon/armor shops, a healer,
  a per-player save file, and the Indigo Dragon as the endgame boss.

## Building

They build with the rest of the repo (they're workspace members):

```sh
cargo build --release          # produces target/release/loid
```

## Wiring a door into bbs-rs

Point a `[[doors]]` entry at the built binary and give it a writable `cwd` (LOID
keeps saves in `loid-saves/` there):

```toml
[[doors]]
name = "Legend of the Indigo Dragon"
command = "/path/to/bbs-rs/target/release/loid"
cwd = "/var/bbs/doors/loid"     # writable; LOID stores saves here
time_limit_secs = 600           # 0 = no limit
drop_file = "dorinfo1.def"      # or "door.sys"; blank = none
```

The door menu appears in the BBS once at least one door is configured.

## Environment a door receives

bbs-rs sets these (any BBS can, too); `doorkit` reads them, falling back to the
drop file and then defaults:

| Variable | Meaning |
|---|---|
| `BBS_USER` | the caller's name |
| `BBS_USER_ROLE` | their role/level |
| `BBS_NODE` | node / line number |
| `BBS_COLS`, `BBS_ROWS` | terminal size |
| `BBS_TIME_LEFT_SECS` | remaining time (0 = unlimited) |
| `TERM` | `xterm-256color` |

## Writing your own door

Depend on `doorkit`, load the [`Session`], make a [`Terminal`], and go:

```rust
use doorkit::{Session, Terminal, Color};

fn main() -> std::io::Result<()> {
    let session = Session::load();          // drop file + env
    let mut term = Terminal::new()?;        // raw mode; restored on drop
    term.clear()?;
    term.say(Color::Cyan, &format!("Hello, {}!", session.username))?;
    term.pause()?;                          // wait for a key
    Ok(())                                  // Terminal's Drop resets the tty
}
```

`Terminal` restores the terminal on drop (even if the door is killed at its time
limit), so the BBS screen comes back cleanly.
