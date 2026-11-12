//! Go to Definition for the PHP inside templates:
//!
//! - classes: `{varType App\Model\User $u}`, `new Foo`, `Foo::BAR`, `instanceof Foo`;
//! - static members: `Foo::create()`, `Foo::BAR`, `Foo::$cache`, `Foo::class`;
//! - members of typed variables, along chains: `$user->getAddress()->city`,
//!   `$items[0]->name`; a property missing in the class falls back to its
//!   getter (`$user->name` → `getName()`), as Nette's `SmartObject` does;
//! - variables, to where the template declares them.
//!
//! Variable types come from the template: `{varType}`, `{var}`, `{default}`,
//! `{parameters}`, `{templateType}` (its class's properties), `{foreach … as
//! $item}` / `n:foreach` (element types), `{var $x = <expression>}`, and
//! Nette's `$presenter` / `$control` (the template's presenter) and `$user`.

use crate::navigation::{Files, Template};
use crate::nette;
use crate::php::{self, ClassIndex, Found, MemberKind, PhpFile, Ty};
use openidea_plugin::{Location, Range};
use std::ops::Range as Span;

/// How deep variable types may refer to other variables.
const MAX_DEPTH: usize = 6;

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// What a definition request works with.
pub struct Ctx<'a> {
    pub files: &'a dyn Files,
    pub index: &'a ClassIndex,
    pub path: &'a str,
    pub text: &'a str,
    pub template: &'a Template,
}

/// How a chain of accesses starts.
#[derive(Debug, PartialEq, Eq)]
enum Start {
    Var(String),
    StaticCall(String, String),
    StaticProp(String, String),
    New(String),
}

#[derive(Debug, PartialEq, Eq)]
enum Seg {
    Prop(String),
    Call(String),
    Index,
}

#[derive(Debug, PartialEq, Eq)]
struct Chain {
    start: Start,
    segs: Vec<Seg>,
}

/// `s` without trailing whitespace, and its new end.
fn trim_end_at(text: &str, from: usize, end: usize) -> usize {
    from + text[from..end].trim_end().len()
}

/// An identifier ending at `end` (exclusive): its start.
fn ident_back(text: &str, from: usize, end: usize) -> usize {
    let b = text.as_bytes();
    let mut i = end;
    while i > from && (is_ident(b[i - 1]) || b[i - 1] == b'\\') {
        i -= 1;
    }
    i
}

/// The `open` bracket matching the `close` one at `end - 1`.
fn match_back(text: &str, from: usize, end: usize, open: u8, close: u8) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0usize;
    let mut i = end;
    while i > from {
        i -= 1;
        if b[i] == close {
            depth += 1;
        } else if b[i] == open {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// The access chain whose last part ends at `end` (exclusive), within `from..`.
fn parse_chain(text: &str, from: usize, end: usize) -> Option<Chain> {
    let mut segs = Vec::new();
    let mut p = trim_end_at(text, from, end);
    loop {
        let b = text.as_bytes();
        if p <= from {
            return None;
        }
        match b[p - 1] {
            b')' => {
                let open = match_back(text, from, p, b'(', b')')?;
                let name_start = ident_back(text, from, open);
                let name = &text[name_start..open];
                if name.is_empty() {
                    // `(new Foo(…))->`
                    let inner = text[open + 1..p - 1].trim();
                    let class = inner.strip_prefix("new")?.trim_start();
                    let len = class.bytes().take_while(|c| is_ident(*c) || *c == b'\\').count();
                    segs.reverse();
                    return Some(Chain { start: Start::New(class[..len].to_owned()), segs });
                }
                let before = trim_end_at(text, from, name_start);
                if text[..before].ends_with("->") {
                    segs.push(Seg::Call(name.to_owned()));
                    p = trim_end_at(text, from, before - 2 - usize::from(text[..before - 2].ends_with('?')));
                } else if text[..before].ends_with("::") {
                    let class_start = ident_back(text, from, before - 2);
                    let class = text[class_start..before - 2].to_owned();
                    segs.reverse();
                    return Some(Chain { start: Start::StaticCall(class, name.to_owned()), segs });
                } else {
                    return None; // a function call
                }
            }
            b']' => {
                p = trim_end_at(text, from, match_back(text, from, p, b'[', b']')?);
                segs.push(Seg::Index);
            }
            c if is_ident(c) => {
                let start = ident_back(text, from, p);
                let name = text[start..p].to_owned();
                if start > from && b[start - 1] == b'$' {
                    let dollar = start - 1;
                    let before = trim_end_at(text, from, dollar);
                    segs.reverse();
                    if text[..before].ends_with("::") {
                        let class_start = ident_back(text, from, before - 2);
                        return Some(Chain { start: Start::StaticProp(text[class_start..before - 2].to_owned(), name), segs });
                    }
                    return Some(Chain { start: Start::Var(name), segs });
                }
                let before = trim_end_at(text, from, start);
                if text[..before].ends_with("->") {
                    segs.push(Seg::Prop(name));
                    p = trim_end_at(text, from, before - 2 - usize::from(text[..before - 2].ends_with('?')));
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

/// `$name` declared in a code region of a typed tag: `Type $name`, maybe `= value`.
struct Decl {
    /// Where `$name` is written.
    span: Span<usize>,
    ty: Option<String>,
    /// The value expression (`{var $x = …}`).
    value: Option<Span<usize>>,
}

/// Top-level parts of `span` separated by `,`.
fn split_args(text: &str, span: Span<usize>) -> Vec<Span<usize>> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = span.start;
    let mut i = span.start;
    while i < span.end {
        match b[i] {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => depth -= 1,
            b'\'' | b'"' => {
                let q = b[i];
                i += 1;
                while i < span.end && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
            }
            b',' if depth <= 0 => {
                out.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(start..span.end);
    out
}

/// `[Type] $name [= value]` declarations of a `{var}`-like tag.
fn declarations(text: &str, args: Span<usize>) -> Vec<(String, Decl)> {
    split_args(text, args)
        .into_iter()
        .filter_map(|part| {
            let s = &text[part.clone()];
            let dollar = s.find('$')?;
            let name_len = s[dollar + 1..].bytes().take_while(|b| is_ident(*b)).count();
            if name_len == 0 {
                return None;
            }
            let ty = s[..dollar].trim();
            if ty.contains(['=', '(']) {
                return None; // `$a = $b` style: not a declaration of $b
            }
            let name = s[dollar + 1..dollar + 1 + name_len].to_owned();
            let after = &s[dollar + 1 + name_len..];
            let value = after.trim_start().strip_prefix('=').filter(|v| !v.starts_with('=')).map(|v| {
                let start = part.end - v.len();
                start..part.end
            });
            let at = part.start + dollar;
            Some((name, Decl { span: at..at + 1 + name_len, ty: (!ty.is_empty()).then(|| ty.to_owned()), value }))
        })
        .collect()
}

/// `{foreach EXPR as [$k =>] $v}` → (EXPR, `$v` span, `$k` span).
fn foreach_parts(text: &str, args: Span<usize>) -> Option<(Span<usize>, Span<usize>, Option<Span<usize>>)> {
    let s = &text[args.clone()];
    let as_at = s.rfind(" as ")?;
    let expr = args.start..args.start + as_at;
    let vars = &s[as_at + 4..];
    let mut spans = Vec::new();
    let mut i = 0;
    let b = vars.as_bytes();
    while i < b.len() {
        if b[i] == b'$' {
            let len = b[i + 1..].iter().take_while(|c| is_ident(**c)).count();
            let at = args.start + as_at + 4 + i;
            spans.push(at..at + 1 + len);
            i += len;
        }
        i += 1;
    }
    match spans.len() {
        1 => Some((expr, spans.remove(0), None)),
        2 => {
            let v = spans.remove(1);
            Some((expr, v, Some(spans.remove(0))))
        }
        _ => None,
    }
}

impl Ctx<'_> {
    fn class(&self, name: &str) -> Option<PhpFile> {
        php::find_template_class(self.files, self.index, name)
    }

    fn member(&self, class: &str, name: &str, kind: MemberKind) -> Option<Found> {
        let file = self.class(class)?;
        php::find_member(self.files, self.index, file, name, kind)
    }

    /// A property, or (Nette `SmartObject`) its getter.
    fn property(&self, class: &str, name: &str) -> Option<Found> {
        self.member(class, name, MemberKind::Property).or_else(|| {
            let mut chars = name.chars();
            let up: String = chars.next()?.to_uppercase().chain(chars).collect();
            self.member(class, &format!("get{up}"), MemberKind::Method)
                .or_else(|| self.member(class, &format!("is{up}"), MemberKind::Method))
        })
    }

    fn template_type(&self, written: &str) -> Ty {
        php::parse_type(written, &|n: &str| n.trim_start_matches('\\').to_owned())
    }

    /// Where `$name` is declared, and its type, as seen at `offset`.
    fn variable(&self, name: &str, offset: usize, depth: usize) -> Option<(Option<Location>, Ty)> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut best: Option<(usize, Option<Location>, Ty)> = None;
        let mut consider = |at: usize, loc: Option<Location>, ty: Ty| {
            // The last declaration before the use wins; else the first one after.
            let better = match &best {
                None => true,
                Some((prev, ..)) => (at <= offset && (*prev > offset || at > *prev)) || (at > offset && *prev > offset && at < *prev),
            };
            if better {
                best = Some((at, loc, ty));
            }
        };
        let here = |span: &Span<usize>| crate::navigation::location(self.path, self.text, span);

        for (tag, args) in &self.template.code {
            match tag.as_str() {
                "varType" | "var" | "default" | "parameters" => {
                    for (n, decl) in declarations(self.text, args.clone()) {
                        if n != name {
                            continue;
                        }
                        let ty = match (&decl.ty, &decl.value) {
                            (Some(t), _) => self.template_type(t),
                            (None, Some(v)) => self.expr_type(v.clone(), offset, depth + 1),
                            _ => Ty::default(),
                        };
                        consider(decl.span.start, Some(here(&decl.span)), ty);
                    }
                }
                "templateType" => {
                    let class = self.text[args.clone()].trim();
                    if let Some(found) = self.property(class, name) {
                        consider(args.start, Some(found.location), found.ty);
                    }
                }
                "foreach" => {
                    let Some((expr, value, key)) = foreach_parts(self.text, args.clone()) else { continue };
                    if &self.text[value.start + 1..value.end] == name {
                        let elem = self.expr_type(expr, offset, depth + 1).elem;
                        consider(value.start, Some(here(&value)), Ty { class: elem, elem: None });
                    } else if let Some(key) = key.filter(|k| &self.text[k.start + 1..k.end] == name) {
                        consider(key.start, Some(here(&key)), Ty::default());
                    }
                }
                _ => {}
            }
        }
        if let Some((_, loc, ty)) = best {
            return Some((loc, ty));
        }
        // Variables Nette and Latte provide.
        let class = match name {
            "presenter" | "control" => {
                let presenter = nette::presenter_of_template(self.files, self.path)?;
                PhpFile::parse(&presenter.path, self.files.read(&presenter.path)?).fqcn()?
            }
            "user" => "Nette\\Security\\User".to_owned(),
            "iterator" => "Latte\\Essential\\CachingIterator".to_owned(),
            _ => return None,
        };
        Some((None, Ty { class: Some(class), elem: None }))
    }

    /// The type of an expression (a chain, `new Foo`), as seen at `offset`.
    fn expr_type(&self, span: Span<usize>, offset: usize, depth: usize) -> Ty {
        let s = self.text[span.clone()].trim();
        if let Some(class) = s.strip_prefix("new ") {
            let len = class.trim_start().bytes().take_while(|c| is_ident(*c) || *c == b'\\').count();
            return Ty { class: Some(class.trim_start()[..len].to_owned()), elem: None };
        }
        // Up to the first filter or operator at the end: `$a->b() ?? null` stays `$a->b()`.
        let end = span.start + self.text[span.clone()].trim_end().len();
        let end = self.text[span.start..end].find(['|', '?']).filter(|p| !self.text[span.start + p..].starts_with("?->")).map_or(end, |p| span.start + p);
        self.chain_type(span.start, end, offset, depth).map(|(ty, _)| ty).unwrap_or_default()
    }

    /// The type of the chain ending at `end`, and the location of its last part.
    fn chain_type(&self, from: usize, end: usize, offset: usize, depth: usize) -> Option<(Ty, Option<Location>)> {
        let chain = parse_chain(self.text, from, end)?;
        let (mut ty, mut loc) = match &chain.start {
            Start::Var(name) => {
                let (loc, ty) = self.variable(name, offset, depth)?;
                (ty, loc)
            }
            Start::StaticCall(class, method) => {
                let f = self.member(class, method, MemberKind::Method)?;
                (f.ty, Some(f.location))
            }
            Start::StaticProp(class, prop) => {
                let f = self.member(class, prop, MemberKind::Property)?;
                (f.ty, Some(f.location))
            }
            Start::New(class) => (Ty { class: Some(class.clone()), elem: None }, None),
        };
        for seg in &chain.segs {
            let found = match seg {
                Seg::Index => {
                    ty = Ty { class: ty.elem.take(), elem: None };
                    loc = None;
                    continue;
                }
                Seg::Prop(name) => self.property(ty.class.as_deref()?, name)?,
                Seg::Call(name) => self.member(ty.class.as_deref()?, name, MemberKind::Method)?,
            };
            ty = found.ty;
            loc = Some(found.location);
        }
        Some((ty, loc))
    }

    /// Go to Definition at `offset` inside one of the template's code regions.
    pub fn definition(&self, offset: usize) -> Option<Location> {
        let (tag, region) = self.template.code.iter().find(|(_, s)| s.start <= offset && offset <= s.end)?;
        let b = self.text.as_bytes();
        // The word under the caret.
        let mut start = offset.min(region.end);
        while start > region.start && (is_ident(b[start - 1]) || b[start - 1] == b'\\') {
            start -= 1;
        }
        let mut end = offset.max(start);
        while end < region.end && is_ident(b[end]) {
            end += 1;
        }
        if start > region.start && b[start - 1] == b'$' {
            start -= 1;
        } else if b.get(start) == Some(&b'$') {
            end = start + 1 + b[start + 1..region.end].iter().take_while(|c| is_ident(**c)).count();
        }
        let word = &self.text[start..end];
        if word.is_empty() || word == "$" {
            return None;
        }
        let before = trim_end_at(self.text, region.start, start);
        let prefix = &self.text[region.start..before];
        let next = self.text[end..region.end].trim_start();
        let call = next.starts_with('(');

        if let Some(var) = word.strip_prefix('$') {
            if prefix.ends_with("::") {
                let class_start = ident_back(self.text, region.start, before - 2);
                return self.member(&self.text[class_start..before - 2], var, MemberKind::Property).map(|f| f.location);
            }
            return self.variable(var, start, 0).and_then(|(loc, ty)| loc.or_else(|| self.class_location(ty.class.as_deref()?)));
        }
        if prefix.ends_with("->") {
            let receiver_end = before - 2 - usize::from(prefix[..prefix.len() - 2].ends_with('?'));
            let (ty, _) = self.chain_type(region.start, receiver_end, start, 0)?;
            let class = ty.class?;
            let found = if call { self.member(&class, word, MemberKind::Method) } else { self.property(&class, word) };
            return found.map(|f| f.location);
        }
        if prefix.ends_with("::") {
            let class_start = ident_back(self.text, region.start, before - 2);
            let class = &self.text[class_start..before - 2];
            if word.eq_ignore_ascii_case("class") {
                return self.class_location(class);
            }
            let kind = if call { MemberKind::Method } else { MemberKind::Constant };
            return self.member(class, word, kind).map(|f| f.location);
        }
        let lower = word.to_ascii_lowercase();
        let typed_tag = matches!(tag.as_str(), "varType" | "var" | "default" | "parameters" | "templateType" | "define");
        let looks_like_class = word.contains('\\')
            || next.starts_with("::")
            || prefix.ends_with("new")
            || prefix.ends_with("instanceof")
            || (typed_tag && !call)
            || word.starts_with(char::is_uppercase);
        if looks_like_class && !call && !php_keyword(&lower) {
            return self.class_location(word);
        }
        None
    }

    fn class_location(&self, name: &str) -> Option<Location> {
        let file = self.class(name)?;
        Some(match &file.class {
            Some(c) => file.location(&c.span),
            None => Location { path: file.path.clone(), range: Range::default() },
        })
    }
}

fn php_keyword(word: &str) -> bool {
    matches!(
        word,
        "int" | "integer" | "string" | "bool" | "boolean" | "float" | "double" | "array" | "mixed" | "iterable"
            | "object" | "callable" | "void" | "null" | "false" | "true" | "self" | "static" | "never" | "list"
            | "new" | "instanceof" | "as" | "and" | "or" | "xor" | "fn" | "match" | "clone"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{self, tests::MapFiles};
    use crate::php::tests::USER;
    use openidea_plugin::providers::Document;
    use openidea_plugin::{text, Position};

    #[test]
    fn parses_chains() {
        let c = |s: &str| parse_chain(s, 0, s.len());
        assert_eq!(
            c("$user->getAddress()->city"),
            Some(Chain { start: Start::Var("user".into()), segs: vec![Seg::Call("getAddress".into()), Seg::Prop("city".into())] })
        );
        assert_eq!(c("$items[0]?->name"), Some(Chain { start: Start::Var("items".into()), segs: vec![Seg::Index, Seg::Prop("name".into())] }));
        assert_eq!(c("User::create($a, $b)->x"), Some(Chain { start: Start::StaticCall("User".into(), "create".into()), segs: vec![Seg::Prop("x".into())] }));
        assert_eq!(c("(new Foo\\Bar)->x"), Some(Chain { start: Start::New("Foo\\Bar".into()), segs: vec![Seg::Prop("x".into())] }));
        assert_eq!(c("Foo::$cache"), Some(Chain { start: Start::StaticProp("Foo".into(), "cache".into()), segs: vec![] }));
        assert_eq!(c("count($x)"), None);
    }

    fn project() -> MapFiles {
        MapFiles::new(&[
            ("/p/composer.json", r#"{"autoload": {"psr-4": {"App\\": "app/"}}}"#),
            ("/p/app/Model/User.php", USER),
            ("/p/app/Model/Entity.php", "<?php namespace App\\Model; abstract class Entity { public function getId(): int {} }"),
            ("/p/app/Model/Address.php", "<?php namespace App\\Model; class Address { public string $city; public function getStreet(): string {} }"),
            ("/p/app/Model/Orders/Order.php", "<?php namespace App\\Model\\Orders;\nclass Order {\n    public const STATE_NEW = 1;\n    public float $total;\n    public function getUser(): \\App\\Model\\User {}\n}"),
            ("/p/app/UI/Home/HomePresenter.php", "<?php namespace App\\UI\\Home; class HomePresenter { public function getCart(): \\App\\Model\\Orders\\Order {} }"),
            ("/p/app/UI/Home/HomeTemplate.php", "<?php namespace App\\UI\\Home;\nuse App\\Model\\User;\nclass HomeTemplate { public User $viewer; /** @var \\App\\Model\\Orders\\Order[] */ public array $recent; }"),
            ("/p/app/UI/Home/default.latte", ""),
        ])
    }

    /// Definition at the first `needle` (+`delta`) of `src`, as (file name, text at the target).
    fn go(files: &MapFiles, src: &str, needle: &str, delta: usize) -> Option<(String, String)> {
        let d = Document { path: "/p/app/UI/Home/default.latte".into(), language: "latte".into(), version: 1, text: src.into() };
        let at = text::position(src, src.find(needle).unwrap_or_else(|| panic!("{needle:?}")) + delta);
        let loc = navigation::definition(Some(files), &d, at).into_iter().next()?;
        let target = if loc.path == d.path { src.to_owned() } else { files.read(&loc.path)? };
        let from = text::offset(&target, loc.range.start).unwrap();
        let to = text::offset(&target, loc.range.end).unwrap();
        Some((loc.path.rsplit('/').next().unwrap().to_owned(), target[from..to].to_owned()))
    }

    fn at(file: &str, text: &str) -> Option<(String, String)> {
        Some((file.to_owned(), text.to_owned()))
    }

    #[test]
    fn goes_to_classes_and_static_members() {
        let files = project();
        let src = r"{varType App\Model\User $u}{if $x instanceof Address}{/if}{=App\Model\Orders\Order::STATE_NEW}{=User::create()}{=User::class}{=Order::$nope}";
        assert_eq!(go(&files, src, r"Model\User", 7), at("User.php", "User"));
        assert_eq!(go(&files, src, "Address", 2), at("Address.php", "Address"));
        assert_eq!(go(&files, src, "STATE_NEW", 1), at("Order.php", "STATE_NEW"));
        assert_eq!(go(&files, src, r"Orders\Order::", 8), at("Order.php", "Order"));
        assert_eq!(go(&files, src, "create", 1), at("User.php", "create"));
        assert_eq!(go(&files, src, "class}", 1), at("User.php", "User"));
        assert_eq!(go(&files, src, "$nope", 2), None);
    }

    #[test]
    fn goes_to_members_of_typed_variables() {
        let files = project();
        let src = r"{varType App\Model\User $u}
{$u->getAddress()->city}
{$u->name} {$u->home->getStreet()} {$u->getId()} {$u->email}
{foreach $u->getOrders() as $order}{$order->total}{/foreach}
{var $latest = $u->getLatest()}{$latest->getUser()->getName()}
{$u->nope}";
        assert_eq!(go(&files, src, "getAddress", 2), at("User.php", "getAddress"));
        assert_eq!(go(&files, src, "city", 1), at("Address.php", "$city"));
        assert_eq!(go(&files, src, "name}", 1), at("User.php", "$name"));
        assert_eq!(go(&files, src, "getStreet", 1), at("Address.php", "getStreet"));
        assert_eq!(go(&files, src, "getId", 1), at("Entity.php", "getId"));
        assert_eq!(go(&files, src, "email", 1), at("User.php", "$email"), "@property-read");
        assert_eq!(go(&files, src, "total", 1), at("Order.php", "$total"), "foreach over Order[]");
        assert_eq!(go(&files, src, "getName()", 1), at("User.php", "getName"), "{{var}} typed by its value");
        assert_eq!(go(&files, src, "nope", 1), None);
    }

    #[test]
    fn getters_stand_in_for_properties() {
        let files = project();
        let src = r"{varType App\Model\Address $a}{$a->street}";
        assert_eq!(go(&files, src, "street", 1), at("Address.php", "getStreet"));
    }

    #[test]
    fn goes_to_variable_declarations() {
        let files = project();
        let src = "{parameters App\\Model\\User $u, int $n = 5}\n{$u->getId()} {$n}";
        let (file, text) = go(&files, src, "$u->", 1).unwrap();
        assert_eq!((file.as_str(), text.as_str()), ("default.latte", "$u"));
        assert_eq!(go(&files, src, "$n}", 1), at("default.latte", "$n"));
    }

    #[test]
    fn template_types_and_nette_variables() {
        let files = project();
        let src = r#"{templateType App\UI\Home\HomeTemplate}
{$viewer->getName()} {foreach $recent as $o}{$o->total}{/foreach}
<a n:if="$presenter->getCart()->total">x</a> {$user->isLoggedIn()}"#;
        assert_eq!(go(&files, src, "$viewer", 1), at("HomeTemplate.php", "$viewer"));
        assert_eq!(go(&files, src, "getName", 1), at("User.php", "getName"));
        assert_eq!(go(&files, src, "total}", 1), at("Order.php", "$total"));
        assert_eq!(go(&files, src, "getCart", 1), at("HomePresenter.php", "getCart"));
        assert_eq!(go(&files, src, "total\"", 1), at("Order.php", "$total"));
        assert!(go(&files, src, "isLoggedIn", 1).is_none(), "Nette\\Security\\User is not in this project");
    }

    #[test]
    fn survives_broken_code() {
        let files = project();
        for src in ["{$a->}", "{$->x}", "{=::x}", "{=Foo::}", "{var $x = $x}{$x->y}", "{foreach as $x}{$x->y}{/foreach}", "{=)->x}", "{$a[->b}"] {
            for i in 0..src.len() {
                let d = Document { path: "/p/app/UI/Home/default.latte".into(), language: "latte".into(), version: 1, text: src.into() };
                let _ = navigation::definition(Some(&files), &d, text::position(src, i));
            }
        }
        let _ = Position::default();
    }
}
