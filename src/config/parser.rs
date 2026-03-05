// config/parser.rs — hand-written recursive-descent parser for trixie.conf
//
// ── Format ────────────────────────────────────────────────────────────────────
//
//   file       = item*
//   item       = source_directive | var_def | block | assignment
//   source_directive = "source" "=" string
//   var_def    = "$" ident "=" value
//   block      = ident label? "{" item* "}"
//   label      = string | ident
//   assignment = ident "=" value ("," value)*
//   value      = string | color | dimension | rgb_color | ident | array
//              | var_ref | number
//   var_ref    = "$" ident
//   array      = "[" (value ("," value)*)? "]"
//   color      = "#" hex{6,8}
//   rgb_color  = "rgb(" hex{6} ")" | "rgba(" hex{8} ")"
//   dimension  = number unit        (unit = "px" | "hz" | "%" | "ms")
//   string     = '"' [^"]* '"'
//   comment    = "//" [^\n]*
//
// ── Variable scoping ──────────────────────────────────────────────────────────
//
//   Variables ($name) are collected in a first pass over each file, then
//   substituted during value parsing. This allows forward-references within
//   a single file. Undefined variables produce a ParseError.
//
// ── Source directives ─────────────────────────────────────────────────────────
//
//   `source = "path"` is resolved at parse time. The sourced file's items are
//   inlined at the point of the directive, fully transparent to Config::from_file.
//   Paths starting with ~/ are expanded using $HOME. Cycles are detected and
//   produce a ParseError.
//
// ── Keybind syntax ────────────────────────────────────────────────────────────
//
//   keybind = SUPER+SHIFT:Return, exec, kitty
//   The combo token (everything before the first comma) is a single Ident that
//   includes '+' and ':' characters. The entire RHS is parsed as an unbracketed
//   comma-separated array of Ident/String values.
//
// ── Unknown blocks/keys ───────────────────────────────────────────────────────
//
//   Unknown blocks and keys are silently accepted by the parser; Config::from_file
//   emits tracing::warn for unrecognised keys. This lets future config keys
//   (animations {}, general {}) round-trip without errors.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

// ── Span ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(start: usize, end: usize, line: u32, col: u32) -> Self {
        Self {
            start,
            end,
            line,
            col,
        }
    }
    pub fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            line: self.line.min(other.line),
            col: self.col,
        }
    }
}

// ── Value ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Ident(String),
    Int(i64),
    Float(f64),
    Dimension(f64, Unit),
    Color(u8, u8, u8, u8), // RGBA
    Array(Vec<Spanned<Value>>),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Px,
    Hz,
    Percent,
    Ms,
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Px => write!(f, "px"),
            Self::Hz => write!(f, "hz"),
            Self::Percent => write!(f, "%"),
            Self::Ms => write!(f, "ms"),
        }
    }
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) | Self::Ident(s) => Some(s.as_str()),
            _ => None,
        }
    }
    /// Like as_str but also converts Int/Float to a temporary — use for
    /// keybind arg parsing where numeric values appear as bare integers.
    pub fn as_arg_string(&self) -> Option<std::borrow::Cow<str>> {
        match self {
            Self::String(s) | Self::Ident(s) => Some(std::borrow::Cow::Borrowed(s.as_str())),
            Self::Int(n) => Some(std::borrow::Cow::Owned(n.to_string())),
            Self::Float(f) => Some(std::borrow::Cow::Owned(f.to_string())),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Float(f) => Some(*f as i64),
            Self::Dimension(f, _) => Some(*f as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Float(f) | Self::Dimension(f, _) => Some(*f),
            Self::Int(n) => Some(*n as f64),
            _ => None,
        }
    }
    pub fn as_px(&self) -> Option<u32> {
        match self {
            Self::Dimension(v, Unit::Px) => Some(*v as u32),
            Self::Int(n) => Some(*n as u32),
            _ => None,
        }
    }
    pub fn as_hz(&self) -> Option<u32> {
        match self {
            Self::Dimension(v, Unit::Hz) => Some(*v as u32),
            Self::Int(n) => Some(*n as u32),
            _ => None,
        }
    }
    pub fn as_color(&self) -> Option<[u8; 4]> {
        if let Self::Color(r, g, b, a) = self {
            Some([*r, *g, *b, *a])
        } else {
            None
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            Self::Ident(s) => match s.as_str() {
                "true" | "yes" | "on" => Some(true),
                "false" | "no" | "off" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Spanned<Value>]> {
        if let Self::Array(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_f32(&self) -> Option<f32> {
        self.as_f64().map(|f| f as f32)
    }
}

// ── Spanned<T> ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }
}

// ── AST nodes ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Assignment {
    pub key: Spanned<String>,
    pub value: Spanned<Value>,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub name: Spanned<String>,
    pub label: Option<Spanned<String>>,
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Item {
    Assignment(Assignment),
    Block(Block),
}

#[derive(Debug, Clone, Default)]
pub struct ConfigFile {
    pub items: Vec<Item>,
}

impl ConfigFile {
    pub fn get_all(&self, key: &str) -> impl Iterator<Item = &Spanned<Value>> {
        let key = key.to_owned();
        self.items.iter().filter_map(move |i| {
            if let Item::Assignment(a) = i {
                if a.key.value == key {
                    return Some(&a.value);
                }
            }
            None
        })
    }
    pub fn get(&self, key: &str) -> Option<&Spanned<Value>> {
        self.get_all(key).next()
    }
    pub fn get_last(&self, key: &str) -> Option<&Spanned<Value>> {
        self.get_all(key).last()
    }
    pub fn blocks(&self, name: &str) -> impl Iterator<Item = &Block> {
        let name = name.to_owned();
        self.items.iter().filter_map(move |i| {
            if let Item::Block(b) = i {
                if b.name.value == name {
                    return Some(b);
                }
            }
            None
        })
    }
    pub fn block(&self, name: &str) -> Option<&Block> {
        self.blocks(name).next()
    }
    pub fn block_last(&self, name: &str) -> Option<&Block> {
        self.blocks(name).last()
    }
}

impl Block {
    pub fn get(&self, key: &str) -> Option<&Spanned<Value>> {
        self.get_last(key)
    }
    /// Last wins — matches Hyprland semantics where later assignments override.
    pub fn get_last(&self, key: &str) -> Option<&Spanned<Value>> {
        self.items.iter().rev().find_map(|i| {
            if let Item::Assignment(a) = i {
                if a.key.value == key {
                    return Some(&a.value);
                }
            }
            None
        })
    }
    pub fn get_all(&self, key: &str) -> impl Iterator<Item = &Spanned<Value>> {
        let key = key.to_owned();
        self.items.iter().filter_map(move |i| {
            if let Item::Assignment(a) = i {
                if a.key.value == key {
                    return Some(&a.value);
                }
            }
            None
        })
    }
    pub fn blocks(&self, name: &str) -> impl Iterator<Item = &Block> {
        let name = name.to_owned();
        self.items.iter().filter_map(move |i| {
            if let Item::Block(b) = i {
                if b.name.value == name {
                    return Some(b);
                }
            }
            None
        })
    }
    /// All known keys in this block, for unknown-key warnings.
    pub fn assignment_keys(&self) -> impl Iterator<Item = &Spanned<String>> {
        self.items.iter().filter_map(|i| {
            if let Item::Assignment(a) = i {
                Some(&a.key)
            } else {
                None
            }
        })
    }
}

// ── ParseError ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

// ── ParseResult ───────────────────────────────────────────────────────────────

pub struct ParseResult {
    pub file: ConfigFile,
    pub errors: Vec<ParseError>,
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Parse `src` in isolation (no source-directive resolution, no file I/O).
/// Used by the LSP server and for unit tests.
pub fn parse(src: &str) -> ParseResult {
    parse_with_context(src, None, &mut HashSet::new())
}

/// Parse `path`, resolving `source =` directives relative to the same
/// directory. This is the entry point for the compositor.
pub fn parse_file(path: &Path) -> ParseResult {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return ParseResult {
                file: ConfigFile::default(),
                errors: vec![ParseError {
                    message: format!("cannot read {:?}: {e}", path),
                    span: Span::default(),
                    line: 1,
                    col: 1,
                }],
            };
        }
    };
    let mut visited = HashSet::new();
    visited.insert(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
    parse_with_context(&src, Some(path), &mut visited)
}

// ── Internal parse entry ──────────────────────────────────────────────────────

fn parse_with_context(
    src: &str,
    file_path: Option<&Path>,
    visited: &mut HashSet<PathBuf>,
) -> ParseResult {
    // Pass 1: collect $variable definitions for forward-reference support.
    let vars = collect_vars(src);

    // Pass 2: full parse.
    let mut parser = Parser::new(src, vars, file_path, visited);
    let file = parser.parse_file();
    ParseResult {
        file,
        errors: parser.errors,
    }
}

// ── Variable pre-collection ───────────────────────────────────────────────────

fn collect_vars(src: &str) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    for line in src.lines() {
        let t = line.trim();
        // Strip inline comment
        let t = strip_inline_comment(t);
        if !t.starts_with('$') {
            continue;
        }
        let t = &t[1..];
        let Some(eq) = t.find('=') else { continue };
        let name = t[..eq].trim().to_string();
        if name.is_empty() || name.contains(|c: char| c.is_whitespace()) {
            continue;
        }
        let rhs = t[eq + 1..].trim();
        if let Some(v) = parse_inline_value(rhs) {
            map.insert(name, v);
        }
    }
    map
}

fn strip_inline_comment(s: &str) -> &str {
    // Only strip `//` that isn't inside a string.
    let mut in_str = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => in_str = !in_str,
            b'/' if !in_str && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return s[..i].trim_end();
            }
            _ => {}
        }
        i += 1;
    }
    s
}

fn parse_inline_value(s: &str) -> Option<Value> {
    let s = strip_inline_comment(s).trim();
    if let Some(v) = try_parse_rgb(s) {
        return Some(v);
    }
    if s.starts_with('#') {
        return parse_hex_color(&s[1..]);
    }
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        return Some(Value::String(s[1..s.len() - 1].to_string()));
    }
    if let Some(v) = try_parse_dimension(s) {
        return Some(v);
    }
    match s {
        "true" | "yes" | "on" => return Some(Value::Bool(true)),
        "false" | "no" | "off" => return Some(Value::Bool(false)),
        _ => {}
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(Value::Int(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Value::Float(f));
    }
    if s.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
        && !s.is_empty()
    {
        return Some(Value::Ident(s.to_string()));
    }
    None
}

fn try_parse_rgb(s: &str) -> Option<Value> {
    let s = s.trim();
    let (hex, alpha) = if s.starts_with("rgba(") && s.ends_with(')') {
        (&s[5..s.len() - 1], true)
    } else if s.starts_with("rgb(") && s.ends_with(')') {
        (&s[4..s.len() - 1], false)
    } else {
        return None;
    };
    let hex = hex.trim();
    if alpha {
        if hex.len() != 8 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(Value::Color(
            (v >> 24) as u8,
            (v >> 16) as u8,
            (v >> 8) as u8,
            v as u8,
        ))
    } else {
        if hex.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        Some(Value::Color((v >> 16) as u8, (v >> 8) as u8, v as u8, 255))
    }
}

fn parse_hex_color(hex: &str) -> Option<Value> {
    match hex.len() {
        6 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            Some(Value::Color((v >> 16) as u8, (v >> 8) as u8, v as u8, 255))
        }
        8 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            Some(Value::Color(
                (v >> 24) as u8,
                (v >> 16) as u8,
                (v >> 8) as u8,
                v as u8,
            ))
        }
        _ => None,
    }
}

fn try_parse_dimension(s: &str) -> Option<Value> {
    let (num_part, unit) = if s.ends_with("px") {
        (&s[..s.len() - 2], Unit::Px)
    } else if s.ends_with("ms") {
        (&s[..s.len() - 2], Unit::Ms)
    } else if s.ends_with("hz") || s.ends_with("Hz") {
        (&s[..s.len() - 2], Unit::Hz)
    } else if s.ends_with('%') {
        (&s[..s.len() - 1], Unit::Percent)
    } else {
        return None;
    };
    let v: f64 = num_part.parse().ok()?;
    Some(Value::Dimension(v, unit))
}

// ── Lexer ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    VarRef(String),
    VarDef(String),
    StringLit(String),
    Int(i64),
    Float(f64),
    Color(u8, u8, u8, u8),
    Dimension(f64, Unit),
    Eq,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Eof,
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    pub line: u32,
    pub col: u32,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.src[self.pos..].chars().next()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(ch)
    }

    fn skip_ws_and_comments(&mut self) {
        loop {
            while self.peek_char().map_or(false, |c| c.is_ascii_whitespace()) {
                self.advance();
            }
            if self.src[self.pos..].starts_with("//") || self.src[self.pos..].starts_with('#') {
                while self.peek_char().map_or(false, |c| c != '\n') {
                    self.advance();
                }
            } else {
                break;
            }
        }
    }

    fn next_tok(&mut self) -> Result<Spanned<Tok>, ParseError> {
        self.skip_ws_and_comments();
        let start = self.pos;
        let line = self.line;
        let col = self.col;

        macro_rules! span {
            () => {
                Span::new(start, self.pos, line, col)
            };
        }
        macro_rules! err {
            ($msg:expr) => {
                return Err(ParseError {
                    message: $msg.to_string(),
                    span: span!(),
                    line,
                    col,
                })
            };
        }

        let Some(ch) = self.peek_char() else {
            return Ok(Spanned::new(Tok::Eof, span!()));
        };

        match ch {
            '=' => {
                self.advance();
                Ok(Spanned::new(Tok::Eq, span!()))
            }
            '{' => {
                self.advance();
                Ok(Spanned::new(Tok::LBrace, span!()))
            }
            '}' => {
                self.advance();
                Ok(Spanned::new(Tok::RBrace, span!()))
            }
            '[' => {
                self.advance();
                Ok(Spanned::new(Tok::LBracket, span!()))
            }
            ']' => {
                self.advance();
                Ok(Spanned::new(Tok::RBracket, span!()))
            }
            ',' => {
                self.advance();
                Ok(Spanned::new(Tok::Comma, span!()))
            }

            '$' => {
                self.advance();
                let name_start = self.pos;
                while self
                    .peek_char()
                    .map_or(false, |c| c.is_alphanumeric() || c == '_')
                {
                    self.advance();
                }
                let name = self.src[name_start..self.pos].to_string();
                if name.is_empty() {
                    err!("expected variable name after '$'");
                }
                let after = self.src[self.pos..]
                    .trim_start_matches(|c: char| c.is_ascii_whitespace() && c != '\n');
                let tok = if after.starts_with('=') && !after.starts_with("==") {
                    Tok::VarDef(name)
                } else {
                    Tok::VarRef(name)
                };
                Ok(Spanned::new(tok, span!()))
            }

            '"' => {
                self.advance();
                let mut s = String::new();
                loop {
                    match self.advance() {
                        None => err!("unterminated string"),
                        Some('"') => break,
                        Some('\\') => match self.advance() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(c) => s.push(c),
                            None => err!("unterminated escape"),
                        },
                        Some(c) => s.push(c),
                    }
                }
                Ok(Spanned::new(Tok::StringLit(s), span!()))
            }

            'r' if self.src[self.pos..].starts_with("rgb(")
                || self.src[self.pos..].starts_with("rgba(") =>
            {
                let is_rgba = self.src[self.pos..].starts_with("rgba(");
                for _ in 0..if is_rgba { 5 } else { 4 } {
                    self.advance();
                }
                let hex_start = self.pos;
                while self.peek_char().map_or(false, |c| c.is_ascii_hexdigit()) {
                    self.advance();
                }
                let hex = &self.src[hex_start..self.pos];
                if self.peek_char() == Some(')') {
                    self.advance();
                } else {
                    err!("expected ')' after rgb/rgba color");
                }
                let expected = if is_rgba { 8 } else { 6 };
                if hex.len() != expected {
                    err!(format!(
                        "rgb color must be {expected} hex digits, got {}",
                        hex.len()
                    ));
                }
                let v = u32::from_str_radix(hex, 16).map_err(|_| ParseError {
                    message: "invalid hex in rgb()".into(),
                    span: span!(),
                    line,
                    col,
                })?;
                let tok = if is_rgba {
                    Tok::Color((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)
                } else {
                    Tok::Color((v >> 16) as u8, (v >> 8) as u8, v as u8, 255)
                };
                Ok(Spanned::new(tok, span!()))
            }

            '#' => {
                self.advance();
                let hex_start = self.pos;
                while self.peek_char().map_or(false, |c| c.is_ascii_hexdigit()) {
                    self.advance();
                }
                let hex = &self.src[hex_start..self.pos];
                match hex.len() {
                    6 => {
                        let v = u32::from_str_radix(hex, 16).map_err(|_| ParseError {
                            message: "invalid hex color".into(),
                            span: span!(),
                            line,
                            col,
                        })?;
                        Ok(Spanned::new(
                            Tok::Color((v >> 16) as u8, (v >> 8) as u8, v as u8, 255),
                            span!(),
                        ))
                    }
                    8 => {
                        let v = u32::from_str_radix(hex, 16).map_err(|_| ParseError {
                            message: "invalid hex color".into(),
                            span: span!(),
                            line,
                            col,
                        })?;
                        Ok(Spanned::new(
                            Tok::Color((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8),
                            span!(),
                        ))
                    }
                    n => err!(format!(
                        "color must be #rrggbb or #rrggbbaa, got {n} hex digits"
                    )),
                }
            }

            // Bare '-' that isn't a negative number — treat as start of an ident
            // so tokens like `set-volume`, `wl-paste`, `-show`, `-dmenu` work.
            '-' => {
                while self.peek_char().map_or(false, |c| {
                    c.is_alphanumeric()
                        || c == '_'
                        || c == '-'
                        || c == '+'
                        || c == ':'
                        || c == '.'
                        || c == '@'
                        || c == '/'
                }) {
                    self.advance();
                }
                let s = self.src[start..self.pos].to_string();
                Ok(Spanned::new(Tok::Ident(s), span!()))
            }

            c if c.is_ascii_digit()
                || (c == '-'
                    && self.src[self.pos + 1..].starts_with(|x: char| x.is_ascii_digit())) =>
            {
                let neg = c == '-';
                if neg {
                    self.advance();
                }
                while self.peek_char().map_or(false, |c| c.is_ascii_digit()) {
                    self.advance();
                }
                let is_float = self.peek_char() == Some('.');
                if is_float {
                    self.advance();
                    while self.peek_char().map_or(false, |c| c.is_ascii_digit()) {
                        self.advance();
                    }
                }
                let num_str = &self.src[start..self.pos];
                let unit = if self.src[self.pos..].starts_with("px") {
                    self.pos += 2;
                    self.col += 2;
                    Some(Unit::Px)
                } else if self.src[self.pos..].starts_with("ms") {
                    self.pos += 2;
                    self.col += 2;
                    Some(Unit::Ms)
                } else if self.src[self.pos..].starts_with("hz")
                    || self.src[self.pos..].starts_with("Hz")
                {
                    self.pos += 2;
                    self.col += 2;
                    Some(Unit::Hz)
                } else if self.peek_char() == Some('%') {
                    self.advance();
                    Some(Unit::Percent)
                } else {
                    None
                };
                let sp = span!();
                if let Some(u) = unit {
                    let v: f64 = num_str.parse().map_err(|_| ParseError {
                        message: format!("invalid number '{num_str}'"),
                        span: sp,
                        line,
                        col,
                    })?;
                    Ok(Spanned::new(Tok::Dimension(v, u), sp))
                } else if is_float {
                    let v: f64 = num_str.parse().map_err(|_| ParseError {
                        message: format!("invalid float '{num_str}'"),
                        span: sp,
                        line,
                        col,
                    })?;
                    Ok(Spanned::new(Tok::Float(v), sp))
                } else {
                    let raw = if neg { &num_str[1..] } else { num_str };
                    let v: i64 = raw.parse().map_err(|_| ParseError {
                        message: format!("invalid integer '{num_str}'"),
                        span: sp,
                        line,
                        col,
                    })?;
                    Ok(Spanned::new(Tok::Int(if neg { -v } else { v }), sp))
                }
            }

            c if c.is_alphabetic() || c == '_' => {
                while self.peek_char().map_or(false, |c| {
                    c.is_alphanumeric()
                        || c == '_'
                        || c == '-'
                        || c == ':'
                        || c == '.'
                        || c == '@'
                        || c == '+'
                        || c == '/'
                }) {
                    self.advance();
                }
                let s = self.src[start..self.pos].to_string();
                let sp = span!();
                let tok = match s.as_str() {
                    "true" | "yes" | "on" => Tok::Ident("true".into()),
                    "false" | "no" | "off" => Tok::Ident("false".into()),
                    _ => Tok::Ident(s),
                };
                Ok(Spanned::new(tok, sp))
            }

            '|' => {
                self.advance();
                Ok(Spanned::new(Tok::Ident("|".into()), span!()))
            }

            other => err!(format!("unexpected character '{other}'")),
        }
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

struct Parser<'a> {
    lexer: Lexer<'a>,
    current: Option<Spanned<Tok>>,
    pub errors: Vec<ParseError>,
    vars: HashMap<String, Value>,
    file_dir: Option<PathBuf>,
    visited: &'a mut HashSet<PathBuf>,
}

impl<'a> Parser<'a> {
    fn new(
        src: &'a str,
        vars: HashMap<String, Value>,
        file_path: Option<&Path>,
        visited: &'a mut HashSet<PathBuf>,
    ) -> Self {
        let file_dir = file_path.and_then(|p| p.parent()).map(|p| p.to_path_buf());
        let mut p = Self {
            lexer: Lexer::new(src),
            current: None,
            errors: Vec::new(),
            vars,
            file_dir,
            visited,
        };
        p.bump();
        p
    }

    fn bump(&mut self) {
        loop {
            let pos_before = self.lexer.pos;
            match self.lexer.next_tok() {
                Ok(t) => {
                    self.current = Some(t);
                    return;
                }
                Err(e) => {
                    self.errors.push(e);
                    // If the lexer didn't advance past the bad character,
                    // force it forward to prevent an infinite loop.
                    if self.lexer.pos == pos_before {
                        self.lexer.advance();
                    }
                }
            }
        }
    }

    fn peek(&self) -> &Tok {
        self.current.as_ref().map(|s| &s.value).unwrap_or(&Tok::Eof)
    }

    fn peek_span(&self) -> Span {
        self.current.as_ref().map(|s| s.span).unwrap_or_default()
    }

    fn expect(&mut self, expected: &Tok) -> Option<Span> {
        if self.peek() == expected {
            let sp = self.peek_span();
            self.bump();
            Some(sp)
        } else {
            let sp = self.peek_span();
            self.errors.push(ParseError {
                message: format!("expected {:?}, got {:?}", expected, self.peek()),
                span: sp,
                line: sp.line,
                col: sp.col,
            });
            None
        }
    }

    fn eat_ident(&mut self) -> Option<Spanned<String>> {
        if let Tok::Ident(_) = self.peek() {
            if let Some(Spanned {
                value: Tok::Ident(s),
                span,
            }) = self.current.take()
            {
                self.bump();
                return Some(Spanned::new(s, span));
            }
        }
        None
    }

    fn resolve_source(&self, raw: &str) -> PathBuf {
        let expanded = if raw.starts_with("~/") {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            format!("{}/{}", home, &raw[2..])
        } else {
            raw.to_string()
        };
        let p = PathBuf::from(&expanded);
        if p.is_absolute() {
            p
        } else if let Some(dir) = &self.file_dir {
            dir.join(&p)
        } else {
            p
        }
    }

    // ── Top-level file parse ──────────────────────────────────────────────────

    pub fn parse_file(&mut self) -> ConfigFile {
        let mut items = Vec::new();
        while *self.peek() != Tok::Eof {
            if let Some(mut sourced) = self.try_parse_source() {
                items.append(&mut sourced);
            } else if self.try_skip_var_def() {
                // already collected in pass 1
            } else {
                match self.parse_item() {
                    Some(i) => items.push(i),
                    None => {
                        self.bump();
                    } // error recovery
                }
            }
        }
        ConfigFile { items }
    }

    fn try_parse_source(&mut self) -> Option<Vec<Item>> {
        let is_source = matches!(self.peek(), Tok::Ident(s) if s == "source");
        if !is_source {
            return None;
        }

        let span = self.peek_span();
        self.bump();

        if *self.peek() != Tok::Eq {
            self.errors.push(ParseError {
                message: "expected '=' after 'source'".into(),
                span,
                line: span.line,
                col: span.col,
            });
            return None;
        }
        self.bump();

        let path_str = match self.current.take() {
            Some(Spanned {
                value: Tok::StringLit(s),
                ..
            }) => {
                self.bump();
                s
            }
            other => {
                let sp = other.as_ref().map(|s| s.span).unwrap_or(span);
                self.errors.push(ParseError {
                    message: "source directive requires a quoted path string".into(),
                    span: sp,
                    line: sp.line,
                    col: sp.col,
                });
                self.current = other;
                self.bump();
                return Some(vec![]);
            }
        };

        let path = self.resolve_source(&path_str);
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());

        if self.visited.contains(&canonical) {
            self.errors.push(ParseError {
                message: format!("source cycle detected: {:?}", path),
                span,
                line: span.line,
                col: span.col,
            });
            return Some(vec![]);
        }
        self.visited.insert(canonical);

        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.errors.push(ParseError {
                    message: format!("cannot read source {:?}: {e}", path),
                    span,
                    line: span.line,
                    col: span.col,
                });
                return Some(vec![]);
            }
        };

        let child_vars = {
            let mut v = self.vars.clone();
            v.extend(collect_vars(&src));
            v
        };
        let mut child = Parser::new(&src, child_vars, Some(&path), self.visited);
        let child_file = child.parse_file();
        for mut e in child.errors {
            e.message = format!("[{path_str}] {}", e.message);
            self.errors.push(e);
        }
        Some(child_file.items)
    }

    fn try_skip_var_def(&mut self) -> bool {
        if !matches!(self.peek(), Tok::VarDef(_)) {
            return false;
        }
        self.bump(); // $name
        if *self.peek() == Tok::Eq {
            self.bump();
        }
        self.skip_value_rhs();
        true
    }

    fn skip_value_rhs(&mut self) {
        // Skip a single value, then only continue if there is a comma
        // (comma-separated array). This prevents consuming the first token
        // of the *next* statement (e.g. eating "bar" after "$green = rgb(...)").
        loop {
            match self.peek() {
                Tok::Eof | Tok::RBrace | Tok::LBrace => break,
                Tok::LBracket => {
                    self.bump();
                    while !matches!(self.peek(), Tok::RBracket | Tok::Eof) {
                        self.bump();
                    }
                    self.bump();
                    // After a bracketed array there can be no comma continuation.
                    break;
                }
                Tok::Ident(_)
                | Tok::VarRef(_)
                | Tok::StringLit(_)
                | Tok::Int(_)
                | Tok::Float(_)
                | Tok::Color(_, _, _, _)
                | Tok::Dimension(_, _) => {
                    self.bump();
                    // Only continue if a comma follows (multi-value array).
                    if *self.peek() == Tok::Comma {
                        self.bump(); // consume comma
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }

    // ── Item parse ────────────────────────────────────────────────────────────

    fn parse_item(&mut self) -> Option<Item> {
        let name = self.eat_ident()?;

        match self.peek() {
            Tok::Eq => {
                self.bump();
                let value = self.parse_value_or_array();
                Some(Item::Assignment(Assignment { key: name, value }))
            }
            Tok::LBrace => {
                self.bump();
                let items = self.parse_block_body();
                let end = self.expect(&Tok::RBrace).unwrap_or(self.peek_span());
                let span = name.span.merge(end);
                Some(Item::Block(Block {
                    name,
                    label: None,
                    items,
                    span,
                }))
            }
            Tok::StringLit(_) | Tok::Ident(_) => {
                // Labeled block: `bar_module clock { }` or `monitor eDP-1 { }`
                let label = match self.current.take() {
                    Some(Spanned {
                        value: Tok::StringLit(s),
                        span,
                    }) => {
                        self.bump();
                        Some(Spanned::new(s, span))
                    }
                    Some(Spanned {
                        value: Tok::Ident(s),
                        span,
                    }) => {
                        self.bump();
                        Some(Spanned::new(s, span))
                    }
                    other => {
                        self.current = other;
                        None
                    }
                };
                if *self.peek() == Tok::LBrace {
                    self.bump();
                    let items = self.parse_block_body();
                    let end = self.expect(&Tok::RBrace).unwrap_or(self.peek_span());
                    let span = name.span.merge(end);
                    Some(Item::Block(Block {
                        name,
                        label,
                        items,
                        span,
                    }))
                } else {
                    // Bare `key value` — treat as assignment with ident value
                    let v = label
                        .map(|l| Spanned::new(Value::Ident(l.value), l.span))
                        .unwrap_or_else(|| Spanned::new(Value::Bool(true), name.span));
                    Some(Item::Assignment(Assignment {
                        key: name,
                        value: v,
                    }))
                }
            }
            _ => {
                // Bare flag — boolean true
                let sp = name.span;
                Some(Item::Assignment(Assignment {
                    key: name,
                    value: Spanned::new(Value::Bool(true), sp),
                }))
            }
        }
    }

    fn parse_block_body(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                Tok::RBrace | Tok::Eof => break,
                _ => {
                    if let Some(mut sourced) = self.try_parse_source() {
                        items.append(&mut sourced);
                    } else if self.try_skip_var_def() {
                        // skip
                    } else {
                        match self.parse_item() {
                            Some(i) => items.push(i),
                            None => {
                                self.bump();
                            }
                        }
                    }
                }
            }
        }
        items
    }

    // ── Value parse ───────────────────────────────────────────────────────────

    fn parse_value_or_array(&mut self) -> Spanned<Value> {
        let first = match self.parse_single_value() {
            Some(v) => v,
            None => {
                let sp = self.peek_span();
                self.errors.push(ParseError {
                    message: "expected value after '='".into(),
                    span: sp,
                    line: sp.line,
                    col: sp.col,
                });
                return Spanned::new(Value::Ident(String::new()), sp);
            }
        };

        if *self.peek() != Tok::Comma {
            return first;
        }

        let start_span = first.span;
        let mut items = vec![first];
        while *self.peek() == Tok::Comma {
            self.bump();
            match self.peek() {
                Tok::RBrace | Tok::Eof | Tok::LBrace => break,
                _ => {}
            }
            match self.parse_single_value() {
                Some(v) => items.push(v),
                None => break,
            }
        }
        let end_span = items.last().map(|v| v.span).unwrap_or(start_span);
        Spanned::new(Value::Array(items), start_span.merge(end_span))
    }

    fn parse_single_value(&mut self) -> Option<Spanned<Value>> {
        let sp = self.peek_span();
        match self.current.take() {
            Some(Spanned {
                value: Tok::StringLit(s),
                span,
            }) => {
                self.bump();
                Some(Spanned::new(Value::String(s), span))
            }
            Some(Spanned {
                value: Tok::Ident(s),
                span,
            }) => {
                self.bump();
                let v = match s.as_str() {
                    "true" | "yes" | "on" => Value::Bool(true),
                    "false" | "no" | "off" => Value::Bool(false),
                    _ => Value::Ident(s),
                };
                Some(Spanned::new(v, span))
            }
            Some(Spanned {
                value: Tok::VarRef(name),
                span,
            }) => {
                self.bump();
                match self.vars.get(&name) {
                    Some(v) => Some(Spanned::new(v.clone(), span)),
                    None => {
                        self.errors.push(ParseError {
                            message: format!("undefined variable '${name}'"),
                            span,
                            line: span.line,
                            col: span.col,
                        });
                        Some(Spanned::new(Value::Ident(String::new()), span))
                    }
                }
            }
            Some(Spanned {
                value: Tok::Int(n),
                span,
            }) => {
                self.bump();
                Some(Spanned::new(Value::Int(n), span))
            }
            Some(Spanned {
                value: Tok::Float(f),
                span,
            }) => {
                self.bump();
                Some(Spanned::new(Value::Float(f), span))
            }
            Some(Spanned {
                value: Tok::Dimension(v, u),
                span,
            }) => {
                self.bump();
                Some(Spanned::new(Value::Dimension(v, u), span))
            }
            Some(Spanned {
                value: Tok::Color(r, g, b, a),
                span,
            }) => {
                self.bump();
                Some(Spanned::new(Value::Color(r, g, b, a), span))
            }
            Some(Spanned {
                value: Tok::LBracket,
                span,
            }) => {
                self.bump();
                let mut items = Vec::new();
                while !matches!(self.peek(), Tok::RBracket | Tok::Eof) {
                    if let Some(v) = self.parse_single_value() {
                        items.push(v);
                    }
                    if *self.peek() == Tok::Comma {
                        self.bump();
                    }
                }
                let end = self.expect(&Tok::RBracket).unwrap_or(sp);
                Some(Spanned::new(Value::Array(items), span.merge(end)))
            }
            other => {
                self.current = other;
                None
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_assignment() {
        let r = parse(r#"font = "/usr/share/fonts/Iosevka.ttf""#);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn hash_color() {
        let r = parse("active_border = #b4befe");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(
            r.file.get("active_border").unwrap().value.as_color(),
            Some([0xb4, 0xbe, 0xfe, 0xff])
        );
    }

    #[test]
    fn rgb_color() {
        let r = parse("pane_bg = rgb(11111b)");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(
            r.file.get("pane_bg").unwrap().value.as_color(),
            Some([0x11, 0x11, 0x1b, 0xff])
        );
    }

    #[test]
    fn rgba_color() {
        let r = parse("overlay = rgba(1e1e2e80)");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(
            r.file.get("overlay").unwrap().value.as_color(),
            Some([0x1e, 0x1e, 0x2e, 0x80])
        );
    }

    #[test]
    fn variable_substitution() {
        let r = parse("$accent = #b4befe\nactive_border = $accent");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(
            r.file.get("active_border").unwrap().value.as_color(),
            Some([0xb4, 0xbe, 0xfe, 0xff])
        );
    }

    #[test]
    fn undefined_variable_errors() {
        let r = parse("active_border = $undefined");
        assert!(!r.errors.is_empty());
        assert!(r.errors[0].message.contains("undefined variable"));
    }

    #[test]
    fn dimension_px() {
        let r = parse("gap = 6px");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.file.get("gap").unwrap().value.as_px(), Some(6));
    }

    #[test]
    fn dimension_hz() {
        let r = parse("refresh = 144hz");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.file.get("refresh").unwrap().value.as_hz(), Some(144));
    }

    #[test]
    fn block() {
        let r = parse("colors { active_border = #b4befe\n inactive_border = #45475a }");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let b = r.file.block("colors").unwrap();
        assert!(b.get("active_border").is_some());
        assert!(b.get("inactive_border").is_some());
    }

    #[test]
    fn labeled_block() {
        let r = parse(r#"bar_module clock { format = "%H:%M" }"#);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let b = r.file.block("bar_module").unwrap();
        assert_eq!(b.label.as_ref().unwrap().value, "clock");
    }

    #[test]
    fn labeled_block_monitor() {
        let r = parse("monitor eDP-1 { width = 1920\n height = 1080 }");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let b = r.file.block("monitor").unwrap();
        assert_eq!(b.label.as_ref().unwrap().value, "eDP-1");
    }

    #[test]
    fn bracketed_array() {
        let r = parse("modules_left = [workspaces, clock]");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(matches!(
            r.file.get("modules_left").unwrap().value,
            Value::Array(_)
        ));
    }

    #[test]
    fn keybind_colon_syntax() {
        // The combo token "SUPER+SHIFT:q" must survive as a single ident.
        let r = parse("keybind = SUPER+SHIFT:q, close");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let v = &r.file.get("keybind").unwrap().value;
        if let Value::Array(items) = v {
            assert_eq!(items[0].value.as_str(), Some("SUPER+SHIFT:q"));
            assert_eq!(items[1].value.as_str(), Some("close"));
        } else {
            panic!("expected array");
        }
    }

    #[test]
    fn keybind_with_args() {
        let r = parse("keybind = SUPER:d, exec, rofi, -show, drun");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let v = &r.file.get("keybind").unwrap().value;
        assert!(matches!(v, Value::Array(items) if items.len() == 5));
    }

    #[test]
    fn boolean_flag() {
        let r = parse("enabled = true");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.file.get("enabled").unwrap().value.as_bool(), Some(true));
    }

    #[test]
    fn comment_ignored() {
        let r = parse("// this is a comment\ngap = 4px");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.file.get("gap").is_some());
    }

    #[test]
    fn inline_comment_stripped() {
        let r = parse("$x = #b4befe  // lavender");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn unknown_block_accepted() {
        let r = parse("animations { enabled = true\n speed = 200ms }");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn pipe_in_exec_args() {
        let r = parse(r#"keybind = SUPER:v, exec, cliphist, list, |, rofi, -dmenu, |, wl-copy"#);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }

    #[test]
    fn last_wins() {
        let r = parse("gap = 4px\ngap = 8px");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        // get_last returns the last assignment
        assert_eq!(r.file.get_last("gap").unwrap().value.as_px(), Some(8));
    }

    #[test]
    fn monitor_block() {
        let r =
            parse("monitor eDP-1 { width = 1920\nheight = 1080\nrefresh = 144hz\nscale = 1.0 }");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let b = r.file.block("monitor").unwrap();
        assert_eq!(b.get("width").unwrap().value.as_px(), Some(1920));
        assert_eq!(b.get("refresh").unwrap().value.as_hz(), Some(144));
        assert!((b.get("scale").unwrap().value.as_f64().unwrap() - 1.0).abs() < f64::EPSILON);
    }
}
