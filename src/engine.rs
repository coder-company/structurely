use crate::{
    inventory::ProjectInventory,
    model::{Evidence, RelationshipKind},
    parser::parse_file_as,
    project_resolution::ProjectResolutionContext,
    store::{FileSummary, SearchHit, StorageMetrics, Store},
};
use anyhow::{anyhow, bail, Context, Result};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
    time::Instant,
};

pub const PROJECT_DIR: &str = ".structurely";
pub const DATABASE_FILE: &str = "graph.db";
pub const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

pub struct Engine {
    root: PathBuf,
    store: Store,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexReport {
    pub epoch: u64,
    pub files_scanned: usize,
    pub files_skipped: usize,
    pub files_changed: usize,
    pub files_deleted: usize,
    pub symbols_changed: usize,
    pub relationships_resolved: usize,
    pub parse_workers: usize,
    pub staging_ms: u128,
    pub resolution_ms: u128,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatus {
    pub root: String,
    pub database: String,
    pub epoch: u64,
    pub indexed_files: usize,
    pub pending_files: usize,
    pub skipped_files: usize,
    pub storage: StorageMetrics,
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
    pub source_truncated: bool,
    pub callers: Vec<(crate::model::Symbol, Evidence)>,
    pub callees: Vec<(crate::model::Symbol, Evidence)>,
    pub referenced_by: Vec<(crate::model::Symbol, Evidence)>,
    pub references: Vec<(crate::model::Symbol, Evidence)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeFile {
    pub file: String,
    pub source: Option<String>,
    pub symbols: Vec<crate::model::Symbol>,
    pub total_lines: usize,
    pub shown_start_line: Option<usize>,
    pub shown_end_line: Option<usize>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeResult {
    pub files: Vec<NodeFile>,
    pub ambiguous: bool,
    pub guidance: Option<String>,
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
        let indexed = self.store.indexed_file_hashes()?;
        let delta = ProjectInventory::new(&self.root)?.delta(&indexed, force_reindex)?;
        let changed = delta.changed;
        let deleted = delta.deleted;
        let files_scanned = delta.files_scanned;
        let files_skipped = delta.files_skipped;
        let files_changed = changed.len();
        let resolution_context = Arc::new(ProjectResolutionContext::load(&self.root));
        let available_workers = thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(1);
        let parse_workers = parse_worker_count(
            std::env::var("STRUCTURELY_PARSE_WORKERS").ok().as_deref(),
            available_workers,
            files_changed,
        );
        let (epoch, relationships_resolved, symbols_changed, staging_ms, resolution_ms) =
            if changed.is_empty() && deleted.is_empty() {
                (self.store.epoch()?, 0, 0, 0, 0)
            } else if parse_workers == 1 {
                let facts = changed.iter().map(|(relative, path, language)| {
                    let source = fs::read_to_string(path)
                        .with_context(|| format!("read source {}", path.display()))?;
                    let mut facts = parse_file_as(relative, &source, *language)?;
                    resolution_context.apply(&mut facts);
                    Ok(facts)
                });
                self.store.publish(facts, &deleted)?
            } else {
                thread::scope(|scope| {
                    let (work_sender, work_receiver) =
                        mpsc::channel::<(String, PathBuf, crate::model::Language)>();
                    let work_receiver = Arc::new(Mutex::new(work_receiver));
                    let (result_sender, result_receiver) =
                        mpsc::sync_channel::<Result<crate::model::FileFacts>>(
                            parse_workers.saturating_mul(2),
                        );
                    for _ in 0..parse_workers {
                        let work_receiver = Arc::clone(&work_receiver);
                        let result_sender = result_sender.clone();
                        let resolution_context = Arc::clone(&resolution_context);
                        scope.spawn(move || loop {
                            let work = match work_receiver.lock() {
                                Ok(receiver) => receiver.recv(),
                                Err(_) => return,
                            };
                            let Ok((relative, path, language)) = work else {
                                return;
                            };
                            let facts = fs::read_to_string(&path)
                                .with_context(|| format!("read source {}", path.display()))
                                .and_then(|source| {
                                    let mut facts = parse_file_as(&relative, &source, language)?;
                                    resolution_context.apply(&mut facts);
                                    Ok(facts)
                                });
                            if result_sender.send(facts).is_err() {
                                return;
                            }
                        });
                    }
                    drop(result_sender);
                    for work in changed.iter().cloned() {
                        work_sender
                            .send(work)
                            .map_err(|_| anyhow!("parser worker queue closed"))?;
                    }
                    drop(work_sender);
                    let mut received = 0;
                    let facts = std::iter::from_fn(|| {
                        if received >= files_changed {
                            return None;
                        }
                        received += 1;
                        Some(
                            result_receiver
                                .recv()
                                .unwrap_or_else(|_| Err(anyhow!("parser worker stopped"))),
                        )
                    });
                    self.store.publish(facts, &deleted)
                })?
            };

        Ok(IndexReport {
            epoch,
            files_scanned,
            files_skipped,
            files_changed,
            files_deleted: deleted.len(),
            symbols_changed,
            relationships_resolved,
            parse_workers,
            staging_ms,
            resolution_ms,
            duration_ms: started.elapsed().as_millis(),
        })
    }

    pub fn status(&self) -> Result<ProjectStatus> {
        let indexed = self.store.indexed_file_hashes()?;
        let delta = ProjectInventory::new(&self.root)?.delta(&indexed, false)?;
        let pending = delta.changed.len() + delta.deleted.len();
        Ok(ProjectStatus {
            root: self.root.display().to_string(),
            database: self.store.path().display().to_string(),
            epoch: self.store.epoch()?,
            indexed_files: indexed.len(),
            pending_files: pending,
            skipped_files: delta.files_skipped,
            storage: self.store.storage_metrics()?,
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

    pub fn watch(
        &mut self,
        stop: Arc<AtomicBool>,
        debounce: Duration,
        on_sync: impl FnMut(&IndexReport),
    ) -> Result<()> {
        self.watch_ready(stop, debounce, || {}, on_sync)
    }

    pub fn watch_ready(
        &mut self,
        stop: Arc<AtomicBool>,
        debounce: Duration,
        on_ready: impl FnOnce(),
        mut on_sync: impl FnMut(&IndexReport),
    ) -> Result<()> {
        let (sender, receiver) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.send(event);
        })?;
        watcher.watch(&self.root, RecursiveMode::Recursive)?;
        on_ready();
        let poll = Duration::from_millis(50);
        let mut last_relevant_event: Option<Instant> = None;
        let mut last_reconcile = Instant::now();

        while !stop.load(Ordering::Relaxed) {
            match receiver.recv_timeout(poll) {
                Ok(Ok(event)) if self.relevant_watch_event(&event) => {
                    last_relevant_event = Some(Instant::now());
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => return Err(error.into()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("filesystem watcher disconnected")
                }
            }
            if last_relevant_event.is_some_and(|last| last.elapsed() >= debounce) {
                let report = self.sync()?;
                if report.files_changed > 0 || report.files_deleted > 0 {
                    on_sync(&report);
                }
                last_relevant_event = None;
                last_reconcile = Instant::now();
            } else if last_reconcile.elapsed() >= Duration::from_secs(1) {
                let report = self.sync()?;
                if report.files_changed > 0 || report.files_deleted > 0 {
                    on_sync(&report);
                }
                last_reconcile = Instant::now();
            }
        }
        if last_relevant_event.is_some() {
            let report = self.sync()?;
            if report.files_changed > 0 || report.files_deleted > 0 {
                on_sync(&report);
            }
        }
        drop(watcher);
        Ok(())
    }

    fn relevant_watch_event(&self, event: &Event) -> bool {
        if matches!(event.kind, EventKind::Access(_)) {
            return false;
        }
        event.paths.iter().any(|path| {
            path.strip_prefix(&self.root).is_ok_and(|relative| {
                !relative
                    .components()
                    .any(|part| part.as_os_str() == PROJECT_DIR)
                    && (crate::model::Language::from_path(relative).is_some()
                        || path.is_dir()
                        || !path.exists())
            })
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
            let raw = fs::read_to_string(self.root.join(&path))
                .with_context(|| format!("read source {}", path))?;
            let total_lines = raw.lines().count().max(1);
            let (source, shown_start_line, shown_end_line, truncated) = if wants_source {
                if symbol.is_some() {
                    let snippets = symbols
                        .iter()
                        .filter_map(|symbol| {
                            raw.get(symbol.start_byte..symbol.end_byte).map(|source| {
                                source
                                    .lines()
                                    .enumerate()
                                    .map(|(offset, line)| {
                                        format!("{}\t{}", symbol.start_line + offset, line)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let start = symbols.iter().map(|symbol| symbol.start_line).min();
                    let end = symbols.iter().map(|symbol| symbol.end_line).max();
                    let (snippets, truncated) = bounded_source(&snippets, 64_000);
                    (Some(snippets), start, end, truncated)
                } else {
                    let start = offset.unwrap_or(1).saturating_sub(1);
                    let maximum = limit.unwrap_or(2_000).min(2_000);
                    let selected = raw
                        .lines()
                        .enumerate()
                        .skip(start)
                        .take(maximum)
                        .map(|(index, line)| format!("{}\t{}", index + 1, line))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let (selected, character_truncated) = bounded_source(&selected, 64_000);
                    let rendered_lines = selected.lines().count();
                    let shown_start = (rendered_lines > 0).then_some(start + 1);
                    let shown_end = (rendered_lines > 0).then_some(start + rendered_lines);
                    (
                        Some(selected),
                        shown_start,
                        shown_end,
                        character_truncated || start + rendered_lines < total_lines,
                    )
                }
            } else {
                (None, None, None, false)
            };
            files.push(NodeFile {
                file: path,
                source,
                symbols,
                total_lines,
                shown_start_line,
                shown_end_line,
                truncated,
            });
        }
        files.sort_by(|left, right| left.file.cmp(&right.file));
        let ambiguous = files.len() > 1;
        let guidance = if ambiguous {
            Some(
                "Multiple files matched; pass an exact project-relative file path or stable symbol id."
                    .to_owned(),
            )
        } else if files.is_empty() {
            Some("No indexed file or symbol matched the request.".to_owned())
        } else {
            None
        };
        Ok(NodeResult {
            files,
            ambiguous,
            guidance,
        })
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
                .unwrap_or_default();
            let (snippet, source_truncated) = bounded_source(snippet, 4_000);
            output.push(ExploreHit {
                callers: self.callers(&hit.symbol.id)?,
                callees: self.callees(&hit.symbol.id)?,
                referenced_by: self.referenced_by(&hit.symbol.id)?,
                references: self.references(&hit.symbol.id)?,
                symbol: hit.symbol,
                source: snippet,
                source_truncated,
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

    pub fn referenced_by(&self, symbol_id: &str) -> Result<Vec<(crate::model::Symbol, Evidence)>> {
        self.store
            .related(symbol_id, true, RelationshipKind::References)
    }

    pub fn references(&self, symbol_id: &str) -> Result<Vec<(crate::model::Symbol, Evidence)>> {
        self.store
            .related(symbol_id, false, RelationshipKind::References)
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

fn bounded_source(source: &str, maximum_chars: usize) -> (String, bool) {
    let boundary = source
        .char_indices()
        .nth(maximum_chars)
        .map(|(index, _)| index);
    match boundary {
        Some(index) => (source[..index].to_owned(), true),
        None => (source.to_owned(), false),
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

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    let index = ((sorted.len() - 1) * percentile).div_ceil(100);
    sorted[index]
}

fn parse_worker_count(configured: Option<&str>, available: usize, files: usize) -> usize {
    let requested = configured
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(available.min(8));
    requested.min(16).min(files.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_file;

    #[test]
    fn parse_worker_configuration_is_bounded_and_never_zero() {
        assert_eq!(parse_worker_count(None, 8, 100), 8);
        assert_eq!(parse_worker_count(Some("4"), 8, 100), 4);
        assert_eq!(parse_worker_count(Some("1000"), 8, 100), 16);
        assert_eq!(parse_worker_count(Some("0"), 8, 100), 8);
        assert_eq!(parse_worker_count(Some("invalid"), 8, 2), 2);
        assert_eq!(parse_worker_count(Some("8"), 8, 0), 1);
    }

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
        assert_eq!(callees[0].1.confidence, 0.75);
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
    fn project_config_controls_custom_extensions_and_explicit_excludes() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("third_party")).unwrap();
        fs::write(
            temp.path().join("structurely.json"),
            r#"{
                "extensions": { ".view": "typescript" },
                "exclude": ["third_party/**"]
            }"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("page.view"),
            "export function configuredPage() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("third_party/ignored.ts"),
            "export function vendorOnly() {}\n",
        )
        .unwrap();

        let (engine, report) = Engine::init(temp.path()).unwrap();
        assert_eq!(report.files_scanned, 1);
        assert!(engine
            .search("configuredPage", 10)
            .unwrap()
            .iter()
            .any(|hit| hit.symbol.name == "configuredPage"));
        assert!(engine.search("vendorOnly", 10).unwrap().is_empty());
        assert_eq!(engine.status().unwrap().pending_files, 0);
    }

    #[test]
    fn oversized_generated_sources_are_skipped_and_reported() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("generated.c"),
            vec![b' '; MAX_SOURCE_BYTES as usize + 1],
        )
        .unwrap();
        let (engine, report) = Engine::init(temp.path()).unwrap();

        assert_eq!(report.files_scanned, 0);
        assert_eq!(report.files_skipped, 1);
        let status = engine.status().unwrap();
        assert_eq!(status.indexed_files, 0);
        assert_eq!(status.pending_files, 0);
        assert_eq!(status.skipped_files, 1);
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
            assert_eq!(callees[0].1.confidence, 0.99);
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
        assert_eq!(file.files[0].total_lines, 3);
        assert_eq!(file.files[0].shown_start_line, Some(2));
        assert_eq!(file.files[0].shown_end_line, Some(2));
        assert!(file.files[0].truncated);
        assert!(!file.ambiguous);

        let symbol = engine
            .node(Some("main"), None, true, None, None, false)
            .unwrap();
        assert!(symbol.files[0]
            .source
            .as_deref()
            .unwrap()
            .contains("1\tfunction main"));
    }

    #[test]
    fn node_discloses_ambiguity_and_character_truncation() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("first")).unwrap();
        fs::create_dir_all(temp.path().join("second")).unwrap();
        fs::write(
            temp.path().join("first/main.ts"),
            format!("export const huge = \"{}\";\n", "x".repeat(100_000)),
        )
        .unwrap();
        fs::write(
            temp.path().join("second/main.ts"),
            "export const small = 1;\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let result = engine
            .node(None, Some("main.ts"), true, None, None, false)
            .unwrap();
        assert!(result.ambiguous);
        assert!(result.guidance.as_deref().unwrap().contains("exact"));
        let huge = result
            .files
            .iter()
            .find(|file| file.file == "first/main.ts")
            .unwrap();
        assert!(huge.truncated);
        assert!(huge.source.as_deref().unwrap().chars().count() <= 64_000);
        assert_eq!(huge.total_lines, 1);
        assert_eq!(huge.shown_start_line, Some(1));
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
    fn natural_language_search_matches_identifier_segments() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("auth.ts"),
            "class AuthService {\n  loginUser() {}\n}\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let hits = engine
            .search("How does AuthService loginUser work?", 10)
            .unwrap();
        assert!(hits.iter().any(|hit| hit.symbol.name == "AuthService"));
        assert!(hits.iter().any(|hit| hit.symbol.name == "loginUser"));
    }

    #[test]
    fn exact_symbol_name_ranks_before_partial_matches() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("auth.ts"),
            "class AuthServiceFactory {}\nclass AuthService {}\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let hits = engine.search("AuthService", 10).unwrap();
        assert_eq!(hits[0].symbol.name, "AuthService");
        assert!(hits[0].score > hits[1].score);
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
    fn repeated_epochs_checkpoint_and_report_bounded_wal_storage() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("main.rs");
        fs::write(&source, "fn revision_0() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        for revision in 1..=300 {
            fs::write(&source, format!("fn revision_{revision}() {{}}\n")).unwrap();
            engine.sync().unwrap();
        }

        let status = engine.status().unwrap();
        assert_eq!(status.epoch, 301);
        assert_eq!(status.storage.wal_autocheckpoint_pages, 256);
        assert_eq!(status.storage.journal_size_limit_bytes, 16 * 1024 * 1024);
        assert!(status.storage.database_bytes > 0);
        assert!(
            status.storage.wal_bytes <= status.storage.journal_size_limit_bytes,
            "WAL was {} bytes",
            status.storage.wal_bytes
        );
    }

    #[test]
    fn injected_failure_cannot_publish_a_partial_graph_epoch() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function before() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let before = serde_json::to_string(&engine.snapshot().unwrap()).unwrap();
        let epoch = engine.status().unwrap().epoch;

        let replacement = parse_file("main.ts", "function after() {}\n").unwrap();
        engine
            .store
            .inject_rolled_back_publish(&[replacement], &[])
            .unwrap();

        assert_eq!(engine.status().unwrap().epoch, epoch);
        assert_eq!(
            serde_json::to_string(&engine.snapshot().unwrap()).unwrap(),
            before
        );
        assert!(engine.search("after", 10).unwrap().is_empty());
        assert_eq!(
            engine.search("before", 10).unwrap()[0].symbol.name,
            "before"
        );
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

    #[test]
    fn resolves_import_extends_and_implements_with_evidence() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("base.ts"),
            "export class Base {}\nexport interface Contract {}\nexport function helper() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("child.ts"),
            "import { helper } from './base';\nclass Child extends Base implements Contract {}\nfunction run() { helper(); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let id = |name: &str| {
            snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .unwrap()
                .id
                .clone()
        };
        let expected = [
            (id("child.ts"), id("helper"), RelationshipKind::Imports),
            (id("Child"), id("Base"), RelationshipKind::Extends),
            (id("Child"), id("Contract"), RelationshipKind::Implements),
        ];
        for (source, target, kind) in expected {
            let relationship = snapshot
                .relationships
                .iter()
                .find(|edge| {
                    edge.source_id == source && edge.target_id == target && edge.kind == kind
                })
                .unwrap_or_else(|| panic!("missing {kind} relationship"));
            assert!(relationship.evidence.provenance.starts_with("tree-sitter/"));
            assert!(relationship.evidence.confidence >= 0.9);
        }
    }

    #[test]
    fn lexical_scope_beats_same_named_global_candidates() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("local.ts"),
            "function helper() {}\nfunction caller() { helper(); }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("remote.ts"),
            "function helper() { return 2; }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callees = engine.callees_named("caller", None, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol.file, "local.ts");
        assert_eq!(callees[0].evidence.confidence, 0.99);
        assert!(callees[0]
            .evidence
            .explanation
            .contains("same-file lexical scope"));
    }

    #[test]
    fn receiver_type_disambiguates_same_named_methods_in_one_file() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("services.ts"),
            "class UserService { save() {} }\n\
             class AuditService { save() {} }\n\
             function persist() {\n\
               const service = new UserService();\n\
               service.save();\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let persist = engine
            .search("persist", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "persist")
            .unwrap();
        let callees = engine.callees(&persist.symbol.id).unwrap();

        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].0.qualified_name, "UserService.save");
        assert_eq!(callees[0].1.confidence, 0.995);
        assert!(callees[0].1.explanation.contains("receiver type"));
    }

    #[test]
    fn receiver_resolution_is_precise_across_java_python_and_rust() {
        for (file, source, caller, expected) in [
            (
                "Services.java",
                "class UserService { void save() {} }\n\
                 class AuditService { void save() {} }\n\
                 class Runner { void persist() { UserService service = new UserService(); service.save(); } }\n",
                "persist",
                "UserService.save",
            ),
            (
                "services.py",
                "class UserService:\n\
                 \x20   def save(self): pass\n\
                 class AuditService:\n\
                 \x20   def save(self): pass\n\
                 def persist():\n\
                 \x20   service = UserService()\n\
                 \x20   service.save()\n",
                "persist",
                "UserService.save",
            ),
            (
                "services.rs",
                "struct UserService; struct AuditService;\n\
                 impl UserService { fn new() -> Self { Self } fn save(&self) {} }\n\
                 impl AuditService { fn save(&self) {} }\n\
                 fn persist() { let service = UserService::new(); service.save(); }\n",
                "persist",
                "UserService.save",
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join(file), source).unwrap();
            let (engine, _) = Engine::init(temp.path()).unwrap();
            let caller = engine
                .search(caller, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == caller)
                .unwrap();
            let save_edges = engine
                .callees(&caller.symbol.id)
                .unwrap()
                .into_iter()
                .filter(|(symbol, _)| symbol.name == "save")
                .collect::<Vec<_>>();
            assert_eq!(save_edges.len(), 1, "{file}");
            assert_eq!(save_edges[0].0.qualified_name, expected, "{file}");
            assert_eq!(save_edges[0].1.confidence, 0.995, "{file}");
        }
    }

    #[test]
    fn explicit_import_scope_beats_global_fallback() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("defs.ts"), "export function helper() {}\n").unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "import { helper } from './defs';\nfunction caller() { helper(); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callees = engine.callees_named("caller", None, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol.file, "defs.ts");
        assert_eq!(callees[0].evidence.confidence, 0.97);
        assert!(callees[0]
            .evidence
            .explanation
            .contains("explicit import scope"));
    }

    #[test]
    fn module_hints_and_aliases_disambiguate_imported_calls() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("defs.ts"),
            "export function helper() { return 'correct'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("other.ts"),
            "export function helper() { return 'wrong'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "import { helper as chosen } from './defs';\nfunction caller() { chosen(); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callees = engine.callees_named("caller", None, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol.file, "defs.ts");
        assert_eq!(callees[0].symbol.name, "helper");
        assert_eq!(callees[0].evidence.confidence, 0.97);
    }

    #[test]
    fn tsconfig_path_aliases_resolve_imports_and_calls_to_canonical_files() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("tsconfig.json"),
            r#"{
              // JSONC comments and trailing commas are ordinary in tsconfig.
              "compilerOptions": {
                "baseUrl": ".",
                "paths": { "@/*": ["missing/*", "src/*"], },
              },
            }"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("src/defs.ts"),
            "export function helper() { return 'aliased'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("other.ts"),
            "export function helper() { return 'wrong'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "import { helper as chosen } from '@/defs';\nfunction caller() { chosen(); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callees = engine.callees_named("caller", None, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol.file, "src/defs.ts");
        assert_eq!(callees[0].evidence.confidence, 0.97);
        assert!(engine.snapshot().unwrap().relationships.iter().any(|edge| {
            edge.kind == RelationshipKind::Imports
                && edge.evidence.provenance == "tsconfig/path-alias"
        }));
    }

    #[test]
    fn workspace_package_imports_constrain_cross_package_calls() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("packages/core/src")).unwrap();
        fs::write(
            temp.path().join("package.json"),
            r#"{"workspaces":["packages/*"]}"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("packages/core/package.json"),
            r#"{"name":"@acme/core","types":"src/index.ts"}"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("packages/core/src/index.ts"),
            "export function helper() { return 'workspace'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("other.ts"),
            "export function helper() { return 'wrong'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "import { helper as chosen } from '@acme/core';\nfunction caller() { chosen(); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callees = engine.callees_named("caller", None, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol.file, "packages/core/src/index.ts");
        assert!(engine.snapshot().unwrap().relationships.iter().any(|edge| {
            edge.kind == RelationshipKind::Imports
                && edge.evidence.provenance == "workspace/package"
        }));
    }

    #[test]
    fn named_callback_registrations_create_evidence_bearing_call_edges() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("callbacks.ts"),
            "export function later() {}\n\
             export function register() {\n\
               setTimeout(later, 10);\n\
               Promise.resolve().then(later);\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let register = engine
            .search("register", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "register")
            .unwrap()
            .symbol;
        let callbacks = engine
            .callees(&register.id)
            .unwrap()
            .into_iter()
            .filter(|(symbol, evidence)| {
                symbol.name == "later" && evidence.provenance == "tree-sitter/callback-registration"
            })
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 2);
        assert!(callbacks
            .iter()
            .all(|(_, evidence)| evidence.confidence == 0.95));
    }

    #[test]
    fn express_routes_are_symbols_wired_to_the_last_named_handler() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("handlers.ts"),
            "export function authorize() {}\nexport function showUser() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("routes.ts"),
            "import { authorize, showUser } from './handlers';\n\
             router.get('/users/:id', authorize, showUser);\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let route = engine
            .search("GET users", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.kind == crate::model::SymbolKind::Route)
            .unwrap()
            .symbol;
        assert_eq!(route.name, "GET /users/:id");
        let callees = engine.callees(&route.id).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].0.name, "showUser");
        assert_eq!(callees[0].0.file, "handlers.ts");
        assert_eq!(callees[0].1.provenance, "framework/express-route");
        assert_eq!(callees[0].1.confidence, 0.97);
    }

    #[test]
    fn route_adapter_rejects_non_router_get_calls_and_dynamic_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function handler() {}\n\
             cache.get('/not-a-route', handler);\n\
             const path = '/dynamic';\n\
             router.get(path, handler);\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| symbol.kind != crate::model::SymbolKind::Route));
    }

    #[test]
    fn literal_event_dispatch_links_to_registered_handler_and_cleans_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let source = "function onReady() {}\n\
                      function register() { bus.on('ready', onReady); }\n\
                      function dispatch() { bus.emit('ready'); }\n";
        fs::write(temp.path().join("events.ts"), source).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();

        let dispatch = engine
            .search("dispatch", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "dispatch")
            .unwrap()
            .symbol;
        let dynamic = engine
            .callees(&dispatch.id)
            .unwrap()
            .into_iter()
            .find(|(symbol, evidence)| {
                symbol.name == "onReady" && evidence.provenance == "dynamic/event-registration"
            })
            .unwrap();
        assert_eq!(dynamic.1.confidence, 0.92);
        assert!(dynamic.1.explanation.contains("events.ts:2"));

        fs::write(
            temp.path().join("events.ts"),
            "function onReady() {}\nfunction dispatch() { bus.emit('ready'); }\n",
        )
        .unwrap();
        engine.sync().unwrap();
        let dispatch = engine
            .search("dispatch", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "dispatch")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&dispatch.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "dynamic/event-registration"));
    }

    #[test]
    fn event_dispatch_fanout_is_capped_and_dynamic_channels_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let handlers = (0..7)
            .map(|index| format!("function handler{index}() {{}}\n"))
            .collect::<String>();
        let registrations = (0..7)
            .map(|index| format!("bus.on('ready', handler{index});\n"))
            .collect::<String>();
        fs::write(
            temp.path().join("events.ts"),
            format!(
                "{handlers}function register() {{\n{registrations}}}\n\
                 function dispatch() {{ bus.emit('ready'); bus.emit(dynamicName); }}\n"
            ),
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let dispatch = engine
            .search("dispatch", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "dispatch")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&dispatch.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "dynamic/event-registration"));
    }

    #[test]
    fn parameter_invocation_does_not_bind_to_unrelated_global_symbol() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function callback() {}\n\
             function invoke(callback: () => void) { callback(); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let invoke = engine
            .search("invoke", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "invoke")
            .unwrap()
            .symbol;
        assert!(engine.callees(&invoke.id).unwrap().is_empty());
    }

    #[test]
    fn function_reference_roles_resolve_imports_and_surface_in_explore() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("target.ts"),
            "export function transform() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("decoy.ts"),
            "export function transform() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "import { transform as selected } from './target';\n\
             function configure() {\n\
             \x20 const alias = selected;\n\
             \x20 const table = { callback: selected };\n\
             \x20 const pipeline = [selected];\n\
             \x20 holder.callback = selected;\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let configure = engine
            .search("configure", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "configure")
            .unwrap()
            .symbol;
        let references = engine.references(&configure.id).unwrap();
        assert_eq!(references.len(), 4);
        assert!(references
            .iter()
            .all(|(symbol, evidence)| symbol.file == "target.ts"
                && symbol.name == "transform"
                && evidence.provenance == "tree-sitter/function-reference"
                && evidence.confidence == 0.95));

        let explored = engine.explore("configure", 5).unwrap();
        let configure_hit = explored
            .iter()
            .find(|hit| hit.symbol.name == "configure")
            .unwrap();
        assert_eq!(configure_hit.references.len(), 4);
    }

    #[test]
    fn function_references_prefer_local_methods_and_refuse_global_ambiguity() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("local.ts"),
            "class Runner {\n\
             \x20 handle() {}\n\
             \x20 configure() { const callback = this.handle; }\n\
             }\n",
        )
        .unwrap();
        fs::write(temp.path().join("a.ts"), "export function shared() {}\n").unwrap();
        fs::write(temp.path().join("b.ts"), "export function shared() {}\n").unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function configureGlobal() { const callback = shared; }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let local = engine
            .search("Runner.configure", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Runner.configure")
            .unwrap()
            .symbol;
        let references = engine.references(&local.id).unwrap();
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].0.qualified_name, "Runner.handle");

        let global = engine
            .search("configureGlobal", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "configureGlobal")
            .unwrap()
            .symbol;
        assert!(engine.references(&global.id).unwrap().is_empty());
    }

    #[test]
    fn fastapi_decorators_create_routes_wired_to_sync_and_async_handlers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "@app.get('/users/{user_id}')\n\
             async def show_user(user_id: str):\n\
             \x20   return user_id\n\n\
             @router.post(\n\
             \x20   '',\n\
             )\n\
             def create_user():\n\
             \x20   return None\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let routes = snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .collect::<Vec<_>>();
        assert_eq!(routes.len(), 2);
        assert!(routes
            .iter()
            .any(|route| route.name == "GET /users/{user_id}"));
        assert!(routes.iter().any(|route| route.name == "POST /"));
        for (route_name, handler_name) in [
            ("GET /users/{user_id}", "show_user"),
            ("POST /", "create_user"),
        ] {
            let route = routes
                .iter()
                .find(|route| route.name == route_name)
                .unwrap();
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].0.name, handler_name);
            assert_eq!(callees[0].1.provenance, "framework/fastapi-route");
            assert_eq!(callees[0].1.confidence, 0.99);
        }
    }

    #[test]
    fn fastapi_adapter_ignores_docstrings_comments_and_dynamic_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "\"\"\"Example: @app.get('/not-real')\"\"\"\n\
             # @router.post('/also-not-real')\n\
             path = '/dynamic'\n\
             @app.get(path)\n\
             def dynamic_route():\n\
             \x20   pass\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| symbol.kind != crate::model::SymbolKind::Route));
    }

    #[test]
    fn react_router_jsx_routes_resolve_v5_and_v6_imported_components() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("dashboard.tsx"),
            "export function Dashboard() { return <main />; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("decoy.tsx"),
            "export function Dashboard() { return <aside />; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("routes.tsx"),
            "import { Dashboard as Page } from './dashboard';\n\
             export function AppRoutes() {\n\
             \x20 return <Routes>\n\
             \x20   <Route element={<Page />} path=\"/dashboard\" />\n\
             \x20   <Route component={Page} path=\"/legacy\" />\n\
             \x20 </Routes>;\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        for route_name in ["ROUTE /dashboard", "ROUTE /legacy"] {
            let route = engine
                .search(route_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.kind == crate::model::SymbolKind::Route)
                .unwrap()
                .symbol;
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1, "{route_name}");
            assert_eq!(callees[0].0.name, "Dashboard");
            assert_eq!(callees[0].0.file, "dashboard.tsx");
            assert_eq!(callees[0].1.provenance, "framework/react-router");
            assert_eq!(callees[0].1.confidence, 0.97);
        }
    }

    #[test]
    fn react_router_object_routes_support_component_and_element_forms() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("routes.tsx"),
            "function Home() { return <main />; }\n\
             function Settings() { return <section />; }\n\
             const router = createBrowserRouter([\n\
             \x20 { path: '/', Component: Home },\n\
             \x20 { path: '/settings', element: <Settings /> },\n\
             ]);\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        for (route_name, component) in [("ROUTE /", "Home"), ("ROUTE /settings", "Settings")] {
            let route = engine
                .search(route_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.kind == crate::model::SymbolKind::Route)
                .unwrap()
                .symbol;
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].0.name, component);
            assert_eq!(callees[0].1.provenance, "framework/react-router");
        }
    }

    #[test]
    fn react_router_adapter_rejects_routes_container_and_dynamic_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("routes.tsx"),
            "function Page() { return <main />; }\n\
             const dynamicPath = '/dynamic';\n\
             export const routes = <Routes path=\"/not-a-route\">\n\
             \x20 <Route path={dynamicPath} element={<Page />} />\n\
             </Routes>;\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| symbol.kind != crate::model::SymbolKind::Route));
    }

    #[test]
    fn watcher_makes_saved_symbols_query_visible() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function before() {}\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let watcher_stop = Arc::clone(&stop);
        let (report_sender, report_receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let handle = std::thread::spawn(move || {
            let mut engine = engine;
            engine
                .watch_ready(
                    watcher_stop,
                    Duration::from_millis(50),
                    || {
                        let _ = ready_sender.send(());
                    },
                    |report| {
                        let _ = report_sender.send(report.clone());
                    },
                )
                .unwrap();
            engine
        });

        ready_receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        fs::write(temp.path().join("main.ts"), "function afterSave() {}\n").unwrap();
        let report = report_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert_eq!(report.files_changed, 1);
        stop.store(true, Ordering::Relaxed);
        let engine = handle.join().unwrap();
        let hits = engine.search("afterSave", 10).unwrap();
        assert_eq!(hits[0].symbol.name, "afterSave");
    }
}
