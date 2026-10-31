//! Downloads, done by whoever embeds the evaluator.

/// Downloads the files that flake inputs are fetched from.
pub trait Fetcher: Sync {
    /// The contents of the file at `url`, which is not a local `file:` URL.
    fn get(&self, url: &str) -> anyhow::Result<Vec<u8>>;
}
