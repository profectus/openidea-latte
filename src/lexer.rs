use openidea_plugin::syntax::{Highlight, Token};

const CONTROL_TAGS: &[&str] = &[
    "if", "elseif", "else", "ifset", "elseifset", "ifchanged",
    "foreach", "as", "for", "while", "continueIf", "breakIf",
    "first", "last", "sep",
    "switch", "case", "default",
    "try", "rollback",
    "var", "default", "varType", "templateType", "parameters", "capture",
];

const MACRO_TAGS: &[&str] = &[
    "block", "define", "snippet", "snippetArea",
    "include", "import", "embed", "sandbox",
    "layout", "extends",
    "spaceless", "syntax", "contentType", "status",
];

const BUILTIN_TAGS: &[&str] = &[
    "dump", "debugbreak", "php", "trace", "line", "l", "r",
];

const ACTION_TAGS: &[&str] = &[
    "link", "plink", "control",
    "form", "formContainer", "formPrint", "input", "label", "inputError",
];

const LATTE_FILTERS: &[&str] = &[
    "truncate", "substr", "trim", "striptags", "stripHtml", "strip", "webalize",
    "toAscii", "lower", "upper", "firstUpper", "capitalize", "indent",
    "padLeft", "padRight", "repeat", "replace", "replaceRe",
    "implode", "explode", "split", "join", "slice", "reverse", "sort",
    "batch", "length", "first", "last", "random", "clamp",
    "date", "timeAgoInWords", "number", "bytes", "dataStream",
    "escapeHtml", "escapeHtmlAttr", "escapeJs", "escapeCss", "escapeIcalendar",
    "escapeXml", "noescape", "checkUrl", "query",
];

const PHP_KEYWORDS: &[&str] = &[
    "and", "or", "xor", "as", "in", "instanceof", "new", "clone", "fn", "function",
];

const PHP_ATOMS: &[&str] = &[
    "true", "false", "null", "TRUE", "FALSE", "NULL",
];

pub fn highlights(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0;

    while i < len {
        // 1. Latte comment: {* ... *}
        if bytes[i..].starts_with(b"{*") {
            let start = i;
            i += 2;
            let is_doc = bytes[start..].starts_with(b"{**");
            let end = match text[i..].find("*}") {
                Some(pos) => i + pos + 2,
                None => len,
            };
            i = end;
            tokens.push(Token {
                range: start..end,
                kind: if is_doc { Highlight::DocComment } else { Highlight::Comment },
            });
            continue;
        }

        // 2. HTML comment: <!-- ... -->
        if bytes[i..].starts_with(b"<!--") {
            let start = i;
            i += 4;
            let end = match text[i..].find("-->") {
                Some(pos) => i + pos + 3,
                None => len,
            };
            i = end;
            tokens.push(Token {
                range: start..end,
                kind: Highlight::Comment,
            });
            continue;
        }

        // 3. Latte tag: {tag ...} or {$var} or {=expr}
        if bytes[i] == b'{'
            && i + 1 < len
            && !bytes[i + 1].is_ascii_whitespace()
            && bytes[i + 1] != b'{'
        {
            lex_latte_tag(text, bytes, &mut i, &mut tokens);
            continue;
        }

        // 4. HTML tag: <tag ...> or </tag>
        if bytes[i] == b'<'
            && i + 1 < len
            && (bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'/' || bytes[i + 1] == b'!')
        {
            lex_html_tag(text, bytes, &mut i, &mut tokens);
            continue;
        }

        // Plain character
        let ch = text[i..].chars().next().unwrap_or(' ');
        i += ch.len_utf8();
    }

    sanitize_tokens(tokens, len)
}

fn lex_latte_tag(text: &str, bytes: &[u8], i: &mut usize, tokens: &mut Vec<Token>) {
    let tag_open = *i;
    *i += 1; // skip '{'
    tokens.push(Token {
        range: tag_open..*i,
        kind: Highlight::Tag,
    });

    let mut prev_token_is_arrow = false;
    let mut prev_token_is_block_decl = false;

    while *i < bytes.len() {
        let b = bytes[*i];

        // End of tag
        if b == b'}' {
            let tag_close = *i;
            *i += 1;
            tokens.push(Token {
                range: tag_close..*i,
                kind: Highlight::Tag,
            });
            return;
        }

        if b.is_ascii_whitespace() {
            *i += 1;
            continue;
        }

        // Strings inside tag: '...' or "..."
        if b == b'\'' || b == b'"' {
            prev_token_is_arrow = false;
            prev_token_is_block_decl = false;
            let start = *i;
            let quote = b;
            *i += 1;
            while *i < bytes.len() {
                if bytes[*i] == b'\\' && *i + 1 < bytes.len() {
                    *i += 2;
                } else if bytes[*i] == quote {
                    *i += 1;
                    break;
                } else if bytes[*i] == b'\n' {
                    break;
                } else {
                    let ch = text[*i..].chars().next().unwrap_or(' ');
                    *i += ch.len_utf8();
                }
            }
            tokens.push(Token {
                range: start..*i,
                kind: Highlight::String,
            });
            continue;
        }

        // Variables: $name
        if b == b'$' {
            prev_token_is_arrow = false;
            prev_token_is_block_decl = false;
            let start = *i;
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
            let name = &text[start..*i];
            let kind = if name == "$this" || name == "$iterator" {
                Highlight::SelfVariable
            } else {
                Highlight::Variable
            };
            tokens.push(Token {
                range: start..*i,
                kind,
            });
            continue;
        }

        // Filter: |name
        if b == b'|' && *i + 1 < bytes.len() && (bytes[*i + 1].is_ascii_alphabetic() || bytes[*i + 1] == b'_') {
            prev_token_is_arrow = false;
            prev_token_is_block_decl = false;
            let op_start = *i;
            *i += 1;
            tokens.push(Token {
                range: op_start..*i,
                kind: Highlight::Operator,
            });

            let name_start = *i;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
            let filter_name = &text[name_start..*i];
            let kind = if LATTE_FILTERS.contains(&filter_name) {
                Highlight::Builtin
            } else {
                Highlight::FunctionCall
            };
            tokens.push(Token {
                range: name_start..*i,
                kind,
            });
            continue;
        }

        // Closing tag: /if, /foreach, etc.
        if b == b'/' && *i + 1 < bytes.len() && bytes[*i + 1].is_ascii_alphabetic() {
            prev_token_is_arrow = false;
            prev_token_is_block_decl = false;
            let start = *i;
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
            tokens.push(Token {
                range: start..*i,
                kind: Highlight::Keyword,
            });
            continue;
        }

        // Numbers
        if b.is_ascii_digit() {
            prev_token_is_arrow = false;
            prev_token_is_block_decl = false;
            let start = *i;
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_digit() || bytes[*i] == b'.') {
                *i += 1;
            }
            tokens.push(Token {
                range: start..*i,
                kind: Highlight::Number,
            });
            continue;
        }

        // Identifiers
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = *i;
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
            let word = &text[start..*i];

            let is_followed_by_paren = *i < bytes.len() && bytes[*i] == b'(';

            let kind = if prev_token_is_arrow {
                if is_followed_by_paren {
                    Highlight::FunctionCall
                } else {
                    Highlight::Property
                }
            } else if prev_token_is_block_decl {
                Highlight::Function
            } else if CONTROL_TAGS.contains(&word) {
                Highlight::Keyword
            } else if MACRO_TAGS.contains(&word) {
                prev_token_is_block_decl = matches!(word, "block" | "define" | "snippet" | "snippetArea");
                Highlight::Macro
            } else if BUILTIN_TAGS.contains(&word) {
                Highlight::Builtin
            } else if ACTION_TAGS.contains(&word) {
                Highlight::FunctionCall
            } else if PHP_KEYWORDS.contains(&word) {
                Highlight::Keyword
            } else if PHP_ATOMS.contains(&word) {
                Highlight::Atom
            } else if is_followed_by_paren {
                Highlight::FunctionCall
            } else {
                Highlight::Constant
            };

            prev_token_is_arrow = false;
            if !matches!(word, "block" | "define" | "snippet" | "snippetArea") {
                prev_token_is_block_decl = false;
            }

            tokens.push(Token {
                range: start..*i,
                kind,
            });
            continue;
        }

        // Multi-character operators
        let op_len = match &bytes[*i..] {
            b if b.starts_with(b"?->") => 3,
            b if b.starts_with(b"===") || b.starts_with(b"!==") || b.starts_with(b"<=>") => 3,
            b if b.starts_with(b"=>") || b.starts_with(b"->") || b.starts_with(b"::") => 2,
            b if b.starts_with(b"==") || b.starts_with(b"!=") || b.starts_with(b"<=") || b.starts_with(b">=") => 2,
            b if b.starts_with(b"&&") || b.starts_with(b"||") || b.starts_with(b"??") => 2,
            b if b.starts_with(b"++") || b.starts_with(b"--") || b.starts_with(b"+=") || b.starts_with(b"-=") => 2,
            _ => 0,
        };

        if op_len > 0 {
            let op_str = &text[*i..*i + op_len];
            prev_token_is_arrow = op_str == "->" || op_str == "?->";
            prev_token_is_block_decl = false;

            tokens.push(Token {
                range: *i..*i + op_len,
                kind: Highlight::Operator,
            });
            *i += op_len;
            continue;
        }

        // Single-character operators
        if b"+-*/%=!<>&|^~?:".contains(&b) {
            prev_token_is_arrow = false;
            prev_token_is_block_decl = false;
            tokens.push(Token {
                range: *i..*i + 1,
                kind: Highlight::Operator,
            });
            *i += 1;
            continue;
        }

        if b"()[],;".contains(&b) {
            prev_token_is_arrow = false;
            tokens.push(Token {
                range: *i..*i + 1,
                kind: Highlight::Punctuation,
            });
            *i += 1;
            continue;
        }

        let ch = text[*i..].chars().next().unwrap_or(' ');
        *i += ch.len_utf8();
    }
}

fn lex_html_tag(text: &str, bytes: &[u8], i: &mut usize, tokens: &mut Vec<Token>) {
    let tag_open = *i;
    *i += 1; // skip '<'

    if *i < bytes.len() && bytes[*i] == b'/' {
        *i += 1;
    }

    while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'-' || bytes[*i] == b':') {
        *i += 1;
    }

    tokens.push(Token {
        range: tag_open..*i,
        kind: Highlight::Tag,
    });

    while *i < bytes.len() {
        let b = bytes[*i];

        if b == b'>' {
            tokens.push(Token {
                range: *i..*i + 1,
                kind: Highlight::Tag,
            });
            *i += 1;
            break;
        }

        if bytes[*i..].starts_with(b"/>") {
            tokens.push(Token {
                range: *i..*i + 2,
                kind: Highlight::Tag,
            });
            *i += 2;
            break;
        }

        if b == b'{'
            && *i + 1 < bytes.len()
            && !bytes[*i + 1].is_ascii_whitespace()
            && bytes[*i + 1] != b'{'
        {
            lex_latte_tag(text, bytes, i, tokens);
            continue;
        }

        if b.is_ascii_whitespace() {
            *i += 1;
            continue;
        }

        // Attribute name: class, id, n:if, n:foreach, etc.
        if b.is_ascii_alphabetic() || b == b'_' || b == b':' || b == b'@' {
            let attr_start = *i;
            *i += 1;
            while *i < bytes.len()
                && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_' || bytes[*i] == b'-' || bytes[*i] == b':')
            {
                *i += 1;
            }
            let attr_name = &text[attr_start..*i];
            let is_n_attr = attr_name.starts_with("n:");

            tokens.push(Token {
                range: attr_start..*i,
                kind: Highlight::Attribute,
            });

            while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
                *i += 1;
            }

            if *i < bytes.len() && bytes[*i] == b'=' {
                tokens.push(Token {
                    range: *i..*i + 1,
                    kind: Highlight::Operator,
                });
                *i += 1;

                while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
                    *i += 1;
                }

                if *i < bytes.len() && (bytes[*i] == b'"' || bytes[*i] == b'\'') {
                    let quote = bytes[*i];
                    let val_open = *i;
                    *i += 1;

                    if is_n_attr {
                        tokens.push(Token {
                            range: val_open..*i,
                            kind: Highlight::Punctuation,
                        });

                        lex_latte_expr(text, bytes, i, quote, tokens);

                        if *i < bytes.len() && bytes[*i] == quote {
                            tokens.push(Token {
                                range: *i..*i + 1,
                                kind: Highlight::Punctuation,
                            });
                            *i += 1;
                        }
                    } else {
                        lex_html_attr_value(text, bytes, i, val_open, quote, tokens);
                    }
                }
            }
            continue;
        }

        let ch = text[*i..].chars().next().unwrap_or(' ');
        *i += ch.len_utf8();
    }
}

fn lex_latte_expr(text: &str, bytes: &[u8], i: &mut usize, quote: u8, tokens: &mut Vec<Token>) {
    let mut prev_token_is_arrow = false;

    while *i < bytes.len() {
        let b = bytes[*i];
        if b == quote {
            break;
        }
        if b.is_ascii_whitespace() {
            *i += 1;
            continue;
        }

        // Substrings inside expression
        if (b == b'\'' || b == b'"') && b != quote {
            prev_token_is_arrow = false;
            let sub_quote = b;
            let start = *i;
            *i += 1;
            while *i < bytes.len() && bytes[*i] != quote {
                if bytes[*i] == b'\\' && *i + 1 < bytes.len() {
                    *i += 2;
                } else if bytes[*i] == sub_quote {
                    *i += 1;
                    break;
                } else {
                    let ch = text[*i..].chars().next().unwrap_or(' ');
                    *i += ch.len_utf8();
                }
            }
            tokens.push(Token {
                range: start..*i,
                kind: Highlight::String,
            });
            continue;
        }

        // Variables: $var
        if b == b'$' {
            prev_token_is_arrow = false;
            let start = *i;
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
            let name = &text[start..*i];
            let kind = if name == "$this" || name == "$iterator" {
                Highlight::SelfVariable
            } else {
                Highlight::Variable
            };
            tokens.push(Token {
                range: start..*i,
                kind,
            });
            continue;
        }

        // Filter: |filter
        if b == b'|' && *i + 1 < bytes.len() && (bytes[*i + 1].is_ascii_alphabetic() || bytes[*i + 1] == b'_') {
            prev_token_is_arrow = false;
            let op_start = *i;
            *i += 1;
            tokens.push(Token {
                range: op_start..*i,
                kind: Highlight::Operator,
            });

            let name_start = *i;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
            let filter_name = &text[name_start..*i];
            let kind = if LATTE_FILTERS.contains(&filter_name) {
                Highlight::Builtin
            } else {
                Highlight::FunctionCall
            };
            tokens.push(Token {
                range: name_start..*i,
                kind,
            });
            continue;
        }

        // Numbers
        if b.is_ascii_digit() {
            prev_token_is_arrow = false;
            let start = *i;
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_digit() || bytes[*i] == b'.') {
                *i += 1;
            }
            tokens.push(Token {
                range: start..*i,
                kind: Highlight::Number,
            });
            continue;
        }

        // Identifiers
        if b.is_ascii_alphabetic() || b == b'_' {
            let start = *i;
            *i += 1;
            while *i < bytes.len() && (bytes[*i].is_ascii_alphanumeric() || bytes[*i] == b'_') {
                *i += 1;
            }
            let word = &text[start..*i];
            let is_followed_by_paren = *i < bytes.len() && bytes[*i] == b'(';

            let kind = if prev_token_is_arrow {
                if is_followed_by_paren {
                    Highlight::FunctionCall
                } else {
                    Highlight::Property
                }
            } else if CONTROL_TAGS.contains(&word) || PHP_KEYWORDS.contains(&word) {
                Highlight::Keyword
            } else if PHP_ATOMS.contains(&word) {
                Highlight::Atom
            } else if is_followed_by_paren {
                Highlight::FunctionCall
            } else {
                Highlight::Constant
            };

            prev_token_is_arrow = false;

            tokens.push(Token {
                range: start..*i,
                kind,
            });
            continue;
        }

        // Multi-char operators
        let op_len = match &bytes[*i..] {
            b if b.starts_with(b"?->") => 3,
            b if b.starts_with(b"===") || b.starts_with(b"!==") || b.starts_with(b"<=>") => 3,
            b if b.starts_with(b"=>") || b.starts_with(b"->") || b.starts_with(b"::") => 2,
            b if b.starts_with(b"==") || b.starts_with(b"!=") || b.starts_with(b"<=") || b.starts_with(b">=") => 2,
            b if b.starts_with(b"&&") || b.starts_with(b"||") || b.starts_with(b"??") => 2,
            _ => 0,
        };

        if op_len > 0 {
            let op_str = &text[*i..*i + op_len];
            prev_token_is_arrow = op_str == "->" || op_str == "?->";

            tokens.push(Token {
                range: *i..*i + op_len,
                kind: Highlight::Operator,
            });
            *i += op_len;
            continue;
        }

        // Single-char operators
        if b"+-*/%=!<>&|^~?:".contains(&b) {
            prev_token_is_arrow = false;
            tokens.push(Token {
                range: *i..*i + 1,
                kind: Highlight::Operator,
            });
            *i += 1;
            continue;
        }

        if b"()[],;".contains(&b) {
            prev_token_is_arrow = false;
            tokens.push(Token {
                range: *i..*i + 1,
                kind: Highlight::Punctuation,
            });
            *i += 1;
            continue;
        }

        let ch = text[*i..].chars().next().unwrap_or(' ');
        *i += ch.len_utf8();
    }
}

fn lex_html_attr_value(
    text: &str,
    bytes: &[u8],
    i: &mut usize,
    val_open: usize,
    quote: u8,
    tokens: &mut Vec<Token>,
) {
    let mut str_segment_start = val_open;

    while *i < bytes.len() {
        let b = bytes[*i];
        if b == quote {
            *i += 1;
            if str_segment_start < *i {
                tokens.push(Token {
                    range: str_segment_start..*i,
                    kind: Highlight::String,
                });
            }
            break;
        }

        if b == b'{'
            && *i + 1 < bytes.len()
            && !bytes[*i + 1].is_ascii_whitespace()
            && bytes[*i + 1] != b'{'
        {
            if str_segment_start < *i {
                tokens.push(Token {
                    range: str_segment_start..*i,
                    kind: Highlight::String,
                });
            }
            lex_latte_tag(text, bytes, i, tokens);
            str_segment_start = *i;
            continue;
        }

        let ch = text[*i..].chars().next().unwrap_or(' ');
        *i += ch.len_utf8();
    }
}

fn sanitize_tokens(mut tokens: Vec<Token>, max_len: usize) -> Vec<Token> {
    tokens.retain_mut(|t| {
        t.range.start = t.range.start.min(max_len);
        t.range.end = t.range.end.min(max_len);
        t.range.start < t.range.end
    });

    tokens.sort_by(|a, b| {
        a.range.start.cmp(&b.range.start).then_with(|| a.range.end.cmp(&b.range.end))
    });

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
