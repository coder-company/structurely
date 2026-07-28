use crate::model::{
    Evidence, FileFacts, Language, Relationship, RelationshipKind, Symbol, SymbolKind,
    GRAPH_MODEL_VERSION,
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Instant,
};

const SCHEMA_VERSION: u32 = 1;
const WAL_AUTOCHECKPOINT_PAGES: u32 = 256;
const JOURNAL_SIZE_LIMIT_BYTES: u64 = 16 * 1024 * 1024;

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
                callee_name TEXT NOT NULL,
                receiver_type TEXT,
                target_file_hint TEXT,
                provenance TEXT NOT NULL DEFAULT 'tree-sitter/name-resolution',
                confidence REAL NOT NULL DEFAULT 1.0,
                explanation TEXT NOT NULL DEFAULT 'direct call expression',
                resolvable INTEGER NOT NULL DEFAULT 1,
                evidence_file TEXT NOT NULL,
                evidence_line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS unresolved_calls_name_idx
                ON unresolved_calls(callee_name);
            CREATE TABLE IF NOT EXISTS dynamic_events (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                owner_public_id TEXT NOT NULL,
                receiver TEXT NOT NULL,
                channel TEXT NOT NULL,
                action TEXT NOT NULL,
                callback_name TEXT,
                evidence_file TEXT NOT NULL,
                evidence_line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS dynamic_events_match_idx
                ON dynamic_events(file_id,receiver,channel,action);
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
            "unresolved_references",
            "binding_name",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        Self::ensure_search_schema(&self.connection)?;
        Self::ensure_column(
            &self.connection,
            "unresolved_references",
            "target_file_hint",
            "TEXT",
        )?;
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        self.connection.execute(
            "INSERT OR IGNORE INTO metadata(key, value) VALUES ('graph_model_version', ?1)",
            [GRAPH_MODEL_VERSION.to_string()],
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
        let mut relationships_resolved = Self::resolve_calls(tx)?;
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
                    file_id,caller_public_id,callee_name,receiver_type,target_file_hint,
                    provenance,confidence,explanation,resolvable,evidence_file,evidence_line
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            )?
            .execute(params![
                file_id,
                call.caller_id,
                call.callee_name,
                call.receiver_type,
                call.target_file_hint,
                call.provenance,
                call.confidence,
                call.explanation,
                call.resolvable,
                call.file,
                call.line as i64
            ])?;
        }
        for event in &file.dynamic_events {
            tx.prepare_cached(
                "INSERT INTO dynamic_events(
                    file_id,owner_public_id,receiver,channel,action,callback_name,
                    evidence_file,evidence_line
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?
            .execute(params![
                file_id,
                event.owner_id,
                event.receiver,
                event.channel,
                event.action.to_string(),
                event.callback_name,
                event.file,
                event.line as i64
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
        tx.execute(
            "DELETE FROM relationships
             WHERE provenance IN (
                'tree-sitter/name-resolution',
                'tree-sitter/callback-registration',
                'framework/express-route',
                'framework/fastapi-route',
                'framework/react-router',
                'dynamic/event-registration'
             )",
            [],
        )?;
        let mut calls_statement = tx.prepare(
            "SELECT u.caller_public_id,u.callee_name,u.receiver_type,u.target_file_hint,
                    u.provenance,u.confidence,u.explanation,u.evidence_file,u.evidence_line,
                    s.language,u.file_id,u.resolvable
             FROM unresolved_calls u
             JOIN symbols s ON s.public_id=u.caller_public_id
             ORDER BY u.evidence_file,u.evidence_line,u.id",
        )?;
        let calls = calls_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)? as usize,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, bool>(11)?,
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
        let mut candidate_cache =
            HashMap::<(String, String, i64, String, String), Vec<(String, String, i64)>>::new();
        for (
            caller_id,
            callee_name,
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
        ) in calls
        {
            if !resolvable {
                continue;
            }
            let receiver_type = receiver_type.unwrap_or_default();
            let target_file_hint = target_file_hint.unwrap_or_default();
            let cache_key = (
                callee_name.clone(),
                language.clone(),
                file_id,
                receiver_type.clone(),
                target_file_hint.clone(),
            );
            if !candidate_cache.contains_key(&cache_key) {
                let direct = direct_candidates
                    .get(&(callee_name.clone(), language.clone()))
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let receiver_qualified = format!("{receiver_type}.{callee_name}");
                let mut targets = direct
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
                    .collect::<Vec<_>>();
                if targets.is_empty() {
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
                if targets.is_empty() {
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
                if targets.is_empty() {
                    targets.extend(
                        direct
                            .iter()
                            .take(CALL_FANOUT_CAP + 1)
                            .map(|candidate| (candidate.0.clone(), candidate.1.clone(), 3)),
                    );
                }
                targets.sort_by(|left, right| {
                    left.2
                        .cmp(&right.2)
                        .then_with(|| left.1.cmp(&right.1))
                        .then_with(|| left.0.cmp(&right.0))
                });
                targets.dedup_by(|left, right| left.0 == right.0);
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
                (3, 1) => 0.75,
                _ => 0.35,
            };
            let scope = match best_rank {
                0 if !target_file_hint.is_empty() => "imported package",
                0 => "receiver type",
                1 => "same-file lexical scope",
                2 => "explicit import scope",
                _ => "language-wide fallback",
            };
            for (target_id, qualified_name, _) in
                targets.iter().take_while(|target| target.2 == best_rank)
            {
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
        let mut dispatch_statement = tx.prepare(
            "SELECT id,file_id,owner_public_id,receiver,channel,evidence_file,evidence_line
             FROM dynamic_events
             WHERE action='dispatch'
             ORDER BY evidence_file,evidence_line,id",
        )?;
        let dispatches = dispatch_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)? as usize,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(dispatch_statement);

        let mut registrations = tx.prepare(
            "SELECT DISTINCT rel.target_public_id,r.evidence_file,r.evidence_line
             FROM dynamic_events r
             JOIN relationships rel
               ON rel.source_public_id=r.owner_public_id
              AND rel.provenance='tree-sitter/callback-registration'
             JOIN symbols target
               ON target.public_id=rel.target_public_id
              AND target.name=r.callback_name
             WHERE r.file_id=?1 AND r.receiver=?2 AND r.channel=?3
               AND r.action='register'
             ORDER BY r.evidence_file,r.evidence_line,rel.target_public_id",
        )?;
        let mut resolved = 0;
        for (_, file_id, dispatcher_id, receiver, channel, file, line) in dispatches {
            let targets = registrations
                .query_map(params![file_id, receiver, channel], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)? as usize,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if targets.is_empty() || targets.len() > EVENT_FANOUT_CAP {
                continue;
            }
            for (target_id, registration_file, registration_line) in targets {
                if dispatcher_id == target_id {
                    continue;
                }
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: dispatcher_id.clone(),
                        target_id,
                        kind: RelationshipKind::Calls,
                        evidence: Evidence::new(
                            "dynamic/event-registration",
                            0.92,
                            format!(
                                "literal event `{channel}` on `{receiver}` dispatches to a handler \
                                 registered at {registration_file}:{registration_line}"
                            ),
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
    }
}
