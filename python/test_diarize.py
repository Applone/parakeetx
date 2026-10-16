import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import types
import unittest
from unittest.mock import patch


SCRIPT = pathlib.Path(__file__).with_name("diarize.py")
SPEC = importlib.util.spec_from_file_location("parakeetx_diarize", SCRIPT)
WORKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(WORKER)


class Annotation:
    def itertracks(self, yield_label):
        return iter([
            (types.SimpleNamespace(start=2.0, end=4.0), None, "SPEAKER_01"),
            (types.SimpleNamespace(start=0.0, end=1.5), None, "SPEAKER_00"),
        ])


class WorkerTests(unittest.TestCase):
    def model_check(self, payload, factory):
        protocol = io.StringIO()
        diagnostics = io.StringIO()
        fake_audio = types.SimpleNamespace(Pipeline=types.SimpleNamespace(from_pretrained=factory))
        with patch.object(WORKER, "PROTOCOL_OUTPUT", protocol), patch.object(sys, "stderr", diagnostics), patch.object(sys, "stdin", io.StringIO(json.dumps(payload))), patch.object(sys, "argv", [str(SCRIPT), "--check-model"]), patch.object(WORKER, "dependencies", return_value=(object(), fake_audio)):
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
        response, diagnostics = self.model_check({"model": "org/model", "token": "secret"}, factory)
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

    def test_model_check_rejects_malformed_requests_without_importing_dependencies(self):
        for request in ["invalid", "[]", '{"model":123}', '{"token":{"invalid":true}}']:
            result = subprocess.run([sys.executable, str(SCRIPT), "--check-model"], input=request, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(json.loads(result.stdout)["ok"])

    def test_rejects_invalid_requests_with_machine_readable_response(self):
        for request in ["invalid", "[]", '{}', '{"audio":123}']:
            result = subprocess.run([sys.executable, str(SCRIPT)], input=request, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(json.loads(result.stdout)["ok"])

    def test_supports_modern_and_legacy_result_objects(self):
        legacy = WORKER.turns_from(Annotation())
        modern = WORKER.turns_from(types.SimpleNamespace(speaker_diarization=Annotation()))
        self.assertEqual(legacy, modern)
        self.assertEqual(modern[0]["speaker"], "SPEAKER_00")
        self.assertEqual(modern[1]["end"], 4.0)

    def test_rejects_non_finite_timestamps(self):
        annotation = types.SimpleNamespace(itertracks=lambda **kwargs: iter([(types.SimpleNamespace(start=float("nan"), end=2.0), None, "speaker")]))
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
        with patch.object(WORKER, "PROTOCOL_OUTPUT", protocol), patch.object(sys, "stderr", diagnostics), patch.object(sys, "stdin", io.StringIO('{}')), patch.object(sys, "argv", [str(SCRIPT)]):
            def respond(request):
                print("library diagnostic")
                WORKER.respond({"ok": True, "turns": []})
            with patch.object(WORKER, "diarize", respond):
                WORKER.main()
        self.assertTrue(json.loads(protocol.getvalue())["ok"])
        self.assertIn("library diagnostic", diagnostics.getvalue())


if __name__ == "__main__":
    unittest.main()
