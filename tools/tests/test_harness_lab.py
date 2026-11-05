import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "harness_lab.py"
SPEC = importlib.util.spec_from_file_location("harness_lab", SCRIPT)
LAB = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LAB)


class HarnessLabTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        (self.root / "bin").write_bytes(b"fixture executable")
        for name, content in (("source", "broken"), ("negative", "always null")):
            path = self.root / name
            path.mkdir()
            (path / "main.py").write_text(content, encoding="utf-8")
        LAB.write(self.root / "baseline.json", {"model": "fixture", "lab_variant": "compaction_off"})
        LAB.write(self.root / "treatment.json", {"model": "fixture"})
        LAB.write(self.root / "recipe.json", {"max_cost_micro_usd": 1000, "wall_time_ms": 60000})
        self.plan = {
            "schema_version": LAB.PLAN, "axis": "compaction", "worker_image": "sha256:" + "a" * 64,
            "order_seeds": [19, 31], "max_campaign_micro_usd": 4000,
            "arms": {key: {"binary": "bin", "settings": f"{key}.json"}
                     for key in ("baseline", "treatment")},
            "cases": [{"id": "fixture", "cohort": "small", "goal": "fix", "source": "source",
                       "negative": "negative", "recipe": "recipe.json"}],
        }

    def freeze(self):
        path = self.root / "plan.json"
        LAB.write(path, self.plan)
        return LAB.freeze(path)

    def test_cap_rejects_before_environment_or_task_work(self):
        frozen = self.freeze()
        path = self.root / "frozen.json"
        LAB.write(path, frozen)
        with patch.object(LAB, "verify_environment") as env:
            with self.assertRaisesRegex(ValueError, "cap not authorized"):
                LAB.run(path, self.root / "output", 3999)
        env.assert_not_called()
        self.assertFalse((self.root / "output").exists())

    def test_changes_outside_declared_axis_rejected(self):
        LAB.axis_settings("tool_profile", ("baseline", "treatment"),
                          {"baseline": {"model": "one"}, "treatment": {"model": "one",
                                                                    "tool_profile": "terminal"}},
                          ["same", "same"])
        with self.assertRaisesRegex(ValueError, "outside its declared axis"):
            LAB.axis_settings("tool_profile", ("baseline", "treatment"),
                              {"baseline": {"model": "one"}, "treatment": {"model": "two",
                                                                        "tool_profile": "terminal"}},
                              ["same", "same"])

    def test_source_change_rejected_before_task_execution(self):
        plan = self.freeze()["plan"]
        (self.root / "source/main.py").write_text("different", encoding="utf-8")
        with patch.object(LAB, "invoke") as invoke:
            with self.assertRaisesRegex(ValueError, "frozen source changed"):
                LAB.run_trial(plan["cases"]["fixture"], plan["arms"]["baseline"],
                              self.root / "trial", self.plan["worker_image"])
        invoke.assert_not_called()

    def test_full_campaign_requires_each_cohort(self):
        self.plan["campaign_kind"] = "full"
        with self.assertRaisesRegex(ValueError, "minimum coding cohorts"):
            self.freeze()

    def test_schedule_stops_before_next_task_exceeds_cumulative_cap(self):
        self.plan["max_campaign_micro_usd"] = 1500
        plan = self.freeze()["plan"]
        output = self.root / "campaign"
        output.mkdir()
        sample = {"known_total": 800, "missing_samples": 0}

        def trial(_case, _arm, directory, _image):
            metrics = {key: sample for key in
                       ("input_tokens", "output_tokens", "accounted_cost_micro_usd")}
            metrics["conservative_charged_micro_usd"] = 800
            return {"outcome": "passed", "stage": "run", "wall_ms": 20, "metrics": metrics}

        with patch.object(LAB.shutil, "disk_usage") as disk:
            disk.return_value.free = LAB.MIN_FREE_BYTES
            with patch.object(LAB, "run_trial", side_effect=trial) as invoke:
                self.assertEqual(LAB.run_schedule(plan, output, resume=False), 2)
        invoke.assert_called_once()
        summary = LAB.read(output / "summary.json")
        self.assertEqual(summary["stop_reason"], "campaign_budget")
        self.assertEqual(summary["settled_campaign_charge_micro_usd"], 800)

    def test_low_disk_stops_before_a_paid_trial(self):
        plan = self.freeze()["plan"]
        output = self.root / "disk-bound"
        output.mkdir()
        with patch.object(LAB.shutil, "disk_usage") as disk, patch.object(LAB, "run_trial") as invoke:
            disk.return_value.free = LAB.MIN_FREE_BYTES - 1
            self.assertEqual(LAB.run_schedule(plan, output, resume=False), 2)
        invoke.assert_not_called()
        self.assertEqual(LAB.read(output / "summary.json")["stop_reason"], "host_disk_space")

    def test_resume_refuses_inflight_trial_and_does_not_dispatch(self):
        plan = self.freeze()["plan"]
        output = self.root / "interrupted"
        output.mkdir()
        directory, *_ = LAB.schedule(plan, output)[0]
        directory.mkdir()
        with patch.object(LAB, "run_trial") as invoke:
            with self.assertRaisesRegex(ValueError, "missing evidence"):
                LAB.run_schedule(plan, output, resume=True)
        invoke.assert_not_called()

    def test_wave_runs_independent_trials_and_reserves_before_launch(self):
        self.plan["max_campaign_micro_usd"] = 2000
        plan = self.freeze()["plan"]
        output = self.root / "parallel"
        output.mkdir()
        rendezvous = threading.Barrier(2)
        sample = {"known_total": 800, "missing_samples": 0}

        def trial(_case, _arm, directory, _image):
            rendezvous.wait(timeout=5)
            self.assertTrue(directory.is_dir())
            metrics = {key: sample for key in
                       ("input_tokens", "output_tokens", "accounted_cost_micro_usd")}
            metrics["conservative_charged_micro_usd"] = 800
            return {"outcome": "passed", "stage": "run", "wall_ms": 20, "metrics": metrics}

        with patch.object(LAB.shutil, "disk_usage") as disk:
            disk.return_value.free = LAB.MIN_FREE_BYTES * 4
            with patch.object(LAB, "run_trial", side_effect=trial) as invoke:
                self.assertEqual(LAB.run_schedule(plan, output, resume=False, jobs=4), 2)
        self.assertEqual(invoke.call_count, 2)
        summary = LAB.read(output / "summary.json")
        self.assertEqual(summary["parallelism_levels"], [2])
        self.assertEqual(summary["stop_reason"], "campaign_budget")

    def test_prior_unsettled_trial_requires_independent_stop_proof(self):
        plan = self.freeze()["plan"]
        output = self.root / "settled-owner"
        output.mkdir()
        directory, case, seed, arm = LAB.schedule(plan, output)[0]
        directory.mkdir()
        row = {"case_id": case, "order_seed": seed, "arm": arm, "outcome": "inconclusive",
               "stage": "run", "metrics": {"conservative_charged_micro_usd": 987}}
        LAB.write(directory / "trial.json", row)
        with patch.object(LAB, "independent_trial_stopped", return_value=False):
            with self.assertRaisesRegex(ValueError, "safe continuation"):
                LAB.resume_rows(plan, output, allow_unsettled=True)
        with patch.object(LAB, "independent_trial_stopped", return_value=True):
            self.assertEqual(LAB.resume_rows(plan, output, allow_unsettled=True), [row])

    def test_explicit_budget_extension_retains_the_frozen_plan_and_prior_report(self):
        frozen = self.freeze()
        plan = self.root / "frozen.json"
        output = self.root / "extension"
        LAB.write(plan, frozen)
        with patch.object(LAB, "verify_environment", return_value={"image": "fixture"}):
            with patch.object(LAB, "run_schedule", return_value=2) as schedule:
                self.assertEqual(LAB.run(plan, output, 4000), 2)
                report = {"stop_reason": "campaign_budget", "recorded_trials": 0}
                LAB.write(output / "summary.json", report)
                with patch.object(LAB.shutil, "disk_usage") as disk:
                    disk.return_value.free = LAB.MIN_FREE_BYTES
                    self.assertEqual(LAB.run(plan, output, 6000, resume=True, raise_cap=True), 2)
                self.assertEqual(schedule.call_args.args[-1], 6000)
        self.assertEqual(LAB.read(plan), frozen)
        self.assertEqual(LAB.read(output / "prior-summary-0001.json"), report)
        header = LAB.read(output / "campaign.json")
        self.assertEqual(LAB.campaign_cap(output, header), (6000, 1))
        (output / "prior-summary-0001.json").write_text("{}", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "budget evidence differs"):
            LAB.campaign_cap(output, header)

    def test_trial_directories_do_not_embed_long_case_identifiers(self):
        self.plan["cases"][0]["id"] = "a" * 48
        plan = self.freeze()["plan"]
        directory, case, _, _ = LAB.schedule(plan, self.root)[0]
        self.assertEqual(case, "a" * 48)
        self.assertLessEqual(len(directory.name), 8)

    def test_synthetic_suite_materializes_independent_protected_expectations(self):
        fixture_script = SCRIPT.with_name("harness_lab_fixtures.py")
        output = self.root / "synthetic"
        subprocess.run([sys.executable, "-B", str(fixture_script), str(output),
                        "--seed", "100", "--max-case-micro-usd", "1000"], check=True)
        inventory = LAB.read(output / "cases.private.json")["cases"]
        self.assertEqual(len({case["id"] for case in inventory}), 150)
        for cohort, count in LAB.MIN_FULL_COHORTS.items():
            self.assertEqual(sum(case["cohort"] == cohort for case in inventory), count)
        small = inventory[0]
        recipe = LAB.read(output / small["recipe"])
        source = output / small["source"] / "main.py"
        self.assertNotEqual(subprocess.check_output(
            [sys.executable, "-B", str(source), str(recipe["checks"][0]["input"])], text=True).strip(),
            str(recipe["checks"][0]["expected"]))

    def test_failure_cost_counts_and_unknown_does_not_become_zero(self):
        def row(seed, arm, outcome, cost):
            sample = {"known_total": cost, "missing_samples": int(cost is None)}
            return {"case_id": "one", "order_seed": seed, "arm": arm, "outcome": outcome,
                    "wall_ms": 17, "metrics": {key: sample for key in
                    ("input_tokens", "output_tokens", "accounted_cost_micro_usd")}}

        trials = [row(19, "baseline", "passed", 10), row(31, "baseline", "failed", 20),
                  row(19, "treatment", "passed", 11), row(31, "treatment", "inconclusive", None)]
        summary = LAB.summarize(trials, 2)
        self.assertEqual(summary["status"], "incomplete")
        self.assertEqual(summary["arms"]["baseline"]["verified_success_rate"], 0.5)
        self.assertEqual(summary["arms"]["baseline"]["wall_ms_per_verified_success"], 34)
        self.assertEqual(summary["arms"]["baseline"]["known_usage_per_verified_success"]
                         ["accounted_cost_micro_usd"], 30)
        self.assertIsNone(summary["arms"]["treatment"]["known_usage_per_verified_success"]
                          ["accounted_cost_micro_usd"])
        self.assertEqual(len(summary["paired_outcomes"]), 2)


if __name__ == "__main__":
    unittest.main()
