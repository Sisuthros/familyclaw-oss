# familyclaw-security

Identiteetin eheys ja ihmisen veto FamilyClaw-alustalle (KERROS A, OSS).

Kaksi turvamekanismia:

1. **Identity-anchorit** (`IdentityAnchor`) — suojattuja, ei-unohtuvia muistoja
   (decay-λ = 0) jotka kantavat olennon identiteettiä.
2. **Ihmiskorjaukset** (`HumanCorrection`) — ihmisen veto: korkein prioriteetti
   muistin haussa, hidas decay (`DecayClass::Slow`).

## Ydin-suunnittelupäätös: identiteetti ON muistissa, EI hashissa

Olennon identiteetti **ei** ole SOUL-sisällön SHA-256-tiivisteessä. Se on niiden
suojattujen anchor-muistojen substraatissa, joita olento ei koskaan unohda
(λ = 0). Tiiviste (`AnchorHash`) on **vain tamper-hälytys** — se varoittaa, jos
ankkuroitu sisältö on muuttunut ankkuroinnin jälkeen, mutta se ei *kanna*
identiteettiä.

Kun peukalointi havaitaan (`IdentityStatus::Tampered`), järjestelmä **ei** menetä
identiteettiä eikä kosketa substraattia — se nostaa hälytyksen ja jättää
anchor-muistot ennalleen. **Substraatti on totuus; hash on vahti.**

## OSS-raja (KERROS A)

Crate on julkaistava. Se ei sisällä perheenjäsenten sieluja, ihmiskorjausten
todellista sisältöä, avaimia, tokeneita, IP-osoitteita eikä henkilökohtaisia
polkuja. Ankkuri tallentaa vain sisällön *tiivisteen* + viittauksen muistoon;
sisältö pysyy KERROS B -profiilissa.

## Julkinen API

| Tyyppi / funktio | Vastuu |
|------------------|--------|
| `IdentityAnchor` | Suojattu ankkuri: `memory_id`, `anchor_hash`, `protected`, `decay`. |
| `IdentityAnchor::verify` | Vertaa nykyistä sisältöä ankkuroituun tiivisteeseen (ei mutatoi). |
| `IdentityStatus` | `Intact` / `Tampered { memory_id, expected, actual }`. |
| `verify_identity` | Tarkistaa joukon ankkureita sisältölähdettä vasten. |
| `AnchorHash` | Validoitu SHA-256-heksatiiviste, vakioaikainen vertailu. |
| `DecayLambda` | Ebbinghaus-λ; `ZERO` = ikuinen ankkuri. |
| `HumanCorrection` | Ihmisen veto: `content`, `priority` (1.0), `decay` (Slow), `applied_at`. |
| `HumanCorrection::wins_against` | Voittaako veto kilpailevan retrieval-pistemäärän (tasapelit ihmiselle). |
| `CorrectionPriority` | Rajattu prioriteetti `0.0..=1.0`; `MAX` = 1.0. |
| `DecayClass` | Nimetty decay-luokka (`Eternal` / `Slow` / `Normal` / `Fast`) → λ. |
| `SecurityError`, `Result` | Craten virhetyyppi (muuntuu `FamilyClawError`:ksi). |
