from mut import *
import json, time, requests
G = json.load(open(os.path.join(HERE, "..", "gtt.json")))
S = "fixtures"
guard()
P("cancelorder", "trigger_pending_sl", k(strategy=S, orderid="26100356024725"), note="cancelallorder skipped this trigger-pending order")
P("cancelorder", "trigger_pending_slm", k(strategy=S, orderid="26100363343152"))
P("cancelgttorder", "oco", k(strategy=S, trigger_id=G["t2"]))
P("gttorderbook", "after_all_cancelled", k())
ob = requests.post(BASE + "/api/v1/orderbook", json=k(), timeout=30).json()
pending = [o for o in ob["data"]["orders"] if o["order_status"] in ("open", "trigger pending", "pending")]
log(f"pre-toggle in-flight orders: {len(pending)}")
if pending:
    raise SystemExit("orders in flight; not toggling")
# toggle validation errors (no state change)
P("analyzer", "toggle_error_missing_mode", k(), path="/api/v1/analyzer/toggle")
P("analyzer", "toggle_error_invalid_mode", k(mode="maybe"), path="/api/v1/analyzer/toggle")
assert mode() is True
try:
    P("analyzer", "toggle_to_live_false", k(mode=False), path="/api/v1/analyzer/toggle", note="round trip for shape only; no orders placed while live")
    P("analyzer", "status_while_live", k())
finally:
    for i in range(5):
        b, r = P("analyzer", "toggle_back_to_analyze_true" if i == 0 else f"toggle_back_retry_{i}", k(mode=True), path="/api/v1/analyzer/toggle")
        if mode() is True:
            break
        time.sleep(1)
P("analyzer", "status_after_round_trip", k())
assert mode() is True, "FAILED TO RESTORE ANALYZE MODE"
print("restored analyze mode")
