use crate::model::{
    ArkuiBuilderFlowFacts, EventChannel, Evidence, FileFacts, Language, Relationship,
    RelationshipKind, Symbol, SymbolKind, GRAPH_MODEL_VERSION,
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    time::Instant,
};

const SCHEMA_VERSION: u32 = 1;
const WAL_AUTOCHECKPOINT_PAGES: u32 = 256;
const JOURNAL_SIZE_LIMIT_BYTES: u64 = 16 * 1024 * 1024;
const INLINE_CALLBACK_DEPTH_CAP: usize = 16;

pub struct Store {
    connection: Connection,
    path: PathBuf,
}

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
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
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
                UNIQUE(source_public_id, target_public_id, kind, provenance, evidence_file, evidence_line)
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
            CREATE TABLE IF NOT EXISTS callback_parameter_invocations (
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                owner_public_id TEXT NOT NULL,
                parameter_index INTEGER NOT NULL,
                PRIMARY KEY(file_id,owner_public_id,parameter_index)
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
        Self::ensure_column(
            &self.connection,
            "unresolved_calls",
            "fallback_caller_public_id",
            "TEXT",
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
                    explanation,evidence_file,evidence_line
             FROM relationships
             ORDER BY source_public_id,target_public_id,kind,provenance,evidence_file,evidence_line",
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
    ) -> Result<(u64, usize, usize, u128, u128)>
    where
        I: IntoIterator<Item = Result<FileFacts>>,
    {
        let next_epoch = self.epoch()? + 1;
        let tx = self.connection.transaction()?;
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
        let resolution_ms = resolution_started.elapsed().as_millis();
        tx.commit()?;
        let _: (u32, u32, u32) =
            self.connection
                .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?;
        Ok((
            next_epoch,
            relationships_resolved,
            symbols_changed,
            staging_ms,
            resolution_ms,
        ))
    }

    fn finish_epoch(tx: &Transaction<'_>, next_epoch: u64) -> Result<usize> {
        Self::clear_inline_callback_symbols(tx)?;
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
        let next_epoch = self.epoch()? + 1;
        let tx = self.connection.transaction()?;
        for path in deleted {
            Self::delete_file(&tx, path)?;
        }
        for file in facts {
            Self::replace_file(&tx, file, next_epoch)?;
        }
        Self::finish_epoch(&tx, next_epoch)?;
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
                    callee_name,receiver_binding,receiver_type,
                    target_file_hint,provenance,confidence,explanation,resolvable,
                    evidence_file,evidence_line,start_byte
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            )?
            .execute(params![
                file_id,
                call.caller_id,
                call.fallback_caller_id,
                call.callee_name,
                call.receiver_binding,
                call.receiver_type,
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
                    u.file_id,u.resolvable,u.start_byte,u.fallback_caller_public_id
             FROM unresolved_calls u
             LEFT JOIN symbols primary_symbol
                    ON primary_symbol.public_id=u.caller_public_id
             LEFT JOIN symbols fallback_symbol
                    ON fallback_symbol.public_id=u.fallback_caller_public_id
             WHERE primary_symbol.public_id IS NOT NULL
                OR fallback_symbol.public_id IS NOT NULL
             ORDER BY u.evidence_file,u.evidence_line,u.id",
        )?;
        let calls = calls_statement
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
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(calls_statement);

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
            _start_byte,
            fallback_caller_id,
        ) in calls
        {
            if !resolvable {
                continue;
            }
            let receiver_binding = receiver_binding.unwrap_or_default();
            let receiver_type = receiver_type.unwrap_or_default();
            let target_file_hint = target_file_hint.unwrap_or_default();
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
                let receiver_qualified = format!("{receiver_type}.{callee_name}");
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
                } else {
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
                if !is_arkui_route && targets.is_empty() {
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
                if !is_arkui_route && targets.is_empty() {
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
                if !is_arkui_route && targets.is_empty() {
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
            let resolution_confidence: f64 = match (best_rank, target_count) {
                (0, 1) => 0.995,
                (1, 1) => 0.99,
                (2, 1) => 0.97,
                (0..=2, _) => 0.65,
                (3, 1) => 0.9,
                (4, 1) => 0.75,
                _ => 0.35,
            };
            let scope = match (is_arkui_route, best_rank) {
                (true, 0) => "exact ArkUI entry page",
                (false, 0) if !target_file_hint.is_empty() => "imported package",
                (false, 0) => "receiver type",
                (false, 1) => "same-file lexical scope",
                (false, 2) => "explicit import scope",
                (false, 3) => "verified Harmony project import scope",
                _ => "language-wide fallback",
            };
            for (target_id, qualified_name, _) in
                targets.iter().take_while(|target| target.2 == best_rank)
            {
                tx.prepare_cached(
                    "INSERT OR REPLACE INTO resolved_call_targets(
                        call_id,target_public_id,target_qualified_name,
                        resolution_confidence,resolution_scope
                     ) VALUES (?1,?2,?3,?4,?5)",
                )?
                .execute(params![
                    call_id,
                    target_id,
                    qualified_name,
                    resolution_confidence,
                    scope
                ])?;
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
                            fact_confidence.min(resolution_confidence),
                            if resolution_confidence >= 0.75 {
                                format!(
                                    "{explanation}; target resolves to {qualified_name} through {scope}"
                                )
                            } else {
                                format!(
                                    "{explanation}; {scope} has multiple candidates; \
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

    fn publish_deferred_inline_calls(tx: &Transaction<'_>) -> Result<usize> {
        let mut statement = tx.prepare(
            "SELECT COALESCE(primary_symbol.public_id,fallback_symbol.public_id),
                    u.provenance,u.confidence,u.explanation,u.evidence_file,u.evidence_line,
                    t.target_public_id,t.target_qualified_name,
                    t.resolution_confidence,t.resolution_scope
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
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)? as usize,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, f64>(8)?,
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
            explanation,
            file,
            line,
            target_id,
            qualified_name,
            resolution_confidence,
            scope,
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
                                "{explanation}; target resolves to {qualified_name} through {scope}"
                            )
                        } else {
                            format!(
                                "{explanation}; {scope} has multiple candidates; \
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
                |(caller, callee, index, target, hint, symbol, line, start_byte)| {
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
            let initial_formal = (callee_id.clone(), argument_index);
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
                    (consumer_id.as_str(), consumer_index) != (callee_id.as_str(), argument_index);
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
                        argument_index + 1,
                        path.join(" -> "),
                        path.last().expect("terminal path"),
                        consumer_index + 1
                    )
                } else {
                    format!(
                        "{callee_qualified} directly invokes callback parameter {}; \
                         registration resolves it to {target_qualified}",
                        argument_index + 1
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
                explanation, evidence_file, evidence_line
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        )?
        .execute(params![
            relationship.source_id,
            relationship.target_id,
            relationship.kind.to_string(),
            relationship.evidence.provenance,
            relationship.evidence.confidence,
            relationship.evidence.explanation,
            relationship.evidence.file,
            relationship.evidence.line as i64
        ])?;
        Ok(())
    }

    pub fn find_symbols_by_name(&self, name: &str) -> Result<Vec<Symbol>> {
        let mut statement = self.connection.prepare(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE s.name=?1 ORDER BY s.qualified_name,f.path",
        )?;
        let rows = statement.query_map([name], Self::symbol_from_row)?;
        let symbols = Self::collect_symbols(rows)?;
        Ok(symbols)
    }

    pub fn find_symbols(&self, identifier: &str) -> Result<Vec<Symbol>> {
        let mut statement = self.connection.prepare(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line
             FROM symbols s JOIN files f ON f.id=s.file_id
             WHERE s.public_id=?1 OR s.name=?1 OR s.qualified_name=?1
             ORDER BY s.qualified_name,f.path",
        )?;
        let rows = statement.query_map([identifier], Self::symbol_from_row)?;
        let symbols = Self::collect_symbols(rows)?;
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
        let terms = search_terms(query);
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
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
        let (join_side, filter_side) = if incoming {
            ("r.source_public_id", "r.target_public_id")
        } else {
            ("r.target_public_id", "r.source_public_id")
        };
        let sql = format!(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line,
                    r.provenance,r.confidence,r.explanation,r.evidence_file,r.evidence_line
             FROM relationships r JOIN symbols s ON s.public_id={join_side}
             JOIN files f ON f.id=s.file_id
             WHERE {filter_side}=?1 AND r.kind=?2 ORDER BY r.confidence DESC,s.qualified_name"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params![symbol_id, kind.to_string()], |row| {
            Ok((
                Self::symbol_from_row(row)?,
                Evidence {
                    provenance: row.get(11)?,
                    confidence: row.get(12)?,
                    explanation: row.get(13)?,
                    file: row.get(14)?,
                    line: row.get::<_, i64>(15)? as usize,
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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

fn normalized_module_key(path: &str) -> String {
    const EXTENSIONS: &[&str] = &[
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "vue", "svelte", "ets",
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

#[cfg(test)]
mod tests {
    use super::*;

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
