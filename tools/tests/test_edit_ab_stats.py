import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "edit_ab_stats.py"


class EditAbStatsTests(unittest.TestCase):
    def trial(self, arm, case="same"):
        return dict(arm=arm, model="fixture", session="local", case=case,
                    rep=0, success=True, input_tokens=10, output_tokens=2,
                    requests=1, tool_calls=1, elapsed_ms=5, status="ok")

    def report(self, trials):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "trials.jsonl"
            source.write_text("".join(json.dumps(t) + "\n" for t in trials), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, "-B", str(SCRIPT), str(source), "--iters", "100"],
                check=True, text=True, capture_output=True,
            )
        return json.loads(result.stdout, parse_constant=lambda token: self.fail(
            f"non-JSON constant: {token}"))

    def assert_no_paired_evidence(self, report):
        paired = report["paired"]
        self.assertEqual(paired["pairs"], 0)
        self.assertIsNone(paired["mcnemar_exact_p"])
        for metric in ("tokens", "requests", "elapsed_ms"):
            self.assertIsNone(paired[f"{metric}_mean_diff"])
            self.assertEqual(paired[f"{metric}_mean_diff_ci95"], [None, None])
            self.assertIsNone(paired[f"{metric}_relative_change"])

    def test_single_arm_is_valid_json_without_paired_evidence(self):
        report = self.report([self.trial("fs_replace")])
        self.assertEqual(report["arms"]["fs_replace"]["n"], 1)
        self.assert_no_paired_evidence(report)

    def test_empty_input_is_valid_json_without_paired_evidence(self):
        self.assert_no_paired_evidence(self.report([]))

    def test_disjoint_cases_do_not_create_paired_evidence(self):
        self.assert_no_paired_evidence(self.report([
            self.trial("fs_patch", "left"), self.trial("fs_replace", "right"),
        ]))

    def test_complete_pairs_keep_numeric_statistics(self):
        report = self.report([self.trial("fs_patch"), self.trial("fs_replace")])
        self.assertEqual(report["paired"]["pairs"], 1)
        self.assertEqual(report["paired"]["mcnemar_exact_p"], 1.0)
        self.assertEqual(report["paired"]["tokens_mean_diff_ci95"], [0.0, 0.0])


if __name__ == "__main__":
    unittest.main()
