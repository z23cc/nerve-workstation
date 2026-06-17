#![no_main]

//! Mutation fuzzing for the multi-mode edit engine: arbitrary bytes are routed
//! into every edit mode against a small in-memory file set. The engine must
//! always return `Ok`/`Err` (never panic, overflow, or hang) regardless of input.

use nerve_core::edit::{self, EditRequest, PatchEntry, ReplaceEdit};
use libfuzzer_sys::fuzz_target;
use std::collections::BTreeMap;

fn reader() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("a.txt".to_string(), "one\ntwo\nthree\nfour\n".to_string());
    map.insert("src/lib.rs".to_string(), "fn main() {\n    work();\n}\n".to_string());
    map
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let mode = data[0] % 4;
    let rest = String::from_utf8_lossy(&data[1..]).into_owned();
    let reader = reader();

    let request = match mode {
        0 => EditRequest::Replace {
            path: "a.txt".to_string(),
            edits: vec![ReplaceEdit {
                old_text: rest,
                new_text: "REPLACED".to_string(),
                all: true,
            }],
        },
        1 => EditRequest::Patch {
            path: "a.txt".to_string(),
            entries: vec![PatchEntry::Update {
                rename: None,
                diff: rest,
            }],
        },
        2 => EditRequest::ApplyPatch { patch: rest },
        _ => EditRequest::Hashline { patch: rest },
    };

    let _ = edit::apply(&request, &reader);
});
