#!/usr/bin/env python3
"""Record OpenAlgo WebSocket proxy exchanges as JSONL (one object per line, with direction + timestamp)."""
import asyncio
import json
import os
import time

import websockets

WS_URL = "ws://127.0.0.1:8765"
APIKEY = os.environ["OA_APIKEY"]
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "websocket")
os.makedirs(OUT, exist_ok=True)
LISTEN = float(os.environ.get("OA_WS_LISTEN", "20"))

REDACT_KEYS = {"user_id", "userid", "client_id", "clientid", "username", "email"}


def redact(obj):
    if isinstance(obj, dict):
        return {k: (f"<{k.upper()}>" if k.lower() in REDACT_KEYS and obj[k] not in (None, "") else redact(v)) for k, v in obj.items()}
    if isinstance(obj, list):
        return [redact(x) for x in obj]
    if isinstance(obj, str):
        return obj.replace(APIKEY, "<APIKEY>")
    return obj


class Recorder:
    def __init__(self, name):
        self.path = os.path.join(OUT, f"{name}.jsonl")
        self.f = open(self.path, "w")
        self.counts = {}

    def write(self, direction, payload, **extra):
        rec = {"ts": round(time.time(), 3), "iso": time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime()) + "Z", "direction": direction}
        if isinstance(payload, str):
            try:
                rec["message"] = redact(json.loads(payload))
            except ValueError:
                rec["raw"] = redact(payload)
        else:
            rec["message"] = redact(payload)
        rec.update(extra)
        self.f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        self.f.flush()
        if direction == "recv" and isinstance(rec.get("message"), dict):
            t = rec["message"].get("type") or rec["message"].get("status")
            self.counts[t] = self.counts.get(t, 0) + 1

    def note(self, text, **extra):
        self.write("note", {"note": text}, **extra)

    def close(self):
        self.f.close()
        print(self.path, self.counts, flush=True)


async def send(ws, rec, msg):
    rec.write("send", msg)
    await ws.send(json.dumps(msg))


async def drain(ws, rec, seconds, label=None, max_market_data=30):
    """Receive for `seconds`; keep all control messages, but cap recorded market_data ticks."""
    end = time.time() + seconds
    md = 0
    md_total = 0
    while True:
        remaining = end - time.time()
        if remaining <= 0:
            break
        try:
            raw = await asyncio.wait_for(ws.recv(), timeout=remaining)
        except asyncio.TimeoutError:
            break
        try:
            m = json.loads(raw)
        except ValueError:
            rec.write("recv", raw)
            continue
        if m.get("type") == "market_data":
            md_total += 1
            if md < max_market_data:
                rec.write("recv", raw)
                md += 1
            continue
        rec.write("recv", raw)
    rec.note(f"listened {seconds}s{(' after ' + label) if label else ''}: market_data received={md_total} recorded={min(md, md_total)}", market_data_total=md_total)
    return md_total


async def wait_for(ws, rec, pred, timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        try:
            raw = await asyncio.wait_for(ws.recv(), timeout=max(0.05, end - time.time()))
        except asyncio.TimeoutError:
            return None
        rec.write("recv", raw)
        try:
            m = json.loads(raw)
        except ValueError:
            continue
        if pred(m):
            return m
    return None


async def session_main():
    rec = Recorder("01_connect_auth_subscribe_flow")
    async with websockets.connect(WS_URL, ping_interval=20, ping_timeout=20) as ws:
        rec.note(f"connected to {WS_URL}", response_headers={k: v for k, v in ws.response.headers.items()} if hasattr(ws, "response") else None)
        await drain(ws, rec, 1.0, "connect (server sends nothing unsolicited?)")

        # authenticate
        await send(ws, rec, {"action": "authenticate", "api_key": APIKEY})
        auth = await wait_for(ws, rec, lambda m: m.get("type") == "auth" or m.get("status") == "error", 30)
        rec.note(f"auth result: {json.dumps(redact(auth))}")

        # ping
        await send(ws, rec, {"action": "ping"})
        await wait_for(ws, rec, lambda m: m.get("type") == "pong", 5)

        # broker info
        await send(ws, rec, {"action": "get_broker_info"})
        await wait_for(ws, rec, lambda m: m.get("type") == "broker_info", 5)
        await send(ws, rec, {"action": "get_supported_brokers"})
        await wait_for(ws, rec, lambda m: m.get("type") == "supported_brokers", 5)

        syms = [{"symbol": "RELIANCE", "exchange": "NSE"}, {"symbol": "NIFTY", "exchange": "NSE_INDEX"}]

        # LTP mode 1 (numeric)
        await send(ws, rec, {"action": "subscribe", "symbols": syms, "mode": 1, "request_id": "req-ltp-1"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe", 15)
        await drain(ws, rec, LISTEN, "subscribe mode=1 (LTP)")

        # Quote mode 2 (string label)
        await send(ws, rec, {"action": "subscribe", "symbols": syms, "mode": "Quote", "request_id": "req-quote-2"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe", 15)
        await drain(ws, rec, LISTEN, "subscribe mode=Quote")

        # Depth mode 3 default 5
        await send(ws, rec, {"action": "subscribe", "symbols": syms, "mode": 3, "request_id": "req-depth-5"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe", 15)
        await drain(ws, rec, LISTEN, "subscribe mode=3 depth default(5)")

        # Depth 20 via "depth" and legacy "depth_level"
        await send(ws, rec, {"action": "subscribe", "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}], "mode": "Depth", "depth": 20, "request_id": "req-depth-20"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe", 15)
        await drain(ws, rec, 5, "subscribe mode=Depth depth=20")
        await send(ws, rec, {"action": "subscribe", "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}], "mode": "Depth", "depth_level": 20, "request_id": "req-depth-20-legacy"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe", 15)
        await drain(ws, rec, 3, "subscribe mode=Depth depth_level=20 (legacy key)")
        await send(ws, rec, {"action": "subscribe", "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}], "mode": "Depth", "depth": 50, "request_id": "req-depth-50"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe", 15)
        await drain(ws, rec, 3, "subscribe mode=Depth depth=50")

        # single-symbol legacy form
        await send(ws, rec, {"action": "subscribe", "symbol": "SBIN", "exchange": "NSE", "mode": "LTP", "request_id": "req-single"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe", 15)
        await drain(ws, rec, 3, "subscribe single symbol form (symbol/exchange top-level)")

        # subscribe with invalid mode / missing symbols / unknown symbol
        await send(ws, rec, {"action": "subscribe", "symbols": syms, "mode": 9, "request_id": "req-badmode"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "subscribe", 10)
        await send(ws, rec, {"action": "subscribe", "mode": 1, "request_id": "req-nosyms"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "subscribe", 10)
        await send(ws, rec, {"action": "subscribe", "symbols": [{"symbol": "FOOBARBAZ", "exchange": "NSE"}], "mode": 1, "request_id": "req-unknown-symbol"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "subscribe", 15)
        await send(ws, rec, {"action": "subscribe", "symbols": [{"symbol": "RELIANCE", "exchange": "XXX"}], "mode": 1, "request_id": "req-bad-exchange"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "subscribe", 15)

        # unsubscribe one symbol/mode
        await send(ws, rec, {"action": "unsubscribe", "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}], "mode": "Quote", "request_id": "req-unsub-1"})
        await wait_for(ws, rec, lambda m: m.get("type") == "unsubscribe", 15)
        # unsubscribe something not subscribed
        await send(ws, rec, {"action": "unsubscribe", "symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "mode": "Depth", "request_id": "req-unsub-notsubscribed"})
        await wait_for(ws, rec, lambda m: m.get("type") == "unsubscribe", 15)
        # unsubscribe with per-symbol mode and invalid mode
        await send(ws, rec, {"action": "unsubscribe", "symbols": [{"symbol": "NIFTY", "exchange": "NSE_INDEX", "mode": 1}, {"symbol": "NIFTY", "exchange": "NSE_INDEX", "mode": "bogus"}], "request_id": "req-unsub-mixed"})
        await wait_for(ws, rec, lambda m: m.get("type") == "unsubscribe", 15)

        # order updates stream
        await send(ws, rec, {"action": "subscribe_orders"})
        await wait_for(ws, rec, lambda m: m.get("type") == "subscribe_orders" or m.get("status") == "error", 10)
        await drain(ws, rec, 3, "subscribe_orders (no orders placed; expect no order_update events)")
        await send(ws, rec, {"action": "unsubscribe_orders"})
        await wait_for(ws, rec, lambda m: m.get("type") == "unsubscribe_orders" or m.get("status") == "error", 10)

        # unsubscribe_all
        await send(ws, rec, {"action": "unsubscribe_all", "request_id": "req-unsub-all"})
        await wait_for(ws, rec, lambda m: m.get("type") == "unsubscribe", 15)
        await drain(ws, rec, 2, "unsubscribe_all")

        # invalid action and 'type' alias
        await send(ws, rec, {"action": "bogus_action"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error", 5)
        await send(ws, rec, {"type": "ping"})
        await wait_for(ws, rec, lambda m: m.get("type") == "pong", 5)

        # malformed message
        rec.write("send", "this is not json {", raw_text=True)
        await ws.send("this is not json {")
        await wait_for(ws, rec, lambda m: m.get("status") == "error", 5)
        # JSON but not an object
        rec.write("send", "[1,2,3]", raw_text=True)
        await ws.send("[1,2,3]")
        await wait_for(ws, rec, lambda m: m.get("status") == "error", 5)
        # empty object
        await send(ws, rec, {})
        await wait_for(ws, rec, lambda m: m.get("status") == "error", 5)

        rec.note(f"closing; close_code pending")
    rec.note("closed by client")
    rec.close()


async def unauth_flow():
    rec = Recorder("02_subscribe_before_auth")
    async with websockets.connect(WS_URL) as ws:
        rec.note("connected; sending subscribe before authenticate")
        await send(ws, rec, {"action": "subscribe", "symbols": [{"symbol": "RELIANCE", "exchange": "NSE"}], "mode": 1, "request_id": "req-pre-auth"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error", 5)
        await send(ws, rec, {"action": "unsubscribe_all"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "unsubscribe", 5)
        await send(ws, rec, {"action": "subscribe_orders"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "subscribe_orders", 5)
        await send(ws, rec, {"action": "ping"})
        await wait_for(ws, rec, lambda m: m.get("type") == "pong" or m.get("status") == "error", 5)
        await send(ws, rec, {"action": "get_broker_info"})
        await wait_for(ws, rec, lambda m: m.get("type") == "broker_info" or m.get("status") == "error", 5)
    rec.close()


async def invalid_auth_flow():
    rec = Recorder("03_invalid_apikey_auth")
    async with websockets.connect(WS_URL) as ws:
        await send(ws, rec, {"action": "authenticate", "api_key": "invalid-key-0000"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "auth", 10)
        await send(ws, rec, {"action": "authenticate"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "auth", 10)
        await send(ws, rec, {"action": "auth", "apikey": "invalid-key-0000"})
        await wait_for(ws, rec, lambda m: m.get("status") == "error" or m.get("type") == "auth", 10)
        rec.note("checking whether the connection is still open after invalid auth")
        try:
            await send(ws, rec, {"action": "ping"})
            await wait_for(ws, rec, lambda m: m.get("type") == "pong", 5)
            rec.note("connection still open after invalid auth (server does not close)")
        except Exception as e:
            rec.note(f"connection closed after invalid auth: {e!r}")
    rec.close()


async def auth_timeout_flow():
    rec = Recorder("04_auth_grace_timeout")
    t0 = time.time()
    try:
        async with websockets.connect(WS_URL) as ws:
            rec.note("connected; sending nothing, waiting for server to enforce WS_AUTH_GRACE_SECONDS (default 15)")
            try:
                raw = await asyncio.wait_for(ws.recv(), timeout=25)
                rec.write("recv", raw)
            except asyncio.TimeoutError:
                rec.note("no close within 25s")
            except websockets.exceptions.ConnectionClosed as e:
                rec.note(f"server closed connection after {round(time.time() - t0, 1)}s", close_code=e.rcvd.code if e.rcvd else None, close_reason=e.rcvd.reason if e.rcvd else None)
    except Exception as e:
        rec.note(f"exception: {e!r}")
    rec.close()


async def alt_auth_forms():
    rec = Recorder("05_auth_alias_forms_and_mode_labels")
    async with websockets.connect(WS_URL) as ws:
        # 'type' instead of 'action', 'apikey' instead of 'api_key'
        await send(ws, rec, {"type": "auth", "apikey": APIKEY})
        await wait_for(ws, rec, lambda m: m.get("type") == "auth" or m.get("status") == "error", 30)
        # re-authenticate on already-authenticated connection
        await send(ws, rec, {"action": "authenticate", "api_key": APIKEY})
        await wait_for(ws, rec, lambda m: m.get("type") == "auth" or m.get("status") == "error", 30)
        for mode in ["ltp", "QUOTE", "depth", "2", 2.0]:
            await send(ws, rec, {"action": "subscribe", "symbols": [{"symbol": "SBIN", "exchange": "NSE"}], "mode": mode, "request_id": f"req-mode-{mode}"})
            await wait_for(ws, rec, lambda m: m.get("type") == "subscribe" or m.get("status") == "error", 15)
        await drain(ws, rec, 5, "mode-label subscriptions")
        await send(ws, rec, {"action": "unsubscribe_all"})
        await wait_for(ws, rec, lambda m: m.get("type") == "unsubscribe", 15)
    rec.close()


async def main():
    await session_main()
    await unauth_flow()
    await invalid_auth_flow()
    await alt_auth_forms()
    await auth_timeout_flow()


if __name__ == "__main__":
    asyncio.run(main())
