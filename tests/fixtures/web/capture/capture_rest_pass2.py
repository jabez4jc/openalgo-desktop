#!/usr/bin/env python3
"""Second pass: redo derived-symbol cases with the correct expiry format.
/expiry returns 'DD-MMM-YY' (e.g. 27-OCT-26); OpenAlgo symbols use DDMMMYY (NIFTY27OCT26FUT)
and optionsymbol/optionchain/syntheticfuture expiry_date expects DDMMMYY (06OCT26)."""
import json
import os
import re

import capture_rest as C

k = C.k
post = C.post


def main():
    exp_fut = json.load(open(os.path.join(C.OUT, "expiry", "nifty_nfo_futures.json")))["response"]["body"]["data"][0]
    exp_opt = json.load(open(os.path.join(C.OUT, "expiry", "nifty_nfo_options.json")))["response"]["body"]["data"][0]
    exp_crude = json.load(open(os.path.join(C.OUT, "expiry", "crudeoil_mcx_futures.json")))["response"]["body"]["data"][0]
    fut_exp = exp_fut.replace("-", "")
    opt_exp = exp_opt.replace("-", "")
    crude_exp = exp_crude.replace("-", "")
    nifty_fut = f"NIFTY{fut_exp}FUT"
    crude_fut = f"CRUDEOIL{crude_exp}FUT"
    print("using", nifty_fut, opt_exp, crude_fut)

    note_fmt = "expiry from /expiry is DD-MMM-YY; converted to DDMMMYY"
    os_ce, _ = post("optionsymbol", "nifty_atm_ce", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, offset="ATM", option_type="CE"), note=note_fmt)
    post("optionsymbol", "nifty_otm2_pe", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, offset="OTM2", option_type="PE"), note=note_fmt)
    post("optionsymbol", "nifty_itm1_ce_with_strike_int", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, offset="ITM1", option_type="CE", strike_int=50), note=note_fmt)
    post("optionsymbol", "expiry_with_dashes_as_returned_by_expiry_api", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=exp_opt, offset="ATM", option_type="CE"),
         note="passing the raw DD-MMM-YY value from /expiry is rejected")
    post("optionsymbol", "underlying_is_future_symbol", k(underlying=nifty_fut, exchange="NFO", offset="ATM", option_type="CE"),
         note="underlying as NIFTYddMMMyyFUT on NFO; expiry inferred")
    nifty_opt = None
    try:
        nifty_opt = os_ce.get("symbol") or (os_ce.get("data") or {}).get("symbol")
    except Exception:
        pass
    print("option symbol:", nifty_opt)
    if not nifty_opt:
        # fall back to search
        body, _ = post("search", "nifty_options_for_expiry", k(query=f"NIFTY{opt_exp}", exchange="NFO"), body_override_fn=lambda r: r)
        for row in body.get("data") or []:
            if row.get("symbol", "").endswith("CE"):
                nifty_opt = row["symbol"]
                break
    pe_sym = re.sub(r"CE$", "PE", nifty_opt)

    post("quotes", "nifty_future_nfo", k(symbol=nifty_fut, exchange="NFO"), note=f"symbol={nifty_fut}")
    post("quotes", "nifty_option_nfo", k(symbol=nifty_opt, exchange="NFO"), note=f"symbol={nifty_opt}")
    post("quotes", "crudeoil_future_mcx", k(symbol=crude_fut, exchange="MCX"), note=f"symbol={crude_fut}")
    post("quotes", "symbol_with_dashed_expiry", k(symbol=f"NIFTY{exp_fut}FUT", exchange="NFO"), note="symbol built with dashed expiry is not found")

    syms = [{"symbol": "RELIANCE", "exchange": "NSE"}, {"symbol": "SBIN", "exchange": "NSE"},
            {"symbol": "NIFTY", "exchange": "NSE_INDEX"}, {"symbol": "BANKNIFTY", "exchange": "NSE_INDEX"},
            {"symbol": nifty_fut, "exchange": "NFO"}, {"symbol": nifty_opt, "exchange": "NFO"},
            {"symbol": crude_fut, "exchange": "MCX"}]
    post("multiquotes", "mixed_seven_symbols", k(symbols=syms))

    post("depth", "nifty_future_nfo", k(symbol=nifty_fut, exchange="NFO"))
    post("depth", "nifty_option_nfo", k(symbol=nifty_opt, exchange="NFO"))
    post("depth", "crudeoil_future_mcx", k(symbol=crude_fut, exchange="MCX"))

    def hist_trunc(resp):
        d = resp.get("data")
        if isinstance(d, list) and len(d) > 60:
            resp = dict(resp)
            resp["data"] = d[:40] + [{"_truncated": f"{len(d) - 50} rows omitted"}] + d[-10:]
            resp["_total_rows"] = len(d)
        return resp

    post("history", "nifty_future_5m", k(symbol=nifty_fut, exchange="NFO", interval="5m", start_date="2026-10-01", end_date="2026-10-02"), body_override_fn=hist_trunc)
    post("history", "crudeoil_future_15m", k(symbol=crude_fut, exchange="MCX", interval="15m", start_date="2026-10-01", end_date="2026-10-02"), body_override_fn=hist_trunc)
    post("history", "nifty_option_5m", k(symbol=nifty_opt, exchange="NFO", interval="5m", start_date="2026-10-01", end_date="2026-10-02"), body_override_fn=hist_trunc)

    post("symbol", "nifty_future_nfo", k(symbol=nifty_fut, exchange="NFO"))
    post("symbol", "nifty_option_nfo", k(symbol=nifty_opt, exchange="NFO"))
    post("symbol", "crudeoil_future_mcx", k(symbol=crude_fut, exchange="MCX"))

    post("optionchain", "nifty_nearest_expiry_5_strikes", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, strike_count=5), note=note_fmt)
    post("optionchain", "nifty_nearest_expiry_3_strikes_with_greeks", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp, strike_count=3, with_greeks=True, interest_rate=6.5), note=note_fmt)
    post("optionchain", "banknifty_monthly_3_strikes", k(underlying="BANKNIFTY", exchange="NSE_INDEX", expiry_date=json.load(open(os.path.join(C.OUT, "expiry", "banknifty_nfo_options.json")))["response"]["body"]["data"][0].replace("-", ""), strike_count=3))

    post("optiongreeks", "nifty_option_default", k(symbol=nifty_opt, exchange="NFO"))
    post("optiongreeks", "nifty_option_with_rate_and_underlying", k(symbol=nifty_opt, exchange="NFO", interest_rate=6.5, underlying_symbol="NIFTY", underlying_exchange="NSE_INDEX"))
    post("multioptiongreeks", "nifty_ce_and_pe", k(symbols=[{"symbol": nifty_opt, "exchange": "NFO"}, {"symbol": pe_sym, "exchange": "NFO"}], interest_rate=6.5))
    post("multioptiongreeks", "one_valid_one_unknown", k(symbols=[{"symbol": nifty_opt, "exchange": "NFO"}, {"symbol": "NIFTY01JAN2010000CE", "exchange": "NFO"}]))

    post("syntheticfuture", "nifty_nearest_expiry", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=opt_exp), note=note_fmt)
    post("syntheticfuture", "nifty_monthly_expiry", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=fut_exp))

    post("margin", "basket_fut_and_option", k(positions=[
        {"symbol": nifty_fut, "exchange": "NFO", "action": "BUY", "quantity": "65", "product": "NRML", "pricetype": "MARKET", "price": "0", "trigger_price": "0"},
        {"symbol": nifty_opt, "exchange": "NFO", "action": "SELL", "quantity": "65", "product": "NRML", "pricetype": "MARKET", "price": "0", "trigger_price": "0"},
    ]), note="lot size 65 from /search")
    post("margin", "single_option_limit", k(positions=[
        {"symbol": nifty_opt, "exchange": "NFO", "action": "BUY", "quantity": "65", "product": "NRML", "pricetype": "LIMIT", "price": "100", "trigger_price": "0"},
    ]))

    idx_path = os.path.join(os.path.dirname(C.OUT), "rest_index.json")
    existing = json.load(open(idx_path))
    by_key = {(r["endpoint"], r["case"]): r for r in existing}
    for e, c, s, n_ in C.INDEX:
        by_key[(e, c)] = {"endpoint": e, "case": c, "status": s, "note": n_}
    with open(idx_path, "w") as f:
        json.dump(list(by_key.values()), f, indent=2)
    print("pass2 done", len(C.INDEX))


if __name__ == "__main__":
    main()
