use crate::model::{
    Evidence, FileFacts, Language, Relationship, RelationshipKind, SourceSpan, Symbol, SymbolKind,
    UnresolvedCall,
};
use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

pub(crate) fn parse_file(relative_path: &str, source: &str) -> Result<FileFacts> {
    let path = Path::new(relative_path);
    let language = Language::from_path(path)
        .ok_or_else(|| anyhow!("unsupported source language: {relative_path}"))?;
    let tree = parse_tree(language, source)?;
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
    collect_calls(
        tree.root_node(),
        source_bytes,
        relative_path,
        &symbols,
        &mut unresolved_calls,
    );

    Ok(FileFacts {
        path: relative_path.to_owned(),
        content_hash,
        language,
        symbols,
        relationships,
        unresolved_calls,
    })
}

fn parse_tree(language: Language, source: &str) -> Result<Tree> {
    let mut parser = Parser::new();
    let grammar = match language {
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX,
        Language::JavaScript | Language::Jsx => tree_sitter_javascript::LANGUAGE,
        Language::Python => tree_sitter_python::LANGUAGE,
        Language::Rust => tree_sitter_rust::LANGUAGE,
    };
    parser
        .set_language(&grammar.into())
        .context("load tree-sitter grammar")?;
    parser
        .parse(source, None)
        .ok_or_else(|| anyhow!("tree-sitter returned no syntax tree"))
}

fn collect_symbols(
    node: Node<'_>,
    source: &[u8],
    language: Language,
    file: &str,
    container: Option<&str>,
    output: &mut Vec<Symbol>,
) {
    let declaration = declaration_at(node, source, container);
    let declaration_container = declaration
        .as_ref()
        .map(|(qualified, _, _)| qualified.clone())
        .or_else(|| container.map(str::to_owned));
    let next_container = rust_impl_container(node, source, container).or(declaration_container);

    if let Some((qualified_name, kind, name_node)) = declaration {
        let name = node_text(name_node, source);
        output.push(Symbol::new(
            language,
            kind,
            name,
            qualified_name,
            file,
            span(node),
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
) -> Option<(String, SymbolKind, Node<'a>)> {
    let (kind, name_node) = match node.kind() {
        "class_declaration" | "class_definition" => {
            (SymbolKind::Class, node.child_by_field_name("name")?)
        }
        "interface_declaration" => (SymbolKind::Interface, node.child_by_field_name("name")?),
        "function_declaration"
        | "generator_function_declaration"
        | "function_definition"
        | "function_item" => (SymbolKind::Function, node.child_by_field_name("name")?),
        "method_definition" | "method_signature" => {
            (SymbolKind::Method, node.child_by_field_name("name")?)
        }
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
        _ => return None,
    };
    let name = node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    let qualified = container
        .map(|parent| format!("{parent}.{name}"))
        .unwrap_or_else(|| name.clone());
    Some((qualified, kind, name_node))
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

fn collect_calls(
    node: Node<'_>,
    source: &[u8],
    file: &str,
    symbols: &[Symbol],
    output: &mut Vec<UnresolvedCall>,
) {
    if matches!(node.kind(), "call_expression" | "call") {
        if let Some(function) = node.child_by_field_name("function") {
            if let Some(name) = call_name(function, source) {
                let caller = symbols
                    .iter()
                    .filter(|symbol| {
                        symbol.kind != SymbolKind::File
                            && symbol.start_byte <= node.start_byte()
                            && symbol.end_byte >= node.end_byte()
                    })
                    .min_by_key(|symbol| symbol.end_byte - symbol.start_byte)
                    .or_else(|| symbols.first());
                if let Some(caller) = caller {
                    output.push(UnresolvedCall {
                        caller_id: caller.id.clone(),
                        callee_name: name,
                        file: file.to_owned(),
                        line: node.start_position().row + 1,
                    });
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_calls(child, source, file, symbols, output);
    }
}

fn call_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "property_identifier" | "field_identifier" | "type_identifier" => {
            Some(node_text(node, source))
        }
        "member_expression" => node
            .child_by_field_name("property")
            .map(|property| node_text(property, source)),
        "attribute" => node
            .child_by_field_name("attribute")
            .map(|attribute| node_text(attribute, source)),
        "field_expression" => node
            .child_by_field_name("field")
            .map(|field| node_text(field, source)),
        "scoped_identifier" | "scoped_type_identifier" => node
            .child_by_field_name("name")
            .map(|name| node_text(name, source)),
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
}
