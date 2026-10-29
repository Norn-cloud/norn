//! Deterministic runner, regression-file convention, and failure reports.

use std::fmt;
use std::ops::Range;

use proptest::strategy::Strategy;
use proptest::test_runner::{
    Config, FileFailurePersistence, RngSeed, TestCaseError, TestError, TestRunner,
};
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest};

use crate::coverage::{self, Coverage};
use crate::machine::DeciderMachine;
use crate::model::{DeciderModel, TurnOf};

/// Cases per run unless `PROPTEST_CASES` is set. Kept small so the default
/// `cargo test` stays fast; CI or a soak run raises it with the env var.
pub const DEFAULT_CASES: u32 = 64;

/// Seed used unless `PROPTEST_RNG_SEED` is set, so a plain `cargo test` is
/// reproducible run to run.
pub const DEFAULT_SEED: u64 = 0x6e6f_726e_2d70_736d; // "norn-psm"

/// Extension of the committed regression file written beside the test source
/// (`tests/psm_foo.rs` → `tests/psm_foo.proptest-regressions`).
pub const REGRESSION_EXTENSION: &str = "proptest-regressions";

/// How one machine run is configured.
///
/// Environment overrides follow proptest's own names: `PROPTEST_CASES`
/// (case count), `PROPTEST_RNG_SEED` (seed), and
/// `PROPTEST_DISABLE_FAILURE_PERSISTENCE`. The runner never forks or times
/// out a case: coverage counters are thread-local.
#[derive(Debug, Clone)]
pub struct Settings {
    source_file: &'static str,
    cases: u32,
    seed: u64,
    transitions: Range<usize>,
    persist: bool,
}

impl Settings {
    /// Settings for a test in `source_file` — pass `file!()`. Failures are
    /// persisted to `<source stem>.proptest-regressions` beside it; commit
    /// that file so the counterexample replays first on every later run.
    #[must_use]
    pub const fn new(source_file: &'static str) -> Self {
        Self {
            source_file,
            cases: DEFAULT_CASES,
            seed: DEFAULT_SEED,
            transitions: 1..32,
            persist: true,
        }
    }

    /// Case count when `PROPTEST_CASES` is unset.
    #[must_use]
    pub const fn cases(mut self, cases: u32) -> Self {
        self.cases = cases;
        self
    }

    /// Seed when `PROPTEST_RNG_SEED` is unset.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Range of sequence lengths (transitions per case). Default `1..32`.
    #[must_use]
    pub fn transitions(mut self, transitions: Range<usize>) -> Self {
        self.transitions = transitions;
        self
    }

    /// Neither reads nor writes a regression file. For tests that expect a
    /// failure (harness self-tests, mutants), never for ordinary law tests.
    #[must_use]
    pub const fn without_persistence(mut self) -> Self {
        self.persist = false;
        self
    }

    /// The configured range of sequence lengths.
    pub(crate) fn transition_range(&self) -> Range<usize> {
        self.transitions.clone()
    }

    /// The proptest configuration this run uses.
    #[must_use]
    pub fn config(&self) -> Config {
        let mut config = Config::default();
        if std::env::var_os("PROPTEST_CASES").is_none() {
            config.cases = self.cases;
        }
        if std::env::var_os("PROPTEST_RNG_SEED").is_none() {
            config.rng_seed = RngSeed::Fixed(self.seed);
        }
        config.source_file = Some(self.source_file);
        config.failure_persistence = if self.persist && config.failure_persistence.is_some() {
            Some(Box::new(FileFailurePersistence::WithSource(
                REGRESSION_EXTENSION,
            )))
        } else {
            None
        };
        config.fork = false;
        config.timeout = 0;
        config
    }
}

/// A failed run: the reason and the shrunk (minimal) transition sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure<T> {
    /// The assertion message of the minimal failing case, or why the run
    /// aborted.
    pub reason: String,
    /// The minimal failing transitions, in order; empty if the run aborted.
    pub transitions: Vec<T>,
}

impl<T: fmt::Debug> fmt::Display for Failure<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.reason)?;
        writeln!(
            f,
            "minimal counterexample ({} transitions):",
            self.transitions.len()
        )?;
        for (ix, turn) in self.transitions.iter().enumerate() {
            writeln!(f, "  #{ix}: {turn:?}")?;
        }
        Ok(())
    }
}

/// Runs model `M`'s machine and returns its coverage, or the minimal
/// counterexample.
///
/// # Errors
///
/// Returns the shrunk [`Failure`] if any law or model check fails.
pub fn run<M: DeciderModel>(settings: &Settings) -> Result<Coverage, Failure<TurnOf<M>>> {
    let config = settings.config();
    let mut runner = TestRunner::new(config.clone());
    let strategy = DeciderMachine::<M>::sequential_strategy(settings.transitions.clone());
    coverage::reset();
    let result = runner.run(&strategy, |(initial, transitions, seen)| {
        DeciderMachine::<M>::test_sequential(config.clone(), initial, transitions, seen);
        Ok(())
    });
    let counts = coverage::take();
    match result {
        Ok(()) => Ok(counts),
        Err(TestError::Fail(reason, (_, transitions, _))) => Err(Failure {
            reason: reason.to_string(),
            transitions,
        }),
        Err(TestError::Abort(reason)) => Err(Failure {
            reason: format!("run aborted: {reason}"),
            transitions: Vec::new(),
        }),
    }
}

/// [`run`], panicking with the minimal counterexample on failure.
///
/// # Panics
///
/// Panics if any law or model check fails.
#[must_use = "assert the coverage with `Coverage::assert_covered`"]
#[track_caller]
#[expect(
    clippy::panic,
    reason = "test-support entry point: a failed property must fail the calling test"
)]
pub fn check<M: DeciderModel>(settings: &Settings) -> Coverage {
    match run::<M>(settings) {
        Ok(coverage) => coverage,
        Err(failure) => panic!("{failure}"),
    }
}

/// Runs a model-free property (for example a [`laws`](crate::laws) check)
/// under the same seed, case-count, and regression-file conventions as
/// [`run`].
///
/// # Errors
///
/// Returns proptest's [`TestError`] with the shrunk (minimal) failing value.
pub fn run_property<S: Strategy>(
    settings: &Settings,
    strategy: &S,
    test: impl Fn(S::Value) -> Result<(), TestCaseError>,
) -> Result<(), TestError<S::Value>> {
    TestRunner::new(settings.config()).run(strategy, test)
}

/// [`run_property`], panicking with the minimal failing value on failure.
///
/// # Panics
///
/// Panics if the property fails.
#[track_caller]
#[expect(
    clippy::panic,
    reason = "test-support entry point: a failed property must fail the calling test"
)]
pub fn check_property<S: Strategy>(
    settings: &Settings,
    strategy: &S,
    test: impl Fn(S::Value) -> Result<(), TestCaseError>,
) {
    if let Err(failure) = run_property(settings, strategy, test) {
        panic!("{failure}");
    }
}
