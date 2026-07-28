use crate::model::{
    DynamicEventFact, EventAction, Evidence, FileFacts, Language, Relationship, RelationshipKind,
    SourceSpan, Symbol, SymbolKind, UnresolvedCall, UnresolvedReference,
};
use tree_sitter::Node;

pub(crate) fn enrich_file_facts(root: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    match facts.language {
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
            collect_javascript_registrations(root, source, facts);
            collect_javascript_function_references(root, source, facts);
            collect_nestjs_routes(root, source, facts);
            if matches!(facts.language, Language::Tsx | Language::Jsx) {
                collect_react_router_jsx(root, source, facts);
            }
        }
        Language::Python => {
            collect_fastapi_routes(root, source, facts);
            collect_django_routes(root, source, facts);
        }
        _ => {}
    }
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
                        callee_name: handler.clone(),
                        receiver_type: None,
                        target_file_hint: None,
                        provenance: "framework/nestjs-route".to_owned(),
                        confidence: 0.99,
                        explanation: format!("NestJS route {name} decorates handler {handler}"),
                        resolvable: true,
                        file: facts.path.clone(),
                        line: decorator.start_position().row + 1,
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
            callee_name: handler_name.clone(),
            receiver_type: None,
            target_file_hint: None,
            provenance: "framework/fastapi-route".to_owned(),
            confidence: 0.99,
            explanation: format!("FastAPI route {name} decorates handler {handler_name}"),
            resolvable: true,
            file: facts.path.clone(),
            line: decorator.start_position().row + 1,
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
        callee_name: handler.clone(),
        receiver_type: None,
        target_file_hint,
        provenance: "framework/django-route".to_owned(),
        confidence: 0.98,
        explanation: format!("Django {label} {name} registers handler {handler}"),
        resolvable: true,
        file: facts.path.clone(),
        line: call.start_position().row + 1,
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
            callee_name: target_name,
            receiver_type: None,
            target_file_hint: None,
            provenance: provenance.to_owned(),
            confidence,
            explanation,
            resolvable: true,
            file: facts.path.clone(),
            line: call.start_position().row + 1,
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
        callee_name: component.clone(),
        receiver_type: None,
        target_file_hint: None,
        provenance: "framework/react-router".to_owned(),
        confidence: 0.98,
        explanation: format!("React Router path {path} renders component {component}"),
        resolvable: true,
        file: facts.path.clone(),
        line: registration.start_position().row + 1,
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
