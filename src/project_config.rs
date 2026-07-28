use crate::model::Language;
use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::Value;
use std::{collections::HashMap, fs, path::Path};

const CONFIG_FILES: [&str; 2] = ["structurely.json", "codegraph.json"];

pub(crate) struct ProjectConfig {
    extensions: HashMap<String, Language>,
    exclude: Gitignore,
    include: Gitignore,
    include_ignored: Gitignore,
    has_forced_paths: bool,
}

impl ProjectConfig {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let file = CONFIG_FILES
            .iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file());
        let value = match file {
            Some(ref path) => {
                let source = fs::read_to_string(path)
                    .with_context(|| format!("read project config {}", path.display()))?;
                serde_json::from_str::<Value>(&source).unwrap_or(Value::Null)
            }
            None => Value::Null,
        };

        let mut extensions = HashMap::new();
        if let Some(entries) = value.get("extensions").and_then(Value::as_object) {
            for (extension, language) in entries {
                let Some(language) = language.as_str().and_then(parse_language) else {
                    continue;
                };
                let extension = extension
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                if !extension.is_empty()
                    && extension
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric())
                {
                    extensions.insert(extension, language);
                }
            }
        }

        let (exclude, _) = build_matcher(root, file.as_deref(), &value, "exclude")?;
        let (include, has_include) = build_matcher(root, file.as_deref(), &value, "include")?;
        let (include_ignored, has_include_ignored) =
            build_matcher(root, file.as_deref(), &value, "includeIgnored")?;
        Ok(Self {
            extensions,
            exclude,
            include,
            include_ignored,
            has_forced_paths: has_include || has_include_ignored,
        })
    }

    pub(crate) fn language_for_path(&self, path: &Path) -> Option<Language> {
        path.extension()
            .and_then(|extension| extension.to_str())
            .and_then(|extension| self.extensions.get(&extension.to_ascii_lowercase()))
            .copied()
            .or_else(|| Language::from_path(path))
    }

    pub(crate) fn excludes(&self, relative: &Path, is_dir: bool) -> bool {
        self.exclude
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    }

    pub(crate) fn includes(&self, relative: &Path, is_dir: bool) -> bool {
        self.include
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    }

    pub(crate) fn includes_ignored_repo(&self, relative: &Path, is_dir: bool) -> bool {
        self.include_ignored
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    }

    pub(crate) fn has_forced_paths(&self) -> bool {
        self.has_forced_paths
    }
}

fn build_matcher(
    root: &Path,
    source: Option<&Path>,
    value: &Value,
    field: &str,
) -> Result<(Gitignore, bool)> {
    let mut builder = GitignoreBuilder::new(root);
    let mut has_patterns = false;
    if let Some(patterns) = value.get(field).and_then(Value::as_array) {
        for pattern in patterns.iter().filter_map(Value::as_str) {
            if !pattern.trim().is_empty()
                && builder
                    .add_line(source.map(Path::to_path_buf), pattern.trim())
                    .is_ok()
            {
                has_patterns = true;
            }
        }
    }
    Ok((builder.build()?, has_patterns))
}

fn parse_language(value: &str) -> Option<Language> {
    match value.to_ascii_lowercase().as_str() {
        "typescript" => Some(Language::TypeScript),
        "tsx" => Some(Language::Tsx),
        "javascript" => Some(Language::JavaScript),
        "jsx" => Some(Language::Jsx),
        "python" => Some(Language::Python),
        "rust" => Some(Language::Rust),
        "go" => Some(Language::Go),
        "java" => Some(Language::Java),
        "csharp" | "c#" => Some(Language::CSharp),
        "c" => Some(Language::C),
        "cpp" | "c++" => Some(Language::Cpp),
        "dart" => Some(Language::Dart),
        "ruby" => Some(Language::Ruby),
        "php" => Some(Language::Php),
        "swift" => Some(Language::Swift),
        "lua" => Some(Language::Lua),
        "kotlin" => Some(Language::Kotlin),
        "scala" => Some(Language::Scala),
        "r" => Some(Language::R),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn structurely_config_overrides_extensions_and_excludes_paths() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("structurely.json"),
            r#"{
                "extensions": { ".view": "typescript", "bad": "unknown" },
                "exclude": ["vendor/**"],
                "include": ["Local/**"],
                "includeIgnored": ["repos/child/**"]
            }"#,
        )
        .unwrap();
        let config = ProjectConfig::load(root.path()).unwrap();
        assert_eq!(
            config.language_for_path(Path::new("src/page.view")),
            Some(Language::TypeScript)
        );
        assert!(config.excludes(Path::new("vendor/generated.ts"), false));
        assert!(!config.excludes(Path::new("src/generated.ts"), false));
        assert!(config.includes(Path::new("Local/generated.ts"), false));
        assert!(config.includes_ignored_repo(Path::new("repos/child/src/lib.rs"), false));
        assert!(config.has_forced_paths());
    }

    #[test]
    fn malformed_config_degrades_to_zero_config() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("structurely.json"), "{not-json").unwrap();
        let config = ProjectConfig::load(root.path()).unwrap();
        assert_eq!(
            config.language_for_path(Path::new("main.rs")),
            Some(Language::Rust)
        );
    }
}
