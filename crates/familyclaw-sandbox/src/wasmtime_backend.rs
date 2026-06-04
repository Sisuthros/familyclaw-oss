//! Oikea wasmtime-pohjainen sandbox-toteutus.
//!
//! **Käännetään vain `wasmtime`-featuren kanssa.** Tämä moduuli kytkee
//! [`CodeSandbox`]-rajapinnan wasmtimen ajoaikaan: polttoaine (fuel) pakottaa
//! suorituskaton ja kyvykkyysmalli rajaa pääsyn. wasmtime on iso riippuvuus
//! (Cranelift + JIT), joten se on optional ettei se hidasta workspacen buildia
//! kun sandboxia ei tarvita.
//!
//! ## Suorituskonventio
//! Ajettavan WASM-moduulin tulee viedä (export) parametriton funktio nimeltä
//! [`WasmtimeSandbox::ENTRY_POINT`] joka palauttaa `i32`-tilakoodin. Moduuli
//! ajetaan ilman host-importteja: koska kyvykkyydet (verkko, FS) eivät ole
//! oletuksena käytössä, importteja vaativa moduuli hylätään selkeällä
//! [`SandboxError::Setup`]-virheellä. Tämä on tietoinen turvalinja —
//! laajennetut WASI-kyvykkyydet lisätään myöhemmin kyvykkyysmallin ohjaamana.

use wasmtime::{Config, Engine, Instance, Module, Store, Trap};

use crate::error::SandboxError;
use crate::sandbox::{CodeSandbox, SandboxOutput, SandboxRequest, SandboxResult};

/// wasmtime-pohjainen [`CodeSandbox`]-toteutus.
///
/// Yksi instanssi kapseloi jaetun [`Engine`]:n (joka pitää sisällään
/// käännetyn koodin välimuistin) ja on `Send + Sync`, joten se voidaan jakaa
/// busin actorien välillä.
#[derive(Clone)]
pub struct WasmtimeSandbox {
    engine: Engine,
}

impl std::fmt::Debug for WasmtimeSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `wasmtime::Engine` ei toteuta Debugia, joten näytetään vain tyyppi.
        f.debug_struct("WasmtimeSandbox").finish_non_exhaustive()
    }
}

impl WasmtimeSandbox {
    /// Pakollisen vietävän funktion nimi (parametriton, palauttaa `i32`).
    pub const ENTRY_POINT: &'static str = "run";

    /// Luo uuden wasmtime-sandboxin polttoainemittaus käytössä.
    ///
    /// # Errors
    /// [`SandboxError::Setup`] jos wasmtime-engineä ei voida alustaa annetulla
    /// konfiguraatiolla.
    pub fn new() -> crate::Result<Self> {
        let mut config = Config::new();
        // Polttoainemittaus pakottaa suorituskaton — ydinturvaominaisuus.
        config.consume_fuel(true);
        // Natiivi unwind-info: tarvitaan jotta trap-unwinding (mm.
        // polttoaineen loppuminen) toimii oikein eikä laukaise
        // __fastfail-aborttausta Windowsilla.
        config.native_unwind_info(true);
        // Ei guest-backtraceja: sandbox ei paljasta epäluotetun koodin
        // pinokuvaa ja keventää kustannusta.
        config.wasm_backtrace(false);
        let engine =
            Engine::new(&config).map_err(|e| SandboxError::setup(format!("engine init: {e}")))?;
        Ok(Self { engine })
    }

    /// Sisäänpääsy jaettuun [`Engine`]:iin (esim. moduulien esikääntämiseen).
    #[must_use]
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

impl CodeSandbox for WasmtimeSandbox {
    fn execute(&self, request: &SandboxRequest) -> SandboxResult {
        // 1) Validoi pyyntö (koodi ei tyhjä, kyvykkyydet hyvinmuodostetut).
        request.validate()?;

        // 2) Käännä moduuli annetusta WASM-tavukoodista. `Module::new` on
        //    turvallinen (toisin kuin `deserialize`), joten unsafe-kieltoa ei
        //    rikota.
        let module = Module::new(&self.engine, &request.code)
            .map_err(|e| SandboxError::setup(format!("module compile: {e}")))?;

        // 3) Turvalinja: importteja vaativaa moduulia ei ajeta. Ilman
        //    myönnettyjä kyvykkyyksiä host ei tarjoa mitään, joten import
        //    jäisi linkittämättä. Hylätään selkeällä viestillä.
        if module.imports().len() > 0 {
            return Err(SandboxError::setup(
                "module requires host imports, which are not granted by the current \
                 capability set",
            ));
        }

        // 4) Luo store ja aseta polttoainebudjetti. "Rajaton" tarkoittaa
        //    käytännössä u64::MAX (consume_fuel on enginen vaatimuksesta silti
        //    päällä, mutta katto on käytännössä ääretön).
        let mut store = Store::new(&self.engine, ());
        let budget = request.fuel_limit.budget().unwrap_or(u64::MAX);
        store
            .set_fuel(budget)
            .map_err(|e| SandboxError::setup(format!("set fuel: {e}")))?;

        // 5) Instantioi moduuli ilman importteja.
        let instance = Instance::new(&mut store, &module, &[])
            .map_err(|e| SandboxError::setup(format!("instantiate: {e}")))?;

        // 6) Hae sovittu entry-point ja aja se.
        let entry = instance
            .get_typed_func::<(), i32>(&mut store, Self::ENTRY_POINT)
            .map_err(|e| {
                SandboxError::setup(format!("missing entry point `{}`: {e}", Self::ENTRY_POINT))
            })?;

        let fuel_before = store
            .get_fuel()
            .map_err(|e| SandboxError::execution(format!("read fuel: {e}")))?;

        let status = match entry.call(&mut store, ()) {
            Ok(status) => status,
            Err(err) => {
                // Erottele polttoaineen loppuminen muista trapeista.
                if err.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
                    // `required` on vähintään budget+1 (saturating: ei panikoi
                    // vaikka budget olisi u64::MAX rajattomassa tapauksessa,
                    // jota ei käytännössä tapahdu polttoaineen loppuessa).
                    return Err(SandboxError::fuel_exhausted(
                        budget,
                        budget.saturating_add(1),
                    ));
                }
                return Err(SandboxError::execution(format!("trap: {err}")));
            }
        };

        // 7) Laske kulutettu polttoaine.
        let fuel_after = store
            .get_fuel()
            .map_err(|e| SandboxError::execution(format!("read fuel: {e}")))?;
        let fuel_consumed = fuel_before.saturating_sub(fuel_after);

        // 8) Pakkaa tilakoodi pikku-endian tavuiksi tulokseen. Laajempi
        //    muistipohjainen output-konventio lisätään kyvykkyysmallin myötä.
        Ok(SandboxOutput::new(
            status.to_le_bytes().to_vec(),
            fuel_consumed,
        ))
    }

    fn backend_name(&self) -> &'static str {
        "wasmtime"
    }

    fn can_execute(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuel::FuelLimit;

    /// Pieni WAT-moduuli joka vie `run`-funktion ja palauttaa annetun arvon.
    fn wat_returning(value: i32) -> Vec<u8> {
        let wat = format!(r#"(module (func (export "run") (result i32) (i32.const {value})))"#);
        wat::parse_str(&wat).expect("valid wat compiles to wasm")
    }

    #[test]
    fn backend_metadata() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        assert_eq!(sandbox.backend_name(), "wasmtime");
        assert!(sandbox.can_execute());
    }

    #[test]
    fn executes_simple_module_and_returns_status() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let req = SandboxRequest::new(wat_returning(7));
        let out = sandbox.execute(&req).expect("simple module runs");
        assert_eq!(out.output, 7_i32.to_le_bytes().to_vec());
        // Jotain polttoainetta kuluu.
        assert!(out.fuel_consumed > 0);
    }

    #[test]
    fn rejects_empty_code() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let req = SandboxRequest::new(Vec::<u8>::new());
        let err = sandbox.execute(&req).expect_err("empty rejected");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn rejects_invalid_wasm() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let req = SandboxRequest::new(vec![0xde, 0xad, 0xbe, 0xef]);
        let err = sandbox.execute(&req).expect_err("garbage rejected");
        assert!(err.to_string().contains("module compile"));
    }

    #[test]
    fn rejects_missing_entry_point() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let wat = r#"(module (func (export "other") (result i32) (i32.const 1)))"#;
        let wasm = wat::parse_str(wat).expect("valid wat");
        let req = SandboxRequest::new(wasm);
        let err = sandbox.execute(&req).expect_err("missing run rejected");
        assert!(err.to_string().contains("entry point"));
    }

    #[test]
    fn rejects_module_with_imports() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let wat = r#"(module
            (import "host" "f" (func))
            (func (export "run") (result i32) (i32.const 0)))"#;
        let wasm = wat::parse_str(wat).expect("valid wat");
        let req = SandboxRequest::new(wasm);
        let err = sandbox.execute(&req).expect_err("imports rejected");
        assert!(err.to_string().contains("host imports"));
    }

    #[test]
    fn infinite_loop_runs_out_of_fuel() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        // Ikuinen silmukka — pitää keskeytyä polttoaineen loppumiseen.
        let wat = r#"(module (func (export "run") (result i32)
            (loop (br 0)) (i32.const 0)))"#;
        let wasm = wat::parse_str(wat).expect("valid wat");
        let req = SandboxRequest::new(wasm).with_fuel_limit(FuelLimit::limited(10_000));
        let err = sandbox.execute(&req).expect_err("infinite loop traps");
        assert!(err.is_fuel_exhausted());
    }

    #[test]
    fn fuel_consumed_scales_with_work() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        // Vähän työtä vs. enemmän työtä → enemmän polttoainetta.
        let light = r#"(module (func (export "run") (result i32) (i32.const 0)))"#;
        let heavy = r#"(module (func (export "run") (result i32)
            (local $i i32)
            (loop $l
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br_if $l (i32.lt_s (local.get $i) (i32.const 1000))))
            (local.get $i)))"#;
        let light_out = sandbox
            .execute(&SandboxRequest::new(wat::parse_str(light).expect("wat")))
            .expect("light runs");
        let heavy_out = sandbox
            .execute(&SandboxRequest::new(wat::parse_str(heavy).expect("wat")))
            .expect("heavy runs");
        assert!(heavy_out.fuel_consumed > light_out.fuel_consumed);
    }

    #[test]
    fn deterministic_fuel_for_same_input() {
        let sandbox = WasmtimeSandbox::new().expect("engine init");
        let code = wat_returning(42);
        let a = sandbox
            .execute(&SandboxRequest::new(code.clone()))
            .expect("run a");
        let b = sandbox.execute(&SandboxRequest::new(code)).expect("run b");
        // Determinismi: sama syöte → sama kulutus + sama tulos (durable-replay).
        assert_eq!(a.fuel_consumed, b.fuel_consumed);
        assert_eq!(a.output, b.output);
    }
}
