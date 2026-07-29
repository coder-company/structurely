use crate::model::{CCompilerMacroAction, CIncludeResolution, EventChannel, FileFacts};
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::Read,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CppIncludePaths {
    quote: Vec<PathBuf>,
    general: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct CppTranslationUnitContext {
    includes: CppIncludePaths,
    compiler_macros: Vec<CCompilerMacroAction>,
}

#[derive(Debug, Clone, Default)]
struct CppCompileContext {
    by_source: HashMap<String, Vec<CppTranslationUnitContext>>,
    shared_includes: Option<CppIncludePaths>,
    database_present: bool,
    fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CppIncludeResolution {
    Unmanaged,
    Resolved(String),
    Rejected,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectResolutionContext {
    root: PathBuf,
    base_url: PathBuf,
    aliases: Vec<AliasPattern>,
    workspace_packages: Vec<WorkspacePackage>,
    harmony_app_roots: Vec<PathBuf>,
    cpp: CppCompileContext,
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
        let cpp = load_cpp_compile_context(root);
        Self {
            root: root.to_owned(),
            base_url,
            aliases,
            workspace_packages,
            harmony_app_roots,
            cpp,
        }
    }

    pub(crate) fn fingerprint(&self) -> &str {
        &self.cpp.fingerprint
    }

    pub(crate) fn has_compilation_database(&self) -> bool {
        !self.cpp.by_source.is_empty()
    }

    pub(crate) fn apply(&self, facts: &mut FileFacts) {
        if matches!(
            facts.language,
            crate::model::Language::C | crate::model::Language::Cpp
        ) {
            facts.c_function_pointers.compiler_macro_contexts = self
                .cpp
                .by_source
                .get(&facts.path)
                .map(|contexts| {
                    contexts
                        .iter()
                        .map(|context| context.compiler_macros.clone())
                        .collect()
                })
                .unwrap_or_default();
            for include in &mut facts.c_function_pointers.includes {
                match self.resolve_cpp_include(&facts.path, &include.path, include.angled) {
                    CppIncludeResolution::Unmanaged => {}
                    CppIncludeResolution::Resolved(resolved) => {
                        include.path = resolved;
                        include.resolution = CIncludeResolution::Resolved;
                    }
                    CppIncludeResolution::Rejected => {
                        include.resolution = CIncludeResolution::Rejected;
                    }
                }
            }
        }
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

    fn resolve_cpp_include(
        &self,
        source_file: &str,
        include: &str,
        angled: bool,
    ) -> CppIncludeResolution {
        if !self.cpp.database_present {
            return CppIncludeResolution::Unmanaged;
        }
        if include.is_empty()
            || include.len() > 4_096
            || include.contains('\0')
            || Path::new(include).is_absolute()
        {
            return CppIncludeResolution::Rejected;
        }
        let mut roots = Vec::<PathBuf>::new();
        if !angled {
            roots.push(
                Path::new(source_file)
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_owned(),
            );
            if let Some(resolved) = self.resolve_cpp_include_from_roots(include, &roots) {
                return CppIncludeResolution::Resolved(resolved);
            }
            roots.clear();
        }
        let contexts = self
            .cpp
            .by_source
            .get(source_file)
            .map(|contexts| {
                contexts
                    .iter()
                    .map(|context| &context.includes)
                    .collect::<Vec<_>>()
            })
            .or_else(|| {
                self.cpp
                    .shared_includes
                    .as_ref()
                    .map(|includes| vec![includes])
            });
        let Some(contexts) = contexts else {
            return CppIncludeResolution::Rejected;
        };
        let mut resolved = Vec::new();
        for context in contexts {
            roots.clear();
            if !angled {
                roots.extend(context.quote.iter().cloned());
            }
            roots.extend(context.general.iter().cloned());
            let Some(candidate) = self.resolve_cpp_include_from_roots(include, &roots) else {
                return CppIncludeResolution::Rejected;
            };
            resolved.push(candidate);
        }
        resolved.sort();
        resolved.dedup();
        if resolved.len() == 1 {
            CppIncludeResolution::Resolved(resolved.remove(0))
        } else {
            CppIncludeResolution::Rejected
        }
    }

    fn resolve_cpp_include_from_roots(&self, include: &str, roots: &[PathBuf]) -> Option<String> {
        let canonical_root = fs::canonicalize(&self.root).ok()?;
        for root in roots {
            let candidate = self.root.join(root).join(include);
            let Ok(canonical) = fs::canonicalize(candidate) else {
                continue;
            };
            let Ok(relative) = canonical.strip_prefix(&canonical_root) else {
                continue;
            };
            if canonical.is_file()
                && crate::model::Language::from_path(relative).is_some_and(|language| {
                    matches!(
                        language,
                        crate::model::Language::C | crate::model::Language::Cpp
                    )
                })
            {
                return Some(normalize_path(relative));
            }
        }
        None
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

fn load_cpp_compile_context(root: &Path) -> CppCompileContext {
    const MAX_DATABASE_BYTES: u64 = 16 * 1024 * 1024;
    const MAX_ENTRIES: usize = 100_000;
    const MAX_ARGUMENTS: usize = 4_096;
    const MAX_TOKEN_BYTES: usize = 4_096;
    const MAX_INCLUDE_DIRS: usize = 8_192;
    const MAX_COMPILER_MACRO_ACTIONS: usize = 256;
    const MAX_COMPILER_MACRO_BYTES: usize = 64 * 1024;
    const CANDIDATES: &[&str] = &[
        "compile_commands.json",
        "build/compile_commands.json",
        "cmake-build-debug/compile_commands.json",
        "cmake-build-release/compile_commands.json",
        "out/compile_commands.json",
    ];

    let Some(database) = CANDIDATES
        .iter()
        .map(|candidate| root.join(candidate))
        .find(|candidate| candidate.is_file())
    else {
        return CppCompileContext {
            database_present: false,
            fingerprint: "cpp-compdb:none:v1".to_owned(),
            ..CppCompileContext::default()
        };
    };
    let Ok(canonical_root) = fs::canonicalize(root) else {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:unreadable:v1".to_owned(),
            ..CppCompileContext::default()
        };
    };
    let Ok(file) = fs::File::open(&database) else {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:unreadable:v1".to_owned(),
            ..CppCompileContext::default()
        };
    };
    let Ok(canonical_database) = fs::canonicalize(&database) else {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:unreadable:v1".to_owned(),
            ..CppCompileContext::default()
        };
    };
    if !canonical_database.starts_with(&canonical_root) {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:outside-root:v1".to_owned(),
            ..CppCompileContext::default()
        };
    }
    let opened_identity = file.try_clone().and_then(same_file::Handle::from_file);
    let path_identity = same_file::Handle::from_path(&canonical_database);
    if !matches!((opened_identity, path_identity), (Ok(opened), Ok(path)) if opened == path) {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:changed-during-open:v1".to_owned(),
            ..CppCompileContext::default()
        };
    }
    let Ok(metadata) = file.metadata() else {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:unreadable:v1".to_owned(),
            ..CppCompileContext::default()
        };
    };
    if metadata.len() > MAX_DATABASE_BYTES {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_nanos());
        return CppCompileContext {
            database_present: true,
            fingerprint: format!("cpp-compdb:oversize:{}:{modified}:v1", metadata.len()),
            ..CppCompileContext::default()
        };
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let Ok(_) = file.take(MAX_DATABASE_BYTES + 1).read_to_end(&mut bytes) else {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:unreadable:v1".to_owned(),
            ..CppCompileContext::default()
        };
    };
    if bytes.len() as u64 > MAX_DATABASE_BYTES
        || fs::canonicalize(&canonical_database)
            .ok()
            .filter(|current| {
                current == &canonical_database && current.starts_with(&canonical_root)
            })
            .is_none()
    {
        return CppCompileContext {
            database_present: true,
            fingerprint: "cpp-compdb:changed-during-read:v1".to_owned(),
            ..CppCompileContext::default()
        };
    }
    let database_fingerprint = format!(
        "cpp-compdb:{}:{}:v1",
        database
            .strip_prefix(root)
            .map(normalize_path)
            .unwrap_or_else(|_| database.display().to_string()),
        blake3::hash(&bytes).to_hex()
    );
    let Ok(entries) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes) else {
        return CppCompileContext {
            database_present: true,
            fingerprint: database_fingerprint,
            ..CppCompileContext::default()
        };
    };
    if entries.len() > MAX_ENTRIES {
        return CppCompileContext {
            database_present: true,
            fingerprint: format!("{database_fingerprint}:entry-overflow:v1"),
            ..CppCompileContext::default()
        };
    }
    let mut by_source = HashMap::<String, Vec<CppTranslationUnitContext>>::new();
    let mut response_fingerprints = Vec::<String>::new();
    let mut rejected_sources = HashSet::<String>::new();
    for entry in entries {
        let directory = entry
            .get("directory")
            .and_then(serde_json::Value::as_str)
            .filter(|directory| directory.len() <= MAX_TOKEN_BYTES)
            .map(PathBuf::from)
            .map(|directory| {
                if directory.is_absolute() {
                    directory
                } else {
                    root.join(directory)
                }
            })
            .unwrap_or_else(|| root.to_owned());
        let source = entry
            .get("file")
            .and_then(serde_json::Value::as_str)
            .filter(|file| !file.is_empty() && file.len() <= MAX_TOKEN_BYTES)
            .and_then(|file| {
                let file = Path::new(file);
                let absolute = if file.is_absolute() {
                    file.to_owned()
                } else {
                    directory.join(file)
                };
                fs::canonicalize(absolute).ok()
            })
            .filter(|source| source.is_file())
            .and_then(|source| {
                source
                    .strip_prefix(&canonical_root)
                    .ok()
                    .map(normalize_path)
            });
        let Some(source) = source else {
            continue;
        };
        let Some(arguments) = compilation_entry_arguments(&entry, MAX_ARGUMENTS, MAX_TOKEN_BYTES)
        else {
            rejected_sources.insert(source);
            continue;
        };
        let (arguments, entry_response_fingerprints) = expand_compiler_response_arguments(
            &arguments,
            &directory,
            &canonical_root,
            MAX_ARGUMENTS,
            MAX_TOKEN_BYTES,
        );
        response_fingerprints.extend(entry_response_fingerprints);
        let Some(arguments) = arguments else {
            rejected_sources.insert(source);
            continue;
        };
        let mut quote = Vec::new();
        let mut ordinary = Vec::new();
        let mut system = Vec::new();
        let mut after = Vec::new();
        let mut compiler_macros = Vec::new();
        let mut compiler_macro_bytes = 0usize;
        let mut context_valid = true;
        let compiler_index = compiler_driver(&arguments).map_or(0, |(_, index)| index);
        let allow_slash_macros = is_msvc_compiler_driver(&arguments);
        let mut options_enabled = true;
        let mut index = compiler_index.saturating_add(1);
        while index < arguments.len() {
            let argument = &arguments[index];
            if !options_enabled {
                index += 1;
                continue;
            }
            if argument == "--" {
                options_enabled = false;
                index += 1;
                continue;
            }
            let (bucket, attached) = if argument == "-iquote" {
                (Some(&mut quote), None)
            } else if let Some(value) = argument.strip_prefix("-iquote") {
                (Some(&mut quote), (!value.is_empty()).then_some(value))
            } else if argument == "-isystem" {
                (Some(&mut system), None)
            } else if let Some(value) = argument.strip_prefix("-isystem") {
                (Some(&mut system), (!value.is_empty()).then_some(value))
            } else if argument == "-idirafter" {
                (Some(&mut after), None)
            } else if let Some(value) = argument.strip_prefix("-idirafter") {
                (Some(&mut after), (!value.is_empty()).then_some(value))
            } else if argument == "-I" || argument == "/I" {
                (Some(&mut ordinary), None)
            } else if let Some(value) = argument
                .strip_prefix("-I")
                .or_else(|| argument.strip_prefix("/I"))
            {
                (Some(&mut ordinary), (!value.is_empty()).then_some(value))
            } else if is_compiler_macro_option(argument, allow_slash_macros) {
                if let Some((action, consumed_next)) =
                    parse_compiler_macro_action(&arguments, index, allow_slash_macros)
                {
                    compiler_macro_bytes =
                        compiler_macro_bytes.saturating_add(c_compiler_macro_action_bytes(&action));
                    compiler_macros.push(action);
                    if compiler_macros.len() > MAX_COMPILER_MACRO_ACTIONS
                        || compiler_macro_bytes > MAX_COMPILER_MACRO_BYTES
                    {
                        context_valid = false;
                    }
                    if consumed_next {
                        index += 1;
                    }
                } else {
                    context_valid = false;
                }
                (None, None)
            } else {
                (None, None)
            };
            if let Some(bucket) = bucket {
                let value = attached.or_else(|| {
                    let next = arguments.get(index + 1)?;
                    if next.starts_with('-') || (next.starts_with("/I") && next.len() > 2) {
                        return None;
                    }
                    index += 1;
                    Some(next.as_str())
                });
                if let Some(value) =
                    value.filter(|value| !value.is_empty() && value.len() <= MAX_TOKEN_BYTES)
                {
                    let candidate = Path::new(value);
                    let absolute = if candidate.is_absolute() {
                        candidate.to_owned()
                    } else {
                        directory.join(candidate)
                    };
                    if let Ok(canonical) = fs::canonicalize(absolute) {
                        if canonical.is_dir() {
                            if let Ok(relative) = canonical.strip_prefix(&canonical_root) {
                                let relative = relative.to_owned();
                                if !bucket.contains(&relative) {
                                    bucket.push(relative);
                                }
                            }
                        }
                    }
                }
            }
            index += 1;
        }
        if !context_valid {
            rejected_sources.insert(source);
            continue;
        }
        let mut general = ordinary;
        general.extend(system);
        general.extend(after);
        quote.truncate(MAX_INCLUDE_DIRS);
        general.truncate(MAX_INCLUDE_DIRS);
        by_source
            .entry(source)
            .or_default()
            .push(CppTranslationUnitContext {
                includes: CppIncludePaths { quote, general },
                compiler_macros,
            });
    }
    for contexts in by_source.values_mut() {
        for context in contexts.iter_mut() {
            let paths = &mut context.includes;
            let mut quote_seen = HashSet::new();
            paths
                .quote
                .retain(|directory| quote_seen.insert(directory.clone()));
            paths.quote.truncate(MAX_INCLUDE_DIRS);
            let mut general_seen = HashSet::new();
            paths
                .general
                .retain(|directory| general_seen.insert(directory.clone()));
            paths.general.truncate(MAX_INCLUDE_DIRS);
        }
        contexts.sort_by(|left, right| {
            left.includes
                .quote
                .cmp(&right.includes.quote)
                .then_with(|| left.includes.general.cmp(&right.includes.general))
                .then_with(|| left.compiler_macros.cmp(&right.compiler_macros))
        });
        contexts.dedup();
    }
    for source in &rejected_sources {
        by_source.remove(source);
    }
    let mut context_overflow = false;
    by_source.retain(|_, contexts| {
        let accepted = contexts.len() <= 32;
        context_overflow |= !accepted;
        accepted
    });
    let mut all_includes = by_source
        .values()
        .flatten()
        .map(|context| &context.includes);
    let shared_includes = (rejected_sources.is_empty() && !context_overflow)
        .then(|| {
            all_includes
                .next()
                .cloned()
                .filter(|first| all_includes.all(|candidate| candidate == first))
        })
        .flatten();
    let fingerprint =
        cpp_context_fingerprint(&database_fingerprint, &by_source, &response_fingerprints);
    CppCompileContext {
        by_source,
        shared_includes,
        database_present: true,
        fingerprint,
    }
}

fn cpp_context_fingerprint(
    database_fingerprint: &str,
    by_source: &HashMap<String, Vec<CppTranslationUnitContext>>,
    response_fingerprints: &[String],
) -> String {
    let mut sources = by_source.iter().collect::<Vec<_>>();
    sources.sort_by(|left, right| left.0.cmp(right.0));
    let mut hasher = blake3::Hasher::new();
    hasher.update(database_fingerprint.as_bytes());
    for (source, contexts) in sources {
        hasher.update(&[0]);
        hasher.update(source.as_bytes());
        for context in contexts {
            hasher.update(&[1]);
            for directory in &context.includes.quote {
                hasher.update(&[2]);
                hasher.update(normalize_path(directory).as_bytes());
            }
            for directory in &context.includes.general {
                hasher.update(&[3]);
                hasher.update(normalize_path(directory).as_bytes());
            }
            for action in &context.compiler_macros {
                hasher.update(&[4]);
                hasher.update(format!("{action:?}").as_bytes());
            }
        }
    }
    for response in response_fingerprints {
        hasher.update(&[5]);
        hasher.update(response.as_bytes());
    }
    format!("cpp-context:{}:v2", hasher.finalize().to_hex())
}

fn compilation_entry_arguments(
    entry: &serde_json::Value,
    max_arguments: usize,
    max_token_bytes: usize,
) -> Option<Vec<String>> {
    let arguments =
        if let Some(arguments) = entry.get("arguments").and_then(serde_json::Value::as_array) {
            if arguments.len() > max_arguments {
                return None;
            }
            arguments
                .iter()
                .map(serde_json::Value::as_str)
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        } else {
            let command = entry.get("command")?.as_str()?;
            if command.len() > max_arguments.saturating_mul(max_token_bytes) {
                return None;
            }
            split_compiler_command(command)?
        };
    (arguments.len() <= max_arguments
        && arguments
            .iter()
            .all(|argument| argument.len() <= max_token_bytes))
    .then_some(arguments)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompilerResponseDialect {
    Gnu,
    Unsupported,
}

fn compiler_response_dialect(arguments: &[String]) -> CompilerResponseDialect {
    let driver = compiler_driver_name(arguments).unwrap_or_default();
    if matches!(
        driver.as_str(),
        "cc" | "c++"
            | "gcc"
            | "g++"
            | "clang"
            | "clang++"
            | "gcc.exe"
            | "g++.exe"
            | "clang.exe"
            | "clang++.exe"
    ) {
        CompilerResponseDialect::Gnu
    } else {
        CompilerResponseDialect::Unsupported
    }
}

struct CompilerResponseExpansion {
    arguments: Vec<String>,
    fingerprints: Vec<String>,
    active: HashSet<PathBuf>,
    files: usize,
    bytes: u64,
    valid: bool,
}

fn expand_compiler_response_arguments(
    arguments: &[String],
    directory: &Path,
    canonical_root: &Path,
    max_arguments: usize,
    max_token_bytes: usize,
) -> (Option<Vec<String>>, Vec<String>) {
    let mut expansion = CompilerResponseExpansion {
        arguments: Vec::new(),
        fingerprints: Vec::new(),
        active: HashSet::new(),
        files: 0,
        bytes: 0,
        valid: true,
    };
    let dialect = compiler_response_dialect(arguments);
    expand_compiler_response_tokens(
        arguments,
        directory,
        canonical_root,
        dialect,
        0,
        max_arguments,
        max_token_bytes,
        &mut expansion,
    );
    let arguments = (expansion.valid && expansion.arguments.len() <= max_arguments)
        .then_some(expansion.arguments);
    (arguments, expansion.fingerprints)
}

#[allow(clippy::too_many_arguments)]
fn expand_compiler_response_tokens(
    tokens: &[String],
    directory: &Path,
    canonical_root: &Path,
    dialect: CompilerResponseDialect,
    depth: usize,
    max_arguments: usize,
    max_token_bytes: usize,
    expansion: &mut CompilerResponseExpansion,
) {
    const MAX_RESPONSE_DEPTH: usize = 8;
    const MAX_RESPONSE_FILES: usize = 32;
    const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
    if !expansion.valid {
        return;
    }
    for token in tokens {
        let Some(response) = token.strip_prefix('@').filter(|path| !path.is_empty()) else {
            if token.len() > max_token_bytes || expansion.arguments.len() >= max_arguments {
                expansion.valid = false;
                return;
            }
            expansion.arguments.push(token.clone());
            continue;
        };
        if dialect != CompilerResponseDialect::Gnu
            || depth >= MAX_RESPONSE_DEPTH
            || expansion.files >= MAX_RESPONSE_FILES
        {
            expansion
                .fingerprints
                .push(format!("rejected:{response}:dialect-or-cap"));
            expansion.valid = false;
            return;
        }
        let candidate = directory.join(response);
        let Some((canonical, bytes)) = read_project_response_file(
            &candidate,
            canonical_root,
            MAX_RESPONSE_BYTES.saturating_sub(expansion.bytes),
        ) else {
            expansion
                .fingerprints
                .push(format!("rejected:{response}:unreadable"));
            expansion.valid = false;
            return;
        };
        let relative = canonical
            .strip_prefix(canonical_root)
            .map(normalize_path)
            .unwrap_or_else(|_| canonical.display().to_string());
        expansion
            .fingerprints
            .push(format!("{}:{}", relative, blake3::hash(&bytes).to_hex()));
        if !expansion.active.insert(canonical.clone()) {
            expansion.fingerprints.push(format!("cycle:{relative}"));
            expansion.valid = false;
            return;
        }
        expansion.files += 1;
        expansion.bytes = expansion.bytes.saturating_add(bytes.len() as u64);
        let Some(source) = std::str::from_utf8(&bytes).ok() else {
            expansion.valid = false;
            return;
        };
        let Some(nested) = split_gnu_response_file(source) else {
            expansion.valid = false;
            return;
        };
        if nested.len() > max_arguments
            || nested
                .iter()
                .any(|argument| argument.len() > max_token_bytes)
        {
            expansion.valid = false;
            return;
        }
        expand_compiler_response_tokens(
            &nested,
            directory,
            canonical_root,
            dialect,
            depth + 1,
            max_arguments,
            max_token_bytes,
            expansion,
        );
        expansion.active.remove(&canonical);
        if !expansion.valid {
            return;
        }
    }
}

fn read_project_response_file(
    candidate: &Path,
    canonical_root: &Path,
    max_bytes: u64,
) -> Option<(PathBuf, Vec<u8>)> {
    let mut file = fs::File::open(candidate).ok()?;
    let canonical = fs::canonicalize(candidate).ok()?;
    if !canonical.starts_with(canonical_root) {
        return None;
    }
    let opened = file
        .try_clone()
        .and_then(same_file::Handle::from_file)
        .ok()?;
    let path_identity = same_file::Handle::from_path(&canonical).ok()?;
    if opened != path_identity {
        return None;
    }
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return None;
    }
    let modified = metadata.modified().ok()?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    let after_metadata = file.metadata().ok()?;
    if bytes.len() as u64 > max_bytes
        || after_metadata.len() != metadata.len()
        || after_metadata.modified().ok()? != modified
        || fs::canonicalize(candidate).ok().as_ref() != Some(&canonical)
        || same_file::Handle::from_path(candidate).ok()? != path_identity
    {
        return None;
    }
    let mut verification_file = fs::File::open(candidate).ok()?;
    if same_file::Handle::from_file(verification_file.try_clone().ok()?).ok()? != path_identity {
        return None;
    }
    let verification_metadata = verification_file.metadata().ok()?;
    if verification_metadata.len() != metadata.len()
        || verification_metadata.modified().ok()? != modified
    {
        return None;
    }
    let mut verification = Vec::with_capacity(bytes.len());
    (&mut verification_file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut verification)
        .ok()?;
    if verification != bytes
        || verification_file.metadata().ok()?.len() != metadata.len()
        || verification_file.metadata().ok()?.modified().ok()? != modified
    {
        return None;
    }
    Some((canonical, bytes))
}

fn parse_compiler_macro_action(
    arguments: &[String],
    index: usize,
    allow_slash: bool,
) -> Option<(CCompilerMacroAction, bool)> {
    let argument = arguments.get(index)?;
    let (kind, attached) = if argument == "-D" || (allow_slash && argument == "/D") {
        ('D', None)
    } else if argument == "-U" || (allow_slash && argument == "/U") {
        ('U', None)
    } else if let Some(value) = argument
        .strip_prefix("-D")
        .or_else(|| allow_slash.then(|| argument.strip_prefix("/D")).flatten())
    {
        ('D', Some(value))
    } else if let Some(value) = argument
        .strip_prefix("-U")
        .or_else(|| allow_slash.then(|| argument.strip_prefix("/U")).flatten())
    {
        ('U', Some(value))
    } else {
        return None;
    };
    let (value, consumed_next) = if let Some(value) = attached.filter(|value| !value.is_empty()) {
        (value, false)
    } else {
        (arguments.get(index + 1)?.as_str(), true)
    };
    let action = if kind == 'U' {
        is_cpp_macro_identifier(value).then(|| CCompilerMacroAction::Undef {
            name: value.to_owned(),
        })?
    } else {
        parse_compiler_define(value)?
    };
    Some((action, consumed_next))
}

fn is_compiler_macro_option(argument: &str, allow_slash: bool) -> bool {
    matches!(argument, "-D" | "-U")
        || argument.starts_with("-D")
        || argument.starts_with("-U")
        || (allow_slash
            && (matches!(argument, "/D" | "/U")
                || argument.starts_with("/D")
                || argument.starts_with("/U")))
}

fn is_msvc_compiler_driver(arguments: &[String]) -> bool {
    matches!(
        compiler_driver_name(arguments).as_deref(),
        Some("cl" | "cl.exe" | "clang-cl" | "clang-cl.exe")
    )
}

fn compiler_driver_name(arguments: &[String]) -> Option<String> {
    compiler_driver(arguments).map(|(name, _)| name)
}

fn compiler_driver(arguments: &[String]) -> Option<(String, usize)> {
    const DRIVERS: &[&str] = &[
        "cc",
        "c++",
        "gcc",
        "g++",
        "clang",
        "clang++",
        "gcc.exe",
        "g++.exe",
        "clang.exe",
        "clang++.exe",
        "cl",
        "cl.exe",
        "clang-cl",
        "clang-cl.exe",
    ];
    const LAUNCHERS: &[&str] = &["ccache", "sccache", "distcc", "icecc"];
    const MAX_LAUNCHER_DEPTH: usize = 4;

    fn basename(argument: &str) -> Option<String> {
        Path::new(argument)
            .file_name()?
            .to_str()
            .map(str::to_ascii_lowercase)
    }

    fn is_environment_assignment(argument: &str) -> bool {
        let Some((name, _)) = argument.split_once('=') else {
            return false;
        };
        let mut characters = name.chars();
        matches!(characters.next(), Some(first) if first == '_' || first.is_ascii_alphabetic())
            && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
    }

    fn resolve(
        arguments: &[String],
        index: usize,
        depth: usize,
        drivers: &[&str],
        launchers: &[&str],
    ) -> Option<(String, usize)> {
        if depth > MAX_LAUNCHER_DEPTH {
            return None;
        }
        let name = basename(arguments.get(index)?)?;
        if drivers.contains(&name.as_str()) {
            return Some((name, index));
        }
        if launchers.contains(&name.as_str()) {
            return resolve(arguments, index + 1, depth + 1, drivers, launchers);
        }
        if name != "env" {
            return None;
        }

        let mut command_index = index + 1;
        while let Some(argument) = arguments.get(command_index) {
            if argument == "--" {
                command_index += 1;
                break;
            }
            if is_environment_assignment(argument)
                || matches!(
                    argument.as_str(),
                    "-i" | "--ignore-environment" | "-0" | "--null"
                )
                || argument.starts_with("--unset=")
                || argument.starts_with("--chdir=")
            {
                command_index += 1;
                continue;
            }
            if matches!(argument.as_str(), "-u" | "--unset" | "-C" | "--chdir") {
                arguments.get(command_index + 1)?;
                command_index += 2;
                continue;
            }
            if argument.starts_with('-') {
                return None;
            }
            break;
        }
        resolve(arguments, command_index, depth + 1, drivers, launchers)
    }

    resolve(arguments, 0, 0, DRIVERS, LAUNCHERS)
}

fn parse_compiler_define(value: &str) -> Option<CCompilerMacroAction> {
    const MAX_NAME_BYTES: usize = 256;
    const MAX_PARAMETERS: usize = 64;
    const MAX_REPLACEMENT_BYTES: usize = 8 * 1024;
    let (declaration, replacement) = value.split_once('=').unwrap_or((value, "1"));
    if declaration.len() > MAX_NAME_BYTES + 1 + MAX_PARAMETERS * (MAX_NAME_BYTES + 1)
        || replacement.len() > MAX_REPLACEMENT_BYTES
        || replacement.contains("##")
        || replacement.contains('#')
    {
        return None;
    }
    let (name, parameters) = if let Some(open) = declaration.find('(') {
        if !declaration.ends_with(')') {
            return None;
        }
        let name = &declaration[..open];
        let raw_parameters = &declaration[open + 1..declaration.len() - 1];
        let parameters = if raw_parameters.trim().is_empty() {
            Vec::new()
        } else {
            raw_parameters
                .split(',')
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        if parameters.len() > MAX_PARAMETERS
            || parameters
                .iter()
                .any(|parameter| !is_cpp_macro_identifier(parameter))
            || parameters.iter().collect::<HashSet<_>>().len() != parameters.len()
        {
            return None;
        }
        (name, Some(parameters))
    } else {
        (declaration, None)
    };
    if name.len() > MAX_NAME_BYTES || !is_cpp_macro_identifier(name) {
        return None;
    }
    Some(CCompilerMacroAction::Define {
        name: name.to_owned(),
        parameters,
        replacement: replacement.to_owned(),
    })
}

fn is_cpp_macro_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn c_compiler_macro_action_bytes(action: &CCompilerMacroAction) -> usize {
    match action {
        CCompilerMacroAction::Define {
            name,
            parameters,
            replacement,
        } => name.len().saturating_add(replacement.len()).saturating_add(
            parameters
                .iter()
                .flatten()
                .map(String::len)
                .fold(0usize, usize::saturating_add),
        ),
        CCompilerMacroAction::Undef { name } => name.len(),
    }
}

fn split_gnu_response_file(source: &str) -> Option<Vec<String>> {
    let mut output = Vec::new();
    let mut token = String::new();
    let mut quote = None::<char>;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                token.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !token.is_empty() {
                output.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if escaped {
        return None;
    }
    if quote.is_some() {
        return None;
    }
    if !token.is_empty() {
        output.push(token);
    }
    Some(output)
}

fn split_compiler_command(command: &str) -> Option<Vec<String>> {
    let mut output = Vec::new();
    let mut token = String::new();
    let mut quote = None::<char>;
    let mut escaped = false;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            let escapes = characters.peek().is_some_and(|next| {
                if quote == Some('"') {
                    matches!(next, '$' | '`' | '"' | '\\' | '\n')
                } else {
                    next.is_whitespace() || matches!(next, '\'' | '"' | '\\')
                }
            });
            if escapes {
                escaped = true;
                continue;
            }
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                token.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character.is_whitespace() {
            if !token.is_empty() {
                output.push(std::mem::take(&mut token));
            }
        } else {
            token.push(character);
        }
    }
    if escaped {
        token.push('\\');
    }
    if quote.is_some() {
        return None;
    }
    if !token.is_empty() {
        output.push(token);
    }
    Some(output)
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
    fn compilation_database_resolves_per_tu_angle_and_quote_includes() {
        let root = tempdir().unwrap();
        for directory in [
            "src",
            "quote dir",
            "include-empty",
            "include-a",
            "include-b",
        ] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        fs::write(root.path().join("src/main.c"), "#include <config.h>\n").unwrap();
        fs::write(root.path().join("src/other.c"), "#include <config.h>\n").unwrap();
        fs::write(root.path().join("src/config.h"), "/* local */").unwrap();
        fs::write(root.path().join("quote dir/config.h"), "/* quote */").unwrap();
        fs::write(root.path().join("include-a/config.h"), "/* a */").unwrap();
        fs::write(root.path().join("include-b/config.h"), "/* b */").unwrap();
        fs::write(
            root.path().join("compile_commands.json"),
            serde_json::to_string(&serde_json::json!([
                {
                    "directory": root.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "-iquote", "quote dir", "-I", "-Iinclude-empty", "-Iinclude-a", "src/main.c"],
                    "command": "cc -Iinclude-b src/main.c"
                },
                {
                    "directory": root.path(),
                    "file": "src/other.c",
                    "command": "cc '-Iinclude-b' src/other.c"
                }
            ]))
            .unwrap(),
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_cpp_include("src/main.c", "config.h", false),
            CppIncludeResolution::Resolved("src/config.h".to_owned())
        );
        assert_eq!(
            context.resolve_cpp_include("src/main.c", "config.h", true),
            CppIncludeResolution::Resolved("include-a/config.h".to_owned())
        );
        assert_eq!(
            context.resolve_cpp_include("src/other.c", "config.h", true),
            CppIncludeResolution::Resolved("include-b/config.h".to_owned())
        );
        assert!(context.fingerprint().starts_with("cpp-context:"));
    }

    #[test]
    fn compilation_database_rejects_outside_roots_and_unclosed_commands() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/main.c"), "#include <escape.h>\n").unwrap();
        fs::write(outside.path().join("escape.h"), "/* outside */").unwrap();
        fs::write(
            root.path().join("compile_commands.json"),
            serde_json::to_string(&serde_json::json!([
                {
                    "directory": root.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "-I", outside.path(), "src/main.c"]
                }
            ]))
            .unwrap(),
        )
        .unwrap();
        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_cpp_include("src/main.c", "escape.h", true),
            CppIncludeResolution::Rejected
        );
        assert_eq!(split_compiler_command("cc -I'unclosed"), None);
        assert_eq!(
            split_compiler_command("cc \"-Ispace dir\" main.c"),
            Some(vec![
                "cc".to_owned(),
                "-Ispace dir".to_owned(),
                "main.c".to_owned()
            ])
        );
        assert_eq!(
            split_compiler_command(r"cc -Ispace\ dir C:\sdk\main.c"),
            Some(vec![
                "cc".to_owned(),
                "-Ispace dir".to_owned(),
                r"C:\sdk\main.c".to_owned()
            ])
        );
        assert_eq!(
            split_compiler_command(r#"cc "/IC:\Program Files\sdk" C:\src\main.c"#),
            Some(vec![
                "cc".to_owned(),
                r"/IC:\Program Files\sdk".to_owned(),
                r"C:\src\main.c".to_owned()
            ])
        );
    }

    #[test]
    fn compilation_database_variants_and_header_contexts_fail_closed() {
        let root = tempdir().unwrap();
        for directory in ["src", "include-a", "include-b"] {
            fs::create_dir_all(root.path().join(directory)).unwrap();
        }
        for file in ["src/main.c", "src/other.c", "src/shared.h"] {
            fs::write(root.path().join(file), "#include <config.h>\n").unwrap();
        }
        fs::write(root.path().join("include-a/config.h"), "/* a */").unwrap();
        fs::write(root.path().join("include-b/config.h"), "/* b */").unwrap();
        fs::write(
            root.path().join("compile_commands.json"),
            serde_json::to_string(&serde_json::json!([
                {
                    "directory": root.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "-Iinclude-a", "src/main.c"]
                },
                {
                    "directory": root.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "-Iinclude-b", "src/main.c"]
                },
                {
                    "directory": root.path(),
                    "file": "src/other.c",
                    "arguments": ["cc", "-Iinclude-b", "src/other.c"]
                }
            ]))
            .unwrap(),
        )
        .unwrap();
        let context = ProjectResolutionContext::load(root.path());
        assert_eq!(
            context.resolve_cpp_include("src/main.c", "config.h", true),
            CppIncludeResolution::Rejected
        );
        assert_eq!(
            context.resolve_cpp_include("src/shared.h", "config.h", true),
            CppIncludeResolution::Rejected
        );
    }

    #[test]
    fn compilation_database_preserves_ordered_macro_variants_and_response_fingerprints() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        for file in ["src/main.c", "src/shared.h"] {
            fs::write(root.path().join(file), "int value;\n").unwrap();
        }
        fs::write(
            root.path().join("flags.rsp"),
            "-DWRAP(x)=x -DSELECT=second\n",
        )
        .unwrap();
        let write_database = || {
            fs::write(
                root.path().join("compile_commands.json"),
                serde_json::to_vec(&serde_json::json!([
                    {
                        "directory": root.path(),
                        "file": "src/main.c",
                        "arguments": [
                            "cc",
                            "-DSELECT=first",
                            "-USELECT",
                            "-DSELECT=final",
                            "src/main.c"
                        ]
                    },
                    {
                        "directory": root.path(),
                        "file": "src/main.c",
                        "arguments": ["cc", "@flags.rsp", "src/main.c"]
                    }
                ]))
                .unwrap(),
            )
            .unwrap();
        };
        write_database();

        let first = ProjectResolutionContext::load(root.path());
        let contexts = &first.cpp.by_source["src/main.c"];
        assert_eq!(contexts.len(), 2);
        assert!(contexts.iter().any(|context| {
            context.compiler_macros
                == [
                    CCompilerMacroAction::Define {
                        name: "SELECT".to_owned(),
                        parameters: None,
                        replacement: "first".to_owned(),
                    },
                    CCompilerMacroAction::Undef {
                        name: "SELECT".to_owned(),
                    },
                    CCompilerMacroAction::Define {
                        name: "SELECT".to_owned(),
                        parameters: None,
                        replacement: "final".to_owned(),
                    },
                ]
        }));
        assert!(contexts.iter().any(|context| {
            context.compiler_macros
                == [
                    CCompilerMacroAction::Define {
                        name: "WRAP".to_owned(),
                        parameters: Some(vec!["x".to_owned()]),
                        replacement: "x".to_owned(),
                    },
                    CCompilerMacroAction::Define {
                        name: "SELECT".to_owned(),
                        parameters: None,
                        replacement: "second".to_owned(),
                    },
                ]
        }));
        let mut header = crate::parser::parse_file("src/shared.h", "int value;\n").unwrap();
        first.apply(&mut header);
        assert!(
            header
                .c_function_pointers
                .compiler_macro_contexts
                .is_empty(),
            "divergent TU macro contexts must not be projected onto headers"
        );
        let first_fingerprint = first.fingerprint().to_owned();
        fs::write(
            root.path().join("flags.rsp"),
            "-DWRAP(x)=x -DSELECT=changed\n",
        )
        .unwrap();
        let second = ProjectResolutionContext::load(root.path());
        assert_ne!(second.fingerprint(), first_fingerprint);
    }

    #[test]
    fn response_files_fail_closed_on_cycles_unknown_dialects_and_outside_roots() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        for file in ["src/cycle.c", "src/unknown.c", "src/outside.c"] {
            fs::write(root.path().join(file), "int value;\n").unwrap();
        }
        fs::write(root.path().join("a.rsp"), "@b.rsp\n").unwrap();
        fs::write(root.path().join("b.rsp"), "@a.rsp\n").unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("outside.rsp"), "-DOUTSIDE=1\n").unwrap();
        fs::write(
            root.path().join("compile_commands.json"),
            serde_json::to_vec(&serde_json::json!([
                {
                    "directory": root.path(),
                    "file": "src/cycle.c",
                    "arguments": ["cc", "@a.rsp", "src/cycle.c"]
                },
                {
                    "directory": root.path(),
                    "file": "src/unknown.c",
                    "arguments": ["mystery-cc", "@a.rsp", "src/unknown.c"]
                },
                {
                    "directory": root.path(),
                    "file": "src/outside.c",
                    "arguments": [
                        "cc",
                        format!("@{}", outside.path().join("outside.rsp").display()),
                        "src/outside.c"
                    ]
                }
            ]))
            .unwrap(),
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        assert!(context.cpp.by_source.is_empty());
    }

    #[test]
    fn compiler_driver_detection_accepts_only_explicit_launcher_chains() {
        let strings = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            compiler_driver_name(&strings(&["clang", "-c", "src/main.c"])).as_deref(),
            Some("clang")
        );
        assert_eq!(
            compiler_driver_name(&strings(&["ccache", "clang++", "-c", "src/main.cc"])).as_deref(),
            Some("clang++")
        );
        assert_eq!(
            compiler_driver_name(&strings(&[
                "env",
                "-i",
                "MODE=release",
                "--unset",
                "FLAGS",
                "sccache",
                "gcc",
                "-c",
                "src/main.c"
            ]))
            .as_deref(),
            Some("gcc")
        );
        assert_eq!(
            compiler_driver_name(&strings(&[
                "mystery-build",
                "--tool-name",
                "clang",
                "@flags.rsp"
            ])),
            None,
            "an arbitrary operand named clang must not select GNU response syntax"
        );
        assert_eq!(
            compiler_driver_name(&strings(&["env", "--split-string=clang -c", "src/main.c"])),
            None,
            "unsupported env options must fail closed"
        );
    }

    #[test]
    fn clang_option_terminator_prevents_operand_macro_parsing() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        for file in ["src/main.c", "src/env.c"] {
            fs::write(root.path().join(file), "int value;\n").unwrap();
        }
        fs::write(
            root.path().join("compile_commands.json"),
            serde_json::to_vec(&serde_json::json!([
                {
                    "directory": root.path(),
                    "file": "src/main.c",
                    "arguments": ["clang", "-DREAL=target", "--", "-DOPERAND=wrong"]
                },
                {
                    "directory": root.path(),
                    "file": "src/env.c",
                    "arguments": ["env", "--", "clang", "-DENV_REAL=target", "src/env.c"]
                }
            ]))
            .unwrap(),
        )
        .unwrap();

        let context = ProjectResolutionContext::load(root.path());
        let contexts = &context.cpp.by_source["src/main.c"];
        assert_eq!(contexts.len(), 1);
        assert_eq!(
            contexts[0].compiler_macros,
            [CCompilerMacroAction::Define {
                name: "REAL".to_owned(),
                parameters: None,
                replacement: "target".to_owned(),
            }]
        );
        assert_eq!(
            context.cpp.by_source["src/env.c"][0].compiler_macros,
            [CCompilerMacroAction::Define {
                name: "ENV_REAL".to_owned(),
                parameters: None,
                replacement: "target".to_owned(),
            }],
            "a launcher delimiter before the compiler must not terminate compiler options"
        );
    }

    #[test]
    fn response_expansion_never_executes_tokens_and_macro_caps_reject_the_tu() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/main.c"), "int value;\n").unwrap();
        let marker = root.path().join("must-not-exist");
        fs::write(
            root.path().join("flags.rsp"),
            format!("-DSELECT=target $(touch {})\n", marker.display()),
        )
        .unwrap();
        let canonical_root = fs::canonicalize(root.path()).unwrap();
        let (expanded, _) = expand_compiler_response_arguments(
            &["cc".to_owned(), "@flags.rsp".to_owned()],
            root.path(),
            &canonical_root,
            4_096,
            4_096,
        );
        assert!(expanded.is_some());
        assert!(!marker.exists(), "response tokens must never be executed");

        let mut arguments = vec!["cc".to_owned()];
        arguments.extend((0..257).map(|index| format!("-DMACRO_{index}=1")));
        arguments.push("src/main.c".to_owned());
        fs::write(
            root.path().join("compile_commands.json"),
            serde_json::to_vec(&serde_json::json!([{
                "directory": root.path(),
                "file": "src/main.c",
                "arguments": arguments
            }]))
            .unwrap(),
        )
        .unwrap();
        assert!(
            ProjectResolutionContext::load(root.path())
                .cpp
                .by_source
                .is_empty(),
            "overflow must reject the whole TU context instead of truncating actions"
        );
    }

    #[test]
    fn one_malformed_compile_variant_rejects_all_macro_contexts_for_the_source() {
        let root = tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::write(root.path().join("src/main.c"), "int value;\n").unwrap();
        fs::write(
            root.path().join("compile_commands.json"),
            serde_json::to_vec(&serde_json::json!([
                {
                    "directory": root.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "-DSELECT=valid", "src/main.c"]
                },
                {
                    "directory": root.path(),
                    "file": "src/main.c",
                    "arguments": ["cc", "-D1INVALID=value", "src/main.c"]
                }
            ]))
            .unwrap(),
        )
        .unwrap();
        let context = ProjectResolutionContext::load(root.path());
        assert!(!context.cpp.by_source.contains_key("src/main.c"));
        assert!(context.cpp.shared_includes.is_none());
    }

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
