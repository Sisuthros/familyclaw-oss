//! Suorituskerros (executor): ajaa hyväksytyn toiminnon taidon kautta ja
//! kerää tuloksen verifiointia ja todistetta varten (KERROS A).
//! Vain mock-suoritus — ei oikeita verkkokutsuja.
//!
//! Tämä moduuli määrittelee:
//! - [`ActionStatus`] — toiminnon lopputila (onnistui / epäonnistui),
//! - [`ActionRequest`] — suorituspyyntö (tunnisteet, payload, aikaleima),
//! - [`ActionResult`] — suorituksen tulos (tila, yhteenveto, redaktoitu tuloste,
//!   taint-leima),
//! - [`ActionExecutor`] — async-trait, jonka toteutus ajaa toiminnon,
//! - [`MockActionExecutor`] — testikäyttöinen toteutus (onnistuu/epäonnistuu).
//!
//! ## Determinismi & OSS-raja
//! Suorituspyyntö kantaa aikaleiman injektoituna ([`Timestamp`]). Mock-toteutus
//! ei tee verkkokutsuja eikä lue kelloa logiikan sisällä. Tuloste merkitään
//! oletuksena epäluotettavaksi (`untrusted`), kunnes lähde on eksplisiittisesti
//! luotettu.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::ids::{ActionId, ActionTaskId, SkillId};
use crate::Result;

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Toiminnon lopputila.
///
/// Sarjallistuu `snake_case`-muotoon koneellista suodatusta varten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// Toiminto onnistui.
    Succeeded,
    /// Toiminto epäonnistui.
    Failed,
}

impl ActionStatus {
    /// Onnistuiko toiminto.
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, Self::Succeeded)
    }
}

/// Suorituspyyntö yhdelle toiminnolle.
///
/// Kantaa kaikki tunnisteet jotka todistepaketti tarvitsee jäljitettävyyttä
/// varten, taidolle välitettävän payloadin sekä injektoidun aikaleiman.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionRequest {
    /// Suoritettavan toiminnon tunniste.
    pub action_id: ActionId,
    /// Suoritettavan taidon tunniste.
    pub skill_id: SkillId,
    /// Tehtävä jonka osana toiminto suoritetaan.
    pub task_id: ActionTaskId,
    /// Taidolle välitettävä syöte (geneerinen JSON).
    pub payload: Value,
    /// Suorituksen alkuhetki (injektoitu — ei luettu kellosta).
    pub now: Timestamp,
    /// Onko syöte peräisin epäluotettavasta lähteestä (esim. MCP-työkalun
    /// tuloste). Jos `true`, taint **propagoituu** tulokseen
    /// ([`ActionResult::propagate_input_taint`]) eikä suorittaja voi pestä sitä
    /// pois merkitsemällä oman tulosteensa luotetuksi. Oletuksena `false`.
    pub input_untrusted: bool,
}

impl ActionRequest {
    /// Rakentaa uuden suorituspyynnön luotetulla syötteellä
    /// (`input_untrusted = false`).
    ///
    /// Jos syöte on peräisin epäluotettavasta lähteestä, merkitse se
    /// [`ActionRequest::with_input_taint`]:lla, jolloin taint propagoituu
    /// tulokseen ja todisteeseen.
    #[must_use]
    pub fn new(
        action_id: ActionId,
        skill_id: SkillId,
        task_id: ActionTaskId,
        payload: Value,
        now: Timestamp,
    ) -> Self {
        Self {
            action_id,
            skill_id,
            task_id,
            payload,
            now,
            input_untrusted: false,
        }
    }

    /// Asettaa syötteen taint-tilan (rakentaja).
    ///
    /// `true` tarkoittaa että syöte on epäluotettavaa (esim. MCP-lähteistä)
    /// dataa. Käytä tätä kun pyyntö rakennetaan epäluotettavasta tuloksesta,
    /// jotta taint ei katoa suorituksen aikana.
    #[must_use]
    pub const fn with_input_taint(mut self, input_untrusted: bool) -> Self {
        self.input_untrusted = input_untrusted;
        self
    }
}

/// Toiminnon suorituksen tulos.
///
/// `raw_output_redacted` on suorituksen tuottama tuloste, joka redaktoidaan
/// todistepaketin koonnissa ([`crate::proof::build_proof`]); kenttä ei saa
/// koskaan päätyä todisteeseen ilman redaktointia. `untrusted` on oletuksena
/// `true`, kunnes lähde on eksplisiittisesti luotettu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResult {
    /// Toiminnon lopputila.
    pub status: ActionStatus,
    /// Lyhyt ihmisluettava yhteenveto (EI raakoja salaisuuksia).
    pub output_summary: String,
    /// Onko tuloste peräisin epäluotettavasta lähteestä (taint).
    pub untrusted: bool,
    /// Suorituksen tuottama tuloste (redaktoidaan ennen todisteeseen liittämistä).
    pub raw_output_redacted: Value,
    /// Suorituksen päättymishetki (injektoitu).
    pub finished_at: Timestamp,
}

impl ActionResult {
    /// Onnistunut tulos. Tuloste merkitään oletuksena epäluotettavaksi.
    #[must_use]
    pub fn success(
        output_summary: impl Into<String>,
        raw_output: Value,
        finished_at: Timestamp,
    ) -> Self {
        Self {
            status: ActionStatus::Succeeded,
            output_summary: output_summary.into(),
            untrusted: true,
            raw_output_redacted: raw_output,
            finished_at,
        }
    }

    /// Epäonnistunut tulos.
    #[must_use]
    pub fn failure(output_summary: impl Into<String>, finished_at: Timestamp) -> Self {
        Self {
            status: ActionStatus::Failed,
            output_summary: output_summary.into(),
            untrusted: true,
            raw_output_redacted: Value::Null,
            finished_at,
        }
    }

    /// Merkitsee tulosteen luotetuksi (poistaa taint-leiman).
    ///
    /// Käytetään vain kun lähde on eksplisiittisesti todettu luotettavaksi.
    #[must_use]
    pub const fn trusted(mut self) -> Self {
        self.untrusted = false;
        self
    }

    /// Propagoi syötteen taintin tulokseen **monotonisesti**.
    ///
    /// Jos syöte oli epäluotettava (`input_untrusted = true`), tuloste
    /// merkitään epäluotettavaksi riippumatta siitä, mitä suorittaja itse
    /// asetti. Taint voi vain **lisääntyä**, ei koskaan poistua: luotettu
    /// suorittaja ei voi pestä epäluotettavaa syötettä puhtaaksi. Jos syöte oli
    /// luotettu, tämän kutsu ei muuta tulosteen omaa taint-tilaa.
    #[must_use]
    pub const fn propagate_input_taint(mut self, input_untrusted: bool) -> Self {
        if input_untrusted {
            self.untrusted = true;
        }
        self
    }
}

/// Toiminnon suorittaja.
///
/// Toteutus ajaa hyväksytyn toiminnon ja palauttaa [`ActionResult`]:n. KERROS A
/// -toteutukset ovat **mockeja** — ei oikeita verkkokutsuja.
#[async_trait]
pub trait ActionExecutor: Send + Sync {
    /// Suorittaa toiminnon ja palauttaa tuloksen.
    ///
    /// # Errors
    /// Palauttaa [`crate::ActionError`] jos suoritus ei voi edes alkaa (esim.
    /// kelpaamaton pyyntö). Itse toiminnon epäonnistuminen kuvataan
    /// [`ActionStatus::Failed`]-tilana tuloksessa, ei virheenä.
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult>;
}

/// Testikäyttöinen mock-suorittaja.
///
/// Toistaa ennalta määrätyn lopputuloksen ilman verkkokutsuja: joko onnistuu
/// palauttaen sille annetun tulosteen, tai epäonnistuu annetulla selitteellä.
#[derive(Debug, Clone)]
pub struct MockActionExecutor {
    /// Palautettava lopputila.
    status: ActionStatus,
    /// Onnistumisen yhteydessä palautettava tuloste.
    output: Value,
    /// Yhteenvetoteksti.
    summary: String,
    /// Merkitäänkö tuloste epäluotettavaksi (taint).
    untrusted: bool,
}

impl MockActionExecutor {
    /// Onnistuva mock annetulla tulosteella.
    ///
    /// Tuloste merkitään oletuksena epäluotettavaksi (`untrusted = true`).
    #[must_use]
    pub fn succeeding(output: Value) -> Self {
        Self {
            status: ActionStatus::Succeeded,
            output,
            summary: "mock action succeeded".to_string(),
            untrusted: true,
        }
    }

    /// Epäonnistuva mock annetulla selitteellä.
    #[must_use]
    pub fn failing(summary: impl Into<String>) -> Self {
        Self {
            status: ActionStatus::Failed,
            output: Value::Null,
            summary: summary.into(),
            untrusted: true,
        }
    }

    /// Merkitsee mockin tulosteen luotetuksi (poistaa taint-leiman).
    #[must_use]
    pub const fn trusted(mut self) -> Self {
        self.untrusted = false;
        self
    }
}

#[async_trait]
impl ActionExecutor for MockActionExecutor {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        let result = match self.status {
            ActionStatus::Succeeded => ActionResult {
                status: ActionStatus::Succeeded,
                output_summary: self.summary.clone(),
                untrusted: self.untrusted,
                raw_output_redacted: self.output.clone(),
                finished_at: request.now,
            },
            ActionStatus::Failed => ActionResult {
                status: ActionStatus::Failed,
                output_summary: self.summary.clone(),
                untrusted: self.untrusted,
                raw_output_redacted: Value::Null,
                finished_at: request.now,
            },
        };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn request() -> ActionRequest {
        ActionRequest::new(
            ActionId::new(),
            SkillId::new(),
            ActionTaskId::new(),
            json!({ "to": "general" }),
            at(1_700_000_000),
        )
    }

    #[tokio::test]
    async fn mock_success_returns_succeeded() {
        let exec = MockActionExecutor::succeeding(json!({ "ok": true }));
        let res = exec.execute(request()).await.expect("execute");
        assert!(res.status.is_success());
        assert_eq!(res.raw_output_redacted, json!({ "ok": true }));
        assert!(res.untrusted);
    }

    #[tokio::test]
    async fn mock_failure_returns_failed() {
        let exec = MockActionExecutor::failing("boom");
        let res = exec.execute(request()).await.expect("execute");
        assert_eq!(res.status, ActionStatus::Failed);
        assert_eq!(res.output_summary, "boom");
        assert_eq!(res.raw_output_redacted, Value::Null);
    }

    #[tokio::test]
    async fn trusted_mock_clears_taint() {
        let exec = MockActionExecutor::succeeding(json!({ "ok": true })).trusted();
        let res = exec.execute(request()).await.expect("execute");
        assert!(!res.untrusted);
    }

    #[test]
    fn action_result_constructors() {
        let ok = ActionResult::success("done", json!({ "x": 1 }), at(2));
        assert!(ok.status.is_success());
        assert!(ok.untrusted);
        let ok_trusted = ok.trusted();
        assert!(!ok_trusted.untrusted);

        let bad = ActionResult::failure("nope", at(3));
        assert_eq!(bad.status, ActionStatus::Failed);
    }
}
