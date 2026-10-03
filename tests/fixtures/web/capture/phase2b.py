from mut import *
import json
C = json.load(open(os.path.join(HERE, "..", "ctx.json")))
ids = json.load(open(os.path.join(HERE, "..", "ids.json")))
guard()
for case, kw in [("market_buy_nrml_crudeoil_future_mcx", dict(exchange="MCX", symbol=C["cfut"], action="BUY", quantity=100, pricetype="MARKET", product="NRML")),
                 ("lowercase_action_buy", dict(exchange="NSE", symbol="SBIN", action="buy", quantity=1, pricetype="MARKET", product="MIS"))]:
    b, r = P("placeorder", case, k(strategy="fixtures", **kw))
    ids[case] = b.get("orderid")
json.dump(ids, open(os.path.join(HERE, "..", "ids.json"), "w"), indent=1)
