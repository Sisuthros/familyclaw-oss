# familyclaw-durable

**Durable substrate — deterministinen replay (crash-proof).**

FamilyClaw-alustan KERROS 1 (design §2.1) ja perheen **#1 kipupisteen —
muistin epäjatkuvuuden — rakenteellinen ratkaisu**. Durable execution tekee
työn jatkuvuudesta *rakenteen*: jos prosessi kaatuu, workflow jatkuu täsmälleen
siitä mihin se jäi, sivuvaikutuksia toistamatta.

## Malli

Journal-pohjainen deterministinen replay (Temporal-/Flawless-malli puhtaana
Rustina; ei wasmtimea tässä vaiheessa):

1. Workflow kääritään askeliin `DurableContext::step`.
2. Jokainen valmistunut askel kirjataan `JournalEntry`:nä append-only
   `Journal`:iin.
3. Uudelleenkäynnistyksessä `DurableContext` rakennetaan samasta journalista, ja
   jo suoritetut askeleet **palautetaan lokista ajamatta niiden sulkimia
   uudelleen** → sivuvaikutukset eivät toistu, tulos on sama.

## Julkinen API

| Tyyppi | Vastuu |
|--------|--------|
| `DurableContext<J>` | `step(name, closure)` -API; replay-kursori, snapshot, finish |
| `Journal` (trait) | append-only loki: `append`, `replay_from`, `snapshot`, `len` |
| `InMemoryJournal` | kestämätön toteutus testaukseen/kehitykseen |
| `FileJournal` | kaatumiskestävä append-only JSONL (`flush` + `fsync`) |
| `JournalEntry`, `EntryKind`, `StepId` | journal-rivit |
| `DurableError`, `Result` | virhetyypit (muuntuvat `FamilyClawError`:ksi) |

## Esimerkki

```rust
use familyclaw_durable::{DurableContext, InMemoryJournal};

// Tuore ajo: suljin ajetaan ja tulos kirjataan lokiin.
let mut ctx = DurableContext::new(InMemoryJournal::new())?;
let greeting: String = ctx.step("greet", || Ok("hello".to_string()))?;

// "Kaatuminen": journal talteen, konteksti uudelleen.
let journal = ctx.finish();
let mut resumed = DurableContext::new(journal)?;

// Replay: askel palautuu lokista — suljinta EI ajeta uudelleen.
let again: String = resumed.step("greet", || Ok("DIFFERENT".to_string()))?;
assert_eq!(again, "hello"); // tallennettu arvo, ei sulkimen uusi arvo
```

## Kaatumiskestävyys

- `FileJournal::append` flushaa ja fsyncaa (`File::sync_all`) ennen paluuta →
  valmistunut askel on levyllä myös äkillisen kaatumisen jälkeen.
- Replay sietää tasan yhden tapauksen: vajaan **viimeisen** rivin (ei
  rivinvaihtoa) joka jäi kesken kirjoituksen kaatuessa. Mikä tahansa *aiempi*
  vioittunut rivi palautuu `DurableError::CorruptEntry`:nä.

## Determinismin invariantti

Koodin täytyy tuottaa samat askeleet (sama nimi, sama järjestys) joka ajolla.
Jos replay-koodi pyytää askeleen jonka nimi ei vastaa journalissa samalla
paikalla olevaa, `step` palauttaa `DurableError::NondeterministicReplay`:n sen
sijaan että jatkaisi hiljaa väärin.

## OSS-raja (KERROS A)

Geneeristä alustakoodia: ei kovakoodattuja sieluja, avaimia, tokeneita,
IP-osoitteita eikä henkilökohtaisia polkuja. Journalin polku annetaan
ajonaikaisesti.
