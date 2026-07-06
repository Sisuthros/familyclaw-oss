> **SUPERSEDED** — Arkistoitu suunnitelmadokumentti. Aktiivinen strategia: [MASTERPLAN.md](../../MASTERPLAN.md).

---

# FamilyClaw — Kehityssuunnitelma

**Päivitetty:** 2026-06-12
**Laajuus:** `crates/familyclaw-channels` (erityisesti `src/discord.rs`)
**Tila:** Discord-adapteri on luonnosvaiheessa — ei käänny nykyisellään eikä vastaanota viestejä. Tämä suunnitelma vie sen tuotantokuntoon.

---

## 1. Nykytilan analyysi

`discord.rs` toteuttaa `Channel`-rajapinnan Discordille serenity 0.12:lla, feature-gated (`discord`). Rakenne on oikeansuuntainen (KERROS A -säännöt: ei kovakoodattuja arvoja, konfiguraatio ajonaikaisesti), mutta toteutuksessa on **kriittisiä virheitä**, jotka estävät kääntymisen ja toiminnan.

### 1.1 Kriittiset viat (estävät kääntymisen)

| # | Vika | Sijainti | Selitys |
|---|------|----------|---------|
| K1 | Olemattomat importit | rivit 20–23 | `AsyncHttpRef`, `BotAuth`, `MessagingType`, `ReqwestClient`, `Event` eivät ole serenity 0.12:n API:a. HTTP-lähetys tehdään `serenity::http::Http`-tyypillä. |
| K2 | `ChannelId` ei ole tuple struct | rivit 134, 148–149, 186 | Serenity 0.12:ssa `ChannelId(u64)` ja `.0` eivät toimi. Oikein: `ChannelId::new(id)` ja `.get()`. |
| K3 | `EventHandler` ilman `#[async_trait]` | rivi 131 | Traitin async-metodit vaativat `#[serenity::async_trait]`-attribuutin. |
| K4 | `ClientBuilder`-virheet | rivit 105–107 | Metodi on `.event_handler(handler)`, ei `.handler(...)`. Lisäksi `.await` palauttaa `Result<Client, _>`, jota ei käsitellä — `client.shard_manager` kaatuu. |
| K5 | `CreateMessage` on kuluttava builder | rivit 190–191 | `msg.content(&body)` palauttaa uuden arvon, ei muuta paikallaan. Oikein: `let msg = CreateMessage::new().content(body);`. |
| K6 | HTTP-lähetys reqwestillä | rivi 187 | `send_message` vaatii serenityn `Http`-instanssin (`Http::new(&token)`), ei raakaa `reqwest::Client`-oliota. |

### 1.2 Kriittiset toimintaviat (kääntyisi, mutta ei toimisi)

| # | Vika | Sijainti | Selitys |
|---|------|----------|---------|
| T1 | **`receive()` ei koskaan tuota viestejä** | rivit 72, 206–212 | `new()` luo mpsc-parin ja **pudottaa vastaanottimen** (`_inbound_rx`). `receive()` luo kokonaan uuden, irrallisen parin ja palauttaa sen rx-pään — gateway-viestit menevät suljettuun kanavaan eikä kutsuja saa mitään. Tämä on arkkitehtuurivika, ei pelkkä bugi. |
| T2 | `MESSAGE_CONTENT`-intent puuttuu | rivi 103 | Ilman `GatewayIntents::MESSAGE_CONTENT`-intentiä `msg.content` on tyhjä kaikissa guild-viesteissä → kaikki viestit suodattuvat pois rivin 144 tyhjyystarkistuksessa. Intent pitää myös kytkeä päälle Discordin Developer Portalissa. |
| T3 | `shard_manager` tallennetaan mutta ei käytetä | rivit 109–111 | Ei `stop()`/graceful shutdown -toteutusta. Prosessi ei voi sulkea gatewayta hallitusti. |
| T4 | `start()` nielee gateway-virheet | rivit 114–118 | Virhe vain lokitetaan taustatehtävässä. Kutsuja luulee kanavan toimivan, vaikka token olisi väärä. |

### 1.3 Laatu- ja siisteysongelmat

- **Sekakieliset roskakommentit:** rivi 37 (kiinaa: "那么这个稳定的渠道 ID"), rivi 45 (venäjää: "шарнир"), rivit 183 ja 209–210 ("Acquista lock", "klooni tx", "Uudelleenrakennus: returna") — siivottava suomeksi/englanniksi.
- **`send_lock` on tarpeeton:** serenity hoitaa Discordin rate limitit itse; globaali mutex vain serialisoi lähetykset turhaan ja piilottaa rinnakkaisuusongelmat.
- **Uusi HTTP-clientti joka lähetyksellä** (rivi 187): `Http` pitää luoda kerran `new()`:ssä ja jakaa `Arc`:lla.
- **Ei viestin pituusrajan käsittelyä:** Discord katkaisee >2000 merkin viestit virheeseen — pitkät viestit on pilkottava.
- **Testit kattavat vain konstruktorin:** ei testejä viestivirralle, lähetysvirheille tai handlerin suodatuslogiikalle.

---

## 2. Tavoitearkkitehtuuri

```
DiscordChannel
├── http: Arc<Http>                      // luodaan kerran, jaetaan send()-kutsuille
├── target_channel_id: ChannelId
├── inbound_rx: Mutex<Option<UnboundedReceiver<InboundEnvelope>>>
│     // rx säilyy rakenteessa, receive() ottaa sen (take) — kutsuttavissa kerran
├── inbound_tx: UnboundedSender<InboundEnvelope>   // kloonataan handlerille
└── shard_manager: Mutex<Option<Arc<ShardManager>>> // stop() käyttää

start()  → ClientBuilder::new(token, intents).event_handler(h).await?
           → readiness-kanava: odota ready-event TAI ensimmäinen virhe → palauta Result
stop()   → shard_manager.shutdown_all().await
send()   → pilko 2000 merkin paloihin → http.send_message → virhemappaus ChannelErroriin
receive()→ ota talletettu rx; toinen kutsu → ChannelError::invalid_state
```

Keskeinen periaate: **yksi mpsc-pari koko kanavan eliniän ajan.** Tx annetaan gatewaylle, rx luovutetaan `receive()`-kutsujalle. Tämä korjaa T1:n pysyvästi.

---

## 3. Työvaiheet

### Vaihe 1 — Käännösvirheiden korjaus (P0, ~0,5 pv)

1. Korvaa importit oikeilla serenity 0.12 -tyypeillä:
   `serenity::all::{ChannelId, CreateMessage, GatewayIntents, Message, Ready}`,
   `serenity::http::Http`, `serenity::gateway::ShardManager`,
   `serenity::client::{Context, EventHandler}`, `serenity::async_trait`.
2. `ChannelId::new(...)` / `.get()` kaikkiin ID-käsittelyihin (K2).
3. `#[async_trait]` `DiscordHandler`-toteutukseen (K3).
4. `ClientBuilder::new(token, intents).event_handler(handler).await` + `?`-virheenkäsittely → `ChannelError::backend` (K4).
5. `CreateMessage::new().content(body)` kuluttavana builderina (K5).
6. `Http::new(&token)` kerran konstruktorissa, `Arc<Http>` send-kutsuille (K6).
7. **Hyväksymiskriteeri:** `cargo build --features discord` ja `cargo clippy --features discord -- -D warnings` menevät läpi.

### Vaihe 2 — Viestivirran korjaus (P0, ~1 pv)

1. Talleta `inbound_rx` rakenteeseen (`Mutex<Option<UnboundedReceiver<_>>>`); `receive()` tekee `take()` ja palauttaa `MessageStream`in. Toinen kutsu palauttaa selkeän virheen (T1).
2. Lisää `GatewayIntents::MESSAGE_CONTENT` ja dokumentoi moduulikommenttiin, että intent on aktivoitava myös Discord Developer Portalissa (T2).
3. Lisää `ready`-event handleriin: lokita botin nimi ja guild-määrä, signaloi readiness `oneshot`-kanavalla, jotta `start()` palauttaa virheen heti jos token/yhteys on rikki (T4).
4. **Hyväksymiskriteeri:** manuaalitesti oikealla botilla — Discordiin kirjoitettu viesti ilmestyy `receive()`-streamiin ja `send()`-viesti näkyy kanavalla.

### Vaihe 3 — Elinkaari ja kestävyys (P1, ~1 pv)

1. Toteuta `stop()`: `shard_manager.shutdown_all().await`, odota shardien sulkeutuminen, tyhjennä tila (T3).
2. Poista tarpeeton `send_lock`; luota serenityn sisäiseen rate limit -käsittelyyn.
3. Viestin pilkonta: jaa >2000 merkin viestit rivirajoja kunnioittaen useaksi viestiksi.
4. Uudelleenyhdistyminen: serenity hoitaa gateway-reconnectin itse — varmista lokituksella ja dokumentoi; lisää `tracing`-span kanavakohtaisesti.
5. Virhemappaus: erottele `ChannelError::send` (väliaikainen, esim. 429/5xx) ja pysyvät virheet (401/403/404 → konfiguraatiovirhe), jotta ylempi kerros voi päättää uudelleenyrityksistä.

### Vaihe 4 — Testaus ja laatu (P1, ~1 pv)

1. Yksikkötestit handlerin suodatuslogiikalle: väärä kanava-ID ohitetaan, bot-viestit ohitetaan, tyhjä sisältö ohitetaan, validi viesti tuottaa oikean `InboundEnvelope`n (sender, conversation, body, `ChannelKind::Discord`). Eriytä logiikka puhtaaksi funktioksi `fn map_message(msg) -> Option<InboundEnvelope>`, jotta se on testattavissa ilman serenity-kontekstia.
2. Testit viestin pilkonnalle (rajatapaukset: tasan 2000, 2001, monirivinen).
3. Testi: `receive()` toinen kutsu palauttaa virheen; rx saa tx:ään työnnetyt viestit.
4. Siivoa kaikki sekakieliset kommentit (rivit 37, 45, 183, 208–210) — kommenttikieli yhtenäisesti suomi (doc-kommentit) kuten muussa moduulissa.
5. `cargo doc --features discord` ilman varoituksia; doc-esimerkki `DiscordChannel`in käytöstä.

### Vaihe 5 — Jatkokehitys (P2, backlog)

- Liitetiedostojen vastaanotto (`msg.attachments` → envelope-metadata) ja lähetys.
- Vastaukset ketjuun (reply/thread-tuki) `OutboundMessage`-metadatan kautta.
- Useamman kanavan kuuntelu yhdellä gateway-yhteydellä (nyt 1 gateway / kanava — raskas, jos kanavia on monta).
- Komento-/mention-suodatus konfiguroitavaksi (nyt kaikki kanavan viestit välitetään).
- Metriikat: lähetetyt/vastaanotetut viestit, virheet, gateway-katkot (`metrics`-crate).
- Integraatiotesti CI:ssä erillisellä testipalvelimella (feature `discord-integration-tests`, ajetaan vain kun `DISCORD_TEST_TOKEN` on asetettu).

---

## 4. Riskit ja huomiot

| Riski | Vaikutus | Hallinta |
|-------|----------|----------|
| MESSAGE_CONTENT on *privileged intent* | Botti ei saa viestisisältöä ilman portaaliaktivointia; >100 palvelimen botit vaativat Discordin hyväksynnän | Dokumentoi käyttöönotto-ohjeeseen; lokita varoitus jos sisältö on tyhjä mutta viestejä tulee |
| Serenity 0.12 → 0.13+ API-muutokset | Käännösrikko päivityksissä | Lukitse versio `Cargo.toml`issa (`serenity = "0.12"`), päivitys omana taskinaan |
| Gateway-yhteys per kanava | Resurssien tuhlaus monikanavakäytössä | Vaiheen 5 yhteinen gateway; hyväksyttävä 1–2 kanavalla |
| Token lokeihin vuotaminen | Tietoturva | Ei koskaan lokita tokenia; `Debug`-toteutus rakenteelle joka redaktoi tokenin |

## 5. Hyväksymiskriteerit (Definition of Done)

1. `cargo build`, `cargo test`, `cargo clippy -- -D warnings` ja `cargo doc` läpi `--features discord` -lipulla.
2. Kaksisuuntainen viestiliikenne todennettu oikeaa Discord-bottia vasten.
3. `receive()`-stream tuottaa viestit; `stop()` sulkee gatewayn siististi.
4. Ei kovakoodattuja arvoja (KERROS A); token redaktoitu kaikesta lokituksesta.
5. Kommentit yhtenäisellä kielellä, ei jäänteitä generoiduista sekakielisistä pätkistä.

**Arvioitu kokonaistyömäärä:** ~3,5 henkilötyöpäivää (vaiheet 1–4); vaihe 5 erikseen priorisoitavana backlogina. Rinnakkaistettuna agenteille (luku 6) kalenteriaika putoaa arviolta yhteen päivään.

---

## 6. Rinnakkaistettu toteutus — agenttijako

### 6.1 Periaate: miksi tämä voidaan rinnakkaistaa

Vaiheet 1–3 kohdistuvat samaan tiedostoon (`discord.rs`), joten niitä **ei saa jakaa eri agenteille** — kaksi agenttia samassa tiedostossa tuottaa merge-helvetin. Sen sijaan työ jaetaan **tiedosto- ja vastuurajojen mukaan**: yksi tiedosto = yksi omistaja. Rinnakkaisuus syntyy siitä, että apumoduulit, testit, dokumentaatio ja CI ovat eri tiedostoissa ja ne voidaan rakentaa **etukäteen sovittuja rajapintasopimuksia vasten** (luku 6.3) jo ennen kuin ydinkoodi on valmis.

### 6.2 Työraidat ja agenttivalinnat

| Raita | Sisältö | Agentti | Perustelu |
|-------|---------|---------|-----------|
| **A — Ydin** | `discord.rs` täysi uudelleenkirjoitus: vaiheet 1–3 (käännöskorjaukset K1–K6, viestivirta-arkkitehtuuri T1–T4, `stop()`, readiness, virhemappaus) | **Claude Opus 4.8 xHigh -koodausagentti** | Vaativin osuus: serenity 0.12:n API-nyanssit, async-elinkaari, arkkitehtuurikorjaus. Tänne kannattaa laittaa kallein malli — virhe täällä blokkaa kaiken muun. |
| **B — Apumoduulit + yksikkötestit** | Uudet tiedostot: `discord/split.rs` (viestin pilkonta ≤2000 merkkiin) ja `discord/map.rs` (`map_message`-puhdas funktio) + niiden kattavat yksikkötestit. Toteutetaan rajapintasopimuksia (6.3) vasten, ei riipu raidasta A. | **Cursor** | Hyvin rajattu, puhdas Rust-logiikka ilman serenity-riippuvuuksia. Selkeät speksit → keskitason agentti riittää. |
| **C — Dokumentaatio, konfiguraatio, CI** | `Cargo.toml`-featuret ja versiolukot, moduulidokumentaatio, käyttöönotto-ohje (Developer Portal: MESSAGE_CONTENT -intent, botin oikeudet), GitHub Actions -workflow (`build/test/clippy/doc --features discord`), integraatiotestirunko `DISCORD_TEST_TOKEN`-ehdolla | **Google Antigravity** | Ei kosketa Rust-ydinkoodia → nollariski konflikteille. Dokumentti- ja infratyö sopii agentille, jolla on hyvä selain-/tutkimustuki. |
| **D — Ristiinkatselmointi** | Raidan A diffin katselmointi luvun 1 vikalistaa ja DoD-kriteerejä vasten; raidan B testien aukkoanalyysi | **GPT-5.5** (ensisijainen) tai **DeepSeek 4 PRO** | Eri malliperhe kuin toteuttaja → löytää sokeat pisteet, joita saman perheen malli ei kyseenalaista. Halpa vakuutus. |

**DeepSeek 4 PRO varalle:** jos raita B valmistuu nopeasti, DeepSeek voi aloittaa vaiheen 5 backlogista liitetiedostotuen spekkauksen — mutta ei toteutusta ennen kuin A on mergattu.

### 6.3 Rajapintasopimukset (lukitaan ENNEN käynnistystä)

Nämä allekirjoitukset jaetaan kaikille agenteille tehtävänannossa, eikä kukaan saa muuttaa niitä ilman orkestroijan päätöstä:

```rust
// discord/split.rs — raita B toteuttaa, raita A kutsuu
/// Pilkkoo viestin ≤ max_len merkin paloihin rivirajoja kunnioittaen.
/// Takuut: yksikään pala ei ole tyhjä; palat järjestyksessä; max_len ≥ 1.
pub fn split_message(body: &str, max_len: usize) -> Vec<String>;

// discord/map.rs — raita B toteuttaa, raita A kutsuu
/// Muuntaa Discord-viestin envelopeksi. None jos viesti pitää suodattaa
/// (väärä kanava, botti, tyhjä sisältö).
pub fn map_message(
    author_id: u64, author_is_bot: bool,
    channel_id: u64, target_channel_id: u64,
    content: &str,
) -> Option<InboundEnvelope>;

// discord.rs — raita A toteuttaa (Channel-traitin lisäksi)
pub async fn start(&self) -> ChannelResult<()>;  // palaa vasta ready/virhe
pub async fn stop(&self) -> ChannelResult<()>;
```

Huom: `map_message` ottaa primitiivit (ei serenityn `Message`-tyyppiä), jotta raita B ei tarvitse serenity-riippuvuutta ja testit pyörivät ilman feature-lippua.

### 6.4 Suoritusjärjestys ja synkronointipisteet

```
T0  Orkestrointi: tehtävänannot + rajapintasopimukset ulos     [Claude, 30 min]
T0→ Raidat A, B, C käynnistyvät RINNAKKAIN
T1  B ja C valmiit (arvio: B ~2 h, C ~2 h) → D katselmoi B:n
T2  A valmis (arvio: 3–5 h) → D katselmoi A:n diffin
T3  Integraatio: A + B + C yhdistetään, cargo build/test/clippy  [Claude, 1–2 h]
T4  Manuaalinen savutesti oikealla Discord-botilla               [the operator, 15 min]
```

Synkronointisäännöt: raita A saa stubata `split_message`/`map_message`-kutsut kunnes B on mergattu; C:n CI-workflow ajetaan ensimmäisen kerran vasta T3:ssa; mikään raita ei muokkaa toisen raidan tiedostoja.

### 6.5 Tarvitaanko minua (Claude Opus / Cowork) rakennusvaiheisiin?

Suoraan sanottuna: **ei rakennusvaiheisiin** — raidat A–D kattavat kaiken kirjoitustyön, ja tehokkainta on käyttää minua vain siellä, missä konteksti ja arviointikyky ratkaisevat:

1. **T0 — Tehtävänantojen kirjoitus:** muunnan tämän suunnitelman kolmeksi itsenäiseksi, copy-paste-valmiiksi agenttipromptiksi (teen pyynnöstä heti). Tämä on suurin vipuvaikutus: hyvä tehtävänanto puolittaa agenttien harhailun.
2. **T3 — Integraatioportti:** yhdistän raitojen tulokset, ajan käännöksen ja testit, ja teen lopullisen laatuarvion DoD-kriteerejä vasten. Tämä on ainoa vaihe, jossa tarvitaan koko projektin konteksti yhdessä päässä.
3. **Erotuomarina**, jos raidan D katselmointi ja toteuttaja-agentti ovat eri mieltä.

Kaikki muu — itse koodaus, testit, dokumentit — kannattaa ajaa halvemmilla/rinnakkaisilla agenteilla yllä olevan jaon mukaan.
