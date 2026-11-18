//! Optional, exact-profile Batch command-line adapter. No fabricated server API.
use openidea_plugin::{host::{self,Child,Command},Error,FileKind,Invocation,Project};
use sha2::{Digest,Sha256};
use std::{collections::BTreeMap,time::{Duration,Instant}};
const OUTPUT:&str="Spring Batch CLI";
const PROBE_SOURCE:&str=include_str!("../runtime/BatchRuntimeProbe.java");
const QUERY_SOURCE:&str=include_str!("../runtime/BatchRuntimeQuery.java");
const RUNNER:&str="org.springframework.batch.core.launch.support.CommandLineJobRunner";
const OPERATOR:&str="org.springframework.batch.core.launch.support.CommandLineJobOperator";
/// Read as `plugin.aritti.spring-integration.batchCliActive` by the Poll and Cancel commands.
const ACTIVE_CONTEXT:&str="batchCliActive";
const EXECUTION_ID:&str="Enter a job execution ID (a positive number).";
const NOT_RUNNING:&str="No Spring Batch CLI process is running. Start one with Spring Batch: Run Job Operation…";
fn invalid(s:&str)->Error{Error::Invalid(s.into())}
fn digest(s:&str)->String{format!("{:x}",Sha256::digest(s.as_bytes()))}
fn relative(s:&str)->bool{!s.is_empty()&&s.len()<=512&&!s.contains(['\\',':',';','*','?','[',']','{','}'])&&!s.starts_with('/')&&!s.chars().any(char::is_control)&&s.split('/').all(|p|!matches!(p,""|"."|".."))}
fn literal(s:&str)->bool{!s.is_empty()&&s.len()<=128&&!s.starts_with('-')&&!s.contains('=')&&!s.chars().any(char::is_control)}
#[derive(Clone,Debug,PartialEq,Eq)]
struct Profile{root:String,profile:String,context:String,source_path:String,source:String,kind:String,classpath:Vec<String>,probe:String}
impl Profile {
 fn load(project:&Project)->Result<Self,Error>{
  let root=project.root()?;if root.is_empty()||root=="/"||root.contains(['\n','\r','\\']){return Err(invalid("Open a local project folder first."));}
  let profile=host::setting_string("springBatch.runtimeProfile").unwrap_or_default();if !matches!(profile.as_str(),"runner-5.2.6"|"operator-6.0.6"){return Err(invalid("Spring Batch job runs are not set up. Set Batch command-line runner, Batch run classpath, Batch application context class and Batch application context source file in Settings › Plugins › Spring Structure Tools."));}
  let context=host::setting_string("springBatch.runtimeContext").unwrap_or_default();let resource=host::setting_string("springBatch.runtimeContextSource").unwrap_or_default();if !relative(&resource){return Err(invalid("Batch application context source file must be a project-relative path, such as src/main/java/com/acme/BatchConfig.java."));}
  let source_path=format!("{root}/{resource}");if host::project_of(&source_path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()){return Err(Error::Stale);}
  let source=project.read_file(&source_path)?;if source.len()>32*1024 {return Err(invalid("The context source file is too large (over 32 KiB)."));}
  let kind=if resource.ends_with(".java")||resource.ends_with(".kt") {
   if context.is_empty()||context.len()>512||context.contains(['/', '\\', ':'])||context.chars().any(char::is_control)||context.split('.').any(str::is_empty){return Err(invalid("Batch application context class must be a fully qualified class name, such as com.acme.BatchConfig."));}
   let file=spring_text::parse_source(&source_path,&source);if file.types.iter().filter(|t|t.outer.is_none()&&format!("{}.{}",file.package,t.name)==context).count()!=1{return Err(invalid("The context source file does not declare the class set in Batch application context class."));}"java"
  }else if resource.ends_with(".xml")&&profile=="runner-5.2.6"&&relative(&context)&&context.ends_with(".xml")&&spring_text::xml::analysis_valid(&source){"xml"}else{return Err(invalid("Use a Java or Kotlin context class. XML context files work only with the Spring Batch 5.2.6 runner."));}.into();
  let configured=host::setting_string("springBatch.runtimeClasspath").unwrap_or_default();let entries:Vec<_>=configured.lines().filter(|line|!line.trim().is_empty()).collect();if entries.is_empty()||entries.len()>32{return Err(invalid("Batch run classpath needs 1 to 32 project-relative entries, one per line, without wildcards."));}
  let mut classpath=Vec::new();for entry in entries {
   if !relative(entry)||entry.trim()!=entry{return Err(invalid("Classpath entries must be project-relative paths without wildcards, .. or surrounding spaces."));}
   let path=format!("{root}/{entry}");if classpath.contains(&path)||host::project_of(&path).and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str()){return Err(invalid("A classpath entry is listed twice or is outside this project."));}
   let stat=project.stat(&path)?;if stat.kind!=FileKind::Directory&&!(stat.kind==FileKind::File&&path.ends_with(".jar")){return Err(invalid("Classpath entries must be existing directories or JAR files."));}classpath.push(path);
  }
  let probe=format!("{}/BatchRuntimeProbe-{}.java",host::fs::plugin_dir()?.trim_end_matches('/'),digest(PROBE_SOURCE));
  Ok(Self{root,profile,context,source_path,source,kind,classpath,probe})
 }
 fn joined(&self)->String{self.classpath.join(if self.root.as_bytes().get(1)==Some(&b':')||self.root.starts_with("//"){";"}else{":"})}
 fn probe_args(&self)->Vec<String>{vec!["--source".into(),"17".into(),self.probe.clone(),self.root.clone(),self.profile.clone(),self.context.clone(),self.source_path.clone(),self.kind.clone(),self.joined()]}
 fn launcher(&self)->&str{if self.profile=="runner-5.2.6"{RUNNER}else{OPERATOR}}
}
#[derive(Clone,Debug,PartialEq,Eq)]
struct Proof{classpath:String,context:String,java:u32}
fn parse_probe(profile:&Profile,output:&str)->Result<Proof,Error>{
 if output.len()>4096{return Err(invalid("Classpath probe output exceeded its protocol limit."));}
 let mut lines=output.lines();if lines.next()!=Some("OPENIDEA_BATCH_PROFILE_V1"){return Err(invalid("Classpath probe did not return its supported protocol."));}
 let mut fields=BTreeMap::new();for line in lines{let(k,v)=line.split_once('=').ok_or_else(||invalid("Malformed classpath proof."))?;if fields.insert(k,v).is_some(){return Err(invalid("Duplicate classpath proof field."));}}
 if fields.len()!=5||fields.get("profile").copied()!=Some(profile.profile.as_str())||fields.get("source-sha256").copied()!=Some(digest(&profile.source).as_str()){return Err(invalid("The compiled context does not match its source file, or the settings changed. Save and rebuild, then run Spring Batch: Validate Configured CLI Profile."));}
 let hash=|name:&str|->Result<String,Error>{let h=fields.get(name).ok_or_else(||invalid("Missing classpath proof hash."))?;if h.len()!=64||!h.bytes().all(|b|b.is_ascii_hexdigit()&&!b.is_ascii_uppercase()){return Err(invalid("Invalid classpath proof hash."));}Ok((*h).into())};
 let java=fields.get("java").and_then(|n|n.parse::<u32>().ok()).filter(|n|*n>=17&&*n<100).ok_or_else(||invalid("This profile requires Java 17 or newer."))?;
 Ok(Proof{classpath:hash("classpath-sha256")?,context:hash("context-sha256")?,java})
}
fn ensure_probe(profile:&Profile)->Result<(),Error>{
 if host::fs::read_file(&profile.probe).ok().as_deref()!=Some(PROBE_SOURCE) {host::fs::write_file(&profile.probe,PROBE_SOURCE)?;}
 if host::fs::read_file(&profile.probe)?!=PROBE_SOURCE {return Err(Error::Stale);}Ok(())
}
fn run_helper(project:&Project,args:&[String])->Result<String,Error>{
 let task=host::begin_task("Running the Spring Batch helper",true);let child=host::start(&Command::new("java").args(args),Some(project))?;child.close_stdin()?;let started=Instant::now();let(mut stdout,mut stderr)=(String::new(),String::new());
 loop {
  if task.is_cancelled(){let _=child.kill();return Err(Error::Cancelled);}
  if started.elapsed()>Duration::from_secs(20){let _=child.kill();return Err(invalid("The Spring Batch helper timed out after 20 seconds and was stopped."));}
  let chunk=child.wait(100)?;stdout.push_str(&chunk.stdout);stderr.push_str(&chunk.stderr);if stdout.len()+stderr.len()>16*1024{let _=child.kill();return Err(invalid("The Spring Batch helper printed too much output and was stopped."));}
  if let Some(status)=chunk.status{if status!=0{return Err(Error::Failed(format!("The Spring Batch helper failed (exit {status}): {}",stderr.chars().take(1500).collect::<String>())));}return Ok(stdout);}
 }
}
fn probe(project:&Project,profile:&Profile)->Result<Proof,Error>{ensure_probe(profile)?;parse_probe(profile,&run_helper(project,&profile.probe_args())?)}
#[derive(Clone,Debug,PartialEq,Eq)]
struct Inspection {execution:u64,found:bool,instance:Option<u64>,job:Option<String>,status:Option<String>,start:Option<String>,end:Option<String>,observed:Option<String>}
fn inspection_args(profile:&Profile,id:u64)->Result<Vec<String>,Error>{
 let path=format!("{}/BatchRuntimeQuery-{}.java",host::fs::plugin_dir()?.trim_end_matches('/'),digest(QUERY_SOURCE));
 if host::fs::read_file(&path).ok().as_deref()!=Some(QUERY_SOURCE){host::fs::write_file(&path,QUERY_SOURCE)?;}if host::fs::read_file(&path)?!=QUERY_SOURCE{return Err(Error::Stale);}
 Ok(vec!["--class-path".into(),profile.joined(),"--source".into(),"17".into(),path,profile.profile.clone(),profile.context.clone(),profile.kind.clone(),id.to_string()])
}
fn parse_inspection(profile:&Profile,id:u64,output:&str)->Result<Inspection,Error>{
 if output.len()>16*1024{return Err(invalid("Execution inspection exceeded its output limit."));}
 let (_,record)=output.rsplit_once("OPENIDEA_BATCH_EXECUTION_V1\n").ok_or_else(||invalid("Execution helper did not return its typed protocol."))?;
 let mut fields=BTreeMap::new();for line in record.lines(){let(k,v)=line.split_once('=').ok_or_else(||invalid("Malformed execution metadata."))?;if fields.insert(k,v).is_some(){return Err(invalid("Duplicate execution metadata field."));}}
 if fields.get("profile").copied()!=Some(profile.profile.as_str())||fields.get("execution-id").and_then(|s|s.parse::<u64>().ok())!=Some(id){return Err(invalid("Execution metadata identity/profile mismatch."));}
 if fields.get("found").copied()==Some("false")&&fields.len()==3{return Ok(Inspection{execution:id,found:false,instance:None,job:None,status:None,start:None,end:None,observed:None});}
 if fields.get("found").copied()!=Some("true")||fields.len()!=9{return Err(invalid("Incomplete execution metadata."));}
 let instance=fields.get("instance-id").and_then(|v|v.parse::<u64>().ok()).ok_or_else(||invalid("Invalid job instance identity."))?;
 let hex=fields.get("job-name-hex").ok_or(Error::Stale)?;if hex.len()>2048||hex.len()%2!=0||!hex.bytes().all(|b|b.is_ascii_hexdigit()){return Err(invalid("Invalid encoded job name."));}
 let bytes=(0..hex.len()).step_by(2).map(|i|u8::from_str_radix(&hex[i..i+2],16).map_err(|_|invalid("Invalid job name."))).collect::<Result<Vec<_>,_>>()?;let job=String::from_utf8(bytes).map_err(|_|invalid("Job name is not UTF8."))?;
 let status=fields.get("status").copied().filter(|s|matches!(*s,"STARTING"|"STARTED"|"STOPPING"|"STOPPED"|"COMPLETED"|"FAILED"|"ABANDONED"|"UNKNOWN")).ok_or_else(||invalid("Unknown execution state."))?.to_owned();
 let time=|key:&str|->Result<String,Error>{let value=fields.get(key).ok_or_else(||invalid("Missing execution time."))?;if value.len()>64||value.chars().any(char::is_control){return Err(invalid("Invalid execution time."));}Ok((*value).into())};
 Ok(Inspection{execution:id,found:true,instance:Some(instance),job:Some(job),status:Some(status),start:Some(time("start-time")?),end:Some(time("end-time")?),observed:Some(time("observed-at")?)})
}
fn inspect(project:&Project,profile:&Profile,id:u64)->Result<Inspection,Error>{parse_inspection(profile,id,&run_helper(project,&inspection_args(profile,id)?)?)}
#[derive(Clone,Debug,PartialEq,Eq)]
struct Operation{kind:String,identity:String,parameters:Vec<String>}
impl Operation {
 fn arguments(&self,profile:&Profile)->Vec<String>{
  let mut args=vec!["-cp".into(),profile.joined(),profile.launcher().into(),profile.context.clone()];
  if profile.profile=="runner-5.2.6" {match self.kind.as_str(){"start"=>{},"next"=>args.push("-next".into()),other=>args.push(format!("-{other}"))};args.push(self.identity.clone());}
  else {args.push(if self.kind=="next"{"startNextInstance".into()}else{self.kind.clone()});args.push(self.identity.clone());}
  args.extend(self.parameters.clone());args
 }
 fn input(profile:&Profile)->Result<Option<Self>,Error>{
  let mut choices:Vec<_>=[("start","Start a job"),("next","Start next instance"),("stop","Request execution stop"),("restart","Restart an execution"),("abandon","Abandon a stopped execution")].into_iter().map(|(id,label)|openidea_plugin::PickItem::new(id,label)).collect();
  if profile.profile.starts_with("operator-"){choices.push(openidea_plugin::PickItem::new("recover","Recover an execution"));}
  let Some(kind)=host::pick("Spring Batch operation",&choices)? else{return Ok(None);};if !choices.iter().any(|item|item.key==kind){return Err(invalid("Unknown Batch operation."));}
  let execution=!matches!(kind.as_str(),"start"|"next");let Some(identity)=host::input(if execution{"Job execution ID"}else{"Job name"},None)? else{return Ok(None);};
  if execution {if identity.parse::<u64>().ok().filter(|n|*n>0).is_none()||identity.bytes().any(|b|!b.is_ascii_digit()){return Err(invalid(EXECUTION_ID));}}else if identity.trim().is_empty()||identity.len()>256||identity.chars().any(char::is_control)||profile.profile=="runner-5.2.6"&&identity.starts_with('-'){return Err(invalid("Enter one job name, without line breaks or a leading -."));}
  let mut parameters=Vec::new();if kind=="start"||profile.profile=="runner-5.2.6"&&matches!(kind.as_str(),"next"|"restart") {
   let Some(text)=host::input_multiline("Job parameters, one key=value per line (empty for none)",Some(""))? else{return Ok(None);};if text.len()>4096{return Err(invalid("Job parameters exceed 4 KiB."));}
   for line in text.lines().filter(|line|!line.is_empty()) {let Some((key,value))=line.split_once('=') else{return Err(invalid("Write each parameter as key=value."));};if !literal(key)||value.chars().any(char::is_control)||parameters.len()>=16{return Err(invalid("Use at most 16 key=value parameters, without line breaks or tabs."));}parameters.push(line.into());}
  }
  if profile.profile=="operator-6.0.6"&&kind=="restart"&&!parameters.is_empty(){return Err(invalid("Operator restart accepts only an execution ID; use an empty parameter list."));}
  Ok(Some(Self{kind,identity,parameters}))
 }
}
trait Running {fn poll(&self,max:u32)->Result<host::Chunk,Error>;fn kill(&self)->Result<(),Error>;}
impl Running for Child {fn poll(&self,max:u32)->Result<host::Chunk,Error>{Child::poll(self,max)}fn kill(&self)->Result<(),Error>{Child::kill(self)}}
trait Launcher {fn inspect(&mut self,project:&Project,profile:&Profile,id:u64)->Result<Inspection,Error>{inspect(project,profile,id)}fn validate(&mut self,project:&Project,profile:&Profile)->Result<Proof,Error>;fn start(&mut self,project:&Project,args:&[String])->Result<Box<dyn Running>,Error>;}
struct Native;
impl Launcher for Native {
 fn validate(&mut self,project:&Project,profile:&Profile)->Result<Proof,Error>{probe(project,profile)}
 fn start(&mut self,project:&Project,args:&[String])->Result<Box<dyn Running>,Error>{let child=host::start(&Command::new("java").args(args),Some(project))?;child.close_stdin()?;Ok(Box::new(child))}
}
struct Session{root:String,profile:String,context:String,operation:Operation,child:Box<dyn Running>,bytes:usize,omitted:bool}
#[derive(Default)]
pub(crate) struct Runtime{sessions:BTreeMap<u64,Session>,next:u64}
impl Runtime {
 pub(crate) fn run_command(&mut self,id:&str,ctx:&Invocation)->Result<Option<String>,Error>{self.run_with(id,ctx,&mut Native)}
 fn run_with(&mut self,id:&str,ctx:&Invocation,launcher:&mut impl Launcher)->Result<Option<String>,Error>{let result=self.run_inner(id,ctx,launcher).map_err(|e|ui_kit::denied::map_capability(e,"Spring Batch runtime","process:java","Spring Structure Tools"));let _=host::set_context(ACTIVE_CONTEXT,!self.sessions.is_empty());result}
 fn run_inner(&mut self,id:&str,ctx:&Invocation,launcher:&mut impl Launcher)->Result<Option<String>,Error>{
  let project=ctx.project().ok_or_else(||invalid("Open the project to run Spring Batch jobs in."))?;let root=project.root()?;
  let foreign:Vec<_>=self.sessions.iter().filter(|(_,s)|s.root!=root).map(|(id,_)|*id).collect();
  if !foreign.is_empty(){for id in &foreign{self.sessions.get(id).ok_or(Error::Stale)?.child.kill()?;self.sessions.remove(id);}return Ok(Some(format!("Project changed: ended {} CLI process(es) of the previous project without sending a Batch stop request. Run the command again.",foreign.len())));}
  match id {
   "batch.runtime-validate"=>{let profile=Profile::load(&project)?;let proof=launcher.validate(&project,&profile)?;Ok(Some(format!("Validated {} with Java {}. Compiled context SHA-256 {}, classpath SHA-256 {}.",profile.profile,proof.java,proof.context,proof.classpath)))},
   "batch.runtime-inspect"=>{
    let profile=Profile::load(&project)?;let Some(raw)=host::input("Job execution ID to inspect",None)? else{return Ok(None);};let id=raw.parse::<u64>().ok().filter(|id|*id>0&&raw.bytes().all(|b|b.is_ascii_digit())).ok_or_else(||invalid(EXECUTION_ID))?;
    let proof=launcher.validate(&project,&profile)?;let query_args=inspection_args(&profile,id)?;
    let review=format!("Project: {}\nRunner: {}\nContext class: {}\nExecution ID: {id}\nCommand: java, with arguments:\n{}\n\nThis starts your application context to read the execution from the job repository. Its startup code runs and connects to its configured database. Nothing is written.",root,profile.profile,profile.context,query_args.join("\n"));
    if !ui_kit::prompts::confirm_exact(&format!("Inspect job execution {id}?"),&review)?{return Err(Error::Cancelled);}
    if ctx.project().and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str())||Profile::load(&project)?!=profile||launcher.validate(&project,&profile)?!=proof{return Err(Error::Stale);}
    let state=launcher.inspect(&project,&profile,id)?;let result=if !state.found {format!("Execution {id} was not found in the job repository of {}.",profile.context)}else{format!("Execution {id}: job {}, instance {}, status {}, started {}, ended {}, checked at {}. Context: {} ({}).",ui_kit::prompts::readable_source(state.job.as_deref().unwrap_or_default()),state.instance.unwrap_or_default(),state.status.unwrap_or_default(),state.start.unwrap_or_default(),state.end.unwrap_or_default(),state.observed.unwrap_or_default(),profile.context,profile.profile)};
    host::output_append(OUTPUT,&format!("\nProject: {root}\n{result}\n"))?;let _=host::output_reveal(OUTPUT);Ok(Some(result))
   },
   "batch.runtime-operate"=>{
    if self.sessions.len()>=4{return Err(invalid("Four CLI processes are already running. Run Spring Batch: Poll CLI Output to collect finished ones, or Spring Batch: Cancel CLI Process."));}
    let profile=Profile::load(&project)?;let Some(operation)=Operation::input(&profile)? else{return Ok(None);};
    let launch=matches!(operation.kind.as_str(),"start"|"next"|"restart");if launch&&self.sessions.values().filter(|s|matches!(s.operation.kind.as_str(),"start"|"next"|"restart")).count()>=3{return Err(invalid("Three jobs are already running. The fourth slot is kept for a stop, abandon or recover operation."));}
    let next=self.next.checked_add(1).ok_or(Error::Stale)?;let proof=launcher.validate(&project,&profile)?;let args=operation.arguments(&profile);
    let review=format!("Project: {}\nRunner: {}\nContext class: {}\nJava: {}\nCommand: java, with arguments:\n{}\n\nThis starts your application context: its startup code runs and connects to its configured database. A stop operation only asks the job to stop. Cancel CLI Process ends the local process without stopping the job.",root,profile.profile,profile.context,proof.java,args.join("\n"));
    if !ui_kit::prompts::confirm_exact("Run this Spring Batch operation?",&review)?{return Err(Error::Cancelled);}
    if ctx.project().and_then(|p|p.root().ok()).as_deref()!=Some(root.as_str())||Profile::load(&project)?!=profile||launcher.validate(&project,&profile)?!=proof{return Err(Error::Stale);}
    let child=launcher.start(&project,&args)?;
    let _=host::output_append(OUTPUT,&format!("\nCLI #{next} · Project: {root}\n{}: {} {} (context {})\n",profile.profile,operation.kind,operation.identity,profile.context));let _=host::output_append(OUTPUT,&format!("Compiled context SHA256: {}\nClasspath SHA256: {}\n",proof.context,proof.classpath));let _=host::output_reveal(OUTPUT);
    self.next=next;self.sessions.insert(next,Session{root,profile:profile.profile,context:profile.context,operation,child,bytes:0,omitted:false});Ok(Some(format!("CLI #{next} started. Run Spring Batch: Poll CLI Output to see its output, or Spring Batch: Cancel CLI Process to end it.")))
   },
   "batch.runtime-poll"=>{
    if self.sessions.is_empty(){return Err(invalid(NOT_RUNNING));}
    let ids:Vec<_>=self.sessions.keys().copied().collect();let mut summaries=Vec::new();
    for id in ids {
     let session=self.sessions.get_mut(&id).ok_or(Error::Stale)?;
     let chunk=session.child.poll(16*1024)?;let data=format!("{}{}",chunk.stdout,chunk.stderr);
     let mut shown=data.len().min((256*1024usize).saturating_sub(session.bytes));while !data.is_char_boundary(shown){shown-=1;}
     if shown>0{host::output_append(OUTPUT,&format!("\nCLI #{id} {} {} {} (context {}):\n{}",session.profile,session.operation.kind,session.operation.identity,session.context,&data[..shown]))?;session.bytes+=shown;}
     if shown<data.len()&&!session.omitted {session.omitted=true;host::output_append(OUTPUT,&format!("\nCLI #{id}: further output omitted (over 256 KiB). The process keeps running.\n"))?;}
     if let Some(status)=chunk.status {
      let label=if session.operation.kind=="stop"&&status==0{"Stop request acknowledged. Run Spring Batch: Inspect Execution Repository State to check whether the job stopped."}else if status==0{"CLI finished. See the output for job details."}else{"CLI failed. See the output for the error."};
      let result=format!("CLI #{id} {} {} {}: CLI exit {status}. {label}",session.profile,session.operation.kind,session.operation.identity);host::output_append(OUTPUT,&format!("\n{result}\n"))?;summaries.push(result);self.sessions.remove(&id);
     }else{summaries.push(format!("CLI #{id} {} {} {} is still running.",session.profile,session.operation.kind,session.operation.identity));}
    }
    let _=host::output_reveal(OUTPUT);Ok(Some(summaries.join("\n")))
   },
   "batch.runtime-cancel"=>{
    let ids:Vec<_>=self.sessions.keys().copied().collect();let id=match ids.as_slice(){[]=>return Err(invalid(NOT_RUNNING)),[id]=>*id,_=>{
     let items:Vec<_>=self.sessions.iter().map(|(id,s)|openidea_plugin::PickItem::new(&id.to_string(),&format!("CLI #{id}: {} {} {} (context {})",s.profile,s.operation.kind,s.operation.identity,s.context))).collect();let Some(choice)=host::pick("Choose the CLI process to end",&items)? else{return Ok(None);};choice.parse::<u64>().ok().filter(|id|self.sessions.contains_key(id)).ok_or(Error::Stale)?
    }};
    let session=self.sessions.get(&id).ok_or(Error::Stale)?;let target=if matches!(session.operation.kind.as_str(),"start"|"next"){"job"}else{"execution"};
    if !host::confirm(&format!("End CLI #{id} ({target} {})?",session.operation.identity),"This ends the local process. It does not send a Batch stop request: use Spring Batch: Run Job Operation… and choose stop for that.")?{return Ok(None);}
    self.sessions.get(&id).ok_or(Error::Stale)?.child.kill()?;self.sessions.remove(&id);Ok(Some(format!("Ended CLI #{id}. No Batch stop request was sent, so the job may still be marked as running.")))
   },
   _=>Err(invalid("Unknown Batch runtime command.")),
  }
 }
}

#[cfg(test)]
mod tests {
 use super::*;
 use openidea_plugin::{mock::MockHost,ProcessOutput};
 struct Done{chunk:std::cell::RefCell<Option<host::Chunk>>}
 impl Running for Done {fn poll(&self,_:u32)->Result<host::Chunk,Error>{Ok(self.chunk.borrow_mut().take().unwrap_or(host::Chunk{status:Some(0),..Default::default()}))}fn kill(&self)->Result<(),Error>{self.chunk.borrow_mut().take();Ok(())}}
 struct Fake;
 impl Launcher for Fake {
  fn inspect(&mut self,p:&Project,f:&Profile,id:u64)->Result<Inspection,Error>{let out=host::run_command(&Command::new("java").args(inspection_args(f,id)?),Some(p),None)?;if out.status!=Some(0){return Err(invalid("Query failed."));}parse_inspection(f,id,&out.stdout)}
  fn validate(&mut self,project:&Project,profile:&Profile)->Result<Proof,Error>{let out=host::run_command(&Command::new("java").args(profile.probe_args()),Some(project),None)?;if out.status!=Some(0){return Err(invalid("Probe failed."));}parse_probe(profile,&out.stdout)}
  fn start(&mut self,project:&Project,args:&[String])->Result<Box<dyn Running>,Error>{let out=host::run_command(&Command::new("java").args(args),Some(project),None)?;Ok(Box::new(Done{chunk:std::cell::RefCell::new(Some(host::Chunk{stdout:out.stdout,stderr:out.stderr,status:out.status}))}))}
 }
 const SOURCE:&str="package fixture; public class Context {}";
 fn setting(host:&MockHost,key:&str,value:&str){let escaped=value.replace('\\',"\\\\").replace('"',"\\\"").replace('\n',"\\n");host.set_setting(key,&format!("\"{escaped}\""));}
 fn configured()->MockHost {
  let host=MockHost::install().with_capabilities(&["project.read","process:java"]);setting(&host,"springBatch.runtimeProfile","runner-5.2.6");setting(&host,"springBatch.runtimeContext","fixture.Context");setting(&host,"springBatch.runtimeContextSource","src/fixture/Context.java");setting(&host,"springBatch.runtimeClasspath","target/classes\ntarget/dependency/spring-batch-core-5.2.6.jar");
  host.add_file("/project/src/fixture/Context.java",SOURCE);host.add_dir("/project/target/classes");host.add_file("/project/target/dependency/spring-batch-core-5.2.6.jar","jar");host
 }
 fn proof(profile:&str,source:&str,classpath:&str)->String{format!("OPENIDEA_BATCH_PROFILE_V1\nprofile={profile}\nsource-sha256={}\ncontext-sha256={}\nclasspath-sha256={classpath}\njava=25\n",digest(source),"b".repeat(64))}
 fn answer(host:&MockHost,kind:&str,id:&str,params:&str){host.answer_next_prompt(Some(&[kind]));host.answer_next_prompt(Some(&[id]));if matches!(kind,"start"|"next"|"restart"){host.answer_next_prompt(Some(&[params]));}host.answer_next_prompt(Some(&["yes"]));}
 fn process(host:&MockHost){host.on_process("java",|args,_|Ok(if args.first().map(String::as_str)==Some("--source"){ProcessOutput{status:Some(0),stdout:proof("runner-5.2.6",SOURCE,&"a".repeat(64)),stderr:String::new()}}else{ProcessOutput{status:Some(0),stdout:"fixture completed\n".into(),stderr:String::new()}}));}
 #[test]
 fn profile_is_unavailable_until_explicit_and_path_source_identity_is_checked() {
  let h=MockHost::install();assert!(Profile::load(&h.invocation().project().unwrap()).is_err());assert!(h.process_runs().is_empty());
  let h=configured();let project=h.invocation().project().unwrap();assert!(Profile::load(&project).is_ok());
  for path in ["../outside.jar","target/*.jar","/other/classes","target/classes\ntarget/classes"]{setting(&h,"springBatch.runtimeClasspath",path);assert!(Profile::load(&project).is_err());}
  setting(&h,"springBatch.runtimeClasspath","target/classes\ntarget/dependency/spring-batch-core-5.2.6.jar");setting(&h,"springBatch.runtimeContext","other.Context");assert!(Profile::load(&project).is_err());
 }
 #[test]
 fn managed_operation_reprobes_exact_profile_and_preserves_argv_boundaries() {
  let h=configured();process(&h);answer(&h,"start","fixture","name=value with spaces\ncount=3");let mut runtime=Runtime::default();assert!(runtime.run_with("batch.runtime-operate",&h.invocation(),&mut Fake).unwrap().unwrap().contains("started"));let runs=h.process_runs();assert_eq!(runs.len(),3);assert!(runs[..2].iter().all(|r|r.args[0]=="--source"));let argv=&runs[2].args;assert_eq!(argv, &vec!["-cp".to_owned(),"/project/target/classes:/project/target/dependency/spring-batch-core-5.2.6.jar".into(),RUNNER.into(),"fixture.Context".into(),"fixture".into(),"name=value with spaces".into(),"count=3".into()]);
  let result=runtime.run_with("batch.runtime-poll",&h.invocation(),&mut Fake).unwrap().unwrap();assert!(result.contains("CLI exit 0"));assert!(runtime.sessions.is_empty());
 }
 #[test]
 fn changed_artifact_proof_denied_java_and_cancelled_review_do_not_launch_job() {
  let h=configured();let counter=std::rc::Rc::new(std::cell::Cell::new(0));let count=counter.clone();h.on_process("java",move|_,_|{count.set(count.get()+1);Ok(ProcessOutput{status:Some(0),stdout:proof("runner-5.2.6",SOURCE,&if count.get()==1{"a".repeat(64)}else{"c".repeat(64)}),stderr:String::new()})});answer(&h,"start","fixture","");assert!(matches!(Runtime::default().run_with("batch.runtime-operate",&h.invocation(),&mut Fake),Err(Error::Stale)));assert_eq!(h.process_runs().len(),2);
  let h=configured();process(&h);h.answer_next_prompt(Some(&["start"]));h.answer_next_prompt(Some(&["fixture"]));h.answer_next_prompt(Some(&[""]));h.answer_next_prompt(None);assert!(matches!(Runtime::default().run_with("batch.runtime-operate",&h.invocation(),&mut Fake),Err(Error::Cancelled)));assert_eq!(h.process_runs().len(),1);
  let h=configured();h.on_process("java",|_,_|Err(Error::Denied("process:java".into())));answer(&h,"start","fixture","");assert!(Runtime::default().run_with("batch.runtime-operate",&h.invocation(),&mut Fake).is_err());assert_eq!(h.process_runs().len(),1);
 }
 #[test]
 fn spring_batch_runtime_denied_names_permission() {
  let h=configured();h.on_process("java",|_,_|Err(Error::Denied("process:java".into())));answer(&h,"start","fixture","");let error=Runtime::default().run_with("batch.runtime-operate",&h.invocation(),&mut Fake).unwrap_err().to_string();assert!(error.contains("needs the process:java permission"),"{error}");assert!(error.contains("Settings › Plugins › Spring Structure Tools"),"{error}");
 }
 #[test]
 fn stop_acknowledgement_cancel_and_project_change_have_distinct_semantics() {
  let h=configured();process(&h);answer(&h,"stop","42","");let mut runtime=Runtime::default();runtime.run_with("batch.runtime-operate",&h.invocation(),&mut Fake).unwrap();assert!(h.process_runs().last().unwrap().args.contains(&"-stop".into()));let result=runtime.run_with("batch.runtime-poll",&h.invocation(),&mut Fake).unwrap().unwrap();assert!(result.contains("Stop request acknowledged"));assert!(result.contains("check whether the job stopped"));
  answer(&h,"start","fixture","");runtime.run_with("batch.runtime-operate",&h.invocation(),&mut Fake).unwrap();h.answer_next_prompt(Some(&["yes"]));let result=runtime.run_with("batch.runtime-cancel",&h.invocation(),&mut Fake).unwrap().unwrap();assert!(result.contains("No Batch stop request was sent"));assert!(runtime.sessions.is_empty());
  answer(&h,"start","fixture","");runtime.run_with("batch.runtime-operate",&h.invocation(),&mut Fake).unwrap();h.set_project_root(Some("/other"));let before=h.process_runs().len();let result=runtime.run_with("batch.runtime-poll",&h.invocation(),&mut Fake).unwrap().unwrap();assert!(result.contains("Project changed"));assert!(runtime.sessions.is_empty());assert_eq!(h.process_runs().len(),before);
 }
 #[test]
 fn stop_control_can_run_while_original_start_child_remains_owned_and_running() {
  struct Held(std::rc::Rc<std::cell::Cell<bool>>);
  impl Running for Held {fn poll(&self,_:u32)->Result<host::Chunk,Error>{Ok(host::Chunk::default())}fn kill(&self)->Result<(),Error>{self.0.set(true);Ok(())}}
  struct HoldStarts(std::rc::Rc<std::cell::Cell<bool>>);
  impl Launcher for HoldStarts {fn validate(&mut self,p:&Project,f:&Profile)->Result<Proof,Error>{Fake.validate(p,f)}fn start(&mut self,p:&Project,args:&[String])->Result<Box<dyn Running>,Error>{if args.iter().any(|a|a=="-stop"){Fake.start(p,args)}else{let _=host::run_command(&Command::new("java").args(args),Some(p),None)?;Ok(Box::new(Held(self.0.clone())))}}}
  let h=configured();process(&h);let killed=std::rc::Rc::new(std::cell::Cell::new(false));let mut backend=HoldStarts(killed.clone());let mut runtime=Runtime::default();answer(&h,"start","fixture","");runtime.run_with("batch.runtime-operate",&h.invocation(),&mut backend).unwrap();assert_eq!(runtime.sessions.len(),1);
  answer(&h,"stop","42","");runtime.run_with("batch.runtime-operate",&h.invocation(),&mut backend).unwrap();assert_eq!(runtime.sessions.len(),2);assert!(!killed.get());
  let result=runtime.run_with("batch.runtime-poll",&h.invocation(),&mut backend).unwrap().unwrap();assert!(result.contains("still running"));assert!(result.contains("Stop request acknowledged"));assert_eq!(runtime.sessions.len(),1);assert!(runtime.sessions.contains_key(&1));assert!(!killed.get());
  h.answer_next_prompt(Some(&["yes"]));runtime.run_with("batch.runtime-cancel",&h.invocation(),&mut backend).unwrap();assert!(killed.get());assert!(runtime.sessions.is_empty());
 }
 #[test]
 fn poll_and_cancel_are_enabled_only_while_a_cli_runs_and_cancel_asks_first() {
  let manifest=include_str!("../plugin.toml");
  for id in ["batch.runtime-poll","batch.runtime-cancel"]{let command=manifest.split("[[contributes.commands]]").find(|c|c.contains(&format!("id = \"{id}\""))).unwrap();assert!(command.contains(&format!("when = \"plugin.aritti.spring-integration.{ACTIVE_CONTEXT}\"")),"{id}");}
  let h=configured();process(&h);let mut runtime=Runtime::default();assert!(runtime.run_with("batch.runtime-poll",&h.invocation(),&mut Fake).unwrap_err().to_string().contains("Run Job Operation"));
  struct Held(std::rc::Rc<std::cell::Cell<bool>>);
  impl Running for Held {fn poll(&self,_:u32)->Result<host::Chunk,Error>{Ok(host::Chunk::default())}fn kill(&self)->Result<(),Error>{self.0.set(true);Ok(())}}
  let killed=std::rc::Rc::new(std::cell::Cell::new(false));runtime.sessions.insert(7,Session{root:"/project".into(),profile:"runner-5.2.6".into(),context:"fixture.Context".into(),operation:Operation{kind:"start".into(),identity:"fixture".into(),parameters:vec![]},child:Box::new(Held(killed.clone())),bytes:0,omitted:false});
  runtime.run_with("batch.runtime-poll",&h.invocation(),&mut Fake).unwrap();assert_eq!(h.context_keys()[ACTIVE_CONTEXT],openidea_plugin::ContextValue::Flag(true));
  h.answer_next_prompt(None);assert_eq!(runtime.run_with("batch.runtime-cancel",&h.invocation(),&mut Fake).unwrap(),None);assert!(!killed.get());assert_eq!(runtime.sessions.len(),1);
  h.answer_next_prompt(Some(&["yes"]));runtime.run_with("batch.runtime-cancel",&h.invocation(),&mut Fake).unwrap();assert!(killed.get());assert_eq!(h.context_keys()[ACTIVE_CONTEXT],openidea_plugin::ContextValue::Flag(false));
 }
 #[test]
 fn unconfigured_runtime_names_the_settings_page() {
  let h=MockHost::install();let error=Profile::load(&h.invocation().project().unwrap()).unwrap_err().to_string();assert!(error.contains("Settings › Plugins › Spring Structure Tools"),"{error}");assert!(error.contains("Batch command-line runner"),"{error}");
 }
 #[test]
 fn probe_is_embedded_and_materialized_only_in_private_hash_named_source() {
  let h=configured();let profile=Profile::load(&h.invocation().project().unwrap()).unwrap();assert!(profile.probe.contains(&digest(PROBE_SOURCE)));ensure_probe(&profile).unwrap();assert_eq!(host::fs::read_file(&profile.probe).unwrap(),PROBE_SOURCE);host::fs::write_file(&profile.probe,"changed").unwrap();ensure_probe(&profile).unwrap();assert_eq!(host::fs::read_file(&profile.probe).unwrap(),PROBE_SOURCE);assert!(h.process_runs().is_empty());
 }
 #[test]
 fn operator_recover_next_and_restart_take_only_their_documented_identity() {
  let h=configured();let mut p=Profile::load(&h.invocation().project().unwrap()).unwrap();p.profile="operator-6.0.6".into();
  for (kind,id,wire) in [("recover","37","recover"),("next","job with spaces","startNextInstance"),("restart","37","restart")] {
   h.answer_next_prompt(Some(&[kind]));h.answer_next_prompt(Some(&[id]));h.answer_next_prompt(Some(&["unconsumed"]));let operation=Operation::input(&p).unwrap().unwrap();assert!(operation.parameters.is_empty());assert_eq!(&operation.arguments(&p)[3..],&["fixture.Context".to_owned(),wire.into(),id.into()]);assert_eq!(host::input("still pending",None).unwrap().as_deref(),Some("unconsumed"));
  }
 }
 fn inspection_record(id:u64,status:&str)->String{format!("startup log\nOPENIDEA_BATCH_EXECUTION_V1\nprofile=runner-5.2.6\nexecution-id={id}\nfound=true\ninstance-id=5\njob-name-hex=66697874757265\nstatus={status}\nstart-time=2026-11-11T00:00:00\nend-time=-\nobserved-at=2026-11-11T00:00:01Z\n")}
 #[test]
 fn repository_inspection_is_separately_reviewed_and_reports_typed_observed_state() {
  let h=configured();h.on_process("java",|args,_|Ok(ProcessOutput{status:Some(0),stdout:if args.first().map(String::as_str)==Some("--source"){proof("runner-5.2.6",SOURCE,&"a".repeat(64))}else{inspection_record(42,"STOPPED")},stderr:String::new()}));h.answer_next_prompt(Some(&["42"]));h.answer_next_prompt(Some(&["yes"]));let mut runtime=Runtime::default();let message=runtime.run_with("batch.runtime-inspect",&h.invocation(),&mut Fake).unwrap().unwrap();assert!(message.contains("Execution 42:"));assert!(message.contains("status STOPPED"));assert!(message.contains("job fixture"));assert_eq!(h.process_runs().len(),3);let args=&h.process_runs()[2].args;assert_eq!(args[0],"--class-path");assert_eq!(args.last().unwrap(),"42");assert!(runtime.sessions.is_empty());
  let profile=Profile::load(&h.invocation().project().unwrap()).unwrap();assert!(parse_inspection(&profile,41,&inspection_record(42,"STOPPED")).is_err());assert!(parse_inspection(&profile,42,&inspection_record(42,"FUTURE_UNKNOWN_STATUS")).is_err());assert!(parse_inspection(&profile,42,&inspection_record(42,"STOPPED").replace("66697874757265","zz")).is_err());
  let missing=parse_inspection(&profile,42,"OPENIDEA_BATCH_EXECUTION_V1\nprofile=runner-5.2.6\nexecution-id=42\nfound=false\n").unwrap();assert!(!missing.found);assert!(missing.status.is_none());
 }
 #[test]
 fn inspection_cancel_does_not_invoke_context_startup_or_poll_existing_sessions() {
  let h=configured();process(&h);h.answer_next_prompt(Some(&["42"]));h.answer_next_prompt(None);assert!(matches!(Runtime::default().run_with("batch.runtime-inspect",&h.invocation(),&mut Fake),Err(Error::Cancelled)));assert_eq!(h.process_runs().len(),1);
 }
 #[test]
 fn operation_output_limit_omits_display_without_killing_business_execution() {
  struct Noisy{polls:std::cell::Cell<u32>,killed:std::rc::Rc<std::cell::Cell<bool>>}
  impl Running for Noisy{fn poll(&self,_:u32)->Result<host::Chunk,Error>{let n=self.polls.get();self.polls.set(n+1);Ok(if n==0{host::Chunk{stdout:"é".repeat(150*1024),stderr:String::new(),status:None}}else{host::Chunk{status:Some(0),..Default::default()}})}fn kill(&self)->Result<(),Error>{self.killed.set(true);Ok(())}}
  let h=configured();let killed=std::rc::Rc::new(std::cell::Cell::new(false));let mut runtime=Runtime::default();runtime.sessions.insert(1,Session{root:"/project".into(),profile:"runner-5.2.6".into(),context:"fixture.Context".into(),operation:Operation{kind:"start".into(),identity:"fixture".into(),parameters:vec![]},child:Box::new(Noisy{polls:std::cell::Cell::new(0),killed:killed.clone()}),bytes:0,omitted:false});
  let result=runtime.run_with("batch.runtime-poll",&h.invocation(),&mut Fake).unwrap().unwrap();assert!(result.contains("still running"));assert_eq!(runtime.sessions.len(),1);assert!(!killed.get());let output=&h.output()[OUTPUT];assert!(output.contains("further output omitted"));assert!(output.len()<256*1024+2048);
  let result=runtime.run_with("batch.runtime-poll",&h.invocation(),&mut Fake).unwrap().unwrap();assert!(result.contains("CLI exit 0"));assert!(runtime.sessions.is_empty());assert!(!killed.get());
 }
 #[test]
 fn runner_xml_context_is_explicit_and_operator_does_not_guess_xml_support() {
  let h=configured();h.add_file("/project/src/jobs.xml","<beans xmlns='http://www.springframework.org/schema/beans'/>");setting(&h,"springBatch.runtimeContextSource","src/jobs.xml");setting(&h,"springBatch.runtimeContext","jobs.xml");let p=Profile::load(&h.invocation().project().unwrap()).unwrap();assert_eq!(p.kind,"xml");assert!(p.probe_args().contains(&"xml".into()));setting(&h,"springBatch.runtimeProfile","operator-6.0.6");assert!(Profile::load(&h.invocation().project().unwrap()).is_err());
 }
 #[test]
 fn kotlin_context_source_requires_an_exact_declared_compiled_class_identity() {
  let h=configured();h.add_file("/project/src/fixture/Context.kt","package fixture\nopen class Context\n");setting(&h,"springBatch.runtimeContextSource","src/fixture/Context.kt");let p=Profile::load(&h.invocation().project().unwrap()).unwrap();assert_eq!(p.context,"fixture.Context");assert_eq!(p.kind,"java");setting(&h,"springBatch.runtimeContext","fixture.ContextKt");assert!(Profile::load(&h.invocation().project().unwrap()).is_err());
 }
 #[test]
 fn classpath_join_uses_native_windows_drive_and_unc_separator() {
  let h=configured();let mut p=Profile::load(&h.invocation().project().unwrap()).unwrap();p.root="C:/project".into();assert!(p.joined().contains(';'));p.root="//server/share/project".into();assert!(p.joined().contains(';'));p.root="/project".into();assert!(!p.joined().contains(';'));
 }
 #[test]
 fn execution_identity_parameters_and_operator_wire_contract_are_strict() {
  let h=configured();let project=h.invocation().project().unwrap();let mut profile=Profile::load(&project).unwrap();profile.profile="operator-6.0.6".into();
  let operation=Operation{kind:"stop".into(),identity:"27".into(),parameters:vec![]};assert_eq!(&operation.arguments(&profile)[2..], &[OPERATOR.to_owned(),"fixture.Context".into(),"stop".into(),"27".into()]);
  for id in ["0","-3","3 x","+3","18446744073709551616"]{h.answer_next_prompt(Some(&["stop"]));h.answer_next_prompt(Some(&[id]));assert!(Operation::input(&profile).is_err());}
  let output=proof("runner-5.2.6","changed source",&"a".repeat(64));assert!(parse_probe(&Profile::load(&project).unwrap(),&output).is_err());
 }
}
