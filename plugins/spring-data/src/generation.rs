//! Source-local repository creation. Only the host applies the atomic workspace edit.
use super::*;
use openidea_plugin::file_actions::{FileAction, FileActions, FileOperation};
use std::time::{Duration, Instant};

const MAX_TEXT: usize = 128 * 1024;
const ENTITIES: &[&str] = &[
    "jakarta.persistence.Entity",
    "javax.persistence.Entity",
    "org.springframework.data.mongodb.core.mapping.Document",
    "org.springframework.data.relational.core.mapping.Table",
];
#[derive(Clone)]
struct Entity {
    name: String,
    at: usize,
    package: String,
    id: String,
    jpa: bool,
    id_name: String,
    projection: Vec<(String,String)>,
    graph: Vec<String>,
}
#[derive(Clone)]
struct Pending {
    root: String,
    doc: Document,
    entity: Entity,
    repository: String,
}
#[derive(Default)]
pub(crate) struct Generation {
    next: u64,
    pending: BTreeMap<String, Pending>,
}

fn identifier(value: &str) -> bool {
    let mut chars = value.chars();
    value.len() <= 128 && chars.next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !"_ abstract as assert boolean break byte case catch char class companion const continue data default do double else enum extends false final finally float for fun if implements import in instanceof int interface is long native new null object open out override package permits private protected public record return sealed short static strictfp super switch synchronized this throw throws trait transient true try typealias typeof val var void volatile when while yield"
            .split_ascii_whitespace().any(|keyword| keyword == value)
}
fn path(value: &str) -> Option<String> {
    let value = value.replace('\\', "/");
    let absolute = value.starts_with('/')
        || (value.as_bytes().get(1) == Some(&b':') && value.as_bytes().get(2) == Some(&b'/') && value.as_bytes()[0].is_ascii_alphabetic());
    (absolute
        && value.len() <= 4096
        && !value.chars().any(char::is_control)
        && !value.split('/').any(|part| matches!(part, "." | ".."))
        && !value.contains(['*', '?', '[', ']', '{', '}']))
    .then_some(value)
}
fn inside(root: &str, file: &str) -> bool {
    file.starts_with(&format!("{}/", root.trim_end_matches('/')))
}
fn annotation_namespace(file: &JavaFile, text: &str, annotation: &java::Annotation) -> Option<String> {
    let tokens = java::tokens(&text[annotation.at..annotation.name_at + annotation.name.len()]);
    let mut names = &tokens[1..];
    if names.get(1).is_some_and(|token| token.s == ":") {
        if annotation.name != "Id" || !matches!(names[0].s, "field" | "get") {
            return None;
        }
        names = &names[2..];
    }
    let written: String = names.iter().map(|token| token.s).collect();
    if written.contains('.') {
        return Some(written);
    }
    if file.types.iter().any(|ty| ty.name == written) {
        return None;
    }
    let imports: Vec<_> =
        file.imports.iter().filter(|import| !import.is_static && import.path.rsplit('.').next() == Some(written.as_str())).collect();
    (imports.len() == 1).then(|| imports[0].path.clone())
}
fn scalar_id(file: &JavaFile, ty: &TypeRef, kotlin: bool) -> Option<String> {
    if !ty.args.is_empty() || ty.text.contains(['[', ']', '<', '>']) {
        return None;
    }
    let written = ty.text.trim();
    let written = if kotlin { written.strip_suffix('?').unwrap_or(written) } else { written };
    let java_primitive = match written {
        "byte" => Some("Byte"),
        "short" => Some("Short"),
        "int" => Some("Integer"),
        "long" => Some("Long"),
        "float" => Some("Float"),
        "double" => Some("Double"),
        "boolean" => Some("Boolean"),
        "char" => Some("Character"),
        _ => None,
    };
    if !kotlin && let Some(name) = java_primitive {
        return Some(format!("java.lang.{name}"));
    }
    let simple = written.rsplit('.').next()?;
    let builtin = ["Byte", "Short", "Integer", "Int", "Long", "Float", "Double", "Boolean", "Character", "Char", "String"];
    let namespace = if written.contains('.') {
        written.to_owned()
    } else {
        if file.types.iter().any(|ty| ty.name == simple) || file.imports.iter().any(|import| import.path.ends_with(".*")) {
            return None;
        }
        let imports: Vec<_> = file.imports.iter().filter(|import| import.path.rsplit('.').next() == Some(simple)).collect();
        if imports.len() > 1 {
            return None;
        }
        if let Some(import) = imports.first() {
            import.path.clone()
        } else if builtin.contains(&simple) && !(kotlin && matches!(simple, "Integer" | "Character")) {
            format!("{}.{simple}", if kotlin { "kotlin" } else { "java.lang" })
        } else {
            return None;
        }
    };
    if namespace == "java.util.UUID" || namespace == "java.math.BigInteger" {
        return Some(namespace);
    }
    let java_name = namespace.strip_prefix("java.lang.");
    let kotlin_name = namespace.strip_prefix("kotlin.");
    let canonical = java_name.or(kotlin_name)?;
    if !builtin.contains(&canonical)
        || (java_name.is_some() && matches!(canonical, "Int" | "Char"))
        || (kotlin_name.is_some() && matches!(canonical, "Integer" | "Character"))
    {
        return None;
    }
    if kotlin {
        Some(format!(
            "kotlin.{}",
            match canonical {
                "Integer" => "Int",
                "Character" => "Char",
                other => other,
            }
        ))
    } else if kotlin_name.is_none() && !matches!(canonical, "Int" | "Char") {
        Some(namespace)
    } else {
        None
    }
}
fn property_access(file:&JavaFile,ty:&java::TypeDecl,text:&str,kotlin:bool)->bool{
    !kotlin && ty.members.iter().any(|member|member.kind==java::MemberKind::Method && member.annotations.iter().any(|a|a.name=="Id"&&annotation_namespace(file,text,a).is_some_and(|full|matches!(full.as_str(),"jakarta.persistence.Id"|"javax.persistence.Id"))))
}
fn projection_fields(file:&JavaFile,ty:&java::TypeDecl,text:&str,kotlin:bool)->Vec<(String,String)> {
    if ty.annotations.iter().any(|a|a.name=="Access") {return Vec::new();}
    let property_access=property_access(file,ty,text,kotlin);
    let mut fields:BTreeMap<String,Option<String>>=BTreeMap::new();
    let mut add=|name:String,ty:&java::TypeRef,annotations:&[java::Annotation]| {
        if !identifier(&name) || annotations.iter().any(|a|a.name=="Transient" || annotation_namespace(file,text,a).is_none_or(|owner| !["jakarta.persistence","javax.persistence","org.springframework.data.annotation","org.springframework.data.mongodb.core.mapping","org.springframework.data.relational.core.mapping"].iter().any(|ns|owner==format!("{ns}.{}",a.name)))) {return;}
        let Some(mut scalar)=scalar_id(file,ty,kotlin) else{return;};
        if kotlin && ty.text.trim().ends_with('?') {scalar.push('?');}
        fields.entry(name).and_modify(|old|if old.as_ref()!=Some(&scalar){*old=None;}).or_insert(Some(scalar));
    };
    for member in &ty.members {
        if !kotlin && (property_access && member.kind!=java::MemberKind::Method || !property_access && member.kind==java::MemberKind::Method) {continue;}
        if member.modifiers.iter().any(|m|matches!(m.as_str(),"static"|"transient")) {continue;}
        let Some(value)=member.ty.as_ref() else{continue;};
        let name=if member.kind==java::MemberKind::Field {member.name.clone()}
        else if member.kind==java::MemberKind::Method && member.params.is_empty() && !member.modifiers.iter().any(|m|m=="private") {
            let Some(tail)=member.name.strip_prefix("get").or_else(||member.name.strip_prefix("is").filter(|_|matches!(value.text.as_str(),"boolean"|"Boolean"|"java.lang.Boolean"|"kotlin.Boolean"))) else{continue;};
            let mut chars=tail.chars();let Some(first)=chars.next().filter(|c|c.is_ascii_uppercase()) else{continue;};
            if chars.as_str().starts_with(char::is_uppercase) {tail.into()} else {format!("{}{}",first.to_ascii_lowercase(),chars.as_str())}
        } else {continue;};
        add(name,value,&member.annotations);
    }
    for component in &ty.components {add(component.name.clone(),&component.ty,&component.annotations);}
    if fields.len()>64 {return Vec::new();}
    fields.into_iter().filter(|(name,_)|bean_property_spelling(name)).filter_map(|(name,ty)|ty.map(|ty|(name,ty))).collect()
}

fn bean_property_spelling(name:&str)->bool{
    let mut chars=name.chars();let Some(first)=chars.next() else{return false;};let rest=chars.as_str();
    let upper=first.to_ascii_uppercase();let preferred=if rest.starts_with(|c:char|c.is_ascii_uppercase()){format!("{upper}{rest}")}else{format!("{}{rest}",upper.to_ascii_lowercase())};
    preferred==name
}
fn finder_method(name:&str)->Option<String>{
    if !name.bytes().all(|b|b.is_ascii_alphanumeric())||!bean_property_spelling(name){return None;}
    let mut chars=name.chars();let first=chars.next()?.to_ascii_uppercase();let property=format!("{first}{}",chars.as_str());
    for suffix in ["","Is"] {
        let method=format!("findBy{property}{suffix}");
        if let Some(parts)=crate::derived(&method) {if parts.len()==1 && parts[0].args==Some(1) && method.get(parts[0].from..parts[0].to)==Some(property.as_str()){return Some(method);}}
    }
    None
}


fn predicate_finder(fields:&[(String,String)],selected:&str,kotlin:bool)->Result<(String,Vec<(String,String)>),Error>{
    let mut names=std::collections::BTreeSet::new();let mut stems=Vec::new();let mut expected=Vec::new();let mut params=Vec::new();
    for selected in selected.split(',').map(str::trim) {
        if stems.len()==8{return Err("Choose at most eight source criteria.".into());}
        let (name,operator)=selected.split_once(':').unwrap_or((selected,"Equals"));
        let name=name.trim();let operator=operator.trim();
        let (_,ty)=fields.iter().find(|(key,_)|key==name&&finder_method(key).is_some()).ok_or("Choose displayed direct source properties.")?;
        if !names.insert(name){return Err("Choose each source property once.".into());}
        let simple=ty.trim_end_matches('?').rsplit('.').next().unwrap_or("");
        let numeric=matches!(simple,"Byte"|"Short"|"Integer"|"Int"|"Long"|"Float"|"Double"|"BigInteger");
        let (suffix,args)=match operator {
            "Equals" => (finder_method(name).ok_or(Error::Stale)?[6..].to_owned(),1),
            "Not"|"In"|"NotIn" => (format!("{}{}",capitalize(name),operator),1),
            "IsNull"|"IsNotNull" => (format!("{}{}",capitalize(name),operator),0),
            "True"|"False" if simple=="Boolean" => (format!("{}{}",capitalize(name),operator),0),
            "Like"|"NotLike"|"Containing"|"StartingWith"|"EndingWith" if simple=="String" => (format!("{}{}",capitalize(name),operator),1),
            "LessThan"|"LessThanEqual"|"GreaterThan"|"GreaterThanEqual"|"Between" if numeric => (format!("{}{}",capitalize(name),operator),if operator=="Between"{2}else{1}),
            _ => return Err("Unsupported operator for this source scalar type. Use Equals/Not/In/NotIn/IsNull/IsNotNull; String Like/NotLike/Containing/StartingWith/EndingWith; numeric comparisons/Between; Boolean True/False.".into()),
        };
        expected.push((capitalize(name),args));stems.push(suffix);
        for index in 0..args {
            let parameter=if args==2{format!("{name}{}",if index==0{"Lower"}else{"Upper"})}else{name.into()};
            let parameter_type=if matches!(operator,"In"|"NotIn"){format!("{}<{ty}>",if kotlin{"kotlin.collections.Collection"}else{"java.util.Collection"})}else{ty.clone()};
            params.push((parameter,parameter_type));
        }
    }
    let method=format!("findBy{}",stems.join("And"));
    let parts=crate::derived(&method).ok_or("The selected criteria cannot form a derived method.")?;
    if parts.len()!=expected.len()||parts.iter().zip(&expected).any(|(part,(name,args))|part.args!=Some(*args)||method.get(part.from..part.to)!=Some(name)){return Err("The complete method has an ambiguous derived-query spelling.".into());}
    let mut used=std::collections::BTreeSet::new();for(name,_)in &mut params {while !used.insert(name.clone()){name.push('_');}}
    Ok((method,params))
}
fn capitalize(name:&str)->String{let mut chars=name.chars();format!("{}{}",chars.next().unwrap().to_ascii_uppercase(),chars.as_str())}

fn graph_fields(file:&JavaFile,ty:&java::TypeDecl,text:&str,namespace:&str,kotlin:bool)->Vec<String> {
    if ty.annotations.iter().any(|a|a.name=="Access") {return Vec::new();}
    let property_access=property_access(file,ty,text,kotlin);
    let mut fields=std::collections::BTreeSet::new();
    for member in &ty.members {
        if !kotlin && (property_access && member.kind!=java::MemberKind::Method || !property_access && member.kind==java::MemberKind::Method) {continue;}
        if member.modifiers.iter().any(|m|matches!(m.as_str(),"static"|"transient")) {continue;}
        if member.annotations.iter().any(|a|matches!(a.name.as_str(),"Transient"|"Access")) {continue;}
        if !member.annotations.iter().any(|a|matches!(a.name.as_str(),"OneToOne"|"OneToMany"|"ManyToOne"|"ManyToMany"|"ElementCollection"|"Embedded") && annotation_namespace(file,text,a).as_deref()==Some(format!("{namespace}.{}",a.name).as_str())) {continue;}
        let name=if member.kind==java::MemberKind::Field {member.name.clone()}
            else if member.kind==java::MemberKind::Method && member.params.is_empty() && !member.modifiers.iter().any(|m|m=="private") {
                let Some(tail)=member.name.strip_prefix("get") else{continue;};let mut chars=tail.chars();let Some(first)=chars.next().filter(|c|c.is_ascii_uppercase()) else{continue;};
                if chars.as_str().starts_with(char::is_uppercase) {tail.into()} else {format!("{}{}",first.to_ascii_lowercase(),chars.as_str())}
            } else {continue;};
        if identifier(&name) {fields.insert(name);}
    }
    for field in &ty.components {
        if identifier(&field.name) && field.annotations.iter().any(|a|matches!(a.name.as_str(),"OneToOne"|"OneToMany"|"ManyToOne"|"ManyToMany"|"ElementCollection"|"Embedded") && annotation_namespace(file,text,a).as_deref()==Some(format!("{namespace}.{}",a.name).as_str())) {fields.insert(field.name.clone());}
    }
    if fields.len()>64 {Vec::new()} else {fields.into_iter().collect()}
}

fn entity(doc: &Document, selection: Range) -> Option<Entity> {
    if doc.text.len() > MAX_TEXT || !matches!(doc.language.as_str(), "java" | "kotlin") || !spring_text::text::balanced(&doc.text, is_kotlin(&doc.path)) {
        return None;
    }
    let kotlin = is_kotlin(&doc.path);
    if kotlin != (doc.language == "kotlin") || (!kotlin && !doc.path.ends_with(".java")) {
        return None;
    }
    let file = parse_source(&doc.path, &doc.text);
    if file.package.is_empty()
        || file.package.len() > 512
        || !file.package.split('.').all(identifier)
        || file.types.iter().any(|ty| ty.outer.is_none() && matches!(ty.name.as_str(), "org" | "java" | "kotlin"))
    {
        return None;
    }
    // Aliased imports and wildcard annotation identities need semantic resolution.
    if kotlin && java::tokens(&doc.text).windows(2).any(|tokens| tokens[0].s == "as" && tokens[1].is_ident()) {
        return None;
    }
    let at = offset(&doc.text, selection.start)?;
    let candidates: Vec<_> = file
        .types
        .iter()
        .filter(|ty| ty.outer.is_none() && matches!(ty.kind.as_str(), "class" | "record") && ty.start <= at && at <= ty.end)
        .collect();
    let [ty] = candidates.as_slice() else {
        return None;
    };
    if !identifier(&ty.name) || !ty.extends.is_empty() || file.types.iter().filter(|other| other.name == ty.name).count() != 1 {
        return None;
    }
    let header = &doc.text[ty.at + ty.name.len()..ty.end];
    if java::tokens(header).iter().take_while(|token| !matches!(token.s, "{" | "(")).any(|token| token.s == "<") {
        return None;
    }
    let markers: Vec<_> = ty
        .annotations
        .iter()
        .filter_map(|annotation| annotation_namespace(&file, &doc.text, annotation))
        .filter(|name| ENTITIES.contains(&name.as_str()))
        .collect();
    let [marker] = markers.as_slice() else {
        return None;
    };
    if ty.annotations.iter().any(|annotation| matches!(annotation.name.as_str(), "IdClass")) {
        return None;
    }
    let expected = if marker == "jakarta.persistence.Entity" {
        "jakarta.persistence.Id"
    } else if marker == "javax.persistence.Entity" {
        "javax.persistence.Id"
    } else {
        "org.springframework.data.annotation.Id"
    };
    let mut ids = Vec::new();
    for member in &ty.members {
        if member.annotations.iter().any(|annotation| annotation.name == "EmbeddedId") {
            return None;
        }
        if member.annotations.iter().filter_map(|a| annotation_namespace(&file, &doc.text, a)).any(|name| name == expected) {
            if member.modifiers.iter().any(|modifier| matches!(modifier.as_str(), "static" | "transient"))
                || member.has(&["Transient"])
                || (member.kind == java::MemberKind::Method
                    && (!member.params.is_empty()
                        || !member
                            .name
                            .strip_prefix("get")
                            .or_else(|| member.name.strip_prefix("is"))
                            .is_some_and(|name| name.starts_with(char::is_uppercase))))
            {
                return None;
            }
            let name = if member.kind == java::MemberKind::Method {
                let tail = member.name.strip_prefix("get").or_else(|| member.name.strip_prefix("is"))?;
                let mut chars=tail.chars(); let first=chars.next()?;
                if first.is_ascii_uppercase() && chars.as_str().starts_with(char::is_uppercase) { tail.into() } else { format!("{}{}",first.to_ascii_lowercase(),chars.as_str()) }
            } else { member.name.clone() };
            if !identifier(&name) { return None; }
            ids.push((scalar_id(&file, member.ty.as_ref()?, kotlin)?,name));
        }
    }
    for component in &ty.components {
        if component.annotations.iter().any(|annotation| annotation.name == "EmbeddedId") {
            return None;
        }
        if component.annotations.iter().filter_map(|a| annotation_namespace(&file, &doc.text, a)).any(|name| name == expected) {
            ids.push((scalar_id(&file, &component.ty, kotlin)?,component.name.clone()));
        }
    }
    let [id] = ids.as_slice() else {
        return None;
    };
    Some(Entity { name: ty.name.clone(), at: ty.at, projection: projection_fields(&file,ty,&doc.text,kotlin), graph: graph_fields(&file,ty,&doc.text,if marker.starts_with("jakarta.") {"jakarta.persistence"} else {"javax.persistence"},kotlin), package: file.package, id: id.0.clone(), id_name: id.1.clone(), jpa: marker.ends_with("persistence.Entity") })
}
fn folder(root: &str, doc: &Document, entity: &Entity) -> Result<String, Error> {
    let source = path(&doc.path).ok_or("Source path is not an unambiguous absolute path.")?;
    if !inside(root, &source) {
        return Err(Error::Stale);
    }
    let parent = source.rsplit_once('/').ok_or(Error::Stale)?.0;
    if !parent.ends_with(&format!("/{}", entity.package.replace('.', "/"))) {
        return Err("The source folder does not match its declared package; choose a conventional package folder first.".into());
    }
    Ok(parent.into())
}
fn collision(project: &openidea_plugin::Project, root: &str, directory: &str, name: &str) -> Result<(), Error> {
    for extension in ["java", "kt"] {
        let target = format!("{directory}/{name}.{extension}");
        match project.read_file(&target) {
            Ok(_) => return Err("A Java/Kotlin file with this repository name already exists; it will not be overwritten.".into()),
            Err(Error::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let relative = directory.strip_prefix(&format!("{}/", root.trim_end_matches('/'))).ok_or(Error::Stale)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    for extension in ["java", "kt"] {
        let files = project.find_files(&format!("{relative}/*.{extension}"), 65)?;
        if files.len() >= 65 {
            return Err("Package folder exceeds the bounded collision check; choose repository creation manually.".into());
        }
        for file in files {
            if Instant::now() >= deadline {
                return Err("Package collision check timed out; no files were changed.".into());
            }
            let text = project.read_file(&file)?;
            if text.len() > MAX_TEXT {
                return Err("A package source exceeds the bounded collision check; no files were changed.".into());
            }
            if parse_source(&file, &text)
                .types
                .iter()
                .any(|ty| ty.outer.is_none() && (ty.name == name || matches!(ty.name.as_str(), "org" | "java" | "kotlin")))
            {
                return Err("A top-level type with this repository name already exists in the target folder.".into());
            }
        }
    }
    Ok(())
}
fn insert_finder(project:&openidea_plugin::Project,pending:&Pending,directory:&str,doc:&Document)->Result<FileAction,Error>{
    let Some(name)=host::input("Existing same-package JpaRepository interface name",None)? else{return Err(Error::Cancelled);};
    let name=name.trim();if !identifier(name)||name==pending.entity.name{return Err("Choose an existing repository type name in this source package.".into());}
    let kotlin=is_kotlin(&doc.path);let target=format!("{directory}/{name}.{}",if kotlin{"kt"}else{"java"});
    let text=project.read_file(&target)?;
    if text.len()>MAX_TEXT||!spring_text::text::balanced(&text, kotlin){return Err("Existing repository is oversized or structurally incomplete.".into());}
    if kotlin && java::tokens(&text).windows(2).any(|tokens|tokens[0].s=="as"&&tokens[1].is_ident()){return Err("Aliased or transformed Kotlin repository identities require explicit semantic ownership; no method was inserted.".into());}
    let file=parse_source(&target,&text);
    let [repository]=file.types.as_slice() else{return Err("Existing-repository insertion requires one directly declared interface.".into());};
    if repository.kind!="interface"||repository.name!=name||repository.outer.is_some()||file.package!=pending.entity.package||file.imports.iter().any(|i|i.path.ends_with(".*")||matches!(i.path.rsplit('.').next(),Some("org"|"java"|"kotlin"))||i.path.rsplit('.').next()==Some(pending.entity.name.as_str())&&i.path!=format!("{}.{}",pending.entity.package,pending.entity.name)) {return Err("Repository ownership is ambiguous; use a direct same-package JpaRepository interface.".into());}
    let parents=repository.extends.iter().chain(&repository.implements).collect::<Vec<_>>();
    let [parent]=parents.as_slice() else{return Err("Existing-repository insertion requires a single direct JpaRepository parent.".into());};
    let parent_name=parent.text.split('<').next().unwrap_or("").trim();
    if parent_name!="org.springframework.data.jpa.repository.JpaRepository"||parent.args.len()!=2||!matches!(parent.args[0].text.trim(),domain if domain==pending.entity.name||domain==format!("{}.{}",pending.entity.package,pending.entity.name))||parent.args[1].text.trim()!=pending.entity.id {return Err("Repository must declare the exact fully qualified JpaRepository parent and this source entity/identifier.".into());}
    let close=repository.end.checked_sub(1).filter(|at|text.as_bytes().get(*at)==Some(&b'}')).ok_or("Existing repository must have an explicit interface body.")?;
    // Reject generic interface type parameters and ambiguous namespace shadows.
    let header=&text[repository.at+repository.name.len()..close];
    if header.trim_start().starts_with('<')||file.types.iter().any(|t|matches!(t.name.as_str(),"org"|"java"|"kotlin")){return Err("Generic or namespace-shadowed repository declarations are outside insertion scope.".into());}
    let options=pending.entity.projection.iter().filter(|(name,_)|finder_method(name).is_some()).map(|(name,_)|name.as_str()).collect::<Vec<_>>().join(", ");
    let Some(selected)=host::input(&format!("Finder criteria: property or property:Operator (up to 8). Properties: {options}"),None)? else{return Err(Error::Cancelled);};
    let(method,params)=predicate_finder(&pending.entity.projection,&selected,kotlin)?;
    if repository.members.iter().any(|m|m.name==method){return Err("This repository already declares a method with that name; no overload is inserted.".into());}
    let params=params.iter().map(|(name,ty)|if kotlin{format!("{name}: {ty}")}else{format!("{ty} {name}")}).collect::<Vec<_>>().join(", ");
    let domain=&pending.entity.name;
    let declaration=if kotlin{format!("    fun {method}({params}): kotlin.collections.List<{domain}>
")}else{format!("    java.util.List<{domain}> {method}({params});
")};
    let insertion=format!("
{declaration}");let insertion=if text.contains("\r\n"){insertion.replace('\n',"\r\n")}else{insertion};
    if !host::confirm("Insert reviewed Spring Data finder?",&format!("Insert into {target} before its closing interface brace.
Entity: {}.{}

{insertion}
No existing method is changed; one undoable workspace edit.",pending.entity.package,pending.entity.name))? {return Err(Error::Cancelled);}
    if project.root()?!=pending.root||project.read_file(&doc.path)?!=doc.text||project.read_file(&target)?!=text||spring_text::index::project_of(&target).and_then(|p|p.root().ok()).as_deref()!=Some(pending.root.as_str()){return Err(Error::Stale);}
    let at=openidea_plugin::text::position(&text,close);
    Ok(FileAction{title:format!("Insert {method} into {name}"),kind:Some("refactor.rewrite".into()),edits:vec![(target.clone(),vec![openidea_plugin::TextEdit{range:Range::new(at,at),text:insertion}])],open:Some(target),..Default::default()})
}

impl FileActions for DataTools {
    fn file_actions(&mut self, doc: &Document, selection: Range) -> Vec<FileAction> {
        let Some(entity) = entity(doc, selection) else {
            return Vec::new();
        };
        let Some(root) = spring_text::index::project_of(&doc.path).and_then(|project| project.root().ok()).and_then(|root| path(&root))
        else {
            return Vec::new();
        };
        if folder(&root, doc, &entity).is_err() {
            return Vec::new();
        }
        let Some(next) = self.generation.next.checked_add(1) else {
            return Vec::new();
        };
        self.generation.next = next;
        if self.generation.pending.len() >= 4 { self.generation.pending.clear(); }
        let mut kinds = vec![("CrudRepository", "org.springframework.data.repository.CrudRepository")];
        if entity.jpa { kinds.push(("JpaRepository", "org.springframework.data.jpa.repository.JpaRepository")); kinds.push(("JpaRepositoryWithSpecifications","specification-repository")); if entity.projection.iter().any(|(name,_)|finder_method(name).is_some()) {kinds.push(("JpaRepositoryWithFinder","finder-repository"));kinds.push(("JpaRepositoryWithPagedFinder","paged-finder-repository"));kinds.push(("JpaRepositoryWithSortedFinder","sorted-finder-repository"));kinds.push(("FinderIntoExistingJpaRepository","insert-finder"));} if !entity.graph.is_empty() {kinds.push(("JpaRepositoryWithEntityGraph","entity-graph-repository"));} }
        if bean_property_spelling(&entity.id_name){kinds.push(("IdentifierProjection", "identifier-projection"));}
        if !entity.projection.is_empty() {kinds.push(("InterfaceProjection","interface-projection"));}
        kinds.into_iter().map(|(label, repository)| {
            let token = format!("repository:{next}:{label}:{}:{}:{}", doc.version, entity.at, entity.name);
            self.generation.pending.insert(token.clone(), Pending { root: root.clone(), doc: doc.clone(), entity: entity.clone(), repository: repository.into() });
            FileAction { title: if repository=="insert-finder"{format!("Add finder to existing JpaRepository for {}…",entity.name)}else{format!("Generate {} for {}…", plain_kind(label), entity.name)}, kind: Some("refactor.rewrite".into()), data: Some(token), ..FileAction::default() }
        }).collect()
    }

    fn resolve_file_action(&mut self, doc: &Document, action: FileAction) -> Result<FileAction, Error> {
        let pending = self.generation.pending.get(action.data.as_deref().ok_or(Error::Stale)?).cloned().ok_or(Error::Stale)?;
        if doc.path != pending.doc.path
            || doc.version != pending.doc.version
            || doc.text != pending.doc.text
            || doc.language != pending.doc.language
        {
            return Err(Error::Stale);
        }
        let project = spring_text::index::project_of(&doc.path).ok_or(Error::Stale)?;
        if path(&project.root()?).as_deref() != Some(pending.root.as_str()) {
            return Err(Error::Stale);
        }
        if project.read_file(&doc.path)? != doc.text {
            return Err(Error::Stale);
        }
        let directory = folder(&pending.root, doc, &pending.entity)?;
        if pending.repository=="insert-finder"{return insert_finder(&project,&pending,&directory,doc);}
        let projection = pending.repository.ends_with("-projection");
        let prompt = if projection { "Projection interface name (same package and folder)" } else { "Repository interface name (same package and folder)" };
        let default = format!("{}{}",pending.entity.name,if pending.repository=="interface-projection" {"Summary"} else if projection { "Identifier" } else { "Repository" });
        let Some(name) = host::input(prompt, Some(&default))?
        else {
            return Err(Error::Cancelled);
        };
        let name = name.trim();
        if !identifier(name)
            || matches!(name, "org" | "java" | "kotlin")
            || name == pending.entity.name
            || parse_source(&doc.path, &doc.text).types.iter().any(|ty| ty.name == name)
        {
            return Err("Choose a new Java/Kotlin identifier without path separators or keywords.".into());
        }
        collision(&project, &pending.root, &directory, name)?;
        let kotlin = is_kotlin(&doc.path);
        let target = format!("{directory}/{name}.{}", if kotlin { "kt" } else { "java" });
        if path(&target).is_none() {
            return Err("The target path exceeds the supported path bound.".into());
        }
        let text = if pending.repository == "specification-repository" {
            let domain=&pending.entity.name;let id=&pending.entity.id;
            if kotlin {format!("package {}\n\ninterface {name} : org.springframework.data.jpa.repository.JpaRepository<{domain}, {id}>, org.springframework.data.jpa.repository.JpaSpecificationExecutor<{domain}>\n",pending.entity.package)}
            else {format!("package {};\n\npublic interface {name} extends org.springframework.data.jpa.repository.JpaRepository<{domain}, {id}>, org.springframework.data.jpa.repository.JpaSpecificationExecutor<{domain}> {{\n}}\n",pending.entity.package)}
        } else if matches!(pending.repository.as_str(), "finder-repository"|"paged-finder-repository"|"sorted-finder-repository") {
            let options=pending.entity.projection.iter().filter(|(name,_)|finder_method(name).is_some()).map(|(name,_)|name.as_str()).collect::<Vec<_>>().join(", ");
            let Some(selected)=host::input(&format!("Finder criteria: property or property:Operator, separated by commas (up to 8). Properties: {options}",),None)? else{return Err(Error::Cancelled);};
            let (method,fields)=predicate_finder(&pending.entity.projection,&selected,kotlin)?;
            let mut params=fields.iter().map(|(property,ty)|if kotlin{format!("{property}: {ty}")}else{format!("{ty} {property}")}).collect::<Vec<_>>().join(", ");
            let domain=&pending.entity.name;let id=&pending.entity.id;
            let paged=pending.repository=="paged-finder-repository";
            if paged || pending.repository=="sorted-finder-repository" {
                let mut parameter=if paged{"pageRequest".to_owned()}else{"sort".to_owned()};
                while fields.iter().any(|(name,_)|*name==parameter){parameter.push('_');}
                let ty=if paged{"org.springframework.data.domain.Pageable"}else{"org.springframework.data.domain.Sort"};
                if !params.is_empty(){params.push_str(", ");}
                params.push_str(&if kotlin{format!("{parameter}: {ty}")}else{format!("{ty} {parameter}")});
            }
            let result=if paged{format!("org.springframework.data.domain.Page<{domain}>")}else if kotlin{format!("kotlin.collections.List<{domain}>")}else{format!("java.util.List<{domain}>")};
            if kotlin {format!("package {}\n\ninterface {name} : org.springframework.data.jpa.repository.JpaRepository<{domain}, {id}> {{\n    fun {method}({params}): {result}\n}}\n",pending.entity.package)}
            else {format!("package {};\n\npublic interface {name} extends org.springframework.data.jpa.repository.JpaRepository<{domain}, {id}> {{\n    {result} {method}({params});\n}}\n",pending.entity.package)}
        } else if pending.repository == "entity-graph-repository" {
            let default=pending.entity.graph.join(", ");
            let Some(selected)=host::input("Entity graph attributes (comma-separated source names)",Some(&default))? else {return Err(Error::Cancelled);};
            let mut seen=std::collections::BTreeSet::new();let mut attributes=Vec::new();
            for name in selected.split(',').map(str::trim) {
                if !pending.entity.graph.iter().any(|known|known==name) || !seen.insert(name) {return Err("Choose each displayed source association once.".into());}
                attributes.push(format!("\"{name}\""));
            }
            let attributes=attributes.join(", ");let domain=&pending.entity.name;let id=&pending.entity.id;
            if kotlin {format!("package {}\n\ninterface {name} : org.springframework.data.jpa.repository.JpaRepository<{domain}, {id}> {{\n    @org.springframework.data.jpa.repository.EntityGraph(attributePaths = [{attributes}])\n    override fun findById(id: {id}): java.util.Optional<{domain}>\n}}\n",pending.entity.package)}
            else {format!("package {};\n\npublic interface {name} extends org.springframework.data.jpa.repository.JpaRepository<{domain}, {id}> {{\n    @org.springframework.data.jpa.repository.EntityGraph(attributePaths = {{{attributes}}})\n    @Override\n    java.util.Optional<{domain}> findById({id} id);\n}}\n",pending.entity.package)}
        } else if pending.repository == "interface-projection" {
            let default=pending.entity.projection.iter().map(|(name,_)|name.as_str()).collect::<Vec<_>>().join(", ");
            let Some(selected)=host::input("Projection properties (comma-separated source names)",Some(&default))? else {return Err(Error::Cancelled);};
            let mut seen=std::collections::BTreeSet::new(); let mut fields=Vec::new();
            for name in selected.split(',').map(str::trim) {
                if !seen.insert(name) {return Err("Select each source property once.".into());}
                let field=pending.entity.projection.iter().find(|(key,_)|key==name).ok_or("Choose property names from the displayed source list.")?;
                fields.push(field);
            }
            let members=fields.into_iter().map(|(name,ty)| {
                if kotlin {format!("    val {name}: {ty}\n")}
                else {let mut chars=name.chars();let first=chars.next().unwrap().to_ascii_uppercase();format!("    {ty} get{first}{}();\n",chars.as_str())}
            }).collect::<String>();
            if kotlin {format!("package {}\n\ninterface {name} {{\n{members}}}\n",pending.entity.package)}
            else {format!("package {};\n\npublic interface {name} {{\n{members}}}\n",pending.entity.package)}
        } else if pending.repository == "identifier-projection" {
            if kotlin { format!("package {}\n\ninterface {name} {{\n    val {}: {}\n}}\n",pending.entity.package,pending.entity.id_name,pending.entity.id) }
            else {
                let mut chars=pending.entity.id_name.chars(); let first=chars.next().ok_or(Error::Stale)?.to_ascii_uppercase();
                format!("package {};\n\npublic interface {name} {{\n    {} get{}{}();\n}}\n",pending.entity.package,pending.entity.id,first,chars.as_str())
            }
        } else if kotlin {
            format!(
                "package {}\n\ninterface {name} : {}<{}, {}>\n",
                pending.entity.package, pending.repository, pending.entity.name, pending.entity.id
            )
        } else {
            format!(
                "package {};\n\npublic interface {name} extends {}<{}, {}> {{\n}}\n",
                pending.entity.package, pending.repository, pending.entity.name, pending.entity.id
            )
        };
        if !host::confirm(
            if projection { "Create Spring Data projection?" } else { "Create Spring Data repository?" },
            &format!(
                "Create only: {target}\nPackage: {}\nEntity: {}.{}\nIdentifier: {}\nSource version: {}\n\n{text}\nNo existing file is overwritten. The host applies this as one undoable workspace edit.",
                pending.entity.package, pending.entity.package, pending.entity.name, pending.entity.id, doc.version
            ),
        )? {
            return Err(Error::Cancelled);
        }
        if spring_text::index::project_of(&doc.path).and_then(|project| project.root().ok()).and_then(|root| path(&root)).as_deref()
            != Some(pending.root.as_str())
            || project.read_file(&doc.path)? != doc.text
        {
            return Err(Error::Stale);
        }
        collision(&project, &pending.root, &directory, name)?;
        Ok(FileAction {
            title: format!("Create {name}"),
            kind: Some("refactor.rewrite".into()),
            operations: vec![FileOperation::Create { path: target.clone(), text, overwrite: false }],
            open: Some(target),
            ..FileAction::default()
        })
    }
}

fn plain_kind(label: &str) -> &str {
    match label {
        "CrudRepository" => "CRUD repository",
        "JpaRepository" => "JPA repository",
        "JpaRepositoryWithSpecifications" => "repository with specifications",
        "JpaRepositoryWithFinder" => "repository with finder",
        "JpaRepositoryWithPagedFinder" => "repository with paged finder",
        "JpaRepositoryWithSortedFinder" => "repository with sorted finder",
        "JpaRepositoryWithEntityGraph" => "repository with entity graph",
        "IdentifierProjection" => "ID projection",
        "InterfaceProjection" => "interface projection",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::{mock::MockHost, text::position};
    const SOURCE: &str = "package com.acme;\nimport jakarta.persistence.Entity;\nimport jakarta.persistence.Id;\n@Entity public class User { @Id private long id; }\n";
    const SOURCE_PATH: &str = "/project/src/main/java/com/acme/User.java";
    fn doc(text: &str) -> Document {
        Document { path: SOURCE_PATH.into(), language: "java".into(), text: text.into(), version: 7 }
    }
    fn selection(doc: &Document) -> Range {
        let at = position(&doc.text, doc.text.find("User").unwrap());
        Range::new(at, at)
    }
    fn setup(text: &str) -> (MockHost, DataTools, Document) {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.add_file("src/main/java/com/acme/User.java", text);
        (host, DataTools::default(), doc(text))
    }
    fn action(plugin: &mut DataTools, doc: &Document) -> FileAction {
        let actions = plugin.file_actions(doc, selection(doc));
        assert!(!actions.is_empty(), "{}", doc.text);
        assert!(actions[0].operations.is_empty() && actions[0].edits.is_empty());
        actions.into_iter().next().unwrap()
    }
    fn approve(host: &MockHost, name: &str) {
        host.answer_next_prompt(Some(&[name]));
        host.answer_next_prompt(Some(&[]));
    }
    fn created(action: &FileAction) -> (&str, &str) {
        assert_eq!(action.operations.len(), 1);
        assert!(action.edits.is_empty());
        assert!(action.data.is_none());
        let FileOperation::Create { path, text, overwrite } = &action.operations[0] else { panic!("create only") };
        assert!(!overwrite);
        assert_eq!(action.open.as_deref(), Some(path.as_str()));
        (path, text)
    }

    #[test]
    fn projection_and_graph_follow_the_source_identifier_access_site(){
        let text="package com.acme;\n@jakarta.persistence.Entity class User { String hidden; @jakarta.persistence.Id public long getId(){return 0;} public String getName(){return null;} @jakarta.persistence.OneToMany public java.util.List<Order> getOrders(){return null;} @jakarta.persistence.OneToMany java.util.List<Order> fieldOnly; }";
        let(_host,mut plugin,doc)=setup(text);let action=action(&mut plugin,&doc);let captured_entity=&plugin.generation.pending[action.data.as_deref().unwrap()].entity;
        assert_eq!(captured_entity.projection.iter().map(|(name,_)|name.as_str()).collect::<Vec<_>>(),vec!["id","name"]);assert_eq!(captured_entity.graph,vec!["orders"]);
        let field=SOURCE.replace("private long id;","private long id; public String getComputed(){return null;}");let captured=entity(&self::doc(&field),selection(&self::doc(&field))).unwrap();assert_eq!(captured.projection.iter().map(|(name,_)|name.as_str()).collect::<Vec<_>>(),vec!["id"]);
    }

    #[test]
    fn specification_repository_is_a_reviewed_jpa_only_parent_variant(){
        let(host,mut plugin,doc)=setup(SOURCE);let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with specifications")).unwrap();approve(&host,"UserSpecifications");let resolved=plugin.resolve_file_action(&doc,pending).unwrap();assert!(created(&resolved).1.contains("JpaRepository<User, java.lang.Long>, org.springframework.data.jpa.repository.JpaSpecificationExecutor<User>"));
    }

    #[test]
    fn generated_finder_names_preserve_operator_like_property_spellings(){
        assert_eq!(finder_method("name").as_deref(),Some("findByName"));
        assert_eq!(finder_method("ageGreaterThan").as_deref(),Some("findByAgeGreaterThanIs"));
        assert_eq!(finder_method("id").as_deref(),Some("findByIdIs"));
        assert!(finder_method("firstAndLast").is_none());assert!(finder_method("nameOrderByAge").is_none());
        assert!(finder_method("Name").is_none());assert!(finder_method("X").is_none());assert!(finder_method("aB").is_none());assert!(finder_method("uRL").is_none());assert_eq!(finder_method("URL").as_deref(),Some("findByURL"));
    }

    #[test]
    fn derived_finder_repository_uses_one_known_scalar_property_without_path_transformation() {
        let text=SOURCE.replace("private long id;","private long id; String name; String postal_code;");let(host,mut plugin,doc)=setup(&text);
        let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with finder")).unwrap();
        host.answer_next_prompt(Some(&["UserFinder"]));host.answer_next_prompt(Some(&["name"]));host.answer_next_prompt(Some(&[]));let resolved=plugin.resolve_file_action(&doc,pending).unwrap();
        assert!(created(&resolved).1.contains("java.util.List<User> findByName(java.lang.String name);"));
        for selected in ["other","postal_code","name.other"] {let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with finder")).unwrap();host.answer_next_prompt(Some(&["UserFinder"]));host.answer_next_prompt(Some(&[selected]));assert!(plugin.resolve_file_action(&doc,pending).is_err());}
    }

    #[test]
    fn equality_conjunction_finder_roundtrips_properties_and_retains_parameter_order() {
        let properties=vec![("id".into(),"java.lang.Long".into()),("ageGreaterThan".into(),"java.lang.Integer".into()),("firstAndLast".into(),"java.lang.String".into()),("name".into(),"java.lang.String".into())];
        assert_eq!(predicate_finder(&properties,"id,ageGreaterThan",false).unwrap().0,"findByIdIsAndAgeGreaterThanIs");
        assert!(predicate_finder(&properties,"firstAndLast,name",false).is_err());
        let text=SOURCE.replace("private long id;","private long id; String name; int age;");let(host,mut plugin,doc)=setup(&text);
        let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with finder")).unwrap();
        host.answer_next_prompt(Some(&["UserFinder"]));host.answer_next_prompt(Some(&["name,age"]));host.answer_next_prompt(Some(&[]));
        let resolved=plugin.resolve_file_action(&doc,pending).unwrap();
        assert!(created(&resolved).1.contains("findByNameAndAge(java.lang.String name, java.lang.Integer age)"));
        let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with finder")).unwrap();
        host.answer_next_prompt(Some(&["UserFinder"]));host.answer_next_prompt(Some(&["name,name"]));assert!(plugin.resolve_file_action(&doc,pending).is_err());
    }
    #[test]
    fn paged_and_sorted_finders_use_explicit_framework_parameters_without_name_collisions() {
        let text=SOURCE.replace("private long id;","private long id; String pageRequest; String sort;");
        for (kind,selected,expected) in [
            ("JpaRepositoryWithPagedFinder", "pageRequest", "org.springframework.data.domain.Page<User> findByPageRequest(java.lang.String pageRequest, org.springframework.data.domain.Pageable pageRequest_);"),
            ("JpaRepositoryWithSortedFinder", "sort", "java.util.List<User> findBySort(java.lang.String sort, org.springframework.data.domain.Sort sort_);"),
        ] {
            let(host,mut plugin,doc)=setup(&text);
            let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with(&format!("Generate {} for", plain_kind(kind)))).unwrap();
            host.answer_next_prompt(Some(&["UserFinder"]));host.answer_next_prompt(Some(&[selected]));host.answer_next_prompt(Some(&[]));
            let resolved=plugin.resolve_file_action(&doc,pending).unwrap();assert!(created(&resolved).1.contains(expected));
        }
    }
    #[test]
    fn typed_predicates_preserve_arity_property_boundaries_and_parameter_types(){
        let fields=vec![("age".into(),"java.lang.Integer".into()),("name".into(),"java.lang.String".into()),("active".into(),"java.lang.Boolean".into())];
        let(method,params)=predicate_finder(&fields,"age:Between,name:Containing,active:True",false).unwrap();
        assert_eq!(method,"findByAgeBetweenAndNameContainingAndActiveTrue");
        assert_eq!(params,vec![("ageLower".into(),"java.lang.Integer".into()),("ageUpper".into(),"java.lang.Integer".into()),("name".into(),"java.lang.String".into())]);
        assert_eq!(predicate_finder(&fields,"age:In",false).unwrap().1[0].1,"java.util.Collection<java.lang.Integer>");
        assert_eq!(predicate_finder(&fields,"name:IsNull",false).unwrap().1.len(),0);
        for criteria in ["name:Between","age:Containing","name:True","age:Regex","age:Equals,age:Not","foreign:Equals"]{assert!(predicate_finder(&fields,criteria,false).is_err(),"{criteria}");}
        let(host,mut plugin,doc)=setup(&SOURCE.replace("private long id;","private long id; int age; String name;"));
        let action=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with finder")).unwrap();
        host.answer_next_prompt(Some(&["UserFinder"]));host.answer_next_prompt(Some(&["age:Between,name:Containing"]));host.answer_next_prompt(Some(&[]));
        assert!(created(&plugin.resolve_file_action(&doc,action).unwrap()).1.contains("findByAgeBetweenAndNameContaining(java.lang.Integer ageLower, java.lang.Integer ageUpper, java.lang.String name)"));
    }
    #[test]
    fn finder_insertion_preserves_existing_repository_and_refuses_duplicate_or_foreign_parent(){
        let(host,mut plugin,doc)=setup(&SOURCE.replace("private long id;","private long id; int age;"));
        let target="src/main/java/com/acme/UserRepository.java";
        let original="package com.acme;\r\npublic interface UserRepository extends org.springframework.data.jpa.repository.JpaRepository<User, java.lang.Long> {\r\n    // retained 🦉\r\n}\r\n";
        host.add_file(target,original);
        let action=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Add finder to existing JpaRepository")).unwrap();
        host.answer_next_prompt(Some(&["UserRepository"]));host.answer_next_prompt(Some(&["age:GreaterThan"]));host.answer_next_prompt(Some(&[]));
        let resolved=plugin.resolve_file_action(&doc,action).unwrap();assert!(resolved.operations.is_empty());assert_eq!(resolved.edits.len(),1);
        assert_eq!(resolved.edits[0].1[0].text,"\r\n    java.util.List<User> findByAgeGreaterThan(java.lang.Integer age);\r\n");
        assert_eq!(host.invocation().project().unwrap().read_file(target).unwrap(),original);
        for source in [original.replace("JpaRepository<User", "JpaRepository<Other"),original.replace("// retained 🦉", "java.util.List<User> findByAgeGreaterThan(java.lang.Integer age);")] {
            host.add_file(target,&source);let action=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Add finder to existing JpaRepository")).unwrap();host.answer_next_prompt(Some(&["UserRepository"]));host.answer_next_prompt(Some(&["age:GreaterThan"]));assert!(plugin.resolve_file_action(&doc,action).is_err());
        }
    }
    #[test]
    fn kotlin_existing_repository_insertion_is_qualified_nullable_and_cancel_safe(){
        let host=MockHost::install().with_capabilities(&["project.read"]);
        let text="package com.acme\n@jakarta.persistence.Entity class User(@jakarta.persistence.Id val id: Long, val name: String?)";
        let doc=Document{path:"/project/src/main/kotlin/com/acme/User.kt".into(),language:"kotlin".into(),text:text.into(),version:1};
        host.add_file(&doc.path,text);let target="src/main/kotlin/com/acme/UserRepository.kt";
        host.add_file(target,"package com.acme\ninterface UserRepository : org.springframework.data.jpa.repository.JpaRepository<User, kotlin.Long> {\n}\n");
        let mut plugin=DataTools::default();
        let action=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Add finder to existing JpaRepository")).unwrap();
        host.answer_next_prompt(Some(&["UserRepository"]));host.answer_next_prompt(Some(&["name:Containing"]));host.answer_next_prompt(Some(&[]));
        let result=plugin.resolve_file_action(&doc,action).unwrap();assert!(result.edits[0].1[0].text.contains("fun findByNameContaining(name: kotlin.String?): kotlin.collections.List<User>"));
        let action=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Add finder to existing JpaRepository")).unwrap();
        host.answer_next_prompt(Some(&["UserRepository"]));host.answer_next_prompt(Some(&["name:IsNull"]));host.answer_next_prompt(None);assert!(matches!(plugin.resolve_file_action(&doc,action),Err(Error::Cancelled)));
        let original=host.invocation().project().unwrap().read_file(target).unwrap();host.add_file(target,&original.replace("package com.acme","package com.acme\nimport foreign.Other as User"));
        let action=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Add finder to existing JpaRepository")).unwrap();host.answer_next_prompt(Some(&["UserRepository"]));assert!(plugin.resolve_file_action(&doc,action).is_err());
    }
    #[test]
    fn source_properties_with_nonroundtripping_bean_spelling_are_not_generated(){
        let text=SOURCE.replace("private long id;","private long id; String Name; String aB; Long AB; String URL;");
        let(_host,_,doc)=setup(&text);let captured=entity(&doc,selection(&doc)).unwrap();
        assert_eq!(captured.projection.iter().map(|(name,_)|name.as_str()).collect::<Vec<_>>(),vec!["AB","URL","id"]);
        let(host,mut plugin,doc)=setup(&SOURCE.replace("private long id;","@jakarta.persistence.Id long Name;").replace("@Id @jakarta.persistence.Id","@jakarta.persistence.Id"));
        assert!(plugin.file_actions(&doc,selection(&doc)).iter().all(|a|!a.title.contains("IdentifierProjection")));assert!(host.process_runs().is_empty());
    }
    #[test]
    fn entity_graph_repository_uses_only_selected_direct_source_associations() {
        let text=SOURCE.replace("private long id;","private long id; @jakarta.persistence.OneToMany java.util.List<Order> orders; @vendor.OneToOne Object foreign;");
        let (host,mut plugin,doc)=setup(&text);
        let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with entity graph")).unwrap();
        host.answer_next_prompt(Some(&["UserGraphRepository"]));host.answer_next_prompt(Some(&["orders"]));host.answer_next_prompt(Some(&[]));
        let resolved=plugin.resolve_file_action(&doc,pending).unwrap();let (_,generated)=created(&resolved);
        assert!(generated.contains("@org.springframework.data.jpa.repository.EntityGraph(attributePaths = {\"orders\"})"));
        assert!(generated.contains("java.util.Optional<User> findById(java.lang.Long id);"));assert!(!generated.contains("foreign"));
        for selected in ["foreign","orders,orders","orders.nested"] {
            let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate repository with entity graph")).unwrap();
            host.answer_next_prompt(Some(&["UserGraphRepository"]));host.answer_next_prompt(Some(&[selected]));assert!(plugin.resolve_file_action(&doc,pending).is_err());
        }
    }

    #[test]
    fn chosen_interface_projection_uses_only_admitted_source_properties() {
        let text=SOURCE.replace("private long id;","private long id; private String name; private boolean active; @jakarta.persistence.Transient String secret; Object unknown;");
        let (host,mut plugin,doc)=setup(&text);
        let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate interface projection")).unwrap();
        host.answer_next_prompt(Some(&["UserSummary"]));host.answer_next_prompt(Some(&["name, active"]));host.answer_next_prompt(Some(&[]));
        let resolved=plugin.resolve_file_action(&doc,pending).unwrap();
        assert_eq!(created(&resolved).1,"package com.acme;\n\npublic interface UserSummary {\n    java.lang.String getName();\n    java.lang.Boolean getActive();\n}\n");
        for selection in ["secret","unknown","name,name","name,other"] {
            let pending=plugin.file_actions(&doc,self::selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate interface projection")).unwrap();
            host.answer_next_prompt(Some(&["UserSummary"]));host.answer_next_prompt(Some(&[selection]));
            assert!(plugin.resolve_file_action(&doc,pending).is_err());
        }
    }
    #[test]
    fn kotlin_interface_projection_preserves_nullable_source_properties() {
        let host=MockHost::install().with_capabilities(&["project.read"]);
        let text="package com.acme\n@jakarta.persistence.Entity class User(@field:jakarta.persistence.Id val id: Long?, val name: String?)\n";
        let doc=Document {path:"/project/src/main/kotlin/com/acme/User.kt".into(),language:"kotlin".into(),text:text.into(),version:1};host.add_file("src/main/kotlin/com/acme/User.kt",text);
        let mut plugin=DataTools::default();let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate interface projection")).unwrap();
        host.answer_next_prompt(Some(&["UserSummary"]));host.answer_next_prompt(Some(&["name"]));host.answer_next_prompt(Some(&[]));
        let resolved=plugin.resolve_file_action(&doc,pending).unwrap();assert!(created(&resolved).1.contains("val name: kotlin.String?"));
    }

    #[test]
    fn identifier_projection_uses_proved_source_name_and_boxed_type() {
        let (host,mut plugin,doc)=setup(&SOURCE.replace("long id;","long accountKey;"));
        let pending=plugin.file_actions(&doc,selection(&doc)).into_iter().find(|a|a.title.starts_with("Generate ID projection")).unwrap();
        approve(&host,"UserIdentifier");let resolved=plugin.resolve_file_action(&doc,pending).unwrap();
        assert_eq!(created(&resolved).1,"package com.acme;\n\npublic interface UserIdentifier {\n    java.lang.Long getAccountKey();\n}\n");
    }

    #[test]
    fn kotlin_identifier_projection_and_java_acronym_getter_keep_property_identity() {
        let host=MockHost::install().with_capabilities(&["project.read"]);
        let text="package com.acme\n@jakarta.persistence.Entity class User(@field:jakarta.persistence.Id val key: Long?)\n";
        let source=Document {path:"/project/src/main/kotlin/com/acme/User.kt".into(),language:"kotlin".into(),text:text.into(),version:1};
        host.add_file("src/main/kotlin/com/acme/User.kt",text);
        let mut plugin=DataTools::default();
        let pending=plugin.file_actions(&source,selection(&source)).into_iter().find(|a|a.title.starts_with("Generate ID projection")).unwrap();
        approve(&host,"UserIdentifier");let resolved=plugin.resolve_file_action(&source,pending).unwrap();
        assert_eq!(created(&resolved).1,"package com.acme\n\ninterface UserIdentifier {\n    val key: kotlin.Long\n}\n");
        let java="package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id public java.lang.String getURL(){return null;} }";
        let owned=entity(&self::doc(java),selection(&self::doc(java))).unwrap();assert_eq!(owned.id_name,"URL");
    }

    #[test]
    fn jpa_repository_generation_is_owned_previewed_and_not_offered_for_mongodb() {
        let (host, mut plugin, doc) = setup(SOURCE);
        let actions = plugin.file_actions(&doc, selection(&doc));
        assert_eq!(actions.len(), 9);
        let pending = actions.into_iter().find(|a| a.title.starts_with("Generate JPA repository")).unwrap();
        approve(&host,"UserJpaRepository");
        let resolved = plugin.resolve_file_action(&doc,pending).unwrap();
        let (target,text) = created(&resolved);
        assert!(target.ends_with("/UserJpaRepository.java"));
        assert!(text.contains("extends org.springframework.data.jpa.repository.JpaRepository<User, java.lang.Long>"));
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
        let mongo = SOURCE.replace("jakarta.persistence.Entity","org.springframework.data.mongodb.core.mapping.Document").replace("@Entity","@Document").replace("jakarta.persistence.Id","org.springframework.data.annotation.Id");
        assert_eq!(plugin.file_actions(&self::doc(&mongo), selection(&self::doc(&mongo))).len(),3);
    }

    #[test]
    fn java_generation_is_one_non_overwriting_host_operation_without_external_writes() {
        let (host, mut plugin, doc) = setup(SOURCE);
        let pending = action(&mut plugin, &doc);
        approve(&host, "UserRepository");
        let resolved = plugin.resolve_file_action(&doc, pending).unwrap();
        let (target, text) = created(&resolved);
        assert_eq!(target, "/project/src/main/java/com/acme/UserRepository.java");
        assert_eq!(
            text,
            "package com.acme;\n\npublic interface UserRepository extends org.springframework.data.repository.CrudRepository<User, java.lang.Long> {\n}\n"
        );
        let project = spring_text::index::project_of(SOURCE_PATH).unwrap();
        assert_eq!(project.read_file(SOURCE_PATH).unwrap(), SOURCE);
        assert!(matches!(project.read_file(target), Err(Error::NotFound(_))), "resolution must not write the file");
        assert!(host.process_runs().is_empty() && host.requests().is_empty());
    }

    #[test]
    fn kotlin_nullable_scalar_id_uses_constructor_annotation_facts() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        let text = "package com.acme\nimport org.springframework.data.mongodb.core.mapping.Document\nimport org.springframework.data.annotation.Id\n@Document data class User(@field:Id val id: Long?)\n";
        let doc =
            Document { path: "/project/src/main/kotlin/com/acme/User.kt".into(), language: "kotlin".into(), text: text.into(), version: 2 };
        host.add_file("src/main/kotlin/com/acme/User.kt", text);
        let mut plugin = DataTools::default();
        let pending = action(&mut plugin, &doc);
        approve(&host, "UserRepository");
        let resolved = plugin.resolve_file_action(&doc, pending).unwrap();
        assert_eq!(
            created(&resolved).1,
            "package com.acme\n\ninterface UserRepository : org.springframework.data.repository.CrudRepository<User, kotlin.Long>\n"
        );
    }

    #[test]
    fn fully_qualified_annotations_record_components_and_getters_preserve_id_namespace() {
        for text in [
            "package com.acme; @jakarta.persistence.Entity public class User { @jakarta.persistence.Id private java.util.UUID id; }",
            "package com.acme; @javax.persistence.Entity public record User(@javax.persistence.Id java.util.UUID id) {}",
            "package com.acme; @jakarta.persistence.Entity public class User { @jakarta.persistence.Id public java.util.UUID getId() { return null; } }",
        ] {
            let (_host, mut plugin, doc) = setup(text);
            let pending = action(&mut plugin, &doc);
            let snapshot = &plugin.generation.pending[pending.data.as_deref().unwrap()];
            assert_eq!(snapshot.entity.id, "java.util.UUID");
        }
    }

    #[test]
    fn namespace_ambiguity_composite_inherited_and_unknown_ids_offer_no_generation() {
        for text in [
            "package com.acme; @Entity class User { @Id long id; }",
            "package com.acme; import other.Entity; import other.Id; @Entity class User { @Id long id; }",
            "package com.acme; import jakarta.persistence.*; @Entity class User { @Id long id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @javax.persistence.Id long id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id Other id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id java.util.List<Long> id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id long id; @jakarta.persistence.Id long second; }",
            "package com.acme; @jakarta.persistence.Entity @jakarta.persistence.IdClass(Key.class) class User { @jakarta.persistence.Id long id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.EmbeddedId Key id; }",
            "package com.acme; @jakarta.persistence.Entity class User extends Base { @jakarta.persistence.Id long id; }",
            "package com.acme; @jakarta.persistence.Entity class User<T> { @jakarta.persistence.Id long id; }",
            "package com.acme; class Outer { @jakarta.persistence.Entity class User { @jakarta.persistence.Id long id; } }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id static long id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id transient long id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id long calculate() { return 1; } }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id java.lang.Int id; }",
            "package com.acme; @jakarta.persistence.Entity class User { @jakarta.persistence.Id long id;",
        ] {
            let (_host, mut plugin, doc) = setup(text);
            assert!(plugin.file_actions(&doc, selection(&doc)).is_empty(), "{text}");
        }
    }

    #[test]
    fn package_path_language_root_and_text_bounds_are_conservative() {
        let (host, mut plugin, doc) = setup(SOURCE);
        for changed in [
            Document { text: SOURCE.replace("com.acme", "other.package"), ..doc.clone() },
            Document { path: "/project/src/User.java".into(), ..doc.clone() },
            Document { path: "/project/src/main/java/com/acme/../User.java".into(), ..doc.clone() },
            Document { language: "kotlin".into(), ..doc.clone() },
            Document { text: format!("{SOURCE}{}", " ".repeat(MAX_TEXT)), ..doc.clone() },
        ] {
            assert!(plugin.file_actions(&changed, selection(&changed)).is_empty());
        }
        host.set_project_root(None);
        assert!(plugin.file_actions(&doc, selection(&doc)).is_empty());
    }

    #[test]
    fn colliding_java_kotlin_and_sibling_declared_type_are_never_overwritten() {
        for (path, text) in [
            ("src/main/java/com/acme/UserRepository.java", "existing dirty or saved file"),
            ("src/main/java/com/acme/UserRepository.kt", "interface UserRepository"),
            ("src/main/java/com/acme/Other.java", "package com.acme; interface UserRepository {}"),
        ] {
            let (host, mut plugin, doc) = setup(SOURCE);
            let pending = action(&mut plugin, &doc);
            host.add_file(path, text);
            approve(&host, "UserRepository");
            assert!(plugin.resolve_file_action(&doc, pending).is_err());
            assert_eq!(spring_text::index::project_of(SOURCE_PATH).unwrap().read_file(path).unwrap(), text);
        }
    }

    #[test]
    fn invalid_repository_names_cannot_escape_package_or_inject_source() {
        for name in [
            "../UserRepository",
            "other.Repo",
            "/tmp/Repo",
            "class",
            "_",
            "User",
            "A; interface Evil {}",
            "A\nB",
            "Répo",
            "org",
            "java",
            "kotlin",
        ] {
            let (host, mut plugin, doc) = setup(SOURCE);
            let pending = action(&mut plugin, &doc);
            approve(&host, name);
            assert!(plugin.resolve_file_action(&doc, pending).is_err(), "{name}");
            assert!(host.process_runs().is_empty());
        }
    }

    #[test]
    fn dismissed_name_or_concrete_confirmation_returns_cancelled_without_writes() {
        let (host, mut plugin, doc) = setup(SOURCE);
        let pending = action(&mut plugin, &doc);
        assert!(matches!(plugin.resolve_file_action(&doc, pending.clone()), Err(Error::Cancelled)));
        host.answer_next_prompt(Some(&["UserRepository"]));
        assert!(matches!(plugin.resolve_file_action(&doc, pending), Err(Error::Cancelled)));
        assert!(spring_text::index::project_of(SOURCE_PATH).unwrap().read_file("src/main/java/com/acme/UserRepository.java").is_err());
    }

    #[test]
    fn changed_version_text_path_or_project_and_expired_actions_are_stale() {
        let (host, mut plugin, doc) = setup(SOURCE);
        let pending = action(&mut plugin, &doc);
        for changed in [
            Document { version: 8, ..doc.clone() },
            Document { text: format!("{SOURCE}//changed"), ..doc.clone() },
            Document { path: "/project/Other.java".into(), ..doc.clone() },
        ] {
            assert!(matches!(plugin.resolve_file_action(&changed, pending.clone()), Err(Error::Stale)));
        }
        let mut fresh = DataTools::default();
        assert!(matches!(fresh.resolve_file_action(&doc, pending.clone()), Err(Error::Stale)));
        host.set_project_root(Some("/other"));
        assert!(matches!(plugin.resolve_file_action(&doc, pending), Err(Error::Stale)));
    }

    #[test]
    fn current_dirty_source_is_preserved_and_stale_editor_text_rejects_resolution() {
        let (host, mut plugin, doc) = setup(SOURCE);
        let dirty = format!("{SOURCE}//unsaved 🦀\n");
        let doc = Document { text: dirty.clone(), ..doc };
        host.open_editor(SOURCE_PATH, "java", &dirty, &[]);
        let pending = action(&mut plugin, &doc);
        approve(&host, "UserRepository");
        plugin.resolve_file_action(&doc, pending.clone()).unwrap();
        assert_eq!(host.editor_text(), dirty);
        host.type_in_editor("//newer edit");
        assert!(matches!(plugin.resolve_file_action(&doc, pending), Err(Error::Stale)));
        assert!(host.editor_text().ends_with("//newer edit"));
    }

    #[test]
    fn bounded_package_collision_check_refuses_incomplete_discovery() {
        let (host, mut plugin, doc) = setup(SOURCE);
        let pending = action(&mut plugin, &doc);
        for index in 0..65 {
            host.add_file(&format!("src/main/java/com/acme/C{index}.java"), &format!("package com.acme; class C{index} {{}}"));
        }
        approve(&host, "UserRepository");
        assert!(plugin.resolve_file_action(&doc, pending).is_err());
    }

    #[test]
    fn utf16_selection_and_windows_paths_preserve_project_package_identity() {
        let host = MockHost::install().with_capabilities(&["project.read"]);
        host.set_project_root(Some("C:/project"));
        let text = SOURCE.replace("@Entity public", "/* 🦀 */ @Entity public");
        let doc = Document { path: "C:/project/src/main/java/com/acme/User.java".into(), text: text.clone(), ..doc(&text) };
        host.add_file("src/main/java/com/acme/User.java", &text);
        let mut plugin = DataTools::default();
        let pending = action(&mut plugin, &doc);
        approve(&host, "UserRepository");
        let resolved = plugin.resolve_file_action(&doc, pending).unwrap();
        assert_eq!(created(&resolved).0, "C:/project/src/main/java/com/acme/UserRepository.java");
    }

    #[test]
    fn kotlin_unknown_implicit_types_aliases_and_parameter_id_sites_are_not_guessed() {
        for declaration in [
            "@field:Id val id: Integer",
            "@param:Id val id: Long",
            "@field:Id val id: java.lang.Int",
            "@field:Id val id: kotlin.Integer",
            "@field:Id val id: Long??",
        ] {
            let text = format!(
                "package com.acme\nimport org.springframework.data.mongodb.core.mapping.Document\nimport org.springframework.data.annotation.Id\n@Document data class User({declaration})\n"
            );
            let doc = Document { path: "/project/src/main/kotlin/com/acme/User.kt".into(), language: "kotlin".into(), text, version: 1 };
            assert!(entity(&doc, selection(&doc)).is_none(), "{declaration}");
        }
        let text = "package com.acme\nimport org.springframework.data.mongodb.core.mapping.Document\nimport org.springframework.data.annotation.Id as Identifier\n@Document data class User(@Identifier val id: Long)\n";
        let doc =
            Document { path: "/project/src/main/kotlin/com/acme/User.kt".into(), language: "kotlin".into(), text: text.into(), version: 1 };
        assert!(entity(&doc, selection(&doc)).is_none());
    }
}
