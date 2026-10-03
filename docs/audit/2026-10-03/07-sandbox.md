# 07 - Sandbox (Analyzer Mode) Parity Audit

Web (reference, read-only): `/Users/openalgo/openalgo-desktop/openalgo`
Desktop (Tauri/Rust): `/Users/openalgo/openalgo-desktop/openalgo-desktop`

All paths below are relative to those roots unless absolute. Line numbers are from the files as read on 2026-10-03.

Verdict in one paragraph: the desktop has a 523-line SQLite stub (`src-tauri/src/db/sqlite/sandbox.rs`) that records orders and nets quantities. It has no execution engine, no fund/margin model, no trade book, no T+1 settlement, no square-off, no session boundary, no GTT, no catch-up, no events, and its schema shares almost no column names with the web. Every engine-level behaviour in Part 1 is "Missing" in Part 2; the handful of things that exist (toggle flag, read-through routing of orderbook/positions/funds/holdings) have bugs listed with lines. Part 3 is the Rust design to close the gap.

---

## Part 1. The web engine, precisely

### 1.1 Mode switch and routing

- Flag: `database/settings_db.py:88-126` (`get_analyze_mode` cached, `set_analyze_mode` invalidates). Default Live.
- Every order/data service short-circuits on the flag before touching the broker:
  `services/place_order_service.py:166` (`if get_analyze_mode() and not force_live`), `place_smart_order_service.py:139,150`, `modify_order_service.py:88`, `cancel_order_service.py:88`, `cancel_all_order_service.py:88`, `close_position_service.py:88`, `basket_order_service.py:204`, `split_order_service.py:183-233`, `orderbook_service.py:126`, `tradebook_service.py:90`, `positionbook_service.py:101`, `openposition_service.py:82`, `orderstatus_service.py:83`, `funds_service.py:52`, `holdings_service.py:86`, `place_gtt_order_service.py:93`, `modify_gtt_order_service.py:52`, `cancel_gtt_order_service.py:51`, `gtt_orderbook_service.py:44`, `options_multiorder_service.py:620,693`, `place_options_order_service.py:310`. Each delegates to `services/sandbox_service.py` (`sandbox_place_order` L42, `sandbox_modify_order` L110, `sandbox_cancel_order` L149, `sandbox_get_orderbook` L179, `sandbox_get_order_status` L202, `sandbox_get_positions` L231, `sandbox_get_holdings` L254, `sandbox_get_tradebook` L277, `sandbox_get_funds` L300, `sandbox_close_position` L329, `sandbox_place_smart_order` L413, `sandbox_cancel_all_orders` L552, `sandbox_get_pnl_symbols` L706, GTT wrappers L778-837).
- `force_live=True` (`place_order_service.py:166`) lets a live strategy run bypass a mid-run toggle. This is a documented invariant in `CLAUDE.md` ("An order path decides once").
- Toggling is one locked step: `services/analyzer_service.py:115-152` `apply_analyze_mode()` writes the flag, re-reads it, then `_reconcile_sandbox()` (L70-113) starts/stops the execution engine and the square-off scheduler and runs `catchup_missed_settlements()`. Three entry points call it: REST `POST /api/v1/analyzer/toggle` (L203-258, 403 in semi-auto mode L350-361), session `POST /auth/analyzer-toggle` (`blueprints/auth.py:1146-1176`, `toggle=True`), and `POST /settings/analyze-mode/<int>` (`blueprints/settings.py:27-50`, `with_scheduler=False, catchup=False`).
- Startup: `app.py:945-965` starts engine + scheduler + catch-up in parallel when the persisted flag is on. Login / master-contract load runs `run_catch_up_tasks()` regardless of mode (`utils/auth_utils.py:170-172`).

### 1.2 Capital and fund model (`sandbox/fund_manager.py`)

- Funds row per user, created lazily (`initialize_funds` L241-275) with `total_capital = available_balance = starting_capital` (config, default 10000000.00), everything else 0, `last_reset_date = now IST`, `reset_count = 0`.
- Columns and their meaning (`database/sandbox_db.py:216-249`): `total_capital`, `available_balance`, `used_margin`, `realized_pnl` (all-time), `today_realized_pnl` (resets at session boundary), `unrealized_pnl`, `total_pnl = realized + unrealized`, `last_reset_date`, `reset_count`.
- Every balance write is a compare-and-set against a fresh read of all seven money columns (`_MONEY_COLUMNS` L49-57, `read_funds_snapshot` L87-113, `write_funds_if_unchanged` L123-157, `apply_funds_change` L160-198, 5 attempts, busy message L65-67). No process lock protects balances; `_lock` (L235) guards only row creation and reset.
- Operations:
  - `block_margin(amount)` L505-549: refuses `amount <= 0`; refuses if `available < amount`; `available -= amount; used += amount`; commits.
  - `stage_release_margin(amount, realized_pnl, count_today=True)` L551-610 and `release_margin` L618-634: refuses negative amount; refuses `amount > used_margin` (L590-599); new values per `_released` L201-220: `available += amount; available += realized_pnl; used -= amount; realized += realized_pnl; total = realized + unrealized; today_realized += realized_pnl (if count_today)`.
  - `stage_margin_delta(delta)` L432-503: single SQL UPDATE with the sufficiency check in the WHERE (used by GTT).
  - `transfer_margin_to_holdings(amount)` L636-706: `used -= amount` only (no credit to available) - the cash "is" the holding now. Refuses `amount > used`.
  - `credit_sale_proceeds(amount)` L708-758: `available += amount`.
  - `stage_prior_session_release(amount, pnl)` L760-805: for stale MIS catch-up; `available += amount + pnl; used = max(0, used - amount); realized += pnl; total = realized + unrealized`; does NOT touch `today_realized_pnl`.
  - `update_unrealized_pnl(x)` L807-834: `unrealized = x; total = realized + x`.
  - `_reset_funds` L351-381: all balances back to `starting_capital`, `reset_count += 1`, deletes the user's positions and holdings (orders/trades kept). Auto-reset: `_check_and_reset_funds` L319-349 on every `get_funds` when `reset_day != Never`, plus a cron job (squareoff_thread L124-168).
  - `rebase_starting_capital(new)` L950-998 (called when `starting_capital` config changes): `total_capital = new; available = new - used_margin + total_pnl`.
  - `reconcile_margin(user, auto_fix)` L1001-1135: `discrepancy = used_margin - (sum(position.margin_blocked where qty != 0) + sum(active GTT margin_blocked))`; with `auto_fix` sets `used = that sum; available += discrepancy`. Called after every fill if `validate_margin_consistency` (L1184-1221) fails (`execution_engine.py:1106-1113`).
- Margin formula `calculate_margin_required(symbol, exchange, product, qty, price, action)` L836-869: `margin = |qty| * price / leverage` (Decimal, no rounding). Leverage `_get_leverage` L871-900:
  - NSE/BSE: MIS -> `equity_mis_leverage` (5); CNC or NRML -> `equity_cnc_leverage` (1).
  - else `is_future(symbol, exchange)` -> `futures_leverage` (10).
  - else `is_option` -> BUY `option_buy_leverage` (1) / SELL `option_sell_leverage` (1).
  - else 1.
- `get_funds()` output shape (L299-313), which is what `/api/v1/funds` returns in `data`:
  `{"availablecash", "collateral": 0.00, "m2munrealized", "m2mrealized" (= today_realized), "total_realized_pnl", "today_realized_pnl", "utiliseddebits" (= used_margin), "grossexposure" (= used_margin), "totalpnl", "last_reset": "YYYY-MM-DD HH:MM:SS", "reset_count"}`.

### 1.3 Order lifecycle (`sandbox/order_manager.py`, `sandbox/execution_engine.py`)

Statuses: `open`, `trigger pending`, `complete`, `cancelled`, `rejected` (CHECK constraint `database/sandbox_db.py:103-106`). Pending set = (`open`, `trigger pending`) (`execution_engine.py:46`, `order_manager.py:40`). No partial fills: `filled_quantity` is 0 or `quantity`.

Placement `OrderManager.place_order(order_data, prefetched_quote=None)` L67-813, held under the position lock (decorator L66):

1. `_validate_order` L1336-1412: required `symbol, exchange, action, quantity, price_type, product`; action in BUY/SELL; price_type in MARKET/LIMIT/SL/SL-M; product in CNC/NRML/MIS; NSE/BSE reject NRML (L1361-1366); NFO/BFO/MCX/CDS/BCD/NCDEX/CRYPTO reject CNC (L1369-1374); qty > 0; LIMIT/SL require price > 0; SL/SL-M require trigger_price > 0; exchange in `VALID_EXCHANGES`.
2. Field normalisation L116-119: MARKET and SL-M drop `price`; MARKET and LIMIT drop `trigger_price`.
3. Symbol must exist in SymToken (L122-132). Lot-size multiple enforced for NFO/BFO/CDS/BCD/MCX/NCDEX/CRYPTO (L135-146).
4. MIS time gate L150-210: if `now_IST >= square_off_time(exchange)` or `now < 09:00`, reject unless the order reduces an existing open MIS position (BUY vs short / SELL vs long). Message: "MIS orders cannot be placed after square-off time (HH:MM IST). Trading resumes at 09:00 AM IST."
5. CNC SELL check L217-255: `available = signed position qty + max(holding qty, 0)`; reject if `available <= 0` or `qty > available`. Rejection is persisted as a `rejected` order row with `rejection_reason`, `margin_blocked = 0` (L611-663) and returned as 400 `{"status":"error","orderid","message","mode":"analyze"}`. MIS SELL short-selling is allowed.
6. Margin price L260-490: MARKET -> live LTP (prefetched quote, else `ExecutionEngine._fetch_quote` 3 attempts with sleeps 0.3/0.6/0.9 s, else `position.ltp`, else reject "Cannot place MARKET order ... unable to fetch current price"); LIMIT -> limit price, and a marketability check (BUY: ltp <= price; SELL: ltp >= price) caches the quote (L340-386); SL/SL-M -> trigger price, and a trigger-met check (BUY: ltp >= trigger; SELL: ltp <= trigger) sets `trigger_price_met`; the quote is cached for SL-M when triggered, for SL only when limit is also satisfiable (L388-478).
7. Margin to block L492-605: `margin_required = calc(qty, price)`. If an opposite-direction open position exists: `order_qty <= |pos|` -> block 0; else block margin for `order_qty - |pos|` only. `should_block_margin`: BUY always; SELL for options, futures, or product MIS/NRML; CNC SELL never (and `margin_blocked` recorded as 0, L598-605). `check_margin_available` then `block_margin`; failure -> 400 with the funds message.
8. Order id `_generate_order_id` L1414-1435: `YYMMDD` + 6-digit microsecond + 2 random digits (14 digits).
9. Initial status L678-680: `trigger pending` for SL/SL-M unless trigger already met, else `open`. Row stores `price = LTP used` for MARKET (L667), `margin_blocked`, `gtt_leg_id`, `order_timestamp = now IST`. `OrderUpdateEvent` published with that status (L714, `_publish_order_update_event` L1166-1197).
10. Immediate execution L716-780: MARKET always via `ExecutionEngine._process_order(order, cached_quote)`; marketable LIMIT fills at **LTP** (price improvement, not the limit) L746-758; SL/SL-M with trigger met fills at LTP L759-769; `quote_looks_stale` (LTP outside day [low, high]) defers (L733-740).
11. If still pending, `WebSocketExecutionEngine.notify_order_placed(order)` (L787-802). Returns 200 `{"status":"success","orderid","mode":"analyze"}`.

Modify `modify_order(orderid, new_data)` L815-969: only from pending statuses (else 400 "Cannot modify order in X status"); `quantity` lot-size checked for F&O; `price` accepted only for LIMIT/SL, `trigger_price` only for SL/SL-M (400 otherwise); conditional UPDATE guarded on pending status (a fill/cancel that landed first wins). Note: margin is **not** recalculated on modify (the design doc `docs/design/07-sandbox/README.md:1525-1575` describes re-margining; the code does not do it). Response `{"status":"success","orderid","message":"Order modified successfully","mode":"analyze"}`.

Cancel `cancel_order(orderid)` L971-1164: from pending statuses; conditional UPDATE to `cancelled`; releases `order.margin_blocked` via `stage_release_margin` in the same transaction (409 if the release is refused); legacy fallback recomputes margin if the column is empty (L1023-1079); publishes `OrderUpdateEvent(cancelled)`. Response `{"status":"success","orderid","message":"Order cancelled successfully","mode":"analyze"}`.

Fill decision `ExecutionEngine._process_order(order, quote)` L432-572 (quote dict: `ltp`, `bid`, `ask`, optional `high`, `low`):
- If a trade already exists for the order, complete the order conditionally and publish (race cleanup, L441-485).
- `ltp <= 0` -> skip. `quote_looks_stale` -> defer (L96-121: stale iff `high>0 and low>0 and not low <= ltp <= high`).
- `trigger pending` -> `_process_trigger_pending_order` L574-654: trigger BUY `ltp >= trigger`, SELL `ltp <= trigger`. SL-M fills at LTP. SL fills at LTP if limit also met (BUY `ltp <= price`, SELL `ltp >= price`), else conditional flip to `open` and publish `OrderUpdateEvent(open)`.
- `open`: MARKET -> BUY at `ask` if > 0 else LTP, SELL at `bid` if > 0 else LTP. LIMIT -> BUY fills when `ltp <= price` **at the limit price**, SELL when `ltp >= price` at the limit price. SL (already open) -> BUY `ltp >= trigger and ltp <= price` fills at LTP; SELL `ltp <= trigger and ltp >= price`. SL-M -> BUY `ltp >= trigger` at LTP; SELL `ltp <= trigger` at LTP.
- The WebSocket engine passes `{"ltp": x, "bid": x, "ask": x}` (`websocket_execution_engine.py:545-549`), so tick-driven MARKET fills are at LTP.

Execute `_execute_order(order, price)` L656-778: `claim_order_fill` (conditional UPDATE to `complete`, `average_price`, `filled_quantity = quantity`, `pending_quantity = 0`, L49-88); re-read and abort if `(quantity, price, trigger_price, price_type, action)` changed since the decision (L673-695); insert `SandboxTrades` (`tradeid = TRADE-YYYYMMDD-HHMMSS-<8hex>`, L1189-1194); commit; `_update_position`; publish `SandboxOrderFilledEvent` + `OrderUpdateEvent(complete)` (L361-430). On exception: order -> `rejected` with `rejection_reason = "Execution error: ..."` (L741-778).

Position netting `_update_position` L811-1122 under a class-level lock (L139), `cv = SymToken.contract_value or 1`:
- No row: create with `quantity = +-qty`, `average_price = ltp = price`, `margin_blocked = order.margin_blocked`, pnl fields 0.
- Row with qty 0 (reopen): `quantity = +-qty, average_price = price, ltp = price, pnl = 0, pnl_percent = 0, margin_blocked = order.margin_blocked`; `accumulated_realized_pnl` and `today_realized_pnl` kept.
- Full close (`old + new == 0`): `realized = (price - avg) * qty * cv` for long, `(avg - price) * qty * cv` for short; `release_margin(position.margin_blocked, realized)`; `accumulated += realized; today += realized; quantity = 0; margin_blocked = 0; ltp = price; pnl = today_realized_pnl; pnl_percent = 0`.
- Same direction add: `avg = (|old|*avg + |new|*price) / (|old|+|new|)`; `quantity = old + new`; `margin_blocked += order.margin_blocked`; `ltp = price`.
- Reduce / reverse: `reduced = min(|old|, |new|)`; realized on `reduced`; `accumulated += realized; today += realized`; `release = position.margin_blocked * reduced / |old|` released with the P&L; remaining margin `= current - release`. If `|new| > |old|` (reverse): `quantity = +-(|new| - |old|)`, `average_price = price`, `margin_blocked = order.margin_blocked * (|new|-|old|) / |new|`. Else `quantity = old + new`, `margin_blocked = remaining`.
- Afterwards `validate_margin_consistency` and auto `reconcile_margin` (L1105-1113), then tell the WS engine to subscribe/unsubscribe the symbol for MTM (L1095-1103, L1124-1164).

Trade book `PositionManager.get_tradebook` L1133-1205: trades with `trade_timestamp >= session start` (SESSION_EXPIRY_TIME, default 03:00, computed in naive local time L1140-1159). Row: `{tradeid, orderid, symbol, exchange, action, quantity, average_price, price, trade_value = round(price*|qty|, 2), product, strategy, timestamp}`. Response `{"status":"success","data":[...],"mode":"analyze"}`.

Order book `get_orderbook` L1199-1284: same session filter on `order_timestamp` (L1206-1235), newest first. Row: `{orderid, symbol, exchange, action, quantity, price, trigger_price, pricetype, product, order_status, average_price, filled_quantity, pending_quantity, rejection_reason, timestamp, strategy}`. `statistics` (L1437-1456): `total_buy_orders, total_sell_orders, total_completed_orders, total_open_orders, total_rejected_orders, total_trigger_pending_orders`. Response `{"status":"success","data":{"orders":[...],"statistics":{...}},"mode":"analyze"}`. `get_order_status` L1286-1334 returns the same row (with `price_type` key instead of `pricetype`) in `data`.

### 1.4 Which engine runs when (`sandbox/execution_thread.py`, `sandbox/websocket_execution_engine.py`)

- `start_execution_engine()` L175-258: type from `SANDBOX_ENGINE_TYPE` env, default `websocket`. If the WebSocket proxy answers a TCP connect on `WEBSOCKET_HOST:WEBSOCKET_PORT` (L148-172) the WS engine starts; otherwise the polling thread starts (`ExecutionEngineThread`, interval = `order_check_interval` config, default 5 s, L34-65) and an upgrade watcher (L326-382) switches to WS every 5 s once the proxy is healthy. A `GTTMaintenanceThread` (L68-120) always runs: reclaim stranded GTT claims every 60 s, expire GTTs hourly.
- WS engine: subscribes to `MarketDataService.subscribe_critical` for all symbols (L104); keeps an in-memory index `{exchange:symbol -> [orderid]}`, `{exchange:symbol -> [gtt leg id]}`, a per-user symbol refcount and a set of open-position symbols (L55-77), rebuilt from the DB at start (L142-209, skipping expired contracts) and maintained by `notify_order_placed / notify_order_completed / notify_position_opened / notify_position_closed / notify_gtt_placed` (L276-380, L495-520). Each symbol is subscribed to the proxy in `LTP` mode through `subscribe_to_symbols` (L678-704) and released when the refcount hits zero. On every tick (`_on_market_data` L382-419) it snapshots the index under a real lock, then runs `_check_and_execute_order` (L522-565: reload order if still pending, call `ExecutionEngine._process_order` with `{ltp, bid: ltp, ask: ltp}`, drop from index when no longer pending) and `_check_gtt_legs` (L421-453). A health monitor (L567-594) starts the polling fallback (`run_execution_engine_once` every `order_check_interval`) when `MarketDataService.is_data_fresh(30s)` is false and stops it when fresh.
- Polling engine `check_and_execute_pending_orders` L141-226: loads all pending orders, groups by `(symbol, exchange)`, one `get_multiquotes` call authenticated with any stored API key (L299-359), processes each order, yields every `ORDER_RATE_LIMIT` orders, then `_check_pending_gtts` (L228-257).

### 1.5 Positions, MTM and session boundary (`sandbox/position_manager.py`, `sandbox/session_boundary.py`)

- Session boundary = `SESSION_EXPIRY_TIME` env (default `03:00` IST). `last_session_expiry_utc` (`session_boundary.py:47-78`) resolves the most recent boundary and converts it to naive UTC because `created_at/updated_at` are written by SQLite `CURRENT_TIMESTAMP` (UTC) while `order_timestamp`/`trade_timestamp` are naive IST set by code. `DISABLE_SESSION_EXPIRY=true` (crypto) turns off the catch-up square-off (`catch_up_processor.py:51-57`).
- `get_open_positions(update_mtm=True)` L514-722: for each position, catch-up reset of `today_realized_pnl` by raw SQL if last updated before the boundary (L551-591, raw SQL so `updated_at` is not bumped); include if `updated_at >= boundary` and (qty != 0, or qty == 0 and today_realized != 0); if older, include only NRML with qty != 0 (L593-607). Then `_check_and_close_expired_positions` (L345-412) and `_update_positions_mtm` (L752-831: WebSocket cache if `last_update` within 5 s, else one multiquotes call; `pnl = (ltp-avg)*qty*cv` or short equivalent; `pnl_percent = (ltp-avg)/avg*100` directional). Funds `unrealized_pnl` is set to the sum over open positions (L693-694).
- Response row (L670-689): `{symbol, exchange, product, quantity, average_price (0.0 when qty==0), ltp, pnl (= today_realized + unrealized for open; today_realized for closed), pnlpercent (= pnl / |avg*qty*cv| * 100, 0 for closed), unrealized_pnl, today_realized_pnl, total_pnl_today, lot_size (= contract_value)}`. Envelope: `{"status","data":[...],"total_pnl","total_unrealized_pnl","total_today_realized_pnl","total_pnl_today","mode":"analyze"}`.
- `close_position(symbol, exchange, product)` L1052-1131 under the position lock: MARKET order for `|qty|` in the opposite direction with `strategy = "AUTO_SQUARE_OFF"`. Response `{"status","message","orderid","mode"}`.
- Expiry: `get_contract_expiry` L152-171 (regex `DDMMMYY` in the symbol, else SymToken.expiry). `is_contract_expired_now` L256-291: past the date -> expired; on the date, expired from `EXCHANGE_CLOSE_TIMES` (NFO/BFO 15:40, CDS/BCD 17:00, MCX 23:30, NCDEX 17:00, default 15:30, L177-190) when `expiry_settlement_timing == expiry_day_close` (default), never on the date when `next_day`. Settlement price `get_expiry_settlement_price` L294-324: options at last LTP (`option_expiry_settlement == ltp`, default) or 0 (`zero`); futures at last LTP else average. `_settle_expired_position` L414-512: claim row (`claim_position_for_settlement` L193-236), `stage_release_margin(position.margin_blocked, close_pnl)`, `quantity = 0, ltp = settlement, pnl = accumulated + close_pnl, accumulated = same, margin = 0`, commit, then raw-SQL `updated_at = expiry date 00:00` to hide it from the session view. Note: `today_realized_pnl` is not credited by expiry settlement (funds `today_realized_pnl` is, via `count_today=True`).
- `cleanup_expired_contracts` L1384-1542 is the same settlement run across all users (startup catch-up and the minute sweep).

### 1.6 Holdings and T+1 (`sandbox/holdings_manager.py`)

- `process_t1_settlement()` L132-304 (one at a time, L35): CNC positions with `created_at < today 00:00` (naive IST compared to a UTC column; `catch_up_processor.py:188-189` converts correctly). For each, claim; qty 0 -> delete the row; qty > 0 -> fold into the holding (weighted average L205-215), `stage_transfer_margin_to_holdings(|qty| * avg)`; qty < 0 -> `holding.quantity += qty`, `stage_credit_sale_proceeds(|qty| * avg)`; holding with qty 0 deleted; create holding (`settlement_date = today`, `ltp = position.ltp or avg`) when none exists; delete the position; single commit; publish `SandboxT1SettlementEvent`. No realized P&L is booked at settlement for a CNC sell (the credit is the sale value at the position's average sell price; the holding's cost basis is simply reduced).
- `get_holdings()` L44-130: `quantity != 0`; MTM via one multiquotes call (`pnl = (ltp-avg)*|qty|`, `pnl_percent = (ltp-avg)/avg*100`); row `{symbol, exchange, product: "CNC", quantity, average_price, ltp, pnl, pnlpercent, current_value, settlement_date}`; `statistics {totalholdingvalue, totalinvvalue, totalprofitandloss, totalpnlpercentage}`; envelope `{"status","data":{"holdings","statistics"},"mode"}`.
- Scheduled at 00:00 IST (`squareoff_thread.py:103-122`); catch-up on login/startup.

### 1.7 Auto square-off (`sandbox/squareoff_manager.py`, `sandbox/squareoff_thread.py`)

- Times per exchange (L52-61): NSE, BSE, **NFO, BFO** -> `nse_bse_square_off_time` (15:15); CDS, BCD -> `cds_bcd_square_off_time` (16:45); MCX -> `mcx_square_off_time` (23:30); NCDEX -> `ncdex_square_off_time` (17:00). There is no separate NFO/BFO key in the web.
- `check_and_square_off()` L72-177 (one sweep at a time, L42): (1) cancel every pending MIS order whose exchange is past its time via `OrderManager.cancel_order` (margin released) L236-291; (1b) cancel pending orders on expired contracts L179-234; (1c) `cleanup_expired_contracts()`; (2) for every MIS position with qty != 0 past its exchange time, `PositionManager.close_position` (MARKET, `AUTO_SQUARE_OFF`) L293-322; publish `SandboxAutoSquareOffEvent(cancelled_orders, closed_positions)` if anything changed.
- Scheduler (`squareoff_thread.py`, APScheduler, IST): cron per config group at its HH:MM (`misfire_grace_time=300`) L61-81; backup interval every 1 minute L90-98; T+1 at 00:00 L103-122; auto reset cron on `reset_day/reset_time` L124-168; daily P&L snapshot at 23:59 skipping weekends/holidays L170-285 (row: `realized_pnl = funds.today_realized_pnl`, `positions_unrealized_pnl`, `holdings_unrealized_pnl`, `total_mtm`, `available_balance`, `used_margin`, `portfolio_value = available + used`); daily P&L reset at `SESSION_EXPIRY_TIME` zeroing `today_realized_pnl` on funds and positions L287-331. `reload_squareoff_schedule` L429-455 is called when any `*_square_off_time`, `reset_day` or `reset_time` config changes (`blueprints/sandbox.py:268-300`). `get_squareoff_scheduler_status` L408-426 returns `{running, timezone, jobs:[{id,name,next_run}]}`.

### 1.8 Catch-up processor (`sandbox/catch_up_processor.py`)

`run_catch_up_tasks()` L343-388 (single-flight): `catch_up_mis_squareoff` L30-172 (MIS positions with `updated_at < last boundary` and a configured exchange: claim, settle at last LTP else avg, `stage_prior_session_release` so P&L goes to all-time only, position `today_realized_pnl = 0`); `catch_up_t1_settlement` L175-205; `catch_up_daily_pnl_reset` L208-254 (zero stale `today_realized_pnl` on positions and funds); `catch_up_daily_pnl_snapshot` L257-340 (backfill yesterday if a trading day, `realized = all_time - today`, unrealized 0); `catch_up_gtts` L391-437 (reclaim stranded legs, evaluate every active leg against a multiquotes snapshot, claim + fire).

### 1.9 GTT in sandbox (`sandbox/gtt_manager.py`)

- Tables `sandbox_gtt` / `sandbox_gtt_legs` (`database/sandbox_db.py:299-460`). `gtt_id = GTT-YYMMDD-<8hex>`. States: GTT `active -> triggered | cancelled | expired | rejected`; leg `pending -> triggering -> triggered | cancelled`.
- `place_gtt` L181-310: legs from `_build_legs` L312-374 (SINGLE: exactly one of `triggerprice_sl` (direction `below`) / `triggerprice_tg` (`above`); OCO: both, same action, limit from `stoploss`/`target` else `price`); margin per leg at `_pricing_basis` (limit, else trigger, else LTP) L113-128; blocked = max of legs for OCO when `gtt_oco_margin_mode == max` (default) else sum; staged margin + rows in one commit; default expiry 365 days (`_resolve_expiry` L83-110); notifies the WS engine. Response `{"status":"success","mode":"analyze","trigger_id"}`.
- `modify_gtt` L378-497 (immutable: action, symbol, exchange, trigger_type L499-536; same leg count; margin delta staged); `cancel_gtt` L538-635 (conditional claim on `active`, legs cancelled, margin released, one commit); `list_gtts(status_filter="active")` L637-701 shape `{trigger_id, trigger_type, status, symbol, exchange, trigger_prices[], last_price, legs[{action, quantity, price, pricetype, product, triggered_order_id}], created_at, updated_at, expires_at, strategy, margin_blocked}`.
- Trigger `leg_is_triggered_by(direction, trigger, ltp)` L131-150: `above` -> `ltp >= trigger`; else `ltp <= trigger` (inclusive).
- `try_claim_trigger(leg_id)` L723-779: conditional UPDATE `pending -> triggering` requiring parent `active` and no sibling in `triggering|triggered` (OCO exclusivity). `fire_leg` L911-955 tries the position lock without waiting (reverts the claim if busy), `_fire_claimed_leg` L957-1095: parent `active -> triggered` conditionally, release the GTT reservation (`stage_margin_delta(-released)`, `margin_blocked = 0`, commit), `OrderManager.place_order` with `gtt_leg_id`, mark leg `triggered` with `triggered_order_id`, cancel the OCO sibling, publish `GTTTriggeredEvent`. Failures compensate (`_compensate_failed_fire` L782-843: restore margin or mark `rejected`).
- Maintenance: `reclaim_stranded_legs` L1218-1255 (`claimed_at < now - gtt_claim_timeout_sec`), `reclaim_stranded_parents` L1258-1395, `expire_due_gtts` L1412-1479 (publishes `GTTExpiredEvent`).

### 1.10 Concurrency: position locks (`sandbox/position_locks.py`)

Keyed re-entrant locks per `(user_id, symbol, EXCHANGE, PRODUCT)` (L62-69). Held by `place_order` (order_manager L66), `close_position` (position_manager L1054), smart order (sandbox_service L439-455), and tried without waiting by GTT firing (gtt_manager L946). Wait bounded to 30 s only under gthread (L50, L72-74); refusal returns 409 `{"status":"error","message":"Another order for X is still being processed. Try again in a moment.","mode":"analyze"}` (L121-132). Plus: order fill claim (conditional UPDATE), funds CAS, position settlement claim, GTT leg claim, one square-off sweep at a time, one T+1 at a time, one catch-up at a time, one mode transition at a time.

### 1.11 Config keys and defaults (`database/sandbox_db.py:545-666`)

| key | default | validation (`blueprints/sandbox.py:888-972`) / effect |
|---|---|---|
| starting_capital | 10000000.00 | one of 100000, 500000, 1000000, 2500000, 5000000, 10000000; on change `rebase_starting_capital` |
| reset_day | Never | Monday..Sunday or Never; reloads schedule |
| reset_time | 00:00 | HH:MM; reloads schedule |
| order_check_interval | 5 | 1-30 s (polling engine) |
| mtm_update_interval | 5 | 0-60 s (0 = manual) |
| nse_bse_square_off_time | 15:15 | HH:MM; applies to NSE, BSE, NFO, BFO; reloads schedule |
| cds_bcd_square_off_time | 16:45 | HH:MM |
| mcx_square_off_time | 23:30 | HH:MM |
| ncdex_square_off_time | 17:00 | HH:MM |
| equity_mis_leverage | 5 | 1-50 |
| equity_cnc_leverage | 1 | 1-50 (also used for NRML on NSE/BSE) |
| futures_leverage | 10 | 1-50 |
| option_buy_leverage | 1 | 1-50 |
| option_sell_leverage | 1 | 1-50 |
| order_rate_limit | 10 | unused |
| api_rate_limit | 50 | unused |
| smart_order_rate_limit | 2 | unused |
| smart_order_delay | 0.5 | unused |
| expiry_settlement_timing | expiry_day_close | or next_day |
| option_expiry_settlement | ltp | or zero |
| gtt_oco_margin_mode | max | or sum |
| gtt_claim_timeout_sec | 60 | integer |

Env: `SESSION_EXPIRY_TIME` (03:00), `SANDBOX_ENGINE_TYPE` (websocket), `SANDBOX_ENGINE_FALLBACK` (true), `DISABLE_SESSION_EXPIRY` (false), `SANDBOX_DATABASE_URL` (sqlite:///db/sandbox.db).

### 1.12 Tables and columns (`database/sandbox_db.py`)

- `sandbox_orders` (L66-116): id, orderid (unique), user_id, strategy, symbol, exchange, action, quantity, price DECIMAL(10,2), trigger_price, price_type, product, order_status (default open), average_price, filled_quantity (0), pending_quantity, rejection_reason, margin_blocked DECIMAL(10,2) (0), gtt_leg_id (unique, nullable), order_timestamp, update_timestamp. Indexes: (user_id, order_status), (symbol, exchange).
- `sandbox_trades` (L119-140): id, tradeid (unique), orderid, user_id, symbol, exchange, action, quantity, price, product, strategy, trade_timestamp.
- `sandbox_positions` (L143-186): id, user_id, symbol, exchange, product, quantity, average_price, ltp, pnl, pnl_percent DECIMAL(10,4), accumulated_realized_pnl, today_realized_pnl, margin_blocked DECIMAL(15,2), created_at, updated_at. UNIQUE(user_id, symbol, exchange, product).
- `sandbox_holdings` (L189-213): id, user_id, symbol, exchange, quantity, average_price, ltp, pnl, pnl_percent, settlement_date DATE, created_at, updated_at. UNIQUE(user_id, symbol, exchange).
- `sandbox_funds` (L216-249): id, user_id (unique), total_capital, available_balance, used_margin, realized_pnl, today_realized_pnl, unrealized_pnl, total_pnl, last_reset_date, reset_count, created_at, updated_at.
- `sandbox_daily_pnl` (L252-284): id, user_id, date, realized_pnl, positions_unrealized_pnl, holdings_unrealized_pnl, total_mtm, available_balance, used_margin, portfolio_value, created_at. UNIQUE(user_id, date).
- `sandbox_config` (L287-296): id, config_key (unique), config_value TEXT, description, updated_at.
- `sandbox_gtt` (L299-370): id, gtt_id (unique), user_id, strategy, trigger_type ('single'|'two-leg'), symbol, exchange, last_price, gtt_status, margin_blocked, expires_at, created_at, updated_at (IST).
- `sandbox_gtt_legs` (L373-460): id, gtt_id FK cascade, leg_number, trigger_price, trigger_direction ('below'|'above'), action, quantity, price, pricetype, product, leg_status, triggered_order_id, leg_margin, claimed_at, created_at, updated_at. Index (leg_status, claimed_at).

### 1.13 Events and SocketIO

Bus events published by the sandbox: `OrderUpdateEvent` topic `order.update` (`events/order_events.py:82-109`; fields orderid, symbol, exchange, action, quantity, price, trigger_price, pricetype, product, order_status, filled_quantity, pending_quantity, average_price, rejection_reason, broker="sandbox") on open / trigger pending / open-after-trigger / complete / cancelled / rejected; `SandboxOrderFilledEvent` (`sandbox.order_filled`), `SandboxAutoSquareOffEvent` (`sandbox.auto_squareoff`), `SandboxT1SettlementEvent` (`sandbox.t1_settlement`) (`events/sandbox_events.py`); `GTTTriggeredEvent`, `GTTExpiredEvent` (`gtt_manager.py:1140-1215`); service-layer `OrderPlacedEvent`, `OrderCancelledEvent`, `OrderModifiedEvent`, `PositionClosedEvent`, `AnalyzerErrorEvent` etc. with `mode="analyze"`.

SocketIO (`subscribers/socketio_subscriber.py`): every analyze-mode event -> `analyzer_update` with payload `{"request": event.request_data, "response": event.response_data}` (L270-275, L224-239); `order.update` -> `order_update` with the full field list (L242-266); also relayed over ZMQ to the WebSocket proxy on topic `ANALYZE_{user_id}_orders` (`subscribers/wsproxy_subscriber.py:17-70`). The UI listens: `analyzer_update` (`frontend/src/hooks/useSocket.ts:264` toasts and plays the `analyzer` sound unless api_type is passive; `useOrderEventRefresh` default events `['order_event','analyzer_update']`), `order_update` (`components/trading/dock/useBlotter.ts:182`), `close_position_event`, `cancel_order_event`, `modify_order_event`, `gtt_event` (live only).

### 1.14 REST and page endpoints

`/api/v1` (flask-restx): `POST /analyzer` `{apikey}` -> `{"status":"success","data":{"mode":"analyze|live","analyze_mode":bool,"total_logs":int}}`; `POST /analyzer/toggle` `{apikey, mode:bool}` -> same `data` plus `"message":"Analyzer mode switched to analyze|live"`; 403 `"Operation analyzer/toggle is not allowed in Semi-Auto mode..."`; 409 `MODE_BUSY_MESSAGE`. `POST /pnl/symbols` `{apikey}` (sandbox only, `restx_api/pnl_symbols.py:23-47`) -> `{"status","data":[{symbol, exchange, product, quantity, pnl, unrealized_pnl, today_realized_pnl, total_pnl_today}],"total_pnl","total_unrealized_pnl","total_today_realized_pnl","total_pnl_today","mode"}`. All order/book/account endpoints return the sandbox shapes above with `"mode":"analyze"` when the flag is on; `placesmartorder` returns `"Positions Already Matched. No Action needed."` / `"No OpenPosition Found. Not placing Exit order."` (sandbox_service L505-511); `cancelallorder` -> `{status, message, canceled_orders[], failed_cancellations[], mode}`; `closeposition` without symbol -> `{status, message, closed_positions, failed_closures, mode}`.

`/sandbox` (`blueprints/sandbox.py`, session auth): `GET /sandbox/api/configs` -> `{"status":"success","configs":{capital:{title,configs:{key:{value,description}}}, leverage, square_off, intervals, expiry}}` (L104-217); `POST /sandbox/update` `{config_key, config_value}` -> `{"status","message"}` with validation and side-effects (L220-312); `POST /sandbox/reset` -> wipes the user's orders, trades, positions, holdings, daily P&L, resets funds and `reset_count += 1`, resets the 14 editable config keys to defaults, under the mode lock (L315-503); `POST /sandbox/reload-squareoff`; `GET /sandbox/squareoff-status`; `GET /sandbox/mypnl/api/data` -> `{"status","data":{summary:{today_realized_pnl, all_time_realized_pnl, positions_unrealized_pnl, holdings_unrealized_pnl, total_unrealized_pnl, today_total_mtm, total_pnl, available_balance, total_capital}, daily_pnl:[{date, realized_pnl, positions_unrealized, holdings_unrealized, total_unrealized, total_mtm, portfolio_value}] (30), positions:[{symbol, exchange, product, quantity, average_price, ltp, unrealized_pnl, today_realized_pnl, all_time_realized_pnl, status, updated_at}], holdings:[...], trades:[...] (50)}}` (L550-717); CSV exports `GET /sandbox/mypnl/export/{daily|positions|holdings|trades}` (L1150-1277). `GET /settings/analyze-mode`, `POST /settings/analyze-mode/<0|1>`, `POST /auth/analyzer-toggle`. `/analyzer/api/data?start_date&end_date` -> `{"status","data":{"stats":{total_requests, issues:{total,...}, symbols[], sources[]}, "requests":[{timestamp, api_type, source, request_data, response_data, analysis:{issues, error, error_type, warnings}, symbol?, exchange?, action?, quantity?, orderid?, position_size?}]}}` (`blueprints/analyzer.py:31-275`), `/analyzer/export`, `/analyzer/stats`, `/analyzer/requests`, `/analyzer/clear`.

Web pages: `frontend/src/pages/Sandbox.tsx` (five config categories from `/sandbox/api/configs`, `/sandbox/update`, `/sandbox/reset`), `SandboxPnL.tsx` (`/sandbox/mypnl/api/data`, exports), `Analyzer.tsx` (`/analyzer/api/data`, `/analyzer/export`). Navbar toggle: `frontend/src/stores/themeStore.ts:111-141` POSTs `/auth/analyzer-toggle` with CSRF and sets `appMode` from `data.analyze_mode`.

---

## Part 2. Parity table

Status legend: OK / Bug / Missing. "Desktop" lines reference `openalgo-desktop/`.

### 2.1 Mode and routing

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| Flag persisted, default Live | OK | `src-tauri/src/db/sqlite/migrations.rs:549-551`, `mod.rs:576-590` | none |
| Toggle starts/stops engine + scheduler + catch-up as one locked step | Missing | `src-tauri/src/services/analyzer_service.rs:36-42` only sets the flag; `commands/settings.rs:464-471` same | `AnalyzerService::toggle_mode` must call `SandboxRuntime::reconcile(mode)` under a `tokio::sync::Mutex`; see Part 3 |
| Toggle refused in semi-auto order mode (403) | Missing | `src-tauri/src/webhook/handlers.rs:1369-1407` | not applicable until desktop has order modes; document |
| Mode string `"analyze"` in status | Bug | `commands/settings.rs:458,469` returns `"analyzer"`; `services/analyzer_service.rs:31` returns `"analyze"` | use `"analyze"` everywhere; frontend `themeStore.ts:13` uses `'analyzer'` for its own enum, fine, but REST/invoke payloads must say `analyze` |
| `total_logs` = analyzer_logs count | Bug | `services/analyzer_service.rs:26` counts `order_logs` | count `analyzer_logs`; and start writing them (next row) |
| Every analyze-mode API call is recorded in analyzer_logs and surfaced on /analyzer | Missing | `create_analyzer_log` has no callers (`grep` over `src-tauri/src`); `webhook/handlers.rs:289-358` never logs | in each handler, when `result.mode == "analyze"` call `state.sqlite.create_analyzer_log(api_type, request_json minus apikey, response_json)` |
| `/analyzer/api/data`, `/analyzer/export` | Missing | `src/pages/Analyzer.tsx:97,125` `fetch('/analyzer/api/data')` relative URL has no Tauri route (`webhook/server.rs:97-162`) | add `get_analyzer_data`/`export_analyzer_csv` Tauri commands over `analyzer_logs` returning the web shape; rewrite page to invoke |
| Startup reconciles engine to persisted flag; login runs catch-up | Missing | no caller in `src-tauri/src/lib.rs` | call `SandboxRuntime::reconcile(get_analyze_mode())` in `setup`, and `catch_up::run()` after broker login |
| Routing of placeorder/modify/cancel/cancelall/closeposition/smart/basket/split/orderbook/tradebook/positionbook/openposition/orderstatus/funds/holdings | OK (routing) | `services/order_service.rs:56-60,119-128,160-179`, `orderbook_service.rs:50-54,83-87`, `position_service.rs:43-47,119-137`, `funds_service.rs:35-39`, `holdings_service.rs:35-39`; smart/basket/split go through `OrderService::place_order` | keep; the sandbox implementations behind them are the problem (below) |
| `force_live` opt-out for a strategy mid-run | Missing | `services/order_service.rs:48-60` | add `force_live: bool` to `OrderService::place_order` |
| GTT endpoints route to sandbox | Missing | no GTT routes in `webhook/server.rs` | Part 3 `gtt.rs` |
| `POST /api/v1/pnl/symbols` | Missing | not in `webhook/server.rs` | add route over `positions::open_positions` |

### 2.2 Funds and margin

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| Funds row per user with 7 money columns, `starting_capital` default 1 crore | Bug | `migrations.rs:357-366`: `available_cash DEFAULT 1000000` (10 lakh), `total_value`, no realized/unrealized/today/total_capital/last_reset/reset_count | new schema (Part 3.5); seed from `sandbox_config.starting_capital` |
| Reset returns balances to `starting_capital`, `reset_count += 1` | Bug | `db/sqlite/sandbox.rs:175-183` hard-codes `1000000`; `src/pages/Sandbox.tsx:417` tells the user "1.00 Crore" | read `starting_capital`; implement `reset_count` |
| Margin = qty*price/leverage with leverage by exchange/product/instrument (5 keys) | Missing | no margin code anywhere; `sandbox.rs:68-114` never touches funds | `funds.rs::calculate_margin_required` + `leverage_for()` |
| Block on placement (net against opposite position; CNC SELL blocks nothing); refuse when insufficient | Missing | `sandbox.rs:68-114` | `orders.rs::place_order` step 7 |
| Release exact `margin_blocked` on close, proportional on reduce, re-block excess on reverse | Missing | `sandbox.rs:117-171` | `positions.rs::apply_fill` |
| Compare-and-set funds writes; refuse over-release; `reconcile_margin` after fills | Missing | - | single `Mutex<Connection>` already serialises; still write funds as one `UPDATE ... WHERE available_balance >= ?` and keep `reconcile()` |
| `GET funds` shape `availablecash, collateral, m2munrealized, m2mrealized, total_realized_pnl, today_realized_pnl, utiliseddebits, grossexposure, totalpnl, last_reset, reset_count` | Bug | `services/funds_service.rs:79-99` returns `available_cash, used_margin, total_margin, opening_balance, ...` (live Funds struct); `src/pages/Dashboard.tsx:100-106` reads `sandboxFunds.available_capital/unrealized_pnl/realized_pnl` which do not exist on `SandboxFunds` (`src/api/tauri-client.ts:350-355`) so the dashboard renders `undefined` | return the web dict for analyze mode; fix Dashboard field names |
| `rebase_starting_capital` when the config changes | Missing | `sandbox.rs:420-444` writes the column only | in `config.rs::set("starting_capital")` call `funds::rebase` |
| Weekly auto-reset (`reset_day/reset_time`) | Missing | columns exist (`migrations.rs:520-521`), nothing reads them | scheduler job (Part 3.4) |
| Unrealized P&L into funds on every MTM | Missing | - | `positions.rs::update_mtm` |

### 2.3 Orders

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| Validation: fields, enums, product/exchange compatibility, qty > 0, price/trigger requirements, exchange list, symbol exists, lot size | Missing | `sandbox.rs:68-114` accepts anything | `orders.rs::validate` |
| MIS time gate after square-off / before 09:00 unless reducing | Missing | - | `orders.rs` using `squareoff::time_for(exchange)` and the clock |
| CNC SELL requires `signed position + holdings >= qty`; rejection persisted with `rejection_reason`, `margin_blocked = 0` | Missing | no `rejection_reason`/`strategy`/`trigger_price`/`margin_blocked`/`pending_quantity` columns (`migrations.rs:293-310`) | schema + logic |
| MARKET margin price = live LTP (prefetched / 3 retries / last position LTP / reject) | Bug | `sandbox.rs:81-86` fills a MARKET order at the caller's `price`; `/api/v1/placeorder` sends `price = 0` for MARKET (`handlers.rs:312-325`) so positions open at 0.00 and `pnl = qty * ltp` | `QuoteSource::ltp()` (cache first, then broker `get_quotes`), reject when unavailable |
| LIMIT marketability / SL trigger-met checks at placement; fills at LTP | Missing | - | `orders.rs` steps 6 and 10 |
| Status vocabulary `open` / `trigger pending` / `complete` / `cancelled` / `rejected` | Bug | `sandbox.rs:84` `"pending"`; `orderbook_service.rs:186` filters on `complete|COMPLETE|FILLED` | use web statuses; add CHECK constraint |
| Order id `YYMMDD` + 8 digits | Bug | `sandbox.rs:78` `SB<12 hex>` | web format |
| Pending LIMIT/SL/SL-M filled by engine | Missing | nothing ever changes `status = 'pending'` after insert | `execution.rs` |
| Trade row per fill; tradebook from `sandbox_trades` with `trade_value` | Bug | `sandbox_trades` exists (`migrations.rs:328-340`) but is never written; `orderbook_service.rs:180-208` fakes the tradebook from completed orders; trades table lacks `product`, `strategy`, `user_id` | insert in `execution::fill`; tradebook from trades |
| Orderbook row has `trigger_price, pricetype, order_status, rejection_reason, strategy, timestamp` + `statistics` | Bug | `orderbook_service.rs:145-178` `trigger_price: 0.0`, `rejection_reason: None`, no statistics, `LIMIT 100` (`sandbox.rs:39-43`) | full row + session filter + statistics |
| Orderbook/tradebook filtered to the current session (03:00 boundary) | Missing | `sandbox.rs:39-43` returns last 100 | `session.rs::session_start()` |
| Modify: pending only; lot size; price only LIMIT/SL; trigger only SL/SL-M; conditional UPDATE | Bug | `services/order_service.rs:121-128` returns success without writing anything | implement `orders.rs::modify` |
| Cancel from `open` or `trigger pending`; release `margin_blocked`; publish | Bug | `sandbox.rs:270-279` only `status = 'pending'`, no margin, no event | implement; return 400 `"Cannot cancel order in X status"` |
| `cancelallorder` returns `canceled_orders[]`, `failed_cancellations[]` | Missing (shape) | `handlers.rs:526-570` | reuse per-order cancel |
| `OrderUpdateEvent` on every status transition | Missing | no emits besides `api_*` (`handlers.rs:331,395,456,...`) | `events.rs` -> Tauri `order_update`, `analyzer_update` |
| Stale-quote guard (LTP outside day range) | Missing | - | `execution.rs::quote_looks_stale` |
| Fill claim is a conditional UPDATE; modify-during-fill re-evaluates | Missing | - | same pattern in rusqlite (`changes() == 1`) |

### 2.4 Positions, MTM, session

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| Netting with realized P&L, accumulated/today realized, margin per position, reverse sets avg = fill price | Bug | `sandbox.rs:139-161`: on reduce keeps `current_avg`; on reversal (`new_qty` crosses zero) also keeps `current_avg` instead of the fill price; no realized P&L anywhere; `qty == 0` zeroes avg | `positions.rs::apply_fill` per 1.3 |
| Position columns `pnl_percent, accumulated_realized_pnl, today_realized_pnl, margin_blocked, user_id` | Missing | `migrations.rs:312-326` | schema |
| `contract_value` multiplier in P&L | Missing | - | read from symbol cache (`state.rs:36-46` has `lot_size`, add `contract_value`) |
| Session filter: today's closed positions shown (qty 0 with today P&L); old MIS/CNC hidden; NRML carried | Missing | `sandbox.rs:12-33` `WHERE quantity != 0` | `positions.rs::open_positions` with `last_session_expiry` |
| `today_realized_pnl` reset at 03:00 and catch-up reset | Missing | - | scheduler job + `catch_up.rs` |
| MTM: feed cache (<=5 s) else multiquotes; `pnl`, `pnl_percent`; funds unrealized | Bug | `sandbox.rs:233-256` only when the UI invokes `update_sandbox_ltp`, which nothing calls (`grep updateSandboxLtp` in `src/`: no callers); `SandboxPnL.tsx:96` recomputes `qty*(ltp-avg)` client-side | engine updates MTM on ticks (throttled) and `open_positions()` refreshes from the tick cache |
| Positionbook row: `pnl = today_realized + unrealized`, `pnlpercent`, `average_price = 0` when closed, totals in the envelope | Bug | `position_service.rs:224-253` maps only quantity/avg/ltp/pnl, `realized_pnl: 0.0` | build the web row |
| `closeposition` places a MARKET order with strategy `AUTO_SQUARE_OFF` under the position lock | Bug | `position_service.rs:121-137` calls the stub at the stored `ltp` (which is the entry price since MTM never runs) | route through `orders::place_order(MARKET)` |
| Expired F&O settlement (timing config, LTP/zero options) | Missing | - | `positions.rs::settle_expired` + minute sweep |
| Position lock per (user, symbol, exchange, product) around read-then-order paths | Missing | `SqliteDb.conn: Mutex<Connection>` (`connection.rs:34-36`) serialises SQL but not the read-decide-write across a quote fetch | `locks.rs` keyed `tokio::sync::Mutex` |

### 2.5 Holdings and T+1

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| T+1 at 00:00 and on catch-up moves CNC positions to holdings, transfers margin, credits sell proceeds | Missing | holdings table exists (`migrations.rs:342-355`) and is never written | `holdings.rs::process_t1` |
| Holdings row with `pnlpercent, current_value, settlement_date, product: CNC` + statistics | Bug | `holdings_service.rs:79-114` has no `settlement_date` column to map; `pnl_percentage` computed from `pnl` | schema + web shape |
| Holdings MTM via multiquotes | Missing | `sandbox.rs:242-250` only via the uncalled command | `holdings.rs::update_mtm` |

### 2.6 Square-off, scheduler, catch-up

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| Exchange times from 4 config keys; NFO/BFO share `nse_bse` 15:15 | Bug | `migrations.rs:532-535` separate `nfo_square_off_time 15:25`, `cds 16:55`, `mcx 23:25`, no NCDEX/BSE/BFO/BCD keys; `src/pages/Sandbox.tsx:62-65` | use web keys and defaults (15:15 / 16:45 / 23:30 / 17:00) and the web exchange map |
| Cron per exchange + 1-min backup + T+1 + snapshot 23:59 + P&L reset 03:00 + weekly reset; reload on config change | Missing | only `scheduler/auto_logout.rs` exists | `squareoff.rs` scheduler (Part 3.4) |
| Cancel pending MIS orders, cancel expired-contract orders, settle expired positions, close MIS positions, publish `SandboxAutoSquareOffEvent` | Missing | - | `squareoff.rs::sweep` |
| `GET /sandbox/squareoff-status`, `POST /sandbox/reload-squareoff` | Missing | - | Tauri commands `get_squareoff_status`, `reload_squareoff` |
| Startup/login catch-up: stale MIS, T+1, P&L reset, snapshot backfill, GTT | Missing | - | `catch_up.rs` |
| Daily P&L snapshot rows (`realized_pnl, positions_unrealized_pnl, holdings_unrealized_pnl, total_mtm, available_balance, used_margin, portfolio_value`) | Bug | `migrations.rs:368-378` has `unrealized_pnl, total_pnl` instead and no writer; `sandbox.rs:470-523` derives `today_realized_pnl` and `all_time_realized_pnl` from this empty table, so the P&L page always shows 0 | schema + 23:59 job; summary from funds |

### 2.7 GTT

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| place/modify/cancel/list GTT, OCO, margin reservation, claim-based firing, expiry, stranded recovery | Missing | no tables, no routes | `gtt.rs` (Part 3.6); can land after the core engine |

### 2.8 Config and UI

| Web behaviour | Desktop status | file:line | Fix |
|---|---|---|---|
| 22 key/value config rows with descriptions; `/sandbox/api/configs` grouped in 5 categories | Bug | single-row table with 17 typed columns, 13 of which have no web counterpart (`migrations.rs:516-539`, `sandbox.rs:282-383`); UI hard-codes labels (`src/pages/Sandbox.tsx:49-112`) | key/value table seeded with web defaults; `get_sandbox_configs` returning the grouped shape so the web `Sandbox.tsx` ports unchanged |
| Validation on update (capital set, leverage 1-50, intervals, HH:MM, reset_day, enums) | Missing | `sandbox.rs:420-444` validates the key only | `config.rs::validate` |
| Reset clears orders/trades/positions/holdings/daily P&L, resets funds and the 14 editable keys, under the mode lock | Bug | `sandbox.rs:175-183` leaves config, sets 10 lakh, no `reset_count`; `analyzer_service.rs:45-48` | implement `_wipe_sandbox_account` equivalent |
| `/sandbox/mypnl/api/data` summary (`today_realized_pnl, all_time_realized_pnl, positions_unrealized_pnl, holdings_unrealized_pnl, total_unrealized_pnl, today_total_mtm, total_pnl, available_balance, total_capital`), positions include closed rows with `status`, CSV exports | Bug | `sandbox.rs:456-523` summary lacks `total_unrealized_pnl/total_pnl/available_balance/total_capital`, positions exclude closed rows, no exports; `src/pages/SandboxPnL.tsx` has no export buttons | compute from funds; add `export_sandbox_csv(kind)` command |
| Navbar toggle shows web warning toast; `appMode` synced from backend | OK | `src/components/layout/Navbar.tsx:40-56`, `src/stores/themeStore.ts:128-175` | none (string `'analyzer'` is UI-internal) |
| Pages refresh on `analyzer_update` / `order_update` events | Bug | `src/hooks/useOrderEventRefresh.ts:43-70` listens for Tauri events `order_event`, `analyzer_update` that the backend never emits (only `api_order`, `api_smart_order`, ... `handlers.rs:331-986`) | `events.rs` emits `analyzer_update {request, response}` and `order_update {...}` with the web payloads |
| Analyzer sound/toast on `analyzer_update` | Missing | no desktop equivalent of `useSocket.ts:264-310` | port after events exist |

---

## Part 3. Rust design for the desktop sandbox engine

### 3.1 Module layout (`src-tauri/src/sandbox/`)

```
sandbox/
  mod.rs          SandboxRuntime (owns the tasks, channels, locks), start/stop/reconcile
  clock.rs        trait Clock { fn now(&self) -> DateTime<Tz> }  (SystemClock, FakeClock)
  config.rs       typed accessors over sandbox_config key/value; validate(); defaults (1.11)
  db.rs           schema (3.5), migrations, row structs, all SQL (rusqlite), one fn per statement
  funds.rs        FundManager: block/release/transfer/credit/prior_session_release/update_unrealized/reset/rebase/reconcile, margin + leverage
  orders.rs       validate, place (1.3 steps 1-11), modify, cancel, orderbook, order_status, id generation
  execution.rs    fill decision (`decide(order, quote) -> Option<FillPrice>`), claim_fill, execute (trade + apply_fill), stale guard
  positions.rs    apply_fill netting, open_positions (session filter + MTM), close_position, tradebook, expiry settlement
  holdings.rs     get_holdings, process_t1, holdings MTM
  squareoff.rs    exchange time map, sweep(), scheduler task (3.4), status
  session.rs      last_session_expiry(now), session_start(now), is_session_expiry_disabled
  catch_up.rs     run_catch_up_tasks (1.8)
  gtt.rs          GTT manager + claim/fire/reclaim/expire (1.9)
  locks.rs        PositionLocks: DashMap<PositionKey, Arc<tokio::sync::Mutex<()>>> with refcount, try_lock variant
  quotes.rs       trait QuoteSource { async fn ltp(&self, sym, exch) -> Option<Quote>; async fn batch(..) }  (FeedCacheThenRest, Fixture)
  events.rs       SandboxEvent enum -> internal bus -> Tauri emit (`order_update`, `analyzer_update`)
  engine.rs       the tokio execution task (3.2)
```

Everything that touches SQL goes through `db.rs` and runs inside `tokio::task::spawn_blocking` (rusqlite is sync and `SqliteDb.conn` is a `parking_lot::Mutex<Connection>`; never hold it across an `.await`). Move the sandbox tables to their own file `sandbox.db` (web isolation) with a second `Mutex<Connection>`, WAL mode, `busy_timeout` 100 ms with retry in Rust.

### 3.2 Event-driven execution task (mirrors `websocket_execution_engine.py`)

- Feed tap: add `tick_tx: tokio::sync::broadcast::Sender<MarketTick>` to `WebSocketManager` (`src-tauri/src/websocket/manager.rs:127`) and send every parsed tick in the loop at `manager.rs:238-243` alongside the `app_handle.emit("market_tick")`. Also keep a `DashMap<SymbolKey, (MarketTick, Instant)>` tick cache in `AppState` for `QuoteSource::FeedCacheThenRest` (fresh if age <= 5 s, matching `WEBSOCKET_DATA_MAX_AGE`).
- `engine.rs` task, one per runtime:

```rust
pub enum EngineCmd { OrderPlaced(OrderRef), OrderResolved(OrderRef), PositionChanged{user, symbol, exchange},
                     GttPlaced(GttRef), ConfigChanged, Rebuild, Shutdown }

loop { tokio::select! {
    Ok(tick) = ticks.recv()        => on_tick(tick).await,          // index lookup, then per-order decide/fill
    Some(cmd) = cmds.recv()         => on_cmd(cmd).await,           // index maintenance + subscribe/unsubscribe
    _ = fallback.tick(), if stale   => poll_once().await,           // order_check_interval; only while feed stale > 30 s
    _ = health.tick()               => stale = last_tick.elapsed() > 30s,
    _ = gtt_reclaim.tick()          => gtt::reclaim_stranded_legs(),  // 60 s
    _ = gtt_expiry.tick()           => gtt::expire_due_gtts(),        // 3600 s
}}
```

- Index (in-task, no lock needed): `HashMap<SymbolKey, Vec<OrderId>>`, `HashMap<SymbolKey, Vec<LegId>>`, `HashSet<(User, SymbolKey)>` for open positions, `HashMap<SymbolKey, usize>` refcount driving `WebSocketManager::subscribe/unsubscribe` in `Ltp` mode. Rebuilt from the DB at start (skip expired contracts) exactly as `_rebuild_order_index`.
- `on_tick`: `quote = Quote{ltp, bid: ltp, ask: ltp, high: 0, low: 0}` (parity with the web's tick-built quote, so the stale guard is inert on ticks); for each indexed order: `spawn_blocking(|| execution::process_order(db, order_id, quote, clock, bus))`, which re-reads the order, decides, claims and fills; drop from index when it leaves the pending set. Then GTT legs. MTM: update `positions.ltp/pnl/pnl_percent` for the symbol at most once per `mtm_update_interval` seconds per symbol (0 = never), and recompute funds `unrealized_pnl` on the same cadence; emit a throttled `sandbox.mtm` for the UI.
- `poll_once` (fallback) = `execution::check_and_execute_pending_orders` with `QuoteSource::batch` (broker `get_quotes` loop, or multiquotes if the broker trait gains it).
- Fills run with the position lock held (`locks.rs`), and the fill claim is `UPDATE sandbox_orders SET order_status='complete', ... WHERE id=? AND order_status IN ('open','trigger pending')` checked with `conn.changes() == 1`; the trade insert and position update follow in the same transaction (the web commits the fill then updates the position; one transaction is strictly safer and gives the same observable result).

### 3.3 Concurrency model

- Per-position re-entrancy: tokio mutexes are not re-entrant, so `orders::place_order_locked(guard, ...)` takes the guard as a parameter and `close_position` / smart order / GTT fire acquire once and pass it down. `PositionLocks::lock(key).await` (unbounded wait, like eventlet) and `try_lock(key)` for GTT firing on the tick path (never wait on the feed task).
- Mode transitions and reset: `SandboxRuntime.mode_lock: tokio::sync::Mutex<()>`; reset pauses the engine (`Shutdown` + await join) and restarts it only if the flag is still on, as `blueprints/sandbox.py:360-425`.
- One sweep at a time: `squareoff.sweep_lock: tokio::sync::Mutex<()>`; same for T+1 and catch-up (`try_lock`, skip if busy).
- DB-level claims identical to the web: fill claim, cancel/modify conditional UPDATE on the pending set, settlement claim (`UPDATE ... WHERE id=? AND quantity=?`), GTT leg claim with the sibling/parent predicates, funds CAS (`UPDATE sandbox_funds SET ... WHERE user_id=? AND available_balance=? AND used_margin=? AND ...`) with up to 5 retries.

### 3.4 Square-off scheduler (chrono-tz `Asia::Kolkata`)

`squareoff.rs::Scheduler` is one tokio task:

```rust
struct Job { id: &'static str, next: DateTime<Kolkata>, kind: JobKind }
enum JobKind { SquareOff(ConfigGroup), BackupSweep, T1Settlement, DailySnapshot, DailyPnlReset, AutoReset }
```

- Build jobs from config on start and on `ConfigChanged` (watch channel): one `SquareOff` per group (`nse_bse`, `cds_bcd`, `mcx`, `ncdex`) at its HH:MM; `BackupSweep` every 60 s; `T1Settlement` 00:00; `DailySnapshot` 23:59 (skip Saturday/Sunday and the holiday list the desktop already uses for market hours, if any); `DailyPnlReset` at `SESSION_EXPIRY_TIME` (default 03:00); `AutoReset` on `reset_day/reset_time` when not `Never`.
- Loop: `sleep_until(min(next))` with `clock.now()`; run due jobs in `spawn_blocking`; a job later than 5 minutes past due is still run once (misfire grace), then `next = next_occurrence(now)`. Every job body is idempotent because the sweep itself checks `now.time() >= square_off_time` and claims rows.
- Exchange map: `NSE|BSE|NFO|BFO -> nse_bse`, `CDS|BCD -> cds_bcd`, `MCX -> mcx`, `NCDEX -> ncdex`; `time_for(exchange)` parses HH:MM and falls back to 15:15.
- `status()` returns `{running, timezone: "Asia/Kolkata", jobs: [{id, name, next_run}]}`.

### 3.5 DB schema (web column names, single-user desktop still carries `user_id`)

```sql
CREATE TABLE sandbox_orders (id INTEGER PRIMARY KEY AUTOINCREMENT, orderid TEXT NOT NULL UNIQUE, user_id TEXT NOT NULL,
  strategy TEXT, symbol TEXT NOT NULL, exchange TEXT NOT NULL, action TEXT NOT NULL CHECK(action IN ('BUY','SELL')),
  quantity INTEGER NOT NULL, price NUMERIC, trigger_price NUMERIC,
  price_type TEXT NOT NULL CHECK(price_type IN ('MARKET','LIMIT','SL','SL-M')),
  product TEXT NOT NULL CHECK(product IN ('CNC','NRML','MIS')),
  order_status TEXT NOT NULL DEFAULT 'open' CHECK(order_status IN ('open','trigger pending','complete','cancelled','rejected')),
  average_price NUMERIC, filled_quantity INTEGER NOT NULL DEFAULT 0, pending_quantity INTEGER NOT NULL,
  rejection_reason TEXT, margin_blocked NUMERIC NOT NULL DEFAULT 0, gtt_leg_id INTEGER,
  order_timestamp TEXT NOT NULL, update_timestamp TEXT NOT NULL);
CREATE UNIQUE INDEX idx_sandbox_orders_gtt_leg ON sandbox_orders(gtt_leg_id) WHERE gtt_leg_id IS NOT NULL;
CREATE INDEX idx_sandbox_user_status ON sandbox_orders(user_id, order_status);
CREATE INDEX idx_sandbox_symbol_exchange ON sandbox_orders(symbol, exchange);

CREATE TABLE sandbox_trades (id INTEGER PRIMARY KEY AUTOINCREMENT, tradeid TEXT NOT NULL UNIQUE, orderid TEXT NOT NULL,
  user_id TEXT NOT NULL, symbol TEXT NOT NULL, exchange TEXT NOT NULL, action TEXT NOT NULL, quantity INTEGER NOT NULL,
  price NUMERIC NOT NULL, product TEXT NOT NULL, strategy TEXT, trade_timestamp TEXT NOT NULL);

CREATE TABLE sandbox_positions (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, symbol TEXT NOT NULL,
  exchange TEXT NOT NULL, product TEXT NOT NULL, quantity INTEGER NOT NULL, average_price NUMERIC NOT NULL, ltp NUMERIC,
  pnl NUMERIC DEFAULT 0, pnl_percent NUMERIC DEFAULT 0, accumulated_realized_pnl NUMERIC DEFAULT 0,
  today_realized_pnl NUMERIC DEFAULT 0, margin_blocked NUMERIC DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  UNIQUE(user_id, symbol, exchange, product));

CREATE TABLE sandbox_holdings (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, symbol TEXT NOT NULL,
  exchange TEXT NOT NULL, quantity INTEGER NOT NULL, average_price NUMERIC NOT NULL, ltp NUMERIC, pnl NUMERIC DEFAULT 0,
  pnl_percent NUMERIC DEFAULT 0, settlement_date TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  UNIQUE(user_id, symbol, exchange));

CREATE TABLE sandbox_funds (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL UNIQUE,
  total_capital NUMERIC DEFAULT 10000000, available_balance NUMERIC DEFAULT 10000000, used_margin NUMERIC DEFAULT 0,
  realized_pnl NUMERIC DEFAULT 0, today_realized_pnl NUMERIC DEFAULT 0, unrealized_pnl NUMERIC DEFAULT 0,
  total_pnl NUMERIC DEFAULT 0, last_reset_date TEXT NOT NULL, reset_count INTEGER DEFAULT 0,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);

CREATE TABLE sandbox_daily_pnl (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id TEXT NOT NULL, date TEXT NOT NULL,
  realized_pnl NUMERIC DEFAULT 0, positions_unrealized_pnl NUMERIC DEFAULT 0, holdings_unrealized_pnl NUMERIC DEFAULT 0,
  total_mtm NUMERIC DEFAULT 0, available_balance NUMERIC DEFAULT 0, used_margin NUMERIC DEFAULT 0,
  portfolio_value NUMERIC DEFAULT 0, created_at TEXT NOT NULL, UNIQUE(user_id, date));

CREATE TABLE sandbox_config (id INTEGER PRIMARY KEY AUTOINCREMENT, config_key TEXT NOT NULL UNIQUE,
  config_value TEXT NOT NULL, description TEXT, updated_at TEXT NOT NULL);   -- seeded with the 22 rows of 1.11

CREATE TABLE sandbox_gtt (...) and sandbox_gtt_legs (...) exactly as database/sandbox_db.py:299-460.
```

Money: store as TEXT/NUMERIC and compute with `rust_decimal::Decimal` (add the crate); never f64 for balances. Timestamps: store every sandbox timestamp as naive IST `YYYY-MM-DD HH:MM:SS` text (the web's `order_timestamp`/`trade_timestamp` convention) and compare session boundaries in IST. This deliberately removes the web's UTC-column quirk; `session.rs` therefore does not need `as_db_utc`. Migration: the existing stub tables (`migrations.rs:293-378, 516-539`) are renamed `_legacy_*` and dropped after one release; nothing depends on their data.

### 3.6 GTT

Port `gtt_manager.py` function for function: `place_gtt`, `_build_legs`, `_pricing_basis`, `modify_gtt`, `cancel_gtt`, `list_gtts`, `leg_is_triggered_by`, `try_claim_trigger` (one UPDATE with `NOT EXISTS` sibling and `EXISTS` active-parent subqueries), `fire_leg` (try-lock, parent claim, staged release, `orders::place_order` with `gtt_leg_id`, sibling cancel), `_compensate_failed_fire`, `reclaim_stranded_legs`, `reclaim_stranded_parents`, `expire_due_gtts`. Routes `/api/v1/placegttorder|modifygttorder|cancelgttorder|gttorderbook` and Tauri commands.

### 3.7 Events on the internal bus

`events.rs`:

```rust
pub enum SandboxEvent {
  OrderUpdate { user_id, orderid, symbol, exchange, action, quantity, price, trigger_price, pricetype, product,
                order_status, filled_quantity, pending_quantity, average_price, rejection_reason },   // "order.update"
  OrderFilled { orderid, tradeid, symbol, exchange, action, quantity, price, product, strategy },      // "sandbox.order_filled"
  AutoSquareOff { cancelled_orders, closed_positions },                                               // "sandbox.auto_squareoff"
  T1Settlement { settled_users, settled_positions },                                                  // "sandbox.t1_settlement"
  GttTriggered { trigger_id, symbol, exchange, triggered_order_id }, GttExpired { trigger_id, symbol, exchange },
  Mtm { symbol, exchange, ltp },                                                                      // throttled, desktop-only
  FundsChanged,
}
```

A `tokio::sync::broadcast::Sender<SandboxEvent>` in `AppState`. Subscribers: (a) Tauri emitter: `OrderUpdate` -> `app.emit("order_update", payload)` with the exact `socketio_subscriber.py:247-266` field set and `mode: "analyze"`, `broker: "sandbox"`; every other variant and every analyze-mode API result -> `app.emit("analyzer_update", {"request": {...api_type, symbol, ...}, "response": {...}})` so `useOrderEventRefresh` and a ported `useSocket` handler work unchanged; (b) analyzer log writer for API-driven events; (c) the engine itself (`PositionChanged` -> subscribe/unsubscribe). The webhook REST layer publishes `OrderPlaced/Modified/Cancelled/PositionClosed/AnalyzerError` on success/failure in analyze mode exactly where the web service layer does.

### 3.8 Tauri commands and REST additions

Commands: `get_sandbox_configs` (grouped web shape), `update_sandbox_config{config_key, config_value}` (validate + side effects), `reset_sandbox`, `get_squareoff_status`, `reload_squareoff`, `get_sandbox_pnl_data` (web `mypnl/api/data` shape), `export_sandbox_csv(kind)`, `get_analyzer_data(start, end)`, `export_analyzer_csv`, `clear_analyzer_logs`, `get_pnl_symbols`. REST: `/api/v1/pnl/symbols`, GTT routes; `/api/v1/analyzer/toggle` goes through `SandboxRuntime::apply_mode`. Remove `place_sandbox_order`, `update_sandbox_ltp`, `cancel_sandbox_order` commands (the UI must use `place_order` etc. so routing is identical to the web).

### 3.9 Test plan

Infrastructure: `cfg(test)` in-memory `rusqlite::Connection`, `FakeClock(Arc<Mutex<DateTime<Utc>>>)` with `advance()` / `set_ist("2026-10-03 15:16:00")`, `FixtureQuoteSource(HashMap<SymbolKey, Quote>)` with per-call scripting, a `tokio::sync::broadcast` tick injector for the engine task, and a symbol master fixture with ZEEL/RELIANCE (lotsize 1, contract_value 1) plus one NFO future, one option, one MCX contract (lot sizes), as `test/sandbox/conftest.py:29-56` seeds.

Fixture-driven fills (`tests/sandbox/fills/*.json`): `{orders:[...], ticks:[{symbol, exchange, ltp, bid, ask, high, low, at}], expect:{orders:[{orderid, status, average_price}], trades: n, positions:[...], funds:{available_balance, used_margin, realized_pnl, today_realized_pnl}}}`. Vectors to encode from the web (each line is a web test to port as a vector or a direct unit test):

- `test/sandbox/test_margin_scenarios.py::test_scenario_1..4` (BUY 100 -> SELL 50 -> SELL 50; cycle; BUY 100 -> SELL 200 reversal; BUY 100 -> BUY 100) at LTP 112.37: used margin 11237 -> 5618.5 -> 0; reversal leaves margin for 100 at the fill price; add gives weighted average.
- `test/sandbox/test_fund_manager.py::test_fund_initialization, test_margin_operations, test_insufficient_funds, test_leverage_calculations (MIS 5x, CNC 1x), test_unrealized_pnl`.
- `test/sandbox/test_cnc_sell_validation.py` (6 cases: no position -> rejected row with reason; with position; exceeding; holdings only; MIS short allowed; position + holdings) and `test_rejected_order.py`, `test_orderbook_api.py` (rejected rows appear in the orderbook with `rejection_reason`).
- `test/sandbox/test_holdings_sell.py::test_partial_holdings_sell, test_full_holdings_sell, test_position_then_holdings_sell` (T+1 credit of proceeds, holding reduced, deleted at 0).
- `test/sandbox/test_stale_quote_guard.py` (8 cases) -> unit tests of `quote_looks_stale` and that both fill paths consult it.
- `test/sandbox/test_position_session_boundary.py` and `test_catch_up_session_boundary.py` (boundary before/after 03:00; malformed `SESSION_EXPIRY_TIME` falls back to 03:00; early-today CNC position is not swept; reopened MIS row survives; overnight MIS settled with P&L to all-time only; crypto / unconfigured exchanges skipped).
- `test/sandbox/test_execution_backlog.py` (cycle cost flat in queue depth: 150 pending orders processed in one poll without sleeps).
- `test/sandbox/test_concurrent_orders.py::test_concurrent_same_symbol_buys_accumulate_position` (N parallel BUYs -> one position with the summed quantity).
- `test/sandbox/test_gtt_manager.py` classes `TestTriggerEvaluation`, `TestDocumentedScenarios`, `TestLegConstruction`, `TestIdFormat`, `TestGTTMargin` (max vs sum, cancel returns exactly, expiry releases, double cancel), `TestClaimConcurrency`, `TestStrandedLegReclaim`, `TestMarginReconciliation` (active GTT margin not flagged), `TestOCOExclusivity`, `TestCancelledGttCannotFire`, `TestMarketOrderMargin`, `TestOrderbookContract`, `TestSuppliedExpiry`.
- Race tests from `test/test_gthread_sandbox_*.py` (run with `tokio::test(flavor="multi_thread")` and a `Barrier`): `fills::test_one_order_fills_once_under_two_threads`, `test_a_stop_loss_release_cannot_reopen_a_cancelled_order`, `test_a_modify_after_the_fill_was_decided_is_not_filled_at_the_old_terms`; `orders::test_two_cancels_of_one_order_release_its_margin_once`, `test_a_cancel_racing_a_fill_leaves_one_outcome`, `test_a_modify_cannot_rewrite_an_order_that_filled_meanwhile`; `positions::test_two_closers_do_not_reverse_the_position`, `test_two_cnc_sells_of_the_same_shares_cannot_both_pass`, `test_two_exit_smart_orders_do_not_reverse_the_position`, `test_orders_on_different_positions_do_not_wait_for_each_other`; `funds::test_two_blocks_at_once_both_land_or_one_is_refused`, `test_reconcile_does_not_erase_a_concurrent_credit`, `test_rebasing_the_capital_keeps_a_block_committed_while_it_runs`; `settlement::test_an_expired_position_settles_once`, `test_t1_settlement_runs_once`, `test_a_stale_mis_position_is_settled_once`, `test_a_second_catch_up_trigger_skips_while_one_runs`; `squareoff::test_the_primary_and_backup_sweeps_close_a_position_once`, `test_a_position_reversed_during_the_sweep_is_closed_as_it_now_stands`; `settings::test_a_reset_restarts_only_what_was_running_and_only_in_analyze_mode`; `review_sandbox::test_a_leg_never_waits_for_another_order_on_its_position`.

Deterministic-time tests (FakeClock): MIS order at 15:16 IST on NSE rejected unless reducing; allowed at 09:00; NFO follows the `nse_bse` time; CDS at 16:45, MCX at 23:30, NCDEX at 17:00; scheduler fires each job exactly once when the clock crosses it and once more after a 5-minute misfire; T+1 at 00:00 moves yesterday's CNC and ignores today's; snapshot skipped on Saturday; `today_realized_pnl` zeroed at 03:00; weekly reset on the configured day; expiry-day settlement at 15:40 for NFO under `expiry_day_close` and not under `next_day`; option settles at LTP vs zero per config.

Property tests (`proptest`) on `positions::apply_fill` over random sequences of signed fills with random prices and order margins: (1) `position.quantity == sum(signed fills)`; (2) `used_margin == sum(position.margin_blocked where qty != 0)` after every fill (margin conservation); (3) `realized_pnl + unrealized_pnl(ltp) == sum over fills of (ltp - fill_price) * signed_qty` (P&L conservation at any marking price); (4) average price after a same-direction add equals the quantity-weighted mean, and after a reversal equals the last fill price; (5) `available_balance + used_margin - realized_pnl == total_capital` while no T+1 transfer or sale credit has happened (web fund identity); (6) netting is order-independent for commuting fills (two adds in either order give the same average and margin).

Engine tests: inject ticks into the broadcast channel and assert fills for each price type/direction matrix (MARKET/LIMIT/SL/SL-M x BUY/SELL x trigger met/not met x limit met/not met), `trigger pending -> open` transition for SL when the limit is not met, index membership after fill/cancel/modify, subscribe/unsubscribe refcounts, fallback polling only while the feed is stale, GTT leg fired exactly once with two concurrent ticks, and that an `OrderUpdate` event is emitted on every transition with the web payload keys.
