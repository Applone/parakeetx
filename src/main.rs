use std::process::ExitCode;

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("--help" | "-h") => {
            println!("parakeetx: local recording, transcription, and notes\n\nUsage: parakeetx [--help | --version | --doctor]\n\nLaunch without arguments for the desktop application.\nPARAKEETX_HOME overrides the configuration and data directory.\nPARAKEETX_API_KEY and HF_TOKEN provide optional API credentials.");
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("parakeetx {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--doctor") => match doctor() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("{error:#}"); ExitCode::FAILURE }
        },
        Some(argument) => { eprintln!("Unknown argument: {argument}. Use --help for usage."); ExitCode::FAILURE }
        None => match parakeetx::ui::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => { eprintln!("Unable to start parakeetx: {error}"); ExitCode::FAILURE }
        },
    }
}

fn doctor() -> anyhow::Result<()> {
    let paths = parakeetx::settings::AppPaths::discover()?;
    let settings = parakeetx::settings::Settings::load(&paths)?;
    println!("Configuration: {}", paths.config.display());
    println!("Library: {}", paths.database().display());
    println!("{} weights: {} ({})", settings.model, settings.model_path().display(), if parakeetx::download::model_ready(settings.model, &settings.model_path()) { "present" } else { "not downloaded" });
    if settings.uses_vad() { println!("Silero weights: {} ({})", settings.vad_path().display(), if settings.vad_path().is_file() { "present" } else { "not downloaded" }); }
    let devices = parakeetx::capture::devices();
    for device in devices.microphones { println!("Microphone: {} [{}]", device.label, device.id); }
    for device in devices.system { println!("System audio: {} [{}]", device.label, device.id); }
    if let Some(note) = devices.note { println!("Audio: {note}"); }
    println!("Python is {}", if settings.diarization_enabled { "enabled for pyannote diarization" } else { "not required (diarization disabled)" });
    Ok(())
}
