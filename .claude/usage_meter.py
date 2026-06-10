#!/usr/bin/env python3
"""
usage_meter.py — lukee the operator Claude Max -tilauksen TODELLISEN käyttörajan
Anthropic-API:n rate-limit-headereista (OAuth-token ~/.claude/.credentials.json).

Todennettu 2026-06-11: headerit täsmäävät Plan-usage-UI:hin:
  anthropic-ratelimit-unified-5h-utilization  -> session-% (0..1)
  anthropic-ratelimit-unified-7d-utilization  -> VIIKKO-% (0..1)
  anthropic-ratelimit-unified-7d-reset        -> viikkorajan reset (unix s)
  anthropic-ratelimit-unified-7d-status       -> allowed | rejected

Käyttö:
  from usage_meter import read_usage
  u = read_usage()   # {'week_util':0.31,'session_util':0.6,'week_reset':..., 'allowed':True, ...}
"""
import json
import time
import urllib.request
import urllib.error
from pathlib import Path

CRED = Path.home() / ".claude" / ".credentials.json"
API = "https://api.anthropic.com/v1/messages"


def _token():
    cred = json.load(open(CRED, encoding="utf-8"))
    return cred["claudeAiOauth"]["accessToken"]


def read_usage(timeout=30):
    """Returns dict with real utilization, or {'error': ...}. Never prints the token."""
    body = json.dumps({
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 1,
        "messages": [{"role": "user", "content": "."}],
    }).encode()
    req = urllib.request.Request(API, data=body, method="POST")
    req.add_header("authorization", f"Bearer {_token()}")
    req.add_header("anthropic-version", "2023-06-01")
    req.add_header("anthropic-beta", "oauth-2025-04-20")
    req.add_header("content-type", "application/json")
    try:
        resp = urllib.request.urlopen(req, timeout=timeout)
        h = dict(resp.headers)
    except urllib.error.HTTPError as e:
        h = dict(e.headers)
        # 429 still carries the headers we need.
        if not h:
            return {"error": f"http {e.code}", "allowed": False}
    except Exception as e:
        return {"error": str(e), "allowed": None}

    def f(name, cast=float, default=None):
        v = h.get(f"anthropic-ratelimit-unified-{name}")
        if v is None:
            return default
        try:
            return cast(v)
        except Exception:
            return default

    return {
        "week_util": f("7d-utilization", float, None),
        "session_util": f("5h-utilization", float, None),
        "week_reset": f("7d-reset", int, None),
        "session_reset": f("5h-reset", int, None),
        "week_status": f("7d-status", str, None),
        "session_status": f("5h-status", str, None),
        "overall_status": f("status", str, None),
        "allowed": (f("status", str, "") or "").lower() == "allowed",
    }


if __name__ == "__main__":
    u = read_usage()
    if "error" in u and u.get("week_util") is None:
        print("ERROR:", u["error"])
    else:
        wk = u.get("week_util")
        ss = u.get("session_util")
        reset = u.get("week_reset")
        mins = int((reset - time.time()) / 60) if reset else None
        print(f"VIIKKO: {wk*100:.1f}% käytetty" if wk is not None else "VIIKKO: ?")
        print(f"SESSION: {ss*100:.1f}% käytetty" if ss is not None else "SESSION: ?")
        print(f"viikkoreset: ~{mins} min" if mins is not None else "")
        print(f"status: {u.get('overall_status')}")
