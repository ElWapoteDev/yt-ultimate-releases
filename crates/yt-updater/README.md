# Rust desktop updates

`yt-updater` replaces the Electron updater with a Rust client for the existing
public release repository, `ElWapoteDev/yt-ultimate-releases`. It does not publish
releases, use account credentials, install during shutdown, or launch a downloaded
file automatically.

## Application integration

```rust,no_run
use yt_updater::{CancellationToken, Edition, Updater};

# async fn example() -> yt_updater::Result<()> {
let updater = Updater::with_edition("5.0.0", "profile/Updates", Edition::Base)?;
if let Some(info) = updater.check().await? {
    // Start this only after the user chooses to download.
    let verified = updater.download(&info, CancellationToken::new(), |progress| {
        println!("{} / {}", progress.downloaded_bytes, progress.total_bytes);
    }).await?;
    // Store `verified` for a separate explicit Install button.
    // updater.open_installer(&verified)?;
    println!("Verified installer: {}", verified.path().display());
}
# Ok(())
# }
```

`Updater::new` selects the base edition. Builds with `native-stems` must use
`Edition::Full`; the application controls the edition, not a user-editable update
preference. `for_developer_repository(version, cache, repository, edition)` is an
explicit developer-only override. The normal client uses the original repository.

`check` returns only a newer, stable SemVer release with a compatible architecture
and edition. It ignores drafts, prereleases, equal-version build metadata and
older releases. A release without a matching installer returns `None`.

`download` supports cancellation and progress, writes a temporary file, checks
the exact published byte count and SHA-256, then atomically publishes the verified
file without overwriting an existing file. A cached installer is checked again
before reuse. `VerifiedUpdate` cannot be constructed by callers. `open_installer`
checks the file again before opening the DMG on macOS or launching NSIS on Windows;
the user then completes the platform installer. It does not terminate the app.
SHA verification inside `open_installer` is synchronous, so a UI should run that
explicit action on a blocking worker.

## Release asset contract

| Platform | Base | Full |
| --- | --- | --- |
| macOS Apple Silicon | `YT Ultimate_<version>_aarch64.dmg` | `YT Ultimate_<version>_aarch64_full.dmg` |
| macOS Intel | `YT Ultimate_<version>_x64.dmg` | `YT Ultimate_<version>_x64_full.dmg` |
| Windows x64 | `yt-ultimate_<version>_x64-setup.exe` | `yt-ultimate_<version>_x64_full-setup.exe` |

The Mac product prefix may also be `YT.Ultimate` or `YT-Ultimate` because GitHub
asset upload tools can normalize spaces. There must be exactly one matching
installer per platform and edition. Base and full never replace one another.
The full Windows name is reserved even when a release does not ship that edition.

Each installer needs either a GitHub release asset `digest` with the
`sha256:<64 hex characters>` format or an entry in the release's `SHA256SUMS`:

```text
<64 lowercase hexadecimal characters>  <exact release asset filename>
```

The packaging command emits this standard format. A release containing multiple
platforms or editions should merge their checksum lines into one `SHA256SUMS`
asset. Filenames include spaces exactly as uploaded. If both the GitHub digest
and checksum manifest exist they must agree. Duplicate or missing matching
checksum entries are errors. SHA-256 confirms integrity against the published
GitHub metadata; it is not a separate publisher signature. Platform signing and
macOS notarization remain release-packaging concerns.

## Bounds and verification

Only HTTPS URLs for the configured GitHub repository's exact release asset are
accepted. Redirects are restricted to GitHub and its release asset CDN hosts,
with a maximum of eight hops. No shell is used to open installers. Version,
architecture, edition, installer filename, cache location and hash are validated
again at download and installation boundaries.

Metadata is limited to 2 MiB, checksum manifests to 64 KiB, and installers to
4 GiB. API/manifest requests have a 45-second deadline; installer requests have
a one-hour deadline and a 45-second read timeout. Failed or canceled downloads
remove temporary files. An existing corrupt cache file is reported and preserved
instead of being executed or silently overwritten.

Tests exercise SemVer selection, architecture and edition separation, untrusted
URLs, ambiguous releases, checksum parsing, HTTP status/size limits, actual
streamed installer bytes, atomic publication, cancellation, and tamper detection
before launch. HTTP fixtures use in-memory responses and do not contact a live
release, open installers or test platform installation interactively.

Primary API references: [GitHub latest release](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)
and [GitHub release assets](https://docs.github.com/en/rest/releases/assets#get-a-release-asset).
