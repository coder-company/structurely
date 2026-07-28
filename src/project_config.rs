use crate::model::Language;
use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::Value;
use std::{collections::HashMap, fs, path::Path};

const CONFIG_FILES: [&str; 2] = ["structurely.json", "codegraph.json"];

pub(crate) struct ProjectConfig {
    extensions: HashMap<String, Language>,
    exclude: Gitignore,
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

        let mut builder = GitignoreBuilder::new(root);
        if let Some(patterns) = value.get("exclude").and_then(Value::as_array) {
            for pattern in patterns.iter().filter_map(Value::as_str) {
                if !pattern.trim().is_empty() {
                    let _ = builder.add_line(file.clone(), pattern.trim());
                }
            }
        }
        Ok(Self {
            extensions,
            exclude: builder.build()?,
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
                "exclude": ["vendor/**"]
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
