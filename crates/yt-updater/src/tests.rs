use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn asset(name: &str, tag: &str) -> serde_json::Value {
    json!({"name":name,"state":"uploaded","size":10,"digest":format!("sha256:{}","a".repeat(64)),"browser_download_url":format!("https://github.com/{RELEASE_REPOSITORY}/releases/download/{tag}/{name}")})
}

fn release(tag: &str, assets: Vec<serde_json::Value>) -> metadata::Release {
    serde_json::from_value(json!({"tag_name":tag,"draft":false,"prerelease":false,"assets":assets}))
        .unwrap()
}

#[test]
fn release_selection_uses_semver_and_exact_architecture() {
    let current = Version::parse("5.9.0").unwrap();
    let name = "yt-ultimate_5.10.0_x64-setup.exe";
    let selected = release("v5.10.0", vec![asset(name, "v5.10.0")]);
    assert!(metadata::select_release(
        &selected,
        &current,
        Platform::WindowsX64,
        Edition::Base,
        RELEASE_REPOSITORY
    )
    .unwrap()
    .is_some());
    assert!(metadata::select_release(
        &selected,
        &current,
        Platform::MacArm64,
        Edition::Base,
        RELEASE_REPOSITORY
    )
    .unwrap()
    .is_none());
    for tag in ["v4.99.0", "v5.9.0", "v5.9.0+build.3", "v6.0.0-beta.1"] {
        assert!(metadata::select_release(
            &release(tag, vec![]),
            &current,
            Platform::WindowsX64,
            Edition::Base,
            RELEASE_REPOSITORY
        )
        .unwrap()
        .is_none());
    }
    assert!(metadata::select_release(
        &release("not-a-version", vec![]),
        &current,
        Platform::WindowsX64,
        Edition::Base,
        RELEASE_REPOSITORY
    )
    .is_err());
    let mut draft = selected;
    draft.draft = true;
    assert!(metadata::select_release(
        &draft,
        &current,
        Platform::WindowsX64,
        Edition::Base,
        RELEASE_REPOSITORY
    )
    .unwrap()
    .is_none());
    draft.draft = false;
    draft.prerelease = true;
    assert!(metadata::select_release(
        &draft,
        &current,
        Platform::WindowsX64,
        Edition::Base,
        RELEASE_REPOSITORY
    )
    .unwrap()
    .is_none());
}

#[test]
fn installer_names_match_packager_version_and_architecture() {
    let version = Version::parse("5.1.0").unwrap();
    assert!(Platform::MacArm64.accepts_name(
        "YT Ultimate_5.1.0_aarch64.dmg",
        &version,
        Edition::Base
    ));
    assert!(Platform::MacArm64.accepts_name(
        "YT.Ultimate_5.1.0_aarch64.dmg",
        &version,
        Edition::Base
    ));
    assert!(Platform::MacX64.accepts_name("YT-Ultimate_5.1.0_x64.dmg", &version, Edition::Base));
    for name in [
        "YT Ultimate_5.1.0_x64.dmg",
        "YT Ultimate_5.0.0_aarch64.dmg",
        "YT Ultimate_5.1.0_aarch64.dmg.exe",
        "../YT Ultimate_5.1.0_aarch64.dmg",
    ] {
        assert!(!Platform::MacArm64.accepts_name(name, &version, Edition::Base));
    }
    assert!(!Platform::WindowsX64.accepts_name(
        "YT-Ultimate-Setup-5.1.0.exe",
        &version,
        Edition::Base
    ));
}

#[test]
fn editions_select_distinct_installers_with_identical_versions() {
    let current = Version::parse("5.0.0").unwrap();
    let version = Version::parse("5.1.0").unwrap();
    let base = "YT Ultimate_5.1.0_aarch64.dmg";
    let full = "YT Ultimate_5.1.0_aarch64_full.dmg";
    let release = release("v5.1.0", vec![asset(base, "v5.1.0"), asset(full, "v5.1.0")]);
    for (edition, expected) in [(Edition::Base, base), (Edition::Full, full)] {
        let chosen = metadata::select_release(
            &release,
            &current,
            Platform::MacArm64,
            edition,
            RELEASE_REPOSITORY,
        )
        .unwrap()
        .unwrap();
        assert_eq!(chosen.asset.name, expected);
    }
    assert!(!Platform::MacArm64.accepts_name(full, &version, Edition::Base));
    assert!(!Platform::MacArm64.accepts_name(base, &version, Edition::Full));
    assert!(Platform::MacX64.accepts_name(
        "YT Ultimate_5.1.0_x64_full.dmg",
        &version,
        Edition::Full
    ));
    assert!(!Platform::WindowsX64.accepts_name(
        "yt-ultimate_5.1.0_x64-setup.exe",
        &version,
        Edition::Full
    ));
    assert!(Platform::WindowsX64.accepts_name(
        "yt-ultimate_5.1.0_x64_full-setup.exe",
        &version,
        Edition::Full
    ));
    assert!(!Platform::WindowsX64.accepts_name(
        "yt-ultimate_5.1.0_x64_full-setup.exe",
        &version,
        Edition::Base
    ));
}

#[test]
fn rejects_ambiguous_assets_and_untrusted_urls() {
    let name = "yt-ultimate_5.1.0_x64-setup.exe";
    let current = Version::parse("5.0.0").unwrap();
    let duplicate = release("v5.1.0", vec![asset(name, "v5.1.0"), asset(name, "v5.1.0")]);
    assert!(metadata::select_release(
        &duplicate,
        &current,
        Platform::WindowsX64,
        Edition::Base,
        RELEASE_REPOSITORY
    )
    .is_err());
    for value in [
        "http://github.com/ElWapoteDev/yt-ultimate-releases/releases/download/v5.1.0/a.exe",
        "https://example.invalid/a.exe",
        "https://github.com/other/repo/releases/download/v5.1.0/a.exe",
    ] {
        assert!(
            metadata::validate_asset_url(value, RELEASE_REPOSITORY, "v5.1.0", "a.exe").is_err()
        );
    }
    for value in [
        "http://github.com/a",
        "https://github.com.evil.invalid/a",
        "https://user:password@github.com/a",
        "https://github.com:444/a",
    ] {
        assert!(!metadata::allowed_transport_url(
            &url::Url::parse(value).unwrap()
        ));
    }
    assert!(metadata::allowed_transport_url(
        &url::Url::parse("https://release-assets.githubusercontent.com/a").unwrap()
    ));
    assert!(!metadata::valid_repository("owner/../repo"));
}

#[test]
fn checksums_require_exact_names_and_a_single_valid_sha256() {
    let hash = "a".repeat(64);
    let name = "YT Ultimate_5.1.0_aarch64.dmg";
    assert_eq!(
        metadata::checksum_for(&format!("# comment\n{hash}  {name}\n"), name).unwrap(),
        hash
    );
    assert_eq!(
        metadata::checksum_for(&format!("{hash} *{name}"), name).unwrap(),
        hash
    );
    assert!(metadata::checksum_for(&format!("{hash}  {name}\n{hash}  {name}"), name).is_err());
    assert!(metadata::checksum_for(&format!("{hash}  other.dmg"), name).is_err());
    assert!(metadata::checksum_for("not-a-hash  file.dmg", "file.dmg").is_err());
    assert_eq!(
        metadata::parse_digest(&format!("sha256:{}", hash.to_ascii_uppercase())).unwrap(),
        Some(hash)
    );
    assert!(metadata::parse_digest("sha512:abc").is_err());
}

fn response(status: u16, content_type: &str, body: &[u8]) -> reqwest::Response {
    http::Response::builder()
        .status(status)
        .header("Content-Type", content_type)
        .header("Content-Length", body.len().to_string())
        .body(body.to_vec())
        .unwrap()
        .into()
}

fn info(body: &[u8]) -> UpdateInfo {
    UpdateInfo{version:"5.1.0".into(),name:"yt-ultimate_5.1.0_x64-setup.exe".into(),download_url:format!("https://github.com/{RELEASE_REPOSITORY}/releases/download/v5.1.0/yt-ultimate_5.1.0_x64-setup.exe"),release_url:format!("https://github.com/{RELEASE_REPOSITORY}/releases/tag/v5.1.0"),sha256:format!("{:x}",Sha256::digest(body)),size:body.len() as u64,platform:Platform::WindowsX64,edition:Edition::Base}
}

#[tokio::test]
async fn download_verifies_actual_bytes_before_atomic_publication() {
    let body = b"test installer binary bytes";
    let expected = info(body);
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join(&expected.name);
    let response = response(200, "application/octet-stream", body);
    let progress = Arc::new(Mutex::new(Vec::new()));
    let captured = progress.clone();
    download_response(
        response,
        &expected,
        &destination,
        CancellationToken::new(),
        move |value| captured.lock().unwrap().push(value),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), body);
    verify_file(&destination, &expected).unwrap();
    assert_eq!(
        progress.lock().unwrap().last().unwrap().downloaded_bytes,
        body.len() as u64
    );
    std::fs::write(&destination, b"modified installer").unwrap();
    assert!(verify_file(&destination, &expected).is_err());
}

#[tokio::test]
async fn failed_hash_cancellation_and_http_errors_leave_no_installer() {
    let body = b"test installer";
    let temp = tempfile::tempdir().unwrap();
    for case in 0..4 {
        let mut expected = info(body);
        if case == 0 {
            expected.sha256 = "0".repeat(64);
        }
        let destination = temp.path().join(format!("case-{case}.exe"));
        let status = if case == 2 { 500 } else { 200 };
        let content_type = if case == 3 {
            "text/html"
        } else {
            "application/octet-stream"
        };
        let response = response(status, content_type, body);
        let token = CancellationToken::new();
        if case == 1 {
            token.cancel();
        }
        assert!(
            download_response(response, &expected, &destination, token, |_| {})
                .await
                .is_err()
        );
        assert!(!destination.exists());
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}

#[tokio::test]
async fn metadata_http_responses_are_bounded_and_errors_are_not_parsed_as_success() {
    let failed = response(500, "application/json", b"{}");
    assert!(bounded_response(failed, 100).await.is_err());
    let oversized = response(200, "application/json", &[b'x'; 200]);
    assert!(bounded_response(oversized, 100).await.is_err());
    let valid = response(200, "application/json", b"{}");
    assert_eq!(bounded_response(valid, 100).await.unwrap(), b"{}");
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn install_rechecks_cached_file_and_refuses_changed_bytes_without_launching() {
    let temp = tempfile::tempdir().unwrap();
    let updater = Updater::new("5.0.0", temp.path()).unwrap();
    let body = b"original bytes";
    let mut expected = info(body);
    expected.platform = Platform::current().unwrap();
    expected.name = match expected.platform {
        Platform::MacArm64 => "YT Ultimate_5.1.0_aarch64.dmg",
        Platform::MacX64 => "YT Ultimate_5.1.0_x64.dmg",
        Platform::WindowsX64 => "yt-ultimate_5.1.0_x64-setup.exe",
    }
    .into();
    let mut url = url::Url::parse(&format!(
        "https://github.com/{RELEASE_REPOSITORY}/releases/download/v5.1.0/"
    ))
    .unwrap();
    url.path_segments_mut()
        .unwrap()
        .pop_if_empty()
        .push(&expected.name);
    expected.download_url = url.to_string();
    let path = temp.path().join(&expected.name);
    std::fs::write(&path, b"changed bytes!").unwrap();
    let update = VerifiedUpdate {
        path,
        info: expected,
    };
    assert!(matches!(
        updater.open_installer(&update),
        Err(UpdateError::Integrity)
    ));
}
