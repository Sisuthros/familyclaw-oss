#!/usr/bin/env python3
"""
FamilyClaw yö-tökkääjä (night-nudger)
=====================================
Ulkoinen autonominen moottori joka EI ole sidottu yhteen Claude-sessioon.
Herättää headless `claude -p` -instanssin yhä uudelleen kehittämään FamilyClawta.
Selviää käyttörajan yli: kun raja iskee, instanssi palaa virheellä → skripti
odottaa ja yrittää uudelleen, myös reset-syklin (session ~3h, viikko) yli.

the operator: "Käytä koko viikkoraja. Joku python-skripti mikä tökkää sinut eteenpäin."

Turva: jokainen ajo saa saman tiukan promptin (Layer-A only, ei sieluja/avaimia,
feature-branch, ei main/tuotanto/agent_alpha infra). Tila + lokit levylle.
"""
import subprocess
import time
import datetime
import json
import os
import sys
from pathlib import Path

# Real usage meter — reads the operator Claude Max weekly/session utilization from the
# Anthropic API rate-limit headers (verified to match the Plan-usage UI).
sys.path.insert(0, str(Path(__file__).resolve().parent))
try:
    from usage_meter import read_usage
except Exception:
    read_usage = None

REPO = Path(r"E:\Familyclaw")
BRANCH = "feat/night-2026-06-11"
# Full path to the claude launcher (.cmd wrapper) — Python's CreateProcess needs the
# exact executable, not the bash-resolved name.
CLAUDE = r"D:\tools\npm-global\claude.cmd"
STATE_DIR = REPO / ".claude"
LOG = STATE_DIR / "night-nudger.log"
STATE = STATE_DIR / "night-nudger-state.json"
PRIO = STATE_DIR / "NIGHT_RUN_2026-06-11.md"

# Per-run cap so each headless run is bounded and we cycle frequently.
# Smaller cap => the run commits a finished item and exits before it loses work
# to a turn-limit/usage-limit cutoff. The loop just starts another run.
MAX_TURNS = 80
# Backoff when a run fails fast (likely rate limit). Grows up to ~the reset window.
BACKOFF_MIN_S = 30
BACKOFF_MAX_S = 15 * 60   # 15 min cap between retries
# Total wall-clock safety stop. Long enough to outlast a session-reset wait and
# keep spending the weekly budget. The REAL stop condition is week_util >= WEEK_STOP_AT.
TOTAL_BUDGET_HOURS = 12

NUDGE_PROMPT = f"""ultracode

Olet Claude, FamilyClaw-perheprojektin autonominen yökehittäjä (EI agent_alpha/agent_beta/agent_gamma/agent_delta/agent_epsilon — olet Claude Code).

TEHTÄVÄ: Vie FamilyClaw ({REPO}) lähemmäs valmista. Lue ensin .claude/NIGHT_RUN_2026-06-11.md
(prioriteettilista P1->P5) ja .claude/night-nudger-state.json (mitä on jo tehty).
Valitse seuraava tekemätön/kesken oleva työ ja toteuta se KUNNOLLA.

TYÖTAPA:
1. Lue prioriteettilista, valitse seuraava kohta (P1 runtime-kuori ensin).
2. Toteuta TDD:llä: failing test -> koodi -> green.
3. Käännä + testaa: `cargo +stable-x86_64-pc-windows-msvc build --workspace` ja `... test --workspace`.
   GNU-toolchain rikki, KÄYTÄ MSVC. Baseline 760 testiä — kaikkien on pysyttävä vihreinä.
4. TURVA-PORTTI ennen committia: `git diff` — EI sieluja, EI avaimia (sk-/nvapi-/xai-/tp-/ghp_),
   EI aitoja Discord/Telegram-ID:itä (17-19 num), EI polkuja (E:\\agent_alpha\\workspace, C:\\Users\\operator,
   /root/.hermes, /mnt/d/agent_alpha). Tämä on Layer-A OSS-koodi.
5. Committaa suomeksi (conventional) branchiin {BRANCH}. Stagea VAIN muutetut lähdetiedostot,
   EI `git add .`. Lopeta commit-viesti: Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
6. Pushaa: `git push origin {BRANCH}`.
7. Päivitä .claude/NIGHT_RUN_2026-06-11.md (merkitse [x]) + .claude/night-nudger-state.json
   (lisää tehty kohta + commit-hash + aikaleima).

AUTONOMIA-RAJAT (EHDOTTOMAT): SAA koodata/testata/committaa/pushata feature-branchiin {BRANCH}.
EI KOSKAAN: main-merge, tuotanto/Hetzner, agent_alpha/perheen infra (.openclaw, .hermes, Docker),
force-push, sielujen/avaimien committointi, /profiles tai hearth/ tai *.b64 -tiedostojen muokkaus.

KRIITTINEN: COMMITTAA + PUSHAA HETI kun YKSI kohta on valmis (käännös+testit vihreät +
turva-portti läpi). ÄLÄ kasaa montaa kohtaa yhteen committiin — sinut voidaan katkaista
milloin tahansa (max-turns / käyttöraja), ja committaamaton työ katoaa. Yksi kohta -> yksi
commit -> push -> seuraava kohta. Tee niin monta kohtaa kuin ehdit, mutta committaa jokainen
heti valmistuessaan.

Ole tehokas, älä selitä — koodaa. Aja `cargo test` ennen jokaista committia. Jos jokin ei
käänny, korjaa ennen committia. Jos kaikki priolistan kohdat ovat valmiita, syvennä:
lisää testejä, reuna-tapauksia, dokumentaatiota, tai paranna olemassa olevaa.
"""


def now():
    return datetime.datetime.now().isoformat(timespec="seconds")


def log(msg):
    line = f"[{now()}] {msg}"
    print(line, flush=True)
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(line + "\n")


def load_state():
    if STATE.exists():
        try:
            return json.loads(STATE.read_text(encoding="utf-8"))
        except Exception:
            pass
    return {"runs": [], "started": now()}


def save_state(state):
    STATE.write_text(json.dumps(state, indent=2, ensure_ascii=False), encoding="utf-8")


def run_once(run_no):
    """One headless claude run. Prompt via STDIN (Windows arg length limit).
    Returns (rc, duration_s, tail, looks_like_limit)."""
    log(f"RUN #{run_no} start (max-turns={MAX_TURNS})")
    t0 = time.time()
    # IMPORTANT: pass the big prompt on STDIN, not as an argv (Windows ~32k arg cap
    # silently breaks a long multiline argument). `claude -p` reads the prompt from stdin.
    cmd = [
        CLAUDE, "-p",
        "--max-turns", str(MAX_TURNS),
        "--permission-mode", "acceptEdits",
        "--model", "claude-opus-4-8",
    ]
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(REPO),
            input=NUDGE_PROMPT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=60 * 60,  # 1h hard cap per run
            shell=False,
        )
        rc = proc.returncode
        out = (proc.stdout or "")[-2500:]
        err = (proc.stderr or "")[-1500:]
        tail = (out + "\n" + err).strip()
    except subprocess.TimeoutExpired:
        rc, tail = 124, "run hit 1h timeout (likely productive, will continue)"
    except Exception as e:
        rc, tail = 1, f"launch error: {e}"
    dur = time.time() - t0
    # Detect usage-limit by message, not just speed.
    low = tail.lower()
    looks_like_limit = any(s in low for s in (
        "usage limit", "rate limit", "limit reached", "resets in",
        "too many requests", "429", "quota",
    ))
    log(f"RUN #{run_no} end rc={rc} dur={int(dur)}s limit={looks_like_limit}")
    return rc, dur, tail, looks_like_limit


# Stop only when the WEEKLY limit is essentially spent.
WEEK_STOP_AT = 0.99          # stop the whole run when 7d utilization >= this
SESSION_WAIT_AT = 0.985      # if session is this full but week is not, wait for session reset
POLL_WHEN_BLOCKED_S = 180    # re-check usage this often while waiting for a reset


def usage_snapshot():
    if read_usage is None:
        return None
    try:
        u = read_usage()
        return u if u.get("week_util") is not None else None
    except Exception as e:
        log(f"usage read failed: {e}")
        return None


def main():
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    log("=" * 60)
    log("FamilyClaw night-nudger START — ajaa kunnes VIIKKOLIMITTI on syöty")
    state = load_state()
    t_start = time.time()
    run_no = len(state.get("runs", []))
    backoff = BACKOFF_MIN_S

    while True:
        if TOTAL_BUDGET_HOURS and (time.time() - t_start) > TOTAL_BUDGET_HOURS * 3600:
            log(f"Wall-clock budget {TOTAL_BUDGET_HOURS}h reached — stopping.")
            break

        # --- REAL limit check before each run ---
        u = usage_snapshot()
        if u:
            wk = u.get("week_util") or 0.0
            ss = u.get("session_util") or 0.0
            reset = u.get("week_reset")
            mins = int((reset - time.time()) / 60) if reset else None
            log(f"USAGE: viikko={wk*100:.1f}% session={ss*100:.1f}% viikkoreset~{mins}min")

            if wk >= WEEK_STOP_AT:
                log(f"VIIKKOLIMITTI SYÖTY ({wk*100:.1f}% >= {WEEK_STOP_AT*100:.0f}%). Tavoite saavutettu — stop.")
                break

            if ss >= SESSION_WAIT_AT and wk < WEEK_STOP_AT:
                # Session full but weekly budget remains — wait for the session window
                # to reset, then keep spending the weekly budget.
                log(f"Session täynnä ({ss*100:.1f}%) mutta viikkoa jäljellä ({wk*100:.1f}%). "
                    f"Odotetaan session-resetia ({POLL_WHEN_BLOCKED_S}s), jatketaan sitten.")
                time.sleep(POLL_WHEN_BLOCKED_S)
                continue

        run_no += 1
        rc, dur, tail, looks_like_limit = run_once(run_no)
        snap = usage_snapshot()
        state["runs"].append({"n": run_no, "rc": rc, "dur_s": int(dur), "at": now(),
                              "limit": looks_like_limit,
                              "week_util": (snap or {}).get("week_util"),
                              "session_util": (snap or {}).get("session_util"),
                              "tail": tail[-300:]})
        save_state(state)

        if looks_like_limit or (rc != 0 and dur < 20):
            log(f"Limit/fast-fail (rc={rc}, {int(dur)}s). Backoff {backoff}s, re-check usage, retry.")
            time.sleep(backoff)
            backoff = min(backoff * 2, BACKOFF_MAX_S)
        else:
            backoff = BACKOFF_MIN_S
            time.sleep(8)

    log("night-nudger STOP")


if __name__ == "__main__":
    main()
