use std::{collections::VecDeque, fs::File, io::{BufReader, BufWriter}, path::Path, sync::atomic::{AtomicBool, Ordering}};

use anyhow::{Context, Result, bail, ensure};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use symphonia::core::{audio::SampleBuffer, codecs::{CODEC_TYPE_NULL, DecoderOptions}, errors::Error as DecodeError, formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint};

use crate::SAMPLE_RATE;

pub type WaveWriter = hound::WavWriter<BufWriter<File>>;
pub type WaveReader = hound::WavReader<BufReader<File>>;

pub fn wave_spec() -> hound::WavSpec {
    hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int }
}

pub fn write_samples(writer: &mut WaveWriter, samples: &[f32]) -> Result<()> {
    for &sample in samples {
        let finite = if sample.is_finite() { sample } else { 0.0 };
        writer.write_sample((finite.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)?;
    }
    Ok(())
}

pub fn open_wave(path: &Path) -> Result<WaveReader> {
    let reader = hound::WavReader::open(path).with_context(|| format!("Cannot open audio: {}", path.display()))?;
    let spec = reader.spec();
    ensure!(spec.channels == 1 && spec.sample_rate == SAMPLE_RATE && spec.bits_per_sample == 16 && spec.sample_format == hound::SampleFormat::Int,
        "Expected a 16 kHz mono PCM recording; import the audio again to normalize it");
    Ok(reader)
}

pub fn read_range(path: &Path, start: u64, length: usize) -> Result<Vec<f32>> {
    let mut reader = open_wave(path)?;
    ensure!(start <= u64::from(u32::MAX), "Recording exceeds the WAV seek limit");
    reader.seek(start as u32)?;
    reader.samples::<i16>().take(length)
        .map(|sample| sample.map(|sample| f32::from(sample) / 32768.0).map_err(Into::into))
        .collect()
}

pub fn import_media(source: &Path, destination: &Path, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<f64> {
    ensure!(source.is_file(), "Select an existing media file");
    ensure!(!destination.exists(), "Destination recording already exists");
    let stream = MediaSourceStream::new(Box::new(File::open(source)?), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = source.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }
    let probed = symphonia::default::get_probe().format(&hint, stream, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .context("Unsupported media container. Import MP3, MP4/M4A with AAC audio, WAV, FLAC, OGG, or MKV")?;
    let mut format = probed.format;
    let mut selected = None;
    for track in format.tracks() {
        if track.codec_params.codec != CODEC_TYPE_NULL && let Ok(decoder) = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()) {
            selected = Some((track.id, track.codec_params.n_frames, decoder));
            break;
        }
    }
    let (track_id, total_frames, mut decoder) = selected.context("No supported audio track was found. The file may contain video only or an unsupported codec.")?;
    let parent = destination.parent().context("Destination has no parent directory")?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut writer = hound::WavWriter::create(temporary.path(), wave_spec())?;
    let mut converter: Option<MonoResampler> = None;
    let mut sample_rate = 0;
    let mut written = 0usize;
    let mut decoded_frames = 0u64;
    loop {
        ensure!(!cancel.load(Ordering::Relaxed), "Import cancelled");
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(DecodeError::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(error) => return Err(error).context("Media container could not be read completely"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).context("Audio decoding failed; the original file has not been modified")?;
        let spec = *decoded.spec();
        let channels = spec.channels.count();
        ensure!(channels > 0 && spec.rate > 0, "Audio has an invalid sample format");
        if sample_rate != spec.rate {
            if let Some(previous) = converter.as_mut() {
                let tail = previous.finish()?;
                written += tail.len();
                write_samples(&mut writer, &tail)?;
            }
            sample_rate = spec.rate;
            converter = Some(MonoResampler::new(sample_rate)?);
        }
        let mut buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        let mono = downmix(buffer.samples(), channels);
        decoded_frames += mono.len() as u64;
        let resampled = converter.as_mut().context("Resampler was not initialized")?.push(&mono)?;
        written += resampled.len();
        write_samples(&mut writer, &resampled)?;
        if let Some(total) = total_frames.filter(|&count| count > 0) {
            progress((decoded_frames as f32 / total as f32).min(0.99));
        }
    }
    if let Some(converter) = converter.as_mut() {
        let tail = converter.finish()?;
        written += tail.len();
        write_samples(&mut writer, &tail)?;
    }
    ensure!(written > 0, "The file contains no decodable audio samples");
    writer.finalize()?;
    temporary.as_file().sync_all()?;
    ensure!(!cancel.load(Ordering::Relaxed), "Import cancelled");
    temporary.persist_noclobber(destination).map_err(|error| error.error)?;
    progress(1.0);
    Ok(written as f64 / f64::from(SAMPLE_RATE))
}

pub fn downmix(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    samples.chunks_exact(channels).map(|frame| {
        frame.iter().map(|sample| if sample.is_finite() { *sample } else { 0.0 }).sum::<f32>() / channels as f32
    }).collect()
}

pub struct MonoResampler {
    inner: Option<SincFixedIn<f32>>,
    pending: VecDeque<f32>,
    skip: usize,
    input_rate: u32,
    input_count: usize,
    output_count: usize,
}

impl MonoResampler {
    pub fn new(input_rate: u32) -> Result<Self> {
        ensure!((8000..=384_000).contains(&input_rate), "Unsupported sample rate: {input_rate}");
        let inner = if input_rate == SAMPLE_RATE { None } else {
            Some(SincFixedIn::new(
                f64::from(SAMPLE_RATE) / f64::from(input_rate), 1.0,
                SincInterpolationParameters { sinc_len: 128, f_cutoff: 0.95, interpolation: SincInterpolationType::Cubic, oversampling_factor: 128, window: WindowFunction::BlackmanHarris2 },
                1024, 1,
            )?)
        };
        let skip = inner.as_ref().map_or(0, |resampler| resampler.output_delay());
        Ok(Self { inner, pending: VecDeque::new(), skip, input_rate, input_count: 0, output_count: 0 })
    }

    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        self.input_count += samples.len();
        if self.inner.is_none() {
            self.output_count += samples.len();
            return Ok(samples.to_vec());
        }
        self.pending.extend(samples);
        let mut output = Vec::new();
        while self.pending.len() >= 1024 {
            let input: Vec<_> = self.pending.drain(..1024).collect();
            let processed = self.inner.as_mut().context("Missing resampler")?.process(&[input], None)?;
            self.append(&processed[0], &mut output);
        }
        Ok(output)
    }

    fn append(&mut self, samples: &[f32], output: &mut Vec<f32>) {
        let skip = self.skip.min(samples.len());
        self.skip -= skip;
        output.extend_from_slice(&samples[skip..]);
        self.output_count += samples.len() - skip;
    }

    pub fn finish(&mut self) -> Result<Vec<f32>> {
        if self.inner.is_none() {
            return Ok(Vec::new());
        }
        let target = (self.input_count as u64 * u64::from(SAMPLE_RATE)).div_ceil(u64::from(self.input_rate)) as usize;
        let remaining = target.saturating_sub(self.output_count);
        let mut output = Vec::new();
        let pending: Vec<_> = self.pending.drain(..).collect();
        let first = self.inner.as_mut().context("Missing resampler")?.process_partial(Some(&[pending]), None)?;
        self.append(&first[0], &mut output);
        for _ in 0..16 {
            if output.len() >= remaining {
                break;
            }
            let tail = self.inner.as_mut().context("Missing resampler")?.process_partial::<Vec<f32>>(None, None)?;
            self.append(&tail[0], &mut output);
        }
        if output.len() < remaining {
            bail!("Resampler failed to flush the remaining audio");
        }
        output.truncate(remaining);
        self.output_count = target;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_preserves_duration_at_common_rates() {
        for rate in [8000, 16_000, 22_050, 44_100, 48_000, 96_000] {
            let mut resampler = MonoResampler::new(rate).unwrap();
            let input: Vec<f32> = (0..rate).map(|index| (index as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin()).collect();
            let mut output = Vec::new();
            for chunk in input.chunks(713) {
                output.extend(resampler.push(chunk).unwrap());
            }
            output.extend(resampler.finish().unwrap());
            assert_eq!(output.len(), SAMPLE_RATE as usize, "rate {rate}");
            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!(output[1000..2000].iter().any(|sample| sample.abs() > 0.8));
        }
    }

    #[test]
    fn imports_stereo_wav_and_does_not_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("stereo.wav");
        let target = directory.path().join("normalized.wav");
        let mut writer = hound::WavWriter::create(&source, hound::WavSpec { channels: 2, sample_rate: 48_000, ..wave_spec() }).unwrap();
        for _ in 0..48_000 {
            writer.write_sample(1000i16).unwrap();
            writer.write_sample(2000i16).unwrap();
        }
        writer.finalize().unwrap();
        let duration = import_media(&source, &target, &AtomicBool::new(false), |_| {}).unwrap();
        assert!((duration - 1.0).abs() < 0.001);
        assert_eq!(read_range(&target, 0, 20_000).unwrap().len(), 16_000);
        assert!(import_media(&source, &target, &AtomicBool::new(false), |_| {}).is_err());
    }

    #[test]
    fn handles_non_finite_samples() {
        assert_eq!(downmix(&[1.0, -1.0, f32::NAN, 1.0], 2), vec![0.0, 0.5]);
    }
}
