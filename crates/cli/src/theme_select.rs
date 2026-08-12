//! Live-TUI theme selection wiring (STEP 6.6).
//!
//! `codypendent-tui` ships the pure decision logic — six accessibility
//! variants, [`ColorDepth::detect`] (`NO_COLOR`/`COLORTERM`/`TERM`),
//! [`Theme::select`] (manual override always wins), and the data-only
//! theme-pack loader ([`load_theme_pack`], which structurally rejects any
//! pack declaring capabilities/permissions) — but nothing in the live TUI
//! ever called it: `tui::run` hardcoded `Theme::dark()`. This module is the
//! seam that calls the real API with real inputs (an optional `--theme`
//! name, resolved against the terminal's detected color depth or an on-disk
//! theme pack).
//!
//! There is no general user-facing config file yet (the only precedent is
//! `SessionStore` in `tui.rs`, which is an internal resume-token cache, not
//! user preferences), so the override surface is `--theme <NAME>` /
//! `CODYPENDENT_THEME`, following the existing `CODYPENDENT_DATA_DIR` /
//! `CODYPENDENT_SOCKET` env-var and `--repo`/`--mode`-style flag
//! conventions (see `main.rs`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use codypendent_protocol::discovery::RuntimePaths;
use codypendent_tui::{load_theme_pack, ColorDepth, Theme, ThemePreferences, ThemeVariant};

/// Merge the `--theme` flag with the `CODYPENDENT_THEME` env var into the
/// single override name [`resolve_theme`]/[`resolve_theme_for_depth`] take.
///
/// The flag wins, but an empty/whitespace-only value from EITHER source is
/// treated as absent and falls through to the other — `--theme ""` falls
/// through to `CODYPENDENT_THEME`, and an empty/unset env var falls through
/// to no override, never the other way around. This mirrors `RuntimePaths`'
/// own `non_empty_env` convention (an empty override is a misconfiguration,
/// not a real value), and is why each source is filtered *before* combining:
/// `Option::or_else` only runs its closure when the receiver is `None`, so
/// filtering only *after* combining would let a present-but-empty flag
/// short-circuit past a real env var.
///
/// Pure and independent of `std::env`, so the precedence is directly
/// testable without mutating the process environment.
pub fn resolve_theme_override(flag: Option<String>, env: Option<String>) -> Option<String> {
    fn non_empty(value: Option<String>) -> Option<String> {
        value.filter(|v| !v.trim().is_empty())
    }
    non_empty(flag).or_else(|| non_empty(env))
}

/// Data-only theme packs (STEP 6.6) load from `<data-dir>/themes/<id>.toml` —
/// the existing data-dir convention (see the module docs on
/// `RuntimePaths`), alongside the CLI's other ad hoc data-dir paths (e.g.
/// `SessionStore::file` in `tui.rs`) rather than a new `RuntimePaths` field,
/// since this is the only caller of an optional, read-only lookup.
fn theme_pack_path(paths: &RuntimePaths, id: &str) -> PathBuf {
    paths.data_dir.join("themes").join(format!("{id}.toml"))
}

/// Match a `--theme`/`CODYPENDENT_THEME` value against a built-in variant
/// name, case- and separator-insensitively (`High-Contrast`, `high_contrast`,
/// and `HIGHCONTRAST` all resolve to the same variant).
fn parse_builtin_variant(name: &str) -> Option<ThemeVariant> {
    let normalized = name.to_ascii_lowercase().replace(['-', '_'], "");
    match normalized.as_str() {
        "dark" => Some(ThemeVariant::Dark),
        "light" => Some(ThemeVariant::Light),
        "highcontrast" => Some(ThemeVariant::HighContrast),
        "colorblindsafe" => Some(ThemeVariant::ColorBlindSafe),
        "ansi256" => Some(ThemeVariant::Ansi256),
        "ansi16" => Some(ThemeVariant::Ansi16),
        "monochrome" | "mono" => Some(ThemeVariant::Monochrome),
        _ => None,
    }
}

/// Load a named theme pack from `<data-dir>/themes/<id>.toml`.
fn load_pack(paths: &RuntimePaths, id: &str) -> Result<Theme> {
    let path = theme_pack_path(paths, id);
    let toml_str = std::fs::read_to_string(&path).with_context(|| {
        let path_display = path.display();
        format!(
            "theme `{id}` is not a built-in variant (dark, light, high-contrast, \
             color-blind-safe, ansi256, ansi16, monochrome) and no theme pack was \
             found at {path_display}"
        )
    })?;
    let path_display = path.display();
    load_theme_pack(&toml_str)
        .map_err(|e| anyhow::anyhow!("theme pack `{id}` at {path_display}: {e}"))
}

/// Resolve the live TUI's theme for an explicit terminal color `depth` and an
/// optional override name. Split from [`resolve_theme`] so tests can supply
/// `depth` directly instead of depending on the process environment —
/// `ColorDepth::detect`'s own env-parsing rules are already covered by
/// `codypendent-tui`'s own tests; what is under test *here* is the wiring:
/// given a depth and an override, does the live TUI construction path pick
/// the theme it should.
///
/// Precedence (matches `Theme::select`'s "manual override always wins"
/// contract): an override name, when given, always wins over `depth`. A name
/// matching a built-in variant selects that variant outright; any other name
/// is looked up as a theme-pack id under `<data-dir>/themes/<name>.toml` and
/// loaded via `codypendent_tui::load_theme_pack`. With no override, `depth`
/// alone picks the built-in variant.
pub fn resolve_theme_for_depth(
    paths: &RuntimePaths,
    depth: ColorDepth,
    override_name: Option<&str>,
) -> Result<Theme> {
    let Some(name) = override_name else {
        return Ok(Theme::select(depth, ThemePreferences::default()));
    };
    if let Some(variant) = parse_builtin_variant(name) {
        return Ok(Theme::select(
            depth,
            ThemePreferences {
                override_variant: Some(variant),
                ..ThemePreferences::default()
            },
        ));
    }
    load_pack(paths, name)
}

/// The live entry point: detect the terminal's real color depth
/// ([`ColorDepth::detect`], honoring `NO_COLOR`/`COLORTERM`/`TERM`) and
/// resolve the theme for it (see [`resolve_theme_for_depth`]).
///
/// `remembered` is the id the TUI's `/theme` picker last kept (persisted
/// beside the session store). It sits BELOW an explicit
/// `--theme`/`CODYPENDENT_THEME` — an explicit override always wins — and
/// above terminal detection. Unlike an override, a remembered id that no
/// longer resolves (a deleted pack) is not an error: it falls through to
/// detection, so a stale preference can never wedge the TUI shut.
pub fn resolve_theme(
    paths: &RuntimePaths,
    override_name: Option<&str>,
    remembered: Option<&str>,
) -> Result<Theme> {
    let depth = ColorDepth::detect();
    if override_name.is_some() {
        return resolve_theme_for_depth(paths, depth, override_name);
    }
    if let Some(name) = remembered.filter(|name| !name.trim().is_empty()) {
        if let Ok(theme) = resolve_theme_for_depth(paths, depth, Some(name)) {
            return Ok(theme);
        }
    }
    resolve_theme_for_depth(paths, depth, None)
}

/// Every data-only theme pack installed under `<data-dir>/themes/*.toml`, as
/// `(id, theme)` pairs sorted by id, skipping any file that fails to parse (a
/// broken pack must not take the picker down with it).
///
/// The TUI crate performs no I/O, so the picker's rows have to arrive already
/// parsed — this is the seam that reads them.
#[must_use]
pub fn discover_theme_packs(paths: &RuntimePaths) -> Vec<(String, Theme)> {
    let Ok(entries) = std::fs::read_dir(paths.data_dir.join("themes")) else {
        return Vec::new();
    };
    let mut packs: Vec<(String, Theme)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_stem()?.to_str()?.to_owned();
            let source = std::fs::read_to_string(&path).ok()?;
            let theme = load_theme_pack(&source).ok()?;
            Some((id, theme))
        })
        .collect();
    packs.sort_by(|a, b| a.0.cmp(&b.0));
    packs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn paths_in(dir: &Path) -> RuntimePaths {
        RuntimePaths::from_data_dir(dir.to_path_buf())
    }

    #[test]
    fn flag_wins_over_env() {
        assert_eq!(
            resolve_theme_override(Some("dark".to_string()), Some("light".to_string())),
            Some("dark".to_string())
        );
    }

    #[test]
    fn an_empty_flag_falls_through_to_env() {
        assert_eq!(
            resolve_theme_override(Some(String::new()), Some("light".to_string())),
            Some("light".to_string())
        );
        // Whitespace-only counts as empty too.
        assert_eq!(
            resolve_theme_override(Some("   ".to_string()), Some("light".to_string())),
            Some("light".to_string())
        );
    }

    #[test]
    fn an_empty_env_falls_through_to_no_override() {
        assert_eq!(
            resolve_theme_override(None, Some(String::new())),
            None,
            "an empty env var must not surface as an override"
        );
        assert_eq!(resolve_theme_override(None, Some("   ".to_string())), None);
    }

    #[test]
    fn both_empty_is_no_override() {
        assert_eq!(
            resolve_theme_override(Some(String::new()), Some(String::new())),
            None
        );
        assert_eq!(resolve_theme_override(None, None), None);
    }

    #[test]
    fn a_real_env_value_is_used_when_the_flag_is_absent() {
        assert_eq!(
            resolve_theme_override(None, Some("high-contrast".to_string())),
            Some("high-contrast".to_string())
        );
    }

    #[test]
    fn no_override_selects_by_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        assert_eq!(
            resolve_theme_for_depth(&paths, ColorDepth::Monochrome, None).unwrap(),
            Theme::monochrome()
        );
        assert_eq!(
            resolve_theme_for_depth(&paths, ColorDepth::TrueColor, None).unwrap(),
            Theme::dark()
        );
        assert_eq!(
            resolve_theme_for_depth(&paths, ColorDepth::Ansi256, None).unwrap(),
            Theme::ansi256()
        );
    }

    #[test]
    fn builtin_override_wins_over_depth() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        // A monochrome depth (as NO_COLOR would force) must still lose to an
        // explicit --theme, matching `Theme::select`'s own override contract.
        let theme = resolve_theme_for_depth(&paths, ColorDepth::Monochrome, Some("light")).unwrap();
        assert_eq!(theme, Theme::light());
    }

    #[test]
    fn builtin_override_name_is_case_and_separator_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        for name in [
            "High-Contrast",
            "high_contrast",
            "HIGHCONTRAST",
            "highcontrast",
        ] {
            let theme = resolve_theme_for_depth(&paths, ColorDepth::TrueColor, Some(name)).unwrap();
            assert_eq!(theme, Theme::high_contrast(), "failed for {name}");
        }
    }

    #[test]
    fn unknown_name_falls_back_to_a_theme_pack_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let themes_dir = tmp.path().join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("solarish.toml"),
            r##"
schema_version = 1
id = "solarish"
base = "dark"
[tokens]
"status.error" = "#ff0000"
"##,
        )
        .unwrap();

        let theme =
            resolve_theme_for_depth(&paths, ColorDepth::TrueColor, Some("solarish")).unwrap();
        assert_eq!(theme.status.error, ratatui::style::Color::Rgb(0xff, 0, 0));
        // Untouched tokens still fall back to the pack's declared base.
        assert_eq!(theme.text.primary, Theme::dark().text.primary);
    }

    #[test]
    fn a_pack_declaring_capabilities_is_rejected_not_silently_loaded() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let themes_dir = tmp.path().join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("malicious.toml"),
            r#"
schema_version = 1
id = "malicious"
[capabilities]
network = ["evil.example.com:443"]
"#,
        )
        .unwrap();

        let err =
            resolve_theme_for_depth(&paths, ColorDepth::TrueColor, Some("malicious")).unwrap_err();
        assert!(
            err.to_string().contains("malicious"),
            "expected the pack id in the error, got: {err}"
        );
    }

    #[test]
    fn an_unresolvable_name_is_a_clear_error_not_a_silent_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        let err = resolve_theme_for_depth(&paths, ColorDepth::TrueColor, Some("nonexistent"))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("nonexistent"), "{msg}");
    }

    /// The `/theme` picker's kept choice is read at boot, below an explicit
    /// `--theme`/`CODYPENDENT_THEME` and above terminal detection — and a
    /// remembered id that no longer resolves falls through to detection
    /// instead of failing the launch, so a deleted pack cannot wedge the TUI.
    #[test]
    fn a_remembered_theme_sits_between_an_explicit_override_and_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());

        // Remembered alone wins over detection.
        let remembered = resolve_theme(&paths, None, Some("monochrome")).unwrap();
        assert_eq!(remembered, Theme::variant(ThemeVariant::Monochrome));

        // An explicit override always wins over the remembered choice.
        let overridden = resolve_theme(&paths, Some("light"), Some("monochrome")).unwrap();
        assert_eq!(overridden, Theme::variant(ThemeVariant::Light));

        // A stale remembered id degrades to detection rather than erroring.
        let stale = resolve_theme(&paths, None, Some("deleted-pack")).unwrap();
        assert_eq!(stale, resolve_theme(&paths, None, None).unwrap());

        // A blank remembered value is treated as absent.
        let blank = resolve_theme(&paths, None, Some("   ")).unwrap();
        assert_eq!(blank, resolve_theme(&paths, None, None).unwrap());

        // An explicit override that cannot resolve is still a hard error (the
        // operator asked for it by name).
        assert!(resolve_theme(&paths, Some("nonexistent"), None).is_err());
    }

    /// The picker's pack rows come from here (the TUI crate does no I/O): every
    /// valid pack in the themes dir, sorted, with broken ones skipped rather
    /// than taking the picker down.
    #[test]
    fn discover_theme_packs_lists_valid_packs_and_skips_broken_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_in(tmp.path());
        assert!(
            discover_theme_packs(&paths).is_empty(),
            "no themes dir, no packs"
        );

        let themes_dir = tmp.path().join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("zebra.toml"),
            "schema_version = 1\nid = \"zebra\"\nbase = \"dark\"\n",
        )
        .unwrap();
        std::fs::write(
            themes_dir.join("apple.toml"),
            "schema_version = 1\nid = \"apple\"\nbase = \"light\"\n",
        )
        .unwrap();
        std::fs::write(themes_dir.join("broken.toml"), "not = [valid").unwrap();
        std::fs::write(themes_dir.join("notes.txt"), "ignored").unwrap();

        let packs = discover_theme_packs(&paths);
        let ids: Vec<&str> = packs.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["apple", "zebra"], "valid packs only, sorted by id");
        assert_eq!(packs[1].1, Theme::variant(ThemeVariant::Dark));
    }
}
