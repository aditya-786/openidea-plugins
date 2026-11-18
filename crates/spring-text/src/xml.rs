//! An XML element scanner: start tags with their attributes, nesting and
//! extent. Comments, CDATA, processing instructions and the doctype are
//! skipped; entities are left as written. Offsets are UTF-8 bytes.
//! Plus the well-formedness gates: [`analysis_valid`] before reading a file,
//! [`generation_valid`] before writing into one.

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::NsReader;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attr {
    /// As written (`batch:next`).
    pub name: String,
    pub at: usize,
    pub value: String,
    /// The bytes between the quotes.
    pub value_at: usize,
    pub value_end: usize,
}

impl Attr {
    /// The name without its namespace prefix.
    pub fn local(&self) -> &str {
        local(&self.name)
    }

    /// Whether the byte offset `at` is inside the value's quotes (or right after its last character).
    pub fn holds(&self, at: usize) -> bool {
        self.value_at <= at && at <= self.value_end
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    /// As written (`int:channel`).
    pub name: String,
    /// Byte offset of the name.
    pub at: usize,
    /// The `<` of the start tag.
    pub start: usize,
    /// After the start tag's `>`.
    pub tag_end: usize,
    /// After the end tag (or the start tag when it closes itself, or the text if unclosed).
    pub end: usize,
    pub attrs: Vec<Attr>,
    /// Index of the parent element.
    pub parent: Option<usize>,
}

impl Element {
    pub fn local(&self) -> &str {
        local(&self.name)
    }

    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name || a.local() == name)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.attr(name).map(|a| a.value.as_str())
    }
}

pub fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Every element of `text`, in document order.
pub fn elements(text: &str) -> Vec<Element> {
    let b = text.as_bytes();
    let find = |from: usize, pat: &str| text[from.min(text.len())..].find(pat).map_or(text.len(), |p| from + p + pat.len());
    let name_char = |c: u8| !c.is_ascii_whitespace() && !matches!(c, b'>' | b'/' | b'=' | b'<' | b'"' | b'\'');
    let mut out: Vec<Element> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    let mut i = 0;
    while let Some(lt) = text[i..].find('<').map(|p| i + p) {
        let rest = &text[lt..];
        if rest.starts_with("<!--") {
            i = find(lt + 4, "-->");
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            i = find(lt + 9, "]]>");
            continue;
        }
        if rest.starts_with("<?") {
            i = find(lt + 2, "?>");
            continue;
        }
        if rest.starts_with("<!") {
            i = find(lt + 2, ">");
            continue;
        }
        if rest.starts_with("</") {
            let mut j = lt + 2;
            while j < b.len() && name_char(b[j]) {
                j += 1;
            }
            let name = &text[lt + 2..j];
            let end = find(j, ">");
            // Close up to the matching element; a stray end tag closes nothing.
            if let Some(pos) = open.iter().rposition(|&e| out[e].name == name) {
                for e in open.drain(pos..) {
                    out[e].end = end;
                }
            }
            i = end;
            continue;
        }
        let mut j = lt + 1;
        while j < b.len() && name_char(b[j]) {
            j += 1;
        }
        if j == lt + 1 {
            i = lt + 1;
            continue;
        }
        let mut el = Element {
            name: text[lt + 1..j].to_owned(),
            at: lt + 1,
            start: lt,
            tag_end: text.len(),
            end: text.len(),
            attrs: Vec::new(),
            parent: open.last().copied(),
        };
        let mut self_closing = false;
        while j < b.len() {
            let c = b[j];
            if c.is_ascii_whitespace() {
                j += 1;
            } else if c == b'>' {
                j += 1;
                break;
            } else if c == b'/' && b.get(j + 1) == Some(&b'>') {
                self_closing = true;
                j += 2;
                break;
            } else if c == b'<' {
                break; // an unfinished tag
            } else if name_char(c) {
                let at = j;
                while j < b.len() && name_char(b[j]) {
                    j += 1;
                }
                let name = text[at..j].to_owned();
                let mut k = j;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                if b.get(k) != Some(&b'=') {
                    el.attrs.push(Attr { name, at, value: String::new(), value_at: j, value_end: j });
                    continue;
                }
                k += 1;
                while k < b.len() && b[k].is_ascii_whitespace() {
                    k += 1;
                }
                match b.get(k) {
                    Some(&q) if q == b'"' || q == b'\'' => {
                        let value_at = k + 1;
                        let value_end = text[value_at..].find(q as char).map_or(text.len(), |p| value_at + p);
                        el.attrs.push(Attr {
                            name,
                            at,
                            value: text[value_at..value_end].to_owned(),
                            value_at,
                            value_end,
                        });
                        j = (value_end + 1).min(text.len());
                    }
                    _ => j = k,
                }
            } else {
                j += 1;
            }
        }
        el.tag_end = j;
        if self_closing {
            el.end = j;
        }
        out.push(el);
        if !self_closing {
            open.push(out.len() - 1);
        }
        i = j.max(lt + 1);
    }
    out
}

/// The element whose start tag holds the byte offset `at`.
pub fn element_at(elements: &[Element], at: usize) -> Option<usize> {
    elements.iter().rposition(|e| e.start < at && at < e.tag_end)
}

/// The root element's local name (`beans`, `flow`, `schema`…).
pub fn root(elements: &[Element]) -> Option<&str> {
    elements.first().map(|e| e.local())
}

/// The namespace URI of `elements[i]`: its prefix (or the default namespace)
/// looked up in the `xmlns` declarations of the element and its ancestors.
pub fn namespace(elements: &[Element], i: usize) -> Option<&str> {
    let decl = elements[i].name.split_once(':').map_or_else(|| "xmlns".to_owned(), |(p, _)| format!("xmlns:{p}"));
    let mut at = Some(i);
    while let Some(e) = at {
        if let Some(a) = elements[e].attrs.iter().find(|a| a.name == decl) {
            return Some(&a.value);
        }
        at = elements[e].parent;
    }
    None
}

/// Bean names declared in a Spring XML file: `<bean id>` and its `name`
/// aliases, `<alias alias>`, and the `id` of any other top-level element
/// (custom namespaces such as `<int:channel id>` register beans too).
pub fn bean_ids(elements: &[Element]) -> Vec<&Attr> {
    let mut out = Vec::new();
    for (i, e) in elements.iter().enumerate() {
        let top = e.parent.is_none_or(|p| elements[p].local() == "beans") || e.local() == "bean";
        if !top || i == 0 && e.local() == "beans" {
            continue;
        }
        match e.local() {
            "alias" => out.extend(e.attr("alias")),
            _ => {
                out.extend(e.attr("id"));
                if e.local() == "bean" {
                    out.extend(e.attr("name"));
                }
            }
        }
    }
    out
}

/// Whether the namespace bindings of `element` are legal: a qualified name
/// with at most one prefix, every prefix bound, and no reserved namespace
/// rebound or aliased.
pub fn namespace_valid<R>(
    reader: &quick_xml::reader::NsReader<R>,
    element: &quick_xml::events::BytesStart<'_>,
) -> bool {
    use quick_xml::name::ResolveResult;
    const XML: &str = "http://www.w3.org/XML/1998/namespace";
    const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
    let qualified = |name: &str| {
        let parts: Vec<_> = name.split(':').collect();
        parts.len() <= 2 && parts.iter().all(|p| !p.is_empty())
    };
    if !qualified(element.name().as_ref())
        || element.name().as_ref().starts_with("xmlns:")
        || matches!(
            reader.resolver().resolve_element(element.name()).0,
            ResolveResult::Unknown(_)
        )
    {
        return false;
    }
    for attribute in element.attributes() {
        let Ok(attribute) = attribute else {
            return false;
        };
        let key = attribute.key.as_ref();
        if !qualified(key) {
            return false;
        }
        if key == "xmlns" || key.starts_with("xmlns:") {
            let Ok(value) = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0) else {
                return false;
            };
            if key == "xmlns:xmlns"
                || key.starts_with("xmlns:") && value.is_empty()
                || key == "xmlns:xml" && value != XML
                || key != "xmlns:xml" && matches!(value.as_ref(), XML | XMLNS)
            {
                return false;
            }
        } else if matches!(
            reader.resolver().resolve_attribute(attribute.key).0,
            ResolveResult::Unknown(_)
        ) {
            return false;
        }
    }
    true
}

/// Whether `text` is a well-formed, namespace-valid XML document worth
/// analysing: one root, nothing but whitespace, comments and processing
/// instructions around it, a DOCTYPE and predefined or numeric entities
/// allowed. Bounded by size (512 KiB) and depth (128), never by time, so the
/// answer doesn't depend on machine speed.
pub fn analysis_valid(text: &str) -> bool {
    text.len() <= 512 * 1024 && walk(text, false)
}

/// Whether `text` is safe to generate into: [`analysis_valid`]'s envelope
/// without a DOCTYPE or any entity reference, with well-formed comments, and
/// at most 8192 parse events.
pub fn generation_valid(text: &str) -> bool {
    walk(text, true)
}

/// The shared well-formedness walk; `strict` is [`generation_valid`]'s rules.
fn walk(text: &str, strict: bool) -> bool {
    let mut reader = NsReader::from_str(text);
    reader.config_mut().check_comments = strict;
    let (mut depth, mut roots, mut declaration) = (0usize, 0usize, false);
    for events in 0usize.. {
        if strict && events == 8192 {
            return false;
        }
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if !open(&reader, &e, depth, &mut roots) {
                    return false;
                }
                depth += 1;
                if depth > 128 {
                    return false;
                }
            }
            Ok(Event::Empty(e)) => {
                if !open(&reader, &e, depth, &mut roots) {
                    return false;
                }
            }
            Ok(Event::End(_)) => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
            }
            Ok(Event::Text(t)) if depth == 0 && !t.as_ref().trim_ascii().is_empty() => return false,
            Ok(Event::CData(_)) if depth == 0 => return false,
            Ok(Event::GeneralRef(r)) => {
                let known = if r.is_char_ref() {
                    matches!(r.resolve_char_ref(), Ok(Some(_)))
                } else {
                    matches!(&*r, "amp" | "lt" | "gt" | "quot" | "apos")
                };
                if strict || depth == 0 || !known {
                    return false;
                }
            }
            Ok(Event::Decl(_)) => {
                if depth != 0 || roots != 0 || declaration {
                    return false;
                }
                declaration = true;
            }
            Ok(Event::DocType(_)) if strict || depth != 0 || roots != 0 => return false,
            Ok(Event::Eof) => return depth == 0 && roots == 1,
            Err(_) => return false,
            _ => {}
        }
    }
    false
}

/// Counts a root and checks the namespaces of an opening tag at `depth`.
fn open<R>(reader: &NsReader<R>, element: &BytesStart<'_>, depth: usize, roots: &mut usize) -> bool {
    if depth == 0 {
        *roots += 1;
    }
    *roots == 1 && namespace_valid(reader, element)
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE beans>
<beans xmlns="http://www.springframework.org/schema/beans" xmlns:int="x">
  <!-- <bean id="commented"/> -->
  <bean id="a" name="b" class="com.acme.A">
    <property name="next" ref="c"/>
    <constructor-arg><ref bean='d'/></constructor-arg>
    <![CDATA[ <bean id="cdata"/> ]]>
  </bean>
  <int:channel id="orders"/>
  <alias name="a" alias="z"/>
  <broken attr=
</beans>"#;

    #[test]
    fn scans_elements() {
        let els = elements(XML);
        let names: Vec<_> = els.iter().map(|e| (e.name.as_str(), e.parent)).collect();
        assert_eq!(names, [
            ("beans", None),
            ("bean", Some(0)),
            ("property", Some(1)),
            ("constructor-arg", Some(1)),
            ("ref", Some(3)),
            ("int:channel", Some(0)),
            ("alias", Some(0)),
            ("broken", Some(0)),
        ]);
        let r = els[2].attr("ref").unwrap();
        assert_eq!(&XML[r.value_at..r.value_end], "c");
        assert_eq!(els[4].value("bean"), Some("d"));
        assert_eq!(els[5].local(), "channel");
        assert!(XML[els[1].start..els[1].end].ends_with("</bean>"));
        let ids: Vec<_> = bean_ids(&els).into_iter().map(|a| a.value.as_str()).collect();
        assert_eq!(ids, ["a", "b", "orders", "z"]);
        assert_eq!(root(&els), Some("beans"));
        assert_eq!(namespace(&els, 2), Some("http://www.springframework.org/schema/beans"));
        assert_eq!(namespace(&els, 5), Some("x"));
    }

    #[test]
    fn finds_the_tag_under_the_caret() {
        let els = elements(XML);
        let at = XML.find("ref=\"c").unwrap() + 5;
        assert_eq!(element_at(&els, at), Some(2));
    }

    #[test]
    fn analysis_valid_accepts_entities_and_doctype() {
        assert!(analysis_valid("<beans><bean id='a' value='x &amp; y'>a &amp; b &lt; c &#38; d &#x26;</bean></beans>"));
        assert!(analysis_valid("<?xml version='1.0'?>\n<!DOCTYPE beans>\n<beans xmlns='urn:b'/>"));
        assert!(analysis_valid(XML.replace("  <broken attr=\n", "").as_str()));
        for bad in [
            "<beans><bean></beans>",
            "<beans><bean>",
            "<beans>&nbsp;</beans>",
            "<beans>&#0;</beans>",
            "<a/><b/>",
            "text<a/>",
            "<a><unbound:b/></a>",
            "<a xmlns:q=''/>",
        ] {
            assert!(!analysis_valid(bad), "{bad}");
        }
        let deep = format!("{}{}", "<r>".repeat(129), "</r>".repeat(129));
        assert!(!analysis_valid(&deep));
        assert!(!analysis_valid(&format!("<a>{}</a>", " ".repeat(512 * 1024))));
        assert!(analysis_valid("<beans><!-- a -- b --></beans>"));
    }

    #[test]
    fn analysis_valid_large_file() {
        let beans: String = (0..2000).map(|i| format!("<bean id='b{i}'>\n  <property name='x'/>\n</bean>\n")).collect();
        assert!(analysis_valid(&format!("<beans>\n{beans}</beans>")));
    }

    #[test]
    fn generation_valid_keeps_the_strict_rules() {
        assert!(generation_valid("<?xml version='1.0'?><beans xmlns='urn:b'><bean id='a'/></beans>"));
        for bad in [
            "<!DOCTYPE beans><beans/>",
            "<beans>&amp;</beans>",
            "<beans><!-- a -- b --></beans>",
            "<beans><bean></beans>",
            "<beans/><beans/>",
            "<a><b:c/></a>",
            "<?xml version='1.0'?><?xml version='1.0'?><beans/>",
        ] {
            assert!(!generation_valid(bad), "{bad}");
        }
        let many: String = (0..5000).map(|i| format!("<b id='b{i}'/>\n")).collect();
        assert!(!generation_valid(&format!("<beans>{many}</beans>")));
    }

    #[test]
    fn survives_broken_input() {
        for src in ["<", "<a", "<a b='", "</", "<!--", "<a></b></a>", "<é attr=\"é\"/>"] {
            elements(src);
        }
        for end in 0..=XML.len() {
            elements(&XML[..end]);
        }
    }
}
