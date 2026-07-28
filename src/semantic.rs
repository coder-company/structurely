use crate::model::{
    DynamicEventFact, EventAction, Evidence, FileFacts, Language, Relationship, RelationshipKind,
    SourceSpan, Symbol, SymbolKind, UnresolvedCall,
};
use tree_sitter::Node;

pub(crate) fn enrich_file_facts(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if !matches!(
        facts.language,
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
    ) {
        return;
    }
    collect_javascript_registrations(root, source, facts);
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
            let symbol = Symbol::new_disambiguated(
                facts.language,
                SymbolKind::Route,
                &name,
                &name,
                &facts.path,
                span(call),
                &format!("express|{verb}|{path}"),
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

    let callback_index =
        callback_argument_index(method.as_str(), &arguments, source, route.is_some());
    let Some(callback) = callback_index.and_then(|index| arguments.get(index).copied()) else {
        return;
    };
    let Some(target_name) = referenced_callable_name(callback, source) else {
        return;
    };
    let (caller_id, provenance, confidence, explanation) = if let Some(route) = route {
        (
            route.id,
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
        callee_name: target_name,
        receiver_type: None,
        provenance: provenance.to_owned(),
        confidence,
        explanation,
        resolvable: true,
        file: facts.path.clone(),
        line: call.start_position().row + 1,
    });
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
        channel,
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
        _ => None,
    }
}

fn member_receiver(function: Node<'_>, source: &[u8]) -> Option<String> {
    (function.kind() == "member_expression")
        .then(|| function.child_by_field_name("object"))
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
    matches!(node.kind(), "string" | "string_literal")
        .then(|| text(node, source).trim_matches(['\'', '"']).to_owned())
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
