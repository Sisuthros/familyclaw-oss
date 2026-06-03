# familyclaw-dream

**Dreaming — yöllinen muistikonsolidaatio (hippokampus-malli).**

FamilyClaw-alustan (KERROS A, OSS) "uni"-vaihe. Peilaa Anthropicin
Dreaming-mallin (6.5.2026) ja perheen Amplifier-muistiproteesin natiiviksi
muistin huolloksi: yöllinen `DreamCycle` lukee muistit
[`familyclaw-memory`]-tallennuksesta ja ristiriitatiedon durable-journalista,
ja siivoaa muistin viidessä vaiheessa.

## Viisi vaihetta

1. **`merge_duplicates`** — lähes-identtiset muistot yhdistetään yhdeksi
   vahvistetuksi edustajaksi (tunteet + tägit unioidaan, muut haudataan).
   Samankaltaisuus on riippuvuusvapaa Jaccard-sananjoukko.
2. **`drop_contradicted`** — durable-journalin ristiriitaisiksi merkitsemät
   muistot haudataan. Journal on totuuden lähde — unijakso ei arvaa.
3. **`absolutize_dates`** — suhteelliset päiväsanat ("eilen", "tomorrow")
   muutetaan absoluuttisiksi ISO-päivämääriksi (`<sana> (YYYY-MM-DD)`).
   Ratkaisee konkreettisesti "eilen vanhenee" -ongelman.
4. **`consolidate`** — korkean tärkeyden muistot vahvistuvat, matalan
   retention (R < kynnys) muistot arkistoituvat.
5. tuottaa `DreamReport`:n johon jokainen vaihe kirjaa `Reflection`:nsa.

Vaiheet ajetaan kiinteässä järjestyksessä → sama syöte tuottaa saman
raportin (deterministinen, toistettava).

## Identiteetti-ankkurit ovat pyhiä

Mikään vaihe ei koskaan hauta tai arkistoi `ProtectedCore`-muistoa —
identiteetti ei vaimene unessa (anchor λ = 0.0).

## Julkinen API

| Tyyppi / funktio | Vastuu |
|------------------|--------|
| `DreamCycle` | unijakson moottori (`run`, `run_without_journal`) |
| `DreamConfig` | vaiheiden kynnykset + kytkimet |
| `DreamReport` / `Reflection` / `ReflectionKind` | tulosraportti |
| `mark_contradicted` / `contradicted_ids` | ristiriitamerkinnät journaliin |
| `jaccard` / `is_near_duplicate` | tekstisamankaltaisuus |
| `absolutize` / `AbsolutizeResult` | päiväysten absolutisointi |

## Esimerkki

```rust,ignore
use familyclaw_dream::{DreamCycle, DreamConfig};
use familyclaw_memory::{LocalJsonStore, Memory, MemoryStore};
use familyclaw_durable::InMemoryJournal;

let store = LocalJsonStore::in_memory();
store.add(Memory::builder("we shipped the release").build()).await?;
store.add(Memory::builder("we shipped the release").build()).await?; // duplikaatti

let journal = InMemoryJournal::new();
let cycle = DreamCycle::with_config(&store, DreamConfig::default());
let report = cycle.run(&journal, familyclaw_core::time::now()).await?;
assert!(report.merged >= 1);
```

## OSS-raja (KERROS A)

Geneeristä alustakoodia. Ei kovakoodattuja sieluja, kalibrointeja, avaimia,
tokeneita, IP-osoitteita eikä henkilökohtaisia polkuja. Kaikki perhe-spesifit
muistot ja kynnykset annetaan ajonaikaisesti.

[`familyclaw-memory`]: ../familyclaw-memory
