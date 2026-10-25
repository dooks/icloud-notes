// Spike 1: let the user sign in to icloud.com inside WebView2, then check whether
// the webview cookie API exposes the HttpOnly X-APPLE-WEBAUTH-* cookies and whether
// reqwest can reuse them against setup.icloud.com.
//
// Privacy: never logs cookie values, dsid, names or emails. Cookies stay in memory.

use std::{
    fs,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder, webview::Cookie};

const ICLOUD: &str = "https://www.icloud.com/";
const SETUP_BASE: &str = "https://setup.icloud.com/setup/ws/1";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
const REPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../out/report.txt");
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

struct Log(String);

impl Log {
    fn line(&mut self, s: impl AsRef<str>) {
        println!("{}", s.as_ref());
        self.0.push_str(s.as_ref());
        self.0.push('\n');
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let win = WebviewWindowBuilder::new(app, "login", WebviewUrl::External(ICLOUD.parse()?))
                .title("spike1: sign in to iCloud")
                .inner_size(1000.0, 800.0)
                .build()?;
            let handle = app.handle().clone();
            // Off the main thread: WebView2 cookie calls deadlock if made from it synchronously.
            thread::spawn(move || watch(handle, win));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn watch(app: AppHandle, win: WebviewWindow) {
    let mut log = Log(String::new());
    let setup_url: tauri::Url = format!("{SETUP_BASE}/validate").parse().unwrap();
    let start = Instant::now();
    let mut last_err = String::new();
    log.line("waiting for X-APPLE-WEBAUTH-TOKEN (sign in in the window)...");

    loop {
        thread::sleep(Duration::from_secs(2));
        if start.elapsed() > TIMEOUT {
            log.line("TIMEOUT: no auth cookie within 15 min");
            break;
        }
        let cookies = match win.cookies_for_url(setup_url.clone()) {
            Ok(c) => c,
            Err(e) => {
                let e = e.to_string();
                if e != last_err {
                    log.line(format!("cookies_for_url error: {e}"));
                    last_err = e;
                }
                continue;
            }
        };
        if !cookies.iter().any(|c| c.name() == "X-APPLE-WEBAUTH-TOKEN") {
            continue;
        }

        log.line(format!("auth cookie seen after {}s; waiting 8s for the page to settle", start.elapsed().as_secs()));
        thread::sleep(Duration::from_secs(8));

        let for_setup = win.cookies_for_url(setup_url.clone()).unwrap_or_default();
        dump(&mut log, "cookies_for_url(setup.icloud.com/setup/ws/1/validate)", &for_setup);
        match win.cookies_for_url(ICLOUD.parse().unwrap()) {
            Ok(c) => dump(&mut log, "cookies_for_url(www.icloud.com/)", &c),
            Err(e) => log.line(format!("cookies_for_url(www) error: {e}")),
        }
        match win.cookies() {
            Ok(c) => {
                let mut names: Vec<_> = c.iter().map(|c| format!("{} @ {}", c.name(), c.domain().unwrap_or("?"))).collect();
                names.sort();
                log.line(format!("\n== cookies() (all, {}) ==", names.len()));
                for n in names {
                    log.line(format!("  {n}"));
                }
            }
            Err(e) => log.line(format!("cookies() error: {e}")),
        }

        let params = [
            ("clientBuildNumber", "2420Hotfix12"),
            ("clientMasteringNumber", "2420Hotfix12"),
            ("clientId", "3B1D5C2A-6F4E-4C1B-9A7D-0E2F8B6C4D11"),
        ];
        let validate = format!("{SETUP_BASE}/validate");
        let account_login = format!("{SETUP_BASE}/accountLogin");
        probe(&mut log, "A validate: cookies+Origin", &validate, &for_setup, true, &[], None);
        probe(&mut log, "D validate: cookies only, no Origin", &validate, &for_setup, false, &[], None);
        probe(&mut log, "B validate: cookies+Origin+client params", &validate, &for_setup, true, &params, None);
        probe(
            &mut log,
            "C accountLogin: cookies+Origin, no dsWebAuthToken",
            &account_login,
            &for_setup,
            true,
            &params,
            Some(json!({ "extended_login": true })),
        );
        break;
    }

    if let Some(dir) = std::path::Path::new(REPORT).parent() {
        let _ = fs::create_dir_all(dir);
    }
    match fs::write(REPORT, &log.0) {
        Ok(()) => println!("report written to {REPORT}"),
        Err(e) => println!("could not write report: {e}"),
    }
    app.exit(0);
}

fn dump(log: &mut Log, title: &str, cookies: &[Cookie<'static>]) {
    log.line(format!("\n== {title}: {} cookies ==", cookies.len()));
    for c in cookies {
        log.line(format!(
            "  {:<32} domain={:<16} path={:<4} httpOnly={:?} secure={:?} sameSite={:?} expires={:?} valueLen={}",
            c.name(),
            c.domain().unwrap_or("-"),
            c.path().unwrap_or("-"),
            c.http_only(),
            c.secure(),
            c.same_site(),
            c.expires_datetime(),
            c.value().len(),
        ));
    }
}

fn probe(
    log: &mut Log,
    label: &str,
    url: &str,
    cookies: &[Cookie<'static>],
    origin: bool,
    params: &[(&str, &str)],
    body: Option<Value>,
) {
    log.line(format!("\n== probe {label} ==\n  POST {url} params={:?}", params.iter().map(|p| p.0).collect::<Vec<_>>()));
    let client = reqwest::blocking::Client::builder().user_agent(UA).build().unwrap();
    let cookie_header = cookies.iter().map(|c| format!("{}={}", c.name(), c.value())).collect::<Vec<_>>().join("; ");
    let mut req = client
        .post(url)
        .query(params)
        .header("Cookie", cookie_header)
        .header("Content-Type", "text/plain;charset=UTF-8")
        .header("Accept", "*/*");
    if origin {
        req = req.header("Origin", "https://www.icloud.com").header("Referer", ICLOUD);
    }
    req = req.body(body.map(|b| b.to_string()).unwrap_or_default());

    let resp = match req.send() {
        Ok(r) => r,
        Err(e) => return log.line(format!("  send error: {e}")),
    };
    log.line(format!("  status: {}", resp.status()));
    let set_cookies: Vec<_> = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok()?.split('=').next().map(str::to_owned))
        .collect();
    log.line(format!("  set-cookie names: {set_cookies:?}"));

    let text = resp.text().unwrap_or_default();
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return log.line(format!("  non-JSON body ({} bytes): {:.200}", text.len(), text));
    };
    let Some(obj) = v.as_object() else {
        return log.line(format!("  JSON but not an object: {:.200}", text));
    };
    log.line(format!("  top-level keys: {:?}", obj.keys().collect::<Vec<_>>()));
    match obj.get("webservices").and_then(Value::as_object) {
        Some(ws) => {
            log.line(format!("  webservices ({}):", ws.len()));
            for (k, s) in ws {
                log.line(format!(
                    "    {k:<24} {} status={}",
                    s.get("url").and_then(Value::as_str).unwrap_or("-"),
                    s.get("status").and_then(Value::as_str).unwrap_or("-"),
                ));
            }
        }
        // Error bodies can carry trustTokens and requestInfo: print only the error field.
        None => log.line(format!("  no webservices; error: {}", obj.get("error").unwrap_or(&Value::Null))),
    }
    if let Some(ds) = obj.get("dsInfo").and_then(Value::as_object) {
        log.line(format!("  dsInfo has dsid: {}", ds.contains_key("dsid")));
    }
}
