# Rust desktop updates

`yt-updater` replaces Electron's updater with a Rust client for the existing public
release repository, `ElWapoteDev/yt-ultimate-releases`. It never publishes releases
or uses account credentials. Downloading and installing are separate operations.

## Application lifecycle

The desktop root owns `updates::use_updates(state)` and renders `UpdatePrompt`.
Checks start after 20 seconds and repeat every six hours. Automatic checking and
downloading default to enabled and can be disabled in Settings. Updates continue
when Settings is unmounted. The downloaded prompt offers **Reiniciar ahora** and
**Más tarde**. Installing on exit requires the separately saved **Instalar al
salir** preference, which defaults to disabled.

Installation first cancels active media jobs and waits for their cancellation
registrations and imports to finish. A two-minute timeout leaves the application
open rather than killing workers. The UI calls `Updater::handoff_install` on a
blocking worker, retains its `InstallGuard`, and closes only after the helper
acknowledges readiness. If another job starts during verification, the handoff
is aborted and the application remains open.

The executable calls `handle_update_arguments()` before opening the UI. Helper
mode is the same Rust executable, without Python, Node, or shell scripts. It
waits for both the parent's file lock and actual process termination, then checks
the installer hash again. A failed installation reopens the existing app and
records an actionable result for the next launch. Logs exclude credentials,
request URLs and the handoff manifest.

- **macOS:** only an installed `.app` with bundle ID `com.ytultimate.desktop`
  can self-update. The OS utility `hdiutil` mounts the verified DMG read-only.
  Rust checks bundle identity/version and validates the original code signature
  using Security.framework, including the project's ad-hoc signatures. It copies
  the app beside the existing installation, preserves executable permissions and
  internal relative symlinks, validates the copy, and atomically exchanges the
  bundles using Darwin `RENAME_SWAP`. Validation/launch failure triggers rollback.
  Recovery material is retained if an I/O failure prevents rollback. The profile
  and library are never part of the replaced bundle. An unwritable installation
  is reported; the user can still choose **Abrir instalador** for manual copying.
- **Windows:** after the old process exits, the verified per-user NSIS installer
  starts with `/S`, adding `/R` for an explicitly requested restart. The existing
  installer preserves the application profile and handles migration. The helper
  exits immediately so it cannot block replacement of the executable.

`Updater::open_installer` remains a separate explicit manual option. It verifies
again, then opens the DMG or launches NSIS without closing the app itself.

## API and release contract

```rust,no_run
use yt_updater::{CancellationToken, Edition, Updater};

# async fn example() -> yt_updater::Result<()> {
let updater = Updater::with_edition("5.0.0", "profile/Updates", Edition::Base)?;
if let Some(info) = updater.check().await? {
    let verified = updater.download(&info, CancellationToken::new(), |_| {}).await?;
    // Run the following on a blocking worker only after installation is chosen.
    // let guard = updater.handoff_install(&verified, true)?;
    // Keep guard alive, then close gracefully; the helper waits for process exit.
    println!("Verified version: {}", verified.info().version);
}
# Ok(())
# }
```

`Updater::new` selects Base. Builds with `native-stems` use Full; a user-editable
setting cannot switch editions. `for_developer_repository` is an explicit
developer override, while production uses the original repository.

`check` selects only newer stable SemVer releases matching architecture and
edition. It ignores drafts, prereleases, equal-version build metadata, and older
versions. Missing compatible assets produce `None`.

| Platform | Base | Full |
| --- | --- | --- |
| macOS Apple Silicon | `YT Ultimate_<version>_aarch64.dmg` | `YT Ultimate_<version>_aarch64_full.dmg` |
| macOS Intel | `YT Ultimate_<version>_x64.dmg` | `YT Ultimate_<version>_x64_full.dmg` |
| Windows x64 | `yt-ultimate_<version>_x64-setup.exe` | `yt-ultimate_<version>_x64_full-setup.exe` |

Mac prefixes `YT.Ultimate` and `YT-Ultimate` are also accepted because upload tools
can normalize spaces. Exactly one matching installer is allowed. Each asset
requires a GitHub `sha256:` digest or an exact filename entry in `SHA256SUMS`.
If both exist they must agree. Hashes authenticate bytes against the published
GitHub metadata; they are not a separate publisher signature. Notarization and
Developer ID signing remain release-packaging concerns.

## Bounds and validation

Only HTTPS URLs for exact assets in the configured repository are accepted.
Redirects stay on GitHub and its release CDN, with eight hops maximum. Metadata
is limited to 2 MiB, checksum manifests to 64 KiB, installers to 4 GiB, and Mac
bundle copies to 12 GiB/100,000 entries. Bundle symlinks must remain inside the
bundle. API requests have a 45-second deadline; downloads have a one-hour total
and 45-second read deadline. Disk utility calls and helper waits are bounded.

Downloads use temporary files and exact size/SHA checks before atomic no-clobber
publication. Canceled/failed downloads remove partials. Cached installers are
reverified before reuse and installation; corrupt caches are reported rather
than executed. Handoff files contain release metadata, never account cookies.

The automated suite covers selection, trusted hosts, checksums, real streamed
fixture bytes, cancellation, no-clobber publication, pre-launch tampering, helper
lock/process waits, Mac atomic rollback, internal links/permissions, and native
ad-hoc signature/resource validation. App tests cover updater state surviving
Settings unmount and explicit persisted exit preferences. The separately invoked
`isolated_dmg_install_preserves_profile_and_replaces_only_test_bundle` test creates
and mounts a synthetic signed DMG, replaces only a temporary test application,
and verifies its signature and untouched sibling media; it passed on macOS.
No test installs over the user's app. Windows helper execution requires Windows
CI; earlier installer migration smoke tests do not constitute that new test.

Primary API references: [GitHub latest release](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)
and [GitHub release assets](https://docs.github.com/en/rest/releases/assets#get-a-release-asset).
