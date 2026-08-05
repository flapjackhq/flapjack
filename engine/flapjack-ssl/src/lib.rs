pub mod acme;
pub mod config;
pub mod error;
pub mod manager;

use once_cell::sync::OnceCell;
use std::sync::Arc;

pub use acme::{install_default_crypto_provider, AcmeClient};
pub use config::{SslConfig, SslMaterialConfig};
pub use error::{FlapjackError, Result};
pub use manager::SslManager;

static GLOBAL_SSL_MANAGER: OnceCell<Arc<manager::SslManager>> = OnceCell::new();

pub fn set_global_manager(manager: Arc<manager::SslManager>) {
    let _ = GLOBAL_SSL_MANAGER.set(manager);
}

pub fn get_global_manager() -> Option<Arc<manager::SslManager>> {
    GLOBAL_SSL_MANAGER.get().map(Arc::clone)
}

#[cfg(test)]
pub(crate) mod source_scan {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Match {
        pub(crate) line: usize,
        pub(crate) word: String,
    }

    pub(crate) fn production_code(source: &str) -> String {
        let syntax = sanitize_source(source, false);
        apply_line_mask(&syntax, &production_line_mask(&syntax))
    }

    /// Same production slice as [`production_code`], but string and char literals
    /// are preserved instead of blanked. Callers that need to assert on a literal
    /// value inside production code (e.g. a hard-coded destination path) must use
    /// this; `production_code` blanks every literal, which silently makes any
    /// "does not contain `\"...\"`" assertion vacuously true.
    pub(crate) fn production_code_with_literals(source: &str) -> String {
        let mask = production_line_mask(&sanitize_source(source, false));
        apply_line_mask(&sanitize_source(source, true), &mask)
    }

    pub(crate) fn find_words(source: &str, words: &[&str]) -> Vec<Match> {
        let syntax = sanitize_source(source, false);
        let production_lines = production_line_mask(&syntax);
        let production = apply_line_mask(&syntax, &production_lines);
        let mut matches: Vec<Match> = production
            .lines()
            .enumerate()
            .flat_map(|(line_index, line)| {
                let lowered = line.to_lowercase();
                words
                    .iter()
                    .filter(move |word| lowered.contains(**word))
                    .map(move |word| Match {
                        line: line_index + 1,
                        word: (*word).to_string(),
                    })
            })
            .collect();

        let source_with_literals = sanitize_source(source, true);
        let production_with_literals = apply_line_mask(&source_with_literals, &production_lines);
        append_process_command_literal_matches(
            &production,
            &production_with_literals,
            words,
            &mut matches,
        );
        matches.sort_by(|left, right| {
            left.line
                .cmp(&right.line)
                .then_with(|| left.word.cmp(&right.word))
        });
        matches.dedup();
        matches
    }

    fn sanitize_source(source: &str, preserve_literals: bool) -> String {
        let chars: Vec<char> = source.chars().collect();
        let mut output = String::with_capacity(source.len());
        let mut index = 0;
        while index < chars.len() {
            let current = chars[index];
            let next = chars.get(index + 1).copied();
            if current == '/' && next == Some('/') {
                index = copy_line_comment(&chars, index, &mut output);
            } else if current == '/' && next == Some('*') {
                index = copy_block_comment(&chars, index, &mut output);
            } else if current == '"' {
                index = copy_quoted_literal(&chars, index, &mut output, preserve_literals);
            } else if current == '\'' && chars.get(index + 2).copied() == Some('\'') {
                copy_or_blank(&chars[index..index + 3], &mut output, preserve_literals);
                index += 3;
            } else if current == 'r' && raw_string_hashes(&chars, index).is_some() {
                index = copy_raw_string_literal(&chars, index, &mut output, preserve_literals);
            } else {
                output.push(current);
                index += 1;
            }
        }
        output
    }

    fn copy_line_comment(chars: &[char], mut index: usize, output: &mut String) -> usize {
        while index < chars.len() {
            let current = chars[index];
            if current == '\n' {
                output.push('\n');
                return index + 1;
            }
            output.push(' ');
            index += 1;
        }
        index
    }

    fn copy_block_comment(chars: &[char], mut index: usize, output: &mut String) -> usize {
        let mut depth = 0usize;
        while index < chars.len() {
            let current = chars[index];
            let next = chars.get(index + 1).copied();
            if current == '/' && next == Some('*') {
                depth += 1;
                output.push_str("  ");
                index += 2;
            } else if current == '*' && next == Some('/') {
                depth = depth.saturating_sub(1);
                output.push_str("  ");
                index += 2;
                if depth == 0 {
                    return index;
                }
            } else {
                output.push(if current == '\n' { '\n' } else { ' ' });
                index += 1;
            }
        }
        index
    }

    fn copy_quoted_literal(
        chars: &[char],
        mut index: usize,
        output: &mut String,
        preserve_literals: bool,
    ) -> usize {
        push_or_blank(chars[index], output, preserve_literals);
        index += 1;
        while index < chars.len() {
            let current = chars[index];
            push_or_blank(current, output, preserve_literals);
            index += 1;
            if current == '\\' && index < chars.len() {
                push_or_blank(chars[index], output, preserve_literals);
                index += 1;
            } else if current == '"' {
                return index;
            }
        }
        index
    }

    fn raw_string_hashes(chars: &[char], index: usize) -> Option<usize> {
        let mut cursor = index + 1;
        let mut hashes = 0usize;
        while chars.get(cursor).copied() == Some('#') {
            hashes += 1;
            cursor += 1;
        }
        (chars.get(cursor).copied() == Some('"')).then_some(hashes)
    }

    fn copy_raw_string_literal(
        chars: &[char],
        index: usize,
        output: &mut String,
        preserve_literals: bool,
    ) -> usize {
        let hashes = raw_string_hashes(chars, index).expect("caller checked raw string prefix");
        let mut cursor = index;
        for _ in 0..(hashes + 2) {
            push_or_blank(chars[cursor], output, preserve_literals);
            cursor += 1;
        }
        while cursor < chars.len() {
            let current = chars[cursor];
            push_or_blank(current, output, preserve_literals);
            cursor += 1;
            if current == '"' && raw_string_closes(chars, cursor, hashes) {
                for _ in 0..hashes {
                    push_or_blank(chars[cursor], output, preserve_literals);
                    cursor += 1;
                }
                return cursor;
            }
        }
        cursor
    }

    fn raw_string_closes(chars: &[char], cursor: usize, hashes: usize) -> bool {
        (0..hashes).all(|offset| chars.get(cursor + offset).copied() == Some('#'))
    }

    fn push_or_blank(character: char, output: &mut String, preserve: bool) {
        output.push(if preserve || character == '\n' {
            character
        } else {
            ' '
        });
    }

    fn copy_or_blank(characters: &[char], output: &mut String, preserve: bool) {
        for &character in characters {
            push_or_blank(character, output, preserve);
        }
    }

    fn production_line_mask(source: &str) -> Vec<bool> {
        let mut mask = Vec::with_capacity(source.lines().count());
        let mut pending_test_cfg = false;
        let mut skip_depth: Option<usize> = None;

        for line in source.lines() {
            if let Some(depth) = skip_depth {
                let next_depth = apply_brace_delta(depth, line);
                if next_depth == 0 {
                    skip_depth = None;
                } else {
                    skip_depth = Some(next_depth);
                }
                mask.push(false);
                continue;
            }

            let trimmed = line.trim();
            if trimmed.contains("#[cfg(test)]") {
                pending_test_cfg = true;
                mask.push(false);
                continue;
            }

            if pending_test_cfg {
                mask.push(false);
                if trimmed.is_empty() || trimmed.starts_with("#[") {
                    continue;
                }

                let depth = apply_brace_delta(0, line);
                if depth > 0 {
                    skip_depth = Some(depth);
                }
                pending_test_cfg = false;
                continue;
            }

            mask.push(true);
        }

        mask
    }

    fn apply_line_mask(source: &str, production_lines: &[bool]) -> String {
        let mut output = String::with_capacity(source.len());
        for (line, is_production) in source.lines().zip(production_lines) {
            if *is_production {
                output.push_str(line);
            }
            output.push('\n');
        }
        output
    }

    fn apply_brace_delta(mut depth: usize, line: &str) -> usize {
        for character in line.chars() {
            match character {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
        depth
    }

    /// The forbidden behavior is shelling out to an external program, whose name
    /// lives in a string literal consumed by a `std::process::Command`. The local
    /// constructor may be aliased (`use std::process::Command as Runner;`), so key
    /// detection off every alias of `std::process::Command` learned from the
    /// source rather than one hard-coded spelling. `"Command::new"` is always
    /// included to cover the direct and fully-qualified (`std::process::Command`)
    /// forms.
    fn command_constructor_triggers(production_syntax: &str) -> Vec<String> {
        let mut triggers = vec!["Command::new".to_string()];
        let needle = "std::process::Command as ";
        let mut cursor = 0;
        while let Some(found) = production_syntax[cursor..].find(needle) {
            let alias_start = cursor + found + needle.len();
            let alias: String = production_syntax[alias_start..]
                .chars()
                .take_while(|character| character.is_alphanumeric() || *character == '_')
                .collect();
            if !alias.is_empty() {
                triggers.push(format!("{alias}::new"));
            }
            cursor = alias_start;
        }
        triggers
    }

    fn append_process_command_literal_matches(
        production_syntax: &str,
        production_with_literals: &str,
        words: &[&str],
        matches: &mut Vec<Match>,
    ) {
        let triggers = command_constructor_triggers(production_syntax);
        let mut inside_command_invocation = false;
        for (line_index, (syntax_line, source_line)) in production_syntax
            .lines()
            .zip(production_with_literals.lines())
            .enumerate()
        {
            if triggers.iter().any(|trigger| syntax_line.contains(trigger)) {
                inside_command_invocation = true;
            }
            if inside_command_invocation {
                let lowered = source_line.to_lowercase();
                for word in words {
                    if lowered.contains(*word) {
                        matches.push(Match {
                            line: line_index + 1,
                            word: (*word).to_string(),
                        });
                    }
                }
            }
            if inside_command_invocation && syntax_line.contains(';') {
                inside_command_invocation = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::source_scan;
    use std::fs;
    use std::path::Path;

    const PRODUCTION_SSL_SOURCE_FILES: [&str; 6] = [
        "acme.rs",
        "config.rs",
        "error.rs",
        "lib.rs",
        "manager.rs",
        "mod.rs",
    ];

    #[test]
    fn production_ssl_source_contains_no_nginx_reload_assumption() {
        let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut matches = Vec::new();

        for file_name in PRODUCTION_SSL_SOURCE_FILES {
            let source = fs::read_to_string(source_dir.join(file_name))
                .unwrap_or_else(|error| panic!("failed to read {file_name}: {error}"));
            for source_match in source_scan::find_words(&source, &["systemctl", "nginx"]) {
                matches.push(format!(
                    "{file_name}:{}:{}",
                    source_match.line, source_match.word
                ));
            }
        }

        println!("files_swept={}", PRODUCTION_SSL_SOURCE_FILES.len());
        println!("matches_found={}", matches.len());
        assert!(
            matches.is_empty(),
            "production SSL source must not assume an external nginx reload: {matches:?}"
        );
    }

    #[test]
    fn source_scan_ignores_harmless_prose_and_literals() {
        let source = r##"
            //! doc comment mentions nginx and systemctl
            /* block comment mentions nginx */
            fn harmless() {
                let text = "nginx inside a string";
                let raw_plain = r"nginx inside a raw string";
                let raw = r#"systemctl inside a raw string"#;
                let url = "https://example.test//not_a_comment";
            }
        "##;

        assert_eq!(source_scan::find_words(source, &["systemctl", "nginx"]), []);
    }

    #[test]
    fn source_scan_skips_test_items_without_hiding_later_production_code() {
        let source = r#"
            #[cfg(test)]
            mod tests {
                fn test_only() {
                    let _ = "nginx";
                    systemctl_reload();
                }
            }

            fn production_after_tests() {
                systemctl_reload();
            }
        "#;

        assert_eq!(
            source_scan::find_words(source, &["systemctl", "nginx"]),
            [source_scan::Match {
                line: 11,
                word: "systemctl".to_string(),
            }]
        );
    }

    #[test]
    fn source_scan_does_not_let_line_comment_markers_inside_strings_hide_code() {
        let source = r#"
            fn production() {
                let url = "https://example.test//still_string"; nginx_reload();
            }
        "#;

        assert_eq!(
            source_scan::find_words(source, &["systemctl", "nginx"]),
            [source_scan::Match {
                line: 3,
                word: "nginx".to_string(),
            }]
        );
    }

    #[test]
    fn source_scan_detects_forbidden_process_command_literals() {
        let source = r#"
            fn refresh_material() {
                let outcome = std::process::Command::new("systemctl")
                    .args(["reload", "nginx"])
                    .output();
            }
        "#;

        assert_eq!(
            source_scan::find_words(source, &["systemctl", "nginx"]),
            [
                source_scan::Match {
                    line: 3,
                    word: "systemctl".to_string(),
                },
                source_scan::Match {
                    line: 4,
                    word: "nginx".to_string(),
                },
            ]
        );
    }

    #[test]
    fn source_scan_detects_aliased_process_command_literals() {
        // An alias whose spelling does not contain "Command" must not evade the
        // guard: detection follows std::process::Command through its local alias,
        // not one hard-coded constructor spelling. Function and variable names are
        // deliberately neutral so the forbidden behavior lives only in the literals.
        let source = r#"
            use std::process::Command as Runner;
            fn refresh_material() {
                let outcome = Runner::new("systemctl")
                    .args(["reload", "nginx"])
                    .output();
            }
        "#;

        assert_eq!(
            source_scan::find_words(source, &["systemctl", "nginx"]),
            [
                source_scan::Match {
                    line: 4,
                    word: "systemctl".to_string(),
                },
                source_scan::Match {
                    line: 5,
                    word: "nginx".to_string(),
                },
            ]
        );
    }
}
