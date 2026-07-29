use serde::{Deserialize, Serialize};
use std::{fmt, path::Path};

pub const GRAPH_MODEL_VERSION: u32 = 62;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[serde(rename = "typescript")]
    TypeScript,
    Tsx,
    #[serde(rename = "javascript")]
    JavaScript,
    Jsx,
    Vue,
    Svelte,
    Astro,
    #[serde(rename = "arkts")]
    ArkTs,
    Python,
    Rust,
    Go,
    Java,
    #[serde(rename = "csharp")]
    CSharp,
    C,
    Cpp,
    Dart,
    Ruby,
    Php,
    Swift,
    Lua,
    Kotlin,
    Scala,
    R,
}

impl Language {
    pub fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "jsx" => Some(Self::Jsx),
            "vue" => Some(Self::Vue),
            "svelte" => Some(Self::Svelte),
            "astro" => Some(Self::Astro),
            "ets" => Some(Self::ArkTs),
            "py" | "pyi" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            "cs" => Some(Self::CSharp),
            "c" | "h" => Some(Self::C),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some(Self::Cpp),
            "dart" => Some(Self::Dart),
            "rb" | "rake" => Some(Self::Ruby),
            "php" | "phtml" => Some(Self::Php),
            "swift" => Some(Self::Swift),
            "lua" => Some(Self::Lua),
            "kt" | "kts" => Some(Self::Kotlin),
            "scala" | "sc" => Some(Self::Scala),
            "r" => Some(Self::R),
            _ => None,
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::JavaScript => "javascript",
            Self::Jsx => "jsx",
            Self::Vue => "vue",
            Self::Svelte => "svelte",
            Self::Astro => "astro",
            Self::ArkTs => "arkts",
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::Dart => "dart",
            Self::Ruby => "ruby",
            Self::Php => "php",
            Self::Swift => "swift",
            Self::Lua => "lua",
            Self::Kotlin => "kotlin",
            Self::Scala => "scala",
            Self::R => "r",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    File,
    Class,
    Interface,
    Struct,
    Trait,
    Enum,
    Type,
    Function,
    Method,
    Variable,
    Route,
    Component,
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::File => "file",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Struct => "struct",
            Self::Trait => "trait",
            Self::Enum => "enum",
            Self::Type => "type",
            Self::Function => "function",
            Self::Method => "method",
            Self::Variable => "variable",
            Self::Route => "route",
            Self::Component => "component",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Symbol {
    pub id: String,
    pub semantic_key: String,
    pub language: Language,
    pub kind: SymbolKind,
    pub name: String,
    pub qualified_name: String,
    pub file: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

impl Symbol {
    pub fn new(
        language: Language,
        kind: SymbolKind,
        name: impl Into<String>,
        qualified_name: impl Into<String>,
        file: impl Into<String>,
        span: SourceSpan,
    ) -> Self {
        Self::new_disambiguated(language, kind, name, qualified_name, file, span, "")
    }

    pub fn new_disambiguated(
        language: Language,
        kind: SymbolKind,
        name: impl Into<String>,
        qualified_name: impl Into<String>,
        file: impl Into<String>,
        span: SourceSpan,
        discriminator: &str,
    ) -> Self {
        let name = name.into();
        let qualified_name = qualified_name.into();
        let file = file.into();
        let semantic_key = if discriminator.is_empty() {
            format!("{language}|{kind}|{file}|{qualified_name}")
        } else {
            let signature = blake3::hash(discriminator.as_bytes()).to_hex();
            format!(
                "{language}|{kind}|{file}|{qualified_name}|signature:{}",
                &signature[..16]
            )
        };
        let digest = blake3::hash(semantic_key.as_bytes()).to_hex();
        Self {
            id: format!("sym_{}", &digest[..24]),
            semantic_key,
            language,
            kind,
            name,
            qualified_name,
            file,
            start_byte: span.start_byte,
            end_byte: span.end_byte,
            start_line: span.start_line,
            end_line: span.end_line,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    Contains,
    Calls,
    References,
    Imports,
    Extends,
    Implements,
}

impl fmt::Display for RelationshipKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Contains => "contains",
            Self::Calls => "calls",
            Self::References => "references",
            Self::Imports => "imports",
            Self::Extends => "extends",
            Self::Implements => "implements",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub provenance: String,
    pub confidence: f64,
    pub explanation: String,
    pub file: String,
    pub line: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<usize>,
}

impl Evidence {
    pub fn new(
        provenance: impl Into<String>,
        confidence: f64,
        explanation: impl Into<String>,
        file: impl Into<String>,
        line: usize,
    ) -> Self {
        Self {
            provenance: provenance.into(),
            confidence: confidence.clamp(0.0, 1.0),
            explanation: explanation.into(),
            file: file.into(),
            line,
            site: None,
        }
    }

    pub fn at_site(mut self, site: usize) -> Self {
        self.site = Some(site);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub source_id: String,
    pub target_id: String,
    pub kind: RelationshipKind,
    pub evidence: Evidence,
}

#[derive(Debug, Clone)]
pub(crate) struct UnresolvedCall {
    pub caller_id: String,
    pub fallback_caller_id: Option<String>,
    pub callee_name: String,
    pub receiver_binding: Option<String>,
    pub receiver_type: Option<String>,
    pub receiver_call_start_byte: Option<usize>,
    pub target_file_hint: Option<String>,
    pub provenance: String,
    pub confidence: f64,
    pub explanation: String,
    pub resolvable: bool,
    pub file: String,
    pub line: usize,
    pub start_byte: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CallableReturnFact {
    pub owner_id: String,
    pub type_name: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CallbackParameterInvocation {
    pub owner_id: String,
    pub parameter_index: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CallbackArgumentFact {
    pub caller_id: String,
    pub callee_name: String,
    pub argument_index: usize,
    pub formal_name: Option<String>,
    pub target_name: String,
    pub target_qualified_hint: Option<String>,
    pub target_symbol: Option<Symbol>,
    pub line: usize,
    pub call_start_byte: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct PythonCallbackFormalFact {
    pub owner_id: String,
    pub formal_name: String,
    pub parameter_index: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct CallbackParameterDelegationFact {
    pub owner_id: String,
    pub parameter_index: usize,
    pub callee_name: String,
    pub argument_index: usize,
    pub line: usize,
    pub call_start_byte: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct ArkuiBuilderFlowFacts {
    pub builders: Vec<ArkuiBuilderDeclarationFact>,
    pub params: Vec<ArkuiBuilderParamDeclarationFact>,
    pub invocations: Vec<ArkuiBuilderParamInvocationFact>,
    pub assignments: Vec<ArkuiBuilderParamAssignmentFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArkuiBuilderDeclarationFact {
    pub target_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArkuiBuilderParamDeclarationFact {
    pub component_id: String,
    pub component_name: String,
    pub param_name: String,
    pub ordinal: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArkuiBuilderParamInvocationFact {
    pub component_id: String,
    pub param_name: String,
    pub owner_id: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ArkuiBuilderParamAssignmentFact {
    pub caller_id: String,
    pub component_binding: String,
    pub param_name: Option<String>,
    pub target_id: Option<String>,
    pub target_symbol: Option<Symbol>,
    pub target_binding: Option<String>,
    pub require_decorated_target: bool,
    pub line: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventAction {
    Register,
    Dispatch,
}

impl fmt::Display for EventAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Register => "register",
            Self::Dispatch => "dispatch",
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) enum EventChannel {
    Canonical(String),
    Imported {
        target_file_hint: String,
        export_name: String,
        member_path: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct DynamicEventFact {
    pub owner_id: String,
    pub receiver: String,
    pub channel: EventChannel,
    pub action: EventAction,
    pub callback_name: Option<String>,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct LiteralBindingFact {
    pub export_name: String,
    pub member_path: String,
    pub channel: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ModuleExportFact {
    pub export_name: String,
    pub target_file_hint: String,
    pub target_name: String,
    pub is_star: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerFacts {
    pub typedefs: Vec<CFunctionPointerTypedefFact>,
    pub layouts: Vec<CStructLayoutFact>,
    pub bindings: Vec<CFunctionPointerBindingFact>,
    pub propagations: Vec<CFunctionPointerPropagationFact>,
    pub dispatches: Vec<CFunctionPointerDispatchFact>,
    pub arrays: Vec<CFunctionPointerArrayFact>,
    pub array_dispatches: Vec<CFunctionPointerArrayDispatchFact>,
    pub includes: Vec<CIncludeFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerTypedefFact {
    pub name: String,
    pub pointer: bool,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CStructLayoutFact {
    pub type_name: String,
    pub fields: Vec<CStructFieldFact>,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CStructFieldFact {
    pub name: String,
    pub index: usize,
    pub value_type: Option<String>,
    pub function_pointer: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerBindingFact {
    pub owner_id: String,
    pub receiver_type: Option<String>,
    pub receiver_path: Vec<String>,
    pub field_name: Option<String>,
    pub field_index: Option<usize>,
    pub target_name: String,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerPropagationFact {
    pub target_receiver_type: Option<String>,
    pub target_receiver_path: Vec<String>,
    pub target_field_name: String,
    pub source_receiver_type: Option<String>,
    pub source_receiver_path: Vec<String>,
    pub source_field_name: String,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerDispatchFact {
    pub owner_id: String,
    pub receiver_type: Option<String>,
    pub receiver_path: Vec<String>,
    pub field_name: String,
    pub proven_function_pointer: bool,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerArrayFact {
    pub name: String,
    pub element_type: String,
    pub pointer_declarator: bool,
    pub targets: Vec<CFunctionPointerArrayTargetFact>,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerArrayTargetFact {
    pub target_name: String,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CFunctionPointerArrayDispatchFact {
    pub owner_id: String,
    pub name: String,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CIncludeFact {
    pub path: String,
    pub line: usize,
    pub site_start_byte: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct FastApiFacts {
    pub routers: Vec<FastApiRouterFact>,
    pub aliases: Vec<FastApiAliasFact>,
    pub factories: Vec<FastApiFactoryFact>,
    pub mounts: Vec<FastApiMountFact>,
    pub routes: Vec<FastApiRouteFact>,
    pub dependencies: Vec<FastApiDependencyFact>,
    pub dependency_aliases: Vec<FastApiAliasFact>,
    pub dependency_factories: Vec<FastApiFactoryFact>,
    pub dependency_type_aliases: Vec<FastApiAliasFact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FastApiAliasFact {
    pub name: String,
    pub router: FastApiRouterRef,
    #[serde(default)]
    pub definition_start_byte: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FastApiRouterRef {
    pub target_file_hint: Option<String>,
    pub name: String,
    pub factory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FastApiRouterFact {
    pub name: String,
    pub prefix: String,
    pub application: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FastApiFactoryFact {
    pub name: String,
    pub router: FastApiRouterRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FastApiMountFact {
    pub parent: FastApiRouterRef,
    pub child: FastApiRouterRef,
    pub prefix: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FastApiRouteFact {
    pub router: FastApiRouterRef,
    pub verb: String,
    pub path: String,
    pub handler_id: String,
    pub handler_name: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FastApiDependencyFact {
    pub owner_id: String,
    pub owner_name: String,
    pub dependency: FastApiRouterRef,
    pub line: usize,
    #[serde(default)]
    pub site_start_byte: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct UnresolvedReference {
    pub source_id: String,
    pub target_name: String,
    pub binding_name: String,
    pub target_file_hint: Option<String>,
    pub kind: RelationshipKind,
    pub provenance: String,
    pub confidence: f64,
    pub explanation: String,
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct FileFacts {
    pub path: String,
    pub content_hash: String,
    pub language: Language,
    pub symbols: Vec<Symbol>,
    pub relationships: Vec<Relationship>,
    pub unresolved_calls: Vec<UnresolvedCall>,
    pub callback_parameter_invocations: Vec<CallbackParameterInvocation>,
    pub callback_parameter_delegations: Vec<CallbackParameterDelegationFact>,
    pub callback_arguments: Vec<CallbackArgumentFact>,
    pub python_callback_formals: Vec<PythonCallbackFormalFact>,
    pub callable_returns: Vec<CallableReturnFact>,
    pub arkui_builder_flow: ArkuiBuilderFlowFacts,
    pub unresolved_references: Vec<UnresolvedReference>,
    pub dynamic_events: Vec<DynamicEventFact>,
    pub literal_bindings: Vec<LiteralBindingFact>,
    pub module_exports: Vec<ModuleExportFact>,
    pub c_function_pointers: CFunctionPointerFacts,
    pub fastapi: FastApiFacts,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_serializes_astro_sources() {
        assert_eq!(
            Language::from_path(Path::new("src/pages/Index.AsTrO")),
            Some(Language::Astro)
        );
        assert_eq!(
            Language::from_path(Path::new("src/module.mts")),
            Some(Language::TypeScript)
        );
        assert_eq!(
            Language::from_path(Path::new("src/module.cts")),
            Some(Language::TypeScript)
        );
        assert_eq!(Language::Astro.to_string(), "astro");
        assert_eq!(
            serde_json::to_string(&Language::Astro).unwrap(),
            "\"astro\""
        );
        assert_eq!(
            serde_json::from_str::<Language>("\"astro\"").unwrap(),
            Language::Astro
        );
    }

    #[test]
    fn stable_identity_ignores_source_position() {
        let first = Symbol::new(
            Language::TypeScript,
            SymbolKind::Function,
            "run",
            "run",
            "src/main.ts",
            SourceSpan {
                start_byte: 0,
                end_byte: 10,
                start_line: 1,
                end_line: 1,
            },
        );
        let moved = Symbol::new(
            Language::TypeScript,
            SymbolKind::Function,
            "run",
            "run",
            "src/main.ts",
            SourceSpan {
                start_byte: 100,
                end_byte: 130,
                start_line: 20,
                end_line: 25,
            },
        );
        assert_eq!(first.id, moved.id);
    }

    #[test]
    fn evidence_confidence_is_bounded() {
        assert_eq!(Evidence::new("test", 2.0, "x", "x.ts", 1).confidence, 1.0);
        assert_eq!(Evidence::new("test", -1.0, "x", "x.ts", 1).confidence, 0.0);
    }
}
