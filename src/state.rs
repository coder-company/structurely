use crate::engine::PROJECT_DIR;
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const DATABASE_FILE: &str = "state.db";
const SCHEMA_VERSION: i64 = 1;
const MAX_NAME_BYTES: usize = 200;
const MAX_TITLE_BYTES: usize = 500;
const MAX_KIND_BYTES: usize = 100;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_SUMMARY_BYTES: usize = 64 * 1024;
const MAX_TAGS: usize = 32;
const MAX_TAG_BYTES: usize = 100;
const MAX_RESULTS: usize = 100;
const RECAP_EVENT_LIMIT: usize = 50;

static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct StateStore {
    path: PathBuf,
    connection: Connection,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub status: SessionStatus,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    pub ended_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Completed,
}

impl SessionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Completed => "completed",
        }
    }

    fn parse(value: &str) -> rusqlite::Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            other => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown session status {other:?}").into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEvent {
    pub id: String,
    pub session_id: String,
    pub sequence: u64,
    pub kind: String,
    pub body: String,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Recap {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub summary: String,
    pub event_count: usize,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Memory {
    pub id: String,
    pub workspace_id: String,
    pub body: String,
    pub tags: Vec<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHit {
    pub memory: Memory,
    pub score: f64,
}

impl StateStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize()?;
        let directory = root.join(PROJECT_DIR);
        fs::create_dir_all(&directory)
            .with_context(|| format!("create state directory {}", directory.display()))?;
        reject_symlink(&directory)?;
        let path = directory.join(DATABASE_FILE);
        reject_symlink(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| format!("open durable state {}", path.display()))?;
        configure_connection(&connection)?;
        let mut store = Self { path, connection };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_read_only(root: impl AsRef<Path>) -> Result<Option<Self>> {
        let root = root.as_ref().canonicalize()?;
        let path = root.join(PROJECT_DIR).join(DATABASE_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        reject_symlink(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .with_context(|| format!("open read-only durable state {}", path.display()))?;
        validate_schema(&connection)?;
        connection.pragma_update(None, "query_only", "ON")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        Ok(Some(Self { path, connection }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_workspace(&self, name: &str) -> Result<Workspace> {
        let name = required_text("workspace name", name, MAX_NAME_BYTES)?;
        let workspace = Workspace {
            id: new_id("ws"),
            name: name.to_owned(),
            created_at_ms: now_ms()?,
            updated_at_ms: now_ms()?,
        };
        self.connection.execute(
            "INSERT INTO workspaces(id,name,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4)",
            params![
                workspace.id,
                workspace.name,
                workspace.created_at_ms,
                workspace.updated_at_ms
            ],
        )?;
        Ok(workspace)
    }

    pub fn workspace(&self, id: &str) -> Result<Option<Workspace>> {
        validate_id("workspace id", id)?;
        self.connection
            .query_row(
                "SELECT id,name,created_at_ms,updated_at_ms FROM workspaces WHERE id=?1",
                [id],
                workspace_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_workspaces(&self, limit: usize) -> Result<Vec<Workspace>> {
        validate_limit(limit)?;
        let mut statement = self.connection.prepare(
            "SELECT id,name,created_at_ms,updated_at_ms
             FROM workspaces ORDER BY updated_at_ms DESC,id LIMIT ?1",
        )?;
        let workspaces = statement
            .query_map([limit as i64], workspace_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(workspaces)
    }

    pub fn rename_workspace(&self, id: &str, name: &str) -> Result<Workspace> {
        validate_id("workspace id", id)?;
        let name = required_text("workspace name", name, MAX_NAME_BYTES)?;
        let changed = self.connection.execute(
            "UPDATE workspaces SET name=?2,updated_at_ms=?3 WHERE id=?1",
            params![id, name, now_ms()?],
        )?;
        anyhow::ensure!(changed == 1, "workspace not found: {id}");
        self.workspace(id)?
            .context("workspace disappeared after rename")
    }

    pub fn create_session(&self, workspace_id: &str, title: &str) -> Result<Session> {
        self.require_workspace(workspace_id)?;
        let title = required_text("session title", title, MAX_TITLE_BYTES)?;
        let timestamp = now_ms()?;
        let session = Session {
            id: new_id("session"),
            workspace_id: workspace_id.to_owned(),
            title: title.to_owned(),
            status: SessionStatus::Active,
            started_at_ms: timestamp,
            updated_at_ms: timestamp,
            ended_at_ms: None,
        };
        self.connection.execute(
            "INSERT INTO sessions(
               id,workspace_id,title,status,started_at_ms,updated_at_ms,ended_at_ms
             ) VALUES(?1,?2,?3,?4,?5,?6,NULL)",
            params![
                session.id,
                session.workspace_id,
                session.title,
                session.status.as_str(),
                session.started_at_ms,
                session.updated_at_ms
            ],
        )?;
        Ok(session)
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        validate_id("session id", id)?;
        self.connection
            .query_row(
                "SELECT id,workspace_id,title,status,started_at_ms,updated_at_ms,ended_at_ms
                 FROM sessions WHERE id=?1",
                [id],
                session_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_sessions(&self, workspace_id: Option<&str>, limit: usize) -> Result<Vec<Session>> {
        validate_limit(limit)?;
        if let Some(workspace_id) = workspace_id {
            self.require_workspace(workspace_id)?;
            let mut statement = self.connection.prepare(
                "SELECT id,workspace_id,title,status,started_at_ms,updated_at_ms,ended_at_ms
                 FROM sessions WHERE workspace_id=?1
                 ORDER BY updated_at_ms DESC,id LIMIT ?2",
            )?;
            return Ok(statement
                .query_map(params![workspace_id, limit as i64], session_from_row)?
                .collect::<rusqlite::Result<_>>()?);
        }
        let mut statement = self.connection.prepare(
            "SELECT id,workspace_id,title,status,started_at_ms,updated_at_ms,ended_at_ms
             FROM sessions ORDER BY updated_at_ms DESC,id LIMIT ?1",
        )?;
        let sessions = statement
            .query_map([limit as i64], session_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(sessions)
    }

    pub fn append_event(
        &mut self,
        session_id: &str,
        kind: &str,
        body: &str,
    ) -> Result<SessionEvent> {
        validate_id("session id", session_id)?;
        let kind = required_text("event kind", kind, MAX_KIND_BYTES)?;
        let body = required_text("event body", body, MAX_BODY_BYTES)?;
        let timestamp = now_ms()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status = transaction
            .query_row(
                "SELECT status FROM sessions WHERE id=?1",
                [session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .with_context(|| format!("session not found: {session_id}"))?;
        anyhow::ensure!(status == "active", "session is not active: {session_id}");
        let sequence = transaction.query_row(
            "SELECT COALESCE(MAX(sequence),0)+1 FROM session_events WHERE session_id=?1",
            [session_id],
            |row| row.get::<_, i64>(0),
        )?;
        let event = SessionEvent {
            id: new_id("event"),
            session_id: session_id.to_owned(),
            sequence: sequence as u64,
            kind: kind.to_owned(),
            body: body.to_owned(),
            created_at_ms: timestamp,
        };
        transaction.execute(
            "INSERT INTO session_events(id,session_id,sequence,kind,body,created_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                event.id,
                event.session_id,
                event.sequence as i64,
                event.kind,
                event.body,
                event.created_at_ms
            ],
        )?;
        transaction.execute(
            "UPDATE sessions SET updated_at_ms=?2 WHERE id=?1",
            params![session_id, timestamp],
        )?;
        transaction.commit()?;
        Ok(event)
    }

    pub fn events(&self, session_id: &str, limit: usize) -> Result<Vec<SessionEvent>> {
        validate_id("session id", session_id)?;
        validate_limit(limit)?;
        let mut statement = self.connection.prepare(
            "SELECT id,session_id,sequence,kind,body,created_at_ms
             FROM session_events WHERE session_id=?1 ORDER BY sequence LIMIT ?2",
        )?;
        let events = statement
            .query_map(params![session_id, limit as i64], event_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(events)
    }

    pub fn complete_session(&self, id: &str) -> Result<Session> {
        validate_id("session id", id)?;
        let timestamp = now_ms()?;
        let changed = self.connection.execute(
            "UPDATE sessions
             SET status='completed',updated_at_ms=?2,ended_at_ms=COALESCE(ended_at_ms,?2)
             WHERE id=?1",
            params![id, timestamp],
        )?;
        anyhow::ensure!(changed == 1, "session not found: {id}");
        self.session(id)?
            .context("session disappeared after completion")
    }

    pub fn save_recap(&self, session_id: &str, title: &str, summary: &str) -> Result<Recap> {
        validate_id("session id", session_id)?;
        anyhow::ensure!(
            self.session(session_id)?.is_some(),
            "session not found: {session_id}"
        );
        let title = required_text("recap title", title, MAX_TITLE_BYTES)?;
        let summary = required_text("recap summary", summary, MAX_SUMMARY_BYTES)?;
        let event_count = self.connection.query_row(
            "SELECT COUNT(*) FROM session_events WHERE session_id=?1",
            [session_id],
            |row| row.get::<_, i64>(0),
        )?;
        let timestamp = now_ms()?;
        let existing = self.recap(session_id)?;
        let id = existing
            .as_ref()
            .map_or_else(|| new_id("recap"), |recap| recap.id.clone());
        let created_at_ms = existing
            .as_ref()
            .map_or(timestamp, |recap| recap.created_at_ms);
        self.connection.execute(
            "INSERT INTO recaps(
               id,session_id,title,summary,event_count,created_at_ms,updated_at_ms
             ) VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(session_id) DO UPDATE SET
               title=excluded.title,summary=excluded.summary,event_count=excluded.event_count,
               updated_at_ms=excluded.updated_at_ms",
            params![
                id,
                session_id,
                title,
                summary,
                event_count,
                created_at_ms,
                timestamp
            ],
        )?;
        self.recap(session_id)?
            .context("recap disappeared after save")
    }

    pub fn generate_recap(&self, session_id: &str) -> Result<Recap> {
        let session = self
            .session(session_id)?
            .with_context(|| format!("session not found: {session_id}"))?;
        let events = self.events(session_id, RECAP_EVENT_LIMIT)?;
        let mut summary = if events.is_empty() {
            "No session events were recorded.".to_owned()
        } else {
            events
                .iter()
                .map(|event| format!("- [{}] {}", event.kind, event.body))
                .collect::<Vec<_>>()
                .join("\n")
        };
        truncate_utf8(&mut summary, MAX_SUMMARY_BYTES);
        self.save_recap(session_id, &session.title, &summary)
    }

    pub fn recap(&self, session_id: &str) -> Result<Option<Recap>> {
        validate_id("session id", session_id)?;
        self.connection
            .query_row(
                "SELECT id,session_id,title,summary,event_count,created_at_ms,updated_at_ms
                 FROM recaps WHERE session_id=?1",
                [session_id],
                recap_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn remember(&mut self, workspace_id: &str, body: &str, tags: &[String]) -> Result<Memory> {
        self.require_workspace(workspace_id)?;
        let body = required_text("memory body", body, MAX_BODY_BYTES)?;
        let tags = validate_tags(tags)?;
        let timestamp = now_ms()?;
        let memory = Memory {
            id: new_id("memory"),
            workspace_id: workspace_id.to_owned(),
            body: body.to_owned(),
            tags,
            created_at_ms: timestamp,
            updated_at_ms: timestamp,
        };
        let tags_json = serde_json::to_string(&memory.tags)?;
        let tags_search = memory.tags.join(" ");
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO memories(id,workspace_id,body,tags_json,created_at_ms,updated_at_ms)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                memory.id,
                memory.workspace_id,
                memory.body,
                tags_json,
                memory.created_at_ms,
                memory.updated_at_ms
            ],
        )?;
        transaction.execute(
            "INSERT INTO memory_search(id,workspace_id,body,tags) VALUES(?1,?2,?3,?4)",
            params![memory.id, memory.workspace_id, memory.body, tags_search],
        )?;
        transaction.commit()?;
        Ok(memory)
    }

    pub fn memory(&self, id: &str) -> Result<Option<Memory>> {
        validate_id("memory id", id)?;
        self.connection
            .query_row(
                "SELECT id,workspace_id,body,tags_json,created_at_ms,updated_at_ms
                 FROM memories WHERE id=?1",
                [id],
                memory_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn search_memories(
        &self,
        workspace_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryHit>> {
        self.require_workspace(workspace_id)?;
        required_text("memory query", query, MAX_TITLE_BYTES)?;
        validate_limit(limit)?;
        let terms = search_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let expression = terms
            .iter()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let mut statement = self.connection.prepare(
            "SELECT m.id,m.workspace_id,m.body,m.tags_json,m.created_at_ms,m.updated_at_ms,
                    bm25(memory_search,2.0,5.0)
             FROM memory_search s
             JOIN memories m ON m.id=s.id
             WHERE memory_search MATCH ?1 AND s.workspace_id=?2
             ORDER BY bm25(memory_search,2.0,5.0),m.updated_at_ms DESC,m.id
             LIMIT ?3",
        )?;
        let hits = statement
            .query_map(params![expression, workspace_id, limit as i64], |row| {
                Ok(MemoryHit {
                    memory: memory_from_row(row)?,
                    score: -row.get::<_, f64>(6)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(hits)
    }

    pub fn forget(&mut self, id: &str) -> Result<bool> {
        validate_id("memory id", id)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute("DELETE FROM memory_search WHERE id=?1", [id])?;
        let changed = transaction.execute("DELETE FROM memories WHERE id=?1", [id])?;
        transaction.commit()?;
        Ok(changed == 1)
    }

    fn require_workspace(&self, id: &str) -> Result<()> {
        validate_id("workspace id", id)?;
        anyhow::ensure!(self.workspace(id)?.is_some(), "workspace not found: {id}");
        Ok(())
    }

    fn migrate(&mut self) -> Result<()> {
        let version = schema_version(&self.connection)?;
        anyhow::ensure!(
            version <= SCHEMA_VERSION,
            "durable state schema {version} is newer than supported schema {SCHEMA_VERSION}"
        );
        if version == 0 {
            let transaction = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "CREATE TABLE workspaces(
                   id TEXT PRIMARY KEY,
                   name TEXT NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE sessions(
                   id TEXT PRIMARY KEY,
                   workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                   title TEXT NOT NULL,
                   status TEXT NOT NULL CHECK(status IN ('active','completed')),
                   started_at_ms INTEGER NOT NULL,
                   updated_at_ms INTEGER NOT NULL,
                   ended_at_ms INTEGER
                 ) STRICT;
                 CREATE INDEX sessions_workspace_updated
                   ON sessions(workspace_id,updated_at_ms DESC);
                 CREATE TABLE session_events(
                   id TEXT PRIMARY KEY,
                   session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                   sequence INTEGER NOT NULL,
                   kind TEXT NOT NULL,
                   body TEXT NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   UNIQUE(session_id,sequence)
                 ) STRICT;
                 CREATE TABLE recaps(
                   id TEXT PRIMARY KEY,
                   session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE,
                   title TEXT NOT NULL,
                   summary TEXT NOT NULL,
                   event_count INTEGER NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 CREATE TABLE memories(
                   id TEXT PRIMARY KEY,
                   workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                   body TEXT NOT NULL,
                   tags_json TEXT NOT NULL,
                   created_at_ms INTEGER NOT NULL,
                   updated_at_ms INTEGER NOT NULL
                 ) STRICT;
                 CREATE INDEX memories_workspace_updated
                   ON memories(workspace_id,updated_at_ms DESC);
                 CREATE VIRTUAL TABLE memory_search USING fts5(
                   id UNINDEXED,
                   workspace_id UNINDEXED,
                   body,
                   tags,
                   tokenize='unicode61 remove_diacritics 2'
                 );",
            )?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            transaction.commit()?;
        }
        validate_schema(&self.connection)
    }
}

fn configure_connection(connection: &Connection) -> Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "busy_timeout", 5_000)?;
    Ok(())
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let version = schema_version(connection)?;
    anyhow::ensure!(
        version == SCHEMA_VERSION,
        "unsupported durable state schema {version}; expected {SCHEMA_VERSION}"
    );
    for table in [
        "workspaces",
        "sessions",
        "session_events",
        "recaps",
        "memories",
        "memory_search",
    ] {
        let exists = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name=?1)",
            [table],
            |row| row.get::<_, bool>(0),
        )?;
        anyhow::ensure!(exists, "durable state schema is missing {table}");
    }
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i64> {
    Ok(connection.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

fn workspace_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    Ok(Workspace {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at_ms: row.get(2)?,
        updated_at_ms: row.get(3)?,
    })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let status = row.get::<_, String>(3)?;
    Ok(Session {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        title: row.get(2)?,
        status: SessionStatus::parse(&status)?,
        started_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
        ended_at_ms: row.get(6)?,
    })
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionEvent> {
    Ok(SessionEvent {
        id: row.get(0)?,
        session_id: row.get(1)?,
        sequence: row.get::<_, i64>(2)? as u64,
        kind: row.get(3)?,
        body: row.get(4)?,
        created_at_ms: row.get(5)?,
    })
}

fn recap_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Recap> {
    Ok(Recap {
        id: row.get(0)?,
        session_id: row.get(1)?,
        title: row.get(2)?,
        summary: row.get(3)?,
        event_count: row.get::<_, i64>(4)? as usize,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
    })
}

fn memory_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let tags_json = row.get::<_, String>(3)?;
    let tags = serde_json::from_str(&tags_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Memory {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        body: row.get(2)?,
        tags,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
    })
}

fn required_text<'a>(field: &str, value: &'a str, max_bytes: usize) -> Result<&'a str> {
    let value = value.trim();
    anyhow::ensure!(!value.is_empty(), "{field} must not be empty");
    anyhow::ensure!(
        value.len() <= max_bytes,
        "{field} exceeds the {max_bytes}-byte limit"
    );
    anyhow::ensure!(!value.contains('\0'), "{field} must not contain NUL");
    Ok(value)
}

fn validate_id(field: &str, value: &str) -> Result<()> {
    required_text(field, value, 200)?;
    Ok(())
}

fn validate_limit(limit: usize) -> Result<()> {
    anyhow::ensure!(
        (1..=MAX_RESULTS).contains(&limit),
        "limit must be 1-{MAX_RESULTS}"
    );
    Ok(())
}

fn validate_tags(tags: &[String]) -> Result<Vec<String>> {
    anyhow::ensure!(
        tags.len() <= MAX_TAGS,
        "a memory may have at most {MAX_TAGS} tags"
    );
    let mut normalized = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = required_text("memory tag", tag, MAX_TAG_BYTES)?.to_owned();
        if !normalized.contains(&tag) {
            normalized.push(tag);
        }
    }
    Ok(normalized)
}

fn search_terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|term| !term.is_empty())
        .take(20)
        .map(str::to_lowercase)
        .collect()
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn now_ms() -> Result<i64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    i64::try_from(millis).context("system timestamp exceeds SQLite integer range")
}

fn new_id(prefix: &str) -> String {
    let sequence = ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut input = Vec::with_capacity(64);
    input.extend_from_slice(prefix.as_bytes());
    input.extend_from_slice(&std::process::id().to_le_bytes());
    input.extend_from_slice(&sequence.to_le_bytes());
    if let Ok(duration) = SystemTime::now().duration_since(UNIX_EPOCH) {
        input.extend_from_slice(&duration.as_nanos().to_le_bytes());
    }
    let digest = blake3::hash(&input).to_hex();
    format!("{prefix}_{}", &digest[..24])
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "refusing to use symlink for durable state: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_events_and_generated_recap_survive_reopen() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open(root.path()).unwrap();
        let workspace = store.create_workspace("Compiler team").unwrap();
        let session = store
            .create_session(&workspace.id, "Trace publication")
            .unwrap();
        let first = store
            .append_event(&session.id, "decision", "Use atomic rename.")
            .unwrap();
        let second = store
            .append_event(&session.id, "result", "Crash recovery passed.")
            .unwrap();
        assert_eq!((first.sequence, second.sequence), (1, 2));
        let recap = store.generate_recap(&session.id).unwrap();
        assert!(recap.summary.contains("atomic rename"));
        assert_eq!(recap.event_count, 2);
        let completed = store.complete_session(&session.id).unwrap();
        assert_eq!(completed.status, SessionStatus::Completed);
        assert!(store
            .append_event(&session.id, "late", "not allowed")
            .is_err());
        drop(store);

        let reopened = StateStore::open_read_only(root.path()).unwrap().unwrap();
        assert_eq!(reopened.events(&session.id, 10).unwrap().len(), 2);
        assert_eq!(reopened.recap(&session.id).unwrap().unwrap(), recap);
    }

    #[test]
    fn memory_search_is_ranked_and_workspace_scoped() {
        let root = tempdir().unwrap();
        let mut store = StateStore::open(root.path()).unwrap();
        let alpha = store.create_workspace("Alpha").unwrap();
        let beta = store.create_workspace("Beta").unwrap();
        let wanted = store
            .remember(
                &alpha.id,
                "Publish files with an atomic rename.",
                &["filesystem".to_owned(), "publication".to_owned()],
            )
            .unwrap();
        store
            .remember(&alpha.id, "Parser recovery notes.", &[])
            .unwrap();
        store
            .remember(&beta.id, "Atomic publication belongs elsewhere.", &[])
            .unwrap();

        let hits = store
            .search_memories(&alpha.id, "atomic publication", 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].memory.id, wanted.id);
        assert!(store.forget(&wanted.id).unwrap());
        assert!(!store.forget(&wanted.id).unwrap());
        assert!(store
            .search_memories(&alpha.id, "atomic publication", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn validates_bounds_and_rejects_newer_schemas() {
        let root = tempdir().unwrap();
        let store = StateStore::open(root.path()).unwrap();
        assert!(store.create_workspace("   ").is_err());
        assert!(store.list_workspaces(0).is_err());
        drop(store);

        let path = root.path().join(PROJECT_DIR).join(DATABASE_FILE);
        let connection = Connection::open(path).unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(connection);
        let error = StateStore::open(root.path()).err().unwrap();
        assert!(error.to_string().contains("newer than supported"));
    }

    #[test]
    fn workspace_and_session_listing_are_bounded() {
        let root = tempdir().unwrap();
        let store = StateStore::open(root.path()).unwrap();
        let workspace = store.create_workspace("Initial").unwrap();
        let renamed = store.rename_workspace(&workspace.id, "Renamed").unwrap();
        assert_eq!(renamed.name, "Renamed");
        store.create_session(&workspace.id, "One").unwrap();
        store.create_session(&workspace.id, "Two").unwrap();
        assert_eq!(store.list_workspaces(10).unwrap().len(), 1);
        assert_eq!(
            store.list_sessions(Some(&workspace.id), 1).unwrap().len(),
            1
        );
        assert!(store.list_sessions(None, MAX_RESULTS + 1).is_err());
    }
}
