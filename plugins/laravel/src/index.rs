//! A text index of a Laravel project: named routes, views, config keys,
//! translation keys, env keys, middleware aliases, assets, controllers and
//! their methods, each with where it is defined.

use std::collections::{BTreeMap, BTreeSet};
#[path="index_read_cache.rs"]
mod read_cache;
#[path="index_builder.rs"]
mod builder;
pub(crate) use builder::{Builder,BuildProgress,PROVIDER_STEP_BUDGET,STEP_BUDGET};

use openidea_plugin::host::{self, Project};
use openidea_plugin::{Error, Position};

use crate::refs::{self, Kind};
use crate::{blade, controllers, migrations, php, routes};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub detail: Option<String>,
    /// Absolute path; empty for built-in names.
    pub path: String,
    pub at: Position,
    /// Length in UTF-16 units of the name in the file; 0 to point at the file.
    pub width: u32,
}

pub type Names = BTreeMap<String, Entry>;

pub trait AddName {
    fn add(&mut self, name: &str, detail: Option<String>, path: &str, at: Position, width: usize);
}

impl AddName for Names {
    /// The first definition of a name wins.
    fn add(&mut self, name: &str, detail: Option<String>, path: &str, at: Position, width: usize) {
        if name.is_empty() {
            return;
        }

        self.entry(name.to_owned()).or_insert(Entry { detail, path: path.to_owned(), at, width: width as u32 });
    }
}

/// A route of `routes/`, with where it is defined.
#[derive(Clone, Debug)]
pub struct RouteEntry {
    pub def: routes::RouteDef,
    pub endpoint_uri: Option<String>,
    pub path: String,
    pub at: Position,
    /// Source token width in UTF-16 units; RouteDef.width remains a byte length.
    pub width: u32,
}

/// A controller rendering a view: `return view('posts.index')`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Render {
    pub view: String,
    /// `PostController::index`.
    pub caller: String,
    pub path: String,
    pub at: Position,
}

#[derive(Default, Debug, Clone)]
pub struct Index {
    pub(crate) source_evidence:BTreeMap<String,Option<(usize,String)>>,
    pub(crate) source_evidence_incomplete:bool,
    pub(crate) source_queries:BTreeMap<(String,u32),Option<Vec<String>>>,
    pub(crate) source_root:String,
    pub(crate) source_settings:BTreeMap<String,Option<String>>,
    pub navigation: BTreeMap<String, ui_kit::navigation::Graph>,
    pub(crate) metadata: crate::metadata::Metadata,
    pub(crate) dependency_files: BTreeSet<String>,
    pub(crate) dependency_roots:BTreeSet<String>,
    pub routes: Names,
    pub(crate) route_files: BTreeSet<String>,
    pub(crate) route_mounts: BTreeMap<String,routes::MountContext>,
    pub(crate) route_mount_unknown: BTreeSet<String>,
    pub(crate) route_controllers:BTreeMap<(String,usize,String),String>,
    pub route_defs: Vec<RouteEntry>,
    pub views: Names,
    /// Source folders relative to the winning view root, for inventory filters.
    pub view_folders: BTreeMap<String, String>,
    pub blade: BTreeMap<String, blade::Facts>,
    pub config: Names,
    /// Bounded source previews for Config browsing; never runtime values.
    pub config_expressions: BTreeMap<String, String>,
    pub config_incomplete: BTreeSet<String>,
    /// Config files whose only unknown keys sit inside the listed dotted arrays; keys elsewhere are authoritative.
    pub config_local_arrays: BTreeMap<String, BTreeSet<String>>,
    /// Files that default-merging could not reconcile; these stay wholly incomplete.
    pub config_merge_incomplete: BTreeSet<String>,
    pub translations: Names,
    /// Every locale definition for inventory browsing; completion keeps its preferred definition.
    pub translations_by_locale: BTreeMap<String, Names>,
    /// `lang/<locale>/<file>.php` files by the file's key, main locale first.
    pub translation_files: BTreeMap<String, Vec<String>>,
    pub env: Names,
    /// Masked display values of env variables; the first definition wins, like `env`.
    pub env_values: BTreeMap<String, String>,
    pub middleware: Names,
    pub assets: Names,
    pub controllers: Names,
    /// Public methods by controller class.
    pub methods: BTreeMap<String, Names>,
    /// Blade components by tag name (`forms.input` for `<x-forms.input>`).
    pub components: Names,
    /// Livewire components by tag name (`counter` for `<livewire:counter>`).
    pub livewire: Names,
    /// Files `@vite()` can name, relative to the project.
    pub vite: Names,
    /// Gate abilities (`Gate::define`) and policy methods.
    pub abilities: Names,
    /// Custom Blade directives (`Blade::directive('name'`) found in `app/`.
    pub directives: Names,
    /// Validation rule names.
    pub rules: Names,
    /// Container names: the framework's aliases, literal `bind('x'` calls and the classes of `app/`.
    pub bindings: Names,
    /// Classes of `app/Providers`.
    pub providers: Names,
    /// Columns by table, from the migrations.
    pub columns: BTreeMap<String, Names>,
    /// Owned SQL constraints and incomplete statements, keyed by source file.
    pub(crate) sql_sources: BTreeMap<String, serde_json::Value>,
    pub(crate) eloquent_models: crate::eloquent_members::Models,
    pub(crate) eloquent_enums: crate::eloquent_members::Enums,
    pub(crate) eloquent_objects: crate::eloquent_members::Objects,
    pub(crate) eloquent_functions: crate::eloquent_members::Functions,
    /// Table of each model, by short class name and by file.
    pub models: BTreeMap<String, String>,
    pub model_files: BTreeMap<String, String>,
    pub(crate) packages: crate::package_integrations::Facts,
    pub(crate) request_fields: crate::validation_requests::Requests,
    pub(crate) mass_assignment_sources: crate::eloquent_checks::Policies,
    pub(crate) connection_override_sources: BTreeSet<String>,
    /// Relation methods of each model and the model each returns.
    pub relations: BTreeMap<String, BTreeMap<String, String>>,
    /// Local scopes (`scopeActive`, `#[Scope]`) by model, as called.
    pub scopes: BTreeMap<String, BTreeSet<String>>,
    pub renders: Vec<Render>,
    /// Inertia pages by name (`Posts/Index`), from `resources/js/Pages` and `resources/js/pages`.
    pub inertia_pages: Names,
    /// File names without extension under `database/factories`.
    pub factories: BTreeSet<String>,
    /// Models that name their factory themselves (`newFactory`, `UseFactory`).
    pub factory_overrides: BTreeSet<String>,
    /// A provider registers component directories or namespaces the index can't follow.
    pub open_components: bool,
    /// A provider registers Livewire components or namespaces the index can't follow.
    pub open_livewire: bool,
    /// Some files were skipped to stay within the call's time budget.
    pub partial: bool,
}

/// Recheck only the source/query evidence captured by a completed index. This
/// performs bounded owned reads and exact prior query replay, never PHP parsing.
pub(crate) fn index_settings_current(index:&Index)->bool{index.source_settings.iter().all(|(key,value)|&host::setting_string(key)==value)}
pub(crate) fn index_sources_current(project:&Project,index:&Index)->bool {
    let Ok(root)=project.root()else{return false;};
    if !index_settings_current(index) || index.partial || index.source_evidence_incomplete || index.source_root!=root || index.source_evidence.is_empty(){return false;}
    for ((glob,limit),expected) in &index.source_queries {
        let Some(expected)=expected else{return false;};
        let Ok(mut found)=project.find_files(glob,*limit)else{return false;};
        found.retain(|path|host::project_of(path).and_then(|owner|owner.root().ok()).as_deref()==Some(root.as_str()));found.sort();found.dedup();
        if &found!=expected{return false;}
    }
    let mut bytes=0usize;
    for (path,expected) in &index.source_evidence {
        let absolute=format!("{}/{}",root.trim_end_matches('/'),path);
        if host::project_of(&absolute).and_then(|owner|owner.root().ok()).as_deref()!=Some(root.as_str()){return false;}
        match expected {
            None=>if !matches!(project.stat(&absolute),Err(Error::NotFound(_))){return false;},
            Some((length,hash))=>{
                bytes=bytes.saturating_add(*length);if bytes>32*1024*1024{return false;}
                let Ok(stat)=project.stat(&absolute)else{return false;};if stat.kind!=openidea_plugin::FileKind::File || stat.size!=*length as u64{return false;}
                let Ok(text)=project.read_file(&absolute)else{return false;};if text.len()!=*length||crate::artisan_metadata::hash(&text)!=*hash{return false;}
            }
        }
    }
    true
}

// Enumeration and retained source budgets are enforced by the resumable builder.
const MAX_FILES: u32 = 5000;
const VIEW_ROOTS: &[&str] = &["resources/views", "app/views", "resources/templates"];
const LANG_ROOTS: &[&str] = &["lang", "resources/lang"];
const VITE_EXTENSIONS: &[&str] = &["css", "scss", "sass", "less", "js", "jsx", "ts", "tsx", "vue"];
const INERTIA_PAGE_ROOTS: &[&str] = &["resources/js/Pages", "resources/js/pages"];
const INERTIA_PAGE_EXTENSIONS: &[&str] = &["vue", "jsx", "tsx", "svelte"];
/// Composer packages that provide `<x-…>` components without a namespace (icon packs, Jetstream, Filament, Flux).
const COMPONENT_PACKAGES: &[&str] = &["/blade-", "laravel/jetstream", "filament/", "livewire/flux"];
const ROUTE_FILES: &[&str] = &["routes/**/*.php", "app/Http/routes.php", "app/routes.php"];

/// Rules of Laravel's validator (the "Available Validation Rules" of the documentation).
const VALIDATION_RULES: &[&str] = &[
    "accepted",
    "accepted_if",
    "active_url",
    "after",
    "after_or_equal",
    "alpha",
    "alpha_dash",
    "alpha_num",
    "array",
    "ascii",
    "bail",
    "before",
    "before_or_equal",
    "between",
    "boolean",
    "confirmed",
    "contains",
    "current_password",
    "date",
    "date_equals",
    "date_format",
    "decimal",
    "declined",
    "declined_if",
    "different",
    "digits",
    "digits_between",
    "dimensions",
    "distinct",
    "doesnt_end_with",
    "doesnt_start_with",
    "email",
    "ends_with",
    "enum",
    "exclude",
    "exclude_if",
    "exclude_unless",
    "exclude_with",
    "exclude_without",
    "exists",
    "extensions",
    "file",
    "filled",
    "gt",
    "gte",
    "hex_color",
    "image",
    "in",
    "in_array",
    "integer",
    "ip",
    "ipv4",
    "ipv6",
    "json",
    "list",
    "lowercase",
    "lt",
    "lte",
    "mac_address",
    "max",
    "max_digits",
    "mimetypes",
    "mimes",
    "min",
    "min_digits",
    "missing",
    "missing_if",
    "missing_unless",
    "missing_with",
    "missing_with_all",
    "multiple_of",
    "not_in",
    "not_regex",
    "nullable",
    "numeric",
    "present",
    "present_if",
    "present_unless",
    "present_with",
    "present_with_all",
    "prohibited",
    "prohibited_if",
    "prohibited_unless",
    "prohibits",
    "regex",
    "required",
    "required_array_keys",
    "required_if",
    "required_if_accepted",
    "required_if_declined",
    "required_unless",
    "required_with",
    "required_with_all",
    "required_without",
    "required_without_all",
    "same",
    "size",
    "sometimes",
    "starts_with",
    "string",
    "timezone",
    "ulid",
    "unique",
    "uppercase",
    "url",
    "uuid",
];

/// Route middleware the framework registers (Laravel's `Middleware::defaultAliases()` and groups).
const BUILT_IN_MIDDLEWARE: &[&str] = &[
    "api",
    "auth",
    "auth.basic",
    "auth.session",
    "cache.headers",
    "can",
    "guest",
    "password.confirm",
    "precognitive",
    "signed",
    "throttle",
    "verified",
    "web",
];

pub fn is_laravel(project: &Project) -> bool {
    if project.read_file("artisan").is_ok() {
        return true;
    }

    project.read_file("composer.json").is_ok_and(|composer| composer.contains("\"laravel/framework\""))
}

/// Byte offset → editor position, for many offsets of one file.
pub struct Lines<'a> {
    text: &'a str,
    starts: Vec<usize>,
    /// Sparse UTF-16 prefix counts keep minified Unicode inventories linear to
    /// index. ASCII files need no additional allocation or prefix rescanning.
    utf16: Vec<(usize, usize)>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Self {
        let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        let mut utf16 = Vec::new();
        if !text.is_ascii() {
            utf16.push((0, 0));
            let mut units = 0;
            let mut checkpoint = 0;
            for (at, character) in text.char_indices() {
                if at - checkpoint >= 256 {
                    utf16.push((at, units));
                    checkpoint = at;
                }
                units += character.len_utf16();
            }
        }
        Self { text, starts, utf16 }
    }

    fn utf16_offset(&self, at: usize) -> usize {
        if self.utf16.is_empty() { return at; }
        let checkpoint = self.utf16.partition_point(|&(offset, _)| offset <= at) - 1;
        let (start, units) = self.utf16[checkpoint];
        units + self.text[start..at].encode_utf16().count()
    }

    pub fn position(&self, at: usize) -> Position {
        let line = self.starts.partition_point(|&start| start <= at) - 1;
        let character = self.utf16_offset(at) - self.utf16_offset(self.starts[line]);
        Position::new(line as u32, character as u32)
    }

    /// Navigation uses the recorded source token, not the composed effective key.
    pub(super) fn width(&self, at: usize, bytes: usize) -> usize {
        at.checked_add(bytes).and_then(|end| self.text.get(at..end))
            .filter(|source| !source.contains(['\r', '\n']))
            .map_or(0, |source| source.encode_utf16().count())
    }
}

/// `resources/views/posts/index.blade.php` relative to its root: `posts.index`;
/// `vendor/mail/x.blade.php` overrides the view `mail::x`.
pub fn view_name(relative: &str) -> Option<String> {
    let stem = relative.strip_suffix(".blade.php").or_else(|| relative.strip_suffix(".php"))?;

    if let Some(rest) = stem.strip_prefix("vendor/")
        && let Some((package, view)) = rest.split_once('/')
    {
        return Some(format!("{package}::{}", view.replace('/', ".")));
    }

    Some(stem.replace('/', "."))
}

/// `ide-blade.json` and `.idea/ide-blade.json` are read by path: `find_files`
/// skips hidden directories on the host. A missing file is `NotFound` in the
/// mock and `Invalid` on the host; both mean absent.
pub fn ide_blade_roots(read: impl Fn(&str) -> Result<String, Error>) -> Vec<(Option<String>, String)> {
    ["ide-blade.json", ".idea/ide-blade.json"]
        .iter()
        .filter_map(|path| read(path).ok())
        .flat_map(|text| ide_blade_namespaces(&text))
        .collect()
}

/// `{"namespaces": [{"namespace": "foo", "path": "res"}]}`: entries without a
/// path are dropped, a blank namespace is an unnamespaced root, malformed JSON
/// yields nothing.
pub fn ide_blade_namespaces(text: &str) -> Vec<(Option<String>, String)> {
    scan_namespaces(text.as_bytes()).unwrap_or_default()
}

fn scan_namespaces(b: &[u8]) -> Option<Vec<(Option<String>, String)>> {
    fn skip(b: &[u8], mut i: usize) -> usize {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        i
    }
    fn string(b: &[u8], mut i: usize) -> Option<(String, usize)> {
        if b.get(i) != Some(&b'"') {
            return None;
        }
        i += 1;
        let mut out = Vec::new();
        loop {
            match *b.get(i)? {
                b'"' => return Some((String::from_utf8_lossy(&out).into_owned(), i + 1)),
                b'\\' => {
                    out.push(match *b.get(i + 1)? {
                        b'n' => b'\n',
                        b't' => b'\t',
                        c => c,
                    });
                    i += 2;
                }
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
    }
    fn skip_value(b: &[u8], mut i: usize) -> Option<usize> {
        let mut depth = 0usize;
        loop {
            match *b.get(i)? {
                b'"' => i = string(b, i)?.1,
                b'{' | b'[' => {
                    depth += 1;
                    i += 1;
                }
                b'}' | b']' if depth > 0 => {
                    depth -= 1;
                    i += 1;
                }
                b',' | b'}' if depth == 0 => return Some(i),
                _ => i += 1,
            }
        }
    }

    let key = b"\"namespaces\"";
    let mut i = b.windows(key.len()).position(|w| w == key)? + key.len();
    i = skip(b, i);
    if b.get(i) != Some(&b':') {
        return None;
    }
    i = skip(b, i + 1);
    if b.get(i) != Some(&b'[') {
        return None;
    }
    i += 1;

    let mut found = Vec::new();
    loop {
        i = skip(b, i);
        match *b.get(i)? {
            b']' => return Some(found),
            b'{' => i += 1,
            _ => return None,
        }

        let (mut namespace, mut path) = (String::new(), String::new());
        loop {
            i = skip(b, i);
            if *b.get(i)? == b'}' {
                i += 1;
                break;
            }
            let (name, next) = string(b, i)?;
            i = skip(b, next);
            if *b.get(i)? != b':' {
                return None;
            }
            i = skip(b, i + 1);
            match string(b, i) {
                Some((value, next)) => {
                    i = next;
                    match name.as_str() {
                        "namespace" => namespace = value,
                        "path" => path = value,
                        _ => {}
                    }
                }
                None => i = skip_value(b, i)?,
            }
        }

        let path = path.trim().trim_start_matches("./").trim_matches('/');
        if !path.is_empty() {
            let namespace = namespace.trim();
            found.push(((!namespace.is_empty()).then(|| namespace.to_owned()), path.to_owned()));
        }
    }
}

/// `Forms/InputField.php` is the tag name `forms.input-field`.
fn tag_name(relative: &str) -> String {
    let stem = relative.strip_suffix(".php").unwrap_or(relative);
    stem.split('/').map(kebab).collect::<Vec<_>>().join(".")
}

fn kebab(word: &str) -> String {
    separated(word, '-')
}

/// `UserProfile` as `user-profile` (or `user_profile`).
pub fn separated(word: &str, separator: char) -> String {
    let mut out = String::new();
    let mut previous: Option<char> = None;

    for c in word.chars() {
        if c.is_ascii_uppercase() && previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) {
            out.push(separator);
        }
        out.push(c.to_ascii_lowercase());
        previous = Some(c);
    }

    out
}

/// The table Eloquent uses for a model without `$table`: snake case, plural.
// ponytail: regular English plurals only (-y, -s/x/z/ch/sh, else -s); set `$table` for irregular names.
pub fn table_name(class: &str) -> String {
    let snake = separated(class, '_');

    if let Some(stem) = snake.strip_suffix('y')
        && !stem.ends_with(['a', 'e', 'i', 'o', 'u'])
    {
        return format!("{stem}ies");
    }

    if snake.ends_with(['s', 'x', 'z']) || snake.ends_with("ch") || snake.ends_with("sh") {
        return format!("{snake}es");
    }

    format!("{snake}s")
}

/// Aliases the framework registers in the container (the "Facade Class Reference" and the container's core aliases).
const CORE_BINDINGS: &[&str] = &[
    "app",
    "auth",
    "auth.driver",
    "blade.compiler",
    "cache",
    "cache.store",
    "config",
    "cookie",
    "db",
    "db.connection",
    "encrypter",
    "events",
    "files",
    "filesystem",
    "filesystem.disk",
    "hash",
    "log",
    "mailer",
    "queue",
    "queue.connection",
    "redirect",
    "request",
    "router",
    "session",
    "session.store",
    "translator",
    "url",
    "validator",
    "view",
];

/// `namespace X; class Stem`: the fully qualified name and the offset of the name.
fn declared_class(masked: &str, stem: &str) -> Option<(String, usize)> {
    let namespace = masked.lines().find_map(|line| line.trim().strip_prefix("namespace ")?.strip_suffix(';')).map(|n| n.trim().to_owned());

    let offset = ["class ", "interface ", "trait ", "enum "].iter().find_map(|keyword| {
        masked.match_indices(keyword).find_map(|(at, _)| {
            let line_start = masked[..at].rfind('\n').map_or(0, |i| i + 1);
            let before = masked[line_start..at].trim();
            let start = at + keyword.len();
            let declares = before.split_whitespace().all(|word| matches!(word, "final" | "abstract" | "readonly"));
            let named =
                masked[start..].starts_with(stem) && !masked.as_bytes().get(start + stem.len()).is_some_and(|b| php::is_identifier(*b));
            (declares && named).then_some(start)
        })
    })?;

    let fqcn = namespace.map_or_else(|| stem.to_owned(), |namespace| format!("{namespace}\\{stem}"));
    Some((fqcn, offset))
}

const RELATION_CALLS: &[&str] =
    &["hasManyThrough", "hasOneThrough", "hasMany", "hasOne", "belongsToMany", "belongsTo", "morphToMany", "morphMany", "morphOne"];

/// Relation methods (`hasMany(Comment::class)`) and local scopes of a model's text.
fn model_members(masked: &str) -> (BTreeMap<String, String>, BTreeSet<String>) {
    let mut relations = BTreeMap::new();
    let mut scopes = BTreeSet::new();

    for (at, _) in masked.match_indices("function ") {
        let name: String = masked[at + "function ".len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let open = at + "function ".len() + name.len();
        if name.is_empty() || !masked[open..].starts_with('(') {
            continue;
        }

        if let Some(rest) = name.strip_prefix("scope").filter(|r| r.starts_with(|c: char| c.is_ascii_uppercase())) {
            let mut chars = rest.chars();
            scopes.insert(chars.next().map(|c| c.to_ascii_lowercase()).into_iter().chain(chars).collect());
        }

        if masked[..at].trim_end().ends_with("#[Scope]") {
            scopes.insert(name.clone());
        }

        let Some(brace) = php::closing(masked, open)
            .and_then(|close| masked[close..].find(['{', ';']).map(|i| close + i))
            .filter(|i| masked.as_bytes()[*i] == b'{')
        else {
            continue;
        };
        let Some(end) = php::closing(masked, brace) else { continue };
        let body = &masked[brace..end];

        for call in RELATION_CALLS {
            let needle = format!("$this->{call}(");
            let Some(found) = body.find(&needle) else { continue };
            let first =
                php::arguments(body, found + needle.len() - 1).into_iter().next().and_then(|range| php::class_constant(body, range));
            if let Some(target) = first {
                relations.insert(name.clone(), target.rsplit('\\').next().unwrap_or(target).to_owned());
            }
            break;
        }
    }

    (relations, scopes)
}

/// `protected $table = 'x';`
pub fn explicit_table(text: &str) -> Option<String> {
    let at = text.find("$table")?;
    let rest = text[at + "$table".len()..].trim_start().strip_prefix('=')?;
    let start = text.len() - rest.len();
    php::literal(text, start..start + rest.find(';')?).map(|(table, _)| table.to_owned())
}

fn comma_separated(key: &str) -> Vec<String> {
    let value = host::setting_string(key).unwrap_or_default();
    value.split(',').map(|p| p.trim().trim_matches('/')).filter(|p| !p.is_empty()).map(str::to_owned).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[path="eloquent_roots.rs"]
mod eloquent_roots;

fn cache_declarations(index:&mut Index,text:&str,path:&str,relative:&str,roots:&[eloquent_roots::Root],role:u8){
    // Conventional roles retain their original facts. Newly shared kind caches
    // require an exact declared Composer namespace/path mapping.
    let mapped=crate::eloquent_members::declaration(text).is_some_and(|full|eloquent_roots::matches(roots,relative,&full));
    if role&1!=0||mapped{crate::eloquent_members::cache(&mut index.eloquent_models,text,path);}
    if role&2!=0||mapped{crate::eloquent_members::cache_object(&mut index.eloquent_objects,text,path);}
    if role&4!=0||mapped{crate::eloquent_members::cache_enum(&mut index.eloquent_enums,text,path);}
}

impl Index {
    /// Unknown keys are confined to arrays that neither are nor contain `parent`: keys elsewhere in the file are authoritative.
    pub(crate) fn config_gap_is_local(&self, path: &str, parent: &str) -> bool {
        !self.config_merge_incomplete.contains(path)
            && self.config_local_arrays.get(path).is_some_and(|arrays| {
                // A spread or unknown key may overwrite siblings, so descendants of an unknown array are unknown too.
                !arrays.iter().any(|array| parent == array || parent.strip_prefix(array.as_str()).is_some_and(|rest| rest.starts_with('.')))
            })
    }
    /// Config files, or arrays inside files whose other keys are known, that could not be fully inventoried.
    pub(crate) fn config_coverage_gaps(&self) -> Vec<String> {
        let mut gaps = BTreeSet::new();
        for path in &self.config_incomplete {
            match self.config_local_arrays.get(path).filter(|_| !self.config_merge_incomplete.contains(path)) {
                Some(arrays) => gaps.extend(arrays.iter().cloned()),
                None => { gaps.insert(path.rsplit('/').next().unwrap_or(path).to_owned()); }
            }
        }
        gaps.into_iter().collect()
    }
    /// Default merging could not reconcile this file, so no per-array coverage may be trusted for it.
    pub(crate) fn merge_incomplete_insert(&mut self, path: String) {
        self.config_merge_incomplete.insert(path.clone());
        self.config_incomplete.insert(path);
    }
    pub(crate) fn merge_incomplete_extend(&mut self, paths: impl IntoIterator<Item = String>) {
        for path in paths { self.merge_incomplete_insert(path); }
    }
    /// Reparse a current route document using its proven application mount.
    /// The parser retains byte offsets into the original document.
    pub(crate) fn parse_routes(&self,path:&str,source:&str)->Vec<routes::RouteDef>{
        if self.route_mount_unknown.contains(path){return vec![];}
        match self.route_mounts.get(path){
            Some(context)=>routes::parse_with_context(source,&self.metadata.route_methods,context),
            None=>routes::parse_with_custom(source,&self.metadata.route_methods),
        }
    }

    pub(crate) fn route_controller_full(&self,entry:&RouteEntry)->Option<&str>{
        self.route_controllers.get(&(entry.path.clone(),entry.def.offset,entry.def.action.clone())).map(String::as_str)
    }
    pub(crate) fn affects(&self, relative: &str) -> bool { self.metadata.affects(relative) || self.dependency_files.contains(relative) || self.dependency_roots.iter().any(|root|relative==root||relative.starts_with(&format!("{root}/"))) }
    pub fn build(project:&Project)->Index {
        let Ok(mut builder)=Builder::new(project)else{return Index{partial:true,..Index::default()};};
        loop{match builder.step(project,std::time::Instant::now()+STEP_BUDGET){Ok(BuildProgress::Ready(index))=>return index,Ok(BuildProgress::Pending{..})=>{},Err(_)=>return Index{partial:true,..Index::default()}}}
    }
    fn build_pass(project:&Project,mut index:&mut Index,session:&builder::Session){
        let _syntax_scope=crate::eloquent_members::index_syntax_scope();
        let root = project.root().unwrap_or_default();
        if !session.done("initial:metadata") {
            if session.heavy("initial:metadata"){session.snapshot(project,"artisan");index.dependency_files.insert("artisan".into());index.metadata=crate::metadata::Metadata::load(project);}
            return;
        }
        let relative = |path: &str| path.strip_prefix(&format!("{root}/")).unwrap_or(path).to_owned();

        let files = |globs:&[String]|session.files(project,&root,globs);
        let globs = |patterns: &[&str]| -> Vec<String> { patterns.iter().map(|p| (*p).to_owned()).collect() };

        let read = |stage:&str,path: &str, index: &mut Index| -> Option<String> {
            if !session.file(project,stage,path,index){return None;}
            let text=match session.source_cache.borrow_mut().read(path,||project.read_file(path).ok()) {
                read_cache::Read::Cached(text)=>return Some(text),
                read_cache::Read::Fresh(text)=>text,
                read_cache::Read::Missing=>return None,
                read_cache::Read::Exhausted=>{
                    index.partial=true;
                    const ISSUE:&str="Index source cache budget exceeded (8 MiB or 5000 files); remaining source reads withheld";
                    if !index.metadata.issues.iter().any(|issue|issue==ISSUE){index.metadata.issues.push(ISSUE.into());}
                    return None;
                }
            };
            if path.ends_with(".php") {
                let graph=crate::indexer::contextual_navigation(index,path,&text);
                index.navigation.insert(path.into(),graph);
                crate::eloquent_members::cache_metadata(&mut index.metadata,&text,path);
                crate::eloquent_members::cache_functions(&mut index.eloquent_functions,&text,path);
                crate::validation_requests::cache(&mut index.request_fields,&text,path);
                crate::eloquent_checks::cache(&mut index.mass_assignment_sources,&text,path);
                if !path.ends_with(".blade.php") {crate::eloquent_members::related_facts::cache_connection_overrides(&mut index.connection_override_sources,&text,path);}
                if !index.metadata.mixins.is_empty() || !index.metadata.forward_methods.is_empty() {crate::eloquent_members::cache_object(&mut index.eloquent_objects,&text,path);}
            }
            Some(text)
        };

        if !session.done("sources:complete") {
        let declaration_roots=session.snapshot(project,"composer.json").and_then(|text|eloquent_roots::parse(&text).ok()).unwrap_or_default();
        let mut view_roots: Vec<(Option<String>, String)> = comma_separated("viewPaths").into_iter().map(|r|(None,r)).collect();
        view_roots.extend(index.metadata.view_roots.clone());
        view_roots.extend(ide_blade_roots(|path| session.snapshot(project,path).ok_or(Error::Stale)));
        view_roots.extend(VIEW_ROOTS.iter().map(|r|(None,(*r).to_owned())));
        for (namespace, view_root) in &view_roots {
            for path in files(&[format!("{view_root}/**/*.php")]) {
                let Some(mut name) = relative(&path).strip_prefix(&format!("{view_root}/")).and_then(view_name) else { continue };
                if let Some(namespace) = namespace {
                    name = format!("{namespace}::{name}");
                }
                let relative_view=relative(&path);
                let relative_view=relative_view.strip_prefix(&format!("{view_root}/")).unwrap_or(&relative_view);
                let folder=relative_view.rsplit_once('/').map(|(folder,_)|folder.replace('/',".")).unwrap_or_else(||"(root)".into());
                index.view_folders.entry(name.clone()).or_insert(folder);
                index.views.add(&name, None, &path, Position::new(0, 0), 0);

                if !path.ends_with(".blade.php") {
                    continue;
                }

                if let Some(text) = read(&format!("views:{namespace:?}:{view_root}"),&path, &mut index) {
                    index.blade.entry(name).or_insert_with(|| blade::facts(&text));
                }
            }
        }

        let mount_source=|relative:&str|->Option<String>{
            let path=format!("{root}/{relative}");
            if !crate::owns_current_path(project,&path){return None;}
            let source=session.snapshot(project,relative).unwrap_or_default();
            (source.len()<=512*1024).then_some(source)
        };
        for relative in ["bootstrap/app.php","app/Providers/RouteServiceProvider.php"]{index.dependency_files.insert(relative.into());}
        let mounts = match (mount_source("bootstrap/app.php"),mount_source("app/Providers/RouteServiceProvider.php")){
            (Some(bootstrap),Some(provider))=>{
                index.route_mount_unknown=routes::application_mount_unknown_paths(&bootstrap,&provider).into_iter().map(|relative|format!("{root}/{relative}")).collect();
                routes::application_mount_contexts(&bootstrap,&provider)
            },
            _=>Some(BTreeMap::new()),
        };
        let mut route_patterns = globs(ROUTE_FILES);
        route_patterns.extend(index.metadata.route_files.clone());
        if let Some(mounts)=&mounts{route_patterns.extend(mounts.keys().cloned());}
        route_patterns.extend(index.route_mount_unknown.iter().cloned());
        for path in files(&route_patterns) {
            index.route_files.insert(path.clone());
            let Some(text) = read("source-1",&path, &mut index) else { continue };
            let masked = php::mask_comments(&text);
            let lines = Lines::new(&text);

            if let Some(context)=mounts.as_ref().and_then(|mounts|mounts.get(&relative(&path))){index.route_mounts.insert(path.clone(),context.clone());}
            if !index.route_mount_unknown.contains(&path){
                let context=index.route_mounts.get(&path).cloned().unwrap_or_default();
                for(def,(full,_))in routes::canonical_controllers(&masked,&index.metadata.route_methods,&context){index.route_controllers.insert((path.clone(),def.offset,def.action),full);}
            }
            for def in index.parse_routes(&path,&masked) {
                let at = lines.position(def.offset);
                let width = lines.width(def.offset, def.width);
                if let Some(name) = &def.name {
                    let detail = format!("{} {} → {}", def.methods, def.uri, def.action);
                    index.routes.add(name, Some(detail), &path, at, width);
                }
                let endpoint_uri = match &mounts {
                    Some(mounts) => mounts.contains_key(&relative(&path)).then(||def.uri.clone()),
                    None => (!relative(&path).ends_with("/api.php")).then(|| def.uri.clone()),
                };
                index.route_defs.push(RouteEntry { def, endpoint_uri, path: path.clone(), at, width: width as u32 });
            }

            index.navigation.insert(path.clone(),crate::indexer::contextual_navigation(&index,&path,&text));
            index.renders.extend(renders(&path, &text));
        }

        for path in files(&globs(&["app/Http/Controllers/**/*.php"])) {
            let Some(text) = read("source-2",&path, &mut index) else { continue };
            let relative_path = relative(&path);
            let inside = relative_path.trim_start_matches("app/Http/Controllers/");
            let masked = php::mask_comments(&text);
            let lines = Lines::new(&text);

            if let Some(controller) = controllers::parse(inside, &masked) {
                let mut methods = Names::new();
                for (method, offset) in &controller.methods {
                    methods.add(method, None, &path, lines.position(*offset), method.len());
                }

                let class_length = controller.class.rsplit('\\').next().map_or(0, str::len);
                index.controllers.add(&controller.class, None, &path, lines.position(controller.class_offset), class_length);
                index.methods.insert(controller.class, methods);
            }

            index.renders.extend(renders(&path, &text));
        }

        for path in files(&globs(&["app/Mail/**/*.php"])) {
            if let Some(text) = read("source-3",&path, &mut index) {
                index.renders.extend(renders(&path, &text));
            }
        }

        let mut config_files = files(&["config/**/*.php".into()]);
        config_files.sort();
        config_files.dedup();
        if config_files.len() >= MAX_FILES as usize { index.config_incomplete.insert(format!("{root}/config")); }
        for path in config_files {
            let Some(text) = read("source-4",&path, &mut index) else { if session.attempted("source-4",&path){index.config_incomplete.insert(path);} continue };
            let relative_path = relative(&path);
            let Some(file_key) = relative_path.strip_prefix("config/").and_then(|p| p.strip_suffix(".php")).map(|p| p.replace('/', "."))
            else {
                continue;
            };

            index.config.add(&file_key, Some("config file".into()), &path, Position::new(0, 0), 0);
            let masked = php::mask_comments(&text);
            let coverage = crate::config_arrays::coverage(&masked, &file_key, None);
            let (leaves, complete) = (coverage.leaves, coverage.complete);
            if !complete {
                index.config_incomplete.insert(path.clone());
                if !coverage.global { index.config_local_arrays.insert(path.clone(), coverage.incomplete_arrays); }
            }
            let lines = Lines::new(&text);
            for leaf in leaves {
                if !leaf.array && !index.config.contains_key(&leaf.path) {
                    let source = masked[leaf.expression.clone()].trim();
                    let mut preview: String = source.chars().take(512).collect();
                    if source.chars().count() > 512 { preview.push('…'); }
                    if preview.is_empty() { preview = "Expression unavailable".into(); }
                    index.config_expressions.insert(leaf.path.clone(), preview);
                }
                let detail = if leaf.array { Some("array".to_owned()) } else { leaf.value };
                index.config.add(&leaf.path, detail, &path, lines.position(leaf.offset), lines.width(leaf.offset, leaf.width));
            }
        }

        index.index_translations(&relative, &files, &|path,index|read("translations",path,index));

        for file in [".env.example", ".env"] {
            let path = format!("{root}/{file}");
            if let Some(text) = read("source-5",&path, &mut index) {
                env_keys(&path, &text, &mut index.env);
                env_values(&text, &mut index.env_values);
            }
        }

        for file in ["bootstrap/app.php", "app/Http/Kernel.php"] {
            let path = format!("{root}/{file}");
            if let Some(text) = read("source-6",&path, &mut index) {
                index.middleware.retain(|_,entry|!entry.path.is_empty());
                middleware_aliases(&path, &text, &mut index.middleware);
            }
        }
        for name in BUILT_IN_MIDDLEWARE {
            index.middleware.add(name, Some("framework".into()), "", Position::new(0, 0), 0);
        }

        let public = host::setting_string("publicDirectory").filter(|d| !d.is_empty()).unwrap_or_else(|| "public".into());
        let public = public.trim_matches('/').to_owned();
        for path in files(&[format!("{public}/**/*")]) {
            let name = relative(&path).trim_start_matches(&format!("{public}/")).to_owned();
            if !name.ends_with(".php") {
                index.assets.add(&name, None, &path, Position::new(0, 0), 0);
            }
        }

        for path in files(&globs(&["app/View/Components/**/*.php"])) {
            let Some(text) = read("source-7",&path, &mut index) else { continue };
            let relative_path = relative(&path);
            let inside = relative_path.trim_start_matches("app/View/Components/");
            let lines = Lines::new(&text);
            if let Some(class) = controllers::parse(inside, &php::mask_comments(&text)) {
                let short = class.class.rsplit('\\').next().map_or(0, str::len);
                let tag=tag_name(inside);
                if index.components.get(&tag).is_some_and(|entry|entry.path.ends_with(".blade.php")){index.components.remove(&tag);}
                index.components.add(&tag, None, &path, lines.position(class.class_offset), short);
            }
        }
        // Class components win over anonymous ones of the same name.
        for path in files(&globs(&["resources/views/components/**/*.blade.php"])) {
            let relative_path = relative(&path);
            let inside = relative_path.trim_start_matches("resources/views/components/").trim_end_matches(".blade.php");
            let name = inside.replace('/', ".");
            let name = name.strip_suffix(".index").unwrap_or(&name);
            index.components.add(name, None, &path, Position::new(0, 0), 0);

            if let Some((parent, last)) = name.rsplit_once('.') {
                if parent.rsplit('.').next() == Some(last) {
                    index.components.add(parent, None, &path, Position::new(0, 0), 0);
                }
            }
        }

        if index.metadata.package_enabled("livewire/livewire", true) {
        for directory in ["app/Livewire", "app/Http/Livewire"] {
            for path in files(&[format!("{directory}/**/*.php")]) {
                let Some(text) = read("source-8",&path, &mut index) else { continue };
                let relative_path = relative(&path);
                let inside = relative_path.trim_start_matches(&format!("{directory}/"));
                let lines = Lines::new(&text);
                if let Some(class) = controllers::parse(inside, &php::mask_comments(&text)) {
                    let short = class.class.rsplit('\\').next().map_or(0, str::len);
                    let tag=tag_name(inside);if index.livewire.get(&tag).is_some_and(|entry|entry.path.ends_with(".blade.php")){index.livewire.remove(&tag);}
                    index.livewire.add(&tag, None, &path, lines.position(class.class_offset), short);
                }
            }
        }

        if !index.metadata.livewire_namespaces.is_empty() {
            let composer_path=format!("{root}/composer.json");
            let composer=if host::project_of(&composer_path).and_then(|p|p.root().ok()).as_deref()==Some(root.as_str()){session.snapshot(project,"composer.json").unwrap_or_default()}else{String::new()};
            let roots=if composer.is_empty(){vec![eloquent_roots::Root{prefix:"App\\".into(),directory:"app".into()}]}else{match eloquent_roots::parse(&composer){Ok(roots) if roots.is_empty()=>vec![eloquent_roots::Root{prefix:"App\\".into(),directory:"app".into()}],Ok(roots)=>roots,Err(())=>Vec::new()}};
            for mapping in index.metadata.livewire_namespaces.clone() {
                if let Some(namespace)=&mapping.class_namespace {
                    let mut matched=false;
                    for source_root in &roots {
                        let directory=if namespace==source_root.prefix.trim_end_matches('\\'){Some(source_root.directory.clone())}
                            else if let Some(tail)=namespace.strip_prefix(&source_root.prefix){Some(format!("{}/{}",source_root.directory,tail.replace('\\',"/")))}
                            else if source_root.prefix.starts_with(&format!("{namespace}\\")){Some(source_root.directory.clone())}else{None};
                        let Some(directory)=directory else{continue;};matched=true;index.dependency_roots.insert(directory.clone());
                        for path in files(&[format!("{directory}/**/*.php")]) {
                            let Some(text)=read(&format!("livewire:{}:{namespace}:{}",mapping.source,mapping.tag_prefix),&path,&mut index)else{continue;};
                            let Some(full)=crate::eloquent_members::declaration(&text)else{continue;};
                            if !eloquent_roots::matches(&roots,&relative(&path),&full){continue;}
                            let Some(inside)=full.strip_prefix(&format!("{namespace}\\"))else{continue;};
                            let name=tag_name(&format!("{}.php",inside.replace('\\',"/")));
                            let name=if mapping.tag_prefix.is_empty(){name}else{format!("{}::{name}",mapping.tag_prefix)};
                            let stem=file_name(&path).trim_end_matches(".php");
                            if let Some(code)=crate::package_facts::code(&text) {
                                let offset=code.match_indices("class").find_map(|(at,_)|{
                                    if at>0 && php::is_identifier(code.as_bytes()[at-1]){return None;}
                                    let after=at+5;let tail=code.get(after..)?;if !tail.starts_with(char::is_whitespace){return None;}
                                    let start=code.len()-tail.trim_start().len();
                                    (code[start..].starts_with(stem)&&!code.as_bytes().get(start+stem.len()).is_some_and(|b|php::is_identifier(*b))).then_some(start)
                                });
                                if let Some(offset)=offset{let lines=Lines::new(&text);if index.livewire.get(&name).is_some_and(|entry|entry.path.ends_with(".blade.php")){index.livewire.remove(&name);}index.livewire.add(&name,Some("ide.json Livewire namespace".into()),&path,lines.position(offset),stem.len());}
                            }
                        }
                    }
                    if !matched{index.metadata.issues.push(format!("{}: Livewire classNamespace {namespace} has no owned Composer PSR-4 mapping",mapping.source));}
                }
            }
            // Class mappings are inserted before view-only components.
            for mapping in index.metadata.livewire_namespaces.clone() {
                if let Some(directory)=mapping.view_path {
                    for path in files(&[format!("{directory}/**/*.blade.php")]) {
                        let rel=relative(&path);let Some(inside)=rel.strip_prefix(&format!("{directory}/"))else{continue;};
                        let name=inside.trim_end_matches(".blade.php").replace('/',".");let name=if mapping.tag_prefix.is_empty(){name}else{format!("{}::{name}",mapping.tag_prefix)};
                        index.livewire.add(&name,Some("ide.json Livewire view namespace".into()),&path,Position::new(0,0),0);
                    }
                }
            }
        }
        } // Livewire package enabled
        if index.metadata.package_enabled("inertiajs/inertia-laravel", true) {
            let mut inertia_roots=index.metadata.inertia_paths.clone();
            if !index.metadata.inertia_ignore_settings {inertia_roots.extend(INERTIA_PAGE_ROOTS.iter().map(|path|crate::metadata::PageRoot{path:(*path).into(),prefix:String::new(),suffix:String::new()}));}
            for page_root in &inertia_roots {
                for extension in INERTIA_PAGE_EXTENSIONS {
                    for path in files(&[format!("{}/**/*.{extension}",page_root.path)]) {
                        let relative=relative(&path);let Some(name)=relative.strip_prefix(&format!("{}/",page_root.path)).and_then(|name|name.strip_suffix(&format!(".{extension}")))else{continue;};
                        let name=format!("{}{name}{}",page_root.prefix,page_root.suffix);
                        index.inertia_pages.add(&name,None,&path,Position::new(0,0),0);
                    }
                }
            }
        }

        for path in files(&globs(&["database/factories/**/*.php", "database/seeders/**/*.php"])) {
            let _ = read("source-10",&path, &mut index);
            if !relative(&path).starts_with("database/factories/") {
                continue;
            }
            if let Some(stem) = file_name(&path).strip_suffix(".php") {
                index.factories.insert(stem.to_owned());
            }
        }

        for extension in VITE_EXTENSIONS {
            for path in files(&[format!("resources/**/*.{extension}")]) {
                let name = relative(&path);
                if !name.starts_with("resources/views/") {
                    index.vite.add(&name, None, &path, Position::new(0, 0), 0);
                }
            }
        }

        // Vite config and manifests augment the source inventory; never execute JS.
        let mut manifests=session.manifests.borrow_mut();
        manifests.extend(["public/build/manifest.json".to_owned(),"public/build/.vite/manifest.json".to_owned()]);
        for config_path in ["vite.config.js", "vite.config.ts", "vite.config.mjs", "vite.config.mts"] {
            let absolute = format!("{root}/{config_path}");
            if host::project_of(&absolute).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {continue;}
            let Some(source)=read("source-11",&absolute,&mut index) else {continue;};
            let config=crate::vite::config(&source);
            for (input,_) in config.inputs {
                index.dependency_files.insert(input.clone());
                let absolute=format!("{root}/{input}");
                if crate::vite::path(&input) && host::project_of(&absolute).and_then(|p|p.root().ok()).as_deref()==Some(root.as_str()) && project.exists(&input).unwrap_or(false) {
                    index.vite.add(&input,Some("Vite configured input".into()),&absolute,Position::new(0,0),0);
                }
            }
            let directory=config.build_directory.unwrap_or_else(||"build".into());
            if crate::vite::path(&directory) {
                if let Some(manifest)=config.manifest.filter(|p|crate::vite::path(p)) {manifests.insert(format!("public/{directory}/{manifest}"));}
                else {manifests.insert(format!("public/{directory}/manifest.json"));manifests.insert(format!("public/{directory}/.vite/manifest.json"));}
            }
        }
        for manifest_path in manifests.iter().cloned().collect::<Vec<_>>() {
            index.dependency_files.insert(manifest_path.clone());
            let absolute=format!("{root}/{manifest_path}");
            if host::project_of(&absolute).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {continue;}
            let Some(source)=read("source-12",&absolute,&mut index) else {continue;};
            if let Ok(assets)=crate::vite::manifest(&source) {for asset in assets {
                let target=format!("{root}/{}",asset.source);
                let owned=host::project_of(&target).and_then(|p|p.root().ok()).as_deref()==Some(root.as_str()) && project.exists(&asset.source).unwrap_or(false);
                index.vite.add(&asset.source,Some(format!("Vite manifest → {}",asset.output)),if owned{&target}else{&absolute},Position::new(0,0),0);
            }}
        }

        for path in files(&globs(&["app/Providers/**/*.php"])) {
            let Some(text) = read("source-13",&path, &mut index) else { continue };
            let masked = php::mask_comments(&text);
            let lines = Lines::new(&text);
            if index.metadata.package_enabled("laravel/passport", true) {
                for (name, entry) in crate::package_facts::passport_names(&path, &text) { index.metadata.passport.insert(name, entry); }
            }
            index.open_components |=
                ["anonymousComponentPath(", "anonymousComponentNamespace(", "componentNamespace(", "Blade::components("]
                    .iter()
                    .any(|call| masked.contains(call));
            for (at, _) in masked.match_indices("Blade::component(") {
                let arguments = php::arguments(&masked, at + "Blade::component".len());
                let literals: Vec<_> = arguments.iter().take(2).filter_map(|range| php::literal(&masked, range.clone())).collect();
                for (alias, offset) in &literals {
                    index.components.add(alias, Some("Blade::component".into()), &path, lines.position(*offset), alias.len());
                }

                if !literals.is_empty() {
                    continue;
                }

                let class =
                    arguments.first().filter(|_| arguments.len() == 1).and_then(|range| php::class_constant(&masked, range.clone()));
                match class {
                    Some(class) => {
                        let basename = class.rsplit('\\').next().unwrap_or(class);
                        index.components.add(&kebab(basename), Some("Blade::component".into()), &path, Position::new(0, 0), 0);
                    }
                    None => index.open_components = true,
                }
            }
            index.open_livewire |=
                ["Livewire::component(", "Livewire::componentNamespace(", "Livewire::addNamespace(", "Livewire::addLocation("]
                    .iter()
                    .any(|call| masked.contains(call));
            for (at, _) in masked.match_indices("Gate::define(") {
                let first = php::arguments(&masked, at + "Gate::define".len()).into_iter().next();
                if let Some((ability, offset)) = first.and_then(|range| php::literal(&masked, range)) {
                    index.abilities.add(ability, Some("Gate::define".into()), &path, lines.position(offset), lines.width(offset, ability.len()));
                }
            }
        }

        for name in &index.metadata.directives {
            index.directives.add(name, Some("ide.json directive".into()), "", Position::new(0, 0), 0);
        }

        for alias in CORE_BINDINGS {
            index.bindings.add(alias, Some("framework".into()), "", Position::new(0, 0), 0);
        }

        for path in files(&globs(&["app/**/*.php"])) {
            let Some(text) = read("source-14",&path, &mut index) else { continue };
            let masked = php::mask_comments(&text);
            let lines = Lines::new(&text);

            if let Some(stem) = file_name(&path).strip_suffix(".php")
                && let Some((fqcn, offset)) = declared_class(&masked, stem)
            {
                index.bindings.add(&fqcn, Some("class".into()), &path, lines.position(offset), stem.len());
                if relative(&path).starts_with("app/Providers/") {
                    index.providers.add(&fqcn, None, &path, lines.position(offset), stem.len());
                }
            }

            for call in ["->bind(", "->singleton(", "->scoped(", "->instance(", "->alias(", "::bind(", "::singleton("] {
                for (at, _) in masked.match_indices(call) {
                    let first = php::arguments(&masked, at + call.len() - 1).into_iter().next();
                    if let Some((name, offset)) = first.and_then(|range| php::literal(&masked, range)) {
                        index.bindings.add(
                            &name.replace("\\\\", "\\"),
                            Some(call.trim_matches(['-', '>', ':', '(']).to_owned()),
                            &path,
                            lines.position(offset),
                            lines.width(offset, name.len()),
                        );
                    }
                }
            }
            for (at, _) in masked.match_indices("Blade::directive(") {
                let first = php::arguments(&masked, at + "Blade::directive".len()).into_iter().next();
                if let Some((name, offset)) = first.and_then(|range| php::literal(&masked, range)) {
                    index.directives.add(name, Some("Blade::directive".into()), &path, lines.position(offset), name.len());
                }
            }
        }

        for path in files(&globs(&["app/Policies/**/*.php"])) {
            let Some(text) = read("source-15",&path, &mut index) else { continue };
            let relative_path = relative(&path);
            let inside = relative_path.trim_start_matches("app/Policies/");
            let lines = Lines::new(&text);
            let Some(policy) = controllers::parse(inside, &php::mask_comments(&text)) else { continue };
            for (method, offset) in policy.methods.iter().filter(|(m, _)| !matches!(m.as_str(), "before" | "after")) {
                index.abilities.add(method, Some(policy.class.clone()), &path, lines.position(*offset), method.len());
            }
        }

        let mut schema_patterns = vec!["database/schema/**/*.sql".to_owned()];
        schema_patterns.extend(index.metadata.schema_directories.iter().map(|p|format!("{p}/**/*.sql")));
        schema_patterns.extend(index.metadata.schema_files.clone());
        for path in files(&schema_patterns) {
            if host::project_of(&path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) { continue; }
            if let Some(text)=read("source-16",&path,&mut index) {
                migrations::apply_sql(&mut index.columns,&text,&path);
                let issues=migrations::sql_issues(&text);
                index.metadata.issues.extend(issues.iter().map(|(offset,message)|format!("{path}:{offset}: {message}")));
                index.sql_sources.insert(path.clone(),serde_json::json!({"constraints":migrations::sql_constraints(&text),"issues":issues.iter().map(|(offset,message)|serde_json::json!({"offset":offset,"message":message})).collect::<Vec<_>>()}));
            }
        }
        for path in files(&globs(&["database/migrations/**/*.php"])) {
            let Some(text) = read("source-17",&path, &mut index) else { continue };
            migrations::apply_custom(&mut index.columns, &php::mask_comments(&text), &path, &index.metadata.column_methods);
        }

        let mut eloquent_seen = session.eloquent_seen.borrow_mut();
        for path in files(&globs(&["app/Enums/**/*.php"])) {
            if let Some(text) = read("source-18",&path, &mut index) { eloquent_seen.insert(path.clone()); cache_declarations(index,&text,&path,&relative(&path),&declaration_roots,4); }
        }

        for path in files(&globs(&["app/Traits/**/*.php","app/Concerns/**/*.php","app/Models/Concerns/**/*.php"])) {
            if let Some(text)=read("source-19",&path,&mut index){eloquent_seen.insert(path.clone());cache_declarations(index,&text,&path,&relative(&path),&declaration_roots,1);}
        }
        for path in files(&globs(&["app/Casts/**/*.php","app/ValueObjects/**/*.php","app/Data/**/*.php"])) {
            if let Some(text)=read("source-20",&path,&mut index){eloquent_seen.insert(path.clone());cache_declarations(index,&text,&path,&relative(&path),&declaration_roots,2);}
        }
        let mut model_globs = globs(&["app/Models/**/*.php", "app/*.php"]);
        model_globs.extend(
            comma_separated("modelDirectories")
                .into_iter()
                .filter(|d| !d.contains("..") && !d.starts_with('/'))
                .map(|d| format!("{d}/**/*.php")),
        );
        for path in files(&model_globs) {
            let Some(text) = read("source-21",&path, &mut index) else { continue };
            eloquent_seen.insert(path.clone());
            cache_declarations(index,&text,&path,&relative(&path),&declaration_roots,3);
            let relative_path = relative(&path);
            let inside = relative_path.trim_start_matches("app/").trim_start_matches("Models/");
            let masked = php::mask_comments(&text);
            let Some(model) = controllers::parse(inside, &masked) else { continue };

            let short = model.class.rsplit('\\').next().unwrap_or(&model.class).to_owned();
            let table = explicit_table(&masked).unwrap_or_else(|| table_name(&short));
            if masked.contains("newFactory") || masked.contains("UseFactory") {
                index.factory_overrides.insert(short.clone());
            }
            let (relations, scopes) = model_members(&masked);
            index.relations.insert(short.clone(), relations);
            index.scopes.insert(short.clone(), scopes);
            index.model_files.insert(path, table.clone());
            index.models.insert(short, table);
        }

        if let Some(composer) = session.snapshot(project,"composer.json") {
            index.open_components |= COMPONENT_PACKAGES.iter().any(|package| composer.contains(package) && (*package!="filament/"||index.metadata.package_enabled("filament/filament",true)));
            match eloquent_roots::parse(&composer) {
                Ok(roots)=>{
                    let mut paths=BTreeSet::new();
                    for source_root in &roots {
                        let found=files(&[format!("{}/**/*.php",source_root.directory)]);
                        if found.len()>=MAX_FILES as usize{index.partial=true;}
                        for path in found {if paths.len()>=MAX_FILES as usize{index.partial=true;break;}paths.insert(path);}
                    }
                    for path in paths {
                        if eloquent_seen.contains(&path){session.retire(&format!("source-23\0{path}"));continue;}
                        if !crate::owns_current_path(project,&path){continue;}
                        let Some(text)=read("source-23",&path,&mut index)else{continue};
                        let Some(full)=crate::eloquent_members::declaration(&text)else{continue};
                        if !eloquent_roots::matches(&roots,&relative(&path),&full){continue;}
                        cache_declarations(index,&text,&path,&relative(&path),&declaration_roots,7);
                    }
                },
                Err(())=>index.partial=true,
            }
        }
        drop(eloquent_seen);
        drop(manifests);
        if session.pending(){return;}
        session.mark_done("sources:complete");
        }
        macro_rules! final_job {($key:literal,$body:block)=>{if !session.done($key){if session.heavy($key){$body}return;}};}
        if !session.done("final:traits"){session.semantics(index,"final:traits");return;}
        final_job!("final:packages",{crate::package_integrations::apply(project,index);crate::eloquent_members::package_queries::apply(project,index);});
        if !session.done("final:relations"){session.semantics(index,"final:relations");return;}
        final_job!("final:model-tables",{
            crate::eloquent_members::apply_model_tables(&mut index.eloquent_models,&index.metadata);
            let inventory=crate::eloquent_members::model_inventory(&index.eloquent_models);let mut counts=BTreeMap::<String,usize>::new();
            for(full,_,_)in &inventory{*counts.entry(full.rsplit('\\').next().unwrap_or(full).to_owned()).or_default()+=1;}
            for(full,path,table)in inventory{let short=full.rsplit('\\').next().unwrap_or(&full).to_owned();index.model_files.insert(path,table.clone());if counts.get(&short)==Some(&1){index.models.insert(short,table);}else{index.models.remove(&short);}}
        });
        final_job!("final:enum-config",{crate::enum_config_keys::apply_project(project,index,&root,session.source_cache.borrow().sources(),&|path|crate::owns_current_path(project,path));});
        final_job!("final:parent-prepare",{crate::metadata::prepare_parent_configs(project,&mut index.metadata,&index.packages.provider_sources,&index.packages.roots);});
        final_job!("final:class-cache",{crate::metadata::complete_class_cache(project,&mut index.metadata,&index.packages.roots);});
        final_job!("final:parent-finish",{crate::metadata::finish_parent_configs(project,&mut index.metadata);crate::metadata::merge_parent_config_inventory(project,index);});
        final_job!("final:forwarded",{crate::eloquent_members::finish_metadata(&mut index.eloquent_models,&mut index.eloquent_objects,&mut index.metadata);});
        final_job!("final:metadata",{
            for rule in VALIDATION_RULES{index.rules.add(rule,Some("validation rule".into()),"",Position::new(0,0),0);}
            for(name,entry)in &index.metadata.additional_gates{index.abilities.entry(name.clone()).or_insert_with(||entry.clone());}
            for(name,entry)in &index.metadata.additional_middleware{index.middleware.entry(name.clone()).or_insert_with(||entry.clone());}
            crate::components::apply_metadata(project,index);
        });
        final_job!("final:publish",{index.renders.sort_by(|a,b|(&a.path,a.at.line,a.at.character).cmp(&(&b.path,b.at.line,b.at.character)));});
    }

    /// Keys of `lang/<locale>/<file>.php` (as `file.key`) and `lang/<locale>.json`.
    fn index_translations(
        &mut self,
        relative: &dyn Fn(&str) -> String,
        files: &dyn Fn(&[String]) -> Vec<String>,
        read: &dyn Fn(&str, &mut Index) -> Option<String>,
    ) {
        let main = host::setting_string("locale").filter(|l| !l.is_empty()).unwrap_or_else(|| "en".into());

        let mut found: Vec<(String, String, bool, String)> = Vec::new();
        for lang_root in LANG_ROOTS {
            for path in files(&[format!("{lang_root}/**/*.php"), format!("{lang_root}/**/*.json")]) {
                let relative_path = relative(&path);
                let Some(inside) = relative_path.strip_prefix(&format!("{lang_root}/")) else { continue };
                let parts: Vec<&str> = inside.split('/').collect();

                // `vendor/<package>/<locale>/<file>.php`, `<locale>/<file>.php`, `<locale>.json`
                let (namespace, rest) = match parts.as_slice() {
                    ["vendor", package, rest @ ..] => (format!("{package}::"), rest.to_vec()),
                    rest => (String::new(), rest.to_vec()),
                };
                let (locale, key) = match rest.as_slice() {
                    [json] if json.ends_with(".json") => (json.trim_end_matches(".json").to_owned(), String::new()),
                    [locale, file @ ..] if !file.is_empty() => ((*locale).to_owned(), file.join("/").trim_end_matches(".php").to_owned()),
                    _ => continue,
                };

                let json = path.ends_with(".json");
                found.push((locale, format!("{namespace}{key}"), json, path));
            }
        }
        found.sort_by_key(|(locale, _, _, path)| (*locale != main, locale.clone(), path.clone()));

        for (locale, key, json, path) in found {
            let Some(text) = read(&path, self) else { continue };
            let lines = Lines::new(&text);

            if json {
                let values = serde_json::from_str::<serde_json::Value>(&text).ok();
                for (name, offset) in json_keys(&text) {
                    let value = format!("[{locale}]");
                    let end = php::string_end(text.as_bytes(), offset - 1);
                    let width = lines.width(offset, end.saturating_sub(offset + 1));
                    self.translations.add(&name, Some(value), &path, lines.position(offset), width);
                    let translated = values.as_ref().and_then(|v| v.get(&name)).and_then(|v| v.as_str()).map(str::to_owned);
                    self.translations_by_locale.entry(locale.clone()).or_default().add(&name, translated, &path, lines.position(offset), width);
                }
                continue;
            }

            let masked = php::mask_comments(&text);
            let Some(open) = php::returned_array(&masked) else { continue };
            self.translation_files.entry(key.clone()).or_default().push(path.clone());

            let mut leaves = Vec::new();
            php::array_paths(&masked, open, &key, &mut leaves, 0);
            for leaf in leaves {
                self.translations_by_locale.entry(locale.clone()).or_default().add(&leaf.path, leaf.value.clone(), &path, lines.position(leaf.offset), lines.width(leaf.offset, leaf.width));
                let detail = leaf.value.map(|v| format!("{v} [{locale}]"));
                self.translations.add(&leaf.path, detail, &path, lines.position(leaf.offset), lines.width(leaf.offset, leaf.width));
            }
        }
    }

    /// The view named by a view file.
    pub fn view_of(&self, path: &str) -> Option<&str> {
        self.views.iter().find(|(_, entry)| entry.path == path).map(|(name, _)| name.as_str())
    }

    /// Resolve literal fallback layouts against the indexed source inventory.
    pub fn layout_parent<'a>(&self, facts:&'a blade::Facts)->Option<&'a str> {
        facts.extends.as_deref().or_else(||facts.extends_first.iter().find(|name|self.views.contains_key(name.as_str())).map(String::as_str))
    }

    /// The views a view extends, nearest first.
    pub fn parents<'a>(&'a self, view: &'a str) -> Vec<&'a str> {
        let mut parents = Vec::new();
        let mut current = view;

        // Layouts are a few levels deep; the limit stops cycles.
        for _ in 0..10 {
            let Some(parent) = self.blade.get(current).and_then(|facts| self.layout_parent(facts)) else { break };
            if parents.contains(&parent) {
                break;
            }
            parents.push(parent);
            current = parent;
        }

        parents
    }

    /// Views that extend or include `view`.
    pub fn users_of(&self, view: &str) -> Vec<&str> {
        let uses = |facts: &blade::Facts| facts.extends.as_deref() == Some(view) || facts.extends_first.iter().any(|name|name==view) || facts.includes.iter().any(|i| i == view);
        self.blade.iter().filter(|(_, facts)| uses(facts)).map(|(name, _)| name.as_str()).collect()
    }

    /// The public methods of a controller named by its class or short name.
    pub fn controller_methods(&self, class: &str) -> Option<&Names> {
        let short = class.rsplit('\\').next().unwrap_or(class);
        self.methods.iter().find(|(known, _)| known.rsplit('\\').next() == Some(short)).map(|(_, methods)| methods)
    }
}

/// View renders of a PHP file: `view('x')`, `->view('x')`, `Route::view`.
fn renders(path: &str, text: &str) -> Vec<Render> {
    let lines = Lines::new(text);
    let stem = file_name(path).trim_end_matches(".php");

    refs::references("php", text)
        .into_iter()
        .filter(|r| r.kind == Kind::View && r.closed)
        .map(|reference| {
            let method = text[..reference.start].rfind("function ").map(|f| {
                let name = &text[f + "function ".len()..];
                &name[..name.bytes().position(|b| !php::is_identifier(b)).unwrap_or(name.len())]
            });
            let caller = match method.filter(|m| !m.is_empty()) {
                Some(method) => format!("{stem}::{method}"),
                None => stem.to_owned(),
            };

            Render {
                view: text[reference.start..reference.end].to_owned(),
                caller,
                path: path.to_owned(),
                at: lines.position(reference.start),
            }
        })
        .collect()
}

/// `KEY=value` lines of an env file; values are not read.
fn env_values(text: &str, values: &mut BTreeMap<String, String>) {
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some((key, raw)) = trimmed.strip_prefix("export ").unwrap_or(trimmed).split_once('=') else { continue };
        let key = key.trim();
        let is_key = !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.');
        if is_key && !trimmed.starts_with('#') {
            values.entry(key.to_owned()).or_insert_with(|| ui_kit::env_value::display_value(key, raw));
        }
    }
}

fn env_keys(path: &str, text: &str, names: &mut Names) {
    for (line_number, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let key_part = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        let Some((key, _)) = key_part.split_once('=') else { continue };
        let key = key.trim();

        let is_key = !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.');
        if is_key && !trimmed.starts_with('#') {
            let column = (line.len() - key_part.len()) as u32;
            names.add(key, Some(file_name(path).to_owned()), path, Position::new(line_number as u32, column), key.len());
        }
    }
}

/// Aliases registered in `bootstrap/app.php` (`$middleware->alias([...])`)
/// or `app/Http/Kernel.php` (`$routeMiddleware`, `$middlewareAliases`).
fn middleware_aliases(path: &str, text: &str, names: &mut Names) {
    let masked = php::mask_comments(text);
    let lines = Lines::new(text);
    let mut arrays = Vec::new();

    for (at, _) in masked.match_indices("alias(") {
        let open = at + "alias".len();
        arrays.extend(php::arguments(&masked, open).into_iter().next().and_then(|first| php::array_open(&masked, first)));
    }

    for property in ["$routeMiddleware", "$middlewareAliases"] {
        for (at, _) in masked.match_indices(property) {
            let rest = masked[at + property.len()..].trim_start();
            if let Some(after) = rest.strip_prefix('=') {
                let start = masked.len() - after.len();
                arrays.extend(php::array_open(&masked, start..masked.len()));
            }
        }
    }

    for open in arrays {
        for item in php::items(&masked, open) {
            let Some((alias, offset)) = item.key.and_then(|k| php::literal(&masked, k)) else { continue };
            let class = php::class_constant(&masked, item.value.clone()).map(str::to_owned);
            names.add(alias, class, path, lines.position(offset), alias.len());
        }
    }
}

/// `"key": "value"` pairs of a flat JSON translation file, with the offset of each key.
fn json_keys(text: &str) -> Vec<(String, usize)> {
    let bytes = text.as_bytes();
    let mut keys = Vec::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }

        let end = php::string_end(bytes, i);
        let after = text[end..].trim_start();
        let is_key = after.starts_with(':') && end > i + 1;

        if is_key && bytes[end - 1] == b'"' {
            let key = text[i + 1..end - 1].replace("\\\"", "\"").replace("\\/", "/");
            keys.push((key, i + 1));
            // Skip the value, which may itself be a string.
            let value = after[1..].trim_start();
            let value_start = text.len() - value.len();
            i = if value.starts_with('"') { php::string_end(bytes, value_start) } else { value_start };
            continue;
        }

        i = end.max(i + 1);
    }

    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_views() {
        assert_eq!(view_name("posts/index.blade.php").as_deref(), Some("posts.index"));
        assert_eq!(view_name("welcome.php").as_deref(), Some("welcome"));
        assert_eq!(view_name("vendor/mail/html/button.blade.php").as_deref(), Some("mail::html.button"));
        assert_eq!(view_name("readme.md"), None);
    }

    #[test]
    fn names_component_tags() {
        assert_eq!(tag_name("Alert.php"), "alert");
        assert_eq!(tag_name("Forms/InputField.php"), "forms.input-field");
        assert_eq!(tag_name("Counter2Button.php"), "counter2-button");
    }

    #[test]
    fn names_model_tables() {
        assert_eq!(table_name("User"), "users");
        assert_eq!(table_name("UserProfile"), "user_profiles");
        assert_eq!(table_name("Category"), "categories");
        assert_eq!(table_name("Day"), "days");
        assert_eq!(table_name("Address"), "addresses");
        assert_eq!(explicit_table("class A { protected $table = 'legacy_users'; }").as_deref(), Some("legacy_users"));
        assert_eq!(explicit_table("class A { public $timestamps = false; }"), None);
    }

    #[test]
    fn reads_env_files() {
        let mut names = Names::new();
        env_keys("/p/.env.example", "# c\nAPP_NAME=Laravel\nexport DB_HOST=x\nnot a key=1\n  QUEUE=sync\n", &mut names);
        let found: Vec<_> = names.iter().map(|(k, e)| (k.as_str(), e.at.line, e.at.character)).collect();
        assert_eq!(found, [("APP_NAME", 1, 0), ("DB_HOST", 2, 7), ("QUEUE", 4, 2)]);
    }

    #[test]
    fn reads_middleware_aliases() {
        let mut names = Names::new();
        let app = "->withMiddleware(function (Middleware $middleware) {\n    $middleware->alias([\n        'admin' => AdminMiddleware::class,\n        'role' => \\Spatie\\Role::class,\n    ]);\n})";
        middleware_aliases("/p/bootstrap/app.php", app, &mut names);
        middleware_aliases(
            "/p/app/Http/Kernel.php",
            "protected $routeMiddleware = [\n 'auth' => \\App\\Http\\Middleware\\Authenticate::class,\n];",
            &mut names,
        );
        let found: Vec<_> = names.iter().map(|(k, e)| (k.as_str(), e.detail.as_deref())).collect();
        assert_eq!(
            found,
            [("admin", Some("AdminMiddleware")), ("auth", Some("App\\Http\\Middleware\\Authenticate")), ("role", Some("Spatie\\Role"))]
        );
    }

    #[test]
    fn reads_json_translation_keys() {
        let text = "{\n  \"Welcome\": \"Bienvenue\",\n  \"Say \\\"hi\\\"\": \"Dis \\\"salut\\\"\",\n  \"Key: value\": \"x\"\n}";
        let keys: Vec<_> = json_keys(text).into_iter().map(|(k, at)| (k, text[at..].chars().next().unwrap())).collect();
        assert_eq!(keys, [("Welcome".into(), 'W'), ("Say \"hi\"".into(), 'S'), ("Key: value".into(), 'K')]);
    }

    #[test]
    fn finds_renders_with_their_callers() {
        let text = "<?php\nclass PostController {\n    public function index()\n    {\n        return view('posts.index');\n    }\n}\n";
        let found = renders("/p/app/Http/Controllers/PostController.php", text);
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].view.as_str(), found[0].caller.as_str(), found[0].at),
            ("posts.index", "PostController::index", Position::new(4, 21))
        );
    }
    #[test]
    fn dynamic_source_token_widths_count_utf16_and_never_make_multiline_same_line_spans() {
        let source="😀 prefix hé😀\r\nnext";let at=source.find("hé😀").unwrap();let lines=Lines::new(source);assert_eq!(lines.width(at,"hé😀".len()),4);assert_eq!(lines.width(at,source.len()-at),0);assert_eq!(lines.width(at+1,1),0);assert_eq!(lines.width(source.len()+1,1),0);
    }

}

#[cfg(test)]
mod line_position_tests {
    use super::*;
    #[test]
    fn sparse_utf16_positions_match_sdk_across_lines_and_minified_unicode_keys() {
        let mut source=String::from("first\r\n😀é start ");
        let mut offsets=Vec::new();
        for n in 0..8192 { offsets.push(source.len());source.push_str(&format!("\"hé😀{n}\":\"value\",")); }
        source.push_str("\r\nlast😀");
        let lines=Lines::new(&source);
        for offset in offsets.into_iter().step_by(127).chain([0,7,source.len()]) {
            assert_eq!(lines.position(offset),openidea_plugin::text::position(&source,offset));
        }
        assert!(lines.utf16.len() < source.len()/128);
        assert!(lines.utf16.windows(2).all(|pairs| pairs[1].0-pairs[0].0 <= 259));
    }
    #[test]
    fn ascii_positions_need_no_utf16_prefix_table() {
        let source="first\r\nsecond\nlast";
        let lines=Lines::new(source);
        assert!(lines.utf16.is_empty());
        for offset in 0..=source.len(){assert_eq!(lines.position(offset),openidea_plugin::text::position(source,offset));}
    }
}
