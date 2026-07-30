use super::normalize_path;
use crate::model::CCompilerMacroAction;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CppIncludePaths {
    pub(super) quote: Vec<PathBuf>,
    pub(super) general: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CppTranslationUnitContext {
    pub(super) includes: CppIncludePaths,
    pub(super) compiler_macros: Vec<CCompilerMacroAction>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct CppCompileContext {
    pub(super) by_source: HashMap<String, Vec<CppTranslationUnitContext>>,
    pub(super) shared_includes: Option<CppIncludePaths>,
    pub(super) database_present: bool,
    pub(super) fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CppIncludeResolution {
    Unmanaged,
    Resolved(String),
    Rejected,
}

pub(super) fn load_cpp_compile_context(root: &Path) -> CppCompileContext {
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

pub(super) fn expand_compiler_response_arguments(
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

pub(super) fn compiler_driver_name(arguments: &[String]) -> Option<String> {
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

pub(super) fn split_compiler_command(command: &str) -> Option<Vec<String>> {
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
