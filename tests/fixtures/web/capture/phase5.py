from mut import *
import json
C = json.load(open(os.path.join(HERE, "..", "ctx.json")))
S = "fixtures"
guard()
P("basketorder", "three_legs_mixed", k(strategy=S, orders=[
    dict(exchange="NSE", symbol="TCS", action="BUY", quantity=1, pricetype="MARKET", product="MIS"),
    dict(exchange="NSE", symbol="HDFCBANK", action="SELL", quantity=2, pricetype="MARKET", product="MIS"),
    dict(exchange="NSE", symbol="SBIN", action="BUY", quantity=1, pricetype="LIMIT", product="CNC", price=round(C["sbin_ltp"]*0.8, 1))]))
P("basketorder", "one_leg_unknown_symbol", k(strategy=S, orders=[
    dict(exchange="NSE", symbol="ITC", action="BUY", quantity=1, pricetype="MARKET", product="MIS"),
    dict(exchange="NSE", symbol="NOTASYMBOL", action="BUY", quantity=1, pricetype="MARKET", product="MIS")]))
P("basketorder", "error_empty_orders", k(strategy=S, orders=[]))
P("basketorder", "error_leg_bad_product", k(strategy=S, orders=[dict(exchange="NSE", symbol="ITC", action="BUY", quantity=1, product="BAD")]))
guard()
P("splitorder", "sbin_10_split_3", k(strategy=S, exchange="NSE", symbol="SBIN", action="BUY", quantity=10, splitsize=3, pricetype="MARKET", product="MIS"))
P("splitorder", "error_splitsize_zero", k(strategy=S, exchange="NSE", symbol="SBIN", action="BUY", quantity=10, splitsize=0))
P("splitorder", "error_missing_splitsize", k(strategy=S, exchange="NSE", symbol="SBIN", action="BUY", quantity=10))
guard()
oo = dict(strategy=S, underlying="NIFTY", exchange="NSE_INDEX", expiry_date=C["oexp"], action="BUY", quantity=65, pricetype="MARKET", product="NRML")
P("optionsorder", "atm_ce_buy", k(**oo, offset="ATM", option_type="CE"))
P("optionsorder", "itm2_pe_buy", k(**oo, offset="ITM2", option_type="PE"))
P("optionsorder", "otm3_ce_sell", k(**dict(oo, action="SELL"), offset="OTM3", option_type="CE"))
P("optionsorder", "atm_ce_buy_with_splitsize", k(**dict(oo, quantity=130), offset="ATM", option_type="CE", splitsize=65))
P("optionsorder", "error_bad_offset", k(**oo, offset="ATM99X", option_type="CE"))
P("optionsorder", "error_bad_expiry", k(**dict(oo, expiry_date="01JAN20"), offset="ATM", option_type="CE"))
P("optionsorder", "error_qty_not_lot_multiple", k(**dict(oo, quantity=10), offset="ATM", option_type="CE"))
guard()
mo = dict(strategy=S, underlying="NIFTY", exchange="NSE_INDEX", expiry_date=C["oexp"])
P("optionsmultiorder", "bull_call_spread_2_legs", k(**mo, legs=[
    dict(offset="ATM", option_type="CE", action="BUY", quantity=65, pricetype="MARKET", product="NRML"),
    dict(offset="OTM2", option_type="CE", action="SELL", quantity=65, pricetype="MARKET", product="NRML")]))
P("optionsmultiorder", "error_empty_legs", k(**mo, legs=[]))
P("optionsmultiorder", "one_leg_bad_offset", k(**mo, legs=[
    dict(offset="ATM", option_type="PE", action="BUY", quantity=65, product="NRML"),
    dict(offset="BAD", option_type="PE", action="SELL", quantity=65, product="NRML")]))
