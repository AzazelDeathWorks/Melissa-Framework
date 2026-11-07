//! Pure verification outcome rules, shared by project verification and replay.
//! A successful retry never erases a failure, missing run or unknown effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail,
    Flaky,
    NotRun,
    Blocked,
    Inconclusive,
}
impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Flaky => "FLAKY",
            Self::NotRun => "NOT_RUN",
            Self::Blocked => "BLOCKED",
            Self::Inconclusive => "INCONCLUSIVE",
        }
    }
    pub fn from_wire(value: &str) -> Self {
        match value {
            "PASS" => Self::Pass,
            "FAIL" => Self::Fail,
            "FLAKY" => Self::Flaky,
            "NOT_RUN" => Self::NotRun,
            "BLOCKED" => Self::Blocked,
            _ => Self::Inconclusive,
        }
    }
}
pub fn aggregate(attempts: &[Outcome], expected: usize) -> Outcome {
    if attempts.is_empty() {
        return Outcome::NotRun;
    }
    if attempts.contains(&Outcome::Inconclusive) {
        return Outcome::Inconclusive;
    }
    if attempts.contains(&Outcome::Blocked) {
        return Outcome::Blocked;
    }
    if attempts.len() != expected || attempts.contains(&Outcome::NotRun) {
        return Outcome::NotRun;
    }
    let pass = attempts.contains(&Outcome::Pass);
    let fail = attempts.contains(&Outcome::Fail);
    if attempts.contains(&Outcome::Flaky) || (pass && fail) {
        Outcome::Flaky
    } else if fail {
        Outcome::Fail
    } else {
        Outcome::Pass
    }
}
/// Different checks may legitimately disagree. That is a failing gate, not a
/// flaky retry of the same check. Keep incomplete/unknown work distinct.
pub fn combine(checks: &[Outcome], expected: usize) -> Outcome {
    if checks.is_empty() {
        return Outcome::NotRun;
    }
    if checks.contains(&Outcome::Inconclusive) {
        return Outcome::Inconclusive;
    }
    if checks.contains(&Outcome::Blocked) {
        return Outcome::Blocked;
    }
    if checks.len() != expected || checks.contains(&Outcome::NotRun) {
        return Outcome::NotRun;
    }
    if checks.contains(&Outcome::Flaky) {
        Outcome::Flaky
    } else if checks.contains(&Outcome::Fail) {
        Outcome::Fail
    } else {
        Outcome::Pass
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn expected_failure_regression_and_unknown_are_distinct() {
        assert_eq!(combine(&[Outcome::Pass, Outcome::Fail], 2), Outcome::Fail);
        assert_eq!(combine(&[Outcome::Pass], 2), Outcome::NotRun);
        assert_eq!(
            combine(&[Outcome::Pass, Outcome::NotRun], 2),
            Outcome::NotRun
        );
        for (runs, expected, outcome) in [
            (vec![], 1, Outcome::NotRun),
            (vec![Outcome::Fail], 1, Outcome::Fail),
            (vec![Outcome::Fail, Outcome::Pass], 2, Outcome::Flaky),
            (vec![Outcome::Pass], 2, Outcome::NotRun),
            (vec![Outcome::Pass, Outcome::Blocked], 2, Outcome::Blocked),
            (
                vec![Outcome::Pass, Outcome::Inconclusive],
                2,
                Outcome::Inconclusive,
            ),
            (vec![Outcome::Pass, Outcome::NotRun], 2, Outcome::NotRun),
            (vec![Outcome::Flaky], 1, Outcome::Flaky),
            (vec![Outcome::Pass], 1, Outcome::Pass),
        ] {
            assert_eq!(aggregate(&runs, expected), outcome);
        }
    }
    proptest! {
        #[test] fn pass_requires_a_complete_nonempty_all_passing_run(raw in prop::collection::vec(0u8..6,0..40), expected in 0usize..40) {
            let choices=[Outcome::Pass,Outcome::Fail,Outcome::Flaky,Outcome::NotRun,Outcome::Blocked,Outcome::Inconclusive];
            let runs:Vec<_>=raw.into_iter().map(|i|choices[i as usize]).collect();
            for result in [aggregate(&runs,expected),combine(&runs,expected)] {if result==Outcome::Pass {prop_assert!(!runs.is_empty());prop_assert_eq!(runs.len(),expected);prop_assert!(runs.iter().all(|r|*r==Outcome::Pass));}}
        }
        #[test] fn ordering_never_launders_flaky_or_unknown_evidence(raw in prop::collection::vec(0u8..6,0..40)) {
            let choices=[Outcome::Pass,Outcome::Fail,Outcome::Flaky,Outcome::NotRun,Outcome::Blocked,Outcome::Inconclusive];
            let mut runs:Vec<_>=raw.into_iter().map(|i|choices[i as usize]).collect();let before=aggregate(&runs,runs.len());runs.reverse();prop_assert_eq!(aggregate(&runs,runs.len()),before);
        }
    }
}
