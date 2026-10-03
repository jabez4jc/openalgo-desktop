# 05 - Frontend parity audit: OpenAlgo web (frontend/) vs OpenAlgo Desktop (Tauri src/)

Web root: `/Users/openalgo/openalgo-desktop/openalgo/frontend/src` (HEAD ad2a3f5, 2026-10-01).
Desktop root: `/Users/openalgo/openalgo-desktop/openalgo-desktop/src` (last src commit 6947b4c, 2026-01-19).
Nothing was modified in either repo. Scope per coordinator: port the ENTIRE web app except Python Strategy Host (/python), Flow (/flow) and pandas-backtester tools; those appear only in the "Excluded" table (section 3.4).

## 0. Headline findings

1. The desktop frontend is a hand-edited copy of the web frontend as it stood on ~2026-01-18 (web Phase 1-6 commits b0706d36e..cc3908ce0, 2026-01-10..12). The web repo has landed 584 of its 653 `frontend/src` commits since then. No desktop page blob matches any web commit (all were edited for Tauri), and only 40 files are byte-identical (31 of them `components/ui/*`).
2. Size: web `src` = 180,540 non-test LOC in 63 top-level pages + 42 nested pages; desktop `src` = 39,889 LOC, 66 routes vs web 121. The web /trading terminal alone is 49,117 non-test LOC (193-file closure, 64,802 LOC incl. shared modules).
3. Desktop wiring is broken in 10 pages that import the `webClient` stub (`src/api/client.ts:35-66` returns `{data:null}`), ~45 raw `fetch('/...')` calls to Flask-relative URLs that have no server inside a Tauri webview, one live `socket.io-client` connection (ActionCenter), 3 Tauri event names listened for but never emitted by Rust, and the whole router is `react-router-dom@7` while web moved to `react-router@8` (e4240ef0c, 2026-08-01).
4. All 51 Tauri `invoke()` command names used by desktop TS are registered in `src-tauri/src/lib.rs:76-185` (no missing commands). The gaps are features, not plumbing.
5. Theme: the first 357 lines of `index.css` (all HSL and OKLch tokens for :root/.dark/.analyzer/.sandbox and 6 accent themes) are byte-identical. Desktop lacks web lines 358-951: analyzer alert overrides, Flow styles (excluded), global themed scrollbars + `color-scheme`, `.no-scrollbar`, and the `--oscript-*` syntax tokens needed by /trading's ScriptPanel.
6. /trading OpenScript backtests run client-side in a Web Worker (`lib/trading/backtestRun.ts:2-10`, `backtestWorkerEntry.ts`, `backtestFold.ts:70-79` -> `openalgo-script.backtest()`); the only network calls are `/api/v1/symbol` and `/api/v1/history` (`backtestRun.ts:217,372`). Live OpenScript strategies (StrategiesPanel) are the one /trading feature that needs Python: `services/openscript_runner_service.py` spawns `openscript_host/openscript_runner.py`, which imports the pip package `openscript==0.8.1` (`requirements.txt:154`).
7. Tests: web has 244 unit test files / 2,644 cases / 52,606 LOC (1,070 cases in `lib/trading`, 362 in `components/trading`), 7 axe-based a11y test files, 5 Playwright specs / 19 cases, 7 shared test utils (828 LOC). Desktop has 4 test files.

---

## 1. Inventory

### 1.1 Route list (web `App.tsx:170-330` vs desktop `App.tsx:97-193`)

Status legend: Identical = file byte-identical; Diverged = present in both, differs (changed-line count from `diff`); Stub = page present but backend module is a NOT_AVAILABLE stub or `webClient` null stub; Missing = no desktop route/page.

| Surface | Web route(s) | Web page (LOC) | Desktop status | Notes |
|---|---|---|---|---|
| Home | `/` | Home.tsx 449 | Diverged (281) | Web has hero counts from `lib/tools.ts` registry; desktop 196 LOC |
| FAQ | `/faq` | Faq.tsx 393 | Missing | fetches `/auth/app-info` |
| Setup | `/setup` | Setup.tsx 316 | Diverged (107) | desktop uses `invoke('check_setup'/'setup')` Setup.tsx:78,127 |
| Login | `/login` | Login.tsx 417 | Diverged (489) | web: `/auth/login`, `/auth/login/totp`, `/auth/session-status`; desktop: invoke login/check_session Login.tsx:61-140 |
| Reset password | `/reset-password` | ResetPassword.tsx 527 | Diverged (7) but broken | 4x raw `fetch('/auth/reset-password')` ResetPassword.tsx:132,162,197,231 + stub CSRF |
| Download | `/download` | Download.tsx 165 | Identical | |
| Server error / rate limited | `/error`, `/rate-limited` | 133 / 125 | Diverged (14/12) | ServerError.tsx:16 raw `fetch('/auth/logout')` |
| Broker select | `/broker` | BrokerSelect.tsx 353 | Diverged (769) | desktop is a 742-LOC rewrite: keychain creds, OAuth `listen('oauth_callback')` BrokerSelect.tsx:147 |
| Broker TOTP | `/broker/:broker/totp`, `/:broker/auth` | BrokerTOTP.tsx 555 | Diverged (178) | |
| Samco auth | `/broker/samco/auth` | SamcoAuth.tsx 223 | Missing | `/samco/callback`, `/samco/ip-status` (blueprints/brlogin.py) |
| Dashboard | `/dashboard` | Dashboard.tsx 505 | Diverged (145) | web: `/auth/dashboard-data`, `/api/master-contract/status`; desktop invoke get_funds/get_sandbox_funds/get_symbol_count Dashboard.tsx:100-157 |
| Positions | `/positions` | Positions.tsx 1079 | Diverged (455) | web uses `EmptyState`, `useLivePrice`, `useOrderEventRefresh` (socket) |
| Orderbook | `/orderbook` | OrderBook.tsx 972 | Diverged (900) | desktop keeps axios-shaped error handling OrderBook.tsx:177,196,252 |
| Tradebook | `/tradebook` | TradeBook.tsx 593 | Diverged (311) | |
| Holdings | `/holdings` (HoldingsRoute guard, crypto brokers redirect) | Holdings.tsx 501 | Diverged (258) | guard uses `stores/brokerStore.ts` (missing in desktop) |
| Search / Token | `/search`, `/search/token` | Search.tsx 585, Token.tsx 788 | Diverged (296/528) | desktop raw fetch `/search/api/*` Token.tsx:89,90,127,165, Search.tsx:85 |
| API key | `/apikey` | ApiKey.tsx 392 | Diverged (129) | invoke get_user_api_key/regenerate_api_key ApiKey.tsx:67,96 |
| Platforms | `/platforms` | Platforms.tsx 93 | Diverged (2) | import specifier only |
| TradingView webhook | `/tradingview` | TradingView.tsx 465 | Diverged (105) | raw fetch `/api/config/host`, `/search/api/search` TradingView.tsx:71,106 |
| GoCharting webhook | `/gocharting` | GoCharting.tsx 449 | Diverged (101) | same pattern GoCharting.tsx:69,104 |
| PnL tracker | `/pnl-tracker` | PnLTracker.tsx 615 | Diverged (229) | local CSRF copy PnLTracker.tsx:8, raw fetch `/pnltracker/api/pnl`:213 |
| Sandbox config / PnL | `/sandbox`, `/sandbox/mypnl` | 512 / 558 | Diverged (353/728) | desktop maps to sandbox invoke commands |
| Analyzer | `/analyzer`, `/logs/sandbox` | Analyzer.tsx 419 | Diverged (9) | |
| Tools index | `/tools` | Tools.tsx 50 + lib/tools.ts 135 | Missing | registry of 18 tools |
| Option chain | `/optionchain` | OptionChain.tsx 1106 (+18 files, 4,620 LOC) | Missing | `components/option-chain/*`, `hooks/useOptionChainLive/Polling/Preferences` |
| Option Greeks (IV chart) | `/ivchart` | IVChart.tsx 658 | Missing | |
| OI tracker / OI range / Max pain | `/oitracker`, `/oirange`, `/maxpain` | 478 / 758 / 451 | Missing | share `api/oi-tracker.ts` |
| Gamma density / GEX / IV smile / OI profile / Vol surface | `/gammadensity`, `/gex`, `/ivsmile`, `/oiprofile`, `/volsurface` | 600/806/565/621/438 | Missing | plotly.js + react-plotly.js |
| Straddle chart / Straddle PnL | `/straddle`, `/straddlepnl` | 825 / 989 | Missing | lightweight-charts |
| Arbitrage | `/arbitrage` | Arbitrage.tsx 765 | Missing | |
| Strategy builder / portfolio | `/strategybuilder`, `/strategybuilder/portfolio` (+ `/tools/strategy*` redirects) | 2092 / 844 (+41 files, 14,953 LOC) | Missing | `components/strategy-builder/*` 5,462 LOC |
| Portfolio backtester / results, SIP backtester / results, Portfolio analyzer | `/portfolio-backtester(/results)`, `/sip-backtester(/results)`, `/portfolio-analyzer`, `/portfolio` redirect | 424/1564/347/709/529 | Missing | EXCLUDED (pandas backtesters) |
| Scalping | `/scalping` | Scalping.tsx 1873 (+9 files, 3,494 LOC) | Missing | `components/scalping/*`, `api/scalping.ts`, socket `scalping_sl_update` |
| WebSocket test | `/websocket/test`, `/websocket/test/{20,30,50}` | WebSocketTest.tsx 1455 | Diverged (864) | desktop uses Tauri `listen('market_tick')` WebSocketTest.tsx:635; web depth 20/30/50 routes missing |
| WebSocket order | `/websocket/order` | WebSocketOrder.tsx 694 | Missing | |
| Chart test (dev) | `/chart/test` | ChartTest.tsx 550 | Missing | dev-only page |
| Python strategies | `/python/*` (6 routes) | 3,445 LOC | Stub | EXCLUDED; desktop `api/python-strategy.ts` is NOT_AVAILABLE stub |
| Strategy module (RMS) | `/strategy`, `/strategy/new`, `/strategy/:id`, `/strategy/:id/edit` | List 255, Wizard 2051, Detail 3050, Edit 66 (+6 files, 6,493 LOC) | Missing (desktop has LEGACY module) | desktop pages/strategy/{StrategyIndex,NewStrategy,ViewStrategy,ConfigureSymbols} are the pre-Aug-2026 webhook-strategy UI, retired in web by 58dc8adb5 / replaced by 2a5d57c53 (2026-08-30) |
| Chartink | `/chartink`, `/chartink/new`, `/chartink/:id`, `/chartink/:id/configure` | 1,514 LOC | Stub | desktop `api/chartink.ts` NOT_AVAILABLE stub; pages raw-fetch `/api/config/host` ChartinkIndex.tsx:51, ViewChartinkStrategy.tsx:76 |
| Flow | `/flow`, `/flow/shortcuts`, `/flow/editor/:id` | 1,934 LOC + 63 nodes | Missing | EXCLUDED |
| Leverage | `/leverage` (LeverageRoute guard on `capabilities.leverage_config`) | Leverage.tsx 152 | Missing | |
| Admin | `/admin`, `/admin/freeze`, `/admin/holidays`, `/admin/timings` | 199/522/621/381 | Stub (Diverged 41/49/189/33) | desktop `api/admin.ts` NOT_AVAILABLE stub although Rust has market holiday/timing commands (lib.rs:166-174) |
| Admin diagnostics / Remote MCP | `/admin/diagnostics`, `/admin/remote-mcp` | 741 (+webServerSummary.ts 188) / 864 | Missing | desktop has extra `/admin/server` ServerSettings.tsx (webhook server config) |
| Telegram | `/telegram`, `/telegram/config`, `/telegram/users`, `/telegram/analytics` | 1,493 LOC | Stub | pages import `webClient` stub (see 2.1) |
| WhatsApp | `/whatsapp` | WhatsAppIndex.tsx 356 | Missing | socket events whatsapp_qr/paired/pair_code/pair_status/status |
| Logs | `/logs`, `/logs/live` | LogsIndex 111, Logs 470 | Stub | Logs.tsx:18,65 `webClient.get('/logs/...')` returns null |
| Monitoring | `/logs/security`, `/logs/traffic`, `/logs/latency` | 1491/339/625 | Stub | all three import `webClient` stub |
| Health monitor | `/health` | HealthMonitor.tsx 893 (+api/health.ts) | Missing | |
| Profile | `/profile` | Profile.tsx 2392 | Stub (Diverged 651) | 11 `webClient` calls Profile.tsx:222-583 return null |
| Master contract | `/master-contract` | MasterContract.tsx 548 | Missing | `/api/master-contract/*`, `/api/cache/*` |
| Action center | `/action-center` | ActionCenter.tsx 856 | Stub (Diverged 244) | `webClient` + live `io()` socket ActionCenter.tsx:17,152 |
| Playground | `/playground` | Playground.tsx 1200 (+components/playground 965) | Diverged (899) | desktop has `config/playgroundEndpoints.ts` |
| Trading terminal | `/trading` | Trading.tsx 1800 (+193-file closure) | Missing | section 4 |
| Agent | `/agent`, `/agent/config` | AgentIndex 82, AgentChat 561, AgentConfig 167 (+52 files, 17,685 LOC) | Missing | `components/agent/*`, `lib/agent/*`, `api/agent.ts` 1,370 LOC |
| Historify | `/historify`, `/historify/charts(/:symbol)` | Historify 3535, HistorifyCharts 258 (+components/historify 712, lib/historify 282) | Diverged (1205 / 1107) | desktop HistorifyCharts is a 925-LOC rewrite on lightweight-charts; web uses openalgo-charts via `lib/chart/feeds/historifyFeed.ts` |
| 404 | `*` | NotFound.tsx 92 | Diverged (2) | |

Desktop-only files (no web counterpart): `api/strategy.ts`, `api/tauri-client.ts`, `config/playgroundEndpoints.ts`, `hooks/useAutoLogout.ts`, `pages/admin/ServerSettings.tsx`, `pages/strategy/{ConfigureSymbols,NewStrategy,StrategyIndex,ViewStrategy}.tsx`, `types/strategy.ts`.

### 1.2 Navigation config (`config/navigation.ts`, 38 changed lines)

Web `navItems` (lines 40-50): Dashboard, Orderbook, Tradebook, Positions, **Trading**, Platforms, Strategies(Boxes icon), Logs, **Tools**. Desktop has Action Center in the navbar instead of Trading, no Tools, label "Strategy" with Code2 icon.
Web `profileMenuItems` (67-92) adds: Action Center, **Agent**, **Master Contract**, **WhatsApp Bot**, **Flow Editor**, **Scalping**, **Leverage**; web `NavItem.external?` flag (line 35-36) and exact-match `isActiveRoute` (101-103) vs desktop `startsWith('/strategy')` special case (77-79). Web Navbar additionally uses `hooks/useProfileMenuItems.ts` (capability filtering, issue #1480) and `components/auth/LogoutConfirmDialog.tsx`; both missing in desktop (`Navbar.tsx` 277 changed lines).

### 1.3 components/ui, hooks, stores, lib, utils, types, contexts

| Area | Web | Desktop | Gap |
|---|---|---|---|
| components/ui | 34 files, 3,185 LOC | 33 files | Missing `empty-state.tsx` (used by Positions.tsx:62, Holdings.tsx:36, OrderBook.tsx:42, agent config). Diverged: `alert.tsx` lacks `warning` variant (web 14-15); `dialog.tsx`/`dropdown-menu.tsx`/`popover.tsx` lack the `container` portal prop (web dialog 43-54, dropdown 28-38, popover 20-30) required by /trading fullscreen (ChartPane.tsx:1200,1239; PlaceOrderDialog.tsx:379; WorkspaceReplayBar.tsx:218); `command.tsx` moved sr-only header |
| components/* | ErrorBoundary.tsx; auth/{BrokerAuthSignOut,LogoutConfirmDialog,TwoFactorEnforcement}; socket/useKeepReconnecting.ts; agent(12), chart(6), flow, historify(1), option-chain(5), playground(4), portfolio(9), scalping(2), strategy-builder(14), trading(49) | only auth/AuthSync, layout/*, socket/SocketProvider, ui/* | every feature component dir missing; `main.tsx` lacks ErrorBoundary + `installGlobalErrorReporter()` |
| hooks | 20 files, 4,050 LOC | 7 files | Missing: useChartWorkspaceCatalog, useLiveQuote, useOptionChainLive/Polling/Preferences, usePageTitle, usePageVisibility, useProfileMenuItems, useStrategyExchanges, useSupportedExchanges, useTrailingSL, useWebSocketTester, useWorkspaceAutosave, useWorkspaceGridTransition. Diverged: useSocket (278 lines; web = socket.io 346 LOC, desktop = Tauri listen 128 LOC), useMarketData (386; desktop talks to Rust websocket_* commands), useLivePrice (137), useOrderEventRefresh (127), useMarketStatus (55) |
| stores | alertStore, authStore, brokerStore, flowWorkflowStore, portfolioBacktestStore, sessionStore, sipBacktestStore, themeStore | authStore (235 LOC desktop rewrite, async login/brokerLogin), themeStore (124 changed) | Missing alertStore (toast position/categories, used by `app/providers.tsx:23`), brokerStore (route guards), sessionStore (active session count) |
| lib | 20 top-level + agent(12) chart(2+2 feeds) flow(3) historify(2) trading(75) = 23,379 LOC trading alone | rateLimiter.ts, utils.ts (21 changed) | everything else missing incl. MarketDataManager.ts 870 LOC, serverSentence.ts, tools.ts, optionGreeks.ts, Plot2D/Plot3D, plotly-2d/3d, strategy*.ts, scalping*.ts |
| utils | chunkReload.ts, errorReporter.ts, toast.ts (showToast wrapper used by all web pages) | none | web pages call `showToast.*` from `@/utils/toast`; desktop pages call `sonner.toast` directly |
| contexts | MarketDataContext.tsx (wired in providers.tsx:5,27) | none | |
| types | 13 files 3,766 LOC | 7 files | Missing flow, option-chain, plotly.d.ts, scalping, strategy_module, websocket, whatsapp; admin.ts 401 vs 75 LOC |
| api | 31 modules, 7,987 LOC (axios `apiClient` /api/v1 + `webClient` + `authClient`, CSRF interceptors client.ts:23-166) | 9 modules; `client.ts` re-exports Tauri wrappers + null `webClient` | Missing 24 api modules |
| test utils | setup.ts (80), test-utils.tsx, a11y-utils.ts, axiosAnswer.ts, fakeChart.tsx (286), marketDataHarness.ts (214), runtimeReports.ts (126) | setup.ts (48, uses vi.fn observers that break Radix), test-utils, a11y-utils | web setup.ts adds class-based Resize/IntersectionObserver mocks and pointer-capture/scrollIntoView stubs (lines 27-67) |

### 1.4 index.css

Lines 1-357 identical (Tailwind v4 `@import`, `@custom-variant dark`, `:root`/`.dark` HSL tokens, `.analyzer` and `.sandbox` OKLch palettes, six `[data-accent=...]` OKLch accent themes, `@theme inline` mapping, sidebar tokens). Desktop ends at 407; web continues to 951 with: `.analyzer [data-slot="alert"]` amber overrides (358-424), Flow editor styles (473-760, excluded), global scrollbar theming + `color-scheme` (762-873), `.no-scrollbar`/`.scrollbar-thin` (838-873), `--oscript-*` syntax tokens + `.oscript-*` classes (875-951, needed by ScriptPanel/openscriptHighlight).

### 1.5 Toolchain and dependency drift (`package.json`)

| Package | Web | Desktop |
|---|---|---|
| react-router | `react-router ^8.3.0` (87 files import `'react-router'`) | `react-router-dom ^7.12.0` (57 files) |
| vite / @vitejs/plugin-react | ^8.0.16 / ^6.0.2 | ^7.2.4 / ^5.1.1 |
| typescript | ^7.0.2 | ~5.9.3 |
| vitest / jsdom | ^4.1.11 / ^27.4.0 | ^4.0.17 / ^24.1.3 |
| tailwindcss | ^4.3.0 | ^4.1.18 |
| openalgo-charts / openalgo-script | 2.6.0 / 0.8.1 | absent |
| @openuidev/react-headless, react-lang, react-ui | ^0.9.13 / ^0.2.15 / ^0.13.10 | absent |
| plotly.js-dist-min, react-plotly.js, @types/plotly.js | present | absent |
| react-markdown, remark-gfm | present | absent |
| @xyflow/react | present (Flow, excluded) | absent |
| axios, socket.io-client | runtime deps | axios absent; socket.io-client only in devDependencies but imported at runtime by ActionCenter.tsx:17 |
| @tauri-apps/api, plugin-shell | absent | ^2.0.0 |
| devDeps playwright, @axe-core/*, jest-axe, vitest-axe, cssnano, svgo | present | absent |

---

## 2. Broken wiring in the desktop frontend

### 2.1 Pages importing the `webClient` null stub (`src/api/client.ts:35-66`; every call resolves `{data:null}` and logs a console.warn)

- `pages/Logs.tsx:18` import; `:65` GET `/logs/?...`
- `pages/Profile.tsx:28` import; `:222` GET profile-data, `:250` broker credentials, `:268` permissions, `:285,338,420,492,530,561,583` POSTs (change password, 2FA, SMTP, permissions fix, credentials)
- `pages/ActionCenter.tsx:19` import; `:107` GET data, `:194,219,265` approve/reject/approve-all, `:243` DELETE
- `pages/telegram/TelegramAnalytics.tsx:5,33`; `TelegramUsers.tsx:5,77,102,130`; `TelegramIndex.tsx:5,74,89,109,129,157`; `TelegramConfig.tsx:5,38,69`
- `pages/monitoring/SecurityDashboard.tsx:5,139,166,176,210,246,277,305,330`; `LatencyDashboard.tsx:5,87,88`; `TrafficDashboard.tsx:5,77,78`
- `fetchCSRFToken` stub (`client.ts:72-75`, returns `''`) imported by `pages/ResetPassword.tsx:15` and `pages/ServerError.tsx:5`; local re-implementations that `fetch('/auth/csrf-token')` exist in `hooks/useMarketStatus.ts:31`, `pages/PnLTracker.tsx:8`, `pages/Historify.tsx:195`.

### 2.2 Raw `fetch()` to Flask-relative URLs (no HTTP origin inside `tauri://localhost`)

`components/layout/Footer.tsx:17` `/auth/app-info`; `hooks/useMarketStatus.ts:32,56,57`; `pages/Token.tsx:89,90,127,165`; `pages/HistorifyCharts.tsx:483,506`; `pages/GoCharting.tsx:69,104`; `pages/Search.tsx:85`; `pages/TradingView.tsx:71,106`; `pages/ResetPassword.tsx:132,162,197,231`; `pages/ServerError.tsx:16`; `pages/PnLTracker.tsx:213`; `pages/Historify.tsx:521-1023` (28 calls: watchlist, catalog, intervals, stats, exchanges, jobs CRUD/pause/resume/cancel/retry, fno/*, delete, upload, export/bulk); `pages/chartink/ChartinkIndex.tsx:51`, `ViewChartinkStrategy.tsx:76`, `pages/strategy/StrategyIndex.tsx:53`, `ViewStrategy.tsx:82` (`/api/config/host`).

### 2.3 Socket.IO vs Tauri events

- `pages/ActionCenter.tsx:17` imports `io` and `:152` opens `io(protocol//host:port)`; there is no Socket.IO server in the desktop; `socket.io-client` is a devDependency only.
- Desktop hooks were rewritten on `@tauri-apps/api/event`: `hooks/useSocket.ts:8,92,108`, `hooks/useOrderEventRefresh.ts:8,65`, `hooks/useMarketData.ts:9,236-263`, `hooks/useAutoLogout.ts:16,76,95`, `pages/BrokerSelect.tsx:2,147`, `pages/WebSocketTest.tsx:26,635-650`.
- Events listened for that Rust never emits (Rust emits only: api_order, api_smart_order, api_basket_order, api_split_order, api_modify_order, api_cancel_order, api_cancel_all_orders, api_close_position, auto_logout, auto_logout_warning, market_tick, oauth_callback, webhook_alert, websocket_disconnected, websocket_error): `order_event` (`useSocket.ts:92`; `useOrderEventRefresh.ts:43` default; `Positions.tsx:230`; `Holdings.tsx:114`), `analyzer_update` and `close_position_event` (`useOrderEventRefresh.ts:43`, `Positions.tsx:230`), `notification` (`useSocket.ts:108`), `websocket_connected` (`useMarketData.ts:243`). Consequence: order-book/position auto-refresh and alert sounds never fire in desktop.
- Web equivalents the port must map: `components/socket/SocketProvider.tsx` exposes `socket` (web lines 6-11) consumed by `hooks/useOrderEventRefresh.ts:3,92-145`, `components/trading/dock/useBlotter.ts:83,182,201` (`order_update`, `connect`), `api/strategy_module.ts:897-1016` (strategy_* room events), `hooks/useSocket.ts:164-264` (force_logout, password_change, master_contract_download, cancel/modify/close events, order_event, pending_order_*, active_sessions_update, analyzer_update).

### 2.4 Tauri commands

Every command string in `src/api/tauri-client.ts` and every direct `invoke('...')` in pages (BrokerSelect.tsx:119-388, BrokerTOTP.tsx:353-414, Dashboard.tsx:100-157, Login.tsx:61-140, Setup.tsx:78,127, ApiKey.tsx:67,96, Playground.tsx:205, admin/ServerSettings.tsx:35,52, hooks/useMarketData.ts:155-306) is present in `src-tauri/src/lib.rs:76-185`. No missing commands. Unused-by-frontend Rust commands: order_logs::* (lib.rs:160-164), market::* holidays/timings (166-174) although `api/admin.ts` stubs them, `get_quote`/`get_market_depth`/`search_symbols`/`get_symbol_info` (only via tauri-client wrappers).

### 2.5 Router and transport

- Router: desktop `App.tsx:2` `react-router-dom`; web `App.tsx:2` `react-router` v8 with `Navigate` guards (`HoldingsRoute` 122-128, `LeverageRoute` 113-119) and `PageTitleUpdater` (156-159). Copying any web file needs a `react-router-dom` -> `react-router` alias in `vite.config.ts`/`vitest.config.ts` or a bulk specifier rewrite plus upgrade to v8.
- axios-shaped error handling survives in desktop while transport is `invoke` (errors are `{code,message}`): `pages/OrderBook.tsx:177-178,196-197,252-253`, `pages/python-strategy/PythonStrategyIndex.tsx:123-124`.
- Desktop `api/trading.ts` (501 LOC) re-shapes Tauri structs to the web `tradingApi` surface; web `api/trading.ts` (343 LOC) posts `/api/v1/*` with `apikey` in the body via `apiClient` and `/modify_order`, `/cancel_order`, `/close_position`, `/close_all_positions`, `/cancel_all_orders`, `/modify_gtt_order`, `/cancel_gtt_order` via session `webClient` (blueprints/orders.py:504-947).

---

## 3. Full surface inventory (in-scope)

Columns: page files (own-closure LOC = page + feature-specific components/lib/api, excluding shared ui/layout/hooks); backend; sockets; third-party npm; Python-runtime verdict (what the Flask side uses; "Rust-portable" = plain HTTP/DB/arithmetic with no Python-only dependency). All Flask blueprints are in `openalgo/blueprints/`, REST namespaces in `openalgo/restx_api/` mounted at `/api/v1` (restx_api/__init__.py:64-111). Socket.IO event emitters are in `openalgo/subscribers/socketio_subscriber.py` (order_event, order_notification, modify/cancel/close_position_event, order_update, analyzer_update) unless noted.

### 3.1 Core trading and account

| Surface | Routes | Files / LOC | Backend endpoints (blueprint) | Sockets | npm | Python? |
|---|---|---|---|---|---|---|
| Setup / Login / Reset / 2FA | `/setup` `/login` `/reset-password` | Setup 316, Login 417, ResetPassword 527, components/auth/TwoFactorEnforcement | `/auth/setup`, `/auth/login`, `/auth/login/totp`, `/auth/session-status`, `/auth/check-setup`, `/auth/csrf-token`, `/auth/reset-password`, `/auth/change-password`, `/auth/2fa/*`, `/auth/smtp-config`, `/auth/test-smtp` (auth.py, 23 routes) | force_logout, password_change | sonner, zustand | Rust-portable (already partly in commands::auth) |
| Broker select / TOTP / Samco | `/broker` `/broker/:b/totp` `/:b/auth` `/broker/samco/auth` | 353 + 555 + 223, api/auth.ts 104 | `/auth/brokers`, `/auth/broker-config`, `/auth/broker/<b>`, `/<broker>/callback` (brlogin.py 5 routes), `/samco/callback`, `/samco/ip-status`, `/api/broker/credentials` (broker_credentials.py), `/api/broker/capabilities` (core.py) | master_contract_download | axios | Rust-portable; desktop already has `src-tauri/src/brokers` + keychain |
| Dashboard | `/dashboard` | 505 | `/auth/dashboard-data` (dashboard.py), `/api/master-contract/status` (master_contract_status.py:33) | order_event etc., active_sessions_update | | Rust-portable |
| Orderbook / Tradebook / Positions / Holdings | `/orderbook` `/tradebook` `/positions` `/holdings` | 972 / 593 / 1079 / 501, api/trading.ts 343, hooks/useLivePrice 354, MarketDataManager 870 | `/api/v1/{orderbook,tradebook,positionbook,holdings,funds,multiquotes,quotes,depth,placeorder,basketorder,gttorderbook}`; `/modify_order`, `/cancel_order`, `/close_position`, `/close_all_positions`, `/cancel_all_orders`, `/modify_gtt_order`, `/cancel_gtt_order` (orders.py:504-947); `/orderbook/export`, `/tradebook/export`, `/positions/export` (orders.py:323-444); `/admin/api/holidays`, `/admin/api/timings` (useMarketStatus) | order_event, order_notification, modify/cancel/close_position_event, analyzer_update; raw WS ws://127.0.0.1:8765 (websocket_proxy/server.py:42) | axios, socket.io-client | Rust-portable (commands::orders/positions/holdings exist; GTT, export, live WS feed proxy missing) |
| Search / Token | `/search` `/search/token` | 585 / 788 | `/search/api/search`, `/search/api/expiries`, `/search/api/underlyings` (search.py:133-248) | | | Rust-portable (commands::symbols) |
| API key | `/apikey` | 392 | `/apikey` GET/POST, `/apikey/mode` (apikey.py) | | | Rust-portable (commands::api_keys) |
| Profile | `/profile` | 2392 | `/auth/profile-data`, `/auth/change-password`, `/auth/2fa/{status,configure}`, `/auth/smtp-config`, `/auth/test-smtp`, `/auth/debug-smtp`, `/api/broker/credentials`, `/api/system/permissions`, `/api/system/permissions/fix` (system_permissions.py), `/auth/analyzer-mode` | | axios | Rust-portable; SMTP needs lettre; system permissions = filesystem checks |
| Action center | `/action-center` | 856 | `/action-center/api/data`, `/action-center/approve/<id>`, `/reject/<id>`, `/delete/<id>`, `/count`, `/approve-all` (orders.py:947-1161) | pending_order_created (services/order_router_service.py:124), pending_order_updated (orders.py:1063-1219) | socket.io-client | Rust-portable (order router) |
| Platforms / TradingView / GoCharting webhooks | `/platforms` `/tradingview` `/gocharting` | 93 / 465 / 449 | `/api/config/host` (core.py), `/search/api/search`; webhook receivers `/tradingview` (tv_json.py), `/gocharting` (gc_json.py), `/api/v1/placeorder`, `/placesmartorder` | | codemirror json | Rust-portable (webhook axum server already exposes `/api/v1/*`, server.rs:120-126) |
| PnL tracker | `/pnl-tracker` | 615 | `/pnltracker/api/pnl` (pnltracker.py, 3 routes; uses pandas+numpy for intraday MTM curve) | | lightweight-charts, html2canvas-pro | Rust-portable (time-series arithmetic) |
| Sandbox / Analyzer | `/sandbox` `/sandbox/mypnl` `/analyzer` `/logs/sandbox` | 512 / 558 / 419 | `/sandbox/api/configs`, `/sandbox/update`, `/sandbox/reset`, `/sandbox/mypnl/api/data`, `/sandbox/mypnl/export/<type>` (sandbox.py 12 routes); `/analyzer/api/data`, `/analyzer/export` (analyzer.py 6); `/auth/analyzer-mode`, `/auth/analyzer-toggle` | analyzer_update (services/orderstatus_service.py:27, openposition_service.py:26) | codemirror json | Rust-portable (commands::sandbox exist; Python engine `sandbox/*.py` uses APScheduler for squareoff) |
| WebSocket test / order | `/websocket/test(/20|30|50)` `/websocket/order` | 1455 / 694, hooks/useWebSocketTester | `/api/websocket/apikey`, `/api/websocket/config`, `/api/websocket/{status,subscriptions,subscribe,unsubscribe}` (websocket_example.py:61-186); raw WS 8765 | ws messages: authenticate, subscribe/unsubscribe, market_data; Socket.IO namespace `/market` (websocket_example.py:323-423) | | Rust-portable (websocket manager exists; needs a local WS server or Tauri events) |
| Playground | `/playground` | 1200 + components/playground 965 | `/playground/api-key`, `/playground/collections`, `/playground/endpoints` (playground.py 4 routes); any `/api/v1/*` | raw WS 8765 | codemirror | Rust-portable |
| Logs / monitoring | `/logs` `/logs/live` `/logs/security` `/logs/traffic` `/logs/latency` | 111 / 470 / 1491 / 339 / 625 | `/logs/`, `/logs/export` (log.py); `/security/api/{data,active-sessions,login-activity}`, `/security/{ban,ban-host,unban,clear-404,settings,stats}` (security.py 11); `/traffic/api/{logs,stats}`, `/traffic/export` (traffic.py); `/latency/api/{logs,stats}`, `/latency/export` (latency.py) | active_sessions_update | axios | Rust-portable (order_logs commands exist; traffic/latency/security tables missing) |
| Health monitor | `/health` | 893 + api/health.ts 229 | `/health/check`, `/health/api/{current,history,stats,alerts}`, `/health/api/alerts/<id>/{acknowledge,resolve}`, `/health/export` (health.py 9; psutil) | | lightweight-charts | Rust-portable (sysinfo crate) |
| Master contract | `/master-contract` | 548 | `/api/master-contract/{status,ready,download,smart-status}`, `/api/cache/{status,health,reload,clear}` (master_contract_status.py:33-233) | master_contract_download | | Rust-portable (refresh_symbol_master exists) |
| Leverage | `/leverage` | 152 | `/leverage/api/current`, `/leverage/api/update` (leverage.py) | | | Rust-portable |
| Admin | `/admin` `/admin/freeze` `/admin/holidays` `/admin/timings` `/admin/diagnostics` `/admin/remote-mcp` | 199/522/621/381/741/864 + webServerSummary 188, api/admin.ts 282, types/admin.ts 401 | `/admin/api/{stats,system,system/diagnostics,system/report,freeze,freeze/<id>,freeze/upload,holidays,holidays/<id>,timings,timings/<ex>,timings/check,errors,errors/groups,errors/stats,errors/client,oauth/clients,oauth/clients/<id>/approve|revoke,mcp/audit,mcp/kill-switch,mcp/settings}` (admin.py 38 routes) | | axios | Rust-portable except Remote MCP, which fronts the Python MCP server (`blueprints/mcp_http.py`, `mcp_oauth.py`, `mcp/` using `mcp.server.fastmcp`) - an MCP server would need a Rust reimplementation |
| Telegram | `/telegram/*` | 1,493 + api/telegram.ts 136, types/telegram.ts | `/telegram/api/{index,config,users,analytics}`, `/telegram/bot/{start,stop}`, `/telegram/broadcast`, `/telegram/test-message`, `/telegram/send-message`, `/telegram/user/<id>/unlink` (telegram.py 15) | | axios | Needs Python today: `python-telegram-bot==22.8` long-polling bot (services/telegram_bot_service.py, uses pandas for charts). Portable to Rust (teloxide) but it is a bot runtime, not CRUD |
| WhatsApp | `/whatsapp` | 356 + api/whatsapp.ts | `/whatsapp/{config,users,stats,pair,pair/status,send,test-message,broadcast,unlink,user/<id>/unlink,bot/start,bot/stop,bot/status}` (whatsapp.py 14) | whatsapp_qr, whatsapp_paired, whatsapp_pair_code, whatsapp_pair_status, whatsapp_status (services/whatsapp_bot_service.py:602,725) | socket.io-client | Python-only bot process today (threading/queue/tempfile, no external lib found); pairing/QR flow would need a Rust WhatsApp client |
| Agent | `/agent` `/agent/config` | 82 + 561 + 167, components/agent 3,340, lib/agent 3,592, api/agent.ts 1,370 | `/agent/api/{status,catalog/providers,catalog/models,models,models/<id>,models/<id>/test,models/<id>/default,settings,websearch,websearch/providers/<p>/key|test,chatgpt/*,voice/*,conversations,conversations/<id>,chat/stream,chat/confirm,chat/<run>/cancel}` (agent.py 36 routes) | fetch-streamed POST `/agent/api/chat/stream` and `/chat/confirm` (lib/agent/stream.ts:230-360), raw WS 8765 for InstrumentCard | @openuidev/react-headless+react-lang+react-ui, react-markdown, remark-gfm, react-syntax-highlighter, plotly.js, react-plotly.js, openalgo-charts | Needs Python today: `litellm==1.99.0` provider router, `mcp`, `openalgo.ta` indicators (services/agent/indicators/compute.py:37), pandas/numpy tool outputs. A Rust port means direct provider HTTP (OpenAI/Anthropic/Gemini) + re-implementing ~20 agent tools |
| Historify | `/historify` `/historify/charts(/:symbol)` | 3535 + 258, components/historify/HistorifyChartPane 712, lib/historify 282, lib/chart/feeds/historifyFeed 156, api/historify.ts | 50 routes in historify.py: `/historify/api/{watchlist,watchlist/bulk,catalog,catalog/metadata,data,intervals,historify-intervals,stats,exchanges,jobs,jobs/<id>,jobs/<id>/{pause,resume,cancel,retry},fno/{underlyings,expiries,chain},delete,delete/bulk,upload,export/bulk,export/bulk/download,sample/csv,sample/parq,schedules...}` | historify_progress, historify_job_complete, historify_job_paused, historify_job_cancelled (services/historify_service.py:1797-1861), historify_schedule_* (historify_scheduler_service.py:294-627) | openalgo-charts, openalgo-script (chart pane), axios, socket.io-client | Rust-portable with effort: pandas (resample/export csv/parquet) + APScheduler + duckdb (`duckdb==1.5.5` in requirements); desktop already has `commands::historify::{get_market_data,download_historical_data}` |
| Chartink | `/chartink/*` | 1,514 + api/chartink.ts 128 | `/chartink/api/strategies`, `/chartink/api/strategy`, `/chartink/api/strategy/<id>`, `/chartink/api/strategy/<id>/toggle`, `/chartink/<id>/configure`, `/chartink/<id>/delete`, `/chartink/<id>/symbol/<m>/delete`, `/chartink/webhook/<id>` (chartink.py 13; APScheduler squareoff) | | axios, cmdk | Rust-portable (webhook server already parses Chartink payloads, webhook/handlers.rs:82) |
| Strategy module (RMS) | `/strategy` `/strategy/new` `/strategy/:id` `/strategy/:id/edit` | List 255, Wizard 2051, Detail 3050, Edit 66, api/strategy_module.ts ~1,100, types/strategy_module.ts, lib/strategyContracts/strategyMath/strategyTemplates/templateResolution | `/strategy/api/strategies`, `/strategies/<id>`, `/<id>/{start,stop,close_all,kill_switch,unlock_webhook,runs,orders,checkpoints,...}` (strategy_module.py 22 routes), `/api/v1/{symbol,optionsymbol,optionchain,expiry}` | strategy_subscribe/unsubscribe (strategy_module.py:1917,1946); strategy_snapshot, strategy_delta, strategy_event, strategy_order_update, strategy_run_update, strategy_terminal (services/strategy_module/broadcast.py:122-127) | @tanstack/react-query, socket.io-client | Rust-portable: engine is pure Python (`services/strategy_module/{engine,runtime,risk_adapter,tick_feed,order_dispatch,...}.py`, APScheduler only); no pandas/subprocess. Substantial port (~20 modules) |
| Scalping | `/scalping` | 1873 + components/scalping 685, api/scalping.ts, lib/scalping{Price,Rows,Tick}.ts, hooks/useTrailingSL 214 | `/scalping/api/{underlyings,all_underlyings,expiry,search,futures,strikes,history,order,close_leg,close_all,cancel_all,tracked,sl}` (scalping.py 16) | scalping_sl_update (services/scalping_risk_monitor_service.py:630); raw WS 8765 | lightweight-charts, @tanstack/react-query | Rust-portable (risk monitor is a thread over quotes) |
| Options analytics tools (15 of 18 in `lib/tools.ts`) | `/optionchain` `/ivchart` `/oitracker` `/oirange` `/maxpain` `/straddle` `/straddlepnl` `/volsurface` `/gex` `/gammadensity` `/ivsmile` `/oiprofile` `/arbitrage` `/strategybuilder` `/strategybuilder/portfolio` | OptionChain 1106 (+4,620 closure), IVChart 658, OITracker 478, OIRange 758, MaxPain 451, StraddleChart 825, CustomStraddle 989, VolSurface 438, GEX 806, GammaDensity 600, IVSmile 565, OIProfile 621, Arbitrage 765, StrategyBuilder 2092 (+14,953 closure), StrategyPortfolio 844; lib/optionGreeks.ts, Plot2D/3D, plotly-2d/3d | `/api/v1/{optionchain,expiry,optiongreeks,multioptiongreeks,syntheticfuture,margin,multiquotes}`; `/ivchart/api/{default-symbols,intervals,iv-data}`; `/oitracker/api/{oi-data,maxpain}`; `/oiprofile/api/{intervals,profile-data}`; `/straddle/api/{intervals,straddle-data}`; `/straddlepnl/api/{intervals,lotsize,simulate}`; `/volsurface/api/surface-data`; `/gex/api/gex-data`; `/gammadensity/api/gamma-data`; `/ivsmile/api/iv-smile-data`; `/arbitrage/api/universe`; `/strategybuilder/api/{strategy-chart,multi-strike-oi,intervals}` (strategy_chart.py); `/api/strategy-portfolio[/<id>]` (strategy_portfolio.py 5) | raw WS 8765 (option chain live, strategy builder) | plotly.js-dist-min, react-plotly.js, lightweight-charts, cmdk, openalgo-charts/openalgo-script (strategy builder chart) | Rust-portable: Greeks are hand-written Black-Scholes in `services/option_greeks_service.py` (no py_vollib, no scipy); iv_chart/straddle/custom_straddle/strategy_chart/multi_strike_oi use pandas only for resampling (`import pandas`), all others import nothing heavy |
| Trading terminal | `/trading` | see section 4 | see section 4 | see section 4 | openalgo-charts 2.6.0, openalgo-script 0.8.1, cmdk, @tanstack/react-query, plus agent deps for AgentPanel | Mostly Rust-portable; live OpenScript runner needs Python (`openscript==0.8.1`) |
| Misc public | `/` `/faq` `/download` `/error` `/rate-limited` `*` | 449 / 393 / 165 / 133 / 125 / 92 | `/auth/app-info` | | | Rust-portable |

### 3.2 Shared infrastructure every surface needs

`api/client.ts` (axios + CSRF), `components/socket/SocketProvider.tsx` + `hooks/useSocket.ts` (socket.io), `lib/MarketDataManager.ts` (raw WS to `websocket_proxy` on 8765 with `/api/websocket/config` + `/api/websocket/apikey` auth, REST fallback to `/api/v1/multiquotes` MarketDataManager.ts:371-425,797), `contexts/MarketDataContext.tsx`, `hooks/useMarketStatus.ts` (`/admin/api/holidays`, `/admin/api/timings`), `stores/{alertStore,brokerStore,sessionStore}`, `utils/{toast,errorReporter,chunkReload}`, `components/ErrorBoundary.tsx`, `hooks/usePageTitle.ts`, `hooks/useProfileMenuItems.ts`, `components/layout/Navbar.tsx` (391 LOC).

### 3.3 Backend Socket.IO event catalogue (web consumes 34 names; `hooks/useSocket.ts:164-264`)

Order lifecycle: order_event, order_notification, modify_order_event, cancel_order_event, close_position_event, order_update (`subscribers/socketio_subscriber.py:21-248`; order_update also from `websocket_proxy/server.py:1874`). Session: force_logout, password_change, active_sessions_update. Analyzer: analyzer_update. Master contract: master_contract_download, cache_loaded. Action center: pending_order_created, pending_order_updated. Historify: 10 events. Strategy module: 6 room events + 2 client emits. WhatsApp: 5. Scalping: scalping_sl_update. Market namespace `/market`: subscribe/unsubscribe/get_ltp/get_quote/get_depth -> ltp_data/quote_data/depth_data/subscription_success|error.

### 3.4 EXCLUDED (per coordinator) - brief

| Surface | Routes | Files / LOC | Why excluded |
|---|---|---|---|
| Python Strategy Host | `/python`, `/python/new`, `/python/:id/{edit,logs,schedule}`, `/python/guide` | 7 pages 3,445 LOC + api/python-strategy.ts 226, types 134 | Runs user Python files as subprocesses (`blueprints/python_strategy.py:16,67,586-604`, 3,959 LOC, APScheduler); requires a Python interpreter on the user's machine |
| Flow | `/flow`, `/flow/shortcuts`, `/flow/editor/:id` | 3 pages 1,934 LOC + components/flow (63 nodes) + lib/flow 2,183 + api/flow.ts + stores/flowWorkflowStore + 288 LOC of index.css | Python executor (`services/flow_executor_service.py`, `flow_price_monitor_service.py`, `flow_scheduler_service.py`), `@xyflow/react` |
| Portfolio Backtester (+results) | `/portfolio-backtester`, `/portfolio-backtester/results`, `/portfolio` | 424 + 1564, components/portfolio 1,361, stores/portfolioBacktestStore, api/portfolio.ts | `/api/v1/portfolio/{backtest,benchmarks,tearsheet,holdings}` -> `services/portfolio_service.py` (pandas) + `portfolio/{engine,analytics,rebalance,walkforward,crisis,...}.py` (pandas/numpy) |
| SIP Backtester (+results) | `/sip-backtester`, `/sip-backtester/results` | 347 + 709, stores/sipBacktestStore, api/sip.ts | `/api/v1/sip/{backtest,frequencies}` -> `services/sip_service.py` + `sip/{engine,analytics,crisis,xirr}.py` (pandas) |
| Portfolio Analyzer | `/portfolio-analyzer` | 529 | `/api/v1/portfolio/tearsheet`, `/portfolio/holdings` -> same pandas `portfolio_service.py` |

The other 15 tools in `lib/tools.ts` (Strategy Builder, Strategy Portfolio, Option Chain, Option Greeks, OI Tracker, OI Range, Max Pain, Straddle Chart, Straddle PnL, Vol Surface, GEX, Gamma Density, IV Smile, OI Profile, Arbitrage) stay in scope; none is a backtester (CustomStraddle `/straddlepnl/api/simulate` is a historical straddle PnL replay over `/history` bars using pandas resample, not a portfolio engine).

---

## 4. /trading charting terminal

### 4.1 npm dependencies actually imported by the /trading closure

Measured over `pages/Trading.tsx`, `components/trading/**`, `lib/trading/**`, `lib/chart/**`, `components/chart/**`, the hooks and api modules it imports:

| Package | Import count (static) | Where | Needed |
|---|---|---|---|
| `openalgo-charts` 2.6.0 (+ subpaths `/workspace`, `/draw`, `/widget`, `/transform`, `/profile`, `/indicators` dynamic) | 78 + 22 + 20 + 8 + 4 + 1; 30 dynamic `import()` in `lib/trading/terminal.ts` (3159-7682), `customIndicators.ts:353-366`, `chartHistory.ts:79`, `openscriptStudies.ts:597`, `components/chart/OpenAlgoChart.tsx:304`, `BacktestChart.tsx:175` | everywhere | yes |
| `openalgo-script` 0.8.1 (+ `/adapters/charts`, `/editor`) | 3 static, dynamic in `backtestFold.ts:70`, `openscriptFiles.ts:166,401`, `openscriptHighlight.ts:104,119`, `openscriptStudies.ts:115,598` | ScriptPanel, BacktestPanel, StrategiesPanel | yes |
| `@tanstack/react-query` | 3 | useBlotter, WatchlistPanel, OptionChainPanel | yes (desktop has it) |
| `zustand` (+ `/middleware`) | 4 | stores | yes (has) |
| `lucide-react`, `react`, `react-dom` | | | has |
| `cmdk` | via `components/ui/command` in SymbolSearchDialog | | has |
| `socket.io-client` | via `SocketProvider` in `dock/useBlotter.ts:14,83` | | replace with Tauri events |
| `axios` | via `api/client.ts` (`apiClient`/`webClient`) | api/trading, watchlist, openscriptRunner, agent, previousClose, backtestRun | replace or add |
| `@openuidev/react-lang`, `@openuidev/react-ui`, `react-markdown`, `remark-gfm`, `react-syntax-highlighter`, `plotly.js-dist-min`, `react-plotly.js` | transitively via `components/trading/AgentPanel.tsx:40-50` -> `components/agent/{Message,Composer,ModelPicker,AgentSetupGate}`, `components/agent/viz/*` | AgentPanel only | only if the AgentPanel ships; omit by stubbing `AgentPanel` to drop 7 packages |
| NOT used by /trading | `lightweight-charts` (other pages), `@xyflow/react`, `html2canvas-pro`, `@codemirror/*` | | |

Vendor dir `frontend/vendor/` holds four stale `openalgo-charts-2.4.0-*.tgz`; `vendor/README.md` says they are no longer referenced; `package-lock.json:11882-11885` resolves 2.6.0 from the npm registry.

### 4.2 Backend endpoints and socket events used by /trading

Transport conventions: `apiClient` = axios baseURL `/api/v1`, JSON, `withCredentials` (`api/client.ts:23-29`); `webClient` = axios baseURL `/`, session cookie + `X-CSRFToken` from `/auth/csrf-token` (`client.ts:104-136`); `terminal.ts.api()` = raw `fetch('/api/v1/'+path)` POST with `{apikey, ...body}` (`lib/trading/terminal.ts:1647-1660`), apikey obtained once from `/api/websocket/apikey` (`pages/Trading.tsx:872`).

| Call | Caller (file:line) | Server | Response shape |
|---|---|---|---|
| GET `/api/websocket/apikey`, GET `/api/websocket/config` | `pages/Trading.tsx:872-873`; `lib/MarketDataManager.ts:371,411` | `blueprints/websocket_example.py:165,186` | `{status:'success', api_key}` / `{websocket_url: 'ws://127.0.0.1:8765', ...}` |
| WS `ws://127.0.0.1:8765` -> `{action:'authenticate', api_key}`, `{action:'subscribe', symbols:[{symbol,exchange}], mode}` | `lib/MarketDataManager.ts:389,425,641`; `hooks/useMarketData.ts`, `useLivePrice.ts`, `useLiveQuote.ts` | `websocket_proxy/server.py:42` (port from `WEBSOCKET_PORT`, app_integration.py:368) | tick frames `{type:'market_data', symbol, exchange, mode, data:{ltp, ...}}`; REST fallback POST `/api/v1/multiquotes` (`MarketDataManager.ts:797`) |
| POST `/api/v1/history` `{apikey, symbol, exchange, interval, start_date, end_date}` | `lib/trading/terminal.ts` via `api('history')`, `lib/trading/previousClose.ts:176`, `lib/trading/backtestRun.ts:371`, `lib/chart/feeds/historifyFeed.ts` | `restx_api/history.py` -> `services/history_service.py:163` | `{status:'success', data:[{timestamp, open, high, low, close, volume, oi?}]}` |
| POST `/api/v1/search`, `/api/v1/symbol`, `/api/v1/intervals` | `terminal.ts:1664` (`api('search')`), `backtestRun.ts:217` | `restx_api/search.py`, `symbol.py`, `intervals.py` | `{status, data:[SearchRow]}` / `{status, data:{symbol, token, lotsize, tick_size, ...}}` |
| POST `/api/v1/quotes`, `/multiquotes`, `/depth`, `/funds`, `/positionbook`, `/orderbook`, `/tradebook`, `/holdings`, `/placeorder`, `/basketorder`, `/gttorderbook` | `api/trading.ts:100-296` (tradingApi) used by `PlaceOrderDialog.tsx:6`, `ExecuteBasketDialog.tsx:3`, `GttTab.tsx:3`, `dock/useBlotter.ts:13`, `dock/orderActions.ts:17` | restx namespaces (`restx_api/__init__.py:64-110`), services `quotes_service.py:144` `{status,data}`, `depth_service.py:114`, `orderbook_service.py:174` `{status, data:{orders, statistics}}`, `basket_order_service.py:376` `{status:'success', results:[...]}` (analyze mode adds `mode:'analyze'`) | as listed |
| POST `/modify_order`, `/cancel_order`, `/close_position`, `/close_all_positions`, `/cancel_all_orders`, `/modify_gtt_order`, `/cancel_gtt_order` (session + CSRF) | `api/trading.ts:239-337` | `blueprints/orders.py:890,752,504,650,696,795,853` | `{status, message, orderid|trigger_id}` |
| POST `/api/v1/optionchain`, `/api/v1/expiry` | `api/option-chain.ts:19,36` via `hooks/useOptionChainLive.ts` in `OptionChainPanel.tsx:27`; `scalpingApi` (`OptionChainPanel.tsx:28`) for `/scalping/api/{underlyings,expiry,strikes}` | `restx_api/option_chain.py:84` -> `services/option_chain_service.py:730` | `{status, data:{underlying, atm_strike, chain:[{strike, ce:{...}, pe:{...}}]}}` |
| POST `/api/v1/market/holidays` | `lib/trading/sessionHours.ts:5,39,159` | `restx_api/market_holidays.py` | `{status, data:[...]}` |
| GET/POST/PATCH/PUT/DELETE `/watchlist/api/lists[/<id>[/clear|/items[/<item>|/order]]]` | `api/watchlist.ts:37-78` from `WatchlistPanel.tsx:25` | `blueprints/watchlist.py:65-175` | `{status:'success', data:[{id,name,items:[{id,symbol,exchange}]}]}`; errors `{status:'error', message}` 400/404/409 |
| POST `/alerts/fired`, GET `/alerts/log`, DELETE `/alerts/log` | `lib/trading/alertLog.ts:75,89,105` (Trading.tsx:84) | `blueprints/alerts.py:52-101` | `{status, fire}` / `{status, fires:[...]}` / `{status, removed}` |
| POST `/api/v1/<channel>/notify` (telegram/whatsapp) | `lib/trading/alertDelivery.ts:158,222` | `restx_api/telegram_bot.py`, `whatsapp_bot.py` | `{status, message}` |
| GET `/custom-indicators/index.json`, GET `/custom-indicators/<file>?v=<mtime>` (ES module import) | `lib/trading/customIndicators.ts:45,298,332,464` | `blueprints/custom_indicators.py:47,78` serving `strategies/indicators/*.js` | `[{file, name, mtime, ...}]`; JS module |
| GET `/openscript/index.json`, GET/POST/DELETE `/openscript/<file>`, GET `/openscript/program/<file>`, GET `/openscript/instrument?symbol&exchange` | `lib/trading/openscriptFiles.ts:99-239`, `openscriptStudies.ts:66,604`, `instrumentFacts.ts:67,232` | `blueprints/openscript.py:235-601` serving `strategies/openscript/` | index `[{file, name, hash, ...}]`; source text; `{status:'success', symbol, contractFound, instrument, ...}` |
| `/openscript/runner/*`: GET overview, POST `start/<file>`, `pause|stop/<name>`, `close/<name>`, POST/DELETE `config/<file>`, POST/DELETE `schedule/<file>`, GET instruments/intervals, GET books `orderbook|tradebook|positions/<file>` | `api/openscriptRunner.ts:131-423` from `StrategiesPanel.tsx:49`, `StrategyBooks.tsx` | `blueprints/openscript_runner.py:696-...` (17 routes) -> `services/openscript_runner_service.py` spawns `openscript_host/openscript_runner.py` (Python, imports pip `openscript==0.8.1`) | `{status, runs:[RunningStrategy], settings, schedules}`; `{status, run}` |
| POST `/strategybuilder/api/strategy-chart`, `/multi-strike-oi`, GET `/intervals` | `api/strategy-chart.ts:112-131` via `lib/chart/feeds/strategyFeed.ts` | `blueprints/strategy_chart.py:32-185` | `{status, data:{points:[...]}}` |
| GET `/historify/api/catalog`, `/catalog/metadata`, `/data` | `api/historify.ts:105-131` via `lib/chart/feeds/historifyFeed.ts` | `blueprints/historify.py` | `{status, data}` |
| POST `/agent/api/chat/stream`, `/chat/confirm` (fetch + ReadableStream, not EventSource) and `webClient` GETs `/agent/api/{status,models,settings,...}` | `lib/agent/stream.ts:40,230,360`; `api/agent.ts:785-1268` from `AgentPanel.tsx:39-50` | `blueprints/agent.py:2549,2700` | NDJSON frames incl. `AgentChartCommand` for the terminal (`stream.ts:156`) |
| Socket.IO `order_update`, `connect` on the shared socket | `components/trading/dock/useBlotter.ts:182,201`; `hooks/useOrderEventRefresh.ts:92` for order_event/analyzer_update | `subscribers/socketio_subscriber.py:21-273`, `websocket_proxy/server.py:1874` | `{orderid, status, symbol, ...}` |
| Browser storage | `hooks/useChartWorkspaceCatalog.ts:34-35` IndexedDB via `createIndexedDbWorkspaceStorage` (openalgo-charts/workspace); `localStorage` grid weights (Trading.tsx:898) | none (no server workspace sync used; `restx_api/chart_api.py` "Cloud Workspace Sync" is not called by the frontend) | |

### 4.3 The `"openalgo": "file:.."` dependency

`frontend/package.json` lists `"openalgo": "file:.."`; `package-lock.json:11878-11881` records `node_modules/openalgo` as `{"resolved": "..", "link": true}`. The repo root has no `package.json` (only `pyproject.toml`), `frontend/node_modules/openalgo` does not exist, and no file under `frontend/src` imports `'openalgo'` or `'openalgo/...'` (grep: 0 hits). It is a dangling symlink dependency and can be dropped. It is not the Python SDK (`openalgo==2.0.5`, requirements.txt:78) and not the `vendor/` tarballs.

### 4.4 Files to copy and LOC (non-test)

| Group | Files | LOC |
|---|---|---|
| `pages/Trading.tsx` | 1 | 1,800 |
| `components/trading/**` (incl. `dock/`) | 49 | 19,432 (largest: ChartPane 2332, ScriptPanel 1357, WatchlistPanel 1096, StrategiesPanel 947, BacktestPanel 915, OptionChainPanel 788, GttTab 674, IndicatorSettingsDialog 661, PlaceOrderDialog 640, DrawingRail 621, SymbolSearchDialog 608) |
| `lib/trading/**` | 75 | 23,379 (terminal.ts 7,979; chartProfiles 1,120; terminalComparisons 747; openscriptStudies 678; alertsModel 596; customIndicators 588; drawingToolMetadata 567; chartContract 560; backtestRun 490) |
| `lib/chart/**` + `components/chart/**` | 4 + 6 | 649 + 1,241 |
| hooks: useChartWorkspaceCatalog 156, useWorkspaceAutosave 186, useTrailingSL 214, useWorkspaceGridTransition 127, useLiveQuote 352, useSupportedExchanges 125, useOptionChainLive, usePageVisibility 186 | 8 | ~1,500 |
| api: trading 343, watchlist 93, openscriptRunner 424, strategy-chart 136, option-chain 44, scalping (~170), historify (~140); agent 1,370 only with AgentPanel | 7-8 | ~1,350 (+1,370) |
| stores: alertStore 142, brokerStore 40, sessionStore 11 | 3 | 193 |
| shared: MarketDataManager 870, contexts/MarketDataContext 161, utils/toast 114, utils/errorReporter 232, lib/strategyMath, lib/serverSentence 115, types/trading 132, types/websocket 21, types/option-chain, types/scalping | ~10 | ~1,900 |
| agent (optional AgentPanel): components/agent 3,340 + lib/agent 3,592 | 24 | 6,932 |
| ui patches: dialog/dropdown-menu/popover `container` prop, alert `warning` variant, empty-state | 5 | ~80 changed |
| index.css lines 762-951 (scrollbars, color-scheme, oscript tokens) | 1 | 190 |
| tests: components/trading 45 files, lib/trading 93 files, hooks 5 files, lib/chart 3 | ~146 | ~29,000 |

Total terminal closure (per inventory script): 193 files, 64,802 LOC including shared modules; 49,117 LOC for the five terminal-specific dirs.

### 4.5 What must change for Tauri

1. Session and CSRF: `webClient` (`/watchlist/*`, `/alerts/*`, `/openscript/runner/*`, `/modify_order`...) and `fetch('/openscript/...')`, `fetch('/custom-indicators/...')` rely on Flask session cookies + `X-CSRFToken`. In Tauri either (a) run the embedded axum server (`src-tauri/src/webhook/server.rs:93-126`, which already serves `/api/v1/{placeorder,...,history,search,symbol,optionchain,...}`) on 127.0.0.1 and point `apiClient`/`webClient` at `http://127.0.0.1:<port>` with a bearer/apikey instead of cookies, or (b) replace each api module with `invoke()` wrappers. Option (a) keeps the 49k-LOC terminal untouched; only `api/client.ts` and the four raw-`fetch` sites (`Trading.tsx:872-873`, `instrumentFacts.ts:232`, `openscriptFiles.ts:135-239`, `terminal.ts:1651`, `customIndicators.ts:464`, `alertLog.ts:58`) need a base-URL helper.
2. Market data: `MarketDataManager.ts` opens `new WebSocket(websocket_url)` and authenticates with the apikey. Desktop Rust already has a websocket manager emitting `market_tick` Tauri events (`commands/websocket.rs:37-133`, `websocket/manager.rs`). Either expose a local WS server on 8765 speaking the proxy protocol (authenticate/subscribe/market_data frames) so `MarketDataManager` works unchanged, or add a Tauri-events adapter inside `MarketDataManager.connect()` (`lib/MarketDataManager.ts:371-425`) mapping `market_tick` payloads to its `SymbolData` fan-out.
3. Order events: `useBlotter.ts:182` needs `order_update`; `useOrderEventRefresh.ts` needs `order_event`/`analyzer_update`. Rust currently emits `api_order`, `api_modify_order`, etc. (webhook/handlers.rs). Add a `SocketProvider` shim exposing a socket-like `{on, off, connected}` backed by `listen()` and have Rust emit `order_update`/`order_event`/`analyzer_update`.
4. Static strategy files: `/custom-indicators/index.json` + ES-module `import()` of indicator JS, and `/openscript/*` source CRUD, map to the app data dir (`strategies/indicators`, `strategies/openscript`). The dynamic `import(/* @vite-ignore */ url)` in `customIndicators.ts:332` needs an `http://127.0.0.1` URL (or `asset://` with CSP) - `tauri://` origin cannot import from `/custom-indicators/...`.
5. OpenScript live runner (`StrategiesPanel`, `StrategyBooks`): needs the Python host (`openscript_host/openscript_runner.py` + `openscript==0.8.1`). Options: bundle a Python sidecar, port the host to Node (openalgo-script is JS; the Python package is a second engine), or hide the panel behind a capability flag. Backtests do NOT need this (browser worker, 4.6 below).
6. Alerts: `alertDelivery.ts:158,222` posts to `/api/v1/<channel>/notify` (Telegram/WhatsApp); `alertNotify.ts` uses the Notification API and `alertDelivery.ts:81-117` WebAudio; Tauri needs `plugin-notification` for OS notifications and the notify endpoints need a Rust implementation or a disabled channel.
7. Agent panel: `lib/agent/stream.ts:360` streams a POST with `ReadableStream` - works in WebKit/WebView2 only against an http origin; needs the local server or a Tauri channel. Ship AgentPanel last.
8. Fullscreen: `ChartPane.tsx:858-865` uses `requestFullscreen` and the `container` portal prop; WKWebView supports element fullscreen on macOS 12+; the three ui components must carry the `container` prop.
9. Router: all terminal files import from `'react-router'`; add the alias or upgrade.
10. Workspace persistence is IndexedDB (`useChartWorkspaceCatalog.ts:34`) and `localStorage`; both work in Tauri webviews without change (origin `tauri://localhost` is stable).

### 4.6 OpenScript backtest confirmed client-side

`lib/trading/backtestRun.ts:2-10`: "Backtesting a saved OpenScript strategy, in the browser, over history... there is no server call here beyond fetching bars." `runBacktestOffThread` posts to `backtestWorkerEntry.ts` (Web Worker) -> `backtestFold.ts:70-79` `await import('openalgo-script')` then `engine.backtest(program, bars, settings, ...)`. Network: `/api/v1/symbol` (`backtestRun.ts:217`) and `/api/v1/history` (`:372`). No Python involved. Results render through `BacktestPanel.tsx`, `BacktestChart.tsx`, `BacktestResultTabs.tsx`, `lib/trading/backtestMarkers.ts`.

---

## 5. Theme / UI parity

- OKLch tokens: identical for `.analyzer` (web/desktop index.css 69-100), `.sandbox`, and all six `[data-accent]` themes; HSL `:root`/`.dark` identical (lines 1-357 byte-identical; `diff` reports only web-side additions from line 358).
- Missing in desktop: analyzer alert overrides (358-424), `color-scheme` + global scrollbar rules (762-837), `.no-scrollbar`/`.scrollbar-thin` (838-873), `--oscript-*` tokens and classes (875-951). Flow styles (473-760) are excluded scope.
- shadcn/ui components web pages use that desktop lacks: `empty-state.tsx` (Positions, Holdings, OrderBook, agent config). Variant/prop gaps: `alert` `warning` variant (web alert.tsx:14-15, used by `showToast.warning` consumers and /trading `ChartStateOverlay`), `container` prop on `DialogContent`/`DropdownMenuContent`/`PopoverContent`, `command.tsx` sr-only header placement. `components/ErrorBoundary.tsx` and `utils/errorReporter.ts` wrap the web app in `main.tsx:4-13`; desktop `main.tsx` has neither.
- Toasts: web `app/providers.tsx:23-34` reads position/duration/visibleToasts from `alertStore` and passes `closeButton`; desktop hardcodes `position="top-right"`.
- Fonts/layout files (`Layout.tsx`, `FullWidthLayout.tsx`, `MobileBottomNav.tsx`) differ only by the router import specifier (2-3 lines).

---

## 6. Web frontend test suite inventory (to carry over)

### 6.1 Configuration
- `frontend/vitest.config.ts`: jsdom, `globals: true`, setup `src/test/setup.ts`, include `src/**/*.{test,spec}.*`, v8 coverage excluding `src/test`, `types`, configs; `css: true`; alias `@`. Uses the `oxc` jsx option (vite 8 / rolldown).
- `frontend/tsconfig.test.json`: extends app config, types `vitest/globals` + `@testing-library/jest-dom`, includes `src/**/*.test.ts(x)` and `src/test/**`.
- `frontend/playwright.config.ts`: testDir `e2e`, baseURL `http://localhost:5173`, projects chromium/firefox/webkit/Mobile Chrome/Mobile Safari, `webServer: npm run dev`, html reporter, trace on first retry.
- Scripts (`package.json`): `test`, `test:run`, `test:coverage`, `test:ui`, `test:a11y` (`--testNamePattern='accessibility'`), `e2e`, `e2e:ui`, `e2e:debug`, `e2e:codegen`.
- Dev deps: vitest ^4.1.11, @vitest/coverage-v8, jsdom ^27.4.0, @testing-library/{react,user-event,jest-dom}, jest-axe ^10, vitest-axe, axe-core, @axe-core/react, @axe-core/playwright, @playwright/test ^1.58.

### 6.2 Shared test utilities (`src/test/`, 828 LOC)
`setup.ts` (80: jest-dom matchers, cleanup, matchMedia, class-based ResizeObserver/IntersectionObserver mocks (lines 27-55, required by Radix/floating-ui), scrollTo, pointer-capture + scrollIntoView stubs (62-66), clipboard); `test-utils.tsx` (23: render with QueryClient/Router providers); `a11y-utils.ts` (27: axe wrapper); `axiosAnswer.ts` (72: axios response factory); `fakeChart.tsx` (286: openalgo-charts double for terminal tests); `marketDataHarness.ts` (214: MarketDataManager/WebSocket harness); `runtimeReports.ts` (126).

### 6.3 Unit tests: 244 files, 2,644 `it/test` cases, 52,606 LOC

| Directory | Files | Cases | LOC | Covers |
|---|---|---|---|---|
| lib/trading | 93 | 1,070 | 19,772 | terminal state machine, drawings, alerts model/delivery, OpenScript files/highlight/studies/intervals, backtest engine/fold/markers, profiles, comparisons, replay, price axis (axe), custom indicators |
| components/trading (+dock) | 40 + 5 | 362 + 53 | 7,805 + 781 | panels/dialogs (ChartSettingsDialog is axe-checked), blotter, order actions |
| pages | 11 | 82 | 3,246 | ActionCenter, GEXDashboard, HistorifyCharts, MasterContract, Positions, PortfolioBacktester.accessibility, StrategyBuilder(+accessibility), Trading, WebSocketTest, toolsRefusalMessages |
| pages/strategy | 5 | 190 | 3,761 | Detail, List, routeParams, strategy_module api, useStrategyLive |
| pages/admin, pages/agent, pages/flow | 2 / 2 / 1 | 27 / 5 / 1 | 359 / 181 / 98 | Diagnostics, webServerSummary; AgentChat.retry, AgentIndex; FlowEditorViewport (excluded scope) |
| components/strategy-builder | 13 | 79 | 2,547 | tabs, TemplateDialog (axe) |
| components/agent (+config, viz) | 4 + 2 + 7 | 49 + 22 + 79 | 774 + 445 + 1,980 | message rendering, provider catalog, Plotly/OpenUI viz |
| components/flow/panels | 6 | 70 | 1,286 | excluded scope |
| components/chart, lib/chart(+feeds), lib/historify | 1 / 1 / 2 / 1 | 26 / 8 / 30 / 25 | 514 / 97 / 496 / 219 | OpenAlgoChart, intervalRegistry, historify/strategy feeds, catalog |
| components/socket, components/layout, components/auth, components/ui | 2 / 3 / 1 / 2 | 23 / 23 / 5 / 24 | 736 / 327 / 132 / 228 | SocketProvider + keep-reconnecting, Navbar/MobileBottomNav (axe), AuthSync, button (axe) + page-loader |
| hooks | 10 | 66 | 1,658 | useChartWorkspaceCatalog, useLivePrice, useOptionChainLive/Polling/Preferences, usePageVisibility, useSupportedExchanges, useTrailingSL, useWorkspaceAutosave, useWorkspaceGridTransition |
| lib (top-level), lib/agent, lib/flow | 12 / 5 / 4 | 150 / 52 / 83 | 2,779 / 1,016 / 703 | MarketDataManager, rateLimiter, strategyMath/templates, scalping*, serverSentence; agent stream/hydrate; flow (excluded) |
| api, stores, utils, config, components/option-chain, components/portfolio | 3 / 1 / 1 / 1 / 1 / 2 | 15 / 5 / 7 / 10 / 1 / 2 | | client.csrf, openscriptRunner, portfolio.tearsheet; flowWorkflowStore.saveRace; errorReporter; navigation; option-chain; portfolio (excluded) |

Mocking patterns to preserve: 13 files `vi.mock('axios'|'@/api/client')`, 10 files stub `global.fetch`, 5 files `vi.mock('openalgo-charts')` (use `test/fakeChart.tsx`), 1 `vi.mock('socket.io-client')`, 1 `vi.mock('react-router')`. For the desktop these become mocks of the transport adapter (`invoke`/`listen`) or of a local http base URL.

### 6.4 Accessibility tests (axe)
`components/layout/MobileBottomNav.test.tsx`, `components/strategy-builder/TemplateDialog.test.tsx`, `components/trading/ChartSettingsDialog.test.tsx`, `components/ui/button.test.tsx`, `lib/trading/priceAxis.test.ts`, `lib/trading/profileSettingsView.test.ts`, `pages/StrategyBuilder.accessibility.test.tsx` (plus `pages/PortfolioBacktester.accessibility.test.tsx`, excluded scope). Run via `npm run test:a11y`.

### 6.5 Playwright e2e (5 specs, 19 cases, 331 LOC)
`e2e/accessibility.spec.ts` (4: axe on public pages), `auth.spec.ts` (4: setup/login redirects), `home.spec.ts` (4), `navbar-fit.spec.ts` (3: navbar fits at breakpoints), `navigation.spec.ts` (4). No fixtures dir. For Tauri these need `tauri-driver`/WebDriver or a `tauri dev` web target with the local backend.

### 6.6 Desktop today
4 test files (`config/navigation.test.ts`, `components/ui/button.test.tsx`, `components/ui/page-loader.test.tsx`, `components/layout/MobileBottomNav.test.tsx`), `src/test/setup.ts` with `vi.fn()`-based observer mocks that web replaced because they break Radix dropdown/select tests (web setup.ts:27-55). No a11y deps, no Playwright.

---

## 7. Reference: scratch artifacts produced during this audit
- `scratchpad/inv.json` / `inv.py`: per-page closure inventory (LOC, URLs, sockets, npm deps) for all 105 web pages.
- `scratchpad/routes.txt`: all 585 Flask blueprint routes with prefixes (58 blueprints).
- `scratchpad/diffrq.txt`: `diff -rq` of the two `src` trees.
