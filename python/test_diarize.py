import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import types
import unittest
from unittest.mock import Mock, patch

SCRIPT = pathlib.Path(__file__).with_name("diarize.py")
SPEC = importlib.util.spec_from_file_location("parakeetx_diarize", SCRIPT)
WORKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKER)


class Annotation:
    def itertracks(self, yield_label):
        return iter(
            [
                (types.SimpleNamespace(start=2.0, end=4.0), None, "SPEAKER_01"),
                (types.SimpleNamespace(start=0.0, end=1.5), None, "SPEAKER_00"),
            ]
        )


class WorkerTests(unittest.TestCase):
    def diarization(self, pipeline, cuda=True):
        protocol = io.StringIO()
        diagnostics = io.StringIO()
        torch = types.SimpleNamespace(
            cuda=types.SimpleNamespace(is_available=lambda: cuda),
            device=lambda name: name,
        )
        payload = {"audio": "recording.wav", "token": "secret", "min_speakers": 2}
        waveform = {"waveform": object(), "sample_rate": 16000}
        with (
            patch.object(WORKER, "PROTOCOL_OUTPUT", protocol),
            patch.object(sys, "stderr", diagnostics),
            patch.object(sys, "stdin", io.StringIO(json.dumps(payload))),
            patch.object(sys, "argv", [str(SCRIPT)]),
            patch.object(WORKER.os.path, "isfile", return_value=True),
            patch.object(
                WORKER,
                "load_pipeline",
                return_value=(
                    torch,
                    pipeline,
                    lambda error: str(error).replace("secret", "[redacted]"),
                ),
            ),
            patch.object(WORKER, "load_waveform", return_value=waveform),
        ):
            try:
                WORKER.main()
            except SystemExit:
                pass
        return json.loads(protocol.getvalue()), diagnostics.getvalue(), waveform

    def test_diarization_moves_pipeline_to_cuda_before_inference(self):
        pipeline = Mock(return_value=Annotation())
        response, diagnostics, waveform = self.diarization(pipeline)
        self.assertTrue(response["ok"])
        self.assertEqual(response["device"], "cuda")
        pipeline.to.assert_called_once_with("cuda")
        pipeline.assert_called_once_with(waveform, min_speakers=2)
        self.assertEqual([call[0] for call in pipeline.mock_calls], ["to", ""])
        self.assertEqual(response["turns"], WORKER.turns_from(Annotation()))
        self.assertIn("Diarization device: CUDA", diagnostics)

    def test_diarization_uses_cpu_when_cuda_is_unavailable(self):
        pipeline = Mock(return_value=Annotation())
        response, diagnostics, waveform = self.diarization(pipeline, cuda=False)
        self.assertTrue(response["ok"])
        self.assertEqual(response["device"], "cpu")
        pipeline.to.assert_called_once_with("cpu")
        pipeline.assert_called_once_with(waveform, min_speakers=2)
        self.assertIn("Diarization device: CPU", diagnostics)

    def test_cuda_initialization_failure_is_actionable_and_never_silently_retries_on_cpu(
        self,
    ):
        pipeline = Mock()
        pipeline.to.side_effect = RuntimeError("CUDA allocation failed: secret")
        response, _, _ = self.diarization(pipeline)
        self.assertFalse(response["ok"])
        self.assertIn("Cannot initialize diarization on CUDA", response["error"])
        self.assertIn("NVIDIA driver", response["hint"])
        self.assertNotIn("secret", json.dumps(response))
        pipeline.to.assert_called_once_with("cuda")
        pipeline.assert_not_called()

    def test_cuda_inference_failure_preserves_error_and_redacts_token(self):
        pipeline = Mock(side_effect=RuntimeError("CUDA out of memory: secret"))
        response, _, _ = self.diarization(pipeline)
        self.assertFalse(response["ok"])
        self.assertIn("CUDA out of memory", response["error"])
        self.assertNotIn("secret", json.dumps(response))
        pipeline.to.assert_called_once_with("cuda")
        self.assertEqual(pipeline.call_count, 1)

    def test_probe_distinguishes_cuda_from_cpu_only_and_unavailable_cuda_builds(self):
        for available, runtime, expected in [
            (True, "12.8", "CUDA · Test GPU"),
            (False, None, "CPU · PyTorch has no CUDA support"),
            (False, "12.8", "CPU · CUDA 12.8 build, but CUDA is unavailable"),
        ]:
            with self.subTest(available=available, runtime=runtime):
                torch = types.SimpleNamespace(
                    __version__="test-torch",
                    version=types.SimpleNamespace(cuda=runtime),
                    cuda=types.SimpleNamespace(
                        is_available=Mock(return_value=available),
                        get_device_name=lambda index: "Test GPU",
                    ),
                )
                protocol = io.StringIO()
                with (
                    patch.object(WORKER, "PROTOCOL_OUTPUT", protocol),
                    patch.object(
                        WORKER,
                        "dependencies",
                        return_value=(
                            torch,
                            types.SimpleNamespace(__version__="test-pyannote"),
                        ),
                    ),
                ):
                    WORKER.probe()
                response = json.loads(protocol.getvalue())
                self.assertTrue(response["ok"])
                self.assertEqual(response["cuda"], available)
                self.assertEqual(response["device"], expected)
                self.assertEqual(response["torch"], "test-torch")

    def model_check(self, payload, factory, cache_dir=""):
        protocol = io.StringIO()
        diagnostics = io.StringIO()
        fake_audio = types.SimpleNamespace(
            Pipeline=types.SimpleNamespace(from_pretrained=factory)
        )
        with (
            patch.dict(WORKER.os.environ, {"HF_HUB_CACHE": cache_dir}),
            patch.object(WORKER, "PROTOCOL_OUTPUT", protocol),
            patch.object(sys, "stderr", diagnostics),
            patch.object(sys, "stdin", io.StringIO(json.dumps(payload))),
            patch.object(sys, "argv", [str(SCRIPT), "--check-model"]),
            patch.object(WORKER, "dependencies", return_value=(object(), fake_audio)),
        ):
            try:
                WORKER.main()
            except SystemExit:
                pass
        return json.loads(protocol.getvalue()), diagnostics.getvalue()

    def test_model_check_downloads_without_audio_inference_and_keeps_stdout_clean(self):
        calls = []

        def factory(model, token=None):
            print("download progress")
            calls.append((model, token))
            return object()

        response, diagnostics = self.model_check(
            {"model": "org/model", "token": "secret"}, factory
        )
        self.assertTrue(response["ok"])
        self.assertEqual(calls, [("org/model", "secret")])
        self.assertIn("download progress", diagnostics)

    def test_model_check_supports_legacy_auth_and_redacts_access_errors(self):
        calls = []

        def legacy(model, use_auth_token=None):
            calls.append(use_auth_token)
            return object()

        self.assertTrue(self.model_check({"token": "secret"}, legacy)[0]["ok"])
        self.assertEqual(calls, ["secret"])

        def denied(model, token=None):
            raise RuntimeError(f"denied with {token}")

        response, _ = self.model_check({"token": "secret"}, denied)
        self.assertFalse(response["ok"])
        self.assertNotIn("secret", json.dumps(response))
        self.assertIn("[redacted]", response["error"])
        self.assertFalse(self.model_check({}, lambda model: None)[0]["ok"])

    def test_model_cache_is_passed_explicitly_for_modern_and_legacy_pyannote(self):
        calls = []

        def modern(model, token=None, cache_dir=None):
            calls.append((token, cache_dir))
            return object()

        def legacy(model, use_auth_token=None, cache_dir=None):
            calls.append((use_auth_token, cache_dir))
            return object()

        for factory in (modern, legacy):
            response, _ = self.model_check(
                {"token": "secret"}, factory, "/configured/models/huggingface/hub"
            )
            self.assertTrue(response["ok"])
        self.assertEqual(calls, [("secret", "/configured/models/huggingface/hub")] * 2)

    def test_model_check_rejects_malformed_requests_without_importing_dependencies(
        self,
    ):
        for request in ["invalid", "[]", '{"model":123}', '{"token":{"invalid":true}}']:
            result = subprocess.run(
                [sys.executable, str(SCRIPT), "--check-model"],
                input=request,
                text=True,
                capture_output=True,
                check=False,
                timeout=10,
            )
            self.assertEqual(result.returncode, 1)
            self.assertFalse(json.loads(result.stdout)["ok"])

    def test_rejects_invalid_requests_with_machine_readable_response(self):
        for request in ["invalid", "[]", "{}", '{"audio":123}']:
            result = subprocess.run(
                [sys.executable, str(SCRIPT)],
                input=request,
                text=True,
                capture_output=True,
                check=False,
                timeout=10,
            )
            self.assertEqual(result.returncode, 1)
            self.assertFalse(json.loads(result.stdout)["ok"])

    def test_supports_modern_and_legacy_result_objects(self):
        legacy = WORKER.turns_from(Annotation())
        modern = WORKER.turns_from(
            types.SimpleNamespace(speaker_diarization=Annotation())
        )
        self.assertEqual(legacy, modern)
        self.assertEqual(modern[0]["speaker"], "SPEAKER_00")
        self.assertEqual(modern[1]["end"], 4.0)

    def test_rejects_non_finite_timestamps(self):
        annotation = types.SimpleNamespace(
            itertracks=lambda **kwargs: iter(
                [(types.SimpleNamespace(start=float("nan"), end=2.0), None, "speaker")]
            )
        )
        with self.assertRaises(ValueError):
            WORKER.turns_from(annotation)

    def test_prefers_exclusive_speaker_turns_for_transcript_assignment(self):
        output = types.SimpleNamespace(
            exclusive_speaker_diarization=Annotation(), speaker_diarization=None
        )
        self.assertEqual(WORKER.turns_from(output), WORKER.turns_from(Annotation()))

    def test_falls_back_when_exclusive_output_is_unavailable(self):
        output = types.SimpleNamespace(
            exclusive_speaker_diarization=None, speaker_diarization=Annotation()
        )
        self.assertEqual(WORKER.turns_from(output), WORKER.turns_from(Annotation()))

    def test_protocol_stdout_is_not_polluted_by_library_prints(self):
        protocol = io.StringIO()
        diagnostics = io.StringIO()
        with (
            patch.object(WORKER, "PROTOCOL_OUTPUT", protocol),
            patch.object(sys, "stderr", diagnostics),
            patch.object(sys, "stdin", io.StringIO("{}")),
            patch.object(sys, "argv", [str(SCRIPT)]),
        ):

            def respond(request):
                print("library diagnostic")
                WORKER.respond({"ok": True, "turns": []})

            with patch.object(WORKER, "diarize", respond):
                WORKER.main()
        self.assertTrue(json.loads(protocol.getvalue())["ok"])
        self.assertIn("library diagnostic", diagnostics.getvalue())


if __name__ == "__main__":
    unittest.main()
