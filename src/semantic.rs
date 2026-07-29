use crate::model::{
    ArkuiBuilderDeclarationFact, ArkuiBuilderParamAssignmentFact, ArkuiBuilderParamDeclarationFact,
    ArkuiBuilderParamInvocationFact, DynamicEventFact, EventAction, EventChannel, Evidence,
    FileFacts, Language, LiteralBindingFact, ModuleExportFact, Relationship, RelationshipKind,
    SourceSpan, Symbol, SymbolKind, UnresolvedCall, UnresolvedReference,
};
use std::{collections::HashSet, path::Path};
use tree_sitter::Node;

pub(crate) fn enrich_file_facts(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    match facts.language {
        Language::TypeScript
        | Language::Tsx
        | Language::JavaScript
        | Language::Jsx
        | Language::Vue
        | Language::Svelte
        | Language::ArkTs => {
            if contains_bytes(source, b"export const ")
                || contains_bytes(source, b"static readonly ")
                || (contains_bytes(source, b"export ") && contains_bytes(source, b" from "))
            {
                collect_exported_literal_bindings(root, source, facts);
            }
            collect_javascript_registrations(root, source, facts);
            collect_javascript_function_references(root, source, facts);
            collect_nestjs_routes(root, source, facts);
            if facts.language == Language::ArkTs {
                collect_arkui_routes(root, source, facts);
                collect_arkui_style_helper_calls(root, source, facts);
                collect_arkui_builder_registrations(root, source, facts);
                collect_ohos_emitter_semantics(root, source, facts);
                collect_arkui_semantics(root, source, facts);
                remove_arkui_intrinsic_calls(facts);
            }
            if matches!(
                facts.language,
                Language::Tsx | Language::Jsx | Language::Vue | Language::Svelte
            ) {
                collect_react_router_jsx(root, source, facts);
                collect_react_runtime_edges(root, source, facts);
            }
            if matches!(facts.language, Language::Vue | Language::Svelte) {
                collect_component_template_edges(root, source, facts);
            }
        }
        Language::Python => {
            collect_fastapi_routes(root, source, facts);
            collect_django_routes(root, source, facts);
        }
        _ => {}
    }
}

fn contains_bytes(source: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && source.windows(needle.len()).any(|window| window == needle)
}

fn collect_ohos_emitter_semantics(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let bindings = ohos_emitter_bindings(root, source);
    if bindings.is_empty() {
        return;
    }
    facts
        .dynamic_events
        .retain(|event| !bindings.contains(&event.receiver));
    let imports = ohos_emitter_value_imports(facts);
    let constructors = ohos_emitter_descriptor_constructors(root, source);
    let descriptors = ohos_emitter_descriptors(root, source, &imports, &constructors);
    let mut callback_ordinal = 0usize;
    collect_ohos_emitter_calls(
        root,
        source,
        facts,
        &bindings,
        &descriptors,
        &imports,
        &constructors,
        &mut callback_ordinal,
    );
}

fn collect_exported_literal_bindings(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "export_statement" {
        collect_forwarded_module_exports(node, source, facts);
    }
    if node.kind() == "variable_declarator" && enclosing_export_const(node, source) {
        if let (Some(name), Some(value)) = (
            node.child_by_field_name("name")
                .filter(|name| name.kind() == "identifier")
                .map(|name| text(name, source)),
            node.child_by_field_name("value"),
        ) {
            if let Some(channel) = canonical_exported_emitter_channel(value, source) {
                facts.literal_bindings.push(LiteralBindingFact {
                    export_name: name,
                    member_path: String::new(),
                    channel,
                });
            }
        }
    }
    if node.kind() == "public_field_definition" {
        let declaration_text = text(node, source);
        let declaration_prefix = declaration_text
            .split_once('=')
            .map(|(prefix, _)| prefix)
            .unwrap_or_default();
        let is_static_readonly = declaration_prefix
            .split_whitespace()
            .any(|word| word == "static")
            && declaration_prefix
                .split_whitespace()
                .any(|word| word == "readonly");
        if !is_static_readonly {
            return collect_exported_literal_binding_children(node, source, facts);
        }
        if let (Some(class_name), Some(member), Some(value)) = (
            enclosing_exported_class_name(node, source),
            node.child_by_field_name("name")
                .filter(|name| matches!(name.kind(), "property_identifier" | "identifier"))
                .map(|name| text(name, source)),
            node.child_by_field_name("value"),
        ) {
            if let Some(channel) = canonical_exported_emitter_channel(value, source) {
                facts.literal_bindings.push(LiteralBindingFact {
                    export_name: class_name,
                    member_path: member,
                    channel,
                });
            }
        }
    }
    collect_exported_literal_binding_children(node, source, facts);
}

fn collect_forwarded_module_exports(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let Some(target_file_hint) = node
        .child_by_field_name("source")
        .and_then(|source_node| string_literal(source_node, source))
    else {
        return;
    };
    let statement = text(node, source);
    let Some((clause, _)) = statement
        .trim()
        .strip_prefix("export")
        .and_then(|body| body.rsplit_once(" from "))
    else {
        return;
    };
    let clause = clause.trim();
    if clause == "*" {
        facts.module_exports.push(ModuleExportFact {
            export_name: String::new(),
            target_file_hint,
            target_name: String::new(),
            is_star: true,
        });
        return;
    }
    let Some(body) = clause
        .strip_prefix('{')
        .and_then(|body| body.strip_suffix('}'))
    else {
        return;
    };
    for specifier in body.split(',') {
        let words = specifier.split_whitespace().collect::<Vec<_>>();
        let (target_name, export_name) = match words.as_slice() {
            [name] => (*name, *name),
            [target, "as", exported] => (*target, *exported),
            _ => continue,
        };
        if !is_javascript_identifier(target_name) || !is_javascript_identifier(export_name) {
            continue;
        }
        facts.module_exports.push(ModuleExportFact {
            export_name: export_name.to_owned(),
            target_file_hint: target_file_hint.clone(),
            target_name: target_name.to_owned(),
            is_star: false,
        });
    }
}

fn collect_exported_literal_binding_children(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_exported_literal_bindings(child, source, facts);
    }
}

fn enclosing_exported_class_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(candidate) = ancestor {
        if candidate.kind() == "class_declaration" {
            let exported = candidate.parent().is_some_and(|parent| {
                parent.kind() == "export_statement"
                    && text(parent, source)
                        .trim_start()
                        .starts_with("export class ")
            });
            return exported.then(|| {
                candidate
                    .child_by_field_name("name")
                    .map(|name| text(name, source))
                    .unwrap_or_default()
            });
        }
        if matches!(
            candidate.kind(),
            "program" | "statement_block" | "function_declaration" | "method_definition"
        ) {
            return None;
        }
        ancestor = candidate.parent();
    }
    None
}

fn enclosing_export_const(node: Node<'_>, source: &[u8]) -> bool {
    let mut ancestor = node.parent();
    while let Some(candidate) = ancestor {
        if candidate.kind() == "export_statement" {
            return text(candidate, source)
                .trim_start()
                .starts_with("export const ");
        }
        if matches!(
            candidate.kind(),
            "program"
                | "statement_block"
                | "class_body"
                | "function_declaration"
                | "method_definition"
        ) {
            return false;
        }
        ancestor = candidate.parent();
    }
    false
}

fn canonical_exported_emitter_channel(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(value) = string_literal(node, source) {
        return Some(format!("s:{value}"));
    }
    if matches!(
        node.kind(),
        "number" | "number_literal" | "integer_literal" | "decimal_integer_literal"
    ) {
        let value = text(node, source).replace('_', "").parse::<i128>().ok()?;
        return Some(format!("n:{value}"));
    }
    if node.kind() != "object" {
        return None;
    }
    let mut cursor = node.walk();
    let mut event_ids = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "pair")
        .filter_map(|pair| {
            let key = pair.child_by_field_name("key")?;
            (text(key, source).trim_matches(['\'', '"']) == "eventId")
                .then(|| pair.child_by_field_name("value"))
                .flatten()
        })
        .collect::<Vec<_>>();
    (event_ids.len() == 1)
        .then(|| event_ids.remove(0))
        .and_then(|value| canonical_exported_emitter_channel(value, source))
}

#[derive(Clone)]
struct ImportedEmitterValue {
    target_file_hint: String,
    export_name: String,
}

fn ohos_emitter_value_imports(
    facts: &FileFacts,
) -> std::collections::HashMap<String, ImportedEmitterValue> {
    let mut candidates = std::collections::HashMap::<String, Vec<ImportedEmitterValue>>::new();
    for reference in &facts.unresolved_references {
        if reference.kind != RelationshipKind::Imports {
            continue;
        }
        let Some(target_file_hint) = reference.target_file_hint.clone() else {
            continue;
        };
        candidates
            .entry(reference.binding_name.clone())
            .or_default()
            .push(ImportedEmitterValue {
                target_file_hint,
                export_name: reference.target_name.clone(),
            });
    }
    candidates
        .into_iter()
        .filter_map(|(binding, mut values)| {
            values.sort_by(|left, right| {
                left.target_file_hint
                    .cmp(&right.target_file_hint)
                    .then_with(|| left.export_name.cmp(&right.export_name))
            });
            values.dedup_by(|left, right| {
                left.target_file_hint == right.target_file_hint
                    && left.export_name == right.export_name
            });
            (values.len() == 1).then(|| (binding, values.remove(0)))
        })
        .collect()
}

fn ohos_emitter_bindings(node: Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut bindings = HashSet::new();
    collect_ohos_emitter_bindings(node, source, &mut bindings);
    bindings
}

fn collect_ohos_emitter_bindings(node: Node<'_>, source: &[u8], bindings: &mut HashSet<String>) {
    if node.kind() == "import_statement" {
        let module = node
            .child_by_field_name("source")
            .and_then(|source_node| string_literal(source_node, source));
        let statement = text(node, source);
        let clause = statement
            .strip_prefix("import")
            .and_then(|body| body.rsplit_once(" from ").map(|(clause, _)| clause.trim()))
            .filter(|clause| !clause.starts_with("type "));
        match (module.as_deref(), clause) {
            (Some("@ohos.events.emitter"), Some(clause)) => {
                if let Some(binding) = arkui_default_or_namespace_binding(clause) {
                    bindings.insert(binding);
                }
            }
            (Some("@kit.BasicServicesKit"), Some(clause)) => {
                bindings.extend(named_import_bindings(clause, "emitter"));
            }
            _ => {}
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_ohos_emitter_bindings(child, source, bindings);
    }
}

fn named_import_bindings(clause: &str, imported_name: &str) -> Vec<String> {
    let Some(body) = clause
        .strip_prefix('{')
        .and_then(|body| body.strip_suffix('}'))
    else {
        return Vec::new();
    };
    body.split(',')
        .filter_map(|specifier| {
            let mut words = specifier.split_whitespace();
            (words.next()? == imported_name)
                .then(|| match (words.next(), words.next()) {
                    (Some("as"), Some(alias)) => alias,
                    (None, None) => imported_name,
                    _ => "",
                })
                .filter(|binding| is_javascript_identifier(binding))
                .map(str::to_owned)
        })
        .collect()
}

fn ohos_emitter_descriptors(
    node: Node<'_>,
    source: &[u8],
    imports: &std::collections::HashMap<String, ImportedEmitterValue>,
    constructors: &HashSet<String>,
) -> std::collections::HashMap<String, EventChannel> {
    let mut candidates = std::collections::HashMap::<String, Vec<EventChannel>>::new();
    let mut reassigned = HashSet::new();
    collect_ohos_emitter_descriptors(
        node,
        source,
        imports,
        constructors,
        &mut candidates,
        &mut reassigned,
    );
    candidates
        .into_iter()
        .filter_map(|(name, mut values)| {
            if reassigned.contains(&name) {
                return None;
            }
            values.sort_by_key(event_channel_sort_key);
            values.dedup_by(|left, right| {
                event_channel_sort_key(left) == event_channel_sort_key(right)
            });
            (values.len() == 1).then(|| (name, values.remove(0)))
        })
        .collect()
}

fn collect_ohos_emitter_descriptors(
    node: Node<'_>,
    source: &[u8],
    imports: &std::collections::HashMap<String, ImportedEmitterValue>,
    constructors: &HashSet<String>,
    output: &mut std::collections::HashMap<String, Vec<EventChannel>>,
    reassigned: &mut HashSet<String>,
) {
    if node.kind() == "variable_declarator" {
        if let (Some(name), Some(value)) = (
            node.child_by_field_name("name")
                .filter(|name| name.kind() == "identifier")
                .map(|name| text(name, source)),
            node.child_by_field_name("value"),
        ) {
            if let Some(channel) = canonical_ohos_emitter_channel(
                value,
                source,
                &Default::default(),
                imports,
                constructors,
            ) {
                let constructor_built = value.kind() == "new_expression";
                if !constructor_built || enclosing_const_declaration(node, source) {
                    if output.contains_key(&name) {
                        reassigned.insert(name.clone());
                    }
                    output.entry(name).or_default().push(channel);
                }
            }
        }
    }
    if matches!(node.kind(), "assignment" | "assignment_expression") {
        if let Some(name) = node
            .child_by_field_name("left")
            .or_else(|| node.child_by_field_name("name"))
            .and_then(|left| {
                if left.kind() == "identifier" {
                    Some(left)
                } else if left.kind() == "member_expression" {
                    left.child_by_field_name("object")
                        .filter(|object| object.kind() == "identifier")
                } else {
                    None
                }
            })
        {
            reassigned.insert(text(name, source));
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_ohos_emitter_descriptors(child, source, imports, constructors, output, reassigned);
    }
}

fn enclosing_const_declaration(node: Node<'_>, source: &[u8]) -> bool {
    let mut ancestor = node.parent();
    while let Some(candidate) = ancestor {
        if matches!(
            candidate.kind(),
            "lexical_declaration" | "variable_declaration"
        ) {
            return text(candidate, source).trim_start().starts_with("const ");
        }
        if matches!(candidate.kind(), "statement_block" | "program") {
            break;
        }
        ancestor = candidate.parent();
    }
    false
}

fn ohos_emitter_descriptor_constructors(node: Node<'_>, source: &[u8]) -> HashSet<String> {
    const MAX_CLASSES: usize = 64;
    let mut valid = std::collections::HashMap::<String, usize>::new();
    let mut conflicting = HashSet::new();
    let mut seen = 0usize;
    let mut overflowed = false;
    collect_ohos_emitter_descriptor_constructors(
        node,
        source,
        &mut valid,
        &mut conflicting,
        &mut seen,
        MAX_CLASSES,
        &mut overflowed,
    );
    if overflowed {
        return HashSet::new();
    }
    valid
        .into_iter()
        .filter_map(|(name, count)| (count == 1 && !conflicting.contains(&name)).then_some(name))
        .collect()
}

fn collect_ohos_emitter_descriptor_constructors(
    node: Node<'_>,
    source: &[u8],
    valid: &mut std::collections::HashMap<String, usize>,
    conflicting: &mut HashSet<String>,
    seen: &mut usize,
    cap: usize,
    overflowed: &mut bool,
) {
    if *overflowed {
        return;
    }
    if node.kind() == "class_declaration" {
        if !is_top_level_declaration(node) {
            return;
        }
        *seen += 1;
        if *seen > cap {
            *overflowed = true;
            return;
        }
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = text(name_node, source);
            if exact_emitter_descriptor_constructor(node, source) {
                *valid.entry(name).or_default() += 1;
            } else {
                conflicting.insert(name);
            }
        }
        return;
    }
    if matches!(
        node.kind(),
        "variable_declarator" | "function_declaration" | "generator_function_declaration"
    ) {
        if let Some(name) = node.child_by_field_name("name") {
            conflicting.insert(text(name, source));
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_ohos_emitter_descriptor_constructors(
            child,
            source,
            valid,
            conflicting,
            seen,
            cap,
            overflowed,
        );
    }
}

fn is_top_level_declaration(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        parent.kind() == "program"
            || (parent.kind() == "export_statement"
                && parent
                    .parent()
                    .is_some_and(|grandparent| grandparent.kind() == "program"))
    })
}

fn exact_emitter_descriptor_constructor(class: Node<'_>, source: &[u8]) -> bool {
    if count_this_event_id_assignments(class, source) != 1 {
        return false;
    }
    let Some(body) = class.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = body.walk();
    let constructors = body
        .named_children(&mut cursor)
        .filter(|member| {
            member.kind() == "method_definition"
                && member
                    .child_by_field_name("name")
                    .is_some_and(|name| text(name, source) == "constructor")
        })
        .collect::<Vec<_>>();
    let [constructor] = constructors.as_slice() else {
        return false;
    };
    let Some(parameters) = constructor.child_by_field_name("parameters") else {
        return false;
    };
    let mut cursor = parameters.walk();
    let parameters = parameters.named_children(&mut cursor).collect::<Vec<_>>();
    let [parameter] = parameters.as_slice() else {
        return false;
    };
    let Some(parameter_name) = simple_parameter_name(*parameter, source) else {
        return false;
    };
    let Some(constructor_body) = constructor.child_by_field_name("body") else {
        return false;
    };
    let mut cursor = constructor_body.walk();
    let statements = constructor_body
        .named_children(&mut cursor)
        .collect::<Vec<_>>();
    let [statement] = statements.as_slice() else {
        return false;
    };
    let Some(assignment) = statement
        .named_child(0)
        .filter(|child| matches!(child.kind(), "assignment_expression" | "assignment"))
    else {
        return false;
    };
    let Some(left) = assignment
        .child_by_field_name("left")
        .filter(|left| left.kind() == "member_expression")
    else {
        return false;
    };
    let is_event_id = left
        .child_by_field_name("object")
        .is_some_and(|object| object.kind() == "this")
        && left
            .child_by_field_name("property")
            .is_some_and(|property| text(property, source) == "eventId");
    let right_matches = assignment
        .child_by_field_name("right")
        .is_some_and(|right| right.kind() == "identifier" && text(right, source) == parameter_name);
    is_event_id && right_matches
}

fn simple_parameter_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() == "identifier" {
        return Some(text(node, source));
    }
    if node.kind() == "required_parameter" {
        return node
            .child_by_field_name("pattern")
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0))
            .filter(|name| name.kind() == "identifier")
            .map(|name| text(name, source));
    }
    None
}

fn count_this_event_id_assignments(node: Node<'_>, source: &[u8]) -> usize {
    let own = matches!(node.kind(), "assignment_expression" | "assignment")
        && node
            .child_by_field_name("left")
            .filter(|left| left.kind() == "member_expression")
            .is_some_and(|left| {
                left.child_by_field_name("object")
                    .is_some_and(|object| object.kind() == "this")
                    && left
                        .child_by_field_name("property")
                        .is_some_and(|property| text(property, source) == "eventId")
            });
    let mut cursor = node.walk();
    own as usize
        + node
            .named_children(&mut cursor)
            .map(|child| count_this_event_id_assignments(child, source))
            .sum::<usize>()
}

fn canonical_ohos_emitter_channel(
    node: Node<'_>,
    source: &[u8],
    descriptors: &std::collections::HashMap<String, EventChannel>,
    imports: &std::collections::HashMap<String, ImportedEmitterValue>,
    constructors: &HashSet<String>,
) -> Option<EventChannel> {
    if let Some(value) = string_literal(node, source) {
        return Some(EventChannel::Canonical(format!("s:{value}")));
    }
    if matches!(
        node.kind(),
        "number" | "number_literal" | "integer_literal" | "decimal_integer_literal"
    ) {
        let raw = text(node, source).replace('_', "");
        let value = raw.parse::<i128>().ok()?;
        return Some(EventChannel::Canonical(format!("n:{value}")));
    }
    if node.kind() == "identifier" {
        let name = text(node, source);
        return descriptors.get(&name).cloned().or_else(|| {
            (!arkui_router_binding_is_shadowed(node, &name, source))
                .then(|| imports.get(&name))
                .flatten()
                .map(|import| EventChannel::Imported {
                    target_file_hint: import.target_file_hint.clone(),
                    export_name: import.export_name.clone(),
                    member_path: String::new(),
                })
        });
    }
    if node.kind() == "member_expression" {
        let root = node
            .child_by_field_name("object")
            .filter(|object| object.kind() == "identifier")
            .map(|object| text(object, source))?;
        let member_path = node
            .child_by_field_name("property")
            .filter(|property| matches!(property.kind(), "property_identifier" | "identifier"))
            .map(|property| text(property, source))?;
        if arkui_router_binding_is_shadowed(node, &root, source) {
            return None;
        }
        let import = imports.get(&root)?;
        return Some(EventChannel::Imported {
            target_file_hint: import.target_file_hint.clone(),
            export_name: import.export_name.clone(),
            member_path,
        });
    }
    if node.kind() == "new_expression" {
        let constructor = node
            .child_by_field_name("constructor")
            .or_else(|| node.child_by_field_name("function"))
            .filter(|constructor| constructor.kind() == "identifier")?;
        let constructor_name = text(constructor, source);
        if !constructors.contains(&constructor_name)
            || parameter_shadows_binding(node, &constructor_name, source)
        {
            return None;
        }
        let arguments = node.child_by_field_name("arguments")?;
        let mut cursor = arguments.walk();
        let arguments = arguments.named_children(&mut cursor).collect::<Vec<_>>();
        let [argument] = arguments.as_slice() else {
            return None;
        };
        return canonical_ohos_emitter_channel(
            *argument,
            source,
            descriptors,
            imports,
            constructors,
        );
    }
    if node.kind() != "object" {
        return None;
    }
    let mut cursor = node.walk();
    let mut event_ids = node
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "pair")
        .filter_map(|pair| {
            let key = pair.child_by_field_name("key")?;
            (text(key, source).trim_matches(['\'', '"']) == "eventId")
                .then(|| pair.child_by_field_name("value"))
                .flatten()
        })
        .collect::<Vec<_>>();
    if event_ids.len() != 1 {
        return None;
    }
    canonical_ohos_emitter_channel(
        event_ids.remove(0),
        source,
        descriptors,
        imports,
        constructors,
    )
}

fn parameter_shadows_binding(node: Node<'_>, binding: &str, source: &[u8]) -> bool {
    let mut ancestor = node.parent();
    while let Some(scope) = ancestor {
        if let Some(parameters) = scope.child_by_field_name("parameters") {
            let mut identifiers = Vec::new();
            collect_identifier_texts(parameters, source, &mut identifiers);
            if identifiers.iter().any(|identifier| identifier == binding) {
                return true;
            }
        }
        ancestor = scope.parent();
    }
    false
}

fn event_channel_sort_key(channel: &EventChannel) -> String {
    match channel {
        EventChannel::Canonical(channel) => format!("c:{channel}"),
        EventChannel::Imported {
            target_file_hint,
            export_name,
            member_path,
        } => format!("i:{target_file_hint}:{export_name}:{member_path}"),
    }
}

fn event_channel_label(channel: &EventChannel) -> String {
    match channel {
        EventChannel::Canonical(channel) => channel.clone(),
        EventChannel::Imported { .. } => event_channel_sort_key(channel),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_ohos_emitter_calls(
    node: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    bindings: &HashSet<String>,
    descriptors: &std::collections::HashMap<String, EventChannel>,
    imports: &std::collections::HashMap<String, ImportedEmitterValue>,
    constructors: &HashSet<String>,
    callback_ordinal: &mut usize,
) {
    if matches!(
        node.kind(),
        "call_expression" | "arkui_component_expression"
    ) {
        enrich_ohos_emitter_call(
            node,
            source,
            facts,
            bindings,
            descriptors,
            imports,
            constructors,
            callback_ordinal,
        );
    } else if node.kind() == "statement_block" {
        collect_recovered_ohos_emitter_calls(
            node,
            source,
            facts,
            bindings,
            descriptors,
            imports,
            constructors,
            callback_ordinal,
        );
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_ohos_emitter_calls(
            child,
            source,
            facts,
            bindings,
            descriptors,
            imports,
            constructors,
            callback_ordinal,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_recovered_ohos_emitter_calls(
    block: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    bindings: &HashSet<String>,
    descriptors: &std::collections::HashMap<String, EventChannel>,
    imports: &std::collections::HashMap<String, ImportedEmitterValue>,
    constructors: &HashSet<String>,
    callback_ordinal: &mut usize,
) {
    let children = direct_named_children(block);
    for window in children.windows(3) {
        let receiver = text(window[0], source)
            .trim()
            .trim_end_matches(';')
            .to_owned();
        let method = text(window[1], source)
            .trim()
            .trim_start_matches('.')
            .trim_end_matches(';')
            .to_owned();
        if !bindings.contains(&receiver)
            || !matches!(method.as_str(), "on" | "once" | "emit")
            || window[2].kind() != "expression_statement"
        {
            continue;
        }
        let Some(parenthesized) = first_descendant_of_kind(window[2], "parenthesized_expression")
        else {
            continue;
        };
        let mut cursor = parenthesized.walk();
        let direct = parenthesized
            .named_children(&mut cursor)
            .collect::<Vec<_>>();
        let arguments = if direct.len() == 1 && direct[0].kind() == "sequence_expression" {
            let mut sequence_cursor = direct[0].walk();
            direct[0]
                .named_children(&mut sequence_cursor)
                .collect::<Vec<_>>()
        } else {
            direct
        };
        enrich_ohos_emitter_parts(
            window[2],
            &receiver,
            &method,
            &arguments,
            source,
            facts,
            descriptors,
            imports,
            constructors,
            callback_ordinal,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn enrich_ohos_emitter_call(
    call: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    bindings: &HashSet<String>,
    descriptors: &std::collections::HashMap<String, EventChannel>,
    imports: &std::collections::HashMap<String, ImportedEmitterValue>,
    constructors: &HashSet<String>,
    callback_ordinal: &mut usize,
) {
    let Some(function) = call
        .child_by_field_name("function")
        .filter(|function| function.kind() == "member_expression")
    else {
        return;
    };
    let Some(receiver) = function
        .child_by_field_name("object")
        .filter(|receiver| receiver.kind() == "identifier")
        .map(|receiver| text(receiver, source))
        .filter(|receiver| bindings.contains(receiver))
    else {
        return;
    };
    let method = function
        .child_by_field_name("property")
        .map(|property| text(property, source))
        .unwrap_or_default();
    let Some(arguments) = call.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = arguments.walk();
    let arguments = arguments.named_children(&mut cursor).collect::<Vec<_>>();
    enrich_ohos_emitter_parts(
        call,
        &receiver,
        &method,
        &arguments,
        source,
        facts,
        descriptors,
        imports,
        constructors,
        callback_ordinal,
    );
}

#[allow(clippy::too_many_arguments)]
fn enrich_ohos_emitter_parts(
    observation: Node<'_>,
    receiver: &str,
    method: &str,
    arguments: &[Node<'_>],
    source: &[u8],
    facts: &mut FileFacts,
    descriptors: &std::collections::HashMap<String, EventChannel>,
    imports: &std::collections::HashMap<String, ImportedEmitterValue>,
    constructors: &HashSet<String>,
    callback_ordinal: &mut usize,
) {
    if arkui_router_binding_is_shadowed(observation, receiver, source) {
        return;
    }
    let Some(channel) = arguments.first().and_then(|argument| {
        canonical_ohos_emitter_channel(*argument, source, descriptors, imports, constructors)
    }) else {
        return;
    };
    let owner = owning_callable_symbol(observation, &facts.symbols)
        .or_else(|| facts.symbols.first())
        .cloned()
        .expect("file symbol");
    match method {
        "emit" => facts.dynamic_events.push(DynamicEventFact {
            owner_id: owner.id,
            receiver: "ohos-emitter".to_owned(),
            channel,
            action: EventAction::Dispatch,
            callback_name: None,
            file: facts.path.clone(),
            line: observation.start_position().row + 1,
        }),
        "on" | "once" => {
            let Some(callback) = arguments.get(1).copied() else {
                return;
            };
            let channel_label = event_channel_label(&channel);
            let callback_name = if let Some(name) = referenced_callable_name(callback, source) {
                let target_file_hint = facts
                    .unresolved_references
                    .iter()
                    .find(|reference| {
                        reference.kind == RelationshipKind::Imports
                            && reference.binding_name == name
                    })
                    .and_then(|reference| reference.target_file_hint.clone())
                    .or_else(|| Some(facts.path.clone()));
                let receiver_type = (callback.kind() == "member_expression"
                    && callback
                        .child_by_field_name("object")
                        .is_some_and(|object| text(object, source) == "this"))
                .then(|| {
                    owner
                        .qualified_name
                        .split('.')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .filter(|receiver| !receiver.is_empty());
                facts.unresolved_calls.push(UnresolvedCall {
                    caller_id: owner.id.clone(),
                    fallback_caller_id: None,
                    callee_name: name.clone(),
                    receiver_binding: None,
                    receiver_type,
                    receiver_call_start_byte: None,
                    target_file_hint,
                    provenance: "framework/ohos-emitter-registration".to_owned(),
                    confidence: 0.97,
                    explanation: format!(
                        "{} registers Harmony emitter callback {name} on {channel_label}",
                        owner.qualified_name
                    ),
                    resolvable: true,
                    file: facts.path.clone(),
                    line: observation.start_position().row + 1,
                    start_byte: observation.start_byte(),
                });
                name
            } else if matches!(callback.kind(), "arrow_function" | "function_expression") {
                *callback_ordinal += 1;
                let name = format!("<emitter callback {channel_label}>");
                let qualified_name = format!("{}.{name}", owner.qualified_name);
                let symbol = Symbol::new_disambiguated(
                    Language::ArkTs,
                    SymbolKind::Function,
                    &name,
                    &qualified_name,
                    &facts.path,
                    span(callback),
                    &format!(
                        "ohos-emitter-callback|{}|{}|{}",
                        owner.semantic_key, channel_label, callback_ordinal
                    ),
                );
                facts.relationships.push(Relationship {
                    source_id: owner.id.clone(),
                    target_id: symbol.id.clone(),
                    kind: RelationshipKind::Calls,
                    evidence: Evidence::new(
                        "framework/ohos-emitter-inline-registration",
                        0.97,
                        format!(
                            "{} registers inline Harmony emitter callback on {channel_label}",
                            owner.qualified_name
                        ),
                        &facts.path,
                        observation.start_position().row + 1,
                    ),
                });
                facts.symbols.push(symbol);
                name
            } else {
                return;
            };
            facts.dynamic_events.push(DynamicEventFact {
                owner_id: owner.id,
                receiver: "ohos-emitter".to_owned(),
                channel,
                action: EventAction::Register,
                callback_name: Some(callback_name),
                file: facts.path.clone(),
                line: observation.start_position().row + 1,
            });
        }
        _ => {}
    }
}

#[derive(Clone)]
struct ArkuiStyleHelper {
    target: Symbol,
    owner: Option<String>,
    extended_intrinsic: Option<String>,
}

#[derive(Clone)]
struct ArkuiBuilder {
    target: Symbol,
    owner: Option<String>,
}

#[derive(Clone)]
struct ArkuiBuilderParam {
    owner: String,
    name: String,
}

fn collect_arkui_builder_registrations(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let mut builders = Vec::new();
    collect_arkui_builders(root, source, facts, &mut builders);
    facts
        .arkui_builder_flow
        .builders
        .extend(builders.iter().map(|builder| ArkuiBuilderDeclarationFact {
            target_id: builder.target.id.clone(),
        }));
    let mut builder_params = Vec::new();
    collect_arkui_builder_params(root, source, facts, &mut builder_params);
    if !builder_params.is_empty() {
        collect_arkui_builder_param_invocations(root, source, facts, &builder_params);
    }
    let mut emitted = HashSet::new();
    if !builders.is_empty() {
        collect_arkui_builder_registration_calls(root, source, facts, &builders, &mut emitted);
    }
    collect_arkui_builder_param_assignments(root, source, facts, &builders);
}

fn collect_arkui_builders(
    node: Node<'_>,
    source: &[u8],
    facts: &FileFacts,
    output: &mut Vec<ArkuiBuilder>,
) {
    if matches!(node.kind(), "function_declaration" | "method_definition")
        && has_decorator(node, "Builder", source)
    {
        let name = node
            .child_by_field_name("name")
            .map(|name| text(name, source));
        let owner = enclosing_arkui_struct_name(node, source);
        let candidates = facts
            .symbols
            .iter()
            .filter(|symbol| {
                name.as_deref() == Some(symbol.name.as_str())
                    && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                    && owner.as_deref().map_or_else(
                        || symbol.qualified_name == symbol.name,
                        |owner| symbol.qualified_name == format!("{owner}.{}", symbol.name),
                    )
            })
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            let target = candidates[0];
            output.push(ArkuiBuilder {
                target: target.clone(),
                owner: target
                    .qualified_name
                    .rsplit_once('.')
                    .map(|(owner, _)| owner.to_owned()),
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_builders(child, source, facts, output);
    }
}

fn enclosing_arkui_struct_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut ancestor = node.parent();
    while let Some(candidate) = ancestor {
        if candidate.kind() == "struct_declaration" {
            return candidate
                .child_by_field_name("name")
                .map(|name| text(name, source));
        }
        ancestor = candidate.parent();
    }
    None
}

fn collect_arkui_builder_params(
    node: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    output: &mut Vec<ArkuiBuilderParam>,
) {
    if node.kind() == "public_field_definition" && has_decorator(node, "BuilderParam", source) {
        if let (Some(owner), Some(name)) = (
            enclosing_arkui_struct_name(node, source),
            node.child_by_field_name("name")
                .map(|name| text(name, source)),
        ) {
            let ordinal = output.iter().filter(|param| param.owner == owner).count();
            let component = facts.symbols.iter().find(|symbol| {
                symbol.qualified_name == owner
                    && matches!(symbol.kind, SymbolKind::Struct | SymbolKind::Component)
            });
            if let Some(component) = component {
                facts
                    .arkui_builder_flow
                    .params
                    .push(ArkuiBuilderParamDeclarationFact {
                        component_id: component.id.clone(),
                        component_name: owner.clone(),
                        param_name: name.clone(),
                        ordinal,
                    });
            }
            output.push(ArkuiBuilderParam { owner, name });
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_builder_params(child, source, facts, output);
    }
}

fn collect_arkui_builder_param_invocations(
    node: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builder_params: &[ArkuiBuilderParam],
) {
    if node.kind() == "call_expression" {
        if let Some(function) = node.child_by_field_name("function") {
            if let Some(param_name) = exact_this_member_name(function, source) {
                if let Some(component_name) = enclosing_arkui_struct_name(node, source) {
                    if builder_params
                        .iter()
                        .any(|param| param.owner == component_name && param.name == param_name)
                    {
                        let component = facts.symbols.iter().find(|symbol| {
                            symbol.qualified_name == component_name
                                && matches!(symbol.kind, SymbolKind::Struct | SymbolKind::Component)
                        });
                        let owner = owning_callable_symbol(node, &facts.symbols);
                        if let (Some(component), Some(owner)) = (component, owner) {
                            facts.arkui_builder_flow.invocations.push(
                                ArkuiBuilderParamInvocationFact {
                                    component_id: component.id.clone(),
                                    param_name,
                                    owner_id: owner.id.clone(),
                                    line: node.start_position().row + 1,
                                },
                            );
                        }
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_builder_param_invocations(child, source, facts, builder_params);
    }
}

fn collect_arkui_builder_param_assignments(
    node: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
) {
    if matches!(node.kind(), "statement_block" | "arkui_children") {
        collect_recovered_arkui_builder_param_assignments(node, source, facts, builders);
    }
    if node.kind() == "arkui_component_expression" {
        enrich_arkui_builder_param_assignment(node, source, facts, builders);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_builder_param_assignments(child, source, facts, builders);
    }
}

fn enrich_arkui_builder_param_assignment(
    component: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
) {
    let Some(component_name) = component
        .child_by_field_name("function")
        .filter(|function| matches!(function.kind(), "identifier" | "type_identifier"))
        .map(|function| text(function, source))
    else {
        return;
    };
    let caller = match owning_callable_symbol(component, &facts.symbols) {
        Some(caller) => caller.clone(),
        None => return,
    };
    if let Some(arguments) = component.child_by_field_name("arguments") {
        collect_arkui_builder_param_object_assignments(
            arguments,
            &component_name,
            &caller,
            source,
            facts,
            builders,
        );
    }
    let Some(children) = component.child_by_field_name("children") else {
        return;
    };
    let is_project_component = facts.symbols.iter().any(|symbol| {
        symbol.name == component_name
            && matches!(symbol.kind, SymbolKind::Struct | SymbolKind::Component)
    }) || facts.unresolved_references.iter().any(|reference| {
        reference.kind == RelationshipKind::Imports && reference.binding_name == component_name
    });
    if !is_project_component {
        return;
    }
    let child_ordinal = facts
        .arkui_builder_flow
        .assignments
        .iter()
        .filter(|assignment| {
            assignment.caller_id == caller.id
                && assignment.component_binding == component_name
                && assignment.param_name.is_none()
        })
        .count()
        + 1;
    let synthetic_name = format!("<BuilderParam child {component_name}>");
    let qualified_name = format!("{}.{}", caller.qualified_name, synthetic_name);
    let target = Symbol::new_disambiguated(
        Language::ArkTs,
        SymbolKind::Function,
        &synthetic_name,
        &qualified_name,
        &facts.path,
        span(children),
        &format!(
            "arkui-builder-param-child|{}|{}|{}",
            caller.semantic_key, component_name, child_ordinal
        ),
    );
    facts
        .arkui_builder_flow
        .assignments
        .push(ArkuiBuilderParamAssignmentFact {
            caller_id: caller.id,
            component_binding: component_name,
            param_name: None,
            target_id: None,
            target_symbol: Some(target),
            target_binding: None,
            require_decorated_target: false,
            line: component.start_position().row + 1,
        });
}

fn collect_recovered_arkui_builder_param_assignments(
    block: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
) {
    let mut cursor = block.walk();
    let statements = block.named_children(&mut cursor).collect::<Vec<_>>();
    for pair in statements.windows(2) {
        let Some(component) = direct_expression_child(pair[0]) else {
            continue;
        };
        let Some(arguments) = direct_expression_child(pair[1]) else {
            continue;
        };
        if component.kind() != "identifier"
            || arguments.kind() != "parenthesized_expression"
            || component.end_byte() != arguments.start_byte()
        {
            continue;
        }
        let component_name = text(component, source);
        let is_project_component = facts.symbols.iter().any(|symbol| {
            symbol.name == component_name
                && matches!(symbol.kind, SymbolKind::Struct | SymbolKind::Component)
        }) || facts.unresolved_references.iter().any(|reference| {
            reference.kind == RelationshipKind::Imports && reference.binding_name == component_name
        });
        if !is_project_component {
            continue;
        }
        let Some(caller) = owning_callable_symbol(component, &facts.symbols).cloned() else {
            continue;
        };
        collect_arkui_builder_param_object_assignments(
            arguments,
            &component_name,
            &caller,
            source,
            facts,
            builders,
        );
    }
}

fn collect_arkui_builder_param_object_assignments(
    container: Node<'_>,
    component_name: &str,
    caller: &Symbol,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
) {
    let caller_owner = caller
        .qualified_name
        .rsplit_once('.')
        .map(|(owner, _)| owner);
    let mut container_cursor = container.walk();
    for object in container
        .named_children(&mut container_cursor)
        .filter(|argument| argument.kind() == "object")
    {
        let mut pair_cursor = object.walk();
        for pair in object
            .named_children(&mut pair_cursor)
            .filter(|child| child.kind() == "pair")
        {
            let Some(key) = pair.child_by_field_name("key") else {
                continue;
            };
            let Some(value) = pair.child_by_field_name("value") else {
                continue;
            };
            let param_name = text(key, source).trim_matches(['\'', '"']).to_owned();
            let (target_id, target_symbol, target_binding, require_decorated_target) =
                if let Some(builder_name) = exact_this_member_name(value, source) {
                    let candidates = builders
                        .iter()
                        .filter(|builder| {
                            builder.target.name == builder_name
                                && builder.owner.as_deref() == caller_owner
                        })
                        .collect::<Vec<_>>();
                    if candidates.len() != 1 {
                        continue;
                    }
                    (Some(candidates[0].target.id.clone()), None, None, true)
                } else if value.kind() == "identifier" {
                    (None, None, Some(text(value, source)), true)
                } else if matches!(value.kind(), "arrow_function" | "function_expression") {
                    let adapter_ordinal = facts
                        .arkui_builder_flow
                        .assignments
                        .iter()
                        .filter(|assignment| {
                            assignment.caller_id == caller.id
                                && assignment.component_binding == component_name
                                && assignment.param_name.as_deref() == Some(&param_name)
                        })
                        .count()
                        + 1;
                    let adapter_name =
                        format!("<BuilderParam adapter {component_name}.{param_name}>");
                    let qualified_name = format!("{}.{}", caller.qualified_name, adapter_name);
                    let adapter = Symbol::new_disambiguated(
                        Language::ArkTs,
                        SymbolKind::Function,
                        &adapter_name,
                        &qualified_name,
                        &facts.path,
                        span(value),
                        &format!(
                            "arkui-builder-param-adapter|{}|{}|{}|{}",
                            caller.semantic_key, component_name, param_name, adapter_ordinal
                        ),
                    );
                    (None, Some(adapter), None, false)
                } else {
                    continue;
                };
            facts
                .arkui_builder_flow
                .assignments
                .push(ArkuiBuilderParamAssignmentFact {
                    caller_id: caller.id.clone(),
                    component_binding: component_name.to_owned(),
                    param_name: Some(param_name),
                    target_id,
                    target_symbol,
                    target_binding,
                    require_decorated_target,
                    line: pair.start_position().row + 1,
                });
        }
    }
}

fn exact_this_member_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if node.kind() != "member_expression"
        || node
            .child_by_field_name("object")
            .is_none_or(|object| text(object, source) != "this")
    {
        return None;
    }
    node.child_by_field_name("property")
        .filter(|property| matches!(property.kind(), "property_identifier" | "identifier"))
        .map(|property| text(property, source))
}

fn collect_arkui_builder_registration_calls(
    node: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
    emitted: &mut HashSet<(String, String, usize)>,
) {
    if node.kind() == "statement_block" {
        collect_recovered_arkui_builder_registration_calls(node, source, facts, builders, emitted);
    }
    if matches!(
        node.kind(),
        "call_expression" | "arkui_component_expression"
    ) {
        enrich_arkui_builder_registration(node, source, facts, builders, emitted);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_builder_registration_calls(child, source, facts, builders, emitted);
    }
}

fn collect_recovered_arkui_builder_registration_calls(
    block: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
    emitted: &mut HashSet<(String, String, usize)>,
) {
    let mut cursor = block.walk();
    let statements = block.named_children(&mut cursor).collect::<Vec<_>>();
    let mut index = 0;
    while index < statements.len() {
        let Some(base) = direct_expression_child(statements[index]) else {
            index += 1;
            continue;
        };
        if base.kind() != "arkui_component_expression" {
            index += 1;
            continue;
        }

        let mut modifier = index + 1;
        while modifier + 1 < statements.len() {
            let Some(dot) = direct_expression_child(statements[modifier]) else {
                break;
            };
            let Some(arguments) = direct_expression_child(statements[modifier + 1]) else {
                break;
            };
            if dot.kind() != "leading_dot_expression"
                || arguments.kind() != "parenthesized_expression"
                || dot.end_byte() != arguments.start_byte()
            {
                break;
            }
            let method = dot
                .child_by_field_name("expression")
                .map(|expression| text(expression, source));
            if method.as_deref() == Some("bindPopup") {
                enrich_arkui_builder_registration_arguments(
                    dot, arguments, source, facts, builders, emitted,
                );
            }
            modifier += 2;
        }
        index = modifier.max(index + 1);
    }
}

fn direct_expression_child(statement: Node<'_>) -> Option<Node<'_>> {
    if statement.kind() != "expression_statement" {
        return None;
    }
    let mut cursor = statement.walk();
    let mut children = statement.named_children(&mut cursor);
    let child = children.next()?;
    children.next().is_none().then_some(child)
}

fn enrich_arkui_builder_registration(
    call: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
    emitted: &mut HashSet<(String, String, usize)>,
) {
    let method = if call.kind() == "arkui_component_expression" {
        let mut property_cursor = call.walk();
        call.children_by_field_name("property", &mut property_cursor)
            .last()
            .map(|property| text(property, source))
    } else {
        call.child_by_field_name("function")
            .filter(|function| function.kind() == "member_expression")
            .and_then(|function| function.child_by_field_name("property"))
            .map(|property| text(property, source))
    };
    if method.as_deref() != Some("bindPopup") {
        return;
    }
    let arguments = if call.kind() == "arkui_component_expression" {
        let mut cursor = call.walk();
        call.children_by_field_name("arguments", &mut cursor).last()
    } else {
        call.child_by_field_name("arguments")
    };
    let Some(arguments) = arguments else {
        return;
    };
    enrich_arkui_builder_registration_arguments(call, arguments, source, facts, builders, emitted);
}

fn enrich_arkui_builder_registration_arguments(
    call: Node<'_>,
    arguments: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    builders: &[ArkuiBuilder],
    emitted: &mut HashSet<(String, String, usize)>,
) {
    let caller = owning_callable_symbol(call, &facts.symbols)
        .or_else(|| facts.symbols.first())
        .cloned()
        .expect("file symbol");
    let caller_owner = caller
        .qualified_name
        .rsplit_once('.')
        .map(|(owner, _)| owner);
    let mut objects = Vec::new();
    collect_arkui_builder_option_objects(arguments, &mut objects);
    for object in objects {
        let mut pair_cursor = object.walk();
        for pair in object
            .named_children(&mut pair_cursor)
            .filter(|child| child.kind() == "pair")
        {
            let Some(key) = pair.child_by_field_name("key") else {
                continue;
            };
            if text(key, source).trim_matches(['\'', '"']) != "builder" {
                continue;
            }
            let Some(value) = pair.child_by_field_name("value") else {
                continue;
            };
            let Some(name) = exact_this_member_name(value, source) else {
                continue;
            };
            let candidates = builders
                .iter()
                .filter(|builder| {
                    builder.target.name == name && builder.owner.as_deref() == caller_owner
                })
                .collect::<Vec<_>>();
            if candidates.len() != 1 {
                continue;
            }
            let target = &candidates[0].target;
            if !emitted.insert((caller.id.clone(), target.id.clone(), pair.start_byte())) {
                continue;
            }
            facts.relationships.push(Relationship {
                source_id: caller.id.clone(),
                target_id: target.id.clone(),
                kind: RelationshipKind::Calls,
                evidence: Evidence::new(
                    "framework/arkui-builder-registration",
                    0.97,
                    format!(
                        "{} registers decorated ArkUI builder {} with bindPopup",
                        caller.qualified_name, target.qualified_name
                    ),
                    &facts.path,
                    pair.start_position().row + 1,
                ),
            });
        }
    }
}

fn collect_arkui_builder_option_objects<'tree>(node: Node<'tree>, output: &mut Vec<Node<'tree>>) {
    if node.kind() == "object" {
        output.push(node);
        return;
    }
    if !matches!(
        node.kind(),
        "arguments" | "parenthesized_expression" | "sequence_expression"
    ) {
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_builder_option_objects(child, output);
    }
}

fn collect_arkui_style_helper_calls(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let mut helpers = Vec::new();
    collect_arkui_style_helpers(root, source, facts, &mut helpers);
    if helpers.is_empty() {
        return;
    }
    let mut emitted = HashSet::new();
    collect_arkui_style_invocations(root, source, facts, &helpers, &mut emitted);
}

fn collect_arkui_style_helpers(
    node: Node<'_>,
    source: &[u8],
    facts: &FileFacts,
    output: &mut Vec<ArkuiStyleHelper>,
) {
    if matches!(node.kind(), "function_declaration" | "method_definition") {
        let styles = has_decorator(node, "Styles", source);
        let extended_intrinsic = arkui_extend_intrinsic(node, source);
        if styles || extended_intrinsic.is_some() {
            if let Some(target) = facts.symbols.iter().find(|symbol| {
                symbol.start_byte == node.start_byte()
                    && matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
            }) {
                output.push(ArkuiStyleHelper {
                    target: target.clone(),
                    owner: (target.kind == SymbolKind::Method)
                        .then(|| {
                            target
                                .qualified_name
                                .rsplit_once('.')
                                .map_or(String::new(), |(owner, _)| owner.to_owned())
                        })
                        .filter(|owner| !owner.is_empty()),
                    extended_intrinsic,
                });
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_style_helpers(child, source, facts, output);
    }
}

fn arkui_extend_intrinsic(node: Node<'_>, source: &[u8]) -> Option<String> {
    decorator_nodes(node)
        .filter_map(|decorator| decorator_call(decorator, source))
        .find_map(|(name, arguments)| {
            (name == "Extend" && arguments.len() == 1)
                .then(|| text(arguments[0], source))
                .filter(|intrinsic| is_javascript_identifier(intrinsic))
        })
}

fn decorator_nodes(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut decorators = direct_decorators(node).collect::<Vec<_>>();
    let mut sibling = node.prev_named_sibling();
    while let Some(candidate) = sibling {
        if candidate.kind() != "decorator" {
            break;
        }
        decorators.push(candidate);
        sibling = candidate.prev_named_sibling();
    }
    decorators.into_iter()
}

fn collect_arkui_style_invocations(
    node: Node<'_>,
    source: &[u8],
    facts: &mut FileFacts,
    helpers: &[ArkuiStyleHelper],
    emitted: &mut HashSet<(String, String, usize)>,
) {
    if node.kind() == "call_expression" {
        if let Some(function) = node.child_by_field_name("function") {
            match function.kind() {
                "member_expression" => {
                    let helper_name = function
                        .child_by_field_name("property")
                        .map(|property| text(property, source));
                    let chain_root = function
                        .child_by_field_name("object")
                        .and_then(|object| arkui_chain_root(object, source));
                    if let Some(helper_name) = helper_name {
                        append_arkui_style_helper_edge(
                            node,
                            &helper_name,
                            chain_root.as_deref(),
                            facts,
                            helpers,
                            emitted,
                        );
                    }
                }
                "identifier" => append_arkui_style_helper_edge(
                    node,
                    &text(function, source),
                    None,
                    facts,
                    helpers,
                    emitted,
                ),
                _ => {}
            }
        }
    } else if node.kind() == "arkui_component_expression" {
        let chain_root = node
            .child_by_field_name("function")
            .and_then(|function| call_name_text(function, source));
        let mut cursor = node.walk();
        for property in node.children_by_field_name("property", &mut cursor) {
            append_arkui_style_helper_edge(
                property,
                &text(property, source),
                chain_root.as_deref(),
                facts,
                helpers,
                emitted,
            );
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_style_invocations(child, source, facts, helpers, emitted);
    }
}

fn append_arkui_style_helper_edge(
    observation: Node<'_>,
    helper_name: &str,
    chain_root: Option<&str>,
    facts: &mut FileFacts,
    helpers: &[ArkuiStyleHelper],
    emitted: &mut HashSet<(String, String, usize)>,
) {
    let Some(caller) = owning_callable_symbol(observation, &facts.symbols).cloned() else {
        return;
    };
    let caller_owner = caller
        .qualified_name
        .rsplit_once('.')
        .map(|(owner, _)| owner);
    let mut candidates = helpers
        .iter()
        .filter(|helper| helper.target.name == helper_name)
        .filter(|helper| {
            helper
                .owner
                .as_deref()
                .is_none_or(|owner| Some(owner) == caller_owner)
                && helper
                    .extended_intrinsic
                    .as_deref()
                    .is_none_or(|intrinsic| Some(intrinsic) == chain_root)
        })
        .collect::<Vec<_>>();
    if candidates.iter().any(|helper| helper.owner.is_some()) {
        candidates.retain(|helper| helper.owner.is_some());
    }
    if candidates.len() != 1 {
        return;
    }
    let target = candidates[0].target.clone();
    let key = (
        caller.id.clone(),
        target.id.clone(),
        observation.start_position().row + 1,
    );
    if !emitted.insert(key) {
        return;
    }
    facts.relationships.push(Relationship {
        source_id: caller.id.clone(),
        target_id: target.id.clone(),
        kind: RelationshipKind::Calls,
        evidence: Evidence::new(
            "framework/arkui-helper",
            0.97,
            format!(
                "{} applies decorated ArkUI style helper {}",
                caller.qualified_name, target.qualified_name
            ),
            &facts.path,
            observation.start_position().row + 1,
        ),
    });
}

fn arkui_chain_root(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "arkui_component_expression" => node
            .child_by_field_name("function")
            .and_then(|function| call_name_text(function, source)),
        "call_expression" => {
            node.child_by_field_name("function")
                .and_then(|function| match function.kind() {
                    "identifier" => Some(text(function, source)),
                    "member_expression" => function
                        .child_by_field_name("object")
                        .and_then(|object| arkui_chain_root(object, source)),
                    _ => None,
                })
        }
        "member_expression" => node
            .child_by_field_name("object")
            .and_then(|object| arkui_chain_root(object, source)),
        "identifier" => Some(text(node, source)),
        _ => None,
    }
}

fn collect_arkui_semantics(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "struct_declaration" {
        if has_decorator(node, "Entry", source) {
            mark_arkui_entry_component(node, source, facts);
        }
        if ["Component", "ComponentV2"]
            .iter()
            .any(|decorator| has_decorator(node, decorator, source))
        {
            enrich_arkui_component(node, source, facts);
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_semantics(child, source, facts);
    }
}

fn mark_arkui_entry_component(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let Some(name) = node
        .child_by_field_name("name")
        .map(|name| text(name, source))
    else {
        return;
    };
    let Some(symbol) = facts.symbols.iter().find(|symbol| {
        symbol.kind == SymbolKind::Struct
            && symbol.name == name
            && symbol.start_byte == node.start_byte()
    }) else {
        return;
    };
    facts.unresolved_references.push(UnresolvedReference {
        source_id: symbol.id.clone(),
        target_name: "__arkui_entry__".to_owned(),
        binding_name: "__arkui_entry__".to_owned(),
        target_file_hint: Some(facts.path.clone()),
        kind: RelationshipKind::References,
        provenance: "framework/arkui-entry".to_owned(),
        confidence: 1.0,
        explanation: format!(
            "{} is an ArkUI @Entry page component",
            symbol.qualified_name
        ),
        file: facts.path.clone(),
        line: node.start_position().row + 1,
    });
}

fn enrich_arkui_component(component: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    const REACTIVE_DECORATORS: &[&str] = &[
        "State",
        "Prop",
        "Link",
        "StorageLink",
        "Local",
        "Param",
        "Provide",
        "Consume",
        "Provider",
        "Consumer",
        "ObjectLink",
    ];
    let Some(component_name) = component
        .child_by_field_name("name")
        .map(|name| text(name, source))
        .filter(|name| !name.is_empty())
    else {
        return;
    };
    let Some(body) = component.child_by_field_name("body") else {
        return;
    };
    let children = direct_named_children(body);
    let mut reactive_fields = Vec::new();
    let mut methods = Vec::new();
    let mut pending_decorators = Vec::new();
    for child in children {
        if child.kind() == "decorator" {
            if let Some(name) = decorator_identifier(child, source) {
                pending_decorators.push(name);
            }
            continue;
        }
        let decorators = direct_decorators(child)
            .filter_map(|decorator| decorator_identifier(decorator, source))
            .chain(pending_decorators.drain(..))
            .collect::<Vec<_>>();
        match child.kind() {
            "public_field_definition"
                if decorators
                    .iter()
                    .any(|name| REACTIVE_DECORATORS.contains(&name.as_str())) =>
            {
                if let Some(name) = child.child_by_field_name("name") {
                    reactive_fields.push(text(name, source));
                }
            }
            "method_definition" => methods.push(child),
            _ => {}
        }
    }
    let build = methods.iter().copied().find(|method| {
        method
            .child_by_field_name("name")
            .is_some_and(|name| text(name, source) == "build")
    });
    for method in &methods {
        collect_arkui_runtime_calls(*method, &component_name, source, facts);
    }
    let Some(build) = build else {
        return;
    };
    for method in methods {
        if method.id() == build.id() || !mutates_arkui_state(method, &reactive_fields, source) {
            continue;
        }
        let Some(owner) = owning_callable_symbol(method, &facts.symbols) else {
            continue;
        };
        facts.unresolved_calls.push(UnresolvedCall {
            caller_id: owner.id.clone(),
            fallback_caller_id: None,
            callee_name: "build".to_owned(),
            receiver_binding: None,
            receiver_type: Some(component_name.clone()),
            receiver_call_start_byte: None,
            target_file_hint: Some(facts.path.clone()),
            provenance: "framework/arkui-state".to_owned(),
            confidence: 0.95,
            explanation: format!(
                "{} mutates reactive ArkUI state and schedules {component_name}.build",
                owner.qualified_name
            ),
            resolvable: true,
            file: facts.path.clone(),
            line: method.start_position().row + 1,
            start_byte: method.start_byte(),
        });
    }
}

fn collect_arkui_runtime_calls(
    node: Node<'_>,
    component_name: &str,
    source: &[u8],
    facts: &mut FileFacts,
) {
    if node.kind() == "arkui_component_expression" {
        if let Some(target) = node
            .child_by_field_name("function")
            .and_then(|function| call_name_text(function, source))
            .filter(|target| target.starts_with(char::is_uppercase))
            .filter(|target| arkui_target_is_project_defined(target, facts))
        {
            append_arkui_call(
                node,
                target,
                component_name,
                "framework/arkui-render",
                0.97,
                facts,
            );
        }
        let mut property_cursor = node.walk();
        let properties = node
            .children_by_field_name("property", &mut property_cursor)
            .collect::<Vec<_>>();
        let mut argument_cursor = node.walk();
        let argument_lists = node
            .children_by_field_name("arguments", &mut argument_cursor)
            .collect::<Vec<_>>();
        for (property, arguments) in properties
            .into_iter()
            .zip(argument_lists.into_iter().skip(1))
        {
            if !is_arkui_event_property(&text(property, source)) {
                continue;
            }
            let mut cursor = arguments.walk();
            let handler = arguments
                .named_children(&mut cursor)
                .next()
                .and_then(|argument| {
                    arkui_component_handler_name(argument, component_name, source, facts)
                });
            if let Some(handler) = handler {
                append_arkui_call(
                    property,
                    handler,
                    component_name,
                    "framework/arkui-event",
                    0.97,
                    facts,
                );
            }
        }
    } else if node.kind() == "call_expression" {
        if let Some(function) = node.child_by_field_name("function") {
            if function.kind() == "member_expression" {
                let method = function
                    .child_by_field_name("property")
                    .map(|property| text(property, source))
                    .unwrap_or_default();
                if is_arkui_event_property(&method) {
                    if let Some(arguments) = node.child_by_field_name("arguments") {
                        let mut cursor = arguments.walk();
                        let handler =
                            arguments
                                .named_children(&mut cursor)
                                .next()
                                .and_then(|argument| {
                                    arkui_component_handler_name(
                                        argument,
                                        component_name,
                                        source,
                                        facts,
                                    )
                                });
                        if let Some(handler) = handler {
                            append_arkui_call(
                                node,
                                handler,
                                component_name,
                                "framework/arkui-event",
                                0.97,
                                facts,
                            );
                        }
                    }
                }
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_runtime_calls(child, component_name, source, facts);
    }
}

fn collect_arkui_routes(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let bindings = arkui_router_bindings(root, source);
    if bindings.is_empty() {
        return;
    }
    collect_arkui_route_calls(root, source, &bindings, facts);
}

fn collect_arkui_route_calls(
    node: Node<'_>,
    source: &[u8],
    bindings: &HashSet<String>,
    facts: &mut FileFacts,
) {
    if node.kind() == "call_expression" {
        collect_arkui_route_call(node, source, bindings, facts);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_route_calls(child, source, bindings, facts);
    }
}

fn collect_arkui_route_call(
    node: Node<'_>,
    source: &[u8],
    bindings: &HashSet<String>,
    facts: &mut FileFacts,
) {
    let Some(function) = node.child_by_field_name("function") else {
        return;
    };
    if function.kind() != "member_expression" {
        return;
    }
    let Some(receiver) = function
        .child_by_field_name("object")
        .filter(|receiver| receiver.kind() == "identifier")
        .map(|receiver| text(receiver, source))
        .filter(|receiver| bindings.contains(receiver))
    else {
        return;
    };
    if arkui_router_binding_is_shadowed(node, &receiver, source)
        || function
            .child_by_field_name("property")
            .is_none_or(|property| {
                !matches!(text(property, source).as_str(), "pushUrl" | "replaceUrl")
            })
    {
        return;
    }
    let Some(arguments) = node.child_by_field_name("arguments") else {
        return;
    };
    let mut arguments_cursor = arguments.walk();
    let Some(configuration) = arguments.named_children(&mut arguments_cursor).next() else {
        return;
    };
    if configuration.kind() != "object" {
        return;
    }
    let mut object_cursor = configuration.walk();
    let url = configuration
        .named_children(&mut object_cursor)
        .filter(|property| property.kind() == "pair")
        .find_map(|property| {
            let key = property.child_by_field_name("key")?;
            (text(key, source).trim_matches(['\'', '"']) == "url")
                .then(|| property.child_by_field_name("value"))
                .flatten()
                .and_then(|value| string_literal(value, source))
        });
    let Some(url) = url else {
        return;
    };
    let Some(target_file_hint) = arkui_route_target_hint(&facts.path, &url) else {
        return;
    };
    let Some(owner) = owning_callable_symbol(node, &facts.symbols) else {
        return;
    };
    facts.unresolved_calls.push(UnresolvedCall {
        caller_id: owner.id.clone(),
        fallback_caller_id: None,
        callee_name: url
            .trim_end_matches(".ets")
            .rsplit('/')
            .next()
            .unwrap_or(&url)
            .to_owned(),
        receiver_binding: None,
        receiver_type: None,
        receiver_call_start_byte: None,
        target_file_hint: Some(target_file_hint),
        provenance: "framework/arkui-route".to_owned(),
        confidence: 0.97,
        explanation: format!(
            "{} navigates to literal ArkUI page {url}",
            owner.qualified_name
        ),
        resolvable: true,
        file: facts.path.clone(),
        line: node.start_position().row + 1,
        start_byte: node.start_byte(),
    });
}

fn arkui_router_bindings(node: Node<'_>, source: &[u8]) -> HashSet<String> {
    let mut bindings = HashSet::new();
    collect_arkui_router_bindings(node, source, &mut bindings);
    bindings
}

fn collect_arkui_router_bindings(node: Node<'_>, source: &[u8], bindings: &mut HashSet<String>) {
    if node.kind() == "import_statement" {
        let module = node
            .child_by_field_name("source")
            .and_then(|source_node| string_literal(source_node, source));
        if matches!(module.as_deref(), Some("@ohos.router" | "@kit.ArkUI")) {
            let statement = text(node, source);
            let clause = statement
                .strip_prefix("import")
                .and_then(|body| body.rsplit_once(" from ").map(|(clause, _)| clause.trim()));
            if let Some(clause) = clause.filter(|clause| !clause.starts_with("type ")) {
                if module.as_deref() == Some("@ohos.router") {
                    if let Some(binding) = arkui_default_or_namespace_binding(clause) {
                        bindings.insert(binding);
                    }
                } else {
                    bindings.extend(arkui_named_router_bindings(clause));
                }
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_router_bindings(child, source, bindings);
    }
}

fn arkui_default_or_namespace_binding(clause: &str) -> Option<String> {
    let candidate = clause
        .strip_prefix("* as ")
        .unwrap_or(clause)
        .split([',', ' ', '\t', '\r', '\n'])
        .find(|part| !part.is_empty())?;
    is_javascript_identifier(candidate).then(|| candidate.to_owned())
}

fn arkui_named_router_bindings(clause: &str) -> Vec<String> {
    named_import_bindings(clause, "router")
}

fn is_javascript_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first == '$' || first.is_ascii_alphabetic())
        && characters.all(|character| {
            character == '_' || character == '$' || character.is_ascii_alphanumeric()
        })
}

fn arkui_router_binding_is_shadowed(call: Node<'_>, binding: &str, source: &[u8]) -> bool {
    let mut root = call;
    while let Some(parent) = root.parent() {
        root = parent;
    }
    if arkui_scope_declares_binding(root, call, binding, source) {
        return true;
    }
    let mut ancestor = call.parent();
    while let Some(scope) = ancestor {
        if let Some(parameters) = scope.child_by_field_name("parameters") {
            let mut identifiers = Vec::new();
            collect_identifier_texts(parameters, source, &mut identifiers);
            if identifiers.iter().any(|identifier| identifier == binding) {
                return true;
            }
        }
        ancestor = scope.parent();
    }
    false
}

fn arkui_scope_declares_binding(
    node: Node<'_>,
    call: Node<'_>,
    binding: &str,
    source: &[u8],
) -> bool {
    if matches!(
        node.kind(),
        "variable_declarator" | "function_declaration" | "class_declaration"
    ) && node
        .child_by_field_name("name")
        .is_some_and(|name| text(name, source) == binding)
    {
        return true;
    }
    let is_nested_callable = matches!(
        node.kind(),
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
    );
    if is_nested_callable
        && !(node.start_byte() <= call.start_byte() && node.end_byte() >= call.end_byte())
    {
        return false;
    }
    let mut cursor = node.walk();
    let declared = node
        .named_children(&mut cursor)
        .any(|child| arkui_scope_declares_binding(child, call, binding, source));
    declared
}

fn collect_identifier_texts(node: Node<'_>, source: &[u8], output: &mut Vec<String>) {
    if node.kind() == "identifier" {
        output.push(text(node, source));
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_identifier_texts(child, source, output);
    }
}

fn arkui_route_target_hint(_source_file: &str, url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.contains(['?', '#', '\\']) {
        return None;
    }
    let route = trimmed
        .strip_prefix('/')
        .unwrap_or(trimmed)
        .trim_end_matches(".ets");
    let mut normalized = Vec::new();
    for component in Path::new(route).components() {
        match component {
            std::path::Component::Normal(segment) => {
                let segment = segment.to_str()?;
                if segment.is_empty() {
                    return None;
                }
                normalized.push(segment);
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    (!normalized.is_empty()).then(|| format!("arkui-route:{}", normalized.join("/")))
}

fn arkui_target_is_project_defined(target: &str, facts: &FileFacts) -> bool {
    facts.symbols.iter().any(|symbol| {
        matches!(symbol.kind, SymbolKind::Struct | SymbolKind::Component) && symbol.name == target
    }) || facts.unresolved_references.iter().any(|reference| {
        reference.kind == RelationshipKind::Imports && reference.binding_name == target
    })
}

fn arkui_component_handler_name(
    argument: Node<'_>,
    component_name: &str,
    source: &[u8],
    facts: &FileFacts,
) -> Option<String> {
    if argument.kind() != "member_expression"
        || argument
            .child_by_field_name("object")
            .is_none_or(|object| text(object, source) != "this")
    {
        return None;
    }
    let name = argument
        .child_by_field_name("property")
        .map(|property| text(property, source))
        .filter(|name| !name.is_empty())?;
    facts
        .symbols
        .iter()
        .any(|symbol| {
            matches!(symbol.kind, SymbolKind::Method | SymbolKind::Variable)
                && symbol.qualified_name == format!("{component_name}.{name}")
        })
        .then_some(name)
}

fn is_arkui_event_property(name: &str) -> bool {
    name.strip_prefix("on")
        .and_then(|suffix| suffix.chars().next())
        .is_some_and(char::is_uppercase)
}

const ARKUI_INTRINSICS: &[&str] = &[
    "$r",
    "$rawfile",
    "AlphabetIndexer",
    "Badge",
    "Blank",
    "Button",
    "CalendarPicker",
    "Canvas",
    "Checkbox",
    "CheckboxGroup",
    "Circle",
    "Column",
    "ColumnSplit",
    "Counter",
    "DataPanel",
    "DatePicker",
    "Divider",
    "Ellipse",
    "Flex",
    "FlowItem",
    "FolderStack",
    "FormLink",
    "Gauge",
    "Grid",
    "GridItem",
    "GridRow",
    "GridCol",
    "Hyperlink",
    "Image",
    "ImageAnimator",
    "Line",
    "List",
    "ListItem",
    "ListItemGroup",
    "LoadingProgress",
    "Marquee",
    "Menu",
    "MenuItem",
    "MenuItemGroup",
    "NavDestination",
    "Navigation",
    "Navigator",
    "NodeContainer",
    "Panel",
    "Path",
    "PatternLock",
    "PluginComponent",
    "Polygon",
    "Polyline",
    "Progress",
    "QRCode",
    "Radio",
    "Rating",
    "Rect",
    "RelativeContainer",
    "RichEditor",
    "RootScene",
    "Row",
    "RowSplit",
    "Scroll",
    "Search",
    "Select",
    "Shape",
    "SideBarContainer",
    "Slider",
    "Span",
    "Stack",
    "Stepper",
    "StepperItem",
    "Swiper",
    "SymbolGlyph",
    "TabContent",
    "Tabs",
    "Text",
    "TextArea",
    "TextClock",
    "TextInput",
    "TextPicker",
    "TextTimer",
    "TimePicker",
    "Toggle",
    "Video",
    "Web",
    "XComponent",
];

pub(crate) fn is_arkui_intrinsic_name(name: &str) -> bool {
    ARKUI_INTRINSICS.contains(&name)
}

fn remove_arkui_intrinsic_calls(facts: &mut FileFacts) {
    let project_names = facts
        .symbols
        .iter()
        .map(|symbol| symbol.name.as_str())
        .chain(
            facts
                .unresolved_references
                .iter()
                .filter(|reference| reference.kind == RelationshipKind::Imports)
                .map(|reference| reference.binding_name.as_str()),
        )
        .collect::<HashSet<_>>();
    facts.unresolved_calls.retain(|call| {
        call.provenance != "tree-sitter/name-resolution"
            || !is_arkui_intrinsic_name(&call.callee_name)
            || project_names.contains(call.callee_name.as_str())
    });
}

fn append_arkui_call(
    observation: Node<'_>,
    target: String,
    component_name: &str,
    provenance: &str,
    confidence: f64,
    facts: &mut FileFacts,
) {
    let Some(owner) = owning_callable_symbol(observation, &facts.symbols) else {
        return;
    };
    if facts.unresolved_calls.iter().any(|pending| {
        pending.caller_id == owner.id
            && pending.callee_name == target
            && pending.provenance == provenance
    }) {
        return;
    }
    facts.unresolved_calls.push(UnresolvedCall {
        caller_id: owner.id.clone(),
        fallback_caller_id: None,
        callee_name: target.clone(),
        receiver_binding: None,
        receiver_type: (provenance == "framework/arkui-event").then(|| component_name.to_owned()),
        receiver_call_start_byte: None,
        target_file_hint: (provenance == "framework/arkui-event").then(|| facts.path.clone()),
        provenance: provenance.to_owned(),
        confidence,
        explanation: format!("{component_name} ArkUI flow invokes {target}"),
        resolvable: true,
        file: facts.path.clone(),
        line: observation.start_position().row + 1,
        start_byte: observation.start_byte(),
    });
}

fn mutates_arkui_state(node: Node<'_>, fields: &[String], source: &[u8]) -> bool {
    if fields.is_empty() {
        return false;
    }
    if matches!(
        node.kind(),
        "assignment_expression" | "augmented_assignment_expression" | "update_expression"
    ) {
        let changed = node
            .child_by_field_name("left")
            .or_else(|| node.child_by_field_name("argument"))
            .or_else(|| node.named_child(0))
            .map(|target| text(target, source));
        if changed
            .is_some_and(|target| fields.iter().any(|field| target == format!("this.{field}")))
        {
            return true;
        }
    }
    if node.kind() == "call_expression" {
        const MUTATORS: &[&str] = &[
            "push", "pop", "shift", "unshift", "splice", "sort", "reverse", "fill",
        ];
        if let Some(function) = node.child_by_field_name("function") {
            if function.kind() == "member_expression" {
                let receiver = function
                    .child_by_field_name("object")
                    .map(|object| text(object, source))
                    .unwrap_or_default();
                let method = function
                    .child_by_field_name("property")
                    .map(|property| text(property, source))
                    .unwrap_or_default();
                if MUTATORS.contains(&method.as_str())
                    && fields
                        .iter()
                        .any(|field| receiver == format!("this.{field}"))
                {
                    return true;
                }
            }
        }
    }
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).any(|child| {
        !matches!(
            child.kind(),
            "arrow_function"
                | "function_expression"
                | "function_declaration"
                | "generator_function"
                | "generator_function_declaration"
                | "method_definition"
        ) && mutates_arkui_state(child, fields, source)
    });
    found
}

fn has_decorator(node: Node<'_>, expected: &str, source: &[u8]) -> bool {
    if has_attached_or_adjacent_decorator(node, expected, source) {
        return true;
    }
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "export_statement" | "export_default_declaration"
        ) && has_attached_or_adjacent_decorator(parent, expected, source)
    })
}

fn has_attached_or_adjacent_decorator(node: Node<'_>, expected: &str, source: &[u8]) -> bool {
    if direct_decorators(node)
        .filter_map(|decorator| decorator_identifier(decorator, source))
        .any(|name| name == expected)
    {
        return true;
    }
    let mut sibling = node.prev_named_sibling();
    while let Some(candidate) = sibling {
        if candidate.kind() != "decorator" {
            break;
        }
        if decorator_identifier(candidate, source).as_deref() == Some(expected) {
            return true;
        }
        sibling = candidate.prev_named_sibling();
    }
    false
}

fn decorator_identifier(decorator: Node<'_>, source: &[u8]) -> Option<String> {
    let mut cursor = decorator.walk();
    let name = decorator
        .named_children(&mut cursor)
        .find_map(|child| match child.kind() {
            "identifier" => Some(text(child, source)),
            "call_expression" => child
                .child_by_field_name("function")
                .map(|function| text(function, source)),
            _ => None,
        });
    name
}

fn call_name_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    matches!(node.kind(), "identifier" | "property_identifier").then(|| text(node, source))
}

fn direct_named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn collect_component_template_edges(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    const TEMPLATE_CHILD_CAP: usize = 32;
    let component_name = Path::new(&facts.path)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("Component");
    let component = Symbol::new(
        facts.language,
        SymbolKind::Component,
        component_name,
        component_name,
        &facts.path,
        span(root),
    );
    let file_symbol = facts.symbols.first().expect("file symbol");
    facts.relationships.push(Relationship {
        source_id: file_symbol.id.clone(),
        target_id: component.id.clone(),
        kind: RelationshipKind::Contains,
        evidence: Evidence::new(
            "framework/component-file",
            1.0,
            format!("{} defines component {component_name}", facts.path),
            &facts.path,
            1,
        ),
    });

    let template = template_only_source(source);
    let provenance = match facts.language {
        Language::Vue => "framework/vue-template",
        Language::Svelte => "framework/svelte-template",
        _ => unreachable!("component template language"),
    };
    let mut targets = template_event_handlers(&template, facts.language);
    targets.extend(template_child_components(&template));
    targets.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    targets.dedup_by(|left, right| left.0 == right.0);
    for (target, offset) in targets.into_iter().take(TEMPLATE_CHILD_CAP) {
        if target == component_name {
            continue;
        }
        let target_file_hint = facts
            .unresolved_references
            .iter()
            .find(|reference| {
                reference.kind == RelationshipKind::Imports && reference.binding_name == target
            })
            .and_then(|reference| reference.target_file_hint.clone())
            .or_else(|| Some(facts.path.clone()));
        facts.unresolved_calls.push(UnresolvedCall {
            caller_id: component.id.clone(),
            fallback_caller_id: None,
            callee_name: target.clone(),
            receiver_binding: None,
            receiver_type: None,
            receiver_call_start_byte: None,
            target_file_hint,
            provenance: provenance.to_owned(),
            confidence: 0.96,
            explanation: format!("{component_name} template invokes or renders {target}"),
            resolvable: true,
            file: facts.path.clone(),
            line: byte_line(source, offset),
            start_byte: offset,
        });
    }
    facts.symbols.push(component);
}

fn template_only_source(source: &[u8]) -> String {
    let source = String::from_utf8_lossy(source);
    let lower = source.to_ascii_lowercase();
    let mut template = source.as_bytes().to_vec();
    for tag in ["script", "style"] {
        let mut offset = 0;
        let opening = format!("<{tag}");
        let closing = format!("</{tag}");
        while let Some(relative_open) = lower[offset..].find(&opening) {
            let open = offset + relative_open;
            let Some(relative_body) = lower[open..].find('>') else {
                break;
            };
            let body = open + relative_body + 1;
            let Some(relative_close) = lower[body..].find(&closing) else {
                break;
            };
            let close = body + relative_close;
            for byte in &mut template[open..close] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
            offset = close + closing.len();
        }
    }
    String::from_utf8(template).expect("template masking preserves UTF-8")
}

fn template_event_handlers(template: &str, language: Language) -> Vec<(String, usize)> {
    let bytes = template.as_bytes();
    let mut handlers = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let marker = match language {
            Language::Svelte
                if bytes.get(index) == Some(&b'=') && bytes.get(index + 1) == Some(&b'{') =>
            {
                Some((index + 2, b'}'))
            }
            Language::Vue
                if bytes.get(index) == Some(&b'=')
                    && matches!(bytes.get(index + 1), Some(b'"' | b'\'')) =>
            {
                Some((index + 2, bytes[index + 1]))
            }
            _ => None,
        };
        let Some((value_start, terminator)) = marker else {
            index += 1;
            continue;
        };
        let attribute_start = template[..index]
            .rfind(|character: char| character.is_ascii_whitespace() || character == '<')
            .map_or(0, |position| position + 1);
        let attribute = &template[attribute_start..index];
        let is_event = match language {
            Language::Svelte => attribute.starts_with("on:") || attribute.starts_with("on"),
            Language::Vue => attribute.starts_with('@') || attribute.starts_with("v-on:"),
            _ => false,
        };
        let Some(relative_end) = bytes[value_start..]
            .iter()
            .position(|byte| *byte == terminator)
        else {
            break;
        };
        let value_end = value_start + relative_end;
        let value = template[value_start..value_end].trim();
        if is_event && is_identifier(value) {
            handlers.push((value.to_owned(), value_start));
        }
        index = value_end + 1;
    }
    handlers
}

fn template_child_components(template: &str) -> Vec<(String, usize)> {
    let bytes = template.as_bytes();
    let mut components = Vec::new();
    let mut index = 0;
    while index < template.len() {
        let Some(relative) = template[index..].find('<') else {
            break;
        };
        let start = index + relative;
        let name_start = start + 1;
        if matches!(bytes.get(name_start), Some(b'/' | b'!' | b'?' | b':')) {
            index = name_start + 1;
            continue;
        }
        let mut end = name_start;
        while bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.'))
        {
            end += 1;
        }
        let name = &template[name_start..end];
        if name.starts_with(char::is_uppercase) && is_identifier(name) {
            components.push((name.to_owned(), name_start));
        }
        index = end.max(name_start + 1).min(template.len());
    }
    components
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character == '$' || character.is_alphabetic())
        && characters
            .all(|character| character == '_' || character == '$' || character.is_alphanumeric())
}

fn byte_line(source: &[u8], offset: usize) -> usize {
    source[..offset.min(source.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

fn collect_nestjs_routes(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "class_declaration" {
        enrich_nestjs_controller(node, source, facts);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_nestjs_routes(child, source, facts);
    }
}

fn enrich_nestjs_controller(class: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    const HTTP_DECORATORS: &[&str] = &[
        "Get", "Post", "Put", "Patch", "Delete", "Head", "Options", "All",
    ];
    let class_decorators = direct_decorators(class);
    let parent_decorators = class.parent().into_iter().flat_map(direct_decorators);
    let Some(prefixes) = class_decorators
        .chain(parent_decorators)
        .find_map(|decorator| {
            let (name, arguments) = decorator_call(decorator, source)?;
            (name == "Controller").then(|| nestjs_controller_paths(&arguments, source))
        })
    else {
        return;
    };
    let Some(prefixes) = prefixes else {
        return;
    };

    let Some(body) = class.child_by_field_name("body") else {
        return;
    };
    let mut cursor = body.walk();
    let children = body.named_children(&mut cursor).collect::<Vec<_>>();
    let mut pending_decorators = Vec::new();
    for method in children {
        if method.kind() == "decorator" {
            pending_decorators.push(method);
            continue;
        }
        if method.kind() != "method_definition" {
            pending_decorators.clear();
            continue;
        }
        let Some(handler) = method
            .child_by_field_name("name")
            .map(|name| text(name, source))
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        for decorator in pending_decorators.drain(..) {
            let Some((decorator_name, arguments)) = decorator_call(decorator, source) else {
                continue;
            };
            if !HTTP_DECORATORS.contains(&decorator_name.as_str()) {
                continue;
            }
            let method_paths = if let Some(argument) = arguments.first() {
                let Some(paths) = literal_string_values(*argument, source) else {
                    continue;
                };
                paths
            } else {
                vec![String::new()]
            };
            for prefix in &prefixes {
                for method_path in &method_paths {
                    let path = join_route_path(prefix, method_path);
                    let verb = decorator_name.to_ascii_uppercase();
                    let name = format!("{verb} {path}");
                    let occurrence = facts
                        .symbols
                        .iter()
                        .filter(|symbol| symbol.kind == SymbolKind::Route && symbol.name == name)
                        .count()
                        + 1;
                    let route = Symbol::new_disambiguated(
                        facts.language,
                        SymbolKind::Route,
                        &name,
                        &name,
                        &facts.path,
                        span(decorator),
                        &format!("nestjs|{verb}|{path}|{handler}|occurrence:{occurrence}"),
                    );
                    let file_symbol = facts.symbols.first().expect("file symbol");
                    facts.relationships.push(Relationship {
                        source_id: file_symbol.id.clone(),
                        target_id: route.id.clone(),
                        kind: RelationshipKind::Contains,
                        evidence: Evidence::new(
                            "framework/nestjs-route",
                            1.0,
                            format!("{name} is registered in {}", facts.path),
                            &facts.path,
                            decorator.start_position().row + 1,
                        ),
                    });
                    facts.unresolved_calls.push(UnresolvedCall {
                        caller_id: route.id.clone(),
                        fallback_caller_id: None,
                        callee_name: handler.clone(),
                        receiver_binding: None,
                        receiver_type: None,
                        receiver_call_start_byte: None,
                        target_file_hint: None,
                        provenance: "framework/nestjs-route".to_owned(),
                        confidence: 0.99,
                        explanation: format!("NestJS route {name} decorates handler {handler}"),
                        resolvable: true,
                        file: facts.path.clone(),
                        line: decorator.start_position().row + 1,
                        start_byte: decorator.start_byte(),
                    });
                    facts.symbols.push(route);
                }
            }
        }
    }
}

fn nestjs_controller_paths(arguments: &[Node<'_>], source: &[u8]) -> Option<Vec<String>> {
    let Some(argument) = arguments.first().copied() else {
        return Some(vec![String::new()]);
    };
    if argument.kind() != "object" {
        return literal_string_values(argument, source);
    }
    let mut cursor = argument.walk();
    for pair in argument.named_children(&mut cursor) {
        if pair.kind() != "pair" {
            continue;
        }
        let key = pair.child_by_field_name("key")?;
        if text(key, source).trim_matches(['\'', '"']) != "path" {
            continue;
        }
        return literal_string_values(pair.child_by_field_name("value")?, source);
    }
    Some(vec![String::new()])
}

fn literal_string_values(node: Node<'_>, source: &[u8]) -> Option<Vec<String>> {
    if let Some(value) = string_literal(node, source) {
        return Some(vec![value]);
    }
    if node.kind() != "array" {
        return None;
    }
    let mut cursor = node.walk();
    let values = node
        .named_children(&mut cursor)
        .map(|element| string_literal(element, source))
        .collect::<Option<Vec<_>>>()?;
    (!values.is_empty()).then_some(values)
}

fn direct_decorators(node: Node<'_>) -> impl Iterator<Item = Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == "decorator")
        .collect::<Vec<_>>()
        .into_iter()
}

fn decorator_call<'tree>(
    decorator: Node<'tree>,
    source: &[u8],
) -> Option<(String, Vec<Node<'tree>>)> {
    let call = first_descendant_of_kind(decorator, "call_expression")?;
    let function = call.child_by_field_name("function")?;
    if function.kind() != "identifier" {
        return None;
    }
    let name = text(function, source);
    let arguments = call.child_by_field_name("arguments")?;
    let mut cursor = arguments.walk();
    let arguments = arguments.named_children(&mut cursor).collect::<Vec<_>>();
    Some((name, arguments))
}

fn join_route_path(prefix: &str, path: &str) -> String {
    let joined = [prefix.trim_matches('/'), path.trim_matches('/')]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if joined.is_empty() {
        "/".to_owned()
    } else {
        format!("/{joined}")
    }
}

fn collect_javascript_function_references(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    match node.kind() {
        "variable_declarator" => {
            if let Some(value) = node.child_by_field_name("value") {
                append_function_reference(node, value, source, "initializer", facts);
            }
        }
        "assignment_expression" => {
            if let Some(value) = node.child_by_field_name("right") {
                append_function_reference(node, value, source, "assignment", facts);
            }
        }
        "pair" => {
            if let Some(value) = node.child_by_field_name("value") {
                append_function_reference(node, value, source, "object property", facts);
            }
        }
        "array" => {
            let mut cursor = node.walk();
            for value in node.named_children(&mut cursor) {
                append_function_reference(node, value, source, "array element", facts);
            }
        }
        _ => {}
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_javascript_function_references(child, source, facts);
    }
}

fn append_function_reference(
    observation: Node<'_>,
    value: Node<'_>,
    source: &[u8],
    role: &str,
    facts: &mut FileFacts,
) {
    let Some(target_name) = referenced_callable_name(value, source) else {
        return;
    };
    let owner = owning_symbol(observation, &facts.symbols)
        .or_else(|| facts.symbols.first())
        .expect("file symbol");
    facts.unresolved_references.push(UnresolvedReference {
        source_id: owner.id.clone(),
        target_name: target_name.clone(),
        binding_name: target_name.clone(),
        target_file_hint: None,
        kind: RelationshipKind::References,
        provenance: "tree-sitter/function-reference".to_owned(),
        confidence: 0.95,
        explanation: format!("{role} stores callable reference {target_name}"),
        file: facts.path.clone(),
        line: value.start_position().row + 1,
    });
}

fn collect_javascript_registrations(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "call_expression" {
        enrich_javascript_call(node, source, facts);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_javascript_registrations(child, source, facts);
    }
}

fn collect_fastapi_routes(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "decorated_definition" {
        enrich_fastapi_definition(node, source, facts);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_fastapi_routes(child, source, facts);
    }
}

fn enrich_fastapi_definition(definition: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let mut cursor = definition.walk();
    let children = definition.named_children(&mut cursor).collect::<Vec<_>>();
    let Some(handler) = children
        .iter()
        .copied()
        .find(|child| child.kind() == "function_definition")
    else {
        return;
    };
    let Some(handler_name) = handler
        .child_by_field_name("name")
        .map(|name| text(name, source))
    else {
        return;
    };
    for decorator in children
        .iter()
        .copied()
        .filter(|child| child.kind() == "decorator")
    {
        let Some(call) = first_descendant_of_kind(decorator, "call") else {
            continue;
        };
        let Some(function) = call.child_by_field_name("function") else {
            continue;
        };
        let Some(method) = member_method(function, source) else {
            continue;
        };
        let Some(receiver) = member_receiver(function, source) else {
            continue;
        };
        if !matches!(receiver.as_str(), "app" | "router")
            || !matches!(
                method.as_str(),
                "get" | "post" | "put" | "patch" | "delete" | "options" | "head"
            )
        {
            continue;
        }
        let Some(arguments) = call.child_by_field_name("arguments") else {
            continue;
        };
        let mut argument_cursor = arguments.walk();
        let Some(path_argument) = arguments.named_children(&mut argument_cursor).next() else {
            continue;
        };
        let Some(mut path) = string_literal(path_argument, source) else {
            continue;
        };
        if path.is_empty() {
            path.push('/');
        }
        let verb = method.to_ascii_uppercase();
        let name = format!("{verb} {path}");
        let route = Symbol::new_disambiguated(
            facts.language,
            SymbolKind::Route,
            &name,
            &name,
            &facts.path,
            span(decorator),
            &format!("fastapi|{verb}|{path}|{handler_name}"),
        );
        let file_symbol = facts.symbols.first().expect("file symbol");
        facts.relationships.push(Relationship {
            source_id: file_symbol.id.clone(),
            target_id: route.id.clone(),
            kind: RelationshipKind::Contains,
            evidence: Evidence::new(
                "framework/fastapi-route",
                1.0,
                format!("{name} is registered in {}", facts.path),
                &facts.path,
                decorator.start_position().row + 1,
            ),
        });
        facts.unresolved_calls.push(UnresolvedCall {
            caller_id: route.id.clone(),
            fallback_caller_id: None,
            callee_name: handler_name.clone(),
            receiver_binding: None,
            receiver_type: None,
            receiver_call_start_byte: None,
            target_file_hint: None,
            provenance: "framework/fastapi-route".to_owned(),
            confidence: 0.99,
            explanation: format!("FastAPI route {name} decorates handler {handler_name}"),
            resolvable: true,
            file: facts.path.clone(),
            line: decorator.start_position().row + 1,
            start_byte: decorator.start_byte(),
        });
        facts.symbols.push(route);
    }
}

fn collect_django_routes(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "call" {
        enrich_django_call(node, source, facts);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_django_routes(child, source, facts);
    }
}

fn enrich_django_call(call: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let Some(function) = call.child_by_field_name("function") else {
        return;
    };
    let Some(arguments_node) = call.child_by_field_name("arguments") else {
        return;
    };
    let mut cursor = arguments_node.walk();
    let arguments = arguments_node
        .named_children(&mut cursor)
        .collect::<Vec<_>>();
    let (path, handler, target_file_hint, label) =
        if matches!(text(function, source).as_str(), "path" | "re_path" | "url") {
            let Some(path) = arguments
                .first()
                .and_then(|argument| string_literal(*argument, source))
            else {
                return;
            };
            let Some((handler, target_file_hint)) = arguments
                .get(1)
                .and_then(|argument| django_handler_target(*argument, source))
            else {
                return;
            };
            (path, handler, target_file_hint, "ROUTE")
        } else if member_method(function, source).as_deref() == Some("register")
            && member_receiver(function, source).as_deref() == Some("router")
        {
            let Some(prefix) = arguments
                .first()
                .and_then(|argument| string_literal(*argument, source))
            else {
                return;
            };
            let Some((handler, target_file_hint)) = arguments
                .get(1)
                .and_then(|argument| django_handler_target(*argument, source))
                .filter(|(handler, _)| handler.ends_with("View") || handler.ends_with("ViewSet"))
            else {
                return;
            };
            (
                format!("/{}", prefix.trim_matches('/')),
                handler,
                target_file_hint,
                "VIEWSET",
            )
        } else {
            return;
        };

    let name = format!("{label} /{}", path.trim_start_matches('/'));
    let occurrence = facts
        .symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Route && symbol.name == name)
        .count()
        + 1;
    let route = Symbol::new_disambiguated(
        facts.language,
        SymbolKind::Route,
        &name,
        &name,
        &facts.path,
        span(call),
        &format!("django|{label}|{path}|{handler}|occurrence:{occurrence}"),
    );
    let file_symbol = facts.symbols.first().expect("file symbol");
    facts.relationships.push(Relationship {
        source_id: file_symbol.id.clone(),
        target_id: route.id.clone(),
        kind: RelationshipKind::Contains,
        evidence: Evidence::new(
            "framework/django-route",
            1.0,
            format!("{name} is registered in {}", facts.path),
            &facts.path,
            call.start_position().row + 1,
        ),
    });
    facts.unresolved_calls.push(UnresolvedCall {
        caller_id: route.id.clone(),
        fallback_caller_id: None,
        callee_name: handler.clone(),
        receiver_binding: None,
        receiver_type: None,
        receiver_call_start_byte: None,
        target_file_hint,
        provenance: "framework/django-route".to_owned(),
        confidence: 0.98,
        explanation: format!("Django {label} {name} registers handler {handler}"),
        resolvable: true,
        file: facts.path.clone(),
        line: call.start_position().row + 1,
        start_byte: call.start_byte(),
    });
    facts.symbols.push(route);
}

fn django_handler_target(node: Node<'_>, source: &[u8]) -> Option<(String, Option<String>)> {
    match node.kind() {
        "identifier" => Some((text(node, source), None)),
        "attribute" => {
            let name = node
                .child_by_field_name("attribute")
                .map(|attribute| text(attribute, source))?;
            let hint = node
                .child_by_field_name("object")
                .or_else(|| node.child_by_field_name("value"))
                .filter(|receiver| receiver.kind() == "identifier")
                .map(|receiver| text(receiver, source));
            Some((name, hint))
        }
        "call" => {
            let function = node.child_by_field_name("function")?;
            if member_method(function, source).as_deref() != Some("as_view") {
                return None;
            }
            let receiver = function
                .child_by_field_name("object")
                .or_else(|| function.child_by_field_name("value"))?;
            match receiver.kind() {
                "identifier" => Some((text(receiver, source), None)),
                "attribute" => {
                    let name = receiver
                        .child_by_field_name("attribute")
                        .map(|attribute| text(attribute, source))?;
                    let hint = receiver
                        .child_by_field_name("object")
                        .or_else(|| receiver.child_by_field_name("value"))
                        .filter(|object| object.kind() == "identifier")
                        .map(|object| text(object, source));
                    Some((name, hint))
                }
                _ => None,
            }
        }
        _ => None,
    }
    .filter(|(name, _)| !name.is_empty())
}

fn first_descendant_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    if node.kind() == kind {
        return Some(node);
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| first_descendant_of_kind(child, kind));
    found
}

fn enrich_javascript_call(call: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let Some(function) = call.child_by_field_name("function") else {
        return;
    };
    let Some(method) = member_method(function, source) else {
        return;
    };
    let receiver = member_receiver(function, source);
    let arguments = call
        .child_by_field_name("arguments")
        .map(|arguments| {
            let mut cursor = arguments.walk();
            arguments.named_children(&mut cursor).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if arguments.is_empty() {
        return;
    }
    collect_react_router_objects(call, method.as_str(), &arguments, source, facts);
    collect_dynamic_event(
        call,
        receiver.as_deref(),
        method.as_str(),
        &arguments,
        source,
        facts,
    );

    let route = express_route(receiver.as_deref(), method.as_str(), &arguments, source).map(
        |(verb, path)| {
            facts.unresolved_calls.retain(|pending| {
                !(pending.provenance == "tree-sitter/name-resolution"
                    && pending.callee_name == method
                    && pending.line == call.start_position().row + 1)
            });
            let name = format!("{verb} {path}");
            let occurrence = facts
                .symbols
                .iter()
                .filter(|symbol| symbol.kind == SymbolKind::Route && symbol.name == name)
                .count()
                + 1;
            let symbol = Symbol::new_disambiguated(
                facts.language,
                SymbolKind::Route,
                &name,
                &name,
                &facts.path,
                span(call),
                &format!("express|{verb}|{path}|occurrence:{occurrence}"),
            );
            let file_symbol = facts.symbols.first().expect("file symbol");
            facts.relationships.push(Relationship {
                source_id: file_symbol.id.clone(),
                target_id: symbol.id.clone(),
                kind: RelationshipKind::Contains,
                evidence: Evidence::new(
                    "framework/express-route",
                    1.0,
                    format!("{name} is registered in {}", facts.path),
                    &facts.path,
                    call.start_position().row + 1,
                ),
            });
            facts.symbols.push(symbol.clone());
            symbol
        },
    );

    let callbacks = if route.is_some() {
        arguments.iter().skip(1).copied().collect::<Vec<_>>()
    } else {
        callback_argument_index(method.as_str(), &arguments, source, false)
            .and_then(|index| arguments.get(index).copied())
            .into_iter()
            .collect()
    };
    for callback in callbacks {
        let Some(target_name) = referenced_callable_name(callback, source) else {
            continue;
        };
        let (caller_id, provenance, confidence, explanation) = if let Some(route) = &route {
            (
                route.id.clone(),
                "framework/express-route",
                0.98,
                format!(
                    "Express route {} registers handler {target_name}",
                    route.name
                ),
            )
        } else {
            let owner = owning_symbol(call, &facts.symbols)
                .or_else(|| facts.symbols.first())
                .expect("file symbol");
            (
                owner.id.clone(),
                "tree-sitter/callback-registration",
                0.95,
                format!("{method} registers callback {target_name}"),
            )
        };
        facts.unresolved_calls.push(UnresolvedCall {
            caller_id,
            fallback_caller_id: None,
            callee_name: target_name,
            receiver_binding: None,
            receiver_type: None,
            receiver_call_start_byte: None,
            target_file_hint: None,
            provenance: provenance.to_owned(),
            confidence,
            explanation,
            resolvable: true,
            file: facts.path.clone(),
            line: call.start_position().row + 1,
            start_byte: call.start_byte(),
        });
    }
}

fn collect_react_router_jsx(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if matches!(
        node.kind(),
        "jsx_self_closing_element" | "jsx_opening_element"
    ) {
        enrich_react_route_element(node, source, facts);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_react_router_jsx(child, source, facts);
    }
}

fn collect_react_runtime_edges(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    collect_react_class_rerenders(root, source, facts);
    collect_jsx_child_renders(root, source, facts);
}

fn collect_react_class_rerenders(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "class_declaration" && is_react_component_class(node, source) {
        let class_name = node
            .child_by_field_name("name")
            .map(|name| text(name, source))
            .unwrap_or_default();
        let methods = direct_children_of_kind(
            node.child_by_field_name("body").unwrap_or(node),
            "method_definition",
        );
        let render = methods.iter().copied().find(|method| {
            method
                .child_by_field_name("name")
                .is_some_and(|name| text(name, source) == "render")
        });
        if let Some(render) = render {
            for method in methods {
                if method.id() == render.id() || !contains_this_set_state(method, source) {
                    continue;
                }
                let Some(owner) = owning_callable_symbol(method, &facts.symbols) else {
                    continue;
                };
                facts.unresolved_calls.push(UnresolvedCall {
                    caller_id: owner.id.clone(),
                    fallback_caller_id: None,
                    callee_name: "render".to_owned(),
                    receiver_binding: None,
                    receiver_type: Some(class_name.clone()),
                    receiver_call_start_byte: None,
                    target_file_hint: Some(facts.path.clone()),
                    provenance: "framework/react-render".to_owned(),
                    confidence: 0.98,
                    explanation: format!(
                        "{} calls this.setState, which schedules {}.render",
                        owner.qualified_name, class_name
                    ),
                    resolvable: true,
                    file: facts.path.clone(),
                    line: method.start_position().row + 1,
                    start_byte: method.start_byte(),
                });
            }
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_react_class_rerenders(child, source, facts);
    }
}

fn is_react_component_class(class: Node<'_>, source: &[u8]) -> bool {
    let Some(heritage) = direct_children_of_kind(class, "class_heritage")
        .into_iter()
        .next()
    else {
        return false;
    };
    text(heritage, source)
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '.'))
        .any(|token| {
            matches!(
                token,
                "Component" | "PureComponent" | "React.Component" | "React.PureComponent"
            )
        })
}

fn contains_this_set_state(node: Node<'_>, source: &[u8]) -> bool {
    if node.kind() == "call_expression" {
        if let Some(function) = node.child_by_field_name("function") {
            if function.kind() == "member_expression"
                && function
                    .child_by_field_name("object")
                    .is_some_and(|object| text(object, source) == "this")
                && function
                    .child_by_field_name("property")
                    .is_some_and(|property| text(property, source) == "setState")
            {
                return true;
            }
        }
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .any(|child| contains_this_set_state(child, source));
    found
}

fn collect_jsx_child_renders(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if matches!(
        node.kind(),
        "jsx_self_closing_element" | "jsx_opening_element"
    ) {
        append_jsx_child_render(node, source, facts);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_jsx_child_renders(child, source, facts);
    }
}

fn append_jsx_child_render(element: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let Some(name_node) = element.child_by_field_name("name") else {
        return;
    };
    let child = text(name_node, source);
    if name_node.kind() != "identifier"
        || !child.starts_with(char::is_uppercase)
        || matches!(child.as_str(), "Route" | "Routes" | "Fragment")
    {
        return;
    }
    let Some(owner) = owning_callable_symbol(element, &facts.symbols) else {
        return;
    };
    if owner.name == child
        || facts.unresolved_calls.iter().any(|pending| {
            pending.caller_id == owner.id
                && pending.callee_name == child
                && pending.provenance == "framework/jsx-render"
        })
    {
        return;
    }
    let target_file_hint = facts
        .unresolved_references
        .iter()
        .find(|reference| {
            reference.kind == RelationshipKind::Imports && reference.binding_name == child
        })
        .and_then(|reference| reference.target_file_hint.clone());
    facts.unresolved_calls.push(UnresolvedCall {
        caller_id: owner.id.clone(),
        fallback_caller_id: None,
        callee_name: child.clone(),
        receiver_binding: None,
        receiver_type: None,
        receiver_call_start_byte: None,
        target_file_hint,
        provenance: "framework/jsx-render".to_owned(),
        confidence: 0.96,
        explanation: format!("{} renders JSX child {child}", owner.qualified_name),
        resolvable: true,
        file: facts.path.clone(),
        line: element.start_position().row + 1,
        start_byte: element.start_byte(),
    });
}

fn owning_callable_symbol<'facts>(
    node: Node<'_>,
    symbols: &'facts [Symbol],
) -> Option<&'facts Symbol> {
    owning_symbol(node, symbols).filter(|symbol| {
        matches!(
            symbol.kind,
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Component
        )
    })
}

fn direct_children_of_kind<'tree>(node: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| child.kind() == kind)
        .collect()
}

fn enrich_react_route_element(element: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    let Some(name) = element.child_by_field_name("name") else {
        return;
    };
    if text(name, source) != "Route" {
        return;
    }
    let Some(path) = jsx_attribute(element, "path", source).and_then(|value| {
        string_literal(value, source).or_else(|| first_string_descendant(value, source))
    }) else {
        return;
    };
    let component = jsx_attribute(element, "component", source)
        .and_then(|value| first_identifier_descendant(value, source))
        .or_else(|| {
            jsx_attribute(element, "element", source)
                .and_then(|value| first_jsx_element_name(value, source))
        });
    let Some(component) = component else {
        return;
    };
    append_react_route(element, path, component, facts);
}

fn collect_react_router_objects(
    call: Node<'_>,
    function_name: &str,
    arguments: &[Node<'_>],
    source: &[u8],
    facts: &mut FileFacts,
) {
    if !matches!(
        function_name,
        "createBrowserRouter"
            | "createHashRouter"
            | "createMemoryRouter"
            | "createRoutesFromElements"
    ) {
        return;
    }
    let Some(configuration) = arguments.first() else {
        return;
    };
    collect_route_objects(*configuration, source, facts);
    facts.unresolved_calls.retain(|pending| {
        !(pending.provenance == "tree-sitter/name-resolution"
            && pending.callee_name == function_name
            && pending.line == call.start_position().row + 1)
    });
}

fn collect_route_objects(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "object" {
        let mut path = None;
        let mut component = None;
        let mut cursor = node.walk();
        for pair in node
            .named_children(&mut cursor)
            .filter(|child| child.kind() == "pair")
        {
            let Some(key) = pair.child_by_field_name("key") else {
                continue;
            };
            let Some(value) = pair.child_by_field_name("value") else {
                continue;
            };
            match text(key, source).trim_matches(['\'', '"']) {
                "path" => {
                    path = string_literal(value, source)
                        .or_else(|| first_string_descendant(value, source));
                }
                "Component" | "component" => {
                    component = first_identifier_descendant(value, source);
                }
                "element" => component = first_jsx_element_name(value, source),
                _ => {}
            }
        }
        if let (Some(path), Some(component)) = (path, component) {
            append_react_route(node, path, component, facts);
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_route_objects(child, source, facts);
    }
}

fn append_react_route(
    registration: Node<'_>,
    path: String,
    component: String,
    facts: &mut FileFacts,
) {
    let name = format!("ROUTE {path}");
    let route = Symbol::new_disambiguated(
        facts.language,
        SymbolKind::Route,
        &name,
        &name,
        &facts.path,
        span(registration),
        &format!("react-router|{path}|{component}"),
    );
    let file_symbol = facts.symbols.first().expect("file symbol");
    facts.relationships.push(Relationship {
        source_id: file_symbol.id.clone(),
        target_id: route.id.clone(),
        kind: RelationshipKind::Contains,
        evidence: Evidence::new(
            "framework/react-router",
            1.0,
            format!("{name} is registered in {}", facts.path),
            &facts.path,
            registration.start_position().row + 1,
        ),
    });
    facts.unresolved_calls.push(UnresolvedCall {
        caller_id: route.id.clone(),
        fallback_caller_id: None,
        callee_name: component.clone(),
        receiver_binding: None,
        receiver_type: None,
        receiver_call_start_byte: None,
        target_file_hint: None,
        provenance: "framework/react-router".to_owned(),
        confidence: 0.98,
        explanation: format!("React Router path {path} renders component {component}"),
        resolvable: true,
        file: facts.path.clone(),
        line: registration.start_position().row + 1,
        start_byte: registration.start_byte(),
    });
    facts.symbols.push(route);
}

fn jsx_attribute<'tree>(
    element: Node<'tree>,
    expected: &str,
    source: &[u8],
) -> Option<Node<'tree>> {
    let mut cursor = element.walk();
    let found = element
        .named_children(&mut cursor)
        .filter(|child| child.kind() == "jsx_attribute")
        .find_map(|attribute| {
            let name = attribute
                .child_by_field_name("name")
                .or_else(|| attribute.named_child(0))?;
            (text(name, source) == expected)
                .then(|| {
                    attribute
                        .child_by_field_name("value")
                        .or_else(|| attribute.named_child(1))
                })
                .flatten()
        });
    found
}

fn first_string_descendant(node: Node<'_>, source: &[u8]) -> Option<String> {
    if let Some(value) = string_literal(node, source) {
        return Some(value);
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| first_string_descendant(child, source));
    found
}

fn first_identifier_descendant(node: Node<'_>, source: &[u8]) -> Option<String> {
    if matches!(node.kind(), "identifier" | "property_identifier") {
        return Some(text(node, source));
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| first_identifier_descendant(child, source));
    found
}

fn first_jsx_element_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    if matches!(
        node.kind(),
        "jsx_self_closing_element" | "jsx_opening_element"
    ) {
        return node
            .child_by_field_name("name")
            .map(|name| text(name, source))
            .filter(|name| name != "Route" && name != "Routes");
    }
    let mut cursor = node.walk();
    let found = node
        .named_children(&mut cursor)
        .find_map(|child| first_jsx_element_name(child, source));
    found
}

fn collect_dynamic_event(
    call: Node<'_>,
    receiver: Option<&str>,
    method: &str,
    arguments: &[Node<'_>],
    source: &[u8],
    facts: &mut FileFacts,
) {
    let Some(receiver) = receiver else {
        return;
    };
    let Some(channel) = arguments
        .first()
        .and_then(|argument| string_literal(*argument, source))
    else {
        return;
    };
    let (action, callback_name) = match method {
        "on" | "once" => (
            EventAction::Register,
            arguments
                .get(1)
                .and_then(|argument| referenced_callable_name(*argument, source)),
        ),
        "emit" | "dispatchEvent" => (EventAction::Dispatch, None),
        _ => return,
    };
    if action == EventAction::Register && callback_name.is_none() {
        return;
    }
    let owner = owning_symbol(call, &facts.symbols)
        .or_else(|| facts.symbols.first())
        .expect("file symbol");
    facts.dynamic_events.push(DynamicEventFact {
        owner_id: owner.id.clone(),
        receiver: receiver.to_owned(),
        channel: EventChannel::Canonical(channel),
        action,
        callback_name,
        file: facts.path.clone(),
        line: call.start_position().row + 1,
    });
}

fn express_route(
    receiver: Option<&str>,
    method: &str,
    arguments: &[Node<'_>],
    source: &[u8],
) -> Option<(String, String)> {
    const METHODS: &[&str] = &[
        "get", "post", "put", "patch", "delete", "options", "head", "all", "use",
    ];
    if !matches!(receiver, Some("app" | "router")) || !METHODS.contains(&method) {
        return None;
    }
    let path = string_literal(*arguments.first()?, source)?;
    if method == "use" && !path.starts_with('/') {
        return None;
    }
    Some((method.to_ascii_uppercase(), path))
}

fn callback_argument_index(
    method: &str,
    arguments: &[Node<'_>],
    source: &[u8],
    is_route: bool,
) -> Option<usize> {
    if is_route {
        return Some(arguments.len() - 1);
    }
    match method {
        "on" | "once" | "addEventListener" => (arguments.len() >= 2).then_some(1),
        "then" | "catch" | "finally" => Some(0),
        _ => {
            let function_name = method;
            matches!(
                function_name,
                "setTimeout" | "setInterval" | "queueMicrotask" | "requestAnimationFrame"
            )
            .then_some(0)
        }
    }
    .filter(|index| {
        arguments
            .get(*index)
            .is_some_and(|argument| referenced_callable_name(*argument, source).is_some())
    })
}

fn member_method(function: Node<'_>, source: &[u8]) -> Option<String> {
    match function.kind() {
        "identifier" => Some(text(function, source)),
        "member_expression" => function
            .child_by_field_name("property")
            .map(|property| text(property, source)),
        "attribute" => function
            .child_by_field_name("attribute")
            .map(|attribute| text(attribute, source)),
        _ => None,
    }
}

fn member_receiver(function: Node<'_>, source: &[u8]) -> Option<String> {
    matches!(function.kind(), "member_expression" | "attribute")
        .then(|| {
            function
                .child_by_field_name("object")
                .or_else(|| function.child_by_field_name("value"))
        })
        .flatten()
        .filter(|object| object.kind() == "identifier")
        .map(|object| text(object, source))
}

fn referenced_callable_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => Some(text(node, source)),
        "member_expression" => node
            .child_by_field_name("property")
            .map(|property| text(property, source)),
        _ => None,
    }
    .filter(|name| !name.is_empty())
}

fn string_literal(node: Node<'_>, source: &[u8]) -> Option<String> {
    if !matches!(node.kind(), "string" | "string_literal") {
        return None;
    }
    let literal = text(node, source);
    let quote_index = literal.find(['\'', '"'])?;
    if !literal[..quote_index]
        .chars()
        .all(|character| matches!(character, 'r' | 'R' | 'b' | 'B' | 'u' | 'U'))
    {
        return None;
    }
    let quote = literal.as_bytes()[quote_index];
    (literal.len() > quote_index + 1 && literal.as_bytes().last() == Some(&quote))
        .then(|| literal[quote_index + 1..literal.len() - 1].to_owned())
}

fn owning_symbol<'a>(node: Node<'_>, symbols: &'a [Symbol]) -> Option<&'a Symbol> {
    symbols
        .iter()
        .filter(|symbol| {
            symbol.kind != SymbolKind::File
                && symbol.start_byte <= node.start_byte()
                && symbol.end_byte >= node.end_byte()
        })
        .min_by_key(|symbol| symbol.end_byte.saturating_sub(symbol.start_byte))
}

fn text(node: Node<'_>, source: &[u8]) -> String {
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
