//! Skill-manifestit: taidon kuvaus, vaaditut oikeudet, riskiluokka,
//! hyväksyntäkäytäntö sekä syöte-/tulosvihjeet (KERROS A, geneerinen — ei
//! oikeita providereita, sieluja eikä avaimia).
//!
//! Manifesti voidaan jäsentää sekä TOML- ([`SkillManifest::from_toml`]) että
//! JSON-muodosta ([`SkillManifest::from_json`]). Validointi
//! ([`SkillManifest::validate`]) torjuu:
//! - tyhjän tai `nil`-tunnisteen,
//! - tyhjän nimen tai version,
//! - salaisuudelta näyttävät arvot missä tahansa tekstikentässä (myös
//!   [`SkillManifest::input_schema`]-skeeman tekstiarvoissa),
//! - [`SkillManifest::input_schema`]-skeeman jonka juuri ei ole JSON-objekti,
//! - ulkoisen kirjoituksen ([`SkillPermission::WriteExternal`]) ilman
//!   hyväksyntää aidosti vaativaa käytäntöä.
//!
//! Tuntemattomat riskiluokat hylkää jo serde (enum-validointi).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ActionError, Result};
use crate::ids::SkillId;
use crate::policy::{detect_secret_like, ActionRisk, ApprovalPolicy, SkillPermission};

/// Moduulin valmiusaste (luuranko-yhteensopivuus).
///
/// Säilytetään, jotta [`crate::all_modules_scaffolded`] kääntyy edelleen.
pub(crate) const SCAFFOLDED: bool = true;

/// Manifestin oletusarvoinen syöteskeema: tyhjä JSON-objekti `{"type":"object"}`.
///
/// Käytetään kahdessa paikassa: serde-deserialisoinnin oletuksena (vanhat
/// tallennetut manifestit ilman `input_schema`-kenttää latautuvat tällä) sekä
/// [`SkillManifest`]-rakentajien lähtöarvona. Juuri on AINA objekti, jotta
/// skeema kelpaa LLM:lle työkalun `parameters`-kentäksi sellaisenaan.
#[must_use]
pub fn default_input_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

/// Yhden taidon manifesti: kaikki tieto jonka rekisteri ja käytäntökerros
/// tarvitsevat ennen kuin taitoa voidaan suunnitella tai suorittaa.
///
/// Manifesti on puhtaasti dataa (ei suoritettavaa logiikkaa) ja sarjallistuu
/// TOML- ja JSON-muotoon ilman muunnoksia.
///
/// `PartialEq` (ei `Eq`) johtuu [`SkillManifest::input_schema`]-kentästä:
/// `serde_json::Value` toteuttaa vain `PartialEq`:n (liukulukujen vuoksi).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillManifest {
    /// Taidon yksilöivä tunniste rekisterissä.
    pub id: SkillId,
    /// Ihmisluettava nimi (esim. `send-greeting`).
    pub name: String,
    /// Versiomerkkijono (esim. `1.0.0`); ei pakoteta semverin muotoon.
    pub version: String,
    /// Lyhyt kuvaus mitä taito tekee.
    pub description: String,
    /// Taidon tarvitsemat oikeudet (capabilityt).
    pub permissions: Vec<SkillPermission>,
    /// Toiminnon riskiluokka.
    pub risk: ActionRisk,
    /// Hyväksyntäkäytäntö (vaaditaanko ihmisen hyväksyntä).
    pub approval_policy: ApprovalPolicy,
    /// Vapaamuotoinen vihje odotetusta syötteen muodosta (esim. skeema-nimi).
    ///
    /// Tarkoitettu ihmisluettavaksi näytöksi; rakenteellisen, koneluettavan
    /// version tarjoaa [`SkillManifest::input_schema`].
    #[serde(default)]
    pub input_hint: Option<String>,
    /// Vapaamuotoinen vihje odotetusta tuloksen muodosta.
    #[serde(default)]
    pub output_hint: Option<String>,
    /// Koneluettava JSON Schema -kuvaus taidon syötteestä.
    ///
    /// Tällä taito voidaan mainostaa LLM:lle aitona työkaluna: skeema siirtyy
    /// sellaisenaan työkalun `parameters`-kentäksi. Juuren TÄYTYY olla
    /// JSON-objekti (skalaari/taulukko ei kelpaa); validointi
    /// ([`SkillManifest::validate`]) torjuu muut. Kun arvoa ei anneta, serde
    /// täyttää sen [`default_input_schema`]-funktiolla (`{"type":"object"}`),
    /// jotta vanhat ilman tätä kenttää tallennetut manifestit latautuvat yhä.
    #[serde(default = "default_input_schema")]
    pub input_schema: Value,
}

impl SkillManifest {
    /// Asettaa koneluettavan syöteskeeman ja palauttaa muokatun manifestin
    /// (rakentaja-tyylinen ketjutus).
    ///
    /// Skeema ei korvaa [`SkillManifest::input_hint`]-vihjettä — molemmat
    /// säilyvät: `input_hint` ihmisnäyttöä, `input_schema` LLM:lle. Arvoa EI
    /// validoida tässä; juuren objektivaatimus ja salaisuustarkistus tehdään
    /// vasta [`SkillManifest::validate`]-kutsussa.
    ///
    /// # Examples
    /// ```
    /// use familyclaw_actions::manifest::SkillManifest;
    /// # use familyclaw_actions::ids::SkillId;
    /// # use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy};
    /// # let base = SkillManifest {
    /// #     id: SkillId::new(),
    /// #     name: "demo".into(),
    /// #     version: "1.0.0".into(),
    /// #     description: "demo".into(),
    /// #     permissions: vec![],
    /// #     risk: ActionRisk::ReadOnly,
    /// #     approval_policy: ApprovalPolicy::AutoIfReadOnly,
    /// #     input_hint: None,
    /// #     output_hint: None,
    /// #     input_schema: serde_json::json!({ "type": "object" }),
    /// # };
    /// let m = base.with_input_schema(serde_json::json!({
    ///     "type": "object",
    ///     "properties": { "text": { "type": "string" } },
    ///     "required": ["text"]
    /// }));
    /// assert_eq!(m.input_schema["type"], "object");
    /// ```
    #[must_use]
    pub fn with_input_schema(mut self, input_schema: Value) -> Self {
        self.input_schema = input_schema;
        self
    }

    /// Jäsentää manifestin TOML-merkkijonosta.
    ///
    /// # Errors
    /// Palauttaa [`ActionError::ManifestParse`] jos TOML on virheellinen tai
    /// ei vastaa manifestin rakennetta (esim. tuntematon riskiluokka).
    pub fn from_toml(input: &str) -> Result<Self> {
        toml::from_str(input).map_err(|e| ActionError::ManifestParse(e.to_string()))
    }

    /// Jäsentää manifestin JSON-merkkijonosta.
    ///
    /// # Errors
    /// Palauttaa [`ActionError::ManifestParse`] jos JSON on virheellinen tai
    /// ei vastaa manifestin rakennetta (esim. tuntematon riskiluokka).
    pub fn from_json(input: &str) -> Result<Self> {
        serde_json::from_str(input).map_err(|e| ActionError::ManifestParse(e.to_string()))
    }

    /// Sarjallistaa manifestin JSON-merkkijonoksi.
    ///
    /// # Errors
    /// Palauttaa [`ActionError::ManifestParse`] jos sarjallistus epäonnistuu.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|e| ActionError::ManifestParse(e.to_string()))
    }

    /// Validoi manifestin sisäisen eheyden ja turvallisuussäännöt.
    ///
    /// # Errors
    /// - [`ActionError::ManifestValidation`] jos tunniste on `nil`, tai nimi/
    ///   versio on tyhjä, tai [`SkillManifest::input_schema`]-skeeman juuri ei
    ///   ole JSON-objekti, tai ulkoinen kirjoitus on ilman hyväksyntää vaativaa
    ///   käytäntöä.
    /// - [`ActionError::SecretInManifest`] jos jokin tekstikenttä tai
    ///   syöteskeeman tekstiarvo sisältää salaisuudelta näyttävän arvon.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_nil() {
            return Err(ActionError::ManifestValidation(
                "skill id puuttuu (nil)".to_string(),
            ));
        }
        if self.name.trim().is_empty() {
            return Err(ActionError::ManifestValidation("name on tyhjä".to_string()));
        }
        if self.version.trim().is_empty() {
            return Err(ActionError::ManifestValidation(
                "version on tyhjä".to_string(),
            ));
        }

        // Syöteskeeman juuren on oltava JSON-objekti, jotta se kelpaa LLM:n
        // työkalun `parameters`-kentäksi sellaisenaan (skalaari/taulukko ei).
        if !self.input_schema.is_object() {
            return Err(ActionError::ManifestValidation(
                "input_schema juuren on oltava JSON-objekti".to_string(),
            ));
        }

        // Turvatarkistus: mikään tekstikenttä ei saa sisältää salaisuutta.
        for (field, value) in self.text_fields() {
            if detect_secret_like(value) {
                return Err(ActionError::SecretInManifest(format!(
                    "kenttä '{field}' näyttää sisältävän salaisuuden"
                )));
            }
        }

        // Sama turvatarkistus syöteskeemalle: skeema on rakenteinen, joten
        // läpikäydään sen kaikki merkkijonosolmut (avaimet ja arvot).
        if let Some(secret_path) = first_secret_in_json(&self.input_schema, "input_schema") {
            return Err(ActionError::SecretInManifest(format!(
                "input_schema-polku '{secret_path}' näyttää sisältävän salaisuuden"
            )));
        }

        // Ulkoinen kirjoitus vaatii käytännön joka aidosti voi vaatia hyväksynnän.
        if self.permissions.contains(&SkillPermission::WriteExternal)
            && !self.approval_policy.can_require_approval()
        {
            return Err(ActionError::ManifestValidation(
                "write_external vaatii hyväksyntää edellyttävän approval_policy-arvon \
                 (require_approval tai always_require_approval)"
                    .to_string(),
            ));
        }

        // Oikeus ↔ riskiluokka -ristiintarkistus (defense in depth).
        //
        // Putki johtaa hyväksyntävaatimuksen riskiluokasta
        // ([`crate::policy::required_approval`]). Jos korkean riskin oikeus
        // (esim. rahankäyttö) merkittäisiin auto-ajettavaan riskiluokkaan
        // (read_only / write_local), putki ajaisi sen ILMAN hyväksyntää.
        // Estetään se täällä: invariantti "spend_money ja irreversible vaativat
        // aina hyväksynnän, vaikka manifesti yrittäisi auto-runia".
        for perm in &self.permissions {
            // Rahankäyttö ei saa naamioitua lievemmäksi riskiksi.
            if perm.requires_spend_money_risk() && self.risk != ActionRisk::SpendMoney {
                return Err(ActionError::ManifestValidation(format!(
                    "oikeus 'spend_money' vaatii risk = spend_money (oli {:?}) — \
                     rahankäyttö ei saa ohittaa hyväksyntää väärällä riskiluokalla",
                    self.risk
                )));
            }
            // Sivuvaikutukselliset oikeudet eivät saa olla auto-ajettavassa
            // riskiluokassa (read_only / write_local).
            if perm.forbids_auto_run_risk() && self.risk.is_auto_runnable_class() {
                return Err(ActionError::ManifestValidation(format!(
                    "oikeus {perm:?} ei saa olla auto-ajettavassa riskiluokassa {:?} — \
                     se ohittaisi vaaditun hyväksynnän",
                    self.risk
                )));
            }
        }

        Ok(())
    }

    /// Palauttaa validoitavat tekstikentät (kenttänimi, arvo) -pareina.
    fn text_fields(&self) -> Vec<(&'static str, &str)> {
        let mut fields = vec![
            ("name", self.name.as_str()),
            ("version", self.version.as_str()),
            ("description", self.description.as_str()),
        ];
        if let Some(hint) = &self.input_hint {
            fields.push(("input_hint", hint.as_str()));
        }
        if let Some(hint) = &self.output_hint {
            fields.push(("output_hint", hint.as_str()));
        }
        fields
    }
}

/// Etsii ensimmäisen salaisuudelta näyttävän merkkijonon JSON-arvosta.
///
/// Käy rekursiivisesti läpi objektien avaimet ja arvot sekä taulukoiden alkiot,
/// ja palauttaa [`Some`]-polun (esim. `input_schema.properties.token`)
/// ensimmäiseen solmuun, jonka tekstiarvo läpäisee
/// [`detect_secret_like`]-tarkistuksen. Palauttaa [`None`], jos salaisuuksia ei
/// löydy. `path` on kutsujan antama juuriprefiksi (esim. `"input_schema"`).
///
/// Tämä laajentaa manifestin salaisuusvapaus-takuun kattamaan myös
/// rakenteisen [`SkillManifest::input_schema`]-skeeman, ei vain tasaisia
/// tekstikenttiä.
fn first_secret_in_json(value: &Value, path: &str) -> Option<String> {
    match value {
        Value::String(s) => {
            if detect_secret_like(s) {
                Some(path.to_string())
            } else {
                None
            }
        }
        Value::Object(map) => map.iter().find_map(|(key, child)| {
            if detect_secret_like(key) {
                return Some(format!("{path}.{key} (avain)"));
            }
            first_secret_in_json(child, &format!("{path}.{key}"))
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .find_map(|(idx, child)| first_secret_in_json(child, &format!("{path}[{idx}]"))),
        // Numerot, boolit ja null eivät voi olla salaisuuksia.
        Value::Number(_) | Value::Bool(_) | Value::Null => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apuri: rakentaa kelvollisen perusmanifestin annetulla tunnisteella.
    fn valid_manifest() -> SkillManifest {
        SkillManifest {
            id: SkillId::new(),
            name: "send-greeting".to_string(),
            version: "1.0.0".to_string(),
            description: "Lähettää tervehdysviestin kanavalle general.".to_string(),
            permissions: vec![SkillPermission::SendMessage],
            risk: ActionRisk::SendMessage,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("text".to_string()),
            output_hint: Some("ack".to_string()),
            input_schema: default_input_schema(),
        }
    }

    #[test]
    fn valid_manifest_loads_from_toml() {
        let id = SkillId::new();
        let toml_src = format!(
            r#"
id = "{id}"
name = "read-doc"
version = "1.2.0"
description = "Lukee paikallisen dokumentin."
permissions = ["read_files"]
risk = "read_only"
approval_policy = "auto_if_read_only"
"#
        );
        let manifest = SkillManifest::from_toml(&toml_src).expect("toml parses");
        assert_eq!(manifest.id, id);
        assert_eq!(manifest.name, "read-doc");
        assert_eq!(manifest.risk, ActionRisk::ReadOnly);
        // input_schema puuttui lähteestä → serde-oletus täyttää sen.
        assert_eq!(manifest.input_schema, default_input_schema());
        manifest.validate().expect("valid manifest validates");
    }

    #[test]
    fn valid_manifest_loads_from_json() {
        let id = SkillId::new();
        let json_src = format!(
            r#"{{
                "id": "{id}",
                "name": "read-doc",
                "version": "1.2.0",
                "description": "Lukee paikallisen dokumentin.",
                "permissions": ["read_files"],
                "risk": "read_only",
                "approval_policy": "auto_if_read_only"
            }}"#
        );
        let manifest = SkillManifest::from_json(&json_src).expect("json parses");
        assert_eq!(manifest.id, id);
        assert_eq!(manifest.approval_policy, ApprovalPolicy::AutoIfReadOnly);
        // Vanha serialisoitu manifesti ilman input_schemaa latautuu yhä.
        assert_eq!(manifest.input_schema, default_input_schema());
        manifest.validate().expect("valid manifest validates");
    }

    #[test]
    fn invalid_id_rejected() {
        let mut m = valid_manifest();
        m.id = SkillId::nil();
        let err = m.validate().expect_err("nil id must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));
    }

    #[test]
    fn empty_name_rejected() {
        let mut m = valid_manifest();
        m.name = "   ".to_string();
        assert!(matches!(
            m.validate(),
            Err(ActionError::ManifestValidation(_))
        ));
    }

    #[test]
    fn unknown_risk_rejected() {
        let id = SkillId::new();
        let json_src = format!(
            r#"{{
                "id": "{id}",
                "name": "x",
                "version": "1.0.0",
                "description": "d",
                "permissions": [],
                "risk": "nuke_planet",
                "approval_policy": "require_approval"
            }}"#
        );
        let parsed = SkillManifest::from_json(&json_src);
        assert!(matches!(parsed, Err(ActionError::ManifestParse(_))));
    }

    #[test]
    fn write_external_without_approval_rejected() {
        let mut m = valid_manifest();
        m.permissions = vec![SkillPermission::WriteExternal];
        m.risk = ActionRisk::WriteExternal;
        m.approval_policy = ApprovalPolicy::AutoIfReadOnly;
        let err = m
            .validate()
            .expect_err("write_external without approval rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));

        // Sama manifesti hyväksyttävällä käytännöllä menee läpi.
        m.approval_policy = ApprovalPolicy::RequireApproval;
        m.validate()
            .expect("write_external with approval validates");
    }

    /// INVARIANT (adversarial): manifesti EI saa ilmoittaa rahankäyttö-oikeutta
    /// ([`SkillPermission::SpendMoney`]) mutta merkitä riskiluokaksi jotain
    /// hyväksynnän ohittavaa (esim. [`ActionRisk::ReadOnly`]). Muuten putki
    /// johtaisi `required_approval(ReadOnly, AutoIfReadOnly) == AutoRun` ja
    /// rahaa käyttävä taito ajaisi ilman hyväksyntää.
    #[test]
    fn spend_money_permission_mislabeled_as_low_risk_rejected() {
        let mut m = valid_manifest();
        m.permissions = vec![SkillPermission::SpendMoney];
        m.risk = ActionRisk::ReadOnly;
        m.approval_policy = ApprovalPolicy::AutoIfReadOnly;
        let err = m
            .validate()
            .expect_err("spend_money mislabeled as read_only must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));

        // Oikein merkittynä (risk = SpendMoney) sama oikeus validoituu.
        m.risk = ActionRisk::SpendMoney;
        m.validate()
            .expect("spend_money with matching risk validates");
    }

    /// INVARIANT (adversarial): ulkoinen kirjoitus -oikeus
    /// ([`SkillPermission::WriteExternal`]) merkittynä peruuttamattoman sijaan
    /// luku-riskiksi ei saa läpäistä — peruuttamaton/ulkoinen sivuvaikutus ei
    /// saa ajaa automaattisesti.
    #[test]
    fn write_external_permission_mislabeled_as_read_only_rejected() {
        let mut m = valid_manifest();
        m.permissions = vec![SkillPermission::WriteExternal];
        m.risk = ActionRisk::ReadOnly;
        // Käytäntö joka aidosti voi vaatia hyväksynnän, jotta aiempi
        // write_external-tarkistus EI ole se mikä hylkää — todistetaan että
        // nimenomaan risk-luokan ristiintarkistus puree.
        m.approval_policy = ApprovalPolicy::RequireApproval;
        let err = m
            .validate()
            .expect_err("write_external mislabeled as read_only must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));
    }

    #[test]
    fn manifest_with_secret_value_rejected() {
        let mut m = valid_manifest();
        // Rakennetaan salaisuus ajonaikana ettei lähteessä ole pitkää literaalia.
        let fake = format!("sk-{}", "live".repeat(4));
        m.description = format!("Käyttää avainta {fake}");
        let err = m.validate().expect_err("secret-looking value rejected");
        assert!(matches!(err, ActionError::SecretInManifest(_)));
    }

    #[test]
    fn json_roundtrip_preserves_manifest() {
        let m = valid_manifest();
        let json = m.to_json().expect("serialize");
        let back = SkillManifest::from_json(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    #[test]
    fn json_roundtrip_preserves_custom_input_schema() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "viesti" },
                "count": { "type": "integer", "minimum": 0 }
            },
            "required": ["text"]
        });
        let m = valid_manifest().with_input_schema(schema.clone());
        m.validate().expect("custom schema validates");
        let back = SkillManifest::from_json(&m.to_json().expect("serialize"))
            .expect("deserialize");
        assert_eq!(back.input_schema, schema);
        assert_eq!(m, back);
    }

    #[test]
    fn default_input_schema_is_empty_object() {
        let schema = default_input_schema();
        assert!(schema.is_object());
        assert_eq!(schema["type"], "object");
    }

    #[test]
    fn with_input_schema_keeps_input_hint() {
        let m = valid_manifest()
            .with_input_schema(serde_json::json!({ "type": "object" }));
        // input_hint säilyy ihmisnäyttöä varten skeeman rinnalla.
        assert_eq!(m.input_hint.as_deref(), Some("text"));
    }

    #[test]
    fn non_object_input_schema_rejected() {
        let mut m = valid_manifest();
        m.input_schema = serde_json::json!("not an object");
        let err = m
            .validate()
            .expect_err("scalar input_schema root must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));

        m.input_schema = serde_json::json!([1, 2, 3]);
        let err = m
            .validate()
            .expect_err("array input_schema root must be rejected");
        assert!(matches!(err, ActionError::ManifestValidation(_)));
    }

    #[test]
    fn secret_in_input_schema_value_rejected() {
        let mut m = valid_manifest();
        // Salaisuus rakennetaan ajonaikana (ei pitkää literaalia lähteessä).
        let fake = format!("sk-{}", "live".repeat(4));
        m.input_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "default": fake }
            }
        });
        let err = m
            .validate()
            .expect_err("secret-looking value inside schema must be rejected");
        assert!(matches!(err, ActionError::SecretInManifest(_)));
    }
}
