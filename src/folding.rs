use openidea_plugin::syntax::{Fold, FoldKind};

const PAIRED_TAGS: &[&str] = &[
    "block", "define", "snippet", "snippetArea",
    "if", "ifset", "ifchanged",
    "foreach", "for", "while",
    "switch", "try", "embed",
    "form", "formContainer",
    "spaceless", "capture",
];

struct TagOpen {
    name: String,
    line: u32,
}

pub fn folding(text: &str) -> Vec<Fold> {
    let mut folds = Vec::new();
    let bytes = text.as_bytes();
    let len = bytes.len();

    let mut current_line = 0u32;
    let mut tag_stack: Vec<TagOpen> = Vec::new();

    let mut i = 0;
    while i < len {
        let b = bytes[i];

        if b == b'\n' {
            current_line += 1;
            i += 1;
            continue;
        }

        // 1. Latte comments: {* ... *}
        if bytes[i..].starts_with(b"{*") {
            let start_line = current_line;
            let end = match text[i + 2..].find("*}") {
                Some(pos) => i + 2 + pos + 2,
                None => len,
            };
            let comment_str = &text[i..end];
            let newlines = comment_str.matches('\n').count() as u32;
            current_line += newlines;
            let end_line = current_line;
            i = end;

            if end_line > start_line {
                folds.push(Fold {
                    start_line,
                    end_line,
                    kind: FoldKind::Comment,
                });
            }
            continue;
        }

        // 2. HTML comments: <!-- ... -->
        if bytes[i..].starts_with(b"<!--") {
            let start_line = current_line;
            let end = match text[i + 4..].find("-->") {
                Some(pos) => i + 4 + pos + 3,
                None => len,
            };
            let comment_str = &text[i..end];
            let newlines = comment_str.matches('\n').count() as u32;
            current_line += newlines;
            let end_line = current_line;
            i = end;

            if end_line > start_line {
                folds.push(Fold {
                    start_line,
                    end_line,
                    kind: FoldKind::Comment,
                });
            }
            continue;
        }

        // 3. Latte tags: {tag} or {/tag}
        if b == b'{'
            && i + 1 < len
            && !bytes[i + 1].is_ascii_whitespace()
            && bytes[i + 1] != b'{'
        {
            let tag_start = i;
            let tag_start_line = current_line;
            let tag_close = match text[i..].find('}') {
                Some(pos) => i + pos + 1,
                None => len,
            };
            let tag_str = &text[tag_start..tag_close];
            let newlines = tag_str.matches('\n').count() as u32;
            current_line += newlines;
            i = tag_close;

            let tag_inner = tag_str.trim_matches(['{', '}']).trim();

            if let Some(close_name) = tag_inner.strip_prefix('/') {
                let close_name = close_name.trim();
                if let Some(pos) = tag_stack.iter().rposition(|open| open.name == close_name) {
                    let open = tag_stack.remove(pos);
                    if current_line > open.line {
                        folds.push(Fold {
                            start_line: open.line,
                            end_line: current_line,
                            kind: FoldKind::Region,
                        });
                    }
                }
            } else {
                let first_word = tag_inner.split_whitespace().next().unwrap_or("");
                if PAIRED_TAGS.contains(&first_word) {
                    tag_stack.push(TagOpen {
                        name: first_word.to_string(),
                        line: tag_start_line,
                    });
                }
            }
            continue;
        }

        let ch = text[i..].chars().next().unwrap_or(' ');
        i += ch.len_utf8();
    }

    // Sort folds and deduplicate
    folds.sort_by(|a, b| a.start_line.cmp(&b.start_line).then_with(|| a.end_line.cmp(&b.end_line)));
    folds.dedup_by(|a, b| a.start_line == b.start_line && a.end_line == b.end_line);

    folds
}
