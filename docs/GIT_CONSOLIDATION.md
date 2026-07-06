# FamilyClaw — Git-konsolidointikartta

> **Horisontti 1 (MASTERPLAN):** yksi puhdas `main`-linja ennen julkista julkaisua.
> Tämä dokumentti kartoittaa haarat ja worktreet 2026-07-06. **Ei suorita mergejä
> automaattisesti** — toimintaohje operoijalle.

**Nykytila:**

| Mittari | Arvo |
|---------|------|
| Aktiivinen checkout | `feat/expo-commercial-foundation` @ `9bb57d2` |
| `main` @ | `785dc10` |
| Etäisyys expo → main | **+16 committia** |
| Paikallisia haaroja | 13 |
| Worktreet | 2 (`E:/Familyclaw`, `E:/fc-pr50-worktree`) |

---

## Suositeltu kohdelinja

```
main  ←  feat/expo-commercial-foundation  (ensisijainen integraatio)
      ←  fix/hearth-surreal-allfeatures   (jos ei jo expossa)
      ←  feat/phase4-*                    (yksi kerrallaan, konfliktit käsin)
      ←  feat/growth-content-hash-approval (kun expo on mainissa)
```

**Periaate:** `feat/expo-commercial-foundation` on de facto integraatiohaara (expo +
commercial docs + growth approval + semantic recall). Merge se `main`:iin ensin,
aja CI, sitten arvioi yksittäiset +1-commit-haarat.

---

## Haarakartta

### MERGE → `main` (korkea prioriteetti)

| Haara | +main | Sisältö | Toimenpide |
|-------|-------|---------|------------|
| **`feat/expo-commercial-foundation`** | +16 | Expo-demo, COMMERCIAL_OFFER, USERS, growth approval (d7f1d10), semantic recall (Ollama), Discord presence, gateway MAX_TOKENS | **Merge ensin.** Aja `cargo test --workspace --all-features` + `audit-layer-b.sh` ennen mergeä. |
| **`fix/hearth-surreal-allfeatures`** | +1 | `5110634` — SurrealDB `--all-features` clippy-korjaus | Merge jos ei sisälly expo-haaraan (tarkista: `git cherry main feat/expo-commercial-foundation`). STATUS väittää korjauksen v1.2.0:ssa — verifioi duplikaatti. |
| **`feat/track2-web-fetch-skill`** | +2 | `web_fetch` research-skill + leak-test | Todennäköisesti **sisältyy jo** expo-haaraan (actions #49). Cherry-pick vain jos puuttuu. |
| **`docs/unified-roadmap`** | +1 | `fc4be42` unified roadmap synteesi | **Ohita merge** — korvattu `MASTERPLAN.md`:llä (2026-07-06). Poista haara merge jälkeen. |

### MERGE → `main` (keskitaso — Phase 4 pirstaleet)

Nämä ovat todennäköisesti **päällekkäisiä** (sama Phase 4 D5 kill-switch / scheduler).
Älä mergeä kaikkia sokeasti — valitse yksi toteutuspolku:

| Haara | +main | Commit | Aihe |
|-------|-------|--------|------|
| `feat/phase4-scheduler-dream` | +1 | `eeaf5d7` | DreamCycle scheduled task |
| `feat/phase4-gateway-killswitch` | +1 | `f8b0b03` | POST `/tasks/{id}/enabled` |
| `feat/phase4-killswitch-route` | +1 | `0ecf8fb` | Scheduler shared handle kill-switch |
| `feat/phase4-task-enabled` | +1 | `1a20867` | Task enabled flag |
| `feat/phase2-turn-tool-metrics` | +1 | `cc5dcf8` | Prometheus turn/tool metrics |

**Toimenpide:** Listaa diffit `git diff main..<branch> --stat`. Yhdistä yhdeksi
`feat/phase4-consolidated`-PR:ksi tai jätä odottamaan kunnes expo on mainissa.

### MERGE myöhemmin (riippuvuudet)

| Haara | +main | Sisältö | Toimenpide |
|-------|-------|---------|------------|
| `feat/growth-content-hash-approval` | +2 | Event-sourced proposal store + tamper-evident approval (`b204268`) | Merge **expo jälkeen**. Worktree: `E:/fc-pr50-worktree`. |
| `feat/expo-finish-pass` | +1 | `627d41b` expo finish | Todennäköisesti **sisältyy** expo-commercial-foundationiin (sama commit näkyy historiassa). Varmista `git branch --contains 627d41b`. |

### ÄLÄ MERGE (arkistoi / hylkää)

| Haara | +main | Syy |
|-------|-------|-----|
| `agent_gamma-amplifier-v1` | +6 | Layer-B-riski (Kartano-hardening, sielupolut). Vaatii erillisen Layer-B-auditin ennen harkintaa. |
| `release/v1.0.0` | +1 | `6f0e5d7` vanha version bump — **historiallinen**, ei aktiivinen. |
| `docs/unified-roadmap` | +1 | Korvattu MASTERPLAN.md |

### Remote-only (ei paikallista checkoutia)

| Haara | Huomio |
|-------|--------|
| `origin/feat/agent_delta-parity-executors` | Arvioi erikseen — executor-pariteetti |

---

## Worktreet

| Polku | Haara | Toimenpide |
|-------|-------|------------|
| `E:/Familyclaw` | `feat/expo-commercial-foundation` | Päätyöpuu — merge tästä mainiin |
| `E:/fc-pr50-worktree` | `feat/growth-content-hash-approval` | Poista worktree merge jälkeen: `git worktree remove E:/fc-pr50-worktree` |

---

## Suositeltu merge-järjestys (askel askeleelta)

1. **Varmista vihreä portti** nykyisessä expossa:
   ```powershell
   cd E:\Familyclaw
   cargo test --workspace --all-features
   bash scripts/audit-layer-b.sh
   ```

2. **Merge expo → main:**
   ```powershell
   git checkout main
   git merge --no-ff feat/expo-commercial-foundation -m "merge: expo-commercial-foundation — expo, commercial, semantic recall"
   ```

3. **Cherry-pick tai merge** vain ne Phase 4 -haarat jotka eivät ole expossa ja joita tarvitaan.

4. **Merge growth** kun expo on mainissa ja CI vihreä.

5. **Siivoa:**
   ```powershell
   git branch -d feat/expo-finish-pass docs/unified-roadmap release/v1.0.0
   git worktree remove E:/fc-pr50-worktree   # kun growth mergattu
   ```

6. **Päivitä `origin`:**
   ```powershell
   git push origin main
   ```

---

## Konfliktiriskit

| Riski | Haarat | Hallinta |
|-------|--------|----------|
| Phase 4 kill-switch kolminkertainen | gateway-killswitch, killswitch-route, task-enabled | Valitse yksi, hylkää muut |
| web_fetch duplikaatti | track2 vs expo/actions #49 | `git log --all --oneline -- crates/familyclaw-actions/src/skills/web_fetch.rs` |
| surreal fix duplikaatti | fix/hearth vs expo | `git cherry -v main feat/expo-commercial-foundation` |
| Layer B vuoto | agent_gamma-amplifier-v1 | **Älä mergeä** ilman `audit-layer-b.sh` + historia-skannia |

---

## Tavoitetila (Horisontti 1 valmis)

| Mittari | Tavoite |
|---------|---------|
| Aktiivinen linja | `main` = shippattava sisältö |
| Paikalliset haarat | `main` + ≤3 elävää featurea |
| Worktreet | 1 (vain `E:/Familyclaw`) |
| `origin/main` | Synkassa paikallisen mainin kanssa |

---

*Kartoitus: 2026-07-06 · Viittaa [MASTERPLAN.md](../MASTERPLAN.md) Horisontti 1.*
