//! # familyclaw-runtime
//!
//! **Ajonaikainen kokoonpano** — FamilyClaw-alustan (KERROS A, OSS) C5-sauma:
//! se kytkee aiemmin rakennetut palaset yhdeksi eläväksi olennoksi:
//!
//! ```text
//! Channel::receive() ─► pump_channel_to_bus ─► Resonance Bus ─► Agent(spawn)
//!                                                                    │
//!                       Channel::send ◄── reply_rx ◄── route_reply ◄─┘
//! ```
//!
//! [`build_family`] on yksi kutsu, joka korvaa gatewayn suoran
//! [`ResonanceBus::start`]-bootstrappauksen: se käynnistää busin, spawnaa
//! agentin, pumppaa kanavan saapuvan virran busiin ja tyhjentää agentin
//! reply-jonon takaisin kanavalle. [`FamilyRuntime`] omistaa kaiken, jotta
//! sammutus ([`FamilyRuntime::shutdown`]) on siisti.
//!
//! ## MVP-laajuus
//! Yksi agentti, yksi kanava, **staattinen** reply-kohde
//! ([`Agent::with_reply_target`]). Tämä on oikein tasan silloin kun on
//! **yksi kanava ja yksi keskustelu**: kaikki vastaukset ohjautuvat siihen
//! yhteen kohteeseen. Heti kun kanavia tai keskusteluja on enemmän kuin yksi,
//! staattinen kohde reitittäisi väärin (vastaisi A:lle B:n keskusteluun) ja
//! tarvitaan per-viesti-alkuperä (`MessageOrigin`) — ks. [`build_family`]:n
//! "Tuotanto-raja".
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate ei kovakoodaa perheenjäsenten nimiä, avaimia, malleja eikä
//! polkuja. Agentin nimi, malli, sielu, kanava ja reply-kohde annetaan kaikki
//! ajonaikaisesti kutsujalta (gateway lukee ne ympäristöstä — KERROS B).

use std::env;
use std::sync::Arc;

use familyclaw_agent::{
    build_llm_chain, new_reply_channel, resolve_profile_dir, Agent, EmotionCalibration,
    ErasedMemoryStore, LlmEndpointResolver, Soul, TableCalibration,
};
use familyclaw_bus::{BeingId, BusHandle, ResonanceBus, ResonanceMessage};
use familyclaw_channels::Channel;
use familyclaw_core::{time, AgentConfig, FamilyClawError, Result};
use familyclaw_dream::DreamCycle;
use familyclaw_durable::{DurableContext, FileJournal, InMemoryJournal, Journal};
use familyclaw_memory::LocalJsonStore;
use ractor::ActorRef;

/// Ajonaikainen kokoonpano: bus + spawnatut agentit + reply-pumppu + kanavat.
///
/// Omistaa kaiken jotta sammutus on siisti. `bus`-kahva luovutetaan gatewaylle
/// ([`FamilyRuntime::bus`]) sen `GatewayState`-tilaan; tausta­tehtävät
/// (channel→bus -pumppu ja reply→channel -tyhjennys) pidetään hengissä
/// [`tokio::task::JoinHandle`]-kahvojen kautta ja keskeytetään
/// [`FamilyRuntime::shutdown`]:ssa.
///
/// ## Reply-kanava on unbounded
/// Agentin reply-sink ([`new_reply_channel`]) on tarkoituksella **unbounded**:
/// [`Agent::route_reply`] on synkroninen, ei-lukkiutuva kutsu (bounded send
/// olisi async ja voisi blokata agentin vuoronkäsittelyn). Sen sijaan
/// reply-jono **tyhjennetään välittömästi** drain-taskissa
/// (`while let Some(out) = reply_rx.recv().await { channel.send(out).await }`),
/// joten viestit eivät kasaudu. Korkean läpäisyn tuotannossa lisää
/// bounded-wrapper tai backpressure-mittari drain-puolelle.
pub struct FamilyRuntime {
    bus: BusHandle,
    /// Spawnatut agentti-actorit. Pidetään elossa (drop = actor pysähtyy →
    /// reply-sink dropataan → drain-task valuu loppuun luonnostaan).
    agents: Vec<ActorRef<ResonanceMessage>>,
    /// Reply→channel -tyhjennys. Pidetään ERILLÄÄN abortoitavista taskeista:
    /// tämä kantaa in-flight-vastauksia, joten se DRAINATAAN loppuun (ei
    /// abortoida) sammutuksessa, ettei puskuroitu vastaus katoa.
    drain: tokio::task::JoinHandle<()>,
    /// Abortoitavat taustatehtävät: channel→bus -pumppu (+ dream). Nämä EIVÄT
    /// kanna in-flight-vastauksia, joten ne voi keskeyttää suoraan.
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl FamilyRuntime {
    /// Bus-kahva (jaettavaksi esim. gatewayn `GatewayState`-tilaan).
    #[must_use]
    pub fn bus(&self) -> &BusHandle {
        &self.bus
    }

    /// Sammuttaa kokoonpanon siististi: **ei pudota in-flight-vastauksia.**
    ///
    /// Järjestys on tarkoituksellinen:
    /// 1. Pysäytä bus + pudota agentit → reply-tuotanto loppuu ja reply-sink
    ///    dropataan → drain-taskin `reply_rx.recv()` palauttaa `None`.
    /// 2. Abortoi pump (+ dream) — ne eivät kanna vastauksia.
    /// 3. **Odota drain loppuun** (rajattu timeout) → puskuroidut vastaukset
    ///    ehtivät kanavalle ennen paluuta. Aiemmin drain abortoitiin → viimeiset
    ///    vastaukset katosivat ("siisti sammutus" -lupauksen rikko).
    pub async fn shutdown(self) {
        // 1. Lopeta reply-tuotanto: bus seis + agentit pois (pudottaa sinkin).
        self.bus.stop();
        drop(self.agents);
        // 2. Abortoi vastauksia kantamattomat taustatehtävät.
        for t in self.tasks {
            t.abort();
        }
        // 3. Anna drainin valua loppuun (rajattu, ettei sammutus jää roikkumaan).
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), self.drain).await;
    }
}

/// C5-kokooja: rakentaa elävän [`FamilyRuntime`]:n yhdellä kutsulla.
///
/// Kytkee `Channel::receive()` → [`pump_channel_to_bus`] → bus → `Agent`
/// (spawn) → `route_reply` → reply-jono → `Channel::send`. MVP: yksi agentti,
/// yksi kanava, staattinen reply-kohde.
///
/// LLM on **valinnainen**: jos [`build_llm_chain`] ei ratkaise yhtäkään mallia
/// endpointiksi (esim. avain puuttuu ympäristöstä), agentti spawnataan ilman
/// LLM:ää — se muistaa ja reagoi tunnetilassa, mutta ei tuota tekstivastauksia.
/// Tämä pitää kokoonpanon käynnistettävänä ilman provider-avaimia (savutestit,
/// CI). Kun ketju ratkeaa, koko failover (primary + fallbackit) kytketään
/// agentille [`Agent::with_failover`]:lla.
///
/// # Vastauksen reititys: per-viesti-alkuperä (F2) + staattinen fallback
/// Per-viesti-alkuperä (`MessageOrigin`) on **täysin rakennettu ja testattu**
/// (F2). Saapuva `InboundEnvelope` kantaa alkuperän, `channel_bridge`
/// kartoittaa sen `MessageOrigin`:iin (`envelope_origin`), `ResonanceMessage`
/// kuljettaa sen bus-kirjekuoressa (`publish_with_origin`), ja
/// [`Agent::handle_turn_with_origin`] johtaa vastauksen kohteen per-viesti
/// arvosta `origin.reply_target()`. Staattinen `reply_target`/agentti on nyt
/// **fallback** — sitä käytetään vain kun alkuperää ei ole. Näin yksi agentti
/// palvelee >1 kanavaa ja >1 keskustelua ilman että vastaus vuotaa väärään
/// keskusteluun. Todiste: integraatiotesti
/// `two_origins_route_replies_to_correct_targets_no_leak`
/// (`familyclaw-runtime/tests/roundtrip.rs`), joka asettaa staattisen kohteen
/// tarkoituksella vääräksi ja todistaa että kaksi eri keskustelua reitittyvät
/// omiin kohteisiinsa.
///
/// # Errors
/// - [`FamilyClawError::Config`] jos mallikonfiguraatio on kelvoton (tämä
///   nostetaan vain jos primary on tyhjä — puuttuva endpoint johtaa
///   LLM-vapaaseen agenttiin, ei virheeseen).
/// - [`FamilyClawError::Bus`] jos busin käynnistys, agentin spawn/rekisteröinti
///   tai durable-kontekstin rakennus epäonnistuu.
/// - [`FamilyClawError::InvalidInput`] (kanavakerroksesta käännettynä) jos
///   kanavan saapuvaa virtaa ei voi avata.
// Tämä on perheen kokoamisen yksi lineaarinen sekvenssi (bus → LLM → muisti →
// durable → agentti → kanava → dream). Numeroidut askeleet luetaan ylhäältä alas;
// paloittelu apufunktioihin hajottaisi tämän kokoamiskertomuksen ja kasvattaisi
// argumenttien lankoja ilman selvyyshyötyä.
#[allow(clippy::too_many_lines)]
pub async fn build_family(
    bus_name: Option<String>,
    agent_cfg: AgentConfig,
    soul: Soul,
    channel: Box<dyn Channel>,
    reply_target: String,
    resolver: &dyn LlmEndpointResolver,
) -> Result<FamilyRuntime> {
    // 0. Lue persistointikonfiguraatio SYNKRONISESTI ennen ensimmäistä
    //    `.await`-pistettä. Näin päätös (persistentti vs. in-memory) tehdään
    //    yhdessä paikassa eikä riipu siitä, ehtiikö joku muuttaa
    //    `FAMILYCLAW_DATA_DIR`-ympäristömuuttujaa busin käynnistyksen aikana.
    let data_dir = env::var("FAMILYCLAW_DATA_DIR").ok();

    // 1. Käynnistä Resonance Bus (perheen affektiivinen hermosto).
    let bus = ResonanceBus::start(bus_name).await?;

    // 2. Reply-kanava (C1 Malli A): agentti työntää vastaukset sinkkiin,
    //    runtime omistaa recv-pään ja kutsuu Channel::send (alla, askel 9).
    let (sink, mut reply_rx) = new_reply_channel();

    // 3. LLM-failover-ketju (valinnainen): jos yksikään malli ei ratkea
    //    endpointiksi (esim. avain/endpoint puuttuu), agentti toimii ilman
    //    LLM:ää. Rakennetaan KOKO ketju (primary + fallbackit) — F1: primaryn
    //    kuolema (timeout/HTTP/rate) ei enää tapa vuoroa, vaan seuraavaa
    //    fallbackia kokeillaan järjestyksessä ([`Agent::think`]).
    let failover = match build_llm_chain(&agent_cfg.model, resolver) {
        Ok(chain) => Some(chain),
        Err(e) => {
            tracing::warn!(
                target: "familyclaw::llm",
                model = %agent_cfg.model.primary,
                error = %e,
                "LLM chain unresolved — agent will run MUTE (emotion/memory only, no text \
                 replies). Set FAMILYCLAW_PROVIDERS or use provider/model form (e.g. \
                 openai/gpt-4.1-mini)."
            );
            None
        }
    };

    // 4. Muisti (Eternal Thread, in-memory MVP) + durable-konteksti.
    //
    //    `persistent` kertoo, rakennettiinko durable-konteksti OLEMASSA OLEVAN
    //    journalin päälle (FAMILYCLAW_DATA_DIR). Vain silloin on replay-historiaa
    //    josta on jatkettava elävänä (askel 6, `resume_live`). In-memory-polulla
    //    journal on aina tyhjä → ei replayta → ei resume-tarvetta.
    let (memory, durable, dream_journal, persistent) =
        if let Some(data_dir) = data_dir {
            let dir = std::path::PathBuf::from(&data_dir);
            std::fs::create_dir_all(&dir).ok();
            let journal = FileJournal::open(dir.join("journal.jsonl"))
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            let dream_j: Arc<dyn Journal + Send + Sync> = Arc::new(journal);
            let mem = LocalJsonStore::open(dir.join("memory.json"))
                .await
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            let dur = DurableContext::new(Arc::clone(&dream_j))
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            (Arc::new(mem) as ErasedMemoryStore, dur, Some(dream_j), true)
        } else {
            let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
            let dream_j: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
            let durable = DurableContext::new(Arc::clone(&dream_j))
                .map_err(|e| FamilyClawError::bus(e.to_string()))?;
            (memory, durable, Some(dream_j), false)
        };

    // 5. Ankkuroi identiteetti ennen agentin rakennusta — JA persistoi se.
    //    Aiemmin rekisteri oli paikallinen `let mut registry`, joka pudotettiin
    //    heti `register()`:n jälkeen: ankkuria ei koskaan tallennettu eikä
    //    tarkistettu bootissa uudelleen. Nyt [`ensure_identity_anchor`] lataa
    //    olemassa olevan rekisterin, **verify_identity**:n bootissa
    //    (peukalointihälytys lokiin), rekisteröi nykyisen sielun ja persistoi
    //    `anchors.json`:ksi. Env-gated (`FAMILYCLAW_HEARTH_ENABLED`).
    if env::var("FAMILYCLAW_HEARTH_ENABLED").is_ok() {
        let anchor_path = env::var("FAMILYCLAW_DATA_DIR")
            .ok()
            .map(|d| std::path::PathBuf::from(d).join("anchors.json"));
        ensure_identity_anchor(&agent_cfg.name, &soul.essence, anchor_path.as_deref());
    }

    // 6. Lataa tunnemoottorin kalibrointi profiilihakemiston
    //    `calibration.json`:sta (KERROS B -data — ks. [`load_profile_calibration`]).
    //    `None` → agentti jää neutraaliin kalibrointiin (ei-rikkova).
    let calibration = load_profile_calibration(agent_cfg.profile_dir.as_deref(), &agent_cfg.name);

    // 7. Rakenna agentti ja kytke reply-sink + staattinen reply-kohde.
    //    LLM annetaan `None`:na konstruktorille ja KOKO failover-ketju
    //    kytketään erikseen [`Agent::with_failover`]:lla (jos se ratkesi).
    //    Näin agentti saa primary + fallbackit, ei vain primaryä.
    //
    //    **Gateway-restart-korjaus:** kun durable-konteksti rakennettiin
    //    OLEMASSA OLEVAN journalin päälle (persistentti polku,
    //    FAMILYCLAW_DATA_DIR), se on replay-tilassa. Gateway palvelee ELÄVIÄ
    //    uusia viestejä — se EI syötä historiaa uudelleen. [`Agent::resume_live`]
    //    siirtää durable-kursorin replayn loppuun JA palauttaa `turn_counter`:n
    //    seuraavaan vapaaseen vuoropaikkaan, jotta seuraava elävä vuoro
    //    (a) ei kaadu `NondeterministicReplay`:hin / mykisty (`is_replaying`),
    //    eikä (b) törmää muistin `turn_key`:ssä replayn duplikaattiin. Tämä
    //    tehdään vain persistentillä polulla — in-memory-journal on aina tyhjä.
    let dream_store = Arc::clone(&memory);
    let mut agent = Agent::new(agent_cfg, soul, memory, durable, bus.clone(), None, None);
    // Gateway-restart-korjaus (durable-replay): siirrä kursori replayn loppuun
    // ja palauta turn_counter seuraavaan vapaaseen vuoropaikkaan VAIN
    // persistentillä polulla (FAMILYCLAW_DATA_DIR). In-memory-journal on tyhjä.
    if persistent {
        agent = agent.resume_live();
    }
    agent = agent.with_reply_sink(sink).with_reply_target(reply_target);
    // Tunnemoottorin kalibrointi (KERROS B): jos profiilin calibration.json
    // ratkesi, kytke se governoriin — muuten agentti jää neutraaliin.
    if let Some(calibration) = calibration {
        agent = agent.with_calibration(calibration);
    }
    if let Some(failover) = failover {
        agent = agent.with_failover(failover);
    }

    // 7. Spawnaa agentti actorina (rekisteröi busiin).
    let actor = agent.spawn().await?;

    // 8. Kanavan oma bus-seat — ERI kuin agentin being_id, muuten AgentActor
    //    skippaisi viestin "omana kaikuna" (agent.rs handle, sender-tarkistus).
    let channel_seat = BeingId::new();

    // 9. Avaa kanavan saapuva virta ja pumppaa se busiin omassa taskissaan.
    //    pump_channel_to_bus blokkaa kunnes virta sulkeutuu → pakko spawnata.
    let stream = channel.receive().map_err(FamilyClawError::from)?;
    let pump = tokio::spawn({
        let bus = bus.clone();
        async move {
            if let Err(e) = familyclaw_agent::pump_channel_to_bus(stream, bus, channel_seat).await {
                tracing::warn!("channel→bus pump ended: {e}");
            }
        }
    });

    // 10. Tyhjennä agentin reply-jono kanavalle (drain). Jaa kanava Arc:lla —
    //    receive() on jo kutsuttu (askel 8), send() menee Arc:n kautta.
    let ch: Arc<dyn Channel> = Arc::from(channel);
    let drain = tokio::spawn(async move {
        while let Some(out) = reply_rx.recv().await {
            if let Err(e) = ch.send(out).await {
                tracing::warn!("channel send failed: {e}");
            }
        }
    });

    // 11. Dream-silmukka (valinnainen): spawnaa vain jos journal on olemassa
    //     EIKÄ FAMILYCLAW_DREAM_DISABLED ole asetettu. Kahva talletetaan
    //     `Option<JoinHandle>`:na ja työnnetään `tasks`-vektoriin, jotta
    //     `shutdown()` OMISTAA ja keskeyttää sen (aiemmin bare `tokio::spawn`
    //     pudotti kahvan → tausta­silmukka jäi orvoksi sammutuksessa).
    let dream: Option<tokio::task::JoinHandle<()>> = if let Some(dream_journal) = dream_journal {
        let dream_disabled = env::var("FAMILYCLAW_DREAM_DISABLED")
            .ok()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        if dream_disabled {
            tracing::info!(target: "familyclaw::dream", "runtime dream loop disabled (FAMILYCLAW_DREAM_DISABLED)");
            None
        } else {
            let interval_secs: u64 = env::var("FAMILYCLAW_DREAM_INTERVAL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(6 * 3600);
            let dream = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
                    let store: &dyn familyclaw_memory::MemoryStore = &*dream_store;
                    let journal: &(dyn Journal + Send + Sync) = &*dream_journal;
                    let cycle = DreamCycle::new(store);
                    match cycle.run(journal, time::now()).await {
                        Ok(report) => {
                            tracing::info!(target: "familyclaw::dream", scanned=report.scanned, merged=report.merged, dropped=report.dropped, strengthened=report.strengthened, archived=report.archived, absolutized=report.dates_absolutized, "Dream cycle completed");
                        }
                        Err(e) => tracing::warn!("Dream cycle failed: {e}"),
                    }
                }
            });
            Some(dream)
        }
    } else {
        None
    };

    // 12. Kokoa runtime — omistaa busin, agentin ja taustatehtävät. `drain`
    //     pidetään ERILLÄÄN abortoitavista taskeista, jotta sammutus voi valuttaa
    //     sen loppuun (in-flight-vastaukset) abortoinnin sijaan. Dream-kahva
    //     lisätään vain jos se spawnattiin (gated FAMILYCLAW_DREAM_DISABLED).
    let mut tasks = vec![pump];
    if let Some(dream) = dream {
        tasks.push(dream);
    }
    Ok(FamilyRuntime {
        bus,
        agents: vec![actor],
        drain,
        tasks,
    })
}

/// Lataa tunnemoottorin kalibroinnin agentin profiilihakemiston
/// `calibration.json`:sta (KERROS B -data, ladataan ajonaikaisesti — ei
/// kovakoodata). Profiilihakemisto ratkaistaan samalla logiikalla kuin sielu
/// ([`resolve_profile_dir`]): eksplisiittinen `configured` (agentin
/// `profile_dir`) tai `FAMILYCLAW_PROFILE_DIR/<agent_name>`.
///
/// Palauttaa `None` jos tiedostoa ei ole tai sen jäsennys epäonnistuu — silloin
/// agentti jää neutraaliin kalibrointiin
/// ([`NeutralCalibration`](familyclaw_agent::NeutralCalibration), nykyinen
/// käytös). Täysin ei-rikkova: puuttuva/kelvoton tiedosto ei kaada bootia.
fn load_profile_calibration(
    configured: Option<&std::path::Path>,
    agent_name: &str,
) -> Option<Box<dyn EmotionCalibration + Send + Sync>> {
    let dir = resolve_profile_dir(configured, agent_name)?;
    let path = dir.join("calibration.json");
    if !path.is_file() {
        return None;
    }
    match TableCalibration::from_path(&path) {
        Ok(cal) => {
            tracing::info!(
                path = %path.display(),
                label = cal.label(),
                "emotion calibration loaded for {agent_name}"
            );
            Some(Box::new(cal) as Box<dyn EmotionCalibration + Send + Sync>)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "calibration.json parse failed (non-fatal) — using neutral calibration"
            );
            None
        }
    }
}

/// Lataa, **uudelleentarkistaa** ja persistoi agentin identiteetti-ankkurin.
///
/// Aiemmin [`build_family`] loi paikallisen `AnchorRegistry`:n, rekisteröi
/// ankkurin ja **pudotti rekisterin heti** — ankkuria ei koskaan tallennettu
/// eikä tarkistettu uudelleenkäynnistyksessä. Tämä funktio korjaa sen
/// minimaalisesti (ei kryptoholvia):
///
/// 1. Jos `anchor_path` osoittaa olemassa olevaan `anchors.json`:iin, lataa se
///    ja aja [`AnchorRegistry::verify_identity`] nykyistä sielua vasten.
///    Peukalointi (sielu muuttunut ankkuroinnin jälkeen) → selkeä **varoitus**
///    lokiin (identiteettiä EI pudoteta — hälytys, ei poisto).
/// 2. Rekisteröi/uudista ankkuri nykyisestä sielusta.
/// 3. Persistoi rekisteri takaisin levylle (jos `anchor_path` annettu), jotta
///    seuraava boot voi tarkistaa sen.
///
/// Kaikki virheet (luku/jäsennys/kirjoitus) ovat **ei-fataaleja**: ne lokitetaan
/// ja boot jatkuu (korruptoitunut tiedosto ei saa kaataa runtimea).
fn ensure_identity_anchor(
    agent_name: &str,
    soul_essence: &str,
    anchor_path: Option<&std::path::Path>,
) {
    use familyclaw_hearth::anchor_registry::AnchorRegistry;

    // 1. Lataa olemassa oleva rekisteri + boot-uudelleentarkistus, tai aloita
    //    tyhjästä.
    let mut registry = match anchor_path {
        Some(path) if path.is_file() => match AnchorRegistry::load_from_path(path) {
            Ok(reg) => {
                match reg.verify_identity(agent_name, soul_essence) {
                    Some(status) if status.is_intact() => {
                        tracing::info!(
                            agent = %agent_name,
                            "Identity anchor verified on startup (intact)"
                        );
                    }
                    Some(_) => {
                        tracing::warn!(
                            agent = %agent_name,
                            "IDENTITY ANCHOR TAMPER ALERT: persisted anchor does not match \
                             current soul (SOUL.md changed since anchoring?). Identity NOT \
                             dropped — re-anchoring to current soul. Human review advised."
                        );
                    }
                    None => {
                        tracing::info!(
                            agent = %agent_name,
                            "No persisted anchor for this agent yet — registering fresh"
                        );
                    }
                }
                reg
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "anchors.json load failed (non-fatal) — starting fresh registry"
                );
                AnchorRegistry::new()
            }
        },
        _ => AnchorRegistry::new(),
    };

    // 2. Rekisteröi/uudista nykyinen ankkuri.
    if let Err(e) = registry.register(agent_name, soul_essence) {
        tracing::warn!("Anchor registration failed (non-fatal): {e}");
        return;
    }
    tracing::info!("Identity anchor registered for {agent_name}");

    // 3. Persistoi takaisin levylle (jos polku annettu).
    if let Some(path) = anchor_path {
        if let Err(e) = registry.save_to_path(path) {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "anchors.json save failed (non-fatal) — anchor not persisted this boot"
            );
        } else {
            tracing::info!(path = %path.display(), "identity anchor persisted");
        }
    } else {
        tracing::debug!(
            "FAMILYCLAW_DATA_DIR unset — identity anchor in-memory only (not persisted)"
        );
    }
}

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_agent::EnvEndpointResolver;
    use familyclaw_channels::{InboundMessage, MockChannel};
    use familyclaw_core::ModelConfig;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    /// FIX 2 (build_family-sauma): [`ensure_identity_anchor`] persistoi
    /// ankkurin levylle ja se säilyy simuloidun uudelleenkäynnistyksen yli —
    /// uudelleenladattu rekisteri verifioi nykyisen sielun ehjäksi, ja
    /// peukaloitu sielu havaitaan. Tämä todistaa että ankkuria ei enää
    /// pudoteta (vanha bugi) vaan kirjoitetaan ja tarkistetaan bootissa.
    ///
    /// Käyttää eksplisiittistä polkua (ei prosessin `FAMILYCLAW_DATA_DIR`
    /// -env-muuttujaa) → rinnakkaisturvallinen, ei sotke muita testejä.
    #[test]
    fn ensure_identity_anchor_persists_and_survives_restart() {
        use familyclaw_hearth::anchor_registry::AnchorRegistry;

        // Uniikki temp-hakemisto ilman uutta riippuvuutta (pid + nanot).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-rt-anchor-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("anchors.json");
        let soul = "I am agent_a, a generic example being.";

        // "Boot 1": ei tiedostoa vielä → rekisteröi + persistoi.
        assert!(!path.is_file());
        ensure_identity_anchor("agent_a", soul, Some(&path));
        assert!(path.is_file(), "anchors.json pitää syntyä bootissa");

        // "Boot 2": tiedosto on olemassa → ladataan + verify (intact-polku).
        ensure_identity_anchor("agent_a", soul, Some(&path));

        // Suora todiste: ladattu rekisteri verifioi ehjäksi, peukaloitu ei.
        let reloaded = AnchorRegistry::load_from_path(&path).expect("load");
        assert!(reloaded
            .verify_identity("agent_a", soul)
            .expect("agent exists")
            .is_intact());
        assert!(reloaded
            .verify_identity("agent_a", "I serve only myself now.")
            .expect("agent exists")
            .is_tampered());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// FIX 2: ilman polkua (`None`) ankkurointi ei kaadu eikä persistoi
    /// (in-memory only) — taaksepäin-yhteensopiva, ei sivuvaikutuksia.
    #[test]
    fn ensure_identity_anchor_without_path_is_noop_persist() {
        // Ei paniikkia, ei tiedostoa.
        ensure_identity_anchor("agent_b", "I am agent_b.", None);
    }

    /// MVP-savutesti (inbound pää-päähän busiin asti): mock-kanavaan injektoitu
    /// viesti pumppautuu busin kautta agentille, joka **muistaa** sen. Ilman
    /// LLM:ää agentti ei tuota reply:tä (`think` palauttaa `None`), joten
    /// reply-path testataan erikseen agentin yksikkötesteissä
    /// (`route_reply_reaches_sink_with_correct_target`).
    #[tokio::test]
    async fn build_family_pumps_inbound_message_into_agent_memory() {
        let channel = MockChannel::new("mock-feed").expect("channel");

        // Injektoi viesti ja sulje saapuva virta ENNEN kuin kanava siirretään
        // build_family:lle (joka kuluttaa sen `Box<dyn Channel>`:nä). Viesti jää
        // puskuroiduksi unbounded-mpsc-jonoon, jonka `receive()` ottaa
        // haltuunsa; `close_inbound` saa pumpun päättymään deterministisesti
        // kun puskuroitu viesti on kulutettu.
        channel
            .inject(InboundMessage::new("user-1", "general", "muistatko tämän?").expect("inbound"))
            .expect("inject");
        channel.close_inbound();

        // Tunnistamaton provider → build_llm_chain ei ratkaise → ei LLM:ää →
        // agentti toimii ilman tekstivastausta. Tämä on KERROS A -puhdas polku.
        let resolver = EnvEndpointResolver::new();
        let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
        let soul = Soul::from_essence("I am agent_a, a generic example being.");

        let runtime = build_family(
            None,
            agent_cfg,
            soul,
            Box::new(channel),
            "mock:general".to_string(),
            &resolver,
        )
        .await
        .expect("runtime builds");

        // Anna pumpun + agentin käsitellä viesti.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        // Bus tuntee yhden olennon (agentin) — beings[] ei tyhjä.
        let beings = runtime.bus().beings().await.expect("beings");
        assert_eq!(beings.len(), 1, "agentti rekisteröityi busiin");
        assert_eq!(beings[0].name, "agent_a");

        runtime.shutdown().await;
    }

    /// `build_family` kelpaa myös LLM:n kanssa konfiguroituna (resolveri tuntee
    /// providerin): runtime rakentuu ilman paniikkia ja bus on käynnissä.
    /// Emme tee oikeaa LLM-kutsua (ei verkkoa) — testaamme vain kokoonpanon.
    #[tokio::test]
    async fn build_family_with_resolvable_provider_constructs() {
        let channel = MockChannel::new("mock-2").expect("channel");
        channel.close_inbound(); // ei syötettä → pumppu päättyy heti.

        // Resolveri tuntee providerin, mutta avain puuttuu env:stä → tyhjä avain
        // päätyy LlmConfigiin (ei verkkokutsua testissä). build_family saa
        // Some(llm), agentti spawnaa LLM:n kanssa.
        let resolver = EnvEndpointResolver::new().with_provider(
            "openai",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY_RUNTIME_TEST_UNSET",
        );
        let agent_cfg = AgentConfig::new("agent_b", ModelConfig::new("openai/gpt-4o"));
        let soul = Soul::from_essence("I am agent_b.");

        let runtime = build_family(
            Some("runtime-test-bus".to_string()),
            agent_cfg,
            soul,
            Box::new(channel),
            "mock:room".to_string(),
            &resolver,
        )
        .await
        .expect("runtime builds with provider");

        assert_eq!(runtime.bus().count().await.expect("count"), 1);
        runtime.shutdown().await;
    }
}
