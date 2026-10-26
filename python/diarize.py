from __future__ import annotations

import array
import contextlib
import inspect
import json
import math
import os
import sys
import wave

PROTOCOL_OUTPUT = sys.stdout
MINIMUM_PYTHON = (3, 10)


def respond(payload: dict) -> None:
    json.dump(payload, PROTOCOL_OUTPUT, allow_nan=False)
    PROTOCOL_OUTPUT.write("\n")
    PROTOCOL_OUTPUT.flush()


def failure(error: str, hint: str = "") -> SystemExit:
    respond({"ok": False, "error": error, "hint": hint})
    return SystemExit(1)


def dependencies():
    if sys.version_info < MINIMUM_PYTHON:
        raise failure("Diarization requires Python 3.10 or newer.")
    try:
        import pyannote.audio as pyannote_audio
        import torch
    except (ImportError, OSError) as error:
        raise failure(
            f"Diarization dependencies are unavailable: {error}",
            "Install pyannote.audio in this interpreter's environment.",
        )
    return torch, pyannote_audio


def probe() -> None:
    torch, pyannote_audio = dependencies()
    cuda = bool(torch.cuda.is_available())
    if cuda:
        device = f"CUDA · {torch.cuda.get_device_name(0)}"
    elif torch.version.cuda is None:
        device = "CPU · PyTorch has no CUDA support"
    else:
        device = f"CPU · CUDA {torch.version.cuda} build, but CUDA is unavailable"
    respond(
        {
            "ok": True,
            "pyannote": getattr(pyannote_audio, "__version__", "unknown"),
            "torch": torch.__version__,
            "python": sys.version.split()[0],
            "cuda": cuda,
            "device": device,
        }
    )


def turns_from(output) -> list[dict]:
    annotation = getattr(output, "exclusive_speaker_diarization", None)
    if annotation is None:
        annotation = getattr(output, "speaker_diarization", output)
    collected = []
    for turn, _, speaker in annotation.itertracks(yield_label=True):
        start, end = float(turn.start), float(turn.end)
        if not math.isfinite(start) or not math.isfinite(end) or start < 0:
            raise ValueError("The pipeline returned an invalid speaker timestamp")
        if end > start:
            collected.append({"start": start, "end": end, "speaker": str(speaker)})
    collected.sort(key=lambda item: item["start"])
    return collected


def load_waveform(path: str, torch):
    with wave.open(path, "rb") as audio:
        if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate()) != (
            1,
            2,
            16000,
        ):
            raise ValueError("Expected 16 kHz mono PCM16 audio")
        samples = array.array("h", audio.readframes(audio.getnframes()))
    if sys.byteorder != "little":
        samples.byteswap()
    waveform = torch.tensor(samples, dtype=torch.float32).unsqueeze(0) / 32768.0
    return {"waveform": waveform, "sample_rate": 16000}


def load_pipeline(request: dict):
    model = request.get("model") or "pyannote/speaker-diarization-community-1"
    token = request.get("token") or None
    if not isinstance(model, str) or (token is not None and not isinstance(token, str)):
        raise failure("Model and token must be strings.")

    def describe(error):
        return str(error).replace(token, "[redacted]") if token else str(error)

    try:
        torch, pyannote_audio = dependencies()
        factory = pyannote_audio.Pipeline.from_pretrained
        argument = (
            "token"
            if "token" in inspect.signature(factory).parameters
            else "use_auth_token"
        )
        options = {argument: token}
        cache_dir = os.environ.get("HF_HUB_CACHE")
        if cache_dir:
            options["cache_dir"] = cache_dir
        pipeline = factory(model, **options)
        if pipeline is None:
            raise RuntimeError("Access to the gated model was denied")
    except Exception as error:
        raise failure(
            f"Cannot load the diarization model: {describe(error)}",
            "Accept its conditions on Hugging Face and supply a valid access token in Settings.",
        ) from error
    return torch, pipeline, describe


def check_model(request: dict) -> None:
    load_pipeline(request)
    respond({"ok": True})


def diarize(request: dict) -> None:
    audio = request.get("audio")
    if not isinstance(audio, str) or not os.path.isfile(audio):
        raise failure("The recording audio file does not exist.")
    torch, pipeline, describe = load_pipeline(request)

    options = {}
    for key in ("min_speakers", "max_speakers"):
        value = request.get(key)
        if value is not None:
            if (
                isinstance(value, bool)
                or not isinstance(value, int)
                or not 1 <= value <= 100
            ):
                raise failure("Speaker counts must be integers between 1 and 100.")
            options[key] = value
    if options.get("min_speakers", 1) > options.get("max_speakers", 100):
        raise failure("Minimum speakers cannot exceed maximum speakers.")

    device = "cuda" if torch.cuda.is_available() else "cpu"
    try:
        pipeline.to(torch.device(device))
    except Exception as error:
        raise failure(
            f"Cannot initialize diarization on {device.upper()}: {describe(error)}",
            "Check the NVIDIA driver and available GPU memory, then check the Python setup in Settings."
            if device == "cuda"
            else "Check the Python setup in Settings.",
        ) from error

    try:
        print(f"Diarization device: {device.upper()}", file=sys.stderr)
        output = pipeline(load_waveform(audio, torch), **options)
        respond({"ok": True, "device": device, "turns": turns_from(output)})
    except Exception as error:
        raise failure(f"Diarization failed: {describe(error)}") from error


def main() -> None:
    with contextlib.redirect_stdout(sys.stderr):
        if "--probe" in sys.argv[1:]:
            probe()
            return
        raw = sys.stdin.read(1_048_577)
        if len(raw) > 1_048_576:
            raise failure("Diarization request is too large.")
        try:
            request = json.loads(raw)
        except json.JSONDecodeError:
            raise failure("Malformed diarization request.")
        if not isinstance(request, dict):
            raise failure("Diarization request must be a JSON object.")
        if "--check-model" in sys.argv[1:]:
            check_model(request)
        else:
            diarize(request)


if __name__ == "__main__":
    main()
