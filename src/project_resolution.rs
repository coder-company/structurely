use crate::model::{EventChannel, FileFacts};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
struct WorkspacePackage {
    name: String,
    root: PathBuf,
    entries: Vec<String>,
    provenance: &'static str,
    directory_entry: bool,
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
    harmony_app_roots: Vec<PathBuf>,
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
        let existing_names = workspace_packages
            .iter()
            .map(|package| package.name.clone())
            .collect::<HashSet<_>>();
        workspace_packages.extend(
            load_ohpm_packages(root)
                .into_iter()
                .filter(|package| !existing_names.contains(&package.name)),
        );
        workspace_packages.extend(load_cargo_packages(root));
        workspace_packages.extend(load_go_packages(root));
        workspace_packages.sort_by(|left, right| right.name.len().cmp(&left.name.len()));
        let harmony_app_roots = load_harmony_app_roots(root);
        Self {
            root: root.to_owned(),
            base_url,
            aliases,
            workspace_packages,
            harmony_app_roots,
        }
    }

    pub(crate) fn apply(&self, facts: &mut FileFacts) {
        let emitter_scope = facts
            .dynamic_events
            .iter()
            .any(|event| event.receiver == "ohos-emitter")
            .then(|| self.harmony_scope_for(&facts.path));
        for event in &mut facts.dynamic_events {
            if event.receiver == "ohos-emitter" {
                event.receiver =
                    format!("ohos-emitter@{}", emitter_scope.as_deref().unwrap_or("."));
            }
            if let EventChannel::Imported {
                target_file_hint, ..
            } = &mut event.channel
            {
                if let Some((resolved, _)) = self.resolve_import_from(&facts.path, target_file_hint)
                {
                    *target_file_hint = resolved;
                }
            }
        }
        for reference in &mut facts.unresolved_references {
            let Some(hint) = reference.target_file_hint.as_deref() else {
                continue;
            };
            if let Some((resolved, provenance)) = self.resolve_import_from(&facts.path, hint) {
                reference.target_file_hint = Some(resolved);
                reference.provenance = provenance.to_owned();
                reference.explanation = format!(
                    "{} through {}",
                    reference.explanation,
                    match provenance {
                        "project/relative-import" => "source-relative import resolution",
                        "tsconfig/path-alias" => "project path alias",
                        "harmony/ohpm" => "Harmony ohpm file dependency",
                        "cargo/workspace" => "Cargo workspace package",
                        _ => "JavaScript workspace package",
                    }
                );
            }
        }
        for export in &mut facts.module_exports {
            if let Some((resolved, _)) =
                self.resolve_import_from(&facts.path, &export.target_file_hint)
            {
                export.target_file_hint = resolved;
            }
        }
        for call in &mut facts.unresolved_calls {
            let Some(hint) = call.target_file_hint.as_deref() else {
                continue;
            };
            if let Some((resolved, provenance)) = self.resolve_import_from(&facts.path, hint) {
                call.target_file_hint = Some(resolved);
                if call.provenance == "tree-sitter/name-resolution" {
                    call.provenance = provenance.to_owned();
                }
                call.explanation =
                    format!("{} through project import resolution", call.explanation);
            }
        }
        for reference in facts
            .fastapi
            .aliases
            .iter_mut()
            .map(|fact| &mut fact.router)
            .chain(
                facts
                    .fastapi
                    .factories
                    .iter_mut()
                    .map(|fact| &mut fact.router),
            )
            .chain(
                facts
                    .fastapi
                    .mounts
                    .iter_mut()
                    .flat_map(|fact| [&mut fact.parent, &mut fact.child]),
            )
            .chain(facts.fastapi.routes.iter_mut().map(|fact| &mut fact.router))
            .chain(
                facts
                    .fastapi
                    .dependencies
                    .iter_mut()
                    .map(|fact| &mut fact.dependency),
            )
            .chain(
                facts
                    .fastapi
                    .dependency_aliases
                    .iter_mut()
                    .map(|fact| &mut fact.router),
            )
            .chain(
                facts
                    .fastapi
                    .dependency_factories
                    .iter_mut()
                    .map(|fact| &mut fact.router),
            )
            .chain(
                facts
                    .fastapi
                    .dependency_type_aliases
                    .iter_mut()
                    .map(|fact| &mut fact.router),
            )
        {
            let Some(hint) = reference.target_file_hint.as_deref() else {
                continue;
            };
            let python_hint = (facts.language == crate::model::Language::Python)
                .then(|| python_relative_import_hint(hint));
            let hint = python_hint.as_deref().unwrap_or(hint);
            if let Some((resolved, _)) = self.resolve_import_from(&facts.path, hint).or_else(|| {
                (facts.language == crate::model::Language::Python && !hint.starts_with('.'))
                    .then(|| canonical_source_hint(&self.root, Path::new(&hint.replace('.', "/"))))
                    .flatten()
                    .map(|resolved| (resolved, "python/project-import"))
            }) {
                reference.target_file_hint = Some(resolved);
            }
        }
    }

    fn harmony_scope_for(&self, file: &str) -> String {
        let file = Path::new(file);
        self.harmony_app_roots
            .iter()
            .filter(|root| !root.as_os_str().is_empty() && file.starts_with(root))
            .max_by_key(|root| root.components().count())
            .map(|root| root.to_string_lossy().replace('\\', "/"))
            .or_else(|| {
                file.ancestors()
                    .skip(1)
                    .find(|ancestor| {
                        self.root
                            .join(ancestor)
                            .join("AppScope/app.json5")
                            .is_file()
                    })
                    .map(normalize_path)
            })
            .unwrap_or_else(|| ".".to_owned())
    }

    fn resolve_import_from(
        &self,
        source_file: &str,
        import_path: &str,
    ) -> Option<(String, &'static str)> {
        if import_path.starts_with("./") || import_path.starts_with("../") {
            let parent = Path::new(source_file)
                .parent()
                .unwrap_or_else(|| Path::new(""));
            let relative = normalize_relative_checked(&parent.join(import_path))?;
            return canonical_source_hint(&self.root, &relative)
                .map(|path| (path, "project/relative-import"));
        }
        self.resolve_import(import_path)
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
                if package.directory_entry && self.root.join(&relative).is_dir() {
                    return Some((normalize_path(&relative), package.provenance));
                }
                if let Some(hint) = canonical_source_hint(&self.root, &relative) {
                    return Some((hint, package.provenance));
                }
                continue;
            }
            if package.directory_entry {
                return Some((normalize_path(&package.root), package.provenance));
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

fn load_harmony_app_roots(root: &Path) -> Vec<PathBuf> {
    const ENTRY_BUDGET: usize = 8_000;
    let mut app_roots = HashSet::new();
    let mut standalone_candidates = HashSet::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .max_depth(Some(8))
        .filter_entry(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some(
                    ".git"
                        | ".structurely"
                        | "node_modules"
                        | "oh_modules"
                        | "target"
                        | "dist"
                        | "build"
                )
            )
        })
        .build()
        .flatten()
        .take(ENTRY_BUDGET)
    {
        match entry.file_name().to_str() {
            Some("app.json5")
                if entry
                    .path()
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "AppScope") =>
            {
                if let Some(application) = entry.path().parent().and_then(Path::parent) {
                    if let Ok(relative) = application.strip_prefix(root) {
                        app_roots.insert(relative.to_owned());
                    }
                }
            }
            Some("build-profile.json5") => {
                if let Some(application) = entry.path().parent() {
                    standalone_candidates.insert(application.to_owned());
                }
            }
            _ => {}
        }
    }
    for candidate in standalone_candidates {
        if candidate.join("oh-package.json5").is_file() {
            if let Ok(relative) = candidate.strip_prefix(root) {
                app_roots.insert(relative.to_owned());
            }
        }
    }
    if app_roots.is_empty() {
        app_roots.insert(PathBuf::new());
    }
    let mut roots = app_roots.into_iter().collect::<Vec<_>>();
    roots.sort_by(|left, right| {
        right
            .components()
            .count()
            .cmp(&left.components().count())
            .then_with(|| left.cmp(right))
    });
    roots
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
            directory_entry: false,
        });
    }
    packages
}

fn load_ohpm_packages(root: &Path) -> Vec<WorkspacePackage> {
    const MANIFEST: &str = "oh-package.json5";
    const MAX_DEPTH: usize = 6;
    const DIRECTORY_BUDGET: usize = 8_000;
    const SKIP_DIRECTORIES: &[&str] = &[
        "node_modules",
        "oh_modules",
        ".git",
        ".structurely",
        ".codegraph",
        ".hvigor",
        ".preview",
        "build",
        "dist",
        "out",
        "target",
    ];

    let mut queue = VecDeque::from([(PathBuf::new(), 0_usize)]);
    let mut visited = 0_usize;
    let canonical_root = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    let mut targets = HashMap::<String, PathBuf>::new();
    let mut ambiguous = HashSet::<String>::new();
    while let Some((relative, depth)) = queue.pop_front() {
        visited += 1;
        if visited > DIRECTORY_BUDGET {
            break;
        }
        let absolute = root.join(&relative);
        let Ok(entries) = fs::read_dir(&absolute) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_DEPTH && !name.starts_with('.') && !SKIP_DIRECTORIES.contains(&name)
                {
                    queue.push_back((relative.join(name), depth + 1));
                }
                continue;
            }
            if name != MANIFEST {
                continue;
            }
            let Some(manifest) = read_jsonc(&entry.path()) else {
                continue;
            };
            let Some(dependencies) = manifest
                .get("dependencies")
                .and_then(serde_json::Value::as_object)
            else {
                continue;
            };
            for (dependency, value) in dependencies {
                let Some(target) = value
                    .as_str()
                    .and_then(|value| value.strip_prefix("file:"))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                else {
                    continue;
                };
                let target_absolute = normalize_absolute(&absolute.join(target));
                let Ok(canonical_target) = fs::canonicalize(&target_absolute) else {
                    continue;
                };
                if !target_absolute.starts_with(root)
                    || !canonical_target.starts_with(&canonical_root)
                    || !canonical_target.is_dir()
                {
                    continue;
                }
                let Ok(target) = target_absolute.strip_prefix(root).map(Path::to_owned) else {
                    continue;
                };
                match targets.get(dependency) {
                    None if !ambiguous.contains(dependency) => {
                        targets.insert(dependency.clone(), target);
                    }
                    Some(existing) if existing != &target => {
                        targets.remove(dependency);
                        ambiguous.insert(dependency.clone());
                    }
                    _ => {}
                }
            }
        }
    }

    let mut packages = targets
        .into_iter()
        .filter_map(|(name, package_root)| {
            let manifest = read_jsonc(&root.join(&package_root).join(MANIFEST));
            let entries = manifest
                .as_ref()
                .and_then(|value| value.get("main"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(str::to_owned)
                .into_iter()
                .collect::<Vec<_>>();
            if entries.is_empty()
                && ["src/index", "index"]
                    .iter()
                    .all(|entry| canonical_source_hint(root, &package_root.join(entry)).is_none())
            {
                return None;
            }
            Some(WorkspacePackage {
                name,
                root: package_root,
                entries,
                provenance: "harmony/ohpm",
                directory_entry: false,
            })
        })
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.name.cmp(&right.name));
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
            directory_entry: false,
        });
    }
    packages
}

fn load_go_packages(root: &Path) -> Vec<WorkspacePackage> {
    let mut member_roots = load_go_work_members(root);
    if root.join("go.mod").is_file() {
        member_roots.push(PathBuf::new());
    }
    member_roots.sort();
    member_roots.dedup();
    let mut packages = Vec::new();
    for member_root in member_roots {
        let Some(source) = fs::read_to_string(root.join(&member_root).join("go.mod")).ok() else {
            continue;
        };
        let Some(name) = source.lines().find_map(|line| {
            line.trim()
                .strip_prefix("module ")
                .map(str::trim)
                .filter(|name| !name.is_empty())
        }) else {
            continue;
        };
        packages.push(WorkspacePackage {
            name: name.to_owned(),
            root: member_root,
            entries: Vec::new(),
            provenance: "go/workspace",
            directory_entry: true,
        });
    }
    packages
}

fn load_go_work_members(root: &Path) -> Vec<PathBuf> {
    let Ok(source) = fs::read_to_string(root.join("go.work")) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    let mut in_use_block = false;
    for raw_line in source.lines() {
        let line = raw_line.split("//").next().unwrap_or_default().trim();
        if line == "use (" {
            in_use_block = true;
            continue;
        }
        if in_use_block && line == ")" {
            in_use_block = false;
            continue;
        }
        let value = if in_use_block {
            line
        } else {
            line.strip_prefix("use ").unwrap_or_default().trim()
        };
        if value.is_empty() {
            continue;
        }
        let normalized = normalize_absolute(Path::new(value.trim_matches(['`', '"'])));
        if !normalized.is_absolute()
            && !normalized
                .components()
                .any(|component| component == Component::ParentDir)
        {
            members.push(normalized);
        }
    }
    members
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|source| serde_json::from_str(&source).ok())
}

fn read_jsonc(path: &Path) -> Option<serde_json::Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|source| serde_json::from_str(&strip_jsonc(&source)).ok())
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

fn normalize_relative_checked(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn canonical_source_hint(root: &Path, relative: &Path) -> Option<String> {
    const EXTENSIONS: &[&str] = &[
        "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "vue", "svelte", "astro", "ets",
        "json", "rs", "py", "pyi",
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
    for extension in ["py", "pyi"] {
        let candidate = relative.join(format!("__init__.{extension}"));
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

fn python_relative_import_hint(module: &str) -> String {
    let dots = module.bytes().take_while(|byte| *byte == b'.').count();
    if dots == 0 {
        return module.to_owned();
    }
    let mut hint = "../".repeat(dots.saturating_sub(1));
    hint.push_str("./");
    hint.push_str(module[dots..].replace('.', "/").as_str());
    hint
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
    fn relative_import_normalization_rejects_project_root_escape() {
        assert_eq!(
            normalize_relative_checked(Path::new("src/pages/../../Card.astro")),
            Some(PathBuf::from("Card.astro"))
        );
        assert_eq!(
            normalize_relative_checked(Path::new("src/pages/../../../Card.astro")),
            None
        );
    }

    #[test]
    fn extensionless_resolution_discovers_astro_files_and_indexes() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("components/Card")).unwrap();
        fs::write(root.path().join("components/Hero.astro"), "").unwrap();
        fs::write(root.path().join("components/Card/index.astro"), "").unwrap();

        assert_eq!(
            canonical_source_hint(root.path(), Path::new("components/Hero")),
            Some("components/Hero".to_owned())
        );
        assert_eq!(
            canonical_source_hint(root.path(), Path::new("components/Card")),
            Some("components/Card/index".to_owned())
        );
    }

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
    fn relative_imports_are_canonicalized_from_the_importing_file() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("first/components")).unwrap();
        fs::create_dir_all(root.path().join("second/components")).unwrap();
        fs::write(root.path().join("first/components/main.svelte"), "").unwrap();
        fs::write(root.path().join("second/components/main.svelte"), "").unwrap();
        let context = ProjectResolutionContext::load(root.path());

        assert_eq!(
            context.resolve_import_from("first/App.svelte", "./components/main"),
            Some((
                "first/components/main".to_owned(),
                "project/relative-import"
            ))
        );
        assert_eq!(
            context.resolve_import_from("second/App.svelte", "./components/main"),
            Some((
                "second/components/main".to_owned(),
                "project/relative-import"
            ))
        );
    }

    #[test]
    fn harmony_scope_falls_back_to_the_nearest_appscope_manifest() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("apps/chat/AppScope")).unwrap();
        fs::create_dir_all(root.path().join("apps/chat/entry/src/main/ets/pages")).unwrap();
        fs::write(root.path().join("apps/chat/AppScope/app.json5"), "{}").unwrap();
        let context = ProjectResolutionContext {
            root: root.path().to_owned(),
            harmony_app_roots: vec![PathBuf::new()],
            ..ProjectResolutionContext::default()
        };
        assert_eq!(
            context.harmony_scope_for("apps/chat/entry/src/main/ets/pages/Index.ets"),
            "apps/chat"
        );
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
    fn ohpm_file_dependencies_resolve_declared_entries_and_subpaths() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("apps/entry")).unwrap();
        fs::create_dir_all(root.path().join("modules/data/src")).unwrap();
        fs::write(
            root.path().join("apps/entry/oh-package.json5"),
            r#"{
              // Registry packages remain external.
              "dependencies": {
                "data": "file:../../modules/data",
                "@ohos/axios": "^2.0.0",
                "escape": "file:../../../../outside",
              },
            }"#,
        )
        .unwrap();
        fs::write(
            root.path().join("modules/data/oh-package.json5"),
            r#"{"name":"data","main":"Index.ets","dependencies":{}}"#,
        )
        .unwrap();
        fs::write(
            root.path().join("modules/data/Index.ets"),
            "export function loadData() {}",
        )
        .unwrap();
        fs::write(
            root.path().join("modules/data/src/testing.ets"),
            "export function fixture() {}",
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_workspace_package("data"),
            Some(("modules/data/Index".to_owned(), "harmony/ohpm"))
        );
        assert_eq!(
            context.resolve_workspace_package("data/src/testing"),
            Some(("modules/data/src/testing".to_owned(), "harmony/ohpm"))
        );
        assert_eq!(context.resolve_workspace_package("@ohos/axios"), None);
        assert_eq!(context.resolve_workspace_package("escape"), None);
    }

    #[test]
    fn ohpm_ambiguous_dependency_names_are_dropped() {
        let root = tempdir().unwrap();
        for (app, module) in [("one", "first"), ("two", "second")] {
            fs::create_dir_all(root.path().join(format!("apps/{app}"))).unwrap();
            fs::create_dir_all(root.path().join(format!("modules/{module}"))).unwrap();
            fs::write(
                root.path().join(format!("apps/{app}/oh-package.json5")),
                format!(r#"{{"dependencies":{{"common":"file:../../modules/{module}"}}}}"#),
            )
            .unwrap();
            fs::write(
                root.path()
                    .join(format!("modules/{module}/oh-package.json5")),
                r#"{"name":"common","main":"Index.ets"}"#,
            )
            .unwrap();
            fs::write(
                root.path().join(format!("modules/{module}/Index.ets")),
                "export function shared() {}",
            )
            .unwrap();
        }

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(context.resolve_workspace_package("common"), None);
    }

    #[cfg(unix)]
    #[test]
    fn ohpm_file_dependencies_reject_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(root.path().join("apps/entry")).unwrap();
        fs::create_dir(root.path().join("modules")).unwrap();
        fs::write(
            root.path().join("apps/entry/oh-package.json5"),
            r#"{"dependencies":{"escape":"file:../../modules/escape"}}"#,
        )
        .unwrap();
        fs::write(
            outside.path().join("oh-package.json5"),
            r#"{"name":"escape","main":"Index.ets"}"#,
        )
        .unwrap();
        fs::write(
            outside.path().join("Index.ets"),
            "export function escaped() {}",
        )
        .unwrap();
        symlink(outside.path(), root.path().join("modules/escape")).unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(context.resolve_workspace_package("escape"), None);
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

    #[test]
    fn go_work_modules_resolve_package_and_subpackage_directories() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("modules/core/subpkg")).unwrap();
        fs::write(
            root.path().join("go.work"),
            "go 1.24\nuse (\n  ./modules/core\n)\n",
        )
        .unwrap();
        fs::write(
            root.path().join("modules/core/go.mod"),
            "module example.com/core\n\ngo 1.24\n",
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_workspace_package("example.com/core"),
            Some(("modules/core".to_owned(), "go/workspace"))
        );
        assert_eq!(
            context.resolve_workspace_package("example.com/core/subpkg"),
            Some(("modules/core/subpkg".to_owned(), "go/workspace"))
        );
    }
}
