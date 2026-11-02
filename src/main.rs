fn main() -> nd::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "run") {
        eprintln!("formalpacer: {}", formalpacer::run_game(&args[1..]));
        std::process::exit(127);
    }
    if std::env::args().any(|a| a == "--headless") {
        if let Err(e) = formalpacer::headless() {
            eprintln!("formalpacer: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    formalpacer::run()
}
