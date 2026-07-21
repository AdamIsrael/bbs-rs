//! A tiny, safe template engine (#89) for operator-authored text — the
//! MOTD/welcome banner, bulletins, and menu labels — rendered against a
//! per-session [`Context`] (transport, user, unread counts, bbs name, date…).
//!
//! Deliberately minimal: variable substitution and boolean conditionals, with
//! **no** expression evaluation, arithmetic, loops, or filesystem/environment
//! access. An operator template can therefore never do more than print the
//! context values we hand it — safe on untrusted operator input by
//! construction. Bytes it doesn't recognise (ANSI escapes, UTF-8, CP437) pass
//! through verbatim, so it composes with the `[theme]`/`[art]` work.
//!
//! Syntax:
//! - `{{ name }}` — substitute a variable; an unknown name renders empty.
//! - `{{#if name}}…{{/if}}`, optionally with `{{else}}`.
//! - `{{#unless name}}…{{/unless}}`, optionally with `{{else}}`.
//!
//! Conditionals nest, and whitespace inside the braces is ignored. A value is
//! truthy when it's a `true` bool, a non-zero int, or a non-empty string other
//! than `"0"`/`"false"` — so `{{#if unread_mail}}` works on the count directly.
//! Malformed or unterminated tags are left in the output verbatim rather than
//! erroring, so a typo degrades to visible text instead of a broken session.

use std::collections::HashMap;

/// A value bound to a template variable.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Bool(bool),
}

impl Value {
    /// How the value prints for `{{ name }}` substitution. Bools substitute as
    /// empty (they exist for conditionals), so `{{#if web}}` is the idiom, not
    /// `{{web}}`.
    fn display(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Int(n) => n.to_string(),
            Value::Bool(_) => String::new(),
        }
    }

    /// Whether the value counts as true in a conditional.
    fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Str(s) => !s.is_empty() && s != "0" && !s.eq_ignore_ascii_case("false"),
        }
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.to_string())
    }
}
impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Int(n)
    }
}
impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Value::Int(n as i64)
    }
}
impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

/// The variables a template renders against. Build with [`Context::new`] and
/// chain [`Context::set`].
#[derive(Debug, Default, Clone)]
pub struct Context {
    vars: HashMap<String, Value>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `name` to `value` (builder-style).
    pub fn set(mut self, name: &str, value: impl Into<Value>) -> Self {
        self.vars.insert(name.to_string(), value.into());
        self
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.vars.get(name)
    }

    /// Whether `name` is bound to a truthy value. Public so art-variant
    /// selection (#90) can test a `when` flag against the same context.
    pub fn truthy(&self, name: &str) -> bool {
        self.get(name).map(Value::truthy).unwrap_or(false)
    }
}

/// A parsed piece of the template.
#[derive(Debug, PartialEq)]
enum Token<'a> {
    /// Literal text (including anything that didn't parse as a tag).
    Text(&'a str),
    Var(&'a str),
    If(&'a str),
    Unless(&'a str),
    Else,
    EndIf,
    EndUnless,
}

/// Where a recursive render call stopped.
#[derive(Debug, PartialEq)]
enum Stop {
    Eof,
    Else,
    End,
}

/// Render `template` against `ctx`. Never panics or errors; malformed input is
/// preserved verbatim. Fast-pathed to a borrow-free copy when there's no tag.
pub fn render(template: &str, ctx: &Context) -> String {
    if !template.contains("{{") {
        return template.to_string();
    }
    let tokens = tokenize(template);
    let mut out = String::with_capacity(template.len());
    let mut pos = 0;
    // Loop so a stray terminator/else at the top level is skipped, not fatal.
    while pos < tokens.len() {
        if exec(&tokens, &mut pos, ctx, &mut out, true) == Stop::Eof {
            break;
        }
    }
    out
}

/// Split the template into literal text and `{{…}}` tags. An unterminated `{{`
/// (no closing `}}`) becomes literal text so nothing is silently dropped.
fn tokenize(template: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        let (before, after_open) = (&rest[..open], &rest[open + 2..]);
        if !before.is_empty() {
            tokens.push(Token::Text(before));
        }
        match after_open.find("}}") {
            Some(close) => {
                let inner = after_open[..close].trim();
                tokens.push(parse_tag(inner));
                rest = &after_open[close + 2..];
            }
            None => {
                // No closing braces: emit the "{{" and the remainder literally.
                tokens.push(Token::Text(&rest[open..]));
                rest = "";
                break;
            }
        }
    }
    if !rest.is_empty() {
        tokens.push(Token::Text(rest));
    }
    tokens
}

/// Classify the trimmed contents of one `{{…}}`. Unrecognised block tags
/// (a `#`/`/` prefix that isn't a known keyword) fall through to [`Token::Var`],
/// which renders empty — a forgiving degradation for typos.
fn parse_tag(inner: &str) -> Token<'_> {
    if let Some(name) = inner.strip_prefix("#if ") {
        Token::If(name.trim())
    } else if let Some(name) = inner.strip_prefix("#unless ") {
        Token::Unless(name.trim())
    } else if inner == "else" {
        Token::Else
    } else if inner == "/if" {
        Token::EndIf
    } else if inner == "/unless" {
        Token::EndUnless
    } else {
        Token::Var(inner)
    }
}

/// Render tokens from `*pos`, emitting into `out` only when `emit` is set,
/// until a terminator (`else`/`/if`/`/unless`) or end of input. Returns what
/// stopped it. Runs even when `emit` is false so a skipped branch still
/// consumes its matching terminators (keeping nesting balanced).
fn exec(tokens: &[Token], pos: &mut usize, ctx: &Context, out: &mut String, emit: bool) -> Stop {
    while *pos < tokens.len() {
        let token = &tokens[*pos];
        *pos += 1;
        match token {
            Token::Text(t) => {
                if emit {
                    out.push_str(t);
                }
            }
            Token::Var(name) => {
                if emit && let Some(v) = ctx.get(name) {
                    out.push_str(&v.display());
                }
            }
            Token::If(name) | Token::Unless(name) => {
                let mut cond = ctx.truthy(name);
                if matches!(token, Token::Unless(_)) {
                    cond = !cond;
                }
                // First (taken-if-true) branch, then an optional else branch.
                let stop = exec(tokens, pos, ctx, out, emit && cond);
                if stop == Stop::Else {
                    exec(tokens, pos, ctx, out, emit && !cond);
                }
            }
            Token::Else => return Stop::Else,
            Token::EndIf | Token::EndUnless => return Stop::End,
        }
    }
    Stop::Eof
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Context {
        Context::new()
            .set("bbs_name", "Test BBS")
            .set("user", "alice")
            .set("web", true)
            .set("ssh", false)
            .set("unread_mail", 3i64)
            .set("who_online", 0usize)
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let c = ctx();
        assert_eq!(render("hello world", &c), "hello world");
        // ANSI escapes and other bytes are preserved verbatim.
        assert_eq!(render("\x1b[31mred\x1b[0m", &c), "\x1b[31mred\x1b[0m");
    }

    #[test]
    fn variables_substitute() {
        let c = ctx();
        assert_eq!(
            render("Hi {{user}} @ {{bbs_name}}", &c),
            "Hi alice @ Test BBS"
        );
        // Whitespace inside braces is ignored.
        assert_eq!(render("{{  user  }}", &c), "alice");
        // Ints render as digits; unknown vars render empty.
        assert_eq!(render("{{unread_mail}} new, {{nope}}!", &c), "3 new, !");
    }

    #[test]
    fn if_and_else_branch_on_truthiness() {
        let c = ctx();
        assert_eq!(
            render("{{#if web}}on web{{else}}on ssh{{/if}}", &c),
            "on web"
        );
        assert_eq!(
            render("{{#if ssh}}on ssh{{else}}elsewhere{{/if}}", &c),
            "elsewhere"
        );
        // A count is truthy when non-zero; the else fires at zero.
        assert_eq!(
            render("{{#if unread_mail}}you have mail{{/if}}", &c),
            "you have mail"
        );
        assert_eq!(
            render(
                "{{#if who_online}}{{who_online}} online{{else}}nobody{{/if}}",
                &c
            ),
            "nobody"
        );
    }

    #[test]
    fn unless_is_the_inverse() {
        let c = ctx();
        assert_eq!(render("{{#unless ssh}}not ssh{{/unless}}", &c), "not ssh");
        assert_eq!(render("{{#unless web}}not web{{/unless}}", &c), "");
    }

    #[test]
    fn conditionals_nest() {
        let c = ctx();
        let t = "{{#if web}}web{{#if unread_mail}} + {{unread_mail}} mail{{/if}}{{/if}}";
        assert_eq!(render(t, &c), "web + 3 mail");
        // The nested block is skipped wholesale when the outer condition is false.
        let t2 = "{{#if ssh}}ssh{{#if unread_mail}} mail{{/if}}{{/if}}done";
        assert_eq!(render(t2, &c), "done");
    }

    #[test]
    fn malformed_tags_degrade_to_literal_text() {
        let c = ctx();
        // Unterminated tag: preserved verbatim.
        assert_eq!(render("a {{user", &c), "a {{user");
        // Unknown block keyword falls through to an (empty) variable lookup.
        assert_eq!(render("x{{#foo bar}}y", &c), "xy");
        // A stray terminator at top level is skipped, not fatal.
        assert_eq!(render("keep{{/if}}going", &c), "keepgoing");
    }

    #[test]
    fn string_truthiness_rules() {
        let c = Context::new()
            .set("empty", "")
            .set("zero", "0")
            .set("false_str", "false")
            .set("name", "bob");
        assert_eq!(render("{{#if empty}}x{{/if}}", &c), "");
        assert_eq!(render("{{#if zero}}x{{/if}}", &c), "");
        assert_eq!(render("{{#if false_str}}x{{/if}}", &c), "");
        assert_eq!(render("{{#if name}}x{{/if}}", &c), "x");
    }
}
