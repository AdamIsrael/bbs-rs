//! Editing `bbs.toml` without destroying it (#141).
//!
//! This is the model behind `bbscfg`, kept out of the binary so it can be
//! tested directly.
//!
//! **The file is mostly documentation.** 125 of `bbs.toml`'s ~200 lines are
//! comments, and around 25 settings ship deliberately commented out as
//! discoverable defaults. Serializing [`crate::config::Settings`] back to TOML
//! would produce a valid file that had lost all of it — a bare list of
//! key-values where a self-documenting config used to be.
//!
//! So edits are made **in place** with `toml_edit`: we mutate the values the
//! operator changed and leave every byte of everything else alone. Two
//! properties follow, and both are load-bearing:
//!
//! 1. Comments, ordering, blank lines, and commented-out examples survive.
//! 2. **Anything this tool doesn't model is preserved untouched.** `[[doors]]`,
//!    `[art.screens]`, and `[seed] boards` are shapes the field editor doesn't
//!    handle (arrays of tables and nested tables); because we never rewrite the
//!    document wholesale, not understanding them is harmless rather than
//!    destructive.

pub mod doc;
pub mod schema;

pub use doc::{ConfigDoc, FieldValue, Issue};
pub use schema::{Field, FieldKind, SECTIONS, Section};
