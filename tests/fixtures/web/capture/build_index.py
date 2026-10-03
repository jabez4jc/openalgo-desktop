#!/usr/bin/env python3
"""Build INDEX.md from the captured fixtures."""
import glob
import json
import os

ROOT = os.path.dirname(os.path.abspath(__file__))

NOTES = {
    ("ping", "apikey_in_body"): "envelope {status, data:{broker, message:'pong'}}",
    ("ping", "apikey_in_x_api_key_header"): "X-API-KEY header NOT honoured by /api/v1 endpoints -> 400 'Missing data for required field' (only telegram/whatsapp read the header)",
    ("ping", "apikey_in_header_and_body"): "header ignored, body key used",
    ("ping", "extra_unknown_field"): "schemas are strict: unknown field -> 400 {foo:['Unknown field.']}",
    ("funds", "apikey_in_body"): "instance was in ANALYZE mode: sandbox funds (availablecash 1e7), extra 'mode':'analyze', fields are numbers (not strings as in live brokers)",
    ("funds", "apikey_in_x_api_key_header"): "400, header not supported",
    ("orderbook", "default"): "data:{orders:[], statistics:{...}} + top-level mode:'analyze'",
    ("tradebook", "default"): "data:[] + mode",
    ("positionbook", "default"): "data:[] plus top-level total_pnl/total_pnl_today/total_today_realized_pnl/total_unrealized_pnl (analyze mode only)",
    ("holdings", "default"): "data:{holdings:[], statistics:{totalholdingvalue,...}} + mode",
    ("gttorderbook", "default"): "data:[] + mode:'analyze'",
    ("openposition", "from_positionbook_or_default"): "no position -> {status:'success', quantity:0, mode}; quantity is an int",
    ("orderstatus", "unknown_orderid"): "404 {status:'error', message:'Order <id> not found', mode}",
    ("expiry", "nifty_nfo_futures"): "data is a flat list of 'DD-MMM-YY' strings (27-OCT-26); NOT the DDMMMYY symbol format",
    ("expiry", "nifty_nfo_options"): "weekly + monthly expiries, DD-MMM-YY",
    ("optionsymbol", "nifty_atm_ce"): "flat response (no data wrapper): {status, symbol, exchange, lotsize, tick_size, freeze_qty, underlying_ltp}; expiry_date must be DDMMMYY",
    ("optionsymbol", "expiry_with_dashes_as_returned_by_expiry_api"): "raw /expiry value (06-OCT-26) rejected with 404 'No strikes found'",
    ("optionsymbol", "underlying_is_future_symbol"): "underlying=NIFTY27OCT26FUT on NFO works, expiry inferred from future",
    ("optionsymbol", "invalid_offset"): "400 marshmallow message",
    ("quotes", "reliance_nse"): "data:{ask,ask_qty,bid,bid_qty,high,low,ltp,oi,open,prev_close,volume}; market closed -> bid/ask/volume 0; ints and floats mixed (prev_close 1187 vs ltp 1167.7)",
    ("quotes", "reliance_nse_x_api_key_header"): "400, header not supported",
    ("quotes", "symbol_with_dashed_expiry"): "400 'Symbol ... not found for exchange' (same shape as unknown symbol)",
    ("multiquotes", "mixed_seven_symbols"): "NOTE key is `results` (not `data`): [{symbol, exchange, data:{quote fields}}]; order of results may differ from request order",
    ("multiquotes", "with_one_unknown_symbol"): "200 + status:success overall; failed item is {symbol, exchange, error:<string>} with no data key; unknown symbol listed first (order not preserved)",
    ("multiquotes", "empty_symbols_list"): "400 'Shorter than minimum length 1.'",
    ("depth", "reliance_nse"): "data:{asks:[{price,quantity}x5], bids:[...], high, low, ltp, ltq, oi, open, prev_close, totalbuyqty, totalsellqty, volume}; 5 levels of zeros when closed",
    ("history", "reliance_nse_1m"): "data:[{close,high,low,oi,open,timestamp,volume}] timestamp = epoch SECONDS (int); truncated in file, _total_rows recorded",
    ("history", "reliance_nse_D"): "daily candles, timestamp epoch seconds",
    ("history", "reliance_nse_D_source_db"): "source='db' -> 404 (no local DB copy)",
    ("history", "bad_date_format_ddmmyyyy"): "dates must be YYYY-MM-DD -> 'Not a valid date.'",
    ("history", "bad_interval"): "schema allows 1s..Y but broker /intervals only lists a subset",
    ("history", "end_before_start"): "200 with data:[] (no validation)",
    ("intervals", "default"): "data:{seconds:[],minutes:[...],hours:['1h'],days:['D'],weeks:[],months:[]}",
    ("symbol", "reliance_nse"): "data:{brexchange,brsymbol,exchange,expiry,freeze_qty,id,instrumenttype,lotsize,name,strike,symbol,tick_size,token}; token is 'a::::b' composite string",
    ("symbol", "nifty_option_nfo"): "brsymbol uses Zerodha weekly code NIFTY26O0622400CE; expiry 'DD-MMM-YY'",
    ("symbol", "unknown_symbol"): "404",
    ("search", "reliance_nse"): "data: list of symbol rows (same row shape as /symbol minus id); truncated to 25",
    ("search", "no_results"): "200 {status:success, data:[], message:'No matching symbols found'}",
    ("optionchain", "nifty_nearest_expiry_5_strikes"): "flat response with chain:[{strike, ce:{...}, pe:{...}}], expiry_date echoed as DDMMMYY, expiry_ts/server_ts epoch seconds, labels ATM/ITMn/OTMn",
    ("optionchain", "nifty_nearest_expiry_3_strikes_with_greeks"): "adds implied_volatility/delta/gamma/theta/vega per leg, forward_price",
    ("optionchain", "bad_expiry"): "404",
    ("optiongreeks", "nifty_option_default"): "flat response; expiry_date here is 'DD-Mon-YYYY' (06-Oct-2026) -- third date format; greeks nested",
    ("optiongreeks", "unknown_symbol"): "400 (first-pass run with wrong symbol format)",
    ("multioptiongreeks", "nifty_ce_and_pe"): "data:[per-symbol results], summary:{total,success,failed}; status 'success'|'partial'|'error' with HTTP 200 even when all fail",
    ("multioptiongreeks", "one_valid_one_unknown"): "status:'partial', HTTP 200",
    ("syntheticfuture", "nifty_nearest_expiry"): "flat: {atm_strike, expiry (DDMMMYY), synthetic_future_price, underlying, underlying_ltp}",
    ("instruments", "nse_json"): "GET only; apikey in query string; data truncated to 20 rows, _total_rows kept",
    ("instruments", "nse_csv"): "text/csv attachment; header row symbol,brsymbol,name,exchange,brexchange,token,expiry,strike,lotsize,instrumenttype,tick_size",
    ("instruments", "all_exchanges_json"): "exchange is effectively required: 400 'Field may not be null.'",
    ("instruments", "post_not_allowed"): "POST -> 404 (not 405)",
    ("margin", "single_equity_mis"): "quantity/price are STRINGS in request; data:{exposure_margin, span_margin, total_margin_required} numbers",
    ("margin", "quantity_as_number_not_string"): "numeric quantity rejected: 400 'Not a valid string.'",
    ("margin", "empty_positions"): "400",
    ("analyzer", "status"): "data:{analyze_mode:true, mode:'analyze', total_logs}; instance was in analyze mode at capture time. /analyzer/toggle NOT called",
    ("market/holidays", "year_2026"): "data:[{date 'YYYY-MM-DD', description, holiday_type, closed_exchanges[], open_exchanges:[{exchange,start_time,end_time ms}]}]",
    ("market/timings", "saturday_2026-10-03"): "only CRYPTO open on Saturday; start_time/end_time epoch MILLISECONDS",
    ("market/timings", "weekday_2026-10-01"): "per-exchange ms timestamps",
    ("ticker", "nse_reliance_D_json"): "GET /ticker/EXCH:SYMBOL?apikey&interval&from&to; same JSON shape as /history",
    ("ticker", "nse_reliance_D_txt"): "format=txt: text/plain CSV lines 'NSE:RELIANCE,YYYY-MM-DD,o,h,l,c,v' (no header)",
    ("ticker", "nse_reliance_5m_txt"): "intraday txt adds HH:MM:SS column: 'NSE:RELIANCE,YYYY-MM-DD,HH:MM:SS,o,h,l,c,v'",
    ("ticker", "no_exchange_prefix_defaults"): "BUG-LIKE: path without 'EXCH:' prefix silently falls back to NSE:RELIANCE (ticker.py)",
    ("ticker", "missing_from_to"): "400 'Field may not be null.' for start_date/end_date (internal names leak)",
    ("ticker", "invalid_apikey_txt"): "BUG: 500 Internal Server Error (TextResponse tuple return breaks flask-restx)",
    ("ticker", "invalid_apikey_json"): "403 standard error",
    ("pnl", "symbols_in_live_mode"): "200 because instance is in analyze mode; in live mode this returns 400 'only available in sandbox/analyzer mode'",
    ("chart", "get_preferences"): "GET with apikey query; data:{} when nothing saved",
    ("chart", "get_missing_apikey"): "400 'Missing apikey parameter'",
    ("strategy", "list"): "POST /strategy/list; data:[]",
    ("strategy", "status_unknown_id"): "strategy_id must be an INTEGER ('Not a valid integer.')",
    ("portfolio", "benchmarks"): "GET; data:[{exchange,name,symbol}] list of index benchmarks",
    ("portfolio", "benchmarks_invalid_apikey"): "403",
    ("errors", "invalid_apikey_funds"): "403 {status:'error', message:'Invalid openalgo apikey'}",
    ("errors", "missing_apikey_funds"): "400 message is an OBJECT {field:[msgs]} (marshmallow) not a string",
    ("errors", "empty_apikey_string"): "400 'Length must be between 1 and 256.'",
    ("errors", "unknown_symbol_quotes"): "400 string message 'Symbol ... not found for exchange ...'",
    ("errors", "bad_exchange_quotes"): "400 lists all valid exchanges: NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO",
    ("errors", "lowercase_exchange_quotes"): "exchange is case-sensitive",
    ("errors", "malformed_json_quotes"): "400 Werkzeug body {message:'The browser (or proxy) sent a request...'} with NO status field",
    ("errors", "non_json_content_type"): "500 {status:'error', message:'An unexpected error occurred'} (request.json None)",
    ("errors", "empty_body_json_content_type"): "400 Werkzeug generic message, no status field",
    ("errors", "json_array_body"): "500 bare {message:'Internal Server Error'} (flask-restx) -- no status field",
    ("errors", "wrong_method_get_on_funds"): "GET on POST endpoint -> 404 (not 405) {status:'error', message:'Not found', path}",
    ("errors", "wrong_method_put_on_ping"): "404 as above",
    ("errors", "unknown_route"): "404 {status:'error', message:'Not found', path:'/api/v1/doesnotexist'}",
    ("errors", "unknown_route_outside_api"): "404 with text/html SPA index shell (not JSON) -- only /api/v1/* 404s are JSON",
    ("errors", "trailing_slash_ping"): "/ping/ accepted (strict_slashes=False)",
    ("errors", "rate_limit_probe_no_429"): "103 pings in 2.6s -> all 200; limit is 100/SECOND (not per minute) so single-client burst could not exceed it; no X-RateLimit headers emitted",
}

NOTES2 = {
    ("errors", "rate_limit_429_placeorder"): "ORDER_RATE_LIMIT 10/s: 429 {message:'10 per 1 second'} -- no status field, no Retry-After / X-RateLimit headers",
    ("placeorder", "market_buy_mis_reliance"): "{status:'success', orderid:'<14-digit string>', mode:'analyze'}; MARKET fills immediately at LTP (bid/ask 0 off-hours)",
    ("placeorder", "limit_buy_cnc_sbin_far_below"): "stays order_status 'open'",
    ("placeorder", "sl_buy_mis_reliance"): "status 'trigger pending' (with a SPACE)",
    ("placeorder", "lowercase_action_buy"): "lowercase action accepted",
    ("placeorder", "error_unknown_symbol"): "400 STRING message 'Symbol X not found on NSE' + mode",
    ("placeorder", "error_quantity_zero"): "400; schema errors on order endpoints are a STRINGIFIED python dict: \"{'quantity': ['Quantity must be a positive number.']}\" (not an object as on read endpoints)",
    ("placeorder", "error_missing_symbol"): "stringified-dict message; NO mode key on placeorder schema errors",
    ("placeorder", "error_limit_price_zero"): "business rule (string): 'LIMIT orders require price'",
    ("placeorder", "error_sl_without_trigger"): "'SL orders require trigger_price'",
    ("placeorder", "error_option_qty_not_lot_multiple"): "'Quantity must be in multiples of lot size 65'",
    ("placeorder", "error_invalid_apikey"): "403 'Invalid openalgo apikey'",
    ("orderstatus", "market_buy_mis_reliance"): "data uses key price_type (orderbook uses pricetype); no rejection_reason; timestamp 'YYYY-MM-DD HH:MM:SS' IST",
    ("orderstatus", "after_cancel"): "order_status 'cancelled'; pending_quantity NOT zeroed",
    ("openposition", "sbin_mis_short"): "negative int quantity for short",
    ("modifyorder", "open_limit_price_and_qty"): "{status, message:'Order modified successfully', orderid, mode}",
    ("modifyorder", "error_unknown_orderid"): "404 'Order <id> not found'",
    ("modifyorder", "error_completed_order"): "400 'Cannot modify order in complete status'",
    ("modifyorder", "error_missing_price"): "stringified dict, WITH mode key (unlike placeorder)",
    ("cancelorder", "open_limit"): "{status, message:'Order cancelled successfully', orderid, mode}",
    ("cancelorder", "error_already_cancelled"): "400 'Cannot cancel order in cancelled status'",
    ("cancelorder", "error_unknown_orderid"): "404",
    ("cancelorder", "trigger_pending_sl"): "trigger-pending orders must be cancelled individually (see cancelallorder bug)",
    ("cancelallorder", "with_open_orders"): "{canceled_orders:[ids], failed_cancellations:[], message:'Canceled N orders. Failed to cancel M orders.', mode}. BUG: skips 'trigger pending' orders (filter checks 'trigger_pending')",
    ("cancelallorder", "nothing_open"): "200 success, message 'No open orders to cancel', empty lists",
    ("closeposition", "with_open_positions"): "{closed_positions:<int count>, failed_closures:<int>, message:'Closed N positions', mode}",
    ("closeposition", "no_open_positions"): "200 SUCCESS (not an error) 'No open positions to close'",
    ("placesmartorder", "open_from_flat_to_10"): "same shape as placeorder",
    ("placesmartorder", "no_action_already_at_5"): "quantity 0 + position matches -> 200 'No OpenPosition Found. Not placing Exit order.' (misleading text)",
    ("placesmartorder", "no_action_qty_nonzero_position_matches"): "200 'Positions Already Matched. No Action needed.' (no orderid)",
    ("basketorder", "three_legs_mixed"): "results:[{batch_order:true, exchange, is_last_order, orderid, product, status, symbol}]",
    ("basketorder", "one_leg_unknown_symbol"): "overall status 'success' HTTP 200; failed leg {message, status:'error', symbol}",
    ("basketorder", "error_leg_bad_product"): "nested stringified dict \"{'orders': {0: {'product': [...]}}}\"",
    ("splitorder", "sbin_10_split_3"): "results:[{order_num, orderid, quantity, status}] remainder as last chunk; split_size, total_quantity",
    ("optionsorder", "atm_ce_buy"): "flat: {exchange, offset, option_type, orderid, symbol, underlying, underlying_ltp, mode}",
    ("optionsorder", "atm_ce_buy_with_splitsize"): "with splitsize: results[] + split_size + total_quantity instead of orderid",
    ("optionsorder", "error_bad_offset"): "400 string, NO mode key",
    ("optionsorder", "error_bad_expiry"): "404 'No strikes found for NIFTY expiring 01JAN20...'",
    ("optionsmultiorder", "bull_call_spread_2_legs"): "results:[{action, exchange, leg, mode, offset, option_type, orderid, product, status, strike:null, symbol}], underlying, underlying_ltp",
    ("optionsmultiorder", "error_empty_legs"): "DIFFERENT error shape: {status:'error', message:'Validation error', errors:{legs:[...]}}",
    ("optionsmultiorder", "one_leg_bad_offset"): "200 success overall; failed leg has message and no orderid",
    ("placegttorder", "single_buy_cnc_trigger_below"): "{status, trigger_id:'GTT-YYMMDD-<8 hex>', mode}",
    ("placegttorder", "error_mis_product"): "GTT only CNC/NRML",
    ("placegttorder", "error_unknown_symbol"): "'Symbol not found' (differs from placeorder text)",
    ("gttorderbook", "after_place"): "rows {created_at ISO local no tz, expires_at (+1y), last_price, legs[{action,price,pricetype,product,quantity,triggered_order_id}], margin_blocked, status:'active', strategy, symbol, trigger_id, trigger_prices[], trigger_type:'single'|'two-leg' (request OCO -> 'two-leg'), updated_at}",
    ("gttorderbook", "status_cancelled"): "status filter only accepts active|all; message is an OBJECT here (read-endpoint style)",
    ("gttorderbook", "status_all"): "includes cancelled rows (status 'cancelled', margin_blocked 0)",
    ("modifygttorder", "single_change_trigger_and_qty"): "{status, trigger_id, mode}",
    ("modifygttorder", "error_unknown_trigger_id"): "BUG 500 'An unexpected error occurred': GTTModifyFailedEvent has no 'exchange' field -> TypeError on any failed sandbox modify",
    ("modifygttorder", "error_cancelled_trigger"): "BUG 500, same cause",
    ("cancelgttorder", "single"): "{status, trigger_id, mode}",
    ("cancelgttorder", "error_already_cancelled"): "404 \"No active GTT with trigger_id '...'\"",
    ("cancelgttorder", "oco"): "500 margin-release failure after OCO modify + closeposition: 'Could not release 1106.80 margin ... The GTT is unchanged - retry the cancel.' (retry also fails; GTT left active)",
    ("analyzer", "toggle_to_live_false"): "data:{analyze_mode:false, message:'Analyzer mode switched to live', mode:'live', total_logs}",
    ("analyzer", "toggle_back_to_analyze_true"): "message 'Analyzer mode switched to analyze'",
    ("analyzer", "toggle_error_invalid_mode"): "400 stringified dict, no mode key",
    ("orderbook", "populated"): "orders newest first; statistics {total_buy_orders,total_completed_orders,total_open_orders,total_rejected_orders,total_sell_orders,total_trigger_pending_orders}",
    ("tradebook", "populated"): "rows {action, average_price, exchange, orderid, price, product, quantity, strategy, symbol, timestamp, trade_value, tradeid:'TRADE-YYYYMMDD-HHMMSS-<8hex>'}",
    ("positionbook", "populated"): "rows {average_price, exchange, lot_size (float), ltp, pnl, pnlpercent, product, quantity, symbol, today_realized_pnl, total_pnl_today, unrealized_pnl}",
    ("positionbook", "after_closeposition"): "closed (qty 0) positions are NOT listed",
    ("holdings", "after_cnc_buys"): "same-day CNC buys do not appear in holdings (T+1 settlement in sandbox)",
    ("funds", "after_activity"): "adds last_reset, reset_count, today_realized_pnl, total_realized_pnl; utiliseddebits == grossexposure",
    ("pnl", "symbols_populated"): "per-symbol rows (no ltp/avg) + top-level totals",
}
NOTES.update(NOTES2)


def main():
    idx = json.load(open(os.path.join(ROOT, "rest_index.json")))
    idx.sort(key=lambda r: (r["endpoint"] != "errors", r["endpoint"], r["case"]))
    lines = []
    lines.append("# OpenAlgo golden fixtures (captured 2026-10-03, Saturday, market closed)\n")
    lines.append("Source instance: http://127.0.0.1:5000 (Flask/Werkzeug dev server), ws://127.0.0.1:8765, broker zerodha.")
    lines.append("All API keys replaced by `<APIKEY>`; `user_id`/client ids by `<USER_ID>`; email by `<EMAIL>`. Numeric market data unchanged.")
    lines.append("IMPORTANT: `/api/v1/analyzer` reported `analyze_mode: true` at capture time, so account endpoints (funds/orderbook/tradebook/positionbook/holdings/gttorderbook/openposition/orderstatus/pnl) are SANDBOX-shaped and carry `\"mode\": \"analyze\"`. Session 1 called no state-changing endpoint. Session 2 (04:11-04:16 UTC, user-authorised) placed/modified/cancelled orders ONLY in analyze mode and did one toggle round trip (analyze -> live -> analyze); final mode analyze. Rows marked S2 are from session 2; see ANALYZER_SESSION.md.\n")
    lines.append("## REST fixtures (`rest/<endpoint>/<case>.json`)\n")
    lines.append("Each file: `{request:{method,path,headers,body}, response:{status_code,headers,body}, note?}`. Large arrays are truncated with `_total_rows`/`_truncated` markers.\n")
    lines.append("| Endpoint | Case | HTTP | Note |")
    lines.append("|---|---|---|---|")
    for r in idx:
        note = NOTES.get((r["endpoint"], r["case"]), "") or r.get("note", "")
        if r.get("note") and NOTES.get((r["endpoint"], r["case"])) and r["note"] not in note:
            note = note + " (" + r["note"] + ")"
        tag = "S2 " if r.get("session") == 2 else ""
        lines.append(f"| {r['endpoint']} | {r['case']} | {r['status']} | {tag}{note.replace('|', '/')} |")

    lines.append("\n## WebSocket fixtures (`websocket/*.jsonl`)\n")
    lines.append("One JSON object per line: `{ts, iso, direction: send|recv|note, message|raw, ...}`. market_data lines capped at 30 per listen window; totals in the `note` lines.\n")
    lines.append("| File | Covers | Note |")
    lines.append("|---|---|---|")
    ws_rows = [
        ("01_connect_auth_subscribe_flow.jsonl", "connect, authenticate, ping, get_broker_info, get_supported_brokers, subscribe LTP(1)/Quote/Depth(3) for RELIANCE NSE + NIFTY NSE_INDEX (20s each), depth 20 via `depth` and legacy `depth_level`, depth 50, single-symbol form, invalid mode, no symbols, unknown symbol, bad exchange, unsubscribe, unsubscribe not-subscribed, mixed per-symbol modes, subscribe_orders/unsubscribe_orders, unsubscribe_all, invalid action, `type` alias, malformed JSON, JSON array, empty object",
         "Ticks DID arrive on Saturday (12 total): one snapshot per symbol shortly after each subscribe (stale last-traded values, volume 0). Depth 20/50 acks report depth 20/50 but no depth ticks arrived. Server answers higher-mode subscription by also emitting lower-mode copies (mode 1 message after mode 2 subscribe)."),
        ("02_subscribe_before_auth.jsonl", "subscribe / unsubscribe_all / subscribe_orders / get_broker_info before authenticate; ping before auth",
         "All gated actions -> {status:'error', code:'NOT_AUTHENTICATED'} (request_id echoed when provided). `ping` works WITHOUT auth."),
        ("03_invalid_apikey_auth.jsonl", "authenticate with bad key, with no key, `auth`+`apikey` aliases",
         "{status:'error', code:'AUTHENTICATION_ERROR', message:'Invalid API key' | 'API key is required'}; connection stays open (pong works afterwards)."),
        ("04_auth_grace_timeout.jsonl", "connect and send nothing",
         "Server closes after ~15s with close code 4401 reason 'auth timeout' (WS_AUTH_GRACE_SECONDS)."),
        ("05_auth_alias_forms_and_mode_labels.jsonl", "`type`:'auth' + `apikey` alias, re-auth on same socket, mode labels ltp/QUOTE/depth, mode '2' (string digit), mode 2.0 (float)",
         "Labels are case-insensitive and canonicalised to LTP/Quote/Depth in the ack. String digit '2' is REJECTED (INVALID_MODE); float rejected 'Mode must be int or str, got float'."),
    ]
    for f, c, n in ws_rows:
        lines.append(f"| {f} | {c} | {n} |")

    lines.append(open(os.path.join(ROOT, "conventions.md")).read())
    with open(os.path.join(ROOT, "INDEX.md"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print("INDEX.md written", len(idx), "rest rows")


if __name__ == "__main__":
    main()
