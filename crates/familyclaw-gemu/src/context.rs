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
/// Kävelee hakemistopuun `std::fs`:llä ja ohittaa kohinan: piilotetut
/// hakemistot (`.git`, `.hermes`), `target/` ja `node_modules/`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Pieni RAII-temp-hakemisto ilman ulkoisia crateja.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            let unique = format!(
                "familyclaw-gemu-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            );
            p.push(unique);
            // Varmista puhdas alku.
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).expect("luo temp-hakemisto");
            Self(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// Kirjoita tiedosto suhteellisella polulla, luoden ylähakemistot.
        fn write(&self, rel: &str, contents: &str) {
            let full = self.0.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).expect("luo ylähakemisto");
            }
            fs::write(&full, contents).expect("kirjoita tiedosto");
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Rakentaa pienen workspace-tyylisen projektin temp-hakemistoon.
    fn sample_project(tag: &str) -> TempDir {
        let tmp = TempDir::new(tag);
        tmp.write("Cargo.toml", "[workspace]\n");
        tmp.write("ARCHITECTURE.md", "# Arkkitehtuuri\n");
        tmp.write("crates/agent_a/Cargo.toml", "[package]\nname=\"agent_a\"\n");
        tmp.write("crates/agent_a/src/lib.rs", "pub fn a() {}\n");
        tmp.write("crates/agent_a/src/util.rs", "pub fn b() {}\n");
        tmp.write("crates/agent_b/Cargo.toml", "[package]\nname=\"agent_b\"\n");
        tmp.write("crates/agent_b/src/main.rs", "fn main() {}\n");
        tmp
    }

    #[test]
    fn scan_collects_cargo_tomls_and_rust_sources() {
        let tmp = sample_project("collects");
        let ctx = scan(tmp.path()).expect("scan onnistuu");

        // Kolme Cargo.toml: juuri + kaksi cratea.
        assert_eq!(ctx.cargo_tomls.len(), 3, "kolme Cargo.toml-tiedostoa");

        // Rust-lähteet ryhmitelty craten mukaan.
        assert!(
            ctx.rust_sources.contains_key("agent_a"),
            "agent_a löytyy: {:?}",
            ctx.rust_sources.keys().collect::<Vec<_>>()
        );
        assert!(ctx.rust_sources.contains_key("agent_b"), "agent_b löytyy");
        assert_eq!(ctx.rust_sources["agent_a"].len(), 2, "agent_a: lib + util");
        assert_eq!(ctx.rust_sources["agent_b"].len(), 1, "agent_b: main");

        // Lähteet on lajiteltu craten sisällä.
        let agent_a = &ctx.rust_sources["agent_a"];
        let mut sorted = agent_a.clone();
        sorted.sort();
        assert_eq!(agent_a, &sorted, "agent_a-lähteet lajiteltu");

        // Yhteensä kolme rust-tiedostoa.
        let rust_total: usize = ctx.rust_sources.values().map(Vec::len).sum();
        assert_eq!(rust_total, 3, "yhteensä kolme rust-tiedostoa");
    }

    #[test]
    fn scan_detects_architecture_docs() {
        let tmp = sample_project("docs");
        let ctx = scan(tmp.path()).expect("scan onnistuu");

        let has_arch = ctx
            .arch_docs
            .iter()
            .any(|p| p.to_string_lossy().contains("ARCHITECTURE"));
        assert!(has_arch, "ARCHITECTURE.md tunnistettu: {:?}", ctx.arch_docs);
    }

    #[test]
    fn scan_skips_hidden_and_target_dirs() {
        let tmp = TempDir::new("skips");
        tmp.write("Cargo.toml", "[workspace]\n");
        tmp.write("src/lib.rs", "pub fn ok() {}\n");
        // Nämä pitää ohittaa kokonaan.
        tmp.write("target/debug/junk.rs", "fn junk() {}\n");
        tmp.write(".hidden/secret.rs", "fn secret() {}\n");
        tmp.write("node_modules/dep/index.rs", "fn dep() {}\n");

        let ctx = scan(tmp.path()).expect("scan onnistuu");

        // Vain yksi rust-tiedosto (src/lib.rs), muut ohitettu.
        let rust_total: usize = ctx.rust_sources.values().map(Vec::len).sum();
        assert_eq!(rust_total, 1, "vain src/lib.rs lasketaan");

        // Puunäkymä ei sisällä ohitettuja hakemistoja.
        assert!(!ctx.tree.contains("target"), "target ei näy puussa");
        assert!(!ctx.tree.contains("node_modules"), "node_modules ei näy");
        assert!(!ctx.tree.contains("secret.rs"), "piilotettu ei näy");
    }

    #[test]
    fn scan_keeps_gitignore_but_skips_other_dotfiles() {
        let tmp = TempDir::new("gitignore");
        tmp.write("Cargo.toml", "[workspace]\n");
        tmp.write(".gitignore", "target/\n");
        tmp.write(".env", "SECRET=1\n");

        let ctx = scan(tmp.path()).expect("scan onnistuu");

        assert!(ctx.tree.contains(".gitignore"), ".gitignore näkyy puussa");
        assert!(!ctx.tree.contains(".env"), ".env ohitetaan");
    }

    #[test]
    fn scan_reports_root_and_estimates_tokens() {
        let tmp = sample_project("meta");
        let ctx = scan(tmp.path()).expect("scan onnistuu");

        // root kanonisoitu ja olemassa.
        assert!(ctx.root.exists(), "root-polku on olemassa");
        // total_files kattaa kaikki ei-ohitetut tiedostot.
        assert!(
            ctx.total_files >= 6,
            "tiedostoja laskettu: {}",
            ctx.total_files
        );
        // Token-arvio johdetaan puunäkymästä, joten > 0 ei-tyhjälle projektille.
        assert!(ctx.estimated_tokens > 0, "token-arvio > 0");
        assert!(!ctx.tree.is_empty(), "puunäkymä ei ole tyhjä");
    }

    #[test]
    fn scan_empty_project_has_no_sources() {
        let tmp = TempDir::new("empty");
        let ctx = scan(tmp.path()).expect("scan onnistuu tyhjälle");

        assert_eq!(ctx.total_files, 0, "ei tiedostoja");
        assert!(ctx.rust_sources.is_empty(), "ei rust-lähteitä");
        assert!(ctx.cargo_tomls.is_empty(), "ei Cargo.toml-tiedostoja");
        assert!(ctx.arch_docs.is_empty(), "ei dokumentteja");
    }
}
