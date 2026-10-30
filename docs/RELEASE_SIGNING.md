# Release signing

Every release carries a `SHA256SUMS` file that lists the four tarballs, plus `SHA256SUMS.minisig`, a signature over it. Both release pages get them: [actual-cli](https://github.com/actual-software/actual-cli/releases) and the [actual-releases](https://github.com/actual-software/actual-releases/releases) mirror. The signing key is an Ed25519 key in Google Cloud KMS that nobody can view or export, and a member of the signing group signs each release by hand once it's out. Nothing in GitHub can sign.

The public keys live in [`release-keys/`](../release-keys). Releases are signed with `primary.pub`'s key; `spare.pub` takes over if the primary is ever retired.

---

## Checking a download

You'll need [minisign](https://jedisct1.github.io/minisign/) (`brew install minisign`, or your package manager). Fetch the tarball, `SHA256SUMS`, `SHA256SUMS.minisig` and the public key, then check the signature first and the checksum second:

```bash
TAG=v0.10.0
base=https://github.com/actual-software/actual-releases/releases/download/$TAG
for f in SHA256SUMS SHA256SUMS.minisig actual-darwin-arm64.tar.gz; do curl -fsSLO "$base/$f"; done
curl -fsSL -o actual-release.pub https://raw.githubusercontent.com/actual-software/actual-cli/main/release-keys/primary.pub

minisign -Vm SHA256SUMS -p actual-release.pub
shasum -a 256 --check --ignore-missing SHA256SUMS
```

If the signature's good, `minisign` prints `Signature and comment signature verified` and then the trusted comment. Make sure the comment names the tag you downloaded. An older release's signature still verifies. And if `minisign` reports a different key id, the spare key signed the release, so check against `spare.pub` instead.

## The trusted comment

The trusted comment is signed too. On every release signature it has exactly three tab-separated fields, in this order, and changing any of them breaks the signature:

```
timestamp:<seconds since the Unix epoch>	file:SHA256SUMS	tag:<tag>
```

`tag:` holds the full tag, such as `tag:v0.10.0`. Code that reads the tag back should split the comment on tabs and compare the `tag:` field exactly. Searching it for a version string isn't enough.

## Signing a release

Once the release workflow has published both release pages, one member of the signing group runs `release-sign` from a checkout of `main` (any member will do). Beyond a Rust toolchain, it only needs your own logins:

- `gcloud auth login` with your work account. Membership in the signing group is what lets that account sign with the primary key.
- `gh auth login` with write access to actual-cli and actual-releases, for the uploads.

```bash
cargo run -p release-sign -- release v0.10.0 --kms-key "projects/$PROJECT/locations/$LOCATION/keyRings/$KEY_RING/cryptoKeys/release-signing-primary/cryptoKeyVersions/1"
```

Maintainers will find the values of `$PROJECT`, `$LOCATION` and `$KEY_RING` in the team's internal docs. `--kms-key` can also come from `RELEASE_SIGN_KMS_KEY`. Before it asks KMS for anything, the tool checks three things:

1. that the tag resolves to a commit, and that exactly one push run of `release.yml` built that commit for that tag;
2. that the `SHA256SUMS` in that run's `release-assets` artifact matches every tarball the run built;
3. that the `SHA256SUMS` on each release page is byte for byte the run's copy, that every tarball on the page matches it, and that the page holds nothing else apart from an earlier `SHA256SUMS.minisig`.

Only then does it sign `SHA256SUMS`. It verifies the signature against the key in `release-keys/` it came from, writes `SHA256SUMS.minisig`, and uploads it to both release pages (replacing an earlier one, if there is one).

If a check fails, stop. It means something on a release page isn't what the release workflow built, and that needs explaining before anyone signs it. `--no-upload` runs every check and signs, but keeps the signature on your machine, and `--run <id>` picks the run when a tag's workflow ran more than once. Running the command again is safe; it's also how to finish when the upload to the second page failed.

Sign while the run's artifacts still exist. They're kept for the repository's artifact retention period, 90 days unless someone has changed it.

Maintainers will find the one-time setup of the signing keys, the audit of who signed what, and the steps for rotating to the spare key in the team's internal docs.

## How the signature is made

A minisign signature in the prehashed format holds two Ed25519 signatures: one over the BLAKE2b-512 digest of `SHA256SUMS`, and one over that signature followed by the trusted comment. Cloud KMS signs the exact bytes it's given (PureEdDSA), so `release-sign` makes two `asymmetricSign` calls and assembles the file. The 8-byte key id isn't covered by either signature: the tool attaches the id of whichever committed key the signature verifies against, checking it with the [minisign-verify](https://crates.io/crates/minisign-verify) crate. The tool's tests make the same file with a local Ed25519 key and check it with stock `minisign -V` and with minisign-verify. No test calls Cloud KMS.
