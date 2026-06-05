#[tokio::main]
async fn main() {
    if let Err(err) = wf_runtime::cli::run_cli().await {
        eprintln!("{}", err.report().render());
        std::process::exit(1);
    }
}
