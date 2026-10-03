from mut import *
import json
P("analyzer", "status_before_mutations", k())
guard()
# discovery
fe,_ = P("expiry", "nifty_nfo_futures_session2", k(symbol="NIFTY", exchange="NFO", instrumenttype="futures"))
ce,_ = P("expiry", "crudeoil_mcx_futures_session2", k(symbol="CRUDEOIL", exchange="MCX", instrumenttype="futures"))
oe,_ = P("expiry", "nifty_nfo_options_session2", k(symbol="NIFTY", exchange="NFO", instrumenttype="options"))
def sym(e): return e.replace("-", "")
nfut = "NIFTY" + sym(fe["data"][0]) + "FUT"
cfut = "CRUDEOIL" + sym(ce["data"][0]) + "FUT"
oexp = sym(oe["data"][0])
os_,_ = P("optionsymbol", "nifty_atm_ce_session2", k(underlying="NIFTY", exchange="NSE_INDEX", expiry_date=oexp, offset="ATM", option_type="CE"))
P("symbol", "nifty_future_session2", k(symbol=nfut, exchange="NFO"))
P("symbol", "crudeoil_future_session2", k(symbol=cfut, exchange="MCX"))
q,_ = P("quotes", "reliance_session2", k(symbol="RELIANCE", exchange="NSE"))
q2,_ = P("quotes", "sbin_session2", k(symbol="SBIN", exchange="NSE"))
json.dump({"nfut": nfut, "cfut": cfut, "oexp": oexp, "opt": os_.get("symbol"), "optlot": os_.get("lotsize"),
           "rel_ltp": q["data"]["ltp"], "sbin_ltp": q2["data"]["ltp"]}, open(os.path.join(HERE, "..", "ctx.json"), "w"), indent=1)
