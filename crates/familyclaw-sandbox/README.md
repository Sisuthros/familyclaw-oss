# familyclaw-sandbox

Eristetty koodisuoritus FamilyClaw-alustalle (KERROS A / OSS).

WASM-pohjainen sandbox jossa **polttoaine (fuel) pakottaa suorituskaton** ja
**kyvykkyysmalli rajaa pääsyn** verkkoon, tiedostoihin ja
ympäristömuuttujiin. Suunnitteluviite: design §2 (turva).

## Periaate: turva oletuksena

- **Deny by default** — ilman eksplisiittisesti myönnettyä `Capability`:ia
  ajettavalla koodilla ei ole verkkoa, tiedostoja eikä ympäristömuuttujia.
- **Polttoaine pakottaa rajan** — ikuinen silmukka keskeytyy
  `SandboxError::FuelExhausted`:lla, ei jää jumiin.
- **Determinismi** — sama `SandboxRequest` tuottaa saman tuloksen (durable-
  replayn edellytys).

## Rakenne

Crate on kerroksittainen, jotta turvalogiikka on testattavissa **ilman**
raskasta wasmtime-riippuvuutta:

| Tyyppi | Vastuu |
|--------|--------|
| `CodeSandbox` (trait) | Backend-riippumaton rajapinta: `execute(&request) -> SandboxResult` |
| `Capability` / `CapabilitySet` | "Deny by default" -kyvykkyysmalli (verkko, FS-luku, env) |
| `FuelLimit` / `FuelMeter` | Polttoainebudjetti ja sen kulutuksen mittaus |
| `SandboxRequest` / `SandboxOutput` | Suorituksen syöte ja tulos (serde-sarjallistuvia) |
| `NoopSandbox` | **Oletustoteutus** — ei aja koodia, palauttaa `NotImplemented` |
| `WasmtimeSandbox` | Oikea wasmtime-toteutus, **`wasmtime`-featuren takana** |

## Feature-flagit

| Feature | Oletus | Vaikutus |
|---------|--------|----------|
| `wasmtime` | ei | Kytkee `WasmtimeSandbox`-toteutuksen. wasmtime on iso riippuvuus (Cranelift + JIT), joten se on optional ettei se hidasta koko workspacen buildia. |

Ilman `wasmtime`-featurea vain `NoopSandbox` on saatavilla, ja
`default_sandbox()` palauttaa sen.

```toml
[dependencies]
familyclaw-sandbox = { version = "0.1", features = ["wasmtime"] }
```

## Käyttö

```rust
use familyclaw_sandbox::{CodeSandbox, SandboxRequest, FuelLimit, Capability, CapabilitySet};

// "Paras saatavilla oleva" backend (wasmtime jos käännetty, muuten noop).
let sandbox = familyclaw_sandbox::default_sandbox()?;

let request = SandboxRequest::new(wasm_bytes)
    .with_fuel_limit(FuelLimit::limited(1_000_000))
    .with_capabilities(
        CapabilitySet::deny_all().with(Capability::read_only_fs("/data")),
    );

let output = sandbox.execute(&request)?;
println!("fuel consumed: {}", output.fuel_consumed);
# Ok::<(), familyclaw_sandbox::SandboxError>(())
```

### wasmtime-backendin suorituskonventio

`WasmtimeSandbox` ajaa WASM-moduulin joka vie (export) parametrittoman
funktion nimeltä `run` joka palauttaa `i32`-tilakoodin. Moduuli ajetaan
**ilman host-importteja** — importteja vaativa moduuli hylätään selkeällä
virheellä, koska kyvykkyydet eivät oletuksena tarjoa host-rajapintaa.
Laajemmat WASI-kyvykkyydet lisätään myöhemmin kyvykkyysmallin ohjaamana.

## OSS-raja (KERROS A)

Tämä crate ei kovakoodaa perheenjäsenten sieluja, API-avaimia, tokeneita,
IP-osoitteita eikä henkilökohtaisia polkuja. Se on geneerinen alustakomponentti.
