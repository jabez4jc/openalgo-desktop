# Mutating endpoint request contracts (documented from source; exercised in analyze mode in session 2, see INDEX.md rows marked S2)

Extracted from `openalgo/restx_api/schemas.py`. `*` = required. All are `POST /api/v1/<name>` with JSON body including `apikey` and `strategy`. Defaults: `pricetype` MARKET, `product` MIS, `price` 0.0, `trigger_price` 0.0, `disclosed_quantity` 0. `quantity` is Float in the schema but coerced to int for non-CRYPTO exchanges (fractional -> 400). `action` accepts BUY/SELL/buy/sell.

| Endpoint | Schema | Fields |
|---|---|---|
| placeorder | OrderSchema | apikey*, strategy*, exchange*, symbol*, action*, quantity* (Float>0), pricetype, product, price, trigger_price, disclosed_quantity, underlying_ltp |
| placesmartorder | SmartOrderSchema | apikey*, strategy*, exchange*, symbol*, action*, quantity* (>=0), position_size*, pricetype, product, price, trigger_price, disclosed_quantity |
| modifyorder | ModifyOrderSchema | apikey*, strategy*, exchange*, symbol*, orderid*, action*, product*, pricetype*, price*, quantity*, disclosed_quantity*, trigger_price* |
| cancelorder | CancelOrderSchema | apikey*, strategy*, orderid* |
| cancelallorder | CancelAllOrderSchema | apikey*, strategy* |
| closeposition | ClosePositionSchema | apikey*, strategy* |
| basketorder | BasketOrderSchema | apikey*, strategy*, orders*: [BasketOrderItemSchema{exchange*, symbol*, action*, quantity*, pricetype, product, price, trigger_price, disclosed_quantity}] |
| splitorder | SplitOrderSchema | apikey*, strategy*, exchange*, symbol*, action*, quantity*, splitsize*, pricetype, product, price, trigger_price, disclosed_quantity |
| optionsorder | OptionsOrderSchema | apikey*, strategy*, underlying, exchange*, expiry_date (DDMMMYY), strike_int, offset* (ATM/ITMn/OTMn), option_type (CE/PE), action*, quantity, splitsize, pricetype, product, price, trigger_price, disclosed_quantity |
| optionsmultiorder | OptionsMultiOrderSchema | apikey*, strategy*, underlying*, exchange*, expiry_date, strike_int, legs* (1..20): [{offset*, option_type, action*, quantity, splitsize, expiry_date, pricetype, product, price, trigger_price, disclosed_quantity}] |
| placegttorder | PlaceGTTOrderSchema | apikey*, strategy*, trigger_type* (SINGLE/OCO), exchange*, symbol*, action*, product, quantity, pricetype, price, triggerprice_sl, triggerprice_tg, stoploss, target, expires_at |
| modifygttorder | ModifyGTTOrderSchema | apikey*, strategy*, trigger_id*, trigger_type*, exchange*, symbol*, action*, product, quantity, pricetype, price, triggerprice_sl, triggerprice_tg, stoploss, target |
| cancelgttorder | CancelGTTOrderSchema | apikey*, strategy*, trigger_id* |
| gttorderbook (read) | GTTOrderBookSchema | apikey*, status |
| analyzer/toggle | AnalyzerToggleSchema | apikey*, mode* (Bool) |
| chart (POST) | ChartSchema | apikey* + arbitrary preference keys (persists preferences) |
| strategy/start, stop, close_all, close_leg, runs, orders, events | strategy_schema.py | apikey*, strategy_id* (Integer) ... |
| portfolio/backtest, tearsheet, holdings | portfolio.py | apikey*, lookback_days, benchmark, benchmark_exchange, risk_free_rate, source (compute-heavy, not pure reads) |
