from mut import *
import json
C = json.load(open(os.path.join(HERE, "..", "ctx.json")))
S = "fixtures"
ids = {}
guard()
def po(case, note="", **kw):
    b, r = P("placeorder", case, k(strategy=S, **kw), note=note)
    ids[case] = b.get("orderid")
    return b
po("market_buy_mis_reliance", exchange="NSE", symbol="RELIANCE", action="BUY", quantity=5, pricetype="MARKET", product="MIS")
po("market_sell_mis_sbin", exchange="NSE", symbol="SBIN", action="SELL", quantity=3, pricetype="MARKET", product="MIS")
po("limit_buy_cnc_sbin_far_below", note="LIMIT far below LTP -> stays open", exchange="NSE", symbol="SBIN", action="BUY", quantity=2, pricetype="LIMIT", product="CNC", price=round(C["sbin_ltp"]*0.8, 1))
po("limit_buy_mis_reliance_far_below", exchange="NSE", symbol="RELIANCE", action="BUY", quantity=1, pricetype="LIMIT", product="MIS", price=round(C["rel_ltp"]*0.8, 1))
po("limit_sell_mis_reliance_far_above", exchange="NSE", symbol="RELIANCE", action="SELL", quantity=1, pricetype="LIMIT", product="MIS", price=round(C["rel_ltp"]*1.2, 1))
po("sl_buy_mis_reliance", note="SL BUY: trigger above LTP", exchange="NSE", symbol="RELIANCE", action="BUY", quantity=1, pricetype="SL", product="MIS", price=round(C["rel_ltp"]*1.06, 1), trigger_price=round(C["rel_ltp"]*1.05, 1))
po("slm_sell_mis_sbin", note="SL-M SELL: trigger below LTP", exchange="NSE", symbol="SBIN", action="SELL", quantity=1, pricetype="SL-M", product="MIS", trigger_price=round(C["sbin_ltp"]*0.95, 1))
po("market_buy_cnc_sbin", exchange="NSE", symbol="SBIN", action="BUY", quantity=2, pricetype="MARKET", product="CNC")
po("market_buy_nrml_nifty_future", exchange="NFO", symbol=C["nfut"], action="BUY", quantity=65, pricetype="MARKET", product="NRML")
po("market_buy_nrml_nifty_option", exchange="NFO", symbol=C["opt"], action="BUY", quantity=65, pricetype="MARKET", product="NRML")
po("market_buy_mis_nifty_option", exchange="NFO", symbol=C["opt"], action="BUY", quantity=65, pricetype="MARKET", product="MIS")
po("market_buy_nrml_crudeoil_future_mcx", exchange="MCX", symbol=C["cfut"], action="BUY", quantity=100, pricetype="MARKET", product="NRML")
po("lowercase_action_buy", note="action accepts lowercase", exchange="NSE", symbol="SBIN", action="buy", quantity=1, pricetype="MARKET", product="MIS")
json.dump(ids, open(os.path.join(HERE, "..", "ids.json"), "w"), indent=1)
