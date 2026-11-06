use openidea_plugin::syntax::{Symbol, SymbolKind};
use std::ops::Range;

struct OpenBlock {
    index: usize,
    tag_name: String,
}

pub fn outline(text: &str) -> Vec<Symbol> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut stack: Vec<OpenBlock> = Vec::new();
    let mut i = 0;

    while i < len {
        // Skip comments
        if bytes[i..].starts_with(b"{*") {
            if let Some(pos) = text[i + 2..].find("*}") {
                i += 2 + pos + 2;
            } else {
                break;
            }
            continue;
        }

        if bytes[i..].starts_with(b"<!--") {
            if let Some(pos) = text[i + 4..].find("-->") {
                i += 4 + pos + 3;
            } else {
                break;
            }
            continue;
        }

        // Latte tag: {tag ...}
        if bytes[i] == b'{'
            && i + 1 < len
            && !bytes[i + 1].is_ascii_whitespace()
            && bytes[i + 1] != b'{'
        {
            let tag_start = i;
            let tag_close = match text[i..].find('}') {
                Some(pos) => i + pos + 1,
                None => len,
            };
            i = tag_close;

            let tag_content = &text[tag_start + 1..tag_close - 1].trim();

            // Closing tags: {/block}, {/define}, {/snippet}, {/snippetArea}
            if tag_content.starts_with('/') {
                let close_name = tag_content[1..].trim();
                if let Some(pos) = stack.iter().rposition(|open| open.tag_name == close_name) {
                    let open = stack.remove(pos);
                    symbols[open.index].range.end = tag_close;
                }
                continue;
            }

            // Extract tag command and argument
            let mut parts = tag_content.split_whitespace();
            let Some(cmd) = parts.next() else { continue };

            match cmd {
                "block" | "define" | "snippet" | "snippetArea" => {
                    let name_arg = parts.next().unwrap_or("unnamed").trim_matches(['\'', '"', '#']);
                    let name = name_arg.to_string();

                    // Find range of the name inside original text
                    let name_range = find_subslice_range(text, tag_start..tag_close, name_arg)
                        .unwrap_or(tag_start + 1..tag_close - 1);

                    let kind = match cmd {
                        "block" => SymbolKind::Function,
                        "define" => SymbolKind::Macro,
                        "snippet" | "snippetArea" => SymbolKind::Field,
                        _ => SymbolKind::Function,
                    };

                    let parent = stack.last().map(|open| open.index);
                    let sym_idx = symbols.len();

                    symbols.push(Symbol {
                        name,
                        detail: Some(cmd.to_string()),
                        kind,
                        range: tag_start..tag_close, // updated when closed
                        name_range,
                        parent,
                    });

                    stack.push(OpenBlock {
                        index: sym_idx,
                        tag_name: cmd.to_string(),
                    });
                }
                "layout" | "extends" => {
                    let path_arg = parts.next().unwrap_or("").trim_matches(['\'', '"']);
                    let name = if path_arg.is_empty() {
                        cmd.to_string()
                    } else {
                        format!("{cmd} {path_arg}")
                    };

                    let name_range = find_subslice_range(text, tag_start..tag_close, path_arg)
                        .unwrap_or(tag_start + 1..tag_close - 1);

                    let parent = stack.last().map(|open| open.index);

                    symbols.push(Symbol {
                        name,
                        detail: Some("template".to_string()),
                        kind: SymbolKind::Package,
                        range: tag_start..tag_close,
                        name_range,
                        parent,
                    });
                }
                "parameters" => {
                    let parent = stack.last().map(|open| open.index);
                    symbols.push(Symbol {
                        name: "parameters".to_string(),
                        detail: Some(tag_content.to_string()),
                        kind: SymbolKind::Variable,
                        range: tag_start..tag_close,
                        name_range: tag_start + 1..tag_start + 1 + "parameters".len(),
                        parent,
                    });
                }
                _ => {}
            }
            continue;
        }

        // Check for n:block or n:snippet inside HTML tags
        if bytes[i] == b'<' && i + 1 < len && bytes[i + 1].is_ascii_alphabetic() {
            let tag_open = i;
            let tag_close = match text[i..].find('>') {
                Some(pos) => i + pos + 1,
                None => len,
            };
            i = tag_close;

            let tag_slice = &text[tag_open..tag_close];
            for (attr_name, kind, detail) in [
                ("n:block", SymbolKind::Function, "n:block"),
                ("n:snippet", SymbolKind::Field, "n:snippet"),
                ("n:define", SymbolKind::Macro, "n:define"),
            ] {
                if let Some(pos) = tag_slice.find(attr_name) {
                    let after = &tag_slice[pos + attr_name.len()..];
                    if let Some(val_start) = after.find('=') {
                        let val_part = after[val_start + 1..].trim_start();
                        if let Some(first_char) = val_part.chars().next() {
                            if first_char == '"' || first_char == '\'' {
                                if let Some(end_quote) = val_part[1..].find(first_char) {
                                    let raw_name = &val_part[1..1 + end_quote];
                                    let name = raw_name.trim_matches('#').to_string();
                                    let abs_name_start = tag_open + pos + attr_name.len() + val_start + 1 + 1;
                                    let name_range = abs_name_start..abs_name_start + raw_name.len();

                                    let parent = stack.last().map(|open| open.index);
                                    symbols.push(Symbol {
                                        name,
                                        detail: Some(detail.to_string()),
                                        kind,
                                        range: tag_open..tag_close,
                                        name_range,
                                        parent,
                                    });
                                }
                            }
                        }
                    }
                }
            }
            continue;
        }

        let ch = text[i..].chars().next().unwrap_or(' ');
        i += ch.len_utf8();
    }

    // Ensure valid ranges and char boundaries
    symbols.retain(|s| {
        s.range.start < s.range.end
            && s.name_range.start <= s.name_range.end
            && text.is_char_boundary(s.range.start)
            && text.is_char_boundary(s.range.end)
            && text.is_char_boundary(s.name_range.start)
            && text.is_char_boundary(s.name_range.end)
    });

    symbols
}

fn find_subslice_range(haystack: &str, within: Range<usize>, needle: &str) -> Option<Range<usize>> {
    if needle.is_empty() || within.end > haystack.len() || within.start >= within.end {
        return None;
    }
    let slice = &haystack[within.clone()];
    let pos = slice.find(needle)?;
    Some(within.start + pos..within.start + pos + needle.len())
}
