use crate::model::{
    Evidence, FileFacts, Language, Relationship, RelationshipKind, Symbol, SymbolKind,
    GRAPH_MODEL_VERSION,
};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;

pub struct Store {
    connection: Connection,
    path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub symbol: Symbol,
    pub score: f64,
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
                evidence_file TEXT NOT NULL,
                evidence_line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS unresolved_calls_name_idx
                ON unresolved_calls(callee_name);
            INSERT OR IGNORE INTO metadata(key, value) VALUES ('graph_epoch', '0');
            COMMIT;
            ",
        )?;
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        self.connection.execute(
            "INSERT INTO metadata(key, value) VALUES ('graph_model_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [GRAPH_MODEL_VERSION.to_string()],
        )?;
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

    pub(crate) fn publish(
        &mut self,
        facts: &[FileFacts],
        deleted: &[String],
    ) -> Result<(u64, usize)> {
        let next_epoch = self.epoch()? + 1;
        let tx = self.connection.transaction()?;
        for path in deleted {
            Self::delete_file(&tx, path)?;
        }
        for file in facts {
            Self::replace_file(&tx, file, next_epoch)?;
        }
        let relationships_resolved = Self::resolve_calls(&tx)?;
        tx.execute(
            "UPDATE metadata SET value = ?1 WHERE key = 'graph_epoch'",
            [next_epoch.to_string()],
        )?;
        tx.commit()?;
        self.connection
            .pragma_update(None, "wal_checkpoint", "PASSIVE")?;
        Ok((next_epoch, relationships_resolved))
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
        tx.execute(
            "INSERT INTO files(path, content_hash, language, indexed_epoch) VALUES (?1, ?2, ?3, ?4)",
            params![file.path, file.content_hash, file.language.to_string(), epoch],
        )?;
        let file_id = tx.last_insert_rowid();
        for symbol in &file.symbols {
            tx.execute(
                "INSERT INTO symbols(
                    public_id, semantic_key, file_id, language, kind, name, qualified_name,
                    start_byte, end_byte, start_line, end_line
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
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
                ],
            )?;
            tx.execute(
                "INSERT INTO symbol_search(public_id, name, qualified_name, file)
                 VALUES (?1, ?2, ?3, ?4)",
                params![symbol.id, symbol.name, symbol.qualified_name, symbol.file],
            )?;
        }
        for relationship in &file.relationships {
            Self::insert_relationship(tx, relationship)?;
        }
        for call in &file.unresolved_calls {
            tx.execute(
                "INSERT INTO unresolved_calls(
                    file_id, caller_public_id, callee_name, evidence_file, evidence_line
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    file_id,
                    call.caller_id,
                    call.callee_name,
                    call.file,
                    call.line as i64
                ],
            )?;
        }
        Ok(())
    }

    fn resolve_calls(tx: &Transaction<'_>) -> Result<usize> {
        tx.execute(
            "DELETE FROM relationships WHERE provenance = 'tree-sitter/name-resolution'",
            [],
        )?;
        let mut calls_statement = tx.prepare(
            "SELECT u.caller_public_id,u.callee_name,u.evidence_file,u.evidence_line,s.language
             FROM unresolved_calls u
             JOIN symbols s ON s.public_id=u.caller_public_id
             ORDER BY u.evidence_file,u.evidence_line,u.id",
        )?;
        let calls = calls_statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? as usize,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(calls_statement);

        let mut resolved = 0;
        for (caller_id, callee_name, file, line, language) in calls {
            let mut target_statement = tx.prepare(
                "SELECT public_id,qualified_name FROM symbols
                 WHERE name=?1 AND public_id<>?2 AND language=?3
                 ORDER BY qualified_name,public_id",
            )?;
            let targets = target_statement
                .query_map(params![callee_name, caller_id, language], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let confidence = match targets.len() {
                0 => continue,
                1 => 0.95,
                _ => 0.55,
            };
            for (target_id, qualified_name) in targets {
                Self::insert_relationship(
                    tx,
                    &Relationship {
                        source_id: caller_id.clone(),
                        target_id,
                        kind: RelationshipKind::Calls,
                        evidence: Evidence::new(
                            "tree-sitter/name-resolution",
                            confidence,
                            if confidence > 0.9 {
                                format!("call name uniquely resolves to {qualified_name}")
                            } else {
                                format!(
                                    "call name has multiple candidates; {qualified_name} is a possible target"
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

    fn insert_relationship(tx: &Transaction<'_>, relationship: &Relationship) -> Result<()> {
        tx.execute(
            "INSERT OR REPLACE INTO relationships(
                source_public_id, target_public_id, kind, provenance, confidence,
                explanation, evidence_file, evidence_line
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                relationship.source_id,
                relationship.target_id,
                relationship.kind.to_string(),
                relationship.evidence.provenance,
                relationship.evidence.confidence,
                relationship.evidence.explanation,
                relationship.evidence.file,
                relationship.evidence.line as i64
            ],
        )?;
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
        let escaped = query
            .split_whitespace()
            .map(|part| format!("\"{}\"*", part.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let mut statement = self.connection.prepare(
            "SELECT s.public_id,s.semantic_key,s.language,s.kind,s.name,s.qualified_name,
                    f.path,s.start_byte,s.end_byte,s.start_line,s.end_line,
                    bm25(symbol_search)
             FROM symbol_search
             JOIN symbols s ON s.public_id=symbol_search.public_id
             JOIN files f ON f.id=s.file_id
             WHERE symbol_search MATCH ?1 ORDER BY bm25(symbol_search) LIMIT ?2",
        )?;
        let rows = statement.query_map(params![escaped, limit as i64], |row| {
            Ok(SearchHit {
                symbol: Self::symbol_from_row(row)?,
                score: -row.get::<_, f64>(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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

fn parse_language(value: &str) -> Language {
    match value {
        "tsx" => Language::Tsx,
        "javascript" => Language::JavaScript,
        "jsx" => Language::Jsx,
        "python" => Language::Python,
        "rust" => Language::Rust,
        _ => Language::TypeScript,
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
        "method" => SymbolKind::Method,
        "variable" => SymbolKind::Variable,
        _ => SymbolKind::Function,
    }
}
