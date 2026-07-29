use crate::{engine::PROJECT_DIR, model::Language};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const DATABASE_FILE: &str = "content.db";
const SCHEMA_VERSION: u32 = 1;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 4 * 1024;
const MAX_CHUNK_LINES: usize = 80;
const MAX_CHUNKS_PER_FILE: usize = 512;

pub struct ContentIndex {
    root: PathBuf,
    connection: Connection,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentSyncReport {
    pub files_seen: usize,
    pub files_indexed: usize,
    pub files_changed: usize,
    pub files_deleted: usize,
    pub chunks: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContentHit {
    pub path: String,
    pub title: String,
    pub text: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f64,
}

#[derive(Debug)]
struct ContentFile {
    path: String,
    hash: String,
    format: String,
    source: String,
}

#[derive(Debug)]
struct Chunk {
    ordinal: usize,
    title: String,
    text: String,
    start_line: usize,
    end_line: usize,
}

impl ContentIndex {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize()?;
        let directory = root.join(PROJECT_DIR);
        fs::create_dir_all(&directory)?;
        let path = directory.join(DATABASE_FILE);
        reject_symlink(&path)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .with_context(|| format!("open repository content index {}", path.display()))?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        let mut index = Self { root, connection };
        index.migrate()?;
        Ok(index)
    }

    pub fn open_read_only(root: impl AsRef<Path>) -> Result<Option<Self>> {
        let root = root.as_ref().canonicalize()?;
        let path = root.join(PROJECT_DIR).join(DATABASE_FILE);
        if !path.is_file() {
            return Ok(None);
        }
        reject_symlink(&path)?;
        let connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open repository content index {}", path.display()))?;
        validate_schema(&connection)?;
        connection.pragma_update(None, "query_only", "ON")?;
        connection.pragma_update(None, "busy_timeout", 5_000)?;
        Ok(Some(Self { root, connection }))
    }

    pub fn sync(&mut self) -> Result<ContentSyncReport> {
        let existing = self.existing_hashes()?;
        let files = collect_content(&self.root)?;
        let seen = files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<HashSet<_>>();
        let deleted = existing
            .keys()
            .filter(|path| !seen.contains(path.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        let changed = files
            .iter()
            .filter(|file| existing.get(&file.path) != Some(&file.hash))
            .collect::<Vec<_>>();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for path in &deleted {
            transaction.execute("DELETE FROM content_search WHERE path=?1", [path])?;
            transaction.execute("DELETE FROM content_files WHERE path=?1", [path])?;
        }
        for file in &changed {
            transaction.execute("DELETE FROM content_search WHERE path=?1", [&file.path])?;
            transaction.execute("DELETE FROM content_files WHERE path=?1", [&file.path])?;
            transaction.execute(
                "INSERT INTO content_files(path,content_hash,format) VALUES (?1,?2,?3)",
                params![file.path, file.hash, file.format],
            )?;
            for chunk in chunks(&file.source) {
                transaction.execute(
                    "INSERT INTO content_chunks(path,ordinal,title,text,start_line,end_line)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        file.path,
                        chunk.ordinal as i64,
                        chunk.title,
                        chunk.text,
                        chunk.start_line as i64,
                        chunk.end_line as i64
                    ],
                )?;
                transaction.execute(
                    "INSERT INTO content_search(path,ordinal,title,text)
                     VALUES (?1,?2,?3,?4)",
                    params![file.path, chunk.ordinal as i64, chunk.title, chunk.text],
                )?;
            }
        }
        transaction.commit()?;
        let files_indexed =
            self.connection
                .query_row("SELECT COUNT(*) FROM content_files", [], |row| row.get(0))?;
        let chunks =
            self.connection
                .query_row("SELECT COUNT(*) FROM content_chunks", [], |row| row.get(0))?;
        Ok(ContentSyncReport {
            files_seen: files.len(),
            files_indexed,
            files_changed: changed.len(),
            files_deleted: deleted.len(),
            chunks,
        })
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<ContentHit>> {
        anyhow::ensure!(!query.trim().is_empty(), "content query must not be empty");
        anyhow::ensure!((1..=100).contains(&limit), "content limit must be 1-100");
        let query_terms = terms(query);
        if query_terms.is_empty() {
            return Ok(Vec::new());
        }
        let expression = query_terms
            .iter()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut statement = self.connection.prepare(
            "SELECT s.path,s.ordinal,s.title,s.text,c.start_line,c.end_line,
                    bm25(content_search)
             FROM content_search s
             JOIN content_chunks c ON c.path=s.path AND c.ordinal=s.ordinal
             WHERE content_search MATCH ?1
             ORDER BY bm25(content_search)
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![expression, (limit * 4) as i64], |row| {
            let path: String = row.get(0)?;
            let title: String = row.get(2)?;
            let text: String = row.get(3)?;
            let path_terms = terms(&path);
            let title_terms = terms(&title);
            let path_matches = query_terms
                .iter()
                .filter(|term| path_terms.contains(*term))
                .count();
            let title_matches = query_terms
                .iter()
                .filter(|term| title_terms.contains(*term))
                .count();
            Ok(ContentHit {
                path,
                title,
                text,
                start_line: row.get::<_, i64>(4)? as usize,
                end_line: row.get::<_, i64>(5)? as usize,
                score: -row.get::<_, f64>(6)?
                    + path_matches as f64 * 4.0
                    + title_matches as f64 * 2.0,
            })
        })?;
        let mut hits = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.start_line.cmp(&right.start_line))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn counts(&self) -> Result<(usize, usize)> {
        let files = self
            .connection
            .query_row("SELECT COUNT(*) FROM content_files", [], |row| row.get(0))?;
        let chunks =
            self.connection
                .query_row("SELECT COUNT(*) FROM content_chunks", [], |row| row.get(0))?;
        Ok((files, chunks))
    }

    fn migrate(&mut self) -> Result<()> {
        if let Some(version) = schema_version(&self.connection)? {
            anyhow::ensure!(
                version <= SCHEMA_VERSION,
                "content index schema {version} is newer than supported schema {SCHEMA_VERSION}"
            );
        }
        self.connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS metadata(
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS content_files(
               path TEXT PRIMARY KEY,
               content_hash TEXT NOT NULL,
               format TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS content_chunks(
               path TEXT NOT NULL REFERENCES content_files(path) ON DELETE CASCADE,
               ordinal INTEGER NOT NULL,
               title TEXT NOT NULL,
               text TEXT NOT NULL,
               start_line INTEGER NOT NULL,
               end_line INTEGER NOT NULL,
               PRIMARY KEY(path,ordinal)
             );
             CREATE VIRTUAL TABLE IF NOT EXISTS content_search USING fts5(
               path UNINDEXED,
               ordinal UNINDEXED,
               title,
               text,
               tokenize='unicode61 remove_diacritics 2'
             );
             INSERT INTO metadata(key,value) VALUES('schema_version','1')
               ON CONFLICT(key) DO UPDATE SET value=excluded.value;
             COMMIT;",
        )?;
        Ok(())
    }

    fn existing_hashes(&self) -> Result<HashMap<String, String>> {
        let mut statement = self
            .connection
            .prepare("SELECT path,content_hash FROM content_files")?;
        let hashes = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(hashes)
    }
}

fn collect_content(root: &Path) -> Result<Vec<ContentFile>> {
    let mut walker = WalkBuilder::new(root);
    let walker_root = root.to_owned();
    walker
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(move |entry| {
            entry
                .path()
                .strip_prefix(&walker_root)
                .ok()
                .is_none_or(|relative| {
                    !relative
                        .components()
                        .any(|part| part.as_os_str() == PROJECT_DIR)
                })
        });
    let mut files = Vec::new();
    for entry in walker.build() {
        let entry = entry?;
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry.path().strip_prefix(root)?;
        let Some(format) = content_format(relative) else {
            continue;
        };
        let Some(source) = read_text(entry.path())? else {
            continue;
        };
        files.push(ContentFile {
            path: relative.to_string_lossy().replace('\\', "/"),
            hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
            format,
            source,
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn content_format(path: &Path) -> Option<String> {
    if let Some(language) = Language::from_path(path) {
        return Some(language.to_string());
    }
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let format = match extension.as_str() {
        "md" | "mdx" | "rst" | "txt" => "document",
        "json" | "jsonc" | "toml" | "yaml" | "yml" => "config",
        "sh" | "bash" | "zsh" | "fish" | "ps1" => "script",
        _ if matches!(
            name.as_str(),
            "dockerfile" | "makefile" | "license" | "notice" | ".gitignore" | ".gitattributes"
        ) =>
        {
            "text"
        }
        _ => return None,
    };
    Some(format.to_owned())
}

fn read_text(path: &Path) -> Result<Option<String>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES || bytes.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

fn chunks(source: &str) -> Vec<Chunk> {
    let lines = source.lines().collect::<Vec<_>>();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut output = Vec::new();
    let mut start = 0usize;
    while start < lines.len() && output.len() < MAX_CHUNKS_PER_FILE {
        let mut end = start;
        let mut bytes = 0usize;
        while end < lines.len() && end - start < MAX_CHUNK_LINES {
            let next = lines[end].len() + 1;
            if end > start && bytes + next > MAX_CHUNK_BYTES {
                break;
            }
            bytes += next;
            end += 1;
        }
        if end == start {
            end += 1;
        }
        let text = lines[start..end].join("\n");
        let title = lines[start..end]
            .iter()
            .find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix('#')
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
            })
            .unwrap_or_default()
            .to_owned();
        output.push(Chunk {
            ordinal: output.len(),
            title,
            text,
            start_line: start + 1,
            end_line: end,
        });
        start = end;
    }
    output
}

fn terms(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn reject_symlink(path: &Path) -> Result<()> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        anyhow::bail!("repository content index must not be a symbolic link");
    }
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<Option<u32>> {
    let has_metadata: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='metadata')",
        [],
        |row| row.get(0),
    )?;
    if !has_metadata {
        return Ok(None);
    }
    Ok(connection
        .query_row(
            "SELECT value FROM metadata WHERE key='schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|value| value.parse())
        .transpose()?)
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let version = schema_version(connection)?.context("content index schema version is missing")?;
    anyhow::ensure!(
        version <= SCHEMA_VERSION,
        "content index schema {version} is newer than supported schema {SCHEMA_VERSION}"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_repository_content_and_removes_deleted_chunks() {
        let project = tempfile::tempdir().unwrap();
        fs::create_dir(project.path().join(PROJECT_DIR)).unwrap();
        fs::write(
            project.path().join("README.md"),
            "# Publication\n\nAtomic file publication is crash safe.\n",
        )
        .unwrap();
        fs::write(project.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname='demo'\n",
        )
        .unwrap();
        fs::write(project.path().join("image.bin"), b"hello\0world").unwrap();

        let mut index = ContentIndex::open(project.path()).unwrap();
        let first = index.sync().unwrap();
        assert_eq!(first.files_indexed, 3);
        let hits = index.search("atomic file publication", 10).unwrap();
        assert_eq!(hits[0].path, "README.md");
        assert_eq!(hits[0].start_line, 1);

        fs::remove_file(project.path().join("README.md")).unwrap();
        let second = index.sync().unwrap();
        assert_eq!(second.files_deleted, 1);
        assert!(index
            .search("atomic file publication", 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn chunking_is_bounded_and_preserves_line_ranges() {
        let source = (1..=200)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunks(&source);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 80);
        assert_eq!(chunks[1].start_line, 81);
        assert!(chunks
            .iter()
            .all(|chunk| chunk.text.len() <= MAX_CHUNK_BYTES));
    }
}
