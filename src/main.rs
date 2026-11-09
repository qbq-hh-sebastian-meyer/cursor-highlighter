#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod desktop;
mod model;
mod startup;

fn main() {
    let self_test = std::env::args().any(|arg| arg == "--self-test");
    let result = if self_test {
        desktop::self_test()
    } else {
        desktop::run()
    };
    if let Err(error) = result {
        if self_test {
            eprintln!("{error}");
        } else {
            desktop::report_error(&error);
        }
        std::process::exit(1);
    }
}
