# Spike 4 findings: staying signed in across restarts (2026-10-25)

Code: this folder. It is a cargo workspace with `common` (DPAPI store, Set-Cookie merge, redacting log), `login` (Tauri 2.12, identifier `com.ziggy.icloud-notes-spike4`) and `probe` (plain reqwest 0.12, no webview).
The redacted logs are in `out/report-{login,validate,refresh,silent,matrix}.txt`. They contain names, lengths, 8-hex SHA-256 fingerprints and expiries only.
Secrets were stored outside the repo, in `%LOCALAPPDATA%\com.ziggy.icloud-notes-spike4\` (`*.dpapi` and the `EBWebView` profile). That folder was deleted after the spike, so re-running `probe` now needs a fresh `login`.
The sign-in used "Keep me signed in", and `/validate` reported `isExtendedLogin=true`, `hsaTrustedBrowser=true` and `hsaChallengeRequired=false`.
Only two calls were made: `POST setup/ws/1/validate` and CloudKit `changes/zone` (resultsLimit 1). Both are read-only.

## TL;DR
- **Q1: yes.** Cookies captured from the webview, saved with DPAPI (CurrentUser scope plus app entropy, a 5.6 KB blob) and loaded in a fresh process with no webview give `validate` **200**. CloudKit `changes/zone` also returns **200**.
- **Q2: yes, with caveats.** Every validate re-issues `X-APPLE-WEBAUTH-TOKEN` (new value) and `X-APPLE-WEBAUTH-VALIDATE` (new value, session cookie). It also re-sends the 9 `PCS-*` cookies and `DS-WEB-SESSION-TOKEN` with the same values. All of them come back with a fresh **+30 days** Expires, so the cookie-side expiry slides. **CloudKit calls also re-issue TOKEN.** Old values are **not** invalidated: the original jar A still validated and read CloudKit after B and C had been issued. Whether the *server-side* session also slides past 30 days can't be seen in one day (see "Still open").
- **Q3: yes.** After a full app restart, the persisted WebView2 profile signs in silently. The page re-validated by itself 1.1 s after launch (TOKEN rotated), and reqwest validate with the webview's cookies returned 200. The profile only keeps persistent cookies: `LOGIN`, `VALIDATE`, `UNIQUE-CLIENT-ID` and `x-apple-group` are dropped on close, and the page sets `VALIDATE` and `x-apple-group` again but never `LOGIN`. The webview still had the *original* TOKEN, which reqwest had refreshed several times by then, and it was accepted. So the webview profile and a separate reqwest jar can coexist.
- **Q4: only two cookies identify the session: `X-APPLE-WEBAUTH-TOKEN` and `X-APPLE-WEBAUTH-USER`.** CloudKit also needs a PCS cookie. Dropping or corrupting anything else (LOGIN, HSA-TRUST, DS-WEB-SESSION-TOKEN, KB, WEB-ID, UNIQUE-CLIENT-ID, x-apple-group, VALIDATE) changes nothing. The full error table is below.
- **Q5:** expiries are listed below. They match the brief, plus `X_APPLE_WEB_KB-*` at about 60 days and `X-APPLE-WEB-ID` at about 14 days.

## Cookie set and observed expiries (at sign-in)
| Cookie | Expires | HttpOnly | Needed? |
|---|---|---|---|
| X-APPLE-WEBAUTH-TOKEN | +30 d (re-issued on every validate *and* CloudKit call) | yes | **yes**: the session |
| X-APPLE-WEBAUTH-USER | +30 d (not re-issued) | yes | **yes** |
| X-APPLE-WEBAUTH-PCS-{Cloudkit,Documents,Events,Mail,News,Notes,Photos,Safari,Sharing} | +30 d (re-issued, same value) | yes | CloudKit needs a valid PCS cookie (see below) |
| X-APPLE-DS-WEB-SESSION-TOKEN (882 B) | +30 d (re-issued, same value) | yes | no (dropped or corrupted → still 200/200) |
| X-APPLE-WEBAUTH-HSA-TRUST | +90 d | yes | no for validate/CloudKit; it is the "trusted browser" token that skips 2FA on the *next sign-in* |
| X_APPLE_WEB_KB-&lt;hash&gt; | +60 d | yes | no |
| X-APPLE-WEB-ID | +14 d (the page turned it into a session cookie on reload) | no | no |
| X-APPLE-WEBAUTH-LOGIN | session | yes | no |
| X-APPLE-WEBAUTH-VALIDATE | session (new value on each validate) | no | no |
| X-APPLE-UNIQUE-CLIENT-ID | session | no | no |
| x-apple-group | session | yes | no |

PCS detail: dropping any single PCS cookie is fine, and USER+TOKEN+PCS-Documents is a working minimal set. USER+TOKEN+PCS-Cloudkit (plus all non-PCS cookies) gets 403. Corrupting PCS-Documents, PCS-Notes or PCS-Sharing gives 401 `could not decrypt pcs cookie`. So CloudKit for `com.apple.notes` evidently checks the Notes/Documents/Sharing PCS cookies and needs at least one valid one. This was not narrowed further. **The app should just send every cookie it has.**

## What failures look like (to decide when to show the login window)
| Situation | `POST /validate` | CloudKit `changes/zone` |
|---|---|---|
| No cookies / TOKEN missing | **421**, `success=false`, `error="Missing X-APPLE-WEBAUTH-TOKEN cookie"` | **401**, `serverErrorCode="AUTHENTICATION_FAILED"`, `reason="no auth method found"` |
| TOKEN invalid (corrupted, the closest thing to expired/revoked) | **421**, `success=false`, `error=1` | **421**, `error=2`, `reason="Invalid global session"` |
| USER missing / invalid | **421**, `error="Missing/Invalid X-APPLE-WEBAUTH-USER cookie"` | **421**, `error=1`, same reason text |
| No `Origin` header | **421**, `error="Invalid or missing Origin header"` | (still 200) |
| PCS cookie bad | 200 | **401**, `serverErrorCode="INTERNAL_ERROR"`, `reason="could not decrypt pcs cookie"` |
| No usable PCS cookie | 200 | **403**, `serverErrorCode="ACCESS_DENIED"`, `reason="private db access disabled for this account"` |
| No User-Agent header | 200 | 200 (UA is not required) |

- Validate's error bodies have keys `configBag, error, requestInfo, success, trustTokens`. **trustTokens is sensitive**, so never log the body. CloudKit error bodies are `{reason, serverErrorCode, uuid}` or `{error, reason}`.
- Note that 421 is also what a *missing Origin* returns, which is a bug on our side, not an expired session. Tell them apart by the `error` text, or just make sure Origin is always sent.
- Sending ~90 requests with broken cookies didn't hurt the real session: validate with the base jar was still 200 at the end of the matrix.
- A **real** server-side expiry was not observed. It is assumed to look like the corrupted-TOKEN row, but that is not proven.

## Recommended session strategy for the real app
1. **The source of truth is our own DPAPI-encrypted cookie jar, not the WebView2 profile.** Store all cookies from `cookies_for_url("https://setup.icloud.com/setup/ws/1/validate")` as JSON (name, value, domain, path, expires, flags). Encrypt it with `CryptProtectData` (CurrentUser, `CRYPTPROTECT_UI_FORBIDDEN`, app-specific entropy) into `%LOCALAPPDATA%\<app>\session.dpapi`. Keep every cookie, even the ones that turned out not to be needed, because Apple may start checking them.
2. **Startup:** load the jar → `POST /validate` (always with `Origin: https://www.icloud.com`, `Referer`, a browser UA) → **merge the Set-Cookie headers** → save the jar. If you get 200 with `webservices.ckdatabasews`, carry on with no window at all.
3. **Persist after every response**, CloudKit included: TOKEN is re-issued on every call. A small cookie-store type that merges Set-Cookie by name is enough (it's ~60 lines in `common`). `reqwest_cookie_store`/`cookie_store` is an option if you want path/domain matching, but all cookies here are `icloud.com` + `/`. Atomic write: tmp file + rename. Old values keep working, so a lost write is not fatal.
4. **Re-auth trigger:** validate returning 421/401, or CloudKit returning 401 `AUTHENTICATION_FAILED` or 421 `Invalid global session`. Then show the login window, which **silently re-signs-in if the WebView2 profile is still valid** (Q3), so in that case the user may not even need to type anything. When `X-APPLE-WEBAUTH-TOKEN` appears in the webview and a reqwest validate succeeds with those cookies, replace the jar, close the window, and resume. Handle 401 `could not decrypt pcs cookie` / 403 `ACCESS_DENIED` the same way, since PCS cookies come from sign-in.
5. **Keep the WebView2 profile** (default persistent data dir, keyed by the identifier). It is the fallback that turns most re-logins into a 1-second silent reload, and it holds HSA-TRUST (90 d), so a real re-login usually skips 2FA. Never use incognito mode for the login window.
6. **Keep-alive (optional, cheap):** if the app is opened at least every few weeks, steps 2–3 keep sliding TOKEN's 30-day cookie expiry. If it hasn't run for more than 30 days, expect a re-login, probably silent via the profile until HSA-TRUST and the server session lapse.
7. **Sign out** = delete `session.dpapi` + clear the WebView2 profile (`clear_all_browsing_data`). This is local only, so the app stays read-only towards Apple.

## Still open
- **The real server-side lifetime: deliberately not pursued** (the user chose not to wait weeks before building). It doesn't block the design. Steps 2–4 above work whether or not refreshing extends the session past 30 days. If it doesn't, the user just sees the login window about once a month, and it often signs in silently via the profile. The real app will reveal this on its own: log (redacted) the date of the first sign-in and of every re-auth trigger.
- Behaviour after a Windows reboot / sign-out (DPAPI is fine across reboots by design; profile persistence was tested across app restarts only).
- Exactly which PCS cookie(s) Notes CloudKit requires (it doesn't matter if all of them are sent).
