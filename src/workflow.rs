use crate::{content::ContentHit, Engine, ExploreHit};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct ResearchReport {
    pub query: String,
    pub graph_epoch: u64,
    pub symbol_findings: Vec<ExploreHit>,
    pub content_findings: Vec<ContentHit>,
    pub files: Vec<String>,
}

pub struct WorkflowService<'a> {
    engine: &'a Engine,
}

impl<'a> WorkflowService<'a> {
    pub fn new(engine: &'a Engine) -> Self {
        Self { engine }
    }

    pub fn research(&self, query: &str, max_files: usize) -> Result<ResearchReport> {
        let symbol_findings = self.engine.explore(query, max_files)?;
        let content_candidates = self
            .engine
            .content_search(query, max_files.saturating_mul(2))?;
        let mut files = symbol_findings
            .iter()
            .map(|finding| finding.symbol.file.clone())
            .collect::<HashSet<_>>();
        let mut content_findings = Vec::new();
        for finding in content_candidates {
            if files.len() >= max_files && !files.contains(&finding.path) {
                continue;
            }
            files.insert(finding.path.clone());
            content_findings.push(finding);
        }
        let mut files = files.into_iter().collect::<Vec<_>>();
        files.sort();
        Ok(ResearchReport {
            query: query.to_owned(),
            graph_epoch: self.engine.committed_epoch()?,
            symbol_findings,
            content_findings,
            files,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn research_combines_graph_symbols_and_repository_content() {
        let project = tempfile::tempdir().unwrap();
        fs::write(
            project.path().join("main.rs"),
            "fn publish_atomically() {}\n",
        )
        .unwrap();
        fs::write(
            project.path().join("README.md"),
            "# Publication guarantees\n\nAtomic publication survives interruption.\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(project.path()).unwrap();

        let report = WorkflowService::new(&engine)
            .research("atomic publication", 10)
            .unwrap();

        assert_eq!(report.graph_epoch, 1);
        assert!(report
            .symbol_findings
            .iter()
            .any(|finding| finding.symbol.name == "publish_atomically"));
        assert!(report
            .content_findings
            .iter()
            .any(|finding| finding.path == "README.md"));
        assert!(report.files.contains(&"main.rs".to_owned()));
        assert!(report.files.contains(&"README.md".to_owned()));
    }
}
