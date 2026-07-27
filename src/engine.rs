use crate::{
    model::{Evidence, FileFacts, RelationshipKind},
    parser::parse_file,
    store::{FileSummary, SearchHit, Store},
};
use anyhow::{bail, Context, Result};
use ignore::WalkBuilder;
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

pub const PROJECT_DIR: &str = ".structurely";
pub const DATABASE_FILE: &str = "graph.db";

pub struct Engine {
    root: PathBuf,
    store: Store,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexReport {
    pub epoch: u64,
    pub files_scanned: usize,
    pub files_changed: usize,
    pub files_deleted: usize,
    pub symbols_changed: usize,
    pub relationships_resolved: usize,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatus {
    pub root: String,
    pub database: String,
    pub epoch: u64,
    pub indexed_files: usize,
    pub pending_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelatedHit {
    pub origin: crate::model::Symbol,
    pub symbol: crate::model::Symbol,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactHit {
    pub depth: usize,
    pub origin: crate::model::Symbol,
    pub symbol: crate::model::Symbol,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExploreHit {
    pub symbol: crate::model::Symbol,
    pub source: String,
    pub callers: Vec<(crate::model::Symbol, Evidence)>,
    pub callees: Vec<(crate::model::Symbol, Evidence)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeFile {
    pub file: String,
    pub source: Option<String>,
    pub symbols: Vec<crate::model::Symbol>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeResult {
    pub files: Vec<NodeFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BenchmarkReport {
    pub project: String,
    pub graph_model_version: u32,
    pub initial_sync: IndexReport,
    pub iterations: usize,
    pub no_change_sync_p50_us: u128,
    pub no_change_sync_p95_us: u128,
    pub query: String,
    pub query_p50_us: u128,
    pub query_p95_us: u128,
    pub database_bytes: u64,
    pub indexed_files: usize,
    pub symbols: usize,
    pub relationships: usize,
}

impl Engine {
    pub fn init(root: impl AsRef<Path>) -> Result<(Self, IndexReport)> {
        let root = absolute(root.as_ref())?;
        fs::create_dir_all(root.join(PROJECT_DIR))?;
        let mut engine = Self::open(&root)?;
        let report = engine.sync()?;
        Ok((engine, report))
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = absolute(root.as_ref())?;
        let database = root.join(PROJECT_DIR).join(DATABASE_FILE);
        if !root.join(PROJECT_DIR).exists() {
            bail!(
                "{} is not initialized; run `structurely init {}`",
                root.display(),
                root.display()
            );
        }
        Ok(Self {
            root,
            store: Store::open(&database)?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn sync(&mut self) -> Result<IndexReport> {
        let started = Instant::now();
        let force_reindex = !self.store.is_current_graph_model()?;
        let mut seen = HashSet::new();
        let mut changed = Vec::<FileFacts>::new();
        let mut files_scanned = 0;

        for entry in WalkBuilder::new(&self.root)
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .filter_entry(|entry| entry.file_name() != PROJECT_DIR)
            .build()
        {
            let entry = entry?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let relative = entry.path().strip_prefix(&self.root)?;
            if crate::model::Language::from_path(relative).is_none() {
                continue;
            }
            let relative = normalize_path(relative);
            seen.insert(relative.clone());
            files_scanned += 1;
            let source = fs::read_to_string(entry.path())
                .with_context(|| format!("read source {}", entry.path().display()))?;
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            if !force_reindex
                && self.store.content_hash(&relative)?.as_deref() == Some(hash.as_str())
            {
                continue;
            }
            changed.push(parse_file(&relative, &source)?);
        }

        let indexed: HashSet<_> = self.store.indexed_files()?.into_iter().collect();
        let deleted: Vec<_> = indexed.difference(&seen).cloned().collect();
        let symbols_changed = changed.iter().map(|facts| facts.symbols.len()).sum();
        let (epoch, relationships_resolved) = if changed.is_empty() && deleted.is_empty() {
            (self.store.epoch()?, 0)
        } else {
            self.store.publish(&changed, &deleted)?
        };

        Ok(IndexReport {
            epoch,
            files_scanned,
            files_changed: changed.len(),
            files_deleted: deleted.len(),
            symbols_changed,
            relationships_resolved,
            duration_ms: started.elapsed().as_millis(),
        })
    }

    pub fn status(&self) -> Result<ProjectStatus> {
        let indexed = self.store.indexed_files()?;
        let indexed_set: HashSet<_> = indexed.iter().cloned().collect();
        let mut current = HashSet::new();
        let mut changed_or_added = 0;
        for entry in WalkBuilder::new(&self.root)
            .filter_entry(|entry| entry.file_name() != PROJECT_DIR)
            .build()
            .flatten()
        {
            if entry.file_type().is_some_and(|kind| kind.is_file()) {
                if let Ok(relative) = entry.path().strip_prefix(&self.root) {
                    if crate::model::Language::from_path(relative).is_some() {
                        let relative = normalize_path(relative);
                        current.insert(relative.clone());
                        let changed = fs::read(entry.path())
                            .ok()
                            .map(|source| blake3::hash(&source).to_hex().to_string())
                            .map(|hash| {
                                self.store.content_hash(&relative).ok().flatten().as_deref()
                                    != Some(hash.as_str())
                            })
                            .unwrap_or(true);
                        changed_or_added += usize::from(changed);
                    }
                }
            }
        }
        let deleted = indexed_set.difference(&current).count();
        let pending = changed_or_added + deleted;
        Ok(ProjectStatus {
            root: self.root.display().to_string(),
            database: self.store.path().display().to_string(),
            epoch: self.store.epoch()?,
            indexed_files: indexed.len(),
            pending_files: pending,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.store.search(query, limit)
    }

    pub fn search_filtered(
        &self,
        query: &str,
        kind: Option<crate::model::SymbolKind>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.store.search_filtered(query, kind, limit)
    }

    pub fn files(&self) -> Result<Vec<FileSummary>> {
        self.store.file_summaries()
    }

    pub fn snapshot(&self) -> Result<crate::store::GraphSnapshot> {
        self.store.snapshot()
    }

    pub fn benchmark(
        &mut self,
        query: &str,
        iterations: usize,
        initial_sync: IndexReport,
    ) -> Result<BenchmarkReport> {
        let iterations = iterations.clamp(1, 1_000);
        let mut sync_times = Vec::with_capacity(iterations);
        let mut query_times = Vec::with_capacity(iterations);
        for _ in 0..iterations {
            let started = Instant::now();
            let report = self.sync()?;
            if report.files_changed != 0 || report.files_deleted != 0 {
                bail!("benchmark source changed during measurement");
            }
            sync_times.push(started.elapsed().as_micros());

            let started = Instant::now();
            let _ = self.search(query, 20)?;
            query_times.push(started.elapsed().as_micros());
        }
        sync_times.sort_unstable();
        query_times.sort_unstable();
        let snapshot = self.snapshot()?;
        Ok(BenchmarkReport {
            project: self.root.display().to_string(),
            graph_model_version: crate::model::GRAPH_MODEL_VERSION,
            initial_sync,
            iterations,
            no_change_sync_p50_us: percentile(&sync_times, 50),
            no_change_sync_p95_us: percentile(&sync_times, 95),
            query: query.to_owned(),
            query_p50_us: percentile(&query_times, 50),
            query_p95_us: percentile(&query_times, 95),
            database_bytes: fs::metadata(self.store.path())?.len(),
            indexed_files: snapshot.files.len(),
            symbols: snapshot.symbols.len(),
            relationships: snapshot.relationships.len(),
        })
    }

    pub fn node(
        &self,
        symbol: Option<&str>,
        file: Option<&str>,
        include_code: bool,
        offset: Option<usize>,
        limit: Option<usize>,
        symbols_only: bool,
    ) -> Result<NodeResult> {
        let all_files = self.store.file_summaries()?;
        let matched_files: Vec<_> = if let Some(file) = file {
            all_files
                .into_iter()
                .filter(|candidate| {
                    candidate.path == file
                        || candidate.path.ends_with(file)
                        || Path::new(&candidate.path)
                            .file_name()
                            .is_some_and(|name| name == file)
                })
                .map(|summary| summary.path)
                .collect()
        } else if let Some(symbol) = symbol {
            self.store
                .find_symbols(symbol)?
                .into_iter()
                .map(|symbol| symbol.file)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };

        let mut files = Vec::new();
        for path in matched_files {
            let mut symbols = self.store.symbols_in_file(&path)?;
            if let Some(identifier) = symbol {
                symbols.retain(|candidate| {
                    candidate.id == identifier
                        || candidate.name == identifier
                        || candidate.qualified_name == identifier
                });
            }
            let wants_source = !symbols_only && (symbol.is_none() || include_code);
            let source = if wants_source {
                let raw = fs::read_to_string(self.root.join(&path))
                    .with_context(|| format!("read source {}", path))?;
                if symbol.is_some() {
                    let snippets = symbols
                        .iter()
                        .filter_map(|symbol| raw.get(symbol.start_byte..symbol.end_byte))
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    Some(snippets)
                } else {
                    let start = offset.unwrap_or(1).saturating_sub(1);
                    let maximum = limit.unwrap_or(2_000).min(2_000);
                    Some(
                        raw.lines()
                            .enumerate()
                            .skip(start)
                            .take(maximum)
                            .map(|(index, line)| format!("{}\t{}", index + 1, line))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )
                }
            } else {
                None
            };
            files.push(NodeFile {
                file: path,
                source,
                symbols,
            });
        }
        files.sort_by(|left, right| left.file.cmp(&right.file));
        Ok(NodeResult { files })
    }

    pub fn explore(&self, query: &str, max_files: usize) -> Result<Vec<ExploreHit>> {
        let mut files = HashSet::new();
        let mut output = Vec::new();
        for hit in self
            .store
            .search(query, max_files.saturating_mul(4).max(1))?
        {
            if !files.contains(&hit.symbol.file) && files.len() >= max_files {
                continue;
            }
            files.insert(hit.symbol.file.clone());
            let path = self.root.join(&hit.symbol.file);
            let source = fs::read_to_string(&path)
                .with_context(|| format!("read source {}", path.display()))?;
            let snippet = source
                .get(hit.symbol.start_byte..hit.symbol.end_byte)
                .unwrap_or_default()
                .to_owned();
            output.push(ExploreHit {
                callers: self.callers(&hit.symbol.id)?,
                callees: self.callees(&hit.symbol.id)?,
                symbol: hit.symbol,
                source: snippet,
            });
        }
        Ok(output)
    }

    pub fn callers(&self, symbol_id: &str) -> Result<Vec<(crate::model::Symbol, Evidence)>> {
        self.store.related(symbol_id, true, RelationshipKind::Calls)
    }

    pub fn callees(&self, symbol_id: &str) -> Result<Vec<(crate::model::Symbol, Evidence)>> {
        self.store
            .related(symbol_id, false, RelationshipKind::Calls)
    }

    pub fn callers_named(
        &self,
        symbol: &str,
        file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RelatedHit>> {
        self.related_named(symbol, file, limit, true)
    }

    pub fn callees_named(
        &self,
        symbol: &str,
        file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RelatedHit>> {
        self.related_named(symbol, file, limit, false)
    }

    pub fn impact_named(
        &self,
        symbol: &str,
        file: Option<&str>,
        max_depth: usize,
    ) -> Result<Vec<ImpactHit>> {
        let roots: Vec<_> = self
            .store
            .find_symbols(symbol)?
            .into_iter()
            .filter(|origin| {
                file.is_none_or(|suffix| {
                    origin.file == suffix
                        || origin.file.ends_with(suffix)
                        || Path::new(&origin.file)
                            .file_name()
                            .is_some_and(|name| name == suffix)
                })
            })
            .collect();
        let mut queue = roots
            .iter()
            .cloned()
            .map(|symbol| (symbol, 0usize))
            .collect::<std::collections::VecDeque<_>>();
        let mut visited: HashSet<String> = roots.iter().map(|symbol| symbol.id.clone()).collect();
        let mut output = Vec::new();
        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for (caller, evidence) in
                self.store
                    .related(&current.id, true, RelationshipKind::Calls)?
            {
                if visited.insert(caller.id.clone()) {
                    output.push(ImpactHit {
                        depth: depth + 1,
                        origin: current.clone(),
                        symbol: caller.clone(),
                        evidence,
                    });
                    queue.push_back((caller, depth + 1));
                }
            }
        }
        Ok(output)
    }

    fn related_named(
        &self,
        identifier: &str,
        file: Option<&str>,
        limit: usize,
        incoming: bool,
    ) -> Result<Vec<RelatedHit>> {
        let origins = self.store.find_symbols(identifier)?;
        let mut output = Vec::new();
        for origin in origins.into_iter().filter(|origin| {
            file.is_none_or(|suffix| {
                origin.file == suffix
                    || origin.file.ends_with(suffix)
                    || Path::new(&origin.file)
                        .file_name()
                        .is_some_and(|name| name == suffix)
            })
        }) {
            let related = self
                .store
                .related(&origin.id, incoming, RelationshipKind::Calls)?;
            for (symbol, evidence) in related {
                output.push(RelatedHit {
                    origin: origin.clone(),
                    symbol,
                    evidence,
                });
                if output.len() >= limit {
                    return Ok(output);
                }
            }
        }
        Ok(output)
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn normalize_path(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    let index = ((sorted.len() - 1) * percentile).div_ceil(100);
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_and_incremental_index_converge() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.ts"), "export function a() { b(); }\n").unwrap();
        fs::write(temp.path().join("b.ts"), "export function b() {}\n").unwrap();
        let (mut engine, first) = Engine::init(temp.path()).unwrap();
        assert_eq!(first.files_changed, 2);
        let second = engine.sync().unwrap();
        assert_eq!(second.files_changed, 0);
        let result = engine.search("a", 10).unwrap();
        let a = result.iter().find(|hit| hit.symbol.name == "a").unwrap();
        let callees = engine.callees(&a.symbol.id).unwrap();
        assert_eq!(callees[0].0.name, "b");
        assert_eq!(callees[0].1.confidence, 0.95);
    }

    #[test]
    fn changing_callee_preserves_edges_from_unchanged_callers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("caller.ts"),
            "export function caller() { callee(); }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("callee.ts"),
            "export function callee() {}\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let caller = engine
            .search("caller", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "caller")
            .unwrap()
            .symbol;
        assert_eq!(engine.callees(&caller.id).unwrap().len(), 1);

        fs::write(
            temp.path().join("callee.ts"),
            "// moved by a harmless edit\nexport function callee() {}\n",
        )
        .unwrap();
        engine.sync().unwrap();

        let callees = engine.callees(&caller.id).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].0.name, "callee");
    }

    #[test]
    fn status_reports_modified_files_as_pending() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.py"), "def main():\n    pass\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        assert_eq!(engine.status().unwrap().pending_files, 0);

        fs::write(
            temp.path().join("main.py"),
            "def main():\n    print('changed')\n",
        )
        .unwrap();
        assert_eq!(engine.status().unwrap().pending_files, 1);
    }

    #[test]
    fn ordinary_name_resolution_does_not_cross_languages() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.py"),
            "def helper():\n    pass\n\ndef caller():\n    helper()\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("lib.rs"),
            "fn helper() {}\nfn caller() { helper(); }\n",
        )
        .unwrap();
        let (engine, report) = Engine::init(temp.path()).unwrap();
        assert_eq!(report.relationships_resolved, 2);
        let callers: Vec<_> = engine
            .search("caller", 10)
            .unwrap()
            .into_iter()
            .filter(|hit| hit.symbol.name == "caller")
            .collect();
        assert_eq!(callers.len(), 2);
        for caller in callers {
            let callees = engine.callees(&caller.symbol.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].0.language, caller.symbol.language);
            assert_eq!(callees[0].1.confidence, 0.95);
        }
    }

    #[test]
    fn files_and_node_expose_indexed_source_safely() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("src/main.ts"),
            "export function main() {\n  return 42;\n}\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let files = engine.files().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.ts");

        let file = engine
            .node(None, Some("main.ts"), false, Some(2), Some(1), false)
            .unwrap();
        assert_eq!(file.files[0].source.as_deref(), Some("2\t  return 42;"));

        let symbol = engine
            .node(Some("main"), None, true, None, None, false)
            .unwrap();
        assert!(symbol.files[0]
            .source
            .as_deref()
            .unwrap()
            .contains("function main"));
    }

    #[test]
    fn impact_is_transitive_and_search_filters_by_kind() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("flow.ts"),
            "class Flow {}\nfunction leaf() {}\nfunction middle() { leaf(); }\nfunction root() { middle(); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let classes = engine
            .search_filtered("Flow", Some(crate::model::SymbolKind::Class), 10)
            .unwrap();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].symbol.kind, crate::model::SymbolKind::Class);
        let functions = engine
            .search_filtered("leaf", Some(crate::model::SymbolKind::Function), 10)
            .unwrap();
        assert_eq!(functions.len(), 1);
        assert!(engine
            .search_filtered("leaf", Some(crate::model::SymbolKind::Class), 10)
            .unwrap()
            .is_empty());

        let impact = engine.impact_named("leaf", None, 2).unwrap();
        assert_eq!(impact.len(), 2);
        assert_eq!(impact[0].symbol.name, "middle");
        assert_eq!(impact[0].depth, 1);
        assert_eq!(impact[1].symbol.name, "root");
        assert_eq!(impact[1].depth, 2);
    }

    #[test]
    fn graph_model_upgrade_forces_semantic_reindex() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.rs"), "fn main() {}\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        drop(engine);
        let database = temp.path().join(PROJECT_DIR).join(DATABASE_FILE);
        let connection = rusqlite::Connection::open(database).unwrap();
        connection
            .execute(
                "UPDATE metadata SET value='1' WHERE key='graph_model_version'",
                [],
            )
            .unwrap();
        drop(connection);

        let mut engine = Engine::open(temp.path()).unwrap();
        let report = engine.sync().unwrap();
        assert_eq!(report.files_changed, 1);
    }

    #[test]
    fn incremental_and_clean_snapshots_are_identical() {
        let incremental_dir = tempfile::tempdir().unwrap();
        let clean_dir = tempfile::tempdir().unwrap();
        let initial = "function caller() { oldName(); }\nfunction oldName() {}\n";
        let final_source = "function caller() { newName(); }\nfunction newName() {}\n";
        fs::write(incremental_dir.path().join("main.ts"), initial).unwrap();
        let (mut incremental, _) = Engine::init(incremental_dir.path()).unwrap();
        fs::write(incremental_dir.path().join("main.ts"), final_source).unwrap();
        incremental.sync().unwrap();

        fs::write(clean_dir.path().join("main.ts"), final_source).unwrap();
        let (clean, _) = Engine::init(clean_dir.path()).unwrap();

        let incremental_json = serde_json::to_string(&incremental.snapshot().unwrap()).unwrap();
        let clean_json = serde_json::to_string(&clean.snapshot().unwrap()).unwrap();
        assert_eq!(incremental_json, clean_json);
    }

    #[test]
    fn benchmark_reports_measured_graph_cardinality() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.py"), "def main():\n    return 1\n").unwrap();
        let (mut engine, initial) = Engine::init(temp.path()).unwrap();
        let report = engine.benchmark("main", 3, initial).unwrap();
        assert_eq!(report.iterations, 3);
        assert_eq!(report.indexed_files, 1);
        assert!(report.symbols >= 2);
        assert!(report.database_bytes > 0);
    }
}
