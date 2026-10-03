"""Helpers for capturing order-mutating fixtures in ANALYZE mode only."""
import json, os, datetime, requests
# requires OA_APIKEY in the environment
import capture_rest as cr
from capture_rest import APIKEY, BASE

HERE = os.path.dirname(os.path.abspath(__file__))
LOG = os.path.join(HERE, "ANALYZER_SESSION.md")
ALLOW_LIVE_TOGGLE = False

def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d %H:%M:%S UTC")

def log(line):
    with open(LOG, "a") as f:
        f.write(f"- {now()} {line}\n")

def mode():
    r = requests.post(BASE + "/api/v1/analyzer", json={"apikey": APIKEY}, timeout=30).json()
    am = r.get("data", {}).get("analyze_mode")
    log(f"POST /api/v1/analyzer (guard) -> analyze_mode={am}")
    return am

def guard():
    am = mode()
    if am is not True:
        raise SystemExit(f"ABORT: analyze_mode={am}; refusing to place orders")

def k(**kw):
    d = {"apikey": APIKEY}; d.update(kw); return d

def P(endpoint, case, body, path=None, note=""):
    import time; time.sleep(0.25)
    b, r = cr.post(endpoint, case, body, path=path, note=note)
    short = json.dumps(cr.redact(b))[:220]
    log(f"POST {path or '/api/v1/'+endpoint} [{case}] -> {r.status_code} {short}")
    return b, r
