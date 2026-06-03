# familyclaw-memory

**Eternal Thread** — FamilyClaw-alustan (KERROS A, OSS) muisti-substraatti.

Antaa olennoille *jatkuvan muistin*: muistot eivät katoa restartissa, vaan
vaimenevat biologisen unohtamiskäyrän (Ebbinghaus) mukaan, vahvistuvat
toistosta ja säilyttävät identiteetti-ankkurit ikuisesti. Ratkaisee perheen
#1 kipupisteen — muistin epäjatkuvuuden — *rakenteena*, ei muistutuksena
(design §2.1).

## Keskeiset tyypit

| Tyyppi | Vastuu |
|--------|--------|
| `Memory` | Yksittäinen muisto: sisältö, VAD-tunnesävy, nimetyt tunteet, tärkeys, vaimennuspolitiikka, elinkaaritila. Rakenna `Memory::builder(...)`. |
| `DecayPolicy` | Unohtamisnopeus (Ebbinghaus λ): `ProtectedCore` (0.0), `Slow` (0.02), `Normal` (0.18), `Fast` (0.5). |
| `ImportanceFactors` | Yhdistelmätärkeys: `emotion·0.45 + identity·0.35 + novelty·0.12 + reinforcement·0.20`. |
| `MemoryStatus` | Elinkaari `Active → Archived → Tombstoned`. |
| `MemoryStore` | Tallennusabstraktio (async). |
| `LocalJsonStore` | Riippuvuusvapaa oletustoteutus (JSON-tiedosto, atominen kirjoitus). |
| `RetrievalContext` / `RetrievalResult` | Haku: avainsana + tunneosuma + retention. |

## Ebbinghaus-retentio

```text
R(t) = e^(-λ · t / S)
```

- `λ` = `DecayPolicy`-vakio (`ProtectedCore` → ei vaimene koskaan),
- `S` = vahvuus, johdettu tärkeydestä (tärkeämpi muisto säilyy pidempään),
- `t` = kulunut aika viimeisestä vahvistuksesta.

`MemoryStore::run_decay` siirtää retentionsa alle kynnyksen pudonneet muistot
elinkaaressa eteenpäin; suojattua ydintä (`ProtectedCore`) ei koskaan siirretä.

## OSS-raja (KERROS A)

Tämä crate on julkaistava. Se **ei** sisällä perheenjäsenten oikeita
muistoja, kalibrointeja, sieluja, API-avaimia, tokeneita, IP-osoitteita eikä
henkilökohtaisia polkuja. Muisti-runko on geneerinen; perheen oikea sisältö
on KERROS B:tä ja ladataan ajonaikaisesti profiilihakemistosta.

## Tuleva työ

- **`Surreal<Any>` (feature-flag):** tuotantotallennus (in-mem dev /
  `RocksDB` prod), sama `MemoryStore`-rajapinta (design §2.3).
- **Vektorihaku:** cosine-similarity / HNSW. Nyt haku on avainsana- +
  tunnepohjainen v1-runko.
