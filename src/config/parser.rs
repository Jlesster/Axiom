// config/parser.rs — hand-written recursive-descent parser for trixie.conf
//
// Grammar (informal):
//
//   file       = item*
//   item       = block | assignment | comment
//   block      = ident ( string )? '{' item* '}'
//   assignment = ident '=' value
//   value      = string | color | dimension | ident | array | number
//   array      = '[' (value (',' value)*)? ']'
//   color      = '#' hex{6,8}
//   dimension  = number unit        (unit = 'px' | 'hz' | '%')
//   string     = '"' [^"]* '"'
//   comment    = '#' [^\n]*         (only when '#' not followed by hex digit after ws)
//
// Tokens carry byte-range spans for LSP diagnostics.

use std::fmt;

// ── Span ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
    pub fn merge(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
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
    pub label: Option<Spanned<String>>, // e.g. `bar_module clock { … }`
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
    /// Find all top-level assignments with the given key.
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Spanned<Value>> + 'a {
        self.items.iter().filter_map(move |i| {
            if let Item::Assignment(a) = i {
                if a.key.value == key {
                    return Some(&a.value);
                }
            }
            None
        })
    }

    /// First top-level assignment value for `key`.
    pub fn get<'a>(&'a self, key: &'a str) -> Option<&'a Spanned<Value>> {
        self.get_all(key).next()
    }

    /// All top-level blocks with the given name.
    pub fn blocks<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Block> + 'a {
        self.items.iter().filter_map(move |i| {
            if let Item::Block(b) = i {
                if b.name.value == name {
                    return Some(b);
                }
            }
            None
        })
    }

    /// First block of the given name.
    pub fn block<'a>(&'a self, name: &'a str) -> Option<&'a Block> {
        self.blocks(name).next()
    }
}

impl Block {
    pub fn get(&self, key: &str) -> Option<&Spanned<Value>> {
        self.items.iter().find_map(|i| {
            if let Item::Assignment(a) = i {
                if a.key.value == key {
                    return Some(&a.value);
                }
            }
            None
        })
    }
    pub fn get_all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Spanned<Value>> + 'a {
        self.items.iter().filter_map(move |i| {
            if let Item::Assignment(a) = i {
                if a.key.value == key {
                    return Some(&a.value);
                }
            }
            None
        })
    }
    pub fn blocks<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Block> + 'a {
        self.items.iter().filter_map(move |i| {
            if let Item::Block(b) = i {
                if b.name.value == name {
                    return Some(b);
                }
            }
            None
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

// ── Lexer ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
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
    line: u32,
    col: u32,
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
            // whitespace
            while self.peek_char().map_or(false, |c| c.is_ascii_whitespace()) {
                self.advance();
            }
            // comment: # not followed by a hex digit (hex digits start colors)
            // We disambiguate: '#' at start of value position = color.
            // '#' at line start or after whitespace where next char is NOT [0-9a-fA-F] = comment.
            // The parser handles this by context; lexer always treats # as potential color start.
            // Line comments use '//'
            if self.src[self.pos..].starts_with("//") {
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

        let Some(ch) = self.peek_char() else {
            return Ok(Spanned::new(Tok::Eof, Span::new(start, start)));
        };

        macro_rules! err {
            ($msg:expr) => {
                Err(ParseError {
                    message: $msg.to_string(),
                    span: Span::new(start, self.pos),
                    line,
                    col,
                })
            };
        }

        match ch {
            '=' => {
                self.advance();
                Ok(Spanned::new(Tok::Eq, Span::new(start, self.pos)))
            }
            '{' => {
                self.advance();
                Ok(Spanned::new(Tok::LBrace, Span::new(start, self.pos)))
            }
            '}' => {
                self.advance();
                Ok(Spanned::new(Tok::RBrace, Span::new(start, self.pos)))
            }
            '[' => {
                self.advance();
                Ok(Spanned::new(Tok::LBracket, Span::new(start, self.pos)))
            }
            ']' => {
                self.advance();
                Ok(Spanned::new(Tok::RBracket, Span::new(start, self.pos)))
            }
            ',' => {
                self.advance();
                Ok(Spanned::new(Tok::Comma, Span::new(start, self.pos)))
            }

            '"' => {
                self.advance();
                let mut s = String::new();
                loop {
                    match self.advance() {
                        None => return err!("unterminated string"),
                        Some('"') => break,
                        Some('\\') => match self.advance() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(c) => s.push(c),
                            None => return err!("unterminated escape"),
                        },
                        Some(c) => s.push(c),
                    }
                }
                Ok(Spanned::new(Tok::StringLit(s), Span::new(start, self.pos)))
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
                            span: Span::new(start, self.pos),
                            line,
                            col,
                        })?;
                        let r = (v >> 16) as u8;
                        let g = (v >> 8) as u8;
                        let b = v as u8;
                        Ok(Spanned::new(
                            Tok::Color(r, g, b, 255),
                            Span::new(start, self.pos),
                        ))
                    }
                    8 => {
                        let v = u32::from_str_radix(hex, 16).map_err(|_| ParseError {
                            message: "invalid hex color".into(),
                            span: Span::new(start, self.pos),
                            line,
                            col,
                        })?;
                        let r = (v >> 24) as u8;
                        let g = (v >> 16) as u8;
                        let b = (v >> 8) as u8;
                        let a = v as u8;
                        Ok(Spanned::new(
                            Tok::Color(r, g, b, a),
                            Span::new(start, self.pos),
                        ))
                    }
                    _ => err!(format!(
                        "color must be #rrggbb or #rrggbbaa, got {} hex chars",
                        hex.len()
                    )),
                }
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
                // check for unit suffix
                let unit = if self.src[self.pos..].starts_with("px") {
                    self.pos += 2;
                    self.col += 2;
                    Some(Unit::Px)
                } else if self.src[self.pos..].starts_with("hz")
                    || self.src[self.pos..].starts_with("Hz")
                {
                    self.pos += 2;
                    self.col += 2;
                    Some(Unit::Hz)
                } else if self.src[self.pos..].starts_with("ms") {
                    self.pos += 2;
                    self.col += 2;
                    Some(Unit::Ms)
                } else if self.peek_char() == Some('%') {
                    self.advance();
                    Some(Unit::Percent)
                } else {
                    None
                };
                let span = Span::new(start, self.pos);
                if let Some(u) = unit {
                    let v: f64 = num_str.parse().map_err(|_| ParseError {
                        message: format!("invalid number '{num_str}'"),
                        span,
                        line,
                        col,
                    })?;
                    Ok(Spanned::new(Tok::Dimension(v, u), span))
                } else if is_float {
                    let v: f64 = num_str.parse().map_err(|_| ParseError {
                        message: format!("invalid float '{num_str}'"),
                        span,
                        line,
                        col,
                    })?;
                    Ok(Spanned::new(Tok::Float(v), span))
                } else {
                    let v: i64 = num_str.parse().map_err(|_| ParseError {
                        message: format!("invalid integer '{num_str}'"),
                        span,
                        line,
                        col,
                    })?;
                    Ok(Spanned::new(Tok::Int(if neg { -v } else { v }), span))
                }
            }

            c if c.is_alphabetic() || c == '_' => {
                while self.peek_char().map_or(false, |c| {
                    c.is_alphanumeric() || c == '_' || c == '-' || c == ':' || c == '.'
                }) {
                    self.advance();
                }
                let s = self.src[start..self.pos].to_string();
                let span = Span::new(start, self.pos);
                let tok = match s.as_str() {
                    "true" => Tok::Ident("true".into()),
                    "false" => Tok::Ident("false".into()),
                    _ => Tok::Ident(s),
                };
                Ok(Spanned::new(tok, span))
            }

            other => err!(format!("unexpected character '{other}'")),
        }
    }
}

// ── Parser ────────────────────────────────────────────────────────────────────

pub struct Parser<'a> {
    lexer: Lexer<'a>,
    current: Option<Spanned<Tok>>,
    pub errors: Vec<ParseError>,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Self {
        let mut p = Self {
            lexer: Lexer::new(src),
            current: None,
            errors: Vec::new(),
        };
        p.bump();
        p
    }

    fn bump(&mut self) {
        loop {
            match self.lexer.next_tok() {
                Ok(t) => {
                    self.current = Some(t);
                    return;
                }
                Err(e) => {
                    self.errors.push(e);
                }
            }
        }
    }

    fn peek(&self) -> &Tok {
        self.current.as_ref().map(|s| &s.value).unwrap_or(&Tok::Eof)
    }

    fn peek_spanned(&self) -> Spanned<&Tok> {
        match &self.current {
            Some(s) => Spanned::new(&s.value, s.span),
            None => Spanned::new(&Tok::Eof, Span::default()),
        }
    }

    fn expect(&mut self, expected: &Tok) -> Result<Span, ParseError> {
        let s = self.peek_spanned();
        if s.value == expected {
            let span = s.span;
            self.bump();
            Ok(span)
        } else {
            let msg = format!("expected {:?}, got {:?}", expected, s.value);
            Err(ParseError {
                message: msg,
                span: s.span,
                line: 0,
                col: 0,
            })
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

    pub fn parse_file(&mut self) -> ConfigFile {
        let mut items = Vec::new();
        while *self.peek() != Tok::Eof {
            match self.parse_item() {
                Some(i) => items.push(i),
                None => {
                    self.bump();
                } // error recovery: skip token
            }
        }
        ConfigFile { items }
    }

    fn parse_item(&mut self) -> Option<Item> {
        let name = self.eat_ident()?;
        match self.peek() {
            Tok::Eq => {
                self.bump();
                let value = self.parse_value().unwrap_or_else(|| {
                    let s = self.peek_spanned().span;
                    self.errors.push(ParseError {
                        message: "expected value after '='".into(),
                        span: s,
                        line: 0,
                        col: 0,
                    });
                    Spanned::new(Value::Ident(String::new()), s)
                });
                Some(Item::Assignment(Assignment { key: name, value }))
            }
            Tok::LBrace => {
                self.bump();
                let items = self.parse_block_body();
                let end = self.expect(&Tok::RBrace).unwrap_or_default();
                let span = name.span.merge(end);
                Some(Item::Block(Block {
                    name,
                    label: None,
                    items,
                    span,
                }))
            }
            Tok::StringLit(_) | Tok::Ident(_) => {
                // block with label: `bar_module clock { … }`
                let label = self.parse_value();
                if *self.peek() == Tok::LBrace {
                    self.bump();
                    let items = self.parse_block_body();
                    let end = self.expect(&Tok::RBrace).unwrap_or_default();
                    let span = name.span.merge(end);
                    let label_sv = label.map(|sv| {
                        Spanned::new(sv.value.as_str().unwrap_or("").to_string(), sv.span)
                    });
                    Some(Item::Block(Block {
                        name,
                        label: label_sv,
                        items,
                        span,
                    }))
                } else {
                    // treat as assignment with ident value
                    let v = label.unwrap_or_else(|| {
                        Spanned::new(Value::Ident(String::new()), Span::default())
                    });
                    Some(Item::Assignment(Assignment {
                        key: name,
                        value: v,
                    }))
                }
            }
            _ => {
                // bare ident — treat as boolean true flag
                Some(Item::Assignment(Assignment {
                    key: name.clone(),
                    value: Spanned::new(Value::Bool(true), name.span),
                }))
            }
        }
    }

    fn parse_block_body(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        while *self.peek() != Tok::RBrace && *self.peek() != Tok::Eof {
            match self.parse_item() {
                Some(i) => items.push(i),
                None => {
                    self.bump();
                }
            }
        }
        items
    }

    fn parse_value(&mut self) -> Option<Spanned<Value>> {
        let s = self.peek_spanned();
        match s.value {
            Tok::StringLit(_) => {
                if let Some(Spanned {
                    value: Tok::StringLit(st),
                    span,
                }) = self.current.take()
                {
                    self.bump();
                    Some(Spanned::new(Value::String(st), span))
                } else {
                    None
                }
            }
            Tok::Ident(_) => {
                if let Some(Spanned {
                    value: Tok::Ident(id),
                    span,
                }) = self.current.take()
                {
                    self.bump();
                    let v = match id.as_str() {
                        "true" => Value::Bool(true),
                        "false" => Value::Bool(false),
                        _ => Value::Ident(id),
                    };
                    Some(Spanned::new(v, span))
                } else {
                    None
                }
            }
            Tok::Int(_) => {
                if let Some(Spanned {
                    value: Tok::Int(n),
                    span,
                }) = self.current.take()
                {
                    self.bump();
                    Some(Spanned::new(Value::Int(n), span))
                } else {
                    None
                }
            }
            Tok::Float(_) => {
                if let Some(Spanned {
                    value: Tok::Float(f),
                    span,
                }) = self.current.take()
                {
                    self.bump();
                    Some(Spanned::new(Value::Float(f), span))
                } else {
                    None
                }
            }
            Tok::Dimension(_, _) => {
                if let Some(Spanned {
                    value: Tok::Dimension(v, u),
                    span,
                }) = self.current.take()
                {
                    self.bump();
                    Some(Spanned::new(Value::Dimension(v, u), span))
                } else {
                    None
                }
            }
            Tok::Color(_, _, _, _) => {
                if let Some(Spanned {
                    value: Tok::Color(r, g, b, a),
                    span,
                }) = self.current.take()
                {
                    self.bump();
                    Some(Spanned::new(Value::Color(r, g, b, a), span))
                } else {
                    None
                }
            }
            Tok::LBracket => {
                let start = s.span;
                self.bump();
                let mut items = Vec::new();
                while *self.peek() != Tok::RBracket && *self.peek() != Tok::Eof {
                    if let Some(v) = self.parse_value() {
                        items.push(v);
                    }
                    if *self.peek() == Tok::Comma {
                        self.bump();
                    }
                }
                let end = self.expect(&Tok::RBracket).unwrap_or_default();
                Some(Spanned::new(Value::Array(items), start.merge(end)))
            }
            _ => None,
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

pub struct ParseResult {
    pub file: ConfigFile,
    pub errors: Vec<ParseError>,
}

pub fn parse(src: &str) -> ParseResult {
    let mut p = Parser::new(src);
    let file = p.parse_file();
    ParseResult {
        file,
        errors: p.errors,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_assignment() {
        let r = parse(r#"font = "JetBrains Mono", 20px"#);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn color_value() {
        let r = parse("active_border = #b4befe");
        assert!(r.errors.is_empty());
        let v = r.file.get("active_border").unwrap();
        assert_eq!(v.value.as_color(), Some([0xb4, 0xbe, 0xfe, 0xff]));
    }

    #[test]
    fn block() {
        let r = parse("colors { active_border = #b4befe\n inactive_border = #45475a }");
        assert!(r.errors.is_empty());
        let b = r.file.block("colors").unwrap();
        assert!(b.get("active_border").is_some());
    }

    #[test]
    fn labeled_block() {
        let r = parse(r#"bar_module clock { format = "%H:%M" }"#);
        assert!(r.errors.is_empty());
        let b = r.file.block("bar_module").unwrap();
        assert_eq!(b.label.as_ref().unwrap().value, "clock");
    }

    #[test]
    fn array_value() {
        let r = parse("modules_left = [workspaces, clock]");
        assert!(r.errors.is_empty());
        let v = r.file.get("modules_left").unwrap();
        assert!(matches!(v.value, Value::Array(_)));
    }

    #[test]
    fn dimension() {
        let r = parse("gap = 4px");
        assert!(r.errors.is_empty());
        let v = r.file.get("gap").unwrap();
        assert_eq!(v.value.as_px(), Some(4));
    }
}
