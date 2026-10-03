# 02 - Login / Auth Flow Audit (OpenAlgo Desktop, Tauri 2 + Rust + React)

Scope: `/Users/openalgo/openalgo-desktop/openalgo-desktop` (desktop) compared against the
read-only web reference `/Users/openalgo/openalgo-desktop/openalgo`. All paths below are
relative to those two roots unless absolute. No files were modified.

---

## (a) Current desktop flow, step by step (as implemented)

### Boot
1. `src/main.tsx` renders `<App/>`; `src/App.tsx` wraps everything in `Providers` (react-query +
   Toaster) -> `BrowserRouter` -> `AuthSync` -> lazy `Routes`.
2. `src/components/auth/AuthSync.tsx:24-43` calls `authStore.checkSession()` once, which fires
   three IPC calls in parallel (`check_session`, `get_current_user`, `get_broker_status`,
   `src/api/auth.ts:52-64`) and writes `user / isAuthenticated / brokerConnected` into the Zustand
   store. The store is `persist`ed to localStorage (`src/stores/authStore.ts:217-225`), so on a
   cold start the UI briefly believes the *previous* run's state until `checkSession` resolves
   (a spinner hides this). If any of the three IPC calls throws, `checkSession` returns `false`
   **without clearing state** (`authStore.ts:197-200`, `AuthSync.tsx:34-37`), so the stale
   persisted "logged in + broker" state survives.
3. Rust side: `src-tauri/src/state.rs:67-70` keeps `user_session` and `broker_session` in plain
   in-memory `RwLock<Option<..>>`. **Nothing restores either on startup.** Every app restart is a
   full logout, both for the OpenAlgo password session and for the broker token, even though the
   broker token *is* written encrypted to the `auth` table in `commands/broker.rs:71-76`.
4. The initial route is `/` which renders `src/pages/Home.tsx`, a marketing landing page with a
   "Login" link. There is no state-based redirect from `/`; the user must click.

### Setup vs login decision
5. There is no central guard. Each public page re-derives state itself:
   - `src/pages/Setup.tsx:75-91`: `check_setup` -> if `!needs_setup` go `/login`.
   - `src/pages/Login.tsx:57-98`: `check_setup` -> `/setup` if needed; else `check_session`;
     if true `get_broker_status` -> `/dashboard` if connected else `/broker`.
   - `Layout.tsx:13-20` / `FullWidthLayout.tsx:12-18` (protected routes): if
     `!isAuthenticated` -> `/login`; if `!user?.broker` -> `/broker`. These read **only the
     Zustand store**, not Rust.
   - `BrokerSelect.tsx` / `BrokerTOTP.tsx`: **no auth guard at all**; they rely on Rust
     `broker_login` rejecting with "User not authenticated" (`commands/broker.rs:48-50`).
6. `check_setup` (`commands/auth.rs:111-120`) = `users` table non-empty.

### Setup
7. `Setup.tsx:127-133` invokes `setup {username,email,password}`. Rust (`commands/auth.rs:171-218`)
   validates, `create_user` (`db/sqlite/user.rs:46-66`) stores only `username` + Argon2id
   peppered hash. **Email is discarded** (users table has no email column,
   `migrations.rs:81-87`), and no TOTP secret is generated, although the Setup page copy promises
   "You'll receive a TOTP QR code for password resets". An API key is auto-created. Then
   `navigate('/login')`.

### Login (password)
8. `Login.tsx:107-119`: invokes `login` directly via `invoke`; on success calls
   `setLogin(response.username, '')` -- this is `authStore.login(username, password)` **called with
   an empty password and not awaited**. That triggers a *second* Rust `login` IPC with
   password `''` which fails (`verify_user`), so the store sets `error` and never sets
   `user`/`isAuthenticated`. The page nevertheless `navigate('/broker')`.
   Result: Rust has a user session, Zustand has `user: null, isAuthenticated: false`.
9. Rust `login` (`commands/auth.rs:49-77`) only populates the in-memory `UserSession`.

### Broker selection / credentials
10. `BrokerSelect.tsx:120-136` loads `get_broker_config` (`commands/settings.rs:274-327`): a
    hard-coded list of 29 brokers with `auth_type` and `has_credentials` (from
    `configured_brokers` table). The Rust `BrokerRegistry` (`brokers/mod.rs:113-122`) implements
    only **angel, zerodha, fyers**; the other 26 are shown as selectable.
11. Credentials dialog (`BrokerSelect.tsx:596-690`): API key for all; API secret only for
    fyers/zerodha; Client ID only for angel. Saved via `save_broker_credentials`
    (`commands/settings.rs:79-110`) AES-GCM encrypted into `broker_credentials`
    (`migrations.rs:563-573`). Edit/Delete use `get_broker_credentials_for_edit` /
    `delete_broker_credentials` with `{ brokerId }` (correct camelCase arg).
12. Submit (`BrokerSelect.tsx:340-450`):
    - TOTP broker -> `navigate('/broker/<id>/totp')`.
    - OAuth broker -> requires `webhookConfig.enabled` (DB flag, not actual listener state),
      reads raw creds, builds `redirectUrl = http://<host>:<port>/<broker>/callback` (or ngrok),
      generates a random `state` (stored in React state, never verified), builds the broker URL
      (fyers `api-t1.fyers.in/api/v3/generate-authcode`, zerodha `kite.zerodha.com/connect/login`)
      and `open()`s the system browser. `isSubmitting` stays true until a callback event arrives.

### OAuth callback path
13. The axum "webhook" server (`src-tauri/src/webhook/server.rs:113`) exposes
    `GET /:broker/callback` -> `handlers.rs:2080-2167`. It accepts `code | auth_code |
    request_token`, emits Tauri event `oauth_callback {broker_id, success, auth_code}` to the
    webview, and returns an HTML "you can close this window" page. The server is only started in
    `lib.rs:52-70` **at boot** if `webhook_enabled` (migration `ENABLE_WEBHOOK_BY_DEFAULT` sets
    it to 1; default host `127.0.0.1`, port `5000`). Bind failure is only logged.
14. `BrokerSelect.tsx:140-240` listens for `oauth_callback`, re-reads creds and invokes
    `broker_login` with `{api_key, api_secret, client_id, auth_code}`. On success it force-sets
    the store (`useAuthStore.setState({... isAuthenticated: true, brokerConnected: true})`,
    with a fallback `username: 'user'` when `user` is null), navigates to `/dashboard`, and fires
    `refresh_symbol_master` in the background.
15. Rust `broker_login` (`commands/broker.rs:41-88`) calls the adapter's `authenticate`, stores
    the token encrypted in `auth` (keyed by broker_id, no user_id/user_name/issued-at), sets the
    in-memory `BrokerSession`.
    - Angel (`brokers/angel/mod.rs:348-416`): needs `client_id`, `password`(PIN), `totp`.
    - Zerodha (`brokers/zerodha/mod.rs:286-346`): needs `request_token` + `api_secret`.
    - Fyers (`brokers/fyers/mod.rs:494-573`): needs `auth_code` + `api_secret`.

### TOTP path
16. `BrokerTOTP.tsx:330-434`: dynamic form per broker; reads raw creds, maps fields into
    `{api_key, api_secret, client_id: stored||userid||mobile, password: password||pin||mpin,
    totp: totp||twofa||otp}`, invokes `broker_login`, force-sets the store the same way, goes to
    `/dashboard`.

### Dashboard / logout / auto-logout / reset
17. `Dashboard.tsx:94-135`: calls `get_funds`; on an error containing "not authenticated" or
    "No broker session" shows a "Session Expired" screen with a link to `/login`. (The real Rust
    message is "Broker not connected", `services/funds_service.rs:67`, so this branch never
    matches and the user sees "Failed to fetch margin data" instead.)
18. Logout (`components/layout/Navbar.tsx:28-37`): `authApi.logout()` then `store.logout()`
    (which calls `authApi.logout()` **again**), then `/login`. Rust `logout`
    (`commands/auth.rs:81-91`) clears both in-memory sessions but **does not revoke/delete the
    stored broker token**.
19. Auto-logout (`scheduler/auto_logout.rs`): background OS thread sleeps until configured time
    (default 03:00 IST), emits warnings, then `clear_all_auth_tokens` + clears broker session
    (keeps user session), emits `auto_logout`. `useAutoLogout.ts:95-116` then calls the
    *full* `logout()` and navigates to `/login` with `state.reason='auto_logout'`.
20. Password reset (`ResetPassword.tsx:131-200+`): still does `fetch('/auth/reset-password')`
    with CSRF -- a **web-only HTTP call that cannot succeed inside Tauri** (no Flask backend).
    Profile "Change Password" (`Profile.tsx:481-510`) posts to `webClient` which is a stub
    returning `{data:null}` (`src/api/client.ts:35-66`) -> `TypeError` -> "Failed to change
    password". `authApi.changePassword` throws "not yet implemented" (`src/api/auth.ts:148-151`).
    The only working recovery is `reset_user_data` from the Login page after 2 failed attempts
    (`Login.tsx:137-154`, `commands/auth.rs:151-167`) which deletes **all users** (but leaves
    broker credentials/tokens/api keys in place).

---

## (b) Concrete defects (numbered, with file:line and fix)

### Session / state model

1. **No session persistence across restarts** -- `src-tauri/src/state.rs:67-70, 119-120`.
   Both sessions live only in memory; `set_active_broker` (the only token-restore code,
   `commands/broker.rs:126-150`) is never invoked by the frontend
   (`grep set_active_broker src/` -> only the unused wrapper in `tauri-client.ts`).
   Fix: in `AppState::new` (or a new `restore_session` command called by `AuthSync`) read the
   `auth` row, check it is not revoked and `authenticated_at` is after the last 03:00 IST
   boundary, decrypt and populate `BrokerSession`. See target flow.

2. **Double login IPC with empty password / store never populated** -- `src/pages/Login.tsx:116`
   `setLogin(response.username, '')` where `setLogin = useAuthStore().login` (signature
   `(username, password)`, `authStore.ts:60-93`). Causes a second failing `login` call, leaves
   `user:null,isAuthenticated:false` in Zustand while Rust is authenticated. This is the actual
   root cause of the "redirect loop" commits (`e45e567`, `d45269f`): `Layout` bounced to
   `/login`, `Login`'s effect saw a live Rust session and bounced to `/dashboard`, forever.
   Fix: replace lines 105-122 with `const ok = await login(username, password)` (store action,
   one IPC) and navigate on `ok`; or keep the direct invoke and call
   `setUser({username, broker:null, brokerId:null, isLoggedIn:true, loginTime})`.
   Then remove the `useAuthStore.setState` fallbacks with `username: 'user'` in
   `BrokerSelect.tsx:170-187` and `BrokerTOTP.tsx:390-405`.

3. **Zustand is the route-guard source of truth but persists stale state** --
   `authStore.ts:217-225` persists `isAuthenticated/brokerConnected/user`; `Layout.tsx:13-20`
   trusts it. If `checkSession` throws, state is not cleared (`authStore.ts:197-200`).
   Fix: do not persist `isAuthenticated/brokerConnected` (persist nothing or only `username`
   for display); on `checkSession` error set `isAuthenticated:false`. Make `Layout` wait on a
   `sessionChecked` flag from `AuthSync` so Rust is the single source of truth.

4. **Guards are duplicated in pages instead of one place; `BrokerSelect`/`BrokerTOTP` have no
   guard** -- `BrokerSelect.tsx` and `BrokerTOTP.tsx` never check `isAuthenticated`; after a
   restart the user can open `/broker`, fill a TOTP and get "User not authenticated" from Rust
   (`commands/broker.rs:48-50`). `Home.tsx` (`/`) never redirects.
   Fix: add a `RequireUser` route element (user session) around `/broker/*`, a
   `RequireBroker` (user + broker) around the `Layout` routes, and a `RootRedirect` at `/`
   that goes to `/setup`, `/login`, `/broker` or `/dashboard` from a single `get_auth_state`
   IPC. Delete the per-page effects in `Login.tsx:57-98` and `Setup.tsx:75-91`.

5. **Store `user.broker` holds a *display name* in some paths and a *broker user id* in others**
   -- `authStore.checkSession` sets `broker: session.broker?.user_id` (`authStore.ts:177`),
   `authStore.brokerLogin` sets `response.user_name || response.user_id` (`:124`), while the
   pages set the display name (`BrokerSelect.tsx:173`, `BrokerTOTP.tsx:393`). `Layout` uses
   `!user?.broker` as "broker connected", so an empty `user_id` (as produced by
   `set_active_broker`, `commands/broker.rs:143`) would bounce to `/broker` while connected.
   Fix: guard on `brokerConnected`, store `brokerId` (`angel`) and `brokerUserId` separately,
   and have Rust return both.

### OAuth

6. **Zerodha OAuth login cannot succeed** -- the callback emits the Kite `request_token` in the
   `auth_code` field (`webhook/handlers.rs:2089-2098`), the frontend forwards it as
   `auth_code` (`BrokerSelect.tsx:160-166`), but the adapter demands
   `credentials.request_token` (`brokers/zerodha/mod.rs:287-289`) -> "Request token is
   required" every time.
   Fix: either map in `BrokerSelect` (`request_token: broker_id==='zerodha' ? code : null`)
   or, better, move the whole exchange into Rust (see target flow: the callback handler itself
   calls `broker_login`), so the frontend never touches tokens.

7. **OAuth `state` is generated but never verified** -- `BrokerSelect.tsx:385-387` creates
   `state`, the handler ignores `params.state` (`handlers.rs:2086`), the listener accepts any
   `oauth_callback` event and immediately attempts a login with it (`:140-150`). Any local
   process/browser tab hitting `http://127.0.0.1:5000/fyers/callback?auth_code=X` triggers a
   login attempt. Fix: keep `pending_oauth: {broker, state, expires}` in `AppState`
   (set by a new `start_oauth` command), have the handler drop callbacks whose `state` does
   not match (Fyers echoes it; for Zerodha use `redirect_params=state=...` or at least check
   the pending broker id), and clear it after use.

8. **Listener availability is never checked; macOS port 5000 conflict** -- `lib.rs:52-70`
   starts the axum server only at boot and only logs bind errors; `BrokerSelect.tsx:355-363`
   checks the DB flag `webhookConfig.enabled`, not whether the listener is actually bound.
   On macOS 12+ AirPlay Receiver already listens on 5000, so `Failed to bind` is common and
   the UI just spins on "Complete authentication in your browser".
   Fix: add `get_oauth_listener_status` (bound host:port or error) and call it before
   `open()`; surface the error with the exact remedy (change port in Settings or disable
   AirPlay Receiver); add `restart_webhook_server` so port changes do not require an app
   restart (`ServerSettings.tsx:61` currently tells users to restart).

9. **`isSubmitting` never resets if the browser flow is abandoned** -- `BrokerSelect.tsx:440`.
   Fix: timeout (e.g. 5 min) + "Cancel" button, driven by `pending_oauth` expiry.

10. **Listener effect re-registers on every `oauthState` change with async cleanup** --
    `BrokerSelect.tsx:120-240` depends on `[oauthState, navigate]`; `fetchBrokerConfig()` and
    `get_webhook_config` are re-run and the previous `listen` is removed only after a promise
    resolves, so two handlers can be live for a moment and double-invoke `broker_login`.
    Fix: register the listener once (`[]` deps) and read `state`/creds via refs or move the
    exchange into Rust (then the event only carries `{broker_id, success, message}`).

11. **Redirect URL uses `webhook_host` verbatim** -- `BrokerSelect.tsx:376-382`,
    `commands/settings.rs:315-318`. If the user binds `0.0.0.0` the displayed redirect URL
    becomes `http://0.0.0.0:5000/...`, which browsers will not navigate to.
    Fix: always display/use `127.0.0.1` (or `localhost`) for the loopback redirect regardless
    of bind address.

12. **Upstox/Dhan/other OAuth brokers are listed but have no adapter** --
    `commands/settings.rs:229-261` lists 29 brokers; `brokers/mod.rs:117-119` registers 3.
    `BrokerSelect.tsx:425-429` shows "OAuth not supported" only after the user has saved
    credentials and clicked. Fix: derive `available_brokers` from `BrokerRegistry::list()` and
    only show the implemented ones (or flag "coming soon" and disable).

### TOTP

13. **Angel Client ID is required in the credentials dialog *and* asked again on the TOTP
    form** -- `BrokerSelect.tsx:649-662` requires `clientId` for angel; `BrokerTOTP.tsx:81-97`
    asks `userid` again; `BrokerTOTP.tsx:368` prefers the stored one, so the typed value is
    silently ignored. Fix: pre-fill/lock the field from stored `client_id` (web asks only
    once on the TOTP form; store it on first success).

14. **Field mapping loses broker-specific fields** -- `BrokerTOTP.tsx:365-373` squashes
    everything into `client_id/password/totp`; Kotak needs `mobile`+`mpin`+`totp` plus UCC and
    access token as key/secret (web `broker/kotak/api/auth_api.py:13-30`), Tradejini a hidden
    `twofatype`, Motilal a `dob`. Fix: pass `extra: HashMap<String,String>` in
    `BrokerCredentials` (add `#[serde(default)]`) and let each adapter pick what it needs.

### Persistence / token lifecycle

15. **Stored broker token is never resumed and never revoked on logout** --
    `commands/auth.rs:81-91` (`logout`) leaves the `auth` row; `commands/broker.rs:92-103`
    (`broker_logout`) deletes it but is not called by the Navbar. Web revokes on logout
    (`blueprints/auth.py:1324`) and resumes on login after validating with a funds call
    (`blueprints/auth.py:189-275`). Fix: `logout` must delete/revoke the `auth` row; add
    `is_revoked`, `user_id`, `user_name`, `authenticated_at` columns to `auth`
    (`migrations.rs:91-100`) so resume can check the 03:00 IST boundary and show the account.

16. **Auto-logout is wall-clock only; no boundary check on resume or wake** --
    `scheduler/auto_logout.rs:116-170` sleeps in a std thread; if the machine is asleep over
    03:00 the logout fires late, and a token issued before the boundary but restored after it
    would be trusted. Fix: compute `token_valid = authenticated_at_ist >= last_boundary(03:00)`
    in one helper (`utils/session.py:57-94` is the reference) and apply it in
    `restore_session`, `get_broker_status`, and before every broker call.

17. **Auto-logout clears broker only, frontend then clears user too** --
    `auto_logout.rs:216-240` keeps the user session ("user stays logged in"); 
    `useAutoLogout.ts:105-106` calls full `logout()`. Harmless now, but contradicts the
    comment and the web behaviour (web revokes the token and clears the whole session,
    `utils/session.py:256-293`). Fix: pick one -- for MVP mirror web: `auto_logout` in Rust
    clears both and revokes the row; frontend only navigates.

### Account / password

18. **Email collected at setup and discarded; no TOTP secret** -- `commands/auth.rs:197-199`,
    `db/sqlite/user.rs:54-57`, `migrations.rs:81-87`. Setup copy
    (`Setup.tsx:194-197`) promises a TOTP QR. Fix: add `email`, `totp_secret_encrypted`,
    `totp_nonce` columns; generate a base32 secret in `setup`, return the `otpauth://` URI and
    render a QR in Setup (web `blueprints/core.py:50-57`).

19. **Password reset page is dead code in Tauri** -- `ResetPassword.tsx:131-140, 161-170,
    196-205, ~231`: `fetch('/auth/reset-password')` + `fetchCSRFToken()` stub. Fix: replace
    with `reset_password_totp {username_or_email, totp, new_password}` IPC (no e-mail step
    on desktop). Keep `reset_user_data` as the "I lost my authenticator" nuke, but make it
    also wipe `broker_credentials`, `configured_brokers`, `auth`, `api_keys` (it only runs
    `DELETE FROM users`, `db/sqlite/user.rs:75-78`).

20. **Change password is broken** -- `Profile.tsx:481-510` uses the `webClient` stub.
    Fix: add `change_password {old_password,new_password}` IPC; on success clear sessions
    and route to `/login` (web does this, `blueprints/auth.py:1456-1465`).

### Smaller correctness issues

21. **Wrong argument casing in two helper wrappers** -- `src/api/tauri-client.ts:605-606`
    `setActiveBroker(... { broker_id })` and `:641-642` `deleteBrokerCredentials(... {
    broker_id })`. Tauri 2 expects `brokerId` (no `rename_all` on the commands,
    `grep rename_all src-tauri/src/commands` -> none). Unused today, but will fail when used.
    Fix: `{ brokerId }`.

22. **Dashboard auth-error match never fires** -- `Dashboard.tsx:127` matches
    "not authenticated" / "No broker session"; Rust emits "Broker not connected"
    (`services/funds_service.rs:67,73`) or "User not authenticated". Fix: match on the error
    `code` (`AUTH_ERROR`, `error.rs:69`) instead of message text -- but `tauriInvoke`
    (`tauri-client.ts:~520`) throws `new Error(message)` and discards `code`; keep the code on
    the thrown error.

23. **Duplicate logout IPC** -- `Navbar.tsx:30-31` calls `authApi.logout()` then
    `logout()` which calls it again. Fix: call only the store action.

24. **`get_broker_config` exposes the configured API key (masked) and the frontend holds raw
    secrets** -- `commands/settings.rs:412-438` `get_broker_credentials_for_edit` returns the
    plaintext secret to the webview and `BrokerSelect`/`BrokerTOTP` pass it back into
    `broker_login`. Not a bug per se, but unnecessary surface. Fix (target flow): `broker_login`
    takes only `{broker_id, form_fields}` and reads credentials server-side.

25. **Setup page copy and Login page copy are web-isms** -- `Login.tsx:295-301`
    "Contact your administrator", `Setup.tsx:186-188` "administrator account ... manage the
    platform". Replace with single-user wording.

---

## Per-broker requirements (web reference) and what the desktop must collect

| Broker | Web .env inputs | User enters at login | Callback / exchange (web) | Desktop status |
|---|---|---|---|---|
| angel | `BROKER_API_KEY` | Client ID, PIN, TOTP (`brlogin.py:87-99`) | POST form, `loginByPassword` with `X-PrivateKey` (`broker/angel/api/auth_api.py`) | Adapter OK; UI asks client id twice (#13) |
| zerodha | `BROKER_API_KEY`, `BROKER_API_SECRET` | none (browser login) | `https://kite.trade/connect/login?api_key=..` (redirect URL fixed in Kite app) -> `GET /zerodha/callback?request_token=..&action=login&status=success` -> `session/token` with sha256(api_key+request_token+secret); token stored as `api_key:access_token` (`brlogin.py:1031-1041`) | Broken (#6) |
| fyers | `BROKER_API_KEY` = `APPID-100`, `BROKER_API_SECRET` | none | `generate-authcode?client_id=..&redirect_uri=..&response_type=code&state=..` -> `GET /fyers/callback?auth_code=..&state=..` -> `validate-authcode` with `appIdHash = sha256(appid:secret)` (`broker/fyers/api/auth_api.py:49-68`) | Works if listener bound |
| upstox | `BROKER_API_KEY`, `BROKER_API_SECRET`, `REDIRECT_URL` | none | `api.upstox.com/v2/login/authorization/dialog?...redirect_uri=..` -> `GET /upstox/callback?code=..` -> token exchange **must send the same `redirect_uri`** (`broker/upstox/api/auth_api.py:22-31`) | No adapter |
| dhan | `BROKER_API_KEY` = `client_id:::api_key`, `BROKER_API_SECRET` | none | `/dhan/initiate-oauth` -> `generate-consent?client_id=` -> `auth.dhan.co/login/consentApp-login?consentAppId=` -> `GET /dhan/callback?tokenId=..` -> `consumeApp-consent` (`broker/dhan/api/auth_api.py`, `brlogin.py:384-495,1078+`) | No adapter |
| kotak | `BROKER_API_KEY` = UCC, `BROKER_API_SECRET` = access token | Mobile, MPIN, TOTP (`brlogin.py:758-784`) | two POSTs `tradeApiLogin` then `tradeApiValidate`; token `trading_token:::sid:::base_url:::access_token` | No adapter; desktop form fields exist |

Redirect URL convention in web: `.sample.env:18` `REDIRECT_URL='http://127.0.0.1:5000/<broker>/callback'`
with `FLASK_HOST_IP=127.0.0.1`, `FLASK_PORT=5000`. The desktop default
(`webhook_host=127.0.0.1`, `webhook_port=5000`, `migrations.rs:ADD_WEBHOOK_SETTINGS`) already
produces the identical URL, so **existing broker app registrations can be reused unchanged**.
Keep that as the documented default; make the port editable with a live listener-status check.

Web behaviours worth copying exactly:
- Token persistence: `database/auth_db.py:602-733` `upsert_auth(name, token, broker, feed_token,
  user_id, revoke)` keyed by username, Fernet-encrypted, `is_revoked` flag.
- Resume: `blueprints/auth.py:189-275` after password login, if row exists and not revoked,
  validate with a cheap funds call, then treat as logged in (`redirect:/dashboard`).
- Expiry: `utils/session.py:57-94` valid iff `not (now_ist > today_03:00 and login_time <
  today_03:00)`; on invalid, `revoke_user_tokens()` revokes the DB row.
- Logout: `blueprints/auth.py:1287-1354` revokes the row and clears everything.
- Broker page: `blueprints/auth.py:553-577` bounce to dashboard only when the stored token is
  still valid, otherwise allow re-auth (issue #1400).

---

## (c) Target MVP flow and exact changes

Single user. Rust is the only source of truth; React never holds tokens or secrets.

### State machine (returned by one IPC `get_auth_state`)
```
needs_setup -> /setup
no_user_session -> /login
user_session && !broker_session -> /broker
user_session && broker_session(valid until next 03:00 IST) -> /dashboard
```

### Flow
1. **Boot**: `AppState::new` loads `app_session` (see below) and `auth` row; if the saved
   broker token's `authenticated_at` is after the last 03:00 IST boundary and `is_revoked=0`,
   populate `BrokerSession` and optionally validate once with `get_funds` in the background
   (web does this on login, not boot). `AuthSync` calls `get_auth_state` and routes.
2. **Setup once**: username, email, password; generate TOTP secret; show QR + manual key;
   create default API key. -> `/login`.
3. **Login**: password only. On success Rust creates the user session, then runs the same
   resume check as (1) and returns `{next: 'broker' | 'dashboard', broker}`.
4. **Broker page**: list only brokers from `BrokerRegistry`. Per broker, a credentials card
   (API key / secret / client id as required by that adapter, defined by the adapter via a
   `credential_fields()` method) stored encrypted in `broker_credentials`. A "Connect" button:
   - TOTP adapters -> in-app form whose fields also come from the adapter
     (`login_fields()`), posted to `broker_login`.
   - OAuth adapters -> `start_oauth` returns the URL and starts/verifies the loopback listener;
     frontend opens the browser and waits for `broker_connected` event.
5. **OAuth callback**: `GET http://127.0.0.1:<port>/<broker>/callback` handled fully in Rust:
   verify `state` against `pending_oauth`, call `broker.authenticate(...)` with stored creds
   (Zerodha: `request_token`; Fyers: `auth_code`; Upstox later: `code` + redirect_uri),
   persist token, set session, emit `broker_connected {broker_id, user_id, user_name}`, render
   the "return to app" page. Also provide a **manual fallback**: a "Paste redirected URL"
   field on the broker page that calls `complete_oauth_manual {url}` with the same handler,
   for users whose port is blocked.
6. **Dashboard**: `RequireBroker` guard (Rust state), master-contract download kicked by Rust
   after `broker_login`, not by the page.
7. **Logout**: `logout` clears both sessions, revokes `auth` row, deletes `app_session`. 
   `broker_logout` only revokes the broker part and returns to `/broker`.
8. **03:00 IST**: scheduler (keep) + boundary check in `get_auth_state` and before broker
   calls; on expiry revoke the row, clear broker session, emit `auto_logout`; frontend routes to
   `/broker` (user stays logged in) or `/login` -- pick one and make Rust do it (recommend:
   broker-only, matching the existing scheduler comment, since the app-password session is
   local).
9. **Password reset**: `/reset-password` = username + TOTP + new password (IPC). Change
   password in Profile (IPC) invalidates the session and returns to `/login`.
   "Reset account" (existing `reset_user_data`) stays as last resort but wipes all tables.

### Rust: schema changes (new migrations in `src-tauri/src/db/sqlite/migrations.rs`)
- `users`: add `email TEXT`, `totp_secret_encrypted TEXT`, `totp_nonce TEXT`.
- `auth`: add `user_id TEXT`, `user_name TEXT`, `authenticated_at TEXT NOT NULL`,
  `is_revoked INTEGER NOT NULL DEFAULT 0`.
- new `app_session (id INTEGER PRIMARY KEY CHECK(id=1), user_id INTEGER, authenticated_at TEXT)`
  -- lets the password session survive restarts (single machine, encrypted data dir; if you
  prefer re-entering the password on every launch, skip this table and only resume the broker
  token after login, exactly like web).
- `broker_credentials`: add `extra_encrypted TEXT, extra_nonce TEXT` (JSON map) for brokers
  needing more than key/secret/client_id (Dhan client id, Kotak UCC, etc.).

### Rust: Tauri commands (final list; keep names where they exist)
Auth (`commands/auth.rs`):
- `check_setup` (keep)
- `setup {username,email,password}` -> `{status, totp_uri, totp_secret}` (generate secret, store encrypted)
- `login {username,password}` -> `{success, username, next: "broker"|"dashboard", broker?: BrokerStatus}` (runs resume)
- `logout` -> clears user+broker session, revokes `auth`, deletes `app_session`
- `get_auth_state` -> `{needs_setup, user: {username}|null, broker: BrokerStatus|null, next_route}` (replaces `check_session`+`get_current_user`+`get_broker_status` triple)
- `change_password {old_password,new_password}`
- `reset_password_totp {username,totp,new_password}`
- `reset_user_data` (keep; extend to wipe `auth`, `broker_credentials`, `configured_brokers`, `api_keys`)
- remove from the handler list: `check_session`, `get_current_user` (fold into `get_auth_state`)

Broker (`commands/broker.rs`):
- `get_available_brokers` -> from `BrokerRegistry`, each `{id, name, auth_type, credential_fields[], login_fields[], has_credentials}` (replaces the hard-coded list in `settings.rs:229-261` and `get_broker_config`)
- `save_broker_credentials {broker_id, fields: map}` / `get_broker_credentials {broker_id}` (masked) / `delete_broker_credentials {broker_id}` (keep; drop `get_raw_broker_credentials` and `get_broker_credentials_for_edit` from the frontend path -- Rust reads creds itself)
- `broker_login {broker_id, fields: map}` (TOTP path) -> `{success, broker_id, user_id, user_name}`; persists `auth` with `authenticated_at`, `user_id`, `user_name`; kicks `refresh_symbol_master` in the background
- `start_oauth {broker_id}` -> `{auth_url, redirect_url, state}`; stores `pending_oauth` in `AppState`; fails fast with a clear error if the listener is not bound
- `complete_oauth_manual {url}` -> same handler as the HTTP callback (fallback)
- `broker_logout` -> revoke row + clear broker session
- `get_broker_status` (keep; add `user_name`, `expires_at`)
- `get_oauth_listener_status` -> `{bound: bool, host, port, error?}`
- `restart_webhook_server` (re-reads config, rebinds; used by ServerSettings)

Adapters (`brokers/mod.rs` trait):
- add `fn auth_type() -> AuthType {Totp|OAuth}`, `fn credential_fields() -> &[FieldSpec]`,
  `fn login_fields() -> &[FieldSpec]`, `fn build_oauth_url(&self, creds, redirect_url, state) -> String`,
  and change `authenticate(&self, creds: &StoredCredentials, input: &LoginInput)` where
  `LoginInput` carries `totp/password/client_id/request_token/auth_code/extra`.
- `brokers/zerodha`: accept `request_token` from `LoginInput`; `brokers/fyers`: `auth_code`.

HTTP callback (`webhook/handlers.rs:2080`): verify `state`/pending broker, run the exchange in
Rust, emit `broker_connected` or `broker_auth_failed`, render result page. Drop the
`oauth_callback`-with-code event.

Scheduler (`scheduler/auto_logout.rs`): add `fn is_token_fresh(authenticated_at) -> bool`
(IST boundary) in a shared `session` module used by `get_auth_state`, resume, and the
scheduler; on fire, revoke the `auth` row (`UPDATE auth SET is_revoked=1`) rather than
`DELETE`, so `get_auth_state` can tell "expired" from "never logged in".

### React changes
- `src/App.tsx`: add `<Route path="/" element={<RootRedirect/>}/>`; wrap `/broker`,
  `/broker/:broker/totp` in `<RequireUser/>`; keep `Layout`/`FullWidthLayout` as
  `RequireBroker`. Remove the `/:broker/auth` catch-all route (`App.tsx:111`) -- it shadows
  nothing useful and can swallow typos.
- `src/components/auth/AuthSync.tsx`: call `get_auth_state`; expose `ready` flag; guards render
  spinner until `ready`. Keep `useAutoLogout`.
- `src/stores/authStore.ts`: state = `{ready, needsSetup, user, broker}`; actions
  `refresh()`, `login()`, `logout()`, `brokerLogout()`; **no `persist`**; delete `apiKey`,
  `brokerLogin`, `refreshSession`, `initializeAuth`.
- `src/api/tauri-client.ts` / `src/api/auth.ts`: wrappers for the commands above; fix camelCase
  args (#21); keep `code` on thrown errors (#22); delete `changePassword` stub and the
  `webClient`/`fetchCSRFToken` stubs in `src/api/client.ts` once Profile/ResetPassword are
  ported.
- `src/pages/Login.tsx`: form only; `await login()`; route on `next`. Remove the setup/session
  effect (handled by `RootRedirect`/guards). Keep the auto-logout banner and the
  "Reset account" escape hatch; add "Forgot password" -> `/reset-password`.
- `src/pages/Setup.tsx`: after success show TOTP QR (`qrcode.react` or render the
  `otpauth://` URI as SVG) with a "I've saved this" confirmation before `/login`.
- `src/pages/BrokerSelect.tsx`: render brokers + field specs from `get_available_brokers`;
  credentials dialog generated from `credential_fields`; "Connect" -> TOTP route or
  `start_oauth` + `open(auth_url)`; listen for `broker_connected`/`broker_auth_failed`; show
  listener status and the redirect URL to register; manual-paste fallback; cancel/timeout.
  Remove all `useAuthStore.setState` calls -- call `refresh()` instead.
- `src/pages/BrokerTOTP.tsx`: form from `login_fields`; invoke `broker_login {broker_id,
  fields}`; on success `refresh()` then `/dashboard`. Delete the 240-line hard-coded
  `brokerFields` map.
- `src/pages/ResetPassword.tsx`: single step username + TOTP + new password via IPC.
- `src/pages/Profile.tsx:481-510`: `change_password` IPC; on success `logout()` -> `/login`.
- `src/pages/Dashboard.tsx:127-131`: drop the message sniffing; rely on the guard.
- `src/components/layout/Navbar.tsx:28-37`: call the store `logout()` once.
- `src/pages/admin/ServerSettings.tsx`: after save call `restart_webhook_server` and show
  `get_oauth_listener_status`.

### Order of work (smallest fixes first, each independently shippable)
1. #2 (Login.tsx one-line fix) + remove the `setState` hacks -> stops the redirect loop at the root.
2. #6 Zerodha `request_token` mapping (3 lines) + #21 arg casing.
3. #8/#9 listener status command + UI message + cancel/timeout.
4. Token resume + revoke on logout + boundary check (#1, #15, #16) -- the biggest UX win.
5. Guards/RootRedirect refactor (#3, #4, #5).
6. Password change / TOTP reset / setup email (#18-#20).
7. Adapter-driven field specs and trimming the broker list (#12-#14), then Upstox/Dhan/Kotak adapters.
