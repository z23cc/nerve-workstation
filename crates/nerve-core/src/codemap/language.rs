use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Php,
}

/// Extra tags-query patterns for containers the bundled Java/PHP grammar queries
/// omit, so they surface as codemap symbols (and gain member expansion).
pub(super) const JAVA_EXTRA_TAGS: &str = concat!(
    "(enum_declaration name: (identifier) @name) @definition.class\n",
    "(record_declaration name: (identifier) @name) @definition.class"
);
pub(super) const PHP_EXTRA_TAGS: &str = "(enum_declaration name: (name) @name) @definition.class";

/// The bundled tree-sitter-c-sharp tags query does not compile against this
/// grammar version, so we supply a minimal working one.
pub(super) const CSHARP_TAGS: &str = concat!(
    "(class_declaration name: (identifier) @name) @definition.class\n",
    "(struct_declaration name: (identifier) @name) @definition.class\n",
    "(interface_declaration name: (identifier) @name) @definition.interface\n",
    "(enum_declaration name: (identifier) @name) @definition.class\n",
    "(record_declaration name: (identifier) @name) @definition.class\n",
    "(method_declaration name: (identifier) @name) @definition.method\n",
    "(property_declaration name: (identifier) @name) @definition.method"
);

impl Language {
    pub(super) fn from_path(path: &str) -> Option<Self> {
        let lower = path.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        Some(match ext {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            "ts" | "mts" | "cts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "go" => Self::Go,
            "java" => Self::Java,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Self::Cpp,
            "cs" => Self::CSharp,
            "rb" => Self::Ruby,
            "php" | "phtml" => Self::Php,
            _ => return None,
        })
    }

    /// Display language tag for the response `language` field. Repo-map keeps
    /// JS/TS/TSX in one resolution family separately (see repomap language_family).
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Tsx => "tsx",
            Self::Go => "go",
            Self::Java => "java",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::CSharp => "csharp",
            Self::Ruby => "ruby",
            Self::Php => "php",
        }
    }

    /// Resolve a language by its display name (the inverse of `name`).
    pub(super) fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "rust" => Self::Rust,
            "python" => Self::Python,
            "javascript" => Self::JavaScript,
            "typescript" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "go" => Self::Go,
            "java" => Self::Java,
            "c" => Self::C,
            "cpp" => Self::Cpp,
            "csharp" => Self::CSharp,
            "ruby" => Self::Ruby,
            "php" => Self::Php,
            _ => return None,
        })
    }

    /// Raw grammar handle for a second parse used by member extraction (the
    /// tags API does not expose its tree).
    pub(super) fn ts_language(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            Self::Java => tree_sitter_java::LANGUAGE.into(),
            Self::C => tree_sitter_c::LANGUAGE.into(),
            Self::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Self::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Self::Ruby => tree_sitter_ruby::LANGUAGE.into(),
            Self::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        }
    }

    /// Node kinds that define a type whose members we expand. Empty disables
    /// member extraction for the language.
    pub(super) fn member_containers(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["struct_item", "enum_item"],
            Self::Python => &["class_definition"],
            Self::JavaScript => &["class_declaration"],
            Self::TypeScript | Self::Tsx => &["class_declaration", "interface_declaration"],
            Self::Go => &["type_spec"],
            Self::Java => &[
                "class_declaration",
                "interface_declaration",
                "enum_declaration",
                "record_declaration",
            ],
            Self::C => &["struct_specifier"],
            Self::Cpp => &["class_specifier", "struct_specifier"],
            Self::CSharp => &[
                "class_declaration",
                "struct_declaration",
                "interface_declaration",
                "record_declaration",
                "enum_declaration",
            ],
            Self::Php => &[
                "class_declaration",
                "interface_declaration",
                "trait_declaration",
                "enum_declaration",
            ],
            Self::Ruby => &["class", "module"],
        }
    }

    /// Node kinds of member declarations inside a container's body list.
    pub(super) fn member_field_kinds(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["field_declaration", "enum_variant"],
            Self::Python => &["expression_statement"],
            Self::JavaScript => &["field_definition"],
            Self::TypeScript | Self::Tsx => &["property_signature", "public_field_definition"],
            Self::Go => &["field_declaration"],
            Self::Java => &[
                "field_declaration",
                "constant_declaration",
                "enum_constant",
                "formal_parameter",
            ],
            Self::C => &["field_declaration"],
            Self::Cpp => &["field_declaration"],
            Self::CSharp => &[
                "field_declaration",
                "property_declaration",
                "enum_member_declaration",
                "parameter",
            ],
            Self::Php => &["property_declaration", "const_declaration", "enum_case"],
            Self::Ruby => &[],
        }
    }

    /// Node kinds carrying a member's name; the first match in document order
    /// wins. Python resolves its name separately (assignment target).
    pub(super) fn member_name_kinds(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["field_identifier", "identifier"],
            Self::JavaScript => &["property_identifier", "private_property_identifier"],
            Self::TypeScript | Self::Tsx => &["property_identifier"],
            Self::Go => &["field_identifier"],
            Self::Java => &["identifier"],
            Self::C | Self::Cpp => &["field_identifier"],
            Self::CSharp => &["identifier"],
            Self::Php => &["variable_name", "name"],
            Self::Python | Self::Ruby => &[],
        }
    }

    /// Cached tags configuration (compiling a query is expensive).
    pub(super) fn config(self) -> Option<&'static TagsConfiguration> {
        fn build(language: tree_sitter::Language, query: &str) -> Option<TagsConfiguration> {
            TagsConfiguration::new(language, query, "").ok()
        }
        macro_rules! cached {
            ($lang:expr, $query:expr) => {{
                static CELL: OnceLock<Option<TagsConfiguration>> = OnceLock::new();
                CELL.get_or_init(|| build($lang.into(), $query)).as_ref()
            }};
        }
        match self {
            Self::Rust => cached!(tree_sitter_rust::LANGUAGE, tree_sitter_rust::TAGS_QUERY),
            Self::Python => cached!(tree_sitter_python::LANGUAGE, tree_sitter_python::TAGS_QUERY),
            Self::JavaScript => {
                cached!(
                    tree_sitter_javascript::LANGUAGE,
                    tree_sitter_javascript::TAGS_QUERY
                )
            }
            // TypeScript's tags.scm only adds TS-specific captures and inherits the
            // rest from JavaScript; concatenate both so class/function decls are seen.
            Self::TypeScript => cached!(
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
                &format!(
                    "{}\n{}",
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                )
            ),
            Self::Tsx => cached!(
                tree_sitter_typescript::LANGUAGE_TSX,
                &format!(
                    "{}\n{}",
                    tree_sitter_javascript::TAGS_QUERY,
                    tree_sitter_typescript::TAGS_QUERY
                )
            ),
            Self::Go => cached!(tree_sitter_go::LANGUAGE, tree_sitter_go::TAGS_QUERY),
            Self::Java => cached!(
                tree_sitter_java::LANGUAGE,
                &format!("{}\n{}", tree_sitter_java::TAGS_QUERY, JAVA_EXTRA_TAGS)
            ),
            Self::C => cached!(tree_sitter_c::LANGUAGE, tree_sitter_c::TAGS_QUERY),
            Self::Cpp => cached!(tree_sitter_cpp::LANGUAGE, tree_sitter_cpp::TAGS_QUERY),
            // The bundled c-sharp tags query fails to compile here; use ours.
            Self::CSharp => cached!(tree_sitter_c_sharp::LANGUAGE, CSHARP_TAGS),
            Self::Ruby => cached!(tree_sitter_ruby::LANGUAGE, tree_sitter_ruby::TAGS_QUERY),
            Self::Php => cached!(
                tree_sitter_php::LANGUAGE_PHP,
                &format!("{}\n{}", tree_sitter_php::TAGS_QUERY, PHP_EXTRA_TAGS)
            ),
        }
    }
}
