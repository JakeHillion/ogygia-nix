use std::path::Path;

use clap::Subcommand;

const STATE_PATHS: [(&str, &str, &str); 3] = [
    ("⚡", "Current system", "/run/current-system"),
    ("🥾", "Booted system", "/run/booted-system"),
    ("🔜", "Next boot system", "/nix/var/nix/profiles/system"),
];

#[derive(Subcommand)]
pub enum Command {
    /// Show build commits for the local host
    Status,
}

impl Command {
    pub fn run(&self) {
        match self {
            Command::Status => show_revisions(),
        }
    }
}

fn show_revisions() {
    for (emoji, label, path) in STATE_PATHS {
        let revision = format_revision(Path::new(path));
        println!("{} {:18} {}", emoji, label, revision);
    }
}

fn format_revision(base_path: &Path) -> String {
    let revision_path = base_path.join("sw/share/ogygia/build-revision");

    match std::fs::read_to_string(&revision_path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.len() > 12 {
                trimmed[..12].to_string()
            } else {
                trimmed.to_string()
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "unknown".into(),
        Err(error) => format!("error reading ({}): {}", revision_path.display(), error),
    }
}
