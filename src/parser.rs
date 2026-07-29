use crate::model::{
    CFunctionPointerArgumentFact, CFunctionPointerArrayDispatchFact, CFunctionPointerArrayFact,
    CFunctionPointerArrayTargetFact, CFunctionPointerBindingFact, CFunctionPointerDispatchFact,
    CFunctionPointerFactoryDispatchFact, CFunctionPointerFacts, CFunctionPointerFormalStorageFact,
    CFunctionPointerPropagationFact, CFunctionPointerReturnFact, CFunctionPointerTypedefFact,
    CIncludeFact, CLocalFunctionPointerBindingFact, CLocalFunctionPointerDispatchFact,
    CStructFieldFact, CStructLayoutFact, CallableReturnFact, CallbackArgumentFact,
    CallbackParameterDelegationFact, CallbackParameterInvocation, Evidence, FileFacts, Language,
    PythonCallbackFormalFact, Relationship, RelationshipKind, SourceSpan, Symbol, SymbolKind,
    UnresolvedCall, UnresolvedReference,
};
use crate::semantic::{enrich_file_facts, is_arkui_intrinsic_name};
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
#[cfg(test)]
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

#[cfg(test)]
pub(crate) fn parse_file(relative_path: &str, source: &str) -> Result<FileFacts> {
    let path = Path::new(relative_path);
    let language = Language::from_path(path)
        .ok_or_else(|| anyhow!("unsupported source language: {relative_path}"))?;
    parse_file_as(relative_path, source, language)
}

pub(crate) fn parse_file_as(
    relative_path: &str,
    source: &str,
    language: Language,
) -> Result<FileFacts> {
    let embedded_source = match language {
        Language::Vue | Language::Svelte => Some(embedded_script_source(source)),
        Language::Astro => Some(astro_script_source(source)),
        _ => None,
    };
    let syntax_source = embedded_source.as_deref().unwrap_or(source);
    let tree = parse_tree(language, syntax_source)?;
    let content_hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let source_bytes = source.as_bytes();

    let file_span = SourceSpan {
        start_byte: 0,
        end_byte: source.len(),
        start_line: 1,
        end_line: source.lines().count().max(1),
    };
    let file_symbol = Symbol::new(
        language,
        SymbolKind::File,
        relative_path,
        relative_path,
        relative_path,
        file_span,
    );
    let mut symbols = vec![file_symbol.clone()];
    collect_symbols(
        tree.root_node(),
        source_bytes,
        language,
        relative_path,
        None,
        &mut symbols,
    );
    disambiguate_duplicate_symbols(&mut symbols);
    let mut callable_returns = Vec::new();
    if matches!(
        language,
        Language::TypeScript | Language::Tsx | Language::Astro | Language::ArkTs
    ) {
        let symbols_by_span = symbols
            .iter()
            .map(|symbol| ((symbol.start_byte, symbol.end_byte), symbol.id.as_str()))
            .collect::<HashMap<_, _>>();
        collect_callable_returns(
            tree.root_node(),
            source_bytes,
            &symbols_by_span,
            &mut callable_returns,
        );
    }

    let mut relationships = Vec::new();
    for symbol in symbols.iter().skip(1) {
        relationships.push(Relationship {
            source_id: file_symbol.id.clone(),
            target_id: symbol.id.clone(),
            kind: RelationshipKind::Contains,
            evidence: Evidence::new(
                "tree-sitter",
                1.0,
                format!("{} is declared in {}", symbol.qualified_name, relative_path),
                relative_path,
                symbol.start_line,
            ),
        });
    }

    let mut unresolved_calls = Vec::new();
    let mut callback_parameter_invocations = Vec::new();
    let mut callback_parameter_delegations = Vec::new();
    let mut callback_arguments = Vec::new();
    let mut symbol_owners = symbols
        .iter()
        .skip(1)
        .map(|symbol| ((symbol.start_byte, symbol.end_byte), symbol.id.clone()))
        .collect::<HashMap<_, _>>();
    let python_callback_formals = if language == Language::Python {
        collect_python_callback_formals(tree.root_node(), source_bytes, &symbol_owners)
    } else {
        Vec::new()
    };
    if language == Language::Dart {
        collect_dart_body_owners(tree.root_node(), &symbols, &mut symbol_owners);
    }
    let c_function_pointers = if matches!(language, Language::C | Language::Cpp) {
        collect_c_function_pointer_facts(
            tree.root_node(),
            source_bytes,
            language,
            &symbols,
            &symbol_owners,
            &file_symbol.id,
        )
    } else {
        CFunctionPointerFacts::default()
    };
    let indirect_call_sites = c_function_pointers
        .dispatches
        .iter()
        .filter(|dispatch| dispatch.proven_function_pointer)
        .map(|dispatch| dispatch.site_start_byte)
        .chain(
            c_function_pointers
                .local_dispatches
                .iter()
                .filter(|dispatch| local_pointer_declaration_at(dispatch, &c_function_pointers))
                .map(|dispatch| dispatch.site_start_byte),
        )
        .collect::<std::collections::HashSet<_>>();
    let inline_callback_symbols = collect_inline_callback_symbols(
        tree.root_node(),
        source_bytes,
        language,
        relative_path,
        &symbols,
        &mut symbol_owners,
    );
    let inline_callback_ids = inline_callback_symbols
        .values()
        .map(|symbol| symbol.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut factory_returns = FactoryReturnMap::new();
    collect_local_factory_returns(tree.root_node(), source_bytes, &mut factory_returns);
    for returns in factory_returns.values_mut() {
        returns.sort_by_key(|binding| binding.position);
    }
    let mut collection_elements = CollectionElementMap::new();
    let mut reassigned_collections = std::collections::HashSet::new();
    collect_collection_element_bindings(
        tree.root_node(),
        source_bytes,
        &mut collection_elements,
        &mut reassigned_collections,
    );
    collection_elements.retain(|key, _| !reassigned_collections.contains(key));
    for elements in collection_elements.values_mut() {
        elements.sort_by_key(|binding| binding.position);
    }
    let mut receiver_bindings = ReceiverBindingMap::new();
    collect_receiver_bindings(
        tree.root_node(),
        source_bytes,
        &factory_returns,
        &collection_elements,
        &mut receiver_bindings,
    );
    for bindings in receiver_bindings.values_mut() {
        bindings.sort_by_key(|binding| binding.position);
    }
    let module_bindings = collect_module_bindings(tree.root_node(), source_bytes, language);
    let project_names = symbols
        .iter()
        .skip(1)
        .map(|symbol| symbol.name.as_str())
        .chain(module_bindings.keys().map(String::as_str))
        .collect::<std::collections::HashSet<_>>();
    let call_context = CallCollectionContext {
        source: source_bytes,
        language,
        file: relative_path,
        symbol_owners: &symbol_owners,
        receiver_bindings: &receiver_bindings,
        module_bindings: &module_bindings,
        project_names: &project_names,
        inline_callback_symbols: &inline_callback_symbols,
        inline_callback_ids: &inline_callback_ids,
        indirect_call_sites: &indirect_call_sites,
        file_symbol_id: &file_symbol.id,
    };
    collect_calls(
        tree.root_node(),
        &call_context,
        &mut unresolved_calls,
        &mut callback_parameter_invocations,
        &mut callback_parameter_delegations,
        &mut callback_arguments,
    );
    collect_stored_callback_parameter_invocations(
        tree.root_node(),
        source_bytes,
        &symbol_owners,
        &mut callback_parameter_invocations,
    );
    let mut unresolved_references = Vec::new();
    collect_structural_references(
        tree.root_node(),
        source_bytes,
        relative_path,
        &symbols,
        &mut unresolved_references,
    );

    let mut facts = FileFacts {
        path: relative_path.to_owned(),
        content_hash,
        language,
        symbols,
        relationships,
        unresolved_calls,
        callback_parameter_invocations,
        callback_parameter_delegations,
        callback_arguments,
        python_callback_formals,
        callable_returns,
        arkui_builder_flow: Default::default(),
        unresolved_references,
        dynamic_events: Vec::new(),
        literal_bindings: Vec::new(),
        module_exports: Vec::new(),
        c_function_pointers,
        fastapi: Default::default(),
    };
    enrich_file_facts(tree.root_node(), source_bytes, &mut facts);
    Ok(facts)
}

fn collect_callable_returns(
    node: Node<'_>,
    source: &[u8],
    symbols_by_span: &HashMap<(usize, usize), &str>,
    output: &mut Vec<CallableReturnFact>,
) {
    if matches!(node.kind(), "function_declaration" | "method_definition") {
        if let (Some(owner_id), Some(return_type)) = (
            symbols_by_span.get(&(node.start_byte(), node.end_byte())),
            node.child_by_field_name("return_type")
                .and_then(|annotation| simple_nominal_return_type(annotation, source)),
        ) {
            output.push(CallableReturnFact {
                owner_id: (*owner_id).to_owned(),
                type_name: return_type,
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_callable_returns(child, source, symbols_by_span, output);
    }
}

fn simple_nominal_return_type(annotation: Node<'_>, source: &[u8]) -> Option<String> {
    let raw = node_text(annotation, source);
    let name = raw.trim().trim_start_matches(':').trim();
    let mut chars = name.chars();
    let first = chars.next()?;
    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        .then_some(())
        .filter(|_| {
            chars.all(|character| {
                character == '_' || character == '$' || character.is_ascii_alphanumeric()
            })
        })?;
    (!matches!(
        name,
        "any"
            | "unknown"
            | "never"
            | "void"
            | "undefined"
            | "null"
            | "string"
            | "number"
            | "boolean"
            | "bigint"
            | "symbol"
            | "object"
    ))
    .then(|| name.to_owned())
}

fn collect_dart_body_owners(
    node: Node<'_>,
    symbols: &[Symbol],
    owners: &mut HashMap<(usize, usize), String>,
) {
    if node.kind() == "function_body" {
        if let Some(signature) = node.prev_named_sibling() {
            let signature_start = if signature.kind() == "method_signature" {
                first_descendant(signature, "function_signature")
                    .or_else(|| first_descendant(signature, "constructor_signature"))
                    .map(|child| child.start_byte())
                    .unwrap_or_else(|| signature.start_byte())
            } else {
                signature.start_byte()
            };
            if let Some(symbol) = symbols
                .iter()
                .find(|symbol| symbol.start_byte == signature_start)
            {
                owners.insert((node.start_byte(), node.end_byte()), symbol.id.clone());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_dart_body_owners(child, symbols, owners);
    }
}

fn first_descendant<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| first_descendant(child, kind));
    found
}

fn disambiguate_duplicate_symbols(symbols: &mut [Symbol]) {
    let mut groups = HashMap::<String, Vec<usize>>::new();
    for (index, symbol) in symbols.iter().enumerate() {
        groups
            .entry(symbol.semantic_key.clone())
            .or_default()
            .push(index);
    }
    for (semantic_key, indices) in groups {
        if indices.len() < 2 {
            continue;
        }
        for (ordinal, index) in indices.into_iter().enumerate() {
            let disambiguated = format!("{semantic_key}|duplicate:{}", ordinal + 1);
            let digest = blake3::hash(disambiguated.as_bytes()).to_hex();
            symbols[index].semantic_key = disambiguated;
            symbols[index].id = format!("sym_{}", &digest[..24]);
        }
    }
}

fn collect_structural_references(
    node: Node<'_>,
    source: &[u8],
    file: &str,
    symbols: &[Symbol],
    output: &mut Vec<UnresolvedReference>,
) {
    match node.kind() {
        "import_clause" => {
            if let (Some(file_symbol), Some((target_name, binding_name, target_file_hint))) =
                (symbols.first(), relative_default_import(node, source))
            {
                push_import_reference(
                    output,
                    file_symbol,
                    target_name,
                    binding_name,
                    Some(target_file_hint),
                    file,
                    node.start_position().row + 1,
                );
            }
        }
        "import_specifier" => {
            if let (Some(file_symbol), Some(name)) =
                (symbols.first(), node.child_by_field_name("name"))
            {
                let target_name = node_text(name, source);
                let binding_name = node
                    .child_by_field_name("alias")
                    .map(|alias| node_text(alias, source))
                    .unwrap_or_else(|| target_name.clone());
                let target_file_hint = import_source_hint(node, source);
                push_import_reference(
                    output,
                    file_symbol,
                    target_name,
                    binding_name,
                    target_file_hint,
                    file,
                    node.start_position().row + 1,
                );
            }
        }
        "import_from_statement" => {
            if let Some(file_symbol) = symbols.first() {
                let module = node.child_by_field_name("module_name");
                let mut names = Vec::new();
                collect_identifier_nodes(node, source, &mut names);
                names.retain(|(candidate, start, end)| {
                    module.is_none_or(|module| {
                        *start < module.start_byte() || *end > module.end_byte()
                    }) && candidate != "as"
                });
                names.sort();
                names.dedup();
                for (name, _, _) in names {
                    push_reference(
                        output,
                        file_symbol,
                        name,
                        RelationshipKind::Imports,
                        ReferenceEvidence {
                            provenance: "tree-sitter/import",
                            confidence: 0.9,
                            file,
                            line: node.start_position().row + 1,
                        },
                    );
                }
            }
        }
        "use_declaration" => {
            if let Some(file_symbol) = symbols.first() {
                if let Some((target_name, binding_name, target_file_hint)) =
                    rust_use_reference(node, source)
                {
                    push_import_reference(
                        output,
                        file_symbol,
                        target_name,
                        binding_name,
                        target_file_hint,
                        file,
                        node.start_position().row + 1,
                    );
                }
            }
        }
        "class_declaration" | "interface_declaration" | "class_definition" => {
            if let Some(source_symbol) = symbols
                .iter()
                .find(|symbol| symbol.start_byte == node.start_byte())
            {
                collect_heritage(node, source, file, source_symbol, output);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_structural_references(child, source, file, symbols, output);
    }
}

fn relative_default_import(clause: Node<'_>, source: &[u8]) -> Option<(String, String, String)> {
    let target_file_hint = import_source_hint(clause, source)?;
    if !target_file_hint.starts_with("./") && !target_file_hint.starts_with("../") {
        return None;
    }
    let binding_name = (0..clause.named_child_count())
        .filter_map(|index| clause.named_child(index))
        .find(|child| child.kind() == "identifier")
        .map(|identifier| node_text(identifier, source))?;
    let target_name = target_file_hint
        .rsplit_once('.')
        .filter(|(_, extension)| extension.eq_ignore_ascii_case("astro"))
        .and_then(|_| std::path::Path::new(&target_file_hint).file_stem())
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| binding_name.clone());
    (!target_name.is_empty() && !binding_name.is_empty()).then_some((
        target_name,
        binding_name,
        target_file_hint,
    ))
}

fn rust_use_reference(
    declaration: Node<'_>,
    source: &[u8],
) -> Option<(String, String, Option<String>)> {
    let declaration = node_text(declaration, source);
    let body = declaration
        .trim()
        .strip_prefix("use ")?
        .trim_end_matches(';')
        .trim();
    if body
        .chars()
        .any(|character| matches!(character, '{' | '}' | '*'))
    {
        return None;
    }
    let (path, alias) = body
        .rsplit_once(" as ")
        .map_or((body, None), |(path, alias)| {
            (path.trim(), Some(alias.trim()))
        });
    let segments = path
        .split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let target_name = segments.last()?.to_string();
    let binding_name = alias.unwrap_or(&target_name).to_owned();
    let target_file_hint = segments
        .first()
        .filter(|segment| !matches!(**segment, "crate" | "self" | "super"))
        .map(|segment| (*segment).to_owned());
    Some((target_name, binding_name, target_file_hint))
}

fn collect_heritage(
    declaration: Node<'_>,
    source: &[u8],
    file: &str,
    source_symbol: &Symbol,
    output: &mut Vec<UnresolvedReference>,
) {
    let mut cursor = declaration.walk();
    for child in declaration.named_children(&mut cursor) {
        let kind = match child.kind() {
            "class_heritage" => None,
            "superclasses" => Some(RelationshipKind::Extends),
            _ => continue,
        };
        if child.kind() == "class_heritage" {
            let mut heritage_cursor = child.walk();
            for clause in child.named_children(&mut heritage_cursor) {
                let relationship_kind = match clause.kind() {
                    "extends_clause" | "extends_type_clause" => RelationshipKind::Extends,
                    "implements_clause" => RelationshipKind::Implements,
                    _ => continue,
                };
                push_heritage_names(
                    clause,
                    source,
                    file,
                    source_symbol,
                    relationship_kind,
                    output,
                );
            }
        } else if let Some(relationship_kind) = kind {
            push_heritage_names(
                child,
                source,
                file,
                source_symbol,
                relationship_kind,
                output,
            );
        }
    }
    if let Some(superclasses) = declaration.child_by_field_name("superclasses") {
        push_heritage_names(
            superclasses,
            source,
            file,
            source_symbol,
            RelationshipKind::Extends,
            output,
        );
    }
}

fn push_heritage_names(
    node: Node<'_>,
    source: &[u8],
    file: &str,
    source_symbol: &Symbol,
    kind: RelationshipKind,
    output: &mut Vec<UnresolvedReference>,
) {
    let mut names = Vec::new();
    collect_identifier_nodes(node, source, &mut names);
    names.sort();
    names.dedup();
    for (name, _, _) in names {
        push_reference(
            output,
            source_symbol,
            name,
            kind,
            ReferenceEvidence {
                provenance: "tree-sitter/heritage",
                confidence: 0.95,
                file,
                line: node.start_position().row + 1,
            },
        );
    }
}

fn push_reference(
    output: &mut Vec<UnresolvedReference>,
    source: &Symbol,
    target_name: String,
    kind: RelationshipKind,
    evidence: ReferenceEvidence<'_>,
) {
    if target_name.is_empty() || target_name == source.name {
        return;
    }
    output.push(UnresolvedReference {
        source_id: source.id.clone(),
        explanation: format!("{kind} reference to {target_name}"),
        binding_name: target_name.clone(),
        target_name,
        target_file_hint: None,
        kind,
        provenance: evidence.provenance.to_owned(),
        confidence: evidence.confidence,
        file: evidence.file.to_owned(),
        line: evidence.line,
    });
}

fn push_import_reference(
    output: &mut Vec<UnresolvedReference>,
    source: &Symbol,
    target_name: String,
    binding_name: String,
    target_file_hint: Option<String>,
    file: &str,
    line: usize,
) {
    if target_name.is_empty() {
        return;
    }
    output.push(UnresolvedReference {
        source_id: source.id.clone(),
        explanation: format!("imports {target_name} as {binding_name}"),
        target_name,
        binding_name,
        target_file_hint,
        kind: RelationshipKind::Imports,
        provenance: "tree-sitter/import".to_owned(),
        confidence: 0.95,
        file: file.to_owned(),
        line,
    });
}

fn import_source_hint(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if parent.kind() == "import_statement" {
            return parent
                .child_by_field_name("source")
                .map(|source_node| node_text(source_node, source))
                .map(|value| value.trim_matches(['\'', '"']).replace('\\', "/"));
        }
        ancestor = parent.parent();
    }
    None
}

struct ReferenceEvidence<'a> {
    provenance: &'a str,
    confidence: f64,
    file: &'a str,
    line: usize,
}

fn collect_identifier_nodes(
    node: Node<'_>,
    source: &[u8],
    output: &mut Vec<(String, usize, usize)>,
) {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "property_identifier"
    ) {
        output.push((node_text(node, source), node.start_byte(), node.end_byte()));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifier_nodes(child, source, output);
    }
}

fn parse_tree(language: Language, source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    let grammar = match language {
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
        Language::JavaScript | Language::Jsx => tree_sitter_javascript::LANGUAGE,
        Language::Vue | Language::Svelte | Language::Astro => tree_sitter_typescript::LANGUAGE_TSX,
        Language::ArkTs => tree_sitter_arkts::LANGUAGE,
        Language::Python => tree_sitter_python::LANGUAGE,
        Language::Rust => tree_sitter_rust::LANGUAGE,
        Language::Go => tree_sitter_go::LANGUAGE,
        Language::Java => tree_sitter_java::LANGUAGE,
        Language::CSharp => tree_sitter_c_sharp::LANGUAGE,
        Language::C => tree_sitter_c::LANGUAGE,
        Language::Cpp => tree_sitter_cpp::LANGUAGE,
        Language::Dart => tree_sitter_dart_orchard::LANGUAGE,
        Language::Ruby => tree_sitter_ruby::LANGUAGE,
        Language::Php => tree_sitter_php::LANGUAGE_PHP,
        Language::Swift => tree_sitter_swift::LANGUAGE,
        Language::Lua => tree_sitter_lua::LANGUAGE,
        Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE,
        Language::Scala => tree_sitter_scala::LANGUAGE,
        Language::R => tree_sitter_r::LANGUAGE,
    };
    parser
        .set_language(&grammar.into())
        .context("load tree-sitter grammar")?;
    parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("tree-sitter returned no syntax tree"))
}

fn embedded_script_source(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let mut masked = source
        .as_bytes()
        .iter()
        .map(|byte| {
            if matches!(*byte, b'\n' | b'\r') {
                *byte
            } else {
                b' '
            }
        })
        .collect::<Vec<_>>();
    let mut offset = 0;
    while let Some(relative_open) = lower[offset..].find("<script") {
        let open = offset + relative_open;
        let Some(relative_body) = lower[open..].find('>') else {
            break;
        };
        let body = open + relative_body + 1;
        let Some(relative_close) = lower[body..].find("</script") else {
            break;
        };
        let close = body + relative_close;
        masked[body..close].copy_from_slice(&source.as_bytes()[body..close]);
        let Some(relative_end) = lower[close..].find('>') else {
            break;
        };
        offset = close + relative_end + 1;
    }
    String::from_utf8(masked).expect("masking source preserves UTF-8")
}

pub(crate) fn astro_script_source(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut masked = bytes
        .iter()
        .map(|byte| {
            if matches!(*byte, b'\n' | b'\r') {
                *byte
            } else {
                b' '
            }
        })
        .collect::<Vec<_>>();

    match copy_astro_frontmatter(bytes, &mut masked) {
        AstroFrontmatter::Absent => copy_astro_script_blocks(bytes, 0, &mut masked),
        AstroFrontmatter::Closed(script_start) => {
            copy_astro_script_blocks(bytes, script_start, &mut masked);
        }
        AstroFrontmatter::Unclosed => {}
    }
    String::from_utf8(masked).expect("masking source preserves UTF-8")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AstroFrontmatter {
    Absent,
    Closed(usize),
    Unclosed,
}

fn copy_astro_frontmatter(source: &[u8], masked: &mut [u8]) -> AstroFrontmatter {
    let mut line_start = 0;
    let (body_start, mut line_start) = loop {
        let Some((start, end, next)) = line_bounds(source, line_start) else {
            return AstroFrontmatter::Absent;
        };
        let mut line = &source[start..end];
        if start == 0 {
            line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
        }
        let trimmed = trim_ascii_horizontal(line);
        if trimmed.is_empty() {
            if next <= line_start {
                return AstroFrontmatter::Absent;
            }
            line_start = next;
            continue;
        }
        if trimmed != b"---" {
            return AstroFrontmatter::Absent;
        }
        break (next, next);
    };

    while let Some((start, end, next)) = line_bounds(source, line_start) {
        if trim_ascii_horizontal(&source[start..end]) == b"---" {
            masked[body_start..start].copy_from_slice(&source[body_start..start]);
            return AstroFrontmatter::Closed(next);
        }
        if next <= line_start {
            break;
        }
        line_start = next;
    }
    AstroFrontmatter::Unclosed
}

fn line_bounds(source: &[u8], start: usize) -> Option<(usize, usize, usize)> {
    if start >= source.len() {
        return None;
    }
    let newline = source[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map(|relative| start + relative);
    match newline {
        Some(newline) => {
            let end = newline - usize::from(newline > start && source[newline - 1] == b'\r');
            Some((start, end, newline + 1))
        }
        None => Some((start, source.len(), source.len())),
    }
}

fn trim_ascii_horizontal(mut value: &[u8]) -> &[u8] {
    while value
        .first()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        value = &value[1..];
    }
    while value
        .last()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        value = &value[..value.len() - 1];
    }
    value
}

fn copy_astro_script_blocks(source: &[u8], mut offset: usize, masked: &mut [u8]) {
    while offset < source.len() {
        if starts_ascii_case_insensitive(&source[offset..], b"<!--") {
            offset = find_ascii_case_insensitive(source, offset + 4, b"-->")
                .map_or(source.len(), |end| end + 3);
            continue;
        }
        if source[offset] != b'<'
            || !starts_ascii_case_insensitive(&source[offset + 1..], b"script")
            || !source
                .get(offset + 7)
                .is_none_or(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
        {
            offset += 1;
            continue;
        }

        let Some(open_end) = find_tag_end(source, offset + 7) else {
            break;
        };
        if source[offset..open_end]
            .iter()
            .rev()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(&b'/')
        {
            offset = open_end + 1;
            continue;
        }
        let body_start = open_end + 1;
        let Some(close) = find_script_close(source, body_start) else {
            break;
        };
        let Some(close_end) = find_tag_end(source, close + 8) else {
            break;
        };
        masked[body_start..close].copy_from_slice(&source[body_start..close]);
        offset = close_end + 1;
    }
}

fn find_tag_end(source: &[u8], mut offset: usize) -> Option<usize> {
    let mut quote = None;
    while let Some(&byte) = source.get(offset) {
        match (quote, byte) {
            (Some(active), current) if current == active => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(offset),
            _ => {}
        }
        offset += 1;
    }
    None
}

fn find_script_close(source: &[u8], mut offset: usize) -> Option<usize> {
    #[derive(Clone, Copy)]
    enum LexicalState {
        Code,
        Quote(u8),
        LineComment,
        BlockComment,
    }

    let mut state = LexicalState::Code;
    while offset < source.len() {
        match state {
            LexicalState::Code => {
                if starts_ascii_case_insensitive(&source[offset..], b"</script")
                    && source
                        .get(offset + 8)
                        .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'>')
                {
                    return Some(offset);
                }
                match (source[offset], source.get(offset + 1).copied()) {
                    (b'/', Some(b'/')) => {
                        state = LexicalState::LineComment;
                        offset += 2;
                        continue;
                    }
                    (b'/', Some(b'*')) => {
                        state = LexicalState::BlockComment;
                        offset += 2;
                        continue;
                    }
                    (quote @ (b'\'' | b'"' | b'`'), _) => {
                        state = LexicalState::Quote(quote);
                    }
                    _ => {}
                }
            }
            LexicalState::Quote(quote) => {
                if source[offset] == b'\\' {
                    offset = (offset + 2).min(source.len());
                    continue;
                }
                if source[offset] == quote {
                    state = LexicalState::Code;
                } else if quote != b'`' && matches!(source[offset], b'\n' | b'\r') {
                    // Recover from a malformed single-line string without hiding all
                    // subsequent, otherwise valid script blocks.
                    state = LexicalState::Code;
                }
            }
            LexicalState::LineComment => {
                if matches!(source[offset], b'\n' | b'\r') {
                    state = LexicalState::Code;
                }
            }
            LexicalState::BlockComment => {
                if source[offset] == b'*' && source.get(offset + 1) == Some(&b'/') {
                    state = LexicalState::Code;
                    offset += 2;
                    continue;
                }
            }
        }
        offset += 1;
    }
    None
}

fn find_ascii_case_insensitive(source: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    source
        .get(start..)?
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|relative| start + relative)
}

fn starts_ascii_case_insensitive(source: &[u8], needle: &[u8]) -> bool {
    source
        .get(..needle.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(needle))
}

fn collect_symbols(
    node: Node<'_>,
    source: &[u8],
    language: Language,
    file: &str,
    container: Option<&str>,
    output: &mut Vec<Symbol>,
) {
    let declaration = declaration_at(node, source, container, language);
    let declaration_container = declaration
        .as_ref()
        .map(|(qualified, _, _, _)| qualified.clone())
        .or_else(|| container.map(str::to_owned));
    let next_container = rust_impl_container(node, source, container).or(declaration_container);

    if let Some((qualified_name, kind, name_node, discriminator)) = declaration {
        let name = node_text(name_node, source);
        output.push(Symbol::new_disambiguated(
            language,
            kind,
            name,
            qualified_name,
            file,
            span(node),
            &discriminator,
        ));
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_symbols(
            child,
            source,
            language,
            file,
            next_container.as_deref(),
            output,
        );
    }
}

fn declaration_at<'a>(
    node: Node<'a>,
    source: &[u8],
    container: Option<&str>,
    _language: Language,
) -> Option<(String, SymbolKind, Node<'a>, String)> {
    let (kind, name_node) = match node.kind() {
        "class_declaration" | "class_definition" => {
            let kind = node
                .child_by_field_name("declaration_kind")
                .map(|declaration| node_text(declaration, source))
                .map(|declaration| match declaration.as_str() {
                    "struct" => SymbolKind::Struct,
                    "enum" => SymbolKind::Enum,
                    _ => SymbolKind::Class,
                })
                .unwrap_or(SymbolKind::Class);
            (kind, node.child_by_field_name("name")?)
        }
        "interface_declaration" => (SymbolKind::Interface, node.child_by_field_name("name")?),
        "function_declaration" | "generator_function_declaration" | "function_item" => {
            (SymbolKind::Function, node.child_by_field_name("name")?)
        }
        "function_signature" => (
            if _language == Language::Dart && container.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            },
            node.child_by_field_name("name")?,
        ),
        "function_definition" => (
            SymbolKind::Function,
            node.child_by_field_name("name")
                .filter(Node::is_named)
                .or_else(|| {
                    node.child_by_field_name("declarator")
                        .and_then(declarator_name)
                })?,
        ),
        "method_definition" | "method_signature" => {
            (SymbolKind::Method, node.child_by_field_name("name")?)
        }
        "method_declaration" | "constructor_declaration" | "protocol_function_declaration" => {
            (SymbolKind::Method, node.child_by_field_name("name")?)
        }
        "local_function_statement" => (SymbolKind::Function, node.child_by_field_name("name")?),
        "variable_declarator" => {
            let value = node.child_by_field_name("value")?;
            if !matches!(value.kind(), "arrow_function" | "function_expression") {
                return None;
            }
            (SymbolKind::Function, node.child_by_field_name("name")?)
        }
        "struct_item" => (SymbolKind::Struct, node.child_by_field_name("name")?),
        "trait_item" => (SymbolKind::Trait, node.child_by_field_name("name")?),
        "enum_item" => (SymbolKind::Enum, node.child_by_field_name("name")?),
        "struct_declaration" | "record_declaration" => {
            (SymbolKind::Struct, node.child_by_field_name("name")?)
        }
        "public_field_definition" if _language == Language::ArkTs => {
            (SymbolKind::Variable, node.child_by_field_name("name")?)
        }
        "enum_declaration" => (SymbolKind::Enum, node.child_by_field_name("name")?),
        "type_spec" => {
            let value = node.child_by_field_name("type")?;
            let kind = match value.kind() {
                "struct_type" => SymbolKind::Struct,
                "interface_type" => SymbolKind::Interface,
                _ => SymbolKind::Type,
            };
            (kind, node.child_by_field_name("name")?)
        }
        "class_specifier" => (SymbolKind::Class, node.child_by_field_name("name")?),
        "struct_specifier" => (SymbolKind::Struct, node.child_by_field_name("name")?),
        "union_specifier" => (SymbolKind::Type, node.child_by_field_name("name")?),
        "class" => (SymbolKind::Class, node.child_by_field_name("name")?),
        "module" => (SymbolKind::Type, node.child_by_field_name("name")?),
        "protocol_declaration" => (SymbolKind::Interface, node.child_by_field_name("name")?),
        "trait_definition" => (SymbolKind::Trait, node.child_by_field_name("name")?),
        "object_declaration" | "object_definition" => {
            (SymbolKind::Type, node.child_by_field_name("name")?)
        }
        "binary_operator" => {
            let rhs = node.child_by_field_name("rhs")?;
            if rhs.kind() != "function_definition" {
                return None;
            }
            let lhs = node.child_by_field_name("lhs")?;
            if lhs.kind() != "identifier" {
                return None;
            }
            (SymbolKind::Function, lhs)
        }
        "method" | "singleton_method" => (
            if container.is_some() {
                SymbolKind::Method
            } else {
                SymbolKind::Function
            },
            node.child_by_field_name("name")?,
        ),
        _ => return None,
    };
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let qualified = container
        .map(|parent| format!("{parent}.{name}"))
        .unwrap_or_else(|| name.clone());
    let discriminator = callable_discriminator(node, source, kind);
    Some((qualified, kind, name_node, discriminator))
}

fn declarator_name(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier" | "field_identifier" | "operator_name" | "destructor_name"
    ) {
        return Some(node);
    }
    if let Some(declarator) = node.child_by_field_name("declarator") {
        if let Some(name) = declarator_name(declarator) {
            return Some(name);
        }
    }
    let mut cursor = node.walk();
    let name = node.named_children(&mut cursor).find_map(declarator_name);
    name
}

fn callable_discriminator(node: Node<'_>, source: &[u8], kind: SymbolKind) -> String {
    if !matches!(kind, SymbolKind::Function | SymbolKind::Method) {
        return String::new();
    }
    let callable = if node.kind() == "binary_operator" {
        node.child_by_field_name("rhs").unwrap_or(node)
    } else {
        node
    };
    let direct = ["parameters", "return_type"]
        .into_iter()
        .filter_map(|field| callable.child_by_field_name(field))
        .map(|child| normalize_signature(&node_text(child, source)))
        .collect::<Vec<_>>()
        .join("->");
    if direct.is_empty() {
        callable
            .child_by_field_name("declarator")
            .map(|child| normalize_signature(&node_text(child, source)))
            .unwrap_or_default()
    } else {
        direct
    }
}

fn normalize_signature(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rust_impl_container(node: Node<'_>, source: &[u8], parent: Option<&str>) -> Option<String> {
    if node.kind() != "impl_item" {
        return None;
    }
    let implemented_type = node.child_by_field_name("type")?;
    let name = node_text(implemented_type, source);
    Some(
        parent
            .map(|container| format!("{container}.{name}"))
            .unwrap_or(name),
    )
}

const MAX_C_FUNCTION_POINTER_FACTS_PER_FILE: usize = 16_384;
const MAX_C_RECEIVER_PATH_DEPTH: usize = 8;

fn collect_c_function_pointer_facts(
    root: Node<'_>,
    source: &[u8],
    language: Language,
    symbols: &[Symbol],
    owners: &HashMap<(usize, usize), String>,
    file_symbol_id: &str,
) -> CFunctionPointerFacts {
    let callable_names = symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
        .map(|symbol| symbol.name.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut facts = CFunctionPointerFacts::default();
    collect_c_function_pointer_typedefs(root, source, &mut facts.typedefs);
    let function_types = facts
        .typedefs
        .iter()
        .map(|fact| (fact.name.clone(), fact.pointer))
        .collect::<HashMap<_, _>>();
    collect_c_struct_layouts(root, source, &function_types, &mut facts.layouts);
    let function_fields = facts
        .layouts
        .iter()
        .flat_map(|layout| {
            layout
                .fields
                .iter()
                .filter(|field| field.function_pointer)
                .map(|field| field.name.clone())
        })
        .collect::<std::collections::HashSet<_>>();
    collect_c_function_pointer_observations(
        root,
        source,
        language,
        owners,
        file_symbol_id,
        &callable_names,
        &function_fields,
        &function_types,
        &mut facts,
    );
    if facts.typedefs.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.typedefs.clear();
    }
    if facts.layouts.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.layouts.clear();
    }
    if facts.bindings.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.bindings.clear();
    }
    if facts.propagations.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.propagations.clear();
    }
    if facts.dispatches.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.dispatches.clear();
    }
    if facts.arrays.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.arrays.clear();
    }
    if facts.array_dispatches.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.array_dispatches.clear();
    }
    if facts.formal_storages.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.formal_storages.clear();
    }
    if facts.arguments.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.arguments.clear();
    }
    if facts.local_bindings.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.local_bindings.clear();
    }
    if facts.local_dispatches.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.local_dispatches.clear();
    }
    if facts.returns.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.returns.clear();
    }
    if facts.factory_dispatches.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.factory_dispatches.clear();
    }
    if facts.includes.len() > MAX_C_FUNCTION_POINTER_FACTS_PER_FILE {
        facts.includes.clear();
    }
    facts
}

fn collect_c_function_pointer_typedefs(
    node: Node<'_>,
    source: &[u8],
    output: &mut Vec<CFunctionPointerTypedefFact>,
) {
    if node.kind() == "type_definition" {
        if let Some(declarator) = node.child_by_field_name("declarator") {
            let function = first_descendant_or_self(declarator, "function_declarator");
            if let (Some(function), Some(name)) = (function, c_declarator_name(declarator)) {
                let pointer = first_descendant_or_self(function, "pointer_declarator").is_some();
                output.push(CFunctionPointerTypedefFact {
                    name: node_text(name, source),
                    pointer,
                    line: node.start_position().row + 1,
                    site_start_byte: node.start_byte(),
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_c_function_pointer_typedefs(child, source, output);
    }
}

fn collect_c_struct_layouts(
    node: Node<'_>,
    source: &[u8],
    function_types: &HashMap<String, bool>,
    output: &mut Vec<CStructLayoutFact>,
) {
    if matches!(node.kind(), "struct_specifier" | "class_specifier") {
        if let (Some(type_name), Some(body)) = (
            c_struct_type_name(node, source),
            node.child_by_field_name("body"),
        ) {
            let mut fields = Vec::new();
            let mut cursor = body.walk();
            for declaration in body
                .named_children(&mut cursor)
                .filter(|child| child.kind() == "field_declaration")
            {
                let declared_type = declaration
                    .child_by_field_name("type")
                    .and_then(|type_node| c_type_name(type_node, source));
                let mut declaration_cursor = declaration.walk();
                for declarator in
                    declaration
                        .named_children(&mut declaration_cursor)
                        .filter(|child| {
                            child.id()
                                != declaration
                                    .child_by_field_name("type")
                                    .map(|n| n.id())
                                    .unwrap_or(usize::MAX)
                        })
                {
                    let Some(name) = declarator_name(declarator) else {
                        continue;
                    };
                    let explicit_pointer =
                        first_descendant_or_self(declarator, "function_declarator").is_some_and(
                            |function| {
                                first_descendant_or_self(function, "pointer_declarator").is_some()
                            },
                        );
                    let typed_pointer = declared_type.as_ref().is_some_and(|name| {
                        function_types.get(name).is_some_and(|typedef_pointer| {
                            *typedef_pointer
                                || first_descendant_or_self(declarator, "pointer_declarator")
                                    .is_some()
                        })
                    });
                    fields.push(CStructFieldFact {
                        name: node_text(name, source),
                        index: fields.len(),
                        value_type: (!explicit_pointer && !typed_pointer)
                            .then_some(declared_type.clone())
                            .flatten(),
                        function_pointer: explicit_pointer || typed_pointer,
                    });
                }
            }
            if !fields.is_empty() {
                output.push(CStructLayoutFact {
                    type_name,
                    fields,
                    line: node.start_position().row + 1,
                    site_start_byte: node.start_byte(),
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_c_struct_layouts(child, source, function_types, output);
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_c_function_pointer_observations(
    node: Node<'_>,
    source: &[u8],
    language: Language,
    owners: &HashMap<(usize, usize), String>,
    file_symbol_id: &str,
    callable_names: &std::collections::HashSet<String>,
    function_fields: &std::collections::HashSet<String>,
    function_types: &HashMap<String, bool>,
    facts: &mut CFunctionPointerFacts,
) {
    if node.kind() == "preproc_include" {
        if let Some(path) = node
            .child_by_field_name("path")
            .filter(|path| path.kind() == "string_literal")
            .map(|path| node_text(path, source))
            .and_then(|path| {
                path.strip_prefix('"')
                    .and_then(|path| path.strip_suffix('"'))
                    .map(str::to_owned)
            })
        {
            facts.includes.push(CIncludeFact {
                path,
                line: node.start_position().row + 1,
                site_start_byte: node.start_byte(),
            });
        }
    } else if node.kind() == "assignment_expression" {
        if let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            if language == Language::Cpp && left.kind() == "identifier" {
                let target_name = c_address_of_callable_reference(right, source, callable_names);
                let factory_name = c_local_pointer_factory_call(right, source);
                if target_name.is_some()
                    || factory_name.is_some()
                    || c_local_pointer_name_has_declaration(
                        &node_text(left, source),
                        node,
                        &facts.local_bindings,
                    )
                {
                    let (scope_start_byte, scope_end_byte) =
                        receiver_enclosing_callable_scope(node);
                    facts.local_bindings.push(CLocalFunctionPointerBindingFact {
                        owner_id: owning_symbol_id(node, owners)
                            .cloned()
                            .unwrap_or_else(|| file_symbol_id.to_owned()),
                        local_name: node_text(left, source),
                        target_name,
                        factory_name,
                        declares_binding: false,
                        conditional: c_write_is_conditional(node),
                        scope_start_byte,
                        scope_end_byte,
                        line: node.start_position().row + 1,
                        site_start_byte: node.start_byte(),
                    });
                }
            }
            if let (Some(mut path), Some(parameter_name)) = (
                c_field_path(left, source),
                (right.kind() == "identifier").then(|| node_text(right, source)),
            ) {
                if let (Some(field_name), Some(parameter_index)) =
                    (path.pop(), c_parameter_index(node, &parameter_name, source))
                {
                    facts
                        .formal_storages
                        .push(CFunctionPointerFormalStorageFact {
                            owner_id: owning_symbol_id(node, owners)
                                .cloned()
                                .unwrap_or_else(|| file_symbol_id.to_owned()),
                            parameter_index,
                            receiver_type: c_receiver_type(
                                node,
                                path.first().map(String::as_str),
                                source,
                            ),
                            receiver_path: path,
                            field_name,
                            line: node.start_position().row + 1,
                            site_start_byte: node.start_byte(),
                        });
                }
            }
            if let (Some(mut target_path), Some(mut source_path)) =
                (c_field_path(left, source), c_field_path(right, source))
            {
                if target_path.len() >= 2 && source_path.len() >= 2 {
                    let target_field_name = target_path.pop().expect("checked field path");
                    let source_field_name = source_path.pop().expect("checked field path");
                    facts.propagations.push(CFunctionPointerPropagationFact {
                        target_receiver_type: c_receiver_type(
                            node,
                            target_path.first().map(String::as_str),
                            source,
                        ),
                        target_receiver_path: target_path,
                        target_field_name,
                        source_receiver_type: c_receiver_type(
                            node,
                            source_path.first().map(String::as_str),
                            source,
                        ),
                        source_receiver_path: source_path,
                        source_field_name,
                        line: node.start_position().row + 1,
                        site_start_byte: node.start_byte(),
                    });
                }
            }
            if let (Some(mut path), Some(target_name)) = (
                c_field_path(left, source),
                c_callable_reference(right, source, callable_names),
            ) {
                if let Some(field_name) = path.pop() {
                    let receiver_type =
                        c_receiver_type(node, path.first().map(String::as_str), source);
                    facts.bindings.push(CFunctionPointerBindingFact {
                        owner_id: owning_symbol_id(node, owners)
                            .cloned()
                            .unwrap_or_else(|| file_symbol_id.to_owned()),
                        receiver_type,
                        receiver_path: path,
                        field_name: Some(field_name),
                        field_index: None,
                        target_name,
                        line: node.start_position().row + 1,
                        site_start_byte: node.start_byte(),
                    });
                }
            }
        }
    } else if node.kind() == "return_statement" {
        if c_enclosing_function_returns_pointer(node, source, function_types) {
            if let Some(value) = node
                .child_by_field_name("value")
                .or_else(|| node.named_child(0))
            {
                let owner_id = owning_symbol_id(node, owners)
                    .cloned()
                    .unwrap_or_else(|| file_symbol_id.to_owned());
                let mut targets = Vec::new();
                c_returned_callable_references(value, source, callable_names, &mut targets);
                targets.retain(|target| {
                    c_parameter_index(node, target, source).is_none()
                        && !c_local_declaration_precedes(node, target, source)
                });
                targets.sort();
                targets.dedup();
                for target_name in targets {
                    facts.returns.push(CFunctionPointerReturnFact {
                        owner_id: owner_id.clone(),
                        target_name,
                        line: node.start_position().row + 1,
                        site_start_byte: node.start_byte(),
                    });
                }
            }
        }
    } else if node.kind() == "declaration" {
        if language == Language::Cpp {
            collect_c_local_function_pointer_declarations(
                node,
                source,
                owners,
                file_symbol_id,
                callable_names,
                &mut facts.local_bindings,
            );
        }
        collect_c_function_pointer_arrays(node, source, callable_names, &mut facts.arrays);
        collect_c_initializer_bindings(
            node,
            source,
            owners,
            file_symbol_id,
            callable_names,
            &mut facts.bindings,
        );
    } else if node.kind() == "call_expression" {
        if let Some(function) = node.child_by_field_name("function") {
            if function.kind() == "identifier" {
                if language == Language::Cpp {
                    let (scope_start_byte, scope_end_byte) = receiver_binding_scope(node, true);
                    facts
                        .local_dispatches
                        .push(CLocalFunctionPointerDispatchFact {
                            owner_id: owning_symbol_id(node, owners)
                                .cloned()
                                .unwrap_or_else(|| file_symbol_id.to_owned()),
                            local_name: node_text(function, source),
                            scope_start_byte,
                            scope_end_byte,
                            line: node.start_position().row + 1,
                            site_start_byte: node.start_byte(),
                        });
                }
                if let Some(arguments) = node.child_by_field_name("arguments") {
                    let mut argument_cursor = arguments.walk();
                    for (argument_index, argument) in
                        arguments.named_children(&mut argument_cursor).enumerate()
                    {
                        if let Some(target_name) =
                            c_callable_reference(argument, source, callable_names)
                        {
                            facts.arguments.push(CFunctionPointerArgumentFact {
                                caller_id: owning_symbol_id(node, owners)
                                    .cloned()
                                    .unwrap_or_else(|| file_symbol_id.to_owned()),
                                callee_name: node_text(function, source),
                                argument_index,
                                target_name,
                                line: argument.start_position().row + 1,
                                site_start_byte: argument.start_byte(),
                            });
                        }
                    }
                }
            }
            if function.kind() == "subscript_expression" {
                if let Some(name) = function
                    .child_by_field_name("argument")
                    .filter(|argument| argument.kind() == "identifier")
                    .map(|argument| node_text(argument, source))
                {
                    facts
                        .array_dispatches
                        .push(CFunctionPointerArrayDispatchFact {
                            owner_id: owning_symbol_id(node, owners)
                                .cloned()
                                .unwrap_or_else(|| file_symbol_id.to_owned()),
                            name,
                            line: node.start_position().row + 1,
                            site_start_byte: node.start_byte(),
                        });
                }
            }
            if language == Language::Cpp && function.kind() == "call_expression" {
                if let Some(factory_name) = function
                    .child_by_field_name("function")
                    .filter(|callee| callee.kind() == "identifier")
                    .map(|callee| node_text(callee, source))
                {
                    facts
                        .factory_dispatches
                        .push(CFunctionPointerFactoryDispatchFact {
                            owner_id: owning_symbol_id(node, owners)
                                .cloned()
                                .unwrap_or_else(|| file_symbol_id.to_owned()),
                            factory_name,
                            line: node.start_position().row + 1,
                            site_start_byte: node.start_byte(),
                        });
                }
            }
            if function.kind() == "field_expression" {
                if let Some(mut path) = c_field_path(function, source) {
                    if let Some(field_name) = path.pop() {
                        let receiver_type =
                            c_receiver_type(node, path.first().map(String::as_str), source);
                        facts.dispatches.push(CFunctionPointerDispatchFact {
                            owner_id: owning_symbol_id(node, owners)
                                .cloned()
                                .unwrap_or_else(|| file_symbol_id.to_owned()),
                            receiver_type,
                            receiver_path: path,
                            proven_function_pointer: language == Language::C
                                || function_fields.contains(&field_name),
                            field_name,
                            line: node.start_position().row + 1,
                            site_start_byte: node.start_byte(),
                        });
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_c_function_pointer_observations(
            child,
            source,
            language,
            owners,
            file_symbol_id,
            callable_names,
            function_fields,
            function_types,
            facts,
        );
    }
}

fn c_enclosing_function_returns_pointer(
    node: Node<'_>,
    source: &[u8],
    function_types: &HashMap<String, bool>,
) -> bool {
    let mut ancestor = node.parent();
    while let Some(function) = ancestor {
        if function.kind() == "lambda_expression" {
            return false;
        }
        if function.kind() == "function_definition" {
            let type_is_pointer = function
                .child_by_field_name("type")
                .map(|kind| c_type_name(kind, source).unwrap_or_else(|| node_text(kind, source)))
                .is_some_and(|name| {
                    name == "auto" || function_types.get(&name).copied().unwrap_or(false)
                });
            let explicit_pointer_return =
                function
                    .child_by_field_name("declarator")
                    .is_some_and(|declarator| {
                        declarator.kind() == "function_declarator"
                            && declarator.child_by_field_name("declarator").is_some_and(
                                |returned_declarator| {
                                    first_descendant_or_self(
                                        returned_declarator,
                                        "pointer_declarator",
                                    )
                                    .is_some()
                                        && first_descendant_or_self(
                                            returned_declarator,
                                            "function_declarator",
                                        )
                                        .is_some()
                                },
                            )
                    });
            return type_is_pointer || explicit_pointer_return;
        }
        ancestor = function.parent();
    }
    false
}

fn collect_c_initializer_bindings(
    declaration: Node<'_>,
    source: &[u8],
    owners: &HashMap<(usize, usize), String>,
    file_symbol_id: &str,
    callable_names: &std::collections::HashSet<String>,
    output: &mut Vec<CFunctionPointerBindingFact>,
) {
    let receiver_type = declaration
        .child_by_field_name("type")
        .and_then(|node| c_type_name(node, source));
    let mut cursor = declaration.walk();
    for initializer in declaration
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "init_declarator")
    {
        let (Some(declarator), Some(value)) = (
            initializer.child_by_field_name("declarator"),
            initializer.child_by_field_name("value"),
        ) else {
            continue;
        };
        let Some(binding_name) = declarator_name(declarator).map(|name| node_text(name, source))
        else {
            continue;
        };
        if value.kind() != "initializer_list" {
            continue;
        }
        let array = first_descendant_or_self(declarator, "array_declarator").is_some();
        if array {
            let mut value_cursor = value.walk();
            for element in value
                .named_children(&mut value_cursor)
                .filter(|element| element.kind() == "initializer_list")
            {
                collect_c_initializer_element(
                    element,
                    &binding_name,
                    receiver_type.as_deref(),
                    source,
                    owners,
                    file_symbol_id,
                    callable_names,
                    output,
                );
            }
        } else {
            collect_c_initializer_element(
                value,
                &binding_name,
                receiver_type.as_deref(),
                source,
                owners,
                file_symbol_id,
                callable_names,
                output,
            );
        }
    }
}

fn collect_c_function_pointer_arrays(
    declaration: Node<'_>,
    source: &[u8],
    callable_names: &std::collections::HashSet<String>,
    output: &mut Vec<CFunctionPointerArrayFact>,
) {
    let Some(element_type) = declaration
        .child_by_field_name("type")
        .and_then(|node| c_type_name(node, source))
    else {
        return;
    };
    let mut cursor = declaration.walk();
    for initializer in declaration
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "init_declarator")
    {
        let (Some(declarator), Some(value)) = (
            initializer.child_by_field_name("declarator"),
            initializer.child_by_field_name("value"),
        ) else {
            continue;
        };
        if value.kind() != "initializer_list"
            || first_descendant_or_self(declarator, "array_declarator").is_none()
        {
            continue;
        }
        let Some(name) = declarator_name(declarator).map(|name| node_text(name, source)) else {
            continue;
        };
        let mut targets = Vec::new();
        let mut value_cursor = value.walk();
        for entry in value.named_children(&mut value_cursor) {
            let target = if entry.kind() == "initializer_pair" {
                entry.child_by_field_name("value")
            } else {
                Some(entry)
            };
            let Some((target, target_name)) = target.and_then(|target| {
                c_callable_reference(target, source, callable_names).map(|name| (target, name))
            }) else {
                continue;
            };
            targets.push(CFunctionPointerArrayTargetFact {
                target_name,
                line: target.start_position().row + 1,
                site_start_byte: target.start_byte(),
            });
        }
        if targets.is_empty() {
            continue;
        }
        output.push(CFunctionPointerArrayFact {
            name,
            element_type: element_type.clone(),
            pointer_declarator: first_descendant_or_self(declarator, "pointer_declarator")
                .is_some(),
            targets,
            line: declaration.start_position().row + 1,
            site_start_byte: declaration.start_byte(),
        });
    }
}

fn collect_c_local_function_pointer_declarations(
    declaration: Node<'_>,
    source: &[u8],
    owners: &HashMap<(usize, usize), String>,
    file_symbol_id: &str,
    callable_names: &std::collections::HashSet<String>,
    output: &mut Vec<CLocalFunctionPointerBindingFact>,
) {
    let mut cursor = declaration.walk();
    for initializer in declaration
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "init_declarator")
    {
        let (Some(declarator), Some(value)) = (
            initializer.child_by_field_name("declarator"),
            initializer.child_by_field_name("value"),
        ) else {
            continue;
        };
        let Some(local_name) = declarator_name(declarator).map(|name| node_text(name, source))
        else {
            continue;
        };
        let target_name = c_address_of_callable_reference(value, source, callable_names);
        let factory_name = c_local_pointer_factory_call(value, source);
        if target_name.is_none() && factory_name.is_none() {
            continue;
        }
        output.push(CLocalFunctionPointerBindingFact {
            owner_id: owning_symbol_id(initializer, owners)
                .cloned()
                .unwrap_or_else(|| file_symbol_id.to_owned()),
            local_name,
            target_name,
            factory_name,
            declares_binding: true,
            conditional: false,
            scope_start_byte: receiver_binding_scope(initializer, true).0,
            scope_end_byte: receiver_binding_scope(initializer, true).1,
            line: initializer.start_position().row + 1,
            site_start_byte: initializer.start_byte(),
        });
    }
}

fn c_local_pointer_factory_call(node: Node<'_>, source: &[u8]) -> Option<String> {
    (node.kind() == "call_expression")
        .then(|| node.child_by_field_name("function"))
        .flatten()
        .filter(|function| function.kind() == "identifier")
        .map(|function| node_text(function, source))
}

fn c_returned_callable_references(
    node: Node<'_>,
    source: &[u8],
    callable_names: &std::collections::HashSet<String>,
    output: &mut Vec<String>,
) {
    if let Some(target) = c_address_of_callable_reference(node, source, callable_names) {
        output.push(target);
        return;
    }
    if node.kind() == "conditional_expression" {
        for field in ["consequence", "alternative"] {
            if let Some(child) = node.child_by_field_name(field) {
                c_returned_callable_references(child, source, callable_names, output);
            }
        }
    } else if node.kind() == "parenthesized_expression" {
        if let Some(child) = node.named_child(0) {
            c_returned_callable_references(child, source, callable_names, output);
        }
    }
}

fn c_local_declaration_precedes(node: Node<'_>, name: &str, source: &[u8]) -> bool {
    fn contains_declaration(current: Node<'_>, before: usize, name: &str, source: &[u8]) -> bool {
        if current.start_byte() >= before {
            return false;
        }
        if current.kind() == "declaration" {
            let type_id = current
                .child_by_field_name("type")
                .map(|type_node| type_node.id());
            let mut cursor = current.walk();
            if current.named_children(&mut cursor).any(|child| {
                Some(child.id()) != type_id
                    && declarator_name(child)
                        .is_some_and(|candidate| node_text(candidate, source) == name)
            }) {
                return true;
            }
        }
        let mut cursor = current.walk();
        let found = current
            .named_children(&mut cursor)
            .any(|child| contains_declaration(child, before, name, source));
        found
    }

    let mut ancestor = node.parent();
    while let Some(function) = ancestor {
        if function.kind() == "function_definition" {
            return function
                .child_by_field_name("body")
                .is_some_and(|body| contains_declaration(body, node.start_byte(), name, source));
        }
        ancestor = function.parent();
    }
    false
}

fn c_local_pointer_name_has_declaration(
    name: &str,
    node: Node<'_>,
    bindings: &[CLocalFunctionPointerBindingFact],
) -> bool {
    let position = node.start_byte();
    bindings.iter().any(|binding| {
        binding.declares_binding
            && binding.local_name == name
            && binding.site_start_byte < position
            && binding.scope_start_byte <= position
            && position < binding.scope_end_byte
    })
}

fn c_write_is_conditional(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if matches!(
            current.kind(),
            "if_statement"
                | "switch_statement"
                | "switch_case"
                | "for_statement"
                | "for_range_loop"
                | "while_statement"
                | "do_statement"
                | "conditional_expression"
                | "try_statement"
                | "catch_clause"
        ) {
            return true;
        }
        if matches!(
            current.kind(),
            "function_definition"
                | "function_declaration"
                | "function_expression"
                | "lambda_expression"
        ) {
            break;
        }
        ancestor = current.parent();
    }
    false
}

fn local_pointer_declaration_at(
    dispatch: &CLocalFunctionPointerDispatchFact,
    facts: &CFunctionPointerFacts,
) -> bool {
    facts.local_bindings.iter().any(|binding| {
        binding.declares_binding
            && binding.owner_id == dispatch.owner_id
            && binding.local_name == dispatch.local_name
            && binding.site_start_byte < dispatch.site_start_byte
            && binding.scope_start_byte <= dispatch.site_start_byte
            && dispatch.site_start_byte < binding.scope_end_byte
    })
}

fn c_address_of_callable_reference(
    node: Node<'_>,
    source: &[u8],
    callable_names: &std::collections::HashSet<String>,
) -> Option<String> {
    (node.kind() == "pointer_expression" && node_text(node, source).trim_start().starts_with('&'))
        .then(|| c_callable_reference(node, source, callable_names))
        .flatten()
}

#[allow(clippy::too_many_arguments)]
fn collect_c_initializer_element(
    element: Node<'_>,
    binding_name: &str,
    receiver_type: Option<&str>,
    source: &[u8],
    owners: &HashMap<(usize, usize), String>,
    file_symbol_id: &str,
    callable_names: &std::collections::HashSet<String>,
    output: &mut Vec<CFunctionPointerBindingFact>,
) {
    let mut cursor = element.walk();
    for (index, value) in element
        .named_children(&mut cursor)
        .filter(|child| !child.is_extra())
        .enumerate()
    {
        let (field_name, target) = if value.kind() == "initializer_pair" {
            let field_name = value
                .child_by_field_name("designator")
                .and_then(|designator| declarator_name(designator))
                .map(|name| node_text(name, source));
            (field_name, value.child_by_field_name("value"))
        } else {
            (None, Some(value))
        };
        let Some(target_name) =
            target.and_then(|target| c_callable_reference(target, source, callable_names))
        else {
            continue;
        };
        output.push(CFunctionPointerBindingFact {
            owner_id: owning_symbol_id(element, owners)
                .cloned()
                .unwrap_or_else(|| file_symbol_id.to_owned()),
            receiver_type: receiver_type.map(str::to_owned),
            receiver_path: vec![binding_name.to_owned()],
            field_name: field_name.clone(),
            field_index: field_name.is_none().then_some(index),
            target_name,
            line: value.start_position().row + 1,
            site_start_byte: value.start_byte(),
        });
    }
}

fn c_callable_reference(
    mut node: Node<'_>,
    source: &[u8],
    callable_names: &std::collections::HashSet<String>,
) -> Option<String> {
    while matches!(
        node.kind(),
        "pointer_expression" | "parenthesized_expression" | "cast_expression"
    ) {
        node = node
            .child_by_field_name("argument")
            .or_else(|| node.child_by_field_name("value"))
            .or_else(|| node.named_child(node.named_child_count().checked_sub(1)?))?;
    }
    if !matches!(
        node.kind(),
        "identifier" | "qualified_identifier" | "template_function"
    ) {
        return None;
    }
    let raw = node_text(node, source);
    let name = raw
        .rsplit("::")
        .next()
        .unwrap_or(&raw)
        .split('<')
        .next()
        .unwrap_or(&raw)
        .to_owned();
    callable_names.contains(&name).then_some(name)
}

fn c_field_path(node: Node<'_>, source: &[u8]) -> Option<Vec<String>> {
    if node.kind() == "identifier" {
        return Some(vec![node_text(node, source)]);
    }
    if node.kind() != "field_expression" {
        return None;
    }
    let argument = node.child_by_field_name("argument")?;
    let field = node.child_by_field_name("field")?;
    let mut path = c_field_path(argument, source)?;
    if path.len() >= MAX_C_RECEIVER_PATH_DEPTH {
        return None;
    }
    path.push(node_text(field, source));
    Some(path)
}

fn c_receiver_type(node: Node<'_>, receiver: Option<&str>, source: &[u8]) -> Option<String> {
    let receiver = receiver?;
    let site = node.start_byte();
    let mut callable = node.parent();
    while let Some(ancestor) = callable {
        if ancestor.kind() == "function_definition" {
            return c_declared_type_in(ancestor, receiver, site, source);
        }
        callable = ancestor.parent();
    }
    None
}

fn c_parameter_index(node: Node<'_>, parameter_name: &str, source: &[u8]) -> Option<usize> {
    let mut callable = node.parent();
    while let Some(ancestor) = callable {
        if ancestor.kind() == "function_definition" {
            let declarator = ancestor.child_by_field_name("declarator")?;
            let function = first_descendant_or_self(declarator, "function_declarator")?;
            let parameters = function.child_by_field_name("parameters")?;
            let mut cursor = parameters.walk();
            return parameters
                .named_children(&mut cursor)
                .filter(|parameter| parameter.kind() == "parameter_declaration")
                .enumerate()
                .find_map(|(index, parameter)| {
                    declarator_name(parameter)
                        .filter(|name| node_text(*name, source) == parameter_name)
                        .map(|_| index)
                });
        }
        callable = ancestor.parent();
    }
    None
}

fn c_declared_type_in(
    node: Node<'_>,
    receiver: &str,
    site: usize,
    source: &[u8],
) -> Option<String> {
    if node.start_byte() >= site {
        return None;
    }
    if matches!(node.kind(), "parameter_declaration" | "declaration") {
        let declared_type = node
            .child_by_field_name("type")
            .and_then(|type_node| c_type_name(type_node, source));
        if declared_type.is_some() {
            let mut cursor = node.walk();
            if node
                .named_children(&mut cursor)
                .filter(|child| {
                    child.id()
                        != node
                            .child_by_field_name("type")
                            .map(|n| n.id())
                            .unwrap_or(usize::MAX)
                })
                .filter_map(declarator_name)
                .any(|name| node_text(name, source) == receiver)
            {
                return declared_type;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "function_definition" && child.start_byte() != node.start_byte() {
            continue;
        }
        if let Some(found) = c_declared_type_in(child, receiver, site, source) {
            return Some(found);
        }
    }
    None
}

fn c_struct_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .map(|name| node_text(name, source))
        .or_else(|| {
            node.parent()
                .filter(|parent| parent.kind() == "type_definition")
                .and_then(|parent| parent.child_by_field_name("declarator"))
                .and_then(c_declarator_name)
                .map(|name| node_text(name, source))
        })
}

fn c_declarator_name(node: Node<'_>) -> Option<Node<'_>> {
    if matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier"
    ) {
        return Some(node);
    }
    if let Some(declarator) = node.child_by_field_name("declarator") {
        if let Some(name) = c_declarator_name(declarator) {
            return Some(name);
        }
    }
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).find_map(c_declarator_name);
    found
}

fn c_type_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "type_identifier" | "primitive_type" => Some(node_text(node, source)),
        "struct_specifier" | "class_specifier" => node
            .child_by_field_name("name")
            .map(|name| node_text(name, source)),
        _ => declarator_name(node).map(|name| node_text(name, source)),
    }
    .filter(|name| !name.is_empty())
}

fn first_descendant_or_self<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| first_descendant_or_self(child, kind));
    found
}

struct CallCollectionContext<'a> {
    source: &'a [u8],
    language: Language,
    file: &'a str,
    symbol_owners: &'a HashMap<(usize, usize), String>,
    receiver_bindings: &'a ReceiverBindingMap,
    module_bindings: &'a HashMap<String, String>,
    project_names: &'a std::collections::HashSet<&'a str>,
    inline_callback_symbols: &'a HashMap<(usize, usize), Symbol>,
    inline_callback_ids: &'a std::collections::HashSet<String>,
    indirect_call_sites: &'a std::collections::HashSet<usize>,
    file_symbol_id: &'a str,
}

struct ReceiverBinding {
    position: usize,
    requires_prior_declaration: bool,
    receiver_type: Option<String>,
}

type ReceiverBindingMap = HashMap<(String, usize, usize), Vec<ReceiverBinding>>;
type FactoryReturnMap = HashMap<(String, usize, usize), Vec<ReceiverBinding>>;
type CollectionElementMap = HashMap<(String, usize, usize), Vec<ReceiverBinding>>;

const MAX_PYTHON_CALLBACK_FORMALS: usize = 64;
const MAX_PYTHON_CALL_ARGUMENTS: usize = 64;

fn collect_python_callback_formals(
    node: Node<'_>,
    source: &[u8],
    owners: &HashMap<(usize, usize), String>,
) -> Vec<PythonCallbackFormalFact> {
    fn visit(
        node: Node<'_>,
        source: &[u8],
        owners: &HashMap<(usize, usize), String>,
        output: &mut Vec<PythonCallbackFormalFact>,
    ) {
        if node.kind() == "function_definition"
            && node
                .parent()
                .is_none_or(|parent| parent.kind() != "decorated_definition")
        {
            if let (Some(owner_id), Some(parameters)) = (
                owners.get(&(node.start_byte(), node.end_byte())),
                node.child_by_field_name("parameters"),
            ) {
                let positional_separator = python_positional_separator_byte(parameters);
                let mut cursor = parameters.walk();
                let children = parameters
                    .named_children(&mut cursor)
                    .filter(|parameter| !parameter.is_extra())
                    .take(MAX_PYTHON_CALLBACK_FORMALS + 1)
                    .collect::<Vec<_>>();
                if children.len() <= MAX_PYTHON_CALLBACK_FORMALS {
                    let mut names = std::collections::HashSet::new();
                    for (parameter_index, parameter) in children.into_iter().enumerate() {
                        if positional_separator
                            .is_some_and(|separator| parameter.start_byte() < separator)
                            || parameter_contains_variadic_form(parameter)
                        {
                            continue;
                        }
                        let name = if parameter.kind() == "identifier" {
                            Some(node_text(parameter, source))
                        } else {
                            parameter
                                .child_by_field_name("name")
                                .or_else(|| parameter.child_by_field_name("pattern"))
                                .filter(|name| name.kind() == "identifier")
                                .map(|name| node_text(name, source))
                                .or_else(|| {
                                    let mut children = parameter.walk();
                                    let name = parameter
                                        .named_children(&mut children)
                                        .find(|child| child.kind() == "identifier")
                                        .map(|name| node_text(name, source));
                                    name
                                })
                        };
                        if let Some(formal_name) =
                            name.filter(|name| !name.is_empty() && names.insert(name.clone()))
                        {
                            output.push(PythonCallbackFormalFact {
                                owner_id: owner_id.clone(),
                                formal_name,
                                parameter_index,
                            });
                        }
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            visit(child, source, owners, output);
        }
    }

    let mut output = Vec::new();
    visit(node, source, owners, &mut output);
    output
}

fn python_positional_separator_byte(parameters: Node<'_>) -> Option<usize> {
    (0..parameters.child_count())
        .filter_map(|index| parameters.child(index))
        .find_map(|child| {
            if !child.is_named() && child.kind() == "/" {
                return Some(child.start_byte());
            }
            (child.kind() == "positional_separator").then(|| {
                (0..child.child_count())
                    .filter_map(|index| child.child(index))
                    .find(|token| !token.is_named() && token.kind() == "/")
                    .map_or(child.start_byte(), |token| token.start_byte())
            })
        })
}

fn python_keyword_argument<'tree>(
    argument: Node<'tree>,
    source: &[u8],
) -> Option<(String, Node<'tree>)> {
    if argument.kind() != "keyword_argument" {
        return None;
    }
    let name = argument.child_by_field_name("name")?;
    let value = argument.child_by_field_name("value")?;
    (name.kind() == "identifier").then(|| (node_text(name, source), value))
}

fn python_keyword_mapping_unsafe(arguments: Node<'_>, source: &[u8]) -> bool {
    let mut cursor = arguments.walk();
    let mut names = std::collections::HashSet::new();
    let unsafe_mapping = arguments
        .named_children(&mut cursor)
        .filter(|argument| !argument.is_extra())
        .take(MAX_PYTHON_CALL_ARGUMENTS + 1)
        .any(|argument| {
            matches!(argument.kind(), "dictionary_splat" | "ERROR")
                || python_keyword_argument(argument, source)
                    .is_some_and(|(name, _)| !names.insert(name))
        });
    unsafe_mapping
}

fn python_call_exceeds_argument_cap(arguments: Node<'_>) -> bool {
    let mut cursor = arguments.walk();
    arguments
        .named_children(&mut cursor)
        .filter(|argument| !argument.is_extra())
        .take(MAX_PYTHON_CALL_ARGUMENTS + 1)
        .count()
        > MAX_PYTHON_CALL_ARGUMENTS
}

struct InlineCallbackCollection<'a> {
    source: &'a [u8],
    language: Language,
    file: &'a str,
    symbol_owners: &'a mut HashMap<(usize, usize), String>,
    known_symbols: HashMap<String, Symbol>,
    ordinals: HashMap<(String, String, usize), usize>,
    output: HashMap<(usize, usize), Symbol>,
}

impl InlineCallbackCollection<'_> {
    fn visit(&mut self, node: Node<'_>) {
        if matches!(
            node.kind(),
            "call_expression"
                | "call"
                | "method_invocation"
                | "invocation_expression"
                | "function_call_expression"
                | "member_call_expression"
                | "nullsafe_member_call_expression"
                | "scoped_call_expression"
                | "function_call"
                | "object_creation_expression"
                | "new_expression"
                | "arkui_component_expression"
        ) {
            if let Some(callee_name) =
                call_target_node(node).and_then(|target| call_name(target, self.source))
            {
                let selector = call_target_node(node)
                    .map(|target| {
                        node_text(target, self.source)
                            .split_whitespace()
                            .collect::<String>()
                    })
                    .unwrap_or_else(|| callee_name.clone());
                let caller = owning_symbol_id(node, self.symbol_owners)
                    .and_then(|id| self.known_symbols.get(id))
                    .cloned();
                if let (Some(caller), Some(arguments)) =
                    (caller, node.child_by_field_name("arguments"))
                {
                    if self.language == Language::Python
                        && python_call_exceeds_argument_cap(arguments)
                    {
                        let mut cursor = node.walk();
                        for child in node.named_children(&mut cursor) {
                            self.visit(child);
                        }
                        return;
                    }
                    let unsafe_keywords = self.language == Language::Python
                        && python_keyword_mapping_unsafe(arguments, self.source);
                    let mut cursor = arguments.walk();
                    let mut positional_mapping_unsafe = false;
                    for (argument_index, argument) in arguments
                        .named_children(&mut cursor)
                        .filter(|argument| !argument.is_extra())
                        .enumerate()
                    {
                        let (callback, formal_name) = if self.language == Language::Python
                            && argument.kind() == "keyword_argument"
                        {
                            let Some((name, value)) =
                                python_keyword_argument(argument, self.source)
                            else {
                                continue;
                            };
                            if unsafe_keywords || !is_inline_callback_argument(value, self.language)
                            {
                                continue;
                            }
                            (value, Some(name))
                        } else {
                            if self.language == Language::Python
                                && matches!(
                                    argument.kind(),
                                    "list_splat" | "dictionary_splat" | "generator_expression"
                                )
                            {
                                positional_mapping_unsafe = true;
                                continue;
                            }
                            if positional_mapping_unsafe
                                || !is_inline_callback_argument(argument, self.language)
                            {
                                continue;
                            }
                            (argument, None)
                        };
                        let stable_argument = formal_name
                            .as_deref()
                            .map(|name| format!("keyword:{name}"))
                            .unwrap_or_else(|| format!("position:{argument_index}"));
                        let ordinal_key = formal_name.as_deref().map_or(argument_index, |_| 0);
                        let ordinal = self
                            .ordinals
                            .entry((
                                caller.id.clone(),
                                format!("{selector}|{stable_argument}"),
                                ordinal_key,
                            ))
                            .and_modify(|value| *value += 1)
                            .or_insert(1);
                        let argument_label = formal_name
                            .as_deref()
                            .map(|name| format!("keyword {name}"))
                            .unwrap_or_else(|| format!("argument {}", argument_index + 1));
                        let name =
                            format!("<callback {callee_name} {argument_label} #{}>", *ordinal);
                        let qualified_name = format!("{}.{}", caller.qualified_name, name);
                        let symbol = Symbol::new_disambiguated(
                            self.language,
                            SymbolKind::Function,
                            &name,
                            &qualified_name,
                            self.file,
                            span(callback),
                            &format!(
                                "inline-callback|{}|{}|{}|{}",
                                caller.semantic_key, selector, stable_argument, *ordinal
                            ),
                        );
                        self.symbol_owners.insert(
                            (callback.start_byte(), callback.end_byte()),
                            symbol.id.clone(),
                        );
                        self.known_symbols.insert(symbol.id.clone(), symbol.clone());
                        self.output
                            .insert((callback.start_byte(), callback.end_byte()), symbol);
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child);
        }
    }
}

fn collect_inline_callback_symbols(
    root: Node<'_>,
    source: &[u8],
    language: Language,
    file: &str,
    symbols: &[Symbol],
    symbol_owners: &mut HashMap<(usize, usize), String>,
) -> HashMap<(usize, usize), Symbol> {
    let mut collection = InlineCallbackCollection {
        source,
        language,
        file,
        symbol_owners,
        known_symbols: symbols
            .iter()
            .cloned()
            .map(|symbol| (symbol.id.clone(), symbol))
            .collect(),
        ordinals: HashMap::new(),
        output: HashMap::new(),
    };
    collection.visit(root);
    collection.output
}

fn is_inline_callback_argument(argument: Node<'_>, language: Language) -> bool {
    matches!(argument.kind(), "arrow_function" | "function_expression")
        || (language == Language::Python && argument.kind() == "lambda")
}

fn collect_calls(
    node: Node<'_>,
    context: &CallCollectionContext<'_>,
    output: &mut Vec<UnresolvedCall>,
    callback_invocations: &mut Vec<CallbackParameterInvocation>,
    callback_delegations: &mut Vec<CallbackParameterDelegationFact>,
    callback_arguments: &mut Vec<CallbackArgumentFact>,
) {
    if matches!(
        node.kind(),
        "call_expression"
            | "call"
            | "method_invocation"
            | "invocation_expression"
            | "object_creation_expression"
            | "function_call_expression"
            | "member_call_expression"
            | "nullsafe_member_call_expression"
            | "scoped_call_expression"
            | "function_call"
            | "new_expression"
            | "arkui_component_expression"
    ) {
        if let Some(function) = call_target_node(node) {
            if let Some(name) = call_name(function, context.source) {
                let parameter_invocation =
                    invoked_parameter(node, &name, context.source, context.symbol_owners);
                let resolvable = parameter_invocation.is_none()
                    && !context.indirect_call_sites.contains(&node.start_byte());
                let mut owner = node.parent();
                let mut caller_id = None::<String>;
                let mut declared_fallback = None::<String>;
                while let Some(ancestor) = owner {
                    if let Some(symbol_id) = context
                        .symbol_owners
                        .get(&(ancestor.start_byte(), ancestor.end_byte()))
                    {
                        caller_id.get_or_insert_with(|| symbol_id.clone());
                        if !context.inline_callback_ids.contains(symbol_id) {
                            declared_fallback = Some(symbol_id.clone());
                            break;
                        }
                    }
                    owner = ancestor.parent();
                }
                let caller_id = caller_id.unwrap_or_else(|| context.file_symbol_id.to_owned());
                let fallback_caller_id =
                    context.inline_callback_ids.contains(&caller_id).then(|| {
                        declared_fallback.unwrap_or_else(|| context.file_symbol_id.to_owned())
                    });
                output.push(UnresolvedCall {
                    caller_id: caller_id.clone(),
                    fallback_caller_id,
                    callee_name: name.clone(),
                    receiver_binding: call_receiver_name(function, context.source),
                    receiver_type: receiver_type_hint(
                        function,
                        node,
                        context.source,
                        context.receiver_bindings,
                    ),
                    receiver_call_start_byte: call_result_receiver(function, context),
                    target_file_hint: call_receiver_name(function, context.source)
                        .and_then(|receiver| context.module_bindings.get(&receiver).cloned()),
                    provenance: "tree-sitter/name-resolution".to_owned(),
                    confidence: 1.0,
                    explanation: "direct call expression".to_owned(),
                    resolvable,
                    file: context.file.to_owned(),
                    line: node.start_position().row + 1,
                    start_byte: node.start_byte(),
                });
                if let Some(invocation) = parameter_invocation {
                    if !callback_invocations.iter().any(|candidate| {
                        candidate.owner_id == invocation.owner_id
                            && candidate.parameter_index == invocation.parameter_index
                    }) {
                        callback_invocations.push(invocation);
                    }
                } else {
                    collect_callback_arguments(
                        node,
                        &name,
                        &caller_id,
                        context,
                        callback_delegations,
                        callback_arguments,
                    );
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(
            child,
            context,
            output,
            callback_invocations,
            callback_delegations,
            callback_arguments,
        );
    }
}

fn call_result_receiver(function: Node<'_>, context: &CallCollectionContext<'_>) -> Option<usize> {
    let receiver = function
        .child_by_field_name("object")
        .or_else(|| function.child_by_field_name("operand"))
        .or_else(|| function.child_by_field_name("value"))?;
    if receiver.kind() != "call_expression" {
        return None;
    }
    if context.language == Language::ArkTs {
        let receiver_name =
            call_target_node(receiver).and_then(|target| call_name(target, context.source));
        if receiver_name
            .as_deref()
            .is_some_and(is_arkui_intrinsic_name)
            && receiver_name
                .as_deref()
                .is_none_or(|name| !context.project_names.contains(name))
        {
            return None;
        }
    }
    Some(receiver.start_byte())
}

fn collect_module_bindings(
    node: Node<'_>,
    source: &[u8],
    language: Language,
) -> HashMap<String, String> {
    let mut bindings = HashMap::new();
    if language == Language::Go {
        collect_go_import_bindings(node, source, &mut bindings);
    }
    bindings
}

fn collect_go_import_bindings(node: Node<'_>, source: &[u8], output: &mut HashMap<String, String>) {
    if node.kind() == "import_spec" {
        if let Some(path_node) = node
            .child_by_field_name("path")
            .or_else(|| node.named_child(node.named_child_count().saturating_sub(1)))
        {
            let path = node_text(path_node, source)
                .trim_matches(['`', '"'])
                .to_owned();
            let alias = node
                .child_by_field_name("name")
                .map(|name| node_text(name, source))
                .unwrap_or_else(|| path.rsplit('/').next().unwrap_or_default().to_owned());
            if !path.is_empty() && !alias.is_empty() && alias != "_" && alias != "." {
                output.insert(alias, path);
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_go_import_bindings(child, source, output);
    }
}

fn call_receiver_name(function: Node<'_>, source: &[u8]) -> Option<String> {
    let receiver = function
        .child_by_field_name("object")
        .or_else(|| function.child_by_field_name("operand"))
        .or_else(|| function.child_by_field_name("value"))?;
    matches!(receiver.kind(), "identifier" | "package_identifier")
        .then(|| node_text(receiver, source))
}

fn invoked_parameter(
    call: Node<'_>,
    name: &str,
    source: &[u8],
    symbol_owners: &HashMap<(usize, usize), String>,
) -> Option<CallbackParameterInvocation> {
    let mut ancestor = call.parent();
    while let Some(node) = ancestor {
        if matches!(
            node.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "lambda"
                | "method_definition"
                | "function_definition"
        ) {
            if let Some(parameters) = node.child_by_field_name("parameters") {
                let parameter_names = if node.kind() == "function_definition" {
                    python_direct_parameter_names(parameters, source)
                } else {
                    direct_parameter_names(parameters, source)
                };
                if let Some(parameter_index) = parameter_names
                    .iter()
                    .position(|parameter| parameter.as_deref() == Some(name))
                {
                    let owner_id = symbol_owners
                        .get(&(node.start_byte(), node.end_byte()))
                        .cloned()?;
                    return Some(CallbackParameterInvocation {
                        owner_id,
                        parameter_index,
                    });
                }
                if parameter_tree_declares_name(parameters, name, source) {
                    return None;
                }
            }
        }
        ancestor = node.parent();
    }
    None
}

const MAX_STORED_CALLBACK_FIELDS_PER_CLASS: usize = 64;
const MAX_STORED_CALLBACK_METHODS_PER_CLASS: usize = 64;
const MAX_STORED_CALLBACK_OPERATIONS_PER_CLASS: usize = 256;

#[derive(Debug)]
struct StoredCallbackField {
    name: String,
    invoked: bool,
    poisoned: bool,
    sources: Vec<(String, usize)>,
}

struct StoredCallbackScan<'a> {
    source: &'a [u8],
    parameter_names: &'a [Option<String>],
    owner_id: &'a str,
    fields: &'a mut [StoredCallbackField],
    operations: &'a mut usize,
    assigned_fields: &'a mut std::collections::HashSet<String>,
}

fn collect_stored_callback_parameter_invocations(
    node: Node<'_>,
    source: &[u8],
    symbol_owners: &HashMap<(usize, usize), String>,
    output: &mut Vec<CallbackParameterInvocation>,
) {
    if matches!(node.kind(), "class_declaration" | "class_definition") {
        collect_class_stored_callback_parameter_invocations(node, source, symbol_owners, output);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_stored_callback_parameter_invocations(child, source, symbol_owners, output);
    }
}

fn collect_class_stored_callback_parameter_invocations(
    class: Node<'_>,
    source: &[u8],
    symbol_owners: &HashMap<(usize, usize), String>,
    output: &mut Vec<CallbackParameterInvocation>,
) {
    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    let mut body_cursor = body.walk();
    let members = body.named_children(&mut body_cursor).collect::<Vec<_>>();
    let method_count = members
        .iter()
        .filter(|member| member.kind() == "method_definition")
        .count();
    if method_count > MAX_STORED_CALLBACK_METHODS_PER_CLASS {
        return;
    }

    let mut fields = members
        .iter()
        .filter(|member| member.kind() == "public_field_definition")
        .filter_map(|field| {
            let name = field.child_by_field_name("name")?;
            let annotation = field.child_by_field_name("type")?;
            callback_compatible_field_type(&node_text(annotation, source))?;
            let poisoned = field
                .child_by_field_name("value")
                .is_some_and(|value| !is_nullish_callback_clear(value, source));
            Some(StoredCallbackField {
                name: node_text(name, source),
                invoked: false,
                poisoned,
                sources: Vec::new(),
            })
        })
        .take(MAX_STORED_CALLBACK_FIELDS_PER_CLASS + 1)
        .collect::<Vec<_>>();
    if fields.is_empty() || fields.len() > MAX_STORED_CALLBACK_FIELDS_PER_CLASS {
        return;
    }

    let mut operations = 0usize;
    for method in members
        .into_iter()
        .filter(|member| member.kind() == "method_definition")
    {
        let Some(parameters) = method.child_by_field_name("parameters") else {
            continue;
        };
        let parameter_names = direct_parameter_names(parameters, source);
        let Some(owner_id) = symbol_owners
            .get(&(method.start_byte(), method.end_byte()))
            .cloned()
        else {
            continue;
        };
        let mut assigned_fields = std::collections::HashSet::new();
        let mut scan = StoredCallbackScan {
            source,
            parameter_names: &parameter_names,
            owner_id: &owner_id,
            fields: &mut fields,
            operations: &mut operations,
            assigned_fields: &mut assigned_fields,
        };
        if !collect_stored_callback_operations(method, method, &mut scan) {
            return;
        }
    }

    for field in fields {
        if field.invoked && !field.poisoned {
            for (owner_id, parameter_index) in field.sources {
                if !output.iter().any(|candidate| {
                    candidate.owner_id == owner_id && candidate.parameter_index == parameter_index
                }) {
                    output.push(CallbackParameterInvocation {
                        owner_id,
                        parameter_index,
                    });
                }
            }
        }
    }
}

fn collect_stored_callback_operations(
    node: Node<'_>,
    method: Node<'_>,
    scan: &mut StoredCallbackScan<'_>,
) -> bool {
    if node.id() != method.id()
        && matches!(
            node.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "class_declaration"
                | "class_definition"
        )
    {
        if !matches!(node.kind(), "class_declaration" | "class_definition") {
            return poison_nested_callback_field_uses(node, scan);
        }
        return true;
    }
    if node.kind() == "assignment_expression" {
        if let Some(name) = node
            .child_by_field_name("left")
            .and_then(|left| direct_this_member(left, scan.source))
            .and_then(|member| member.strip_prefix("this.").map(str::to_owned))
        {
            if let Some(field) = scan.fields.iter_mut().find(|field| field.name == name) {
                *scan.operations += 1;
                if *scan.operations > MAX_STORED_CALLBACK_OPERATIONS_PER_CLASS {
                    return false;
                }
                let right = node.child_by_field_name("right");
                if !is_plain_assignment(node, scan.source) {
                    field.poisoned = true;
                } else if right.is_some_and(|right| is_nullish_callback_clear(right, scan.source)) {
                    if scan.assigned_fields.contains(&name) {
                        field.poisoned = true;
                    }
                } else if let Some(parameter_index) = right
                    .filter(|right| right.kind() == "identifier")
                    .map(|right| node_text(right, scan.source))
                    .and_then(|name| {
                        scan.parameter_names
                            .iter()
                            .position(|parameter| parameter.as_deref() == Some(name.as_str()))
                    })
                {
                    field
                        .sources
                        .push((scan.owner_id.to_owned(), parameter_index));
                    scan.assigned_fields.insert(name);
                } else {
                    field.poisoned = true;
                }
            }
        }
    }
    if node.kind() == "augmented_assignment_expression" {
        if let Some(name) = node
            .child_by_field_name("left")
            .and_then(|left| direct_this_member(left, scan.source))
            .and_then(|member| member.strip_prefix("this.").map(str::to_owned))
        {
            if let Some(field) = scan.fields.iter_mut().find(|field| field.name == name) {
                *scan.operations += 1;
                if *scan.operations > MAX_STORED_CALLBACK_OPERATIONS_PER_CLASS {
                    return false;
                }
                field.poisoned = true;
            }
        }
    }
    if node.kind() == "call_expression" {
        if let Some(name) = node
            .child_by_field_name("function")
            .and_then(|function| direct_this_member(function, scan.source))
            .and_then(|member| member.strip_prefix("this.").map(str::to_owned))
        {
            if let Some(field) = scan.fields.iter_mut().find(|field| field.name == name) {
                *scan.operations += 1;
                if *scan.operations > MAX_STORED_CALLBACK_OPERATIONS_PER_CLASS {
                    return false;
                }
                field.invoked = true;
            }
        }
    }
    if let Some(name) = direct_this_member(node, scan.source)
        .and_then(|member| member.strip_prefix("this.").map(str::to_owned))
    {
        if let Some(field) = scan.fields.iter_mut().find(|field| field.name == name) {
            *scan.operations += 1;
            if *scan.operations > MAX_STORED_CALLBACK_OPERATIONS_PER_CLASS {
                return false;
            }
            if !stored_callback_member_use_is_safe(node, scan.source) {
                field.poisoned = true;
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !collect_stored_callback_operations(child, method, scan) {
            return false;
        }
    }
    true
}

fn poison_nested_callback_field_uses(node: Node<'_>, scan: &mut StoredCallbackScan<'_>) -> bool {
    if matches!(node.kind(), "class_declaration" | "class_definition") {
        return true;
    }
    if let Some(name) = direct_this_member(node, scan.source)
        .and_then(|member| member.strip_prefix("this.").map(str::to_owned))
    {
        if let Some(field) = scan.fields.iter_mut().find(|field| field.name == name) {
            *scan.operations += 1;
            if *scan.operations > MAX_STORED_CALLBACK_OPERATIONS_PER_CLASS {
                return false;
            }
            field.poisoned = true;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !poison_nested_callback_field_uses(child, scan) {
            return false;
        }
    }
    true
}

fn is_plain_assignment(node: Node<'_>, source: &[u8]) -> bool {
    let (Some(left), Some(right)) = (
        node.child_by_field_name("left"),
        node.child_by_field_name("right"),
    ) else {
        return false;
    };
    source
        .get(left.end_byte()..right.start_byte())
        .and_then(|operator| std::str::from_utf8(operator).ok())
        .is_some_and(|operator| operator.trim() == "=")
}

fn stored_callback_member_use_is_safe(member: Node<'_>, source: &[u8]) -> bool {
    let Some(parent) = member.parent() else {
        return false;
    };
    if parent.kind() == "call_expression"
        && parent
            .child_by_field_name("function")
            .is_some_and(|function| function.id() == member.id())
    {
        return true;
    }
    if matches!(
        parent.kind(),
        "assignment_expression" | "augmented_assignment_expression"
    ) && parent
        .child_by_field_name("left")
        .is_some_and(|left| left.id() == member.id())
    {
        return true;
    }
    if parent.kind() == "binary_expression" {
        let left = parent.child_by_field_name("left");
        let right = parent.child_by_field_name("right");
        let other = if left.is_some_and(|left| left.id() == member.id()) {
            right
        } else if right.is_some_and(|right| right.id() == member.id()) {
            left
        } else {
            None
        };
        if other.is_some_and(|other| is_nullish_callback_clear(other, source)) {
            let raw = node_text(parent, source);
            return ["===", "!==", "==", "!="]
                .into_iter()
                .any(|operator| raw.contains(operator));
        }
    }
    if parent.kind() == "unary_expression"
        && node_text(parent, source).trim_start().starts_with("typeof")
    {
        return true;
    }
    stored_callback_truthiness_guard(member, source)
}

fn stored_callback_truthiness_guard(mut node: Node<'_>, source: &[u8]) -> bool {
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "parenthesized_expression" | "unary_expression"
        ) {
            node = parent;
            continue;
        }
        if parent.kind() == "binary_expression" {
            let (Some(left), Some(right)) = (
                parent.child_by_field_name("left"),
                parent.child_by_field_name("right"),
            ) else {
                return false;
            };
            let logical = source
                .get(left.end_byte()..right.start_byte())
                .and_then(|operator| std::str::from_utf8(operator).ok())
                .is_some_and(|operator| matches!(operator.trim(), "&&" | "||"));
            if !logical {
                return false;
            }
            node = parent;
            continue;
        }
        return matches!(
            parent.kind(),
            "if_statement" | "while_statement" | "do_statement" | "ternary_expression"
        ) && parent
            .child_by_field_name("condition")
            .is_some_and(|condition| condition.id() == node.id());
    }
    false
}

fn callback_compatible_field_type(raw: &str) -> Option<()> {
    let compact = raw
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let annotation = compact.strip_prefix(':').unwrap_or(&compact);
    let mut callable_count = 0usize;
    for member in annotation.split('|') {
        if matches!(member, "null" | "undefined") {
            continue;
        }
        if member == "Function" || (member.starts_with('(') && member.contains(")=>")) {
            callable_count += 1;
        } else {
            return None;
        }
    }
    (callable_count == 1).then_some(())
}

fn is_nullish_callback_clear(node: Node<'_>, source: &[u8]) -> bool {
    matches!(node_text(node, source).trim(), "null" | "undefined")
}

fn direct_parameter_names(parameters: Node<'_>, source: &[u8]) -> Vec<Option<String>> {
    let mut cursor = parameters.walk();
    parameters
        .named_children(&mut cursor)
        .filter(|parameter| !parameter.is_extra())
        .map(|parameter| {
            let raw = node_text(parameter, source);
            if parameter_contains_variadic_form(parameter)
                || raw.contains("...")
                || raw.trim_start().starts_with('*')
                || raw.replace("=>", "").contains('=')
                || raw.trim_start().starts_with(['{', '['])
            {
                return None;
            }
            if parameter.kind() == "identifier" {
                return Some(raw);
            }
            parameter
                .child_by_field_name("pattern")
                .or_else(|| parameter.child_by_field_name("name"))
                .filter(|name| name.kind() == "identifier")
                .map(|name| node_text(name, source))
                .or_else(|| {
                    let mut children = parameter.walk();
                    let name = parameter
                        .named_children(&mut children)
                        .find(|child| child.kind() == "identifier")
                        .map(|name| node_text(name, source));
                    name
                })
        })
        .collect()
}

fn python_direct_parameter_names(parameters: Node<'_>, source: &[u8]) -> Vec<Option<String>> {
    let mut cursor = parameters.walk();
    parameters
        .named_children(&mut cursor)
        .filter(|parameter| !parameter.is_extra())
        .map(|parameter| {
            if parameter_contains_variadic_form(parameter) {
                return None;
            }
            if parameter.kind() == "identifier" {
                return Some(node_text(parameter, source));
            }
            parameter
                .child_by_field_name("name")
                .or_else(|| parameter.child_by_field_name("pattern"))
                .filter(|name| name.kind() == "identifier")
                .map(|name| node_text(name, source))
                .or_else(|| {
                    let mut children = parameter.walk();
                    let name = parameter
                        .named_children(&mut children)
                        .find(|child| child.kind() == "identifier")
                        .map(|name| node_text(name, source));
                    name
                })
        })
        .collect()
}

fn parameter_contains_variadic_form(node: Node<'_>) -> bool {
    if matches!(
        node.kind(),
        "list_splat"
            | "dictionary_splat"
            | "list_splat_pattern"
            | "dictionary_splat_pattern"
            | "rest_pattern"
            | "variadic_parameter"
    ) {
        return true;
    }
    let mut cursor = node.walk();
    let contains = node
        .named_children(&mut cursor)
        .any(parameter_contains_variadic_form);
    contains
}

fn collect_callback_arguments(
    call: Node<'_>,
    callee_name: &str,
    caller_id: &str,
    context: &CallCollectionContext<'_>,
    delegations: &mut Vec<CallbackParameterDelegationFact>,
    output: &mut Vec<CallbackArgumentFact>,
) {
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return;
    };
    if context.language == Language::Python && python_call_exceeds_argument_cap(arguments) {
        return;
    }
    let unsafe_keywords = context.language == Language::Python
        && python_keyword_mapping_unsafe(arguments, context.source);
    let mut cursor = arguments.walk();
    let mut positional_mapping_unsafe = false;
    for (argument_index, argument) in arguments
        .named_children(&mut cursor)
        .filter(|argument| !argument.is_extra())
        .enumerate()
    {
        let (argument, formal_name) =
            if context.language == Language::Python && argument.kind() == "keyword_argument" {
                let Some((name, value)) = python_keyword_argument(argument, context.source) else {
                    continue;
                };
                if unsafe_keywords {
                    continue;
                }
                (value, Some(name))
            } else {
                if context.language == Language::Python
                    && matches!(
                        argument.kind(),
                        "list_splat" | "dictionary_splat" | "generator_expression"
                    )
                {
                    positional_mapping_unsafe = true;
                    continue;
                }
                if positional_mapping_unsafe {
                    continue;
                }
                (argument, None)
            };
        match referenced_parameter(call, argument, context.source, context.symbol_owners) {
            ParameterReference::Exact {
                owner_id,
                parameter_index,
            } => {
                if formal_name.is_some() {
                    continue;
                }
                delegations.push(CallbackParameterDelegationFact {
                    owner_id,
                    parameter_index,
                    callee_name: callee_name.to_owned(),
                    argument_index,
                    line: call.start_position().row + 1,
                    call_start_byte: call.start_byte(),
                });
                continue;
            }
            ParameterReference::Unsafe => continue,
            ParameterReference::NotFormal => {}
        }
        let (target_name, target_qualified_hint, target_symbol) = match argument.kind() {
            "identifier" => (node_text(argument, context.source), None, None),
            "member_expression"
                if argument
                    .child_by_field_name("object")
                    .is_some_and(|object| node_text(object, context.source) == "this") =>
            {
                let Some(property) = argument.child_by_field_name("property") else {
                    continue;
                };
                if property.kind() != "property_identifier" && property.kind() != "identifier" {
                    continue;
                }
                let Some(_) = owning_symbol_id(call, context.symbol_owners) else {
                    continue;
                };
                let owner_name = enclosing_container_name(call, context.source);
                (
                    node_text(property, context.source),
                    owner_name
                        .map(|owner| format!("{owner}.{}", node_text(property, context.source))),
                    None,
                )
            }
            "arrow_function" | "function_expression" | "lambda"
                if is_inline_callback_argument(argument, context.language) =>
            {
                let Some(symbol) = context
                    .inline_callback_symbols
                    .get(&(argument.start_byte(), argument.end_byte()))
                    .cloned()
                else {
                    continue;
                };
                (symbol.name.clone(), None, Some(symbol))
            }
            _ => continue,
        };
        if target_name.is_empty() {
            continue;
        }
        output.push(CallbackArgumentFact {
            caller_id: caller_id.to_owned(),
            callee_name: callee_name.to_owned(),
            argument_index,
            formal_name,
            target_name,
            target_qualified_hint,
            target_symbol,
            line: call.start_position().row + 1,
            call_start_byte: call.start_byte(),
        });
    }
}

fn owning_symbol_id<'a>(
    node: Node<'_>,
    owners: &'a HashMap<(usize, usize), String>,
) -> Option<&'a String> {
    let mut ancestor = Some(node);
    while let Some(current) = ancestor {
        if let Some(id) = owners.get(&(current.start_byte(), current.end_byte())) {
            return Some(id);
        }
        ancestor = current.parent();
    }
    None
}

fn enclosing_container_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if matches!(
            current.kind(),
            "class_declaration" | "class_definition" | "struct_declaration"
        ) {
            return current
                .child_by_field_name("name")
                .map(|name| node_text(name, source));
        }
        ancestor = current.parent();
    }
    None
}

enum ParameterReference {
    NotFormal,
    Unsafe,
    Exact {
        owner_id: String,
        parameter_index: usize,
    },
}

fn referenced_parameter(
    call: Node<'_>,
    argument: Node<'_>,
    source: &[u8],
    symbol_owners: &HashMap<(usize, usize), String>,
) -> ParameterReference {
    if argument.kind() != "identifier" {
        return ParameterReference::NotFormal;
    }
    let name = node_text(argument, source);
    let mut ancestor = call.parent();
    let mut crossed_callable = false;
    while let Some(current) = ancestor {
        if matches!(
            current.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "lambda"
                | "method_definition"
                | "function_definition"
        ) {
            let Some(parameters) = current.child_by_field_name("parameters") else {
                crossed_callable = true;
                ancestor = current.parent();
                continue;
            };
            let parameter_names = if current.kind() == "function_definition" {
                python_direct_parameter_names(parameters, source)
            } else {
                direct_parameter_names(parameters, source)
            };
            let parameter_index = parameter_names
                .iter()
                .position(|parameter| parameter.as_deref() == Some(name.as_str()));
            let Some(parameter_index) = parameter_index else {
                if parameter_tree_declares_name(parameters, &name, source) {
                    return ParameterReference::Unsafe;
                }
                crossed_callable = true;
                ancestor = current.parent();
                continue;
            };
            let Some(owner_id) = symbol_owners
                .get(&(current.start_byte(), current.end_byte()))
                .cloned()
            else {
                return ParameterReference::Unsafe;
            };
            if crossed_callable || parameter_mutated_before(current, call, &name, source) {
                return ParameterReference::Unsafe;
            }
            return ParameterReference::Exact {
                owner_id,
                parameter_index,
            };
        }
        ancestor = current.parent();
    }
    ParameterReference::NotFormal
}

fn parameter_tree_declares_name(node: Node<'_>, name: &str, source: &[u8]) -> bool {
    if matches!(
        node.kind(),
        "identifier" | "shorthand_property_identifier_pattern"
    ) && node_text(node, source) == name
    {
        return true;
    }
    let mut cursor = node.walk();
    let declares = node
        .named_children(&mut cursor)
        .any(|child| parameter_tree_declares_name(child, name, source));
    declares
}

fn parameter_mutated_before(callable: Node<'_>, call: Node<'_>, name: &str, source: &[u8]) -> bool {
    fn visit(
        node: Node<'_>,
        callable_start: usize,
        before: usize,
        name: &str,
        source: &[u8],
    ) -> bool {
        if node.start_byte() >= before {
            return false;
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        let assigned = match node.kind() {
            "assignment_expression"
            | "assignment"
            | "augmented_assignment_expression"
            | "augmented_assignment" => node
                .child_by_field_name("left")
                .or_else(|| node.child_by_field_name("target"))
                .is_some_and(|target| target_contains_identifier(target, name, source)),
            "named_expression" => node
                .child_by_field_name("name")
                .is_some_and(|target| target_contains_identifier(target, name, source)),
            "update_expression" => children
                .iter()
                .any(|child| target_contains_identifier(*child, name, source)),
            "for_in_statement" | "for_of_statement" => node
                .child_by_field_name("left")
                .is_some_and(|target| target_contains_identifier(target, name, source)),
            "for_statement" => node
                .child_by_field_name("left")
                .is_some_and(|target| target_contains_identifier(target, name, source)),
            "with_item" | "except_clause" | "as_pattern" | "aliased_import" => node
                .child_by_field_name("alias")
                .or_else(|| node.child_by_field_name("name"))
                .is_some_and(|target| target_contains_identifier(target, name, source)),
            "case_pattern" | "except_group_clause" => {
                target_contains_identifier(node, name, source)
            }
            "delete_statement" => children
                .iter()
                .any(|child| target_contains_identifier(*child, name, source)),
            "import_statement" | "import_from_statement" => {
                target_contains_identifier(node, name, source)
            }
            "function_definition" | "class_definition" if node.start_byte() != callable_start => {
                node.child_by_field_name("name")
                    .is_some_and(|target| target_contains_identifier(target, name, source))
            }
            _ => false,
        };
        assigned
            || children
                .into_iter()
                .any(|child| visit(child, callable_start, before, name, source))
    }

    visit(
        callable,
        callable.start_byte(),
        call.start_byte(),
        name,
        source,
    )
}

fn target_contains_identifier(node: Node<'_>, name: &str, source: &[u8]) -> bool {
    if node.kind() == "identifier" && node_text(node, source) == name {
        return true;
    }
    let mut cursor = node.walk();
    let contains = node
        .named_children(&mut cursor)
        .any(|child| target_contains_identifier(child, name, source));
    contains
}

fn receiver_type_hint(
    function: Node<'_>,
    call: Node<'_>,
    source: &[u8],
    receiver_bindings: &ReceiverBindingMap,
) -> Option<String> {
    let receiver = function
        .child_by_field_name("object")
        .or_else(|| function.child_by_field_name("receiver"))
        .or_else(|| function.child_by_field_name("expression"))
        .or_else(|| function.child_by_field_name("value"))
        .or_else(|| call.child_by_field_name("object"))
        .or_else(|| call.child_by_field_name("receiver"))?;
    if matches!(
        receiver.kind(),
        "new_expression" | "object_creation_expression"
    ) {
        return constructor_type(receiver, source);
    }
    if receiver.kind() == "member_expression"
        && receiver
            .child_by_field_name("object")
            .is_some_and(|object| node_text(object, source) == "this")
    {
        let field = receiver
            .child_by_field_name("property")
            .map(|property| format!("this.{}", node_text(property, source)))?;
        return receiver_binding_at_call(&field, call, receiver_bindings);
    }
    if !matches!(receiver.kind(), "identifier" | "simple_identifier" | "name") {
        return None;
    }
    let variable = node_text(receiver, source);
    receiver_binding_at_call(&variable, call, receiver_bindings)
}

fn receiver_binding_at_call(
    name: &str,
    call: Node<'_>,
    receiver_bindings: &ReceiverBindingMap,
) -> Option<String> {
    let call_position = call.start_byte();
    let mut ancestor = Some(call);
    while let Some(scope) = ancestor {
        if receiver_scope_kind(scope.kind()) {
            if let Some(bindings) =
                receiver_bindings.get(&(name.to_owned(), scope.start_byte(), scope.end_byte()))
            {
                let preceding =
                    bindings.partition_point(|binding| binding.position < call_position);
                if let Some(binding) = bindings[..preceding].last().or_else(|| {
                    bindings
                        .iter()
                        .find(|binding| !binding.requires_prior_declaration)
                }) {
                    return binding.receiver_type.clone();
                }
                return None;
            }
        }
        ancestor = scope.parent();
    }
    receiver_bindings
        .get(&(name.to_owned(), 0, usize::MAX))
        .and_then(|bindings| {
            let preceding = bindings.partition_point(|binding| binding.position < call_position);
            bindings[..preceding]
                .last()
                .or_else(|| {
                    bindings
                        .iter()
                        .find(|binding| !binding.requires_prior_declaration)
                })
                .and_then(|binding| binding.receiver_type.clone())
        })
}

fn collect_receiver_bindings(
    node: Node<'_>,
    source: &[u8],
    factory_returns: &FactoryReturnMap,
    collection_elements: &CollectionElementMap,
    output: &mut ReceiverBindingMap,
) {
    if matches!(node.kind(), "for_in_statement" | "for_of_statement") && for_loop_uses_of(node) {
        if let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) {
            let binding_names = bound_binding_names(left, source);
            let collection = transparent_collection_member(right, source);
            if binding_names.len() == 1 {
                if let Some(receiver_type) = collection.and_then(|collection| {
                    receiver_binding_at_call(&collection, node, collection_elements)
                }) {
                    let (scope_start, scope_end) = receiver_binding_scope(node, true);
                    output
                        .entry((binding_names[0].clone(), scope_start, scope_end))
                        .or_default()
                        .push(ReceiverBinding {
                            position: node.start_byte(),
                            requires_prior_declaration: true,
                            receiver_type: Some(receiver_type),
                        });
                }
            }
        }
    }
    if node.kind() == "public_field_definition" {
        if let Some(name) = node.child_by_field_name("name") {
            let receiver_type = node
                .child_by_field_name("value")
                .and_then(|value| constructor_type(value, source))
                .or_else(|| {
                    node.child_by_field_name("type")
                        .and_then(|kind| simple_receiver_type(kind, source))
                });
            if let Some(receiver_type) = receiver_type {
                let (scope_start, scope_end) = receiver_binding_scope(node, false);
                output
                    .entry((
                        format!("this.{}", node_text(name, source)),
                        scope_start,
                        scope_end,
                    ))
                    .or_default()
                    .push(ReceiverBinding {
                        position: node.start_byte(),
                        requires_prior_declaration: false,
                        receiver_type: Some(receiver_type),
                    });
            }
        }
    }
    if matches!(
        node.kind(),
        "variable_declarator"
            | "lexical_declaration"
            | "let_declaration"
            | "assignment"
            | "assignment_expression"
    ) {
        let name = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("pattern"))
            .or_else(|| node.child_by_field_name("left"));
        let value = node
            .child_by_field_name("value")
            .or_else(|| node.child_by_field_name("right"));
        if let Some(name) = name {
            let receiver_type = value
                .and_then(|value| constructor_type(value, source))
                .or_else(|| {
                    value.and_then(|value| local_factory_call_type(value, source, factory_returns))
                })
                .or_else(|| declared_variable_type(node, source));
            let declares_binding = matches!(
                node.kind(),
                "variable_declarator" | "lexical_declaration" | "let_declaration"
            );
            let binding_names = bound_binding_names(name, source);
            let simple_binding = binding_names.len() == 1
                && matches!(name.kind(), "identifier" | "simple_identifier" | "name");
            let assignable_binding =
                simple_binding || matches!(name.kind(), "object_pattern" | "array_pattern");
            if declares_binding || assignable_binding {
                for variable in binding_names {
                    let (scope_start, scope_end) = if declares_binding {
                        receiver_binding_scope(node, true)
                    } else {
                        receiver_assignment_scope(&variable, node, output)
                    };
                    let bindings = output
                        .entry((variable, scope_start, scope_end))
                        .or_default();
                    if receiver_type.is_none()
                        && bindings.iter().any(|binding| {
                            binding.position == node.start_byte() && binding.receiver_type.is_some()
                        })
                    {
                        continue;
                    }
                    bindings.push(ReceiverBinding {
                        position: node.start_byte(),
                        requires_prior_declaration: true,
                        receiver_type: simple_binding.then(|| receiver_type.clone()).flatten(),
                    });
                }
            }
        }
    }
    if is_parameter_binding(node) {
        let name = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("pattern"))
            .or_else(|| {
                matches!(node.kind(), "identifier" | "simple_identifier" | "name").then_some(node)
            });
        if let Some(name) = name {
            let binding_names = bound_binding_names(name, source);
            let simple_binding = binding_names.len() == 1
                && matches!(name.kind(), "identifier" | "simple_identifier" | "name");
            let receiver_type = simple_binding
                .then(|| declared_variable_type(node, source))
                .flatten();
            let (scope_start, scope_end) = receiver_binding_scope(node, true);
            for variable in binding_names {
                output
                    .entry((variable, scope_start, scope_end))
                    .or_default()
                    .push(ReceiverBinding {
                        position: node.start_byte(),
                        requires_prior_declaration: true,
                        receiver_type: receiver_type.clone(),
                    });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_receiver_bindings(child, source, factory_returns, collection_elements, output);
    }
}

fn collect_collection_element_bindings(
    node: Node<'_>,
    source: &[u8],
    output: &mut CollectionElementMap,
    reassigned: &mut std::collections::HashSet<(String, usize, usize)>,
) {
    if node.kind() == "public_field_definition" {
        if let Some(name) = node.child_by_field_name("name") {
            let receiver_type = node
                .child_by_field_name("type")
                .and_then(|annotation| {
                    collection_element_annotation(&node_text(annotation, source))
                })
                .or_else(|| {
                    node.child_by_field_name("value")
                        .filter(|value| {
                            matches!(
                                value.kind(),
                                "new_expression" | "object_creation_expression"
                            )
                        })
                        .and_then(|value| collection_element_initializer(&node_text(value, source)))
                });
            let (scope_start, scope_end) = receiver_binding_scope(node, false);
            output
                .entry((
                    format!("this.{}", node_text(name, source)),
                    scope_start,
                    scope_end,
                ))
                .or_default()
                .push(ReceiverBinding {
                    position: node.start_byte(),
                    requires_prior_declaration: false,
                    receiver_type,
                });
        }
    }
    if matches!(
        node.kind(),
        "assignment" | "assignment_expression" | "augmented_assignment_expression"
    ) {
        if let Some(member) = node
            .child_by_field_name("left")
            .or_else(|| node.child_by_field_name("name"))
            .and_then(|left| direct_this_member(left, source))
        {
            let (scope_start, scope_end) = receiver_binding_scope(node, false);
            reassigned.insert((member, scope_start, scope_end));
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_collection_element_bindings(child, source, output, reassigned);
    }
}

fn collection_element_annotation(raw: &str) -> Option<String> {
    let compact = raw
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let annotation = compact.strip_prefix(':').unwrap_or(&compact);
    ["Set<", "Array<", "ReadonlySet<", "ReadonlyArray<"]
        .into_iter()
        .find_map(|prefix| {
            let element = annotation.strip_prefix(prefix)?.strip_suffix('>')?;
            simple_receiver_name(element).then(|| element.to_owned())
        })
}

fn collection_element_initializer(raw: &str) -> Option<String> {
    let compact = raw
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    ["newSet<", "newArray<"].into_iter().find_map(|prefix| {
        let tail = compact.strip_prefix(prefix)?;
        let (element, arguments) = tail.split_once('>')?;
        (simple_receiver_name(element) && arguments.starts_with('(') && arguments.ends_with(')'))
            .then(|| element.to_owned())
    })
}

fn direct_this_member(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "member_expression"
        || node
            .child_by_field_name("object")
            .is_none_or(|object| node_text(object, source) != "this")
    {
        return None;
    }
    node.child_by_field_name("property")
        .map(|property| format!("this.{}", node_text(property, source)))
}

fn transparent_collection_member(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(member) = direct_this_member(node, source) {
        return Some(member);
    }
    if node.kind() == "parenthesized_expression" {
        return (node.named_child_count() == 1)
            .then(|| node.named_child(0))
            .flatten()
            .and_then(|child| transparent_collection_member(child, source));
    }
    if node.kind() != "array" || node.named_child_count() != 1 {
        return None;
    }
    let spread = node.named_child(0)?;
    if spread.kind() != "spread_element" || spread.named_child_count() != 1 {
        return None;
    }
    spread
        .named_child(0)
        .and_then(|child| direct_this_member(child, source))
}

fn for_loop_uses_of(node: Node<'_>) -> bool {
    let mut cursor = node.walk();
    let uses_of = node.children(&mut cursor).any(|child| child.kind() == "of");
    uses_of
}

fn collect_local_factory_returns(node: Node<'_>, source: &[u8], output: &mut FactoryReturnMap) {
    if node.kind() == "variable_declarator" {
        if let Some(name) = node
            .child_by_field_name("name")
            .filter(|name| matches!(name.kind(), "identifier" | "simple_identifier" | "name"))
        {
            let receiver_type = node
                .child_by_field_name("value")
                .filter(|value| {
                    matches!(value.kind(), "arrow_function" | "function_expression")
                        && enclosing_const_declaration(node, source)
                })
                .and_then(|value| direct_factory_return_type(value, source));
            let (scope_start, scope_end) = receiver_binding_scope(node, true);
            output
                .entry((node_text(name, source), scope_start, scope_end))
                .or_default()
                .push(ReceiverBinding {
                    position: node.start_byte(),
                    requires_prior_declaration: true,
                    receiver_type,
                });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_local_factory_returns(child, source, output);
    }
}

fn enclosing_const_declaration(node: Node<'_>, source: &[u8]) -> bool {
    let mut ancestor = node.parent();
    while let Some(declaration) = ancestor {
        if matches!(
            declaration.kind(),
            "lexical_declaration" | "variable_declaration"
        ) {
            return node_text(declaration, source)
                .trim_start()
                .starts_with("const ");
        }
        if receiver_scope_kind(declaration.kind()) {
            break;
        }
        ancestor = declaration.parent();
    }
    false
}

fn direct_factory_return_type(callable: Node<'_>, source: &[u8]) -> Option<String> {
    let declaration = node_text(callable, source);
    let trimmed = declaration.trim_start();
    let mut modifier_cursor = callable.walk();
    let has_async_modifier = callable
        .children(&mut modifier_cursor)
        .any(|child| child.kind() == "async");
    if callable.kind().contains("generator")
        || has_async_modifier
        || trimmed.starts_with("async ")
        || trimmed.starts_with("async(")
        || trimmed.starts_with("function*")
        || trimmed.starts_with("function *")
    {
        return None;
    }
    if let Some(return_type) = callable
        .child_by_field_name("return_type")
        .and_then(|annotation| simple_nominal_return_type(annotation, source))
    {
        return Some(return_type);
    }
    let body = callable
        .child_by_field_name("body")
        .or_else(|| callable.named_child(callable.named_child_count().saturating_sub(1)))?;
    if matches!(body.kind(), "new_expression" | "object_creation_expression") {
        return constructor_type(body, source);
    }
    if !matches!(body.kind(), "statement_block" | "block") {
        return None;
    }
    let mut cursor = body.walk();
    let statements = body.named_children(&mut cursor).collect::<Vec<_>>();
    if statements.len() != 1 || statements[0].kind() != "return_statement" {
        return None;
    }
    statements[0]
        .named_child(0)
        .and_then(|returned| constructor_type(returned, source))
}

fn local_factory_call_type(
    value: Node<'_>,
    source: &[u8],
    factory_returns: &FactoryReturnMap,
) -> Option<String> {
    if !matches!(value.kind(), "call" | "call_expression") {
        return None;
    }
    let function = value
        .child_by_field_name("function")
        .or_else(|| value.named_child(0))?;
    if !matches!(function.kind(), "identifier" | "simple_identifier" | "name") {
        return None;
    }
    receiver_binding_at_call(&node_text(function, source), value, factory_returns)
}

fn bound_binding_names(node: Node<'_>, source: &[u8]) -> Vec<String> {
    fn visit(node: Node<'_>, source: &[u8], output: &mut Vec<String>) {
        if matches!(
            node.kind(),
            "identifier" | "simple_identifier" | "name" | "shorthand_property_identifier_pattern"
        ) {
            output.push(node_text(node, source));
            return;
        }
        if matches!(
            node.kind(),
            "pair_pattern" | "pair" | "object_assignment_pattern"
        ) {
            if let Some(value) = node
                .child_by_field_name("value")
                .or_else(|| node.child_by_field_name("right"))
            {
                visit(value, source, output);
            }
            return;
        }
        if node.kind() == "assignment_pattern" {
            if let Some(left) = node
                .child_by_field_name("left")
                .or_else(|| node.child_by_field_name("pattern"))
            {
                visit(left, source, output);
            }
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            visit(child, source, output);
        }
    }

    let mut names = Vec::new();
    visit(node, source, &mut names);
    names.sort();
    names.dedup();
    names
}

fn receiver_assignment_scope(
    name: &str,
    node: Node<'_>,
    bindings: &ReceiverBindingMap,
) -> (usize, usize) {
    let position = node.start_byte();
    bindings
        .keys()
        .filter(|(candidate, start, end)| {
            candidate == name && *start <= position && position < *end
        })
        .min_by_key(|(_, start, end)| end.saturating_sub(*start))
        .map(|(_, start, end)| (*start, *end))
        .unwrap_or_else(|| receiver_enclosing_callable_scope(node))
}

fn receiver_enclosing_callable_scope(node: Node<'_>) -> (usize, usize) {
    let mut ancestor = node.parent();
    while let Some(scope) = ancestor {
        if matches!(
            scope.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "method_declaration"
        ) {
            return (scope.start_byte(), scope.end_byte());
        }
        ancestor = scope.parent();
    }
    (0, usize::MAX)
}

fn is_parameter_binding(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "required_parameter"
            | "optional_parameter"
            | "formal_parameter"
            | "typed_parameter"
            | "default_parameter"
            | "parameter"
    ) || (matches!(node.kind(), "identifier" | "simple_identifier" | "name")
        && node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "formal_parameters" | "parameters" | "parameter_list" | "catch_clause"
            )
        }))
}

fn receiver_binding_scope(node: Node<'_>, local: bool) -> (usize, usize) {
    let mut ancestor = node.parent();
    while let Some(scope) = ancestor {
        let is_scope = if local {
            matches!(
                scope.kind(),
                "statement_block"
                    | "block"
                    | "compound_statement"
                    | "for_statement"
                    | "for_in_statement"
                    | "for_of_statement"
                    | "if_statement"
                    | "while_statement"
                    | "catch_clause"
                    | "switch_case"
                    | "switch_statement"
                    | "function_declaration"
                    | "function_definition"
                    | "function_expression"
                    | "arrow_function"
                    | "method_definition"
                    | "method_declaration"
            )
        } else {
            matches!(
                scope.kind(),
                "class_declaration" | "class_body" | "struct_declaration" | "struct_body"
            )
        };
        if is_scope {
            return (scope.start_byte(), scope.end_byte());
        }
        ancestor = scope.parent();
    }
    (0, usize::MAX)
}

fn receiver_scope_kind(kind: &str) -> bool {
    matches!(
        kind,
        "statement_block"
            | "block"
            | "for_statement"
            | "for_in_statement"
            | "for_of_statement"
            | "catch_clause"
            | "switch_case"
            | "switch_statement"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "method_declaration"
            | "class_declaration"
            | "class_body"
            | "struct_declaration"
            | "struct_body"
    )
}

fn constructor_type(node: Node<'_>, source: &[u8]) -> Option<String> {
    let constructor = if matches!(node.kind(), "new_expression" | "object_creation_expression") {
        node.child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("type"))
            .or_else(|| node.named_child(0))
    } else if matches!(node.kind(), "call" | "call_expression") {
        node.child_by_field_name("function")
            .or_else(|| node.named_child(0))
    } else {
        None
    }?;
    if matches!(
        constructor.kind(),
        "scoped_identifier" | "scoped_type_identifier"
    ) {
        return constructor
            .child_by_field_name("scope")
            .or_else(|| constructor.child_by_field_name("path"))
            .map(|scope| node_text(scope, source));
    }
    let name = call_name(constructor, source)?;
    name.chars()
        .next()
        .is_some_and(char::is_uppercase)
        .then_some(name)
}

fn declared_variable_type(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(receiver_type) = node
        .child_by_field_name("type")
        .and_then(|kind| simple_receiver_type(kind, source))
    {
        return Some(receiver_type);
    }
    let mut declaration = node.parent();
    while let Some(parent) = declaration {
        if matches!(
            parent.kind(),
            "local_variable_declaration" | "variable_declaration"
        ) {
            return parent
                .child_by_field_name("type")
                .and_then(|kind| simple_receiver_type(kind, source));
        }
        if matches!(
            parent.kind(),
            "function_declaration" | "method_declaration" | "statement_block" | "block"
        ) {
            break;
        }
        declaration = parent.parent();
    }
    None
}

fn simple_receiver_type(annotation: Node<'_>, source: &[u8]) -> Option<String> {
    let raw = node_text(annotation, source);
    let annotation = raw.trim().strip_prefix(':').unwrap_or(raw.trim()).trim();
    let mut nominal_members = top_level_type_union_members(annotation)?
        .into_iter()
        .filter(|member| !matches!(*member, "null" | "undefined" | "void"))
        .collect::<Vec<_>>();
    if nominal_members.len() != 1 {
        return None;
    }
    simple_receiver_nominal_name(nominal_members.remove(0))
}

fn top_level_type_union_members(annotation: &str) -> Option<Vec<&str>> {
    let mut members = Vec::new();
    let mut start = 0;
    let mut delimiters = Vec::new();
    for (offset, character) in annotation.char_indices() {
        match character {
            '<' => delimiters.push('>'),
            '(' => delimiters.push(')'),
            '[' => delimiters.push(']'),
            '{' => delimiters.push('}'),
            '>' | ')' | ']' | '}' => {
                if delimiters.pop() != Some(character) {
                    return None;
                }
            }
            '|' if delimiters.is_empty() => {
                let member = annotation[start..offset].trim();
                if member.is_empty() {
                    return None;
                }
                members.push(member);
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    if !delimiters.is_empty() {
        return None;
    }
    let member = annotation[start..].trim();
    if member.is_empty() {
        return None;
    }
    members.push(member);
    Some(members)
}

fn simple_receiver_nominal_name(annotation: &str) -> Option<String> {
    if simple_receiver_name(annotation) {
        return Some(annotation.to_owned());
    }
    let generic_start = annotation.find('<')?;
    let outer = annotation[..generic_start].trim();
    if !simple_receiver_name(outer) || !annotation.ends_with('>') {
        return None;
    }

    let arguments = &annotation[generic_start + 1..annotation.len() - 1];
    if arguments.trim().is_empty() {
        return None;
    }
    let mut depth = 1usize;
    for character in arguments.chars() {
        match character {
            '<' => depth = depth.checked_add(1)?,
            '>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    (depth == 1).then(|| outer.to_owned())
}

fn simple_receiver_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first == '$' || first.is_ascii_alphabetic())
        && characters.all(|character| {
            character == '_' || character == '$' || character.is_ascii_alphanumeric()
        })
}

fn call_target_node(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "call" => node
            .child_by_field_name("method")
            .or_else(|| node.child_by_field_name("function")),
        "member_call_expression" | "nullsafe_member_call_expression" | "scoped_call_expression" => {
            node.child_by_field_name("name")
        }
        "function_call" => node.child_by_field_name("name"),
        "call_expression" if node.child_by_field_name("function").is_none() => node.named_child(0),
        "method_invocation" => node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("function")),
        "object_creation_expression" => node.child_by_field_name("type"),
        "invocation_expression" => node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0)),
        _ => node.child_by_field_name("function"),
    }
}

fn call_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier"
        | "property_identifier"
        | "field_identifier"
        | "type_identifier"
        | "simple_identifier"
        | "name"
        | "variable" => Some(node_text(node, source)),
        "member_expression" => node
            .child_by_field_name("property")
            .map(|property| node_text(property, source)),
        "attribute" => node
            .child_by_field_name("attribute")
            .map(|attribute| node_text(attribute, source)),
        "field_expression" => node
            .child_by_field_name("field")
            .map(|field| node_text(field, source)),
        "selector_expression" => node
            .child_by_field_name("field")
            .map(|field| node_text(field, source)),
        "field_access" | "member_access_expression" => node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("field"))
            .map(|field| node_text(field, source)),
        "scoped_identifier" | "scoped_type_identifier" => node
            .child_by_field_name("name")
            .map(|name| node_text(name, source)),
        "qualified_identifier" => node
            .child_by_field_name("name")
            .and_then(|name| call_name(name, source)),
        "method_index_expression" | "dot_index_expression" => node
            .child_by_field_name("method")
            .or_else(|| node.child_by_field_name("field"))
            .or_else(|| node.named_child(node.named_child_count().saturating_sub(1)))
            .and_then(|name| call_name(name, source)),
        "generic_function" => node
            .child_by_field_name("function")
            .and_then(|function| call_name(function, source)),
        _ => None,
    }
}

fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or_default().to_owned()
}

fn span(node: Node<'_>) -> SourceSpan {
    SourceSpan {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_only_simple_outer_generic_receiver_annotations() {
        assert_eq!(
            simple_receiver_nominal_name("LRUCache<string, ParseResult | null>"),
            Some("LRUCache".to_owned())
        );
        assert_eq!(
            simple_receiver_nominal_name("ParseWorkerPool<Array<ParseTask>>"),
            Some("ParseWorkerPool".to_owned())
        );
        assert_eq!(simple_receiver_nominal_name("pkg.LRUCache<string>"), None);
        assert_eq!(
            simple_receiver_nominal_name("LRUCache<string> & Disposable"),
            None
        );
        assert_eq!(
            simple_receiver_nominal_name(
                "T extends string ? LRUCache<string> : ParseWorkerPool<string>"
            ),
            None
        );
        assert_eq!(simple_receiver_nominal_name("LRUCache<>"), None);
        assert_eq!(simple_receiver_nominal_name("LRUCache<string>>"), None);
    }

    #[test]
    fn callback_argument_facts_preserve_exact_positions() {
        let facts = parse_file(
            "main.ts",
            "function selected() {}\nfunction invoke(value: number, callback: () => void) { callback(); }\nfunction caller() { invoke(1, selected); }\n",
        )
        .unwrap();
        assert_eq!(facts.callback_parameter_invocations.len(), 1);
        assert_eq!(facts.callback_parameter_invocations[0].parameter_index, 1);
        assert!(facts.callback_arguments.iter().any(|argument| {
            argument.callee_name == "invoke"
                && argument.argument_index == 1
                && argument.target_name == "selected"
        }));
        let member_facts = parse_file(
            "member.ts",
            "class Invoker { invoke(callback: () => void) { callback(); } }\nclass Page { selected = () => {}; run() { new Invoker().invoke(this.selected); } }\n",
        )
        .unwrap();
        assert!(member_facts.callback_arguments.iter().any(|argument| {
            argument.callee_name == "invoke"
                && argument.target_name == "selected"
                && argument.target_qualified_hint.as_deref() == Some("Page.selected")
        }));

        let delegated = parse_file(
            "delegated.ts",
            "function leaf(callback: () => void) { callback(); }\n\
             function middle(callback: () => void) { leaf(callback); }\n",
        )
        .unwrap();
        assert_eq!(delegated.callback_parameter_delegations.len(), 1);
        let delegation = &delegated.callback_parameter_delegations[0];
        assert_eq!(delegation.parameter_index, 0);
        assert_eq!(delegation.callee_name, "leaf");
        assert_eq!(delegation.argument_index, 0);
        assert!(delegation.call_start_byte > 0);

        let deferred_outer = parse_file(
            "deferred.ts",
            "function leaf(callback: () => void) { callback(); }\n\
             function outer(callback: () => void) {\n\
               [1].forEach(() => leaf(callback));\n\
             }\n",
        )
        .unwrap();
        assert!(deferred_outer.callback_parameter_delegations.is_empty());
        assert!(deferred_outer
            .callback_arguments
            .iter()
            .all(|argument| argument.target_name != "callback"));

        let inline = parse_file(
            "inline.ts",
            "function selected() {}\n\
             function invoke(callback: () => void) { callback(); }\n\
             function caller() { invoke(() => selected()); }\n",
        )
        .unwrap();
        let inline_argument = inline
            .callback_arguments
            .iter()
            .find(|argument| argument.callee_name == "invoke")
            .unwrap();
        let inline_symbol = inline_argument.target_symbol.as_ref().unwrap();
        assert_eq!(inline_symbol.name, "<callback invoke argument 1 #1>");
        assert!(inline
            .unresolved_calls
            .iter()
            .any(|call| { call.caller_id == inline_symbol.id && call.callee_name == "selected" }));
        assert!(inline
            .symbols
            .iter()
            .all(|symbol| symbol.id != inline_symbol.id));
    }

    #[test]
    fn python_positional_lambdas_materialize_exact_callback_facts() {
        let facts = parse_file(
            "main.py",
            "def selected():\n    pass\n\
             def invoke(value, callback):\n    callback()\n\
             def caller():\n    invoke(1, lambda: selected())\n",
        )
        .unwrap();
        assert!(facts
            .callback_parameter_invocations
            .iter()
            .any(|invocation| invocation.parameter_index == 1));
        let argument = facts
            .callback_arguments
            .iter()
            .find(|argument| argument.callee_name == "invoke")
            .unwrap();
        assert_eq!(argument.argument_index, 1);
        assert_eq!(
            argument.target_symbol.as_ref().unwrap().name,
            "<callback invoke argument 2 #1>"
        );
    }

    #[test]
    fn python_keyword_lambdas_map_to_exact_eligible_formals() {
        let facts = parse_file(
            "main.py",
            "def invoke(value, callback=lambda: None, *, on_done=None):\n    callback()\n    on_done()\n\ndef caller():\n    invoke(on_done=lambda: None, callback=lambda: None, value=1)\n",
        )
        .unwrap();
        let invoke = facts
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "invoke")
            .unwrap();
        assert!(facts.python_callback_formals.iter().any(|formal| {
            formal.owner_id == invoke.id
                && formal.formal_name == "callback"
                && formal.parameter_index == 1
        }));
        assert!(facts
            .callback_parameter_invocations
            .iter()
            .any(|invocation| invocation.owner_id == invoke.id && invocation.parameter_index == 1));
        assert!(facts
            .callback_parameter_invocations
            .iter()
            .any(|invocation| invocation.owner_id == invoke.id && invocation.parameter_index == 3));
        assert!(facts.python_callback_formals.iter().any(|formal| {
            formal.owner_id == invoke.id
                && formal.formal_name == "on_done"
                && formal.parameter_index == 3
        }));
        let keyword_callbacks = facts
            .callback_arguments
            .iter()
            .filter(|argument| argument.callee_name == "invoke")
            .collect::<Vec<_>>();
        assert_eq!(keyword_callbacks.len(), 2);
        assert!(keyword_callbacks.iter().any(|argument| {
            argument.formal_name.as_deref() == Some("callback")
                && argument
                    .target_symbol
                    .as_ref()
                    .is_some_and(|symbol| symbol.name.contains("keyword callback"))
        }));
        assert!(keyword_callbacks.iter().any(|argument| {
            argument.formal_name.as_deref() == Some("on_done")
                && argument
                    .target_symbol
                    .as_ref()
                    .is_some_and(|symbol| symbol.name.contains("keyword on_done"))
        }));
    }

    #[test]
    fn python_keyword_callback_formals_fail_closed_for_unsafe_shapes() {
        let decorated = parse_file(
            "decorated.py",
            "def deco(fn):\n    return fn\n\
             @deco\n\
             def invoke(callback):\n    callback()\n\
             def caller():\n    invoke(callback=lambda: None)\n",
        )
        .unwrap();
        let decorated_invoke = decorated
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "invoke")
            .unwrap();
        assert!(decorated
            .python_callback_formals
            .iter()
            .all(|formal| formal.owner_id != decorated_invoke.id));

        let positional_only = parse_file(
            "positional.py",
            "def invoke(callback, /, ordinary=None, *args, keyword_only=None, **kwargs):\n\
                 callback()\n\
                 ordinary()\n\
                 keyword_only()\n",
        )
        .unwrap();
        let names = positional_only
            .python_callback_formals
            .iter()
            .map(|formal| formal.formal_name.as_str())
            .collect::<Vec<_>>();
        assert!(!names.contains(&"callback"));
        assert!(!names.contains(&"args"));
        assert!(!names.contains(&"kwargs"));
        assert!(names.contains(&"ordinary"));
        assert!(names.contains(&"keyword_only"));
        let positional_invocation = parse_file(
            "positional_invocation.py",
            "def invoke(callback, /):\n    callback()\n\ndef caller():\n    invoke(lambda: None)\n",
        )
        .unwrap();
        let positional_invoke = positional_invocation
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "invoke")
            .unwrap();
        assert!(positional_invocation
            .callback_parameter_invocations
            .iter()
            .any(|invocation| {
                invocation.owner_id == positional_invoke.id && invocation.parameter_index == 0
            }));

        let slash_in_default = parse_file(
            "slash_default.py",
            "def invoke(callback, path='a/b'):\n    callback()\n\ndef caller():\n    invoke(callback=lambda: None)\n",
        )
        .unwrap();
        let invoke = slash_in_default
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "invoke")
            .unwrap();
        assert!(slash_in_default
            .python_callback_formals
            .iter()
            .any(|formal| {
                formal.owner_id == invoke.id
                    && formal.formal_name == "callback"
                    && formal.parameter_index == 0
            }));

        for source in [
            "def invoke(callback):\n    callback()\n\ndef caller():\n    invoke(callback=lambda: None, callback=lambda: None)\n",
            "def invoke(callback):\n    callback()\n\ndef caller(values):\n    invoke(callback=lambda: None, **values)\n",
        ] {
            let facts = parse_file("unsafe.py", source).unwrap();
            assert!(
                facts
                    .callback_arguments
                    .iter()
                    .all(|argument| argument.formal_name.is_none()),
                "unexpected keyword callback for {source}"
            );
        }
    }

    #[test]
    fn python_callback_calls_above_argument_cap_emit_no_callback_facts() {
        let mut arguments = vec!["lambda: None".to_owned()];
        arguments.extend((1..=MAX_PYTHON_CALL_ARGUMENTS).map(|index| index.to_string()));
        let source = format!(
            "def invoke(callback, *values):\n    callback()\n\ndef caller():\n    invoke({})\n",
            arguments.join(", ")
        );
        let facts = parse_file("oversized.py", &source).unwrap();
        assert!(facts.callback_arguments.is_empty());
    }

    #[test]
    fn python_lambda_callbacks_fail_closed_after_non_positional_arguments() {
        for source in [
            "def invoke(value, callback):\n    callback()\n\ndef caller(values):\n    invoke(*values, lambda: None)\n",
            "def invoke(value, callback):\n    callback()\n\ndef caller(values):\n    invoke(**values, lambda: None)\n",
            "def invoke(value, callback):\n    callback()\n\ndef caller(values):\n    invoke((value for value in values), lambda: None)\n",
        ] {
            let facts = parse_file("main.py", source).unwrap();
            assert!(
                facts
                    .callback_arguments
                    .iter()
                    .all(|argument| argument.target_symbol.is_none()),
                "unexpected Python lambda callback for {source}"
            );
        }
    }

    #[test]
    fn python_callback_parameters_stop_at_lambda_shadows_and_mutations() {
        let facts = parse_file(
            "main.py",
            "def leaf(callback):\n    callback()\n\
             def shadowed(callback):\n    (lambda callback: leaf(callback))(other)\n\
             def assigned(callback):\n    callback = other\n    leaf(callback)\n\
             def augmented(callback):\n    callback += other\n    leaf(callback)\n\
             def named(callback):\n    (callback := other)\n    leaf(callback)\n",
        )
        .unwrap();
        assert!(
            facts.callback_parameter_delegations.is_empty(),
            "{:?}",
            facts.callback_parameter_delegations
        );
    }

    #[test]
    fn python_variadic_formals_and_loop_rebindings_do_not_delegate_callbacks() {
        let facts = parse_file(
            "main.py",
            "def leaf(callback):\n    callback()\n\
             def positional(*callback):\n    leaf(callback)\n\
             def keywords(**callback):\n    leaf(callback)\n\
             def wrapped(*callback: object):\n    leaf(callback)\n\
             def looped(callback, values):\n    for callback in values:\n        pass\n    leaf(callback)\n",
        )
        .unwrap();
        assert!(
            facts.callback_parameter_delegations.is_empty(),
            "{:?}",
            facts.callback_parameter_delegations
        );
    }

    #[test]
    fn python_match_and_exception_group_captures_do_not_delegate_callbacks() {
        let facts = parse_file(
            "main.py",
            "def leaf(callback):\n    callback()\n\
             def matched(callback, value):\n    match value:\n        case callback:\n            leaf(callback)\n\
             def grouped(callback):\n    try:\n        raise ExceptionGroup('errors', [ValueError()])\n    except* ValueError as callback:\n        leaf(callback)\n",
        )
        .unwrap();
        assert!(
            facts.callback_parameter_delegations.is_empty(),
            "{:?}",
            facts.callback_parameter_delegations
        );
    }

    #[test]
    fn same_class_stored_callback_fields_mark_the_source_parameter_invoked() {
        let facts = parse_file(
            "InputDeviceUtil.ets",
            "class InputDevice {\n\
               private callback: Function | null = null\n\
               registerChange(callback: Function): void { this.callback = callback }\n\
               unregisterChange(): void {\n\
                 if (this.callback !== null) { this.callback([]); this.callback = null }\n\
               }\n\
               onChange(): void { if (this.callback) { this.callback([]) } }\n\
             }\n",
        )
        .unwrap();
        let register = facts
            .symbols
            .iter()
            .find(|symbol| symbol.qualified_name == "InputDevice.registerChange")
            .unwrap();
        assert!(facts
            .callback_parameter_invocations
            .iter()
            .any(
                |invocation| invocation.owner_id == register.id && invocation.parameter_index == 0
            ));
    }

    #[test]
    fn stored_callback_summaries_fail_closed_for_unsafe_fields_and_writes() {
        for source in [
            "class Unsafe { callback: Function | string = null; set(callback: Function) { this.callback = callback } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = () => {}; set(callback: Function) { this.callback = callback } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback; this.callback = other } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { const nested = () => { this.callback = callback } } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback } run() { const nested = () => this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback } leak() { return this.callback } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback } leak() { use(this.callback) } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback } leak() { const alias = this.callback } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback } run() { this.callback.call(this) } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback; this.callback += callback } run() { this.callback() } }",
            "class Unsafe { callback: Function | null = null; set(callback: Function) { this.callback = callback; this.callback = null } run() { this.callback() } }",
        ] {
            let facts = parse_file("unsafe.ets", source).unwrap();
            assert!(
                facts.callback_parameter_invocations.is_empty(),
                "unexpected stored callback summary for {source}"
            );
        }
    }

    #[test]
    fn stored_callback_operation_cap_fails_closed() {
        let guards = "if (this.callback) {}\n".repeat(MAX_STORED_CALLBACK_OPERATIONS_PER_CLASS + 1);
        let source = format!(
            "class Capped {{\n\
               callback: Function | null = null\n\
               set(callback: Function) {{ this.callback = callback }}\n\
               run() {{ this.callback(); {guards} }}\n\
             }}"
        );
        let facts = parse_file("capped.ets", &source).unwrap();
        assert!(facts.callback_parameter_invocations.is_empty());
    }

    #[test]
    fn extracts_typescript_symbols_and_calls() {
        let facts = parse_file(
            "src/demo.ts",
            "class Greeter { greet() { helper(); } }\nfunction helper() {}\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_offset_preserved_vue_and_svelte_script_blocks() {
        for (file, source, language, function_line) in [
            (
                "Panel.vue",
                "<template><FakeCall>你好</FakeCall></template>\n\
                 <script lang=\"ts\">\n\
                 export function loadPanel() { helper(); }\n\
                 function helper() {}\n\
                 </script>\n",
                Language::Vue,
                3,
            ),
            (
                "Panel.svelte",
                "<h1>Привет</h1>\n\
                 <script lang=\"ts\">\n\
                 export function loadPanel() { helper(); }\n\
                 function helper() {}\n\
                 </script>\n",
                Language::Svelte,
                3,
            ),
        ] {
            let facts = parse_file(file, source).unwrap();
            assert_eq!(facts.language, language);
            assert!(facts
                .symbols
                .iter()
                .any(|symbol| symbol.name == "loadPanel" && symbol.start_line == function_line));
            assert!(!facts.symbols.iter().any(|symbol| symbol.name == "FakeCall"));
            let call = facts
                .unresolved_calls
                .iter()
                .find(|call| call.callee_name == "helper")
                .unwrap();
            assert_eq!(call.line, function_line);
        }
    }

    #[test]
    fn extracts_astro_frontmatter_and_multiple_scripts_with_exact_offsets() {
        let source = "\u{feff}\r\n\
                      \t--- \r\n\
                      import Graph from './GraphDiagram.astro'\r\n\
                      export function loadCafé() { helper() }\r\n\
                      function helper() {}\r\n\
                      ---\r\n\
                      <main>你好</main>\r\n\
                      <script data-label='>'>\r\n\
                      const decoy = \"</script>\" // </script>\r\n\
                      /* </script> */ export function browserOne() {}\r\n\
                      </ScRiPt>\r\n\
                      <script>\r\n\
                      export function browserTwo() {}\r\n\
                      </script>\r\n";
        let facts = parse_file("src/pages/Index.astro", source).unwrap();
        assert_eq!(facts.language, Language::Astro);

        for (name, line) in [
            ("loadCafé", 4),
            ("helper", 5),
            ("browserOne", 10),
            ("browserTwo", 13),
        ] {
            let symbol = facts
                .symbols
                .iter()
                .find(|symbol| symbol.name == name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(
                symbol.start_byte,
                source.find(&format!("function {name}")).unwrap()
            );
            assert_eq!(symbol.start_line, line);
        }
        assert!(!facts.symbols.iter().any(|symbol| symbol.name == "main"));

        let import = facts
            .unresolved_references
            .iter()
            .find(|reference| reference.binding_name == "Graph")
            .expect("Astro default import reference");
        assert_eq!(import.target_name, "GraphDiagram");
        assert_eq!(
            import.target_file_hint.as_deref(),
            Some("./GraphDiagram.astro")
        );
        assert_eq!(import.kind, RelationshipKind::Imports);
    }

    #[test]
    fn astro_mask_preserves_every_byte_and_line_break() {
        let source = "\r\n---\r\nconst café = '你好'\r\n---\r\n<div>Привет</div>\r\n\
                      <script>const λ = 1</script>\r\n";
        let masked = astro_script_source(source);
        assert_eq!(masked.len(), source.len());
        for (index, byte) in source.bytes().enumerate() {
            if matches!(byte, b'\r' | b'\n') {
                assert_eq!(masked.as_bytes()[index], byte);
            }
        }
        for retained in ["const café = '你好'", "const λ = 1"] {
            let start = source.find(retained).unwrap();
            assert_eq!(&masked[start..start + retained.len()], retained);
        }
        for hidden in ["---", "<div>Привет</div>", "<script>", "</script>"] {
            let start = source.find(hidden).unwrap();
            assert!(masked.as_bytes()[start..start + hidden.len()]
                .iter()
                .all(|byte| *byte == b' '));
        }
    }

    #[test]
    fn malformed_astro_regions_fail_closed_without_poisoning_markup() {
        for source in [
            "---\nconst text = `<script>function fake() {}</script>`\n",
            "<script data-value=\">\nfunction fake() {}\n</script>",
            "<script>\nconst text = `</script>`\nfunction fake() {}\n",
            "<!-- <script>function fake() {}</script> -->",
            "<scripture>function fake() {}</scripture>",
        ] {
            let facts = parse_file("Broken.astro", source).unwrap();
            assert!(
                facts.symbols.iter().all(|symbol| symbol.name != "fake"),
                "malformed Astro source leaked code: {source:?}"
            );
        }
    }

    #[test]
    fn astro_script_close_scanner_ignores_strings_templates_and_comments() {
        let source = "<script>\n\
                      const a = '</script>';\n\
                      const b = \"</SCRIPT>\";\n\
                      const c = `literal </script> ${1 + 1}`;\n\
                      // </script>\n\
                      /* </script> */\n\
                      export function retained() {}\n\
                      </script>\n";
        let facts = parse_file("Lexical.astro", source).unwrap();
        let retained = facts
            .symbols
            .iter()
            .find(|symbol| symbol.name == "retained")
            .expect("lexically valid closing tag must be found");
        assert_eq!(
            retained.start_byte,
            source.find("function retained").unwrap()
        );
        assert_eq!(retained.start_line, 7);
    }

    #[test]
    fn extracts_arkts_component_struct_members_and_dsl_calls() {
        let facts = parse_file(
            "entry/src/main/ets/pages/Index.ets",
            "@Entry\n\
             @Component\n\
             struct Counter {\n\
               @State count: number = 0\n\
               increment() { this.count++ }\n\
               build() {\n\
                 Column() {\n\
                   TodoRow()\n\
                   Button('Add').onClick(this.increment)\n\
                 }\n\
               }\n\
             }\n\
             @Component\n\
             struct TodoRow { build() { Text('row') } }\n",
        )
        .unwrap();
        assert_eq!(facts.language, Language::ArkTs);
        for qualified_name in [
            "Counter",
            "Counter.count",
            "Counter.increment",
            "Counter.build",
            "TodoRow",
            "TodoRow.build",
        ] {
            assert!(facts
                .symbols
                .iter()
                .any(|symbol| symbol.qualified_name == qualified_name));
        }
        assert!(facts
            .unresolved_calls
            .iter()
            .any(|call| call.callee_name == "TodoRow"));
        assert!(facts
            .unresolved_calls
            .iter()
            .all(|call| !matches!(call.callee_name.as_str(), "Column" | "Button" | "Text")));
    }

    #[test]
    fn infers_typescript_receiver_type_from_local_constructor() {
        let facts = parse_file(
            "src/demo.ts",
            "class UserService { save() {} }\n\
             function run() { const service = new UserService(); service.save(); }\n",
        )
        .unwrap();
        let call = facts
            .unresolved_calls
            .iter()
            .find(|call| call.callee_name == "save")
            .unwrap();
        assert_eq!(call.receiver_type.as_deref(), Some("UserService"));
    }

    #[test]
    fn infers_receiver_types_across_constructor_dialects() {
        for (file, source, callee, expected) in [
            (
                "Demo.java",
                "class UserService { void save() {} }\n\
                 class Demo { void run() { UserService service = new UserService(); service.save(); } }\n",
                "save",
                "UserService",
            ),
            (
                "demo.py",
                "class UserService:\n\
                 \x20   def save(self):\n\
                 \x20       pass\n\
                 def run():\n\
                 \x20   service = UserService()\n\
                 \x20   service.save()\n",
                "save",
                "UserService",
            ),
            (
                "demo.rs",
                "struct UserService;\n\
                 impl UserService { fn new() -> Self { Self } fn save(&self) {} }\n\
                 fn run() { let service = UserService::new(); service.save(); }\n",
                "save",
                "UserService",
            ),
        ] {
            let facts = parse_file(file, source).unwrap();
            let call = facts
                .unresolved_calls
                .iter()
                .find(|call| call.callee_name == callee)
                .unwrap_or_else(|| panic!("{file} did not extract {callee}"));
            assert_eq!(
                call.receiver_type.as_deref(),
                Some(expected),
                "receiver inference failed for {file}"
            );
        }
    }

    #[test]
    fn comment_only_edit_preserves_ids() {
        let before = parse_file("x.ts", "function run() {}\n").unwrap();
        let after = parse_file("x.ts", "// heading\nfunction run() {}\n").unwrap();
        assert_eq!(before.symbols[1].id, after.symbols[1].id);
    }

    #[test]
    fn extracts_python_symbols_and_calls() {
        let facts = parse_file(
            "app/main.py",
            "class Greeter:\n    def greet(self):\n        helper()\n\ndef helper():\n    pass\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_rust_symbols_impl_methods_and_calls() {
        let facts = parse_file(
            "src/lib.rs",
            "struct Greeter;\nimpl Greeter { fn greet(&self) { helper(); } }\nfn helper() {}\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn overload_ids_use_signatures_not_source_positions() {
        let before = parse_file(
            "overloads.ts",
            "function parse(value: string): string;\nfunction parse(value: number): string;\n",
        )
        .unwrap();
        let after = parse_file(
            "overloads.ts",
            "// moved\nfunction parse(value: string): string;\n\nfunction parse(value: number): string;\n",
        )
        .unwrap();
        let before_ids: Vec<_> = before
            .symbols
            .iter()
            .skip(1)
            .map(|symbol| &symbol.id)
            .collect();
        let after_ids: Vec<_> = after
            .symbols
            .iter()
            .skip(1)
            .map(|symbol| &symbol.id)
            .collect();
        assert_eq!(before_ids, after_ids);
        assert_ne!(before_ids[0], before_ids[1]);
    }

    #[test]
    fn duplicate_declarations_get_deterministic_unique_identities() {
        let source = "static int helper(void) { return 1; }\n\
                      static int helper(void) { return 2; }\n";
        let moved = format!("// generated alternatives\n{source}");
        let before = parse_file("generated.c", source).unwrap();
        let after = parse_file("generated.c", &moved).unwrap();
        let before_helpers = before
            .symbols
            .iter()
            .filter(|symbol| symbol.name == "helper")
            .map(|symbol| (&symbol.id, &symbol.semantic_key))
            .collect::<Vec<_>>();
        let after_helpers = after
            .symbols
            .iter()
            .filter(|symbol| symbol.name == "helper")
            .map(|symbol| (&symbol.id, &symbol.semantic_key))
            .collect::<Vec<_>>();

        assert_eq!(before_helpers.len(), 2);
        assert_ne!(before_helpers[0].0, before_helpers[1].0);
        assert_ne!(before_helpers[0].1, before_helpers[1].1);
        assert_eq!(before_helpers, after_helpers);
    }

    #[test]
    fn extracts_go_declarations_methods_and_calls() {
        let facts = parse_file(
            "server/server.go",
            "package server\n\ntype Server struct{}\nfunc (s *Server) Run() { helper() }\nfunc helper() {}\n",
        )
        .unwrap();
        let symbols: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.kind))
            .collect();
        assert!(symbols.contains(&("Server", SymbolKind::Struct)));
        assert!(symbols.contains(&("Run", SymbolKind::Method)));
        assert!(symbols.contains(&("helper", SymbolKind::Function)));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_dart_classes_functions_methods_and_calls() {
        let source = "class Service {\n\
               void run() { helper(); }\n\
             }\n\
             void helper() {}\n\
             void boot() { helper(); }\n";
        let facts = parse_file("service.dart", source).unwrap();
        assert!(facts
            .symbols
            .iter()
            .any(|symbol| symbol.kind == SymbolKind::Class && symbol.name == "Service"));
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.kind == SymbolKind::Method
                && symbol.name == "run"
                && symbol.qualified_name == "Service.run"
        }));
        assert!(facts
            .symbols
            .iter()
            .any(|symbol| symbol.kind == SymbolKind::Function && symbol.name == "helper"));
        assert!(facts
            .unresolved_calls
            .iter()
            .any(|call| call.callee_name == "helper"));
    }

    #[test]
    fn extracts_java_declarations_methods_and_calls() {
        let facts = parse_file(
            "src/Greeter.java",
            "class Greeter { void greet() { helper(); } void helper() {} }\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"Greeter.helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_csharp_declarations_methods_and_calls() {
        let facts = parse_file(
            "src/Greeter.cs",
            "class Greeter { void Greet() { Helper(); } void Helper() {} }\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.Greet"));
        assert!(names.contains(&"Greeter.Helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "Helper");
    }

    #[test]
    fn extracts_c_functions_structs_and_calls() {
        let facts = parse_file(
            "src/main.c",
            "struct State { int ready; };\nvoid helper(void) {}\nint run(void) { helper(); return 0; }\n",
        )
        .unwrap();
        let symbols: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| (symbol.name.as_str(), symbol.kind))
            .collect();
        assert!(symbols.contains(&("State", SymbolKind::Struct)));
        assert!(symbols.contains(&("helper", SymbolKind::Function)));
        assert!(symbols.contains(&("run", SymbolKind::Function)));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_cpp_classes_methods_and_calls() {
        let facts = parse_file(
            "src/greeter.cpp",
            "class Greeter { public: void greet() { helper(); } void helper() {} };\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"Greeter.helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_ruby_classes_methods_and_calls() {
        let facts = parse_file(
            "lib/greeter.rb",
            "class Greeter\n  def greet\n    helper()\n  end\n  def helper\n  end\nend\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"Greeter.helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_php_classes_methods_and_calls() {
        let facts = parse_file(
            "src/Greeter.php",
            "<?php\nclass Greeter { function greet() { $this->helper(); } function helper() {} }\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"Greeter.helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_swift_types_functions_and_calls() {
        let facts = parse_file(
            "Sources/Greeter.swift",
            "struct Greeter { func greet() { helper() } func helper() {} }\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"Greeter.helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_lua_functions_and_calls() {
        let facts = parse_file(
            "src/runner.lua",
            "local function helper() end\nlocal function run() helper() end\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"run"));
        assert!(!names.contains(&"function"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_kotlin_classes_functions_and_calls() {
        let facts = parse_file(
            "src/Greeter.kt",
            "class Greeter {\n  fun greet() {\n    helper()\n  }\n\n  fun helper() {}\n}\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"Greeter.helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_scala_classes_functions_and_calls() {
        let facts = parse_file(
            "src/Greeter.scala",
            "class Greeter { def greet(): Unit = helper(); def helper(): Unit = () }\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.qualified_name.as_str())
            .collect();
        assert!(names.contains(&"Greeter"));
        assert!(names.contains(&"Greeter.greet"));
        assert!(names.contains(&"Greeter.helper"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn extracts_r_assigned_functions_and_calls() {
        let facts = parse_file(
            "src/runner.R",
            "helper <- function() 1\nrun <- function() helper()\n",
        )
        .unwrap();
        let names: Vec<_> = facts
            .symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect();
        assert!(names.contains(&"helper"));
        assert!(names.contains(&"run"));
        assert_eq!(facts.unresolved_calls[0].callee_name, "helper");
    }

    #[test]
    fn arkui_component_extensions_are_not_factory_call_results() {
        let facts = parse_file(
            "Sample.ets",
            "@Extend(Circle) function colorPicker(item: number, callback: () => void) {\n\
               callback()\n\
             }\n\
             @Entry @Component struct Sample {\n\
               colorArray: Color[] = [Color.Red]\n\
               mColor: Color = Color.Red\n\
               build() {\n\
                 ForEach(this.colorArray, (item: Color, index) => {\n\
                   Circle({ width: 20, height: 20 })\n\
                     .colorPicker(item, () => {\n\
                       this.mColor = item\n\
                     }).id('circle' + index)\n\
                 })\n\
               }\n\
             }\n",
        )
        .unwrap();
        let color_picker = facts
            .unresolved_calls
            .iter()
            .find(|call| call.callee_name == "colorPicker")
            .unwrap();
        assert_eq!(color_picker.receiver_call_start_byte, None);
        assert!(facts
            .callback_arguments
            .iter()
            .any(|argument| argument.callee_name == "colorPicker"));
    }

    #[test]
    fn repeated_receiver_names_are_partitioned_by_lexical_scope() {
        let mut source = String::from("class Service { run() {} }\n");
        for index in 0..5_000 {
            source.push_str(&format!(
                "function sibling{index}() {{ let value: Service; value.run(); }}\n"
            ));
        }
        let tree = parse_tree(Language::TypeScript, &source).unwrap();
        let mut bindings = ReceiverBindingMap::new();
        collect_receiver_bindings(
            tree.root_node(),
            source.as_bytes(),
            &FactoryReturnMap::new(),
            &CollectionElementMap::new(),
            &mut bindings,
        );

        let value_buckets = bindings
            .iter()
            .filter(|((name, _, _), _)| name == "value")
            .collect::<Vec<_>>();
        assert_eq!(value_buckets.len(), 5_000);
        assert!(value_buckets
            .iter()
            .all(|(_, scoped_bindings)| scoped_bindings.len() == 1));
    }

    #[test]
    fn extracts_exact_c_function_pointer_facts() {
        let source = r#"
typedef int (*handler_t)(int);
typedef int handler_fn(int);
typedef struct CallbackBox { handler_t callback; } CallbackBox;
typedef struct Ops {
  int tag;
  int (*run)(int);
  handler_t reset;
  struct Next *next;
  handler_fn plain;
  handler_fn *typed;
} Ops;
static int leaf(int x) { return x; }
static Ops table[] = {
  { 1, leaf, &leaf, 0 },
  { .tag = 2, .run = leaf, .reset = &leaf },
};
static handler_t callbacks[] = { leaf, [2] = (handler_t)leaf };
void store_callback(CallbackBox *box, handler_t callback) { box->callback = callback; }
void wire_callback(CallbackBox *box) { store_callback(box, leaf); }
int invoke_callback(CallbackBox *box, int x) { return box->callback(x); }
void install(Ops *ops, Ops *other) {
  ops->run = &leaf;
  ops->reset = leaf;
  ops->typed = leaf;
  ops->run = other->reset;
}
int dispatch(Ops *ops, int x) { return ops->next->run(x) + ops->reset(x); }
int array_dispatch(int slot, int x) { return callbacks[slot](x); }
"#;
        let facts = parse_file("dispatch.c", source).unwrap();
        let pointers = &facts.c_function_pointers;
        assert_eq!(pointers.typedefs.len(), 2);
        assert!(pointers
            .typedefs
            .iter()
            .any(|fact| fact.name == "handler_t" && fact.pointer));
        assert!(pointers
            .typedefs
            .iter()
            .any(|fact| fact.name == "handler_fn" && !fact.pointer));

        let layout = pointers
            .layouts
            .iter()
            .find(|layout| layout.type_name == "Ops")
            .unwrap();
        assert_eq!(layout.fields.len(), 6);
        assert_eq!(
            layout
                .fields
                .iter()
                .map(|field| (field.name.as_str(), field.index, field.function_pointer))
                .collect::<Vec<_>>(),
            vec![
                ("tag", 0, false),
                ("run", 1, true),
                ("reset", 2, true),
                ("next", 3, false),
                ("plain", 4, false),
                ("typed", 5, true),
            ]
        );
        assert_eq!(layout.fields[3].value_type.as_deref(), Some("Next"));

        assert_eq!(pointers.bindings.len(), 7);
        assert!(pointers.bindings.iter().any(|binding| {
            binding.receiver_type.as_deref() == Some("Ops")
                && binding.receiver_path == ["table"]
                && binding.field_index == Some(1)
                && binding.target_name == "leaf"
        }));
        assert_eq!(pointers.propagations.len(), 1);
        let propagation = &pointers.propagations[0];
        assert_eq!(propagation.target_receiver_type.as_deref(), Some("Ops"));
        assert_eq!(propagation.target_receiver_path, ["ops"]);
        assert_eq!(propagation.target_field_name, "run");
        assert_eq!(propagation.source_receiver_type.as_deref(), Some("Ops"));
        assert_eq!(propagation.source_receiver_path, ["other"]);
        assert_eq!(propagation.source_field_name, "reset");
        assert_eq!(pointers.arrays.len(), 1);
        assert_eq!(pointers.arrays[0].name, "callbacks");
        assert_eq!(pointers.arrays[0].element_type, "handler_t");
        assert!(!pointers.arrays[0].pointer_declarator);
        assert_eq!(pointers.arrays[0].targets.len(), 2);
        assert_eq!(pointers.array_dispatches.len(), 1);
        assert_eq!(pointers.array_dispatches[0].name, "callbacks");
        assert_eq!(pointers.formal_storages.len(), 1);
        assert_eq!(pointers.formal_storages[0].parameter_index, 1);
        assert_eq!(pointers.formal_storages[0].field_name, "callback");
        assert!(pointers.arguments.iter().any(|argument| {
            argument.callee_name == "store_callback"
                && argument.argument_index == 1
                && argument.target_name == "leaf"
        }));
        assert!(pointers.bindings.iter().any(|binding| {
            binding.receiver_path == ["table"]
                && binding.field_name.as_deref() == Some("reset")
                && binding.target_name == "leaf"
        }));
        assert!(pointers.bindings.iter().any(|binding| {
            binding.receiver_type.as_deref() == Some("Ops")
                && binding.receiver_path == ["ops"]
                && binding.field_name.as_deref() == Some("run")
                && binding.target_name == "leaf"
        }));

        assert_eq!(pointers.dispatches.len(), 3);
        assert!(pointers.dispatches.iter().any(|dispatch| {
            dispatch.receiver_type.as_deref() == Some("Ops")
                && dispatch.receiver_path == ["ops", "next"]
                && dispatch.field_name == "run"
                && dispatch.proven_function_pointer
        }));
        assert!(pointers.dispatches.iter().all(|dispatch| {
            facts
                .unresolved_calls
                .iter()
                .any(|call| call.start_byte == dispatch.site_start_byte && !call.resolvable)
        }));
    }
}
