use crate::model::Language;
use anyhow::{bail, Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use serde_json::Value;
use std::{collections::HashMap, path::Path};

use crate::source::{read_source_snapshot, SourceRead};

const CONFIG_FILE: &str = "structurely.json";

pub(crate) struct ProjectConfig {
    extensions: HashMap<String, Language>,
    exclude: Gitignore,
    include: Gitignore,
    include_ignored: Gitignore,
    has_forced_paths: bool,
}

impl ProjectConfig {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_FILE);
        let file = path.symlink_metadata().is_ok().then_some(path);
        let value = match file {
            Some(ref path) => {
                let source = match read_source_snapshot(path)
                    .with_context(|| format!("read project config {}", path.display()))?
                {
                    SourceRead::Snapshot(source) => source,
                    SourceRead::TooLarge => {
                        anyhow::bail!(
                            "project config exceeds the bounded read limit: {}",
                            path.display()
                        )
                    }
                };
                serde_json::from_str::<Value>(&source)
                    .with_context(|| format!("parse project config {}", path.display()))?
            }
            None => Value::Null,
        };
        if !value.is_null() && !value.is_object() {
            bail!("project config root must be a JSON object");
        }

        let mut extensions = HashMap::new();
        if let Some(raw_extensions) = value.get("extensions") {
            let entries = raw_extensions
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("`extensions` must be a JSON object"))?;
            for (extension, language) in entries {
                let language_name = language.as_str().ok_or_else(|| {
                    anyhow::anyhow!("language for extension `{extension}` must be a string")
                })?;
                let language = parse_language(language_name).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unsupported language `{language_name}` for extension `{extension}`"
                    )
                })?;
                let extension = extension
                    .trim()
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                if extension.is_empty()
                    || !extension
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric())
                {
                    bail!("invalid custom extension `{extension}`");
                }
                extensions.insert(extension, language);
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
    if let Some(raw_patterns) = value.get(field) {
        let patterns = raw_patterns
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("`{field}` must be an array of glob strings"))?;
        for pattern in patterns {
            let pattern = pattern
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("`{field}` entries must be glob strings"))?
                .trim();
            if pattern.is_empty() {
                bail!("`{field}` entries must not be empty");
            }
            builder
                .add_line(source.map(Path::to_path_buf), pattern)
                .with_context(|| format!("invalid `{field}` glob `{pattern}`"))?;
            has_patterns = true;
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
        "vue" => Some(Language::Vue),
        "svelte" => Some(Language::Svelte),
        "astro" => Some(Language::Astro),
        "arkts" => Some(Language::ArkTs),
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
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn structurely_config_overrides_extensions_and_excludes_paths() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("structurely.json"),
            r#"{
                "extensions": { ".view": "typescript" },
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
    fn malformed_config_fails_closed() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("structurely.json"), "{not-json").unwrap();
        let error = ProjectConfig::load(root.path()).err().unwrap().to_string();
        assert!(error.contains("parse project config"));
    }

    #[test]
    fn invalid_config_policy_fields_fail_closed() {
        let root = tempdir().unwrap();
        for source in [
            r#"{"extensions":[]}"#,
            r#"{"extensions":{"view":"unknown"}}"#,
            r#"{"exclude":"vendor/**"}"#,
            r#"{"include":[42]}"#,
            r#"{"includeIgnored":[""]}"#,
        ] {
            fs::write(root.path().join("structurely.json"), source).unwrap();
            assert!(ProjectConfig::load(root.path()).is_err(), "{source}");
        }
    }

    #[test]
    fn astro_is_available_as_a_builtin_and_custom_extension_language() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("structurely.json"),
            r#"{"extensions": {".page": "AsTrO"}}"#,
        )
        .unwrap();
        let config = ProjectConfig::load(root.path()).unwrap();
        assert_eq!(
            config.language_for_path(Path::new("src/index.astro")),
            Some(Language::Astro)
        );
        assert_eq!(
            config.language_for_path(Path::new("src/index.page")),
            Some(Language::Astro)
        );
    }
}
