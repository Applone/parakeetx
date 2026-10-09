use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use sha1::{Digest, Sha1};
use sha2::Sha256;

use crate::{domain::{TranscriptionModel, WhisperModel}, settings::private_directory};

pub const VAD_URL: &str = "https://raw.githubusercontent.com/snakers4/silero-vad/bfdc0193023f121ea5b3cc7b176dbed570a68a59/src/silero_vad/data/silero_vad.onnx";
pub const VAD_SHA1: &str = "2dad4d2d2c3fd4cde949d5f4939c0027935635d4";

#[derive(Debug, Clone, Copy)]
pub struct ModelFile {
    pub filename: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub bytes: u64,
}

pub const PARAKEET_FILES: [ModelFile; 3] = [
    ModelFile {
        filename: "encoder-model.int8.onnx",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/encoder-model.int8.onnx",
        sha256: "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09",
        bytes: 652_183_999,
    },
    ModelFile {
        filename: "decoder_joint-model.int8.onnx",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/decoder_joint-model.int8.onnx",
        sha256: "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70",
        bytes: 18_202_004,
    },
    ModelFile {
        filename: "vocab.txt",
        url: "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/vocab.txt",
        sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        bytes: 93_939,
    },
];

pub fn model_url(model: WhisperModel) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", model.filename())
}

pub fn model_ready(model: TranscriptionModel, path: &Path) -> bool {
    match model {
        TranscriptionModel::ParakeetTdt06bV3 => bundle_ready(path, &PARAKEET_FILES),
        TranscriptionModel::Whisper(_) => path.metadata().is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0),
    }
}

fn bundle_ready(directory: &Path, files: &[ModelFile]) -> bool {
    files.iter().all(|file| directory.join(file.filename).metadata().is_ok_and(|metadata| metadata.is_file() && metadata.len() == file.bytes))
}

pub fn fetch_model(model: TranscriptionModel, destination: &Path, cancel: &AtomicBool, progress: impl FnMut(f32)) -> Result<()> {
    match model {
        TranscriptionModel::ParakeetTdt06bV3 => fetch_bundle(destination, &PARAKEET_FILES, cancel, progress),
        TranscriptionModel::Whisper(model) => fetch(&model_url(model), destination, Some(model.sha1()), cancel, progress),
    }
}

fn fetch_bundle(destination: &Path, files: &[ModelFile], cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
    let mut verified = Vec::with_capacity(files.len());
    for file in files {
        let valid = digest_hashed::<Sha256>(&destination.join(file.filename), cancel)
            .is_ok_and(|actual| actual.eq_ignore_ascii_case(file.sha256));
        ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
        verified.push(valid);
    }
    if verified.iter().all(|valid| *valid) && bundle_ready(destination, files) {
        progress(1.0);
        return Ok(());
    }
    let parent = destination.parent().context("Model directory has no parent")?;
    private_directory(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let total: u64 = files.iter().map(|file| file.bytes).sum();
    let mut completed = 0u64;
    for (file, valid) in files.iter().zip(verified) {
        ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
        let target = staging.path().join(file.filename);
        if valid {
            std::fs::copy(destination.join(file.filename), &target)?;
            File::open(&target)?.sync_all()?;
        } else {
            fetch_sha256(file.url, &target, file.sha256, cancel, |fraction| {
                progress((completed as f64 + file.bytes as f64 * f64::from(fraction)) as f32 / total as f32);
            }).with_context(|| format!("Cannot download Parakeet {}", file.filename))?;
        }
        completed += file.bytes;
        progress(completed as f32 / total as f32);
    }
    ensure!(bundle_ready(staging.path(), files), "The Parakeet model download is incomplete");
    ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
    let backup = tempfile::tempdir_in(parent)?;
    let previous = backup.path().join("previous");
    let replacing = destination.exists();
    if replacing {
        ensure!(destination.is_dir(), "{} must be a model directory", destination.display());
        std::fs::rename(destination, &previous).context("Cannot prepare the model directory for replacement")?;
    }
    if let Err(error) = std::fs::rename(staging.path(), destination) {
        if replacing && let Err(restore_error) = std::fs::rename(&previous, destination) {
            let saved = backup.keep();
            anyhow::bail!("Cannot install model: {error}. Cannot restore previous model: {restore_error}. Previous files are preserved at {}", saved.join("previous").display());
        }
        return Err(error).context("Cannot install the downloaded model bundle");
    }
    progress(1.0);
    Ok(())
}

pub fn fetch(url: &str, destination: &Path, expected_sha1: Option<&str>, cancel: &AtomicBool, progress: impl FnMut(f32)) -> Result<()> {
    fetch_hashed::<Sha1>(url, destination, expected_sha1, cancel, progress)
}

pub fn fetch_sha256(url: &str, destination: &Path, expected_sha256: &str, cancel: &AtomicBool, progress: impl FnMut(f32)) -> Result<()> {
    fetch_hashed::<Sha256>(url, destination, Some(expected_sha256), cancel, progress)
}

fn fetch_hashed<Hash: Digest + Default>(url: &str, destination: &Path, expected_digest: Option<&str>, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
    if destination.is_file() && expected_digest.is_some_and(|expected| digest_hashed::<Hash>(destination, cancel).is_ok_and(|actual| actual.eq_ignore_ascii_case(expected))) {
        progress(1.0);
        return Ok(());
    }
    ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
    let parent = destination.parent().context("Destination has no parent directory")?;
    private_directory(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut hasher = Hash::new();
    crate::http::run(cancel, async {
        let client = crate::http::client_builder().connect_timeout(Duration::from_secs(30)).read_timeout(Duration::from_secs(60)).build()?;
        let mut response = client.get(url).send().await.context("Cannot reach the model download server")?;
        ensure!(response.status().is_success(), "Download failed with status {}", response.status());
        let total = response.content_length();
        let mut downloaded = 0u64;
        let mut reported = -1.0f32;
        while let Some(bytes) = response.chunk().await.context("The model download was interrupted")? {
            temporary.write_all(&bytes).context("Cannot write model data. Check available disk space.")?;
            hasher.update(&bytes);
            downloaded += bytes.len() as u64;
            ensure!(downloaded <= 5 * 1024 * 1024 * 1024, "Model download exceeds 5 GiB");
            if let Some(total) = total.filter(|total| *total > 0) {
                let fraction = (downloaded as f32 / total as f32).min(0.99);
                if fraction - reported >= 0.005 { progress(fraction); reported = fraction; }
            }
        }
        ensure!(downloaded > 0, "The download returned an empty file");
        if let Some(total) = total { ensure!(downloaded == total, "The downloaded model is incomplete"); }
        Ok(())
    })?;
    if let Some(expected) = expected_digest {
        let actual: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
        ensure!(actual.eq_ignore_ascii_case(expected), "Downloaded model checksum mismatch. The existing model has not been changed.");
    }
    ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
    temporary.as_file().sync_all()?;
    temporary.persist(destination).map_err(|error| error.error).context("Cannot install the downloaded model")?;
    progress(1.0);
    Ok(())
}

pub fn digest(path: &Path) -> Result<String> {
    digest_hashed::<Sha1>(path, &AtomicBool::new(false))
}

fn digest_hashed<Hash: Digest + Default>(path: &Path, cancel: &AtomicBool) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let mut hasher = Hash::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "Download cancelled");
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_point_at_published_artifacts() {
        assert_eq!(model_url(WhisperModel::LargeV3), "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin");
        assert!(VAD_URL.ends_with("silero_vad.onnx"));
        assert!(WhisperModel::ALL.iter().all(|model| model_url(*model).starts_with("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-")));
        for file in PARAKEET_FILES {
            assert!(file.url.contains("/resolve/8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce/"));
            assert!(file.url.ends_with(file.filename));
            assert_eq!(file.sha256.len(), 64);
            assert!(file.sha256.chars().all(|character| character.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn checksums_match_known_values_and_detect_corruption() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.bin");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(digest(&path).unwrap(), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert!(!digest(&path).unwrap().eq_ignore_ascii_case(WhisperModel::Tiny.sha1()));
        assert_eq!(digest_hashed::<Sha256>(&path, &AtomicBool::new(false)).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn cancelled_downloads_leave_no_partial_file() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("model.bin");
        let cancel = AtomicBool::new(true);
        assert!(fetch("http://127.0.0.1:1/x", &destination, None, &cancel, |_| {}).is_err());
        assert!(!destination.exists());
        assert!(!destination.with_extension("partial").exists());
        assert!(fetch_model(TranscriptionModel::default(), &destination, &cancel, |_| {}).is_err());
        assert!(!destination.exists());
    }

    #[test]
    fn parakeet_requires_every_complete_artifact() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!model_ready(TranscriptionModel::default(), directory.path()));
        for file in PARAKEET_FILES {
            let output = File::create(directory.path().join(file.filename)).unwrap();
            output.set_len(file.bytes).unwrap();
        }
        assert!(model_ready(TranscriptionModel::default(), directory.path()));
        File::create(directory.path().join("vocab.txt")).unwrap();
        assert!(!model_ready(TranscriptionModel::default(), directory.path()));
    }

    #[test]
    fn bundle_failure_preserves_all_previous_files() {
        use std::{net::TcpListener, thread};
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            for body in [b"abc".as_slice(), b"invalid"] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut request = [0u8; 4096];
                let count = stream.read(&mut request).unwrap();
                assert!(count > 0, "The download client sent no request");
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                stream.write_all(body).unwrap();
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("model");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("encoder"), b"previous encoder").unwrap();
        std::fs::write(destination.join("decoder"), b"previous decoder").unwrap();
        let url: &'static str = Box::leak(url.into_boxed_str());
        let file = ModelFile { filename: "encoder", url, sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", bytes: 3 };
        let files = [file, ModelFile { filename: "decoder", ..file }];
        assert!(fetch_bundle(&destination, &files, &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(std::fs::read(destination.join("encoder")).unwrap(), b"previous encoder");
        assert_eq!(std::fs::read(destination.join("decoder")).unwrap(), b"previous decoder");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        server.join().unwrap();
    }
}
