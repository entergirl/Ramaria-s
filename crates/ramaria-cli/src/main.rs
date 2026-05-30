fn main() {
    println!("Ramaria CLI v0.1.0 -- skeleton ready");
    println!(
        "  core  : {}",
        ramaria_app::ramaria_core::config::AppConfig {
            version: "0.1.0".into(),
        }
        .version
    );
    println!("  app   : {}", ramaria_app::hello_app());
}
