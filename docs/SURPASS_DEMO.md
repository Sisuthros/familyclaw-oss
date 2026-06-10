# FamilyClaw — Surpass Demo

> **Mitä tämä todistaa:** FamilyClawin *durable crash-replay* + *eternal-thread recall* lyö
> tiedosto-pohjaisen agenttimuistin (OpenClaw/Hermes-tyylinen `MEMORY.md`) **yhdellä komennolla,
> deterministisesti, tavu-tavulta toistettavasti.**

## Rehellisyys ensin

Vertailun "baseline" EI ole elävä OpenClaw- tai Hermes-instanssi. Se on
`markdown-file-baseline` — **competitor-SHAPED model**, joka mallintaa näiden alustojen
*dokumentoidut* vikatilat:

- `MEMORY.md`-puskuri joka **typistää vanhimmat hiljaa** kun bootstrap-budjetti (8) ylittyy
- **ei deterministista crash-replayta** → restart **ajaa sivuvaikutukset uudelleen**
- ei suojattua ydintä / ei decay-politiikkaa → identiteettifaktat typistyvät siinä missä muutkin

Se ei väitä olevansa minkään tuotteen sisäinen toteutus. Artefakti todistaa
*"voittaa competitor-shaped baselinen"*, ei *"voittaa OpenClaw/Hermes -tuotteen"*.

## Aja se itse (yksi komento)

```bash
cargo +stable-x86_64-pc-windows-msvc run -p familyclaw-bench --bin bench -- compare
```

Tuottaa `crates/familyclaw-bench/out/COMPARISON.md` + `docs/COMPARISON.md` (tavu-identtiset).

## Tulos (live-ajettu 2026-06-10, injektoitu kello 2026-06-04T12:00:00Z)

| Subject | Overall |
|---------|---------|
| familyclaw (FamilyClaw) | **PASS** |
| markdown-file-baseline | **FAIL** |

### S1 — Crash Matrix (ratkaiseva ero)

| Mittari | FamilyClaw | Baseline |
|---------|:----------:|:--------:|
| result | **PASS** | **FAIL** |
| side_effect_overcount | **0** | **17** |
| resume_correctness | **1.0** | **0.0** |

FamilyClaw ajetaan **aitona OS-aliprosessina** (`continuity_daemon`-binääri), tapetaan
neljässä eri kaatumispisteessä (BeforeWrite / MidWrite / MidReplay / CorruptedJournal),
käynnistetään uudelleen, ja todistetaan että se ajaa **nolla** sivuvaikutusta uudelleen
(durable journal replay) palautuen täsmälleen kaatumista edeltäneeseen tilaan. Baseline
ajaa 17 sivuvaikutusta uudelleen ja epäonnistuu. Tämä ylittää **aidon prosessirajan** — ei
in-process-kirjastofeikki.

### S2/S4 — Recall restartin yli

| Mittari | FamilyClaw | Baseline |
|---------|:----------:|:--------:|
| subject_recall_hits (S2) | **5** | **0** |
| subject_recall_hits (S4) | **1** | **0** |

Baselinen naiivi muisti typisti faktat → recall palauttaa tyhjän. FamilyClawin
eternal-thread muistaa ne prosessin uudelleenkäynnistyksen yli.

## Mitä tämä on JA mitä se ei ole

**ON:** reprodusoitava, yhden komennon artefakti joka todistaa durable crash-replayn (S1,
aidon prosessirajan yli) + recall-across-restartin. Skeptikko voi ajaa binäärin itse.

**EI VIELÄ:** ajettu elävää kilpailijaa vasten. Seuraava askel = kääriä oikea
OpenClaw/Hermes-runtime saman `Subject`-rajapinnan taakse (sauma on jo suunniteltu
juuri tähän) → silloin crash_matrix tappaa oikean kilpailijaprosessin.

## Bonus: gateway-turvaregressio suljettu

`POST /inject` vaati ennen ei mitään autentikointia (vs OpenClawin `OPENCLAW_GATEWAY_TOKEN`).
Nyt: jos `FAMILYCLAW_GATEWAY_TOKEN` on asetettu, `/inject` vaatii `Authorization: Bearer <token>`
(constant-time-vertailu, ei token-vuotoa lokiin) → 401 ilman, 200 oikealla. `/healthz` + `/readyz`
pysyvät auki (probet). Testattu (401-ilman / 200-oikealla / back-compat-ilman-tokenia).
