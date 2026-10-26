use std::{env, fs, path::{Path, PathBuf}, process::Command, sync::atomic::{AtomicBool, Ordering}, time::Duration};

use anyhow::{Context, Result, ensure};

use crate::{diarize, process, settings::{AppPaths, Settings, atomic_write, private_directory}};

const REQUIREMENTS: &str = include_str!("../python/requirements.txt");
const READY: &str = concat!("# torch-backend=auto\n", include_str!("../python/requirements.txt"));
pub const UV_INSTALL_URL: &str = "https://docs.astral.sh/uv/getting-started/installation/";

#[derive(Clone, Debug)]
pub struct Detection {
    pub uv: Option<PathBuf>,
    pub uv_status: String,
    pub dependencies: std::result::Result<String, String>,
    pub managed_python: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct SetupResult {
    pub interpreter: PathBuf,
    pub description: String,
}

fn command(uv: &Path) -> Command {
    let mut command = Command::new(uv);
    // GUI apps may inherit an activated environment or unrelated uv configuration.
    for (name, _) in env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("UV_") || name_text.starts_with("PIP_") || matches!(name_text.as_ref(), "VIRTUAL_ENV" | "CONDA_PREFIX" | "PYTHONPATH" | "PYTHONHOME" | "HF_TOKEN" | "HUGGING_FACE_HUB_TOKEN" | "PARAKEETX_API_KEY") {
            command.env_remove(name);
        }
    }
    command.args(["--no-config", "--no-progress", "--color", "never"]);
    command
}

fn uv_candidates(path: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Vec<PathBuf> {
    let executable = if cfg!(windows) { "uv.exe" } else { "uv" };
    let mut candidates = Vec::new();
    if let Some(path) = path {
        candidates.extend(env::split_paths(path).filter(|path| path.is_absolute()).map(|path| path.join(executable)));
    }
    if let Some(home) = home {
        candidates.extend([home.join(".local/bin").join(executable), home.join(".cargo/bin").join(executable)]);
    }
    #[cfg(target_os = "macos")]
    candidates.extend([PathBuf::from("/opt/homebrew/bin/uv"), PathBuf::from("/usr/local/bin/uv")]);
    candidates
}

fn validate_uv(uv: &Path, cancel: &AtomicBool) -> Result<String> {
    ensure!(uv.is_absolute() && uv.is_file(), "Select an existing uv executable using its absolute path");
    let version = process::run(command(uv).arg("--version"), cancel, Duration::from_secs(10), "Checking uv")?;
    ensure!(version.trim_start().starts_with("uv "), "The selected executable is not uv");
    let help = process::run(command(uv).args(["pip", "install", "--help"]), cancel, Duration::from_secs(10), "Checking uv capabilities")?;
    ensure!(cfg!(target_os = "macos") || help.contains("--torch-backend"), "Update uv to support automatic PyTorch backend selection, then check again");
    Ok(version.trim().to_owned())
}

fn find_uv(selected: Option<&Path>, cancel: &AtomicBool) -> (Option<PathBuf>, String) {
    if let Some(path) = selected {
        return match validate_uv(path, cancel) {
            Ok(version) => (Some(path.to_owned()), format!("{version} · {}", path.display())),
            Err(error) => (None, format!("{error:#}")),
        };
    }
    let home = directories::BaseDirs::new().map(|paths| paths.home_dir().to_owned());
    let mut last_error = "uv was not found. Install uv or browse to its executable, then check again.".to_owned();
    for candidate in uv_candidates(env::var_os("PATH").as_deref(), home.as_deref()) {
        if cancel.load(Ordering::Relaxed) { break; }
        if !candidate.is_file() { continue; }
        match validate_uv(&candidate, cancel) {
            Ok(version) => return (Some(candidate.clone()), format!("{version} · {}", candidate.display())),
            Err(error) => last_error = format!("{error:#}"),
        }
    }
    (None, last_error)
}

pub fn environment_root(settings: &Settings) -> PathBuf { settings.models_dir.join("diarization").join("envs") }

fn setup_command(uv: &Path, settings: &Settings) -> Command {
    let mut command = command(uv);
    command.env("UV_CACHE_DIR", settings.models_dir.join("diarization/cache"))
        .env("UV_PYTHON_INSTALL_DIR", settings.models_dir.join("diarization/python"));
    command
}

pub fn interpreter(environment: &Path) -> PathBuf {
    environment.join(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" })
}

fn managed_candidates(root: &Path, markers: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut candidates: Vec<_> = entries.flatten().filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| fs::read_to_string(entry.path().join("ready.txt")).is_ok_and(|contents| markers.contains(&contents.as_str())))
        .collect();
    candidates.sort_by_key(|entry| std::cmp::Reverse(entry.metadata().and_then(|metadata| metadata.modified()).ok()));
    candidates.into_iter().map(|entry| interpreter(&entry.path())).collect()
}

pub fn detect(paths: &AppPaths, settings: &Settings, selected_uv: Option<&Path>, cancel: &AtomicBool) -> Detection {
    detect_with_probe(paths, settings, selected_uv, cancel, diarize::probe)
}

fn detect_with_probe(paths: &AppPaths, settings: &Settings, selected_uv: Option<&Path>, cancel: &AtomicBool, mut probe: impl FnMut(&Settings, &AtomicBool) -> Result<String>) -> Detection {
    let dependencies = probe(settings, cancel).map_err(|error| format!("{error:#}"));
    let (uv, uv_status) = find_uv(selected_uv, cancel);
    let mut managed_python = None;
    // Keep old managed environments discoverable without moving their absolute paths.
    let mut candidates = managed_candidates(&environment_root(settings), &[READY, REQUIREMENTS]);
    candidates.extend(managed_candidates(&paths.data.join("diarization/envs"), &[READY, REQUIREMENTS]));
    for python in candidates {
        if cancel.load(Ordering::Relaxed) { break; }
        if python.to_string_lossy() == settings.python_path && dependencies.is_ok() {
            managed_python = Some(python);
            break;
        }
        let mut candidate = settings.clone();
        candidate.python_path = python.to_string_lossy().into_owned();
        if probe(&candidate, cancel).is_ok() { managed_python = Some(python); break; }
    }
    Detection { uv, uv_status, dependencies, managed_python }
}

pub fn install(settings: &Settings, uv: &Path, cancel: &AtomicBool, mut progress: impl FnMut(&str, f32)) -> Result<SetupResult> {
    validate_uv(uv, cancel)?;
    install_with_probe(settings, uv, cancel, &mut progress, diarize::probe)
}

fn install_with_probe(settings: &Settings, uv: &Path, cancel: &AtomicBool, progress: &mut impl FnMut(&str, f32), mut probe: impl FnMut(&Settings, &AtomicBool) -> Result<String>) -> Result<SetupResult> {
    ensure!(!cancel.load(Ordering::Relaxed), "Setup cancelled");
    // Old CPU-only environments remain selectable, but must not prevent a CUDA-capable setup.
    for python in managed_candidates(&environment_root(settings), &[READY]) {
        ensure!(!cancel.load(Ordering::Relaxed), "Setup cancelled");
        let mut candidate = settings.clone();
        candidate.python_path = python.to_string_lossy().into_owned();
        progress("Checking existing managed environment", 0.0);
        if let Ok(description) = probe(&candidate, cancel) {
            ensure!(!cancel.load(Ordering::Relaxed), "Setup cancelled");
            progress("Existing speaker identification environment is ready", 1.0);
            return Ok(SetupResult { interpreter: python, description });
        }
    }
    ensure!(!cancel.load(Ordering::Relaxed), "Setup cancelled");
    let root = environment_root(settings);
    private_directory(&root)?;
    // Keep the final path from creation onward: virtual environments contain absolute paths.
    let environment = tempfile::Builder::new().prefix("python312-auto-").tempdir_in(&root)?;
    let python = interpreter(environment.path());
    let mut candidate = settings.clone();
    candidate.python_path = python.to_str().context("Environment path contains invalid characters")?.to_owned();
    progress("Creating Python 3.12 environment (Python may be downloaded)", 0.05);
    process::run(setup_command(uv, settings).args(["venv", "--python", "3.12"]).arg(environment.path()), cancel, Duration::from_secs(300), "Creating Python environment")?;
    let requirements = environment.path().join("requirements.txt");
    atomic_write(&requirements, REQUIREMENTS.as_bytes())?;
    progress("Installing pyannote.audio with automatic PyTorch backend selection", 0.2);
    let mut install = setup_command(uv, settings);
    install.args(["pip", "install", "--python"]).arg(&python).arg("--requirements").arg(&requirements);
    if !cfg!(target_os = "macos") { install.args(["--torch-backend", "auto"]); }
    process::run(&mut install, cancel, Duration::from_secs(1800), "Installing speaker identification dependencies")?;
    progress("Validating Python and pyannote.audio", 0.9);
    let description = probe(&candidate, cancel)?;
    ensure!(!cancel.load(Ordering::Relaxed), "Setup cancelled");
    atomic_write(&environment.path().join("ready.txt"), READY.as_bytes())?;
    let _ = environment.keep();
    progress("Speaker identification dependencies are ready", 1.0);
    Ok(SetupResult { interpreter: python, description })
}

pub fn model_url(model: &str) -> Option<String> {
    let model = model.trim();
    let parts: Vec<_> = model.split('/').collect();
    if parts.len() != 2 || parts.iter().any(|part| part.is_empty() || *part == "." || *part == ".." || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))) { return None; }
    Some(format!("https://huggingface.co/{model}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "downloads Python/pyannote dependencies; requires installed uv and internet access"]
    fn real_auto_environment_setup() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths { config: directory.path().join("config"), data: directory.path().join("data") };
        let settings = Settings::for_paths(&paths);
        settings.prepare_directories().unwrap();
        let cancel = AtomicBool::new(false);
        let (uv, message) = find_uv(None, &cancel);
        let uv = uv.unwrap_or_else(|| panic!("{message}"));
        let result = install(&settings, &uv, &cancel, |label, _| eprintln!("{label}")).unwrap();
        assert!(result.interpreter.is_file());
        assert!(result.description.contains("CPU") || result.description.contains("CUDA"));
        let reused = install(&settings, &uv, &cancel, |_, _| {}).unwrap();
        assert_eq!(reused.interpreter, result.interpreter);
    }

    #[test]
    fn discovery_uses_absolute_search_paths_and_model_links_cannot_escape_hugging_face() {
        let root = tempfile::tempdir().unwrap();
        let search = env::join_paths([root.path().join("tools with spaces"), PathBuf::from("relative")]).unwrap();
        let paths = uv_candidates(Some(&search), Some(root.path()));
        assert!(paths.iter().all(|path| path.is_absolute()));
        assert!(paths[0].starts_with(root.path().join("tools with spaces")));
        assert!(paths.iter().any(|path| path.starts_with(root.path().join(".local/bin"))));
        assert_eq!(model_url("pyannote/speaker-diarization-community-1").unwrap(), "https://huggingface.co/pyannote/speaker-diarization-community-1");
        for invalid in ["https://example.com/model", "../model", "org/../model", "org/model?token=secret", "org/"] { assert!(model_url(invalid).is_none()); }
        assert!(interpreter(root.path()).ends_with(if cfg!(windows) { "Scripts/python.exe" } else { "bin/python" }));
    }

    #[cfg(unix)]
    fn fake_uv(root: &Path, failure: bool, supported: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("uv with spaces");
        let source = format!(r#"#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(__file__ + '.log', 'a') as log:
    log.write(json.dumps(args) + '\n')
assert not any(key in os.environ for key in ('HF_TOKEN', 'HUGGING_FACE_HUB_TOKEN', 'PARAKEETX_API_KEY', 'VIRTUAL_ENV', 'CONDA_PREFIX', 'UV_SYSTEM_PYTHON'))
if '--version' in args:
    print('uv 0.10.0')
elif '--help' in args:
    print('{}')
elif 'venv' in args:
    target = pathlib.Path(args[-1])
    assert pathlib.Path(os.environ['UV_CACHE_DIR']) == target.parent.parent / 'cache'
    assert pathlib.Path(os.environ['UV_PYTHON_INSTALL_DIR']) == target.parent.parent / 'python'
    (target / 'bin').mkdir(exist_ok=True)
    (target / 'bin' / 'python').symlink_to(sys.executable)
elif 'install' in args:
    if {}:
        print('simulated install failure', file=sys.stderr)
        sys.exit(1)
    assert pathlib.Path(args[args.index('--requirements') + 1]).read_text().find('pyannote.audio') >= 0
"#, if supported { "--torch-backend" } else { "old uv" }, if failure { "True" } else { "False" });
        fs::write(&path, source).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn managed_setup_embeds_requirements_selects_backend_automatically_and_keeps_only_validated_environments() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths { config: directory.path().join("config"), data: directory.path().join("data with spaces") };
        let settings = Settings { models_dir: directory.path().join("external models"), ..Settings::for_paths(&paths) };
        let original = settings.python_path.clone();
        let uv = fake_uv(directory.path(), false, true);
        let cancel = AtomicBool::new(false);
        assert!(validate_uv(&uv, &cancel).unwrap().starts_with("uv "));
        let result = install_with_probe(&settings, &uv, &cancel, &mut |_, _| {}, |candidate, _| {
            assert!(Path::new(&candidate.python_path).is_file());
            Ok("pyannote.audio test on Python 3.12 (CPU)".into())
        }).unwrap();
        assert!(result.interpreter.is_file());
        assert!(result.interpreter.starts_with(settings.models_dir.join("diarization/envs")));
        assert!(!paths.data.join("diarization").exists());
        assert_eq!(settings.python_path, original);
        let environments: Vec<_> = fs::read_dir(environment_root(&settings)).unwrap().collect();
        assert_eq!(environments.len(), 1);
        let commands = fs::read_to_string(uv.with_file_name("uv with spaces.log")).unwrap();
        assert!(commands.contains("3.12"));
        assert!(commands.contains("--python"));
        if !cfg!(target_os = "macos") { assert!(commands.contains("--torch-backend") && commands.contains("\"auto\"")); }
        let detection = detect_with_probe(&paths, &settings, Some(&uv), &cancel, |candidate, _| {
            if candidate.python_path == original { anyhow::bail!("missing dependencies") } else { Ok("ready".into()) }
        });
        assert!(detection.dependencies.is_err());
        assert_eq!(detection.managed_python, Some(result.interpreter.clone()));
        let failed = install_with_probe(&settings, &uv, &cancel, &mut |_, _| {}, |_, _| anyhow::bail!("probe failed"));
        assert!(failed.is_err());
        assert_eq!(fs::read_dir(environment_root(&settings)).unwrap().count(), 1);
        assert!(result.interpreter.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn automatic_setup_preserves_legacy_cpu_environments_and_reuses_new_environments() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths { config: directory.path().join("config"), data: directory.path().join("data") };
        let settings = Settings::for_paths(&paths);
        let legacy = environment_root(&settings).join("python312-cpu-old");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("ready.txt"), REQUIREMENTS).unwrap();
        let uv = fake_uv(directory.path(), false, true);
        let cancel = AtomicBool::new(false);
        let result = install_with_probe(&settings, &uv, &cancel, &mut |_, _| {}, |candidate, _| {
            assert_ne!(Path::new(&candidate.python_path), interpreter(&legacy));
            Ok("CUDA environment ready".into())
        }).unwrap();
        assert_eq!(fs::read_dir(environment_root(&settings)).unwrap().count(), 2);
        assert_eq!(fs::read_to_string(legacy.join("ready.txt")).unwrap(), REQUIREMENTS);
        let log = uv.with_file_name("uv with spaces.log");
        let commands = fs::read(&log).unwrap();
        let reused = install_with_probe(&settings, &uv, &cancel, &mut |_, _| {}, |candidate, _| {
            assert_eq!(Path::new(&candidate.python_path), result.interpreter);
            Ok("CUDA environment ready".into())
        }).unwrap();
        assert_eq!(reused.interpreter, result.interpreter);
        assert_eq!(fs::read(log).unwrap(), commands);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_prefers_configured_models_and_preserves_legacy_environments() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths { config: directory.path().join("config"), data: directory.path().join("data") };
        let settings = Settings { models_dir: directory.path().join("external models"), ..Settings::for_paths(&paths) };
        let configured = environment_root(&settings).join("current");
        let legacy = paths.data.join("diarization/envs/old");
        for environment in [&configured, &legacy] {
            fs::create_dir_all(environment).unwrap();
            fs::write(environment.join("ready.txt"), REQUIREMENTS).unwrap();
        }
        let uv = fake_uv(directory.path(), false, true);
        let detect = || detect_with_probe(&paths, &settings, Some(&uv), &AtomicBool::new(false), |candidate, _| {
            ensure!(candidate.python_path != settings.python_path, "selected interpreter unavailable");
            Ok("ready".into())
        });
        assert_eq!(detect().managed_python, Some(interpreter(&configured)));
        fs::remove_file(configured.join("ready.txt")).unwrap();
        assert_eq!(detect().managed_python, Some(interpreter(&legacy)));
        assert_eq!(fs::read_to_string(legacy.join("ready.txt")).unwrap(), REQUIREMENTS);
    }

    #[cfg(unix)]
    #[test]
    fn failures_cancellation_and_unsupported_uv_are_actionable_and_clean() {
        let directory = tempfile::tempdir().unwrap();
        let paths = AppPaths { config: directory.path().join("config"), data: directory.path().join("data") };
        let settings = Settings::for_paths(&paths);
        let uv = fake_uv(directory.path(), true, true);
        let cancel = AtomicBool::new(false);
        let error = install_with_probe(&settings, &uv, &cancel, &mut |_, _| {}, |_, _| unreachable!()).unwrap_err();
        assert!(error.to_string().contains("simulated install failure"));
        assert_eq!(fs::read_dir(environment_root(&settings)).unwrap().count(), 0);
        let uv = fake_uv(directory.path(), false, true);
        let result = install_with_probe(&settings, &uv, &cancel, &mut |_, fraction| {
            if fraction >= 0.2 { cancel.store(true, Ordering::Relaxed); }
        }, |_, _| unreachable!());
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert_eq!(fs::read_dir(environment_root(&settings)).unwrap().count(), 0);
        let uv = fake_uv(directory.path(), false, false);
        let result = validate_uv(&uv, &AtomicBool::new(false));
        if !cfg!(target_os = "macos") { assert!(result.unwrap_err().to_string().contains("Update uv")); }
        let (uv, message) = find_uv(Some(&directory.path().join("missing")), &AtomicBool::new(false));
        assert!(uv.is_none());
        assert!(message.contains("existing uv executable"));
    }
}
