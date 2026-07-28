use crate::model::FileFacts;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
struct WorkspacePackage {
    name: String,
    root: PathBuf,
    entries: Vec<String>,
    provenance: &'static str,
}

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
    workspace_packages: Vec<WorkspacePackage>,
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
        let mut workspace_packages = load_workspace_packages(root);
        workspace_packages.extend(load_cargo_packages(root));
        workspace_packages.sort_by(|left, right| right.name.len().cmp(&left.name.len()));
        Self {
            root: root.to_owned(),
            base_url,
            aliases,
            workspace_packages,
        }
    }

    pub(crate) fn apply(&self, facts: &mut FileFacts) {
        for reference in &mut facts.unresolved_references {
            let Some(hint) = reference.target_file_hint.as_deref() else {
                continue;
            };
            if let Some((resolved, provenance)) = self.resolve_import(hint) {
                reference.target_file_hint = Some(resolved);
                reference.provenance = provenance.to_owned();
                reference.explanation = format!(
                    "{} through {}",
                    reference.explanation,
                    match provenance {
                        "tsconfig/path-alias" => "project path alias",
                        "cargo/workspace" => "Cargo workspace package",
                        _ => "JavaScript workspace package",
                    }
                );
            }
        }
    }

    fn resolve_import(&self, import_path: &str) -> Option<(String, &'static str)> {
        self.resolve_alias(import_path)
            .map(|path| (path, "tsconfig/path-alias"))
            .or_else(|| self.resolve_workspace_package(import_path))
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

    fn resolve_workspace_package(&self, import_path: &str) -> Option<(String, &'static str)> {
        for package in &self.workspace_packages {
            let subpath = if import_path == package.name {
                None
            } else {
                import_path
                    .strip_prefix(&package.name)
                    .and_then(|suffix| suffix.strip_prefix('/'))
            };
            if import_path != package.name && subpath.is_none() {
                continue;
            }
            if let Some(subpath) = subpath {
                let relative = package.root.join(subpath);
                if let Some(hint) = canonical_source_hint(&self.root, &relative) {
                    return Some((hint, package.provenance));
                }
                continue;
            }
            for entry in &package.entries {
                if let Some(hint) = canonical_source_hint(&self.root, &package.root.join(entry)) {
                    return Some((hint, package.provenance));
                }
            }
            for fallback in ["src/index", "index"] {
                if let Some(hint) = canonical_source_hint(&self.root, &package.root.join(fallback))
                {
                    return Some((hint, package.provenance));
                }
            }
        }
        None
    }
}

fn load_workspace_packages(root: &Path) -> Vec<WorkspacePackage> {
    let root_package = read_json(&root.join("package.json"));
    let mut patterns = root_package
        .as_ref()
        .and_then(|value| value.get("workspaces"))
        .map(|workspaces| {
            workspaces.as_array().or_else(|| {
                workspaces
                    .get("packages")
                    .and_then(serde_json::Value::as_array)
            })
        })
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    patterns.extend(load_pnpm_patterns(root));
    if patterns.is_empty() {
        return Vec::new();
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in &patterns {
        if let Ok(glob) = Glob::new(pattern.trim_end_matches('/')) {
            builder.add(glob);
        }
    }
    let Ok(workspaces) = builder.build() else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some(".git" | ".structurely" | "node_modules" | "target" | "dist" | "build")
            )
        })
        .build()
        .flatten()
    {
        if entry.file_name() != "package.json" || entry.path() == root.join("package.json") {
            continue;
        }
        let Some(package_root) = entry.path().parent() else {
            continue;
        };
        let Ok(relative_root) = package_root.strip_prefix(root) else {
            continue;
        };
        if !workspaces.is_match(relative_root) {
            continue;
        }
        let Some(value) = read_json(entry.path()) else {
            continue;
        };
        let Some(name) = value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let entries = ["types", "module", "main", "source"]
            .iter()
            .filter_map(|field| value.get(field).and_then(serde_json::Value::as_str))
            .map(str::to_owned)
            .collect();
        packages.push(WorkspacePackage {
            name: name.to_owned(),
            root: relative_root.to_owned(),
            entries,
            provenance: "workspace/package",
        });
    }
    packages
}

fn load_pnpm_patterns(root: &Path) -> Vec<String> {
    let Ok(source) = fs::read_to_string(root.join("pnpm-workspace.yaml")) else {
        return Vec::new();
    };
    let mut in_packages = false;
    let mut patterns = Vec::new();
    for raw_line in source.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !raw_line.chars().next().is_some_and(char::is_whitespace) {
            in_packages = line == "packages:";
            continue;
        }
        if !in_packages {
            continue;
        }
        let Some(value) = line.strip_prefix('-') else {
            continue;
        };
        let value = value.trim().trim_matches(['\'', '"']).trim_end_matches('/');
        if !value.is_empty() && !value.starts_with('!') {
            patterns.push(value.to_owned());
        }
    }
    patterns
}

fn load_cargo_packages(root: &Path) -> Vec<WorkspacePackage> {
    let Ok(source) = fs::read_to_string(root.join("Cargo.toml")) else {
        return Vec::new();
    };
    let Ok(document) = source.parse::<toml_edit::DocumentMut>() else {
        return Vec::new();
    };
    let patterns = document
        .get("workspace")
        .and_then(toml_edit::Item::as_table)
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml_edit::Item::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml_edit::Value::as_str)
        .collect::<Vec<_>>();
    if patterns.is_empty() {
        return Vec::new();
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        if let Ok(glob) = Glob::new(pattern.trim_end_matches('/')) {
            builder.add(glob);
        }
    }
    let Ok(members) = builder.build() else {
        return Vec::new();
    };
    let mut packages = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some(".git" | ".structurely" | "node_modules" | "target" | "dist" | "build")
            )
        })
        .build()
        .flatten()
    {
        if entry.file_name() != "Cargo.toml" || entry.path() == root.join("Cargo.toml") {
            continue;
        }
        let Some(package_root) = entry.path().parent() else {
            continue;
        };
        let Ok(relative_root) = package_root.strip_prefix(root) else {
            continue;
        };
        if !members.is_match(relative_root) {
            continue;
        }
        let Ok(member_source) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(member) = member_source.parse::<toml_edit::DocumentMut>() else {
            continue;
        };
        let Some(name) = member
            .get("package")
            .and_then(toml_edit::Item::as_table)
            .and_then(|package| package.get("name"))
            .and_then(toml_edit::Item::as_str)
        else {
            continue;
        };
        let entries = ["src/lib", "src/main"]
            .into_iter()
            .filter(|entry| canonical_source_hint(root, &relative_root.join(entry)).is_some())
            .map(str::to_owned)
            .collect();
        packages.push(WorkspacePackage {
            name: name.replace('-', "_"),
            root: relative_root.to_owned(),
            entries,
            provenance: "cargo/workspace",
        });
    }
    packages
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|source| serde_json::from_str(&source).ok())
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
    const EXTENSIONS: &[&str] = &[
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "json", "rs",
    ];
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

    #[test]
    fn workspace_packages_resolve_scoped_entries_and_subpaths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("packages/core/src")).unwrap();
        fs::write(
            root.path().join("package.json"),
            r#"{"workspaces":{"packages":["packages/*"]}}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("packages/core/package.json"),
            r#"{"name":"@acme/core","types":"src/index.ts"}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("packages/core/src/index.ts"),
            "export function helper() {}",
        )
        .unwrap();
        fs::write(
            root.path().join("packages/core/testing.ts"),
            "export function fixture() {}",
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_workspace_package("@acme/core"),
            Some(("packages/core/src/index".to_owned(), "workspace/package"))
        );
        assert_eq!(
            context.resolve_workspace_package("@acme/core/testing"),
            Some(("packages/core/testing".to_owned(), "workspace/package"))
        );
        assert_eq!(context.resolve_workspace_package("@acme/missing"), None);
    }

    #[test]
    fn pnpm_workspace_patterns_discover_packages_without_root_package_workspaces() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("packages/core/src")).unwrap();
        fs::write(
            root.path().join("pnpm-workspace.yaml"),
            "packages:\n  - 'packages/*'\n  - '!**/fixtures/**'\n",
        )
        .unwrap();
        fs::write(root.path().join("package.json"), r#"{"private":true}"#).unwrap();
        fs::write(
            root.path().join("packages/core/package.json"),
            r#"{"name":"@acme/core","source":"src/index.ts"}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("packages/core/src/index.ts"),
            "export function helper() {}",
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_workspace_package("@acme/core"),
            Some(("packages/core/src/index".to_owned(), "workspace/package"))
        );
    }

    #[test]
    fn cargo_workspace_crate_names_resolve_to_library_entries() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("crates/core-lib/src")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        fs::write(
            root.path().join("crates/core-lib/Cargo.toml"),
            "[package]\nname = \"core-lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("crates/core-lib/src/lib.rs"),
            "pub fn helper() {}",
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_workspace_package("core_lib"),
            Some(("crates/core-lib/src/lib".to_owned(), "cargo/workspace"))
        );
    }
}
