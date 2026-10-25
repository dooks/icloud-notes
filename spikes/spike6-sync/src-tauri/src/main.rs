// Debug builds keep the console so the spike's log is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `offline` analyses spike 2's saved pages without opening a window.
    if std::env::args().nth(1).as_deref() == Some("offline") {
        spike6_sync_lib::offline()
    } else {
        spike6_sync_lib::run()
    }
}
