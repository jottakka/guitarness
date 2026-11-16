//! Tonica game entry point: reads the arguments and runs the app.

fn main() {
    let cfg = tonica_game::AppConfig::from_args(std::env::args().skip(1));
    tonica_game::build_app(cfg).run();
}
