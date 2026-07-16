//! Static game content (gear tables) and monster generation.

use rand::Rng;

pub struct Weapon {
    pub name: &'static str,
    pub attack: i32,
    pub price: i64,
}

pub struct Armor {
    pub name: &'static str,
    pub defense: i32,
    pub price: i64,
}

/// Weapon tiers, cheapest first. A player owns an index into this table.
pub const WEAPONS: &[Weapon] = &[
    Weapon {
        name: "Bare Fists",
        attack: 3,
        price: 0,
    },
    Weapon {
        name: "Rusty Dagger",
        attack: 8,
        price: 60,
    },
    Weapon {
        name: "Short Sword",
        attack: 16,
        price: 250,
    },
    Weapon {
        name: "Broadsword",
        attack: 30,
        price: 800,
    },
    Weapon {
        name: "War Axe",
        attack: 52,
        price: 2200,
    },
    Weapon {
        name: "Dragonfang Blade",
        attack: 85,
        price: 6000,
    },
];

/// Armor tiers, cheapest first.
pub const ARMORS: &[Armor] = &[
    Armor {
        name: "Peasant Rags",
        defense: 0,
        price: 0,
    },
    Armor {
        name: "Leather Jerkin",
        defense: 4,
        price: 50,
    },
    Armor {
        name: "Chainmail",
        defense: 10,
        price: 220,
    },
    Armor {
        name: "Plate Armor",
        defense: 20,
        price: 700,
    },
    Armor {
        name: "Knight's Aegis",
        defense: 34,
        price: 1900,
    },
    Armor {
        name: "Indigo Dragonscale",
        defense: 55,
        price: 5500,
    },
];

/// Level a player must reach before the Indigo Dragon will face them.
pub const DRAGON_LEVEL: i32 = 8;

const FOREST_MONSTERS: &[&str] = &[
    "a Snarling Kobold",
    "a Giant Forest Rat",
    "a Goblin Scout",
    "a Wild Boar",
    "a Skeleton Warrior",
    "a Bog Lurker",
    "a Dire Wolf",
    "a Bandit Rogue",
    "an Ogre Brute",
    "a Shadow Wisp",
    "a Venomous Serpent",
    "a Corrupted Treant",
];

#[derive(Debug, Clone)]
pub struct Monster {
    pub name: String,
    pub hp: i32,
    pub max_hp: i32,
    pub attack: i32,
    pub xp: i64,
    pub gold: i64,
    pub is_dragon: bool,
}

/// A random forest monster scaled to the player's `level`.
pub fn spawn_forest(level: i32, rng: &mut impl Rng) -> Monster {
    let name = FOREST_MONSTERS[rng.random_range(0..FOREST_MONSTERS.len())].to_string();
    let hp = 12 + level * 8 + rng.random_range(0..=(level * 4).max(1));
    Monster {
        name,
        hp,
        max_hp: hp,
        attack: 3 + level * 2 + rng.random_range(0..=level.max(1)),
        xp: (15 + level as i64 * 10) + rng.random_range(0..=20),
        gold: (8 + level as i64 * 6) + rng.random_range(0..=(level as i64 * 4).max(1)),
        is_dragon: false,
    }
}

/// The Indigo Dragon, growing tougher with each time it's been slain.
pub fn spawn_dragon(kills: u32) -> Monster {
    let k = kills as i32;
    let hp = 220 + k * 60;
    Monster {
        name: "the Indigo Dragon".to_string(),
        hp,
        max_hp: hp,
        attack: 30 + k * 6,
        xp: 1000 + kills as i64 * 300,
        gold: 2000 + kills as i64 * 500,
        is_dragon: true,
    }
}
