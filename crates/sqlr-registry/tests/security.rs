use std::io::Write;

use sqlr_registry::{Fetcher, ManualInspection, Registry, RegistryError, sha256_hex};

const DRIVER_TOML: &[u8] = b"id = \"echo\"\nversion = \"1.0.0\"\nexecutable = \"echo\"\n";

fn registry(dir: &std::path::Path) -> Registry {
    Registry::new(dir.to_path_buf(), String::new(), "")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn manifest_archive(manifest: &[u8]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(manifest.len() as u64);
    header.set_cksum();
    archive
        .append_data(&mut header, "./driver.toml", manifest)
        .unwrap();
    gzip(&archive.into_inner().unwrap())
}

#[tokio::test]
async fn oversized_local_download_is_refused_before_opening_destination() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("oversized.tar.gz");
    std::fs::File::create(&source)
        .unwrap()
        .set_len(1024 * 1024 * 1024 + 1)
        .unwrap();
    // A missing parent also keeps the unfixed implementation from copying a GiB.
    let dest = dir.path().join("missing/download.tar.gz");
    let error = Fetcher::new()
        .download_to(&format!("file://{}", source.display()), &dest, &|_, _| {})
        .await
        .unwrap_err();
    assert!(
        matches!(error, RegistryError::Fetch { ref reason, .. } if reason.contains("size limit")),
        "oversized input must be refused before touching the destination: {error}"
    );
    assert!(!dest.exists());
}

#[tokio::test]
async fn manual_inspection_refuses_oversized_entries_before_skipping_their_body() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("oversized-entry.tar.gz");
    let mut header = tar::Header::new_gnu();
    header.set_path("payload").unwrap();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(2 * 1024 * 1024 * 1024 + 1);
    header.set_cksum();
    // No body is needed: checking the header must precede consuming its payload.
    std::fs::write(&file, gzip(header.as_bytes())).unwrap();
    let error = registry(dir.path())
        .install_manual(&file)
        .await
        .unwrap_err();
    assert!(
        matches!(error, RegistryError::UnsafeArchive(ref reason) if reason.contains("size limit")),
        "oversized entry was consumed before its size was checked: {error}"
    );
}

#[tokio::test]
async fn manual_inspection_refuses_a_manifest_larger_than_64_kib() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("oversized-manifest.tar.gz");
    let mut manifest = DRIVER_TOML.to_vec();
    manifest.resize(64 * 1024 + 1, b' ');
    std::fs::write(&file, manifest_archive(&manifest)).unwrap();
    let error = registry(dir.path())
        .install_manual(&file)
        .await
        .unwrap_err();
    assert!(
        matches!(error, RegistryError::UnsafeArchive(ref reason) if reason.contains("driver.toml") && reason.contains("size limit")),
        "oversized manifests must be rejected, rather than truncated: {error}"
    );
}

#[tokio::test]
async fn manual_inspection_accepts_a_manifest_at_the_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("manifest-at-limit.tar.gz");
    let mut manifest = DRIVER_TOML.to_vec();
    manifest.resize(64 * 1024, b' ');
    let bytes = manifest_archive(&manifest);
    std::fs::write(&file, &bytes).unwrap();
    let inspection = registry(dir.path()).install_manual(&file).await.unwrap();
    assert_eq!(inspection.id, "echo");
    assert_eq!(inspection.version, "1.0.0");
    assert_eq!(inspection.sha256, sha256_hex(&bytes));
    assert_eq!(inspection.size, bytes.len() as u64);
}

#[tokio::test]
async fn manual_inspection_and_confirmation_refuse_oversized_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("manual.tar.gz");
    std::fs::write(&file, manifest_archive(DRIVER_TOML)).unwrap();
    let registry = registry(dir.path());
    let inspection = registry.install_manual(&file).await.unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap()
        .set_len(1024 * 1024 * 1024 + 1)
        .unwrap();
    let inspect_error = registry.install_manual(&file).await.unwrap_err();
    let confirm_error = registry.confirm_manual(&inspection).await.unwrap_err();
    for error in [inspect_error, confirm_error] {
        assert!(
            matches!(error, RegistryError::UnsafeArchive(ref reason) if reason.contains("size limit")),
            "the compressed file must be bounded before reading or hashing it: {error}"
        );
    }
    assert!(!dir.path().join("drivers").exists());
}

#[tokio::test]
async fn confirmation_cannot_bypass_the_manifest_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("oversized-manifest.tar.gz");
    let mut manifest = DRIVER_TOML.to_vec();
    manifest.resize(64 * 1024 + 1, b' ');
    let bytes = manifest_archive(&manifest);
    std::fs::write(&file, &bytes).unwrap();
    let inspection = ManualInspection {
        file,
        sha256: sha256_hex(&bytes),
        size: bytes.len() as u64,
        id: "echo".into(),
        version: "1.0.0".into(),
    };
    let error = registry(dir.path())
        .confirm_manual(&inspection)
        .await
        .unwrap_err();
    assert!(
        matches!(error, RegistryError::UnsafeArchive(ref reason) if reason.contains("driver.toml") && reason.contains("size limit")),
        "confirmation must check the full manifest's size before unpacking it: {error}"
    );
    assert!(!dir.path().join("drivers/echo").exists());
}

#[tokio::test]
async fn local_index_fetch_accepts_the_limit_and_refuses_one_more_byte() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("index.json");
    let source = std::fs::File::create(&file).unwrap();
    source.set_len(8 * 1024 * 1024).unwrap();
    let url = format!("file://{}", file.display());
    let fetcher = Fetcher::new();
    assert_eq!(
        fetcher.fetch_bytes(&url).await.unwrap().len(),
        8 * 1024 * 1024
    );
    source.set_len(8 * 1024 * 1024 + 1).unwrap();
    assert!(matches!(
        fetcher.fetch_bytes(&url).await,
        Err(RegistryError::Fetch { .. })
    ));
}
