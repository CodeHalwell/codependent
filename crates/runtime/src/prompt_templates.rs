//! Prompt template slash commands (`.codypendent/commands/*.md`).
//!
//! Loads markdown prompt template files from:
//! - Global: `~/.codypendent/commands/*.md`
//! - Project: `<project_root>/.codypendent/commands/*.md`
//!
//! Each command file defines a slash command with optional frontmatter:
//! ```markdown
//! ---
//! description: Review a git diff
//! ---
//! Please review the following diff:
//! $ARGUMENTS
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A parsed user-defined prompt template command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub template: String,
    pub source_path: PathBuf,
}

impl PromptTemplate {
    /// Parse a markdown file into a prompt template.
    #[must_use]
    pub fn parse(name: String, content: &str, source_path: PathBuf) -> Self {
        let trimmed = content.trim();
        let (description, template) = if let Some(rest) = trimmed.strip_prefix("---") {
            if let Some(end) = rest.find("---") {
                let fm = &rest[..end];
                let body = rest[end + 3..].trim();
                let desc = fm
                    .lines()
                    .find_map(|line| {
                        let line = line.trim();
                        line.strip_prefix("description:")
                            .map(|val| val.trim().trim_matches('"').trim_matches('\'').to_string())
                    })
                    .unwrap_or_else(|| format!("Run /{name} command"));
                (desc, body.to_string())
            } else {
                (format!("Run /{name} command"), trimmed.to_string())
            }
        } else {
            (format!("Run /{name} command"), trimmed.to_string())
        };

        Self {
            name,
            description,
            template,
            source_path,
        }
    }

    /// Render the template by substituting `$ARGUMENTS`, `$@`, and positional
    /// `$1..$N`.
    ///
    /// ONE left-to-right pass over the template, because repeated
    /// `String::replace` calls are not a substitution: an ascending `$1`, `$2`, …
    /// sweep rewrites the `$1` *inside* `$10`, so `$10` silently became
    /// `<token1>0` and the later `$10` pass could no longer find it. The same
    /// bug corrupted the "clear the unset placeholders" sweep. Scanning once and
    /// taking the whole `$<digits>` run as a single placeholder makes `$1` and
    /// `$10` independent, and makes substituted argument text inert (a token
    /// containing `$2` is never re-expanded).
    ///
    /// A `$<digits>` placeholder with no matching token expands to the empty
    /// string, whatever its width — `$10` is cleared exactly like `$1`. `$0` is
    /// not a positional parameter and any other `$…` (including a bare `$`) is
    /// left verbatim.
    #[must_use]
    pub fn render(&self, args: &str) -> String {
        let trimmed_args = args.trim();
        let tokens: Vec<&str> = trimmed_args.split_whitespace().collect();

        let mut rendered = String::with_capacity(self.template.len());
        let mut rest = self.template.as_str();
        while let Some(at) = rest.find('$') {
            rendered.push_str(&rest[..at]);
            let after = &rest[at + 1..];

            if let Some(tail) = after.strip_prefix("ARGUMENTS") {
                rendered.push_str(trimmed_args);
                rest = tail;
                continue;
            }
            if let Some(tail) = after.strip_prefix('@') {
                rendered.push_str(trimmed_args);
                rest = tail;
                continue;
            }

            let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
            match after[..digits].parse::<usize>() {
                // `$1` is the first token; an index past the end clears.
                Ok(index) if index >= 1 => {
                    if let Some(token) = tokens.get(index - 1) {
                        rendered.push_str(token);
                    }
                }
                // `$0`, a bare `$`, `$foo`, or a digit run too large to be an
                // index: not a positional parameter, so keep it verbatim.
                _ => {
                    rendered.push('$');
                    rendered.push_str(&after[..digits]);
                }
            }
            rest = &after[digits..];
        }
        rendered.push_str(rest);

        rendered.trim().to_string()
    }
}

/// Discover all prompt template commands available for `cwd` and `home`.
#[must_use]
pub fn discover_commands(cwd: &Path, home: Option<&Path>) -> HashMap<String, PromptTemplate> {
    let mut map = HashMap::new();

    if let Some(home) = home {
        scan_dir(&home.join(".codypendent/commands"), &mut map);
    }

    let mut dirs = Vec::new();
    for dir in cwd.ancestors() {
        dirs.push(dir.to_path_buf());
        if dir.join(".git").exists() || dir.join(".codypendent").exists() {
            break;
        }
    }

    // Root to cwd order so more specific project commands overwrite global ones
    dirs.reverse();
    for dir in dirs {
        scan_dir(&dir.join(".codypendent/commands"), &mut map);
    }

    map
}

fn scan_dir(dir: &Path, out: &mut HashMap<String, PromptTemplate>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(stem) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(ToString::to_string)
                {
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let tmpl = PromptTemplate::parse(stem.clone(), &content, path);
                        out.insert(stem, tmpl);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_and_render_substitutions() {
        let content = r#"---
description: "Review a git diff or code block"
---
Please perform a rigorous review of:
$ARGUMENTS
Focus specifically on: $1"#;

        let tmpl = PromptTemplate::parse("review".into(), content, PathBuf::from("review.md"));
        assert_eq!(tmpl.name, "review");
        assert_eq!(tmpl.description, "Review a git diff or code block");

        let rendered = tmpl.render("security src/lib.rs");
        assert!(rendered.contains("Please perform a rigorous review of:\nsecurity src/lib.rs"));
        assert!(rendered.contains("Focus specifically on: security"));
    }

    /// PR #68 review: the ascending `$1`, `$2`, … replace sweep rewrote the `$1`
    /// inside `$10`, so `$10` rendered as `<token1>0` and every double-digit
    /// placeholder past it was unreachable. The unset-parameter sweep had the
    /// same bug, mangling `$12` into `$1`-plus-`2` leftovers instead of clearing
    /// it. One pass, one placeholder at a time.
    #[test]
    fn double_digit_positionals_are_not_corrupted_by_single_digit_ones() {
        let template = (1..=12)
            .map(|i| format!("p{i}=[${i}]"))
            .collect::<Vec<_>>()
            .join("\n");
        let tmpl = PromptTemplate::parse("pos".into(), &template, PathBuf::from("pos.md"));

        let args = (1..=12)
            .map(|i| format!("t{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let rendered = tmpl.render(&args);
        for i in 1..=12 {
            assert!(
                rendered.contains(&format!("p{i}=[t{i}]")),
                "placeholder ${i} rendered wrong:\n{rendered}"
            );
        }

        // Only three tokens supplied: every unset placeholder — single AND
        // double digit — is cleared, never mangled into a stray digit.
        let sparse = tmpl.render("t1 t2 t3");
        assert!(sparse.contains("p1=[t1]"), "{sparse}");
        assert!(sparse.contains("p3=[t3]"), "{sparse}");
        for i in 4..=12 {
            assert!(
                sparse.contains(&format!("p{i}=[]")),
                "unset ${i} was not cleared cleanly:\n{sparse}"
            );
        }
        assert!(!sparse.contains('$'), "no placeholder survives:\n{sparse}");
    }

    /// Substituted argument text is inert: a token that itself looks like a
    /// placeholder is never re-expanded by a later index, which the repeated
    /// `String::replace` loop could not guarantee.
    #[test]
    fn substituted_tokens_are_not_re_expanded() {
        let tmpl = PromptTemplate::parse(
            "echo".into(),
            "first=$1 second=$2 all=$ARGUMENTS",
            PathBuf::from("echo.md"),
        );
        let rendered = tmpl.render("$2 alpha");
        assert_eq!(rendered, "first=$2 second=alpha all=$2 alpha");
    }

    /// `$0` is not a positional parameter, and a `$` that starts no placeholder
    /// (`$HOME`, `$$`, a trailing `$`) is literal text.
    #[test]
    fn non_positional_dollars_survive_verbatim() {
        let tmpl = PromptTemplate::parse(
            "shell".into(),
            "echo $0 $HOME $$ $1 $",
            PathBuf::from("shell.md"),
        );
        assert_eq!(tmpl.render("x"), "echo $0 $HOME $$ x $");
        assert_eq!(tmpl.render(""), "echo $0 $HOME $$  $");
    }

    #[test]
    fn test_discover_commands() {
        let tmp = tempdir().unwrap();
        let cmd_dir = tmp.path().join(".codypendent/commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(
            cmd_dir.join("explain.md"),
            "---\ndescription: Explain code\n---\nExplain this: $ARGUMENTS",
        )
        .unwrap();

        let commands = discover_commands(tmp.path(), None);
        assert_eq!(commands.len(), 1);
        let explain = commands.get("explain").unwrap();
        assert_eq!(explain.description, "Explain code");
        assert_eq!(explain.render("fn main() {}"), "Explain this: fn main() {}");
    }
}
