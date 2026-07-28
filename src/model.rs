use serde::{Deserialize, Serialize};
use std::{fmt, path::Path};

pub const GRAPH_MODEL_VERSION: u32 = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    #[serde(rename = "typescript")]
    TypeScript,
    Tsx,
    #[serde(rename = "javascript")]
    JavaScript,
    Jsx,
    Python,
    Rust,
    Go,
    Java,
    #[serde(rename = "csharp")]
    CSharp,
    C,
    Cpp,
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
            "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "mjs" | "cjs" => Some(Self::JavaScript),
            "jsx" => Some(Self::Jsx),
            "py" | "pyi" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            "cs" => Some(Self::CSharp),
            "c" | "h" => Some(Self::C),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some(Self::Cpp),
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
            Self::Python => "python",
            Self::Rust => "rust",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::C => "c",
            Self::Cpp => "cpp",
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
    Imports,
    Extends,
    Implements,
}

impl fmt::Display for RelationshipKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Contains => "contains",
            Self::Calls => "calls",
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
        }
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
    pub callee_name: String,
    pub receiver_type: Option<String>,
    pub provenance: String,
    pub confidence: f64,
    pub explanation: String,
    pub file: String,
    pub line: usize,
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
    pub unresolved_references: Vec<UnresolvedReference>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
