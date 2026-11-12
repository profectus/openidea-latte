//! Highlighting of Latte templates: HTML markup, Latte tags `{…}`, `n:`
//! attributes and the PHP expressions inside them.
//!
//! The editor shows these tokens only for languages without a CodeMirror
//! mode, which is why `languages/latte.toml` sets no `ui-mode`: this lexer
//! colours the HTML as well.
//!
//! Colours follow the IDE scheme's categories: tag names and `n:` attribute
//! names are keywords, block names are function
//! declarations, block references and components are function calls, link
//! destinations and PHP classes are types.

use openidea_plugin::syntax::{Highlight, Token};

/// Latte's built-in filters (others are custom: still highlighted, as calls).
const LATTE_FILTERS: &[&str] = &[
    "batch", "breakLines", "breaklines", "bytes", "capitalize", "ceil", "checkUrl", "clamp", "dataStream",
    "datastream", "date", "escapeUrl", "explode", "filter", "first", "firstLower", "firstUpper", "floor",
    "group", "implode", "indent", "join", "last", "length", "localDate", "lower", "number", "padLeft",
    "padRight", "query", "random", "repeat", "replace", "replaceRE", "replaceRe", "reverse", "round", "slice",
    "sort", "spaceless", "split", "strip", "stripHtml", "striphtml", "stripTags", "striptags", "substr",
    "toAscii", "trim", "truncate", "upper", "webalize", "noescape", "escapeHtml", "escapeHtmlAttr",
    "escapeJs", "escapeCss", "escapeICal", "escapeXml", "nocheck", "timeAgoInWords", "translate",
];

/// Words that are keywords inside Latte/PHP expressions.
const KEYWORDS: &[&str] = &[
    "and", "or", "xor", "as", "in", "instanceof", "new", "clone", "fn", "function", "match", "use", "static",
    "true", "false", "null", "default", "from", "with", "local", "this", "parent", "block", "file", "none",
    "auto", "if", "else", "print", "list", "array", "isset", "empty",
];

/// Primitive types, typed in `{varType}`, `{var}`, `{parameters}`, ….
const PRIMITIVES: &[&str] = &[
    "int", "integer", "string", "bool", "boolean", "float", "double", "array", "mixed", "iterable", "object",
    "callable", "void", "null", "false", "true", "self", "static", "never", "list", "non-empty-string",
    "positive-int", "class-string", "resource", "scalar", "numeric",
];

/// Tags declaring a block: the first argument is its name.
const BLOCK_DECL: &[&str] = &["block", "define", "snippet", "snippetArea"];
/// Tags whose first argument is a template file or a block reference.
const INCLUDE_TAGS: &[&str] = &["include", "embed", "import", "layout", "extends", "sandbox", "includeblock"];
/// Tags whose first argument is a component or a form control.
const COMPONENT_TAGS: &[&str] = &[
    "control", "form", "formPrint", "formClassPrint", "formContainer", "formContext", "input", "label",
    "inputError",
];
/// Tags whose arguments start with PHP types.
const TYPE_TAGS: &[&str] = &["varType", "var", "default", "parameters", "templateType", "varPrint", "templatePrint"];

/// What ends an expression.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The `}` closing a Latte tag (nested braces are counted).
    Brace,
    /// The quote closing an attribute value.
    Quote(u8),
    /// Whitespace or `>` (an unquoted attribute value).
    Bare,
}

/// The previous significant token of an expression.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    Other,
    /// `->` / `?->`: a member follows.
    Arrow,
    /// `::`: a static member follows.
    Static,
    /// `new` / `instanceof`: a class follows.
    Class,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Can `{` at `i` open a Latte tag? (Not `{ `, not `{{`.)
fn opens_tag(bytes: &[u8], i: usize) -> bool {
    bytes[i] == b'{' && bytes.get(i + 1).is_some_and(|n| !n.is_ascii_whitespace() && *n != b'{' && *n != b'}')
}

struct Lexer<'a> {
    text: &'a str,
    b: &'a [u8],
    out: Vec<Token>,
}

pub fn highlights(text: &str) -> Vec<Token> {
    let mut lx = Lexer { text, b: text.as_bytes(), out: Vec::new() };
    lx.document();
    sanitize_tokens(lx.out, text.len())
}

impl Lexer<'_> {
    fn push(&mut self, start: usize, end: usize, kind: Highlight) {
        if start < end {
            self.out.push(Token { range: start..end, kind });
        }
    }

    /// Advances past one character.
    fn step(&self, i: usize) -> usize {
        i + self.text[i..].chars().next().map_or(1, char::len_utf8)
    }

    fn document(&mut self) {
        let len = self.b.len();
        let mut i = 0;
        while i < len {
            let rest = &self.b[i..];
            if rest.starts_with(b"{*") {
                let end = self.text[i + 2..].find("*}").map_or(len, |p| i + 2 + p + 2);
                let kind = if rest.starts_with(b"{**") { Highlight::DocComment } else { Highlight::Comment };
                self.push(i, end, kind);
                i = end;
            } else if rest.starts_with(b"<!--") {
                let end = self.text[i + 4..].find("-->").map_or(len, |p| i + 4 + p + 3);
                self.push(i, end, Highlight::Comment);
                i = end;
            } else if opens_tag(self.b, i) {
                let tag_start = i;
                let name = self.latte_tag(&mut i);
                if name == "syntax" && self.text[tag_start..i].contains("off") {
                    // Plain text until `{/syntax}`.
                    match self.text[i..].find("{/syntax}") {
                        Some(p) => {
                            let close = i + p;
                            i = close;
                            self.latte_tag(&mut i);
                        }
                        None => i = len,
                    }
                }
            } else if rest[0] == b'<' && rest.get(1).is_some_and(|n| n.is_ascii_alphabetic() || *n == b'/' || *n == b'!') {
                let name = self.html_tag(&mut i);
                if matches!(name.as_str(), "script" | "style") {
                    self.raw_text(&mut i, &name);
                }
            } else if rest[0] == b'&' {
                i = self.entity(i);
            } else {
                i = self.step(i);
            }
        }
    }

    /// `&amp;`, `&#160;`, `&#x1F600;`.
    fn entity(&mut self, i: usize) -> usize {
        let end = self.b[i + 1..].iter().take(32).position(|c| !(c.is_ascii_alphanumeric() || *c == b'#'));
        match end {
            Some(n) if n > 0 && self.b[i + 1 + n] == b';' => {
                self.push(i, i + n + 2, Highlight::Escape);
                i + n + 2
            }
            _ => i + 1,
        }
    }

    /// Contents of `<script>` / `<style>`: only Latte tags and comments.
    fn raw_text(&mut self, i: &mut usize, element: &str) {
        let close = format!("</{element}");
        while *i < self.b.len() {
            if self.text[*i..].len() >= close.len() && self.text[*i..*i + close.len()].eq_ignore_ascii_case(&close) {
                return;
            }
            if self.b[*i..].starts_with(b"{*") {
                let end = self.text[*i + 2..].find("*}").map_or(self.b.len(), |p| *i + 2 + p + 2);
                self.push(*i, end, Highlight::Comment);
                *i = end;
            } else if opens_tag(self.b, *i) && !matches!(self.b[*i + 1], b'\'' | b'"') {
                self.latte_tag(i);
            } else {
                *i = self.step(*i);
            }
        }
    }

    /// A Latte tag at `{`; returns its name (`""` for `{$var}` / `{=expr}`).
    fn latte_tag(&mut self, i: &mut usize) -> String {
        self.push(*i, *i + 1, Highlight::Tag);
        let mut j = *i + 1;
        let name_start = j;
        if self.b.get(j) == Some(&b'/') {
            j += 1;
        }
        while j < self.b.len() && is_ident(self.b[j]) {
            j += 1;
        }
        let name = self.text[name_start..j].trim_start_matches('/').to_owned();
        if name.is_empty() || !is_ident_start(name.as_bytes()[0]) {
            j = name_start; // `{$x}`, `{=…}`, `{/}`
            if self.b.get(j) == Some(&b'/') {
                self.push(j, j + 1, Highlight::Keyword);
                j += 1;
            }
        } else {
            self.push(name_start, j, Highlight::Keyword);
        }
        self.expr(&mut j, Stop::Brace, &name);
        if self.b.get(j) == Some(&b'}') {
            self.push(j, j + 1, Highlight::Tag);
            j += 1;
        }
        *i = j;
        name
    }

    /// An HTML tag at `<`; returns the lower-case element name (`""` for closing tags).
    fn html_tag(&mut self, i: &mut usize) -> String {
        let start = *i;
        let mut j = *i + 1;
        let closing = self.b.get(j) == Some(&b'/');
        if closing || self.b.get(j) == Some(&b'!') {
            j += 1;
        }
        let name_start = j;
        while j < self.b.len() && (self.b[j].is_ascii_alphanumeric() || matches!(self.b[j], b'-' | b':' | b'_')) {
            j += 1;
        }
        let name = self.text[name_start..j].to_ascii_lowercase();
        self.push(start, j, Highlight::Tag);

        while j < self.b.len() {
            let c = self.b[j];
            if c == b'>' {
                self.push(j, j + 1, Highlight::Tag);
                j += 1;
                break;
            }
            if self.b[j..].starts_with(b"/>") {
                self.push(j, j + 2, Highlight::Tag);
                j += 2;
                break;
            }
            if c == b'<' {
                break; // unclosed tag
            }
            if opens_tag(self.b, j) {
                self.latte_tag(&mut j);
                continue;
            }
            if c.is_ascii_whitespace() {
                j += 1;
                continue;
            }
            if is_ident_start(c) || matches!(c, b':' | b'@' | b'[' | b'(' | b'#' | b'*') {
                j = self.attribute(j);
                continue;
            }
            j = self.step(j);
        }
        *i = j;
        if closing { String::new() } else { name }
    }

    /// An attribute and its value; returns where it ends.
    fn attribute(&mut self, start: usize) -> usize {
        let mut j = start + 1;
        while j < self.b.len() && !self.b[j].is_ascii_whitespace() && !matches!(self.b[j], b'=' | b'>' | b'/' | b'{') {
            j += 1;
        }
        let attr = &self.text[start..j];
        let latte = attr.strip_prefix("n:").map(|n| {
            let n = n.trim_start_matches("inner-").trim_start_matches("tag-");
            match n {
                "href" => "link",
                "name" => "control",
                other => other,
            }
            .to_owned()
        });
        self.push(start, j, if latte.is_some() { Highlight::Keyword } else { Highlight::Attribute });

        let mut k = j;
        while k < self.b.len() && self.b[k].is_ascii_whitespace() {
            k += 1;
        }
        if self.b.get(k) != Some(&b'=') {
            return j;
        }
        self.push(k, k + 1, Highlight::Operator);
        k += 1;
        while k < self.b.len() && self.b[k].is_ascii_whitespace() {
            k += 1;
        }
        let Some(&q) = self.b.get(k) else { return k };
        match (&latte, q) {
            (Some(tag), b'"' | b'\'') => {
                self.push(k, k + 1, Highlight::String);
                k += 1;
                self.expr(&mut k, Stop::Quote(q), tag);
                if self.b.get(k) == Some(&q) {
                    self.push(k, k + 1, Highlight::String);
                    k += 1;
                }
                k
            }
            (Some(tag), _) => {
                self.expr(&mut k, Stop::Bare, tag);
                k
            }
            (None, b'"' | b'\'') => self.attr_value(k + 1, q, k),
            (None, _) => {
                let end = k + self.b[k..].iter().take_while(|c| !c.is_ascii_whitespace() && **c != b'>').count();
                self.push(k, end, Highlight::String);
                end
            }
        }
    }

    /// A quoted HTML attribute value from `from` (after the quote at `open`),
    /// with Latte tags inside; returns where it ends.
    fn attr_value(&mut self, from: usize, quote: u8, open: usize) -> usize {
        let mut segment = open;
        let mut i = from;
        while i < self.b.len() {
            if self.b[i] == quote {
                self.push(segment, i + 1, Highlight::String);
                return i + 1;
            }
            if opens_tag(self.b, i) {
                self.push(segment, i, Highlight::String);
                self.latte_tag(&mut i);
                segment = i;
                continue;
            }
            i = self.step(i);
        }
        self.push(segment, i, Highlight::String);
        i
    }

    /// A word (`[\w.\-/:#@]`) at `i`, as unquoted Latte arguments are written.
    fn bare_word(&self, i: usize) -> usize {
        let mut j = i;
        while j < self.b.len() && (is_ident(self.b[j]) || matches!(self.b[j], b'.' | b'-' | b'/' | b':' | b'#' | b'@' | b'!' | b'\\'))
        {
            j += 1;
        }
        j
    }

    /// The first argument of tags that name something; `true` when consumed.
    fn first_arg(&mut self, i: &mut usize, tag: &str, stop: Stop) -> bool {
        let c = self.b[*i];
        if c == b'$' || c == b'\'' || c == b'"' || (stop == Stop::Quote(c)) {
            return false;
        }
        let end = self.bare_word(*i);
        if end == *i {
            return false;
        }
        let word = &self.text[*i..end];
        let kind = if BLOCK_DECL.contains(&tag) {
            if word == "local" {
                self.push(*i, end, Highlight::Keyword);
                *i = end;
                return self.next_first_arg(i, tag, stop);
            }
            Highlight::Function
        } else if INCLUDE_TAGS.contains(&tag) || matches!(tag, "ifset" | "elseifset") {
            match word {
                "block" | "file" => {
                    self.push(*i, end, Highlight::Keyword);
                    *i = end;
                    return self.next_first_arg(i, tag, stop);
                }
                "parent" | "this" | "none" | "auto" | "false" => Highlight::Keyword,
                w if w.contains(['.', '/']) && !w.starts_with('#') => Highlight::String,
                _ => Highlight::FunctionCall,
            }
        } else if matches!(tag, "link" | "plink") {
            Highlight::Type
        } else if COMPONENT_TAGS.contains(&tag) {
            Highlight::FunctionCall
        } else {
            return false;
        };
        self.push(*i, end, kind);
        *i = end;
        true
    }

    /// After `block` / `file` / `local`: the name that follows.
    fn next_first_arg(&mut self, i: &mut usize, tag: &str, stop: Stop) -> bool {
        while *i < self.b.len() && self.b[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if *i < self.b.len() {
            let tag = if tag == "ifset" || tag == "elseifset" { "include" } else { tag };
            self.first_arg(i, tag, stop);
        }
        true
    }

    fn at_stop(&self, i: usize, stop: Stop, depth: usize) -> bool {
        let c = self.b[i];
        match stop {
            Stop::Brace => c == b'}' && depth == 0,
            Stop::Quote(q) => c == q,
            Stop::Bare => c.is_ascii_whitespace() || c == b'>',
        }
    }

    /// The next non-space byte at or after `i`.
    fn peek(&self, mut i: usize) -> Option<u8> {
        while i < self.b.len() && self.b[i].is_ascii_whitespace() {
            i += 1;
        }
        self.b.get(i).copied()
    }

    /// A Latte/PHP expression up to `stop` (exclusive).
    fn expr(&mut self, i: &mut usize, stop: Stop, tag: &str) {
        let types = TYPE_TAGS.contains(&tag);
        let mut first = true;
        let mut prev = Prev::Other;
        let mut depth = 0usize;

        while *i < self.b.len() && !self.at_stop(*i, stop, depth) {
            let c = self.b[*i];
            if c.is_ascii_whitespace() {
                *i += 1;
                continue;
            }
            if first {
                first = false;
                if self.first_arg(i, tag, stop) {
                    continue;
                }
            }
            match c {
                b'{' => {
                    depth += 1;
                    self.push(*i, *i + 1, Highlight::Punctuation);
                    *i += 1;
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    self.push(*i, *i + 1, Highlight::Punctuation);
                    *i += 1;
                }
                b'\'' | b'"' => {
                    let start = *i;
                    *i += 1;
                    while *i < self.b.len() && self.b[*i] != c && !(stop == Stop::Quote(self.b[*i])) {
                        *i = if self.b[*i] == b'\\' { (*i + 2).min(self.b.len()) } else { self.step(*i) };
                    }
                    if self.b.get(*i) == Some(&c) {
                        *i += 1;
                    }
                    self.push(start, *i, Highlight::String);
                    prev = Prev::Other;
                }
                b'$' => {
                    let start = *i;
                    *i += 1;
                    while *i < self.b.len() && is_ident(self.b[*i]) {
                        *i += 1;
                    }
                    let kind = if prev == Prev::Static { Highlight::Property } else { Highlight::Variable };
                    self.push(start, *i, kind);
                    prev = Prev::Other;
                }
                b'|' if self.b.get(*i + 1).is_some_and(|n| is_ident_start(*n)) => {
                    self.push(*i, *i + 1, Highlight::Operator);
                    let start = *i + 1;
                    *i = start;
                    while *i < self.b.len() && is_ident(self.b[*i]) {
                        *i += 1;
                    }
                    let name = &self.text[start..*i];
                    let kind = if LATTE_FILTERS.contains(&name) { Highlight::Builtin } else { Highlight::FunctionCall };
                    self.push(start, *i, kind);
                    prev = Prev::Other;
                }
                b'0'..=b'9' => {
                    let start = *i;
                    while *i < self.b.len() && (self.b[*i].is_ascii_alphanumeric() || matches!(self.b[*i], b'.' | b'_')) {
                        *i += 1;
                    }
                    self.push(start, *i, Highlight::Number);
                    prev = Prev::Other;
                }
                _ if is_ident_start(c) || (c == b'\\' && self.b.get(*i + 1).is_some_and(|n| is_ident_start(*n))) => {
                    let start = *i;
                    while *i < self.b.len() && (is_ident(self.b[*i]) || self.b[*i] == b'\\') {
                        *i += 1;
                    }
                    // `non-empty-string` and friends in types.
                    while types && self.b.get(*i) == Some(&b'-') && self.b.get(*i + 1).is_some_and(|n| is_ident_start(*n)) {
                        *i += 1;
                        while *i < self.b.len() && is_ident(self.b[*i]) {
                            *i += 1;
                        }
                    }
                    let word = &self.text[start..*i];
                    let next = self.peek(*i);
                    let call = next == Some(b'(');
                    let lower = word.to_ascii_lowercase();
                    let kind = match prev {
                        Prev::Arrow if call => Some(Highlight::FunctionCall),
                        Prev::Arrow => Some(Highlight::Property),
                        Prev::Static if call => Some(Highlight::FunctionCall),
                        Prev::Static => Some(Highlight::Constant),
                        Prev::Class => Some(Highlight::Type),
                        Prev::Other if word.contains('\\') => Some(Highlight::Type),
                        Prev::Other if self.b[*i..].starts_with(b"::") => Some(Highlight::Type),
                        Prev::Other if types && !call && (PRIMITIVES.contains(&lower.as_str()) || word.starts_with(char::is_uppercase)) => {
                            Some(Highlight::Type)
                        }
                        Prev::Other if KEYWORDS.contains(&lower.as_str()) => Some(Highlight::Keyword),
                        Prev::Other if call => Some(Highlight::FunctionCall),
                        Prev::Other if word.len() > 1 && word.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') => {
                            Some(Highlight::Constant)
                        }
                        Prev::Other if word.starts_with(char::is_uppercase) => Some(Highlight::Type),
                        Prev::Other => None,
                    };
                    if let Some(kind) = kind {
                        self.push(start, *i, kind);
                    }
                    prev = if matches!(lower.as_str(), "new" | "instanceof") && prev == Prev::Other {
                        Prev::Class
                    } else {
                        Prev::Other
                    };
                }
                _ => {
                    let rest = &self.b[*i..];
                    let op = [&b"?->"[..], b"->", b"::", b"===", b"!==", b"<=>", b"=>", b"==", b"!=", b"<=", b">=", b"&&", b"||", b"??=", b"??", b"?:", b"++", b"--", b"+=", b"-=", b".=", b"**"]
                        .into_iter()
                        .find(|op| rest.starts_with(op));
                    if let Some(op) = op {
                        self.push(*i, *i + op.len(), Highlight::Operator);
                        prev = match op {
                            b"->" | b"?->" => Prev::Arrow,
                            b"::" => Prev::Static,
                            _ => Prev::Other,
                        };
                        *i += op.len();
                    } else if b"+-*/%=!<>&|^~?:.@".contains(&c) {
                        self.push(*i, *i + 1, Highlight::Operator);
                        prev = Prev::Other;
                        *i += 1;
                    } else if b"()[],;".contains(&c) {
                        self.push(*i, *i + 1, Highlight::Punctuation);
                        prev = Prev::Other;
                        *i += 1;
                    } else {
                        *i = self.step(*i);
                    }
                }
            }
        }
    }
}

/// Clamps to the text, sorts, and drops overlapping tokens.
fn sanitize_tokens(mut tokens: Vec<Token>, max_len: usize) -> Vec<Token> {
    tokens.retain_mut(|t| {
        t.range.start = t.range.start.min(max_len);
        t.range.end = t.range.end.min(max_len);
        t.range.start < t.range.end
    });
    tokens.sort_by(|a, b| a.range.start.cmp(&b.range.start).then_with(|| a.range.end.cmp(&b.range.end)));
    let mut out = Vec::with_capacity(tokens.len());
    let mut last_end = 0;
    for t in tokens {
        if t.range.start >= last_end {
            last_end = t.range.end;
            out.push(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kind of the token covering exactly the first `needle` (after `skip` bytes in it).
    fn kind_of(src: &str, needle: &str) -> Option<Highlight> {
        let tokens = highlights(src);
        let at = src.find(needle).unwrap_or_else(|| panic!("{needle:?} not in source"));
        tokens.iter().find(|t| t.range.start == at && t.range.end == at + needle.len()).map(|t| t.kind)
    }

    #[test]
    fn tag_names_are_keywords() {
        let src = "{define menu}{/define}{control cart}{ifcontent}{/ifcontent}{do $x++}{_'Hello'}{translate}x{/translate}";
        for name in ["define", "/define", "control", "ifcontent", "/ifcontent", "do", "_", "translate"] {
            assert_eq!(kind_of(src, name), Some(Highlight::Keyword), "{name}");
        }
        assert_eq!(kind_of(src, "menu"), Some(Highlight::Function));
        assert_eq!(kind_of(src, "cart"), Some(Highlight::FunctionCall));
    }

    #[test]
    fn names_in_arguments() {
        let src = "{include 'parts/menu.latte'}{include #footer}{include block sidebar}{layout none}{link Product:show $id}{form signIn}{embed card.latte}";
        assert_eq!(kind_of(src, "'parts/menu.latte'"), Some(Highlight::String));
        assert_eq!(kind_of(src, "#footer"), Some(Highlight::FunctionCall));
        assert_eq!(kind_of(src, "sidebar"), Some(Highlight::FunctionCall));
        assert_eq!(kind_of(src, "none"), Some(Highlight::Keyword));
        assert_eq!(kind_of(src, "Product:show"), Some(Highlight::Type));
        assert_eq!(kind_of(src, "$id"), Some(Highlight::Variable));
        assert_eq!(kind_of(src, "signIn"), Some(Highlight::FunctionCall));
        assert_eq!(kind_of(src, "card.latte"), Some(Highlight::String));
    }

    #[test]
    fn php_expressions() {
        let src = r"{varType App\Model\User $user}{var int $n = 1}{if $user instanceof Admin && Foo::BAR > 0}{=Strings::lower($x->name)}{$item?->getPrice()|number:2|myFilter}{/if}";
        assert_eq!(kind_of(src, r"App\Model\User"), Some(Highlight::Type));
        assert_eq!(kind_of(src, "int"), Some(Highlight::Type));
        assert_eq!(kind_of(src, "Admin"), Some(Highlight::Type));
        assert_eq!(kind_of(src, "Foo"), Some(Highlight::Type));
        assert_eq!(kind_of(src, "BAR"), Some(Highlight::Constant));
        assert_eq!(kind_of(src, "Strings"), Some(Highlight::Type));
        assert_eq!(kind_of(src, "lower"), Some(Highlight::FunctionCall));
        assert_eq!(kind_of(src, "name"), Some(Highlight::Property));
        assert_eq!(kind_of(src, "getPrice"), Some(Highlight::FunctionCall));
        assert_eq!(kind_of(src, "number"), Some(Highlight::Builtin));
        assert_eq!(kind_of(src, "myFilter"), Some(Highlight::FunctionCall));
        assert_eq!(kind_of(src, "instanceof"), Some(Highlight::Keyword));
    }

    #[test]
    fn html_and_n_attributes() {
        let src = r#"<div class="box {$cls}" n:if="$show" n:class="$a ? active"><a n:href="Home:default">x</a><form n:name=signIn>&nbsp;</form></div>"#;
        assert_eq!(kind_of(src, "<div"), Some(Highlight::Tag));
        assert_eq!(kind_of(src, "class"), Some(Highlight::Attribute));
        assert_eq!(kind_of(src, "\"box "), Some(Highlight::String));
        assert_eq!(kind_of(src, "$cls"), Some(Highlight::Variable));
        assert_eq!(kind_of(src, "n:if"), Some(Highlight::Keyword));
        assert_eq!(kind_of(src, "$show"), Some(Highlight::Variable));
        assert_eq!(kind_of(src, "Home:default"), Some(Highlight::Type));
        assert_eq!(kind_of(src, "signIn"), Some(Highlight::FunctionCall));
        assert_eq!(kind_of(src, "&nbsp;"), Some(Highlight::Escape));
        assert_eq!(kind_of(src, "</form"), Some(Highlight::Tag));
    }

    #[test]
    fn scripts_and_syntax_off() {
        let src = "<script>var o = { a: 1 }; if (x) { y() }\nlet u = {$url};</script>{syntax off}{notATag}{/syntax}";
        assert_eq!(kind_of(src, "$url"), Some(Highlight::Variable));
        assert!(kind_of(src, "a").is_none(), "JS is plain text");
        assert_eq!(kind_of(src, "</script"), Some(Highlight::Tag));
        assert!(kind_of(src, "notATag").is_none());
        assert_eq!(kind_of(src, "/syntax"), Some(Highlight::Keyword));
    }

    #[test]
    fn comments() {
        let src = "{* note *}{** doc *}<!-- html -->";
        assert_eq!(kind_of(src, "{* note *}"), Some(Highlight::Comment));
        assert_eq!(kind_of(src, "{** doc *}"), Some(Highlight::DocComment));
        assert_eq!(kind_of(src, "<!-- html -->"), Some(Highlight::Comment));
    }
}
