use super::*;
use openidea_plugin::diagnostics::DiagnosticsProvider;
use openidea_plugin::hover::HoverProvider;
use openidea_plugin::mock::MockHost;
use crate::tests::{activated, doc};

const LANGUAGES: [(&str, &str); 4] = [
    ("twig", "/project/templates/fuzz.html.twig"),
    ("php", "/project/src/Controller/Fuzz.php"),
    ("yaml", "/project/config/services.yaml"),
    ("xml", "/project/config/services.xml"),
];

const SEEDS: [&str; 5] = ["{{ x }}{#}", "café", "<?php\nuse $😀", "é::class", "<!-- Paramètres --><a/>"];

fn position_at(text: &str, offset: usize) -> Position {
    let before = &text[..offset];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    Position::new(line, before[line_start..].encode_utf16().count() as u32)
}

fn survive(seeds: &[&str]) {
    let host = MockHost::install();
    let mut plugin = activated(&host);
    for seed in seeds {
        for (language, path) in LANGUAGES {
            let document = doc(path, language, seed);
            plugin.diagnostics(&document);
            for offset in (0..=seed.len()).filter(|at| seed.is_char_boundary(*at)) {
                let at = position_at(seed, offset);
                plugin.completion(&document, at, None);
                plugin.hover(&document, at);
                plugin.code_actions(&document, Range { start: at, end: at });
            }
        }
    }
}

#[test]
fn twig_comment_opener_without_closer() {
    survive(&["{{ x }}{#}", "{% for a in b %}{%}", "{#} {# @var x Foo #}"]);
}

#[test]
fn non_ascii_before_class_constant() {
    survive(&["type:}éMailer::class(", "#[Ünï(x)] #[éRoute(]"]);
}

#[test]
fn use_with_emoji_line_end() {
    survive(&["<?php\nuse $😀"]);
}

#[test]
fn xml_comment_non_ascii_keeps_offsets() {
    let text = "<!-- Paramètres -->\n<service id=\"a\"><tag name=\"kernel.event_listener\"/></service>";
    let token = text.find("kernel.event_listener").unwrap();
    let services = crate::wiring::xml_services(text);
    assert_eq!(services[0].tags[0].name_range, token..token + "kernel.event_listener".len());
    survive(&[text]);
}

#[test]
fn yaml_reference_at_zero_no_underflow() {
    for language in ["php", "twig", "yaml"] {
        let reference = crate::refs::Reference { kind: crate::refs::Kind::Translation, start: 0, end: 3 };
        assert_eq!(crate::refs::translation_domain(language, "app.title", &reference), None);
    }
    survive(&["app.title", "'app.title'", "app.title: x"]);
}

#[test]
fn unbalanced_component_text_no_panic() {
    survive(&["<twig:Alert", "<twig:Alert <twig:B>", "{% component A %}<twig:", "</twig:A><twig:A foo=\"{{ ", "<twig:A \"x>"]);
}

#[test]
fn symfony_providers_survive_offsets() {
    survive(&SEEDS);
}

#[test]
fn index_build_survives_unclosed_twig_tags() {
    for text in ["{%}", "{#}", "{{ x }}{%}{% block a %}", "{% props %}{%}", "{# @var x Foo #}{#}"] {
        crate::components::template_blocks(text);
        crate::components::template_props(text);
        let host = MockHost::install();
        let mut plugin = activated(&host);
        host.add_file("templates/broken.html.twig", text);
        host.add_file("templates/components/Broken.html.twig", text);
        plugin.run_command("reindex", &host.invocation(), None).unwrap();
    }
}
