use clap::Command;

fn main() {
    Command::new("ogygia")
        .version(env!("CARGO_PKG_VERSION"))
        .about("ogygia")
        .get_matches();
}
