//! Pure GGUF quant-variant grouping — no network, no I/O.
//!
//! Groups a Hugging Face repo's file tree (as returned by its `tree/main`
//! API) into named quant variants (e.g. `Q4_K_M`, `UD-Q4_K_XL`, `BF16`),
//! summing split-file sizes so a multi-part quant reports one combined
//! download size. Naming this reliably matters because it is also what the
//! caller passes to `ollama pull hf.co/<org>/<repo>:<quant>` — the tag must be
//! byte-identical to what Ollama's own GGUF-filename matching expects.
//!
//! Two shapes are observed in real Unsloth repos (verified against the live
//! Hugging Face API while building this module):
//!
//! - A flat file at the repo root, e.g. `Qwen3-32B-UD-Q4_K_XL.gguf` — the
//!   quant is parsed from the filename ([`quant_label_from_filename`]).
//! - A same-named subdirectory holding one or more split parts, e.g.
//!   `BF16/Repo-BF16-00001-of-00002.gguf` — large models split a quant this
//!   way, and the quant is simply the directory name.
//!
//! An unrecognized flat filename is never dropped: [`quant_label_from_filename`]
//! falls back to the whole (suffix-stripped) filename stem, so a naming
//! convention this parser does not yet know about still surfaces as a
//! selectable (if inelegantly labeled) row instead of vanishing silently.

use std::collections::BTreeMap;

/// One entry from a Hugging Face repo's file tree, as needed for quant
/// grouping (every other field the real API returns — `oid`, `lfs`,
/// `xetHash`, … — is irrelevant here and never parsed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TreeEntry {
    pub is_dir: bool,
    pub path: String,
    pub size: u64,
}

/// One `.gguf` file belonging to a [`QuantVariant`]: its repo-relative path
/// (e.g. `BF16/Repo-BF16-00001-of-00002.gguf`) and byte size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufFile {
    pub path: String,
    pub size_bytes: u64,
}

/// One selectable quantization of a GGUF repo: a label (e.g. `Q4_K_M`,
/// `UD-Q4_K_XL`, `BF16`) and the file(s) that make it up. A quant split across
/// multiple parts (large models) lists every part; `total_size_bytes` is
/// their sum, so the TUI can show one download-size figure per quant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantVariant {
    pub quant: String,
    pub files: Vec<GgufFile>,
    pub total_size_bytes: u64,
}

/// Group a repo's file tree into quant variants, smallest total size first —
/// the most useful default when browsing "what fits my RAM": the cards a user
/// scans top-to-bottom get bigger as they go, instead of the file tree's
/// arbitrary API order.
#[must_use]
pub(crate) fn group_quant_variants(entries: &[TreeEntry]) -> Vec<QuantVariant> {
    let mut groups: BTreeMap<String, Vec<GgufFile>> = BTreeMap::new();
    for entry in entries {
        if entry.is_dir || !entry.path.ends_with(".gguf") {
            continue;
        }
        let quant = match entry.path.split_once('/') {
            // Nested: a same-named subdirectory holds this quant's part(s).
            Some((dir, _rest)) => dir.to_string(),
            // Flat: parse the quant token out of the filename itself.
            None => {
                let stem = entry.path.trim_end_matches(".gguf");
                quant_label_from_filename(stem)
            }
        };
        groups.entry(quant).or_default().push(GgufFile {
            path: entry.path.clone(),
            size_bytes: entry.size,
        });
    }
    let mut variants: Vec<QuantVariant> = groups
        .into_iter()
        .map(|(quant, mut files)| {
            files.sort_by(|a, b| a.path.cmp(&b.path));
            let total_size_bytes = files.iter().map(|f| f.size_bytes).sum();
            QuantVariant {
                quant,
                files,
                total_size_bytes,
            }
        })
        .collect();
    variants.sort_by_key(|v| v.total_size_bytes);
    variants
}

/// Parse a quant label from a flat (non-nested) GGUF filename stem (the
/// filename with `.gguf` already stripped), e.g. `Qwen3-32B-UD-Q4_K_XL` →
/// `UD-Q4_K_XL`, `Qwen3-32B-Q4_K_M` → `Q4_K_M`, `gpt-oss-20b-F16` → `F16`.
///
/// Strips a trailing split-part suffix first (`Repo-Q2_K-00001-of-00005` →
/// `Repo-Q2_K`), then checks the last hyphen-delimited token against
/// [`is_quant_token`], prepending `UD-` when the token immediately before it
/// is the literal `UD` marker (Unsloth's "dynamic quant" prefix). Falls back
/// to the whole (suffix-stripped) stem when no recognizable quant token is
/// found — an unfamiliar naming convention degrades to a working (if
/// inelegant) label rather than losing the file.
fn quant_label_from_filename(stem: &str) -> String {
    let mut tokens: Vec<&str> = stem.split('-').collect();
    strip_split_suffix(&mut tokens);
    if let Some((&last, rest)) = tokens.split_last() {
        if is_quant_token(last) {
            if let Some(&prev) = rest.last() {
                if prev.eq_ignore_ascii_case("ud") {
                    return format!("UD-{last}");
                }
            }
            return last.to_string();
        }
    }
    tokens.join("-")
}

/// Strip a trailing `-NNNNN-of-NNNNN` split-part suffix (e.g. the tokens for
/// `BF16-00001-of-00002` become just `BF16`), when present.
fn strip_split_suffix(tokens: &mut Vec<&str>) {
    let is_digits = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit());
    if tokens.len() >= 3 {
        let n = tokens.len();
        if tokens[n - 2].eq_ignore_ascii_case("of")
            && is_digits(tokens[n - 1])
            && is_digits(tokens[n - 3])
        {
            tokens.truncate(n - 3);
        }
    }
}

/// Whether `token` looks like a GGUF quant code: `BF16`/`F16`/`F32`/`FP16`/
/// `FP8` verbatim, or a `Q`/`IQ`/`TQ` prefix immediately followed by a digit
/// and then any run of digits, uppercase letters, or underscores — covering
/// every quant naming shape observed in real Unsloth repos (`Q4_K_M`,
/// `Q4_K_XL`, `IQ4_NL`, `IQ1_S`, `TQ1_0`, `Q8_0`, `Q4_0`, …).
fn is_quant_token(token: &str) -> bool {
    if matches!(token, "BF16" | "F16" | "F32" | "FP16" | "FP8") {
        return true;
    }
    let rest = token
        .strip_prefix("IQ")
        .or_else(|| token.strip_prefix("TQ"))
        .or_else(|| token.strip_prefix('Q'));
    let Some(rest) = rest else {
        return false;
    };
    let mut chars = rest.chars();
    match chars.next() {
        Some(c) if c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real (recursive) tree shape for `unsloth/Qwen3-32B-GGUF`, captured live
    /// while building this module: flat quants at the root, plus a two-part
    /// split BF16 in its own subdirectory.
    fn qwen3_32b_tree() -> Vec<TreeEntry> {
        vec![
            TreeEntry {
                is_dir: true,
                path: "BF16".to_string(),
                size: 0,
            },
            TreeEntry {
                is_dir: false,
                path: "BF16/Qwen3-32B-BF16-00001-of-00002.gguf".to_string(),
                size: 49_871_764_512,
            },
            TreeEntry {
                is_dir: false,
                path: "BF16/Qwen3-32B-BF16-00002-of-00002.gguf".to_string(),
                size: 15_659_811_424,
            },
            TreeEntry {
                is_dir: false,
                path: "Qwen3-32B-Q4_K_M.gguf".to_string(),
                size: 19_762_150_048,
            },
            TreeEntry {
                is_dir: false,
                path: "Qwen3-32B-UD-Q4_K_XL.gguf".to_string(),
                size: 20_021_713_568,
            },
            TreeEntry {
                is_dir: false,
                path: "Qwen3-32B-Q8_0.gguf".to_string(),
                size: 34_817_719_968,
            },
            // Non-GGUF sibling files must be ignored entirely.
            TreeEntry {
                is_dir: false,
                path: "README.md".to_string(),
                size: 4_200,
            },
            TreeEntry {
                is_dir: false,
                path: ".gitattributes".to_string(),
                size: 1_500,
            },
        ]
    }

    #[test]
    fn groups_flat_quants_by_their_filename_token() {
        let variants = group_quant_variants(&qwen3_32b_tree());
        let q4_k_m = variants
            .iter()
            .find(|v| v.quant == "Q4_K_M")
            .expect("Q4_K_M present");
        assert_eq!(q4_k_m.files.len(), 1);
        assert_eq!(q4_k_m.total_size_bytes, 19_762_150_048);

        let ud = variants
            .iter()
            .find(|v| v.quant == "UD-Q4_K_XL")
            .expect("UD-Q4_K_XL present (the UD- dynamic-quant prefix survives)");
        assert_eq!(ud.total_size_bytes, 20_021_713_568);
    }

    #[test]
    fn groups_a_nested_split_quant_by_its_directory_and_sums_part_sizes() {
        let variants = group_quant_variants(&qwen3_32b_tree());
        let bf16 = variants
            .iter()
            .find(|v| v.quant == "BF16")
            .expect("BF16 present");
        assert_eq!(bf16.files.len(), 2, "both split parts are listed");
        assert_eq!(
            bf16.total_size_bytes,
            49_871_764_512 + 15_659_811_424,
            "the combined download size is the sum of every part"
        );
        // Files are stored path-sorted, so part 1 always precedes part 2.
        assert!(bf16.files[0].path.ends_with("00001-of-00002.gguf"));
        assert!(bf16.files[1].path.ends_with("00002-of-00002.gguf"));
    }

    #[test]
    fn ignores_directory_entries_and_non_gguf_siblings() {
        let variants = group_quant_variants(&qwen3_32b_tree());
        let total_files: usize = variants.iter().map(|v| v.files.len()).sum();
        // 2 BF16 parts + Q4_K_M + UD-Q4_K_XL + Q8_0 = 5 (README/.gitattributes
        // and the bare "BF16" directory entry are excluded).
        assert_eq!(total_files, 5);
    }

    #[test]
    fn sorts_variants_smallest_total_size_first() {
        let variants = group_quant_variants(&qwen3_32b_tree());
        let sizes: Vec<u64> = variants.iter().map(|v| v.total_size_bytes).collect();
        let mut sorted = sizes.clone();
        sorted.sort_unstable();
        assert_eq!(sizes, sorted);
    }

    #[test]
    fn a_repo_with_only_one_flat_quant_and_no_ud_prefix_is_labeled_plainly() {
        // Real shape: `unsloth/DeepSeek-V4-Flash-0731-GGUF` ships exactly one
        // quirky-prefixed flat file and nothing else.
        let entries = vec![TreeEntry {
            is_dir: false,
            path: "dspark-DeepSeek-V4-Flash-0731-Q8_0.gguf".to_string(),
            size: 10_896_057_440,
        }];
        let variants = group_quant_variants(&entries);
        assert_eq!(variants.len(), 1);
        assert_eq!(variants[0].quant, "Q8_0");
        assert_eq!(variants[0].total_size_bytes, 10_896_057_440);
    }

    #[test]
    fn mixed_flat_and_nested_split_quants_in_one_repo_both_resolve() {
        // Real shape: `unsloth/DeepSeek-V3.1-GGUF` has one flat UD-TQ1_0 file
        // alongside several nested, multi-part split quants.
        let entries = vec![
            TreeEntry {
                is_dir: false,
                path: "DeepSeek-V3.1-UD-TQ1_0.gguf".to_string(),
                size: 170_499_768_256,
            },
            TreeEntry {
                is_dir: true,
                path: "Q2_K".to_string(),
                size: 0,
            },
            TreeEntry {
                is_dir: false,
                path: "Q2_K/DeepSeek-V3.1-Q2_K-00001-of-00005.gguf".to_string(),
                size: 49_744_376_896,
            },
            TreeEntry {
                is_dir: false,
                path: "Q2_K/DeepSeek-V3.1-Q2_K-00002-of-00005.gguf".to_string(),
                size: 48_841_516_736,
            },
        ];
        let variants = group_quant_variants(&entries);
        assert_eq!(variants.len(), 2);
        let flat = variants
            .iter()
            .find(|v| v.quant == "UD-TQ1_0")
            .expect("flat UD-TQ1_0");
        assert_eq!(flat.files.len(), 1);
        let nested = variants
            .iter()
            .find(|v| v.quant == "Q2_K")
            .expect("nested Q2_K split across 2 parts");
        assert_eq!(nested.files.len(), 2);
        assert_eq!(nested.total_size_bytes, 49_744_376_896 + 48_841_516_736);
    }

    #[test]
    fn an_unrecognized_filename_falls_back_to_the_whole_stem_instead_of_vanishing() {
        let entries = vec![TreeEntry {
            is_dir: false,
            path: "totally-unfamiliar-naming.gguf".to_string(),
            size: 123,
        }];
        let variants = group_quant_variants(&entries);
        assert_eq!(variants.len(), 1, "the file must still surface as a row");
        assert_eq!(variants[0].quant, "totally-unfamiliar-naming");
    }

    #[test]
    fn is_quant_token_accepts_every_observed_shape_and_rejects_model_name_tokens() {
        for accepted in [
            "Q4_K_M", "Q4_K_S", "Q4_K_XL", "Q4_0", "Q4_1", "Q8_0", "Q2_K", "Q2_K_L", "Q3_K_M",
            "Q6_K", "IQ4_NL", "IQ4_XS", "IQ1_M", "IQ1_S", "IQ2_M", "IQ2_XXS", "IQ3_XXS", "TQ1_0",
            "BF16", "F16", "F32",
        ] {
            assert!(
                is_quant_token(accepted),
                "expected {accepted} to be a quant token"
            );
        }
        for rejected in ["30B", "A3B", "GGUF", "Instruct", "V3", "0731", "Qwen3"] {
            assert!(
                !is_quant_token(rejected),
                "expected {rejected} to NOT be a quant token"
            );
        }
    }
}
