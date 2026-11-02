//! SMT-RESULT-1: bounded, attested solver failures never become `unsat`.
use sha2::{Digest, Sha256};
use std::{path::Path, time::Duration};

/// The pinned backend release, shared with verification-tool bootstrap.
pub const Z3_VERSION: &str = "5.1.0";
/// The caller's resource budget; timeout covers all subprocesses together.
#[derive(Debug, Clone, Copy)]
pub struct SmtBound {
    /// Maximum wall time (at most 60 seconds).
    pub timeout: Duration,
    /// Maximum input and captured output bytes (at most one MiB).
    pub bytes: usize,
}
/// A solver answer; faults and unsupported input have no logical authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmtResult {
    /// Satisfiable, with the backend's model text.
    Sat(String),
    /// The attested backend returned exactly `unsat`.
    Unsat,
    /// No usable answer; the reason is bounded diagnostic text.
    Unknown(String),
}
/// Observed backend identity and the exact obligation's answer; no admission token.
#[derive(Debug, Clone)]
pub struct SmtEvidence {
    /// Observed release, absent if probing failed.
    pub z3_version: Option<String>,
    /// Observed executable digest, absent if the binary was unavailable.
    pub z3_sha256: Option<String>,
    /// SHA-256 of the original SMT-LIB2 bytes.
    pub obligation_sha256: String,
    /// The declared resource budget.
    pub bound: SmtBound,
    /// Closed solver result.
    pub result: SmtResult,
}
#[cfg(any(test, unix))]
fn answer(output: &str, marker: &str) -> Result<SmtResult, &'static str> {
    let body = output
        .strip_prefix(&format!("{marker}\n"))
        .and_then(|s| s.strip_suffix(&format!("{marker}\n")))
        .ok_or("invalid_output")?;
    if body == "unsat\n" {
        return Ok(SmtResult::Unsat);
    }
    if body == "sat\n" {
        return Ok(SmtResult::Sat(String::new()));
    }
    if let Some(model) = body.strip_prefix("sat\n")
        && model.trim().starts_with('(')
        && model.trim().ends_with(')')
        && !model.contains("(error ")
    {
        return Ok(SmtResult::Sat(model.into()));
    }
    Err("solver_unknown_or_unsupported")
}
/// Dispatch silent SMT-LIB2 declarations/assertions through an immutable tool install.
/// `expected_sha256` comes from the caller's trusted tool attestation, never the input.
/// Output-producing commands, errors, missing tools and timeouts return `unknown`.
#[must_use]
pub fn dispatch_smt(
    text: &str,
    bound: SmtBound,
    binary: &Path,
    expected_sha256: &str,
) -> SmtEvidence {
    let evidence = SmtEvidence {
        bound,
        z3_version: None,
        z3_sha256: None,
        obligation_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
        result: SmtResult::Unknown("unsupported_platform".into()),
    };
    #[cfg(unix)]
    {
        let mut evidence = evidence;
        match host::dispatch(text, bound, binary, expected_sha256, &mut evidence) {
            Ok(result) => evidence.result = result,
            Err(reason) => evidence.result = SmtResult::Unknown(reason.into()),
        }
        evidence
    }
    #[cfg(not(unix))]
    {
        let _host_only = (bound, binary, expected_sha256);
        evidence
    }
}
#[cfg(unix)]
mod host {
    use super::{
        Digest, Duration, Path, Sha256, SmtBound, SmtEvidence, SmtResult, Z3_VERSION, answer,
    };
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{
        fs::File,
        io::{Read, Seek, SeekFrom, Write},
        process::{Command, Stdio},
        time::Instant,
    };
    fn temporary_file() -> Result<File, &'static str> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "norn-smt-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|_error| "io")?;
        std::fs::remove_file(path).map_err(|_error| "io")?;
        Ok(file)
    }
    fn capture(file: &mut File, limit: usize) -> Result<String, &'static str> {
        file.rewind().map_err(|_error| "io")?;
        let mut bytes = Vec::new();
        file.take(
            u64::try_from(limit)
                .map_err(|_error| "invalid_bound")?
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|_error| "io")?;
        if bytes.len() > limit {
            return Err("output_bound");
        }
        String::from_utf8(bytes).map_err(|_error| "invalid_output")
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "explicit bounded host solver adapter outside the kernel"
    )]
    fn run(
        binary: &Path,
        args: &[&str],
        input: &str,
        deadline: Instant,
        limit: usize,
    ) -> Result<String, &'static str> {
        if Instant::now() >= deadline {
            return Err("timeout");
        }
        if input.len() > limit {
            return Err("invalid_bound");
        }
        let mut stdin = temporary_file()?;
        stdin.write_all(input.as_bytes()).map_err(|_error| "io")?;
        stdin.seek(SeekFrom::Start(0)).map_err(|_error| "io")?;
        let mut stdout = temporary_file()?;
        let mut stderr = temporary_file()?;
        let mut child = Command::new(binary)
            .args(args)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout.try_clone().map_err(|_error| "io")?))
            .stderr(Stdio::from(stderr.try_clone().map_err(|_error| "io")?))
            .spawn()
            .map_err(|_error| "missing_or_unexecutable_binary")?;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                state => {
                    if state.is_err() || Instant::now() >= deadline {
                        let _killed = child.kill();
                        let _reaped = child.wait();
                        return Err(if state.is_err() {
                            "process_error"
                        } else {
                            "timeout"
                        });
                    }
                    std::thread::sleep(
                        Duration::from_millis(1)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
            }
        };
        if !status.success() {
            return Err("solver_crash_or_error");
        }
        if !capture(&mut stderr, limit)?.is_empty() {
            return Err("solver_warning_or_unsupported");
        }
        capture(&mut stdout, limit)
    }
    pub(super) fn dispatch(
        text: &str,
        bound: SmtBound,
        binary: &Path,
        expected: &str,
        evidence: &mut SmtEvidence,
    ) -> Result<SmtResult, &'static str> {
        if bound.timeout > Duration::from_secs(60)
            || bound.bytes == 0
            || bound.bytes > 1_048_576
            || text.len() > bound.bytes
        {
            return Err("invalid_bound");
        }
        let deadline = Instant::now()
            .checked_add(bound.timeout)
            .ok_or("invalid_bound")?;
        let canonical = binary.canonicalize().map_err(|_error| "missing_binary")?;
        let binary = canonical.as_path();
        let file = File::open(binary).map_err(|_error| "missing_binary")?;
        let metadata = file.metadata().map_err(|_error| "io")?;
        if !metadata.is_file() || metadata.len() > 268_435_456 {
            return Err("invalid_binary");
        }
        let mut hash = Sha256::new();
        std::io::copy(&mut file.take(268_435_457), &mut hash).map_err(|_error| "io")?;
        let hash = format!("{:x}", hash.finalize());
        evidence.z3_sha256 = Some(hash.clone());
        if hash != expected {
            return Err("attestation_mismatch");
        }
        let version = run(binary, &["--version"], "", deadline, bound.bytes)?;
        evidence.z3_version = Some(version.trim().into());
        if version.split_whitespace().nth(2) != Some(Z3_VERSION) {
            return Err("unsupported_version");
        }
        let marker = format!("norn_smt_{}", evidence.obligation_sha256);
        let query = |model: bool| {
            format!(
                "(set-option :produce-models true)\n{text}\n(echo \"{marker}\")\n(check-sat)\n{}(echo \"{marker}\")\n",
                if model { "(get-model)\n" } else { "" }
            )
        };
        if answer(
            &run(binary, &["-in", "-smt2"], &query(false), deadline, bound.bytes)?,
            &marker,
        )? == SmtResult::Unsat
        {
            return Ok(SmtResult::Unsat);
        }
        match answer(
            &run(binary, &["-in", "-smt2"], &query(true), deadline, bound.bytes)?,
            &marker,
        )? {
            SmtResult::Sat(model) if !model.is_empty() => Ok(SmtResult::Sat(model)),
            SmtResult::Sat(_) | SmtResult::Unsat | SmtResult::Unknown(_) => Err("model_unavailable"),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    proptest::proptest! {
        #[test]
        fn only_the_exact_framed_unsat_answer_has_unsat_authority(text in ".{0,128}") {
            let framed = format!("marker\n{text}\nmarker\n");
            proptest::prop_assert_eq!(answer(&framed, "marker") == Ok(SmtResult::Unsat), text == "unsat");
        }
    }
}
