//! Guitarness game entry point: reads the arguments and runs the app.

fn main() {
    let cfg = guitarness_game::AppConfig::from_args(std::env::args().skip(1));
    guitarness_game::build_app(cfg).run();
}
