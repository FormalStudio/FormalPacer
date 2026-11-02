fn main() -> nd::Result<()> {
    if std::env::args().any(|a| a == "--headless") {
        if let Err(e) = formalpacer::headless() {
            eprintln!("formalpacer: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    formalpacer::run()
}
