// Spike 4 probe: no webview. Loads the DPAPI-saved cookies and runs only read-only
// calls (setup validate, CloudKit changes/zone with 1 record).
//
//   probe show [name]            describe a stored jar, no network
//   probe validate [--save] [--file name]
//   probe refresh-test           old vs refreshed cookies
//   probe matrix                 drop / corrupt each cookie, minimal set (never saves)
//
// Privacy: see common. Nothing here prints a cookie value, dsid or error body.

use std::{thread, time::Duration};

use common::{Jar, Log, Outcome};
use reqwest::blocking::Client;

const GAP: Duration = Duration::from_millis(1500);

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let file = args.iter().position(|a| a == "--file").and_then(|i| args.get(i + 1)).cloned().unwrap_or("session".into());
    match args.first().map(String::as_str) {
        Some("show") => show(args.get(1).map(String::as_str).unwrap_or("session")),
        Some("validate") => validate_cmd(&file, args.iter().any(|a| a == "--save")),
        Some("refresh-test") => refresh_test(),
        Some("matrix") => matrix(),
        _ => eprintln!("usage: probe show|validate [--save] [--file name]|refresh-test|matrix"),
    }
}

fn load(log: &mut Log, name: &str) -> Option<Jar> {
    match common::load(&common::store_path(name)) {
        Ok(j) => Some(j),
        Err(e) => {
            log.line(format!("load {name} failed: {e}"));
            None
        }
    }
}

fn show(name: &str) {
    let mut log = Log::new("show");
    if let Some(j) = load(&mut log, name) {
        j.describe(&mut log, name);
    }
}

fn v(log: &mut Log, label: &str, client: &Client, jar: &Jar, origin: bool) -> Option<Outcome> {
    match common::validate(client, jar, origin) {
        Ok(o) => {
            log.line(format!("[{label}] validate: {} ok={} {}", o.status, o.ok, o.summary));
            Some(o)
        }
        Err(e) => {
            log.line(format!("[{label}] validate send error: {e}"));
            None
        }
    }
}

fn ck(log: &mut Log, label: &str, client: &Client, jar: &Jar, ckdb: &str) -> Option<Outcome> {
    match common::changes_zone(client, jar, ckdb) {
        Ok(o) => {
            log.line(format!("[{label}] changes/zone: {} ok={} {}", o.status, o.ok, o.summary));
            Some(o)
        }
        Err(e) => {
            log.line(format!("[{label}] changes/zone send error: {e}"));
            None
        }
    }
}

fn validate_cmd(file: &str, save: bool) {
    let mut log = Log::new("validate");
    let Some(mut jar) = load(&mut log, file) else { return };
    jar.describe(&mut log, &format!("loaded {file}.dpapi"));
    let client = common::client(true);
    let Some(o) = v(&mut log, "fresh process", &client, &jar, true) else { return };
    let infos = jar.merge(&o.set_cookies);
    common::log_set_cookies(&mut log, &infos);
    if let Some(ckdb) = &o.ckdb {
        thread::sleep(GAP);
        if let Some(c) = ck(&mut log, "fresh process, refreshed jar", &client, &jar, ckdb) {
            common::log_set_cookies(&mut log, &jar.merge(&c.set_cookies));
        }
    }
    if save && o.ok {
        jar.saved_at = common::now();
        jar.source = "probe validate --save".into();
        match common::save(&common::store_path(file), &jar) {
            Ok(n) => log.line(format!("saved refreshed jar ({n} bytes)")),
            Err(e) => log.line(format!("SAVE FAILED: {e}")),
        }
        jar.describe(&mut log, "saved");
    }
}

fn refresh_test() {
    let mut log = Log::new("refresh");
    let Some(a) = load(&mut log, "session") else { return };
    let _ = common::save(&common::store_path("session.before-refresh"), &a);
    a.describe(&mut log, "A = session.dpapi as loaded");
    let client = common::client(true);

    let Some(o1) = v(&mut log, "1: A", &client, &a, true) else { return };
    let mut b = a.clone();
    common::log_set_cookies(&mut log, &b.merge(&o1.set_cookies));
    let Some(ckdb) = o1.ckdb.clone() else { return log.line("A did not validate; stopping") };

    thread::sleep(GAP);
    let Some(o2) = v(&mut log, "2: B = A + set-cookies of 1", &client, &b, true) else { return };
    let mut c = b.clone();
    common::log_set_cookies(&mut log, &c.merge(&o2.set_cookies));

    thread::sleep(GAP);
    v(&mut log, "3: A again (old values after a refresh)", &client, &a, true);
    thread::sleep(GAP);
    ck(&mut log, "4: A on CloudKit", &client, &a, &ckdb);
    thread::sleep(GAP);
    ck(&mut log, "5: C on CloudKit", &client, &c, &ckdb);
    thread::sleep(GAP);
    v(&mut log, "6: C (latest)", &client, &c, true);

    c.saved_at = common::now();
    c.source = "probe refresh-test".into();
    match common::save(&common::store_path("session"), &c) {
        Ok(_) => log.line("saved C as session.dpapi (A kept as session.before-refresh.dpapi)"),
        Err(e) => log.line(format!("SAVE FAILED: {e}")),
    }
    c.describe(&mut log, "C");
}

struct Case {
    label: String,
    v: (u16, bool),
    ck: (u16, bool),
}

fn run_case(log: &mut Log, client: &Client, label: &str, jar: &Jar, ckdb: &str, origin: bool) -> Case {
    thread::sleep(GAP);
    let vo = v(log, label, client, jar, origin).map(|o| (o.status, o.ok)).unwrap_or((0, false));
    thread::sleep(GAP);
    let co = ck(log, label, client, jar, ckdb).map(|o| (o.status, o.ok)).unwrap_or((0, false));
    Case { label: label.into(), v: vo, ck: co }
}

fn matrix() {
    let mut log = Log::new("matrix");
    let Some(base) = load(&mut log, "session") else { return };
    base.describe(&mut log, "base (in memory only; nothing is saved)");
    let client = common::client(true);
    let Some(o) = v(&mut log, "baseline", &client, &base, true) else { return };
    let Some(ckdb) = o.ckdb else { return log.line("baseline did not validate; stopping") };

    let mut cases = vec![run_case(&mut log, &client, "baseline", &base, &ckdb, true)];
    cases.push(run_case(&mut log, &client, "no cookies", &Jar::default(), &ckdb, true));
    cases.push(run_case(&mut log, &client, "all cookies, no Origin/Referer (validate only)", &base, &ckdb, false));
    let no_ua = common::client(false);
    cases.push(run_case(&mut log, &no_ua, "all cookies, no User-Agent", &base, &ckdb, true));

    let names = base.names();
    let mut needed = vec![];
    for n in &names {
        let c = run_case(&mut log, &client, &format!("drop {n}"), &base.without(n), &ckdb, true);
        if !(c.v.1 && c.ck.1) {
            needed.push(n.clone());
        }
        cases.push(c);
    }
    for n in &names {
        cases.push(run_case(&mut log, &client, &format!("corrupt {n}"), &base.corrupted(n), &ckdb, true));
    }

    // Minimal set: start from the cookies whose removal broke something, add others until both calls work.
    let mut set = needed.clone();
    let mut c = run_case(&mut log, &client, &format!("only {set:?}"), &base.only(&set), &ckdb, true);
    let mut rest: Vec<_> = names.iter().filter(|n| !set.contains(n)).cloned().collect();
    cases.push(Case { label: c.label.clone(), v: c.v, ck: c.ck });
    while !(c.v.1 && c.ck.1) && !rest.is_empty() {
        set.push(rest.remove(0));
        c = run_case(&mut log, &client, &format!("only {set:?}"), &base.only(&set), &ckdb, true);
        cases.push(Case { label: c.label.clone(), v: c.v, ck: c.ck });
    }
    // Then try removing each member of the working set, in case two cookies can stand in for each other.
    if c.v.1 && c.ck.1 {
        for n in set.clone() {
            let smaller: Vec<_> = set.iter().filter(|x| **x != n).cloned().collect();
            let t = run_case(&mut log, &client, &format!("only {smaller:?}"), &base.only(&smaller), &ckdb, true);
            if t.v.1 && t.ck.1 {
                set = smaller;
            }
            cases.push(t);
        }
        log.line(format!("MINIMAL WORKING SET: {set:?}"));
    } else {
        log.line("no working subset found");
    }

    thread::sleep(GAP);
    v(&mut log, "after matrix: base again", &client, &base, true);

    log.line("\n== summary (validate status/ok | changes/zone status/ok) ==");
    for c in &cases {
        log.line(format!("  {:<70} validate {} {:<5} | ck {} {}", c.label, c.v.0, c.v.1, c.ck.0, c.ck.1));
    }
}
