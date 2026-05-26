fn main() {
    if let Err(err) = vv_agent::cli::main() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}
