# Windows-tuotantokäyttöönotto

FamilyClaw-gatewayn käyttöönotto Windowsilla (Layer B erillään reposta).

Katso myös [`QUICKSTART.md`](QUICKSTART.md) (kehitysdemo) ja [`LAYER_BOUNDARY.md`](LAYER_BOUNDARY.md) (mitä ei koskaan commitoida).

## Esivaatimukset

- Rust 1.85+ ([`rustup`](https://rustup.rs/))
- Git
- PowerShell 5.1+ tai PowerShell 7
- Repo kloonattu esim. `E:\Familyclaw`

Tarkista Rust:

```powershell
rustc --version   # 1.85 tai uudempi
```

## Hakemistorakenne (Layer B)

Nämä polut ovat **paikallisia** — eivät kuulu git-repoon.

| Polku | Käyttö |
|-------|--------|
| `E:\familyclaw-profiles` | Agenttien sielut (`SOUL.md`, `IDENTITY.md`, kalibrointi) |
| `E:\familyclaw-data` | Ajonaikainen data (MVP JSON tai myöhemmin SurrealDB/RocksDB) |

Luo hakemistot kerran:

```powershell
New-Item -ItemType Directory -Force -Path E:\familyclaw-profiles, E:\familyclaw-data
```

Profiilirakenne (esimerkki agentille `agent_alpha`):

```
E:\familyclaw-profiles\
  agent_alpha\
    SOUL.md
    IDENTITY.md
```

Gateway lataa sielun polusta `FAMILYCLAW_PROFILE_DIR\<agent_name>\` kun `FAMILYCLAW_AGENT_NAME` on asetettu (oletus `agent_a`).

## Pakolliset ympäristömuuttujat

Aseta ne istunnossa tai `.env`-tiedostossa **repoon ulkopuolella** (esim. `E:\familyclaw-profiles\.env`).

| Muuttuja | Kuvaus |
|----------|--------|
| `FAMILYCLAW_PROFILE_DIR` | Profiilien juuri → `E:\familyclaw-profiles` |
| `FAMILYCLAW_DATA_DIR` | Pysyvän muistin hakemisto → `E:\familyclaw-data` |
| `TELEGRAM_BOT_TOKEN` | Telegram Bot API -token ([@BotFather](https://t.me/BotFather)) |
| `FAMILYCLAW_GATEWAY_TOKEN` | Jaettu salaisuus tuotantoon (webhook/HTTP-suojaus; Layer B — älä commitoi) |

Lisäksi gateway vaatii Telegram-kanavalle (katso `familyclaw-gateway`):

| Muuttuja | Kuvaus |
|----------|--------|
| `FAMILYCLAW_TELEGRAM_CHANNEL_ID` | Looginen kanavatunniste (esim. `tg-agent_alpha`) |
| `FAMILYCLAW_REPLY_TARGET` | Telegram-chat-id vastauksille (numeerinen) |
| `FAMILYCLAW_AGENT_NAME` | Agentin nimi = profiilikansion nimi (esim. `agent_alpha`) |

LLM-vastaukset (valinnainen mutta suositeltu tuotannossa):

```powershell
$env:FAMILYCLAW_PROVIDERS = "anthropic=https://api.anthropic.com/v1=ANTHROPIC_API_KEY"
$env:ANTHROPIC_API_KEY = "<avaimesi>"
```

Esimerkki istunnon alustuksesta:

```powershell
cd E:\Familyclaw

$env:FAMILYCLAW_PROFILE_DIR = "E:\familyclaw-profiles"
$env:FAMILYCLAW_DATA_DIR    = "E:\familyclaw-data"
$env:FAMILYCLAW_GATEWAY_TOKEN = "<pitkä satunnainen merkkijono>"
$env:TELEGRAM_BOT_TOKEN     = "<bot-token>"
$env:FAMILYCLAW_TELEGRAM_CHANNEL_ID = "tg-agent_alpha"
$env:FAMILYCLAW_REPLY_TARGET = "<chat-id>"
$env:FAMILYCLAW_AGENT_NAME  = "agent_alpha"
$env:FAMILYCLAW_GATEWAY_ADDR = "127.0.0.1:8787"
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
| `E:\familyclaw-data\journal.jsonl` | Durable-journali (crash-replay) |
| `E:\familyclaw-data\memory.json` | `LocalJsonStore` — työmuisti levyllä |

Ilman `FAMILYCLAW_DATA_DIR`:ää muisti on vain prosessin RAM:issa (katoaa uudelleenkäynnistyksessä).

Tämä polku on yksiprosessinen ja turvallinen Windows-kehitykseen — **ei RocksDB LOCK -ristiriitoja**.

### Myöhempi — SurrealDB + RocksDB (The Hearth)

`familyclaw-hearth` tukee SurrealDB 3.x -yhteyttä:

- Kehitys: `mem://`
- Tuotanto (tiedosto): `rocksdb:///E:/familyclaw-data/hearth`

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

**Yksi prosessi kerrallaan** avaa `E:\familyclaw-data` (tai sen RocksDB-alikansion).

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
Copy-Item E:\Familyclaw\familyclaw.toml.example "$configDir\familyclaw.toml"
# Muokkaa: agent.name, channel.telegram, provider — salaisuudet env:iin
```

Tai osoita eksplisiittisesti:

```powershell
$env:FAMILYCLAW_CONFIG = "E:\familyclaw-profiles\familyclaw.toml"
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

## E2E: Telegram + agent_alpha

Kun I1-env on kunnossa ja `E:\familyclaw-profiles\agent_alpha\SOUL.md` on paikallaan:

```powershell
$env:FAMILYCLAW_PROFILE_DIR = "E:\familyclaw-profiles"
$env:FAMILYCLAW_DATA_DIR    = "E:\familyclaw-data"
cargo run -p familyclaw-gateway -- serve
```

Lähetä viesti Telegram-botille → odota vastaus agentin SOUL:lla ja muistilla restartin yli (`memory.json` + `journal.jsonl`).

## Vianetsintä

**`TELEGRAM_BOT_TOKEN must be set`**

- Token puuttuu env:stä tai TOML on tyhjä — aseta env (env voittaa TOML:n).

**Gateway ei vastaa `/readyz`**

- Bus ei käynnistynyt — tarkista lokit; aja `doctor`.

**Muisti tyhjenee uudelleenkäynnistyksessä**

- `FAMILYCLAW_DATA_DIR` ei ollut asetettu → käytä `E:\familyclaw-data`.

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
