mod api;
mod app;
mod tools;
mod ui;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let api = api::Api::from_env();

    eprintln!("{}", api.whoami().await);
    eprintln!("  key: {}", if api.key.is_empty() { "not set" } else { "set" });

    let mut app = app::App::new(api);
    app.run().await;
}
