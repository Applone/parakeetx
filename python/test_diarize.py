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
