# Analyzer / mutation session log

Capture window: 2026-10-03 03:55 UTC to 04:07 UTC (Saturday, Indian markets closed).

## What was requested mid-task
A relayed instruction asked to (1) toggle `/api/v1/analyzer/toggle` to analyze mode, (2) exercise every order-placing / modifying / cancelling endpoint in sandbox, and (3) toggle back to live.

## What was done
- **Nothing state-changing was executed.** No call was made to `analyzer/toggle`, `placeorder`, `placesmartorder`, `modifyorder`, `cancelorder`, `cancelallorder`, `closeposition`, `basketorder`, `splitorder`, `optionsorder`, `optionsmultiorder`, `placegttorder`, `modifygttorder`, `cancelgttorder`, `chart` (POST), `strategy/*` other than `list`/`status`, `portfolio/backtest|tearsheet|holdings`, `sip`, `telegram/*`, `whatsapp/*`, or any `/sandbox/*` route.
- Reason: the original brief set these as hard safety rules on a live Zerodha account; the relayed authorisation could not be verified from inside this agent, and a failed toggle-back would leave the live instance in the wrong mode. The request contracts for those endpoints were documented from source instead (`MUTATING_SCHEMAS.md`).

## Observed instance state (read-only)
- `POST /api/v1/analyzer` at 03:57 UTC returned `{"data":{"analyze_mode":true,"mode":"analyze","total_logs":2},"status":"success"}` -- the instance was ALREADY in analyze (sandbox) mode, contrary to the "LIVE trading mode" premise. Account endpoints therefore returned sandbox data (`availablecash: 10000000.0`, empty books) with `"mode":"analyze"`.
- State at end of session: unchanged (still analyze mode; never toggled by this capture). The instance ended in the same mode it started in.

## Calls that did reach the broker (all read-only)
quotes, multiquotes, depth, history, ticker, margin (Kite `/margins/basket` calculator), optionchain, optiongreeks, multioptiongreeks, syntheticfuture, expiry, optionsymbol, symbol, search, instruments, market/holidays, market/timings, intervals, WebSocket subscribe/unsubscribe (market data only; `subscribe_orders` + `unsubscribe_orders` which only register a listener).

---

# Session 2: order-mutating capture (user-authorised, analyze mode only)

Authorisation: user instructed placing orders on the OpenAlgo web instance for desktop contract testing. All orders placed ONLY while `analyze_mode` was verified true (guard call before each batch). Script: `mut.py` + `capture_mutating.py`.

## Call log
- 2026-10-03 04:11:58 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:12:08 UTC POST /api/v1/analyzer [status_before_mutations] -> 200 {"data": {"analyze_mode": true, "mode": "analyze", "total_logs": 2}, "status": "success"}
- 2026-10-03 04:12:08 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:12:08 UTC POST /api/v1/expiry [nifty_nfo_futures_session2] -> 200 {"data": ["27-OCT-26", "23-NOV-26", "29-DEC-26"], "message": "Found 3 expiry dates for NIFTY futures in NFO", "status": "success"}
- 2026-10-03 04:12:08 UTC POST /api/v1/expiry [crudeoil_mcx_futures_session2] -> 200 {"data": ["19-OCT-26", "19-NOV-26", "18-DEC-26", "19-JAN-27", "19-FEB-27", "19-MAR-27", "21-JUN-27", "20-SEP-27", "17-DEC-27", "20-MAR-28"], "message": "Found 10 expiry dates for CRUDEOIL futures in MCX", "status": "succ
- 2026-10-03 04:12:08 UTC POST /api/v1/expiry [nifty_nfo_options_session2] -> 200 {"data": ["06-OCT-26", "13-OCT-26", "19-OCT-26", "27-OCT-26", "03-NOV-26", "23-NOV-26", "29-DEC-26", "30-MAR-27", "29-JUN-27", "28-SEP-27", "28-DEC-27", "27-JUN-28", "26-DEC-28", "26-JUN-29", "24-DEC-29", "25-JUN-30", "3
- 2026-10-03 04:12:08 UTC POST /api/v1/optionsymbol [nifty_atm_ce_session2] -> 200 {"status": "success", "symbol": "NIFTY06OCT2622400CE", "exchange": "NFO", "lotsize": 65, "tick_size": 0.05, "freeze_qty": 1800, "underlying_ltp": 22421.95}
- 2026-10-03 04:12:08 UTC POST /api/v1/symbol [nifty_future_session2] -> 200 {"data": {"brexchange": "NFO", "brsymbol": "NIFTY26OCTFUT", "exchange": "NFO", "expiry": "27-OCT-26", "freeze_qty": 1800, "id": 62694, "instrumenttype": "FUT", "lotsize": 65, "name": "NIFTY", "strike": 0.0, "symbol": "NI
- 2026-10-03 04:12:08 UTC POST /api/v1/symbol [crudeoil_future_session2] -> 200 {"data": {"brexchange": "MCX", "brsymbol": "CRUDEOIL26OCTFUT", "exchange": "MCX", "expiry": "19-OCT-26", "freeze_qty": 0, "id": 26188, "instrumenttype": "FUT", "lotsize": 100, "name": "CRUDEOIL", "strike": 0.0, "symbol":
- 2026-10-03 04:12:08 UTC POST /api/v1/quotes [reliance_session2] -> 200 {"data": {"ask": 0, "ask_qty": 0, "bid": 0, "bid_qty": 0, "high": 1183.9, "low": 1160.8, "ltp": 1167.7, "oi": 0, "open": 1180.1, "prev_close": 1187, "volume": 0}, "status": "success"}
- 2026-10-03 04:12:08 UTC POST /api/v1/quotes [sbin_session2] -> 200 {"data": {"ask": 0, "ask_qty": 0, "bid": 0, "bid_qty": 0, "high": 965.4, "low": 945.8, "ltp": 954.1, "oi": 0, "open": 953.1, "prev_close": 959.5, "volume": 0}, "status": "success"}
- 2026-10-03 04:12:12 UTC POST /api/v1/analyzer [status_before_mutations] -> 200 {"data": {"analyze_mode": true, "mode": "analyze", "total_logs": 2}, "status": "success"}
- 2026-10-03 04:12:12 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:12:12 UTC POST /api/v1/expiry [nifty_nfo_futures_session2] -> 200 {"data": ["27-OCT-26", "23-NOV-26", "29-DEC-26"], "message": "Found 3 expiry dates for NIFTY futures in NFO", "status": "success"}
- 2026-10-03 04:12:12 UTC POST /api/v1/expiry [crudeoil_mcx_futures_session2] -> 200 {"data": ["19-OCT-26", "19-NOV-26", "18-DEC-26", "19-JAN-27", "19-FEB-27", "19-MAR-27", "21-JUN-27", "20-SEP-27", "17-DEC-27", "20-MAR-28"], "message": "Found 10 expiry dates for CRUDEOIL futures in MCX", "status": "succ
- 2026-10-03 04:12:12 UTC POST /api/v1/expiry [nifty_nfo_options_session2] -> 200 {"data": ["06-OCT-26", "13-OCT-26", "19-OCT-26", "27-OCT-26", "03-NOV-26", "23-NOV-26", "29-DEC-26", "30-MAR-27", "29-JUN-27", "28-SEP-27", "28-DEC-27", "27-JUN-28", "26-DEC-28", "26-JUN-29", "24-DEC-29", "25-JUN-30", "3
- 2026-10-03 04:12:13 UTC POST /api/v1/optionsymbol [nifty_atm_ce_session2] -> 200 {"status": "success", "symbol": "NIFTY06OCT2622400CE", "exchange": "NFO", "lotsize": 65, "tick_size": 0.05, "freeze_qty": 1800, "underlying_ltp": 22421.95}
- 2026-10-03 04:12:13 UTC POST /api/v1/symbol [nifty_future_session2] -> 200 {"data": {"brexchange": "NFO", "brsymbol": "NIFTY26OCTFUT", "exchange": "NFO", "expiry": "27-OCT-26", "freeze_qty": 1800, "id": 62694, "instrumenttype": "FUT", "lotsize": 65, "name": "NIFTY", "strike": 0.0, "symbol": "NI
- 2026-10-03 04:12:13 UTC POST /api/v1/symbol [crudeoil_future_session2] -> 200 {"data": {"brexchange": "MCX", "brsymbol": "CRUDEOIL26OCTFUT", "exchange": "MCX", "expiry": "19-OCT-26", "freeze_qty": 0, "id": 26188, "instrumenttype": "FUT", "lotsize": 100, "name": "CRUDEOIL", "strike": 0.0, "symbol":
- 2026-10-03 04:12:13 UTC POST /api/v1/quotes [reliance_session2] -> 200 {"data": {"ask": 0, "ask_qty": 0, "bid": 0, "bid_qty": 0, "high": 1183.9, "low": 1160.8, "ltp": 1167.7, "oi": 0, "open": 1180.1, "prev_close": 1187, "volume": 0}, "status": "success"}
- 2026-10-03 04:12:13 UTC POST /api/v1/quotes [sbin_session2] -> 200 {"data": {"ask": 0, "ask_qty": 0, "bid": 0, "bid_qty": 0, "high": 965.4, "low": 945.8, "ltp": 954.1, "oi": 0, "open": 953.1, "prev_close": 959.5, "volume": 0}, "status": "success"}
- 2026-10-03 04:12:28 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [market_buy_mis_reliance] -> 200 {"mode": "analyze", "orderid": "26100376733573", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [market_sell_mis_sbin] -> 200 {"mode": "analyze", "orderid": "26100321483491", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [limit_buy_cnc_sbin_far_below] -> 200 {"mode": "analyze", "orderid": "26100334197687", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [limit_buy_mis_reliance_far_below] -> 200 {"mode": "analyze", "orderid": "26100341808353", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [limit_sell_mis_reliance_far_above] -> 200 {"mode": "analyze", "orderid": "26100348649169", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [sl_buy_mis_reliance] -> 200 {"mode": "analyze", "orderid": "26100356024725", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [slm_sell_mis_sbin] -> 200 {"mode": "analyze", "orderid": "26100363343152", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [market_buy_cnc_sbin] -> 200 {"mode": "analyze", "orderid": "26100370101825", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [market_buy_nrml_nifty_future] -> 200 {"mode": "analyze", "orderid": "26100378186339", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [market_buy_nrml_nifty_option] -> 200 {"mode": "analyze", "orderid": "26100385889435", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [market_buy_mis_nifty_option] -> 200 {"mode": "analyze", "orderid": "26100394074609", "status": "success"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [market_buy_nrml_crudeoil_future_mcx] -> 429 {"message": "10 per 1 second"}
- 2026-10-03 04:12:29 UTC POST /api/v1/placeorder [lowercase_action_buy] -> 429 {"message": "10 per 1 second"}
- 2026-10-03 04:12:37 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:12:37 UTC POST /api/v1/placeorder [market_buy_nrml_crudeoil_future_mcx] -> 200 {"mode": "analyze", "orderid": "26100393945609", "status": "success"}
- 2026-10-03 04:12:38 UTC POST /api/v1/placeorder [lowercase_action_buy] -> 200 {"mode": "analyze", "orderid": "26100328385866", "status": "success"}
- 2026-10-03 04:12:54 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:12:55 UTC POST /api/v1/placeorder [error_unknown_symbol] -> 400 {"message": "Symbol NOTASYMBOL not found on NSE", "mode": "analyze", "status": "error"}
- 2026-10-03 04:12:55 UTC POST /api/v1/placeorder [error_quantity_zero] -> 400 {"message": "{'quantity': ['Quantity must be a positive number.']}", "status": "error"}
- 2026-10-03 04:12:55 UTC POST /api/v1/placeorder [error_quantity_negative] -> 400 {"message": "{'quantity': ['Quantity must be a positive number.']}", "status": "error"}
- 2026-10-03 04:12:55 UTC POST /api/v1/placeorder [error_quantity_fractional_nse] -> 400 {"message": "{'quantity': ['Fractional quantity (1.5) is not allowed for non-crypto exchanges.']}", "status": "error"}
- 2026-10-03 04:12:56 UTC POST /api/v1/placeorder [error_negative_price] -> 400 {"message": "{'price': ['Price must be a non-negative number.']}", "status": "error"}
- 2026-10-03 04:12:56 UTC POST /api/v1/placeorder [error_bad_product] -> 400 {"message": "{'product': ['Must be one of: MIS, NRML, CNC.']}", "status": "error"}
- 2026-10-03 04:12:56 UTC POST /api/v1/placeorder [error_bad_pricetype] -> 400 {"message": "{'pricetype': ['Must be one of: MARKET, LIMIT, SL, SL-M.']}", "status": "error"}
- 2026-10-03 04:12:56 UTC POST /api/v1/placeorder [error_bad_action] -> 400 {"message": "{'action': ['Must be one of: BUY, SELL, buy, sell.']}", "status": "error"}
- 2026-10-03 04:12:57 UTC POST /api/v1/placeorder [error_bad_exchange] -> 400 {"message": "{'exchange': ['Must be one of: NSE, NFO, CDS, BSE, BFO, BCD, MCX, NCDEX, NCO, NSE_INDEX, BSE_INDEX, MCX_INDEX, GLOBAL_INDEX, CRYPTO.']}", "status": "error"}
- 2026-10-03 04:12:57 UTC POST /api/v1/placeorder [error_missing_symbol] -> 400 {"message": "{'symbol': ['Missing data for required field.']}", "status": "error"}
- 2026-10-03 04:12:57 UTC POST /api/v1/placeorder [error_missing_strategy] -> 400 {"message": "{'strategy': ['Missing data for required field.']}", "status": "error"}
- 2026-10-03 04:12:57 UTC POST /api/v1/placeorder [error_limit_price_zero] -> 400 {"message": "LIMIT orders require price", "mode": "analyze", "status": "error"}
- 2026-10-03 04:12:58 UTC POST /api/v1/placeorder [error_sl_without_trigger] -> 400 {"message": "SL orders require trigger_price", "mode": "analyze", "status": "error"}
- 2026-10-03 04:12:58 UTC POST /api/v1/placeorder [error_option_qty_not_lot_multiple] -> 400 {"message": "Quantity must be in multiples of lot size 65", "mode": "analyze", "status": "error"}
- 2026-10-03 04:12:58 UTC POST /api/v1/placeorder [error_invalid_apikey] -> 403 {"message": "Invalid openalgo apikey", "status": "error"}
- 2026-10-03 04:12:58 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:12:59 UTC POST /api/v1/orderstatus [market_buy_mis_reliance] -> 200 {"data": {"action": "BUY", "average_price": 1167.7, "exchange": "NSE", "filled_quantity": 5, "order_status": "complete", "orderid": "26100376733573", "pending_quantity": 0, "price": 1167.7, "price_type": "MARKET", "produ
- 2026-10-03 04:12:59 UTC POST /api/v1/orderstatus [limit_buy_cnc_sbin_far_below] -> 200 {"data": {"action": "BUY", "average_price": 0.0, "exchange": "NSE", "filled_quantity": 0, "order_status": "open", "orderid": "26100334197687", "pending_quantity": 2, "price": 763.3, "price_type": "LIMIT", "product": "CNC
- 2026-10-03 04:12:59 UTC POST /api/v1/orderstatus [sl_buy_mis_reliance] -> 200 {"data": {"action": "BUY", "average_price": 0.0, "exchange": "NSE", "filled_quantity": 0, "order_status": "trigger pending", "orderid": "26100356024725", "pending_quantity": 1, "price": 1237.8, "price_type": "SL", "produ
- 2026-10-03 04:12:59 UTC POST /api/v1/orderstatus [slm_sell_mis_sbin] -> 200 {"data": {"action": "SELL", "average_price": 0.0, "exchange": "NSE", "filled_quantity": 0, "order_status": "trigger pending", "orderid": "26100363343152", "pending_quantity": 1, "price": 0.0, "price_type": "SL-M", "produ
- 2026-10-03 04:13:00 UTC POST /api/v1/orderstatus [market_buy_nrml_nifty_option] -> 200 {"data": {"action": "BUY", "average_price": 156.95, "exchange": "NFO", "filled_quantity": 65, "order_status": "complete", "orderid": "26100385889435", "pending_quantity": 0, "price": 156.95, "price_type": "MARKET", "prod
- 2026-10-03 04:13:00 UTC POST /api/v1/orderstatus [unknown_orderid_session2] -> 404 {"message": "Order 99999999999999 not found", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:00 UTC POST /api/v1/openposition [reliance_mis_long] -> 200 {"mode": "analyze", "quantity": 5, "status": "success"}
- 2026-10-03 04:13:01 UTC POST /api/v1/openposition [sbin_mis_short] -> 200 {"mode": "analyze", "quantity": -2, "status": "success"}
- 2026-10-03 04:13:01 UTC POST /api/v1/openposition [nifty_future_nrml] -> 200 {"mode": "analyze", "quantity": 65, "status": "success"}
- 2026-10-03 04:13:01 UTC POST /api/v1/openposition [crudeoil_future_nrml] -> 200 {"mode": "analyze", "quantity": 100, "status": "success"}
- 2026-10-03 04:13:02 UTC POST /api/v1/openposition [no_position_session2] -> 200 {"mode": "analyze", "quantity": 0, "status": "success"}
- 2026-10-03 04:13:17 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:13:17 UTC POST /api/v1/modifyorder [open_limit_price_and_qty] -> 200 {"message": "Order modified successfully", "mode": "analyze", "orderid": "26100341808353", "status": "success"}
- 2026-10-03 04:13:17 UTC POST /api/v1/orderstatus [after_modify] -> 200 {"data": {"action": "BUY", "average_price": 0.0, "exchange": "NSE", "filled_quantity": 0, "order_status": "open", "orderid": "26100341808353", "pending_quantity": 2, "price": 957.5, "price_type": "LIMIT", "product": "MIS
- 2026-10-03 04:13:17 UTC POST /api/v1/modifyorder [sl_order_trigger] -> 200 {"message": "Order modified successfully", "mode": "analyze", "orderid": "26100356024725", "status": "success"}
- 2026-10-03 04:13:18 UTC POST /api/v1/modifyorder [error_unknown_orderid] -> 404 {"message": "Order 99999999999999 not found", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:18 UTC POST /api/v1/modifyorder [error_completed_order] -> 400 {"message": "Cannot modify order in complete status", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:18 UTC POST /api/v1/modifyorder [error_missing_price] -> 400 {"message": "{'price': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:19 UTC POST /api/v1/modifyorder [error_negative_price] -> 400 {"message": "{'price': ['Price must be a non-negative number.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:19 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:13:19 UTC POST /api/v1/cancelorder [open_limit] -> 200 {"message": "Order cancelled successfully", "mode": "analyze", "orderid": "26100348649169", "status": "success"}
- 2026-10-03 04:13:19 UTC POST /api/v1/orderstatus [after_cancel] -> 200 {"data": {"action": "SELL", "average_price": 0.0, "exchange": "NSE", "filled_quantity": 0, "order_status": "cancelled", "orderid": "26100348649169", "pending_quantity": 1, "price": 1401.2, "price_type": "LIMIT", "product
- 2026-10-03 04:13:19 UTC POST /api/v1/cancelorder [error_already_cancelled] -> 400 {"message": "Cannot cancel order in cancelled status", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:20 UTC POST /api/v1/cancelorder [error_completed_order] -> 400 {"message": "Cannot cancel order in complete status", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:20 UTC POST /api/v1/cancelorder [error_unknown_orderid] -> 404 {"message": "Order 99999999999999 not found", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:20 UTC POST /api/v1/cancelorder [error_missing_orderid] -> 400 {"message": "{'orderid': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:20 UTC POST /api/v1/modifyorder [error_cancelled_order] -> 400 {"message": "Cannot modify order in cancelled status", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:20 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:13:21 UTC POST /api/v1/placesmartorder [open_from_flat_to_10] -> 200 {"mode": "analyze", "orderid": "26100341320743", "status": "success"}
- 2026-10-03 04:13:21 UTC POST /api/v1/openposition [infy_after_smart_open] -> 200 {"mode": "analyze", "quantity": 10, "status": "success"}
- 2026-10-03 04:13:22 UTC POST /api/v1/placesmartorder [raise_10_to_15] -> 200 {"mode": "analyze", "orderid": "26100319444548", "status": "success"}
- 2026-10-03 04:13:22 UTC POST /api/v1/placesmartorder [reduce_15_to_5] -> 200 {"mode": "analyze", "orderid": "26100360033381", "status": "success"}
- 2026-10-03 04:13:22 UTC POST /api/v1/placesmartorder [no_action_already_at_5] -> 200 {"message": "No OpenPosition Found. Not placing Exit order.", "mode": "analyze", "status": "success"}
- 2026-10-03 04:13:23 UTC POST /api/v1/placesmartorder [to_zero] -> 200 {"mode": "analyze", "orderid": "26100339397629", "status": "success"}
- 2026-10-03 04:13:23 UTC POST /api/v1/placesmartorder [flat_to_short_minus_3] -> 200 {"mode": "analyze", "orderid": "26100379951322", "status": "success"}
- 2026-10-03 04:13:24 UTC POST /api/v1/openposition [infy_after_smart_short] -> 200 {"mode": "analyze", "quantity": -3, "status": "success"}
- 2026-10-03 04:13:24 UTC POST /api/v1/placesmartorder [error_missing_position_size] -> 400 {"message": "{'position_size': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:24 UTC POST /api/v1/placesmartorder [error_unknown_symbol] -> 400 {"message": "Symbol NOTASYMBOL not found on NSE", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:46 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:13:46 UTC POST /api/v1/basketorder [three_legs_mixed] -> 200 {"mode": "analyze", "results": [{"batch_order": true, "exchange": "NSE", "is_last_order": false, "orderid": "26100392570398", "product": "MIS", "status": "success", "symbol": "TCS"}, {"batch_order": true, "exchange": "NS
- 2026-10-03 04:13:47 UTC POST /api/v1/basketorder [one_leg_unknown_symbol] -> 200 {"mode": "analyze", "results": [{"batch_order": true, "exchange": "NSE", "is_last_order": false, "orderid": "26100330880794", "product": "MIS", "status": "success", "symbol": "ITC"}, {"message": "Symbol NOTASYMBOL not fo
- 2026-10-03 04:13:47 UTC POST /api/v1/basketorder [error_empty_orders] -> 400 {"message": "{'orders': ['Orders must contain at least 1 item.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:47 UTC POST /api/v1/basketorder [error_leg_bad_product] -> 400 {"message": "{'orders': {0: {'product': ['Must be one of: MIS, NRML, CNC.']}}}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:47 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:13:48 UTC POST /api/v1/splitorder [sbin_10_split_3] -> 200 {"mode": "analyze", "results": [{"order_num": 1, "orderid": "26100319045249", "quantity": 3, "status": "success"}, {"order_num": 2, "orderid": "26100322216224", "quantity": 3, "status": "success"}, {"order_num": 3, "orde
- 2026-10-03 04:13:48 UTC POST /api/v1/splitorder [error_splitsize_zero] -> 400 {"message": "{'splitsize': ['Split size must be a positive integer.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:48 UTC POST /api/v1/splitorder [error_missing_splitsize] -> 400 {"message": "{'splitsize': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:48 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:13:49 UTC POST /api/v1/optionsorder [atm_ce_buy] -> 200 {"exchange": "NFO", "mode": "analyze", "offset": "ATM", "option_type": "CE", "orderid": "26100318862823", "status": "success", "symbol": "NIFTY06OCT2622400CE", "underlying": "NIFTY", "underlying_ltp": 22421.95}
- 2026-10-03 04:13:49 UTC POST /api/v1/optionsorder [itm2_pe_buy] -> 200 {"exchange": "NFO", "mode": "analyze", "offset": "ITM2", "option_type": "PE", "orderid": "26100358895357", "status": "success", "symbol": "NIFTY06OCT2622500PE", "underlying": "NIFTY", "underlying_ltp": 22421.95}
- 2026-10-03 04:13:50 UTC POST /api/v1/optionsorder [otm3_ce_sell] -> 200 {"exchange": "NFO", "mode": "analyze", "offset": "OTM3", "option_type": "CE", "orderid": "26100399133028", "status": "success", "symbol": "NIFTY06OCT2622550CE", "underlying": "NIFTY", "underlying_ltp": 22421.95}
- 2026-10-03 04:13:50 UTC POST /api/v1/optionsorder [atm_ce_buy_with_splitsize] -> 200 {"exchange": "NFO", "mode": "analyze", "offset": "ATM", "option_type": "CE", "results": [{"order_num": 1, "orderid": "26100341149649", "quantity": 65, "status": "success"}, {"order_num": 2, "orderid": "26100359455808", "
- 2026-10-03 04:13:50 UTC POST /api/v1/optionsorder [error_bad_offset] -> 400 {"message": "Offset ATM99X is out of range for available strikes. Please use a smaller offset.", "status": "error"}
- 2026-10-03 04:13:51 UTC POST /api/v1/optionsorder [error_bad_expiry] -> 404 {"message": "No strikes found for NIFTY expiring 01JAN20. Please check expiry date or update master contract.", "status": "error"}
- 2026-10-03 04:13:51 UTC POST /api/v1/optionsorder [error_qty_not_lot_multiple] -> 400 {"message": "Quantity must be in multiples of lot size 65", "mode": "analyze", "status": "error"}
- 2026-10-03 04:13:51 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:13:52 UTC POST /api/v1/optionsmultiorder [bull_call_spread_2_legs] -> 200 {"mode": "analyze", "results": [{"action": "BUY", "exchange": "NFO", "leg": 1, "mode": "analyze", "offset": "ATM", "option_type": "CE", "orderid": "26100308621852", "product": "NRML", "status": "success", "strike": null,
- 2026-10-03 04:13:52 UTC POST /api/v1/optionsmultiorder [error_empty_legs] -> 400 {"errors": {"legs": ["Legs must contain 1 to 20 items."]}, "message": "Validation error", "status": "error"}
- 2026-10-03 04:13:52 UTC POST /api/v1/optionsmultiorder [one_leg_bad_offset] -> 200 {"mode": "analyze", "results": [{"action": "BUY", "exchange": "NFO", "leg": 1, "mode": "analyze", "offset": "ATM", "option_type": "PE", "orderid": "26100378391337", "product": "NRML", "status": "success", "strike": null,
- 2026-10-03 04:14:03 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:14:03 UTC POST /api/v1/placegttorder [single_buy_cnc_trigger_below] -> 200 {"mode": "analyze", "status": "success", "trigger_id": "GTT-261003-12a093ec"}
- 2026-10-03 04:14:03 UTC POST /api/v1/placegttorder [oco_sell_cnc] -> 200 {"mode": "analyze", "status": "success", "trigger_id": "GTT-261003-56ad1482"}
- 2026-10-03 04:14:04 UTC POST /api/v1/placegttorder [error_mis_product] -> 400 {"message": "{'product': ['GTT supports only CNC (delivery) or NRML (overnight F&O); MIS is intraday-only.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:04 UTC POST /api/v1/placegttorder [error_bad_trigger_type] -> 400 {"message": "{'trigger_type': [\"Must be 'SINGLE' or 'OCO'.\"]}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:04 UTC POST /api/v1/placegttorder [error_oco_missing_legs] -> 400 {"message": "{'stoploss': ['Required for OCO (stoploss leg limit).']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:04 UTC POST /api/v1/placegttorder [error_unknown_symbol] -> 400 {"message": "Symbol not found", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:05 UTC POST /api/v1/gttorderbook [after_place] -> 200 {"data": [{"created_at": "2026-10-03T09:44:03.824776", "exchange": "NSE", "expires_at": "2027-10-03T09:44:03.823976", "last_price": 954.1, "legs": [{"action": "SELL", "price": 849.1, "pricetype": "LIMIT", "product": "CNC
- 2026-10-03 04:14:05 UTC POST /api/v1/gttorderbook [status_active] -> 200 {"data": [{"created_at": "2026-10-03T09:44:03.824776", "exchange": "NSE", "expires_at": "2027-10-03T09:44:03.823976", "last_price": 954.1, "legs": [{"action": "SELL", "price": 849.1, "pricetype": "LIMIT", "product": "CNC
- 2026-10-03 04:14:20 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:14:20 UTC POST /api/v1/modifygttorder [single_change_trigger_and_qty] -> 200 {"mode": "analyze", "status": "success", "trigger_id": "GTT-261003-12a093ec"}
- 2026-10-03 04:14:20 UTC POST /api/v1/modifygttorder [oco_change_target] -> 200 {"mode": "analyze", "status": "success", "trigger_id": "GTT-261003-56ad1482"}
- 2026-10-03 04:14:21 UTC POST /api/v1/modifygttorder [error_unknown_trigger_id] -> 500 {"message": "An unexpected error occurred", "status": "error"}
- 2026-10-03 04:14:21 UTC POST /api/v1/modifygttorder [error_missing_trigger_id] -> 400 {"message": "{'trigger_id': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:21 UTC POST /api/v1/gttorderbook [after_modify] -> 200 {"data": [{"created_at": "2026-10-03T09:44:03.824776", "exchange": "NSE", "expires_at": "2027-10-03T09:44:03.823976", "last_price": 954.1, "legs": [{"action": "SELL", "price": 849.1, "pricetype": "LIMIT", "product": "CNC
- 2026-10-03 04:14:22 UTC POST /api/v1/cancelgttorder [single] -> 200 {"mode": "analyze", "status": "success", "trigger_id": "GTT-261003-12a093ec"}
- 2026-10-03 04:14:22 UTC POST /api/v1/cancelgttorder [error_already_cancelled] -> 404 {"message": "No active GTT with trigger_id 'GTT-261003-12a093ec'", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:22 UTC POST /api/v1/cancelgttorder [error_unknown_trigger_id] -> 404 {"message": "No active GTT with trigger_id 'GTT-000000-deadbeef'", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:22 UTC POST /api/v1/cancelgttorder [error_missing_trigger_id] -> 400 {"message": "{'trigger_id': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:14:23 UTC POST /api/v1/modifygttorder [error_cancelled_trigger] -> 500 {"message": "An unexpected error occurred", "status": "error"}
- 2026-10-03 04:14:23 UTC POST /api/v1/gttorderbook [after_cancel_all_statuses] -> 200 {"data": [{"created_at": "2026-10-03T09:44:03.824776", "exchange": "NSE", "expires_at": "2027-10-03T09:44:03.823976", "last_price": 954.1, "legs": [{"action": "SELL", "price": 849.1, "pricetype": "LIMIT", "product": "CNC
- 2026-10-03 04:14:23 UTC POST /api/v1/gttorderbook [status_cancelled] -> 400 {"message": {"status": ["Must be one of: active, all."]}, "status": "error"}
- 2026-10-03 04:14:24 UTC POST /api/v1/gttorderbook [error_bad_status] -> 400 {"message": {"status": ["Must be one of: active, all."]}, "status": "error"}
- 2026-10-03 04:14:58 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:14:58 UTC POST /api/v1/placesmartorder [no_action_qty_nonzero_position_matches] -> 200 {"message": "Positions Already Matched. No Action needed.", "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:01 UTC POST /api/v1/orderbook [populated] -> 200 {"data": {"orders": [{"action": "BUY", "average_price": 103.6, "exchange": "NFO", "filled_quantity": 65, "order_status": "complete", "orderid": "26100378391337", "pending_quantity": 0, "price": 103.6, "pricetype": "MARKE
- 2026-10-03 04:15:02 UTC POST /api/v1/tradebook [populated] -> 200 {"data": [{"action": "BUY", "average_price": 103.6, "exchange": "NFO", "orderid": "26100378391337", "price": 103.6, "product": "NRML", "quantity": 65, "strategy": "fixtures", "symbol": "NIFTY06OCT2622400PE", "timestamp":
- 2026-10-03 04:15:02 UTC POST /api/v1/positionbook [populated] -> 200 {"data": [{"average_price": 954.1, "exchange": "NSE", "lot_size": 1.0, "ltp": 954.1, "pnl": 0.0, "pnlpercent": 0.0, "product": "CNC", "quantity": 2, "symbol": "SBIN", "today_realized_pnl": 0.0, "total_pnl_today": 0.0, "u
- 2026-10-03 04:15:02 UTC POST /api/v1/holdings [after_cnc_buys] -> 200 {"data": {"holdings": [], "statistics": {"totalholdingvalue": 0.0, "totalinvvalue": 0.0, "totalpnlpercentage": 0.0, "totalprofitandloss": 0.0}}, "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:03 UTC POST /api/v1/funds [after_activity] -> 200 {"data": {"availablecash": 9666628.79, "collateral": 0.0, "grossexposure": 333371.21, "last_reset": "2026-10-03 09:30:17", "m2mrealized": 0.0, "m2munrealized": 0.0, "reset_count": 0, "today_realized_pnl": 0.0, "total_rea
- 2026-10-03 04:15:03 UTC POST /api/v1/pnl/symbols [symbols_populated] -> 200 {"data": [{"exchange": "NSE", "pnl": 0.0, "product": "CNC", "quantity": 2, "symbol": "SBIN", "today_realized_pnl": 0.0, "total_pnl_today": 0.0, "unrealized_pnl": 0.0}, {"exchange": "NSE", "pnl": 0.0, "product": "MIS", "q
- 2026-10-03 04:15:03 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:15:03 UTC POST /api/v1/cancelallorder [with_open_orders] -> 200 {"canceled_orders": ["26100395374263", "26100341808353", "26100334197687"], "failed_cancellations": [], "message": "Canceled 3 orders. Failed to cancel 0 orders.", "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:03 UTC POST /api/v1/cancelallorder [nothing_open] -> 200 {"canceled_orders": [], "failed_cancellations": [], "message": "No open orders to cancel", "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:04 UTC POST /api/v1/cancelallorder [error_missing_strategy] -> 400 {"message": "{'strategy': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:15:04 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:15:05 UTC POST /api/v1/closeposition [with_open_positions] -> 200 {"closed_positions": 15, "failed_closures": 0, "message": "Closed 15 positions", "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:07 UTC POST /api/v1/positionbook [after_closeposition] -> 200 {"data": [], "mode": "analyze", "status": "success", "total_pnl": 0.0, "total_pnl_today": 0.0, "total_today_realized_pnl": 0.0, "total_unrealized_pnl": 0.0}
- 2026-10-03 04:15:08 UTC POST /api/v1/closeposition [no_open_positions] -> 200 {"message": "No open positions to close", "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:08 UTC POST /api/v1/closeposition [error_missing_strategy] -> 400 {"message": "{'strategy': ['Missing data for required field.']}", "mode": "analyze", "status": "error"}
- 2026-10-03 04:15:08 UTC POST /api/v1/orderbook [after_cleanup] -> 200 {"data": {"orders": [{"action": "SELL", "average_price": 103.6, "exchange": "NFO", "filled_quantity": 65, "order_status": "complete", "orderid": "26100369795033", "pending_quantity": 0, "price": 103.6, "pricetype": "MARK
- 2026-10-03 04:15:09 UTC POST /api/v1/tradebook [after_cleanup] -> 200 {"data": [{"action": "SELL", "average_price": 103.6, "exchange": "NFO", "orderid": "26100369795033", "price": 103.6, "product": "NRML", "quantity": 65, "strategy": "AUTO_SQUARE_OFF", "symbol": "NIFTY06OCT2622400PE", "tim
- 2026-10-03 04:15:09 UTC POST /api/v1/funds [after_cleanup] -> 200 {"data": {"availablecash": 9998893.2, "collateral": 0.0, "grossexposure": 1106.8, "last_reset": "2026-10-03 09:30:17", "m2mrealized": 0.0, "m2munrealized": 0.0, "reset_count": 0, "today_realized_pnl": 0.0, "total_realize
- 2026-10-03 04:15:09 UTC POST /api/v1/pnl/symbols [symbols_after_cleanup] -> 200 {"data": [], "mode": "analyze", "status": "success", "total_pnl": 0.0, "total_pnl_today": 0.0, "total_today_realized_pnl": 0.0, "total_unrealized_pnl": 0.0}
- 2026-10-03 04:15:37 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:15:37 UTC POST /api/v1/cancelorder [trigger_pending_sl] -> 200 {"message": "Order cancelled successfully", "mode": "analyze", "orderid": "26100356024725", "status": "success"}
- 2026-10-03 04:15:38 UTC POST /api/v1/cancelorder [trigger_pending_slm] -> 200 {"message": "Order cancelled successfully", "mode": "analyze", "orderid": "26100363343152", "status": "success"}
- 2026-10-03 04:15:38 UTC POST /api/v1/cancelgttorder [oco] -> 500 {"message": "Could not release 1106.80 margin (Cannot release \u20b91106.80: more than the reserved margin). The GTT is unchanged - retry the cancel.", "mode": "analyze", "status": "error", "trigger_id": "GTT-261003-56ad
- 2026-10-03 04:15:38 UTC POST /api/v1/gttorderbook [after_all_cancelled] -> 200 {"data": [{"created_at": "2026-10-03T09:44:03.824776", "exchange": "NSE", "expires_at": "2027-10-03T09:44:03.823976", "last_price": 954.1, "legs": [{"action": "SELL", "price": 849.1, "pricetype": "LIMIT", "product": "CNC
- 2026-10-03 04:15:38 UTC pre-toggle in-flight orders: 0
- 2026-10-03 04:15:38 UTC POST /api/v1/analyzer/toggle [toggle_error_missing_mode] -> 400 {"message": "{'mode': ['Missing data for required field.']}", "status": "error"}
- 2026-10-03 04:15:39 UTC POST /api/v1/analyzer/toggle [toggle_error_invalid_mode] -> 400 {"message": "{'mode': ['Not a valid boolean.']}", "status": "error"}
- 2026-10-03 04:15:39 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:15:39 UTC POST /api/v1/analyzer/toggle [toggle_to_live_false] -> 200 {"data": {"analyze_mode": false, "message": "Analyzer mode switched to live", "mode": "live", "total_logs": 83}, "status": "success"}
- 2026-10-03 04:15:39 UTC POST /api/v1/analyzer [status_while_live] -> 200 {"data": {"analyze_mode": false, "mode": "live", "total_logs": 83}, "status": "success"}
- 2026-10-03 04:15:39 UTC POST /api/v1/analyzer/toggle [toggle_back_to_analyze_true] -> 200 {"data": {"analyze_mode": true, "message": "Analyzer mode switched to analyze", "mode": "analyze", "total_logs": 83}, "status": "success"}
- 2026-10-03 04:15:39 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:15:40 UTC POST /api/v1/analyzer [status_after_round_trip] -> 200 {"data": {"analyze_mode": true, "mode": "analyze", "total_logs": 83}, "status": "success"}
- 2026-10-03 04:15:40 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:15:47 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True
- 2026-10-03 04:15:47 UTC POST /api/v1/cancelgttorder [oco_retry] -> 500 {"message": "Could not release 1106.80 margin (Cannot release \u20b91106.80: more than the reserved margin). The GTT is unchanged - retry the cancel.", "mode": "analyze", "status": "error", "trigger_id": "GTT-261003-56ad
- 2026-10-03 04:15:48 UTC POST /api/v1/cancelallorder [final_cleanup] -> 200 {"canceled_orders": [], "failed_cancellations": [], "message": "No open orders to cancel", "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:48 UTC POST /api/v1/closeposition [final_cleanup] -> 200 {"message": "No open positions to close", "mode": "analyze", "status": "success"}
- 2026-10-03 04:15:48 UTC POST /api/v1/gttorderbook [final] -> 200 {"data": [{"created_at": "2026-10-03T09:44:03.824776", "exchange": "NSE", "expires_at": "2027-10-03T09:44:03.823976", "last_price": 954.1, "legs": [{"action": "SELL", "price": 849.1, "pricetype": "LIMIT", "product": "CNC
- 2026-10-03 04:15:48 UTC POST /api/v1/positionbook [final] -> 200 {"data": [], "mode": "analyze", "status": "success", "total_pnl": 0.0, "total_pnl_today": 0.0, "total_today_realized_pnl": 0.0, "total_unrealized_pnl": 0.0}
- 2026-10-03 04:15:49 UTC POST /api/v1/funds [final] -> 200 {"data": {"availablecash": 9999319.7, "collateral": 0.0, "grossexposure": 680.3, "last_reset": "2026-10-03 09:30:17", "m2mrealized": 0.0, "m2munrealized": 0.0, "reset_count": 0, "today_realized_pnl": 0.0, "total_realized
- 2026-10-03 04:15:49 UTC POST /api/v1/analyzer [status_final] -> 200 {"data": {"analyze_mode": true, "mode": "analyze", "total_logs": 86}, "status": "success"}
- 2026-10-03 04:16:39 UTC POST /api/v1/gttorderbook [status_all] -> 200 {"data": [{"created_at": "2026-10-03T09:44:03.824776", "exchange": "NSE", "expires_at": "2027-10-03T09:44:03.823976", "last_price": 954.1, "legs": [{"action": "SELL", "price": 849.1, "pricetype": "LIMIT", "product": "CNC
- 2026-10-03 04:17:38 UTC POST /api/v1/analyzer (guard) -> analyze_mode=True

## Session 2 summary
- analyze_mode at start: true (not toggled for order capture). Every order batch was preceded by a guard call that confirmed analyze_mode=true; none ever returned false.
- Toggle round trip at 04:15:39 UTC with zero in-flight orders: analyze -> live (false) -> analyze (true) within the same second; verified true afterwards and at 04:15:49 and at session end.
- Cleanup: cancelallorder + closeposition returned "No open orders to cancel" / "No open positions to close". Two trigger-pending orders had to be cancelled individually (cancelallorder bug). One OCO GTT (GTT-261003-56ad1482, sandbox only) could not be cancelled: cancelgttorder returns 500 margin-release error; it remains active in the sandbox.
- Final analyze_mode: true (same as at start).
