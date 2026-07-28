use crate::{
    engine::{MAX_SOURCE_BYTES, PROJECT_DIR},
    model::Language,
    project_config::ProjectConfig,
};
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub(crate) struct ProjectDelta {
    pub(crate) changed: Vec<(String, PathBuf, Language)>,
    pub(crate) deleted: Vec<String>,
    pub(crate) files_scanned: usize,
    pub(crate) files_skipped: usize,
}

pub(crate) struct ProjectInventory<'a> {
    root: &'a Path,
    config: Arc<ProjectConfig>,
}

impl<'a> ProjectInventory<'a> {
    pub(crate) fn new(root: &'a Path) -> Result<Self> {
        Ok(Self {
            root,
            config: Arc::new(ProjectConfig::load(root)?),
        })
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

        let mut walker = WalkBuilder::new(self.root);
        let root = self.root.to_owned();
        let config = Arc::clone(&self.config);
        walker
            .hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .filter_entry(move |entry| should_descend(&root, &config, entry.path()));
        for entry in walker.build() {
            let entry = entry?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let relative_path = entry.path().strip_prefix(self.root)?;
            self.collect_file(
                entry.path(),
                relative_path,
                indexed,
                force_reindex,
                &mut seen,
                &mut changed,
                &mut files_scanned,
                &mut files_skipped,
            )?;
        }

        if self.config.has_forced_paths() {
            let mut forced = WalkBuilder::new(self.root);
            let root = self.root.to_owned();
            forced
                .standard_filters(false)
                .hidden(false)
                .filter_entry(move |entry| {
                    !is_builtin_ignored(&root, entry.path()) && !is_linked_worktree(entry.path())
                });
            for entry in forced.build() {
                let entry = entry?;
                if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                    continue;
                }
                let relative_path = entry.path().strip_prefix(self.root)?;
                let opted_in = self.config.includes(relative_path, false)
                    || self.has_opted_ignored_repo_ancestor(entry.path());
                if !opted_in {
                    continue;
                }
                self.collect_file(
                    entry.path(),
                    relative_path,
                    indexed,
                    force_reindex,
                    &mut seen,
                    &mut changed,
                    &mut files_scanned,
                    &mut files_skipped,
                )?;
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

    #[allow(clippy::too_many_arguments)]
    fn collect_file(
        &self,
        path: &Path,
        relative_path: &Path,
        indexed: &HashMap<String, String>,
        force_reindex: bool,
        seen: &mut HashSet<String>,
        changed: &mut Vec<(String, PathBuf, Language)>,
        files_scanned: &mut usize,
        files_skipped: &mut usize,
    ) -> Result<()> {
        let Some(language) = self.config.language_for_path(relative_path) else {
            return Ok(());
        };
        if self.config.excludes(relative_path, false) || is_builtin_ignored(self.root, path) {
            return Ok(());
        }
        let relative = normalize_path(relative_path);
        if !seen.insert(relative.clone()) {
            return Ok(());
        }
        if fs::metadata(path)?.len() > MAX_SOURCE_BYTES {
            *files_skipped += 1;
            return Ok(());
        }
        *files_scanned += 1;
        let source =
            fs::read_to_string(path).with_context(|| format!("read source {}", path.display()))?;
        let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
        if force_reindex || indexed.get(&relative) != Some(&hash) {
            changed.push((relative, path.to_owned(), language));
        }
        Ok(())
    }

    fn has_opted_ignored_repo_ancestor(&self, path: &Path) -> bool {
        let Ok(relative_path) = path.strip_prefix(self.root) else {
            return false;
        };
        if !self.config.includes_ignored_repo(relative_path, false) {
            return false;
        }
        let mut current = path.parent();
        while let Some(directory) = current {
            if directory == self.root {
                break;
            }
            if directory.join(".git").exists() {
                return true;
            }
            current = directory.parent();
        }
        false
    }
}

fn should_descend(root: &Path, config: &ProjectConfig, path: &Path) -> bool {
    if path == root {
        return true;
    }
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    !is_builtin_ignored(root, path)
        && !is_linked_worktree(path)
        && !config.excludes(relative, path.is_dir())
}

fn is_linked_worktree(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let marker = path.join(".git");
    if !marker.is_file() {
        return false;
    }
    let Ok(contents) = fs::read_to_string(marker) else {
        return false;
    };
    let Some(git_dir) = contents
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    let git_dir = Path::new(git_dir);
    git_dir
        .components()
        .any(|component| component.as_os_str() == "worktrees")
}

const DEFAULT_IGNORED_DIRS: &[&str] = &[
    ".git",
    PROJECT_DIR,
    "node_modules",
    "dist",
    "build",
    "out",
    "target",
    "vendor",
    "coverage",
    "__pycache__",
    ".venv",
    "venv",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".cache",
];

fn is_builtin_ignored(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().any(|component| {
            DEFAULT_IGNORED_DIRS
                .iter()
                .any(|ignored| component.as_os_str() == *ignored)
        })
    })
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
            .unwrap()
            .delta(&indexed, false)
            .unwrap();

        assert_eq!(delta.files_scanned, 1);
        assert_eq!(delta.files_skipped, 1);
        assert_eq!(delta.changed[0].0, "kept.ts");
        assert_eq!(delta.deleted, vec!["deleted.ts"]);
    }

    #[test]
    fn forced_source_and_opted_nested_repo_respect_safety_and_exclude_precedence() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::create_dir_all(root.path().join("Local")).unwrap();
        fs::create_dir_all(root.path().join("repos/child/.git")).unwrap();
        fs::create_dir_all(root.path().join("repos/other/.git")).unwrap();
        fs::create_dir_all(root.path().join("node_modules/forced")).unwrap();
        fs::write(
            root.path().join(".gitignore"),
            "Local/\nrepos/\nnode_modules/\n",
        )
        .unwrap();
        fs::write(
            root.path().join("structurely.json"),
            r#"{
                "include": ["Local/**", "node_modules/forced/**"],
                "includeIgnored": ["repos/child/**"],
                "exclude": ["Local/excluded.ts"]
            }"#,
        )
        .unwrap();
        fs::write(
            root.path().join("Local/forced.ts"),
            "export const kept = 1;",
        )
        .unwrap();
        fs::write(
            root.path().join("Local/excluded.ts"),
            "export const dropped = 1;",
        )
        .unwrap();
        fs::write(
            root.path().join("repos/child/lib.rs"),
            "pub fn included() {}",
        )
        .unwrap();
        fs::write(
            root.path().join("repos/other/lib.rs"),
            "pub fn not_selected() {}",
        )
        .unwrap();
        fs::write(
            root.path().join("node_modules/forced/index.ts"),
            "export const dependency = 1;",
        )
        .unwrap();

        let delta = ProjectInventory::new(root.path())
            .unwrap()
            .delta(&HashMap::new(), false)
            .unwrap();
        let mut paths = delta
            .changed
            .into_iter()
            .map(|(path, _, _)| path)
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths, vec!["Local/forced.ts", "repos/child/lib.rs"]);
    }

    #[test]
    fn opted_submodules_are_indexed_but_linked_worktrees_are_not_duplicated() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join(".git")).unwrap();
        fs::create_dir_all(root.path().join("nested/submodule")).unwrap();
        fs::create_dir_all(root.path().join("nested/worktree")).unwrap();
        fs::write(root.path().join(".gitignore"), "nested/\n").unwrap();
        fs::write(
            root.path().join("structurely.json"),
            r#"{"includeIgnored":["nested/**"]}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("nested/submodule/.git"),
            "gitdir: ../../.git/modules/submodule\n",
        )
        .unwrap();
        fs::write(
            root.path().join("nested/submodule/lib.rs"),
            "pub fn from_submodule() {}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("nested/worktree/.git"),
            "gitdir: ../../.git/worktrees/feature\n",
        )
        .unwrap();
        fs::write(
            root.path().join("nested/worktree/lib.rs"),
            "pub fn duplicate_worktree_view() {}\n",
        )
        .unwrap();

        let delta = ProjectInventory::new(root.path())
            .unwrap()
            .delta(&HashMap::new(), false)
            .unwrap();
        let paths = delta
            .changed
            .into_iter()
            .map(|(path, _, _)| path)
            .collect::<Vec<_>>();
        assert_eq!(paths, ["nested/submodule/lib.rs"]);
    }
}
