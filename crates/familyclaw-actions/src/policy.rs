//! Käytäntökerros (policy): toiminnon riskiluokka, vaaditut capabilityt
//! (oikeudet) ja hyväksyntäkäytäntö (KERROS A, geneerinen — ei oikeita
//! providereita, sieluja eikä avaimia).
//!
//! Tämä moduuli määrittelee:
//! - [`SkillPermission`] — yksittäinen capability jota taito tarvitsee,
//! - [`ActionRisk`] — toiminnon riskiluokka,
//! - [`ApprovalPolicy`] — milloin ihmisen hyväksyntä vaaditaan,
//! - [`detect_secret_like`] — heuristinen tunnistin merkkijonoille jotka
//!   muistuttavat salaisuutta (käytetään manifestin validoinnissa ja
//!   todisteiden redaktoinnissa).
//!
//! Determinismi: tämä moduuli ei lue kelloa eikä tee verkkokutsuja.

use serde::{Deserialize, Serialize};

/// Moduulin valmiusaste (luuranko-yhteensopivuus).
///
/// Säilytetään, jotta [`crate::all_modules_scaffolded`] kääntyy edelleen
/// muiden vielä luurankovaiheessa olevien moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Yksittäinen capability (oikeus) jonka taito tarvitsee toimiakseen.
///
/// Geneerinen — ei viittauksia oikeisiin providereihin tai palveluihin.
/// Käytäntö ([`ApprovalPolicy`]) ja riskiluokka ([`ActionRisk`]) johdetaan
/// osittain näistä oikeuksista.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillPermission {
    /// Lukea paikallisia tiedostoja.
    ReadFiles,
    /// Kirjoittaa paikallisia tiedostoja (palautettavissa).
    WriteLocalFiles,
    /// Lukea dataa verkosta (vain luku, ei sivuvaikutuksia).
    NetworkRead,
    /// Lähettää viestin (esim. chat-kanavalle) — sivuvaikutuksellinen.
    SendMessage,
    /// Suorittaa koodia.
    ExecuteCode,
    /// Käyttää rahaa (maksutapahtuma).
    SpendMoney,
    /// Kirjoittaa ulkoiseen järjestelmään (palauttamaton sivuvaikutus).
    WriteExternal,
}

/// Toiminnon riskiluokka, kasvavassa vaarallisuusjärjestyksessä mielessä.
///
/// Luokka ohjaa oletushyväksyntää ([`ApprovalPolicy::requires_human`]) ja
/// audit-kirjauksen painotusta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    /// Vain luku — ei sivuvaikutuksia.
    ReadOnly,
    /// Paikallinen kirjoitus (palautettavissa).
    WriteLocal,
    /// Ulkoinen kirjoitus (vaikuttaa kolmannen osapuolen järjestelmään).
    WriteExternal,
    /// Palauttamaton toiminto.
    Irreversible,
    /// Rahankäyttö.
    SpendMoney,
    /// Viestin lähetys (näkyy ulospäin).
    SendMessage,
    /// Koodin suoritus.
    ExecuteCode,
}

impl ActionRisk {
    /// Onko tämä riskiluokka pelkkä luku (ei sivuvaikutuksia).
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    /// Voiko tämä riskiluokka *koskaan* tuottaa automaattisen ajon
    /// ([`ApprovalRequirement::AutoRun`]) jollain käytännöllä.
    ///
    /// Vain [`ActionRisk::ReadOnly`] ja [`ActionRisk::WriteLocal`] voivat ajaa
    /// automaattisesti (luku aina, paikallinen kirjoitus `RequireApproval`-
    /// käytännöllä). Korkeamman riskin luokat (raha, peruuttamaton, ulkoinen
    /// kirjoitus, viesti, koodi) vaativat aina hyväksynnän käytännöstä
    /// riippumatta — ne **eivät** ole auto-ajettavissa.
    ///
    /// Tätä käytetään manifestin ristiintarkistuksessa: korkean riskin oikeutta
    /// ei saa merkitä auto-ajettavaan riskiluokkaan.
    #[must_use]
    pub const fn is_auto_runnable_class(self) -> bool {
        matches!(self, Self::ReadOnly | Self::WriteLocal)
    }
}

impl SkillPermission {
    /// Vaatiiko tämä oikeus että ilmoitettu riskiluokka EI ole auto-ajettava
    /// (eli oikeus on aina hyväksyntää vaativa sivuvaikutus).
    ///
    /// Manifestin ristiintarkistus ([`crate::manifest::SkillManifest::validate`])
    /// käyttää tätä estääkseen tilanteen, jossa rahaa käyttävä
    /// ([`SkillPermission::SpendMoney`]) tai ulkoisesti kirjoittava
    /// ([`SkillPermission::WriteExternal`]) taito merkitään esim.
    /// [`ActionRisk::ReadOnly`]-riskiksi ja näin ohittaa hyväksynnän putkessa.
    #[must_use]
    pub const fn forbids_auto_run_risk(self) -> bool {
        matches!(self, Self::SpendMoney | Self::WriteExternal)
    }

    /// Vaatiiko tämä oikeus että ilmoitettu riskiluokka on täsmälleen
    /// [`ActionRisk::SpendMoney`].
    ///
    /// Rahankäyttö ([`SkillPermission::SpendMoney`]) ei saa naamioitua
    /// lievemmäksi riskiksi: jos oikeus on mukana, riskiluokan on oltava
    /// [`ActionRisk::SpendMoney`], jotta audit ja hyväksyntä kohtelevat sitä
    /// rahankäyttönä.
    #[must_use]
    pub const fn requires_spend_money_risk(self) -> bool {
        matches!(self, Self::SpendMoney)
    }
}

/// Hyväksyntäkäytäntö: milloin ihmisen hyväksyntä vaaditaan ennen suoritusta.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    /// Hyväksyntä ohitetaan vain jos toiminto on luku-tyyppinen
    /// ([`ActionRisk::ReadOnly`]); muutoin hyväksyntä vaaditaan.
    AutoIfReadOnly,
    /// Hyväksyntä vaaditaan, ellei riskiluokka ole pelkkä luku.
    RequireApproval,
    /// Hyväksyntä vaaditaan aina, riskiluokasta riippumatta.
    AlwaysRequireApproval,
}

/// Hyväksyntävaatimus jonka [`required_approval`] palauttaa: saako toiminnon
/// ajaa automaattisesti vai vaaditaanko ihmisen hyväksyntä ensin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRequirement {
    /// Toiminnon saa ajaa ilman erillistä hyväksyntää.
    AutoRun,
    /// Toiminto vaatii ihmisen hyväksynnän ennen suoritusta.
    RequireApproval,
}

impl ApprovalRequirement {
    /// Vaatiiko vaatimus ihmisen hyväksynnän.
    #[must_use]
    pub const fn requires_approval(self) -> bool {
        matches!(self, Self::RequireApproval)
    }
}

/// Ratkaisee hyväksyntävaatimuksen riskiluokan ja käytännön perusteella.
///
/// Sääntölogiikka (fail-safe — epävarmassa tilanteessa vaaditaan hyväksyntä):
/// - [`ActionRisk::SpendMoney`] ja [`ActionRisk::Irreversible`] vaativat
///   **aina** hyväksynnän, vaikka käytäntö yrittäisi ohittaa sen
///   (esim. manifesti pyytäisi auto-run). Nämä eivät koskaan aja itsestään.
/// - [`ActionRisk::WriteExternal`], [`ActionRisk::SendMessage`] ja
///   [`ActionRisk::ExecuteCode`] vaativat oletuksena hyväksynnän.
/// - [`ActionRisk::ReadOnly`] ja [`ActionRisk::WriteLocal`] saavat ajaa
///   automaattisesti **ellei** käytäntö pakota hyväksyntää
///   ([`ApprovalPolicy::AlwaysRequireApproval`], tai mikä tahansa
///   käytäntö joka ei salli auto-runia ei-luku-luokalle).
///
/// Käytännön rooli: [`ApprovalPolicy::AutoIfReadOnly`] sallii auto-runin vain
/// luku-luokalle; [`ApprovalPolicy::RequireApproval`] sallii sen luku- ja
/// paikallisen kirjoituksen luokille; [`ApprovalPolicy::AlwaysRequireApproval`]
/// ei salli koskaan. Korkean riskin luokat (raha, peruuttamaton, ulkoinen,
/// viesti, koodi) eivät koskaan aja automaattisesti käytännöstä riippumatta.
#[must_use]
pub const fn required_approval(risk: ActionRisk, policy: ApprovalPolicy) -> ApprovalRequirement {
    use ActionRisk::{ExecuteCode, Irreversible, ReadOnly, SendMessage, SpendMoney, WriteLocal};

    // Fail-safe: raha + peruuttamaton vaativat aina hyväksynnän.
    if matches!(risk, SpendMoney | Irreversible) {
        return ApprovalRequirement::RequireApproval;
    }

    // Sivuvaikutukselliset ulospäin näkyvät luokat vaativat oletuksena
    // hyväksynnän (ulkoinen kirjoitus, viesti, koodin suoritus).
    if matches!(risk, ExecuteCode | SendMessage) {
        return ApprovalRequirement::RequireApproval;
    }

    // Käytäntö joka pakottaa aina → hyväksyntä.
    if matches!(policy, ApprovalPolicy::AlwaysRequireApproval) {
        return ApprovalRequirement::RequireApproval;
    }

    // Loput (ReadOnly, WriteLocal, WriteExternal) käytännön mukaan.
    match (risk, policy) {
        // Vain luku saa ajaa automaattisesti molemmilla ei-pakottavilla käytännöillä.
        (ReadOnly, ApprovalPolicy::AutoIfReadOnly | ApprovalPolicy::RequireApproval) => {
            ApprovalRequirement::AutoRun
        }
        // Paikallinen kirjoitus saa ajaa vain RequireApproval-käytännöllä
        // (joka sallii sivuvaikutuksettomamman paikallisen kirjoituksen),
        // EI AutoIfReadOnly-käytännöllä joka sallii vain puhtaan luvun.
        (WriteLocal, ApprovalPolicy::RequireApproval) => ApprovalRequirement::AutoRun,
        // Ulkoinen kirjoitus ja kaikki muut yhdistelmät → hyväksyntä.
        _ => ApprovalRequirement::RequireApproval,
    }
}

impl ApprovalPolicy {
    /// Vaaditaanko annetulla riskiluokalla ihmisen hyväksyntä.
    ///
    /// - [`ApprovalPolicy::AlwaysRequireApproval`] vaatii aina.
    /// - [`ApprovalPolicy::AutoIfReadOnly`] ja
    ///   [`ApprovalPolicy::RequireApproval`] vaativat aina paitsi kun
    ///   riskiluokka on [`ActionRisk::ReadOnly`].
    #[must_use]
    pub const fn requires_human(self, risk: ActionRisk) -> bool {
        match self {
            Self::AlwaysRequireApproval => true,
            Self::AutoIfReadOnly | Self::RequireApproval => !risk.is_read_only(),
        }
    }

    /// Onko tämä käytäntö sellainen, joka aidosti voi vaatia hyväksynnän
    /// (eli **ei** koskaan automaattisesti ohita sivuvaikutuksellisia toimia).
    ///
    /// Käytetään validoinnissa: ulkoinen kirjoitus
    /// ([`SkillPermission::WriteExternal`]) ei saa olla puhtaasti
    /// luku-automaation varassa.
    #[must_use]
    pub const fn can_require_approval(self) -> bool {
        matches!(self, Self::RequireApproval | Self::AlwaysRequireApproval)
    }
}

/// Tunnistaa heuristisesti salaisuudelta näyttävän merkkijonon.
///
/// Käytetään kahteen tarkoitukseen:
/// 1. **Manifestin validointi** — manifestin tekstikentät eivät saa sisältää
///    salaisuuksia (avaimia/tokeneita).
/// 2. **Todisteiden redaktointi** — saman heuristiikan avulla redaktoidaan
///    salaisuudelta näyttävät arvot todistepaketeista.
///
/// Tunnistettavat kuviot:
/// - `sk-`-etuliite jota seuraa ≥8 sanamerkkiä (OpenAI-tyyliset avaimet),
/// - AWS-tyyliset access key -tunnukset (`AKIA` + 16 isoa kirjainta/numeroa),
/// - `Bearer <token>` -muotoiset Authorization-arvot,
/// - pitkät yhtenäiset hex- tai base64-jonot (≥32 merkkiä),
/// - kenttämuotoiset paljastukset kuten `api_key=...`, `apikey: ...`,
///   `secret=...`, `password=...`, `token=...`.
///
/// Heuristiikka on tarkoituksella varovainen (false-positive ennemmin kuin
/// false-negative), koska salaisuuden vuotaminen on vakavampi kuin liiallinen
/// redaktointi.
#[must_use]
pub fn detect_secret_like(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    if has_sk_prefix(trimmed)
        || has_aws_access_key(trimmed)
        || has_bearer_token(trimmed)
        || has_long_token_run(trimmed)
    {
        return true;
    }

    has_secret_field_assignment(trimmed)
}

/// `sk-`-etuliite jota seuraa vähintään 8 sanamerkkiä.
fn has_sk_prefix(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.split(|c: char| !is_token_char(c)).any(|chunk| {
        let lower_chunk = chunk;
        lower_chunk.starts_with("sk-") && lower_chunk.len() >= 3 + 8 && {
            lower_chunk[3..].chars().all(is_token_char)
        }
    })
}

/// AWS-tyylinen access key id: `AKIA` + 16 isoa aakkosnumeerista merkkiä.
fn has_aws_access_key(value: &str) -> bool {
    value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|chunk| {
            chunk.len() == 20
                && chunk.starts_with("AKIA")
                && chunk[4..]
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        })
}

/// `Bearer <token>` -muotoinen Authorization-arvo (token ≥8 merkkiä).
fn has_bearer_token(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let Some(pos) = lower.find("bearer ") else {
        return false;
    };
    let rest = value[pos + "bearer ".len()..].trim_start();
    let token: String = rest.chars().take_while(|&c| is_token_char(c)).collect();
    token.len() >= 8
}

/// Pitkä yhtenäinen hex- tai base64-tyylinen jono (≥32 merkkiä).
fn has_long_token_run(value: &str) -> bool {
    value
        .split(|c: char| !is_base64_char(c))
        .any(|chunk| chunk.len() >= 32 && looks_high_entropy(chunk))
}

/// Kenttämuotoinen paljastus, esim. `api_key=...`, `secret: ...`, `token=...`.
fn has_secret_field_assignment(value: &str) -> bool {
    const FIELD_NAMES: [&str; 6] = ["api_key", "apikey", "secret", "password", "token", "passwd"];
    let lower = value.to_ascii_lowercase();
    for name in FIELD_NAMES {
        let mut search_from = 0;
        while let Some(rel) = lower[search_from..].find(name) {
            let idx = search_from + rel;
            // Varmista että kenttänimeä edeltää sananraja (ei esim. "mytoken").
            let boundary_ok = idx == 0
                || !lower.as_bytes()[idx - 1].is_ascii_alphanumeric()
                    && lower.as_bytes()[idx - 1] != b'_';
            let after = idx + name.len();
            if boundary_ok && assignment_has_value(&lower[after..]) {
                return true;
            }
            search_from = idx + name.len();
        }
    }
    false
}

/// Onko kenttänimen jälkeen `=`/`:` ja vähintään yksi tokenmerkki arvona.
fn assignment_has_value(after: &str) -> bool {
    let after = after.trim_start();
    let Some(rest) = after.strip_prefix('=').or_else(|| after.strip_prefix(':')) else {
        return false;
    };
    let rest = rest.trim_start().trim_start_matches(['"', '\'']);
    rest.chars().next().is_some_and(is_token_char)
}

/// Onko merkki sallittu token-merkki (aakkosnumeerinen, `-` tai `_`).
const fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Onko merkki tyypillinen base64/hex-aakkosto (sis. `+`, `/`, `=`).
const fn is_base64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' || c == '-' || c == '_'
}

/// Karkea entropia-arvio: jonossa on sekä kirjaimia että numeroita, tai se on
/// puhdasta pitkää hexiä. Estää esim. pitkän pelkän kirjainjonon (sana)
/// luokittelun salaisuudeksi.
fn looks_high_entropy(chunk: &str) -> bool {
    let has_digit = chunk.chars().any(|c| c.is_ascii_digit());
    let has_alpha = chunk.chars().any(|c| c.is_ascii_alphabetic());
    let is_hex = chunk.chars().all(|c| c.is_ascii_hexdigit());
    (has_digit && has_alpha) || (is_hex && chunk.len() >= 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_serde_snake_case() {
        let json = serde_json::to_string(&SkillPermission::WriteExternal).expect("serialize");
        assert_eq!(json, "\"write_external\"");
        let back: SkillPermission = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, SkillPermission::WriteExternal);
    }

    #[test]
    fn risk_serde_snake_case() {
        let json = serde_json::to_string(&ActionRisk::ReadOnly).expect("serialize");
        assert_eq!(json, "\"read_only\"");
    }

    #[test]
    fn unknown_risk_is_rejected_by_serde() {
        let parsed: std::result::Result<ActionRisk, _> = serde_json::from_str("\"nuke_planet\"");
        assert!(parsed.is_err());
    }

    #[test]
    fn approval_policy_requires_human_logic() {
        assert!(!ApprovalPolicy::AutoIfReadOnly.requires_human(ActionRisk::ReadOnly));
        assert!(ApprovalPolicy::AutoIfReadOnly.requires_human(ActionRisk::WriteExternal));
        assert!(!ApprovalPolicy::RequireApproval.requires_human(ActionRisk::ReadOnly));
        assert!(ApprovalPolicy::RequireApproval.requires_human(ActionRisk::SpendMoney));
        assert!(ApprovalPolicy::AlwaysRequireApproval.requires_human(ActionRisk::ReadOnly));
    }

    #[test]
    fn can_require_approval_flags() {
        assert!(!ApprovalPolicy::AutoIfReadOnly.can_require_approval());
        assert!(ApprovalPolicy::RequireApproval.can_require_approval());
        assert!(ApprovalPolicy::AlwaysRequireApproval.can_require_approval());
    }

    #[test]
    fn read_only_may_auto_run_unless_forced() {
        assert_eq!(
            required_approval(ActionRisk::ReadOnly, ApprovalPolicy::AutoIfReadOnly),
            ApprovalRequirement::AutoRun
        );
        assert_eq!(
            required_approval(ActionRisk::ReadOnly, ApprovalPolicy::RequireApproval),
            ApprovalRequirement::AutoRun
        );
        // Käytäntö pakottaa hyväksynnän jopa lukutoiminnolle.
        assert_eq!(
            required_approval(ActionRisk::ReadOnly, ApprovalPolicy::AlwaysRequireApproval),
            ApprovalRequirement::RequireApproval
        );
    }

    #[test]
    fn write_external_requires_approval_by_default() {
        for policy in [
            ApprovalPolicy::AutoIfReadOnly,
            ApprovalPolicy::RequireApproval,
            ApprovalPolicy::AlwaysRequireApproval,
        ] {
            assert_eq!(
                required_approval(ActionRisk::WriteExternal, policy),
                ApprovalRequirement::RequireApproval,
                "write_external must always require approval (policy {policy:?})"
            );
        }
    }

    #[test]
    fn send_message_and_execute_code_require_approval() {
        for risk in [ActionRisk::SendMessage, ActionRisk::ExecuteCode] {
            for policy in [
                ApprovalPolicy::AutoIfReadOnly,
                ApprovalPolicy::RequireApproval,
                ApprovalPolicy::AlwaysRequireApproval,
            ] {
                assert!(
                    required_approval(risk, policy).requires_approval(),
                    "{risk:?} must require approval (policy {policy:?})"
                );
            }
        }
    }

    #[test]
    fn spend_money_and_irreversible_always_require_approval_even_if_auto() {
        // Fail-safe: vaikka käytäntö olisi kaikkein salliva, näitä ei aja koskaan
        // automaattisesti.
        for risk in [ActionRisk::SpendMoney, ActionRisk::Irreversible] {
            assert_eq!(
                required_approval(risk, ApprovalPolicy::AutoIfReadOnly),
                ApprovalRequirement::RequireApproval,
                "{risk:?} must fail safe to RequireApproval"
            );
            assert_eq!(
                required_approval(risk, ApprovalPolicy::RequireApproval),
                ApprovalRequirement::RequireApproval
            );
        }
    }

    #[test]
    fn write_local_auto_runs_only_under_require_approval_policy() {
        assert_eq!(
            required_approval(ActionRisk::WriteLocal, ApprovalPolicy::RequireApproval),
            ApprovalRequirement::AutoRun
        );
        assert_eq!(
            required_approval(ActionRisk::WriteLocal, ApprovalPolicy::AutoIfReadOnly),
            ApprovalRequirement::RequireApproval
        );
    }

    #[test]
    fn approval_requirement_serde_snake_case() {
        let json = serde_json::to_string(&ApprovalRequirement::AutoRun).expect("serialize");
        assert_eq!(json, "\"auto_run\"");
        let back: ApprovalRequirement = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ApprovalRequirement::AutoRun);
    }

    #[test]
    fn detects_sk_prefix_secret() {
        // Rakennetaan ajonaikana ettei lähdekoodissa ole pitkää literaalia.
        let fake = format!("sk-{}", "live".repeat(4));
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_aws_access_key() {
        let fake = format!("AKIA{}", "A1B2C3D4E5F6G7H8");
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_bearer_token() {
        let fake = format!("Authorization: Bearer {}", "abcd1234efgh");
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_long_hex_run() {
        let fake = "a1b2".repeat(10); // 40 hex-merkkiä
        assert!(detect_secret_like(&fake));
    }

    #[test]
    fn detects_field_assignment() {
        assert!(detect_secret_like("api_key=abc123"));
        assert!(detect_secret_like("password: hunter2x"));
    }

    #[test]
    fn ignores_plain_text() {
        assert!(!detect_secret_like(
            "Lähettää tervehdysviestin kanavalle general."
        ));
        assert!(!detect_secret_like(""));
        assert!(!detect_secret_like("mock-1"));
        assert!(!detect_secret_like("example-org/example-repo"));
    }
}
