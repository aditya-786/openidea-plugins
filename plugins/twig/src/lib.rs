//! Twig for OpenIDEA: highlighting, structure view (blocks, macros) and
//! folding (tag pairs, comments) from a hand-written lexer, exported as the
//! plugin's `syntax-provider`. Detection comes from `languages/twig.toml`.

mod checks;
mod twig_config;
mod reviewed_symbols;
mod macro_rename;
mod template_index;
mod signatures;
mod formatting;
mod comments;
mod delimiters;
mod injections;
mod indexing;
mod complete;
mod navigate;
mod php_context;
mod macro_arguments;
mod hover;
mod gutter;

use std::collections::BTreeMap;
use std::ops::Range;

use std::path::Path;

use navigate::{Extension, ExtensionKind};
use openidea_plugin::providers::{CodeAction, CompletionItem, CompletionKind, Document, Providers};
use openidea_plugin::syntax::{Fold, FoldKind, Highlight, Symbol, SymbolKind, SyntaxProvider, Token};
use openidea_plugin::text::{offset, position};
use openidea_plugin::{
    Context, Diagnostic, Error, Event, EventKind, Invocation, Location, LogLevel, Plugin, Project, Range as EditorRange, Severity, TextEdit, host,
};

const SOURCE: &str = "Twig";
const MAX_SYNTAX_BYTES: usize = 256 << 10;
const MAX_SYNTAX_TOKENS: usize = 16_384;
const MAX_STRUCTURE_TAGS: usize = 4096;
const MAX_STRUCTURE_ITEMS: usize = 2048;

const EXTENSION_GLOBS: &[&str] = &["src/**/*.php", "app/**/*.php", "lib/**/*.php"];
const EXTENSION_FILES: u32 = 1500;
const TEMPLATE_GLOBS: &[&str] = &["**/*.twig"];
const TEMPLATE_FILES: u32 = 1000;
const SCAN_FILES: u32 = EXTENSION_FILES + TEMPLATE_FILES;

#[derive(Default)]
pub struct Twig {
    reviewed_symbols:reviewed_symbols::State,
    /// Filters, functions and tests registered in the project's PHP, by absolute file path.
    extensions: BTreeMap<String, Vec<Extension>>,
    /// Literal render-call and `#[Template]` variable sites in project PHP, by absolute file path.
    renders: BTreeMap<String, Vec<php_context::Site>>,
    /// Template references and blocks of the project's Twig files, by absolute file path, for the gutter's reverse links.
    templates: BTreeMap<String, gutter::Facts>,
    /// PHP files still to scan, scanned in short slices so no single call blocks.
    pending: Vec<String>,
    scanned_files: u32,
    scanned_bytes: usize,
}

const SCAN_SLICE: std::time::Duration = std::time::Duration::from_millis(40);
const MAX_RESCANNED_PHP: usize = 64;

impl Twig {
    fn scan_extensions(&mut self) {
        self.pending.clear();
        self.scanned_files=0;
        self.scanned_bytes=0;
        let php=host::setting_bool("projectExtensions").unwrap_or(true);
        let mut templates=Vec::new();
        for project in host::projects().unwrap_or_default() {
            for glob in EXTENSION_GLOBS.iter().filter(|_|php) {
                let room=EXTENSION_FILES.saturating_sub(self.pending.len() as u32);
                if room==0 {break;}
                self.pending.extend(project.find_files(glob,room).unwrap_or_default());
            }
            for glob in TEMPLATE_GLOBS {
                let room=TEMPLATE_FILES.saturating_sub(templates.len() as u32);
                templates.extend(project.find_files(glob,room).unwrap_or_default().into_iter().filter(|p|!p.contains("/vendor/")&&!p.contains("/node_modules/")));
            }
        }
        self.pending.extend(templates);
        self.pending.reverse();
        self.drain_pending();
    }
    fn drain_pending(&mut self) {self.drain(SCAN_SLICE)}
    fn drain(&mut self,slice:std::time::Duration) {
        let deadline=std::time::Instant::now()+slice;
        while std::time::Instant::now()<deadline {
            let Some(path)=self.pending.pop() else {return};
            if self.scanned_files>=SCAN_FILES||self.scanned_bytes+(128<<10)>(12<<20) {self.pending.clear();return;}
            let Some(project)=project_of(&path) else {self.forget(&path);continue};
            self.scanned_files+=1;
            self.scanned_bytes+=self.scan_file(&project,&path);
        }
    }
    /// Queues paths for the 40 ms slices instead of dropping what they said: each is re-read or forgotten there.
    fn requeue(&mut self,paths:Vec<String>) {
        if paths.is_empty() {return;}
        if self.pending.is_empty() {self.scanned_files=0;self.scanned_bytes=0;}
        let room=(SCAN_FILES as usize).saturating_sub(self.pending.len());
        for path in paths.into_iter().take(room) {
            if !self.pending.contains(&path) {self.pending.push(path);}
        }
    }
    fn forget(&mut self,path:&str) {self.extensions.remove(path);self.renders.remove(path);self.templates.remove(path);}
    fn scan_file(&mut self,project:&Project,path:&str)->usize {
        let root=project.root().unwrap_or_default();
        if path.ends_with(".twig") {
            let text=(same_owner(&root,path)&&project.stat(path).is_ok_and(|s|s.kind==openidea_plugin::FileKind::File&&s.size<=128<<10))
                .then(||project.read_file(path).ok()).flatten();
            let bytes=text.as_ref().map_or(0,String::len);
            match text.zip(delimiters::Syntax::scope()).map(|(t,_syntax)|gutter::facts(&t)) {
                Some(facts)=>{self.templates.insert(path.to_owned(),facts);}
                None=>{self.templates.remove(path);}
            }
            return bytes;
        }
        let source=if host::setting_bool("projectExtensions").unwrap_or(true)&&same_owner(&root,path)&&extension_source(&root,path)
            &&project.stat(path).is_ok_and(|s|s.kind==openidea_plugin::FileKind::File&&s.size<=128<<10){
            project.read_file(path).ok().filter(|s|s.len()<=128<<10)
        }else{None};
        let bytes=source.as_ref().map_or(0,String::len);
        let sites=source.as_deref().map(php_context::render_sites).unwrap_or_default();
        if sites.is_empty(){self.renders.remove(path);}else{self.renders.insert(path.to_owned(),sites);}
        let found=if is_symfony(project) {Vec::new()} else {source.map(|php|navigate::extensions(&php)).unwrap_or_default()};
        if found.is_empty(){self.extensions.remove(path);}else{self.extensions.insert(path.to_owned(),found);}
        bytes
    }

    /// Cached render sites that pass variables to this template (no file reads beyond the Twig configuration).
    fn template_sites(&self, project: &Project, template_file: &str) -> Vec<(&String, &php_context::Site)> {
        let Some(root) = project.root().ok() else { return Vec::new() };
        let Some((fixed, _)) = main_template_dirs(project) else { return Vec::new() };
        let Some(relative) = template_file.strip_prefix(&format!("{root}/")) else { return Vec::new() };
        let Some(name) = fixed.iter().find_map(|dir| relative.strip_prefix(&format!("{dir}/"))) else { return Vec::new() };
        if template_path(project, name).as_deref() != Some(template_file) {
            return Vec::new();
        }
        let roots: Vec<_> = host::projects().unwrap_or_default().into_iter().filter_map(|p| p.root().ok()).collect();
        self.renders
            .iter()
            .filter(|(path, _)| extension_source(&root, path) && roots.iter().filter(|r| Path::new(path.as_str()).starts_with(r)).max_by_key(|r| r.len()).is_some_and(|r| *r == root))
            .flat_map(|(path, sites)| sites.iter().filter(|s| s.template == name).map(move |s| (path, s)))
            .collect()
    }

    /// Variable names for completion, from the render sites cached when PHP was scanned or saved.
    fn template_variable_names(&self, project: &Project, template_file: &str) -> Vec<String> {
        self.template_sites(project, template_file).into_iter().flat_map(|(_, site)| site.keys.iter().map(|(key, _)| key.clone())).collect()
    }

    /// Literal variable keys passed to this template by project render sites, with the controller key locations.
    fn template_variables(&self, project: &Project, template_file: &str) -> Vec<(String, Location)> {
        let mut found = Vec::new();
        for (path, site) in self.template_sites(project, template_file) {
            let Some(php) = project.read_file(path).ok().filter(|t| t.len() <= 128 << 10) else { continue };
            if !php_context::render_sites(&php).contains(site) {
                continue;
            }
            for (key, range) in &site.keys {
                let at = EditorRange::new(position(&php, range.start), position(&php, range.end));
                found.push((key.clone(), Location { path: path.clone(), range: at }));
            }
        }
        found
    }

    fn extra_names(&self, root: &str, kind: ExtensionKind) -> Vec<&str> {
        if root.is_empty()||!host::setting_bool("projectExtensions").unwrap_or(true){return Vec::new();}
        let roots:Vec<_>=host::projects().unwrap_or_default().into_iter().filter_map(|p|p.root().ok()).collect();
        let inside=|(path,_):&(&String,&Vec<Extension>)|extension_source(root,path)&&roots.iter().filter(|r|Path::new(path.as_str()).starts_with(r)).max_by_key(|r|r.len()).is_some_and(|r|r==root);
        let mut names: Vec<&str> = self
            .extensions
            .iter()
            .filter(inside)
            .flat_map(|(_, found)| found)
            .filter(|e| e.kind == kind)
            .map(|e| e.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

fn canonical(root:&str,path:&str)->bool {
    root.starts_with('/')&&root!="/"&&!root.ends_with('/')&&root.len()<=4096&&path.len()<=4096
        &&!root.chars().chain(path.chars()).any(char::is_control)&&!root.contains('\\')&&!path.contains('\\')
        &&!root.contains("//")&&!path.contains("//")&&!root.split('/').chain(path.split('/')).any(|p|matches!(p,"."|".."))
        &&path.starts_with(&format!("{root}/"))
}
fn extension_source(root:&str,path:&str)->bool {
    canonical(root,path)&&path.strip_prefix(&format!("{root}/")).is_some_and(|p|p.ends_with(".php")&&["src/","app/","lib/"].iter().any(|prefix|p.starts_with(prefix)))
}
fn same_owner(root:&str,path:&str)->bool {canonical(root,path)&&project_of(path).and_then(|p|p.root().ok()).as_deref()==Some(root)}

fn safe_namespace_part(value: &str, allow_slash: bool) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('/')
        && !value.contains(['\\', ':', '*', '?', '[', ']', '@'])
        && !value.chars().any(char::is_control)
        && (allow_slash || !value.contains('/'))
        && value.trim_end_matches('/').split('/').all(|p| !p.is_empty() && !matches!(p, "." | ".."))
}

/// `templateNamespaces` entries as (namespace, project-relative directory): bounded, relative, no `..`.
fn template_namespaces() -> Vec<(String, String)> {
    let raw = host::setting("templateNamespaces").unwrap_or_default();
    let entries: Vec<serde_json::Value> = if raw.len() > 16 << 10 { Vec::new() } else { serde_json::from_str(&raw).unwrap_or_default() };
    entries
        .into_iter()
        .take(32)
        .filter_map(|e| {
            let field = |key: &str| e.get(key).and_then(|v| v.as_str()).map(str::to_owned);
            Some((field("namespace")?.trim_start_matches('@').to_owned(), field("path")?.trim_end_matches('/').to_owned()))
        })
        .filter(|(namespace, path)| safe_namespace_part(namespace, false) && safe_namespace_part(path, true))
        .collect()
}

/// Config blocks that are always active (`twig.yaml` top level) and blocks that apply in some environment only.
fn config_blocks(project: &Project) -> ConfigBlocks {
    let root = project.root().unwrap_or_default();
    if let Some(known) = project_fact(&root, |f| f.blocks.clone()) {
        return known;
    }
    let found = read_config_blocks(project);
    remember_project_fact(&root, |f| f.blocks = Some(found.clone()));
    found
}

fn read_config_blocks(project: &Project) -> ConfigBlocks {
    let read = |path: &str| project.read_file(path).ok().map(|t| twig_config::blocks(&t));
    let mut always = None;
    let mut some_env = Vec::new();
    if let Some((top, whens)) = read("config/packages/twig.yaml") {
        always = Some(top);
        some_env.extend(whens);
    }
    for env in ["dev", "prod", "test"] {
        if let Some((top, whens)) = read(&format!("config/packages/{env}/twig.yaml")) {
            some_env.push(top);
            some_env.extend(whens);
        }
    }
    (always, some_env)
}

/// Setting entries plus Symfony `twig.paths` from every environment's config; a namespace named by the setting
/// ignores the config, and a namespace the config maps to different directories is refused.
fn merged_template_namespaces(project: &Project) -> Vec<(String, String)> {
    let mut all = template_namespaces();
    let explicit: Vec<String> = all.iter().map(|(n, _)| n.clone()).collect();
    let (always, some_env) = config_blocks(project);
    let mut configured: Vec<(String, String)> = Vec::new();
    for (dir, ns) in always.iter().chain(&some_env).flat_map(|b| b.paths.iter()) {
        let (Some(ns), dir) = (ns, dir.trim_end_matches('/')) else { continue };
        if explicit.contains(ns) || !safe_namespace_part(ns, false) || !safe_namespace_part(dir, true) {
            continue;
        }
        if !configured.iter().any(|(n, d)| n == ns && d == dir) {
            configured.push((ns.clone(), dir.to_owned()));
        }
    }
    let conflicting: Vec<String> = configured.iter().filter(|(n, _)| configured.iter().filter(|(m, _)| m == n).count() > 1).map(|(n, _)| n.clone()).collect();
    all.extend(configured.into_iter().filter(|(n, _)| !conflicting.contains(n)));
    all
}

/// Main-namespace template directories: (always active, environment-dependent). `None` when `default_path` differs between sources.
fn main_template_dirs(project: &Project) -> Option<(Vec<String>, Vec<String>)> {
    let (always, some_env) = config_blocks(project);
    let defaults: Vec<&String> = always.iter().chain(&some_env).filter_map(|b| b.default_path.as_ref()).collect();
    if defaults.iter().any(|d| *d != defaults[0]) {
        return None;
    }
    let usable = |dir: &str| safe_namespace_part(dir, true).then(|| dir.trim_end_matches('/').to_owned());
    let mut fixed = Vec::new();
    let mut variable = Vec::new();
    let add = |list: &mut Vec<String>, other: &[String], dir: Option<String>| {
        if let Some(dir) = dir {
            if !list.contains(&dir) && !other.contains(&dir) {
                list.push(dir);
            }
        }
    };
    let base_default = always.as_ref().and_then(|b| b.default_path.as_deref()).unwrap_or("templates");
    add(&mut fixed, &[], usable(base_default));
    for (dir, _) in always.iter().flat_map(|b| b.paths.iter()).filter(|(_, ns)| ns.is_none()) {
        add(&mut fixed, &[], usable(dir));
    }
    for block in &some_env {
        if let Some(default) = &block.default_path {
            add(&mut variable, &fixed, usable(default));
        }
        for (dir, _) in block.paths.iter().filter(|(_, ns)| ns.is_none()) {
            add(&mut variable, &fixed, usable(dir));
        }
    }
    Some((fixed, variable))
}

fn template_path(project: &Project, name: &str) -> Option<String> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', ':', '*', '?', '[', ']'])
        || name.chars().any(char::is_control)
        || name.split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
    {
        return None;
    }
    let root = project.root().ok()?;
    let existing = |direct: &str| -> Option<String> {
        let full = format!("{root}/{direct}");
        (same_owner(&root, &full) && project.stat(direct).ok()?.kind == openidea_plugin::FileKind::File).then_some(full)
    };
    let Some(namespaced) = name.strip_prefix('@') else {
        let (fixed, variable) = main_template_dirs(project)?;
        if let Some(found) = fixed.iter().find_map(|dir| existing(&format!("{dir}/{name}"))) {
            return Some(found);
        }
        let mut found: Vec<String> = variable.iter().filter_map(|dir| existing(&format!("{dir}/{name}"))).collect();
        found.dedup();
        return (found.len() == 1).then(|| found.remove(0));
    };
    let (namespace, rest) = namespaced.split_once('/')?;
    let mapped = merged_template_namespaces(project).into_iter().filter(|(n, _)| n == namespace).collect::<Vec<_>>();
    if mapped.len() != 1 {
        return None;
    }
    existing(&format!("{}/{rest}", mapped[0].1))
}

fn macros_of(project: &Project, current: &Document, template: &str) -> Option<(String, String)> {
    if template == "_self" {
        return Some((current.path.clone(), current.text.clone()));
    }

    let path = template_path(project, template)?;
    // A literal import of this same template must use the supplied current
    // buffer, just like _self, rather than saved offsets/names.
    if path == current.path { return Some((path, current.text.clone())); }
    if project.stat(&path).ok()?.size > 128 << 10 { return None; }
    let text = project.read_file(&path).ok()?;
    if text.len() > 128 << 10 {
        return None;
    }
    Some((path, text))
}

fn resolved_macro(doc:&Document,call:usize,receiver:Option<&str>,name:&str)->Option<(navigate::Macro,String)> {
    let project=project_of(&doc.path);
    let (template,name)=if receiver==Some("_self") {Some(("_self".to_owned(),name.to_owned()))}
        else if !project.as_ref().is_some_and(is_symfony) {navigate::binding(&doc.text,call,receiver,name)} else {None}?;
    let (path,text)=if template=="_self" {(doc.path.clone(),doc.text.clone())}else{macros_of(project.as_ref()?,doc,&template)?};
    if path!=doc.path && !checks::check(&text).is_empty(){return None;}
    let mut found=navigate::macros(&text).into_iter().filter(|m|m.name==name);let result=found.next()?;if found.next().is_some(){return None;}
    Some((result,path))
}

fn in_twig_code(text: &str, offset: usize) -> bool {
    navigate::code_at(text, offset, false)
}

fn macro_items(project: &Project, doc: &Document, offset: usize) -> Vec<CompletionItem> {
    let (receiver, range) = match navigate::dotted_at(&doc.text, offset) {
        Some((receiver, _, range)) => (receiver, range),
        None => (None, offset..offset),
    };
    let replace = EditorRange::new(position(&doc.text, range.start), position(&doc.text, range.end));

    let template = match receiver {
        Some(alias) if alias == "_self" && in_twig_code(&doc.text, range.start) => Some("_self".into()),
        Some(alias) if in_twig_code(&doc.text, range.start) => {
            navigate::binding(&doc.text, range.start, Some(&alias), "").map(|(template, _)| template)
        }
        Some(_) => None,
        None if navigate::code_at(&doc.text, range.start, false) => from_import_template(&doc.text, range.start),
        None => None,
    };
    let Some(template) = template else {
        return Vec::new();
    };
    let Some((source_path, text)) = macros_of(project, doc, &template) else {
        return Vec::new();
    };

    // External broken source is not a trustworthy owner. The current buffer
    // may contain the unfinished call being completed, so retain local recovery.
    if source_path != doc.path && !checks::check(&text).is_empty() { return Vec::new(); }
    let macros = navigate::macros(&text);
    let mut counts = BTreeMap::new();
    for declaration in &macros { *counts.entry(declaration.name.as_str()).or_insert(0usize) += 1; }
    macros.iter()
        .filter(|m| counts.get(m.name.as_str()) == Some(&1))
        .map(|m| {
            let mut item = CompletionItem::new(&m.name, CompletionKind::Function);
            item.detail = Some(format!("macro({})", m.arguments));
            item.replace = Some(replace);
            item
        })
        .collect()
}

/// The template of a `{% from 'x' import …` tag the caret is inside the name list of.
fn from_import_template(text: &str, offset: usize) -> Option<String> {
    let syntax=delimiters::Syntax::current();
    let tag_start = text[..offset].rfind(&syntax.values[0])?;
    if text[tag_start..offset].contains(&syntax.values[1]) {
        return None;
    }

    let body = text[tag_start + syntax.values[0].len()..offset].trim_start_matches(['-', '~']).trim_start();
    let rest = body.strip_prefix("from")?.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let inner = &rest[1..];
    let end = inner.find(quote)?;
    let after = inner[end + 1..].trim_start();

    after.starts_with("import").then(|| inner[..end].to_owned())
}

impl Plugin for Twig {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error> {
        for kind in [EventKind::FileOpened, EventKind::FileSaved] {
            if let Err(error) = host::subscribe(kind) {
                host::log(LogLevel::Warn, &format!("Twig inspections need the {} event: {error}", kind.name()));
            }
        }

        if let Err(error)=host::subscribe_paths("files-changed", &[EXTENSION_GLOBS,TEMPLATE_GLOBS].concat()) {
            host::log(LogLevel::Warn,&format!("Twig extension cache notifications unavailable: {error}"));
        }
        php_context::refresh_project_lexer();
        self.scan_extensions();

        Ok(())
    }

    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation) {
        let started=std::time::Instant::now();
        let paths=|events:&[Event]|events.iter().flat_map(|event|match event {
            Event::FileSaved(path)=>vec![path.clone()],
            Event::FilesChanged(batch)=>batch.created.iter().chain(&batch.modified).chain(&batch.removed).cloned().collect(),
            _=>Vec::new(),
        }).collect::<Vec<_>>();
        let scanned=|p:&String|p.ends_with(".php")||p.ends_with(".twig");
        let php:Vec<String>=paths(events).into_iter().filter(|p|p.ends_with(".php")).collect();
        if php.len()>MAX_RESCANNED_PHP {php_context::refresh_project_lexer();}
        else {for path in &php {php_context::update_project_lexer(path);}}
        if events.len()>256 {self.requeue(paths(events));return self.drain_pending();}
        self.drain_pending();
        let mut rescanned=0usize;
        for (at,event) in events.iter().enumerate() {
            if started.elapsed()>std::time::Duration::from_millis(100) {self.requeue(paths(&events[at..]));return self.drain_pending();}
            match event {
                Event::FileSaved(path) => {
                    if path.ends_with("composer.json")||path.ends_with("symfony.lock")||path.ends_with("twig.yaml") {forget_project_facts();}
                    if !scanned(path) {continue;}
                    if let Some(project)=project_of(path) { self.scan_file(&project,path); }
                    else {self.forget(path);}
                }
                Event::FilesChanged(batch) => {
                    for path in batch.removed.iter().chain(batch.created.iter().chain(&batch.modified).filter(|p|!scanned(p))) {
                        self.forget(path);
                    }
                    let changed:Vec<String>=batch.created.iter().chain(&batch.modified).filter(|p|scanned(p)).cloned().collect();
                    let room=MAX_RESCANNED_PHP.saturating_sub(rescanned).min(changed.len());
                    for path in &changed[..room] {
                        rescanned+=1;
                        match project_of(path) {
                            Some(project)=>{self.scan_file(&project,path);}
                            None=>{self.forget(path);}
                        }
                    }
                    self.requeue(changed[room..].to_vec());
                }
                _ => {}
            }
        }
    }

    fn run_command(&mut self, id: &str, ctx: &Invocation, _arg: Option<&str>) -> Result<Option<String>, Error> {
        if id=="source-symbol-occurrences" {return if let Some(arg)=_arg{reviewed_symbols::occurrences(&mut self.reviewed_symbols,ctx,arg)}else{reviewed_symbols::occurrences_at_caret(&mut self.reviewed_symbols,ctx)};}
        if id=="toggle-comment" {return comments::toggle(ctx);}
        Err(Error::NotFound(format!("command {id}")))
    }
}

impl SyntaxProvider for Twig {
    fn highlights(&mut self, _language: &str, text: &str) -> Vec<Token> {
        let Some(_syntax)=delimiters::Syntax::scope() else {return Vec::new();};
        if text.len() > MAX_SYNTAX_BYTES {
            return Vec::new();
        }
        let mut tokens = lex(text).tokens;
        tokens.truncate(MAX_SYNTAX_TOKENS);
        tokens
    }

    fn outline(&mut self, _language: &str, text: &str) -> Vec<Symbol> {
        let Some(_syntax)=delimiters::Syntax::scope() else {return Vec::new();};
        structure(text).0
    }

    fn folding(&mut self, _language: &str, text: &str) -> Vec<Fold> {
        let Some(_syntax)=delimiters::Syntax::scope() else {return Vec::new();};
        structure(text).1
    }
}

fn macro_locations(path: &str, text: &str, name: &str) -> Vec<Location> {
    if text.len() > 128 << 10 || !checks::check(text).is_empty() {
        return Vec::new();
    }
    let found: Vec<_> = navigate::macros(text).into_iter().filter(|m| m.name == name).collect();
    if found.len() != 1 {
        return Vec::new();
    }
    let m = &found[0];
    vec![Location { path: path.into(), range: EditorRange::new(position(text, m.name_range.start), position(text, m.name_range.end)) }]
}

impl Providers for Twig {
    fn line_markers(&mut self, doc: &Document) -> Vec<openidea_plugin::providers::LineMarker> {
        self.drain(std::time::Duration::from_millis(10));
        gutter::markers(&self.templates, doc)
    }

    fn completion(&mut self, doc: &Document, at: openidea_plugin::Position, _trigger: Option<&str>) -> Vec<CompletionItem> {
        let Some(_syntax)=delimiters::Syntax::scope() else {return Vec::new();};
        if doc.language != "twig" || !doc.path.ends_with(".twig") || doc.text.len() > 128 << 10 {
            return Vec::new();
        }
        let Some(offset) = offset(&doc.text, at) else {
            return Vec::new();
        };
        let project = project_of(&doc.path);

        let symfony = project.as_ref().is_some_and(is_symfony);
        if let Some(slot) = macro_arguments::slot(&doc.text, offset).filter(|_|macro_arguments::supported(project.as_ref())) {
            let binding = if slot.receiver.as_deref() == Some("_self") {
                Some(("_self".to_owned(),slot.name.clone()))
            } else if !symfony {
                navigate::binding(&doc.text,slot.call,slot.receiver.as_deref(),&slot.name)
            } else {None};
            if let Some((template,name)) = binding {
                let source = if template=="_self" {Some(doc.text.clone())} else {
                    project.as_ref().and_then(|p|macros_of(p,doc,&template)).map(|(_,text)|text)
                };
                if let Some(source) = source {
                    let found:Vec<_>=navigate::macros(&source).into_iter().filter(|m|m.name==name).collect();
                    if found.len()==1 {return macro_arguments::items(&doc.text,&slot,&found[0].arguments);}
                }
            }
        }
        let macros = if symfony { Vec::new() } else { project.as_ref().map(|p| macro_items(p, doc, offset)).unwrap_or_default() };
        if !macros.is_empty() {
            return macros;
        }

        let names = !symfony;
        let root = project.as_ref().and_then(|p| p.root().ok()).unwrap_or_default();
        let extra = complete::Extra {
            filters: self.extra_names(&root, ExtensionKind::Filter),
            functions: self.extra_names(&root, ExtensionKind::Function),
            tests: self.extra_names(&root, ExtensionKind::Test),
            variables: match &project {
                Some(p) if complete::variable_slot(&doc.text, offset) => self.template_variable_names(p, &doc.path),
                _ => Vec::new(),
            },
        };

        complete::items_with(&doc.text, offset, names, &extra)
    }

    /// Quick fixes for the unclosed-tag findings the diagnostics report on the selected lines.
    fn code_actions(&mut self, doc: &Document, selection: EditorRange) -> Vec<CodeAction> {
        let Some(_syntax) = delimiters::Syntax::scope() else { return Vec::new() };
        if doc.language != "twig" || !doc.path.ends_with(".twig") || doc.text.len() > 128 << 10 || !host::setting_bool("diagnostics").unwrap_or(true) {
            return Vec::new();
        }
        let text = doc.text.as_str();
        let parsed = lex(text);
        if parsed.tags.len() > MAX_STRUCTURE_TAGS || parsed.tokens.len() > MAX_SYNTAX_TOKENS {
            return Vec::new();
        }

        checks::check(text)
            .into_iter()
            .filter_map(|finding| {
                let fix = finding.fix?;
                let touches = position(text, finding.range.start).line <= selection.end.line && position(text, finding.range.end).line >= selection.start.line;
                let at = position(text, fix.at);

                touches.then(|| CodeAction {
                    title: fix.title,
                    kind: Some("quickfix".into()),
                    preferred: true,
                    edits: vec![(doc.path.clone(), vec![TextEdit { range: EditorRange::new(at, at), text: fix.text }])],
                    data: None,
                })
            })
            .collect()
    }

    fn definition(&mut self, doc: &Document, at: openidea_plugin::Position) -> Vec<Location> {
        let Some(_syntax)=delimiters::Syntax::scope() else {return Vec::new();};
        if doc.language != "twig" || !doc.path.ends_with(".twig") || doc.text.len() > 128 << 10 {
            return Vec::new();
        }
        let Some(offset) = offset(&doc.text, at) else {
            return Vec::new();
        };
        if let Some(target)=signatures::definition(doc,at) {return vec![target];}
        let symbols=reviewed_symbols::definition(doc,at);if !symbols.is_empty(){return symbols;}
        if let Some((Some(receiver), name, _)) = navigate::dotted_at(&doc.text, offset) {
            if receiver == "_self" && navigate::code_at(&doc.text, offset, false) {
                return macro_locations(&doc.path, &doc.text, &name);
            }
        }
        let Some(project) = project_of(&doc.path) else {
            return Vec::new();
        };
        let range_in = |text: &str, range: Range<usize>| EditorRange::new(position(text, range.start), position(text, range.end));

        if let Some(name) = navigate::block_call_at(&doc.text, offset) {
            if is_symfony(&project) || navigate::block_definitions(&doc.text).iter().any(|(n, _)| *n == name) {
                return Vec::new();
            }
            let Some(used) = navigate::used_templates(&doc.text) else {
                return Vec::new();
            };
            let mut found = Vec::new();
            for template in used {
                let Some((path, text)) = macros_of(&project, doc, &template) else {
                    return Vec::new();
                };
                let defined: Vec<_> = navigate::block_definitions(&text).into_iter().filter(|(n, _)| *n == name).collect();
                if defined.len() > 1 {
                    return Vec::new();
                }
                if let Some((_, range)) = defined.into_iter().next() {
                    found.push(Location { path, range: range_in(&text, range) });
                }
            }
            return if found.len() == 1 { found } else { Vec::new() };
        }

        if let Some((name, _)) = navigate::template_at(&doc.text, offset) {
            if is_symfony(&project) && !name.starts_with('@') {
                return Vec::new();
            }

            return template_path(&project, &name)
                .map(|path| vec![Location { path, range: EditorRange::new(position("", 0), position("", 0)) }])
                .unwrap_or_default();
        }

        let Some((receiver, word, word_range)) = navigate::dotted_at(&doc.text, offset) else {
            return Vec::new();
        };
        if !navigate::code_at(&doc.text, offset, false) {
            return Vec::new();
        }
        let template = if receiver.as_deref() == Some("_self") {
            Some(("_self".into(), word.clone()))
        } else {
            navigate::binding(&doc.text, offset, receiver.as_deref(), &word)
        };

        if let Some((template, name)) = template {
            if template != "_self" && is_symfony(&project) {
                return Vec::new();
            }
            let Some((path, text)) = macros_of(&project, doc, &template) else {
                return Vec::new();
            };
            return macro_locations(&path, &text, &name);
        }

        if receiver.is_none() && navigate::code_at(&doc.text, offset, false) && navigate::plain_variable_at(&doc.text, word_range.clone()) {
            let found: Vec<Location> = self.template_variables(&project, &doc.path).into_iter().filter(|(name, _)| *name == word).map(|(_, at)| at).collect();
            if !found.is_empty() {
                return found;
            }
        }

        if receiver.is_some() || is_symfony(&project) || !navigate::code_at(&doc.text, offset, false) {
            return Vec::new();
        }

        if navigate::imports(&doc.text)
            .iter()
            .any(|import| matches!(import,navigate::Import::Names{names,..} if names.iter().any(|(_,used)|used==&word)))
        {
            return Vec::new();
        }
        let before = doc.text[..offset].trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_');
        let kind = if before.ends_with('|') {
            ExtensionKind::Filter
        } else if before.trim_end().ends_with(" is") || before.trim_end().ends_with(" not") {
            ExtensionKind::Test
        } else {
            ExtensionKind::Function
        };
        if kind == ExtensionKind::Function && !doc.text[word_range.end..].trim_start().starts_with('(') {
            return Vec::new();
        }
        let root = project.root().unwrap_or_default();
        if root.is_empty() {
            return Vec::new();
        }

        if !host::setting_bool("projectExtensions").unwrap_or(true){return Vec::new();}
        self.extensions.iter().filter(|(path,_)|Path::new(path.as_str()).starts_with(&root))
            .flat_map(|(path,found)|found.iter().map(move|e|(path,e)))
            .filter(|(_,e)|e.kind==kind&&e.name==word).take(16)
            .filter_map(|(path,e)|{
                if !same_owner(&root,path)||!extension_source(&root,path)||!project.stat(path).ok().is_some_and(|s|s.kind==openidea_plugin::FileKind::File&&s.size<=128<<10){return None;}
                let php=project.read_file(path).ok().filter(|s|s.len()<=128<<10)?;
                if !navigate::extensions(&php).iter().any(|fresh| fresh.name == e.name && fresh.kind == e.kind && fresh.range == e.range) {
                    return None;
                }
                Some(Location { path: path.clone(), range: range_in(&php, e.range.clone()) })
            })
            .collect()
    }
}

fn project_of(path: &str) -> Option<Project> {
    host::projects()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| Some((p.root().ok()?, p)))
        .filter(|(root, _)| Path::new(path).starts_with(root))
        .max_by_key(|(root, _)| root.len())
        .map(|(_, project)| project)
}

/// The test `aritti.symfony` uses to switch on. Inside such a project that
/// plugin owns tag, filter, function and test completion, so this one stays
/// silent there instead of listing everything twice.
fn is_symfony(project: &Project) -> bool {
    let root = project.root().unwrap_or_default();
    if let Some(known) = project_fact(&root, |f| f.symfony) {
        return known;
    }
    let found = project.read_file("symfony.lock").is_ok()
        || project
            .read_file("composer.json")
            .is_ok_and(|composer| composer.contains("\"symfony/framework-bundle\"") || composer.contains("\"symfony/symfony\""));
    remember_project_fact(&root, |f| f.symfony = Some(found));
    found
}

type ConfigBlocks = (Option<twig_config::Block>, Vec<twig_config::Block>);

#[derive(Clone, Default)]
struct ProjectFacts {
    at: Option<std::time::Instant>,
    symfony: Option<bool>,
    blocks: Option<ConfigBlocks>,
}

thread_local! {static FACTS: std::cell::RefCell<BTreeMap<String, ProjectFacts>> = const { std::cell::RefCell::new(BTreeMap::new()) };}

const FACTS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn project_fact<T>(root: &str, pick: impl Fn(&ProjectFacts) -> Option<T>) -> Option<T> {
    FACTS.with(|f| f.borrow().get(root).filter(|f| f.at.is_some_and(|at| at.elapsed() < FACTS_TTL)).and_then(pick))
}

fn remember_project_fact(root: &str, set: impl FnOnce(&mut ProjectFacts)) {
    FACTS.with(|f| {
        let mut all = f.borrow_mut();
        if all.len() > 16 {
            all.clear();
        }
        let entry = all.entry(root.to_owned()).or_default();
        if !entry.at.is_some_and(|at| at.elapsed() < FACTS_TTL) {
            *entry = ProjectFacts::default();
        }
        entry.at.get_or_insert_with(std::time::Instant::now);
        set(entry);
    });
}

/// Saving composer.json, symfony.lock or a twig.yaml changes what the cached facts say.
fn forget_project_facts() {
    FACTS.with(|f| f.borrow_mut().clear());
}

impl openidea_plugin::diagnostics::DiagnosticsProvider for Twig {
    fn diagnostics(&mut self, doc: &openidea_plugin::providers::Document) -> Vec<Diagnostic> {
        let Some(_syntax)=delimiters::Syntax::scope() else {return vec![Diagnostic::new(EditorRange::new(openidea_plugin::Position::new(0,0),openidea_plugin::Position::new(0,0)),Severity::Error,"Invalid Twig delimiter settings: use 1–16 byte non-whitespace delimiters and distinct opening delimiters without shared prefixes.").code("invalid-delimiter-settings")];};
        if doc.language!="twig" || !doc.path.ends_with(".twig") || doc.text.len()>128<<10 || !host::setting_bool("diagnostics").unwrap_or(true) {return Vec::new();}
        let text=doc.text.as_str();
        let parsed=lex(text);if parsed.tags.len()>MAX_STRUCTURE_TAGS || parsed.tokens.len()>MAX_SYNTAX_TOKENS{return Vec::new();}

        let mut findings = checks::check(text);
        if host::setting_bool("expressionErrors").unwrap_or(false) {
            findings.extend(checks::brackets(text));
        }

        let diagnostics: Vec<Diagnostic> = findings
            .into_iter()
            .take(500)
            .map(|f| Diagnostic {
                code: Some(f.code.into()),
                source: Some(SOURCE.into()),
                ..Diagnostic::new(EditorRange::new(position(text, f.range.start), position(text, f.range.end)), Severity::Error, f.message)
            })
            .collect();

        diagnostics
    }
}

// API 1.3 registers the optional exports on the same component.
openidea_plugin::register_plugin!(Twig, injections, providers, syntax, diagnostics, hover, language_features, file_actions);

const KEYWORDS: &[&str] =
    &["and", "or", "not", "in", "is", "matches", "starts", "ends", "with", "as", "only", "ignore", "missing", "if", "else", "for"];
const CONSTANTS: &[&str] = &["true", "false", "null", "none", "TRUE", "FALSE", "NULL", "NONE"];

/// A `{% name … %}` tag: its name, the identifier after it (a block's or
/// macro's name), and where it is.
#[derive(Debug, PartialEq, Eq)]
struct Tag {
    name: String,
    argument: Option<Range<usize>>,
    range: Range<usize>,
    /// Has an `{% endname %}`: not `{% block title 'Short' %}` or `{% set x = 1 %}`.
    paired: bool,
}

#[derive(Default)]
struct Lexed {
    tokens: Vec<Token>,
    tags: Vec<Tag>,
    comments: Vec<Range<usize>>,
    boundaries: Vec<(Range<usize>,bool)>,
    /// Openers (`{{`, `{%`, `{#`) whose closer is missing.
    unterminated: Vec<Range<usize>>,
}

impl Lexed {
    fn push(&mut self, range: Range<usize>, kind: Highlight) {
        if range.start < range.end {
            self.tokens.push(Token { range, kind });
        }
    }
}

struct Lexer<'a> {
    text: &'a str,
    bytes: &'a [u8],
    out: Lexed,
    syntax: delimiters::Syntax,
}

fn is_identifier_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_identifier(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn lex(text: &str) -> Lexed {
    let mut lexer = Lexer { text, bytes: text.as_bytes(), out: Lexed::default(),syntax:delimiters::Syntax::current() };
    lexer.html(0);
    lexer.out
}

impl Lexer<'_> {
    fn at(&self, i: usize, s: &str) -> bool {
        self.bytes[i..].starts_with(self.syntax.expand(s).as_bytes())
    }

    fn find(&self, from: usize, s: &str) -> Option<usize> {
        self.text[from..].find(self.syntax.expand(s)).map(|i| from + i)
    }

    fn html(&mut self, mut i: usize) {
        while i < self.bytes.len() {
            if self.at(i, "{#") || self.at(i, "{{") || self.at(i, "{%") {
                i = self.twig(i);
                continue;
            }

            if self.at(i, "<!--") {
                let end = self.find(i + 4, "-->").map_or(self.bytes.len(), |e| e + 3);
                self.out.push(i..end, Highlight::Comment);
                i = end;
                continue;
            }

            let opens_element =
                self.bytes[i] == b'<' && self.bytes.get(i + 1).is_some_and(|b| is_identifier_start(*b) || *b == b'/' || *b == b'!');
            if opens_element {
                i = self.element(i);
                continue;
            }

            i += 1;
        }
    }

    /// `<name attr="value" …>`, with Twig anywhere in it.
    fn element(&mut self, start: usize) -> usize {
        let mut i = start + 1;
        while i < self.bytes.len() && (is_identifier(self.bytes[i]) || matches!(self.bytes[i], b'/' | b'!' | b'-' | b':')) {
            i += 1;
        }
        self.out.push(start..i, Highlight::Tag);

        while i < self.bytes.len() {
            let b = self.bytes[i];

            if self.at(i, "{#") || self.at(i, "{{") || self.at(i, "{%") {
                i = self.twig(i);
            } else if b == b'>' {
                self.out.push(i..i + 1, Highlight::Tag);
                return i + 1;
            } else if self.at(i, "/>") {
                self.out.push(i..i + 2, Highlight::Tag);
                return i + 2;
            } else if b == b'"' || b == b'\'' {
                i = self.attribute_value(i);
            } else if b.is_ascii_whitespace() || b == b'=' {
                i += 1;
            } else if b == b'<' {
                return i;
            } else {
                let name_start = i;
                while i < self.bytes.len()
                    && !["{{","{%","{#"].iter().any(|s|self.at(i,s))
                    && !matches!(self.bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b'=' | b'>' | b'/' | b'"' | b'\'' | b'{' | b'<')
                {
                    i += 1;
                }
                i = i.max(name_start + 1);
                self.out.push(name_start..i, Highlight::Attribute);
            }
        }

        i
    }

    fn attribute_value(&mut self, start: usize) -> usize {
        let quote = self.bytes[start];
        let mut segment = start;
        let mut i = start + 1;

        while i < self.bytes.len() {
            if self.bytes[i] == quote {
                self.out.push(segment..i + 1, Highlight::String);
                return i + 1;
            }

            if self.at(i, "{{") || self.at(i, "{%") || self.at(i, "{#") {
                self.out.push(segment..i, Highlight::String);
                i = self.twig(i);
                segment = i;
                continue;
            }

            i += 1;
        }

        self.out.push(segment..i, Highlight::String);
        i
    }

    /// A Twig comment, print or tag starting at `start`; returns its end.
    fn twig(&mut self, start: usize) -> usize {
        if self.at(start, "{#") {
            let closer = self.find(start + self.syntax.values[4].len(), "#}");
            if closer.is_none() {
                self.out.unterminated.push(start..start + self.syntax.values[4].len());
            }
            let end = closer.map_or(self.bytes.len(), |e| e + self.syntax.values[5].len());
            self.out.push(start..end, Highlight::Comment);
            self.out.comments.push(start..end);
            return end;
        }

        let is_tag = self.at(start, "{%");
        let close = if is_tag { "%}" } else { "}}" };
        let open_len=self.syntax.expand(if is_tag {"{%"} else {"{{"}).len();
        let mut i = start + open_len;
        if self.bytes.get(i).is_some_and(|b| *b == b'-' || *b == b'~') {
            i += 1;
        }
        self.out.push(start..i, Highlight::Punctuation);
        self.out.boundaries.push((start..i,true));

        let mut tag_name: Option<String> = None;
        let mut argument = None;
        let mut token_count = 0;
        let mut assigns = false;
        let mut previous = 0u8;
        let mut closed = false;
        let mut groups:Vec<(u8,bool)>=Vec::new();
        let mut groups_known=true;

        while i < self.bytes.len() {
            let b = self.bytes[i];

            let trimmed_close = (b == b'-' || b == b'~') && self.at(i + 1, close);
            if groups.is_empty() && (self.at(i, close) || trimmed_close) {
                let end = i + self.syntax.expand(close).len() + usize::from(trimmed_close);
                self.out.push(i..end, Highlight::Punctuation);
                self.out.boundaries.push((i..end,false));
                i = end;
                closed = true;
                break;
            }

            if b.is_ascii_whitespace() {
                i += 1;
                continue;
            }

            let token_start = i;
            token_count += 1;
            if b == b'"' || b == b'\'' {
                i += 1;
                while i < self.bytes.len() && self.bytes[i] != b {
                    i += if self.bytes[i] == b'\\' { 2 } else { 1 };
                }
                let closed_string = self.bytes.get(i) == Some(&b);
                i = (i + 1).min(self.bytes.len());
                self.out.push(token_start..i, Highlight::String);
                if closed_string
                    && tag_name.as_deref() == Some("embed")
                    && argument.is_none()
                    && self.bytes.get(i.saturating_sub(1)) == Some(&b)
                    && i > token_start + 1
                {
                    argument = Some(token_start + 1..i - 1);
                }
            } else if b.is_ascii_digit() {
                i = self.number_end(i);
                self.out.push(token_start..i, Highlight::Number);
            } else if is_identifier_start(b) || b >= 0x80 {
                while i < self.bytes.len() && (is_identifier(self.bytes[i]) || self.bytes[i] >= 0x80) {
                    i += 1;
                }
                let word = &self.text[token_start..i];
                let next = self.text[i..].trim_start().bytes().next();
                let is_tag_name = is_tag && tag_name.is_none();

                let declaration_name = is_tag && !is_tag_name && argument.is_none();
                if declaration_name {
                    argument = Some(token_start..i);
                }

                let last=self.out.tokens.last().map(|t|(&self.text[t.range.clone()],t.kind==Highlight::Keyword));
                let prior=self.out.tokens.iter().rev().nth(1).map(|t|(&self.text[t.range.clone()],t.kind==Highlight::Keyword));
                let test_position=last==Some(("is",true)) || last==Some(("not",true))&&prior==Some(("is",true));
                let test_name=test_position&&["constant","defined","divisible","empty","even","iterable","none","null","odd","same"].contains(&word);
                let test_by=word=="by"&&self.out.tokens.last().is_some_and(|t|t.kind==Highlight::Function&&&self.text[t.range.clone()]=="divisible");
                let tail=self.text[i..].trim_start();
                let named_argument=groups_known&&groups.last().is_some_and(|(open,call)|*open==b'('&&*call)
                    &&last.is_some_and(|(raw,_)|matches!(raw,"("|","))
                    &&(tail.starts_with(':')&&!tail.starts_with("::")||tail.starts_with('=')&&!tail.starts_with("==")&&!tail.starts_with("=>"));
                let kind = if is_tag_name {
                    tag_name = Some(word.to_owned());
                    Highlight::Keyword
                } else if declaration_name && matches!(tag_name.as_deref(),Some("macro"|"apply")) {
                    Highlight::Function
                } else if declaration_name && matches!(tag_name.as_deref(), Some("block" | "endblock")) {
                    Highlight::Property
                } else if previous == b'|' {
                    Highlight::Function
                } else if previous == b'.' {
                    if next == Some(b'(') { Highlight::FunctionCall } else { Highlight::Property }
                } else if test_name {
                    Highlight::Function
                } else if test_by {
                    Highlight::Keyword
                } else if named_argument {
                    Highlight::Property
                } else if KEYWORDS.contains(&word) {
                    Highlight::Keyword
                } else if CONSTANTS.contains(&word) {
                    Highlight::Constant
                } else if next == Some(b'(') {
                    Highlight::FunctionCall
                } else if word == "_self" || word == "_context" || word == "loop" {
                    Highlight::SelfVariable
                } else {
                    Highlight::Variable
                };

                self.out.push(token_start..i, kind);
            } else {
                i += if b == b'.' && self.bytes.get(i + 1) == Some(&b'.') { 2 } else { 1 };
                assigns |= b == b'=' && !matches!(self.bytes.get(i), Some(b'=')) && !matches!(previous, b'=' | b'!' | b'<' | b'>');
                match b {
                    b'('|b'['|b'{'=>{
                        let compound_test=self.out.tokens.last().is_some_and(|t|t.kind==Highlight::Keyword&&matches!(&self.text[t.range.clone()],"as"|"by"))
                            &&self.out.tokens.iter().rev().nth(1).is_some_and(|t|t.kind==Highlight::Function&&matches!(&self.text[t.range.clone()],"same"|"divisible"));
                        let callable=b==b'(' && !(is_tag&&tag_name.as_deref()==Some("macro"))
                            &&(compound_test||self.out.tokens.last().is_some_and(|t|matches!(t.kind,Highlight::Function|Highlight::FunctionCall)));
                        if groups.len()<128 {groups.push((b,callable));}else{groups_known=false;}
                    },
                    b')'|b']'|b'}'=>{let expected=match b{b')'=>b'(',b']'=>b'[',_=>b'{'};if groups.pop().is_none_or(|(open,_)|open!=expected){groups_known=false;}},
                    _=>{}
                }
                let kind = if b"()[]{},".contains(&b) { Highlight::Punctuation } else { Highlight::Operator };
                self.out.push(token_start..i, kind);
            }

            previous = if b == b'.' && i - token_start == 2 { 0 } else { b };
        }

        if !closed {
            self.out.unterminated.push(start..start + open_len);
        }

        if let Some(name) = tag_name {
            let raw = name == "verbatim" || name == "raw";
            let paired = match name.as_str() {
                "block" => token_count <= 2,
                "set" => !assigns,
                _ => true,
            };
            self.out.tags.push(Tag { name: name.clone(), argument, range: start..i, paired });

            if raw {
                return self.find_tag(i, &format!("end{name}")).unwrap_or(self.bytes.len());
            }
        }

        i
    }

    /// Twig 3 numeric grammar: grouped digits, optional decimal/exponent.
    /// A range's two dots are operators, never part of the preceding number.
    fn number_end(&self, from: usize) -> usize {
        let digits = |mut at: usize| {
            while at < self.bytes.len() {
                if self.bytes[at].is_ascii_digit() {
                    at += 1;
                } else if self.bytes[at] == b'_' && self.bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
                    at += 2;
                } else {
                    break;
                }
            }
            at
        };
        let mut end = digits(from);
        if self.bytes.get(end) == Some(&b'.') && self.bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
            end = digits(end + 1);
        }
        if matches!(self.bytes.get(end), Some(b'e' | b'E')) {
            let mut exponent = end + 1;
            if matches!(self.bytes.get(exponent), Some(b'+' | b'-')) {
                exponent += 1;
            }
            if self.bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
                end = digits(exponent);
            }
        }
        end
    }

    /// Start of the next `{% name` tag after `from`.
    fn find_tag(&self, from: usize, name: &str) -> Option<usize> {
        let mut at = from;
        while let Some(open) = self.find(at, "{%") {
            let inside = self.text[open + self.syntax.values[0].len()..].trim_start_matches(['-', '~']).trim_start();
            if let Some(rest) = inside.strip_prefix(name) {
                let rest = rest.trim_start();
                if rest.trim_start_matches(['-','~']).starts_with(&self.syntax.values[1]) {
                    return Some(open);
                }
            }
            at = open + self.syntax.values[0].len();
        }
        None
    }
}

/// Structure-view symbols (blocks, macros) and folds (tag pairs, comments).
fn structure(text: &str) -> (Vec<Symbol>, Vec<Fold>) {
    if text.len() > MAX_SYNTAX_BYTES {
        return (Vec::new(), Vec::new());
    }
    let lexed = lex(text);
    if lexed.tags.len() > MAX_STRUCTURE_TAGS {
        return (Vec::new(), Vec::new());
    }
    let line_starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1)).collect();
    let line_at = |at: usize| (line_starts.partition_point(|start| *start <= at) - 1) as u32;
    let mut symbols: Vec<Symbol> = Vec::new();
    let mut folds = Vec::new();
    // (tag name, tag start, symbol index)
    let mut open: Vec<(&str, usize, Option<usize>)> = Vec::new();

    for tag in &lexed.tags {
        if let Some(name) = tag.name.strip_prefix("end") {
            let Some(at) = open.iter().rposition(|(n, _, _)| *n == name) else {
                continue;
            };
            let (_, start, symbol) = open[at];
            open.truncate(at);

            let (start_line, end_line) = (line_at(start), line_at(tag.range.start));
            if end_line > start_line {
                folds.push(Fold { start_line, end_line, kind: FoldKind::Region });
            }
            if let Some(symbol) = symbol {
                symbols[symbol].range.end = tag.range.end;
            }
            continue;
        }

        let kind = match tag.name.as_str() {
            "block" => Some(SymbolKind::Field),
            "macro" => Some(SymbolKind::Function),
            "embed" => Some(SymbolKind::Module),
            _ => None,
        };
        let symbol = kind.zip(tag.argument.clone()).map(|(kind, name_range)| {
            let parent = open.iter().rev().find_map(|(_, _, s)| *s);
            symbols.push(Symbol {
                name: text[name_range.clone()].to_owned(),
                detail: None,
                kind,
                range: tag.range.clone(),
                name_range,
                parent,
            });
            symbols.len() - 1
        });
        if tag.paired
            && [
                "apply",
                "autoescape",
                "block",
                "cache",
                "embed",
                "filter",
                "for",
                "if",
                "macro",
                "raw",
                "sandbox",
                "set",
                "spaceless",
                "verbatim",
                "with",
            ]
            .contains(&tag.name.as_str())
        {
            open.push((tag.name.as_str(), tag.range.start, symbol));
        }
    }

    for comment in &lexed.comments {
        let (start_line, end_line) = (line_at(comment.start), line_at(comment.end));
        if end_line > start_line {
            folds.push(Fold { start_line, end_line, kind: FoldKind::Comment });
        }
    }

    // Prefix truncation keeps every retained symbol's parent index valid.
    symbols.truncate(MAX_STRUCTURE_ITEMS);
    folds.truncate(MAX_STRUCTURE_ITEMS);
    (symbols, folds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, &'static str)> {
        lex(text).tokens.iter().map(|t| (&text[t.range.clone()], t.kind.name())).collect()
    }

    #[test]
    fn highlights_twig_in_html() {
        let text = r#"<a href="{{ path('home') }}" class="x">{# note #}{{ user.name|upper }}</a>"#;
        assert_eq!(
            kinds(text),
            [
                ("<a", "tag"),
                ("href", "attribute"),
                ("\"", "string"),
                ("{{", "punctuation"),
                ("path", "function-call"),
                ("(", "punctuation"),
                ("'home'", "string"),
                (")", "punctuation"),
                ("}}", "punctuation"),
                ("\"", "string"),
                ("class", "attribute"),
                ("\"x\"", "string"),
                (">", "tag"),
                ("{# note #}", "comment"),
                ("{{", "punctuation"),
                ("user", "variable"),
                (".", "operator"),
                ("name", "property"),
                ("|", "operator"),
                ("upper", "function"),
                ("}}", "punctuation"),
                ("</a", "tag"),
                (">", "tag"),
            ]
        );
    }

    #[test]
    fn highlights_tags() {
        assert_eq!(
            kinds("{%- if items is not empty and true -%}"),
            [
                ("{%-", "punctuation"),
                ("if", "keyword"),
                ("items", "variable"),
                ("is", "keyword"),
                ("not", "keyword"),
                ("empty", "function"),
                ("and", "keyword"),
                ("true", "constant"),
                ("-%}", "punctuation"),
            ]
        );
    }

    #[test]
    fn skips_verbatim_contents() {
        let text = "{% verbatim %}{{ not twig }}{% endverbatim %}";
        let names: Vec<_> = kinds(text).into_iter().map(|(t, _)| t).collect();
        assert_eq!(names, ["{%", "verbatim", "%}", "{%", "endverbatim", "%}"]);
    }

    #[test]
    fn classifies_test_operators_apply_filters_and_named_call_arguments_by_local_shape(){
        let tokens=kinds("{{ value is defined }}{{ value is not empty }}{{ n is divisible by(3) }}{{ a is same as(b) }}{{ value is null }}{{ empty }}{{ null }}{% apply upper %}x{% endapply %}{{ range(low:1, high=3) }}");
        for token in [("defined","function"),("empty","function"),("divisible","function"),("by","keyword"),("same","function"),("null","function"),("upper","function"),("low","property"),("high","property"),("empty","variable"),("null","constant")]{assert!(tokens.contains(&token),"{token:?}: {tokens:?}");}
    }
    #[test]
    fn named_argument_roles_do_not_leak_into_hash_keys_grouped_comparisons_or_macro_parameters(){
        let source="{{ f(key: 1, data: {inside: 2}, test: (value == 3)) }}{% macro badge(label='x') %}{{ label }}{% endmacro %}{{ a is same as(expected: b) }}";
        let tokens=kinds(source);
        for token in [("key","property"),("data","property"),("test","property"),("inside","variable"),("value","variable"),("label","variable"),("expected","property")]{assert!(tokens.contains(&token),"{token:?}: {tokens:?}");}
        assert!(!tokens.contains(&("inside","property"))&&!tokens.contains(&("label","property"))&&!tokens.contains(&("value","property")));
    }
    #[test]
    fn added_roles_keep_every_unicode_prefix_raw_fold_and_outline_range_valid(){
        let source="😀\r\n{% block 名称 %}{{ value is not empty }}{{ f(arg: 2, other=(3)) }}\r\n{% endblock %}{% verbatim %}{{ f(hidden: 1) }}{% endverbatim %}";
        for end in source.char_indices().map(|(i,_)|i).chain(std::iter::once(source.len())){
            let text=&source[..end];for t in lex(text).tokens{assert!(t.range.end<=end&&text.is_char_boundary(t.range.start)&&text.is_char_boundary(t.range.end));}
        }
        let (outline,folds)=structure(source);assert!(outline.iter().any(|s|s.name=="名称"));assert!(!folds.is_empty());
        let tokens=kinds(source);assert!(!tokens.contains(&("hidden","property")));
    }
    #[test]
    fn classifies_numbers_members_and_named_declarations_by_verified_source_shape() {
        let tokens = kinds(
            "{{ 1..3 1.25 1_000.25e-2 2E+3 1__2 7e+ user.method() user.field }}{% macro input(name) %}{% endmacro %}{% block title %}{% endblock title %}",
        );
        for token in [
            ("1", "number"),
            ("..", "operator"),
            ("3", "number"),
            ("1.25", "number"),
            ("1_000.25e-2", "number"),
            ("2E+3", "number"),
            ("method", "function-call"),
            ("field", "property"),
            ("input", "function"),
            ("title", "property"),
        ] {
            assert!(tokens.contains(&token), "{token:?}");
        }
        assert!(!tokens.iter().any(|(value, _)| *value == "1..3" || *value == "1__2" || *value == "7e+"));
    }

    #[test]
    fn unicode_identifiers_and_every_valid_prefix_keep_exact_token_and_symbol_boundaries() {
        let source =
            "😀<p>{{ naïve.名()|upper }} {{ 😀name }}</p>\r\n{% block 名称 %}\n{% endblock %}\n{% macro café(x) %}{{ x }}{% endmacro %}";
        for end in source.char_indices().map(|(at, _)| at).chain(std::iter::once(source.len())) {
            let text = &source[..end];
            let tokens = lex(text).tokens;
            for token in &tokens {
                assert!(token.range.start < token.range.end && token.range.end <= text.len());
                assert!(text.is_char_boundary(token.range.start) && text.is_char_boundary(token.range.end));
                assert_eq!(openidea_plugin::text::offset(text, position(text, token.range.start)), Some(token.range.start));
                assert_eq!(openidea_plugin::text::offset(text, position(text, token.range.end)), Some(token.range.end));
            }
            assert!(tokens.windows(2).all(|pair| pair[0].range.end <= pair[1].range.start));
            let (symbols, _) = structure(text);
            for (index, symbol) in symbols.iter().enumerate() {
                assert_eq!(&text[symbol.name_range.clone()], symbol.name);
                assert!(
                    symbol.range.start <= symbol.name_range.start
                        && symbol.name_range.end <= symbol.range.end
                        && symbol.range.end <= text.len()
                );
                assert!(symbol.parent.is_none_or(|parent| parent < index));
            }
        }
        let tokens = kinds(source);
        assert!(tokens.contains(&("名", "function-call")));
        assert!(tokens.contains(&("😀name", "variable")));
    }

    #[test]
    fn raw_closers_must_be_exact_and_unclosed_raw_contents_remain_suppressed() {
        for opener in ["raw", "verbatim"] {
            let source = format!(
                "{{% {opener} %}}\n{{% end{opener}Extra %}}{{{{ bad }}}}\n{{% end{opener}π %}}{{{{ also_bad }}}}\n{{% end{opener} argument %}}\n{{% end{opener} %}}\n{{{{ good }}}}"
            );
            let tokens = kinds(&source);
            assert!(!tokens.iter().any(|(name, _)| matches!(*name, "bad" | "also_bad" | "argument")));
            assert!(tokens.contains(&("good", "variable")));
            let unclosed = format!("{{% {opener} %}} {{{{ hidden }}}} {{% if hidden %}}");
            assert!(!kinds(&unclosed).iter().any(|(name, _)| *name == "hidden"));
        }
    }

    #[test]
    fn original_embed_outline_and_known_block_folds_exclude_branches_and_unknown_tags() {
        let source = "{% embed 'panel.twig' %}\r\n{% block title %}\r\nA\r\n{% endblock %}\r\n{% endembed %}\r\n{% if yes %}\nA\n{% else %}\nB\n{% endif %}\n{% mystery %}\nX\n{% endmystery %}";
        let (symbols, folds) = structure(source);
        assert_eq!(
            symbols.iter().map(|symbol| (symbol.name.as_str(), symbol.kind, symbol.parent)).collect::<Vec<_>>(),
            [("panel.twig", SymbolKind::Module, None), ("title", SymbolKind::Field, Some(0))]
        );
        assert_eq!(folds.iter().map(|fold| (fold.start_line, fold.end_line)).collect::<Vec<_>>(), [(1, 3), (0, 4), (5, 9)]);
        assert_eq!(&source[symbols[0].name_range.clone()], "panel.twig");
        assert!(structure("{% embed 'unterminated\\'").0.is_empty());
    }

    #[test]
    fn syntax_export_limits_are_explicit_and_do_not_leak_partial_structure_parents() {
        let _host=openidea_plugin::mock::MockHost::install();
        let mut plugin = Twig::default();
        let huge = "x".repeat(MAX_SYNTAX_BYTES + 1);
        assert!(plugin.highlights("twig", &huge).is_empty());
        assert!(plugin.folding("twig", &huge).is_empty());
        assert!(plugin.outline("twig", &huge).is_empty());
        let dense = "{{ x + y }}".repeat(4000);
        assert_eq!(plugin.highlights("twig", &dense).len(), MAX_SYNTAX_TOKENS);
        let tags = "{% block a %}{% endblock %}".repeat(MAX_STRUCTURE_TAGS / 2 + 1);
        assert!(structure(&tags).0.is_empty());
        assert!(structure(&tags).1.is_empty());
        let source = "{# {% block fake %}\n{% endblock %} #}<!-- {% macro fake() %} -->";
        assert!(structure(source).0.is_empty());
    }

    #[test]
    fn outlines_and_folds_blocks() {
        let text = "{% extends 'base.html.twig' %}\n{% block body %}\n  {% set x = 1 %}\n  {% block inner %}\n  {% endblock %}\n  {% for i in 1..3 %}\n  {% endfor %}\n  {% block title 'Short' %}\n{% endblock %}\n{#\n long\n#}\n{% macro field(name) %}\n{% endmacro %}\n";
        let (symbols, folds) = structure(text);

        let outline: Vec<_> = symbols.iter().map(|s| (s.name.as_str(), s.kind, s.parent)).collect();
        assert_eq!(
            outline,
            [
                ("body", SymbolKind::Field, None),
                ("inner", SymbolKind::Field, Some(0)),
                ("title", SymbolKind::Field, Some(0)),
                ("field", SymbolKind::Function, None),
            ]
        );
        assert!(text[symbols[0].range.clone()].starts_with("{% block body %}"));
        assert!(text[symbols[0].range.clone()].ends_with("{% endblock %}"));

        let lines: Vec<_> = folds.iter().map(|f| (f.start_line, f.end_line, f.kind)).collect();
        assert_eq!(
            lines,
            [
                (3, 4, FoldKind::Region),
                (5, 6, FoldKind::Region),
                (1, 8, FoldKind::Region),
                (12, 13, FoldKind::Region),
                (9, 11, FoldKind::Comment),
            ]
        );
    }

    fn project(files: &[(&str, &str)]) -> openidea_plugin::mock::MockHost {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_project_root(Some("/p"));
        forget_project_facts();
        for (path, text) in files {
            host.add_file(path, text);
        }
        host
    }

    fn open(host: &openidea_plugin::mock::MockHost, path: &str) -> Vec<(Option<String>, Option<String>)> {
        Twig::default().handle_events(&[Event::FileOpened(path.into())], &host.invocation());
        let project = host::project_of(path).unwrap();
        let text = project.read_file(path).unwrap();
        host.open_editor(path, "twig", &text, &[]);
        host.refresh_diagnostics(&mut Twig::default());
        host.provider_diagnostics().get(path).into_iter().flatten().map(|d| (d.code.clone(), d.source.clone())).collect()
    }

    #[test]
    fn publishes_findings_for_a_twig_file() {
        let host = project(&[("/p/a.html.twig", "{% if a %}\n{{ x ")]);
        let found = open(&host, "/p/a.html.twig");
        assert!(found.contains(&(Some("unclosed-tag".into()), Some("Twig".into()))));
        assert!(found.iter().any(|(code, _)| code.as_deref() == Some("unterminated-delimiter")));
    }

    #[test]
    fn live_diagnostics_are_bounded_language_scoped_and_replace_fixed_current_text(){
        use openidea_plugin::diagnostics::DiagnosticsProvider;
        let h=openidea_plugin::mock::MockHost::install();let mut twig=Twig::default();
        let mut doc=Document{path:"/project/page.twig".into(),language:"twig".into(),version:4,text:"{% endif %}".repeat(600)};
        let found=twig.diagnostics(&doc);assert_eq!(found.len(),500);
        doc.version+=1;doc.text="😀\r\n{% if ready %}ok{% endif %}".into();assert!(twig.diagnostics(&doc).is_empty());
        doc.text="{% if ready %}".into();doc.language="html".into();assert!(twig.diagnostics(&doc).is_empty());
        doc.language="twig".into();doc.text.push_str(&" ".repeat(128<<10));assert!(twig.diagnostics(&doc).is_empty());
        assert!(h.process_runs().is_empty());assert_eq!(h.fetches(),0);
    }
    #[test]
    fn unclosed_tag_offers_an_insert_end_tag_quick_fix_on_its_line_only() {
        let h = openidea_plugin::mock::MockHost::install();
        let doc = Document { path: "/project/page.twig".into(), language: "twig".into(), version: 3, text: "😀 {% endfor %}\n<div>\n  {% if a %}\n{{ (x }}\n</div>\n".into() };
        let line = |n: u32| EditorRange::new(openidea_plugin::Position::new(n, 0), openidea_plugin::Position::new(n, 0));
        let actions = Twig::default().code_actions(&doc, line(2));
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Insert {% endif %} at end of file");
        let (path, edits) = &actions[0].edits[0];
        assert_eq!(path, "/project/page.twig");
        assert_eq!((edits[0].range.start, edits[0].text.as_str()), (openidea_plugin::Position::new(5, 0), "  {% endif %}\n"));
        h.set_setting("expressionErrors", "true");
        for n in [0, 1, 3] {
            assert!(Twig::default().code_actions(&doc, line(n)).is_empty());
        }
        h.set_setting("tagOpen", r#""<%""#);
        h.set_setting("tagClose", r#""%>""#);
        let custom = Document { text: "<% if a %>\nx\n".into(), ..doc.clone() };
        assert_eq!(Twig::default().code_actions(&custom, line(0))[0].edits[0].1[0].text, "<% endif %>\n");
        h.set_setting("diagnostics", "false");
        assert!(Twig::default().code_actions(&custom, line(0)).is_empty());
    }

    #[test]
    fn diagnostics_can_be_switched_off() {
        let host = project(&[("/p/a.html.twig", "{% if a %}")]);
        host.set_setting("diagnostics", "false");
        assert!(open(&host, "/p/a.html.twig").is_empty());
    }

    #[test]
    fn completes_names_outside_symfony_but_not_inside() {
        let doc = |path: &str| Document { path: path.into(), language: "twig".into(), version: 1, text: "{% i".into() };
        let at = openidea_plugin::Position::new(0, 4);

        let _plain = project(&[("composer.json", "{}")]);
        assert!(Twig::default().completion(&doc("/p/a.twig"), at, None).iter().any(|i| i.label == "endif"));

        let _symfony = project(&[("composer.json", r#"{"require": {"symfony/framework-bundle": "^7"}}"#)]);
        assert!(Twig::default().completion(&doc("/p/a.twig"), at, None).is_empty());
    }
}

#[cfg(test)]
mod project_tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    const FORMS: &str = "{% macro input(name, value = '') %}<input>{% endmacro %}\n{% macro button(label) %}<b>{% endmacro %}\n";

    fn host() -> MockHost {
        let host = MockHost::install();
        host.add_file("composer.json", r#"{"require": {"twig/twig": "^3"}}"#);
        host.add_file("templates/forms.html.twig", FORMS);
        host.add_file("src/Ext.php", "<?php\nclass Ext { function f() { return [new TwigFilter('money', fn() => 1), new TwigFunction('asset_url', fn() => 1)]; } }\n");
        host
    }

    fn doc(text: &str) -> Document {
        Document { path: "/project/templates/page.html.twig".into(), language: "twig".into(), version: 1, text: text.into() }
    }

    fn at(marked: &str) -> (Document, openidea_plugin::Position) {
        let offset = marked.find('^').unwrap();
        let text = marked.replacen('^', "", 1);
        let position = position(&text, offset);
        (doc(&text), position)
    }

    fn labels(items: Vec<CompletionItem>) -> Vec<String> {
        items.into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn nested_project_targets_and_disabled_or_projectless_extension_cache_never_supply_outer_owners(){
        let h=host();let mut twig=Twig::default();twig.activate(&h.context("aritti.twig")).unwrap();
        let(mut template,at_template)=at("{% include 'forms.ht^ml.twig' %}");template.path="/project/page.twig".into();assert_eq!(twig.definition(&template,at_template).len(),1);
        h.add_project("/project/templates");assert!(twig.definition(&template,at_template).is_empty());
        let(mut imported,at_import)=at("{% import 'forms.html.twig' as forms %}{{ forms.in^put() }}");imported.path="/project/page.twig".into();assert!(twig.definition(&imported,at_import).is_empty());
        let(mut d,p)=at("{{ value|mon^ey }}");d.path="/project/page.twig".into();assert_eq!(twig.definition(&d,p).len(),1);
        h.set_setting("projectExtensions","false");assert!(twig.definition(&d,p).is_empty());
        let(mut d,p)=at("{{ value|mo^ }}");d.path="/project/page.twig".into();assert!(!labels(twig.completion(&d,p,None)).contains(&"money".into()));
        h.set_setting("projectExtensions","true");h.set_project_root(None);assert!(!labels(twig.completion(&d,p,None)).contains(&"money".into()));
    }
    #[test]
    fn independently_opened_php_project_and_oversized_fresh_target_cannot_supply_outer_references(){
        let h=host();let mut twig=Twig::default();twig.activate(&h.context("aritti.twig")).unwrap();let(d,p)=at("{{ value|mon^ey }}");
        h.add_project("/project/src");assert!(twig.definition(&d,p).is_empty());let(d,p)=at("{{ value|mo^ }}");assert!(!labels(twig.completion(&d,p,None)).contains(&"money".into()));
        let h=host();let mut twig=Twig::default();twig.activate(&h.context("aritti.twig")).unwrap();let(d,p)=at("{{ value|mon^ey }}");let php=h.invocation().project().unwrap().read_file("src/Ext.php").unwrap();h.add_file("src/Ext.php",&format!("{php}{}"," ".repeat(128<<10)));assert!(twig.definition(&d,p).is_empty());
    }
    #[test]
    fn saved_vendor_php_does_not_expand_the_documented_source_ownership(){
        let h=host();h.add_file("vendor/Ext.php","<?php new TwigFilter('vendor_only', fn()=>1);");let mut twig=Twig::default();twig.activate(&h.context("aritti.twig")).unwrap();
        twig.handle_events(&[Event::FileSaved("/project/vendor/Ext.php".into())],&h.invocation());
        let(d,p)=at("{{ value|vendor_on^ly }}");assert!(twig.definition(&d,p).is_empty());let(d,p)=at("{{ value|vendor_^ }}");assert!(!labels(twig.completion(&d,p,None)).contains(&"vendor_only".into()));
    }
    #[test]
    fn completes_macros_of_an_imported_template() {
        let host = host();
        let mut twig = Twig::default();
        twig.activate(&host.context("aritti.twig")).unwrap();

        let (d, p) = at("{% import 'forms.html.twig' as forms %}{{ forms.^ }}");
        assert_eq!(labels(twig.completion(&d, p, None)), ["input", "button"]);
        let (d, p) = at("{% import 'forms.html.twig' as forms %}{{ forms.bu^ }}");
        assert_eq!(labels(twig.completion(&d, p, None)), ["input", "button"]);
        let (d, p) = at("{% from 'forms.html.twig' import ^ %}");
        assert_eq!(labels(twig.completion(&d, p, None)), ["input", "button"]);
        let (d, p) = at("{% macro local(a) %}{% endmacro %}{% import _self as me %}{{ me.^ }}");
        assert_eq!(labels(twig.completion(&d, p, None)), ["local"]);
        let (d, p) = at("{{ other.^ }}");
        assert!(labels(twig.completion(&d, p, None)).is_empty());
    }

    fn changed(created: &[&str], modified: &[&str], removed: &[&str]) -> Event {
        let own = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        Event::FilesChanged(openidea_plugin::FilesChanged { created: own(created), modified: own(modified), removed: own(removed) })
    }

    #[test]
    fn files_changed_rescans_php_extension() {
        let host=host();let mut twig=Twig::default();twig.activate(&host.context("aritti.twig")).unwrap();
        let(d,p)=at("{{ value|mo^ }}");
        assert!(labels(twig.completion(&d,p,None)).contains(&"money".into()));
        host.add_file("src/Ext.php","<?php\nclass Ext { function f() { return new TwigFilter('euro', fn() => 1); } }\n");
        twig.handle_events(&[changed(&[],&["/project/src/Ext.php"],&[])],&host.invocation());
        let found=labels(twig.completion(&d,p,None));
        assert!(!found.contains(&"money".into()));
        let(d,p)=at("{{ value|eu^ }}");
        assert!(labels(twig.completion(&d,p,None)).contains(&"euro".into()));
        host.add_file("src/New.php","<?php\nclass N { function f() { return new TwigFilter('newone', fn() => 1); } }\n");
        twig.handle_events(&[changed(&["/project/src/New.php"],&[],&[])],&host.invocation());
        let(d,p)=at("{{ value|newo^ }}");
        assert!(labels(twig.completion(&d,p,None)).contains(&"newone".into()));
        twig.handle_events(&[changed(&[],&[],&["/project/src/Ext.php"])],&host.invocation());
        let(d,p)=at("{{ value|eu^ }}");
        assert!(!labels(twig.completion(&d,p,None)).contains(&"euro".into()));
    }

    #[test]
    fn save_all_of_many_php_files_keeps_extensions_and_renders() {
        let host=host();let mut twig=Twig::default();twig.activate(&host.context("aritti.twig")).unwrap();
        let ext=host.invocation().project().unwrap().read_file("src/Ext.php").unwrap();
        let mut saved=vec![Event::FileSaved("/project/src/Ext.php".into())];
        for n in 0..9 {
            host.add_file(&format!("src/C{n}.php"),&format!("<?php\nclass C{n} {{ function f() {{ return $this->render('page{n}.html.twig', ['v{n}' => 1]); }} }}\n"));
            saved.push(Event::FileSaved(format!("/project/src/C{n}.php")));
        }
        twig.handle_events(&saved,&host.invocation());
        let(d,p)=at("{{ value|mo^ }}");
        assert!(labels(twig.completion(&d,p,None)).contains(&"money".into()),"{ext}");
        assert_eq!(twig.renders.len(),9);
        let flood:Vec<Event>=(0..300).map(|_|Event::FileSaved("/project/src/C0.php".into())).chain(saved).collect();
        twig.handle_events(&flood,&host.invocation());
        assert!(labels(twig.completion(&d,p,None)).contains(&"money".into()),"an event flood requeues instead of clearing");
        assert_eq!(twig.renders.len(),9);
    }

    #[test]
    fn symfony_project_skips_extension_scan() {
        let host=host();
        host.add_file("composer.json",r#"{"require": {"symfony/framework-bundle": "^7"}}"#);
        let mut twig=Twig::default();twig.activate(&host.context("aritti.twig")).unwrap();
        assert!(twig.extensions.is_empty());
    }

    #[test]
    fn completion_in_filter_position_skips_template_variables() {
        let host=host();
        host.add_file("composer.json",r#"{"require": {"symfony/framework-bundle": "^7"}}"#);
        host.add_file("templates/page.html.twig","{{ title }}");
        host.add_file("src/A.php","<?php class A { function a() { return $this->render('page.html.twig', ['title' => 1]); } }");
        let mut twig=Twig::default();twig.activate(&host.context("aritti.twig")).unwrap();
        let(d,p)=at("{{ x|ti^ }}");
        assert!(labels(twig.completion(&d,p,None)).is_empty());
        let(d,p)=at("{{ ti^ }}");
        assert_eq!(labels(twig.completion(&d,p,None)),["title"]);
        host.add_file("src/A.php","<?php class A { function a() { return $this->render('page.html.twig', ['other' => 1]); } }");
        assert_eq!(labels(twig.completion(&d,p,None)),["title"]);
    }

    #[test]
    fn dynamic_template_expressions_do_not_borrow_a_literal_prefix_target() {
        let _host=host();
        let mut twig=Twig::default();
        for source in ["{% include 'forms.ht^ml.twig' ~ suffix %}","{% extends 'forms.ht^ml.twig'|lower %}","{{ include('forms.ht^ml.twig' ~ suffix) }}","{{ source('forms.ht^ml.twig' ?? fallback) }}"] {
            let(d,p)=at(source);
            assert!(twig.definition(&d,p).is_empty(),"{source}");
        }
        for source in ["{% include 'forms.ht^ml.twig' with {'a':1} only %}","{{ include('forms.ht^ml.twig', {'a':1}) }}"] {
            let(d,p)=at(source);assert_eq!(twig.definition(&d,p).len(),1,"{source}");
        }
    }

    #[test]
    fn later_imports_do_not_supply_earlier_macro_owners() {
        let _host=host();
        let mut twig=Twig::default();
        for source in ["{{ forms.in^put() }}{% import 'forms.html.twig' as forms %}", "{{ fi^eld() }}{% from 'forms.html.twig' import input as field %}"] {
            let(d,p)=at(source);
            assert!(twig.definition(&d,p).is_empty(),"{source}");
        }
        let(d,p)=at("{{ forms.^ }}{% import 'forms.html.twig' as forms %}");
        assert!(twig.completion(&d,p,None).is_empty());
    }

    #[test]
    fn macro_completion_withholds_duplicate_or_malformed_target_ownership() {
        let host=host();
        let mut twig=Twig::default();
        host.add_file("templates/ambiguous.twig","{% macro repeated(a) %}{% endmacro %}{% macro repeated(b) %}{% endmacro %}{% macro unique() %}{% endmacro %}");
        let(d,p)=at("{% import 'ambiguous.twig' as m %}{{ m.^ }}");
        assert_eq!(labels(twig.completion(&d,p,None)),["unique"]);
        let(d,p)=at("{% import 'ambiguous.twig' as m %}{{ m.re^peated() }}");
        assert!(twig.definition(&d,p).is_empty());
        host.add_file("templates/broken.twig","{% macro broken(a) %}");
        let(d,p)=at("{% import 'broken.twig' as m %}{{ m.^ }}");
        assert!(twig.completion(&d,p,None).is_empty());
    }

    #[test]
    fn literal_self_template_import_uses_unsaved_current_macro_source_and_utf16_range() {
        let _host=host();
        let mut twig=Twig::default();
        let (mut d,p)=at("😀{% macro fresh(a) %}{% endmacro %}{% import 'forms.html.twig' as forms %}{{ forms.fr^esh() }}");
        d.path="/project/templates/forms.html.twig".into();
        let found=twig.definition(&d,p);
        assert_eq!(found.len(),1);
        assert_eq!(found[0].path,d.path);
        assert_eq!(found[0].range.start.character,11);
        let (mut d,p)=at("{% macro fresh(a) %}{% endmacro %}{% import 'forms.html.twig' as forms %}{{ forms.^ }}");
        d.path="/project/templates/forms.html.twig".into();
        assert_eq!(labels(twig.completion(&d,p,None)),["fresh"]);
    }

    #[test]
    fn goes_to_macros_and_templates() {
        let host = host();
        let mut twig = Twig::default();
        twig.activate(&host.context("aritti.twig")).unwrap();

        let (d, p) = at("{% import 'forms.html.twig' as forms %}{{ forms.but^ton('x') }}");
        let found = twig.definition(&d, p);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].path.as_str(), found[0].range.start.line), ("/project/templates/forms.html.twig", 1));

        let (d, p) = at("{% from 'forms.html.twig' import input as field %}{{ fie^ld('a') }}");
        assert_eq!(twig.definition(&d, p)[0].range.start.line, 0);

        let (d, p) = at("{% extends 'forms.ht^ml.twig' %}");
        assert_eq!(twig.definition(&d, p)[0].path, "/project/templates/forms.html.twig");
    }

    #[test]
    fn project_extension_names_complete_and_navigate_outside_symfony() {
        let host = host();
        let mut twig = Twig::default();
        twig.activate(&host.context("aritti.twig")).unwrap();

        let (d, p) = at("{{ price|mo^ }}");
        assert!(labels(twig.completion(&d, p, None)).contains(&"money".to_owned()));
        let (d, p) = at("{{ asse^ }}");
        assert!(labels(twig.completion(&d, p, None)).contains(&"asset_url".to_owned()));
        let (d, p) = at("{{ price|mon^ey }}");
        let found = twig.definition(&d, p);
        assert_eq!((found[0].path.as_str(), found[0].range.start.line), ("/project/src/Ext.php", 1));

        host.add_file("symfony.lock", "{}"); forget_project_facts();
        let (d, p) = at("{{ price|mo^ }}");
        assert!(twig.completion(&d, p, None).is_empty());
        let (d, p) = at("{{ price|mon^ey }}");
        assert!(twig.definition(&d, p).is_empty());
    }
    #[test]
    fn scoped_imports_shadow_only_the_current_block_and_embed_isolates_outer_bindings() {
        let host = host();
        host.add_file("templates/other.twig", "{% macro input(x) %}{% endmacro %}");
        let mut twig = Twig::default();
        let (d, p) = at(
            "{% import 'forms.html.twig' as f %}{% block one %}{% import 'other.twig' as f %}{{ f.in^put() }}{% endblock %}{% block two %}{{ f.input() }}{% endblock %}",
        );
        assert!(twig.definition(&d, p)[0].path.ends_with("other.twig"));
        let (d, p) = at(
            "{% import 'forms.html.twig' as f %}{% block one %}{% import 'other.twig' as f %}{{ f.input() }}{% endblock %}{% block two %}{{ f.in^put() }}{% endblock %}",
        );
        assert!(twig.definition(&d, p)[0].path.ends_with("forms.html.twig"));
        for source in [
            "{% import 'forms.html.twig' as f %}{% embed 'base.twig' %}{{ f.in^put() }}{% endembed %}",
            "{% import 'forms.html.twig' as f %}{% macro local() %}{{ f.in^put() }}{% endmacro %}",
            "{% if dynamic %}{% import 'forms.html.twig' as f %}{% endif %}{{ f.in^put() }}",
            "{% import 'forms.html.twig' as f %}{% import 'other.twig' as f %}{{ f.in^put() }}",
        ] {
            let (d, p) = at(source);
            assert!(twig.definition(&d, p).is_empty(), "{source}");
        }
        let (d, p) = at("{% embed 'base.twig' %}{% import 'other.twig' as f %}{{ f.in^put() }}{% endembed %}");
        assert!(twig.definition(&d, p)[0].path.ends_with("other.twig"));
    }
    #[test]
    fn commented_raw_html_and_quoted_imports_or_calls_do_not_create_owners() {
        let host = host();
        let mut twig = Twig::default();
        twig.activate(&host.context("aritti.twig")).unwrap();
        for source in [
            "{# {% import 'forms.html.twig' as f %} #}{{ f.in^put() }}",
            "{% verbatim %}{% import 'forms.html.twig' as f %}{% endverbatim %}{{ f.in^put() }}",
            "{% import 'forms.html.twig' as f %}<p>f.in^put()</p>",
            "{% import 'forms.html.twig' as f %}{{ 'f.in^put()' }}",
            "{# {% extends 'forms.ht^ml.twig' %} #}",
            "<p>include('forms.ht^ml.twig')</p>",
            "{{ \"include('forms.ht^ml.twig')\" }}",
            "{{ object.include('forms.ht^ml.twig') }}",
            "{{ asset_u^rl }}",
        ] {
            let (d, p) = at(source);
            assert!(twig.definition(&d, p).is_empty(), "{source}");
        }
        host.add_file("templates/fake.twig", "{# {% macro input() %}{% endmacro %} #}");
        let (d, p) = at("{% import 'fake.twig' as f %}{{ f.in^put() }}");
        assert!(twig.definition(&d, p).is_empty());
    }
    #[test]
    fn template_paths_have_conventional_ownership_without_namespace_or_suffix_guesses() {
        let host = host();
        host.add_file("elsewhere/unique.twig", "x");
        let mut twig = Twig::default();
        for name in ["unique.twig", "@bundle/forms.html.twig", "../templates/forms.html.twig", "forms*.twig", "forms.html.twig/../x"] {
            let (d, p) = at(&format!("{{% include '{name}^' %}}"));
            assert!(twig.definition(&d, p).is_empty(), "{name}");
        }
        let (d, p) = at("{% include 'forms.ht^ml.twig' %}");
        assert_eq!(twig.definition(&d, p).len(), 1);
        host.add_file("symfony.lock", "{}"); forget_project_facts();
        assert!(twig.definition(&d, p).is_empty());
        let (d, p) = at("{% import 'forms.html.twig' as f %}{{ f.in^put() }}");
        assert!(twig.definition(&d, p).is_empty());
        let (d, p) = at("{% import 'forms.html.twig' as f %}{{ f.^ }}");
        assert!(twig.completion(&d, p, None).is_empty());
    }
    #[test]
    fn configured_namespaces_resolve_template_literals_and_macros_with_bounded_paths() {
        let host = host();
        host.add_file("lib/ui/forms.html.twig", "{% macro input(a) %}{% endmacro %}");
        host.add_file("elsewhere/forms.html.twig", "x");
        let mut twig = Twig::default();
        let (d, p) = at("{% include '@ui/forms.ht^ml.twig' %}");
        assert!(twig.definition(&d, p).is_empty());
        host.set_setting("templateNamespaces", r#"[{"namespace":"@ui","path":"lib/ui/"}]"#);
        assert!(twig.definition(&d, p)[0].path.ends_with("/lib/ui/forms.html.twig"));
        for kind in ["extends", "embed", "use", "import", "from"] {
            let tail = match kind { "import" => " as f", "from" => " import input", "use" => "", _ => "" };
            let (d, p) = at(&format!("{{% {kind} '@ui/forms.ht^ml.twig'{tail} %}}"));
            assert_eq!(twig.definition(&d, p).len(), 1, "{kind}");
        }
        let (d, p) = at("{% import '@ui/forms.html.twig' as f %}{{ f.in^put() }}");
        assert!(twig.definition(&d, p)[0].path.ends_with("/lib/ui/forms.html.twig"));
        host.add_file("symfony.lock", "{}"); forget_project_facts();
        let (d, p) = at("{% include '@ui/forms.ht^ml.twig' %}");
        assert_eq!(twig.definition(&d, p).len(), 1);
        for bad in [
            r#"[{"namespace":"ui","path":"../elsewhere"}]"#,
            r#"[{"namespace":"ui","path":"/abs"}]"#,
            r#"[{"namespace":"ui","path":"lib/../elsewhere"}]"#,
            r#"[{"namespace":"u/i","path":"lib/ui"}]"#,
            r#"[{"namespace":"ui","path":"lib/ui"},{"namespace":"ui","path":"elsewhere"}]"#,
            "not json",
        ] {
            host.set_setting("templateNamespaces", bad);
            assert!(twig.definition(&d, p).is_empty(), "{bad}");
        }
    }
    #[test]
    fn symfony_twig_paths_merge_with_setting_and_explicit_wins() {
        let host = host();
        host.add_file("lib/ui/forms.html.twig", "x");
        host.add_file("backend/templates/a.twig", "x");
        host.add_file("other/a.twig", "x");
        host.add_file("config/packages/twig.yaml", "twig:\n    paths:\n        'backend/templates': 'admin'\n        'lib/ui': ui\n        'tpl/main': ~\n"); forget_project_facts();
        let mut twig = Twig::default();
        let (d, p) = at("{% include '@admin/a.t^wig' %}");
        assert!(twig.definition(&d, p)[0].path.ends_with("/backend/templates/a.twig"));
        let (d, p) = at("{% include '@ui/forms.ht^ml.twig' %}");
        assert_eq!(twig.definition(&d, p).len(), 1);
        host.set_setting("templateNamespaces", r#"[{"namespace":"admin","path":"other"}]"#);
        let (d, p) = at("{% include '@admin/a.t^wig' %}");
        assert!(twig.definition(&d, p)[0].path.ends_with("/other/a.twig"));
        let (d, p) = at("{% include '@ui/forms.ht^ml.twig' %}");
        assert_eq!(twig.definition(&d, p).len(), 1);
        host.add_file("config/packages/twig.yaml", "twig:\n    paths: { 'lib/ui': ui }\n"); forget_project_facts();
        assert!(twig.definition(&d, p).is_empty());
    }
    #[test]
    fn default_path_and_main_namespace_paths_resolve_plain_names() {
        let host = host();
        host.add_file("views/a.twig", "x");
        host.add_file("templates/a.twig", "x");
        host.add_file("tpl/main/b.twig", "x");
        let mut twig = Twig::default();
        let (d, p) = at("{% include 'a.t^wig' %}");
        assert!(twig.definition(&d, p)[0].path.ends_with("/templates/a.twig"));
        host.add_file("config/packages/twig.yaml", "twig:\n    default_path: '%kernel.project_dir%/views'\n    paths:\n        'tpl/main': ~\n"); forget_project_facts();
        assert!(twig.definition(&d, p)[0].path.ends_with("/views/a.twig"));
        let (d, p) = at("{% include 'b.t^wig' %}");
        assert!(twig.definition(&d, p)[0].path.ends_with("/tpl/main/b.twig"));
    }
    #[test]
    fn environment_blocks_add_directories_and_refuse_conflicting_namespaces() {
        let host = host();
        host.add_file("dev/ui/x.twig", "x");
        host.add_file("prod/ui/x.twig", "x");
        host.add_file("only/dev/y.twig", "x");
        host.add_file("config/packages/twig.yaml", "twig:\n    paths:\n        a: base\nwhen@dev:\n    twig:\n        paths:\n            only/dev: devns\n            dev/ui: ui\nwhen@prod:\n    twig:\n        paths:\n            prod/ui: ui\n"); forget_project_facts();
        host.add_file("only/dev/y.twig", "x");
        let mut twig = Twig::default();
        let (d, p) = at("{% include '@devns/y.t^wig' %}");
        assert_eq!(twig.definition(&d, p).len(), 1);
        let (d, p) = at("{% include '@ui/x.t^wig' %}");
        assert!(twig.definition(&d, p).is_empty());
        host.add_file("config/packages/test/twig.yaml", "twig:\n    default_path: tests/views\n"); forget_project_facts();
        host.add_file("tests/views/z.twig", "x");
        let (d, p) = at("{% include 'z.t^wig' %}");
        assert_eq!(twig.definition(&d, p).len(), 1);
        host.add_file("config/packages/dev/twig.yaml", "twig:\n    default_path: dev/views\n"); forget_project_facts();
        assert!(twig.definition(&d, p).is_empty());
    }
    #[test]
    fn use_and_embed_targets_and_used_blocks_resolve_only_when_unambiguous() {
        let host = host();
        host.add_file("templates/blocks.twig", "{% block sidebar %}s{% endblock %}{% block dup %}a{% endblock %}{% block dup %}b{% endblock %}");
        host.add_file("templates/other.twig", "{% block sidebar %}o{% endblock %}");
        let mut twig = Twig::default();
        for source in ["{% use 'blocks.t^wig' %}", "{% embed 'blocks.t^wig' %}{% endembed %}", "{% embed 'blocks.t^wig' with {'a':1} only %}{% endembed %}"] {
            let (d, p) = at(source);
            assert_eq!(twig.definition(&d, p).len(), 1, "{source}");
        }
        let (d, p) = at("{% use 'blocks.twig' %}{{ block('side^bar') }}");
        let found = twig.definition(&d, p);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("templates/blocks.twig"));
        for source in [
            "{% use 'blocks.twig' with sidebar as side %}{{ block('side^bar') }}",
            "{% use 'blocks.twig' %}{% use 'other.twig' %}{{ block('side^bar') }}",
            "{% use 'blocks.twig' %}{{ block('d^up') }}",
            "{% use 'blocks.twig' %}{% block sidebar %}x{% endblock %}{{ block('side^bar') }}",
            "{% extends 'other.twig' %}{% use 'blocks.twig' %}{{ block('side^bar') }}",
            "{% use 'blocks.twig' %}{{ block('missing^') }}",
            "{% use 'blocks.twig' %}{{ x.block('side^bar') }}",
        ] {
            let (d, p) = at(source);
            assert!(twig.definition(&d, p).is_empty(), "{source}");
        }
    }
    #[test]
    fn self_macros_use_unsaved_unicode_source_without_project_or_stale_ranges() {
        let host = MockHost::install().with_capabilities(&[]);
        host.set_project_root(None);
        let mut twig = Twig::default();
        let (d, p) = at("😀 {% macro café(x) %}{{ x }}{% endmacro %}{{ _self.ca^fé('x') }}");
        let found = twig.definition(&d, p);
        assert_eq!(found.len(), 1);
        let r = found[0].range;
        assert_eq!(&d.text[offset(&d.text, r.start).unwrap()..offset(&d.text, r.end).unwrap()], "café");
        let fixed = Document { text: d.text.replace("macro café", "macro other"), version: 2, ..d.clone() };
        assert!(twig.definition(&fixed, position(&fixed.text, fixed.text.find("_self.café").unwrap() + 8)).is_empty());
        let (d, p) = at("{% macro x() %}{% endmacro %}{% macro x() %}{% endmacro %}{{ _self.^x() }}");
        assert!(twig.definition(&d, p).is_empty());
        assert_eq!(host.fetches(), 0);
    }
    #[test]
    fn refreshed_targets_duplicate_bindings_and_provider_bounds_clear_old_navigation() {
        let host = host();
        let mut twig = Twig::default();
        let (d, p) = at("{% from 'forms.html.twig' import input as field %}{{ fie^ld() }}");
        assert_eq!(twig.definition(&d, p).len(), 1);
        host.open_editor("/project/templates/forms.html.twig", "twig", "{% macro changed() %}{% endmacro %}", &[]);
        assert!(twig.definition(&d, p).is_empty());
        let (d, p) = at("{% from 'forms.html.twig' import input as field,button as field %}{{ fie^ld() }}");
        assert!(twig.definition(&d, p).is_empty());
        let (d, p) = at("{% macro one() %}{% endmacro %}{{ _self.o^ne() }}");
        let wrong = Document { language: "html".into(), ..d.clone() };
        assert!(twig.definition(&wrong, p).is_empty());
        let huge = Document { text: format!("{}{}", " ".repeat(128 << 10), d.text), ..d };
        assert!(twig.definition(&huge, position(&huge.text, huge.text.len() - 6)).is_empty());
        let (d, p) = at(&format!("{}{{% macro one() %}}{{% endmacro %}}{{{{ _self.o^ne() }}}}", "{% do 1 %}".repeat(4096)));
        assert!(twig.definition(&d, p).is_empty());
    }
}
