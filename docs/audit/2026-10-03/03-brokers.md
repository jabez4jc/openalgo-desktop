# 03 - Broker Adapters Audit (OpenAlgo Desktop vs OpenAlgo Web)

Date: 2026-10-03
Desktop repo: `/Users/openalgo/openalgo-desktop/openalgo-desktop` (Tauri 2 + Rust, `src-tauri/src`)
Reference: `/Users/openalgo/openalgo-desktop/openalgo` (Flask web app, read-only)
No file in either repo was modified. All paths below are absolute unless prefixed `D:` (desktop `src-tauri/src/`) or `W:` (web `openalgo/`).

Contracts read first: `W:.claude/skills/broker-integration/SKILL.md` + `references/*.md`, `W:docs/prompt/symbol-format.md`, `order-constants.md`, `websockets-format.md`, `services_documentation.md`.

## Report structure

| Part | Content |
| --- | --- |
| 0 | Cross-cutting findings that affect every desktop adapter (trait, services, symbol DB, websocket manager) |
| A | Function-by-function audit of the three existing desktop adapters: angel, zerodha, fyers |
| B | Implementable spec sheets for the brokers to add next: upstox, dhan, kotak, groww |
| C | Recommended Rust module layout and Broker-trait additions for MVP parity |
| D | Family-based plan for the remaining brokers (all 36 web plugins classified), incl. deltaexchange (crypto) |

---

# Part 0 - Cross-cutting findings (verified directly, apply to all adapters)

## 0.1 The Broker trait is missing most of the web module contract

`D:brokers/mod.rs:16-83` defines 14 methods. The web contract (`W:.claude/skills/broker-integration/SKILL.md` "Step 3 - the module contract") requires the following that the desktop trait does not have at all:

| Web function | Desktop status | Where the gap shows |
| --- | --- | --- |
| `BrokerData.get_history(symbol, exchange, interval, start, end)` | Missing (no trait method) | `D:services/history_service.rs:60-99` serves DuckDB cache only; `:90` and `:137` are `TODO`. `/history` and Historify can never fetch from a broker. |
| `BrokerData.timeframe_map` (intervals) | Missing | `D:services/history_service.rs:102-120` hardcodes 12 intervals (`1m..1M`) regardless of broker; web `intervals_service` advertises only what the broker serves (fyers has seconds, zerodha has no W/M). |
| `BrokerData.get_multiquotes(symbols)` | Missing as a distinct capability | `D:services/quotes_service.rs:95-102` just forwards to `get_quote`; each adapter decides whether it batches (none batch correctly, see Part A). The options tools in `D:services/options_service.rs:114-284` call `QuotesService::get_quote` one symbol at a time. |
| `calculate_margin_api(positions, auth)` | Missing | No `/margin` equivalent anywhere (`grep -ril margin services/` finds only funds). |
| `cancel_all_orders_api(data, auth)` | Partially emulated in service | `D:services/order_service.rs:220-246` fetches the book and cancels rows whose `status` is `"PENDING" / "OPEN" / "TRIGGER PENDING"` (uppercase). Web `map_order_data` lowercases statuses (`open`, `trigger pending`); angel and fyers adapters return lowercase, zerodha returns Kite's `OPEN`/`TRIGGER PENDING`. Result: cancel-all is a no-op for angel/fyers and works only for zerodha. |
| `close_all_positions(api_key, auth)` | Emulated in service | `D:services/position_service.rs:167-200` loops `close_position` -> `OrderService::place_order`, which looks the symbol up by OpenAlgo symbol (`D:services/order_service.rs:67`). Because the adapters return broker trading symbols in `Position.symbol` (e.g. `RELIANCE-EQ`, `NIFTY25OCT25000CE`), the lookup misses and the close order is sent with no `broker_symbol`/`symbol_token` (falls back to the raw symbol / token `"0"`). |
| `get_open_position(symbol, exchange, product, auth)` | Emulated in service | `D:services/position_service.rs:66-84` compares `p.symbol` with the OpenAlgo symbol; same symbol-vocabulary mismatch means smart orders (`D:services/smart_order_service.rs:80`) see `current_qty = 0` for most instruments and re-enter positions. |
| GTT (`place/modify/cancel_gtt_order`, `get_gtt_book`) | Missing | Web gates on module presence and returns 501; desktop has no capability flag. |
| `<Name>WebSocketAdapter` factory | Missing | Streaming is a hardcoded 3-way `match broker_id` in `D:websocket/manager.rs:166-176, 423-430, 883-900`; adding a broker means editing the manager. |

## 0.2 The symbol master drops expiry / strike / option type

- `D:brokers/types.rs:158-173 SymbolData` carries `expiry`, `strike`, `option_type`.
- `D:state.rs:35-47 SymbolInfo` does not; `D:services/symbol_service.rs:217-230` converts `SymbolData -> SymbolInfo` and silently drops them.
- `D:db/sqlite/migrations.rs:116-128` creates `symtoken(expiry TEXT, strike REAL, option_type TEXT)` but `D:db/sqlite/symbol.rs:28-31` inserts only 9 columns and `load_symbols` (`:65-67`) never reads them.
- Consequences: `SymbolSearchResult.strike/expiry` are always `None` (`D:services/symbol_service.rs:73-74, 98-99`); `get_expiry_dates` (`:161-198`) falls back to slicing 5 characters off the symbol suffix (`:251-263`), which cannot represent the OpenAlgo `DDMMMYY` 7-character expiry and returns strings like `30OCT` instead of `30-OCT-25`; the option-chain builder has no strike column to sort on.
- Web contract (`W:.claude/skills/broker-integration/references/master-contract.md`): `expiry` must be `DD-MMM-YY` uppercase, `instrumenttype` only `EQ/FUT/CE/PE`, index rows use `EQ` with exchange `NSE_INDEX/BSE_INDEX/MCX_INDEX`.
- Fix: add `expiry: Option<String>, strike: Option<f64>, option_type: Option<String>` (and ideally `contract_value`) to `SymbolInfo`, write/read them in `symbol.rs`, build expiry dropdowns from the column, and store expiry in `DD-MMM-YY`.

## 0.3 Broker symbols leak into normalised books

Web always runs `map_*` (broker code -> OpenAlgo code, `get_oa_symbol(brsymbol, exchange)`) before `transform_*` (`W:.claude/skills/broker-integration/references/data-and-account.md`, Part 2). The desktop adapters return the broker's own `tradingsymbol` in `Order.symbol`, `Position.symbol`, `Holding.symbol` (details per broker in Part A). Because the desktop symbol cache is keyed by `exchange:symbol` (OpenAlgo) and `exchange:token`, every downstream consumer (close position, smart order, orderbook UI, webhook `orderstatus`) sees a different vocabulary than it sends. The fix is a shared helper `symbol_cache.oa_symbol_from_brsymbol(exchange, brsymbol)` (needs a third index `exchange:brsymbol -> token`) used by all adapters.

## 0.4 Quote argument order is inconsistent

`D:brokers/mod.rs:67-71` documents `symbols: Vec<(String, String)>` without naming the tuple. `D:services/quotes_service.rs:33` and `D:commands/quotes.rs:30-34` build `(exchange, symbol)`. The angel (`D:brokers/angel/mod.rs:904`) and zerodha (`D:brokers/zerodha/mod.rs:749, 780`) adapters destructure `(symbol, exchange)`. Every quote request therefore asks for e.g. `RELIANCE:NSE`. Replace the tuple with a named struct `QuoteKey { exchange, symbol }`.

## 0.5 WebSocket manager (`D:websocket/manager.rs`, 1129 lines) - what it actually covers

What exists:
- One tokio task per connection, `mpsc` command channel, Tauri event `market_tick` (`:240`), `websocket_disconnected` (`:258`), `websocket_error` (`:263`). A `MarketDepth` struct is defined (`:92-99`) but **never emitted**: only `MarketTick` (best bid/ask only) reaches the UI, so OpenAlgo "Depth" mode (5 levels buy/sell with `orders`) is not implemented for any broker.
- URL/handshake per broker (`:166-200`), binary parsers (`:423-861`), subscribe/unsubscribe builders (`:883-1129`), a 30 s heartbeat (`:296-312`).
- No reconnect/backoff, no token refresh, no stall watchdog, no re-subscribe after reconnect (web requirements in `W:.claude/skills/broker-integration/references/streaming.md`).
- Not wired to the `Broker` trait: `commands/websocket.rs:37-68` pulls `feed_token` from the auth table and `api_key` from credentials and calls `manager.connect(broker_id, client_id, api_key, feed_token)`; the broker adapter has no say in URL, auth or framing.

Per-broker verification against web parsers:

| Broker | Desktop | Verified web behaviour | Verdict |
| --- | --- | --- | --- |
| angel | URL + headers `:167-170, 182-189`; sends `Authorization: Bearer <feed_token>` | `W:broker/angel/streaming/smartWebSocketV2.py` sends raw JWT (auth token) in `Authorization`, `x-api-key`, `x-client-code`, `x-feed-token` | Bug: `Bearer ` prefix and wrong token (feed token instead of auth JWT) |
| angel | binary offsets `:442-560`: mode@0, exch@1, token 2..27, seq 27..35, ts 35..43, LTP 43..51 (/100), quote fields 51..123, OI 131..139, best-5 at 147 (20-byte records) | `smartWebSocketV2.py:561-630` identical offsets; best-5 split by `flag` (0/1) then swapped at `:629-630` so buy = flag 1, sell = flag 0 | Offsets OK. Bug: desktop assumes positional order (first 5 sell, next 5 buy, `:544-558`) instead of using the flag; depth levels 2-5 discarded; `exchangeType` derived from OA exchange so `NSE_INDEX/BSE_INDEX/MCX_INDEX` map to 1 (`:909-918`); unsubscribe always `mode: 1` (`:965`) |
| zerodha | URL `:171-174` uses `feed_token` as `access_token` | Desktop stores `public_token` as `feed_token` (`D:brokers/zerodha/mod.rs:342`); Kite needs the `access_token` half of `api_key:access_token` | Bug: 403 on every connect |
| zerodha | packet framing `:567-597` (u16 count, u16 len) | `W:broker/zerodha/streaming/zerodha_websocket.py:681-702` identical | OK |
| zerodha | fields: LTP/100, quote 44 bytes, depth @64 (`:600-680`) | `zerodha_websocket.py:710-800` identical /100 (web also does not special-case CDS/BCD divisors) | OK (shared limitation) |
| zerodha | exchange = `(token/256) % 256` with table 1 NSE, 2 NFO, 3 BSE, 4 BFO, 5 MCX, 6 CDS (`:606-620`) | web uses `token_exchange_map` populated at subscribe (`:735-737`); Kite segment is `token & 0xFF` (1 NSE, 2 NFO, 3 CDS, 4 BSE, 5 BFO, 6 BCD, 7 MCX, 9 INDICES) | Bug: wrong formula and wrong table; the correct exchange sitting in `token_map` is discarded |
| zerodha | subscribe = one text frame `"{sub}\n{mode}"` (`:1001`) | web sends two separate JSON frames | Bug: not valid JSON, Kite drops it |
| zerodha | 1-byte binary heartbeat sent every 30 s (`:305-309`) | Kite sends 1-byte heartbeats to the client; client sends nothing | Harmless but wrong direction |
| zerodha | index packets (28/32 bytes) | web branches on length (`:720-724`) and index packets carry OHLC at different offsets | Desktop parses only LTP for indices (len < 44) |
| fyers | `commands/websocket.rs:52-53` requires `feed_token`; fyers `authenticate` returns `feed_token: None` (`D:brokers/fyers/mod.rs:569`) | n/a | Bug: fyers streaming can never connect ("No feed token found") |
| fyers | sends `Authorization: <feed_token>` header (`:192`) and uses `feed_token` verbatim as HSM key (`:206`) | `W:broker/fyers/streaming/fyers_hsm_websocket.py:207-245` extracts `hsm_key` from the JWT payload of the access token; `:933-945` documents that the gateway silently drops a handshake carrying any `Authorization` header | Bug x2 |
| fyers | auth frame builder `:1021-1056` | `fyers_hsm_websocket.py:250-290` byte-identical layout | OK |
| fyers | subscribe frame uses `"{exchange}:{token}"` strings (`:1067`) | web subscribes HSM tokens of the form `sf|nse_cm|2885` obtained via `https://api-t1.fyers.in/data/symbol-token` (`fyers_hsm_websocket.py:30`, `fyers_token_converter.py`) | Bug: wrong symbol vocabulary; nothing will tick |
| fyers | snapshot parser `:748-861` | web `fyers_hsm_websocket.py:540-720` (scrip/index/depth packet types with multiplier/precision) | Desktop handles only type `S` scrips, drops `U` updates (`:722-733`), never parses index or depth packets |

Conclusion: binary parsing is partially implemented (angel and zerodha quote-level offsets match the web), but none of the three brokers can currently establish an authenticated feed from the desktop as written, depth is not surfaced, and the manager is not pluggable.

## 0.6 Other shared gaps
- `OrderRequest`/`ModifyOrderRequest` (`D:brokers/types.rs:7-37`) lack the fields brokers need for modify (exchange, symbol, token, product) and for emulation (MPP market protection, `tag`), and `ModifyOrderRequest` has no `symbol/exchange/product`, which the angel modify endpoint requires.
- `Funds` (`:105-116`) has no `m2m_unrealized/m2m_realized` fields, which the web `/funds` response requires (`availablecash, collateral, m2munrealized, m2mrealized, utiliseddebits`).
- `Order.status` is not normalised to the web's lowercase set; `webhook/handlers.rs` forwards it verbatim.
- Redirect URL convention: web uses `http://127.0.0.1:5000/<broker>/callback` built from `REDIRECT_URL`; the desktop has no single callback handler convention (each adapter expects `request_token`/`auth_code` already pasted into `BrokerCredentials`).


---

# Part A - Existing desktop adapters (angel, zerodha, fyers)

Each sub-section is a function-by-function comparison (Function | Web behaviour | Desktop status OK/Bug/Missing | file:line | Fix). Headline bugs were independently re-verified by the lead auditor against the source (and, for zerodha, against the live Kite instruments header).

---

## A. Angel One (SmartAPI) adapter audit: Desktop (Rust) vs Web (Python reference)

Scope: read-only comparison of
`/Users/openalgo/openalgo-desktop/openalgo-desktop/src-tauri/src/brokers/angel/mod.rs` (1294 lines) plus
`brokers/mod.rs`, `brokers/types.rs`, `services/{order,position,symbol,quotes,history}_service.rs`,
`websocket/manager.rs`, `commands/websocket.rs`, `state.rs`, `db/sqlite/{symbol,migrations}.rs`
against `/Users/openalgo/openalgo-desktop/openalgo/broker/angel/**` and
`docs/prompt/{symbol-format,order-constants}.md`.

Path abbreviations used below:
- `D:` = `/Users/openalgo/openalgo-desktop/openalgo-desktop/src-tauri/src/`
- `W:` = `/Users/openalgo/openalgo-desktop/openalgo/broker/angel/`

Severity legend: **P0** = feature does not work / wrong data sent to broker; **P1** = wrong data shown or
contract divergence from the web app; **P2** = minor / hardening.

---

## 0. Cross-cutting findings (affect every endpoint)

| # | Finding | Desktop location | Severity |
|---|---------|------------------|----------|
| X1 | **`X-PrivateKey` is sent EMPTY on every authenticated call.** Every secure call does `self.get_headers("", Some(auth_token))`, and `get_headers` puts the first arg into `X-PrivateKey` (`D:brokers/angel/mod.rs:42`). Web sends `BROKER_API_KEY` in `X-PrivateKey` on all calls (`W:api/order_api.py:40,188,404,463`, `W:api/data.py:93`, `W:api/funds.py:57`, `W:api/margin_api.py:51`). SmartAPI validates this header; empty value risks 401/403 on orders, book, funds, quotes. | `D:brokers/angel/mod.rs:502,573,628,655,714,767,816,864,937,1032` | P0 |
| X2 | Hard-coded dummy client headers: `X-ClientLocalIP: 127.0.0.1`, `X-ClientPublicIP: 127.0.0.1`, `X-MACAddress: 00:00:00:00:00:00` (`D:brokers/angel/mod.rs:39-41`). Web also hard-codes placeholders (`"CLIENT_LOCAL_IP"`, `"CLIENT_PUBLIC_IP"`, `"MAC_ADDRESS"`, `W:api/auth_api.py:25-27`), so this is parity, not a regression. Neither derives real values. | `D:brokers/angel/mod.rs:39-41` | P2 (parity) |
| X3 | No rate limiting / 403-"exceeding access rate" retry. Web has a shared per-category limiter (quote 0.15 s, history 0.5 s) and exponential back-off on 403/429 (`W:api/data.py:36-170`, `W:api/order_api.py:24-82`). Desktop does `response.json().await?` directly; Angel's plain-text rate-limit body surfaces as a serde "error decoding response body". | all `D:brokers/angel/mod.rs` request sites | P2 |
| X4 | **No broker-symbol -> OpenAlgo-symbol reverse mapping anywhere in the desktop.** Web maps `tradingsymbol` back through `get_symbol(symboltoken, exchange)` / `get_oa_symbol(brsymbol, exchange)` for orderbook, tradebook, positions, holdings (`W:mapping/order_data.py:48-56,183-191,298-304`). Desktop adapter returns Angel's raw `tradingsymbol` (e.g. `RELIANCE-EQ`) and no service layer converts it (`grep brsymbol services/` only hits symbol_service). Consequence: `PositionService::close_position` -> `OrderService::place_order` does `get_symbol_by_name(exchange, "RELIANCE-EQ")` -> miss -> `symboltoken "0"` -> broker rejects. Also breaks smart-order netting and UI symbol display. | `D:brokers/angel/mod.rs:686,739,790,841`; `D:services/order_service.rs:67-77`; `D:services/position_service.rs:76-81,141-157` | P0 |
| X5 | `SymbolInfo` drops `expiry`, `strike`, `option_type` even though the `symtoken` table has those columns (`D:db/sqlite/migrations.rs:125-127`). `store_symbols` never writes them (`D:db/sqlite/symbol.rs:29-48`), `SymbolService` returns `strike: None, expiry: None` (`D:services/symbol_service.rs:73-74,98-99`), and `get_expiry_dates` falls back to slicing 5 chars off the symbol string (`D:services/symbol_service.rs:251-263`). Web stores and queries `expiry`, `strike`, `lotsize`, `instrumenttype` (`W:database/master_contract_db.py:38-41`). Option chain / expiry APIs cannot be correct. | `D:state.rs:35-47`; `D:services/symbol_service.rs:217-230` | P1 |

---

## 1. Function-by-function comparison

### 1.1 authenticate

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `POST https://apiconnect.angelone.in/rest/auth/angelbroking/user/v1/loginByPassword` (`W:api/auth_api.py:32`) | OK | `D:brokers/angel/mod.rs:376-379` | - |
| Headers | `Content-Type, Accept, X-UserType: USER, X-SourceID: WEB, X-ClientLocalIP, X-ClientPublicIP, X-MACAddress, X-PrivateKey: api_key` (`W:api/auth_api.py:20-29`) | OK (api_key correctly passed here, `get_headers(&credentials.api_key, None)`) | `D:brokers/angel/mod.rs:33-52,380` | - |
| Body | `{"clientcode","password","totp"}` (`W:api/auth_api.py:19`) | OK | `D:brokers/angel/mod.rs:361-372` | - |
| Response | returns `jwtToken` as auth_token, `feedToken` as feed_token; `refreshToken` ignored (`W:api/auth_api.py:43-47`) | OK (refreshToken deserialised then dropped, same as web; `user_name: None`) | `D:brokers/angel/mod.rs:385-415` | Optional: fetch `/rest/secure/angelbroking/user/v1/getProfile` for user_name. |
| Error handling | returns `message` on missing jwtToken | OK | `D:brokers/angel/mod.rs:402-404` | Treat HTTP != 200 / non-JSON body explicitly (currently serde error). |

### 1.2 place_order

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `POST /rest/secure/angelbroking/order/v1/placeOrder` (`W:api/order_api.py:215`) | OK | `D:brokers/angel/mod.rs:496-499` | - |
| Headers | includes `X-PrivateKey: api_key` (`W:api/order_api.py:179-189`) | **Bug** (X1: empty) | `D:brokers/angel/mod.rs:502` | Store api_key in `AngelBroker` (or in `BrokerSession`) and pass it to `get_headers`. |
| symboltoken / tradingsymbol | `get_token(symbol, exchange)`, `get_br_symbol(symbol, exchange)` (`W:api/order_api.py:177`, `W:mapping/transform_data.py:11`) | OK (resolved by `OrderService` from cache; falls back to `"0"` with a warning instead of failing) | `D:services/order_service.rs:67-77`; `D:brokers/angel/mod.rs:438-453` | Fail fast when token is missing rather than sending `symboltoken: "0"`. |
| variety | `map_variety`: MARKET/LIMIT -> `NORMAL`, SL/SL-M -> `STOPLOSS` (`W:mapping/transform_data.py:79-84`). Web has no AMO. | OK (+ AMO superset). Note: SL + AMO yields `STOPLOSS`, AMO flag dropped. | `D:brokers/angel/mod.rs:458-461` | Acceptable. |
| ordertype | MARKET->MARKET, LIMIT->LIMIT, SL->STOPLOSS_LIMIT, SL-M->STOPLOSS_MARKET, default MARKET (`W:mapping/transform_data.py:54-64`) | OK | `D:brokers/angel/mod.rs:464-470` | - |
| producttype | CNC->DELIVERY, NRML->CARRYFORWARD, MIS->INTRADAY, default INTRADAY (`W:mapping/transform_data.py:67-76`). **Web applies no per-exchange rule** (CNC on NFO stays DELIVERY). | OK (parity) | `D:brokers/angel/mod.rs:473-478` | None needed for parity; a CNC-on-derivatives -> CARRYFORWARD guard would be an improvement over web. |
| transactiontype | `data["action"].upper()` (`W:mapping/transform_data.py:18`) | OK-ish (passes `order.side` un-uppercased) | `D:brokers/angel/mod.rs:484` | `.to_uppercase()`. |
| duration | hard-coded `"DAY"` (`W:mapping/transform_data.py:22`) | OK (passes `order.validity`, DAY/IOC superset) | `D:brokers/angel/mod.rs:488` | - |
| price | string from request, default `"0"` | OK (`f64::to_string`) | `D:brokers/angel/mod.rs:489` | - |
| squareoff | `"0"` | OK | `D:brokers/angel/mod.rs:490` | - |
| stoploss | **web sends `trigger_price`** as `stoploss` (`W:mapping/transform_data.py:25`, forwarded `W:api/order_api.py:203`) | Minor divergence: always `"0"` | `D:brokers/angel/mod.rs:491` | Send trigger_price (or `"0"`) to match web. |
| triggerprice | always a string, default `"0"` (`W:api/order_api.py:201`) | **Bug**: `Option<String>` with no `skip_serializing_if` -> serialises `"triggerprice": null` for MARKET/LIMIT orders | `D:brokers/angel/mod.rs:434,493` | `order.trigger_price.map(..).unwrap_or_else(|| "0".into())`. |
| quantity | string | OK | `D:brokers/angel/mod.rs:492` | - |
| disclosedquantity | web's final payload omits it (`W:api/order_api.py:190-206`) | OK (parity) | - | - |
| `#[serde(rename_all = "camelCase")]` | n/a | Harmless (all field names are single lowercase words, so no renaming occurs) but misleading | `D:brokers/angel/mod.rs:420` | Remove attribute. |
| Response | `status is True` -> `data.orderid` (`W:api/order_api.py:234-238`) | OK | `D:brokers/angel/mod.rs:505-533` | - |

### 1.3 modify_order

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `POST /rest/secure/angelbroking/order/v1/modifyOrder` (`W:api/order_api.py:469`) | OK | `D:brokers/angel/mod.rs:567-570` | - |
| Required body | `variety (map_variety(pricetype)), orderid, ordertype (mapped), producttype (mapped), duration "DAY", price, quantity, tradingsymbol (br symbol), symboltoken, exchange, disclosedquantity, stoploss` (`W:mapping/transform_data.py:37-51`, `W:api/order_api.py:447-450`) | **Bug / Missing**: desktop sends only `variety:"NORMAL", orderid, quantity?, price?, ordertype?, triggerprice?, duration?`. Missing `tradingsymbol`, `symboltoken`, `exchange`, `producttype` which SmartAPI requires -> modify will be rejected. `variety` is hard-coded NORMAL even for SL orders (web: STOPLOSS). `ordertype` is passed as the raw OpenAlgo pricetype (`"SL"`), not mapped to `STOPLOSS_LIMIT`. | `D:brokers/angel/mod.rs:541-565` | Extend `ModifyOrderRequest` (`D:brokers/types.rs:31-37`) with `symbol, exchange, product, order_type`; in `OrderService::modify_order` resolve brsymbol+token from cache like place_order; map variety/ordertype/producttype with the same tables as place_order; always send `duration: "DAY"` and `producttype`. |
| Headers | `X-PrivateKey` | **Bug** (X1) | `D:brokers/angel/mod.rs:573` | as X1 |
| Success check | `status == "true" or message == "SUCCESS"` (`W:api/order_api.py:479`) | OK (checks bool `status`) | `D:brokers/angel/mod.rs:588-599` | - |

### 1.4 cancel_order

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `POST /rest/secure/angelbroking/order/v1/cancelOrder` (`W:api/order_api.py:417`) | OK | `D:brokers/angel/mod.rs:623-626` | - |
| Body | `{"variety":"NORMAL","orderid"}` always (`W:api/order_api.py:408-413`) | OK (`variety.unwrap_or("NORMAL")`; caller passes through from UI `D:commands/orders.rs:64-68`) | `D:brokers/angel/mod.rs:611-620` | - |
| Headers | `X-PrivateKey` | **Bug** (X1) | `D:brokers/angel/mod.rs:628` | as X1 |

### 1.5 cancel_all_orders

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Filter | orderbook rows with `status in ["open", "trigger pending"]` (lowercase, Angel's native strings) (`W:api/order_api.py:499-503`) | **Bug**: filters `"PENDING" \|\| "OPEN" \|\| "TRIGGER PENDING"` (uppercase). Angel returns lowercase `open` / `trigger pending`, the adapter passes `o.status` through unchanged (`D:brokers/angel/mod.rs:698`), so **nothing is ever cancelled** for Angel. | `D:services/order_service.rs:226` | Compare case-insensitively, or normalise status in the adapter. |

### 1.6 close_all_positions / get_open_position

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| get_open_position | converts OA symbol to br symbol via `get_br_symbol`, then matches `tradingsymbol == br_symbol && exchange && producttype (Angel value)` on raw position book; returns `netqty` string (`W:api/order_api.py:151-170`) | **Bug** (X4 chain): `PositionService::get_open_position` matches `p.symbol` (raw Angel `RELIANCE-EQ`) against the OA symbol (`RELIANCE`) -> never matches for NSE/BSE equities (works by accident for NFO where Angel symbol == OA symbol). | `D:services/position_service.rs:66-84` | Map position symbol to OA symbol in the adapter via token (`symboltoken` is not even deserialised in `AngelPositionData`, `D:brokers/angel/mod.rs:107-135`; add it and use `state.get_symbol_by_token`). |
| close_all_positions | for each `netqty != 0`: `symbol = get_symbol(symboltoken, exchange)`, product = `reverse_map_product_type`, MARKET order (`W:api/order_api.py:335-383`) | **Bug** (X4 chain): iterates positions and calls `close_position(exchange, p.symbol, p.product)` -> `get_open_position` miss -> `NotFound` error, or if it matched, `place_order` cannot resolve token. | `D:services/position_service.rs:167-200` | Same as above. |

### 1.7 orderbook

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `GET /rest/secure/angelbroking/order/v1/getOrderBook` (`W:api/order_api.py:86`) | OK | `D:brokers/angel/mod.rs:650-653` | - |
| Symbol | `get_symbol(symboltoken, exchange)` -> OA symbol (`W:mapping/order_data.py:48-56`) | **Bug** (X4): raw `tradingsymbol`; `symboltoken` not deserialised | `D:brokers/angel/mod.rs:71-104,686` | Add `symboltoken` to `AngelOrderData`, map to OA symbol. |
| producttype -> product | NSE/BSE+DELIVERY->CNC; INTRADAY->MIS; NFO/MCX/BFO/CDS+CARRYFORWARD->NRML (`W:mapping/order_data.py:57-69`) | OK | `D:brokers/angel/mod.rs:672-677` | - |
| ordertype -> pricetype | STOPLOSS_LIMIT->SL, STOPLOSS_MARKET->SL-M, else as-is (`W:mapping/order_data.py:135-139`) | OK | `D:brokers/angel/mod.rs:666-670` | - |
| status | raw Angel string (`open`, `complete`, `rejected`, `cancelled`, `trigger pending`) (`W:mapping/order_data.py:151`) | OK (raw; but see 1.5 for consumer bug) | `D:brokers/angel/mod.rs:698` | - |
| price | `averageprice or price` (`W:mapping/order_data.py:146`) | OK (both exposed) | `D:brokers/angel/mod.rs:692-694` | - |
| timestamp | `updatetime` | OK | `D:brokers/angel/mod.rs:701` | - |

### 1.8 tradebook

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `GET /rest/secure/angelbroking/order/v1/getTradeBook` (`W:api/order_api.py:90`) | OK | `D:brokers/angel/mod.rs:709-712` | - |
| Fields | `fillprice` -> average_price, `tradevalue`, `filltime` -> timestamp, `quantity` (`W:mapping/order_data.py:213-228`); symbol via `get_oa_symbol(tradingsymbol, exchange)` | **Bug**: reuses `AngelOrderData`; reads `price/averageprice/filledshares/updatetime` which the trade book does not carry -> `average_price`, `price`, `filled_quantity`, `order_timestamp` all 0/empty. Raw symbol (X4). `order_type` not reverse-mapped (minor). | `D:brokers/angel/mod.rs:707-758` | Add an `AngelTradeData` struct with `fillprice, fillsize, filltime, tradevalue, symboltoken`; map `average_price = fillprice`, `order_timestamp = filltime`. |

### 1.9 positions

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `GET /rest/secure/angelbroking/order/v1/getPosition` (`W:api/order_api.py:94`) | OK | `D:brokers/angel/mod.rs:762-765` | - |
| Fields | `netqty` -> quantity (int), `avgnetprice`, `ltp`, `pnl` (floats via `_to_float`) (`W:mapping/order_data.py:235-248`) | OK (plus realised/unrealised/buy/sell extras) | `D:brokers/angel/mod.rs:785-803` | - |
| Symbol | `get_symbol(symboltoken, exchange)` (`W:mapping/order_data.py:231-232` -> `map_order_data`) | **Bug** (X4) | `D:brokers/angel/mod.rs:790` | as X4 |
| product | same 3-rule mapping | OK | `D:brokers/angel/mod.rs:780-785` | - |

### 1.10 holdings

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `GET /rest/secure/angelbroking/portfolio/v1/getAllHolding` (`W:api/order_api.py:98`) | OK | `D:brokers/angel/mod.rs:811-814` | - |
| Null handling | `holdings: null` -> `[]`, `totalholding: None` (`W:mapping/order_data.py:254,285-290`) | OK | `D:brokers/angel/mod.rs:826-833` | - |
| Symbol / product | `get_oa_symbol(tradingsymbol, exchange)`; product forced to `CNC` (`W:mapping/order_data.py:298-313`) | **Bug** (X4) for symbol; `Holding` struct has no `product` field (contract gap vs web `/holdings` which returns `product: CNC`) | `D:brokers/angel/mod.rs:841`; `D:brokers/types.rs:90-102` | Map symbol; add `product: "CNC"`. |
| totalholding stats | `totalholdingvalue, totalinvvalue, totalprofitandloss, totalpnlpercentage` (`W:mapping/order_data.py:320-340`) | **Missing**: `AngelTotalHolding` parses only `totalholdingvalue` and discards it (`#[allow(dead_code)]`); desktop recomputes `current_value = qty*ltp` per row only | `D:brokers/angel/mod.rs:168-173,834-852` | Extend trait/return type to carry portfolio statistics, or compute all four totals in a service. |

### 1.11 funds

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `GET /rest/secure/angelbroking/user/v1/getRMS` (`W:api/funds.py:61`) | OK | `D:brokers/angel/mod.rs:859-862` | - |
| collateral | `availablecash - utilisedpayout` (`W:api/funds.py:88-89`) | OK | `D:brokers/angel/mod.rs:880` | - |
| availablecash | **derived**: `raw_availablecash + utiliseddebits - collateral` (`W:api/funds.py:94`) because Angel's raw `availablecash` is a net-margin figure | **Bug**: desktop returns raw `availablecash` | `D:brokers/angel/mod.rs:874,883` | `available_cash = raw + utiliseddebits - collateral`. |
| m2mrealized / m2munrealized | derived from the position book: qty==0 -> realised, else unrealised (`W:api/funds.py:16-38,96`) | **Missing**: `Funds` struct (`D:brokers/types.rs:106-116`) has no m2m fields; adapter does not fetch positions | `D:brokers/angel/mod.rs:857-891` | Add `m2m_realized/m2m_unrealized` to `Funds`; derive from `get_positions` as web does. |
| utiliseddebits | returned as string (`W:api/funds.py:103`) | Parsed but not exposed (`AngelFundsData.utiliseddebits` unused); desktop instead exposes `used_margin = utilisedmargin`, `total_margin = net`, `opening_balance = payin = availableintradaypayin`, `span`, `exposure` | `D:brokers/angel/mod.rs:881-890` | Align `/funds` REST output with web contract (`availablecash, collateral, m2munrealized, m2mrealized, utiliseddebits`). |

### 1.12 margin API

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| calculate_margin | `POST /rest/secure/angelbroking/margin/v1/batch` with `{"positions":[{exchange, qty, price, productType, token, tradeType, orderType}]}`; response `data.totalMarginRequired`, `data.marginComponents.spanMargin`, exposure=0 (`W:api/margin_api.py:65`, `W:mapping/margin_data.py:10-150`) | **Missing**: no margin method on the `Broker` trait or in the adapter (`grep margin/v1` -> no hits) | `D:brokers/mod.rs:16-83` | Add `calculate_margin` to the trait and implement. |

### 1.13 quotes

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint / mode | `POST /rest/secure/angelbroking/market/v1/quote/` with `{"mode":"FULL","exchangeTokens":{exchange:[token]}}` (`W:api/data.py:258`) | OK (endpoint + mode string) | `D:brokers/angel/mod.rs:921-934` | - |
| Tuple order | n/a | **Bug**: `QuotesService` passes `(exchange, symbol)` (`D:services/quotes_service.rs:32`, `D:commands/quotes.rs:32`, `D:webhook/handlers.rs:1477`) but the Angel adapter destructures `for (symbol, exchange) in &symbols` -> `exchangeTokens` is keyed by the symbol name and contains the exchange string as the "token". Fyers adapter uses `(ex, sym)` (`D:brokers/fyers/mod.rs:1065`); Zerodha uses `(symbol, exchange)` (`D:brokers/zerodha/mod.rs:749`) -> inconsistent across adapters. | `D:brokers/angel/mod.rs:904` | Fix destructuring to `(exchange, symbol)` and make the convention explicit in the trait doc. |
| Token resolution | `get_token(symbol, exchange)` (`W:api/data.py:244`) | **Bug** (acknowledged TODO): pushes the *symbol* into `exchangeTokens` ("Should be token, not symbol") -> Angel returns nothing / error for every quote | `D:brokers/angel/mod.rs:905-917` | Resolve token via `state.get_symbol_by_name` in `QuotesService` and pass `(exchange, token)` (or extend tuple to include token). |
| Index exchange normalisation | NSE_INDEX->NSE, BSE_INDEX->BSE, MCX_INDEX->MCX (`W:api/data.py:247-252`) | OK | `D:brokers/angel/mod.rs:907-912` | - |
| Response fields | `ltp, open, high, low, close->prev_close, tradeVolume->volume, opnInterest->oi, depth.buy[0]/sell[0] -> bid/ask` (`W:api/data.py:285-295`) | OK (field names match) | `D:brokers/angel/mod.rs:200-248,952-977` | - |
| Returned symbol/exchange | OA symbol and OA exchange (web keeps the caller's values via token_map, `W:api/data.py:408-414`) | **Bug**: returns Angel `tradingSymbol` (`RELIANCE-EQ`) and Angel exchange (`NSE` for an NSE_INDEX request) | `D:brokers/angel/mod.rs:963-964` | Build a `exchange:symbolToken -> (oa_symbol, oa_exchange)` map before the call and use it when building `Quote`. |
| Batching | 50 tokens per request (`W:api/data.py:316-343`) | **Missing**: no chunking; >50 symbols will be rejected | `D:brokers/angel/mod.rs:895-981` | Chunk into 50. |
| timestamp | n/a (web omits) | uses `Utc::now()` | `D:brokers/angel/mod.rs:976` | Acceptable; Angel returns `exchFeedTime`/`exchTradeTime` if wanted. |

### 1.14 depth

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| get_depth | same quote endpoint, FULL mode, pads to exactly 5 bids/asks, returns `ltq (lastTradeQty), totalbuyqty (totBuyQuan), totalsellqty (totSellQuan), oi, open/high/low/ltp/prev_close/volume` (`W:api/data.py:828-910`) | **Bug**: inserts the *symbol* as the token (`vec![symbol.to_string()]`); also makes a redundant first `get_quote` call that itself is broken; returns only `bids/asks` (no ltp/ltq/totals, `MarketDepth` struct `D:brokers/types.rs:141-146`). Does not pad to 5 levels. | `D:brokers/angel/mod.rs:984-1092,1013` | Resolve token; drop redundant call; extend `MarketDepth` with `ltp, ltq, open, high, low, prev_close, volume, oi, totalbuyqty, totalsellqty`; pad to 5. |

### 1.15 history (candles + OI)

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Endpoint | `POST /rest/secure/angelbroking/historical/v1/getCandleData` body `{exchange, symboltoken, interval, fromdate "YYYY-MM-DD HH:MM", todate}` (`W:api/data.py:541-547,572`) | **Missing**: no `get_history` in `Broker` trait; `HistoryService::get_history` only reads DuckDB and has `// TODO: Fetch from broker API` (`D:services/history_service.rs:88-97`) | `D:services/history_service.rs:46-98` | Implement. |
| Interval map | `1m ONE_MINUTE, 3m THREE_MINUTE, 5m FIVE_MINUTE, 10m TEN_MINUTE, 15m FIFTEEN_MINUTE, 30m THIRTY_MINUTE, 1h ONE_HOUR, D ONE_DAY` (`W:api/data.py:222-233`) | Missing. Also `HistoryService::get_intervals` advertises `2h, 4h, 1d, 1w, 1M` which Angel does not support, and uses `1d` instead of the OpenAlgo `D` (`D:services/history_service.rs:102-118`) | `D:services/history_service.rs:102-118` | Per-broker interval list; use `D`. |
| Chunking | days per request: 1m 30, 3m 60, 5m 100, 10m 100, 15m 200, 30m 200, 1h 400, D 2000; `from` 00:00, `to` 23:59 or now; chunk retry x4 on rate-limit (`W:api/data.py:508-520,536-620`) | Missing | - | Implement. |
| Timestamp | `pd.to_datetime(ts)`, **daily only: +5h30m** (UTC->IST shift), then epoch seconds; sort + dedupe (`W:api/data.py:633-648`) | Missing | - | Implement. |
| OI | for NFO/BFO/CDS/MCX always call `POST /rest/secure/angelbroking/historical/v1/getOIData` same payload, rename `time`->`timestamp`, left-merge, fill 0 (`W:api/data.py:651-668,686-820`) | Missing | - | Implement. |
| Index exchange | NSE_INDEX->NSE etc. (`W:api/data.py:489-494`) | Missing | - | - |

### 1.16 master contract

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| URL | `https://margincalculator.angelbroking.com/OpenAPI_File/files/OpenAPIScripMaster.json` (`W:database/master_contract_db.py:394`) | OK | `D:brokers/angel/mod.rs:13,1097` | - |
| Exchange filtering | **none** in web: every `exch_seg` row is inserted (NSE, BSE, NFO, BFO, MCX, CDS, NCDEX...). `plugin.json` supported_exchanges: NSE, BSE, NFO, BFO, CDS, MCX, NSE_INDEX, BSE_INDEX, MCX_INDEX (no BCD). | OK (parity, no filtering). No exchanges "missing" relative to web. | `D:brokers/angel/mod.rs:1094-1105` | Optional: drop NCDEX rows not in plugin list. |
| brsymbol / brexchange | `brsymbol = symbol` (raw), `brexchange = exch_seg` (`W:database/master_contract_db.py:159-160`) | OK | `D:brokers/angel/mod.rs:1113-1117,1235-1236` | - |
| Index exchange | `AMXIDX` + NSE/BSE/MCX -> `NSE_INDEX/BSE_INDEX/MCX_INDEX` (`W:database/master_contract_db.py:163-165`) | OK | `D:brokers/angel/mod.rs:1140-1147` | - |
| Index symbol | NSE_INDEX: `symbol = name.upper().replace(" ","").replace("-","")`; BSE_INDEX: same plus strip `"S&P "`; then overrides `NIFTY50->NIFTY, NIFTYBANK->BANKNIFTY, NIFTYFINSERVICE->FINNIFTY, NIFTYNEXT50->NIFTYNXT50, NIFTYMIDSELECT/NIFTYMIDCAPSELECT->MIDCPNIFTY, SNSX50->SENSEX50` (`W:database/master_contract_db.py:285-315`) | **Bug**: desktop keeps Angel's `symbol` field and only special-cases 7 literal strings in `normalize_index_name` (`"Nifty 50"`, `"Nifty Bank"`, ...). Every other index (e.g. `Nifty IT`, `Nifty Auto`, `S&P BSE SENSEX`, `NIFTY MIDCAP 100`) keeps spaces/case and does not match the documented `NIFTYIT`, `NIFTYAUTO`, `SENSEX` names in `symbol-format.md`. | `D:brokers/angel/mod.rs:1203,1266-1277` | Derive from `name` with the same upper/strip rules for NSE_INDEX and BSE_INDEX, then apply the override table. |
| -EQ/-BE/-MF/-SG stripping | regex replace anywhere (`W:...:168`) | OK | `D:brokers/angel/mod.rs:1150-1154` | - |
| Expiry | `strptime("%d%b%Y").strftime("%d-%b-%y").upper()`; unparsable -> original (`W:...:119-125,171-172`) | Partial: positional slice `[0..2],[2..5],[7..9]` on any string >= 9 chars; shorter strings uppercased; empty string for equities becomes `Some("")` instead of `None`/NULL | `D:brokers/angel/mod.rs:1136,1243-1255` | Use `chrono::NaiveDate::parse_from_str(s, "%d%b%Y")`; map empty -> `None`. |
| Strike | `/100`; CDS `OPTCUR`/`OPTIRC`: the already-/100 value is divided again by `100000` (net `/1e7`) (`W:...:175-181`) | Divergence: desktop uses `raw/100/1000` = `/1e5` | `D:brokers/angel/mod.rs:1119-1127` | Verify against live CDS rows; align with whichever yields e.g. `USDINR...83.25CE`. |
| Option symbol strike text | `str(strike).replace(r"\.0","")` (regex, also strips an interior `.0`, e.g. `100.05`->`10005`) (`W:...:200`) | Desktop `format_strike` strips only a true `.0` fraction. Differs on strikes like `x.05`, but desktop is the saner one. | `D:brokers/angel/mod.rs:1258-1264` | Keep desktop; be aware of cross-app symbol mismatch for such strikes. |
| Symbol construction | CDS `FUTCUR/FUTIRC`, MCX `FUTCOM`, BFO `FUTIDX/FUTSTK` -> `name+DDMMMYY+FUT`; CDS `OPTCUR/OPTIRC`, MCX `OPTFUT`, BFO `OPTIDX/OPTSTK` -> `name+DDMMMYY+strike+CE/PE` (last 2 chars of Angel symbol); NFO untouched (`W:...:186-280`) | OK | `D:brokers/angel/mod.rs:1157-1200` | - |
| Instrument type | `OPTIDX/OPTSTK/OPTFUT/OPTCUR/OPTIRC` -> CE/PE by symbol suffix; `FUTIDX/FUTSTK/FUTCOM/FUTCUR/FUTIRC/FUTIRT` -> FUT; equities keep Angel's `""` (`W:...:317-373`) | OK (parity). Note `symbol-format.md` says instrumenttype is EQ/FUT/CE/PE; neither app emits `EQ` for Angel equities; desktop stores `""` (DB default `'EQ'` only applies to NULL). | `D:brokers/angel/mod.rs:1216-1225` | Optional: map `""` -> `EQ`. |
| lotsize / tick_size | `int(lotsize)`, `tick_size/100` (`W:...:183-184`) | OK | `D:brokers/angel/mod.rs:1130,1232-1233` | - |
| Persisted columns | `symbol, brsymbol, name, exchange, brexchange, token, expiry, strike, lotsize, instrumenttype, tick_size` (`W:...:29-45`) | **Gap** (X5): `expiry`, `strike`, `option_type` computed in `SymbolData` but dropped at `SymbolInfo` conversion and never written | `D:services/symbol_service.rs:217-230`; `D:db/sqlite/symbol.rs:29-48`; `D:state.rs:35-47` | Add the three fields to `SymbolInfo` and the INSERT/SELECTs. |
| Dedup | `copy_from_dataframe` skips tokens already present (token-unique) (`W:...:64-68`) | `INSERT OR REPLACE` on `UNIQUE(exchange, symbol)` -> duplicates on symbol silently overwrite (e.g. BSE rows sharing a symbol) | `D:db/sqlite/symbol.rs:28-30`; `D:db/sqlite/migrations.rs:128` | Consider `UNIQUE(exchange, token)`. |

### 1.17 WebSocket streaming

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| URL | `wss://smartapisocket.angelone.in/smart-stream` (no query params) (`W:streaming/smartWebSocketV2.py:21`) | OK-ish: adds `?clientCode=&feedToken=&apiKey=` query params (SmartAPI accepts both forms) | `D:websocket/manager.rs:166-169` | Acceptable. |
| Headers | `Authorization: <jwt auth_token>` (raw, no Bearer), `x-api-key`, `x-client-code`, `x-feed-token` (`W:streaming/smartWebSocketV2.py:383-388`) | **Bug**: `Authorization: Bearer {feed_token}` -> sends the *feed* token (not the JWT) and adds a `Bearer ` prefix. `commands/websocket.rs` reads the JWT (`_auth_token`) and discards it. Connection may still succeed via the query params, but the header is wrong. | `D:websocket/manager.rs:185`; `D:commands/websocket.rs:50` | Pass `auth_token` into `connect` and set `Authorization: {auth_token}`. |
| Subscribe JSON | `{"correlationID", "action": 1, "params": {"mode", "tokenList": [{"exchangeType", "tokens": [...]}]}}`; one message per mode (`W:streaming/smartWebSocketV2.py:230-234`, `W:streaming/angel_adapter.py:555-582`) | Mostly OK. Divergences: (a) a single message uses `max(mode)` across all requests, so mixed-mode batches are all promoted; (b) `exchangeType` derived from the **OpenAlgo** exchange sent by the UI, so `BSE_INDEX` -> `1` (web: 3 via brexchange `BSE`) and `MCX_INDEX` -> `1` (web: 5 via brexchange `MCX`). | `D:websocket/manager.rs:888-933` | Group by mode and send one message per mode; resolve `brexchange` from the symbol cache (`SymbolInfo.brexchange`) instead of the OA exchange. |
| exchangeType codes | NSE 1, NFO 2, BSE 3, BFO 4, MCX 5, NCX 7, CDS 13, NSE_INDEX 1, BSE_INDEX 3 (`W:streaming/angel_mapping.py:8-18`) | OK for the six real exchanges; wrong for BSE_INDEX/MCX_INDEX (above) | `D:websocket/manager.rs:892-901` | as above |
| Unsubscribe | `action: 0`, same `mode` as the subscription (`W:streaming/smartWebSocketV2.py:277-283`, `W:streaming/angel_adapter.py:514`) | **Bug**: always `"mode": 1` -> unsubscribe is ignored for Quote/SnapQuote subscriptions | `D:websocket/manager.rs:964-970` | Track mode per subscription and send it. |
| Heartbeat | ping every 10 s with payload `ping`; text `pong` handled (`W:streaming/smartWebSocketV2.py:22-23,153-156`) | OK: text `"ping"` every 30 s (SmartAPI docs allow 30 s); inbound text (incl. `pong` and JSON error frames) only debug-logged | `D:websocket/manager.rs:228,298-304,246-248` | Optionally surface JSON error frames (bad token / quota) to the UI. |
| Binary layout (common) | LE; `[0]` mode u8, `[1]` exchange_type u8, `[2:27]` token NUL-terminated, `[27:35]` seq i64, `[35:43]` exch_ts i64, `[43:51]` LTP i64 (/100) (`W:streaming/smartWebSocketV2.py:560-567`) | OK | `D:websocket/manager.rs:443-486` | - |
| Quote fields (mode 2/3) | `[51]` ltq i64, `[59]` avg i64, `[67]` volume i64, `[75]` tot_buy f64, `[83]` tot_sell f64, `[91]` open, `[99]` high, `[107]` low, `[115]` close (all /100 for prices) (`W:...:574-603`) | OK offsets/types. Minor: `tick.bid_qty = total_buy_qty`, `tick.ask_qty = total_sell_qty` mislabels aggregate totals as best-level quantities. | `D:websocket/manager.rs:496-520` | Add `total_buy_qty/total_sell_qty` to `MarketTick`. |
| SnapQuote (mode 3) | `[123]` ltt, `[131]` OI, `[139]` OI change %, `[147:347]` 10 x 20-byte best-5 packets (`flag u16, qty i64, price i64, orders u16`; flag 0/1 decides buy/sell, lists then swapped so flag==1 -> buy, flag==0 -> sell), `[347]` upper circuit, `[355]` lower circuit, `[363]` 52w high, `[371]` 52w low (`W:...:606-636,685-713`) | Partial: reads OI at 131 and first packet at 147 as *ask* and packet at 247 as *bid* **by position**, ignoring the flag; only level 1 kept; circuits / 52-week ignored; `orders` ignored. | `D:websocket/manager.rs:523-544` | Parse all 10 packets, route by flag (0 -> sell/ask, 1 -> buy/bid), expose 5 levels + circuit limits. |
| Depth mode 4 | Angel mode 4 = 20-level depth, NSE only, <=50 tokens, different packet layout starting at byte 43 (`W:...:239-245,255-260,638-652`); web only allows modes 1-3 (`W:streaming/angel_adapter.py:353-361`) | **Bug**: UI `"full"/"depth"` maps to `SubscriptionMode::Full = 4` and is sent to Angel; incoming mode-4 packets fall through the mode>=2 branch and are mis-parsed as quote fields | `D:commands/websocket.rs:98-99`; `D:websocket/manager.rs:20-27,495` | Clamp Angel to mode 3 (or implement the mode-4 parser and NSE/50-token guard). |
| Tick symbol/exchange resolution | `(token, exchange_type)` -> subscription's OA `symbol`/`exchange` (so NSE_INDEX stays NSE_INDEX) (`W:streaming/angel_adapter.py:686-717`) | Partial: `token_map` keyed by token only (`register_symbol(token, symbol, exchange)`), collision-prone across segments; emitted `exchange` is derived from `exchange_type` (`NSE` for an index) instead of the registered OA exchange | `D:websocket/manager.rs:411-415,468-493` | Key by `(exchange_type, token)`; emit the registered OA exchange. |
| Price scaling | all prices /100 (`W:streaming/angel_adapter.py:767-790`) | OK | `D:websocket/manager.rs:482,513-516,535,541` | - |

---

## 2. Symbol token lookup and brexchange usage (desktop)

- Cache keys: `symbol_cache["{exchange}:{token}"]`, `symbol_reverse_cache["{exchange}:{symbol}"]` (`D:state.rs:193-205`). Lookup by OA name works for order placement (`D:services/order_service.rs:67-77`), which sets `broker_symbol = brsymbol` and `symbol_token = token`. Good.
- `brexchange` is stored (`D:db/sqlite/symbol.rs`, migration 031) but **never read** by any service or the websocket layer; the Angel adapter and websocket manager use the OA exchange string directly. Works for NSE/BSE/NFO/BFO/MCX/CDS because Angel's `exch_seg` equals the OA code, but breaks for the three `*_INDEX` exchanges in websocket subscribe (see 1.17). Quotes/depth do their own `*_INDEX` -> base mapping inline.
- No token -> OA symbol reverse path is used by the book/position/holding mappers (X4), although `state.get_symbol_by_token(exchange, token)` exists (`D:state.rs:158-161`) and would be the right tool once `symboltoken` is deserialised from the Angel responses.

## 3. Contract docs check (`symbol-format.md`, `order-constants.md`)

- Expiry `DD-MMM-YY` uppercase: desktop produces this (`19-MAR-24`) but does not persist it (X5).
- Index exchanges `NSE_INDEX/BSE_INDEX/MCX_INDEX`: produced correctly for `AMXIDX`, but index *symbols* are not normalised per the documented list (1.16).
- `instrumenttype` EQ/FUT/CE/PE: FUT/CE/PE correct; equities are `""` in both apps.
- Product/pricetype constants: desktop maps identically to web for place_order; modify_order does not map (1.3).

---

## 4. Prioritised fix list

1. **P0** Pass the real API key in `X-PrivateKey` on all secure calls (`D:brokers/angel/mod.rs:502,573,628,655,714,767,816,864,937,1032`).
2. **P0** Quotes/depth: fix `(exchange, symbol)` destructuring (`:904`) and send the symbol **token**, not the name (`:917,1013`); return OA symbol/exchange; chunk by 50.
3. **P0** Reverse-map broker symbols to OA symbols (deserialise `symboltoken`, use `get_symbol_by_token`) in orderbook/tradebook/positions/holdings; this unblocks close_position, close_all_positions and smart orders.
4. **P0** `modify_order`: send `tradingsymbol, symboltoken, exchange, producttype`, mapped `variety`/`ordertype`, `duration: DAY` (`:541-565`).
5. **P0** `cancel_all_orders`: lowercase status comparison (`D:services/order_service.rs:226`).
6. **P0** Implement history (`getCandleData` + `getOIData`, interval map, chunk limits, daily +5h30) and margin (`/margin/v1/batch`).
7. **P1** Funds: `available_cash = raw + utiliseddebits - collateral`; add m2m realised/unrealised from positions.
8. **P1** Tradebook: parse `fillprice/filltime/fillsize/tradevalue`.
9. **P1** Master contract: derive index symbols from `name` (upper, strip spaces/hyphens/`S&P `) + override table; persist `expiry/strike/option_type` in `SymbolInfo`/DB; parse expiry with chrono; map empty expiry to NULL.
10. **P1** WebSocket: `Authorization` = raw JWT; `exchangeType` from `brexchange`; one subscribe per mode; unsubscribe with the real mode; route best-5 packets by flag; block/implement mode 4; key token map by `(exchange_type, token)`.
11. **P2** `triggerprice` must be `"0"` not `null`; send `stoploss = trigger_price` to match web; uppercase `transactiontype`; remove misleading `rename_all = "camelCase"`; add rate-limit back-off.

---

## A. Zerodha (Kite Connect) adapter audit: Desktop (Rust/Tauri) vs Web (Python)

Scope: read-only comparison of
`openalgo-desktop/src-tauri/src/brokers/zerodha/mod.rs` (+ `websocket/manager.rs`, `services/*`, `state.rs`, `db/sqlite/symbol.rs`, `commands/websocket.rs`, frontend `BrokerSelect.tsx`, `useMarketData.ts`) against
`openalgo/broker/zerodha/{api,mapping,database,streaming}` and `openalgo/blueprints/brlogin.py`.

Paths below are absolute under `/Users/openalgo/openalgo-desktop/`. "D" = desktop file `openalgo-desktop/src-tauri/src/brokers/zerodha/mod.rs` unless another path is given; "W" = web file under `openalgo/broker/zerodha/`.

Legend: **OK** = behaviourally equivalent; **Bug** = present but wrong; **Missing** = web feature absent in desktop; **Partial** = present but reduced.

---

## 1. Severity-ranked headline findings

| # | Severity | Finding | Desktop location |
|---|----------|---------|------------------|
| 1 | Critical | Master-contract CSV parsed by **wrong column indices**: `expiry=fields[4]` is actually `last_price`, `lot_size=fields[5]` is `expiry`, `tick_size=fields[6]` is `strike`, `strike=fields[7]` is `tick_size`. Every FUT/CE/PE symbol is garbage (e.g. `NIFTY22450.5FUT`, `NIFTY22450.50.05CE`), lot sizes all fall back to 1, tick sizes become strike prices. | D:906-918 |
| 2 | Critical | WebSocket uses `public_token` as `access_token`. `authenticate` stores `feed_token = public_token` (D:342); `commands/websocket.rs:52-64` passes feed_token to `manager.rs:170-173` as `access_token=` -> Kite returns 403 on every connect. Web splits `api_key:access_token` and uses the access_token half (W `streaming/zerodha_adapter.py:97-104`). | D:342, `websocket/manager.rs:170` |
| 3 | Critical | `get_quote` / `get_market_depth` destructure the tuple as `(symbol, exchange)` but every caller passes `(exchange, symbol)` (`commands/quotes.rs:32`, `services/quotes_service.rs:60`). Query becomes `i=RELIANCE:NSE`. | D:749, D:780 |
| 4 | Critical | Quotes/depth send the **OpenAlgo** symbol, not Kite's `tradingsymbol` (`brsymbol`). Web resolves `get_br_symbol` first (`api/data.py:211,341,600`). Indices (`NIFTY` must be `NIFTY 50`), all F&O and MCX symbols fail. | D:755, D:830 |
| 5 | Critical | WS subscribe sends `subscribe` and `mode` JSON **in one text frame joined by `\n`** -> not valid JSON, Kite drops it. Web sends two frames (W `streaming/zerodha_websocket.py:440-446`). | `websocket/manager.rs:1001` |
| 6 | Critical | WS token must be the integer `instrument_token`, but desktop stores token as `"instrument_token::::exchange_token"` (D:940) and nothing outside mod.rs ever splits on `::::`; `manager.rs:979` `parse::<u32>()` fails -> empty token list. Web splits `[0]` (W `zerodha_adapter.py:291-292`). | D:940, `manager.rs:976-980`, `src/hooks/useMarketData.ts:150` |
| 7 | High | WS tick exchange derived from token with wrong formula and wrong table (`(token/256)%256`; Kite segment is the **low byte** `token & 0xFF`: 1 NSE, 2 NFO, 3 CDS, 4 BSE, 5 BFO, 6 BCD, 7 MCX, 8 MCXSX, 9 INDICES). The correct exchange from `token_map` is discarded (`let (symbol, _) = ...`). All index ticks tagged `NSE`, so frontend key `NSE_INDEX:NIFTY` never updates. | `manager.rs:602-620` |
| 8 | High | Master contract exchange mapping incomplete: Kite `GLOBAL` and `NSEIX` rows not folded into `GLOBAL_INDEX` (web `database/master_contract_db.py:180-181`); index symbol rename table has 7 entries vs ~60 NSE + 35 BSE + `GIFT NIFTY` in web (W:296-409); BSE renames applied to **all** rows, not only `BSE_INDEX` (web restricts, W:364-370, to avoid clobbering equities `AUTO`, `METAL`, ...). MCX `lot_size=1` not replaced by real contract size (W:231-266 + `mapping/mcx_contract_size.py`). Blank `name` not back-filled from tradingsymbol (W:274-275). | D:921-929, D:1033-1044 |
| 9 | High | Symbol store drops `expiry`, `strike`, `option_type` (`state.rs:35-47`, `db/sqlite/symbol.rs:29-31`, `services/symbol_service.rs:219-229`) even though `SymbolData` carries them; `/expiry`, option chain, `SymbolSearchResult.strike/expiry` are always `None`. | `state.rs:35`, `symbol.rs:29` |
| 10 | High | Historical data: **entirely missing**. `Broker` trait has no history method; `services/history_service.rs:90,137` are TODOs. Web: `GET /instruments/historical/{instrument_token}/{interval}?from=..&to=..&oi=1`, interval map, 2000/60-day chunking, +5:30 for daily (W `api/data.py:168-181, 509-567`). | `history_service.rs:46-141` |
| 11 | High | Order/trade/position/holding symbols returned as Kite `tradingsymbol`, never converted to OpenAlgo symbol (web `map_order_data`/`map_position_data` call `get_oa_symbol`, `mapping/order_data.py:98,288`). Order statuses not lowercased (`COMPLETE` vs `complete`, W:164-173) and leak raw to `/api/v1/orderbook` (`webhook/handlers.rs:644`). | D:533,576,622,672; D:544 |
| 12 | High | Holdings: Kite leaves numerics `null` for unpriced scrips; `#[serde(default)]` on `f64`/`i32` does **not** accept explicit `null`, so one such row fails the whole `/portfolio/holdings` call (web coerces, W `order_data.py:48-60, 321-345`). | D:150-169 |
| 13 | Medium | Funds: `available_cash = equity.net + commodity.net` includes collateral; web `availablecash = net + debits - collateral` (W `api/funds.py:76-79`). `opening_balance` set from `available.intraday_payin` (Kite has `available.opening_balance`). No m2m realised/unrealised (web derives from positions + `/quote/ltp`, W:81-134). | D:720-738 |
| 14 | Medium | `place_order` omits `disclosed_quantity`, `tag=openalgo`, `market_protection=-1`; sends `price` only when `>0` (web always sends `price` and `trigger_price`, W `mapping/transform_data.py:28-49`, `api/order_api.py:223-236`); no MCX units->contracts conversion (`to_kite_quantity`). | D:362-378 |
| 15 | Medium | Multiquotes: no 500-instrument batching or 1 s inter-batch sleep (W `api/data.py:287-316`). `get_multi_quotes` just forwards everything in one URL. | D:741-816, `quotes_service.rs:95-102` |
| 16 | Medium | Margin calculator (`POST /margins/basket?consider_positions=true` / `/margins/orders`, W `api/margin_api.py:69-78`) and GTT (`/gtt/triggers`, W `api/gtt_api.py`) absent. | — |
| 17 | Medium | WS order postbacks (`{"type":"order"}` text frames) ignored; web `streaming/zerodha_order_adapter.py` normalises them. Desktop only `debug!`s text frames. | `manager.rs:247-250` |
| 18 | Low | WS client **sends** a 1-byte binary frame every 30 s (`manager.rs:305-309`). Kite's 1-byte heartbeat is server->client only; web never sends it (relies on WS ping). | `manager.rs:305` |
| 19 | Low | `/session/token` call lacks `X-Kite-Version: 3` (web sends it, W `api/auth_api.py:28`). | D:303-308 |
| 20 | Low | Naive `line.split(',')` CSV parsing breaks on quoted `name` fields containing commas (web uses `pd.read_csv`). | D:905 |

---

## 2. Function-by-function comparison

### 2.1 Authentication and session

| Function | Web behaviour (file:line) | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Login URL | Not built server-side; user opens Kite login. `REDIRECT_URL='http://127.0.0.1:5000/<broker>/callback'` (`openalgo/.sample.env:18`); callback route `brlogin.broker_callback`, reads `request_token` or `code` (`blueprints/brlogin.py:1028-1031`). | OK | `src/pages/BrokerSelect.tsx:426-428` builds `https://kite.zerodha.com/connect/login?v=3&api_key=..&redirect_uri=..&state=..`; callback `GET /{broker}/callback` (`webhook/server.rs:113`, `handlers.rs:2088-2091` accepts `request_token`). Redirect default `http://127.0.0.1:5000/zerodha/callback` (`BrokerSelect.tsx:404-411`). | Note: Kite ignores `redirect_uri`/`state` query params (uses the app's registered redirect; only `redirect_params=` is echoed). Harmless but misleading; `state` is never validated server-side (`handlers.rs:2080-2117`). |
| `authenticate` | `POST https://api.kite.trade/session/token`, form `api_key, request_token, checksum=sha256(api_key+request_token+api_secret)`, header `X-Kite-Version: 3` (`api/auth_api.py:15-36`). Returns `access_token` only; `brlogin.py:1040-1041` stores `auth_token = f"{BROKER_API_KEY}:{access_token}"`. | OK (checksum, body, combined token) / **Bug** (feed token) / Low (header) | D:286-346 | (a) `feed_token: Some(data.access_token)` instead of `public_token` (D:342) — WS needs access_token. (b) add `.header("X-Kite-Version","3")` at D:305. |
| `get_headers` | `{"X-Kite-Version":"3","Authorization":f"token {api_key:access_token}"}` (`api/order_api.py:52`, `api/data.py:95-99`). | OK | D:32-40 | — |
| Token storage | DB `auth_token` = `api_key:access_token`; WS adapters split on `:` (`streaming/zerodha_adapter.py:97-104`, `zerodha_order_adapter.py:137-138`). | OK | D:338 | — |

### 2.2 Orders

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| `place_order` | `POST /orders/regular` (always `regular`, `api/order_api.py:254-256`), `Content-Type: application/x-www-form-urlencoded`. Payload (`order_api.py:223-236`, `transform_data.py:21-54`): `tradingsymbol`(=brsymbol), `exchange`, `transaction_type`, `order_type` (MARKET/LIMIT/SL/SL-M identity), `quantity` (MCX units->contracts via `to_kite_quantity`), `product` (CNC/NRML/MIS identity, no per-exchange rules), `price` (always, default "0"), `trigger_price` (always, default "0"), `disclosed_quantity`, `validity="DAY"` (forced), `market_protection="-1"`, `tag="openalgo"`. Success: `data.order_id`. | Partial / Bug | D:348-416 | Always send `price` and `trigger_price` (Kite accepts "0"); add `disclosed_quantity` (field exists on `OrderRequest`, `brokers/types.rs:17`), `tag=openalgo`, `market_protection=-1`; add MCX contract-size conversion once lot sizes are real. `amo` variety support (D:360) is a superset of web — fine. |
| `modify_order` | `PUT /orders/regular/{orderid}` form: `order_type`, `quantity`, `price` ("0" if falsy), `disclosed_quantity`, `validity="DAY"`, `trigger_price` only if set (`order_api.py:500-534`). | OK-ish (Partial) | D:418-476 | Send `disclosed_quantity` and default `validity=DAY`; variety hard-coded `regular` same as web. |
| `cancel_order` | `DELETE /orders/regular/{orderid}` (`order_api.py:468-470`); returns `data.order_id`. | OK | D:478-508 | Desktop additionally allows `variety` param — superset. |
| `cancel_all_orders` | Orderbook filter `status in ["OPEN","TRIGGER PENDING"]`, cancel each (`order_api.py:559-576`). | OK | `services/order_service.rs:226` (`PENDING`/`OPEN`/`TRIGGER PENDING`, raw Kite casing) | Works only because status is left raw (see orderbook mapping bug): if status mapping is fixed, update this filter. |
| `get_open_position` | brsymbol lookup, match `net[]` by `tradingsymbol+exchange+product`, MCX contracts->units, 1 s position cache + per-symbol lock (`order_api.py:153-191`). | Partial | `services/position_service.rs:66-84` | Matches on desktop `Position.symbol` which is the Kite tradingsymbol (D:622), while callers pass OpenAlgo symbols -> F&O/MCX never match. Convert positions to OA symbols (see 2.3) or compare against `brsymbol`. No cache/lock. |
| `close_all_positions` | Iterate `net[]`, skip qty 0, `SELL` if >0 else `BUY`, `get_oa_symbol`, `reverse_map_product_type` (identity), MARKET order; collects failures (`order_api.py:372-444`). | Partial | `position_service.rs:167-200` -> `close_position` -> `OrderService::place_order` | Works only accidentally: symbol lookup misses (`order_service.rs:67-77` warns) and D:350-356 falls back to raw symbol which happens to equal Kite's tradingsymbol. Fix symbol normalisation. |
| `place_smartorder` | `order_api.py:279-369` | Partial (not zerodha-specific) | `services/smart_order_service.rs` | Depends on `get_open_position` fix. |

### 2.3 Books

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Orderbook | `GET /orders` (`order_api.py:90-91`); `map_order_data` converts `tradingsymbol`->OA symbol via `get_oa_symbol`, MCX qty fields contracts->units (`order_data.py:63-104`); `transform_order_data` maps status `COMPLETE->complete, REJECTED->rejected, TRIGGER PENDING->trigger pending, OPEN->open, CANCELLED->cancelled` and emits `symbol, exchange, action, quantity, price, trigger_price, pricetype, product, orderid, order_status, timestamp` (`order_data.py:148-191`). | Bug | D:510-551 (`symbol: o.tradingsymbol` D:533; `status: o.status` D:544) | Map `tradingsymbol`->OA symbol (`brsymbol` reverse lookup; adapter needs access to symbol cache or do it in `orderbook_service.rs`). Lower-case status per web table (also fix `order_service.rs:226` filter accordingly). Other in-flight Kite statuses (`PUT ORDER REQ RECEIVED`, `VALIDATION PENDING`, ...) -> `open` per `zerodha_order_adapter.py:41-55`. |
| Tradebook | `GET /trades`; emits `symbol, exchange, product, action, quantity, average_price, trade_value, orderid, tradeid (=trade_id), timestamp = fill_timestamp or order_timestamp` (`order_data.py:210-253`). | Partial | D:553-594 | Reuses `KiteOrderData`; drops `trade_id`, uses `order_timestamp` instead of `fill_timestamp`; symbol not OA-mapped. Add a `KiteTradeData` struct with `trade_id`, `fill_timestamp`. |
| Positions | `GET /portfolio/positions`, use `data.net` (`order_data.py:256-294`), MCX contracts->units, OA symbol; `transform_positions_data` emits `symbol, exchange, product, quantity, pnl (2dp), average_price (str 2dp), ltp` (W:297-314). | Partial | D:596-638 | Net positions correct; symbol not OA-mapped (D:622); no MCX units conversion. |
| Holdings | `GET /portfolio/holdings`; product forced `CNC` (W:363-380); `_to_float` null-tolerant; `pnlpercent=0` when avg or last price is 0 (W:317-348); statistics `totalholdingvalue, totalinvvalue, totalprofitandloss, totalpnlpercentage` (W:383-405). | Bug | D:148-169, D:640-686 | Change numeric fields to `Option<f64>/Option<i32>` or use `#[serde(deserialize_with=...)]` to accept `null` (`#[serde(default)]` alone rejects `null`). Guard `pnl_percentage` when `ltp==0`. `t1_quantity` is included (OK). |

### 2.4 Funds / margin

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| `get_funds` | `GET /user/margins`. `utiliseddebits = equity.utilised.debits + commodity.utilised.debits`; `collateral = sum(available.collateral)`; `availablecash = (equity.net + commodity.net) + debits - collateral` (derives cash; W `api/funds.py:52-79`); `m2mrealized` = sum over closed net positions `(sell_value - buy_value)`; `m2munrealized` = `(live_ltp - avg) * qty * multiplier` via `GET /quote/ltp` (W:81-134). Commodity segment **is** included. | Bug (medium) | D:688-739 | `available_cash` should subtract collateral (currently `equity.net+commodity.net`, D:720). `opening_balance` should read `available.opening_balance`, not `intraday_payin` (D:725,732). `Funds` struct lacks m2m fields; `span`/`exposure`/`payin`/`payout` extras are fine. |
| Margin calculator | `POST /margins/basket?consider_positions=true` if >1 leg else `/margins/orders`; JSON array of `{exchange, tradingsymbol, transaction_type, variety:"regular", product, order_type, quantity, price, trigger_price}` (`api/margin_api.py:69-87`, `mapping/margin_data.py:11-64`); response parsed into `initial/final/orders` (`margin_data.py:118+`). | Missing | — | Add `calculate_margin` to `Broker` trait. |
| GTT | `/gtt/triggers` place/modify/cancel/book (`api/gtt_api.py:152-283`). | Missing | — | Out of scope for parity v1; note. |

### 2.5 Market data (REST)

| Function | Web behaviour | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Quote exchange prefix | `NSE_INDEX->NSE, BSE_INDEX->BSE, MCX_INDEX->MCX, GLOBAL_INDEX->row.brexchange (GLOBAL or NSEIX)`; others pass through (`api/data.py:22-39`). | Partial | D:750-754, D:824-828 | Add `MCX_INDEX->MCX` and `GLOBAL_INDEX->brexchange`. |
| `get_quotes` | brsymbol via `get_br_symbol`; `GET /quote?i=urlencoded(EXCH:brsymbol)`; result keyed `EXCH:brsymbol`; returns `ask, bid, ask_qty, bid_qty, high, low, ltp, open, prev_close, volume, oi` from `depth.buy[0]/sell[0]`, `ohlc`, `last_price` (`api/data.py:200-274`). | Bug | D:741-816 | (1) Tuple order: callers pass `(exchange, symbol)`; adapter destructures `(symbol, exchange)` (D:749, D:780). (2) Use `brsymbol` (needs symbol cache; e.g. have `QuotesService` resolve `brsymbol` and pass it, like `OrderService` does at `order_service.rs:67-74`). (3) URL-encode `EXCH:SYM` (`NIFTY 50` has a space; reqwest will encode but key lookup must use the raw string — it does). Field mapping itself matches web. |
| `get_multiquotes` | Batches of 500 with 1 s sleep between batches; per-symbol error entries for unresolved/no-data (`api/data.py:276-454`). | Missing (batching) | `quotes_service.rs:95-102` -> D:741 | Chunk `symbols` by 500, `tokio::time::sleep(1s)` between chunks; emit error entries instead of silently dropping (D:778-815 `filter_map` drops missing). |
| `get_market_depth` | Same `/quote` call; 5 levels each side **padded with `{price:0,quantity:0}`**; plus `ltp, ltq (last_quantity), oi, open, high, low, prev_close, totalbuyqty, totalsellqty, volume` (`api/data.py:589-676`). | Partial / Bug | D:818-888 | Same symbol/tuple issues; `MarketDepth` (`brokers/types.rs:141-146`) lacks ltp/ltq/oi/ohlc/totals so `/api/v1/depth` cannot match web contract; pad to 5 levels. |
| `get_history` | `instrument_token = token.split("::::")[0]`; interval map `1m->minute, 3m->3minute, 5m->5minute, 10m->10minute, 15m->15minute, 30m->30minute, 60m/1h->60minute, D->day` (`api/data.py:168-181`); `GET /instruments/historical/{token}/{interval}?from=YYYY-MM-DD+00:00:00&to=YYYY-MM-DD+23:59:59&oi=1`; chunk 2000 days (`day`) / 60 days (intraday) (W:509-547); candles `[timestamp,o,h,l,c,volume,oi]`; ISO8601->epoch s; **+5h30m for `D`** (W:559-567); sort+dedupe. | Missing | `services/history_service.rs:46-141` (TODO) ; `Broker` trait has no history fn (`brokers/mod.rs:16-83`) | Add `get_history` to trait and implement per web. `get_intervals()` advertises `2h,4h,1d,1w,1M` (`history_service.rs:105-118`) which Kite does not support and omits `D`. |

### 2.6 Master contract

Kite CSV header (what `pd.read_csv` keys on in `master_contract_db.py:117,204-206`):
`instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange`
i.e. indices 0..11 = token, exch_token, tradingsymbol, name, **last_price(4), expiry(5), strike(6), tick_size(7), lot_size(8)**, instrument_type(9), segment(10), exchange(11).

| Item | Web behaviour (`database/master_contract_db.py`) | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| Download | `GET https://api.kite.trade/instruments` with auth headers (W:103-117). | OK | D:890-899 | — |
| CSV parsing | `pd.read_csv` by column name (W:117,164). | **Bug** | D:905-918 | `raw_expiry = fields[4]` -> must be `fields[5]`; `lot_size = fields[5]` -> `fields[8]`; `tick_size = fields[6]` -> `fields[7]`; `strike = fields[7]` -> `fields[6]`. Use the `csv` crate (quoted names with commas). |
| Token | `instrument_token::::exchange_token` (W:201). | OK | D:940 | Consumers must split: WS needs `[0]`, history needs `[0]`. Nothing in desktop does. |
| Exchange map | `NSE,NFO,CDS,BSE,BFO,BCD,MCX,NCO` identity; `GLOBAL->GLOBAL_INDEX`, `NSEIX->GLOBAL_INDEX`; `brexchange` keeps raw Kite code (W:169-189,219). INDICES segment: `NSE->NSE_INDEX, BSE->BSE_INDEX, MCX->MCX_INDEX, CDS->CDS_INDEX` (W:192-195). | Partial | D:911-929 | Add `GLOBAL`/`NSEIX` -> `GLOBAL_INDEX`. (Note `CDS_INDEX` is not in `symbol-format.md`/`order-constants.md`; both web and desktop emit it.) Supported list per `plugin.json:8`: `NSE,BSE,NFO,BFO,CDS,MCX,NCO,NSE_INDEX,BSE_INDEX,MCX_INDEX,GLOBAL_INDEX` (web adds `BCD` via map). |
| Expiry format | `%d-%b-%y` upper, e.g. `28-MAR-24` (W:198). | OK (once column fixed) | D:995-1021 | — |
| FUT symbol | `name + expiry_nodash + "FUT"` (W:281). | OK (once column fixed) | D:943-945 | — |
| CE/PE symbol | `name + expiry_nodash + format_strike(strike) + CE/PE`; strike `190.0->"190"`, `187.5->"187.5"` (W:285-293). | OK (once column fixed) | D:946-953, D:1024-1030 | Rust `format!("{}", 187.5)` -> `187.5` OK. |
| EQ / others | `symbol = tradingsymbol` (W:217-218 then overridden only for FUT/CE/PE). | OK | D:954-957 | — |
| Blank `name` | Back-filled from `brsymbol` (W:268-275). | Missing | — | Add. |
| NSE index renames | ~60 entries (`NIFTY 50->NIFTY`, `NIFTY BANK->BANKNIFTY`, `NIFTY FIN SERVICE->FINNIFTY`, `NIFTY MID SELECT->MIDCPNIFTY`, `NIFTY NEXT 50->NIFTYNXT50`, `INDIA VIX->INDIAVIX`, `NIFTY 100->NIFTY100`, ... W:296-360). | Partial | D:1033-1044 (7 entries) | Port full table. |
| BSE index renames | Applied only where `exchange=='BSE_INDEX'`: `SNSX50->SENSEX50`, `SNXT50->BSESENSEXNEXT50`, `MID150->BSE150MIDCAPINDEX`, `AUTO->BSEAUTO`, `BSE CG->BSECAPITALGOODS`, ... `METAL->BSEMETAL`, `POWER->BSEPOWER` (W:362-401). `SENSEX`, `BANKEX` unchanged. | Partial / Bug | D:1041 | Only `SNSX50` ported and it is applied to every exchange. Port table, gate on `BSE_INDEX`. |
| GLOBAL_INDEX renames | `GIFT NIFTY->GIFTNIFTY` (W:403-409). | Missing | — | Add. |
| MCX lot size | Kite reports `lot_size=1` (contracts); web replaces with real contract size per `name`+expiry (`mapping/mcx_contract_size.py`, W:222-266) and converts qty at Kite boundary. | Missing | — | Port table + `to_kite_quantity`/`from_kite_quantity`, or document that desktop MCX qty is in contracts. |
| Instrument type | Kite values `EQ/FUT/CE/PE` already match contract (`symbol-format.md`). | OK | D:913 | — |
| Persistence | `symtoken` has `expiry, strike, lotsize, instrumenttype, tick_size, brsymbol, brexchange` (W:34-50). | Bug | `state.rs:35-47`, `db/sqlite/symbol.rs:29-31`, `symbol_service.rs:219-229` | `SymbolInfo`/table drop `expiry`, `strike`, `option_type`; `SymbolService` returns `strike: None, expiry: None` (`symbol_service.rs:73-74`). |

### 2.7 WebSocket (market data)

| Item | Web behaviour (`streaming/zerodha_websocket.py`, `zerodha_adapter.py`) | Desktop status | Desktop file:line | Fix |
|---|---|---|---|---|
| URL | `wss://ws.kite.trade?api_key={api_key}&access_token={access_token}` where access_token = second half of stored `api_key:access_token` (ws.py:129, adapter.py:97-104). | **Bug** | `websocket/manager.rs:170-173`; `commands/websocket.rs:52-64`; D:342 | `feed_token` is `public_token`. Store `access_token` as feed token (or split `auth_token` on `:` in `websocket_connect`). |
| Subscribe | Two frames: `{"a":"subscribe","v":[tokens]}` then `{"a":"mode","v":["full",[tokens]]}` (ws.py:440-446); batches of 200, 3000 max per connection (ws.py:61,69). | **Bug** | `manager.rs:976-1002` | Send two separate `Message::Text` frames; `"{}\n{}"` is one invalid frame. Add 200-token batching. |
| Token | `int(token.split("::::")[0])` (adapter.py:291-302). | **Bug** | `manager.rs:979` (`parse().ok()` on `"123::::456"` fails); frontend `useMarketData.ts:150` passes `s.token || s.symbol` | Split on `::::` (in `websocket_subscribe` or `create_zerodha_subscribe`). |
| Mode | Per token, highest subscribed mode (adapter.py:338-343); `1->ltp,2->quote,3->full`. | OK (approx.) | `manager.rs:990-994` | Max over batch rather than per token; acceptable. |
| Unsubscribe | `{"a":"unsubscribe","v":[tokens]}` (ws.py:470). | OK | `manager.rs:1005-1017` | Same `::::` issue. |
| Heartbeat | Server sends 1-byte binary; client ignores it (ws.py:527-530); client relies on WS ping (ws.py:253-254). Client never sends 1-byte frames. | Deviation | `manager.rs:305-309` sends `Message::Binary(vec![0])` every 30 s | Remove; use `Message::Ping`. Incoming 1-byte frame is dropped by `len < 4` check (`manager.rs:557`) — OK. |
| Frame header | `>H` packet count, per packet `>H` length (ws.py:687-702). | OK | `manager.rs:562-586` | — |
| LTP packet (8) | token u32, ltp i32 /100 (ws.py:716-718). | OK | `manager.rs:597-599` | — |
| Quote packet (44) | `>11i`: token, ltp, ltq, atp, volume, total_buy, total_sell, open, high, low, close, all /100 for prices (ws.py:746-769). | OK (semantics) | `manager.rs:631-656` | Desktop puts `total_buy_qty`/`total_sell_qty` into `bid_qty`/`ask_qty` (`manager.rs:649-650`) — mislabelled. |
| Index packets (28 quote / 32 full) | token, ltp, high, low, open, close, change, [exchange_timestamp]. Web does **not** parse these either (falls to LTP-only). | Missing (both) | `manager.rs:628` (`>= 44` only) | Parse `len == 28/32` for NSE_INDEX/BSE_INDEX/MCX_INDEX OHLC. |
| Full packet (184) | 44-48 last_trade_time (web mislabels `price_change`), 48-52 oi, 52-56 oi_day_high, 56-60 oi_day_low, 60-64 exchange_timestamp, depth at 64: 10 x 12 bytes (qty i32, price i32, orders i16, pad i16), buy 64-124, sell 124-184 (ws.py:773-806). | Partial | `manager.rs:658-680` | Reads oi and best bid/ask correctly; `MarketTick` has no depth levels or exchange timestamp. Web publishes 5 levels (adapter.py:729-749). |
| Price divisor | /100 everywhere (ws.py:718,749-761,785,793). Kite spec: CDS /10,000,000; BCD /10,000. Web has the same gap. | Partial (same as web) | `manager.rs:599,643-646` | Divide by segment: `token & 0xFF == 3 (CDS) -> 1e7`, `== 6 (BCD) -> 1e4`. |
| Exchange of tick | From subscription map `token_to_symbol[token] -> (symbol, exchange)` (adapter.py:572-577); index exchanges kept as `NSE_INDEX` etc. | **Bug** | `manager.rs:602-620` | Formula `instrument_token/256 % 256` is wrong (segment is low byte `& 0xFF`), table is wrong (3 should be CDS, 4 BSE, 5 BFO, 6 BCD, 7 MCX, 9 INDICES), and the correct exchange already in `token_map` is thrown away (`let (symbol, _) = ...`). Use `token_map` exchange. |
| Text frames | `type:error` logged; `type:order` postbacks normalised by `zerodha_order_adapter.py:75-123` (status map, MCX units, OA symbol via token). | Missing | `manager.rs:247-250` | Parse `{"type":"order"}` and emit an `order_update` event. |
| Reconnect | Exponential backoff, token refresh from DB, resubscribe all (ws.py:239-319, 654-678). | Missing | `manager.rs:233-262` breaks loop on error | Not zerodha-specific. |

---

## 3. Exchanges / indices coverage summary

| Exchange | Web master contract | Desktop master contract | Desktop quotes prefix | Desktop WS exchange tag |
|---|---|---|---|---|
| NSE, BSE, NFO, BFO, CDS, BCD, MCX, NCO | pass-through, `brexchange` raw | pass-through (OK) | pass-through (OK) | wrong (derived from token, mostly `NSE`) |
| NSE_INDEX / BSE_INDEX / MCX_INDEX / CDS_INDEX | via `segment==INDICES` | OK | NSE/BSE mapped; **MCX_INDEX missing** | wrong |
| GLOBAL_INDEX (Kite `GLOBAL`, `NSEIX`) | mapped, `GIFT NIFTY->GIFTNIFTY` | **Missing** (rows stored as exchange `GLOBAL`/`NSEIX`) | **Missing** (needs `brexchange`) | wrong |

Index renames: desktop 7/≈100.

---

## 4. Suggested fix order

1. Fix CSV column indices (D:906-918) and switch to the `csv` crate; persist `expiry/strike/option_type`.
2. Fix WS: use `access_token` as feed token (D:342), split `::::` before `parse::<u32>()`, send `subscribe` and `mode` as two frames, take exchange from `token_map`, stop sending the 1-byte frame.
3. Fix quote/depth tuple order (D:749,780) and resolve `brsymbol` before calling Kite; add `MCX_INDEX`/`GLOBAL_INDEX` prefixes; batch 500.
4. Normalise book outputs: OA symbol + lowercase status; null-tolerant holdings structs.
5. Port full index rename tables, `GLOBAL/NSEIX->GLOBAL_INDEX`, MCX contract sizes.
6. Add history, margin and order-postback support to the `Broker` trait/WS manager.

---

## A. FYERS adapter audit: openalgo-desktop (Rust) vs openalgo web (Python reference)

Scope: read-only comparison. Desktop file is
`/Users/openalgo/openalgo-desktop/openalgo-desktop/src-tauri/src/brokers/fyers/mod.rs` (1496 lines) plus
`websocket/manager.rs`, `commands/websocket.rs`, `services/{order,position,quotes,history,symbol}_service.rs`, `state.rs`.
Reference is `/Users/openalgo/openalgo-desktop/openalgo/broker/fyers/**`.

Abbreviations: `D:` = desktop `brokers/fyers/mod.rs` line, `W:` = web python `file:line`.
Live Fyers CSV/JSON masters were downloaded on 2026-10-03 to verify column layouts and lot sizes (see "Evidence").

---

## 0. Executive summary (ranked)

| # | Severity | Area | Problem | Where |
|---|----------|------|---------|-------|
| 1 | Critical | Master contract | F&O OpenAlgo symbol built as `NAME+YY+MMM+DD+...` (e.g. `BANKNIFTY26OCT27FUT`) instead of standard `NAME+DD+MMM+YY+...` (`BANKNIFTY27OCT26FUT`). Every NFO/BFO/CDS/MCX symbol in the desktop cache is wrong, so no F&O symbol typed in OpenAlgo format resolves. | D:1452, D:1467 vs W:`database/master_contract_db.py:251-257` |
| 2 | Critical | WebSocket | Fyers login stores `feed_token: None` (D:569) and `websocket_connect` hard-fails on `None` ("No feed token found", `commands/websocket.rs:52-53`). Even if it were set, the HSM auth frame needs the `hsm_key` JWT claim, not the token; and subscribe frames send `EXCHANGE:token` instead of `sf|nse_cm|<scrip>` / `if|nse_cm|<index name>` / `dp|...`. Market data streaming for Fyers cannot work at all. | `websocket/manager.rs:174-214, 1059-1070, 1096-1127` vs W:`streaming/fyers_hsm_websocket.py:207-337`, `fyers_token_converter.py:262-318` |
| 3 | Critical | Quotes/Depth | `get_quote`/`get_market_depth` send `"{exchange}:{openalgo_symbol}"` (`NSE:SBIN`, `NFO:BANKNIFTY27OCT26FUT`, `NSE_INDEX:NIFTY`) - never the brsymbol (`NSE:SBIN-EQ`, `NSE:BANKNIFTY26OCTFUT`, `NSE:NIFTY50-INDEX`). Fyers rejects all of these except accidental matches. No brsymbol lookup happens in `QuotesService` either. | D:1067, D:1127; `services/quotes_service.rs:45, 86` vs W:`api/data.py:148, 311, 703` |
| 4 | Critical | Positions/Smart order | Order book, trade book, positions, holdings return the Fyers symbol with the exchange prefix stripped (`SBIN-EQ`, `BANKNIFTY26OCTFUT`) instead of reverse-mapping brsymbol -> OpenAlgo symbol (web `get_oa_symbol`). `PositionService::get_open_position` matches on OpenAlgo symbol, so smart orders never find the open position; `close_position` re-submits `NFO:BANKNIFTY26OCTFUT` which Fyers rejects. | D:461-467, 795-798, 854-857, 913-916, 972-975 vs W:`mapping/order_data.py:44-62, 251-271` |
| 5 | High | History | No `get_history` in the `Broker` trait; `HistoryService` is a TODO that returns empty candles. Web implements `/data/history` with resolution map and chunking. | `services/history_service.rs:88-99` vs W:`api/data.py:391-622` |
| 6 | High | Master contract | NSE_CD and MCX lot sizes read from CSV "Minimum lot size" (always 1). Web uses the JSON master's `qtyMultiplier` (USDINR 1000, CRUDEOIL 100, NATURALGAS 1250, SILVERM 5). | D:1193-1200, 1285 vs W:`master_contract_db.py:159,164, 580, 714` |
| 7 | High | Master contract | Index symbol normalization missing: NSE indices keep spaces/hyphens (`NIFTY 500`, `BHARATBOND-APR30`), no `NIFTYMID50 -> NIFTYMIDCAP50`, no BSE index map (`100 -> BSE100`, `SNXT50 -> BSESENSEXNEXT50`...). Index `name` is `NIFTYBANK-INDEX` instead of the HSM display name needed for `if|` subscriptions. | D:1316, 1351 vs W:`master_contract_db.py:304-314, 381-451, 209-248` |
| 8 | Medium | Funds | `available_cash` taken from "Available Balance"; web deliberately uses "Clear Balance" to avoid double-counting collateral (GitHub #1582). No `code == 200` check; error bodies silently become zeros. | D:1032-1038 vs W:`api/funds.py:88-93, 112-121` |
| 9 | Medium | Place order | `validity` and `offlineOrder` passed through from the request (web hardcodes `DAY`/`False`); `stopLoss`/`takeProfit`/`orderTag` omitted; unknown product passed raw (web defaults INTRADAY). | D:616-627 vs W:`mapping/transform_data.py:21-35` |
| 10 | Medium | Missing | Margin API (`POST /api/v3/multiorder/margin`), order-update socket (`wss://socket.fyers.in/trade/v3`), 50-level TBT depth socket, exit-all (`DELETE /positions {exit_all:1}`), quote batching (50), OI via depth, 429 rate limiting. | n/a vs W:`api/margin_api.py`, `streaming/fyers_order_adapter.py`, `fyers_tbt_websocket.py`, `api/order_api.py:353-400` |

---

## 1. Function-by-function comparison

Legend for status: **OK** = equivalent; **Bug** = behaves wrongly; **Missing** = not implemented; **Diff** = works but diverges from reference.

### 1.1 Authentication

| Function | Web behaviour | Desktop status | Desktop location | Fix |
|---|---|---|---|---|
| authenticate | `POST https://api-t1.fyers.in/api/v3/validate-authcode`, JSON `{grant_type:"authorization_code", appIdHash: sha256(f"{api_key}:{api_secret}"), code: request_token}`, headers `Content-Type`+`Accept: application/json`, success when `s=="ok"`, token = `access_token` (W:`api/auth_api.py:45-101`). Every later call uses `Authorization: f"{BROKER_API_KEY}:{AUTH_TOKEN}"` (W:`order_api.py:52`). | **OK** | D:494-574. Hash D:453-458 identical. Payload D:509-519 identical. Stores combined `api_key:access_token` as `auth_token` (D:567) and `get_headers` puts it verbatim in `Authorization` (D:445) - equivalent to web. | Minor: add `Accept: application/json`. Store the raw access token (or at least expose it) so the WS layer can decode the JWT `hsm_key` (see WS section). |
| feed_token | n/a in web (HSM key is decoded from the access-token JWT at connect time, W:`fyers_hsm_websocket.py:207-249`). | **Bug** | D:569 `feed_token: None`. `commands/websocket.rs:52-53` then errors "No feed token found". | Set `feed_token = Some(access_token)` (raw JWT) or decode `hsm_key` from JWT payload and store that. |
| user_id | web gets it from the auth response / env. | **Diff** | D:562-564 derives `client_id` from `api_key.split('-')[0]` (the APP ID, not the Fyers client ID). | Cosmetic; fetch `/api/v3/profile` if a real client ID is needed. |

### 1.2 Orders

| Function | Web behaviour | Desktop status | Desktop location | Fix |
|---|---|---|---|---|
| place_order | `POST /api/v3/orders/sync`; payload `symbol=get_br_symbol(sym,exch)` (e.g. `NSE:SBIN-EQ`), `qty`, `type` {MARKET:2, LIMIT:1, SL:4, SL-M:3; unknown->2}, `side` {BUY:1, SELL:-1}, `productType` {CNC:CNC, NRML:MARGIN, MIS:INTRADAY, CO, BO; unknown->INTRADAY}, `limitPrice`, `stopPrice`, `validity:"DAY"`, `disclosedQty`, `offlineOrder:False`, `stopLoss:0`, `takeProfit:0`, `orderTag:"openalgo"`. Success `s=="ok"` -> `id`. No MPP/emulation (W:`transform_data.py:10-37`, `order_api.py:193-246`). | **Diff / partial Bug** | D:575-662. Mapping D:592-602 OK. Symbol D:606-612: uses `order.broker_symbol` resolved by `OrderService` from `SymbolInfo.brsymbol` (`services/order_service.rs:67-71`) - correct format `NSE:SBIN-EQ` when the cache hit; fallback `format!("{}:{}", exchange, symbol)` (D:611) produces invalid `NFO:...`/`NSE_INDEX:...`/`NSE:SBIN`. D:624 `validity` passthrough (IOC would be sent), D:626 `offlineOrder = amo` (web never sends AMO). Missing `stopLoss/takeProfit/orderTag`. Unknown product passed raw (D:403-412). | Fail fast when brsymbol is missing instead of guessing. Hardcode `validity:"DAY"`, `offlineOrder:false` (or keep AMO only if intended). Add `stopLoss:0,takeProfit:0,orderTag:"openalgo"`. Default unknown product to `INTRADAY`. Also surface Fyers `message` on error (done) and `id` on `s=="error"` (web returns it). |
| modify_order | `PATCH /api/v3/orders/sync` with `{id, qty, type, limitPrice, stopPrice}` - all five always present; `type` defaults to 2 if pricetype missing (W:`transform_data.py:40-76`, `order_api.py:453-490`). | **Diff** | D:664-725. Sends only `Some(..)` fields (`skip_serializing_if`). `type` omitted when `order_type` is None. | Fyers requires `type` on modify; send all fields like web (qty default 0, prices default 0.0, type default 2). |
| cancel_order | `DELETE /api/v3/orders/sync` body `{"id": orderid}`; success `s=="ok"` (W:`order_api.py:403-440`). | **OK** | D:727-765. | - |
| cancel_all_orders | Web: fetch `/orders`, cancel those with `status in [4,6]` (W:`order_api.py:507-557`). | **OK** | Generic `services/order_service.rs:198-245` filters `"OPEN"`/`"TRIGGER PENDING"` which desktop maps from 6/4 (D:359-368). | - |
| close_all_positions | Web: `DELETE /api/v3/positions` with `{"exit_all": 1}` (W:`order_api.py:353-390`). | **Diff + Bug** | Not in trait; `services/position_service.rs:167-203` loops positions and places MARKET orders per position via `OrderService::place_order`. Because positions carry `SBIN-EQ`/`BANKNIFTY26OCTFUT` (D:913-916) the symbol-cache lookup misses and the fallback symbol is `NSE:SBIN-EQ` (works by accident) or `NFO:BANKNIFTY26OCTFUT` (rejected). | Add a Fyers override using `exit_all:1`; and fix symbol reverse mapping (1.3). |
| get_open_position | Web converts OA symbol -> brsymbol and matches `position["symbol"] == brsymbol and productType == product(Fyers)` on `netPositions`, returns `netQty` (W:`order_api.py:176-190`). | **Bug** | `services/position_service.rs:66-84` matches on OA `symbol`/`exchange`/`product`, but Fyers positions are returned with symbol `SBIN-EQ` and `product` already mapped to `MIS`. Symbol never matches -> smart orders think no position exists. | Reverse-map brsymbol -> OA symbol in `get_positions` (see 1.3). |
| margin (calculate_margin) | `POST /api/v3/multiorder/margin` `{data:[{symbol,qty,side,type,productType,limitPrice,stopLoss,stopPrice,takeProfit}]}` -> `data.margin_new_order` (W:`api/margin_api.py:11-107`, `mapping/margin_data.py`). | **Missing** | No trait method. | Add `calculate_margin` to Broker trait and implement. |

### 1.3 Books (order/trade/positions/holdings)

| Function | Web behaviour | Desktop status | Desktop location | Fix |
|---|---|---|---|---|
| orderbook | `GET /api/v3/orders` -> `orderBook[]`. Exchange from `(exchange,segment)` map {(10,10)NSE,(10,11)NFO,(10,12)CDS,(12,10)BSE,(12,11)BFO,(11,20)MCX}. **Symbol via `get_oa_symbol(brsymbol, exchange)`**. status {1 cancelled, 2 complete, 4 trigger pending, 5 rejected, 6 open}; type {1 LIMIT,2 MARKET,3 SL-M,4 SL}; product {CNC,INTRADAY->MIS,MARGIN->NRML,CO,BO}; fields qty, limitPrice, stopPrice, id, orderDateTime (W:`order_data.py:10-25, 28-62, 106-159`). | **Bug (symbol)** / rest OK | D:767-824. Exchange map D:346-356 OK (default `NSE` instead of "Unknown Exchange" - fine). Status D:359-368 OK (uppercase convention). Type D:380-388 OK. Product D:391-400 OK. Symbol D:795-798 `extract_symbol_name` just strips `NSE:` -> `SBIN-EQ` / `BANKNIFTY26OCTFUT`. `filledQty`,`tradedPrice`,`message` extra - fine. | Add a `brsymbol -> SymbolInfo` reverse index in `AppState` (keyed `exchange:brsymbol`) and map to OA `symbol` in all four book functions; fall back to raw on miss (web logs a warning). |
| tradebook | `GET /api/v3/tradebook` -> `tradeBook[]`; same symbol mapping; fields side, productType, tradedQty, tradePrice, tradeValue, orderNumber, orderDateTime (W:`order_data.py:162-231`). | **Bug (symbol)** | D:826-883. Same `extract_symbol_name` issue (D:854-857). `order_type` hardcoded MARKET, `status` COMPLETE - acceptable. | As above. |
| positions | `GET /api/v3/positions` -> `netPositions[]`; symbol mapping; `netQty`, `netAvg`, `ltp`, `pl`, productType reverse-map; web also sums `realized_profit`/`unrealized_profit` for funds (W:`order_data.py:234-306`, `funds.py:141-158`). | **Bug (symbol)** | D:885-942. Fields OK (`netQty`, `netAvg`, `ltp`, `pl`, `realized_profit`, `unrealized_profit`). `buy_qty/sell_qty/values` are synthesised from net qty (D:931-934) although Fyers returns `buyQty`,`sellQty`,`buyAvg`,`sellAvg`. | Map symbol; optionally use real `buyQty/sellQty/buyAvg/sellAvg`. |
| holdings | `GET /api/v3/holdings` -> `holdings[]`; `holdingType` HLD/T1 -> CNC; symbol mapping; `quantity`, `costPrice`, `ltp`, `pl`, pnlpercent = (ltp-cost)/cost*100 (W:`order_data.py:309-380`). | **Bug (symbol)** / Diff | D:944-1004. Same symbol issue (D:972-975). `isin` not mapped though Fyers returns it; `close_price = avg_price` (D:995) is a placeholder; `t1_quantity` 0 (Fyers has `holdingType == "T1"`). | Map symbol; map `isin`; set `t1_quantity` from `holdingType=="T1"`. |

### 1.4 Funds

| Function | Web behaviour | Desktop status | Desktop location | Fix |
|---|---|---|---|---|
| get_funds | `GET /api/v3/funds`; require `code == 200`; build `title.lower().replace(" ","_")` map of `{equityAmount, commodityAmount}`; `availablecash = clear_balance` (equity+commodity) - **not** `available_balance` (which already folds collateral in; see comment at W:`funds.py:112-117`); `collateral = collaterals`; `utiliseddebits = utilized_amount`; `m2mrealized/unrealized` from position book (W:`api/funds.py:88-167`). Web returns `{}` on any error so the session is flagged expired. | **Bug** | D:1006-1053. Key builder D:1027-1030 matches. D:1032 uses `available_balance` -> double counts collateral vs web. D:1035 `total_balance` -> `total_margin`/`opening_balance` (reasonable). No `code==200`/`s=="ok"` check (D:1014-1020): a 401 body deserialises to empty `fund_limit` and yields all-zero funds, masking an expired token. | Use `clear_balance` for `available_cash`; check `code==200` and return `Err` otherwise. |

### 1.5 Market data

| Function | Web behaviour | Desktop status | Desktop location | Fix |
|---|---|---|---|---|
| quotes (single) | Web `get_quotes` uses `/data/depth?symbol=<brsymbol>&ohlcv_flag=1` to include OI; fields `bids[0].price`, `ask[0].price`, `o,h,l,ltp,c,v,oi` (W:`data.py:138-184`). | **Bug** | D:1055-1119 uses `/data/quotes?symbols=` with `format!("{}:{}", ex, sym)` (D:1067) i.e. the **OpenAlgo** symbol, not brsymbol -> invalid for equities (`NSE:SBIN`), all F&O (`NFO:`), all indices (`NSE_INDEX:`). Response symbol/exchange come from `n` (brsymbol) so the caller receives `SBIN-EQ`, not the requested OA symbol (D:1083-1088). `oi: 0`, `bid_qty/ask_qty 0`. | Resolve brsymbol in `QuotesService` (or pass `SymbolInfo`) before calling; map response back by brsymbol -> requested (exchange, symbol). |
| multiquotes | `/data/quotes?symbols=a,b,...` **batches of 50**, 0.1s between batches, OI per-symbol via `/data/depth` for FNO exchanges when <=100 symbols; `v` fields `bid, ask, open_price, high_price, low_price, lp, prev_close_price, volume` (W:`data.py:192-389`). | **Bug / Diff** | Same function D:1055-1119. Field mapping D:1090-1105 correct (`lp, open_price, high_price, low_price, prev_close_price, volume, bid, ask, ch, chp`). No batching (Fyers caps at 50/request). | Batch in 50s; brsymbol fix as above. |
| depth | `/data/depth?symbol=<brsymbol>&ohlcv_flag=1`; `d[brsymbol]`; `bids[:5]`/`ask[:5]` -> `{price, quantity: volume}`, pad to 5; plus `totalbuyqty,totalsellqty,h,l,ltp,ltq,o,c,v,oi` (W:`data.py:693-755`). | **Bug (symbol) / Diff (fields)** | D:1121-1185. URL D:1133 correct, `ask` key rename D:310 correct, take(5)+pad D:1155-1177 correct. Symbol D:1127 is OA symbol (same bug). `MarketDepth` struct (`brokers/types.rs:141-146`) has no totals/OHLC/LTP/OI, so those are dropped. `DepthLevel.orders` always 0 (Fyers provides `ord`). | brsymbol fix; extend `MarketDepth` with `totalbuyqty,totalsellqty,ltp,open,high,low,prev_close,volume,oi`; map `ord` to `orders`. |
| history | `/data/history?symbol=<brsymbol>&resolution=R&date_format=1&range_from=YYYY-MM-DD&range_to=YYYY-MM-DD&cont_flag=1[&oi_flag=1 for NFO/BFO/MCX/CDS]`. Resolution map `5s..45s -> 5S..45S`, `1m..30m -> 1..30`, `1h 60, 2h 120, 4h 240, D -> 1D`; W/M unsupported. Chunking: 300 days (1D), 25 days (seconds, and start clamped to last 30 days), 60 days otherwise. Candles are `[ts, o, h, l, c, v]` or 7 columns with OI when `oi_flag` and `len==7`; `date_format=1` returns epoch so no TZ shift; sort+dedupe by timestamp (W:`data.py:109-136, 391-622`). | **Missing** | No `get_history` in `Broker` trait (`brokers/mod.rs:18-82`); `services/history_service.rs:88-99` returns empty with a TODO. | Add `get_history` to trait and port the web logic (resolution map, chunking, 6/7-column handling). |
| option chain | `/data/options-chain-v3?symbol=&strikecount=` (W:`data.py:624-691`). | **Missing** | - | Optional. |

### 1.6 Master contract

| Function | Web behaviour | Desktop status | Desktop location | Fix |
|---|---|---|---|---|
| download URLs | `NSE_CM.csv`, `NSE_FO.csv`, `BSE_CM.csv`, `BSE_FO.csv` as CSV; **`NSE_CD_sym_master.json`** and **`MCX_COM_sym_master.json`** as JSON (for `qtyMultiplier`) (W:`master_contract_db.py:158-165`). Also `index_hsm_mapping.json` for index display names (W:209-231). | **Bug (lot size)** | D:1193-1200 downloads `NSE_CD.csv` and `MCX_COM.csv`. Those CSVs exist (verified) but their "Minimum lot size" column is `1` for every row; JSON `qtyMultiplier` is the real lot (USDINR 1000, CRUDEOIL 100, NATURALGAS 1250, SILVERM 5). | Switch CDS/MCX to the JSON masters and use `qtyMultiplier` for `lot_size`; fields `fyToken, symDetails, expiryDate, strikePrice, tickSize, symTicker, optType`. |
| CSV columns (no header) | 21 cols: 0 Fytoken, 1 Symbol Details, 2 Exchange Instrument type, 3 Minimum lot size, 4 Tick size, 5 ISIN, 6 Trading Session, 7 Last update date, 8 Expiry date (epoch), 9 Symbol ticker, 10 Exchange, 11 Segment, 12 Scrip code, 13 Underlying symbol, 14 Underlying scrip code, 15 Strike price, 16 Option type, 17 Underlying FyToken, 18-20 reserved (W:29-51). | **OK** | D:1285-1294 indices 0,1,2,3,4,8,9,13,15,16 - correct. Naive `split(',')` (D:1232) is safe today: no quoted fields in any of the five CSVs (verified). | - |
| NSE_CM filter | type in {0,9} -> NSE/EQ; type 2 and ticker ends `-GB` -> NSE/EQ; type 10 -> NSE_INDEX/EQ; `symbol = Underlying symbol`; NSE_INDEX symbol: strip spaces and hyphens, then `NIFTYMID50 -> NIFTYMIDCAP50`; `brexchange = NSE` (W:260-346). | **Bug (index symbol) / Diff (instrumenttype)** | D:1294-1329. Filters OK (D:1307-1311). `instrument_type` is `INDEX`/`GB` instead of `EQ` (web keeps the 4-value vocabulary EQ/FUT/CE/PE). D:1316 uses raw underlying -> `NIFTY 500`, `NIFTY ALPHA 50`, `BHARATBOND-APR30`, `NIFTYMID50` (verified in live CSV). | Strip spaces/hyphens for NSE_INDEX; apply `NIFTYMID50 -> NIFTYMIDCAP50`; use `EQ`. |
| BSE_CM filter | type in {0,4,50} -> BSE/EQ; 10 -> BSE_INDEX/EQ; BSE_INDEX symbol via explicit map (`100->BSE100`, `SNXT50->BSESENSEXNEXT50`, `CG->BSECAPITALGOODS`, ...) with upper/no-space fallback (W:349-483). | **Bug (index symbol)** | D:1331-1364. Filter OK. D:1351 raw underlying -> `100`, `200`, `ALLCAP`, `BASMTR`... (verified). | Port the `bse_index_map`. |
| F&O symbol | `reformat_symbol_detail("BANKNIFTY 27 Oct 26 FUT") = parts[0]+parts[1]+parts[2].upper()+parts[3]+parts[4]` -> **`BANKNIFTY27OCT26FUT`** (DDMMMYY); options `+CE/PE` -> `BANKNIFTY27OCT2671900CE` (W:251-257, 514-524). | **Critical Bug** | D:1439-1484. D:1452 `format!("{}{}{}{}{}", name, year, month, day, suffix)` -> **`BANKNIFTY26OCT27FUT`**, `BANKNIFTY26OCT2771900CE`. Doc-comment D:1434-1436 even states the wrong target. Shorter-form branch D:1467 same order. | Change to `name, day, month, year, suffix`. |
| F&O instrumenttype / expiry / strike | `instrumenttype = Option type` with `XX -> FUT` (BFO NaN -> FUT); expiry `to_datetime(epoch, unit="s").strftime("%d-%b-%y").upper()` (UTC); strike raw; `brexchange = exchange` (W:486-554, 620-688). | **OK** | D:1387-1394 (`XX`/empty -> FUT), D:1421-1436 UTC `%d-%b-%y` uppercased -> `27-OCT-26`, strike `>0` else None (web keeps -1.0; harmless). | - |
| index HSM names | `fetch_index_hsm_names()` stores the HSM display name (e.g. "Nifty IT") in `symtoken.name` for index rows (W:209-248) - the streaming converter subscribes `if|nse_cm|<name>`. | **Missing** | D:1319 stores `Symbol Details` (`NIFTYBANK-INDEX`). | Fetch `index_hsm_mapping.json` and set `name`. |
| dedupe | web skips tokens already inserted (W:116-139). | **Diff** | `db/sqlite/symbol.rs:29` `INSERT OR REPLACE`. Fine. | - |

### 1.7 WebSocket (market data) - desktop `websocket/manager.rs`

| Item | Web behaviour | Desktop status | Desktop location | Fix |
|---|---|---|---|---|
| URL | `wss://socket.fyers.in/hsm/v1-5/prod` (W:`fyers_hsm_websocket.py:29`). | **OK** | `manager.rs:174`. | - |
| Connect headers | None (plain websocket). | **Diff** | `manager.rs:191-199` sends `Authorization: <feed_token>` and a UA. Harmless. | Drop. |
| HSM key | Decode JWT payload of the access token (after stripping `appid:`), read `hsm_key`, check `exp` (W:207-249). | **Bug** | `manager.rs:212` passes `feed_token` (which is `None` for Fyers -> connect fails at `commands/websocket.rs:52`). No JWT decode anywhere. | Store access token as feed_token; base64url-decode the JWT payload and use `hsm_key`. |
| Auth frame | `[u16 len=buffer_size-2][1][4] [1][u16][hsm_key] [2][u16 1]"P" [3][u16 1][0x01] [4][u16][source]`, source `"OpenAlgo-HSM"` (W:251-291). | **OK (layout)** | `manager.rs:1020-1056` byte-identical layout, source `"openalgo-desktop"`. | - |
| Auth ack | Type-1 frame: field at `[5:7]` len, value must be `"K"`; otherwise auth rejected (W:340-355, 369-391). | **Missing** | `manager.rs:698-702` ignores type 1 entirely; state set Connected before auth. | Parse ack; only subscribe after `"K"`. |
| Subscribe frame | `[u16 6+len][4][2] [1][u16 len][u16 count]{[u8 len][ascii token]}* [2][u16 1][channel 11]`; **tokens are HSM topics**: `sf|<seg>|<fytoken[10:]>` (quote/LTP), `dp|<seg>|<fytoken[10:]>` (5-level depth), `if|<seg>|<index display name>` (indices, any mode). `seg` from `fytoken[:4]` {1010 nse_cm, 1011 nse_fo, 1120 mcx_fo, 1210 bse_cm, 1211 bse_fo, 1212 bcs_fo, 1012 cde_fo, 1020 nse_com} (W:293-337; `fyers_token_converter.py:33-41, 262-318`). | **Bug** | `manager.rs:1059-1093`: frame layout OK but each entry is `format!("{}:{}", req.exchange, req.token)` (`NSE:10100000003045`) - not an HSM topic. No mode distinction (depth vs quote). | Build `sf|`/`dp|`/`if|` topics from `SymbolInfo.token` (fytoken) and `exchange`; index name from `SymbolInfo.name`. |
| Unsubscribe | Web does **not** send an unsubscribe (HSM has no selective unsubscribe; W:`fyers_adapter.py:326-333`). | **Diff (unverified)** | `manager.rs:1096-1127` emits request type 5 with the same wrong topic strings. | Remove or verify protocol. |
| Data frame header | type 6; scrip count at `[7:9]` BE; then per scrip `data_type` 83 snapshot / 85 update (W:420-457). | **OK** | `manager.rs:698-717`. | - |
| Snapshot `sf|` | `topic_id` (2B, native/LE in python), `u8 name_len`, topic name; `u8 field_count`; `field_count` x `i32 BE` (sentinel `-2147483648` = absent) mapped to DATA_FIELDS `[ltp, vol_traded_today, last_traded_time, exch_feed_time, bid_size, ask_size, bid_price, ask_price, last_traded_qty, tot_buy_qty, tot_sell_qty, avg_trade_price, OI, low_price, high_price, Yhigh, Ylow, lower_ckt, upper_ckt, open_price, prev_close_price, type, symbol]`; skip 2; `u16 multiplier`; `u8 precision`; 3 x (`u8 len`, str) = exchange, exchange_token, symbol (W:506-595). | **Mostly OK** | `manager.rs:754-863`: layout and field indices (0 ltp, 1 vol, 3 exch_time, 4/5 sizes, 6/7 bid/ask, 12 OI, 13 low, 14 high, 19 open, 20 prev_close) match. Reads `topic_id` big-endian (python native). No `-2147483648` sentinel handling (absent fields become huge negatives). | Treat `i32::MIN` as 0/None. |
| Price scaling | `price / multiplier / segment_divisor` where `segment_divisor = 100` for non-index NSE/BSE/NFO/MCX/CDS/BCD, then round to `precision` (W:`fyers_mapping.py:124-144`). | **Bug** | `manager.rs:838-855` divides by `multiplier` only (default 100 if missing). Per the web rule prices for scrips come out 100x too large when multiplier is 1. | Apply the same divisor logic. |
| Snapshot `if|` (index) | 8 fields `[ltp, prev_close_price, exch_feed_time, high_price, low_price, open_price, type, symbol]`, **no** multiplier/precision/string block (W:597-638). | **Bug** | Desktop uses the `sf|` layout for every topic (`manager.rs:754`): index snapshots misparse and desync the offset for the rest of the frame. | Branch on topic prefix. |
| Snapshot `dp|` (depth) | 32 fields `bid_price1..5, ask_price1..5, bid_size1..5, ask_size1..5, bid_order1..5, ask_order1..5, type, symbol` + multiplier/precision + strings (W:640-719). | **Missing** | Not handled. | Implement. |
| Update frames (85) | `topic_id`, `field_count`, `field_count` x i32 applied positionally to the stored snapshot for that topic; emit changed record (W:721-820). Live ticks after the first snapshot arrive **only** this way. | **Bug** | `manager.rs:740-749` skips updates (`offset += 3 + field_count*4`). Desktop will show one snapshot per symbol and never update. | Keep per-topic state keyed by `topic_id`, apply updates, emit ticks. |
| 50-level depth | Separate TBT socket `wss://rtsocket-api.fyers.in/versova`, `Authorization: <appid:token>` header, protobuf `msg_pb2` (W:`fyers_tbt_websocket.py:32, 105, 216`). | **Missing** | - | Optional. |
| Index synthetic depth | Index `if|` quotes are turned into synthetic depth for depth subscribers (W:`fyers_mapping.py:337-430`). | **Missing** | - | Optional. |
| Reconnect / token refresh / health check | Reconnect with backoff, re-read token at 3 AM rollover, 90s stall detection (W:980-1136). | **Missing** | - | Optional, but needed for all-day use. |

### 1.8 Order-update socket

| Item | Web behaviour | Desktop status | Fix |
|---|---|---|---|
| Order updates | `wss://socket.fyers.in/trade/v3`, header `Authorization: f"{app_id}:{access_token}"`, after open send `{"T":"SUB_ORD","SLIST":["orders"],"SUB_T":1}`, records arrive under `orders`/`d`/`data` (W:`fyers_order_adapter.py:6-17, 37, 141-150`). | **Missing** (no `trade/v3` anywhere in desktop). | Implement if order-update streaming is in scope. |

---

## 2. Symbol resolution architecture (root cause of items 3 and 4)

- The desktop `AppState` has `symbol_cache` keyed `exchange:token` and `symbol_reverse_cache` keyed `exchange:symbol` (OpenAlgo symbol) -> token (`state.rs:158-184`). There is **no index keyed by brsymbol**.
- Only `OrderService::place_order` looks up `SymbolInfo.brsymbol` (`services/order_service.rs:67-71`). `QuotesService`, `PositionService`, holdings/orderbook paths do not.
- The Fyers adapter therefore (a) receives OpenAlgo symbols for quotes/depth and builds `"{exchange}:{symbol}"` which is only coincidentally valid for nothing in Fyers (equities need `-EQ`, F&O brexchange is `NSE`/`BSE`/`MCX`, indices need `-INDEX`); and (b) returns Fyers-shaped symbols (`SBIN-EQ`) from books, which no downstream lookup (`get_symbol_by_name(exchange, symbol)`) understands.
- Fix shape: add `brsymbol_cache: DashMap<"exchange:brsymbol", token>` populated in `load_symbol_cache`; give the broker trait access to a resolver (or have services pre-resolve both directions) and use it in all Fyers book/quote functions, mirroring `get_br_symbol`/`get_oa_symbol` in the web.

---

## 3. Evidence (live Fyers master files, 2026-10-03)

```
NSE_CM.csv  RELIANCE row: ...,0,1,0.1,...,NSE:RELIANCE-EQ,10,10,2885,RELIANCE,...   (type 0, lot 1)
NSE_CM.csv  index rows (type 10) underlying with spaces/hyphens:
            NSE:NIFTY500-INDEX | NIFTY 500 ; NSE:NIFTYALPHA50-INDEX | NIFTY ALPHA 50 ; NSE:BHARATBOND-APR30-INDEX | BHARATBOND-APR30
            NSE:NIFTYMIDCAP50-INDEX | NIFTYMID50   (web overrides to NIFTYMIDCAP50)
BSE_CM.csv  index rows underlying: 100, 200, 500, 150MIDCAP, ALLCAP, BASMTR ... (web maps to BSE100, BSE200, ...)
NSE_FO.csv  "BANKNIFTY 27 Oct 26 FUT" -> web BANKNIFTY27OCT26FUT ; desktop BANKNIFTY26OCT27FUT
            "BANKNIFTY 27 Oct 26 71900 CE" -> web BANKNIFTY27OCT2671900CE ; desktop BANKNIFTY26OCT2771900CE
MCX_COM.csv CRUDEOIL 19 Oct 26 FUT: Minimum lot size = 1 ; MCX_COM_sym_master.json qtyMultiplier = 100
NSE_CD.csv  USDINR 09 Oct 26 FUT:   Minimum lot size = 1 ; NSE_CD_sym_master.json qtyMultiplier = 1000
All five CSVs: 21 columns, zero quoted fields (naive split(',') is currently safe).
```

---

## 4. Prioritised fix list

1. `reformat_symbol_detail` order -> `name + day + month + year + suffix` (D:1452, D:1467).
2. Add brsymbol reverse index; use `get_br_symbol`-equivalent in `get_quote`/`get_market_depth` (and batch quotes by 50), and `get_oa_symbol`-equivalent in orderbook/tradebook/positions/holdings.
3. Fyers login: set `feed_token = Some(access_token)`; in `manager.rs` decode JWT `hsm_key`; build `sf|/dp|/if|` topics from fytoken + exchange; handle auth ack `"K"`; branch snapshot parsing on topic prefix; implement update (85) frames; apply `/multiplier/100` scaling; handle `i32::MIN` sentinel.
4. CDS/MCX masters from JSON with `qtyMultiplier`; NSE/BSE index symbol normalization + maps; store HSM index display names in `name`; `instrument_type = EQ` for index/GB rows.
5. Add `get_history` to the trait with the web resolution map, chunking (300/60/25 days), `oi_flag` for derivatives, 6-vs-7 column handling.
6. Funds: `clear_balance` for available cash; check `code == 200`.
7. Place order: hardcode `validity DAY`, `offlineOrder false`, add `stopLoss/takeProfit/orderTag`, default unknown product to INTRADAY, fail on missing brsymbol. Modify order: always send `type/qty/limitPrice/stopPrice`.
8. Add Fyers `close_all_positions` via `DELETE /positions {exit_all:1}`; add margin API; order-update socket (optional).


---

# Part B - Spec sheets for brokers to add (upstox, dhan, kotak, groww)

Each spec is extracted literally from the web plugin with web file:line citations so a Rust developer can implement without reading Python.

---

## B. UPSTOX broker — literal implementation spec (port target: Rust)

Source of truth read in full: every file under `/Users/openalgo/openalgo-desktop/openalgo/broker/upstox/` plus the cross-cutting files named in each section. Nothing was modified.

**Citation legend** (every `path:line` below is relative to `/Users/openalgo/openalgo-desktop/openalgo/`):

| Abbrev | Path |
| --- | --- |
| `plugin.json` | `broker/upstox/plugin.json` |
| `auth_api.py` | `broker/upstox/api/auth_api.py` |
| `order_api.py` | `broker/upstox/api/order_api.py` |
| `data.py` | `broker/upstox/api/data.py` |
| `funds.py` | `broker/upstox/api/funds.py` |
| `margin_api.py` | `broker/upstox/api/margin_api.py` |
| `gtt_api.py` | `broker/upstox/api/gtt_api.py` |
| `rate_limiter.py` | `broker/upstox/api/rate_limiter.py` |
| `transform_data.py` | `broker/upstox/mapping/transform_data.py` |
| `order_data.py` | `broker/upstox/mapping/order_data.py` |
| `margin_data.py` | `broker/upstox/mapping/margin_data.py` |
| `gtt_data.py` | `broker/upstox/mapping/gtt_data.py` |
| `master_contract_db.py` | `broker/upstox/database/master_contract_db.py` |
| `MarketDataFeedV3.proto` | `broker/upstox/streaming/MarketDataFeedV3.proto` |
| `MarketDataFeedV3_pb2.py` | `broker/upstox/streaming/MarketDataFeedV3_pb2.py` |
| `upstox_client.py` | `broker/upstox/streaming/upstox_client.py` |
| `upstox_adapter.py` | `broker/upstox/streaming/upstox_adapter.py` |
| `upstox_mapping.py` | `broker/upstox/streaming/upstox_mapping.py` |
| `upstox_order_adapter.py` | `broker/upstox/streaming/upstox_order_adapter.py` |
| `brlogin.py` | `blueprints/brlogin.py` |
| `broker_credentials.py` | `blueprints/broker_credentials.py` |
| `BrokerSelect.tsx` | `frontend/src/pages/BrokerSelect.tsx` |
| `config.py` | `utils/config.py` |
| `auth_utils.py` | `utils/auth_utils.py` |
| `base_adapter.py` | `websocket_proxy/base_adapter.py` |
| `ws_mapping.py` | `websocket_proxy/mapping.py` |
| `order_adapter.py` | `websocket_proxy/order_adapter.py` |
| `token_db_enhanced.py` | `database/token_db_enhanced.py` |
| `backpressure.py` | `utils/broker_backpressure.py` |
| `mpp_slab.py` | `utils/mpp_slab.py` |
| `skill:history-data.md` | `.claude/skills/broker-integration/references/history-data.md` |

There is **no** `api/baseurl.py` and **no** `mapping/exchange.py` in `broker/upstox/` (directory listing). Base URLs are string constants inside each module (see §2). Exchange mapping lives in `master_contract_db.py` (download side) and `upstox_mapping.py` (streaming/GTT side).

---

## 0. Plugin manifest

`plugin.json:1-11`:

```json
{
    "Plugin Name": "upstox",
    "Plugin URI": "https://openalgo.in",
    "Description": "Upstox OpenAlgo Plugin",
    "Version": "1.0",
    "Author": "Rajandran R",
    "Author URI": "https://openalgo.in",
    "supported_exchanges": ["NSE", "BSE", "NFO", "BFO", "CDS", "BCD", "MCX", "NSE_INDEX", "BSE_INDEX", "GLOBAL_INDEX"],
    "broker_type": "IN_stock",
    "leverage_config": false
}
```

---

## 1. Auth flow

### 1.1 Credentials the user supplies

| Env var | Read at | Meaning |
| --- | --- | --- |
| `BROKER_API_KEY` | `config.py:11-18` (`get_broker_api_key()` = `os.getenv("BROKER_API_KEY")`); `auth_api.py:14` | Upstox app **API Key** = OAuth `client_id` |
| `BROKER_API_SECRET` | `config.py:21-28`; `auth_api.py:15` | Upstox app **API Secret** = OAuth `client_secret` |
| `REDIRECT_URL` | `auth_api.py:16`; `broker_credentials.py:80` | OAuth redirect URI registered in the Upstox developer app |

All three must be non-empty, else `authenticate_broker` returns `(None, "Configuration error: Missing API credentials.")` (`auth_api.py:18-22`).

`REDIRECT_URL` convention: must match regex `^https?://.+/[^/]+/callback$` (`broker_credentials.py:163`) and the broker name is derived from it with `/([^/]+)/callback$` (`broker_credentials.py:59-62`). So for Upstox it is `<HOST_SERVER>/upstox/callback`, e.g. `http://127.0.0.1:5000/upstox/callback` (`HOST_SERVER` default `http://127.0.0.1:5000`, `config.py:51-57`; docs `docs/broker-integration-guide.md:1175` "`REDIRECT_URL` in `.env` is set to `https://domain/<broker>/callback`").

The user-facing docs list Upstox credentials as "API Key, Secret" from https://api.upstox.com (`docs/userguide/05-first-time-setup/README.md:120`).

### 1.2 Login URL opened in the browser (built client-side)

`BrokerSelect.tsx:187-188`:

```
https://api.upstox.com/v2/login/authorization/dialog?response_type=code&client_id=${broker_api_key}&redirect_uri=${redirect_url}
```

`broker_api_key` and `redirect_url` come from the backend broker-config payload (`BrokerSelect.tsx:131`, `broker_credentials.py:112` returns `"redirect_url": redirect_url` from `get_env_value("REDIRECT_URL")`, `:80`). `redirect_url` is interpolated **unencoded** (no `encodeURIComponent`) in the template string.

### 1.3 Callback

- Route: `@brlogin_bp.route("/<broker>/callback", methods=["POST", "GET"])` (`brlogin.py:37`), rate-limited by `LOGIN_RATE_LIMIT_MIN`/`LOGIN_RATE_LIMIT_HOUR` (`:38-39`; defaults `"5 per minute"`, `"25 per hour"`, `config.py:31-48`).
- A logged-in OpenAlgo session is required (`"user" in session`, else redirect to login) (`brlogin.py:55-57`).
- Auth function resolved as `app.broker_auth_functions["upstox_auth"]` (`brlogin.py:64-65`).
- There is **no** `upstox`-specific branch (grep for "upstox" in `brlogin.py` returns nothing). Upstox falls into the generic `else` branch (`brlogin.py:1030-1034`):

```python
code = request.args.get("code") or request.args.get("request_token")
auth_token, error_message = auth_function(code)
forward_url = "broker.html"
```

  i.e. the callback query param consumed is **`code`**.
- On success: `session["broker"] = "upstox"` (`:1038`); the token is **not** prefixed (only `zerodha` and `dhan` modify it, `:1040-1043`); `handle_auth_success(auth_token, session["user"], broker, feed_token=None)` (`:1073`). `handle_auth_success` stores the token encrypted in the DB via `upsert_auth(user_session_key, auth_token, broker, feed_token=None, user_id=None)` (`auth_utils.py:518-520`) and never puts it in the Flask session (`auth_utils.py:462-466`), then triggers the master-contract download (`:524-530`).
- On failure: `handle_auth_failure(error_message, forward_url="broker.html")` (`:1075`).

### 1.4 Token exchange request (`authenticate_broker(code)`, `auth_api.py:12-70`)

| Item | Literal value | Cite |
| --- | --- | --- |
| Method | `POST` | `auth_api.py:34` |
| URL | `https://api.upstox.com/v2/login/authorization/token` | `:24` |
| Content-Type | `application/x-www-form-urlencoded` (httpx `client.post(url, data=data)` form-encodes; no explicit header set) | `:34` |
| Body fields | `code=<callback code>`, `client_id=<BROKER_API_KEY>`, `client_secret=<BROKER_API_SECRET>`, `redirect_uri=<REDIRECT_URL>`, `grant_type=authorization_code` | `:25-31` |
| Success | HTTP 200 and JSON `access_token` present → return `(access_token, None)` | `:36-41` |
| 200 but no token | `(None, "Authentication succeeded but no access token was returned.")` | `:42-45` |
| Non-200 | parse JSON, join `errors[].message` with `"; "` → `(None, "Upstox API Error: <joined>")`; if body not JSON → `(None, "Upstox API authentication failed.")` | `:46-62` |
| Transport error | `(None, "An HTTP request error occurred: <e>")` | `:64-66` |

### 1.5 Stored auth token format

The raw Upstox `access_token` string, unmodified (`auth_api.py:41`, `brlogin.py:1036-1073`). Every later call sends it as `Authorization: Bearer <access_token>` (see §2). The streaming client treats a token shorter than 10 chars as invalid (`upstox_client.py:645-648`).

### 1.6 Token expiry behaviour

- Upstox tokens expire daily; code comments say rollover "~3 AM IST" (`upstox_client.py:91-93`, `:653`). Nothing refreshes it automatically — the user must log in again.
- Streaming authorize returning HTTP **401** is treated as "stored login expired": first refusal logs a warning, later ones are debug-only, reconnects continue quietly (`upstox_client.py:708-723`, `:120-125`); after 50 failed attempts the error "Stopped retrying Upstox market data: Upstox still refuses the stored login. Log in to Upstox again and market data restarts." is raised (`:238-243`).
- On every reconnect the client re-reads the token from DB with `get_auth_token(user_id, bypass_cache=True)` (`upstox_client.py:650-671`).
- Funds endpoint returns HTTP **423** with `errors[].errorCode == "UDAPI100072"` outside service hours ("5:30 AM to 12:00 AM IST"); the plugin returns all-zero funds in that case (`funds.py:98-115`).

---

## 2. Base URLs and headers

| Constant / literal | Value | Used for | Cite |
| --- | --- | --- | --- |
| `UPSTOX_BASE_URL` | `https://api.upstox.com` | v2 order book / trade book / positions / holdings | `order_api.py:39` |
| `UPSTOX_HFT_BASE_URL` | `https://api-hft.upstox.com` | **v3 order place / modify / cancel only** ("Upstox serves [v3 orders] only from the low-latency api-hft host") | `order_api.py:40-43` |
| data.py base | `https://api.upstox.com/v3{endpoint}` | quotes, depth, history | `data.py:51` |
| indicator LTP | `https://api.upstox.com/v2/market-quote/ltp?instrument_key=` | GLOBAL_INDICATOR only | `data.py:324` |
| funds | `https://api.upstox.com/v3/user/get-funds-and-margin` | funds | `funds.py:40` |
| margin | `https://api.upstox.com/v2/charges/margin` | basket margin | `margin_api.py:76-77` |
| `_GTT_BASE` | `https://api.upstox.com/v3/order/gtt` | GTT | `gtt_api.py:51-52` |
| `AUTH_ENDPOINT` | `https://api.upstox.com/v3/feed/market-data-feed/authorize` | market WS authorize | `upstox_client.py:36-37` |
| `UPSTOX_PORTFOLIO_AUTH_URL` | `https://api.upstox.com/v2/feed/portfolio-stream-feed/authorize` | order WS authorize | `upstox_order_adapter.py:25` |
| `UPSTOX_PORTFOLIO_DIRECT_WS_URL` | `wss://api.upstox.com/v2/feed/portfolio-stream-feed?update_types=order` | order WS fallback | `upstox_order_adapter.py:29-31` |
| master contract | `https://assets.upstox.com/market-quote/instruments/exchange/complete.json.gz` | symbol master | `master_contract_db.py:341` |

Header sets (all literal):

| Caller | Headers | Cite |
| --- | --- | --- |
| `order_api.get_api_response` (GET/POST/PUT/DELETE) and `place_order_api` | `Authorization: Bearer <token>`, `Content-Type: application/json`, `Accept: application/json` | `order_api.py:101-105`, `:368-372` |
| `data.get_api_response`, `_get_indicator_ltp` | `Authorization: Bearer <token>`, `Accept: application/json` | `data.py:48`, `:320-323` |
| funds | `Authorization: Bearer <token>`, `Accept: application/json`, **`Api-Version: 3.0`** | `funds.py:27-31` |
| margin, GTT | `Authorization: Bearer <token>`, `Content-Type: application/json`, `Accept: application/json` | `margin_api.py:54-58`, `gtt_api.py:73-79` |
| WS authorize | `Accept: application/json`, `Authorization: Bearer <token>` | `upstox_client.py:676` |
| Order WS authorize / direct | `Authorization: Bearer <token>`, `Accept: application/json` (authorize); `Authorization: Bearer <token>` only (direct wss handshake) | `upstox_order_adapter.py:73-76`, `:94-97` |

POST/PUT bodies are sent as a pre-serialised JSON string (`content=payload` with `json.dumps(...)`) (`order_api.py:111-113`, `:364`, `:380-382`).

Generic Upstox response envelope consumed everywhere: `{"status": "success"|"error", "data": ..., "errors": [{"errorCode"|"error_code": "...", "message": "..."}]}` (`order_api.py:283-304`, `gtt_api.py:90-112`).

---

## 3. REST endpoints

### 3.1 Place order — `POST https://api-hft.upstox.com/v3/order/place` (`order_api.py:326-413`)

Pre-steps: `token = get_token(symbol, exchange)` (DB `SymToken.token`, which for Upstox is the instrument key, see §4.6) (`:338-341`); `newdata = transform_data(data, token)` (`:343`).

Request JSON (all values exactly as built, `order_api.py:344-362`; sources `transform_data.py:22-40`):

```json
{
  "quantity": "<data.quantity as given>",
  "product": "D" | "I",
  "validity": "DAY",
  "price": "<price or \"0\">",
  "tag": "openalgo",
  "instrument_token": "NSE_EQ|INE848E01016",
  "order_type": "MARKET" | "LIMIT" | "SL" | "SL-M",
  "transaction_type": "BUY" | "SELL",
  "disclosed_quantity": "<disclosed_quantity or \"0\">",
  "trigger_price": "<trigger_price or \"0\">",
  "is_amo": "false",
  "market_protection": -1 | 1..25        // OPTIONAL — only when caller supplied a usable value
}
```

Rules:
- `price` is passed only when `order_type in ("LIMIT","SL")`, else `"0"`; `trigger_price` only when `order_type in ("SL","SL-M")`, else `"0"` (Upstox rejects non-zero price on MARKET/SL-M with `UDAPI1040 "Price not required"`) (`transform_data.py:15-19`).
- `is_amo` is the **string** `"false"` (`transform_data.py:33`); `place_order_api`'s `newdata.get("is_amo", False)` default is never hit (`order_api.py:355`).
- `tag` is `"openalgo"` (`transform_data.py:27`; `order_api.py:349` default `"string"` never hit).
- `market_protection`: `int(value)`; kept only if `== -1` or `1 <= v <= 25`; absent key selects Upstox's own `-1` default; Upstox applies it to MARKET and SL-M only (`transform_data.py:63-87`).

Rate limit: `apply_rate_limit("order")` before send; **no 429 retry** on place (`order_api.py:373-379`).

Response: `response.raise_for_status()` then JSON. If `status == "success"`: `order_id = _extract_order_id(data)` → `data["order_ids"][0]` if list (v3 sliced orders yield one id per slice), else `data["order_ids"]` if scalar, else `data["order_id"]` (`order_api.py:307-323`, `:392-397`). Returns `(response, response_data, order_id)`.
HTTP error: body flattened by `_extract_error`: `message = " | ".join(f"{errorCode|error_code}: {message}")`, or `"Failed to place order."` (`:283-304`, `:406-410`).

### 3.2 Modify order — `PUT https://api-hft.upstox.com/v3/order/modify` (`order_api.py:580-611`)

Body (`transform_data.py:45-60`):

```json
{
  "quantity": "<quantity>",
  "validity": "DAY",
  "price": "<price>",
  "order_id": "<orderid>",
  "order_type": "MARKET"|"LIMIT"|"SL"|"SL-M",
  "disclosed_quantity": "<disclosed_quantity or \"0\">",
  "trigger_price": "<trigger_price or \"0\">",
  "market_protection": <optional, same validation>
}
```

Note: modify does **not** zero price/trigger by order type (passes `data["price"]` and `trigger_price` through). Success → `{"status":"success","orderid": data.order_id}` (via `_extract_order_id`), 200; else `{"status":"error","message": ...}`, 400 (`order_api.py:598-605`).

### 3.3 Cancel order — `DELETE https://api-hft.upstox.com/v3/order/cancel?order_id={orderid}` (`order_api.py:549-577`)

No body. Success → `{"status":"success","orderid": <data.order_id>}`, 200; else 400 (`:562-571`).

### 3.4 Cancel all — client-side loop (`order_api.py:614-662`)

No multi-cancel endpoint is used. `GET /v2/order/retrieve-all`, keep orders where `order["status"] in ["open", "trigger pending"]` (lowercase literal match on the raw Upstox status, `:627-631`), then call §3.3 for each `order["order_id"]`; returns `(canceled_ids, failed_ids)`.

### 3.5 Order book — `GET https://api.upstox.com/v2/order/retrieve-all` (`order_api.py:170-172`)

Fields consumed from each `data[]` item: `instrument_token`, `exchange`, `product`, `transaction_type`, `status`, `tradingsymbol` (overwritten), `quantity`, `price`, `trigger_price`, `order_type`, `order_id`, `order_timestamp` (`order_data.py:74-92`, `:157-169`).

### 3.6 Trade book — `GET https://api.upstox.com/v2/order/trades/get-trades-for-day` (`order_api.py:175-177`)

Fields: `instrument_token`, `exchange`, `product`, `tradingsymbol`, `transaction_type`, `quantity`, `average_price`, `order_id`, `order_timestamp` (`order_data.py:176-195`).

### 3.7 Positions — `GET https://api.upstox.com/v2/portfolio/short-term-positions` (`order_api.py:180-182`)

Fields: `instrument_token`, `exchange`, `product`, `tradingsymbol`, `quantity`, `average_price`, `buy_price`, `day_buy_price`, `sell_price`, `day_sell_price`, `pnl`, `last_price` (`order_data.py:213-246`); `realised`, `unrealised` (funds, `funds.py:78-79`). Response wrapper check `status == "success"` (`order_api.py:215-217`).

### 3.8 Holdings — `GET https://api.upstox.com/v2/portfolio/long-term-holdings` (`order_api.py:185-187`)

Fields: `product`, `tradingsymbol`, `exchange`, `quantity`, `average_price`, `last_price`, `pnl`, `instrument_token` (`order_data.py:258-284`, `:311-312`, `:323-326`).

### 3.9 Funds — `GET https://api.upstox.com/v3/user/get-funds-and-margin` (`funds.py:17-127`)

Headers include `Api-Version: 3.0` (`:30`). Rate category `standard` (`:47`). Response path consumed:

```
data.available_to_trade.cash_available_to_trade.total
data.available_to_trade.cash_available_to_trade.margin_used.total
data.available_to_trade.pledge_available_to_trade.margin_from_pledge.total
data.available_to_trade.pledge_available_to_trade.margin_used.total
```
(`:59-70`; comment: `pledge_available_to_trade` has **no** top-level `total`, `:63-64`).

Output (strings formatted `%.2f`, `:84-90`):

| OpenAlgo key | Formula |
| --- | --- |
| `availablecash` | `cash.total` |
| `collateral` | `pledge.margin_from_pledge.total - pledge.margin_used.total` |
| `utiliseddebits` | `cash.margin_used.total + pledge.margin_used.total` |
| `m2mrealized` | `sum(position["realised"])` over §3.7 positions (after `map_order_data`) |
| `m2munrealized` | `sum(position["unrealised"])` |

If body `status == "error"` → `{}` (`:54-57`). HTTP 423 + `UDAPI100072` → all `"0.00"` (`:98-115`). Other errors → `{}`.

### 3.10 Margin calculator — `POST https://api.upstox.com/v2/charges/margin` (`margin_api.py:11-106`)

Request (`margin_data.py:55-64`, `margin_api.py:61`):

```json
{"instruments": [
  {"instrument_key": "NSE_EQ|INE002A01018", "quantity": 10, "transaction_type": "BUY", "product": "D"|"I", "price": 123.4 /* optional, only if >0 */}
]}
```

Max 20 instruments, else error `"Upstox supports maximum 20 instruments per margin request. Please reduce the number of positions."` with status 400 (`margin_api.py:40-51`). Positions whose `get_token` is missing or has no `|` are skipped (`margin_data.py:31-52`). Rate category `standard` (`margin_api.py:72`).

Response consumed: `data.required_margin`, `data.final_margin`, `data.margins[].span_margin`, `data.margins[].exposure_margin` (`margin_data.py:115-133`). Output:

```json
{"status":"success","data":{"total_margin_required": <required_margin>, "span_margin": <sum span>, "exposure_margin": <sum exposure>}}
```
(`margin_data.py:136-142`). `margin_benefit = required - final` is computed but not returned (`:122`).

### 3.11 Quotes — `GET https://api.upstox.com/v3/market-quote/quotes?instrument_key=<urlencoded key>` (`data.py:173-275`)

- Instrument key = `_get_instrument_key(symbol, exchange)`: `get_token(symbol, exchange)`; if missing and exchange in `NSE/BSE/MCX` → retry with `f"{exchange}_INDEX"`; if missing and exchange ends with `_INDEX` → retry with base exchange; else `ValueError` (`data.py:132-164`).
- Reversed-parameter guard: if `symbol` is in the known-exchange list and `exchange` is not, swap them (`:188-211`).
- GLOBAL_INDICATOR keys (`instrument_key.startswith("GLOBAL_INDICATOR|")`) short-circuit to §3.12 (`:220-225`).
- URL-encode via `urllib.parse.quote` (`:218`).
- Response `data` is a dict keyed `"<EXCHANGE>:<TRADING_SYMBOL>"`; the entry is found by `value["instrument_token"] == instrument_key` (`:252-261`).
- Mapping `_quote_from_full_v3` (`:277-304`):

| OpenAlgo key | Upstox field |
| --- | --- |
| `ask` | `depth.sell[0].price` |
| `bid` | `depth.buy[0].price` |
| `high` / `low` / `open` | `ohlc.high` / `ohlc.low` / `ohlc.open` |
| `ltp` | `last_price` |
| `prev_close` | `prev_close_price` |
| `volume` | top-level `volume` (int) |
| `oi` | `oi` (int) |

Errors: `status != "success"` → raise `"API Error - Code: {errors[0].errorCode}, Message: {errors[0].message}"` (`:232-244`).

### 3.12 Indicator LTP — `GET https://api.upstox.com/v2/market-quote/ltp?instrument_key=<urlencoded>` (`data.py:306-353`)

Only for `GLOBAL_INDICATOR|...` keys (USDINR, BRENTOIL, WTIOIL are "LTP-only on Upstox — OHLC and full-quote endpoints return UDAPI100500", `:220-223`). Paced with `apply_rate_limit("standard")` (`:318`). Upstox bug: outer key is `"GLOBAL_INDICATOR:null"`, so match on inner `instrument_token` (`:335-341`). Returns `{"ask":0,"bid":0,"high":0,"low":0,"ltp":<last_price>,"open":0,"prev_close":0,"volume":0,"oi":0}` (`:343-353`).

### 3.13 Multi-quotes (`data.py:355-538`)

- `FULL_QUOTE_BATCH_SIZE = 500` ("/v3/market-quote/quotes accepts at most 500 instrument keys per call (UDAPI100042)", `:30-31`).
- Keys comma-joined then URL-encoded: `instrument_key=` + `quote(",".join(chunk))` (`:492-494`).
- GLOBAL_INDICATOR keys are split out and fetched one by one via §3.12 (`:451-472`).
- Output: list of `{"symbol","exchange","data": <§3.11 dict>}` or `{"symbol","exchange","error": "<msg>"}`; ordering = skipped + indicator + resolved (`:510-538`).
- No inter-batch sleep (removed; pacing handled by the shared limiter, `:368-375`).

### 3.14 Depth — same `GET /v3/market-quote/quotes?instrument_key=` (`data.py:1034-1119`)

Output:

```json
{
  "asks": [{"price": depth.sell[i].price, "quantity": depth.sell[i].quantity}, ...],
  "bids": [{"price": depth.buy[i].price,  "quantity": depth.buy[i].quantity},  ...],
  "high": ohlc.high, "low": ohlc.low, "open": ohlc.open,
  "ltp": last_price,
  "ltq": last_quantity,            // undocumented; defaults 0
  "oi": oi,
  "prev_close": prev_close_price,  // NOT ohlc.close
  "totalbuyqty": total_buy_quantity,
  "totalsellqty": total_sell_quantity,
  "volume": volume
}
```
(`:1086-1112`). Alias `get_market_depth = get_depth` (`:1119`).

### 3.15 History — v3 only (`data.py:540-1032`); details in §6

- Intraday: `GET https://api.upstox.com/v3/historical-candle/intraday/{key}/{unit}/{interval}` (`:716-718`).
- Historical: `GET https://api.upstox.com/v3/historical-candle/{key}/{unit}/{interval}/{to_date}/{from_date}` — **to_date first** (`:749-750`).
- Response consumed: `data.candles` (`:725`, `:757`).

### 3.16 GTT (v3) (`gtt_api.py`, `gtt_data.py`)

| Op | Method/URL | Body | Cite |
| --- | --- | --- | --- |
| Place | `POST https://api.upstox.com/v3/order/gtt/place` | `{"type":"SINGLE"\|"MULTIPLE","quantity":int,"product":"D"\|"I","instrument_token":"<key>","transaction_type":"BUY"\|"SELL","rules":[{"strategy":"ENTRY"\|"TARGET"\|"STOPLOSS","trigger_type":"BELOW"\|"ABOVE"\|"IMMEDIATE","trigger_price":float,"market_protection":int?}]}` | `gtt_api.py:207`, `gtt_data.py:228-237`, `:311-318` |
| Modify | `PUT https://api.upstox.com/v3/order/gtt/modify` | `{"type","quantity","gtt_order_id","rules"}` (no `market_protection`, no instrument/product/side) | `gtt_api.py:254`, `gtt_data.py:331-340` |
| Cancel | `DELETE https://api.upstox.com/v3/order/gtt/cancel` **with JSON body** `{"gtt_order_id": "<id>"}` | | `gtt_api.py:284-294` |
| Book | `GET https://api.upstox.com/v3/order/gtt` | — | `gtt_api.py:326` |

Response id: `data.gtt_order_ids[0]` or `data.gtt_order_id` (`gtt_api.py:115-125`). All GTT calls use the **order** rate category (`gtt_api.py:54-60`). Semantics: SINGLE = one ENTRY rule, direction `BELOW` if trigger < LTP, `ABOVE` if >, `IMMEDIATE` if equal, fallback to field semantics (`gtt_data.py:173-196`); OCO → `MULTIPLE` with ENTRY(IMMEDIATE @ LTP) + TARGET + STOPLOSS, entry side is the inverse of OpenAlgo `action` (`:251-277`); MARKET child → `market_protection` from MPP slab (`:199-225`, `mpp_slab.py:17-30`) clamped 1..25. Book filter: keep GTT if any rule status in `{"SCHEDULED","PENDING","OPEN","INACTIVE"}` (`gtt_data.py:93`); timestamps `created_at`/`expires_at` are epoch integers (observed **microseconds**), normalised by magnitude to ISO `%Y-%m-%dT%H:%M:%SZ` (`:95-159`); book `exchange` is an Upstox segment (`NSE_EQ`) reversed via `UpstoxExchangeMapper.get_openalgo_exchange` (`:382-383`).

### 3.17 Shared REST wrapper behaviour

`order_api.get_api_response(endpoint, auth, method, payload, base_url)` (`order_api.py:76-167`): category = `"order"` if `base_url == UPSTOX_HFT_BASE_URL` or path contains any of `/order/place`, `/order/modify`, `/order/cancel`, `/order/multi`, `/order/exit`, `/order/gtt`; else `"standard"` (`:50-73`). On HTTP 429 (`is_rate_limited`), **only `standard`** calls retry up to `MAX_RETRIES` with `retry_delay_from_headers`; mutations surface the error (`:125-158`). Error body returned as parsed JSON if possible (`:161-164`).

`data.get_api_response` (`data.py:34-101`): always `standard`; detects 429 **or** body `errors[].error_code|errorCode == "UDAPI10005"`; retries up to 3 with `Retry-After` or 1,2,4s backoff (`:71-96`).

---

## 4. Field mappings

### 4.1 Exchange — OpenAlgo → Upstox segment

Two tables exist; which one applies depends on the path.

**(a) Master contract (authoritative for `brexchange` stored in DB)** — Upstox `segment` → OpenAlgo `exchange` (`master_contract_db.py:133-148`):

| Upstox `segment` | OpenAlgo `exchange` |
| --- | --- |
| `NSE_EQ` | `NSE` |
| `NSE_FO` | `NFO` |
| `NCD_FO` | `CDS` |
| `NSE_INDEX` | `NSE_INDEX` |
| `BSE_INDEX` | `BSE_INDEX` |
| `BSE_EQ` | `BSE` |
| `BSE_FO` | `BFO` |
| `BCD_FO` | `BCD` |
| `MCX_FO` | `MCX` |
| `GLOBAL_INDEX` | `GLOBAL_INDEX` |
| `GLOBAL_INDICATOR` | `GLOBAL_INDEX` |
| `NSE_COM` | **dropped** (`:126`) |
| anything else | `NaN`/NULL exchange (pandas `.map`) |

`brexchange` = the original `segment` string (`:149`, `:181`).

**(b) Streaming / GTT mapper** `UpstoxExchangeMapper.EXCHANGE_TYPES` (`upstox_mapping.py:15-34`): `NSE→NSE_EQ`, `NFO→NSE_FO`, `NSE_INDEX→NSE_INDEX`, `CDS→NSE_CD`, `BSE→BSE_EQ`, `BFO→BSE_FO`, `BSE_INDEX→BSE_INDEX`, `MCX→MCX_FO`, plus identity for `NSE_EQ/NSE_FO/NSE_CD/BSE_EQ/BSE_FO/MCX_FO`; unknown/None → `NSE_EQ` with warning (`:49-76`). Reverse `REVERSE_EXCHANGE_TYPES` (`:38-47`): `NSE_EQ→NSE`, `NSE_FO→NFO`, `NSE_CD→CDS`, `BSE_EQ→BSE`, `BSE_FO→BFO`, `MCX_FO→MCX`, `NSE_INDEX→NSE_INDEX`, `BSE_INDEX→BSE_INDEX`; default `NSE` (`:89-91`).

**Verification note:** table (b) says `CDS→NSE_CD`, but the master contract stores `NCD_FO` as `brexchange` for CDS rows, and the WebSocket adapter builds the instrument key from `brexchange` (`upstox_adapter.py:525-531`), so on the wire CDS keys are `NCD_FO|<token>`, not `NSE_CD|...`. `BCD`/`GLOBAL_INDEX` have no entry in (b). `get_exchange_type` (forward) has no caller in `broker/upstox/`; only `get_openalgo_exchange` is used (by `gtt_data.py:383`). A Rust port should treat **the DB `brexchange` (table a)** as the source for instrument keys and table (b) only for reversing GTT-book exchanges.

### 4.2 Product

Forward `map_product_type` (`transform_data.py:101-113`): `CNC→D`, `NRML→D`, `MIS→I`; unknown → `I` (warning).

Reverse `reverse_map_product_type(exchange, product)` (`transform_data.py:116-142`): `I→MIS`; `D→` by exchange: `NSE→CNC`, `BSE→CNC`, `NFO→NRML`, `BFO→NRML`, `MCX→NRML`, `CDS→NRML`; other exchange → `None`. (BCD is **not** in this table → `None`.)

Inline reverse in `map_order_data` (`order_data.py:83-92`): `(NSE|BSE) & D → CNC`; `I → MIS`; `(NFO|MCX|BFO|CDS) & D → NRML`. Holdings: `D → CNC` else warning, unchanged (`order_data.py:311-317`). Order-update WS: `{"D":"CNC","I":"MIS"}` (`upstox_order_adapter.py:49`).

### 4.3 Price type / order type

`map_order_type` (`transform_data.py:90-98`): identity for `MARKET`, `LIMIT`, `SL`, `SL-M`; unknown → `MARKET`. Reverse: `order_type` passed through as `pricetype` (`order_data.py:164`).

### 4.4 Action, validity, AMO, tag, disclosed qty

`transaction_type = data["action"].upper()` (`transform_data.py:30`); `validity = "DAY"` always (`:25`, `:48`); `is_amo = "false"` (`:33`); `tag = "openalgo"` (`:27`); `disclosed_quantity = data.get("disclosed_quantity","0")` (`:31`).

### 4.5 Order status normalisation (`order_data.py:14-48`)

`status = str(raw).strip().upper().replace("_"," ")` then:

| Result | Upstox statuses (uppercased) |
| --- | --- |
| `complete` | `COMPLETE` |
| `rejected` | `REJECTED` |
| `cancelled` | `CANCELLED`, `CANCELED`, `CANCELLED AFTER MARKET ORDER` |
| `open` | `OPEN`, `OPEN PENDING`, `TRIGGER PENDING`, `VALIDATION PENDING`, `MODIFY PENDING`, `MODIFY VALIDATION PENDING`, `CANCEL PENDING`, `MODIFIED`, `NOT MODIFIED`, `NOT CANCELLED`, `PUT ORDER REQ RECEIVED`, `AFTER MARKET ORDER REQ RECEIVED`, `MODIFY AFTER MARKET ORDER REQ RECEIVED` |
| else | `status.lower()` passthrough |

(Upstox raw statuses are lowercase on the wire: cancel-all matches `"open"`, `"trigger pending"` literally, `order_api.py:630`.)

Order-update WebSocket `_STATUS_MAP` (`upstox_order_adapter.py:36-46`) keeps `"trigger pending"` as its own status: `complete→complete`, `cancelled→cancelled`, `rejected→rejected`, `open→open`, `trigger pending→trigger pending`, `put order req received→open`, `modified→open`, `modify pending→open`, `cancel pending→open`; unknown → passthrough lowercase, empty → `open`.

### 4.6 Instrument token / symbol resolution

- DB column `SymToken.token` holds the Upstox **instrument_key**, e.g. `NSE_EQ|INE848E01016` (`master_contract_db.py:167`; `upstox_order_adapter.py:117-119`; `gtt_api.py:150-151`). Equities use ISIN after the `|`; derivatives/indices use whatever Upstox puts in `instrument_key` (the plugin never parses it; see §5).
- `get_token(symbol, exchange)` → `SymToken.token` where `symbol==OpenAlgo symbol and exchange==OpenAlgo exchange` (`token_db_enhanced.py:988-997`).
- `get_symbol(token, exchange)` → OpenAlgo `symbol` where `token==<instrument_token> and exchange==<exchange>` (`:1003-1012`). Order book / positions use `order["instrument_token"]` + `order["exchange"]` for this lookup (`order_data.py:74-78`), so the Upstox v2 `exchange` field must equal the OpenAlgo exchange code stored in DB (`NSE`, `NFO`, ...).
- `get_br_symbol(symbol, exchange)` → `brsymbol` (`:1018-1027`); `get_oa_symbol(brsymbol, exchange)` → `symbol` (`:1033-1042`); `get_brexchange(symbol, exchange)` → `brexchange` (`:1048-1057`).
- Smart-order position match: `position["tradingsymbol"] == br_symbol and position["exchange"] == exchange and position["product"] == product` (Upstox product code `D`/`I`) (`order_api.py:244-258`, `:432`).

### 4.7 Upstox → OpenAlgo output shapes

Order book item (`order_data.py:157-169`):
```json
{"symbol": tradingsymbol(after DB rewrite), "exchange": exchange, "action": transaction_type, "quantity": quantity, "price": price, "trigger_price": trigger_price, "pricetype": order_type, "product": product(after rewrite), "orderid": order_id, "order_status": normalize(status), "timestamp": order_timestamp}
```
Stats (`:101-139`): `total_buy_orders`, `total_sell_orders`, `total_completed_orders`, `total_open_orders`, `total_rejected_orders`.

Trade book item (`:183-193`): `symbol, exchange, product, action←transaction_type, quantity, average_price, trade_value = quantity*average_price, orderid←order_id, timestamp←order_timestamp`.

Position item (`:238-246`): `symbol, exchange, product, quantity, average_price, pnl, ltp←last_price`. `average_price` fallback when null/0 (`:213-236`): qty>0 → `buy_price` then `day_buy_price`; qty<0 → `sell_price` then `day_sell_price`; qty==0 → 0.0.

Holding item (`:272-284`): `symbol, exchange, quantity, product, average_price, ltp←last_price, pnl (round 2), pnlpercent = round((last-avg)/avg*100, 2)` (0.0 if avg==0). Portfolio stats (`:322-335`): `totalholdingvalue = Σ last_price*quantity`, `totalinvvalue = Σ average_price*quantity`, `totalprofitandloss = Σ pnl`, `totalpnlpercentage = pnl/inv*100` (0 if inv==0).

Close-all order payload (`order_api.py:506-514`): `{"apikey","strategy":"Squareoff","symbol":get_symbol(instrument_token, exchange),"action": SELL if qty>0 else BUY,"exchange": position.exchange,"pricetype":"MARKET","product": reverse_map_product_type(exchange, product),"quantity": str(abs(qty))}`; positions with qty 0 skipped (`:493-497`).

---

## 5. Master contract (`master_contract_db.py`)

### 5.1 Download

- URL: `https://assets.upstox.com/market-quote/instruments/exchange/complete.json.gz` (`:341`); `requests.get(url, timeout=10)` (`:89`); written to `tmp/temp_upstox.json.gz`, gunzipped to `tmp/upstox.json` (`:342-343`, `:84-95`), both deleted afterwards (`:325-336`). Not rate-limited ("static file on a different host", `rate_limiter.py:56-58`).
- Format: a single gzip'd **JSON array** of instrument objects (`pd.read_json(path)`, `:123`).

### 5.2 Columns read (Upstox JSON keys)

`segment`, `instrument_key`, `trading_symbol`, `name`, `expiry`, `strike_price`, `lot_size`, `instrument_type`, `tick_size` (`:126`, `:151`, `:153-164`). Column rename to DB (`:165-177`):

| Upstox key | DB column |
| --- | --- |
| `instrument_key` | `token` |
| `trading_symbol` | `symbol` (then rewritten, see 5.4) and copied to `brsymbol` (`:179`) |
| `name` | `name` |
| `expiry` | `expiry` |
| `strike_price` | `strike` |
| `lot_size` | `lotsize` |
| `instrument_type` | `instrumenttype` |
| `segment` (mapped) | `exchange` |
| `segment` (raw) | `brexchange` (`:181`) |
| `tick_size` | `tick_size` |

DB schema `SymToken` (`:28-44`): `symbol`(idx), `brsymbol`(idx), `name`, `exchange`(idx), `brexchange`(idx), `token`(idx), `expiry` (String), `strike` (Float), `lotsize` (Integer), `instrumenttype`, `tick_size` (Float); composite index `(symbol, exchange)`.

### 5.3 Filters / transforms

- Drop rows with `segment == "NSE_COM"` (`:126`).
- `exchange = exchange_map[segment]` (table §4.1a) (`:150`).
- `expiry`: Upstox gives epoch **milliseconds**; converted with `pd.to_datetime(expiry, unit="ms").dt.strftime("%d-%b-%y").str.upper()` → e.g. `26-DEC-24`; NaN expiry → NaT → NaN (`:151`).
- `tick_size = to_numeric(tick_size)/100` — Upstox ships **paise** (NSE_EQ 1/5/10, NSE_FO/BSE_FO 5, MCX_FO 50, BCD_FO/NCD_FO 0.25); indices arrive `null` → NaN (`:311-320`).
- `instrumenttype` is stored **as Upstox gives it** (`EQ`, `FUT`, `CE`, `PE`, `INDEX`, ...), no remapping (`:161`, `:173`).
- Insert: table is wiped (`delete_symtoken_table`, `:352`) then bulk-inserted, skipping rows whose `token` already exists in the table (`:63-67`).

### 5.4 Symbol construction (`reformat_symbol`, `:98-115`)

Operates on the Upstox `trading_symbol` split on single spaces:

| `instrumenttype` | Condition | Result |
| --- | --- | --- |
| `FUT` | exactly 5 parts `[p0 p1 p2 p3 p4]` | `p0 + p2 + p3 + p4 + p1` |
| `CE` / `PE` | exactly 6 parts `[p0 p1 p2 p3 p4 p5]` | `p0 + p3 + p4 + p5 + p1 + p2` |
| otherwise (or wrong part count) | — | unchanged `trading_symbol` |

Reading the positional assumption: for FUT the code expects `NAME FUT DD MMM YY` → `NAME` `DD` `MMM` `YY` `FUT` (e.g. `NIFTY 26 DEC 24` style tokens produce `NIFTY26DEC24FUT`); for options `NAME STRIKE CE|PE DD MMM YY` → `NAME DD MMM YY STRIKE CE|PE` (e.g. `NIFTY24000CE` form `NIFTY26DEC2424000CE`). The strike token is concatenated **verbatim** (a decimal strike such as `87.5` stays `87.5` in the symbol; no reformatting). Equity symbols are unchanged (Upstox equity `trading_symbol` has no `-EQ` suffix handling in this file). `brsymbol` always keeps the original Upstox `trading_symbol` (`:179-180`).

### 5.5 Index renames

NSE indices (applied to **all rows**, plain `str.replace` on the whole symbol column, `:184-248`):

| Upstox `trading_symbol` | OpenAlgo symbol |
| --- | --- |
| `NIFTY 50` | `NIFTY` |
| `NIFTY NEXT 50` | `NIFTYNXT50` |
| `NIFTY FIN SERVICE` | `FINNIFTY` |
| `NIFTY BANK` | `BANKNIFTY` |
| `NIFTY MID SELECT` | `MIDCPNIFTY` |
| `INDIA VIX` | `INDIAVIX` |
| `HANGSENG BEES NAV` | `HANGSENGBEESNAV` |
| `NIFTY 100` / `NIFTY 200` / `NIFTY 500` | `NIFTY100` / `NIFTY200` / `NIFTY500` |
| `NIFTY ALPHA 50` | `NIFTYALPHA50` |
| `NIFTY AUTO` | `NIFTYAUTO` |
| `NIFTY COMMODITIES` | `NIFTYCOMMODITIES` |
| `NIFTY CONSUMPTION` | `NIFTYCONSUMPTION` |
| `NIFTY CPSE` | `NIFTYCPSE` |
| `NIFTY DIV OPPS 50` | `NIFTYDIVOPPS50` |
| `NIFTY ENERGY` | `NIFTYENERGY` |
| `NIFTY FMCG` | `NIFTYFMCG` |
| `NIFTY GROWSECT 15` | `NIFTYGROWSECT15` |
| `NIFTY INFRA` | `NIFTYINFRA` |
| `NIFTY IT` | `NIFTYIT` |
| `NIFTY MEDIA` | `NIFTYMEDIA` |
| `NIFTY METAL` | `NIFTYMETAL` |
| `NIFTY MNC` | `NIFTYMNC` |
| `NIFTY PHARMA` | `NIFTYPHARMA` |
| `NIFTY PSE` | `NIFTYPSE` |
| `NIFTY PSU BANK` | `NIFTYPSUBANK` |
| `NIFTY PVT BANK` | `NIFTYPVTBANK` |
| `NIFTY REALTY` | `NIFTYREALTY` |
| `NIFTY SERV SECTOR` | `NIFTYSERVSECTOR` |
| `NIFTY MID LIQ 15` | `NIFTYMIDLIQ15` |
| `NIFTY MIDCAP 50` / `100` / `150` | `NIFTYMIDCAP50` / `NIFTYMIDCAP100` / `NIFTYMIDCAP150` |
| `NIFTY MIDSML 400` | `NIFTYMIDSML400` |
| `NIFTY SMLCAP 50` / `100` / `250` | `NIFTYSMLCAP50` / `NIFTYSMLCAP100` / `NIFTYSMLCAP250` |
| `NIFTY100 EQL WGT` | `NIFTY100EQLWGT` |
| `NIFTY100 LIQ 15` | `NIFTY100LIQ15` |
| `NIFTY100 LOWVOL30` | `NIFTY100LOWVOL30` |
| `NIFTY100 QUALTY30` | `NIFTY100QUALTY30` |
| `NIFTY200 QUALTY30` | `NIFTY200QUALTY30` |
| `NIFTY50 DIV POINT` | `NIFTY50DIVPOINT` |
| `NIFTY50 EQL WGT` | `NIFTY50EQLWGT` |
| `NIFTY50 PR 1X INV` | `NIFTY50PR1XINV` |
| `NIFTY50 PR 2X LEV` | `NIFTY50PR2XLEV` |
| `NIFTY50 TR 1X INV` | `NIFTY50TR1XINV` |
| `NIFTY50 TR 2X LEV` | `NIFTY50TR2XLEV` |
| `NIFTY50 VALUE 20` | `NIFTY50VALUE20` |
| `NIFTY GS 10YR` | `NIFTYGS10YR` |
| `NIFTY GS 10YR CLN` | `NIFTYGS10YRCLN` |
| `NIFTY GS 11 15YR` | `NIFTYGS1115YR` |
| `NIFTY GS 15YRPLUS` | `NIFTYGS15YRPLUS` |
| `NIFTY GS 4 8YR` | `NIFTYGS48YR` |
| `NIFTY GS 8 13YR` | `NIFTYGS813YR` |
| `NIFTY GS COMPSITE` | `NIFTYGSCOMPSITE` |

BSE indices (applied **only where `exchange == "BSE_INDEX"`**, `:250-289`):

| Upstox | OpenAlgo |
| --- | --- |
| `SNSX50` | `SENSEX50` |
| `SNXT50` | `BSESENSEXNEXT50` |
| `MID150` | `BSE150MIDCAPINDEX` |
| `LMI250` | `BSE250LARGEMIDCAPINDEX` |
| `MSL400` | `BSE400MIDSMALLCAPINDEX` |
| `AUTO` | `BSEAUTO` |
| `BSE CG` | `BSECAPITALGOODS` |
| `CARBON` | `BSECARBONEX` |
| `BSE CD` | `BSECONSUMERDURABLES` |
| `CPSE` | `BSECPSE` |
| `DOL100` / `DOL200` / `DOL30` | `BSEDOLLEX100` / `BSEDOLLEX200` / `BSEDOLLEX30` |
| `ENERGY` | `BSEENERGY` |
| `BSEFMC` | `BSEFASTMOVINGCONSUMERGOODS` |
| `FINSER` | `BSEFINANCIALSERVICES` |
| `GREENX` | `BSEGREENEX` |
| `BSE HC` | `BSEHEALTHCARE` |
| `INFRA` | `BSEINDIAINFRASTRUCTUREINDEX` |
| `INDSTR` | `BSEINDUSTRIALS` |
| `BSE IT` | `BSEINFORMATIONTECHNOLOGY` |
| `BSEIPO` | `BSEIPO` |
| `LRGCAP` | `BSELARGECAP` |
| `METAL` | `BSEMETAL` |
| `MIDCAP` | `BSEMIDCAP` |
| `MIDSEL` | `BSEMIDCAPSELECTINDEX` |
| `OILGAS` | `BSEOIL&GAS` |
| `POWER` | `BSEPOWER` |
| `BSEPSU` | `BSEPSU` |
| `REALTY` | `BSEREALTY` |
| `SMLCAP` | `BSESMALLCAP` |
| `SMLSEL` | `BSESMALLCAPSELECTINDEX` |
| `SMEIPO` | `BSESMEIPO` |
| `TECK` | `BSETECK` |
| `TELCOM` | `BSETELECOM` |

`SENSEX`, `BANKEX` are **not** renamed (not in the table; they pass through as Upstox names them).

GLOBAL_INDEX (applied only where `exchange == "GLOBAL_INDEX"`, `:291-309`):

| Upstox | OpenAlgo |
| --- | --- |
| `^HSI` | `HANGSENG` |
| `^DJI` | `DOWJONES` |
| `^FTSE` | `UK100` |
| `^GSPC` | `US500` |
| `^GDAXI` | `GERMANY40` |
| `^FCHI` | `FRANCE40` |
| `^N225` | `JAPAN225` |
| `IXIX` | `US100` |
| `GIFT NIFTY` | `GIFTNIFTY` |
| `DOW FUTURES` | `US30` |
| `BZUSD` | `BRENTOIL` |
| `CLUSD` | `WTIOIL` |

(`USDINR` is referenced as a GLOBAL_INDICATOR at `data.py:220` but has no rename.)

### 5.6 Search

`search_symbols(symbol, exchange)`: `symbol LIKE %symbol% AND exchange == exchange` (`:364-367`).

---

## 6. History API (`data.py:540-1032`)

### 6.1 Timeframe map (`data.py:111-130`) → `(unit, interval)`

| key | unit | interval |
| --- | --- | --- |
| `1m` `2m` `3m` `5m` `10m` `15m` `30m` `60m` | `minutes` | `1 2 3 5 10 15 30 60` |
| `1h` `2h` `3h` `4h` | `hours` | `1 2 3 4` |
| `D` | `days` | `1` |
| `W` | `weeks` | `1` |
| `M` | `months` | `1` |

Unknown → `Exception("Invalid interval: ...")` (`:560-562`). Only Upstox supports W/M among the five majors (`skill:history-data.md:146,151`).

### 6.2 Chunk limits in calendar days, keyed `(unit, interval)` (`data.py:574-603`)

| (unit, interval) | days |
| --- | --- |
| `minutes` 1,2,3,5,10,15 | 30 |
| `minutes` 30, 60 | 90 |
| `hours` 1,2,3,4 | 90 |
| `days` 1 | 3650 |
| `weeks` 1 / `months` 1 | 7300 |
| unknown | 30 (warning) |

Loop (`:611-648`): `current_start = from_date`; `current_end = min(current_start + (chunk_days-1) days, to_date)`; fetch; `current_start = current_end + 1 day`; failed chunks are logged and skipped. After concat: `drop_duplicates(subset=["timestamp"])`, sort, reset index (`:660-667`). Empty → DataFrame with columns `["close","high","low","open","timestamp","volume","oi"]` (`:655-657`).

### 6.3 Endpoint selection per chunk (`_fetch_chunk_data`, `:677-992`)

Dates formatted `%Y-%m-%d` (`:707-708`).

1. **Intraday** — only if `unit in ["minutes","hours"]` **and** `end_date.date() == today` (`:714`): `GET /v3/historical-candle/intraday/{quote(key)}/{unit}/{interval}`; candles filtered to `[start, end+1day)` by `_filter_candles_by_date` (`:733-736`, `:994-1032`).
2. **Historical** — if no intraday candles **or** `start_date.date() < today` (`:746`): `GET /v3/historical-candle/{quote(key)}/{unit}/{interval}/{to_date}/{from_date}` — **path order is `{to_date}/{from_date}`** (`:749-750`; `skill:history-data.md:119-120`).
3. **Today's daily candle** — only when `unit == "days" and interval == "D"` and today lies within the chunk (`:779-784`): if no candle dated today is present (`:786-802`), call `get_quotes(symbol, exchange)` and append a synthetic candle unless its O/H/L/C/V equal the last historical candle ("stale", `:811-849`):
   ```
   [ datetime.combine(today, 00:00, tzinfo=IST).isoformat(),   # e.g. "2026-10-03T00:00:00+05:30"
     quotes.open|ltp, quotes.high|ltp, quotes.low|ltp, quotes.ltp, quotes.volume, quotes.oi ]
   ```
   (`:860-871`). `IST = timezone(timedelta(hours=5, minutes=30))` (`:28`).

### 6.4 Candle array layout

Upstox returns each candle as an array; the plugin loads it as columns `["timestamp","open","high","low","close","volume","oi"]` (`:892-894`) and then **reorders output to `["close","high","low","open","timestamp","volume","oi"]`** (`:984`). Raw timestamps are ISO-8601 with offset, e.g. `'2024-12-09T15:29:00+05:30'` (`:902`); numeric timestamps are treated as milliseconds (`:912-913`, `:1019-1022`).

### 6.5 Timestamp conversion (`:901-975`)

- String → `pd.to_datetime(ts)` (tz-aware); numeric → `pd.to_datetime(ts, unit="ms")`; NaT rows dropped.
- If `interval == "D"`: timestamp is replaced by its calendar **date** then re-parsed as a **naive** midnight (`:925-931`) — i.e. the `+05:30` offset is discarded and the daily epoch becomes midnight-UTC-of-that-IST-date (pandas treats naive as UTC in `.timestamp()`). This is the "daily normalisation": the day is preserved, the time-of-day is 00:00 with no offset. (The skill note `skill:history-data.md:30` describes Upstox as "using `timedelta(hours=5, minutes=30)`"; in the current code that IST offset is only applied when **stamping the synthetic today candle**, `:860-863`, so daily rows from Upstox and the synthetic row both normalise to the same calendar date.) `W`/`M` are **not** date-normalised (only `interval == "D"`).
- Final `timestamp = int(x.timestamp())` epoch seconds (`:935-939`). Numeric columns coerced, NaN→0 (`:978-981`).

### 6.6 Intraday date filter (`_filter_candles_by_date`, `:994-1032`)

`start_ts = start.timestamp()*1000`, `end_ts = (end + 1 day).timestamp()*1000`; keep `start_ts <= candle_ts < end_ts`; **mutates `candle[0]` to the millisecond float** (`:1027-1030`), which is why the converter must accept numeric ms.

---

## 7. WebSocket streaming

### 7.1 Market-data authorize + connect (`upstox_client.py`)

1. `GET https://api.upstox.com/v3/feed/market-data-feed/authorize` with `Accept: application/json`, `Authorization: Bearer <token>`, timeout 10 s (`:36-40`, `:673-681`). Response → `data.authorized_redirect_uri` (a `wss://...` URL with AWS SigV4 query params; the query is stripped before logging, `:682-693`). Fetched **fresh on every (re)connect** (`:313-318`), after re-reading the token from DB (`:650-671`).
2. `websocket.WebSocketApp(ws_url, on_open, on_message, on_error, on_close, on_ping, on_pong)`; `run_forever(sslopt={"cert_reqs": ssl.CERT_NONE}, ping_interval=30, ping_timeout=10)` (`:197-205`, `:224-228`).
3. Reconnect: `max_attempts=50`, `base_delay=2`, `max_delay=30`; delay = `min(2 * 2**(attempt-1), 30)` (`:143`, `:738-740`), slept in 0.2 s slices (`:49`, `:299-302`); attempt counter resets on each successful open (`:489`). After `NEVER_CONNECTED_ALERT_AFTER = 3` consecutive failures with no handshake ever, emit the "over the Upstox per-user limit (2 Standard / 5 Plus)" error (`:86`, `:261-274`).
4. Health check thread every `HEALTH_CHECK_INTERVAL = 30` s; if no message/ping/pong for `DATA_TIMEOUT = 90` s → `_force_reconnect()` (close socket) (`:43-44`, `:559-595`). Pongs **and** server pings refresh `_last_message_time` (`:506-524`); Upstox sends **no application-level heartbeat** (`:509`).
5. Server-side subscriptions are dropped on close; the adapter replays everything on `on_connect` (`:492-497`; `upstox_adapter.py:561-599`).

### 7.2 Subscribe / unsubscribe message (`upstox_client.py:742-752`)

```json
{"guid": "<uuid4 hex, first 20 chars>", "method": "sub" | "unsub", "data": {"instrumentKeys": ["NSE_EQ|INE848E01016", "..."], "mode": "ltpc" | "full" | "option_greeks" | "full_d30"}}
```
- `mode` is included only for `sub` (`:750-751`).
- Sent as a **BINARY frame** containing the UTF-8 JSON (`ws.send(json.dumps(msg).encode("utf-8"), opcode=ABNF.OPCODE_BINARY)`, `:356-360`, `:436`).
- Per-mode key caps enforced client-side (`MODE_KEY_LIMITS`, `:60-65`): `ltpc 5000`, `option_greeks 3000`, `full 2000`, `full_d30 50`; default 2000. Combined caps once >1 mode is live (`:75-81`): `ltpc 2000`, `option_greeks 2000`, `full 1500`, `full_d30 1500`; budget = min over live modes. Keys over cap are dropped with an ERROR log (`:369-427`). Keys within cap are chunked to at most `limit` per message (`:351-355`).
- Text (non-binary) frames are JSON; `{"status":"failed","error":...,"method":...}` → error callback (`:608-620`).
- Mode selection (`upstox_adapter.py:533-552`): OpenAlgo `mode 1 → "ltpc"`, `2 → "full"`, `3 → "full"`; `full_d30` is **never** emitted (Plus-only, 50-key cap, and the adapter publishes 5 levels anyway); depth requests >5 are served as 5 with a one-time warning (`:186-192`).

### 7.3 Protobuf schema — `MarketDataFeedV3.proto` (verbatim field numbers)

`syntax = "proto3"; package com.upstox.marketdatafeederv3udapi.rpc.proto; import "google/protobuf/wrappers.proto";` (`:1-3`)

| Message | Fields (`name = number : type`) | Cite |
| --- | --- | --- |
| `LTPC` | `ltp=1 double`, `ltt=2 int64`, `ltq=3 int64`, `cp=4 double`, `iep=5 google.protobuf.DoubleValue` (indicative equilibrium price; pre-open/closing auction only) | `:5-11` |
| `MarketLevel` | `bidAskQuote=1 repeated Quote` | `:13-15` |
| `MarketOHLC` | `ohlc=1 repeated OHLC` | `:17-19` |
| `Quote` | `bidQ=1 int64`, `bidP=2 double`, `askQ=3 int64`, `askP=4 double` | `:21-26` |
| `OptionGreeks` | `delta=1`, `theta=2`, `gamma=3`, `vega=4`, `rho=5` (all double) | `:28-34` |
| `OHLC` | `interval=1 string`, `open=2`, `high=3`, `low=4`, `close=5` (double), `vol=6 int64`, `ts=7 int64` | `:36-44` |
| `enum Type` | `initial_feed=0`, `live_feed=1`, `market_info=2` | `:46-50` |
| `MarketFullFeed` | `ltpc=1 LTPC`, `marketLevel=2 MarketLevel`, `optionGreeks=3 OptionGreeks`, `marketOHLC=4 MarketOHLC`, `atp=5 double`, `vtt=6 int64`, `oi=7 double`, `iv=8 double`, `tbq=9 double`, `tsq=10 double`, `iep=11 double`, `rp=12 double`, `ieq=13 int64`, `iiqTotal=14 int64`, `iiqM=15 int64`, `casEligible=16 bool` | `:52-69` |
| `IndexFullFeed` | `ltpc=1 LTPC`, `marketOHLC=2 MarketOHLC` | `:71-74` |
| `FullFeed` | `oneof FullFeedUnion { marketFF=1 MarketFullFeed; indexFF=2 IndexFullFeed }` | `:77-82` |
| `FirstLevelWithGreeks` | `ltpc=1`, `firstDepth=2 Quote`, `optionGreeks=3`, `vtt=4 int64`, `oi=5 double`, `iv=6 double` | `:84-91` |
| `Feed` | `oneof FeedUnion { ltpc=1 LTPC; fullFeed=2 FullFeed; firstLevelWithGreeks=3 FirstLevelWithGreeks }`, `requestMode=4 RequestMode` | `:93-100` |
| `enum RequestMode` | `ltpc=0`, `full_d5=1`, `option_greeks=2`, `full_d30=3` | `:102-107` |
| `enum MarketStatus` | `PRE_OPEN_START=0`, `PRE_OPEN_END=1`, `NORMAL_OPEN=2`, `NORMAL_CLOSE=3`, `CLOSING_START=4`, `CLOSING_END=5` | `:109-116` |
| `StatusInfo` | `status=1 string`, `updatedTime=2 int64` | `:119-122` |
| `MarketInfo` | `segmentStatus=1 map<string,MarketStatus>`, `casMarketStatus=2 map<string,StatusInfo>`, `preOpenSessionStatus=3 map<string,StatusInfo>` | `:124-128` |
| `FeedResponse` | `type=1 Type`, `feeds=2 map<string,Feed>`, `currentTs=3 int64`, `marketInfo=4 MarketInfo` | `:130-135` |

`MarketDataFeedV3_pb2.py` is the protoc output of exactly this file (Protobuf Python 6.33.5, `:5`); the serialized descriptor in `:28` encodes the same messages/fields. Note the proto's `RequestMode.full_d5` vs the wire mode string `"full"` used in subscribe messages.

### 7.4 Decoding (`upstox_client.py:622-642`)

`FeedResponse.ParseFromString(bytes)` → `MessageToDict(msg)` with **default** options: camelCase keys (`marketOHLC`, `bidAskQuote`, `iiqTotal`), proto3 default scalars omitted, int64 rendered as **strings** (`"vtt"`, `"ltt"`, `"iiqTotal"`), `DoubleValue iep` flattened to its bare value only when present. A Rust port must reproduce: treat absent fields as absent (not 0), parse int64 strings with sign preserved.

### 7.5 Adapter: key construction, matching, topics (`upstox_adapter.py`)

- `SymbolMapper.get_token_from_symbol(symbol, exchange)` → `{"token": SymToken.token, "brexchange": SymToken.brexchange}` (`ws_mapping.py:34-54`).
- `instrument_key = f"{brexchange}|{token.split('|')[-1]}"` (`upstox_adapter.py:525-531`), e.g. `NSE_EQ|INE848E01016`.
- Subscription record keyed `f"{symbol}_{exchange}_{mode}"` (`:201`); subscribe calls are coalesced for `batch_delay = 0.5` s then one `sub` message per mode (`:69`, `:239-315`).
- Incoming `feeds` map key is matched to `sub_info["instrument_key"]` exactly, or by the part after `|` equalling `sub_info["token"]` (`:681-689`).
- ZMQ topic: `f"{exchange}_{symbol}_{LTP|QUOTE|DEPTH}"` for modes 1/2/3 (`:554-558`); published as `send_multipart([topic.encode(), json.dumps(data).encode()])` (`base_adapter.py:388-404`).
- `type == "market_info"` messages update `market_status` by per-map merge (`segmentStatus`, `casMarketStatus`, `preOpenSessionStatus`) (`:633-676`).

### 7.6 Published normalized dicts (no price scaling — proto doubles are already rupees)

Common base: `{"symbol", "exchange", "token"}` (`:740`).

**Mode 1 LTP** (`:864-886`) from `Feed.ltpc`:
```json
{"symbol":"RELIANCE","exchange":"NSE","token":"NSE_EQ|INE002A01018","ltp": float(ltpc.ltp), "ltq": int(ltpc.ltq), "ltt": int(ltpc.ltt), "cp": float(ltpc.cp), "indicative_equilibrium_price": <only if ltpc.iep present>}
```

**Mode 2 QUOTE** (`:888-932`) from `Feed.fullFeed.marketFF|indexFF`:
```json
{"symbol","exchange","token",
 "open": ohlc.open, "high": ohlc.high, "low": ohlc.low, "close": ohlc.close,   // ohlc = entry with interval=="1d", else first
 "ltp": ltpc.ltp, "last_trade_quantity": int(ltpc.ltq),
 "volume": int(ohlc.vol), "average_price": float(atp),
 "total_buy_quantity": int(tbq), "total_sell_quantity": int(tsq),
 "timestamp": int(ohlc.ts) or currentTs,
 /* CAS extras, only when present: */ "indicative_equilibrium_price", "reference_price"(rp), "indicative_equilibrium_quantity"(ieq), "indicative_imbalance_quantity_total"(iiqTotal, signed), "indicative_imbalance_quantity_market"(iiqM), "cas_eligible"(casEligible)}
```
If `fullFeed` is absent the extractor returns `{}` and the LTPC carry-forward (below) applies.

**Mode 3 DEPTH** (`:934-979`, wrapped at `:705-713`): from `marketFF.marketLevel.bidAskQuote[]` → `buy` levels `{price: bidP, quantity: int(bidQ), orders: 0}` where `bidP>0`, sorted desc; `sell` levels `{price: askP, quantity: int(askQ), orders: 0}` where `askP>0`, sorted asc; each padded with `{"price":0.0,"quantity":0,"orders":0}` to 5 and truncated to 5. Published as:
```json
{"symbol","exchange","token","ltp": ltpc.ltp, "timestamp": currentTs, "depth": {"buy": [5 levels], "sell": [5 levels], "timestamp": currentTs}, /* CAS extras beside depth */}
```

**LTPC carry-forward** (`:760-782`): the last LTPC per instrument is cached (`:742-746`); if an extractor returns `{}` the publish is `base + {ltp, ltq, ltt, cp, timestamp: currentTs}` from cache; if `ltp` is 0/missing it is replaced by the cached `ltp`. Cached `iep` is never carried forward.

### 7.7 Order-update WebSocket (`upstox_order_adapter.py`)

- Authorize: `GET https://api.upstox.com/v2/feed/portfolio-stream-feed/authorize?update_types=order` with Bearer → `data.authorized_redirect_uri` (`:25`, `:68-82`); fetched on **every** (re)connect because the embedded code is single-use (`:9-13`, `:59-66`). Fallback: connect directly to `wss://api.upstox.com/v2/feed/portfolio-stream-feed?update_types=order` with header `Authorization: Bearer <token>` (HTTP 302 handshake) (`:29-31`, `:87-97`).
- Transport params come from `BaseOrderUpdateAdapter`: `ping_interval = max(2, ws_ping_interval())`, `ping_timeout = min(10, max(1, ping_interval-1))` (`order_adapter.py:308-313`).
- Messages are JSON text. `normalize` (`:99-148`): ignore if `update_type` not in `(None, "order")`; fields read: `status`, `quantity`, `filled_quantity`, `exchange`, `trading_symbol`|`tradingsymbol`, `instrument_token`|`instrument_key`, `order_id`, `transaction_type`, `price`, `trigger_price`, `order_type`, `product`, `pending_quantity`, `average_price`, `status_message`. Symbol resolved via `get_symbol(instrument_token, exchange)`, falling back to the broker symbol (`:120-127`). Output:
```json
{"orderid","symbol","exchange","action": transaction_type, "quantity": int, "price": float, "trigger_price": float, "pricetype": order_type, "product": D→CNC|I→MIS, "order_status": <_STATUS_MAP>, "filled_quantity": int, "pending_quantity": int(pending_quantity or max(qty-filled,0)), "average_price": float, "rejection_reason": status_message|status if rejected else ""}
```
Example event in docs: `docs/api/websocket-streaming/order-updates.md:78-88`.

---

## 8. Rate limits, batch sizes, sleeps (`rate_limiter.py`)

Published Upstox budgets (three simultaneous rolling windows each) (`:3-10`):

| Category | per sec | per min | per 30 min | Endpoints |
| --- | --- | --- | --- | --- |
| **order** | 10 | 500 | 2000 | Place, Modify, Cancel, Multi Order, GTT (`_ORDER_PATH_FRAGMENTS`, `order_api.py:50-57`; all of `gtt_api.py`, `:60`) |
| **standard** | 50 | 500 | 2000 | holdings, positions, order book, trade book, funds, margin, quotes, depth, history |

Configured headroom (env-overridable, clamped to the published ceiling) (`:270-283`, `:103-131`):

| Limiter | per sec | per min | per 30 min | Env vars (ceiling) |
| --- | --- | --- | --- | --- |
| `ORDER_LIMITER` | **8** | **475** | **1900** | `UPSTOX_ORDER_MAX_PER_SECOND` (50 — SEBI-registered algo tier), `UPSTOX_ORDER_MAX_PER_MINUTE` (500), `UPSTOX_ORDER_MAX_PER_30MIN` (2000) |
| `STANDARD_LIMITER` | **45** | **475** | **1900** | `UPSTOX_MAX_PER_SECOND` (50), `UPSTOX_MAX_PER_MINUTE` (500), `UPSTOX_MAX_PER_30MIN` (2000) |

Algorithm (`SlidingWindowLimiter.reserve`, `:197-232`): purge reservations older than 1800 s; `slot = now`; for each `(cap, span)` in `[(sec,1.0),(min,60.0),(30min,1800.0)]`: if `len(reserved) >= cap` then `slot = max(slot, reserved[-cap] + span)`; append slot; caller sleeps `slot-now` **outside** the lock (`:234-247`). Under the gthread worker only, a wait longer than `max_queue_wait(kind)` raises `BrokerBusyError` instead of booking (`:207`, `:220-231`; `backpressure.py:43` `BROKER_MAX_ORDER_QUEUE_WAIT_SECONDS = 10.0`).

Reactive 429 handling: `MAX_RETRIES = 3`, `BASE_BACKOFF = 1.0` (1, 2, 4 s), `RATE_LIMIT_ERROR_CODE = "UDAPI10005"`; `Retry-After` header honoured (`max(float, 0.05)`); Upstox documents no Retry-After (`:304-364`). Only read endpoints retry.

Deliberately **not** paced (`:44-58`): token exchange (`auth_api.py`), WS authorize calls (`upstox_client.py`, `upstox_order_adapter.py`), master-contract download.

Other limits / sizes:
- Multiquote batch **500** keys (`data.py:30-31`, `:366`), no inter-batch sleep (`:368-375`).
- Margin basket max **20** instruments (`margin_api.py:40-51`).
- Smart-order position cache 1 s, invalidated after each smart order (`order_api.py:197-202`, `:220-234`).
- WS: 2 connections/user (Standard), 5 (Plus); `MAX_WEBSOCKET_CONNECTIONS` default 3 is one too many for Standard (`upstox_adapter.py:36-45`; `base_adapter.py:28`). Subscription caps per §7.2. Batch subscribe delay 0.5 s (`upstox_adapter.py:69`). Reconnect backoff 2→30 s, 50 attempts (`upstox_client.py:143`).
- History: no sleep between chunks (`skill:history-data.md:90-91`).

---

## 9. Quirks and gotchas (all literal, all cited)

1. **Two hosts.** v3 place/modify/cancel go to `https://api-hft.upstox.com`; everything else to `https://api.upstox.com` (`order_api.py:39-43`). GTT v3 uses `api.upstox.com` (`gtt_api.py:51`).
2. **Quotes/LTP/depth are v3 only** (`/v3/market-quote/quotes`), except `GLOBAL_INDICATOR|*` which must use `/v2/market-quote/ltp` and returns only `ltp` (`data.py:220-225`, `:306-353`); its response outer key is the bug `"GLOBAL_INDICATOR:null"`, so always match on inner `instrument_token` (`:335-336`, `:452-453`).
3. **v3 quote dict is keyed `"<EXCHANGE>:<TRADING_SYMBOL>"`**, never by instrument key — match on `value.instrument_token` (`data.py:252-261`, `:485-488`).
4. **`prev_close` must come from `prev_close_price`**, not `ohlc.close` (live session) (`data.py:289-291`, `:1106-1108`).
5. **Price/trigger zeroing by order type** on place (UDAPI1040) (`transform_data.py:15-19`); not on modify.
6. **`is_amo` is the string `"false"`** (`transform_data.py:33`).
7. **Order id shape differs:** v3 place → `data.order_ids` (list); modify/cancel → `data.order_id` (`order_api.py:307-323`).
8. **Error envelope** `errors[].errorCode` (orders/quotes) vs `errors[].error_code` (GTT, rate limit) — both spellings are checked (`order_api.py:298`, `gtt_api.py:104`, `rate_limiter.py:361`).
9. **Cancel-all** filters raw lowercase statuses `"open"` and `"trigger pending"` (`order_api.py:630`), then cancels one by one — there is no multi-cancel call.
10. **Funds**: v3 endpoint with `Api-Version: 3.0`; v2's `available_margin`/`notional_cash` were wrong (`funds.py:33-39`); `pledge_available_to_trade` has no `.total` (`:63-64`); HTTP 423 `UDAPI100072` = outside 5:30 AM–12:00 AM IST → zeros (`:98-115`).
11. **Master contract**: `tick_size` is in **paise** → divide by 100 (`master_contract_db.py:311-320`); `expiry` is epoch **ms** (`:151`); `NSE_COM` dropped (`:126`); CDS segment code is **`NCD_FO`**, BCD is **`BCD_FO`** (`:136`, `:141`), while the streaming mapper says `NSE_CD` (`upstox_mapping.py:20`) — the DB `brexchange` wins for instrument keys (§4.1 note).
12. **Symbol reformat is purely positional** on space-split `trading_symbol` (5 parts for FUT, 6 for CE/PE); anything else is left untouched; strikes (including decimals like `87.5`) are concatenated verbatim (`master_contract_db.py:98-115`).
13. **Index renames**: NSE table is applied to **every** row (safe because names contain spaces); BSE table only to `BSE_INDEX` rows to avoid clobbering equities named `AUTO`, `METAL`, etc. (`:250-252`); GLOBAL_INDEX only to `GLOBAL_INDEX` rows (`:291-293`).
14. **History path order is `{to_date}/{from_date}`** (`data.py:750`); wrong order returns empty data, not an error (`skill:history-data.md:119-120`). End date is made inclusive with `+1 day` in the intraday filter (`data.py:1005`). `W`/`M` supported; chunk 7300 days.
15. **Daily candle normalisation** strips the time and offset (`data.py:925-931`); the synthetic today-candle is stamped IST midnight ISO with offset (`:860-863`) and skipped if identical to the last candle (`:831-849`).
16. **WebSocket subscribe is a binary frame of JSON** (`upstox_client.py:356-360`); modes on the wire are `ltpc`, `full`, `option_greeks`, `full_d30` (proto enum calls the second `full_d5`). OpenAlgo depth (mode 3) is **`full`**, never `full_d30` (Plus-only, 50 keys) (`upstox_adapter.py:533-552`).
17. **Depth published as 5 levels only**, `orders` always 0 (`upstox_adapter.py:954-969`).
18. **Protobuf → dict conventions are load-bearing**: camelCase keys, int64 as strings (sign of `iiqTotal` matters), defaults omitted, `LTPC.iep` wrapper present-only (`upstox_client.py:625-639`).
19. **Market-data authorize URL is single-use and SigV4-signed**; never log the query string (`upstox_client.py:690-693`; `docs/releases/version-2.0.2.2-released.md:37` records the earlier leak of the presigned feed URL including `X-Amz-Signature`).
20. **No app-level heartbeat** from Upstox; liveness = protocol pings/pongs (`upstox_client.py:506-524`).
21. **Order-update feed** authorize code is single-use → refetch every reconnect; direct wss fallback with Bearer header (`upstox_order_adapter.py:9-13`, `:29-31`).
22. **GTT**: no limit price field; MARKET child orders rejected (UDAPI1158) → `market_protection` %; every GTT needs an ENTRY rule (UDAPI1141), so OpenAlgo OCO becomes a bracket that **opens** a position (`gtt_api.py:12-33`); GTT `created_at`/`expires_at` are epoch **microseconds** (`gtt_data.py:95-112`); cancel is a `DELETE` **with a body** (`gtt_api.py:277-294`).
23. **Reversed-arg guard** in `get_quotes` swaps `symbol`/`exchange` if `symbol` looks like an exchange code (`data.py:185-211`).
24. **Index fallback** in `_get_instrument_key`: `NSE→NSE_INDEX`, `BSE→BSE_INDEX`, `MCX→MCX_INDEX`, and `*_INDEX→base` (`data.py:138-157`).
25. **Rate limit categories**: order-book/trade-book/positions/holdings reads are `standard` despite living in `order_api.py` (`order_api.py:45-49`); GTT **book read** is `order` (`gtt_api.py:54-60`).
26. `plugin.json` advertises `GLOBAL_INDEX` (`:8`), while `docs/prompt/symbol-format.md:565` says GLOBAL_INDEX is "Zerodha only" — the Upstox master contract does populate it (`master_contract_db.py:143-147`, `:291-309`).

---

## B. DHAN broker — literal implementation spec (port target: Rust)

Source of truth: `/Users/openalgo/openalgo-desktop/openalgo/broker/dhan/`. Every fact below cites `file:line` relative to that directory unless prefixed otherwise. File aliases used in citations:

| Alias | File |
|---|---|
| `auth` | `api/auth_api.py` |
| `base` | `api/baseurl.py` |
| `order` | `api/order_api.py` |
| `data` | `api/data.py` |
| `funds` | `api/funds.py` |
| `margin` | `api/margin_api.py` |
| `gtt` | `api/gtt_api.py` |
| `tx` | `mapping/transform_data.py` |
| `od` | `mapping/order_data.py` |
| `md` | `mapping/margin_data.py` |
| `gd` | `mapping/gtt_data.py` |
| `mc` | `database/master_contract_db.py` |
| `ws` | `streaming/dhan_websocket.py` |
| `ad` | `streaming/dhan_adapter.py` |
| `map` | `streaming/dhan_mapping.py` |
| `oa` | `streaming/dhan_order_adapter.py` |
| `brlogin` | `/Users/openalgo/openalgo-desktop/openalgo/blueprints/brlogin.py` |
| `sbx/*` | `/Users/openalgo/openalgo-desktop/openalgo/broker/dhan_sandbox/*` |
| `mpp` | `/Users/openalgo/openalgo-desktop/openalgo/utils/mpp_slab.py` |
| `skill/*` | `/Users/openalgo/openalgo-desktop/openalgo/.claude/skills/broker-integration/references/*` |

Plugin metadata (`plugin.json:1-11`): `"Plugin Name": "dhan"`, `supported_exchanges: ["NSE","BSE","NFO","BFO","CDS","BCD","MCX","NSE_INDEX","BSE_INDEX"]`, `broker_type: "IN_stock"`, `leverage_config: false`. Note: the master contract additionally emits exchange `NCO` (NSE commodity, `mc:252`) and the exchange maps in `tx` and `data` know `NCO -> NSE_COMM`, though `NCO` is not in `plugin.json`.

---

## 1. Authentication

### 1.1 Credentials and the `:::` composite

- `BROKER_API_KEY` is the composite `client_id:::api_key` (`auth:22-24`, `brlogin:1087-1092`, `/Users/openalgo/openalgo-desktop/openalgo/docs/broker-integration-guide.md:1360`, `skill/auth-and-login.md:64`). Split on the first `:::`: index 0 is the Dhan **client id** (e.g. `1234567890`), index 1 is the Dhan **app id / api key**.
- `BROKER_API_SECRET` is the Dhan **app secret** for the consent flow (`auth:20,35,89,100`). It is NOT the access token (`ad:99-100`).
- `utils/config.py:11-28` only does `os.getenv("BROKER_API_KEY")` / `os.getenv("BROKER_API_SECRET")` — there is no central parsing; every Dhan module re-splits on `":::"` itself (`order:204-205`, `data:89-92`, `margin:33-35`, `gtt:49-51`, `ad:93-97`, `oa:177-181`).
- Fallback when no `:::`: `data:91-92` and `ad:96-97` treat the entire `BROKER_API_KEY` as the client id; `order:207-211` / `margin:37-40` / `gtt:52-55` fall back to the DB `user_id` column stored at login (see 1.4).

### 1.2 Flow (a): App/consent ("Individual") OAuth flow — the one the live plugin implements

`AUTH_BASE_URL = "https://auth.dhan.co"` (`auth:13`).

Step 0 (UI): the login button goes to the backend route `GET /dhan/initiate-oauth` (`brlogin:1078`; `skill/auth-and-login.md:179`). The route requires a logged-in session (`brlogin:1084-1085`), splits `BROKER_API_KEY` to get `client_id` (`brlogin:1091-1092`), errors if absent (`brlogin:1094-1097`).

Step 1 — generate consent (`auth:16-75`):
```
POST https://auth.dhan.co/app/generate-consent?client_id={dhan_client_id}
headers: app_id: <api_key part>, app_secret: <BROKER_API_SECRET>
body: none
```
(`auth:35,38,46-47`). Success is HTTP 200 **and** `data["status"] == "success"` (`auth:52-54`); returns `data["consentAppId"]` (`auth:55`). Any other status -> error string `"Failed to generate consent: {data}"` (`auth:62`) or `"Failed to generate consent: HTTP {code} - {text}"` (`auth:69`).

Step 2 — login URL (`auth:78-83`):
```
https://auth.dhan.co/login/consentApp-login?consentAppId={consentAppId}
```
`brlogin:1108-1131` stores `consent_app_id` in the session and returns an HTML page whose `<script>window.location.href = "{login_url}"</script>` performs the redirect.

Step 3 — callback: Dhan redirects to `GET /dhan/callback` with `tokenId` (`brlogin:389-413`). The code accepts `tokenId`, `token_id`, or `token` as the query parameter name (`brlogin:409-413`). If there is no tokenId on GET, it redirects to `/dhan/initiate-oauth` (`brlogin:451-454`).

Step 4 — consume consent (`auth:86-135`):
```
POST https://auth.dhan.co/app/consumeApp-consent?tokenId={tokenId}
headers: app_id: <api_key part>, app_secret: <BROKER_API_SECRET>, Content-Type: application/json
```
(`auth:98-108`). On HTTP 200 reads `accessToken` (`auth:112`); additional fields captured (`auth:115-121`): `dhanClientId`, `dhanClientName`, `dhanClientUcc`, `givenPowerOfAttorney` (-> `ddpi_status`, default `False`), `expiryTime` (-> `token_expiry`). Missing accessToken -> `"Access token not found in response"` (`auth:129`); non-200 -> `"Failed to consume consent: {status}"` (`auth:131`).

`authenticate_broker(code)` (`auth:152-182`) dispatch:
- `len(code) > 100` -> treated as a **direct access token** (JWT) -> `get_direct_access_token` (`auth:156-160`), returns 2-tuple `(token, None)`.
- else -> `consume_consent(code)`; returns 3-tuple `(access_token, dhanClientId, None)` (`auth:163-173`) or `(None, None, error)` (`auth:176`).
- empty code -> `(None, None, "No token ID provided for authentication")` (`auth:178`).

`brlogin:419-426` accepts both 2- and 3-tuples. After getting a token, it **validates it** by calling `test_auth_token` = `GET /v2/fundlimit` (`brlogin:431-440`, `funds:17-51`); failure -> `handle_auth_failure`.

Step 5 — storage: `brlogin:1042-1043` stores the auth token verbatim (`auth_token = f"{auth_token}"`). Because `"dhan"` is in the list at `brlogin:1046`, `handle_auth_success(auth_token, session["user"], "dhan", feed_token=None, user_id=user_id)` is called (`brlogin:1065-1067`), so the **`dhanClientId` returned by consume-consent is persisted in the `Auth.user_id` column** (`database/auth_db.py:602`, `:901-910`), retrievable via `get_user_id(name)`. No feed token for Dhan.

### 1.3 Flow (b): Direct access-token paste

`POST /dhan/callback` with form field `access_token` (`brlogin:456-459`). `authenticate_broker(access_token)` -> since `len > 100` -> `get_direct_access_token` (`auth:138-149`): the only validation is `len(access_token) >= 50` (`auth:142`); returns `(token, None)`. `brlogin:464-483` then validates via `test_auth_token` (`/v2/fundlimit`); on failure returns JSON `{"status":"error","message":"Token validation failed: ..."}` HTTP 401. In this path **no `dhanClientId` is learned** (`user_id` is None, `auth:157-159`), so the client id MUST come from the `:::` prefix of `BROKER_API_KEY` for orders, data, margin, GTT and websocket (`order:204-211`, `data:89-95`).

Token validity: the code never checks it. Comments state tokens "roll over daily at ~3 AM IST" (`ws:84,148`, `ad:118-119`); on websocket reconnect the token is re-read from DB with `get_auth_token(user_id, bypass_cache=True)` (`ws:145-168`). `expiryTime` from consume-consent is captured but unused (`auth:120`).

### 1.4 Partner flow

The live `broker/dhan/` plugin does **not** implement the Partner flow. Only `dhan_sandbox` has it (`sbx/api/auth_api.py:221-299`): env `BROKER_PARTNER_ID` / `BROKER_PARTNER_SECRET` (`:222-223`); `POST {AUTH}/partner/generate-consent` with headers `partner_id`, `partner_secret` -> `consentId` (`:238-247`); login URL `{AUTH}/consent-login?consentId={consentId}` (`:264`); `POST {AUTH}/partner/consume-consent?tokenId={tokenId}` with the same headers -> `accessToken` + metadata (`:281-295`). These functions exist but `authenticate_broker` in the sandbox never calls them (`sbx/api/auth_api.py:421-460`).

### 1.5 Sandbox auth differences (`sbx/api/auth_api.py`)

- Base URLs env-overridable: `DHAN_AUTH_BASE_URL` default `https://auth.dhan.co`, `DHAN_API_BASE_URL` default `https://api.dhan.co` (`:10-11`); REST base `DHAN_SANDBOX_BASE_URL` default `https://sandbox.dhan.co` (`sbx/api/baseurl.py:4`).
- `brlogin:561-565` calls `authenticate_broker("dhan_sandbox")` -> uses `BROKER_API_SECRET` **as the access token** directly (`:431-437`); accepts a direct JWT (`len>100 and "." in code`, `:440-441`); otherwise tries `consume_consent(code)` (`:444`).
- Extra endpoints implemented only in sandbox: `POST {AUTH}/app/generateAccessToken?dhanClientId=&pin=&totp=` (`:88-91`), `GET {API}/v2/RenewToken` with headers `access-token`, `dhanClientId` (`:118-121`), `POST {API}/v2/ip/setIP`, `PUT {API}/v2/ip/modifyIP` body `{"dhanClientId","ip","ipFlag":"PRIMARY"|"SECONDARY"}` (`:315-348`), `GET {API}/v2/ip/getIP` (`:370-373`), `GET {API}/v2/profile` (`:395-398`).

---

## 2. Base URL and headers

- `BASE_URL = "https://api.dhan.co"` (`base:4`); `get_url(endpoint)` prepends `/` if missing and concatenates (`base:8-20`). All endpoints below are given with their `/v2/...` path as passed to `get_url`.
- Sandbox: `https://sandbox.dhan.co` (`sbx/api/baseurl.py:4`), same `/v2/...` paths.

Header sets actually used:

| Context | Headers | Cite |
|---|---|---|
| orderbook/tradebook/positions/holdings, cancel, modify | `access-token: <token>`, `Content-Type: application/json`, `Accept: application/json` (NO client-id) | `order:34-38,410-414,455-459` |
| place order | same three + `client-id: <client_id>` when known | `order:220-228` |
| funds | three headers, no client-id | `funds:24-28,61-65` |
| margin (single & multi) | three + `client-id` when known | `margin:74-81,202-208` |
| GTT | three + `client-id` only on POST/PUT (not on GET/DELETE) | `gtt:59-67,92,126,200,255,296` |
| data (quotes/history) | `access-token`, `client-id` (REQUIRED, raises if missing), `Content-Type`, `Accept` | `data:85-107` |

GTT uses a dedicated `httpx.Client(http2=False, timeout=30.0)` because Dhan's ELB returns bogus `301 Location: https://api.dhan.co:443/v2/` on HTTP/2 POST/PUT/DELETE to `/v2/forever/orders` (`gtt:22-30`). **Port: force HTTP/1.1 for Forever Order calls.**

### 2.1 Error envelope handling (common)

`order:63-88` — response is a dict; if `status in ("failed","error")`, errors are in `data` as `{"<code>": "<message>"}` (first key = error code, `order:66-73`); alternatively `errorType`/`errorCode`/`errorMessage` top-level keys (`order:76-81`). Connection failures are returned as `{"errorType":"ConnectionError","errorMessage":str(e)}` (`order:88`).

`data:129-155` — on `status == "failed"`: error code = first key of `data`; code `"805"` is retried up to 3 times with exponential backoff `2.0 * 2**retry_count` seconds (`data:73-74,136-142`); fixed messages (`data:144-151`): `805` "Rate limit exceeded...", `806` "Data APIs not subscribed...", `810` "Authentication failed: Invalid client ID", `401` "Invalid or expired access token", `820`/`821` "Market data subscription required"; else raises `"Dhan API Error {code}: {message}"`.

`funds:37-44` — `errorType == "Invalid_Authentication"` -> invalid token; `status == "error"` -> `errors` field.

---

## 3. REST endpoints

### 3.1 Place order — `POST /v2/orders` (`order:198-266`)

Client id resolution (`order:200-215`): split `BROKER_API_KEY`; if no `:::`, `verify_api_key(data["apikey"]) -> user_id -> get_user_id(user_id)` (DB column from login). Then `data["dhan_client_id"] = client_id`, `data["apikey"] = BROKER_API_KEY` (the api_key half) (`order:214-217`), `token = get_token(symbol, exchange)` (`order:218`), body = `transform_data(data, token)` (`tx:92-167`).

Request body (`tx:98-107` plus optionals):
```json
{
  "dhanClientId": "<client_id>",          // tx:99 — falls back to data["apikey"] if dhan_client_id absent
  "transactionType": "BUY" | "SELL",     // tx:100 action.upper()
  "exchangeSegment": "NSE_EQ",           // tx:101 map_exchange_type (section 4.1)
  "productType": "INTRADAY",             // tx:102 map_product_type (4.2)
  "orderType": "LIMIT",                  // tx:103 map_order_type (4.3)
  "validity": "DAY",                     // tx:104; "IOC" if data.validity == "IOC" (tx:164-165)
  "securityId": "<token>",               // tx:105 (string from SymToken.token)
  "quantity": 1,                         // tx:106 int
  "correlationId": "<correlation_id>",   // tx:110-112 only if data.correlation_id non-empty (NOT strategy)
  "price": 100.5,                        // tx:117-119 only for pricetype LIMIT or SL (float); MARKET sends NO price
  "disclosedQuantity": 0,                // tx:122-124 only if > 0
  "triggerPrice": 99.0,                  // tx:127-131 for SL / SL-M; raises ValueError if <= 0
  "afterMarketOrder": true,              // tx:147-149 if data.after_market_order truthy
  "amoTime": "OPEN",                     // tx:150-152 one of PRE_OPEN | OPEN | OPEN_30 | OPEN_60
  "boProfitValue": 1.0,                  // tx:155-161 only if data.product == "BO"
  "boStopLossValue": 1.0
}
```
**SL-M emulation (`tx:133-144`)**: when `pricetype == "SL-M"`, `orderType` is overridden to `"STOP_LOSS"` and `price` is set to `_slm_protected_price(...)` (section 9.1). A bare `STOP_LOSS_MARKET` is therefore never sent at place time.

Response: HTTP 200 or 201 with `orderId` (`order:258-262`); returns `(res, response_data, orderid)`. Smart order (`order:270-350`) and close-all (`order:353-402`) wrap this; close-all uses `pricetype: "MARKET"`, `product = reverse_map_product_type(position.productType)`, `exchange = map_exchange(position.exchangeSegment)`, `symbol = get_symbol(position.securityId, exchange)` (`order:378-391`). Open position lookup matches `exchangeSegment == map_exchange_type(exchange)` AND `productType == product` AND `str(securityId) == str(token)` (fallback `tradingSymbol == brsymbol`), reading `netQty` (`order:181-193`).

### 3.2 Modify order — `PUT /v2/orders/{orderId}` (`order:443-487`)

Body (`tx:170-212`):
```json
{
  "dhanClientId": "<client_id or apikey>",  // tx:172
  "orderId": "<orderid>",                   // tx:173
  "orderType": "LIMIT",                     // tx:174 map_order_type; SL-M -> "STOP_LOSS" (tx:201-206)
  "legName": "ENTRY_LEG",                   // tx:175 literal
  "quantity": 1,                            // tx:176
  "validity": "DAY",                        // tx:177 literal
  "price": 100.5,                           // tx:181-182 for LIMIT/SL; SL-M gets protective price (tx:206)
  "disclosedQuantity": 0,                   // tx:185-187 if > 0
  "triggerPrice": 99.0                      // tx:193-197 for SL/SL-M; ValueError if <= 0
}
```
Note `order:447` sets `data["apikey"] = BROKER_API_KEY` **without** stripping `client_id:::`, and `modify_order` never sets `dhan_client_id`, so `dhanClientId` in the modify body is the raw `BROKER_API_KEY` string (a latent quirk; port should send the real client id). Response: `data["orderId"]` truthy -> `{"status":"success","orderid": data["orderId"]}` 200 (`order:481-482`); a missing key raises KeyError (`order:481`).

### 3.3 Cancel order — `DELETE /v2/orders/{orderId}` (`order:405-440`)
Any non-empty JSON body is treated as success -> `{"status":"success","orderid":orderid}` 200 (`order:432-434`). Cancel-all filters orderbook rows with `orderStatus in ["PENDING"]` only (`order:499-501`).

### 3.4 Read endpoints (`order:91-104`), all GET, no body
| Function | Path | Response shape |
|---|---|---|
| orderbook | `/v2/orders` | bare JSON list of orders |
| tradebook | `/v2/trades` | bare list |
| positions | `/v2/positions` | bare list; `[]` when empty; any dict = error (`order:134-140`) |
| holdings | `/v2/holdings` | bare list, or error dict with `errorCode == "DHOLDING_ERROR"` / `internalErrorCode == "DH-1111"` / `internalErrorMessage == "No holdings available"` meaning "no holdings" (`od:263-272`) |

Fields consumed — orderbook (`od:33-35,90-106,134-155`): `securityId`, `exchangeSegment`, `tradingSymbol`, `productType`, `transactionType`, `orderStatus`, `orderType`, `quantity`, `price`, `triggerPrice`, `orderId`, `updateTime`.
Tradebook (`od:166-181`): `tradingSymbol`, `exchangeSegment`, `productType`, `transactionType`, `tradedQuantity`, `tradedPrice`, `orderId`, `updateTime`.
Positions (`od:219-238`, `order:182-192`, `funds:117-122`): `tradingSymbol`, `exchangeSegment`, `productType`, `netQty`, `costPrice`, `realizedProfit`, `unrealizedProfit`, `securityId`.
Holdings (`od:279-282,336-347,363-365`): `securityId`, `tradingSymbol`, `totalQty`, `avgCostPrice`, `exchange` (always `"ALL"`, ignored).

### 3.5 Funds — `GET /v2/fundlimit` (`funds:54-147`)
Fields consumed: `availabelBalance` (Dhan's misspelling, `funds:132`), `collateralAmount` (`funds:133,139`), `utilizedAmount` (`funds:142`). `sodLimit`, `receiveableAmount`, `blockedPayoutAmount`, `withdrawableBalance` are NOT read. Output (`funds:137-143`):
```
availablecash  = availabelBalance - collateralAmount   (2dp string)
collateral     = collateralAmount
m2munrealized  = sum(positions[].unrealizedProfit)      (from GET /v2/positions, funds:98-125)
m2mrealized    = sum(positions[].realizedProfit)
utiliseddebits = utilizedAmount
```
Auth error or `status == "error"` -> all `"0.00"` (`funds:76-95`).

### 3.6 Margin (`margin`, `md`)
Routing (`margin:254-309`): exactly one position -> single calculator; 2+ -> multi calculator. Client id required (`margin:270-280`).

Single — `POST /v2/margincalculator` (`margin:94`), body per position (`md:36-48`):
```json
{"dhanClientId":"<id>","exchangeSegment":"NSE_FNO","transactionType":"BUY","quantity":50,
 "productType":"MARGIN","securityId":"<token>","price":0.0,"triggerPrice":0.0 /* only if > 0 */}
```
`productType` map for margin: CNC->CNC, NRML->MARGIN, MIS->INTRADAY, default INTRADAY (`md:64-69`). Response fields: `totalMargin`, `spanMargin`, `exposureMargin` (`md:96-98`) -> `{"status":"success","data":{"total_margin_required","span_margin","exposure_margin"}}` (`md:100-107`). Error if `errorType` present or `status in {error,failed,failure}` using `errorMessage` | `message` | `errors` (`md:86-94`).

Multi — `POST /v2/margincalculator/multi` (`margin:222`), body (`margin:210-215`):
```json
{"dhanClientId":"<id>","includePosition":true,"includeOrder":true,"scripList":[ <same objects as single> ]}
```
Response accepts snake or camel: `total_margin|totalMargin`, `span_margin|spanMargin`, `exposure_margin|exposureMargin|exposure` (`margin:172-176`).

`_normalise_success_response` (`margin:45-59`): if HTTP 200 but parsed `status == "error"`, return a fake response object with `status_code = 400` so the service layer (which treats any 200 as success) sees a failure. JSON decode failure -> 502 (`margin:100-105`); exception -> 500 (`margin:118-123`).

### 3.7 GTT / Forever Orders (`gtt`, `gd`)
All via the HTTP/1.1-only client (`gtt:30`).

Place — `POST /v2/forever/orders` (`gtt:91`), body (`gd:57-79`):
```json
{
  "dhanClientId": "<id>", "orderFlag": "SINGLE" | "OCO",
  "transactionType": "BUY", "exchangeSegment": "NSE_EQ", "productType": "CNC",
  "orderType": "LIMIT" | "MARKET",        // data.pricetype default "LIMIT"
  "validity": "DAY", "securityId": "<token>", "quantity": 1,
  "price": <SINGLE: data.price | OCO: data.stoploss>,
  "triggerPrice": <SINGLE: resolved trigger | OCO: data.triggerprice_sl>,
  "price1": <OCO only: data.target>, "triggerPrice1": <OCO only: data.triggerprice_tg>, "quantity1": <OCO only: qty>,
  "correlationId": "<correlation_id or strategy, truncated to 30 chars>"   // gd:76-79
}
```
SINGLE trigger resolution (`gd:25-33`): `trigger_price` if set, else `triggerprice_sl` if > 0 else `triggerprice_tg`. Response: HTTP 200/201 with `orderId` -> trigger id (`gtt:112-113`).

Modify — `PUT /v2/forever/orders/{orderId}` one leg per call (`gtt:148-243`). OCO sends two PUTs, `STOP_LOSS_LEG` then `TARGET_LEG` (`gtt:168-169`); SINGLE first does `GET /v2/forever/orders`, finds rows with matching `orderId`, and uses the stored `legName` (may be `ENTRY_LEG`, `STOP_LOSS_LEG` or `TARGET_LEG`), fallback `ENTRY_LEG` (`gtt:118-145,170-183`). SINGLE with pricetype LIMIT and price 0 is coerced to MARKET (DH-905 otherwise) (`gtt:185-198`). Body (`gd:114-124`):
```json
{"dhanClientId":"<id>","orderId":"<trigger_id>","orderFlag":"SINGLE|OCO","orderType":"LIMIT|MARKET",
 "legName":"ENTRY_LEG|STOP_LOSS_LEG|TARGET_LEG","quantity":1,"price":<leg price>,"triggerPrice":<leg trigger>,"validity":"DAY"}
```
Success requires HTTP 200 and `orderId` in the response (`gtt:224-226`).

Cancel — `DELETE /v2/forever/orders/{orderId}` (`gtt:252-255`); success = 200 and `orderId` present (`gtt:269-274`).

Book — `GET /v2/forever/orders` (`gtt:294-298`; the documented `/v2/forever/all` returns 404, `gtt:292-293`). Response: bare list (or `{"data":[...]}`) of **legs**, one row per leg (`gtt:323-324`, `gd:127-135`). Row fields consumed (`gd:144-199`): `orderId`, `orderStatus`, `exchangeSegment`, `tradingSymbol`, `triggerPrice`, `price`, `transactionType`, `quantity`, `productType`, `legName`, `orderType` (reused by Dhan as the SINGLE/OCO flag, `gd:183-185`), `createTime`, `updateTime`. Only `TRANSIT`/`PENDING`/`CONFIRM` are kept (`gd:141-149`). Status map (`gd:14-22`): TRANSIT/PENDING/CONFIRM->`active`, TRADED->`triggered`, EXPIRED->`expired`, CANCELLED->`cancelled`, REJECTED->`rejected`. Per-leg `pricetype` inferred: price 0 -> MARKET else LIMIT (`gd:170-172`). Legs sorted by triggerPrice ascending (`gd:162-165`). `last_price` always 0, `expires_at` "" (`gd:195,200`).

### 3.8 Quotes — `POST /v2/marketfeed/quote` (`data:783-784,980-981,1108-1109`)
Body: `{"<exchangeSegment>": [<securityId int>, ...], ...}` e.g. `{"NSE_EQ":[1333]}` (`data:778-780`, `data:944-946`). Security ids are **ints** in the request (`data:779,946`) but **string keys** in the response. Response path: `response["data"][<segment>][str(securityId)]` (`data:787-789`). Fields consumed (`data:811-836,1056-1078,1131-1174`): `last_price` (fallback `lastPrice`), `ohlc.open/high/low/close`, `volume`, `oi` (fallback `open_interest`), `depth.buy[]`/`depth.sell[]` each `{price, quantity}`, `last_quantity`. `average_price` and `net_change` are NOT read. `/v2/marketfeed/ltp` and `/ohlc` are NOT used; LTP/OHLC/depth all come from `/quote`.

OpenAlgo quote output (`data:815-836`): `{ltp, open, high, low, volume(int), oi(int), bid (=depth.buy[0].price), ask (=depth.sell[0].price), prev_close (=ohlc.close)}`. Empty quote -> all zeros (`data:795-805`). "not subscribed" error -> zeros plus `error` key (`data:841-853`).

Multiquote (`data:862-1083`): groups symbols per segment into ONE request; `BATCH_SIZE = 1000` (`data:873`); `time.sleep(1.0)` between batches (`data:874,893-894`). Per-symbol output `{"symbol","exchange","data":{bid,ask,open,high,low,ltp,prev_close,volume,oi}}` or `{"symbol","exchange","error":"No quote data available"}` (`data:1040-1046,1066-1080`).

Depth (`data:1085-1205`): same `/quote` call; emits exactly 5 `bids`/`asks` `{price, quantity}` padded with zeros (`data:1140-1162`); `ltp = last_price`, `ltq = last_quantity`, `volume`, `open/high/low`, `prev_close = ohlc.close`, `oi`, `totalbuyqty`/`totalsellqty` = sum of the 5 levels (NOT Dhan's totals) (`data:1164-1177`).

### 3.9 History (`data:380-758`) — see section 6 for the algorithm.
Daily — `POST /v2/charts/historical` body (`data:458-468`):
```json
{"securityId":"1333","exchangeSegment":"NSE_EQ","instrument":"EQUITY","fromDate":"YYYY-MM-DD","toDate":"YYYY-MM-DD","oi":true,"expiryCode":0}
```
Intraday — `POST /v2/charts/intraday` body (`data:515-524,589-598`):
```json
{"securityId":"1333","exchangeSegment":"NSE_EQ","instrument":"EQUITY","interval":"1|5|15|25|60","fromDate":"YYYY-MM-DD","toDate":"YYYY-MM-DD","oi":true,"expiryCode":0}
```
Response: parallel arrays `timestamp`, `open`, `high`, `low`, `close`, `volume`, `open_interest` (`data:478-484`). Candle rows: floats for OHLC, `int(float(volume))`, `int(float(open_interest))`, falsy -> 0 (`data:489-498`).

Option chain / expired options endpoints: **not used** anywhere in `broker/dhan/`.

---

## 4. Field mappings

### 4.1 Exchange — OpenAlgo -> Dhan `exchangeSegment`
`tx:232-241` (`map_exchange_type`, orders/margin/GTT): NSE->`NSE_EQ`, BSE->`BSE_EQ`, CDS->`NSE_CURRENCY`, NFO->`NSE_FNO`, BFO->`BSE_FNO`, BCD->`BSE_CURRENCY`, MCX->`MCX_COMM`, NCO->`NSE_COMM`; **no NSE_INDEX/BSE_INDEX** (returns None).
`data:245-256` (`_get_exchange_segment`, quotes/history) adds NSE_INDEX->`IDX_I`, BSE_INDEX->`IDX_I`.
`map:12-23` (websocket) identical to the data map.
Reverse (`tx:249-262`, `map_exchange`): NSE_EQ->NSE, BSE_EQ->BSE, NSE_CURRENCY->CDS, NSE_FNO->NFO, BSE_FNO->BFO, BSE_CURRENCY->BCD, MCX_COMM->MCX, NSE_COMM->NCO; unknown -> returned as-is (not None).

### 4.2 Product
Forward (`tx:269-274`): CNC->`CNC`, NRML->`MARGIN`, MIS->`INTRADAY`, default `INTRADAY`.
Reverse (`tx:285`): CNC->CNC, MARGIN->NRML, INTRADAY->MIS, else None. Orderbook mapper (`od:43-55`): `CNC` kept on NSE/BSE; `INTRADAY`->`MIS`; `MARGIN`->`NRML` only when exchange in `[NFO, MCX, BFO, CDS, BCD, NCO]`.

### 4.3 Order type
Forward (`tx:219-225`): MARKET->`MARKET`, LIMIT->`LIMIT`, SL->`STOP_LOSS`, SL-M->`STOP_LOSS_MARKET`, default `MARKET` — but SL-M is then overridden to `STOP_LOSS` + protective price (`tx:135-140`).
Reverse (`od:134-141`): MARKET->MARKET, LIMIT->LIMIT, STOP_LOSS->`SL`, STOP_LOSS_MARKET->`SL-M`.

### 4.4 Transaction type / validity
`transactionType` = `action.upper()` (`tx:100`), i.e. `BUY`/`SELL`. `validity` = `"DAY"` default, `"IOC"` if requested (`tx:104,164-165`); GTT always `"DAY"` (`gd:64,123`).

### 4.5 Order status (orderbook) — `od:96-106`
`TRADED`->`complete`, `PENDING`->`open`, `REJECTED`->`rejected`, `CANCELLED`->`cancelled`. Other Dhan statuses (`TRANSIT`, `PART_TRADED`, `EXPIRED`, `CONFIRM`) are passed through **unchanged** by the REST mapper. The order-update websocket mapper (`oa:25-32`) maps TRANSIT->`open`, PENDING->`open`, REJECTED->`rejected`, CANCELLED->`cancelled`, TRADED->`complete`, EXPIRED->`expired`.

### 4.6 Symbol resolution
Orderbook/tradebook/positions: `exchange = map_exchange(exchangeSegment)`, `symbol = get_symbol(securityId, exchange)` (token-keyed lookup, `od:33-42`); fallback to raw `tradingSymbol` or `str(securityId)` (`od:61-63`). Holdings: probe `get_symbol(securityId, "NSE")` then `"BSE"` (`od:284-292`), default exchange `"NSE"`.

### 4.7 Output records
Orderbook (`od:143-155`): `{symbol, exchange, action, quantity, price, trigger_price, pricetype, product, orderid, order_status, timestamp(=updateTime)}`.
Tradebook (`od:169-179`): `{symbol, exchange, product, action, quantity(=tradedQuantity), average_price(=tradedPrice), trade_value(=qty*price), orderid, timestamp(=updateTime)}`.
Positions (`od:229-237`): `{symbol, exchange, product, quantity(=netQty), average_price(=costPrice), ltp (fetched via multiquote since /positions has no LTP, od:188-216), pnl = round(realizedProfit+unrealizedProfit, 2)}`.
Holdings (`od:372-381`): `{symbol, exchange, quantity(=totalQty), product:"CNC", average_price(=avgCostPrice), ltp (via multiquote), pnl=(ltp-avg)*qty, pnlpercent}`; portfolio stats (`od:336-349`) `totalinvvalue = Σ avgCostPrice*totalQty`, `totalholdingvalue = Σ (ltp or avgCostPrice)*totalQty`.

### 4.8 Scaling
No price scaling anywhere: Dhan REST prices are rupees as floats. Only master-contract tick size is scaled (section 5.5).

---

## 5. Master contract (`mc`)

### 5.1 Download
URL: `https://images.dhan.co/api-data/api-scrip-master.csv` (`mc:92`) — the **non-detailed** file. Saved to `tmp/master.csv` (`mc:104,262`), `requests.get(timeout=10)` (`mc:100`). Flow (`mc:548-568`): download -> delete symtoken table -> process -> bulk insert (skipping tokens already present, `mc:69-72`) -> delete temp CSV -> socketio emit `master_contract_download`.

### 5.2 Columns read
`SEM_EXM_EXCH_ID`, `SEM_SEGMENT`, `SEM_SMST_SECURITY_ID`, `SEM_INSTRUMENT_NAME`, `SEM_EXPIRY_CODE`, `SEM_TRADING_SYMBOL`, `SEM_LOT_UNITS`, `SEM_CUSTOM_SYMBOL`, `SEM_EXPIRY_DATE`, `SEM_STRIKE_PRICE`, `SEM_OPTION_TYPE`, `SEM_TICK_SIZE`, `SEM_EXPIRY_FLAG`, `SEM_EXCH_INSTRUMENT_TYPE`, `SEM_SERIES`, `SM_SYMBOL_NAME` (`mc:511-528`; column names are stripped of whitespace `mc:265`).

Column -> SymToken (`mc:280-295`): `token = SEM_SMST_SECURITY_ID`; `name = SM_SYMBOL_NAME`; `expiry = SEM_EXPIRY_DATE` parsed with `pd.to_datetime(errors="coerce")`, formatted `%d-%b-%y` then `.upper()` (e.g. `28-MAR-24`), NaT -> `"-1"` (`mc:268-282`); `strike = SEM_STRIKE_PRICE`; `lotsize = SEM_LOT_UNITS`; `brsymbol = SEM_TRADING_SYMBOL`.

### 5.3 Segment gating -> (exchange, brexchange, instrumenttype) (`mc:181-254`)
Segment codes: `E` equity, `I` index, `D` equity derivative, `C` currency, `M` commodity (`mc:183-187`). Instrument sets: `EQUITY_FNO_INSTRUMENTS = {FUTIDX, FUTSTK, OPTIDX, OPTSTK, OPTFUT}`, `CURRENCY_INSTRUMENTS = {FUTCUR, OPTCUR}`, `COMMODITY_INSTRUMENTS = {FUTCOM, FUTIDX, OPTFUT, OPTIDX}` (`mc:190-192`). `derivative_type = SEM_OPTION_TYPE if "OPT" in instrument else "FUT"` (`mc:214`).

| SEM_SEGMENT | SEM_INSTRUMENT_NAME | SEM_EXM_EXCH_ID | exchange | brexchange | instrumenttype | cite |
|---|---|---|---|---|---|---|
| E | EQUITY | NSE | NSE | NSE_EQ | EQ | mc:216-218 |
| E | EQUITY | BSE | BSE | BSE_EQ | EQ | mc:219-220 |
| I | INDEX | NSE | NSE_INDEX | IDX_I | INDEX | mc:222-224 |
| I | INDEX | BSE | BSE_INDEX | IDX_I | INDEX | mc:225-226 |
| D | in EQUITY_FNO | NSE | NFO | NSE_FNO | CE/PE/FUT | mc:228-230 |
| D | in EQUITY_FNO | BSE | BFO | BSE_FNO | CE/PE/FUT | mc:231-232 |
| C | in CURRENCY | NSE | CDS | NSE_CURRENCY | CE/PE/FUT | mc:234-236 |
| C | in CURRENCY | BSE | BCD | BSE_CURRENCY | CE/PE/FUT | mc:237-238 |
| M | in COMMODITY | MCX | MCX | MCX_COMM | CE/PE/FUT | mc:240-242 |
| M | in COMMODITY | NSE | NCO | NSE_COMM | CE/PE/FUT | mc:251-252 |
| anything else | | | Unknown (row dropped) | | | mc:254,309-321 |

No series filter: all NSE equity series are kept, non-EQ series get a suffix (5.4). Security ids are unique per **segment**, not exchange (`mc:181-182,200-207`); duplicates on `(exchange, token)` and `(symbol, exchange)` are logged as errors (`mc:477-508`).

### 5.4 Symbol construction (`mc:146-178`)
`expiry` string with dashes removed, e.g. `28MAR24` (`mc:150`).
- EQUITY, NSE: `qualify_equity_symbol(SEM_TRADING_SYMBOL, SEM_SERIES)` = `SEM_TRADING_SYMBOL` if series blank or `EQ`, else `"{SEM_TRADING_SYMBOL}-{SERIES}"` e.g. `ELECTCAST-W1` (`mc:130-143,154-155`).
- EQUITY, BSE: `SEM_TRADING_SYMBOL` (`mc:156-157`).
- INDEX: `SEM_TRADING_SYMBOL` (`mc:158-159`), then renamed (5.6).
- FUT: `parts = SEM_CUSTOM_SYMBOL.split(" ")`; if 3 or 4 parts -> `"{parts[0]}{expiry}FUT"` e.g. `NIFTY28MAR24FUT` (`mc:161-167`).
- CE/PE: if 4 or 5 parts -> `"{parts[0]}{expiry}{format_strike(SEM_STRIKE_PRICE)}{CE|PE}"` e.g. `NIFTY28MAR2420800CE` (`mc:168-173`). `format_strike` = `f"{float(strike):.6f}".rstrip("0").rstrip(".")` -> `20800`, `87.5`, `1.015` (`mc:115-127`).
- Weekly options: no special handling; the DDMMMYY expiry already disambiguates.
- Otherwise the raw `SEM_CUSTOM_SYMBOL` is kept (`mc:175-176`).

Derivative `name` is overwritten with the underlying root extracted from the built symbol via regex `^(.+?)(\d{2}(?:JAN|...|DEC)\d{2})(?:\d+(?:\.\d+)?)?(?:FUT|CE|PE)?$` group 1 (`mc:443-462`; `/Users/openalgo/openalgo-desktop/openalgo/database/token_db_enhanced.py:26-29,82-84`).

### 5.5 Tick size / lot size
`tick_size = SEM_TICK_SIZE / 100` (paise -> rupees) for every row EXCEPT `SEM_INSTRUMENT_NAME == "INDEX"`, which is kept as-is (`mc:285-294`). `lotsize = SEM_LOT_UNITS` unscaled. (Sandbox keeps `tick_size = SEM_TICK_SIZE` unscaled, `sbx/database/master_contract_db.py:285`.)

### 5.6 Index renames
NSE_INDEX (`mc:326-365`): symbol uppercased, spaces and dashes removed, then renamed via `{NIFTYNEXT50: NIFTYNXT50, NIFTYMCAP50: NIFTYMIDCAP50, NIFTYMIDSMALLCAP400: NIFTYMIDSML400, NIFTYSMALLCAP100: NIFTYSMLCAP100, NIFTYSMALLCAP250: NIFTYSMLCAP250, NIFTYSMALLCAP50: NIFTYSMLCAP50, NIFTY100EQUALWEIGHT: NIFTY100EQLWGT, NIFTY100LOWVOLATILITY30: NIFTY100LOWVOL30, NIFTYMID100FREE: NIFTYMIDCAP100}`; if the result is NOT in the allow-list below, the original `SEM_TRADING_SYMBOL` is restored. Allow-list (`mc:327-341`): NIFTY, NIFTYNXT50, FINNIFTY, BANKNIFTY, MIDCPNIFTY, INDIAVIX, HANGSENGBEESNAV, NIFTY100, NIFTY200, NIFTY500, NIFTYALPHA50, NIFTYAUTO, NIFTYCOMMODITIES, NIFTYCONSUMPTION, NIFTYCPSE, NIFTYDIVOPPS50, NIFTYENERGY, NIFTYFMCG, NIFTYGROWSECT15, NIFTYGS10YR, NIFTYGS10YRCLN, NIFTYGS1115YR, NIFTYGS15YRPLUS, NIFTYGS48YR, NIFTYGS813YR, NIFTYGSCOMPSITE, NIFTYINFRA, NIFTYIT, NIFTYMEDIA, NIFTYMETAL, NIFTYMIDLIQ15, NIFTYMIDCAP100, NIFTYMIDCAP150, NIFTYMIDCAP50, NIFTYMIDSML400, NIFTYMNC, NIFTYPHARMA, NIFTYPSE, NIFTYPSUBANK, NIFTYPVTBANK, NIFTYREALTY, NIFTYSERVSECTOR, NIFTYSMLCAP100, NIFTYSMLCAP250, NIFTYSMLCAP50, NIFTY100EQLWGT, NIFTY100LIQ15, NIFTY100LOWVOL30, NIFTY100QUALTY30, NIFTY200QUALTY30, NIFTY50DIVPOINT, NIFTY50EQLWGT, NIFTY50PR1XINV, NIFTY50PR2XLEV, NIFTY50TR1XINV, NIFTY50TR2XLEV, NIFTY50VALUE20.

BSE_INDEX exact-match rename (`mc:369-411`): SENSEX->SENSEX, BANKEX->BANKEX, SNSX50->SENSEX50, SNXT50->BSESENSEXNEXT50, BSE100->BSE100, BSE200->BSE200, BSE500->BSE500, MID150->BSE150MIDCAPINDEX, LMI250->BSE250LARGEMIDCAPINDEX, MSL400->BSE400MIDSMALLCAPINDEX, AUTO->BSEAUTO, `BSE CG`->BSECAPITALGOODS, `BSE CD`->BSECONSUMERDURABLES, `BSE HC`->BSEHEALTHCARE, `BSE IT`->BSEINFORMATIONTECHNOLOGY, CARBON->BSECARBONEX, CPSE->BSECPSE, DOL100->BSEDOLLEX100, DOL200->BSEDOLLEX200, DOL30->BSEDOLLEX30, ENERGY->BSEENERGY, BSEFMC->BSEFASTMOVINGCONSUMERGOODS, FINSER->BSEFINANCIALSERVICES, GREENX->BSEGREENEX, INFRA->BSEINDIAINFRASTRUCTUREINDEX, INDSTR->BSEINDUSTRIALS, BSEIPO->BSEIPO, LRGCAP->BSELARGECAP, METAL->BSEMETAL, MIDCAP->BSEMIDCAP, MIDSEL->BSEMIDCAPSELECTINDEX, OILGAS->BSEOIL&GAS, POWER->BSEPOWER, BSEPSU->BSEPSU, REALTY->BSEREALTY, SMLCAP->BSESMALLCAP, SMLSEL->BSESMALLCAPSELECTINDEX, SMEIPO->BSESMEIPO, TECK->BSETECK, TELCOM->BSETELECOM.

### 5.7 Stored row
SymToken columns (`mc:33-49`): `symbol`, `brsymbol` (= `SEM_TRADING_SYMBOL`), `name`, `exchange`, `brexchange` (Dhan segment string, e.g. `NSE_FNO`), `token` (= `SEM_SMST_SECURITY_ID` as string), `expiry` (`DD-MMM-YY` upper or `-1`), `strike` (float), `lotsize` (int), `instrumenttype` (`EQ|INDEX|FUT|CE|PE`), `tick_size` (float, rupees).

---

## 6. Historical data algorithm (`data:160-758`)

Timeframe map (`data:165-174`): `1m`->`"1"`, `5m`->`"5"`, `15m`->`"15"`, `25m`->`"25"`, `1h`->`"60"`, `D`->`"D"`. Unsupported -> exception listing supported keys (`data:399-403`). (There is no `60m` key; the hour key is `1h`.)

Steps:
1. Normalise dates to `YYYY-MM-DD` (`data:406-409`). Move a weekend start forward to Monday and a weekend end back to Friday (`data:345-366,412`). If both still non-trading -> empty frame (`data:415-419`).
2. If `start == end`, set `end = start + 1 day` (`data:422-427`).
3. `security_id = get_token(symbol, exchange)` (`data:431`), `exchange_segment = _get_exchange_segment` (4.1), `instrument = _get_instrument_type(exchange, symbol)` (6.1).
4. Daily (`interval == "D"`): endpoint `/v2/charts/historical`, `fromDate = start`, `toDate = end + 1 day` (inclusive, `data:452-456`), `oi: true`, `expiryCode: 0` (`data:458-468`). Timestamp conversion `_convert_timestamp_to_ist(ts, is_daily=True)` (`data:203-214`): `utc = utcfromtimestamp(ts) + 5h30m`; take that IST calendar date at 00:00 local-naive; return `int(start_of_day.timestamp() + 19800)`.
5. Intraday: endpoint `/v2/charts/intraday`. If `start == end - 1 day` (single day) -> one request with `fromDate=start`, `toDate=end` (`data:510-524`). Else split into **90-day chunks** `[(start, min(start+90d, end)), ...]` (`data:222-241,563`); skip a chunk only if it contains no weekday at all (`data:577-583`); request `fromDate=chunk_start`, `toDate=chunk_end` (`data:586-598`); per-chunk retry `CHUNK_MAX_RETRIES = 3` with backoff `2.0 * 2**attempt` for exceptions AND for empty-200 responses (`data:611-672`); a chunk that still fails raises (`data:674-681`); an empty chunk strictly between non-empty chunks raises "interior gap" (`data:690-704`). Intraday timestamp: `int((utcfromtimestamp(ts) + 5h30m).timestamp())` (`data:216-220`), i.e. the raw epoch plus 19800 seconds.
6. Daily only: if today in `[start, end]`, call `get_quotes` and append a candle `{timestamp: int(today 00:00 local + 19800), open, high, low, close=ltp, volume, oi}` when `ltp > 0` (`data:707-736`).
7. Sort by timestamp, drop duplicate timestamps (`data:745-750`). Output columns `timestamp, open, high, low, close, volume, oi` (`data:418,742`).

### 6.1 `instrument` derivation (`data:259-334`)
- NSE/BSE -> `EQUITY`; NSE_INDEX/BSE_INDEX -> `INDEX`.
- NFO/BFO: symbol ends with `CE`/`PE` -> `OPTIDX` if any of `[NIFTY, NIFTYNXT50, FINNIFTY, BANKNIFTY, MIDCPNIFTY, INDIAVIX, SENSEX, BANKEX, SENSEX50]` is a substring, else `OPTSTK`; otherwise `FUTIDX` (same substring test) else `FUTSTK` (`data:268-309`).
- NCO: CE/PE -> `OPTFUT`, else `FUTCOM` (`data:312-315`).
- MCX: if symbol starts with one of `MCXBULLDEX`, `MCXMETLDEX`, `MCXENRGDEX` (`data:34`) -> `OPTIDX`/`FUTIDX`, else CE/PE -> `OPTFUT`, else `FUTCOM` (`data:318-324`).
- CDS/BCD: CE/PE -> `OPTCUR`, else `FUTCUR` (`data:327-332`).
`expiryCode` is always `0` (`data:468,523,597`); `oi` always `true`.

---

## 7. WebSocket market feed (`ws`, `ad`, `map`)

### 7.1 URLs
- 5-depth / regular: `wss://api-feed.dhan.co?version=2&token={access_token}&clientId={client_id}&authType=2` (`ws:132-140`, urlencoded).
- 20-depth: `wss://depth-api-feed.dhan.co/twentydepth?token={access_token}&clientId={client_id}&authType=2` (no `version`) (`ws:128-130`).
- Credentials: `client_id` from `BROKER_API_KEY` prefix (`ad:93-97`); `access_token` from DB `get_auth_token(user_id, bypass_cache=True)` (`ad:102`). On reconnect the token is re-read and the URL rebuilt (`ws:145-168,245`).
- Connection: `ping_interval=30`, `ping_timeout=10` (`ws:203`). Reconnect: up to 10 attempts, delay `min(5 * 2**(attempt-1), 60)` seconds (`ws:182-238`). Data-stall watchdog: check every 30 s, force close if no inbound frame for 90 s (`ws:66-67,530-554`). Fatal (no reconnect) if the error text contains `429`, `too many requests`, `client id is blocked`, `subscription`, or `plan` (`ws:574-584`). 20-depth socket is connected lazily on first 20-depth subscribe (`ad:162-163,404-407`).

### 7.2 Request JSON
```json
{"RequestCode": 15, "InstrumentCount": 2, "InstrumentList": [{"ExchangeSegment": "NSE_EQ", "SecurityId": "1333"}, ...]}
```
(`ws:328-332`; `SecurityId` is the SymToken token string, `ad:357`). Request codes (`ws:50-60`): SUBSCRIBE_TICKER 15, UNSUBSCRIBE_TICKER 16, SUBSCRIBE_QUOTE 17, UNSUBSCRIBE_QUOTE 18, SUBSCRIBE_FULL 21, UNSUBSCRIBE_FULL 22, SUBSCRIBE_20_DEPTH 23, UNSUBSCRIBE_20_DEPTH 24 (unverified; vendored docs say 25, `ws:45-49`), DISCONNECT 12 (`{"RequestCode": 12}`, `ws:258`). Batch limit: 100 instruments per message on the regular socket, 50 on 20-depth (`ws:323,404,488`). Limits: 5000 subscriptions per 5-depth connection, 50 per 20-depth (`map:99-100`). OpenAlgo mode -> Dhan mode: 1->TICKER, 2->QUOTE, 3->FULL (5-depth) or 20_DEPTH (`ad:360-364`). 20-depth only for NSE/NFO (`map:86-96`), requested via a `:20` symbol suffix (`ad:299-302`); falls back to 5-depth FULL if no 20-depth data in 30 s (`ad:399-400,936,950-1009`). Subscribes are coalesced for 0.5 s then sent grouped by mode (`ad:69,165-209`).

### 7.3 Binary framing — all little-endian (`struct "<"`)
Regular socket header, 8 bytes (`ws:653-657`):
| offset | type | field |
|---|---|---|
| 0 | u8 | feed response code |
| 1-2 | u16 | message length (total incl. header) |
| 3 | u8 | exchange segment code |
| 4-7 | u32 | security id |
Payload = `data[offset+8 : offset+message_length]`; multiple messages may be concatenated in one frame (`ws:649-707`). Code 0 = heartbeat, ignored (`ws:694-696`). Unknown codes ignored.

Payload offsets are relative to the start of the payload (byte 8 of the message):

**Ticker, code 2** (`ws:759-775`, min 8 bytes): `ltp f32 @0`, `ltt u32 @4`.

**Quote, code 4** (`ws:777-799`, min 42 bytes): `ltp f32 @0`, `ltq u16 @4`, `ltt u32 @6`, `atp f32 @10`, `volume u32 @14`, `total_sell_quantity u32 @18`, `total_buy_quantity u32 @22`, `open f32 @26`, `close f32 @30`, `high f32 @34`, `low f32 @38`.

**OI, code 5** (`ws:801-813`, min 4): `oi u32 @0`.

**Prev close, code 6** (`ws:815-828`, min 8): `prev_close f32 @0`, `prev_oi u32 @4`.

**Market status, code 7**: not handled (falls into "unknown code", `ws:697-698`).

**Full, code 8** (`ws:830-887`, min 154 bytes payload = 162 total): `ltp f32 @0`, `ltq u16 @4`, `ltt u32 @6`, `atp f32 @10`, `volume u32 @14`, `total_sell_quantity u32 @18`, `total_buy_quantity u32 @22`, `oi u32 @26`, `oi_high u32 @30`, `oi_low u32 @34`, `open f32 @38`, `close f32 @42`, `high f32 @46`, `low f32 @50`, then 5 depth levels of 20 bytes starting @54: for level i at `54 + 20*i`: `bid_qty u32 @+0`, `ask_qty u32 @+4`, `bid_orders u16 @+8`, `ask_orders u16 @+10`, `bid_price f32 @+12`, `ask_price f32 @+16`. Emitted as `depth.buy[i] = {price: bid_price, quantity: bid_qty, orders: bid_orders}`, `depth.sell[i] = {price: ask_price, quantity: ask_qty, orders: ask_orders}`.

**Disconnect, code 50** (`ws:913-923`): `disconnect_code u16 @0`; known reason `805 = "Maximum websocket connections exceeded"`.

20-depth socket header, 12 bytes (`ws:719-727`): `message_length u16 @0`, `feed_response_code u8 @2`, `exchange_segment u8 @3`, `security_id u32 @4`, `sequence u32 @8` (skipped). Payload at offset 12. Codes 41 = DEPTH_20_BID, 51 = DEPTH_20_ASK (`ws:33-34,737-738`). Payload (`ws:889-911`, min 320 bytes): 20 levels x 16 bytes: `price f64 @+0`, `quantity u32 @+8`, `orders u32 @+12`. Bid and ask arrive separately and are accumulated per security id; published only when both sides are present (`ad:751-819`).

### 7.4 Exchange segment numeric codes (`map:39-49`)
0 IDX_I -> NSE_INDEX (BSE_INDEX also 0), 1 NSE_EQ -> NSE, 2 NSE_FNO -> NFO, 3 NSE_CURRENCY -> CDS, 4 BSE_EQ -> BSE, 5 MCX_COMM -> MCX, 6 NSE_COMM -> NCO (inferred, never observed), 7 BSE_CURRENCY -> BCD, 8 BSE_FNO -> BFO. Inbound packets are matched to subscriptions by `(token == security_id AND segment == expected)` then by token alone (`ad:669-689`).

### 7.5 Normalised tick published to ZMQ (`ad:829-885`, `websocket_proxy/base_adapter.py:388-408`)
Topic `f"{exchange}_{symbol}_{LTP|QUOTE|DEPTH}"` (`ad:706-717`); payload is JSON:
- ticker: `{symbol, exchange, timestamp: now_ms, mode: 1, ltp, ltt}`
- quote: `{..., mode: 2, ltp, ltt, volume, open, high, low, close, last_quantity(=ltq), average_price(=atp), total_buy_quantity, total_sell_quantity}`
- full: `{..., mode: 3, ltp, ltt, volume, open, high, low, close, oi, oi_high, oi_low, depth: {buy:[{price,quantity,orders}x5], sell:[...]}, depth_level: 5}`
- 20-depth: `{symbol, exchange, mode: 3, timestamp, depth: {buy: [20 levels], sell: [20 levels]}, depth_level: 20}` (`ad:802-812`)
OI (code 5) and prev-close (code 6) packets are parsed but NOT published (`ad:712-715`).

### 7.6 Order-update WebSocket (`oa`)
URL `wss://api-order-update.dhan.co` (`oa:18`), no headers (`oa:92-93`). After connect send:
```json
{"LoginReq": {"MsgCode": 42, "ClientId": "<client_id>", "Token": "<access_token>"}, "UserType": "SELF"}
```
(`oa:95-105`). Inbound frames are JSON; only `Type == "order_alert"` is processed, payload in `Data` (`oa:113-116`). Field names are **camelCase on the wire** with PascalCase fallback (`oa:66-78`): `orderNo`, `status` (Title-case, uppercased), `quantity`, `tradedQty`, `exchange`, `segment` (`E|D|C|M`), `securityId`, `symbol`, `txnType` (`B|S`), `orderType` (`LMT|MKT|SL|SLM`), `product` (`C|I|M|F`), `price`, `triggerPrice`, `avgTradedPrice`, `reasonDescription`. Maps (`oa:25-63`): product C->CNC, I->MIS, M->NRML, F->NRML; pricetype LMT->LIMIT, MKT->MARKET, SL->SL, SLM->SL-M; action B->BUY, S->SELL; (exchange,segment) -> NSE/E->NSE, NSE/D->NFO, NSE/C->CDS, BSE/E->BSE, BSE/D->BFO, BSE/C->BCD, MCX/M->MCX. Output (`oa:142-161`): `{orderid, symbol, exchange, action, quantity, price, trigger_price, pricetype, product, order_status, filled_quantity(=tradedQty), pending_quantity(=max(qty-traded,0)), average_price, rejection_reason (only when REJECTED)}`.

---

## 8. Rate limits and sleeps

Published Dhan limits (`skill/cross-broker-reference.md:56-64`): per second — Order 10, Data 5, Quote **1**, Non-trading 20; per minute Order 250; per hour Order 1000; per day Order 7000, Data 100000. 25 modifications per order (`skill/cross-broker-reference.md:71`, `skill/data-and-account.md:156-157`). Quote batch cap 1000 securities per request (`data:873`, `skill/cross-broker-reference.md:33`).

Implemented pacing (`data:21-68`): per-category last-call timestamps; `DHAN_DATA_INTERVAL = 0.2` s for any endpoint not starting with `/v2/marketfeed`; `DHAN_QUOTE_INTERVAL = 1.1` s for `/v2/marketfeed*` (`data:28-29,78`). Slot is reserved under a lock then slept outside it; under the gthread worker a wait beyond `max_queue_wait("data")` raises `BrokerBusyError` (`data:59`, `utils/broker_backpressure.py:151-162`). Additional sleeps: `805` retry backoff 2, 4, 8 s (`data:136-142`); multiquote inter-batch 1.0 s (`data:893-894`); history chunk retry backoff 2, 4 s (`data:647-655,665-672`). Order endpoints have NO client-side pacing. Websocket: subscribe batch delay 0.5 s (`ad:69`), 5-connection-per-user limit noted (`ad:163`).

---

## 9. Quirks a port must reproduce

### 9.1 SL-M -> protective STOP_LOSS (`tx:31-89`)
Dhan rejects/mangles bare `STOP_LOSS_MARKET` under MPP (DH-906 "Trigger Price should be greater than Price") (`tx:35-39`). Algorithm:
1. `instrument_type = CE|PE|FUT|EQ` from symbol suffix (`mpp:36-54`).
2. `tick_size` from SymToken (`tx:60-69`); must be finite and > 0 else raise.
3. `pct = get_mpp_percentage(trigger, instrument_type)/100` with slabs EQ/FUT: `<100 -> 2%`, `<500 -> 1%`, else `0.5%`; options: `<10 -> 5%`, `<100 -> 3%`, `<500 -> 2%`, else `1%` (`mpp:17-30,73-100`).
4. SELL: `raw = min(trigger*(1-pct), trigger - tick)`, snap **floor** to tick; must be > 0 (`tx:73-82`). BUY: `raw = max(trigger*(1+pct), trigger + tick)`, snap **ceil** (`tx:83-87`).
5. `_snap_to_tick`: `k = floor|ceil(round(value/tick, 6))`, result `round(k*tick, decimals(tick))` (`tx:19-28`).
Applied on both place (`tx:135-140`) and modify (`tx:201-206`).

### 9.2 Client id sourcing differences
Consent flow stores `dhanClientId` in `Auth.user_id`; direct-token flow stores nothing, so `BROKER_API_KEY` MUST be `client_id:::api_key` (section 1.3). `data.py` raises if `BROKER_API_KEY` is missing (`data:85-87`). `funds.py` and the read endpoints send no `client-id` header (`order:34-38`, `funds:24-28`).

### 9.3 Error payload with HTTP 200
Margin: `_normalise_success_response` turns a 200 + `status:"error"` into a 400 (`margin:45-59`). Quotes: `status == "failed"` with HTTP 200 (`data:130`). Holdings: "no holdings" is an error-shaped dict (`od:263-272`).

### 9.4 Forever-order quirks
HTTP/1.1 only (`gtt:22-30`); GET is `/v2/forever/orders` not `/all` (`gtt:292-293`); stored `legName` must be looked up before SINGLE modify (`gtt:118-183`); LIMIT with price 0 coerced to MARKET on SINGLE modify (`gtt:185-198`); `orderType` in the GET response carries the SINGLE/OCO flag (`gd:183-185`).

### 9.5 Exchange coverage gaps
`map_exchange_type` has no index segments (orders on indexes are impossible) (`tx:232-241`). NSE_COMM (NCO) accepts orders and margin but Dhan streams no feed and `/v2/marketfeed` does not echo the key (`map:29-38`, `mc:243-250`). 20-depth only on NSE/NFO (`map:86-96`). BSE_INDEX and NSE_INDEX share segment 0 so inbound index ticks default to NSE_INDEX unless token matches (`map:27,40,68-71,76-79`).

### 9.6 Position/holdings LTP
`/v2/positions` and `/v2/holdings` return no LTP; the mapper calls the internal multiquote service to fill `ltp` (`od:188-216,295-323`). Holdings exchange is always `"ALL"` from Dhan; real exchange resolved by probing NSE then BSE with `securityId` (`od:276-292`).

### 9.7 Sandbox (`broker/dhan_sandbox/`) differences
Base URL `https://sandbox.dhan.co` (env `DHAN_SANDBOX_BASE_URL`) (`sbx/api/baseurl.py:4`); token = `BROKER_API_SECRET` (`sbx/api/auth_api.py:431-437`); `client-id` header required on every call including funds and reads (`sbx/api/order_api.py:59-61`, `sbx/api/funds.py:37-39`); no client-side pacing, instead HTTP 429 retry x3 (`sbx/api/data.py:69-96`); intraday chunks are 5 days (`sbx/api/data.py:190-191`); quotes synthesised from `/v2/charts/intraday` 1-minute candles because the sandbox has no `/marketfeed/quote` (`sbx/api/data.py:698-700`); tick_size unscaled (`sbx/database/master_contract_db.py:285`); no GTT module; market websocket is a mock generator (`sbx/streaming/dhan_sandbox_adapter.py:15-20`); no SL-M protective conversion (`diff` of `tx` vs `sbx/mapping/transform_data.py`).

### 9.8 Misc
- `correlationId` for regular orders comes only from `data["correlation_id"]` (`tx:110-112`); for GTT from `correlation_id` or `strategy`, truncated to 30 (`gd:76-79`).
- `cancel_all` cancels only `PENDING` orders (`order:500`).
- `modify_order` body `dhanClientId` is the raw `BROKER_API_KEY` (bug noted in 3.2).
- Place-order headers log redacts `access-token` (`order:232-237`).
- Daily and intraday candle timestamps are both epoch **+19800 s** (IST-shifted naive epoch), not true UTC (`data:214,220,723`; `skill/history-data.md:29-30`).

---

## B. Kotak Neo (plugin id `kotak`) — literal implementation spec for a Rust port

Source of truth root: `/Users/openalgo/openalgo-desktop/openalgo/`. Every citation below is `<path relative to that root>:<line>`; the broker plugin lives under `broker/kotak/`. Nothing in this document is inferred from Kotak's public docs — it is a transcription of what the Python web code actually sends and parses.

## 0. File inventory (all read in full)

| Path (under `/Users/openalgo/openalgo-desktop/openalgo/`) | Lines | Role |
|---|---|---|
| `broker/kotak/plugin.json` | 10 | Plugin metadata |
| `broker/kotak/api/auth_api.py` | 178 | Two-step TOTP + MPIN login |
| `broker/kotak/api/order_api.py` | 574 | Orders, trades, positions, holdings, place/modify/cancel, smart order, close-all |
| `broker/kotak/api/data.py` | 1248 | Quotes, depth, multiquotes, history |
| `broker/kotak/api/funds.py` | 112 | Limits (funds) |
| `broker/kotak/api/margin_api.py` | 145 | Margin calculator |
| `broker/kotak/mapping/transform_data.py` | 279 | OpenAlgo -> Kotak order payload, exchange/product/pricetype maps, SL-M -> SL conversion |
| `broker/kotak/mapping/order_data.py` | 586 | Kotak -> OpenAlgo orderbook/tradebook/positions/holdings |
| `broker/kotak/mapping/margin_data.py` | 134 | Margin payload/response mapping |
| `broker/kotak/database/master_contract_db.py` | 680 | Scrip master download + CSV processing |
| `broker/kotak/streaming/__init__.py` | 9 | Exports `KotakWebSocketAdapter`, `KotakWebSocket` |
| `broker/kotak/streaming/kotak_adapter.py` | 1515 | BaseBrokerWebSocketAdapter implementation (ZMQ publisher) |
| `broker/kotak/streaming/kotak_feed_config.py` | 161 | Per-data-centre feed host lookup |
| `broker/kotak/streaming/sfeed_protocol.py` | 420 | SFeed binary decoder (current feed) |
| `broker/kotak/streaming/sfeed_websocket.py` | 644 | SFeed client (JSON control plane + binary ticks) |
| `broker/kotak/streaming/kotak_websocket.py` | 639 | Legacy HSM client wrapper |
| `broker/kotak/streaming/HSWebSocketLib.py` | 1565 | Legacy HSM binary codec (vendored from Kotak SDK) |
| `broker/kotak/streaming/kotak_mapping.py` | 116 | Streaming-side exchange/product/order-type maps |
| `broker/kotak/streaming/kotak_order_adapter.py` | 321 | Order-update WebSocket (`/realtime`) |
| `blueprints/brlogin.py` | 758-784 | Login form handling for kotak |
| `frontend/src/pages/BrokerTOTP.tsx` | 141-176, 377-379 | React login form fields for kotak |
| `utils/config.py` | 11-28 | `BROKER_API_KEY` / `BROKER_API_SECRET` getters |
| `websocket_proxy/base_adapter.py` | 97-565 | Base adapter contract (ZMQ publish, response helpers) |
| `websocket_proxy/order_adapter.py` | 12-240 | Base order-update adapter (reconnect schedule, symbol mapping) |

`plugin.json` (`broker/kotak/plugin.json:1-11`): `"Plugin Name": "Kotak"`, `"supported_exchanges": ["NSE", "BSE", "NFO", "BFO", "CDS", "MCX", "NSE_INDEX", "BSE_INDEX"]`, `"broker_type": "IN_stock"`, `"leverage_config": false`. Note **BCD is not in the supported list** even though the mapping tables contain it (`broker/kotak/mapping/transform_data.py:247,264`).

---

## 1. Authentication (two-step: TOTP, then MPIN)

### 1.1 Credentials the user supplies

| Where | Value | Citation |
|---|---|---|
| `.env` `BROKER_API_KEY` | **UCC** (Kotak Unique Client Code). Read raw via `get_broker_api_key()` -> `os.getenv("BROKER_API_KEY")` | `broker/kotak/api/auth_api.py:42`, `utils/config.py:11-18` |
| `.env` `BROKER_API_SECRET` | **Access token** (the long-lived Neo API "access token" from the Kotak developer portal). Read raw via `get_broker_api_secret()` -> `os.getenv("BROKER_API_SECRET")` | `broker/kotak/api/auth_api.py:43`, `utils/config.py:21-28` |
| Login form `mobile` (or `mobilenumber`) | 10-digit mobile; React prefixes `+91` before POST | `blueprints/brlogin.py:765`, `frontend/src/pages/BrokerTOTP.tsx:144-153,377-379` |
| Login form `totp` | 6-digit TOTP from Kotak NEO app | `blueprints/brlogin.py:766`, `BrokerTOTP.tsx:163-171` |
| Login form `mpin` | 6-digit trading MPIN | `blueprints/brlogin.py:767`, `BrokerTOTP.tsx:154-162` |

There is **no consumer key/secret, no `/oauth2/token`, no Bearer exchange, no password, no `hsServerId`** anywhere in the kotak code. The access token is sent verbatim (no `Bearer ` prefix) in the `Authorization` header (`auth_api.py:72`).

The broker-integration skill reference claims kotak's `BROKER_API_KEY` is `<something>:::client_id` (`.claude/skills/broker-integration/references/auth-and-login.md:68`). **That is stale**: the current code never splits `BROKER_API_KEY` (`auth_api.py:42`) and streaming uses the whole value as the SFeed `user` (`broker/kotak/streaming/kotak_adapter.py:105`).

### 1.2 Form handling order (`blueprints/brlogin.py:758-784`)

1. `GET /kotak/callback` -> `redirect("/broker/kotak/totp")` (React page) (`brlogin.py:760-762`).
2. `POST /kotak/callback` (React posts `multipart/form-data` with `mobile`, `mpin`, `totp`, `csrf_token`; `BrokerTOTP.tsx:173,376-392`). Server reads `request.form.get("mobile") or request.form.get("mobilenumber")`, `totp`, `mpin` (`brlogin.py:765-767`). If any is missing: `400` with JSON `{"status": "error", "message": "Please provide Mobile Number, TOTP, and MPIN"}` (`brlogin.py:770-772`).
3. Calls `authenticate_broker(mobile_number, totp, mpin)` -> `(auth_token, error_message)` (`brlogin.py:777`). React UI warning text: `"Make sure TOTP is registered in your Kotak NEO mobile app. Go to Settings > Security > Enable TOTP."` (`BrokerTOTP.tsx:174-175`).

### 1.3 Mobile normalisation (`auth_api.py:57-63`)

```
m = mobile.strip().replace("+91", "").replace(" ", "")
if m.startswith("91") and len(m) == 12: m = m[2:]
mobile = "+91" + m
```

### 1.4 Step 1 — TOTP login (`auth_api.py:68-98`)

```
POST https://mis.kotaksecurities.com/login/1.0/tradeApiLogin
Headers:
  Authorization: <BROKER_API_SECRET access token, verbatim>
  neo-fin-key: neotradeapi
  Content-Type: application/json
Body (JSON): {"mobileNumber": "+919876543210", "ucc": "<BROKER_API_KEY>", "totp": "123456"}
```

Success test: `"data" in resp and resp["data"]["status"] == "success"` (`auth_api.py:91`). Error message: `resp.get("errMsg", resp.get("message", "TOTP login failed"))` -> returned as `"TOTP Login Error: <msg>"` (`auth_api.py:92-94`). Extract `view_token = resp["data"]["token"]`, `view_sid = resp["data"]["sid"]` (`auth_api.py:97-98`).

### 1.5 Step 2 — MPIN validate (`auth_api.py:102-148`)

```
POST https://mis.kotaksecurities.com/login/1.0/tradeApiValidate
Headers:
  Authorization: <access token>
  neo-fin-key: neotradeapi
  sid: <view_sid>          (lower-case header name here)
  Auth: <view_token>
  Content-Type: application/json
Body (JSON): {"mpin": "123456"}
```

Success test identical (`data.status == "success"`, `auth_api.py:127`); error -> `"MPIN Validation Error: <errMsg|message>"` (`auth_api.py:128-130`). Extract:

| Response field | Variable | Note | Citation |
|---|---|---|---|
| `data.token` | `trading_token` | goes in `Auth` header for all trading REST calls | `auth_api.py:133` |
| `data.sid` | `trading_sid` | goes in `Sid` header | `auth_api.py:134` |
| `data.baseUrl` | `base_url` | dynamic REST base, e.g. `https://cis.kotaksecurities.com` (docstring). Missing -> warning only | `auth_api.py:33,135,142-143` |
| `data.dataCenter` | `data_center` | e.g. `"E43"`, `"E21"`; selects the market-data feed host. Missing -> warning, streaming falls back to default SFeed | `auth_api.py:136-148` |

### 1.6 Stored auth string and how it is parsed back

```
"<trading_token>:::<trading_sid>:::<base_url>:::<access_token>:::<data_center>"
```
(`auth_api.py:153-162`). The 5th part was appended later; **every reader takes `split(":::")[:4]` positionally** and treats a missing 5th as unknown (`auth_api.py:155-159`): `order_api.py:31,247,452,489`; `data.py:195-197`; `funds.py:20-30` (rejects `< 4` parts, returns `{}`); `margin_api.py:27`; `master_contract_db.py:330`; `kotak_adapter.py:193-201,959-971` (zips into keys `["auth_token","sid","hs_server_id","access_token"]` — the key literally named `hs_server_id` **holds the base_url**; it is never used, see §9); `kotak_order_adapter.py:301-309`. Data centre: `auth_parts[4] if len(auth_parts) > 4 else ""` (`kotak_adapter.py:70-77`). `BrokerData.__init__` raises `ValueError("Kotak auth token missing baseUrl. Please re-login (TOTP + MPIN) to refresh credentials.")` if `base_url` is empty or does not start with `http` (`data.py:199-203`), and `rstrip("/")`s it (`data.py:205`).

Return-shape to the login blueprint: `(auth_string, None)` on success, `(None, "<message>")` on any failure (`auth_api.py:165-178`).

---

## 2. REST header conventions

| Call family | Headers | Body encoding | Citation |
|---|---|---|---|
| Trading GETs (orders, trades, positions, holdings) | `accept: application/json`, `Sid: <trading_sid>`, `Auth: <trading_token>`, `neo-fin-key: neotradeapi` | none | `order_api.py:39-50` |
| Trading POSTs (place, modify, cancel, limits, check-margin) | same four + `Content-Type: application/x-www-form-urlencoded` | `jData=` + `urllib.parse.quote(json.dumps(obj))` (percent-encoded JSON; `quote` with default `safe="/"`) | `order_api.py:266-275,457-465,508-516`; `funds.py:42-52`; `margin_api.py:36-46` |
| Market data (quotes, history) and scrip-master file-paths | `Authorization: <access_token>` (verbatim), `Content-Type: application/json` | none (GET) | `data.py:284,969`; `master_contract_db.py:350` |
| Login | see §1.4/1.5 | JSON | `auth_api.py:71-75,105-111` |

All trading/market URLs are `f"{base_url}{endpoint}"` where `base_url` is the MPIN-validate `baseUrl` (`order_api.py:47`). There is **no `sId=`/`hsServerId` query parameter anywhere**.

The shared httpx client is used (connection pooling); default timeout is the shared client's 120 s unless a per-call timeout is given (`data.py:74-77`).

---

## 3. Every REST endpoint

### 3.1 Order book — `GET {base}/quick/user/orders` (`order_api.py:57-58`)

Response: `{"stat": "Ok", "data": [ {order}, ... ]}` or `{"stat": "Not_Ok", ...}`; `data` may be `null` (`order_data.py:51-63`). Per-order fields read: `exSeg`, `tok`, `trdSym`, `sym`, `trnsTp`, `ordSt`, `prcTp`, `avgPrc`, `prc`, `qty`, `trgPrc`, `prod`, `nOrdNo`, `ordEntTm`, `GuiOrdId` (`order_data.py:67-72,94-111,138-168`).

### 3.2 Trade book — `GET {base}/quick/user/trades` (`order_api.py:61-62`)

Response: same `stat`/`data` envelope (`order_data.py:185-197`). Fields read: `exSeg`, `tok` (**empty string on tradebook rows**, `order_data.py:12-13`), `trdSym`, `sym`, `trnsTp`, `prod`, `fldQty`, `avgPrc`, `nOrdNo`, `flId` (per-fill id), `exTm`, `GuiOrdId` (`order_data.py:201-243`).

### 3.3 Positions — `GET {base}/quick/user/positions` (`order_api.py:143-162`)

Response `{"stat": "Ok"|"Not_Ok", "data": [...]|null}`. `stat` check is case-insensitive `== "ok"` (`order_api.py:194-201`). A non-dict payload (e.g. a list error body) **raises** `Exception("Kotak returned a positions payload that is not an object: <type>")` — deliberately not normalised to empty, because close-all treats an empty book as success (`order_api.py:145-160`). Fields read: `exSeg`, `trdSym`, `tok`, `prod`, `flBuyQty`, `flSellQty`, `cfBuyQty`, `cfSellQty`, `buyAmt`, `sellAmt`, `cfBuyAmt`, `cfSellAmt`, `upldPrc`, `avgnetprice`, `multiplier`, `genNum`, `genDen`, `prcNum`, `prcDen` (`order_api.py:233-240,405-418`; `order_data.py:294,339-363,377-405`). **Kotak returns no LTP and no pnl in positions**; `_backfill_ltp` (`order_api.py:65-140`) resolves each row to an OpenAlgo symbol, batch-calls `get_multiquotes` once for the distinct `(symbol, exchange)` set, and stamps a scratch `_ltp` float on each raw row (best-effort, never fatal).

### 3.4 Holdings — `GET {base}/portfolio/v1/holdings` (`order_api.py:165-166`)

Response `{"data": [...]}`; `data is None` -> `{}` (`order_data.py:540-543`). Fields read: `instrumentToken`, `exchangeSegment`, `instrumentType` (`"Equity"` -> `"CNC"`), `displaySymbol`, `quantity`, `averagePrice`, `mktValue`, `holdingCost` (`order_data.py:496-519,550-563`).

### 3.5 Place order — `POST {base}/quick/order/rule/ms/place` (`order_api.py:246-296`)

Body: `jData=<urlencoded JSON>` of the dict built by `transform_data` (§4.1). Response: `{"stat": "Ok", "nOrdNo": "<id>"}`; `orderid = resp["nOrdNo"] if resp["stat"] == "Ok" else None` (`order_api.py:289`). Returns `(response, response_data, orderid)`; on payload-build failure returns `(None, {"stat": "Not_Ok", "emsg": str(e)}, None)` (`order_api.py:259-264`); on HTTP/other error `(None, {"stat": "NotOk", "error": str(e)}, None)` (`order_api.py:291-296`). The `response.status` attribute is set to `status_code` for caller compatibility (`order_api.py:285`).

### 3.6 Modify order — `POST {base}/quick/order/vr/modify` (`order_api.py:488-545`)

Body: `jData=` of `transform_modify_order_data` dict (§4.2). Success: `stat == "Ok"` -> `({"status": "success", "orderid": resp["nOrdNo"]}, 200)`; else `({"status": "error", "message": resp.get("emsg", "Failed to modify order")}, http_status)` (`order_api.py:531-536`). Payload-build errors return `({"status": "error", "message": str(e)}, 400)` (`order_api.py:499-504`).

### 3.7 Cancel order — `POST {base}/quick/order/cancel` (`order_api.py:451-485`)

Body: `jData=` of `{"on": "<orderid>", "am": "NO"}` (`order_api.py:457`). Success `stat == "Ok"` -> `({"status": "success", "orderid": resp.get("nOrdNo")}, 200)`; else `({"status": "error", "message": resp.get("emsg", "Failed to cancel order")}, http_status)` (`order_api.py:474-479`).

Cancel-all (`order_api.py:548-574`): fetch orderbook; if `data is None` return `([], [])`; cancel every order with `ordSt in ["open", "trigger pending"]`; return `(canceled_ids, failed_ids)`.

### 3.8 Funds / limits — `POST {base}/quick/user/limits` (`funds.py:12-112`)

Body is the literal string `jData=%7B%22seg%22%3A%22ALL%22%2C%22exch%22%3A%22ALL%22%2C%22prod%22%3A%22ALL%22%7D` i.e. `{"seg":"ALL","exch":"ALL","prod":"ALL"}` (`funds.py:42-44`). Response: flat object with `stat`, `emsg`, and numeric strings. Mapping (`funds.py:87-96`), all formatted `f"{x:.2f}"`:

| OpenAlgo key | Kotak field | Note |
|---|---|---|
| `availablecash` | `CollateralValue` | **misnamed by Kotak: it is cash**, verified live (`funds.py:78-86`) |
| `collateral` | `Collateral` | pledged-shares margin |
| `m2munrealized` | `UnrealizedMtomPrsnt` | |
| `m2mrealized` | `RealizedMtomPrsnt` | |
| `utiliseddebits` | `MarginUsed` | |

Other fields observed but unused: `Net` (== `CollateralValue + Collateral - MarginUsed`), `RmsPayInAmt`, `RmsPayOutAmt` (`funds.py:82-86`). `stat != "Ok"` -> log `emsg`, return `{}` (`funds.py:67-70`). Any exception -> `{}`.

### 3.9 Margin calculator — `POST {base}/quick/user/check-margin` (`margin_api.py:15-84`)

One order per request (`margin_api.py:91-93`). Body `jData=` of (`margin_data.py:47-57`; all values strings):

```json
{"brkName": "KOTAK", "brnchId": "ONLINE", "exSeg": "nse_fo", "prc": "0", "prcTp": "MKT",
 "prod": "NRML", "qty": "75", "tok": "<token>", "trnsTp": "B"}
```

`trnsTp` = `"B"` if action upper == `"BUY"` else `"S"` (`margin_data.py:44`). Response: `stat == "Ok"` then `reqdMrgn` is used as `total_margin_required`; other fields present per comment `avlMrgn, ordMrgn, mrgnUsd, rmsVldtd`; error text in `errMsg` (`margin_data.py:81-97`). Output `{"status":"success","data":{"total_margin_required": float, "span_margin": 0, "exposure_margin": 0}}`; multiple positions are summed (`margin_data.py:104-130`).

### 3.10 Quotes / depth / multi-quotes — `GET {base}/script-details/1.0/quotes/neosymbol/{query}/all` (`data.py:276-361`)

- `query` = `"<segment>|<pSymbol>"`, or comma-joined list for multi (`data.py:420,756,772`). URL-encoded with `urllib.parse.quote(query, safe="|,")` so `|` and `,` stay literal and spaces become `%20` (`data.py:281`).
- `filter_name` is always `"all"` in current code; `"depth"` exists on the API but is a strict subset (book only, no ltp/ohlc), so depth also uses `"all"` (`data.py:502-507,537-539`).
- Headers: `Authorization: <access_token>`, `Content-Type: application/json` (`data.py:284`). Timeout 15 s (`data.py:77,299`).
- Response: a JSON **list** of quote objects. Fields read: `display_symbol`, `exchange`, `exchange_token`, `ltp`, `ohlc.open/high/low/close` (close = previous close, `data.py:610`), `depth.buy[]`/`depth.sell[]` each `{price, quantity}` (`data.py:560-576`), `total_buy`, `total_sell`, `last_volume`, `open_int`, `last_traded_quantity` (`data.py:441-475,543-613,799-801`). Every field arrives as a string; a quantity can be `"16131960.0000"` (`data.py:152-165`).
- Error on HTTP 200: `{"stat":"Not_Ok","emsg":"...","stCode":1009}` for an invalid symbol -> treated as failure (`data.py:322-334`).
- Empty list on 200 is "no data" (default quote returned) (`data.py:477-480`).

Index symbols: `exchange` containing `"INDEX"` uses `_get_kotak_exchange` (`NSE_INDEX`->`nse_cm`, `BSE_INDEX`->`bse_cm`) and the **name candidate map** (§9.4), probing each candidate until a non-empty list returns (`data.py:363-377,384-400`). Multi-quote uses only the first candidate (`data.py:724-731`).

Non-index: `psymbol = get_token(symbol, exchange)`, `brexchange = get_brexchange(symbol, exchange)`; if `brexchange in ["NSE","BSE","NFO","BFO","CDS","MCX"]` it is mapped through `_get_kotak_exchange`, otherwise used as-is (cash rows store `"NSE"`/`"BSE"`, F&O rows store `nse_fo` etc., see §5) (`data.py:402-421`).

Quote output (`data.py:466-476`): `bid` = `depth.buy[0].price` else ltp; `ask` = `depth.sell[0].price` else ltp; `open/high/low` from `ohlc`; `ltp`; `prev_close` = `ohlc.close`; `volume` = `float(last_volume)`; `oi` = `int(open_int)`. Default quote all zeros (`data.py:864-876`).

Depth output (`data.py:600-614`): `bids`/`asks` = first 5 of `depth.buy`/`depth.sell` as `{"price": float, "quantity": int}`, **padded to 5** with `{"price":0,"quantity":0}` (`data.py:582-586`); `totalbuyqty` = `int(total_buy)` or sum of level quantities if 0 (Neo leaves it 0 on F&O, `data.py:588-596`); `totalsellqty` likewise; `ltp`, `ltq` = `last_traded_quantity`, `open/high/low`, `prev_close` = `ohlc.close`, `volume` = `int(last_volume)`, `oi` = `int(open_int)`. Default depth (`data.py:878-897`) carries all keys with zeros and 5 zero levels.

Multi-quote (`data.py:626-862`): `BATCH_SIZE = 25` (live cap is 42-49; 50 returns HTTP 400 `"Please set the Neo symbol max value to 50."`), `RATE_LIMIT_DELAY = 0.2` s between batches (`data.py:637-645`). Response rows are matched back to queries by `f"{exchange}|{exchange_token}"` and `f"{exchange}|{display_symbol without '-EQ'/'-IN'}"`, case-insensitive fallback (`data.py:796-821`). Output per symbol `{"symbol","exchange","data":{bid,ask,open,high,low,ltp,prev_close,volume,oi}}` or `{"symbol","exchange","error": "..."}` (`data.py:823-859`). A failed sub-batch yields error rows for every symbol in it; only all-batches-failed raises (`data.py:665-686`).

### 3.11 History — `GET {base}/market-data/1.0/historical/details` (`data.py:953-1025`) — **implemented, not a placeholder**

The skill reference `.claude/skills/broker-integration/references/history-data.md:182-186` says "Kotak Neo serves no historical data ... `timeframe_map` is empty". **That is stale**; the current `data.py` implements it fully. Details in §6.

### 3.12 Scrip-master file paths — `GET {base}/script-details/1.0/masterscrip/file-paths` (`master_contract_db.py:321-472`)

Headers `Authorization: <access_token>`, `Content-Type: application/json`; timeout 30 s. Tried against `[base_url, "https://cis.kotaksecurities.com", "https://neo-gw.kotaksecurities.com"]` in order (`master_contract_db.py:334-338`). Response `{"data": {"filesPaths": ["https://.../nse_cm-v1.csv", ...]}}` (`:371-372`). Details §5.

### 3.13 Feed config — `GET https://lapi.kotaksecurities.com/5config/config?appVersion=1.0.0&platform=api&environment=prod` (`kotak_feed_config.py:28,36-38,100-108`)

No auth headers; timeout 5 s. Response `{"data": {"configs": {...}}}`. Keys consumed: `"<DC>_broadcast_source"` -> `"hs"|"sh"|"ks"`, `"<DC>_<src>_broadcast_endpoint"` (market data), `"<DC>_<src>_interactive_endpoint"` (order feed) (`kotak_feed_config.py:119-130`). Details §7.1.

---

## 4. OpenAlgo -> Kotak order payloads

### 4.1 Place (`transform_data.py:148-179`) — all values strings

```json
{
  "am": "NO",                 // after-market flag, always "NO"
  "dq": "0",                  // disclosed_quantity, str(data.get("disclosed_quantity","0"))
  "es": "nse_cm",             // reverse_map_exchange(exchange)
  "mp": "0",                  // market protection, always "0"
  "pc": "MIS",                // data.get("product","MIS") -- RAW OpenAlgo product, not passed through map_product_type
  "pf": "N",                  // always "N"
  "pr": "0",                  // _fmt_price(price): "0" when zero, else str(value)
  "pt": "MKT",                // map_order_type(pricetype)
  "qt": "75",                 // str(quantity)
  "rt": "DAY",                // validity, always "DAY"
  "tp": "0",                  // _fmt_price(trigger_price)
  "ts": "RELIANCE-EQ",        // brsymbol (pTrdSymbol) via get_br_symbol(symbol, exchange)
  "tt": "B",                  // "B" for BUY, "S" for SELL, else "None"
  "ig": "openalgo"            // ORDER_TAG; echoed back as GuiOrdId; Kotak rejects a blank one
}
```

Citations: `am/dq/es/mp/pc/pf/pr/pt/qt/rt/tp/ts/tt` `transform_data.py:158-172`; `ig` `:143-145,174`; `_fmt_price` `:132-140` ("Kotak rejects '0.0' on numeric fields"). The `token` argument is **unused** for placement (`transform_data.py:148`).

**SL-M conversion** (`transform_data.py:59-129`): Kotak rejects `SL-M` from API sessions (`"Market order with Algo Id not allowed"`). When `pricetype == "SL-M"`: trigger must be > 0 else `ValueError("Trigger price is required and must be positive for SL-M orders")`; `pt` is rewritten to `"SL"` and `pr` to a protective limit one MPP band past the trigger, snapped to the instrument tick (from `SymToken.tick_size`; unresolvable tick -> `ValueError`, fail closed). Direction: SELL -> `min(trigger - offset, trigger - tick)` floored to tick, must be > 0; BUY -> `max(trigger + offset, trigger + tick)` ceiled to tick. MPP offset tables (`:19-25`): EQ/FUT `[(100, 2.0%), (500, 1.0%), (inf, 0.5%)]`; options `[(5, 10%), (10, 5%), (100, 3%), (500, 2%), (inf, 1%)]` with an absolute `0.10` offset when price < `1.0`. Instrument type is derived from the symbol suffix `CE`/`PE`/`FUT`/else `EQ` (`utils/mpp_slab.py:36-54`). Tick decimals: `-Decimal(str(tick)).as_tuple().exponent` (`:43-45`); snap: `round(value/tick, 6)` then floor/ceil then `round(k*tick, decimals)` (`:48-56`).

### 4.2 Modify (`transform_data.py:182-205`)

```json
{"tk": "<token>", "dq": "0", "es": "nse_cm", "mp": "0", "dd": "NA", "vd": "DAY",
 "pc": "MIS", "pr": "0", "pt": "L", "qt": "1", "tp": "0", "ts": "RELIANCE-EQ",
 "no": "<orderid>", "tt": "B"}
```

No `ig`, no `am`, no `rt`, no `pf`. `tt` uses `data["action"] == "BUY"` without `.upper()` (`:198`). Same SL-M -> SL rewrite applied (`:201-203`).

### 4.3 Static maps (`transform_data.py`)

| OpenAlgo | Kotak | Function / line |
|---|---|---|
| `MARKET`->`MKT`, `LIMIT`->`L`, `SL`->`SL`, `SL-M`->`SL-M`; default **`"MARKET"`** (not a Kotak code) | | `map_order_type` `:208-213` |
| `CNC`->`CNC`, `NRML`->`NRML`, `MIS`->`MIS`; unknown -> `None` | | `map_product_type` `:216-225`, `reverse_map_product_type` `:270-279` |
| `NSE`->`nse_cm`, `BSE`->`bse_cm`, `CDS`->`cde_fo`, `NFO`->`nse_fo`, `BFO`->`bse_fo`, `BCD`->`bcs_fo`, `MCX`->`mcx_fo`; **`NSE_INDEX`/`BSE_INDEX` -> `None`** | | `reverse_map_exchange` `:253-267` |
| reverse: `nse_cm`->`NSE`, `bse_cm`->`BSE`, `cde_fo`->`CDS`, `nse_fo`->`NFO`, `bse_fo`->`BFO`, `bcs_fo`->`BCD`, `mcx_fo`->`MCX` | | `map_exchange` `:236-250` |
| `map_variety` (`MARKET`/`LIMIT`->`NORMAL`, `SL`/`SL-M`->`STOPLOSS`) — defined, **unused** | | `:228-233` |

Streaming has its own copy (`broker/kotak/streaming/kotak_mapping.py:7-24`) which additionally maps `NSE_INDEX`->`nse_cm`, `BSE_INDEX`->`bse_cm`, lower-case keys, and **`BCD`->`"bcs-fo"` (hyphen, inconsistent with `bcs_fo`)**. Its product/order-type tables (`:29-72`) are unused by the adapter.

### 4.4 Smart order / close-all semantics (`order_api.py:169-448`)

- Per-symbol lock and 1 s position-book cache, invalidated after each placement (`order_api.py:169-219`).
- `get_open_position`: converts symbol to brsymbol, exchange to segment, product mapped; matches `trdSym`, `exSeg`, `prod`; `net_qty = (flBuyQty - flSellQty) + (cfBuyQty - cfSellQty)` (`:222-243`).
- Smart order decision tree `:316-390` (standard OpenAlgo logic).
- Close-all `:393-448`: for each position with non-zero net, action `SELL` if net > 0 else `BUY`, symbol via `get_symbol(position["tok"], map_exchange(exSeg))`, product `reverse_map_product_type(prod)`, `pricetype: "MARKET"`, strategy `"Squareoff"`; returns `({"status":"success","message":"All Open Positions SquaredOff"}, 200)`; empty book -> `({"message":"No Open Positions Found"}, 200)`.

---

## 5. Kotak -> OpenAlgo reverse mappings

### 5.1 Symbol resolution for broker rows (`order_data.py:8-35`)

`_openalgo_symbol(row, exchange)`: try `get_symbol(row["tok"], exchange)` when `tok` non-empty; else `get_oa_symbol(row["trdSym"] or row["sym"], exchange)` (brsymbol == `pTrdSymbol` == what Kotak sends as `trdSym`); else `None` (caller keeps the raw broker symbol).

### 5.2 Orderbook

`map_order_data` (`:38-73`): `stat == "Not_Ok"` -> `{}`; `data is None` -> `{}`; else for each order set `exSeg = map_exchange(exSeg)` and `trdSym = <OpenAlgo symbol>` if resolvable.

`calculate_order_statistics` (`:76-120`): `trnsTp` `"B"`->`"BUY"`, `"S"`->`"SELL"`; `ordSt == "trigger pending"` -> `"open"`; counts `complete`, `open`, `rejected`.

`transform_order_data` (`:123-172`): `prcTp` `MKT`->`MARKET`, `L`->`LIMIT`, `SL`->`SL`, `SL-M`->`SL-M`. `price` = `avgPrc`, except for `LIMIT`/`SL` with `ordSt != "complete"` -> `prc`. Output:

```json
{"symbol": trdSym, "exchange": exSeg, "action": trnsTp, "quantity": qty, "price": <above>,
 "trigger_price": trgPrc, "pricetype": prcTp, "product": prod, "orderid": nOrdNo,
 "order_status": ordSt, "timestamp": ordEntTm, "order_tag": GuiOrdId}
```

Kotak `ordSt` strings seen (lower-case free text), with the full lifecycle vocabulary from the order-feed adapter (`kotak_order_adapter.py:61-78`): `complete`, `rejected`, `cancelled`, `canceled`, `open`, `trigger pending`, `put order req received`, `validation pending`, `open pending`, `modified`, `modify validation pending`, `modify pending`, `cancel pending`, `after market order req received`, `modify after market order req received`, `cancelled after market order`. REST orderbook passes `ordSt` through verbatim except the `trigger pending`->`open` collapse.

### 5.3 Tradebook (`:175-245`)

`map_trade_data` same envelope handling; `trnsTp` B/S -> BUY/SELL. Output per trade:

```json
{"symbol": trdSym, "exchange": exSeg, "product": prod, "action": trnsTp, "quantity": fldQty,
 "average_price": avgPrc, "trade_value": float(fldQty)*float(avgPrc), "orderid": nOrdNo,
 "tradeid": flId, "timestamp": exTm, "order_tag": GuiOrdId}
```

### 5.4 Positions (`:248-489`)

`map_position_data` == `map_order_data` (`:248-249`). `_number(field)` = `float(value or 0)`, unparsable -> `0.0` (`:252-271`).

- `quantity = int((flBuyQty - flSellQty) + (cfBuyQty - cfSellQty))` (`:382-385`).
- `factor = multiplier * (genNum/genDen) * (prcNum/prcDen)`, each term `or 1.0` (`:274-295`).
- Carry-forward amounts (`:298-363`): if `cfBuyQty or cfSellQty`: use `upldPrc` when > 0 -> `cf_buy = cfBuyQty*upldPrc*factor`, `cf_sell = cfSellQty*upldPrc*factor`; else fall back to `cfBuyAmt`/`cfSellAmt` and flag `carried_valuation = True` (these are Kotak's previous-settlement re-valuation, not cost).
- `total_buy_amt = cf_buy + buyAmt`; `total_sell_amt = cf_sell + sellAmt`; `buy_qty = flBuyQty + cfBuyQty`; `sell_qty = flSellQty + cfSellQty` (`:402-405`).
- `average_price`: default `avgnetprice`; if qty > 0 and buy_qty > 0 -> `round(total_buy_amt/(buy_qty*factor), 2)`; if qty < 0 and sell_qty > 0 -> `round(total_sell_amt/(sell_qty*factor), 2)`; if qty != 0 otherwise -> `0.0` (`:386,414-419`).
- `ltp = round(_ltp, 2)` (scratch field from backfill) (`:377,390`).
- `pnl`: `realized = total_sell_amt - total_buy_amt`; net 0 -> `round(realized,2) or 0.0`; ltp present -> `round(realized + net_qty*ltp*factor, 2) or 0.0`; open with no ltp -> `0.0` (`:444-464`).
- `average_price_basis = "carry_forward_valuation"` added only when the fallback amounts were used (`:484-485`).

Output keys: `symbol`, `exchange`, `product`, `quantity`, `average_price`, `ltp`, `pnl`, optional `average_price_basis`.

### 5.5 Holdings (`:492-586`)

`map_portfolio_data`: `exchangeSegment = map_exchange(exchangeSegment)`; `symbol = get_symbol(instrumentToken, exchange)` if found; `instrumentType "Equity" -> "CNC"`. `transform_holdings_data` output per row: `symbol = displaySymbol`, `exchange = exchangeSegment`, `quantity`, `product = instrumentType`, `average_price = round(float(averagePrice),2)`, `pnl = round(mktValue - holdingCost, 2)`, `pnlpercent = round((mktValue-holdingCost)/holdingCost*100, 2)` (0 when cost 0). Totals (`:571-586`): `totalholdingvalue = Σ mktValue`, `totalinvvalue = Σ holdingCost`, `totalprofitandloss`, `totalpnlpercentage` rounded 2.

---

## 6. Master contract (`broker/kotak/database/master_contract_db.py`)

### 6.1 Discovering file URLs (`:321-472`)

1. Read the logged-in user's stored auth string, take `access_token` = part `[3]` (`:326-330`).
2. For each base in `[base_url, "https://cis.kotaksecurities.com", "https://neo-gw.kotaksecurities.com"]`: `GET {base}/script-details/1.0/masterscrip/file-paths` with `Authorization: <access_token>`, `Content-Type: application/json`, timeout 30 (`:343-358`). On 200 with `data.filesPaths` present, bucket each URL by substring of its basename (lower-cased): `nse_cm`->`NSE_CM`, `bse_cm`->`BSE_CM`, `nse_fo`->`NSE_FO`, `bse_fo`->`BSE_FO`, `cde_fo`->`CDE_FO`, `mcx_fo`->`MCX_FO`, `nse_com`->`NSE_COM` (`:376-394`). **`bcs_fo` is never requested**, so BCD rows never enter the table.
3. Fallback direct URLs (dated `YYYY-MM-DD`, `:421-431`): `https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod/{today}/transformed/{cde_fo|mcx_fo|nse_fo|bse_fo|nse_com}.csv` and `.../{today}/transformed-v1/{bse_cm-v1|nse_cm-v1}.csv`; each probed with a streamed `GET` carrying `Range: bytes=0-0` (HEAD loops on redirects, `:433-465`), accepted on 200/206.
4. Download each with `GET url` timeout 30, saved as `tmp/<KEY>.csv` (`:108-121`). `NSE_COM` is downloaded but has **no processor** (`:629-636`).

### 6.2 Target table (`SymToken`, `:34-50`)

Columns: `symbol`, `brsymbol`, `name`, `exchange`, `brexchange`, `token`, `expiry`, `strike` (float), `lotsize` (int), `instrumenttype`, `tick_size` (float). Composite index `(symbol, exchange)`.

`copy_from_dataframe` (`:64-87`) **skips any row whose `token` already exists anywhere in the table** (global, not per exchange) — a pSymbol shared across segments is inserted only once, from whichever file is processed first (order: NSE_CM, NSE_FO, BSE_CM, CDE_FO, MCX_FO, BSE_FO, `:629-636`). Table is wiped before processing (`:626`).

### 6.3 CSV columns and per-segment rules

Raw column quirks: NSE_CM headers contain `"dStrikePrice;"` and `"dTickSize "` (trailing semicolon / space) and are read as-is (`:152,154`); all other processors first do `df.columns.str.replace(" ", "").str.replace(";", "")` (`:197-198,288-289,483-484,524-525,570-571`).

| Segment file | token | name | symbol | brsymbol | brexchange | exchange | expiry | strike | lotsize | tick_size | instrumenttype | Filters | Citation |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `NSE_CM.csv` | `pSymbol` | `pDesc` | `pSymbolName` | `pTrdSymbol` | **`"NSE"`** (literal, not `nse_cm`) | `"NSE"` if `pGroup in ["EQ","BE"]`; `"NSE_INDEX"` if `pISIN` is NaN | `pExpiryDate` (raw string) | `dStrikePrice;` (raw, not scaled) | `lLotSize` | `dTickSize ` / 100 | `"EQ"` for EQ/BE and for index rows | keep rows where effective `pGroup in ["EQ","BE",""]` (index rows get `pGroup=""`) | `:138-186` |
| `BSE_CM.csv` | `pSymbol` | `pDesc` | `pSymbolName` | `pTrdSymbol` | **`"BSE"`** | `"BSE"`, or `"BSE_INDEX"` when `pISIN` NaN | `pExpiryDate` | `dStrikePrice` raw | `lLotSize` | `dTickSize`/100 | `"EQ"` | drop rows with NaN `pSymbolName`; otherwise all rows kept | `:189-237` |
| `NSE_FO.csv` | `pSymbol` | `pSymbolName` | built (§6.4) | `pTrdSymbol` | `pExchSeg` (`nse_fo`) | `"NFO"` | `lExpiryDate + 315513000` -> unix s -> `%d-%b-%y` upper (e.g. `25-SEP-26`) | `dStrikePrice / 100` (int if integral) | `lLotSize` | `dTickSize`/100 | `pOptionType` with `XX`->`FUT`, stripped | `_demote_non_expiring` | `:280-318` |
| `CDE_FO.csv` | same as NFO | | | | `pExchSeg` (`cde_fo`) | `"CDS"` | **+315513000 offset** | `/100` | | | same | same | `:475-513` |
| `MCX_FO.csv` | same | | | | `pExchSeg` (`mcx_fo`) | `"MCX"` | **no offset** (`lExpiryDate` as unix s) | `/100` | | | same | `dropna(pOptionType)`, `_demote_non_expiring` | `:516-559` |
| `BSE_FO.csv` | same | | | | `pExchSeg` (`bse_fo`) | `"BFO"` | **no offset** | `/100` | | | same | `dropna(pOptionType)`, `_demote_non_expiring` | `:562-600` |

Index rows are identified solely by **null `pISIN`** (`:161,217`); their `instrumenttype` is `"EQ"` (not `"INDEX"`) and the exchange column (`NSE_INDEX`/`BSE_INDEX`) is what marks them (`:163-168`). Their `symbol` is the raw `pSymbolName` from the CSV; **no NIFTY/BANKNIFTY renaming is performed in the master contract** — the index-name candidate map lives only in the quote and streaming layers (§9.4). Strike is **not** scaled per segment beyond the `/100` for F&O (there is no 1e7 currency scaling in this code).

`_demote_non_expiring` (`:240-266`): if raw `lExpiryDate` (before offset) `<= 0` (MCX uses `0`, CDS uses `-1`), set `expiry = ""` and `instrumenttype = ""` so spot/reference rows do not become `GOLD01JAN70` futures.

### 6.4 Symbol construction for derivatives (`combine_details`, `:269-277`)

```
base = name + expiry.replace("-", "")          # e.g. NIFTY25SEP26
FUT        -> base + "FUT"                     # NIFTY25SEP26FUT
CE / PE    -> base + strike + type             # NIFTY25SEP2624500CE  (strike int if integral)
otherwise  -> base
```

### 6.5 Completion signal

`socketio.emit("master_contract_download", {"status": "success", "message": f"Successfully Downloaded {n} records"})` or `{"status": "error", "message": ...}` (`:661-674`). Temp CSVs deleted afterwards (`:603-611,657`).

---

## 7. History (`broker/kotak/api/data.py`) — implemented

| Item | Value | Citation |
|---|---|---|
| Endpoint | `GET {base}/market-data/1.0/historical/details?neosymbol=<seg>\|<token>&fromdate=YYYY-MM-DD&todate=YYYY-MM-DD&interval=<res>` (pipe left literal via `urlencode(params, safe='|')`) | `:957-968` |
| Headers | `Authorization: <access_token>`, `Content-Type: application/json`; timeout 60 s | `:969,981` |
| `timeframe_map` | `1m->1min, 3m->3min, 5m->5min, 10m->10min, 15m->15min, 30m->30min, 1h->60min, 60m->60min, D->D, W->W` | `:213-227` |
| Segments served | `{"nse_cm","nse_fo","bse_cm","bse_fo"}`; CDS/MCX raise `"Kotak Neo serves historical data for NSE, BSE, NFO, BFO, NSE_INDEX and BSE_INDEX only..."` | `:18,916-920` |
| Segment resolution | `brexchange` mapped if it is an OpenAlgo code, else used as-is, else from `exchange` | `:899-921` |
| neosymbol candidates | `"<seg>|<token>"` first; for `*_INDEX` also each name candidate and its `.upper()` (INDIAVIX answers only to `"INDIA VIX"` here) | `:923-951` |
| Chunking (days per request) | `1min/3min/5min: 30; 10min/15min: 60; 30min/60min: 90; D/W: 180`; `current_end = min(start + chunk-1 days, end)` | `:23-33,1150-1156` |
| Rate limit | 1 request/s global (`HISTORY_RATE_LIMIT_PER_SEC = 1`), lock-reserved slots | `:53-54,105-117` |
| 429 handling | up to 4 retries, backoff `1,2,4,8` s or `Retry-After` | `:94-96,179-187,979-994` |
| Lookback clamp | earliest `fromdate` = IST today − 5 years + 1 day; end before that -> empty frame | `:84-92,141-149,1136-1148` |
| Response | `{"status":"success","data":{"candles":[[ts,o,h,l,c,v,oi],...]}}`; fault body `{"fault":{"message":...}}` or `emsg` | `:1005-1025` |
| "No data" faults (return `[]`, not error) | message contains any of `"no data found"`, `"data not available"`, `"no data is available"`, `"market has not yet opened"` (case-insensitive) | `:127-138,1011-1016` |
| Row normalisation | pad each candle to 7 columns with 0 (oi unpopulated); skip rows shorter than 5 | `:1089-1104` |
| Timestamps | ISO 8601 with `+0530` parsed as UTC epoch; for `D`/`W`: `+5h30m` then floor to day; then `int64 // 10**9` | `:1196-1213` |
| Repair | widen `high`/`low` to include open/close; zero negative volume; drop rows with NaN OHLC; sort + dedupe on timestamp | `:1028-1086,1225-1230` |
| Output columns | `["timestamp","open","high","low","close","volume","oi"]` | `:36` |
| `get_supported_intervals` | `{"seconds": [], "minutes": [1m,3m,5m,10m,15m,30m,60m], "hours": [1h], "days": ["D"], "weeks": ["W"], "months": []}` | `:1238-1248` |

---

## 8. WebSocket streaming

### 8.1 Feed host selection (`kotak_feed_config.py`, `kotak_adapter.py:80-106`)

1. `data_center` = 5th auth part. Empty -> default `wss://sfeed.kotaksecurities.com/apifeed` (`kotak_feed_config.py:32,87-92`).
2. `GET https://lapi.kotaksecurities.com/5config/config?appVersion=1.0.0&platform=api&environment=prod` (timeout 5). `source = configs["<DC>_broadcast_source"]` (`"hs"` HSM, `"sh"` SFeed, `"ks"` cdtstream). `market_data_url = to_wss(configs["<DC>_<source>_broadcast_endpoint"])`, `order_feed_url = to_wss(configs["<DC>_<source>_interactive_endpoint"])` (the latter is computed but **not used** by the order adapter, which derives its URL from `baseUrl`). `https://`->`wss://`, bare host -> `wss://` prefix (`:48-64`).
3. `source == "hs"` -> error log, fall back to SFeed default anyway (`:143-154`). Adapter: if `source == "hs"` build `KotakWebSocket` (legacy HSM) else `KotakSFeedWebSocket(auth_config, ws_url, ucc=BROKER_API_KEY)` (`kotak_adapter.py:92-106`). Both clients expose `connect/close/subscribe_batch/unsubscribe_batch/set_callbacks/is_connected/wait_until_closed` and emit identical normalised dicts.

### 8.2 SFeed (current) — `broker/kotak/streaming/sfeed_websocket.py` + `sfeed_protocol.py`

**Transport**: `websocket.WebSocketApp(url).run_forever(ping_interval=30, ping_timeout=10)` (`sfeed_websocket.py:170-177`).

**Auth frame** (JSON text, sent on open, `:333-361`):

```json
{"user": "<UCC or 'neome'>", "auth": "<trading_sid>", "format": "native_batch",
 "source": "NEOTRADEAPI", "platform": "Web", "version": "1.2.3", "sdk_version": 2,
 "sdk_date": "2026-08-07T09:41:17.667Z", "conn_req_time": <epoch ms int>, "sessionValidation": false}
```

Note: SFeed authenticates with the **sid**, not the trading token (`:79-83`). Auth timeout 15 s -> close socket so the adapter reconnects (`:68,373-388`).

**Auth response** (JSON text): `message_code` in `(1117, 1119)` (`sfeed_protocol.py:45`). Body: `"exchanges": {"<name>": {"value": <exchange_id>, "divider": <int>}}` -> dividers map keyed by `value` (fallback to static `EXCHANGE_NAME_TO_ID`), default divider 100 (`sfeed_websocket.py:434-443`). `"format": "native_fallback"` means the server refused `native_batch`; the client logs an error and does not decode (`:421-432`). After auth, queued control frames are flushed and `on_open` fires (`:445-462`).

**Subscribe / unsubscribe control frames** (JSON text, `:264-273,300-302`):

```json
{"event": "subscribeScrips",  "inputtoken": "nse_cm|11536,nse_cm|1594", "ack_symbol": true}
{"event": "subscribeDepth",   "inputtoken": "...", "ack_symbol": true}
{"event": "subscribeIndices", "inputtoken": "nse_cm|Nifty 50", "ack_symbol": true}
{"event": "unsubscribeScrips"|"unsubscribeDepth"|"unsubscribeIndices", "inputtoken": "..."}
```

Adapter-side sub_types map `mws->subscribeScrips`, `dps->subscribeDepth`, `ifs->subscribeIndices`, `mwu/dpu/ifu` -> the unsubscribes (`:48-60`). `channelnum` is accepted and ignored (`:232-236`). Cap: 3000 total subscribed tokens, enforced client-side (`:66,252-262`); `MAX_BATCH_SIZE = 3000` (one frame) (`:92-97`). Frames sent before auth are queued (max 256, oldest dropped) and cleared on close (`:73,308-324,495-511`).

**Subscribe ack** (JSON text): `message_code == 1109` with `"trading_symbols"` (or `"tradingSymbols"`) `{ "<exchange>|<token>": "<trading symbol>" }`; stored to attach `ts` to ticks (`sfeed_websocket.py:464-471`, `sfeed_protocol.py:46-48`).

**Binary frames** (`sfeed_protocol.py`): little-endian packed. One frame = several packets, each prefixed by its own `u16` length; scan stops on a truncated tail (`split_batch`, `:127-144`). Header `struct "<HHbBBBB"` = `message_length u16, message_code u16, exchange_id i8, level u8, auction_flag u8, seq_no u8, bitmask_length u8` (9 bytes, `:106`). Exchange ids: `none 0, nse_cm 1, nse_fo 2, cde_fo 3, nse_com 4, bse_cm 5, bse_fo 6, bse_cd 7, bse_co 8, mcx_fo 9, ncd_co 10` (`:69-81`). Routing (`:185-211`): code `6511`/`6521` -> market open/close (header only); `7207` -> index; `105` -> market status with body `"<H5s"`; `104` -> CAS `"<IIqq"` (all-zero dropped); else by `level`: `1` -> mini touch line; `2,4,8,16` -> market picture (4 = touch line, 8 = depth, 16 = full depth). Prices = integer / exchange divider; percent fields / 100 (`:243-246`).

Body structs at offset 9:
- Index `"<IiiiiiQiiidBi21s"` = `token, open, close, high, low, index_value, last_trade_time(u64), yearly_high, yearly_low, net_chg_percent, market_cap(f64), precision(u8), multiplier(i32), name[21]` (`:110,214-251`).
- Mini `"<IqIqIiiIBI"` = `token, last_trade_time(i64), ltp, last_trade_qty(i64), close, net_chg_percent, net_chg, market_lot, precision(u8), multiplier` (`:113,301-328`).
- Market picture `"<IqqqqqIIIIIqIIIIhiIdiIIIIIBI"` (135 bytes, ends at offset 144) = `token, total_buy_qty, total_sell_qty, volume_traded_today, last_trade_time, last_update_time, open, close, high, low, ltp, last_trade_qty(i64), avg_trade_price, indicative_close, buy_depth_count, sell_depth_count, trading_status(i16), net_chg_percent(i32), open_interest, total_traded_value(f64), net_chg(i32), upper_circuit, lower_circuit, yearly_high, yearly_low, market_lot, precision(u8), multiplier`; followed by `buy_n + sell_n` depth rows `"<qii"` = `quantity(i64), price(i32), orders(i32)` of 16 bytes; touch line forces 1+1 rows (`:117-121,331-420`). Negative `last_update_time` -> 0 (`:363-368`).

**Normalised dicts emitted by the SFeed client** (HSM field names kept for the adapter, `sfeed_websocket.py:567-644`):
- Quote (`scrip`, level 1/2/4): `{"bid": buy[0].price, "ask": sell[0].price, "open", "high", "low", "ltp", "prev_close": close_price, "volume": volume_traded_today, "ts": <symbol from ack>, "tk": token, "e": exchange_segment}`.
- Lite (`scrip_lite`): same keys with bid/ask/open/high/low/volume = 0 (0 means "unchanged" in the adapter's merge contract).
- Depth (`scrip`, level 8/16): quote dict + `"bids"`/`"asks"` lists of `{"price","quantity","orders"}` truncated and **padded to 5** (`:610-629`). Note: no `totalbuyqty`/`totalsellqty` keys are produced by the SFeed depth path (the adapter defaults them to 0, `kotak_adapter.py:489-490`).
- Index: `{"bid":0,"ask":0,"open","high","low","ltp": index_value,"prev_close": close,"volume":0,"ts": <index name from packet>,"tk","e"}`, delivered on the quote callback (adapter sets no `on_index`) (`:536-543,631-644`).
- `market_status` and `cas` dicts are logged only (`:546-560`).

### 8.3 Legacy HSM — `kotak_websocket.py` + `HSWebSocketLib.py` (used only when `source == "hs"`)

- URL default `wss://mlhsm.kotaksecurities.com` (`kotak_websocket.py:31`); `run_forever(ping_interval=30, ping_timeout=10)` (`HSWebSocketLib.py:1158-1159`).
- Control API is JSON dicts converted to **big-endian binary** frames by `hs_send` (`HSWebSocketLib.py:1247-1333`, sent as opcode `0x2`):
  - Connect: `{"type":"cn","Authorization":<trading_token>,"Sid":<trading_sid>}` (`kotak_websocket.py:256-266`) -> `prepareConnectionRequest2(jwt, sid)`: `[len u16][type=1][fieldCount=3][fid=1][len u16][jwt][fid=2][len u16][sid][fid=3][len u16]"JS_API"` (`HSWebSocketLib.py:446-469`).
  - Subscribe/unsubscribe: `{"type":"mws"|"mwu"|"ifs"|"ifu"|"dps"|"dpu","scrips":"nse_cm|11536&nse_cm|1594","channelnum":"1"}` (scrips `&`-joined, max 100 per frame, `kotak_websocket.py:175,207`; `HSWebSocketLib.py:16,471-477`). Binary: `[len u16][type 4|5][fieldCount=2][fid=1][len u16][scripCount u16][(len u8)(prefix|scrip)...][fid=2][len u16=1][channelnum u8]` with prefix `sf` (scrip), `if` (index), `dp` (depth) (`HSWebSocketLib.py:479-527,1213-1215`).
  - Also defined: snapshot `mwsp/dpsp/ifsp` (type 9), channel pause/resume `cp/cr` (types 7/8, 64-bit channel mask), throttling `ti` (type 2), option-chain `opc` (type 10) (`HSWebSocketLib.py:529-669,1307-1328`).
- Responses are binary, parsed to JSON arrays (`HSWebSocketLib.py:833-1124`): `[u16 packetsCount][u8 type]...`. Type 1 (connection) -> `[{"stat":"Ok"|"NotOk","type":"cn","msg":...,"stCode":200|11001}]` and stores `ack_num` (every `ack_num`-th data message must be acknowledged with `get_acknowledgement_req(msg_num)` = `[len][type=3][1][1][len u16=4][msg_num u32]`, `:416-426,906-925`). Type 6 (data): optional `u32 msg_num`, then `u16 count` of sub-messages each `[u16 len][u8 respType 83 SNAP|85 UPDATE]`; SNAP = `[u32 topicId][u8 nameLen][name "sf|nse_cm|11536"][u8 fcount][fcount × i32 values][u8 scount][(u8 fid)(u8 len)(str)...]`; UPDATE = `[u32 topicId][u8 fcount][fcount × i32]` (`:926-1023`). Types 4/5/9/7/8/10 are status acks `{"stat","type":"sub"|"unsub"|"snap"|"cp"|"cr"|"opc","msg","stCode"}`.
- Field indices -> JSON keys (`HSWebSocketLib.py:121-211`): scrip `0 ftm0,1 dtm1,2 fdtm,3 ltt,4 v,5 ltp,6 ltq,7 tbq,8 tsq,9 bp,10 sp,11 bq,12 bs,13 ap,14 lo,15 h,16 lcl,17 ucl,18 yh,19 yl,20 op,21 c,22 oi,23 mul,24 prec,25 cng,26 nc,27 to`; strings `51 name,52 tk,53 e,54 ts`. Depth `2-6 bp,bp1..bp4; 7-11 sp,sp1..sp4; 12-16 bq..bq4; 17-21 bs..bs4; 22-26 bno1..bno5; 27-31 sno1..sno5; 32 mul; 33 prec`. Index `2 iv,3 ic,4 tvalue,5 highPrice,6 lowPrice,7 openingPrice,8 mul,9 prec,10 cng,11 nc`.
- Scaling: FLOAT32 fields = `raw / (mul * 10**prec)` with `mul`/`prec` taken from the same tick (defaults 1 and 2) (`:696-702,769-776,790-794`); sentinel `0x80000000` (= `2147483648`, decoded as `21474836.48` after /100) means "not available" and is skipped (`:18-27,348-353`). Dates formatted by `getFormatDate`.
- `kotak_websocket.py` parses the JSON arrays: `{"type":"cn","stat":"Ok"}` -> authenticated, flush queue (`:338-360`); `name == "dp"` -> depth (`:369-375`); `name == "if"` -> index (`:376-382`); anything with `ltp`/`bp`/`sp`/`op`/`h`/`lo`/`c`/`v` -> quote (`:383-389`). Quote dict: `bid=bp, ask=sp, open=op, high=h, low=lo, ltp, prev_close=c, volume=v, ts, tk, e` (`:451-463`). Depth dict: levels from `bp/bp1..bp4`, `bq/bq1..`, `sp/sp1..`, `bs/bs1..`, orders `bno1..5`/`sno1..5`, skipping price `21474836.48`, padded to 5, plus `totalbuyqty`, `totalsellqty`, `ltp`, `ltq`, `volume=v`, `open=op`, `high=h`, `low=lo`, `prev_close=c`, `oi`, `ts`, `tk`, `e` (`:524-586`). Index dict: `ltp=iv, prev_close=ic, timestamp=tvalue, high=highPrice, low=lowPrice, open=openingPrice, mul, prec, cng, nc, tk, e` (`:602-617`). Partial-update merging keeps last known non-zero fields per `"<e>|<tk>"` (`:391-469`).

### 8.4 Adapter behaviour (`kotak_adapter.py`) — what the Rust port must reproduce for the ZMQ side

- `initialize(broker, user_id)`: closes any live client, loads auth from DB (`bypass_cache=True`), splits `:::`, builds client per §8.1, sets `_max_batch_size = client.MAX_BATCH_SIZE` (100 HSM / 3000 SFeed) (`:165-210`).
- Subscribe modes: `1` LTP, `2` Quote, `3` Depth (the adapter's own numbering; `base_adapter.py:237-245` docstring mentions `4` for depth but kotak uses `3`) (`:1071-1086,511`). Mode 3 subscribes **both** `dps` and `mws` because Kotak sends depth and LTP as separate streams (`:1074-1081`). Mode 1/2 -> `mws`.
- Index exchanges (`*_INDEX`): subscribe via `ifs` using **only the first name candidate** (e.g. `nse_cm|Nifty 50`), but register every candidate **and** the master-contract token as inbound aliases sharing one mode set, because the index tick identifies itself by name (`ts`) or by a Kotak-chosen token (NIFTY answers with `tk 4247863880`, not 26000) (`:291-302,668-694,1204-1206,1312-1316`).
- Non-index key: `(kotak_exchange, str(token))` where `kotak_exchange = kotak_mapping.get_kotak_exchange(exchange)` and `token = get_token(symbol, exchange)` (`:1169-1181`).
- Batching: 50 ms debounce; last op per `(exchange, token, channel, family)` wins; grouped per `(sub_type, channelnum)`; chunked by `_max_batch_size` (`:149-163,696-759`).
- Unsubscribe from broker only when no LTP/Quote modes remain (for `mwu`) or no Depth mode remains (for `dpu`) (`:1237-1270,1346-1375`).
- Partial-update handling: a tick is "partial" when `ltp <= 0 or no ts` and none of `open/high/low/prev_close` is non-zero; zero price fields never overwrite stored state; depth levels merge per level (`:313-460,634-655`). Invalid sentinels: price `21474836.48`, volume `2147483648` (`:343,350`).
- Publish topics: `f"{exchange}_{symbol}_{LTP|QUOTE|DEPTH}"` (`:511-513`). Payloads (`:515-616`), all with `symbol`, `exchange`, `timestamp` (epoch ms) appended:
  - LTP (mode 1, only when ltp > 0): `{"ltp": float, "ltt": <timestamp or now ms>}`.
  - QUOTE (mode 2, always): `{"ltp": effective_ltp, "ltt", "volume", "open", "high", "low", "close": prev_close}`.
  - DEPTH (mode 3): `{"timestamp", "ltp": effective_ltp, "depth": {"buy": [...], "sell": [...]}, "totalbuyqty", "totalsellqty"}`; for an index with no ltp -> skipped; index with ltp -> empty ladder; uses cached depth when the packet carries only LTP (`:535-607`).
- Reconnect: on close, exponential `5 * 2**n` capped 60 s, max 10 attempts; on attempt, close old client, re-read auth from DB, rebuild client, re-subscribe all saved `(symbol, exchange, mode)` (`:127-134,837-984`).
- Polled getters `get_ltp/get_quote/get_depth` return nested `{exchange: {symbol: {...}}}` (`:1380-1483`).
- ZMQ publish: `socket.send_multipart([topic.encode(), json.dumps(data).encode()])` to `tcp://ZMQ_HOST:ZMQ_PORT` (default `127.0.0.1:5555`) (`websocket_proxy/base_adapter.py:206-222,388-408`). Success/error response shapes `{"status":"success","message":...}` / `{"status":"error","code":...,"message":...}` (`base_adapter.py:410-422`).

### 8.5 Order-update feed — `kotak_order_adapter.py`

| Item | Value | Citation |
|---|---|---|
| URL | `wss://<host of baseUrl>/realtime` (scheme swapped from the stored `baseUrl`) | `:104-117,133-134` |
| Handshake headers | none | `:136-137` |
| Connect frame (first try) | JSON `{"type":"cn","Authorization":"<trading_token>","Sid":"<trading_sid>","src":"WEB"}` | `:150-160` |
| Connect frame (alternate) | raw string `{type:cn,Authorization:<token>,Sid:<sid>,src:WEB}`; encoding flips on every attempt that opened without an ack | `:142-167` |
| Ack | `{"ak":"ok","type":"cn","msg":"connected"}`; `ak == "ok"` sets acked | `:15,226-228` |
| Ack watchdog | 10 s, closes socket if no ack and no data | `:57,170-201` |
| WS ping | protocol ping every 30 s (`ping_timeout` = min(10, interval-1) from base) | `:52,139-140`; `order_adapter.py:310-313` |
| Reconnect | base schedule `[1,2,5,10,30,60]` s; 401/403 handshake -> stand down until next login | `order_adapter.py:12,15-23,195-240` |
| Frames | JSON `{"type":"order","data":{...}}` published; `{"type":"position",...}` ignored; non-JSON text logged | `:203-233` |
| Data fields read | `nOrdNo`, `ordSt`, `exSeg`, `trdSym`/`sym`, `tok`, `qty`, `fldQty`, `unFldSz`, `prcTp`, `trnsTp`, `prc`, `trgPrc`, `avgPrc`, `prod`, `rejRsn` | `:234-284` |
| Status map | §5.2 list; unknown -> raw lower-case or `"open"` | `:61-78,239-240` |
| Pricetype map | `MKT/MARKET->MARKET`, `L/LMT/LIMIT->LIMIT`, `SL`, `SL-M` | `:83-91` |
| Action | `B->BUY`, `S->SELL` | `:93` |
| Output | `{"orderid","symbol","exchange","action","quantity","price","trigger_price","pricetype","product","order_status","filled_quantity","pending_quantity","average_price","rejection_reason"}` | `:269-284` |

Symbol: `to_openalgo_symbol(trdSym or sym, map_exchange(exSeg) or exSeg, token=tok)` -> `get_symbol(token, exchange)` then `get_oa_symbol(brsymbol, exchange)` then raw (`order_adapter.py:26-49`). `pending_quantity = unFldSz` or `max(qty - fldQty, 0)` (`:256`).

---

## 9. Rate limits, batch sizes, sleeps, timeouts (all literal)

| Where | Constant | Citation |
|---|---|---|
| Quotes concurrency | `QUOTES_MAX_INFLIGHT = 4` (module-level semaphore; Neo rejects on concurrency, not rate) | `data.py:56-70,81,298` |
| Quotes 429 retry | `QUOTES_MAX_RETRIES = 3`, backoff `0.5 * 2**attempt` or `Retry-After` | `data.py:71-72,168-176,297-312` |
| Quotes timeout | `QUOTES_TIMEOUT = 15.0` | `data.py:77` |
| Multi-quote batch | `BATCH_SIZE = 25`, `RATE_LIMIT_DELAY = 0.2` s between batches | `data.py:644-645,681-682` |
| History pacing | 1 req/s (`HISTORY_MIN_INTERVAL = 1.0`), 4 retries backoff 1/2/4/8, timeout 60 | `data.py:53-54,95-96,981` |
| History chunk sizes | see §7 | `data.py:23-33` |
| Master download | timeout 30 per file; file-paths timeout 30; fallback probe timeout 10 | `master_contract_db.py:112,358,454` |
| Feed config | timeout 5 s | `kotak_feed_config.py:40` |
| SFeed | auth timeout 15 s; pending-frame cap 256; subscription cap 3000; ping 30/10 | `sfeed_websocket.py:66-73,177` |
| HSM | 100 scrips per frame; topic cache cap 5000; ping 30/10 | `HSWebSocketLib.py:16,342,1158`; `kotak_websocket.py:29` |
| Adapter batching | 50 ms debounce | `kotak_adapter.py:158` |
| Adapter reconnect | base 5 s, `*2**attempt`, cap 60 s, max 10 attempts | `kotak_adapter.py:131-134,856-859` |
| Order feed | ping 30 s; ack timeout 10 s; reconnect `[1,2,5,10,30,60]` | `kotak_order_adapter.py:52,57`; `order_adapter.py:12` |
| Smart order position cache | 1 s TTL, per-symbol lock | `order_api.py:169-181` |

---

## 10. Quirks a port must preserve

1. **No `hsServerId`, no `sId=` query, no Bearer/OAuth** — two session credentials only: `Auth` (token) and `Sid`, plus the static access token for market-data/scrip-master endpoints (§1, §2). The adapter key literally named `hs_server_id` contains the `baseUrl` and is never read (`kotak_adapter.py:199`).
2. **Dynamic `baseUrl`** from MPIN validate is mandatory for every trading call, quotes and history; scrip-master additionally tries `https://cis.kotaksecurities.com` and `https://neo-gw.kotaksecurities.com` (`master_contract_db.py:334-338`).
3. **`pc` is the raw OpenAlgo product** (`"MIS"/"CNC"/"NRML"`), never mapped (`transform_data.py:163`). `map_product_type` is only used to match positions (`order_api.py:323`).
4. **Zero prices must be `"0"`, not `"0.0"`** (`transform_data.py:132-140`). All jData values are strings.
5. **`ig: "openalgo"` is mandatory on place**; it comes back as `GuiOrdId` (`transform_data.py:143-145`).
6. **SL-M is never sent**; it becomes `SL` with a tick-snapped protective limit (§4.1).
7. **Depth padding to 5** in REST depth (`data.py:582-586`), SFeed depth (`sfeed_websocket.py:627-629`), HSM depth (`kotak_websocket.py:564-568`), and adapter merge (`kotak_adapter.py:369-420`). REST depth levels have `{"price","quantity"}` only; streaming levels add `"orders"`.
8. **Quote key is composite** `"<segment>|<pSymbol>"`; `token` column = `pSymbol`; `brexchange` is **`"NSE"`/`"BSE"` for cash rows** but `nse_fo`/`bse_fo`/`cde_fo`/`mcx_fo` for F&O rows, so map only when it is one of `["NSE","BSE","NFO","BFO","CDS","MCX"]` (`data.py:413-417`, `master_contract_db.py:178,229,308`).
9. **Index quote/stream names** (`data.py:258-273`, identical copy at `kotak_adapter.py:29-44`; a test fails if they drift):

   | OpenAlgo | Candidates (priority order) |
   |---|---|
   | `NIFTY`, `NIFTY50` | `["Nifty 50"]` |
   | `BANKNIFTY` | `["Nifty Bank"]` |
   | `FINNIFTY` | `["Nifty Fin Service"]` |
   | `MIDCPNIFTY` | `["Nifty Mid Select", "Nifty Midcap Sel", "Nifty Midcap Select", "NIFTY MID SELECT"]` |
   | `NIFTYNXT50` | `["Nifty Next 50"]` |
   | `INDIAVIX` | `["India VIX"]` (history additionally tries `"INDIA VIX"`) |
   | `SENSEX` | `["SENSEX"]` |
   | `BANKEX` | `["BANKEX"]` |
   | anything else | `[symbol]` |

   Index exchange -> segment: `NSE_INDEX`->`nse_cm`, `BSE_INDEX`->`bse_cm` (`data.py:238-239`). Quote probes candidates sequentially; stream subscribes only the first.
10. **`tok` is empty on tradebook rows**; always fall back to `trdSym` lookup (`order_data.py:12-17`).
11. **Positions carry no LTP/P&L**; LTP is back-filled via multi-quote (`order_api.py:65-140`) and P&L computed per Kotak's formula with carry-forward quirks (§5.4).
12. **Funds**: `CollateralValue` is cash, `Collateral` is pledged margin (`funds.py:78-86`).
13. **Master contract**: NSE cash CSV has dirty headers `"dStrikePrice;"`, `"dTickSize "`; cash `strike` is not `/100`, F&O strikes are; NFO/CDS expiry epoch needs `+315513000`, MCX/BFO does not; `lExpiryDate <= 0` means no expiry; global token de-dup across exchanges; `bcs_fo` (BCD) is never downloaded; `NSE_COM` is downloaded but unprocessed (§6).
14. **History exists** (contrary to the skill doc) only for `nse_cm/nse_fo/bse_cm/bse_fo`, 1 req/s, 5-year horizon, "no data" faults are empty results (§7).
15. **Three market-data protocols**: SFeed (little-endian batched, divider from auth response, symbols from subscribe ack, auth by `sid` + UCC) is the default; cdtstream (`"ks"`) shares the SFeed protocol; HSM (big-endian, per-tick `mul`/`prec`, `0x80000000` sentinel, auth by token+sid) only when the config service says `"hs"` (§8).
16. **Order feed**: `wss://<baseUrl host>/realtime`, JSON `cn` frame with `src: "WEB"`, alternates to a raw non-JSON string if unacked; `position` frames discarded (§8.5).
17. **Mode numbering in the kotak adapter is 1/2/3** (LTP/QUOTE/DEPTH); depth mode also subscribes the quote stream.
18. **Streaming exchange map has `BCD -> "bcs-fo"` (hyphen)** while REST uses `bcs_fo`; BCD is not in `supported_exchanges` anyway.
19. `map_order_type` default `"MARKET"` (unknown pricetype) is not a valid Kotak `pt`; Kotak will reject it — preserve or validate upstream.
20. Login endpoints are on `https://mis.kotaksecurities.com`, not on `baseUrl` (`auth_api.py:80,116`).

---

## B. GROWW broker — literal implementation spec (port target: Rust)

Source of truth: `/Users/openalgo/openalgo-desktop/openalgo/broker/groww/` as read on 2026-10-03. Every fact below cites `file:line`. Paths are relative to `/Users/openalgo/openalgo-desktop/openalgo/` unless absolute. Where the Python implementation is buggy or inconsistent the spec says so explicitly (section 9) rather than silently "fixing" it, so the porter can decide.

Files read in full (line counts): `broker/groww/plugin.json` (11), `api/auth_api.py` (113), `api/data.py` (2446), `api/funds.py` (110), `api/margin_api.py` (103), `api/order_api.py` (3518), `database/master_contract_db.py` (891), `mapping/margin_data.py` (138), `mapping/order_data.py` (1030), `mapping/transform_data.py` (245), `streaming/__init__.py` (8), `streaming/groww_adapter.py` (1224), `streaming/groww_mapping.py` (173), `streaming/groww_nats.py` (424), `streaming/groww_nkeys.py` (489), `streaming/groww_protobuf.py` (328), `streaming/nats_websocket.py` (1060). There is **no** `baseurl.py`, no `.proto` file and no generated `*_pb2.py`; the protobuf is hand-decoded in `groww_protobuf.py`. No Groww API docs are vendored in the repo (a comment at `websocket_proxy/order_adapter.py:379-381` refers to "broker-api-docs groww docs" which do not exist in this tree).

---

## 0. Plugin metadata and platform wiring

| Fact | Value | Cite |
| --- | --- | --- |
| Plugin name | `"groww"` | `plugin.json:2` |
| supported_exchanges | `["NSE", "BSE", "NFO", "BFO", "NSE_INDEX", "BSE_INDEX"]` | `plugin.json:8` |
| broker_type | `"IN_stock"` | `plugin.json:9` |
| leverage_config | `false` | `plugin.json:10` |
| Frontend login entry | listed as `{ id: 'groww', name: 'Groww', authType: 'totp' }` (label only) | `frontend/src/pages/BrokerSelect.tsx:34` |
| Frontend login URL | `/${selectedBroker}/callback` i.e. `GET /groww/callback` (no external redirect, no form) | `frontend/src/pages/BrokerSelect.tsx:149,159` |
| Tauri label | `("groww", "Groww", "totp")` | `/Users/openalgo/openalgo-desktop/openalgo-desktop/src-tauri/src/commands/settings.rs:243` |
| Order updates | REST orderbook polling, no push feed: `_POLLING_BROKERS = {"groww", "fivepaisa", "samco"}` | `services/order_update_service.py:82,98` |
| Env keys | `BROKER_API_KEY`, `BROKER_API_SECRET` — read raw, no `:::` composition for groww | `utils/config.py:11-28`; `api/auth_api.py:100-101` |
| Skill family | classified under "OAuth2 / checksum redirect" family | `.claude/skills/broker-integration/SKILL.md:44` |
| Doc auth family | "OAuth2 (simple)" returning `(auth_token, error_message)` | `docs/broker-integration-guide.md:119` |

Service-layer call order (what the platform does with the broker functions):

| Service | Calls, in order | Cite |
| --- | --- | --- |
| orderbook | `get_order_book(auth)` → `map_order_data(order_data=)` → `calculate_order_statistics(...)` → `transform_order_data(...)` | `services/orderbook_service.py:149-164` |
| positions | `get_positions(auth)` → `map_position_data` → `transform_positions_data` | `services/positionbook_service.py:124-138` |
| tradebook | `get_trade_book(auth)` → `map_trade_data(trade_data=)` → `transform_tradebook_data` | `services/tradebook_service.py:113-127` |
| holdings | `get_holdings(auth)` → `map_portfolio_data` → `calculate_portfolio_statistics` → `transform_holdings_data` | `services/holdings_service.py:109-124` |
| funds | `get_margin_data(auth)` | `services/funds_service.py:75` |
| margin | `calculate_margin_api(positions, auth)` → `(response, response_data)` | `services/margin_service.py:194` |
| quotes | `BrokerData(auth_token).get_quotes(symbol, exchange)` | `services/quotes_service.py:134-139` |
| multiquotes | `BrokerData(auth_token).get_multiquotes(clean_symbols)` | `services/quotes_service.py:311` |
| depth | `BrokerData(auth_token).get_depth(symbol, exchange)` | `services/depth_service.py:104-109` |
| history | `BrokerData(auth_token).get_history(symbol, exchange, interval, start_date, end_date)` | `services/history_service.py:153` |
| intervals | `BrokerData(auth_token).timeframe_map` keys, filtered | `services/intervals_service.py:51,72` |

---

## 1. Authentication

### 1.1 What is implemented: approval / checksum flow ONLY

Only ONE variant exists in code. **TOTP flow (`key_type: "totp"`) is NOT implemented. Direct access-token paste is NOT implemented.** `authenticate_broker(code)` ignores `code` entirely (`api/auth_api.py:88-110`).

Credentials the user supplies: `.env` `BROKER_API_KEY` (Groww API key) and `BROKER_API_SECRET` (Groww API secret) (`api/auth_api.py:100-101`; `.sample.env:10-11`). Both are required or the function returns the error string `"BROKER_API_KEY and BROKER_API_SECRET environment variables are required for Groww authentication"` (`api/auth_api.py:103-107`).

Algorithm (`api/auth_api.py:29-85`):

1. `timestamp = str(int(time.time()))` — Unix epoch **seconds**, decimal string (`:43`).
2. `checksum = sha256( (api_secret + timestamp).encode("utf-8") ).hexdigest()` — lowercase hex (`:11-26`, `:46`).
3. `POST https://api.groww.in/v1/token/api/access` (`:58`), timeout 30 s (`:61`).
   - Headers (`:52`):
     ```json
     {"Authorization": "Bearer <BROKER_API_KEY>", "Content-Type": "application/json"}
     ```
   - Body (`:55`):
     ```json
     {"key_type": "approval", "checksum": "<sha256 hex>", "timestamp": "<epoch seconds string>"}
     ```
4. Response handling (`:63-79`):
   - HTTP 200 and JSON contains `"token"` → return `(response_data["token"], None)` (`:67-68`).
   - HTTP 200 without `"token"` → `(None, "Authentication succeeded but no token found in response: {response_data}")` (`:70-73`).
   - Non-200 → `(None, "HTTP error {status}: {json or text}")` (`:75-79`).
   - Exception → `(None, "Request failed: {e}")` (`:81-82`).

Only the `token` field is consumed. No expiry field is read; token lifetime is not parsed anywhere. The stored token is the raw Groww token string with **no prefix or composition** (contrast zerodha which prepends api key at `blueprints/brlogin.py:1040-1041`). The platform assumption is that Indian broker tokens expire daily ~3:00 AM IST (`CLAUDE.md` "Security and Deployment Model"; `streaming/groww_adapter.py:150-152` comment "tokens roll daily ~3 AM IST").

### 1.2 Login route handling

`blueprints/brlogin.py:567-571`:
```python
elif broker == "groww":
    code = "groww"
    auth_token, error_message = auth_function(code)
    forward_url = "broker.html"
```
`auth_function` is `app.broker_auth_functions["groww_auth"]` → `broker.groww.api.auth_api.authenticate_broker` (`blueprints/brlogin.py:64-68`). On success it falls to the generic branch `handle_auth_success(auth_token, session["user"], broker, feed_token=None)` (`blueprints/brlogin.py:1073`) which calls `upsert_auth(user, auth_token, "groww", feed_token=None, user_id=None)` (`utils/auth_utils.py:516-518`) and triggers the master-contract download if needed (`utils/auth_utils.py:524-530`). No form fields are posted for groww; `GET /groww/callback` is sufficient. No `feed_token`, no `user_id`.

### 1.3 Token reuse by streaming

The streaming adapter re-reads the token from the DB with `get_auth_token(user_id, bypass_cache=True)` at init (`streaming/groww_adapter.py:137`) and before every reconnect via `token_provider=self._fetch_fresh_token` (`:157`, `:162-168`; `streaming/nats_websocket.py:172-187`, `:518`).

---

## 2. Base URL and headers

Base URL: `https://api.groww.in` (`api/order_api.py:62`; `api/margin_api.py:10`; `api/data.py:54`; `api/auth_api.py:58`). All REST paths below are under `/v1/...`.

Header sets actually sent (they differ per call; port exactly or unify at your discretion):

| Call family | Headers | Cite |
| --- | --- | --- |
| Token exchange | `Authorization: Bearer <api_key>`, `Content-Type: application/json` | `api/auth_api.py:52` |
| Orders (list/create/modify/cancel/trades), positions, holdings (first, shadowed def) | `Authorization: Bearer <token>`, `Accept: application/json`, `Content-Type: application/json` | `api/order_api.py:83-87, 887-891, 1360-1364, 1748-1752, 2525-2529, 2837-2841, 3361-3365` |
| Holdings (effective def) | `Accept: application/json`, `Authorization: Bearer <token>`, `X-API-VERSION: 1.0` | `api/order_api.py:2201-2205` |
| Funds | `Accept: application/json`, `Authorization: Bearer <token>` | `api/funds.py:23` |
| Margin calc | `Authorization`, `Accept`, `Content-Type`, `X-API-VERSION: 1.0` | `api/margin_api.py:54-59` |
| Live data / history (`get_api_response`) | `Accept: application/json`, `Content-Type: application/json`, `Authorization: Bearer <token>` | `api/data.py:58-62` |
| Socket token create | see section 7.1 | `streaming/nats_websocket.py:201-209` |

HTTP client: shared `httpx` client with default timeout `REQUEST_TIMEOUT_SECONDS = 120.0` (`utils/httpx_client.py:23`); explicit `timeout=30` on positions/cancel/trades (`api/order_api.py:910, 1121, 2567, 3384`), `timeout=10.0` on effective holdings via a throwaway `httpx.Client()` (`api/order_api.py:2210-2215`), `timeout=30` on token exchange (`api/auth_api.py:61`), `timeout=15` (requests) on socket token (`streaming/nats_websocket.py:213`).

Generic GET/POST helper `get_api_response(endpoint, auth_token, method, params, data, debug)` (`api/data.py:27-103`): prefixes `/` if missing (`:50-51`), uses `params` for GET and `json=data` for POST (`:67-69`), `raise_for_status()` (`:84`), returns parsed JSON; on `HTTPStatusError` returns `{"error": "HTTP error {status}", "details": <response text>}` (`:96-98`); on other exception `{"error": str(e)}` (`:99-103`); non-JSON body → `{"error": "Response is not valid JSON", "content": text}` (`:92-95`).

Groww envelope: every successful REST response is `{"status": "SUCCESS", "payload": {...}}` (checked at e.g. `api/order_api.py:123, 928, 1790, 2874, 3395`; `api/funds.py:41`; `api/data.py:419, 1229, 1738, 2124`). Error bodies carry `message`, `mode`, `details` (`api/order_api.py:1814-1816`) or `error.message` (`api/order_api.py:3476`) or `errors[0].message` (`mapping/margin_data.py:115-117`).

---

## 3. REST endpoints — request and response contract

### 3.1 Order list — `GET /v1/order/list`

`api/order_api.py:63, 70-378`.

- Query params per page (`:107`): `segment` ∈ {`CASH`, `FNO`} (`:93`), `page` starting at 0 (`:96`), `page_size = 25` (comment: "Maximum allowed by Groww API", `:97`).
- Pagination loop (`:104-157`): for each segment, fetch page; stop when `status != "SUCCESS"` or `payload.order_list` empty (`:123-129`), or when `len(order_list) < page_size` (`:145-149`); else `page += 1`. Any exception breaks the loop for that segment (`:153-157`). Orders from both segments are concatenated.
- Response consumed: `payload.order_list[]` with fields `trading_symbol`, `exchange`, `segment`, `token` (`:163-166, 204`), `groww_order_id`, `order_status`, `order_type`, `transaction_type`, `product`, `quantity`, `price`, `trigger_price`, `created_at`, `order_reference_id`, `remark`, `filled_quantity` (`mapping/order_data.py:77-84, 128-143`; `api/order_api.py:528`).
- Post-processing per order (`:162-349`): sets `order["brsymbol"] = trading_symbol`, `order["brexchange"] = exchange`; decides derivative-ness — **if any of `"CE","PE","C","P"` is a substring of trading_symbol → exchange forced to `"NFO"`** (`:178-181`; see quirk 9.1), elif `"FUT" in symbol or segment == "FNO"` → `"NFO"` futures (`:186-193`); else exchange left as Groww's (`:194-196`). For derivatives tries (a) `get_oa_symbol(token, "NFO")` (`:212`), (b) DB `SymToken.brsymbol == trading_symbol and exchange == "NFO"` (`:231-246`), (c) regex `([A-Z]+)(\d{2})(\d{2})(\d{2})(\d+)(CE|PE)` → `{name}{DD}{MMM}{YY}{strike}{CE|PE}` (`:258-298`) or `([A-Z]+)(\d{2})(\d{2})(\d{2})(?:FUT)?` → `{name}{DD}{MMM}{YY}FUT` (`:303-339`), (d) fallback raw (`:344-346`). Non-derivatives: `order["symbol"] = trading_symbol` (`:349`).
- Returns `{"data": all_orders, "order_list": all_orders, "raw_response": {"status":"SUCCESS","payload":{"order_list": all_orders}}}` (`:353-357`); on exception same shape with empty lists and `"FAILURE"` (`:374-378`).

### 3.2 Place order — `POST /v1/order/create`

`api/order_api.py:1600-1897` (`direct_place_order_api`, wrapped by `place_order_api` `:1885-1897`). `transform_data()` in `mapping/transform_data.py:49-100` exists but is **not** what builds the live payload; the live path builds its own.

Symbol resolution (`:1623-1643`): query `SymToken.filter_by(symbol=<openalgo symbol>, exchange=<openalgo exchange>).first()`; if found use `brsymbol`; else fallback `format_openalgo_to_groww_symbol(symbol, exchange)` (`database/master_contract_db.py:92-222`, which emits a **space-separated** form like `"NIFTY 29MAY25 24500 CE"`, see 5.7).

Payload (`:1692-1702`, plus `:1705-1730`):
```json
{
  "trading_symbol": "<brsymbol>",
  "quantity": <int>,
  "validity": "DAY" | "IOC",
  "exchange": "NSE" | "BSE",
  "segment": "CASH" | "FNO",
  "product": "CNC" | "MIS" | "NRML",
  "order_type": "MARKET" | "LIMIT" | "STOP_LOSS_LIMIT" | "STOP_LOSS_MARKET",
  "transaction_type": "BUY" | "SELL",
  "order_reference_id": "<8-20 chars>",
  "price": <float, only when order_type == LIMIT>,
  "trigger_price": <float, only when order_type in {STOP_LOSS_LIMIT, STOP_LOSS_MARKET}>
}
```
- `price` is taken only when `pricetype.upper() == "LIMIT"` (`:1654-1656`, `:1705-1713`); `trigger_price` only for `SL`/`SL-M` (`:1657-1661`, `:1716-1730`). Quantity must be `> 0` else `ValueError` (`:1733-1742`).
- `order_reference_id` generation (`:1663-1689`): if the request has `order_reference_id` use it, else `f"{YYYYMMDD}-{uuid4 hex[:8]}"` (17 chars). Then: strip every char not `[a-zA-Z0-9-]`; keep at most two hyphens (excess removed); left-justify-pad with `"0"` to minimum 8; truncate to 20.
- Response (`:1776-1837`): HTTP 200 and `status == "SUCCESS"` → `payload.groww_order_id` (returned as orderid), `payload.order_status`, `payload.order_reference_id`, `payload.remark` (`:1792-1805`). Formatted return:
  ```json
  {"groww_order_id": "...", "order_status": "...", "order_reference_id": "...", "remark": "...", "trading_symbol": "<brsymbol>", "symbol": "<openalgo symbol>"}
  ```
  with status object `.status = 200` and `orderid` (`:1810-1811`). HTTP 200 but non-SUCCESS → `{"status":"error","message": <message>, "mode": <mode>}` with status 400 (`:1814-1837`). Non-200 → `{"status":"error","message": <message or "API error: {code}">}` with status = HTTP code (`:1838-1871`).

### 3.3 Modify order — `POST /v1/order/modify`

`api/order_api.py:2712-3008`.

- `groww_order_id = data["orderid"]` required (`:2733-2735`). `order_type` from `pricetype` via `map_order_type`, else looked up from the order book by `groww_order_id` (`:2738-2760`), else default `"MARKET"` (`:2763-2767`). `segment = map_segment_type(data.get("exchange","NSE"))` (`:2770-2771`).
- Payload (`:2774`, `:2784`, `:2802`, `:2822`):
  ```json
  {"groww_order_id": "...", "order_type": "...", "segment": "CASH|FNO", "quantity": <int>?, "price": <float, LIMIT only>?, "trigger_price": <float, SL/SL-M only>?}
  ```
- Response: `payload.order_status` (default `"MODIFICATION_REQUESTED"`) (`:2876-2877`). **Any HTTP 200 and any non-200 both return `{"status":"success", ...}`** (`:2881-2895`, `:2947-2953`, `:2965-2971`); `modify_order()` returns `({"status":"success","orderid":...,"order_status":...,"message":"Order modification request processed successfully"}, 200)` (`:2999-3005`). Only a JSON-decode failure yields `status: error` (`:2896-2903`).

### 3.4 Cancel order — `POST /v1/order/cancel`

`api/order_api.py:2436-2709`; signature `cancel_order(orderid, auth, segment=None, symbol=None, exchange=None)`.

- Segment resolution when not supplied (`:2457-2519`): if `orderid.startswith("GLTFO")` → `FNO` (`:2474-2478`); else scan order book for `groww_order_id == orderid` and map `segment` `CASH`→CASH, `{FNO,F&O,OPTIONS,FUTURES}`→FNO, `CURRENCY`→`SEGMENT_CURRENCY`, `COMMODITY`→`SEGMENT_COMMODITY` (`:2483-2498`; **these two constants are undefined — NameError if reached**, quirk 9.3); if not found and orderid contains `CE`/`PE`/`FUT` → FNO (`:2501-2510`). Default CASH (`:2515-2519`). Then again: if orderid starts with `GLTFO` or contains `CE`/`PE`/`FUT` → force FNO (`:2531-2544`).
- Payload (`:2550`): `{"segment": "CASH|FNO", "groww_order_id": "<id>"}`.
- Response consumed: `payload.order_status` (`"CANCELLATION_REQUESTED"` or `"CANCELLED"`), `payload.groww_order_id` (`:2623-2646`). Return on HTTP 200: `{"status":"success","orderid":..,"api_status":..,"message":..,"raw_response":..,"order_status":..,"groww_order_id":..,"symbol":?}` (`:2612-2654`). **Non-200 also returns `status: "success"`** with `api_message`, `api_status_code` (`:2670-2677`); exceptions also return success (`:2686-2709`). Always `(response, 200)`.

`cancel_all_orders_api(data, auth)` (`:3011-3248`): cancellable statuses `OPEN, PENDING, TRIGGER_PENDING, PLACED, PENDING_ORDER, NEW, ACKED, APPROVED, MODIFICATION_REQUESTED` (case-insensitive) (`:3071-3083`); segment from order `segment` field (`:3127-3137`, same undefined-constant hazard); returns `(cancelled_orders, failed_to_cancel)` lists (`:3237`).

### 3.5 Trades for an order — `GET /v1/order/trades/{orderid}`

`api/order_api.py:67, 3251-3518`; `get_order_trades(orderid, auth, segment=None)`.

- URL: `f"{BASE}/v1/order/trades/{orderid}?segment={segment}&page=0&page_size=50"` (`:3368-3372`). There is **no `/v1/order/trade-list`** call and no `/v1/order/detail/{id}` call anywhere.
- Segment resolution when missing (`:3278-3355`): `GMKFO`/`GLTFO` prefix → FNO (`:3298-3301`); else search order book; unknown → `GMK` prefix → CASH (`:3343-3345`); default CASH.
- Response: `payload.trade_list[]` with `groww_trade_id`, `groww_order_id`, `exchange_trade_id`, `exchange_order_id`, `trading_symbol`, `quantity`, `price`, `trade_status`, `exchange`, `segment`, `product`, `transaction_type`, `created_at`, `trade_date_time`, `settlement_number`, `remark` (`:3404-3423`). Normalised to keys `trade_id, order_id, exchange_trade_id, exchange_order_id, symbol, quantity, price, trade_status, exchange, segment, product, transaction_type, created_at, trade_date_time, settlement_number, remarks`.
- HTTP 404 on an FNO order with known `filled_quantity > 0` → synthetic trade `trade_id = f"synthetic_{orderid}"` (`:3436-3473`).
- `price` is in **rupees** and must pass through unscaled (`:396-440` docstring; `test/test_groww_tradebook_price.py:1-62`).

`get_trade_book(auth)` (`:443-839`): fetches the order book, selects orders whose status is in `EXECUTED, COMPLETED, FILLED, PARTIAL, COMPLETE` or contains `EXECUT`/`FILL`/`COMPLET` or `filled_quantity > 0` (`:517-548`); segment FNO if orderid starts with `GLTFO` (`:638-640`); calls `get_order_trades` per order; synthesises trades on empty/404/error when `filled_quantity > 0` (`:669-781`); maps each via `transform_groww_trade` (`:396-440`) to keys `tradingSymbol, exchangeSegment, productType, transactionType, tradedQuantity, tradedPrice, orderId, updateTime, tradeId, trade_id, order_id, exchange, segment, symbol, quantity, price, transaction_type, trade_date_time, created_at, status`; returns `({"status":"success","message":..,"data":[...],"tradebook":[...],"raw_data":[...]}, 200)` (`:811-826`).

### 3.6 Positions — `GET /v1/positions/user?segment=CASH|FNO`

`api/order_api.py:860-1342`; `get_positions(auth, strict=False)`.

- Two calls: `segment=CASH` (`:897-910`) then `segment=FNO` (`:1118-1121`), timeout 30.
- Response `payload.positions[]` fields consumed: `credit_quantity`, `carry_forward_credit_quantity`, `debit_quantity`, `carry_forward_debit_quantity`, `quantity`, `net_price`, `trading_symbol`, `exchange`, `segment`, `product`, `credit_price`, `debit_price`, `symbol_isin` (`:937-1105`).
- Derivations: `buy_qty = credit_quantity + carry_forward_credit_quantity`; `sell_qty = debit_quantity + carry_forward_debit_quantity`; `net_qty = quantity if present else buy_qty - sell_qty` (`:937-943`); `avg_price = net_price; if avg_price > 1000: avg_price /= 100` (heuristic "likely paise", `:946-948`); `buy_price = credit_price / 100`; `sell_price = debit_price / 100 if debit_price > 0 else 0` (`:1093-1097`).
- Exchange remap (CASH loop `:1058-1067`): `NSE→"NSE_EQ"`, `BSE→"BSE_EQ"`, `NFO→"NSE_FO"`, else passthrough; FNO loop (`:1265-1274`): `NSE→"NSE"`, `BSE→"BSE"`, `NFO→"NSE_FO"`. Segment string `"EQ"` for CASH (`:1091`), `"FO"` for FNO (`:1287`).
- FNO symbol conversion: `get_oa_symbol(trading_symbol, "NFO")` (lookup by brsymbol, `database/token_db_enhanced.py:937-949`) then the same regexes as 3.1 (`:965-1052`, `:1169-1260`).
- Output row keys (`:1080-1108`, `:1277-1299`): `symbol, tradingsymbol, exchange, product, quantity, net_quantity, average_price, buy_quantity, sell_quantity, segment, buy_price, sell_price, symbol_isin, pnl(0), last_price(0), close_price(0), instrument_token(=symbol_isin), unrealised(0), realised(0)`.
- Return `({"status":"success","message":..,"data":[...],"raw_response":<CASH json>, "failed_segments": ["FNO"]?}, 200)` (`:1311-1322`); in `strict=True` a CASH read failure returns `({"status":"error",...}, 502)` (`:1306-1308`); an FNO failure is tolerated and reported via `failed_segments` (`:842-857`, `:1116-1122`, `:1317-1319`). "Empty book" detection via `says_no_positions` (`utils/position_read.py:206-229`).
- Segment per exchange for smart-order checks: `{"NSE":"CASH","BSE":"CASH","NFO":"FNO","BFO":"FNO"}` (`:861`).

### 3.7 Holdings — `GET /v1/holdings/user`

**Two `get_holdings` definitions exist; Python keeps the last one.** The first (`api/order_api.py:1345-1467`, `GET /v1/portfolio/holdings`) is dead. The effective one is `api/order_api.py:2186-2263`:

- URL `https://api.groww.in/v1/holdings/user` (`:2212`), headers `Accept`, `Authorization`, `X-API-VERSION: 1.0` (`:2201-2205`), timeout 10 s.
- Requires `status == "SUCCESS"` (`:2231`); reads `payload.holdings[]` fields `trading_symbol, isin, quantity, average_price, demat_free_quantity, demat_locked_quantity, groww_locked_quantity, pledge_quantity, t1_quantity` (`:2241-2253`).
- Output row: `{"symbol", "isin", "quantity", "average_price", "free_quantity" (=demat_free_quantity), "locked_quantity" (=demat_locked+groww_locked), "pledged_quantity", "t1_quantity"}`; returns **tuple** `(formatted_holdings, {"status":"success"})` (`:2258`) or `(None, {"status":"error","message":..})` (`:2225, 2234, 2263`). Downstream `calculate_portfolio_statistics` unwraps the tuple (`mapping/order_data.py:966-971`; pinned by `test/test_groww_holdings_mapping.py:48-53`). Note: there is no price/pnl in the Groww holdings payload (test comment `test/test_groww_holdings_mapping.py:19-20`).

### 3.8 Funds — `GET /v1/margins/detail/user`

`api/funds.py:14-110`. Requires `status == "SUCCESS"` (`:41`); `payload` fields and output mapping (`:69-104`):

| Output key | Source | Format |
| --- | --- | --- |
| `availablecash` | `payload.clear_cash` | `"{:.2f}"` |
| `collateral` | `payload.collateral_available` | `"{:.2f}"` |
| `m2munrealized` | hard-coded 0 (`:55-66`) | `"0.00"` |
| `m2mrealized` | hard-coded 0 | `"0.00"` |
| `utiliseddebits` | `payload.net_margin_used` | `"{:.2f}"` |
| `brokerage_and_charges` | `payload.brokerage_and_charges` | |
| `adhoc_margin` | `payload.adhoc_margin` | |
| `equity_cnc_balance` | `payload.equity_margin_details.cnc_balance_available` | |
| `equity_mis_balance` | `payload.equity_margin_details.mis_balance_available` | |
| `fno_futures_balance` | `payload.fno_margin_details.future_balance_available` | |
| `fno_option_buy_balance` | `payload.fno_margin_details.option_buy_balance_available` | |
| `fno_option_sell_balance` | `payload.fno_margin_details.option_sell_balance_available` | |

Any failure → `{}` (`:34, 43, 50, 110`).

### 3.9 Margin calculator — `POST /v1/margins/detail/orders?segment=CASH|FNO`

`api/margin_api.py:11, 14-103`; `mapping/margin_data.py`.

- Body is a **JSON array** of positions (`api/margin_api.py:72-74`), each (`mapping/margin_data.py:71-82`):
  ```json
  {"trading_symbol": "<brsymbol via get_br_symbol>", "transaction_type": "BUY|SELL", "quantity": <int>, "order_type": "MARKET|LIMIT|STOP_LOSS_LIMIT|STOP_LOSS_MARKET", "product": "CNC|MIS|NRML", "exchange": "NSE|BSE", "price": <float>?}
  ```
  `exchange` via `map_margin_exchange` (`NSE,NFO→NSE`; `BSE,BFO→BSE`) (`mapping/margin_data.py:16-27`). All positions must share one segment; mixed segments → later ones dropped (`:50-60`). CASH segment permits only ONE position (first kept) (`api/margin_api.py:46-51`).
- Response (`mapping/margin_data.py:97-138`): `status == "SUCCESS"` else `message` / `errors[0].message`; `payload.total_requirement`, `payload.span_required`, `payload.exposure_required` → `{"status":"success","data":{"total_margin_required","span_margin","exposure_margin"}}`.

### 3.10 Quote — `GET /v1/live-data/quote?exchange=&segment=&trading_symbol=`

`api/data.py:1049-1482` (`get_quotes`), `:1601-1860` (`get_depth`), `:2216-2446` (`_overlay_full_quotes`).

- Exchange/segment: `NSE→NSE/CASH`, `BSE→BSE/CASH`, `NFO→NSE/FNO`, `BFO→BSE/FNO`, else `NSE/CASH` with warning (`:1146-1161`, `:1684-1698`). `NSE_INDEX`/`BSE_INDEX` are not special-cased in `get_quotes`/`get_depth` (fall to default NSE/CASH; works because the brsymbol is e.g. `NIFTY`).
- `trading_symbol` = `get_br_symbol(symbol, exchange)`; for NFO/BFO fallback `_convert_openalgo_to_groww_derivative_symbol` (`:1167-1173`, see 5.8); else raw symbol.
- Payload fields consumed (`:1240-1355`, `:1766-1851`, `:2307-2385`): `last_price`, `ohlc` (**either a dict `{open,high,low,close}` or a non-JSON string `"{open: 149.50,high: 150.50,low: 148.50,close: 149.50}"`** parsed by splitting on `,` and `:` — `:1243-1261`), `day_change`, `day_change_perc`, `volume` | `total_volume` | `traded_volume`, `bid_price` | `bid` | `best_bid_price`, `offer_price` | `ask` | `best_offer_price` | `best_ask_price`, `bid_quantity` | `bid_size` | `best_bid_quantity`, `offer_quantity` | `ask_quantity` | `ask_size` | `offer_size` | `best_offer_quantity`, `open_interest` | `oi`, `total_buy_quantity`, `total_sell_quantity`, `last_trade_time`, `upper_circuit_limit`, `lower_circuit_limit`, `last_trade_quantity` (`:1825`), `depth.buy[] {price, quantity}`, `depth.sell[] {price, quantity}`.
- Single-symbol return (what the REST `/quotes` API returns as `data`) (`:1497-1509`):
  ```json
  {"ltp": 0.0, "open": 0.0, "high": 0.0, "low": 0.0, "prev_close": <ohlc.close>, "volume": 0, "bid": <bid_price>, "ask": <ask_price>, "bid_qty": 0, "ask_qty": 0, "oi": <only if NFO/BFO else 0>}
  ```
  Multi-symbol return `{"status":"success","data":[quote_item...]}` where `quote_item` keys are `symbol, exchange, token, ltp, last_price, open, high, low, close, prev_close, change, change_percent, volume, bid, ask, bid_price, bid_qty, ask_price, ask_qty, total_buy_qty, total_sell_qty, oi, timestamp, upper_circuit?, lower_circuit?, depth?{buy[{price,quantity,orders:0}],sell[...]}` (`:1313-1391`). Zero-price depth levels dropped (`:1367-1389`).
- Depth return (`:1836-1851`): `{"bids":[5 x {price,quantity}], "asks":[5 x ...], "ltp", "ltq" (=last_trade_quantity), "open","high","low","prev_close" (=ohlc.close), "volume", "totalbuyqty", "totalsellqty", "oi" (derivatives only)}`; levels capped at 5 and **padded with `{price:0,quantity:0}` to exactly 5** (`:1797-1821`); `{}` on any failure (`:1740, 1860`).

### 3.11 OHLC batch — `GET /v1/live-data/ohlc?segment=&exchange_symbols=NSE_SBIN,NSE_TCS,...`

`api/data.py:2021-2214` (`_fetch_ohlc_batch`), driven by `get_multiquotes` (`:1870-1914`) and `_process_quotes_batch` (`:1916-2019`). **The `/v1/live-data/ltp` endpoint is NOT used anywhere.**

- `exchange_symbols` is a comma-joined list of `f"{NSE|BSE}_{brsymbol}"` (`:1961-1970`, `:2035`); prefix `NSE` for `NSE, NFO, NSE_INDEX`, `BSE` for `BSE, BFO, BSE_INDEX`, default `NSE` (`:1962-1967`). Symbols are grouped by segment: NFO/BFO → `FNO`, all others → `CASH` (`:1980-1983`), one request per segment (`:1998-2016`).
- `payload` is keyed by the exact `exchange_symbol`; each value is a string `"{open: ..,high: ..,low: ..,close: ..}"`, or a dict, or a scalar LTP (`:2145-2182`). `ltp = close` (`:2174-2175`).
- Error `details` containing `"Invalid trading symbol: (\w+)"` → mark that symbol `{"error":"Invalid trading symbol in Groww"}` and retry without it, at most 5 recursive retries (`:2063-2102`).
- Result item (`:2184-2198`): `{"symbol","exchange","data":{"bid":0,"ask":0,"open","high","low","ltp","prev_close":<close>,"volume":0,"oi":0}}`; error items `{"symbol","exchange","error"}`.
- FNO hybrid (`:2003-2016`): after the OHLC batch, per-symbol `/v1/live-data/quote` overlay sequentially with `REQUEST_INTERVAL = 0.25 s` and abort after `MAX_CONSECUTIVE_429 = 4` consecutive errors whose text contains `"429"` or `"Rate limit"` (`:2233-2237`, `:2393-2413`); merges keys `bid, ask, bid_qty, ask_qty, volume, oi, total_buy_qty, total_sell_qty, depth` onto the baseline (`:2427-2439`); bid/ask fall back to top-of-book `depth.buy[0]`/`depth.sell[0]` when `bid_price`/`offer_price` are null (`:2320-2346`).

### 3.12 History — `GET /v1/historical/candle/range`

`api/data.py:312-932`. Query params (`:407-414`):
```
exchange=NSE|BSE  segment=CASH|FNO  trading_symbol=<brsymbol>
start_time="YYYY-MM-DD 09:15:00"  end_time="YYYY-MM-DD 15:30:00"  interval_in_minutes=<see 6>
```
Response: `payload.candles` as list of `[timestamp, open, high, low, close, volume]` (`:478-492`, `:545-561`); dict form `{timestamp, open, high, low, close, volume}` also tolerated (`:493-506`, `:563-606`). Timestamp heuristics: `if ts > 4102444800: ts /= 1000` (ms → s) (`:481-482`, `:550-551`). Details in section 6.

### 3.13 Streaming socket token — `POST https://api.groww.in/v1/api/apex/v1/socket/token/create/`

See section 7.1.

### 3.14 Master contract CSV — `GET https://growwapi-assets.groww.in/instruments/instrument.csv`

See section 5.

Endpoints requested in the brief but **absent** from the implementation: `/v1/order/detail/{id}`, `/v1/order/trade-list`, `/v1/live-data/ltp`, TOTP token flow.

---

## 4. Field mappings

### 4.1 OpenAlgo → Groww (`mapping/transform_data.py`)

Constants (`:7-46`): `VALIDITY_DAY="DAY"`, `VALIDITY_IOC="IOC"`, `EXCHANGE_NSE="NSE"`, `EXCHANGE_BSE="BSE"`, `SEGMENT_CASH="CASH"`, `SEGMENT_FNO="FNO"`, `PRODUCT_CNC/MIS/NRML`, `ORDER_TYPE_MARKET="MARKET"`, `ORDER_TYPE_LIMIT="LIMIT"`, **`ORDER_TYPE_SL="STOP_LOSS_LIMIT"`** (`:27`), `ORDER_TYPE_SLM="STOP_LOSS_MARKET"` (`:28`), `TRANSACTION_TYPE_BUY/SELL`, order statuses `NEW, ACKED, TRIGGER_PENDING, APPROVED, FAILED, EXECUTED, DELIVERY_AWAITED, CANCELLED, CANCELLATION_REQUESTED, MODIFICATION_REQUESTED, COMPLETED, REJECTED` (`:35-46`). There is **no** `SEGMENT_CURRENCY`/`SEGMENT_COMMODITY` constant (see 9.3).

| OpenAlgo field | Groww field | Mapping (default) | Cite |
| --- | --- | --- | --- |
| `exchange` | `exchange` | `NSE→NSE`, `BSE→BSE`, `NFO→NSE`, `BFO→BSE` (default `NSE`) | `:158-164` |
| `exchange` | `segment` | `NSE→CASH`, `BSE→CASH`, `NFO→FNO`, `BFO→FNO` (default `CASH`) | `:217-223` (dup `:204-210`) |
| `product` | `product` | `CNC→CNC`, `NRML→NRML`, `MIS→MIS` (default `CNC`) | `:184-189` |
| `pricetype` | `order_type` | `MARKET→MARKET`, `LIMIT→LIMIT`, `SL→STOP_LOSS_LIMIT`, `SL-M→STOP_LOSS_MARKET` (default `MARKET`) | `:143-151` |
| `action` | `transaction_type` | `BUY→BUY`, `SELL→SELL` (default `BUY`) | `:242-245` |
| `validity` | `validity` | `DAY→DAY`, `IOC→IOC`, `GTC→DAY` (default `DAY`) | `:230-235` |
| `symbol` | `trading_symbol` | `SymToken.brsymbol` for (symbol, exchange); fallback `format_openalgo_to_groww_symbol` | `api/order_api.py:1627-1643` |

Streaming exchange map (`streaming/groww_mapping.py:10-20`) is broader: `NSE→NSE/CASH`, `BSE→BSE/CASH`, `NFO→NSE/FNO`, `BFO→BSE/FNO`, `MCX→MCX/COMM`, `CDS→NSE/CDS`, `BCD→BSE/CDS`, `NSE_INDEX→NSE/CASH`, `BSE_INDEX→BSE/CASH` (MCX/CDS/BCD are not in `supported_exchanges`, so unreachable in practice).

Historical/quotes mapping for indices: `NSE_INDEX→NSE/CASH`, `BSE_INDEX→BSE/CASH` (`api/data.py:187-209`).

### 4.2 Groww → OpenAlgo order book (`mapping/order_data.py`)

Status map (identical at `:146-156` and `:423-433`):

| Groww `order_status` | OpenAlgo |
| --- | --- |
| `NEW`, `ACKED`, `OPEN`, `APPROVED` | `open` |
| `TRIGGER_PENDING` | `trigger pending` |
| `EXECUTED`, `COMPLETED` | `complete` |
| `CANCELLED` | `cancelled` |
| `REJECTED` | `rejected` |
| anything else | `map_order_data`: `"open"` (`:158`); `transform_order_data`: `status.lower()` (`:439`) |

Order type reverse (`:409-413`): `STOP_LOSS→SL`, `STOP_LOSS_MARKET→SL-M`, else passthrough (`MARKET`, `LIMIT`). Note the forward map sends `STOP_LOSS_LIMIT` but the reverse map only recognises `STOP_LOSS`; a `STOP_LOSS_LIMIT` echoed by Groww would pass through unmapped (quirk 9.6).

Product reverse (`:163-168`, `:416-420`): `CNC→CNC`, `INTRADAY→MIS`, `MARGIN→NRML`, else passthrough.

`transform_order_data` output row (`:465-477`):
```json
{"symbol", "exchange" (order.exchange, default "NSE"), "action" (transaction_type), "quantity", "price", "trigger_price", "pricetype", "product", "orderid" (groww_order_id), "order_status", "timestamp" (created_at)}
```
No `filled_quantity`, `average_price`, `pending_quantity`, `rejection_reason` are emitted (relevant to 7.8 polling).

Statistics (`:242-257`): BUY/SELL counted from `transaction_type`; completed = `EXECUTED|COMPLETED`; open = `NEW|ACKED|APPROVED|OPEN`; rejected = `REJECTED`.

### 4.3 Positions (final shape after `transform_positions_data`, `mapping/order_data.py:742-867`)

Exchange re-derived: `NFO` if `segment == "FNO"` or symbol contains `CE`/`PE`/`FUT`, else `NSE` (`:778-783`) — BSE/BFO are lost. Reads `quantity, sellQty, buyQty, avgPrice, closePrice, lastPrice, pnl, multiplier, unrealised, realised` (`:837-846`) — camelCase keys that `get_positions` never emits (it emits `average_price`, `buy_quantity`, `sell_quantity`), so `average_price`, `buy_quantity`, `sell_quantity` end up `0.0` in the public positionbook (quirk 9.7). Output keys (`:848-863`): `symbol, exchange, product, quantity, average_price, close_price, last_price, pnl, multiplier, unrealised, realised, buy_quantity, sell_quantity, instrument_token`.

### 4.4 Holdings (`mapping/order_data.py:870-913`, `:943-1030`)

`transform_holdings_data` output: `{"symbol", "exchange" (default "NSE"), "quantity", "average_price", "product": "CNC" default, "pnl", "pnlpercent"}`. Statistics: `totalholdingvalue = totalinvvalue = Σ quantity*average_price`, `totalprofitandloss = Σ pnl`, `totalpnlpercentage = pnl/inv*100`, all rounded to 2 dp (`:1002-1030`). `map_portfolio_data` is a Dhan leftover no-op (`:916-940`).

### 4.5 Smart order / open position lookup

`get_open_position(tradingsymbol, exchange, product, auth)` (`api/order_api.py:1523-1597`): converts symbol with `get_br_symbol`, reads a 1-second cached strict position book (`:1479-1515`, `utils/smart_order_guard.PositionBookCache`), raises `PositionReadError` if the needed segment failed (`:1546-1550`), matches `tradingsymbol|symbol|trading_symbol == brsymbol`, `exchange in {"NSE":{NSE,NSE_EQ}, "BSE":{BSE,BSE_EQ}, "NFO":{NFO,NSE_FO,NSE}, "BFO":{BFO,BSE_FO,BSE}}` (`:1570-1575`) and `product == map_product_type(product)` (`:2037`); returns `net_quantity|netqty|quantity` as string. Smart-order decision logic is the standard OpenAlgo one (`:1975-2183`); per-symbol lock `SymbolLocks(name="groww smart orders")` (`:1477`).

`close_all_positions(token, auth)` (`:2266-2433`): for each non-zero position, strips `_EQ`/`_FO` from exchange (`:2320`), forces `exchange="NFO", product="MIS"` when segment `FO` or exchange contains `FNO`/`NFO` (`:2343-2351`), and places a `MARKET` order with `strategy: "Squareoff"` (`:2354-2363`).

---

## 5. Master contract (`database/master_contract_db.py`)

### 5.1 Download

- URL `https://growwapi-assets.groww.in/instruments/instrument.csv` (`:402`), saved as `tmp/master.csv` (`:401`, `:789`). Groww's own header row is kept (`:437-449`).
- Required columns validated by name (`:409-422`): `exchange, exchange_token, trading_symbol, groww_symbol, name, instrument_type, segment, underlying_symbol, expiry_date, strike_price, lot_size, tick_size`.
- Full documented CSV header (comment `:533`): `exchange,exchange_token,trading_symbol,groww_symbol,name,instrument_type,segment,series,isin,underlying_symbol,underlying_exchange_token,lot_size,expiry_date,strike_price,tick_size,freeze_quantity,is_reserved,buy_allowed,sell_allowed,feed_key`. Columns **not read**: `underlying_exchange_token, freeze_quantity, is_reserved, buy_allowed, sell_allowed, feed_key`.
- dtypes: `exchange_token` read as **string** (preserve leading zeros) (`:538`); `lot_size`, `strike_price`, `tick_size` float (`:548-551`).

### 5.2 Column → SymToken mapping (`:559-583`)

| CSV | DataFrame/SymToken | Note |
| --- | --- | --- |
| `exchange` | `brexchange` (`:560`, re-set `:673`) | `NSE`/`BSE` |
| `exchange_token` | `token` | string |
| `trading_symbol` | `brsymbol` (`:562`) and initial `symbol` (`:583`) | |
| `groww_symbol` | `groww_symbol` (`:563`) then **overwritten by `name`** (`:564`) | not a SymToken column; dropped |
| `instrument_type`, `segment`, `series`, `isin`, `underlying_symbol`→`underlying` | intermediate only | not SymToken columns |
| `lot_size` | `lotsize` int, NaN→1 (`:623-625`) | |
| `expiry_date` | `expiry` as `DD-MMM-YY` upper, else `""` (`:631-635`) | from `yyyy-mm-dd` |
| `strike_price` | `strike` float, NaN→0 (`:622, 626`) | |
| `tick_size` | `tick_size` float, NaN→0.05 (`:628`) | |
| (none) | `name` | always `""` (`:616-618`; never populated — quirk 9.9) |

### 5.3 Exchange / instrumenttype assignment

- `instrumenttype` from `instrument_type`: `EQ→EQ`, `IDX→INDEX`, `FUT→FUT`, `CE→CE`, `PE→PE`, `ETF→EQ`, `CURR→CUR`, `COM→COM` (`:639-651`); missing: CASH→`EQ`, FNO with `strike_price>0`→`OPT`, FNO with `strike_price==0`→`FUT`, remaining→`EQ` (`:654-670`). Any row with `instrument_type=="IDX"` or `segment=="IDX"` → `INDEX` (`:703-704`).
- `exchange`: start as CSV `exchange`; `NSE & segment FNO → NFO`; `BSE & segment FNO → BFO`; `NSE & (segment IDX | instrument_type IDX) → NSE_INDEX`; `BSE & (...) → BSE_INDEX` (`:682-699`). No filtering by `series` or `segment` is applied — every CSV row is loaded (CURR/COM rows keep their CSV exchange).

### 5.4 Index symbol renames (`:588-602`, applied to `symbol` only, `brsymbol` keeps the Groww name)

| Groww `trading_symbol` | OpenAlgo `symbol` |
| --- | --- |
| `NIFTYJR` | `NIFTYNXT50` |
| `NIFTYMIDSELECT` | `MIDCPNIFTY` |
| `NIFTYMIDCAP` | `NIFTYMIDCAP100` |
| `NIFTYSMALL` | `NIFTYSMLCAP100` |
| `NIFTYSMALLCAP250` | `NIFTYSMLCAP250` |
| `NIFTYCDTY` | `NIFTYCOMMODITIES` |
| `MIDCAP50` | `NIFTYMIDCAP50` |
| `BSESMLCAP` | `BSESMALLCAP` |

`NIFTY`, `BANKNIFTY`, `FINNIFTY`, `INDIAVIX`, `SENSEX`, `BANKEX` are **not** renamed (they pass through as Groww names them).

### 5.5 F&O OpenAlgo symbol construction (`:708-754`)

Only rows with `brexchange == "NSE" and segment == "FNO" and expiry != ""` (`:708-713`) — **BFO rows are never reformatted; their `symbol` stays equal to Groww's `trading_symbol`** (quirk 9.10).

- `expiry_str = strftime("%d%b%y").upper()` of the `DD-MMM-YY` expiry → `DDMMMYY` e.g. `29MAY25` (`:717-718`).
- `base = underlying_symbol if non-empty else trading_symbol` (`:721-722`).
- `strike_str`: `str(int(f))` if whole, else `str(f)` (so `287.5` stays `287.5`, `190.0`→`190`) (`:727-730`).
- `FUT`: `base + expiry_str + "FUT"` (`:736-740`); `CE`: `base + expiry_str + strike_str + "CE"` (`:743-747`); `PE`: likewise (`:750-754`). `OPT` (fallback type) rows are not rebuilt.
- Post-step (`master_contract_download` `:836-842`): for `exchange=="NFO"` and `instrumenttype in {CE,PE}` whose `brsymbol` contains a space, `symbol = brsymbol.replace(" ", "")`.
- NaN hygiene (`:852-862`): `symbol`/`brsymbol` NaN→`""`; empty `brsymbol` ← `symbol`; rows with empty `symbol` dropped (logged count).

### 5.6 Insert semantics

`delete_symtoken_table()` then `copy_from_dataframe(df)` (`:795-798`, `:866`). `copy_from_dataframe` (`:59-88`) **filters out rows whose `token` already exists in the table, keyed on `token` alone** (`:65-68`), inserts in batches of 10,000 via `bulk_insert_mappings`. Extra DataFrame columns (`groww_symbol`, `instrument_type`, `segment`, `series`, `isin`, `underlying`) are not SymToken columns. On success emits SocketIO `master_contract_download {"status":"success","message":"Successfully downloaded and inserted N symbols"}` (`:875-881`). Temp files `master.csv, groww_instruments.csv, groww_master.csv` deleted (`:764-783`).

Stored per row: `symbol` (OpenAlgo), `brsymbol` (= CSV `trading_symbol`), `name` (`""`), `exchange` (NSE/BSE/NFO/BFO/NSE_INDEX/BSE_INDEX/...), `brexchange` (NSE/BSE), `token` (= CSV `exchange_token`), `expiry` (`DD-MMM-YY`), `strike`, `lotsize`, `instrumenttype`, `tick_size`. `feed_key` and `groww_symbol` are **not stored**.

### 5.7 Fallback symbol converters (used only when the DB lookup misses)

- `format_openalgo_to_groww_symbol(symbol, exchange)` (`:92-222`): for `NFO` + `CE|PE`: base symbol from `["NIFTY","BANKNIFTY","FINNIFTY","MIDCPNIFTY","SENSEX","AARTIIND"]` prefix list or leading alphabetic run; date `(\d{2})([A-Za-z]{3})(\d{2})`; returns **`"{BASE} {DDMMMYY} {STRIKE} {CE|PE}"`** with spaces (`:154`); futures `"{BASE} {DDMMM} FUT"` (`:217`, note: year-less because regex `\d{2}[A-Za-z]{3}`); otherwise returns input.
- `format_groww_to_openalgo_symbol(groww_symbol, exchange)` (`:225-323`): splits on spaces; `BASE DATE STRIKE CE|PE` → concatenation; `BASE DATE FUT` → `BASE+DATE+FUT`; 3-part with numeric third → assumes `CE`; else strips spaces.
- `reformat_symbol`/`assign_values` (`:456-509`) are **dead code** (never called); they expect `groww_symbol` of the form `NSE-AARTIIND-26Jun25-435-CE`.

### 5.8 Derivative symbol fallback in data.py

`_convert_openalgo_to_groww_derivative_symbol` (`api/data.py:137-164`): `^([A-Z]+)(\d{2})([A-Z]{3})(\d{2})(FUT)$` → `{base}{YY}{MMM}FUT` (`SBIN30SEP25FUT→SBIN25SEPFUT`); `^([A-Z]+)(\d{2})([A-Z]{3})(\d{2})(\d+)(CE|PE)$` → `{base}{YY}{MMM}{strike}{CE|PE}` (`SBIN30SEP25800CE→SBIN25SEP800CE`). This assumes a *third* Groww format (year+month, no day), different from both the CSV `trading_symbol` and the order-book regexes (`NIFTY25515266550CE` style, `api/order_api.py:255-259`). The authoritative value is always the CSV `trading_symbol` stored in `brsymbol`; treat the regex converters as last-resort heuristics.

---

## 6. Historical data (`api/data.py:106-135`, `:312-932`)

### 6.1 Interval map and limits

`timeframe_map` (`:112-124`): `"1m"→"1"`, `"5m"→"5"`, `"10m"→"10"`, `"1h"→"60"`, `"4h"→"240"`, `"D"→"1440"`, `"W"→"10080"`. Unknown → `"1440"` with warning (`:336-340`). `get_intervals()` advertises `minutes: [1m,5m,10m], hours: [1h,4h], days: [D], weeks: [W]`, empty seconds/months (`:952-962`).

Documented Groww duration constraints `time_constraints` (`:127-135`): ≤3 days → min 1 min; ≤15 → 5; ≤30 → 10; ≤150 → 60; ≤365 → 240; ≤1080 → 1440; >1080 → 10080. `get_valid_interval(start, end, interval)` (`:964-1047`) implements the upgrade rule (and maps `1d→D`, `1w→W`) but **is not called by `get_history`** — chunking is used instead.

Chunk sizes in days (`:374-383`): weekly 300; daily 100; interval ≥ 60 min → 15; ≥ 5 min → 7; 1 min → 3. Loop (`:389-443`): `current_end = min(current_start + (chunk-1) days, end_date)`; request with `start_time=f"{chunk_start} 09:15:00"`, `end_time=f"{chunk_end} 15:30:00"` (date strings `YYYY-MM-DD`, IST wall-clock); on non-SUCCESS or empty candles skip the chunk and continue (`:419-433`); `current_start = current_end + 1 day`.

### 6.2 Candle processing

- Candle `[ts, o, h, l, c, v]`; `ts` ms→s if `> 4102444800` (`:479-482`, `:548-551`); `volume = int(v) if not None else 0`.
- **EOD (D and W)** (`:461-535`): date taken from `fromtimestamp(ts, tz=Asia/Kolkata).date()`, then **timestamp = midnight UTC of that date** (`:516-522`, `:529`) — NOT 09:15 IST (quirk 9.11). Weekly: Groww returns daily candles for 10080, so after EOD processing the frame is resampled `W-MON, closed="left", label="left"` with `open=first, high=max, low=min, close=last, volume=sum` and the index set to 09:15 IST (`:714-778`); if fewer than 5 candles result, a manual 7-day bucketing fallback runs (`:780-838`).
- **Intraday** (`:536-639`): `dt = fromtimestamp(ts, tz=IST)`; candles outside `09:15 ≤ dt ≤ 15:30` IST are **dropped** (`:608-626`); sorted; `fix_timestamps` clamps into market hours again (`:237-310`, `:702-703`).
- Final frame columns in order: `timestamp, open, high, low, close, volume, oi` with `oi = 0` always (`:907-911`); sorted by timestamp, de-duplicated on timestamp, reset index (`:915-919`). Empty frame returned with those columns on error (`:694-695`, `:709-711`, `:858-860`, `:932`).

### 6.3 Symbol/exchange for history (`:166-230`)

`NSE,BSE,NSE_INDEX,BSE_INDEX → CASH`; `NFO,BFO → FNO`; anything else raises `ValueError("Unsupported exchange")` (`:187-195`). Exchange: `NFO,NSE_INDEX→NSE`; `BFO,BSE_INDEX→BSE`. `trading_symbol = get_br_symbol(symbol, exchange)`; for NFO/BFO fallback 5.8; equity fallback raw symbol.

---

## 7. WebSocket streaming — NATS over WebSocket + hand-rolled protobuf

The web adapter does NOT poll REST for market data; it uses Groww's NATS-over-WebSocket feed (`streaming/nats_websocket.py:1-3`, `:76`). Order updates, by contrast, ARE REST-polled (7.8).

### 7.1 Socket token (`streaming/nats_websocket.py:189-238`)

1. Generate an Ed25519 keypair and NATS nkey encoding (`groww_nkeys.generate_keypair()` `:195`; `streaming/groww_nkeys.py:405-483`): 32 random bytes seed; public key = base32(no padding) of `[PREFIX_BYTE_USER=0xA0] + 32-byte pubkey + CRC16-XMODEM little-endian 2 bytes` → starts with `U` (`groww_nkeys.py:16, 432-449, 279-290`); seed encoding `[0x90 | (0xA0>>5), (0xA0 & 31)<<3] + seed + crc16` → starts with `SU` (`:293-317`). Store the encoded seed (`nats_websocket.py:198`).
2. `POST https://api.groww.in/v1/api/apex/v1/socket/token/create/` (`:77`, `:213`) with headers (`:201-209`):
   ```json
   {"x-request-id": "<uuid4>", "Authorization": "Bearer <auth token>", "Content-Type": "application/json", "x-client-id": "growwapi", "x-client-platform": "growwapi-python-client", "x-client-platform-version": "0.0.8", "x-api-version": "1.0"}
   ```
   body `{"socketKey": "<nkey public key string>"}` (`:211`), timeout 15 s.
3. 200 → `socket_token = json["token"]` (a JWT), `subscription_id = json["subscriptionId"]` (`:215-218`). Any failure → fallback `socket_token = auth_token`, `subscription_id = "direct_auth"`, `nkey_seed = None` (`:223-238`).

### 7.2 WebSocket connect (`:240-282`)

URL `wss://socket-api.groww.in` (`:76`). Request headers (`:252-259`):
```
Authorization: Bearer <socket_token>
X-Subscription-Id: <subscription_id>
User-Agent: Python/3.10 nats.py/2.10.18
X-Client-Id: nats-py
X-API-Version: 1.0
Sec-WebSocket-Protocol: nats
```
TLS with certifi CA bundle, `cert_reqs=CERT_REQUIRED`; `websocket-client` `run_forever(ping_interval=30, ping_timeout=10)` (`:249`, `:271-275`). Connect wait: 10 s for open, then 3 s for auth, else "assume authenticated" (`:146-165`).

### 7.3 NATS handshake (`streaming/groww_nats.py`, `streaming/nats_websocket.py`)

- Server sends `INFO {json}\r\n` first (`nats_websocket.py:284-291`); parse JSON after first `{` (`groww_nats.py:178-195`); store `nonce` if present (`nats_websocket.py:459-461`).
- Client replies `CONNECT {json}\r\n` (`groww_nats.py:65-97`) with:
  ```json
  {"verbose": false, "pedantic": false, "tls_required": true, "jwt": "<socket_token>", "protocol": 1, "version": "2.10.18", "lang": "python3", "name": "nats.py", "headers": true, "no_responders": true, "nkey": "<nkey public>", "sig": "<base64 ed25519 signature of nonce bytes>"}
  ```
  `nkey`/`sig` only when a seed and nonce exist (`nats_websocket.py:98-110`): `sig = base64.b64encode(ed25519_sign(seed, nonce.encode()))` (standard base64, `:106-107`). Immediately followed by `PING\r\n` (`:123`).
- `+OK\r\n` → authenticated, resubscribe all (`:466-470`). If no `+OK` within 2 s of open, assume authenticated and resubscribe (`:294-305`). `-ERR '...'` containing `authorization`/`authentication` → `authenticated=False`; containing `Stale Connection` → `connected=False` (`:472-480`).
- Keepalive: respond `PONG\r\n` to server `PING\r\n` (`:482-488`); client sends `PING\r\n` every 10 s from a daemon thread (`:308-327`), in addition to WS-level ping 30/10.
- Reconnect (`:504-524`): on close, if `running`, sleep 5 s, refresh auth token from DB, regenerate socket token (new keypair), re-run WebSocket. **No retry cap** (`docs/audit/websocket-keepalive-audit.md:182-186`; `docs/audit/websocket-broker-priority.md:433-441`).

### 7.4 Subjects / subscribe / unsubscribe

`format_topic_for_groww(exchange, segment, token, mode)` (`groww_nats.py:365-424`):

| mode | subject |
| --- | --- |
| `index` / `index_ltp` | `/ld/indices/{nse|bse}/price.{token}` |
| `index_depth` | `/ld/indices/{nse|bse}/book.{token}` |
| `ltp` | `/ld/{seg}/{nse|bse}/price.{token}` |
| `depth` | `/ld/{seg}/{nse|bse}/book.{token}` |

`seg`: `CASH→eq`, `FNO→fo`, `COMM→comm`, `CDS→cds`, else `segment.lower()` (`:402-411`). `exchange.lower()`.

In practice `subscribe_batch` only ever stores `mode` `"ltp"` or `"depth"` (`nats_websocket.py:734-759`), so **index subscriptions use the `ltp` form**: for `NSE_INDEX` the adapter substitutes the OpenAlgo symbol for the token (`groww_adapter.py:401-407`) giving e.g. `/ld/eq/nse/price.NIFTY`; for `BSE_INDEX` the numeric CSV token is kept (`:408-413`) giving `/ld/eq/bse/price.<token>`. Depth on an index is redirected to `ltp` (`nats_websocket.py:714-721`; `groww_adapter.py:446-452`).

Wire commands (`groww_nats.py:99-143`): `SUB {subject} {sid}\r\n` (sid = incrementing decimal from 1, `:59-63`); `UNSUB {sid}\r\n`. Batch: send all SUBs back-to-back, then one `PING\r\n`, then sleep 100 ms (`nats_websocket.py:771-808`). Legacy single path `_send_nats_subscription` does SUB + PING + 100 ms (`:651-686`).

Inbound `MSG <subject> <sid> [reply-to] <#bytes>\r\n<payload>\r\n` parsed from the binary frame (`groww_nats.py:197-306`; `nats_websocket.py:329-410`); payload bytes are protobuf. Dispatch matches by `sid` → `nats_sids[sub_key]`, fallback by trailing token of the subject (`price.`→ltp, `book.`→depth) (`nats_websocket.py:548-633`). Each dispatched dict gets `symbol` (OpenAlgo), `exchange` (OpenAlgo exchange, or `NSE`/`BSE` for index mode), `mode` numeric (1 ltp / 3 depth), `string_mode`, `original_exchange` (`:559-577`).

### 7.5 Protobuf wire format (`streaming/groww_protobuf.py`)

Outer message (`:19-25`, `:62-87`):

| Field # | Wire type | Name | Decode |
| --- | --- | --- | --- |
| 1 | length-delimited | `symbol` | UTF-8 string |
| 2 | varint | `segment` | enum `{0: CASH, 1: FNO, 2: CURRENCY, 3: COMMODITY}` (`:302-305`) |
| 3 | varint | `exchange` | enum `{0: BSE, 1: NSE, 2: MCX, 3: MCXSX, 4: NCDEX, 5: GLOBAL, 6: US}` (`:297-300`) |
| 4 | length-delimited | `StocksLivePriceProto` → `ltp_data` | nested below |
| 5 | length-delimited | market depth → `depth_data` | nested below |
| 6 | length-delimited | live indices → `index_data` | nested below |

`StocksLivePriceProto` (`:27-35`, `:171-203`), all values **fixed64 little-endian double**: 1 `tsInMillis` → `timestamp`; 2 `open`; 3 `high`; 4 `low`; 5 `close`; 6 `volume`; 7 `value`; 13 `ltp`. Fields 8–12 skipped.

Market depth (`:205-238`): 1 `tsInMillis` double → `timestamp`; 2 repeated buy level; 3 repeated sell level. Level (`:240-273`): 1 varint `orders`; 2 nested `{1: price double, 2: quantity double → int}`.

Live indices (`:275-295`): 1 `tsInMillis` double; 2 `value` double.

Unknown fields skipped by wire type (`:159-169`). Parser returns partial data on error (`:88-92`).

### 7.6 Adapter behaviour (`streaming/groww_adapter.py`)

- Modes 1 LTP / 2 Quote / 3 Depth; depth level forced to 5 (`:365-374`; `groww_mapping.py:71-88`); `max_subscriptions: 1000` (`groww_mapping.py:172`).
- Token: `SymbolMapper.get_token_from_symbol(symbol, exchange)` → `{token, brexchange}` from SymToken (`websocket_proxy/mapping.py:34-57`); index override as in 7.4.
- Correlation id `f"{symbol}_{exchange}_{mode}"` (`:422`). Subscribe batching: leading-edge flush, 500 ms debounce window (`:100-109`, `:639-726`).
- Depth subscriptions automatically add a **shadow LTP** subscription (`sub_key = f"_shadow_ltp_{correlation_id}"`) because Groww's book topic carries no LTP/OHLC/volume (`:475-521`); a per-token merge cache `(groww_exchange, segment, token)` merges `ltp/open/high/low/close/volume/ltt` from LTP ticks and `depth/ltt` from depth ticks (`:24-85`, `:915-929`).
- Normalisation (`:1061-1224`): LTP-shaped ticks are normalised with mode 2 → `{ltp, ltt}` plus `open/high/low/close/volume/value` **only when non-zero** (`:1082-1105`); depth ticks → `{ltt, depth:{buy:[≤5 non-zero levels], sell:[...]}}` with placeholder `{price:0,quantity:0}` levels filtered (`:1133-1147`); index ticks → `{ltp: value, ltt}` (`:1149-1155`).
- Published payload (`:966-1035`), topic `f"{exchange}_{symbol}_{LTP|QUOTE|DEPTH}"`, ZMQ multipart `[topic, json]` (`websocket_proxy/base_adapter.py:388-408`):
  - common: `symbol, exchange, mode (1|2|3), timestamp (ms now), broker: "groww", topic, subscription_mode`;
  - mode 1: `ltp, ltt`;
  - mode 2: `ltp, ltt, open, high, low, close, volume` (missing → `0.0`/`0`), optional `value`;
  - mode 3: merged `ltp, open, high, low, close, volume, ltt, depth{buy[{price,quantity,orders}],sell[]}` plus top-of-book lifted to `bid, bid_price, bid_qty, bid_size, bid_quantity, ask, ask_price, offer_price, ask_qty, ask_size, ask_quantity, offer_quantity` (only when non-zero). Depth-mode publishes on every LTP *or* depth tick once the cache is non-empty (`:945-954`); LTP/Quote subs ignore depth ticks (`:955-961`).
- Unsubscribe sends `UNSUB` for the key and its shadow; cache entry cleared when no other sub references the token (`:543-637`).

### 7.7 Capability summary for the port

| Capability | Groww |
| --- | --- |
| LTP | yes, `/ld/{eq|fo}/{nse|bse}/price.{token}` |
| Quote (OHLCV) | same LTP topic; OHLCV fields present when Groww sends them non-zero |
| Depth | 5 levels, `/ld/{eq|fo}/{nse|bse}/book.{token}`, no LTP on that topic |
| Index | LTP only; NSE index token = symbol name; BSE index token = numeric |
| OI in stream | not decoded (no field) |

### 7.8 Order updates — REST polling (not a socket)

`PollingOrderUpdateAdapter(broker_name="groww", user_id)` (`websocket_proxy/order_adapter.py:375-520`): every `ORDER_POLL_INTERVAL` seconds (env, default `5`; `.sample.env:157-159`; `order_adapter.py:398`) calls `services.orderbook_service.get_orderbook(auth_token, broker)`, snapshots `(order_status, filled_quantity)` per `orderid`, seeds silently on first poll, and publishes an `OrderUpdateEvent` for every change (`:437-470`, `:478-520`). Since groww's `transform_order_data` emits no `filled_quantity`, the diff reduces to `order_status` transitions (quirk 9.8).

---

## 8. Rate limits, batch sizes, sleeps, pagination (as coded)

| Item | Value | Cite |
| --- | --- | --- |
| Order list page size | 25 per page, both segments | `api/order_api.py:97` |
| Trades page size | 50, page 0 only | `api/order_api.py:3368-3369` |
| OHLC batch size | 50 symbols per request (`BATCH_SIZE = 50`, "Groww API limit: up to 50 instruments per request") | `api/data.py:1881`; `.claude/skills/broker-integration/references/cross-broker-reference.md:36` |
| Delay between OHLC batches | 0.2 s | `api/data.py:1882, 1901-1902` |
| Per-symbol quote overlay spacing | 0.25 s (~4 rps, "observed sustained safe rate") | `api/data.py:2233-2234` |
| Overlay abort | after 4 consecutive 429/"Rate limit" errors | `api/data.py:2237, 2403-2411` |
| Invalid-symbol retry depth | 5 | `api/data.py:2096` |
| History chunking | 3/7/15/100/300 days by interval | `api/data.py:374-383` |
| Position book cache | 1 s TTL, invalidated after each smart order | `api/order_api.py:1479-1484` |
| Order poll interval | 5 s default | `.sample.env:159` |
| NATS keepalive | PING every 10 s; WS ping 30 s / timeout 10 s | `nats_websocket.py:271-275, 308-327` |
| Reconnect delay | fixed 5 s, unbounded retries | `nats_websocket.py:513` |
| Subscribe batch debounce | 0.5 s | `groww_adapter.py:109` |
| Batch SUB flush wait | 0.1 s | `nats_websocket.py:806` |
| Max WS subscriptions | 1000 (declared) | `groww_mapping.py:172` |

Groww's published API rate limits (orders/sec, live-data/sec, per-minute quotas) are **not encoded anywhere in the code**; there is no order-path throttle. Only the live-data pacing above exists.

---

## 9. Quirks, hazards and known defects the porter must decide on

1. **Order-book exchange misclassification.** `any(suffix in groww_symbol for suffix in ["CE","PE","C","P"])` (`api/order_api.py:178`) treats any symbol containing the letter `C` or `P` (e.g. `TCS`, `HDFCBANK`, `SBIN` no, `ITC` yes) as an option and sets `exchange = "NFO"`. Recommend: use `segment == "FNO"` (and the stored `brexchange`) instead.
2. **`get_oa_symbol` called with a token.** `get_oa_symbol(token, "NFO")` (`api/order_api.py:212`) but `get_oa_symbol(brsymbol, exchange)` looks up by `brsymbol` (`database/token_db_enhanced.py:937-949`); the by-token path never hits and the code falls to the direct DB lookup by `brsymbol` (`:231-246`), which is the one that works. Positions use it correctly (`:965`, `:1170`).
3. **Undefined `SEGMENT_CURRENCY` / `SEGMENT_COMMODITY`.** Referenced at `api/order_api.py:2492, 2494, 3135, 3137, 3314, 3316` but never defined/imported → `NameError` if an order with `segment` `CURRENCY`/`COMMODITY` is encountered. Plugin only supports CASH/FNO anyway.
4. **`init_groww_client` undefined** at `api/order_api.py:1928` (dead SDK test helper `direct_place_order`).
5. **Modify/cancel always "success".** HTTP errors and exceptions are reported as `status: "success"` (`:2670-2677`, `:2686-2709`, `:2947-2953`, `:2965-2971`). FNO detection by `CE`/`PE`/`FUT` substring of the **order id** (`:2501-2510`, `:2533`) is heuristic; prefer passing the known segment.
6. **STOP_LOSS naming mismatch.** Outbound `SL → "STOP_LOSS_LIMIT"` (`mapping/transform_data.py:27`); inbound reverse map recognises only `"STOP_LOSS"` (`mapping/order_data.py:410`). Verify against Groww's actual enum.
7. **Positions lose average price and BSE.** `transform_positions_data` reads camelCase keys (`avgPrice`, `buyQty`, `sellQty`, `lastPrice`, `closePrice`) that `get_positions` does not emit (`mapping/order_data.py:837-846` vs `api/order_api.py:1080-1108`) and re-derives exchange as `NFO` or `NSE` only (`:778-783`). Paise/rupee handling is heuristic: `net_price/100 if > 1000`; `credit_price/100`, `debit_price/100` unconditionally (`api/order_api.py:946-948, 1093-1097`). Trade prices are rupees (do not scale) — regression test `test/test_groww_tradebook_price.py`.
8. **Order updates by status diff only** (7.8) because `transform_order_data` omits `filled_quantity`/`average_price` (`mapping/order_data.py:465-477`).
9. **Master contract `name` column is always `""`** and CSV `name` overwrites the `groww_symbol` working column (`database/master_contract_db.py:563-564, 616-618`). Token dedupe is on `token` alone across all exchanges (`:65-68`).
10. **BFO symbols not normalised** to OpenAlgo `[BASE][DDMMMYY][STRIKE][CE|PE]` form (`:709`), and `OPT`-typed rows are never rebuilt. `reformat_symbol`/`assign_values` are dead.
11. **EOD candle timestamps are midnight UTC**, not 09:15 IST (`api/data.py:516-522`); intraday candles outside 09:15–15:30 IST are discarded (`:608-626`); `oi` always 0 (`:909`). Weekly is resampled client-side from daily (`:714-838`).
12. **Three competing beliefs about Groww's F&O symbol text:** CSV `trading_symbol` (authoritative, stored as `brsymbol`), the space-separated fallback `"NIFTY 29MAY25 24500 CE"` (`master_contract_db.py:154`), the compact `NIFTY25SEP24500CE` (`api/data.py:137-164`), and the order-book regexes expecting `NIFTY25051334000CE` (`api/order_api.py:258-259`). Always resolve via SymToken; treat the rest as fallbacks.
13. **Index streaming tokens**: NSE indices subscribe with the *symbol* as token (`groww_adapter.py:401-407`), BSE indices with the numeric token (`:408-413`). The stored SymToken `token` for an NSE index is therefore not what the feed uses.
14. **Quote `ohlc` may be a non-JSON string** `"{open: 149.50,high: 150.50,low: 148.50,close: 149.50}"` (`api/data.py:1243-1261`, `:2145-2167`); parse by splitting on `,` then `:`.
15. **REST depth pads to 5 zero levels; streaming filters zero levels** (`api/data.py:1818-1821` vs `groww_adapter.py:1139-1145`).
16. **Streaming auth fallback**: if the socket-token call fails, the plain auth token is sent as the NATS `jwt` with `X-Subscription-Id: direct_auth` and no nkey signature (`nats_websocket.py:223-238`).
17. **Reconnect is recursive and uncapped** (`nats_websocket.py:504-524`); audits recommend a `while running` loop with a cap (`docs/audit/websocket-broker-priority.md:433-441`).
18. **`order_reference_id` rules** (8–20 chars, `[A-Za-z0-9-]`, ≤2 hyphens, zero-padded) are enforced client-side (`api/order_api.py:1663-1689`); the generic `transform_data` path instead uses `strategy[:8].ljust(8,"0")` (`mapping/transform_data.py:95-98`) but is unused.
19. **`segment` is required** on list/positions/trades/cancel/modify/margin/quote/ohlc/history; only holdings, funds and the token endpoints omit it.
20. **Groww order id prefixes observed in code**: `GMK...` (CASH), `GMKFO...`/`GLTFO...` (FNO) (`api/order_api.py:638, 2474, 3298, 3343`).

---

## 10. Minimal JSON examples (literal shapes the code sends)

Place order:
```json
POST https://api.groww.in/v1/order/create
{"trading_symbol":"RELIANCE","quantity":1,"validity":"DAY","exchange":"NSE","segment":"CASH","product":"CNC","order_type":"LIMIT","transaction_type":"BUY","order_reference_id":"20261003-3fa85f64","price":2500.5}
```
Modify: `{"groww_order_id":"GMK...","order_type":"LIMIT","segment":"CASH","quantity":2,"price":2501.0}`
Cancel: `{"segment":"FNO","groww_order_id":"GLTFO..."}`
Margin: `POST /v1/margins/detail/orders?segment=FNO` body `[{"trading_symbol":"NIFTY25OCT24500CE","transaction_type":"BUY","quantity":75,"order_type":"MARKET","product":"NRML","exchange":"NSE"}]`
Quote: `GET /v1/live-data/quote?exchange=NSE&segment=FNO&trading_symbol=<brsymbol>`
OHLC: `GET /v1/live-data/ohlc?segment=CASH&exchange_symbols=NSE_SBIN,NSE_TCS,BSE_RELIANCE`
History: `GET /v1/historical/candle/range?exchange=NSE&segment=CASH&trading_symbol=SBIN&start_time=2026-09-01%2009:15:00&end_time=2026-09-03%2015:30:00&interval_in_minutes=5`
Socket token: `POST /v1/api/apex/v1/socket/token/create/` body `{"socketKey":"U..."}` → `{"token":"<jwt>","subscriptionId":"..."}`
NATS: `CONNECT {...}\r\nPING\r\n`, `SUB /ld/eq/nse/price.2885 1\r\n`, `SUB /ld/eq/nse/book.2885 2\r\n`, `UNSUB 2\r\n`


---

# Part C - Recommended Rust module layout and Broker-trait additions

## C.1 Module layout (one directory per broker, mirrors the web `broker/<name>/` contract)

```
src-tauri/src/brokers/
  mod.rs                 # Broker trait, BrokerRegistry, capability flags, re-exports
  types.rs               # common request/response structs (extended, see C.3)
  common/
    mod.rs
    http.rs              # shared reqwest::Client (pooled, rustls, explicit timeouts), retry/backoff helper
    ratelimit.rs         # token-bucket / rolling-window limiter keyed by (broker, category)
    symbols.rs           # SymbolResolver trait: oa->br symbol/token/exchange and br->oa reverse lookups
    mapping.rs           # OpenAlgo enums: Exchange, Product, PriceType, Action, Validity, OrderStatus (+ Display/FromStr)
    history.rs           # Candle, Interval, chunking loop helper (chunk_days, +5:30 daily shift, sort+dedupe)
    streaming.rs         # BrokerFeed trait, NormalizedTick, DepthLevel, reconnect/backoff/watchdog driver
    master_contract.rs   # CSV/JSON download helpers, expiry formatting (DD-MMM-YY), strike formatting, index renames
  <broker>/
    mod.rs               # pub struct <Name>Broker { cfg, http }  +  impl Broker (thin: delegates to the files below)
    auth.rs              # authenticate(): login/redirect/token exchange; AuthToken (de)serialisation
    orders.rs            # place/modify/cancel/cancel_all/close_all/get_open_position + book endpoints
    data.rs              # quotes, multiquotes, depth, history, intervals (timeframe map)
    funds.rs             # get_funds() + margin calculator (margin_api equivalent)
    gtt.rs               # optional; present only for dhan/zerodha
    mapping.rs           # OpenAlgo <-> broker enum maps, order/trade/position/holding normalisers (= transform_data.py + order_data.py)
    master_contract.rs   # download + parse -> Vec<SymbolData> (= database/master_contract_db.py)
    streaming.rs         # impl BrokerFeed: URL, handshake, subscribe frames, binary/JSON/protobuf parsing (= streaming/*)
    proto/ (if needed)   # .proto + prost-generated code (upstox, groww)
```

Family brokers (Part D) share code under `brokers/families/noren/` and `brokers/families/xts/`, each exposing a generic `NorenBroker` / `XtsBroker` parametrised by a `&'static Config`; the per-broker directory then shrinks to `mod.rs` holding the config constant and any hook overrides.

## C.2 Broker trait additions for MVP parity

```rust
#[async_trait]
pub trait Broker: Send + Sync {
    // ---- identity / capabilities ----
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn logo(&self) -> &'static str;
    fn login_kind(&self) -> LoginKind;                 // replaces requires_totp(); see C.4
    fn supported_exchanges(&self) -> &'static [&'static str]; // from plugin.json
    fn capabilities(&self) -> Capabilities;            // bitflags: HISTORY, MULTIQUOTES, MARGIN, GTT, ORDER_FEED, DEPTH_20, DEPTH_50

    // ---- auth ----
    async fn authenticate(&self, creds: BrokerCredentials) -> Result<AuthResponse>;
    fn login_url(&self, creds: &BrokerCredentials, redirect_uri: &str) -> Option<String>; // OAuth brokers

    // ---- orders (existing, with corrected signatures) ----
    async fn place_order(&self, auth: &AuthToken, order: &ResolvedOrder) -> Result<OrderResponse>;
    async fn modify_order(&self, auth: &AuthToken, order: &ResolvedModify) -> Result<OrderResponse>;
    async fn cancel_order(&self, auth: &AuthToken, order_id: &str, variety: Option<&str>) -> Result<()>;
    async fn cancel_all_orders(&self, auth: &AuthToken) -> Result<CancelAllResult>;      // NEW: (cancelled[], failed[])
    async fn close_all_positions(&self, auth: &AuthToken) -> Result<Vec<OrderResponse>>;  // NEW: default impl = positions + market orders
    async fn get_open_position(&self, auth: &AuthToken, oa_symbol: &str, exchange: &str, product: &str) -> Result<i64>; // NEW: net qty

    // ---- books (symbols already normalised to OpenAlgo) ----
    async fn get_order_book(&self, auth: &AuthToken) -> Result<Vec<Order>>;
    async fn get_trade_book(&self, auth: &AuthToken) -> Result<Vec<Trade>>;               // NEW type (avg price, trade value, fill time)
    async fn get_positions(&self, auth: &AuthToken) -> Result<Vec<Position>>;
    async fn get_holdings(&self, auth: &AuthToken) -> Result<Vec<Holding>>;
    async fn get_funds(&self, auth: &AuthToken) -> Result<Funds>;                       // Funds gains m2m_unrealized / m2m_realized / utilised_debits
    async fn calculate_margin(&self, auth: &AuthToken, legs: &[MarginLeg]) -> Result<MarginResult>; // NEW; default Err(Unsupported)

    // ---- market data ----
    async fn get_quote(&self, auth: &AuthToken, key: &QuoteKey) -> Result<Quote>;
    async fn get_multiquotes(&self, auth: &AuthToken, keys: &[QuoteKey]) -> Result<Vec<QuoteResult>>; // NEW; default = loop get_quote; override with batch endpoint + per-broker BATCH_SIZE
    async fn get_market_depth(&self, auth: &AuthToken, key: &QuoteKey) -> Result<MarketDepth>;      // pad/truncate to 5 levels
    async fn get_history(&self, auth: &AuthToken, req: &HistoryRequest) -> Result<Vec<Candle>>;      // NEW; epoch seconds, 7 columns incl. oi
    fn timeframe_map(&self) -> &'static [(&'static str, &'static str)];                          // NEW; keys advertised by /intervals

    // ---- GTT (optional) ----
    async fn place_gtt(&self, auth: &AuthToken, req: &GttRequest) -> Result<GttResponse> { Err(AppError::Unsupported("gtt")) }
    async fn modify_gtt(..) / cancel_gtt(..) / get_gtt_book(..)  // same default

    // ---- master contract ----
    async fn download_master_contract(&self, auth: &AuthToken) -> Result<Vec<SymbolData>>; // SymbolData must be persisted with expiry/strike/option_type

    // ---- streaming ----
    fn create_feed(&self, auth: &AuthToken, creds: &BrokerCredentials) -> Result<Box<dyn BrokerFeed>>; // NEW adapter factory
    fn create_order_feed(&self, ..) -> Option<Box<dyn OrderFeed>> { None }                            // optional
}
```

Supporting types:

```rust
pub struct AuthToken { pub raw: String, pub parts: HashMap<&'static str, String> } // e.g. zerodha "api_key:access_token", kotak token/sid/hsServerId, xts interactive/market tokens
pub struct QuoteKey { pub exchange: String, pub symbol: String }                  // fixes the (exchange,symbol) tuple ambiguity (Part 0.4)
pub struct ResolvedOrder { pub oa: OrderRequest, pub brsymbol: String, pub token: String, pub brexchange: String, pub lot_size: i32, pub tick_size: f64 }
pub struct HistoryRequest { pub key: QuoteKey, pub token: String, pub brsymbol: String, pub interval: String, pub from: NaiveDate, pub to: NaiveDate }
pub struct Candle { pub timestamp: i64, pub open: f64, pub high: f64, pub low: f64, pub close: f64, pub volume: i64, pub oi: i64 }
pub struct MarginLeg { pub key: QuoteKey, pub action: Action, pub quantity: i32, pub product: Product, pricetype: PriceType, price: f64, trigger_price: f64 }
pub struct MarginResult { pub total_margin_required: f64, pub span_margin: f64, pub exposure_margin: f64, pub total_charges: Option<f64> }
```

Services do the OpenAlgo-symbol -> broker-symbol resolution once (`ResolvedOrder`), so adapters never see an unresolved OpenAlgo symbol, and adapters do the reverse mapping via `common::symbols::SymbolResolver` before returning books (fixes Part 0.3). `SymbolResolver` needs a third cache index `exchange:brsymbol -> token`.

## C.3 Streaming adapter contract (replaces the 3-way match in `websocket/manager.rs`)

```rust
#[async_trait]
pub trait BrokerFeed: Send + Sync {
    fn ws_request(&self) -> Result<http::Request<()>>;                 // URL + headers (angel: 4 headers; fyers: NO Authorization header)
    async fn on_connected(&mut self, tx: &mut FeedSink) -> Result<()>; // handshake frames (fyers HSM auth, kotak "cn", noren "c", xts none)
    fn subscribe_frames(&self, subs: &[FeedSubscription]) -> Vec<Message>;   // one or more frames (zerodha needs 2)
    fn unsubscribe_frames(&self, subs: &[FeedSubscription]) -> Vec<Message>;
    fn parse(&mut self, msg: Message) -> Vec<FeedEvent>;               // FeedEvent::{Tick(NormalizedTick), Depth(NormalizedDepth), AuthOk, AuthFailed(String), Heartbeat, OrderUpdate(..)}
    fn heartbeat(&self) -> Option<(Duration, Message)>;                // angel "ping" text; noren {"t":"h"}; zerodha None
    fn supported_depth_levels(&self) -> &'static [u8];                 // [5], fyers [5,50], dhan [5,20]
    fn needs_token_refresh(&self, close: &CloseFrame) -> bool;         // 403 / 1008 -> re-login path
}
pub struct FeedSubscription { pub key: QuoteKey, pub token: String, pub brexchange: String, pub brsymbol: String, pub mode: Mode /*Ltp|Quote|Depth*/, pub depth: u8 }
pub struct NormalizedTick { symbol, exchange, mode, ltp, open, high, low, close, volume, average_price, last_quantity, total_buy_quantity, total_sell_quantity, oi, change, change_percent, timestamp_ms }
pub struct NormalizedDepth { symbol, exchange, ltp, buy: Vec<DepthLevel{price,quantity,orders}>, sell: Vec<DepthLevel>, timestamp_ms }
```

`WebSocketManager` becomes broker-agnostic: owns the socket, the command channel, reconnect with exponential backoff, stall watchdog, re-subscribe on reconnect, and emits `market_tick` / `market_depth` Tauri events plus (for the external WS server on 8765) the OpenAlgo client-facing payloads from `W:docs/prompt/websockets-format.md` (mode 1/2/3, `depth.buy/sell`, `last_trade_quantity`, `avg_trade_price`).

## C.4 Login kinds (drives the UI form instead of `requires_totp()`)

```rust
pub enum LoginKind {
    Redirect { param: &'static str },                       // zerodha request_token, fyers auth_code, upstox code, dhan tokenId, flattrade code, paytm requestToken
    DirectTotp { fields: &'static [&'static str] },         // angel: client_id,password,totp ; mstock: password then totp (2 step)
    TwoStep { step1: &'static [&'static str], step2: &'static [&'static str] }, // kotak: mobile/ucc + TOTP, then MPIN
    AccessToken,                                             // dhan direct token, groww access-token variant, nubra, tradejini
    ApiKeySecret,                                            // deltaexchange (HMAC), XTS (dual key pairs -> extra fields)
}
```

## C.5 Minimum fix order for the existing three brokers (from Part A)
1. `SymbolInfo` + `symtoken` persistence of expiry/strike/option_type; add brsymbol reverse index.
2. `QuoteKey` struct; fix angel/zerodha tuple order; send `brsymbol`/token in quotes/depth.
3. Zerodha CSV column indices; fyers derivative symbol order (DDMMMYY); angel `X-PrivateKey` on every call.
4. Normalise book symbols/statuses in the adapters (lowercase statuses; `get_oa_symbol`).
5. Streaming: zerodha access_token in URL + two subscribe frames + exchange from subscription map; fyers hsm_key from JWT, no Authorization header, `sf|..` topics; angel raw JWT header + flag-based depth; emit depth events.
6. Add `get_history`/`timeframe_map`/`get_multiquotes`/`calculate_margin`/`cancel_all_orders`/`get_open_position` to the trait and wire `HistoryService`/`QuotesService`.


---

# Part D - Family-based plan for all 36 web broker plugins

Classification method: every `W:broker/*/plugin.json` was read for `broker_type` and `supported_exchanges`; every `api/*.py` was grepped for the family telltales named in `W:.claude/skills/broker-integration/SKILL.md` ("Step 1"): Noren (`/NorenWClientTP/`, `jData=`/`jKey=` form bodies), Symphony XTS (`/interactive/user/session`, `/apimarketdata/`, `BROKER_API_KEY_MARKET`), TOTP in `auth_api.py`, and the login URL hosts. Where the SKILL family table and the code disagree (SKILL lists `ibulls`/`wisdom` as Noren; the code uses XTS endpoints), the code wins and the D-xts section documents the resolution.

## D.0 Classification of the 36 directories

| # | Dir | Family | Login shape | supported_exchanges (plugin.json) | Notes |
| --- | --- | --- | --- | --- | --- |
| 1 | angel | Direct TOTP | clientcode + password/PIN + TOTP | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX,MCX_INDEX | Desktop exists (Part A). GTT + margin modules present. |
| 2 | zerodha | OAuth/checksum | request_token -> sha256(api_key+request_token+secret) | NSE,BSE,NFO,BFO,CDS,MCX,NCO,NSE_INDEX,BSE_INDEX,MCX_INDEX,GLOBAL_INDEX | Desktop exists (Part A). GTT + margin. Only broker with NCO. |
| 3 | fyers | OAuth/checksum | auth_code -> appIdHash | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | Desktop exists (Part A). 50-level TBT socket; GTT file present. |
| 4 | upstox | OAuth | code -> /v2/login/authorization/token | NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX,GLOBAL_INDEX | Spec in Part B. Protobuf feed. GTT. |
| 5 | dhan | OAuth (partner consent) or pasted access token | tokenId consume / access token | NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX | Spec in Part B. Binary feed, 20-depth feed, GTT (forever orders). |
| 6 | dhan_sandbox | dhan clone | pasted token (+TOTP helper) | same as dhan | Delta documented in the OAuth/redirect section below (D8). |
| 7 | kotak | Direct, two-step | mobile/UCC + TOTP, then MPIN | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | Spec in Part B. jData form bodies but NOT Noren. History IS implemented (`W:broker/kotak/api/data.py:966` `/market-data/1.0/historical/details`, NSE/BSE/NFO/BFO only); the skill reference saying otherwise is stale. |
| 8 | groww | Direct token (TOTP or approval checksum) | api key + TOTP / secret checksum / pasted token | NSE,BSE,NFO,BFO,NSE_INDEX,BSE_INDEX | Spec in Part B. REST polling for order updates. |
| 9 | shoonya | Noren | redirect -> code -> `/NorenWClientAPI/GenAcsTok` checksum sha256(client_id+secret+code) | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | Canonical Noren template. No in-tree Noren broker uses QuickAuth (uid/pwd/factor2); all four classic members use the redirect + access-token flow, firstock a TOTP form. |
| 10 | flattrade | Noren | redirect -> request_code -> /trade/apitoken | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | Persistent streaming adapter; dual rolling-window limiter. |
| 11 | tradesmart | Noren | redirect -> code -> GenAcsTok (`/NorenWClientAPIv2`) | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | |
| 12 | zebu | Noren | redirect -> code -> GenAcsTok (`go.mynt.in/NorenWClientAPI`) | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX | No BSE_INDEX. |
| 13 | firstock | Noren-derived (JSON transport) | uid/pwd/TOTP at `api.firstock.in/V1/login` | NSE,BSE,NFO,BFO,NSE_INDEX | Different transport from Noren; needs own struct or transport hook. |
| 14 | fivepaisaxts | XTS | interactive + market app keys | NSE,BSE,NFO,BFO,NSE_INDEX,BSE_INDEX | Canonical XTS template. |
| 15 | jainamxts | XTS | dual keys | NSE,BSE,NFO,BFO,NSE_INDEX,BSE_INDEX | |
| 16 | compositedge | XTS | dual keys | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | |
| 17 | rmoney | XTS | dual keys | NSE,BSE,NFO,BFO,NSE_INDEX,BSE_INDEX | |
| 18 | ibulls | XTS (code), listed as Noren in SKILL | dual keys | NSE,BSE,NFO,BFO,MCX,NSE_INDEX,BSE_INDEX | Code uses `/interactive/user/session` + `/apibinarymarketdata`; SKILL family table is wrong here. |
| 19 | wisdom | XTS (code), listed as Noren in SKILL | dual keys | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | Code uses XTS endpoints; SKILL family table is wrong here. |
| 20 | iifl | XTS | dual keys | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | |
| 21 | iiflcapital | Bespoke (confirmed: REST `https://api.iiflcapital.com/v1` + MQTT feed; NOT XTS) | - | NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX | Has an order-update stream adapter that emits events (SKILL calls it an exception). |
| 22 | arrow | API key + redirect | - | NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX | Binary feed with documented offset pitfalls; MCXFO quote code. |
| 23 | paytm | OAuth | requestToken -> /accounts/v2/gettoken | NSE,BSE,NFO,BFO,NSE_INDEX,BSE_INDEX | |
| 24 | aliceblue | API key + session | - | NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX | No REST quote API; quotes via websocket. |
| 25 | definedge | Direct (OTP) + susertoken | - | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | |
| 26 | pocketful | OAuth | redirect `trade.pocketful.in` | NSE,BSE,NFO,BFO,MCX,NSE_INDEX,BSE_INDEX | |
| 27 | hdfcsky | OAuth-like | - | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | |
| 28 | hdfcsecurities | OAuth-like | - | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | No margin module. |
| 29 | mstock | Direct TOTP (2 step) | password -> TOTP (`typeb` API) | NSE,BSE,NFO,BFO,CDS,NSE_INDEX,BSE_INDEX | |
| 30 | motilal | Direct TOTP | password + TOTP (+DOB) | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | |
| 31 | samco | Direct | userid/password/yob | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | |
| 32 | tradejini | Direct TOTP / token | - | NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX | |
| 33 | fivepaisa | Direct TOTP | TOTPLogin -> GetAccessToken | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX | Not XTS (that is fivepaisaxts). |
| 34 | nubra | Direct TOTP | - | NSE,BSE,NFO,BFO,MCX,NSE_INDEX,BSE_INDEX | |
| 35 | indmoney | Direct TOTP (INDstocks) | - | NSE,BSE,NFO,BFO,NSE_INDEX,BSE_INDEX | API host `api.indstocks.com`. |
| 36 | deltaexchange | Crypto, HMAC API key | api key + secret | CRYPTO | `broker_type: crypto`, only `leverage_config: true` plugin. No family. |

Web-side streaming registry (`W:websocket_proxy/__init__.py:122-150`) explicitly registers 29 adapters; aliceblue, deltaexchange, dhan_sandbox, firstock, groww, tradejini, zebu rely on the dynamic-import fallback `broker.<name>.streaming.<name>_adapter`.

## D.1 Rust design for 36 brokers

- `brokers/families/noren/{mod.rs, auth.rs, orders.rs, data.rs, funds.rs, mapping.rs, master_contract.rs, streaming.rs}` exposing `pub struct NorenBroker { cfg: &'static NorenConfig }` and `brokers/families/xts/...` exposing `pub struct XtsBroker { cfg: &'static XtsConfig }`. Both implement the Part C `Broker` trait once. Each member broker directory holds only `mod.rs` with a `pub static CONFIG: NorenConfig = NorenConfig { .. }` (+ `register!` into `BrokerRegistry`), and optional hook overrides via `cfg.hooks: &'static NorenHooks` (fn pointers for the few divergent behaviours the D-noren / D-xts tables identify: login variant, funds formula, master URL set, index naming, persistent-socket flag).
- Shared OAuth helpers (`common/oauth.rs`): `sha256_checksum(parts: &[&str])`, redirect-URL builder (`http://127.0.0.1:<port>/<broker>/callback`), form-vs-json token exchange; used by zerodha/upstox/fyers/paytm/flattrade/pocketful.
- Direct-login helpers (`common/login.rs`): TOTP generation (`totp-rs`) when the user stores a TOTP secret, two-step state machine for kotak/mstock.
- Streaming: each family provides one `BrokerFeed` impl; families whose brokers differ only by URL (Noren, XTS) read the URL from config.
- Crypto: `deltaexchange` gets its own directory implementing the same trait with `supported_exchanges = ["CRYPTO"]`, a `leverage` extension trait (`set_leverage/get_leverage`) gated by `Capabilities::LEVERAGE`, and symbol rules from `W:docs/prompt/crypto-symbol-format.md`.
- Suggested cadence: families first (Noren = 5 brokers, XTS = 7-8 brokers for the cost of one implementation each), then the four Part B brokers, then the bespoke direct/OAuth brokers in order of user demand.

The sections below give the shared template behaviour and the per-broker delta tables for each family, then medium-depth specs for brokers that fit no family.

---

## D. NOREN / Finvasia broker family — port spec for `NorenBroker<Config>`

Scope: `broker/{shoonya,flattrade,tradesmart,zebu,firstock}/` in the OpenAlgo Python web app.
All paths below are relative to `/Users/openalgo/openalgo-desktop/openalgo/`. Every fact cites `file:line` as read on 2026-10-03. READ-ONLY survey; nothing was modified.

### 0. Headline findings (read these before designing)

1. **No in-tree Noren broker uses `QuickAuth` any more.** `grep -rn 'QuickAuth|"factor2"|apkversion'` over `broker/ utils/ services/ blueprints/` returns nothing. All five brokers are redirect-based:
   - shoonya, zebu, tradesmart: OAuth redirect → `?code=` → `POST .../GenAcsTok` with `checksum = sha256(client_id + secret + code)` → `access_token` (Bearer).
   - flattrade: redirect → `?code=` → `POST https://authapi.flattrade.in/trade/apitoken` with `api_secret = sha256(api_key + code + secret)` → `token` (jKey).
   - firstock: direct login form (userid/password/TOTP) → `POST https://api.firstock.in/V1/login` JSON → `susertoken` (jKey in JSON body).
   A `NorenLogin::QuickAuth` variant is still worth keeping in the Rust enum for other Noren white-labels, but it has no reference implementation in this tree.
2. **Two REST transport dialects exist inside the "classic" Noren group**, and they are the single biggest per-broker delta:
   - **Dialect A — Bearer header**: body `jData=<json>` with `Content-Type: text/plain` + `Authorization: Bearer <token>`. Used by shoonya (`/NorenWClientAPI/`), zebu (`/NorenWClientAPI/`), tradesmart (`/NorenWClientAPIv2/`). (`broker/shoonya/api/order_api.py:37-46`, `broker/zebu/api/order_api.py:37-46`, `broker/tradesmart/api/baseurl.py:74-96`)
   - **Dialect B — jKey in body**: body `jData=<json>&jKey=<token>` with `Content-Type: application/x-www-form-urlencoded`, no auth header. Used by flattrade (`/PiConnectAPI/`) everywhere (`broker/flattrade/api/data.py:86-91`), and by **shoonya for the two chart endpoints only** (`broker/shoonya/api/data.py:286-311` — "Chart endpoints want jData=<json>&jKey=<token> form-urlencoded, NOT a Bearer header"; the legacy `/NorenWClientTP/` path "answers 502").
   - The `/NorenWClientTP/` prefix named in the task brief is **not used by any of the five** brokers today.
3. **Firstock is a Noren derivative only in vocabulary** (prd codes C/M/I, prctyp LMT/MKT/SL-LMT/SL-MKT, trantype B/S, order statuses, `norenordno` on its WS). Its transport is a bespoke JSON REST API at `https://api.firstock.in/V1/<camelCase>` with `jKey` and `userId` as JSON body members, camelCase field names, `{"status":"success","data":...}` envelopes, and a different WebSocket (`wss://socket.firstock.in/V2/ws`, token in URL query, `{"action":"subscribe","tokens":"NSE:26000|..."}`). It needs its own transport layer (see §C).
4. **Byte-identical-modulo-URL claim**: nothing is literally byte-identical. The closest pair is **shoonya ↔ zebu** (same Bearer dialect, same `/NorenWClientAPI/` paths, same funds formula, same master-contract zip layout); but zebu differs in MPP scope (MARKET only, not SL-M), tick-size scaling, index renames, no BSE indices, no margin API, no `&`→`&` escaping, and no shoonya-specific quote-identity guard. **tradesmart** is the shoonya dialect with `/NorenWClientAPIv2/` paths, a composite stored token `uid:::access_token`, `remarks:"openalgo"` and no `mkt_protection` on PlaceOrder, a per-leg `GetOrderMargin`, and a WebSocket-backed multiquote. **flattrade** is dialect B with S3 CSV masters, a dual rolling-window limiter, polling order updates and a persistent-session WS. Details in §B.

---

### A. Shared template behaviour (canonical: `broker/shoonya/`)

### A1. Credentials / environment

| Env | Meaning (classic Noren) | Source |
|---|---|---|
| `BROKER_API_KEY` | composite `"<userid>:::<client_id_or_api_key>"`; `[0]` = trading uid/actid used in every `jData`; `[1]` = OAuth client_id (shoonya/zebu/tradesmart) or API key (flattrade) | `broker/shoonya/api/auth_api.py:17-21`, `broker/shoonya/api/funds.py:17-23`, `broker/flattrade/api/auth_api.py:27`, `broker/zebu/api/auth_api.py:16-18` (`Z56004:::Z56004_U`), `broker/tradesmart/api/baseurl.py:14-25` |
| `BROKER_API_SECRET` | OAuth secret (checksum ingredient) | `broker/shoonya/api/auth_api.py:22`, `broker/flattrade/api/auth_api.py:28` |
| `utils/config.py` | only exposes `get_broker_api_key()`/`get_broker_api_secret()` (lines 11-28); the `:::` split is done in each broker module, not centrally | `utils/config.py:11-28` |

Firstock inverts this: `BROKER_API_KEY` = **vendorCode** (`"<USERID>_API"`), `BROKER_API_SECRET` = **apiKey**; userId is derived by stripping the suffix (`api_key[:-4]` in `broker/firstock/api/order_api.py:35`, `broker/firstock/api/funds.py:27`, `broker/firstock/api/data.py:38`; `.replace("_API","")` in `broker/firstock/api/margin_api.py:36`, `broker/firstock/mapping/transform_data.py:34-35`, `broker/firstock/streaming/firstock_adapter.py:87`, `broker/firstock/database/master_contract_db.py:696`). (`broker/firstock/api/auth_api.py:42-43`)

### A2. Login flows (literal)

**Variant 1 — OAuth redirect + GenAcsTok (shoonya, zebu, tradesmart).**

Redirect built in `blueprints/brlogin.py`:
- shoonya: `https://api.shoonya.com/OAuthlogin/authorize/oauth?client_id={client_id}` (`blueprints/brlogin.py:621-623`), client_id = `BROKER_API_KEY.split(":::",1)[1]` (`:614-620`). Callback `GET /shoonya/callback?code=...` (`:600-604`).
- zebu: `https://go.mynt.in/OAuthlogin/authorize/oauth?client_id={client_id}` (`:594-597`).
- tradesmart: `https://v2api.tradesmartonline.in/OAuthlogin/authorize/oauth?client_id={client_id}` (`:752-756`); accepts `code | request_token | request-token` (`:719-723`); manual fallback `?access_token=<TOKEN>&uid=<CLIENT_ID>` stores `"{uid}:::{token}"` (`:727-735`).

Token exchange (shoonya; zebu/tradesmart identical modulo host/path):
```
POST https://api.shoonya.com/NorenWClientAPI/GenAcsTok
Content-Type: text/plain
jData={"code": "<code>", "checksum": "<sha256(client_id + secret_key + code)>"}
→ {"stat":"Ok","access_token":"..."}   (error: {"stat":"Not_Ok","emsg":"..."})
```
(`broker/shoonya/api/auth_api.py:30-58`; zebu `broker/zebu/api/auth_api.py:26-52`; tradesmart `broker/tradesmart/api/auth_api.py:49-74`, URL `broker/tradesmart/api/baseurl.py:38`). tradesmart additionally accepts `accesstoken|token|susertoken` as the token key and `actid|uid|accountId|actId|client_id|uname` as the uid, and stores the composite `"{uid}:::{token}"` (`broker/tradesmart/api/auth_api.py:66-93`).

**Variant 2 — Flattrade request_code exchange.**
Login URL is built in the React frontend, not Flask: `https://auth.flattrade.in/?app_key=${flattradeApiKey}` (`frontend/src/pages/BrokerSelect.tsx:178-180`). Callback `GET /flattrade/callback?code=...&client=...` (`blueprints/brlogin.py:705-714`; `client` is ignored).
```
POST https://authapi.flattrade.in/trade/apitoken
Content-Type: application/json
{"api_key": "<api_key>", "request_code": "<code>", "api_secret": "<sha256(api_key + code + secret)>"}
→ {"stat":"Ok","token":"..."}
```
(`broker/flattrade/api/auth_api.py:33-54`). Note the **hash order differs**: flattrade `api_key+code+secret` vs GenAcsTok `client_id+secret+code` (`broker/tradesmart/api/auth_api.py:9-11` calls this out).

**Variant 3 — Firstock direct TOTP login.** `GET /firstock/callback` redirects to React page `/broker/firstock/totp`; `POST` form fields `userid`, `password`, `totp` (`blueprints/brlogin.py:625-636`).
```
POST https://api.firstock.in/V1/login
Content-Type: application/json
{"userId": "<userid>", "password": "<sha256(password)>", "TOTP": "<totp or ''>", "vendorCode": "<BROKER_API_KEY>", "apiKey": "<BROKER_API_SECRET>"}
→ {"status":"success","data":{"susertoken":"..."}}  (also accepts data.jKey)
```
(`broker/firstock/api/auth_api.py:64-100`). Failure shape `{"status":"failed","message":..., "error":{"field":..., "message":...}}` (`:101-113`).

Stored auth token semantics: shoonya/zebu/flattrade/firstock store the bare token; tradesmart stores `uid:::token` and splits it with `parse_auth()` on every use (`broker/tradesmart/api/baseurl.py:41-49`).

### A3. Request transport conventions

| Dialect | Headers | Body | `uid` injection | Brokers |
|---|---|---|---|---|
| A (Bearer) | `Content-Type: text/plain`, `Authorization: Bearer <token>` | `jData=<json>` | module adds `uid` (and `actid` for book calls) from `BROKER_API_KEY[0]` / `resolve_uid()` | shoonya `broker/shoonya/api/order_api.py:32-46`, data `:259-268`; zebu `broker/zebu/api/order_api.py:32-46`; tradesmart `broker/tradesmart/api/baseurl.py:74-96` |
| B (jKey) | `Content-Type: application/x-www-form-urlencoded` (flattrade order_api uses `application/json` for every book endpoint except Holdings — `broker/flattrade/api/order_api.py:55-58`) | `jData=<json>&jKey=<token>` | same | flattrade `broker/flattrade/api/data.py:86-92`, `funds.py:63-64`; shoonya chart endpoints only `broker/shoonya/api/data.py:305-311` |
| Firstock | `Content-Type: application/json` (+`Accept`) | JSON object with `"jKey"` and `"userId"` members | `payload.update({"jKey": auth, "userId": api_key})` | `broker/firstock/api/order_api.py:37-44, 239`, `data.py:40-54` |

Ampersand handling (Noren splits the body on `&`): shoonya data path serialises `&` as `\u0006`-style JSON escape `"\\u0026"` (`broker/shoonya/api/data.py:54-66`); order paths across all five replace `&`→`%26` in `tsym` (`broker/shoonya/mapping/transform_data.py:26-27`, flattrade `:27-28`, tradesmart `:23-25`, zebu `:26-27`, firstock `:39-40`).

Error convention: `{"stat":"Not_Ok","emsg":"..."}` for every failure; an empty book is `{"stat":"Not_Ok","emsg":"no data"}` (`broker/shoonya/api/order_api.py:100-106`). Session death is detected by substring on `emsg`: `session expired | invalid session | session key | not logged in | invalid input : uid` (`broker/shoonya/api/data.py:116-137`). Rate-limit rejection text: `"... exceeds Limit N for user"` (`broker/flattrade/api/rate_limit.py:82-88`, `broker/tradesmart/api/rate_limiter.py:12-15`).

### A4. Endpoint table (classic Noren; path prefix is per-broker)

| Function | Path suffix | jData (beyond `uid`) | Source (shoonya) |
|---|---|---|---|
| PlaceOrder | `/PlaceOrder` | see A5 | `broker/shoonya/api/order_api.py:175` |
| ModifyOrder | `/ModifyOrder` | see A5 | `:387` |
| CancelOrder | `/CancelOrder` | `{"uid","norenordno"}` | `:335-345` |
| OrderBook | `/OrderBook` | `{"uid","actid"}` POST | `:60` |
| TradeBook | `/TradeBook` | `{"uid","actid"}` | `:64` |
| PositionBook | `/PositionBook` | `{"uid","actid"}` | `:68` |
| Holdings | `/Holdings` | `{"uid","actid","prd":"C"}` | `:34-35, 72` |
| Limits (funds) | `/Limits` | `{"uid","actid"}` | `broker/shoonya/api/funds.py:26-43` |
| GetQuotes | `/GetQuotes` | `{"uid","exch","token"}` | `broker/shoonya/api/data.py:221-225` |
| TPSeries (intraday) | `/TPSeries` | `{"uid","exch","token","st","et","intrv"}` | `:950, 969-975` |
| EODChartData (daily) | `/EODChartData` | `{"uid","sym":"EXCH:TSYM","from","to"}` | `:947, 963-967` |
| GetBasketMargin | `/GetBasketMargin` | first leg flat + `basketlists:[...]` | `broker/shoonya/api/margin_api.py:66`, `mapping/margin_data.py:131-143` |
| GetOrderMargin (tradesmart only) | `/GetOrderMargin` | single leg + `rorgqty:"0"`, `rorgprc:"0"` | `broker/tradesmart/api/margin_api.py:40`, `mapping/margin_data.py:29-42` |
| GenAcsTok | `/GenAcsTok` | `{"code","checksum"}` no auth | `broker/shoonya/api/auth_api.py:30` |

Prefixes: shoonya `https://api.shoonya.com/NorenWClientAPI` (`order_api.py:46`), zebu `https://go.mynt.in/NorenWClientAPI` (`broker/zebu/api/order_api.py:46`), tradesmart `https://v2api.tradesmartonline.in/NorenWClientAPIv2` (`broker/tradesmart/api/baseurl.py:34`), flattrade `https://piconnect.flattrade.in/PiConnectAPI` (`broker/flattrade/api/order_api.py:60`). Not used by any of the five: `SearchScrip`, `GetSecurityInfo`, `UserDetails`, `Logout`, `SpanCalc`, `GetMultiQuotes` (there is no bulk-quote endpoint; multiquote is a fan-out — `broker/flattrade/api/data.py:200-202`, `broker/tradesmart/api/data.py:104-106`).

### A5. Order payloads and enums

PlaceOrder jData (shoonya `broker/shoonya/mapping/transform_data.py:151-166`):
```json
{"uid":"<uid>","actid":"<uid>","exch":"NFO","tsym":"<brsymbol, & as %26>","qty":"75","prc":"<price>",
 "trgprc":"<trigger_price or 0>","dscqty":"<disclosed or 0>","prd":"M","trantype":"B","prctyp":"LMT",
 "mkt_protection":"0","ret":"DAY","ordersource":"API"}
```
All values are strings. flattrade identical (`broker/flattrade/mapping/transform_data.py:151-166`); zebu identical (`broker/zebu/mapping/transform_data.py:97-112`); tradesmart drops `mkt_protection` and adds `"remarks":"openalgo"` (`broker/tradesmart/mapping/transform_data.py:37-52`). `ret` is always `"DAY"` (IOC/EOS are never sent); `amo` is never sent.

ModifyOrder jData (`broker/shoonya/mapping/transform_data.py:180-196`):
```json
{"uid":"<uid>","exch":"...","norenordno":"<orderid>","prctyp":"LMT","prc":"...","qty":"...","tsym":"...","ret":"DAY","dscqty":"0"}
```
`trgprc` is included **only** when pricetype ∈ {SL, SL-M} ("Sending trgprc=0 for LIMIT orders causes 'Trigger price invalid - 0.00'", `:192-195`). Zebu sends `prc:"0"` for MARKET modifies (`broker/zebu/mapping/transform_data.py:131`). ModifyOrder receives no MPP conversion on classic brokers (firstock does, §B).

CancelOrder jData: `{"uid":"<uid>","norenordno":"<orderid>"}` (`broker/shoonya/api/order_api.py:335`). Note shoonya/flattrade/zebu read the error as `data.get("message", ...)` not `emsg` (`broker/shoonya/api/order_api.py:358-361`), tradesmart reads `emsg` (`broker/tradesmart/api/order_api.py:282-285`).

Responses: `stat == "Ok"` → `norenordno` is the order id (`broker/shoonya/api/order_api.py:185-186`); else `emsg`.

Enums (identical in all five):
| OpenAlgo | Noren | Source |
|---|---|---|
| pricetype MARKET/LIMIT/SL/SL-M | `MKT`/`LMT`/`SL-LMT`/`SL-MKT` (shoonya/flattrade/zebu default `"MARKET"` on miss — a latent bug; tradesmart/firstock default `"MKT"`) | `broker/shoonya/mapping/transform_data.py:204-205`, `broker/tradesmart/mapping/transform_data.py:88-89`, `broker/firstock/mapping/transform_data.py:261-262` |
| product CNC/NRML/MIS | `C`/`M`/`I` (default `I`) | `:212-217` |
| reverse C/M/I | CNC/NRML/MIS; the orderbook mapper only maps `C→CNC` on NSE/BSE and `M→NRML` on NFO/MCX/BFO/CDS | `:224-229`; `broker/shoonya/mapping/order_data.py:87-94` |
| action BUY/SELL | `trantype` `B`/`S` | `:161` |
| validity | always `"DAY"` | `:164` |
| exchange | pass-through NSE/BSE/NFO/BFO/CDS/MCX; `NSE_INDEX→NSE`, `BSE_INDEX→BSE` for data/WS only | `broker/shoonya/api/data.py:363-366`, `broker/shoonya/streaming/shoonya_mapping.py:9-18` |
| H/B (cover/bracket) and F (MTF) product codes | exist in TradeSmart docs; mapped to raw code, never emitted | `broker/tradesmart/streaming/tradesmart_order_adapter.py:89-92` |

Order status normalisation (REST path, `broker/shoonya/mapping/order_data.py:12-42`): uppercase, `_`→space; `COMPLETE→complete`; `{OPEN, PENDING, TRIGGER PENDING, NEW, REPLACED, OPEN PENDING, MODIFY PENDING, CANCEL PENDING, AFTER MARKET ORDER REQ RECEIVED}→open`; `{REJECTED, REJECT}→rejected`; `{CANCELED, CANCELLED}→cancelled`; otherwise lowercase passthrough. **TRIGGER_PENDING folds to `open` on REST but stays `"trigger pending"` on the WS push path** (`broker/shoonya/streaming/shoonya_order_adapter.py:55-77`). cancel_all cancels everything that normalises to `open` (`broker/shoonya/api/order_api.py:412-417`).

### A6. Book response fields consumed

- OrderBook rows: `token`, `exch`, `tsym`, `prd`, `prctyp`, `trantype`, `qty`, `prc`, `trgprc`, `norenordno`, `status`, `norentm`, plus `avgprc`, `rprc`, `fillshares`, `instname`, `rejreason` on flattrade/tradesmart (`broker/shoonya/mapping/order_data.py:70-104, 167-179`; flattrade diff lines adding `avgprc`/`rprc` price preference and `filled_quantity = fillshares`, `pending_quantity = qty - fillshares`, `average_price = avgprc` — `broker/flattrade/mapping/order_data.py:103-121, 218-236`).
- TradeBook rows: `tsym`, `exch`, `prd`, `trantype`, `qty`, `avgprc`, `norenordno`, `norentm` ("HH:MM:SS DD-MM-YYYY"; shoonya keeps only the time part `:245-249`, flattrade/tradesmart keep the whole string).
- PositionBook rows: `tsym`, `exch`, `prd`, `token`, `netqty`, `netavgprc`, `lp`, `rpnl`, `urmtom`, `daybuyavgprc`, `totbuyavgprc`, `prcftr` (`broker/shoonya/mapping/order_data.py:311-346`; flattrade/tradesmart use `rpnl + urmtom` with `(lp-netavgprc)*netqty*prcftr` fallback — `broker/flattrade/api/funds.py:16-31`).
- Holdings rows: each row has `stat`, `exch_tsym:[{exch,tsym,token,pp,ti,ls}]`, `holdqty`, `btstqty`, `brkcolqty`, `unplgdqty`, `benqty`, `npoadqty` (zebu: `npoadt1qty`), `dpqty`, `usedqty`, `upldprc`, `s_prdt_ali` (`broker/shoonya/mapping/order_data.py:375-385, 424-437, 466-496`; zebu diff `npoadt1qty`). Only the `exch=="NSE"` leg is reported. shoonya quantity = `btstqty+holdqty+brkcolqty+unplgdqty+benqty+max(npoadqty,dpqty)-usedqty`; flattrade/tradesmart/zebu quantity = `holdqty + max(npoadqty,dpqty)`, product from `exch_tsym.product` (flattrade diff `:557-565`).

### A7. Funds (`/Limits`)

Fields read: `cash`, `payin`, `marginused`, `brkcollamt`, `collateral`, `rpnl`, `unmtom`.
```
availablecash  = cash + payin - marginused
collateral     = brkcollamt                      (flattrade: collateral or brkcollamt — issue #1936)
utiliseddebits = marginused
m2mrealized    = -rpnl                           (shoonya/zebu, from Limits)
m2munrealized  = unmtom                          (shoonya/zebu, from Limits)
```
(`broker/shoonya/api/funds.py:59-77`, `broker/zebu/api/funds.py:59-76`). flattrade and tradesmart instead fetch `/PositionBook` and sum `rpnl` and `urmtom` across positions for m2m (`broker/flattrade/api/funds.py:78-119`, `broker/tradesmart/api/funds.py:46-76`). Firstock: `/V1/limit` → `data.{cash,payin,marginused,brkcollamt}`, m2m hard-coded `"0.00"` (`broker/firstock/api/funds.py:33-71`). All values formatted `f"{x:.2f}"` strings.

### A8. Margin calculator

- shoonya/flattrade `GetBasketMargin`: leg `{exch,tsym,qty,prc,trgprc,prd,trantype,prctyp}`; first leg flat with `uid`,`actid`, rest in `basketlists` (`broker/shoonya/mapping/margin_data.py:104-143`). Response `marginused` (shoonya `:163`) vs `marginusedtrade` preferred by flattrade (`broker/flattrade/mapping/margin_data.py` diff lines 194-207). MKT/SL-MKT are converted to LMT/SL-LMT by MPP before sending (both brokers' `_apply_mpp`); flattrade raises `MarginPriceUnavailable` when no positive price can be found and reads tick size from SymToken when the quote omits `ti`.
- tradesmart: no basket endpoint; `/GetOrderMargin` per leg, summed; reads `ordermargin` else `marginused` (`broker/tradesmart/api/margin_api.py:16-84`, `mapping/margin_data.py:45-60`).
- zebu: `NotImplementedError` (`broker/zebu/api/margin_api.py:22`).
- firstock: `/V1/basketMargin` camelCase legs + `BasketList_Params`, reads `data.TradedMargin` (`broker/firstock/mapping/margin_data.py:115-162, 203`).

### A9. Quotes and depth (`/GetQuotes`)

Response fields consumed: `stat`, `emsg`, `exch`, `token`, `tsym`, `lp`, `o`, `h`, `l`, `c`, `v`, `oi`, `ti` (tick size), `ltq`, `bp1..bp5`, `bq1..bq5`, `bo1..bo5`, `sp1..sp5`, `sq1..sq5`, `so1..so5` (`broker/shoonya/api/data.py:376-387, 754-782`; flattrade adds `bo{i}`/`so{i}` and `oi` in depth `broker/flattrade/api/data.py:559-589`; tradesmart adds `bq1`/`sq1` as `bid_qty`/`ask_qty` `broker/tradesmart/api/data.py:462-475`). shoonya depth hard-codes `oi: 0` (`:781`), zebu/flattrade/tradesmart pass `oi`.

Quote-identity guard (shoonya only): the REST GetQuotes intermittently returns another instrument's snapshot (~9% measured); `quote_matches_request()` compares echoed `token`/`exch` and retries up to `SHOONYA_QUOTE_ATTEMPTS` (default 3) (`broker/shoonya/api/data.py:83-239`). Worth porting as a generic optional quirk.

Multiquote fan-out: shoonya BATCH_SIZE 20 + 1 s sleep (`:405-406`); flattrade 10, pacing owned by limiter (`broker/flattrade/api/data.py:213`); zebu 10 + 1 s (`broker/zebu/api/data.py:169-170`); tradesmart serves from a pooled WS depth subscription (`{"t":"d","k":"NFO|54321#NFO|54322"}`), falls back to REST, cap 500 subscribed scrips, 4 s snapshot timeout (`broker/tradesmart/api/data.py:101-435`); firstock has a true bulk endpoint `/getMultiQuotes` with 50 per call + 1 s (`broker/firstock/api/data.py:231-232, 316-318`).

### A10. History

Timeframe map (`broker/shoonya/api/data.py:333-347`): `1m→"1", 3m→"3", 5m→"5", 10m→"10", 15m→"15", 30m→"30", 1h→"60", 2h→"120", 4h→"240", D→"D"`. flattrade and tradesmart omit `4h` (`broker/flattrade/api/data.py:123-136`, `broker/tradesmart/api/data.py:442-452`). Firstock uses `"1mi"…"240mi"` and `"1d"` (`broker/firstock/api/data.py:957-967, 951`).

Date → epoch: `strptime(start+" 00:00:00")` / `strptime(end+" 23:59:59")` in **host-local time** (`broker/shoonya/api/data.py:926-931`).

Intraday `TPSeries` jData: `{"uid","exch","token","st":"<epoch>","et":"<epoch>","intrv":"<1|3|…|240>"}` (`:969-975`). `intrv="D"` is **not accepted** (hangs → 504) (`:938-940`).
Daily `EODChartData` jData: `{"uid","sym":"NSE:<TSYM or index display name>","from":"<epoch>","to":"<epoch>"}` (`:963-967`). Shoonya resolves NSE_INDEX symbols to display names: `NIFTY→"Nifty 50"`, `BANKNIFTY→"Nifty Bank"`, `FINNIFTY→"Nifty Financial Services"`, `MIDCPNIFTY→"Nifty Midcap Select"`, `NIFTYNXT50→"Nifty Next 50"`, `INDIAVIX→"India VIX"`; BSE indices have no EOD series (`:69-80`).

Response: a JSON **list**, elements may be JSON **strings** (EOD) needing a second `json.loads` (`:1058-1060`). Candle fields: `ssboe` (epoch, preferred), `time` (`"DD-MM-YYYY HH:MM:SS"` for TPSeries, `"DD-MMM-YYYY"` for EOD), `into`, `inth`, `intl`, `intc`, `intv` (bar volume as delta of running `v`), `intvwap`, `v`, `oi`/`intoi` (`:1051-1114`). Rows with all-zero OHLC are skipped (`:1071-1076`). TPSeries `time` is IST wall-clock parsed naively; EOD date-only parsed as UTC midnight (`:1082-1100`). Errors return a dict `{"stat":"Not_Ok","emsg":...}` instead of a list; `"no data"` is treated as empty success (`:994-1012`).

Chunking (shoonya, `:787-809`): per-request window seconds `1m:5d, 3m:10d, 5m:20d, 10m:40d, 15m:60d, 30m:90d, 1h:180d, 2h:180d, 4h:365d, D:730d` (EOD silently truncates to newest 1201 rows; TPSeries 504s on long ranges). flattrade/tradesmart/zebu issue a single request. Daily series get a synthetic "today" bar from GetQuotes (`o,h,l,lp,v,oi`) stamped at UTC-midnight+5:30 (`:1126-1176`; zebu uses local midnight without +5:30 `broker/zebu/api/data.py:646-648`). Shoonya also repairs candles (`low<=open,close<=high`, negative `intv` → 0) (`:811-871`). Column order: shoonya returns `[timestamp,open,high,low,close,volume,oi]`; flattrade/tradesmart reorder to `["close","high","low","open","timestamp","volume","oi"]` (`broker/flattrade/api/data.py:809`).

### A11. MPP (Market Price Protection) emulation

Noren OMS rejects `MKT` and `SL-MKT` for API orders ("ALGO_CHK: MKT Order type not allowed for API order", `broker/tradesmart/mapping/transform_data.py:14-16`). Every classic broker converts at order-transform time (`broker/shoonya/mapping/transform_data.py:34-148`):
1. For pricetype ∈ {MARKET, SL-M}: fetch `BrokerData(auth).get_quotes()`, read `ltp` and `tick_size` (= quote `ti`).
2. `protected = calculate_protected_price(ltp, action, symbol, instrument_type, tick_size)` from `utils/mpp_slab.py:130-191`: slabs EQ/FUT `(<100:2%, <500:1%, else 0.5%)`, OPT (symbol ends CE/PE) `(<10:5%, <100:3%, <500:2%, else 1%)` (`utils/mpp_slab.py:17-30`); BUY adds, SELL subtracts; rounded to tick (`round(price/tick)*tick`, 2 dp) (`:103-127`).
3. MARKET→`LMT` with `prc=protected`; SL-M→`SL-LMT` keeping `trgprc`.
4. SL-M fallback when no quote: still send `SL-LMT` priced off the trigger (plus MPP buffer only if SymToken has a tick size) (`:107-148`). MARKET without a quote falls through as `MKT`.
`mkt_protection:"0"` is always sent so the server-side MPP is a no-op.

Deltas: zebu converts **MARKET only**, SL-M is sent as `SL-MKT` (`broker/zebu/mapping/transform_data.py:35`); tradesmart `_apply_mpp` always converts and prices SL-M off the trigger (`broker/tradesmart/mapping/transform_data.py:104-166`); firstock also applies MPP on **modify** and falls back to server-side MPP with `mkt_protection:"1"` when action is unknown or the quote fails (`broker/firstock/mapping/transform_data.py:135-254`), and reads tick size from SymToken because `/getQuote` omits it (`:69-75`). Reference: `.claude/skills/broker-integration/references/order-type-emulation.md:26-33`.

### A12. Master contract

Download (shoonya `broker/shoonya/database/master_contract_db.py:124-167`): zip per exchange, `https://api.shoonya.com/{NSE,NFO,CDS,MCX,BSE,BFO}_symbols.txt.zip`, extracted to `tmp/<EXCH>_symbols.txt`. CSV columns used: `Exchange, Token, LotSize, Symbol, TradingSymbol, Expiry, Instrument, OptionType, StrikePrice, TickSize` (+`Precision, Multiplier` on CDS, `GNGD` on MCX) (`:182-193, 292-306, 414-430, 550-565`). Placeholder rows with blank Symbol/TradingSymbol are dropped (`:58-85`).

Symbol rules:
- NSE: `symbol = brsymbol` with `-EQ`/`-BE` stripped; `Instrument=="INDEX"` → `exchange=NSE_INDEX`; `BE`→`EQ`; `tick_size = TickSize/100` (paise→rupees); `expiry=""`, `strike=-1` (`:205-237`).
- NSE index renames: uppercase, strip spaces/hyphens, then `{NIFTY50→NIFTY, NIFTYINDEX→NIFTY, NIFTYBANK→BANKNIFTY, NIFTYFIN→FINNIFTY, NIFTYFINSERVICE→FINNIFTY, NIFTYFINANCIALSERVICES→FINNIFTY, NIFTYNEXT50→NIFTYNXT50, NIFTYMIDSELECT→MIDCPNIFTY, NIFTYMIDCAPSELECT→MIDCPNIFTY}` (`:255-277`). (INDIAVIX arrives already as "INDIA VIX"→"INDIAVIX" via the generic strip.)
- NFO/BFO/CDS/MCX: `expiry` `DD-MMM-YYYY`→`DD-MMM-YY` upper (`:331-338`); `OptionType=="XX"`→`FUT`, else CE/PE (`:344-346`; CDS maps `OPTCUR`, MCX `OPTFUT` instrument to the option type `:470-481, 600-611`); symbol = `{name}{DDMMMYY}FUT` or `{name}{DDMMMYY}{strike}{CE|PE}` with integer strikes rendered without `.0` (`:349-364`); CDS drops `Token<=100` dummy rows (`:448-450`).
- BFO: `name` is re-derived as leading letters of TradingSymbol and type from its suffix FUT/CE/PE (`:857-878`); strike rendered via `f"{x:.2f}".rstrip("0").rstrip(".")` (`:905-910`).
- BSE: all rows `instrumenttype=EQ`; manual rows **SENSEX token "1"** and **BANKEX token "12"** on `BSE_INDEX` (`:719, 750-777`).
- Dedupe on insert by `(token, exchange)` (`:98-107`); zebu/flattrade/firstock dedupe by token only (`broker/zebu/database/master_contract_db.py` diff 64-68, flattrade diff 72-76, `broker/firstock/database/master_contract_db.py:58-59`).

### A13. Market-data WebSocket (Noren JSON protocol)

URL per broker: shoonya `wss://api.shoonya.com/NorenWSAPI/` (`broker/shoonya/streaming/shoonya_websocket.py:144`), zebu `wss://go.mynt.in/NorenWSAPI/` (`broker/zebu/streaming/zebu_websocket.py:28`), tradesmart `wss://v2api.tradesmartonline.in/NorenWSAPI/` (`broker/tradesmart/streaming/tradesmart_websocket.py:47`), flattrade `wss://piconnect.flattrade.in/PiConnectWSAPI/` (`broker/flattrade/streaming/flattrade_websocket.py:22`). (`/NorenWSTP/` from the brief is not used.)

Handshake (`broker/shoonya/streaming/shoonya_websocket.py:439-445`):
```json
{"t":"a","uid":"<uid>","actid":"<uid>","source":"API","accesstoken":"<token>"}
→ {"t":"ak","s":"OK"}     (shoonya compares s=="OK" :505; flattrade/zebu case-insensitive :344/:317; tradesmart "OK" :285)
```
Note field is `accesstoken` (not `susertoken`), `source` is `"API"`; `t:"a"` not `"c"` (`broker/shoonya/streaming/shoonya_order_adapter.py:17-21` documents the doc-vs-live discrepancy). Credentials: `uid/actid = BROKER_API_KEY[0]`, token = stored auth token (`broker/shoonya/streaming/shoonya_adapter.py:352-363`); tradesmart splits the composite (`broker/tradesmart/streaming/tradesmart_adapter.py:153-157`).

Subscribe/unsubscribe (`:183-195, 961`): `{"t":"t","k":"NSE|22#NSE|2885"}` touchline, `{"t":"u","k":...}`, `{"t":"d","k":...}` depth, `{"t":"ud","k":...}`; scrip key `f"{exch}|{token}"` with `NSE_INDEX→NSE`, `BSE_INDEX→BSE` (`broker/shoonya/streaming/shoonya_adapter.py:712-716`, `shoonya_mapping.py:9-18`); `#`-joined batches of ≤100 with 0.1 s pacing (`shoonya_websocket.py:178-180, 899`). Heartbeat `{"t":"h"}` every 30 s, ack `t:"hk"` (or echoed `"h"`), socket recycled after 120 s silence or 180 s of no market data once armed (`:152-169, 696, 705-749`).

Inbound frames: `t ∈ {tk, tf}` touchline (ack = full snapshot, feed = changed fields only), `{dk, df}` depth (`broker/shoonya/streaming/shoonya_adapter.py:42-46`); routing key is `e` + `tk` (`:1372-1381`). Fields consumed: `e, tk, lp, pc, v, o, h, l, c, ap, ltq, ltt, ft, tbq, tsq, bp1..5, bq1..5, bo1..5, sp1..5, sq1..5, so1..5, uc, lc, 52h, 52l, toi` (`:143-265`). Mode mapping: LTP(1) and Quote(2) → touchline; Depth(3) → depth (`:658-664, 1430-1440`); publish topic `{exchange}_{symbol}_{MODE}`. **Snapshot/incremental merge**: cache per scrip; new frame overlays old, except zero/blank `o,h,l,c,ap` do not overwrite non-zero cached values (`:104-125`); flattrade/zebu additionally copy forward every cached key missing from the new frame (`broker/flattrade/streaming/flattrade_adapter.py:123-157`).

### A14. Order-update stream

Dedicated second Noren socket, same URL and handshake; after `ak`, send `{"t":"o","actid":"<uid>"}`; frames arrive as `t:"om"` (`broker/shoonya/streaming/shoonya_order_adapter.py:9-16, 119-126, 175-179`). Fields: `norenordno` (shoonya doc spells `norenoordno`, both read `:198`), `tsym, exch, trantype, qty, prc, trgprc, prctyp, prd` (flattrade/firstock use `pcode` — `broker/flattrade/streaming/flattrade_order_adapter.py:33-35, 217`), `status, reporttype, fillshares, avgprc, rejreason` (`:200-215`). Status map keeps `"trigger pending"` distinct (`:61-77`); `reporttype` fallback for Rejected/Canceled (`:83-87`). Registry: `services/order_update_service.py:65-78` registers shoonya, flattrade, zebu, tradesmart; firstock is **absent** (no order updates at all — `:112-118` returns None).

### A15. Rate limiting

- shoonya: none beyond multiquote batching (20/batch, 1 s) and 3 quote-identity retries.
- flattrade: `broker/flattrade/api/rate_limit.py` — two `SlidingWindowLimiter`s, reserve-earliest-slot across both rolling windows; DATA default 9/s & 110/min (env-overridable, ceilings 40/200), ORDER 9/s & 38/min (ceilings 10/40) (`:236-250`); adaptive downward clamp parsed from `"exceeds Limit N"` + `"current second|minute"` (`:86-88, 326-349`); read endpoints retry 3× with 2/4/8 s backoff, order endpoints never auto-retry (`:263-323`). Documented in `.claude/skills/broker-integration/references/rate-limiting.md:80-103`.
- tradesmart: `broker/tradesmart/api/rate_limiter.py` — general bucket 8/s & 110/min (broker: 10/120), quotes 90/s with no per-minute cap (broker: 100/s) (`:53-62`); gthread back-pressure refusal via `max_queue_wait("data")` (`:106-130`).
- zebu: fixed 10-per-batch + 1 s (`broker/zebu/api/data.py:168-170`).
- firstock: plain-text `"rate limit"` response → sleep 1 s and retry once (`broker/firstock/api/data.py:88-111`); multiquote 50/batch + 1 s.

---

### B. Per-broker delta table

| Item | shoonya | zebu | tradesmart | flattrade | firstock |
|---|---|---|---|---|---|
| plugin exchanges | NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX (`broker/shoonya/plugin.json:8`) | same **minus BSE_INDEX** (`broker/zebu/plugin.json:8`) | same as shoonya (`broker/tradesmart/plugin.json:8`) | same as shoonya (`broker/flattrade/plugin.json:8`) | NSE,BSE,NFO,BFO,NSE_INDEX (`broker/firstock/plugin.json:8`) |
| REST base | `https://api.shoonya.com/NorenWClientAPI` | `https://go.mynt.in/NorenWClientAPI` | `https://v2api.tradesmartonline.in/NorenWClientAPIv2` | `https://piconnect.flattrade.in/PiConnectAPI` | `https://api.firstock.in/V1` |
| Transport | Bearer + `jData=` text/plain; charts jKey form | Bearer + `jData=` text/plain | Bearer + `jData=` text/plain | `jData=&jKey=` form (json header on books) | JSON body with `jKey`,`userId` |
| WS URL | `wss://api.shoonya.com/NorenWSAPI/` | `wss://go.mynt.in/NorenWSAPI/` | `wss://v2api.tradesmartonline.in/NorenWSAPI/` | `wss://piconnect.flattrade.in/PiConnectWSAPI/` | `wss://socket.firstock.in/V2/ws?userId=&jKey=&source=developer-api` |
| Master URLs | `api.shoonya.com/{EXCH}_symbols.txt.zip` ×6 | `go.mynt.in/{EXCH}_symbols.txt.zip` ×6 (`broker/zebu/database/master_contract_db.py` diff 85-92) | `v2api.tradesmartonline.in/{EXCH}_symbols.txt.zip` ×6 (`broker/tradesmart/database/master_contract_db.py:103-110`) | S3 CSVs: `flattrade.s3.ap-south-1.amazonaws.com/scripmaster/{NSE_Equity,Nfo_Equity_Derivatives,Nfo_Index_Derivatives,Currency_Derivatives,Commodity,BSE_Equity,Bfo_Index_Derivatives,Bfo_Equity_Derivatives}.csv` (flattrade diff 93-102) | `api.firstock.in/V1/symbols/{NSE,BSE,NFO,BFO}?ref=firstock.in` CSV + authenticated `POST /V1/indexList` (`broker/firstock/database/master_contract_db.py:76-81, 671-763`) |
| Credentials | `BROKER_API_KEY=userid:::client_id`, `BROKER_API_SECRET` | `userid:::client_id` (e.g. `Z56004:::Z56004_U`), secret | `CLIENT_ID:::API_KEY` (or bare key), secret; stored token `uid:::access_token` | `userid:::api_key`, secret | `BROKER_API_KEY=<USERID>_API` (vendorCode), `BROKER_API_SECRET=apiKey`; user types userid/password/TOTP |
| Login | redirect `api.shoonya.com/OAuthlogin/authorize/oauth?client_id=` → GenAcsTok | redirect `go.mynt.in/OAuthlogin/...` → GenAcsTok | redirect `v2api.../OAuthlogin/...` → GenAcsTok; manual token paste | frontend redirect `auth.flattrade.in/?app_key=` → `/trade/apitoken` | TOTP form → `/V1/login` |
| Checksum | `sha256(client_id+secret+code)` | same | same | `sha256(api_key+code+secret)` | password `sha256` only |
| vc/imei/apkversion | none | none | none | none | none |
| MPP | MARKET & SL-M → LMT/SL-LMT; SL-M trigger fallback | **MARKET only**; SL-M sent as SL-MKT | always convert (quote or fallback) | as shoonya | place+modify; server MPP fallback `mkt_protection:"1"` |
| PlaceOrder extras | `mkt_protection:"0"`, `ordersource:"API"` | same | `remarks:"openalgo"`, no `mkt_protection` | same as shoonya | camelCase fields, `retention`, `remarks=strategy` |
| Funds m2m | from Limits `rpnl`,`unmtom` | same | from PositionBook sum | from PositionBook sum; `collateral` field preferred | `0.00` |
| Margin API | GetBasketMargin | **none** (NotImplementedError) | GetOrderMargin per leg | GetBasketMargin (`marginusedtrade`) | `/V1/basketMargin` |
| Quote guard | identity retry ×3 | no | no | no | n/a |
| Multiquote | 20/batch REST | 10/batch REST | WS depth snapshot + REST fallback | 10/batch REST, limiter-paced | `/getMultiQuotes` 50/batch |
| History 4h | yes | yes | **no** | **no** | yes (`240mi`) |
| History chunking | yes (per-interval windows) | no | no | no | yes (aggressive; 1m per-day) |
| Master tick_size | `/100` | **raw** (zebu diff 201-202) | `/100` | constant 0.05 (0.0025 CDS) | raw |
| Master index names | strip + override map | exact-name map `{"NIFTY INDEX","NIFTY BANK","NIFTY FIN SERVICE","NIFTY MIDCAP SELECT","NIFTY NEXT 50","INDIA VIX"}` | strip + override map (same as shoonya) | strip + override map; BSE `Instrument=="UNDIND"`→BSE_INDEX with `BSESENSEX/S&PBSESENSEX→SENSEX`, `…SENSEX50→SENSEX50`, `…SENSEXNEXT50→BSESENSEXNEXT50` | heuristic `ISIN empty & TickSize==0 & FreezeQty==0` → INDEX; alias tables (`:528-588`) |
| BSE indices | manual SENSEX(1)/BANKEX(12) | **none** | manual SENSEX(1)/BANKEX(12) | from CSV UNDIND rows | from BSE CSV heuristic + indexList |
| Rate limiter | none | none (batch sleep) | dual bucket module | dual window + adaptive clamp | text retry |
| Order updates | dedicated WS `t:"o"` → `om` | dedicated WS (inferred, unverified) | dedicated WS, **no `t:"o"`** (auto-push) | **REST polling by default** (`FLATTRADE_ORDER_WS=TRUE` opts into dedicated socket, which evicts the market feed) | **none** |
| WS extras | batched sub worker, silence watchdog | silence watchdog | silence watchdog, close-frame decode, raw socket shutdown | persistent session, flap backoff (`STABLE_SESSION_SECONDS=60`, `FLAP_ALERT_THRESHOLD=3`), close-code 1000 = eviction | token refreshed before reconnect; paise→rupee `/100`; invalid marker `9223372036854775808` |

### B1. Shoonya-specific notes
- `_encode_jdata` escapes `&` as `&` because the server splits on `&` and never URL-decodes (`broker/shoonya/api/data.py:54-66`).
- `EOD_INDEX_SYMBOLS` display-name table (`:73-80`).
- Holdings quantity uses the full 8-term formula (`broker/shoonya/mapping/order_data.py:480-488`).
- WS adapter keeps `_token_to_scrips` fallback for frames without `e` and drops ambiguous ones (`broker/shoonya/streaming/shoonya_adapter.py:1405-1428`).

### B2. Zebu-specific notes
- `brexchange` hard-coded `"NSE"` for the whole NSE file including indices (zebu diff 186-187); token cast to str.
- Expiry via `pd.to_datetime(...).dt.strftime('%d-%b-%y')` (diff 296-300); BFO reads `Strike` (no `OptionType`) and symbol uses hyphenated expiry (`{name}{row['expiry']}FUT` — i.e. `NIFTY28-AUG-25FUT`, a likely bug, diff 779-789).
- WS auth ack has no `auth_failed` flag; heartbeat thread uses bare `time.sleep` (`broker/zebu/streaming/zebu_websocket.py:428-443`).
- Order-update adapter is written from inference, not a doc (`broker/zebu/streaming/zebu_order_adapter.py:7-20`).

### B3. TradeSmart-specific notes
- `baseurl.py` is the only place hosts/headers live; everything calls `post(endpoint, jdata, auth_token)` (`broker/tradesmart/api/baseurl.py:82-96`).
- `resolve_uid()` priority: composite token uid → `BROKER_API_KEY[0]` → bare env (`:61-71`).
- Order-update socket also pushes `am` (alerts/GTT), `rm` (admin), `ms` (market status) frames which are ignored (`broker/tradesmart/streaming/tradesmart_order_adapter.py:28-29, 174-178`).
- `_QuoteStream` registry keyed by bearer; stale sockets closed on re-login (`broker/tradesmart/api/data.py:393-435`).
- `"no data"` on history → empty DataFrame, not an error (`:729-741`); CDS has no history on this backend.
- SymToken declares an extra `contract_value` column (`broker/tradesmart/database/master_contract_db.py:59-60`); BFO file names the strike column `Strike` and has a trailing comma (`:382-395`).

### B4. Flattrade-specific notes
- `authenticate_broker()` **unconditionally** takes `BROKER_API_KEY.split(":::")[1]`, so the `:::` is mandatory (`broker/flattrade/api/auth_api.py:27`).
- Books: `OrderBook/TradeBook/PositionBook` sent with `Content-Type: application/json` yet form-encoded body; Holdings with form-urlencoded (`broker/flattrade/api/order_api.py:55-58`).
- Orderbook `prc` fallback: `avgprc` when `instname` present, else `rprc` for MARKET/SL-M with `prc==0` (`broker/flattrade/mapping/order_data.py:103-121`).
- `close_all_positions` returns HTTP 429 with a count when the order limiter refused some square-offs under gthread (`broker/flattrade/api/order_api.py:373-385`).
- Master: NFO/BFO are two CSVs each (equity + index) combined locally (flattrade diff 852-867); CSV header is `Token, Lotsize, Symbol, Tradingsymbol, Instrument, Expiry, Strike, Optiontype` (note casing differs from Noren zips) (diff 167-177); expiry first formatted `DDMMMYY` then re-hyphenated to `DD-MMM-YY` for storage (diff 354-361).
- WS: PiConnect allows one session per uid/token; a second socket (e.g. the order adapter) evicts the market feed with close code 1000 — hence polling order updates (`broker/flattrade/streaming/flattrade_order_adapter.py:5-18, 243-291`, `flattrade_websocket.py:439-445`). Order feed uses `pcode`, unsubscribe is `t:"uo"` acked `"uok"` (`flattrade_order_adapter.py:33-38`). Depth normaliser labels `toi` as `total_traded_value` (sic) (`flattrade_adapter.py:206` of the excerpt; shoonya labels it `open_interest` `shoonya_adapter.py:263`).

### B5. Firstock — bespoke transport, documented in detail

REST (`broker/firstock/api/order_api.py`, `data.py`, `funds.py`, `margin_api.py`):
| OpenAlgo op | Endpoint | Request JSON | Response read |
|---|---|---|---|
| login | `POST /V1/login` | `{userId, password:sha256, TOTP, vendorCode, apiKey}` | `data.susertoken` (`auth_api.py:64-98`) |
| orderbook | `POST /V1/orderBook` | `{jKey, userId}` | `data[]` rows: `orderNumber, tradingSymbol, token, exchange, transactionType, product, priceType, quantity, price, triggerPrice, status, orderTime` (`mapping/order_data.py:38-63`) |
| tradebook | `POST /V1/tradeBook` | same | `fillQuantity, fillPrice, fillTime, orderNumber, transactionType, product` (`:230-250`) |
| positions | `POST /V1/positionBook` | same | `netQuantity, netAveragePrice, dayBuyQuantity, daySellQuantity, dayBuyAmount, dayBuyAveragePrice, daySellAmount, daySellAveragePrice, unrealizedMTOM, RealizedPNL, tradingSymbol, token, exchange, product` (`:537-566`) |
| holdings | `POST /V1/holdings` (+ `POST /V1/getLtp {jKey,userId,exchange,token}` per NSE leg) | same | `data[].exchangeTradingSymbol[] {exchange, token, tradingSymbol}`; the V1 response carries no quantity/price, so holdings are emitted with zero qty/pnl (`order_api.py:108-134`, `mapping/order_data.py:340-392`) |
| place | `POST /V1/placeOrder` | `{jKey, userId, exchange, tradingSymbol, quantity, price, triggerPrice, product, transactionType, priceType, retention:"DAY", mkt_protection:"0", remarks}` | `status=="success"` → `data.orderNumber` (`order_api.py:229-266`, `transform_data.py:114-127`) |
| modify | `POST /V1/modifyOrder` | `{jKey, userId, exchange, orderNumber, priceType, price, quantity, tradingSymbol, triggerPrice, retention, mkt_protection, product?}` | `status`; errors `error.message`, `code`, `name` (`order_api.py:542-606`, `transform_data.py:237-254`) |
| cancel | `POST /V1/cancelOrder` | `{jKey, userId, orderNumber}` | `status`; error `{error:{message,field}, code, name}`, HTTP status from `code` (`:495-539`) |
| funds | `POST /V1/limit` | `{jKey, userId}` | `data.cash, payin, marginused, brkcollamt` (`funds.py:33-61`) |
| margin | `POST /V1/basketMargin` | first leg flat + `BasketList_Params[]`, `userId`, `jKey` | `data.TradedMargin` (`margin_api.py:63`, `mapping/margin_data.py:151-203`) |
| quote/depth | `POST /V1/getQuote` | `{userId, exchange, tradingSymbol, jKey}` (symbol, not token) | `data.bestBuyPrice1..5, bestBuyQuantity1..5, bestSellPrice1..5, bestSellQuantity1..5, dayOpenPrice, dayHighPrice, dayLowPrice, dayClosePrice, lastTradedPrice, lastTradedQuantity, totalBuyQuantity, totalSellQuantity, volume, openInterest`; **no tick size** (`data.py:161-171, 189-215, 366-415`) |
| multiquote | `POST /V1/getMultiQuotes` | `{userId, jKey, data:[{exchange, tradingSymbol}]}` ≤50 | `data[]` same fields + echoed `exchange`,`tradingSymbol` (`:316-348`) |
| history | `POST /V1/timePriceSeries` | `{userId, jKey, exchange, tradingSymbol, startTime:"HH:MM:SS DD-MM-YYYY", endTime, interval:"1mi|3mi|5mi|10mi|15mi|30mi|60mi|120mi|240mi|1d"}` | `data[].epochTime` (or `time` ISO), `open, high, low, close, volume`; daily bars re-stamped to 09:15 IST; 600 s read timeout; chunk days `3m:2,5m:3,10m:5,15m:7,30m:10,1h/2h/4h:15,D:30`, 1m fetched per calendar day (`:949-1075, 794-804, 589-734, 58-72`) |
| cancel-all | — | filters raw status ∈ {`OPEN`,`TRIGGER_PENDING`} (`order_api.py:619-624`) | |

Status mapping on REST: `COMPLETE→complete, OPEN→open, REJECTED→rejected, CANCELED/CANCELLED→cancelled, TRIGGER PENDING/TRIGGER_PENDING→"trigger_pending", PENDING→open` — note `trigger_pending` (underscore) is **not** folded to `open` here, unlike the Noren siblings (`mapping/order_data.py:161-170`). Price-type back-map tolerates `LIMIT`, `SL-LIMIT`, `SL-MARKET` long forms (`:106-119`). `reverse_map_product_type` defaults to `MIS` (`transform_data.py:278`).

Exchange: `NSE_INDEX→NSE`, `BSE_INDEX→BSE` (`data.py:16-26`); indices come from `/V1/indexList` (`{userId,jKey}` → `data[]{exchange, tradingSymbol, idxname, token}`) and from the BSE CSV heuristic (`master_contract_db.py:155-157, 359-363, 671-763`); `brexchange` for NSE_INDEX is `NSE` (`:214`). Master CSV columns: NSE/BSE `Exchange, Token, LotSize, TradingSymbol, CompanyName, ISIN, TickSize, FreezeQty`; NFO/BFO `Exchange, Token, LotSize, Symbol, TradingSymbol, CompanyName, Expiry, Instrument, OptionType, StrikePrice, TickSize, FreezeQty` (`:88-91`); tick_size stored raw (`:222`).

WebSocket (`broker/firstock/streaming/firstock_websocket.py`, `firstock_adapter.py`):
- URL `wss://socket.firstock.in/V2/ws?userId=<uid>&jKey=<susertoken>&source=developer-api` (`:19, 131-132`); no connect frame; server replies `{"status":"success",...}` or `{"status":"failed"|"message":"unauthenticated"}` (`:480-519`). Token re-read from DB before every reconnect (daily ~03:00 IST rollover) (`:102-121`, adapter `:154-174`).
- Subscribe `{"action":"subscribe","tokens":"NSE:26000|NFO:65872"}`, unsubscribe `{"action":"unsubscribe",...}`; scrip key `EXCH:TOKEN` (colon, pipe-separated) (`:369-372, 420`, adapter `:232`). No touchline/depth distinction — one feed carries everything; mode only affects normalisation (adapter `:218-219`).
- Tick shapes: V1 flat `{c_symbol, c_exch_seg, ...}`; V2 `{"EX:TOKEN": {...}}` possibly batched, unwrapped with `c_symbol/c_exch_seg` injected (`:522-553`). Fields (paise, divide by 100 for prices): `i_last_traded_price, i_open_price, i_high_price, i_low_price, i_closing_price, i_average_trade_price, i_upper_circuit_limit, i_lower_circuit_limit, i_volume_traded_today, i_last_trade_quantity, i_total_buy_quantity, i_total_sell_quantity, i_open_interest, i_total_open_interest, i_last_trade_time, i_feed_time, c_exch_feed_time, c_net_change_indicator, i_buy_depth_size, i_sell_depth_size, best_buy[]/best_sell[] {price, quantity, orders}` (adapter `:545-575, 614, 757-845`). Sentinel `9223372036854775808` = "no value"; zero prices do not overwrite the snapshot (`:587-611, 666-671`). Depth padded to 5 levels.
- Ping every 30 s (`ping_interval`), stale after 40 s without pong (`:281-284, 658`); max 5 reconnects, 5 s apart (`adapter :140-141`).
- Order/position frames (`norenordno` / `netqty`+`pcode`) are recognised but only logged (`adapter :519-523, 847-855`).

---

### C. Rust design

### C1. Config types

```rust
pub enum NorenTransport {
    /// `Content-Type: text/plain`, `Authorization: Bearer <tok>`, body `jData=<json>`
    BearerHeader,
    /// `Content-Type: application/x-www-form-urlencoded`, body `jData=<json>&jKey=<tok>`
    JKeyInBody,
}

pub enum NorenLogin {
    /// Not used by any in-tree broker today; kept for Noren white-labels that still expose it.
    QuickAuth { vc: &'static str, imei: &'static str, apkversion: &'static str, appkey_rule: AppKeyRule },
    /// shoonya / zebu / tradesmart: redirect to `authorize_url?client_id=`, callback `?code=`,
    /// `POST {token_url}` jData `{code, checksum=sha256(client_id+secret+code)}` → `access_token`
    GenAcsTok { authorize_url: &'static str, token_url: &'static str, accept_manual_token: bool },
    /// flattrade: redirect `https://auth.flattrade.in/?app_key=`, callback `?code=&client=`,
    /// `POST https://authapi.flattrade.in/trade/apitoken` JSON `{api_key, request_code, api_secret=sha256(api_key+code+secret)}` → `token`
    RequestCodeExchange { login_url: &'static str, token_url: &'static str },
}

pub enum MasterSource {
    /// `https://<host>/{NSE,NFO,CDS,MCX,BSE,BFO}_symbols.txt.zip`, Noren column names, TickSize in paise
    NorenZip { host: &'static str, tick_divisor: f64 /* 100.0 shoonya/tradesmart, 1.0 zebu */ },
    /// flattrade S3 CSVs, `Tradingsymbol/Lotsize/Strike/Optiontype` casing, NFO/BFO split eq+idx, fixed tick sizes
    FlattradeCsv { urls: &'static [(&'static str, &'static str)] },
}

pub struct NorenQuirks {
    pub mpp_scope: MppScope,               // MarketAndStopMarket (shoonya/flattrade/tradesmart) | MarketOnly (zebu)
    pub mpp_on_modify: bool,               // false for all classic brokers
    pub send_mkt_protection_zero: bool,    // true shoonya/flattrade/zebu, false tradesmart
    pub place_remarks: Option<&'static str>, // Some("openalgo") tradesmart
    pub chart_transport_override: Option<NorenTransport>, // Some(JKeyInBody) for shoonya TPSeries/EODChartData
    pub escape_ampersand_as_unicode: bool, // shoonya data path `&`
    pub quote_identity_retries: u8,        // 3 shoonya, 0 others
    pub funds_m2m: FundsM2m,               // FromLimits{rpnl,unmtom} | FromPositionBookSum
    pub collateral_fields: &'static [&'static str], // ["brkcollamt"] or ["collateral","brkcollamt"]
    pub margin: MarginApi,                 // Basket{prefer_marginusedtrade: bool} | PerLegOrderMargin | Unsupported
    pub history_intervals: &'static [(&'static str, &'static str)], // with/without 4h
    pub history_chunk_seconds: Option<fn(&str) -> u64>, // Some for shoonya
    pub eod_index_display_names: &'static [((&'static str,&'static str), &'static str)], // shoonya
    pub daily_today_bar_offset_secs: i64,  // 19800 (+5:30) shoonya/flattrade/tradesmart, 0 zebu
    pub stored_token_is_composite: bool,   // tradesmart `uid:::token`
    pub holdings_qty_formula: HoldingsQty, // Full8Term (shoonya) | HoldPlusMaxNpoadDp{npoad_key:"npoadqty"|"npoadt1qty"}
    pub nse_index_renames: IndexRename,    // StripAndOverride(map) | ExactName(map)
    pub manual_bse_indices: bool,          // SENSEX(1)/BANKEX(12) rows
    pub order_update: OrderUpdateMode,     // DedicatedWsSubscribe{send_t_o: true} | DedicatedWsAutoPush (tradesmart) | RestPolling (flattrade) | None (firstock)
    pub order_feed_product_keys: &'static [&'static str], // ["prd"] or ["pcode","prd"]
    pub ws_single_session: bool,           // flattrade: second socket evicts the first
    pub rate_limit: RateLimitPolicy,       // None | DualWindow{data:(u32,u32), order:(u32,u32), adaptive:bool} | TwoBucket{general:(u32,u32), quote:(u32,Option<u32>)}
    pub multiquote: MultiquotePolicy,      // RestFanOut{batch, delay_ms} | WsDepthSnapshot{cap:500, timeout_ms:4000}
}

pub struct NorenConfig {
    pub id: &'static str,                  // "shoonya" | "zebu" | "tradesmart" | "flattrade"
    pub name: &'static str,
    pub base_url: &'static str,            // incl. path prefix, e.g. "https://api.shoonya.com/NorenWClientAPI"
    pub ws_url: &'static str,              // ".../NorenWSAPI/" or ".../PiConnectWSAPI/"
    pub transport: NorenTransport,
    pub login: NorenLogin,
    pub source: &'static str,              // "API" (both jData.ordersource and WS connect.source)
    pub supported_exchanges: &'static [&'static str],
    pub master: MasterSource,
    pub quirks: NorenQuirks,
}

pub struct NorenBroker { pub cfg: &'static NorenConfig }
```

Suggested statics (values from §B): `SHOONYA`, `ZEBU`, `TRADESMART`, `FLATTRADE`. Firstock is **not** a `NorenConfig` instance.

### C2. Generic `impl Broker for NorenBroker` — what is shared vs hooked

Fully generic from config (no hook):
- `login_url()`, `exchange_code(code)` → GenAcsTok / apitoken; token storage (composite when `stored_token_is_composite`).
- `request(endpoint, jdata)` → chooses header/body per `transport` (and `chart_transport_override` for `/TPSeries`, `/EODChartData`); injects `uid`/`actid`; `&` handling; parses `stat/emsg`; session-error and rate-limit classification.
- place/modify/cancel/orderbook/tradebook/positions/holdings/funds — payload builders in A5–A7 parameterised by `quirks.{send_mkt_protection_zero, place_remarks, funds_m2m, collateral_fields, holdings_qty_formula}`.
- quotes/depth/history — A9/A10 with `history_intervals`, `history_chunk_seconds`, `eod_index_display_names`, `daily_today_bar_offset_secs`, `quote_identity_retries`.
- MPP — one implementation of `utils/mpp_slab.py` + the A11 state machine, gated by `mpp_scope`.
- master contract — two loaders selected by `MasterSource`; symbol-building rules shared (A12), with `tick_divisor`, `nse_index_renames`, `manual_bse_indices`, dedupe key `(token, exchange)`.
- WebSocket market data — one Noren JSON client (A13) with URL from config; identical frames for all four.
- Order updates — one `om` normaliser (A14) with `order_feed_product_keys`, and three connection strategies from `order_update`.
- Rate limiting — one `SlidingWindowLimiter` type instantiated per `RateLimitPolicy`.

Needs a per-broker hook (trait object or fn pointer on `NorenQuirks`):
| Hook | Why | Brokers |
|---|---|---|
| `fn margin(&self, legs) -> MarginResult` | basket vs per-leg vs unsupported, different response keys | all four differ |
| `fn resolve_uid(stored_token, env) -> String` | tradesmart composite token precedence | tradesmart |
| `fn parse_token_response(json) -> (token, Option<uid>)` | tradesmart multi-key fallback | tradesmart |
| `fn multiquote(&self, symbols)` | WS-snapshot strategy owns a socket registry | tradesmart |
| `fn history_post_process(df)` | shoonya `_repair_candles`, flattrade column order | shoonya, flattrade |
| `fn master_bfo_columns()` | `StrikePrice`+`OptionType` vs `Strike` (+trailing comma) | tradesmart, zebu |
| `fn ws_on_close_policy(code, session_age)` | flattrade flap backoff on code 1000 | flattrade |
| `fn cancel_error_message(json)` | `message` vs `emsg` | shoonya/flattrade/zebu vs tradesmart |

### C3. Firstock

Implement `struct FirstockBroker` separately with its own `FirstockTransport` (JSON body with `jKey`/`userId`, camelCase fields, `{"status","data","error"}` envelope) and `FirstockWs` (query-string auth, `action/tokens` subscribe, `EX:TOKEN` keys, paise scaling, sentinel filtering). It can **reuse** from the Noren crate: the product/pricetype/action enum maps, `utils/mpp_slab` MPP math, the status-normaliser vocabulary (with its own `trigger_pending` spelling), and the OpenAlgo symbol-construction helpers (`-EQ` strip, `{name}{DDMMMYY}{strike}{CE|PE}`). It must not share the request/WS layers.

### C4. Open items / caveats found while reading
- Zebu BFO symbols embed the hyphenated expiry (`NIFTY28-AUG-25FUT`) — probably a latent bug to decide on when porting (zebu master diff 779-789).
- shoonya/flattrade/zebu `map_order_type` default `"MARKET"` is not a valid Noren code (tradesmart/firstock default `"MKT"`).
- Cancel-order error text reads `message` not `emsg` on shoonya/flattrade/zebu.
- Zebu's order-update frame layout is inferred, not verified (`broker/zebu/streaming/zebu_order_adapter.py:7-20`).
- Flattrade's `authenticate_broker()` crashes without `:::` in `BROKER_API_KEY`.

---

## D. Symphony XTS broker family — port spec for `XtsBroker<Config>`

All paths are relative to `/Users/openalgo/openalgo-desktop/openalgo/` unless absolute. `F/` abbreviates `broker/fivepaisaxts/` (canonical template). Every fact cites `file:line` as read on 2026-10-03; where a sibling broker differs the delta is cited against the sibling's own file.

### 0. Family membership (verified by code, not by the SKILL table)

| Dir | XTS markers found | Verdict |
|---|---|---|
| `broker/fivepaisaxts` | `/interactive/user/session` (`F/api/auth_api.py:27`), `/apimarketdata` (`F/baseurl.py:7`), `BROKER_API_KEY_MARKET` (`F/api/auth_api.py:64`) | **XTS — canonical** |
| `broker/jainamxts` | `auth_api.py:27` session URL, `baseurl.py:7` `/apibinarymarketdata`, `BROKER_API_KEY_MARKET` | **XTS** |
| `broker/compositedge` | same (`baseurl.py:4-8`) | **XTS** (OAuth front door) |
| `broker/rmoney` | `baseurl.py:7-15`, `auth_api.py` feed login, streaming | **XTS** (OAuth front door, hostlookup stub) |
| `broker/ibulls` | `baseurl.py:4-8` (docstring still says "CompositEdge" — copy artefact), `auth_api.py` byte-identical to F | **XTS** — the SKILL.md table (`.claude/skills/broker-integration/SKILL.md:45`) that lists ibulls under Noren is **wrong** |
| `broker/wisdom` | `baseurl.py:4-8`, `auth_api.py` identical to F except log level | **XTS** — SKILL.md:45 listing under Noren is **wrong** |
| `broker/iifl` | `baseurl.py:4` `https://ttblaze.iifl.com`, `/apimarketdata`, `auth_api.py` byte-identical to F | **XTS** |
| `broker/iiflcapital` | zero hits for `interactive/user/session`, `apimarketdata`, `BROKER_API_KEY_MARKET` | **NOT XTS** — bespoke REST (`https://api.iiflcapital.com/v1`) + MQTT. SKILL.md:46 listing it under XTS is **wrong**. Standalone spec in §D. |

`websocket_proxy/__init__.py:128-147` registers streaming adapters for all 8 (`ibulls`, `compositedge`, `fivepaisaxts`, `iifl`, `iiflcapital`, `wisdom`, `jainamxts`, `rmoney`). `.sample.env:22` lists all 8 in `VALID_BROKERS`.

---

### A. SHARED TEMPLATE (fivepaisaxts canonical)

### A.1 Credentials and configuration

| Env var | Used by | Cite |
|---|---|---|
| `BROKER_API_KEY` / `BROKER_API_SECRET` | interactive (trading) login `appKey`/`secretKey` | `F/api/auth_api.py:19-20,23` |
| `BROKER_API_KEY_MARKET` / `BROKER_API_SECRET_MARKET` | market-data login `appKey`/`secretKey`; also re-read by the streaming adapter | `F/api/auth_api.py:64-65`; `F/streaming/fivepaisaxts_adapter.py:105-106,118-119` |
| `.sample.env:13-16` documents the market pair as "Optional and Required only for XTS API Supported Brokers". |
| `utils/config.py:11-28` only exposes `get_broker_api_key()`/`get_broker_api_secret()`; there is **no** getter for the `_MARKET` pair — every XTS module calls `os.getenv` directly. No composite (`:::`) key form is used by any XTS broker (the `:::` split in `blueprints/brlogin.py:329-332` and `1091-1092` is for iiflcapital/dhan only). |

`BROKER_API_KEY` is also read (unused) in `F/api/order_api.py:28` and `F/api/funds.py:16-17`.

### A.2 Authentication

**Interactive login** — `F/api/auth_api.py:14-58`
```
POST {INTERACTIVE_URL}/user/session            # F/api/auth_api.py:27
Content-Type: application/json                  # :25
{"appKey": BROKER_API_KEY, "secretKey": BROKER_API_SECRET, "source": "WebAPI"}   # :23  (note casing: "WebAPI", not "WEBAPI")
→ 200 {"type":"success","result":{"token": "<jwt>", "userID": ..., "isInvestorClient": ...}}
   token = result["result"]["token"]            # :32-33
   non-success → error from .get("message")     # :53-54
```
`isInvestorClient` and `clientID` are **never read or sent** by any of the 7 XTS plugins (grep over all 7 dirs returned nothing). Orders are always placed without `clientID`.

**Market-data login** — `F/api/auth_api.py:61-100`
```
POST {MARKET_DATA_URL}/auth/login              # :77
{"secretKey": BROKER_API_SECRET_MARKET, "appKey": BROKER_API_KEY_MARKET, "source": "WebAPI"}   # :68-72
→ result.token (feed token), result.userID      # :86-87
   error text from .get("description")           # :93-95
```
`authenticate_broker(request_token)` returns a **4-tuple** `(token, feed_token, user_id, error)` (`:41`); the `request_token` argument is ignored by fivepaisaxts (brlogin passes the literal string `"fivepaisaxts"`, `blueprints/brlogin.py:171-176`). If the feed login fails the interactive token is still returned with `feed_token=None` and an error string (`:38-39`).

**Storage of the three values** — they are **not** concatenated into one string. `blueprints/brlogin.py:1071-1073` calls `handle_auth_success(auth_token, session["user"], broker, feed_token=feed_token)` for fivepaisaxts/jainamxts/ibulls/iifl/wisdom (so `user_id` is **dropped** for these five), and `:1046-1067` passes `user_id=user_id` too for `compositedge`/`rmoney` (and iiflcapital). `utils/auth_utils.py:453` → `database/auth_db.py:602 upsert_auth(name, auth_token, broker, feed_token=None, user_id=None)` stores three separate encrypted columns (`auth_db.py:232-236`). Read-back: `get_auth_token(name)` `:736`, `get_feed_token(name)` `:840`, `get_user_id(name)` `:901`. The data layer is constructed as `BrokerData(auth_token, feed_token, user_id)` (`services/depth_service.py:100`) or `BrokerData(auth_token, feed_token)` (`services/history_service.py:146`).

**Rust takeaway:** `XtsSession { interactive_token, market_token: Option<String>, market_user_id: Option<String> }`. If a single opaque string is required, serialise as JSON; nothing in the Python depends on a `:::` layout.

### A.3 Interactive REST (trading) — `F/api/order_api.py`

Common request: `headers = {"authorization": <interactive token>, "Content-Type": "application/json"}` (`:33-36`; lowercase header name, raw token, no `Bearer`), `url = f"{INTERACTIVE_URL}{endpoint}"` (`:38`). XTS envelope: `{"type":"success"|"error","code":..,"description":..,"result":...}` (`_position_book_ok` `:101-103`).

| Op | Method/Path | Body / notes | Cite |
|---|---|---|---|
| Order book | `GET /orders` | `result` = list of orders | `:58-59` |
| Trade book | `GET /orders/trades` | | `:62-63` |
| Positions | `GET /portfolio/positions?dayOrNet=NetWise` | `result.positionList[]` | `:66-67`, `:292` |
| Holdings | `GET /portfolio/holdings` | `result.RMSHoldings.Holdings{isin:{...}}` | `:70-71`; `F/mapping/order_data.py:384-386` |
| Place | `POST /orders` | payload below; success → `result.AppOrderID` | `:171`, `:185-189` |
| Modify | `PUT /orders` | body = JSON string (`content=`) of modify payload | `:392-395` |
| Cancel | `DELETE /orders?appOrderID={id}` | no body (a `payload` is built at `:353` but never sent) | `:356` |
| Cancel-all | client-side loop over order book; cancels `OrderStatus in ["New","Trigger Pending"]` by `AppOrderID` | **does not call** `/interactive/orders/cancelall` | `:411-443`, `:426`, `:434` |
| Square-off | client-side loop over `positionList`, places MARKET orders using the position's own `ExchangeSegment`/`ExchangeInstrumentId`/`ProductType` | **does not call** `/portfolio/squareoff` | `:285-336`, `:306-325` |
| Funds | `GET /user/balance` | see A.5 | `F/api/funds.py:25` |
| Margin calc | not implemented (raises `NotImplementedError`) | | `F/api/margin_api.py:19-20` |

**Place payload** (`F/mapping/transform_data.py:18-30`):
```json
{"exchangeSegment": "NSECM|BSECM|NSEFO|BSEFO|NSECD|MCXFO",   // map_exchange :53-62 (unknown → "EXCHANGE")
 "exchangeInstrumentID": <token from symtoken>,               // str in F; rmoney casts int
 "productType": "MIS|NRML|CNC",                               // :82-87, default MIS
 "orderType": "MARKET|LIMIT|STOPLIMIT|STOPMARKET",            // :69-75 (SL→STOPLIMIT, SL-M→STOPMARKET), default MARKET
 "orderSide": "BUY|SELL",
 "timeInForce": "DAY",                                        // always DAY; IOC never emitted
 "disclosedQuantity": "0", "orderQuantity": "<qty>",
 "limitPrice": "<price|0>", "stopPrice": "<trigger|0>",
 "orderUniqueIdentifier": "openalgo"}
```
`place_order_api` accepts a pre-built payload if it already has `exchangeSegment/exchangeInstrumentID/productType/orderType` keys (`F/api/order_api.py:151-156`) — used by square-off.

**Modify payload** (`transform_data.py:36-46`): `appOrderID, modifiedProductType, modifiedOrderType, modifiedOrderQuantity, modifiedDisclosedQuantity, modifiedLimitPrice, modifiedStopPrice, modifiedTimeInForce:"DAY", orderUniqueIdentifier:"openalgo"`.

**Response-check quirks to preserve or fix in Rust:**
- `cancel_order` treats a truthy `data.get("status")` as success (`:363`) — XTS answers with `type`, not `status`; the Python path therefore mostly reports error on a successful cancel unless the server also echoes `status`.
- `modify_order` checks `data.get("status")=="true" or data.get("message")=="SUCCESS"` and reads `data["data"]["orderid"]` (`:402-403`) — not XTS shape; same caveat.
- `get_open_position` in F looks for `positions_data["status"]`/`["data"]`/`tradingsymbol` (`:132-137`) which never matches XTS (`type`/`result.positionList`/`TradingSymbol`); jainamxts/rmoney/ibulls carry the corrected lookup (see B). Rust should use the corrected shape: match `TradingSymbol`, `ExchangeSegment` (mapped from OA exchange), `ProductType`, read `Quantity`.

**Reverse mappings** (`F/mapping/order_data.py`):
- Segment→OA exchange: `NSECM→NSE, BSECM→BSE, NSEFO→NFO, BSEFO→BFO, MCXFO→MCX, NSECD→CDS` (`:19-26`, repeated `:106-113,178-185,217-224,277-284`).
- OrderType strings as returned by XTS: `Limit→LIMIT, Market→MARKET, StopLimit→SL, StopMarket→SL-M` (`:116-121`; note Title-case in responses vs upper-case in requests).
- Status: `Filled→complete, Rejected→rejected, Cancelled→cancelled, New→open`; anything else passes through unchanged (`:123-128,147`), so `PartiallyFilled`, `Trigger Pending`, `Open` are **not** normalised. Statistics count `Filled/New/Rejected` only (`:80-85`).
- Product reverse map is identity `CNC/NRML/MIS` (`transform_data.py:95-101`).
- Order fields consumed: `TradingSymbol` (overwritten from DB via `get_symbol(ExchangeInstrumentID, exchange)` `:40-49`), `ExchangeSegment, OrderSide, OrderQuantity, OrderPrice, OrderStopPrice, OrderType, ProductType, AppOrderID (float→int→str), OrderStatus, LastUpdateDateTime` (`:149-161`).
- Trade fields: `OrderQuantity, OrderAverageTradedPrice, AppOrderID, OrderGeneratedDateTime` (`:231-245`).
- Position fields: `ExchangeInstrumentId` (lower-case d), `ExchangeSegment, ProductType, Quantity, BuyAveragePrice/SellAveragePrice` (avg chosen by sign of Quantity), `ltp`/`pnl` default 0 (`:295-325`).
- Holdings: iterate `RMSHoldings.Holdings` dict keyed by ISIN; `ExchangeNSEInstrumentId → get_symbol(.., "NSE")`, `HoldingQuantity`, `BuyAvgPrice`; product forced `CNC`; P&L placeholders 0 (`:395-438`).

### A.4 Market-data REST — `F/api/data.py`

Headers: `{"authorization": feed_token if present else auth_token, "Content-Type": "application/json"}` (`:33-36`), base `MARKET_DATA_URL` (`:38`). XTS error on expired feed token is HTTP 200 + `{"type":"error","description":"Invalid Token"}`; F refreshes once via `get_feed_token()` and retries (`:103-117,193-199`) but only in `_fetch_market_data`; iifl centralises this (B).

**Numeric exchangeSegment** (`:129-138`, repeated `:335-344,843`): `NSE 1, NSE_INDEX 1, NFO 2, CDS 3, BSE 11, BSE_INDEX 11, BFO 12, MCX 51`. **String exchangeSegment** for OHLC (`:531-540`): `NSE/NSE_INDEX→NSECM, BSE/BSE_INDEX→BSECM, NFO→NSEFO, BFO→BSEFO, CDS→NSECD, MCX→MCXFO`. The depth path's map omits the index exchanges (`:843`).

Token lookup is by `SymToken.exchange == exchange AND brsymbol == get_br_symbol(symbol)` (`:148-158`).

**Quotes** `POST /instruments/quotes` (`:173-185`):
```json
{"instruments":[{"exchangeSegment": <int>, "exchangeInstrumentID": "<token>"}], "xtsMessageCode": 1502, "publishFormat": "JSON"}
```
`result.listQuotes` is a list of **JSON strings** (`:207-217`, `json.loads`). Codes used: `1502` for touchline+depth (`:242`), `1510` for OI (`:249`, best-effort). Touchline fields: `AskInfo.Price, BidInfo.Price, High, Low, LastTradedPrice, Open, Close (→prev_close), TotalTradedQuantity` (`:254-265`); OI from `OpenInterest` of the 1510 payload (`:268-269`). Multiquotes: batches of **50** instruments with **0.1 s** gap (`:291-292`), response matched back by `f"{ExchangeSegment}_{ExchangeInstrumentID}"` (`:463-465`); OI not fetched in F's multiquotes (rmoney adds it).

**Depth** = same 1502 call; `Bids[]/Asks[]` top-5 of `{Price, Size}` (`:896-903`); extra fields `LastTradedQunatity` (sic, XTS typo), `TotalBuyQuantity, TotalSellQuantity` (`:906-919`). Empty 5-level structure on error (`:924-939`).

**History** `GET /instruments/ohlc` (`:583-599`) with query params:
```
exchangeSegment=<NSECM..>  exchangeInstrumentID=<token>
startTime="%b %d %Y %H%M%S"  endTime=same      # IST wall-clock, e.g. "Jan 02 2026 000000"  (:575-576)
compressionValue = 1|60|120|180|300|600|900|1800|3600|D   # timeframe map :508-519 (1s,1m,2m,3m,5m,10m,15m,30m,60m,D)
```
Chunking: 6-day windows (`current_start + timedelta(days=6)`, `:571-572`), from 00:00:00 to 23:59:59 IST (`:559-566`). Response `result.dataReponse` (**typo key**, `:608`) is a string of rows separated by `,` with fields separated by `|`: `epoch|open|high|low|close|volume` (`:614-633`). Post-processing (`:717-741`): sort+dedupe; for `D` snap to midnight; for intraday **subtract 5:30 h** then floor to interval (so broker epochs are IST-shifted). If no rows and `D` requested for today, synthesise a candle from a 1502 quote (`:642-711`). `get_intervals()` returns `["1s","1m","2m","3m","5m","10m","15m","30m","60m","D"]` (`:765`).

### A.5 Funds — `F/api/funds.py`
`GET {INTERACTIVE_URL}/user/balance` (`:25`). Read `result.BalanceList[0].limitObject.RMSSubLimits` (`:31-36`); keys `netMarginAvailable→availablecash, collateral→collateral, UnrealizedMTM→m2munrealized, RealizedMTM→m2mrealized, marginUtilized→utiliseddebits` (`:38-63`), each `f"{float:.2f}"`, `"nan"`→`"0.00"`. `cashAvailable`/`MTM` are not read.

### A.6 Master contract — `F/database/master_contract_db.py`

Download: for each segment in `["NSECM","NSEFO","BSECM","BSEFO"]` (`:94`) `POST {MARKET_DATA_URL}/instruments/master` body `{"exchangeSegmentList":[segment]}` (`:104-106`, **no auth header** `:100`). `result` is one big string: rows split on `\n`, fields on `|` (`:123-126`). Column headers are supplied client-side:

- CM segments (`:95`): `ExchangeSegment,ExchangeInstrumentID,InstrumentType,Name,Description,Series,NameWithSeries,InstrumentID,PriceBand.High,PriceBand.Low, FreezeQty,TickSize,LotSize,Multiplier,DisplayName,ISIN,PriceNumerator,PriceDenominator,DetailedDescription,ExtendedSurvIndicator,CautionIndicator,GSMIndicator`
- FO/CD segments (`:96`): `ExchangeSegment,ExchangeInstrumentID,InstrumentType,Name,Description,Series,NameWithSeries,InstrumentID,PriceBand.High,PriceBand.Low,FreezeQty,TickSize,LotSize,Multiplier,UnderlyingInstrumentId,UnderlyingIndexName,ContractExpiration,StrikePrice,OptionType,DisplayName, PriceNumerator,PriceDenominator,DetailedDescription`

(The leading spaces in ` FreezeQty` / ` PriceNumerator` are literal and the pandas `dtype` map uses `" PriceNumerator"`, `:304`.)

Per-segment transform:
- NSECM (`:233-257`): keep `Series=="EQ"` (`:242`); `symbol=Name`, `brsymbol=DisplayName`, `exchange="NSE"`, `brexchange=ExchangeSegment`, `token=ExchangeInstrumentID`, `instrumenttype=Series`, `lotsize=LotSize`, `tick_size=TickSize`, `strike=1.0`.
- BSECM (`:260-293`): no Series filter; `Series=="SPOT"` rows → `exchange="BSE_INDEX"` (`:275-277`) with symbol normalised through `BSE_INDEX_SYMBOL_MAP` (`:186-222`, e.g. `SNSX50→SENSEX50`, `BSE IT→BSEINFORMATIONTECHNOLOGY`) and spaces/hyphens removed (`:225-230`).
- NSEFO/BSEFO/NSECD/MCXFO (`:296-480`): `symbol = Name + ContractExpiration.strftime("%d%b%y").upper() + [strike if option] + ("FUT" if OptionType==1 else "CE" if 3 else "PE")` (`:312-318`); strike rendered as int when whole, else decimal (`:315`); `brsymbol=Description`; `expiry="%d-%b-%y"` upper (`:330`); `instrumenttype` from `OptionType {1:FUT,3:CE,4:PE}` (`:333`) — CDS derives it from the symbol suffix instead (`:384-386`); MCX drops rows with `ContractExpiration=="1"` (`:451`). Exchange map `NSEFO→NFO, BSEFO→BFO, NSECD→CDS, MCXFO→MCX`.
- Index list (`:135-174`): `GET {MARKET_DATA_URL}/instruments/indexlist?exchangeSegment=1|11` (`:145`); `result.indexList[]` entries are `"NIFTY 50_26000"` → `rsplit("_",1)` into name/token (`:163`); exchange `NSE_INDEX` for 1, `BSE_INDEX` for 11 (`:169`). Rename map (`:504-513`): `NIFTY 50→NIFTY, NIFTY BANK→BANKNIFTY, INDIA VIX→INDIAVIX, NIFTY FIN SERVICE→FINNIFTY, NIFTY MID SELECT→MIDCPNIFTY, NIFTY NEXT 50→NIFTYNXT50, HANGSENG BEES NAV→HANGSENGBEESNAV`; BSE via the same `BSE_INDEX_SYMBOL_MAP`; then strip `[\s\-]+` (`:523`); `lotsize=1`, `instrumenttype="INDEX"`, `tick_size=0.05`, `brexchange=exchange` (`:525-531`). So BSE indices can arrive **twice** (SPOT rows + indexlist); `copy_from_dataframe` dedupes on `token` only (`:72-75`).
- Flow (`:547-597`): download → `delete_symtoken_table()` → per-segment processing each wrapped in try/except → index → delete temp CSVs → `socketio.emit("master_contract_download", ...)`.

### A.7 Streaming — Socket.IO market data

Client `F/streaming/fivepaisaxts_websocket.py`, adapter `F/streaming/fivepaisaxts_adapter.py`, mapping `F/streaming/fivepaisaxts_mapping.py`.

1. **Login (again, inside the socket client)** `POST {BASE_URL}/apibinarymarketdata/auth/login` (`websocket.py:161` — note: `apibinarymarketdata` even though REST uses `/apimarketdata`), body `{"appKey","secretKey","source":"WebAPI"}` (`:163-167`), reads `result.token` and `result.userID` (`:181-182`). Credentials come from `BROKER_API_KEY_MARKET/_SECRET_MARKET` (`adapter.py:105-106`); the DB feed token is fetched (`adapter.py:96-97`) only to prove the user is logged in.
2. **Connect** `sio.connect(f"{BASE_URL}/?token={market_token}&userID={userID}&publishFormat=JSON&broadcastMode=FULL", transports=["websocket"], socketio_path="/apimarketdata/socket.io")` (`:217-232`, `SOCKET_PATH :26`). No auth header.
3. **Subscribe** is REST, not socket: `POST {MARKET_DATA_URL}/instruments/subscription` headers `{"Authorization": market_token}` body `{"instruments":[{"exchangeSegment":<int>,"exchangeInstrumentID":"<token>"}], "xtsMessageCode": code}` (`:278-293`); the response's `result.listQuotes[]` (JSON strings) are fed to the data callback as initial snapshots (`:302-311`). **Unsubscribe** = `PUT` same URL/body (`:385-390`).
4. **Mode → code** (`:50-54`): `1 (LTP)→1512, 2 (Quote)→1501, 3 (Depth)→1502`. Depth levels 5 or 20 accepted (`adapter.py:495`); capability table `NSE/NFO:[5,20]`, others `[5]` (`mapping.py:129-138`).
5. **Events registered** (`:133-148`): `1501-json-full/partial, 1502-json-full/partial, 1505-json-full/partial, 1510-json-full/partial, 1512-json-full/partial, 1105-json-full/partial`, plus `message` and a `*` catch-all (`:151`). `1502` payloads may arrive as JSON **strings** (`:534-540`).
6. **Message layouts** (`adapter.py:970-1004`): for `MessageCode==1502` the OHLC/LTP live under `Touchline{LastTradedPrice, LastTradedTime, TotalTradedQuantity, Open, High, Low, Close, LastTradedQunatity, AverageTradedPrice, TotalBuyQuantity, TotalSellQuantity}` with root `Bids[]/Asks[]` of `{Price, Size, TotalOrders}` (`:1083-1088`), `OpenInterest`, `UpperCircuitLimit`, `LowerCircuitLimit`; for `1501`/`1512` the same fields sit at the **root** (`1512` carries `LastTradedPrice, LastTradedTime, LastTradedQunatity`). Instrument identity: root `ExchangeSegment` (int) + `ExchangeInstrumentID` (`:818-819`). Mode is decided from the message's `MessageCode` (`:900-912`); ZMQ topic `f"{exchange}_{symbol}_{LTP|QUOTE|DEPTH}"` (`:926`). Output per mode (`:1006-1064`): LTP `{ltp,ltt,ltq}`; Quote adds `volume,open,high,low,close,last_quantity,average_price,total_buy_quantity,total_sell_quantity`; Depth adds `oi, upper_circuit, lower_circuit, depth{buy[],sell[]}` padded to 5 and capped at 20 (`:1093-1096`).
7. **Index resolution**: segment 1 tokens `26000 NIFTY, 26001 BANKNIFTY, 26008 FINNIFTY, 26037 MIDCPNIFTY` and segment 11 `1 SENSEX, 12 BANKEX` are tried against `NSE_INDEX`/`BSE_INDEX` first (`adapter.py:155-168,850-864`); otherwise fall back to the index exchange when the cash lookup misses (`:871-887`).
8. **1105 "binary" text format** (`websocket.py:604-668`): `t:<seg>_<id>,110:<ltp>,111:<ltq>,112:<vol>,113:<avg>,114:<open>,115:<high>,116:<low>,117:<close>,118:<tbq>,119:<tsq>`; only forwarded if the instrument is in the local subscription book.
9. **Heartbeat / stall**: XTS has no heartbeat; F runs a watchdog every 30 s and forces a transport drop after 90 s of silence **only while a subscribed segment's session is open** (equity 09:15–15:30, MCX 09:00–23:30 IST; MCX segment ids `"5"` or `"51"`) (`:63-73,416-488`). Reconnect with exponential backoff `5·2^n` capped 60 s, max 10 attempts (`adapter.py:58-61,276`).
10. **Subscription batching** (F only, newest template): queue per mode, ≤50 instruments per POST, 0.5 s between batches, 0.5 s collect window (`adapter.py:36-50,361-471`).
11. **Order-update socket**: **none** of the 7 XTS plugins opens `/interactive/socket.io`; no `*_order_adapter.py` exists in any XTS dir (grep). Order updates fall back to `websocket_proxy/order_adapter.py:375 PollingOrderUpdateAdapter` (REST polling).
12. JWT helper `_extract_client_id_from_token` decodes the feed-token payload's `userID` (`adapter.py:177-219`) but is not on the hot path.

### A.8 Rate limiting
No broker-side pacer in the XTS family. Only: multiquote batch 50 / 0.1 s (`F/api/data.py:291-292`), streaming subscribe batch 50 / 0.5 s (`adapter.py:36-39`). Global Flask limits come from `.sample.env:192-195` (`API_RATE_LIMIT="100 per second"`, `ORDER_RATE_LIMIT="10 per second"`, `SMART_ORDER_RATE_LIMIT="10 per second"`, `WEBHOOK_RATE_LIMIT="100 per minute"`). Smart orders are serialised per symbol with a 1 s position-book cache (`F/api/order_api.py:81-120`).

---

### B. PER-BROKER DELTA TABLE

### B.1 Normalised diff size vs fivepaisaxts (changed lines / file length; broker names normalised)

| File | jainamxts | compositedge | rmoney | ibulls | wisdom | iifl |
|---|---|---|---|---|---|---|
| api/auth_api.py | 10/104 | 10/104 | 74/94 | **0**/100 | 4/100 | **0**/100 |
| api/order_api.py | 104/463 | 10/443 | 140/471 | 297/554 | 50/439 | 34/437 |
| api/data.py | 93/1016 | 10/963 | 118/1063 | 56/963 | 14/963 | 123/994 |
| api/funds.py | 10/66 | 4/68 | 22/68 | 0/68 | 0/68 | 0/68 |
| api/margin_api.py | 0 | 0 | 88/94 | 0 | 0 | 0 |
| mapping/transform_data.py | 2 | 0 | 53/134 | 71/160 | 2 | 0 |
| mapping/order_data.py | 10 | 6 | 8 | 4 | 22/483 | 2 |
| mapping/margin_data.py | 0 | 0 | 147/159 | 0 | 0 | 0 |
| database/master_contract_db.py | 50/611 | 92/585 | **0**/603 | 116/523 | 58/585 | 58/585 |
| streaming/*_mapping.py | 0 | 14 | 25 | 22 | 14 | 14 |
| streaming/*_websocket.py | 896/1072 | 300/552 | 1253/1059 | 253/549 | 303/551 | 248/552 |
| streaming/*_adapter.py | 462/876 | 398/882 | 706/946 | 318/878 | 404/880 | 318/878 |

Reading: the REST layer is essentially one codebase (most non-zero cells are log-level/docstring churn). The streaming layer diverges because fivepaisaxts received the newer watchdog + batching template while compositedge/ibulls/wisdom/iifl/jainamxts still carry the older one (no `MODE_TO_XTS_CODE` constant, no `_stamped`, no batching, `time.sleep` backoff, unsubscribe reads code from stored correlation id). rmoney's socket client is a separate rewrite.

### B.2 Delta table

| | fivepaisaxts | jainamxts | compositedge | rmoney | ibulls | wisdom | iifl |
|---|---|---|---|---|---|---|---|
| Plugin name | "5paisa (XTS)" `plugin.json:2` | "jainamxts" | "compositedge" | "RMoney" | "IBulls" | "Wisdom Capital (XTS)" | "IIFL" |
| `BASE_URL` | `https://xtsmum.5paisa.com/` **trailing slash → `//apimarketdata`** (`baseurl.py:4,7`) | `https://jtrade.jainam.in:5000` (`:4`) | `https://xts.compositedge.com` (`:4`) | `https://xts.rmoneyindia.co.in:3000` (`:7`); `HOSTLOOKUP_URL=https://xts.rmoneyindia.co.in:4000/hostlookup` (`:4`) | `https://xts.ibullssecurities.com` (`:4`) | `https://trade.wisdomcapital.in` (`:4`) | `https://ttblaze.iifl.com` (`:4`) |
| REST market-data path | `/apimarketdata` | **`/apibinarymarketdata`** (`:7`) | `/apimarketdata` | **`/apibinarymarketdata`** (`:14`) | **`/apibinarymarketdata`** (`:7`) | `/apimarketdata` | `/apimarketdata` |
| Socket.IO path | `/apimarketdata/socket.io` (`ws:26`) | **`/apibinarymarketdata/socket.io`** (`ws:25`) | `/apimarketdata/socket.io` (`ws:25`) | `/apimarketdata/socket.io` + login/subscribe under `/apimarketdata` (`ws:42-43,105-107`) — **differs from its REST path** | `/apimarketdata/socket.io` (`ws:22`) | `/apimarketdata/socket.io` (`ws:24`) | `/apimarketdata/socket.io` (`ws:25`) |
| Socket login URL | `{BASE}/apibinarymarketdata/auth/login` (`ws:161`) | same (`ws:131`) | same (`ws:124`) | `{BASE}/apimarketdata/auth/login`, payload **without** `source` (`ws:105,252-255`) | same as F (`ws:121`) | same (`ws:123`) | same (`ws:124`) |
| Interactive login body | `{appKey, secretKey, source:"WebAPI"}` (`auth:23`) | `{appKey, secretKey, accessToken: request_token}` — **no `source`**; `request_token` is the literal `"jainamxts"` (`brlogin.py:377`) | `{appKey, secretKey, accessToken: <OAuth accessToken>}` (auth diff vs F lines 23-27; token from callback `brlogin.py:234`) | **no `/user/session` call**: OAuth callback JSON already contains `token` + `userID` (`brlogin.py:942-943`); `authenticate_broker` returns `request_token` as-is and only fetches the feed token (rmoney `auth_api.py:36-53`) | identical to F | identical to F | identical to F |
| Front-door / login flow | direct (`brlogin.py:171-177`) | direct (`:376-382`) | OAuth: frontend sends user to `https://xts.compositedge.com/interactive/thirdparty?appKey=&returnURL=` (`frontend/src/pages/BrokerSelect.tsx:174`); callback `POST session=<json>` (`brlogin.py:188-241`); session may be re-established for admin user (`:1048-1062`) | OAuth: redirect to `{INTERACTIVE_URL}/thirdparty?appKey=&returnURL=` built from `HOST_SERVER` (`brlogin.py:967-979`); callback `session=` JSON (`:921-943`) | direct (`:292-298`) | direct (`:573-577`) | direct (`:300-306`) |
| `user_id` persisted? | no (`brlogin.py:1073`) | no | **yes** (`:1046,1066`) | **yes** | no | no | no |
| hostlookup | – | – | – | `get_host_lookup()` posts `{"AccessPassword":"2021HostLookUpAccess","version":"interactive_1.0.1"}` → `result.UniqueKey`, `connectionString` (rmoney `auth_api.py:10-33`) — **defined, never called** | – | – | – |
| `isInvestorClient`/`clientID` | unused | unused | unused | unused | unused | unused | unused |
| supported_exchanges | NSE,BSE,NFO,BFO,NSE_INDEX,BSE_INDEX (`plugin.json:8`) | same | +CDS,MCX | NSE,BSE,NFO,BFO,+idx | +MCX (no CDS) | +CDS,MCX | +CDS,MCX |
| Master segments requested | `NSECM,NSEFO,BSECM,BSEFO` (`mc:94`) | same 4 (`mc:94`); CDS/MCX processors exist with empty-df guards but are **not called** | `NSECM,NSECD,NSEFO,BSECM,BSEFO,MCXFO` (`mc:93`), no per-segment try/except | identical to F (diff = 0) | `NSECM,NSEFO,BSECM,BSEFO,MCXFO` (`mc:93`); CDS processor removed | 6 segments (`mc:93`) | 6 segments (`mc:93`) |
| Funds | `BalanceList[0]` | same | same | picks entry with `limitHeader=="ALL|ALL|ALL"`, falls back to `[0]` (rmoney `funds.py:29-38`) | same | same | same |
| Margin calculator | `NotImplementedError` | same | same | **`POST {INTERACTIVE}/orders/margindetails` `{portfolio:[{exchange:<int seg>, exchangeInstrumentId:int, productType, orderType, orderSide, quantity:int, price:float, stopPrice:float, orderSessionType:1}]}` → `result.brokerageDeatils{MarginRequired, MarginAvailable, MarginShortfall, IsValid, ErrorMessage}`** (rmoney `margin_api.py:60-86`, `margin_data.py:58-66,95-100,151-157`) | same as F | same | same |
| transform_data (place) | strings | same | same | **typed**: `exchangeInstrumentID:int, disclosedQuantity:int, orderQuantity:int, limitPrice:float, stopPrice:float`; `map_exchange` raises `ValueError` on unknown; adds `map_exchange_numeric` (`transform_data.py:20-28,65-95`) | **MARKET orders rewritten as LIMIT** at `bid×1.001` (BUY) / `ask×0.999` (SELL) using a quote fetched with the feed token of `session["username"]`, **fallback literal user `"kalaivani"`** (ibulls `transform_data.py:22-75`) — do not port | same | same |
| get_open_position | broken shape (A.3) | fixed: `type/result.positionList`, matches `TradingSymbol`, `ExchangeSegment`(mapped), `ProductType` | broken as F | fixed; also accepts `result` as flat list | fixed but matches symbol+product only, scans `NetQty/Quantity/netQty/NetQuantity/net_qty` | broken as F | broken as F |
| Position id field | `ExchangeInstrumentId` | same | same | `ExchangeInstrumentID` **or** `Id` (`order_data.py:295`, `order_api` close-all) | same | same | same |
| data.py extras | single-shot refresh in `_fetch_market_data` | `int(token)` in quotes; refresh-retry in history + today-candle; `FEED_TOKEN` falls back to auth token | – | multiquotes adds `bid_qty/ask_qty` (`AskInfo.Size/BidInfo.Size`) and merges a second **1510 OI** call; refresh-retry in multiquotes/history | logging only | logging only | `_market_data_request` wrapper: one "Invalid Token" refresh+retry for quotes/multiquotes/ohlc (iifl `data.py:120-161`) |
| order_data extras | – | – | – | – | – | tradebook int/float guarded; `map_position_data` returns `{"positionList": []}` when empty (wisdom `order_data.py:231-245,275`) | – |
| Streaming template | new (watchdog, batching, `MODE_TO_XTS_CODE`) | old + **`xts-binary-packet`** handler: 16-byte header `<H pktType, <H msgCode, <h seg, <i instId, <h bookType, <h xMarketType, <H uncompressedSize`; skips LZ4 (`pktType & 0x100`); LTP at payload offset 85 (1501) / 166 (1502) (jainam `ws:580-650`) | old | rewritten: `reconnection=False`, Engine.IO ping floor 295 s / activity 300 s (`ws:44-46,157-162,202-237`), `joined` + `connect_error` events (`ws:167-169`), `MODE_TO_XTS_CODE {1:1501, 2:1501, 3:1502}` (`ws:65-70`) so LTP mode gets touchline, `broadcastMode:"Full"` via `urlencode` (`ws:344-353`), binary `xts-binary-packet` decoder with per-code double offsets (`ws:739-844`), `requests.Session` reuse (`ws:137`) | old | old | old |
| Order-update feed | polling fallback | polling | polling | polling | polling | polling | polling |
| Byte-identical to F except URLs/names? | – | **No** (auth body, socket path, binary handler, fixed position lookup) | **Nearly** for REST (auth body differs: `accessToken`, OAuth); master segments 6; streaming is older template | **No** (OAuth, margin API, typed payloads, funds selector, socket rewrite) | REST **yes except** ibulls' market→limit hack and smart-order restructure; master segments 5; streaming older | REST **yes** (log levels + tradebook guards); master segments 6; streaming older | REST **yes** (+ centralised token refresh); master segments 6; streaming older |

Other shared facts: all 7 use `orderUniqueIdentifier:"openalgo"`, all read `AppOrderID` for the order id, all call `/portfolio/positions?dayOrNet=NetWise`, all build symbols identically from the master.

---

### C. RUST DESIGN

```rust
/// Static per-broker configuration. One `const` per white-label.
pub struct XtsConfig {
    pub id: &'static str,                 // "fivepaisaxts" | "jainamxts" | ...
    pub name: &'static str,               // plugin display name
    pub base_url: &'static str,           // scheme://host[:port], NO trailing slash (fixes F's "//apimarketdata")
    pub interactive_path: &'static str,   // "/interactive" everywhere
    pub marketdata_rest_path: &'static str, // "/apimarketdata" | "/apibinarymarketdata"
    pub marketdata_socket_path: &'static str, // "/apimarketdata/socket.io" | "/apibinarymarketdata/socket.io"
    pub marketdata_socket_login_path: &'static str, // "/apibinarymarketdata/auth/login" (F) | "/apimarketdata/auth/login" (rmoney)
    pub source: Option<&'static str>,     // Some("WebAPI") for F/ibulls/wisdom/iifl; None for jainam/compositedge (they send accessToken) and rmoney socket login
    pub login: XtsLogin,                  // how the interactive token is obtained
    pub supported_exchanges: &'static [Exchange],
    pub master_segments: &'static [XtsSegment], // e.g. [NSECM, NSEFO, BSECM, BSEFO] or 6
    pub index_segments: &'static [u8],    // [1, 11]
    pub stream_mode_codes: [u16; 3],      // [1512, 1501, 1502] default; rmoney [1501, 1501, 1502]
    pub quirks: XtsQuirks,
}

pub enum XtsLogin {
    /// POST /interactive/user/session {appKey, secretKey, source}
    Direct,
    /// Redirect to {interactive}/thirdparty?appKey&returnURL; callback `session=` JSON has `accessToken`,
    /// then POST /user/session {appKey, secretKey, accessToken}   (compositedge)
    OAuthAccessToken { thirdparty_path: &'static str },
    /// Same redirect; callback JSON already holds `token` + `userID`; no /user/session call (rmoney)
    OAuthSessionToken { thirdparty_path: &'static str },
}

#[derive(Default)]
pub struct XtsQuirks {
    pub typed_order_fields: bool,         // ints/floats instead of strings (rmoney); recommend true for all
    pub funds_balance_header: Option<&'static str>, // Some("ALL|ALL|ALL") for rmoney
    pub margin_details: bool,             // POST /orders/margindetails supported (rmoney)
    pub position_id_key_uppercase: bool,  // accept "ExchangeInstrumentID" as well as "ExchangeInstrumentId" (always do both)
    pub binary_packets: bool,             // register "xts-binary-packet" (jainam, rmoney)
    pub socket_login_sends_source: bool,  // false for rmoney
    pub engineio_ping_floor_secs: Option<u32>, // Some(295) for rmoney
    pub market_to_limit_hack: bool,       // ibulls — recommend false (drop)
}

pub struct XtsBroker<C: XtsConfigProvider> { cfg: &'static XtsConfig, http: Client, session: RwLock<Option<XtsSession>>, _c: PhantomData<C> }

pub struct XtsSession { pub interactive_token: String, pub market_token: Option<String>, pub market_user_id: Option<String> }
```

`impl Broker for XtsBroker<C>` is fully generic; the following methods consult `cfg`/`quirks`:

| Trait method | Shared implementation | Per-broker hook |
|---|---|---|
| `login_url()` / `authenticate(callback)` | feed login always `POST {md_rest}/auth/login {appKey,secretKey,source?}` | `XtsLogin` variant; `source` |
| `place_order` | `/orders` payload per A.3 | `typed_order_fields`; (`market_to_limit_hack` — recommend not implementing) |
| `modify_order`, `cancel_order` | `PUT /orders`, `DELETE /orders?appOrderID=`; **check `type=="success"`** (fix Python's `status` check) | none |
| `cancel_all` | loop book, statuses `New`,`Trigger Pending`,(`Open`,`PartiallyFilled` recommended) | none |
| `close_positions` | loop `positionList`, MARKET orders | `position_id_key_uppercase` (just read both keys) |
| `positions/orderbook/tradebook/holdings` | A.3 maps; normalise status map to include `PartiallyFilled→open`, `Trigger Pending→trigger pending` | none |
| `funds` | `/user/balance` RMSSubLimits | `funds_balance_header` selector |
| `margin` | `Err(Unsupported)` | `margin_details` → `/orders/margindetails` |
| `quotes/multiquotes/depth` | 1502 (+1510 OI), batches of 50, `listQuotes` JSON strings, single feed-token refresh on `"Invalid Token"` (centralised like iifl) | `marketdata_rest_path` |
| `history` | `/instruments/ohlc` params, 6-day chunks, `dataReponse` parser, IST shift | `marketdata_rest_path` |
| `master_contract` | `/instruments/master` per segment + `/instruments/indexlist`, A.6 transforms | `master_segments`, `index_segments` |
| `stream.connect` | socket login → `?token&userID&publishFormat=JSON&broadcastMode=FULL`, REST subscribe/unsubscribe, batching 50/0.5 s, watchdog 30/90 s session-aware | `marketdata_socket_path`, `marketdata_socket_login_path`, `socket_login_sends_source`, `stream_mode_codes`, `binary_packets`, `engineio_ping_floor_secs` |
| `stream.decode` | A.7 §6 JSON layouts, 1105 text, index-token resolution | `binary_packets` decoder (16-byte header; LZ4 unsupported) |
| `order_updates` | polling adapter | none (no XTS plugin uses `/interactive/socket.io`) |

Concrete configs (values only, all cited in B.2):

| id | base_url | md_rest | socket path | socket login | source | login | master segments |
|---|---|---|---|---|---|---|---|
| fivepaisaxts | `https://xtsmum.5paisa.com` | `/apimarketdata` | `/apimarketdata/socket.io` | `/apibinarymarketdata/auth/login` | WebAPI | Direct | NSECM,NSEFO,BSECM,BSEFO |
| jainamxts | `https://jtrade.jainam.in:5000` | `/apibinarymarketdata` | `/apibinarymarketdata/socket.io` | `/apibinarymarketdata/auth/login` | – (`accessToken:"jainamxts"`) | Direct* | same 4 |
| compositedge | `https://xts.compositedge.com` | `/apimarketdata` | `/apimarketdata/socket.io` | `/apibinarymarketdata/auth/login` | – | OAuthAccessToken `/interactive/thirdparty` | 6 |
| rmoney | `https://xts.rmoneyindia.co.in:3000` | `/apibinarymarketdata` | `/apimarketdata/socket.io` | `/apimarketdata/auth/login` (no source) | – | OAuthSessionToken `/interactive/thirdparty` | 4 (same as F) |
| ibulls | `https://xts.ibullssecurities.com` | `/apibinarymarketdata` | `/apimarketdata/socket.io` | `/apibinarymarketdata/auth/login` | WebAPI | Direct | NSECM,NSEFO,BSECM,BSEFO,MCXFO |
| wisdom | `https://trade.wisdomcapital.in` | `/apimarketdata` | `/apimarketdata/socket.io` | `/apibinarymarketdata/auth/login` | WebAPI | Direct | 6 |
| iifl | `https://ttblaze.iifl.com` | `/apimarketdata` | `/apimarketdata/socket.io` | `/apibinarymarketdata/auth/login` | WebAPI | Direct | 6 |

\*jainamxts: the Python sends `accessToken:"jainamxts"` with no `source` — a `Direct` login whose body shape matches compositedge's. Model as `XtsLogin::Direct` with `source=None` + `extra_body=[("accessToken", id)]`, or verify against Jainam docs whether `source:"WebAPI"` is accepted.

Recommended Rust-side corrections to Python behaviour: strip trailing slash from base URLs; use `type=="success"` for cancel/modify; use the corrected position lookup (jainam/rmoney variant) for smart orders; always send typed numbers (rmoney variant) — XTS accepts them and it removes a class of string bugs; do not port ibulls' market→limit conversion or its hard-coded username fallback.

---

### D. STANDALONE SPEC — `broker/iiflcapital` (NOT XTS)

Base: `BASE_URL="https://api.iiflcapital.com/v1"`, `LOGIN_URL="https://markets.iiflcapital.com/"` (`broker/iiflcapital/baseurl.py:3-4`). plugin: `supported_exchanges NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX` (`plugin.json:8`).

**Auth** (`api/auth_api.py`): login URL `{LOGIN_URL}?v=1&appkey=<BROKER_API_KEY>&redirecturl=<REDIRECT_URL>&redirectUrl=<REDIRECT_URL>` (`:28-33`). Callback params `authCode` (variants `authcode/auth_code/code`) and `clientId` (variants; fallback to `BROKER_API_KEY` or its `clientid:::appkey` prefix) (`blueprints/brlogin.py:308-332`). `checkSum = sha256(clientId + authCode + BROKER_API_SECRET)` (`auth_api.py:12-14,51`); `POST {BASE}/getusersession {"checkSum"}` → `status=="ok"`, `userSession` JWT (`:56-71`). Returns 2-tuple `(token, error)`. Headers everywhere: `Authorization: Bearer <userSession>`, `Content-Type/Accept: application/json` (`api/order_api.py:80-83`, `api/funds.py:26-31`, `api/data.py:489-493`). Session rolls daily ~03:00 IST (`streaming/iiflcapital_adapter.py:87-89`).

**Endpoints** (`api/order_api.py`): `GET /orders` (`:288`), `GET /trades` (`:306`), `GET /positions` (`:321`), `GET /holdings` (`:326`), `POST /orders` with a **list** `[order]` (`:379-381`) → `result[0].brokerOrderId` (`:386-387`), `PUT /orders/{id}` (`:529`), `DELETE /orders/{id}` (`:505`); success = `status in {"success","ok"}` possibly nested (`:39,219-233`). Order writes that get 429/"try after some time" are **never retried** (`:30-38,155-162`); reads retry ≤3 with `Retry-After` or 1/2/4 s (`api/rate_limiter.py:66-69,127-143`). Process-wide pacer: min 0.125 s between calls, separate clock for order writes under gthread (`rate_limiter.py:57-112`). Cancel-all filters `orderStatus ∈ _OPEN_STATUSES` (`:548-549`).

**Order payload** (`mapping/transform_data.py:168-218`): `instrumentId:str, exchange ∈ {NSEEQ,BSEEQ,NSEFO,BSEFO,NSECURR,BSECURR,MCXCOMM,NCDEXCOMM}` (`:89-100`), `transactionType BUY|SELL, quantity:str, orderComplexity:"REGULAR", product ∈ {INTRADAY,DELIVERY,NORMAL}` (`:123-129`; reverse adds `BNPL→CNC` `:132-139`), `orderType ∈ {MARKET,LIMIT,SL,SLM}` (`:103-110`), `validity DAY|IOC`, `apiOrderSource:"openalgo"`, `price` (LIMIT/SL), `slTriggerPrice` (SL/SLM), optional `disclosedQuantity`, `orderTag` (strategy, ≤50 chars). **SL-M is converted to SL** with a tick-snapped MPP-protected limit (SELL below / BUY above trigger) (`:30-86,192-205`). Modify sends only changed fields (`:221-269`).

**Mappings** (`mapping/order_data.py`): exchange `NSEEQ→NSE, BSEEQ→BSE, NSEFO→NFO, BSEFO→BFO, NSECURR→CDS, BSECURR→BCD, MCXCOMM/NSECOMM/NCDEXCOMM→MCX` (`:89-99`); status `COMPLETE/COMPLETED/FILLED/SUCCESS/EXECUTED→complete`, `REJECTED/FAIL/FAILED→rejected`, `CANCELLED/CANCELED→cancelled`, `TRIGGER_PENDING→trigger pending`, else `open` (`:104-116`); order fields `transactionType, price, slTriggerPrice|triggerPrice, orderType, product, brokerOrderId|exchangeOrderId|orderId, orderStatus, rejectionReason` (`:220-236`); trades `filledQuantity|quantity|filledQty, tradedPrice|averageTradedPrice|price` (`:258-262`); positions `netQuantity|quantity, netAveragePrice|averagePrice, ltp|lastPrice|previousDayClose, realizedPnl` (`:297-311`).

**Funds** (`api/funds.py:159-207`): `GET /limits`, `/limits/equity`, `/limits/fno`; fields `tradingLimit|openingCashLimit→availablecash, collateralMargin→collateral, utilizedMargin→utiliseddebits` (+ extras `creditForSell, adhocMargin, utilizedSpanMargin, utilizedExposureMargin`); prefers pooled when non-zero else sums segments. **Margin**: `POST /spanexposure` with transformed positions (`api/margin_api.py:47-51`).

**Market data** (`api/data.py`): all `POST` through `_post` (`:474-497`): `/marketdata/marketquotes` body = list of `{"exchange": <NSEEQ..>, "instrumentId": "<token>"}` (`:540,611-619`; `brexchange` from DB, `INDICES`→normalised exchange `:612-614`); `/marketdata/openinterest` single instrument, fanned out ≤32 threads for chains (`:557,581-600`); `/marketdata/marketdepth` (`:765`), fields `depth.buy|bids / sell|asks`, `ltp, ltq, open, high, low, close|previousClose, volume, oi` (`:777-790`); `/marketdata/historicaldata {exchange, instrumentId, interval, fromDate, toDate}` (`:830-838`), dates `"%d-%b-%Y"` (`:449-453`), intervals `1m→"1 minute", 5m,10m,15m,30m,60m/1h→"60 minutes", D→"1 day", W→"weekly", M→"monthly"` (`:461-472`), candles as arrays `[ts, o, h, l, c, v, oi?]` with ms→s normalisation (`:350-371`). Exchange normalisation adds `NSE_INDEX→NSEEQ, BSE_INDEX→BSEEQ, MCX_INDEX→MCXCOMM` (`:215-229`).

**Master contract** (`database/master_contract_db.py`): `GET {BASE}/contractfiles/{NSEEQ,BSEEQ,NSEFO,BSEFO,NSECURR,BSECURR,NSECOMM,MCXCOMM,NCDEXCOMM,INDICES}.csv` (`:72-83`); columns `Exchange, Underlying Instrument Symbol, Instrument ID, Instrument Type, Option Type, Strike Price, Underlying Instrument Name, Trading Symbol, Expiry, Lot Size, Tick Size` (`:57-67`); equity: drop `Instrument Type=="INDEX"`, `symbol=Underlying…Symbol`, `brsymbol=Trading Symbol`, `instrumenttype="EQ"` (`:294-323`); derivatives: `Option Type XX→FUT, CE, PE` (`:391-392`), `symbol = underlying + DDMMMYY + [strike] + FUT|CE|PE` (`:397-417`), expiry `"%d-%b-%y"` (`:255-263`); indices from `INDICES.csv` with `Exchange NSEEQ→NSE_INDEX, BSEEQ→BSE_INDEX` (`:453-454`) and rename maps `NIFTY50→NIFTY, NIFTYBANK→BANKNIFTY, NIFTYFINSERVICE→FINNIFTY, NIFTYNEXT50→NIFTYNXT50, NIFTYMIDCAPSELECT→MIDCPNIFTY` + BSE short codes (`:103-138`). Download failures abort before truncating the table (`:504-513`).

**Streaming** — MQTT v3.1.1 over TLS to `bridge.iiflcapital.com:8883`, hand-rolled client (`streaming/iiflcapital_mqtt.py`; CONNECT flags username+password+clean-session `:334`, keepalive 20). Credentials: `username = JWT.preferred_username` (`streaming/iiflcapital_websocket.py:106-128,268`), `password = "OPENID~~{userSession}~"` (`:269`), `client_id = "openalgo"+%d%m%y%H%M%S%f+4 random bytes hex` (`:263-267`). Topics `prod/marketfeed/mw/v1/{seg_lower}/{token}`, `prod/marketfeed/index/v1/…`, `prod/marketfeed/oi/v1/…` (`:44-46,439-442`); ≤100 topics per SUBSCRIBE, ≤5800 per connection (`:57-61`); payload is binary `MWBOCombined` (C# Pack=2, little-endian, decoded with ctypes `:65-105`) — one packet shape, so the adapter subscribes once at the highest requested mode and fans out LTP/Quote/Depth (`streaming/iiflcapital_adapter.py:278-292`); depth is L5 only. **Order updates**: separate MQTT connection, same credential scheme, topics `prod/updates/order/v1/{clientId}` and `prod/updates/trade/v1/{clientId}` (`streaming/iiflcapital_order_adapter.py:111-112,130-131,566`), publishes `OrderUpdateEvent` on the event bus (`:705`); status map `:138-156`.

Rust: model as its own `IiflCapitalBroker` (REST + MQTT), not an `XtsConfig` instance.

---

## D. OAuth / redirect-callback brokers (arrow, paytm, aliceblue, definedge, pocketful, hdfcsky, hdfcsecurities) + dhan_sandbox delta

All paths below are relative to `/Users/openalgo/openalgo-desktop/openalgo/`. Citations are `file:line`.
Shared facts used by every broker in this part:

- Callback route: `blueprints/brlogin.py` `broker_callback(broker)`; success path stores `session["broker"]` and calls `handle_auth_success(auth_token, user, broker, feed_token=..., user_id=...)` (`brlogin.py:1033-1079`). Brokers that return `user_id` + `feed_token` 4-tuples: `pocketful`, `definedge` (list at `brlogin.py:1046`); `paytm` passes feed_token only (`brlogin.py:1068-1070`).
- Browser login URL is built client-side in `frontend/src/pages/BrokerSelect.tsx` (`brokerConfig.broker_api_key`, `redirect_url` from `blueprints/auth.py:141,151` = `REDIRECT_URL` env). Brokers that go straight to `/<broker>/callback` (backend-hosted or form login): `aliceblue`, `definedge`, `dhan_sandbox` (`BrokerSelect.tsx:135-160`).
- `utils/config.py` only exposes `get_broker_api_key()`/`get_broker_api_secret()` = `BROKER_API_KEY`/`BROKER_API_SECRET` env (`utils/config.py:11-28`); composite `:::` forms are parsed per-broker (table in `.claude/skills/broker-integration/references/auth-and-login.md:57-70`). Of the brokers here only **definedge** stores a `:::` composite — and it is the *stored auth token*, not the env key (see D4).
- OpenAlgo normalized order statuses: `complete | open | trigger pending | cancelled | rejected`. Normalized tick keys (zerodha shape): `ltp/last_price, open/high/low/close, volume, average_price, last_quantity, total_buy_quantity, total_sell_quantity, oi, depth.buy/sell[{price,quantity,orders}]`.

---

### D1. Arrow (`broker/arrow/`)

**Family**: Zerodha-style redirect + SHA256 checksum exchange (`SKILL.md:44`). Closest template: `broker/zerodha/` (adapter and smart-order code are explicit ports: `order_api.py:88-89`, `streaming/arrow_websocket.py:3`).

**Auth** (`api/auth_api.py`, `api/baseurl.py`)
- Credentials: `BROKER_API_KEY` = Arrow `appID`, `BROKER_API_SECRET` = `appSecret` (`auth_api.py:35-36`). No composite.
- Login URL (client): `https://app.arrow.trade/app/login?appID=${broker_api_key}` (`BrokerSelect.tsx:195-197`).
- Callback params: `request-token` (hyphen) or fallbacks `request_token`/`requestToken`/`code` (`brlogin.py:987-1000`). `authenticate_broker(request_token)` -> `(auth_token, error)` 2-tuple.
- Checksum: `SHA256(f"{appID}:{appSecret}:{request_token}")` — colon-separated, this order (`auth_api.py:41-43`).
- Exchange: `POST https://edge.arrow.trade/auth/app/authenticate-token` (`baseurl.py:16,20`), JSON `{"appID", "token": <request-token>, "checkSum"}` (capital S; `auth_api.py:45-51`). Response `{"status":"success","data":{"name","token","userID"}}`; stored auth = `data.token` (JWT) (`auth_api.py:63-69`). Failure envelope `{"status":"failure"|"error","message"}` (`auth_api.py:75-76`).
- Expiry: daily ~3 AM IST rollover (WS re-reads fresh JWT from DB: `arrow_websocket.py:12-13,170-187`).

**Base URLs / headers** (`api/baseurl.py:16-35,38-54`)
- REST `https://edge.arrow.trade`; history `https://historical-api.arrow.trade`; WS market `wss://ds.arrow.trade`; WS orders `wss://order-updates.arrow.trade`; HFT stream `wss://socket.arrow.trade` (unused).
- Headers on every call: `appID: <BROKER_API_KEY>`, `token: <JWT>` (NOT Bearer), plus `Content-Type: application/json` for bodies.

**REST endpoints** (`api/order_api.py`, `api/funds.py`, `api/margin_api.py`, `api/data.py`)
| Op | Method/path | Body / notes | Consumed |
|---|---|---|---|
| place | `POST /order/regular` (`order_api.py:176-178`) | `transform_data` payload below | `data.orderNo` (`:183-185`) |
| modify | `PATCH /order/regular/{orderid}` (`:338-342`) | same shape minus side | `data.orderNo` |
| cancel | `DELETE /order/regular/{orderid}` (`:310-312`) | plain-string body "order cancellation request accepted"; success = HTTP 200 (`:316-318`) | — |
| cancel-all | GET orderbook, cancel where `orderStatus ∈ {OPEN,PENDING,TRIGGER_PENDING}`, id = `orderNo` or `id` (`:364-375`) | | |
| orderbook / tradebook / positions / holdings | `GET /user/orders`, `/user/trades`, `/user/positions`, `/user/holdings` (`:71-84`) | envelope `{"status":"success","data":[...]}` flat list (`:146-147`) | |
| close-all | positions `qty`, reverse_map product, MARKET (`:272-302`) | | |
| funds | `GET /user/limits` (`funds.py:28`) | `data.margin.{allocated,utilized,unrealizedPnl,realizedPnl}`, `data.allocations[].nonCashCurrent` | availablecash = allocated-utilized; collateral = Σ nonCashCurrent; utiliseddebits = utilized (`funds.py:50-68`) |
| margin single | `POST /margin/order` body `{exchange, symbol(brsymbol), quantity, product(I/C/M), price, transactionType(B/S), order(MKT/LMT)}` (`mapping/margin_data.py:42-52`) | returns `requiredMargin`, `charge.total` (`margin_data.py:77-78`); span/exposure reported 0 |
| margin basket | `POST /margin/basket` `{"orders":[...], "includePositions": true}` (`margin_api.py:133-137`) | `final_margin`, `orders[].charge.total` (`margin_data.py:98-103`); falls back to per-order sum on failure |
| quote/depth | `POST /info/quote/full` `{"exchange","symbol"}` (`data.py:94-104`) | `ltp, open, high, low, close(prev), volume, oi, ltq, totalBuyQty, totalSellQty, bids[]/asks[]{price,quantity}` |
| multiquote | `POST /info/quotes/full` body = list of `{exchange,symbol}`; response `data[]` keyed by `token` (`data.py:366-382`) | |
| history | `GET {HISTORICAL_URL}/candle/{exch}/{token}/{interval}?from=YYYY-MM-DDT00:00:00&to=...T23:59:59[&oi=1]` (`data.py:423-429`) | bare list `[ts, o, h, l, c, v(, oi)]`; dict = error (`:435-443`) |

**Place payload** (`mapping/transform_data.py:66-95`): `{"exchange", "symbol": brsymbol, "quantity": str, "transactionType": "B"/"S", "order": MKT|LMT|SL-LMT|SL-MKT, "product": C|M|I, "price": str, "validity":"DAY", "disclosedQty": str, "remarks": strategy[:16], "mpp": true (only for MKT, since plain market is disabled), "triggerPrice": str (SL types; field name unconfirmed TODO)}`.

**Enums**
- OA->Arrow: pricetype `MARKET->MKT, LIMIT->LMT, SL->SL-LMT, SL-M->SL-MKT` (`transform_data.py:16-21`); product `CNC->C, NRML->M, MIS->I` (`:26-30`); action `BUY->B/SELL->S` (`:55-57`); validity always DAY. Exchange passed through as OpenAlgo code for orders.
- Arrow->OA status: `COMPLETE->complete, OPEN/PENDING->open, TRIGGER_PENDING->trigger pending, CANCELLED->cancelled, REJECTED->rejected` (`mapping/order_data.py:16-23`); order-stream adds `AFTER_MARKET_ORDER_REQ_RECEIVED->open` (`streaming/arrow_order_adapter.py:31-39`). Product reverse `C->CNC, M->NRML, I->MIS` (`transform_data.py:33-37`) — NOTE order-stream maps `M->MIS` (`arrow_order_adapter.py:46`), inconsistent.
- Quote exchange codes: `NSE,BSE,NFO,BFO` pass-through; **MCX -> "MCXFO"**; all `*_INDEX -> "INDEX"` (`mapping/exchange.py:43-52`). `QUOTE_UNSUPPORTED_EXCHANGES = {CDS,BCD,NCO}` — 400s whole batch (`:59`). History path segments lowercase, indices -> `nse`/`bse` (`:65-77`).
- Price scaling: quotes/history prices are paise ×100 -> divide by `PRICE_SCALE=100` (`data.py:22-32,470-474`); volume/oi raw.
- Index quote symbol vocabulary: 5 derivative indices answer to OA name (NIFTY, BANKNIFTY, FINNIFTY, MIDCPNIFTY, NIFTYNXT50), others to UPPERCASED display name; candidates tried in order `[oa_symbol, brsymbol.upper(), brsymbol]`, 400 => next, cached per token (`data.py:39-49,106-142`).

**Master contract** (`database/master_contract_db.py`)
- `GET https://edge.arrow.trade/all` (auth headers, CSV, ~221k rows) + `GET /info/index-list` -> `[{name, token}]` (`:5-7,122-158`).
- Columns used: `ExchSeg, Underlying, Symbol, TradingSymbol, FullName, OptionType, Expiry (DD-Mon-YYYY), StrikePrice, TickSize, LotSize, Token` (`:184-247`).
- ExchSeg map: `NSECM->NSE, BSECM->BSE, NSEFO->NFO, BSEFO->BFO, NSECD->CDS, BSECD->BCD, NSECO->NCO, MCXFO->MCX, NSEIDX->NSE_INDEX, BSEIDX->BSE_INDEX, MCXIDX->MCX_INDEX` (`mapping/exchange.py:25-37`). brexchange = ExchSeg; index-list rows get brexchange `"INDEX"` (`:289-292`).
- Currency segments: strike /100000, tick /100 (`:163-168,197-204`). Futures = OptionType `XX` (TradingSymbol ends `F`) or has expiry (`:207-213`).
- Symbols: equity = TradingSymbol with `-EQ` stripped only; FUT = `Underlying+DDMMMYY+FUT`; OPT = `Underlying+DDMMMYY+strike+CE/PE`; expiry stored `DD-MMM-YY` upper (`:192-195,231-233`). instrumenttype EQ/FUT/CE/PE (indices stored as EQ) (`:215-219`). Index renames: `_NSE_INDEX_MAP`/`_BSE_INDEX_MAP`/`_MCX_INDEX_MAP` keyed by upper-no-space display name (`exchange.py:88-215`).

**History**: interval map `1m->min, 3m->3min, 5m->5min, 10m->10min, 15m->15min, 30m->30min, 1h->hour, 2h->2hours, 3h->3hours, 4h->4hours, D->day, W->week, M->month` (`data.py:59-73`); chunk 2000 days for day/week/month else 60 (`:417`); `oi=1` only for NFO/BFO (`:405-406,428-429`); ISO8601 +0530 timestamps parsed, D/W/M shifted +5:30 to represent IST midnight, then epoch seconds (`:459-468`); 0.15 s sleep between chunks (`:251,448-449`).

**Streaming** (`streaming/arrow_websocket.py`, `arrow_adapter.py`, `arrow_mapping.py`)
- URL `wss://ds.arrow.trade?appID=<appID>&token=<JWT>` (`:131-132`); no header auth; TLS CERT_REQUIRED.
- Subscribe JSON text: `{"code":"sub","mode":<m>, <m>: [tokens...]}` where the array key EQUALS the mode string (`:331-333`); unsub `{"code":"unsub","mode":m, m:[...]}` (`:357`). Modes `ltp|ltpc|quote|full`; OA mode map `1->ltpc, 2->quote, 3->full` (`arrow_mapping.py:64-68`). Batches ≤100 tokens, 0.3 s apart, ≤1000/conn (`:72-74`).
- Heartbeat: client sends text `"PONG"` every 3 s (server ignores protocol pings; `:65-70,465-475`); data-stall watchdog 90 s; reconnect base 5 s ×1.5^n max 60 s, 50 tries; auth-failure refresh ≤3 (`:76-80,224-262`).
- Frames: ONE big-endian binary packet per message; size->mode `13 ltp, 17 ltpc, 93 quote, 249 full (241 legacy)` (`:62`). Offsets (`:521-540`): `token u32 @0, ltp u32 @4` (paise); ltpc: `close u32 @13`; quote (`">II5xIIQQIIIIQIIQQQ"`): `ltq@13, avg@17, tbq u64@21, tsq u64@29, open@37, high@41, close@45, low@49, volume u64@53, ltt@61, time@65, oi u64@69, oi_hi@77, oi_lo@85`; full: `lower_limit@93, upper_limit@97`, 10 depth levels ×14 B (`qty u64, price u32, orders u16`) starting @109 (249 B) or @101 (241 B); levels 0-4 bids, 5-9 asks. Prices /100.
- Topic `EXCHANGE_SYMBOL_{LTP|QUOTE|DEPTH}` (`arrow_adapter.py:25-30,188`).
- Order stream: `wss://order-updates.arrow.trade?appID=&token=`; JSON frames with `updateType=="ORDER_UPDATE"` and `id`; fields `orderStatus, quantity, cumulativeFillQty, leavesQuantity, price, orderTriggerPrice, order, product, transactionType, averagePrice, rejectionReason, symbol, exchange, token`; client "PONG" every 3 s (`arrow_order_adapter.py:27-122`).

**Limits/quirks**: multiquote hard cap 100/request (101 -> HTTP 500), 10 req/s; 0.15 s between batches (`data.py:241-251`). Equity brsymbol needs `-EQ` series suffix in quotes (`data.py:371-373`). Holdings consolidate under `symbols[]` array, first element used (`order_data.py:184-201`). Position qty field `qty`, prices `avgPrice/ltp/realisedPnL/unrealisedMarkToMarket` (`order_data.py:162-181`).

---

### D2. Paytm Money (`broker/paytm/`)

**Family**: OAuth-style redirect with request token exchanged using api_key + secret (no hash). Template: zerodha-like exchange but **no history API**; closest shape `broker/zerodha/` for auth, `broker/angel/` for binary WS.

**Auth** (`api/auth_api.py`)
- Login URL: `https://login.paytmmoney.com/merchant-login?apiKey=${broker_api_key}&state={default}` (`BrokerSelect.tsx:213-214`). Callback param `requestToken` (`brlogin.py:786-790`).
- Exchange: `POST https://developer.paytmmoney.com/accounts/v2/gettoken` JSON `{"api_key","api_secret_key","request_token"}` (`auth_api.py:31-39`). Response fields: `access_token` (REST), `public_access_token` (WS; stored as **feed_token**), `read_access_token` (unused) (`:45-56`). Falls back to access_token for both if public missing (`:57-63`). Errors `{"errors":[{"message"}]}` (`:71-74`). Returns `(access_token, public_access_token, error)`.

**Base URL / headers**: `https://developer.paytmmoney.com`; headers `x-jwt-token: <access_token>`, `Content-Type/Accept: application/json` (`api/order_api.py:28-33`, `api/data.py:20-25`). Reads retried 3× with 2 s delay on 5xx (`order_api.py:27-77`).

**REST endpoints**
| Op | Method/path | Notes |
|---|---|---|
| place | `POST /orders/v1/place/regular` (`order_api.py:341-343`) | orderid `data[0].order_no` (`:350-351`) |
| modify | `POST /orders/v1/modify/regular` (`:670-675`) | needs full order fields from book incl. `serial_no`, `group_id`, `off_mkt_flag`, `source:"N"` (`:648-666`); modifiable statuses OPEN/TRIGGER PENDING/MODIFIED/PENDING (`:638`) |
| cancel | `POST /orders/v1/cancel/regular` (`:606-608`) | body copies book row: `order_no, source:"N", txn_type, exchange, segment, product, security_id, quantity, validity, order_type, price, off_mkt_flag, mkt_type, serial_no, group_id` (`:586-604`); only when `status=="Pending"` |
| cancel-all | orders with `status=="Pending"` (`:701-704`) |
| orderbook & tradebook | both `GET /orders/v1/user/orders` (`:80-88`; no separate tradebook) |
| positions | `GET /orders/v1/position` (`:91-92`); fields `security_id, exchange, product, instrument, net_qty/netQty, display_name, buy_avg, sell_avg, net_avg, last_traded_price, net_val, realised_profit, unrealised_profit` |
| holdings | `GET /holdings/v1/get-user-holdings-data` (`:95-96`); rows `nse_security_id/bse_security_id, nse_symbol/bse_symbol, exchange ("ALL"), quantity, cost_price, last_traded_price, pc` (`order_data.py:460-503`) |
| funds | `GET /accounts/v1/funds/summary?config=true` (`funds.py:16-17`); `data.funds_summary.{available_cash, collaterals, utilised_amount}`; m2m from positions `realised_profit`/`unrealised_profit` sums (`:38-61`) |
| margin calc | not supported (`margin_api.py:21-24`) |
| quotes/depth/multiquote | `GET /data/v1/price/live?mode=FULL&pref=<EXCH:token:TYPE>[,...]` URL-encoded (`data.py:148-155,304-311`); `pref` type ∈ `EQUITY|FUTURE|OPTION|INDEX`, exchange NFO->NSE, BFO->BSE, *_INDEX->NSE/BSE (`:98-119`); response `data[].{security_id,last_price,ohlc{open,high,low,close},volume_traded,oi,change_oi,last_quantity,depth{buy[],sell[]{price,quantity}}}` (`:170-180,413-458`). bid/ask = 0 in quotes |
| history | NOT SUPPORTED — returns empty DF, `timeframe_map={}` (`data.py:64-66,469-489`) |

**Place payload** (`mapping/transform_data.py:7-38`): `{"security_id": token, "exchange": NSE|BSE (NFO->NSE, BFO->BSE), "txn_type": B|S, "order_type": MKT|LMT|SL|SLM, "quantity", "product": C|M|I, "price", "validity":"DAY", "segment": "E" (NSE/BSE) | "D" (derivatives), "source": "M"}` — trigger_price/disclosed_quantity NOT sent (`:28-29`).

**Enums**: pricetype `MARKET->MKT, LIMIT->LMT, SL->SL, SL-M->SLM` (`transform_data.py:96-103`); product `CNC->C, MARGIN->M, MIS->I` (`:87`) and back `C->CNC, M->MARGIN, I->MIS` (`:73-79`; order_data maps C->CNC on cash, I/M->MIS, derivatives->NRML `order_data.py:43-52`). Status (`display_status`): `Successful->complete, Rejected->rejected, Pending->trigger pending, Open->open, Cancelled->cancelled` (`order_data.py:121-130`). Exchange on responses: NSE/BSE + `instrument` containing OPT/FUT -> NFO/BFO (`order_data.py:34-37,138-142`). Prices are rupees (no scaling).

**Master contract** (`database/master_contract_db.py`): `GET https://developer.paytmmoney.com/data/v1/scrips/security_master.csv` (`:117`). Columns: `security_id, symbol, name, exchange, instrument_type, expiry_date, strike_price, lot_size, tick_size, series, segment, ...` (`:270-309`). instrument_type -> exchange/brexchange/type: `ES|ETF -> NSE/BSE EQ; I -> NSE_INDEX/BSE_INDEX "INDEX"; FUTIDX/FUTSTK -> NFO/BFO FUT; OPTIDX/OPTSTK -> NFO/BFO CE|PE` (CE/PE from `CALL`/`PUT` in `name`; `:192-249`). Symbols: equity = `symbol`; index = name uppercased no spaces (brsymbol same); FUT = `name.split()[0]+DDMMMYY+FUT`; OPT = `base+DDMMMYY+strike+CE/PE` (`:140-189`). token = security_id, brsymbol = `symbol`. Expiry `DD-MMM-YY` ("-1" when none) (`:261-267`). Index renames NSE: `NIFTYNEXT50->NIFTYNXT50, NIFTYMCAP50->NIFTYMIDCAP50, NIFTYSMALLCAP250->NIFTYSMLCAP250, NIFTYMIDSELECT->MIDCPNIFTY`; BSE map `SNSX50->SENSEX50, SNXT50->BSESENSEXNEXT50, AUTO->BSEAUTO, ...` (`:319-351`).

**Streaming** (`streaming/paytm_websocket.py`, `paytm_adapter.py`)
- URL `wss://developer-ws.paytmmoney.com/broadcast/user/v1/data?x_jwt_token=<public_access_token>` (`:18,232`). Protocol-level ping every 30 s, pong timeout 10 s (`:255-259`), CERT_REQUIRED.
- Subscribe: JSON array of preferences `[{"actionType":"ADD"|"REMOVE","modeType":"LTP"|"QUOTE"|"FULL","scripType":"INDEX|EQUITY|ETF|FUTURE|OPTION","exchangeType":"NSE"|"BSE","scripId":"<token>"}]` (`:153-181`, `paytm_adapter.py:416-422`); adapter debounces 500 ms and sends mixed modes in one frame (`paytm_adapter.py:64-72,287-316`).
- Binary frames, **little-endian**, first byte = packet code: `61 LTP(23 B), 62 QUOTE(67 B), 63 FULL(175 B), 64 INDEX_LTP(23), 65 INDEX_QUOTE(43), 66 INDEX_FULL(39)` (`:43-48,293-326`).
  - LTP: `code u8@0, last_price f32@1, ltt u32@5, security_id u32@9, tradable u8@13, mode u8@14, change_abs f32@15, change_pct f32@19` (`:332-345`).
  - QUOTE: + `ltq u32@15, atp f32@19, volume u32@23, tbq u32@27, tsq u32@31, open f32@35, close f32@39, high f32@43, low f32@47, change_pct f32@51, change_abs f32@55, 52wk_high f32@59, 52wk_low f32@63` (`:363-393`).
  - FULL: `depth 5×20 B @1` (per level: `buy_qty i32@0, sell_qty i32@4, buy_orders i16@8, sell_orders i16@10, buy_price f32@12, sell_price f32@16`), then `last_price@101, ltt@105, security_id@109, tradable@113, mode@114, ltq@115, atp@119, volume@123, tbq@127, tsq@131, open@135, close@139, high@143, low@147, change_pct@151, change_abs@155, 52h@159, 52l@163, oi u32@167, oi_change u32@171` (`:416-513`).
  - INDEX_QUOTE (43): `last_price@1, security_id@5, tradable@9, mode@10, open@11, close@15, high@19, low@23, change_abs@27, change_pct@31, 52h@35, 52l@39` (`:395-414`); INDEX_FULL (39): same to low then `change_pct@27, change_abs@31, ltt u32@35` (`:515-533`).
- Reconnect owned by adapter: 5 s ×2^n max 60 s, 10 attempts, reset if connected ≥30 s (`paytm_adapter.py:37-40,205-271`). No order-update stream. Depth 5 only.

**Quirks**: multiquote batch 100, 0.1 s delay (`data.py:200-201`). Smart-order position matching by security_id OR display_name substring (`order_api.py:308-331`). Holdings default exchange NSE unless `exchange=="BSE"` (`order_data.py:460-468`).

---

### D3. AliceBlue (`broker/aliceblue/`)

**Family**: redirect to vendor login + SHA256 checksum (`userId+authCode+apiSecret`) -> session JWT (V2 "open-api"). Template: `broker/zerodha/` for auth; **no REST quote API — quotes via Noren websocket** (`data-and-account.md:18-45`). Noren-lineage WS (like flattrade/shoonya).

**Auth** (`api/auth_api.py`, `brlogin.py:141-169`)
- `BROKER_API_KEY` = appCode (login redirect only), `BROKER_API_SECRET` = apiSecret (`auth_api.py:25-33`). Login form fields: `userid` only (`auth-and-login.md:201`); flow actually redirects: GET `/aliceblue/callback` -> `https://ant.aliceblueonline.com/?appcode=<appCode>` (`brlogin.py:160-169`); callback params `authCode`, `userId` (`brlogin.py:145-147`).
- `checkSum = SHA256(userId + authCode + apiSecret)`; `POST https://ant.aliceblueonline.com/open-api/od/v1/vendor/getUserDetails` JSON `{"checkSum"}` (`auth_api.py:44-58`). Response `{"stat":"Ok","userSession":<JWT>,"clientId"}`; error `{"stat":"Not_ok","emsg"}` (`:72-81`). Stored auth = `userSession` JWT; user_id = `clientId` (fallback callback `userId`) (`brlogin.py:155-156`); no feed token. UCC also readable from JWT claim `ucc` (`streaming/aliceblue_order_adapter.py:221-229`). Daily rollover (adapter refreshes token on reconnect `aliceblue_adapter.py:1000-1012`).

**Base URL / headers**: `https://a3.aliceblueonline.com` (`order_api.py:40`); `Authorization: Bearer <JWT>`, `Content-Type: application/json` (`:64-67`). Reads go through `apply_rate_limit()` — 1800 req / 15 min window, safety margin 50 (`api/rate_limiter.py:39-45`); order place/modify/cancel are NOT limited (`order_api.py:52-57`).

**REST endpoints**
| Op | Method/path | Body / consumed |
|---|---|---|
| place | `POST /open-api/od/v1/orders/placeorder` body = **list** `[payload]` (`order_api.py:355-366`) | `{"status":"Ok","result":[{"brokerOrderId","status","message"}]}` (`:374-385`) |
| modify | `POST /open-api/od/v1/orders/modify` (`:612`) | `{"brokerOrderId", "quantity", "orderType", "slTriggerPrice", "price", "slLegPrice":"", "trailingSlAmount":"", "targetLegPrice":"", "validity":"DAY", "disclosedQuantity", "marketProtection":"", "deviceId":""}` (`transform_data.py:104-121`) |
| cancel | `POST /open-api/od/v1/orders/cancel` `{"brokerOrderId"}` (`:562-568`) |
| cancel-all | normalized `Status ∈ {open, trigger pending}`, id `Nstordno` (`:652-661`) |
| orderbook | `GET /open-api/od/v1/orders/book` (`:131`) rows `brokerOrderId, exchange, tradingSymbol, formattedInstrumentName, transactionType, quantity, filledQuantity, pendingQuantity, price, slTriggerPrice, averageTradedPrice, orderType, product, orderStatus, orderTime, rejectionReason, orderTag, instrumentId` (`order_data.py:16-64`) |
| tradebook | `GET /open-api/od/v1/orders/trades` (`:152`) `tradedPrice, filledQuantity, fillTimestamp, exchangeTradeId` |
| positions | `GET /open-api/od/v1/positions` (`:184`) `tradingSymbol, exchange, product, netQuantity, dayBuyPrice/netAveragePrice, daySellPrice, buyQuantity, sellQuantity, ltp, mtm, unrealisedPnl, realizedPnl` (`order_data.py:94-110`); "Failed to retrieve"(EC919)/EC920 = empty book semantics (`:187-204,291-299`) |
| holdings | `GET /open-api/od/v1/holdings/CNC` (`:209`) `nseTradingSymbol, bseTradingSymbol, dpQuantity/totalQuantity, t1Quantity, ltp, averageTradedPrice/investedPrice, isin` (`order_data.py:113-136`) |
| funds | `GET /open-api/od/v1/limits/` (`funds.py:61`) `result[0].{tradingLimit->availablecash, collateralMargin->collateral, utilizedMargin->utiliseddebits}`; m2mrealized = Σ positions `realizedPnl`; unrealized 0 (`:86-103`) |
| margin calc | not supported (`margin_api.py:20`) |
| quotes/depth | **WebSocket only**: subscribe tick (`t`) or depth (`d`), sleep 2.0 s, read cache, unsubscribe (`data.py:261-360,574-676`); multiquote batch 100 with 0.05 s polling (`:365-560`) |
| history | `POST https://a3.aliceblueonline.com/open-api/ChartAPIService/api/chart/history` (`data.py:24-25`) JSON `{"token","exchange","from":<ms>,"to":<ms>,"resolution":"1"|"D"}` (`:936-942`); response `{"stat","result":[{time,open,high,low,close,volume}]}` (`:968-1004`) |

**Place payload** (`transform_data.py:74-101`): `{"exchange", "instrumentId": str(int(token)), "transactionType": BUY|SELL, "quantity": int, "product": LONGTERM|NRML|INTRADAY, "orderComplexity":"REGULAR", "orderType": MARKET|LIMIT|SL|SLM, "validity":"DAY", "price": str, "slLegPrice":"", "targetLegPrice":"", "slTriggerPrice": str, "disclosedQuantity": str, "marketProtectionPercent":"", "deviceId":"", "trailingSlAmount":"", "apiOrderSource":"", "algoId":"", "orderTag":"openalgo"}`.

**Enums**: product `CNC->LONGTERM, NRML->NRML, MIS->INTRADAY`; reverse adds `MTF/DELIVERY->CNC` (`transform_data.py:13-34`). pricetype `SL-M->SLM` (`:37-68`); legacy codes `MKT/L/SL/SL-M` in normalized rows (`order_data.py:22-24`). Status = `orderStatus.lower()` (`order_data.py:26-28`); order-stream map `new/replaced->open, trigger_pending->trigger pending, complete, rejected, cancelled/canceled` (`aliceblue_order_adapter.py:45-55`). No price scaling (rupee floats).

**Master contract** (`database/master_contract_db.py:143-152`): per-exchange CSVs `https://v2api.aliceblueonline.com/restpy/static/contract_master/V2/{CDS,NFO,NSE,BSE,BFO,BCD,MCX,INDICES}.csv`. Columns: `Exch, Exchange Segment, Symbol, Trading Symbol, Instrument Name, Instrument Type, Option Type, Strike Price, Expiry Date, Lot Size, Tick Size, Token, Group Name, Formatted Ins Name`; INDICES: `symbol, exch, token`. NSE filter `Group Name ∈ {EQ,BE}` (`:219`); BSE requires non-empty Trading Symbol (`:250`); MCX drops `Exchange Segment=="mcx_idx"` (`:473`). Tokens coerced to int strings (`:181-200`). Symbols: equity = `Symbol`; NFO/CDS/BCD FUT = `Trading Symbol + "UT"` (broker symbol ends in `F`), MCX FUT = `Trading Symbol + "FUT"`; OPT = `Symbol+DDMMMYY+strike+CE/PE` (`:286-318,496-506`); BFO = `Formatted Ins Name` without spaces (`:428-434`). Futures flagged by Instrument Type `FUTSTK/FUTIDX/FUTCUR/FUTCOM/SF/IF -> Option Type XX` (`:304-305,373,424-425,492-493,562`). Expiry `DD-MMM-YY`. Indices: exchange/instrumenttype `NSE_INDEX|BSE_INDEX|MCX_INDEX`, alias map `_INDEX_SYMBOL_ALIASES` applied before and after space-strip, unlisted BSE indices prefixed `BSE` (`:605-716`). brexchange = Exch.

**History**: `timeframe_map` `1m,3m,5m,10m,15m,30m,1h -> "1"` (resampled from 1-minute), `D -> "D"` (`data.py:91-100`); timestamps 13-digit ms; daily `to` must be a day boundary (rounded up to next midnight) else "No data available" (`:893-914`); intraday min range 1 h (`:925-933`); indices use exchange `NSE::index`/`BSE::index`/`MCX::index` (`:734-741`); BCD unsupported (`:758-760`); MCX/NFO/CDS only current expiry; response `time` string `YYYY-MM-DD HH:MM:SS` IST -> localized epoch (`:1015-1035`); resample for non-1m intraday (`:1074-1089`).

**Streaming** (`api/alicebluewebsocket.py`, `streaming/aliceblue_adapter.py`, `aliceblue_mapping.py`)
- Session prep (REST): `POST /open-api/od/v1/profile/invalidateWsSess` then `POST /open-api/od/v1/profile/createWsSess`, body `{"source":"API","userId":<UCC>}`, Bearer header (`alicebluewebsocket.py:91-156`).
- URL `wss://ws1.aliceblueonline.com/NorenWS/` (alt `ws2`) (`:26-27`). Connect frame: `{"susertoken": SHA256(SHA256(JWT)), "t":"c", "actid": "<UCC>_API", "uid": "<UCC>_API", "source":"API"}` (`:81-82,295-301`); ack `t=="ck"`.
- Subscribe `{"t":"t"|"d", "k":"NSE|2885#NFO|54957"}`; unsubscribe `{"t":"u","k":...}` (`:851-865`, `aliceblue_mapping.py:264-276`). Heartbeat `{"k":"","t":"h"}` every 30 s (docs 50 s) (`aliceblue_adapter.py:369-394`).
- Frames JSON: `t ∈ tk (tick ack, full snapshot) | tf (tick feed, deltas) | dk | df (depth)`; keys `e, tk, ts, lp, o, h, l, c, v, pc, cv, ap, ft, ltq, tbq, tsq, oi, poi, toi, bp1..5/bq1..5/bo1..5, sp1..5/sq1..5/so1..5` (`:476-556,616-741`). `tf` is a delta merged onto cached `tk`. Modes: OA 1/2 -> `t`, 3 -> `d` (`aliceblue_adapter.py:586`); 5-level depth only.
- Order stream: `GET https://a3.aliceblueonline.com/open-api/order-notify/ws/createWsToken` (Bearer) -> `result[0].orderToken`; connect `wss://a3.aliceblueonline.com/open-api/order-notify/websocket`, send `{"orderToken","userId":<UCC>}`, heartbeat `{"heartbeat":"h","userId"}` every 55 s; frames `t=="om"` with Noren fields `norenordno, tsym, exch, trantype, qty, prc, trgprc, prctyp(MKT/L/SL/SL-M), pcode, status, fillshares, avgprc, rejreason` (`aliceblue_order_adapter.py:38-206`); stop after 5 token failures (`:66`).

**Quirks**: every quote costs ~2 s wait; outside market hours no tick => failure. Smart-order uses `strict=True` positions read (`order_api.py:302-309`). `ant.aliceblueonline.com` + `/open-api/order-notify/...` returns SPA HTML (use `a3`) (`aliceblue_order_adapter.py:29-37`).

---

### D4. Definedge Securities (`broker/definedge/`)

**Family**: API-key + API-secret + **OTP** (two-step), Noren-lineage REST/WS. UI type `totp`, backend route `/definedge/callback` (`BrokerSelect.tsx:29,144`). Template: `broker/shoonya/`/`flattrade` (Noren WS) + custom REST ("Integrate" API).

**Auth** (`api/auth_api.py`, `brlogin.py:820-911`)
- Env: `BROKER_API_KEY` = api_token, `BROKER_API_SECRET` = api_secret (`auth_api.py:31-32`). (Skill table claims a 3-part env composite; code uses the plain key — the 3-part `:::` string is the STORED auth token, below.)
- Step 1 (GET on callback route): `GET https://signin.definedgesecurities.com/auth/realms/debroking/dsbpkc/login/{api_token}` header `api_secret: <secret>` -> `{"otp_token", ...}`; stored in Flask session, redirect to `/broker/definedge/totp` (`auth_api.py:62-87`, `brlogin.py:821-852`). POST with `action=resend` repeats step 1 (`brlogin.py:857-875`).
- Step 2 (POST form field `otp`): `ac = SHA256(otp_token + otp + api_secret)`; `POST {SESSION_URL}/token` JSON `{"otp_token","otp","ac"}` (`auth_api.py:103-113`). Response `{"stat":"Ok","api_session_key","susertoken","uid"|"uccid"}` (`:40-46`).
- Stored auth token = **`f"{api_session_key}:::{susertoken}:::{api_token}"`**; feed_token = `susertoken`; user_id = `uid` (`:52-55`). Every API call does `api_session_key, susertoken, api_token = auth.split(":::")` (`order_api.py:33`).

**Base URLs / headers** (`api/baseurl.py:4-10`): trading `https://integrate.definedgesecurities.com/dart/v1`; data `https://data.definedgesecurities.com/sds`; headers `Authorization: <api_session_key>` (raw, no Bearer) (+ Content-Type JSON) (`order_api.py:40`). Rate limiter: 0.1 s min interval, 429 retry ×3 with backoff 1/2/4 s (`api/rate_limiter.py:31-34`). SEBI `algo_id` required on orders: env `DEFINEDGE_ALGO_ID` else `99999` (NSE) / `9999999999999999` (BSE/BFO/BCD) (`transform_data.py:10-19`).

**REST endpoints**
| Op | Method/path | Body / consumed |
|---|---|---|
| place | `POST /placeorder` (`order_api.py:254`) `{"tradingsymbol", "exchange", "quantity", "price" ("0" for MARKET/SL-MARKET), "price_type", "product_type", "order_type": BUY|SELL, "algo_id", ["trigger_price"], ["disclosed_quantity"]}` (`transform_data.py:33-51`) | `{"status":"SUCCESS","order_id"}` or `stat/norenordno` (`:277-278`); HTTP 200 w/o id coerced to 400 (`:290-296`) |
| modify | `POST /modify` (`:724`) `{"order_id","tradingsymbol","exchange","quantity","price","price_type","product_type","order_type","validity":"DAY",["trigger_price"],["disclosed_quantity"]}` (`transform_data.py:84-153`) |
| cancel | `GET /cancel/{orderid}` (`:637`) -> `{"status":"SUCCESS","order_id","request_time"}` |
| cancel-all | statuses `open,new,replaced,trigger pending,pending,open pending,trigger_pending` (`:798-803`) |
| orderbook/tradebook/positions/holdings | `GET /orders`, `/trades`, `/positions`, `/holdings` (`:70-95`); envelopes `{"status":"SUCCESS","orders"|"trades"|"positions"|"data":[...]}` (`order_data.py:21,198,313,400`) |
| funds | `GET /limits` (`funds.py:34`) `cash->availablecash, brokerCollateralAmount->collateral, currentUnrealizedMtom (or Σ segment MTOM fields), currentRealizedPNL (or Σ segments), marginUsed->utiliseddebits`; status "SUCCESS" or "200" (`:50-133`) |
| margin | `POST /spancalculator` `{"positions":[{product_type, exchange, symbol_name, tradingsymbol, open_buy_qty, open_sell_qty}]}` -> `span`, `exposure` (`margin_api.py:12,57-60`, `margin_data.py:61-66,126-128`) |
| quotes | `GET /quotes/{exchange}/{token}` (`data.py:89-90`; indices -> NSE/BSE/MCX) fields `best_bid_price1, best_ask_price1, day_open, day_high, day_low, ltp, volume` — no prev_close (day_open used) and no OI; OI back-filled from 1-minute history for derivatives (`:148-187,311-331`). Multiquote batch 20, concurrent (async httpx or thread pool) (`:339-641`) |
| security info | `GET /securityinfo/{exchange}/{token}` (`:205`) |
| history | `GET {DATA_URL}/history/{segment}/{token}/{minute|day}/{ddMMyyyyHHmm}/{ddMMyyyyHHmm}` (`:755-770`); CSV no header: `datetime,open,high,low,close,volume[,oi]` (tick: `utc,ltp,ltq,oi`) (`:852-883`) |

**Enums**: pricetype `MARKET, LIMIT, SL->SL-LIMIT, SL-M->SL-MARKET` (`transform_data.py:214-220`); product `MIS->INTRADAY, CNC->CNC, NRML->NORMAL` (`:191-196`), reverse `NORMAL` -> CNC on NSE/BSE else NRML (`order_data.py:44-57,227-233`). Exchanges pass-through NSE/BSE/NFO/BFO/CDS/MCX. Status (`order_status`): `COMPLETE/EXECUTED->complete, REJECTED->rejected, OPEN/NEW/REPLACED/PENDING/...->open, CANCELED/CANCELLED->cancelled` (`order_data.py:141-161`). Action field is `order_type` (BUY/SELL). Order stream maps Noren codes `prctyp LMT/MKT/SL-LMT/SL-MKT`, `prd C/M/I -> CNC/NRML/MIS` (`definedge_order_adapter.py:52-57`).

**Master contract** (`database/master_contract_db.py`): `GET https://app.definedgesecurities.com/public/allmaster.zip` -> `allmaster.csv` headerless (`:78-105`). Columns assigned: `Exchange, Token, Name, TradingSymbol, InstrumentType, Expiry(DDMMYYYY), TickSize(paise), LotSize, OptionType, StrikePrice(paise), Col10, Col11, Col12, PriceFactor, Col14` (`:315-331`); strike/100, tick/100 (`:346-352`). NSE keeps only `EQ, BE, IDX, INDEX`; NSE equity symbol strips `-(EQ|BE|MF|SG)$`, instrumenttype EQ, strike 1.0 (`:362-416`); indices `IDX/INDEX -> NSE_INDEX/BSE_INDEX/MCX_INDEX`, instrumenttype `IDX`, symbol upper no spaces/hyphens + rename map (`:427-532`); derivatives symbol = `Name + DDMMMYY + [strike] + FUT|CE|PE` for NFO/BFO/CDS/MCX, expiry reformatted `DD-MMM-YY` (`:543-660`). brsymbol = TradingSymbol, brexchange = Exchange.

**History**: `timeframe_map` `1m,5m,15m,30m,1h -> "minute"` (resampled from 1-min aligned to session open 09:15/09:00), `D -> "day"` (`data.py:265-274,960-1002`); chunk days `1m:30, 5m:90, 15m:150, 30m:180, 1h:180, D:365` (`:716-725`); chunk end at end-of-day; API ignores `to` so result clipped client-side (`:1059-1067`); datetime zfilled to 12 chars and parsed `%d%m%Y%H%M`; daily -> naive midnight epoch, intraday -> IST localized -> UTC epoch (`:1079-1097`); retries per chunk.

**Streaming** (`streaming/definedge_websocket.py`, `definedge_order_adapter.py`)
- URL `wss://trade.definedgesecurities.com/NorenWSTRTP/` (`:63`). Connect `{"t":"c","uid":<uid>,"actid":<uid>,"source":"TRTP","susertoken":<feed_token>}` -> ack `{"t":"ck","s":"Ok"}` (`:364-375,383-391`).
- Subscribe `{"t":"t","k":"NSE|22#BSE|508123"}` (touchline) / `{"t":"d","k":...}` (depth); unsub `t:"u"` / `t:"ud"` (`:530-625`). Acks `tk/dk`, feeds `tf/df`, `uk/udk` (`:317-340`). Field keys as AliceBlue Noren (`lp, ft, v, o, h, l, c, pc, ap, oi, poi, toi, bp1/bq1/sp1/sq1 ...`) (`definedge_adapter.py:1039-1063`). Heartbeat `{"t":"h"}` every 30 s, stall timeouts 120 s / data-silence watchdog; WS ping 30 s (`:81-105,484-528`). Modes: 1,2 -> touchline, 3 -> depth (5 levels).
- Order stream: same URL/connect, then `{"t":"o","actid":<uid>}`; feed `t=="om"` Noren fields; heartbeat `{"t":"h"}` 30 s (`definedge_order_adapter.py:9-15,74-137`).

**Quirks**: multiquote batch 20 (`cross-broker-reference.md:37`); quotes lack OI and prev_close; limits realized PnL may be absolute value (`funds.py:83-86`); `symbol_map.py` helper adds/strips `-EQ` (`mapping/symbol_map.py:6-30`).

---

### D5. Pocketful (`broker/pocketful/`)

**Family**: true OAuth2 authorization-code with Basic client auth. Template: `broker/upstox/` (OAuth) + Angel-style binary WS for quotes (no REST quotes).

**Auth** (`api/auth_api.py`)
- `BROKER_API_KEY` = client_id, `BROKER_API_SECRET` = client_secret (`:40-41`). Login URL (client): `https://trade.pocketful.in/oauth2/auth?client_id=&redirect_uri=&response_type=code&scope=orders%20holdings&state=<random16>` (state saved in localStorage) (`BrokerSelect.tsx:217-221`). Callback params `code`, `state`, `error`, `error_description` (`brlogin.py:792-818`).
- Exchange: `POST https://trade.pocketful.in/oauth2/token`, headers `Authorization: Basic base64(client_id:client_secret)`, `Content-Type: application/x-www-form-urlencoded`, form `grant_type=authorization_code&code=&redirect_uri=<REDIRECT_URL env>` (`:51-72`). Response `access_token` (refresh token not used). Then `GET /api/v1/user/trading_info` Bearer -> `data.client_id` (`:96-124`). Returns `(access_token, None, client_id, error)`; `client_id` persisted as `Auth.user_id` (`order_api.py:152-176`).

**Base URL / headers**: `https://trade.pocketful.in` (`order_api.py:24`); `Authorization: Bearer <access_token>` + JSON (`:37`). `client_id` is a REQUIRED query/body param on most calls.

**REST endpoints**
| Op | Method/path | Body / consumed |
|---|---|---|
| place | `POST /api/v1/orders` (`order_api.py:25,659-680`) payload below | `data.oms_order_id` |
| modify | `PUT /api/v1/orders` (`:1015-1023`) same payload + `oms_order_id` (`transform_data.py:164-182`) | `data.oms_order_id` |
| cancel | `DELETE /api/v1/orders/{orderid}?client_id=` (`:935`) -> `data.oms_order_id` (`:938-963`) |
| cancel-all | `GET /api/v1/orders?client_id=&type=pending`; statuses incl. OPEN/PENDING/TRIGGER PENDING/NEW/RECEIVED/PLACED/VALIDATED/ACCEPTED/PENDING_n or `mode=="NEW"` (`:1074-1151`) |
| orderbook | `GET /api/v1/orders?client_id=&type=completed` + `type=pending`, merged `data.orders` (`:93-132,179-220`) |
| tradebook | `GET /api/v1/trades?client_id=` -> `data.trades` (`:243-276`) fields `tradingsymbol, exchange, product, transaction_type, fill_quantity, avg_price, trade_id, order_id, fill_timestamp` (`order_data.py:360-368`) |
| positions | `GET /api/v1/positions?client_id=&type=live` -> `data` or `data.positions` (`:317-369`); qty `quantity`/`net_quantity`, `trading_symbol` |
| holdings | `GET /api/v1/holdings?client_id=` -> `data.holdings` (`:407-468`) |
| funds | `GET /api/v2/funds/view?client_id=&type=all` (`funds.py:24,59`) -> `data.values` = `[label, value]` pairs: `Available Margin->availablecash, Margin Used->utiliseddebits, Total Pledge Collateral->collateral, unrealized_mtm/realized_mtm` (`:98-131`) |
| margin calc | not supported (`margin_api.py:20`) |
| quotes/depth | **WebSocket only** (`data.py:213-366,543-708`): detailed marketdata for quotes, `full_snapquote` for depth; prices ×100 -> /100 |
| history | NOT SUPPORTED (`data.py:368-397`, `timeframe_map={}` `:194`) |

**Place payload** (`transform_data.py:108-124`): `{"exchange": OA code, "instrument_token": token, "client_id", "order_type": MARKET|LIMIT|SL|SLM, "amo": false, "price": float, "quantity": int, "disclosed_quantity": int, "validity":"DAY", "product": CNC|NRML|MIS, "order_side": BUY|SELL, "device":"WEB", "user_order_id": 1, "trigger_price": float, "execution_type":"REGULAR"}`. MARKET orders are converted to LIMIT at an MPP-protected price from live LTP (`utils/mpp_slab.calculate_protected_price`; `transform_data.py:42-105`).

**Enums**: pricetype `SL-M->SLM` else identical (`transform_data.py:185-195`); product identical (`:198-216`). Status (`order_status` substring): `CANCEL_CONFIRMED->cancelled, COMPLETE->complete, REJECTED->rejected, TRIGGER PENDING->trigger pending, OPEN|PENDING|AMO_SUBMIT|MODIFY->open, CANCEL->cancelled` (`order_data.py:189-212`); orderid `oms_order_id` (`:237`). Exchange codes for WS: `NSE 1, NFO 2, CDS 3, MCX 4, BSE 6, BFO 7` (`data.py:190`, `streaming/pocketful_mapping.py:9-18`).

**Master contract** (`database/master_contract_db.py`): ZIP `https://trade.pocketful.in/api/v1/contract/Compact?info=download&exchanges=NSE,NFO,BSE,BFO,MCX` -> `{NSE,BSE,NFO,BFO,MCX}CompactScrip.csv` (`:150-161`). Columns (lowercased): `trading_symbol, company_name, exchange, exchange_token, lot_size, tick_size, instrument_name, option_type, strike, expiry, segment` (`:191-220`). NSE filter `instrument_name=="EQ"`, symbol strips `-EQ` (`:207-210`); BSE symbol strips `-.*$`, `segment=="IDX"` -> BSE_INDEX, `SNSX50->SENSEX50` (`:241-255`); NFO/BFO/MCX: `option_type XX` (BFO SF/IF, MCX FUTCOM/FUTIDX set to XX) -> `company_name|base + DDMMMYY + FUT`, options `+strike+CE/PE`; MCX base = leading letters of trading_symbol, drops `instrument_name=="COM"` (`:260-442`); NSE indices from `segment=="INDICES"` with map `Nifty 50->NIFTY, Nifty Bank->BANKNIFTY, India VIX->INDIAVIX, Nifty Fin Service->FINNIFTY, NIFTY MID SELECT->MIDCPNIFTY, Nifty Next 50->NIFTYNXT50` (`:457-473`). token = exchange_token, brsymbol = trading_symbol, expiry `DD-MMM-YY`.

**Streaming** (`streaming/pocketful_adapter.py`, `api/pocketfulwebsocket.py`, `api/packet_decoder.py`)
- URL `wss://trade.pocketful.in/ws/v1/feeds?login_id=<client_id>&access_token=<token>` (`pocketful_adapter.py:139`, `pocketfulwebsocket.py:416-418`).
- Subscribe `{"a":"subscribe","v":[[exchange_code, token_int]],"m":"marketdata"|"compact_marketdata"|"full_snapquote"}`; unsubscribe `a:"unsubscribe"` (`pocketful_adapter.py:353-383`). OA mode -> Pocketful: `1(LTP)->compact(2), 2(QUOTE)->detailed(1), 3(DEPTH)->snapquote(4)` (`:219-224`). Heartbeat `{"a":"h"}` every ~15 s (`pocketfulwebsocket.py:234-242`).
- Binary frames **big-endian**, byte0 = mode (`1 detailed, 2 compact, 4 snapquote, 50 order, 51 trade`) (`pocketfulwebsocket.py:139-185`); JSON frames also accepted (`packet_decoder.py:25-72`). Prices integer paise -> /100.
  - Detailed (≥102 B): `mode i8@0, exch i8@1, token u32@2, ltp@6, ltt@10, ltq@14, volume@18, bid@22, bid_qty@26, ask@30, ask_qty@34, tbq u64@38, tsq u64@46, atp@54, exch_ts@58, open@62, high@66, low@70, close@74, 52h@78, 52l@82, lowDPR@86, highDPR@90, oi@94, init_oi@98` (`packet_decoder.py:160-186`).
  - Compact (≥42 B): `mode@0, exch@1, token@2, ltp@6, change@10, ltt@14, lowDPR@18, highDPR@22, oi@26, init_oi@30, bid@34, ask@38` (`:252-265`).
  - Snapquote (≥166 B): `mode@0, exch@1, token@2, buyers[5]u32@6, bidPrices[5]@26, bidQtys[5]@46, sellers[5]@66, askPrices[5]@86, askQtys[5]@106, atp@126, open@130, high@134, low@138, close@142, totalBuyQty u64@146, totalSellQty u64@154, volume u32@162` (`:83-137`). Snapquote has no LTP — adapter/data use `averageTradePrice` as ltp (`pocketful_adapter.py:586`, `data.py:689`).
  - Order/trade updates (mode 50/51) are UTF-8 JSON after a 5-byte prefix (`:279-302`).
- Normalization: detailed -> ltp/open/high/low/close/volume/ltq/avg/tbq/tsq/oi; compact -> ltp/change/oi/bid_price/ask_price (`pocketful_adapter.py:493-553`). Depth 5 levels.

**Quirks**: `client_id` cached in `Auth.user_id`, else re-fetched from `trading_info` (`order_api.py:137-176`). Quote fetch waits for a packet matching the token (`data.py:102-118,300-320`); multiquote batch 50 (`:740`). Holdings/positions `trading_symbol` -> OA via `get_oa_symbol` (`order_data.py:36-70`).

---

### D6. HDFC Sky (`broker/hdfcsky/`)

**Family**: redirect + request-token exchange with `apiSecret` body (no hash). Template: `broker/arrow/` (file layout, smart-order and WS client are near copies) / zerodha. HDFC Sky and HDFC Securities (InvestRight) share gateway design and the protobuf `GenericDTO` market feed but are separate hosts/apps/credentials; nothing is shared in code (`broker/hdfcsecurities/api/baseurl.py:13-20`).

**Auth** (`api/auth_api.py`, `api/baseurl.py`)
- Env `BROKER_API_KEY` = api_key, `BROKER_API_SECRET` = api secret (`auth_api.py:33-34`); optional `BROKER_CLIENT_ID` fallback (`baseurl.py:116-117`), `HDFCSKY_UAT=1` switches host to `https://uat-developer.hdfcsky.com` (`:32,47-51`).
- Login URL: `https://developer.hdfcsky.com/oapi/v1/login?api_key=${broker_api_key}` (`BrokerSelect.tsx:207-210`). Callback token param: any of `request_token|requestToken|request-token|code` (`brlogin.py:1002-1014`).
- Exchange: `POST https://developer.hdfcsky.com/oapi/v1/access-token?api_key=<key>&request_token=<tok>` body `{"apiSecret": <secret>}`, headers User-Agent + JSON, no Authorization (`auth_api.py:41-53`). Response `{"accessToken"}` (or `data.accessToken`) (`:63-71`). Stored auth = accessToken JWT; `client_id` = JWT `sub` claim (`baseurl.py:95-113`) — not stored separately.

**Base URL / headers** (`baseurl.py:29-86,120-127`): `https://developer.hdfcsky.com`; headers `Authorization: <access_token>` (**no Bearer**), `User-Agent` mandatory (Chrome UA string), `Accept: application/json`; query params `api_key` on every call and `client_id` on account calls (`base_params`).

**REST endpoints** (`api/order_api.py:5-12`)
| Op | Method/path | Notes |
|---|---|---|
| place | `POST /oapi/v1/orders?api_key=` (`:206-211`) -> `data.oms_order_id`; HTTP 200 error payload coerced to 400 (`:222-229`) |
| modify | `PUT /oapi/v1/orders?api_key=` same body + `oms_order_id` (`:375-377`, `transform_data.py:319-332`) |
| cancel | `DELETE /oapi/v1/orders/{oms_order_id}?api_key=&client_id=&execution_type=REGULAR` (`:350-355`) |
| cancel-all | `GET /oapi/v1/orders?type=pending`, `order_status ∈ CANCELLABLE_STATUSES` (`:395-409`) |
| orderbook | `GET /oapi/v1/orders?type=pending` + `type=completed`, merged `data.orders` (`:73-94`) |
| tradebook | `GET /oapi/v1/trades` -> `data.trades` (`:97-98`) |
| positions | `GET /oapi/v1/positions?type=historical` (netwise) -> flat `data[]` (`:101-104,160-169`); fields `trading_symbol, exchange, product, net_quantity` |
| holdings | `GET /oapi/v1/holdings` -> `data.holdings` (`:107-108`, `order_data.py:10`) |
| funds | `GET /oapi/v1/funds/view?api_key=&client_id=&type=all` (`funds.py:71-75`); `data.values` label/value pairs `Available Margin->availablecash, Margin Used->utiliseddebits, Pledge Benefit->collateral`, top-level `realized_mtm/unrealized_mtm` preferred (`:28-34,116-130`); V2 endpoint is 404 |
| margin | `POST /oapi/v1/margin?api_key=` `{"data":[{segment: Capital|FutOpt|Currency|Commodities, series, exchange, side, mode:"NEW", symbol: brsymbol, underlying: int(price), token, quantity, price, product: "0"(NRML)|"1"(CNC)|"2"(MIS)}]}` (`margin_api.py:166-173`, `margin_data.py:23-30,70-86`, `transform_data.py:170`); result `span`, `exposure_margin`, `span_spread_margin`... (`margin_data.py:35-45,99-128`) |
| LTP | `PUT /oapi/v1/fetch-ltp?api_key=` `{"data":[{"exchange","token"}]}` -> `data[].{exchange, token, ltp, prev_close}` (`data.py:154-205`); indices use `NSE_INDEX`/`BSE_INDEX` codes (`transform_data.py:90-101`); **max 10 instruments per request** (`data.py:327-332`) |
| quotes | LTP+prev_close from fetch-ltp, OHLCV from latest DAY candle; bid/ask/oi = 0 (`data.py:216-266`) |
| depth | REST scalars + WS snapshot (`ALL` subscribe) for 5-level book, ltq, tbq/tsq, oi (`:289-321,436-535`) |
| history | `GET /oapi/charts-api/charts/v1/fetch-candle?api_key=&symbol=&exchange=&chartType=MINUTE|DAY&seriesType=&start=YYYY-MM-DD&end=` (`:563-593`); rows `[open, high, low, close, volume, oi, "DD-MM-YYYY[ HH:MM]", cum_vol]` unsorted (`:566-573`) |

**Place payload** (`transform_data.py:283-306`): `{"exchange": NSE|BSE|NFO|BFO|CDS|MCX, "instrument_token": token, "client_id", "order_type": MARKET|LIMIT|SL|SLM, "order_side": BUY|SELL, "product": CNC|NRML|MIS, "quantity": int, "price": float, "trigger_price": float, "disclosed_quantity": int, "validity":"DAY", "device":"WEB", "execution_type":"REGULAR", "amo": false, "user_order_id": <ms epoch % 1e9>, ["tags":[strategy[:32]]]}`. MARKET -> LIMIT and SL-M -> SL with MPP buffer because merchant keys may only place limit orders (`:212-265`).

**Enums**: pricetype `SL-M->SLM` (`transform_data.py:147-161`); product identical (+`MTF->CNC` reverse) (`:165-166`); REST exchange: `*_INDEX -> NSE/BSE`, response `NCD->CDS` (`:44-66`); WS prefixes `CDS->NCD`, `NSE_INDEX/BSE_INDEX` kept (`:71-80`). Status map (GenericDTO `Status` enum): `COMPLETE->complete; REJECTED, MODIFY_REJECTED, CANCEL_REJECTED, BRACKET_ORDER_REJECTED->rejected; CANCELLED, CANCEL_CONFIRMED, BATCH_CANCEL_CONFIRMED, AMO_CANCEL_CONFIRMED, BRACKET_ORDER_CANCELLED->cancelled; SL_TRIGGER_CONFIRMED, TRIGGER_PENDING->trigger pending; ACCEPTED, CONFIRMED, PENDING, MODIFY_*, CANCEL_ACCEPTED/PENDING, PARTIAL_TRADE, AMO_*, UNACCEPTED, EXCHANGE_RESPONSE_PENDING, RRM_PENDING_AT_EXCHANGE, RMS_VALIDATION_COMPLETED->open` (`order_data.py:27-58`). Prices rupees (no scaling).

**Master contract** (`database/master_contract_db.py`): ZIP `https://hdfcsky.com/api/v1/contract/Compact?info=download` -> `CompactScrip.csv` (~182k rows, public) (`:6-7,116-141`). Columns `exchange_token, trading_symbol, company_name, close_price, expiry (DD-Mon-YYYY), strike, tick_size, lot_size, instrument_name, option_type, segment, exchange` (`:9-12`). Exchange already OA codes; indices = NSE rows `segment=="INDICES"`, BSE rows `segment=="IDX"` -> `NSE_INDEX/BSE_INDEX` (`:201,352-359`). NSE equity strips `-EQ`; BSE cash strips `-<group>` (`:414-427`); FUT/OPT symbol = underlying (recovered by stripping broker suffix incl. weekly `YY+M+DD` and `O/N/D` months) + `DDMMMYY` + `FUT|strike+CE/PE` (`:149-193,428-431`); futures flagged by expiry + trading_symbol ends `FUT` (`:376`). Index renames `_NSE_INDEX_MAP`/`_BSE_INDEX_MAP` (`:215-324`). instrumenttype EQ/FUT/CE/PE; non-derivative MCX rows dropped; duplicates on (symbol, exchange) dropped (`:469-478`). token = exchange_token, brsymbol = trading_symbol, brexchange = exchange.

**History**: `_interval_spec` `1m:(MINUTE,None), 3m/5m/10m/15m/30m:(MINUTE,resample), 1h:(MINUTE,"60min"), D:(DAY,None), W:(DAY,"W-MON"), M:(DAY,"MS")` (`data.py:121-134`); chunk 2000 days for DAY, 31 for MINUTE (`:675`); 0.15 s between chunks, 429 retry 0.5/1/2 s ×4, chunk-level retry ×4 (`:333-341`); timestamps day-first IST strings; DAY -> normalize + 5:30 shift, intraday -> IST->UTC epoch (`:779-795`); candle `seriesType` derived from row (EQ/FUTIDX/OPTSTK...; index underlyings `NFO_INDEX_UNDERLYINGS` / `BFO_INDEX_UNDERLYINGS` `transform_data.py:140-141`); index chart symbol resolved from candidates and cached (`:343-346,538-561`).

**Streaming** (`streaming/hdfcsky_websocket.py`, `hdfcsky_mapping.py`, `api/baseurl.py:54-65`)
- URL `wss://developer.hdfcsky.com/wsapi/v1/session?token=<access_token>&api_key=<key>` (query auth only; header auth 401s).
- Subscribe JSON text: `{"heart_beat": false, "subscribe": [{"scripId": "NSE_2885", "type": "LTP"|"ALL"|"GREEK"}], "unSubscribe": [...]}` (capital S) (`:15-19,330-345,350-367`); scripId = `<PREFIX>_<token>` with prefixes `NSE, BSE, NFO, BFO, NCD(CDS), MCX, NSE_INDEX, BSE_INDEX` (`transform_data.py:71-116`). OA mode 1->LTP, 2/3->ALL (`hdfcsky_mapping.py:29-36`). ≤100 scrips per frame, ≤300 instruments/connection, 3 connections/key (`:85-86`, `mapping.py:43-46`).
- Heartbeat `{"heart_beat": true}` JSON (`:519`); reconnect 5 s ×1.5^n max 60 s, 50 tries; token refresh on auth failure ≤3.
- Inbound: binary protobuf `GenericDTOList` (fallback bare `GenericDTO`) (`:536-566`; messages `GenericDTO, IndexData, MBPData, MarketDepthDTO(List), GreekData` in `hdfcsky_market_pb2.py`). Packet fields (`:568-650`): `packetType` (HEARTBEAT skipped), `instrumentId`, `packetTimestamp` (ms); index: `indexData.{indexValue, openingIndex, highIndexValue, lowIndexValue, closingIndex, packetTimeStamp}`; mbp: `mbpData.{lastTradedPrice, openPrice, highPrice, lowPrice, closingPrice, volumeTradedToday, lastTradeQuantity, averageTradePrice, totalBuyQuantity, totalSellQuantity, oi, lowerCircuitLimit, upperCircuitLimit, lastTradeTime, marketDepthDTOList.marketDepthDTO[]{price, quantity, numberOfOrders, buyFlag}}`; greek: `greekData.{delta,gamma,vega,theta,rho}`. Depth 5. No order-update stream.

**Quirks**: no REST full-quote/depth endpoint; LTP batch cap 10 (400 "maximum 10 items allowed"); chart API 429 `merchantKeyRateLimit`; raw httpx errors must not be logged (URL carries api_key) (`data.py:185-186,609-610`); `_user_order_id` numeric; funds 401 shape `{"error":"invalid credentials"}` (`funds.py:90-99`).

---

### D7. HDFC Securities InvestRight (`broker/hdfcsecurities/`)

**Family**: identical redirect/apiSecret flow and protobuf feed to HDFC Sky, but a different product/host with a three-field instrument address and **no history/quote REST**. Template: `broker/hdfcsky/` (structure) — differences called out below.

**Auth** (`api/auth_api.py`, `api/baseurl.py`): login `https://developer.hdfcsec.com/oapi/v1/login?api_key=` (`BrokerSelect.tsx:200-204`); callback param variants as Sky (`brlogin.py:1016-1028`); exchange `POST https://developer.hdfcsec.com/oapi/v1/access-token?api_key=&request_token=` body `{"apiSecret"}` -> `accessToken` (`auth_api.py:41-74`). Headers `Authorization: <token>` (no Bearer), mandatory `User-Agent`, `Accept` (`baseurl.py:58-72`); query `api_key` only — **no client_id anywhere** (`:79-86`). No UAT switch.

**REST endpoints** (`api/order_api.py:5-11`)
| Op | Method/path | Notes |
|---|---|---|
| place | `POST /oapi/v1/orders/regular?api_key=` (`:284-288`) -> `data.order_id` (`:303-304`) |
| modify | `PUT /oapi/v1/orders/regular/{order_id}` body `{"quantity","order_type","validity":"DAY","disclosed_quantity","product","price","trigger_price","amo":false}` (`:487`, `transform_data.py:416-425`) |
| cancel | `DELETE /oapi/v1/orders/regular/{order_id}` -> `data.order_id` (`:465-470`) |
| cancel-all | `GET /oapi/v1/orders`, `is_cancellable` via normalized `status` (`:502-526`, `order_data.py:76-102`) |
| orderbook / tradebook | `GET /oapi/v1/orders`, `GET /oapi/v1/trades` -> `data[]` (`:81-86`; `order_data.py:7-8`) |
| positions | `GET /oapi/v1/portfolio/cumulative-positions` -> `data.net[]` (`overall_positions` 404s) (`:16-20,151-160,229-242`); rows carry no LTP, so LTPs are batch-fetched via fetch-ltp and merged (`:89-148`); qty `net_qty`; address via `(exchange, instrument_segment, security_id)` |
| holdings | `GET /oapi/v1/portfolio/holdings` -> `data[]` (`:165-166`) |
| funds | `GET /oapi/v1/user/margins?api_key=` -> `data.equity.{total_available_limit->availablecash, total_utilised_limit->utiliseddebits, totalLimitDetails.pledge_limit->collateral}`; MTM derived from positions (`funds.py:3-24,91-146`) |
| margin calc | none (no `margin_api.py`) |
| LTP | `PUT /oapi/v1/fetch-ltp?api_key=` `{"data":[{"exchange","token"}]}` -> `data[].{exchange (may be "" for BFO/CDS), token, ltp, prev_close}`; exchange code = OA segment code incl. NFO/BFO/CDS/NSE_INDEX (`data.py:97-170`, `transform_data.py:101-111`); batch 10, 0.15 s (`:94-95`) |
| quotes/depth | LTP + prev_close from fetch-ltp; OHLC/volume/OI/best bid-ask/5-level book from a WS `ALL` snapshot (`data.py:183-373`) |
| history | NOT SUPPORTED — raises explicit error, `timeframe_map={}` (`data.py:62-68,453-472`) |

**Place payload** (`transform_data.py:379-407`): `{"exchange": NSE|BSE|MCX (parent; NFO->NSE, BFO->BSE, CDS->NSE), "security_id": brsymbol (alphanumeric broker code e.g. NIFTYEQEQNR), "instrument_segment": EQUITY|FUTIDX|OPTIDX|FUTSTK|OPTSTK|FUTCUR|OPTCUR|FUTCOM|OPTFUT, "transaction_type": BUY|SELL, "product": DELIVERY|INTRADAY (cash) | OVERNIGHT|INTRADAY (derivatives), "order_type": MARKET|LIMIT|SL|SL-M, "quantity", "price", "trigger_price", "disclosed_quantity", "validity":"DAY", "amo": false, "external_reference_number": <13-digit ms counter>, [derivatives: "expiry_date": YYYYMMDD, "underlying_symbol": underlying cash row's security_id, "option_type": CE|PE, "strike_price": float]}` (`:195-232,338-376`).

**Enums**: pricetype identical incl. `SL-M` (reverse accepts `SL-L`, `SLM`) (`transform_data.py:240-254`); product `CNC/NRML->DELIVERY` on cash, `->OVERNIGHT` on derivatives, `MIS->INTRADAY`; reverse `DELIVERY/MTF/COLL-SELL/ENCASH->CNC, OVERNIGHT->NRML, INTRADAY/COVER->MIS` (`:258-269`); `(exchange, instrument_segment)->OA` table (`:52-71`); `option_type` returns as `Call/Put` (`:307-313`); side title-cased `Buy` (`:302-304`). Status (normalized, separators->spaces): `TRADED/EXECUTED/COMPLETE/COMPLETED/FULLY EXECUTED->complete; REJECTED, CANCEL REJECTED, MODIFY REJECTED->rejected; CANCELLED/CANCELED/CANCEL CONFIRMED->cancelled; TRIGGER PENDING, SL TRIGGER PENDING->trigger pending; PENDING, OPEN, PLACED, ACCEPTED, CONFIRMED, RECEIVED, MODIFIED, MODIFY PENDING, CANCEL PENDING, PARTIALLY TRADED, PARTIAL TRADE, PUT ORDER REQ RECEIVED, VALIDATION PENDING, AFTER MARKET ORDER REQ RECEIVED, OPEN PENDING, TRANSIT->open` (`order_data.py:43-73`). Rupee prices.

**Master contract** (`database/master_contract_db.py`): plain CSV `GET https://developer.hdfcsec.com/oapi/v1/security-master` (public) (`:8-10,163-183`). Columns `exchange, security_id, instrument_segment, expiry_date (YYYY-MM-DD), strike_price, option_type, lot_size, tick_size, close_price, exch_security_id, symbol_name, underline_symbol, open_price` (`:12-15`). Exchange from `(exchange, instrument_segment)` table (`:190-203`); indices = EQUITY rows with NSE token in 26000-26999 or BSE token ≤999 (`:212-213,320-323`); unmapped segments (COM, UNDCUR) dropped. Symbols: cash = `symbol_name` (BSE blanks borrow NSE's name by shared `security_id`, else `security_id`) (`:350-360`); FUT = `symbol_name+DDMMMYY+FUT`, OPT `+strike+CE/PE` (`:364-365`); indices via `_INDEX_SYMBOL_OVERRIDES` (`:219-232`). **token = `exch_security_id`, brsymbol = `security_id`**, brexchange = exchange; duplicates resolved by `_cash_rank` preference (`:239-262,406-419`). Table replaced atomically (`replace_symtoken_table`, `:140`).

**Streaming** (`streaming/hdfcsecurities_websocket.py`, `hdfcsecurities_mapping.py`): URL `wss://developer.hdfcsec.com/wsapi/v1/session?token=&api_key=` (`baseurl.py:46-55`); same JSON subscribe/unSubscribe/heart_beat frames as Sky (`:15-21`), same prefixes (`transform_data.py:76-85`), ≤100 scrips/frame, **1500 instruments/connection**, 3 connections/key (`mapping.py:43-46`). Protobuf `GenericDTOList`/`GenericDTO` parsing identical, plus dedicated `circuit` (lower/upper limits only) and `oi` packet kinds and a `packetType -> exchange` map (`:635-741`). Adapter publishes every subscribed mode (not just highest) and merges partial packets per key (`hdfcsecurities_adapter.py:35-60,296-357`). No order-update stream.

**Quirks**: instrument addressed by 3 fields; `underlying_symbol` must be the cash row's security_id (`transform_data.py:13-17`); positions nested `data.net`; funds lack MTM; fetch-ltp parent exchange silently drops derivative legs (`transform_data.py:104-110`).

---

### D8. dhan_sandbox — delta vs `broker/dhan/`

Only differences; the Dhan spec covers everything else. Files differing: `api/{auth_api,baseurl,data,funds,margin_api,order_api}.py`, `database/master_contract_db.py`, `mapping/{margin_data,order_data,transform_data}.py`, `plugin.json`, `streaming/*` (diff -rq).

- **Base URL**: `BASE_URL = env DHAN_SANDBOX_BASE_URL or "https://sandbox.dhan.co"` (`dhan_sandbox/api/baseurl.py:4`) vs dhan `https://api.dhan.co` (`dhan/api/baseurl.py:4`). Auth helpers keep `DHAN_AUTH_BASE_URL` (`https://auth.dhan.co`) / `DHAN_API_BASE_URL` env overrides (`dhan_sandbox/api/auth_api.py:10-11`).
- **Auth flow**: UI type `totp` but it just hits `/dhan_sandbox/callback` (`BrokerSelect.tsx:28,143`); brlogin calls `authenticate_broker("dhan_sandbox")` (`brlogin.py:561-565`). `authenticate_broker(code)` (`auth_api.py:421-460`): if `code` is empty or `"dhan_sandbox"` -> uses **`BROKER_API_SECRET` directly as the access token** (`get_direct_access_token`, requires len ≥ 50; `:410-418,434-437`); a JWT-like string (>100 chars with '.') is used as-is; otherwise tries consent `consume_consent(tokenId)` then falls back to env token. No TOTP/PIN in the live path (TOTP helper `generate_access_token_with_totp(dhanClientId, pin, totp)` exists at `:78-105` but is unused by the callback). 2-tuple `(auth_token, error)`.
- **Client id**: `BROKER_API_KEY` = `client_id:::api_key` or bare client id; `_get_dhan_client_id()` returns the part before `:::` (`order_api.py` diff; `funds.py`, `data.py` have identical helpers). Place order refuses with HTTP 400 "BROKER_API_KEY must include Dhan client-id for dhan_sandbox" when missing (`order_api.py:218-233`); live dhan instead resolves client id via `verify_api_key`/`get_user_id` from the DB. Sandbox sends `client-id` header on funds/positions requests (`funds.py` diff) in addition to `access-token`.
- **Data**: sandbox has no `/v2/marketfeed/*` quote/depth endpoints; quotes, multiquotes and depth are derived from the last 1-minute candle of `POST /v2/charts/intraday` (`data.py:696-735,947-1000`) and then passed through `_apply_sandbox_mock_realism` (deterministic noise, `:871`); depth is synthetic (`:1000-1010`). Dhan's per-category rate limiter (`DHAN_DATA_INTERVAL 0.2 s`, `DHAN_QUOTE_INTERVAL 1.1 s`) is replaced by a simple 429 retry (0.5/1/2 s) (`data.py` diff). History endpoints are the same (`/v2/charts/historical`, `/v2/charts/intraday`, 5-day intraday chunks) (`data.py:190,414,471`).
- **Orders**: same Dhan v2 endpoints; sandbox `transform_data.py` lacks the live plugin's SL-M -> STOP_LOSS MPP protection logic (`_slm_protected_price`, present only in `dhan/mapping/transform_data.py`); no GTT (`gtt_api.py`, `gtt_data.py` only in dhan). Margin API additionally has `parse_batch_margin_response` / `parse_multi_margin_response` (`margin_api.py` diff).
- **Master contract**: same CSV `https://images.dhan.co/api-data/api-scrip-master.csv` (`dhan_sandbox/database/master_contract_db.py:92`); sandbox stores `tick_size = SEM_TICK_SIZE` raw whereas dhan divides by 100 except INDEX rows (`master_contract_db.py` diff).
- **Exchanges**: `plugin.json` identical set `NSE, BSE, NFO, BFO, CDS, BCD, MCX, NSE_INDEX, BSE_INDEX` (`dhan_sandbox/plugin.json:8`).
- **Streaming**: no real feed. `streaming/__init__.py` exports both `DhanWebSocketAdapter` and `Dhan_sandboxWebSocketAdapter`; the sandbox adapter (`streaming/dhan_sandbox_adapter.py:15-21,123-140`) is a **mock** that generates deterministic synthetic ticks (option premiums estimated from strikes via `fno_search_symbols`) in a thread; `initialize()` needs no auth. The copied `dhan_websocket.py`/`dhan_adapter.py` are unused by the sandbox name resolution.

---

## D. Direct-login brokers (TOTP / password / API-key) and the crypto broker

Scope: mstock, motilal, samco, tradejini, fivepaisa, nubra, indmoney (direct login, no OAuth redirect) and deltaexchange (API key + HMAC-SHA256, `broker_type: crypto`). Angel and kotak are covered in their own spec sheets and are not repeated here. All citations are `file:line` relative to `/Users/openalgo/openalgo-desktop/openalgo/` unless absolute. Source repo was read-only.

Quick index of auth shapes (details in each section):

| Broker | User-supplied login fields | BROKER_API_KEY / BROKER_API_SECRET | Per-call auth header |
|---|---|---|---|
| mstock | password (step 1) then TOTP (step 2) | clientcode / API key (`X-PrivateKey`) | `Authorization: Bearer <jwtToken>` + `X-PrivateKey` (Type B API) |
| motilal | userid, password, TOTP, DOB | API key used in `sha256(password+apikey)` | `Authorization: <AuthToken>` + 20 fixed headers; stored `"<AuthToken>:::<accesstoken>"` |
| samco | none (server-to-server) | apiKey / apiSecret | `x-session-token` |
| tradejini | CubePlus PIN (password), twoFa, twoFaTyp | api_key / (secret unused) | `Authorization: Bearer <api_key>:<access_token>` |
| fivepaisa | TOTP, PIN | `api_key:::user_id:::client_id` / EncryKey | `head{key,...}` envelope + `Authorization: bearer <token>` |
| nubra | phone OTP (x1-2) + MPIN, or TOTP | phone / MPIN | `Authorization: Bearer` + `x-device-id: OPENALGO` |
| indmoney | MPIN + TOTP | client id (`x-api-key`) / optional pasted 24h token | `Authorization: <token>` (no Bearer) |
| deltaexchange | none | API key / API secret | `api-key`, `timestamp`, `signature` (HMAC-SHA256 of METHOD+ts+path+query+body) |

### mstock

Paths below are relative to `/Users/openalgo/openalgo-desktop/openalgo/broker/mstock/` unless absolute. `brlogin` = `/Users/openalgo/openalgo-desktop/openalgo/blueprints/brlogin.py`.

### 1. Family / template
- Direct login + TOTP (password + TOTP in one form POST, no OAuth redirect). SKILL.md family table lists mstock with angel as the copy template (`.claude/skills/broker-integration/SKILL.md:47`). The mstock "Type B" API is an Angel-SmartAPI clone: same field names (`tradingsymbol/symboltoken/transactiontype/producttype/variety`), same binary WS layout (`mapping/transform_data.py:147`, `api/mstockwebsocket.py:154-255`).
- plugin.json: `supported_exchanges` NSE, BSE, NFO, BFO, CDS, NSE_INDEX, BSE_INDEX; `broker_type` IN_stock; `leverage_config` false (`plugin.json:8-10`).

### 2. Auth flow
| Item | Value | Cite |
|---|---|---|
| `BROKER_API_KEY` | mstock **clientcode** (login id) | `api/auth_api.py:26-29` |
| `BROKER_API_SECRET` | mstock **API key** → sent as `X-PrivateKey` header on every call | `api/auth_api.py:83-87`, `api/funds.py:15,30` |
| Form fields (React TOTP page) | `password`, `totp` (GET `/mstock/callback` redirects to `/broker/mstock/totp`) | brlogin:101-125 |
| Step 1 | `POST https://api.mstock.trade/openapi/typeb/connect/login` headers `{"X-Mirae-Version":"1","Content-Type":"application/json"}` body `{"clientcode":<KEY>,"password":<pw>,"totp":<totp>,"state":""}` → `data.refreshToken` (fallback `data.jwtToken`); success when `status in (True,"true")` | `api/auth_api.py:41-78` |
| Step 2 | `POST .../typeb/session/verifytotp` headers + `X-PrivateKey:<SECRET>` body `{"refreshToken":<rt>,"totp":<totp>}` → `data.jwtToken` (auth token), `data.feedToken` | `api/auth_api.py:84-124` |
| Alt OTP path (unused by brlogin) | `send_otp()` same login with `totp:""` then `POST .../typeb/session/token` body `{"refreshToken","otp"}` | `api/auth_api.py:141-305` |
| Stored auth | bare `jwtToken`; `feed_token` passed to `handle_auth_success(..., feed_token=feed_token, user_id=None)` | brlogin:136-139 |
| Expiry | daily (~3 AM IST rollover noted in WS client); no refresh implemented | `api/mstockwebsocket.py:130-131` |
- Session-loss quirk: POST without Flask session returns JSON 401 instead of redirect (brlogin:50-52, 108-115).

### 3. Base URL / headers
- REST base `https://api.mstock.trade/openapi/typeb` (`api/order_api.py:97`, `api/data.py:31`).
- Per-call headers: `X-Mirae-Version: 1`, `Authorization: Bearer <jwtToken>`, `X-PrivateKey: <BROKER_API_SECRET>`, `Content-Type: application/json` (`api/order_api.py:90-95`).
- HTTP 200 is returned for business failures; success is judged by payload `status in (True,"true","True","TRUE")` (`api/order_api.py:26-45`). Responses are sometimes wrapped in a single-element list (`api/order_api.py:288-291`, `:545-548`).

### 4. REST endpoints
| Op | Method/path | Body / notes | Response consumed | Cite |
|---|---|---|---|---|
| Place | `POST /orders/regular` | transform_data dict (see §5) | `data.orderid` or `data.uniqueorderid` | `order_api.py:245-311`, `:48-69` |
| Modify | `PUT /orders/regular/{orderid}` | modify dict incl. `orderid`, `modqty_remng:"0"` | `data.orderid` fallback request id | `order_api.py:564-661`, `transform_data.py:102-135` |
| Cancel | `DELETE /orders/regular/{orderid}` body `{"variety":"NORMAL","orderid":id}` | success if payload status true or `message=="SUCCESS"`; HTTP200 failure → 400 | `order_api.py:495-562` |
| Cancel-all | `GET /orders` then `POST /orders/cancelall` (no body) when any order status lower in `open/pending/o-pending/trigger pending` | — | `order_api.py:664-748` |
| Orderbook | `GET /orders` | — | `data[]` | `order_api.py:120-122` |
| Tradebook | `GET /tradebook` | — | uppercase keys `SEC_ID, SYMBOL, EXCHANGE, INSTRUMENT_NAME, PRODUCT` | `order_api.py:125-127`, `order_data.py:280-285,346` |
| Positions | `GET /portfolio/positions` | — | `symboltoken, exchange, producttype, netqty, avgnetprice, netvalue, instrumenttype` | `order_api.py:130-132`, `order_data.py:504-536` |
| Holdings | `GET /portfolio/holdings` | — | `product` (DELIVERY→CNC) | `order_api.py:135-137`, `order_data.py:579-581` |
| Close-all | positions loop → MARKET place with `reverse_map_product_type`; derivatives looked up under NFO/BFO when `instrumenttype` in OPTIDX/OPTSTK/FUTIDX/FUTSTK | — | `order_api.py:402-492` |
| Funds | `GET /user/fundsummary` | — | `data[0]` keys below | `funds.py:35-54` |
| Margin | `POST /margins/orders` body `{"orders":[{product_type,transaction_type,quantity,price,exchange,symbol_name,token,trigger_price}]}` | `summary.total_charges`, breakup SPAN/exposure | `margin_api.py:42-81`, `margin_data.py:49-56,120-146` |
| Quotes/multiquotes | `GET /instruments/quote` **with JSON body** `{"mode":"OHLC","exchangeTokens":{"NSE":["3045"]}}` | `data.fetched[]` → `ltp/open/high/low/close/volume/symbolToken`; bid/ask/oi = 0 | `data.py:215-243`, `:386-417` |
| Depth | one-off WS `fetch_quote(token, exchange_type, mode=3)` (no REST) | 5-level bids/asks | `data.py:854-912`, `mstockwebsocket.py:877-959` |
| History | `GET /instruments/historical` (JSON body) / `POST /instruments/intraday` | §7 | `data.py:611-623`, `:766-791` |
| Master | `GET /instruments/OpenAPIScripMaster` | JSON array | §6 | `master_contract_db.py:91-120` |

### 5. Enum mappings
- Exchange: passed through unchanged in orders (`transform_data.py:84`). Broker→OA: `NSE/BSE` + instrumenttype in OPTIDX/OPTSTK/FUTIDX/FUTSTK → `NFO/BFO` (`order_data.py:9-29`).
- Product OA→broker: CNC→DELIVERY, NRML→CARRYFORWARD, MIS→INTRADAY (default INTRADAY) (`transform_data.py:158-169`). Broker→OA: DELIVERY→CNC, CARRYFORWARD→NRML, INTRADAY→MIS, MARGIN→MIS (`transform_data.py:182-192`).
- Pricetype→ordertype: MARKET, LIMIT, SL→STOPLOSS_LIMIT, SL-M→STOPLOSS_MARKET (`transform_data.py:149-155`); variety: MARKET/LIMIT→NORMAL, SL/SL-M→STOPLOSS (`transform_data.py:178`). Reverse accepts STOPLOSS_LIMIT/STOP_LOSS/SL→SL, STOPLOSS_MARKET/STOP_LOSS_MARKET/SL-M→SL-M (`order_data.py:48-57`).
- Action: `action.upper()` → `transactiontype`; validity always `duration:"DAY"` (`transform_data.py:85,95`).
- Place payload literal: `{"variety","tradingsymbol","symboltoken","exchange","transactiontype","ordertype","quantity","producttype","price","triggerprice","squareoff":"0","stoploss":"0","trailingStopLoss":"","disclosedquantity","duration":"DAY","ordertag":""}` (`transform_data.py:80-97`). Symbol resolution prefers `-EQ` over `-BZ` over `-BE` brsymbol (`transform_data.py:11-59`).
- Order status→OA: `Traded`/contains `TRADE CONFIRMED`→complete; `O-Pending/Pending/pending/O-Modified/o-modified`→open; `Rejected`→rejected; `Cancelled/O-Cancelled`→cancelled; contains `trigger pending`→trigger pending; else lowercased (`order_data.py:208-220`). Price shown = `averageprice` when complete or MARKET/SL-M, else `price` (`order_data.py:225-234`). Timestamp `updatetime`.
- Funds: AVAILABLE_BALANCE→availablecash, COLLATERALS→collateral, REALISED_PROFITS→m2mrealized, MTM_COMBINED→m2munrealized, AMOUNT_UTILIZED→utiliseddebits, formatted `%.2f` (`funds.py:48-65`).
- Price scaling: REST prices are rupees; WS binary prices are paise (`/100`) (`mstockwebsocket.py:171`). Positions `ltp` = "NA", pnl = `netvalue` (`order_data.py:514-527`).

### 6. Master contract
- `GET https://api.mstock.trade/openapi/typeb/instruments/OpenAPIScripMaster` with auth headers → JSON array (`master_contract_db.py:91-115`). Columns renamed: API `symbol`→`name`, API `name`→`brsymbol`, `exch_seg`→`exchange`, plus `token, lotsize, instrumenttype, expiry, strike, tick_size` (`:299-310`). `brexchange = exchange` (`:317`).
- Exchange fixes: NSE rows with instrumenttype in OPTCUR/FUTCUR/OPTIRC/FUTIRC → `CDS`; BSE → `BCD` (`:324-333`). BSE index tokens scraped from `https://tradingapi.mstock.com/docs/v1/Annexure/` (regex on `<tr>` rows ending `BSE`) → `BSE_INDEX` with rename map e.g. MID150→BSE150MIDCAPINDEX, MIDSEL→BSEMIDCAPSELECTINDEX (`:362-428`).
- Equity symbol = brsymbol stripped of `-EQ$|-BZ$` (`:336`). Expiry → `DD-MMM-YY` upper (`:126-181`). Symbols: FUT = `name+DDMMMYY+FUT`; options = `name+DDMMMYY+strike(no .0)+CE|PE` for NFO/BFO/CDS/BCD/MCX (`:437-680`). instrumenttype normalised to FUT/CE/PE (`:683-700`).
- NSE indices scraped from same Annexure page (rows ending `NSE`): `symbol=script`, `brsymbol=name`, `exchange=NSE_INDEX`, `brexchange=NSE`, renames NIFTY50→NIFTY, NIFTYNEXT50→NIFTYNXT50, NIFTYFINSERVICE→FINNIFTY, NIFTYBANK→BANKNIFTY, NIFTYMIDSELECT→MIDCPNIFTY, etc. (`:200-268`). Pipeline: delete table, insert master, then insert NSE indices (`:710-750`).

### 7. History
- Intervals: 1m,3m,5m,10m,15m,30m→`ONE_MINUTE…THIRTY_MINUTE`, 1h→`ONE_HOUR`, D→`ONE_DAY` (`data.py:114-125`). Exchange strings NSE/BSE/NFO/BFO/CDS/MCX (indices→NSE/BSE) (`:129-139`).
- Historical: `GET /instruments/historical` JSON body `{"exchange","symboltoken","interval","fromdate":"YYYY-MM-DD HH:MM","todate"}`; chunk days per interval 1m:2, 3m:8, 5m:13, 10m:26, 15m:40, 30m:76, 1h:166, D:1000 (≈1000-candle cap); 1 s sleep between chunks (`:576-623`). Candles `[ts,o,h,l,c,v]`, ts like `2024-01-01T09:15:00+05` → UTC epoch seconds; D normalised to midnight; oi=0 (`:650-725`).
- Current day: if range is today only → `POST /instruments/intraday` body `{"exchange":<numeric 1 NSE,4 BSE,2 NFO,5 BFO,3 CDS,6 MCX>,"symboltoken","interval"}`; ts `"2025-04-04 15:27"` IST→UTC epoch (`:142-153`, `:755-830`). If range ends today → historical to yesterday + intraday, concat/dedupe (`:486-521`).

### 8. Streaming
- URL `wss://ws.mstock.trade?API_KEY=<BROKER_API_SECRET or KEY>&ACCESS_TOKEN=<jwtToken>` (`mstockwebsocket.py:73,88,123-125`). On open send text `LOGIN:<jwtToken>`; no ACK awaited (`:605-625`). Any binary/text frame marks logged-in.
- Subscribe JSON `{"action":1,"params":{"mode":<1|2|3>,"tokenList":[{"exchangeType":<int>,"tokens":["3045"]}]}}`; unsubscribe `action:0` (`:726-840`). exchangeType: NSE 1, NFO 2, BSE 3, BFO 4, CDS 13, MCX 5 (unverified) (`streaming/mstock_mapping.py:13-25`). Modes 1 LTP, 2 Quote, 3 Snap Quote/depth(5) (`:65-81`).
- Frame: optional 4-byte header `<H num_packets, <H packet_size` then packets (`:300-360`). Packet little-endian: `[0]` mode u8, `[1]` exchange_type u8, `[2:27]` token ASCII NUL-padded, `[27:35]` seq u64, `[35:43]` exch ts u64, `[43:51]` ltp u64/100 (51-byte LTP). Quote 123 bytes adds `[51:59]` ltq u64, `[59:67]` avg u64/100, `[67:75]` vol u64, `[75:83]` tot buy f64, `[83:91]` tot sell f64, `[91:99]` open, `[99:107]` high, `[107:115]` low, `[115:123]` close (all u64/100). Full 379 bytes adds `[123:131]` last trade ts, `[131:139]` oi u64, `[139:147]` oi% u64/100, `[147:347]` depth 10×20 bytes (bids 0-99, asks 100-199; each `+2:+10` qty u64, `+10:+18` price u64/100, `+18:+20` orders u16), `[347:355]` UC, `[355:363]` LC, `[363:371]` 52wH, `[371:379]` 52wL (`:154-298`).
- Keepalive: websocket ping_interval 20 s / timeout 10 s; reconnect exponential base 2 s, max 60 s, 10 attempts, token re-read from DB before each reconnect (`:64-68,127-150,408-481`). Adapter batches subscribes per mode, topic `{exchange}_{symbol}_{LTP|QUOTE|DEPTH}` (`streaming/mstock_adapter.py:181-195,452-590`). No order-update stream.

### 9. Quirks
- Type B (`/openapi/typeb/...`) is the only API used; no Type A paths remain. GET endpoints `/instruments/quote` and `/instruments/historical` take JSON bodies (`data.py:39-43`).
- Data API rate limit 1 req/s → 1.0 s sleep between history chunks and multiquote batches of 500 (`data.py:262-281,594-605`).
- Depth opens a fresh WS per request; gthread per-step timeout 3 s, total 5 s (`mstockwebsocket.py:30-33`).
- Smart order uses per-symbol locks and 1 s position cache (`order_api.py:140-190`).

---

### motilal

Paths relative to `/Users/openalgo/openalgo-desktop/openalgo/broker/motilal/` unless absolute. `brlogin` = `/Users/openalgo/openalgo-desktop/openalgo/blueprints/brlogin.py`.

### 1. Family / template
- Direct login + TOTP (userid + password + TOTP + DOB form, SHA-256 password hash). SKILL.md family table lists motilal under "Direct login + TOTP", copy template `broker/angel/` (`.claude/skills/broker-integration/SKILL.md:47`). Unlike angel it has a centralised `api/baseurl.py` with an endpoint table and common-header builder, a `:::`-joined two-part auth token, a binary broadcast feed, and a separate JSON order-update socket.
- plugin.json: exchanges NSE, BSE, NFO, BFO, CDS, MCX, NSE_INDEX, BSE_INDEX; `broker_type` IN_stock; `leverage_config` false (`plugin.json:8-10`).

### 2. Auth flow
| Item | Value | Cite |
|---|---|---|
| `BROKER_API_KEY` | App API Key → `ApiKey` header and login hash salt | `api/baseurl.py:27-28`, `api/auth_api.py:117` |
| `BROKER_API_SECRET` (fallback `BROKER_API_SECRET_KEY`) | App API Secret → `apisecretkey` header; optional | `api/baseurl.py:136-144` |
| Client code | NOT env; typed on TOTP page as `userid`, persisted as auth record `user_id`, read back via `get_user_id` | `api/baseurl.py:147-184`, brlogin:697-702 |
| Form fields | `userid`, `password`, `totp`, `dob` (GET redirects to `/broker/motilal/totp`) | brlogin:686-704 |
| Step 1 | `POST {base}/rest/login/v7/authdirectapi` headers = common set **without** Authorization; body `{"userid":<id>,"password":sha256(password+BROKER_API_KEY).hexdigest(),"2FA":<dob DD/MM/YYYY>,"totp":<totp>}` → `AuthToken`, `isAuthTokenVerified`; `status=="SUCCESS"` | `api/auth_api.py:128-169` |
| Verified gate | if `isAuthTokenVerified` in FALSE/0/NO/N → error (OTP path not implemented) | `api/auth_api.py:178-191` |
| Step 2 (optional) | `POST /rest/login/v1/getaccesstoken`, no body, common headers with `Authorization:<AuthToken>` + `apisecretkey`; → `accesstoken`; skipped when no secret, never fails login | `api/auth_api.py:54-94` |
| Stored auth | `"<AuthToken>:::<accesstoken>"` or bare AuthToken; split by `split_auth_token` | `api/baseurl.py:50,115-133` |
| feed_token | always None; `user_id=userid` passed to `handle_auth_success` | `api/auth_api.py:201`, brlogin:1046-1065 |
| Expiry | daily (~3 AM IST) per WS token_provider comment | `api/motilal_websocket.py:70-72` |

### 3. Base URLs / headers
- REST `https://openapi.motilaloswal.com` (UAT `https://openapi.motilaloswaluat.com`, override via `BROKER_API_URL`) (`api/baseurl.py:38-39,92-94`). Feed WS `wss://ws1feed.motilaloswal.com/jwebsocket/jwebsocket`; trade WS `wss://openapi.motilaloswal.com/ws` (`:43-48`).
- Common headers (every call): `Content-Type/Accept: application/json`, `User-Agent: MOSL/V.1.1.0`, `ApiKey:<KEY>`, `ClientLocalIp` (env `BROKER_CLIENT_LOCAL_IP` default 127.0.0.1), `ClientPublicIp`, `MacAddress` (default 00:00:00:00:00:00), `SourceId: WEB`, `vendorinfo` (`BROKER_VENDOR_CODE` or client code), `osname: Windows 10`, `osversion: 10.0.19041`, `devicemodel: AHV`, `manufacturer: DELL`, `productname: OpenAlgo`, `productversion: 1.0.0`, `browsername: Chrome`, `browserversion: 120.0`, optional `apisecretkey`, `Authorization:<AuthToken>`, `accesstoken:<accesstoken>` (`api/baseurl.py:192-237`).
- Response envelope `status` SUCCESS/FAILURE + `message` + `errorcode` (e.g. MO8001 invalid token, MO2012 vendorinfo, MO1062 client code needed, MO2031 client code forbidden) (`api/order_api.py:94-118`, `api/data.py:143-150`).

### 4. REST endpoints (paths from `api/baseurl.py:53-89`)
| Op | Method/path | Body | Response used | Cite |
|---|---|---|---|---|
| Place | `POST /rest/trans/v2/placeorder` | `{"exchange","symboltoken":int,"buyorsell","ordertype","producttype","orderduration":"DAY","price":float,"triggerprice":float,"quantityinlot":int,"disclosedquantity":int,"amoorder":"N"}` (+ optional algoid/goodtilldate/tag/participantcode) | top-level `uniqueorderid` | `order_api.py:350-475` |
| Modify | `POST /rest/trans/v5/modifyorder` | `{"uniqueorderid","newordertype","neworderduration":"DAY","newprice","newtriggerprice","newquantityinlot","newdisclosedquantity","newgoodtilldate":"","lastmodifiedtime","qtytradedtoday"}`; lastmodifiedtime from order detail, fallback recordinserttime/entrydatetime (MO1089 otherwise) | status only | `transform_data.py:177-202`, `order_api.py:695-884` |
| Order detail | `POST /rest/book/v5/getorderdetailbyuniqueorderid` `{"uniqueorderid"}` | `data[]` | `order_api.py:211-220` |
| Cancel | `POST /rest/trans/v2/cancelorder` `{"uniqueorderid"}` | status | `order_api.py:654-692` |
| Cancel-all | orderbook → cancel each with `orderstatus` lower in confirm/sent/open/partial | — | `order_api.py:886-923` |
| Orderbook | `POST /rest/book/v5/getorderbook` (no body) | `symboltoken, exchange, producttype, orderstatus, ordertype, orderqty, price, triggerprice, averageprice, uniqueorderid, lastmodifiedtime/entrydatetime/recordinserttime, buyorsell` | `order_api.py:167-168`, `order_data.py:249-320` |
| Tradebook | `POST /rest/book/v4/gettradebook` | `tradeqty, tradeprice, tradevalue, precision, tradetime` | `order_api.py:171-204`, `order_data.py:380-430` |
| Positions | `POST /rest/book/v4/getposition` | `buyquantity, sellquantity, buyamount, sellamount, LTP, marktomarket, bookedprofitloss, productname, symboltoken` | `order_api.py:207-208`, `order_data.py:492-531` |
| Holdings | `POST /rest/report/v3/getdpholding` `{}` | `scripname, dpquantity, buyavgprice, nsesymboltoken, bsescripcode` | `order_api.py:223-260`, `order_data.py:585-696` |
| Close-all | positions loop (net = buyquantity−sellquantity) → MARKET order via place_order_api | — | `order_api.py:573-651` |
| Funds | `POST /rest/report/v3/getreportmargindetail` `{}` | rows `{srno,particulars,amount}` | `funds.py:89-191` |
| Margin calc | **none** – raises NotImplementedError → HTTP 501 | — | `margin_api.py:19-34` |
| Quotes | `POST /rest/report/v3/getltpdata` `{"exchange":<NSE/NSEFO..>,"scripcode":int}` (+`clientcode` in dealer mode) | `data.{ltp,bid,ask,open,high,low,close,volume}` paise → /100 | `data.py:560-608` |
| Index quotes | `POST /rest/report/v3/getindexltpdata` `{"exchangename":"NSE","scripcode":"26000"}` (tries `exchange` spelling once; MO1051) | `data[]` list, rupees, o/h/l/c/ltp only | `data.py:614-712` |
| Multiquotes | WS register + poll, batch 100, 0.1 s between batches | — | `data.py:714-1045` |
| Depth | broadcast WS (register_scrip, wait for level packets) | 5 levels | `data.py:1047-1264` |
| History | none (see §7) | — | `data.py:1393-1471` |
| Master | `GET /getscripmastercsv?name=<EXCH>`, `GET /getindexdatacsv?name=NSE|BSE` | CSV | `master_contract_db.py:101,386` |

### 5. Enum mappings
- Exchange OA→broker: NSE, BSE, NFO→NSEFO, CDS→NSECD, MCX, BFO→BSEFO (`transform_data.py:18-29`); reverse at `:36-44`.
- Product OA→broker, exchange-aware: cash (NSE/BSE) CNC→DELIVERY, MIS→VALUEPLUS, NRML→NORMAL; F&O all three→NORMAL (`transform_data.py:242-298`). Broker→OA: DELIVERY→CNC, VALUEPLUS→MIS, NORMAL→NRML, SELLFROMDP→CNC, BTST→CNC, MTF→NRML, unknown→MIS (`:339-350`).
- Pricetype→ordertype: MARKET, LIMIT, SL→STOPLOSS, SL-M→STOPLOSS (`:205-216`). **MPP**: MARKET → LIMIT at protected price from live LTP; SL-M → STOPLOSS with limit at trigger±slab (Motilal rejects MARKET on algo channel, M01108) (`:47-150`). Reverse: STOPLOSS→SL if triggerprice>0 else SL-M (`order_data.py:276-278`).
- Action `buyorsell` = action.upper(); validity `orderduration:"DAY"`; `amoorder:"N"` (`transform_data.py:156-172`).
- Quantity: shares ÷ lotsize → `quantityinlot`; must be multiple; derivative lotsize missing → refuse (DERIVATIVE_EXCHANGES NFO/CDS/MCX/BFO/NSEFO/NSECD/BSEFO) (`order_api.py:29,63-92,386-402`).
- Order status→OA (lowercased key): traded/complete→complete; sent/confirm/open/partial/unknown→open; rejected/error→rejected; cancel/cancelled→cancelled (`order_data.py:29-41`).
- Price scaling: REST books ship integer prices scaled by `10**precision` when a `precision` field exists; tradebook defaults to 2 when absent/0; orderbook left unscaled when no field (`order_data.py:19,61-100,380-410`). getltpdata values are paise (/100); index LTP in rupees. WS prices are floats in rupees.
- Funds: availablecash = srno 201 (fallback 102−220+300−600), collateral = srno 220, utiliseddebits = srno 300 (fallback sum 301/321/340/360/380/381), m2munrealized = Σ srno 402/422/442/462/482, m2mrealized = Σ 403/423/443/463/483 (`funds.py:23-42,121-191`).
- Positions: quantity = buyquantity−sellquantity, avg = buyamount/buyqty or sellamount/sellqty, pnl = marktomarket+bookedprofitloss (`order_data.py:502-531`). Holdings exchange chosen by nsesymboltoken>0 → NSE else bsescripcode → BSE, product CNC (`:645-696`).

### 6. Master contract
- Per exchange `GET {base}/getscripmastercsv?name=` for NSE, BSE, NSEFO, NSECD, MCX, BSEFO (public, no auth) (`master_contract_db.py:86-116,802`). Columns renamed: scripcode→token, scripname→symbol(→brsymbol), scripshortname→name, marketlot→lotsize, instrumentname→instrumenttype, expirydate→expiry, strikeprice→strike, ticksize→tick_size, exchangename→brexchange (`:495-510`). Exchange map NSEFO→NFO, NSECD/NSECO→CDS, BSEFO→BFO, BSECD/BSECO→BCD (`:513-526`).
- Expiry parsed from scripname token `DD-MMM-YYYY` → `DD-MMM-YY` (expirydate column is seconds since 1980, deliberately ignored) (`:313-370,529`). instrumenttype stripped/upper; FUT when contains FUT & optiontype XX; CE/PE from `optiontype`; IDX rows → NSE_INDEX/BSE_INDEX/MCX_INDEX; blank cash → EQ; non-option strike → 0.0 (`:548-601`).
- Symbols: FUT `name+DDMMMYY+FUT`; options `name+DDMMMYY+strike+CE|PE` (strike int if whole) (`:625-692`). BFO underlying aliases BSX→SENSEX, BKX→BANKEX, SX50→SENSEX50, BIT unmapped (`:292-338`). Cash symbol = `name` (scripshortname) with series suffix stripped; EQ row wins dedupe; brsymbol keeps raw `"INFY EQ"` (`:713-760`).
- Indices `GET /getindexdatacsv?name=NSE|BSE`: indexcode→token, indexname→symbol/brsymbol/name, exchange `NSE_INDEX`/`BSE_INDEX`, instrumenttype INDEX, lotsize 1, tick 0.05 (`:371-470`). Aliases: NIFTY50→NIFTY, NIFTYNEXT50→NIFTYNXT50, NIFTYFINSERVICE/NIFTYFINSERV→FINNIFTY, NIFTYBANK→BANKNIFTY, NIFTYMIDSELECT→MIDCPNIFTY; BSE live names e.g. "BSE SENSEX 50"→SENSEX50, "BSE 150 MIDCAP"→BSE150MIDCAPINDEX, MIDSEL→BSEMIDCAPSELECTINDEX (`:131-233`). Final dedupe on (symbol, exchange) (`:853`).

### 7. History
- No historical/intraday OHLC API. `timeframe_map = {"D":"D"}`; any non-daily interval raises; only today's bar is synthesised from `get_quotes` (open/high/low/volume + ltp as close), stamped midnight UTC of IST date (`data.py:283-292,1336-1471`).

### 8. Streaming
- Market feed: binary jWebSocket `wss://ws1feed.motilaloswal.com/jwebsocket/jwebsocket` (`baseurl.py:43`). Login packet struct `=cHB15sB30sBBBB10sBBBBB45s`: `'Q'`, u16 111, len(clientcode), clientcode ljust 15, len again, clientcode ljust 30, 1,1,1, len(version), `"1.0.0"` ljust 10, 0,0,0,0,1, 45 spaces (114 bytes); no auth token in frame; first binary reply marks authenticated (`motilal_websocket.py:52,355-395,402-430`).
- Register packet `=cHcciB`: `'D'`, u16 7, exchange char (N NSE/NSEFO, B BSE, M MCX, C NSECD, D NCDEX, G BSEFO), segment char `C`ASH/`D`ERIVATIVES, i32 scrip, u8 1 add / 0 remove (`:966-992,1049-1068,1243-1260`). Index subscribe JSON `{"clientid","action":"IndexRegister","exchange":"NSE"}` (undocumented guess) (`:1075-1118`).
- Inbound frames = concatenated 30-byte packets, little-endian: `[0]` exchange char, `[1:5]` i32 scrip, `[5:9]` i32 ts, `[9]` msgtype, `[10:30]` body (`:453-486`). msgtype: `A` LTP (f32 rate, i32 ltq, i32 cum vol, f32 avg price, i32 OI); `B`-`F` depth levels 1-5 (f32 bid, i32 bidqty, i16 bid orders, f32 ask, i32 askqty, i16 ask orders); `G` OHLC (4×f32 open/high/low/prev close); `H` index (f32 rate); `m` OI (3×i32 oi/high/low); `W` DPR (f32 upper, f32 lower); `1` heartbeat (`:518-560,595-805`).
- Adapter polls client caches every cycle and publishes `{exchange}_{symbol}_{LTP|QUOTE|DEPTH}`; modes 1/2/3, depth 5 (`streaming/motilal_adapter.py:443-520`, `motilal_mapping.py:107-135`). Reconnect backoff `min(2**n,30)` s; no heartbeat sent; 60 s silence = disconnected (`motilal_websocket.py:842-893,1187-1201`).
- Order updates: JSON `wss://openapi.motilaloswal.com/ws`; on open send `{"clientid","authtoken":<AuthToken half>,"apikey":<BROKER_API_KEY>}` then `{"clientid","action":"OrderSubscribe"}`; heartbeat `{"clientid","action":"heartbeat"}` every 30 s; logout action on disconnect; frames with `orderstatus`+`uniqueorderid` normalised (prices already rupees); errors MO1001/MO8000/MO2012 (`streaming/motilal_order_adapter.py:1-76,110-137,144-229`).

### 9. Quirks
- Dealer mode: `clientcode` added to LTP/index payloads only after MO1062 is seen (`data.py:143-198`).
- `vendorinfo` mandatory (MO2012); `browsername/browserversion` mandatory for SourceId=WEB (`baseurl.py:193,221-224`).
- Multiquotes 100 per batch, 0.1 s sleep; depth waits up to 8 concurrent feed waiters (`data.py:28,729-746`).
- `symboltoken` returned as Number by books; coerce to str before DB lookup (`order_data.py:155-159`).

---

### samco

All paths relative to `/Users/openalgo/openalgo-desktop/openalgo/`. `W` = `broker/samco/`.

### 1. Family / template
- Listed in the **Direct login + TOTP** family (`.claude/skills/broker-integration/SKILL.md:47`), template `broker/angel/`. In practice samco v3.2 is a pure **server-to-server API key + secret exchange** with no user-entered TOTP (`W/api/auth_api.py:4-12`); closest behavioural analogue is a header-token broker (token in `x-session-token`), with the bespoke React page `SamcoAuth.tsx` at `/broker/samco/auth` (`references/auth-and-login.md:214`).
- plugin.json: exchanges `NSE,BSE,NFO,BFO,CDS,MCX,NSE_INDEX,BSE_INDEX`, `broker_type: IN_stock`, `leverage_config: false` (`W/plugin.json:8-10`).

### 2. Auth flow
| Item | Value | Cite |
|---|---|---|
| Credentials | `BROKER_API_KEY` = OAuth app apiKey, `BROKER_API_SECRET` = apiSecret (plain env, no `:::` composite) | `W/api/auth_api.py:65-72`, `utils/config.py:11-28` |
| Login form | GET `/samco/callback` redirects to `/broker/samco/auth`; POST calls `authenticate_broker()` with **no form fields** | `blueprints/brlogin.py:675-685` |
| Step 1 | `POST https://tradeapi.samco.in/session/token`, headers `Content-Type: application/json, Accept: application/json`, body `{"apiKey": ..., "apiSecret": ...}` sent verbatim (no client-side encryption) | `W/api/auth_api.py:105-125` |
| Success | `status=="Success"` and `sessionToken` present; also `accountID`, `srcIp`, `primaryIp`, `secondaryIp` | `W/api/auth_api.py:128-133`, `:83-85` |
| Stored token | raw `sessionToken` JWT string -> `auth_token`; no feed token | `W/api/auth_api.py:203` |
| Expiry | valid until 08:00 IST next day | `W/api/auth_api.py:6` |
| Error codes | `EOAUTH001` bad key/app inactive, `EOAUTH008` bad secret, `EOAUTH009` IP not registered; message built from `statusMessage` + hint | `W/api/auth_api.py:29-62` |
| IP diagnostic | `GET /ip/whoami` header `x-session-token`; fields `srcIp/primaryIp/secondaryIp/matches/matchedAs`; surfaced at `GET /samco/ip-status` | `W/api/auth_api.py:144-172`, `brlogin.py:1159-1198` |
| Static IP | Order endpoints reject unregistered IP with HTTP 403; registration only via dashboard `https://tradeapi.samco.in/app/login` | `W/api/auth_api.py:76-102` |

### 3. Base URL and headers
- REST base `https://tradeapi.samco.in` (`W/api/order_api.py:23`, `data.py:17`, `funds.py:12`).
- Every call: `Accept: application/json`, `Content-Type: application/json` (POST/PUT), `x-session-token: <sessionToken>` (`order_api.py:32-36`, `data.py:49-53`). WebSocket token is `unquote()`d before use (`streaming/samcoWebSocket.py:84`).

### 4. REST endpoints
| Function | Method + path | Request | Response consumed | Cite |
|---|---|---|---|---|
| place | `POST /order/placeOrder` | `symbolName, exchange, transactionType, orderType, quantity, disclosedQuantity, orderValidity, productType, afterMarketOrderFlag, [price], [triggerPrice], [marketProtection]` | `status=="Success"` -> `orderNumber` | `order_api.py:212-250` |
| modify | `PUT /order/modifyOrder/{orderNumber}` | `orderType, quantity, orderValidity, [disclosedQuantity>0], [price], [triggerPrice], [marketProtection]` | `orderNumber` or lowercase `ordernumber` (actual body) | `order_api.py:490-506`, `transform_data.py:192-228` |
| cancel | `DELETE /order/cancelOrder?orderNumber=` | query | `status` | `order_api.py:450-465` |
| cancel-all | orderbook, filter `orderStatus.lower() in [open,pending,trigger pending]`, cancel each | | | `order_api.py:514-543` |
| orderbook | `GET /order/orderBook` | | `orderBookDetails[]` | `order_api.py:59-63` |
| tradebook | `GET /trade/tradeBook` | | `tradeBookDetails[]` | `order_api.py:66-70` |
| positions | `GET /position/getPositions?positionType=DAY|NET` | | `positionDetails[]` | `order_api.py:73-90` |
| close-all | fetch DAY then NET, dedupe on (tradingSymbol,exchange,productCode), DAY wins; refuse if either book failed; close via MARKET `place_order_api` | | | `order_api.py:336-437` |
| holdings | `GET /holding/getHoldings` | | `holdingDetails[]`, `holdingSummary` | `order_api.py:93-97`, `order_data.py:395-414` |
| funds | `GET /limit/getLimits` | | `equityLimit.{netAvailableMargin, marginUsed, collateralMarginAgainstShares}` | `funds.py:21-50` |
| margin calc | `POST /spanMargin` body `{"request":[{exchange,tradingSymbol,qty,productType,orderType:"L",transactionType,price}]}` (derivatives only: NFO/MCX/CDS/BFO/MFO) | | `spanDetails.{totalMargin|totalRequirement, marginRequired|spanRequirement, exposureMargin, spreadBenefit}` | `margin_api.py:74-80`, `margin_data.py:44-86,131-152` |
| quote | `GET /quote/getQuote?symbolName=<br>&exchange=<brexchange unless NSE>` | | `quoteDetails.{bestBids[0].price, bestAsks[0].price, openValue, highValue, lowValue, lastTradedPrice, previousClose, totalTradedVolume, openInterest}` | `data.py:271-306` |
| index quote | `GET /quote/indexQuote?indexName=<brsymbol>` | | `indexDetails[0].{spotPrice, openValue, highValue, lowValue, closeValue, totalTradedVolume, listingId}` | `data.py:311-352`, `:217-254` |
| multiquote | `POST /quote/multiQuote` body `{"<EXCH>":[brsymbols...]}` (MCX derivs under `MFO` key) | batch 25, 0.2s sleep between batches | `multiQuotes[].{symbol(="<scripCode>_<seg>" join key), bidPrice, askPrice, bidSize, askSize, open, high, low, lastTradePrice, previousClose, totalTradeVolume, openInterest}` | `data.py:485-486,602-690` |
| depth | `POST /marketDepth` body `{symbolName, [exchange]}` + a `getQuote` for OHLC | | `MarketDepthDetails.marketDepth.{bestFiveBid[].bidPrice/bidSize, bestFiveAsk[].askPrice/askSize, tBuyQty, tSellQty}` | `data.py:389-469` |
| history | see section 7 | | | |

### 5. Enum mappings and formulas
- Exchange: OpenAlgo code sent as-is for orders (`transform_data.py:165`); quote/depth/multiquote use `brexchange` (MCX derivs -> `MFO`) (`data.py:167-185`). Candle endpoints never use MFO (`data.py:176-177`).
- Product OA->broker: `CNC->CNC, NRML->NRML, MIS->MIS`, default MIS; reverse identical (`transform_data.py:242-263`).
- Pricetype: `MARKET->MKT, LIMIT->L, SL->SL, SL-M->SL-M` nominally, **but** MARKET is converted to `L` at MPP-protected LTP and SL-M to `SL` at protected trigger, with `marketProtection` = slab % formatted `%g` (Samco docs only L/SL) (`transform_data.py:39-134,137-145,231-239`). Modify uses same conversion (`:192-228`).
- Action: `transactionType` = uppercased action (`transform_data.py:158,166`). Validity always `DAY`, `afterMarketOrderFlag: "NO"` (`:171-172`).
- Order status broker->OA: `open/pending/ordered/trigger pending/after market order req received -> open; complete/completed/executed/filled -> complete; cancelled/canceled -> cancelled; rejected -> rejected` (`order_data.py:48-61`).
- Ordertype broker->OA: `L`+`marketProtection` -> MARKET, `L` -> LIMIT, `MKT` -> MARKET, `SL`, `SL-M` (`order_data.py:181-194`). Fill fields `filledQuantity`, `unfilledQuantity|pendingQuantity`, `averagePrice||fillPrice`, qty key `totalQuanity` [sic] (`:200-227`).
- Positions: `netQuantity` always positive; negate when `transactionType=="SELL"`; avg = `averageSellPrice`/`averageBuyPrice`; pnl = realized+unrealized; strings may carry commas (`order_data.py:344-388`).
- Funds: `availablecash=equityLimit.netAvailableMargin`, `collateral=collateralMarginAgainstShares`, `utiliseddebits=marginUsed`, m2m fields 0 (`funds.py:38-50`).
- Holdings: pnl% = `pnl/(holdingsValue-pnl)*100`; totals from `holdingSummary.portfolioValue`, `totalGainAndLossAmount` (`order_data.py:426-448,467-474`).
- Price scaling: none (decimal strings).

### 6. Master contract
- URL `https://developers.stocknote.com/doc/ScripMaster.csv` via `requests.get`, saved to `tmp/samco_scripmaster.csv` (`W/database/master_contract_db.py:738-752`).
- Column map: `Exchange/exchange -> exchange`, `Trading Symbol/tradingSymbol -> brsymbol`, `Symbol Name/symbolName -> name`, `Instrument -> instrumenttype`, `symbolCode/Symbol Code/Token -> token`, `Lot Size`, `Tick Size`, `Expiry Date`, `Strike Price` (`:820-845`). `brexchange = exchange` before remap (`:849`).
- Expiry normalized to `DD-MMM-YY` via many input formats (`:783-810`); for symbols dashes stripped -> `26FEB24` (`:904`).
- Exchange remap: `MFO -> MCX`; INDEX rows -> `NSE_INDEX/BSE_INDEX/MCX_INDEX` (`:886-891`).
- Symbols: EQ strips `-EQ|-BE|-MF|-SG` (`:895-897`); FUT = `name+expiry+FUT` for NFO/MCX/CDS/BFO; options = `name+expiry+strike(.0 stripped)+CE|PE` using brsymbol suffix (`:906-981`). Index renames `NIFTY 50->NIFTY`, `NIFTY BANK->BANKNIFTY`, `NIFTY FIN SERVICE->FINNIFTY`, `NIFTY MID SELECT->MIDCPNIFTY`, `INDIA VIX->INDIAVIX`, etc. (`:984-999`). instrumenttype normalized to `CE/PE/FUT` (`:1004-1016`).
- Indices appended from hardcoded list (68 total; 38 NSE + 30 BSE) since CSV lacks them: `brsymbol` = Samco indexName (e.g. `NIFTY 50`), `token` like `NIFTY_50`, `brexchange NSE/BSE`, lotsize 1, tick 0.05 (`:76-726`).
- Token stored = Samco `symbolCode` in form `<scripCode>_<segment>` (e.g. `41015_NFO`), which is also the multiQuote/WS join key (`data.py:579-589`).

### 7. History
- Intervals: `1m,5m,10m,15m,30m,1h -> 1,5,10,15,30,60`; `D -> DAY` (`data.py:141-152`).
- Daily: `GET /history/candleData?symbolName=&fromDate=YYYY-MM-DD&toDate=&[exchange=]` (key `historicalCandleData`), index: `/history/indexCandleData?indexName=` (keys `indexCandleData|historicalCandleData`); `date` -> normalized midnight -> epoch (`data.py:816-871`). When end=today, fetches only through yesterday (`:756-767`).
- Intraday: `GET /intraday/candleData?symbolName=&fromDate=YYYY-MM-DD%2000:00:00&toDate=...%2023:59:59&[interval=]&[exchange=]` (key `intradayCandleData`), index `/intraday/indexCandleData` (keys `indexIntraDayCandleData|intradayCandleData`); `dateTime` IST -> UTC epoch (`:926-1007`). No chunking; one call per range. Retries: 429 and >=500 with 1/2/4s backoff, max 3; 403 -> auth error (`:70-123`).

### 8. Streaming
- URL `wss://stream.samco.in`, header `x-session-token: <unquoted token>`; ping 30s/timeout 10s; app heartbeat closes socket if no message for 120s (`streaming/samcoWebSocket.py:30-40,222-234,262,813-822`).
- Protocol **JSON, newline-delimited**: each frame sent as `json.dumps(req)+"\n"` (`:509-517`).
- Subscribe (full replace semantics, coalesced 0.25s): `{"request":{"streaming_type":"quote"|"quote2","data":{"symbols":[{"symbol":"<scripCode>_<SEG>"}]},"request_type":"subscribe"|"unsubscribe","response_format":"json"}}` (`:389,467-520,982-993`). Index symbol key = `listingId` from `/quote/indexQuote` (negative, e.g. `-23`), no suffix (`:849-863`, `samco_adapter.py:239-250`).
- Streams: `quote` (flat frame: `sym, ltp, ltq, o, h, l, c, ch, chPer, vol, oI, avgPr, bPr, bSz, aPr, aSz, tBQ, tSQ, lTrdT/ltt, streaming_type`) and `quote2`/`marketDepth` (wrapped `{"response":{"data":{...bidValues[{price,qty,no}], askValues, tbq, taq},"streaming_type":"quote2"}}`); depth subscribers are put on both and frames merged per symbol (`:471-476,522-667`).
- Modes 1 LTP, 2 Quote, 3 Depth(5 only); exchangeType map uses brexchange incl. `MFO` (`samco_mapping.py:11-21,44-56`). Reconnect: adapter exp backoff 5s*2^n cap 60s, max 10; stops on 401/403 auth failure (`samco_adapter.py:32-35,119-197`). No order-update stream.

### 9. Quirks
- MPP conversion is mandatory for MARKET/SL-M (fetches LTP via REST per market order) (`transform_data.py:68-104`).
- multiQuote does not echo requested tradingSymbol (returns compact `NIFTY2681124600CE`) nor MFO exchange; join on `symbol` token (`data.py:625-648`).
- Batch 25 / 5 req/s; Samco attaches `msgId`/`serverTime` to 5xx for support (`data.py:105-123`).
- Positions split DAY/NET; smart-order position cache 1s + per-symbol lock (`order_api.py:100-150`).
- `/quote/getQuote` omits `exchange` param for NSE (`data.py:275-277`).

---

### tradejini

All paths relative to `/Users/openalgo/openalgo-desktop/openalgo/`. `W` = `broker/tradejini/`.

### 1. Family / template
- **Direct login + TOTP** family (`.claude/skills/broker-integration/SKILL.md:47`), template `broker/angel/`. Form rendered by `BrokerTOTP.tsx` at `/broker/tradejini/totp` (`references/auth-and-login.md:190-193`). Auth is a single form POST exchanging password + 2FA for a bearer access token; every later call uses `Bearer <api_key>:<access_token>`.
- plugin.json: exchanges `NSE,BSE,NFO,BFO,CDS,BCD,MCX,NSE_INDEX,BSE_INDEX`, `broker_type: IN_stock`, `leverage_config: false` (`W/plugin.json:8-10`).

### 2. Auth flow
| Item | Value | Cite |
|---|---|---|
| Credentials | `BROKER_API_KEY` = app API key (32-char alnum); fallback read from `BROKER_API_SECRET` for old `.env`s; placeholder `YOUR_*` ignored. No `:::` composite. API secret never sent in individual flow | `W/api/auth_api.py:19-45` |
| Form fields | `password` (CubePlus login PIN), `twofa`, `twofatype` (`otp`|`totp`, default totp) | `blueprints/brlogin.py:260-280`, `auth_api.py:84-86` |
| Step 1 | `POST https://api.tradejini.com/v2/api-gw/oauth/individual-token-v2`, headers `Authorization: Bearer <api_key>`, `Content-Type: application/x-www-form-urlencoded`; form `password, twoFa, twoFaTyp` | `auth_api.py:98-113` |
| Response | `{scope, access_token, token_type:"bearer", expires_in}`; token_type checked case-insensitively | `auth_api.py:121-131` |
| Errors | envelope `{"s":"error","msg":...}`; HTTP 401 is a bare "Unauthorized" for wrong IP/key/PIN/TOTP alike -> fixed hint string | `auth_api.py:54-65,132-139` |
| Stored | `auth_token` = `access_token` only (api_key re-read from env on every call); no feed token | `brlogin.py:272-275`, `order_api.py:85-92` |
| Alt OAuth (unused by login) | `GET /api-gw/oauth/authorize?client_id=<api_key>&redirect_uri&response_type=code&scope=general&state`; `POST /api-gw/oauth/token` with `client_secret=BROKER_API_SECRET` | `auth_api.py:148-190` |
| Static IP | individual apps accept only whitelisted IP; rejected at gateway as 401 | `auth_api.py:58-65` |

### 3. Base URL and headers
- `https://api.tradejini.com/v2` (`auth_api.py:13`, `order_api.py:100`, `data.py:1077`).
- Headers: `Authorization: Bearer <api_key>:<access_token>`; `Content-Type: application/x-www-form-urlencoded` for OMS calls (bodies are form-encoded, not JSON) (`order_api.py:89-92,965-968`); some GETs send `application/json` (`order_api.py:268,397`). History sends `Accept: application/json` (`data.py:1105`).
- Envelope: `{"s":"ok"|"no-data"|"error","d":...,"msg":...}`; `no-data` is an empty, non-error result (`order_api.py:44-62,233-236`).

### 4. REST endpoints
| Function | Method + path | Request | Response consumed | Cite |
|---|---|---|---|---|
| place | `POST /api/oms/place-order` (form) | `symId(brsymbol), qty, side(buy/sell), type, product, validity, [limitPrice], [trigPrice], [discQty], [remarks<=10ch], [amo:"true"], [mktProt:"2" for market/stopmarket]` | `d.orderId`, `d.msg` | `order_api.py:973-1023`, `transform_data.py:10-47` |
| modify | `PUT /api/oms/modify-order` (form) | `symId, orderId, qty(=filled+new), type, validity, side, [limitPrice], [trigPrice], [discQty], [mktProt]` | `d.orderId` | `order_api.py:1601-1615`, `transform_data.py:50-93` |
| cancel | `DELETE /api/oms/cancel-order?orderId=` | query | `d.orderId` | `order_api.py:1402-1428` |
| cancel-all | orderbook -> status in `OPEN, TRIGGER PENDING, MODIFIED, PENDING` -> cancel each | | | `order_api.py:1436-1554` |
| orderbook | `GET /api/oms/orders?symDetails=true` | | `d[].{sym{id,exchange,tradSymbol}, symId, qty, side, type, product, orderId, orderTime, status, avgPrice, limitPrice, trigPrice, fillQty, pendingQty, validity, validTill, reason}` | `order_api.py:169-224` |
| tradebook | `GET /api/oms/trades?symDetails=true` | | `d[].{sym, symId, product, side, fillQty, fillPrice, fillValue, orderId, time, exchOrderId, remarks}` | `order_api.py:272-350` |
| positions | `GET /api/oms/positions?symDetails=true` (timeout 10s) | | `d[].{sym, symId, netQty, netAvgPrice, product, realizedPnl, dayPos{dayQty,dayAvg,dayRealizedPnl}}` | `order_api.py:400-508` |
| close-all | positions -> MARKET order opposite side per non-zero `quantity` | | | `order_api.py:1257-1377` |
| holdings | `GET /api/oms/holdings?symDetails=true` | | `d.holdings[].{sym, symId, qty|saleableQty, avgPrice, realizedPnl, product}`; no LTP in payload | `order_api.py:574-629`, `order_data.py:569-581` |
| funds | `GET /api/oms/limits` | | `d` object or per-segment array (numeric fields summed): `availMargin, stockCollateral, unrealizedPnL, realizedPnl, marginUsed` | `funds.py:45-90` |
| margin calc | not supported: `NotImplementedError` | | | `margin_api.py:6-22` |
| quote / multiquote / depth | **WebSocket only** (no REST quote): connect `NxtradStream`, `subscribeL1([token_EXCH])` or `subscribeL2`, wait, close | L1 quote complete when `ltp,open,high,low,close,vol,bidPrice,askPrice` (+`OI` on NFO/BFO/CDS/BCD/MCX) present; quote waits up to 40x1s after a 3s settle; multiquote batch 100, wait 2-10s (0.05s/sym); depth waits 20s | `data.py:82-111,443-595,597-793,795-858` |
| history | `GET /api/mkt-data/chart/interval-data?id=<brsymbol>&interval=&from=<epoch s>&to=` | | `d.bars[]` as `{time,open,high,low,close,volume}` or `[t,o,h,l,c,v]` | `data.py:1076-1185` |
| master | `GET /api/mkt-data/scrips/symbol-store?version=0` and `/symbol-store/{group}?version=0` (unauthenticated) | | see 6 | `master_contract_db.py:93-151` |

### 5. Enum mappings and formulas
- Exchange: OpenAlgo code used as-is; `_INDEX` stripped for WS keys (`data.py:476-477`). WS segment ids `1 NSE,2 BSE,3 NFO,4 BFO,5 CDS,6 BCD,7 MCD,8 MCX,9 NCO,10 BCO` (`streaming/tradejini_mapping.py:24-35`).
- Product OA->broker: `CNC->delivery, NRML->normal, MIS->intraday` (default intraday); reverse adds `cover->CO, bracket->BO, margin->NRML` (`transform_data.py:109-114,141-159`).
- Pricetype: `MARKET->market, LIMIT->limit, SL->stoplimit, SL-M->stopmarket`; reverse inverse (`:96-106,162-174`). Action: `buy`/`sell` lowercase (`:22`).
- Validity: `DAY->day, IOC->ioc, GTC->gtc, EOS->eos` (eos only on BSE/BFO/BCD, else day) (`:117-138`).
- Order status broker->OA: `completed/traded/filled/complete -> complete; open/pending -> open; trigger pending -> trigger pending; rejected; cancelled/canceled -> cancelled` (`order_data.py:85-97`).
- Funds: `availablecash=availMargin, collateral=stockCollateral, m2munrealized=unrealizedPnL, m2mrealized=realizedPnl, utiliseddebits=marginUsed` (`funds.py:84-90`).
- Holdings pnl = `(qty*ltp - qty*avgPrice) + realizedPnl`, ltp falls back to avgPrice (`order_data.py:569-581,738-750`).
- Price scaling (WS only): per-segment divisor `NSE/BSE/NFO/BFO/MCX 100`, `CDS 1e7`, `BCD/MCD/NCO/BCO 1e4`; `chngPer`/`OIChngPer` always /100 (`streaming/nxtradstream.py:50-61,77,87`). REST values are plain decimals.

### 6. Master contract
- Groups from `symbol-store` -> `d.symbolStore[]` with `name` and `idFormat` (e.g. `instrument_symbol_exchange_expiry_strike_optType`); each group CSV fetched and split on `,` (`master_contract_db.py:98-151,154-172`).
- Groups: `Securities` (id `instrument_symbol_series_exchange`) -> `symbol=dispName`, instrumenttype `EQ`; `FutureContracts/CurrencyFuture/CommodityFuture` -> `{symbol}{DDMMMYY}FUT`; `NSEOptions/BSEOptions/CurrencyOptions/CommodityOptions` -> `{symbol}{DDMMMYY}{strike}{CE|PE}` (integer strike without `.0`); `Index` -> exchange `NSE_INDEX/BSE_INDEX`, `token=excToken`, instrumenttype INDEX; spot rows skipped (`:219-401`).
- Columns stored: `brsymbol = id` (e.g. `EQT_RELIANCE_EQ_NSE`), `token = excToken`, `brexchange = exchange`, `expiry = DD-MMM-YY`, `lotsize = lot`, `tick_size = tick`, `name` = underlying root for derivatives (`:274-287,314-328,381-394`).
- Expiry parsed from `%Y-%m-%d` (ids) or `%d%b%Y/%d%b%y/%d-%b-%Y/%d-%b-%y` (`:175-192`).
- Index renames: NSE map (`India VIX->INDIAVIX`, `Nifty 100->NIFTY100`, ...), BSE map (`SNXT50->BSESENSEXNEXT50`, `BSE HC->BSEHEALTHCARE`, ...); unmapped -> uppercase with spaces removed (`:411-500`).
- Table deleted first, then inserted group by group with dedupe on token (`:66-89,505-547`).

### 7. History
- Supported intervals advertised `1m,5m,30m` (`data.py:338-342`); request map `1m->1, 5m->5, 15m->15, 30m->30`, others passed through (`:999-1000`). No daily/weekly handling.
- Timestamps: start -> 09:15:00 IST, end -> 23:59:59 IST, sent as epoch seconds `from/to`; bars `time` epoch s (ms tolerated) -> returned epoch s (`:951-970,1022-1041,1052-1063`). Single request, no chunking; `no-data` -> empty frame (`:1128-1130`). `id` param is the brsymbol (`:1094-1101`).

### 8. Streaming (NxtradStream, binary)
- URL `wss://api.tradejini.com/v2.1/stream?token=<api_key>:<access_token>&version=3.1` (`streaming/nxtradstream.py:175,195`; `tradejini_adapter.py:84-118`). Auth is in the URL; server replies with pkt type 13 `auth_status` (1 = ok) (`:132-134`, adapter `:570-576`).
- Requests are JSON + `"\n"`: `{"type":"L1"|"L5"|"L1S"|"L5S"|"OHLC"|"greeks"|"event"|"PING","action":"sub"|"unsub","tokens":[{"t":"<token>_<EXCH>"}], ["chartInterval"]}`; a `sub` **replaces** the server-side list for that feed, `unsub` clears the whole feed (`:208-369`, adapter `:252-267`). Adapter coalesces per-feed re-sends with 0.25s debounce (`adapter.py:27,269-388`).
- Frame (little-endian native structs): bytes `[0:4]` int32 total len, `[4]` int8 version (must be 1), `[5]` int8 compression (100 = zlib on `[6:]`), then packets. Each packet: `[0:2]` int16 pktLen, `[2]` int8 pktType, then from offset 3 repeated `uint8 fieldKey` + value (`:613-634,395-396,439-455`).
- pktType: `10 L1, 11 L5, 12 OHLC, 13 auth, 14 marketStatus, 15 EVENTS, 16 PING, 17 greeks` (`:62`).
- L1 field keys: `26 exchSeg(B), 27 token(i), 28 precision(B), 29 ltp, 30 open, 31 high, 32 low, 33 close, 34 chng, 35 chngPer, 36 atp, 37 yHigh, 38 yLow (i, /divisor), 39 ltq(<I), 40 vol(<I), 41 ttv(d), 42 ucl, 43 lcl, 44 OI(<I), 45 OIChngPer, 46 ltt(i epoch), 49 bidPrice, 50 qty, 51 no, 52 askPrice, 53 qty, 54 no, 55 nDepth(B), 56 nLen(H), 58 prevOI, 59 dayHighOI, 60 dayLowOI, 70 spotPrice, 71 dayClose, 74 vwap` (`:67-103`). Symbol = `"<token>_<exchSeg>"`; L1 packets are deltas merged into a per-symbol cache (`:423-429,460`).
- L5 keys: `26,27,28, 47 totBuyQty, 48 totSellQty, 55 nDepth`, then `nDepth` x `{49 price,50 qty,51 no}` bids followed by `{52 price,53 qty,54 no}` asks (`:104-117,465-512`). Greeks (17): doubles `63 itm,64 iv,65 delta,66 gamma,67 theta,68 rho,69 vega,72 highiv,73 lowiv` (`:148-160`).
- Modes: 1 LTP and 2 Quote both ride L1; 3 Depth rides L5 (5 levels only) (`adapter.py:471-472,412-415`). Reconnect: exp backoff 5s*2^(n-1) cap 60s, max 10, single attempt in flight, replay both feeds on `connected` (`adapter.py:35-38,142-238,529-558`). No order-update stream (REST poller only).

### 9. Quirks
- Quote/depth/multiquote open a fresh WS per request and close it in `finally`; gthread limits concurrent feed waiters to 8 (`data.py:29-66,590-595`).
- Modify sends total qty = `filled_quantity + quantity` (`transform_data.py:61-64`).
- `mktProt` fixed at `"2"` for market/stopmarket on place (`transform_data.py:38-40`); remarks truncated to 10 chars (`:29-31`).
- Symbol object field names vary (`id|symId`, `exchange|exch`, `symbol|sym`, `tradSymbol|trdSym|dispSymbol|dispSym`) (`order_data.py:13-48`).
- `/api/oms/limits` documented as object but returns per-segment array in practice (`funds.py:62-77`). `api/nxtradstream.py` and `streaming/nxtradstream.py` are identical copies (diff: one blank line).

---

### fivepaisa

Paths below are relative to `/Users/openalgo/openalgo-desktop/openalgo/broker/fivepaisa/` unless absolute.

### 1. Family / template
- Direct login + TOTP family (SKILL.md:47 lists fivepaisa with angel/mstock/...); closest template is `broker/angel/` (same TOTP React page flow, `blueprints/brlogin.py:74-85` mirrors angel at :87-99). Unlike angel it returns only one token (no feed token) and every REST body is wrapped in a `{"head": {"key": api_key}, "body": {...}}` envelope.
- `plugin.json:8-10`: supported_exchanges NSE, BSE, NFO, BFO, CDS, MCX, NSE_INDEX, BSE_INDEX; `broker_type: "IN_stock"`, `leverage_config: false`.

### 2. Auth flow
- Env: `BROKER_API_KEY = "api_key:::user_id:::client_id"` (split at `api/auth_api.py:35`, error text :39); `BROKER_API_SECRET` = 5paisa "EncryKey" (:28, :78). `utils/config.py:11-28` only exposes raw getters; no fivepaisa-specific parsing there.
- Login form (`blueprints/brlogin.py:74-85`): GET redirects to `/broker/fivepaisa/totp`; POST fields `userid`|`clientid` (clientcode = 5paisa login email, `auth_api.py:17`), `pin`, `totp`. `auth_function(clientcode, broker_pin, totp_code)` -> `(auth_token, error)`.
- Step 1 `POST https://Openapi.5paisa.com/VendorsAPI/Service1.svc/TOTPLogin` (`auth_api.py:54-58`), headers `Content-Type: application/json`, `Accept: application/json` (:42), body:
  ```json
  {"head": {"Key": "<api_key>"}, "body": {"Email_ID": "<clientcode>", "TOTP": "<totp>", "PIN": "<pin>"}}
  ```
  Response consumed: `body.RequestToken` (:62); error text in `body.Message` (:70).
- Step 2 `POST .../VendorsAPI/Service1.svc/GetAccessToken` (:83-87), body:
  ```json
  {"head": {"Key": "<api_key>"}, "body": {"RequestToken": "<RequestToken>", "EncryKey": "<BROKER_API_SECRET>", "UserId": "<user_id>"}}
  ```
  Response consumed: `body.AccessToken` (:96-97) -> stored as the OpenAlgo auth token. No feed token (brlogin only unpacks 2 values).
- Token is a JWT; its `RedirectServer` claim (A/B/C) shards the WebSocket host (`streaming/fivepaisa_websocket.py:27-47`). Expiry: daily (adapter re-reads a fresh token before every reconnect, `fivepaisa_adapter.py:145-159`).

### 3. Base URL / headers
- `BASE_URL = "https://Openapi.5paisa.com"` (`api/order_api.py:33`, `api/data.py:64`); master contract uses lowercase `https://openapi.5paisa.com` (`database/master_contract_db.py:404`).
- Per call: `Authorization: bearer <AccessToken>` (lowercase "bearer", `order_api.py:61`), `Content-Type: application/json`. Every body is `{"head": {"key": api_key}, "body": {...}}` (`order_api.py:40`; note lowercase `key` here vs `Key` in login). Account calls carry `body.ClientCode = client_id`.

### 4. REST endpoints (all POST to `BASE_URL + path` unless noted)
| Purpose | Path | Body (inside `body`) | Response fields consumed | Cite |
|---|---|---|---|---|
| Place | `/VendorsAPI/Service1.svc/V1/PlaceOrderRequest` | see transform in sec. 5 | `head.statusDescription=="Success"` AND `body.Status==0` AND `body.BrokerOrderID!=0` -> orderid; else `body.Message`/`RMSResponseCode` rejection, status forced 400 | `order_api.py:366-418` |
| Modify | `/VendorsAPI/Service1.svc/V1/ModifyOrderRequest` | `{ExchOrderID, Price, Qty, StopLossPrice, DisQty}` (looked up from orderbook by `BrokerOrderId`) | `head.status=="0"` success; `body.BrokerOrderID` | `order_api.py:681-739`, `transform_data.py:213-233` |
| Cancel | `/VendorsAPI/Service1.svc/V1/CancelOrderRequest` | `{ExchOrderID}` (found via orderbook; refuses when `OrderStatus=="Pending"` and no ExchOrderID) | `head.statusDescription=="Success"` | `order_api.py:596-660` |
| Cancel all | loop over orderbook rows with `OrderStatus in ["Pending","Modified"]` -> cancel_order(BrokerOrderId) | | | `order_api.py:760-793` |
| Orderbook | `/VendorsAPI/Service1.svc/V3/OrderBook` | `{ClientCode}` | `body.OrderBookDetail[]` | `order_api.py:98-101` |
| Tradebook | `/VendorsAPI/Service1.svc/V1/TradeBook` | `{ClientCode}` | `body.TradeBookDetail[]` | `order_api.py:117-120` |
| Positions | `/VendorsAPI/Service1.svc/V2/NetPositionNetWise` | `{ClientCode}`; 60s timeout, 3 attempts | `body.NetPositionDetail[]` (null when flat) | `order_api.py:143-168` |
| Holdings | `/VendorsAPI/Service1.svc/V3/Holding` | `{ClientCode}` | `body.Data[]` | `order_api.py:200-203` |
| Close all | positions -> MARKET order per non-zero `NetQty` (SELL if >0) | | | `order_api.py:528-580` |
| Funds | `/VendorsAPI/Service1.svc/V4/Margin` | `{ClientCode}` | `body.EquityMargin[0]` | `api/funds.py:56-74` |
| Margin calc | not supported (raises NotImplementedError) | | | `api/margin_api.py:21-22` |
| Quotes | `/VendorsAPI/Service1.svc/MarketSnapshot` | `{ClientCode, Data:[{Exchange, ExchangeType, ScripCode, ScripData}]}`; `ScripData=brsymbol` only when token=="0" | `body.Data[0]`: LastTradedPrice, Open, High, Low, PClose (fallback PreviousClose, Close), Volume, OpenInterest | `data.py:381-452` |
| Depth | `/VendorsAPI/Service1.svc/V2/MarketDepth` | `{ClientCode, Exchange, ExchangeType, ScripCode, ScripData}` | `body.MarketDepthData[]` with `BbBuySellFlag` 66=Buy/83=Sell, `Price`, `Quantity` | `data.py:154-206`, `288-335` |
| Multiquotes | MarketSnapshot with up to 50 scrips per `Data` array, 0.5 s between batches | | same as quotes; bid/ask always 0 | `data.py:473-474`, `577-643` |
| History | GET `/V2/historical/{Exch}/{ExchType}/{token}/{interval}?from=YYYY-MM-DD&end=YYYY-MM-DD` | | `status=="success"`, `data.candles[]` = `[ts, o, h, l, c, v]` | `data.py:796-818` |

### 5. Enum mappings
- Action: BUY->`OrderType:"B"`, SELL->`"S"` (`transform_data.py:236-241`).
- Exchange -> `Exchange`/`ExchangeType`: NSE N/C, BSE B/C, NFO N/D, BFO B/D, CDS N/U, BCD B/U, MCX M/D, NSE_INDEX N/C, BSE_INDEX B/C (`transform_data.py:244-277`). Reverse `(Exch,ExchType)` -> exchange at :314-326.
- Product: `IsIntraday = (product=="MIS")` (`transform_data.py:204`); `map_product_type` CNC->D, NRML->D, MIS->I (:293-302); reverse D->CNC on NSE/BSE else NRML, I->MIS (:329-344).
- Pricetype: 5paisa has no order-type field. `Price=0` means market; `StopLossPrice` = trigger. MARKET is converted to an MPP-protected LIMIT off the LTP (`transform_data.py:111-164`, uses `utils/mpp_slab`), SL-M to a stop-LIMIT one tick/MPP% beyond the trigger (:166-190, `_slm_protected_price` :35-92) because the API rejects "Market order with Algo Id not allowed" (:39-44). Validity: none sent (`AHPlaced:"N"`, `RemoteOrderID:"OpenAlgo"`, `DisQty`) (:193-208).
- Place body literal:
  ```json
  {"OrderType":"B","Exchange":"N","ExchangeType":"C","ScripCode":1660,"Price":2.5,"Qty":1,"StopLossPrice":0.0,"DisQty":0,"IsIntraday":true,"AHPlaced":"N","RemoteOrderID":"OpenAlgo"}
  ```
- Order status (lowercased) -> OpenAlgo (`mapping/order_data.py:43-78`): fully executed->complete; pending/open/modified/placed/ah placed/ah modified/xmitted->open; cancelled/canceled/ah cancelled->cancelled; rejected by 5p / rejected by exch->rejected; fallback substring "rejected"/"cancel".
- Orderbook pricetype derivation (`order_data.py:208-218`): `AtMarket=="Y"` & trigger 0 -> MARKET; `N` & 0 -> LIMIT; `Y` & >0 -> SL-M; `N` & >0 -> SL. Fields: `Qty` (order qty, not TradedQty), `Rate` price, `SLTriggerRate`, `BrokerOrderId` (string), `BrokerOrderTime` in MS JSON `/Date(ms+0530)/` (:12-30), `Reason`. BuySell B/S -> BUY/SELL (:153-158).
- Positions: `NetQty`, avg = `BuyAvgRate` if net>0 else `SellAvgRate` (`order_data.py:395-400`); product from `OrderFor` (:370-381). Holdings: `Exch` N/B->NSE/BSE, `AvgRate`, `CurrentPrice`, `Quantity`, `Symbol`, product always CNC (:437-495).
- Funds (`api/funds.py:89-95`): availablecash=`NetAvailableMargin`, collateral=`TotalCollateralValue`, utiliseddebits=`MarginUtilized`; m2munrealized=sum(`MTOM`) and m2mrealized=sum(`BookedPL`) over NetPositionDetail. No price scaling anywhere (rupees as floats).

### 6. Master contract
- URL `https://openapi.5paisa.com/VendorsAPI/Service1.svc/ScripMaster/segment/all` -> CSV to `tmp/5paisa.csv`, streamed with 120 s timeout, 3 retries (`master_contract_db.py:87-156`, :404).
- Columns used: `Exch`, `ExchType`, `ScripCode`, `Series`, `ScripType`, `Expiry`, `StrikeRate`, `SymbolRoot`, `Name`, `FullName`(unused), `LotSize`, `TickSize` (:169-260).
- Exchange: (N,C)->NSE or NSE_INDEX if `ScripCode > 999900`; (B,C)->BSE/BSE_INDEX same rule; others via (Exch,ExchType) table (:170-191). Filter `Series in [EQ, BE, XX, "  "]`, XX/blank Series replaced by `ScripType` (CE/PE/XX) (:194-196).
- Expiry stored `DD-MMM-YY` upper (:201-204); strike trailing `.0`/`.00` stripped (:207-221). Symbols: EQ/BE -> `SymbolRoot`; XX -> `Root+DDMMMYY+FUT`; CE/PE -> `Root+DDMMMYY+Strike+CE|PE` (:227-238). `instrumenttype` XX->FUT (:259). `brsymbol = Name.upper().rstrip()`, `brexchange = exchange`, `token = ScripCode` (:242-248). `name` = SymbolRoot for derivatives, symbol otherwise (:371-377).
- Index normalisation: uppercase, strip spaces/hyphens, then rename map (NIFTY50->NIFTY, NIFTYBANK->BANKNIFTY, NIFTYFINSERVICE->FINNIFTY, NIFTYMIDSELECT->MIDCPNIFTY, SNSX50->SENSEX50, BSEBANKEX->BANKEX, ...) (:263-351); duplicates on (symbol, exchange) dropped for index rows (:380-384).

### 7. History
- `map_interval` (`data.py:672-684`): 1m->1m, 5m->5m, 10m->10m, 15m->15m, 30m->30m, 1h->60m, D/d/1d->1d; anything else raises (:754-758). Supported list `["1m","5m","10m","15m","30m","1h","D"]` (:1013).
- Chunking: 100 days for daily, 30 days intraday (:775-780). URL per chunk per sec. 4. Candle `[0]` parsed `%Y-%m-%dT%H:%M:%S` as IST wall clock (:835); intraday localised Asia/Kolkata -> epoch seconds (:894-896); daily -> midnight UTC epoch (:879-887) then re-normalised to date midnight with +5:30 (:949-959). Filters: index rows only drop all-zero OHLC; daily non-index drop volume==0 or high==low; intraday non-index drop all-zero only (:853-876). `oi` column always 0 (:982). Output column order `close,high,low,open,timestamp,volume,oi` (:985). Index exchange normalisation via exact-name list (`data.py:37-60`).

### 8. Streaming (`streaming/fivepaisa_websocket.py`, `fivepaisa_adapter.py`, `fivepaisa_mapping.py`)
- URL by JWT `RedirectServer`: A `wss://aopenfeed.5paisa.com/feeds/api/chat`, B `wss://bopenfeed...`, C `wss://openfeed...`, default `wss://openfeed.5paisa.com/Feeds/api/chat` (`fivepaisa_websocket.py:19-24`). Connect `?Value1={access_token}|{client_code}` (:164); no auth frame. client_code = third `:::` part of BROKER_API_KEY (`fivepaisa_adapter.py:125-143`). `run_forever(ping_interval=10, ping_timeout=5, CERT_NONE)` (:180-184).
- Subscribe frame (JSON text, :226-233):
  ```json
  {"Method":"MarketFeedV3","Operation":"Subscribe","ClientCode":"<client>","MarketFeedData":[{"Exch":"N","ExchType":"C","ScripCode":1660}]}
  ```
  `Operation:"Unsubscribe"` to drop. Methods: `MarketFeedV3` (modes 1/2), `MarketDepthService` (mode 3), `GetScripInfoForFuture` (OI, unused) (`fivepaisa_mapping.py:80-84`). Depth mode subscribes BOTH MarketDepthService and MarketFeedV3 because depth frames carry no LTP (`fivepaisa_adapter.py:490-513`). Exchange codes for streaming: `fivepaisa_mapping.py:9-31` (CDS->N/U, MCX->M/D). Depth level only 5 (:77).
- Frames are JSON (object or array of objects, :285-289). Quote keys consumed (`fivepaisa_adapter.py:785-807`): `Token`, `LastRate`, `TickDt` (`/Date(ms)/` -> ms, :856-877), `TotalQty`, `OpenRate`, `High`, `Low`, `PClose`, `LastQty`, `AvgRate`, `TBidQ`, `TOffQ`, `BidRate`, `BidQty`, `OffRate`, `OffQty`. Depth keys: `TBidQ`, `TOffQ`, `Time`, `Details[]` with `BbBuySellFlag` 66/83, `Price`, `Quantity`, `NumberOfOrders` (:808-854). Snapshot merge: zero/None values of LastRate/OpenRate/High/Low/PClose/BidRate/OffRate/AvgRate replaced by last non-zero per `(token,mode)` (:727-772).
- Batching: up to 50 scrips per frame, 0.5 s between frames, 0.5 s collect window (:31-46). Reconnect: single driver thread, backoff `5*2^n` capped 60 s, 10 attempts, fresh token re-read each attempt (:55-58, :208-260). Resubscribe deduped on (method, token) on open (:619-647). ZMQ topic `{exchange}_{symbol}_{LTP|QUOTE|DEPTH}` (:695-696).
- Order-update stream: `OrderTradeConfirmations` adapter exists (`fivepaisa_order_adapter.py`) but is NOT registered; 5paisa allows one feed connection per token and evicts the other, so fivepaisa uses REST polling (`:5-28`). Documented payload: `ReqType` P/M/C carry `BrokerOrderID`, T/S carry only `ExchOrderID` (:53-68); status map :124-138; subscribe frame `{"Method":"OrderTradeConfirmations","Operation":"Subscribe","ClientCode":...}` (:201-209).

### 9. Rate limits / quirks
- Multiquote batch 50, 0.5 s sleep (`data.py:473-474`); API returns empty for 100+ per request (:472).
- Positions call needs 60 s timeout and 3 retries; null `NetPositionDetail` when flat (`order_api.py:127-168`, `funds.py:78-82`).
- HTTP 200 with `statusDescription=="Success"` does not mean accepted; check `body.Status==0` and `BrokerOrderID!=0` (`order_api.py:388-418`). `head.key` is the plaintext API key, so only `body` is logged (:354-359).
- Cancel/modify need `ExchOrderID` resolved from the orderbook; orders still "Pending" at the broker cannot be cancelled (:615-627).
- MARKET and SL-M orders never go out as Price=0; they are MPP-protected limits (sec. 5). Tick size comes from the SymToken DB, not the API (`transform_data.py:125-129`).
- Smart order uses per-symbol locks and a position cache (`order_api.py:214-266`); position match on `ScripCode`, `Exch`, `ExchType`, `OrderFor` (:311-330).

---

### nubra

Paths below are relative to `/Users/openalgo/openalgo-desktop/openalgo/broker/nubra/` unless absolute.

### 1. Family / template
- Direct login family (SKILL.md:47), but the default path is a two-step phone SMS-OTP + MPIN flow rather than TOTP; a TOTP variant exists (`api/auth_api.py:261-328`) for TOTP-enrolled accounts. Closest template `broker/angel/` for the login UI; REST shape is Nubra's own V3 "intent order" API (`mapping/transform_data.py:2-10`). Streaming is binary protobuf (unique among direct-login brokers).
- `plugin.json:8-10`: NSE, BSE, NFO, BFO, MCX, NSE_INDEX, BSE_INDEX; `broker_type: "IN_stock"`, `leverage_config: false`.

### 2. Auth flow
- Env: `BROKER_API_KEY` = registered mobile number, `BROKER_API_SECRET` = MPIN (`auth_api.py:130-131`). No `:::` composite. `NUBRA_USE_UAT=1|true|yes` switches host (`api/baseurl.py:59-62`).
- Every authenticated call sends `Authorization: Bearer <session_token>`, `Accept: application/json`, `x-device-id: OPENALGO` (constant `DEVICE_ID`, must equal the id used at login), `Content-Type: application/json` when a body is sent (`baseurl.py:30`, :76-91). HTTP 440 = session expired, never retried (`baseurl.py:36-41`).
- Login (`blueprints/brlogin.py:638-673`): GET calls `request_login_otp()` -> stores `temp_token` + masked phone in Flask session -> redirect `/broker/nubra/totp`; POST reads `otp`|`totp`, pops the single-use temp_token, calls `auth_function(otp_code, temp_token)` -> `(session_token, None, error)`.
- Step 1 `POST {base}/sendphoneotp`, headers `Content-Type` only (no device id), body `{"phone": "<phone>", "flow": "", "skip_totp": false}` (`auth_api.py:358-365`). Response: `temp_token`, `next` (`VERIFY_MOBILE` or `VERIFY_TOTP`), `message`, `expiry`, `attempts_left` (:352-354).
- Step 2 (only when `next=="VERIFY_TOTP"`): same endpoint with header `x-temp-token: <temp_token>` and `skip_totp: true` to force SMS (:177-182). Any other `next` is an error (:183-187).
- Step 3 `POST {base}/verifyphoneotp`, headers `Content-Type`, `x-temp-token`, `x-device-id`, body `{"phone": "<phone>", "otp": "<otp>"}` -> `auth_token` (:393-408).
- Step 4 `POST {base}/verifypin`, headers Bearer `<auth_token>` + `x-device-id` (no x-temp-token), body `{"pin": "<mpin>"}` -> `session_token` (:100-120). `session_token` is what OpenAlgo stores as auth token; no feed token (:254).
- TOTP alternative: `POST /totp/login` body `{"phone", "totp": <int or zero-padded str>, "otp": ""}` -> `auth_token`, then step 4 (:49-87, :261-328). Enrolment helpers `GET /totp/generate-secret`, `POST /totp/enable {"mpin","totp"}`, `POST /totp/disable {"mpin"}` (:456-544).
- `GET /userinfo` -> `env_info.user_ws_url` / `env_info.market_ws_url` (:550-584, :636-649). `GET /ipaddress/validate` -> `is_matched`, `current_ip_address`, `primary_ip_address`, `secondary_ip_address` (:587-633; UI route `blueprints/brlogin.py:1199-1250`, read-only).

### 3. Base URLs
- PROD `https://api.nubra.io`, UAT `https://uatapi.nubra.io` (`baseurl.py:22-23`); market WS `wss://api.nubra.io/apibatch/ws` (`api/nubrawebsocket.py:38`); order WS from `/userinfo`, fallback env `NUBRA_ORDER_WS_URL`, default `wss://uatapi.nubra.io/ws` (`streaming/nubra_order_adapter.py:36`, :128-143). Index master `GET /public/indexes?format=csv` unauthenticated (`baseurl.py:26`).

### 4. REST endpoints (headers per sec. 2; every write wraps items in a top-level `orders` array)
| Purpose | Method/Path | Request | Response consumed | Cite |
|---|---|---|---|---|
| Place | POST `/sentinel/orders/create` | `{"orders":[item]}` (sec. 5) | 200/201 and `orders[0].intentOrderId` -> orderid; rejection text in `error` | `api/order_api.py:341-410` |
| Modify | POST `/sentinel/orders/modify` | `{"orders":[{orderId, qty, deliveryType, priceType, validityType, executionMode, entryPrice?, entryConfig?}]}` | 200/201; `orders[0].intentOrderId` | `order_api.py:675-752`, `transform_data.py:146-175` |
| Cancel | POST `/sentinel/orders/cancel` | `{"orders":[{"orderId": <int>}]}` | 200/201/204 success | `order_api.py:604-672` |
| Cancel all | orderbook buckets `open` + `gtt` -> cancel each, 100 ms apart | | | `order_api.py:755-804` |
| Orderbook / Tradebook | GET `/sentinel/orders` | | `orders` dict keyed by bucket `open/executed/cancelled/expired/rejected/gtt` | `order_api.py:172-191`, `mapping/order_data.py:16-44` |
| Positions | GET `/sentinel/portfolio/positions` | | `portfolio.positions[]` (`refId`, `symbol`, `exchange`, `derivativeType`, `deliveryType`, `netQty`|`netQuantity`, `avgPrice`, `avgBuyPrice`, `avgSellPrice`, `ltp`|`lastTradedPrice`, `pnl`, `pnlChg`) | `order_api.py:194-204`, `order_data.py:101-148`, :515-560 |
| Holdings | GET `/sentinel/portfolio/holdings` | | `portfolio.holdings[]` (`refId`, `symbol`, `exchange`, `quantity`, `avgPrice`, `lastTradedPrice`, `prevClose`, `investedValue`, `currentValue`, `netPnl`, `netPnlChg`, `dayPnl`), `portfolio.holdingStats` | `order_api.py:207-215`, `order_data.py:622-703` |
| Close all | positions -> MARKET order per non-zero net qty, 100 ms sleep, reports failures | | | `order_api.py:497-601` |
| Funds | GET `/sentinel/portfolio/user_funds_and_margin` | | `portFundsAndMargin.{netMarginAvailable,totalCollateral,netDerivativePrem,mtmEqIdayCnc,mtmEqDelivery,mtmDeriv,totalMarginBlocked}` | `api/funds.py:30-80` |
| Margin calc | POST `/sentinel/orders/funds_required` | `{"requestType":"NEW","orders":[place items]}` | `marginInfo.totalMargin`, `totalFundsRequired`, `brokerageInfo.totalChargesFloat`; `code==1` success | `api/margin_api.py:55-56`, `mapping/margin_data.py:18-172` |
| Quotes (REST fallback) | GET `/orderbooks/{ref_id}?levels=1` | | `orderBook.{bid[0].p, ask[0].p, ltp, ltq, volume, prev_close}` (paise); no OHLC/OI | `api/data.py:513-549` |
| Depth (REST fallback) | GET `/orderbooks/{ref_id}?levels=5` | | `orderBook.bid[]/ask[]` with `p`,`q`,`o`; `ltp`,`ltq`,`volume`,`high`,`low`,`open`,`prev_close`,`oi` | `data.py:1339-1398` |
| History | POST `/charts/timeseries` | sec. 7 | `message=="charts"`, `result[0].values[][brsymbol].{open,high,low,close,tick_volume}[].{ts,v}` | `data.py:983-1069` |
| Master | GET `/refdata/refdata/{YYYY-MM-DD}?exchange=NSE|BSE|MCX` | | `refdata[]` | `database/master_contract_db.py:104-116` |
Quotes/depth/multiquotes prefer the market WebSocket (sec. 8) and fall back to REST (`data.py:296-343`, :568-750, :1171-1214).

### 5. Enum mappings and payloads
- Place item (`transform_data.py:106-143`):
  ```json
  {"refId": 72329, "qty": 1, "side": "BUY", "deliveryType": "IDAY", "priceType": "LIMIT", "validityType": "DAY", "isMultiLeg": false, "executionMode": "ENTRY", "entryPrice": 127000, "stratTags": ["my-strategy"], "entryConfig": {"triggers": {"ltp": {"atOrAbove": {"value": 126000}}}}}
  ```
  `refId` = SymToken.token (Nubra ref_id). All prices are integer paise (`_paise`, :15-21). `entryPrice` only for LIMIT; `entryConfig` only for SL/SL-M (BUY -> `atOrAbove`, SELL -> `atOrBelow`, :87-103). `stratTags` single tag, `[^A-Za-z0-9]+` -> `-`, lowercased (:24-35). SL/SL-M without trigger is refused before the call (`order_api.py:330-338`).
- Action: BUY/SELL verbatim (:38-40). Product: CNC->CNC, MIS->IDAY, NRML->CNC (no NRML in V3) (:43-54); reverse CNC->CNC, IDAY->MIS (:57-59). Pricetype: MARKET->MARKET, LIMIT->LIMIT, SL->LIMIT+trigger, SL-M->MARKET+trigger (:62-76). Validity: IOC for MARKET, DAY otherwise (:79-84).
- Exchange (broker->OpenAlgo): Nubra only has NSE/BSE/MCX; `derivativeType in (OPT, FUT)` folds NSE->NFO, BSE->BFO; MCX stays (`transform_data.py:201-225`); `candidate_exchanges` probes both when derivativeType missing (:228-249). Resolution is confirmed against the master contract by `ref_id` then `stockName` (`order_data.py:60-87`).
- Orderbook: bucket -> status (`order_data.py:155-162`): open->open, executed->complete, cancelled->cancelled, rejected->rejected, expired->cancelled, gtt->open; open SL/SL-M -> "trigger pending" (:331-332). Pricetype from `priceType` + presence of `entryConfig.triggers.ltp.{atOrAbove|atOrBelow}.value` (:259-301). Fields: `intentOrderId`, `side`, `deliveryType`, `orderQty`, `filledQty`, `filledPrice`/100, `orderPrice`/100, `stratTags[0]`, `timestamps.{lastUpdatedAt,filledAt,sentToColoAt,intentCreatedAt}` RFC3339 (:165-209), `exchangeOrderIds` map (:361-376), instrument on `refData` or `legs[0].refData` (:212-256). Tradebook = orders with `filledQty>0` (:453-491).
- Funds (`funds.py:65-80`, all /100): availablecash=`netMarginAvailable`, collateral=`totalCollateral`, m2mrealized=`netDerivativePrem`, m2munrealized=`mtmEqIdayCnc+mtmEqDelivery+mtmDeriv`, utiliseddebits=`totalMarginBlocked`. Margin response: total=`totalFundsRequired` (fallback totalMargin), span=`totalMargin`, exposure=0 (`margin_data.py:164-172`).

### 6. Master contract (`database/master_contract_db.py`)
- Requires an active Nubra session: looks up `Auth(broker='nubra')` row for the token (:85-93). Three GETs `/refdata/refdata/{date}?exchange=X`, 120 s timeout, concatenated and saved as JSON `tmp/nubra_instruments.json` (:104-123). Index CSV `/public/indexes?format=csv` -> `tmp/nubra_indexes.csv` (:239-257).
- refdata columns: `ref_id` (token), `lot_size`, `tick_size`/100, `asset` (name), `stock_name` (brsymbol), `exchange` (brexchange), `derivative_type` (STOCK/FUT/OPT), `option_type` (CE/PE), `expiry` int `YYYYMMDD`, `strike_price`/100 (:150-198). Exchange: NSE+non-STOCK->NFO, BSE+non-STOCK->BFO, else unchanged (:160-167). instrumenttype FUT/CE/PE/EQ (:170-174). Expiry stored `DD-MMM-YY` upper (:191-195). Symbols: FUT `asset+DDMMMYY+FUT`; options `asset+DDMMMYY+strike(no .0)+CE|PE`; cash = brsymbol (:177, :207-222).
- Index CSV columns `EXCHANGE`, `INDEX_SYMBOL`, `ZANSKAR_INDEX_SYMBOL`, `INDEX_NAME` -> exchange `NSE_INDEX`/`BSE_INDEX`, brsymbol = token = ZANSKAR symbol, instrumenttype INDEX, tick 0.05 (:281-395). Rename map e.g. `INDIA_VIX->INDIAVIX`, `NIFTYMIDCAP->NIFTYMIDCAP100`, `SNXT50->BSESENSEXNEXT50`, `BSECG->BSECAPITALGOODS` (:306-374).

### 7. History (`api/data.py:828-1148`)
- Interval map (:209-227): 1s, 1m, 2m, 3m, 5m, 15m, 30m, 1h, D->1d, W->1w, M->1mt. Chunk days (:912-925): 1s 7, 1m/2m 30, 3m/5m/15m 60, 30m/1h 90, D 365, W 1000, M 1500; 1.0 s sleep between chunks (60 req/min limit, :1084-1086).
- Query maps exchange -> (`exchange`, `type`): NSE_INDEX->NSE/INDEX, BSE_INDEX->BSE/INDEX, NFO->NSE/OPT|FUT (CE/PE in symbol), BFO->BSE/OPT|FUT, MCX->MCX/OPT|FUT, NSE/BSE->STOCK (:863-894). Body:
  ```json
  {"query":[{"exchange":"NSE","type":"STOCK","values":["<brsymbol>"],"fields":["open","high","low","close","tick_volume"],"startDate":"2026-01-01T00:00:00.000Z","endDate":"2026-01-30T23:59:59.000Z","interval":"1m","intraDay":false,"realTime":false}]}
  ```
  INDEX omits `tick_volume` (rejected with "invalid field tick_volume", :896-904). Whole UTC days requested, end clipped to now UTC (:958-974).
- Response values `{ts, v}` with `ts` in nanoseconds, prices /100 (:1040-1067, :1111); D/W/M normalised to midnight (:1114-1115); epoch seconds; `oi` always 0 (:1121). Rejections (`error` key) raised only if nothing was collected (:1014-1020, :1091-1099). No OI history (:1150-1169).

### 8. Streaming
Market data (`api/nubrawebsocket.py`, wrapped by `streaming/nubra_adapter.py`):
- Connect `wss://api.nubra.io/apibatch/ws` with headers `Authorization: Bearer <session_token>`, `x-device-id: OPENALGO`; `run_forever(ping_interval=20, ping_timeout=10)`; reconnect backoff `2*2^min(n-1,5)` capped 60 s, 50 attempts, caches cleared, token re-read via `token_provider` (:106-180).
- Subscribe/unsubscribe are TEXT frames (:580-643):
  - `batch_subscribe <token> index {"instruments":[],"indexes":["Nifty 50","TCS"]} NSE`
  - `batch_subscribe <token> index_bucket {"instruments":[],"indexes":[...]} 1m NSE` (OHLCV; adapter uses `1d`, `nubra_adapter.py:280-281`)
  - `batch_subscribe <token> orderbook {"instruments":[72329],"indexes":[]}` then `batch_subscribe <token> orderbook_depth 5` (required to start flow, :607-619)
  - `batch_subscribe <token> greeks {"instruments":[...],"indexes":[]}` (OI)
  - `batch_unsubscribe ...` same shapes. Index names for subscription: `SUBSCRIPTION_MAP` NIFTY->"Nifty 50", BANKNIFTY->"Nifty Bank", FINNIFTY->"Nifty Financial Services", SENSEX->"Bse Sensex", SENSEX50->"Bse Sensex 50" (:52-58); incoming `indexname` upper-cased and mapped back via `INDEX_NAME_MAP` (:42-48). Exchange for WS: NSE/BSE/MCX only; NFO->NSE, BFO->BSE, *_INDEX->NSE/BSE (`streaming/nubra_mapping.py:18-26`).
- Frames: binary protobuf, outer `google.protobuf.Any` whose `value` is an inner `Any`; dispatch on inner `type_url` suffix (:252-283). Text frame `Invalid Token` = token expired (:233-235). Messages (`protos/nubrafrontend_pb2.py`, package `protos.zanskarsecurities.nubrafrontend`; field numbers from descriptors):
  - `BatchWebSocketIndexMessage`: 1 timestamp int64, 2 indexes[], 3 instruments[] of `WebSocketMsgIndex` {1 indexname str, 2 timestamp int64, 3 index_value, 4 high_index_value, 5 low_index_value, 6 volume, 7 changepercent float, 8 tick_volume, 9 prev_close, 10 exchange str, 11 volume_oi} (int64 prices in paise, /100 at :315-329).
  - `BatchWebSocketOrderbookMessage`: 2 instruments[] of `WebSocketMsgOrderBook` {1 inst_id uint32, 2 timestamp, 3 bids[], 4 asks[] of `OrderBookLevel` {1 price, 2 quantity, 3 orders}, 5 ltp, 6 ltq, 7 volume, 8 ref_id} (:336-376; keyed by ref_id, falls back to inst_id).
  - `BatchWebSocketGreeksMessage`: 2 instruments[] of `WebSocketMsgOptionChainItem` {1 inst_id, 2 ts, 3 sp, 4 ls, 5 ltp, 6 ltpchg, 7 iv, 8-11 delta/gamma/theta/vega, 12 oi, 13 volume, 14 ref_id, 15 prev_oi, 16 price_pcp}; OI merged into depth cache (:378-402).
  - `BatchWebSocketIndexBucketMessage`: 2 indexes[], 3 instruments[] of `WebSocketMsgIndexBucket` {1 indexname, 2 exchange, 3 interval enum, 4 timestamp, 5 open, 6 high, 7 low, 8 close, 9 bucket_volume, 10 tick_volume, 11 cumulative_volume, 12 bucket_timestamp} (:404-451).
- Adapter modes: 1/2 use index channel (+index_bucket for open/close) for all symbols; 3 uses orderbook (instruments only, depth 5) and the orderbook callback also fans out LTP/QUOTE for non-index instruments (`nubra_adapter.py:343-396`, :789-896). Subscribe calls coalesced in a 0.5 s timer window into one call per (channel, exchange) (:126-134, :236-300). Topics `{exchange}_{symbol}_{LTP|QUOTE|DEPTH}`, depth payload `mode:"full"` with `depth.buy/sell` 5 levels `price/quantity/orders` (:881-896).
- Order updates (`streaming/nubra_order_adapter.py`): URL from `/userinfo env_info.user_ws_url`; headers Bearer + x-device-id; post-open text `subscribe <session_token> notifications notification` (:151-153). Frames: outer Any -> inner Any with type_url suffix `NubraToClientIntentUpdate`, decoded with a hand-written wire walker (:52-101) because the message is not in the bundled protos. Field numbers: update.1 = response; response.1 intentOrderId, 2 status (1 open, 2 complete, 3 rejected, 4 trigger pending, 5 cancelled, 6 expired), 7 deliveryType (1 CNC, 2 MIS), 8 priceType (1 LIMIT, 2 MARKET), 13 orderQty, 14 filledQty, 17 price paise, 18 avg paise, 19 tradeFill{2 price}, 25 refData{1 ref_id, 5 stock_name, 10 exchange, 11 derivativeType}, 29 side (1 BUY, 2 SELL), 31 rejection reason (:38-49, :187-248).

### 9. Rate limits / quirks
- 429 handled with exponential backoff 1 s * 2^attempt, 3 attempts (`order_api.py:33-34`, :131-142; `data.py:154-178`). Sequential loops paced at 10 ops/s (100 ms) (`order_api.py:30-32`, :580, :802). History 1 req/s (`data.py:1086`).
- HTTP 440 = re-login; `NubraSessionExpired` raised from data/funds so it is not reported as zero prices (`baseurl.py:42-55`, `data.py:185-187`, `funds.py:38-45`).
- V3 error body `{"error": "...", "nubra_error_code": ""}`; `error` is copied to `message` for the service layer (`order_api.py:99-111`).
- Live positions use `netQty/ltp` not the documented `netQuantity/lastTradedPrice`; both read (`order_data.py:101-117`). Single orders may come back `isMulti: true` with instrument on `legs[0]` (:212-231).
- Quote waits: WS poll 50 ms up to 2.0 s; depth 10 x 0.5 s; under gthread at most 8 concurrent feed waiters (`data.py:36-43`, :91-94). Index quotes come from 1m OHLCV candles and always wait the full window (:86-90).
- Static IP: `GET /ipaddress/validate` only verifies; no register/update API (`auth_api.py:587-596`). Nubra TOTP docs show `totp` as an int; leading-zero codes are retried as a string (:49-87).

---

### indmoney

All paths relative to `/Users/openalgo/openalgo-desktop/openalgo/`; `B` = `broker/indmoney/`.

### 1. Family / template
- **Direct login + TOTP** (MPIN + TOTP, no OAuth redirect), with an alternative "paste a 24h token" path. Closest template per `.claude/skills/broker-integration/SKILL.md:47` is `broker/angel/`; brlogin flow redirects to a React TOTP page like fivepaisa/mstock/nubra (`references/auth-and-login.md:191`). API host is INDstocks (`references/cross-broker-reference.md:19`).
- plugin.json (`B/plugin.json:8-10`): `supported_exchanges` = NSE, BSE, NFO, BFO, NSE_INDEX, BSE_INDEX; `broker_type` = `IN_stock`; `leverage_config` = false. No `api/baseurl.py` beyond `BASE_URL = "https://api.indstocks.com"` + `get_url()` (`B/api/baseurl.py:4-20`).

### 2. Auth flow
Credentials (`B/api/auth_api.py:13-18`, no composite `:::` form; `utils/config.py` has no indmoney-specific handling):
| Env | Meaning |
|---|---|
| `BROKER_API_KEY` | static **Client ID** from indstocks.com > API Trading > Access Tokens; sent as `x-api-key` |
| `BROKER_API_SECRET` | optional manually generated 24h access token; if set, used as-is and TOTP skipped |

Login dispatch (`blueprints/brlogin.py:496-553`):
1. GET `/<broker>/callback`: if `BROKER_API_SECRET` non-blank -> `authenticate_broker()` validates it (below); on rejection and a Client ID present -> redirect `/broker/indmoney/totp` (`:516-521`). If only Client ID -> redirect `/broker/indmoney/totp` (`:523-525`). Neither -> error (`:527-532`).
2. POST form fields `mpin`, `totp` (`:538-539`) -> `authenticate_broker_totp(mpin, totp_code)` returns `(auth_token, error)` (`:547`). No feed token; the single access token is used for REST and both WebSockets.

Step A, TOTP token mint (`B/api/auth_api.py:158-240`):
```
POST https://api.indstocks.com/generate/token
headers: {"x-api-key": <client_id>, "Content-Type": "application/json", "Accept": "application/json"}
body:    {"mpin": "<mpin>", "totp": "<6-digit string, leading zeros kept>"}
```
Success 200/201: token at `data.token` (also accepts `data.access_token`, top-level `token`/`access_token`) (`:210-219`). Errors: `message`/`error` fields; 429 = 1 req/60s throttle; 401/403 = bad id/MPIN/TOTP; 5 wrong codes/15 min -> 15-min lockout, 3 lockouts/hour -> 1-hour lockout; never auto-retry (`:35-38`, `:254-272`). Only one TOTP-generated token is live at a time (`:20-22`). Expiry: 24 hours (`:4`, `:47`); the WS token_provider notes a daily roll around ~3 AM IST (`B/streaming/indWebSocket.py:58-59`).

Step B, manual-token validation (`B/api/auth_api.py:43-106`): `GET /user/profile` with `Authorization: <token>` (raw, no Bearer); 200/201 valid; 401/403 rejected; other/network = indeterminate -> proceed with token (`:135-143`).

Stored auth token = the raw access token string. Every REST header is `Authorization: <token>` **without "Bearer"** (`B/api/order_api.py:43-47`, `B/api/data.py:60-64`, `B/api/funds.py:80`).

### 3. Base URLs / headers
- REST: `https://api.indstocks.com` (`B/api/baseurl.py:4`). Headers: `Authorization`, `Content-Type: application/json`, `Accept: application/json` (`B/api/order_api.py:43-47`).
- WS prices: `wss://ws-prices.indstocks.com/api/v1/ws/prices`; WS orders: `wss://ws-order-updates.indstocks.com/api/v1/ws/trades` (`B/streaming/indWebSocket.py:18-19`), header `Authorization: <token>` (`:322`).
- Response envelope: `{"status": "success"|"error"|"failure", "data": ..., "message"|"error": {"msg"}}`; `get_api_response` unwraps `data` on success (`B/api/order_api.py:98-110`); failure payload uses `error.msg` (`:100-101`, `:953-954`). Market endpoints may return `success: false` with no `status` (`:86-95`).

### 4. REST endpoints
| Op | Method/path | Request | Response consumed | Source |
|---|---|---|---|---|
| Place | `POST /order` (LIMIT/MARKET) or `POST /smart/order` (TRIGGER) | body below | `data.order_id`; smart: `data.order_data[n].order_id` (+`child_order_details.order_id`) | `order_api.py:644-718`, `:615-641` |
| Modify | `POST /order/modify` or `/smart/order/modify` | `{"segment","order_id","qty","limit_price"}`; smart adds `algo_id:"99999"`, `trigger_price`, `trigger_limit_price` (replaces `limit_price`) | `status=="success"` | `transform_data.py:232-243`, `order_api.py:961-1024` |
| Cancel | `POST /order/cancel` or `/smart/order/cancel` | `{"segment": "DERIVATIVE" if id startswith "DRV-" else "EQUITY", "order_id"}` | `status` | `order_api.py:909-958` |
| Cancel all | orderbook filter status in OPEN ∪ TRIGGER_PENDING, loop cancel | | | `:1027-1057` |
| Orderbook | `GET /order-book` | | list of orders | `:127-137` |
| Tradebook | `GET /trade-book?segment=EQUITY` + `?segment=DERIVATIVE`, merged and enriched from orderbook by `exch_order_id` | | `fill_id, exch_order_id, quantity, price, trade_date, scrip_code` | `:140-237`, `order_data.py:421-519` |
| Positions | `GET /portfolio/positions?segment=&product=` x4: (derivative,margin),(derivative,intraday),(equity,cnc),(equity,intraday); flat `data` list, rows tagged `query_segment/query_product`; then LTP via `GET /market/quotes/ltp?scrip-codes=SEG_token,...` | | `security_id, net_qty, average_price, realized_profit, exchange, segment` | `:334-430`, `:242-308` |
| Holdings | `GET /portfolio/holdings` | | `security_id, symbol, total_qty, avg_price, dp_qty, t1_qty, isin` | `:447-458`, `order_data.py:872-886` |
| Funds | `GET /funds` | | `sod_balance, withdrawal_balance, pledge_received, realized_pnl, unrealized_pnl, detailed_avl_balance.{eq_cnc,eq_mis,eq_mtf}` | `funds.py:83-141` |
| Margin | `GET /margin` **with JSON body** per leg: `{"segment","txnType","quantity"(str),"price"(str),"product","securityID","exchange"}`; aggregated across legs | | `data.total_margin, span_margin, exposure_margin` | `margin_api.py:64-100`, `margin_data.py:49-56,157-162` |
| Quote | `GET /market/quotes/full?scrip-codes=<SEG>_<token>`; fallback `/market/quotes/ltp` + `/market/quotes/mkt` | | `data[scrip].live_price|ltp, day_open, day_high, day_low, prev_close|close, volume, oi|open_interest, market_depth.depth[0].buy/sell.price` | `data.py:324-460` |
| Multiquote | `/market/quotes/full`, batch 500, comma-joined; bisection on "Invalid scrip" 400 with 300s poison cache | | same | `data.py:463-647`, `:17-50` |
| Depth | `/market/quotes/mkt` (+ `/full` for OHLC); `market_depth.depth[i].buy|sell.{price,quantity}` (5 levels), `market_depth.aggregate` totals; also tolerates nested `market_depth.<scrip>.depth` | | | `data.py:649-863`, `:278-301` |
| History | `GET /market/historical/<interval>?scrip-codes=&start_time=&end_time=` | see §7 | | `data.py:865-1050` |
| Profile | `GET /user/profile` | | status only | `auth_api.py:71-77` |

Place-order body (`transform_data.py:194-220`):
```json
{"txn_type":"BUY","exchange":"NSE","segment":"EQUITY","product":"INTRADAY","order_type":"LIMIT","validity":"DAY","security_id":"<token>","qty":1,"is_amo":false,"algo_id":"99999","limit_price":100.5}
```
TRIGGER body for SL/SL-M (`:175-187`): `order_type:"TRIGGER"`, `validity:"DAY"`, `trigger_price`, `trigger_limit_price`, `algo_id` — NSE only (`:157-161`). `algo_id` = `"99999"` NSE, `"9999999999999999"` BSE (`:140`). MARKET orders are converted to LIMIT at LTP ±0.1% when a session token is available (`:76-120`). Order IDs are prefixed `EQ-`/`DRV-`/`GTT-` (`order_api.py:589-591`). Smart-order detection for cancel/modify: `GTT-` prefix or orderbook `order_type` in {TRIGGER, OCO, GTT_*} (`:586-612`).

### 5. Enum mappings
OpenAlgo -> broker (`transform_data.py`):
| Field | Map |
|---|---|
| exchange -> `exchange` | NSE->NSE, BSE->BSE, NFO->NSE, BFO->BSE, CDS->NSE, BCD->BSE, MCX->MCX (rejected) (`:292-305`); placeable set NSE/BSE/NFO/BFO/CDS/BCD (`:132-138`) |
| exchange -> `segment` | NSE/BSE->EQUITY; NFO/BFO/CDS/BCD/MCX->DERIVATIVE (`:264-279`) |
| product | CNC->CNC, NRML->MARGIN, MIS->INTRADAY, default INTRADAY (`:316-325`) |
| pricetype | MARKET->MARKET, LIMIT->LIMIT, SL->TRIGGER, SL-M->TRIGGER (`:246-261`) |
| action | passthrough upper BUY/SELL (`:74`) |
| validity | DAY default; IOC if requested (`:200`, `:219-220`) |

Broker -> OpenAlgo (`order_data.py`):
- Status: complete={SUCCESS,TRADED,COMPLETE,EXECUTED}; open={QUEUED,O-PENDING,PENDING,PROCESSING,INITIATED,MODIFIED,PARTIALLY FILLED,PARTIALLY EXECUTED}; trigger pending={SL-PENDING}; rejected={REJECTED,FAILED,ABORTED}; cancelled={CANCELLED,EXPIRED,PARTIALLY FILLED - CANCELLED,PARTIALLY FILLED - EXPIRED} (`:12-32`, `:177-194`).
- Product: INTRADAY->MIS, CNC/DELIVERY->CNC, MARGIN->NRML; unknown -> NRML for derivatives else MIS (`:160-174`).
- Ordertype: MARKET, LIMIT, STOP_LOSS->SL, STOP_LOSS_MARKET->SL-M, TRIGGER->SL, GTT_LIMIT->SL, GTT_MARKET->SL-M, OCO->OCO (`:67-77`).
- Exchange resolution from `(exchange, segment)` pair: (NSE,EQUITY)->NSE, (BSE,EQUITY)->BSE, (NSE,DERIVATIVE)->NFO, (BSE,DERIVATIVE)->BFO; legacy NSE_EQ/NSE_FNO/NSE_FO/BSE_FNO...; else probe token against NFO/BFO or NSE/BSE via `get_symbol` (`:43-139`).
- Orderbook fields: `id, security_id, txn_type, product, order_type, status, requested_qty, requested_price, tgt_limit_price, sl_limit_price, sl_trigger_price, tgt_trigger_price, created_at, name` (`:240-270`).
- Funds formulas (`funds.py:125-162`): `availablecash` = `detailed_avl_balance.eq_cnc` (else eq_mis, eq_mtf, else `withdrawal_balance`); `collateral=pledge_received`; `m2mrealized=realized_pnl`; `m2munrealized=unrealized_pnl`; `utiliseddebits=max(0, sod_balance - availablecash)`; all `"%.2f"` strings.
- Prices: no scaling; numeric strings may carry commas, stripped by `_clean_number` (`data.py:303`) / `_first_price` (`order_data.py:141-157`).

### 6. Master contract (`B/database/master_contract_db.py`)
- Needs a live auth token (`:162-182`). URLs: `GET https://api.indstocks.com/market/instruments?source=<equity|fno|index>` with `Authorization` header, saved as `<segment>.csv` (`:184-212`).
- CSV columns used (`:404-420`): `EXCH, SEGMENT, SECURITY_ID, INSTRUMENT_NAME, EXPIRY_CODE, TRADING_SYMBOL, LOT_UNITS, CUSTOM_SYMBOL, EXPIRY_DATE, STRIKE_PRICE, OPTION_TYPE, TICK_SIZE, EXPIRY_FLAG, SEM_EXCH_INSTRUMENT_TYPE, SERIES, SYMBOL_NAME`.
- Stored: `token=SECURITY_ID` (str), `expiry` = EXPIRY_DATE formatted `%d-%b-%y` upper (e.g. `28-AUG-25`; `-1` if none), `strike`, `lotsize=LOT_UNITS` (default 1), `tick_size` (default 0.05), `brsymbol = TRADING_SYMBOL` (index file: `SEGMENT` column), `brexchange` = NSE/BSE (`:436-462`, `:482-495`).
- `exchange/brexchange/instrumenttype` (`:316-363`): index files -> `NSE_INDEX`/`BSE_INDEX`, type `INDEX`; SEGMENT `E` -> NSE/BSE `EQ`; SEGMENT `D` or `FNO` -> NFO/BFO with type `CE`/`PE`/`FUT`.
- Symbol (`:224-288`): EQUITY -> TRADING_SYMBOL; FUT -> `{base}{DDMMMYY}FUT` where base = TRADING_SYMBOL before first `-`; options -> `{base}{DDMMMYY}{strike}{CE|PE}` (integer strike when whole, else `%g`); index -> SEGMENT value / SYMBOL_NAME.
- Index renames (`:466-474`): `NIFTY 50->NIFTY, Nifty Next 50->NIFTYNXT50, Nifty Financial->FINNIFTY, BANK NIFTY->BANKNIFTY, Nifty Midcap Sel->MIDCPNIFTY, India VIX->INDIAVIX, S&P BSE SENSEX 50->SENSEX50`.
- `name` = underlying root (TRADING_SYMBOL before `-`) for CE/PE/FUT else symbol (`:290-313`). Dedup on `token` keep first (`:509`); whole table deleted then bulk-inserted (`:545-549`).

### 7. History (`B/api/data.py:865-1050`)
- Endpoint `GET /market/historical/{interval}` params `scrip-codes=<SEG>_<token>`, `start_time`, `end_time` (epoch **milliseconds**, local-time 00:00:00 / 23:59:59 of the dates, `:1053-1063`).
- Interval map (`:223-242`): 1m..30m -> `1minute,2minute,3minute,4minute,5minute,10minute,15minute,30minute`; 1h/2h/3h/4h -> `60minute,120minute,180minute,240minute`; D/W/M -> `1day,1week,1month`. No sub-minute.
- Chunk limits (`:911-930`): minute intervals 7 days, hourly 14 days, D/W/M 365 days; range split into consecutive chunks (`:1065-1075`). Pacing via shared limiter (data bucket).
- Response: `data[scrip].candles[]` of `{"ts","o","h","l","c","v"}`; `ts` is **seconds** despite docs saying ms (`:967-981`); fallback list form `[ts_ms,o,h,l,c,v]` divided by 1000 (`:983-996`). `oi` always 0. Sorted + dedup by timestamp. No special current-day candle handling.
- Scrip-code segments for data (`:253-263`): NSE, BSE, NFO, BFO, MCX, CDS, BCD, `NSE_INDEX->NIDX`, `BSE_INDEX->BIDX`.

### 8. Streaming
Market data (`B/streaming/indWebSocket.py`, `indmoney_adapter.py`, `indmoney_mapping.py`): protocol **JSON text**.
- Connect `wss://ws-prices.indstocks.com/api/v1/ws/prices`, header `Authorization: <token>` (`indWebSocket.py:18, 322-343`); no auth message. Heartbeat: client ping payload `"ping"` every 30s, pong timeout 10s, server replies text `"pong"` (`:21-23`, `:349-354`, `:105-112`).
- Subscribe/unsubscribe (`:248-253`, `:271-275`): `{"action":"subscribe"|"unsubscribe","mode":"ltp"|"quote","instruments":["NSE:2885","NFO:51011",...]}`; instrument = `SEGMENT:TOKEN` with segments NSE/NFO/BSE/BFO/NIDX/BIDX (`indmoney_mapping.py:13-20`). Max 1000 instruments per frame, 3000 per connection; frames split per segment (mixed frames deliver only first segment) (`indWebSocket.py:36-37`, `:225-257`).
- Modes: OpenAlgo 1->`ltp`, 2->`quote`; depth not supported (level 1 only) (`indmoney_mapping.py:58-60`, `:108-117`).
- Tick frame (`indmoney_adapter.py:548-569`, `:656-690`): `{"mode":"ltp","instrument":"2885","timestamp":1750138351089,"data":{"ltp":1426}}`; quote `data` keys: `ltp, open, high, low, close, volume, bid_price, bid_qty, ask_price, ask_qty, average_price, oi, oi_change`. `instrument` is the bare token (no segment) — adapter resolves via subscriptions, drops ambiguous tokens (`:585-606`). Zero values replaced from per-symbol cache (`:663-668`). Published topic `{exchange}_{symbol}_{LTP|QUOTE}` (`:617-618`).
- Reconnect: adapter single loop, delay 5s * 2^n capped 60s, max 10 attempts, re-reads token from DB before reconnect, resubscribes grouped by (mode, segment) (`indmoney_adapter.py:30-33`, `:171-218`, `:466-510`). Subscription batching window 500ms (`:53`).

Order updates (`B/streaming/indmoney_order_adapter.py`): `wss://ws-order-updates.indstocks.com/api/v1/ws/trades`, header `Authorization: <token>` (`:47`, `:139-141`); post-connect send `{"action":"subscribe","mode":"order_update"}` (`:156`). Observed frame (`:12-18`):
```json
{"mode":"order_update","timestamp":1786000826536,"data":{"order_id":"96057848","entity_name":"Yes Bank Ltd","order_type":"SELL","order_status":"S","lot":1,"executed_price":22.89,"elapsed_time":23,"error_message":" ","req_quantity":1}}
```
Status letters: R/P -> open, S -> complete (confirmed); C/X -> cancelled, F/E/J -> rejected (inferred); else REST vocabulary (`:184-196`, `:214-235`). `order_type` here is the side. Bare numeric `order_id` is resolved to canonical `EQ-/DRV-` id via orderbook suffix match (`:27-31`, `:69-126`). Output fields: `orderid, action, order_status, quantity, filled_quantity, pending_quantity, average_price[, rejection_reason]` (`:334-346`).

### 9. Rate limits / quirks (`B/api/rate_limiter.py`)
- Documented: Order 10/s; Data (instruments, historical) 5/s + 100k/day; Quote 5/s + 100k/day; Non-trading 15/s + 100k/day; token mint 1/60s (`:4-10`). Paced at 80% headroom (8/4/4/12 per s) with a module-level shared slot clock (`:47-59`, `:114-172`); daily counter per IST day, warn at 80% (`:61-64`, `:192-228`).
- Classification (`:90-111`): `/market/quotes*` -> quote; `/market/historical*`, `/market/instruments*`, `/margin` -> data; POST to `/order, /order/modify, /order/cancel, /smart/order, /smart/order/modify, /smart/order/cancel` -> order; else non_trading.
- 429 retry: up to 3 retries, backoff 1s*2^n capped 30s or `Retry-After`; **order writes never retried** (`:66-68`, `:240-296`). `/generate/token` bypasses the limiter (`:26-29`).
- Quirks: raw token in `Authorization` (no Bearer); `GET /margin` with a JSON body; positions need 4 queries and lack LTP/unrealized P&L (`order_api.py:242-250`); trade-book has no exchange field (`order_api.py:194-197`); `/market/quotes/full` 400-rejects a whole batch on one bad scrip (`data.py:17-22`); stop orders go to `/smart/order` as TRIGGER, NSE-only, and come back as `GTT_LIMIT` (`order_api.py:594-596`); a Cloudflare 403 on `/funds` is logged specially (`funds.py:97-104`); 24h token expiry forces daily re-login.

---

### deltaexchange (CRYPTO — API key + HMAC-SHA256, no login UI step)

All paths below are under `/Users/openalgo/openalgo-desktop/openalgo/` unless absolute. `B=broker/deltaexchange`.

### 1. Family classification
- **Family:** API-key + HMAC-SHA256 request signing. No OAuth, no TOTP, no session token: every REST call and the private WebSocket are signed fresh with `BROKER_API_KEY` / `BROKER_API_SECRET` (`B/api/baseurl.py:45-85`, `B/api/auth_api.py:11-34`). The "login" is just a signed `GET /v2/profile` probe (`auth_api.py:36-67`).
- **Platform-wide difference:** the only broker with `"broker_type": "crypto"`, `"supported_exchanges": ["CRYPTO"]`, `"leverage_config": true` (`B/plugin.json:8-10`); every other plugin.json is `IN_stock` / `leverage_config: false` (e.g. `broker/angel/plugin.json:9-10`).
- **Closest template:** none of the Indian brokers; structurally it is a "stateless signed REST" broker like the kotak/angel spec's REST layer but with a per-request signature instead of a bearer token. Reuse the Indian F&O symbol renderers (dated FUT/CE/PE) per `docs/prompt/crypto-symbol-format.md:13-16`.

### 2. Auth flow
- **Credentials:** `BROKER_API_KEY` = Delta API key, `BROKER_API_SECRET` = Delta API secret, both plain (no `:::` composite) (`auth_api.py:28-29`; `utils/config.py:11-28` only returns the raw env vars).
- **Login call sequence** (`blueprints/brlogin.py:555-559`): `code="deltaexchange"` → `auth_function(code)` → `authenticate_broker(code)`; frontend lists it with `authType: 'totp'` but falls into the generic submit branch (`frontend/src/pages/BrokerSelect.tsx:26,141`) — no fields are actually required from the user.
- **Step 1 (only step):** `GET https://api.india.delta.exchange/v2/profile` with signed headers (`auth_api.py:37-54`). Success = HTTP 200 and `success: true`; stores `api_key` itself as the auth token: `return api_key, None` (`auth_api.py:59-67`). 401 → "Invalid API key or signature"; 403 → IP-whitelist message (`auth_api.py:74-86`). Response `result.email` is only logged.
- **Signature (every request):** `prehash = METHOD.upper() + timestamp + path + query_string + body`, `timestamp = str(int(time.time()))` (seconds), `signature = hex(HMAC_SHA256(secret, prehash))` (`baseurl.py:27-76`). `query_string` MUST include the leading `?` and is built as `"?" + "&".join(f"{k}={v}" for sorted(params))` with no URL-encoding (`order_api.py:56-58`, `margin_api.py:133`). Body is the exact JSON string sent (`order_api.py:60,88-108`).
- **Headers:** `api-key`, `timestamp`, `signature`, `User-Agent: openalgo-python-client`, `Content-Type: application/json`, `Accept: application/json` (`baseurl.py:78-85`).
- **Expiry:** Delta rejects signatures older than 5 s ("SignatureExpired") — rate-limiter `consume()` must run BEFORE signing (`rate_limiter.py:137-139`, `order_api.py:76-95`). Stored token never expires; session expiry is disabled instance-wide for crypto via `DISABLE_SESSION_EXPIRY=true` → `get_session_expiry_time()` returns 365 days (`utils/session.py:21-30`, `.sample.env:204-208`).
- **Stored auth token:** the API key string (`order_api.py:42`, `funds.py:109`). Secret is always re-read from env (`order_api.py:51`). No feed token.

### 3. Base URLs
- REST: `BASE_URL = os.getenv("DELTA_BASE_URL", "https://api.india.delta.exchange")` (`baseurl.py:9`) — India by default; override env to testnet/global. Master contract hardcodes `https://api.india.delta.exchange/v2/products` (`master_contract_db.py:295`).
- WS public: `wss://public-socket.india.delta.exchange`; WS private: `wss://socket.india.delta.exchange` (`B/streaming/delta_websocket.py:76-77`).
- Public market data (tickers, l2orderbook, candles, products) is sent **unauthenticated** with only `Accept: application/json` (`data.py:86-87`, `master_contract_db.py:296`).

### 4. REST endpoints
| Function | Method/path | Auth | Request | Response consumed |
|---|---|---|---|---|
| verify login | `GET /v2/profile` | signed | — | `success`, `result.email` (`auth_api.py:37-67`) |
| set leverage | `POST /v2/products/{product_id}/orders/leverage` | signed | `{"leverage": "10"}` (string) | `success` (`order_api.py:398-431`) |
| place | `POST /v2/orders` | signed | transform_data payload (sec 5) | `result.id`, `result.product_id` → orderid `"{product_id}:{id}"` (`order_api.py:475-485`) |
| modify | `PUT /v2/orders` | signed | `{"id","product_id","size","limit_price"[,"stop_price"]}` (`transform_data.py:161-172`) | `success` (`order_api.py:681-695`) |
| cancel | `DELETE /v2/orders` | signed | `{"id": int, "product_id": int}` from composite id (`order_api.py:615-625`) | `success` |
| cancel all | `DELETE /v2/orders/all` body `{"cancel_limit_orders":true,"cancel_stop_orders":true,"cancel_reduce_only_orders":true}`; fallback: per-order cancel of `state in (open,pending)` from `GET /v2/orders?state=open` (`order_api.py:637-674`) | signed | | |
| orderbook | `GET /v2/orders?state=open` + `GET /v2/orders/history`, filtered to today's IST date via `created_at[:19]` UTC→IST (`order_api.py:167-205`) | signed | | list of order dicts |
| tradebook | `GET /v2/fills`, filtered to today IST (`order_api.py:211-243`) | signed | | fills |
| positions | `GET /v2/positions/margined` + `GET /v2/wallet/balances` (non-INR/USD assets with `balance - blocked_margin > 0` synthesised as spot positions `"{asset}_INR"`, `_is_spot: True`) (`order_api.py:250-317`) | signed | | `product_id, product_symbol, size, entry_price, realized_pnl, unrealized_pnl` |
| holdings | none — returns `[]` (`order_api.py:320-322`) | | | |
| funds | `GET /v2/wallet/balances` + `GET /v2/positions/margined` (`funds.py:83-183`) | signed | | sec 5 |
| margin | `GET /v2/users/trading_preferences` (margin mode, `portfolio_margin_enabled`/`margin_type=="cross"`) then per-leg `GET /v2/products/{product_id}/margin_required?order_type=&side=&size=[&limit_price=]` (`margin_api.py:16-62,120-160`) | signed | | `result.initial_margin` → total & span, exposure 0 (`margin_data.py:132-141`) |
| quotes | `GET /v2/tickers/{brsymbol}` (`data.py:131-159,239-283`) | public | | `mark_price→ltp, open, high, low, volume, close→prev_close, oi, quotes.best_bid/best_ask` |
| depth | `GET /v2/tickers/{brsymbol}` then `GET /v2/l2orderbook/{product_id}` (`data.py:289-401`) | public | | `result.buy[]/sell[]` `{price,size,depth}` top 5, `ltq=0` |
| history | `GET /v2/history/candles?symbol=&resolution=&start=&end=` (`data.py:430-625`) | public | | `result[][ts,o,h,l,c,v(,oi)]` or dicts |
| products | `GET /v2/products?page_size=500&states=live[&after=cursor]` (`master_contract_db.py:280-376`) | public | | sec 6 |
| multiquotes | not implemented (no batch method in `data.py`) | | | |
| rate quota | `GET /v2/rate_limits/quota` → `current_quota`, `remaining_time_in_milliseconds` (`rate_limiter.py:194-218`) | public | | |

Response envelope everywhere: `{"success": bool, "result": ..., "error": {"code","message"}}`; `meta.after` for pagination (`master_contract_db.py:357-362`). Place-order response shim returns 200/400 based on `success` (`order_api.py:492-497`).

### 5. Enum mappings and formulas
- **Exchange:** OpenAlgo `CRYPTO` only; `map_exchange*` always return `"CRYPTO"` (`transform_data.py:204-217`). Streaming mapper aliases NSE/BSE/MCX→CRYPTO as safety (`delta_mapping.py:17-22`).
- **Product:** not sent to Delta at all; `map_product_type` is identity, `reverse_map_product_type` → `"NRML"` (`transform_data.py:188-201`). Order/trade book rows report `productType="NRML"`; positions report `"CNC"` for spot wallet rows else `"NRML"` (`order_data.py:58,309,442`). close_all sends `product = "CNC" if is_spot else "NRML"` (`order_api.py:742`). Per `crypto-symbol-format.md:122-124` Indian CNC/NRML/MIS semantics do not apply.
- **Pricetype → order_type** (`transform_data.py:177-185`): `MARKET→market_order`, `LIMIT→limit_order`, `SL→limit_order`+`stop_order_type=stop_loss_order`, `SL-M→market_order`+stop; `stop_price=trigger_price`, `stop_trigger_method` default `last_traded_price` (`transform_data.py:77-88`). `limit_price` is a **string** (`transform_data.py:73-75`).
- **Action:** `data["action"].lower()` → `buy`/`sell` (`transform_data.py:59`). **Validity:** default `time_in_force="gtc"`; `validity=="IOC"` → `"ioc"` (`transform_data.py:69,90-92`). Optional pass-throughs: `post_only`, `reduce_only`, `client_order_id`, `trail_amount`, bracket fields `bracket_stop_loss_price|bracket_stop_loss_limit_price|bracket_trail_amount|bracket_stop_trigger_method|bracket_take_profit_price|bracket_take_profit_limit_price` (`transform_data.py:94-117`).
- **Place payload (literal):**
```json
{"product_id": 27, "product_symbol": "BTCUSD", "size": 1, "side": "buy", "order_type": "limit_order", "time_in_force": "gtc", "limit_price": "60000"}
```
- **Size semantics:** `_order_size` — SPOT instrument (`instrumenttype=="SPOT"`) keeps fractional float; derivatives must be whole ints else `ValueError` (`transform_data.py:10-27`). Quantity = number of contracts, not underlying units.
- **Broker → OpenAlgo order status** (`order_data.py:81-91`): `open→open`, `closed|filled→complete`, `cancelled→cancelled`, `pending→open` (untriggered stop); stats pass also maps `rejected→rejected` (`order_data.py:164-177`). **Order type reverse:** `stop_order_type==stop_loss_order` + `limit_order→SL`, + `market_order→SL-M`; else `limit_order→LIMIT`, `market_order→MARKET` (`order_data.py:68-77`). Composite `orderId = f"{product_id}:{id}"` (`order_data.py:50`). Timestamp field `created_at`.
- **Positions:** `netQty=float(size)` (fractional allowed), `avgCostPrice=entry_price`, `pnlAbsolute = realized_pnl + unrealized_pnl`, `lastTradedPrice=0` (enriched later), `lot_size = SymToken.contract_value` (e.g. 0.01 ETH per ETHUSD contract) so the frontend scales live P&L (`order_data.py:444-477`).
- **Funds** (`funds.py:157-176`): `availablecash = Σ balance_inr`, `collateral = Σ cross_locked_collateral`, `utiliseddebits = Σ blocked_margin` over `/v2/wallet/balances.result[]`; `m2mrealized = Σ realized_pnl`, `m2munrealized = Σ unrealized_pnl` over `/v2/positions/margined.result[]`. All formatted `%.2f`. Frontend shows USD (`frontend/src/lib/utils.ts:32`), Telegram uses `$` for `CRYPTO_BROKERS` (`services/telegram_bot_service.py:217`).
- **Price scaling:** none; all prices are decimal strings parsed with `float()`.
- **Leverage (`leverage_config`):** before every `POST /v2/orders`, resolve leverage with priority `data["leverage"]` (order payload) → `database.leverage_db.get_leverage()` (single-row table `leverage_config(id=1, leverage FLOAT)`, cached 1 h) → env `DELTA_DEFAULT_LEVERAGE`; if non-empty and `!= "0"` call `POST /v2/products/{token}/orders/leverage {"leverage": "<int>"}` (`order_api.py:455-469`, `database/leverage_db.py:33-71`). Failure only warns unless `DELTA_ABORT_ON_LEVERAGE_FAILURE=true` raises (`order_api.py:413-431`). UI: `/leverage` page gated on `capabilities.leverage_config === true` (`frontend/src/hooks/useProfileMenuItems.ts:25`), REST `GET /leverage/api/current`, `POST /leverage/api/update {"leverage": 10}` (whole non-negative int; 0 = broker default) (`blueprints/leverage.py:16-55`). No get-leverage call to Delta exists. Capabilities exposed by `GET /broker/capabilities` from `utils/plugin_loader.py:41-47` (`blueprints/broker_credentials.py:312-339`).
- **Smart order:** per-symbol `SymbolLocks` + 1 s `PositionBookCache`, position matched on `product_symbol == brsymbol`, `strict=True` read raises `PositionReadError` if either half fails (`order_api.py:332-391,537-600`).

### 6. Master contract
- Source: `GET https://api.india.delta.exchange/v2/products` JSON, cursor pagination `page_size=500&states=live`, follow `meta.after` until null/empty; `MAX_PAGES=100`; any error → `fetch_success=False` and the existing table is preserved (`master_contract_db.py:280-376,510-557`). Weight 3/page on PUBLIC bucket.
- Filters: `state=="live"`, `trading_status=="operational"`, skip `product_specs.only_reduce_only_orders_allowed` (`master_contract_db.py:407-421`).
- Columns (`master_contract_db.py:483-498`): `token=str(id)`, `brsymbol=symbol`, `symbol=_to_canonical_symbol(...)`, `name=underlying_asset.symbol` (root e.g. BTC; option-chain lookups ilike on it), `exchange="CRYPTO"`, **`brexchange="DELTAIN"`** (code; the doc says `deltaexchange` — code wins), `expiry = settlement_time ISO → "%d-%b-%y".upper()` e.g. `28-FEB-25`, else `""`, `strike=strike_price` (fallback parse `parts[2]` of `C-BTC-80000-280225`), `lotsize=product_specs.min_order_size or 1` (float; fractional for spot), `instrumenttype=CONTRACT_TYPE_MAP[contract_type]`, `tick_size=float(tick_size)`, `contract_value=float(contract_value or 1.0)` (extra column added by idempotent migration `master_contract_db.py:54,63-80`).
- `CONTRACT_TYPE_MAP` (`master_contract_db.py:251-265`): `perpetual_futures→PERPFUT, futures→FUT, call_options→CE, put_options→PE, spot→SPOT, move_options→MOVE, interest_rate_swaps→IRS, spreads→SPREAD, options_combos→COMBO, turbo_call_options→TCE, turbo_put_options→TPE, synth_call_options→SYNCE, synth_put_options→SYNPE`.
- Symbol construction `_to_canonical_symbol` (`master_contract_db.py:174-247`):
  - options (CE/TCE/SYNCE/PE/TPE/SYNPE): `C-BTC-80000-280225` + expiry `28-FEB-25` → `BTC28FEB2580000CE` (`{underlying}{DDMONYY}{strike}{CE|PE}`).
  - dated FUT: `BTCUSD28Feb2025` → strip `DDMON2025` suffix → `BTCUSD` → strip quote suffix (`USDT|USD|BTC|ETH`) → `BTC28FEB25FUT`.
  - PERPFUT: `BTCUSD` → `BTCUSDFUT` (code appends `FUT`; docstring's `.P` is stale — see `crypto-symbol-format.md:39-43`).
  - SPOT: `BTC_INR` → `BTCINR`. Others kept native.
- `SYMBOL_ALIASES = {"NEARBRC": "NEARUSD"}`; `search_symbols` uses `LIKE %sym%` (`master_contract_db.py:275-281,564-581`). Dedup on `token`; chunked insert 500 rows (`master_contract_db.py:118,505`).
- Cache cutoff: crypto brokers (`utils/constants.py:30 CRYPTO_BROKERS={"deltaexchange"}`) re-download once per UTC day per `CRYPTO_MASTER_CONTRACT_CUTOFF_TIME` default `00:00` UTC (`utils/auth_utils.py:55-60`).

### 7. History
- `GET /v2/history/candles` params `symbol=brsymbol, resolution, start, end` (epoch seconds as strings) (`data.py:531-543`).
- Interval map (`data.py:213-227`): `1m,3m,5m,15m,30m,1h,2h,4h,6h,1d,1w` identity; aliases `D→1d`, `W→1w`.
- Chunking `CHUNK_DAYS` (`data.py:416-428`): `1m:1, 3m:7, 5m:12, 15m:30, 30m:60, 1h/2h/4h/6h:90, 1d/1w:0 (no chunking)`; comment notes a ~2000-candle cap per request.
- Timestamps: dates are **IST calendar dates** → `00:00:00`/`23:59:59` IST localized → epoch (`data.py:743-761`); `end_ts = min(end_of_day, now)` and rows with `timestamp > now` dropped because Delta pads flat zero-volume candles to `end` (`data.py:513-529,598-603`). Output columns `timestamp,open,high,low,close,volume,oi` sorted/deduped. Transient httpx errors retried 2x with 0.3 s (`data.py:58-67`).

### 8. Streaming
- Two sockets (`delta_adapter.py:112-153`): public `wss://public-socket.india.delta.exchange` (no auth; auth frame is rejected there) for `ticker` + `ob_l2`; private `wss://socket.india.delta.exchange` with auth for `orders`, `positions`, `margins`. Legacy `v2/ticker`/`l2_orderbook` names on the private socket are deprecated (removal 31 Jul 2026) and cap 20 symbols (`delta_websocket.py:10-15`).
- **Auth message** (private only, sent in on_open): `{"type":"key-auth","payload":{"api-key":K,"signature":hex(HMAC_SHA256(secret,"GET"+ts+"/live")),"timestamp":ts}}`, `ts` = epoch seconds string (`delta_websocket.py:156-173,379-383`).
- **Subscribe/unsubscribe:** `{"type":"subscribe"|"unsubscribe","payload":{"channels":[{"name":"ticker","symbols":["BTCUSD",...]}]}}` (`delta_websocket.py:177-184`). Limits: `ticker` unlimited per frame (150 verified), **`ob_l2` exactly 1 symbol per frame** (`delta_websocket.py:88-91`). Private: `{"name":"orders","symbols":["all"]}`, same for `positions`; `margins` without symbols (`delta_websocket.py:268-280`).
- **Modes** (`delta_mapping.py:44-48`): 1 LTP→`ticker`; 2 QUOTE→`ticker`; 3 DEPTH→`ob_l2`+`ticker`. Depth levels supported `[1,5]` (ob_l2 gives 15, top 5 used).
- **Ticker frame** (JSON, `delta_adapter.py:532-605`): `{"type":"ticker","sy":"BTCUSD","sp":"63860.7","ts":µs,"d":[{"s","m","ohlc":[o,h,l,c],"oi":[oi,oi_chg],"q":[best_ask,ask_size,best_bid,bid_size,impact_mid],"g":[...],"qiv":[...]}]}`. Mapping: `ltp←d.m` (fallback `sp`), `open/high/low/close←ohlc[0..3]`, `oi←oi[0]`, `ask_price←q[0]`, `ask_qty←q[1]`, `bid_price←q[2]`, `bid_qty←q[3]`; no volume on WS. Absent fields omitted (not zeroed); null inside a present array → 0.
- **ob_l2 frame:** `{"type":"ob_l2","sy":"BTCUSD","a":[["price","size"],...],"b":[...]}` best-first → `depth.buy/sell` top 5 `{price,quantity}`, `totalbuyqty/totalsellqty` (`delta_adapter.py:607-633`).
- Publishing: per-symbol merged cache across channels, topic `f"{exchange}_{symbol}_{LTP|QUOTE|DEPTH}"`; private events republished raw on ZMQ topics `deltaexchange_orders|positions|margins` with `timestamp` added (`delta_adapter.py:449-485,510-528`). Order event example: `{"type":"orders","action":"fill","id","product_id","product_symbol","size","side","average_fill_price","state","client_order_id"}` (`delta_adapter.py:422-431`). No separate order-update adapter file.
- Heartbeat: websocket-client `ping_interval=30`, `ping_timeout=10`; reconnect loop max 5 attempts, delay 5 s ×2 capped 60 s, budget reset after a healthy connection; subscription registry replayed on every reconnect and re-auth sent (`delta_websocket.py:78,314-360,385-404`). Adapter coalesces subscribe bursts with a 0.5 s batch timer; subscribe/unsubscribe in the same window cancel out (`delta_adapter.py:73-81,308-345`). Acks: `key-auth`, `subscriptions`; `error` logged (`delta_websocket.py:417-423`).

### 9. Rate limits / quirks
- **Weighted quota:** 10000 units / fixed 5-min window; local budget 90% (9000); two buckets PUBLIC (by IP, unauthenticated market data) and PRIVATE (by user) (`rate_limiter.py:39-48`). Weights: `POST/PUT/DELETE /v2/orders/batch=25`, `POST/PUT/DELETE /v2/orders=5`, `/v2/orders/history=10`, `/v2/fills=10`, `/v2/wallet/transactions=10`, `/v2/positions/change_margin=5`, `/v2/products|tickers|l2orderbook|history/candles|history/sparklines|orders(GET)|positions|wallet/balances|rate_limits/quota=3`, default 1 (`rate_limiter.py:65-90`). On 429 Delta returns `X-RATE-LIMIT-RESET` in **ms** (no Retry-After); retry up to 3 with capped wait ≤30 s + jitter, re-sign each attempt (`order_api.py:69-125`, `rate_limiter.py:221-287`). `MAX_WAIT_SECONDS=30` else `DeltaRateLimitError`.
- Composite order id `"{product_id}:{order_id}"` is required for cancel/modify (`order_api.py:607-634`).
- Orderbook/tradebook are filtered to **today in IST** although the venue is 24x7 UTC (`order_api.py:173-205`).
- Spot "positions" are synthesised from wallet balances with `entry_price="0"` (`order_api.py:296-306`).
- IP whitelisting is enforced by Delta (403 from CDN) (`auth_api.py:79-86`; `CLAUDE.md` SEBI note).
- Platform-wide crypto handling: `CRYPTO_EXCHANGES={"CRYPTO"}`, `INSTRUMENT_PERPFUT="PERPFUT"`, `CRYPTO_QUOTE_CURRENCY="USDT"` (`utils/constants.py:20-37`); market calendar treats CRYPTO as 24/7 `00:00-23:59:59` (`database/market_calendar_db.py:51,72`); python strategies default to 00:00-23:59 and `is_24x7` for CRYPTO (`blueprints/python_strategy.py:2231-2238,2939-2962`); option tools use `construct_crypto_option_symbol` and PERPFUT as underlying (`services/option_symbol_service.py:414`, `services/option_chain_service.py:167-189,379-380`); playground loads bruno collections by `broker_type` (`blueprints/playground.py:346-348`).

---

