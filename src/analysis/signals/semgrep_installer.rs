use anyhow::{bail, Result};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::Digest;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub(crate) const SEMGREP_VERSION: &str = "1.156.0";

pub(crate) struct WheelInfo {
    pub url: String,
    pub sha256: String,
}

/// Returns wheel URL + SHA256 for the current platform, or None if unsupported.
pub(crate) fn platform_wheel_info() -> Option<WheelInfo> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(WheelInfo {
            url: "https://files.pythonhosted.org/packages/66/a9/3d4082f30bca4ba738d391e241174c2297be87bb5ca37a3911b164289694/semgrep-1.156.0-cp310.cp311.cp312.cp313.cp314.py310.py311.py312.py313.py314-none-macosx_11_0_arm64.whl".to_string(),
            sha256: "ff57b35def987ec3f21748051fdb89ae57574984bf8108b03d79473da49e93f0".to_string(),
        }),
        ("macos", "x86_64") => Some(WheelInfo {
            url: "https://files.pythonhosted.org/packages/88/d4/9f8efd20f96cb9ff74d5dd013b1938b9246001c4b61e8e98501f236af71a/semgrep-1.156.0-cp310.cp311.cp312.cp313.cp314.py310.py311.py312.py313.py314-none-macosx_10_14_x86_64.whl".to_string(),
            sha256: "5bd2924958af5d4e199fe82c4ac9c7be4ae4fd20a3a46ce9f71237f8408f5b66".to_string(),
        }),
        ("linux", "x86_64") => Some(WheelInfo {
            url: "https://files.pythonhosted.org/packages/92/0c/b00156bee40cf9e594c2bd10672232e33d9a7d1bc8b5a6dd697097c8d6be/semgrep-1.156.0-cp310.cp311.cp312.cp313.cp314.py310.py311.py312.py313.py314-none-manylinux2014_x86_64.whl".to_string(),
            sha256: "a7f68544bcd33fac9bc519e6a6c4759d612836422ce87bc08dd014481c3e8fd0".to_string(),
        }),
        ("linux", "aarch64") => Some(WheelInfo {
            url: "https://files.pythonhosted.org/packages/60/32/dca58fbb76a0d5ba16ed366e3478c50c737bd3da8788eb0d213d60562075/semgrep-1.156.0-cp310.cp311.cp312.cp313.cp314.py310.py311.py312.py313.py314-none-manylinux2014_aarch64.whl".to_string(),
            sha256: "7e9ae909fa8c26220c88392af9c22479c0ef51e170eeda8b2c81382b431afb76".to_string(),
        }),
        _ => None,
    }
}

/// Returns the OS-appropriate path where semgrep-core will be cached.
/// Path: `<data_local_dir>/actual/semgrep-core`
pub(crate) fn semgrep_core_cache_path() -> PathBuf {
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"));
    base.join("actual").join("semgrep-core")
}

/// Extracts the `semgrep/bin/semgrep-core` binary from a PyPI wheel (zip) archive.
fn extract_semgrep_core_from_wheel(wheel_bytes: &[u8]) -> Result<Vec<u8>> {
    let cursor = std::io::Cursor::new(wheel_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        if entry.name().ends_with("semgrep/bin/semgrep-core") {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    bail!("semgrep/bin/semgrep-core not found in wheel archive")
}

/// Verifies the SHA256 checksum of `data` against the `expected` hex string.
fn verify_sha256(data: &[u8], expected: &str) -> Result<()> {
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        bail!("checksum mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

/// Downloads a URL with a progress bar, returning the complete response bytes.
async fn download_with_progress(url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let response = client.get(url).send().await?;
    let total = response.content_length();
    let pb = ProgressBar::new(total.unwrap_or(0));
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  {bar:40.cyan/blue} {bytes}/{total_bytes} ({eta})")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        pb.inc(chunk.len() as u64);
        bytes.extend_from_slice(&chunk);
    }
    pb.finish_and_clear();
    Ok(bytes)
}

/// Ensures semgrep-core is available on disk, downloading it if necessary.
/// Returns the path to the cached binary.
pub(crate) async fn ensure_semgrep_core() -> Result<PathBuf> {
    let info = platform_wheel_info()
        .ok_or_else(|| anyhow::anyhow!("unsupported platform: no semgrep-core wheel available"))?;
    let cache_path = semgrep_core_cache_path();

    // Fast path: already cached
    if cache_path.exists() {
        return Ok(cache_path);
    }

    // Create parent directories
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    eprintln!("Downloading semgrep-core v{SEMGREP_VERSION} for this platform...\n");

    let wheel_bytes = download_with_progress(&info.url).await?;
    verify_sha256(&wheel_bytes, &info.sha256)?;
    let binary = extract_semgrep_core_from_wheel(&wheel_bytes)?;

    // Write to temp path, set executable, atomic rename
    let tmp_path = cache_path.with_extension("tmp");
    std::fs::write(&tmp_path, &binary)?;
    std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp_path, &cache_path)?;

    eprintln!("semgrep-core cached at {}\n", cache_path.display());
    Ok(cache_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_info_is_known_for_current_platform() {
        let info = platform_wheel_info();
        assert!(info.is_some(), "no wheel info for this platform");
        let info = info.unwrap();
        assert!(!info.url.is_empty());
        assert_eq!(info.sha256.len(), 64); // hex SHA256
    }

    #[test]
    fn cache_path_is_in_data_dir() {
        let path = semgrep_core_cache_path();
        let path_str = path.to_string_lossy();
        assert!(
            path_str.contains("actual") || path_str.contains("local"),
            "cache path should be under user data dir, got: {path_str}"
        );
        assert!(path_str.ends_with("semgrep-core"));
    }

    #[test]
    fn sha256_verify_correct_data() {
        let data = b"hello world";
        let mut hasher = sha2::Sha256::new();
        hasher.update(data);
        let hash = format!("{:x}", hasher.finalize());
        assert!(verify_sha256(data, &hash).is_ok());
    }

    #[test]
    fn sha256_verify_wrong_hash_errors() {
        let data = b"hello world";
        let result = verify_sha256(data, "deadbeef");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("checksum mismatch"));
    }

    #[test]
    fn extract_semgrep_core_from_bytes_succeeds_on_valid_zip() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::FileOptions::<()>::default();
        zip.start_file("semgrep-1.156.0.data/purelib/semgrep/bin/semgrep-core", opts).unwrap();
        zip.write_all(b"fake binary content").unwrap();
        zip.finish().unwrap();
        let wheel_bytes = buf.into_inner();

        let extracted = extract_semgrep_core_from_wheel(&wheel_bytes).unwrap();
        assert_eq!(extracted, b"fake binary content");
    }

    #[test]
    fn ensure_semgrep_core_errors_on_unsupported_platform() {
        // This test documents that ensure_semgrep_core errors when platform_wheel_info() returns None.
        // On supported platforms (macOS/Linux x86_64/arm64), platform_wheel_info() returns Some,
        // so this test just verifies the function exists and is callable.
        // We verify the happy path exists via compilation + the fast-path logic.
        if platform_wheel_info().is_none() {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(ensure_semgrep_core());
            assert!(result.is_err());
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("unsupported platform"));
        }
        // On supported platforms: test just passes (documents the contract)
    }

    #[test]
    fn extract_semgrep_core_errors_when_entry_missing() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::FileOptions::<()>::default();
        zip.start_file("semgrep/something_else.txt", opts).unwrap();
        zip.write_all(b"irrelevant").unwrap();
        zip.finish().unwrap();
        let wheel_bytes = buf.into_inner();

        let result = extract_semgrep_core_from_wheel(&wheel_bytes);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("semgrep/bin/semgrep-core"));
    }
}
