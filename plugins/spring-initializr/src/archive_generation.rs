//! Complete ZIP import through an explicitly approved, isolated Python adapter.
use super::*;
use openidea_plugin::{HttpRequest, host::Command};
use serde_json::{Value, json};
const ADAPTER: &str = include_str!("archive_adapter.py");
fn denied(error:Error)->Error {
    match error {
        Error::Denied(cap) if cap.starts_with("net") => Error::Invalid(format!("Allow {cap} for Spring Initializr in Settings › Plugins.")),
        Error::Denied(_) => Error::Invalid("Allow process:python3 for Spring Initializr in Settings › Plugins.".into()),
        other => other,
    }
}
fn adapter(ctx: &Invocation, request: &Value) -> Result<Value,Error> {
    adapter_raw(ctx, request).map_err(denied)
}
/// The adapter call with permission denials left as `Error::Denied`, so Generate and Open can fall back to the browser.
fn adapter_raw(ctx: &Invocation, request: &Value) -> Result<Value,Error> {
    let output=host::run_command(&Command::new("python3").args(["-I","-c",ADAPTER]),ctx.project().as_ref(),Some(&request.to_string()))?;
    if output.status!=Some(0) {
        let staging=output.stderr.lines().find_map(|line|serde_json::from_str::<Value>(line).ok()?["staging"].as_str().map(str::to_owned));
        let kept=staging.map_or(String::new(),|path|format!(" Staging folder kept for review: {path}."));
        return Err(Error::Failed(format!("Archive adapter refused or failed. No project was opened.{kept} {}",ui_kit::prompts::readable_source(&output.stderr.chars().take(2048).collect::<String>()))));
    }
    if output.stdout.len()>256<<10 {return Err(Error::Invalid("Archive adapter result exceeds 256 KiB.".into()));}
    let value=crate::strict_json::parse(&output.stdout,256<<10).map_err(|_|Error::Invalid("Malformed archive adapter result.".into()))?;
    if value["error"]=="not-local" {return Err(Error::Invalid("This folder doesn't exist on the machine running the project.".into()));}
    Ok(value)
}
fn preview(proof:&Value,archive:bool)->Result<String,Error> {
    let files=proof["files"].as_array().filter(|files|!files.is_empty()&&files.len()<=512).ok_or(Error::Stale)?;
    let mut text=format!("Destination: {}\nArchive SHA-256: {}\nAdapter: Python {} / {}\n\n",proof["target"].as_str().ok_or(Error::Stale)?,proof["sha256"].as_str().ok_or(Error::Stale)?,proof["python"].as_str().ok_or(Error::Stale)?,proof["platform"].as_str().ok_or(Error::Stale)?);
    for file in files {text.push_str(&format!("{} · {} bytes · mode {} · SHA-256 {}\n",file["path"].as_str().ok_or(Error::Stale)?,file["bytes"],file["mode"],file["sha256"].as_str().ok_or(Error::Stale)?));}
    if let Some(dirs)=proof["directories"].as_array() {for directory in dirs {text.push_str(&format!("Directory: {}\n",directory.as_str().ok_or(Error::Stale)?));}}
    if archive {text.push_str("\nAll archive files, including binary wrappers, are extracted as data into a private staging directory. Unix permission bits are retained (group/other write bits cleared, owner read/write forced; setuid/setgid/sticky, links and special entries are refused) and ZIP entry modification times are applied to files. No wrapper, build, project script or generated executable is run. The complete staged tree is committed with an exclusive directory rename. An occupied destination is never merged/replaced. Failed staging is retained; no automatic cleanup. After commit, request the normal project trust/open flow in a new window.");}
    Ok(text)
}
impl Launcher {
    pub(crate) fn review_cli_tree(&mut self,ctx:&Invocation,root:&str,target:&str)->Result<(),Error> {
        if ctx.project().and_then(|p|p.root().ok()).as_deref()!=Some(root) || self.owner!=format!("project:{root}") {return Err(Error::Stale)}
        let proof=adapter(ctx,&json!({"operation":"inspect-tree","target":target}))?;
        if proof["target"].as_str()!=Some(target) || !proof["files"].as_array().is_some_and(|files|files.iter().any(|f|matches!(f["path"].as_str(),Some("pom.xml"|"build.gradle"|"build.gradle.kts")))) {return Err(Error::Invalid("No root build descriptor in the generated folder.".into()))}
        let message=format!("{}\n\nThis is the current physical tree after a successful CLI exit, not verification of the build or of server-generated source identity. No generated executable or build is run. Open through the normal IDE trust flow?",preview(&proof,false)?);
        if !ui_kit::prompts::confirm_exact("Open reviewed CLI result?",&message)? {return Ok(())}
        if ctx.project().and_then(|p|p.root().ok()).as_deref()!=Some(root) || self.owner!=format!("project:{root}") {return Err(Error::Stale)}
        let verified=adapter(ctx,&json!({"operation":"verify-tree","target":target,"proof":proof}))?;
        if verified!=proof {return Err(Error::Stale)}
        host::open_project(target,true)
    }
    /// Generate and Open; without its capabilities, the browser download instead (nothing is written before a denial).
    pub(crate) fn generate_archive(&mut self,ctx:&Invocation)->Result<Option<String>,Error> {
        let result=match self.missing_archive_capability() {Some(capability)=>Err(Error::Denied(capability.into())),None=>self.generate_reviewed_archive(ctx)};
        let Err(Error::Denied(capability))=result else {return result};
        if let Some(problem)=self.problem() {return Err(Error::Invalid(problem));}
        Self::warn(&format!("{} Downloading {}.zip in the browser instead.",ui_kit::denied::message("Generate and Open",&capability,"Spring Initializr Launcher"),self.get("artifactId")));
        host::open_url(&self.starter_url())?;
        Ok(None)
    }
    fn generate_reviewed_archive(&mut self,ctx:&Invocation)->Result<Option<String>,Error> {
        let _=self.changed();
        if self.server!=START {return Err(Error::Invalid("Complete archive generation uses the declared start.spring.io service.".into()));}
        self.load_metadata(false);
        if let Some(problem)=self.problem() {return Err(Error::Invalid(problem));}
        let artifact=self.get("artifactId");
        let owner=self.owner.clone();let query=self.query();let metadata=self.metadata.clone();
        let root=ctx.project().and_then(|p|p.root().ok());
        let Some(parent)=host::pick_directory("Choose a parent for a NEW generated project",root.as_deref())? else {return Ok(None)};
        if self.owner!=owner || self.server!=START || self.query()!=query || self.metadata!=metadata {return Err(Error::Stale)}
        let task=host::begin_task("Generating reviewed Spring project",true);
        if task.is_cancelled(){return Err(Error::Cancelled)}
        let response=host::fetch(&HttpRequest::get(&format!("{START}/starter.zip?{query}")))?;
        if task.is_cancelled(){return Err(Error::Cancelled)}
        if response.status!=200 || response.body.is_empty() || response.body.len()>8<<20 {return Err(Error::Invalid("Project ZIP failed or exceeds 8 MiB; no destination files were created.".into()));}
        let archive=format!("{}/generation-archive.zip",host::fs::plugin_dir()?.trim_end_matches('/'));
        host::fs::write_bytes(&archive,&response.body)?;
        let outcome=(||->Result<Option<String>,Error> {
        let mut request=json!({"operation":"inspect","archive":archive,"parent":parent,"artifact":artifact});
        let proof=adapter_raw(ctx,&request)?;
        if task.is_cancelled(){return Err(Error::Cancelled)}
        let descriptor=match self.get("type").as_str(){"maven-project"=>"pom.xml","gradle-project"=>"build.gradle","gradle-project-kotlin"=>"build.gradle.kts",_=>return Err(Error::Invalid("Unsupported generated build type.".into()))};
        if !proof["files"].as_array().is_some_and(|files|files.iter().any(|file|file["path"].as_str()==Some(descriptor))) {return Err(Error::Invalid("The archive does not contain the selected root build descriptor.".into()));}
        let review=format!("Downloaded: {START}/starter.zip?{query}\nValidated by the approved python3 adapter (standard library only).\n{}",preview(&proof,true)?);
        if !ui_kit::prompts::confirm_exact("Generate and open reviewed Spring project?",&review)? {return Ok(None)}
        if self.owner!=owner || self.server!=START || self.query()!=query || self.metadata!=metadata {return Err(Error::Stale)}
        if task.is_cancelled(){return Err(Error::Cancelled)}
        request["operation"]=json!("extract");request["proof"]=proof.clone();
        let result=adapter_raw(ctx,&request)?;
        if task.is_cancelled(){return Err(Error::Cancelled)}
        let target=proof["target"].as_str().ok_or(Error::Stale)?;
        if result["committed"]!=true || result["target"].as_str()!=Some(target) || result["sha256"]!=proof["sha256"] {return Err(Error::Stale)}
        host::open_project(target,true)?;
        Ok(Some(format!("Generated {} archive files at {target}; requested normal project trust/open. No wrapper or build executed.",result["files"])))
        })();
        let _=adapter_raw(ctx,&json!({"operation":"remove-archive","archive":archive}));
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::{Cursor,Write},process::{Command as NativeCommand,Stdio},hash::BuildHasher};
    struct Scratch(std::path::PathBuf);
    impl Scratch {fn new()->Self {let random=std::collections::hash_map::RandomState::new();let path=std::env::temp_dir().join(format!("initializr-archive-test-{:016x}",random.hash_one(0u8)));std::fs::create_dir(&path).unwrap();Self(std::fs::canonicalize(path).unwrap())}}
    impl Drop for Scratch {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
    fn zip(entries:&[(&str,&[u8],u32)])->Vec<u8> {
        let mut zip=zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name,data,mode) in entries {zip.start_file(*name,zip::write::SimpleFileOptions::default().unix_permissions(*mode)).unwrap();zip.write_all(data).unwrap();}
        zip.finish().unwrap().into_inner()
    }
    fn run(request:&Value)->std::process::Output {
        let mut child=NativeCommand::new("python3").args(["-I","-c",ADAPTER]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("Python 3 adapter integration tests require Python 3");
        child.stdin.take().unwrap().write_all(request.to_string().as_bytes()).unwrap();child.wait_with_output().unwrap()
    }
    #[test]
    fn command_download_review_extract_and_open_are_separate_and_cancellation_never_extracts() {
        use openidea_plugin::mock::MockHost;
        for mode in 0..4 {
            let host=MockHost::install().with_capabilities(&["storage","project.read","net:start.spring.io","process:python3"]);
            if mode==3 {host.set_project_root(None);}
            let mut launcher=Launcher::default();launcher.bind(&host.invocation()).unwrap();let _=launcher.changed();
            launcher.metadata=Some(json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]},"dependencies":{"values":[]}}));
            host.on_fetch(|request| {assert!(request.url.starts_with("https://start.spring.io/starter.zip?"));Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"archive inspected by scripted adapter".to_vec()})});
            host.on_process("python3",move |args,stdin| {
                assert_eq!(&args[..2],["-I","-c"]);assert_eq!(args[2],ADAPTER);
                let request:Value=serde_json::from_str(stdin.unwrap()).unwrap();
                let proof=json!({"parent":"/outside","identity":[1,2],"target":"/outside/demo","python":"3.12.0","platform":"darwin","sha256":"reviewed-sha","directories":[],"files":[{"path":"pom.xml","bytes":10,"mode":420,"sha256":"source-sha"}]});
                let result=if request["operation"]=="remove-archive" {json!({"removed":true})} else if request["operation"]=="inspect" {proof} else {
                    assert_eq!(request["proof"],proof);
                    if mode==2 {return Ok(openidea_plugin::ProcessOutput{status:Some(1),stdout:String::new(),stderr:"Destination exists; no merge or overwrite".into()})}
                    json!({"target":"/outside/demo","sha256":"reviewed-sha","files":1,"committed":true})
                };
                Ok(openidea_plugin::ProcessOutput{status:Some(0),stdout:result.to_string(),stderr:String::new()})
            });
            // One folder pick and one exact confirmation: no second prompt is scripted.
            host.answer_next_prompt(Some(&["/outside"]));host.answer_next_prompt(if mode==1 {None} else {Some(&[])});
            let result=launcher.generate_archive(&host.invocation());
            assert_eq!(result.is_ok(),mode!=2);
            assert_eq!(host.process_runs().len(),if mode==1 {2} else {3});
            assert_eq!(host.opened_projects().len(),usize::from(matches!(mode,0|3)));
            if matches!(mode,0|3) {assert_eq!(host.opened_projects(),vec![("/outside/demo".into(),true)]);}
        }
    }
    fn scripted_generation(fail_extract:Option<&'static str>,not_local:bool)->(openidea_plugin::mock::MockHost,Result<Option<String>,Error>,std::rc::Rc<std::cell::RefCell<Vec<Value>>>) {
        use openidea_plugin::mock::MockHost;
        let host=MockHost::install().with_capabilities(&["storage","project.read","net:start.spring.io","process:python3"]);
        let mut launcher=Launcher::default();launcher.bind(&host.invocation()).unwrap();let _=launcher.changed();
        launcher.metadata=Some(json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]},"dependencies":{"values":[]}}));
        host.on_fetch(|_|Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"archive".to_vec()}));
        let requests=std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));let seen=requests.clone();
        host.on_process("python3",move |_,stdin| {
            let request:Value=serde_json::from_str(stdin.unwrap()).unwrap();seen.borrow_mut().push(request.clone());
            let proof=json!({"parent":"/outside","identity":[1,2],"target":"/outside/demo","python":"3.12.0","platform":"darwin","sha256":"reviewed-sha","directories":[],"files":[{"path":"pom.xml","bytes":10,"mode":420,"sha256":"source-sha"}]});
            let out=|value:Value|Ok(openidea_plugin::ProcessOutput{status:Some(0),stdout:value.to_string(),stderr:String::new()});
            match request["operation"].as_str().unwrap() {
                "remove-archive"=>out(json!({"removed":true})),
                _ if not_local=>out(json!({"error":"not-local"})),
                "inspect"=>out(proof),
                _=>match fail_extract {
                    Some(stderr)=>Ok(openidea_plugin::ProcessOutput{status:Some(1),stdout:String::new(),stderr:stderr.into()}),
                    None=>out(json!({"target":"/outside/demo","sha256":"reviewed-sha","files":1,"committed":true})),
                },
            }
        });
        host.answer_next_prompt(Some(&["/outside"]));host.answer_next_prompt(Some(&[]));host.answer_next_prompt(Some(&[]));
        let result=launcher.generate_archive(&host.invocation());
        (host,result,requests)
    }
    fn removes_archive_last(requests:&std::cell::RefCell<Vec<Value>>)->bool {
        let requests=requests.borrow();let last=requests.last().unwrap();
        last["operation"]=="remove-archive"&&last["archive"].as_str().is_some_and(|path|path.ends_with("/generation-archive.zip"))
    }
    #[test]
    fn archive_removed_after_success() {
        let (host,result,requests)=scripted_generation(None,false);
        assert!(result.is_ok());assert!(removes_archive_last(&requests));
        assert_eq!(host.opened_projects().len(),1);
    }
    #[test]
    fn archive_removed_after_refusal() {
        let (_host,result,requests)=scripted_generation(Some("Destination exists"),false);
        assert!(result.is_err());assert!(removes_archive_last(&requests));
    }
    #[test]
    fn failed_extract_reports_staging_path() {
        let (_host,result,_)=scripted_generation(Some("{\"committed\": false, \"staging\": \"/outside/.initializr-ab12\"}\nOSError: disk full"),false);
        let message=result.unwrap_err().to_string();
        assert!(message.contains("Staging folder kept for review: /outside/.initializr-ab12."),"{message}");
    }
    #[test]
    fn generate_refuses_parent_missing_on_runner() {
        let (host,result,requests)=scripted_generation(None,true);
        let message=result.unwrap_err().to_string();
        assert!(message.contains("doesn't exist on the machine running the project"),"{message}");
        assert!(host.opened_projects().is_empty());assert!(removes_archive_last(&requests));
        let scratch=Scratch::new();
        let output=run(&json!({"operation":"inspect","archive":scratch.0.join("none.zip"),"parent":scratch.0.join("missing"),"artifact":"demo"}));
        assert!(output.status.success());assert_eq!(serde_json::from_slice::<Value>(&output.stdout).unwrap(),json!({"error":"not-local"}));
    }
    #[test]
    fn real_adapter_removes_only_the_generation_archive() {
        let scratch=Scratch::new();let archive=scratch.0.join("generation-archive.zip");let other=scratch.0.join("keep.zip");
        std::fs::write(&archive,b"zip").unwrap();std::fs::write(&other,b"zip").unwrap();
        assert!(run(&json!({"operation":"remove-archive","archive":archive})).status.success());assert!(!archive.exists());
        assert!(run(&json!({"operation":"remove-archive","archive":archive})).status.success());
        assert!(!run(&json!({"operation":"remove-archive","archive":other})).status.success());assert!(other.exists());
    }
    #[test]
    fn known_missing_capability_downloads_in_the_browser_without_prompts() {
        use openidea_plugin::mock::MockHost;
        let host=MockHost::install().with_capabilities(&["storage","project.read","net:start.spring.io"]);
        let mut launcher=Launcher::default();launcher.activate(&host.context("aritti.spring-initializr")).unwrap();
        host.answer_next_prompt(Some(&["/outside"]));
        launcher.handle("initializr",ViewEvent::Button("generate-archive".into()),&host.invocation()).unwrap();
        assert!(host.opened_urls()[0].starts_with("https://start.spring.io/starter.zip?"));
        assert!(host.notifications()[0].body.contains("process:python3"));
        assert!(host.process_runs().is_empty() && host.opened_projects().is_empty() && host.fetches()==0);
    }
    #[test]
    fn denial_during_generation_falls_back_to_the_browser_and_writes_nothing() {
        use openidea_plugin::mock::MockHost;
        let host=MockHost::install().with_capabilities(&["storage","project.read","net:start.spring.io"]);
        let mut launcher=Launcher::default();launcher.bind(&host.invocation()).unwrap();let _=launcher.changed();
        launcher.metadata=Some(json!({"bootVersion":{"default":"3.5.16","values":[{"id":"3.5.16"}]},"dependencies":{"values":[]}}));
        host.on_fetch(|_|Ok(openidea_plugin::HttpResponse{status:200,headers:vec![],body:b"zip".to_vec()}));
        host.answer_next_prompt(Some(&["/outside"]));
        assert_eq!(launcher.generate_archive(&host.invocation()).unwrap(),None);
        assert!(host.opened_urls()[0].starts_with("https://start.spring.io/starter.zip?"));
        assert!(host.notifications().iter().any(|n|n.body.contains("needs the process:python3 permission. Allow it in Settings › Plugins › Spring Initializr Launcher. Downloading")));
        assert!(host.opened_projects().is_empty());
    }
    #[test]
    fn actual_generated_tree_review_detects_changed_bytes_and_links() {
        let scratch=Scratch::new();let target=scratch.0.join("demo");std::fs::create_dir(&target).unwrap();std::fs::write(target.join("pom.xml"),b"original").unwrap();
        let mut request=json!({"operation":"inspect-tree","target":target});let inspected=run(&request);assert!(inspected.status.success(),"{}",String::from_utf8_lossy(&inspected.stderr));request["proof"]=serde_json::from_slice(&inspected.stdout).unwrap();request["operation"]=json!("verify-tree");assert!(run(&request).status.success());
        std::fs::write(target.join("pom.xml"),b"changed").unwrap();assert!(!run(&request).status.success());
        #[cfg(unix)] {std::os::unix::fs::symlink("pom.xml",target.join("link")).unwrap();request["operation"]=json!("inspect-tree");assert!(!run(&request).status.success());}
    }
    #[test]
    fn actual_python_adapter_preserves_binary_wrappers_executable_bits_and_never_overwrites() {
        let scratch=Scratch::new();let archive=scratch.0.join("project.zip");
        std::fs::write(&archive,zip(&[("pom.xml",b"<project/>",0o644),("mvnw",b"#!/bin/sh\necho must-not-run",0o755),(".mvn/wrapper/maven-wrapper.jar",&[0,255,128,10],0o644)])).unwrap();
        let mut request=json!({"operation":"inspect","archive":archive,"parent":scratch.0,"artifact":"demo"});
        let inspected=run(&request);assert!(inspected.status.success(),"{}",String::from_utf8_lossy(&inspected.stderr));let proof:Value=serde_json::from_slice(&inspected.stdout).unwrap();assert_eq!(proof["files"].as_array().unwrap().len(),3);
        request["operation"]=json!("extract");request["proof"]=proof;
        let extracted=run(&request);assert!(extracted.status.success(),"{}",String::from_utf8_lossy(&extracted.stderr));
        assert_eq!(std::fs::read(scratch.0.join("demo/.mvn/wrapper/maven-wrapper.jar")).unwrap(),[0,255,128,10]);
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(scratch.0.join("demo/mvnw")).unwrap().permissions().mode()&0o777,0o755);}
        let again=run(&request);assert!(!again.status.success());assert_eq!(std::fs::read(scratch.0.join("demo/pom.xml")).unwrap(),b"<project/>");
    }
    #[test]
    #[cfg(unix)]
    fn actual_adapter_preserves_safe_modes_and_timestamps_and_refuses_unsafe_ones() {
        use std::os::unix::fs::PermissionsExt;
        let scratch=Scratch::new();let archive=scratch.0.join("project.zip");
        let mut writer=zip::ZipWriter::new(Cursor::new(Vec::new()));
        let old=zip::DateTime::from_date_and_time(2001,2,3,4,6,8).unwrap();
        for (name,mode) in [("pom.xml",0o664u32),("gradlew",0o775),("run.sh",0o700),("ro.txt",0o444)] {
            writer.start_file(name,zip::write::SimpleFileOptions::default().unix_permissions(mode).last_modified_time(old)).unwrap();writer.write_all(b"x").unwrap();
        }
        std::fs::write(&archive,writer.finish().unwrap().into_inner()).unwrap();
        let mut request=json!({"operation":"inspect","archive":archive,"parent":scratch.0,"artifact":"demo"});
        let inspected=run(&request);assert!(inspected.status.success(),"{}",String::from_utf8_lossy(&inspected.stderr));
        request["proof"]=serde_json::from_slice(&inspected.stdout).unwrap();request["operation"]=json!("extract");
        let extracted=run(&request);assert!(extracted.status.success(),"{}",String::from_utf8_lossy(&extracted.stderr));
        let mode=|n:&str|std::fs::metadata(scratch.0.join("demo").join(n)).unwrap().permissions().mode()&0o7777;
        assert_eq!((mode("pom.xml"),mode("gradlew"),mode("run.sh"),mode("ro.txt")),(0o644,0o755,0o700,0o644));
        let expected=request["proof"]["files"][0]["mtime"].as_i64().unwrap();
        let modified=std::fs::metadata(scratch.0.join("demo/pom.xml")).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert_eq!(modified,expected);assert!((expected-981173168).abs()<=86400);
        for (name,mode) in [("setuid",0o4755u32),("sticky",0o1755),("setgid",0o2755)] {
            let mut writer=zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer.start_file(name,zip::write::SimpleFileOptions::default().unix_permissions(mode)).unwrap();writer.write_all(b"x").unwrap();
            let mut bytes=writer.finish().unwrap().into_inner();
            let central=bytes.windows(4).position(|w|w==b"PK\x01\x02").unwrap();
            bytes[central+38..central+42].copy_from_slice(&((0o100000|mode)<<16).to_le_bytes());
            std::fs::write(&archive,bytes).unwrap();
            let refused=json!({"operation":"inspect","archive":archive,"parent":scratch.0,"artifact":"other"});
            assert!(!run(&refused).status.success());assert!(!scratch.0.join("other").exists());
        }
    }
    #[test]
    fn actual_adapter_rejects_traversal_case_collisions_and_changed_archive_after_review() {
        let scratch=Scratch::new();let archive=scratch.0.join("project.zip");let mut request=json!({"operation":"inspect","archive":archive,"parent":scratch.0,"artifact":"demo"});
        for entries in [vec![("../outside",&b"bad"[..],0o644)],vec![("Foo/file",&b"one"[..],0o644),("foo/other",&b"two"[..],0o644)],vec![(".git/config",&b"bad"[..],0o644)]] {
            std::fs::write(&archive,zip(&entries)).unwrap();assert!(!run(&request).status.success());assert!(!scratch.0.join("demo").exists());
        }
        std::fs::write(&archive,zip(&[("pom.xml",b"original",0o644)])).unwrap();let output=run(&request);assert!(output.status.success());request["proof"]=serde_json::from_slice(&output.stdout).unwrap();request["operation"]=json!("extract");
        std::fs::write(&archive,zip(&[("pom.xml",b"changed",0o644)])).unwrap();assert!(!run(&request).status.success());assert!(!scratch.0.join("demo").exists());
    }
}
