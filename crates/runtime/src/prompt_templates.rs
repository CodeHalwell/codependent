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

    /// Render the template by substituting `$ARGUMENTS`, `$@`, and positional `$1..$N`.
    #[must_use]
    pub fn render(&self, args: &str) -> String {
        let trimmed_args = args.trim();
        let tokens: Vec<&str> = trimmed_args.split_whitespace().collect();

        let mut rendered = self.template.clone();
        rendered = rendered.replace("$ARGUMENTS", trimmed_args);
        rendered = rendered.replace("$@", trimmed_args);

        for (i, token) in tokens.iter().enumerate() {
            rendered = rendered.replace(&format!("${}", i + 1), token);
        }

        // Remove any remaining unset positional parameters
        for i in 1..=30 {
            rendered = rendered.replace(&format!("${i}"), "");
        }

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
