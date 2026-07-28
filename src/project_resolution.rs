use crate::model::FileFacts;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
struct AliasPattern {
    prefix: String,
    suffix: String,
    wildcard: bool,
    replacements: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectResolutionContext {
    root: PathBuf,
    base_url: PathBuf,
    aliases: Vec<AliasPattern>,
}

impl ProjectResolutionContext {
    pub(crate) fn load(root: &Path) -> Self {
        let config = ["tsconfig.json", "jsconfig.json"]
            .iter()
            .map(|name| root.join(name))
            .find(|path| path.is_file())
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|source| {
                serde_json::from_str::<serde_json::Value>(&strip_jsonc(&source)).ok()
            });
        let compiler = config
            .as_ref()
            .and_then(|value| value.get("compilerOptions"));
        let base_url = compiler
            .and_then(|value| value.get("baseUrl"))
            .and_then(serde_json::Value::as_str)
            .map(|value| root.join(value))
            .unwrap_or_else(|| root.to_owned());
        let mut aliases = Vec::new();
        if let Some(paths) = compiler
            .and_then(|value| value.get("paths"))
            .and_then(serde_json::Value::as_object)
        {
            for (pattern, targets) in paths {
                let replacements = targets
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if replacements.is_empty() {
                    continue;
                }
                let (prefix, suffix, wildcard) = split_wildcard(pattern);
                aliases.push(AliasPattern {
                    prefix,
                    suffix,
                    wildcard,
                    replacements,
                });
            }
        }
        aliases.sort_by(|left, right| {
            right
                .prefix
                .len()
                .cmp(&left.prefix.len())
                .then_with(|| left.wildcard.cmp(&right.wildcard))
        });
        Self {
            root: root.to_owned(),
            base_url,
            aliases,
        }
    }

    pub(crate) fn apply(&self, facts: &mut FileFacts) {
        for reference in &mut facts.unresolved_references {
            let Some(hint) = reference.target_file_hint.as_deref() else {
                continue;
            };
            if let Some(resolved) = self.resolve_alias(hint) {
                reference.target_file_hint = Some(resolved);
                reference.provenance = "tsconfig/path-alias".to_owned();
                reference.explanation =
                    format!("{} through project path alias", reference.explanation);
            }
        }
    }

    fn resolve_alias(&self, import_path: &str) -> Option<String> {
        for pattern in &self.aliases {
            let captured = if pattern.wildcard {
                let Some(remainder) = import_path.strip_prefix(&pattern.prefix) else {
                    continue;
                };
                let Some(captured) = remainder.strip_suffix(&pattern.suffix) else {
                    continue;
                };
                captured
            } else if import_path == pattern.prefix {
                ""
            } else {
                continue;
            };
            for replacement in &pattern.replacements {
                let replacement = if pattern.wildcard {
                    replacement.replacen('*', captured, 1)
                } else {
                    replacement.clone()
                };
                let absolute = normalize_absolute(&self.base_url.join(replacement));
                if !absolute.starts_with(&self.root) {
                    continue;
                }
                let relative = absolute.strip_prefix(&self.root).ok()?;
                if let Some(existing) = canonical_source_hint(&self.root, relative) {
                    return Some(existing);
                }
            }
            return None;
        }
        None
    }
}

fn split_wildcard(pattern: &str) -> (String, String, bool) {
    pattern.find('*').map_or_else(
        || (pattern.to_owned(), String::new(), false),
        |index| {
            (
                pattern[..index].to_owned(),
                pattern[index + 1..].to_owned(),
                true,
            )
        },
    )
}

fn normalize_absolute(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn canonical_source_hint(root: &Path, relative: &Path) -> Option<String> {
    const EXTENSIONS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "json"];
    let direct = root.join(relative);
    if direct.is_file() {
        return Some(normalize_source_hint(relative));
    }
    for extension in EXTENSIONS {
        let candidate = relative.with_extension(extension);
        if root.join(&candidate).is_file() {
            return Some(normalize_source_hint(&candidate));
        }
    }
    for extension in EXTENSIONS {
        let candidate = relative.join(format!("index.{extension}"));
        if root.join(&candidate).is_file() {
            return Some(normalize_source_hint(&candidate));
        }
    }
    None
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn normalize_source_hint(path: &Path) -> String {
    let without_extension = path.with_extension("");
    normalize_path(&without_extension)
}

fn strip_jsonc(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    let mut in_string = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            output.push(byte as char);
            if byte == b'\\' && index + 1 < bytes.len() {
                index += 1;
                output.push(bytes[index] as char);
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
        } else {
            output.push(byte as char);
            index += 1;
        }
    }
    let bytes = output.as_bytes();
    let mut cleaned = String::with_capacity(output.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b',' {
            let mut next = index + 1;
            while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                next += 1;
            }
            if matches!(bytes.get(next), Some(b'}' | b']')) {
                index += 1;
                continue;
            }
        }
        cleaned.push(bytes[index] as char);
        index += 1;
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn aliases_honor_jsonc_specificity_fallback_and_escape_protection() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/special")).unwrap();
        fs::write(root.path().join("src/fallback.ts"), "export const x = 1;").unwrap();
        fs::write(
            root.path().join("src/special/value.ts"),
            "export const value = 1;",
        )
        .unwrap();
        fs::write(
            root.path().join("tsconfig.json"),
            r#"{
              // Most-specific aliases must win.
              "compilerOptions": {
                "baseUrl": ".",
                "paths": {
                  "@special/*": ["src/special/*"],
                  "@/*": ["missing/*", "src/*"],
                  "@escape/*": ["../outside/*"],
                },
              },
            }"#,
        )
        .unwrap();
        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_alias("@special/value"),
            Some("src/special/value".to_owned())
        );
        assert_eq!(
            context.resolve_alias("@/fallback"),
            Some("src/fallback".to_owned())
        );
        assert_eq!(context.resolve_alias("@escape/secret"), None);
    }

    #[test]
    fn jsonc_stripping_preserves_comment_markers_inside_strings() {
        let stripped = strip_jsonc(
            r#"{"url":"https://example.test/a/*b*/",// comment
          "items":[1,],}"#,
        );
        let value: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(value["url"], "https://example.test/a/*b*/");
    }
}
