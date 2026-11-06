//! OpenIDEA Latte Plugin: Syntax highlighting, outline symbols, code folding,
//! and autocompletion for Nette Latte templates (.latte).

mod completion;
mod folding;
mod lexer;
mod outline;

use openidea_plugin::providers::{CompletionItem, Document, Providers};
use openidea_plugin::syntax::{Fold, Symbol, SyntaxProvider, Token};
use openidea_plugin::{Error, Invocation, Plugin, Position};

#[derive(Default)]
pub struct Latte;

impl Plugin for Latte {
    fn run_command(
        &mut self,
        id: &str,
        _ctx: &Invocation,
        _arg: Option<&str>,
    ) -> Result<Option<String>, Error> {
        Err(Error::NotFound(format!("command {id}")))
    }
}

impl SyntaxProvider for Latte {
    fn highlights(&mut self, _language: &str, text: &str) -> Vec<Token> {
        lexer::highlights(text)
    }

    fn outline(&mut self, _language: &str, text: &str) -> Vec<Symbol> {
        outline::outline(text)
    }

    fn folding(&mut self, _language: &str, text: &str) -> Vec<Fold> {
        folding::folding(text)
    }
}

impl Providers for Latte {
    fn completion(
        &mut self,
        doc: &Document,
        at: Position,
        trigger: Option<&str>,
    ) -> Vec<CompletionItem> {
        completion::completion(doc, at, trigger)
    }
}

#[cfg(not(target_arch = "wasm32"))]
const _: fn() -> Latte = <Latte as ::core::default::Default>::default;

#[cfg(target_arch = "wasm32")]
const _: () = {
    struct __OpenideaComponent;
    openidea_plugin::__plugin_exports!(Latte, __OpenideaComponent, openidea_plugin::bindings);
    openidea_plugin::__providers_exports!(Latte, __OpenideaComponent);

    impl openidea_plugin::syntax_bindings::exports::openidea::plugin::syntax_provider::Guest for __OpenideaComponent {
        fn highlights(
            language: ::std::string::String,
            text: ::std::string::String,
        ) -> ::std::vec::Vec<openidea_plugin::syntax_bindings::exports::openidea::plugin::syntax_provider::Token> {
            openidea_plugin::__rt::highlights::<Latte>(&language, &text)
        }
        fn outline(
            language: ::std::string::String,
            text: ::std::string::String,
        ) -> ::std::vec::Vec<openidea_plugin::syntax_bindings::exports::openidea::plugin::syntax_provider::Symbol> {
            openidea_plugin::__rt::outline::<Latte>(&language, &text)
        }
        fn folding(
            language: ::std::string::String,
            text: ::std::string::String,
        ) -> ::std::vec::Vec<openidea_plugin::syntax_bindings::exports::openidea::plugin::syntax_provider::Fold> {
            openidea_plugin::__rt::folding::<Latte>(&language, &text)
        }
    }

    openidea_plugin::bindings::__export_plugin_world!(__OpenideaComponent with_types_in openidea_plugin::bindings);
    openidea_plugin::syntax_bindings::__export_syntax_world!(__OpenideaComponent with_types_in openidea_plugin::syntax_bindings);
};

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::syntax::{FoldKind, Highlight, SymbolKind};

    const SAMPLE: &str = r#"{layout '@layout.latte'}

{* Komentář v Latte šabloně se speciálními znaky:
   Příliš žluťoučký kůň *}
<!-- Běžný HTML komentář -->

{block content}
<div id="main" class="container {$extraClass}" n:if="$user->isLoggedIn()">
    <h1 n:block="title">Vítejte {$user->name|upper}!</h1>
    
    <ul n:if="$items">
        <li n:foreach="$items as $key => $item" n:class="$iterator->isFirst() ? first">
            {$key}: {$item->title|truncate:20}
        </li>
    </ul>

    <p n:else>Nebyly nalezeny žádné položky.</p>
</div>
{/block}
"#;

    #[test]
    fn highlights_produce_valid_sorted_tokens() {
        let tokens = lexer::highlights(SAMPLE);
        assert!(!tokens.is_empty());

        for w in tokens.windows(2) {
            assert!(
                w[0].range.end <= w[1].range.start,
                "Overlapping tokens: {:?} and {:?}",
                w[0],
                w[1]
            );
        }

        for t in &tokens {
            assert!(t.range.start < t.range.end);
            assert!(t.range.end <= SAMPLE.len());
            assert!(SAMPLE.is_char_boundary(t.range.start));
            assert!(SAMPLE.is_char_boundary(t.range.end));
        }
    }

    #[test]
    fn highlights_tags_and_variables() {
        let tokens = lexer::highlights(SAMPLE);

        let find_kind = |sub: &str| {
            let at = SAMPLE.find(sub).unwrap();
            tokens.iter().find(|t| t.range.start == at).map(|t| t.kind)
        };

        assert_eq!(find_kind("{layout"), Some(Highlight::Tag));
        assert_eq!(find_kind("layout"), Some(Highlight::Macro));
        assert_eq!(find_kind("{* Komentář"), Some(Highlight::Comment));
        assert_eq!(find_kind("<!-- Běžný"), Some(Highlight::Comment));
        assert_eq!(find_kind("{block"), Some(Highlight::Tag));
        assert_eq!(find_kind("block"), Some(Highlight::Macro));
        assert_eq!(find_kind("content"), Some(Highlight::Function));
        assert_eq!(find_kind("<div"), Some(Highlight::Tag));
        assert_eq!(find_kind("id="), Some(Highlight::Attribute));
        assert_eq!(find_kind("n:if="), Some(Highlight::Attribute));
        assert_eq!(find_kind("$user"), Some(Highlight::Variable));
        assert_eq!(find_kind("isLoggedIn"), Some(Highlight::FunctionCall));
        assert_eq!(find_kind("n:block="), Some(Highlight::Attribute));
        assert_eq!(find_kind("n:foreach="), Some(Highlight::Attribute));
        assert_eq!(find_kind("$iterator"), Some(Highlight::SelfVariable));
        assert_eq!(find_kind("isFirst"), Some(Highlight::FunctionCall));
        assert_eq!(find_kind("truncate"), Some(Highlight::Builtin));
    }

    #[test]
    fn outline_extracts_blocks_and_hierarchy() {
        let symbols = outline::outline(SAMPLE);
        assert!(!symbols.is_empty());

        for (i, s) in symbols.iter().enumerate() {
            if let Some(p) = s.parent {
                assert!(p < i, "Parent {} must come before child {}", p, i);
            }
        }

        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"layout @layout.latte"));
        assert!(names.contains(&"content"));
        assert!(names.contains(&"title"));

        let content_sym = symbols.iter().find(|s| s.name == "content").unwrap();
        assert_eq!(content_sym.kind, SymbolKind::Function);
        assert_eq!(content_sym.detail.as_deref(), Some("block"));

        let title_sym = symbols.iter().find(|s| s.name == "title").unwrap();
        assert_eq!(title_sym.kind, SymbolKind::Function);
        assert_eq!(title_sym.detail.as_deref(), Some("n:block"));
    }

    #[test]
    fn folding_extracts_regions() {
        let folds = folding::folding(SAMPLE);
        assert!(!folds.is_empty());

        for f in &folds {
            assert!(f.start_line < f.end_line);
        }

        assert!(folds.iter().any(|f| f.kind == FoldKind::Comment));
        assert!(folds.iter().any(|f| f.kind == FoldKind::Region));
    }

    #[test]
    fn completion_suggests_tags_and_attributes() {
        let doc = Document {
            path: "sample.latte".to_string(),
            language: "latte".to_string(),
            version: 1,
            text: "{i".to_string(),
        };

        // Autocomplete after '{i'
        let items = completion::completion(&doc, Position::new(0, 2), None);
        assert!(!items.is_empty());
        let labels: Vec<&str> = items.iter().map(|it| it.label.as_str()).collect();
        assert!(labels.contains(&"if"));
        assert!(labels.contains(&"ifset"));

        // Autocomplete after '|' (filters)
        let filter_doc = Document {
            path: "sample.latte".to_string(),
            language: "latte".to_string(),
            version: 1,
            text: "{$item->title|".to_string(),
        };
        let filter_items = completion::completion(&filter_doc, Position::new(0, 14), Some("|"));
        let filter_labels: Vec<&str> = filter_items.iter().map(|it| it.label.as_str()).collect();
        assert!(filter_labels.contains(&"truncate"));
        assert!(filter_labels.contains(&"upper"));
        assert!(filter_labels.contains(&"date"));

        // Autocomplete inside HTML tag (n: attributes)
        let html_doc = Document {
            path: "sample.latte".to_string(),
            language: "latte".to_string(),
            version: 1,
            text: "<div n:".to_string(),
        };
        let n_items = completion::completion(&html_doc, Position::new(0, 7), None);
        let n_labels: Vec<&str> = n_items.iter().map(|it| it.label.as_str()).collect();
        assert!(n_labels.contains(&"n:if"));
        assert!(n_labels.contains(&"n:foreach"));
        assert!(n_labels.contains(&"n:class"));
    }

    #[test]
    fn resilience_against_broken_inputs() {
        let broken_inputs = [
            "",
            "{",
            "}",
            "{*",
            "<!--",
            "<?php",
            "{$",
            "{if",
            "{block",
            "<div n:if=\"$foo",
            "\"unterminated string",
            "'unterminated string",
            "Příliš žluťoučký kůň úpěl ďábelské ódy",
            "{* comment with unicode: ěščřžýáíé *}",
            "{|broken|filter",
            "{=1 +}",
        ];

        for input in broken_inputs {
            let tokens = lexer::highlights(input);
            for w in tokens.windows(2) {
                assert!(w[0].range.end <= w[1].range.start);
            }
            for t in &tokens {
                assert!(t.range.start < t.range.end);
                assert!(t.range.end <= input.len());
                assert!(input.is_char_boundary(t.range.start));
                assert!(input.is_char_boundary(t.range.end));
            }

            let _ = outline::outline(input);
            let _ = folding::folding(input);
        }
    }
}
