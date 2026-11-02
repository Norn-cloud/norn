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
    /// Closed solver result.
    pub result: SmtResult,
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[cfg(any(test, not(target_family = "wasm")))]
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
    if let Some(model) = body.strip_prefix("sat\n") {
        if model.trim().starts_with('(')
            && model.trim().ends_with(')')
            && !model.contains("(error ")
        {
            return Ok(SmtResult::Sat(model.into()));
        }
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
        z3_version: None,
        z3_sha256: None,
        obligation_sha256: digest(text.as_bytes()),
        result: SmtResult::Unknown("unsupported_platform".into()),
    };
    #[cfg(not(target_family = "wasm"))]
    {
        let mut evidence = evidence;
        match host::dispatch(text, bound, binary, expected_sha256, &mut evidence) {
            Ok(result) => evidence.result = result,
            Err(reason) => evidence.result = SmtResult::Unknown(reason.into()),
        }
        evidence
    }
    #[cfg(target_family = "wasm")]
    {
        let _host_only = (bound, binary, expected_sha256);
        evidence
    }
}
#[cfg(not(target_family = "wasm"))]
mod host {
    use super::*;
    use std::{
        fs::File,
        io::{Read, Seek, SeekFrom, Write},
        process::{Command, Stdio},
        time::Instant,
    };
    use wait_timeout::ChildExt;
    fn capture(file: &mut File, limit: usize) -> Result<String, &'static str> {
        file.rewind().map_err(|_| "io")?;
        let mut bytes = Vec::new();
        file.take(
            u64::try_from(limit)
                .map_err(|_| "invalid_bound")?
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|_| "io")?;
        if bytes.len() > limit {
            return Err("output_bound");
        }
        String::from_utf8(bytes).map_err(|_| "invalid_output")
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
        let mut stdin = tempfile::tempfile().map_err(|_| "io")?;
        stdin.write_all(input.as_bytes()).map_err(|_| "io")?;
        stdin.seek(SeekFrom::Start(0)).map_err(|_| "io")?;
        let mut stdout = tempfile::tempfile().map_err(|_| "io")?;
        let mut stderr = tempfile::tempfile().map_err(|_| "io")?;
        let mut child = Command::new(binary)
            .args(args)
            .stdin(Stdio::from(stdin))
            .stdout(Stdio::from(stdout.try_clone().map_err(|_| "io")?))
            .stderr(Stdio::from(stderr.try_clone().map_err(|_| "io")?))
            .spawn()
            .map_err(|_| "missing_or_unexecutable_binary")?;
        let status = child.wait_timeout(deadline.saturating_duration_since(Instant::now()));
        let status = match status {
            Ok(Some(status)) => status,
            other => {
                let _killed = child.kill();
                let _reaped = child.wait();
                return Err(if other.is_err() {
                    "process_error"
                } else {
                    "timeout"
                });
            }
        };
        if !status.success() {
            return Err("solver_crash_or_error");
        }
        if !capture(&mut stderr, limit)?.trim().is_empty() {
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
        let file = File::open(binary).map_err(|_| "missing_binary")?;
        let metadata = file.metadata().map_err(|_| "io")?;
        if !metadata.is_file() || metadata.len() > 268_435_456 {
            return Err("invalid_binary");
        }
        let mut hash = Sha256::new();
        std::io::copy(&mut file.take(268_435_457), &mut hash).map_err(|_| "io")?;
        let hash = format!("{:x}", hash.finalize());
        evidence.z3_sha256 = Some(hash.clone());
        if hash != expected {
            return Err("attestation_mismatch");
        }
        let version = run(binary, &["--version"], "", deadline, bound.bytes)?;
        evidence.z3_version = Some(version.trim().into());
        if !version
            .split_whitespace()
            .nth(2)
            .is_some_and(|v| v == Z3_VERSION)
        {
            return Err("unsupported_version");
        }
        let marker = format!("norn_smt_{}", evidence.obligation_sha256);
        let query = |model: bool| {
            format!(
                "(set-option :produce-models true)\n{text}\n(echo \"{marker}\")\n(check-sat)\n{}(echo \"{marker}\")\n",
                if model { "(get-model)\n" } else { "" }
            )
        };
        let result = answer(
            &run(
                binary,
                &["-in", "-smt2"],
                &query(false),
                deadline,
                bound.bytes,
            )?,
            &marker,
        )?;
        match result {
            SmtResult::Sat(_) => match answer(
                &run(
                    binary,
                    &["-in", "-smt2"],
                    &query(true),
                    deadline,
                    bound.bytes,
                )?,
                &marker,
            )? {
                SmtResult::Sat(model) if !model.is_empty() => Ok(SmtResult::Sat(model)),
                _ => Err("model_unavailable"),
            },
            SmtResult::Unsat => Ok(SmtResult::Unsat),
            SmtResult::Unknown(_) => Err("solver_unknown"),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn result_framing_refuses_spoofed_or_unsupported_output() {
        assert_eq!(answer("x\nunsat\nx\n", "x"), Ok(SmtResult::Unsat));
        for output in [
            "unsat\n",
            "(error unsupported)\nx\nunsat\nx\n",
            "x\nunknown\nx\n",
            "x\nunsat\n(error failed)\nx\n",
        ] {
            assert!(answer(output, "x").is_err());
        }
    }
    proptest::proptest! {
        #[test]
        fn arbitrary_unframed_output_cannot_claim_unsat(text in ".{0,128}") {
            proptest::prop_assume!(text != "unsat");
            let framed = format!("marker\n{text}\nmarker\n");
            proptest::prop_assert_ne!(answer(&framed, "marker"), Ok(SmtResult::Unsat));
        }
    }
}
