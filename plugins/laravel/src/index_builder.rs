//! Owned resumable indexing. Completed file-phase jobs are never parsed again;
//! cached enumeration lets subsequent calls reach every later inventory.
use super::{Index,read_cache};
use openidea_plugin::{Error,Project,host};
use std::{cell::{Cell,RefCell},collections::{BTreeMap,BTreeSet}};
const FILES_PER_STEP:usize=16;
const BYTES_PER_STEP:usize=256*1024;
pub(crate) const STEP_BUDGET:std::time::Duration=std::time::Duration::from_millis(150);
pub(crate) const PROVIDER_STEP_BUDGET:std::time::Duration=std::time::Duration::from_millis(20);
const PROVIDER_FILE_BYTES:usize=64*1024;
pub(crate) enum BuildProgress{Pending{completed:usize,total:usize},Ready(Index)}
pub(crate) struct Builder{root:String,index:Index,session:Session,finished:bool,topology:BTreeSet<String>}
#[derive(Default)]pub(super) struct Session{
 done:RefCell<BTreeSet<String>>,known:RefCell<BTreeSet<String>>,attempted:RefCell<BTreeSet<String>>,
 used:Cell<usize>,semantic_used:Cell<usize>,bytes:Cell<usize>,pending:Cell<bool>,deadline:Cell<Option<std::time::Instant>>,provider:Cell<bool>,captured:Cell<usize>,opaque:Cell<bool>,listing_bytes:Cell<usize>,
 pub(super) source_cache:RefCell<read_cache::SourceCache>,
 listings:RefCell<BTreeMap<Vec<String>,Vec<String>>>,queries:RefCell<BTreeMap<(String,u32),Option<Vec<String>>>>,
 snapshots:RefCell<BTreeMap<String,Option<String>>>,snapshot_bytes:Cell<usize>,
 pub(super) eloquent_seen:RefCell<BTreeSet<String>>,
 pub(super) manifests:RefCell<BTreeSet<String>>,
}
impl Builder{
 pub(crate) fn new(project:&Project)->Result<Self,Error>{let root=project.root()?;let source_settings=["viewPaths","modelDirectories","publicDirectory","locale"].into_iter().map(|key|(key.into(),host::setting_string(key))).collect();let topology=host::projects()?.into_iter().filter_map(|p|p.root().ok()).collect();Ok(Self{index:Index{source_root:root.clone(),source_settings,..Index::default()},root,session:Session::default(),finished:false,topology})}
 pub(crate) fn root(&self)->&str{&self.root}
 /// One step that stops starting new work at `deadline`; capture resumes on the next step.
 pub(crate) fn step(&mut self,project:&Project,deadline:std::time::Instant)->Result<BuildProgress,Error>{self.advance(project,deadline,false)}
 /// Only reads files from listings an earlier step made; never lists, runs heavy jobs or captures.
 pub(crate) fn provider_step(&mut self,project:&Project,deadline:std::time::Instant)->Result<BuildProgress,Error>{self.advance(project,deadline,true)}
 fn advance(&mut self,project:&Project,deadline:std::time::Instant,provider:bool)->Result<BuildProgress,Error>{
  if self.finished||project.root()?!=self.root||!super::index_settings_current(&self.index){return Err(Error::Stale);}
  let topology=host::projects()?.into_iter().filter_map(|p|p.root().ok()).collect::<BTreeSet<_>>();if topology!=self.topology{return Err(Error::Stale);}
  self.session.begin(deadline,provider);
  if !self.session.done("final:publish"){Index::build_pass(project,&mut self.index,&self.session);}
  if self.session.done("final:publish")&&!provider&&self.session.capture(project,&mut self.index) {
   self.finished=true;
   return Ok(BuildProgress::Ready(std::mem::take(&mut self.index)));
  }
  let completed=self.session.done.borrow().len();let total=self.session.known.borrow().len().max(completed+1);
  Ok(BuildProgress::Pending{completed,total})
 }
}
impl Session{
 fn begin(&self,deadline:std::time::Instant,provider:bool){self.deadline.set(Some(deadline));self.provider.set(provider);self.used.set(0);self.semantic_used.set(0);self.bytes.set(0);self.pending.set(false);self.attempted.borrow_mut().clear();}
 pub(super) fn retire(&self,key:&str){if self.known.borrow().contains(key){self.done.borrow_mut().insert(key.into());}}
 pub(super) fn mark_done(&self,key:&str){self.known.borrow_mut().insert(key.into());self.done.borrow_mut().insert(key.into());}
 pub(super) fn done(&self,key:&str)->bool{self.done.borrow().contains(key)}
 pub(super) fn pending(&self)->bool{self.pending.get()}
 pub(super) fn heavy(&self,key:&str)->bool{
  self.known.borrow_mut().insert(key.into());if self.done(key){return false;}
  if self.provider.get()||self.used.get()>0||self.semantic_used.get()>0||self.out_of_time(){self.pending.set(true);return false;}
  self.used.set(FILES_PER_STEP);self.done.borrow_mut().insert(key.into());true
 }
 pub(super) fn semantics(&self,index:&mut Index,stage:&str)->bool{
  if self.provider.get()||self.used.get()>0{self.pending.set(true);return false;}
  let keys=index.eloquent_models.keys().cloned().collect::<Vec<_>>();
  for round in 0..4{for key in &keys{
   let job=format!("{stage}:{round}:{key}");self.known.borrow_mut().insert(job.clone());if self.done(&job){continue;}
   if self.semantic_used.get()>=32||self.out_of_time(){self.pending.set(true);return false;}
   self.semantic_used.set(self.semantic_used.get()+1);crate::eloquent_members::finish_model(&mut index.eloquent_models,key);self.done.borrow_mut().insert(job);
  }}
  self.mark_done(stage);true
 }
 fn out_of_time(&self)->bool{self.deadline.get().is_some_and(|deadline|std::time::Instant::now()>=deadline)}
 pub(super) fn attempted(&self,stage:&str,path:&str)->bool{self.attempted.borrow().contains(&format!("{stage}\0{path}"))}
 pub(super) fn file(&self,project:&Project,stage:&str,path:&str,index:&mut Index)->bool{
  let key=format!("{stage}\0{path}");self.known.borrow_mut().insert(key.clone());if self.done(&key){return false;}
  if self.used.get()>=FILES_PER_STEP||self.out_of_time(){self.pending.set(true);return false;}
  let root=&index.source_root;let absolute=if path.starts_with('/') {path.into()}else{format!("{root}/{path}")};
  if host::project_of(&absolute).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()) {
   self.opaque.set(true);index.partial=true;self.done.borrow_mut().insert(key);self.used.set(self.used.get()+1);return false;
  }
  let size=project.stat(path).map(|s|s.size as usize).unwrap_or(0);
  if self.used.get()>0&&self.bytes.get().saturating_add(size)>BYTES_PER_STEP||self.provider.get()&&size>PROVIDER_FILE_BYTES{self.pending.set(true);return false;}
  self.done.borrow_mut().insert(key.clone());self.attempted.borrow_mut().insert(key);self.used.set(self.used.get()+1);
  self.bytes.set(self.bytes.get()+size);if size>BYTES_PER_STEP{self.used.set(FILES_PER_STEP);}true
 }
 pub(super) fn files(&self,project:&Project,root:&str,globs:&[String])->Vec<String>{
  if let Some(found)=self.listings.borrow().get(globs){return found.clone();}
  if self.provider.get(){self.pending.set(true);return Vec::new();}
  let mut paths=Vec::new();
  for glob in globs{
   let key=(glob.clone(),super::MAX_FILES);
   let previous=self.queries.borrow().get(&key).cloned();
   let result=if let Some(found)=previous{found}else{
    let found=project.find_files(glob,super::MAX_FILES).ok().map(|mut found|{if found.len()>=super::MAX_FILES as usize{self.opaque.set(true);}found.retain(|path|host::project_of(path).and_then(|p|p.root().ok()).as_deref()==Some(root));found.sort();found.dedup();found});
    self.queries.borrow_mut().insert(key,found.clone());found
   };
   if let Some(found)=result{paths.extend(found)}else{self.opaque.set(true);}
  }
  paths.sort();paths.dedup();let bytes=paths.iter().map(String::len).sum::<usize>();
  if self.listings.borrow().len()>=256||self.listing_bytes.get().saturating_add(bytes)>8*1024*1024{self.opaque.set(true);return Vec::new();}
  self.listing_bytes.set(self.listing_bytes.get()+bytes);self.listings.borrow_mut().insert(globs.to_vec(),paths.clone());paths
 }
 pub(super) fn snapshot(&self,project:&Project,path:&str)->Option<String>{
  if let Some(text)=self.snapshots.borrow().get(path){return text.clone();}
  if self.provider.get(){self.pending.set(true);return None;}
  let text=project.read_file(path).ok();let bytes=path.len()+text.as_ref().map_or(0,String::len);
  if self.snapshots.borrow().len()>=1024||self.snapshot_bytes.get().saturating_add(bytes)>2*1024*1024{self.opaque.set(true);return None;}
  if text.is_none()&&!matches!(project.stat(path),Err(Error::NotFound(_))){self.opaque.set(true);}
  self.snapshot_bytes.set(self.snapshot_bytes.get()+bytes);self.snapshots.borrow_mut().insert(path.into(),text.clone());text
 }
 /// Records source evidence; false when the deadline cut it short (the next step resumes it).
 fn capture(&self,project:&Project,index:&mut Index)->bool{
  let root=index.source_root.clone();
  for (path,text)in self.source_cache.borrow().sources().iter().chain(self.snapshots.borrow().iter()).skip(self.captured.get()){
   if self.out_of_time(){self.pending.set(true);return false;}
   self.captured.set(self.captured.get()+1);
   let relative=path.strip_prefix(&format!("{root}/")).unwrap_or(path).to_owned();
   let stamp=text.as_ref().map(|text|(text.len(),crate::artisan_metadata::hash(text)));
   if stamp.is_none()&&!matches!(project.stat(&relative),Err(Error::NotFound(_))){self.opaque.set(true);}
   if index.source_evidence.get(&relative).is_some_and(|old|old!=&stamp){self.opaque.set(true);}
   index.source_evidence.insert(relative,stamp);
  }
  index.source_queries.extend(self.queries.borrow().clone());index.source_evidence_incomplete|=self.opaque.get();index.partial|=self.opaque.get();
  for(path,stamp)in &index.metadata.source_evidence{if index.source_evidence.get(path).is_some_and(|old|old!=stamp){index.source_evidence_incomplete=true;}index.source_evidence.insert(path.clone(),stamp.clone());}index.source_queries.extend(index.metadata.source_queries.clone());index.source_evidence_incomplete|=index.metadata.source_evidence_incomplete;
  true
 }
}
#[cfg(test)]mod tests{
 use super::*;use openidea_plugin::mock::MockHost;
 fn complete(builder:&mut Builder,project:&Project)->(Index,usize){let mut previous=0;for step in 0..512{match builder.step(project,std::time::Instant::now()+STEP_BUDGET).unwrap(){BuildProgress::Ready(index)=>return(index,step+1),BuildProgress::Pending{completed,total}=>{assert!(completed>=previous);assert!(total>completed);previous=completed;}}}panic!("builder did not finish");}
 #[test]fn resumed_jobs_retain_late_models_migrations_vite_and_exact_freshness_queries(){
  let h=MockHost::install();h.add_file("composer.json",r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#);
  for i in 0..13{h.add_file(&format!("app/Http/Controllers/C{i}.php"),&format!("<?php namespace App\\Http\\Controllers; class C{i}{{public function index(){{}}}}"));}
  h.add_file("app/Models/Last.php","<?php namespace App\\Models; class Last extends \\Illuminate\\Database\\Eloquent\\Model{}");
  h.add_file("database/migrations/last.php","<?php Schema::create('lasts',function($table){$table->string('name');});");
  h.add_file("vite.config.js","import laravel from 'laravel-vite-plugin'; export default {plugins:[laravel({input:['frontend/app.ts'],buildDirectory:'assets'})]};");h.add_file("frontend/app.ts","export default 1;");h.add_file("public/assets/manifest.json",r#"{"logo":{"src":"logo.svg","file":"logo-hash.svg"}}"#);
  let project=h.invocation().project().unwrap();let mut builder=Builder::new(&project).unwrap();assert_eq!(builder.root(),"/project");let(index,steps)=complete(&mut builder,&project);assert!(steps>4);assert!(!index.partial,"{:?}",index.metadata.issues);assert!(index.eloquent_models.contains_key("app\\models\\last"));assert!(index.columns.contains_key("lasts"));assert!(index.vite.contains_key("frontend/app.ts"));assert!(super::super::index_sources_current(&project,&index),"incomplete={} evidence={:?}",index.source_evidence_incomplete,index.source_evidence);
  h.add_file("app/Models/New.php","<?php namespace App\\Models; class New{}");assert!(!super::super::index_sources_current(&project,&index));
 }
 fn done_count(builder:&Builder)->usize{builder.session.done.borrow().len()}
 #[test]fn provider_step_on_a_fresh_builder_lists_and_reads_nothing(){
  let h=MockHost::install();h.add_file("composer.json","{}");h.add_file("artisan","<?php");
  let project=h.invocation().project().unwrap();let mut builder=Builder::new(&project).unwrap();
  let far=std::time::Instant::now()+std::time::Duration::from_secs(60);
  assert!(matches!(builder.provider_step(&project,far).unwrap(),BuildProgress::Pending{..}));
  assert_eq!(done_count(&builder),0);assert!(builder.session.listings.borrow().is_empty());assert!(builder.session.snapshots.borrow().is_empty());
 }
 #[test]fn provider_steps_read_some_files_stop_at_the_budget_and_make_progress(){
  let h=MockHost::install();h.add_file("composer.json",r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#);
  for i in 0..200{h.add_file(&format!("resources/views/v{i}.blade.php"),&format!("<div>{i}</div>"));}
  let project=h.invocation().project().unwrap();let mut builder=Builder::new(&project).unwrap();
  let far=||std::time::Instant::now()+std::time::Duration::from_secs(60);
  builder.step(&project,far()).unwrap();builder.step(&project,far()).unwrap();
  let listed=builder.session.listings.borrow().len();assert!(listed>0);
  let expired=std::time::Instant::now();let before=done_count(&builder);
  builder.provider_step(&project,expired).unwrap();assert_eq!(done_count(&builder),before,"an expired budget starts no unit");
  let mut previous=before;
  for _ in 0..3{
   assert!(matches!(builder.provider_step(&project,far()).unwrap(),BuildProgress::Pending{..}));
   let now=done_count(&builder);assert!(now>previous&&now-previous<=FILES_PER_STEP,"{previous} -> {now}");previous=now;
  }
  assert_eq!(builder.session.listings.borrow().len(),listed,"provider steps never list files");
  let (index,_)=complete(&mut builder,&project);assert_eq!(index.views.len(),200);assert_eq!(index.blade.len(),200);
 }
 #[test]fn pending_is_not_a_published_partial_and_source_mutation_invalidates_completed_proof(){
  let h=MockHost::install();h.add_file("composer.json","{}");h.add_file("config/app.php","<?php return ['name'=>'first'];");let project=h.invocation().project().unwrap();let mut builder=Builder::new(&project).unwrap();assert!(matches!(builder.step(&project,std::time::Instant::now()+STEP_BUDGET).unwrap(),BuildProgress::Pending{..}));assert!(!builder.index.partial);let(index,_)=complete(&mut builder,&project);assert!(super::super::index_sources_current(&project,&index));h.add_file("config/app.php","<?php return ['name'=>'other'];");assert!(!super::super::index_sources_current(&project,&index));assert!(matches!(builder.step(&project,std::time::Instant::now()+STEP_BUDGET),Err(Error::Stale)));
 }
 #[test]fn paused_builder_rejects_changed_settings_and_project_ownership_topology(){
  let h=MockHost::install();h.add_file("composer.json","{}");let project=h.invocation().project().unwrap();let mut builder=Builder::new(&project).unwrap();h.set_setting("modelDirectories","\"src/Models\"");assert!(matches!(builder.step(&project,std::time::Instant::now()+STEP_BUDGET),Err(Error::Stale)));let mut builder=Builder::new(&project).unwrap();h.add_project("/project/nested");assert!(matches!(builder.step(&project,std::time::Instant::now()+STEP_BUDGET),Err(Error::Stale)));
 }

}
