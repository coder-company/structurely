use crate::{
    budget::ResourceBudget,
    content::{is_corrupt_content_index, ContentHit, ContentIndex},
    inventory::{InventoryStale, ProjectInventory},
    model::{Evidence, RelationshipKind},
    parser::parse_file_as,
    project_resolution::ProjectResolutionContext,
    source::{read_source_snapshot, SourceRead},
    store::{
        is_corrupt_database, ConcurrentPublication, FileSummary, SearchHit, StorageMetrics, Store,
    },
};
use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
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
pub struct Engine {
    root: PathBuf,
    store: Store,
    content: Option<ContentIndex>,
    storage_recovery: Option<String>,
    _writer_lock: Option<std::fs::File>,
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
    pub content_files_indexed: usize,
    pub content_files_changed: usize,
    pub content_files_deleted: usize,
    pub content_chunks: usize,
    pub parse_workers: usize,
    pub staging_ms: u128,
    pub resolution_ms: u128,
    pub duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintenance_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectStatus {
    pub root: String,
    pub database: String,
    pub epoch: u64,
    pub indexed_files: usize,
    pub symbols: usize,
    pub relationships: usize,
    pub pending_files: usize,
    pub skipped_files: usize,
    pub storage: StorageMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_recovery: Option<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathTraceStatus {
    Found,
    NoPath,
    SourceNotFound,
    TargetNotFound,
    AmbiguousSource,
    AmbiguousTarget,
    AmbiguousEndpoints,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathTraceStep {
    pub source: crate::model::Symbol,
    pub target: crate::model::Symbol,
    pub relationship: RelationshipKind,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathTraceResult {
    pub status: PathTraceStatus,
    pub source_candidates: Vec<crate::model::Symbol>,
    pub target_candidates: Vec<crate::model::Symbol>,
    pub path: Vec<PathTraceStep>,
    pub examined_nodes: usize,
    pub examined_edges: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExploreHit {
    pub symbol: crate::model::Symbol,
    pub score: f64,
    pub source: String,
    pub source_truncated: bool,
    pub callers: Vec<(crate::model::Symbol, Evidence)>,
    pub callees: Vec<(crate::model::Symbol, Evidence)>,
    pub referenced_by: Vec<(crate::model::Symbol, Evidence)>,
    pub references: Vec<(crate::model::Symbol, Evidence)>,
    pub relationships_truncated: bool,
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

type WatcherErrorSlot = Arc<Mutex<Option<notify::Error>>>;

fn create_project_watcher(
    root: &Path,
) -> Result<(
    notify::RecommendedWatcher,
    mpsc::Receiver<()>,
    WatcherErrorSlot,
)> {
    const WATCH_SIGNAL_CAPACITY: usize = 1;
    let (sender, receiver) = mpsc::sync_channel(WATCH_SIGNAL_CAPACITY);
    let errors = Arc::new(Mutex::new(None));
    let callback_errors = Arc::clone(&errors);
    let callback_root = root.to_owned();
    let mut watcher =
        notify::recommended_watcher(move |result: notify::Result<Event>| match result {
            Ok(event) if relevant_watch_event(&callback_root, &event) => {
                let _ = sender.try_send(());
            }
            Ok(_) => {}
            Err(error) => {
                if let Ok(mut slot) = callback_errors.lock() {
                    *slot = Some(error);
                }
                let _ = sender.try_send(());
            }
        })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok((watcher, receiver, errors))
}

fn relevant_watch_event(root: &Path, event: &Event) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|path| {
        path.strip_prefix(root).is_ok_and(|relative| {
            !relative
                .components()
                .any(|part| part.as_os_str() == PROJECT_DIR)
                && (crate::model::Language::from_path(relative).is_some()
                    || path.is_dir()
                    || !path.exists())
        })
    })
}

impl Engine {
    pub fn init(root: impl AsRef<Path>) -> Result<(Self, IndexReport)> {
        let root = absolute(root.as_ref())?;
        ensure_project_directory(&root, true)?;
        let mut engine = Self::open(&root)?;
        let report = engine.sync()?;
        Ok((engine, report))
    }

    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref())
    }

    pub(crate) fn open_for_daemon(root: impl AsRef<Path>) -> Result<Self> {
        Self::open_inner(root.as_ref())
    }

    fn open_inner(root: &Path) -> Result<Self> {
        let root = absolute(root)?;
        ensure_project_directory(&root, false)?;
        let database = root.join(PROJECT_DIR).join(DATABASE_FILE);
        let mut writer_lock = Some(writer_coordination_lock(&database, false)?);
        let (store, mut storage_recovery) = match Store::open(&database) {
            Ok(store) => (store, None),
            Err(error) if is_corrupt_database(&error) => {
                drop(writer_lock.take());
                let recovery_lock = Some(writer_coordination_lock(&database, true).map_err(
                    |lock_error| {
                        anyhow!("coordinate graph database recovery after {error}: {lock_error}")
                    },
                )?);
                let recovery = quarantine_database_set(&database)
                    .with_context(|| format!("preserve corrupt graph database: {error}"))?;
                let store = Store::open(&database)
                    .with_context(|| format!("rebuild graph database after {error}"))?;
                drop(recovery_lock);
                writer_lock = Some(writer_coordination_lock(&database, false)?);
                (
                    store,
                    Some(format!(
                        "corrupt graph database was preserved at {} and rebuilt",
                        recovery.display()
                    )),
                )
            }
            Err(error) => return Err(error),
        };
        let content_database = root.join(PROJECT_DIR).join("content.db");
        let content = match ContentIndex::open(&root) {
            Ok(content) => Some(content),
            Err(error) if is_corrupt_content_index(&error) => {
                let recovery = quarantine_database_set(&content_database)
                    .with_context(|| format!("preserve corrupt content index: {error}"))?;
                let content = ContentIndex::open(&root)
                    .with_context(|| format!("rebuild content index after {error}"))?;
                let message = format!(
                    "corrupt content index was preserved at {} and rebuilt",
                    recovery.display()
                );
                storage_recovery = Some(match storage_recovery {
                    Some(existing) => format!("{existing}; {message}"),
                    None => message,
                });
                Some(content)
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            root,
            store,
            content,
            storage_recovery,
            _writer_lock: writer_lock,
        })
    }

    pub fn open_read_only(root: impl AsRef<Path>) -> Result<Self> {
        let root = absolute(root.as_ref())?;
        ensure_project_directory(&root, false)?;
        let database = root.join(PROJECT_DIR).join(DATABASE_FILE);
        let content = ContentIndex::open_read_only(&root)?;
        Ok(Self {
            root,
            store: Store::open_read_only(&database)?,
            content,
            storage_recovery: None,
            _writer_lock: None,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn committed_epoch(&self) -> Result<u64> {
        self.store.epoch()
    }

    pub fn sync(&mut self) -> Result<IndexReport> {
        const MAX_SYNC_ATTEMPTS: usize = 3;
        const INITIAL_RETRY_DELAY_MS: u64 = 5;
        let started = Instant::now();
        for attempt in 0..MAX_SYNC_ATTEMPTS {
            match self.sync_once() {
                Ok(mut report) => {
                    if let Some(content) = self.content.as_mut() {
                        let content_report = content.sync()?;
                        report.content_files_indexed = content_report.files_indexed;
                        report.content_files_changed = content_report.files_changed;
                        report.content_files_deleted = content_report.files_deleted;
                        report.content_chunks = content_report.chunks;
                    }
                    report.duration_ms = started.elapsed().as_millis();
                    if let Some(recovery) = &self.storage_recovery {
                        report.maintenance_warning = Some(match report.maintenance_warning {
                            Some(warning) => format!("{recovery}; {warning}"),
                            None => recovery.clone(),
                        });
                    }
                    return Ok(report);
                }
                Err(error)
                    if (error.downcast_ref::<InventoryStale>().is_some()
                        || error.downcast_ref::<ConcurrentPublication>().is_some())
                        && attempt + 1 < MAX_SYNC_ATTEMPTS =>
                {
                    thread::sleep(Duration::from_millis(INITIAL_RETRY_DELAY_MS << attempt));
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("bounded sync attempts always return")
    }

    fn sync_once(&mut self) -> Result<IndexReport> {
        const RESOLUTION_FINGERPRINT_KEY: &str = "project_resolution_fingerprint";
        let started = Instant::now();
        let resolution_context = Arc::new(ProjectResolutionContext::load(&self.root));
        let resolution_changed = self
            .store
            .metadata_value(RESOLUTION_FINGERPRINT_KEY)?
            .as_deref()
            != Some(resolution_context.fingerprint());
        let force_reindex = !self.store.is_current_graph_model()? || resolution_changed;
        let indexed = self.store.indexed_file_hashes()?;
        let inventory = ProjectInventory::new(&self.root)?;
        let mut delta = inventory.delta(&indexed, force_reindex)?;
        let header_context_changed = resolution_context.has_compilation_database()
            && (delta
                .changed
                .iter()
                .any(|snapshot| is_c_header(&snapshot.relative))
                || delta.deleted.iter().any(|path| is_c_header(path)));
        if header_context_changed && !force_reindex {
            drop(delta);
            delta = inventory.delta(&indexed, true)?;
        }
        let changed = delta.changed;
        let deleted = delta.deleted;
        let files_scanned = delta.files_scanned;
        let files_skipped = delta.files_skipped;
        let files_changed = changed.len();
        let available_workers = thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(1);
        let parse_workers = parse_worker_count(
            std::env::var("STRUCTURELY_PARSE_WORKERS").ok().as_deref(),
            available_workers,
            files_changed,
        );
        let (
            epoch,
            relationships_resolved,
            symbols_changed,
            staging_ms,
            resolution_ms,
            maintenance_warning,
        ) = if changed.is_empty() && deleted.is_empty() {
            (self.store.epoch()?, 0, 0, 0, 0, None)
        } else if parse_workers == 1 {
            let facts = changed.into_iter().map(|snapshot| {
                let (relative, language, source) = snapshot.into_parts()?;
                let mut facts = parse_file_as(&relative, &source, language)?;
                resolution_context.apply(&mut facts);
                Ok(facts)
            });
            self.store.publish(
                facts,
                &deleted,
                &[(RESOLUTION_FINGERPRINT_KEY, resolution_context.fingerprint())],
            )?
        } else {
            thread::scope(|scope| {
                let (work_sender, work_receiver) =
                    mpsc::channel::<crate::inventory::SourceSnapshot>();
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
                        let Ok(snapshot) = work else {
                            return;
                        };
                        let facts = (|| {
                            let (relative, language, source) = snapshot.into_parts()?;
                            let mut facts = parse_file_as(&relative, &source, language)?;
                            resolution_context.apply(&mut facts);
                            Ok(facts)
                        })();
                        if result_sender.send(facts).is_err() {
                            return;
                        }
                    });
                }
                drop(result_sender);
                for work in changed {
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
                self.store.publish(
                    facts,
                    &deleted,
                    &[(RESOLUTION_FINGERPRINT_KEY, resolution_context.fingerprint())],
                )
            })?
        };
        if (resolution_changed || force_reindex) && files_changed == 0 && deleted.is_empty() {
            self.store.mark_empty_graph_current(&[(
                RESOLUTION_FINGERPRINT_KEY,
                resolution_context.fingerprint(),
            )])?;
        }

        Ok(IndexReport {
            epoch,
            files_scanned,
            files_skipped,
            files_changed,
            files_deleted: deleted.len(),
            symbols_changed,
            relationships_resolved,
            content_files_indexed: 0,
            content_files_changed: 0,
            content_files_deleted: 0,
            content_chunks: 0,
            parse_workers,
            staging_ms,
            resolution_ms,
            duration_ms: started.elapsed().as_millis(),
            maintenance_warning,
        })
    }

    pub fn status(&self) -> Result<ProjectStatus> {
        let indexed = self.store.indexed_file_hashes()?;
        let (symbols, relationships) = self.store.graph_counts()?;
        let delta = ProjectInventory::new(&self.root)?.delta(&indexed, false)?;
        let pending = delta.changed.len() + delta.deleted.len();
        Ok(ProjectStatus {
            root: self.root.display().to_string(),
            database: self.store.path().display().to_string(),
            epoch: self.store.epoch()?,
            indexed_files: indexed.len(),
            symbols,
            relationships,
            pending_files: pending,
            skipped_files: delta.files_skipped,
            storage: self.store.storage_metrics()?,
            storage_recovery: self.storage_recovery.clone(),
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.store.search(
            ResourceBudget::query(query)?,
            ResourceBudget::result_limit(limit)?,
        )
    }

    pub fn search_filtered(
        &self,
        query: &str,
        kind: Option<crate::model::SymbolKind>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        self.store.search_filtered(
            ResourceBudget::query(query)?,
            kind,
            ResourceBudget::result_limit(limit)?,
        )
    }

    pub fn files(&self) -> Result<Vec<FileSummary>> {
        self.store.file_summaries()
    }

    pub fn content_search(&self, query: &str, limit: usize) -> Result<Vec<ContentHit>> {
        match &self.content {
            Some(content) => content.search(
                ResourceBudget::query(query)?,
                ResourceBudget::result_limit(limit)?,
            ),
            None => Ok(Vec::new()),
        }
    }

    pub fn content_counts(&self) -> Result<(usize, usize)> {
        match &self.content {
            Some(content) => content.counts(),
            None => Ok((0, 0)),
        }
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
        let query = ResourceBudget::query(query)?;
        let iterations = ResourceBudget::benchmark_iterations(iterations)?;
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
        self.watch_resilient(
            stop,
            debounce,
            on_ready,
            |report| {
                if report.files_changed > 0 || report.files_deleted > 0 {
                    on_sync(report);
                }
            },
            |_| {},
            |_| {},
        )
    }

    pub fn watch_resilient(
        &mut self,
        stop: Arc<AtomicBool>,
        debounce: Duration,
        on_ready: impl FnOnce(),
        mut on_reconcile: impl FnMut(&IndexReport),
        mut on_error: impl FnMut(&anyhow::Error),
        mut on_polling: impl FnMut(&IndexReport),
    ) -> Result<()> {
        const RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
        const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);
        let poll = Duration::from_millis(50);
        let mut fallback_sender = None;
        let mut watcher_needs_rebuild = false;
        let mut initial_watcher_error = None;
        let (mut watcher, mut receiver, mut watcher_errors) =
            match create_project_watcher(&self.root) {
                Ok((watcher, receiver, errors)) => (Some(watcher), receiver, errors),
                Err(error) => {
                    let (sender, receiver) = mpsc::sync_channel(1);
                    fallback_sender = Some(sender);
                    watcher_needs_rebuild = true;
                    initial_watcher_error = Some(error);
                    (None, receiver, Arc::new(Mutex::new(None)))
                }
            };
        on_ready();
        let mut last_relevant_event: Option<Instant> = None;
        let mut last_reconcile = Instant::now();
        let mut sync_retry_delay = Duration::from_millis(50);
        let mut sync_retry_at: Option<Instant> = None;
        let mut watcher_retry_delay = Duration::from_millis(50);
        let mut watcher_retry_at: Option<Instant> = None;
        let mut last_sync_error = None;
        let mut degraded = initial_watcher_error.is_some();
        let mut first_reconcile = true;
        if let Some(error) = initial_watcher_error {
            on_error(&error);
        }

        while !stop.load(Ordering::Relaxed) {
            match receiver.recv_timeout(poll) {
                Ok(()) => {
                    last_relevant_event = Some(Instant::now());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if !watcher_needs_rebuild {
                        let error = anyhow!(
                            "filesystem watcher disconnected; using polling while rebuilding"
                        );
                        on_error(&error);
                        degraded = true;
                        drop(watcher.take());
                    }
                    watcher_needs_rebuild = true;
                    watcher_retry_at.get_or_insert(Instant::now());
                    let (sender, fallback_receiver) = mpsc::sync_channel(1);
                    fallback_sender = Some(sender);
                    receiver = fallback_receiver;
                }
            }
            if let Some(error) = watcher_errors
                .lock()
                .ok()
                .and_then(|mut error| error.take())
            {
                if !watcher_needs_rebuild {
                    let error = anyhow!("filesystem watcher error: {error}");
                    on_error(&error);
                    degraded = true;
                    drop(watcher.take());
                }
                watcher_needs_rebuild = true;
                watcher_retry_at.get_or_insert(Instant::now());
            }
            if watcher_needs_rebuild
                && watcher_retry_at.is_none_or(|deadline| Instant::now() >= deadline)
            {
                match create_project_watcher(&self.root) {
                    Ok((replacement, replacement_receiver, replacement_errors)) => {
                        watcher = Some(replacement);
                        receiver = replacement_receiver;
                        watcher_errors = replacement_errors;
                        fallback_sender = None;
                        watcher_needs_rebuild = false;
                        watcher_retry_at = None;
                        watcher_retry_delay = Duration::from_millis(50);
                        last_relevant_event = Some(Instant::now());
                    }
                    Err(_) => {
                        watcher_retry_at = Some(Instant::now() + watcher_retry_delay);
                        watcher_retry_delay =
                            watcher_retry_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
                    }
                }
            }

            let debounce_ready = last_relevant_event.is_some_and(|last| last.elapsed() >= debounce);
            let periodic_ready = last_reconcile.elapsed() >= RECONCILE_INTERVAL;
            let retry_ready = sync_retry_at.is_some_and(|deadline| Instant::now() >= deadline);
            if (debounce_ready || periodic_ready || retry_ready)
                && sync_retry_at.is_none_or(|deadline| Instant::now() >= deadline)
            {
                match self.sync() {
                    Ok(report) => {
                        let recovered = last_sync_error.take().is_some();
                        if watcher_needs_rebuild {
                            if recovered
                                || report.files_changed > 0
                                || report.files_deleted > 0
                                || report.maintenance_warning.is_some()
                            {
                                on_polling(&report);
                            }
                        } else if first_reconcile
                            || degraded
                            || recovered
                            || report.files_changed > 0
                            || report.files_deleted > 0
                            || report.maintenance_warning.is_some()
                        {
                            on_reconcile(&report);
                        }
                        first_reconcile = false;
                        degraded = watcher_needs_rebuild;
                        last_relevant_event = None;
                        last_reconcile = Instant::now();
                        sync_retry_delay = Duration::from_millis(50);
                        sync_retry_at = None;
                    }
                    Err(error) => {
                        on_error(&error);
                        degraded = true;
                        last_sync_error = Some(error.to_string());
                        sync_retry_at = Some(Instant::now() + sync_retry_delay);
                        sync_retry_delay = sync_retry_delay.saturating_mul(2).min(MAX_RETRY_DELAY);
                    }
                }
            }
        }
        match self.sync() {
            Ok(report) => {
                if watcher_needs_rebuild {
                    on_polling(&report);
                } else if first_reconcile
                    || degraded
                    || last_sync_error.is_some()
                    || report.files_changed > 0
                    || report.files_deleted > 0
                    || report.maintenance_warning.is_some()
                {
                    on_reconcile(&report);
                }
            }
            Err(error) => {
                on_error(&error);
                return Err(error);
            }
        }
        drop(fallback_sender);
        drop(watcher);
        Ok(())
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
        let symbol = symbol.map(ResourceBudget::identifier).transpose()?;
        let file = file.map(ResourceBudget::identifier).transpose()?;
        let offset = offset.map(ResourceBudget::node_offset).transpose()?;
        let limit = limit.map(ResourceBudget::node_lines).transpose()?;
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
            let raw = read_bounded_source(&self.root.join(&path))?;
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
                    let start = ResourceBudget::node_offset(offset.unwrap_or(1))?.saturating_sub(1);
                    let maximum = ResourceBudget::node_lines(
                        limit.unwrap_or(ResourceBudget::MAX_NODE_LINES),
                    )?;
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
        let query = ResourceBudget::query(query)?;
        let max_files = ResourceBudget::result_limit(max_files)?;
        let mut files = HashSet::new();
        let mut sources = HashMap::new();
        let mut output = Vec::new();
        for hit in self
            .store
            .search_candidates(query, max_files.saturating_mul(4))?
        {
            if !files.contains(&hit.symbol.file) && files.len() >= max_files {
                continue;
            }
            files.insert(hit.symbol.file.clone());
            if !sources.contains_key(&hit.symbol.file) {
                let path = self.root.join(&hit.symbol.file);
                sources.insert(hit.symbol.file.clone(), read_bounded_source(&path)?);
            }
            let source = &sources[&hit.symbol.file];
            let snippet = source
                .get(hit.symbol.start_byte..hit.symbol.end_byte)
                .unwrap_or_default();
            let (snippet, source_truncated) = bounded_source(snippet, 4_000);
            let mut relationships_truncated = false;
            let callers = self.explore_relationships(
                &hit.symbol.id,
                true,
                RelationshipKind::Calls,
                &mut relationships_truncated,
            )?;
            let callees = self.explore_relationships(
                &hit.symbol.id,
                false,
                RelationshipKind::Calls,
                &mut relationships_truncated,
            )?;
            let referenced_by = self.explore_relationships(
                &hit.symbol.id,
                true,
                RelationshipKind::References,
                &mut relationships_truncated,
            )?;
            let references = self.explore_relationships(
                &hit.symbol.id,
                false,
                RelationshipKind::References,
                &mut relationships_truncated,
            )?;
            output.push(ExploreHit {
                callers,
                callees,
                referenced_by,
                references,
                score: hit.score,
                symbol: hit.symbol,
                source: snippet,
                source_truncated,
                relationships_truncated,
            });
        }
        Ok(output)
    }

    fn explore_relationships(
        &self,
        symbol_id: &str,
        incoming: bool,
        kind: RelationshipKind,
        truncated: &mut bool,
    ) -> Result<Vec<(crate::model::Symbol, Evidence)>> {
        let mut relationships = self.store.related_limited(
            symbol_id,
            incoming,
            kind,
            ResourceBudget::MAX_EXPLORE_RELATIONSHIPS + 1,
        )?;
        if relationships.len() > ResourceBudget::MAX_EXPLORE_RELATIONSHIPS {
            relationships.truncate(ResourceBudget::MAX_EXPLORE_RELATIONSHIPS);
            *truncated = true;
        }
        Ok(relationships)
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
        let file = file.map(ResourceBudget::identifier).transpose()?;
        self.related_named(
            ResourceBudget::identifier(symbol)?,
            file,
            ResourceBudget::result_limit(limit)?,
            true,
        )
    }

    pub fn callees_named(
        &self,
        symbol: &str,
        file: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RelatedHit>> {
        let file = file.map(ResourceBudget::identifier).transpose()?;
        self.related_named(
            ResourceBudget::identifier(symbol)?,
            file,
            ResourceBudget::result_limit(limit)?,
            false,
        )
    }

    pub fn impact_named(
        &self,
        symbol: &str,
        file: Option<&str>,
        max_depth: usize,
    ) -> Result<Vec<ImpactHit>> {
        let symbol = ResourceBudget::identifier(symbol)?;
        let file = file.map(ResourceBudget::identifier).transpose()?;
        let max_depth = ResourceBudget::traversal_depth(max_depth)?;
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
        let mut examined_edges = 0usize;
        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            let callers = self
                .store
                .related(&current.id, true, RelationshipKind::Calls)?;
            examined_edges = examined_edges
                .checked_add(callers.len())
                .ok_or_else(|| anyhow!("impact edge work counter overflowed"))?;
            if examined_edges > ResourceBudget::MAX_IMPACT_EDGES {
                bail!(
                    "impact traversal exceeded the {}-edge work limit",
                    ResourceBudget::MAX_IMPACT_EDGES
                );
            }
            for (caller, evidence) in callers {
                if visited.insert(caller.id.clone()) {
                    if output.len() >= ResourceBudget::MAX_IMPACT_NODES {
                        bail!(
                            "impact traversal exceeded the {}-node work limit",
                            ResourceBudget::MAX_IMPACT_NODES
                        );
                    }
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

    /// Finds the shortest directed relationship path between two symbols.
    ///
    /// Names and qualified names are accepted, but ambiguous endpoints are
    /// returned to the caller instead of silently choosing one. A file suffix
    /// can disambiguate either endpoint. Every step retains the graph evidence
    /// that caused it to be traversed.
    pub fn trace_path_named(
        &self,
        source: &str,
        source_file: Option<&str>,
        target: &str,
        target_file: Option<&str>,
        max_depth: usize,
    ) -> Result<PathTraceResult> {
        let source = ResourceBudget::identifier(source)?;
        let target = ResourceBudget::identifier(target)?;
        let source_file = source_file.map(ResourceBudget::identifier).transpose()?;
        let target_file = target_file.map(ResourceBudget::identifier).transpose()?;
        let max_depth = ResourceBudget::traversal_depth(max_depth)?;
        let source_candidates = self.trace_candidates(source, source_file)?;
        let target_candidates = self.trace_candidates(target, target_file)?;

        let endpoint_status = match (source_candidates.len(), target_candidates.len()) {
            (0, _) => Some(PathTraceStatus::SourceNotFound),
            (_, 0) => Some(PathTraceStatus::TargetNotFound),
            (1, 1) => None,
            (1, _) => Some(PathTraceStatus::AmbiguousTarget),
            (_, 1) => Some(PathTraceStatus::AmbiguousSource),
            (_, _) => Some(PathTraceStatus::AmbiguousEndpoints),
        };
        if let Some(status) = endpoint_status {
            let guidance = match status {
                PathTraceStatus::SourceNotFound => "source symbol was not found",
                PathTraceStatus::TargetNotFound => "target symbol was not found",
                PathTraceStatus::AmbiguousSource => {
                    "source matched multiple symbols; provide source_file or a symbol ID"
                }
                PathTraceStatus::AmbiguousTarget => {
                    "target matched multiple symbols; provide target_file or a symbol ID"
                }
                PathTraceStatus::AmbiguousEndpoints => {
                    "both endpoints matched multiple symbols; provide file suffixes or symbol IDs"
                }
                PathTraceStatus::Found | PathTraceStatus::NoPath => unreachable!(),
            };
            return Ok(PathTraceResult {
                status,
                source_candidates,
                target_candidates,
                path: Vec::new(),
                examined_nodes: 0,
                examined_edges: 0,
                guidance: Some(guidance.to_owned()),
            });
        }

        let origin = source_candidates[0].clone();
        let destination = target_candidates[0].clone();
        if origin.id == destination.id {
            return Ok(PathTraceResult {
                status: PathTraceStatus::Found,
                source_candidates,
                target_candidates,
                path: Vec::new(),
                examined_nodes: 1,
                examined_edges: 0,
                guidance: None,
            });
        }

        type Predecessor = (
            String,
            crate::model::Symbol,
            crate::model::Symbol,
            RelationshipKind,
            Evidence,
        );
        let mut queue = std::collections::VecDeque::from([(origin.clone(), 0usize)]);
        let mut visited = HashSet::from([origin.id.clone()]);
        let mut predecessors = HashMap::<String, Predecessor>::new();
        let mut examined_edges = 0usize;
        let relationship_kinds = [
            RelationshipKind::Calls,
            RelationshipKind::References,
            RelationshipKind::Imports,
            RelationshipKind::Extends,
            RelationshipKind::Implements,
            RelationshipKind::Contains,
        ];

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            for kind in relationship_kinds {
                let neighbors = self.store.related(&current.id, false, kind)?;
                examined_edges = examined_edges
                    .checked_add(neighbors.len())
                    .ok_or_else(|| anyhow!("path trace edge work counter overflowed"))?;
                if examined_edges > ResourceBudget::MAX_IMPACT_EDGES {
                    bail!(
                        "path trace exceeded the {}-edge work limit",
                        ResourceBudget::MAX_IMPACT_EDGES
                    );
                }
                for (neighbor, evidence) in neighbors {
                    if !visited.insert(neighbor.id.clone()) {
                        continue;
                    }
                    if visited.len() > ResourceBudget::MAX_IMPACT_NODES {
                        bail!(
                            "path trace exceeded the {}-node work limit",
                            ResourceBudget::MAX_IMPACT_NODES
                        );
                    }
                    predecessors.insert(
                        neighbor.id.clone(),
                        (
                            current.id.clone(),
                            current.clone(),
                            neighbor.clone(),
                            kind,
                            evidence,
                        ),
                    );
                    if neighbor.id == destination.id {
                        let mut path = Vec::new();
                        let mut cursor = destination.id.clone();
                        while cursor != origin.id {
                            let (previous, source, target, relationship, evidence) =
                                predecessors.remove(&cursor).ok_or_else(|| {
                                    anyhow!("path trace predecessor chain is incomplete")
                                })?;
                            path.push(PathTraceStep {
                                source,
                                target,
                                relationship,
                                evidence,
                            });
                            cursor = previous;
                        }
                        path.reverse();
                        return Ok(PathTraceResult {
                            status: PathTraceStatus::Found,
                            source_candidates,
                            target_candidates,
                            path,
                            examined_nodes: visited.len(),
                            examined_edges,
                            guidance: None,
                        });
                    }
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        Ok(PathTraceResult {
            status: PathTraceStatus::NoPath,
            source_candidates,
            target_candidates,
            path: Vec::new(),
            examined_nodes: visited.len(),
            examined_edges,
            guidance: Some(format!(
                "no directed relationship path was found within depth {max_depth}"
            )),
        })
    }

    fn trace_candidates(
        &self,
        identifier: &str,
        file: Option<&str>,
    ) -> Result<Vec<crate::model::Symbol>> {
        Ok(self
            .store
            .find_symbols(identifier)?
            .into_iter()
            .filter(|symbol| {
                file.is_none_or(|suffix| {
                    symbol.file == suffix
                        || symbol.file.ends_with(suffix)
                        || Path::new(&symbol.file)
                            .file_name()
                            .is_some_and(|name| name == suffix)
                })
            })
            .collect())
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

fn read_bounded_source(path: &Path) -> Result<String> {
    match read_source_snapshot(path)? {
        SourceRead::Snapshot(source) => Ok(source),
        SourceRead::TooLarge => bail!("source exceeds the bounded read limit: {}", path.display()),
    }
}

fn is_c_header(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "h" | "hh" | "hpp" | "hxx"
            )
        })
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

fn ensure_project_directory(root: &Path, create: bool) -> Result<()> {
    let project = root.join(PROJECT_DIR);
    match project.symlink_metadata() {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "refusing unsafe project state directory {}",
                    project.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            fs::create_dir_all(&project)
                .with_context(|| format!("create project state directory {}", project.display()))?;
            let metadata = project.symlink_metadata().with_context(|| {
                format!("inspect project state directory {}", project.display())
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "refusing unsafe project state directory {}",
                    project.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "{} is not initialized; run `structurely init {}`",
                root.display(),
                root.display()
            );
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect project state directory {}", project.display()))
        }
    }
    Ok(())
}

fn writer_coordination_lock(database: &Path, exclusive: bool) -> Result<std::fs::File> {
    let parent = database
        .parent()
        .ok_or_else(|| anyhow!("graph database has no parent directory"))?;
    let recovery_lock_path = parent.join("recovery.lock");
    if recovery_lock_path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        bail!(
            "refusing unsafe recovery lock file {}",
            recovery_lock_path.display()
        );
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&recovery_lock_path)
        .with_context(|| format!("open recovery lock {}", recovery_lock_path.display()))?;
    if exclusive {
        lock.try_lock_exclusive()
            .context("cannot recover graph database while another Structurely writer is running")?;
    } else {
        FileExt::try_lock_shared(&lock)
            .context("cannot open graph database while recovery is running")?;
    }
    Ok(lock)
}

fn quarantine_database_set(database: &Path) -> Result<PathBuf> {
    let parent = database
        .parent()
        .ok_or_else(|| anyhow!("graph database has no parent directory"))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let recovery = (0usize..)
        .map(|attempt| parent.join(format!("recovery-{timestamp}-{attempt}")))
        .find(|candidate| !candidate.exists())
        .ok_or_else(|| anyhow!("unable to allocate recovery directory"))?;
    fs::create_dir(&recovery)
        .with_context(|| format!("create recovery directory {}", recovery.display()))?;

    let candidates = [
        database.to_path_buf(),
        PathBuf::from(format!("{}-wal", database.display())),
        PathBuf::from(format!("{}-shm", database.display())),
    ];
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for source in candidates.into_iter().filter(|path| path.exists()) {
        let name = source
            .file_name()
            .ok_or_else(|| anyhow!("database recovery source has no file name"))?;
        let destination = recovery.join(name);
        if let Err(error) = fs::rename(&source, &destination) {
            let mut rollback_failures = Vec::new();
            for (original, quarantined) in moved.into_iter().rev() {
                if let Err(rollback_error) = fs::rename(&quarantined, &original) {
                    rollback_failures.push(format!(
                        "{} -> {}: {rollback_error}",
                        quarantined.display(),
                        original.display()
                    ));
                }
            }
            if rollback_failures.is_empty() {
                let _ = fs::remove_dir(&recovery);
                return Err(error).with_context(|| {
                    format!(
                        "move database recovery file {} to {}",
                        source.display(),
                        destination.display()
                    )
                });
            }
            bail!(
                "move database recovery file {} to {}: {error}; partial recovery remains at {}; \
                 rollback failures: {}",
                source.display(),
                destination.display(),
                recovery.display(),
                rollback_failures.join("; ")
            );
        }
        moved.push((source, destination));
    }
    anyhow::ensure!(
        !moved.is_empty(),
        "no graph database files were available to preserve"
    );
    Ok(recovery)
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
    use crate::source::MAX_SOURCE_BYTES;

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
    fn source_only_edits_skip_identical_graph_rematerialization() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.ts");
        fs::write(
            &path,
            "function target() {}\nfunction caller() { target(); }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let before = engine.snapshot().unwrap();

        fs::write(
            &path,
            "function target() {}\nfunction caller() { target(); }\n// documentation only\n",
        )
        .unwrap();
        let report = engine.sync().unwrap();
        let after = engine.snapshot().unwrap();

        assert_eq!(report.files_changed, 1);
        assert_eq!(report.symbols_changed, 0);
        assert_eq!(report.relationships_resolved, 0);
        assert_eq!(report.resolution_ms, 0);
        assert_eq!(after.relationships, before.relationships);
        assert_eq!(
            after
                .symbols
                .iter()
                .filter(|symbol| symbol.name == "target" || symbol.name == "caller")
                .collect::<Vec<_>>(),
            before
                .symbols
                .iter()
                .filter(|symbol| symbol.name == "target" || symbol.name == "caller")
                .collect::<Vec<_>>()
        );
        assert!(after
            .symbols
            .iter()
            .zip(&before.symbols)
            .any(|(after, before)| after.end_byte > before.end_byte));
        assert_ne!(after.files[0].content_hash, before.files[0].content_hash);
    }

    #[test]
    fn path_trace_returns_the_shortest_evidence_bearing_route() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("flow.ts"),
            "function finish() {}\n\
             function middle() { finish(); }\n\
             function start() { middle(); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let trace = engine
            .trace_path_named("start", None, "finish", None, 4)
            .unwrap();

        assert_eq!(trace.status, PathTraceStatus::Found);
        assert_eq!(trace.path.len(), 2);
        assert_eq!(trace.path[0].source.name, "start");
        assert_eq!(trace.path[0].target.name, "middle");
        assert_eq!(trace.path[1].target.name, "finish");
        assert!(trace
            .path
            .iter()
            .all(|step| step.relationship == RelationshipKind::Calls
                && !step.evidence.provenance.is_empty()
                && !step.evidence.file.is_empty()));
        assert!(trace.examined_nodes >= 3);
        assert!(trace.examined_edges >= 2);
    }

    #[test]
    fn path_trace_exposes_ambiguous_endpoints_and_accepts_file_suffixes() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("one")).unwrap();
        fs::create_dir(temp.path().join("two")).unwrap();
        fs::write(
            temp.path().join("one/flow.ts"),
            "function finish() {}\nfunction start() { finish(); }\n",
        )
        .unwrap();
        fs::write(temp.path().join("two/flow.ts"), "function start() {}\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let ambiguous = engine
            .trace_path_named("start", None, "finish", None, 3)
            .unwrap();
        assert_eq!(ambiguous.status, PathTraceStatus::AmbiguousSource);
        assert_eq!(ambiguous.source_candidates.len(), 2);
        assert!(ambiguous.path.is_empty());
        assert!(ambiguous.guidance.unwrap().contains("source_file"));

        let resolved = engine
            .trace_path_named("start", Some("one/flow.ts"), "finish", None, 3)
            .unwrap();
        assert_eq!(resolved.status, PathTraceStatus::Found);
        assert_eq!(resolved.path.len(), 1);
    }

    #[test]
    fn path_trace_honors_the_depth_bound() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("flow.ts"),
            "function finish() {}\n\
             function middle() { finish(); }\n\
             function start() { middle(); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let trace = engine
            .trace_path_named("start", None, "finish", None, 1)
            .unwrap();

        assert_eq!(trace.status, PathTraceStatus::NoPath);
        assert!(trace.path.is_empty());
        assert!(trace.guidance.unwrap().contains("depth 1"));
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
    fn linked_worktrees_require_and_support_a_worktree_local_index() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".gitignore"), "worktree/\n").unwrap();
        fs::write(
            root.path().join("main.rs"),
            "pub fn main_checkout_only() {}\n",
        )
        .unwrap();
        fs::create_dir(root.path().join("worktree")).unwrap();
        fs::write(
            root.path().join("worktree/.git"),
            "gitdir: ../.git/worktrees/feature\n",
        )
        .unwrap();
        fs::write(
            root.path().join("worktree/feature.rs"),
            "pub fn feature_worktree_only() {}\n",
        )
        .unwrap();

        let (parent, _) = Engine::init(root.path()).unwrap();
        assert!(parent
            .search("feature_worktree_only", 10)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.name != "feature_worktree_only"));
        assert!(Engine::open(root.path().join("worktree")).is_err());

        let (worktree, _) = Engine::init(root.path().join("worktree")).unwrap();
        assert_eq!(
            worktree
                .search("feature_worktree_only", 10)
                .unwrap()
                .into_iter()
                .filter(|hit| hit.symbol.name == "feature_worktree_only")
                .count(),
            1
        );
        assert!(worktree
            .search("main_checkout_only", 10)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.name != "main_checkout_only"));
    }

    #[test]
    fn oversized_generated_sources_are_skipped_and_reported() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("generated.c");
        fs::write(&source, "void previously_indexed(void) {}\n").unwrap();
        let (mut engine, initial) = Engine::init(temp.path()).unwrap();
        assert_eq!(initial.files_changed, 1);
        assert_eq!(engine.search("previously_indexed", 10).unwrap().len(), 1);

        fs::write(&source, vec![b' '; MAX_SOURCE_BYTES as usize + 1]).unwrap();
        let report = engine.sync().unwrap();

        assert_eq!(report.files_scanned, 0);
        assert_eq!(report.files_skipped, 1);
        assert_eq!(report.files_deleted, 1);
        assert!(engine.search("previously_indexed", 10).unwrap().is_empty());
        let status = engine.status().unwrap();
        assert_eq!(status.indexed_files, 0);
        assert_eq!(status.pending_files, 0);
        assert_eq!(status.skipped_files, 1);

        fs::write(&source, "void indexed_again(void) {}\n").unwrap();
        let recovered = engine.sync().unwrap();
        assert_eq!(recovered.files_changed, 1);
        assert_eq!(engine.search("indexed_again", 10).unwrap().len(), 1);
    }

    #[test]
    fn invalid_utf8_bytes_do_not_abort_or_destabilize_the_index() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("legacy.ts"),
            b"// legacy byte: \xff\nexport function recovered() { return 1; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("healthy.ts"),
            "export function healthy() { return recovered(); }\n",
        )
        .unwrap();

        let (mut engine, report) = Engine::init(temp.path()).unwrap();
        assert_eq!(report.files_changed, 2);
        let recovered = engine
            .search("recovered", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "recovered")
            .unwrap()
            .symbol;
        assert_eq!(recovered.file, "legacy.ts");
        assert_eq!(recovered.start_line, 2);
        assert_eq!(engine.sync().unwrap().files_changed, 0);
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
    fn explore_accepts_the_full_public_file_budget() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let hits = engine.explore("main", ResourceBudget::MAX_RESULTS).unwrap();

        assert!(!hits.is_empty());
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
    fn semantic_search_uses_matching_file_names_as_ranking_evidence() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("atomic_file.rs"),
            "fn write_atomic() {}\nfn publish() { write_atomic(); }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("parser.rs"),
            "fn atomic_publication_parser_fallback() {}\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let hits = engine.search("atomic file publication", 10).unwrap();
        assert_eq!(hits[0].symbol.file, "atomic_file.rs");
    }

    #[test]
    fn semantic_search_prefers_the_owning_module_over_a_descriptive_test_name() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("project_config.rs"),
            "struct ProjectConfig;\nfn custom_extensions() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("engine.rs"),
            "fn project_config_controls_custom_extensions() {}\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();

        let hits = engine
            .search("project config custom extensions", 10)
            .unwrap();
        assert_eq!(hits[0].symbol.file, "project_config.rs");
    }

    #[test]
    fn graph_model_upgrade_forces_semantic_reindex() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function selected() {}\n\
             function invoke(callback: () => void) { callback(); }\n\
             function caller() { invoke(selected); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        drop(engine);
        let database = temp.path().join(PROJECT_DIR).join(DATABASE_FILE);
        let connection = rusqlite::Connection::open(database).unwrap();
        connection
            .execute(
                "UPDATE metadata SET value='48' WHERE key='graph_model_version'",
                [],
            )
            .unwrap();
        connection
            .execute(
                "UPDATE callback_argument_batches
                 SET payload='[[\"legacy\",\"invoke\",0,\"selected\",null,
                 {\"id\":\"old\",\"semantic_key\":\"old\",\"language\":\"typescript\",
                  \"kind\":\"function\",\"name\":\"old\",\"qualified_name\":\"old\",
                  \"file\":\"main.ts\",\"start_byte\":0,\"end_byte\":1,
                  \"start_line\":1,\"end_line\":1},3,42]]'",
                [],
            )
            .unwrap();
        drop(connection);

        let mut engine = Engine::open(temp.path()).unwrap();
        let report = engine.sync().unwrap();
        assert_eq!(report.files_changed, 1);
        let invoke = engine
            .search("invoke", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "invoke")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&invoke.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "selected"
                    && evidence.provenance == "dynamic/callback-argument"
            }));
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
    fn corrupt_database_set_is_preserved_and_rebuilt_before_sync() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        drop(engine);
        let database = temp.path().join(PROJECT_DIR).join(DATABASE_FILE);
        let wal = PathBuf::from(format!("{}-wal", database.display()));
        let shm = PathBuf::from(format!("{}-shm", database.display()));
        fs::write(&database, b"not a sqlite database").unwrap();
        fs::write(&wal, b"preserved wal evidence").unwrap();
        fs::write(&shm, b"preserved shm evidence").unwrap();

        let mut engine = Engine::open(temp.path()).unwrap();
        let second_writer = Engine::open(temp.path()).unwrap();
        drop(second_writer);
        let status = engine.status().unwrap();
        let recovery = status.storage_recovery.unwrap();
        let recovery = recovery
            .strip_prefix("corrupt graph database was preserved at ")
            .unwrap()
            .strip_suffix(" and rebuilt")
            .unwrap();
        let recovery = PathBuf::from(recovery);
        assert_eq!(
            fs::read(recovery.join(DATABASE_FILE)).unwrap(),
            b"not a sqlite database"
        );
        assert_eq!(
            fs::read(recovery.join(format!("{DATABASE_FILE}-wal"))).unwrap(),
            b"preserved wal evidence"
        );
        assert_eq!(
            fs::read(recovery.join(format!("{DATABASE_FILE}-shm"))).unwrap(),
            b"preserved shm evidence"
        );

        let report = engine.sync().unwrap();
        assert!(report
            .maintenance_warning
            .as_deref()
            .unwrap()
            .contains("corrupt graph database was preserved"));
        assert!(!engine.search("main", 10).unwrap().is_empty());
    }

    #[test]
    fn corrupt_content_index_is_preserved_and_rebuilt_before_sync() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("README.md"),
            "# Recovery\n\nDurable repository content.\n",
        )
        .unwrap();
        fs::write(temp.path().join("main.rs"), "fn main() {}\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        drop(engine);

        let content_database = temp.path().join(PROJECT_DIR).join("content.db");
        fs::write(&content_database, b"not a sqlite database").unwrap();

        let mut engine = Engine::open(temp.path()).unwrap();
        let status = engine.status().unwrap();
        assert!(status
            .storage_recovery
            .as_deref()
            .unwrap()
            .contains("corrupt content index was preserved"));

        let report = engine.sync().unwrap();
        assert_eq!(report.content_files_indexed, 2);
        assert!(engine
            .content_search("durable repository content", 10)
            .unwrap()
            .iter()
            .any(|hit| hit.path == "README.md"));

        let preserved = fs::read_dir(temp.path().join(PROJECT_DIR))
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("content.db"))
            .find(|path| {
                path.is_file() && fs::read(path).ok().as_deref() == Some(b"not a sqlite database")
            })
            .expect("corrupt content index should be preserved in a recovery directory");
        assert!(preserved.is_file());
    }

    #[test]
    fn corruption_recovery_refuses_to_race_an_existing_writer() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (writer, _) = Engine::init(temp.path()).unwrap();
        let database = temp.path().join(PROJECT_DIR).join(DATABASE_FILE);
        fs::write(&database, b"not a sqlite database").unwrap();

        let error = Engine::open(temp.path()).err().unwrap().to_string();

        assert!(
            error.contains("another Structurely writer is running"),
            "{error}"
        );
        assert_eq!(fs::read(&database).unwrap(), b"not a sqlite database");
        assert!(!fs::read_dir(temp.path().join(PROJECT_DIR))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_name().to_string_lossy().starts_with("recovery-")));
        drop(writer);
    }

    #[test]
    fn query_only_open_does_not_change_database_or_existing_wal_contents() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        drop(engine);
        let database = temp.path().join(PROJECT_DIR).join(DATABASE_FILE);
        let wal = PathBuf::from(format!("{}-wal", database.display()));
        let shm = PathBuf::from(format!("{}-shm", database.display()));
        let before_database = fs::read(&database).unwrap();
        let before_wal = fs::read(&wal).ok();
        let before_shm = fs::read(&shm).ok();

        let engine = Engine::open_read_only(temp.path()).unwrap();
        assert!(!engine.search("main", 10).unwrap().is_empty());
        drop(engine);

        assert_eq!(fs::read(&database).unwrap(), before_database);
        if let Some(before) = before_wal {
            assert_eq!(fs::read(&wal).unwrap(), before);
        }
        if let Some(before) = before_shm {
            assert_eq!(fs::read(&shm).unwrap(), before);
        }
    }

    #[test]
    fn query_only_open_observes_committed_live_wal_state() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function main() {}\n").unwrap();
        let (mut writer, _) = Engine::init(temp.path()).unwrap();
        writer
            .store
            .set_metadata_value("graph_epoch", "77")
            .unwrap();
        let database = temp.path().join(PROJECT_DIR).join(DATABASE_FILE);
        let wal = PathBuf::from(format!("{}-wal", database.display()));
        assert!(wal.metadata().is_ok_and(|metadata| metadata.len() > 0));

        let reader = Engine::open_read_only(temp.path()).unwrap();

        assert_eq!(reader.committed_epoch().unwrap(), 77);
    }

    #[cfg(unix)]
    #[test]
    fn initialization_rejects_symbolic_link_state_directories() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.path().join(PROJECT_DIR)).unwrap();

        let error = Engine::init(root.path()).err().unwrap().to_string();

        assert!(error.contains("refusing unsafe project state directory"));
        assert!(!outside.path().join(DATABASE_FILE).exists());
    }

    #[cfg(unix)]
    #[test]
    fn database_open_rejects_symbolic_link_sidecars() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("main.ts"), "function main() {}\n").unwrap();
        let (engine, _) = Engine::init(root.path()).unwrap();
        drop(engine);
        let database = root.path().join(PROJECT_DIR).join(DATABASE_FILE);
        let wal = PathBuf::from(format!("{}-wal", database.display()));
        if wal.exists() {
            fs::remove_file(&wal).unwrap();
        }
        let outside = root.path().join("outside-wal");
        fs::write(&outside, b"must remain untouched").unwrap();
        symlink(&outside, &wal).unwrap();

        let error = Engine::open_read_only(root.path())
            .err()
            .unwrap()
            .to_string();

        assert!(error.contains("refusing unsafe graph database file"));
        assert_eq!(fs::read(outside).unwrap(), b"must remain untouched");
    }

    #[test]
    fn injected_failure_cannot_publish_a_partial_graph_epoch() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.ts"), "function before() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let before = serde_json::to_string(&engine.snapshot().unwrap()).unwrap();
        let epoch = engine.status().unwrap().epoch;
        let fingerprint = engine
            .store
            .metadata_value("project_resolution_fingerprint")
            .unwrap();

        let replacement = parse_file("main.ts", "function after() {}\n").unwrap();
        engine
            .store
            .inject_rolled_back_publish_with_metadata(
                &[replacement],
                &[],
                &[("project_resolution_fingerprint", "must-not-commit")],
            )
            .unwrap();

        assert_eq!(engine.status().unwrap().epoch, epoch);
        assert_eq!(
            engine
                .store
                .metadata_value("project_resolution_fingerprint")
                .unwrap(),
            fingerprint
        );
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
    fn post_commit_checkpoint_failure_reports_maintenance_without_failing_publication() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("main.ts");
        fs::write(&source, "function before() {}\n").unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let epoch = engine.status().unwrap().epoch;

        fs::write(&source, "function after() {}\n").unwrap();
        engine.store.inject_checkpoint_failure_once();
        let report = engine.sync().unwrap();

        assert_eq!(report.epoch, epoch + 1);
        assert!(report
            .maintenance_warning
            .as_deref()
            .is_some_and(|warning| warning.contains("graph committed")));
        assert!(engine.search("before", 10).unwrap().is_empty());
        assert_eq!(engine.search("after", 10).unwrap().len(), 1);
    }

    #[test]
    fn empty_projects_atomically_advance_graph_metadata_without_spurious_epochs() {
        let temp = tempfile::tempdir().unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let epoch = engine.status().unwrap().epoch;
        engine
            .store
            .set_metadata_value("graph_model_version", "0")
            .unwrap();
        assert!(!engine.store.is_current_graph_model().unwrap());

        let report = engine.sync().unwrap();

        assert_eq!(report.epoch, epoch);
        assert!(engine.store.is_current_graph_model().unwrap());
        assert!(engine
            .store
            .metadata_value("project_resolution_fingerprint")
            .unwrap()
            .is_some());
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
            if kind == RelationshipKind::Imports {
                assert_eq!(relationship.evidence.provenance, "project/relative-import");
            } else {
                assert!(relationship.evidence.provenance.starts_with("tree-sitter/"));
            }
            assert!(relationship.evidence.confidence >= 0.9);
        }
    }

    #[test]
    fn external_imports_do_not_fan_out_to_same_named_project_symbols() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("consumer.ts"),
            "import { duplicate } from 'external-package';\n\
             export function run() { duplicate(); }\n",
        )
        .unwrap();
        for index in 0..12 {
            fs::write(
                temp.path().join(format!("duplicate-{index}.ts")),
                "export function duplicate() {}\n",
            )
            .unwrap();
        }

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let consumer = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == "consumer.ts")
            .unwrap();
        assert!(snapshot.relationships.iter().all(|relationship| {
            relationship.source_id != consumer.id || relationship.kind != RelationshipKind::Imports
        }));
    }

    #[test]
    fn interface_dispatch_bridges_contract_methods_to_concrete_implementations() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("notifier.ts"),
            "interface Notifier { notify(message: string): void; }\n\
             class EmailNotifier implements Notifier {\n\
               notify(message: string) { return message; }\n\
             }\n\
             function deliver(notifier: Notifier) { notifier.notify('ready'); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let contract = engine
            .search("Notifier.notify", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Notifier.notify")
            .unwrap()
            .symbol;
        let implementations = engine.callees(&contract.id).unwrap();
        assert!(implementations.iter().any(|(symbol, evidence)| {
            symbol.qualified_name == "EmailNotifier.notify"
                && evidence.provenance == "dynamic/interface-implementation"
                && evidence.confidence == 0.94
        }));

        let deliver = engine
            .search("deliver", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "deliver")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&deliver.id)
            .unwrap()
            .iter()
            .any(|(symbol, _)| symbol.qualified_name == "Notifier.notify"));
    }

    #[test]
    fn interface_dispatch_refuses_unbounded_implementation_fanout() {
        let temp = tempfile::tempdir().unwrap();
        let mut source = "interface Handler { handle(): void; }\n".to_owned();
        for index in 0..9 {
            source.push_str(&format!(
                "class Handler{index} implements Handler {{ handle() {{}} }}\n"
            ));
        }
        fs::write(temp.path().join("handlers.ts"), source).unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let contract = engine
            .search("Handler.handle", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Handler.handle")
            .unwrap()
            .symbol;
        assert!(engine.callees(&contract.id).unwrap().is_empty());
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
    fn explicit_field_annotations_disambiguate_uninitialized_receivers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("services.ts"),
            "class Desired { ping() {} pong() {} }\n\
             class Decoy { ping() {} pong() {} }\n\
             class Runner {\n\
               private dependency: Desired\n\
               run() {\n\
                 this.dependency.ping()\n\
                 let local: Desired\n\
                 local.pong()\n\
               }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let run = engine
            .search("Runner.run", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Runner.run")
            .unwrap()
            .symbol;
        let callees = engine.callees(&run.id).unwrap();
        assert_eq!(callees.len(), 2);
        assert!(callees
            .iter()
            .any(|(target, _)| target.qualified_name == "Desired.ping"));
        assert!(callees
            .iter()
            .any(|(target, _)| target.qualified_name == "Desired.pong"));
        assert!(callees.iter().all(|(target, evidence)| {
            target.qualified_name != "Decoy.ping"
                && evidence.confidence == 0.995
                && evidence.explanation.contains("receiver type")
        }));
    }

    #[test]
    fn receiver_annotations_respect_lexical_shadowing_and_sibling_functions() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("scope.ts"),
            "class Outer { ping() {} outerOnly() {} }\n\
             class Inner { ping() {} innerOnly() {} }\n\
             export function scoped() {\n\
               let value: Outer\n\
               value.outerOnly()\n\
               {\n\
                 let value: Inner\n\
                 value.innerOnly()\n\
               }\n\
               value.outerOnly()\n\
             }\n\
             export function first() {\n\
               let shared: Outer\n\
               shared.ping()\n\
             }\n\
             export function second(shared: unknown) {\n\
               shared.ping()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let caller = |name: &str| {
            engine
                .search(name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == name)
                .unwrap()
                .symbol
        };
        let scoped = engine.callees(&caller("scoped").id).unwrap();
        assert_eq!(
            scoped
                .iter()
                .filter(|(target, _)| target.qualified_name == "Outer.outerOnly")
                .count(),
            2
        );
        assert_eq!(
            scoped
                .iter()
                .filter(|(target, _)| target.qualified_name == "Inner.innerOnly")
                .count(),
            1
        );
        assert!(engine
            .callees(&caller("first").id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "Outer.ping" && evidence.confidence == 0.995
            }));
        assert!(engine
            .callees(&caller("second").id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.confidence < 0.995));
    }

    #[test]
    fn untyped_inner_bindings_shadow_outer_receiver_annotations() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("scope.ts"),
            "class Typed { ping() {} }\n\
             export function outer() {\n\
               let value: Typed\n\
               function parameterShadow(value: unknown) { value.ping() }\n\
               function destructuredShadow({ value }: { value: unknown }) { value.ping() }\n\
               {\n\
                 let value: unknown\n\
                 value.ping()\n\
               }\n\
               {\n\
                 let { value } = { value: new Typed() }\n\
                 value.ping()\n\
               }\n\
               {\n\
                 value.ping()\n\
                 let value: unknown\n\
               }\n\
               try { throw new Error() } catch (value) { value.ping() }\n\
               value.ping()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |name: &str| {
            engine
                .search(name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == name)
                .unwrap()
                .symbol
        };
        assert!(engine
            .callees(&symbol("parameterShadow").id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.confidence < 0.995));
        assert!(engine
            .callees(&symbol("destructuredShadow").id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.confidence < 0.995));
        let outer = engine.callees(&symbol("outer").id).unwrap();
        assert_eq!(
            outer
                .iter()
                .filter(|(target, evidence)| {
                    target.qualified_name == "Typed.ping" && evidence.confidence == 0.995
                })
                .count(),
            1
        );
    }

    #[test]
    fn nested_assignments_update_the_owning_receiver_binding() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("scope.ts"),
            "class First { firstOnly() {} }\n\
             class Second { secondOnly() {} }\n\
             export function changed() {\n\
               let value: First\n\
               { value = new Second() }\n\
               value.secondOnly()\n\
               value.firstOnly()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let changed = engine
            .search("changed", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "changed")
            .unwrap()
            .symbol;
        let callees = engine.callees(&changed.id).unwrap();
        assert!(callees.iter().any(|(target, evidence)| {
            target.qualified_name == "Second.secondOnly" && evidence.confidence == 0.995
        }));
        assert!(callees
            .iter()
            .all(|(target, _)| target.qualified_name != "First.firstOnly"));
    }

    #[test]
    fn const_arrow_factories_infer_local_receiver_types_without_scope_leaks() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("factory.ts"),
            "class FileWatcher { start() {} stop() {} }\n\
             class Decoy { start() {} stop() {} }\n\
             export function suite() {\n\
               const newWatcher = () => new FileWatcher()\n\
               const watcher = newWatcher()\n\
               watcher.start()\n\
               watcher.stop()\n\
             }\n\
             export function shadowed() {\n\
               const newWatcher = unknownFactory\n\
               const watcher = newWatcher()\n\
               watcher.start()\n\
             }\n\
             export function unsafeFactories(flag: boolean) {\n\
               const asyncFactory = async\t() => new FileWatcher()\n\
               const commentedAsyncFactory = async /* promise */ () => new FileWatcher()\n\
               const branchedFactory = () => {\n\
                 if (flag) { return new Decoy() }\n\
                 return new FileWatcher()\n\
               }\n\
               const asyncWatcher = asyncFactory()\n\
               const commentedAsyncWatcher = commentedAsyncFactory()\n\
               const branchedWatcher = branchedFactory()\n\
               asyncWatcher.start()\n\
               commentedAsyncWatcher.start()\n\
               branchedWatcher.start()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let suite = engine
            .search("suite", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "suite")
            .unwrap()
            .symbol;
        let callees = engine.callees(&suite.id).unwrap();
        assert!(["FileWatcher.start", "FileWatcher.stop"]
            .iter()
            .all(|qualified_name| callees.iter().any(|(target, evidence)| {
                target.qualified_name == *qualified_name && evidence.confidence == 0.995
            })));
        assert!(callees
            .iter()
            .all(|(target, _)| !target.qualified_name.starts_with("Decoy.")));
        let shadowed = engine
            .search("shadowed", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "shadowed")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&shadowed.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.confidence < 0.995));
        let unsafe_factories = engine
            .search("unsafeFactories", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "unsafeFactories")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&unsafe_factories.id)
            .unwrap()
            .iter()
            .filter(|(target, _)| target.name == "start")
            .all(|(_, evidence)| evidence.confidence < 0.995));
    }

    #[test]
    fn exact_collection_elements_type_for_of_receiver_bindings() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("collection.ts"),
            "class MCPSession { stop() {} }\n\
             class Decoy { stop() {} }\n\
             class Daemon {\n\
               private clients = new Set<MCPSession>()\n\
               private weak = new WeakSet<Decoy>()\n\
               private promised: Promise<Array<MCPSession>>\n\
               private mutable = new Set<MCPSession>()\n\
               private compound = new Set<MCPSession>()\n\
               private chained = new Set<MCPSession>().values()\n\
               reap(): void {\n\
                 for (const session of [...this.clients]) { session.stop() }\n\
               }\n\
               invalidate(): void {\n\
                 this.mutable = new Set<Decoy>()\n\
                 this.compound ??= new Set<Decoy>()\n\
               }\n\
               unsafe(): void {\n\
                 for (const key in this.clients) { key.stop() }\n\
                 for (const transformed of transform(this.clients)) { transformed.stop() }\n\
                 for (const weak of [...this.weak]) { weak.stop() }\n\
                 for (const promised of [...this.promised]) { promised.stop() }\n\
                 for (const stale of [...this.mutable]) { stale.stop() }\n\
                 for (const compound of [...this.compound]) { compound.stop() }\n\
                 for (const chained of [...this.chained]) { chained.stop() }\n\
               }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let reap = engine
            .search("Daemon.reap", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Daemon.reap")
            .unwrap()
            .symbol;
        let callees = engine.callees(&reap.id).unwrap();
        assert!(callees.iter().any(|(target, evidence)| {
            target.qualified_name == "MCPSession.stop" && evidence.confidence == 0.995
        }));
        assert!(callees
            .iter()
            .all(|(target, _)| target.qualified_name != "Decoy.stop"));
        let unsafe_method = engine
            .search("Daemon.unsafe", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Daemon.unsafe")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&unsafe_method.id)
            .unwrap()
            .iter()
            .filter(|(target, _)| target.name == "stop")
            .all(|(_, evidence)| evidence.confidence < 0.995));
    }

    #[test]
    fn receiver_annotations_do_not_escape_loop_or_catch_scopes() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("scope.ts"),
            "class Outer { outerOnly() {} }\n\
             class Inner { innerOnly() {} }\n\
             export function scoped() {\n\
               let value: Outer\n\
               for (let value: Inner; false;) {\n\
                 value.innerOnly()\n\
               }\n\
               value.outerOnly()\n\
               try { throw new Error() } catch (value) {\n\
                 let caught: Inner\n\
                 caught.innerOnly()\n\
               }\n\
               value.outerOnly()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let scoped = engine
            .search("scoped", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "scoped")
            .unwrap()
            .symbol;
        let callees = engine.callees(&scoped.id).unwrap();
        assert_eq!(
            callees
                .iter()
                .filter(|(target, _)| target.qualified_name == "Inner.innerOnly")
                .count(),
            2
        );
        assert_eq!(
            callees
                .iter()
                .filter(|(target, _)| target.qualified_name == "Outer.outerOnly")
                .count(),
            2
        );
        assert!(callees.iter().all(|(target, _)| {
            !matches!(
                target.qualified_name.as_str(),
                "Outer.innerOnly" | "Inner.outerOnly"
            )
        }));
    }

    #[test]
    fn receiver_annotations_require_one_local_or_imported_nominal_type() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("a.ts"),
            "export class Desired { ping() {} }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("b.ts"),
            "export class Desired { ping() {} }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("imported.ts"),
            "import { Desired } from './a'\n\
             export function imported() {\n\
               let value: Desired\n\
               value.ping()\n\
             }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("ambiguous.ts"),
            "export function ambiguous() {\n\
               let value: Desired\n\
               value.ping()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let caller = |name: &str| {
            engine
                .search(name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == name)
                .unwrap()
                .symbol
        };
        let imported = engine.callees(&caller("imported").id).unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].0.qualified_name, "Desired.ping");
        assert_eq!(imported[0].0.file, "a.ts");
        assert_eq!(imported[0].1.confidence, 0.995);
        assert!(engine.callees(&caller("ambiguous").id).unwrap().is_empty());
    }

    #[test]
    fn generic_receiver_annotations_use_only_the_simple_outer_nominal_type() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("workers.ts"),
            "export class LRUCache<K, V> { get(_key: K): V { throw new Error() } }\n\
             export class DecoyCache<K, V> { get(_key: K): V { throw new Error() } }\n\
             export class ParseWorkerPool<T> { submit(_value: T) {} }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("caller.ts"),
            "import { LRUCache, ParseWorkerPool } from './workers'\n\
             export function run() {\n\
               let cache: LRUCache<string, { value: number | null }> | null\n\
               let workers: ParseWorkerPool<Array<string>>\n\
               cache.get('entry')\n\
               workers.submit(['source'])\n\
             }\n\
             export function rejected(\n\
               qualified: workers.LRUCache<string, number>,\n\
               intersection: LRUCache<string, number> & ParseWorkerPool<string>,\n\
               conditional: true extends boolean ? LRUCache<string, number> : ParseWorkerPool<string>,\n\
               union: LRUCache<string, number> | ParseWorkerPool<string>\n\
             ) {\n\
               qualified.get('entry')\n\
               intersection.get('entry')\n\
               conditional.get('entry')\n\
               union.get('entry')\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let caller = |name: &str| {
            engine
                .search(name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == name)
                .unwrap()
                .symbol
        };
        let run = engine.callees(&caller("run").id).unwrap();
        assert_eq!(run.len(), 2);
        assert!(run
            .iter()
            .any(|(target, _)| target.qualified_name == "LRUCache.get"));
        assert!(run
            .iter()
            .any(|(target, _)| target.qualified_name == "ParseWorkerPool.submit"));
        assert!(run.iter().all(|(_, evidence)| evidence.confidence == 0.995));
        let rejected = engine.callees(&caller("rejected").id).unwrap();
        assert!(rejected
            .iter()
            .all(|(_, evidence)| evidence.confidence < 0.995));
    }

    #[test]
    fn exact_typed_receivers_do_not_fall_through_to_free_imported_functions() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("util.ts"), "export function send() {}\n").unwrap();
        fs::write(
            temp.path().join("caller.ts"),
            "import { send } from './util'\n\
             class Client {}\n\
             export function caller() {\n\
               let client: Client\n\
               client.send()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let caller = engine
            .search("caller", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "caller")
            .unwrap()
            .symbol;
        assert!(engine.callees(&caller.id).unwrap().is_empty());
    }

    #[test]
    fn typed_receivers_resolve_nearest_verified_inherited_methods() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("base.ts"),
            "export class Base { notify() {} }\n\
             export class Mid extends Base { notify() {} }\n\
             export class Leaf extends Mid {}\n",
        )
        .unwrap();
        let child_path = temp.path().join("child.ts");
        fs::write(
            &child_path,
            "import { Base } from './base'\n\
             export class Child extends Base {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("decoy.ts"),
            "export class Base { notify() {} }\n\
             export class Child extends Base {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("caller.ts"),
            "import { Leaf } from './base'\n\
             import { Child } from './child'\n\
             export function fromBase() {\n\
               const value: Child = new Child()\n\
               value.notify()\n\
             }\n\
             export function fromNearest() {\n\
               const value: Leaf = new Leaf()\n\
               value.notify()\n\
             }\n",
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let inherited_target = |engine: &Engine, caller: &str| {
            let source = engine
                .search(caller, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller)
                .unwrap()
                .symbol;
            engine
                .callees(&source.id)
                .unwrap()
                .into_iter()
                .find(|(_, evidence)| {
                    evidence
                        .explanation
                        .contains("nearest inherited receiver type")
                })
        };
        let base = inherited_target(&engine, "fromBase").unwrap();
        assert_eq!(base.0.qualified_name, "Base.notify");
        assert_eq!(base.0.file, "base.ts");
        assert_eq!(base.1.confidence, 0.97);
        let nearest = inherited_target(&engine, "fromNearest").unwrap();
        assert_eq!(nearest.0.qualified_name, "Mid.notify");
        assert_eq!(nearest.1.confidence, 0.97);

        fs::write(&child_path, "export class Child {}\n").unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(inherited_target(&engine, "fromBase").is_none());
        assert_eq!(
            inherited_target(&engine, "fromNearest")
                .unwrap()
                .0
                .qualified_name,
            "Mid.notify"
        );
    }

    #[test]
    fn explicit_return_types_resolve_immediate_call_result_receivers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("client.ts"),
            "export class Client { send() {} }\n\
             export class Other { send() {} }\n",
        )
        .unwrap();
        let factory = temp.path().join("factory.ts");
        let factory_source = |first_return: &str| {
            format!(
                "import {{ Client, Other }} from './client'\n\
                 export function makeClient(): {first_return} {{ return new Client() }}\n\
                 export function makeOther(): Other {{ return new Other() }}\n\
                 export function inferred() {{ return new Client() }}\n\
                 export function promised(): Promise<Client> {{ return Promise.resolve(new Client()) }}\n\
                 export function unioned(): Client | Other {{ return new Client() }}\n"
            )
        };
        fs::write(&factory, factory_source("Client")).unwrap();
        let caller = temp.path().join("caller.ts");
        let source =
            "import { makeClient, makeOther, inferred, promised, unioned } from './factory'\n\
                      export function run() {\n\
                        makeClient().send(); makeOther().send();\n\
                        inferred().send(); promised().send(); unioned().send();\n\
                      }\n";
        fs::write(&caller, source).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let run = engine
            .search("run", 20)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "run")
            .unwrap()
            .symbol;
        let result_edges = |engine: &Engine, caller_id: &str| {
            engine
                .callees(caller_id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.explanation.contains("receiver resolves from"))
                .collect::<Vec<_>>()
        };
        let edges = result_edges(&engine, &run.id);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().any(|(target, evidence)| {
            target.qualified_name == "Client.send"
                && evidence.explanation.contains("makeClient")
                && evidence.explanation.contains("explicit return annotation")
        }));
        assert!(edges.iter().any(|(target, evidence)| {
            target.qualified_name == "Other.send" && evidence.explanation.contains("makeOther")
        }));
        assert!(edges
            .iter()
            .all(|(_, evidence)| evidence.confidence >= 0.97));

        let moved = format!("// comment-only position edit\n{source}");
        fs::write(&caller, &moved).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(result_edges(&engine, &run.id).len(), 2);

        fs::write(&factory, factory_source("Other")).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let retargeted = result_edges(&engine, &run.id);
        assert_eq!(
            retargeted
                .iter()
                .filter(|(target, _)| target.qualified_name == "Other.send")
                .count(),
            1
        );
        assert!(retargeted
            .iter()
            .all(|(target, _)| target.qualified_name != "Client.send"));

        let clean_temp = tempfile::tempdir().unwrap();
        fs::write(
            clean_temp.path().join("client.ts"),
            "export class Client { send() {} }\n\
             export class Other { send() {} }\n",
        )
        .unwrap();
        fs::write(
            clean_temp.path().join("factory.ts"),
            factory_source("Other"),
        )
        .unwrap();
        fs::write(clean_temp.path().join("caller.ts"), &moved).unwrap();
        let (clean, _) = Engine::init(clean_temp.path()).unwrap();
        assert_eq!(
            serde_json::to_string(&engine.snapshot().unwrap()).unwrap(),
            serde_json::to_string(&clean.snapshot().unwrap()).unwrap()
        );

        fs::remove_file(&factory).unwrap();
        assert_eq!(engine.sync().unwrap().files_deleted, 1);
        assert!(result_edges(&engine, &run.id).is_empty());
    }

    #[test]
    fn arkts_imported_singleton_return_type_resolves_project_methods() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("InputHandler.ets"),
            "export class InputHandler {\n\
               static getInstance(): InputHandler { return new InputHandler() }\n\
               insertText(value: string): void {}\n\
             }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("KeyItem.ets"),
            "import { InputHandler } from './InputHandler'\n\
             export function tap(): void {\n\
               InputHandler.getInstance().insertText('a')\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let tap = engine
            .search("tap", 20)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "tap")
            .unwrap()
            .symbol;
        let callees = engine.callees(&tap.id).unwrap();
        assert!(
            callees.iter().any(|(target, evidence)| {
                target.qualified_name == "InputHandler.insertText"
                    && evidence.confidence == 0.97
                    && evidence
                        .explanation
                        .contains("InputHandler.getInstance's explicit return annotation")
                    && evidence
                        .explanation
                        .contains("factory resolved through imported package")
            }),
            "{callees:#?}"
        );
    }

    #[test]
    fn call_result_resolution_is_fail_closed_for_ambiguous_generators_and_deep_chains() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("types.ts"),
            "export class Client { send() {} }\n\
             export class Unique { next(): Client { return new Client() } }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("factory.ts"),
            "import { Client, Unique } from './types'\n\
             interface Client { send(): void }\n\
             export function ambiguousType(): Client { throw new Error() }\n\
             export function* generated(): Client { yield new Client() }\n\
             export function make(): Unique { return new Unique() }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("caller.ts"),
            "import { ambiguousType, generated, make } from './factory'\n\
             export function run() {\n\
               ambiguousType().send()\n\
               generated().send()\n\
               make().next().send()\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let run = engine
            .search("run", 20)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "run")
            .unwrap()
            .symbol;
        let callees = engine.callees(&run.id).unwrap();
        assert!(callees.iter().any(|(target, evidence)| {
            target.qualified_name == "Unique.next"
                && evidence.explanation.contains("explicit return annotation")
        }));
        assert!(callees.iter().all(|(target, evidence)| {
            target.qualified_name != "Client.send"
                || !evidence.explanation.contains("explicit return annotation")
        }));
    }

    #[test]
    fn call_result_evidence_survives_accepted_and_fallback_inline_callbacks() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("client.ts"),
            "export class Client { send() {} }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("factory.ts"),
            "import { Client } from './client'\n\
             export function makeClient(): Client { return new Client() }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("caller.ts"),
            "import { makeClient } from './factory'\n\
             function invoke(callback: () => void) { callback() }\n\
             function never(callback: () => void) {}\n\
             export function caller() {\n\
               invoke(() => makeClient().send())\n\
               never(() => makeClient().send())\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callback = engine
            .search("<callback invoke argument 1 #1>", 20)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "<callback invoke argument 1 #1>")
            .unwrap()
            .symbol;
        let caller = engine
            .search("caller", 20)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "caller")
            .unwrap()
            .symbol;
        for source in [&callback, &caller] {
            let edges = engine
                .callees(&source.id)
                .unwrap()
                .into_iter()
                .filter(|(target, evidence)| {
                    target.qualified_name == "Client.send"
                        && evidence.explanation.contains("explicit return annotation")
                })
                .collect::<Vec<_>>();
            assert_eq!(edges.len(), 1, "source={} edges={edges:#?}", source.name);
            assert_eq!(edges[0].1.confidence, 0.97);
            assert!(edges[0]
                .1
                .explanation
                .contains("factory resolved through explicit import scope"));
        }
    }

    #[test]
    fn arkts_typed_fields_disambiguate_this_member_calls() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Page.ets"),
            "class UserService { save() {} }\n\
             class AuditService { save() {} }\n\
             @Component\n\
             struct Page {\n\
               private service: UserService = new UserService()\n\
               persist() { this.service.save() }\n\
               build() { Button('save').onClick(() => this.persist()) }\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let persist = engine
            .search("Page.persist", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Page.persist")
            .unwrap()
            .symbol;
        let callees = engine.callees(&persist.id).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].0.qualified_name, "UserService.save");
        assert_eq!(callees[0].1.confidence, 0.995);
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
    fn cargo_workspace_imports_constrain_cross_crate_calls() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("crates/core-lib/src")).unwrap();
        fs::create_dir_all(temp.path().join("crates/app/src")).unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("crates/core-lib/Cargo.toml"),
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("crates/app/Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("crates/core-lib/src/lib.rs"),
            "pub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("crates/app/src/decoy.rs"),
            "pub fn helper() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("crates/app/src/main.rs"),
            "use core_lib::helper;\nfn caller() { helper(); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callees = engine.callees_named("caller", None, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol.file, "crates/core-lib/src/lib.rs");
        assert_eq!(callees[0].evidence.confidence, 0.97);
        assert!(engine.snapshot().unwrap().relationships.iter().any(|edge| {
            edge.kind == RelationshipKind::Imports
                && edge.evidence.provenance == "cargo/workspace"
                && edge.target_id == callees[0].symbol.id
        }));
    }

    #[test]
    fn go_workspace_import_aliases_constrain_cross_module_calls() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("modules/core")).unwrap();
        fs::create_dir_all(temp.path().join("modules/decoy")).unwrap();
        fs::create_dir_all(temp.path().join("apps/main")).unwrap();
        fs::write(
            temp.path().join("go.work"),
            "go 1.24\nuse (\n ./modules/core\n ./modules/decoy\n ./apps/main\n)\n",
        )
        .unwrap();
        for (directory, module) in [
            ("modules/core", "example.com/core"),
            ("modules/decoy", "example.com/decoy"),
            ("apps/main", "example.com/app"),
        ] {
            fs::write(
                temp.path().join(directory).join("go.mod"),
                format!("module {module}\n\ngo 1.24\n"),
            )
            .unwrap();
        }
        fs::write(
            temp.path().join("modules/core/helper.go"),
            "package core\nfunc Execute() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("modules/decoy/helper.go"),
            "package decoy\nfunc Execute() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("apps/main/main.go"),
            "package main\n\
             import core \"example.com/core\"\n\
             func Caller() { core.Execute() }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callees = engine.callees_named("Caller", None, 10).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].symbol.file, "modules/core/helper.go");
        assert_eq!(callees[0].evidence.provenance, "go/workspace");
        assert_eq!(callees[0].evidence.confidence, 0.995);
        assert!(callees[0].evidence.explanation.contains("imported package"));
    }

    #[test]
    fn dart_methods_own_sibling_bodies_and_resolve_calls() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("service.dart"),
            "class Service {\n\
               void run() { helper(); }\n\
             }\n\
             void helper() {}\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let run = engine
            .search("run", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "run")
            .unwrap()
            .symbol;
        assert_eq!(run.language, crate::model::Language::Dart);
        assert_eq!(run.qualified_name, "Service.run");
        let callees = engine.callees(&run.id).unwrap();
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].0.name, "helper");
        assert_eq!(callees[0].0.language, crate::model::Language::Dart);
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
    fn express_routes_are_symbols_wired_to_every_named_middleware_and_handler() {
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
        assert_eq!(callees.len(), 2);
        assert_eq!(
            callees
                .iter()
                .map(|(symbol, _)| symbol.name.as_str())
                .collect::<Vec<_>>(),
            ["authorize", "showUser"]
        );
        assert!(callees.iter().all(|(symbol, evidence)| {
            symbol.file == "handlers.ts"
                && evidence.provenance == "framework/express-route"
                && evidence.confidence == 0.97
        }));
    }

    #[test]
    fn duplicate_express_routes_get_unique_stable_semantic_keys() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("routes.js"),
            "function first() {}\n\
             function second() {}\n\
             app.get('/same', first);\n\
             app.get('/same', second);\n",
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let initial = engine
            .search("GET /same", 10)
            .unwrap()
            .into_iter()
            .filter(|hit| hit.symbol.kind == crate::model::SymbolKind::Route)
            .map(|hit| hit.symbol.id)
            .collect::<Vec<_>>();
        assert_eq!(initial.len(), 2);
        assert_ne!(initial[0], initial[1]);

        engine.sync().unwrap();
        let unchanged = engine
            .search("GET /same", 10)
            .unwrap()
            .into_iter()
            .filter(|hit| hit.symbol.kind == crate::model::SymbolKind::Route)
            .map(|hit| hit.symbol.id)
            .collect::<Vec<_>>();
        assert_eq!(initial, unchanged);
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
    fn direct_callback_arguments_propagate_by_exact_parameter_position() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function selected() {}\n\
             function decoy() {}\n\
             function invoke(value: number, callback: () => void) { callback(); }\n\
             function caller() { invoke(1, selected); }\n",
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
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 1);
        assert_eq!(callbacks[0].0.name, "selected");
        assert_eq!(callbacks[0].1.confidence, 0.96);
        assert_eq!(callbacks[0].1.line, 4);
    }

    #[test]
    fn outer_callback_parameters_invoked_from_nested_closures_propagate() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function selected() {}\n\
             function invoke(callback: () => void) {\n\
               [1].forEach((value: number) => callback())\n\
             }\n\
             function caller() { invoke(selected) }\n",
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
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 1);
        assert_eq!(callbacks[0].0.name, "selected");
        assert_eq!(callbacks[0].1.line, 5);
    }

    #[test]
    fn callback_arguments_resolve_verified_imported_callables() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("callbacks.ts"),
            "export function selected() {}\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "import { selected } from './callbacks'\n\
             function invoke(callback: () => void) { callback() }\n\
             export function caller() { invoke(selected) }\n",
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
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 1);
        assert_eq!(callbacks[0].0.file, "callbacks.ts");
        assert_eq!(callbacks[0].0.qualified_name, "selected");
    }

    #[test]
    fn callback_argument_members_require_exact_owner_and_unique_callee() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "class Invoker { invoke(callback: () => void) { callback(); } }\n\
             class Page {\n\
               selected() {}\n\
               run() { new Invoker().invoke(this.selected); }\n\
             }\n\
             class Other { selected() {} }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let invoke = engine
            .search("invoke", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Invoker.invoke")
            .unwrap()
            .symbol;
        let run = engine
            .search("run", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Page.run")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&run.id)
            .unwrap()
            .iter()
            .any(|(target, _)| target.qualified_name == "Invoker.invoke"));
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 1);
        assert_eq!(callbacks[0].0.qualified_name, "Page.selected");
    }

    #[test]
    fn arkts_callback_argument_fields_resolve_through_typed_receivers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ets"),
            "class ResourceManager {\n\
               getAppIconWithCache(id: number, callback: Function) { callback(id) }\n\
             }\n\
             @Component\n\
             struct DeleteDialog {\n\
               private manager: ResourceManager = new ResourceManager()\n\
               iconLoadCallback = (image: number): void => {}\n\
               updateIcon(): void {\n\
                 this.manager.getAppIconWithCache(1, this.iconLoadCallback)\n\
               }\n\
               build() { Column() }\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let invoke = engine
            .search("ResourceManager.getAppIconWithCache", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "ResourceManager.getAppIconWithCache")
            .unwrap()
            .symbol;
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 1);
        assert_eq!(
            callbacks[0].0.qualified_name,
            "DeleteDialog.iconLoadCallback"
        );
        assert_eq!(callbacks[0].1.line, 9);
    }

    #[test]
    fn nullable_typed_fields_preserve_inline_callback_flows() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ets"),
            "class SocketImpl {\n\
               setOnCloseListener(callback: () => void): void { callback() }\n\
             }\n\
             class Decoy {\n\
               setOnCloseListener(callback: () => void): void { callback() }\n\
             }\n\
             class Model {\n\
               private socket: SocketImpl | null = null\n\
               run(): void {\n\
                 if (this.socket == null) { return }\n\
                 this.socket.setOnCloseListener(() => { console.info('closed') })\n\
               }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let run = engine
            .search("Model.run", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Model.run")
            .unwrap()
            .symbol;
        let callees = engine.callees(&run.id).unwrap();
        assert!(callees.iter().any(|(target, evidence)| {
            target.qualified_name == "SocketImpl.setOnCloseListener" && evidence.confidence == 0.995
        }));
        assert!(callees
            .iter()
            .all(|(target, _)| target.qualified_name != "Decoy.setOnCloseListener"));
        let callback = engine
            .search("<callback setOnCloseListener argument 1 #1>", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "<callback setOnCloseListener argument 1 #1>")
            .unwrap()
            .symbol;
        assert!(engine
            .callees_named("SocketImpl.setOnCloseListener", None, 10)
            .unwrap()
            .iter()
            .any(|hit| {
                hit.symbol.id == callback.id
                    && hit.evidence.provenance == "dynamic/callback-argument"
            }));
    }

    #[test]
    fn harmony_imported_singletons_prefer_the_unique_project_callback_callee() {
        let temp = tempfile::tempdir().unwrap();
        let page = temp.path().join("Demo/entry/src/main/ets/Page.ets");
        let local = temp
            .path()
            .join("Demo/features/download/src/main/ets/Request.ets");
        let other = temp.path().join("Other/entry/src/main/ets/Request.ets");
        fs::create_dir_all(page.parent().unwrap()).unwrap();
        fs::create_dir_all(local.parent().unwrap()).unwrap();
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(
            &page,
            "import { requestDownload } from '@ohos/download'\n\
             @Component\n\
             struct Page {\n\
               callback = (): void => {}\n\
               run(): void { requestDownload.downloadFile(this.callback) }\n\
               build() { Column() }\n\
             }\n",
        )
        .unwrap();
        fs::write(
            &local,
            "class RequestDownload {\n\
               downloadFile(callback: () => void): void { callback() }\n\
             }\n",
        )
        .unwrap();
        fs::write(
            &other,
            "class RequestDownload {\n\
               downloadFile(callback: () => void): void { callback() }\n\
             }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let local_invoke = engine
            .search("RequestDownload.downloadFile", 20)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.file == "Demo/features/download/src/main/ets/Request.ets")
            .unwrap()
            .symbol;
        let callbacks = |engine: &Engine| {
            engine
                .callees(&local_invoke.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
                .collect::<Vec<_>>()
        };
        assert_eq!(callbacks(&engine).len(), 1);
        assert_eq!(callbacks(&engine)[0].0.qualified_name, "Page.callback");

        let duplicate = temp
            .path()
            .join("Demo/features/duplicate/src/main/ets/Request.ets");
        fs::create_dir_all(duplicate.parent().unwrap()).unwrap();
        fs::write(
            &duplicate,
            "class RequestDownload {\n\
               downloadFile(callback: () => void): void { callback() }\n\
             }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(callbacks(&engine).is_empty());
    }

    #[test]
    fn callback_argument_propagation_fails_closed_for_unsafe_shapes_and_ambiguity() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function selected() {}\n\
             function callback() {}\n\
             function invoke(callback: () => void) { callback(); }\n\
             function never(callback: () => void) {}\n\
             function defaulted(callback = selected) { callback(); }\n\
             function forwarding(callback: () => void) { invoke(callback); }\n\
             function shadowed(callback: () => void) {\n\
               [1].forEach((callback: () => void) => callback())\n\
             }\n\
             function caller() {\n\
               never(selected);\n\
               defaulted(selected);\n\
               shadowed(selected);\n\
             }\n\
             class First { ambiguous(callback: () => void) { callback(); } }\n\
             class Second { ambiguous(callback: () => void) { callback(); } }\n\
             function unknown(value: any) { value.ambiguous(selected); }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        for name in ["invoke", "never", "defaulted", "shadowed", "ambiguous"] {
            for symbol in engine
                .search(name, 20)
                .unwrap()
                .into_iter()
                .filter(|hit| hit.symbol.name == name)
            {
                assert!(engine
                    .callees(&symbol.symbol.id)
                    .unwrap()
                    .iter()
                    .all(|(_, evidence)| evidence.provenance != "dynamic/callback-argument"));
            }
        }
    }

    #[test]
    fn callback_argument_relationships_are_removed_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.ts");
        fs::write(
            &path,
            "function selected() {}\n\
             function invoke(callback: () => void) { callback(); }\n\
             function caller() { invoke(selected); }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let invoke = engine
            .search("invoke", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "invoke")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&invoke.id)
            .unwrap()
            .iter()
            .any(|(_, evidence)| evidence.provenance == "dynamic/callback-argument"));

        fs::write(
            &path,
            "function selected() {}\n\
             function invoke(callback: () => void) { callback(); }\n\
             function caller() {}\n",
        )
        .unwrap();
        engine.sync().unwrap();
        assert!(engine
            .callees(&invoke.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "dynamic/callback-argument"));
    }

    #[test]
    fn input_device_stored_callback_lifecycle_propagates_and_removes_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("InputDeviceUtil.ets");
        let source = "class InputDevice {\n\
               private callback: Function | null = null\n\
               registerChange(callback: Function): void {\n\
                 this.callback = callback\n\
               }\n\
               unregisterChange(): void {\n\
                 if (this.callback !== null) {\n\
                   this.callback([])\n\
                   this.callback = null\n\
                 }\n\
               }\n\
               onChange(): void {\n\
                 if (this.callback) { this.callback([]) }\n\
               }\n\
             }\n\
             function selected(devices: object[]): void {}\n\
             function start(): void {\n\
               const device: InputDevice = new InputDevice()\n\
               device.registerChange(selected)\n\
             }\n";
        fs::write(&path, source).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let register = engine
            .search("InputDevice.registerChange", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "InputDevice.registerChange")
            .unwrap()
            .symbol;
        let propagated = |engine: &Engine| {
            engine
                .callees(&register.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
                .collect::<Vec<_>>()
        };
        assert_eq!(propagated(&engine).len(), 1);
        assert_eq!(propagated(&engine)[0].0.qualified_name, "selected");

        fs::write(&path, source.replace("this.callback([])", "void 0")).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(propagated(&engine).is_empty());
    }

    #[test]
    fn inline_callback_arguments_materialize_stable_callable_flows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.ts");
        let initial = "function selected() {}\n\
                       function other() {}\n\
                       function invoke(/* formal */ callback: () => void) { callback(); }\n\
                       function invokeSecond(value: number, callback: () => void) { callback(); }\n\
                       function never(callback: () => void) {}\n\
                       class First { ambiguous(callback: () => void) { callback(); } }\n\
                       class Second { ambiguous(callback: () => void) { callback(); } }\n\
                       function unknown(value: any) { value.ambiguous(() => selected()); }\n\
                       function caller() { invoke(/* registration */ () => selected()); invoke(function () { other(); }); invokeSecond(1, /* between */ () => selected()); never(() => selected()); }\n";
        fs::write(&path, initial).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |engine: &Engine, qualified_name: &str| {
            engine
                .search(qualified_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol
        };
        let invoke = symbol(&engine, "invoke");
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 2);
        assert!(callbacks
            .iter()
            .all(|(_, evidence)| { evidence.confidence == 0.96 && evidence.line == 9 }));
        let arrow = callbacks
            .iter()
            .find(|(target, _)| target.name == "<callback invoke argument 1 #1>")
            .map(|(target, _)| target.clone())
            .unwrap();
        let function = callbacks
            .iter()
            .find(|(target, _)| target.name == "<callback invoke argument 1 #2>")
            .map(|(target, _)| target.clone())
            .unwrap();
        let invoke_second = symbol(&engine, "invokeSecond");
        let second_argument = engine
            .callees(&invoke_second.id)
            .unwrap()
            .into_iter()
            .find(|(target, evidence)| {
                target.name == "<callback invokeSecond argument 2 #1>"
                    && evidence.provenance == "dynamic/callback-argument"
            })
            .map(|(target, _)| target)
            .unwrap();
        assert!(engine
            .callees(&arrow.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "selected"
                    && evidence.provenance == "tree-sitter/name-resolution"
            }));
        assert!(engine
            .callees(&function.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "other"
                    && evidence.provenance == "tree-sitter/name-resolution"
            }));
        let caller = symbol(&engine, "caller");
        assert_eq!(
            engine
                .snapshot()
                .unwrap()
                .relationships
                .into_iter()
                .filter(|relationship| {
                    relationship.source_id == caller.id
                        && relationship.kind == RelationshipKind::Contains
                        && relationship.evidence.provenance == "dynamic/callback-inline"
                })
                .count(),
            3
        );
        assert!(engine
            .search("<callback never argument 1 #1>", 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.name != "<callback never argument 1 #1>"));
        assert!(engine
            .search("<callback ambiguous argument 1 #1>", 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.name != "<callback ambiguous argument 1 #1>"));
        assert!(engine
            .callees(&caller.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "selected"
                    && evidence.provenance == "tree-sitter/name-resolution"
            }));
        let unknown = symbol(&engine, "unknown");
        assert!(engine
            .callees(&unknown.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "selected"
                    && evidence.provenance == "tree-sitter/name-resolution"
            }));

        let comment_changed = initial
            .replace("/* registration */", "/* registration changed */")
            .replace("/* between */", "/* between changed */");
        fs::write(&path, &comment_changed).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(symbol(&engine, &arrow.qualified_name).id, arrow.id);
        assert_eq!(
            symbol(&engine, &second_argument.qualified_name).id,
            second_argument.id
        );

        let moved = format!("// position-only edit\n{comment_changed}");
        fs::write(&path, &moved).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(symbol(&engine, &arrow.qualified_name).id, arrow.id);
        assert_eq!(symbol(&engine, &function.qualified_name).id, function.id);

        let body_changed = moved.replace(
            "invoke(/* registration changed */ () => selected())",
            "invoke(/* registration changed */ () => { selected(); other(); })",
        );
        fs::write(&path, &body_changed).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let stable_arrow = symbol(&engine, &arrow.qualified_name);
        assert_eq!(stable_arrow.id, arrow.id);
        let body_callees = engine.callees(&stable_arrow.id).unwrap();
        assert!(body_callees
            .iter()
            .any(|(target, _)| target.qualified_name == "selected"));
        assert!(body_callees
            .iter()
            .any(|(target, _)| target.qualified_name == "other"));

        let removed = "function selected() {}\n\
                       function invoke(callback: () => void) { callback(); }\n\
                       function caller() {}\n";
        let before_rollback = serde_json::to_string(&engine.snapshot().unwrap()).unwrap();
        let replacement = parse_file("main.ts", removed).unwrap();
        engine
            .store
            .inject_rolled_back_publish(&[replacement], &[])
            .unwrap();
        assert_eq!(
            serde_json::to_string(&engine.snapshot().unwrap()).unwrap(),
            before_rollback
        );
        assert_eq!(symbol(&engine, &arrow.qualified_name).id, arrow.id);

        fs::write(&path, removed).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(engine
            .search(&arrow.qualified_name, 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.id != arrow.id));
        assert!(engine
            .search(&function.qualified_name, 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.id != function.id));
        assert!(engine
            .search(&second_argument.qualified_name, 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.id != second_argument.id));
    }

    #[test]
    fn python_keyword_inline_callbacks_resolve_exact_formals_and_keep_stable_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.py");
        let initial = "def selected():\n    pass\n\ndef invoke(value, callback=None, *, on_done=None):\n    callback()\n    on_done()\n\ndef caller():\n    invoke(value=1, callback=lambda: selected(), on_done=lambda: selected())\n";
        fs::write(&path, initial).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let invoke = engine
            .search("invoke", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "invoke")
            .unwrap()
            .symbol;
        let callback_targets = |engine: &Engine| {
            engine
                .callees(&invoke.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
                .map(|(target, _)| (target.name, target.id))
                .collect::<Vec<_>>()
        };
        let before = callback_targets(&engine);
        assert_eq!(before.len(), 2, "{before:?}");
        assert!(before
            .iter()
            .any(|(name, _)| name.contains("keyword callback")));
        assert!(before
            .iter()
            .any(|(name, _)| name.contains("keyword on_done")));

        fs::write(
            &path,
            initial.replace(
                "value=1, callback=lambda: selected(), on_done=lambda: selected()",
                "on_done=lambda: selected(), value=2, callback=lambda: selected()",
            ),
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let mut before_ids = before.into_iter().map(|(_, id)| id).collect::<Vec<_>>();
        let mut after_ids = callback_targets(&engine)
            .into_iter()
            .map(|(_, id)| id)
            .collect::<Vec<_>>();
        before_ids.sort();
        after_ids.sort();
        assert_eq!(after_ids, before_ids);
    }

    #[test]
    fn python_keyword_callbacks_require_unique_callee_and_exact_formal_name() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.py"),
            "def invoke(callback):\n\
                 callback()\n\
             def invoke(callback):\n\
                 callback()\n\
             def caller():\n\
                 invoke(callback=lambda: None)\n\
                 invoke(missing=lambda: None)\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        for invoke in engine
            .search("invoke", 10)
            .unwrap()
            .into_iter()
            .filter(|hit| hit.symbol.qualified_name == "invoke")
            .map(|hit| hit.symbol)
        {
            assert!(engine
                .callees(&invoke.id)
                .unwrap()
                .iter()
                .all(|(_, evidence)| evidence.provenance != "dynamic/callback-argument"));
        }
    }

    #[test]
    fn python_positional_only_callbacks_resolve_by_exact_positional_index() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.py"),
            "def selected():\n    pass\n\ndef invoke(callback, /):\n    callback()\n\ndef caller():\n    invoke(lambda: selected())\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let invoke = engine
            .search("invoke", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "invoke")
            .unwrap()
            .symbol;
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 1);
        assert_eq!(callbacks[0].0.name, "<callback invoke argument 1 #1>");
        assert_eq!(callbacks[0].1.confidence, 0.96);
    }

    #[test]
    fn python_positional_lambda_callbacks_are_exact_fail_closed_and_incremental() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.py");
        let initial = "def selected():\n    pass\n\
                       \n\
                       def invoke(value, callback):\n    callback()\n\
                       \n\
                       def caller(values):\n    invoke(1, lambda: selected())\n    invoke(*values, lambda: selected())\n    invoke(unknown=lambda: selected(), value=1)\n";
        fs::write(&path, initial).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |engine: &Engine, qualified_name: &str| {
            engine
                .search(qualified_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol
        };
        let invoke = symbol(&engine, "invoke");
        let callbacks = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .collect::<Vec<_>>();
        assert_eq!(callbacks.len(), 1);
        assert_eq!(callbacks[0].0.name, "<callback invoke argument 2 #1>");
        assert_eq!(callbacks[0].1.confidence, 0.96);
        let callback = callbacks[0].0.clone();
        assert!(engine
            .callees(&callback.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "selected"
                    && evidence.provenance == "tree-sitter/name-resolution"
            }));

        fs::write(
            &path,
            initial.replace("invoke(1, lambda: selected())", "selected()"),
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(engine
            .search("<callback invoke argument 2 #1>", 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.id != callback.id));
        assert!(engine
            .callees(&invoke.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "dynamic/callback-argument"));
    }

    #[test]
    fn inline_callbacks_resolve_nested_and_delegated_flows_to_a_bounded_fixed_point() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.ts");
        let source = "function selected() {}\n\
                      function invoke(callback: () => void) { callback(); }\n\
                      function leaf(callback: () => void) { callback(); }\n\
                      function outer(callback: () => void) { leaf(callback); }\n\
                      function caller() { invoke(() => invoke(() => selected())); outer(() => selected()); }\n";
        fs::write(&path, source).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |engine: &Engine, qualified_name: &str| {
            engine
                .search(qualified_name, 50)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol
        };
        let caller = symbol(&engine, "caller");
        let invoke = symbol(&engine, "invoke");
        let leaf = symbol(&engine, "leaf");
        let snapshot = engine.snapshot().unwrap();
        let inline_relationships = snapshot
            .relationships
            .iter()
            .filter(|relationship| {
                relationship.kind == RelationshipKind::Contains
                    && relationship.evidence.provenance == "dynamic/callback-inline"
            })
            .collect::<Vec<_>>();
        assert_eq!(inline_relationships.len(), 3);
        let outer_nested_id = inline_relationships
            .iter()
            .find(|relationship| {
                relationship.source_id == caller.id
                    && snapshot.symbols.iter().any(|symbol| {
                        symbol.id == relationship.target_id
                            && symbol.name == "<callback invoke argument 1 #1>"
                    })
            })
            .map(|relationship| relationship.target_id.clone())
            .unwrap();
        let inner_nested_id = inline_relationships
            .iter()
            .find(|relationship| relationship.source_id == outer_nested_id)
            .map(|relationship| relationship.target_id.clone())
            .unwrap();
        assert!(engine
            .callees(&invoke.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.id == inner_nested_id && evidence.provenance == "dynamic/callback-argument"
            }));
        assert!(engine
            .callees(&outer_nested_id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.id == invoke.id && evidence.provenance == "tree-sitter/name-resolution"
            }));
        assert!(engine
            .callees(&inner_nested_id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "selected"
                    && evidence.provenance == "tree-sitter/name-resolution"
            }));
        let delegated = engine
            .callees(&leaf.id)
            .unwrap()
            .into_iter()
            .find(|(_, evidence)| evidence.provenance == "dynamic/callback-delegation")
            .unwrap();
        assert_eq!(delegated.1.confidence, 0.94);
        assert!(engine
            .callees(&delegated.0.id)
            .unwrap()
            .iter()
            .any(|(target, _)| target.qualified_name == "selected"));

        let moved = format!("// stable edit\n{source}");
        fs::write(&path, &moved).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let incremental = serde_json::to_string(&engine.snapshot().unwrap()).unwrap();
        let clean_temp = tempfile::tempdir().unwrap();
        fs::write(clean_temp.path().join("main.ts"), &moved).unwrap();
        let (clean, _) = Engine::init(clean_temp.path()).unwrap();
        assert_eq!(
            incremental,
            serde_json::to_string(&clean.snapshot().unwrap()).unwrap()
        );

        fs::remove_file(&path).unwrap();
        assert_eq!(engine.sync().unwrap().files_deleted, 1);
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| !symbol.name.starts_with("<callback ")));
        assert!(engine
            .search("<callback", 50)
            .unwrap()
            .into_iter()
            .all(|hit| !hit.symbol.name.starts_with("<callback ")));
    }

    #[test]
    fn inline_callback_nesting_stops_at_the_materialization_depth_cap() {
        let temp = tempfile::tempdir().unwrap();
        let mut expression = "selected()".to_owned();
        for _ in 0..17 {
            expression = format!("invoke(() => {expression})");
        }
        fs::write(
            temp.path().join("main.ts"),
            format!(
                "function selected() {{}}\n\
                 function invoke(callback: () => void) {{ callback(); }}\n\
                 function caller() {{ {expression}; }}\n"
            ),
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        assert_eq!(
            snapshot
                .relationships
                .iter()
                .filter(|relationship| {
                    relationship.kind == RelationshipKind::Contains
                        && relationship.evidence.provenance == "dynamic/callback-inline"
                })
                .count(),
            16
        );
        assert_eq!(
            snapshot
                .symbols
                .iter()
                .filter(|symbol| symbol.name.starts_with("<callback invoke argument"))
                .count(),
            16
        );
    }

    #[test]
    fn callback_facts_inside_rejected_inline_callbacks_do_not_leak() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("main.ts"),
            "function selected() {}\n\
             function other() {}\n\
             function invoke(callback: () => void) { callback(); }\n\
             function never(callback: () => void) {}\n\
             function caller() {\n\
               never(() => invoke(selected));\n\
               never(() => invoke(() => other()));\n\
               invoke(() => invoke(selected));\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let invoke = engine
            .search("invoke", 20)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "invoke")
            .unwrap()
            .symbol;
        let callback_targets = engine
            .callees(&invoke.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-argument")
            .map(|(target, _)| target)
            .collect::<Vec<_>>();
        assert_eq!(
            callback_targets
                .iter()
                .filter(|target| target.qualified_name == "selected")
                .count(),
            1
        );
        assert_eq!(
            callback_targets
                .iter()
                .filter(|target| target.name.starts_with("<callback invoke argument"))
                .count(),
            1
        );
        assert!(engine
            .search("<callback never", 20)
            .unwrap()
            .into_iter()
            .all(|hit| !hit.symbol.name.starts_with("<callback never")));
    }

    #[test]
    fn callback_delegation_is_exact_bounded_and_incremental() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.ts");
        let source = "function selected() {}\n\
                      function other() {}\n\
                      function callback() {}\n\
                      function leaf(decoy: () => void, callback: () => void) { callback(); }\n\
                      function middle(callback: () => void) { leaf(other, callback); }\n\
                      function outer(callback: () => void) { middle(callback); }\n\
                      function mutated(callback: () => void) { callback = other; leaf(other, callback); }\n\
                      function nestedMutated(callback: () => void) { (() => { callback = other; })(); leaf(other, callback); }\n\
                      function captured(callback: () => void) { [1].forEach(() => leaf(other, callback)); }\n\
                      function shadowContainer() { [1].forEach((callback: () => void) => leaf(other, callback)); }\n\
                      function defaultForward(callback = other) { leaf(other, callback); }\n\
                      function restForward(...callback: Array<() => void>) { leaf(other, callback); }\n\
                      function left(callback: () => void) { leaf(other, callback); }\n\
                      function right(callback: () => void) { leaf(other, callback); }\n\
                      function diamond(callback: () => void) { left(callback); right(callback); }\n\
                      function cycleA(callback: () => void) { cycleB(callback); }\n\
                      function cycleB(callback: () => void) { cycleA(callback); }\n\
                      class AmbiguousA { forward(callback: () => void) { leaf(other, callback); } }\n\
                      class AmbiguousB { forward(callback: () => void) { leaf(other, callback); } }\n\
                      function ambiguousForward(value: any, callback: () => void) { value.forward(callback); }\n\
                      class First { invoke(callback: () => void) { callback(); } }\n\
                      class Second { invoke(callback: () => void) { callback(); } }\n\
                      function caller() { outer(selected); mutated(selected); nestedMutated(selected); captured(other); shadowContainer(); defaultForward(selected); restForward(selected); diamond(selected); cycleA(selected); ambiguousForward(new AmbiguousA(), other); new First().invoke(selected); new Second().invoke(other); }\n";
        fs::write(&path, source).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |engine: &Engine, qualified_name: &str| {
            engine
                .search(qualified_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol
        };
        let delegated = |engine: &Engine, qualified_name: &str| {
            let owner = symbol(engine, qualified_name);
            engine
                .callees(&owner.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    matches!(
                        evidence.provenance.as_str(),
                        "dynamic/callback-argument" | "dynamic/callback-delegation"
                    )
                })
                .collect::<Vec<_>>()
        };

        let leaf = delegated(&engine, "leaf");
        assert_eq!(leaf.len(), 1);
        assert_eq!(leaf[0].0.qualified_name, "selected");
        assert_eq!(leaf[0].1.provenance, "dynamic/callback-delegation");
        assert_eq!(leaf[0].1.confidence, 0.94);
        assert!(leaf[0].1.explanation.contains("leaf"));
        assert!(delegated(&engine, "middle").is_empty());
        assert!(delegated(&engine, "outer").is_empty());
        assert!(delegated(&engine, "mutated").is_empty());
        assert!(delegated(&engine, "nestedMutated").is_empty());
        assert!(delegated(&engine, "captured").is_empty());
        assert!(delegated(&engine, "shadowContainer").is_empty());
        assert!(delegated(&engine, "defaultForward").is_empty());
        assert!(delegated(&engine, "restForward").is_empty());
        assert!(delegated(&engine, "left").is_empty());
        assert!(delegated(&engine, "right").is_empty());
        assert!(delegated(&engine, "diamond").is_empty());
        assert!(delegated(&engine, "cycleA").is_empty());
        assert!(delegated(&engine, "cycleB").is_empty());
        assert!(delegated(&engine, "ambiguousForward").is_empty());

        let first = delegated(&engine, "First.invoke");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0.qualified_name, "selected");
        let second = delegated(&engine, "Second.invoke");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0.qualified_name, "other");

        let replacement_source = source.replacen("callback();", "void callback;", 1);
        let before_rollback = serde_json::to_string(&engine.snapshot().unwrap()).unwrap();
        let replacement = parse_file("main.ts", &replacement_source).unwrap();
        engine
            .store
            .inject_rolled_back_publish(&[replacement], &[])
            .unwrap();
        assert_eq!(
            serde_json::to_string(&engine.snapshot().unwrap()).unwrap(),
            before_rollback
        );
        assert_eq!(delegated(&engine, "leaf").len(), 1);

        fs::write(&path, replacement_source).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(delegated(&engine, "leaf").is_empty());
        assert_eq!(delegated(&engine, "First.invoke").len(), 1);
        assert_eq!(delegated(&engine, "Second.invoke").len(), 1);
    }

    #[test]
    fn callback_delegation_bounds_branching_and_depth() {
        let temp = tempfile::tempdir().unwrap();
        let mut source = "function selected() {}\n\
                          function other() {}\n\
                          function leafOk(callback: () => void) { callback(); }\n\
                          function leafOverflow(callback: () => void) { callback(); }\n"
            .to_owned();
        for index in (0..15).rev() {
            let next = if index == 14 {
                "leafOk".to_owned()
            } else {
                format!("ok{}", index + 1)
            };
            source.push_str(&format!(
                "function ok{index}(callback: () => void) {{ {next}(callback); }}\n"
            ));
        }
        for index in (0..16).rev() {
            let next = if index == 15 {
                "leafOverflow".to_owned()
            } else {
                format!("overflow{}", index + 1)
            };
            source.push_str(&format!(
                "function overflow{index}(callback: () => void) {{ {next}(callback); }}\n"
            ));
        }
        for layer in (0..6).rev() {
            for branch in 0..6 {
                let calls = if layer == 5 {
                    "leafOk(callback);".to_owned()
                } else {
                    (0..6)
                        .map(|next| format!("wide{}_{}(callback);", layer + 1, next))
                        .collect::<Vec<_>>()
                        .join(" ")
                };
                source.push_str(&format!(
                    "function wide{layer}_{branch}(callback: () => void) {{ {calls} }}\n"
                ));
            }
        }
        source.push_str(
            "function wideRoot(callback: () => void) { \
               wide0_0(callback); wide0_1(callback); wide0_2(callback); \
               wide0_3(callback); wide0_4(callback); wide0_5(callback); \
             }\n\
             function caller() { ok0(selected); overflow0(other); wideRoot(selected); }\n",
        );
        fs::write(temp.path().join("main.ts"), source).unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let callbacks = |qualified_name: &str| {
            let owner = engine
                .search(qualified_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol;
            engine
                .callees(&owner.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "dynamic/callback-delegation")
                .collect::<Vec<_>>()
        };
        let accepted = callbacks("leafOk");
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].0.qualified_name, "selected");
        assert!(callbacks("leafOverflow").is_empty());
    }

    #[test]
    fn call_resolution_refuses_unbounded_ambiguous_fanout() {
        let temp = tempfile::tempdir().unwrap();
        let candidates = "def shared():\n    return 1\n".repeat(7);
        fs::write(
            temp.path().join("caller.py"),
            format!("{candidates}\ndef dispatch():\n    shared()\n"),
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
        assert!(engine.callees(&dispatch.id).unwrap().is_empty());
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
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             router = APIRouter(prefix='/created')\n\
             app.include_router(router)\n\n\
             @app.get('/users/{user_id}')\n\
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
        assert!(routes.iter().any(|route| route.name == "POST /created"));
        for (route_name, handler_name) in [
            ("GET /users/{user_id}", "show_user"),
            ("POST /created", "create_user"),
        ] {
            let route = routes
                .iter()
                .find(|route| route.name == route_name)
                .unwrap();
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].0.name, handler_name);
            assert_eq!(callees[0].1.provenance, "framework/fastapi-route");
            assert_eq!(callees[0].1.confidence, 0.995);
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
    fn django_paths_and_drf_viewsets_create_evidence_bearing_routes() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("views.py"),
            "def home(request):\n    return None\n\
             class ArticleView:\n    @classmethod\n    def as_view(cls):\n        return cls\n\
             class ArticleViewSet:\n    pass\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("urls.py"),
            "from views import home, ArticleView, ArticleViewSet\n\
             urlpatterns = [\n\
                 path('home/', home, name='home'),\n\
                 re_path(r'^articles/$', ArticleView.as_view()),\n\
             ]\n\
             router.register(r'articles', ArticleViewSet)\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let cases = [
            ("ROUTE /home/", "home"),
            ("ROUTE /^articles/$", "ArticleView"),
            ("VIEWSET /articles", "ArticleViewSet"),
        ];
        for (route_name, handler_name) in cases {
            let route = engine
                .search(route_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| {
                    hit.symbol.kind == crate::model::SymbolKind::Route
                        && hit.symbol.name == route_name
                })
                .unwrap()
                .symbol;
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].0.name, handler_name);
            assert_eq!(callees[0].1.provenance, "framework/django-route");
            assert_eq!(callees[0].1.confidence, 0.97);
        }
    }

    #[test]
    fn django_adapter_rejects_dynamic_paths_non_callables_and_lookalike_registers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("urls.py"),
            "def home(request):\n    return None\n\
             prefix = 'dynamic/'\n\
             path(prefix, home)\n\
             path('string-handler/', 'home')\n\
             registry.register('articles', ArticleViewSet)\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        assert!(engine
            .search("ROUTE", 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.kind != crate::model::SymbolKind::Route));
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
    fn react_runtime_bridges_set_state_to_render_and_render_to_jsx_child() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("child.tsx"),
            "export function Child() { return <span>ready</span>; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app.tsx"),
            "import React from 'react';\n\
             import { Child } from './child';\n\
             export class App extends React.Component {\n\
               update() { this.setState({ ready: true }); }\n\
               render() { return <Child />; }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let update = engine
            .search("App.update", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "App.update")
            .unwrap()
            .symbol;
        let update_callees = engine.callees(&update.id).unwrap();
        assert!(update_callees.iter().any(|(symbol, evidence)| {
            symbol.qualified_name == "App.render"
                && evidence.provenance == "framework/react-render"
                && evidence.confidence == 0.98
        }));

        let render = engine
            .search("App.render", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "App.render")
            .unwrap()
            .symbol;
        let render_callees = engine.callees(&render.id).unwrap();
        assert!(render_callees.iter().any(|(symbol, evidence)| {
            symbol.name == "Child"
                && symbol.file == "child.tsx"
                && evidence.provenance == "framework/jsx-render"
                && evidence.confidence == 0.96
        }));
    }

    #[test]
    fn react_runtime_rejects_non_react_set_state_and_intrinsic_jsx() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("store.tsx"),
            "class Store {\n\
               setState(value: object) {}\n\
               update() { this.setState({ ready: true }); }\n\
               render() { return <section />; }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let update = engine
            .search("Store.update", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Store.update")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&update.id)
            .unwrap()
            .iter()
            .all(
                |(_, evidence)| evidence.provenance != "framework/react-render"
                    && evidence.provenance != "framework/jsx-render"
            ));
    }

    #[test]
    fn vue_and_svelte_templates_link_events_and_child_components() {
        for (extension, app_source, child_source, handler, provenance) in [
            (
                "vue",
                "<script setup lang=\"ts\">\n\
                 import Child from './Child.vue'\n\
                 function save() {}\n\
                 </script>\n\
                 <template><Child @click=\"save\" /></template>\n",
                "<script setup>export function childLogic() {}</script>\n\
                 <template><span /></template>\n",
                "save",
                "framework/vue-template",
            ),
            (
                "svelte",
                "<script lang=\"ts\">\n\
                 import Child from './Child.svelte';\n\
                 function handleReady() {}\n\
                 </script>\n\
                 <Child on:ready={handleReady} />\n",
                "<script>export function childLogic() {}</script>\n<span />\n",
                "handleReady",
                "framework/svelte-template",
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join(format!("App.{extension}")), app_source).unwrap();
            fs::write(temp.path().join(format!("Child.{extension}")), child_source).unwrap();

            let (engine, _) = Engine::init(temp.path()).unwrap();
            let app = engine
                .snapshot()
                .unwrap()
                .symbols
                .into_iter()
                .find(|symbol| {
                    symbol.kind == crate::model::SymbolKind::Component && symbol.name == "App"
                })
                .unwrap();
            let callees = engine.callees(&app.id).unwrap();
            assert!(callees.iter().any(|(symbol, evidence)| {
                symbol.kind == crate::model::SymbolKind::Component
                    && symbol.name == "Child"
                    && evidence.provenance == provenance
            }));
            assert!(callees.iter().any(|(symbol, evidence)| {
                symbol.name == handler && evidence.provenance == provenance
            }));
        }
    }

    #[test]
    fn astro_components_routes_and_imported_templates_are_exact_and_incremental() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("src/pages/blog")).unwrap();
        fs::create_dir_all(temp.path().join("src/components")).unwrap();
        fs::create_dir_all(temp.path().join("src/utils")).unwrap();
        fs::write(
            temp.path().join("src/components/Layout.astro"),
            "<main><slot /></main>\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("src/components/Scripture.astro"),
            "<article><slot /></article>\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("src/components/ImportedCard.astro"),
            "<article>card</article>\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("src/utils/format.ts"),
            "export default function formatDate(value: string) { return value; }\n",
        )
        .unwrap();
        let page = temp.path().join("src/pages/blog/[id].astro");
        let original = "---\n\
             import Layout from '../../components/Layout.astro';\n\
             import Scripture from '../../components/Scripture.astro';\n\
             import ImportedCard from '../../components/ImportedCard.astro';\n\
             import formatDate from '../../utils/format';\n\
             const decoy = '<Layout />';\n\
             ---\n\
             <script>const hidden = '<Layout />';</script>\n\
             <style>.fake::after { content: '</styleguide>'; } /* <ImportedCard /> */</style>\n\
             <!-- <Layout /> -->\n\
             <Layout><p>story</p></Layout>\n\
             <Scripture />\n\
             <Ghost />\n\
             <UI.Card />\n\
             <{Layout} />\n\
             <p>{formatDate('2026-07-29')}</p>\n\
             <p>{formatDate(\n\
               '2026-07-30'\n\
             )}</p>\n\
             <Fragment><Code /><Debug /></Fragment>\n";
        fs::write(&page, original).unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let page_component = snapshot
            .symbols
            .iter()
            .find(|symbol| {
                symbol.file == "src/pages/blog/[id].astro"
                    && symbol.kind == crate::model::SymbolKind::Component
            })
            .unwrap();
        assert_eq!(page_component.name, "[id]");
        let route = snapshot
            .symbols
            .iter()
            .find(|symbol| {
                symbol.file == "src/pages/blog/[id].astro"
                    && symbol.kind == crate::model::SymbolKind::Route
            })
            .unwrap();
        assert_eq!(route.name, "/blog/:id");
        assert!(engine
            .callees(&route.id)
            .unwrap()
            .iter()
            .any(|(symbol, evidence)| {
                symbol.id == page_component.id
                    && evidence.provenance == "framework/astro-route"
                    && evidence.confidence == 1.0
            }));
        let template_callees = engine.callees(&page_component.id).unwrap();
        assert_eq!(
            template_callees
                .iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/astro-template")
                .count(),
            2
        );
        assert!(template_callees.iter().any(|(symbol, evidence)| {
            symbol.file == "src/components/Layout.astro"
                && symbol.kind == crate::model::SymbolKind::Component
                && evidence.provenance == "framework/astro-template"
                && evidence.confidence == 0.99
        }));
        assert!(template_callees.iter().any(|(symbol, evidence)| {
            symbol.file == "src/components/Scripture.astro"
                && symbol.kind == crate::model::SymbolKind::Component
                && evidence.provenance == "framework/astro-template"
        }));
        assert!(template_callees.iter().all(|(symbol, evidence)| {
            symbol.file != "src/components/ImportedCard.astro"
                || evidence.provenance != "framework/astro-template"
        }));
        assert_eq!(
            template_callees
                .iter()
                .filter(|(symbol, evidence)| {
                    symbol.file == "src/utils/format.ts"
                        && symbol.name == "formatDate"
                        && evidence.provenance == "framework/astro-template-expression"
                        && evidence.confidence == 0.97
                })
                .count(),
            1,
            "{template_callees:#?}"
        );

        fs::write(
            &page,
            original.replace("<Layout><p>story</p></Layout>", "<p>story</p>"),
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let changed_component = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| {
                symbol.file == "src/pages/blog/[id].astro"
                    && symbol.kind == crate::model::SymbolKind::Component
            })
            .unwrap();
        assert_eq!(changed_component.id, page_component.id);
        let changed_callees = engine.callees(&changed_component.id).unwrap();
        assert!(changed_callees.iter().all(|(symbol, evidence)| {
            symbol.file != "src/components/Layout.astro"
                || evidence.provenance != "framework/astro-template"
        }));
        assert!(changed_callees.iter().any(|(symbol, evidence)| {
            symbol.file == "src/components/Scripture.astro"
                && evidence.provenance == "framework/astro-template"
        }));

        fs::write(&page, original).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let incremental = serde_json::to_string(&engine.snapshot().unwrap()).unwrap();
        let (fresh, _) = Engine::init(temp.path()).unwrap();
        assert_eq!(
            incremental,
            serde_json::to_string(&fresh.snapshot().unwrap()).unwrap()
        );
    }

    #[test]
    fn astro_routes_and_template_bindings_fail_closed_on_malformed_or_ambiguous_input() {
        let temp = tempfile::tempdir().unwrap();
        for relative in [
            "src/pages/index.astro",
            "src/pages/docs/[...slug].astro",
            "src/pages/index/ordinary.astro",
            "src/pages/[...rest]/child.astro",
            "src/pages/[...one]/[...two].astro",
            "src/pages/_private.astro",
            "src/pages/site.config.astro",
            "src/pages/über.astro",
            "src/pages/bad/[broken.astro",
            "pages/outside.astro",
        ] {
            let path = temp.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "<p>page</p>\n").unwrap();
        }
        fs::create_dir_all(temp.path().join("src/components")).unwrap();
        fs::write(temp.path().join("src/components/One.astro"), "<p>one</p>\n").unwrap();
        fs::write(temp.path().join("src/components/Two.astro"), "<p>two</p>\n").unwrap();
        fs::write(
            temp.path().join("src/pages/ambiguous.astro"),
            "---\n\
             import Card from '../components/One.astro';\n\
             import Card from '../components/Two.astro';\n\
             ---\n\
             <Card />\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("src/components/SocialIcons.astro"),
            "---\n\
             import Default from '@astrojs/starlight/components/SocialIcons.astro';\n\
             ---\n\
             <Default />\n",
        )
        .unwrap();
        fs::write(temp.path().join("Card.astro"), "<p>root card</p>\n").unwrap();
        fs::write(
            temp.path().join("src/components/Escape.astro"),
            "---\n\
             import Card from '../../../Card.astro';\n\
             ---\n\
             <Card />\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<std::collections::HashSet<_>>();
        assert!(routes.contains("/"));
        assert!(routes.contains("/docs/*slug"));
        assert!(routes.contains("/index/ordinary"));
        assert!(routes.contains("/*rest/child"));
        assert!(routes.contains("/ambiguous"));
        assert!(routes.contains("/site.config"));
        assert!(routes.contains("/über"));
        assert_eq!(routes.len(), 7);

        let ambiguous = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| {
                symbol.file == "src/pages/ambiguous.astro"
                    && symbol.kind == crate::model::SymbolKind::Component
            })
            .unwrap();
        assert!(engine
            .callees(&ambiguous.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "framework/astro-template"));
        let social_icons = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| {
                symbol.file == "src/components/SocialIcons.astro"
                    && symbol.kind == crate::model::SymbolKind::Component
            })
            .unwrap();
        assert!(engine
            .callees(&social_icons.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "framework/astro-template"));
        let escaping = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| {
                symbol.file == "src/components/Escape.astro"
                    && symbol.kind == crate::model::SymbolKind::Component
            })
            .unwrap();
        let escaping_callees = engine.callees(&escaping.id).unwrap();
        assert!(
            escaping_callees
                .iter()
                .all(|(_, evidence)| evidence.provenance != "framework/astro-template"),
            "{escaping_callees:#?}"
        );
    }

    #[test]
    fn arkui_components_link_state_rebuilds_children_and_event_handlers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Index.ets"),
            "@Entry\n\
             @Component\n\
             struct Counter {\n\
               @State count: number = 0\n\
               increment() { this.count++ }\n\
               build() {\n\
                 Column() {\n\
                   TodoRow()\n\
                   Button('Add').onClick(this.increment)\n\
                 }\n\
               }\n\
             }\n\
             @Component\n\
             struct TodoRow { build() { Text('row') } }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |qualified_name: &str| {
            engine
                .search(qualified_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol
        };
        let increment = symbol("Counter.increment");
        assert!(engine
            .callees(&increment.id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "Counter.build"
                    && evidence.provenance == "framework/arkui-state"
                    && evidence.confidence == 0.95
            }));

        let build = symbol("Counter.build");
        let callees = engine.callees(&build.id).unwrap();
        assert!(callees.iter().any(|(target, evidence)| {
            target.name == "TodoRow"
                && evidence.provenance == "framework/arkui-render"
                && evidence.confidence == 0.97
        }));
        assert!(callees.iter().any(|(target, evidence)| {
            target.qualified_name == "Counter.increment"
                && evidence.provenance == "framework/arkui-event"
                && evidence.confidence == 0.97
        }));
    }

    #[test]
    fn arkui_state_bridge_requires_component_and_reactive_mutation() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Decoy.ets"),
            "struct NotAComponent {\n\
               value: number = 0\n\
               change() { this.value++ }\n\
               build() { Text('x') }\n\
             }\n\
             @Component\n\
             struct ReadOnly {\n\
               @State value: number = 0\n\
               inspect() { return this.value }\n\
               build() { Text(`${this.value}`) }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for qualified_name in ["NotAComponent.change", "ReadOnly.inspect"] {
            let method = engine
                .search(qualified_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol;
            assert!(engine
                .callees(&method.id)
                .unwrap()
                .iter()
                .all(|(_, evidence)| evidence.provenance != "framework/arkui-state"));
        }
    }

    #[test]
    fn arkui_inference_bounds_decorators_intrinsics_events_and_nested_state() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Adversarial.ets"),
            "@Component\n\
             struct Decorated {\n\
               @State value: number = 0\n\
               change() { this.value++ }\n\
               build() { Text('decorated') }\n\
             }\n\
             struct PlainSibling {\n\
               @State value: number = 0\n\
               change() { this.value++ }\n\
               build() { Text('plain') }\n\
             }\n\
             @ComponentV2\n\
             struct Modern {\n\
               @Local value: number = 0\n\
               change() { this.value++ }\n\
               deferred() { const later = () => { this.value++ } }\n\
               shadowed() { let value = 0; value++ }\n\
               fieldHandler: () => void = () => {}\n\
               handle() {}\n\
               build() {\n\
                 Button('ok').onClick(this.handle)\n\
                 Button('field').onClick(this.fieldHandler)\n\
                 Button('no').onward(this.handle)\n\
               }\n\
             }\n\
             function Text() { return 'project function' }\n\
             export function callProjectText() { return Text() }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |qualified_name: &str| {
            engine
                .search(qualified_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol
        };
        assert!(engine
            .callees(&symbol("Modern.change").id)
            .unwrap()
            .iter()
            .any(|(target, evidence)| {
                target.qualified_name == "Modern.build"
                    && evidence.provenance == "framework/arkui-state"
            }));
        for method in ["PlainSibling.change", "Modern.deferred", "Modern.shadowed"] {
            assert!(engine
                .callees(&symbol(method).id)
                .unwrap()
                .iter()
                .all(|(_, evidence)| evidence.provenance != "framework/arkui-state"));
        }
        let build_edges = engine.callees(&symbol("Modern.build").id).unwrap();
        assert_eq!(
            build_edges
                .iter()
                .filter(|(target, evidence)| {
                    target.qualified_name == "Modern.handle"
                        && evidence.provenance == "framework/arkui-event"
                })
                .count(),
            1
        );
        assert!(build_edges.iter().any(|(target, evidence)| {
            target.qualified_name == "Modern.fieldHandler"
                && evidence.provenance == "framework/arkui-event"
                && evidence.confidence == 0.97
        }));
        assert!(engine
            .callees(&symbol("callProjectText").id)
            .unwrap()
            .iter()
            .any(|(target, _)| target.name == "Text"));
    }

    #[test]
    fn arkts_and_typescript_imports_resolve_in_both_directions() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("entry/pages")).unwrap();
        fs::write(
            temp.path().join("entry/logic.ts"),
            "export function formatLabel() { return 'ready'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("entry/pages/Index.ets"),
            "import { formatLabel } from '../logic'\n\
             export function arkHelper() { return formatLabel(); }\n\
             @Component\n\
             struct Index { build() { Text(formatLabel()) } }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("entry/consumer.ts"),
            "import { arkHelper } from './pages/Index.ets';\n\
             export function consume() { return arkHelper(); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for (caller_name, target_name, target_file) in [
            ("Index.build", "formatLabel", "entry/logic.ts"),
            ("consume", "arkHelper", "entry/pages/Index.ets"),
        ] {
            let caller = engine
                .search(caller_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            assert!(engine
                .callees(&caller.id)
                .unwrap()
                .iter()
                .any(|(target, _)| target.name == target_name && target.file == target_file));
        }
    }

    #[test]
    fn arkts_ohpm_file_dependencies_constrain_bare_imports() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("entry/src/main/ets/pages")).unwrap();
        fs::create_dir_all(temp.path().join("features/data")).unwrap();
        fs::write(
            temp.path().join("entry/oh-package.json5"),
            r#"{"dependencies":{"data":"file:../features/data"}}"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("features/data/oh-package.json5"),
            r#"{"name":"data","main":"Index.ets"}"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("features/data/Index.ets"),
            "export function loadData() { return 'local'; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("entry/src/main/ets/pages/Home.ets"),
            "import { loadData } from 'data'\n\
             @Component\n\
             struct Home { build() { Text(loadData()) } }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("decoy.ets"),
            "export function loadData() { return 'wrong'; }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let build = engine
            .search("Home.build", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "Home.build")
            .unwrap()
            .symbol;
        let edges = engine.callees(&build.id).unwrap();
        assert!(edges.iter().any(|(target, evidence)| {
            target.file == "features/data/Index.ets"
                && target.name == "loadData"
                && evidence.confidence == 0.97
                && evidence.explanation.contains("explicit import scope")
        }));
        assert!(edges
            .iter()
            .all(|(target, _)| target.file != "decoy.ets" || target.name != "loadData"));
        assert!(engine.snapshot().unwrap().relationships.iter().any(|edge| {
            edge.kind == RelationshipKind::Imports
                && edge.evidence.provenance == "harmony/ohpm"
                && edge.target_id
                    == engine
                        .search("loadData", 10)
                        .unwrap()
                        .into_iter()
                        .find(|hit| hit.symbol.file == "features/data/Index.ets")
                        .unwrap()
                        .symbol
                        .id
        }));
    }

    #[test]
    fn arkui_member_handlers_use_file_and_receiver_hints_together() {
        let temp = tempfile::tempdir().unwrap();
        for directory in ["first", "second"] {
            fs::create_dir(temp.path().join(directory)).unwrap();
            fs::write(
                temp.path().join(directory).join("Panel.ets"),
                "@Component\n\
                 struct Panel {\n\
                   handler: () => void = () => {}\n\
                   build() { Button('go').onClick(this.handler) }\n\
                 }\n",
            )
            .unwrap();
        }

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let build = engine
            .search("Panel.build", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.file == "first/Panel.ets")
            .unwrap()
            .symbol;
        let edges = engine.callees(&build.id).unwrap();
        assert_eq!(
            edges
                .iter()
                .filter(|(target, evidence)| {
                    target.qualified_name == "Panel.handler"
                        && evidence.provenance == "framework/arkui-event"
                })
                .count(),
            1
        );
        assert!(edges.iter().any(|(target, evidence)| {
            target.file == "first/Panel.ets"
                && target.qualified_name == "Panel.handler"
                && evidence.confidence == 0.97
        }));
    }

    #[test]
    fn arkui_literal_routes_resolve_only_to_same_module_entry_pages() {
        let temp = tempfile::tempdir().unwrap();
        for module in ["first", "second"] {
            fs::create_dir_all(temp.path().join(format!("{module}/src/main/ets/pages"))).unwrap();
            fs::write(
                temp.path()
                    .join(format!("{module}/src/main/ets/pages/Detail.ets")),
                format!(
                    "@Entry\n@Component\nstruct {module}Detail {{ build() {{ Text('detail') }} }}\n"
                ),
            )
            .unwrap();
        }
        fs::write(
            temp.path().join("first/src/main/ets/pages/Plain.ets"),
            "@Component\nstruct Plain { build() { Text('not a page') } }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("first/src/main/ets/pages/Home.ets"),
            "import router from '@ohos.router'\n\
             @Entry\n\
             @Component\n\
             struct Home {\n\
               openDetail() { router.pushUrl({ url: 'pages/Detail' }) }\n\
               replaceDetail() { router.replaceUrl({ url: 'pages/Detail' }) }\n\
               dynamic(url: string) { router.pushUrl({ url }) }\n\
               openPlain() { router.pushUrl({ url: 'pages/Plain' }) }\n\
               escape() { router.pushUrl({ url: '../second/src/main/ets/pages/Detail' }) }\n\
               build() { Text('home') }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for caller_name in ["Home.openDetail", "Home.replaceDetail"] {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            let routes = engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/arkui-route")
                .collect::<Vec<_>>();
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].0.qualified_name, "firstDetail");
            assert_eq!(routes[0].0.file, "first/src/main/ets/pages/Detail.ets");
            assert_eq!(routes[0].1.confidence, 0.97);
        }
        for caller_name in ["Home.dynamic", "Home.openPlain", "Home.escape"] {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            assert!(engine
                .callees(&caller.id)
                .unwrap()
                .iter()
                .all(|(_, evidence)| evidence.provenance != "framework/arkui-route"));
        }
    }

    #[test]
    fn arkui_routes_cover_import_aliases_utilities_and_normalized_literals() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("entry/src/main/ets/pages")).unwrap();
        fs::create_dir_all(temp.path().join("entry/src/main/ets/utils")).unwrap();
        fs::write(
            temp.path().join("entry/src/main/ets/pages/Detail.ets"),
            "@Entry\n@Component\nstruct Detail { build() { Text('detail') } }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("entry/src/main/ets/utils/navigation.ets"),
            "import nav from '@ohos.router'\n\
             export function openDetail() { nav.pushUrl({ url: './pages/Detail.ets' }) }\n\
             export function replaceDetail() { nav.replaceUrl({ url: '/pages/Detail' }) }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for caller_name in ["openDetail", "replaceDetail"] {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            let routes = engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/arkui-route")
                .collect::<Vec<_>>();
            assert_eq!(routes.len(), 1);
            assert_eq!(routes[0].0.qualified_name, "Detail");
            assert_eq!(routes[0].0.file, "entry/src/main/ets/pages/Detail.ets");
            assert_eq!(routes[0].1.confidence, 0.97);
            assert!(routes[0].1.explanation.contains("exact ArkUI entry page"));
        }
    }

    #[test]
    fn arkui_routes_require_verified_unshadowed_router_bindings() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("entry/src/main/ets/pages")).unwrap();
        fs::write(
            temp.path().join("entry/src/main/ets/pages/Detail.ets"),
            "@Entry\n@Component\nstruct Detail { build() { Text('detail') } }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("entry/src/main/ets/pages/Safe.ets"),
            "import nav from '@ohos.router'\n\
             export function safe() { nav.pushUrl({ url: 'pages/Detail' }) }\n\
             export function parameter(nav: LocalRouter) { nav.pushUrl({ url: 'pages/Detail' }) }\n\
             export function local() { const nav = fakeRouter; nav.pushUrl({ url: 'pages/Detail' }) }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("entry/src/main/ets/pages/Lookalike.ets"),
            "export function lookalike() { router.pushUrl({ url: 'pages/Detail' }) }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for (caller_name, expected) in [
            ("safe", 1),
            ("parameter", 0),
            ("local", 0),
            ("lookalike", 0),
        ] {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            assert_eq!(
                engine
                    .callees(&caller.id)
                    .unwrap()
                    .iter()
                    .filter(|(_, evidence)| evidence.provenance == "framework/arkui-route")
                    .count(),
                expected
            );
        }
    }

    #[test]
    fn arkui_routes_fail_closed_on_ambiguous_entries_and_clean_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("entry/src/main/ets/pages")).unwrap();
        let target = temp.path().join("entry/src/main/ets/pages/Detail.ets");
        let caller_file = temp.path().join("entry/src/main/ets/pages/Home.ets");
        fs::write(
            &target,
            "@Entry\n@Component\nstruct First { build() {} }\n\
             @Entry\n@Component\nstruct Second { build() {} }\n",
        )
        .unwrap();
        fs::write(
            &caller_file,
            "import router from '@ohos.router'\n\
             export function openDetail() { router.pushUrl({ url: 'pages/Detail' }) }\n",
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let route_count = |engine: &Engine| {
            let caller = engine
                .search("openDetail", 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == "openDetail")
                .unwrap()
                .symbol;
            engine
                .callees(&caller.id)
                .unwrap()
                .iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/arkui-route")
                .count()
        };
        assert_eq!(route_count(&engine), 0);

        fs::write(
            &target,
            "@Entry\n@Component\nstruct Detail { build() { Text('detail') } }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(route_count(&engine), 1);

        fs::write(
            &target,
            "@Component\nstruct Detail { build() { Text('plain') } }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(route_count(&engine), 0);

        fs::write(
            &target,
            "@Entry\n@Component\nstruct Detail { build() { Text('detail') } }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(route_count(&engine), 1);

        fs::remove_file(&target).unwrap();
        assert_eq!(engine.sync().unwrap().files_deleted, 1);
        assert_eq!(route_count(&engine), 0);

        fs::write(
            &caller_file,
            "import router from '@ohos.router'\n\
             export function openDetail() { return 'closed' }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(route_count(&engine), 0);
    }

    #[test]
    fn arkui_decorated_style_helpers_resolve_exact_chains_and_clean_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("Styles.ets");
        fs::write(
            &source,
            "@Extend(Text)\n\
             function textStyle() { .fontSize(16) }\n\
             @Extend(Button)\n\
             function buttonOnly() { .width(100) }\n\
             function undecorated() {}\n\
             @Component\n\
             struct Card {\n\
               @Styles cardStyle() { .height(40) }\n\
               @Styles wrongOwner() { .width(20) }\n\
               build() {\n\
                 Text('ok').textStyle()\n\
                 Text('wrong').buttonOnly()\n\
                 Text('plain').undecorated()\n\
                 Column() { Text('card') }.cardStyle()\n\
               }\n\
             }\n\
             @Component\n\
             struct Other {\n\
               build() { Column().wrongOwner() }\n\
             }\n",
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let helper_names = |engine: &Engine, caller_name: &str| {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/arkui-helper")
                .map(|(target, evidence)| {
                    assert_eq!(evidence.confidence, 0.97);
                    target.qualified_name
                })
                .collect::<Vec<_>>()
        };
        let mut card_helpers = helper_names(&engine, "Card.build");
        card_helpers.sort();
        assert_eq!(card_helpers, ["Card.cardStyle", "textStyle"]);
        assert!(helper_names(&engine, "Other.build").is_empty());

        fs::write(
            &source,
            "@Extend(Text)\n\
             function textStyle() { .fontSize(16) }\n\
             @Component\n\
             struct Card { build() { Text('plain') } }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(helper_names(&engine, "Card.build").is_empty());
    }

    #[test]
    fn harmony_emitter_channels_join_exact_callbacks_with_app_scope_and_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        for app in ["first", "second"] {
            fs::create_dir_all(temp.path().join(format!("{app}/AppScope"))).unwrap();
            fs::create_dir_all(temp.path().join(format!("{app}/entry/src/main/ets"))).unwrap();
            fs::write(temp.path().join(format!("{app}/AppScope/app.json5")), "{}").unwrap();
        }
        let listener = temp.path().join("first/entry/src/main/ets/listener.ets");
        fs::write(
            &listener,
            "import bus from '@ohos.events.emitter'\n\
             function handleFive() { return 'named' }\n\
             export function registerFive() {\n\
               let event: bus.InnerEvent = { eventId: 5, priority: 1 }\n\
               bus.on(event, handleFive)\n\
               bus.once({ eventId: 5 }, () => { console.info('inline') })\n\
               bus.on({ eventId: '5' }, () => { console.info('string') })\n\
               bus.on({ eventId: unknownId }, handleFive)\n\
             }\n\
             export function reassigned() {\n\
               let changed = { eventId: 5 }\n\
               changed = { eventId: 6 }\n\
               bus.on(changed, handleFive)\n\
             }\n\
             export function shadowed(bus: LocalEmitter) {\n\
               bus.on({ eventId: 5 }, handleFive)\n\
             }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("first/entry/src/main/ets/sender.ets"),
            "import { emitter as bus } from '@kit.BasicServicesKit'\n\
             export function sendFive() { bus.emit({ eventId: 5 }) }\n\
             export function sendStringFive() { bus.emit({ eventId: '5' }) }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("second/entry/src/main/ets/listener.ets"),
            "import emitter from '@ohos.events.emitter'\n\
             function otherFive() {}\n\
             export function registerOther() { emitter.on({ eventId: 5 }, otherFive) }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("first/entry/src/main/ets/lookalike.ets"),
            "function fake() {}\n\
             export function wrongImport() { emitter.on({ eventId: 5 }, fake) }\n",
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let emitter_targets = |engine: &Engine, caller_name: &str| {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/ohos-emitter")
                .collect::<Vec<_>>()
        };
        let number_targets = emitter_targets(&engine, "sendFive");
        assert_eq!(number_targets.len(), 2);
        assert!(number_targets.iter().any(|(target, evidence)| {
            target.qualified_name == "handleFive" && evidence.confidence == 0.97
        }));
        assert!(number_targets.iter().any(|(target, _)| {
            target.file == "first/entry/src/main/ets/listener.ets"
                && target.name == "<emitter callback n:5>"
        }));
        assert!(number_targets
            .iter()
            .all(|(target, _)| target.qualified_name != "otherFive"));

        let string_targets = emitter_targets(&engine, "sendStringFive");
        assert_eq!(string_targets.len(), 1);
        assert_eq!(string_targets[0].0.name, "<emitter callback s:5>");

        let wrong = engine
            .search("wrongImport", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.qualified_name == "wrongImport")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&wrong.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "framework/ohos-emitter"));

        fs::remove_file(&listener).unwrap();
        assert_eq!(engine.sync().unwrap().files_deleted, 1);
        assert!(emitter_targets(&engine, "sendFive").is_empty());
        assert!(emitter_targets(&engine, "sendStringFive").is_empty());
    }

    #[test]
    fn harmony_emitter_accepts_only_exact_immutable_constructor_descriptors() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("events.ets"),
            "import emitter from '@ohos.events.emitter'\n\
             class EventDescriptor {\n\
               eventId: number\n\
               constructor(id: number) { this.eventId = id }\n\
             }\n\
             class ExtraWork {\n\
               eventId: number\n\
               constructor(id: number) { this.eventId = id; console.info('side effect') }\n\
             }\n\
             class ExtraParameter {\n\
               eventId: number\n\
               constructor(id: number, other: number) { this.eventId = id }\n\
             }\n\
             class LaterMutation {\n\
               eventId: number\n\
               constructor(id: number) { this.eventId = id }\n\
               replace(id: number) { this.eventId = id }\n\
             }\n\
             const fortyTwo = new EventDescriptor(42)\n\
             let mutable = new EventDescriptor(42)\n\
             mutable = new EventDescriptor(43)\n\
             const propertyMutated = new EventDescriptor(45)\n\
             propertyMutated.eventId = 46\n\
             function onDirect() {}\n\
             function onBound() {}\n\
             function onExtra() {}\n\
             function onMutable() {}\n\
             function onExtraParameter() {}\n\
             function onUnknown() {}\n\
             function onLaterMutation() {}\n\
             function onPropertyMutated() {}\n\
             function onNestedLeak() {}\n\
             function defineNested() {\n\
               class NestedDescriptor {\n\
                 eventId: number\n\
                 constructor(id: number) { this.eventId = id }\n\
               }\n\
             }\n\
             export function register() {\n\
               emitter.on(new EventDescriptor(41), onDirect)\n\
               emitter.on(fortyTwo, onBound)\n\
               emitter.on(new ExtraWork(43), onExtra)\n\
               emitter.on(mutable, onMutable)\n\
               emitter.on(new ExtraParameter(44, 1), onExtraParameter)\n\
               emitter.on(new EventDescriptor(unknownId), onUnknown)\n\
               emitter.on(new LaterMutation(45), onLaterMutation)\n\
               emitter.on(propertyMutated, onPropertyMutated)\n\
             }\n\
             export function sendDirect() { emitter.emit(new EventDescriptor(41)) }\n\
             export function sendBound() { emitter.emit(fortyTwo) }\n\
             export function sendExtra() { emitter.emit(new ExtraWork(43)) }\n\
             export function sendMutable() { emitter.emit(mutable) }\n\
             export function sendExtraParameter() { emitter.emit(new ExtraParameter(44, 1)) }\n\
             export function sendUnknown() { emitter.emit(new EventDescriptor(unknownId)) }\n\
             export function sendLaterMutation() { emitter.emit(new LaterMutation(45)) }\n\
             export function sendPropertyMutated() { emitter.emit(propertyMutated) }\n\
             export function sendShadowed(EventDescriptor: LocalDescriptor) {\n\
               emitter.emit(new EventDescriptor(41))\n\
             }\n\
             export function sendNestedLeak() {\n\
               emitter.on(new NestedDescriptor(47), onNestedLeak)\n\
               emitter.emit(new NestedDescriptor(47))\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let emitter_targets = |caller_name: &str| {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/ohos-emitter")
                .map(|(target, _)| target.qualified_name)
                .collect::<Vec<_>>()
        };

        assert_eq!(emitter_targets("sendDirect"), ["onDirect"]);
        assert_eq!(emitter_targets("sendBound"), ["onBound"]);
        assert!(emitter_targets("sendExtra").is_empty());
        assert!(emitter_targets("sendMutable").is_empty());
        assert!(emitter_targets("sendExtraParameter").is_empty());
        assert!(emitter_targets("sendUnknown").is_empty());
        assert!(emitter_targets("sendLaterMutation").is_empty());
        assert!(emitter_targets("sendPropertyMutated").is_empty());
        assert!(emitter_targets("sendShadowed").is_empty());
        assert!(emitter_targets("sendNestedLeak").is_empty());
    }

    #[test]
    fn arkui_popup_builder_registrations_require_same_owner_decorated_targets() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("Popup.ets");
        fs::write(
            &source,
            "@Component\n\
             struct PopupCard {\n\
               @State open: boolean = false\n\
               @Builder PopupBuilder() { Text('popup') }\n\
               PlainBuilder() { Text('plain') }\n\
               build() {\n\
                 Row() {\n\
                   Text('trigger')\n\
                 }\n\
                   .onClick((): void => { this.open = true })\n\
                   .bindPopup(this.open, {\n\
                     builder: this.PopupBuilder,\n\
                     placement: Placement.Top,\n\
                     onStateChange: (event: EventVisibility): void => {\n\
                       if (!event.isVisible) { this.open = false }\n\
                     }\n\
                   })\n\
                 Row().bindPopup(this.open, { builder: this.PlainBuilder })\n\
                 customApi({ builder: this.PopupBuilder })\n\
                 Column() { Text('quoted key') }\n\
                   .bindPopup(this.open, { 'builder': this.PopupBuilder })\n\
               }\n\
             }\n\
             @Component\n\
             struct OrphanCard {\n\
               @State open: boolean = false\n\
               @Builder PopupBuilder() { Text('orphan') }\n\
               build() {\n\
                 .bindPopup\n\
                 (this.open, { builder: this.PopupBuilder })\n\
                 Row() { Text('interrupted') }\n\
                 .bindPopup\n\
                 Text('not arguments')\n\
                 (this.open, { builder: this.PopupBuilder })\n\
               }\n\
             }\n\
             @Component\n\
             struct OtherCard {\n\
               @Builder PopupBuilder() { Text('other') }\n\
               build() { Row() }\n\
             }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine, qualified_name: &str| {
            let callable = engine
                .search(qualified_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol;
            engine
                .callees(&callable.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "framework/arkui-builder-registration"
                })
                .collect::<Vec<_>>()
        };
        let registered = targets(&engine, "PopupCard.build");
        assert_eq!(registered.len(), 2);
        assert!(registered.iter().all(|(target, evidence)| {
            target.qualified_name == "PopupCard.PopupBuilder" && evidence.confidence == 0.97
        }));
        assert_eq!(
            registered
                .iter()
                .map(|(_, evidence)| evidence.line)
                .collect::<Vec<_>>(),
            vec![12, 21]
        );
        assert!(targets(&engine, "OrphanCard.build").is_empty());

        fs::write(
            &source,
            "@Component\n\
             struct PopupCard {\n\
               @State open: boolean = false\n\
               @Builder PopupBuilder() { Text('popup') }\n\
               build() { Row() }\n\
             }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(targets(&engine, "PopupCard.build").is_empty());
    }

    #[test]
    fn arkui_builder_params_require_exact_fields_and_same_owner_decorated_members() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("BuilderParam.ets");
        fs::write(
            &source,
            "@Component\n\
             struct Slot {\n\
               @BuilderParam content: any\n\
               @BuilderParam legend: any\n\
               build() { Column() { this.content(); this.legend() } }\n\
             }\n\
             @Component\n\
             struct Host {\n\
               @Builder content() { Text('content') }\n\
               @Builder legend() { Text('legend') }\n\
               plain() { Text('plain') }\n\
               build() {\n\
                 Slot({ content: this.content, 'legend': this.legend })\n\
                 Slot({ content: this.plain })\n\
                 Slot({ missing: this.content })\n\
                 Slot({ content: other.content })\n\
                 Slot({ content })\n\
               }\n\
             }\n\
             @Component\n\
             struct Other {\n\
               @Builder content() { Text('other') }\n\
               build() { Column() }\n\
             }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine| {
            let build = engine
                .search("Host.build", 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == "Host.build")
                .unwrap()
                .symbol;
            engine
                .callees(&build.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/arkui-builder-param")
                .collect::<Vec<_>>()
        };
        let assigned = targets(&engine);
        assert_eq!(assigned.len(), 2);
        assert_eq!(
            assigned
                .iter()
                .map(|(target, evidence)| (
                    target.qualified_name.as_str(),
                    evidence.confidence,
                    evidence.line
                ))
                .collect::<Vec<_>>(),
            vec![("Host.content", 0.97, 13), ("Host.legend", 0.97, 13)]
        );

        fs::write(
            &source,
            "@Component\n\
             struct Slot {\n\
               @BuilderParam content: any\n\
               build() { this.content() }\n\
             }\n\
             @Component\n\
             struct Host {\n\
               @Builder content() { Text('content') }\n\
               build() { Slot({ missing: this.content }) }\n\
             }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(targets(&engine).is_empty());
    }

    #[test]
    fn arkui_builder_param_flows_resolve_imported_builders_and_trailing_children() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Slot.ets"),
            "@Component\n\
             export struct Slot {\n\
               @BuilderParam content: () => void\n\
               build() { Column() { this.content() } }\n\
             }\n\
             @Component\n\
             export struct MultiSlot {\n\
               @BuilderParam content: () => void\n\
               @BuilderParam footer: () => void\n\
               build() { this.content(); this.footer() }\n\
             }\n\
             @Component\n\
             export struct PlainSlot {\n\
               handler: () => void\n\
               build() { Text('plain') }\n\
             }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("Builders.ets"),
            "@Builder\n\
             export function importedContent() { Text('imported') }\n\
             export function undecorated() { Text('plain') }\n",
        )
        .unwrap();
        let host = temp.path().join("Host.ets");
        fs::write(
            &host,
            "import { Slot as AliasedSlot, MultiSlot, PlainSlot } from './Slot'\n\
             import { importedContent as contentBuilder, undecorated } from './Builders'\n\
             @Component\n\
             struct Host {\n\
               build() {\n\
                 AliasedSlot({ content: contentBuilder })\n\
                 AliasedSlot({ content: undecorated })\n\
                 AliasedSlot({ missing: contentBuilder })\n\
                 AliasedSlot({ content: () => { Text('adapter') } })\n\
                 AliasedSlot() { Text('inline') }\n\
                 MultiSlot() { Text('ambiguous') }\n\
                 Column() { AliasedSlot({ content: () => { Text('recovered') } }) }\n\
                 Column() { PlainSlot({ handler: () => { Text('not a param') } }) }\n\
               }\n\
             }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let symbol = |engine: &Engine, qualified_name: &str| {
            engine
                .search(qualified_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == qualified_name)
                .unwrap()
                .symbol
        };
        let host_build = symbol(&engine, "Host.build");
        let registrations = engine
            .callees(&host_build.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "framework/arkui-builder-param")
            .collect::<Vec<_>>();
        assert_eq!(registrations.len(), 4);
        assert!(registrations.iter().any(|(target, evidence)| {
            target.qualified_name == "importedContent"
                && evidence.confidence == 0.97
                && evidence.line == 6
        }));
        let inline = registrations
            .iter()
            .find(|(target, evidence)| {
                target.name == "<BuilderParam child AliasedSlot>" && evidence.line == 10
            })
            .map(|(target, _)| target.clone())
            .unwrap();
        let adapter = registrations
            .iter()
            .find(|(target, evidence)| {
                target.name == "<BuilderParam adapter AliasedSlot.content>" && evidence.line == 9
            })
            .map(|(target, _)| target.clone())
            .unwrap();
        assert_eq!(
            registrations
                .iter()
                .filter(|(target, _)| {
                    target.name == "<BuilderParam adapter AliasedSlot.content>"
                })
                .count(),
            2
        );
        assert!(engine
            .search("<BuilderParam adapter PlainSlot.handler>", 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.name != "<BuilderParam adapter PlainSlot.handler>"));
        assert!(engine
            .search("<BuilderParam child MultiSlot>", 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.name != "<BuilderParam child MultiSlot>"));

        let slot_build = symbol(&engine, "Slot.build");
        let dispatches = engine
            .callees(&slot_build.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "framework/arkui-builder-param-dispatch")
            .collect::<Vec<_>>();
        assert_eq!(dispatches.len(), 4);
        assert!(dispatches
            .iter()
            .any(|(target, _)| target.qualified_name == "importedContent"));
        assert!(dispatches.iter().any(|(target, _)| target.id == inline.id));
        assert!(dispatches.iter().any(|(target, _)| target.id == adapter.id));

        let original = fs::read_to_string(&host).unwrap();
        fs::write(&host, format!("// position-only edit\n{original}")).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let moved_host = symbol(&engine, "Host.build");
        assert!(engine
            .callees(&moved_host.id)
            .unwrap()
            .into_iter()
            .any(|(target, evidence)| {
                target.id == inline.id
                    && evidence.provenance == "framework/arkui-builder-param"
                    && evidence.line == 11
            }));
        assert!(engine
            .callees(&moved_host.id)
            .unwrap()
            .into_iter()
            .any(|(target, evidence)| {
                target.id == adapter.id
                    && evidence.provenance == "framework/arkui-builder-param"
                    && evidence.line == 10
            }));

        fs::write(
            &host,
            "import { AliasedSlot } from './Missing'\n\
             @Component\n\
             struct Host {\n\
               build() { AliasedSlot() { Text('gone') } }\n\
             }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        let host_build = symbol(&engine, "Host.build");
        assert!(engine
            .callees(&host_build.id)
            .unwrap()
            .into_iter()
            .all(|(_, evidence)| evidence.provenance != "framework/arkui-builder-param"));
        assert!(engine
            .search(&adapter.qualified_name, 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.id != adapter.id));
        assert!(engine
            .search(&inline.qualified_name, 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.id != inline.id));
    }

    #[test]
    fn harmony_emitter_resolves_imported_exported_descriptors_and_literals() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("app/AppScope")).unwrap();
        fs::create_dir_all(temp.path().join("app/entry/src/main/ets")).unwrap();
        fs::write(temp.path().join("app/AppScope/app.json5"), "{}").unwrap();
        let constants = temp.path().join("app/entry/src/main/ets/events.ets");
        fs::write(
            &constants,
            "export const messageEvent = { eventId: 1 }\n\
             export const numericEvent = 1001\n\
             export const stringEvent = '1001'\n\
             export let mutableEvent = { eventId: 1 }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/EmitterConst.ts"),
            "export class EmitterConst {\n\
               public static readonly NUMERIC = 1025\n\
               public static readonly STRING = '1025'\n\
               public static MUTABLE = 1025\n\
             }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/barrel-one.ets"),
            "export { messageEvent as forwardedMessage, numericEvent } from './events'\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/barrel-two.ets"),
            "export { forwardedMessage as barrelMessage } from './barrel-one'\n\
             export * from './barrel-one'\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/value-a.ets"),
            "export const shared = 11\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/value-b.ets"),
            "export const shared = 12\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/ambiguous.ets"),
            "export * from './value-a'\nexport * from './value-b'\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/cycle-a.ets"),
            "export * from './cycle-b'\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/cycle-b.ets"),
            "export * from './cycle-a'\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/listener.ets"),
            "import emitter from '@ohos.events.emitter'\n\
             import { messageEvent as incoming, numericEvent, stringEvent, mutableEvent } from './events'\n\
             import { EmitterConst as Events } from './EmitterConst'\n\
             import { barrelMessage, numericEvent as barrelNumeric } from './barrel-two'\n\
             import { shared as ambiguous } from './ambiguous'\n\
             import { missing as cyclic } from './cycle-a'\n\
             function onMessage() {}\n\
             function onNumeric() {}\n\
             function onString() {}\n\
             function onMutable() {}\n\
             function onMemberNumeric() {}\n\
             function onMemberString() {}\n\
             function onMemberMutable() {}\n\
             function onBarrelMessage() {}\n\
             function onBarrelNumeric() {}\n\
             function onAmbiguous() {}\n\
             function onCycle() {}\n\
             export function registerImported() {\n\
               emitter.on(incoming, onMessage)\n\
               emitter.on(numericEvent, onNumeric)\n\
               emitter.on(stringEvent, onString)\n\
               emitter.on(mutableEvent, onMutable)\n\
               emitter.on(Events.NUMERIC, onMemberNumeric)\n\
               emitter.on(Events.STRING, onMemberString)\n\
               emitter.on(Events.MUTABLE, onMemberMutable)\n\
               emitter.on(barrelMessage, onBarrelMessage)\n\
               emitter.on(barrelNumeric, onBarrelNumeric)\n\
               emitter.on(ambiguous, onAmbiguous)\n\
               emitter.on(cyclic, onCycle)\n\
             }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("app/entry/src/main/ets/sender.ets"),
            "import { emitter as bus } from '@kit.BasicServicesKit'\n\
             import { messageEvent, numericEvent, stringEvent } from './events'\n\
             import { EmitterConst } from './EmitterConst'\n\
             import { barrelMessage, numericEvent as barrelNumeric } from './barrel-two'\n\
             import { shared as ambiguous } from './ambiguous'\n\
             import { missing as cyclic } from './cycle-a'\n\
             export function sendMessage() { bus.emit(messageEvent) }\n\
             export function sendNumeric() { bus.emit(numericEvent) }\n\
             export function sendString() { bus.emit(stringEvent) }\n\
             export function sendMemberNumeric() { bus.emit(EmitterConst.NUMERIC) }\n\
             export function sendMemberString() { bus.emit(EmitterConst.STRING) }\n\
             export function sendMemberMutable() { bus.emit(EmitterConst.MUTABLE) }\n\
             export function sendBarrelMessage() { bus.emit(barrelMessage) }\n\
             export function sendBarrelNumeric() { bus.emit(barrelNumeric) }\n\
             export function sendAmbiguous() { bus.emit(ambiguous) }\n\
             export function sendCycle() { bus.emit(cyclic) }\n\
             export function sendShadowed(EmitterConst: LocalEvents) {\n\
               bus.emit(EmitterConst.NUMERIC)\n\
             }\n",
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine, caller_name: &str| {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.qualified_name == caller_name)
                .unwrap()
                .symbol;
            let mut targets = engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/ohos-emitter")
                .map(|(target, _)| target.qualified_name)
                .collect::<Vec<_>>();
            targets.sort();
            targets
        };
        assert_eq!(
            targets(&engine, "sendMessage"),
            ["onBarrelMessage", "onMessage"]
        );
        assert_eq!(
            targets(&engine, "sendNumeric"),
            ["onBarrelNumeric", "onNumeric"]
        );
        assert_eq!(targets(&engine, "sendString"), ["onString"]);
        assert_eq!(targets(&engine, "sendMemberNumeric"), ["onMemberNumeric"]);
        assert_eq!(targets(&engine, "sendMemberString"), ["onMemberString"]);
        assert!(targets(&engine, "sendMemberMutable").is_empty());
        assert_eq!(
            targets(&engine, "sendBarrelMessage"),
            ["onBarrelMessage", "onMessage"]
        );
        assert_eq!(
            targets(&engine, "sendBarrelNumeric"),
            ["onBarrelNumeric", "onNumeric"]
        );
        assert!(targets(&engine, "sendAmbiguous").is_empty());
        assert!(targets(&engine, "sendCycle").is_empty());
        assert!(targets(&engine, "sendShadowed").is_empty());
        assert!(!targets(&engine, "sendMessage")
            .iter()
            .any(|target| target == "onMutable"));

        fs::remove_file(constants).unwrap();
        assert_eq!(engine.sync().unwrap().files_deleted, 1);
        assert!(targets(&engine, "sendMessage").is_empty());
        assert!(targets(&engine, "sendNumeric").is_empty());
        assert!(targets(&engine, "sendString").is_empty());
    }

    #[test]
    fn component_templates_ignore_script_strings_and_intrinsic_elements() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("Safe.svelte"),
            "<script>\n\
             function local() { return '<Fake on:click={local} />'; }\n\
             </script>\n\
             <button>safe</button>\n<",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let component = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| symbol.kind == crate::model::SymbolKind::Component)
            .unwrap();
        assert!(engine.callees(&component.id).unwrap().is_empty());
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
    fn nestjs_controller_decorators_join_paths_and_resolve_handlers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("users.controller.ts"),
            "@Controller('api/users')\n\
             export class UsersController {\n\
               @Get(':id')\n\
               findOne() { return 1; }\n\
               @Post()\n\
               create() { return 2; }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for (route_name, handler) in [
            ("GET /api/users/:id", "findOne"),
            ("POST /api/users", "create"),
        ] {
            let route = engine
                .search(route_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| {
                    hit.symbol.kind == crate::model::SymbolKind::Route
                        && hit.symbol.name == route_name
                })
                .unwrap_or_else(|| {
                    panic!(
                        "missing {route_name}; routes: {:?}",
                        engine
                            .search("GET POST", 20)
                            .unwrap()
                            .into_iter()
                            .filter(|hit| hit.symbol.kind == crate::model::SymbolKind::Route)
                            .map(|hit| hit.symbol.name)
                            .collect::<Vec<_>>()
                    )
                })
                .symbol;
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].0.name, handler);
            assert_eq!(callees[0].1.provenance, "framework/nestjs-route");
            assert_eq!(callees[0].1.confidence, 0.99);
        }
    }

    #[test]
    fn nestjs_adapter_rejects_dynamic_paths_and_lookalike_decorators() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("fake.controller.ts"),
            "const dynamic = 'users';\n\
             @Controller(dynamic)\n\
             class FakeController {\n\
               @Get(dynamic)\n\
               dynamicRoute() {}\n\
               @get('lowercase')\n\
               lookalike() {}\n\
             }\n\
             class NotAController {\n\
               @Get('accidental')\n\
               accidental() {}\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        assert!(engine
            .search("ROUTE", 20)
            .unwrap()
            .into_iter()
            .all(|hit| hit.symbol.kind != crate::model::SymbolKind::Route));
    }

    #[test]
    fn nestjs_controller_supports_literal_path_arrays_and_options() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("health.controller.ts"),
            "@Controller({ path: ['health', 'ready'] })\n\
             class HealthController {\n\
               @Get(['live', 'status'])\n\
               check() {}\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for route_name in [
            "GET /health/live",
            "GET /health/status",
            "GET /ready/live",
            "GET /ready/status",
        ] {
            let route = engine
                .search(route_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| {
                    hit.symbol.kind == crate::model::SymbolKind::Route
                        && hit.symbol.name == route_name
                })
                .unwrap()
                .symbol;
            assert_eq!(engine.callees(&route.id).unwrap()[0].0.name, "check");
        }
    }

    #[test]
    fn nestjs_transport_decorators_create_exact_routes_and_handler_edges() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("transport.ts"),
            "import { SubscribeMessage as OnMessage } from '@nestjs/websockets';\n\
             import { MessagePattern, EventPattern as OnEvent, GrpcMethod, GrpcStreamMethod } from '@nestjs/microservices';\n\
             export class TransportController {\n\
               @OnMessage('chat.message')\n\
               socket() {}\n\
               @MessagePattern([{ cmd: 'sum', version: 2 }, 'health'], { transport: 'tcp' }, dynamicExtras)\n\
               message() {}\n\
               @MessagePattern(-7)\n\
               numeric() {}\n\
               @OnEvent({ topic: 'created', durable: true })\n\
               event() {}\n\
               @GrpcMethod()\n\
               unary() {}\n\
               @GrpcMethod('')\n\
               blankService() {}\n\
               @GrpcMethod('Heroes', '')\n\
               blankMethod() {}\n\
               @GrpcStreamMethod('Heroes', 'FindMany')\n\
               stream() {}\n\
               @GrpcStreamMethod('', '')\n\
               blankStream() {}\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for (route_name, handler, provenance) in [
            (
                "WS \"chat.message\"",
                "socket",
                "framework/nestjs-websocket",
            ),
            (
                "MESSAGE {\"cmd\":\"sum\",\"version\":2}",
                "message",
                "framework/nestjs-microservice",
            ),
            (
                "MESSAGE \"health\"",
                "message",
                "framework/nestjs-microservice",
            ),
            ("MESSAGE -7", "numeric", "framework/nestjs-microservice"),
            (
                "EVENT {\"durable\":true,\"topic\":\"created\"}",
                "event",
                "framework/nestjs-microservice",
            ),
            (
                "GRPC TransportController/Unary",
                "unary",
                "framework/nestjs-microservice",
            ),
            (
                "GRPC TransportController/BlankService",
                "blankService",
                "framework/nestjs-microservice",
            ),
            (
                "GRPC Heroes/BlankMethod",
                "blankMethod",
                "framework/nestjs-microservice",
            ),
            (
                "GRPC_STREAM Heroes/FindMany",
                "stream",
                "framework/nestjs-microservice",
            ),
            (
                "GRPC_STREAM TransportController/BlankStream",
                "blankStream",
                "framework/nestjs-microservice",
            ),
        ] {
            let route = engine
                .snapshot()
                .unwrap()
                .symbols
                .into_iter()
                .find(|symbol| {
                    symbol.kind == crate::model::SymbolKind::Route && symbol.name == route_name
                })
                .unwrap_or_else(|| panic!("missing transport route {route_name}"));
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1, "{route_name}");
            assert_eq!(callees[0].0.name, handler);
            assert_eq!(
                callees[0].0.qualified_name,
                format!("TransportController.{handler}")
            );
            assert_eq!(callees[0].1.provenance, provenance);
            assert_eq!(callees[0].1.confidence, 0.99);
        }
    }

    #[test]
    fn nestjs_transport_adapter_rejects_unproven_shadowed_and_dynamic_decorators() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("invalid.ts"),
            "import type { MessagePattern as Typed } from '@nestjs/microservices';\n\
             import { MessagePattern, GrpcMethod } from '@nestjs/microservices';\n\
             import { SubscribeMessage } from './lookalike';\n\
             const pattern = 'dynamic';\n\
             function scope(MessagePattern: Function) { return MessagePattern; }\n\
             class Invalid {\n\
               @Typed('typed') typed() {}\n\
               @MessagePattern(pattern) dynamic() {}\n\
               @MessagePattern({ nested: { value: 1 } }) nested() {}\n\
               @MessagePattern('too-many', 1, 2, 3) tooMany() {}\n\
               @GrpcMethod('Service', pattern) grpcDynamic() {}\n\
               @GrpcMethod('A', 'B', 'C') grpcExtra() {}\n\
               @SubscribeMessage('fake') fake() {}\n\
             }\n",
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
    fn nestjs_transport_shadow_detection_covers_destructured_parameters_and_catches() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("shadowed.ts"),
            "import { MessagePattern as ParamPattern, EventPattern as CatchPattern } from '@nestjs/microservices';\n\
             function parameter({ nested: { ParamPattern } }: any) {\n\
               class Nested { @ParamPattern('false-param') handle() {} }\n\
             }\n\
             try { throw {}; } catch ({ nested: { CatchPattern } }) {\n\
               class Nested { @CatchPattern('false-catch') handle() {} }\n\
             }\n",
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
    fn nestjs_transport_route_ids_ignore_unrelated_same_pattern_insertions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stable.ts");
        let original = "import { MessagePattern } from '@nestjs/microservices';\n\
                        class Stable { @MessagePattern('same') target() {} }\n";
        fs::write(&path, original).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let original_id = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| {
                symbol.kind == crate::model::SymbolKind::Route && symbol.name == "MESSAGE \"same\""
            })
            .unwrap()
            .id;

        fs::write(
            &path,
            "import { MessagePattern } from '@nestjs/microservices';\n\
             class Earlier { @MessagePattern('same') unrelated() {} }\n\
             class Stable { @MessagePattern('same') target() {} }\n",
        )
        .unwrap();
        engine.sync().unwrap();
        let stable_route = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| {
                symbol.kind == crate::model::SymbolKind::Route
                    && symbol.name == "MESSAGE \"same\""
                    && symbol.id == original_id
            });
        assert!(stable_route.is_some());
    }

    #[test]
    fn nestjs_transport_routes_clean_up_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consumer.ts");
        fs::write(
            &path,
            "import { EventPattern } from '@nestjs/microservices';\n\
             class Consumer { @EventPattern('created') consume() {} }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .any(|symbol| symbol.kind == crate::model::SymbolKind::Route));

        fs::write(&path, "class Consumer { consume() {} }\n").unwrap();
        engine.sync().unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| symbol.kind != crate::model::SymbolKind::Route));
    }

    #[test]
    fn nestjs_graphql_decorators_create_exact_routes_and_handler_edges() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("graphql.ts"),
            "import {\n\
               Resolver as ObjectResolver,\n\
               Query as Read,\n\
               Mutation,\n\
               ResolveField as Field,\n\
               Subscription,\n\
               ResolveReference,\n\
             } from '@nestjs/graphql';\n\
             import * as gql from '@nestjs/graphql';\n\
             class User {}\n\
             @ObjectResolver(of => User)\n\
             export class UserResolver {\n\
               @Read(() => User, { name: 'user' })\n\
               findOne() {}\n\
               @Mutation('createUser')\n\
               create() {}\n\
               @Field(() => User, { name: 'displayName' })\n\
               display() {}\n\
               @Subscription(() => User)\n\
               userChanged() {}\n\
               @ResolveReference()\n\
               resolveReference() {}\n\
             }\n\
             @gql.Resolver('Account')\n\
             class AccountResolver {\n\
               @gql.ResolveField('owner')\n\
               accountOwner() {}\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for (route_name, handler) in [
            ("GRAPHQL QUERY user", "findOne"),
            ("GRAPHQL MUTATION createUser", "create"),
            ("GRAPHQL FIELD User.displayName", "display"),
            ("GRAPHQL SUBSCRIPTION userChanged", "userChanged"),
            ("GRAPHQL REFERENCE User", "resolveReference"),
            ("GRAPHQL FIELD Account.owner", "accountOwner"),
        ] {
            let route = engine
                .search(route_name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| {
                    hit.symbol.kind == crate::model::SymbolKind::Route
                        && hit.symbol.name == route_name
                })
                .unwrap_or_else(|| panic!("missing GraphQL route {route_name}"))
                .symbol;
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1, "{route_name}");
            assert_eq!(callees[0].0.name, handler);
            assert_eq!(callees[0].1.provenance, "framework/nestjs-graphql");
            assert_eq!(callees[0].1.confidence, 0.99);
        }
    }

    #[test]
    fn nestjs_graphql_adapter_rejects_unproven_dynamic_and_parentless_shapes() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("adversarial.ts"),
            "import { Resolver, Query, Query as Read, ResolveField } from '@nestjs/graphql';\n\
             import { Mutation } from 'lookalike';\n\
             import * as gql from 'lookalike';\n\
             import * as duplicate from '@nestjs/graphql';\n\
             import * as duplicate from 'lookalike';\n\
             const dynamicName = 'unsafe';\n\
             function Query() {}\n\
             class User {}\n\
             @Resolver(() => User.Model)\n\
             class QualifiedParent { @ResolveField() qualified() {} }\n\
             @Resolver(() => { return User })\n\
             class BlockParent { @ResolveField() block() {} }\n\
             @Resolver(async () => User)\n\
             class AsyncParent { @ResolveField() asynchronous() {} }\n\
             @Resolver((value = User) => User)\n\
             class DefaultParent { @ResolveField() defaulted() {} }\n\
             @Resolver(({ value }) => User)\n\
             class DestructuredParent { @ResolveField() destructured() {} }\n\
             @Resolver((...value) => User)\n\
             class RestParent { @ResolveField() rest() {} }\n\
             @duplicate.Resolver()\n\
             class DuplicateNamespace { @duplicate.Query('duplicate') duplicate() {} }\n\
             @Resolver()\n\
             class Parentless {\n\
               @ResolveField() field() {}\n\
               @Query('shadowed') shadowed() {}\n\
               @Read(() => User, { name: dynamicName }) dynamic() {}\n\
               @Read('$invalid') invalidGraphqlName() {}\n\
               @Mutation('lookalike') mutation() {}\n\
               @gql.Query('namespace') namespaceQuery() {}\n\
             }\n\
             function nested(Query) {\n\
               @Resolver()\n\
               class NestedResolver { @Query('nestedShadow') nestedShadow() {} }\n\
             }\n\
             class MissingResolver { @Query('accidental') accidental() {} }\n",
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
    fn nestjs_graphql_shadowing_is_lexical_and_namespace_provenance_is_exact() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("lexical.ts"),
            "import { Resolver, Query } from '@nestjs/graphql';\n\
             import * as gql from '@nestjs/graphql';\n\
             function unrelated(Query, { gql }) { return Query || gql }\n\
             @Resolver()\n\
             class RootResolver { @Query('_root2') root() {} }\n\
             @gql.Resolver()\n\
             class NamespaceResolver { @gql.Query('namespace2') namespaced() {} }\n\
             function hoistedVarScope() {\n\
               @Resolver()\n\
               class HoistedResolver { @Query('hoistedVarLeak') leaked() {} }\n\
               if (condition) { var Query = fakeDecorator }\n\
             }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        for route_name in ["GRAPHQL QUERY _root2", "GRAPHQL QUERY namespace2"] {
            assert!(engine
                .snapshot()
                .unwrap()
                .symbols
                .iter()
                .any(|symbol| symbol.kind == crate::model::SymbolKind::Route
                    && symbol.name == route_name));
        }
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| symbol.kind != crate::model::SymbolKind::Route
                || symbol.name != "GRAPHQL QUERY hoistedVarLeak"));
    }

    #[test]
    fn nestjs_graphql_route_ids_ignore_unrelated_same_operation_insertions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("stable.ts");
        fs::write(
            &path,
            "import { Resolver, Query } from '@nestjs/graphql';\n\
             @Resolver() class StableResolver { @Query('same') target() {} }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let original = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| {
                symbol.kind == crate::model::SymbolKind::Route
                    && symbol.name == "GRAPHQL QUERY same"
            })
            .unwrap();

        fs::write(
            &path,
            "import { Resolver, Query } from '@nestjs/graphql';\n\
             @Resolver() class EarlierResolver { @Query('same') unrelated() {} }\n\
             @Resolver() class StableResolver { @Query('same') target() {} }\n",
        )
        .unwrap();
        engine.sync().unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .any(|symbol| symbol.id == original.id
                && symbol.kind == crate::model::SymbolKind::Route
                && symbol.name == "GRAPHQL QUERY same"));
    }

    #[test]
    fn nestjs_graphql_routes_clean_up_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("incremental.ts");
        fs::write(
            &path,
            "import { Resolver, Query } from '@nestjs/graphql';\n\
             @Resolver() class ResolverClass { @Query() active() {} }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .any(|symbol| symbol.kind == crate::model::SymbolKind::Route
                && symbol.name == "GRAPHQL QUERY active"));

        fs::write(&path, "class ResolverClass { active() {} }\n").unwrap();
        engine.sync().unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| symbol.kind != crate::model::SymbolKind::Route));
    }

    #[test]
    fn fastapi_composes_nested_imported_factory_and_constructed_class_routers() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("routers.py"),
            "from fastapi import APIRouter\n\
             v1 = APIRouter(prefix='/items')\n\
             nested = APIRouter(prefix='/detail')\n\
             v1.include_router(nested, prefix='/nested')\n\n\
             @nested.get('/{item_id}')\n\
             def show_item(item_id: str):\n\
             \x20   return item_id\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("factory.py"),
            "from fastapi import APIRouter\n\
             router = APIRouter(prefix='/factory')\n\n\
             @router.post('/run')\n\
             def run_factory():\n\
             \x20   return None\n\n\
             def create_router(config, *, enabled=True):\n\
             \x20   return router\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.py"),
            "from fastapi import FastAPI, APIRouter\n\
             from .routers import v1\n\
             import factory\n\n\
             class HealthRoutes:\n\
             \x20   def __init__(self):\n\
             \x20       self.router = APIRouter(prefix='/health')\n\n\
             \x20   @self.router.get('/ready')\n\
             \x20   def ready(self):\n\
             \x20       return True\n\n\
             health = HealthRoutes()\n\
             app = FastAPI()\n\
             app.include_router(v1, prefix='/api')\n\
             app.include_router(factory.create_router({'mode': 'safe'}, enabled=True), prefix='/api')\n\
             app.include_router(health.router, prefix='/ops')\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let mut names = snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "GET /api/items/nested/detail/{item_id}",
                "GET /ops/health/ready",
                "POST /api/factory/run"
            ]
        );
        for route in snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
        {
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].1.provenance, "framework/fastapi-route");
            assert_eq!(callees[0].1.confidence, 0.995);
        }
    }

    #[test]
    fn fastapi_exact_composition_rejects_spoofs_dynamic_prefixes_ambiguity_and_cycles() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             first = APIRouter(prefix='/first')\n\
             second = APIRouter(prefix='/second')\n\
             first.include_router(second)\n\
             second.include_router(first)\n\
             dynamic = '/dynamic'\n\
             app.include_router(first, prefix=dynamic)\n\
             ambiguous = APIRouter()\n\
             ambiguous = APIRouter(prefix='/changed')\n\n\
             @second.get('/safe')\n\
             def safe(): return True\n\n\
             @ambiguous.get('/excluded')\n\
             def excluded(): return False\n\n\
             class Fake:\n\
             \x20   def get(self, path): return lambda fn: fn\n\
             fake = Fake()\n\
             @fake.get('/spoof')\n\
             def spoof(): return False\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        assert!(routes.is_empty());
    }

    #[test]
    fn fastapi_resolves_bounded_immutable_string_route_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            r#"from fastapi import FastAPI
ROOT = "/webui"
API = "/api"
VERSION = "/v1"

def create_app():
    webui_path = ROOT
    combined = API + VERSION
    app = FastAPI()

    @app.get(webui_path)
    def webui(): return True

    @app.get(f"{webui_path}/")
    def webui_slash(): return True

    @app.get(combined + "/items")
    def items(): return True

    return app
"#,
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let mut routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        routes.sort();
        assert_eq!(routes, ["GET /api/v1/items", "GET /webui", "GET /webui/"]);
    }

    #[test]
    fn fastapi_string_route_paths_fail_closed_on_mutation_shadowing_and_dynamic_expressions() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            r#"from fastapi import FastAPI
from settings import IMPORTED

ROOT = "/real"
REASSIGNED = "/first"
REASSIGNED = "/second"
CYCLIC_A = CYCLIC_B
CYCLIC_B = CYCLIC_A
app = FastAPI()

@app.get(ROOT)
def real(): return True

@app.get(REASSIGNED)
def reassigned(): return False

@app.get(IMPORTED)
def imported(): return False

@app.get(settings.PATH)
def attribute(): return False

@app.get(make_path())
def called(): return False

@app.get(f"{ROOT!r}")
def converted(): return False

@app.get(f"{ROOT:>20}")
def formatted(): return False

@app.get(CYCLIC_A)
def cyclic(): return False

def parameter_shadow(ROOT):
    @app.get(ROOT)
    def parameter(): return False

def unresolved_nearer_shadow():
    ROOT = make_path()
    @app.get(ROOT)
    def nearer(): return False

def conditional_binding(flag):
    if flag:
        PATH = "/conditional"
    @app.get(PATH)
    def conditional(): return False

def loop_binding():
    for PATH in ["/loop"]:
        pass
    @app.get(PATH)
    def looped(): return False

def import_shadow():
    from settings import ROOT
    @app.get(ROOT)
    def imported_shadow(): return False

class Fake:
    def get(self, path): return lambda function: function

fake = Fake()
@fake.get(ROOT)
def spoof(): return False
"#,
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        assert_eq!(routes, ["GET /real"]);
    }

    #[test]
    fn fastapi_string_paths_respect_python_order_scope_and_literal_semantics() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            r#"from fastapi import FastAPI

GLOBAL = "/global"
UNICODE = "/café"
RAW = r"/raw\segment"
TRIPLE = """/triple"""
ESCAPED = "/\u0061"
BYTES = b"/bytes"
STALE = "/stale"
del STALE
MATCHED = "/outer-match"
app = FastAPI()

@app.get(path=GLOBAL)
def keyword_path(): return True

@app.get(UNICODE)
def unicode_path(): return True

@app.get(RAW)
def raw_path(): return True

@app.get(TRIPLE)
def triple_path(): return True

@app.get(ESCAPED)
def escaped_path(): return False

@app.get(BYTES)
def bytes_path(): return False

@app.get(STALE)
def deleted_path(): return False

@app.get(LATER)
def defined_later(): return False

LATER = "/later"

class Scope:
    GLOBAL = "/wrong-class-value"

    def register(self):
        @app.get(GLOBAL)
        def class_scope_is_not_a_closure(): return True

def match_shadow(value):
    match value:
        case MATCHED:
            pass
    @app.get(MATCHED)
    def captured(): return False

def declared_global():
    global GLOBAL
    @app.get(GLOBAL)
    def global_declaration_is_conservative(): return False

def outer():
    NONLOCAL = "/nonlocal"
    def inner():
        nonlocal NONLOCAL
        @app.get(NONLOCAL)
        def nonlocal_declaration_is_conservative(): return False
"#,
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let mut routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        routes.sort();
        assert_eq!(
            routes,
            [
                "GET /café",
                "GET /global",
                "GET /global",
                r"GET /raw\segment",
                "GET /triple",
            ]
        );
    }

    #[test]
    fn fastapi_materialization_is_stable_and_cleans_up_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let router_path = temp.path().join("router.py");
        fs::write(
            &router_path,
            "from fastapi import APIRouter\n\
             router = APIRouter(prefix='/users')\n\
             @router.get('/all')\n\
             def list_users(): return []\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.py"),
            "from fastapi import FastAPI\n\
             from .router import router\n\
             app = FastAPI()\n\
             app.include_router(router, prefix='/api')\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let original = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| symbol.name == "GET /api/users/all")
            .unwrap();
        engine.sync().unwrap();
        let unchanged = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .find(|symbol| symbol.name == "GET /api/users/all")
            .unwrap();
        assert_eq!(original.id, unchanged.id);

        fs::write(
            &router_path,
            "from fastapi import APIRouter\nrouter = APIRouter(prefix='/users')\n",
        )
        .unwrap();
        engine.sync().unwrap();
        assert!(engine
            .snapshot()
            .unwrap()
            .symbols
            .iter()
            .all(|symbol| symbol.kind != crate::model::SymbolKind::Route));
    }

    #[test]
    fn fastapi_dependencies_resolve_direct_annotated_decorator_and_factory_returns() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("dependencies.py"),
            "from typing import Annotated\n\
             from fastapi import Depends\n\
             def get_settings(): return object()\n\
             def get_graphiti(): return object()\n\
             def dependency_factory():\n\
             \x20   async def combined_dependency(): return True\n\
             \x20   return combined_dependency\n\
             GraphDependency = Annotated[object, Depends(get_graphiti)]\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from typing import Annotated\n\
             from fastapi import Depends, FastAPI\n\
             from .dependencies import get_settings, dependency_factory, GraphDependency\n\
             app = FastAPI()\n\
             combined_auth = dependency_factory()\n\
             @app.get('/items', dependencies=[Depends(combined_auth)])\n\
             async def items(settings: Annotated[object, Depends(get_settings)], graph: GraphDependency): return settings\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let items = engine
            .search("items", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "items")
            .unwrap()
            .symbol;
        let mut dependencies = engine
            .callees(&items.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "framework/fastapi-dependency")
            .map(|(symbol, evidence)| (symbol.qualified_name, evidence.confidence))
            .collect::<Vec<_>>();
        dependencies.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            dependencies,
            [
                ("dependency_factory.combined_dependency".to_owned(), 0.995),
                ("get_graphiti".to_owned(), 0.995),
                ("get_settings".to_owned(), 0.995)
            ]
        );
    }

    #[test]
    fn fastapi_dependencies_follow_barrels_and_fail_closed_on_spoofs_shadows_and_cycles() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("pkg")).unwrap();
        fs::write(
            temp.path().join("pkg/deps.py"),
            "def verified_dependency(): return True\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("pkg/__init__.py"),
            "from .deps import verified_dependency\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("cycle_a.py"),
            "from .cycle_b import dependency\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("cycle_b.py"),
            "from .cycle_a import dependency\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import Depends\n\
             from .pkg import verified_dependency\n\
             from .cycle_a import dependency as cyclic\n\
             def accepted(value=Depends(verified_dependency)): return value\n\
             def empty(value=Depends()): return value\n\
             def cycle(value=Depends(cyclic)): return value\n\
             def shadowed(Depends, value=Depends(verified_dependency)): return value\n\
             class Annotated:\n\
             \x20   def __class_getitem__(cls, value): return value\n\
             def fake_annotated(value: Annotated[object, Depends(verified_dependency)]): return value\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("spoof.py"),
            "def Depends(value): return value\n\
             def fake(): return True\n\
             def rejected(value=Depends(fake)): return value\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let accepted = engine
            .search("accepted", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "accepted")
            .unwrap()
            .symbol;
        let dependencies = engine
            .callees(&accepted.id)
            .unwrap()
            .into_iter()
            .filter(|(_, evidence)| evidence.provenance == "framework/fastapi-dependency")
            .collect::<Vec<_>>();
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].0.name, "verified_dependency");
        for name in ["empty", "cycle", "shadowed", "fake_annotated", "rejected"] {
            let owner = engine
                .search(name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == name)
                .unwrap()
                .symbol;
            assert!(engine
                .callees(&owner.id)
                .unwrap()
                .iter()
                .all(|(_, evidence)| evidence.provenance != "framework/fastapi-dependency"));
        }
    }

    #[test]
    fn fastapi_dependency_edges_are_stable_and_clean_up_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        let api = temp.path().join("api.py");
        fs::write(
            &api,
            "from fastapi import Depends\n\
             def dependency(): return True\n\
             def endpoint(value=Depends(dependency)): return value\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let endpoint = engine
            .search("endpoint", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "endpoint")
            .unwrap()
            .symbol;
        let first = engine
            .callees(&endpoint.id)
            .unwrap()
            .into_iter()
            .find(|(_, evidence)| evidence.provenance == "framework/fastapi-dependency")
            .unwrap();
        engine.sync().unwrap();
        let second = engine
            .callees(&endpoint.id)
            .unwrap()
            .into_iter()
            .find(|(_, evidence)| evidence.provenance == "framework/fastapi-dependency")
            .unwrap();
        assert_eq!(first.0.id, second.0.id);

        fs::write(
            &api,
            "from fastapi import Depends\n\
             def dependency(): return True\n\
             def endpoint(value=None): return value\n",
        )
        .unwrap();
        engine.sync().unwrap();
        let endpoint = engine
            .search("endpoint", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "endpoint")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&endpoint.id)
            .unwrap()
            .iter()
            .all(|(_, evidence)| evidence.provenance != "framework/fastapi-dependency"));
    }

    #[test]
    fn fastapi_dependencies_enforce_alias_dominance_type_proof_and_site_identity() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("types.py"),
            "def OrdinaryAnnotation(): return object()\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import Depends\n\
             from .types import OrdinaryAnnotation\n\
             def direct(): return True\n\
             def factory():\n\
             \x20   def nested(): return True\n\
             \x20   return nested\n\
             def local_factory(): return direct\n\
             def before(value=Depends(late)): return value\n\
             late = factory()\n\
             reassigned = factory()\n\
             reassigned = local_factory()\n\
             def rejected(value=Depends(reassigned)): return value\n\
             local_alias = local_factory()\n\
             def accepted(value=Depends(local_alias)): return value\n\
             def typed(value: OrdinaryAnnotation): return value\n\
             InvalidAlias = Depends(direct)\n\
             def invalid_type_alias(value: InvalidAlias): return value\n\
             def before_type_alias(value: LateAlias): return value\n\
             LateAlias = Annotated[object, Depends(direct)]\n\
             def duplicate(left=Depends(direct), right=Depends(direct)): return left\n\
             def custom(callback): return callback\n\
             @custom(Depends(direct))\n\
             def decorated(): return True\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let dependency_edges = |name: &str| {
            let owner = engine
                .search(name, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == name)
                .unwrap()
                .symbol;
            engine
                .callees(&owner.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| evidence.provenance == "framework/fastapi-dependency")
                .collect::<Vec<_>>()
        };

        for rejected in [
            "before",
            "rejected",
            "typed",
            "invalid_type_alias",
            "before_type_alias",
            "decorated",
        ] {
            assert!(dependency_edges(rejected).is_empty(), "{rejected}");
        }
        let accepted = dependency_edges("accepted");
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].0.name, "direct");
        let duplicate = dependency_edges("duplicate");
        assert_eq!(duplicate.len(), 2);
        assert_ne!(duplicate[0].1.site, duplicate[1].1.site);
    }

    #[test]
    fn fastapi_supports_verified_module_style_constructors() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "import fastapi as framework\n\
             app = framework.FastAPI()\n\
             router = framework.APIRouter(prefix='/items')\n\
             app.include_router(prefix='/api', router=router)\n\
             @router.get('/all')\n\
             def all_items(): return []\n\
             @router.get('/')\n\
             def item_root(): return []\n\
             @router.trace('/trace')\n\
             def trace_items(): return None\n\
             @router.websocket('/events')\n\
             async def item_events(socket): return None\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let mut routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        routes.sort();
        assert_eq!(
            routes,
            [
                "GET /api/items/",
                "GET /api/items/all",
                "TRACE /api/items/trace",
                "WEBSOCKET /api/items/events"
            ]
        );
    }

    #[test]
    fn fastapi_does_not_publish_unmounted_routers_when_an_application_exists() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             mounted = APIRouter(prefix='/mounted')\n\
             hidden = APIRouter(prefix='/hidden')\n\
             app.include_router(mounted)\n\
             @mounted.get('/yes')\n\
             def yes(): return True\n\
             @hidden.get('/no')\n\
             def no(): return False\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        assert_eq!(routes, ["GET /mounted/yes"]);
    }

    #[test]
    fn fastapi_duplicate_mount_ids_survive_unrelated_mount_removal() {
        let temp = tempfile::tempdir().unwrap();
        let api = temp.path().join("api.py");
        fs::write(
            &api,
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             router = APIRouter()\n\
             app.include_router(router, prefix='/earlier')\n\
             app.include_router(router, prefix='/same')\n\
             @router.get('/route')\n\
             def route(): return True\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let original_id = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.name == "GET /same/route")
            .map(|symbol| symbol.id)
            .next()
            .unwrap();

        fs::write(
            &api,
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             router = APIRouter()\n\
             # unrelated text may freely shift mount source lines\n\
             # first mount removed\n\
             app.include_router(router, prefix='/same')\n\
             @router.get('/route')\n\
             def route(): return True\n",
        )
        .unwrap();
        engine.sync().unwrap();
        let remaining_ids = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.name == "GET /same/route")
            .map(|symbol| symbol.id)
            .collect::<Vec<_>>();
        assert_eq!(remaining_ids.len(), 1);
        assert_eq!(remaining_ids[0], original_id);
    }

    #[test]
    fn fastapi_lexical_bindings_reject_parameter_and_local_router_shadows() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             router = APIRouter()\n\
             app.include_router(router)\n\
             @router.get('/real')\n\
             def real(): return True\n\
             def register_parameter(router):\n\
             \x20   @router.get('/parameter-fake')\n\
             \x20   def parameter_fake(): return False\n\
             def register_local():\n\
             \x20   router = object()\n\
             \x20   @router.get('/local-fake')\n\
             \x20   def local_fake(): return False\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        assert_eq!(routes, ["GET /real"]);
    }

    #[test]
    fn fastapi_same_named_local_factory_routers_compose_independently() {
        let temp = tempfile::tempdir().unwrap();
        let api = temp.path().join("api.py");
        fs::write(
            &api,
            "from fastapi import FastAPI, APIRouter\n\
             def create_alpha():\n\
             \x20   router = APIRouter(prefix='/alpha')\n\
             \x20   @router.get('/value')\n\
             \x20   def alpha_value(): return 'alpha'\n\
             \x20   return router\n\
             def create_beta():\n\
             \x20   router = APIRouter(prefix='/beta')\n\
             \x20   @router.get('/value')\n\
             \x20   def beta_value(): return 'beta'\n\
             \x20   return router\n\
             app = FastAPI()\n\
             app.include_router(create_alpha())\n\
             app.include_router(create_beta())\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let mut routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| (symbol.name, symbol.id))
            .collect::<Vec<_>>();
        routes.sort();
        assert_eq!(
            routes
                .iter()
                .map(|route| route.0.as_str())
                .collect::<Vec<_>>(),
            ["GET /alpha/value", "GET /beta/value"]
        );
        let stable_ids = routes
            .iter()
            .map(|route| route.1.clone())
            .collect::<Vec<_>>();

        let original = fs::read_to_string(&api).unwrap();
        fs::write(&api, format!("# unrelated insertion\n{original}")).unwrap();
        engine.sync().unwrap();
        let mut shifted = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| (symbol.name, symbol.id))
            .collect::<Vec<_>>();
        shifted.sort();
        assert_eq!(
            shifted.into_iter().map(|route| route.1).collect::<Vec<_>>(),
            stable_ids
        );
    }

    #[test]
    fn fastapi_rejects_reassigned_and_comprehensively_shadowed_bindings() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("routes.py"),
            "from fastapi import APIRouter\n\
             router = APIRouter()\n\
             @router.get('/leak')\n\
             def leak(): return False\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import FastAPI, APIRouter\n\
             from .routes import router\n\
             app = FastAPI()\n\
             for router in []:\n\
             \x20   pass\n\
             app.include_router(router)\n\
             local = APIRouter()\n\
             local: object = object()\n\
             app.include_router(local)\n\
             @local.get('/also-leaks')\n\
             def also_leaks(): return False\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        assert!(routes.is_empty(), "unexpected routes: {routes:?}");
    }

    #[test]
    fn fastapi_factories_require_one_proven_return_target() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             wanted = APIRouter(prefix='/wanted')\n\
             other = APIRouter(prefix='/other')\n\
             @wanted.get('/route')\n\
             def route(): return True\n\
             def choose(flag):\n\
             \x20   if flag:\n\
             \x20       return other\n\
             \x20   return wanted\n\
             app.include_router(choose(True))\n",
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
    fn fastapi_preserves_literal_double_slashes_and_valid_empty_paths() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("api.py"),
            "from fastapi import FastAPI, APIRouter\n\
             app = FastAPI()\n\
             router = APIRouter(prefix='//items')\n\
             app.include_router(router, prefix='//api')\n\
             @router.get('')\n\
             def root(): return True\n\
             @router.get('//detail')\n\
             def detail(): return True\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let mut routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        routes.sort();
        assert_eq!(routes, ["GET //api//items", "GET //api//items//detail"]);
    }

    #[test]
    fn fastapi_resolves_router_reexports_through_python_package_initializers() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("pkg")).unwrap();
        fs::write(
            temp.path().join("pkg/routes.py"),
            "from fastapi import APIRouter\n\
             router = APIRouter(prefix='/pkg')\n\
             @router.get('/route')\n\
             def route(): return True\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("pkg/__init__.py"),
            "from .routes import router\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.py"),
            "from fastapi import FastAPI\n\
             from pkg import router\n\
             app = FastAPI()\n\
             app.include_router(router)\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        assert_eq!(routes, ["GET /pkg/route"]);
    }

    #[test]
    fn fastapi_composes_imported_only_mounts_and_decorators_through_proven_declarations() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("application.py"),
            "from fastapi import FastAPI\napp = FastAPI()\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("routes.py"),
            "from fastapi import APIRouter\n\
             router = APIRouter(prefix='/items')\n\
             @router.get('/base')\n\
             def base(): return True\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("mounts.py"),
            "from application import app\n\
             from routes import router\n\
             app.include_router(router, prefix='/api')\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("extra.py"),
            "from routes import router\n\
             @router.post('/extra')\n\
             def extra(): return True\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("lookalike.py"),
            "app = object()\n\
             router = object()\n\
             @router.get('/spoof')\n\
             def spoof(): return False\n\
             app.include_router(router)\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("spoof_mounts.py"),
            "from lookalike import app, router\napp.include_router(router)\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let mut routes = engine
            .snapshot()
            .unwrap()
            .symbols
            .into_iter()
            .filter(|symbol| symbol.kind == crate::model::SymbolKind::Route)
            .map(|symbol| symbol.name)
            .collect::<Vec<_>>();
        routes.sort();
        assert_eq!(routes, ["GET /api/items/base", "POST /api/items/extra"]);
        for route_name in routes {
            let route = engine
                .search(&route_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == route_name)
                .unwrap()
                .symbol;
            let callees = engine.callees(&route.id).unwrap();
            assert_eq!(callees.len(), 1);
            assert_eq!(callees[0].1.provenance, "framework/fastapi-route");
            assert_eq!(callees[0].1.confidence, 0.995);
        }
    }

    #[test]
    fn c_function_pointer_dispatch_resolves_tables_chains_and_evidence_sites() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("ops.h"),
            "typedef int (*handler_t)(int);\n\
             typedef struct Inner { handler_t run; } Inner;\n\
             typedef struct Outer { int tag; Inner *inner; } Outer;\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("dispatch.c"),
            "#include \"ops.h\"\n\
             static int alpha(int value) { return value + 1; }\n\
             static int beta(int value) { return value + 2; }\n\
             static Inner table[] = { { alpha }, { .run = beta } };\n\
             void install(Inner *ops) { ops->run = &alpha; }\n\
             int direct(Inner *ops, int value) { return ops->run(value); }\n\
             int chained(Outer *outer, int value) { return outer->inner->run(value); }\n\
             int twice(Inner *ops, int value) { return ops->run(value) + ops->run(value); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let symbols = snapshot
            .symbols
            .iter()
            .map(|symbol| (symbol.id.as_str(), symbol))
            .collect::<std::collections::HashMap<_, _>>();
        let edges = snapshot
            .relationships
            .iter()
            .filter(|edge| edge.evidence.provenance == "dynamic/c-function-pointer-dispatch")
            .map(|edge| {
                (
                    symbols[edge.source_id.as_str()].name.as_str(),
                    symbols[edge.target_id.as_str()].name.as_str(),
                    edge.evidence.site,
                )
            })
            .collect::<Vec<_>>();

        for source in ["direct", "chained"] {
            assert_eq!(
                edges
                    .iter()
                    .filter(|(actual, _, _)| *actual == source)
                    .map(|(_, target, _)| *target)
                    .collect::<std::collections::HashSet<_>>(),
                std::collections::HashSet::from(["alpha", "beta"])
            );
        }
        let twice = edges
            .iter()
            .filter(|(source, _, _)| *source == "twice")
            .collect::<Vec<_>>();
        assert_eq!(twice.len(), 4);
        assert_eq!(
            twice
                .iter()
                .map(|(_, _, site)| site.unwrap())
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2
        );
    }

    #[test]
    fn c_function_pointer_dispatch_fails_closed_and_cleans_up_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("one.h"),
            "typedef struct Ops { int (*run)(int); } Ops;\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("two.h"),
            "typedef struct Ops { int (*run)(int); } Ops;\n",
        )
        .unwrap();
        let source = temp.path().join("dispatch.c");
        fs::write(
            &source,
            "#include \"one.h\"\n\
             #include \"two.h\"\n\
             static int alpha(int value) { return value; }\n\
             void install(Ops *ops) { ops->run = alpha; }\n\
             int dispatch(Ops *ops, int value) { return ops->run(value); }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let edge_count = |engine: &Engine| {
            engine
                .snapshot()
                .unwrap()
                .relationships
                .iter()
                .filter(|edge| edge.evidence.provenance == "dynamic/c-function-pointer-dispatch")
                .count()
        };
        assert_eq!(edge_count(&engine), 0);

        fs::remove_file(temp.path().join("two.h")).unwrap();
        assert_eq!(engine.sync().unwrap().files_deleted, 1);
        assert_eq!(edge_count(&engine), 1);

        fs::write(
            &source,
            "#include \"one.h\"\n\
             static int alpha(int value) { return value; }\n\
             void install(Ops *ops) { ops->run = alpha; }\n\
             int dispatch(Ops *ops, int value) { return value; }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(edge_count(&engine), 0);
    }

    #[test]
    fn c_function_pointer_field_propagation_reaches_a_fixed_point_and_rejects_siblings() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("dispatch.c"),
            "typedef int (*Fn)(int);\n\
             typedef struct Hook { Fn func; int value; } Hook;\n\
             typedef struct Registry { Fn fn; int value; } Registry;\n\
             typedef struct Stage { Fn cb; } Stage;\n\
             static int leaf(int value) { return value + 1; }\n\
             void seed(Hook *hook) { hook->func = leaf; }\n\
             void wire(Hook *hook, Registry *registry, Stage *stage) {\n\
               registry->fn = hook->func;\n\
               stage->cb = registry->fn;\n\
               registry->value = hook->value;\n\
               registry->fn = stage->cb;\n\
             }\n\
             int through_registry(Registry *registry, int value) { return registry->fn(value); }\n\
             int through_stage(Stage *stage, int value) { return stage->cb(value); }\n\
             int non_pointer(Registry *registry) { return registry->value(); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let symbols = snapshot
            .symbols
            .iter()
            .map(|symbol| (symbol.id.as_str(), symbol.name.as_str()))
            .collect::<std::collections::HashMap<_, _>>();
        let edges = snapshot
            .relationships
            .iter()
            .filter(|edge| edge.evidence.provenance == "dynamic/c-function-pointer-dispatch")
            .map(|edge| {
                (
                    symbols[edge.source_id.as_str()],
                    symbols[edge.target_id.as_str()],
                )
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            edges,
            std::collections::HashSet::from([
                ("through_registry", "leaf"),
                ("through_stage", "leaf"),
            ])
        );
    }

    #[test]
    fn c_function_pointer_field_propagation_is_include_safe_and_incremental() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("hook.h"),
            "typedef int (*Fn)(int);\n\
             typedef struct Hook { Fn func; } Hook;\n\
             typedef struct Registry { Fn fn; } Registry;\n",
        )
        .unwrap();
        let source = temp.path().join("dispatch.c");
        let render = |register: bool| {
            format!(
                "#include \"hook.h\"\n\
                 static int leaf(int value) {{ return value; }}\n\
                 void seed(Hook *hook) {{ {} }}\n\
                 void wire(Hook *hook, Registry *registry) {{ registry->fn = hook->func; }}\n\
                 int dispatch(Registry *registry, int value) {{ return registry->fn(value); }}\n",
                if register {
                    "hook->func = leaf;"
                } else {
                    "(void)hook;"
                }
            )
        };
        fs::write(&source, render(true)).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let edge_count = |engine: &Engine| {
            engine
                .snapshot()
                .unwrap()
                .relationships
                .iter()
                .filter(|edge| edge.evidence.provenance == "dynamic/c-function-pointer-dispatch")
                .count()
        };
        assert_eq!(edge_count(&engine), 1);

        fs::write(&source, render(false)).unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(edge_count(&engine), 0);

        fs::write(&source, render(true)).unwrap();
        fs::write(
            temp.path().join("ambiguous.h"),
            "typedef int (*Fn)(int);\n\
             typedef struct Hook { Fn func; } Hook;\n",
        )
        .unwrap();
        fs::write(
            &source,
            render(true).replace(
                "#include \"hook.h\"",
                "#include \"hook.h\"\n#include \"ambiguous.h\"",
            ),
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 2);
        assert_eq!(edge_count(&engine), 0);
    }

    #[test]
    fn c_function_pointer_arrays_are_typedef_proven_file_local_and_site_exact() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("callbacks.c"),
            "typedef int (*callback_t)(int);\n\
             typedef int callback_fn(int);\n\
             typedef int data_t;\n\
             static int alpha(int value) { return value + 1; }\n\
             static int beta(int value) { return value + 2; }\n\
             static callback_t handlers[] = { alpha, [3] = (callback_t)beta };\n\
             static callback_fn *functions[] = { beta };\n\
             static data_t data[] = { alpha };\n\
             int run(int slot, int value) {\n\
               return handlers[slot](value) + handlers[slot + 1](value);\n\
             }\n\
             int run_function_type(int slot, int value) { return functions[slot](value); }\n\
             int not_a_dispatch(int slot) { return data[slot]; }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("other.c"),
            "typedef int (*callback_t)(int);\n\
             static int gamma(int value) { return value + 3; }\n\
             static callback_t handlers[] = { gamma };\n\
             int other_run(int slot, int value) { return handlers[slot](value); }\n",
        )
        .unwrap();

        let (engine, _) = Engine::init(temp.path()).unwrap();
        let snapshot = engine.snapshot().unwrap();
        let symbols = snapshot
            .symbols
            .iter()
            .map(|symbol| (symbol.id.as_str(), symbol))
            .collect::<std::collections::HashMap<_, _>>();
        let edges = snapshot
            .relationships
            .iter()
            .filter(|edge| edge.evidence.provenance == "dynamic/c-function-pointer-dispatch")
            .map(|edge| {
                (
                    symbols[edge.source_id.as_str()].name.as_str(),
                    symbols[edge.target_id.as_str()].name.as_str(),
                    edge.evidence.site.unwrap(),
                )
            })
            .collect::<Vec<_>>();

        let run = edges
            .iter()
            .filter(|(source, _, _)| *source == "run")
            .collect::<Vec<_>>();
        assert_eq!(run.len(), 4);
        assert_eq!(
            run.iter()
                .map(|(_, target, _)| *target)
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from(["alpha", "beta"])
        );
        assert_eq!(
            run.iter()
                .map(|(_, _, site)| *site)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2
        );
        assert_eq!(
            edges
                .iter()
                .filter(|(source, _, _)| *source == "run_function_type")
                .map(|(_, target, _)| *target)
                .collect::<Vec<_>>(),
            ["beta"]
        );
        assert_eq!(
            edges
                .iter()
                .filter(|(source, _, _)| *source == "other_run")
                .map(|(_, target, _)| *target)
                .collect::<Vec<_>>(),
            ["gamma"]
        );
        assert!(edges
            .iter()
            .all(|(source, _, _)| *source != "not_a_dispatch"));
    }

    #[test]
    fn c_function_pointer_formals_flow_from_callers_into_stored_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("callback.h"),
            "typedef int (*callback_t)(int);\n\
             typedef struct Box { callback_t callback; } Box;\n\
             void store_callback(Box *box, callback_t callback);\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("store.c"),
            "#include \"callback.h\"\n\
             void store_callback(Box *box, callback_t callback) { box->callback = callback; }\n\
             int invoke_callback(Box *box, int value) { return box->callback(value); }\n",
        )
        .unwrap();
        let caller = temp.path().join("caller.c");
        fs::write(
            &caller,
            "#include \"callback.h\"\n\
             static int real_handler(int value) { return value + 1; }\n\
             void wire(Box *box) { store_callback(box, real_handler); }\n",
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine| {
            let invoke = engine
                .search("invoke_callback", 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == "invoke_callback")
                .unwrap()
                .symbol;
            engine
                .callees(&invoke.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, _)| target.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(&engine), ["real_handler"]);

        fs::write(
            &caller,
            "#include \"callback.h\"\n\
             static int real_handler(int value) { return value + 1; }\n\
             void wire(Box *box) { (void)box; }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert!(targets(&engine).is_empty());
    }

    #[test]
    fn cpp_local_function_pointers_are_same_owner_address_proven_and_incremental() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("dispatch.cpp");
        fs::write(
            &source,
            "static int alpha(int value) { return value + 1; }\n\
             static int beta(int value) { return value + 2; }\n\
             int run(bool split, int value) {\n\
               auto local = &alpha;\n\
               if (split) local = &beta;\n\
               return local(value);\n\
             }\n\
             int sibling(int value) { return local(value); }\n",
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine, caller_name: &str| {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == caller_name)
                .unwrap()
                .symbol;
            engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, evidence)| (target.name, evidence.site.unwrap()))
                .collect::<Vec<_>>()
        };
        let run = targets(&engine, "run");
        assert_eq!(
            run.iter()
                .map(|(name, _)| name.as_str())
                .collect::<std::collections::HashSet<_>>(),
            std::collections::HashSet::from(["alpha", "beta"])
        );
        assert_eq!(
            run.iter()
                .map(|(_, site)| *site)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            1
        );
        assert!(targets(&engine, "sibling").is_empty());

        fs::write(
            &source,
            "static int alpha(int value) { return value + 1; }\n\
             static int beta(int value) { return value + 2; }\n\
             int run(bool split, int value) {\n\
               (void)split;\n\
               auto local = &beta;\n\
               return local(value);\n\
             }\n\
             int sibling(int value) { return local(value); }\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(
            targets(&engine, "run")
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["beta"]
        );
    }

    #[test]
    fn cpp_local_function_pointer_flow_respects_order_shadowing_and_kills() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("scopes.cpp"),
            "static int alpha(int value) { return value + 1; }\n\
             static int beta(int value) { return value + 2; }\n\
             static int local(int value) { return value + 3; }\n\
             int ordered(int value) {\n\
               int before = local(value);\n\
               auto local = &alpha;\n\
               return before + local(value);\n\
             }\n\
             int shadowed(int value) {\n\
               auto pointer = &alpha;\n\
               { auto pointer = &beta; value += pointer(value); }\n\
               return pointer(value);\n\
             }\n\
             int killed(int value) {\n\
               auto pointer = &alpha;\n\
               pointer = nullptr;\n\
               return pointer(value);\n\
             }\n\
             int rebound(int value) {\n\
               auto pointer = &alpha;\n\
               pointer = &beta;\n\
               return pointer(value);\n\
             }\n\
             static int fallback(int value) { return value + 4; }\n\
             int if_scoped(int value) {\n\
               if (auto fallback = &alpha; value) value = fallback(value);\n\
               return fallback(value);\n\
             }\n",
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let dynamic_targets = |caller_name: &str| {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == caller_name)
                .unwrap()
                .symbol;
            engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, evidence)| (evidence.line, target.name))
                .collect::<std::collections::HashSet<_>>()
        };

        assert_eq!(
            dynamic_targets("ordered"),
            std::collections::HashSet::from([(7, "alpha".to_owned())])
        );
        let ordered = engine
            .search("ordered", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "ordered")
            .unwrap()
            .symbol;
        assert!(engine
            .callees(&ordered.id)
            .unwrap()
            .into_iter()
            .any(|(target, evidence)| target.name == "local"
                && evidence.line == 5
                && evidence.provenance != "dynamic/c-function-pointer-dispatch"));
        assert_eq!(
            dynamic_targets("shadowed"),
            std::collections::HashSet::from([(11, "beta".to_owned()), (12, "alpha".to_owned())])
        );
        assert!(dynamic_targets("killed").is_empty());
        assert_eq!(
            dynamic_targets("rebound"),
            std::collections::HashSet::from([(22, "beta".to_owned())])
        );
        assert_eq!(
            dynamic_targets("if_scoped"),
            std::collections::HashSet::from([(26, "alpha".to_owned())])
        );
        let if_scoped = engine
            .search("if_scoped", 10)
            .unwrap()
            .into_iter()
            .find(|hit| hit.symbol.name == "if_scoped")
            .unwrap()
            .symbol;
        assert!(engine.callees(&if_scoped.id).unwrap().into_iter().any(
            |(target, evidence)| target.name == "fallback"
                && evidence.line == 27
                && evidence.provenance != "dynamic/c-function-pointer-dispatch"
        ));
    }

    #[test]
    fn cpp_function_pointer_factories_flow_to_scoped_local_dispatches() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("factory.cpp");
        let write_source = |choose_return: &str| {
            fs::write(
                &source,
                format!(
                    "static int alpha(int value) {{ return value + 1; }}\n\
                     static int beta(int value) {{ return value + 2; }}\n\
                     static int decoy(int value) {{ return value + 3; }}\n\
                     typedef int (*callback_t)(int);\n\
                     callback_t choose(bool split) {{ {choose_return} }}\n\
                     auto exact_factory() {{ return &alpha; }}\n\
                     int (*explicit_factory(bool split))(int) {{ return split ? &alpha : &beta; }}\n\
                     int scalar_factory() {{ return 7; }}\n\
                     callback_t unsafe_parameter(callback_t alpha) {{ return &alpha; }}\n\
                     callback_t unsafe_local() {{ callback_t alpha = nullptr; return &alpha; }}\n\
                     callback_t unsafe_uninitialized() {{ callback_t alpha; return &alpha; }}\n\
                     auto lambda_factory() {{ return []() {{ return &alpha; }}; }}\n\
                     int run(bool split, int value) {{\n\
                       auto pointer = choose(split);\n\
                       return pointer(value);\n\
                     }}\n\
                     int exact_run(int value) {{\n\
                       auto pointer = exact_factory();\n\
                       return pointer(value);\n\
                     }}\n\
                     int explicit_run(bool split, int value) {{\n\
                       auto pointer = explicit_factory(split);\n\
                       return pointer(value);\n\
                     }}\n\
                     int rejected(int value) {{\n\
                       auto pointer = scalar_factory();\n\
                       return pointer(value);\n\
                     }}\n\
                     int immediate(bool split, int value) {{\n\
                       return choose(split)(value);\n\
                     }}\n\
                     int rejected_parameter(int value) {{\n\
                       auto pointer = unsafe_parameter(&alpha);\n\
                       return pointer(value);\n\
                     }}\n\
                     int rejected_local(int value) {{\n\
                       auto pointer = unsafe_local();\n\
                       return pointer(value);\n\
                     }}\n\
                     int rejected_uninitialized(int value) {{\n\
                       auto pointer = unsafe_uninitialized();\n\
                       return pointer(value);\n\
                     }}\n\
                     int rejected_lambda(int value) {{\n\
                       return lambda_factory()(value);\n\
                     }}\n\
                     int killed_factory(bool split, int value) {{\n\
                       auto pointer = choose(split);\n\
                       pointer = scalar_factory();\n\
                       return pointer(value);\n\
                     }}\n"
                ),
            )
            .unwrap();
        };
        write_source("return (&decoy != nullptr) ? &alpha : &beta;");
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine, caller_name: &str| {
            let caller = engine
                .search(caller_name, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == caller_name)
                .unwrap()
                .symbol;
            engine
                .callees(&caller.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, evidence)| (target.name, evidence.confidence))
                .collect::<Vec<_>>()
        };

        for caller in ["run", "explicit_run"] {
            let mut actual = targets(&engine, caller);
            actual.sort_by(|left, right| left.0.cmp(&right.0));
            assert_eq!(
                actual,
                [("alpha".to_owned(), 0.97), ("beta".to_owned(), 0.97)]
            );
        }
        assert_eq!(targets(&engine, "exact_run"), [("alpha".to_owned(), 0.995)]);
        assert!(targets(&engine, "rejected").is_empty());
        let mut immediate = targets(&engine, "immediate");
        immediate.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            immediate,
            [("alpha".to_owned(), 0.97), ("beta".to_owned(), 0.97)]
        );
        assert!(targets(&engine, "rejected_parameter").is_empty());
        assert!(targets(&engine, "rejected_local").is_empty());
        assert!(targets(&engine, "rejected_uninitialized").is_empty());
        assert!(targets(&engine, "rejected_lambda").is_empty());
        assert!(targets(&engine, "killed_factory").is_empty());

        write_source("return &beta;");
        assert_eq!(engine.sync().unwrap().files_changed, 1);
        assert_eq!(targets(&engine, "run"), [("beta".to_owned(), 0.995)]);
        assert_eq!(targets(&engine, "immediate"), [("beta".to_owned(), 0.995)]);
    }

    #[test]
    fn compilation_database_disambiguates_angle_includes_and_refreshes_incrementally() {
        let temp = tempfile::tempdir().unwrap();
        for directory in [
            "src",
            "include-a",
            "include-b",
            "include-new",
            "nested/include-a",
        ] {
            fs::create_dir_all(temp.path().join(directory)).unwrap();
        }
        fs::write(
            temp.path().join("include-a/config.h"),
            "typedef struct Ops { int (*run)(int); } Ops;\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("include-b/config.h"),
            "typedef struct Ops { int data; int (*run)(int); } Ops;\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("nested/include-a/config.h"),
            "typedef struct Ops { int decoy; } Ops;\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("src/main.c"),
            "#include <config.h>\n\
             static int alpha(int value) { return value + 1; }\n\
             static Ops table = { alpha };\n\
             int dispatch(Ops *ops) { return ops->run(1); }\n",
        )
        .unwrap();
        let write_database = |includes: &[&str]| {
            let mut arguments = vec!["cc".to_owned()];
            arguments.extend(includes.iter().map(|include| format!("-I{include}")));
            arguments.extend(["-c".to_owned(), "src/main.c".to_owned()]);
            fs::write(
                temp.path().join("compile_commands.json"),
                serde_json::to_string(&serde_json::json!([{
                    "directory": temp.path(),
                    "file": "src/main.c",
                    "arguments": arguments
                }]))
                .unwrap(),
            )
            .unwrap();
        };
        write_database(&["include-a"]);
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine| {
            let dispatch = engine
                .search("dispatch", 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == "dispatch")
                .unwrap()
                .symbol;
            engine
                .callees(&dispatch.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, _)| target.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(&engine), ["alpha"]);

        write_database(&["include-b"]);
        assert_eq!(engine.sync().unwrap().files_changed, 4);
        assert!(targets(&engine).is_empty());

        write_database(&["include-a"]);
        assert_eq!(engine.sync().unwrap().files_changed, 4);
        assert_eq!(targets(&engine), ["alpha"]);
        assert_eq!(engine.sync().unwrap().files_changed, 0);

        write_database(&["include-new", "include-a"]);
        assert_eq!(engine.sync().unwrap().files_changed, 4);
        assert_eq!(targets(&engine), ["alpha"]);
        fs::write(
            temp.path().join("include-new/config.h"),
            "typedef struct Ops { int data; int (*run)(int); } Ops;\n",
        )
        .unwrap();
        assert_eq!(engine.sync().unwrap().files_changed, 5);
        assert!(targets(&engine).is_empty());
        fs::remove_file(temp.path().join("include-new/config.h")).unwrap();
        let restored = engine.sync().unwrap();
        assert_eq!(restored.files_changed, 4);
        assert_eq!(restored.files_deleted, 1);
        assert_eq!(targets(&engine), ["alpha"]);
    }

    #[test]
    fn c_include_resolution_preserves_quote_angle_and_fail_closed_semantics() {
        let dynamic_targets = |engine: &Engine| {
            let dispatch = engine
                .search("dispatch", 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == "dispatch")
                .unwrap()
                .symbol;
            engine
                .callees(&dispatch.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, _)| target.name)
                .collect::<Vec<_>>()
        };
        let local = tempfile::tempdir().unwrap();
        fs::create_dir_all(local.path().join("src")).unwrap();
        fs::write(
            local.path().join("config.h"),
            "typedef struct Ops { int data; int (*run)(int); } Ops;\n",
        )
        .unwrap();
        fs::write(
            local.path().join("src/config.h"),
            "typedef struct Ops { int (*run)(int); } Ops;\n",
        )
        .unwrap();
        let source = |delimiter: char| {
            let (open, close) = if delimiter == '"' {
                ('"', '"')
            } else {
                ('<', '>')
            };
            format!(
                "#include {open}config.h{close}\n\
                 static int alpha(int value) {{ return value + 1; }}\n\
                 static Ops table = {{ alpha }};\n\
                 int dispatch(Ops *ops) {{ return ops->run(1); }}\n"
            )
        };
        fs::write(local.path().join("src/main.c"), source('"')).unwrap();
        let (mut engine, _) = Engine::init(local.path()).unwrap();
        assert_eq!(dynamic_targets(&engine), ["alpha"]);

        fs::write(local.path().join("src/main.c"), source('<')).unwrap();
        engine.sync().unwrap();
        assert!(
            dynamic_targets(&engine).is_empty(),
            "an unmanaged angle include must not inherit quoted source-directory precedence"
        );

        let variants = tempfile::tempdir().unwrap();
        fs::create_dir_all(variants.path().join("src")).unwrap();
        fs::create_dir_all(variants.path().join("include-a")).unwrap();
        fs::write(
            variants.path().join("include-a/config.h"),
            "typedef struct Ops { int (*run)(int); } Ops;\n",
        )
        .unwrap();
        fs::write(
            variants.path().join("src/main.c"),
            "#include <config.h>\n\
             static int alpha(int value) { return value + 1; }\n\
             static Ops table = { alpha };\n\
             int dispatch(Ops *ops) { return ops->run(1); }\n",
        )
        .unwrap();
        fs::write(
            variants.path().join("compile_commands.json"),
            serde_json::to_string(&serde_json::json!([
                {
                    "directory": variants.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "-Iinclude-a", "src/main.c"]
                },
                {
                    "directory": variants.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "src/main.c"]
                }
            ]))
            .unwrap(),
        )
        .unwrap();
        let (engine, _) = Engine::init(variants.path()).unwrap();
        assert!(
            dynamic_targets(&engine).is_empty(),
            "a rejected build variant must not fall back to the sole global suffix match"
        );
    }

    #[cfg(unix)]
    #[test]
    fn compilation_database_fingerprint_tracks_include_symlink_targets() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        for directory in ["src", "include-a", "include-b"] {
            fs::create_dir_all(temp.path().join(directory)).unwrap();
        }
        fs::write(
            temp.path().join("include-a/config.h"),
            "typedef struct Ops { int (*run)(int); } Ops;\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("include-b/config.h"),
            "typedef struct Ops { int data; int (*run)(int); } Ops;\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("src/main.c"),
            "#include <config.h>\n\
             static int alpha(int value) { return value + 1; }\n\
             static Ops table = { alpha };\n\
             int dispatch(Ops *ops) { return ops->run(1); }\n",
        )
        .unwrap();
        symlink("include-a", temp.path().join("selected")).unwrap();
        fs::write(
            temp.path().join("compile_commands.json"),
            serde_json::to_string(&serde_json::json!([{
                "directory": temp.path(),
                "file": "src/main.c",
                "arguments": ["cc", "-Iselected", "src/main.c"]
            }]))
            .unwrap(),
        )
        .unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine| {
            let dispatch = engine
                .search("dispatch", 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == "dispatch")
                .unwrap()
                .symbol;
            engine
                .callees(&dispatch.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, _)| target.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(&engine), ["alpha"]);

        fs::remove_file(temp.path().join("selected")).unwrap();
        symlink("include-b", temp.path().join("selected")).unwrap();
        assert!(engine.sync().unwrap().files_changed >= 3);
        assert!(
            targets(&engine).is_empty(),
            "retargeting an include-directory symlink must invalidate persisted resolution"
        );
    }

    #[test]
    fn compilation_database_macros_are_per_tu_ordered_and_response_incremental() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::write(
            temp.path().join("src/a.c"),
            "typedef struct AOps { int (*run)(int); } AOps;\n\
             static int alpha(int v) { return v + 1; }\n\
             static int decoy(int v) { return v - 1; }\n\
             static AOps table = { SELECT };\n\
             int dispatch_a(AOps *ops) { return ops->run(1); }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("src/b.c"),
            "typedef struct BOps { int (*run)(int); } BOps;\n\
             static int beta(int v) { return v + 2; }\n\
             static int beta_second(int v) { return v + 3; }\n\
             static BOps table = { WRAP(SELECT) };\n\
             int dispatch_b(BOps *ops) { return ops->run(1); }\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("nested.rsp"),
            "-DWRAP(x)=x -DSELECT=beta\n",
        )
        .unwrap();
        fs::write(temp.path().join("b.rsp"), "@nested.rsp\n").unwrap();
        fs::write(
            temp.path().join("compile_commands.json"),
            serde_json::to_vec_pretty(&serde_json::json!([
                {
                    "directory": temp.path(),
                    "file": "src/a.c",
                    "arguments": [
                        "cc",
                        "-DSELECT=decoy",
                        "-U",
                        "SELECT",
                        "-D",
                        "SELECT=alpha",
                        "src/a.c"
                    ]
                },
                {
                    "directory": temp.path(),
                    "file": "src/b.c",
                    "arguments": ["ccache", "clang", "@b.rsp", "src/b.c"]
                }
            ]))
            .unwrap(),
        )
        .unwrap();

        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine, caller: &str| {
            let symbol = engine
                .search(caller, 10)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == caller)
                .unwrap()
                .symbol;
            engine
                .callees(&symbol.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, _)| target.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(&engine, "dispatch_a"), ["alpha"]);
        assert_eq!(targets(&engine, "dispatch_b"), ["beta"]);

        fs::write(
            temp.path().join("nested.rsp"),
            "-DWRAP(x)=x -DSELECT=beta_second\n",
        )
        .unwrap();
        assert!(
            engine.sync().unwrap().files_changed >= 2,
            "a response-only edit must invalidate affected preprocessing context"
        );
        assert_eq!(targets(&engine, "dispatch_a"), ["alpha"]);
        assert_eq!(targets(&engine, "dispatch_b"), ["beta_second"]);
        assert_eq!(engine.sync().unwrap().files_changed, 0);
    }

    #[test]
    fn c_macro_tables_and_preprocessor_guards_are_bounded_exact_and_incremental() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("defs.h"),
            "#define INCLUDED included_target\n\
             #define PASS(x) x\n\
             #define SLOT(x) .slot_run = PASS(x)\n\
             #define MAKE(x) { .whole_run = PASS(x) }\n",
        )
        .unwrap();
        let source = |object_target: &str| {
            format!(
                "#include \"defs.h\"\n\
                 typedef struct ObjectOps {{ int (*object_run)(int); }} ObjectOps;\n\
                 typedef struct SlotOps {{ int (*slot_run)(int); }} SlotOps;\n\
                 typedef struct WholeOps {{ int (*whole_run)(int); }} WholeOps;\n\
                 typedef struct IncludedOps {{ int (*included_run)(int); }} IncludedOps;\n\
                 typedef struct ConditionalOps {{ int (*conditional_run)(int); }} ConditionalOps;\n\
                 typedef struct KnownOps {{ int (*known_run)(int); }} KnownOps;\n\
                 typedef struct UnknownOps {{ int (*unknown_run)(int); }} UnknownOps;\n\
                 typedef struct UndefOps {{ int (*undef_run)(int); }} UndefOps;\n\
                 typedef struct RejectedOps {{ int (*rejected_run)(int); }} RejectedOps;\n\
                 static int object_first(int v) {{ return v + 1; }}\n\
                 static int object_second(int v) {{ return v + 2; }}\n\
                 static int slot_target(int v) {{ return v + 3; }}\n\
                 static int whole_target(int v) {{ return v + 4; }}\n\
                 static int included_target(int v) {{ return v + 5; }}\n\
                 static int included_second(int v) {{ return v + 6; }}\n\
                 static int inactive_target(int v) {{ return v + 7; }}\n\
                 static int active_target(int v) {{ return v + 8; }}\n\
                 static int known_target(int v) {{ return v + 9; }}\n\
                 static int known_decoy(int v) {{ return v - 9; }}\n\
                 static int unknown_yes(int v) {{ return v + 10; }}\n\
                 static int unknown_no(int v) {{ return v + 11; }}\n\
                 static int undef_first(int v) {{ return v + 10; }}\n\
                 static int undef_second(int v) {{ return v + 11; }}\n\
                 static int rejected_target(int v) {{ return v + 12; }}\n\
                 #define OBJECT {object_target}\n\
                 static ObjectOps object_table = {{ OBJECT }};\n\
                 static SlotOps slot_table = {{ SLOT(slot_target) }};\n\
                 static WholeOps whole_table = MAKE(whole_target);\n\
                 static IncludedOps included_table = {{ .included_run = INCLUDED }};\n\
                 #if 0\n\
                 static ConditionalOps inactive_table = {{ inactive_target }};\n\
                 #else\n\
                 static ConditionalOps active_table = {{ active_target }};\n\
                 #endif\n\
                 #define KNOWN 1\n\
                 #ifdef KNOWN\n\
                 static KnownOps known_table = {{ known_target }};\n\
                 #else\n\
                 static KnownOps known_bad = {{ known_decoy }};\n\
                 #endif\n\
                 #if EXTERNAL_FEATURE\n\
                 static UnknownOps unknown_a = {{ unknown_yes }};\n\
                 #else\n\
                 static UnknownOps unknown_b = {{ unknown_no }};\n\
                 #endif\n\
                 #define PICK undef_first\n\
                 #undef PICK\n\
                 #define PICK undef_second\n\
                 static UndefOps undef_table = {{ PICK }};\n\
                 #define WRONG(x,y) x\n\
                 #define PASTE(x) x ## _target\n\
                 #define RECURSE RECURSE\n\
                 static RejectedOps wrong = {{ WRONG(rejected_target) }};\n\
                 static RejectedOps pasted = {{ PASTE(rejected) }};\n\
                 static RejectedOps recursive = {{ RECURSE }};\n\
                 int dispatch_object(ObjectOps *ops) {{ return ops->object_run(1); }}\n\
                 int dispatch_slot(SlotOps *ops) {{ return ops->slot_run(1); }}\n\
                 int dispatch_whole(WholeOps *ops) {{ return ops->whole_run(1); }}\n\
                 int dispatch_included(IncludedOps *ops) {{ return ops->included_run(1); }}\n\
                 int dispatch_conditional(ConditionalOps *ops) {{ return ops->conditional_run(1); }}\n\
                 int dispatch_known(KnownOps *ops) {{ return ops->known_run(1); }}\n\
                 int dispatch_unknown(UnknownOps *ops) {{ return ops->unknown_run(1); }}\n\
                 int dispatch_undef(UndefOps *ops) {{ return ops->undef_run(1); }}\n\
                 int dispatch_rejected(RejectedOps *ops) {{ return ops->rejected_run(1); }}\n"
            )
        };
        fs::write(temp.path().join("main.c"), source("object_first")).unwrap();
        let (mut engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |engine: &Engine, caller: &str| {
            let symbol = engine
                .search(caller, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == caller)
                .unwrap()
                .symbol;
            engine
                .callees(&symbol.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, _)| target.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(&engine, "dispatch_object"), ["object_first"]);
        assert_eq!(targets(&engine, "dispatch_slot"), ["slot_target"]);
        assert_eq!(targets(&engine, "dispatch_whole"), ["whole_target"]);
        assert_eq!(targets(&engine, "dispatch_included"), ["included_target"]);
        assert_eq!(targets(&engine, "dispatch_conditional"), ["active_target"]);
        assert_eq!(targets(&engine, "dispatch_known"), ["known_target"]);
        assert_eq!(
            targets(&engine, "dispatch_unknown"),
            ["unknown_no", "unknown_yes"]
        );
        assert_eq!(targets(&engine, "dispatch_undef"), ["undef_second"]);
        assert!(targets(&engine, "dispatch_rejected").is_empty());

        fs::write(temp.path().join("main.c"), source("object_second")).unwrap();
        engine.sync().unwrap();
        assert_eq!(targets(&engine, "dispatch_object"), ["object_second"]);
        fs::write(
            temp.path().join("defs.h"),
            "#define INCLUDED included_second\n\
             #define PASS(x) x\n\
             #define SLOT(x) .slot_run = PASS(x)\n\
             #define MAKE(x) { .whole_run = PASS(x) }\n",
        )
        .unwrap();
        engine.sync().unwrap();
        assert_eq!(targets(&engine, "dispatch_included"), ["included_second"]);
        assert_eq!(engine.sync().unwrap().files_changed, 0);
    }

    #[test]
    fn c_macro_branch_correlation_rejects_impossible_tables_and_includes() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("inactive.h"),
            "#define INACTIVE_HEADER inactive_header_target\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("first-branch.h"),
            "#if 1\n#define FIRST_HEADER good_target\n#endif\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("second-branch.h"),
            "#if 0\n#define COLLIDING_HEADER_BAD impossible_target\n#endif\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("repeated.h"),
            "#ifdef REPEAT_SWITCH\n#define REPEATED repeated_second\n#else\n#define REPEATED repeated_first\n#endif\n",
        )
        .unwrap();
        fs::write(
            temp.path().join("main.c"),
            r#"
#include "first-branch.h"
#include "second-branch.h"
#undef REPEAT_SWITCH
#include "repeated.h"
#define REPEAT_SWITCH 1
#include "repeated.h"

typedef int (*Callback)(int);
typedef struct InactiveOps { int (*run)(int); } InactiveOps;
typedef struct ImpossibleOps { int (*run)(int); } ImpossibleOps;
typedef struct UndefOps { int (*run)(int); } UndefOps;
typedef struct EmptyOps { int (*run)(int); } EmptyOps;
typedef struct RowOps { int (*run)(int); } RowOps;
typedef struct CollisionOps { int (*run)(int); } CollisionOps;
typedef struct RepeatedOps { int (*run)(int); } RepeatedOps;
typedef struct ConstantOps { int (*run)(int); } ConstantOps;
typedef struct BooleanOps { int (*run)(int); } BooleanOps;
typedef struct FactoryTextOps { int (*run)(int); } FactoryTextOps;
typedef struct ConvergedOps { int (*run)(int); } ConvergedOps;
typedef struct MutatedConditionOps { int (*run)(int); } MutatedConditionOps;

static int good_target(int v) { return v + 1; }
static int impossible_target(int v) { return v + 2; }
static int inactive_header_target(int v) { return v + 3; }
static int array_first(int v) { return v + 4; }
static int array_second(int v) { return v + 5; }
static int row_first(int v) { return v + 6; }
static int row_second(int v) { return v + 7; }
static int repeated_first(int v) { return v + 8; }
static int repeated_second(int v) { return v + 9; }
static Callback make_callback(void) { return good_target; }

static CollisionOps collision = { COLLIDING_HEADER_BAD };
static RepeatedOps repeated = { REPEATED };

#if 0 /* deliberately disabled */
static ConstantOps commented_zero = { impossible_target };
#endif
#if 0 && EXTERNAL_FEATURE
static ConstantOps false_and_unknown = { impossible_target };
#endif
#if defined(EXTERNAL_FEATURE) && 0
static ConstantOps unknown_and_false = { impossible_target };
#endif
#if 0x0
static ConstantOps hexadecimal_zero = { impossible_target };
#endif
#if 00
static ConstantOps octal_zero = { impossible_target };
#endif
#if 0U
static ConstantOps unsigned_zero = { impossible_target };
#endif
#if 0L
static ConstantOps long_zero = { impossible_target };
#endif
#if -0
static ConstantOps negative_zero = { impossible_target };
#endif
#if 1 || EXTERNAL_FEATURE
static BooleanOps true_or_unknown = { good_target };
#endif
#if +1
static BooleanOps positive_one = { good_target };
#endif
#define GET_CALLBACK() make_callback()
static FactoryTextOps factory_text = { GET_CALLBACK() };

#if UNKNOWN_0
#define CONVERGED_0 1
#else
#define CONVERGED_0 1
#endif
#if UNKNOWN_1
#define CONVERGED_1 1
#else
#define CONVERGED_1 1
#endif
#if UNKNOWN_2
#define CONVERGED_2 1
#else
#define CONVERGED_2 1
#endif
#if UNKNOWN_3
#define CONVERGED_3 1
#else
#define CONVERGED_3 1
#endif
#if UNKNOWN_4
#define CONVERGED_4 1
#else
#define CONVERGED_4 1
#endif
#if UNKNOWN_5
#define CONVERGED_5 1
#else
#define CONVERGED_5 1
#endif
#define CONVERGED_TARGET good_target
static ConvergedOps converged = { CONVERGED_TARGET };

#define MUTATED_FLAG 1
#if MUTATED_FLAG
#undef MUTATED_FLAG
static MutatedConditionOps mutated_good = { good_target };
#else
static MutatedConditionOps mutated_bad = { impossible_target };
#endif

#if 0
#include "inactive.h"
#endif
static InactiveOps inactive_include = { INACTIVE_HEADER };

#if EXTERNAL_FEATURE
#define ONLY_A impossible_target
#else
#define ONLY_B ONLY_A
#endif
static ImpossibleOps impossible_branch = { ONLY_B };

#define FLAG 1
#undef FLAG
#ifdef FLAG
static UndefOps explicit_undef_bad = { impossible_target };
#else
static UndefOps explicit_undef_good = { good_target };
#endif

#if 1
static int ordinary_scalar;
#else
#define EMPTY_PRIMARY_BAD impossible_target
#endif
static EmptyOps empty_primary = { EMPTY_PRIMARY_BAD };

#define CALLBACKS { array_first, array_second }
static Callback callbacks[] = CALLBACKS;

#define ROWS { { row_first }, { row_second } }
static RowOps rows[] = ROWS;

int dispatch_inactive(InactiveOps *ops) { return ops->run(1); }
int dispatch_impossible(ImpossibleOps *ops) { return ops->run(1); }
int dispatch_undef(UndefOps *ops) { return ops->run(1); }
int dispatch_empty(EmptyOps *ops) { return ops->run(1); }
int dispatch_array(unsigned i) { return callbacks[i](1); }
int dispatch_rows(RowOps *ops) { return ops->run(1); }
int dispatch_collision(CollisionOps *ops) { return ops->run(1); }
int dispatch_repeated(RepeatedOps *ops) { return ops->run(1); }
int dispatch_constant(ConstantOps *ops) { return ops->run(1); }
int dispatch_boolean(BooleanOps *ops) { return ops->run(1); }
int dispatch_factory_text(FactoryTextOps *ops) { return ops->run(1); }
int dispatch_converged(ConvergedOps *ops) { return ops->run(1); }
int dispatch_mutated(MutatedConditionOps *ops) { return ops->run(1); }
"#,
        )
        .unwrap();
        let (engine, _) = Engine::init(temp.path()).unwrap();
        let targets = |caller: &str| {
            let symbol = engine
                .search(caller, 20)
                .unwrap()
                .into_iter()
                .find(|hit| hit.symbol.name == caller)
                .unwrap()
                .symbol;
            engine
                .callees(&symbol.id)
                .unwrap()
                .into_iter()
                .filter(|(_, evidence)| {
                    evidence.provenance == "dynamic/c-function-pointer-dispatch"
                })
                .map(|(target, _)| target.name)
                .collect::<Vec<_>>()
        };

        let inactive = targets("dispatch_inactive");
        assert!(
            inactive.is_empty(),
            "unexpected inactive targets: {inactive:?}"
        );
        let impossible = targets("dispatch_impossible");
        assert!(
            impossible.is_empty(),
            "unexpected impossible targets: {impossible:?}"
        );
        assert_eq!(targets("dispatch_undef"), ["good_target"]);
        assert!(targets("dispatch_empty").is_empty());
        assert_eq!(targets("dispatch_array"), ["array_first", "array_second"]);
        assert_eq!(targets("dispatch_rows"), ["row_first", "row_second"]);
        assert!(targets("dispatch_collision").is_empty());
        assert_eq!(targets("dispatch_repeated"), ["repeated_second"]);
        assert!(targets("dispatch_constant").is_empty());
        assert_eq!(targets("dispatch_boolean"), ["good_target"]);
        assert!(targets("dispatch_factory_text").is_empty());
        assert_eq!(targets("dispatch_converged"), ["good_target"]);
        assert_eq!(targets("dispatch_mutated"), ["good_target"]);
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
        for revision in 0..256 {
            fs::write(
                temp.path().join("main.ts"),
                format!("function afterSave{revision}() {{}}\n"),
            )
            .unwrap();
        }
        let report = report_receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert_eq!(report.files_changed, 1);
        thread::sleep(Duration::from_millis(150));
        stop.store(true, Ordering::Relaxed);
        let engine = handle.join().unwrap();
        let hits = engine.search("afterSave255", 10).unwrap();
        assert_eq!(hits[0].symbol.name, "afterSave255");
        let reconciliations = 1 + report_receiver.try_iter().count();
        assert!(
            reconciliations < 16,
            "256 writes should coalesce into bounded reconciliation work, got {reconciliations}"
        );
    }
}
