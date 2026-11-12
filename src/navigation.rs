//! Go to Definition and gutter markers between Latte templates:
//!
//! - file references: `{include 'file.latte'}`, `{layout}`, `{extends}`,
//!   `{import}`, `{embed}`, `{sandbox}`, `{includeblock}`, `{include x from 'file'}`;
//! - block references: `{include name}`, `{include #name}`, `{include block name}`,
//!   `{ifset #name}`, resolved in the template itself, its `{import}`s and its
//!   layout chain;
//! - gutter markers: the layout/imported file, and blocks that override a block
//!   of the layout.
//!
//! Parsing works on byte offsets; [`openidea_plugin::text`] converts them to
//! editor positions (UTF-16 columns).

use crate::php::ClassIndex;
use crate::{latte_php, nette};
use openidea_plugin::providers::{Document, LineMarker};
use openidea_plugin::text;
use openidea_plugin::{Location, Position, Project, Range};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range as Span;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How deep `{layout}` / `{import}` chains are followed (guards cycles too).
const MAX_DEPTH: usize = 8;

/// Tags whose first argument is a template file.
const FILE_TAGS: &[&str] = &["layout", "extends", "import", "sandbox", "includeblock"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A template path as written (relative to the template, or absolute).
    File(String),
    /// A block, optionally looked up in another template (`from 'file'`).
    Block { name: String, from: Option<String> },
    /// A Nette link destination: `Product:show`, `:Admin:Home:`, `signal!`.
    Link(String),
    /// A component of the presenter: `{control name}`, `{form name}`, `<form n:name>`.
    Component(String),
}

/// Something in a template that points elsewhere; `span` is what the user
/// clicks (the path without quotes, the block name without `#`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub tag: String,
    pub target: Target,
    pub span: Span<usize>,
}

/// A block definition: `{block name}`, `{define name}`, `n:block="name"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDef {
    pub name: String,
    pub span: Span<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template {
    pub refs: Vec<Reference>,
    pub blocks: Vec<BlockDef>,
    /// `{layout}` / `{extends}` target.
    pub parent: Option<String>,
    /// `{import}` targets.
    pub imports: Vec<String>,
    /// `{layout none}`: no automatic layout either.
    pub layout_disabled: bool,
    /// Latte code regions: (tag, arguments) of `{tag …}` and the values of
    /// `n:tag="…"` attributes (`n:inner-`/`n:tag-` prefixes dropped).
    pub code: Vec<(String, Span<usize>)>,
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// A Latte tag `{name args}`: the tag name and the byte span of its arguments.
struct RawTag {
    name: String,
    args: Span<usize>,
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Parses the references and block definitions of a template.
pub fn parse(text: &str) -> Template {
    let mut t = Template::default();
    for tag in scan(text, &mut t) {
        parse_tag(text, &tag, &mut t);
    }
    t
}

/// The value span of an HTML attribute value starting at `at` (after `=`):
/// quoted, or bare up to whitespace or `>`. Returns (value, where to resume).
fn attr_value(bytes: &[u8], at: usize) -> Option<(Span<usize>, usize)> {
    let &first = bytes.get(at)?;
    if first == b'"' || first == b'\'' {
        let start = at + 1;
        let end = bytes[start..].iter().position(|b| *b == first).map_or(bytes.len(), |p| start + p);
        return Some((start..end, (end + 1).min(bytes.len())));
    }
    let end = at + bytes[at..].iter().take_while(|b| !b.is_ascii_whitespace() && **b != b'>').count();
    (end > at).then_some((at..end, end))
}

/// Is the attribute at `at` on a `<form>` element?
fn in_form_tag(text: &str, at: usize) -> bool {
    text[..at].rfind('<').is_some_and(|lt| {
        let tag = &text[lt + 1..at];
        tag.len() > 4 && tag[..4].eq_ignore_ascii_case("form") && tag.as_bytes()[4].is_ascii_whitespace() && !tag.contains('>')
    })
}

/// An `n:` attribute at `i`: records what it names, returns where to resume.
fn n_attr(text: &str, i: usize, t: &mut Template) -> Option<usize> {
    let bytes = text.as_bytes();
    if i == 0 || !bytes[i - 1].is_ascii_whitespace() {
        return None;
    }
    let name_len = bytes[i + 2..].iter().take_while(|b| b.is_ascii_alphanumeric() || **b == b'_' || **b == b'-').count();
    if name_len == 0 || bytes.get(i + 2 + name_len) != Some(&b'=') {
        return None;
    }
    let name = &text[i + 2..i + 2 + name_len];
    let name = name.strip_prefix("inner-").or_else(|| name.strip_prefix("tag-")).unwrap_or(name);
    let (value, resume) = attr_value(bytes, i + 2 + name_len + 1)?;
    t.code.push((name.to_owned(), value.clone()));
    match name {
        "block" => t.blocks.extend(block_def(text, value)),
        "href" => {
            if let Some(arg) = next_arg(text, value.start, value.end)
                && is_static(&text[arg.span.clone()])
            {
                let dest = text[arg.span.clone()].to_owned();
                t.refs.push(Reference { tag: "n:href".into(), target: Target::Link(dest), span: arg.span });
            }
        }
        "name" if in_form_tag(text, i) => {
            if let Some(arg) = next_arg(text, value.start, value.end)
                && is_static(&text[arg.span.clone()])
            {
                let name = text[arg.span.clone()].to_owned();
                t.refs.push(Reference { tag: "n:name".into(), target: Target::Component(name), span: arg.span });
            }
        }
        _ => {}
    }
    Some(resume)
}

/// Latte tags of `text` in order, skipping comments and `{syntax off}`
/// regions. Block definitions and `n:` attributes found on the way go
/// straight to `t`.
fn scan(text: &str, t: &mut Template) -> Vec<RawTag> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut tags = Vec::new();
    let mut i = 0;

    while i < len {
        if bytes[i..].starts_with(b"{*") {
            i = text[i + 2..].find("*}").map_or(len, |p| i + 2 + p + 2);
            continue;
        }
        if bytes[i..].starts_with(b"<!--") {
            i = text[i + 4..].find("-->").map_or(len, |p| i + 4 + p + 3);
            continue;
        }
        if bytes[i..].starts_with(b"n:")
            && let Some(resume) = n_attr(text, i, t)
        {
            i = resume;
            continue;
        }
        if bytes[i] == b'{' && i + 1 < len && !bytes[i + 1].is_ascii_whitespace() && bytes[i + 1] != b'{' {
            let end = tag_end(bytes, i + 1);
            let inner = i + 1..end;
            i = (end + 1).min(len);

            let name_end = inner.start + bytes[inner.clone()].iter().take_while(|b| is_name_byte(**b)).count();
            if name_end == inner.start || !bytes[inner.start].is_ascii_alphabetic() {
                // `{$var}`, `{=expr}`, `{_'text'}` are code; `{/block}` is not.
                if bytes[inner.start] != b'/' {
                    t.code.push((String::new(), inner));
                }
                continue;
            }
            let tag = RawTag { name: text[inner.start..name_end].to_owned(), args: name_end..inner.end };
            t.code.push((tag.name.clone(), tag.args.clone()));

            if tag.name == "syntax" && text[tag.args.clone()].trim() == "off" {
                // Nothing inside is Latte until `{/syntax}`.
                i = text[i..].find("{/syntax}").map_or(len, |p| i + p + 9);
                continue;
            }
            if matches!(tag.name.as_str(), "block" | "define")
                && let Some(def) = block_def(text, tag.args.clone())
            {
                t.blocks.push(def);
            }
            tags.push(tag);
            continue;
        }
        i += 1;
    }
    tags
}

/// The byte offset of the `}` closing a tag whose content starts at `from`
/// (or the end of the text), skipping quoted strings and nested braces.
fn tag_end(bytes: &[u8], from: usize) -> usize {
    let mut depth = 0usize;
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"') => {
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'{' => depth += 1,
            b'}' if depth == 0 => return i,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    bytes.len()
}

/// One argument word: a quoted string (span without quotes) or a bare word.
#[derive(Debug)]
struct Arg {
    quoted: bool,
    span: Span<usize>,
}

/// The argument starting at or after `from` and before `end`.
fn next_arg(text: &str, from: usize, end: usize) -> Option<Arg> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < end && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= end {
        return None;
    }
    if bytes[i] == b'\'' || bytes[i] == b'"' {
        let q = bytes[i];
        let start = i + 1;
        let mut j = start;
        while j < end && bytes[j] != q {
            j += 1;
        }
        return Some(Arg { quoted: true, span: start..j });
    }
    let start = i;
    while i < end && !bytes[i].is_ascii_whitespace() && !matches!(bytes[i], b',' | b'|' | b'(' | b')') {
        i += 1;
    }
    (i > start).then_some(Arg { quoted: false, span: start..i })
}

/// Text after an argument (`next_arg` resumes there; a quote is skipped).
fn after(arg: &Arg) -> usize {
    arg.span.end + usize::from(arg.quoted)
}

/// A literal that names something statically (no variables or expressions).
fn is_static(s: &str) -> bool {
    !s.is_empty() && !s.contains(['$', '{', '}', '(', ')', '[', ']'])
}

/// `{block name}` / `{define local name, $args}` / `n:block="name"` → its name.
fn block_def(text: &str, args: Span<usize>) -> Option<BlockDef> {
    let mut arg = next_arg(text, args.start, args.end)?;
    // `{define local name}`; a lone `local` is the block's name.
    if !arg.quoted
        && &text[arg.span.clone()] == "local"
        && let Some(name) = next_arg(text, after(&arg), args.end)
    {
        arg = name;
    }
    let mut span = arg.span;
    if text[span.clone()].starts_with('#') {
        span.start += 1;
    }
    let name = &text[span.clone()];
    (is_static(name) && !name.starts_with(['|', '='])).then(|| BlockDef { name: name.to_owned(), span })
}

fn parse_tag(text: &str, tag: &RawTag, t: &mut Template) {
    let end = tag.args.end;
    let Some(first) = next_arg(text, tag.args.start, end) else { return };
    let first_text = &text[first.span.clone()];
    let mut push = |target: Target, span: Span<usize>| {
        t.refs.push(Reference { tag: tag.name.clone(), target, span });
    };

    match tag.name.as_str() {
        name if FILE_TAGS.contains(&name) => {
            if matches!(name, "layout" | "extends") && !first.quoted && matches!(first_text, "none" | "false") {
                t.layout_disabled = true;
            }
            if !is_static(first_text) || (!first.quoted && matches!(first_text, "auto" | "none" | "false")) {
                return;
            }
            let path = first_text.to_owned();
            match name {
                "layout" | "extends" => t.parent = Some(path.clone()),
                "import" => t.imports.push(path.clone()),
                _ => {}
            }
            push(Target::File(path), first.span);
        }
        "include" | "embed" => {
            // `{include file 'x'}` / `{include block x}` name the kind explicitly.
            let (arg, kind) = match (first.quoted, first_text) {
                (false, "file") => (next_arg(text, after(&first), end), Some(true)),
                (false, "block") => (next_arg(text, after(&first), end), Some(false)),
                (false, "parent" | "this") => return,
                _ => (Some(first), None),
            };
            let Some(arg) = arg else { return };
            let mut span = arg.span.clone();
            let raw = &text[span.clone()];
            let is_file = kind.unwrap_or_else(|| arg.quoted || raw.contains(['.', '/']));
            if !is_file && raw.starts_with('#') {
                span.start += 1;
            }
            let value = &text[span.clone()];
            if !is_static(value) {
                return;
            }
            if is_file {
                push(Target::File(value.to_owned()), span);
                return;
            }
            // `{include name from 'file.latte'}`
            let mut from = None;
            if let Some(kw) = next_arg(text, after(&arg), end)
                && !kw.quoted
                && &text[kw.span.clone()] == "from"
                && let Some(file) = next_arg(text, after(&kw), end)
                && is_static(&text[file.span.clone()])
            {
                let path = text[file.span.clone()].to_owned();
                from = Some(path.clone());
                push(Target::File(path), file.span);
            }
            push(Target::Block { name: value.to_owned(), from }, span);
        }
        "ifset" | "elseifset" => {
            let arg = match (first.quoted, first_text) {
                (false, "block") => next_arg(text, after(&first), end),
                (false, s) if s.starts_with('#') => Some(first),
                _ => None,
            };
            let Some(arg) = arg else { return };
            let mut span = arg.span;
            if text[span.clone()].starts_with('#') {
                span.start += 1;
            }
            let name = &text[span.clone()];
            if is_static(name) {
                push(Target::Block { name: name.to_owned(), from: None }, span);
            }
        }
        "link" | "plink" => {
            if is_static(first_text) {
                push(Target::Link(first_text.to_owned()), first.span);
            }
        }
        "control" | "form" | "formPrint" | "formClassPrint" => {
            if is_static(first_text) {
                push(Target::Component(first_text.to_owned()), first.span);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Read access to the project's files (the host, or a map in tests).
pub trait Files {
    /// The text of the file at the absolute `path`.
    fn read(&self, path: &str) -> Option<String>;
    /// Absolute paths of project files matching `glob` (relative to the
    /// project root, `**` for any directories), at most `limit`.
    fn glob(&self, glob: &str, limit: u32) -> Vec<String>;
    /// The project root: absolute, `/` separators, no trailing `/`.
    fn root(&self) -> &str;

    fn exists(&self, path: &str) -> bool {
        self.read(path).is_some()
    }

    /// A cached [`ClassIndex`] of the project, if the implementation keeps one.
    fn cached_class_index(&self) -> Option<Rc<ClassIndex>> {
        None
    }

    /// `path` relative to the root (`""` for the root itself), if inside it.
    fn relative(&self, path: &str) -> Option<String> {
        let path = normalize(path);
        let root = self.root();
        if path == root {
            return Some(String::new());
        }
        path.strip_prefix(root)?.strip_prefix('/').map(str::to_owned)
    }
}

/// The class index of `files`: its cached one, or a fresh one.
pub fn class_index(files: &dyn Files) -> Rc<ClassIndex> {
    files.cached_class_index().unwrap_or_else(|| Rc::new(ClassIndex::load(files)))
}

/// How long a project's class index is reused before Composer's maps are read again.
const INDEX_TTL: Duration = Duration::from_secs(300);

/// Class indexes by project root, kept by the plugin between calls.
pub type IndexCache = Rc<RefCell<HashMap<String, (Instant, Rc<ClassIndex>)>>>;

/// The host project containing `path`.
pub struct ProjectFiles {
    project: Project,
    root: String,
    cache: IndexCache,
}

impl ProjectFiles {
    pub fn for_path(path: &str, cache: &IndexCache) -> Option<Self> {
        let path = normalize(path);
        openidea_plugin::host::projects()
            .ok()?
            .into_iter()
            .filter_map(|project| {
                let root = normalize(&project.root().ok()?);
                let root = root.trim_end_matches('/').to_owned();
                let inside = path == root || path.starts_with(&format!("{root}/"));
                inside.then(|| Self { project, root, cache: cache.clone() })
            })
            .max_by_key(|f| f.root.len())
    }
}

impl Files for ProjectFiles {
    fn read(&self, path: &str) -> Option<String> {
        self.project.read_file(path).ok()
    }

    fn glob(&self, glob: &str, limit: u32) -> Vec<String> {
        self.project.find_files(glob, limit).unwrap_or_default()
    }

    fn root(&self) -> &str {
        &self.root
    }

    fn cached_class_index(&self) -> Option<Rc<ClassIndex>> {
        if let Some((at, index)) = self.cache.borrow().get(&self.root)
            && at.elapsed() < INDEX_TTL
        {
            return Some(index.clone());
        }
        let index = Rc::new(ClassIndex::load(self));
        self.cache.borrow_mut().insert(self.root.clone(), (Instant::now(), index.clone()));
        Some(index)
    }
}

/// `path` with `.` and `..` segments resolved and `/` separators.
pub(crate) fn normalize(path: &str) -> String {
    let path = path.replace('\\', "/");
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

pub(crate) fn dir_of(path: &str) -> &str {
    path.rfind(['/', '\\']).map_or("", |i| &path[..i])
}

pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `dir/name`, or `name` in the root (`dir` empty).
pub(crate) fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_owned() } else { format!("{dir}/{name}") }
}

/// Is `path` absolute (`/x`, `C:/x`, `C:\x`)?
fn is_absolute(path: &str) -> bool {
    path.starts_with(['/', '\\']) || path.as_bytes().get(1..3).is_some_and(|s| s[0] == b':' && (s[1] == b'/' || s[1] == b'\\'))
}

/// Resolves a template path written in the template at `from` to an existing
/// file: relative to that template's directory as Latte does, then (for
/// paths the app's loader may resolve differently) by its trailing segments
/// anywhere in the project.
pub fn resolve_file(files: &dyn Files, from: &str, written: &str) -> Option<(String, String)> {
    // A Windows drive (`C:`) stays the first segment.
    let candidate = if is_absolute(written) {
        normalize(written)
    } else {
        normalize(&format!("{}/{written}", dir_of(from)))
    };
    if let Some(text) = files.read(&candidate) {
        return Some((candidate, text));
    }
    let suffix: Vec<&str> = written
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    if suffix.is_empty() {
        return None;
    }
    let path = files.glob(&format!("**/{}", suffix.join("/")), 1).into_iter().next()?;
    let text = files.read(&path)?;
    Some((path, text))
}

/// A parsed template in the project.
struct Loaded {
    path: String,
    text: String,
    template: Template,
}

fn load(files: &dyn Files, from: &str, written: &str) -> Option<Loaded> {
    let (path, text) = resolve_file(files, from, written)?;
    let template = parse(&text);
    Some(Loaded { path, text, template })
}

pub(crate) fn location(path: &str, text: &str, span: &Span<usize>) -> Location {
    Location {
        path: path.to_owned(),
        range: Range::new(text::position(text, span.start), text::position(text, span.end)),
    }
}

/// Templates a template inherits blocks from, as written: its `{import}`s
/// and its `{layout}` / `{extends}`.
fn explicit_ancestors(template: &Template) -> Vec<String> {
    template.imports.iter().chain(template.parent.iter()).cloned().collect()
}

/// Like [`explicit_ancestors`], plus the layout Nette picks for a presenter
/// template that names none (as an absolute path).
fn ancestors(files: &dyn Files, path: &str, template: &Template) -> Vec<String> {
    let mut list = explicit_ancestors(template);
    if let Some(layout) = nette::auto_layout(files, path, template) {
        list.push(layout);
    }
    list
}

/// Where the block `name` is defined in the templates `related` (written
/// in the template at `path`) or the ones they inherit from in turn.
fn block_in_ancestors(
    files: &dyn Files,
    path: &str,
    related: &[String],
    name: &str,
    seen: &mut HashSet<String>,
    depth: usize,
) -> Option<Location> {
    if depth > MAX_DEPTH {
        return None;
    }
    for written in related {
        let Some(other) = load(files, path, written) else { continue };
        if !seen.insert(normalize(&other.path)) {
            continue;
        }
        if let Some(def) = other.template.blocks.iter().find(|b| b.name == name) {
            return Some(location(&other.path, &other.text, &def.span));
        }
        let next = explicit_ancestors(&other.template);
        if let Some(found) = block_in_ancestors(files, &other.path, &next, name, seen, depth + 1) {
            return Some(found);
        }
    }
    None
}

/// The definition of the block `name` used in the document.
fn find_block(files: Option<&dyn Files>, doc: &Document, template: &Template, name: &str, from: Option<&str>) -> Option<Location> {
    if let Some(from) = from {
        let other = load(files?, &doc.path, from)?;
        let def = other.template.blocks.iter().find(|b| b.name == name)?;
        return Some(location(&other.path, &other.text, &def.span));
    }
    if let Some(def) = template.blocks.iter().find(|b| b.name == name) {
        return Some(location(&doc.path, &doc.text, &def.span));
    }
    let files = files?;
    let mut seen = HashSet::from([normalize(&doc.path)]);
    block_in_ancestors(files, &doc.path, &ancestors(files, &doc.path, template), name, &mut seen, 0)
}

fn target_of(files: Option<&dyn Files>, doc: &Document, template: &Template, r: &Reference) -> Option<Location> {
    match &r.target {
        Target::File(written) => {
            let (path, _) = resolve_file(files?, &doc.path, written)?;
            Some(Location { path, range: Range::default() })
        }
        Target::Block { name, from } => find_block(files, doc, template, name, from.as_deref()),
        Target::Link(dest) => nette::link_target(files?, &doc.path, dest),
        Target::Component(name) => nette::component_target(files?, &doc.path, name),
    }
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

/// Go to Definition at `at`.
pub fn definition(files: Option<&dyn Files>, doc: &Document, at: Position) -> Vec<Location> {
    let Some(offset) = text::offset(&doc.text, at) else { return Vec::new() };
    let template = parse(&doc.text);
    let mut refs = template.refs.iter().filter(|r| r.span.start <= offset && offset <= r.span.end).peekable();
    if refs.peek().is_some() {
        return refs.find_map(|r| target_of(files, doc, &template, r)).into_iter().collect();
    }
    // PHP in the template's code: classes, members, variables.
    let Some(files) = files else { return Vec::new() };
    let index = class_index(files);
    let ctx = latte_php::Ctx { files, index: &index, path: &doc.path, text: &doc.text, template: &template };
    ctx.definition(offset).into_iter().collect()
}

/// Gutter markers: the presenter (line 1), the layout and imported
/// templates, and blocks that override a block of the layout chain.
pub fn line_markers(files: Option<&dyn Files>, doc: &Document) -> Vec<LineMarker> {
    let Some(files) = files else { return Vec::new() };
    let template = parse(&doc.text);
    let mut markers: Vec<LineMarker> = Vec::new();
    let push = |markers: &mut Vec<LineMarker>, marker: LineMarker| {
        if !markers.iter().any(|m| m.line == marker.line) {
            markers.push(marker);
        }
    };

    if let Some(marker) = nette::template_presenter_marker(files, &doc.path) {
        push(&mut markers, marker);
    }

    for r in &template.refs {
        let Target::File(written) = &r.target else { continue };
        if !matches!(r.tag.as_str(), "layout" | "extends" | "import") {
            continue;
        }
        let Some((path, _)) = resolve_file(files, &doc.path, written) else { continue };
        let marker = LineMarker {
            line: text::position(&doc.text, r.span.start).line,
            icon: Some("navigate".into()),
            tooltip: format!("Go to {}", file_name(&path)),
            target: Some(Location { path, range: Range::default() }),
        };
        push(&mut markers, marker);
    }

    let related = ancestors(files, &doc.path, &template);
    if !related.is_empty() {
        for def in &template.blocks {
            let mut seen = HashSet::from([normalize(&doc.path)]);
            let Some(target) = block_in_ancestors(files, &doc.path, &related, &def.name, &mut seen, 0) else {
                continue;
            };
            let marker = LineMarker {
                line: text::position(&doc.text, def.span.start).line,
                icon: Some("implementing".into()),
                tooltip: format!("Overrides block '{}' in {}", def.name, file_name(&target.path)),
                target: Some(target),
            };
            push(&mut markers, marker);
        }
    }
    markers
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Files in memory, by absolute path, in the project `/p`.
    pub(crate) struct MapFiles(BTreeMap<String, String>);

    impl MapFiles {
        pub(crate) fn new(files: &[(&str, &str)]) -> Self {
            Self(files.iter().map(|(p, t)| ((*p).to_owned(), (*t).to_owned())).collect())
        }
    }

    /// `*` within a segment, `**` across segments (like the host's globs).
    fn glob_match(glob: &[u8], path: &[u8]) -> bool {
        match glob {
            [] => path.is_empty(),
            [b'*', b'*', b'/', rest @ ..] => glob_match(rest, path) || (!path.is_empty() && glob_match(glob, &path[1..])),
            [b'*', b'*', rest @ ..] => (0..=path.len()).any(|i| glob_match(rest, &path[i..])),
            [b'*', rest @ ..] => (0..=path.len())
                .take_while(|&i| i == 0 || path[i - 1] != b'/')
                .any(|i| glob_match(rest, &path[i..])),
            [c, rest @ ..] => path.first() == Some(c) && glob_match(rest, &path[1..]),
        }
    }

    impl Files for MapFiles {
        fn read(&self, path: &str) -> Option<String> {
            self.0.get(path).cloned()
        }

        fn glob(&self, glob: &str, limit: u32) -> Vec<String> {
            self.0
                .keys()
                .filter(|p| p.strip_prefix("/p/").is_some_and(|rel| glob_match(glob.as_bytes(), rel.as_bytes())))
                .take(limit as usize)
                .cloned()
                .collect()
        }

        fn root(&self) -> &str {
            "/p"
        }
    }

    fn doc(path: &str, text: &str) -> Document {
        Document { path: path.into(), language: "latte".into(), version: 1, text: text.into() }
    }

    /// The editor position of the first `needle` in `text`, `delta` bytes in.
    fn pos(text: &str, needle: &str, delta: usize) -> Position {
        text::position(text, text.find(needle).unwrap() + delta)
    }

    fn targets(t: &Template) -> Vec<(String, Target)> {
        t.refs.iter().map(|r| (r.tag.clone(), r.target.clone())).collect()
    }

    fn file(p: &str) -> Target {
        Target::File(p.into())
    }

    fn block(n: &str) -> Target {
        Target::Block { name: n.into(), from: None }
    }

    #[test]
    fn parses_references() {
        let t = parse(
            r#"{layout '../@layout.latte'}
{import 'components/forms.latte'}
{include 'partials/menu.latte', active: true}
{include header}
{include #footer}
{include block sidebar}
{include file "x.latte"}
{include parts/bare.latte}
{include item from 'list.latte'}
{embed 'card.latte'}{/embed}
{ifset #title}{/ifset}
{include $dynamic}
{include "row-$i.latte"}
{include parent}
{* {include 'commented.latte'} *}
{syntax off}{include 'raw.latte'}{/syntax}
"#,
        );
        assert_eq!(t.parent.as_deref(), Some("../@layout.latte"));
        assert_eq!(t.imports, ["components/forms.latte"]);
        assert_eq!(
            targets(&t),
            [
                ("layout".into(), file("../@layout.latte")),
                ("import".into(), file("components/forms.latte")),
                ("include".into(), file("partials/menu.latte")),
                ("include".into(), block("header")),
                ("include".into(), block("footer")),
                ("include".into(), block("sidebar")),
                ("include".into(), file("x.latte")),
                ("include".into(), file("parts/bare.latte")),
                ("include".into(), file("list.latte")),
                ("include".into(), Target::Block { name: "item".into(), from: Some("list.latte".into()) }),
                ("embed".into(), file("card.latte")),
                ("ifset".into(), block("title")),
            ]
        );
    }

    #[test]
    fn spans_cover_the_clickable_text() {
        let src = "{include #footer}{layout '@layout.latte'}";
        let t = parse(src);
        assert_eq!(&src[t.refs[0].span.clone()], "footer");
        assert_eq!(&src[t.refs[1].span.clone()], "@layout.latte");
    }

    #[test]
    fn parses_block_definitions() {
        let src = r#"{block content}{/block}
{define local item, $x}{/define}
{block #title}{/block}
<div n:block="sidebar"></div>
{block}{/block}
{block|upper}x{/block}
{block $name}{/block}
{layout none}
"#;
        let t = parse(src);
        let names: Vec<&str> = t.blocks.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["content", "item", "title", "sidebar"]);
        assert_eq!(&src[t.blocks[2].span.clone()], "title");
        assert_eq!(t.parent, None, "{{layout none}} is no layout");
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("/a/b/../c/./d.latte"), "/a/c/d.latte");
        assert_eq!(normalize("/a/../../x"), "/x");
        assert_eq!(normalize("a\\b\\..\\c"), "a/c");
    }

    const LAYOUT: &str = "<html>\n{block title}Site{/block}\n{include content}\n{define menu}{/define}\n</html>\n";
    const PAGE: &str = "{layout '../@layout.latte'}\n\n{block title}Home{/block}\n\n{block content}\n{include menu}\n{include 'parts/card.latte'}\n{include local}\n{define local}{/define}\n{/block}\n";

    fn project() -> MapFiles {
        MapFiles::new(&[
            ("/p/app/UI/@layout.latte", LAYOUT),
            ("/p/app/UI/Home/default.latte", PAGE),
            ("/p/app/UI/Home/parts/card.latte", "card"),
            ("/p/app/UI/Shared/widgets/box.latte", "box"),
        ])
    }

    #[test]
    fn goes_to_included_files_and_layout() {
        let files = project();
        let d = doc("/p/app/UI/Home/default.latte", PAGE);

        let to_layout = definition(Some(&files), &d, pos(PAGE, "@layout", 3));
        assert_eq!(to_layout.len(), 1);
        assert_eq!(to_layout[0].path, "/p/app/UI/@layout.latte");

        let to_card = definition(Some(&files), &d, pos(PAGE, "parts/card", 0));
        assert_eq!(to_card[0].path, "/p/app/UI/Home/parts/card.latte");

        assert!(definition(Some(&files), &d, pos(PAGE, "{layout", 1)).is_empty(), "the tag name is no link");
    }

    #[test]
    fn falls_back_to_a_project_wide_match() {
        let files = project();
        let src = "{include 'widgets/box.latte'}";
        let d = doc("/p/app/UI/Home/default.latte", src);
        let found = definition(Some(&files), &d, pos(src, "box", 0));
        assert_eq!(found[0].path, "/p/app/UI/Shared/widgets/box.latte");

        let missing = "{include 'nope.latte'}";
        assert!(definition(Some(&files), &doc("/p/a.latte", missing), pos(missing, "nope", 0)).is_empty());
    }

    #[test]
    fn goes_to_blocks_here_and_in_the_layout() {
        let files = project();
        let d = doc("/p/app/UI/Home/default.latte", PAGE);

        let local = definition(Some(&files), &d, pos(PAGE, "{include local", 10));
        assert_eq!(local[0].path, "/p/app/UI/Home/default.latte");
        assert_eq!(local[0].range.start, pos(PAGE, "{define local", 8));

        let menu = definition(Some(&files), &d, pos(PAGE, "{include menu", 10));
        assert_eq!(menu[0].path, "/p/app/UI/@layout.latte");
        assert_eq!(menu[0].range.start, pos(LAYOUT, "menu", 0));

        // Without project access, only blocks of the document itself.
        assert_eq!(definition(None, &d, pos(PAGE, "{include local", 10)).len(), 1);
        assert!(definition(None, &d, pos(PAGE, "{include menu", 10)).is_empty());
    }

    #[test]
    fn follows_imports_and_from() {
        let files = MapFiles::new(&[
            ("/p/forms.latte", "{define input}{/define}"),
            ("/p/list.latte", "\n{define item}{/define}"),
        ]);
        let src = "{import 'forms.latte'}{include input}{include item from 'list.latte'}";
        let d = doc("/p/page.latte", src);
        assert_eq!(definition(Some(&files), &d, pos(src, "input}", 0))[0].path, "/p/forms.latte");
        let item = definition(Some(&files), &d, pos(src, "item", 1));
        assert_eq!(item[0].path, "/p/list.latte");
        assert_eq!(item[0].range.start, Position::new(1, 8));
    }

    #[test]
    fn layout_cycles_terminate() {
        let files = MapFiles::new(&[("/p/a.latte", "{layout 'b.latte'}"), ("/p/b.latte", "{layout 'a.latte'}")]);
        let src = "{layout 'a.latte'}{include missing}";
        let d = doc("/p/c.latte", src);
        assert!(definition(Some(&files), &d, pos(src, "missing", 0)).is_empty());
    }

    #[test]
    fn marks_layout_and_overridden_blocks() {
        let files = project();
        let d = doc("/p/app/UI/Home/default.latte", PAGE);
        let markers = line_markers(Some(&files), &d);
        let summary: Vec<(u32, &str)> = markers.iter().map(|m| (m.line, m.tooltip.as_str())).collect();
        assert_eq!(
            summary,
            [(0, "Go to @layout.latte"), (2, "Overrides block 'title' in @layout.latte")],
            "content has no {{block}} in the layout (only an include), local is not inherited"
        );
        let title = markers[1].target.as_ref().unwrap();
        assert_eq!(title.range.start, pos(LAYOUT, "title", 0));
        assert!(line_markers(None, &d).is_empty());
    }

    #[test]
    fn works_with_the_host_project() {
        let host = openidea_plugin::mock::MockHost::install();
        host.add_file("app/UI/@layout.latte", LAYOUT);
        host.add_file("app/UI/Home/default.latte", PAGE);
        let d = doc("/project/app/UI/Home/default.latte", PAGE);
        let files = ProjectFiles::for_path(&d.path, &IndexCache::default()).expect("the mock project contains the document");
        let menu = definition(Some(&files), &d, pos(PAGE, "{include menu", 10));
        assert_eq!(menu[0].path, "/project/app/UI/@layout.latte");
        assert_eq!(line_markers(Some(&files), &d).len(), 2);
    }

    #[test]
    fn survives_broken_input() {
        let files = project();
        for src in ["{include", "{include '", "{layout \"", "{include #", "<div n:block=\"", "{block", "{syntax off}", "{include x from"] {
            let d = doc("/p/app/UI/x.latte", src);
            let _ = parse(src);
            for col in 0..=src.len() as u32 {
                let _ = definition(Some(&files), &d, Position::new(0, col));
            }
            let _ = line_markers(Some(&files), &d);
        }
    }
}
