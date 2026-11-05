//! HTML/XML helpers shared by the editor (tag auto-closing, Enter between
//! tags) and the checker (tag-balance diagnostics). No external tools: HTML
//! and XML have no standard command-line checker, so this one is built in.

/// Elements that never have content or an end tag.
const HTML_VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements whose end tag HTML lets you leave out, so a missing one isn't
/// an error.
const HTML_OPTIONAL_END: &[&str] = &[
    "html", "head", "body", "p", "li", "dt", "dd", "tr", "td", "th", "thead", "tbody", "tfoot",
    "colgroup", "caption", "option", "optgroup", "rt", "rp",
];

/// Elements whose content is raw text up to their own end tag, so a `<`
/// inside isn't a tag.
const HTML_RAW_TEXT: &[&str] = &["script", "style", "textarea", "title"];

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '-')
}

fn in_list(list: &[&str], name: &str) -> bool {
    list.iter().any(|v| v.eq_ignore_ascii_case(name))
}

/// The element name an opening tag starts with, given the text between its
/// `<` and `>` (or the cursor).
fn opening_tag_name(inner: &str) -> Option<&str> {
    if !inner.chars().next()?.is_ascii_alphabetic() {
        return None; // `</x>`, `<!--`, `<?xml`, `< `, `<3`...
    }
    let end = inner
        .find(|c: char| !is_name_char(c))
        .unwrap_or(inner.len());
    Some(&inner[..end])
}

/// Scans the inside of a tag: whether an unquoted `>` appears, and whether
/// the text ends inside an open quote (an attribute value).
fn scan_tag_text(inner: &str) -> (bool, bool) {
    let mut quote: Option<char> = None;
    for c in inner.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '>' => return (true, false),
            None => {}
        }
    }
    (false, quote.is_some())
}

/// `before` is the line's text up to the cursor, and a `>` is being typed
/// there. If that `>` finishes an opening tag, the closing tag to insert
/// after the cursor (`</div>`).
pub fn closing_tag_for(before: &str, html: bool) -> Option<String> {
    let inner = &before[before.rfind('<')? + 1..];
    let (already_closed, in_quote) = scan_tag_text(inner);
    if already_closed || in_quote {
        return None;
    }
    let name = opening_tag_name(inner)?;
    if inner.trim_end().ends_with('/') || (html && in_list(HTML_VOID, name)) {
        return None; // `<br/>`, `<img>`
    }
    Some(format!("</{name}>"))
}

/// Whether the cursor sits exactly between an opening tag and a closing
/// tag (`<div>|</div>`), where Enter should open up an indented line.
pub fn between_tags(before: &str, after: &str, html: bool) -> bool {
    if !after.starts_with("</") || !before.ends_with('>') {
        return false;
    }
    let Some(start) = before.rfind('<') else {
        return false;
    };
    let inner = &before[start + 1..before.len() - 1];
    let Some(name) = opening_tag_name(inner) else {
        return false;
    };
    !inner.trim_end().ends_with('/') && !(html && in_list(HTML_VOID, name))
}

pub struct MarkupError {
    /// 0-indexed, like the editor's own coordinates.
    pub line: usize,
    pub col: usize,
    pub message: String,
}

fn matches_at(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, p)| chars.get(i + k) == Some(&p))
}

fn find_from(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    (from..chars.len()).find(|&i| matches_at(chars, i, pat))
}

/// Case-insensitive `find_from`, for HTML's end tags.
fn find_from_ci(chars: &[char], from: usize, pat: &str) -> Option<usize> {
    let pat: Vec<char> = pat.chars().map(|c| c.to_ascii_lowercase()).collect();
    (from..chars.len()).find(|&i| {
        pat.iter()
            .enumerate()
            .all(|(k, p)| chars.get(i + k).map(|c| c.to_ascii_lowercase()) == Some(*p))
    })
}

/// Checks that tags nest and close properly. HTML mode is lenient the way
/// HTML is: void elements and omittable end tags (`<p>`, `<li>`...) are
/// fine, names compare case-insensitively, and `<script>`/`<style>`
/// contents are raw text. XML mode is strict.
pub fn check(source: &str, html: bool) -> Vec<MarkupError> {
    let chars: Vec<char> = source.chars().collect();

    let mut positions = Vec::with_capacity(chars.len() + 1);
    let (mut line, mut col) = (0usize, 0usize);
    for &c in &chars {
        positions.push((line, col));
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    positions.push((line, col));

    let error_at = |i: usize, message: String| MarkupError {
        line: positions[i].0,
        col: positions[i].1,
        message,
    };
    let names_eq = |a: &str, b: &str| {
        if html {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };

    struct Open {
        name: String,
        at: usize,
    }
    let mut stack: Vec<Open> = Vec::new();
    let mut errors: Vec<MarkupError> = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }

        if matches_at(&chars, i, "<!--") {
            match find_from(&chars, i + 4, "-->") {
                Some(end) => i = end + 3,
                None => {
                    errors.push(error_at(i, "unterminated comment".into()));
                    break;
                }
            }
            continue;
        }
        if !html && matches_at(&chars, i, "<![CDATA[") {
            match find_from(&chars, i + 9, "]]>") {
                Some(end) => i = end + 3,
                None => {
                    errors.push(error_at(i, "unterminated CDATA section".into()));
                    break;
                }
            }
            continue;
        }
        if matches_at(&chars, i, "<?") {
            match find_from(&chars, i + 2, "?>") {
                Some(end) => i = end + 2,
                None => {
                    errors.push(error_at(i, "unterminated processing instruction".into()));
                    break;
                }
            }
            continue;
        }
        if matches_at(&chars, i, "<!") {
            match find_from(&chars, i + 2, ">") {
                Some(end) => i = end + 1,
                None => {
                    errors.push(error_at(i, "unterminated declaration".into()));
                    break;
                }
            }
            continue;
        }

        if matches_at(&chars, i, "</") {
            let mut j = i + 2;
            while j < chars.len() && is_name_char(chars[j]) {
                j += 1;
            }
            let name: String = chars[i + 2..j].iter().collect();
            let Some(gt) = find_from(&chars, j, ">") else {
                errors.push(error_at(i, "unterminated closing tag".into()));
                break;
            };
            if name.is_empty() {
                errors.push(error_at(i, "empty closing tag".into()));
            } else if let Some(depth) = stack.iter().rposition(|o| names_eq(&o.name, &name)) {
                // Whatever was opened after the match was never closed.
                for o in stack.drain(depth + 1..).rev() {
                    if !(html && in_list(HTML_OPTIONAL_END, &o.name)) {
                        errors.push(error_at(o.at, format!("unclosed <{}>", o.name)));
                    }
                }
                stack.pop();
            } else {
                errors.push(error_at(i, format!("unexpected closing tag </{name}>")));
            }
            i = gt + 1;
            continue;
        }

        if chars.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic()) {
            let mut j = i + 1;
            while j < chars.len() && is_name_char(chars[j]) {
                j += 1;
            }
            let name: String = chars[i + 1..j].iter().collect();

            // Find the `>` that ends the tag, skipping any inside quoted
            // attribute values.
            let mut quote: Option<char> = None;
            let mut gt = None;
            for (k, &c) in chars.iter().enumerate().skip(j) {
                match quote {
                    Some(q) if c == q => quote = None,
                    Some(_) => {}
                    None if c == '"' || c == '\'' => quote = Some(c),
                    None if c == '>' => {
                        gt = Some(k);
                        break;
                    }
                    None => {}
                }
            }
            let Some(gt) = gt else {
                errors.push(error_at(i, format!("unterminated <{name}> tag")));
                break;
            };

            let self_closing = chars[j..gt]
                .iter()
                .rev()
                .find(|c| !c.is_whitespace())
                .is_some_and(|&c| c == '/');
            let void = html && in_list(HTML_VOID, &name);

            if !self_closing && !void {
                stack.push(Open {
                    name: name.clone(),
                    at: i,
                });
                if html && in_list(HTML_RAW_TEXT, &name) {
                    // Skip the raw-text body; the loop then sees the end tag.
                    let close = format!("</{name}");
                    i = find_from_ci(&chars, gt + 1, &close).unwrap_or(gt + 1);
                    continue;
                }
            }
            i = gt + 1;
            continue;
        }

        i += 1; // a lone `<` is just text
    }

    for o in stack.into_iter().rev() {
        if !(html && in_list(HTML_OPTIONAL_END, &o.name)) {
            errors.push(error_at(o.at, format!("unclosed <{}>", o.name)));
        }
    }

    errors.sort_by_key(|e| (e.line, e.col));
    errors.truncate(50);
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(source: &str, html: bool) -> Vec<String> {
        check(source, html).into_iter().map(|e| e.message).collect()
    }

    // ---- auto-closing ----

    #[test]
    fn typing_gt_after_an_opening_tag_closes_it() {
        assert_eq!(closing_tag_for("<div", true).as_deref(), Some("</div>"));
        assert_eq!(
            closing_tag_for("  <a href=\"x\" class='y'", true).as_deref(),
            Some("</a>")
        );
        assert_eq!(
            closing_tag_for("<ns:item-1", false).as_deref(),
            Some("</ns:item-1>")
        );
        // Text before the tag on the same line doesn't matter.
        assert_eq!(
            closing_tag_for("<p>hello <b", true).as_deref(),
            Some("</b>")
        );
    }

    #[test]
    fn no_auto_close_for_things_that_are_not_opening_tags() {
        for before in [
            "</div",     // a closing tag
            "<!-- note", // comment
            "<!DOCTYPE html",
            "<?xml version=\"1.0\"?",
            "<br/", // self-closing
            "<img", // void element (HTML)
            "<IMG src=\"a\"",
            "1 < 2",        // not a tag
            "<div>",        // already closed
            "<a title=\"x", // `>` would be inside an attribute value
            "plain text",
        ] {
            assert_eq!(closing_tag_for(before, true), None, "{before:?}");
        }
    }

    #[test]
    fn void_elements_only_exempt_in_html_not_xml() {
        assert_eq!(closing_tag_for("<br", true), None);
        assert_eq!(closing_tag_for("<br", false).as_deref(), Some("</br>"));
    }

    #[test]
    fn enter_between_tags_detects_only_open_then_close() {
        assert!(between_tags("<div>", "</div>", true));
        assert!(between_tags("  <ul class=\"a\">", "</ul>", true));
        assert!(!between_tags("<div>text", "</div>", true));
        assert!(!between_tags("<div>", "text</div>", true));
        assert!(!between_tags("</p>", "</div>", true)); // closing then closing
        assert!(!between_tags("<br>", "</p>", true)); // void element
        assert!(!between_tags("<br/>", "</p>", true)); // self-closing
        assert!(!between_tags("<!-- c -->", "</p>", true));
    }

    // ---- checking ----

    #[test]
    fn well_formed_documents_have_no_errors() {
        let html = "<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"><title>t</title>\n\
                    <style>a > b { color: red }</style></head>\n\
                    <body><!-- <not a tag> -->\n<p>one<p>two\n<ul><li>a<li>b</ul>\n\
                    <img src=\"a.png\"><br><input value=\"a>b\">\n\
                    <script>if (a < b && c > d) { x = \"</div>\"; }</script></body></html>";
        assert!(msgs(html, true).is_empty(), "{:?}", msgs(html, true));

        let xml =
            "<?xml version=\"1.0\"?>\n<root a=\"1\"><item/><item><![CDATA[ <x> ]]></item></root>";
        assert!(msgs(xml, false).is_empty(), "{:?}", msgs(xml, false));
    }

    #[test]
    fn reports_unclosed_and_unexpected_tags_at_their_position() {
        let errs = check("<div>\n  <span>text\n</div>\n</p>", true);
        let got: Vec<_> = errs
            .iter()
            .map(|e| (e.line, e.col, e.message.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, 2, "unclosed <span>"),
                (3, 0, "unexpected closing tag </p>"),
            ]
        );
    }

    #[test]
    fn reports_unclosed_tag_at_end_of_file() {
        assert_eq!(msgs("<div>\n<b>x</b>", true), vec!["unclosed <div>"]);
    }

    #[test]
    fn html_names_are_case_insensitive_but_xml_names_are_not() {
        assert!(msgs("<DIV></div>", true).is_empty());
        assert_eq!(
            msgs("<DIV></div>", false),
            vec!["unclosed <DIV>", "unexpected closing tag </div>"]
        );
    }

    #[test]
    fn reports_unterminated_constructs() {
        assert_eq!(msgs("<!-- never ends", true), vec!["unterminated comment"]);
        assert_eq!(msgs("<a href=\"x", true), vec!["unterminated <a> tag"]);
    }
}
