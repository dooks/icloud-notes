// Spike 4 login: sign in to icloud.com in WebView2 (own profile, own identifier) and
// save the cookies with DPAPI. `--silent-check` reopens the same profile without the
// user doing anything and checks whether the session came back by itself.
//
// Privacy: logs cookie names, lengths, fingerprints and expiries only.

// Debug builds keep the console so the log is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    thread,
    time::{Duration, Instant},
};

use common::{Jar, Log, StoredCookie, VALIDATE};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder, webview::Cookie};

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SILENT_WAIT: Duration = Duration::from_secs(25);

fn main() {
    let silent = std::env::args().any(|a| a == "--silent-check");
    tauri::Builder::default()
        .setup(move |app| {
            let title = if silent {
                "spike4 silent check: don't touch, closes by itself"
            } else {
                "spike4: sign in to iCloud (tick Keep me signed in)"
            };
            let win = WebviewWindowBuilder::new(app, "login", WebviewUrl::External(common::ICLOUD.parse()?))
                .title(title)
                .inner_size(1000.0, 800.0)
                .build()?;
            let handle = app.handle().clone();
            // Off the main thread: WebView2 cookie calls deadlock if made from it synchronously.
            thread::spawn(move || if silent { silent_check(handle, win) } else { sign_in(handle, win) });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn grab(win: &WebviewWindow) -> Option<Vec<Cookie<'static>>> {
    win.cookies_for_url(VALIDATE.parse().unwrap()).ok()
}

fn to_jar(cookies: &[Cookie<'static>], source: &str) -> Jar {
    Jar {
        saved_at: common::now(),
        source: source.into(),
        cookies: cookies
            .iter()
            .map(|c| StoredCookie {
                name: c.name().to_owned(),
                value: c.value().to_owned(),
                domain: c.domain().map(str::to_owned),
                path: c.path().map(str::to_owned),
                expires: c.expires_datetime().map(|d| d.unix_timestamp()),
                http_only: c.http_only(),
                secure: c.secure(),
                same_site: c.same_site().map(|s| s.to_string()),
            })
            .collect(),
    }
}

fn has_token(cookies: &[Cookie<'static>]) -> bool {
    cookies.iter().any(|c| c.name() == "X-APPLE-WEBAUTH-TOKEN")
}

fn other_domains(log: &mut Log, win: &WebviewWindow) {
    if let Ok(all) = win.cookies() {
        let mut names: Vec<_> = all
            .iter()
            .filter(|c| c.domain() != Some("icloud.com"))
            .map(|c| format!("{} @ {}", c.name(), c.domain().unwrap_or("?")))
            .collect();
        names.sort();
        log.line(format!("cookies() total {}, outside icloud.com: {names:?}", all.len()));
    }
}

fn sign_in(app: AppHandle, win: WebviewWindow) {
    let mut log = Log::new("login");
    let start = Instant::now();
    log.line("waiting for X-APPLE-WEBAUTH-TOKEN (sign in in the window)...");
    loop {
        thread::sleep(Duration::from_secs(2));
        if start.elapsed() > SIGN_IN_TIMEOUT {
            log.line("TIMEOUT: no auth cookie within 15 min");
            return app.exit(1);
        }
        if grab(&win).is_some_and(|c| has_token(&c)) {
            break;
        }
    }
    log.line(format!("auth cookie seen after {}s; waiting 10s for the page to settle", start.elapsed().as_secs()));
    thread::sleep(Duration::from_secs(10));
    let Some(cookies) = grab(&win) else {
        log.line("cookies_for_url failed after settle");
        return app.exit(1);
    };
    let jar = to_jar(&cookies, "webview sign-in");
    jar.describe(&mut log, "captured from webview");
    other_domains(&mut log, &win);
    let path = common::store_path("session");
    match common::save(&path, &jar) {
        Ok(n) => log.line(format!("saved DPAPI blob ({n} bytes) to {}", path.display())),
        Err(e) => log.line(format!("SAVE FAILED: {e}")),
    }
    // Keep an untouched copy for comparisons after probes refresh session.dpapi.
    let _ = common::save(&common::store_path("session.orig"), &jar);
    app.exit(0);
}

fn silent_check(app: AppHandle, win: WebviewWindow) {
    let mut log = Log::new("silent");
    let start = Instant::now();

    let before = grab(&win).map(|c| to_jar(&c, "profile at launch")).unwrap_or_default();
    before.describe(&mut log, "cookies in the profile right after launch");
    let token0 = before.get("X-APPLE-WEBAUTH-TOKEN").map(|c| common::fp(&c.value));

    // Poll and note when the page itself rotates the token (= icloud.com ran validate).
    let mut rotated_at = None;
    while start.elapsed() < SILENT_WAIT {
        thread::sleep(Duration::from_millis(500));
        let Some(c) = grab(&win) else { continue };
        let t = c.iter().find(|c| c.name() == "X-APPLE-WEBAUTH-TOKEN").map(|c| common::fp(c.value()));
        if rotated_at.is_none() && t.is_some() && t != token0 {
            rotated_at = Some(start.elapsed().as_millis());
        }
    }
    log.line(format!("token present at launch: {}, rotated by the page after: {rotated_at:?} ms", token0.is_some()));

    let after = grab(&win).map(|c| to_jar(&c, "profile after silent load")).unwrap_or_default();
    after.describe(&mut log, &format!("cookies after {}s", SILENT_WAIT.as_secs()));
    let gone: Vec<_> = before.names().into_iter().filter(|n| after.get(n).is_none()).collect();
    let new: Vec<_> = after.names().into_iter().filter(|n| before.get(n).is_none()).collect();
    log.line(format!("names gone: {gone:?}; names new: {new:?}"));

    let client = common::client(true);
    match common::validate(&client, &after, true) {
        Ok(o) => log.line(format!("reqwest validate with the webview's cookies: {} ok={} {}", o.status, o.ok, o.summary)),
        Err(e) => log.line(format!("validate send error: {e}")),
    }
    let _ = common::save(&common::store_path("silent"), &after);
    other_domains(&mut log, &win);
    app.exit(0);
}
