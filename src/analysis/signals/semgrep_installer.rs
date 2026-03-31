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
}
