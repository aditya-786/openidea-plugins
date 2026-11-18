//! Correlate mutable rich controls with the exact form/preview that emitted them.
use crate::*;
fn selected_button(key:&str)->bool {matches!(key,"describe"|"yaml"|"logs"|"log-view"|"shell"|"attach"|"show-secret"|"edit-secret"|"debug"|"port-forward"|"delete")}
fn snapshot_button(key:&str)->bool {matches!(key,"log-filter"|"log-since"|"log-until"|"log-regex"|"log-case"|"log-wrap"|"log-time"|"log-source"|"log-end"|"log-clear"|"log-refresh"|"log-copy"|"log-download")}
fn forward_button(key:&str)->bool {matches!(key,"start-forward"|"start-managed-forward"|"forward-status"|"stop-forward"|"restart-forward")}
fn protected_button(key:&str)->bool {matches!(key,"back"|"create-secret")||selected_button(key)||snapshot_button(key)||forward_button(key)||matches!(key,"submit-secret"|"add-secret-key"|"submit-secret-edit"|"confirm-delete-manifest"|"apply"|"save-file"|"reload-yaml")}
impl KubeExplorer {
    pub(crate) fn new_rich_callback(&mut self)->String {
        self.rich_fields.get_mut().clear();
        self.details_nonce=self.details_nonce.checked_add(1).expect("rich callback nonce exhausted");
        let callback=self.details_proof(self.details_nonce);self.rich_callback=Some(callback.clone());callback
    }
    fn rich_key(&self,key:&str)->String {self.rich_callback.as_ref().map_or_else(||key.into(),|proof|format!("rich:{proof}:{key}"))}
    fn issue_rich_field(&self,original:&str)->String {
        let Some(proof)=&self.rich_callback else{return original.into()};
        let mut fields=self.rich_fields.borrow_mut();
        if let Some(issued)=fields.get(original){return issued.clone();}
        let issued=format!("rich-field:{proof}:{}",fields.len());
        fields.insert(original.into(),issued.clone());issued
    }
    pub(crate) fn protect_rich_view(&self,mut view:View)->View {
        if let Body::Form(fields)=&mut view.body {for field in fields {if field.key.starts_with("secret-")||field.key=="ports"{field.key=self.issue_rich_field(&field.key);}}}
        for button in &mut view.toolbar {if protected_button(&button.id){button.id=self.rich_key(&button.id);}}
        view
    }
    fn rich_control_available(&self,key:&str,field:bool)->bool {
        if field {return match key {
            "ports"=>self.forward.is_some()&&!self.show_forward_session,
            "secret-name"|"secret-key"|"secret-value"=>self.draft.is_some(),
            "secret-new-key"|"secret-new-value"|"secret-remove"=>self.editing_secret.is_some(),
            key if key.starts_with("secret-edit:")=>self.editing_secret.is_some(),
            _=>false,
        };}
        if key=="create-secret" {return !self.all_namespaces();}
        if key=="back" {return self.has_rich_cache()||self.forward.is_some()||self.forward_session.is_some()||self.log_view.is_some()||self.show_forward_session;}
        if forward_button(key){return match key {
            "start-forward"|"start-managed-forward"=>self.forward.is_some()&&!self.show_forward_session,
            _=>self.forward_session.is_some(),
        };}
        if selected_button(key){return self.selected.is_some();}
        if snapshot_button(key){return self.log_view.is_some();}
        match key {
            "submit-secret"|"add-secret-key"=>self.draft.is_some(),
            "submit-secret-edit"=>self.editing_secret.is_some(),
            "confirm-delete-manifest"=>self.pending_delete.is_some(),
            "apply"|"save-file"|"reload-yaml"=>self.details.as_ref().is_some_and(|details|details.editing.is_some()),
            _=>false,
        }
    }
    fn checked_rich_key(&self,key:String,field:bool)->Result<String,Error> {
        if let Some(payload)=key.strip_prefix("rich-field:") {
            if !field{return Err(Error::Stale);}
            let (proof,_)=payload.split_once(':').ok_or(Error::Stale)?;
            if self.rich_callback.as_deref()!=Some(proof){return Err(Error::Stale);}
            let original=self.rich_fields.borrow().iter().find(|(_,issued)|*issued==&key).map(|(original,_)|original.clone()).ok_or(Error::Stale)?;
            if !self.rich_control_available(&original,true){return Err(Error::Stale);}
            return Ok(original);
        }
        if let Some(payload)=key.strip_prefix("rich:") {
            if field{return Err(Error::Stale);}
            let (proof,original)=payload.split_once(':').ok_or(Error::Stale)?;
            if self.rich_callback.as_deref()!=Some(proof)||!self.rich_control_available(original,field){return Err(Error::Stale);}
            return Ok(original.into());
        }
        if (field&&(key.starts_with("secret-")||key=="ports"))||(!field&&protected_button(&key)){return Err(Error::Stale);}
        Ok(key)
    }
    pub(crate) fn checked_rich_event(&self,event:ViewEvent)->Result<ViewEvent,Error> {
        match event {
            ViewEvent::Field(key,value)=>Ok(ViewEvent::Field(self.checked_rich_key(key,true)?,value)),
            ViewEvent::Button(key)=>Ok(ViewEvent::Button(self.checked_rich_key(key,false)?)),
            other=>Ok(other),
        }
    }
    #[cfg(test)]
    pub(crate) fn test_control_key(&self,key:&str)->String {if key.starts_with("secret-")||key=="ports" {self.issue_rich_field(key)}else{self.rich_key(key)}}
}
