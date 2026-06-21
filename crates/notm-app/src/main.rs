mod app;
mod cli;
mod config;
mod logging;
mod paths;

fn main() -> anyhow::Result<()> {
    logging::init();
    app::run(cli::parse())
}
