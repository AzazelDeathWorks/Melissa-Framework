import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / "bench/harbor/compare_matched.py"
SPEC = importlib.util.spec_from_file_location("compare_matched", SCRIPT)
COMPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COMPARE)


class MatchedTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.controls = {
            "dataset": {"name": "fixture-not-a-benchmark", "version": "1", "source_url": "https://example.invalid/tasks", "git_commit": "c" * 40},
            "tasks": ["one", "two", "three"], "repeats": 1,
            "model": {"id": "same-model", "endpoint_sha256": "e" * 64, "protocol": "open_ai_chat_completions"},
            "context_tokens": 8192, "max_output_tokens": 1024, "max_turns": 100, "concurrency": 1, "seed": 42,
            **{key: "d" * 64 for key in ("deadline_policy_sha256", "retry_policy_sha256", "authority_sha256", "environment_sha256")},
        }

    def manifest(self, side, rewards, repeats=1):
        controls = copy.deepcopy(self.controls)
        controls["repeats"] = repeats
        manifest = {"schema_version": "melissa.matched-harbor/v1", "controls": controls,
                    "agent": {"id": side, "harbor_agent": side, "revision_sha256": "a" * 64}, "trials": []}
        for repeat in range(repeats):
            for task, reward in zip(controls["tasks"], rewards):
                trial = {"task_name": task, "config": {"task": {"git_url": controls["dataset"]["source_url"], "git_commit_id": controls["dataset"]["git_commit"]}, "agent": {"name": side, "model_name": "same-model"}}, "verifier_result": {"rewards": {"reward": reward}}, "exception_info": None}
                file = self.root / f"{side}-{task}-{repeat}.json"
                file.write_text(json.dumps(trial))
                manifest["trials"].append({"task": task, "repeat": repeat, "result": file.name, "result_sha256": COMPARE.sha(file.read_bytes())})
        path = self.root / f"{side}.json"
        path.write_text(json.dumps(manifest))
        return path

    def change(self, path, apply):
        value = json.loads(path.read_text())
        apply(value)
        path.write_text(json.dumps(value))

    def test_complete_pairs_have_deterministic_statistics_and_no_selected_trials(self):
        a, b = self.manifest("a", [1, 1, 0]), self.manifest("b", [0, 1, 1])
        result = COMPARE.compare(a, b, 200)
        self.assertEqual(result, COMPARE.compare(a, b, 200))
        self.assertTrue(result["all_trials_verified"])
        self.assertEqual(result["scheduled_pairs"], 3)
        self.assertEqual(result["statistics"]["exact_mcnemar_p"], 1)
        self.assertEqual(result["statistics"]["pass_rate_delta_left_minus_right"], 0)
        self.assertEqual(COMPARE.mcnemar(10, 0), 2 / 1024)

    def test_missing_trials_are_visible_and_suppress_significance(self):
        a, b = self.manifest("a", [1]), self.manifest("b", [0, 1, 1])
        result = COMPARE.compare(a, b, 100)
        self.assertFalse(result["all_trials_verified"])
        self.assertIsNone(result["statistics"])
        self.assertEqual(result["counts"]["left"]["missing_trial"], 2)
        self.assertEqual(result["scheduled_pairs"], 3)

    def test_mismatched_controls_and_altered_results_are_rejected(self):
        a, b = self.manifest("a", [1]), self.manifest("b", [1])
        self.change(b, lambda x: x["controls"].update(concurrency=2))
        with self.assertRaises(ValueError): COMPARE.compare(a, b, 100)
        b = self.manifest("b", [1])
        (self.root / "b-one-0.json").write_text("{}")
        with self.assertRaises(ValueError): COMPARE.compare(a, b, 100)

    def test_duplicate_trials_paths_and_json_keys_are_not_overwritten(self):
        a, b = self.manifest("a", [1]), self.manifest("b", [1])
        self.change(a, lambda x: x["trials"].append(x["trials"][0]))
        with self.assertRaises(ValueError): COMPARE.compare(a, b, 100)
        a = self.manifest("a", [1])
        self.change(a, lambda x: x["trials"][0].update(result="../outside.json"))
        with self.assertRaises(ValueError): COMPARE.compare(a, b, 100)
        a.write_text('{"schema_version":1,"schema_version":2}')
        with self.assertRaises(ValueError): COMPARE.compare(a, b, 100)

    def test_errors_never_count_as_success_and_repeats_are_clustered(self):
        a, b = self.manifest("a", [1, 0, 1], 2), self.manifest("b", [0, 0, 1], 2)
        self.assertIsNone(COMPARE.compare(a, b, 100)["statistics"]["exact_mcnemar_p"])
        trial = self.root / "a-one-0.json"
        self.change(trial, lambda x: x.update(exception_info={"exception_type": "ProviderRateLimit"}))
        self.change(a, lambda x: x["trials"][0].update(result_sha256=COMPARE.sha(trial.read_bytes())))
        result = COMPARE.compare(a, b, 100)
        self.assertIsNone(result["statistics"])
        self.assertEqual(result["counts"]["left"]["error"], 1)

    def test_actual_model_identity_is_checked_not_just_declared_controls(self):
        a, b = self.manifest("a", [1]), self.manifest("b", [1])
        trial = self.root / "a-one-0.json"
        self.change(trial, lambda x: x["config"]["agent"].update(model_name="different"))
        self.change(a, lambda x: x["trials"][0].update(result_sha256=COMPARE.sha(trial.read_bytes())))
        with self.assertRaises(ValueError): COMPARE.compare(a, b, 100)
