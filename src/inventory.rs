use crate::{
    engine::{MAX_SOURCE_BYTES, PROJECT_DIR},
    model::Language,
};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct ProjectDelta {
    pub(crate) changed: Vec<(String, PathBuf)>,
    pub(crate) deleted: Vec<String>,
    pub(crate) files_scanned: usize,
    pub(crate) files_skipped: usize,
}

pub(crate) struct ProjectInventory<'a> {
    root: &'a Path,
}

impl<'a> ProjectInventory<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self { root }
    }

    pub(crate) fn delta(
        &self,
        indexed: &HashMap<String, String>,
        force_reindex: bool,
    ) -> Result<ProjectDelta> {
        let mut seen = HashSet::new();
        let mut changed = Vec::new();
        let mut files_scanned = 0;
        let mut files_skipped = 0;

        for entry in WalkBuilder::new(self.root)
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
            let relative_path = entry.path().strip_prefix(self.root)?;
            if Language::from_path(relative_path).is_none() {
                continue;
            }
            if entry.metadata()?.len() > MAX_SOURCE_BYTES {
                files_skipped += 1;
                continue;
            }

            let relative = normalize_path(relative_path);
            seen.insert(relative.clone());
            files_scanned += 1;
            let source = fs::read_to_string(entry.path())
                .with_context(|| format!("read source {}", entry.path().display()))?;
            let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
            if force_reindex || indexed.get(&relative) != Some(&hash) {
                changed.push((relative, entry.path().to_owned()));
            }
        }

        let deleted = indexed
            .keys()
            .filter(|path| !seen.contains(*path))
            .cloned()
            .collect();
        Ok(ProjectDelta {
            changed,
            deleted,
            files_scanned,
            files_skipped,
        })
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn delta_applies_one_policy_to_changes_deletions_ignores_and_size_limits() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::write(root.path().join(".gitignore"), "ignored.ts\n").unwrap();
        fs::write(root.path().join("kept.ts"), "export const kept = 1;\n").unwrap();
        fs::write(
            root.path().join("ignored.ts"),
            "export const ignored = 1;\n",
        )
        .unwrap();
        fs::write(
            root.path().join("large.ts"),
            vec![b'x'; MAX_SOURCE_BYTES as usize + 1],
        )
        .unwrap();

        let mut indexed = HashMap::new();
        indexed.insert("kept.ts".to_owned(), "stale".to_owned());
        indexed.insert("deleted.ts".to_owned(), "old".to_owned());
        let delta = ProjectInventory::new(root.path())
            .delta(&indexed, false)
            .unwrap();

        assert_eq!(delta.files_scanned, 1);
        assert_eq!(delta.files_skipped, 1);
        assert_eq!(delta.changed[0].0, "kept.ts");
        assert_eq!(delta.deleted, vec!["deleted.ts"]);
    }
}
