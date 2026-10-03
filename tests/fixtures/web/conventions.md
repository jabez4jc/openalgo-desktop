
## Observed conventions

### Transport and auth
- All `/api/v1/*` data endpoints are `POST` with `Content-Type: application/json` and `apikey` **inside the JSON body**. Exceptions that are `GET` with `?apikey=` in the query string: `/instruments`, `/ticker/<EXCH:SYMBOL>`, `/chart`, `/portfolio/benchmarks`.
- `X-API-KEY` header is **not** honoured by any `/api/v1` endpoint captured (only `telegram/*` and `whatsapp/*` read it). Header-only auth -> `400 {"status":"error","message":{"apikey":["Missing data for required field."]}}`.
- Marshmallow schemas are strict: unknown JSON fields -> 400 `{"<field>":["Unknown field."]}`; `exchange` is case-sensitive and must be one of `NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO`.
- Trailing slash tolerated (`/ping/` == `/ping`).

### Success envelope
- Most endpoints: `{"status":"success","data":...}`. Account endpoints in analyze mode add top-level `"mode":"analyze"`; `positionbook`/`pnl/symbols` add top-level `total_pnl`, `total_pnl_today`, `total_today_realized_pnl`, `total_unrealized_pnl`.
- **Flat (no `data` wrapper)** responses: `optionsymbol`, `optionchain`, `optiongreeks`, `syntheticfuture`, `openposition` (`{"status","quantity","mode"}`), `multioptiongreeks` (`data` list + `summary`), `expiry` (`data` list + `message`).
- Numbers are JSON numbers (never strings) in all market-data and sandbox account responses; ints and floats are mixed within the same object (`prev_close: 1187`, `ltp: 1167.7`), so parse as f64.
- `token` in symbol rows is a composite string `"<instrument_token>::::<exchange_token>"`.

### Error envelope
- Application errors: `{"status":"error","message": <string | object>}`. `message` is a **string** for business errors (invalid key, symbol not found) and an **object `{field:[msgs]}`** for schema validation errors.
- HTTP codes: 400 validation / unknown symbol / bad exchange; 403 invalid apikey (`"Invalid openalgo apikey"`); 404 symbol lookup miss, unknown orderid, unknown route, **and wrong HTTP method** (flask-restx returns `{"status":"error","message":"Not found","path":...}` for GET on a POST route, not 405); 500 for unhandled cases.
- Framework-level errors do NOT carry `status`: malformed JSON / empty body -> 400 `{"message":"The browser (or proxy) sent a request that this server could not understand."}`; JSON array body -> 500 `{"message":"Internal Server Error"}`. A client must tolerate a missing `status`.
- `ticker` with `format=txt` + invalid apikey -> 500 (server bug); JSON form -> 403.

### Headers
- Response headers: `Content-Type: application/json` (`text/plain` for ticker txt, `text/csv` + `Content-Disposition: attachment` for instruments csv), `Access-Control-Allow-Origin: http://127.0.0.1:5000`, strict CSP/X-Frame-Options/X-Content-Type-Options security headers. **No `X-RateLimit-*` or `Retry-After` headers** on normal responses.

### Rate limiting
- `API_RATE_LIMIT` on this instance is `100 per second` (env; code default 10/s for ping, 50/s elsewhere), flask-limiter moving-window, in-memory, keyed by remote address. Order endpoints use `ORDER_RATE_LIMIT=10 per second`. A burst of 103 pings completed in 2.6 s with zero 429s (server throughput < limit), so the 429 body was not observable; per flask-limiter defaults it would be `429 {"message": "..."}`.

### Date / time formats (four different ones)
- Request dates: `start_date`/`end_date`/`from`/`to`/`market/timings.date` = `YYYY-MM-DD`. `expiry_date` for optionsymbol/optionchain/syntheticfuture = `DDMMMYY` (`06OCT26`).
- `/expiry` returns `DD-MMM-YY` (`06-OCT-26`); `/symbol` rows return `expiry` as `DD-MMM-YY`; `/optiongreeks` returns `expiry_date` as `DD-Mon-YYYY` (`06-Oct-2026`); `/optionchain` echoes `DDMMMYY`. Clients must convert `DD-MMM-YY` -> `DDMMMYY` to build symbols (`NIFTY27OCT26FUT`, `NIFTY06OCT2622400CE`).
- History/ticker candle `timestamp` = epoch **seconds** (int). market/holidays & market/timings `start_time`/`end_time` = epoch **milliseconds**. optionchain `expiry_ts`/`server_ts` = epoch seconds. WebSocket `timestamp`/`ltt`/`server_timestamp` = epoch milliseconds.
- Ticker `format=txt`: `EXCH:SYMBOL,YYYY-MM-DD,o,h,l,c,v` for `D`; intraday adds `HH:MM:SS` as third column; no header line.

### Symbol conventions
- Equity `RELIANCE`/`NSE`; index `NIFTY`/`NSE_INDEX`; futures `NIFTY27OCT26FUT`/`NFO`, `CRUDEOIL19OCT26FUT`/`MCX`; options `NIFTY06OCT2622400CE`/`NFO`. NIFTY lot size 65, freeze 1800.

### WebSocket protocol (ws://127.0.0.1:8765, `websockets/15.0.1`)
- Client -> server JSON with `action` (alias `type`): `authenticate|auth` (`api_key` or `apikey`), `subscribe`, `unsubscribe`, `unsubscribe_all`, `subscribe_orders`, `unsubscribe_orders`, `get_broker_info`, `get_supported_brokers`, `ping`. Optional `request_id` is echoed on acks and on errors.
- Auth ack: `{"type":"auth","status":"success","message","broker","user_id","supported_features":{"ltp","quote","depth"}}`. Unauthenticated sockets are closed after 15 s with code **4401 "auth timeout"**; a failed auth does NOT close the socket. `ping` works before auth; everything else -> `NOT_AUTHENTICATED`.
- Subscribe: `{"action":"subscribe","symbols":[{"symbol","exchange"}],"mode":1|2|3|"LTP"|"Quote"|"Depth" (case-insensitive),"depth":5|20|50 (legacy key `depth_level` also accepted),"request_id"}`; single-symbol form `{"symbol","exchange"}` at top level also works. Mode as string digit `"2"` or float is rejected (`INVALID_MODE`).
- Subscribe ack: `{"type":"subscribe","status":"success|partial","subscriptions":[{"symbol","exchange","status","mode":"LTP|Quote|Depth","depth":N,"broker"} | {...,"status":"error","message"}],"message":"Subscription processing complete","broker","request_id"}`. Unknown symbol/exchange -> `status:"partial"` with per-item `"Token not found for X on Y"`; HTTP-level success.
- Unsubscribe ack: `{"type":"unsubscribe","status":"success|partial","message","successful":[{symbol,exchange,mode,status,broker}],"failed":[{...,"mode":null,"message"}],"broker","request_id"}`. `unsubscribe_all` uses the same `type:"unsubscribe"` ack listing every active key. Unsubscribing a never-subscribed key still reports success.
- Errors: `{"status":"error","code":"NOT_AUTHENTICATED|AUTHENTICATION_ERROR|INVALID_MODE|INVALID_PARAMETERS|INVALID_ACTION|INVALID_JSON|SERVER_ERROR|BROKER_ERROR|PROCESSING_ERROR","message", "request_id"?}` with no `type` field. `{}` -> `INVALID_ACTION "Invalid action: None"`; a JSON array -> `SERVER_ERROR "'list' object has no attribute 'get'"`.
- Market data: `{"type":"market_data","symbol","exchange","mode":1|2|3 (int),"broker","data":{...}}`. `data` repeats `symbol`, `exchange` and a lowercase `mode` label (`"ltp"|"quote"|"depth"`), plus `ltp`, `ltt` (ms), `timestamp` (ms); quote adds `volume,last_quantity,average_price,total_buy_quantity,total_sell_quantity,open,high,low,close` (index quote instead has `price_change`, `price_change_percent` and lacks OHLC when closed). A client subscribed at mode 2 also receives mode-1 copies when a mode-1 subscription exists on the same socket. Field names differ from the docs (`ltt` vs documented none; docs' `change`/`change_percent` appear as `price_change`/`price_change_percent`).
- Order stream: `subscribe_orders` -> `{"type":"subscribe_orders","status":"success","message":"Subscribed to order updates"}`; `unsubscribe_orders` mirrors it. No `order_update` events observed (WS capture predates session 2 orders).
- Misc acks: `pong` `{"type":"pong","status":"success","server_timestamp": ms}`; `broker_info` `{"type","status","broker","adapter_status":"connected","user_id"}`; `supported_brokers` `{"type","status","brokers":[36 names],"count":36}`.

### Not captured (deliberately) and read-only notes for the desktop clone
- Session 1 called no mutating endpoint; session 2 exercised them all in analyze mode (see section below). Request contracts (from `restx_api/schemas.py`, `*` = required): see `MUTATING_SCHEMAS.md`.
- `/sandbox/*` web routes (`/sandbox/`, `/sandbox/api/configs`, `/sandbox/update` POST, `/sandbox/reset` POST, `/sandbox/reload-squareoff` POST, `/sandbox/squareoff-status`, `/sandbox/mypnl`, `/sandbox/mypnl/api/data`, `/sandbox/mypnl/export/{daily,positions,holdings,trades}`) are all guarded by `@check_session_validity` (browser session cookie), not by apikey, so they are not reachable from the API-key client and were not called.


### Order-mutating endpoints (session 2, analyze mode only)
- **Mode field**: every order/account response in analyze mode carries top-level `"mode":"analyze"`. Exceptions: placeorder schema errors, optionsorder errors, optionsmultiorder validation errors and analyzer toggle errors have no `mode`. Live mode would omit it (toggle response reports `mode:"live"`).
- **Order id**: 14-digit numeric STRING `YYMMDD` + 8 digits (e.g. `26100376733573`). GTT id: `GTT-YYMMDD-<8 hex>`. Trade id: `TRADE-YYYYMMDD-HHMMSS-<8 hex>`.
- **Status strings** (`order_status`): `complete`, `open`, `trigger pending` (with a space), `cancelled`; statistics also count `rejected`. GTT status: `active`, `cancelled`; GTT `trigger_type` stored as `single` / `two-leg` (request uses `SINGLE` / `OCO`).
- **Price-type key differs**: orderbook rows use `pricetype`, orderstatus `data` uses `price_type`.
- **Timestamps**: order/trade `timestamp` = `YYYY-MM-DD HH:MM:SS` IST without zone; GTT `created_at`/`updated_at`/`expires_at` = ISO-8601 with microseconds, IST, no zone; funds `last_reset` = `YYYY-MM-DD HH:MM:SS`.
- **Error message shape on order endpoints**: marshmallow failures are a STRINGIFIED Python dict (`"{'quantity': ['Quantity must be a positive number.']}"`, nested `"{'orders': {0: {...}}}"` for basket) - unlike read endpoints (incl. gttorderbook) where `message` is a JSON object. optionsmultiorder uses yet another shape: `{status:'error', message:'Validation error', errors:{field:[...]}}`. Business errors are plain strings. A Rust client should accept `message` as `String | Object` and an optional `errors`.
- **HTTP codes**: 400 validation/business rule/unknown symbol; 403 bad apikey; 404 unknown orderid (`Order <id> not found`), unknown/inactive GTT, optionsorder bad expiry; 429 order rate limit (`{"message":"10 per 1 second"}`, no `status`, no Retry-After); 500 on two server bugs below.
- **Batch shapes**: basketorder/splitorder/optionsmultiorder return HTTP 200 + `status:"success"` even when some legs fail; per-leg `status` decides. optionsorder with `splitsize` swaps `orderid` for `results[]` + `split_size` + `total_quantity`.
- **Success-without-action**: cancelallorder with nothing open, closeposition with no positions, and placesmartorder no-op cases all return 200 `status:"success"` with only a `message`.
- **Sandbox behaviour**: MARKET orders fill instantly at LTP even on a Saturday (bid/ask 0); MIS is blocked only between square-off time (15:15 NSE) and 09:00 IST; quantity must be a lot multiple for F&O; same-day CNC buys do not appear in holdings; closed positions disappear from positionbook.
- **Server bugs found** (web app, analyze mode): (1) `cancelallorder` filters on `"trigger_pending"` but stored status is `"trigger pending"`, so SL/SL-M trigger-pending orders are NOT cancelled (`services/sandbox_service.py` sandbox_cancel_all_orders). (2) Any failed sandbox `modifygttorder` returns 500 because `GTTModifyFailedEvent` (events/order_events.py) has no `exchange` field but `services/modify_gtt_order_service.py` passes `exchange=`. (3) After an OCO GTT modify raised `margin_blocked` (1059.1 -> 1106.8) and a later closeposition, `cancelgttorder` returns 500 "Could not release 1106.80 margin ... more than the reserved margin"; the GTT `GTT-261003-56ad1482` stays `active` in the sandbox and could not be cleaned up via API.
