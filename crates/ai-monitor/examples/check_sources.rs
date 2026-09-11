//! Developer-only live probe; not part of the installed TUI's command surface.
use ai_monitor::{config::Config, http::Http, model::Source, providers};

fn main() {
    let config = Config::load().expect("configuration");
    std::thread::scope(|scope| {
        for source in Source::ALL {
            let config = &config;
            scope.spawn(move || {
                let result = providers::prepare(source, config).and_then(|input| {
                    providers::fetch(
                        source,
                        &input,
                        &Http::new()?,
                        config,
                        &mut providers::Session::default(),
                    )
                });
                match result {
                    Ok(cards) => println!("{}: {:?}", source.title(), cards),
                    Err(error) => println!("{}: {}", source.title(), error.message),
                }
            });
        }
    });
}
