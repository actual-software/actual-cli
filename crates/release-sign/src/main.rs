//! Signs a published actual-cli release's SHA256SUMS with the org's Cloud KMS
//! key. docs/RELEASE_SIGNING.md covers the one-time setup and when to run it.

mod github;
mod keys;
mod kms;
mod minisign;
mod release;
mod sums;
#[cfg(test)]
mod test_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use crate::minisign::{Signer, TrustedKey};

#[derive(Parser)]
#[command(
    name = "release-sign",
    about = "Sign actual-cli releases with the org's Cloud KMS key (see docs/RELEASE_SIGNING.md)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check a published release against the run that built it, then sign its SHA256SUMS and upload the signature to both release pages
    Release {
        /// The release tag, such as v1.2.3
        tag: String,
        /// The Cloud KMS key version to sign with, by its full resource name
        #[arg(long, env = "RELEASE_SIGN_KMS_KEY", value_name = "KEY_VERSION")]
        kms_key: String,
        /// The release run to check against, when the tag's workflow ran more than once
        #[arg(long, value_name = "RUN_ID")]
        run: Option<u64>,
        /// Check and sign, but keep SHA256SUMS.minisig on this machine
        #[arg(long)]
        no_upload: bool,
        /// Where to write SHA256SUMS.minisig
        #[arg(long, default_value = ".", value_name = "DIR")]
        out_dir: PathBuf,
    },
    /// Sign a local file into FILE.minisig, as the setup's test step does
    SignFile {
        file: PathBuf,
        /// The Cloud KMS key version to sign with, by its full resource name
        #[arg(long, env = "RELEASE_SIGN_KMS_KEY", value_name = "KEY_VERSION")]
        kms_key: String,
    },
    /// Print the minisign public key for a Cloud KMS Ed25519 key exported as PEM
    PublicKey {
        /// The PEM file `gcloud kms keys versions get-public-key` wrote
        #[arg(long)]
        pem: PathBuf,
        /// The key id as 16 hex digits; random when omitted
        #[arg(long)]
        key_id: Option<String>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Release {
            tag,
            kms_key,
            run,
            no_upload,
            out_dir,
        } => {
            let keys = keys::committed()?;
            release::check_tag(&tag)?;
            let signer = kms_signer(&kms_key)?;
            let work = tempfile::tempdir()?;
            let options = release::Options {
                tag,
                run_id: run,
                upload: !no_upload,
                out_dir,
                timestamp: now()?,
            };
            let signed = release::sign_release(
                &github::Gh::new("gh"),
                &signer,
                &keys,
                work.path(),
                &options,
            )?;
            let summary = format!(
                "Signed {} (built by run {}) with the {} key",
                options.tag, signed.run_id, signed.key_name
            );
            if signed.uploaded_to.is_empty() {
                println!(
                    "{summary} and wrote {} without uploading it.",
                    signed.signature_path.display()
                );
            } else {
                println!(
                    "{summary}; {} is on the release pages of {}.",
                    release::SIGNATURE_FILE,
                    signed.uploaded_to.join(" and ")
                );
            }
        }
        Command::SignFile { file, kms_key } => {
            let keys = keys::committed()?;
            let signer = kms_signer(&kms_key)?;
            let out = sign_file(&signer, &keys, &file, now()?)?;
            println!("Wrote {}.", out.display());
        }
        Command::PublicKey { pem, key_id } => {
            let pem =
                fs::read_to_string(&pem).with_context(|| format!("reading {}", pem.display()))?;
            print!("{}", public_key(&pem, key_id.as_deref())?);
        }
    }
    Ok(())
}

fn kms_signer(kms_key: &str) -> Result<kms::KmsSigner> {
    kms::KmsSigner::new(kms_key, kms::gcloud_access_token(Path::new("gcloud"))?)
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn sign_file(
    signer: &dyn Signer,
    keys: &[TrustedKey],
    file: &Path,
    timestamp: u64,
) -> Result<PathBuf> {
    let data = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let name = file
        .file_name()
        .with_context(|| format!("{} isn't a file", file.display()))?
        .to_string_lossy()
        .into_owned();
    let signature = minisign::sign(
        signer,
        &data,
        &format!("timestamp:{timestamp}\tfile:{name}"),
    )?;
    let (key, text) = minisign::encode_for_trusted_key(&signature, &data, keys)?;
    let out = file.with_file_name(format!("{name}.minisig"));
    fs::write(&out, text).with_context(|| format!("writing {}", out.display()))?;
    eprintln!(
        "Signed with the {} key (key id {}); the signature verifies.",
        key.name,
        key.key.key_id_hex()
    );
    Ok(out)
}

fn public_key(pem: &str, key_id: Option<&str>) -> Result<String> {
    let key = minisign::ed25519_key_from_pem(pem)?;
    let key_id = match key_id {
        Some(hex) => minisign::parse_key_id(hex)?,
        None => minisign::random_key_id()?,
    };
    Ok(minisign::PublicKey { key_id, key }.to_file_contents())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{trusted, LocalSigner};

    const RFC_8410_PEM: &str =
        "-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n-----END PUBLIC KEY-----\n";

    #[test]
    fn prints_a_minisign_public_key_for_a_kms_pem() {
        let text = public_key(RFC_8410_PEM, Some("0807060504030201")).unwrap();
        let key = minisign::PublicKey::parse(&text).unwrap();
        assert_eq!(key.key_id, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            key.key,
            minisign::ed25519_key_from_pem(RFC_8410_PEM).unwrap()
        );
        let random = public_key(RFC_8410_PEM, None).unwrap();
        assert_ne!(
            minisign::PublicKey::parse(&random).unwrap().key_id,
            key.key_id
        );
        assert!(public_key(RFC_8410_PEM, Some("xyz")).is_err());
    }

    #[test]
    fn signs_a_local_file_next_to_it() {
        let signer = LocalSigner::generate();
        let keys = [trusted("primary", &signer, [9; 8])];
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("signing-test.txt");
        fs::write(&file, "test\n").unwrap();
        let out = sign_file(&signer, &keys, &file, 1_792_800_000).unwrap();
        assert_eq!(out, dir.path().join("signing-test.txt.minisig"));
        let comment =
            minisign::verify(&fs::read_to_string(&out).unwrap(), b"test\n", &keys[0].key).unwrap();
        assert_eq!(comment, "timestamp:1792800000\tfile:signing-test.txt");
        assert!(sign_file(&signer, &keys, &dir.path().join("missing"), 1).is_err());
    }

    #[test]
    fn parses_the_three_commands() {
        let cli = Cli::try_parse_from([
            "release-sign",
            "release",
            "v1.2.3",
            "--kms-key",
            "projects/p/locations/l/keyRings/r/cryptoKeys/k/cryptoKeyVersions/1",
            "--no-upload",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Release {
                no_upload: true,
                run: None,
                ..
            }
        ));
        let cli = Cli::try_parse_from(["release-sign", "public-key", "--pem", "k.pem"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::PublicKey { key_id: None, .. }
        ));
        let cli =
            Cli::try_parse_from(["release-sign", "sign-file", "f", "--kms-key", "x"]).unwrap();
        assert!(matches!(cli.command, Command::SignFile { .. }));
    }

    #[test]
    fn the_public_key_command_reads_its_pem_file() {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("primary.pem");
        fs::write(&pem, RFC_8410_PEM).unwrap();
        let ok = Cli {
            command: Command::PublicKey { pem, key_id: None },
        };
        run(ok).unwrap();
        let missing = Cli {
            command: Command::PublicKey {
                pem: dir.path().join("absent.pem"),
                key_id: None,
            },
        };
        assert!(run(missing).is_err());
    }

    /// Placeholder keys stop it first, real keys stop it at the tag; neither reaches gcloud.
    #[test]
    fn release_stops_before_gcloud_on_a_bad_tag() {
        let release = Cli {
            command: Command::Release {
                tag: "main".into(),
                kms_key: "projects/p/locations/l/keyRings/r/cryptoKeys/k/cryptoKeyVersions/1"
                    .into(),
                run: None,
                no_upload: true,
                out_dir: ".".into(),
            },
        };
        let err = format!("{:#}", run(release).unwrap_err());
        assert!(
            err.contains("release-keys/primary.pub") || err.contains("isn't a release tag"),
            "{err}"
        );
    }
}
