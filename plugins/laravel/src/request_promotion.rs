//! Bounded authored validation-rule transfer; no general PHP resolution.
use crate::{Main, request_creation, view_creation};
use openidea_plugin::{
    Error, Range, TextEdit,
    file_actions::{FileAction, FileOperation},
    host,
    providers::Document,
    text::{offset, position},
};
use std::collections::{BTreeMap, BTreeSet};
#[path = "request_promotion_ordinary.rs"]
mod ordinary;
#[derive(Default)]
pub(crate) struct State {
    next: u64,
    pending: BTreeMap<String, Pending>,
}
#[derive(Clone)]
struct Pending {
    doc: Document,
    root: String,
    shape: Shape,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shape {
    controller: String,
    prefix: String,
    parameter: std::ops::Range<usize>,
    call: std::ops::Range<usize>,
    rules: String,
    variable: String,
    strict: bool,
}
#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}
struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    at: usize,
}
impl<'a> Parser<'a> {
    fn token(&mut self) -> Option<Token<'a>> {
        let t = *self.tokens.get(self.at)?;
        self.at += 1;
        Some(t)
    }
    fn expect(&mut self, text: &str) -> Option<Token<'a>> {
        let t = self.token()?;
        (t.text == text).then_some(t)
    }
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(|t| t.text)
    }
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || i > 0 && b.is_ascii_digit())
}
fn ordinary_variable(name: &str) -> bool {
    identifier(name)
        && ![
            "this", "GLOBALS", "_SERVER", "_GET", "_POST", "_FILES", "_COOKIE", "_SESSION",
            "_REQUEST", "_ENV",
        ]
        .contains(&name)
}
fn tokens(text: &str) -> Option<Vec<Token<'_>>> {
    if text.len() > 16 << 10 || !text.starts_with("<?php") {
        return None;
    }
    let b = text.as_bytes();
    let mut at = 5;
    let mut out = Vec::new();
    while at < b.len() {
        if b[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        let start = at;
        if b[at] == b'\'' {
            at += 1;
            let content = at;
            while at < b.len() && b[at] != b'\'' {
                if b[at] == b'\\' || b[at] < 32 {
                    return None;
                }
                at += 1;
            }
            if at == b.len() || at - content > 512 {
                return None;
            }
            at += 1;
        } else if b[at].is_ascii_alphabetic() || matches!(b[at], b'_' | b'\\' | b'$') {
            at += 1;
            while at < b.len() && (b[at].is_ascii_alphanumeric() || matches!(b[at], b'_' | b'\\')) {
                at += 1;
            }
        } else if b[at..].starts_with(b"->") || b[at..].starts_with(b"=>") {
            at += 2;
        } else if b"{}[]():,;=".contains(&b[at]) {
            at += 1;
        } else {
            return None;
        }
        out.push(Token {
            text: text.get(start..at)?,
            start,
            end: at,
        });
        if out.len() > 2048 {
            return None;
        }
    }
    Some(out)
}
fn string(t: Token<'_>) -> Option<&str> {
    t.text.strip_prefix('\'')?.strip_suffix('\'')
}
fn rule(p: &mut Parser<'_>) -> Option<()> {
    let s = string(p.token()?)?;
    (!s.is_empty()).then_some(())
}
fn literal_shape(doc: &Document) -> Option<Shape> {
    if doc.language != "php" || !doc.path.ends_with(".php") || doc.path.ends_with(".blade.php") {
        return None;
    }
    let mut p = Parser {
        tokens: tokens(&doc.text)?,
        at: 0,
    };
    p.expect("namespace")?;
    let namespace = p.token()?.text;
    let prefix = namespace.strip_suffix("Http\\Controllers")?;
    // The same strict namespace and Composer proof as mapped scaffolding is
    // repeated at resolve; this lexical shape does not load PHP or autoloaders.
    if !request_creation::promotion_namespace(prefix) {
        return None;
    }
    p.expect(";")?;
    p.expect("class")?;
    let controller = p.token()?.text;
    if !identifier(controller) || !request_creation::promotion_namespace(&format!("{controller}\\"))
    {
        return None;
    }
    p.expect("{")?;
    p.expect("public")?;
    p.expect("function")?;
    let method = p.token()?.text;
    if !identifier(method)
        || method.starts_with("__")
        || !request_creation::promotion_namespace(&format!("{method}\\"))
    {
        return None;
    }
    p.expect("(")?;
    let parameter = p.expect("\\Illuminate\\Http\\Request")?;
    let variable = p.token()?.text.strip_prefix('$')?;
    if !ordinary_variable(variable) {
        return None;
    }
    p.expect(")")?;
    p.expect(":")?;
    p.expect("array")?;
    p.expect("{")?;
    let assigned = if p.peek() == Some("return") {
        p.token()?;
        None
    } else {
        let local = p.token()?.text.strip_prefix('$')?;
        if !ordinary_variable(local) || local == variable {
            return None;
        }
        p.expect("=")?;
        Some(local)
    };
    let receiver = p.token()?;
    if receiver.text != format!("${variable}") {
        return None;
    }
    p.expect("->")?;
    p.expect("validate")?;
    p.expect("(")?;
    let open = p.expect("[")?;
    let mut keys = BTreeSet::new();
    loop {
        let key = string(p.token()?)?;
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._*-".contains(&b))
            || !keys.insert(key.to_owned())
            || keys.len() > 64
        {
            return None;
        }
        p.expect("=>")?;
        if p.peek() == Some("[") {
            p.token()?;
            let mut count = 0;
            loop {
                rule(&mut p)?;
                count += 1;
                if count > 32 {
                    return None;
                }
                if p.peek() != Some(",") {
                    break;
                }
                p.token()?;
                if p.peek() == Some("]") {
                    break;
                }
            }
            p.expect("]")?;
        } else {
            rule(&mut p)?;
        }
        if p.peek() != Some(",") {
            break;
        }
        p.token()?;
        if p.peek() == Some("]") {
            break;
        }
    }
    let close = p.expect("]")?;
    if close.end - open.start > 4096 {
        return None;
    }
    p.expect(")")?;
    let call_end = p.tokens[p.at - 1].end;
    p.expect(";")?;
    if let Some(local) = assigned {
        p.expect("return")?;
        if p.token()?.text != format!("${local}") {
            return None;
        }
        p.expect(";")?;
    }
    p.expect("}")?;
    p.expect("}")?;
    if p.at != p.tokens.len() {
        return None;
    }
    Some(Shape {
        controller: controller.into(),
        prefix: prefix.into(),
        parameter: parameter.start..parameter.end,
        call: receiver.start..call_end,
        rules: doc.text[open.start..close.end].into(),
        variable: variable.into(),
        strict: false,
    })
}
fn shapes(doc: &Document) -> Vec<Shape> {
    let mut result = ordinary::shapes(doc).unwrap_or_default();
    if let Some(shape) = literal_shape(doc) {
        if !result.contains(&shape) {
            result.push(shape);
        }
    }
    result
}
fn admitted(main: &Main, p: &Pending, doc: &Document) -> Result<(), Error> {
    if doc.path != p.doc.path
        || doc.language != p.doc.language
        || doc.version != p.doc.version
        || doc.text != p.doc.text
        || !shapes(doc).contains(&p.shape)
    {
        return Err(Error::Stale);
    }
    let (project, root) = view_creation::owner(doc, Some(&p.root))?;
    if main.indexes.get(&root).is_none_or(|i| i.partial)
        || project.read_file(&doc.path)? != doc.text
    {
        return Err(Error::Stale);
    }
    Ok(())
}
pub(crate) fn actions(main: &mut Main, doc: &Document, selection: Range) -> Vec<FileAction> {
    let Ok((_, root)) = view_creation::owner(doc, None) else {
        return vec![];
    };
    if main.indexes.get(&root).is_none_or(|i| i.partial) {
        return vec![];
    }
    let (Some(start), Some(end)) = (
        offset(&doc.text, selection.start),
        offset(&doc.text, selection.end),
    ) else {
        return vec![];
    };
    let Some(shape) = shapes(doc)
        .into_iter()
        .find(|shape| start >= shape.parameter.start && end >= start && end <= shape.parameter.end)
    else {
        return vec![];
    };
    if main.request_promotions.pending.len() >= 8 {
        main.request_promotions.pending.clear();
    }
    let Some(next) = main.request_promotions.next.checked_add(1) else {
        return vec![];
    };
    main.request_promotions.next = next;
    let key = format!("request-promote:{next}:{}", doc.version);
    main.request_promotions.pending.insert(
        key.clone(),
        Pending {
            doc: doc.clone(),
            root,
            shape,
        },
    );
    vec![FileAction {
        title: crate::ui_text::text("requestPromotion.moveAction","Move validation rules to a Form Request…").into(),
        kind: Some("refactor.extract".into()),
        data: Some(key),
        ..Default::default()
    }]
}
pub(crate) fn resolve(
    main: &mut Main,
    doc: &Document,
    action: FileAction,
) -> Result<FileAction, Error> {
    let key = action.data.as_deref().ok_or(Error::Stale)?;
    let p = main
        .request_promotions
        .pending
        .get(key)
        .cloned()
        .ok_or(Error::Stale)?;
    admitted(main, &p, doc)?;
    let name = host::input(crate::ui_text::text("requestPromotion.input","New FormRequest class name"), None)?.ok_or(Error::Cancelled)?;
    admitted(main, &p, doc)?;
    let (project, _) = view_creation::owner(doc, Some(&p.root))?;
    let (target, stamp, source) =
        request_creation::validate_promotion(&project, &p.root, doc, &name, &p.shape.prefix)?;
    if doc.path
        != format!(
            "{}/{source}/Http/Controllers/{}.php",
            p.root, p.shape.controller
        )
    {
        return Err(Error::Stale);
    }
    let eol = if doc.text.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let head=format!("<?php\n\n{}namespace {}Http\\Requests;\n\nuse Illuminate\\Foundation\\Http\\FormRequest;\n\nclass {name} extends FormRequest\n{{\n    public function authorize(): bool\n    {{\n        return true;\n    }}\n\n    public function rules(): array\n    {{\n        return ",(if p.shape.strict {"declare(strict_types=1);\n\n"}else{""}),p.shape.prefix).replace('\n',eol);
    let text = head + &p.shape.rules + &";\n    }\n}\n".replace('\n', eol);
    let mut replacement = doc.text.clone();
    replacement.replace_range(
        p.shape.call.clone(),
        &format!("${}->validated()", p.shape.variable),
    );
    replacement.replace_range(
        p.shape.parameter.clone(),
        &format!("\\{}Http\\Requests\\{name}", p.shape.prefix),
    );
    let preview = crate::ui_text::format("requestPromotion.review","Project: {root}\nSource: {path}\nDocument version: {version}\nVerified Composer mapping: {namespace} → {source}/\n\nCreate (no overwrite): {target}\nExact new request contents:\n{text}\nExact complete replacement controller:\n{replacement}\n\nAuthored literal rules and supported self-contained class-rule expressions are transferred. Moved class references are fully qualified; external variables, closures and controller self/parent scope are withheld. The selected request parameter becomes the explicit absolute generated type. Other parameters, comments, statements and local assignment/return bindings stay in place. Only a first-statement validation call is promoted; other controller methods stay unchanged. validated() preserves the original validation call's array result; safe() would instead return ValidatedInput and is not substituted. authorize() returns true, adding no request-specific authorization gate; review route/controller authorization separately. FormRequest validation runs before the controller method, so preparation/error timing may differ. Installed framework/PHP/autoloader state and custom policies are not resolved. No PHP/Artisan/process runs.\n\nApply one undoable Create + controller Edit atomically?",&[("root",&p.root),("path",&doc.path),("version",&doc.version.to_string()),("namespace",&p.shape.prefix),("source",&source),("target",&target),("text",&text),("replacement",&replacement)]);
    if !ui_kit::prompts::confirm_exact(crate::ui_text::text("requestPromotion.moveConfirm","Move validation rules to a Form Request?"), &preview)? {
        return Err(Error::Cancelled);
    }
    admitted(main, &p, doc)?;
    let (current, current_stamp, current_source) =
        request_creation::validate_promotion(&project, &p.root, doc, &name, &p.shape.prefix)?;
    if current != target || current_stamp != stamp || current_source != source {
        return Err(Error::Stale);
    }
    main.request_promotions.pending.remove(key);
    Ok(FileAction {
        title: crate::ui_text::format("requestPromotion.applied","Promote validation to {name}",&[("name",&name)]),
        operations: vec![FileOperation::Create {
            path: target.clone(),
            text,
            overwrite: false,
        }],
        edits: vec![(
            doc.path.clone(),
            vec![TextEdit {
                range: Range::new(position(&doc.text, 0), position(&doc.text, doc.text.len())),
                text: replacement,
            }],
        )],
        open: Some(target),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use crate::Main;
    use openidea_plugin::{
        Range,
        file_actions::{FileActions, FileOperation},
        mock::MockHost,
        providers::Document,
        text::position,
    };
    const SOURCE: &str = "<?php\nnamespace App\\Http\\Controllers;\nclass PostController\n{\n    public function store(\\Illuminate\\Http\\Request $request): array\n    {\n        return $request->validate([\n            'title' => ['required', 'string'],\n            'body' => 'nullable|string',\n        ]);\n    }\n}\n";
    fn setup(source: &str) -> (MockHost, Main, Document, Range) {
        let h = MockHost::install().with_capabilities(&["project.read", "editor.read"]);
        for dir in ["app", "app/Http", "app/Http/Controllers"] {
            h.add_dir(dir);
        }
        h.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#,
        );
        let path = "/project/app/Http/Controllers/PostController.php";
        h.add_file(path, source);
        h.open_editor(path, "php", source, &[]);
        let doc = Document {
            path: path.into(),
            language: "php".into(),
            version: 19,
            text: source.into(),
        };
        let at = position(source, source.find("\\Illuminate\\Http\\Request").unwrap());
        let mut main = Main::default();
        main.indexes
            .insert("/project".into(), crate::index::Index::default());
        (h, main, doc, Range::new(at, at))
    }
    #[test]
    fn registered_literal_validation_promotes_rules_and_parameter_in_one_reviewed_action() {
        let (h, mut main, doc, range) = setup(SOURCE);
        let actions = main.file_actions(&doc, range);
        assert_eq!(
            actions.len(),
            1,
            "authored literal validation promotion action is missing"
        );
        h.answer_next_prompt(Some(&["StorePostRequest"]));
        h.answer_next_prompt(Some(&["yes"]));
        let ready = main.resolve_file_action(&doc, actions[0].clone()).unwrap();
        let [
            FileOperation::Create {
                path,
                text,
                overwrite,
            },
        ] = ready.operations.as_slice()
        else {
            panic!("one create")
        };
        assert_eq!(path, "/project/app/Http/Requests/StorePostRequest.php");
        assert!(!overwrite);
        assert!(
            text.contains("'title' => ['required', 'string']")
                && text.contains("'body' => 'nullable|string'")
        );
        assert!(
            text.contains("return true;")
                && text.contains("use Illuminate\\Foundation\\Http\\FormRequest;")
        );
        assert_eq!(ready.edits.len(), 1);
        let (edited_path, edits) = &ready.edits[0];
        assert_eq!(edited_path, &doc.path);
        assert_eq!(edits.len(), 1);
        assert!(
            edits[0]
                .text
                .contains("\\App\\Http\\Requests\\StorePostRequest $request")
        );
        assert!(edits[0].text.contains("return $request->validated();"));
        assert!(!edits[0].text.contains("->validate("));
        assert_eq!(
            h.invocation()
                .project()
                .unwrap()
                .read_file(&doc.path)
                .unwrap(),
            SOURCE
        );
        assert!(h.invocation().project().unwrap().read_file(path).is_err());
        assert!(h.process_runs().is_empty());
    }
    #[test]
    fn promotion_refuses_dynamic_rules_receivers_variables_and_other_php_scope() {
        for source in [
            SOURCE.replace("['required', 'string']", "[$rule]"),
            SOURCE.replace("['required', 'string']", "[Rule::requiredIf($user)]"),
            SOURCE.replace("'nullable|string'", "'nullable' . $suffix"),
            SOURCE.replace("'nullable|string'", "fn() => 'string'"),
            SOURCE.replace("'nullable|string'", "\"required $value\""),
            SOURCE.replace("'body' =>", "'title' =>"),
            SOURCE.replace("return $request", "return $other"),
            SOURCE.replace("return $request", "$request = request(); return $request"),
            SOURCE.replace(
                "class PostController",
                "class PostController extends Controller",
            ),
            SOURCE.replace("public function store", "public function __invoke"),
            SOURCE.replace("public function store", "public static function store"),
            SOURCE.replace("$request", "$_GET"),
            SOURCE.replace("class PostController", "class Wrong\\Qualified"),
            SOURCE.replace("$request->validate", "$request->validateWithBag"),
            SOURCE.replace("]);", "], ['title.required' => 'message']);"),
            SOURCE.to_owned() + "<?php helper();",
        ] {
            let (_h, mut main, doc, range) = setup(&source);
            assert!(main.file_actions(&doc, range).is_empty(), "{}", source);
        }
    }
    #[test]
    fn promotion_cancel_stale_collision_and_foreign_owners_return_no_partial_action() {
        for case in 0..8 {
            let (h, mut main, mut doc, range) = setup(SOURCE);
            let lazy = main.file_actions(&doc, range).remove(0);
            match case {
                0 => h.answer_next_prompt(None),
                1 => {
                    h.answer_next_prompt(Some(&["StorePostRequest"]));
                    h.answer_next_prompt(None);
                }
                2 => {
                    doc.version += 1;
                }
                3 => {
                    doc.text = doc.text.replace("'required'", "'nullable'");
                }
                4 => {
                    h.add_file("app/Http/Requests/StorePostRequest.php", "existing");
                    h.answer_next_prompt(Some(&["StorePostRequest"]));
                }
                5 => {
                    h.add_project("/project/app/Http/Requests");
                    h.answer_next_prompt(Some(&["StorePostRequest"]));
                }
                6 => {
                    h.add_file(
                        "composer.json",
                        r#"{"autoload":{"psr-4":{"App\\":["app/","other/"]}}}"#,
                    );
                    h.answer_next_prompt(Some(&["StorePostRequest"]));
                }
                _ => {
                    h.answer_next_prompt(Some(&["../StorePostRequest"]));
                }
            }
            assert!(main.resolve_file_action(&doc, lazy).is_err(), "case {case}");
            if case != 4 {
                assert!(
                    h.invocation()
                        .project()
                        .unwrap()
                        .read_file("/project/app/Http/Requests/StorePostRequest.php")
                        .is_err()
                );
            }
            assert!(h.process_runs().is_empty());
        }
    }
    #[test]
    fn promotion_preserves_literal_unicode_crlf_and_absolute_mapped_namespace() {
        let source = SOURCE
            .replace("App\\Http\\Controllers", "Acme\\Web\\Http\\Controllers")
            .replace("'nullable|string'", "'nullable|string|in:café😀'")
            .replace('\n', "\r\n");
        let (h, mut main, mut doc, range) = setup(&source);
        h.add_dir("src");
        h.add_dir("src/Web");
        h.add_dir("src/Web/Http");
        h.add_dir("src/Web/Http/Controllers");
        doc.path = "/project/src/Web/Http/Controllers/PostController.php".into();
        h.add_file(&doc.path, &source);
        h.open_editor(&doc.path, "php", &source, &[]);
        h.add_file(
            "composer.json",
            r#"{"autoload":{"psr-4":{"Acme\\Web\\":"src/Web/"}}}"#,
        );
        let lazy = main.file_actions(&doc, range).remove(0);
        h.answer_next_prompt(Some(&["StorePostRequest"]));
        h.answer_next_prompt(Some(&["yes"]));
        let ready = main.resolve_file_action(&doc, lazy.clone()).unwrap();
        let [FileOperation::Create { path, text, .. }] = ready.operations.as_slice() else {
            panic!("one create")
        };
        assert_eq!(path, "/project/src/Web/Http/Requests/StorePostRequest.php");
        assert!(
            text.contains("namespace Acme\\Web\\Http\\Requests;")
                && text.contains("'nullable|string|in:café😀'")
        );
        assert!(!text.contains("\r\r\n"));
        assert!(text.lines().count() > 10);
        assert_eq!(
            ready.edits[0].1[0].range.end,
            position(&doc.text, doc.text.len())
        );
        assert!(
            ready.edits[0].1[0]
                .text
                .contains("\\Acme\\Web\\Http\\Requests\\StorePostRequest $request")
        );
        assert!(main.resolve_file_action(&doc, lazy).is_err());
    }
    #[test]
    fn registered_assignment_validation_promotes_without_changing_local_array_return_binding() {
        let source = SOURCE
            .replace(
                "return $request->validate",
                "$validated = $request->validate",
            )
            .replace("]);", "]);\n        return $validated;");
        let (h, mut main, doc, range) = setup(&source);
        let actions = main.file_actions(&doc, range);
        assert_eq!(
            actions.len(),
            1,
            "assigned authored validation promotion action is missing"
        );
        h.answer_next_prompt(Some(&["StorePostRequest"]));
        h.answer_next_prompt(Some(&["yes"]));
        let ready = main.resolve_file_action(&doc, actions[0].clone()).unwrap();
        assert_eq!(ready.operations.len(), 1);
        assert_eq!(ready.edits.len(), 1);
        let text = &ready.edits[0].1[0].text;
        assert!(text.contains("$validated = $request->validated();\n        return $validated;"));
        assert!(text.contains("\\App\\Http\\Requests\\StorePostRequest $request"));
        let [FileOperation::Create { text: request, .. }] = ready.operations.as_slice() else {
            panic!("one create")
        };
        assert!(
            request.contains("'title' => ['required', 'string']")
                && request.contains("'body' => 'nullable|string'")
        );
        assert_eq!(
            h.invocation()
                .project()
                .unwrap()
                .read_file(&doc.path)
                .unwrap(),
            source
        );
        assert!(h.process_runs().is_empty());
    }

    #[test]
    fn assigned_promotion_refuses_mutated_request_bindings_and_external_rules() {
        let base = SOURCE
            .replace(
                "return $request->validate",
                "$validated = $request->validate",
            )
            .replace("]);", "]); return $validated;");
        for source in [
            base.replace("$validated", "$request"),
            base.replace("$validated", "$_POST"),
            base.replace("'body' => 'nullable|string'", "'body' => $external"),
        ] {
            let (_h, mut main, doc, range) = setup(&source);
            assert!(main.file_actions(&doc, range).is_empty(), "{source}");
        }
    }
    fn ordinary_setup(source: &str, parameter: &str) -> (MockHost, Main, Document, Range) {
        // Keep the same production Composer/ownership fixture as the original
        // tests while selecting the actual imported parameter spelling.
        let sentinel = source.replace(parameter, "\\Illuminate\\Http\\Request");
        let (host, main, mut doc, _) = setup(&sentinel);
        host.add_file(&doc.path, source);
        host.open_editor(&doc.path, "php", source, &[]);
        doc.text = source.into();
        let at = position(source, source.find(parameter).unwrap());
        (host, main, doc, Range::new(at, at))
    }
    #[test]
    fn imported_request_alias_framework_controller_and_ordinary_statements_preserve_source_bindings()
     {
        let source = "<?php\nnamespace App\\Http\\Controllers;\nuse Illuminate\\Http\\Request as HttpRequest;\nuse Illuminate\\Routing\\Controller as FrameworkController;\nuse Illuminate\\Validation\\Rule as ValidationRule;\nuse App\\Enums\\Status;\nfinal class PostController extends FrameworkController\n{\n    public function store(HttpRequest $request, int $id)\n    {\n        // Keep the original local binding and every later expression.\n        $data = $request->validate([\n            'title' => ['required', 'string'],\n            'status' => [ValidationRule::enum(Status::class)],\n            'slug' => [ValidationRule::unique('posts', 'slug')->ignore(123)],\n        ]);\n        $data['id'] = $id;\n        return response()->json($data);\n    }\n    public function show(int $id): array\n    {\n        return ['id' => $id];\n    }\n}\n";
        let (host, mut main, doc, selection) = ordinary_setup(source, "HttpRequest $request");
        // The helper selects the full marker; action accepts only the type span.
        let selection = Range::new(selection.start, selection.start);
        let action = main
            .file_actions(&doc, selection)
            .into_iter()
            .find(|action| {
                action
                    .data
                    .as_deref()
                    .is_some_and(|data| data.starts_with("request-promote:"))
            })
            .unwrap();
        host.answer_next_prompt(Some(&["StorePostRequest"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = main.resolve_file_action(&doc, action).unwrap();
        let FileOperation::Create {
            path,
            text,
            overwrite,
        } = &ready.operations[0]
        else {
            panic!("one create")
        };
        assert_eq!(path, "/project/app/Http/Requests/StorePostRequest.php");
        assert!(!overwrite);
        assert!(
            text.contains("\\Illuminate\\Validation\\Rule::enum(\\App\\Enums\\Status::class)"),
            "{text}"
        );
        assert!(
            text.contains("\\Illuminate\\Validation\\Rule::unique('posts', 'slug')->ignore(123)"),
            "{text}"
        );
        let replacement = &ready.edits[0].1[0].text;
        let expected = source.replace(
            "HttpRequest $request",
            "\\App\\Http\\Requests\\StorePostRequest $request",
        );
        let begin = expected.find("$request->validate(").unwrap();
        let open = begin + "$request->validate".len();
        let end = crate::php::closing(&expected, open).unwrap() + 1;
        let mut expected = expected;
        expected.replace_range(begin..end, "$request->validated()");
        assert_eq!(replacement, &expected);
        assert!(
            host.invocation()
                .project()
                .unwrap()
                .read_file(path)
                .is_err()
        );
        assert_eq!(
            host.invocation()
                .project()
                .unwrap()
                .read_file(&doc.path)
                .unwrap(),
            source
        );
        assert!(host.process_runs().is_empty());
    }
    #[test]
    fn quoted_rule_spelling_comments_and_post_validation_expressions_remain_unchanged() {
        for source in [
            SOURCE.replace("'nullable|string'", "'required\\\\escaped'"),
            SOURCE.replace("return $request", "/* ordinary comment */ return $request"),
            SOURCE.replace("): array", ")"),
            SOURCE.replace("$request):", "$request, int $id):"),
            SOURCE
                .replace("return $request->validate", "$data = $request->validate")
                .replace("]);", "]); helper($data); return $data;"),
        ] {
            let (host, mut main, doc, selection) = setup(&source);
            let action = main
                .file_actions(&doc, selection)
                .into_iter()
                .find(|action| {
                    action
                        .data
                        .as_deref()
                        .is_some_and(|data| data.starts_with("request-promote:"))
                })
                .unwrap_or_else(|| panic!("{source}"));
            host.answer_next_prompt(Some(&["StorePostRequest"]));
            host.answer_next_prompt(Some(&["yes"]));
            let ready = main.resolve_file_action(&doc, action).unwrap();
            let replacement = &ready.edits[0].1[0].text;
            assert!(
                replacement.contains("$request->validated()"),
                "{replacement}"
            );
            if source.contains("helper($data)") {
                assert!(replacement.contains("helper($data); return $data;"));
            }
            if source.contains("/* ordinary comment */") {
                assert!(replacement.contains("/* ordinary comment */"));
            }
            let FileOperation::Create { text, .. } = &ready.operations[0] else {
                panic!()
            };
            if source.contains("required\\\\escaped") {
                assert!(text.contains("required\\\\escaped"));
            }
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    fn external_rule_expressions_receiver_mutation_multiple_validations_and_unknown_base_classes_stay_withheld()
     {
        let base = SOURCE.replace(
            "namespace App\\Http\\Controllers;",
            "namespace App\\Http\\Controllers;\nuse Illuminate\\Validation\\Rule;",
        );
        for source in [
            base.replace("'nullable|string'", "Rule::unique('posts')->ignore($id)"),
            base.replace("'nullable|string'", "Rule::requiredIf(fn () => true)"),
            base.replace("'nullable|string'", "self::RULES"),
            base.replace("'nullable|string'", "config('validation.rule')"),
            base.replace("'nullable|string'", "new CustomRule()"),
            base.replace(
                "return $request",
                "$request->merge(['title'=>'changed']); return $request",
            ),
            base.replace("return $request", "$request = request(); return $request"),
            base.replace("return $request->validate", "$data = $request->validate")
                .replace("]);", "]); return $request->validate(['other'=>'string']);"),
            base.replace(
                "class PostController",
                "class PostController extends UnknownController",
            ),
        ] {
            let (_host, mut main, doc, selection) = setup(&source);
            assert!(
                main.file_actions(&doc, selection)
                    .iter()
                    .all(|action| !action
                        .data
                        .as_deref()
                        .is_some_and(|data| data.starts_with("request-promote:"))),
                "{source}"
            );
        }
    }

    #[test]
    fn strict_types_is_retained_for_moved_rule_expression_evaluation() {
        let source = SOURCE.replace("<?php\n", "<?php\ndeclare(strict_types=1);\n");
        let (host, mut main, doc, selection) = setup(&source);
        let action = main
            .file_actions(&doc, selection)
            .into_iter()
            .find(|action| {
                action
                    .data
                    .as_deref()
                    .is_some_and(|data| data.starts_with("request-promote:"))
            })
            .unwrap();
        host.answer_next_prompt(Some(&["StorePostRequest"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = main.resolve_file_action(&doc, action).unwrap();
        let FileOperation::Create { text, .. } = &ready.operations[0] else {
            panic!()
        };
        assert!(
            text.starts_with("<?php\n\ndeclare(strict_types=1);\n\nnamespace App\\Http\\Requests;"),
            "{text}"
        );
        assert!(
            ready.edits[0].1[0]
                .text
                .contains("declare(strict_types=1);")
        );
    }
    #[test]
    fn selected_method_and_nested_controller_namespace_keep_other_requests_unchanged() {
        let source = "<?php\nnamespace App\\Http\\Controllers\\Admin;\nuse Illuminate\\Http\\Request;\nclass PostController\n{\n    public function first(Request $first): array { return $first->validate(['first'=>'required']); }\n    public function second(Request $second, int $id): array\n    {\n        $data = $second->validate(['title'=>'required|string']);\n        $data['id'] = $id;\n        return $data;\n    }\n}\n";
        let (host, mut main, mut doc, _) = ordinary_setup(source, "Request $second");
        host.add_dir("app/Http/Controllers/Admin");
        doc.path = "/project/app/Http/Controllers/Admin/PostController.php".into();
        host.add_file(&doc.path, source);
        host.open_editor(&doc.path, "php", source, &[]);
        let at = position(source, source.find("Request $second").unwrap());
        let action = main
            .file_actions(&doc, Range::new(at, at))
            .into_iter()
            .find(|action| {
                action
                    .data
                    .as_deref()
                    .is_some_and(|data| data.starts_with("request-promote:"))
            })
            .unwrap();
        host.answer_next_prompt(Some(&["StorePostRequest"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = main.resolve_file_action(&doc, action).unwrap();
        let replacement = &ready.edits[0].1[0].text;
        assert!(
            replacement.contains(
                "first(Request $first): array { return $first->validate(['first'=>'required']); }"
            ),
            "{replacement}"
        );
        assert!(
            replacement
                .contains("second(\\App\\Http\\Requests\\StorePostRequest $second, int $id)"),
            "{replacement}"
        );
        assert!(replacement.contains("$data = $second->validated();"));
        assert!(replacement.contains("$data['id'] = $id;"));
    }
    #[test]
    fn expanded_promotion_revalidates_import_identity_and_target_after_preparation() {
        let source = "<?php\nnamespace App\\Http\\Controllers;\nuse Illuminate\\Http\\Request;\nclass PostController\n{\n    public function store(Request $request)\n    {\n        $data = $request->validate(['title'=>'required']);\n        return response()->json($data);\n    }\n}\n";
        for mode in 0..4 {
            let (host, mut main, mut doc, selection) = ordinary_setup(source, "Request $request");
            let action = main
                .file_actions(&doc, selection)
                .into_iter()
                .find(|action| {
                    action
                        .data
                        .as_deref()
                        .is_some_and(|data| data.starts_with("request-promote:"))
                })
                .unwrap();
            match mode {
                0 => doc.text = source.replace("Illuminate\\Http\\Request", "Vendor\\Request"),
                1 => {
                    host.add_file("app/Http/Requests/StorePostRequest.php", "KEEP");
                    host.answer_next_prompt(Some(&["StorePostRequest"]));
                }
                2 => host.answer_next_prompt(None),
                _ => {
                    host.answer_next_prompt(Some(&["StorePostRequest"]));
                    host.answer_next_prompt(None);
                }
            }
            assert!(main.resolve_file_action(&doc, action).is_err(), "{mode}");
            assert_eq!(
                host.invocation()
                    .project()
                    .unwrap()
                    .read_file("/project/app/Http/Controllers/PostController.php")
                    .unwrap(),
                source
            );
            assert!(host.process_runs().is_empty());
        }
    }
    #[test]
    #[ignore = "requires LARAVEL_PROMOTION_FIXTURE pointing to a disposable Laravel acceptance app and php"]
    fn generated_form_request_preserves_real_laravel_validation_array_and_rule_factory_behavior() {
        let fixture = std::env::var("LARAVEL_PROMOTION_FIXTURE")
            .expect("set disposable Laravel fixture path");
        let directory = std::path::Path::new(&fixture)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            directory.starts_with("laravel-artisan-acceptance-"),
            "use a disposable acceptance app"
        );
        let source = "<?php\ndeclare(strict_types=1);\nnamespace App\\Http\\Controllers;\nuse Illuminate\\Http\\Request;\nuse Illuminate\\Validation\\Rule;\nclass PostController\n{\n    public function store(Request $request, int $id): array\n    {\n        $data = $request->validate([\n            'title' => ['required', 'string'],\n            'status' => [Rule::in(['draft', 'live'])],\n            'note' => 'nullable|string',\n        ]);\n        $data['id'] = $id;\n        return $data;\n    }\n}\n";
        let (host, mut main, doc, selection) = ordinary_setup(source, "Request $request");
        let action = main
            .file_actions(&doc, selection)
            .into_iter()
            .find(|action| {
                action
                    .data
                    .as_deref()
                    .is_some_and(|data| data.starts_with("request-promote:"))
            })
            .unwrap();
        host.answer_next_prompt(Some(&["ParityQaRequest"]));
        host.answer_next_prompt(Some(&["yes"]));
        let ready = main.resolve_file_action(&doc, action).unwrap();
        let FileOperation::Create { text, .. } = &ready.operations[0] else {
            panic!()
        };
        let payload = serde_json::json!({"before":source.replace("class PostController","class ParityQaBeforeController"),"request":text,"after":ready.edits[0].1[0].text.replace("class PostController","class ParityQaAfterController")});
        let php = r#"
            require $argv[1].'/vendor/autoload.php';
            $app=require $argv[1].'/bootstrap/app.php';
            $app->make(Illuminate\Contracts\Console\Kernel::class)->bootstrap();
            $payload=json_decode($argv[2],true,512,JSON_THROW_ON_ERROR);
            foreach(['before','request','after'] as $key){eval(substr($payload[$key],5));}
            $cases=[['title'=>'Hello','status'=>'draft','extra'=>'omitted'],['title'=>'Hi','status'=>'live','note'=>null],['status'=>'draft'],['title'=>'Hi','status'=>'invalid']];
            foreach($cases as $input){
                $before=Illuminate\Http\Request::create('/','POST',$input);
                $after=App\Http\Requests\ParityQaRequest::createFrom($before);
                $after->setContainer($app);$after->setRedirector($app['redirect']);
                try{$left=['ok'=>(new App\Http\Controllers\ParityQaBeforeController)->store($before,42)];}catch(Illuminate\Validation\ValidationException $error){$left=['errors'=>$error->errors()];}
                try{$after->validateResolved();$right=['ok'=>(new App\Http\Controllers\ParityQaAfterController)->store($after,42)];}catch(Illuminate\Validation\ValidationException $error){$right=['errors'=>$error->errors()];}
                if($left!==$right){throw new RuntimeException('Validation output differs: '.json_encode([$left,$right]));}
                if(isset($right['ok'])&&!is_array($right['ok'])){throw new RuntimeException('Array became ValidatedInput');}
            }
            echo 'PASS: actual generated FormRequest preserves valid/invalid results and array shape';
        "#;
        let output = std::process::Command::new("php")
            .args(["-r", php, &fixture, &payload.to_string()])
            .output()
            .expect("run PHP");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("PASS:"));
        assert!(host.process_runs().is_empty());
    }
}
