//! Building narinfo responses from store-path metadata.

use ogygia_nixutils::Compression;
use ogygia_nixutils::NarInfo;
use ogygia_nixutils::PathInfo;

/// Build a [`NarInfo`] response from store-path metadata.
///
/// This encodes irisd's serving policy — zstd compression and a NAR URL with
/// the NarHash embedded so NAR requests are self-describing and need no
/// server-side state to match a narinfo to its NAR.
///
/// `FileHash`/`FileSize` are set to placeholders (the uncompressed NAR hash and
/// size); the caller fills in the real compressed values after generating the
/// NAR.
pub fn narinfo_from_path_info(info: &PathInfo) -> NarInfo {
    // Store path name without the `/nix/store/` prefix.
    let store_name = info.path.strip_prefix("/nix/store/").unwrap_or(&info.path);

    // Encode the NarHash in the URL so NAR requests are self-describing and we
    // don't need server-side state to match a narinfo to its NAR.
    let url = format!("nar/{}/{}.nar.zst", info.nar_hash.to_hex(), store_name);

    // References and deriver are reported as bare store names, not full paths.
    let references: Vec<String> = info
        .references
        .iter()
        .filter_map(|r| r.strip_prefix("/nix/store/"))
        .map(String::from)
        .collect();
    let deriver = info
        .deriver
        .as_ref()
        .and_then(|d| d.strip_prefix("/nix/store/").map(String::from));

    NarInfo {
        store_path: info.path.clone(),
        url,
        compression: Compression::Zstd,
        // FileHash is a placeholder (the NAR hash) until the NAR is generated
        // and compressed, at which point the caller fills in the real value.
        file_hash: info.nar_hash,
        file_size: info.nar_size,
        nar_hash: info.nar_hash,
        nar_size: info.nar_size,
        references,
        deriver,
        signatures: info.signatures.clone(),
        ca: info.ca.clone(),
    }
}

#[cfg(test)]
mod tests {
    use ogygia_nixutils::NarHash;
    use ogygia_nixutils::Signature;

    use super::*;

    #[test]
    fn test_narinfo_from_path_info() {
        let path_info = PathInfo {
            path: "/nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10".to_string(),
            nar_hash: NarHash::from_sri("sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY=")
                .unwrap(),
            nar_size: 12345,
            references: vec!["/nix/store/xyz789abc123def456ghi012jkl345mno-glibc-2.35".to_string()],
            deriver: Some(
                "/nix/store/drv123abc456def789ghi012jkl345mno-hello-2.10.drv".to_string(),
            ),
            signatures: vec![
                "cache.nixos.org-1:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw=="
                    .parse::<Signature>()
                    .unwrap(),
            ],
            ca: None,
        };

        let narinfo = narinfo_from_path_info(&path_info);

        assert_eq!(
            narinfo.store_path,
            "/nix/store/abc123def456ghi789jkl012mno345pq-hello-2.10"
        );
        assert_eq!(
            narinfo.url,
            "nar/4b4ca6bc50c269e51f6525b5bdeab4814d765a857cd2a6525dc832965c02cd96/\
             abc123def456ghi789jkl012mno345pq-hello-2.10.nar.zst"
        );
        assert_eq!(narinfo.compression, Compression::Zstd);
        assert_eq!(
            narinfo.nar_hash.to_sri(),
            "sha256-S0ymvFDCaeUfZSW1veq0gU12WoV80qZSXcgyllwCzZY="
        );
        assert_eq!(narinfo.nar_size, 12345);
        assert_eq!(
            narinfo.references,
            vec!["xyz789abc123def456ghi012jkl345mno-glibc-2.35"]
        );
        assert_eq!(
            narinfo.deriver.as_deref(),
            Some("drv123abc456def789ghi012jkl345mno-hello-2.10.drv")
        );
        assert_eq!(narinfo.signatures.len(), 1);
    }
}
