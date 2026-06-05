fn main() {
    let cfg = ramaria_app::ramaria_core::RamariaConfig::default();
    println!("Ramaria CLI {} -- skeleton ready", cfg.version);
    println!(
        "  core           : {}  (schema v{})",
        cfg.version, cfg.schema_version
    );
    println!("  app            : {}", ramaria_app::hello_app());
}
