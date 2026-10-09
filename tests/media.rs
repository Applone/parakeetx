use std::{path::PathBuf, process::Command, sync::atomic::AtomicBool};

use parakeetx::audio;

#[test]
#[ignore = "requires ffmpeg and .test-cache/jfk.wav to generate compressed fixtures"]
fn native_mp3_and_video_mp4_import_without_external_decoders() {
    let directory = tempfile::tempdir().unwrap();
    let sample = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".test-cache/jfk.wav");
    for extension in ["mp3", "mp4"] {
        let media = directory.path().join(format!("input.{extension}"));
        let mut encode = Command::new("ffmpeg");
        encode.args(["-v", "error", "-nostdin"]);
        if extension == "mp4" {
            encode.args(["-f", "lavfi", "-i", "color=c=black:s=160x90:r=10"]);
        }
        encode.arg("-i").arg(&sample);
        if extension == "mp4" { encode.args(["-c:v", "mpeg4", "-c:a", "aac", "-shortest"]); }
        let output = encode.arg(&media).output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let normalized = directory.path().join(format!("{extension}.wav"));
        let duration = audio::import_media(&media, &normalized, &AtomicBool::new(false), |_| {}).unwrap();
        assert!((10.0..12.0).contains(&duration), "{extension}: {duration}");
        let samples = audio::read_range(&normalized, 0, 30 * 16_000).unwrap();
        assert!(samples.iter().any(|sample| sample.abs() > 0.1));
        assert!(media.exists());
    }
}
