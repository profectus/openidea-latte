//! Just enough PHP to navigate from templates: a tokenizer, the declarations
//! of a file (namespace, imports, the class with its members and their
//! types), Composer's autoload maps to find a class's file, and member lookup
//! through parents, interfaces and traits.

use crate::navigation::{location, normalize, Files};
use openidea_plugin::Location;
use std::collections::HashMap;
use std::ops::Range as Span;

/// How far `extends` / `implements` / `use Trait` chains are followed.
const MAX_HIERARCHY: usize = 10;

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Tok {
    /// A name, possibly qualified (`Foo`, `\App\Foo`, `App\Foo`).
    Ident(Span<usize>),
    /// `$name` (span includes the `$`).
    Var(Span<usize>),
    /// A doc comment `/** … */`.
    Doc(Span<usize>),
    Punct(u8, usize),
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

fn tokenize(text: &str) -> Vec<Tok> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // Inline HTML before `<?php`.
    if let Some(p) = text.find("<?php").or_else(|| text.find("<?")) {
        i = p + if text[p..].starts_with("<?php") { 5 } else { 2 };
    }
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if b[i..].starts_with(b"/**") && !b[i..].starts_with(b"/**/") {
            let end = text[i + 3..].find("*/").map_or(b.len(), |p| i + 3 + p + 2);
            out.push(Tok::Doc(i..end));
            i = end;
        } else if b[i..].starts_with(b"/*") {
            i = text[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
        } else if b[i..].starts_with(b"#[") {
            // Attribute: skip it, brackets balanced.
            let mut depth = 0usize;
            while i < b.len() {
                match b[i] {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            i += 1;
                            break;
                        }
                    }
                    b'\'' | b'"' => i = skip_string(b, i) - 1,
                    _ => {}
                }
                i += 1;
            }
        } else if b[i..].starts_with(b"//") || c == b'#' {
            while i < b.len() && b[i] != b'\n' && !b[i..].starts_with(b"?>") {
                i += 1;
            }
        } else if c == b'\'' || c == b'"' || c == b'`' {
            i = skip_string(b, i);
        } else if b[i..].starts_with(b"<<<") {
            // Heredoc / nowdoc: up to the line holding only the identifier.
            let mut j = i + 3;
            while j < b.len() && matches!(b[j], b' ' | b'\'' | b'"') {
                j += 1;
            }
            let id_start = j;
            while j < b.len() && is_ident(b[j]) {
                j += 1;
            }
            let id = &text[id_start..j];
            i = j;
            while i < b.len() {
                let line_end = text[i..].find('\n').map_or(b.len(), |p| i + p);
                let line = text[i..line_end].trim_start();
                i = (line_end + 1).min(b.len());
                if !id.is_empty() && line.starts_with(id) && !line[id.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                    break;
                }
            }
        } else if c == b'$' && b.get(i + 1).is_some_and(|n| is_ident_start(*n)) {
            let start = i;
            i += 1;
            while i < b.len() && is_ident(b[i]) {
                i += 1;
            }
            out.push(Tok::Var(start..i));
        } else if is_ident_start(c) || (c == b'\\' && b.get(i + 1).is_some_and(|n| is_ident_start(*n))) {
            let start = i;
            while i < b.len() && (is_ident(b[i]) || b[i] == b'\\') {
                i += 1;
            }
            out.push(Tok::Ident(start..i));
        } else if c.is_ascii_digit() {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'_') {
                i += 1;
            }
        } else {
            out.push(Tok::Punct(c, i));
            i += 1;
        }
    }
    out
}

/// Past the string starting with the quote at `i`.
fn skip_string(b: &[u8], i: usize) -> usize {
    let q = b[i];
    let mut j = i + 1;
    while j < b.len() && b[j] != q {
        j += if b[j] == b'\\' { 2 } else { 1 };
    }
    (j + 1).min(b.len())
}

// ---------------------------------------------------------------------------
// Declarations
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Property,
    Constant,
}

#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    /// The name in the file (`$name` for properties declared in code).
    pub span: Span<usize>,
    /// Declared type (return type of a method), as written.
    pub ty: Option<String>,
    /// `@return` / `@var` type of its doc comment, as written.
    pub doc_ty: Option<String>,
}

impl Member {
    /// The most precise type: the doc type when the declared one says less.
    pub fn best_type(&self) -> Option<&str> {
        let vague = |t: &str| {
            let t = t.trim_start_matches('?').to_ascii_lowercase();
            matches!(t.as_str(), "array" | "iterable" | "mixed" | "object" | "static" | "self")
                || t.ends_with("|null") && matches!(t.trim_end_matches("|null"), "array" | "iterable")
        };
        match (&self.ty, &self.doc_ty) {
            (Some(t), Some(d)) if vague(t) && !d.trim().is_empty() => Some(d),
            (Some(t), _) => Some(t),
            (None, d) => d.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ClassDecl {
    pub name: String,
    pub span: Span<usize>,
    /// Parent classes (several for an interface), as written.
    pub extends: Vec<String>,
    pub implements: Vec<String>,
    pub traits: Vec<String>,
    pub members: Vec<Member>,
}

/// A PHP file's declarations.
#[derive(Clone, Debug, Default)]
pub struct PhpFile {
    pub path: String,
    pub text: String,
    pub namespace: String,
    /// alias → fully qualified name.
    pub uses: HashMap<String, String>,
    pub class: Option<ClassDecl>,
}

impl PhpFile {
    pub fn parse(path: &str, text: String) -> Self {
        let mut file = Self { path: path.to_owned(), ..Self::default() };
        parse_into(&mut file, &text);
        file.text = text;
        file
    }

    /// The fully qualified name of the file's class.
    pub fn fqcn(&self) -> Option<String> {
        let class = self.class.as_ref()?;
        Some(if self.namespace.is_empty() { class.name.clone() } else { format!("{}\\{}", self.namespace, class.name) })
    }

    /// A class name as written in this file → fully qualified (no leading `\`).
    pub fn resolve(&self, name: &str) -> String {
        if let Some(abs) = name.strip_prefix('\\') {
            return abs.to_owned();
        }
        let lower = name.to_ascii_lowercase();
        if matches!(lower.as_str(), "self" | "static" | "$this") {
            return self.fqcn().unwrap_or_else(|| name.to_owned());
        }
        let (first, rest) = name.split_once('\\').map_or((name, None), |(f, r)| (f, Some(r)));
        if let Some(full) = self.uses.iter().find(|(alias, _)| alias.eq_ignore_ascii_case(first)).map(|(_, f)| f) {
            return match rest {
                Some(rest) => format!("{full}\\{rest}"),
                None => full.clone(),
            };
        }
        if self.namespace.is_empty() { name.to_owned() } else { format!("{}\\{name}", self.namespace) }
    }

    pub fn location(&self, span: &Span<usize>) -> Location {
        location(&self.path, &self.text, span)
    }
}

const MODIFIERS: &[&str] =
    &["public", "protected", "private", "static", "readonly", "var", "final", "abstract", "const"];

/// The text of a type between declarations, without modifiers.
fn type_between(text: &str, span: Span<usize>) -> Option<String> {
    let words: Vec<&str> = text[span].split_whitespace().filter(|w| !MODIFIERS.contains(&w.to_ascii_lowercase().as_str())).collect();
    let t = words.join(" ");
    let t = t.trim_end_matches(['&', '.']).trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// The value of `@tag` in a doc comment: the type after it.
fn doc_type(doc: &str, tag: &str) -> Option<String> {
    doc.lines().find_map(|line| {
        let rest = line.trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == '/').strip_prefix(tag)?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        read_type(rest.trim_start()).map(|(t, _)| t.to_owned())
    })
}

/// A type at the start of `s` (balanced `<…>`, `{…}` and `(…)`), and the rest.
fn read_type(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0i32;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '{' | '(' => depth += 1,
            '>' | '}' | ')' => depth -= 1,
            c if c.is_whitespace() && depth <= 0 => return (i > 0).then(|| (&s[..i], &s[i..])),
            '$' if depth <= 0 => return (i > 0).then(|| (s[..i].trim_end(), &s[i..])),
            _ => {}
        }
    }
    (!s.is_empty()).then_some((s, ""))
}

/// `@property Type $name` / `@method Type name()` lines of a class doc comment.
fn doc_members(text: &str, doc: &Span<usize>, members: &mut Vec<Member>) {
    let mut line_start = doc.start;
    for line in text[doc.clone()].split_inclusive('\n') {
        let offset = line_start;
        line_start += line.len();
        let trimmed = line.trim_start_matches(|c: char| c.is_whitespace() || c == '*' || c == '/');
        let at = offset + (line.len() - trimmed.len());
        let Some(rest) = trimmed.strip_prefix('@') else { continue };
        let (tag, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let rest_at = at + 1 + tag.len() + 1;
        match tag {
            "property" | "property-read" | "property-write" => {
                let Some(dollar) = rest.find('$') else { continue };
                let ty = rest[..dollar].trim();
                let name_len = rest[dollar + 1..].bytes().take_while(|b| is_ident(*b)).count();
                let start = rest_at + dollar;
                members.push(Member {
                    name: rest[dollar + 1..dollar + 1 + name_len].to_owned(),
                    kind: MemberKind::Property,
                    span: start..start + 1 + name_len,
                    ty: (!ty.is_empty()).then(|| ty.to_owned()),
                    doc_ty: None,
                });
            }
            "method" => {
                let Some(paren) = rest.find('(') else { continue };
                let head = rest[..paren].trim_end();
                let name_start = head.rfind(char::is_whitespace).map_or(0, |p| p + 1);
                let name = &head[name_start..];
                let ty = head[..name_start].trim().trim_start_matches("static").trim();
                if name.is_empty() {
                    continue;
                }
                let start = rest_at + name_start;
                members.push(Member {
                    name: name.to_owned(),
                    kind: MemberKind::Method,
                    span: start..start + name.len(),
                    ty: (!ty.is_empty()).then(|| ty.to_owned()),
                    doc_ty: None,
                });
            }
            _ => {}
        }
    }
}

fn parse_into(file: &mut PhpFile, text: &str) {
    let toks = tokenize(text);
    let word = |t: &Tok| match t {
        Tok::Ident(s) => Some(&text[s.clone()]),
        _ => None,
    };
    let is_punct = |t: Option<&Tok>, c: u8| matches!(t, Some(Tok::Punct(p, _)) if *p == c);

    let mut depth = 0usize;
    let mut class: Option<ClassDecl> = None;
    let mut class_depth = usize::MAX; // brace depth inside the class body
    let mut done = false; // the first class is closed
    let mut doc: Option<Span<usize>> = None;
    // Where the current member declaration started (after `;`, `{`, `}` or a doc comment).
    let mut decl_start = 0usize;
    let mut k = 0;

    while k < toks.len() {
        let t = &toks[k];
        let in_body = class.is_some() && depth == class_depth && !done;
        match t {
            Tok::Doc(s) => {
                doc = Some(s.clone());
                decl_start = s.end;
            }
            Tok::Punct(b'{', at) => {
                depth += 1;
                decl_start = at + 1;
            }
            Tok::Punct(b'}', at) => {
                depth = depth.saturating_sub(1);
                if class.is_some() && depth + 1 == class_depth {
                    done = true;
                }
                decl_start = at + 1;
                doc = None;
            }
            Tok::Punct(b';', at) => {
                decl_start = at + 1;
                doc = None;
            }
            Tok::Ident(s) if depth == 0 && class.is_none() => {
                let w = &text[s.clone()];
                match w.to_ascii_lowercase().as_str() {
                    "namespace" => {
                        if let Some(name) = toks.get(k + 1).and_then(word) {
                            file.namespace = name.trim_start_matches('\\').to_owned();
                            k += 1;
                        }
                    }
                    "use" => k = parse_use(&toks, k + 1, text, file) - 1,
                    "class" | "interface" | "trait" | "enum" => {
                        let after_new_or_static = k > 0
                            && (matches!(toks[k - 1], Tok::Punct(b':', _)) || toks.get(k - 1).and_then(word).is_some_and(|w| w.eq_ignore_ascii_case("new")));
                        if let Some(Tok::Ident(name)) = toks.get(k + 1).filter(|_| !after_new_or_static) {
                            let mut decl = ClassDecl { name: text[name.clone()].to_owned(), span: name.clone(), ..ClassDecl::default() };
                            if let Some(d) = &doc {
                                doc_members(text, d, &mut decl.members);
                            }
                            // extends / implements up to `{`
                            let mut j = k + 2;
                            let mut list: Option<bool> = None; // Some(true): extends, Some(false): implements
                            while j < toks.len() && !is_punct(toks.get(j), b'{') {
                                if let Some(w) = word(&toks[j]) {
                                    match w.to_ascii_lowercase().as_str() {
                                        "extends" => list = Some(true),
                                        "implements" => list = Some(false),
                                        _ => match list {
                                            Some(true) => decl.extends.push(w.to_owned()),
                                            Some(false) => decl.implements.push(w.to_owned()),
                                            None => {}
                                        },
                                    }
                                }
                                j += 1;
                            }
                            class = Some(decl);
                            class_depth = depth + 1;
                            k = j;
                            continue;
                        }
                    }
                    _ => {}
                }
            }
            Tok::Ident(s) if in_body => {
                let decl = class.as_mut().expect("in a class body");
                match text[s.clone()].to_ascii_lowercase().as_str() {
                    "use" => {
                        let mut j = k + 1;
                        while let Some(t) = toks.get(j) {
                            match t {
                                Tok::Ident(n) => decl.traits.push(text[n.clone()].to_owned()),
                                Tok::Punct(b',', _) => {}
                                _ => break,
                            }
                            j += 1;
                        }
                        k = j;
                        continue;
                    }
                    "const" => {
                        // `const [type] NAME = …, OTHER = …;`
                        let mut j = k + 1;
                        let mut level = 0usize;
                        while let Some(t) = toks.get(j) {
                            match t {
                                Tok::Punct(b';', _) if level == 0 => break,
                                Tok::Punct(b'(' | b'[' | b'{', _) => level += 1,
                                Tok::Punct(b')' | b']' | b'}', _) => level = level.saturating_sub(1),
                                Tok::Ident(n) if level == 0 && is_punct(toks.get(j + 1), b'=') => decl.members.push(Member {
                                    name: text[n.clone()].to_owned(),
                                    kind: MemberKind::Constant,
                                    span: n.clone(),
                                    ty: None,
                                    doc_ty: None,
                                }),
                                _ => {}
                            }
                            j += 1;
                        }
                        k = j;
                        continue;
                    }
                    "case" => {
                        if let Some(Tok::Ident(n)) = toks.get(k + 1) {
                            decl.members.push(Member {
                                name: text[n.clone()].to_owned(),
                                kind: MemberKind::Constant,
                                span: n.clone(),
                                ty: None,
                                doc_ty: None,
                            });
                        }
                    }
                    "function" => {
                        let mut j = k + 1;
                        if is_punct(toks.get(j), b'&') {
                            j += 1;
                        }
                        if let Some(Tok::Ident(n)) = toks.get(j) {
                            let name = text[n.clone()].to_owned();
                            let doc_text = doc.as_ref().map(|d| &text[d.clone()]);
                            // Parameters: promoted constructor properties.
                            let params_open = j + 1;
                            let mut level = 0usize;
                            let mut p = params_open;
                            let mut param_start = toks.get(params_open).map_or(0, |t| tok_end(t));
                            let mut promoted = false;
                            while let Some(t) = toks.get(p) {
                                match t {
                                    Tok::Punct(b'(' | b'[', _) => level += 1,
                                    Tok::Punct(b')' | b']', _) => {
                                        level -= 1;
                                        if level == 0 {
                                            break;
                                        }
                                    }
                                    Tok::Punct(b',', at) if level == 1 => {
                                        param_start = at + 1;
                                        promoted = false;
                                    }
                                    Tok::Ident(m) if level == 1 => {
                                        let m = text[m.clone()].to_ascii_lowercase();
                                        if matches!(m.as_str(), "public" | "protected" | "private" | "readonly") {
                                            promoted = true;
                                        }
                                    }
                                    Tok::Var(v) if level == 1 && promoted && name.eq_ignore_ascii_case("__construct") => {
                                        decl.members.push(Member {
                                            name: text[v.start + 1..v.end].to_owned(),
                                            kind: MemberKind::Property,
                                            span: v.clone(),
                                            ty: type_between(text, param_start..v.start),
                                            doc_ty: None,
                                        });
                                        promoted = false;
                                    }
                                    _ => {}
                                }
                                p += 1;
                            }
                            // Return type: after `):` up to `{` or `;`.
                            let mut ty = None;
                            if is_punct(toks.get(p + 1), b':')
                                && let Some(Tok::Punct(_, colon)) = toks.get(p + 1)
                            {
                                let mut q = p + 2;
                                while q < toks.len() && !is_punct(toks.get(q), b'{') && !is_punct(toks.get(q), b';') {
                                    q += 1;
                                }
                                let end = toks.get(q).map_or(text.len(), tok_start);
                                let t = text[colon + 1..end].trim();
                                ty = (!t.is_empty()).then(|| t.to_owned());
                            }
                            decl.members.push(Member {
                                name,
                                kind: MemberKind::Method,
                                span: n.clone(),
                                ty,
                                doc_ty: doc_text.and_then(|d| doc_type(d, "@return")),
                            });
                            // Skip the parameters (and the body, through depth tracking).
                            k = p + 1;
                            doc = None;
                            continue;
                        }
                    }
                    _ => {}
                }
            }
            Tok::Var(v) if in_body => {
                let decl = class.as_mut().expect("in a class body");
                let doc_text = doc.as_ref().map(|d| &text[d.clone()]);
                decl.members.push(Member {
                    name: text[v.start + 1..v.end].to_owned(),
                    kind: MemberKind::Property,
                    span: v.clone(),
                    ty: type_between(text, decl_start..v.start),
                    doc_ty: doc_text.and_then(|d| doc_type(d, "@var")),
                });
                // `public int $a = 1, $b;` — the rest of the statement.
                let mut j = k + 1;
                let mut level = 0usize;
                while let Some(t) = toks.get(j) {
                    match t {
                        Tok::Punct(b';', _) if level == 0 => break,
                        Tok::Punct(b'(' | b'[' | b'{', _) => level += 1,
                        Tok::Punct(b')' | b']' | b'}', _) => level = level.saturating_sub(1),
                        Tok::Var(v2) if level == 0 && is_punct(toks.get(j - 1), b',') => decl.members.push(Member {
                            name: text[v2.start + 1..v2.end].to_owned(),
                            kind: MemberKind::Property,
                            span: v2.clone(),
                            ty: type_between(text, decl_start..v.start),
                            doc_ty: None,
                        }),
                        _ => {}
                    }
                    j += 1;
                }
                k = j;
                continue;
            }
            _ => {}
        }
        k += 1;
    }
    file.class = class;
}

fn tok_start(t: &Tok) -> usize {
    match t {
        Tok::Ident(s) | Tok::Var(s) | Tok::Doc(s) => s.start,
        Tok::Punct(_, at) => *at,
    }
}

fn tok_end(t: &Tok) -> usize {
    match t {
        Tok::Ident(s) | Tok::Var(s) | Tok::Doc(s) => s.end,
        Tok::Punct(_, at) => at + 1,
    }
}

/// `use A\B;`, `use A\B as C, D;`, `use A\{B, C as D};` at `k`; returns the index after `;`.
fn parse_use(toks: &[Tok], mut k: usize, text: &str, file: &mut PhpFile) -> usize {
    let word = |t: Option<&Tok>| match t {
        Some(Tok::Ident(s)) => Some(&text[s.clone()]),
        _ => None,
    };
    if word(toks.get(k)).is_some_and(|w| w.eq_ignore_ascii_case("function") || w.eq_ignore_ascii_case("const")) {
        while k < toks.len() && !matches!(toks[k], Tok::Punct(b';', _)) {
            k += 1;
        }
        return k + 1;
    }
    let mut prefix = String::new();
    while k < toks.len() {
        match &toks[k] {
            Tok::Punct(b';', _) => return k + 1,
            Tok::Punct(b'{', _) => {
                // Group: `use A\{B, C}` — `A\` is the previous name.
                if let Some(Tok::Ident(p)) = toks.get(k.wrapping_sub(1)) {
                    prefix = text[p.clone()].trim_start_matches('\\').to_owned();
                    let key = prefix.trim_end_matches('\\').rsplit('\\').next().unwrap_or_default().to_owned();
                    file.uses.remove(&key);
                }
            }
            Tok::Ident(s) => {
                let name = &text[s.clone()];
                if name.eq_ignore_ascii_case("as") {
                    if let Some(alias) = word(toks.get(k + 1))
                        && let Some(Tok::Ident(prev)) = toks.get(k - 1)
                    {
                        let full = format!("{prefix}{}", text[prev.clone()].trim_start_matches('\\'));
                        let short = full.rsplit('\\').next().unwrap_or(&full).to_owned();
                        file.uses.remove(&short);
                        file.uses.insert(alias.to_owned(), full);
                        k += 2;
                        continue;
                    }
                } else {
                    let full = format!("{prefix}{}", name.trim_start_matches('\\'));
                    let short = full.trim_end_matches('\\').rsplit('\\').next().unwrap_or(&full).to_owned();
                    file.uses.insert(short, full);
                }
            }
            _ => {}
        }
        k += 1;
    }
    k
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

const PRIMITIVE_TYPES: &[&str] = &[
    "int", "integer", "string", "bool", "boolean", "float", "double", "array", "mixed", "iterable", "object",
    "callable", "void", "null", "false", "true", "never", "list", "non-empty-array", "non-empty-list",
    "non-empty-string", "positive-int", "negative-int", "class-string", "resource", "scalar", "numeric",
    "array-key", "callable-string",
];

/// A type reduced to what navigation needs: the class of the value, and
/// the class of its elements (`Foo[]`, `array<int, Foo>`, `Collection<Foo>`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ty {
    pub class: Option<String>,
    pub elem: Option<String>,
}

/// Parses a written type; `resolve` turns a class name as written into a
/// fully qualified one.
pub fn parse_type(written: &str, resolve: &dyn Fn(&str) -> String) -> Ty {
    let mut ty = Ty::default();
    for part in split_top(written.trim().trim_start_matches('?'), &['|', '&']) {
        let part = part.trim().trim_start_matches('?').trim_start_matches('(').trim_end_matches(')').trim();
        if part.is_empty() {
            continue;
        }
        if let Some(inner) = part.strip_suffix("[]") {
            if ty.elem.is_none() {
                ty.elem = parse_type(inner, resolve).class;
            }
            continue;
        }
        let (base, generic) = match part.find('<') {
            Some(lt) if part.ends_with('>') => (&part[..lt], Some(&part[lt + 1..part.len() - 1])),
            _ => (part, None),
        };
        let base_lower = base.to_ascii_lowercase();
        if let Some(args) = generic
            && ty.elem.is_none()
        {
            let last = split_top(args, &[',']).pop().unwrap_or_default();
            ty.elem = parse_type(last, resolve).class;
        }
        if ty.class.is_none() && !PRIMITIVE_TYPES.contains(&base_lower.as_str()) && !base.starts_with(['\'', '"', '{', '$']) && base.bytes().next().is_some_and(|b| is_ident_start(b) || b == b'\\') {
            ty.class = Some(resolve(base));
        }
    }
    ty
}

/// `s` split on `seps` outside `<…>`, `{…}`, `(…)`.
fn split_top<'a>(s: &'a str, seps: &[char]) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '{' | '(' => depth += 1,
            '>' | '}' | ')' => depth -= 1,
            c if depth == 0 && seps.contains(&c) => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

// ---------------------------------------------------------------------------
// Finding classes
// ---------------------------------------------------------------------------

/// Composer's autoload maps of a project (`vendor/composer/autoload_*.php`,
/// or `composer.json`'s `psr-4` when nothing is installed).
#[derive(Clone, Debug, Default)]
pub struct ClassIndex {
    /// Namespace prefix (`App\`) → directories.
    pub psr4: Vec<(String, Vec<String>)>,
    /// Class → file, loaded on first need (it can be large).
    classmap: std::cell::OnceCell<HashMap<String, String>>,
    root: String,
}

/// `'App\\Model\\' => array($baseDir . '/app/Model', …)` → strings and paths of one line.
fn autoload_line(line: &str, root: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    let key_start = line.find('\'')? + 1;
    let key_end = key_start + line[key_start..].find('\'')?;
    let key = line[key_start..key_end].replace("\\\\", "\\");
    let mut paths = Vec::new();
    let mut rest = &line[key_end..];
    while let Some(p) = rest.find("Dir . '") {
        let var_start = rest[..p].rfind('$').unwrap_or(p);
        let base = match &rest[var_start..p] {
            "$vendor" => format!("{root}/vendor"),
            _ => root.to_owned(),
        };
        let value_start = p + "Dir . '".len();
        let Some(len) = rest[value_start..].find('\'') else { break };
        paths.push(normalize(&format!("{base}{}", &rest[value_start..value_start + len])));
        rest = &rest[value_start + len..];
    }
    (!paths.is_empty()).then_some((key, paths))
}

impl ClassIndex {
    pub fn load(files: &dyn Files) -> Self {
        let root = files.root().to_owned();
        let mut psr4: Vec<(String, Vec<String>)> = files
            .read(&format!("{root}/vendor/composer/autoload_psr4.php"))
            .map(|text| text.lines().filter_map(|l| autoload_line(l, &root)).collect())
            .unwrap_or_default();
        if psr4.is_empty()
            && let Some(json) = files.read(&format!("{root}/composer.json"))
        {
            psr4 = composer_json_psr4(&json, &root);
        }
        psr4.sort_by_key(|(prefix, _)| std::cmp::Reverse(prefix.len()));
        Self { psr4, classmap: std::cell::OnceCell::new(), root }
    }

    fn classmap(&self, files: &dyn Files) -> &HashMap<String, String> {
        self.classmap.get_or_init(|| {
            files
                .read(&format!("{}/vendor/composer/autoload_classmap.php", self.root))
                .map(|text| {
                    text.lines()
                        .filter_map(|l| autoload_line(l, &self.root))
                        .filter_map(|(k, mut v)| Some((k.to_ascii_lowercase(), v.pop()?)))
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// The file declaring `fqcn` (PSR-4, then the class map).
    pub fn locate(&self, files: &dyn Files, fqcn: &str) -> Option<String> {
        let fqcn = fqcn.trim_start_matches('\\');
        for (prefix, dirs) in &self.psr4 {
            let Some(rest) = fqcn.strip_prefix(prefix.as_str()) else { continue };
            for dir in dirs {
                let path = format!("{dir}/{}.php", rest.replace('\\', "/"));
                if files.exists(&path) {
                    return Some(path);
                }
            }
        }
        self.classmap(files).get(&fqcn.to_ascii_lowercase()).cloned()
    }

    /// Classes of the class map named `short` (any namespace).
    pub fn by_short_name(&self, files: &dyn Files, short: &str) -> Vec<(String, String)> {
        let suffix = format!("\\{}", short.to_ascii_lowercase());
        let mut found: Vec<(String, String)> = self
            .classmap(files)
            .iter()
            .filter(|(k, _)| k.ends_with(&suffix) || **k == short.to_ascii_lowercase())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        found.sort();
        found
    }
}

/// `"psr-4": {"App\\": "app/", "X\\": ["a/", "b/"]}` of `composer.json`
/// (`autoload` and `autoload-dev`), without a JSON parser.
fn composer_json_psr4(json: &str, root: &str) -> Vec<(String, Vec<String>)> {
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(p) = rest.find("\"psr-4\"") {
        rest = &rest[p + 7..];
        let Some(open) = rest.find('{') else { break };
        let Some(close) = rest[open..].find('}') else { break };
        let body = &rest[open + 1..open + close];
        // "Key": "dir" | ["dir", …]
        let strings: Vec<(usize, &str)> = {
            let mut v = Vec::new();
            let mut i = 0;
            let b = body.as_bytes();
            while i < b.len() {
                if b[i] == b'"' {
                    let start = i + 1;
                    let mut j = start;
                    while j < b.len() && b[j] != b'"' {
                        j += if b[j] == b'\\' { 2 } else { 1 };
                    }
                    v.push((start, &body[start..j.min(b.len())]));
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
            v
        };
        let mut current: Option<(String, Vec<String>)> = None;
        for (at, s) in strings {
            let after = body[at + s.len()..].trim_start_matches('"').trim_start();
            if after.starts_with(':') {
                out.extend(current.take());
                current = Some((s.replace("\\\\", "\\"), Vec::new()));
            } else if let Some((_, dirs)) = current.as_mut() {
                dirs.push(normalize(&format!("{root}/{s}")));
            }
        }
        out.extend(current.take());
        rest = &rest[open + close..];
    }
    out.retain(|(_, dirs)| !dirs.is_empty());
    out
}

/// The file declaring the class `fqcn`, parsed: Composer's maps first, then
/// `**/<Short>.php` files of the project declaring it.
pub fn find_class(files: &dyn Files, index: &ClassIndex, fqcn: &str) -> Option<PhpFile> {
    let fqcn = fqcn.trim_start_matches('\\');
    let matches = |f: &PhpFile| f.fqcn().is_some_and(|n| n.eq_ignore_ascii_case(fqcn));
    if let Some(path) = index.locate(files, fqcn)
        && let Some(text) = files.read(&path)
    {
        let file = PhpFile::parse(&path, text);
        if matches(&file) {
            return Some(file);
        }
    }
    let short = fqcn.rsplit('\\').next().unwrap_or(fqcn);
    files
        .glob(&format!("**/{short}.php"), 20)
        .into_iter()
        .filter_map(|p| files.read(&p).map(|t| PhpFile::parse(&p, t)))
        .find(matches)
}

/// A class known only by its short name (as written in a template, where
/// there are no imports): the class map, then project files.
pub fn find_class_by_short_name(files: &dyn Files, index: &ClassIndex, short: &str) -> Option<PhpFile> {
    for (_, path) in index.by_short_name(files, short) {
        if let Some(text) = files.read(&path) {
            let file = PhpFile::parse(&path, text);
            if file.class.as_ref().is_some_and(|c| c.name.eq_ignore_ascii_case(short)) {
                return Some(file);
            }
        }
    }
    files
        .glob(&format!("**/{short}.php"), 20)
        .into_iter()
        .filter_map(|p| files.read(&p).map(|t| PhpFile::parse(&p, t)))
        .find(|f| f.class.as_ref().is_some_and(|c| c.name.eq_ignore_ascii_case(short)))
}

/// A class name from a template: qualified → [`find_class`], short → by short name.
pub fn find_template_class(files: &dyn Files, index: &ClassIndex, name: &str) -> Option<PhpFile> {
    let name = name.trim_start_matches('\\');
    if name.contains('\\') { find_class(files, index, name) } else { find_class_by_short_name(files, index, name) }
}

/// A member found in a class hierarchy, with its type resolved in the
/// declaring file (`static` / `self` stay the class it was looked up on).
pub struct Found {
    pub location: Location,
    pub ty: Ty,
}

/// `name` of `kind` in the class of `file` or the classes, interfaces and
/// traits it inherits from. Methods ignore case; properties and constants don't.
pub fn find_member(files: &dyn Files, index: &ClassIndex, file: PhpFile, name: &str, kind: MemberKind) -> Option<Found> {
    let receiver = file.fqcn();
    let mut queue = std::collections::VecDeque::from([file]);
    let mut seen: Vec<String> = Vec::new();
    while let Some(current) = queue.pop_front() {
        if seen.len() > MAX_HIERARCHY {
            break;
        }
        let Some(class) = current.class.as_ref() else { continue };
        let fqcn = current.fqcn().unwrap_or_default().to_ascii_lowercase();
        if seen.contains(&fqcn) {
            continue;
        }
        seen.push(fqcn);
        let member = class.members.iter().find(|m| {
            m.kind == kind && if kind == MemberKind::Method { m.name.eq_ignore_ascii_case(name) } else { m.name == name }
        });
        if let Some(m) = member {
            let resolve = |n: &str| match n.to_ascii_lowercase().as_str() {
                "static" | "self" | "$this" => receiver.clone().unwrap_or_else(|| current.resolve(n)),
                _ => current.resolve(n),
            };
            let ty = m.best_type().map(|t| parse_type(t, &resolve)).unwrap_or_default();
            return Some(Found { location: current.location(&m.span), ty });
        }
        // Breadth: traits first (they are part of the class), then parents and interfaces.
        let related: Vec<String> = class
            .traits
            .iter()
            .chain(&class.extends)
            .chain(&class.implements)
            .map(|n| current.resolve(n))
            .collect();
        for parent in related {
            if let Some(f) = find_class(files, index, &parent) {
                queue.push_back(f);
            }
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::navigation::tests::MapFiles;

    pub(crate) const USER: &str = r#"<?php
declare(strict_types=1);

namespace App\Model;

use Nette\Database\Table\ActiveRow;
use App\Model\Orders\{Order, OrderList as Orders};

/**
 * @property-read string $email
 * @method Address getAddress()
 */
#[Attribute]
final class User extends Entity implements Identity
{
    use Timestamps;

    public const ROLE_ADMIN = 'admin', ROLE_USER = 'user';

    /** @var Order[] */
    private array $orders = [];

    public ?string $name = null;

    public function __construct(
        private readonly int $id,
        public Address $home,
        string $plain,
    ) {}

    public function getName(): string
    {
        $x = function () { return 1; };
        return $this->name;
    }

    /** @return Order[] */
    public function getOrders(): array { return $this->orders; }

    public function getLatest(): ?Order {}

    public static function create(): static {}

    public function list(): Orders {}
}
"#;

    #[test]
    fn parses_declarations() {
        let f = PhpFile::parse("/p/app/Model/User.php", USER.to_owned());
        assert_eq!(f.namespace, "App\\Model");
        assert_eq!(f.fqcn().as_deref(), Some("App\\Model\\User"));
        assert_eq!(f.resolve("ActiveRow"), "Nette\\Database\\Table\\ActiveRow");
        assert_eq!(f.resolve("Order"), "App\\Model\\Orders\\Order");
        assert_eq!(f.resolve("Orders"), "App\\Model\\Orders\\OrderList");
        assert_eq!(f.resolve("Address"), "App\\Model\\Address");
        assert_eq!(f.resolve("\\DateTime"), "DateTime");

        let c = f.class.as_ref().unwrap();
        assert_eq!((c.extends.as_slice(), c.implements.as_slice(), c.traits.as_slice()), (&["Entity".to_owned()][..], &["Identity".to_owned()][..], &["Timestamps".to_owned()][..]));
        let members: Vec<(&str, MemberKind, Option<&str>)> =
            c.members.iter().map(|m| (m.name.as_str(), m.kind, m.best_type())).collect();
        use MemberKind::*;
        assert_eq!(
            members,
            [
                ("email", Property, Some("string")),
                ("getAddress", Method, Some("Address")),
                ("ROLE_ADMIN", Constant, None),
                ("ROLE_USER", Constant, None),
                ("orders", Property, Some("Order[]")),
                ("name", Property, Some("?string")),
                ("id", Property, Some("int")),
                ("home", Property, Some("Address")),
                ("__construct", Method, None),
                ("getName", Method, Some("string")),
                ("getOrders", Method, Some("Order[]")),
                ("getLatest", Method, Some("?Order")),
                ("create", Method, Some("static")),
                ("list", Method, Some("Orders")),
            ]
        );
        let email = &c.members[0];
        assert_eq!(&USER[email.span.clone()], "$email");
        let addr = &c.members[1];
        assert_eq!(&USER[addr.span.clone()], "getAddress");
    }

    #[test]
    fn parses_types() {
        let r = |n: &str| format!("App\\{}", n.trim_start_matches('\\'));
        let t = |s: &str| parse_type(s, &r);
        assert_eq!(t("?Foo"), Ty { class: Some("App\\Foo".into()), elem: None });
        assert_eq!(t("Foo[]"), Ty { class: None, elem: Some("App\\Foo".into()) });
        assert_eq!(t("array<int, Foo>"), Ty { class: None, elem: Some("App\\Foo".into()) });
        assert_eq!(t("Collection<int, Foo>|null"), Ty { class: Some("App\\Collection".into()), elem: Some("App\\Foo".into()) });
        assert_eq!(t("int|string"), Ty::default());
        assert_eq!(t("list<Foo>"), Ty { class: None, elem: Some("App\\Foo".into()) });
    }

    #[test]
    fn reads_composer_maps() {
        let files = MapFiles::new(&[
            ("/p/vendor/composer/autoload_psr4.php", "<?php\nreturn array(\n    'Nette\\\\' => array($vendorDir . '/nette/utils/src', $vendorDir . '/nette/forms/src'),\n    'App\\\\' => array($baseDir . '/app'),\n);\n"),
            ("/p/vendor/composer/autoload_classmap.php", "<?php\nreturn array(\n    'Latte\\\\Engine' => $vendorDir . '/latte/latte/src/Latte/Engine.php',\n);\n"),
            ("/p/app/Model/User.php", USER),
            ("/p/vendor/latte/latte/src/Latte/Engine.php", "<?php namespace Latte; class Engine {}"),
            ("/p/vendor/nette/forms/src/Forms/Form.php", "<?php namespace Nette\\Forms; class Form {}"),
        ]);
        let index = ClassIndex::load(&files);
        assert_eq!(index.locate(&files, "App\\Model\\User").as_deref(), Some("/p/app/Model/User.php"));
        assert_eq!(index.locate(&files, "Nette\\Forms\\Form").as_deref(), Some("/p/vendor/nette/forms/src/Forms/Form.php"));
        assert_eq!(index.locate(&files, "Latte\\Engine").as_deref(), Some("/p/vendor/latte/latte/src/Latte/Engine.php"));
        assert_eq!(find_template_class(&files, &index, "Engine").unwrap().path, "/p/vendor/latte/latte/src/Latte/Engine.php");

        let json = MapFiles::new(&[("/p/composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/", "Lib\\": ["lib/", "src/"]}}}"#)]);
        let index = ClassIndex::load(&json);
        assert_eq!(index.psr4.len(), 2);
        assert!(index.psr4.iter().any(|(p, d)| p == "Lib\\" && d == &["/p/lib", "/p/src"]));
    }

    #[test]
    fn finds_members_through_the_hierarchy() {
        let files = MapFiles::new(&[
            ("/p/composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#),
            ("/p/app/Model/User.php", USER),
            ("/p/app/Model/Entity.php", "<?php namespace App\\Model; abstract class Entity { public function getId(): int {} }"),
            ("/p/app/Model/Timestamps.php", "<?php namespace App\\Model; trait Timestamps { public ?\\DateTime $created; }"),
            ("/p/app/Model/Identity.php", "<?php namespace App\\Model; interface Identity { function getRoles(): array; }"),
        ]);
        let index = ClassIndex::load(&files);
        let user = || find_class(&files, &index, "App\\Model\\User").unwrap();
        let get = |name: &str, kind| find_member(&files, &index, user(), name, kind);

        let latest = get("getlatest", MemberKind::Method).unwrap();
        assert_eq!(latest.ty.class.as_deref(), Some("App\\Model\\Orders\\Order"));
        assert_eq!(get("getOrders", MemberKind::Method).unwrap().ty.elem.as_deref(), Some("App\\Model\\Orders\\Order"));
        assert_eq!(get("create", MemberKind::Method).unwrap().ty.class.as_deref(), Some("App\\Model\\User"));
        assert_eq!(get("getId", MemberKind::Method).unwrap().location.path, "/p/app/Model/Entity.php");
        let created = get("created", MemberKind::Property).unwrap();
        assert_eq!((created.location.path.as_str(), created.ty.class.as_deref()), ("/p/app/Model/Timestamps.php", Some("DateTime")));
        assert_eq!(get("getRoles", MemberKind::Method).unwrap().location.path, "/p/app/Model/Identity.php");
        assert!(get("Name", MemberKind::Property).is_none(), "properties are case-sensitive");
        assert!(get("nope", MemberKind::Method).is_none());
    }
}
