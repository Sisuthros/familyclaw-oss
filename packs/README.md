# Workflow packs

Evaluation templates that apply FamilyClaw's **at-most-once under crash**
guarantee to a concrete money- or infra-touching story. Each pack is a
30-minute local proof — **not** production credentials, not a hosted SaaS.

| Pack | Story | Demo entrypoint |
|---|---|---|
| [refund-guard](refund-guard/) | Refund / payout must not double-fire after SIGKILL | `scripts/run_demo.ps1` |
| [infra-teardown](infra-teardown/) | Irreversible cloud teardown behind approval | `scripts/run_demo.ps1` |
| [migration-runner](migration-runner/) | Multi-step migration resume without re-apply | `scripts/run_demo.ps1` |

## How to use

From the repo root (Rust toolchain on `PATH`):

```powershell
.\packs\refund-guard\scripts\run_demo.ps1
```

Then open the Reliability Console while experimenting with a live gateway:

```text
http://127.0.0.1:8787/console
```

Copy a pack's `familyclaw.toml.example` to `familyclaw.toml` in your private
config dir if you want that channel-less profile as a starting point — never
commit Layer B secrets. (`familyclaw.toml` is gitignored repo-wide; only the
blanked `.example` files ship.)
