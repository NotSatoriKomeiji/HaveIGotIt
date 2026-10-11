#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod ui;

fn main() {
    #[cfg(windows)]
    ui::run();
    #[cfg(not(windows))]
    println!("HaveIGotIt is a Windows desktop app. Run cargo test for the portable core.");
}
