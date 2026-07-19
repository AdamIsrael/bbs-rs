//! The editable config document: read values, change them, write the file back
//! without disturbing anything else (#141).

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item, Value};

use super::schema::{self, FieldKind};

/// A setting's value, in the shapes the editor supports.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<String>),
}

impl FieldValue {
    /// How the value reads in a list of settings.
    pub fn display(&self) -> String {
        match self {
            FieldValue::Bool(b) => b.to_string(),
            FieldValue::Int(i) => i.to_string(),
            FieldValue::Str(s) => s.clone(),
            FieldValue::List(v) => v.join(", "),
        }
    }
}

/// Something wrong with the configuration, found before saving rather than at
/// the next boot.
#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    /// `[section]` the problem belongs to, for jumping straight there.
    pub section: String,
    pub message: String,
    /// A hard error blocks a working board; a warning is worth knowing.
    pub blocking: bool,
}

/// `bbs.toml`, loaded for editing.
pub struct ConfigDoc {
    path: PathBuf,
    doc: DocumentMut,
    /// The bytes we loaded, to tell whether anything actually changed.
    original: String,
}

impl ConfigDoc {
    /// Load a config for editing. A missing file starts from the annotated
    /// default that `bbs-rs` itself would write, so a new board gets the same
    /// commented, self-documenting file rather than a bare skeleton.
    pub fn load(path: &Path) -> Result<Self> {
        let original = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                crate::config::DEFAULT_CONFIG_TOML.to_string()
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let doc: DocumentMut = original
            .parse()
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            doc,
            original,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the file existed when we loaded it.
    pub fn is_new(&self) -> bool {
        !self.path.exists()
    }

    /// The current text, exactly as it would be written.
    pub fn to_text(&self) -> String {
        self.doc.to_string()
    }

    /// Whether anything changed since loading.
    pub fn is_dirty(&self) -> bool {
        self.doc.to_string() != self.original
    }

    /// Read a setting. `None` means the key isn't present — which for most
    /// settings means "the built-in default applies", not "empty".
    pub fn get(&self, section: &str, key: &str) -> Option<FieldValue> {
        let item = self.doc.get(section)?.get(key)?;
        let kind = schema::section(section)
            .and_then(|s| s.field(key))
            .map(|f| f.kind);
        Some(match (item, kind) {
            (Item::Value(Value::Boolean(b)), _) => FieldValue::Bool(*b.value()),
            (Item::Value(Value::Integer(i)), _) => FieldValue::Int(*i.value()),
            (Item::Value(Value::Array(a)), _) => FieldValue::List(
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            ),
            (Item::Value(Value::String(s)), _) => FieldValue::Str(s.value().clone()),
            // Anything else (a table, a float, a date) isn't editable here; report
            // it as its rendered text so a UI can show it read-only rather than
            // pretend it's absent.
            (other, _) => FieldValue::Str(other.to_string().trim().to_string()),
        })
    }

    /// The value a UI should show: what's set, or the default from the shipped
    /// config when the key is absent (usually because it ships commented out).
    pub fn effective(&self, section: &str, key: &str) -> Option<FieldValue> {
        self.get(section, key).or_else(|| default_of(section, key))
    }

    /// Change a setting.
    ///
    /// Creates the `[section]` if it doesn't exist, and adds the key inside it
    /// if it's absent. A key that ships only as a commented-out example gets a
    /// real entry added alongside the comment — we don't uncomment in place,
    /// because the comment usually explains the setting and is worth keeping.
    pub fn set(&mut self, section: &str, key: &str, value: FieldValue) {
        if self.doc.get(section).is_none() {
            self.doc[section] = Item::Table(toml_edit::Table::new());
        }
        self.doc[section][key] = match value {
            FieldValue::Bool(b) => toml_edit::value(b),
            FieldValue::Int(i) => toml_edit::value(i),
            FieldValue::Str(s) => toml_edit::value(s),
            FieldValue::List(items) => {
                let mut arr = Array::new();
                for i in items {
                    arr.push(i);
                }
                toml_edit::value(arr)
            }
        };
    }

    /// Remove a setting, falling back to the built-in default.
    pub fn unset(&mut self, section: &str, key: &str) -> bool {
        match self.doc.get_mut(section) {
            Some(t) => t
                .as_table_like_mut()
                .is_some_and(|t| t.remove(key).is_some()),
            None => false,
        }
    }

    // ---- Door games: an array of tables (#145) -------------------------
    //
    // `[[doors]]` is a list of entries rather than a set of settings, so it gets
    // its own small API. Everything here goes through `ArrayOfTables`, which
    // keeps each entry's own comments and formatting — removing door 2 leaves
    // doors 1 and 3 exactly as the operator wrote them.

    /// How many doors are configured.
    pub fn door_count(&self) -> usize {
        self.doc
            .get("doors")
            .and_then(|i| i.as_array_of_tables())
            .map(|a| a.len())
            .unwrap_or(0)
    }

    /// Every door's menu label, for the list screen. A door with no `name` yet
    /// shows as `(unnamed)` rather than vanishing.
    pub fn door_names(&self) -> Vec<String> {
        let Some(arr) = self.doc.get("doors").and_then(|i| i.as_array_of_tables()) else {
            return Vec::new();
        };
        arr.iter()
            .map(|t| {
                t.get("name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("(unnamed)")
                    .to_string()
            })
            .collect()
    }

    /// Read one field of one door.
    pub fn door_get(&self, index: usize, key: &str) -> Option<FieldValue> {
        let arr = self.doc.get("doors")?.as_array_of_tables()?;
        let item = arr.get(index)?.get(key)?;
        Some(match item {
            Item::Value(Value::Boolean(b)) => FieldValue::Bool(*b.value()),
            Item::Value(Value::Integer(i)) => FieldValue::Int(*i.value()),
            Item::Value(Value::Array(a)) => FieldValue::List(
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
            ),
            Item::Value(Value::String(s)) => FieldValue::Str(s.value().clone()),
            other => FieldValue::Str(other.to_string().trim().to_string()),
        })
    }

    /// Set one field of one door. Out-of-range indexes are ignored rather than
    /// panicking — the UI and the document can disagree if something else edited
    /// the file underneath us.
    pub fn door_set(&mut self, index: usize, key: &str, value: FieldValue) {
        let Some(arr) = self
            .doc
            .get_mut("doors")
            .and_then(|i| i.as_array_of_tables_mut())
        else {
            return;
        };
        let Some(table) = arr.get_mut(index) else {
            return;
        };
        table[key] = match value {
            FieldValue::Bool(b) => toml_edit::value(b),
            FieldValue::Int(i) => toml_edit::value(i),
            FieldValue::Str(s) => toml_edit::value(s),
            FieldValue::List(items) => {
                let mut a = Array::new();
                for i in items {
                    a.push(i);
                }
                toml_edit::value(a)
            }
        };
    }

    /// Append a door, pre-filled so it's a valid entry from the moment it
    /// exists — a half-written `[[doors]]` block would stop the whole config
    /// parsing, and the operator might not save immediately.
    pub fn door_add(&mut self, name: &str, command: &str) -> usize {
        if self
            .doc
            .get("doors")
            .and_then(|i| i.as_array_of_tables())
            .is_none()
        {
            self.doc["doors"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
        }
        let arr = self.doc["doors"]
            .as_array_of_tables_mut()
            .expect("just set");
        let mut table = toml_edit::Table::new();
        table["name"] = toml_edit::value(name);
        table["command"] = toml_edit::value(command);
        table["args"] = toml_edit::value(Array::new());
        table["time_limit_secs"] = toml_edit::value(0i64);
        arr.push(table);
        arr.len() - 1
    }

    /// Remove a door. Returns whether one was there.
    pub fn door_remove(&mut self, index: usize) -> bool {
        let Some(arr) = self
            .doc
            .get_mut("doors")
            .and_then(|i| i.as_array_of_tables_mut())
        else {
            return false;
        };
        if index >= arr.len() {
            return false;
        }
        arr.remove(index);
        // No cleanup needed when the last one goes: an empty `ArrayOfTables`
        // serializes to nothing, so the `[[doors]]` header disappears on its
        // own. (I had a `doc.remove("doors")` here until a mutation test showed
        // the output was identical without it — a line that looks defensive but
        // does nothing is worse than no line.)
        true
    }

    /// Move a door one place up or down. Menu order is what callers see, so it's
    /// worth being able to change without deleting and re-adding.
    pub fn door_move(&mut self, index: usize, up: bool) -> Option<usize> {
        let arr = self
            .doc
            .get_mut("doors")
            .and_then(|i| i.as_array_of_tables_mut())?;
        let target = if up {
            index.checked_sub(1)?
        } else {
            let t = index + 1;
            (t < arr.len()).then_some(t)?
        };
        // `ArrayOfTables` has no swap, so rebuild in the new order. Each table
        // moves whole, keeping its own comments.
        let tables: Vec<toml_edit::Table> = arr.iter().cloned().collect();
        let mut reordered = tables;
        reordered.swap(index, target);
        let mut fresh = toml_edit::ArrayOfTables::new();
        for t in reordered {
            fresh.push(t);
        }
        *arr = fresh;
        Some(target)
    }

    /// Which `[sections]` differ from the loaded file.
    pub fn changed_sections(&self) -> Vec<&'static str> {
        let before: DocumentMut = match self.original.parse() {
            Ok(d) => d,
            Err(_) => return schema::SECTIONS.iter().map(|s| s.name).collect(),
        };
        schema::SECTIONS
            .iter()
            .filter(|s| {
                let a = before.get(s.name).map(|i| i.to_string());
                let b = self.doc.get(s.name).map(|i| i.to_string());
                a != b
            })
            .map(|s| s.name)
            .collect()
    }

    /// Changed sections that only take effect after a restart.
    pub fn restart_needed(&self) -> Vec<&'static str> {
        self.changed_sections()
            .into_iter()
            .filter(|name| schema::section(name).is_some_and(|s| s.restart_only))
            .collect()
    }

    /// Check the configuration the way startup would, plus the cross-section
    /// constraints that are real but invisible in the file.
    ///
    /// Reuses the **existing** validators rather than restating their rules:
    /// the whole document is parsed as [`crate::config::Settings`], and the
    /// federation origin goes through `Origin::from_config` — the same
    /// fail-closed check the server runs. A second implementation would drift
    /// from the first and give an operator confident-sounding wrong answers.
    pub fn validate(&self) -> Vec<Issue> {
        let mut issues = Vec::new();

        let settings: crate::config::Settings = match toml::from_str(&self.doc.to_string()) {
            Ok(s) => s,
            Err(e) => {
                issues.push(Issue {
                    section: String::new(),
                    message: format!("config does not parse: {e}"),
                    blocking: true,
                });
                return issues;
            }
        };

        if settings.federation.enabled {
            if let Err(e) = crate::services::federation::Origin::from_config(&settings.federation) {
                issues.push(Issue {
                    section: "federation".into(),
                    message: format!("origin is not usable: {e}"),
                    blocking: true,
                });
            }
            if !settings.web.enabled {
                issues.push(Issue {
                    section: "federation".into(),
                    message: "federation needs [web] enabled — every ActivityPub endpoint \
                              (WebFinger, actors, inbox) is served by the web frontend"
                        .into(),
                    blocking: true,
                });
            } else if settings.web.port != 443 && settings.web.acme_domains.is_empty() {
                issues.push(Issue {
                    section: "federation".into(),
                    message: format!(
                        "[web] port is {} — acct: URIs have no port component, so peers must \
                         reach you on 443 (set port 443, or put a reverse proxy in front)",
                        settings.web.port
                    ),
                    blocking: false,
                });
            }
            if settings.federation.debug_insecure {
                issues.push(Issue {
                    section: "federation".into(),
                    message: "debug_insecure is on — this permits http/localhost origins and \
                              must never be set on a real board"
                        .into(),
                    blocking: false,
                });
            }
        }

        if settings.web.enabled && settings.web.port < 1024 {
            issues.push(Issue {
                section: "web".into(),
                message: format!(
                    "binding port {} needs privilege (a service manager, or setcap)",
                    settings.web.port
                ),
                blocking: false,
            });
        }
        if settings.network.port < 1024 {
            issues.push(Issue {
                section: "network".into(),
                message: format!("binding SSH port {} needs privilege", settings.network.port),
                blocking: false,
            });
        }
        if !settings.features.registration && !settings.features.guest {
            issues.push(Issue {
                section: "features".into(),
                message: "registration and guest are both off — nobody new can get in".into(),
                blocking: false,
            });
        }
        issues
    }

    /// Write the file back.
    ///
    /// Backs up the previous contents to `<path>.bak` and writes through a
    /// temporary file in the same directory, renamed into place. The operator
    /// may have hand-edited this file for years; a half-written config after a
    /// crash, or a lost original, is not an acceptable way to find that out.
    pub fn save(&mut self) -> Result<()> {
        let text = self.doc.to_string();
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));

        if self.path.exists() {
            let backup = self.path.with_extension("toml.bak");
            std::fs::copy(&self.path, &backup)
                .with_context(|| format!("backing up to {}", backup.display()))?;
        }

        let tmp = dir.join(format!(
            ".{}.tmp",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("bbs.toml")
        ));
        std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;

        self.original = text;
        Ok(())
    }
}

/// The value the shipped default config gives a setting, used when the key is
/// absent from the operator's file.
fn default_of(section: &str, key: &str) -> Option<FieldValue> {
    use std::sync::OnceLock;
    static DEFAULTS: OnceLock<DocumentMut> = OnceLock::new();
    let doc = DEFAULTS.get_or_init(|| {
        crate::config::DEFAULT_CONFIG_TOML
            .parse()
            .expect("the shipped default config must parse")
    });
    let item = doc.get(section)?.get(key)?;
    let kind = schema::section(section)
        .and_then(|s| s.field(key))
        .map(|f| f.kind);
    Some(match item {
        Item::Value(Value::Boolean(b)) => FieldValue::Bool(*b.value()),
        Item::Value(Value::Integer(i)) => FieldValue::Int(*i.value()),
        Item::Value(Value::Array(a)) => FieldValue::List(
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        ),
        Item::Value(Value::String(s)) => FieldValue::Str(s.value().clone()),
        _ => match kind {
            Some(FieldKind::Bool) => FieldValue::Bool(false),
            Some(FieldKind::Int { .. }) => FieldValue::Int(0),
            Some(FieldKind::StrList) => FieldValue::List(Vec::new()),
            _ => FieldValue::Str(String::new()),
        },
    })
}
