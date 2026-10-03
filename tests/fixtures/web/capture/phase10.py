from mut import *
import json
G = json.load(open(os.path.join(HERE, "..", "gtt.json")))
S = "fixtures"
guard()
P("cancelgttorder", "oco_retry", k(strategy=S, trigger_id=G["t2"]))
P("cancelallorder", "final_cleanup", k(strategy=S))
P("closeposition", "final_cleanup", k(strategy=S))
P("gttorderbook", "final", k())
P("positionbook", "final", k())
P("funds", "final", k())
P("analyzer", "status_final", k())
