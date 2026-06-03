//! Sandbox-rajapinta: [`CodeSandbox`]-trait sekä suorituksen syöte- ja
//! tulostyypit.
//!
//! Rajapinta on backend-riippumaton: oletustoteutus
//! [`NoopSandbox`](crate::NoopSandbox) ei aja koodia (palauttaa
//! [`SandboxError::NotImplemented`](crate::SandboxError::NotImplemented)),
//! ja oikea wasmtime-pohjainen toteutus elää `wasmtime`-featuren takana.
//! Tämä pitää koko workspacen buildin kevyenä silloin kun sandboxia ei
//! tarvita.

use serde::{Deserialize, Serialize};

use crate::capability::CapabilitySet;
use crate::fuel::FuelLimit;

/// Yhden sandbox-suorituksen pyyntö.
///
/// Kokoaa kaiken mitä suoritus tarvitsee: ajettava WASM-tavukoodi,
/// polttoaineraja ja myönnetyt kyvykkyydet. Rakennetaan tyypillisesti
/// builder-tyylillä; oletukset ovat turvalliset (rajattu polttoaine,
/// ei kyvykkyyksiä).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxRequest {
    /// Ajettava WASM-moduuli tavukoodina (`.wasm`).
    pub code: Vec<u8>,

    /// Polttoaineraja tälle suoritukselle.
    pub fuel_limit: FuelLimit,

    /// Suoritukselle myönnetyt kyvykkyydet (oletuksena tyhjä = "deny all").
    pub capabilities: CapabilitySet,
}

impl SandboxRequest {
    /// Rakentaa pyynnön annetusta WASM-tavukoodista turvallisilla oletuksilla
    /// (rajattu oletuspolttoaine, ei kyvykkyyksiä).
    #[must_use]
    pub fn new(code: impl Into<Vec<u8>>) -> Self {
        Self {
            code: code.into(),
            fuel_limit: FuelLimit::default(),
            capabilities: CapabilitySet::deny_all(),
        }
    }

    /// Asettaa polttoainerajan (builder-tyyli).
    #[must_use]
    pub fn with_fuel_limit(mut self, limit: FuelLimit) -> Self {
        self.fuel_limit = limit;
        self
    }

    /// Asettaa kyvykkyysjoukon (builder-tyyli).
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: CapabilitySet) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Validoi pyynnön ennen suoritusta.
    ///
    /// Tarkistaa että koodia on annettu ja että kyvykkyysjoukko on
    /// hyvinmuodostettu. Polttoaineraja on aina kelvollinen tyyppinsä kautta.
    ///
    /// # Errors
    /// [`crate::SandboxError::Setup`] jos koodi on tyhjä, tai
    /// [`crate::SandboxError::Capability`] jos jokin kyvykkyys on kelvoton.
    pub fn validate(&self) -> crate::Result<()> {
        if self.code.is_empty() {
            return Err(crate::SandboxError::setup("sandbox code must not be empty"));
        }
        self.capabilities.validate()?;
        Ok(())
    }
}

/// Onnistuneen sandbox-suorituksen tulos.
///
/// Sarjallistuva, jotta tulos voidaan kirjata durable-lokiin tai välittää
/// busin yli. `output` on ajettavan koodin tuottama tavuvirta (esim. stdout
/// tai eksplisiittinen paluuarvo), `fuel_consumed` mittaa kustannuksen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxOutput {
    /// Koodin tuottama tulostavu-virta.
    #[serde(default)]
    pub output: Vec<u8>,

    /// Suorituksen kuluttama polttoaine.
    pub fuel_consumed: u64,
}

impl SandboxOutput {
    /// Rakentaa tuloksen tavuvirrasta ja kulutuksesta.
    #[must_use]
    pub fn new(output: impl Into<Vec<u8>>, fuel_consumed: u64) -> Self {
        Self {
            output: output.into(),
            fuel_consumed,
        }
    }

    /// Tulostavujen tulkinta UTF-8-merkkijonona (lossy).
    ///
    /// Kelvottomat tavut korvataan U+FFFD-merkillä, joten tämä ei koskaan
    /// epäonnistu — sopii lokitukseen ja diagnostiikkaan.
    #[must_use]
    pub fn output_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

/// Sandbox-suorituksen kokonaistulos.
///
/// Tyyppialias selkeyttää [`CodeSandbox::execute`]:n allekirjoitusta.
pub type SandboxResult = crate::Result<SandboxOutput>;

/// Eristetyn koodisuorituksen rajapinta.
///
/// Toteutukset ajavat (tai kieltäytyvät ajamasta) WASM-tavukoodia annetulla
/// polttoainerajalla ja kyvykkyyksillä. Sopimus:
/// - **Determinismi rajat:** sama [`SandboxRequest`] tuottaa saman tuloksen
///   jos itse WASM on deterministinen (durable-replayn edellytys).
/// - **Turva oletuksena:** ilman myönnettyä kyvykkyyttä koodilla ei ole
///   verkkoa, tiedostoja eikä ympäristömuuttujia.
/// - **Polttoaine pakottaa rajan:** ikuinen silmukka keskeytyy
///   [`SandboxError::FuelExhausted`](crate::SandboxError::FuelExhausted):lla.
///
/// `Send + Sync` jotta sandbox voidaan jakaa actorien välillä busissa.
pub trait CodeSandbox: Send + Sync {
    /// Ajaa annetun pyynnön ja palauttaa tuloksen.
    ///
    /// # Errors
    /// - [`SandboxError::Setup`](crate::SandboxError::Setup) jos pyyntö on
    ///   kelvoton tai moduulin lataus epäonnistuu.
    /// - [`SandboxError::Capability`](crate::SandboxError::Capability)
    ///   kyvykkyysrikkomuksesta.
    /// - [`SandboxError::FuelExhausted`](crate::SandboxError::FuelExhausted)
    ///   jos polttoaine loppuu.
    /// - [`SandboxError::Execution`](crate::SandboxError::Execution) muusta
    ///   suoritusvirheestä.
    /// - [`SandboxError::NotImplemented`](crate::SandboxError::NotImplemented)
    ///   jos backend ei tue suoritusta (esim. oletus-`NoopSandbox`).
    fn execute(&self, request: &SandboxRequest) -> SandboxResult;

    /// Backendin tunniste lokitusta ja diagnostiikkaa varten.
    ///
    /// Oletustoteutus palauttaa `"unknown"`; konkreettiset backendit
    /// ylikirjoittavat tämän (esim. `"noop"`, `"wasmtime"`).
    fn backend_name(&self) -> &'static str {
        "unknown"
    }

    /// Voiko tämä backend oikeasti ajaa koodia.
    ///
    /// Oletuksena `true`. [`NoopSandbox`](crate::NoopSandbox) palauttaa
    /// `false`, jotta kutsuja voi tarkistaa tilanteen ennen suoritusta.
    fn can_execute(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capability;

    #[test]
    fn request_defaults_are_safe() {
        let req = SandboxRequest::new(vec![0x00, 0x61, 0x73, 0x6d]);
        assert_eq!(req.fuel_limit, FuelLimit::default());
        assert!(req.capabilities.is_empty());
        assert!(req.validate().is_ok());
    }

    #[test]
    fn request_builder_sets_fields() {
        let caps = CapabilitySet::deny_all().with(Capability::network("h"));
        let req = SandboxRequest::new(vec![1, 2, 3])
            .with_fuel_limit(FuelLimit::limited(42))
            .with_capabilities(caps.clone());
        assert_eq!(req.fuel_limit, FuelLimit::limited(42));
        assert_eq!(req.capabilities, caps);
        assert_eq!(req.code, vec![1, 2, 3]);
    }

    #[test]
    fn request_validate_rejects_empty_code() {
        let req = SandboxRequest::new(Vec::<u8>::new());
        let err = req.validate().expect_err("empty code must fail");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn request_validate_rejects_bad_capabilities() {
        let bad = CapabilitySet::deny_all().with(Capability::network("  "));
        let req = SandboxRequest::new(vec![1]).with_capabilities(bad);
        assert!(req.validate().is_err());
    }

    #[test]
    fn output_string_lossy_handles_invalid_utf8() {
        let out = SandboxOutput::new(vec![0xff, 0xfe], 10);
        // Ei panikoi, korvaa kelvottomat tavut.
        let s = out.output_string_lossy();
        assert!(!s.is_empty());
    }

    #[test]
    fn output_string_lossy_decodes_valid_utf8() {
        let out = SandboxOutput::new(b"hello".to_vec(), 5);
        assert_eq!(out.output_string_lossy(), "hello");
        assert_eq!(out.fuel_consumed, 5);
    }

    #[test]
    fn output_serde_roundtrip() {
        let out = SandboxOutput::new(b"data".to_vec(), 99);
        let json = serde_json::to_string(&out).expect("serialize");
        let back: SandboxOutput = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(out, back);
    }

    // Pieni testidouble joka todistaa että trait on objekti-turvallinen ja
    // oletusmetodit toimivat.
    struct EchoSandbox;
    impl CodeSandbox for EchoSandbox {
        fn execute(&self, request: &SandboxRequest) -> SandboxResult {
            request.validate()?;
            Ok(SandboxOutput::new(request.code.clone(), 1))
        }
    }

    #[test]
    fn trait_is_object_safe_and_defaults_apply() {
        let sandbox: Box<dyn CodeSandbox> = Box::new(EchoSandbox);
        assert_eq!(sandbox.backend_name(), "unknown");
        assert!(sandbox.can_execute());
        let req = SandboxRequest::new(vec![7, 8, 9]);
        let out = sandbox.execute(&req).expect("echo ok");
        assert_eq!(out.output, vec![7, 8, 9]);
        assert_eq!(out.fuel_consumed, 1);
    }
}
