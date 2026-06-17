use std::collections::BTreeMap;

use super::analysis::IndexedFile;

/// Resolution family for cross-file reference edges. JS/TS/TSX share one family
/// so references resolve across `.js`/`.ts`/`.tsx` even though each file's
/// displayed `language` differs. Other languages map to themselves.
pub(super) fn language_family(language: &str) -> &str {
    match language {
        "typescript" | "tsx" => "javascript",
        other => other,
    }
}

pub(super) fn language_file_counts(files: &[IndexedFile]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for file in files {
        *counts
            .entry(language_family(&file.language).to_string())
            .or_insert(0) += 1;
    }
    counts
}

pub(super) fn is_reference_stopword(identifier: &str, language: &str) -> bool {
    !is_identifier(identifier)
        || identifier.len() < 3
        || language_keywords(language).contains(&identifier)
}

pub(super) fn is_high_document_frequency(definer_count: usize, language_file_count: usize) -> bool {
    const HIGH_DF_MIN_FILES: usize = 4;
    const HIGH_DF_MAX_NUMERATOR: usize = 1;
    const HIGH_DF_MAX_DENOMINATOR: usize = 4;

    definer_count >= HIGH_DF_MIN_FILES
        && definer_count * HIGH_DF_MAX_DENOMINATOR > language_file_count * HIGH_DF_MAX_NUMERATOR
}

fn language_keywords(language: &str) -> &'static [&'static str] {
    match language {
        "rust" => &RUST_STOPWORDS,
        "python" => &PYTHON_STOPWORDS,
        "javascript" => &JAVASCRIPT_STOPWORDS,
        "go" => GO_STOPWORDS,
        _ => &[],
    }
}

const GO_STOPWORDS: &[&str] = &[
    // keywords
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
    // builtins
    "append",
    "cap",
    "clear",
    "close",
    "complex",
    "copy",
    "delete",
    "imag",
    "len",
    "make",
    "max",
    "min",
    "new",
    "panic",
    "print",
    "println",
    "real",
    "recover",
    // predeclared types
    "any",
    "bool",
    "byte",
    "comparable",
    "complex64",
    "complex128",
    "error",
    "float32",
    "float64",
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "rune",
    "string",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    // common constants / pervasive identifiers
    "false",
    "true",
    "iota",
    "nil",
    "err",
    "ctx",
    "ok",
];

const RUST_STOPWORDS: [&str; 56] = [
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "unsafe", "unsized", "use", "virtual", "where", "while", "yield", "Result", "Option", "Some",
    "None", "Ok",
];

const PYTHON_STOPWORDS: [&str; 37] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try",
    "while", "with", "yield", "print",
];

const JAVASCRIPT_STOPWORDS: [&str; 47] = [
    "arguments",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "null",
    "of",
    "return",
    "set",
    "static",
    "super",
    "switch",
    "target",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

fn is_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    is_identifier_start(first) && bytes.all(is_identifier_continue)
}

fn is_identifier_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_identifier_continue(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}
