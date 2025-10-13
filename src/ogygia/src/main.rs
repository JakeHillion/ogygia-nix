mod status;

use clap::Parser;

#[derive(Parser)]
#[command(name = "ogygia", version, about = "ogygia")]
struct Cli {
    #[command(subcommand)]
    command: status::Command,
}

fn main() {
    let cli = Cli::parse();
    cli.command.run();
}
