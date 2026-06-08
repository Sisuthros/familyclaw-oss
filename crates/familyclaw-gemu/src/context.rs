//! Koodipuun skanneri — tuottaa rakenteellisen näkymän projektista.
//!
//! Gemu ei syötä kaikkea koodia Geminille kerralla, vaan antaa
//! korkean tason arkkitehtuurikuvan + polut olennaisiin tiedostoihin.
//! Gemini lukee tarvitsemansa tiedostot itse omilla työkaluillaan.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Projektin rakenteellinen näkymä.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// Työhakemiston absoluuttinen polku.
    pub root: PathBuf,
    /// Puunäkymä (sisennetty, kuten `tree`-komento).
    pub tree: String,
    /// Löydetyt arkkitehtuuridokumentit (ARCHITECTURE.md, DESIGN.md, docs/*.md).
    pub arch_docs: Vec<PathBuf>,
    /// Cargo.toml -tiedostot (workspace-rakenne).
    pub cargo_tomls: Vec<PathBuf>,
    /// Rust-lähdekooditiedostot, ryhmiteltynä craten mukaan.
    pub rust_sources: BTreeMap<String, Vec<PathBuf>>,
    /// Tiedostojen kokonaismäärä (suodatettuna).
    pub total_files: usize,
    /// Arvioitu token-määrä jos kaikki luettaisiin.
    pub estimated_tokens: usize,
}

/// Skannaa projektin ja tuottaa rakenteellisen kontekstin.
///
/// Käyttää `ignore`-cratea (sama kuin ripgrep) `.gitignore`-kunnioitukseen.
pub fn scan(root: &Path) -> anyhow::Result<ProjectContext> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut tree_lines: Vec<String> = Vec::new();
    let mut arch_docs: Vec<PathBuf> = Vec::new();
    let mut cargo_tomls: Vec<PathBuf> = Vec::new();
    let mut rust_sources: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut total_files = 0usize;

    tree_lines.push(format!("{}", root.display()));
    tree_lines.push("│".to_string());
    walk_dir(
        &root,
        &root,
        0,
        &mut tree_lines,
        &mut arch_docs,
        &mut cargo_tomls,
        &mut rust_sources,
        &mut total_files,
    )?;

    // Lajittele Rust-tiedostot
    let mut sorted: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for (k, mut v) in rust_sources {
        v.sort();
        sorted.insert(k, v);
    }
    let rust_sources = sorted;

    // Arvioi tokeneita karkeasti (4 merkkiä ≈ 1 token)
    let estimated_tokens = tree_lines.iter().map(String::len).sum::<usize>() / 4;

    Ok(ProjectContext {
        root,
        tree: tree_lines.join("\n"),
        arch_docs,
        cargo_tomls,
        rust_sources,
        total_files,
        estimated_tokens,
    })
}

/// Rekursiivinen hakemiston läpikäynti.
#[allow(clippy::too_many_arguments)]
fn walk_dir(
    base: &Path,
    current: &Path,
    depth: usize,
    tree_lines: &mut Vec<String>,
    arch_docs: &mut Vec<PathBuf>,
    cargo_tomls: &mut Vec<PathBuf>,
    rust_sources: &mut BTreeMap<String, Vec<PathBuf>>,
    total_files: &mut usize,
) -> anyhow::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(current)?.filter_map(Result::ok).collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let prefix = if depth > 0 {
        "  ".repeat(depth - 1)
    } else {
        String::new()
    };

    for (i, entry) in entries.iter().enumerate() {
        let is_last = i == entries.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let name = entry.file_name().to_string_lossy().to_string();
        let file_type = entry.file_type()?;

        // Ohitetaan piilotetut (.git, .hermes, node_modules, target)
        if name.starts_with('.') && name != ".gitignore" {
            continue;
        }
        if name == "target" || name == "node_modules" {
            continue;
        }

        let rel = entry
            .path()
            .strip_prefix(base)
            .unwrap_or(&entry.path())
            .to_path_buf();

        if file_type.is_dir() {
            tree_lines.push(format!("{prefix}{connector}{name}/"));
            walk_dir(
                base,
                &entry.path(),
                depth + 1,
                tree_lines,
                arch_docs,
                cargo_tomls,
                rust_sources,
                total_files,
            )?;
        } else {
            *total_files += 1;
            tree_lines.push(format!("{prefix}{connector}{name}"));

            let ext = entry
                .path()
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_lowercase)
                .unwrap_or_default();

            // Tunnista arkkitehtuuridokumentit
            let is_md = entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("md"));
            let is_txt = entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
            if name.contains("ARCHITECTURE")
                || name.contains("DESIGN")
                || name.contains("SCORECARD")
                || (is_md && depth <= 3)
            {
                // Syvemmältä vain jos dokumentti
                if is_md || is_txt {
                    arch_docs.push(rel.clone());
                }
            }

            // Cargo.toml
            if name == "Cargo.toml" {
                cargo_tomls.push(rel.clone());
            }

            // Rust-lähdekoodit
            if ext == "rs" {
                let crate_name = rel
                    .iter()
                    .nth(1) // crates/<nimi>/...
                    .map_or_else(|| "root".to_string(), |c| c.to_string_lossy().to_string());
                rust_sources
                    .entry(crate_name)
                    .or_default()
                    .push(rel.clone());
            }
        }
    }

    Ok(())
}
