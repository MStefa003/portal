# Releasing

1. Bump `version` in the workspace `Cargo.toml`, run `cargo build` so
   `Cargo.lock` follows, and move the `[Unreleased]` notes in `CHANGELOG.md`
   under the new version.
2. Commit, then tag and push: `git tag v0.3.0 && git push origin main v0.3.0`.

`.github/workflows/release.yml` then:

- builds Windows, macOS and Linux archives (x86_64 and ARM64) with SHA-256
  files;
- signs every archive with the release key (`.minisig` files) and checks
  the signatures against the key built into `portal update`; without the
  `PORTAL_SIGNING_KEY` secret nothing is published;
- publishes them as a GitHub Release;
- attaches a [build provenance attestation](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations)
  to every archive, so anyone can check it was built by this workflow from
  the tag: `gh attestation verify <archive> --repo MStefa003/portal`;
- regenerates `bucket/portal.json` (Scoop), `Formula/portal.rb` (Homebrew)
  and `packaging/winget/` with `packaging/update_manifests.py`, and commits
  them to `main`. If `main` is protected, allow `github-actions[bot]` to push
  or the commit step fails (the release itself is already published then);
- signs and submits to winget when the secrets below are set.

## Release signing key (required)

`portal update` installs only archives signed with Portal's release key, and
refuses everything else. The public key is `RELEASE_KEY` in
`crates/portal-cli/src/update.rs` (also in `install.sh` and `SECURITY.md`);
the secret key is the repository secret `PORTAL_SIGNING_KEY`. Signatures
are in [minisign](https://jedisct1.github.io/minisign/) format, made by
`packaging/sign.py`, and name the file in their trusted comment so an
archive cannot be passed off as another version or platform.

Keep an offline backup of the secret key. If it is lost, releases can no
longer be signed for existing installations: a new key needs a release that
ships the new public key, and people on older versions must then update by
reinstalling. If it leaks, rotate it the same way and say so in the release
notes.

To make a new key pair: `python3 packaging/sign.py keygen`, then put the
secret in `PORTAL_SIGNING_KEY` and the public key in the three files above.

## Package managers

The repository is its own Scoop bucket and Homebrew tap, so these work as
soon as the release workflow has committed the manifests:

```console
scoop bucket add portal https://github.com/MStefa003/portal
scoop install portal

brew tap mstefa003/portal https://github.com/MStefa003/portal
brew install portal

cargo binstall --git https://github.com/MStefa003/portal portal-cli
```

### winget (one-time setup)

winget packages live in [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs),
where every version is a pull request that Microsoft's bots and moderators
review.

1. Create a classic GitHub personal access token with the `public_repo`
   scope and save it as the repository secret `WINGET_TOKEN`.
2. From then on each release submits the generated manifests with
   [wingetcreate](https://github.com/microsoft/winget-create). The first
   submission creates the package `MStefa003.Portal`; review can take a few
   days. To submit the current version by hand:
   `wingetcreate submit --token <token> packaging/winget`.

After approval: `winget install MStefa003.Portal`.

## Code signing

Unsigned programs work, but browsers and Windows SmartScreen warn about
them when downloaded with a browser, and macOS Gatekeeper blocks them. The
install scripts and package managers are not affected (they do not mark
downloads as coming from the internet). Each platform's signing switches on
when its secrets exist; without them releases are built unsigned.

### Windows: SignPath (free for open source)

Since 2023, Windows code-signing certificates must live on hardware or in a
cloud HSM, so they cannot simply be stored as a CI secret. The
[SignPath Foundation](https://signpath.org/) signs open-source projects for
free with its own certificate, from CI:

1. Apply at <https://signpath.org/apply>. They check that the project is
   open source, built in public CI and has no malware.
2. In SignPath, create a project with slug `portal`, a signing policy with
   slug `release-signing`, and link it to this GitHub repository.
3. Add the API token as the repository secret `SIGNPATH_API_TOKEN` and the
   organization ID as the repository *variable* `SIGNPATH_ORGANIZATION_ID`.

Alternatives: Azure Artifact Signing (paid monthly; for individuals only in
some countries), or a certificate from a CA on a hardware token, which
needs a self-hosted Windows runner.

### macOS: Developer ID and notarization

Needs an Apple Developer Program membership (99 USD per year).

1. Create a *Developer ID Application* certificate, export it with its
   private key as a `.p12` file, and save `base64 -i cert.p12` as the secret
   `APPLE_CERTIFICATE` and its password as `APPLE_CERTIFICATE_PASSWORD`.
2. Save the certificate's name, e.g.
   `Developer ID Application: Your Name (TEAMID1234)`, as
   `APPLE_SIGNING_IDENTITY`, and the team ID as `APPLE_TEAM_ID`.
3. Create an app-specific password at <https://account.apple.com> and save
   it as `APPLE_APP_PASSWORD`, with your Apple ID email as `APPLE_ID`.
