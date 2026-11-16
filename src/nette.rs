//! Nette Application conventions, following `Nette\Application\UI\Presenter`
//! (3.2) `formatTemplateFiles()` / `formatLayoutTemplateFiles()`:
//!
//! - presenter → its templates: `templates/<Presenter>/<view>.latte`,
//!   `templates/<Presenter>.<view>.latte` (in the presenter's directory or the
//!   one above), or `<view>.latte` next to the presenter;
//! - template → its presenter (the same rules backwards, confirmed forwards);
//! - the automatic `@layout.latte` of a presenter template;
//! - `{link}` / `n:href` destinations → `action<X>()` / `render<X>()` /
//!   `handle<X>()`, and `{control}` / `{form}` → `createComponent<X>()`.
//!
//! PHP is read with a few lexical patterns, not parsed: good enough to find
//! classes and methods, and wrong only for code that hides them in strings.

use crate::navigation::{dir_of, file_name, join, location, normalize, Files, Template};
use openidea_plugin::providers::{Document, LineMarker};
use openidea_plugin::text;
use openidea_plugin::{Location, Range};
use std::ops::Range as Span;

/// How many `extends` are followed to find an inherited method.
const MAX_PARENTS: usize = 6;
/// How many module levels `@layout.latte` is searched upwards.
const LAYOUT_LEVELS: usize = 3;

// ---------------------------------------------------------------------------
// PHP
// ---------------------------------------------------------------------------

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// The identifier starting at `at` (after whitespace), with its span.
fn ident_at(text: &str, mut at: usize) -> Option<(&str, Span<usize>)> {
    let bytes = text.as_bytes();
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    let start = at;
    while at < bytes.len() && (is_ident_byte(bytes[at]) || bytes[at] == b'\\') {
        at += 1;
    }
    (at > start && !bytes[start].is_ascii_digit()).then(|| (&text[start..at], start..at))
}

/// Offsets of the keyword `word` in `text` as a whole word followed by whitespace.
fn keyword_offsets<'a>(text: &'a str, word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let bytes = text.as_bytes();
    text.match_indices(word).map(|(i, _)| i).filter(move |&i| {
        let before_ok = i == 0 || !(is_ident_byte(bytes[i - 1]) || bytes[i - 1] == b'$' || bytes[i - 1] == b':');
        let after_ok = bytes.get(i + word.len()).is_some_and(|b| b.is_ascii_whitespace());
        before_ok && after_ok
    })
}

/// Methods declared in `text`: (name, span of the name).
pub fn php_methods(text: &str) -> Vec<(String, Span<usize>)> {
    let bytes = text.as_bytes();
    keyword_offsets(text, "function")
        .filter_map(|i| {
            let mut at = i + "function".len();
            while at < bytes.len() && (bytes[at].is_ascii_whitespace() || bytes[at] == b'&') {
                at += 1;
            }
            let (name, span) = ident_at(text, at)?;
            (!name.contains('\\')).then(|| (name.to_owned(), span))
        })
        .collect()
}

/// The span of the method `name` (PHP method names ignore case).
fn php_method(text: &str, name: &str) -> Option<Span<usize>> {
    php_methods(text).into_iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, s)| s)
}

/// The first class declared in `text`.
pub struct PhpClass {
    pub name: String,
    pub span: Span<usize>,
    /// Short name of the parent class (`Base` of `\App\UI\Base`).
    pub extends: Option<String>,
}

pub fn php_class(text: &str) -> Option<PhpClass> {
    keyword_offsets(text, "class").find_map(|i| {
        let (name, span) = ident_at(text, i + "class".len())?;
        if matches!(name, "extends" | "implements") || name.contains('\\') {
            return None; // `new class extends …`
        }
        let rest = &text[span.end..];
        let trimmed = rest.trim_start();
        let extends = trimmed
            .strip_prefix("extends")
            .filter(|r| r.starts_with(char::is_whitespace))
            .and_then(|r| ident_at(r, 0))
            .map(|(parent, _)| parent.rsplit('\\').next().unwrap_or(parent).to_owned());
        Some(PhpClass { name: name.to_owned(), span, extends })
    })
}

/// A PHP file and its first class.
struct PhpFile {
    path: String,
    text: String,
    class: Option<PhpClass>,
}

fn load_php(files: &dyn Files, path: &str) -> Option<PhpFile> {
    let text = files.read(path)?;
    let class = php_class(&text);
    Some(PhpFile { path: path.to_owned(), text, class })
}

/// The class `short` (`**/<short>.php` declaring it), preferring files near `near`.
fn find_class(files: &dyn Files, short: &str, near: &str) -> Option<PhpFile> {
    let mut candidates = files.glob(&format!("**/{short}.php"), 20);
    candidates.sort_by_key(|p| std::cmp::Reverse(common_prefix(p, near)));
    candidates
        .into_iter()
        .filter_map(|p| load_php(files, &p))
        .find(|f| f.class.as_ref().is_some_and(|c| c.name == short))
}

/// The first of `methods` found in the class of `start` or its parents.
fn method_in_hierarchy(files: &dyn Files, start: PhpFile, methods: &[String]) -> Option<Location> {
    let mut current = start;
    for _ in 0..=MAX_PARENTS {
        for m in methods {
            if let Some(span) = php_method(&current.text, m) {
                return Some(location(&current.path, &current.text, &span));
            }
        }
        let parent = current.class.as_ref()?.extends.clone()?;
        current = find_class(files, &parent, &current.path)?;
    }
    None
}

fn class_location(file: &PhpFile) -> Location {
    match &file.class {
        Some(c) => location(&file.path, &file.text, &c.span),
        None => Location { path: file.path.clone(), range: Range::default() },
    }
}

fn ucfirst(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

fn lcfirst(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_lowercase().chain(c).collect())
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.split('/').zip(b.split('/')).take_while(|(x, y)| x == y).count()
}

// ---------------------------------------------------------------------------
// Presenters and templates
// ---------------------------------------------------------------------------

/// `Product` of `.../ProductPresenter.php`.
pub fn presenter_name(path: &str) -> Option<&str> {
    file_name(path).strip_suffix("Presenter.php").filter(|n| !n.is_empty())
}

/// Does the directory (absolute) contain a `templates` directory with files?
fn has_templates(files: &dyn Files, dir: &str) -> bool {
    files.relative(dir).is_some_and(|rel| !files.glob(&join(&rel, "templates/**"), 1).is_empty())
}

/// Template files Nette tries for `view` of the presenter at `presenter`, in order.
pub fn template_candidates(files: &dyn Files, presenter: &str, view: &str) -> Vec<String> {
    let Some(name) = presenter_name(presenter) else { return Vec::new() };
    let mut dir = normalize(dir_of(presenter));
    if !has_templates(files, &dir) {
        let orig = dir.clone();
        dir = dir_of(&orig).to_owned();
        if !has_templates(files, &dir) {
            return vec![format!("{orig}/{view}.latte")];
        }
    }
    vec![format!("{dir}/templates/{name}/{view}.latte"), format!("{dir}/templates/{name}.{view}.latte")]
}

/// Layout files Nette tries for the presenter at `presenter`, in order.
pub fn layout_candidates(files: &dyn Files, presenter: &str) -> Vec<String> {
    let Some(name) = presenter_name(presenter) else { return Vec::new() };
    let root = files.root().to_owned();
    let mut dir = normalize(dir_of(presenter));
    let mut list = Vec::new();
    let up = |dir: &mut String| {
        let parent = dir_of(dir).to_owned();
        let ok = parent.len() >= root.len() && !parent.is_empty();
        if ok {
            *dir = parent;
        }
        ok
    };
    if !has_templates(files, &dir) {
        let orig = dir.clone();
        up(&mut dir);
        if !has_templates(files, &dir) {
            list.push(format!("{orig}/@layout.latte"));
            for _ in 0..=LAYOUT_LEVELS {
                list.push(format!("{dir}/@layout.latte"));
                if !up(&mut dir) {
                    break;
                }
            }
            list.dedup();
            return list;
        }
    }
    list.push(format!("{dir}/templates/{name}/@layout.latte"));
    list.push(format!("{dir}/templates/{name}.@layout.latte"));
    for _ in 0..=LAYOUT_LEVELS {
        list.push(format!("{dir}/templates/@layout.latte"));
        if !up(&mut dir) {
            break;
        }
    }
    list
}

/// The presenter rendering the template at `path`, and its view.
pub struct PresenterOf {
    pub path: String,
    pub view: String,
}

/// The presenter whose view template `path` is: candidates from the
/// directory layout, kept only if Nette would pick `path` for them.
pub fn presenter_of_template(files: &dyn Files, path: &str) -> Option<PresenterOf> {
    let path = normalize(path);
    let stem = file_name(&path).strip_suffix(".latte")?;
    if stem.starts_with('@') {
        return None;
    }
    let dir = dir_of(&path);
    let mut guesses: Vec<(String, String)> = Vec::new(); // (glob, view)
    if file_name(dir) == "templates"
        && let Some((name, view)) = stem.split_once('.')
    {
        // templates/Product.show.latte
        let base = files.relative(dir_of(dir))?;
        guesses.push((join(&base, &format!("{name}Presenter.php")), view.to_owned()));
        guesses.push((join(&base, &format!("*/{name}Presenter.php")), view.to_owned()));
    } else if file_name(dir_of(dir)) == "templates" {
        // templates/Product/show.latte
        let name = file_name(dir);
        let base = files.relative(dir_of(dir_of(dir)))?;
        guesses.push((join(&base, &format!("{name}Presenter.php")), stem.to_owned()));
        guesses.push((join(&base, &format!("*/{name}Presenter.php")), stem.to_owned()));
    }
    // Product/show.latte next to Product/ProductPresenter.php
    guesses.push((join(&files.relative(dir)?, "*Presenter.php"), stem.to_owned()));

    guesses.into_iter().find_map(|(glob, view)| {
        files
            .glob(&glob, 10)
            .into_iter()
            .find(|p| template_candidates(files, p, &view).iter().any(|c| *c == path))
            .map(|p| PresenterOf { path: p, view })
    })
}

/// The layout Nette uses for a presenter template that names none.
pub fn auto_layout(files: &dyn Files, path: &str, template: &Template) -> Option<String> {
    if template.parent.is_some() || template.layout_disabled {
        return None;
    }
    let presenter = presenter_of_template(files, path)?;
    let me = normalize(path);
    layout_candidates(files, &presenter.path).into_iter().find(|c| *c != me && files.exists(c))
}

/// Line 1 of a presenter template: to its `render<View>()` / `action<View>()`.
pub fn template_presenter_marker(files: &dyn Files, path: &str) -> Option<LineMarker> {
    let presenter = presenter_of_template(files, path)?;
    let file = load_php(files, &presenter.path)?;
    let view = ucfirst(&presenter.view);
    let class = file.class.as_ref().map_or_else(|| file_name(&file.path).to_owned(), |c| c.name.clone());
    let (target, tooltip) = ["render", "action"]
        .iter()
        .find_map(|prefix| {
            let method = format!("{prefix}{view}");
            let span = php_method(&file.text, &method)?;
            Some((location(&file.path, &file.text, &span), format!("Go to {class}::{method}()")))
        })
        .unwrap_or_else(|| (class_location(&file), format!("Go to {class}")));
    Some(LineMarker { line: 0, icon: Some("implemented".into()), tooltip, target: Some(target) })
}

/// Gutter markers of a presenter: from `render<X>()` / `action<X>()` to the template.
pub fn presenter_markers(files: &dyn Files, doc: &Document) -> Vec<LineMarker> {
    if presenter_name(&doc.path).is_none() {
        return Vec::new();
    }
    let mut markers: Vec<LineMarker> = Vec::new();
    for (method, span) in php_methods(&doc.text) {
        let Some(view) = method.strip_prefix("render").or_else(|| method.strip_prefix("action")) else { continue };
        if !view.starts_with(|c: char| c.is_ascii_uppercase()) {
            continue;
        }
        let line = text::position(&doc.text, span.start).line;
        if markers.iter().any(|m| m.line == line) {
            continue;
        }
        let Some(template) = template_candidates(files, &doc.path, &lcfirst(view)).into_iter().find(|c| files.exists(c))
        else {
            continue;
        };
        let shown = files.relative(&template).unwrap_or_else(|| template.clone());
        markers.push(LineMarker {
            line,
            icon: Some("implementing".into()),
            tooltip: format!("Go to template {shown}"),
            target: Some(Location { path: template, range: Range::default() }),
        });
    }
    markers
}

// ---------------------------------------------------------------------------
// Links and components
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum LinkDest {
    /// `signal!` of the current presenter.
    Signal(String),
    /// `[:][Module:]Presenter:action`, or an `action` of the current presenter.
    Action { modules: Vec<String>, presenter: Option<String>, action: String },
}

/// Parses a link destination; `None` for `this`, component signals and the like.
pub fn parse_link(dest: &str) -> Option<LinkDest> {
    let dest = dest.trim_matches(['\'', '"']);
    let dest = dest.strip_prefix("//").unwrap_or(dest);
    let dest = dest.split(['#', '?']).next().unwrap_or(dest);
    if dest.is_empty() || dest == "this" || dest.contains(['$', '-']) {
        return None;
    }
    if let Some(signal) = dest.strip_suffix('!') {
        return (!signal.contains(':') && !signal.is_empty()).then(|| LinkDest::Signal(signal.to_owned()));
    }
    let dest = dest.strip_prefix(':').unwrap_or(dest);
    let mut parts: Vec<&str> = dest.split(':').collect();
    let action = parts.pop()?;
    let action = if action.is_empty() { "default" } else { action };
    let presenter = parts.pop().map(str::to_owned);
    if presenter.as_deref() == Some("") || parts.iter().any(|m| m.is_empty()) {
        return None;
    }
    Some(LinkDest::Action { modules: parts.into_iter().map(str::to_owned).collect(), presenter, action: action.to_owned() })
}

/// The presenter file `name` in `modules`, preferring paths that name the
/// modules, then ones near `near`.
fn find_presenter(files: &dyn Files, modules: &[String], name: &str, near: &str) -> Option<String> {
    let candidates = files.glob(&format!("**/{name}Presenter.php"), 50);
    let module_hits = |path: &str| {
        let segs: Vec<String> = path.split('/').map(str::to_ascii_lowercase).collect();
        modules
            .iter()
            .filter(|m| {
                let m = m.to_ascii_lowercase();
                segs.iter().any(|s| *s == m || *s == format!("{m}module"))
            })
            .count()
    };
    candidates.into_iter().max_by_key(|p| (module_hits(p), common_prefix(p, near)))
}

/// `{link}` / `n:href` in the template at `path` → the presenter method.
pub fn link_target(files: &dyn Files, path: &str, dest: &str) -> Option<Location> {
    let dest = parse_link(dest)?;
    let current = || presenter_of_template(files, path).map(|p| p.path);
    let (presenter, methods) = match dest {
        LinkDest::Signal(signal) => (current()?, vec![format!("handle{}", ucfirst(&signal))]),
        LinkDest::Action { modules, presenter, action } => {
            let file = match presenter {
                Some(name) => find_presenter(files, &modules, &name, path)?,
                None => current()?,
            };
            let a = ucfirst(&action);
            (file, vec![format!("action{a}"), format!("render{a}")])
        }
    };
    let file = load_php(files, &presenter)?;
    let fallback = class_location(&file);
    Some(method_in_hierarchy(files, file, &methods).unwrap_or(fallback))
}

/// `{control name}` / `{form name}` / `<form n:name>` → `createComponent<Name>()`
/// of the template's presenter (or a PHP class next to the template).
pub fn component_target(files: &dyn Files, path: &str, name: &str) -> Option<Location> {
    let first = name.split(['-', ':']).next().filter(|n| !n.is_empty())?;
    let methods = [format!("createComponent{}", ucfirst(first))];
    if let Some(presenter) = presenter_of_template(files, path)
        && let Some(file) = load_php(files, &presenter.path)
        && let Some(found) = method_in_hierarchy(files, file, &methods)
    {
        return Some(found);
    }
    let rel = files.relative(dir_of(&normalize(path)))?;
    files
        .glob(&join(&rel, "*.php"), 20)
        .into_iter()
        .filter_map(|p| load_php(files, &p))
        .find_map(|f| method_in_hierarchy(files, f, &methods))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{self, tests::MapFiles};
    use openidea_plugin::Position;

    fn doc(path: &str, text: &str) -> Document {
        Document { path: path.into(), language: "latte".into(), version: 1, text: text.into() }
    }

    fn pos(text: &str, needle: &str, delta: usize) -> Position {
        text::position(text, text.find(needle).unwrap() + delta)
    }

    const BASE: &str = r#"<?php
namespace App\UI;

abstract class BasePresenter extends \Nette\Application\UI\Presenter
{
    public function handleLogout(): void {}

    protected function createComponentMenu(): Menu {}
}
"#;

    const PRODUCT: &str = r#"<?php
namespace App\UI\Product;

use App\UI\BasePresenter;

final class ProductPresenter extends BasePresenter
{
    public function actionEdit(int $id): void {}

    public function renderDefault(): void {}

    public function renderShow(int $id): void {}

    public function renderMissing(): void {}

    protected function createComponentEditForm(): Form
    {
        $x = new class extends Foo {};
        return new Form;
    }
}
"#;

    /// Nette 3.2 layout: templates next to presenters.
    fn modern() -> MapFiles {
        MapFiles::new(&[
            ("/p/app/UI/BasePresenter.php", BASE),
            ("/p/app/UI/@layout.latte", "{block content}{/block}\n{block title}{/block}"),
            ("/p/app/UI/Product/ProductPresenter.php", PRODUCT),
            ("/p/app/UI/Product/default.latte", "{block content}\n{/block}"),
            ("/p/app/UI/Product/show.latte", "{block content}{/block}"),
            ("/p/app/UI/Product/edit.latte", "{layout none}"),
            ("/p/app/UI/Admin/Product/ProductPresenter.php", "<?php class ProductPresenter { function renderDefault() {} }"),
            ("/p/app/UI/Admin/Product/default.latte", ""),
            ("/p/app/UI/Home/HomePresenter.php", "<?php class HomePresenter { function renderDefault() {} }"),
            ("/p/app/UI/Home/default.latte", ""),
            ("/p/app/UI/Home/partial.latte", ""),
        ])
    }

    /// Classic layout: `presenters/` with `templates/<Presenter>/`.
    fn classic() -> MapFiles {
        MapFiles::new(&[
            ("/p/app/Presenters/SignPresenter.php", "<?php class SignPresenter { function renderIn() {} }"),
            ("/p/app/Presenters/templates/@layout.latte", "{block content}{/block}"),
            ("/p/app/Presenters/templates/Sign/in.latte", "{block content}{/block}"),
            ("/p/app/Presenters/templates/Sign.out.latte", ""),
        ])
    }

    #[test]
    fn reads_php_classes_and_methods() {
        let class = php_class(PRODUCT).unwrap();
        assert_eq!(class.name, "ProductPresenter");
        assert_eq!(class.extends.as_deref(), Some("BasePresenter"));
        assert_eq!(php_class(BASE).unwrap().extends.as_deref(), Some("Presenter"));
        assert_eq!(php_class("<?php $x = Foo::class; class Bar {}").unwrap().name, "Bar");
        let names: Vec<String> = php_methods(PRODUCT).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["actionEdit", "renderDefault", "renderShow", "renderMissing", "createComponentEditForm"]);
        assert!(php_method(PRODUCT, "RENDERshow").is_some(), "method names ignore case");
    }

    #[test]
    fn parses_link_destinations() {
        let action = |m: &[&str], p: Option<&str>, a: &str| LinkDest::Action {
            modules: m.iter().map(|s| (*s).to_owned()).collect(),
            presenter: p.map(str::to_owned),
            action: a.into(),
        };
        assert_eq!(parse_link("Product:show"), Some(action(&[], Some("Product"), "show")));
        assert_eq!(parse_link(":Admin:Product:"), Some(action(&["Admin"], Some("Product"), "default")));
        assert_eq!(parse_link("//Product:show#top"), Some(action(&[], Some("Product"), "show")));
        assert_eq!(parse_link("edit"), Some(action(&[], None, "edit")));
        assert_eq!(parse_link("logout!"), Some(LinkDest::Signal("logout".into())));
        assert_eq!(parse_link("this"), None);
        assert_eq!(parse_link("cart-add!"), None, "component signals are not followed");
    }

    #[test]
    fn maps_templates_to_presenters_and_back() {
        let files = modern();
        let p = "/p/app/UI/Product/ProductPresenter.php";
        assert_eq!(template_candidates(&files, p, "show"), ["/p/app/UI/Product/show.latte"]);
        let of = presenter_of_template(&files, "/p/app/UI/Product/show.latte").unwrap();
        assert_eq!((of.path.as_str(), of.view.as_str()), (p, "show"));
        assert!(presenter_of_template(&files, "/p/app/UI/@layout.latte").is_none());

        let files = classic();
        let p = "/p/app/Presenters/SignPresenter.php";
        assert_eq!(
            template_candidates(&files, p, "in"),
            ["/p/app/Presenters/templates/Sign/in.latte", "/p/app/Presenters/templates/Sign.in.latte"]
        );
        assert_eq!(presenter_of_template(&files, "/p/app/Presenters/templates/Sign/in.latte").unwrap().path, p);
        let out = presenter_of_template(&files, "/p/app/Presenters/templates/Sign.out.latte").unwrap();
        assert_eq!((out.path.as_str(), out.view.as_str()), (p, "out"));
    }

    #[test]
    fn finds_the_automatic_layout() {
        let files = modern();
        let show = navigation::parse("{block content}{/block}");
        assert_eq!(
            auto_layout(&files, "/p/app/UI/Product/show.latte", &show).as_deref(),
            Some("/p/app/UI/@layout.latte")
        );
        let none = navigation::parse("{layout none}");
        assert!(auto_layout(&files, "/p/app/UI/Product/edit.latte", &none).is_none());

        let files = classic();
        assert_eq!(
            auto_layout(&files, "/p/app/Presenters/templates/Sign/in.latte", &show).as_deref(),
            Some("/p/app/Presenters/templates/@layout.latte")
        );
    }

    #[test]
    fn blocks_of_a_presenter_template_override_the_automatic_layout() {
        let files = modern();
        let src = "{block content}\n{/block}";
        let markers = navigation::line_markers(Some(&files), &doc("/p/app/UI/Product/default.latte", src));
        let summary: Vec<(u32, &str)> = markers.iter().map(|m| (m.line, m.tooltip.as_str())).collect();
        // Line 0 belongs to the presenter; the override marker would share it.
        assert_eq!(summary, [(0, "Go to ProductPresenter::renderDefault()")]);

        let src = "\n{block title}{/block}";
        let markers = navigation::line_markers(Some(&files), &doc("/p/app/UI/Product/show.latte", src));
        assert_eq!(markers[1].tooltip, "Overrides block 'title' in @layout.latte");
        assert_eq!(markers[0].tooltip, "Go to ProductPresenter::renderShow()");
        let target = markers[0].target.as_ref().unwrap();
        assert_eq!(target.range.start, pos(PRODUCT, "renderShow", 0));
    }

    #[test]
    fn marks_the_presenter_action_when_there_is_no_render() {
        let files = modern();
        let m = template_presenter_marker(&files, "/p/app/UI/Product/edit.latte").unwrap();
        assert_eq!(m.tooltip, "Go to ProductPresenter::actionEdit()");
        // A view needs no method of its own: the marker goes to the class.
        let m = template_presenter_marker(&files, "/p/app/UI/Home/partial.latte").unwrap();
        assert_eq!(m.tooltip, "Go to HomePresenter");
    }

    #[test]
    fn follows_links() {
        let files = modern();
        let src = r#"{link Product:show 5} {link edit} {plink :Admin:Product:} <a n:href="logout!">x</a> <a n:href=Home:>h</a> {link Product:nope}"#;
        let d = doc("/p/app/UI/Product/default.latte", src);
        let at = |needle: &str| navigation::definition(Some(&files), &d, pos(src, needle, 1));

        let show = at("Product:show");
        assert_eq!(show[0].path, "/p/app/UI/Product/ProductPresenter.php");
        assert_eq!(show[0].range.start, pos(PRODUCT, "renderShow", 0));

        assert_eq!(at("edit}")[0].range.start, pos(PRODUCT, "actionEdit", 0), "action wins over render");
        assert_eq!(at(":Admin")[0].path, "/p/app/UI/Admin/Product/ProductPresenter.php");
        let logout = at("logout!");
        assert_eq!((logout[0].path.as_str(), logout[0].range.start), ("/p/app/UI/BasePresenter.php", pos(BASE, "handleLogout", 0)));
        assert_eq!(at("Home:>")[0].path, "/p/app/UI/Home/HomePresenter.php");
        assert_eq!(at("Product:nope")[0].range.start, pos(PRODUCT, "ProductPresenter", 0), "unknown action: the class");
    }

    #[test]
    fn follows_components() {
        let files = modern();
        let src = "{control menu}\n{form editForm}\n<form n:name=\"editForm\" class=x>\n<input n:name=\"name\">\n{control nope}";
        let d = doc("/p/app/UI/Product/default.latte", src);
        let at = |needle: &str| navigation::definition(Some(&files), &d, pos(src, needle, 1));

        assert_eq!(at("menu")[0].range.start, pos(BASE, "createComponentMenu", 0), "inherited from the base presenter");
        assert_eq!(at("editForm}")[0].range.start, pos(PRODUCT, "createComponentEditForm", 0));
        assert_eq!(at("editForm\"")[0].range.start, pos(PRODUCT, "createComponentEditForm", 0));
        assert!(at("name\"").is_empty(), "n:name of an input is a form field");
        assert!(at("nope").is_empty());
    }

    #[test]
    fn finds_components_next_to_a_control_template() {
        let files = MapFiles::new(&[
            ("/p/app/Components/Cart/CartControl.php", "<?php class CartControl { function createComponentItems() {} }"),
            ("/p/app/Components/Cart/default.latte", ""),
        ]);
        let src = "{control items-5}";
        let d = doc("/p/app/Components/Cart/default.latte", src);
        let found = navigation::definition(Some(&files), &d, pos(src, "items", 0));
        assert_eq!(found[0].path, "/p/app/Components/Cart/CartControl.php");
    }

    #[test]
    fn marks_templates_in_presenters() {
        let files = modern();
        let d = Document {
            path: "/p/app/UI/Product/ProductPresenter.php".into(),
            language: "php".into(),
            version: 1,
            text: PRODUCT.into(),
        };
        let markers = presenter_markers(&files, &d);
        let summary: Vec<(u32, &str)> = markers.iter().map(|m| (m.line, m.tooltip.as_str())).collect();
        assert_eq!(
            summary,
            [
                (7, "Go to template app/UI/Product/edit.latte"),
                (9, "Go to template app/UI/Product/default.latte"),
                (11, "Go to template app/UI/Product/show.latte"),
            ],
            "renderMissing has no template"
        );
        let other = Document { path: "/p/app/Model/Foo.php".into(), ..d };
        assert!(presenter_markers(&files, &other).is_empty());
    }
}
