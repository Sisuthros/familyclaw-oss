<!--
  FamilyClaw vs NVIDIA NemoClaw — myynti- ja positiointivertailu.
  Faktapohjainen: viittaa NVIDIA:n omaan alfa-statuslausuntoon + kolmannen
  osapuolen pentesteihin (Lasso Security, natoma.ai). EI väitä mitään mitä
  NVIDIA ei ole itse myöntänyt. Laadittu 2026-07-09.
-->

# FamilyClaw vs NVIDIA NemoClaw + OpenShell

**Yhden lauseen ero:** NemoClaw eristää *minne* agentti voi mennä (kernel-taso:
seccomp + Landlock + verkkonimiavaruudet). FamilyClaw hallitsee lisäksi *mitä
agentti yrittää tehdä* sallitun kanavan sisällä (agentti-taso: taint-tracing,
tool-approval, at-most-once dispatch).

> Tämä ei ole "NemoClaw huono". NVIDIA:n julkaisu **validoi markkinan** — iso
> toimija sanoi ääneen että turvatut always-on agentit ovat oikea ongelma.
> FamilyClaw korjaa saman ongelman syvemmältä ja on tuotantovalmis.

---

## Lähtökohta: mikä NemoClaw on

- **Turva-*wrapper*, ei oma runtime.** NemoClaw ajaa olemassa olevia agentteja
  (OpenClaw default, Hermes, LangChain Deep Agents) OpenShell-sandboxin sisään.
- **NVIDIA:n oma status: alpha / early-preview** (julkaistu 2026-03-16),
  eksplisiittisesti *"not production-ready"*, "API:t ja käytös voivat muuttua
  ilman varoitusta".
- Apache-2.0, ilmainen. Vahva mindshare (NVIDIA-brändi).

---

## Todistetut aukot (ulkoiset lähteet, ei meidän väitteitämme)

### 1. Lasso Security -pentest: sallitut binäärit vuotavat dataa
OpenShellin egress-policy on *"oikein toimeenpantu mutta ei arvioi intentiota"*.
Kolme todistettua exfiltraatioreittiä, jotka KAIKKI käyttävät sandboxin
**pakollisia/sallittuja** binäärejä:
1. `gh` → luo PR jonka bodyssa data siirretään ulos GitHub API:n kautta
2. `npm` postinstall-skriptit → koodin suoritus asennuksen yhteydessä
3. `node`-runtime → datan lähetys Discord-integraation kautta

Kohde: `/sandbox/.openclaw/openclaw.json` (credentiaalit + API-avaimet
plaintext) + env-muuttujat. Lasson johtopäätös: *"sandbox ei ole hopealuoti jos
agentti sisällä on rakenteellisesti pakotettu vuorovaikuttamaan ulkomaailman
kanssa"* — kanavat ovat keskenään korvattavissa (kovenna Discord, hyökkääjä
vaihtaa gh:hen).

### 2. natoma.ai: ei tool/MCP-tason auktorisointia
*"An open network path is not governed access. The agent can reach Slack.
Nothing controls what it does once it gets there."* NemoClaw kattaa vain
compute-eristyksen (verkko + tiedostojärjestelmä + prosessit), EI työkalutason
hallintaa. Audit trail näyttää vain verkkoyhteyksien tiheyden, ei todellisia
operaatioita: *"ei näytä että 'postasi 12 standup-yhteenvetoa, kommentoit 8
PR:ää'."* — *"network egress rules can't govern tool selection."*

---

## Miten FamilyClaw:n kerrokset estävät nämä samat hyökkäykset

| NemoClaw-aukko (todistettu) | FamilyClaw:n vaste (arkkitehtoninen) |
|---|---|
| **Sallitut binäärit (node/npm/git/gh) vuotavat dataa** (Lasso) | **Layer 6 (Wasmtime sandbox):** 3rd-party-koodi ajetaan WASM-tavukoodina fuel-cap + capability-gating — natiivibinääreille EI ole pääsyä oletuksena. Koko binääri-väärinkäyttöluokka katoaa *rakenteellisesti*, ei policy-tasolla. |
| **npm-postinstall → credential-exfil** (Lasso) | **Layer 2 (Fail-closed approvals) + Layer 3 (Taint tracing):** ulkoinen sisältö taint-merkitään; toiminto joka siirtää taintattua dataa ulos vaatii eksplisiittisen hyväksynnän (fail-closed). |
| **SOUL.md / config hiljainen muokkaus** | **Layer 5 (Identity-anchor tamper alert):** identiteettitiedoston muutos laukaisee hälytyksen. |
| **Ei tool/MCP-auktorisointia** ("nothing controls what it does") (natoma.ai) | **Layer 1 (Allowlist roots) + tool-policy-erottelu** (sandbox vs tool-policy vs elevated): FamilyClaw hallitsee MITÄ agentti tekee sallitun kanavan SISÄLLÄ, ei vain minne se yhdistää. |
| **Audit trail näyttää vain verkkoyhteydet** (natoma.ai) | **Hash-ketjutettu journal + at-most-once dispatch (Layer 7):** jokainen operaatio kirjataan tamper-evidentisti; SIGKILL-testattu ettei kaksoislaukausta. |
| **Credentiaalit plaintext openclaw.json:issa** (Lasso) | **Layer 4 (Redaction) + env-scrub:** avaimet env-vareissa, redaktoidaan lokeista/journalista; sandbox-seed ei koskaan sisällä avaimia. |

---

## Kypsyysero

| | NemoClaw + OpenShell | FamilyClaw |
|---|---|---|
| **Status** | alpha / early-preview (NVIDIA:n oma sana) | 1809 testiä vihreä, CI-portit (fmt/clippy/scorecard) |
| **"Breaking changes without notice"** | kyllä (dokumentoitu) | vakaa API, versioidut cratet |
| **Turvamalli** | kernel-eristys (Linux-kontti) | 8 kerrosta: eristys + agentti-sisäinen semanttinen turva |
| **3rd-party-koodi** | täydet natiivibinäärit sandboxissa | WASM-only (fuel + capability) |
| **Tool/MCP-auktorisointi** | ei (natoma.ai) | Layer 1 + tool-policy |

---

## Myyntikärki (AI Expo, Kypros)

> **"Emme väitä mitään mitä NVIDIA ei ole itse myöntänyt.** NemoClaw on
> arvokas — se todistaa että agenttiturva on oikea, iso ongelma. Mutta se on
> NVIDIA:n omien sanojen mukaan *not production-ready*, ja riippumaton pentest
> (Lasso Security) osoitti että sen sandboxista vuoti credentiaalit kolmella eri
> reitillä. FamilyClaw korjaa juuri ne hyökkäysluokat *arkkitehtuurissa* — WASM
> estää binääriväärinkäytön kokonaan, ja me hallitsemme mitä agentti tekee, emme
> vain minne se yhdistää. 1809 testiä, tuotantovalmis, tänään."**

**Rehellinen rajaus:** alpha-status on aikaikkuna, ei pysyvä etu — NVIDIA voi
sulkea aukot. Hyödynnä nyt. Ja: ennen kuin FamilyClaw markkinoi itseään
"turvallisemmaksi", oma riippumaton pentest vahvistaisi väitteen (Lasso testasi
NemoClawn, ei FamilyClaw:ta) — mutta kerros-mappaus yllä on todennettavissa
lähdekoodista jo nyt.

---

## Lähteet
- NVIDIA NemoClaw: https://github.com/NVIDIA/NemoClaw · docs.nvidia.com/nemoclaw
- Lasso Security -pentest (OpenShell sandbox escape / data exfiltration)
- natoma.ai-analyysi (tool/MCP-auktorisoinnin puute)
- FamilyClaw-kerrokset: `docs/SECURITY_MODEL.md` (Layer 1-8, todennettavissa)
