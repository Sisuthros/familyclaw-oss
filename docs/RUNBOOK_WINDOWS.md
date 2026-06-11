# Windows — julkinen demo ja yksityinen käyttöönotto

1. **Julkinen demo (Kerros A)** — ensin, ei avaimia: [`QUICKSTART.md`](QUICKSTART.md), `scripts/public-demo.ps1`
2. **Yksityinen gateway (Kerros B)** — omat profiilit ja kanavat repoon ulkopuolella: [`LAYER_BOUNDARY.md`](LAYER_BOUNDARY.md)

## Julkinen demo (Kerros A)

Ei Telegramia, ei SOUL-tiedostoja, ei salaisuuksia:

```powershell
cd <repo-root>
powershell -File scripts/public-demo.ps1
powershell -File scripts/public-demo.ps1 -Full   # + compare-bench
```

Tai yksittäinen 10 s -ajo:

```powershell
cargo run -p minimal-gateway -- --duration 10
```

## Esivaatimukset (Kerros B)

- Rust 1.85+ ([`rustup`](https://rustup.rs/))
- Git
- PowerShell 5.1+ tai PowerShell 7
- Repo kloonattu esim. `E:\Familyclaw`

Tarkista Rust:

```powershell
rustc --version   # 1.85 tai uudempi
```

## Hakemistorakenne (Kerros B)

Nämä polut ovat **paikallisia** — valitse omat sijaintisi, eivät kuulu git-repoon.

| Muuttuja / polku | Käyttö |
|------------------|--------|
| `FAMILYCLAW_PROFILE_DIR` | Agenttien profiilit (`SOUL.md`, `IDENTITY.md`) |
| `FAMILYCLAW_DATA_DIR` | Pysyvä data (MVP: `memory.json`, `journal.jsonl`) |

Esimerkki (korvaa omat polut):

```powershell
$profiles = Join-Path $env:USERPROFILE "familyclaw-profiles"
$data     = Join-Path $env:USERPROFILE "familyclaw-data"
New-Item -ItemType Directory -Force -Path $profiles, $data | Out-Null
```

Profiilirakenne (geneerinen `agent_a`):

```
%FAMILYCLAW_PROFILE_DIR%\
  agent_a\
    SOUL.md
    IDENTITY.md
```

Gateway lataa sielun polusta `FAMILYCLAW_PROFILE_DIR\<agent_name>\` kun `FAMILYCLAW_AGENT_NAME` on asetettu (oletus `agent_a`).

Kehityksessä voit alustaa JSON-datan repoon sidottuun `.local/data` (gitignored):

```powershell
powershell -File scripts/init-familyclaw-data.ps1
```

## Pakolliset ympäristömuuttujat (Kerros B)

Kopioi reposta [`.env.example`](../.env.example) yksityiseen polkuun ja täytä arvot:

```powershell
$configDir = Join-Path $env:USERPROFILE ".config" "familyclaw"
New-Item -ItemType Directory -Force -Path $configDir | Out-Null
Copy-Item .env.example "$configDir\familyclaw.env"
# muokkaa familyclaw.env — älä commitoi
. .\scripts\load-env.ps1 -Path "$configDir\familyclaw.env"
```

| Muuttuja | Kuvaus |
|----------|--------|
| `FAMILYCLAW_PROFILE_DIR` | Profiilien juuri |
| `FAMILYCLAW_DATA_DIR` | Pysyvän muistin hakemisto |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot API -token ([@BotFather](https://t.me/BotFather)) |
| `FAMILYCLAW_GATEWAY_TOKEN` | Jaettu salaisuus tuotantoon (webhook/HTTP-suojaus) |

Lisäksi gateway vaatii Telegram-kanavalle:

| Muuttuja | Kuvaus |
|----------|--------|
| `FAMILYCLAW_TELEGRAM_CHANNEL_ID` | Looginen kanavatunniste (esim. `tg-main`) |
| `FAMILYCLAW_REPLY_TARGET` | Telegram-chat-id vastauksille (numeerinen) |
| `FAMILYCLAW_AGENT_NAME` | Profiilikansion nimi (esim. `agent_a`) |

LLM-vastaukset (valinnainen mutta suositeltu tuotannossa):

```powershell
$env:FAMILYCLAW_PROVIDERS = "anthropic=https://api.anthropic.com/v1=ANTHROPIC_API_KEY"
$env:ANTHROPIC_API_KEY = "<avaimesi>"
```

Esimerkki istunnon alustuksesta:

```powershell
cd <repo-root>
. .\scripts\load-env.ps1 -Path "$env:USERPROFILE\.config\familyclaw\familyclaw.env"
```

Kuuntele vain localhostilla oletuksena. Jos avaat portin verkkoon, varmista palomuuri ja `FAMILYCLAW_GATEWAY_TOKEN`.

## Rakenna gateway

```powershell
cd E:\Familyclaw
cargo build --release -p familyclaw-gateway --locked
```

Binääri: `target\release\familyclaw-gateway.exe`

Vaihtoehtoinen debug-ajo ilman erillistä asennusta:

```powershell
cargo build -p familyclaw-gateway
```

## Käynnistä gateway

```powershell
# Esitarkistus (ei käynnistä palvelinta)
cargo run -p familyclaw-gateway -- doctor

# Käynnistys (oletus: serve)
cargo run -p familyclaw-gateway -- serve

# Tai release-binääristä:
.\target\release\familyclaw-gateway.exe serve
```

Terveystarkistukset toisessa ikkunassa:

```powershell
curl.exe -i http://127.0.0.1:8787/healthz
curl.exe -i http://127.0.0.1:8787/readyz
```

Tilan kysely CLI:llä:

```powershell
cargo run -p familyclaw-gateway -- status
```

Sammutus: `Ctrl+C` (gateway kutsuu siistin `shutdown`-polun).

## MVP JSON vs SurrealDB (The Hearth)

### MVP — suositus Windowsille ensin

Kun `FAMILYCLAW_DATA_DIR` on asetettu, runtime käyttää **JSON-tiedostoja**:

| Tiedosto | Sisältö |
|----------|---------|
| `%FAMILYCLAW_DATA_DIR%\journal.jsonl` | Durable-journali (crash-replay) |
| `%FAMILYCLAW_DATA_DIR%\memory.json` | `LocalJsonStore` — työmuisti levyllä |

Ilman `FAMILYCLAW_DATA_DIR`:ää muisti on vain prosessin RAM:issa (katoaa uudelleenkäynnistyksessä).

Tämä polku on yksiprosessinen ja turvallinen Windows-kehitykseen — **ei RocksDB LOCK -ristiriitoja**.

### Myöhempi — SurrealDB + RocksDB (The Hearth)

`familyclaw-hearth` tukee SurrealDB 3.x -yhteyttä:

- Kehitys: `mem://`
- Tuotanto (tiedosto): `rocksdb:///<absolute-path>/hearth`

Rakenna feature-flagilla kun Hearth otetaan käyttöön:

```powershell
cargo build -p familyclaw-hearth --features surreal
```

Identiteetti-ankkuri (valinnainen):

```powershell
$env:FAMILYCLAW_HEARTH_ENABLED = "1"
```

**Älä käytä JSON-MVP:tä ja RocksDB-hearthia samanaikaisesti samassa hakemistossa ilman erillisiä alikansioita.** Pidä MVP JSON juuressa (`memory.json`, `journal.jsonl`) ja hearth erillisessä alikansiossa (`hearth\`).

## RocksDB LOCK — yksi prosessi kerrallaan

RocksDB sallii vain **yhden kirjoittavan prosessin** per tietokantapolku. Tyypillinen virhe:

```
Resource temporarily unavailable
IO error: lock hold by current process
```

### Sääntö

**Yksi prosessi kerrallaan** avaa RocksDB-tietokantapolun.

### Korjaus

1. Sammuta kaikki gateway-instanssit:

```powershell
Get-Process familyclaw-gateway -ErrorAction SilentlyContinue | Stop-Process -Force
```

2. Sulje ohjelmat, jotka pitävät data-kansiota auki (Cursor/VS Code -explorer, toinen terminaali `cargo run`, vanha `continuity_daemon`).

3. **MVP-vaiheessa** käytä vain JSON-polku (`FAMILYCLAW_DATA_DIR` ilman SurrealDB/RocksDB-hearthia) — suositus parallel-agent -suunnitelmassa.

4. Jos LOCK jää roikkuun prosessin kaatumisen jälkeen, tarkista prosessilista uudelleen. Poista `LOCK`-tiedosto **vain** kun varmistat ettei yhtään prosessia käytä kantaa.

5. Älä aja benchmarkia (`continuity_daemon`) ja gatewayta **samaan** data-hakemistoon samanaikaisesti.

## Konfiguraatio (TOML)

Valinnainen TOML täydentää env-muuttujia. Kopioi esimerkki:

```powershell
$configDir = "$env:USERPROFILE\.config\familyclaw"
New-Item -ItemType Directory -Force -Path $configDir
Copy-Item familyclaw.toml.example "$configDir\familyclaw.toml"
# Muokkaa: agent.name, channel.telegram, provider — salaisuudet env:iin
```

Tai osoita eksplisiittisesti:

```powershell
$env:FAMILYCLAW_CONFIG = "$configDir\familyclaw.toml"
```

## Validointi (bench + CI)

Aja ennen tuotantoon viemistä tai mergeä:

```powershell
cd E:\Familyclaw

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings -A clippy::doc_markdown -A clippy::too_many_lines
cargo test --workspace
```

Jatkuvuusbenchmark (vaatii `continuity_daemon`-binäärin — rakennetaan automaattisesti):

```powershell
cargo build -p familyclaw-agent --bin continuity_daemon
cargo run -p familyclaw-bench --bin bench -- all
```

Tulokset:

- `crates\familyclaw-bench\out\scorecard.json`
- `crates\familyclaw-bench\out\SCORECARD.md`
- `docs\SCORECARD.md` (päivittyy `all`-ajossa)

Odotettu yhteenveto: **Overall: PASS** (katso [`SCORECARD.md`](SCORECARD.md)).

Yksittäinen skenaario:

```powershell
cargo run -p familyclaw-bench --bin bench -- s1   # crash matrix
cargo run -p familyclaw-bench --bin bench -- s3   # dream quality
```

## E2E: gateway + Telegram (Kerros B)

Kun yksityinen env on ladattu ja `%FAMILYCLAW_PROFILE_DIR%\agent_a\SOUL.md` on olemassa:

```powershell
. .\scripts\load-env.ps1 -Path "$env:USERPROFILE\.config\familyclaw\familyclaw.env"
.\scripts\e2e-gateway.ps1 -StartGateway
```

Lähetä viesti Telegram-botille → odota vastaus agentin SOUL:lla ja muistilla restartin yli.

## Vianetsintä

**`TELEGRAM_BOT_TOKEN must be set`**

- Token puuttuu env:stä tai TOML on tyhjä — aseta env (env voittaa TOML:n).

**Gateway ei vastaa `/readyz`**

- Bus ei käynnistynyt — tarkista lokit; aja `doctor`.

**Muisti tyhjenee uudelleenkäynnistyksessä**

- `FAMILYCLAW_DATA_DIR` ei ollut asetettu → aja `scripts/init-familyclaw-data.ps1` tai aseta polku env:iin.

**RocksDB LOCK / data-kansio lukittu**

- Katso [RocksDB LOCK](#rocksdb-lock--yksi-prosessi-kerrallaan) — sulje kilpailevat prosessit, käytä JSON-MVP:tä ensin.

**`cargo` puuttuu**

- Asenna Rust: [`rustup.rs`](https://rustup.rs/)

**Testit epäonnistuvat rinnakkain**

```powershell
cargo test --workspace -- --test-threads=1
```

---

FamilyClaw: agentit jotka muistavat, tuntevat, uneksivat ja ajattelevat — Windows-tuotannossa Layer B pysyy yksityisenä.
