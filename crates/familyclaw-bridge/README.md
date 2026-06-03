# familyclaw-bridge

Siltakerros (KERROS A / OSS) FamilyClaw-alustalle: **agenttirekisteri,
tehtävätaulu ja tapahtumaväylä** puhtaana, kuljetuskerroksesta riippumattomana
Rust-rajapintana.

> Design §3 — *"käytä olemassa olevaa"*. Tämä crate mallintaa olemassa olevan
> `family-bridge`-MCP:n semantiikan natiivina Rustina. MCP- ja HTTP-adapterit
> kytketään myöhemmin erikseen — tämä crate ei sisällä kuljetuskerrosta.

## Mitä tämä tarjoaa

| Osa | Vastuu |
|-----|--------|
| `AgentRegistry` | `register` / `list` / `get` / `deregister`, `heartbeat`, liveness-tila aikakatkaisulla |
| `Task` + `TaskStatus` | tehtävä ja sen tilakone (`Pending` → `Active`/`Handed` → `Done`) |
| `TaskBoard` | `create` / `update_status` / `handoff` / `assign`, suodattava listaus |
| `EventBus` + `Event` | fan-out publish/subscribe (`tokio::sync::broadcast`) |
| `FamilyBridge` | koostaa edellä mainitut ja julkaisee tapahtumat tilamuutoksista |

## Liveness

Agentti on `Online` kun sen viimeisin heartbeat on tuoreempi kuin rekisterin
aikakatkaisu (oletus 30 s), `Offline` kun se on vanhentunut, ja `Unknown` jos
heartbeatia ei ole vielä saatu. Nykyhetki annetaan aina parametrina
(`liveness_at(id, now)`), jotta logiikka on deterministinen ja testattava.

## Handoff-säännöt

`TaskBoard::handoff(task, from, to)` onnistuu vain kun:

- `from` on tehtävän nykyinen vastuuagentti,
- `from != to`,
- tehtävä ei ole terminaalitilassa (`Done`).

Onnistuessa `assignee` vaihtuu `to`:ksi ja tila siirtyy `Handed`:iin, josta
vastaanottaja voi siirtää sen `Active`:een.

## Suunnitteluperiaatteet

- Tokio-pohjainen, säieturvallinen (`Arc<RwLock<…>>` / `broadcast`); julkisivut
  ovat `Clone` ja jakavat tilansa.
- Ei `unwrap()` / `expect()` / `panic!()` tuotantopolulla — kaikki virheet
  kulkevat `familyclaw_core::Result`-tyypin kautta.
- OSS-raja: ei kovakoodattuja sieluja, avaimia, tokeneita, IP-osoitteita
  eikä henkilökohtaisia polkuja. Tyypit ovat geneerisiä (`agent_a`, `agent_b`).
