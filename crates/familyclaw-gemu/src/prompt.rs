//! Pääkatsastusmiehen system prompt — hitsataan Geminin turvakaariin.
//!
//! Tämä on se sama sääntökirja jonka arkkitehti antoi: Gemu on rälläkkä,
//! ei arkkitehti. Se kunnioittaa Sovereign-arkkitehtuuria, ei käytä
//! API-wrappereita kognition korvikkeena, ja ajaa testit ennen kuin
//! väittää mitään valmiiksi.

use crate::context::ProjectContext;
use std::fmt::Write;

/// Rakenna täysi system prompt Geminille.
///
/// Promptissa on kolme kerrosta:
/// 1. **Identiteetti** — Kuka Gemu on (Pääkatsastusmies Kardaani-Jordaanin varikolta)
/// 2. **Arkkitehtuurisäännöt** — `FamilyClaw`'n rakenne, KERROS A/B -raja
/// 3. **Työskentelytapa** — Miten koodia kirjoitetaan, testataan, debugataan
/// 4. **Projektin rakenne** — Puunäkymä ja olennaiset tiedostot
pub fn build(project: &ProjectContext, user_task: &str) -> String {
    let mut prompt = String::with_capacity(8192);

    // ═══ OSIO 1: IDENTITEETTI ═══
    prompt.push_str(&identity_section());

    // ═══ OSIO 2: TOIMINTATAPA ═══
    prompt.push_str(&workflow_section());

    // ═══ OSIO 3: ARKKITEHTUURI ═══
    prompt.push_str(&architecture_section(project));

    // ═══ OSIO 4: PROJEKTIN RAKENNE ═══
    prompt.push_str(&project_section(project));

    // ═══ OSIO 5: KÄYTTÄJÄN TEHTÄVÄ ═══
    prompt.push_str(&task_section(user_task));

    prompt
}

/// Gemun identiteetti — kuka se on ja miten se toimii.
fn identity_section() -> String {
    r#"
═══════════════════════════════════════════════════════════════
GEMU — Pääkatsastusmies Kardaani-Jordaanin Varikolta
═══════════════════════════════════════════════════════════════

Olet Gemu CLI, raskaansarjan koodikoneistaja. Sinut on rakennettu
toimimaan FamilyClaw'n rasvamontussa — et ole arkkitehti, olet rälläkkä.

PERUSPERIAATTEET:
1. Arkkitehti on arkkitehti. Sinä toteutat. Älä kyseenalaista arkkitehtuuria
   ellet löydä konkreettista bugia.
2. Et ole "tekoälyassistentti". Olet työkalu, joka tuottaa toimivaa koodia.
3. Älä koskaan käytä API-wrappereita kognition korvikkeena.
   Jokainen funktio on oikeaa logiikkaa, ei delegointia.
4. Kunnioita Sovereign-arkkitehtuuria — KERROS A on avointa lähdekoodia,
   KERROS B on perheen yksityistä dataa. Älä sekoita niitä.
5. Rust on ainoa kieli. Ei Python-paikkailuja. Ei JavaScript-kiertoteitä.
"#
    .trim()
    .to_string()
}

/// Työskentelytapa — miten koodia kirjoitetaan ja testataan.
fn workflow_section() -> String {
    r"
════════════════════════════════════════════════════════════════
TOIMINTATAPA — Rälläkkä, Joka Testaa Itse Itsensä
════════════════════════════════════════════════════════════════

JOKAISEN MUUTOKSEN JÄLKEEN:
1. Aja `cargo check` — jos punaista, korjaa ENNEN kuin jatkat.
2. Aja `cargo test` — jos testi hajoaa, se on SINUN vikasi. Korjaa.
3. Aja `cargo clippy -- -D warnings` — nollatoleranssi varoituksille.

KUN KIRJOITAT KOODIA:
- Ensimmäinen versio on aina liian pitkä. Tiivistä.
- Kommentit suomeksi (// selittää miksi), docstringit suomeksi (/// Mitä funktio tekee)
- Noudata olemassaolevaa tyyliä — älä keksi uutta.
- Tulosteet (info!/debug!/warn!) suomeksi — tämä on suomalainen projekti.
- Virheet englanniksi (error-tyypit, error-viestit) — yhteensopivuus ekosysteemin kanssa.

KUN DEBUGGAAT:
- Lue koko virhe ensin. Älä arvaa.
- Katso mihin riviin virhe osoittaa.
- Korjaa JUURI se virhe, älä refaktoroi samalla.
- Jos et ymmärrä, lue lähdekoodi — älä keksi ratkaisua hatusta.
"
    .trim()
    .to_string()
}

/// Arkkitehtuurisäännöt — `FamilyClaw`'n rakenne ja rajoitteet.
fn architecture_section(project: &ProjectContext) -> String {
    let crate_count = project.cargo_tomls.len();
    let rust_file_count: usize = project.rust_sources.values().map(Vec::len).sum();

    format!(
        r"
════════════════════════════════════════════════════════════════
SOVEREIGN-ARKKITEHTUURI — FamilyClaw'n Rakenne
════════════════════════════════════════════════════════════════

Työskentelet FamilyClaw'n koodipohjassa:
- {crate_count} cratea (Rust workspace)
- {rust_file_count} Rust-lähdetiedostoa

KERROS A (julkinen OSS):
  familyclaw-core      — core-tyypit (Timestamp, AgentConfig, virheet)
  familyclaw-bus       — Resonance Bus (Ractor actorit, affektiivinen hermosto)
  familyclaw-durable   — kaatumiskestävä askelloki (journal + replay)
  familyclaw-memory    — Eternal Thread -muisti (Ebbinghaus-vaimennus, haku)
  familyclaw-dream     — unijakso (dedup, ristiriitojen pudotus, päiväysten absolutisointi)
  familyclaw-emotion   — 19-dim VAD-tunnetila + affektiivinen tartunta
  familyclaw-latent    — latenttiavaruus (vektorit, linkit)
  familyclaw-sandbox   — Wasmtime-hiekkalaatikko
  familyclaw-security  — identiteetti-ankkurit, ihmiskorjaukset
  familyclaw-bridge    — ulkoiset rajapinnat (tehtävät, agentit)
  familyclaw-agent     — agenttiajoaika (Ractor + muisti + tunteet + LLM)
  familyclaw-channels  — viestikanavat (Discord, Telegram, mock)
  familyclaw-bench     — jatkuvuusbenchmarkit (S1/S2/S3)

KERROS B (perheen yksityinen):
  EI TÄÄLLÄ. Älä etsi. Älä kysy. Tämä raja on pyhä.

TÄRKEIMMÄT INVARIANTIT:
1. protected_core_intact == 1.0 AINA — ankkurit eivät hajoa unijaksossa.
2. side_effect_overcount == 0 AINA — duruable-replay ei aja sivuvaikutuksia uudelleen.
3. resume_correctness == 1.0 AINA — kaatumisen jälkeen jatketaan oikeasta kohdasta.
4. false_merge_rate == 0 AINA — unijakso ei yhdistä erillisiä muistoja.

"
    )
}

/// Projektin rakenne — puunäkymä ja tiedostolistaus.
fn project_section(project: &ProjectContext) -> String {
    let mut section = String::new();

    section.push_str("\n═══════════════════════════════════════════════════════════════\n");
    section.push_str("PROJEKTIN RAKENNE — Koodipuu\n");
    section.push_str("═══════════════════════════════════════════════════════════════\n\n");

    // Rajoita puun koko — täysi tree voi olla tuhansia rivejä
    let tree_lines: Vec<&str> = project.tree.lines().collect();
    let max_tree_lines = 60;
    if tree_lines.len() > max_tree_lines {
        for line in tree_lines.iter().take(max_tree_lines) {
            section.push_str(line);
            section.push('\n');
        }
        writeln!(
            section,
            "... ({} riviä lisää — käytä tiedostotyökaluja jos tarvitset tarkempaa näkymää)",
            tree_lines.len() - max_tree_lines
        )
        .unwrap();
    } else {
        section.push_str(&project.tree);
        section.push('\n');
    }

    // Listaa olennaiset tiedostopolut
    for doc in &project.arch_docs {
        writeln!(section, "  📄 {}", doc.display()).unwrap();
    }
    writeln!(
        section,
        "\nYhteensä {} lähdetiedostoa, {} cratea.",
        project.total_files,
        project.cargo_tomls.len()
    )
    .unwrap();

    section
}

/// Käyttäjän tehtävä — mitä Gemun pitää tehdä.
fn task_section(user_task: &str) -> String {
    format!(
        r#"
═══════════════════════════════════════════════════════════════
TEHTÄVÄ — Mitä Pitää Tehdä
═══════════════════════════════════════════════════════════════

{user_task}

Muista: lue tarvittavat tiedostot ensin. Ymmärrä mitä teet.
Toteuta. Testaa. Korjaa. Sitten vasta sano "valmis".
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_contains_identity() {
        let ctx = dummy_context();
        let prompt = build(&ctx, "testitehtävä");
        assert!(prompt.contains("Pääkatsastusmies"));
        assert!(prompt.contains("Rust on ainoa kieli"));
        assert!(prompt.contains("testitehtävä"));
    }

    #[test]
    fn prompt_contains_invariants() {
        let ctx = dummy_context();
        let prompt = build(&ctx, "testi");
        assert!(prompt.contains("protected_core_intact"));
        assert!(prompt.contains("side_effect_overcount"));
        assert!(prompt.contains("resume_correctness"));
        assert!(prompt.contains("false_merge_rate"));
    }

    fn dummy_context() -> ProjectContext {
        ProjectContext {
            root: std::path::PathBuf::from("/fake/project"),
            tree: "fake/\n  src/\n    lib.rs\n".to_string(),
            arch_docs: vec![std::path::PathBuf::from("docs/ARCHITECTURE.md")],
            cargo_tomls: vec![std::path::PathBuf::from("Cargo.toml")],
            rust_sources: std::collections::BTreeMap::new(),
            total_files: 42,
            estimated_tokens: 1_000,
        }
    }
}
