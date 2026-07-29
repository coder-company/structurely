use crate::budget::ResourceBudget;
use crate::model::{
    ArkuiBuilderFlowFacts, CCompilerMacroAction, CFunctionPointerBindingFact,
    CFunctionPointerFacts, CMacroInitializerRole, CPreprocessorEventFact, CPreprocessorEventKind,
    CPreprocessorGuardFact, CPreprocessorGuardKind, EventChannel, Evidence, FastApiFacts,
    FastApiRouterRef, FileFacts, Language, Relationship, RelationshipKind, SourceSpan, Symbol,
    SymbolKind, GRAPH_MODEL_VERSION,
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use std::{
    cell::Cell,
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    time::Instant,
};

const SCHEMA_VERSION: u32 = 2;
const WAL_AUTOCHECKPOINT_PAGES: u32 = 256;
const JOURNAL_SIZE_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const INLINE_CALLBACK_DEPTH_CAP: usize = 16;
const CALL_RESULT_DEPENDENT_CAP: usize = 100_000;

#[derive(Debug, thiserror::Error)]
#[error("graph changed concurrently; retry publication")]
pub(crate) struct ConcurrentPublication;

pub struct Store {
    connection: Connection,
    path: PathBuf,
    checkpoint_failure_injected: bool,
}

#[derive(Clone)]
enum InheritedMemberResolution {
    NoMatch,
    Ambiguous,
    Unique(String, String),
}

#[derive(Clone)]
enum ReceiverNominalResolution {
    NoMatch,
    Ambiguous,
    Unique(String),
}

type NominalType = (String, i64, String);
type NominalTypeMap = HashMap<(i64, String), Vec<NominalType>>;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub symbol: Symbol,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileSummary {
    pub path: String,
    pub language: String,
    pub symbols: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotFile {
    pub path: String,
    pub content_hash: String,
    pub language: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphSnapshot {
    pub graph_model_version: u32,
    pub files: Vec<SnapshotFile>,
    pub symbols: Vec<Symbol>,
    pub relationships: Vec<Relationship>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageMetrics {
    pub database_bytes: u64,
    pub wal_bytes: u64,
    pub wal_autocheckpoint_pages: u32,
    pub journal_size_limit_bytes: u64,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open graph database {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        connection.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES)?;
        connection.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
        let mut store = Self {
            connection,
            path: path.to_owned(),
            checkpoint_failure_injected: false,
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn metadata_value(&self, key: &str) -> Result<Option<String>> {
        self.connection
            .query_row("SELECT value FROM metadata WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn set_metadata_value(&mut self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO metadata(key,value) VALUES (?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            (key, value),
        )?;
        Ok(())
    }

    pub(crate) fn mark_empty_graph_current(&mut self, metadata: &[(&str, &str)]) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let file_count: u64 = tx.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        if file_count != 0 {
            return Err(ConcurrentPublication.into());
        }
        for (key, value) in metadata {
            tx.execute(
                "INSERT INTO metadata(key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                (key, value),
            )?;
        }
        tx.execute(
            "INSERT INTO metadata(key,value) VALUES ('graph_model_version',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [GRAPH_MODEL_VERSION.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn storage_metrics(&self) -> Result<StorageMetrics> {
        let wal_path = PathBuf::from(format!("{}-wal", self.path.display()));
        Ok(StorageMetrics {
            database_bytes: file_size(&self.path)?,
            wal_bytes: file_size(&wal_path)?,
            wal_autocheckpoint_pages: self.connection.pragma_query_value(
                None,
                "wal_autocheckpoint",
                |row| row.get(0),
            )?,
            journal_size_limit_bytes: self.connection.pragma_query_value(
                None,
                "journal_size_limit",
                |row| row.get(0),
            )?,
        })
    }

    fn migrate(&mut self) -> Result<()> {
        self.connection.execute_batch(
            "
            BEGIN IMMEDIATE;
            CREATE TABLE IF NOT EXISTS metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                content_hash TEXT NOT NULL,
                language TEXT NOT NULL,
                indexed_epoch INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS symbols (
                row_id INTEGER PRIMARY KEY,
                public_id TEXT NOT NULL UNIQUE,
                semantic_key TEXT NOT NULL UNIQUE,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                language TEXT NOT NULL,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                start_byte INTEGER NOT NULL,
                end_byte INTEGER NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS symbols_name_idx ON symbols(name);
            CREATE INDEX IF NOT EXISTS symbols_file_idx ON symbols(file_id);
            CREATE VIRTUAL TABLE IF NOT EXISTS symbol_search USING fts5(
                public_id UNINDEXED,
                name,
                qualified_name,
                file,
                segments,
                tokenize = 'unicode61 remove_diacritics 2'
            );
            CREATE TABLE IF NOT EXISTS relationships (
                id INTEGER PRIMARY KEY,
                source_public_id TEXT NOT NULL,
                target_public_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                provenance TEXT NOT NULL,
                confidence REAL NOT NULL CHECK(confidence >= 0 AND confidence <= 1),
                explanation TEXT NOT NULL,
                evidence_file TEXT NOT NULL,
                evidence_line INTEGER NOT NULL,
                evidence_site INTEGER NOT NULL DEFAULT 0,
                UNIQUE(source_public_id, target_public_id, kind, provenance, evidence_file, evidence_line, evidence_site)
            );
            CREATE INDEX IF NOT EXISTS relationships_source_idx
                ON relationships(source_public_id, kind);
            CREATE INDEX IF NOT EXISTS relationships_target_idx
                ON relationships(target_public_id, kind);
            CREATE TABLE IF NOT EXISTS unresolved_calls (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                caller_public_id TEXT NOT NULL,
                fallback_caller_public_id TEXT,
                callee_name TEXT NOT NULL,
                receiver_binding TEXT,
                receiver_type TEXT,
                receiver_call_start_byte INTEGER,
                target_file_hint TEXT,
                provenance TEXT NOT NULL DEFAULT 'tree-sitter/name-resolution',
                confidence REAL NOT NULL DEFAULT 1.0,
                explanation TEXT NOT NULL DEFAULT 'direct call expression',
                resolvable INTEGER NOT NULL DEFAULT 1,
                evidence_file TEXT NOT NULL,
                evidence_line INTEGER NOT NULL,
                start_byte INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS unresolved_calls_name_idx
                ON unresolved_calls(callee_name);
            CREATE TABLE IF NOT EXISTS callable_return_types (
                owner_public_id TEXT PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                type_name TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS callable_return_types_file_idx
                ON callable_return_types(file_id);
            CREATE TABLE IF NOT EXISTS callback_parameter_invocations (
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                owner_public_id TEXT NOT NULL,
                parameter_index INTEGER NOT NULL,
                PRIMARY KEY(file_id,owner_public_id,parameter_index)
            );
            CREATE TABLE IF NOT EXISTS python_callback_formals (
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                owner_public_id TEXT NOT NULL,
                formal_name TEXT NOT NULL,
                parameter_index INTEGER NOT NULL,
                PRIMARY KEY(file_id,owner_public_id,formal_name)
            );
            CREATE TABLE IF NOT EXISTS callback_parameter_delegations (
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                owner_public_id TEXT NOT NULL,
                parameter_index INTEGER NOT NULL,
                callee_name TEXT NOT NULL,
                argument_index INTEGER NOT NULL,
                evidence_line INTEGER NOT NULL,
                call_start_byte INTEGER NOT NULL,
                PRIMARY KEY(
                    file_id,owner_public_id,parameter_index,callee_name,
                    argument_index,evidence_line,call_start_byte
                )
            );
            CREATE TABLE IF NOT EXISTS callback_argument_batches (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                payload TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS callback_inline_symbols (
                public_id TEXT PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS callback_inline_symbols_file_idx
                ON callback_inline_symbols(file_id);
            CREATE TABLE IF NOT EXISTS arkui_builder_flow_batches (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                payload TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS fastapi_fact_batches (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                payload TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS c_function_pointer_fact_batches (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                payload TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS fastapi_generated_symbols (
                public_id TEXT PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS dynamic_events (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                owner_public_id TEXT NOT NULL,
                receiver TEXT NOT NULL,
                channel TEXT NOT NULL,
                channel_target_file_hint TEXT,
                channel_export_name TEXT,
                channel_member_path TEXT,
                action TEXT NOT NULL,
                callback_name TEXT,
                evidence_file TEXT NOT NULL,
                evidence_line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS dynamic_events_match_idx
                ON dynamic_events(file_id,receiver,channel,action);
            CREATE TABLE IF NOT EXISTS literal_bindings (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                export_name TEXT NOT NULL,
                member_path TEXT NOT NULL,
                channel TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS literal_bindings_lookup_idx
                ON literal_bindings(file_id,export_name,member_path);
            CREATE TABLE IF NOT EXISTS module_exports (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                export_name TEXT NOT NULL,
                target_file_hint TEXT NOT NULL,
                target_name TEXT NOT NULL,
                is_star INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS module_exports_lookup_idx
                ON module_exports(file_id,export_name,is_star);
            CREATE TABLE IF NOT EXISTS unresolved_references (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                source_public_id TEXT NOT NULL,
                target_name TEXT NOT NULL,
                binding_name TEXT NOT NULL DEFAULT '',
                target_file_hint TEXT,
                kind TEXT NOT NULL,
                provenance TEXT NOT NULL,
                confidence REAL NOT NULL,
                explanation TEXT NOT NULL,
                evidence_file TEXT NOT NULL,
                evidence_line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS unresolved_references_name_idx
                ON unresolved_references(target_name,kind);
            CREATE TABLE IF NOT EXISTS import_bindings (
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                binding_name TEXT NOT NULL,
                target_public_id TEXT NOT NULL,
                confidence REAL NOT NULL,
                PRIMARY KEY(file_id,binding_name,target_public_id)
            );
            CREATE INDEX IF NOT EXISTS import_bindings_name_idx
                ON import_bindings(file_id,binding_name);
            INSERT OR IGNORE INTO metadata(key, value) VALUES ('graph_epoch', '0');
            COMMIT;
            ",
        )?;
        let relationship_columns = self
            .connection
            .prepare("PRAGMA table_info(relationships)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        if !relationship_columns.contains("evidence_site") {
            self.connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE relationships RENAME TO relationships_v1;
                 CREATE TABLE relationships (
                    id INTEGER PRIMARY KEY,
                    source_public_id TEXT NOT NULL,
                    target_public_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    provenance TEXT NOT NULL,
                    confidence REAL NOT NULL CHECK(confidence >= 0 AND confidence <= 1),
                    explanation TEXT NOT NULL,
                    evidence_file TEXT NOT NULL,
                    evidence_line INTEGER NOT NULL,
                    evidence_site INTEGER NOT NULL DEFAULT 0,
                    UNIQUE(source_public_id,target_public_id,kind,provenance,evidence_file,evidence_line,evidence_site)
                 );
                 INSERT INTO relationships(
                    id,source_public_id,target_public_id,kind,provenance,confidence,
                    explanation,evidence_file,evidence_line,evidence_site
                 )
                 SELECT id,source_public_id,target_public_id,kind,provenance,confidence,
                    explanation,evidence_file,evidence_line,0
                 FROM relationships_v1;
                 DROP TABLE relationships_v1;
                 CREATE INDEX relationships_source_idx
                    ON relationships(source_public_id,kind);
                 CREATE INDEX relationships_target_idx
                    ON relationships(target_public_id,kind);
                 COMMIT;",
            )?;
        }
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "fallback_caller_public_id",
            "TEXT",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "receiver_call_start_byte",
            "INTEGER",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "receiver_binding",
            "TEXT",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "receiver_type",
            "TEXT",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "target_file_hint",
            "TEXT",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "provenance",
            "TEXT NOT NULL DEFAULT 'tree-sitter/name-resolution'",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "confidence",
            "REAL NOT NULL DEFAULT 1.0",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "explanation",
            "TEXT NOT NULL DEFAULT 'direct call expression'",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "resolvable",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "start_byte",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::ensure_column(
            &self.connection,
            "unresolved_references",
            "binding_name",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        Self::ensure_column(
            &self.connection,
            "dynamic_events",
            "channel_target_file_hint",
            "TEXT",
        )?;
        Self::ensure_column(
            &self.connection,
            "dynamic_events",
            "channel_export_name",
            "TEXT",
        )?;
        Self::ensure_column(
            &self.connection,
            "dynamic_events",
            "channel_member_path",
            "TEXT",
        )?;
        Self::ensure_search_schema(&self.connection)?;
        Self::ensure_column(
            &self.connection,
            "unresolved_references",
            "target_file_hint",
            "TEXT",
        )?;
        self.connection
            .execute_batch("DROP TABLE IF EXISTS callback_arguments;")?;
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        self.connection.execute(
            "INSERT OR IGNORE INTO metadata(key, value) VALUES ('graph_model_version', ?1)",
            [GRAPH_MODEL_VERSION.to_string()],
        )?;
        self.connection.execute_batch(
            "
            DROP TABLE IF EXISTS main.resolved_call_targets;
            DROP TABLE IF EXISTS temp.resolved_call_targets;
            CREATE TEMP TABLE resolved_call_targets (
                call_id INTEGER NOT NULL,
                target_public_id TEXT NOT NULL,
                target_qualified_name TEXT NOT NULL,
                resolution_confidence REAL NOT NULL,
                resolution_scope TEXT NOT NULL,
                relationship_explanation TEXT NOT NULL,
                PRIMARY KEY(call_id,target_public_id)
            ) WITHOUT ROWID;
            ",
        )?;
        Ok(())
    }

    fn ensure_search_schema(connection: &Connection) -> Result<()> {
        let mut statement = connection.prepare("PRAGMA table_info(symbol_search)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|column| column == "segments") {
            connection.execute_batch(
                "
                DROP TABLE symbol_search;
                CREATE VIRTUAL TABLE symbol_search USING fts5(
                    public_id UNINDEXED,
                    name,
                    qualified_name,
                    file,
                    segments,
                    tokenize = 'unicode61 remove_diacritics 2'
                );
                ",
            )?;
        }
        Ok(())
    }

    fn ensure_column(
        connection: &Connection,
        table: &str,
        column: &str,
        declaration: &str,
    ) -> Result<()> {
        let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !columns.iter().any(|existing| existing == column) {
            connection.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {declaration}"),
                [],
            )?;
        }
        Ok(())
    }

    pub fn epoch(&self) -> Result<u64> {
        let value: String = self.connection.query_row(
            "SELECT value FROM metadata WHERE key = 'graph_epoch'",
            [],
            |row| row.get(0),
        )?;
        Ok(value.parse()?)
    }

    pub fn is_current_graph_model(&self) -> Result<bool> {
        let version: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM metadata WHERE key = 'graph_model_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(version.as_deref().and_then(|value| value.parse().ok()) == Some(GRAPH_MODEL_VERSION))
    }

    pub fn content_hash(&self, path: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT content_hash FROM files WHERE path = ?1",
                [path],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn indexed_files(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM files ORDER BY path")?;
        let paths = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(paths)
    }

    pub(crate) fn indexed_file_hashes(&self) -> Result<HashMap<String, String>> {
        let mut statement = self
            .connection
            .prepare("SELECT path, content_hash FROM files ORDER BY path")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<HashMap<_, _>>>()
            .map_err(Into::into)
    }

    pub fn file_summaries(&self) -> Result<Vec<FileSummary>> {
        let mut statement = self.connection.prepare(
            "SELECT f.path,f.language,COUNT(s.row_id)
             FROM files f LEFT JOIN symbols s ON s.file_id=f.id
             GROUP BY f.id,f.path,f.language ORDER BY f.path",
        )?;
        let summaries = statement
            .query_map([], |row| {
                Ok(FileSummary {
                    path: row.get(0)?,
                    language: row.get(1)?,
                    symbols: row.get::<_, i64>(2)? as usize,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(summaries)
    }

    pub fn snapshot(&self) -> Result<GraphSnapshot> {
        let mut file_statement = self
            .connection
            .prepare("SELECT path,content_hash,language FROM files ORDER BY path")?;
        let files = file_statement
            .query_map([], |row| {
                Ok(SnapshotFile {
                    path: row.get(0)?,
                    content_hash: row.get(1)?,
                    language: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(file_statement);

        let mut symbol_statement = self.connection.prepare(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line
             FROM symbols s JOIN files f ON f.id=s.file_id
             ORDER BY s.public_id",
        )?;
        let symbols = symbol_statement
            .query_map([], Self::symbol_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(symbol_statement);

        let mut relationship_statement = self.connection.prepare(
            "SELECT source_public_id,target_public_id,kind,provenance,confidence,
                    explanation,evidence_file,evidence_line,evidence_site
             FROM relationships
             ORDER BY source_public_id,target_public_id,kind,provenance,evidence_file,
                      evidence_line,evidence_site",
        )?;
        let relationships = relationship_statement
            .query_map([], |row| {
                Ok(Relationship {
                    source_id: row.get(0)?,
                    target_id: row.get(1)?,
                    kind: parse_relationship_kind(&row.get::<_, String>(2)?),
                    evidence: Evidence {
                        provenance: row.get(3)?,
                        confidence: row.get(4)?,
                        explanation: row.get(5)?,
                        file: row.get(6)?,
                        line: row.get::<_, i64>(7)? as usize,
                        site: (row.get::<_, i64>(8)? != 0)
                            .then(|| row.get::<_, i64>(8).unwrap_or_default() as usize),
                    },
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(GraphSnapshot {
            graph_model_version: GRAPH_MODEL_VERSION,
            files,
            symbols,
            relationships,
        })
    }

    pub fn symbols_in_file(&self, file: &str) -> Result<Vec<Symbol>> {
        let mut statement = self.connection.prepare(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE f.path=?1 ORDER BY s.start_byte,s.end_byte",
        )?;
        let rows = statement.query_map([file], Self::symbol_from_row)?;
        let symbols = Self::collect_symbols(rows)?;
        Ok(symbols)
    }

    pub(crate) fn publish<I>(
        &mut self,
        facts: I,
        deleted: &[String],
        metadata: &[(&str, &str)],
    ) -> Result<(u64, usize, usize, u128, u128, Option<String>)>
    where
        I: IntoIterator<Item = Result<FileFacts>>,
    {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_epoch: String = tx.query_row(
            "SELECT value FROM metadata WHERE key = 'graph_epoch'",
            [],
            |row| row.get(0),
        )?;
        let next_epoch = current_epoch
            .parse::<u64>()?
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("graph epoch overflow"))?;
        let staging_started = Instant::now();
        for path in deleted {
            Self::delete_file(&tx, path)?;
        }
        let mut symbols_changed = 0;
        for file in facts {
            let file = file?;
            symbols_changed += file.symbols.len();
            Self::replace_file(&tx, &file, next_epoch)?;
        }
        let staging_ms = staging_started.elapsed().as_millis();
        let resolution_started = Instant::now();
        let relationships_resolved = Self::finish_epoch(&tx, next_epoch)?;
        for (key, value) in metadata {
            tx.execute(
                "INSERT INTO metadata(key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                (key, value),
            )?;
        }
        let resolution_ms = resolution_started.elapsed().as_millis();
        tx.commit()?;
        let checkpoint: Result<(u32, u32, u32)> =
            if std::mem::take(&mut self.checkpoint_failure_injected) {
                Err(anyhow::anyhow!("injected post-commit checkpoint failure"))
            } else {
                self.connection
                    .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                    .map_err(anyhow::Error::from)
            };
        let maintenance_warning = match checkpoint {
            Ok((0, _, _)) => None,
            Ok((busy, log_pages, checkpointed_pages)) => Some(format!(
                "graph committed; WAL checkpoint deferred: busy={busy}, \
                 log_pages={log_pages}, checkpointed_pages={checkpointed_pages}"
            )),
            Err(error) => Some(format!("graph committed; WAL checkpoint deferred: {error}")),
        };
        Ok((
            next_epoch,
            relationships_resolved,
            symbols_changed,
            staging_ms,
            resolution_ms,
            maintenance_warning,
        ))
    }

    #[cfg(test)]
    pub(crate) fn inject_checkpoint_failure_once(&mut self) {
        self.checkpoint_failure_injected = true;
    }

    fn finish_epoch(tx: &Transaction<'_>, next_epoch: u64) -> Result<usize> {
        Self::clear_inline_callback_symbols(tx)?;
        Self::clear_fastapi_generated_symbols(tx)?;
        let mut relationships_resolved = Self::resolve_calls(tx)?;
        let _ = Self::resolve_callback_arguments(tx)?;
        relationships_resolved += Self::publish_deferred_inline_calls(tx)?;
        relationships_resolved += tx.query_row(
            "SELECT COUNT(*) FROM relationships
             WHERE provenance IN (
                 'dynamic/callback-argument',
                 'dynamic/callback-delegation',
                 'dynamic/callback-inline'
             )",
            [],
            |row| row.get::<_, usize>(0),
        )?;
        relationships_resolved += Self::resolve_arkui_builder_flows(tx)?;
        relationships_resolved += Self::resolve_interface_dispatch(tx)?;
        relationships_resolved += Self::resolve_dynamic_events(tx)?;
        relationships_resolved += Self::resolve_c_function_pointer_dispatch(tx)?;
        relationships_resolved += Self::materialize_fastapi_routes(tx)?;
        tx.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'graph_epoch'",
            [next_epoch.to_string()],
        )?;
        tx.execute(
            "INSERT INTO metadata(key,value) VALUES ('graph_model_version',?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [GRAPH_MODEL_VERSION.to_string()],
        )?;
        Ok(relationships_resolved)
    }

    fn clear_fastapi_generated_symbols(tx: &Transaction<'_>) -> Result<()> {
        tx.execute(
            "DELETE FROM relationships
             WHERE source_public_id IN (SELECT public_id FROM fastapi_generated_symbols)
                OR target_public_id IN (SELECT public_id FROM fastapi_generated_symbols)
                OR provenance IN ('framework/fastapi-route','framework/fastapi-dependency')",
            [],
        )?;
        tx.execute(
            "DELETE FROM symbol_search
             WHERE public_id IN (SELECT public_id FROM fastapi_generated_symbols)",
            [],
        )?;
        tx.execute(
            "DELETE FROM symbols
             WHERE public_id IN (SELECT public_id FROM fastapi_generated_symbols)",
            [],
        )?;
        tx.execute("DELETE FROM fastapi_generated_symbols", [])?;
        Ok(())
    }

    fn clear_inline_callback_symbols(tx: &Transaction<'_>) -> Result<()> {
        tx.execute(
            "DELETE FROM relationships
             WHERE source_public_id IN (SELECT public_id FROM callback_inline_symbols)
                OR target_public_id IN (SELECT public_id FROM callback_inline_symbols)
                OR provenance IN (
                    'dynamic/callback-argument',
                    'dynamic/callback-delegation',
                    'dynamic/callback-inline'
                )",
            [],
        )?;
        tx.execute(
            "DELETE FROM symbol_search
             WHERE public_id IN (SELECT public_id FROM callback_inline_symbols)",
            [],
        )?;
        tx.execute(
            "DELETE FROM symbols
             WHERE public_id IN (SELECT public_id FROM callback_inline_symbols)",
            [],
        )?;
        tx.execute("DELETE FROM callback_inline_symbols", [])?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn inject_rolled_back_publish(
        &mut self,
        facts: &[FileFacts],
        deleted: &[String],
    ) -> Result<()> {
        self.inject_rolled_back_publish_with_metadata(facts, deleted, &[])
    }

    #[cfg(test)]
    pub(crate) fn inject_rolled_back_publish_with_metadata(
        &mut self,
        facts: &[FileFacts],
        deleted: &[String],
        metadata: &[(&str, &str)],
    ) -> Result<()> {
        let next_epoch = self.epoch()? + 1;
        let tx = self.connection.transaction()?;
        for path in deleted {
            Self::delete_file(&tx, path)?;
        }
        for file in facts {
            Self::replace_file(&tx, file, next_epoch)?;
        }
        Self::finish_epoch(&tx, next_epoch)?;
        for (key, value) in metadata {
            tx.execute(
                "INSERT INTO metadata(key,value) VALUES (?1,?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                (key, value),
            )?;
        }
        tx.rollback()?;
        Ok(())
    }

    fn delete_file(tx: &Transaction<'_>, path: &str) -> Result<()> {
        tx.execute("DELETE FROM symbol_search WHERE file = ?1", [path])?;
        tx.execute(
            "DELETE FROM relationships WHERE evidence_file = ?1
             OR source_public_id IN (
                 SELECT public_id FROM symbols s JOIN files f ON f.id=s.file_id WHERE f.path=?1
             )
             OR target_public_id IN (
                 SELECT public_id FROM symbols s JOIN files f ON f.id=s.file_id WHERE f.path=?1
             )",
            [path],
        )?;
        tx.execute("DELETE FROM files WHERE path = ?1", [path])?;
        Ok(())
    }

    fn replace_file(tx: &Transaction<'_>, file: &FileFacts, epoch: u64) -> Result<()> {
        Self::delete_file(tx, &file.path)?;
        tx.prepare_cached(
            "INSERT INTO files(path, content_hash, language, indexed_epoch) VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![
            file.path,
            file.content_hash,
            file.language.to_string(),
            epoch
        ])?;
        let file_id = tx.last_insert_rowid();
        for symbol in &file.symbols {
            tx.prepare_cached(
                "INSERT INTO symbols(
                    public_id, semantic_key, file_id, language, kind, name, qualified_name,
                    start_byte, end_byte, start_line, end_line
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?
            .execute(params![
                symbol.id,
                symbol.semantic_key,
                file_id,
                symbol.language.to_string(),
                symbol.kind.to_string(),
                symbol.name,
                symbol.qualified_name,
                symbol.start_byte as i64,
                symbol.end_byte as i64,
                symbol.start_line as i64,
                symbol.end_line as i64
            ])?;
            tx.prepare_cached(
                "INSERT INTO symbol_search(public_id, name, qualified_name, file, segments)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?
            .execute(params![
                symbol.id,
                symbol.name,
                symbol.qualified_name,
                symbol.file,
                identifier_segments(&format!("{} {}", symbol.name, symbol.qualified_name))
                    .join(" ")
            ])?;
        }
        for relationship in &file.relationships {
            Self::insert_relationship(tx, relationship)?;
        }
        for call in &file.unresolved_calls {
            tx.prepare_cached(
                "INSERT INTO unresolved_calls(
                    file_id,caller_public_id,fallback_caller_public_id,
                    callee_name,receiver_binding,receiver_type,receiver_call_start_byte,
                    target_file_hint,provenance,confidence,explanation,resolvable,
                    evidence_file,evidence_line,start_byte
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            )?
            .execute(params![
                file_id,
                call.caller_id,
                call.fallback_caller_id,
                call.callee_name,
                call.receiver_binding,
                call.receiver_type,
                call.receiver_call_start_byte.map(|value| value as i64),
                call.target_file_hint,
                call.provenance,
                call.confidence,
                call.explanation,
                call.resolvable,
                call.file,
                call.line as i64,
                call.start_byte as i64
            ])?;
        }
        for callable_return in &file.callable_returns {
            tx.prepare_cached(
                "INSERT INTO callable_return_types(owner_public_id,file_id,type_name)
                 VALUES (?1,?2,?3)",
            )?
            .execute(params![
                callable_return.owner_id,
                file_id,
                callable_return.type_name
            ])?;
        }
        for invocation in &file.callback_parameter_invocations {
            tx.prepare_cached(
                "INSERT INTO callback_parameter_invocations(
                    file_id,owner_public_id,parameter_index
                 ) VALUES (?1,?2,?3)",
            )?
            .execute(params![
                file_id,
                invocation.owner_id,
                invocation.parameter_index as i64
            ])?;
        }
        for formal in &file.python_callback_formals {
            tx.prepare_cached(
                "INSERT INTO python_callback_formals(
                    file_id,owner_public_id,formal_name,parameter_index
                 ) VALUES (?1,?2,?3,?4)",
            )?
            .execute(params![
                file_id,
                formal.owner_id,
                formal.formal_name,
                formal.parameter_index as i64
            ])?;
        }
        for delegation in &file.callback_parameter_delegations {
            tx.prepare_cached(
                "INSERT INTO callback_parameter_delegations(
                    file_id,owner_public_id,parameter_index,callee_name,
                    argument_index,evidence_line,call_start_byte
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            )?
            .execute(params![
                file_id,
                delegation.owner_id,
                delegation.parameter_index as i64,
                delegation.callee_name,
                delegation.argument_index as i64,
                delegation.line as i64,
                delegation.call_start_byte as i64
            ])?;
        }
        if !file.callback_arguments.is_empty() {
            let arguments = file
                .callback_arguments
                .iter()
                .map(|argument| {
                    (
                        argument.caller_id.as_str(),
                        argument.callee_name.as_str(),
                        argument.argument_index,
                        argument.formal_name.as_deref(),
                        argument.target_name.as_str(),
                        argument.target_qualified_hint.as_deref(),
                        argument.target_symbol.as_ref().map(|symbol| {
                            (
                                symbol.language,
                                symbol.id.as_str(),
                                symbol.semantic_key.as_str(),
                                symbol.name.as_str(),
                                symbol.qualified_name.as_str(),
                                symbol.start_byte,
                                symbol.end_byte,
                                symbol.start_line,
                                symbol.end_line,
                            )
                        }),
                        argument.line,
                        argument.call_start_byte,
                    )
                })
                .collect::<Vec<_>>();
            let payload = serde_json::to_string(&arguments)?;
            tx.prepare_cached(
                "INSERT INTO callback_argument_batches(file_id,payload) VALUES (?1,?2)",
            )?
            .execute(params![file_id, payload])?;
        }
        if !file.arkui_builder_flow.builders.is_empty()
            || !file.arkui_builder_flow.params.is_empty()
            || !file.arkui_builder_flow.invocations.is_empty()
            || !file.arkui_builder_flow.assignments.is_empty()
        {
            let payload = serde_json::to_string(&file.arkui_builder_flow)?;
            tx.prepare_cached(
                "INSERT INTO arkui_builder_flow_batches(file_id,payload) VALUES (?1,?2)",
            )?
            .execute(params![file_id, payload])?;
        }
        if !file.fastapi.routers.is_empty()
            || !file.fastapi.aliases.is_empty()
            || !file.fastapi.factories.is_empty()
            || !file.fastapi.mounts.is_empty()
            || !file.fastapi.routes.is_empty()
            || !file.fastapi.dependencies.is_empty()
            || !file.fastapi.dependency_aliases.is_empty()
            || !file.fastapi.dependency_factories.is_empty()
            || !file.fastapi.dependency_type_aliases.is_empty()
        {
            let payload = serde_json::to_string(&file.fastapi)?;
            tx.prepare_cached("INSERT INTO fastapi_fact_batches(file_id,payload) VALUES (?1,?2)")?
                .execute(params![file_id, payload])?;
        }
        if !file.c_function_pointers.typedefs.is_empty()
            || !file.c_function_pointers.layouts.is_empty()
            || !file.c_function_pointers.bindings.is_empty()
            || !file.c_function_pointers.propagations.is_empty()
            || !file.c_function_pointers.dispatches.is_empty()
            || !file.c_function_pointers.arrays.is_empty()
            || !file.c_function_pointers.array_dispatches.is_empty()
            || !file.c_function_pointers.formal_storages.is_empty()
            || !file.c_function_pointers.arguments.is_empty()
            || !file.c_function_pointers.local_bindings.is_empty()
            || !file.c_function_pointers.local_dispatches.is_empty()
            || !file.c_function_pointers.returns.is_empty()
            || !file.c_function_pointers.factory_dispatches.is_empty()
            || !file.c_function_pointers.includes.is_empty()
            || !file.c_function_pointers.preprocessor_guards.is_empty()
            || !file.c_function_pointers.preprocessor_events.is_empty()
            || !file.c_function_pointers.macro_initializers.is_empty()
            || !file.c_function_pointers.compiler_macro_contexts.is_empty()
        {
            let payload = serde_json::to_string(&file.c_function_pointers)?;
            tx.prepare_cached(
                "INSERT INTO c_function_pointer_fact_batches(file_id,payload) VALUES (?1,?2)",
            )?
            .execute(params![file_id, payload])?;
        }
        for event in &file.dynamic_events {
            let (channel, target_file_hint, export_name, member_path) = match &event.channel {
                EventChannel::Canonical(channel) => (channel.as_str(), None, None, None),
                EventChannel::Imported {
                    target_file_hint,
                    export_name,
                    member_path,
                } => (
                    "",
                    Some(target_file_hint.as_str()),
                    Some(export_name.as_str()),
                    Some(member_path.as_str()),
                ),
            };
            tx.prepare_cached(
                "INSERT INTO dynamic_events(
                    file_id,owner_public_id,receiver,channel,channel_target_file_hint,
                    channel_export_name,channel_member_path,action,callback_name,
                    evidence_file,evidence_line
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?
            .execute(params![
                file_id,
                event.owner_id,
                event.receiver,
                channel,
                target_file_hint,
                export_name,
                member_path,
                event.action.to_string(),
                event.callback_name,
                event.file,
                event.line as i64
            ])?;
        }
        for binding in &file.literal_bindings {
            tx.prepare_cached(
                "INSERT INTO literal_bindings(
                    file_id,export_name,member_path,channel
                 ) VALUES (?1,?2,?3,?4)",
            )?
            .execute(params![
                file_id,
                binding.export_name,
                binding.member_path,
                binding.channel
            ])?;
        }
        for export in &file.module_exports {
            tx.prepare_cached(
                "INSERT INTO module_exports(
                    file_id,export_name,target_file_hint,target_name,is_star
                 ) VALUES (?1,?2,?3,?4,?5)",
            )?
            .execute(params![
                file_id,
                export.export_name,
                export.target_file_hint,
                export.target_name,
                export.is_star
            ])?;
        }
        for reference in &file.unresolved_references {
            tx.prepare_cached(
                "INSERT INTO unresolved_references(
                    file_id,source_public_id,target_name,binding_name,target_file_hint,
                    kind,provenance,confidence,explanation,evidence_file,evidence_line
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?
            .execute(params![
                file_id,
                reference.source_id,
                reference.target_name,
                reference.binding_name,
                reference.target_file_hint,
                reference.kind.to_string(),
                reference.provenance,
                reference.confidence,
                reference.explanation,
                reference.file,
                reference.line as i64
            ])?;
        }
        Ok(())
    }

    fn resolve_calls(tx: &Transaction<'_>) -> Result<usize> {
        const CALL_FANOUT_CAP: usize = 6;
        let mut resolved = Self::resolve_structural_references(tx)?;
        tx.execute("DELETE FROM resolved_call_targets", [])?;
        tx.execute(
            "DELETE FROM relationships
             WHERE provenance IN (
                'tree-sitter/name-resolution',
                'tree-sitter/callback-registration',
                'framework/express-route',
                'framework/fastapi-route',
                'framework/react-router',
                'framework/arkui-route',
                'framework/ohos-emitter-registration',
                'framework/ohos-emitter',
                'dynamic/event-registration'
             )",
            [],
        )?;
        let mut calls_statement = tx.prepare(
            "SELECT u.id,COALESCE(primary_symbol.public_id,fallback_symbol.public_id),
                    u.callee_name,u.receiver_binding,u.receiver_type,
                    u.target_file_hint,u.provenance,u.confidence,u.explanation,
                    u.evidence_file,u.evidence_line,
                    COALESCE(primary_symbol.language,fallback_symbol.language),
                    u.file_id,u.resolvable,u.start_byte,u.fallback_caller_public_id,
                    u.receiver_call_start_byte
             FROM unresolved_calls u
             LEFT JOIN symbols primary_symbol
                    ON primary_symbol.public_id=u.caller_public_id
             LEFT JOIN symbols fallback_symbol
                    ON fallback_symbol.public_id=u.fallback_caller_public_id
             WHERE primary_symbol.public_id IS NOT NULL
                OR fallback_symbol.public_id IS NOT NULL
             ORDER BY u.evidence_file,u.evidence_line,u.id",
        )?;
        let mut calls = calls_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, f64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)? as usize,
                    row.get::<_, String>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, bool>(13)?,
                    row.get::<_, i64>(14)? as usize,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, Option<i64>>(16)?.map(|value| value as usize),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(calls_statement);
        calls.sort_by_key(|call| call.16.is_some());
        let receiver_callsite_keys = calls
            .iter()
            .filter_map(|call| {
                call.16
                    .map(|receiver_start| (call.12, call.1.clone(), receiver_start))
            })
            .collect::<HashSet<_>>();
        let resolve_call_results =
            call_result_resolution_enabled(calls.iter().filter(|call| call.16.is_some()).count());

        type Candidate = (String, String, i64, String);
        let mut direct_candidates = HashMap::<(String, String), Vec<Candidate>>::new();
        let mut local_candidates = HashMap::<(i64, String, String), Vec<Candidate>>::new();
        {
            let mut statement = tx.prepare(
                "SELECT s.name,s.language,s.public_id,s.qualified_name,s.file_id,f.path
                 FROM symbols s JOIN files f ON f.id=s.file_id
                 ORDER BY s.name,s.language,s.qualified_name,s.public_id",
            )?;
            for candidate in statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            {
                let value = (candidate.2, candidate.3, candidate.4, candidate.5);
                direct_candidates
                    .entry((candidate.0.clone(), candidate.1.clone()))
                    .or_default()
                    .push(value.clone());
                local_candidates
                    .entry((value.2, candidate.0, candidate.1))
                    .or_default()
                    .push(value);
            }
        }
        let mut imported_candidates = HashMap::<(i64, String, String), Vec<Candidate>>::new();
        {
            let mut statement = tx.prepare(
                "SELECT b.file_id,b.binding_name,s.language,s.public_id,s.qualified_name,
                        s.file_id,f.path
                 FROM import_bindings b
                 JOIN symbols s ON s.public_id=b.target_public_id
                 JOIN files f ON f.id=s.file_id
                 ORDER BY b.file_id,b.binding_name,s.language,s.qualified_name,s.public_id",
            )?;
            for candidate in statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            {
                imported_candidates
                    .entry((candidate.0, candidate.1, candidate.2))
                    .or_default()
                    .push((candidate.3, candidate.4, candidate.5, candidate.6));
            }
        }
        let (arkui_entry_candidates, arkui_module_roots) = {
            let mut statement = tx.prepare(
                "SELECT s.public_id,s.qualified_name,s.file_id,f.path
                 FROM unresolved_references u
                 JOIN symbols s ON s.public_id=u.source_public_id
                 JOIN files f ON f.id=s.file_id
                 WHERE u.provenance='framework/arkui-entry'
                   AND s.language='arkts' AND s.kind='struct'
                 ORDER BY f.path,s.qualified_name,s.public_id",
            )?;
            let candidates = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<Candidate>>>()?;
            let mut by_route = HashMap::<String, Vec<Candidate>>::new();
            let mut module_roots = HashSet::new();
            for candidate in candidates {
                if let Some((module, route)) = arkui_route_parts(&candidate.3) {
                    module_roots.insert(module);
                    by_route.entry(route).or_default().push(candidate);
                }
            }
            (by_route, module_roots)
        };
        let mut candidate_cache = HashMap::<
            (String, String, i64, String, String, String, String),
            Vec<(String, String, i64)>,
        >::new();
        let imported_bindings = {
            let mut statement = tx.prepare(
                "SELECT DISTINCT file_id,binding_name
                 FROM unresolved_references
                 WHERE kind='imports'
                 ORDER BY file_id,binding_name",
            )?;
            let bindings = statement
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            bindings
        };
        let return_summaries = {
            let mut statement = tx.prepare(
                "SELECT owner_public_id,file_id,type_name
                 FROM callable_return_types
                 ORDER BY owner_public_id",
            )?;
            let summaries = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        (row.get::<_, i64>(1)?, row.get::<_, String>(2)?),
                    ))
                })?
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            summaries
        };
        let mut local_nominal_types = NominalTypeMap::new();
        {
            let mut statement = tx.prepare(
                "SELECT s.file_id,s.name,s.qualified_name,f.path
                 FROM symbols s
                 JOIN files f ON f.id=s.file_id
                 WHERE s.kind IN ('class','struct','interface')
                 ORDER BY s.file_id,s.name,s.qualified_name,s.public_id",
            )?;
            for row in statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            {
                local_nominal_types
                    .entry((row.0, row.1))
                    .or_default()
                    .push((row.2, row.0, row.3));
            }
        }
        let mut imported_nominal_types = NominalTypeMap::new();
        {
            let mut statement = tx.prepare(
                "SELECT b.file_id,b.binding_name,s.qualified_name,s.file_id,f.path
                 FROM import_bindings b
                 JOIN symbols s ON s.public_id=b.target_public_id
                 JOIN files f ON f.id=s.file_id
                 WHERE s.kind IN ('class','struct','interface')
                 ORDER BY b.file_id,b.binding_name,s.qualified_name,s.public_id",
            )?;
            for row in statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            {
                imported_nominal_types
                    .entry((row.0, row.1))
                    .or_default()
                    .push((row.2, row.3, row.4));
            }
        }
        let (nominal_symbols, nominal_owners) = {
            let mut statement = tx.prepare(
                "SELECT s.public_id,s.qualified_name,f.path,s.file_id
                 FROM symbols s
                 JOIN files f ON f.id=s.file_id
                 WHERE s.kind IN ('class','struct','interface')
                 ORDER BY s.qualified_name,s.public_id",
            )?;
            let symbols = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut owners = HashMap::<(i64, String), Vec<String>>::new();
            for (public_id, qualified_name, _, file_id) in &symbols {
                owners
                    .entry((*file_id, qualified_name.clone()))
                    .or_default()
                    .push(public_id.clone());
            }
            (
                symbols
                    .into_iter()
                    .map(|(public_id, qualified_name, path, _)| (public_id, qualified_name, path))
                    .collect::<Vec<_>>(),
                owners,
            )
        };
        let inherited_bases = {
            let mut statement = tx.prepare(
                "SELECT source_public_id,target_public_id
                 FROM relationships
                 WHERE kind='extends'
                 ORDER BY source_public_id,target_public_id",
            )?;
            let mut bases = HashMap::<String, Vec<String>>::new();
            for (child, parent) in statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            {
                bases.entry(child).or_default().push(parent);
            }
            bases
        };
        let nominal_ids_by_identity = {
            let mut identities = HashMap::<(String, String), Vec<String>>::new();
            for (public_id, qualified_name, path) in &nominal_symbols {
                identities
                    .entry((qualified_name.clone(), path.clone()))
                    .or_default()
                    .push(public_id.clone());
            }
            identities
        };
        let inherited_methods = {
            let mut statement = tx.prepare(
                "SELECT file_id,name,public_id,qualified_name
                 FROM symbols
                 WHERE kind='method'
                 ORDER BY file_id,name,qualified_name,public_id",
            )?;
            let mut methods = HashMap::<(String, String), Vec<(String, String)>>::new();
            for (file_id, name, method_id, qualified_name) in statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            {
                let Some(owner_name) = qualified_name.strip_suffix(&format!(".{name}")) else {
                    continue;
                };
                let Some(owners) = nominal_owners.get(&(file_id, owner_name.to_owned())) else {
                    continue;
                };
                if owners.len() != 1 {
                    continue;
                }
                methods
                    .entry((owners[0].clone(), name))
                    .or_default()
                    .push((method_id, qualified_name));
            }
            methods
        };
        type ResolvedCallsiteTarget = (String, String, f64, String);
        let mut resolved_callsite_targets =
            HashMap::<(i64, String, usize), Vec<ResolvedCallsiteTarget>>::new();
        let mut nominal_result_cache = HashMap::<(i64, String), Option<(NominalType, f64)>>::new();
        let mut imported_receiver_cache = HashMap::<(i64, String), Option<NominalType>>::new();
        let mut inherited_member_cache =
            HashMap::<(String, String), InheritedMemberResolution>::new();
        let mut receiver_nominal_cache =
            HashMap::<(i64, String, String), ReceiverNominalResolution>::new();
        for (
            call_id,
            caller_id,
            callee_name,
            receiver_binding,
            receiver_type,
            target_file_hint,
            provenance,
            fact_confidence,
            explanation,
            file,
            line,
            language,
            file_id,
            resolvable,
            start_byte,
            fallback_caller_id,
            receiver_call_start_byte,
        ) in calls
        {
            if !resolvable {
                continue;
            }
            let receiver_binding = receiver_binding.unwrap_or_default();
            let mut receiver_type = receiver_type.unwrap_or_default();
            let mut target_file_hint = target_file_hint.unwrap_or_default();
            if provenance.starts_with("framework/astro-template")
                && (target_file_hint.starts_with("./") || target_file_hint.starts_with("../"))
            {
                // Project resolution canonicalizes valid relative imports. A
                // surviving relative hint escaped the root or targeted no
                // indexed source and must not fall through to global names.
                continue;
            }
            let mut inferred_factory = None::<(String, String)>;
            let mut inferred_confidence = 1.0_f64;
            if receiver_type.is_empty() && !receiver_binding.is_empty() {
                let receiver_key = (file_id, receiver_binding.clone());
                if !imported_receiver_cache.contains_key(&receiver_key) {
                    let mut imported_receivers = imported_nominal_types
                        .get(&receiver_key)
                        .cloned()
                        .unwrap_or_default();
                    imported_receivers.sort();
                    imported_receivers.dedup();
                    let unique = if imported_receivers.len() == 1 {
                        imported_receivers.pop()
                    } else {
                        None
                    };
                    imported_receiver_cache.insert(receiver_key.clone(), unique);
                }
                if let Some(imported_receiver) = imported_receiver_cache[&receiver_key].as_ref() {
                    receiver_type = imported_receiver.0.clone();
                    target_file_hint = imported_receiver.2.clone();
                    inferred_confidence = 0.97;
                }
            }
            if let Some(receiver_call_start_byte) = receiver_call_start_byte {
                if !resolve_call_results {
                    continue;
                }
                let Some(receiver_targets) = resolved_callsite_targets.get(&(
                    file_id,
                    caller_id.clone(),
                    receiver_call_start_byte,
                )) else {
                    continue;
                };
                if receiver_targets.len() != 1 {
                    continue;
                }
                let (factory_id, factory_qualified, factory_confidence, factory_scope) =
                    &receiver_targets[0];
                let Some((factory_file_id, return_type)) = return_summaries.get(factory_id) else {
                    continue;
                };
                let nominal_key = (*factory_file_id, return_type.clone());
                if !nominal_result_cache.contains_key(&nominal_key) {
                    let mut nominal_targets = local_nominal_types
                        .get(&nominal_key)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|target| (target, 0.995_f64))
                        .collect::<Vec<_>>();
                    nominal_targets.extend(
                        imported_nominal_types
                            .get(&nominal_key)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .map(|target| (target, 0.97_f64)),
                    );
                    nominal_targets.sort_by(|left, right| left.0.cmp(&right.0));
                    nominal_targets.dedup_by(|left, right| {
                        if left.0 == right.0 {
                            left.1 = left.1.max(right.1);
                            true
                        } else {
                            false
                        }
                    });
                    let unique = if nominal_targets.len() == 1 {
                        nominal_targets.pop()
                    } else {
                        None
                    };
                    nominal_result_cache.insert(nominal_key.clone(), unique);
                }
                let Some((nominal_target, nominal_confidence)) =
                    nominal_result_cache[&nominal_key].as_ref()
                else {
                    continue;
                };
                receiver_type = nominal_target.0.clone();
                target_file_hint = nominal_target.2.clone();
                inferred_factory = Some((factory_qualified.clone(), factory_scope.clone()));
                inferred_confidence = inferred_confidence
                    .min(*factory_confidence)
                    .min(*nominal_confidence);
                if factory_scope.is_empty() {
                    continue;
                }
            }
            let cache_key = (
                callee_name.clone(),
                language.clone(),
                file_id,
                receiver_binding.clone(),
                receiver_type.clone(),
                target_file_hint.clone(),
                provenance.clone(),
            );
            let is_arkui_route = provenance == "framework/arkui-route";
            if !candidate_cache.contains_key(&cache_key) {
                let direct = direct_candidates
                    .get(&(callee_name.clone(), language.clone()))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let receiver_nominal = if receiver_type.is_empty() {
                    ReceiverNominalResolution::NoMatch
                } else {
                    let nominal_key = (file_id, receiver_type.clone(), target_file_hint.clone());
                    receiver_nominal_cache
                        .entry(nominal_key)
                        .or_insert_with(|| {
                            resolve_receiver_nominal(
                                file_id,
                                &receiver_type,
                                &target_file_hint,
                                &local_nominal_types,
                                &imported_nominal_types,
                                &nominal_ids_by_identity,
                            )
                        })
                        .clone()
                };
                let mut receiver_ambiguous =
                    matches!(receiver_nominal, ReceiverNominalResolution::Ambiguous);
                let receiver_exact =
                    matches!(receiver_nominal, ReceiverNominalResolution::Unique(_));
                let mut targets = if is_arkui_route {
                    target_file_hint
                        .strip_prefix("arkui-route:")
                        .and_then(|route| arkui_entry_candidates.get(route))
                        .into_iter()
                        .flatten()
                        .filter(|candidate| {
                            arkui_route_candidate_matches(&file, &candidate.3, &arkui_module_roots)
                        })
                        .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 0))
                        .collect::<Vec<_>>()
                } else if let ReceiverNominalResolution::Unique(receiver_id) = &receiver_nominal {
                    let direct_methods = inherited_methods
                        .get(&(receiver_id.clone(), callee_name.clone()))
                        .cloned()
                        .unwrap_or_default();
                    if direct_methods.len() > 1 {
                        receiver_ambiguous = true;
                        Vec::new()
                    } else if direct_methods.is_empty() {
                        let receiver_qualified = format!("{receiver_type}.{callee_name}");
                        direct
                            .iter()
                            .filter(|candidate| {
                                (target_file_hint.is_empty()
                                    || module_hint_matches(&target_file_hint, &candidate.3))
                                    && (candidate.1 == receiver_qualified
                                        || candidate.1.ends_with(&format!(".{receiver_qualified}")))
                            })
                            .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 0))
                            .collect::<Vec<_>>()
                    } else {
                        direct_methods
                            .into_iter()
                            .map(|candidate| (candidate.0, candidate.1, 0))
                            .collect::<Vec<_>>()
                    }
                } else if receiver_ambiguous {
                    Vec::new()
                } else {
                    let receiver_qualified = format!("{receiver_type}.{callee_name}");
                    direct
                        .iter()
                        .filter(|candidate| {
                            (!target_file_hint.is_empty() || !receiver_type.is_empty())
                                && (target_file_hint.is_empty()
                                    || module_hint_matches(&target_file_hint, &candidate.3))
                                && (receiver_type.is_empty()
                                    || candidate.1 == receiver_qualified
                                    || candidate.1.ends_with(&format!(".{receiver_qualified}")))
                        })
                        .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 0))
                        .collect::<Vec<_>>()
                };
                if !is_arkui_route && targets.is_empty() && !receiver_ambiguous {
                    if let ReceiverNominalResolution::Unique(receiver_id) = &receiver_nominal {
                        let inherited_key = (receiver_id.clone(), callee_name.clone());
                        let inherited =
                            inherited_member_cache
                                .entry(inherited_key)
                                .or_insert_with(|| {
                                    resolve_inherited_member(
                                        receiver_id,
                                        &callee_name,
                                        &inherited_bases,
                                        &inherited_methods,
                                    )
                                });
                        match inherited {
                            InheritedMemberResolution::Unique(target_id, qualified_name) => {
                                targets.push((target_id.clone(), qualified_name.clone(), 5));
                            }
                            InheritedMemberResolution::Ambiguous => receiver_ambiguous = true,
                            InheritedMemberResolution::NoMatch => {}
                        }
                    }
                }
                if !is_arkui_route && targets.is_empty() && !receiver_ambiguous && !receiver_exact {
                    if let Some(local) =
                        local_candidates.get(&(file_id, callee_name.clone(), language.clone()))
                    {
                        targets.extend(
                            local
                                .iter()
                                .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 1)),
                        );
                    }
                }
                if !is_arkui_route && targets.is_empty() && !receiver_ambiguous && !receiver_exact {
                    for candidate_language in
                        std::iter::once(language.as_str()).chain(compatible_web_language(&language))
                    {
                        if let Some(imported) = imported_candidates.get(&(
                            file_id,
                            callee_name.clone(),
                            candidate_language.to_owned(),
                        )) {
                            targets.extend(
                                imported
                                    .iter()
                                    .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 2)),
                            );
                        }
                    }
                }
                if !is_arkui_route
                    && targets.is_empty()
                    && !receiver_ambiguous
                    && !receiver_exact
                    && language == "arkts"
                    && !receiver_binding.is_empty()
                    && imported_bindings.contains(&(file_id, receiver_binding.clone()))
                {
                    if let Some(root) = harmony_project_root(&file) {
                        targets.extend(
                            direct
                                .iter()
                                .filter(|candidate| path_is_within(&candidate.3, &root))
                                .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 3)),
                        );
                    }
                }
                if !is_arkui_route && targets.is_empty() && !receiver_ambiguous && !receiver_exact {
                    targets.extend(
                        direct
                            .iter()
                            .take(CALL_FANOUT_CAP + 1)
                            .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 4)),
                    );
                }
                targets.sort_by(|left, right| {
                    left.2
                        .cmp(&right.2)
                        .then_with(|| left.1.cmp(&right.1))
                        .then_with(|| left.0.cmp(&right.0))
                });
                targets.dedup_by(|left, right| left.0 == right.0);
                if is_arkui_route && targets.len() != 1 {
                    targets.clear();
                }
                candidate_cache.insert(cache_key.clone(), targets);
            }
            let targets = &candidate_cache[&cache_key];
            let Some(best_rank) = targets.first().map(|target| target.2) else {
                continue;
            };
            let target_count = targets
                .iter()
                .take_while(|target| target.2 == best_rank)
                .count();
            if target_count > CALL_FANOUT_CAP {
                continue;
            }
            if inferred_factory.is_some() && target_count != 1 {
                continue;
            }
            let resolution_confidence: f64 = match (best_rank, target_count) {
                (0, 1) => 0.995,
                (1, 1) => 0.99,
                (2, 1) => 0.97,
                (0..=2, _) => 0.65,
                (3, 1) => 0.9,
                (4, 1) => 0.75,
                (5, 1) => 0.97,
                _ => 0.35,
            };
            let scope = match (is_arkui_route, best_rank) {
                (true, 0) => "exact ArkUI entry page",
                (false, 0) if !target_file_hint.is_empty() => "imported package",
                (false, 0) => "receiver type",
                (false, 1) => "same-file lexical scope",
                (false, 2) => "explicit import scope",
                (false, 3) => "verified Harmony project import scope",
                (false, 5) => "nearest inherited receiver type",
                _ => "language-wide fallback",
            };
            let relationship_explanation = inferred_factory
                .as_ref()
                .map(|(factory, factory_scope)| {
                    format!(
                        "{explanation}; receiver resolves from {factory}'s explicit return \
                         annotation after the factory resolved through {factory_scope}"
                    )
                })
                .unwrap_or_else(|| explanation.clone());
            let effective_confidence = fact_confidence
                .min(resolution_confidence)
                .min(inferred_confidence);
            for (target_id, qualified_name, _) in
                targets.iter().take_while(|target| target.2 == best_rank)
            {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO resolved_call_targets(
                        call_id,target_public_id,target_qualified_name,
                        resolution_confidence,resolution_scope,relationship_explanation
                     ) VALUES (?1,?2,?3,?4,?5,?6)",
                )?
                .execute(params![
                    call_id,
                    target_id,
                    qualified_name,
                    effective_confidence,
                    scope,
                    relationship_explanation
                ])?;
                let callsite_key = (file_id, caller_id.clone(), start_byte);
                if receiver_callsite_keys.contains(&callsite_key) {
                    let callsite_targets =
                        resolved_callsite_targets.entry(callsite_key).or_default();
                    let correlated = (
                        target_id.clone(),
                        qualified_name.clone(),
                        effective_confidence,
                        scope.to_owned(),
                    );
                    if let Some(existing) = callsite_targets
                        .iter_mut()
                        .find(|existing| existing.0 == *target_id)
                    {
                        if correlated.2 < existing.2 {
                            *existing = correlated;
                        }
                    } else {
                        callsite_targets.push(correlated);
                    }
                }
                if fallback_caller_id.is_some() {
                    continue;
                }
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: caller_id.clone(),
                        target_id: target_id.clone(),
                        kind: RelationshipKind::Calls,
                        evidence: Evidence::new(
                            &provenance,
                            effective_confidence,
                            if effective_confidence >= 0.75 {
                                format!(
                                    "{relationship_explanation}; target resolves to \
                                     {qualified_name} through {scope}"
                                )
                            } else {
                                format!(
                                    "{relationship_explanation}; {scope} has multiple candidates; \
                                     {qualified_name} is a possible target"
                                )
                            },
                            &file,
                            line,
                        ),
                    },
                )?;
                resolved += 1;
            }
        }
        Ok(resolved)
    }

    fn resolve_c_function_pointer_dispatch(tx: &Transaction<'_>) -> Result<usize> {
        const PROVENANCE: &str = "dynamic/c-function-pointer-dispatch";
        const INCLUDE_DEPTH_CAP: usize = 16;
        const WORK_CAP: usize = 100_000;
        const TARGET_FANOUT_CAP: usize = 300;
        type LayoutKey = (String, String);
        type FieldKey = (String, String, String);

        tx.execute(
            "DELETE FROM relationships WHERE provenance=?1",
            [PROVENANCE],
        )?;

        let mut statement = tx.prepare(
            "SELECT f.path,b.payload
             FROM c_function_pointer_fact_batches b
             JOIN files f ON f.id=b.file_id
             ORDER BY f.path",
        )?;
        let batches = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        if batches.is_empty() {
            return Ok(0);
        }

        let mut files = HashMap::<String, CFunctionPointerFacts>::new();
        for (path, payload) in batches {
            files.insert(path, serde_json::from_str(&payload)?);
        }
        let all_paths = files.keys().cloned().collect::<HashSet<_>>();
        let mut include_edges = HashMap::<String, Vec<String>>::new();
        for (path, facts) in &files {
            let mut targets = Vec::new();
            for include in &facts.includes {
                if let Some(target) = resolve_c_include(path, include, &all_paths) {
                    targets.push(target);
                }
            }
            targets.sort();
            targets.dedup();
            include_edges.insert(path.clone(), targets);
        }

        let mut layouts = HashMap::<LayoutKey, Vec<crate::model::CStructLayoutFact>>::new();
        for (path, facts) in &files {
            for layout in &facts.layouts {
                layouts
                    .entry((path.clone(), layout.type_name.clone()))
                    .or_default()
                    .push(layout.clone());
            }
        }
        let resolve_layout = |source_file: &str, type_name: &str| -> Option<LayoutKey> {
            let normalized_type = normalize_c_type_name(type_name);
            let accepted_types = [normalized_type.clone(), format!("{normalized_type}_tag")];
            let visible = c_visible_files(source_file, &include_edges, INCLUDE_DEPTH_CAP, WORK_CAP);
            let mut candidates = Vec::new();
            for path in visible {
                for candidate_type in &accepted_types {
                    let key = (path.clone(), candidate_type.clone());
                    if layouts.get(&key).is_some_and(|items| items.len() == 1) {
                        candidates.push(key);
                    }
                }
            }
            candidates.sort();
            candidates.dedup();
            if candidates.len() == 1 {
                return Some(candidates.remove(0));
            }
            if !candidates.is_empty() {
                return None;
            }
            None
        };
        let resolve_layout_by_field = |source_file: &str, field_name: &str| -> Option<LayoutKey> {
            let visible = c_visible_files(source_file, &include_edges, INCLUDE_DEPTH_CAP, WORK_CAP);
            let visible = visible.into_iter().collect::<HashSet<_>>();
            let mut candidates = layouts
                .iter()
                .filter(|((path, _), items)| {
                    visible.contains(path)
                        && items.len() == 1
                        && items[0]
                            .fields
                            .iter()
                            .any(|field| field.name == field_name && field.function_pointer)
                })
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            candidates.sort();
            candidates.dedup();
            if candidates.len() == 1 {
                return Some(candidates.remove(0));
            }
            if !candidates.is_empty() {
                return None;
            }
            None
        };
        let resolve_path_layout =
            |source_file: &str, receiver_type: &str, receiver_path: &[String]| {
                let mut layout_key = resolve_layout(source_file, receiver_type)?;
                for member in receiver_path.iter().skip(1) {
                    let layout = layouts.get(&layout_key)?.first()?;
                    let next_type = layout
                        .fields
                        .iter()
                        .find(|field| field.name == *member)?
                        .value_type
                        .as_deref()?;
                    layout_key = resolve_layout(source_file, next_type)?;
                }
                Some(layout_key)
            };
        let is_function_pointer_field = |layout_key: &LayoutKey, field_name: &str| {
            let Some(field) = layouts
                .get(layout_key)
                .and_then(|items| items.first())
                .and_then(|layout| layout.fields.iter().find(|field| field.name == field_name))
            else {
                return false;
            };
            if field.function_pointer {
                return true;
            }
            let Some(value_type) = field.value_type.as_deref() else {
                return false;
            };
            let visible =
                c_visible_files(&layout_key.0, &include_edges, INCLUDE_DEPTH_CAP, WORK_CAP);
            let mut matches = visible
                .into_iter()
                .filter_map(|path| files.get(&path))
                .flat_map(|facts| facts.typedefs.iter())
                .filter(|typedef| typedef.name == value_type)
                .map(|typedef| typedef.pointer)
                .collect::<Vec<_>>();
            matches.sort();
            matches.dedup();
            matches == [true]
        };

        let mut symbols_by_file_name = HashMap::<(String, String), Vec<String>>::new();
        let mut symbol_name_by_id = HashMap::<String, String>::new();
        let mut symbol_statement = tx.prepare(
            "SELECT f.path,s.name,s.public_id
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE s.kind IN ('function','method')
             ORDER BY f.path,s.name,s.public_id",
        )?;
        for row in symbol_statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (file, name, id) = row?;
            symbol_name_by_id.insert(id.clone(), name.clone());
            symbols_by_file_name
                .entry((file, name))
                .or_default()
                .push(id);
        }
        drop(symbol_statement);
        for candidates in symbols_by_file_name.values_mut() {
            candidates.sort();
            candidates.dedup();
        }
        let macro_resolver = CMacroResolver::new(&files, &all_paths);
        let mut macro_bindings = HashMap::<String, Vec<CFunctionPointerBindingFact>>::new();
        let mut macro_array_target_names = HashMap::<(String, String), Vec<String>>::new();
        for (path, facts) in &files {
            let catalog = c_guard_catalog(facts);
            for use_fact in &facts.macro_initializers {
                let states = macro_resolver.states_at(path, use_fact.site_start_byte);
                for state in states {
                    if c_guard_truth(&use_fact.guard_path, &catalog, &state, path)
                        == CPreprocessorTruth::False
                    {
                        continue;
                    }
                    let Some(expanded) = expand_c_macro_use(use_fact, &state) else {
                        continue;
                    };
                    for initializer in parse_c_expanded_initializers(
                        &expanded,
                        use_fact.field_name.as_deref(),
                        use_fact.field_index,
                    ) {
                        if use_fact.role == CMacroInitializerRole::PointerArrayElement {
                            macro_array_target_names
                                .entry((
                                    path.clone(),
                                    use_fact.receiver_path.first().cloned().unwrap_or_default(),
                                ))
                                .or_default()
                                .push(initializer.target_name);
                        } else {
                            macro_bindings.entry(path.clone()).or_default().push(
                                CFunctionPointerBindingFact {
                                    owner_id: use_fact.owner_id.clone(),
                                    receiver_type: use_fact.receiver_type.clone(),
                                    receiver_path: use_fact.receiver_path.clone(),
                                    field_name: initializer.field_name,
                                    field_index: initializer.field_index,
                                    target_name: initializer.target_name,
                                    guard_path: Vec::new(),
                                    line: use_fact.line,
                                    site_start_byte: use_fact.site_start_byte,
                                },
                            );
                        }
                    }
                }
            }
        }
        for bindings in macro_bindings.values_mut() {
            bindings.sort_by(|left, right| {
                left.site_start_byte
                    .cmp(&right.site_start_byte)
                    .then_with(|| left.field_name.cmp(&right.field_name))
                    .then_with(|| left.field_index.cmp(&right.field_index))
                    .then_with(|| left.target_name.cmp(&right.target_name))
            });
            bindings.dedup_by(|left, right| {
                left.site_start_byte == right.site_start_byte
                    && left.field_name == right.field_name
                    && left.field_index == right.field_index
                    && left.target_name == right.target_name
            });
        }
        let mut returned_pointer_targets = HashMap::<String, HashSet<String>>::new();
        for (path, facts) in &files {
            for returned in &facts.returns {
                let candidates = symbols_by_file_name
                    .get(&(path.clone(), returned.target_name.clone()))
                    .cloned()
                    .unwrap_or_default();
                if candidates.len() == 1 {
                    returned_pointer_targets
                        .entry(returned.owner_id.clone())
                        .or_default()
                        .insert(candidates[0].clone());
                }
            }
        }

        let mut array_targets = HashMap::<(String, String), Vec<String>>::new();
        for (path, facts) in &files {
            let visible = c_visible_files(path, &include_edges, INCLUDE_DEPTH_CAP, WORK_CAP)
                .into_iter()
                .collect::<HashSet<_>>();
            for array in &facts.arrays {
                let mut typedefs = files
                    .iter()
                    .filter(|(candidate_path, _)| visible.contains(*candidate_path))
                    .flat_map(|(_, candidate_facts)| candidate_facts.typedefs.iter())
                    .filter(|typedef| typedef.name == array.element_type)
                    .map(|typedef| typedef.pointer)
                    .collect::<Vec<_>>();
                typedefs.sort();
                typedefs.dedup();
                if typedefs.len() != 1 || (!typedefs[0] && !array.pointer_declarator) {
                    continue;
                }
                let mut targets = Vec::new();
                for target in &array.targets {
                    let candidates = symbols_by_file_name
                        .get(&(path.clone(), target.target_name.clone()))
                        .cloned()
                        .unwrap_or_default();
                    if candidates.len() == 1 {
                        targets.push(candidates[0].clone());
                    }
                }
                targets.sort();
                targets.dedup();
                if !targets.is_empty() && targets.len() <= TARGET_FANOUT_CAP {
                    array_targets.insert((path.clone(), array.name.clone()), targets);
                }
            }
        }
        for (key, target_names) in macro_array_target_names {
            let mut targets = target_names
                .into_iter()
                .flat_map(|target_name| {
                    symbols_by_file_name
                        .get(&(key.0.clone(), target_name))
                        .filter(|candidates| candidates.len() == 1)
                        .into_iter()
                        .flatten()
                        .cloned()
                })
                .collect::<Vec<_>>();
            targets.sort();
            targets.dedup();
            if !targets.is_empty() && targets.len() <= TARGET_FANOUT_CAP {
                array_targets.entry(key).or_default().extend(targets);
            }
        }
        for targets in array_targets.values_mut() {
            targets.sort();
            targets.dedup();
        }

        let mut registered = HashMap::<FieldKey, HashSet<String>>::new();
        let mut work = 0usize;
        let mut paths = files.keys().cloned().collect::<Vec<_>>();
        paths.sort();
        for path in &paths {
            let facts = &files[path];
            for binding in facts
                .bindings
                .iter()
                .chain(macro_bindings.get(path).into_iter().flatten())
            {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                if !binding.guard_path.is_empty()
                    && macro_resolver.guard_truth(
                        path,
                        binding.site_start_byte,
                        &binding.guard_path,
                    ) == CPreprocessorTruth::False
                {
                    continue;
                }
                let Some(receiver_type) = binding.receiver_type.as_deref() else {
                    continue;
                };
                let Some(layout_key) =
                    resolve_path_layout(path, receiver_type, &binding.receiver_path)
                else {
                    continue;
                };
                let Some(layout) = layouts.get(&layout_key).and_then(|items| items.first()) else {
                    continue;
                };
                let field = if let Some(name) = binding.field_name.as_deref() {
                    layout.fields.iter().find(|field| field.name == name)
                } else if let Some(index) = binding.field_index {
                    layout.fields.iter().find(|field| field.index == index)
                } else {
                    None
                };
                let Some(field) =
                    field.filter(|field| is_function_pointer_field(&layout_key, &field.name))
                else {
                    continue;
                };
                let candidates = symbols_by_file_name
                    .get(&(path.clone(), binding.target_name.clone()))
                    .cloned()
                    .unwrap_or_default();
                if candidates.len() != 1 {
                    continue;
                }
                registered
                    .entry((layout_key.0, layout_key.1, field.name.clone()))
                    .or_default()
                    .insert(candidates[0].clone());
            }
        }

        let mut formal_storages = Vec::<(String, usize, FieldKey)>::new();
        for path in &paths {
            for storage in &files[path].formal_storages {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let (Some(owner_name), Some(receiver_type)) = (
                    symbol_name_by_id.get(&storage.owner_id),
                    storage.receiver_type.as_deref(),
                ) else {
                    continue;
                };
                let Some(layout_key) =
                    resolve_path_layout(path, receiver_type, &storage.receiver_path)
                else {
                    continue;
                };
                let field_is_pointer = is_function_pointer_field(&layout_key, &storage.field_name);
                if field_is_pointer {
                    formal_storages.push((
                        owner_name.clone(),
                        storage.parameter_index,
                        (layout_key.0, layout_key.1, storage.field_name.clone()),
                    ));
                }
            }
        }
        formal_storages.sort();
        formal_storages.dedup();
        for path in &paths {
            for argument in &files[path].arguments {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let mut destinations = formal_storages
                    .iter()
                    .filter(|(callee_name, parameter_index, _)| {
                        callee_name == &argument.callee_name
                            && *parameter_index == argument.argument_index
                    })
                    .map(|(_, _, destination)| destination.clone())
                    .collect::<Vec<_>>();
                destinations.sort();
                destinations.dedup();
                if destinations.len() != 1 {
                    continue;
                }
                let candidates = symbols_by_file_name
                    .get(&(path.clone(), argument.target_name.clone()))
                    .cloned()
                    .unwrap_or_default();
                if candidates.len() != 1 {
                    continue;
                }
                registered
                    .entry(destinations.remove(0))
                    .or_default()
                    .insert(candidates[0].clone());
            }
        }

        let mut propagations = Vec::<(FieldKey, FieldKey)>::new();
        for path in &paths {
            for propagation in &files[path].propagations {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let (Some(target_type), Some(source_type)) = (
                    propagation.target_receiver_type.as_deref(),
                    propagation.source_receiver_type.as_deref(),
                ) else {
                    continue;
                };
                let (Some(target_layout_key), Some(source_layout_key)) = (
                    resolve_path_layout(path, target_type, &propagation.target_receiver_path),
                    resolve_path_layout(path, source_type, &propagation.source_receiver_path),
                ) else {
                    continue;
                };
                let target_is_pointer =
                    is_function_pointer_field(&target_layout_key, &propagation.target_field_name);
                let source_is_pointer =
                    is_function_pointer_field(&source_layout_key, &propagation.source_field_name);
                if !target_is_pointer || !source_is_pointer {
                    continue;
                }
                propagations.push((
                    (
                        target_layout_key.0,
                        target_layout_key.1,
                        propagation.target_field_name.clone(),
                    ),
                    (
                        source_layout_key.0,
                        source_layout_key.1,
                        propagation.source_field_name.clone(),
                    ),
                ));
            }
        }
        propagations.sort();
        propagations.dedup();
        let mut changed = true;
        while changed && work < WORK_CAP {
            changed = false;
            for (target, source) in &propagations {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let Some(source_targets) = registered.get(source).cloned() else {
                    continue;
                };
                if source_targets.len() > TARGET_FANOUT_CAP {
                    continue;
                }
                let target_targets = registered.entry(target.clone()).or_default();
                let previous = target_targets.len();
                target_targets.extend(source_targets);
                if target_targets.len() > TARGET_FANOUT_CAP {
                    target_targets.clear();
                    continue;
                }
                changed |= target_targets.len() != previous;
            }
        }

        let mut resolved = 0usize;
        for path in &paths {
            let facts = &files[path];
            for dispatch in &facts.dispatches {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let owner_exists = tx
                    .query_row(
                        "SELECT 1 FROM symbols
                         WHERE public_id=?1 AND kind IN ('function','method')",
                        [&dispatch.owner_id],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !owner_exists {
                    continue;
                }
                let layout_key = dispatch
                    .receiver_type
                    .as_deref()
                    .and_then(|receiver_type| {
                        resolve_path_layout(path, receiver_type, &dispatch.receiver_path)
                    })
                    .or_else(|| resolve_layout_by_field(path, &dispatch.field_name));
                let Some(layout_key) = layout_key else {
                    continue;
                };
                let field_is_pointer = is_function_pointer_field(&layout_key, &dispatch.field_name);
                if !field_is_pointer {
                    continue;
                }
                let key = (
                    layout_key.0.clone(),
                    layout_key.1.clone(),
                    dispatch.field_name.clone(),
                );
                let mut targets = registered
                    .get(&key)
                    .map(|targets| targets.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                targets.sort();
                if targets.is_empty() || targets.len() > TARGET_FANOUT_CAP {
                    continue;
                }
                for target_id in targets {
                    Self::insert_relationship(
                        tx,
                        &Relationship {
                            source_id: dispatch.owner_id.clone(),
                            target_id,
                            kind: RelationshipKind::Calls,
                            evidence: Evidence::new(
                                PROVENANCE,
                                0.97,
                                format!(
                                    "proven C/C++ function-pointer may-dispatch through {}.{}",
                                    layout_key.1, dispatch.field_name
                                ),
                                path,
                                dispatch.line,
                            )
                            .at_site(dispatch.site_start_byte),
                        },
                    )?;
                    resolved += 1;
                }
            }
        }
        for path in &paths {
            let visible = c_visible_files(path, &include_edges, INCLUDE_DEPTH_CAP, WORK_CAP)
                .into_iter()
                .collect::<HashSet<_>>();
            for dispatch in &files[path].array_dispatches {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let local_key = (path.clone(), dispatch.name.clone());
                let selected = if array_targets.contains_key(&local_key) {
                    Some(local_key)
                } else {
                    let mut candidates = array_targets
                        .keys()
                        .filter(|(candidate_file, name)| {
                            name == &dispatch.name && visible.contains(candidate_file)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    candidates.sort();
                    candidates.dedup();
                    (candidates.len() == 1).then(|| candidates.remove(0))
                };
                let Some(selected) = selected else {
                    continue;
                };
                let Some(targets) = array_targets.get(&selected) else {
                    continue;
                };
                for target_id in targets {
                    Self::insert_relationship(
                        tx,
                        &Relationship {
                            source_id: dispatch.owner_id.clone(),
                            target_id: target_id.clone(),
                            kind: RelationshipKind::Calls,
                            evidence: Evidence::new(
                                PROVENANCE,
                                0.97,
                                format!(
                                    "proven C/C++ function-pointer array may-dispatch through {}",
                                    dispatch.name
                                ),
                                path,
                                dispatch.line,
                            )
                            .at_site(dispatch.site_start_byte),
                        },
                    )?;
                    resolved += 1;
                }
            }
        }
        for path in &paths {
            for dispatch in &files[path].factory_dispatches {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let factories = symbols_by_file_name
                    .get(&(path.clone(), dispatch.factory_name.clone()))
                    .cloned()
                    .unwrap_or_default();
                if factories.len() != 1 {
                    continue;
                }
                let mut targets = returned_pointer_targets
                    .get(&factories[0])
                    .map(|targets| targets.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                targets.sort();
                if targets.is_empty() || targets.len() > TARGET_FANOUT_CAP {
                    continue;
                }
                let confidence = if targets.len() == 1 { 0.995 } else { 0.97 };
                for target_id in targets {
                    Self::insert_relationship(
                        tx,
                        &Relationship {
                            source_id: dispatch.owner_id.clone(),
                            target_id,
                            kind: RelationshipKind::Calls,
                            evidence: Evidence::new(
                                PROVENANCE,
                                confidence,
                                format!(
                                    "{} C++ function-pointer factory dispatch through {}",
                                    if confidence < 0.99 {
                                        "bounded may-call"
                                    } else {
                                        "exact"
                                    },
                                    dispatch.factory_name,
                                ),
                                path,
                                dispatch.line,
                            )
                            .at_site(dispatch.site_start_byte),
                        },
                    )?;
                    resolved += 1;
                }
            }
        }
        for path in &paths {
            let mut local_bindings = HashMap::<
                (String, String),
                Vec<&crate::model::CLocalFunctionPointerBindingFact>,
            >::new();
            for binding in &files[path].local_bindings {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                local_bindings
                    .entry((binding.owner_id.clone(), binding.local_name.clone()))
                    .or_default()
                    .push(binding);
            }
            for bindings in local_bindings.values_mut() {
                bindings.sort_by_key(|binding| binding.site_start_byte);
            }
            for dispatch in &files[path].local_dispatches {
                if work >= WORK_CAP {
                    break;
                }
                work += 1;
                let Some(bindings) =
                    local_bindings.get(&(dispatch.owner_id.clone(), dispatch.local_name.clone()))
                else {
                    continue;
                };
                let declaration = bindings
                    .iter()
                    .copied()
                    .filter(|binding| {
                        binding.declares_binding
                            && binding.site_start_byte < dispatch.site_start_byte
                            && binding.scope_start_byte <= dispatch.site_start_byte
                            && dispatch.site_start_byte < binding.scope_end_byte
                    })
                    .min_by_key(|binding| {
                        (
                            binding
                                .scope_end_byte
                                .saturating_sub(binding.scope_start_byte),
                            usize::MAX - binding.site_start_byte,
                        )
                    });
                let Some(declaration) = declaration else {
                    continue;
                };
                let mut targets = HashSet::<String>::new();
                let mut has_may_target = false;
                for binding in bindings.iter().copied().filter(|binding| {
                    declaration.site_start_byte <= binding.site_start_byte
                        && binding.site_start_byte < dispatch.site_start_byte
                        && declaration.scope_start_byte <= binding.site_start_byte
                        && binding.site_start_byte < declaration.scope_end_byte
                }) {
                    let binding_declaration = bindings
                        .iter()
                        .copied()
                        .filter(|candidate| {
                            candidate.declares_binding
                                && candidate.site_start_byte <= binding.site_start_byte
                                && candidate.scope_start_byte <= binding.site_start_byte
                                && binding.site_start_byte < candidate.scope_end_byte
                        })
                        .min_by_key(|candidate| {
                            (
                                candidate
                                    .scope_end_byte
                                    .saturating_sub(candidate.scope_start_byte),
                                usize::MAX - candidate.site_start_byte,
                            )
                        });
                    if binding_declaration.is_none_or(|candidate| {
                        candidate.site_start_byte != declaration.site_start_byte
                    }) {
                        continue;
                    }
                    let mut binding_targets =
                        if let Some(target_name) = binding.target_name.as_ref() {
                            let candidates = symbols_by_file_name
                                .get(&(path.clone(), target_name.clone()))
                                .cloned()
                                .unwrap_or_default();
                            if candidates.len() == 1 {
                                vec![candidates[0].clone()]
                            } else {
                                Vec::new()
                            }
                        } else if let Some(factory_name) = binding.factory_name.as_ref() {
                            let factories = symbols_by_file_name
                                .get(&(path.clone(), factory_name.clone()))
                                .cloned()
                                .unwrap_or_default();
                            if factories.len() == 1 {
                                returned_pointer_targets
                                    .get(&factories[0])
                                    .map(|targets| targets.iter().cloned().collect::<Vec<_>>())
                                    .unwrap_or_default()
                            } else {
                                Vec::new()
                            }
                        } else {
                            Vec::new()
                        };
                    binding_targets.sort();
                    binding_targets.dedup();
                    if binding_targets.len() > TARGET_FANOUT_CAP {
                        binding_targets.clear();
                    }
                    if binding.conditional {
                        has_may_target |= !binding_targets.is_empty();
                        targets.extend(binding_targets);
                    } else {
                        targets.clear();
                        has_may_target = binding_targets.len() > 1;
                        targets.extend(binding_targets);
                    }
                }
                if targets.len() > TARGET_FANOUT_CAP {
                    continue;
                }
                let mut targets = targets.iter().cloned().collect::<Vec<_>>();
                targets.sort();
                for target_id in targets {
                    Self::insert_relationship(
                        tx,
                        &Relationship {
                            source_id: dispatch.owner_id.clone(),
                            target_id,
                            kind: RelationshipKind::Calls,
                            evidence: Evidence::new(
                                PROVENANCE,
                                if has_may_target { 0.97 } else { 0.995 },
                                format!(
                                    "{} same-owner C++ local function-pointer dispatch through {}",
                                    if has_may_target {
                                        "bounded may-call"
                                    } else {
                                        "exact"
                                    },
                                    dispatch.local_name,
                                ),
                                path,
                                dispatch.line,
                            )
                            .at_site(dispatch.site_start_byte),
                        },
                    )?;
                    resolved += 1;
                }
            }
        }
        Ok(resolved)
    }

    fn materialize_fastapi_routes(tx: &Transaction<'_>) -> Result<usize> {
        const DEPTH_CAP: usize = 16;
        const WORK_CAP: usize = 10_000;
        type RouterKey = (String, String);

        let mut statement = tx.prepare(
            "SELECT f.id,f.path,b.payload
             FROM fastapi_fact_batches b JOIN files f ON f.id=b.file_id
             ORDER BY f.path",
        )?;
        let batches = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        if batches.is_empty() {
            return Ok(0);
        }
        let mut files = HashMap::new();
        let mut paths = Vec::new();
        for (file_id, path, payload) in batches {
            let facts = serde_json::from_str::<FastApiFacts>(&payload)?;
            paths.push(path.clone());
            files.insert(path, (file_id, facts));
        }
        paths.sort();

        let mut declarations = HashMap::<RouterKey, (String, bool)>::new();
        let mut aliases = HashMap::<RouterKey, FastApiRouterRef>::new();
        let mut factories = HashMap::<RouterKey, FastApiRouterRef>::new();
        for (path, (_, facts)) in &files {
            for router in &facts.routers {
                declarations.insert(
                    (path.clone(), router.name.clone()),
                    (router.prefix.clone(), router.application),
                );
            }
            for factory in &facts.factories {
                factories.insert((path.clone(), factory.name.clone()), factory.router.clone());
            }
            for alias in &facts.aliases {
                aliases.insert((path.clone(), alias.name.clone()), alias.router.clone());
            }
        }

        let resolve = |reference: &FastApiRouterRef, current_file: &str| {
            resolve_fastapi_router_ref(
                reference,
                current_file,
                &paths,
                &declarations,
                &aliases,
                &factories,
                0,
                &mut HashSet::new(),
            )
        };
        let mut edges = HashMap::<RouterKey, Vec<(RouterKey, String, String, usize)>>::new();
        let mut routes = HashMap::<RouterKey, Vec<(String, crate::model::FastApiRouteFact)>>::new();
        for (path, (_, facts)) in &files {
            for mount in &facts.mounts {
                let (Some(parent), Some(child)) =
                    (resolve(&mount.parent, path), resolve(&mount.child, path))
                else {
                    continue;
                };
                edges.entry(parent).or_default().push((
                    child,
                    mount.prefix.clone(),
                    path.clone(),
                    mount.line,
                ));
            }
            for route in &facts.routes {
                if let Some(router) = resolve(&route.router, path) {
                    routes
                        .entry(router)
                        .or_default()
                        .push((path.clone(), route.clone()));
                }
            }
        }
        for children in edges.values_mut() {
            children.sort_by(|left, right| {
                left.0
                    .cmp(&right.0)
                    .then_with(|| left.1.cmp(&right.1))
                    .then_with(|| left.2.cmp(&right.2))
                    .then_with(|| left.3.cmp(&right.3))
            });
            children.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        }

        let mut roots = declarations
            .iter()
            .filter(|(_, (_, application))| *application)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        roots.sort();
        let mut materialized = Vec::new();
        let mut work = 0usize;
        for root in roots {
            let root_identity = format!("{}#{}", root.0, root.1);
            let mut stack = vec![(
                root,
                String::new(),
                root_identity,
                Vec::<RouterKey>::new(),
                0usize,
            )];
            while let Some((router, inherited, mount_identity, mut ancestry, depth)) = stack.pop() {
                if depth > DEPTH_CAP || work >= WORK_CAP || ancestry.contains(&router) {
                    continue;
                }
                work += 1;
                ancestry.push(router.clone());
                let own_prefix = &declarations[&router].0;
                let effective_prefix = join_route_path(&inherited, own_prefix);
                if let Some(handlers) = routes.get(&router) {
                    for (file, route) in handlers {
                        let path = join_route_path(&effective_prefix, &route.path);
                        if path.is_empty() {
                            continue;
                        }
                        materialized.push((
                            route.verb.clone(),
                            if path.is_empty() {
                                "/".to_owned()
                            } else {
                                path
                            },
                            file.clone(),
                            mount_identity.clone(),
                            route.clone(),
                        ));
                    }
                }
                if let Some(children) = edges.get(&router) {
                    for (child, mount_prefix, mount_file, _) in children.iter().rev() {
                        stack.push((
                            child.clone(),
                            join_route_path(&effective_prefix, mount_prefix),
                            format!(
                                "{mount_identity}>{mount_file}:{}#{}:{mount_prefix}",
                                child.0, child.1
                            ),
                            ancestry.clone(),
                            depth + 1,
                        ));
                    }
                }
            }
        }
        materialized.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.0.cmp(&right.0))
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.3.cmp(&right.3))
                .then_with(|| left.4.line.cmp(&right.4.line))
                .then_with(|| left.4.handler_name.cmp(&right.4.handler_name))
        });

        let mut resolved = 0;
        for (verb, path, endpoint_file, mount_identity, route_fact) in materialized {
            let Some((file_id, _)) = files.get(&endpoint_file) else {
                continue;
            };
            let handler_exists = tx
                .query_row(
                    "SELECT 1 FROM symbols
                     WHERE file_id=?1 AND public_id=?2 AND kind IN ('function','method')",
                    params![file_id, route_fact.handler_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !handler_exists {
                continue;
            }
            let file_symbol_id: String = tx.query_row(
                "SELECT public_id FROM symbols WHERE file_id=?1 AND kind='file'",
                [file_id],
                |row| row.get(0),
            )?;
            let name = format!("{verb} {path}");
            let symbol = Symbol::new_disambiguated(
                Language::Python,
                SymbolKind::Route,
                &name,
                &name,
                &endpoint_file,
                SourceSpan {
                    start_byte: route_fact.start_byte,
                    end_byte: route_fact.end_byte,
                    start_line: route_fact.line,
                    end_line: route_fact.end_line,
                },
                &format!(
                    "fastapi|{verb}|{path}|{}|mount:{mount_identity}",
                    route_fact.handler_name
                ),
            );
            tx.prepare_cached(
                "INSERT INTO symbols(
                    public_id,semantic_key,file_id,language,kind,name,qualified_name,
                    start_byte,end_byte,start_line,end_line
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?
            .execute(params![
                symbol.id,
                symbol.semantic_key,
                file_id,
                symbol.language.to_string(),
                symbol.kind.to_string(),
                symbol.name,
                symbol.qualified_name,
                symbol.start_byte as i64,
                symbol.end_byte as i64,
                symbol.start_line as i64,
                symbol.end_line as i64
            ])?;
            tx.prepare_cached(
                "INSERT INTO symbol_search(public_id,name,qualified_name,file,segments)
                 VALUES (?1,?2,?3,?4,?5)",
            )?
            .execute(params![
                symbol.id,
                symbol.name,
                symbol.qualified_name,
                symbol.file,
                identifier_segments(&format!("{} {}", symbol.name, symbol.qualified_name))
                    .join(" ")
            ])?;
            tx.prepare_cached(
                "INSERT INTO fastapi_generated_symbols(public_id,file_id) VALUES (?1,?2)",
            )?
            .execute(params![symbol.id, file_id])?;
            Self::insert_relationship(
                tx,
                &Relationship {
                    source_id: file_symbol_id,
                    target_id: symbol.id.clone(),
                    kind: RelationshipKind::Contains,
                    evidence: Evidence::new(
                        "framework/fastapi-route",
                        1.0,
                        format!("{name} is registered through exact FastAPI router composition"),
                        &endpoint_file,
                        route_fact.line,
                    ),
                },
            )?;
            Self::insert_relationship(
                tx,
                &Relationship {
                    source_id: symbol.id,
                    target_id: route_fact.handler_id.clone(),
                    kind: RelationshipKind::Calls,
                    evidence: Evidence::new(
                        "framework/fastapi-route",
                        0.995,
                        format!("{name} decorates handler {}", route_fact.handler_name),
                        &endpoint_file,
                        route_fact.line,
                    ),
                },
            )?;
            resolved += 1;
        }
        let mut callable_aliases = HashMap::<RouterKey, FastApiRouterRef>::new();
        let mut callable_factories = HashMap::<RouterKey, FastApiRouterRef>::new();
        let mut dependency_sites = Vec::new();
        for (path, (_, facts)) in &files {
            for alias in facts
                .aliases
                .iter()
                .chain(facts.dependency_aliases.iter())
                .chain(facts.dependency_type_aliases.iter())
            {
                callable_aliases.insert((path.clone(), alias.name.clone()), alias.router.clone());
            }
            for factory in &facts.dependency_factories {
                callable_factories
                    .insert((path.clone(), factory.name.clone()), factory.router.clone());
            }
            for dependency in &facts.dependencies {
                dependency_sites.push((path.clone(), dependency.clone()));
            }
        }
        dependency_sites.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.line.cmp(&right.1.line))
                .then_with(|| left.1.owner_id.cmp(&right.1.owner_id))
        });

        let mut callable_symbols = HashMap::<RouterKey, Vec<(String, String)>>::new();
        let mut all_paths = tx
            .prepare("SELECT path FROM files ORDER BY path")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut symbol_statement = tx.prepare(
            "SELECT f.path,s.name,s.qualified_name,s.public_id
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE s.kind IN ('function','method')
             ORDER BY f.path,s.name,s.qualified_name,s.public_id",
        )?;
        for symbol in symbol_statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })? {
            let (file, name, qualified_name, public_id) = symbol?;
            callable_symbols
                .entry((file.clone(), name))
                .or_default()
                .push((qualified_name.clone(), public_id.clone()));
            callable_symbols
                .entry((file, qualified_name.clone()))
                .or_default()
                .push((qualified_name, public_id));
        }
        drop(symbol_statement);
        all_paths.sort();
        all_paths.dedup();
        for candidates in callable_symbols.values_mut() {
            candidates.sort();
            candidates.dedup();
        }

        for (current_file, site) in dependency_sites.into_iter().take(WORK_CAP) {
            let owner_exists = tx
                .query_row(
                    "SELECT 1 FROM symbols WHERE public_id=?1 AND kind IN ('function','method')",
                    [&site.owner_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !owner_exists {
                continue;
            }
            let Some(target_id) = resolve_fastapi_callable_ref(
                &site.dependency,
                &current_file,
                &all_paths,
                &callable_symbols,
                &callable_aliases,
                &callable_factories,
                0,
                &mut HashSet::new(),
            ) else {
                continue;
            };
            Self::insert_relationship(
                tx,
                &Relationship {
                    source_id: site.owner_id,
                    target_id,
                    kind: RelationshipKind::Calls,
                    evidence: Evidence::new(
                        "framework/fastapi-dependency",
                        0.995,
                        format!("{} receives an exact FastAPI dependency", site.owner_name),
                        &current_file,
                        site.line,
                    )
                    .at_site(site.site_start_byte),
                },
            )?;
            resolved += 1;
        }
        Ok(resolved)
    }

    fn publish_deferred_inline_calls(tx: &Transaction<'_>) -> Result<usize> {
        let mut statement = tx.prepare(
            "SELECT COALESCE(primary_symbol.public_id,fallback_symbol.public_id),
                    u.provenance,u.confidence,u.evidence_file,u.evidence_line,
                    t.target_public_id,t.target_qualified_name,
                    t.resolution_confidence,t.resolution_scope,t.relationship_explanation
             FROM unresolved_calls u
             JOIN resolved_call_targets t ON t.call_id=u.id
             LEFT JOIN symbols primary_symbol
                    ON primary_symbol.public_id=u.caller_public_id
             LEFT JOIN symbols fallback_symbol
                    ON fallback_symbol.public_id=u.fallback_caller_public_id
             WHERE u.fallback_caller_public_id IS NOT NULL
               AND (primary_symbol.public_id IS NOT NULL
                    OR fallback_symbol.public_id IS NOT NULL)
             ORDER BY u.evidence_file,u.evidence_line,u.id,t.target_qualified_name",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)? as usize,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, f64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut resolved = 0;
        for (
            caller_id,
            provenance,
            fact_confidence,
            file,
            line,
            target_id,
            qualified_name,
            resolution_confidence,
            scope,
            relationship_explanation,
        ) in rows
        {
            Self::insert_relationship(
                tx,
                &Relationship {
                    source_id: caller_id,
                    target_id,
                    kind: RelationshipKind::Calls,
                    evidence: Evidence::new(
                        &provenance,
                        fact_confidence.min(resolution_confidence),
                        if resolution_confidence >= 0.75 {
                            format!(
                                "{relationship_explanation}; target resolves to {qualified_name} \
                                 through {scope}"
                            )
                        } else {
                            format!(
                                "{relationship_explanation}; {scope} has multiple candidates; \
                                 {qualified_name} is a possible target"
                            )
                        },
                        &file,
                        line,
                    ),
                },
            )?;
            resolved += 1;
        }
        Ok(resolved)
    }

    fn resolve_callback_arguments(tx: &Transaction<'_>) -> Result<(usize, usize)> {
        type StoredInlineCallbackSymbol = (
            Language,
            String,
            String,
            String,
            String,
            usize,
            usize,
            usize,
            usize,
        );
        type StoredCallbackArgument = (
            String,
            String,
            usize,
            Option<String>,
            String,
            Option<String>,
            Option<StoredInlineCallbackSymbol>,
            usize,
            usize,
        );
        let mut arguments = Vec::new();
        let mut statement = tx.prepare(
            "SELECT b.file_id,f.path,b.payload
             FROM callback_argument_batches b
             JOIN files f ON f.id=b.file_id
             ORDER BY f.path",
        )?;
        for row in statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            let payload = serde_json::from_str::<Vec<StoredCallbackArgument>>(&row.2)
                .with_context(|| format!("decode callback argument observations for {}", row.1))?;
            arguments.extend(payload.into_iter().map(
                |(caller, callee, index, formal_name, target, hint, symbol, line, start_byte)| {
                    let symbol = symbol.map(
                        |(
                            language,
                            id,
                            semantic_key,
                            name,
                            qualified_name,
                            start_byte,
                            end_byte,
                            start_line,
                            end_line,
                        )| Symbol {
                            id,
                            semantic_key,
                            language,
                            kind: SymbolKind::Function,
                            name,
                            qualified_name,
                            file: row.1.clone(),
                            start_byte,
                            end_byte,
                            start_line,
                            end_line,
                        },
                    );
                    (
                        row.0,
                        caller,
                        callee,
                        index,
                        formal_name,
                        target,
                        hint,
                        symbol,
                        row.1.clone(),
                        line,
                        start_byte,
                    )
                },
            ));
        }
        drop(statement);

        type CallsiteKey = (i64, String, String, usize, usize);
        let mut resolved_callees = HashMap::<CallsiteKey, Vec<(String, String)>>::new();
        let mut statement = tx.prepare(
            "SELECT u.file_id,u.caller_public_id,
                    u.callee_name,u.evidence_line,
                    u.start_byte,t.target_public_id,t.target_qualified_name
             FROM unresolved_calls u
             JOIN resolved_call_targets t ON t.call_id=u.id
             ORDER BY u.file_id,u.caller_public_id,u.callee_name,u.evidence_line,
                      u.start_byte,t.target_qualified_name,t.target_public_id",
        )?;
        for row in statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? as usize,
                    row.get::<_, i64>(4)? as usize,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            resolved_callees
                .entry((row.0, row.1, row.2, row.3, row.4))
                .or_default()
                .push((row.5, row.6));
        }
        drop(statement);

        let mut invoked_parameters = HashSet::new();
        let mut statement = tx.prepare(
            "SELECT owner_public_id,parameter_index
             FROM callback_parameter_invocations
             ORDER BY owner_public_id,parameter_index",
        )?;
        for invocation in statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            invoked_parameters.insert(invocation);
        }
        drop(statement);

        type Formal = (String, usize);
        let mut delegations = HashMap::<Formal, Vec<(Formal, String)>>::new();
        let mut statement = tx.prepare(
            "SELECT d.file_id,d.owner_public_id,d.parameter_index,d.callee_name,
                    d.argument_index,d.evidence_line,d.call_start_byte
             FROM callback_parameter_delegations d
             ORDER BY d.file_id,d.owner_public_id,d.parameter_index,d.evidence_line,
                      d.call_start_byte,d.callee_name,d.argument_index",
        )?;
        for row in statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as usize,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)? as usize,
                    row.get::<_, i64>(5)? as usize,
                    row.get::<_, i64>(6)? as usize,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            let key = (row.0, row.1.clone(), row.3, row.5, row.6);
            let callees = resolved_callees
                .get(&key)
                .map(Vec::as_slice)
                .unwrap_or_default();
            if callees.len() != 1 {
                continue;
            }
            let (callee_id, callee_qualified) = &callees[0];
            delegations
                .entry((row.1, row.2))
                .or_default()
                .push(((callee_id.clone(), row.4), callee_qualified.clone()));
        }
        drop(statement);
        for edges in delegations.values_mut() {
            edges.sort();
            edges.dedup();
        }

        let mut resolved = 0;
        let mut materialized = 0;
        let mut caller_depths = {
            let mut statement = tx.prepare("SELECT public_id FROM symbols ORDER BY public_id")?;
            let public_ids = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            public_ids
                .into_iter()
                .map(|public_id| (public_id, 0usize))
                .collect::<HashMap<_, _>>()
        };
        for (
            file_id,
            caller_id,
            callee_name,
            argument_index,
            formal_name,
            target_name,
            target_qualified_hint,
            target_symbol,
            file,
            line,
            call_start_byte,
        ) in arguments
        {
            let Some(caller_depth) = caller_depths.get(&caller_id).copied() else {
                continue;
            };
            if target_symbol.is_some() && caller_depth >= INLINE_CALLBACK_DEPTH_CAP {
                continue;
            }
            let callees = resolved_callees
                .get(&(
                    file_id,
                    caller_id.clone(),
                    callee_name.clone(),
                    line,
                    call_start_byte,
                ))
                .map(Vec::as_slice)
                .unwrap_or_default();
            if callees.len() != 1 {
                continue;
            }
            let (callee_id, callee_qualified) = &callees[0];
            let parameter_index = if let Some(formal_name) = formal_name.as_deref() {
                let mut statement = tx.prepare(
                    "SELECT parameter_index
                     FROM python_callback_formals
                     WHERE owner_public_id=?1 AND formal_name=?2
                     ORDER BY parameter_index",
                )?;
                let matches = statement
                    .query_map(params![callee_id, formal_name], |row| {
                        row.get::<_, i64>(0).map(|value| value as usize)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if matches.len() != 1 {
                    continue;
                }
                matches[0]
            } else {
                argument_index
            };
            let initial_formal = (callee_id.clone(), parameter_index);
            let mut terminal_consumers = HashMap::<Formal, Vec<String>>::new();
            let mut queue =
                VecDeque::from([(initial_formal.clone(), vec![callee_qualified.clone()])]);
            let mut visited = HashSet::from([initial_formal.clone()]);
            while let Some((formal, path)) = queue.pop_front() {
                if invoked_parameters.contains(&formal) {
                    terminal_consumers.insert(formal.clone(), path.clone());
                }
                if path.len() >= 16 {
                    continue;
                }
                for (next, next_qualified) in delegations.get(&formal).into_iter().flatten() {
                    if !visited.insert(next.clone()) {
                        continue;
                    }
                    let mut next_path = path.clone();
                    next_path.push(next_qualified.clone());
                    queue.push_back((next.clone(), next_path));
                }
            }
            if terminal_consumers.is_empty() {
                continue;
            }

            let mut targets = if let Some(target) = target_symbol.as_ref() {
                vec![(target.id.clone(), target.qualified_name.clone())]
            } else if let Some(qualified_hint) = target_qualified_hint.as_deref() {
                let mut target_statement = tx.prepare(
                    "SELECT public_id,qualified_name FROM symbols
                     WHERE file_id=?1 AND name=?2 AND qualified_name=?3
                     ORDER BY public_id",
                )?;
                let targets = target_statement
                    .query_map(params![file_id, target_name, qualified_hint], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                targets
            } else {
                let mut target_statement = tx.prepare(
                    "SELECT s.public_id,s.qualified_name
                     FROM symbols s
                     WHERE s.file_id=?1 AND s.name=?2
                     UNION
                     SELECT s.public_id,s.qualified_name
                     FROM import_bindings b
                     JOIN symbols s ON s.public_id=b.target_public_id
                     WHERE b.file_id=?1 AND b.binding_name=?2
                     ORDER BY 2,1",
                )?;
                let targets = target_statement
                    .query_map(params![file_id, target_name], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                targets
            };
            targets.sort();
            targets.dedup();
            if targets.len() != 1 {
                continue;
            }
            let (target_id, target_qualified) = targets.pop().expect("one callback target");
            if let Some(target) = target_symbol.as_ref() {
                let inserted = tx
                    .prepare_cached(
                        "INSERT OR IGNORE INTO symbols(
                            public_id,semantic_key,file_id,language,kind,name,
                            qualified_name,start_byte,end_byte,start_line,end_line
                         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    )?
                    .execute(params![
                        target.id,
                        target.semantic_key,
                        file_id,
                        target.language.to_string(),
                        target.kind.to_string(),
                        target.name,
                        target.qualified_name,
                        target.start_byte as i64,
                        target.end_byte as i64,
                        target.start_line as i64,
                        target.end_line as i64
                    ])?;
                if inserted == 1 {
                    tx.prepare_cached(
                        "INSERT INTO symbol_search(
                            public_id,name,qualified_name,file,segments
                         ) VALUES (?1,?2,?3,?4,?5)",
                    )?
                    .execute(params![
                        target.id,
                        target.name,
                        target.qualified_name,
                        target.file,
                        identifier_segments(&format!("{} {}", target.name, target.qualified_name))
                            .join(" ")
                    ])?;
                    tx.prepare_cached(
                        "INSERT INTO callback_inline_symbols(public_id,file_id)
                         VALUES (?1,?2)",
                    )?
                    .execute(params![target.id, file_id])?;
                    Self::insert_relationship(
                        tx,
                        &Relationship {
                            source_id: caller_id.clone(),
                            target_id: target.id.clone(),
                            kind: RelationshipKind::Contains,
                            evidence: Evidence::new(
                                "dynamic/callback-inline",
                                1.0,
                                format!(
                                    "{} contains an inline callback passed to {} argument {}",
                                    caller_id,
                                    callee_qualified,
                                    argument_index + 1
                                ),
                                &file,
                                line,
                            ),
                        },
                    )?;
                    materialized += 1;
                    resolved += 1;
                }
                caller_depths.insert(target.id.clone(), caller_depth + 1);
            }
            let mut terminal_consumers = terminal_consumers.into_iter().collect::<Vec<_>>();
            terminal_consumers.sort_by(|left, right| left.0.cmp(&right.0));
            for ((consumer_id, consumer_index), path) in terminal_consumers {
                let delegated =
                    (consumer_id.as_str(), consumer_index) != (callee_id.as_str(), parameter_index);
                let provenance = if delegated {
                    "dynamic/callback-delegation"
                } else {
                    "dynamic/callback-argument"
                };
                let confidence = if delegated { 0.94 } else { 0.96 };
                let explanation = if delegated {
                    format!(
                        "{} delegates callback parameter {} through {}; {} directly invokes \
                         parameter {}; registration resolves it to {target_qualified}",
                        callee_qualified,
                        parameter_index + 1,
                        path.join(" -> "),
                        path.last().expect("terminal path"),
                        consumer_index + 1
                    )
                } else {
                    format!(
                        "{callee_qualified} directly invokes callback parameter {}; \
                         registration resolves it to {target_qualified}",
                        parameter_index + 1
                    )
                };
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: consumer_id,
                        target_id: target_id.clone(),
                        kind: RelationshipKind::Calls,
                        evidence: Evidence::new(provenance, confidence, explanation, &file, line),
                    },
                )?;
                resolved += 1;
            }
        }
        Ok((resolved, materialized))
    }

    fn resolve_arkui_builder_flows(tx: &Transaction<'_>) -> Result<usize> {
        tx.execute(
            "DELETE FROM relationships
             WHERE source_public_id IN (
                       SELECT public_id FROM symbols
                       WHERE name LIKE '<BuilderParam adapter %'
                          OR name LIKE '<BuilderParam child %'
                   )
                OR target_public_id IN (
                       SELECT public_id FROM symbols
                       WHERE name LIKE '<BuilderParam adapter %'
                          OR name LIKE '<BuilderParam child %'
                   )",
            [],
        )?;
        tx.execute(
            "DELETE FROM relationships
             WHERE provenance IN (
                 'framework/arkui-builder-param',
                 'framework/arkui-builder-param-dispatch',
                 'framework/arkui-builder-param-adapter',
                 'framework/arkui-builder-param-child'
             )",
            [],
        )?;
        tx.execute(
            "DELETE FROM symbol_search
             WHERE public_id IN (
                 SELECT public_id FROM symbols
                 WHERE name LIKE '<BuilderParam adapter %'
                    OR name LIKE '<BuilderParam child %'
             )",
            [],
        )?;
        tx.execute(
            "DELETE FROM symbols
             WHERE name LIKE '<BuilderParam adapter %'
                OR name LIKE '<BuilderParam child %'",
            [],
        )?;
        let mut batch_statement = tx.prepare(
            "SELECT b.file_id,f.path,b.payload
             FROM arkui_builder_flow_batches b
             JOIN files f ON f.id=b.file_id
             ORDER BY f.path",
        )?;
        let batches = batch_statement
            .query_map([], |row| {
                let payload = row.get::<_, String>(2)?;
                let facts =
                    serde_json::from_str::<ArkuiBuilderFlowFacts>(&payload).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            payload.len(),
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, facts))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(batch_statement);

        let mut imported = HashMap::<(i64, String), Vec<String>>::new();
        let mut import_statement = tx.prepare(
            "SELECT file_id,binding_name,target_public_id
             FROM import_bindings
             ORDER BY file_id,binding_name,target_public_id",
        )?;
        for binding in import_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            imported
                .entry((binding.0, binding.1))
                .or_default()
                .push(binding.2);
        }
        drop(import_statement);

        let mut local = HashMap::<(i64, String), Vec<String>>::new();
        let mut symbol_statement = tx.prepare(
            "SELECT file_id,name,public_id FROM symbols
             ORDER BY file_id,name,public_id",
        )?;
        for symbol in symbol_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            local
                .entry((symbol.0, symbol.1))
                .or_default()
                .push(symbol.2);
        }
        drop(symbol_statement);

        let mut builders = HashSet::<String>::new();
        let mut params = HashMap::<String, Vec<_>>::new();
        let mut invocations = HashMap::<(String, String), Vec<_>>::new();
        for (_, _, facts) in &batches {
            builders.extend(
                facts
                    .builders
                    .iter()
                    .map(|builder| builder.target_id.clone()),
            );
            for param in &facts.params {
                params
                    .entry(param.component_id.clone())
                    .or_default()
                    .push(param.clone());
            }
            for invocation in &facts.invocations {
                invocations
                    .entry((
                        invocation.component_id.clone(),
                        invocation.param_name.clone(),
                    ))
                    .or_default()
                    .push(invocation.clone());
            }
        }
        for declarations in params.values_mut() {
            declarations.sort_by_key(|param| param.ordinal);
        }

        let mut resolved = 0;
        for (file_id, file, facts) in &batches {
            for assignment in &facts.assignments {
                let mut component_ids = local
                    .get(&(*file_id, assignment.component_binding.clone()))
                    .into_iter()
                    .flatten()
                    .chain(
                        imported
                            .get(&(*file_id, assignment.component_binding.clone()))
                            .into_iter()
                            .flatten(),
                    )
                    .filter(|candidate| params.contains_key(*candidate))
                    .cloned()
                    .collect::<Vec<_>>();
                component_ids.sort();
                component_ids.dedup();
                if component_ids.len() != 1 {
                    continue;
                }
                let component_id = &component_ids[0];
                let Some(declarations) = params.get(component_id) else {
                    continue;
                };
                let matching_params = declarations
                    .iter()
                    .filter(|param| {
                        assignment
                            .param_name
                            .as_deref()
                            .is_none_or(|name| name == param.param_name)
                    })
                    .collect::<Vec<_>>();
                if matching_params.len() != 1 {
                    continue;
                }
                let param = matching_params[0];

                let mut target_ids = if let Some(target_id) = &assignment.target_id {
                    vec![target_id.clone()]
                } else if let Some(target) = &assignment.target_symbol {
                    vec![target.id.clone()]
                } else if let Some(binding) = &assignment.target_binding {
                    local
                        .get(&(*file_id, binding.clone()))
                        .into_iter()
                        .flatten()
                        .chain(
                            imported
                                .get(&(*file_id, binding.clone()))
                                .into_iter()
                                .flatten(),
                        )
                        .cloned()
                        .collect::<Vec<_>>()
                } else {
                    continue;
                };
                if assignment.require_decorated_target {
                    target_ids.retain(|target| builders.contains(target));
                }
                target_ids.sort();
                target_ids.dedup();
                if target_ids.len() != 1 {
                    continue;
                }
                let target_id = &target_ids[0];
                if let Some(target) = assignment
                    .target_symbol
                    .as_ref()
                    .filter(|target| target.id == *target_id)
                {
                    let is_child = target.name.starts_with("<BuilderParam child ");
                    let provenance = if is_child {
                        "framework/arkui-builder-param-child"
                    } else {
                        "framework/arkui-builder-param-adapter"
                    };
                    let explanation = if is_child {
                        format!(
                            "{} contains an inline ArkUI BuilderParam child for {}",
                            assignment.caller_id, param.component_name
                        )
                    } else {
                        format!(
                            "{} contains an inline ArkUI BuilderParam adapter for {}.{}",
                            assignment.caller_id, param.component_name, param.param_name
                        )
                    };
                    let inserted = tx
                        .prepare_cached(
                            "INSERT OR IGNORE INTO symbols(
                            public_id, semantic_key, file_id, language, kind, name,
                            qualified_name, start_byte, end_byte, start_line, end_line
                         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                        )?
                        .execute(params![
                            target.id,
                            target.semantic_key,
                            file_id,
                            target.language.to_string(),
                            target.kind.to_string(),
                            target.name,
                            target.qualified_name,
                            target.start_byte as i64,
                            target.end_byte as i64,
                            target.start_line as i64,
                            target.end_line as i64
                        ])?;
                    if inserted == 1 {
                        tx.prepare_cached(
                            "INSERT INTO symbol_search(
                                public_id, name, qualified_name, file, segments
                             ) VALUES (?1,?2,?3,?4,?5)",
                        )?
                        .execute(params![
                            target.id,
                            target.name,
                            target.qualified_name,
                            target.file,
                            identifier_segments(&format!(
                                "{} {}",
                                target.name, target.qualified_name
                            ))
                            .join(" ")
                        ])?;
                        Self::insert_relationship(
                            tx,
                            &Relationship {
                                source_id: assignment.caller_id.clone(),
                                target_id: target.id.clone(),
                                kind: RelationshipKind::Contains,
                                evidence: Evidence::new(
                                    provenance,
                                    1.0,
                                    explanation,
                                    file,
                                    assignment.line,
                                ),
                            },
                        )?;
                        resolved += 1;
                    }
                }
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: assignment.caller_id.clone(),
                        target_id: target_id.clone(),
                        kind: RelationshipKind::Calls,
                        evidence: Evidence::new(
                            "framework/arkui-builder-param",
                            0.97,
                            format!(
                                "{} assigns an ArkUI builder to {}.{}",
                                assignment.caller_id, param.component_name, param.param_name
                            ),
                            file,
                            assignment.line,
                        ),
                    },
                )?;
                resolved += 1;

                if let Some(consumers) =
                    invocations.get(&(component_id.clone(), param.param_name.clone()))
                {
                    for consumer in consumers {
                        Self::insert_relationship(
                            tx,
                            &Relationship {
                                source_id: consumer.owner_id.clone(),
                                target_id: target_id.clone(),
                                kind: RelationshipKind::Calls,
                                evidence: Evidence::new(
                                    "framework/arkui-builder-param-dispatch",
                                    0.97,
                                    format!(
                                        "{}.{} invokes the builder assigned at {}:{}",
                                        param.component_name,
                                        param.param_name,
                                        file,
                                        assignment.line
                                    ),
                                    file,
                                    assignment.line,
                                ),
                            },
                        )?;
                        resolved += 1;
                    }
                }
            }
        }
        Ok(resolved)
    }

    fn resolve_interface_dispatch(tx: &Transaction<'_>) -> Result<usize> {
        const IMPLEMENTATION_FANOUT_CAP: usize = 8;
        tx.execute(
            "DELETE FROM relationships
             WHERE provenance='dynamic/interface-implementation'",
            [],
        )?;

        #[derive(Clone)]
        struct Method {
            id: String,
            name: String,
            owner: String,
            file_id: i64,
            file: String,
            line: usize,
        }

        let mut methods_statement = tx.prepare(
            "SELECT s.public_id,s.name,s.qualified_name,s.file_id,f.path,s.start_line
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE s.kind='method'
             ORDER BY s.file_id,s.qualified_name,s.public_id",
        )?;
        let methods = methods_statement
            .query_map([], |row| {
                let qualified_name = row.get::<_, String>(2)?;
                Ok(Method {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    owner: qualified_name
                        .rsplit_once('.')
                        .map_or(String::new(), |(owner, _)| owner.to_owned()),
                    file_id: row.get(3)?,
                    file: row.get(4)?,
                    line: row.get::<_, i64>(5)? as usize,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(methods_statement);

        let mut methods_by_owner = HashMap::<(i64, String), Vec<Method>>::new();
        let mut methods_by_signature = HashMap::<(i64, String, String), Method>::new();
        for method in methods {
            methods_by_owner
                .entry((method.file_id, method.owner.clone()))
                .or_default()
                .push(method.clone());
            methods_by_signature.insert(
                (method.file_id, method.owner.clone(), method.name.clone()),
                method,
            );
        }

        let mut heritage_statement = tx.prepare(
            "SELECT implementation.qualified_name,implementation.file_id,
                    interface.qualified_name,interface.file_id,interface.public_id
             FROM relationships relationship
             JOIN symbols implementation
               ON implementation.public_id=relationship.source_public_id
             JOIN symbols interface
               ON interface.public_id=relationship.target_public_id
             WHERE relationship.kind='implements'
             ORDER BY interface.public_id,implementation.public_id",
        )?;
        let implementations = heritage_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(heritage_statement);

        let mut candidates = HashMap::<String, Vec<Method>>::new();
        for (implementation, implementation_file, interface, interface_file, _) in implementations {
            let Some(interface_methods) =
                methods_by_owner.get(&(interface_file, interface.clone()))
            else {
                continue;
            };
            for interface_method in interface_methods {
                let Some(implementation_method) = methods_by_signature.get(&(
                    implementation_file,
                    implementation.clone(),
                    interface_method.name.clone(),
                )) else {
                    continue;
                };
                candidates
                    .entry(interface_method.id.clone())
                    .or_default()
                    .push(implementation_method.clone());
            }
        }

        let mut resolved = 0;
        for (interface_method, mut implementation_methods) in candidates {
            implementation_methods.sort_by(|left, right| left.id.cmp(&right.id));
            implementation_methods.dedup_by(|left, right| left.id == right.id);
            if implementation_methods.len() > IMPLEMENTATION_FANOUT_CAP {
                continue;
            }
            for implementation in implementation_methods {
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: interface_method.clone(),
                        target_id: implementation.id,
                        kind: RelationshipKind::Calls,
                        evidence: Evidence::new(
                            "dynamic/interface-implementation",
                            0.94,
                            format!(
                                "interface dispatch may invoke {}.{}",
                                implementation.owner, implementation.name
                            ),
                            implementation.file,
                            implementation.line,
                        ),
                    },
                )?;
                resolved += 1;
            }
        }
        Ok(resolved)
    }

    fn resolve_dynamic_events(tx: &Transaction<'_>) -> Result<usize> {
        const EVENT_FANOUT_CAP: usize = 6;
        type LiteralKey = (String, String, String);
        type ExportTarget = (String, String);
        let mut literals = HashMap::<LiteralKey, HashSet<String>>::new();
        let mut literal_statement = tx.prepare(
            "SELECT f.path,b.export_name,b.member_path,b.channel
             FROM literal_bindings b
             JOIN files f ON f.id=b.file_id
             ORDER BY f.path,b.export_name,b.member_path,b.channel",
        )?;
        for literal in literal_statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })? {
            let (file, export_name, member_path, channel) = literal?;
            literals
                .entry((normalized_module_key(&file), export_name, member_path))
                .or_default()
                .insert(channel);
        }
        drop(literal_statement);

        let mut named_exports = HashMap::<(String, String), Vec<ExportTarget>>::new();
        let mut star_exports = HashMap::<String, Vec<String>>::new();
        let mut export_statement = tx.prepare(
            "SELECT f.path,e.export_name,e.target_file_hint,e.target_name,e.is_star
             FROM module_exports e
             JOIN files f ON f.id=e.file_id
             ORDER BY f.path,e.export_name,e.target_file_hint,e.target_name",
        )?;
        for export in export_statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
            ))
        })? {
            let (file, export_name, target_file_hint, target_name, is_star) = export?;
            let file = normalized_module_key(&file);
            if is_star {
                star_exports.entry(file).or_default().push(target_file_hint);
            } else {
                named_exports
                    .entry((file, export_name))
                    .or_default()
                    .push((target_file_hint, target_name));
            }
        }
        drop(export_statement);
        for targets in named_exports.values_mut() {
            targets.sort();
            targets.dedup();
        }
        for targets in star_exports.values_mut() {
            targets.sort();
            targets.dedup();
        }

        let mut event_statement = tx.prepare(
            "SELECT id,file_id,owner_public_id,receiver,channel,
                    channel_target_file_hint,channel_export_name,channel_member_path,
                    action,callback_name,evidence_file,evidence_line
             FROM dynamic_events
             ORDER BY evidence_file,evidence_line,id",
        )?;
        let raw_events = event_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)? as usize,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(event_statement);
        let mut events = Vec::new();
        for (
            id,
            file_id,
            owner,
            receiver,
            channel,
            target_file_hint,
            export_name,
            member_path,
            action,
            callback,
            file,
            line,
        ) in raw_events
        {
            let effective_channel = if channel.is_empty() {
                let (Some(target_file_hint), Some(export_name)) = (target_file_hint, export_name)
                else {
                    continue;
                };
                resolve_exported_literal(
                    &target_file_hint,
                    &export_name,
                    member_path.as_deref().unwrap_or_default(),
                    &literals,
                    &named_exports,
                    &star_exports,
                    &mut HashSet::new(),
                    0,
                )
            } else {
                Some(channel)
            };
            if let Some(channel) = effective_channel {
                events.push((
                    id, file_id, owner, receiver, channel, action, callback, file, line,
                ));
            }
        }

        let mut callback_statement = tx.prepare(
            "SELECT rel.source_public_id,target.name,rel.target_public_id
             FROM relationships rel
             JOIN symbols target ON target.public_id=rel.target_public_id
             WHERE rel.provenance IN (
                'tree-sitter/callback-registration',
                'framework/ohos-emitter-registration',
                'framework/ohos-emitter-inline-registration'
              )
             ORDER BY rel.source_public_id,target.name,rel.target_public_id",
        )?;
        let mut callback_targets = HashMap::<(String, String), Vec<String>>::new();
        for callback in callback_statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })? {
            let (owner, name, target) = callback?;
            callback_targets
                .entry((owner, name))
                .or_default()
                .push(target);
        }
        drop(callback_statement);
        for targets in callback_targets.values_mut() {
            targets.sort();
            targets.dedup();
        }

        type EventKey = (i64, String, String);
        type RegisteredTarget = (String, String, usize);
        let mut registrations = HashMap::<EventKey, Vec<RegisteredTarget>>::new();
        for (_, file_id, owner, receiver, channel, action, callback, file, line) in &events {
            if action != "register" {
                continue;
            }
            let Some(callback) = callback else {
                continue;
            };
            let Some(targets) = callback_targets.get(&(owner.clone(), callback.clone())) else {
                continue;
            };
            let scope_file_id = if receiver.starts_with("ohos-emitter@") {
                0
            } else {
                *file_id
            };
            let entry = registrations
                .entry((scope_file_id, receiver.clone(), channel.clone()))
                .or_default();
            entry.extend(
                targets
                    .iter()
                    .cloned()
                    .map(|target| (target, file.clone(), *line)),
            );
        }
        for targets in registrations.values_mut() {
            targets.sort();
            targets.dedup_by(|left, right| left.0 == right.0);
        }

        let mut resolved = 0;
        for (_, file_id, dispatcher_id, receiver, channel, action, _, file, line) in events {
            if action != "dispatch" {
                continue;
            }
            let is_ohos_emitter = receiver.starts_with("ohos-emitter@");
            let scope_file_id = if is_ohos_emitter { 0 } else { file_id };
            let Some(targets) =
                registrations.get(&(scope_file_id, receiver.clone(), channel.clone()))
            else {
                continue;
            };
            if targets.len() > EVENT_FANOUT_CAP {
                continue;
            }
            for (target_id, registration_file, registration_line) in targets {
                if &dispatcher_id == target_id {
                    continue;
                }
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: dispatcher_id.clone(),
                        target_id: target_id.clone(),
                        kind: RelationshipKind::Calls,
                        evidence: Evidence::new(
                            if is_ohos_emitter {
                                "framework/ohos-emitter"
                            } else {
                                "dynamic/event-registration"
                            },
                            if is_ohos_emitter { 0.97 } else { 0.92 },
                            if is_ohos_emitter {
                                format!(
                                    "Harmony emitter channel `{channel}` dispatches to a handler \
                                     registered at {registration_file}:{registration_line}"
                                )
                            } else {
                                format!(
                                    "literal event `{channel}` on `{receiver}` dispatches to a \
                                     handler registered at \
                                     {registration_file}:{registration_line}"
                                )
                            },
                            &file,
                            line,
                        ),
                    },
                )?;
                resolved += 1;
            }
        }
        Ok(resolved)
    }

    fn resolve_structural_references(tx: &Transaction<'_>) -> Result<usize> {
        const REFERENCE_FANOUT_CAP: usize = 8;
        tx.execute(
            "DELETE FROM relationships
             WHERE provenance IN (
                'tree-sitter/import',
                'tree-sitter/heritage',
                'tree-sitter/function-reference'
             )",
            [],
        )?;
        tx.execute("DELETE FROM import_bindings", [])?;
        let mut reference_statement = tx.prepare(
            "SELECT u.source_public_id,u.target_name,u.binding_name,u.target_file_hint,
                    u.kind,u.provenance,u.confidence,u.explanation,u.evidence_file,
                    u.evidence_line,s.language,u.file_id
             FROM unresolved_references u
             JOIN symbols s ON s.public_id=u.source_public_id
             WHERE u.provenance<>'framework/arkui-entry'
             ORDER BY CASE WHEN u.kind='imports' THEN 0 ELSE 1 END,
                      u.evidence_file,u.evidence_line,u.id",
        )?;
        let references = reference_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, f64>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)? as usize,
                    row.get::<_, String>(10)?,
                    row.get::<_, i64>(11)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(reference_statement);

        let mut resolved = 0;
        type ReferenceCandidate = (String, String, i64);
        let mut reference_candidates: Option<HashMap<(String, String), Vec<ReferenceCandidate>>> =
            None;
        let mut reference_imports: Option<HashMap<(i64, String, String), Vec<ReferenceCandidate>>> =
            None;
        for (
            source_id,
            target_name,
            binding_name,
            target_file_hint,
            kind,
            provenance,
            base_confidence,
            explanation,
            file,
            line,
            language,
            file_id,
        ) in references
        {
            if kind == "references" {
                if reference_candidates.is_none() {
                    let mut direct = HashMap::<(String, String), Vec<ReferenceCandidate>>::new();
                    let mut statement = tx.prepare(
                        "SELECT name,language,public_id,qualified_name,file_id
                         FROM symbols
                         WHERE kind IN ('function','method','component')
                         ORDER BY name,language,qualified_name,public_id",
                    )?;
                    for candidate in statement
                        .query_map([], |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, i64>(4)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                    {
                        direct.entry((candidate.0, candidate.1)).or_default().push((
                            candidate.2,
                            candidate.3,
                            candidate.4,
                        ));
                    }
                    reference_candidates = Some(direct);

                    let mut imported =
                        HashMap::<(i64, String, String), Vec<ReferenceCandidate>>::new();
                    let mut statement = tx.prepare(
                        "SELECT b.file_id,b.binding_name,s.language,s.public_id,
                                s.qualified_name,s.file_id
                         FROM import_bindings b
                         JOIN symbols s ON s.public_id=b.target_public_id
                         WHERE s.kind IN ('function','method','component')
                         ORDER BY b.file_id,b.binding_name,s.language,s.qualified_name,s.public_id",
                    )?;
                    for candidate in statement
                        .query_map([], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, i64>(5)?,
                            ))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                    {
                        imported
                            .entry((candidate.0, candidate.1, candidate.2))
                            .or_default()
                            .push((candidate.3, candidate.4, candidate.5));
                    }
                    reference_imports = Some(imported);
                }
                let mut ranked = HashMap::<String, (String, i64)>::new();
                if let Some(direct) = reference_candidates
                    .as_ref()
                    .and_then(|candidates| candidates.get(&(target_name.clone(), language.clone())))
                {
                    for (candidate_id, qualified_name, candidate_file_id) in direct {
                        if candidate_id != &source_id {
                            ranked.insert(
                                candidate_id.clone(),
                                (
                                    qualified_name.clone(),
                                    if *candidate_file_id == file_id { 0 } else { 2 },
                                ),
                            );
                        }
                    }
                }
                if let Some(imported) = reference_imports.as_ref().and_then(|candidates| {
                    candidates.get(&(file_id, binding_name.clone(), language.clone()))
                }) {
                    for (candidate_id, qualified_name, _) in imported {
                        if candidate_id != &source_id {
                            ranked
                                .entry(candidate_id.clone())
                                .and_modify(|candidate| candidate.1 = candidate.1.min(1))
                                .or_insert((qualified_name.clone(), 1));
                        }
                    }
                }
                let mut candidates = ranked
                    .into_iter()
                    .map(|(candidate_id, (qualified_name, rank))| {
                        (candidate_id, qualified_name, rank)
                    })
                    .collect::<Vec<_>>();
                candidates.sort_by(|left, right| {
                    left.2
                        .cmp(&right.2)
                        .then_with(|| left.1.cmp(&right.1))
                        .then_with(|| left.0.cmp(&right.0))
                });
                let Some(best_rank) = candidates.first().map(|candidate| candidate.2) else {
                    continue;
                };
                let best = candidates
                    .iter()
                    .take_while(|candidate| candidate.2 == best_rank)
                    .collect::<Vec<_>>();
                if best.len() != 1 {
                    continue;
                }
                let (target_id, qualified_name, _) = best[0];
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id,
                        target_id: target_id.clone(),
                        kind: RelationshipKind::References,
                        evidence: Evidence::new(
                            &provenance,
                            base_confidence,
                            format!("{explanation}; resolves uniquely to {qualified_name}"),
                            &file,
                            line,
                        ),
                    },
                )?;
                resolved += 1;
                continue;
            }
            let mut target_statement = tx.prepare(
                "SELECT s.public_id,s.qualified_name,f.path,s.file_id
                 FROM symbols s JOIN files f ON f.id=s.file_id
                 WHERE s.name=?1 AND s.public_id<>?2
                   AND (
                     s.language=?3
                     OR (?3='arkts' AND s.language='typescript')
                     OR (?3='typescript' AND s.language='arkts')
                     OR (
                       ?3='astro'
                       AND s.language IN ('typescript','tsx','javascript','jsx')
                     )
                   )
                 ORDER BY s.qualified_name,s.public_id",
            )?;
            let mut targets = target_statement
                .query_map(params![target_name, source_id, language], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if let Some(hint) = target_file_hint.as_deref() {
                targets = targets
                    .iter()
                    .filter(|target| module_hint_matches(hint, &target.2))
                    .cloned()
                    .collect();
            }
            if kind != "imports" && target_file_hint.is_none() {
                let same_file = targets
                    .iter()
                    .filter(|target| target.3 == file_id)
                    .cloned()
                    .collect::<Vec<_>>();
                if !same_file.is_empty() {
                    targets = same_file;
                } else {
                    let imported_ids = {
                        let mut statement = tx.prepare_cached(
                            "SELECT target_public_id
                             FROM import_bindings
                             WHERE file_id=?1 AND binding_name=?2",
                        )?;
                        let ids = statement
                            .query_map(params![file_id, binding_name], |row| {
                                row.get::<_, String>(0)
                            })?
                            .collect::<rusqlite::Result<Vec<_>>>()?;
                        ids
                    };
                    let imported = targets
                        .iter()
                        .filter(|target| imported_ids.contains(&target.0))
                        .cloned()
                        .collect::<Vec<_>>();
                    if !imported.is_empty() {
                        targets = imported;
                    } else if targets.len() != 1 {
                        targets.clear();
                    }
                }
            }
            if targets.len() > REFERENCE_FANOUT_CAP {
                targets.clear();
            }
            let confidence = if targets.len() == 1 {
                base_confidence
            } else {
                base_confidence.min(0.55)
            };
            for (target_id, qualified_name, _, _) in targets {
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: source_id.clone(),
                        target_id: target_id.clone(),
                        kind: parse_relationship_kind(&kind),
                        evidence: Evidence::new(
                            &provenance,
                            confidence,
                            if confidence == base_confidence {
                                explanation.clone()
                            } else {
                                format!(
                                    "{explanation}; {qualified_name} is one of multiple candidates"
                                )
                            },
                            &file,
                            line,
                        ),
                    },
                )?;
                if kind == "imports" {
                    tx.execute(
                        "INSERT OR REPLACE INTO import_bindings(
                            file_id,binding_name,target_public_id,confidence
                         ) VALUES (?1,?2,?3,?4)",
                        params![file_id, binding_name, target_id, confidence],
                    )?;
                }
                resolved += 1;
            }
        }
        Ok(resolved)
    }

    fn insert_relationship(tx: &Transaction<'_>, relationship: &Relationship) -> Result<()> {
        tx.prepare_cached(
            "INSERT OR REPLACE INTO relationships(
                source_public_id, target_public_id, kind, provenance, confidence,
                explanation, evidence_file, evidence_line, evidence_site
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        )?
        .execute(params![
            relationship.source_id,
            relationship.target_id,
            relationship.kind.to_string(),
            relationship.evidence.provenance,
            relationship.evidence.confidence,
            relationship.evidence.explanation,
            relationship.evidence.file,
            relationship.evidence.line as i64,
            relationship.evidence.site.unwrap_or(0) as i64
        ])?;
        Ok(())
    }

    pub fn find_symbols_by_name(&self, name: &str) -> Result<Vec<Symbol>> {
        ResourceBudget::identifier(name)?;
        let mut statement = self.connection.prepare(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE s.name=?1 ORDER BY s.qualified_name,f.path LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![
                name,
                ResourceBudget::MAX_IMPACT_NODES.saturating_add(1) as i64
            ],
            Self::symbol_from_row,
        )?;
        let symbols = Self::collect_symbols(rows)?;
        ensure_query_cardinality(&symbols, "symbol lookup")?;
        Ok(symbols)
    }

    pub fn find_symbols(&self, identifier: &str) -> Result<Vec<Symbol>> {
        ResourceBudget::identifier(identifier)?;
        let mut statement = self.connection.prepare(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE s.public_id=?1 OR s.name=?1 OR s.qualified_name=?1
             ORDER BY s.qualified_name,f.path LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![
                identifier,
                ResourceBudget::MAX_IMPACT_NODES.saturating_add(1) as i64
            ],
            Self::symbol_from_row,
        )?;
        let symbols = Self::collect_symbols(rows)?;
        ensure_query_cardinality(&symbols, "symbol lookup")?;
        Ok(symbols)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_filtered(query, None, limit)
    }

    pub fn search_filtered(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        ResourceBudget::query(query)?;
        let limit = ResourceBudget::result_limit(limit)?;
        self.search_filtered_with_limit(query, kind, limit)
    }

    pub(crate) fn search_candidates(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        ResourceBudget::query(query)?;
        let limit = ResourceBudget::search_candidate_limit(limit)?;
        self.search_filtered_with_limit(query, None, limit)
    }

    fn search_filtered_with_limit(
        &self,
        query: &str,
        kind: Option<SymbolKind>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let terms = search_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        anyhow::ensure!(
            terms.len() <= ResourceBudget::MAX_SEARCH_TERMS,
            "query exceeds the {}-term limit",
            ResourceBudget::MAX_SEARCH_TERMS
        );
        let escaped = terms
            .iter()
            .map(|part| format!("\"{}\"*", part.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let candidate_limit = limit.saturating_mul(4).max(limit);
        let sql = if kind.is_some() {
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line,
                    bm25(symbol_search)
             FROM symbol_search
             JOIN symbols s ON s.public_id=symbol_search.public_id
             JOIN files f ON f.id=s.file_id
             WHERE symbol_search MATCH ?1 AND s.kind=?2
             ORDER BY bm25(symbol_search) LIMIT ?3"
        } else {
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line,
                    bm25(symbol_search)
             FROM symbol_search
             JOIN symbols s ON s.public_id=symbol_search.public_id
             JOIN files f ON f.id=s.file_id
             WHERE symbol_search MATCH ?1 ORDER BY bm25(symbol_search) LIMIT ?2"
        };
        let mut statement = self.connection.prepare(sql)?;
        let mut map_row = |row: &rusqlite::Row<'_>| {
            let symbol = Self::symbol_from_row(row)?;
            let mut score = -row.get::<_, f64>(11)?;
            let normalized_query = query.trim().to_lowercase();
            let name = symbol.name.to_lowercase();
            let qualified_name = symbol.qualified_name.to_lowercase();
            if name == normalized_query || qualified_name == normalized_query {
                score += 10.0;
            } else if terms.contains(&name) {
                score += 6.0;
            } else if terms.iter().any(|term| name.starts_with(term)) {
                score += 3.0;
            }
            Ok(SearchHit { symbol, score })
        };
        let mut hits: Vec<SearchHit> = if let Some(kind) = kind {
            statement
                .query_map(
                    params![escaped, kind.to_string(), candidate_limit as i64],
                    &mut map_row,
                )?
                .collect::<rusqlite::Result<_>>()?
        } else {
            statement
                .query_map(params![escaped, candidate_limit as i64], &mut map_row)?
                .collect::<rusqlite::Result<_>>()?
        };
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.symbol.qualified_name.cmp(&right.symbol.qualified_name))
                .then_with(|| left.symbol.file.cmp(&right.symbol.file))
                .then_with(|| left.symbol.id.cmp(&right.symbol.id))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn related(
        &self,
        symbol_id: &str,
        incoming: bool,
        kind: RelationshipKind,
    ) -> Result<Vec<(Symbol, Evidence)>> {
        let related = self.related_with_limit(
            symbol_id,
            incoming,
            kind,
            ResourceBudget::MAX_IMPACT_NODES.saturating_add(1),
        )?;
        ensure_query_cardinality(&related, "relationship lookup")?;
        Ok(related)
    }

    pub(crate) fn related_limited(
        &self,
        symbol_id: &str,
        incoming: bool,
        kind: RelationshipKind,
        limit: usize,
    ) -> Result<Vec<(Symbol, Evidence)>> {
        let limit = ResourceBudget::result_limit(limit)?;
        self.related_with_limit(symbol_id, incoming, kind, limit)
    }

    fn related_with_limit(
        &self,
        symbol_id: &str,
        incoming: bool,
        kind: RelationshipKind,
        limit: usize,
    ) -> Result<Vec<(Symbol, Evidence)>> {
        ResourceBudget::identifier(symbol_id)?;
        let (join_side, filter_side) = if incoming {
            ("r.source_public_id", "r.target_public_id")
        } else {
            ("r.target_public_id", "r.source_public_id")
        };
        let sql = format!(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line,
                    r.provenance,r.confidence,r.explanation,r.evidence_file,r.evidence_line,
                    r.evidence_site
             FROM relationships r JOIN symbols s ON s.public_id={join_side}
             JOIN files f ON f.id=s.file_id
             WHERE {filter_side}=?1 AND r.kind=?2
             ORDER BY r.confidence DESC,s.qualified_name,r.evidence_file,
                      r.evidence_line,r.evidence_site
             LIMIT ?3"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows =
            statement.query_map(params![symbol_id, kind.to_string(), limit as i64], |row| {
                Ok((
                    Self::symbol_from_row(row)?,
                    Evidence {
                        provenance: row.get(11)?,
                        confidence: row.get(12)?,
                        explanation: row.get(13)?,
                        file: row.get(14)?,
                        line: row.get::<_, i64>(15)? as usize,
                        site: (row.get::<_, i64>(16)? != 0)
                            .then(|| row.get::<_, i64>(16).unwrap_or_default() as usize),
                    },
                ))
            })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn collect_symbols(
        rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Symbol>>,
    ) -> Result<Vec<Symbol>> {
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn symbol_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Symbol> {
        let language: String = row.get(2)?;
        let kind: String = row.get(3)?;
        Ok(Symbol {
            id: row.get(0)?,
            semantic_key: row.get(1)?,
            language: parse_language(&language),
            kind: parse_symbol_kind(&kind),
            name: row.get(4)?,
            qualified_name: row.get(5)?,
            file: row.get(6)?,
            start_byte: row.get::<_, i64>(7)? as usize,
            end_byte: row.get::<_, i64>(8)? as usize,
            start_line: row.get::<_, i64>(9)? as usize,
            end_line: row.get::<_, i64>(10)? as usize,
        })
    }
}

const C_MACRO_EXPANSION_DEPTH_CAP: usize = 6;
const C_MACRO_ENVIRONMENT_CAP: usize = 32;
const C_MACRO_ARGUMENT_CAP: usize = 64;
const C_MACRO_OUTPUT_BYTES_CAP: usize = 64 * 1024;
const C_MACRO_TOKEN_WORK_CAP: usize = 4_096;
const C_MACRO_REPLAY_WORK_CAP: usize = 100_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CPreprocessorTruth {
    True,
    False,
    Unknown,
}

#[derive(Clone, Default)]
struct CMacroState {
    macros: HashMap<String, CPreprocessorEventFact>,
    undefined: HashSet<String>,
    branches: HashMap<String, usize>,
    fingerprint: [u8; 32],
}
type CGuardCatalog = HashMap<usize, Vec<CPreprocessorGuardFact>>;

struct CMacroResolver<'a> {
    files: &'a HashMap<String, CFunctionPointerFacts>,
    all_paths: &'a HashSet<String>,
    replay_work: Cell<usize>,
}

impl<'a> CMacroResolver<'a> {
    fn new(
        files: &'a HashMap<String, CFunctionPointerFacts>,
        all_paths: &'a HashSet<String>,
    ) -> Self {
        Self {
            files,
            all_paths,
            replay_work: Cell::new(0),
        }
    }

    fn states_at(&self, file: &str, site: usize) -> Vec<CMacroState> {
        if self.replay_work.get() >= C_MACRO_REPLAY_WORK_CAP {
            return Vec::new();
        }
        let Some(facts) = self.files.get(file) else {
            return Vec::new();
        };
        let initial_work = facts
            .compiler_macro_contexts
            .iter()
            .flatten()
            .map(|action| {
                c_compiler_macro_action_weight(action)
                    .saturating_add(63)
                    .saturating_div(64)
                    .saturating_add(1)
            })
            .fold(0usize, usize::saturating_add);
        if self.replay_work.get().saturating_add(initial_work) > C_MACRO_REPLAY_WORK_CAP {
            return Vec::new();
        }
        let mut states = if facts.compiler_macro_contexts.is_empty() {
            vec![CMacroState::default()]
        } else {
            facts
                .compiler_macro_contexts
                .iter()
                .map(|actions| {
                    let mut state = CMacroState::default();
                    for action in actions {
                        apply_c_compiler_macro_action(&mut state, action);
                    }
                    state
                })
                .collect()
        };
        dedup_c_macro_states(&mut states);
        let mut visiting = HashSet::new();
        let mut work = initial_work;
        if !self.replay_file(file, site, file, 0, &mut visiting, &mut states, &mut work) {
            self.replay_work
                .set(self.replay_work.get().saturating_add(work));
            return Vec::new();
        }
        self.replay_work
            .set(self.replay_work.get().saturating_add(work));
        dedup_c_macro_states(&mut states);
        states
    }

    #[allow(clippy::too_many_arguments)]
    fn replay_file(
        &self,
        file: &str,
        before: usize,
        branch_context: &str,
        depth: usize,
        visiting: &mut HashSet<String>,
        states: &mut Vec<CMacroState>,
        work: &mut usize,
    ) -> bool {
        const INCLUDE_DEPTH_CAP: usize = 16;
        if depth > INCLUDE_DEPTH_CAP
            || self.replay_work.get().saturating_add(*work) >= C_MACRO_REPLAY_WORK_CAP
            || !visiting.insert(file.to_owned())
        {
            return depth <= INCLUDE_DEPTH_CAP;
        }
        let Some(facts) = self.files.get(file) else {
            visiting.remove(file);
            return true;
        };
        enum Item<'b> {
            Include(&'b crate::model::CIncludeFact),
            Event(&'b CPreprocessorEventFact),
        }
        let mut items = facts
            .includes
            .iter()
            .map(|include| (include.site_start_byte, Item::Include(include)))
            .chain(
                facts
                    .preprocessor_events
                    .iter()
                    .map(|event| (event.site_start_byte, Item::Event(event))),
            )
            .filter(|(position, _)| *position < before)
            .collect::<Vec<_>>();
        items.sort_by_key(|(position, _)| *position);
        let catalog = c_guard_catalog(facts);
        let mut last_guard_position = HashMap::<usize, usize>::new();
        for (position, guards) in facts
            .includes
            .iter()
            .map(|fact| (fact.site_start_byte, &fact.guard_path))
            .chain(
                facts
                    .preprocessor_events
                    .iter()
                    .map(|fact| (fact.site_start_byte, &fact.guard_path)),
            )
            .chain(
                facts
                    .bindings
                    .iter()
                    .map(|fact| (fact.site_start_byte, &fact.guard_path)),
            )
            .chain(
                facts
                    .macro_initializers
                    .iter()
                    .map(|fact| (fact.site_start_byte, &fact.guard_path)),
            )
        {
            for guard in guards {
                last_guard_position
                    .entry(guard.group_start_byte)
                    .and_modify(|last| *last = (*last).max(position))
                    .or_insert(position);
            }
        }
        let mut retirement_schedule = last_guard_position
            .into_iter()
            .map(|(group, last)| (last, group))
            .collect::<Vec<_>>();
        retirement_schedule.sort();
        let mut retirement_cursor = 0usize;
        for (position, item) in items {
            while retirement_schedule
                .get(retirement_cursor)
                .is_some_and(|(last, _)| *last < position)
            {
                let group = retirement_schedule[retirement_cursor].1;
                retire_c_macro_branches(states, branch_context, std::iter::once(group));
                *work = work.saturating_add(states.len().max(1));
                retirement_cursor += 1;
            }
            dedup_c_macro_states(states);
            *work = work.saturating_add(states.len().max(1));
            if self.replay_work.get().saturating_add(*work) > C_MACRO_REPLAY_WORK_CAP {
                visiting.remove(file);
                return false;
            }
            match item {
                Item::Include(include) => {
                    *work = work.saturating_add(c_macro_state_weight(states));
                    if self.replay_work.get().saturating_add(*work) > C_MACRO_REPLAY_WORK_CAP {
                        visiting.remove(file);
                        return false;
                    }
                    let mut next = Vec::new();
                    for state in states.iter() {
                        for (variant, selected) in split_c_macro_state_for_guards(
                            state,
                            &include.guard_path,
                            &catalog,
                            branch_context,
                        ) {
                            if selected {
                                if let Some(target) =
                                    resolve_c_include(file, include, self.all_paths)
                                {
                                    let mut included = vec![variant];
                                    let include_context = format!(
                                        "{branch_context}>{file}:{}->{target}",
                                        include.site_start_byte
                                    );
                                    if !self.replay_file(
                                        &target,
                                        usize::MAX,
                                        &include_context,
                                        depth + 1,
                                        visiting,
                                        &mut included,
                                        work,
                                    ) {
                                        visiting.remove(file);
                                        return false;
                                    }
                                    next.extend(included);
                                } else {
                                    next.push(variant);
                                }
                            } else {
                                next.push(variant);
                            }
                        }
                    }
                    *states = next;
                }
                Item::Event(event) => {
                    if event.guard_path.is_empty() {
                        for state in states.iter_mut() {
                            apply_c_preprocessor_event(state, event);
                        }
                    } else {
                        *work = work.saturating_add(c_macro_state_weight(states));
                        if self.replay_work.get().saturating_add(*work) > C_MACRO_REPLAY_WORK_CAP {
                            visiting.remove(file);
                            return false;
                        }
                        let mut next = Vec::new();
                        for state in states.iter() {
                            for (mut variant, selected) in split_c_macro_state_for_guards(
                                state,
                                &event.guard_path,
                                &catalog,
                                branch_context,
                            ) {
                                if selected {
                                    apply_c_preprocessor_event(&mut variant, event);
                                }
                                next.push(variant);
                            }
                        }
                        *states = next;
                    }
                }
            }
            dedup_c_macro_states(states);
            if states.len() > C_MACRO_ENVIRONMENT_CAP {
                visiting.remove(file);
                return false;
            }
        }
        while retirement_schedule
            .get(retirement_cursor)
            .is_some_and(|(last, _)| *last < before)
        {
            let group = retirement_schedule[retirement_cursor].1;
            retire_c_macro_branches(states, branch_context, std::iter::once(group));
            *work = work.saturating_add(states.len().max(1));
            retirement_cursor += 1;
        }
        dedup_c_macro_states(states);
        if self.replay_work.get().saturating_add(*work) > C_MACRO_REPLAY_WORK_CAP {
            visiting.remove(file);
            return false;
        }
        visiting.remove(file);
        true
    }

    fn guard_truth(
        &self,
        file: &str,
        site: usize,
        guards: &[CPreprocessorGuardFact],
    ) -> CPreprocessorTruth {
        let Some(facts) = self.files.get(file) else {
            return CPreprocessorTruth::False;
        };
        let states = self.states_at(file, site);
        if states.is_empty() {
            return CPreprocessorTruth::False;
        }
        let catalog = c_guard_catalog(facts);
        let mut saw_true = false;
        let mut saw_false = false;
        let mut saw_unknown = false;
        for state in &states {
            match c_guard_truth(guards, &catalog, state, file) {
                CPreprocessorTruth::True => saw_true = true,
                CPreprocessorTruth::False => saw_false = true,
                CPreprocessorTruth::Unknown => saw_unknown = true,
            }
        }
        if saw_unknown || (saw_true && saw_false) {
            CPreprocessorTruth::Unknown
        } else if saw_true {
            CPreprocessorTruth::True
        } else {
            CPreprocessorTruth::False
        }
    }
}

fn c_macro_state_weight(states: &[CMacroState]) -> usize {
    states
        .iter()
        .map(|state| {
            let entry_weight = state
                .macros
                .len()
                .saturating_add(state.undefined.len())
                .saturating_add(state.branches.len())
                .saturating_add(1);
            let byte_weight = state
                .macros
                .values()
                .map(|event| {
                    event
                        .name
                        .len()
                        .saturating_add(event.replacement.len())
                        .saturating_add(
                            event
                                .parameters
                                .iter()
                                .map(String::len)
                                .fold(0usize, usize::saturating_add),
                        )
                })
                .fold(0usize, usize::saturating_add)
                .saturating_add(63)
                / 64;
            entry_weight.saturating_add(byte_weight)
        })
        .fold(0usize, usize::saturating_add)
}

fn retire_c_macro_branches(
    states: &mut [CMacroState],
    branch_context: &str,
    groups: impl Iterator<Item = usize>,
) {
    for group in groups {
        let branch_key = format!("{branch_context}:{group}");
        for state in states.iter_mut() {
            if let Some(selected) = state.branches.remove(&branch_key) {
                toggle_c_macro_fingerprint(
                    &mut state.fingerprint,
                    c_macro_branch_digest(&branch_key, selected),
                );
            }
        }
    }
}

fn c_guard_catalog(facts: &CFunctionPointerFacts) -> CGuardCatalog {
    let mut catalog = CGuardCatalog::new();
    let guards = facts.preprocessor_guards.iter();
    for guard in guards {
        let branches = catalog.entry(guard.group_start_byte).or_default();
        if !branches
            .iter()
            .any(|candidate| candidate.branch_index == guard.branch_index)
        {
            branches.push(guard.clone());
        }
    }
    for branches in catalog.values_mut() {
        branches.sort_by_key(|guard| guard.branch_index);
    }
    catalog
}

fn c_guard_truth(
    guards: &[CPreprocessorGuardFact],
    catalog: &CGuardCatalog,
    state: &CMacroState,
    branch_context: &str,
) -> CPreprocessorTruth {
    let variants = split_c_macro_state_for_guards(state, guards, catalog, branch_context);
    let selected = variants.iter().filter(|(_, selected)| *selected).count();
    if selected == 0 {
        CPreprocessorTruth::False
    } else if selected == variants.len() {
        CPreprocessorTruth::True
    } else {
        CPreprocessorTruth::Unknown
    }
}

fn split_c_macro_state_for_guards(
    state: &CMacroState,
    guards: &[CPreprocessorGuardFact],
    catalog: &CGuardCatalog,
    branch_context: &str,
) -> Vec<(CMacroState, bool)> {
    let mut variants = vec![state.clone()];
    let mut groups = guards
        .iter()
        .map(|guard| guard.group_start_byte)
        .collect::<Vec<_>>();
    groups.sort();
    groups.dedup();
    for group in groups {
        let branch_key = format!("{branch_context}:{group}");
        let mut next = Vec::new();
        for variant in variants {
            if variant.branches.contains_key(&branch_key) {
                next.push(variant);
                continue;
            }
            let Some(branches) = catalog.get(&group) else {
                next.push(variant);
                continue;
            };
            for selected in c_feasible_preprocessor_branches(branches, &variant) {
                let mut branch_variant = variant.clone();
                branch_variant.branches.insert(branch_key.clone(), selected);
                toggle_c_macro_fingerprint(
                    &mut branch_variant.fingerprint,
                    c_macro_branch_digest(&branch_key, selected),
                );
                next.push(branch_variant);
            }
        }
        dedup_c_macro_states(&mut next);
        if next.len() > C_MACRO_ENVIRONMENT_CAP {
            return Vec::new();
        }
        variants = next;
    }
    variants
        .into_iter()
        .map(|variant| {
            let selected = guards.iter().all(|guard| {
                let branch_key = format!("{branch_context}:{}", guard.group_start_byte);
                variant
                    .branches
                    .get(&branch_key)
                    .is_some_and(|branch| *branch == guard.branch_index)
            });
            (variant, selected)
        })
        .collect()
}

fn c_feasible_preprocessor_branches(
    branches: &[CPreprocessorGuardFact],
    state: &CMacroState,
) -> Vec<usize> {
    const NO_BRANCH: usize = usize::MAX;
    let mut feasible = Vec::new();
    let mut can_fall_through = true;
    for branch in branches {
        if !can_fall_through {
            break;
        }
        if branch.kind == CPreprocessorGuardKind::Else {
            feasible.push(branch.branch_index);
            can_fall_through = false;
            break;
        }
        match c_condition_truth(branch, state) {
            CPreprocessorTruth::True => {
                feasible.push(branch.branch_index);
                can_fall_through = false;
            }
            CPreprocessorTruth::False => {}
            CPreprocessorTruth::Unknown => {
                feasible.push(branch.branch_index);
            }
        }
    }
    if can_fall_through {
        feasible.push(NO_BRANCH);
    }
    feasible.sort();
    feasible.dedup();
    feasible
}

fn c_condition_truth(guard: &CPreprocessorGuardFact, state: &CMacroState) -> CPreprocessorTruth {
    match guard.kind {
        CPreprocessorGuardKind::Ifdef | CPreprocessorGuardKind::Elifdef => {
            if state.macros.contains_key(guard.condition.trim()) {
                CPreprocessorTruth::True
            } else if state.undefined.contains(guard.condition.trim()) {
                CPreprocessorTruth::False
            } else {
                CPreprocessorTruth::Unknown
            }
        }
        CPreprocessorGuardKind::Ifndef | CPreprocessorGuardKind::Elifndef => {
            if state.macros.contains_key(guard.condition.trim()) {
                CPreprocessorTruth::False
            } else if state.undefined.contains(guard.condition.trim()) {
                CPreprocessorTruth::True
            } else {
                CPreprocessorTruth::Unknown
            }
        }
        CPreprocessorGuardKind::Else => CPreprocessorTruth::True,
        CPreprocessorGuardKind::If | CPreprocessorGuardKind::Elif => {
            c_if_expression_truth(guard.condition.trim(), state)
        }
    }
}

fn c_if_expression_truth(expression: &str, state: &CMacroState) -> CPreprocessorTruth {
    let expression = strip_c_preprocessor_comments(expression);
    c_if_expression_truth_at(expression.trim(), state, 0)
}

fn c_if_expression_truth_at(
    expression: &str,
    state: &CMacroState,
    depth: usize,
) -> CPreprocessorTruth {
    const EXPRESSION_DEPTH_CAP: usize = 16;
    if depth > EXPRESSION_DEPTH_CAP {
        return CPreprocessorTruth::Unknown;
    }
    let mut expression = expression.trim();
    while expression.starts_with('(')
        && expression.ends_with(')')
        && c_outer_delimiters_wrap(expression, b'(', b')')
    {
        expression = expression[1..expression.len() - 1].trim();
    }
    if let Some((left, right)) = split_c_preprocessor_boolean(expression, "||") {
        return match (
            c_if_expression_truth_at(left, state, depth + 1),
            c_if_expression_truth_at(right, state, depth + 1),
        ) {
            (CPreprocessorTruth::True, _) | (_, CPreprocessorTruth::True) => {
                CPreprocessorTruth::True
            }
            (CPreprocessorTruth::False, CPreprocessorTruth::False) => CPreprocessorTruth::False,
            _ => CPreprocessorTruth::Unknown,
        };
    }
    if let Some((left, right)) = split_c_preprocessor_boolean(expression, "&&") {
        return match (
            c_if_expression_truth_at(left, state, depth + 1),
            c_if_expression_truth_at(right, state, depth + 1),
        ) {
            (CPreprocessorTruth::False, _) | (_, CPreprocessorTruth::False) => {
                CPreprocessorTruth::False
            }
            (CPreprocessorTruth::True, CPreprocessorTruth::True) => CPreprocessorTruth::True,
            _ => CPreprocessorTruth::Unknown,
        };
    }
    if expression == "0" {
        return CPreprocessorTruth::False;
    }
    if expression == "1" {
        return CPreprocessorTruth::True;
    }
    if let Some(value) = parse_c_preprocessor_integer(expression) {
        return if value == 0 {
            CPreprocessorTruth::False
        } else {
            CPreprocessorTruth::True
        };
    }
    if let Some(rest) = expression.strip_prefix('!') {
        return match c_if_expression_truth_at(rest.trim(), state, depth + 1) {
            CPreprocessorTruth::True => CPreprocessorTruth::False,
            CPreprocessorTruth::False => CPreprocessorTruth::True,
            CPreprocessorTruth::Unknown => CPreprocessorTruth::Unknown,
        };
    }
    let defined_name = expression
        .strip_prefix("defined")
        .filter(|rest| {
            rest.starts_with(|character: char| character.is_ascii_whitespace())
                || rest.starts_with('(')
        })
        .map(str::trim)
        .map(|name| name.trim_start_matches('(').trim_end_matches(')').trim())
        .filter(|name| is_c_macro_identifier(name));
    if let Some(name) = defined_name {
        let truth = if state.macros.contains_key(name) {
            CPreprocessorTruth::True
        } else if state.undefined.contains(name) {
            CPreprocessorTruth::False
        } else {
            CPreprocessorTruth::Unknown
        };
        return truth;
    }
    if is_c_macro_identifier(expression) {
        if let Some(definition) = state.macros.get(expression) {
            if definition.replacement.trim().is_empty() {
                return CPreprocessorTruth::True;
            }
            return c_if_expression_truth_at(&definition.replacement, state, depth + 1);
        }
    }
    CPreprocessorTruth::Unknown
}

fn parse_c_preprocessor_integer(expression: &str) -> Option<u128> {
    let expression = expression
        .strip_prefix('+')
        .or_else(|| expression.strip_prefix('-'))
        .unwrap_or(expression);
    let literal = expression.trim_end_matches(['u', 'U', 'l', 'L']);
    if literal.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(hexadecimal) = literal
        .strip_prefix("0x")
        .or_else(|| literal.strip_prefix("0X"))
    {
        (hexadecimal, 16)
    } else if let Some(binary) = literal
        .strip_prefix("0b")
        .or_else(|| literal.strip_prefix("0B"))
    {
        (binary, 2)
    } else if literal.len() > 1 && literal.starts_with('0') {
        (&literal[1..], 8)
    } else {
        (literal, 10)
    };
    (!digits.is_empty())
        .then(|| u128::from_str_radix(digits, radix).ok())
        .flatten()
}

fn split_c_preprocessor_boolean<'a>(
    expression: &'a str,
    operator: &str,
) -> Option<(&'a str, &'a str)> {
    let bytes = expression.as_bytes();
    let operator = operator.as_bytes();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index + operator.len() <= bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ if depth == 0 && &bytes[index..index + operator.len()] == operator => {
                return Some((&expression[..index], &expression[index + operator.len()..]));
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn strip_c_preprocessor_comments(expression: &str) -> String {
    let bytes = expression.as_bytes();
    let mut output = String::with_capacity(expression.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            break;
        }
        if bytes[index..].starts_with(b"/*") {
            let Some(end) = bytes[index + 2..]
                .windows(2)
                .position(|window| window == b"*/")
            else {
                break;
            };
            output.push(' ');
            index += end + 4;
            continue;
        }
        output.push(bytes[index] as char);
        index += 1;
    }
    output
}

fn apply_c_compiler_macro_action(state: &mut CMacroState, action: &CCompilerMacroAction) {
    let event = match action {
        CCompilerMacroAction::Define {
            name,
            parameters,
            replacement,
        } => CPreprocessorEventFact {
            kind: if parameters.is_some() {
                CPreprocessorEventKind::DefineFunction
            } else {
                CPreprocessorEventKind::DefineObject
            },
            name: name.clone(),
            parameters: parameters.clone().unwrap_or_default(),
            replacement: replacement.clone(),
            variadic: false,
            uses_stringification: false,
            uses_token_pasting: false,
            guard_path: Vec::new(),
            line: 0,
            site_start_byte: 0,
            site_end_byte: 0,
        },
        CCompilerMacroAction::Undef { name } => CPreprocessorEventFact {
            kind: CPreprocessorEventKind::Undef,
            name: name.clone(),
            parameters: Vec::new(),
            replacement: String::new(),
            variadic: false,
            uses_stringification: false,
            uses_token_pasting: false,
            guard_path: Vec::new(),
            line: 0,
            site_start_byte: 0,
            site_end_byte: 0,
        },
    };
    apply_c_preprocessor_event(state, &event);
}

fn c_compiler_macro_action_weight(action: &CCompilerMacroAction) -> usize {
    match action {
        CCompilerMacroAction::Define {
            name,
            parameters,
            replacement,
        } => name.len().saturating_add(replacement.len()).saturating_add(
            parameters
                .iter()
                .flatten()
                .map(String::len)
                .fold(0usize, usize::saturating_add),
        ),
        CCompilerMacroAction::Undef { name } => name.len(),
    }
}

fn apply_c_preprocessor_event(state: &mut CMacroState, event: &CPreprocessorEventFact) {
    match event.kind {
        CPreprocessorEventKind::Undef => {
            if let Some(previous) = state.macros.remove(&event.name) {
                toggle_c_macro_fingerprint(&mut state.fingerprint, c_macro_event_digest(&previous));
            }
            if state.undefined.insert(event.name.clone()) {
                toggle_c_macro_fingerprint(
                    &mut state.fingerprint,
                    c_macro_undefined_digest(&event.name),
                );
            }
        }
        CPreprocessorEventKind::DefineObject | CPreprocessorEventKind::DefineFunction => {
            if state.undefined.remove(&event.name) {
                toggle_c_macro_fingerprint(
                    &mut state.fingerprint,
                    c_macro_undefined_digest(&event.name),
                );
            }
            if let Some(previous) = state.macros.insert(event.name.clone(), event.clone()) {
                toggle_c_macro_fingerprint(&mut state.fingerprint, c_macro_event_digest(&previous));
            }
            toggle_c_macro_fingerprint(&mut state.fingerprint, c_macro_event_digest(event));
        }
    }
}

fn dedup_c_macro_states(states: &mut Vec<CMacroState>) {
    let mut seen = HashSet::new();
    states.retain(|state| seen.insert(state.fingerprint));
}

fn toggle_c_macro_fingerprint(fingerprint: &mut [u8; 32], digest: [u8; 32]) {
    for (byte, digest_byte) in fingerprint.iter_mut().zip(digest) {
        *byte ^= digest_byte;
    }
}

fn c_macro_event_digest(event: &CPreprocessorEventFact) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"macro\0");
    hasher.update(event.name.as_bytes());
    hasher.update(&[match event.kind {
        CPreprocessorEventKind::DefineObject => 0,
        CPreprocessorEventKind::DefineFunction => 1,
        CPreprocessorEventKind::Undef => 2,
    }]);
    for parameter in &event.parameters {
        hasher.update(&(parameter.len() as u64).to_le_bytes());
        hasher.update(parameter.as_bytes());
    }
    hasher.update(&(event.replacement.len() as u64).to_le_bytes());
    hasher.update(event.replacement.as_bytes());
    hasher.update(&[
        u8::from(event.variadic),
        u8::from(event.uses_stringification),
        u8::from(event.uses_token_pasting),
    ]);
    *hasher.finalize().as_bytes()
}

fn c_macro_undefined_digest(name: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"undefined\0");
    hasher.update(name.as_bytes());
    *hasher.finalize().as_bytes()
}

fn c_macro_branch_digest(branch_key: &str, selected: usize) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"branch\0");
    hasher.update(branch_key.as_bytes());
    hasher.update(&selected.to_le_bytes());
    *hasher.finalize().as_bytes()
}

fn is_c_macro_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn expand_c_macro_use(
    use_fact: &crate::model::CMacroInitializerFact,
    state: &CMacroState,
) -> Option<String> {
    let mut active = HashSet::new();
    let mut work = 0usize;
    expand_c_macro_text(&use_fact.expression, state, 0, &mut active, &mut work)
}

fn expand_c_macro_text(
    text: &str,
    state: &CMacroState,
    depth: usize,
    active: &mut HashSet<String>,
    work: &mut usize,
) -> Option<String> {
    if depth > C_MACRO_EXPANSION_DEPTH_CAP || text.len() > C_MACRO_OUTPUT_BYTES_CAP {
        return None;
    }
    let bytes = text.as_bytes();
    let mut output = String::new();
    let mut index = 0usize;
    while index < bytes.len() {
        *work += 1;
        if *work > C_MACRO_TOKEN_WORK_CAP || output.len() > C_MACRO_OUTPUT_BYTES_CAP {
            return None;
        }
        if bytes[index] == b'_' || bytes[index].is_ascii_alphabetic() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
            {
                index += 1;
            }
            let name = &text[start..index];
            let Some(definition) = state.macros.get(name) else {
                output.push_str(name);
                continue;
            };
            if definition.variadic
                || definition.uses_stringification
                || definition.uses_token_pasting
                || !active.insert(name.to_owned())
            {
                return None;
            }
            let replacement = match definition.kind {
                CPreprocessorEventKind::Undef => None,
                CPreprocessorEventKind::DefineObject => {
                    expand_c_macro_text(&definition.replacement, state, depth + 1, active, work)
                }
                CPreprocessorEventKind::DefineFunction => {
                    let mut open = index;
                    while open < bytes.len() && bytes[open].is_ascii_whitespace() {
                        open += 1;
                    }
                    if bytes.get(open) != Some(&b'(') {
                        active.remove(name);
                        output.push_str(name);
                        continue;
                    }
                    let (arguments, end) = split_c_macro_arguments(text, open)?;
                    if arguments.len() != definition.parameters.len()
                        || arguments.len() > C_MACRO_ARGUMENT_CAP
                    {
                        return None;
                    }
                    index = end;
                    let substituted = substitute_c_macro_parameters(
                        &definition.replacement,
                        &definition.parameters,
                        &arguments,
                    )?;
                    expand_c_macro_text(&substituted, state, depth + 1, active, work)
                }
            };
            active.remove(name);
            output.push_str(&replacement?);
            continue;
        }
        output.push(bytes[index] as char);
        index += 1;
    }
    (output.len() <= C_MACRO_OUTPUT_BYTES_CAP).then_some(output)
}

fn split_c_macro_arguments(text: &str, open: usize) -> Option<(Vec<String>, usize)> {
    let bytes = text.as_bytes();
    let mut arguments = Vec::new();
    let mut start = open + 1;
    let mut index = start;
    let mut depth = 1usize;
    let mut quote = None;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if matches!(byte, b'(' | b'{' | b'[') {
            depth += 1;
        } else if matches!(byte, b')' | b'}' | b']') {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                let argument = text[start..index].trim();
                if !argument.is_empty() || !arguments.is_empty() {
                    arguments.push(argument.to_owned());
                }
                return Some((arguments, index + 1));
            }
        } else if byte == b',' && depth == 1 {
            arguments.push(text[start..index].trim().to_owned());
            start = index + 1;
        }
        index += 1;
    }
    None
}

fn substitute_c_macro_parameters(
    replacement: &str,
    parameters: &[String],
    arguments: &[String],
) -> Option<String> {
    let replacements = parameters
        .iter()
        .zip(arguments)
        .map(|(parameter, argument)| (parameter.as_str(), argument.as_str()))
        .collect::<HashMap<_, _>>();
    let bytes = replacement.as_bytes();
    let mut output = String::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'_' || bytes[index].is_ascii_alphabetic() {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
            {
                index += 1;
            }
            let token = &replacement[start..index];
            if let Some(argument) = replacements.get(token) {
                output.push_str(argument);
            } else {
                output.push_str(token);
            }
        } else {
            output.push(bytes[index] as char);
            index += 1;
        }
        if output.len() > C_MACRO_OUTPUT_BYTES_CAP {
            return None;
        }
    }
    Some(output)
}

#[derive(Debug)]
struct CExpandedInitializer {
    field_name: Option<String>,
    field_index: Option<usize>,
    target_name: String,
}

fn parse_c_expanded_initializers(
    expanded: &str,
    base_field_name: Option<&str>,
    base_field_index: Option<usize>,
) -> Vec<CExpandedInitializer> {
    parse_c_expanded_initializers_at(expanded, base_field_name, base_field_index, 0)
}

fn parse_c_expanded_initializers_at(
    expanded: &str,
    base_field_name: Option<&str>,
    base_field_index: Option<usize>,
    depth: usize,
) -> Vec<CExpandedInitializer> {
    const NESTED_INITIALIZER_DEPTH_CAP: usize = 8;
    if depth > NESTED_INITIALIZER_DEPTH_CAP {
        return Vec::new();
    }
    let mut text = expanded.trim();
    while text.starts_with('{') && text.ends_with('}') && c_outer_delimiters_wrap(text, b'{', b'}')
    {
        text = text[1..text.len() - 1].trim();
    }
    let entries = split_c_top_level(text, b',');
    let mut output = Vec::new();
    for (offset, entry) in entries.into_iter().enumerate() {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (field_name, value) = if let Some(rest) = entry.strip_prefix('.') {
            let Some((field, value)) = rest.split_once('=') else {
                continue;
            };
            let field = field.trim();
            if !is_c_macro_identifier(field) {
                continue;
            }
            (Some(field.to_owned()), value.trim())
        } else {
            (base_field_name.map(str::to_owned), entry)
        };
        if value.starts_with('{')
            && value.ends_with('}')
            && c_outer_delimiters_wrap(value, b'{', b'}')
        {
            output.extend(parse_c_expanded_initializers_at(
                value,
                field_name.as_deref(),
                base_field_index,
                depth + 1,
            ));
            continue;
        }
        let Some(target_name) = c_expanded_callable_name(value) else {
            continue;
        };
        output.push(CExpandedInitializer {
            field_name,
            field_index: base_field_name
                .is_none()
                .then(|| base_field_index.unwrap_or(0).saturating_add(offset)),
            target_name,
        });
    }
    output
}

fn c_outer_delimiters_wrap(text: &str, open: u8, close: u8) -> bool {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == open {
            depth += 1;
        } else if byte == close {
            let Some(next) = depth.checked_sub(1) else {
                return false;
            };
            depth = next;
            if depth == 0 && index + 1 != bytes.len() {
                return false;
            }
        }
    }
    depth == 0
}

fn split_c_top_level(text: &str, delimiter: u8) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut output = Vec::new();
    let mut start = 0usize;
    let mut depths = [0usize; 3];
    let mut quote = None;
    let mut escaped = false;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if let Some(end) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == end {
                quote = None;
            }
            continue;
        }
        if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
            continue;
        }
        match byte {
            b'(' => depths[0] += 1,
            b')' => depths[0] = depths[0].saturating_sub(1),
            b'{' => depths[1] += 1,
            b'}' => depths[1] = depths[1].saturating_sub(1),
            b'[' => depths[2] += 1,
            b']' => depths[2] = depths[2].saturating_sub(1),
            _ if byte == delimiter && depths == [0, 0, 0] => {
                output.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    output.push(&text[start..]);
    output
}

fn c_expanded_callable_name(value: &str) -> Option<String> {
    let mut value = value.trim();
    loop {
        if value.starts_with('(')
            && value.ends_with(')')
            && c_outer_delimiters_wrap(value, b'(', b')')
        {
            value = value[1..value.len() - 1].trim();
        } else {
            break;
        }
    }
    if let Some(addressed) = value.strip_prefix('&') {
        value = addressed.trim();
        while value.starts_with('(')
            && value.ends_with(')')
            && c_outer_delimiters_wrap(value, b'(', b')')
        {
            value = value[1..value.len() - 1].trim();
        }
    }
    is_c_macro_identifier(value).then(|| value.to_owned())
}

fn normalize_c_type_name(type_name: &str) -> String {
    type_name
        .split_whitespace()
        .filter(|part| !matches!(*part, "const" | "volatile" | "struct" | "class"))
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|character| character == '*' || character == '&' || character == ' ')
        .to_owned()
}

fn resolve_c_include(
    source_file: &str,
    include: &crate::model::CIncludeFact,
    all_paths: &HashSet<String>,
) -> Option<String> {
    use crate::model::CIncludeResolution;

    let path = include.path.replace('\\', "/");
    match include.resolution {
        CIncludeResolution::Rejected => return None,
        CIncludeResolution::Resolved => return all_paths.contains(&path).then_some(path),
        CIncludeResolution::Unmanaged => {}
    }

    if !include.angled {
        let parent = source_file
            .rsplit_once('/')
            .map_or("", |(parent, _)| parent);
        let joined = if parent.is_empty() {
            path.clone()
        } else {
            format!("{parent}/{path}")
        };
        if let Some(normalized) = normalize_project_relative_path(&joined) {
            if all_paths.contains(&normalized) {
                return Some(normalized);
            }
        }
    }
    let suffix = format!("/{path}");
    let mut suffix_matches = all_paths
        .iter()
        .filter(|candidate| candidate.as_str() == path || candidate.ends_with(&suffix))
        .cloned()
        .collect::<Vec<_>>();
    suffix_matches.sort();
    suffix_matches.dedup();
    (suffix_matches.len() == 1).then(|| suffix_matches.remove(0))
}

fn normalize_project_relative_path(path: &str) -> Option<String> {
    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop()?;
            }
            value => components.push(value),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn c_visible_files(
    source_file: &str,
    include_edges: &HashMap<String, Vec<String>>,
    depth_cap: usize,
    work_cap: usize,
) -> Vec<String> {
    let mut visible = HashSet::new();
    let mut queue = VecDeque::from([(source_file.to_owned(), 0usize)]);
    let mut work = 0usize;
    while let Some((file, depth)) = queue.pop_front() {
        if work >= work_cap || depth > depth_cap || !visible.insert(file.clone()) {
            continue;
        }
        work += 1;
        if let Some(includes) = include_edges.get(&file) {
            for include in includes {
                queue.push_back((include.clone(), depth + 1));
            }
        }
    }
    let mut visible = visible.into_iter().collect::<Vec<_>>();
    visible.sort();
    visible
}

fn normalized_module_key(path: &str) -> String {
    const EXTENSIONS: &[&str] = &[
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "vue", "svelte", "astro", "ets",
    ];
    for extension in EXTENSIONS {
        if let Some(stem) = path.strip_suffix(&format!(".{extension}")) {
            return stem.to_owned();
        }
    }
    path.to_owned()
}

#[allow(clippy::too_many_arguments)]
fn resolve_exported_literal(
    file: &str,
    export_name: &str,
    member_path: &str,
    literals: &HashMap<(String, String, String), HashSet<String>>,
    named_exports: &HashMap<(String, String), Vec<(String, String)>>,
    star_exports: &HashMap<String, Vec<String>>,
    visited: &mut HashSet<(String, String, String)>,
    depth: usize,
) -> Option<String> {
    const MAX_EXPORT_DEPTH: usize = 16;
    if depth >= MAX_EXPORT_DEPTH {
        return None;
    }
    let file = normalized_module_key(file);
    let visit = (file.clone(), export_name.to_owned(), member_path.to_owned());
    if !visited.insert(visit.clone()) {
        return None;
    }

    let literal_key = (file.clone(), export_name.to_owned(), member_path.to_owned());
    let mut candidates = literals.get(&literal_key).cloned().unwrap_or_default();
    if candidates.is_empty() {
        if let Some(targets) = named_exports.get(&(file.clone(), export_name.to_owned())) {
            for (target_file, target_name) in targets {
                if let Some(channel) = resolve_exported_literal(
                    target_file,
                    target_name,
                    member_path,
                    literals,
                    named_exports,
                    star_exports,
                    visited,
                    depth + 1,
                ) {
                    candidates.insert(channel);
                }
            }
        } else if let Some(targets) = star_exports.get(&file) {
            for target_file in targets {
                if let Some(channel) = resolve_exported_literal(
                    target_file,
                    export_name,
                    member_path,
                    literals,
                    named_exports,
                    star_exports,
                    visited,
                    depth + 1,
                ) {
                    candidates.insert(channel);
                }
            }
        }
    }
    visited.remove(&visit);
    (candidates.len() == 1)
        .then(|| candidates.into_iter().next())
        .flatten()
}

fn file_size(path: &Path) -> Result<u64> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error).with_context(|| format!("read metadata for {}", path.display())),
    }
}

fn parse_language(value: &str) -> Language {
    match value {
        "tsx" => Language::Tsx,
        "javascript" => Language::JavaScript,
        "jsx" => Language::Jsx,
        "vue" => Language::Vue,
        "svelte" => Language::Svelte,
        "astro" => Language::Astro,
        "arkts" => Language::ArkTs,
        "python" => Language::Python,
        "rust" => Language::Rust,
        "go" => Language::Go,
        "java" => Language::Java,
        "csharp" => Language::CSharp,
        "c" => Language::C,
        "cpp" => Language::Cpp,
        "dart" => Language::Dart,
        "ruby" => Language::Ruby,
        "php" => Language::Php,
        "swift" => Language::Swift,
        "lua" => Language::Lua,
        "kotlin" => Language::Kotlin,
        "scala" => Language::Scala,
        "r" => Language::R,
        _ => Language::TypeScript,
    }
}

fn compatible_web_language(language: &str) -> Option<&'static str> {
    match language {
        "arkts" => Some("typescript"),
        "astro" => Some("typescript"),
        "typescript" => Some("arkts"),
        _ => None,
    }
}

fn parse_symbol_kind(value: &str) -> SymbolKind {
    match value {
        "file" => SymbolKind::File,
        "class" => SymbolKind::Class,
        "interface" => SymbolKind::Interface,
        "struct" => SymbolKind::Struct,
        "trait" => SymbolKind::Trait,
        "enum" => SymbolKind::Enum,
        "type" => SymbolKind::Type,
        "method" => SymbolKind::Method,
        "variable" => SymbolKind::Variable,
        "route" => SymbolKind::Route,
        "component" => SymbolKind::Component,
        _ => SymbolKind::Function,
    }
}

fn parse_relationship_kind(value: &str) -> RelationshipKind {
    match value {
        "calls" => RelationshipKind::Calls,
        "references" => RelationshipKind::References,
        "imports" => RelationshipKind::Imports,
        "extends" => RelationshipKind::Extends,
        "implements" => RelationshipKind::Implements,
        _ => RelationshipKind::Contains,
    }
}

fn module_hint_matches(hint: &str, candidate: &str) -> bool {
    let raw_hint = hint.trim_matches(['\'', '"']).replace('\\', "/");
    if raw_hint.starts_with("./") || raw_hint.starts_with("../") {
        // Valid project-relative imports are canonicalized before persistence.
        // A remaining relative hint failed project resolution and must not be
        // suffix-matched to an unrelated in-project file.
        return false;
    }
    let normalize = |value: &str| {
        value
            .trim_matches(['\'', '"'])
            .trim_start_matches("./")
            .replace('\\', "/")
    };
    let hint = normalize(hint);
    let candidate = normalize(candidate);
    let candidate_without_extension = candidate
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&candidate);
    candidate == hint
        || candidate_without_extension == hint
        || candidate_without_extension.starts_with(&format!("{hint}/"))
        || candidate_without_extension.ends_with(&format!("/{hint}"))
        || candidate_without_extension.ends_with(&format!("/{hint}/index"))
}

#[allow(clippy::too_many_arguments)]
fn resolve_fastapi_router_ref(
    reference: &FastApiRouterRef,
    current_file: &str,
    paths: &[String],
    declarations: &HashMap<(String, String), (String, bool)>,
    aliases: &HashMap<(String, String), FastApiRouterRef>,
    factories: &HashMap<(String, String), FastApiRouterRef>,
    depth: usize,
    seen: &mut HashSet<(String, String)>,
) -> Option<(String, String)> {
    if depth > 16 {
        return None;
    }
    let target_file = match reference.target_file_hint.as_deref() {
        None => current_file.to_owned(),
        Some(hint) => {
            if paths.iter().any(|path| path == hint) {
                hint.to_owned()
            } else {
                let matches = paths
                    .iter()
                    .filter(|path| python_module_hint_matches(hint, path))
                    .cloned()
                    .collect::<HashSet<_>>();
                let mut matches = matches.into_iter().collect::<Vec<_>>();
                matches.sort();
                let normalized_hint = hint.replace(['.', '\\'], "/");
                let initializer = matches.iter().find(|path| {
                    let normalized = path.replace('\\', "/");
                    normalized.ends_with(&format!(
                        "{}/__init__.py",
                        normalized_hint.trim_matches('/')
                    ))
                });
                if let Some(initializer) = initializer {
                    initializer.clone()
                } else {
                    let [path] = matches.as_slice() else {
                        return None;
                    };
                    path.clone()
                }
            }
        }
    };
    let key = (target_file, reference.name.clone());
    if !reference.factory {
        if declarations.contains_key(&key) {
            return Some(key);
        }
        if !seen.insert(key.clone()) {
            return None;
        }
        let aliased = aliases.get(&key)?;
        let resolved = resolve_fastapi_router_ref(
            aliased,
            &key.0,
            paths,
            declarations,
            aliases,
            factories,
            depth + 1,
            seen,
        );
        seen.remove(&key);
        return resolved;
    }
    if !seen.insert(key.clone()) {
        return None;
    }
    let returned = factories.get(&key)?;
    let resolved = resolve_fastapi_router_ref(
        returned,
        &key.0,
        paths,
        declarations,
        aliases,
        factories,
        depth + 1,
        seen,
    );
    seen.remove(&key);
    resolved
}

#[allow(clippy::too_many_arguments)]
fn resolve_fastapi_callable_ref(
    reference: &FastApiRouterRef,
    current_file: &str,
    paths: &[String],
    symbols: &HashMap<(String, String), Vec<(String, String)>>,
    aliases: &HashMap<(String, String), FastApiRouterRef>,
    factories: &HashMap<(String, String), FastApiRouterRef>,
    depth: usize,
    seen: &mut HashSet<(String, String, bool)>,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let target_file = match reference.target_file_hint.as_deref() {
        None => current_file.to_owned(),
        Some(hint) => {
            if paths.iter().any(|path| path == hint) {
                hint.to_owned()
            } else {
                let matches = paths
                    .iter()
                    .filter(|path| python_module_hint_matches(hint, path))
                    .cloned()
                    .collect::<HashSet<_>>();
                let mut matches = matches.into_iter().collect::<Vec<_>>();
                matches.sort();
                let normalized_hint = hint.replace(['.', '\\'], "/");
                if let Some(initializer) = matches.iter().find(|path| {
                    path.replace('\\', "/").ends_with(&format!(
                        "{}/__init__.py",
                        normalized_hint.trim_matches('/')
                    ))
                }) {
                    initializer.clone()
                } else {
                    let [path] = matches.as_slice() else {
                        return None;
                    };
                    path.clone()
                }
            }
        }
    };
    let type_alias_name = reference.name.strip_prefix("@dependency-type:");
    let key = (
        target_file,
        type_alias_name.unwrap_or(&reference.name).to_owned(),
    );
    let seen_key = (key.0.clone(), key.1.clone(), reference.factory);
    if !seen.insert(seen_key.clone()) {
        return None;
    }
    let resolved = if type_alias_name.is_some() {
        aliases.get(&key).and_then(|alias| {
            resolve_fastapi_callable_ref(
                alias,
                &key.0,
                paths,
                symbols,
                aliases,
                factories,
                depth + 1,
                seen,
            )
        })
    } else if reference.factory {
        if let Some(returned) = factories.get(&key) {
            resolve_fastapi_callable_ref(
                returned,
                &key.0,
                paths,
                symbols,
                aliases,
                factories,
                depth + 1,
                seen,
            )
        } else if let Some(alias) = aliases.get(&key) {
            let mut target = alias.clone();
            target.factory = true;
            resolve_fastapi_callable_ref(
                &target,
                &key.0,
                paths,
                symbols,
                aliases,
                factories,
                depth + 1,
                seen,
            )
        } else {
            None
        }
    } else if let Some(candidates) = symbols.get(&key) {
        let unique = candidates
            .iter()
            .map(|(_, public_id)| public_id)
            .collect::<HashSet<_>>();
        let unique = unique.into_iter().collect::<Vec<_>>();
        let [public_id] = unique.as_slice() else {
            seen.remove(&seen_key);
            return None;
        };
        Some((*public_id).clone())
    } else if let Some(alias) = aliases.get(&key) {
        resolve_fastapi_callable_ref(
            alias,
            &key.0,
            paths,
            symbols,
            aliases,
            factories,
            depth + 1,
            seen,
        )
    } else {
        None
    };
    seen.remove(&seen_key);
    resolved
}

fn python_module_hint_matches(hint: &str, candidate: &str) -> bool {
    if hint.starts_with('.') || hint.starts_with("./") || hint.starts_with("../") {
        return false;
    }
    let hint = hint
        .trim_matches(['\'', '"'])
        .replace(['.', '\\'], "/")
        .trim_matches('/')
        .to_owned();
    let candidate = candidate.replace('\\', "/");
    let candidate = candidate
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(&candidate);
    candidate == hint || candidate.ends_with(&format!("/{hint}"))
}

fn join_route_path(prefix: &str, path: &str) -> String {
    format!("{prefix}{path}")
}

fn harmony_project_root(path: &str) -> Option<String> {
    let normalized = path.replace('\\', "/");
    let components = normalized.split('/').collect::<Vec<_>>();
    let marker = components
        .iter()
        .position(|component| matches!(*component, "entry" | "feature" | "features"))?;
    (marker > 0).then(|| components[..marker].join("/"))
}

fn path_is_within(path: &str, root: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized == root || normalized.starts_with(&format!("{root}/"))
}

fn arkui_route_parts(path: &str) -> Option<(String, String)> {
    let normalized = path.replace('\\', "/");
    let components = normalized.split('/').collect::<Vec<_>>();
    let markers = components
        .windows(3)
        .enumerate()
        .filter_map(|(index, window)| (window == ["src", "main", "ets"]).then_some(index))
        .collect::<Vec<_>>();
    let marker_start = *markers.first()?;
    if markers.len() != 1 {
        return None;
    }
    let module = components[..marker_start].join("/");
    let route_with_extension = components[marker_start + 3..].join("/");
    let route = route_with_extension
        .strip_suffix(".ets")
        .unwrap_or(&route_with_extension)
        .to_owned();
    (!route.is_empty()).then_some((module, route))
}

fn arkui_route_candidate_matches(
    caller: &str,
    candidate: &str,
    module_roots: &HashSet<String>,
) -> bool {
    let Some((candidate_module, _)) = arkui_route_parts(candidate) else {
        return false;
    };
    let normalized_caller = caller.replace('\\', "/");
    let caller_module = module_roots
        .iter()
        .filter(|module| {
            module.is_empty()
                || normalized_caller == **module
                || normalized_caller
                    .strip_prefix(module.as_str())
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
        .max_by_key(|module| module.len());
    caller_module.is_some_and(|module| module == &candidate_module)
}

fn resolve_receiver_nominal(
    file_id: i64,
    receiver_type: &str,
    target_file_hint: &str,
    local_nominal_types: &NominalTypeMap,
    imported_nominal_types: &NominalTypeMap,
    nominal_ids_by_identity: &HashMap<(String, String), Vec<String>>,
) -> ReceiverNominalResolution {
    let key = (file_id, receiver_type.to_owned());
    let local = local_nominal_types.get(&key).cloned().unwrap_or_default();
    let candidates = if local.is_empty() {
        imported_nominal_types
            .get(&key)
            .cloned()
            .unwrap_or_default()
    } else {
        local
    };
    let had_scoped_candidates = !candidates.is_empty();
    let mut public_ids = candidates
        .iter()
        .filter(|(_, _, path)| {
            target_file_hint.is_empty() || module_hint_matches(target_file_hint, path)
        })
        .filter_map(|(qualified_name, _, path)| {
            nominal_ids_by_identity.get(&(qualified_name.clone(), path.clone()))
        })
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    if public_ids.is_empty() && !had_scoped_candidates {
        let qualified_suffix = format!(".{receiver_type}");
        for ((qualified_name, path), ids) in nominal_ids_by_identity {
            if (qualified_name == receiver_type || qualified_name.ends_with(&qualified_suffix))
                && (target_file_hint.is_empty() || module_hint_matches(target_file_hint, path))
            {
                public_ids.extend(ids.iter().cloned());
            }
        }
    }
    if public_ids.is_empty() && had_scoped_candidates {
        return ReceiverNominalResolution::Ambiguous;
    }
    public_ids.sort();
    public_ids.dedup();
    match public_ids.len() {
        0 => ReceiverNominalResolution::NoMatch,
        1 => ReceiverNominalResolution::Unique(public_ids.pop().expect("one nominal receiver")),
        _ => ReceiverNominalResolution::Ambiguous,
    }
}

fn resolve_inherited_member(
    receiver_id: &str,
    method_name: &str,
    inherited_bases: &HashMap<String, Vec<String>>,
    inherited_methods: &HashMap<(String, String), Vec<(String, String)>>,
) -> InheritedMemberResolution {
    const INHERITANCE_DEPTH_CAP: usize = 8;
    const INHERITANCE_NODE_CAP: usize = 64;
    let mut frontier = vec![receiver_id.to_owned()];
    let mut visited = frontier.iter().cloned().collect::<HashSet<_>>();
    for _ in 0..INHERITANCE_DEPTH_CAP {
        let raw_parents = frontier
            .iter()
            .filter_map(|child| inherited_bases.get(child))
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        if raw_parents.is_empty() {
            return InheritedMemberResolution::NoMatch;
        }
        let mut parents = raw_parents
            .into_iter()
            .filter(|parent| !visited.contains(parent))
            .collect::<Vec<_>>();
        parents.sort();
        parents.dedup();
        if parents.is_empty() {
            return InheritedMemberResolution::Ambiguous;
        }
        if visited.len().saturating_add(parents.len()) > INHERITANCE_NODE_CAP {
            return InheritedMemberResolution::Ambiguous;
        }
        visited.extend(parents.iter().cloned());

        let mut methods = parents
            .iter()
            .filter_map(|parent| inherited_methods.get(&(parent.clone(), method_name.to_owned())))
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        methods.sort();
        methods.dedup();
        match methods.len() {
            0 => frontier = parents,
            1 => {
                let (public_id, qualified_name) = methods.pop().expect("one inherited method");
                return InheritedMemberResolution::Unique(public_id, qualified_name);
            }
            _ => return InheritedMemberResolution::Ambiguous,
        }
    }
    if frontier
        .iter()
        .any(|child| inherited_bases.contains_key(child))
    {
        InheritedMemberResolution::Ambiguous
    } else {
        InheritedMemberResolution::NoMatch
    }
}

fn identifier_segments(value: &str) -> Vec<String> {
    let mut segments = Vec::new();
    for token in value.split(|character: char| !character.is_alphanumeric()) {
        if token.is_empty() {
            continue;
        }
        let normalized = token.to_lowercase();
        if !segments.contains(&normalized) {
            segments.push(normalized);
        }

        let characters: Vec<char> = token.chars().collect();
        let mut start = 0;
        for index in 1..characters.len() {
            let previous = characters[index - 1];
            let current = characters[index];
            let next = characters.get(index + 1).copied();
            let boundary = (previous.is_lowercase() && current.is_uppercase())
                || (previous.is_alphabetic() && current.is_numeric())
                || (previous.is_numeric() && current.is_alphabetic())
                || (previous.is_uppercase()
                    && current.is_uppercase()
                    && next.is_some_and(char::is_lowercase));
            if boundary {
                let segment = characters[start..index]
                    .iter()
                    .collect::<String>()
                    .to_lowercase();
                if !segments.contains(&segment) {
                    segments.push(segment);
                }
                start = index;
            }
        }
        let segment = characters[start..]
            .iter()
            .collect::<String>()
            .to_lowercase();
        if !segments.contains(&segment) {
            segments.push(segment);
        }
    }
    segments
}

fn search_terms(query: &str) -> Vec<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "after", "an", "and", "are", "before", "does", "for", "from", "how", "in", "is", "of",
        "on", "or", "the", "to", "what", "where", "why", "with", "work", "works",
    ];
    let all_terms = identifier_segments(query);
    let meaningful = all_terms
        .iter()
        .filter(|term| !STOP_WORDS.contains(&term.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if meaningful.is_empty() {
        all_terms
    } else {
        meaningful
    }
}

fn call_result_resolution_enabled(dependent_calls: usize) -> bool {
    dependent_calls <= CALL_RESULT_DEPENDENT_CAP
}

fn ensure_query_cardinality<T>(items: &[T], label: &str) -> Result<()> {
    anyhow::ensure!(
        items.len() <= ResourceBudget::MAX_IMPACT_NODES,
        "{label} exceeded the {}-item work limit",
        ResourceBudget::MAX_IMPACT_NODES
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_publishers_allocate_distinct_serialized_epochs() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("graph.db");
        let left_store = Store::open(&database).unwrap();
        let right_store = Store::open(&database).unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let mut epochs = std::thread::scope(|scope| {
            let left_barrier = Arc::clone(&barrier);
            let left = scope.spawn(move || {
                let mut store = left_store;
                let facts = crate::parser::parse_file("left.ts", "function left() {}\n").unwrap();
                left_barrier.wait();
                store.publish([Ok(facts)], &[], &[]).unwrap().0
            });
            let right_barrier = Arc::clone(&barrier);
            let right = scope.spawn(move || {
                let mut store = right_store;
                let facts = crate::parser::parse_file("right.ts", "function right() {}\n").unwrap();
                right_barrier.wait();
                store.publish([Ok(facts)], &[], &[]).unwrap().0
            });
            vec![left.join().unwrap(), right.join().unwrap()]
        });
        epochs.sort_unstable();

        assert_eq!(epochs, [1, 2]);
        let store = Store::open(&database).unwrap();
        assert_eq!(store.epoch().unwrap(), 2);
        assert_eq!(store.indexed_files().unwrap().len(), 2);
    }

    #[test]
    fn c_macro_expansion_substitutes_nested_designated_initializers() {
        let header = crate::parser::parse_file(
            "defs.h",
            "#define PASS(x) x\n#define SLOT(x) .slot_run = PASS(x)\n",
        )
        .unwrap();
        let source = crate::parser::parse_file(
            "main.c",
            "typedef struct SlotOps { int (*slot_run)(int); } SlotOps;\n\
             static int slot_target(int v) { return v; }\n\
             static SlotOps table = { SLOT(slot_target) };\n",
        )
        .unwrap();
        let mut state = CMacroState::default();
        for event in header.c_function_pointers.preprocessor_events {
            apply_c_preprocessor_event(&mut state, &event);
        }
        let use_fact = source
            .c_function_pointers
            .macro_initializers
            .first()
            .unwrap();
        let expanded = expand_c_macro_use(use_fact, &state).unwrap();
        assert_eq!(expanded.trim(), ".slot_run = slot_target");
        let parsed = parse_c_expanded_initializers(
            &expanded,
            use_fact.field_name.as_deref(),
            use_fact.field_index,
        );
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].field_name.as_deref(), Some("slot_run"));
        assert_eq!(parsed[0].target_name, "slot_target");
    }

    #[test]
    fn c_macro_replay_handles_large_unconditional_environments_linearly() {
        let facts = CFunctionPointerFacts {
            preprocessor_events: (0..10_000)
                .map(|index| CPreprocessorEventFact {
                    kind: CPreprocessorEventKind::DefineObject,
                    name: format!("MACRO_{index}"),
                    parameters: Vec::new(),
                    replacement: index.to_string(),
                    variadic: false,
                    uses_stringification: false,
                    uses_token_pasting: false,
                    guard_path: Vec::new(),
                    line: index + 1,
                    site_start_byte: index,
                    site_end_byte: index + 1,
                })
                .collect(),
            ..CFunctionPointerFacts::default()
        };
        let files = HashMap::from([("main.c".to_owned(), facts)]);
        let all_paths = HashSet::from(["main.c".to_owned()]);
        let resolver = CMacroResolver::new(&files, &all_paths);
        let states = resolver.states_at("main.c", usize::MAX);

        assert_eq!(states.len(), 1);
        assert_eq!(states[0].macros.len(), 10_000);
        assert_eq!(
            states[0].macros["MACRO_9999"].replacement,
            "9999".to_owned()
        );
    }

    #[test]
    fn astro_language_and_module_keys_round_trip_through_storage_helpers() {
        assert_eq!(parse_language("astro"), Language::Astro);
        assert_eq!(
            normalized_module_key("src/components/Hero.astro"),
            "src/components/Hero"
        );
        assert_eq!(compatible_web_language("astro"), Some("typescript"));
        assert!(!module_hint_matches("../../../Card.astro", "Card.astro"));
    }

    #[test]
    fn identifier_vocabulary_splits_common_naming_conventions() {
        assert_eq!(
            identifier_segments("HTTPAuthService login_user2"),
            [
                "httpauthservice",
                "http",
                "auth",
                "service",
                "login",
                "user2",
                "user",
                "2"
            ]
        );
    }

    #[test]
    fn natural_language_search_terms_drop_noise_and_keep_identifiers() {
        assert_eq!(
            search_terms("How does AuthService login_user work?"),
            ["authservice", "auth", "service", "login", "user"]
        );
    }

    #[test]
    fn call_result_resolution_cap_is_inclusive_and_fail_closed() {
        assert!(call_result_resolution_enabled(CALL_RESULT_DEPENDENT_CAP));
        assert!(!call_result_resolution_enabled(
            CALL_RESULT_DEPENDENT_CAP + 1
        ));
    }

    #[test]
    fn inherited_member_resolution_is_bounded_and_ambiguity_safe() {
        let methods = HashMap::from([(
            ("base".to_owned(), "run".to_owned()),
            vec![("method".to_owned(), "Base.run".to_owned())],
        )]);
        let unique = resolve_inherited_member(
            "child",
            "run",
            &HashMap::from([("child".to_owned(), vec!["base".to_owned()])]),
            &methods,
        );
        assert!(matches!(
            unique,
            InheritedMemberResolution::Unique(_, ref name) if name == "Base.run"
        ));
        assert!(matches!(
            resolve_receiver_nominal(
                1,
                "Child",
                "",
                &HashMap::new(),
                &HashMap::new(),
                &HashMap::from([
                    (
                        ("Child".to_owned(), "child.ts".to_owned()),
                        vec!["child".to_owned()]
                    ),
                    (
                        ("Child".to_owned(), "other.ts".to_owned()),
                        vec!["other".to_owned()]
                    ),
                ]),
            ),
            ReceiverNominalResolution::Ambiguous
        ));

        let mut deep_bases = HashMap::new();
        for depth in 0..=8 {
            deep_bases.insert(format!("depth{depth}"), vec![format!("depth{}", depth + 1)]);
        }
        assert!(matches!(
            resolve_inherited_member("depth0", "run", &deep_bases, &HashMap::new(),),
            InheritedMemberResolution::Ambiguous
        ));

        let wide_parents = (0..64).map(|index| format!("base{index}")).collect();
        assert!(matches!(
            resolve_inherited_member(
                "wide",
                "run",
                &HashMap::from([("wide".to_owned(), wide_parents)]),
                &HashMap::new(),
            ),
            InheritedMemberResolution::Ambiguous
        ));

        assert!(matches!(
            resolve_inherited_member(
                "cycle-a",
                "run",
                &HashMap::from([
                    ("cycle-a".to_owned(), vec!["cycle-b".to_owned()]),
                    ("cycle-b".to_owned(), vec!["cycle-a".to_owned()]),
                ]),
                &HashMap::new(),
            ),
            InheritedMemberResolution::Ambiguous
        ));
    }

    #[test]
    fn additive_migration_upgrades_legacy_reference_table() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("legacy.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE unresolved_references (
                    id INTEGER PRIMARY KEY,
                    file_id INTEGER NOT NULL,
                    source_public_id TEXT NOT NULL,
                    target_name TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    provenance TEXT NOT NULL,
                    confidence REAL NOT NULL,
                    explanation TEXT NOT NULL,
                    evidence_file TEXT NOT NULL,
                    evidence_line INTEGER NOT NULL
                );
                CREATE TABLE unresolved_calls (
                    id INTEGER PRIMARY KEY,
                    file_id INTEGER NOT NULL,
                    caller_public_id TEXT NOT NULL,
                    callee_name TEXT NOT NULL,
                    evidence_file TEXT NOT NULL,
                    evidence_line INTEGER NOT NULL
                );
                ",
            )
            .unwrap();
        drop(connection);

        let store = Store::open(&path).unwrap();
        let mut statement = store
            .connection
            .prepare("PRAGMA table_info(unresolved_references)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(columns.contains(&"binding_name".to_owned()));
        assert!(columns.contains(&"target_file_hint".to_owned()));

        let mut statement = store
            .connection
            .prepare("PRAGMA table_info(unresolved_calls)")
            .unwrap();
        let call_columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(call_columns.contains(&"receiver_type".to_owned()));
        assert!(call_columns.contains(&"target_file_hint".to_owned()));
        assert!(call_columns.contains(&"provenance".to_owned()));
        assert!(call_columns.contains(&"confidence".to_owned()));
        assert!(call_columns.contains(&"explanation".to_owned()));
        assert!(call_columns.contains(&"resolvable".to_owned()));
        assert!(call_columns.contains(&"start_byte".to_owned()));
        assert!(call_columns.contains(&"receiver_call_start_byte".to_owned()));
        let callable_returns_exist = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name='callable_return_types'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap();
        assert_eq!(callable_returns_exist, 1);
        let delegations_exist = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name='callback_parameter_delegations'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap();
        assert_eq!(delegations_exist, 1);
        let resolved_calls_are_temporary = store
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_temp_master
                 WHERE type='table' AND name='resolved_call_targets'",
                [],
                |row| row.get::<_, usize>(0),
            )
            .unwrap();
        assert_eq!(resolved_calls_are_temporary, 1);
    }
}
