//! Property and round-trip tests for the multi-mode edit engine.
//!
//! These nail correctness invariants the hand-written unit tests only sample:
//! - **panic-freedom**: arbitrary bytes fed to any mode return `Ok`/`Err`, never panic;
//! - **round-trip**: a `replace` (and a hashline `SWAP`) can be inverted to recover the original;
//! - **determinism**: applying the same request twice yields identical changes;
//! - **stale-hash safety**: a hashline patch with the wrong tag is refused, never applied;
//! - **view invariants**: `hashline_view` line numbering and tag match the file.

use nerve_core::edit::{
    self, EditRequest, FileChange, PatchEntry, ReplaceEdit, hashline_view, snapshot_tag,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";

fn single_file(path: &str, content: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(path.to_string(), content.to_string());
    map
}

/// Content built from distinct, single-occurrence lines plus a trailing newline.
/// `unique_lines(3)` -> "ln0_a\nln1_b\nln2_c\n" with each token appearing once.
fn unique_lines(tokens: &[String]) -> String {
    let mut out = String::new();
    for (idx, token) in tokens.iter().enumerate() {
        out.push_str(&format!("ln{idx}_{token}\n"));
    }
    out
}

/// A line token: 1-12 ascii letters, guaranteed non-empty, no whitespace/newlines.
fn line_token() -> impl Strategy<Value = String> {
    "[a-z]{1,12}"
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    /// Arbitrary bytes as an apply-patch / hashline envelope never panic the engine.
    #[test]
    fn arbitrary_envelopes_never_panic(patch in any::<String>(), content in any::<String>()) {
        let reader = single_file("a.txt", &content);
        let _ = edit::apply(&EditRequest::ApplyPatch { patch: patch.clone() }, &reader);
        let _ = edit::apply(&EditRequest::Hashline { patch: patch.clone() }, &reader);
        // Patch-mode diff body and a replace edit, both from arbitrary text.
        let _ = edit::apply(
            &EditRequest::Patch {
                path: "a.txt".to_string(),
                entries: vec![PatchEntry::Update { rename: None, diff: patch.clone() }],
            },
            &reader,
        );
        let _ = edit::apply(
            &EditRequest::Replace {
                path: "a.txt".to_string(),
                edits: vec![ReplaceEdit { old_text: patch, new_text: content, all: true }],
            },
            &reader,
        );
    }

    /// `replace` of a unique line is invertible: replacing it back recovers the file.
    #[test]
    fn replace_round_trips(
        tokens in prop::collection::vec(line_token(), 1..8),
        idx in 0usize..8,
        repl in line_token(),
    ) {
        let content = unique_lines(&tokens);
        let target_idx = idx % tokens.len();
        let old_text = format!("ln{target_idx}_{}", tokens[target_idx]);
        let new_text = format!("REPL_{repl}_{target_idx}"); // unique, absent from content
        prop_assume!(!content.contains(&new_text));

        let reader = single_file("f.txt", &content);
        let forward = edit::apply(
            &EditRequest::Replace {
                path: "f.txt".to_string(),
                edits: vec![ReplaceEdit { old_text: old_text.clone(), new_text: new_text.clone(), all: false }],
            },
            &reader,
        ).expect("forward replace");
        let FileChange::Update { content: after, .. } = &forward[0] else {
            panic!("expected Update");
        };
        prop_assert!(after.contains(&new_text));
        prop_assert!(!after.contains(&old_text));

        // Invert: replace new_text back to old_text on the produced file.
        let back_reader = single_file("f.txt", after);
        let back = edit::apply(
            &EditRequest::Replace {
                path: "f.txt".to_string(),
                edits: vec![ReplaceEdit { old_text: new_text, new_text: old_text, all: false }],
            },
            &back_reader,
        ).expect("inverse replace");
        let FileChange::Update { content: recovered, .. } = &back[0] else {
            panic!("expected Update");
        };
        prop_assert_eq!(recovered, &content);
    }

    /// Applying the same request twice is deterministic.
    #[test]
    fn replace_is_deterministic(
        tokens in prop::collection::vec(line_token(), 1..8),
        idx in 0usize..8,
    ) {
        let content = unique_lines(&tokens);
        let target_idx = idx % tokens.len();
        let old_text = format!("ln{target_idx}_{}", tokens[target_idx]);
        let reader = single_file("f.txt", &content);
        let req = EditRequest::Replace {
            path: "f.txt".to_string(),
            edits: vec![ReplaceEdit { old_text, new_text: "ZZZ".to_string(), all: false }],
        };
        let first = edit::apply(&req, &reader).expect("first");
        let second = edit::apply(&req, &reader).expect("second");
        prop_assert_eq!(first, second);
    }

    /// A hashline SWAP on a correctly-tagged file replaces exactly one line and
    /// is invertible back to the original.
    #[test]
    fn hashline_swap_round_trips(
        tokens in prop::collection::vec(line_token(), 1..8),
        idx in 0usize..8,
        repl in line_token(),
    ) {
        let content = unique_lines(&tokens);
        let n = tokens.len();
        let line_no = (idx % n) + 1; // 1-based, within real content lines
        let original_line = format!("ln{}_{}", line_no - 1, tokens[line_no - 1]);
        let new_line = format!("SWAPPED_{repl}_{line_no}");
        prop_assume!(!content.contains(&new_line));

        let tag = snapshot_tag(&content);
        let patch = format!("{BEGIN}\n[f.txt#{tag}]\nSWAP {line_no}.={line_no}:\n+{new_line}\n{END}\n");
        let reader = single_file("f.txt", &content);
        let changes = edit::apply(&EditRequest::Hashline { patch }, &reader).expect("swap");
        let FileChange::Update { content: after, .. } = &changes[0] else {
            panic!("expected Update");
        };
        prop_assert!(after.contains(&new_line));
        prop_assert!(!after.contains(&original_line));
        // Only the targeted line changed: line count is preserved.
        prop_assert_eq!(after.lines().count(), content.lines().count());

        // Invert with a fresh tag computed from the modified file.
        let back_tag = snapshot_tag(after);
        let back_patch =
            format!("{BEGIN}\n[f.txt#{back_tag}]\nSWAP {line_no}.={line_no}:\n+{original_line}\n{END}\n");
        let back_reader = single_file("f.txt", after);
        let back = edit::apply(&EditRequest::Hashline { patch: back_patch }, &back_reader).expect("inverse");
        let FileChange::Update { content: recovered, .. } = &back[0] else {
            panic!("expected Update");
        };
        prop_assert_eq!(recovered, &content);
    }

    /// A hashline patch carrying any tag other than the file's real tag is refused
    /// — the file is never silently mutated against a stale view.
    #[test]
    fn hashline_wrong_tag_is_refused(
        tokens in prop::collection::vec(line_token(), 1..8),
        bogus in "[0-9A-Fa-f]{16}",
    ) {
        let content = unique_lines(&tokens);
        let real = snapshot_tag(&content);
        prop_assume!(!bogus.eq_ignore_ascii_case(&real));
        let patch = format!("{BEGIN}\n[f.txt#{bogus}]\nSWAP 1.=1:\n+x\n{END}\n");
        let reader = single_file("f.txt", &content);
        let result = edit::apply(&EditRequest::Hashline { patch }, &reader);
        prop_assert!(result.is_err(), "stale tag must be refused, got {result:?}");
    }

    /// `hashline_view` numbers exactly the file's lines and embeds its real tag.
    #[test]
    fn hashline_view_is_consistent(tokens in prop::collection::vec(line_token(), 1..8)) {
        let content = unique_lines(&tokens);
        let view = hashline_view("f.txt", &content);
        let tag = snapshot_tag(&content);
        let expected_header = format!("[f.txt#{tag}]\n");
        prop_assert!(view.starts_with(&expected_header));
        // One numbered row per real line (trailing newline does not add a row).
        let numbered = view.lines().filter(|l| l.starts_with(|c: char| c.is_ascii_digit())).count();
        prop_assert_eq!(numbered, content.lines().count());
    }
}
