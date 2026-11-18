//! The one index of the project's Java, Kotlin and XML sources: each file is
//! read and parsed once into a record holding the facts of every module.
use super::*;
use spring_text::index::{Index, Indexes, View as IndexView};
use std::ops::{Deref, DerefMut};

#[derive(Clone)]
pub(crate) struct Record {
    pub refs: Vec<Ref>,
    pub names: Vec<batch::Name>,
    pub unit: Option<modulith::Unit>,
}

pub(crate) type Refs<'a> = IndexView<'a, Vec<Ref>>;

pub(crate) fn refs_of(record: &Record) -> Option<&Vec<Ref>> {
    Some(&record.refs).filter(|refs| !refs.is_empty())
}

fn names_of(record: &Record) -> Option<&Vec<batch::Name>> {
    Some(&record.names).filter(|names| !names.is_empty())
}

fn unit_of(record: &Record) -> Option<&modulith::Unit> {
    record.unit.as_ref()
}

pub(crate) fn record(path: &str, text: &str) -> Option<Record> {
    let refs = index_file(path, text).unwrap_or_default();
    let names = batch::index_file(path, text).unwrap_or_default();
    let unit = if spring_text::SOURCE_GLOBS.iter().any(|glob| spring_text::index::glob_matches(glob, path)) {
        modulith::scan(path, text)
    } else {
        None
    };
    Some(Record { refs, names, unit }).filter(|record| !record.refs.is_empty() || !record.names.is_empty() || record.unit.is_some())
}

pub(crate) struct Shared(Indexes<Record>);

impl Default for Shared {
    fn default() -> Self {
        Self(Indexes::new(&beans::GLOBS, record).excluding_nested_projects())
    }
}

impl Deref for Shared {
    type Target = Indexes<Record>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Shared {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Shared {
    /// Starts over, unless a scan is in progress: that would throw away what it has read.
    pub(crate) fn restart_project(&mut self, project: &Project) {
        let index = self.0.get(project, std::time::Duration::ZERO);
        if index.complete() && !index.scanning() {
            self.0.reset();
        }
    }

    fn for_document_of(&mut self, doc: &Document) -> Option<&mut Index<Record>> {
        spring_text::index::owning_root(&doc.path)?;
        self.0.for_document(&doc.path, &doc.text, PROVIDER_BUDGET)
    }

    fn cached_of(&mut self, doc: &Document) -> Option<&mut Index<Record>> {
        spring_text::index::owning_root(&doc.path)?;
        self.0.for_cached_document(&doc.path, &doc.text)
    }
}

impl batch::Source for Shared {
    fn for_document(&mut self, doc: &Document) -> Option<batch::Names<'_>> {
        Some(self.for_document_of(doc)?.view(names_of))
    }

    fn cached(&mut self, doc: &Document) -> Option<batch::Names<'_>> {
        Some(self.cached_of(doc)?.view(names_of))
    }

    fn project(&mut self, project: &Project, budget: std::time::Duration) -> batch::Names<'_> {
        self.0.get(project, budget).view(names_of)
    }

    fn restart(&mut self, project: &Project) {
        self.restart_project(project);
    }
}

impl modulith::Source for Shared {
    fn for_document(&mut self, doc: &Document) -> Option<modulith::Units<'_>> {
        Some(self.for_document_of(doc)?.view(unit_of))
    }

    fn cached(&mut self, doc: &Document) -> Option<modulith::Units<'_>> {
        Some(self.cached_of(doc)?.view(unit_of))
    }

    fn project(&mut self, project: &Project, budget: std::time::Duration) -> modulith::Units<'_> {
        self.0.get(project, budget).view(unit_of)
    }

    fn restart(&mut self, project: &Project) {
        self.restart_project(project);
    }
}
