# agent_gamma Amplifier — Perheen oma verification-gated memory

> **Suunnitelma:** 2026-06-04  
> **Tekijä:** agent_gamma 💎  
> **Tila:** Ehdotus — odottaa agent_alpha ja the operator hyväksyntää  
> **Vaikutus:** `familyclaw-memory` crate (~165 uutta riviä + testit)

---

## 1. Perustelut

### Ongelma

FamilyClawin muistit (Memory) ovat tällä hetkellä joko "olemassa" tai "ei olemassa". Kun agentti tallentaa muiston, sillä ei ole luottamustasoa — kaikki muistot painavat saman verran haussa riippumatta siitä onko väite vahvistettu vai arvaus.

**Concrete esimerkki:** agent_gamma tallentaa muistiin "Projekti käyttää SQLiteä". Kolme sessiota myöhemmin projekti on siirtynyt SurrealDB:hyn. agent_gamma hakee vanhan muiston, jolla on edelleen korkea retention (koska se vahvistettiin silloin), eikä mikään varoita että tämä tieto on vanhentunut.

### Ratkaisu

Tuodaan Claude Amplifierin kolme ydinkonseptia FamilyClawin olemassaolevaan memory-crateen:

1. **Verification-Gated Memory** — claim → evidence → confirmed
2. **Write-Verify** — lue rivi takaisin ennen kuin palautat "ok"
3. **Confidence-Weighted Retrieval** — confirmed-muistot painaa enemmän haussa

---

## 2. Muutokset

### 2.1 Memory-struct (memory.rs, ~35 riviä)

Uudet kentät olemassaolevaan `Memory`-structiin:

```rust
/// Muiston varmennustila: kuinka luotettava tämä tieto on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Väite — ei vahvistettu, voi olla väärä. (Oletus uusille muistoille.)
    #[default]
    Claim,
    /// Todisteita on, mutta ei vielä varmistettu.
    Evidence,
    /// Vahvistettu kahdella eri todisteella.
    Confirmed,
}

impl VerificationStatus {
    /// Palauttaa painon (0.0–1.0) retrieval-scoringia varten.
    #[must_use]
    pub const fn weight(self) -> f32 {
        match self {
            VerificationStatus::Claim => 0.2,
            VerificationStatus::Evidence => 0.6,
            VerificationStatus::Confirmed => 1.0,
        }
    }
}
```

Uudet kentät Memory-structiin:

```rust
    /// Varmennustila — kuinka luotettava tämä muisto on.
    #[serde(default)]
    pub verification_status: VerificationStatus,

    /// Luottamustaso 0.0–1.0 (johdettu varmennustilasta + evidenceistä).
    #[serde(default)]
    pub confidence: f32,

    /// Todisteet jotka tukevat tätä muistoa.
    #[serde(default)]
    pub evidence: Vec<Evidence>,

    /// Ryhmittelyavain samankaltaisille muistoille (esim. "db-valinta").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern_key: Option<String>,
```

Myös `Evidence`-struct:

```rust
/// Yksittäinen todiste muiston tueksi.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Todistetyyppi.
    pub evidence_type: EvidenceType,
    /// Linkki todisteeseen (commit SHA, testinimi, keskustelu-id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Aikaleima.
    pub recorded_at: Timestamp,
}

/// Todistetyypit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceType {
    BuildPassed,
    TestPassed,
    UserConfirmation,
    IndependentObservation,
    ExternalDoc,
    ProductionMetric,
}
```

`#[serde(default)]` varmistaa taaksepäin yhteensopivuuden olemassaoleville persistoiduille muistoille.

### 2.2 Promote-logiikka (memory.rs, ~40 riviä)

Uudet metodit Memorylle:

```rust
impl Memory {
    /// Lisää todisteen ja päivittää varmennustilan automaattisesti.
    ///
    /// Säännöt:
    /// - Claim + 1 evidence → Evidence (confidence 0.7)
    /// - Evidence + user_confirmation → Confirmed (confidence 1.0)
    /// - Claim + 2 distinct evidence types → Confirmed (confidence 1.0)
    pub fn add_evidence(&mut self, evidence: Evidence) {
        self.evidence.push(evidence);

        // Kerää uniikit todistetyypit
        let mut types: Vec<EvidenceType> = self.evidence.iter()
            .map(|e| e.evidence_type)
            .collect();
        types.sort();
        types.dedup();

        match self.verification_status {
            VerificationStatus::Claim => {
                if types.len() >= 2 {
                    self.verification_status = VerificationStatus::Confirmed;
                    self.confidence = 1.0;
                } else {
                    self.verification_status = VerificationStatus::Evidence;
                    self.confidence = 0.7;
                }
            }
            VerificationStatus::Evidence => {
                if types.contains(&EvidenceType::UserConfirmation) || types.len() >= 2 {
                    self.verification_status = VerificationStatus::Confirmed;
                    self.confidence = 1.0;
                }
                // Muuten pysyy Evidencenä
            }
            VerificationStatus::Confirmed => {
                // Pysyy confirmed — confidence voi nousta, muttei laske
                self.confidence = self.confidence.max(1.0);
            }
        }
    }
}
```

### 2.3 Write-Verify store.rs:ään (~15 riviä)

Nykyinen `LocalJsonStore::add()`: INSERT → palauta id. Uusi:

```rust
// store.rs — LocalJsonStore::add()
async fn add(&self, memory: Memory) -> Result<MessageId> {
    let id = memory.id;
    let mut store = self.inner.write().await;
    
    // Tarkista duplikaatti turn_key
    if let Some(ref key) = memory.turn_key {
        if store.memories.iter().any(|m| m.turn_key.as_deref() == Some(key)) {
            return Ok(id); // Idempotentti — ohita
        }
    }
    
    store.memories.push(memory);
    self.persist(&store).await?;
    
    // --- WRITE VERIFY ---
    // Lue takaisin varmistaaksesi että kirjoitus onnistui
    let exists = store.memories.iter().any(|m| m.id == id);
    if !exists {
        return Err(FamilyClawError::Memory(
            "Write-verify failed: memory not found after insert".into()
        ));
    }
    // --- END WRITE VERIFY ---
    
    Ok(id)
}
```

> **Tarkennus (the operator verifioinnin perusteella):** LocalJsonStore pitää muistot `Vec<Memory>`-muistissa ja persistoi JSON-tiedostoon. Read-back on triviaali — tarkistetaan vaan että muistin id löytyy vektorista persistoinnin jälkeen. SurrealDB-versiossa read-back olisi `SELECT WHERE id = ?`.

### 2.4 Confidence-Weighted Retrieval (retrieval.rs, ~15 riviä)

Nykyinen relevanssikaava:
```
relevance = (keyword · 0.55 + emotion · 0.25 + importance · 0.20) · retention
```

Uusi (confidence kertoo retention:ia):
```
relevance = (keyword · 0.55 + emotion · 0.25 + importance · 0.20) 
          · adjusted_retention

missä adjusted_retention = retention · (0.2 + 0.8 · memory.confidence)
```

Eli:
- `confirmed` (confidence=1.0) → adjusted_retention = retention × 1.0 (ei muutosta)
- `claim` (confidence=0.2) → adjusted_retention = retention × 0.36 (vaimennettu)

```rust
// retrieval.rs — osana relevance-laskentaa
fn adjusted_retention(memory: &Memory, at: Timestamp) -> f32 {
    let base = memory.retention(at);
    let confidence = memory.confidence; // 0.0–1.0
    base * (0.2 + 0.8 * confidence)
}
```

### 2.5 Oracle-moduuli (uusi tiedosto: oracle.rs, ~100 riviä)

```rust
//! Pattern Oracle — tarkistaa ennen muistin kirjoitusta/tärkeää tehtävää,
//! onko vastaavia kuvioita nähty aiemmin.

use crate::memory::{Memory, VerificationStatus};

/// Tuloste Oracle-tarkistuksesta.
pub struct OracleResult {
    pub risk_level: RiskLevel,
    pub score: f32,
    pub matched_patterns: Vec<PatternMatch>,
    pub suggested_approach: Option<String>,
}

pub struct PatternMatch {
    pub pattern_key: Option<String>,
    pub title: String,
    pub confidence: f32,
    pub weight_contribution: f32,
}

pub enum RiskLevel { Low, Medium, High, Critical }

/// Aja Oracle ennen muistin tallennusta — onko tää sama virhe/juttu nähty ennen?
///
/// score = Σ match.frequency · match.confidence · weight(verification_status)
/// weight: Confirmed=1.0, Evidence=0.6, Claim=0.2
pub fn preflight(
    prompt: &str,
    candidates: &[Memory],
) -> OracleResult {
    let tokens = tokenize(prompt);
    let mut matches = Vec::new();
    let mut total_score = 0.0;

    for mem in candidates {
        let overlap = overlap_score(&tokens, mem);
        if overlap < 0.15 { continue; }

        let status_weight = mem.verification_status.weight();
        let contribution = mem.confidence * status_weight * overlap;
        total_score += contribution;

        matches.push(PatternMatch {
            pattern_key: mem.pattern_key.clone(),
            title: mem.content.clone(),
            confidence: mem.confidence,
            weight_contribution: contribution,
        });
    }

    let risk_level = if total_score >= 6.0 { RiskLevel::Critical }
        else if total_score >= 3.0 { RiskLevel::High }
        else if total_score >= 1.0 { RiskLevel::Medium }
        else { RiskLevel::Low };

    OracleResult {
        risk_level,
        score: total_score,
        matched_patterns: matches,
        suggested_approach: None,
    }
}
```

Tokeni tus + overlap-vertailu käyttää olemassaolevaa keyword-matching-logiikkaa retrieval.rs:stä.

### 2.6 MemoryBuilder-päivitys (memory.rs, ~10 riviä)

```rust
pub struct MemoryBuilder {
    // ... olemassaolevat kentät ...
    verification_status: VerificationStatus,
    confidence: f32,
    evidence: Vec<Evidence>,
    pattern_key: Option<String>,
}

impl MemoryBuilder {
    pub fn verification_status(mut self, status: VerificationStatus) -> Self {
        self.verification_status = status;
        self
    }
    
    pub fn pattern_key(mut self, key: impl Into<String>) -> Self {
        self.pattern_key = Some(key.into());
        self
    }
    
    pub fn build(self) -> Memory {
        // ... olemassaoleva build ...
        verification_status: self.verification_status,
        confidence: self.confidence,
        evidence: self.evidence,
        pattern_key: self.pattern_key,
        // ...
    }
}
```

---

## 3. Tiedostomuutokset

| Tiedosto | Muutos | Rivit |
|---------|--------|-------|
| `memory.rs` | Uusi enum + struct + promote-logiikka + builder | ~85 |
| `store.rs` | Write-verify add():iin | ~15 |
| `retrieval.rs` | Confidence-weighted retrieval | ~15 |
| `oracle.rs` (uusi) | Preflight Oracle | ~100 |
| `lib.rs` | Re-export uudet tyypit | ~5 |
| Testit (uusi) | Vähintään claim→evidence→confirmed-putki, write-verify, preflight-scoring | ~80 |
| **Yhteensä** | | **~300** |

> **Päivitetty arvio (the operator tarkennuksen jälkeen):** Ei 20 eikä 120 — noin ~300 riviä krattina. Yksi crate-muutos, ei uusi projekti.

---

## 4. MemoryBuilder- ja serde-yhteensopivuus

Kaikki uudet kentät on merkitty `#[serde(default)]` — olemassaolevat persitoidut muistot (ilman verification_status, confidence, evidence, pattern_key) deserialisoituvat oikein:

```rust
// Vanha JSON (ilman uusia kenttiä):
{ "id": "...", "content": "test", ... }

→ deserialisoituu: verification_status = Claim (default)
                        confidence = 0.0 (default)
                        evidence = [] (default)
                        pattern_key = None (default)
```

`MemoryBuilder`-oletukset:
- `verification_status`: `Claim` (uudet muistot alkavat väitteinä)
- `confidence`: `0.0` (lasketaan promote-logiikalla)
- `evidence`: tyhjä
- `pattern_key`: `None`

---

## 5. Decay × Confidence — suunnittelupäätös

**Kysymys:** Vaikuttaako confidence decay'hyn? (confirmed-muistot unohtuu hitaammin?)

**Vaihtoehto A (yksinkertainen):** Confidence EI vaikuta decay'hyn. Decay perustuu vain importanceen + retentioniin. Confidence vaikuttaa vain retrieval-painotukseen.

**Vaihtoehto B (syvempi):** Confirmed-muistoilla on decay multiplier < 1.0.

**Suositus:** **Vaihtoehto A toistaiseksi.** Confidence-akseli on uusi — sen ei kannata vaikuttaa decay-hyn ennen kuin nähdään miten se toimii käytännössä. Vaihtoehto B on helppo lisätä myöhemmin kertomalla decay_policy.retention() confidence-kertoimella.

---

## 6. Testit

Uusi testitiedosto `tests/amplifier_tests.rs` tai moduuli `memory.rs`-testien yhteydessä:

1. **Claim→Evidence→Confirmed** — lisää evidence, tarkista tilasiirtymä
2. **2 distinct evidence types → Confirmed** — ilman user_confirmationia
3. **Write-verify success** — add() palauttaa Ok(id)
4. **Write-verify failure** — add() palauttaa Err jos rivi ei löydy
5. **Oracle preflight scoring** — Confirmed painaa 5× enemmän kuin Claim
6. **Taaksepäin yhteensopivuus** — vanha JSON deserialisoituu oikein
7. **Confidence-weighted retrieval** — sama muisto eri confidencella antaa eri relevanssin
8. **Evidence-linkkien tallennus ja haku** — roundtrip

---

## 7. Toteutusjärjestys

| Vaihe | Mitä | Kuka |
|------|------|------|
| 1. | `VerificationStatus`-enum + `Evidence`-struct memory.rs:ään | agent_gamma |
| 2. | Uudet kentät `Memory`-structiin + serde-defaultit | agent_gamma |
| 3. | `add_evidence()` + promote-logiikka | agent_gamma |
| 4. | `MemoryBuilder`-päivitys | agent_gamma |
| 5. | Write-verify store.rs:ään | agent_gamma |
| 6. | Confidence-weighted retrieval | agent_gamma |
| 7. | Oracle-moduuli (oracle.rs) | agent_gamma |
| 8. | Re-exportit lib.rs:ään | agent_gamma |
| 9. | Testit | agent_gamma |
| 10. | `cargo clippy` + `cargo test` → vihreä | agent_gamma |
| 11. | agent_alpha arkkitehtuurikatselmointi | agent_alpha |

---

## 8. Integraatio Hermes-agentiin (agent_gamma oma työkalu)

FamilyClaw-muutoksen lisäksi teen skills-muutoksen Hermes-agentille:

**Skill: `agent_gamma-amplifier`** joka:
- Pattern Oracle ennen memory-add-kutsua
- Pattern Oracle ennen isoa työtä (tarkista onko sama ongelma nähty ennen)
- Kirjoittaa confidence-tason memory-muistiinpanoihin

Tämä on **key-value**-muotoinen CLI-skripti (Python) eikä Rust-muutos — täysin erillään FamilyClaw-cratesta.

---

## 9. Bottom line

> **agent_gamma Amplifier ei ole uusi projekti — se on ~300 riviä lisää olemassaolevaan familyclaw-memory-crateen + yksi Hermes-skill. Konseptit tulevat Claude Amplifierista, mutta toteutus on natiivia Rustia ilman ylimääräisiä riippuvuuksia. Taaksepäin yhteensopiva, ei rikkovia muutoksia.**

---

*Suunnitelma valmis 4.6.2026 klo 10:00 UTC*  
*agent_gamma 💎*
