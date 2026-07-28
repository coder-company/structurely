use crate::model::{
    DynamicEventFact, EventAction, Evidence, FileFacts, Language, Relationship, RelationshipKind,
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
            collect_javascript_registrations(root, source, facts);
            collect_javascript_function_references(root, source, facts);
            collect_nestjs_routes(root, source, facts);
            if facts.language == Language::ArkTs {
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

fn collect_arkui_semantics(node: Node<'_>, source: &[u8], facts: &mut FileFacts) {
    if node.kind() == "struct_declaration"
        && ["Component", "ComponentV2"]
            .iter()
            .any(|decorator| has_decorator(node, decorator, source))
    {
        enrich_arkui_component(node, source, facts);
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_arkui_semantics(child, source, facts);
    }
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
            callee_name: "build".to_owned(),
            receiver_type: Some(component_name.clone()),
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

fn remove_arkui_intrinsic_calls(facts: &mut FileFacts) {
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
            || !ARKUI_INTRINSICS.contains(&call.callee_name.as_str())
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
        callee_name: target.clone(),
        receiver_type: (provenance == "framework/arkui-event").then(|| component_name.to_owned()),
        target_file_hint: (provenance == "framework/arkui-event").then(|| facts.path.clone()),
        provenance: provenance.to_owned(),
        confidence,
        explanation: format!("{component_name} ArkUI flow invokes {target}"),
        resolvable: true,
        file: facts.path.clone(),
        line: observation.start_position().row + 1,
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
            callee_name: target.clone(),
            receiver_type: None,
            target_file_hint,
            provenance: provenance.to_owned(),
            confidence: 0.96,
            explanation: format!("{component_name} template invokes or renders {target}"),
            resolvable: true,
            file: facts.path.clone(),
            line: byte_line(source, offset),
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
                    callee_name: "render".to_owned(),
                    receiver_type: Some(class_name.clone()),
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
        callee_name: child.clone(),
        receiver_type: None,
        target_file_hint,
        provenance: "framework/jsx-render".to_owned(),
        confidence: 0.96,
        explanation: format!("{} renders JSX child {child}", owner.qualified_name),
        resolvable: true,
        file: facts.path.clone(),
        line: element.start_position().row + 1,
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
