from mut import *
import json
C = json.load(open(os.path.join(HERE, "..", "ctx.json")))
S = "fixtures"
L = C["sbin_ltp"]
guard()
g = dict(strategy=S, exchange="NSE", symbol="SBIN", action="BUY", product="CNC", quantity=1, pricetype="LIMIT")
b1,_ = P("placegttorder", "single_buy_cnc_trigger_below", k(**g, trigger_type="SINGLE", price=round(L*0.9, 1), triggerprice_sl=round(L*0.9, 1)), note="SINGLE: one trigger")
b2,_ = P("placegttorder", "oco_sell_cnc", k(**dict(g, action="SELL"), trigger_type="OCO", price=round(L, 1), triggerprice_sl=round(L*0.9, 1), stoploss=round(L*0.89, 1), triggerprice_tg=round(L*1.1, 1), target=round(L*1.11, 1)))
# fix: SINGLE needs trigger_type
P("placegttorder", "error_mis_product", k(**dict(g, product="MIS"), trigger_type="SINGLE", price=900, triggerprice_sl=900))
P("placegttorder", "error_bad_trigger_type", k(**g, trigger_type="TRIPLE", price=900, triggerprice_sl=900))
P("placegttorder", "error_oco_missing_legs", k(**g, trigger_type="OCO", price=900, triggerprice_sl=900))
P("placegttorder", "error_unknown_symbol", k(**dict(g, symbol="NOTASYMBOL"), trigger_type="SINGLE", price=900, triggerprice_sl=900))
bk,_ = P("gttorderbook", "after_place", k())
P("gttorderbook", "status_active", k(status="active"))
def tid(b):
    d = b.get("data") if isinstance(b.get("data"), dict) else b
    return str(d.get("trigger_id") or d.get("id") or "") if isinstance(d, dict) else ""
t1, t2 = tid(b1), tid(b2)
print("trigger ids", t1, t2)
json.dump({"t1": t1, "t2": t2}, open(os.path.join(HERE, "..", "gtt.json"), "w"))
