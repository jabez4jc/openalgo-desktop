# 06 - Build / Test / CI Baseline: openalgo-desktop (Tauri 2 + Rust + React)

Repo: `/Users/openalgo/openalgo-desktop/openalgo-desktop` (git, HEAD `6947b4c`, no `.github/` directory yet)
Reference: `/Users/openalgo/openalgo-desktop/openalgo` (read-only; `.github/workflows/ci.yml`, `security.yml`)
Measured on: macOS 14.6.1 arm64, rustc/cargo 1.92.0, clippy 0.1.92, rustfmt 1.8.0, node 22.23.1, npm 10.9.8, tauri crate 2.12.1, @tauri-apps/cli 2.9.6.
Logs: `scratchpad/{cargo-check,clippy,fmt,cargo-test-norun,cargo-test,tsc,biome,biome-github,vitest,vite-build}.log`

## 1. Baseline table

| Tool / command | Exit | Errors | Warnings | Notes |
|---|---|---|---|---|
| `cargo check` (src-tauri) | 0 | 0 | 0 | 9m51s cold. Dominated by `duckdb` (bundled C++) and `libsqlite3-sys`. |
| `cargo clippy --all-targets -- -D warnings` | 101 | 250 (lib) / 251 (lib test) | 0 | 212 of 250 are one lint, `clippy::result_large_err` (`AppError` is >=136 bytes because of the `tungstenite::Error` variant). 38 other findings, all mechanical. |
| `cargo test --no-run` | 0 | 0 | 0 | 19m05s cold (test profile recompiles duckdb). |
| `cargo test` | 0 | 0 failed | - | 26 passed, 0 ignored, 2.14s. Doc-tests: 0. `main.rs` bin: 0 tests. |
| `cargo fmt --check` | 1 | 436 diff hunks in ~70 files (6441 diff lines) | - | Rust tree has never been rustfmt'd. No `rustfmt.toml`. |
| `npx tsc -b --noEmit` | 0 | 0 | 0 | Clean, but see 6.4: `strict: false`, and `*.test.*` / `src/test/**` are excluded from every tsconfig, so test files are never type-checked. |
| `npx biome check ./src` | 1 | 12 | 63 (+3 infos) | 144 files. Errors: 9 `format`, 3 `assist/source/organizeImports`. Warnings: 51 `useExhaustiveDependencies`, 5 `noUnusedVariables`, 2 `noUnusedImports`, 1 `useOptionalChain`, 1 `useButtonType`; infos: 3 `useParseIntRadix`. All 12 errors auto-fix with `biome check --write`. |
| `npx vitest run` | 1 | 4 failed tests, 2 suites failed to load | - | 11 passed (all in `navigation.test.ts`). Two root causes, both config: no vitest config (no `environment: 'jsdom'`, no `setupFiles`) and `jest-axe` not installed. |
| `npx vite build` | 0 | 0 | 0 | 1m05s. `dist/` 2.6 MB. Largest chunk `index-BbSAgOWm.js` 401 kB (gzip 131 kB); no chunk-size warning because limit is raised to 600. |
| `npx tauri info` | error | 1 | 3 outdated | "version mismatched Tauri packages": `tauri` crate 2.12.1 vs `@tauri-apps/api` 2.9.1; `tauri-plugin-shell` 2.4.0 vs `@tauri-apps/plugin-shell` 2.3.4. Xcode (full) not installed (only CLT) - irrelevant for CI, fine locally for non-iOS. |

Compile-time budget that CI must plan for (cold, 10-core M-series): check ~10 min, test build ~19 min; a `tauri build` release with `lto = true`, `codegen-units = 1`, `opt-level = "s"` will be longer again. See 7.5 for the DuckDB mitigation.

## 2. Top blocking errors and fixes

Ordered by what blocks a green CI first.

### 2.1 Clippy under `-D warnings`: 250 errors (BLOCKS `rust-lint`)

212 x `result_large_err`, concentrated in `src-tauri/src/db/sqlite/mod.rs` (99), `db/sqlite/sandbox.rs` (15), `db/sqlite/market.rs` (11), `db/sqlite/api_keys.rs` (9), `db/sqlite/settings.rs` (8), then 1-7 each across `db/sqlite/*`, `services/*`, `security/*`, `db/duckdb/*`, `state.rs`.

Root cause (`src-tauri/src/error.rs:22`):
```rust
#[error("WebSocket error: {0}")]
WebSocket(#[from] tokio_tungstenite::tungstenite::Error),   // largest variant, >=136 bytes
```
Proper fix: box the fat variants and keep `?` ergonomics with a manual `From`:
```rust
#[error("WebSocket error: {0}")]
WebSocket(Box<tokio_tungstenite::tungstenite::Error>),
#[error("HTTP request error: {0}")]
Http(Box<reqwest::Error>),

impl From<tokio_tungstenite::tungstenite::Error> for AppError {
    fn from(e: tokio_tungstenite::tungstenite::Error) -> Self { AppError::WebSocket(Box::new(e)) }
}
impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self { AppError::Http(Box::new(e)) }
}
```
(Also update the two `match` arms in `From<AppError> for ErrorResponse`; nothing else pattern-matches the payload.) Interim fix if the boxing is deferred: in `src-tauri/Cargo.toml`
```toml
[lints.clippy]
result_large_err = "allow"
```
Do not use `#![allow]` sprinkled in files; the Cargo `[lints]` table is the single switch CI and editors both honour.

The remaining 38 (dedup, file:line, with the one-line fix):
- `brokers/angel/mod.rs:1105` redundant closure -> pass the fn directly.
- `brokers/angel/mod.rs:1160,1164,1168,1176,1182,1188` `if` with identical blocks (6) -> merge conditions with `||`; this looks like a copy/paste mapping bug worth a real look, not just a lint fix.
- `brokers/fyers/mod.rs:1240`, `websocket/manager.rs:830,837` `.get(0)` -> `.first()`.
- `brokers/fyers/mod.rs:1366` (9 args), `db/sqlite/latency_logs.rs:28` (12), `db/sqlite/order_logs.rs:30` (13), `db/sqlite/mod.rs:163,369,737`, `db/sqlite/sandbox.rs:71`, `db/sqlite/strategy.rs:86`, `db/sqlite/traffic_logs.rs:28` (8 each) too_many_arguments (10) -> introduce param structs (`NewOrderLog`, `NewLatencyLog`, ...) or `#[allow(clippy::too_many_arguments)]` on the row-insert fns.
- `commands/settings.rs:294` collapsible `if`.
- `db/sqlite/api_keys.rs:134` `filter_map` -> `map`.
- `db/sqlite/latency_logs.rs:186,219,230`, `db/sqlite/traffic_logs.rs:109,123` `for x in iter { if let Ok(v) = x` -> `.flatten()` (5).
- `db/sqlite/mod.rs:904` very complex type -> `type` alias.
- `db/sqlite/settings.rs:268,275,282,289` manual range -> `(a..=b).contains(&x)` (4).
- `security/encryption.rs:62` needless borrow.
- `services/options_service.rs:358,360` manual strip -> `strip_prefix("ITM"/"OTM")`.
- `webhook/handlers.rs:2004,2009,2014` `to_string()` for comparison -> compare `&str`.
- `websocket/manager.rs:30` `impl Default` -> `#[derive(Default)]`; `:790` index loop -> `iter().enumerate()` or `iter()`.

Estimated effort: 1-2 hours; `cargo clippy --fix --all-targets --allow-dirty` auto-fixes roughly half.

### 2.2 `cargo fmt --check` fails on ~70 files (BLOCKS `rust-lint`)

One-time: `cd src-tauri && cargo fmt`, commit as a formatting-only change (6.4k diff lines; review by `git diff --stat` only). Optionally add `src-tauri/rustfmt.toml` with `edition = "2021"` so CI and editors agree. Do this before 2.1 so the clippy diff is readable.

### 2.3 Biome: 12 errors (BLOCKS `frontend-lint`)

`npx biome check --write ./src` fixes all 12 (format: `src/api/tauri-client.ts`, `src/pages/{BrokerSelect,BrokerTOTP,Login,Playground,Sandbox,SandboxPnL,WebSocketTest,admin/ServerSettings}.tsx`; import order: `src/pages/{Sandbox,SandboxPnL,WebSocketTest}.tsx`). The 63 warnings do not fail CI (biome exits 1 only on errors) but 51 `useExhaustiveDependencies` hits (`src/pages/Historify.tsx:408` alone has 7 missing deps) are the same class of stale-closure bug the web repo tolerates with `continue-on-error`; recommend leaving them as warnings and tracking them, not downgrading to `off`.

Use `biome ci ./src` in CI (read-only, fails on errors, honours `--reporter=github` for annotations) rather than `npm run lint`, which is `biome lint` only and skips the formatter and import-sorting assist that produced all 12 errors.

### 2.4 Vitest: 4 failing tests + 2 suites that cannot load (BLOCKS `frontend-test`)

Not test bugs; two missing pieces of project setup:

1. No vitest configuration. `vite.config.ts` has no `test` block and there is no `vitest.config.ts`, so tests run in the default `node` environment: `ReferenceError: document is not defined` (all 4 `page-loader.test.tsx` cases) and `src/test/setup.ts` (jest-dom matchers, matchMedia/ResizeObserver mocks) never loads. The reference web frontend has `frontend/vitest.config.ts` with `globals: true, environment: 'jsdom', setupFiles: ['./src/test/setup.ts'], include: ['src/**/*.{test,spec}.{ts,tsx}'], coverage.provider: 'v8'`. Copy it (adjust the `@` alias to `./src`).
2. `jest-axe` is imported by `src/components/ui/button.test.tsx:1`, `src/components/layout/MobileBottomNav.test.tsx:2` and `src/test/a11y-utils.ts:1` but is not in `package.json`. Add `jest-axe` (`^10.0.0`) and `@types/jest-axe` (`^3.5.9`) to devDependencies (same pins as `openalgo/frontend/package.json`).

After both, the expected result is 48 tests across 4 files (13 + 20 + 4 + 11 by `it(` count), not the 15 currently collected.

### 2.5 Tauri JS/Rust version mismatch (RISK for `tauri-build`)

`tauri` 2.12.1 (Cargo) vs `@tauri-apps/api` 2.9.1 and `@tauri-apps/cli` 2.9.6; `tauri-plugin-shell` 2.4.0 vs `@tauri-apps/plugin-shell` 2.3.4. `tauri info` reports this as an error. Tauri only guarantees IPC compatibility within the same major.minor. Fix: `npm i -D @tauri-apps/cli@^2.12 && npm i @tauri-apps/api@^2.12 @tauri-apps/plugin-shell@^2.4`, or pin the crates down (`tauri = "=2.9.x"`). Add `npx tauri info` to the `tauri-build` job so drift fails fast.

### 2.6 `Cargo.lock` is gitignored (BLOCKS reproducible CI builds)

`.gitignore` line `Cargo.lock` under "Rust / Cargo". For a binary/application crate the lockfile must be committed: it is what makes `cargo check` in CI resolve the same `duckdb 1.10506.0`, `tauri 2.12.1`, etc. that were tested locally, and `Swatinem/rust-cache` keys its cache on it. Remove that line and commit `src-tauri/Cargo.lock`. (`src-tauri/gen/` gitignored is correct; Tauri regenerates `gen/schemas` at build.)

## 3. Test inventory

### 3.1 Frontend (Vitest 4.0.17, jsdom 24, @testing-library/react 16)

| File | `it(` count | Status now |
|---|---|---|
| `src/components/ui/button.test.tsx` | 20 | suite fails to load (`jest-axe`) |
| `src/components/layout/MobileBottomNav.test.tsx` | 13 | suite fails to load (`jest-axe`) |
| `src/components/ui/page-loader.test.tsx` | 4 | 4 fail (`document is not defined`) |
| `src/config/navigation.test.ts` | 11 | 11 pass |
| `src/test/setup.ts`, `test-utils.tsx`, `a11y-utils.ts` | harness | present, never loaded |

48 tests over 4 files for 144 source files. Nothing covers `src/api/tauri-client.ts` (737 lines, the whole `invoke()` boundary, 23 distinct command names), `src/stores/*`, `src/hooks/*` (`useMarketData`, `useSocket`, `useAutoLogout` all use Tauri `listen`), or any page.

### 3.2 Rust (26 tests, 6 modules, all inline `#[cfg(test)]`; no `src-tauri/tests/`)

| Module | Tests | What is covered |
|---|---|---|
| `src/security/mod.rs` | 8 | AES-GCM round trip, nonce uniqueness, wrong-nonce rejection, empty/unicode/long inputs, argon2 hash/verify, unique salts |
| `src/db/sqlite/api_keys.rs` | 7 | generate, mask, create+validate, invalid key, list, delete, duplicate name |
| `src/webhook/rate_limiter.rs` | 4 | token bucket basic/refill, rate-limit type detection, smart-order delay |
| `src/scheduler/auto_logout.rs` | 3 | duration math, warnings order |
| `src/security/encryption.rs` | 2 | encrypt/decrypt, nonces |
| `src/security/hashing.rs` | 2 | hash/verify, distinct hashes |

Coverage estimate by layer (lines in files with >=1 test / lines in layer):
- `commands/` (16 files, 92 `#[tauri::command]`, all 92 registered in `lib.rs`): 0 / 2,146 lines -> 0%.
- `services/` (12 files, 40 pub fns): 0 / 2,411 -> 0%.
- `brokers/` (angel 1,294, fyers 1,496, zerodha 1,045, types, mod): 0 / 4,147 -> 0%. No fixtures, no HTTP mocking dependency.
- `webhook/` (handlers 2,167 with 36 handler fns and 44 routes, server 271, types 1,145, rate_limiter 330): 330 / 3,913 -> ~8% (rate limiter only; zero on handlers/types/server, i.e. the public API surface).
- `websocket/` (manager 1,129, handlers 80): 0%.
- `db/sqlite` (17 files): 396 / 5,344 -> ~7% (api_keys only; migrations, sandbox, settings, market, symbol, order/traffic/latency/analyzer logs untested). `db/duckdb`: 0%.
- `security/`: 446 / 573 -> ~78% (file_storage.rs 0).
- `scheduler/`: 288 / 298.
- Whole crate: ~1,460 / 20,601 lines are in modules with any test (~7%); realistic executed-line coverage is 4-5%. `cargo-llvm-cov` should be run to replace this estimate (see 8.6).

Other census: 70 `.unwrap()`, 4 `.expect(`, 0 `unsafe`, 0 `todo!`, 35 `#[allow(...)]` (mostly `dead_code` in `brokers/{angel,fyers}`).

## 4. Reference CI conventions (`openalgo/.github/workflows/`)

### ci.yml (on push/PR to `main`; `permissions: contents: read, actions: write`; `concurrency: ${{ github.workflow }}-${{ github.ref }}` with `cancel-in-progress: true`)

| Job | Runner | What it does | Convention to copy |
|---|---|---|---|
| `backend-lint` | ubuntu-latest | `ruff check`, `ruff format --check`, both `continue-on-error: true` ("pre-existing warnings") | Tolerates legacy lint debt explicitly, per step |
| `backend-test` | ubuntu-latest, matrix py 3.12/3.13/3.14, `fail-fast: false` | explicit CI-safe pytest list, `--timeout=60` | Allowlist of tests that need no broker creds |
| `gunicorn-boot`, `eventlet-fallback`, `gthread-gates`, `gthread-migration` | ubuntu-latest, `needs: backend-lint` | boot the real server with a throwaway `.env`, run smoke; "every test ran" guard that fails if anything skipped | Smoke-boot the real binary; fail on silent skips |
| `gthread-platforms` | matrix `windows-latest, macos-latest, ubuntu-24.04-arm` | portable runtime tests | Cross-platform matrix incl. ARM |
| `frontend-lint` | ubuntu, node 22, `actions/setup-node@v7` `cache: npm` + `cache-dependency-path` | `npm ci && npm run lint` | |
| `frontend-build` | matrix node 20/22/24 | `npm run build`, `upload-artifact@v6` dist, 7 days | Matrix over `engines` majors |
| `frontend-test` | matrix node 20/22/24 | `test:run`, `test:coverage`, upload coverage | |
| `frontend-e2e` | ubuntu | `playwright install --with-deps chromium`, `npm run e2e -- --project=chromium`, upload report on failure | |
| `security-scan` | ubuntu | bandit, pip-audit (`continue-on-error`) | |
| `commit-dist` | main push only, `permissions: contents: write`, `needs` build/lint/test | rebuild, `git add -f frontend/dist` (excluding .gz/.br), commit `[skip ci]`, push as `github-actions[bot]` | Not applicable: desktop ships bundles, not a committed dist |
| `docker-build` / `docker-manifest` | native amd64 + `ubuntu-24.04-arm`, digest push, manifest merge, Trivy | | Native-arch matrix instead of QEMU; same idea as building Linux ARM Tauri bundles natively |

Action pins used: `actions/checkout@v6`, `actions/setup-node@v7`, `actions/upload-artifact@v6`, `actions/download-artifact@v8`, `github/codeql-action/upload-sarif@v4`.

### security.yml (cron `0 2 * * 1` + `workflow_dispatch`; `permissions: security-events: write`)

bandit -> JSON + SARIF (with a stub-SARIF fallback for a formatter crash), `upload-sarif` category `bandit`, `pip-audit --format=json`, upload all reports 30 days, every scanner `|| true` so the run never fails on findings. Desktop equivalents: `cargo audit` (rustsec) and `cargo deny check` for Rust, `npm audit --omit=dev` for JS, `trivy fs` for both lockfiles with SARIF upload.

## 5. Secrets and environment usage

`grep -rn "dotenv\|\.env\|process.env\|std::env::var" src src-tauri/src`:
- Rust: no `std::env::var`, no dotenv crate. Only `tracing_subscriber::EnvFilter::try_from_default_env()` (`RUST_LOG`) in `src-tauri/src/lib.rs:29` with a safe default. Credentials go through `keyring` + AES-GCM (`security/`), not env.
- `vite.config.ts:34,36,38`: `process.env.TAURI_ENV_PLATFORM` / `TAURI_ENV_DEBUG`, set by the Tauri CLI; correct.
- `src/pages/{GoCharting.tsx:203, Profile.tsx:1391,1832, chartink/ChartinkIndex.tsx:135, TradingView.tsx:222, ServerError.tsx:68}`: UI copy telling the user to edit a `.env` file / `HOST_SERVER`. Inherited from the web app; there is no `.env` in the desktop app. Cosmetic, but misleading; replace with the desktop settings path.
- No `.env*` files in the repo; `.gitignore` already excludes `.env`, `*.pem/.p12/.key`, DB files.
- Hardcoded-secret regex (`api_key|secret|password|token = "<12+ chars>"`) over `src` and `src-tauri/src`: no hits.
- CI will need: `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` only if the updater plugin is added later (not today); `APPLE_*` / `WINDOWS_CERTIFICATE*` only when signing is turned on (`signingIdentity: null`, `certificateThumbprint: null` now).

## 6. Config review

### 6.1 `src-tauri/tauri.conf.json`
- `build.beforeDevCommand: npm run dev`, `devUrl: http://localhost:5173`, `beforeBuildCommand: npm run build` (= `tsc -b && vite build`), `frontendDist: ../dist`: consistent with `vite.config.ts` (`port: 5173, strictPort: true`, `outDir: dist`). OK.
- `bundle.active: true, targets: "all"`: per-OS this yields app+dmg (macOS), nsis+msi (Windows), deb+rpm+appimage (Linux). Fine for tauri-action. `nsis.installMode: currentUser` avoids UAC in CI smoke runs.
- `bundle.icon`: all five listed files exist in `src-tauri/icons/` (32x32.png, 128x128.png, 128x128@2x.png, icon.icns 454 kB, icon.ico). No `icon.png` (Tauri's default 512x512 for Linux desktop entry/tray); not referenced, so not an error, but `npx tauri icon public/logo.png` would regenerate the full set incl. `icon.png` and Windows Store sizes.
- `app.security.csp: null` and `withGlobalTauri: true`: CSP unset means the webview will load any origin; the frontend already imports from `@tauri-apps/api/core` so `withGlobalTauri` is unnecessary. Both widen attack surface (the app embeds TradingView/GoCharting/Chartink pages). Recommend a CSP such as `default-src 'self' ipc: http://ipc.localhost; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; connect-src 'self' ipc: http://ipc.localhost http://127.0.0.1:* ws://127.0.0.1:* https://*.angelone.in https://api.kite.trade https://api-t1.fyers.in wss://...` (tune per broker) and `withGlobalTauri: false`. Cross-ref report 01-security.
- `plugins.shell.open: true` with capability `shell:allow-open`: needed for broker OAuth. The `default` capability also grants `core:webview:allow-internal-toggle-devtools`; drop for release builds.
- `copyright: "Copyright © 2024 OpenAlgo"`: stale year.

### 6.2 `src-tauri/Cargo.toml`
- `rust-version = "1.77"` is not credible for `tauri 2.12` / `duckdb 1.10506` / `reqwest 0.12`; CI uses stable 1.92 so it is inert, but either bump it to what `cargo msrv` reports or drop it so nobody trusts it.
- `thiserror = "1.0"` while the lock also resolves `thiserror 2.0.21` transitively; harmless duplicate.
- `[profile.release]` `lto = true, codegen-units = 1, opt-level = "s", strip = true, panic = "abort"`: correct for a shipped desktop binary; expect long release link times on Windows.
- `duckdb = { features = ["bundled"] }` + `rusqlite bundled`: this is the 10-19 minute cold compile. See 7.5.
- `crate-type = ["lib", "cdylib", "staticlib"]`: default Tauri 2 template (mobile-ready). Fine.

### 6.3 `vite.config.ts`
- `manualChunks` references `recharts`/`d3-` which are not dependencies (dead branches, harmless). `vendor-charts` carries `lightweight-charts` (168 kB).
- `chunkSizeWarningLimit: 600` hides the 401 kB main chunk; acceptable for desktop.
- No `test` block (see 2.4). Prefer a separate `vitest.config.ts` that `mergeConfig`s `vite.config.ts` so `tsconfig.node.json` (`include: ["vite.config.ts"]`) also covers it.

### 6.4 `tsconfig*.json`
- `tsconfig.app.json`: `strict: false, strictNullChecks: false, noImplicitAny: false, noUnusedLocals: false` - the clean `tsc` result is weak evidence. `exclude` drops `src/**/*.test.ts(x)` and `src/test/**`, and no other project includes them, so test files are never type-checked (e.g. the `jest-axe` import would have been caught). Add `tsconfig.test.json` (`types: ["vitest/globals", "@testing-library/jest-dom", "jest-axe"]`, `include: ["src/**/*.test.*", "src/test/**"]`) and reference it from `tsconfig.json`.
- `tsconfig.node.json` is strict and only covers `vite.config.ts`. Good.
- `package.json` has no `typecheck` script although `README.md:192` tells contributors to run `npm run typecheck`; add `"typecheck": "tsc -b --noEmit"`. Also add `"test:coverage": "vitest run --coverage"` (coverage-v8 is already installed) and `"lint:ci": "biome ci ./src"`.

### 6.5 `biome.json`
- Schema 2.3.11 matches the installed CLI. `vcs.useIgnoreFile: true` so `dist/`, `node_modules/` are skipped. `files.includes` limits to `src/**` so `vite.config.ts` is unlinted; fine.
- Noise suppression (`noExplicitAny: off`, `noNonNullAssertion: off`, `useKeyWithClickEvents: off`) mirrors the web frontend. OK for parity; revisit `noDangerouslySetInnerHtml: warn` once Playground/Docs pages are reviewed.

### 6.6 `.gitignore`
- `Cargo.lock` must come out (2.6). `*.log` would also hide any committed test fixtures named `.log`; fine today.

## 7. Proposed `.github/workflows/ci.yml`

Design goals: mirror the web repo's conventions (checkout@v6, setup-node@v7 npm cache, upload-artifact@v6, concurrency cancel, `fail-fast: false`, `permissions: contents: read`, explicit `continue-on-error` only where debt is acknowledged), keep lint/typecheck/unit jobs on ubuntu for speed, run the expensive `tauri build` as a 3-OS matrix (+ Linux ARM for Raspberry Pi), cache Rust aggressively, and release on tags with `tauri-apps/tauri-action`.

```yaml
name: CI

on:
  push:
    branches: [main]
    tags: ['v*']
  pull_request:
    branches: [main]

permissions:
  contents: read

concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ !startsWith(github.ref, 'refs/tags/') }}

env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0          # rust-cache works better without incremental artifacts
  RUSTFLAGS: -D warnings        # warnings fail every cargo step, not only clippy
  RUST_BACKTRACE: 1

jobs:
  # ---------------------------------------------------------------- Rust
  rust-lint:
    name: rust-lint (fmt + clippy)
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v6
      - name: Tauri 2 system deps
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf \
            build-essential curl wget file libssl-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: src-tauri
          shared-key: lint-ubuntu       # shared with rust-test on the same OS
      - name: rustfmt
        run: cargo fmt --all --check
        working-directory: src-tauri
      - name: clippy
        run: cargo clippy --all-targets --locked -- -D warnings
        working-directory: src-tauri

  rust-test:
    name: rust-test (${{ matrix.os }})
    needs: rust-lint
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-22.04, windows-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6
      - name: Tauri 2 system deps (Linux)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf \
            build-essential curl wget file libssl-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: src-tauri
          shared-key: test-${{ matrix.os }}
      - uses: taiki-e/install-action@nextest
      - name: Unit + integration tests
        run: cargo nextest run --all-targets --locked --no-fail-fast
        working-directory: src-tauri
      - name: Doc tests (nextest does not run them)
        run: cargo test --doc --locked
        working-directory: src-tauri

  rust-coverage:
    name: rust-coverage (llvm-cov)
    needs: rust-lint
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v6
      - run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libssl-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
        with: { components: llvm-tools-preview }
      - uses: Swatinem/rust-cache@v2
        with: { workspaces: src-tauri, shared-key: cov-ubuntu }
      - uses: taiki-e/install-action@v2
        with: { tool: cargo-llvm-cov,cargo-nextest }
      - name: Coverage with threshold
        working-directory: src-tauri
        run: |
          cargo llvm-cov nextest --all-targets --locked --lcov --output-path lcov.info \
            --fail-under-lines 20     # baseline today is ~5%; raise in steps (see 8.6)
      - uses: actions/upload-artifact@v6
        with: { name: rust-coverage, path: src-tauri/lcov.info, retention-days: 7 }

  # ------------------------------------------------------------ Frontend
  frontend-lint:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions/setup-node@v7
        with: { node-version: '22', cache: npm, cache-dependency-path: package-lock.json }
      - run: npm ci
      - run: npx biome ci ./src --reporter=github

  frontend-typecheck:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions/setup-node@v7
        with: { node-version: '22', cache: npm, cache-dependency-path: package-lock.json }
      - run: npm ci
      - run: npx tsc -b --noEmit

  frontend-test:
    strategy:
      fail-fast: false
      matrix:
        node-version: ['20', '22', '24']   # engines in package.json
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions/setup-node@v7
        with: { node-version: '${{ matrix.node-version }}', cache: npm, cache-dependency-path: package-lock.json }
      - run: npm ci
      - run: npx vitest run --coverage --reporter=default --reporter=junit --outputFile=vitest-junit.xml
      - uses: actions/upload-artifact@v6
        if: always()
        with:
          name: frontend-coverage-node${{ matrix.node-version }}
          path: |
            coverage/
            vitest-junit.xml
          retention-days: 7

  frontend-e2e:
    needs: [frontend-lint, frontend-typecheck]
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions/setup-node@v7
        with: { node-version: '22', cache: npm, cache-dependency-path: package-lock.json }
      - run: npm ci
      - run: npx playwright install --with-deps chromium
      - run: npx vite build
      - run: npx playwright test --project=chromium     # see 8.3: mocked invoke layer, vite preview
      - uses: actions/upload-artifact@v6
        if: failure()
        with: { name: playwright-report, path: playwright-report/, retention-days: 7 }

  # ------------------------------------------------------- Tauri bundles
  tauri-build:
    name: tauri-build (${{ matrix.name }})
    needs: [rust-lint, frontend-lint, frontend-typecheck]
    strategy:
      fail-fast: false
      matrix:
        include:
          - name: linux-x86_64
            os: ubuntu-22.04              # oldest glibc => widest AppImage/deb compatibility
            target: x86_64-unknown-linux-gnu
          - name: linux-aarch64            # Raspberry Pi 4/5 (64-bit OS), native, no QEMU
            os: ubuntu-22.04-arm
            target: aarch64-unknown-linux-gnu
          - name: windows-x86_64
            os: windows-latest
            target: x86_64-pc-windows-msvc
          - name: macos-aarch64
            os: macos-latest
            target: aarch64-apple-darwin
          - name: macos-x86_64
            os: macos-latest
            target: x86_64-apple-darwin
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6
      - name: Tauri 2 system deps (Linux)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf \
            build-essential curl wget file libssl-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: '${{ matrix.target }}' }
      - uses: Swatinem/rust-cache@v2
        with:
          workspaces: src-tauri
          shared-key: build-${{ matrix.name }}
      - uses: actions/setup-node@v7
        with: { node-version: '22', cache: npm, cache-dependency-path: package-lock.json }
      - run: npm ci
      - name: Tauri env sanity (fails on JS/Rust version drift)
        run: npx tauri info
      - name: Build bundles (no upload on PR, debug symbols kept)
        if: "!startsWith(github.ref, 'refs/tags/')"
        uses: tauri-apps/tauri-action@v0
        with:
          args: --target ${{ matrix.target }} --ci
      - uses: actions/upload-artifact@v6
        with:
          name: bundles-${{ matrix.name }}
          path: |
            src-tauri/target/${{ matrix.target }}/release/bundle/**/*.deb
            src-tauri/target/${{ matrix.target }}/release/bundle/**/*.rpm
            src-tauri/target/${{ matrix.target }}/release/bundle/**/*.AppImage
            src-tauri/target/${{ matrix.target }}/release/bundle/**/*.msi
            src-tauri/target/${{ matrix.target }}/release/bundle/**/*.exe
            src-tauri/target/${{ matrix.target }}/release/bundle/**/*.dmg
          retention-days: 7
          if-no-files-found: error

  # ----------------------------------------------------------- Release
  release:
    name: release (${{ matrix.name }})
    if: startsWith(github.ref, 'refs/tags/v')
    needs: [rust-test, frontend-test, tauri-build]
    permissions:
      contents: write                  # tauri-action creates the GitHub release
    strategy:
      fail-fast: false
      matrix:
        include:
          - { name: linux-x86_64,   os: ubuntu-22.04,     target: x86_64-unknown-linux-gnu }
          - { name: linux-aarch64,  os: ubuntu-22.04-arm, target: aarch64-unknown-linux-gnu }
          - { name: windows-x86_64, os: windows-latest,   target: x86_64-pc-windows-msvc }
          - { name: macos-aarch64,  os: macos-latest,     target: aarch64-apple-darwin }
          - { name: macos-x86_64,   os: macos-latest,     target: x86_64-apple-darwin }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v6
      - if: runner.os == 'Linux'
        run: |
          sudo apt-get update
          sudo apt-get install -y --no-install-recommends \
            libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf \
            build-essential curl wget file libssl-dev libayatana-appindicator3-dev libxdo-dev
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: '${{ matrix.target }}' }
      - uses: Swatinem/rust-cache@v2
        with: { workspaces: src-tauri, shared-key: build-${{ matrix.name }} }
      - uses: actions/setup-node@v7
        with: { node-version: '22', cache: npm, cache-dependency-path: package-lock.json }
      - run: npm ci
      - uses: tauri-apps/tauri-action@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # Enable when signing is configured (all optional today):
          # APPLE_CERTIFICATE, APPLE_CERTIFICATE_PASSWORD, APPLE_SIGNING_IDENTITY,
          # APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID,
          # TAURI_SIGNING_PRIVATE_KEY, TAURI_SIGNING_PRIVATE_KEY_PASSWORD (updater plugin only)
        with:
          tagName: ${{ github.ref_name }}
          releaseName: 'OpenAlgo Desktop ${{ github.ref_name }}'
          releaseBody: 'See CHANGELOG.md for details.'
          releaseDraft: true
          prerelease: ${{ contains(github.ref_name, '-') }}
          args: --target ${{ matrix.target }} --ci
```

Notes on the design:

7.1 `RUSTFLAGS: -D warnings` at the workflow level makes `cargo check/test` fail on compiler warnings too (today there are 0, so this locks in the baseline); clippy gets `-D warnings` explicitly as well. `--locked` makes a stale `Cargo.lock` a hard failure instead of a silent re-resolve (requires 2.6).

7.2 `rust-test` runs on all three desktop OSes because `keyring`, `rusqlite` file paths and the axum socket binding are OS-sensitive; `rust-lint` runs once on Linux. `cargo-nextest` gives per-test process isolation (a hung `keyring`/socket test cannot stall the suite), JUnit output, retries, and is 20-40% faster on this many small tests; doc tests still need `cargo test --doc`.

7.3 Linux apt list is the Tauri 2 prerequisites list: `libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf` as requested, plus `build-essential curl wget file libssl-dev libayatana-appindicator3-dev libxdo-dev` which the official docs add and which `keyring` (secret-service / dbus) and `tray` features pull in. `ubuntu-22.04` rather than `-latest` so the produced `.deb`/AppImage links against glibc 2.35 and WebKitGTK 4.1 and runs on Debian 12 / Raspberry Pi OS Bookworm; `ubuntu-22.04-arm` is a GitHub-hosted native arm64 runner (free for public repos), the same approach the web repo uses with `ubuntu-24.04-arm` for its Docker arm64 leg.

7.4 Caching: `Swatinem/rust-cache@v2` with `workspaces: src-tauri` (the Cargo project is not at repo root) and a per-OS `shared-key` so the 10-19 minute dependency compile (DuckDB, SQLite, tauri, wry) is paid once per OS per lockfile change; `actions/setup-node@v7` with `cache: npm` + `cache-dependency-path: package-lock.json` (root, unlike the web repo's `frontend/`). Consider `mozilla-actions/sccache-action` for Windows, where MSVC compiles of DuckDB are the slowest leg.

7.5 DuckDB is the single biggest CI cost. Options, in order of leverage: (a) have `libduckdb-sys` link a prebuilt library in CI by downloading the matching `libduckdb-<os>.zip` release and setting `DUCKDB_LIB_DIR`/`DUCKDB_INCLUDE_DIR` while dropping `bundled` behind a feature flag (`duckdb = { version = "1.0", default-features = false }` + `[features] bundled-duckdb = ["duckdb/bundled"]`, default on for local dev); (b) keep `bundled` and rely on rust-cache (works, but every `Cargo.lock` bump re-pays ~10 min x 5 matrix legs); (c) `cargo chef`-style prebuilt dependency layer is not available on GitHub-hosted runners without containers.

7.6 `tauri-action@v0` on PRs builds without uploading (no `tagName`) so bundle failures surface before release; on `v*` tags the `release` job creates a draft GitHub release with all 5 platform bundles, mirroring Tauri's documented workflow. macOS runs twice (aarch64 + x86_64) instead of `universal-apple-darwin` so each half is testable; switch to universal if download count matters more than CI minutes.

7.7 Not mirrored from the web CI, deliberately: `commit-dist` (desktop ships bundles; `dist/` stays gitignored), Docker jobs, and the Python matrix. Kept: node 20/22/24 matrix for `frontend-test` (engines), `fail-fast: false`, artifact retention 7 days, Playwright chromium-only.

### Companion `.github/workflows/security.yml` (weekly Monday 02:00 UTC + dispatch, `permissions: security-events: write`)
- `rustsec/audit-check@v2` (or `cargo audit --json` via `taiki-e/install-action@cargo-audit`) and `cargo deny check advisories licenses bans` with an `EmbarkStudios/cargo-deny-action@v2`.
- `npm audit --omit=dev --audit-level=high --json > npm-audit.json || true`.
- `aquasecurity/trivy-action` `scan-type: fs`, `format: sarif`, scanning `package-lock.json` + `src-tauri/Cargo.lock`, then `github/codeql-action/upload-sarif@v4` (category `trivy-fs`), same stub-SARIF guard as the web workflow.
- `github/codeql-action` `init/analyze` for `javascript-typescript` (Rust CodeQL is still beta; skip).
- Upload all JSON/SARIF as `security-reports`, 30 days, every scanner `|| true` so findings land in Code Scanning without breaking the cron.

## 8. Testing strategy for the desktop port

Goal: the desktop app must prove it is a faithful port, not merely that it compiles. The web app has 360 `test_*.py` files and 64 API docs with request/response samples; most of that is reusable as oracle data. The strategy below is layered so every layer has a cheap, deterministic, CI-friendly form plus an opt-in live form.

### 8.1 Rust: broker mapping unit tests with recorded fixtures (per broker)

- Today: 0 tests in `brokers/{angel,fyers,zerodha}` (3,835 lines). All three implement `trait Broker` (`brokers/mod.rs`), so the test harness can be generic.
- Add `src-tauri/tests/fixtures/<broker>/{login,placeorder,orderbook,positions,holdings,funds,quotes,depth,history,symbol_master}.json` recorded once from the real broker API (scrub tokens). The web repo already ships broker test corpora (`openalgo/test/test_angel_holdings_mapping.py`, `test_aliceblue_*`, `test_upstox_tick_size.py`, `test_zerodha_quote_depth_qty.py`) whose embedded JSON is the same payload shape; lift them.
- Serve fixtures with `wiremock` (async, tokio-native, pairs with reqwest; prefer over `mockito` which is sync/thread-based) via `MockServer::start().await` and point the broker struct's `base_url` at it. This requires making the base URL a constructor parameter instead of a `const` in each `brokers/*/mod.rs` (check `angel/mod.rs:139-206` `#[allow(dead_code)]` constants).
- Assert the OpenAlgo-side normalized structs (`brokers/types.rs`) with `insta` snapshots (`insta::assert_json_snapshot!`), one snapshot per fixture; review diffs with `cargo insta review`. Snapshot the *request* bodies too (wiremock `received_requests()`), which is where exchange-code, product and price-scaling bugs live. This directly covers the `angel/mod.rs:1160-1188` identical-branch finding from clippy.
- Error paths: wiremock responses for 401 (token expired), 429 (rate limit), 5xx, malformed JSON, each asserting the `AppError` variant and that nothing is logged at `info` with a token in it (mirror `openalgo/test/test_broker_credential_logging.py` and `test_core_credential_logging.py` with `tracing-test` or a `tracing_subscriber` capture layer).

### 8.2 Rust: service tests with a mock `Broker`

- `services/*` (12 files, 40 pub fns, 0 tests) orchestrate `Broker` + `SqliteDb` + sandbox. Introduce `mockall` (`#[cfg_attr(test, automock)]` on `trait Broker`) and a `SqliteDb::in_memory()` constructor (rusqlite `Connection::open_in_memory` through the existing `r2d2` pool with `max_size(1)`), so each service test is: in-memory DB + `MockBroker` with `expect_place_order().returning(...)` + call the service + assert DB rows and returned struct.
- Priority order by risk: `order_service` (358 lines; the money path), `smart_order_service` (position-delta maths; port `openalgo/test/test_all_order_types_execution.py` cases as table-driven `rstest` cases), `position_service`/`orderbook_service` (aggregation), `options_service` (ITM/OTM strike selection at `:357-360`, Greeks; port `openalgo/test/README_OPTION_TESTS.md` and `test_option_greeks_api.py` expectations), `symbol_service`, `history_service`.
- Analyzer mode: `analyzer_service` must route to sandbox, never to a broker; a `MockBroker` with `.times(0)` expectations proves it (parity with `openalgo/test/test_analyzer_toggle_restriction.py`).

### 8.3 Rust: HTTP contract tests against the axum webhook server, golden fixtures from `openalgo/docs/api/**`

- `webhook/server.rs` builds an axum `Router` with 44 routes (`/api/v1/placeorder` ... `/api/v1/optionsmultiorder`, `/webhook/:id`, `/:broker/callback`). Expose `pub fn router(state) -> axum::Router` (today it is built inside `start()`), then test it in-process with `tower::ServiceExt::oneshot` (no port, no network) or with `axum_test::TestServer` for a friendlier API.
- Golden fixtures: a small build-time or `xtask` script extracts every "Sample API Request" / "Sample API Response" JSON block from `openalgo/docs/api/**/*.md` (64 files; `placeorder.md` alone has market and limit variants) into `src-tauri/tests/contracts/<endpoint>/<case>.{request,response}.json`. The contract test posts the request (with `apikey` swapped for a key created through `db/sqlite/api_keys.rs`) against the router backed by `MockBroker` or analyzer mode, and asserts: HTTP status, `status: "success"|"error"` field, exact key set of the response (not values like `orderid`), and the error envelope for a missing `apikey` / bad `exchange`. Validation rules come from `openalgo/restx_api/schemas.py`, `data_schemas.py`, `account_schema.py` (the README names them as source of truth); mirror each marshmallow `validate=` as a negative case.
- Reuse the web's live suite directly: `openalgo/test/test_all_order_types_execution.py`, `test_options_order_api.py`, `test_option_symbol_api.py`, `test_options_multiorder_api.py`, `test_option_greeks_api.py`, `test_broker_integration.py --base-url` all hard-code `BASE_URL = "http://127.0.0.1:5000"` and use `requests.post(...)`. Add an optional `desktop-contract` CI job that starts the desktop webhook server headless (needs a `--headless-server` flag or a tiny `src-tauri/src/bin/webhook-server.rs` that runs `WebhookServer` without a window, in analyzer mode, with a seeded API key), then runs those files with `BASE_URL` patched to the desktop port (`sed`/env). `test_broker_integration.py` already takes `--base-url`; propose the same env override for the others in the web repo. That makes the web suite the parity oracle for `04-api-parity`.
- Rate limiting: `webhook/rate_limiter.rs` has unit tests; add router-level tests that fire N+1 requests and assert the 429 body matches `openalgo/docs/api/rate-limiting.md`.
- Webhook ingestion (`/webhook/:webhook_id`, TradingView/Chartink payloads): fixtures from `openalgo/docs/api/strategy-services/webhook.md` plus `openalgo/test/test_flow_qa_regressions.py` payloads; assert strategy lookup, symbol mapping and that a disabled strategy returns the same error text as the web.

### 8.4 Rust: WebSocket protocol tests

- `websocket/manager.rs` (1,129 lines) speaks the broker feeds (binary ticks: `byteorder` parsing at `:790-837`) and emits Tauri events to the UI. Two layers:
  1. Parser unit tests: recorded binary frames per broker (Angel SmartAPI binary, Fyers binary/HSM, Zerodha KiteTicker modes LTP/quote/full) as `include_bytes!` fixtures; assert decoded tick structs with `insta` snapshots; `proptest` over frame lengths/garbage to prove no panic on truncated frames (relevant: 70 `.unwrap()` in the crate).
  2. Protocol tests: spin a fake broker WS server with `tokio-tungstenite` `accept_async` on an ephemeral `TcpListener`, drive subscribe/unsubscribe/heartbeat/reconnect (close the server socket, assert exponential backoff and re-subscription of the registry). Mirror `openalgo/test/test_websocket_unsubscribe_contract.py` and `test_mstock_websocket_resilience.py`. If the desktop later exposes the OpenAlgo WS API on 8765 (`docs/api/websocket-streaming/`), test the server side with a `tokio-tungstenite` `connect_async` client against the same auth/subscribe JSON that `openalgo/test/test_websocket.py` sends.
- Use `tokio::time::pause()` + `advance()` so reconnect/backoff tests run in milliseconds.

### 8.5 Rust: DB migration and persistence tests

- `db/sqlite/migrations.rs` (593 lines) and `db/duckdb/migrations.rs`: tests that (a) migrate an empty DB and snapshot `sqlite_master` schema with `insta`; (b) migrate a *populated* fixture DB from each prior app version (`tests/fixtures/db/v1.0.0.sqlite`, committed, small) and assert row counts and that encrypted credentials still decrypt; (c) migrations are idempotent (run twice); (d) `PRAGMA foreign_key_check` and `integrity_check` are clean. Use `tempfile::TempDir` (already a dev-dep) for file-backed cases, in-memory for speed.
- Per-module CRUD tests for `sandbox.rs` (523 lines, PnL maths; port `openalgo/test/sandbox/*` and `test_gthread_sandbox_{funds,orders}.py` cases), `settings.rs`, `market.rs` (holidays/timings; `openalgo/test/test_market_calendar_validation.py`), `symbol.rs` (100k-row batch insert performance guard with a `#[ignore]`d bench), the four log tables.
- DuckDB history store: write/read round trip on a `TempDir`, plus a `cfg(test)` feature that swaps DuckDB for an in-memory instance so service tests are not slowed by it.

### 8.6 Rust: property tests and fuzzing for parsing

- `proptest` for: OpenAlgo symbol <-> broker symbol round trips (`NIFTY28MAR24C22000` style option symbols, futures expiry formats, BSE/NSE/MCX exchange codes) in `services/symbol_service.rs` and each broker's mapper; price/quantity string parsing (`"1"`, `"0.05"`, lot multiples); `useParseIntRadix`-class issues in Rust equivalents; webhook payload parsing (`webhook/types.rs` 1,145 lines of serde structs - proptest `Arbitrary` derives via `proptest-derive`, assert `serde_json` round trip and no panic).
- `cargo fuzz` targets (nightly, run weekly in `security.yml`, not per PR) for the binary tick parsers and the webhook JSON entrypoints.

### 8.7 Rust test infrastructure and gates

- `cargo-nextest` as the runner (per-test process isolation, retries for the live-opt-in tests, JUnit for the Actions test summary), `rstest` for table-driven cases, `insta` for snapshots, `wiremock` for HTTP mocks, `mockall` for the `Broker` trait, `proptest`, `tokio::test` with paused time, `tracing-test` for log assertions, `assert_fs`/`tempfile` for files. Tag live-broker tests `#[ignore]` and run them only via `workflow_dispatch` with repository secrets.
- Coverage: `cargo llvm-cov nextest --lcov` in the `rust-coverage` job with `--fail-under-lines` as a ratchet: start at 20% once 8.1-8.3 land, raise by 10 points per milestone to 70%; upload `lcov.info` and, if desired, Codecov with `flags: rust`. Exclude `src-tauri/src/commands/*` thin wrappers only if they are truly one-liners; otherwise test them via `tauri::test::mock_builder()` (`tauri` has a `test` feature) which lets a command be invoked in-process with a managed `AppState`.

### 8.8 Frontend: unit, component and accessibility tests (Vitest)

- Fix 2.4 first, then extend: `src/api/tauri-client.ts` (737 lines) is the seam; mock `@tauri-apps/api/core` `invoke` with `vi.mock` (or `@tauri-apps/api/mocks` `mockIPC`/`mockWindows`, which is the official way) and assert every exported client function sends the right command name and args and maps errors into the UI error shape; a generated table from `cmds-registered.txt` (92 commands) can assert the client never calls an unregistered command (grep found 23 distinct names invoked today).
- Stores (`src/stores/*`, zustand) and hooks (`useMarketData`, `useSocket`, `useAutoLogout`) with `@testing-library/react` `renderHook` and `mockIPC` for `listen` events.
- Component tests for the shared UI (`components/ui/*`, already started) and for the top 10 pages by order risk (OrderBook, Positions, ActionCenter, Sandbox, BrokerSelect, Profile) using `src/test/test-utils.tsx` providers; a11y via `jest-axe` (`toHaveNoViolations`) in each page test using `src/test/a11y-utils.ts` `checkA11y`.
- Coverage: `@vitest/coverage-v8` is installed; set `coverage.thresholds` in `vitest.config.ts` (`lines/statements 60, branches 50, functions 60` as the first gate, `perFile: false`), with `coverage.include: ['src/**']` and exclude `src/test`, `*.d.ts`, `src/components/ui/*` shadcn primitives if preferred. Fails `frontend-test` when breached.
- Type-check tests (6.4) so a bad import fails `frontend-typecheck`, not only at runtime.

### 8.9 Frontend: Playwright E2E with a mocked invoke layer, plus visual parity

- Tauri's WebView is not drivable by Playwright, so E2E runs the React app in Chromium against `vite preview` (CI) or `vite dev` with `window.__TAURI_INTERNALS__` shimmed: a Playwright `addInitScript` that installs a fake `invoke(cmd, args)` dispatcher backed by a JSON fixture map (`e2e/fixtures/commands/<cmd>.json`) and a fake `listen` emitter. Auth flow, broker select, place order from the UI, order book refresh, analyzer toggle, settings persistence, logout. Chromium-only as in the web CI; `--project=chromium`.
- Real-shell smoke on each OS in `tauri-build`: after bundling, launch the built binary with `--headless-smoke` (exit 0 after `setup()` completes, DBs migrate, webhook server binds) under `xvfb-run` on Linux; this catches `keyring`, WebKitGTK and bundling failures that unit tests cannot. Tauri's WebDriver (`tauri-driver`) is Linux/Windows only and flaky; use it later, not as a gate.
- Visual parity against the web UI: the desktop frontend is a port of `openalgo/frontend` ("same theme, same components"). Run Playwright `toHaveScreenshot()` for the same routes on both apps (web via its `vite preview`, desktop via mocked preview), same viewport (1400x900) and both themes; store baselines under `e2e/__screenshots__/`, `maxDiffPixelRatio: 0.01`. Alternatively Storybook + Chromatic/Loki for component-level parity. Mark these `continue-on-error` initially as the web repo does for known debt.

### 8.10 Cross-platform CI matrix (incl. Raspberry Pi)

- `rust-test`: ubuntu-22.04, windows-latest, macos-latest (keyring: secret-service vs Credential Manager vs Keychain; path separators; SQLite WAL on each FS).
- `tauri-build`/`release`: add `ubuntu-22.04-arm` for `aarch64-unknown-linux-gnu` (Raspberry Pi 4/5 running 64-bit Raspberry Pi OS Bookworm; 22.04's glibc 2.35 <= Bookworm's 2.36 so the AppImage/deb runs). Native runner, no QEMU (QEMU made the web repo's Docker arm64 legs 10x slower). Optional later: `armv7-unknown-linux-gnueabihf` via `cross` for 32-bit Pi OS, and the macOS `universal-apple-darwin` target.
- Run the headless smoke (8.9) on every matrix leg so a Pi-only WebKitGTK or DuckDB-on-aarch64 failure is caught in CI, not by a user.
- Weekly cron also runs `cargo +nightly udeps` and `cargo msrv verify` so `rust-version` becomes truthful (6.2).

### 8.11 Reusing the web `test/` suite as the parity oracle

- Mechanically reusable now (black-box, `requests` + `BASE_URL`): `test_all_order_types_execution.py`, `test_options_order_api.py`, `test_option_symbol_api.py`, `test_options_multiorder_api.py`, `test_option_greeks_api.py`, `test_broker_integration.py --base-url`, `test_telegram_api_contract.py` (if/when Telegram ports), WebSocket: `test_websocket.py` (`WS_URL`). Run them from a `desktop-contract` job: `uv sync` in `openalgo/`, start the desktop headless server in analyzer mode on 5000 (or patch `BASE_URL`), `pytest <list> -o addopts="" --timeout=60`.
- Not reusable as-is (Flask `app.test_client()`, SQLAlchemy models, eventlet/gthread internals): `test_strategy_restx_api.py`, `test_gthread_*`, `test_auth_*`. Use them as *specifications*: each assertion becomes a Rust `rstest` case in 8.3.
- Keep the oracle honest with the web repo's own "every test ran" guard: parse the JUnit XML and fail if `tests == 0 or skipped > 0`.

### 8.12 Suggested sequencing

1. Unblock CI (section 2): fmt, clippy, biome --write, vitest config + jest-axe, commit Cargo.lock, align Tauri versions. Land `ci.yml` with `rust-coverage --fail-under-lines 0` so the pipeline is green on day one.
2. Router extraction + contract fixtures from `docs/api` (8.3) and `MockBroker` (8.2): biggest parity payoff per hour.
3. Broker fixtures + wiremock + insta (8.1), tick-parser proptests (8.4/8.6).
4. Migration fixtures (8.5), frontend `tauri-client` + store tests with `mockIPC` (8.8), coverage thresholds on both sides.
5. Playwright with the invoke shim, headless smoke on all bundles, Pi ARM leg (8.9/8.10), then visual parity.
