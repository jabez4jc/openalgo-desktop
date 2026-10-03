#!/usr/bin/env python3
"""Golden-fixture capture for OpenAlgo REST API (READ-ONLY endpoints only).

Never calls placeorder/modifyorder/cancelorder/closeposition/basket/split/options
orders/GTT mutations/analyzer toggle/telegram/whatsapp/strategy mutations/sip/
portfolio mutations/sandbox mutations.
"""
import json
import os
import re
import sys
import time
from concurrent.futures import ThreadPoolExecutor

import requests

BASE = "http://127.0.0.1:5000"
APIKEY = os.environ["OA_APIKEY"]
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "rest")
EMAIL = os.environ.get("OA_EMAIL", "")

KEEP_HEADERS = (
    "content-type",
    "content-length",
    "content-disposition",
    "retry-after",
    "access-control-allow-origin",
    "server",
)
REDACT_KEYS = {
    "clientid", "client_id", "user_id", "userid", "username", "user_name",
    "email", "login_id", "loginid", "accountid", "account_id", "broker_user_id",
}

INDEX = []  # (endpoint, case, status, note)


def redact_text(s: str) -> str:
    s = s.replace(APIKEY, "<APIKEY>")
    if EMAIL:
        s = s.replace(EMAIL, "<EMAIL>")
    return s


def redact(obj):
    if isinstance(obj, dict):
        out = {}
        for k, v in obj.items():
            if k.lower() in REDACT_KEYS and isinstance(v, (str, int)) and v not in ("", None):
                out[k] = f"<{k.upper()}>"
            else:
                out[k] = redact(v)
        return out
    if isinstance(obj, list):
        return [redact(x) for x in obj]
    if isinstance(obj, str):
        return redact_text(obj)
    return obj


def save(endpoint, case, req, resp, note="", body_override=None):
    d = os.path.join(OUT, endpoint)
    os.makedirs(d, exist_ok=True)
    headers = {k: v for k, v in resp.headers.items()
               if k.lower() in KEEP_HEADERS or k.lower().startswith("x-ratelimit")}
    if body_override is not None:
        body = body_override
    else:
        try:
            body = resp.json()
        except ValueError:
            txt = resp.text
            body = {"_raw_text": txt[:6000], "_raw_text_truncated": len(txt) > 6000, "_raw_len": len(txt)}
    rec = {
        "request": {
            "method": req["method"],
            "path": req["path"],
            "headers": req.get("headers", {}),
            "body": req.get("body"),
        },
        "response": {"status_code": resp.status_code, "headers": headers, "body": body},
    }
    if note:
        rec["note"] = note
    rec = redact(rec)
    path = os.path.join(d, f"{case}.json")
    with open(path, "w") as f:
        json.dump(rec, f, indent=2, ensure_ascii=False)
    INDEX.append((endpoint, case, resp.status_code, note))
    print(f"[{resp.status_code}] {endpoint}/{case}", flush=True)
    return body


def post(endpoint, case, body, path=None, note="", headers=None, raw=None, body_override_fn=None):
    path = path or f"/api/v1/{endpoint}"
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    if raw is not None:
        r = requests.post(BASE + path, data=raw, headers=hdrs, timeout=60)
        req = {"method": "POST", "path": path, "headers": hdrs, "body": raw}
    else:
        r = requests.post(BASE + path, json=body, headers=hdrs, timeout=60)
        req = {"method": "POST", "path": path, "headers": hdrs, "body": body}
    bo = None
    if body_override_fn is not None:
        try:
            bo = body_override_fn(r.json())
        except ValueError:
            bo = None
    return save(endpoint, case, req, r, note, body_override=bo), r


def get(endpoint, case, path, params=None, note="", headers=None, body_override_fn=None):
    hdrs = headers or {}
    r = requests.get(BASE + path, params=params, headers=hdrs, timeout=120)
    full = r.request.path_url
    req = {"method": "GET", "path": full, "headers": hdrs, "body": None}
    bo = None
    if body_override_fn is not None:
        try:
            bo = body_override_fn(r.json())
        except ValueError:
            bo = body_override_fn(r.text)
    return save(endpoint, case, req, r, note, body_override=bo), r


def k(**kw):
    d = {"apikey": APIKEY}
    d.update(kw)
    return d


def main():
    # ---------------- ping ----------------
    post("ping", "apikey_in_body", k())
    post("ping", "apikey_in_x_api_key_header", {}, headers={"X-API-KEY": APIKEY},
         note="Header-only auth: server reads apikey from JSON body only (schema required=True)")
    post("ping", "apikey_in_header_and_body", k(), headers={"X-API-KEY": APIKEY})
    post("ping", "extra_unknown_field", k(foo="bar"))

    # ---------------- account ----------------
    post("funds", "apikey_in_body", k())
    post("funds", "apikey_in_x_api_key_header", {}, headers={"X-API-KEY": APIKEY})
    post("funds", "apikey_in_header_and_body", k(), headers={"X-API-KEY": APIKEY})
    ob, _ = post("orderbook", "default", k())
    post("tradebook", "default", k())
    pb, _ = post("positionbook", "default", k())
    post("holdings", "default", k())
    post("gttorderbook", "default", k())

    # openposition
    sym, exch, prod = "RELIANCE", "NSE", "MIS"
    try:
        positions = pb.get("data") or []
        if positions:
            p0 = positions[0]
            sym, exch, prod = p0["symbol"], p0["exchange"], p0["product"]
    except Exception:
        pass
    post("openposition", "from_positionbook_or_default", k(strategy="fixtures", symbol=sym, exchange=exch, product=prod),
         note=f"symbol={sym} exchange={exch} product={prod}")
    post("openposition", "no_position_symbol", k(strategy="fixtures", symbol="SBIN", exchange="NSE", product="CNC"))

    # orderstatus
    orderid = None
    try:
        orders = (ob.get("data") or {}).get("orders") or []
        if orders:
            orderid = orders[0]["orderid"]
    except Exception:
        pass
    if orderid:
        post("orderstatus", "existing_orderid", k(strategy="fixtures", orderid=str(orderid)))
    post("orderstatus", "unknown_orderid", k(strategy="fixtures", orderid="000000000000000"),
         note="orderbook was empty -> unknown orderid error case" if not orderid else "unknown orderid")

    # ---------------- discovery: expiry ----------------
    exp_fut, _ = post("expiry", "nifty_nfo_futures", k(symbol="NIFTY", exchange="NFO", instrumenttype="futures"))
    exp_opt, _ = post("expiry", "nifty_nfo_options", k(symbol="NIFTY", exchange="NFO", instrumenttype="options"))
    exp_crude, _ = post("expiry", "crudeoil_mcx_futures", k(symbol="CRUDEOIL", exchange="MCX", instrumenttype="futures"))
    post("expiry", "banknifty_nfo_options", k(symbol="BANKNIFTY", exchange="NFO", instrumenttype="options"))

    def first_expiry(resp):
        try:
            d = resp.get("data")
            if isinstance(d, list) and d:
                return d[0]
            if isinstance(d, dict):
                for key in ("expiry_dates", "expiries", "data"):
                    if d.get(key):
                        return d[key][0]
        except Exception:
            pass
        return None

    fut_exp = first_expiry(exp_fut)
    opt_exp = first_expiry(exp_opt)
    crude_exp = first_expiry(exp_crude)
    nifty_fut = f"NIFTY{fut_exp}FUT" if fut_exp else "NIFTY30OCT26FUT"
    crude_fut = f"CRUDEOIL{crude_exp}FUT" if crude_exp else None
    print("discovered:", fut_exp, opt_exp, crude_exp, nifty_fut, crude_fut)

    # ---------------- option symbol ----------------
    os_ce, _ = post("optionsymbol", "nifty_atm_ce", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, offset="ATM", option_type="CE"))
    post("optionsymbol", "nifty_otm2_pe", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, offset="OTM2", option_type="PE"))
    post("optionsymbol", "nifty_itm1_ce_with_strike_int", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, offset="ITM1", option_type="CE", strike_int=50))
    post("optionsymbol", "invalid_offset", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, offset="ATM3", option_type="CE"))
    nifty_opt = None
    try:
        nifty_opt = os_ce.get("symbol") or (os_ce.get("data") or {}).get("symbol")
    except Exception:
        pass
    nifty_opt = nifty_opt or "NIFTY30OCT2626000CE"
    print("option symbol:", nifty_opt)

    # ---------------- quotes ----------------
    post("quotes", "reliance_nse", k(symbol="RELIANCE", exchange="NSE"))
    post("quotes", "reliance_nse_x_api_key_header", {"symbol": "RELIANCE", "exchange": "NSE"}, headers={"X-API-KEY": APIKEY})
    post("quotes", "reliance_nse_header_and_body", k(symbol="RELIANCE", exchange="NSE"), headers={"X-API-KEY": APIKEY})
    post("quotes", "sbin_nse", k(symbol="SBIN", exchange="NSE"))
    post("quotes", "nifty_nse_index", k(symbol="NIFTY", exchange="NSE_INDEX"))
    post("quotes", "banknifty_nse_index", k(symbol="BANKNIFTY", exchange="NSE_INDEX"))
    post("quotes", "nifty_future_nfo", k(symbol=nifty_fut, exchange="NFO"), note=f"symbol={nifty_fut}")
    post("quotes", "nifty_option_nfo", k(symbol=nifty_opt, exchange="NFO"), note=f"symbol={nifty_opt}")
    if crude_fut:
        post("quotes", "crudeoil_future_mcx", k(symbol=crude_fut, exchange="MCX"), note=f"symbol={crude_fut}")
    post("quotes", "reliance_bse", k(symbol="RELIANCE", exchange="BSE"))

    syms = [{"symbol": "RELIANCE", "exchange": "NSE"}, {"symbol": "SBIN", "exchange": "NSE"},
            {"symbol": "NIFTY", "exchange": "NSE_INDEX"}, {"symbol": "BANKNIFTY", "exchange": "NSE_INDEX"},
            {"symbol": nifty_fut, "exchange": "NFO"}, {"symbol": nifty_opt, "exchange": "NFO"}]
    if crude_fut:
        syms.append({"symbol": crude_fut, "exchange": "MCX"})
    post("multiquotes", "mixed_seven_symbols", k(symbols=syms))
    post("multiquotes", "with_one_unknown_symbol", k(symbols=[{"symbol": "RELIANCE", "exchange": "NSE"}, {"symbol": "FOOBARBAZ", "exchange": "NSE"}]))
    post("multiquotes", "empty_symbols_list", k(symbols=[]))

    # ---------------- depth ----------------
    post("depth", "reliance_nse", k(symbol="RELIANCE", exchange="NSE"))
    post("depth", "nifty_nse_index", k(symbol="NIFTY", exchange="NSE_INDEX"))
    post("depth", "nifty_future_nfo", k(symbol=nifty_fut, exchange="NFO"))
    post("depth", "nifty_option_nfo", k(symbol=nifty_opt, exchange="NFO"))

    # ---------------- history ----------------
    def hist_note(resp):
        try:
            d = resp.get("data")
            if isinstance(d, list):
                return f"rows={len(d)} first_ts={d[0].get('timestamp') if d else None} keys={sorted(d[0].keys()) if d else None}"
        except Exception:
            pass
        return ""

    def hist_trunc(resp):
        d = resp.get("data")
        if isinstance(d, list) and len(d) > 60:
            resp = dict(resp)
            resp["data"] = d[:40] + [{"_truncated": f"{len(d) - 50} rows omitted"}] + d[-10:]
            resp["_total_rows"] = len(d)
        return resp

    for interval, (s, e) in {"1m": ("2026-09-29", "2026-10-02"), "5m": ("2026-09-29", "2026-10-02"),
                             "15m": ("2026-09-22", "2026-10-02"), "D": ("2026-08-01", "2026-10-02")}.items():
        body, r = post("history", f"reliance_nse_{interval}", k(symbol="RELIANCE", exchange="NSE", interval=interval, start_date=s, end_date=e),
                       note=f"range {s}..{e}", body_override_fn=hist_trunc)
        try:
            INDEX[-1] = (INDEX[-1][0], INDEX[-1][1], INDEX[-1][2], INDEX[-1][3] + " " + hist_note(r.json()))
        except Exception:
            pass
    post("history", "nifty_index_D", k(symbol="NIFTY", exchange="NSE_INDEX", interval="D", start_date="2026-09-01", end_date="2026-10-02"), body_override_fn=hist_trunc)
    post("history", "nifty_future_5m", k(symbol=nifty_fut, exchange="NFO", interval="5m", start_date="2026-10-01", end_date="2026-10-02"), body_override_fn=hist_trunc)
    post("history", "reliance_nse_D_source_db", k(symbol="RELIANCE", exchange="NSE", interval="D", start_date="2026-09-01", end_date="2026-10-02", source="db"), body_override_fn=hist_trunc)
    post("history", "bad_date_format_ddmmyyyy", k(symbol="RELIANCE", exchange="NSE", interval="D", start_date="01-09-2026", end_date="02-10-2026"),
         note="Date format must be YYYY-MM-DD")
    post("history", "bad_interval", k(symbol="RELIANCE", exchange="NSE", interval="7m", start_date="2026-09-01", end_date="2026-10-02"))
    post("history", "end_before_start", k(symbol="RELIANCE", exchange="NSE", interval="D", start_date="2026-10-02", end_date="2026-09-01"))

    post("intervals", "default", k())

    # ---------------- symbol / search ----------------
    post("symbol", "reliance_nse", k(symbol="RELIANCE", exchange="NSE"))
    post("symbol", "nifty_future_nfo", k(symbol=nifty_fut, exchange="NFO"))
    post("symbol", "nifty_option_nfo", k(symbol=nifty_opt, exchange="NFO"))
    post("symbol", "nifty_nse_index", k(symbol="NIFTY", exchange="NSE_INDEX"))
    post("symbol", "unknown_symbol", k(symbol="FOOBARBAZ", exchange="NSE"))

    def trunc_data(n):
        def f(resp):
            d = resp.get("data")
            if isinstance(d, list) and len(d) > n:
                resp = dict(resp)
                resp["_total_rows"] = len(d)
                resp["data"] = d[:n]
                resp["_truncated"] = True
            return resp
        return f

    post("search", "reliance_nse", k(query="RELIANCE", exchange="NSE"), body_override_fn=trunc_data(25))
    post("search", "nifty_no_exchange", k(query="NIFTY"), body_override_fn=trunc_data(25))
    post("search", "nifty_nfo", k(query="NIFTY", exchange="NFO"), body_override_fn=trunc_data(25))
    post("search", "no_results", k(query="ZZZZQQQQ"))

    # ---------------- options analytics ----------------
    post("optionchain", "nifty_nearest_expiry_5_strikes", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, strike_count=5))
    post("optionchain", "nifty_nearest_expiry_3_strikes_with_greeks", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, strike_count=3, with_greeks=True))
    post("optionchain", "bad_expiry", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date="01JAN20", strike_count=3))

    post("optiongreeks", "nifty_option_default", k(symbol=nifty_opt, exchange="NFO"))
    post("optiongreeks", "nifty_option_with_rate_and_underlying", k(symbol=nifty_opt, exchange="NFO", interest_rate=6.5, underlying_symbol="NIFTY", underlying_exchange="NSE_INDEX"))
    post("optiongreeks", "unknown_symbol", k(symbol="NIFTY01JAN2010000CE", exchange="NFO"))

    pe_sym = re.sub(r"CE$", "PE", nifty_opt)
    post("multioptiongreeks", "nifty_ce_and_pe", k(symbols=[{"symbol": nifty_opt, "exchange": "NFO"}, {"symbol": pe_sym, "exchange": "NFO"}], interest_rate=6.5))

    post("syntheticfuture", "nifty_nearest_expiry", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp))

    # ---------------- instruments ----------------
    def instr_trunc(resp):
        if isinstance(resp, dict):
            d = resp.get("data")
            if isinstance(d, list):
                resp = dict(resp)
                resp["_total_rows"] = len(d)
                resp["data"] = d[:20]
                resp["_truncated"] = True
            return resp
        if isinstance(resp, str):
            lines = resp.splitlines()
            return {"_csv_first_lines": lines[:21], "_total_lines": len(lines)}
        return resp

    get("instruments", "nse_json", "/api/v1/instruments", params={"apikey": APIKEY, "exchange": "NSE"}, body_override_fn=instr_trunc,
        note="GET with query params; data truncated to 20 rows, _total_rows recorded")
    get("instruments", "nfo_json", "/api/v1/instruments", params={"apikey": APIKEY, "exchange": "NFO"}, body_override_fn=instr_trunc)
    get("instruments", "nse_csv", "/api/v1/instruments", params={"apikey": APIKEY, "exchange": "NSE", "format": "csv"}, body_override_fn=instr_trunc)
    get("instruments", "all_exchanges_json", "/api/v1/instruments", params={"apikey": APIKEY}, body_override_fn=instr_trunc)
    get("instruments", "bad_exchange", "/api/v1/instruments", params={"apikey": APIKEY, "exchange": "XXX"})
    post("instruments", "post_not_allowed", k(exchange="NSE"), note="instruments is GET-only")

    # ---------------- margin (read-only calculator -> Kite /margins/basket) ----------------
    post("margin", "single_equity_mis", k(positions=[{"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": "10", "product": "MIS", "pricetype": "MARKET", "price": "0", "trigger_price": "0"}]))
    post("margin", "basket_fut_and_option", k(positions=[
        {"symbol": nifty_fut, "exchange": "NFO", "action": "BUY", "quantity": "75", "product": "NRML", "pricetype": "MARKET", "price": "0", "trigger_price": "0"},
        {"symbol": nifty_opt, "exchange": "NFO", "action": "SELL", "quantity": "75", "product": "NRML", "pricetype": "MARKET", "price": "0", "trigger_price": "0"},
    ]))
    post("margin", "quantity_as_number_not_string", k(positions=[{"symbol": "RELIANCE", "exchange": "NSE", "action": "BUY", "quantity": 10, "product": "MIS", "pricetype": "MARKET"}]),
         note="schema declares quantity/price as Str")
    post("margin", "empty_positions", k(positions=[]))

    # ---------------- analyzer status (read) ----------------
    post("analyzer", "status", k(), note="POST /analyzer returns mode status; /analyzer/toggle NOT called")

    # ---------------- market ----------------
    post("market/holidays", "year_2026", k(year=2026))
    post("market/holidays", "no_year", k())
    post("market/holidays", "year_out_of_range", k(year=2019))
    post("market/timings", "saturday_2026-10-03", k(date="2026-10-03"))
    post("market/timings", "weekday_2026-10-01", k(date="2026-10-01"))
    post("market/timings", "bad_date", k(date="03/10/2026"))

    # ---------------- ticker (GET) ----------------
    get("ticker", "nse_reliance_D_json", "/api/v1/ticker/NSE:RELIANCE", params={"apikey": APIKEY, "interval": "D", "from": "2026-09-01", "to": "2026-10-02"}, body_override_fn=hist_trunc)
    get("ticker", "nse_reliance_5m_json", "/api/v1/ticker/NSE:RELIANCE", params={"apikey": APIKEY, "interval": "5m", "from": "2026-10-01", "to": "2026-10-02"}, body_override_fn=hist_trunc)
    get("ticker", "nse_reliance_D_txt", "/api/v1/ticker/NSE:RELIANCE", params={"apikey": APIKEY, "interval": "D", "from": "2026-09-01", "to": "2026-10-02", "format": "txt"})
    get("ticker", "nse_reliance_5m_txt", "/api/v1/ticker/NSE:RELIANCE", params={"apikey": APIKEY, "interval": "5m", "from": "2026-10-01", "to": "2026-10-02", "format": "txt"})
    get("ticker", "nse_index_nifty_D_json", "/api/v1/ticker/NSE_INDEX:NIFTY", params={"apikey": APIKEY, "interval": "D", "from": "2026-09-01", "to": "2026-10-02"}, body_override_fn=hist_trunc)
    get("ticker", "no_exchange_prefix_defaults", "/api/v1/ticker/SBIN", params={"apikey": APIKEY, "interval": "D", "from": "2026-09-20", "to": "2026-10-02"}, body_override_fn=hist_trunc,
        note="no 'EXCH:' prefix -> server silently uses NSE:RELIANCE (see ticker.py)")
    get("ticker", "missing_from_to", "/api/v1/ticker/NSE:RELIANCE", params={"apikey": APIKEY, "interval": "D"})
    get("ticker", "invalid_apikey_txt", "/api/v1/ticker/NSE:RELIANCE", params={"apikey": "invalid-key", "interval": "D", "from": "2026-09-01", "to": "2026-10-02", "format": "txt"})
    get("ticker", "invalid_apikey_json", "/api/v1/ticker/NSE:RELIANCE", params={"apikey": "invalid-key", "interval": "D", "from": "2026-09-01", "to": "2026-10-02"})

    # ---------------- pnl (sandbox-only) ----------------
    post("pnl", "symbols_in_live_mode", k(), path="/api/v1/pnl/symbols", note="only available in analyzer mode")

    # ---------------- chart prefs (GET) ----------------
    get("chart", "get_preferences", "/api/v1/chart", params={"apikey": APIKEY})
    get("chart", "get_missing_apikey", "/api/v1/chart")

    # ---------------- strategy (list / status) ----------------
    post("strategy", "list", k(), path="/api/v1/strategy/list")
    post("strategy", "status_unknown_id", k(strategy_id="00000000-0000-0000-0000-000000000000"), path="/api/v1/strategy/status")

    # ---------------- portfolio (GET only) ----------------
    get("portfolio", "benchmarks", "/api/v1/portfolio/benchmarks", params={"apikey": APIKEY})
    get("portfolio", "benchmarks_invalid_apikey", "/api/v1/portfolio/benchmarks", params={"apikey": "invalid"})

    # ---------------- errors ----------------
    post("errors", "invalid_apikey_funds", {"apikey": "invalid-key-0000"}, path="/api/v1/funds")
    post("errors", "invalid_apikey_quotes", {"apikey": "invalid-key-0000", "symbol": "RELIANCE", "exchange": "NSE"}, path="/api/v1/quotes")
    post("errors", "missing_apikey_funds", {}, path="/api/v1/funds")
    post("errors", "missing_apikey_quotes", {"symbol": "RELIANCE", "exchange": "NSE"}, path="/api/v1/quotes")
    post("errors", "empty_apikey_string", {"apikey": ""}, path="/api/v1/ping")
    post("errors", "missing_required_field_quotes_symbol", k(exchange="NSE"), path="/api/v1/quotes")
    post("errors", "missing_required_field_history_interval", k(symbol="RELIANCE", exchange="NSE", start_date="2026-09-01", end_date="2026-10-02"), path="/api/v1/history")
    post("errors", "unknown_symbol_quotes", k(symbol="FOOBARBAZ", exchange="NSE"), path="/api/v1/quotes")
    post("errors", "unknown_symbol_depth", k(symbol="FOOBARBAZ", exchange="NSE"), path="/api/v1/depth")
    post("errors", "bad_exchange_quotes", k(symbol="RELIANCE", exchange="XXX"), path="/api/v1/quotes")
    post("errors", "lowercase_exchange_quotes", k(symbol="RELIANCE", exchange="nse"), path="/api/v1/quotes")
    post("errors", "malformed_json_quotes", None, path="/api/v1/quotes", raw='{"apikey": "' + APIKEY + '", "symbol": "RELIANCE", ')
    post("errors", "non_json_content_type", None, path="/api/v1/ping", raw="apikey=" + APIKEY, headers={"Content-Type": "application/x-www-form-urlencoded"})
    post("errors", "empty_body_json_content_type", None, path="/api/v1/ping", raw="")
    post("errors", "json_array_body", None, path="/api/v1/ping", raw='["' + APIKEY + '"]')
    get("errors", "wrong_method_get_on_funds", "/api/v1/funds", params={"apikey": APIKEY})
    get("errors", "wrong_method_get_on_quotes", "/api/v1/quotes")
    r = requests.put(BASE + "/api/v1/ping", json=k(), timeout=30)
    save("errors", "wrong_method_put_on_ping", {"method": "PUT", "path": "/api/v1/ping", "headers": {"Content-Type": "application/json"}, "body": k()}, r)
    get("errors", "unknown_route", "/api/v1/doesnotexist")
    post("errors", "unknown_route_post", k(), path="/api/v1/doesnotexist")
    get("errors", "unknown_route_outside_api", "/doesnotexist")
    post("errors", "trailing_slash_ping", k(), path="/api/v1/ping/", note="strict_slashes=False")

    # ---------------- rate limit probe on /ping (API_RATE_LIMIT=100 per second, moving-window, keyed by remote addr) ----------------
    limit = 100
    n = limit + 3
    url = BASE + "/api/v1/ping"
    sess = requests.Session()

    def one(i):
        t0 = time.time()
        rr = sess.post(url, json=k(), timeout=30)
        return i, rr, t0, time.time()

    t_start = time.time()
    with ThreadPoolExecutor(max_workers=40) as ex:
        results = list(ex.map(one, range(n)))
    t_end = time.time()
    counts = {}
    first_429 = None
    for i, rr, a, b in results:
        counts[rr.status_code] = counts.get(rr.status_code, 0) + 1
        if rr.status_code == 429 and first_429 is None:
            first_429 = (i, rr)
    summary = {"requests_sent": n, "wall_seconds": round(t_end - t_start, 3), "status_counts": counts,
               "limit_config": "API_RATE_LIMIT=100 per second (flask-limiter, moving-window, memory://, key=remote address)"}
    if first_429:
        i, rr = first_429
        body = save("errors", "rate_limit_429_ping", {"method": "POST", "path": "/api/v1/ping", "headers": {"Content-Type": "application/json"}, "body": k()}, rr,
                    note=f"burst of {n} pings in {summary['wall_seconds']}s; first 429 at request index {i}; counts={counts}")
    else:
        # record the last response and the summary even if no 429 was hit
        _, rr, _, _ = results[-1]
        save("errors", "rate_limit_probe_no_429", {"method": "POST", "path": "/api/v1/ping", "headers": {"Content-Type": "application/json"}, "body": k()}, rr,
             note=f"burst of {n} pings in {summary['wall_seconds']}s produced no 429; counts={counts}")
    with open(os.path.join(OUT, "errors", "rate_limit_probe_summary.json"), "w") as f:
        json.dump(summary, f, indent=2)
    time.sleep(2)

    with open(os.path.join(os.path.dirname(OUT), "rest_index.json"), "w") as f:
        json.dump([{"endpoint": e, "case": c, "status": s, "note": n_} for e, c, s, n_ in INDEX], f, indent=2)
    print("done", len(INDEX))


if __name__ == "__main__":
    main()
