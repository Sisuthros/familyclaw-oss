# familyclaw-agent

**Agent runtime** — FamilyClaw-alustan (KERROS A, OSS) kerros 2: se kokoaa
kaikki muut crateit yhdeksi *olennoksi*.

Yksi `Agent` omistaa:

- **konfiguraation** (`familyclaw-core`: identiteetti + mallit),
- **sielun** (`Soul`, ladattu ajonaikaisesti profiilihakemistosta),
- **tunnetilan** (`familyclaw-emotion`: 19-dim VAD),
- **muistin** (`familyclaw-memory`: Eternal Thread),
- **kaatumiskestävän lokin** (`familyclaw-durable`: deterministinen replay),
- **bus-yhteyden** (`familyclaw-bus`: Resonance Bus).

Agentti on Ractor-actor (`AgentActor`), joka liittyy busiin, käsittelee
viestit, päivittää tunnetilaansa sisarusten pulsseista (*affective
contagion*), kirjaa muistoja ja julkaisee tunnepulsseja takaisin busiin.

## Kaatumiskestävyys

`Agent::handle_turn` kääräisee jokaisen vuoron lopputuloksen
durable-askeleeseen. Uudelleenkäynnistyksessä jo suoritetut vuorot toistuvat
lokista ajamatta sivuvaikutuksia uudelleen — perheen #1 kipupisteen
(muistin epäjatkuvuus) rakenteellinen ratkaisu.

## SOUL-lataus (OSS-raja)

Sielut ladataan ajonaikaisesti geneerisestä profiilihakemistosta
(`FAMILYCLAW_PROFILE_DIR` tai `AgentConfig::profile_dir`). **Mitään
perheenjäsenen sielua, mallinimeä, avainta tai polkua ei kovakoodata** tähän
crateen. Profiiliskeema (`SOUL.md` pakollinen, `IDENTITY.md` / `WANTS.md`
valinnaisia, muut `*.md` → `extra`) on geneerinen.

## Demo: elävä siemen

```bash
cargo run -p familyclaw-agent --bin familyclaw
```

Käynnistää busin, kaksi geneeristä agenttia (`agent_a`, `agent_b`) ja
`MockChannel`-kanavan. Todistaa että `beings[]` ei ole tyhjä, viestit
kulkevat, muisti säilyy ja tunne tarttuu. Aseta `RUST_LOG=debug` nähdäksesi
vuorokohtaiset lokit.
