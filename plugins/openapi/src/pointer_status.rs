//! Three-state physical source pointer evidence for diagnostics. Not a Rename graph.
use crate::{json,spec,yaml_pointer};
use serde_json::Value;
use std::collections::BTreeMap;
use yaml_rust2::Yaml;
const MAX_BYTES:usize=512<<10;
const MAX_NODES:usize=8192;
const MAX_DEPTH:usize=48;
const MAX_PATH_BYTES:usize=4<<20;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum Status { Present,Missing,Unknown }
enum Root { Json(Value),Yaml(Yaml) }
#[derive(Clone,Copy)]
enum Node<'a> { Json(&'a Value),Yaml(&'a Yaml) }
impl<'a> Node<'a> {
    fn object(self)->bool {matches!(self,Self::Json(Value::Object(_))|Self::Yaml(Yaml::Hash(_)))}
    fn array(self)->Option<Vec<Self>> {match self {
        Self::Json(Value::Array(items))=>Some(items.iter().map(Self::Json).collect()),
        Self::Yaml(Yaml::Array(items))=>Some(items.iter().map(Self::Yaml).collect()),
        _=>None,
    }}
    fn string(self)->Option<&'a str> {match self {Self::Json(value)=>value.as_str(),Self::Yaml(value)=>value.as_str()}}
    fn entries(self)->Vec<(&'a str,Self)> {
        match self {
            Self::Json(Value::Object(map))=>map.iter().map(|(name,value)|(name.as_str(),Self::Json(value))).collect(),
            Self::Yaml(Yaml::Hash(map))=>map.iter().filter_map(|(name,value)|name.as_str().map(|name|(name,Self::Yaml(value)))).collect(),
            _=>Vec::new(),
        }
    }
    fn child(self,name:&str)->Result<Option<Self>,Status> {
        match self {
            Self::Json(Value::Object(map))=>Ok(map.get(name).map(Self::Json)),
            Self::Yaml(Yaml::Hash(map))=>Ok(map.get(&Yaml::String(name.into())).map(Self::Yaml)),
            Self::Json(Value::Array(_))|Self::Yaml(Yaml::Array(_))=>Err(Status::Unknown),
            _=>Err(Status::Missing),
        }
    }
}
pub(crate) struct Parameter {
    pub name:String,
    pub location:String,
    pub declared_at:Option<(usize,usize)>,
}
pub(crate) struct Proof {
    root:Root,
    text:String,
    keys:Vec<spec::Key>,
    source:BTreeMap<String,usize>,
    references:BTreeMap<(usize,usize),String>,
    reference_paths:BTreeMap<String,(usize,String)>,
}
fn bounded(value:&Value,depth:usize,nodes:&mut usize)->bool {
    *nodes+=1;
    if depth>MAX_DEPTH || *nodes>MAX_NODES {return false;}
    match value {
        Value::Object(map)=>map.values().all(|v|bounded(v,depth+1,nodes)),
        Value::Array(items)=>items.iter().all(|v|bounded(v,depth+1,nodes)),
        _=>true,
    }
}
impl Proof {
    pub fn retained_bytes(&self)->usize {
        // Conservative accounting for owned tree/text, duplicated flattened
        // paths, scanner/source maps and captured reference strings.
        self.text.len()*4+self.keys.iter().map(|key|key.key.len()*2+256).sum::<usize>()
            +self.references.values().map(|value|value.len()*2+64).sum::<usize>()
            +self.reference_paths.iter().map(|(path,(_,target))|path.len()*2+target.len()*2+96).sum::<usize>()
    }
    pub fn parse(text:&str)->Option<Self> {
        if text.len()>MAX_BYTES {return None;}
        let root=if spec::is_json(text) {
            let value:Value=serde_json::from_str(text).ok()?;
            if !bounded(&value,0,&mut 0) {return None;}
            Root::Json(value)
        } else {Root::Yaml(yaml_pointer::model(text)?)};
        let keys=if matches!(&root,Root::Json(_)) {json::bounded_keys(text,MAX_NODES,MAX_PATH_BYTES)?} else {crate::yaml_scan::keys(text,MAX_NODES,MAX_PATH_BYTES,MAX_DEPTH)?};
        if keys.len()>MAX_NODES {return None;}
        let mut source=BTreeMap::new();let mut bytes=0usize;
        for (index,key) in keys.iter().enumerate() {
            bytes=bytes.checked_add(key.key.len())?;
            if bytes>MAX_PATH_BYTES {return None;}
            let raw=text.get(key.at..key.end)?;
            let name=if matches!(&root,Root::Json(_)) {json::unescape(raw)} else {raw.into()};
            if matches!(name.as_str(),"$id"|"<<") || source.insert(key.key.clone(),index).is_some() {return None;}
        }
        let mut proof=Self {root,text:text.into(),keys,source,references:BTreeMap::new(),reference_paths:BTreeMap::new()};
        let mut references=BTreeMap::new();let mut reference_paths=BTreeMap::new();let mut reference_path_bytes=0usize;
        let node=match &proof.root {Root::Json(value)=>Node::Json(value),Root::Yaml(value)=>Node::Yaml(value)};
        proof.collect_references(node,&mut String::new(),&mut String::new(),&mut references,&mut reference_paths,&mut reference_path_bytes);
        proof.references=references;proof.reference_paths=reference_paths;
        Some(proof)
    }
    fn scanner_prefix(&self,path:&str)->bool {
        if self.source.contains_key(path) {return true;}
        for separator in [".","["] {
            let prefix=format!("{path}{separator}");
            if self.source.range(prefix.clone()..).next().is_some_and(|(key,_)|key.starts_with(&prefix)) {return true;}
        }
        false
    }
    fn node(&self,pointer:&str)->Result<Node<'_>,Status> {
        let mut current=match &self.root {Root::Json(value)=>Node::Json(value),Root::Yaml(value)=>Node::Yaml(value)};
        if pointer.is_empty() {return Ok(current);}
        if pointer.len()>4096 {return Err(Status::Unknown);}
        let rest=pointer.strip_prefix('/').ok_or(Status::Unknown)?;
        let mut path=String::new();
        for token in rest.split('/') {
            if token.contains('%') {return Err(Status::Unknown);}
            let name=token.replace("~1","/").replace("~0","~");
            if name.replace('~',"~0").replace('/',"~1")!=token || name.chars().any(char::is_control) {return Err(Status::Unknown);}
            if !path.is_empty() {path.push('.');}path.push_str(&name);
            current=match current.child(&name)? {
                Some(value)=>value,
                None=>return Err(if self.scanner_prefix(&path) {Status::Unknown} else {Status::Missing}),
            };
            let key=self.source.get(&path).and_then(|index|self.keys.get(*index)).ok_or(Status::Unknown)?;
            let raw=self.text.get(key.at..key.end).filter(|_|key.at<key.end).ok_or(Status::Unknown)?;
            let matches=match &self.root {Root::Json(_)=>json::unescape(raw)==name,Root::Yaml(_)=>raw==name};
            if !matches {return Err(Status::Unknown);}
        }
        Ok(current)
    }
    pub fn status(&self,pointer:&str)->Status {
        match self.node(pointer) {Ok(_)=>Status::Present,Err(status)=>status}
    }
    fn collect_references(&self,node:Node<'_>,path:&mut String,pointer:&mut String,references:&mut BTreeMap<(usize,usize),String>,reference_paths:&mut BTreeMap<String,(usize,String)>,reference_path_bytes:&mut usize) {
        for (name,value) in node.entries() {
            let before=path.len();let pointer_before=pointer.len();
            if !path.is_empty() {path.push('.');}path.push_str(name);
            pointer.push('/');pointer.push_str(&name.replace('~',"~0").replace('/',"~1"));
            if let Some(key)=self.source.get(path.as_str()).and_then(|index|self.keys.get(*index)) {
                let exact=self.text.get(key.at..key.end).is_some_and(|raw|key.at<key.end&&match &self.root {Root::Json(_)=>json::unescape(raw)==name,Root::Yaml(_)=>raw==name});
                if exact {
                    if name=="$ref" {
                        if let Some(value)=value.string() {
                            references.insert((key.value_at,key.value_end),value.into());
                            Self::marker_reference(pointer,value,key.value_at,reference_paths,reference_path_bytes);
                        }
                    }
                    // A simple singleton YAML flow reference can be proved from
                    // its exact parent span and the already validated semantic
                    // map, without claiming general flow-child scanner ownership.
                    if matches!(&self.root,Root::Yaml(_)) {
                        if let (Node::Yaml(Yaml::Hash(map)),Some(raw))=(value,self.text.get(key.value_at..key.value_end)) {
                            if map.len()==1 && raw.starts_with('{') && raw.ends_with('}') {
                                if let Some(target)=map.get(&Yaml::String("$ref".into())).and_then(Yaml::as_str) {
                                    let fragment=format!("flow: {raw}");let refs=spec::refs(&fragment);
                                    if let [reference]=refs.as_slice() {
                                        if reference.target==target && reference.at>=6 && reference.end<=fragment.len() {
                                            Self::marker_reference(&format!("{pointer}/$ref"),target,key.value_at+reference.at-6,reference_paths,reference_path_bytes);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Actual mapping-name traversal preserves dotted literal keys;
                    // arrays and unproved flow/transformed source spans stay unknown.
                    self.collect_references(value,path,pointer,references,reference_paths,reference_path_bytes);
                }
            }
            path.truncate(before);pointer.truncate(pointer_before);
        }
    }
    fn marker_reference(pointer:&str,target:&str,at:usize,paths:&mut BTreeMap<String,(usize,String)>,bytes:&mut usize) {
        let weight=pointer.len().saturating_add(target.len());
        if bytes.saturating_add(weight)<=MAX_PATH_BYTES {*bytes+=weight;paths.insert(pointer.into(),(at,target.into()));}
    }
    pub fn reference(&self,reference:&spec::Ref)->bool {
        self.references.get(&(reference.at,reference.end)).is_some_and(|value|value==&reference.target)
    }
    /// A copied pointer is a source fact, so select the actual key token at
    /// the caret rather than a flattened path or the first key on its line.
    pub fn copy_pointer_at(&self,caret:usize)->Option<String> {
        let root=match &self.root {Root::Json(value)=>Node::Json(value),Root::Yaml(value)=>Node::Yaml(value)};
        let version=["openapi","swagger"].into_iter().any(|name| {
            self.node(&format!("/{name}")).is_ok_and(|value| match value {
                Node::Json(Value::String(text))|Node::Yaml(Yaml::String(text))=>!text.is_empty(),
                Node::Json(Value::Number(_))|Node::Yaml(Yaml::Integer(_))|Node::Yaml(Yaml::Real(_))=>true,
                _=>false,
            })
        });
        if !version {return None;}
        fn walk(proof:&Proof,node:Node<'_>,path:&mut String,pointer:&mut String,caret:usize)->Option<String> {
            if let Some(items)=node.array() {
                for (index,item) in items.into_iter().enumerate() {
                    let before=path.len();let pointer_before=pointer.len();
                    path.push_str(&format!("[{index}]"));pointer.push('/');pointer.push_str(&index.to_string());
                    let found=walk(proof,item,path,pointer,caret);
                    path.truncate(before);pointer.truncate(pointer_before);
                    if found.is_some() {return found;}
                }
                return None;
            }
            for (name,value) in node.entries() {
                let before=path.len();let pointer_before=pointer.len();
                if !path.is_empty() {path.push('.');}path.push_str(name);
                pointer.push('/');pointer.push_str(&name.replace('~',"~0").replace('/',"~1"));
                let found=(|| {
                    let key=proof.source.get(path.as_str()).and_then(|index|proof.keys.get(*index))?;
                    let raw=proof.text.get(key.at..key.end)?;
                    let exact=key.at<key.end && !name.chars().any(char::is_control) && match &proof.root {
                        Root::Json(_)=>json::unescape(raw)==name,Root::Yaml(_)=>raw==name,
                    };
                    if !exact {return None;}
                    let start=key.at.checked_sub(1).filter(|index| matches!(proof.text.as_bytes()[*index],b'\''|b'"')).unwrap_or(key.at);
                    if start<=caret && caret<=key.end {
                        // RFC6901 URI-fragment representation: escape tokens
                        // first, then encode UTF8 bytes outside unreserved and
                        // the structural '/' separators. Never reinterpret a
                        // literal '%' in a source member name as an escape.
                        let mut fragment=String::from("#");
                        for byte in pointer.bytes() {
                            if byte.is_ascii_alphanumeric() || matches!(byte,b'-'|b'.'|b'_'|b'~'|b'/') {fragment.push(byte as char);}
                            else {fragment.push_str(&format!("%{byte:02X}"));}
                            if fragment.len()>4096 {return None;}
                        }
                        return Some(fragment);
                    }
                    walk(proof,value,path,pointer,caret)
                })();
                path.truncate(before);pointer.truncate(pointer_before);
                if found.is_some() {return found;}
            }
            None
        }
        walk(self,root,&mut String::new(),&mut String::new(),caret)
    }
    /// Select only literal reference fields whose complete mapping/array ancestry
    /// and lexical source span agree. This does not expand diagnostic coverage.
    pub fn navigation_ref_at(&self,caret:usize)->Option<spec::Ref> {
        fn walk(proof:&Proof,node:Node<'_>,path:&str,caret:usize)->Option<spec::Ref> {
            if let Some(items)=node.array() {
                for (index,item) in items.into_iter().enumerate() {
                    if let Some(found)=walk(proof,item,&format!("{path}[{index}]"),caret) {return Some(found);}
                }
                return None;
            }
            for (name,value) in node.entries() {
                let next=if path.is_empty(){name.into()}else{format!("{path}.{name}")};
                let Some(key)=proof.source.get(&next).and_then(|index|proof.keys.get(*index)) else {continue;};
                let raw=proof.text.get(key.at..key.end)?;
                let exact=match &proof.root {Root::Json(_)=>json::unescape(raw)==name,Root::Yaml(_)=>raw==name};
                if !exact {continue;}
                if name=="$ref" && key.value_at<=caret && caret<=key.value_end {
                    let target=value.string()?;
                    let raw=proof.text.get(key.value_at..key.value_end)?;
                    let literal=match &proof.root {Root::Json(_)=>json::unescape(raw)==target,Root::Yaml(_)=>raw==target};
                    if literal {return Some(spec::Ref{target:target.into(),at:key.value_at,end:key.value_end});}
                }
                if matches!(&proof.root,Root::Yaml(_)) {
                    if let (Node::Yaml(Yaml::Hash(map)),Some(raw))=(value,proof.text.get(key.value_at..key.value_end)) {
                        if map.len()==1 && raw.starts_with('{') && raw.ends_with('}') {
                            if let Some(target)=map.get(&Yaml::String("$ref".into())).and_then(Yaml::as_str) {
                                let fragment=format!("flow: {raw}");
                                if let [reference]=spec::refs(&fragment).as_slice() {
                                    if reference.target==target && reference.at>=6 && reference.end<=fragment.len() {
                                        let at=key.value_at+reference.at-6;let end=key.value_at+reference.end-6;
                                        if at<=caret && caret<=end {return Some(spec::Ref{target:target.into(),at,end});}
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(found)=walk(proof,value,&next,caret) {return Some(found);}
            }
            None
        }
        let root=match &self.root {Root::Json(value)=>Node::Json(value),Root::Yaml(value)=>Node::Yaml(value)};
        walk(self,root,"",caret)
    }
    fn field<'a>(&'a self,owner:Node<'a>,path:&str,name:&str)->Option<(Node<'a>,&'a spec::Key)> {
        let value=owner.child(name).ok()??;
        let key=self.source.get(&format!("{path}.{name}")).and_then(|index|self.keys.get(*index))?;
        let raw=self.text.get(key.at..key.end).filter(|_|key.at<key.end)?;
        let exact=match &self.root {Root::Json(_)=>json::unescape(raw)==name,Root::Yaml(_)=>raw==name};
        exact.then_some((value,key))
    }
    fn string_field<'a>(&'a self,owner:Node<'a>,path:&str,name:&str)->Option<(&'a str,&'a spec::Key)> {
        let (value,key)=self.field(owner,path,name)?;
        let value=value.string().filter(|value|!value.is_empty()&&value.len()<=4096&&!value.chars().any(char::is_control))?;
        let raw=self.text.get(key.value_at..key.value_end).filter(|_|key.value_at<key.value_end)?;
        let exact=match &self.root {Root::Json(_)=>json::unescape(raw)==value,Root::Yaml(_)=>raw==value};
        exact.then_some((value,key))
    }
    fn parameter(&self,owner:Node<'_>,path:&str,referenced:bool)->Option<Parameter> {
        if !owner.object() || owner.child("$ref").ok()?.is_some() {return None;}
        let (name,key)=self.string_field(owner,path,"name")?;
        let (location,_)=self.string_field(owner,path,"in")?;
        if !matches!(location,"path"|"query"|"header"|"cookie"|"body"|"formData") {return None;}
        Some(Parameter {name:name.into(),location:location.into(),declared_at:(!referenced).then_some((key.value_at,key.value_end))})
    }
    fn parameters(&self,owner:Node<'_>,path:&str)->Option<Vec<Parameter>> {
        let list_path=format!("{path}.parameters");
        let Some(_) = owner.child("parameters").ok()? else {
            return (!self.scanner_prefix(&list_path)).then(Vec::new);
        };
        let (list,_)=self.field(owner,path,"parameters")?;
        let items=list.array()?;
        if items.len()>1024 {return None;}
        let mut found=Vec::new();let mut identities=std::collections::HashSet::new();
        for (index,item) in items.into_iter().enumerate() {
            let item_path=format!("{list_path}[{index}]");
            let parameter=if item.child("$ref").ok()?.is_some() {
                if item.child("name").ok()?.is_some() || item.child("in").ok()?.is_some() {return None;}
                let (reference,_)=self.string_field(item,&item_path,"$ref")?;
                let pointer=reference.strip_prefix('#')?;
                let target=self.node(pointer).ok()?;
                let target_path=pointer.strip_prefix('/')?.split('/').map(|part|part.replace("~1","/").replace("~0","~")).collect::<Vec<_>>().join(".");
                self.parameter(target,&target_path,true)?
            } else {self.parameter(item,&item_path,false)?};
            if !identities.insert((parameter.name.clone(),parameter.location.clone())) {return None;}
            found.push(parameter);
        }
        Some(found)
    }
    fn operation_owner(&self,operation:&spec::Operation)->Option<(Node<'_>,Node<'_>,String)> {
        let method=operation.method.to_ascii_lowercase();
        let path_pointer=format!("/paths/{}",operation.path.replace('~',"~0").replace('/',"~1"));
        let path=self.node(&path_pointer).ok()?;
        let operation_pointer=format!("{path_pointer}/{method}");
        let owner=self.node(&operation_pointer).ok()?;
        if !path.object() || !owner.object() || path.child("$ref").ok()?.is_some() || owner.child("$ref").ok()?.is_some() {return None;}
        let operation_path=format!("paths.{}.{method}",operation.path);
        let source=self.source.get(&operation_path).and_then(|index|self.keys.get(*index))?;
        if source.at!=operation.at || source.end!=operation.end {return None;}
        Some((path,owner,operation_path))
    }
    pub fn operation_parameters(&self,operation:&spec::Operation)->Option<Vec<Parameter>> {
        let (path,owner,operation_path)=self.operation_owner(operation)?;
        let shared=self.parameters(path,&format!("paths.{}",operation.path))?;
        let own=self.parameters(owner,&operation_path)?;
        // Operation parameters override a path-item entry by (name, in).
        let mut effective:Vec<_>={
            let identities:std::collections::HashSet<_>=own.iter().map(|parameter|(parameter.name.as_str(),parameter.location.as_str())).collect();
            shared.into_iter().filter(|parameter|!identities.contains(&(parameter.name.as_str(),parameter.location.as_str()))).collect()
        };
        effective.extend(own);Some(effective)
    }
    pub fn operation_id(&self,operation:&spec::Operation)->Option<(&str,(usize,usize))> {
        let (_,owner,path)=self.operation_owner(operation)?;
        let (value,key)=self.string_field(owner,&path,"operationId")?;
        Some((value,(key.value_at,key.value_end)))
    }
    pub fn pointer_location(&self,pointer:&str)->Option<openidea_plugin::Range> {
        self.node(pointer).ok()?;
        let path=pointer.strip_prefix('/')?.split('/').map(|part|part.replace("~1","/").replace("~0","~")).collect::<Vec<_>>().join(".");
        let key=self.source.get(&path).and_then(|index|self.keys.get(*index))?;
        (key.at<key.end).then(||spring_text::range(&self.text,key.at,key.end))
    }
    /// Every resolvable local `$ref` of the operation in source order, one per distinct target.
    pub fn operation_references(&self,operation:&spec::Operation)->Vec<(String,openidea_plugin::Range)> {
        if self.operation_owner(operation).is_none() {return Vec::new();}
        let prefix=format!("/paths/{}/{}/",operation.path.replace('~',"~0").replace('/',"~1"),operation.method.to_ascii_lowercase());
        let mut found:Vec<_>=self.reference_paths.range(prefix.clone()..).take_while(|(pointer,_)|pointer.starts_with(&prefix))
            .filter_map(|(_, (at,target))| {
                let pointer=target.strip_prefix('#')?;
                let location=self.pointer_location(pointer)?;
                Some((*at,target.clone(),location))
            }).collect();
        found.sort_by_key(|(at,_,_)|*at);
        let mut seen=std::collections::HashSet::new();
        found.into_iter().filter(|(_,_,location)|seen.insert((location.start.line,location.start.character)))
            .map(|(_,target,location)|(target,location)).collect()
    }
    /// Components of this document with the local `$ref` value spans that point at them or a descendant, in source order.
    pub fn component_usages(&self)->Vec<(String,openidea_plugin::Range,Vec<openidea_plugin::Range>)> {
        let mut grouped:BTreeMap<String,Vec<openidea_plugin::Range>>=BTreeMap::new();
        for ((from,to),target) in &self.references {
            let Some(pointer)=target.strip_prefix("#/components/") else {continue;};
            let owner=pointer.splitn(3,'/').take(2).collect::<Vec<_>>().join("/");
            grouped.entry(format!("/components/{owner}")).or_default().push(spring_text::range(&self.text,*from,*to));
        }
        let mut usages:Vec<_>=grouped.into_iter().filter_map(|(base,uses)| {
            let base=self.component_base(&base)?;
            let declaration=self.pointer_location(&base)?;
            Some((base,declaration,uses))
        }).collect();
        usages.sort_by_key(|(_,declaration,_)|(declaration.start.line,declaration.start.character));
        usages
    }

}

impl Proof {
    pub(crate) fn component_base(&self, pointer: &str) -> Option<String> {
        let parts: Vec<_> = pointer.strip_prefix('/')?.split('/').collect();
        if parts.len() < 3 || parts[0] != "components" || parts[2].is_empty()
            || !["schemas","responses","parameters","examples","requestBodies","headers","securitySchemes","links","callbacks","pathItems"].contains(&parts[1]) { return None; }
        let base = format!("/components/{}/{}", parts[1], parts[2]);
        self.pointer_location(&base)?; Some(base)
    }
    pub(crate) fn complete_refactor_references(&self) -> Option<Vec<spec::Ref>> {
        fn count(node: Node<'_>) -> Option<usize> {
            let mut total = 0usize;
            if let Some(items) = node.array() {for item in items {total += count(item)?;}}
            for (name,value) in node.entries() {
                if matches!(name,"discriminator"|"$dynamicRef"|"$recursiveRef"|"$anchor"|"$dynamicAnchor") {return None;}
                if name=="$ref" && value.string().is_some() {total+=1;}
                total+=count(value)?;
            }
            (total<=512).then_some(total)
        }
        let root=match &self.root {Root::Json(value)=>Node::Json(value),Root::Yaml(value)=>Node::Yaml(value)};
        let total=count(root)?;let candidates=spec::refs(&self.text);
        if candidates.len()!=total {return None;}
        if candidates.iter().any(|reference|!self.navigation_ref_at(reference.at).is_some_and(|owned|owned.at==reference.at&&owned.end==reference.end&&owned.target==reference.target)) {return None;}
        Some(candidates)
    }
    pub(crate) fn physical_references(&self) -> Option<Vec<spec::Ref>> {
        let candidates = spec::refs(&self.text);
        if candidates.len() > 512 { return None; }
        Some(candidates.into_iter().filter(|reference| self.navigation_ref_at(reference.at)
            .is_some_and(|owned| owned.at == reference.at && owned.end == reference.end && owned.target == reference.target)).collect())
    }
    /// Exact physical-source occurrences; refs to descendants count as uses of
    /// their owning component, while a similarly prefixed component does not.
    pub(crate) fn component_occurrences(&self, caret: usize) -> Option<Vec<(String, openidea_plugin::Range)>> {
        let pointer = self.navigation_ref_at(caret).and_then(|r| r.target.strip_prefix('#').map(str::to_owned))
            .or_else(|| self.copy_pointer_at(caret).and_then(|p| p.strip_prefix('#').map(str::to_owned)))?;
        let parts: Vec<_> = pointer.strip_prefix('/')?.split('/').collect();
        if parts.len() < 3 || parts[0] != "components" || parts[1].is_empty() || parts[2].is_empty() { return None; }
        if !["schemas","responses","parameters","examples","requestBodies","headers","securitySchemes","links","callbacks","pathItems"].contains(&parts[1]) { return None; }
        let base = format!("/components/{}/{}", parts[1], parts[2]);
        let definition = self.pointer_location(&base)?;
        let target = format!("#{base}");
        let prefix = format!("{target}/");
        let mut spans = vec![("Component declaration".into(), definition)];
        for ((from, to), value) in &self.references {
            if value == &target || value.starts_with(&prefix) {
                spans.push(("Local $ref".into(), spring_text::range(&self.text, *from, *to)));
            }
        }
        if spans.len() > 512 { return None; }
        spans.sort_by_key(|(_, r)| (r.start.line, r.start.character));
        Some(spans)
    }
}
#[cfg(test)]
mod component_occurrence_tests {
    use super::*;
    #[test]
    fn json_and_yaml_component_identity_and_descendants() {
        for text in [r##"{"openapi":"3.1.1","components":{"schemas":{"Foo":{"type":"object"},"Foo2":{}}},"paths":{},"a":{"$ref":"#/components/schemas/Foo"},"b":{"$ref":"#/components/schemas/Foo/properties/id"},"c":{"$ref":"#/components/schemas/Foo2"},"description":"#/components/schemas/Foo"}"##,
            "openapi: 3.1.1\ncomponents:\n  schemas:\n    Foo:\n      type: object\n    Foo2: {}\na:\n  $ref: '#/components/schemas/Foo'\nb:\n  $ref: '#/components/schemas/Foo/properties/id'\nc:\n  $ref: '#/components/schemas/Foo2'\ndescription: '#/components/schemas/Foo'\n"] {
            let proof = Proof::parse(text).unwrap();
            let at = text.find("Foo").unwrap();
            let spans = proof.component_occurrences(at).unwrap();
            assert_eq!(spans.len(), 3, "{text}");
            assert_eq!(proof.component_occurrences(text.find("#/components/schemas/Foo").unwrap()).unwrap().len(), 3);
        }
    }
    #[test]
    fn escaped_identity_is_read_only_and_external_refs_are_excluded() {
        let text = r##"{"openapi":"3.1.1","components":{"schemas":{"a/b":{}}},"a":{"$ref":"#/components/schemas/a~1b"},"b":{"$ref":"other.json#/components/schemas/a~1b"}}"##;
        let proof = Proof::parse(text).unwrap();
        assert_eq!(proof.component_occurrences(text.find("a/b").unwrap()).unwrap().len(), 2);
        assert!(proof.component_occurrences(text.find("other.json").unwrap()).is_none());
    }
}
