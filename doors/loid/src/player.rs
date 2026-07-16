//! The player character, its progression, and its save file.

use std::path::PathBuf;

use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::content::{ARMORS, WEAPONS};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    pub name: String,
    pub level: i32,
    pub hp: i32,
    pub max_hp: i32,
    pub xp: i64,
    pub gold: i64,
    /// Index into [`WEAPONS`].
    pub weapon: usize,
    /// Index into [`ARMORS`].
    pub armor: usize,
    pub dragon_kills: u32,
    pub deaths: u32,
}

impl Player {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            level: 1,
            hp: 30,
            max_hp: 30,
            xp: 0,
            gold: 0,
            weapon: 0,
            armor: 0,
            dragon_kills: 0,
            deaths: 0,
        }
    }

    pub fn weapon_name(&self) -> &'static str {
        WEAPONS[self.weapon].name
    }
    pub fn armor_name(&self) -> &'static str {
        ARMORS[self.armor].name
    }

    /// XP required to reach the next level.
    pub fn xp_to_next(&self) -> i64 {
        (self.level as i64) * (self.level as i64) * 100
    }

    /// A single attack's damage: weapon + level scaling + a little luck.
    pub fn attack_power(&self, rng: &mut impl Rng) -> i32 {
        WEAPONS[self.weapon].attack + self.level * 3 + rng.random_range(0..=(self.level + 4))
    }

    /// Damage soaked by armor.
    pub fn defense(&self) -> i32 {
        ARMORS[self.armor].defense
    }

    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }

    pub fn heal_full(&mut self) {
        self.hp = self.max_hp;
    }

    /// Award XP and gold; level up (repeatedly, if warranted) and return how
    /// many levels were gained.
    pub fn grant(&mut self, xp: i64, gold: i64) -> u32 {
        self.xp += xp;
        self.gold += gold;
        let mut gained = 0;
        while self.xp >= self.xp_to_next() {
            self.xp -= self.xp_to_next();
            self.level += 1;
            self.max_hp += 15;
            self.heal_full();
            gained += 1;
        }
        gained
    }

    /// After slaying the dragon: a fresh journey, but the kill count endures.
    pub fn new_journey(&mut self) {
        let kills = self.dragon_kills;
        let name = self.name.clone();
        *self = Player::new(&name);
        self.dragon_kills = kills;
    }
}

/// Directory where per-player saves live (`$LOID_SAVE_DIR` or `./loid-saves`).
fn save_dir() -> PathBuf {
    std::env::var_os("LOID_SAVE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("loid-saves"))
}

fn save_path(name: &str) -> PathBuf {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    save_dir().join(format!("{slug}.json"))
}

/// Load a player's save, if present and valid.
pub fn load(name: &str) -> Option<Player> {
    let text = std::fs::read_to_string(save_path(name)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist a player's save (best effort).
pub fn save(player: &Player) {
    let dir = save_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_string_pretty(player) {
        let _ = std::fs::write(save_path(&player.name), json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leveling_consumes_xp_and_grows_hp() {
        let mut p = Player::new("Tester");
        let need = p.xp_to_next();
        let gained = p.grant(need, 10);
        assert_eq!(gained, 1);
        assert_eq!(p.level, 2);
        assert_eq!(p.max_hp, 45);
        assert_eq!(p.hp, 45); // healed on level-up
        assert_eq!(p.gold, 10);
    }

    #[test]
    fn new_journey_keeps_dragon_kills() {
        let mut p = Player::new("Hero");
        p.dragon_kills = 3;
        p.level = 12;
        p.new_journey();
        assert_eq!(p.level, 1);
        assert_eq!(p.dragon_kills, 3);
    }
}
