#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = tfy_bgm_lib::separation_worker_main() {
        std::process::exit(code);
    }
    tfy_bgm_lib::run()
}
