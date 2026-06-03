mod app;
mod artifacts;
mod cli;
mod defaults;
mod model;
mod planner;
mod validation;

fn main() {
    if let Err(error) = app::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
