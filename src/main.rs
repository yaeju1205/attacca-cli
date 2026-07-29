mod api;
mod app;
mod bg;
mod event;
mod handler;
mod tools;
mod transport;
mod ui;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let transport = transport::Transport::from_env();

    eprintln!("{}", api::whoami(&transport).await);
    eprintln!(
        "  key: {}",
        if transport.key.is_empty() {
            "not set"
        } else {
            "set"
        }
    );

    let mut app = app::App::new(transport);
    event::run(&mut app).await;
}
