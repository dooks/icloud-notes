// Debug builds keep the console so the spike's log is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    spike7_shared_lib::run()
}
