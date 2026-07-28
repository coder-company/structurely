use crate::model::{
    Evidence, FileFacts, Language, Relationship, RelationshipKind, SourceSpan, Symbol, SymbolKind,
    UnresolvedCall, UnresolvedReference,
};
use crate::semantic::enrich_file_facts;
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
    let embedded_source = matches!(language, Language::Vue | Language::Svelte)
        .then(|| embedded_script_source(source));
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
    let mut symbol_owners = symbols
        .iter()
        .skip(1)
        .map(|symbol| ((symbol.start_byte, symbol.end_byte), symbol.id.clone()))
        .collect::<HashMap<_, _>>();
    if language == Language::Dart {
        collect_dart_body_owners(tree.root_node(), &symbols, &mut symbol_owners);
    }
    let mut receiver_bindings = HashMap::<String, Vec<(usize, String)>>::new();
    collect_receiver_bindings(tree.root_node(), source_bytes, &mut receiver_bindings);
    let module_bindings = collect_module_bindings(tree.root_node(), source_bytes, language);
    let call_context = CallCollectionContext {
        source: source_bytes,
        file: relative_path,
        symbol_owners: &symbol_owners,
        receiver_bindings: &receiver_bindings,
        module_bindings: &module_bindings,
        file_symbol_id: &file_symbol.id,
    };
    collect_calls(tree.root_node(), &call_context, &mut unresolved_calls);
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
        unresolved_references,
        dynamic_events: Vec::new(),
        literal_bindings: Vec::new(),
        module_exports: Vec::new(),
    };
    enrich_file_facts(tree.root_node(), source_bytes, &mut facts);
    Ok(facts)
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
        Language::Vue | Language::Svelte => tree_sitter_typescript::LANGUAGE_TSX,
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

struct CallCollectionContext<'a> {
    source: &'a [u8],
    file: &'a str,
    symbol_owners: &'a HashMap<(usize, usize), String>,
    receiver_bindings: &'a HashMap<String, Vec<(usize, String)>>,
    module_bindings: &'a HashMap<String, String>,
    file_symbol_id: &'a str,
}

fn collect_calls(
    node: Node<'_>,
    context: &CallCollectionContext<'_>,
    output: &mut Vec<UnresolvedCall>,
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
                let resolvable = !is_parameter_invocation(node, &name, context.source);
                let mut owner = node.parent();
                let mut caller_id = None;
                while let Some(ancestor) = owner {
                    if let Some(symbol_id) = context
                        .symbol_owners
                        .get(&(ancestor.start_byte(), ancestor.end_byte()))
                    {
                        caller_id = Some(symbol_id.as_str());
                        break;
                    }
                    owner = ancestor.parent();
                }
                output.push(UnresolvedCall {
                    caller_id: caller_id.unwrap_or(context.file_symbol_id).to_owned(),
                    callee_name: name,
                    receiver_type: receiver_type_hint(
                        function,
                        node,
                        context.source,
                        context.receiver_bindings,
                    ),
                    target_file_hint: call_receiver_name(function, context.source)
                        .and_then(|receiver| context.module_bindings.get(&receiver).cloned()),
                    provenance: "tree-sitter/name-resolution".to_owned(),
                    confidence: 1.0,
                    explanation: "direct call expression".to_owned(),
                    resolvable,
                    file: context.file.to_owned(),
                    line: node.start_position().row + 1,
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, context, output);
    }
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

fn is_parameter_invocation(call: Node<'_>, name: &str, source: &[u8]) -> bool {
    let mut ancestor = call.parent();
    while let Some(node) = ancestor {
        if matches!(
            node.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "function_definition"
        ) {
            return node
                .child_by_field_name("parameters")
                .is_some_and(|parameters| {
                    let mut identifiers = Vec::new();
                    collect_identifier_nodes(parameters, source, &mut identifiers);
                    identifiers
                        .iter()
                        .any(|(parameter, _, _)| parameter == name)
                });
        }
        ancestor = node.parent();
    }
    false
}

fn receiver_type_hint(
    function: Node<'_>,
    call: Node<'_>,
    source: &[u8],
    receiver_bindings: &HashMap<String, Vec<(usize, String)>>,
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
    if !matches!(receiver.kind(), "identifier" | "simple_identifier" | "name") {
        return None;
    }
    let variable = node_text(receiver, source);
    receiver_bindings.get(&variable).and_then(|bindings| {
        bindings
            .iter()
            .rev()
            .find(|(position, _)| *position < call.start_byte())
            .map(|(_, receiver_type)| receiver_type.clone())
    })
}

fn collect_receiver_bindings(
    node: Node<'_>,
    source: &[u8],
    output: &mut HashMap<String, Vec<(usize, String)>>,
) {
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
            let variable = node_text(name, source);
            let receiver_type = value
                .and_then(|value| constructor_type(value, source))
                .or_else(|| declared_variable_type(node, source));
            if let Some(receiver_type) = receiver_type {
                output
                    .entry(variable)
                    .or_default()
                    .push((node.start_byte(), receiver_type));
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_receiver_bindings(child, source, output);
    }
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
    let mut declaration = node.parent();
    while let Some(parent) = declaration {
        if matches!(
            parent.kind(),
            "local_variable_declaration" | "variable_declaration"
        ) {
            return parent
                .child_by_field_name("type")
                .map(|kind| node_text(kind, source));
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
}
