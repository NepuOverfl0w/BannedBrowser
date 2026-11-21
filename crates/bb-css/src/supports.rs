//! `@supports` (CSS Conditional 3 §6) evaluation against what BannedBrowser
//! actually implements.

use crate::parser::Cv;

/// Properties (and value restrictions) we implement. Keep in sync with bb-style.
fn supports_declaration(name: &str, value: &[Cv]) -> bool {
    let v: Vec<String> = value.iter().filter_map(|c| c.ident().map(|s| s.to_ascii_lowercase())).collect();
    match name {
        "display" => !v.iter().any(|x| matches!(x.as_str(), "grid" | "inline-grid" | "subgrid" | "ruby")),
        "position" => !v.iter().any(|x| x == "sticky"),
        "color" | "background" | "background-color" | "margin" | "padding" | "border" | "width"
        | "height" | "font" | "font-size" | "font-family" | "font-weight" | "text-align" | "float"
        | "clear" | "line-height" | "max-width" | "min-width" | "box-sizing" | "border-radius"
        | "opacity" | "overflow" | "white-space" | "text-decoration" | "visibility" | "vertical-align"
        | "list-style" | "color-scheme" => true,
        n if n.starts_with("--") => true,
        _ => false,
    }
}

pub(crate) fn evaluate(prelude: &[Cv]) -> bool {
    let items: Vec<&Cv> = prelude.iter().filter(|c| !c.is_ws()).collect();
    eval_items(&items)
}

fn eval_items(items: &[&Cv]) -> bool {
    if items.is_empty() {
        return false;
    }
    if items[0].is_ident("not") {
        return items.get(1).is_some_and(|c| !eval_one(c));
    }
    let mut result = eval_one(items[0]);
    let mut i = 1;
    while i + 1 < items.len() {
        let op = items[i].ident().unwrap_or("").to_ascii_lowercase();
        let rhs = eval_one(items[i + 1]);
        result = match op.as_str() {
            "and" => result && rhs,
            "or" => result || rhs,
            _ => return false,
        };
        i += 2;
    }
    result
}

fn eval_one(cv: &Cv) -> bool {
    match cv {
        Cv::Block('(', inner) => {
            let items: Vec<&Cv> = inner.iter().filter(|c| !c.is_ws()).collect();
            if let (
                Some(Cv::Token(crate::tokenizer::Token::Ident(name))),
                Some(Cv::Token(crate::tokenizer::Token::Colon)),
            ) = (items.first(), items.get(1))
            {
                let value: Vec<Cv> = items[2..].iter().map(|c| (*c).clone()).collect();
                return supports_declaration(&name.to_ascii_lowercase(), &value);
            }
            eval_items(&items)
        }
        Cv::Function(f, args) if f == "selector" => crate::selectors::parse_selector_list(args).is_some(),
        _ => false,
    }
}
