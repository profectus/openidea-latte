use openidea_plugin::providers::{CompletionItem, CompletionKind, Document};
use openidea_plugin::Position;

pub fn completion(doc: &Document, at: Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    let line = doc.text.lines().nth(at.line as usize).unwrap_or("");
    let char_idx = line
        .char_indices()
        .nth(at.character as usize)
        .map_or(line.len(), |(i, _)| i);
    let prefix = &line[..char_idx];
    let trimmed_prefix = prefix.trim_end();

    // Context 1: Filter completion (after '|')
    if let Some(pipe_pos) = prefix.rfind('|') {
        let after_pipe = &prefix[pipe_pos + 1..];
        if !after_pipe.contains([' ', '}', '"', '\'', '>']) {
            add_filter_completions(&mut items);
            return items;
        }
    }

    // Context 2: Variable completion (after '$')
    if let Some(dollar_pos) = prefix.rfind('$') {
        let after_dollar = &prefix[dollar_pos + 1..];
        if !after_dollar.contains([' ', '}', '"', '\'', '>', ')', ',', ';']) {
            add_variable_completions(&mut items, &doc.text);
            return items;
        }
    }

    // Context 3: n:attribute completion (inside HTML tag or typing n:)
    if trimmed_prefix.ends_with("n:") || is_inside_html_tag(prefix) {
        add_n_attribute_completions(&mut items);
        if trimmed_prefix.ends_with("n:") {
            return items;
        }
    }

    // Context 4: Latte tag completion (after '{' or typing tag name)
    if let Some(brace_pos) = prefix.rfind('{') {
        let after_brace = &prefix[brace_pos + 1..];
        if !after_brace.contains(['}', '>', '<']) {
            add_tag_completions(&mut items, after_brace.is_empty());
            return items;
        }
    }

    // Default suggestions: Tags & n:attributes
    add_tag_snippets(&mut items);
    add_n_attribute_completions(&mut items);

    items
}

fn is_inside_html_tag(prefix: &str) -> bool {
    if let Some(last_lt) = prefix.rfind('<') {
        if let Some(last_gt) = prefix.rfind('>') {
            last_lt > last_gt
        } else {
            true
        }
    } else {
        false
    }
}

fn add_tag_completions(items: &mut Vec<CompletionItem>, just_opened_brace: bool) {
    let tags = [
        // Conditions
        ("if", "if ${1:\\$condition}}\n\t$0\n{/if}", "Podmínka {if ...}...{/if}"),
        ("ifset", "ifset ${1:\\$var}}\n\t$0\n{/ifset}", "Podmínka {ifset ...}...{/ifset}"),
        ("ifchanged", "ifchanged ${1:\\$expr}}\n\t$0\n{/ifchanged}", "Podmínka změny {ifchanged ...}"),
        ("elseif", "elseif ${1:\\$condition}}", "Větev {elseif ...}"),
        ("else", "else}", "Větev {else}"),
        // Loops
        ("foreach", "foreach ${1:\\$items} as ${2:\\$item}}\n\t$0\n{/foreach}", "Cyklus {foreach $items as $item}"),
        ("foreach as key => item", "foreach ${1:\\$items} as ${2:\\$key} => ${3:\\$item}}\n\t$0\n{/foreach}", "Cyklus {foreach $items as $key => $item}"),
        ("for", "for ${1:\\$i} = 0; ${1:\\$i} < ${2:10}; ${1:\\$i}++}\n\t$0\n{/for}", "Cyklus {for ...}"),
        ("while", "while ${1:\\$condition}}\n\t$0\n{/while}", "Cyklus {while ...}"),
        ("continueIf", "continueIf ${1:\\$condition}}", "Podmíněný continue"),
        ("breakIf", "breakIf ${1:\\$condition}}", "Podmíněný break"),
        ("first", "first}\n\t$0\n{/first}", "První průchod cyklem {first}"),
        ("last", "last}\n\t$0\n{/last}", "Poslední průchod cyklem {last}"),
        ("sep", "sep}, {/sep}", "Oddělovač prvků cyklu {sep}"),
        // Switch / Match
        ("switch", "switch ${1:\\$expr}}\n\t{case ${2:val}}\n\t\t$0\n\t{default}\n{/switch}", "Přepínač {switch ...}"),
        ("case", "case ${1:value}}", "Větev {case ...}"),
        ("default", "default}", "Výchozí větev {default}"),
        // Try / Rollback
        ("try", "try}\n\t$1\n{rollback}\n\t$0\n{/try}", "Ošetření chyb {try}...{rollback}"),
        // Blocks & Templates
        ("block", "block ${1:content}}\n\t$0\n{/block}", "Blok {block name}"),
        ("define", "define ${1:macroName}}\n\t$0\n{/define}", "Definice makra/šablony {define name}"),
        ("snippet", "snippet ${1:snippetName}}\n\t$0\n{/snippet}", "AJAX snippet {snippet name}"),
        ("snippetArea", "snippetArea ${1:areaName}}\n\t$0\n{/snippetArea}", "Oblast snippetů {snippetArea name}"),
        ("include", "include ${1:blockName}}", "Vložení bloku {include name}"),
        ("layout", "layout '${1:@layout.latte}'}", "Hlavní layout šablony {layout ...}"),
        ("extends", "extends '${1:@layout.latte}'}", "Dědičnost layoutu {extends ...}"),
        ("import", "import '${1:blocks.latte}'}", "Import bloků ze souboru"),
        ("embed", "embed '${1:component.latte}'}\n\t$0\n{/embed}", "Vložení komponenty {embed ...}"),
        // Variables & Parameters
        ("var", "var ${1:\\$name} = ${2:value}}", "Deklarace proměnné {var $x = ...}"),
        ("default", "default ${1:\\$name} = ${2:value}}", "Výchozí hodnota proměnné {default $x = ...}"),
        ("parameters", "parameters ${1:type \\$name = default}}", "Typované parametry šablony {parameters ...}"),
        ("capture", "capture ${1:\\$var}}\n\t$0\n{/capture}", "Zachycení výstupu do proměnné {capture $var}"),
        // Links & UI
        ("link", "link ${1:Presenter:action}}", "Odkaz {link Presenter:action}"),
        ("plink", "plink ${1:Presenter:action}}", "Presenter odkaz {plink Presenter:action}"),
        ("control", "control ${1:name}}", "Vykreslení komponenty {control name}"),
        // Debugging & Helpers
        ("dump", "dump ${1:\\$var}}", "Tracy ladicí výpis {dump $var}"),
        ("debugbreak", "debugbreak}", "Zastavení debuggeru"),
        ("php", "php ${1:\\$code}}", "Vložení PHP kódu"),
        ("spaceless", "spaceless}\n\t$0\n{/spaceless}", "Odstranění nadbytečných mezer"),
        ("contentType", "contentType ${1:text/html}}", "Hlavička Content-Type"),
        // Closing tags
        ("/if", "/if}", "Ukončení {/if}"),
        ("/foreach", "/foreach}", "Ukončení {/foreach}"),
        ("/for", "/for}", "Ukončení {/for}"),
        ("/while", "/while}", "Ukončení {/while}"),
        ("/block", "/block}", "Ukončení {/block}"),
        ("/define", "/define}", "Ukončení {/define}"),
        ("/snippet", "/snippet}", "Ukončení {/snippet}"),
        ("/switch", "/switch}", "Ukončení {/switch}"),
        ("/try", "/try}", "Ukončení {/try}"),
        ("/embed", "/embed}", "Ukončení {/embed}"),
        ("/capture", "/capture}", "Ukončení {/capture}"),
    ];

    for (label, snippet_body, detail) in tags {
        let insert = if just_opened_brace {
            snippet_body.to_string()
        } else {
            // Already inside or typing name
            snippet_body.to_string()
        };

        let mut item = CompletionItem::new(label, CompletionKind::Snippet);
        item.insert_text = insert;
        item.snippet = true;
        item.detail = Some(detail.to_string());
        items.push(item);
    }
}

fn add_tag_snippets(items: &mut Vec<CompletionItem>) {
    let snippets = [
        ("{if ...}", "{if ${1:\\$condition}}\n\t$0\n{/if}", "Podmínka {if}"),
        ("{ifset ...}", "{ifset ${1:\\$var}}\n\t$0\n{/ifset}", "Podmínka {ifset}"),
        ("{foreach ...}", "{foreach ${1:\\$items} as ${2:\\$item}}\n\t$0\n{/foreach}", "Cyklus {foreach}"),
        ("{block ...}", "{block ${1:content}}\n\t$0\n{/block}", "Blok {block}"),
        ("{snippet ...}", "{snippet ${1:name}}\n\t$0\n{/snippet}", "Snippet {snippet}"),
        ("{var ...}", "{var ${1:\\$var} = ${2:value}}", "Proměnná {var}"),
        ("{dump ...}", "{dump ${1:\\$var}}", "Ladicí výpis {dump}"),
    ];

    for (label, snippet, detail) in snippets {
        let mut item = CompletionItem::new(label, CompletionKind::Snippet);
        item.insert_text = snippet.to_string();
        item.snippet = true;
        item.detail = Some(detail.to_string());
        items.push(item);
    }
}

fn add_n_attribute_completions(items: &mut Vec<CompletionItem>) {
    let n_attrs = [
        ("n:if", "n:if=\"${1:\\$condition}\"", "Podmínka elementu"),
        ("n:ifset", "n:ifset=\"${1:\\$var}\"", "Podmínka existence proměnné"),
        ("n:ifchanged", "n:ifchanged=\"${1:\\$val}\"", "Vykreslí element pouze při změně hodnoty"),
        ("n:foreach", "n:foreach=\"${1:\\$items} as ${2:\\$item}\"", "Cyklické opakování elementu"),
        ("n:for", "n:for=\"${1:\\$i} = 0; ${1:\\$i} < ${2:10}; ${1:\\$i}++\"", "Cyklus for na elementu"),
        ("n:while", "n:while=\"${1:\\$condition}\"", "Cyklus while na elementu"),
        ("n:inner-if", "n:inner-if=\"${1:\\$condition}\"", "Podmínka pro vnitřek elementu"),
        ("n:inner-foreach", "n:inner-foreach=\"${1:\\$items} as ${2:\\$item}\"", "Cyklus pro vnitřek elementu"),
        ("n:tag-if", "n:tag-if=\"${1:\\$condition}\"", "Vykreslí obalující HTML tag pouze pokud platí podmínka"),
        ("n:class", "n:class=\"${1:\\$active ? active}\"", "Dynamické třídy elementu"),
        ("n:style", "n:style=\"${1:color => \\$color}\"", "Dynamické inline CSS styly"),
        ("n:attr", "n:attr=\"${1:title => \\$title}\"", "Dynamické HTML atributy"),
        ("n:href", "n:href=\"${1:Presenter:action}\"", "Generování odkazu (href) na presenter"),
        ("n:snippet", "n:snippet=\"${1:name}\"", "Označení AJAX snippetu"),
        ("n:block", "n:block=\"${1:title}\"", "Definice bloku přímo na HTML elementu"),
        ("n:name", "n:name=\"${1:controlName}\"", "Propojení s formulářovým prvkem Nette"),
        ("n:nonce", "n:nonce", "Přidá CSP nonce"),
    ];

    for (label, snippet, detail) in n_attrs {
        let mut item = CompletionItem::new(label, CompletionKind::Property);
        item.insert_text = snippet.to_string();
        item.snippet = true;
        item.detail = Some(detail.to_string());
        items.push(item);
    }
}

fn add_filter_completions(items: &mut Vec<CompletionItem>) {
    let filters = [
        ("truncate", "truncate:${1:30}", "Zkrátí řetězec na maximální délku"),
        ("upper", "upper", "Převede text na velká písmena"),
        ("lower", "lower", "Převede text na malá písmena"),
        ("capitalize", "capitalize", "Převede první písmena slov na velká"),
        ("firstUpper", "firstUpper", "Převede první písmeno textu na velké"),
        ("date", "date:'${1:j. n. Y}'", "Formátuje datum a čas"),
        ("timeAgoInWords", "timeAgoInWords", "Relativní čas ('před 5 minutami')"),
        ("noescape", "noescape", "Vypne kontextové escapování (vypíše surové HTML)"),
        ("stripHtml", "stripHtml", "Odstraní z textu všechny HTML tagy"),
        ("webalize", "webalize", "Upraví text do tvaru URL slugu (odstraní diakritiku)"),
        ("trim", "trim", "Ořeže bílé znaky ze začátku a konce"),
        ("number", "number:${1:2}", "Formátuje číslo se zadaným počtem desetinných míst"),
        ("bytes", "bytes", "Formátuje velikost v bajtech na lidsky čitelný tvar (KB, MB, ...)"),
        ("replace", "replace:${1:search}, ${2:replace}", "Nahradí řetězec za jiný"),
        ("implode", "implode:${1:', '}", "Spojí pole do řetězce"),
        ("explode", "explode:${1:', '}", "Rozdělí řetězec podle oddělovače"),
        ("split", "split:${1:', '}", "Rozdělí řetězec podle oddělovače"),
        ("join", "join:${1:', '}", "Spojí prvky pole"),
        ("reverse", "reverse", "Obrátí pořadí pole nebo řetězce"),
        ("length", "length", "Vrátí délku řetězce nebo pole"),
        ("first", "first", "Vrátí první prvek"),
        ("last", "last", "Vrátí poslední prvek"),
        ("clamp", "clamp:${1:min}, ${2:max}", "Omezí číslo do zadaného intervalu"),
        ("repeat", "repeat:${1:3}", "Zopakuje řetězec n-krát"),
        ("padLeft", "padLeft:${1:10}", "Zarovná text zleva na zadanou délku"),
        ("padRight", "padRight:${1:10}", "Zarovná text zprava na zadanou délku"),
        ("escapeHtml", "escapeHtml", "Explicitní HTML escapování"),
        ("escapeJs", "escapeJs", "Escapování pro vložení do JavaScriptu"),
        ("escapeCss", "escapeCss", "Escapování pro vložení do CSS"),
        ("checkUrl", "checkUrl", "Zkontroluje bezpečnost URL proti javascript: útokům"),
    ];

    for (label, snippet, detail) in filters {
        let mut item = CompletionItem::new(label, CompletionKind::Function);
        item.insert_text = snippet.to_string();
        item.snippet = snippet.contains('$');
        item.detail = Some(detail.to_string());
        items.push(item);
    }
}

fn add_variable_completions(items: &mut Vec<CompletionItem>, text: &str) {
    let builtins = [
        ("$iterator", "Nette\\Utils\\Iterators\\Mapper", "Objekt iterátoru v cyklu foreach"),
        ("$iterator->isFirst()", "bool", "Vrátí true, pokud jde o první průchod cyklem"),
        ("$iterator->isLast()", "bool", "Vrátí true, pokud jde o poslední průchod cyklem"),
        ("$iterator->counter", "int", "Číslo aktuálního průchodu cyklem (od 1)"),
        ("$iterator->odd", "bool", "True při lichém průchodu cyklem"),
        ("$iterator->even", "bool", "True při sudém průchodu cyklem"),
        ("$presenter", "Nette\\Application\\UI\\Presenter", "Aktuální presenter aplikace"),
        ("$control", "Nette\\Application\\UI\\Control", "Aktuální komponenta"),
        ("$user", "Nette\\Security\\User", "Aktuálně přihlášený uživatel"),
        ("$baseUrl", "string", "Základní URL aplikace"),
        ("$basePath", "string", "Cesta ke kořenovému adresáři aplikace"),
    ];

    for (name, type_hint, doc) in builtins {
        let label = name.strip_prefix('$').unwrap_or(name);
        let mut item = CompletionItem::new(label, CompletionKind::Variable);
        item.insert_text = label.to_string();
        item.detail = Some(type_hint.to_string());
        item.documentation = Some(doc.to_string());
        items.push(item);
    }

    // Scan the text for custom template variables: $foo
    let mut seen = std::collections::HashSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > start {
                let var_name = &text[start..j];
                if seen.insert(var_name.to_string()) && !builtins.iter().any(|(b, _, _)| b.ends_with(var_name)) {
                    let mut item = CompletionItem::new(var_name, CompletionKind::Variable);
                    item.insert_text = var_name.to_string();
                    item.detail = Some("Proměnná šablony".to_string());
                    items.push(item);
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
}
