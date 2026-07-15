//! Legend of the Indigo Dragon — a small LORD-inspired door game.
//!
//! Built on [`doorkit`], so it runs under bbs-rs or any BBS that provides a
//! drop file + a terminal. Configure it as a door pointing `command` at this
//! binary, with a `cwd` the process can write to (for `loid-saves/`).

mod content;
mod player;

use std::io::Result;

use doorkit::{Color, Key, Session, Terminal};
use rand::Rng;
use rand::rngs::ThreadRng;

use content::{ARMORS, DRAGON_LEVEL, Monster, WEAPONS, spawn_dragon, spawn_forest};
use player::Player;

fn main() {
    let session = Session::load();
    let term = match Terminal::new() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("loid: cannot set up the terminal: {e}");
            return;
        }
    };
    let mut game = Game {
        term,
        rng: rand::rng(),
        session,
    };
    if let Err(e) = game.run() {
        // The terminal is restored by Drop; just note the error.
        eprintln!("loid: {e}");
    }
    // `game` (and its Terminal) drops here, restoring the terminal.
}

struct Game {
    term: Terminal,
    rng: ThreadRng,
    session: Session,
}

enum Combat {
    Won,
    Fled,
    Died,
}

impl Game {
    fn run(&mut self) -> Result<()> {
        let mut player = player::load(&self.session.username)
            .unwrap_or_else(|| Player::new(&self.session.username));
        // A returning player who died last time comes back rested.
        if !player.is_alive() {
            player.heal_full();
        }

        self.title(&player)?;
        self.town(&mut player)?;

        player::save(&player);
        self.term.reset()?;
        self.term.clear()?;
        self.term.say(
            Color::Cyan,
            "The tavern door swings shut behind you. Farewell!",
        )?;
        Ok(())
    }

    // ---- Screens ---------------------------------------------------------

    fn title(&mut self, player: &Player) -> Result<()> {
        self.term.clear()?;
        let art = [
            "                                                 ",
            "     L E G E N D   O F   T H E                   ",
            "        ___           _ _                        ",
            "       |_ _|_ _  __ _(_) |_  ___                 ",
            "        | || ' \\/ _` | | / _ \\                   ",
            "       |___|_||_\\__,_|_|_\\___/                   ",
            "           D  R  A  G  O  N                      ",
        ];
        self.term.bold()?;
        self.term.color(Color::Magenta)?;
        for line in art {
            self.term.println(line)?;
        }
        self.term.reset()?;
        self.term.println("")?;
        self.term
            .say(Color::Cyan, &format!("Welcome, {}!", player.name))?;
        if player.dragon_kills > 0 {
            self.term.say(
                Color::Yellow,
                &format!(
                    "Slayer of the Indigo Dragon {} time(s). The realm remembers.",
                    player.dragon_kills
                ),
            )?;
        }
        self.term.println("")?;
        self.term.print("Press any key to enter the town...")?;
        self.term.pause()?;
        Ok(())
    }

    fn town(&mut self, player: &mut Player) -> Result<()> {
        loop {
            if self.session.time_up() {
                self.term.println("")?;
                self.term
                    .say(Color::Red, "Your time in the realm has run out. Rest well.")?;
                return Ok(());
            }

            self.term.clear()?;
            self.status_bar(player)?;
            self.term.println("")?;
            self.term.say(Color::Green, "== The Town of Emberhold ==")?;
            self.term.println("")?;
            self.menu_line("F", "the Forest (fight monsters)")?;
            self.menu_line("H", "the Healer's Hut")?;
            self.menu_line("W", "the Weapon Shop")?;
            self.menu_line("A", "the Armorer")?;
            self.menu_line("S", "view your Stats")?;
            if player.level >= DRAGON_LEVEL {
                self.term
                    .say(Color::Magenta, "  (D)  seek the INDIGO DRAGON's lair!")?;
            }
            self.menu_line("Q", "Quit back to the BBS")?;
            self.term.println("")?;
            self.term.print("Your choice: ")?;

            match self.read_choice()? {
                'f' => self.forest(player)?,
                'h' => self.healer(player)?,
                'w' => self.weapon_shop(player)?,
                'a' => self.armor_shop(player)?,
                's' => self.stats(player)?,
                'd' if player.level >= DRAGON_LEVEL => self.dragon(player)?,
                'q' => return Ok(()),
                _ => {}
            }
        }
    }

    fn stats(&mut self, player: &Player) -> Result<()> {
        self.term.clear()?;
        self.term.say(Color::Green, "== Your Stats ==")?;
        self.term.println("")?;
        self.term.println(&format!("  Name    : {}", player.name))?;
        self.term
            .println(&format!("  Level   : {}", player.level))?;
        self.term
            .println(&format!("  HP      : {}/{}", player.hp, player.max_hp))?;
        self.term.println(&format!(
            "  XP      : {} / {}",
            player.xp,
            player.xp_to_next()
        ))?;
        self.term.println(&format!("  Gold    : {}", player.gold))?;
        self.term
            .println(&format!("  Weapon  : {}", player.weapon_name()))?;
        self.term
            .println(&format!("  Armor   : {}", player.armor_name()))?;
        self.term
            .println(&format!("  Dragons : {}", player.dragon_kills))?;
        self.term
            .println(&format!("  Deaths  : {}", player.deaths))?;
        self.term.println("")?;
        self.press_any()?;
        Ok(())
    }

    // ---- The forest & combat --------------------------------------------

    fn forest(&mut self, player: &mut Player) -> Result<()> {
        let monster = spawn_forest(player.level, &mut self.rng);
        self.term.clear()?;
        self.term
            .say(Color::Green, "You venture into the dark forest...")?;
        self.term.println("")?;
        self.term
            .say(Color::Yellow, &format!("You encounter {}!", monster.name))?;
        std::thread::sleep(std::time::Duration::from_millis(400));
        match self.combat(player, monster.clone())? {
            Combat::Won => {
                let levels = player.grant(monster.xp, monster.gold);
                self.term.println("")?;
                self.term.say(
                    Color::Cyan,
                    &format!(
                        "You vanquish {}! +{} XP, +{} gold.",
                        monster.name, monster.xp, monster.gold
                    ),
                )?;
                if levels > 0 {
                    self.term.say(
                        Color::Magenta,
                        &format!(
                            "*** You reached level {}! Max HP is now {}. ***",
                            player.level, player.max_hp
                        ),
                    )?;
                }
            }
            Combat::Fled => {
                self.term.println("")?;
                self.term
                    .say(Color::White, "You escape back to town, breathless.")?;
            }
            Combat::Died => self.on_death(player)?,
        }
        self.press_any()
    }

    fn dragon(&mut self, player: &mut Player) -> Result<()> {
        self.term.clear()?;
        self.term.say(
            Color::Magenta,
            "You climb to the Indigo Dragon's lair. The air hums with cold power.",
        )?;
        self.term.println("")?;
        self.term.print("Do you dare face it? (y/n) ")?;
        if self.read_choice()? != 'y' {
            return Ok(());
        }
        let dragon = spawn_dragon(player.dragon_kills);
        match self.combat(player, dragon.clone())? {
            Combat::Won => {
                player.grant(dragon.xp, dragon.gold);
                player.dragon_kills += 1;
                self.term.clear()?;
                self.term.bold()?;
                self.term.color(Color::Yellow)?;
                self.term
                    .println("*******************************************")?;
                self.term
                    .println("*   THE INDIGO DRAGON IS SLAIN!  VICTORY!  *")?;
                self.term
                    .println("*******************************************")?;
                self.term.reset()?;
                self.term.println("")?;
                self.term.say(
                    Color::Cyan,
                    &format!(
                        "You claim {} gold and eternal glory. Dragons slain: {}.",
                        dragon.gold, player.dragon_kills
                    ),
                )?;
                self.term.say(
                    Color::White,
                    "A new journey begins — but the realm will remember your name.",
                )?;
                player.new_journey();
            }
            Combat::Fled => {
                self.term.println("")?;
                self.term.say(
                    Color::White,
                    "You flee the lair. The dragon's laughter follows you.",
                )?;
            }
            Combat::Died => self.on_death(player)?,
        }
        self.press_any()
    }

    /// Fight `monster` to the death (or a successful escape).
    fn combat(&mut self, player: &mut Player, mut monster: Monster) -> Result<Combat> {
        loop {
            self.term.clear()?;
            self.term
                .say(Color::Yellow, &format!("~ {} ~", monster.name))?;
            self.hp_bar("Foe ", monster.hp, monster.max_hp, Color::Red)?;
            self.hp_bar("You ", player.hp, player.max_hp, Color::Green)?;
            self.term.println("")?;
            self.term.print("(A)ttack   (R)un   ")?;

            match self.read_choice()? {
                'a' => {
                    let dmg = player.attack_power(&mut self.rng);
                    monster.hp -= dmg;
                    self.term.println("")?;
                    self.term.say(
                        Color::Cyan,
                        &format!("You strike {} for {dmg} damage!", monster.name),
                    )?;
                    if monster.hp <= 0 {
                        return Ok(Combat::Won);
                    }
                    // Monster retaliates.
                    let hit =
                        (monster.attack - player.defense() + self.rng.random_range(-2..=2)).max(1);
                    player.hp -= hit;
                    self.term
                        .say(Color::Red, &format!("{} hits you for {hit}!", monster.name))?;
                    if !player.is_alive() {
                        return Ok(Combat::Died);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                'r' => {
                    // The dragon does not let its prey leave.
                    let flee = !monster.is_dragon && self.rng.random_range(0..100) < 55;
                    if flee {
                        return Ok(Combat::Fled);
                    }
                    self.term.println("")?;
                    self.term.say(Color::White, "You fail to escape!")?;
                    let hit = (monster.attack - player.defense()).max(1);
                    player.hp -= hit;
                    self.term
                        .say(Color::Red, &format!("{} hits you for {hit}!", monster.name))?;
                    if !player.is_alive() {
                        return Ok(Combat::Died);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                _ => {}
            }
        }
    }

    fn on_death(&mut self, player: &mut Player) -> Result<()> {
        player.deaths += 1;
        let lost = player.gold / 2;
        player.gold -= lost;
        player.heal_full();
        self.term.println("")?;
        self.term.say(Color::Red, "You have fallen in battle...")?;
        self.term.say(
            Color::White,
            &format!("A forest sprite revives you in town. You lost {lost} gold in the fray.",),
        )
    }

    // ---- Shops -----------------------------------------------------------

    fn weapon_shop(&mut self, player: &mut Player) -> Result<()> {
        self.shop(player, true)
    }
    fn armor_shop(&mut self, player: &mut Player) -> Result<()> {
        self.shop(player, false)
    }

    /// A gear shop. `weapons` selects the weapon table vs. the armor table.
    fn shop(&mut self, player: &mut Player, weapons: bool) -> Result<()> {
        self.term.clear()?;
        let title = if weapons {
            "== Weapon Shop =="
        } else {
            "== Armorer =="
        };
        self.term.say(Color::Green, title)?;
        self.term
            .say(Color::Yellow, &format!("Gold: {}", player.gold))?;
        self.term.println("")?;

        let owned = if weapons { player.weapon } else { player.armor };
        let count = if weapons { WEAPONS.len() } else { ARMORS.len() };
        for i in 0..count {
            let (name, stat, price, label) = if weapons {
                let w = &WEAPONS[i];
                (w.name, w.attack, w.price, "atk")
            } else {
                let a = &ARMORS[i];
                (a.name, a.defense, a.price, "def")
            };
            let tag = if i == owned {
                " (equipped)".to_string()
            } else if i < owned {
                " (owned)".to_string()
            } else {
                format!("  {price} gold")
            };
            self.term.println(&format!(
                "  {}) {:<22} {label} {:<3}{}",
                i + 1,
                name,
                stat,
                tag
            ))?;
        }
        self.term.println("")?;
        self.term.print("Buy which number (or Q to leave)? ")?;
        let c = self.read_choice()?;
        if let Some(d) = c.to_digit(10) {
            let idx = d as usize;
            if idx >= 1 && idx <= count {
                let i = idx - 1;
                let price = if weapons {
                    WEAPONS[i].price
                } else {
                    ARMORS[i].price
                };
                if i <= owned {
                    self.term
                        .say(Color::White, "You already have that or better.")?;
                } else if player.gold < price {
                    self.term.say(Color::Red, "You can't afford that.")?;
                } else {
                    player.gold -= price;
                    if weapons {
                        player.weapon = i;
                    } else {
                        player.armor = i;
                    }
                    self.term.say(Color::Cyan, "A fine purchase! Equipped.")?;
                }
                self.press_any()?;
            }
        }
        Ok(())
    }

    fn healer(&mut self, player: &mut Player) -> Result<()> {
        self.term.clear()?;
        self.term.say(Color::Green, "== The Healer's Hut ==")?;
        let missing = player.max_hp - player.hp;
        let cost = (missing as i64 * 2).max(0);
        self.term
            .println(&format!("  HP: {}/{}", player.hp, player.max_hp))?;
        if missing <= 0 {
            self.term
                .say(Color::White, "You are already at full health.")?;
            return self.press_any();
        }
        self.term
            .println(&format!("  Full heal costs {cost} gold."))?;
        self.term
            .println(&format!("  You have {} gold.", player.gold))?;
        self.term.println("")?;
        self.term.print("Heal up? (y/n) ")?;
        if self.read_choice()? == 'y' {
            if player.gold >= cost {
                player.gold -= cost;
                player.heal_full();
                self.term.say(Color::Cyan, "You feel restored!")?;
            } else {
                self.term.say(Color::Red, "You can't afford it.")?;
            }
            self.press_any()?;
        }
        Ok(())
    }

    // ---- Small UI helpers ------------------------------------------------

    fn status_bar(&mut self, player: &Player) -> Result<()> {
        let time = match self.session.time_left() {
            Some(d) => format!("  {}m left", d.as_secs() / 60),
            None => String::new(),
        };
        self.term.color(Color::Blue)?;
        self.term.println(&format!(
            "[{}]  Lv {}  HP {}/{}  Gold {}{}",
            player.name, player.level, player.hp, player.max_hp, player.gold, time
        ))?;
        self.term.reset()
    }

    fn menu_line(&mut self, key: &str, text: &str) -> Result<()> {
        self.term
            .print(&format!("\x1b[1;33m  ({key})\x1b[0m  {text}\r\n"))
    }

    fn hp_bar(&mut self, label: &str, hp: i32, max: i32, color: Color) -> Result<()> {
        let width = 24i32;
        let filled = if max > 0 {
            (hp.max(0) * width / max).clamp(0, width)
        } else {
            0
        };
        let bar: String = "#".repeat(filled as usize) + &"-".repeat((width - filled) as usize);
        self.term.color(color)?;
        self.term
            .print(&format!("{label}[{bar}] {}/{}\r\n", hp.max(0), max))?;
        self.term.reset()
    }

    fn press_any(&mut self) -> Result<()> {
        self.term.println("")?;
        self.term.print("[ press any key ]")?;
        self.term.pause()
    }

    /// Read a key and return it as a lowercase char (`\0` for non-char keys).
    fn read_choice(&mut self) -> Result<char> {
        Ok(match self.term.read_key()? {
            Key::Char(c) => c.to_ascii_lowercase(),
            Key::Enter => '\n',
            Key::Esc => 'q',
            _ => '\0',
        })
    }
}
