//! Function contract admission (T1-FN-1, #2776; ruling 2026-09-27 compute
//! boundary §2).
//!
//! The three records are defined in [`norn_ir::function`]; this module admits
//! them. Every finding is located by an RFC 6901 pointer into the record.
//!
//! - [`admit_function_manifest`]: the manifest is a v1 contract whose code,
//!   schema, runtime, and dependency identities are content digests (never a
//!   mutable tag, bare URI, or object key), whose transitive dependency
//!   closure is present and matches its digest, whose determinism claim
//!   carries every evidence field its profile requires, whose deterministic
//!   profiles are capability-free, whose every resource bound (output
//!   included) is finite and positive, and which uses the action cache only
//!   for a qualified profile. Conflicting golden-run output digests
//!   quarantine a hermetic container.
//! - [`check_function_invocation`]: the command names the admitted manifest,
//!   runs in `main` (a fork never performs), carries only canonical scalar
//!   inputs and content-addressed blobs (secrets and mutable URLs are never
//!   semantic inputs), stays within the manifest's input bound, declares a
//!   freshness policy it can satisfy, and is bounded by a deadline and
//!   retry/cost ceilings.
//! - [`check_function_receipt`]: the receipt is bound to its dispatch (the
//!   invocation digest, command, generation, manifest, execution profile,
//!   input identity, and dependency token), its output manifest matches its
//!   outputs, its summary fits the §1 envelope, a completed outcome stayed
//!   within the manifest's bounds, and a cache hit is a success of a
//!   cacheable profile under the command's own action key.
//!
//! Admission checks the contract only; whether a result may drive a commit
//! is result reconciliation in `norn-kernel` (T2-RES-1).

use std::collections::BTreeSet;

use norn_ir::RuntimeValue;
use norn_ir::canonical::{CanonicalJsonError, canonical_json_of};
use norn_ir::function::{
    ADMITTED_WASM_IMPORTS, CacheOrigin, CachePolicy, DeterminismProfile, Digest, FUNCTION_ABI_V1,
    FreshnessMode, FunctionInvocation, FunctionManifest, FunctionOutcome, FunctionReceipt,
    HermeticEvidence, ImplementationFormat, MAX_RECEIPT_SUMMARY_BYTES, ResourceBounds,
    WasmQualification, output_manifest_digest,
};

use crate::property_types::error;
use crate::{Diagnostic, Span};

/// Stable `function.*` codes.
pub mod codes {
    /// The text does not decode as the record.
    pub const INVALID_DOCUMENT: &str = "function.validation.invalid_document";

    /// The manifest declares an ABI other than `norn.function-abi/1`.
    pub const UNSUPPORTED_ABI: &str = "function.manifest.unsupported_abi";
    /// A required manifest name or id is blank.
    pub const MANIFEST_BLANK_VALUE: &str = "function.manifest.blank_value";
    /// A code, schema, runtime, or dependency identity is not a content
    /// digest (a mutable tag, bare URI, or object key).
    pub const MUTABLE_IDENTITY: &str = "function.manifest.mutable_identity";
    /// A locator or capability scope can carry a credential (userinfo, a
    /// scheme, query, or fragment, or a secret-shaped value).
    pub const EMBEDDED_SECRET: &str = "function.manifest.embedded_secret";
    /// A manifest string or key is not NFC: hashing normalizes it, so it
    /// would share an identity with a different value.
    pub const MANIFEST_NON_CANONICAL_TEXT: &str = "function.manifest.non_canonical_text";
    /// The transitive dependency closure is absent (or empty for an image).
    pub const MISSING_DEPENDENCY_CLOSURE: &str = "function.manifest.missing_dependency_closure";
    /// The closure digest does not match its entries, or entries repeat or
    /// are unsorted.
    pub const DEPENDENCY_CLOSURE_MISMATCH: &str = "function.manifest.dependency_closure_mismatch";
    /// The packaging, platform, or capabilities do not fit the profile.
    pub const PROFILE_MISMATCH: &str = "function.manifest.profile_mismatch";
    /// A determinism claim lacks an evidence field its profile requires.
    pub const MISSING_DETERMINISM_EVIDENCE: &str = "function.manifest.missing_determinism_evidence";
    /// An evidence field holds a value the profile does not admit.
    pub const INADMISSIBLE_EVIDENCE: &str = "function.manifest.inadmissible_evidence";
    /// Golden runs produced different output digests: the key is quarantined.
    pub const CONFLICTING_GOLDEN_RUNS: &str = "function.manifest.conflicting_golden_runs";
    /// A resource bound is absent or zero.
    pub const UNBOUNDED_RESOURCE: &str = "function.manifest.unbounded_resource";
    /// The action cache is declared for a profile that is not qualified.
    pub const UNCACHEABLE_PROFILE: &str = "function.manifest.uncacheable_profile";
    /// Two pipeline steps share an id.
    pub const DUPLICATE_STEP: &str = "function.manifest.duplicate_step";

    /// A required invocation member is blank.
    pub const INVOCATION_BLANK_VALUE: &str = "function.invocation.blank_value";
    /// The invocation names a manifest other than the admitted one.
    pub const MANIFEST_MISMATCH: &str = "function.invocation.manifest_mismatch";
    /// The invocation is dispatched outside the `main` world.
    pub const FORK_PERFORM: &str = "function.invocation.fork_perform";
    /// A semantic input is an array or object, not a canonical scalar.
    pub const NON_SCALAR_INPUT: &str = "function.invocation.non_scalar_input";
    /// A semantic input is, or is named as, a secret.
    pub const SECRET_INPUT: &str = "function.invocation.secret_input";
    /// An invocation string or key is not NFC.
    pub const INVOCATION_NON_CANONICAL_TEXT: &str = "function.invocation.non_canonical_text";
    /// A semantic input or blob identity is a mutable URL or object key.
    pub const MUTABLE_INPUT: &str = "function.invocation.mutable_input";
    /// The staged inputs exceed the manifest's input bound.
    pub const INPUT_BOUND: &str = "function.invocation.input_bound";
    /// The declared freshness policy lacks what it is checked against.
    pub const FRESHNESS_SHAPE: &str = "function.invocation.freshness_shape";
    /// The deadline, attempt ceiling, or cost ceiling is absent or exceeds
    /// the profile.
    pub const UNBOUNDED_COMMAND: &str = "function.invocation.unbounded_command";

    /// A required receipt member is blank.
    pub const RECEIPT_BLANK_VALUE: &str = "function.receipt.blank_value";
    /// A receipt string or key is not NFC.
    pub const RECEIPT_NON_CANONICAL_TEXT: &str = "function.receipt.non_canonical_text";
    /// The receipt is not bound to the dispatch it claims.
    pub const DISPATCH_MISMATCH: &str = "function.receipt.dispatch_mismatch";
    /// The attempt number is zero or beyond the retry ceiling.
    pub const ATTEMPT_OUT_OF_RANGE: &str = "function.receipt.attempt_out_of_range";
    /// The output-manifest digest does not match the outputs and summary.
    pub const OUTPUT_MANIFEST_MISMATCH: &str = "function.receipt.output_manifest_mismatch";
    /// The summary or failure detail exceeds the §1 envelope.
    pub const SUMMARY_BOUND: &str = "function.receipt.summary_bound";
    /// A completed outcome used more than the manifest's bounds.
    pub const BOUND_EXCEEDED: &str = "function.receipt.bound_exceeded";
    /// A cache hit that is not a success of a cacheable profile under the
    /// command's own action key.
    pub const CACHE_ORIGIN: &str = "function.receipt.cache_origin";
}

/// An admitted manifest and its identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedFunction {
    /// The manifest.
    pub manifest: FunctionManifest,
    /// Its content digest.
    pub digest: Digest,
    /// Its execution-profile digest.
    pub execution_profile: Digest,
}

fn at<const N: usize>(segments: [&str; N]) -> Span {
    Span::from_segments(segments)
}

fn canonical_failure(record: &str, failure: &CanonicalJsonError) -> Diagnostic {
    error(
        codes::INVALID_DOCUMENT,
        format!("the {record} cannot be canonicalized: {failure}"),
        Span::pointer(""),
    )
}

fn undecodable(record: &str, decode: &serde_json::Error) -> Vec<Diagnostic> {
    vec![error(
        codes::INVALID_DOCUMENT,
        format!("the document is not a Function {record}: {decode}"),
        Span::pointer(""),
    )]
}

/// Decode a Function manifest from its JSON text. Numbers are read exactly.
///
/// # Errors
///
/// [`codes::INVALID_DOCUMENT`] when the text does not decode (closed members,
/// types, and enums are the decoder's).
pub fn decode_function_manifest(text: &str) -> Result<FunctionManifest, Vec<Diagnostic>> {
    serde_json::from_str::<FunctionManifest>(text)
        .map_err(|decode| undecodable("manifest", &decode))
}

/// Decode a Function invocation command from its JSON text.
///
/// # Errors
///
/// As [`decode_function_manifest`].
pub fn decode_function_invocation(text: &str) -> Result<FunctionInvocation, Vec<Diagnostic>> {
    serde_json::from_str::<FunctionInvocation>(text)
        .map_err(|decode| undecodable("invocation", &decode))
}

/// Decode a Function result receipt from its JSON text.
///
/// # Errors
///
/// As [`decode_function_manifest`].
pub fn decode_function_receipt(text: &str) -> Result<FunctionReceipt, Vec<Diagnostic>> {
    serde_json::from_str::<FunctionReceipt>(text).map_err(|decode| undecodable("receipt", &decode))
}

/// Admit a manifest: every finding of [`check_function_manifest`], or the
/// admitted manifest with its identities.
///
/// # Errors
///
/// The findings when there is any.
pub fn admit_function_manifest(
    manifest: &FunctionManifest,
) -> Result<AdmittedFunction, Vec<Diagnostic>> {
    let mut findings = check_function_manifest(manifest);
    let identities = manifest
        .content_digest()
        .and_then(|digest| Ok((digest, manifest.execution_profile_digest()?)));
    match identities {
        Ok((digest, execution_profile)) if findings.is_empty() => Ok(AdmittedFunction {
            manifest: manifest.clone(),
            digest,
            execution_profile,
        }),
        Ok(_) => Err(findings),
        Err(failure) => {
            findings.push(canonical_failure("manifest", &failure));
            Err(findings)
        }
    }
}

/// Whether a record survives Norn1 canonicalization unchanged. Hashing
/// NFC-normalizes strings and keys, so a record that does not round-trip
/// would share its identity with a different value (for example an entry
/// point or input in a decomposed form). Canonicalization failures count as
/// not canonical.
fn text_is_canonical<T: PartialEq>(
    canonical: Result<Vec<u8>, CanonicalJsonError>,
    record: &T,
    decode: impl FnOnce(Vec<u8>) -> Option<T>,
) -> bool {
    canonical
        .ok()
        .and_then(decode)
        .is_some_and(|decoded| &decoded == record)
}

fn non_canonical(code: &str, record: &str) -> Diagnostic {
    error(
        code,
        format!(
            "a {record} string or key is not in NFC form; hashing normalizes it, so it would share an identity with a different value"
        ),
        Span::pointer(""),
    )
}

fn blank(value: &str) -> bool {
    value.trim().is_empty()
}

fn require_digest(digest: &Digest, code: &str, what: &str, span: Span, out: &mut Vec<Diagnostic>) {
    if !digest.is_well_formed() {
        out.push(error(
            code,
            format!(
                "{what} {:?} is not a content digest (sha256:<64 hex>); a tag, URI, or object key is never an identity",
                digest.as_str()
            ),
            span,
        ));
    }
}

/// Check a manifest. Empty when it is admissible. See the module docs.
#[must_use]
pub fn check_function_manifest(manifest: &FunctionManifest) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if manifest.abi != FUNCTION_ABI_V1 {
        out.push(error(
            codes::UNSUPPORTED_ABI,
            format!(
                "abi {:?} is not supported; expected {FUNCTION_ABI_V1:?}",
                manifest.abi
            ),
            at(["abi"]),
        ));
    }
    if !text_is_canonical(canonical_json_of(manifest), manifest, |bytes| {
        serde_json::from_slice::<FunctionManifest>(&bytes).ok()
    }) {
        out.push(non_canonical(
            codes::MANIFEST_NON_CANONICAL_TEXT,
            "manifest",
        ));
    }
    check_manifest_names(manifest, &mut out);
    check_manifest_identities(manifest, &mut out);
    check_closure(manifest, &mut out);
    check_profile(manifest, &mut out);
    check_bounds(&manifest.bounds, &mut out);
    if manifest.cache == CachePolicy::ActionCache && !manifest.is_cacheable_profile() {
        out.push(error(
            codes::UNCACHEABLE_PROFILE,
            format!(
                "profile {} is not qualified for content-based reuse; declare cache \"none\"",
                manifest.determinism.name()
            ),
            at(["cache"]),
        ));
    }
    out
}

fn check_manifest_names(manifest: &FunctionManifest, out: &mut Vec<Diagnostic>) {
    let names = [
        (manifest.function_id.as_str(), at(["functionId"])),
        (
            manifest.implementation.entry_point.as_str(),
            at(["implementation", "entryPoint"]),
        ),
        (manifest.runtime.id.as_str(), at(["runtime", "id"])),
        (
            manifest.runtime.version.as_str(),
            at(["runtime", "version"]),
        ),
        (manifest.platform.os.as_str(), at(["platform", "os"])),
        (manifest.platform.arch.as_str(), at(["platform", "arch"])),
        (manifest.input_schema.id.as_str(), at(["inputSchema", "id"])),
        (
            manifest.output_schema.id.as_str(),
            at(["outputSchema", "id"]),
        ),
        (manifest.error_schema.id.as_str(), at(["errorSchema", "id"])),
    ];
    for (value, span) in names {
        if blank(value) {
            out.push(error(
                codes::MANIFEST_BLANK_VALUE,
                "the value must not be blank".to_owned(),
                span,
            ));
        }
    }
    for (index, capability) in manifest.capabilities.iter().enumerate() {
        let index = index.to_string();
        if blank(&capability.name) || blank(&capability.scope) {
            out.push(error(
                codes::MANIFEST_BLANK_VALUE,
                "a capability needs a name and a scope".to_owned(),
                at(["capabilities", &index]),
            ));
        }
        if secret_value(&capability.scope) || !safe_scope(&capability.scope) {
            out.push(error(
                codes::EMBEDDED_SECRET,
                "a capability scope names what is granted, never a credential; the supervisor binds credentials"
                    .to_owned(),
                at(["capabilities", &index, "scope"]),
            ));
        }
    }
    let mut step_ids = BTreeSet::new();
    for (index, step) in manifest.steps.iter().enumerate() {
        let index = index.to_string();
        if blank(&step.id) {
            out.push(error(
                codes::MANIFEST_BLANK_VALUE,
                "the value must not be blank".to_owned(),
                at(["steps", &index, "id"]),
            ));
        } else if !step_ids.insert(step.id.as_str()) {
            out.push(error(
                codes::DUPLICATE_STEP,
                format!("pipeline step {:?} is declared twice", step.id),
                at(["steps", &index, "id"]),
            ));
        }
    }
}

fn check_manifest_identities(manifest: &FunctionManifest, out: &mut Vec<Diagnostic>) {
    let code = codes::MUTABLE_IDENTITY;
    for (schema, member) in [
        (&manifest.input_schema, "inputSchema"),
        (&manifest.output_schema, "outputSchema"),
        (&manifest.error_schema, "errorSchema"),
    ] {
        require_digest(
            &schema.digest,
            code,
            "the schema digest",
            at([member, "digest"]),
            out,
        );
    }
    for (index, step) in manifest.steps.iter().enumerate() {
        let index = index.to_string();
        for (schema, member) in [
            (&step.input_schema, "inputSchema"),
            (&step.output_schema, "outputSchema"),
        ] {
            require_digest(
                &schema.digest,
                code,
                "the step schema digest",
                at(["steps", &index, member, "digest"]),
                out,
            );
        }
    }
    let implementation = &manifest.implementation;
    require_digest(
        &implementation.digest,
        code,
        "the implementation digest",
        at(["implementation", "digest"]),
        out,
    );
    if let Some(locator) = &implementation.locator {
        let pinned = format!("@{}", implementation.digest.as_str());
        match locator.strip_suffix(&pinned) {
            None => out.push(error(
                code,
                format!(
                    "locator {locator:?} does not pin the implementation digest; a mutable tag or bare URI is never a code identity"
                ),
                at(["implementation", "locator"]),
            )),
            Some(reference) if !plain_reference(reference) => out.push(error(
                codes::EMBEDDED_SECRET,
                "the locator is not a plain registry reference; userinfo, a scheme, a query, or a fragment can carry a credential, and artifact authentication belongs to the supervisor"
                    .to_owned(),
                at(["implementation", "locator"]),
            )),
            Some(_) => {}
        }
    }
    require_digest(
        &manifest.runtime.digest,
        code,
        "the runtime digest",
        at(["runtime", "digest"]),
        out,
    );
}

/// A capability scope names what is granted (a host, a model id, an account
/// alias) in a closed character set: no userinfo, query, fragment, or
/// whitespace that could carry a credential.
fn safe_scope(scope: &str) -> bool {
    scope
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '*' | '+'))
}

/// `registry/path` with no scheme, userinfo, query, fragment, or whitespace.
fn plain_reference(reference: &str) -> bool {
    !reference.is_empty()
        && !reference.contains("://")
        && !reference.contains(['@', '?', '#'])
        && !reference.contains(char::is_whitespace)
}

fn check_closure(manifest: &FunctionManifest, out: &mut Vec<Diagnostic>) {
    let Some(closure) = &manifest.dependency_closure else {
        out.push(error(
            codes::MISSING_DEPENDENCY_CLOSURE,
            "the transitive dependency/asset closure is required".to_owned(),
            at(["dependencyClosure"]),
        ));
        return;
    };
    if closure.entries.is_empty()
        && manifest.implementation.format == ImplementationFormat::OciImage
    {
        out.push(error(
            codes::MISSING_DEPENDENCY_CLOSURE,
            "an OCI image's closure must name its base image and pinned dependencies".to_owned(),
            at(["dependencyClosure", "entries"]),
        ));
    }
    let mut previous: Option<&str> = None;
    for (index, entry) in closure.entries.iter().enumerate() {
        let index = index.to_string();
        if blank(&entry.name) {
            out.push(error(
                codes::MANIFEST_BLANK_VALUE,
                "the value must not be blank".to_owned(),
                at(["dependencyClosure", "entries", &index, "name"]),
            ));
        }
        if previous.is_some_and(|name| name >= entry.name.as_str()) {
            out.push(error(
                codes::DEPENDENCY_CLOSURE_MISMATCH,
                format!(
                    "closure entry {:?} is repeated or out of order; entries are sorted by name",
                    entry.name
                ),
                at(["dependencyClosure", "entries", &index, "name"]),
            ));
        }
        previous = Some(entry.name.as_str());
        require_digest(
            &entry.digest,
            codes::MUTABLE_IDENTITY,
            "the dependency digest",
            at(["dependencyClosure", "entries", &index, "digest"]),
            out,
        );
    }
    match closure.computed_digest() {
        Ok(computed) if computed == closure.digest => {}
        Ok(computed) => out.push(error(
            codes::DEPENDENCY_CLOSURE_MISMATCH,
            format!(
                "the closure digest {} does not match its entries ({computed})",
                closure.digest
            ),
            at(["dependencyClosure", "digest"]),
        )),
        Err(failure) => out.push(canonical_failure("dependency closure", &failure)),
    }
}

fn check_profile(manifest: &FunctionManifest, out: &mut Vec<Diagnostic>) {
    let format = manifest.implementation.format;
    let deterministic = !matches!(manifest.determinism, DeterminismProfile::Effectful(_));
    match &manifest.determinism {
        DeterminismProfile::QualifiedWasm(evidence) => {
            if format != ImplementationFormat::WasmModule || manifest.platform.arch != "wasm32" {
                out.push(error(
                    codes::PROFILE_MISMATCH,
                    "qualified_wasm needs a wasm_module implementation on arch wasm32".to_owned(),
                    at(["implementation", "format"]),
                ));
            }
            check_wasm_evidence(evidence, out);
        }
        DeterminismProfile::HermeticContainer(evidence) => {
            if format != ImplementationFormat::OciImage {
                out.push(error(
                    codes::PROFILE_MISMATCH,
                    "hermetic_container needs an oci_image implementation".to_owned(),
                    at(["implementation", "format"]),
                ));
            }
            check_hermetic_evidence(evidence, out);
        }
        DeterminismProfile::Effectful(policy) => {
            if policy.max_attempts == 0 {
                out.push(error(
                    codes::INADMISSIBLE_EVIDENCE,
                    "an effectful producer needs at least one attempt".to_owned(),
                    at(["determinism", "effectful", "maxAttempts"]),
                ));
            }
        }
    }
    if deterministic && !manifest.capabilities.is_empty() {
        out.push(error(
            codes::PROFILE_MISMATCH,
            format!(
                "profile {} is capability-free; capabilities belong to an effectful producer",
                manifest.determinism.name()
            ),
            at(["capabilities"]),
        ));
    }
}

fn missing_evidence(profile: &str, member: &str, out: &mut Vec<Diagnostic>) {
    out.push(error(
        codes::MISSING_DETERMINISM_EVIDENCE,
        format!("the {profile} claim needs evidence field {member}"),
        at(["determinism", profile, member]),
    ));
}

fn inadmissible(profile: &str, member: &str, detail: &str, out: &mut Vec<Diagnostic>) {
    out.push(error(
        codes::INADMISSIBLE_EVIDENCE,
        format!("{profile} evidence {member}: {detail}"),
        at(["determinism", profile, member]),
    ));
}

fn check_wasm_evidence(evidence: &WasmQualification, out: &mut Vec<Diagnostic>) {
    const P: &str = "qualified_wasm";
    match &evidence.import_closure {
        None => missing_evidence(P, "importClosure", out),
        Some(imports) => {
            for import in imports {
                if !ADMITTED_WASM_IMPORTS.contains(&import.as_str()) {
                    inadmissible(
                        P,
                        "importClosure",
                        &format!("import {import:?} is not admitted deterministic ABI glue"),
                        out,
                    );
                }
            }
        }
    }
    match evidence.fresh_instance {
        None => missing_evidence(P, "freshInstance", out),
        Some(false) => inadmissible(P, "freshInstance", "instances must be fresh or reset", out),
        Some(true) => {}
    }
    match evidence.threads {
        None => missing_evidence(P, "threads", out),
        Some(true) => inadmissible(P, "threads", "threads and shared state are refused", out),
        Some(false) => {}
    }
    if evidence.simd.is_none() {
        missing_evidence(P, "simd", out);
    }
    if evidence.float_policy.is_none() {
        missing_evidence(P, "floatPolicy", out);
    }
    for (value, member) in [
        (evidence.fuel, "fuel"),
        (evidence.max_stack_bytes, "maxStackBytes"),
        (evidence.max_module_bytes, "maxModuleBytes"),
    ] {
        match value {
            None => missing_evidence(P, member, out),
            Some(0) => inadmissible(P, member, "must be positive", out),
            Some(_) => {}
        }
    }
}

fn check_hermetic_evidence(evidence: &HermeticEvidence, out: &mut Vec<Diagnostic>) {
    const P: &str = "hermetic_container";
    for (value, member, refused) in [
        (evidence.network, "network", true),
        (evidence.credentials, "credentials", true),
        (evidence.isolated_scratch, "isolatedScratch", false),
    ] {
        match value {
            None => missing_evidence(P, member, out),
            Some(flag) if flag == refused => {
                inadmissible(P, member, "isolation is not enforced", out);
            }
            Some(_) => {}
        }
    }
    for (value, member) in [
        (&evidence.locale, "locale"),
        (&evidence.timezone, "timezone"),
    ] {
        match value {
            None => missing_evidence(P, member, out),
            Some(text) if blank(text) => inadmissible(P, member, "must be pinned", out),
            Some(_) => {}
        }
    }
    for (index, run) in evidence.golden_runs.iter().enumerate() {
        require_digest(
            &run.output_digest,
            codes::MUTABLE_IDENTITY,
            "the golden-run output digest",
            at([
                "determinism",
                P,
                "goldenRuns",
                &index.to_string(),
                "outputDigest",
            ]),
            out,
        );
    }
    let digests: BTreeSet<&str> = evidence
        .golden_runs
        .iter()
        .map(|run| run.output_digest.as_str())
        .collect();
    if digests.len() > 1 {
        out.push(error(
            codes::CONFLICTING_GOLDEN_RUNS,
            format!(
                "golden runs produced {} different output digests; the key is quarantined",
                digests.len()
            ),
            at(["determinism", P, "goldenRuns"]),
        ));
    }
    for (index, run) in evidence.golden_runs.iter().enumerate() {
        if blank(&run.placement) {
            out.push(error(
                codes::MANIFEST_BLANK_VALUE,
                "a golden run names its placement".to_owned(),
                at([
                    "determinism",
                    P,
                    "goldenRuns",
                    &index.to_string(),
                    "placement",
                ]),
            ));
        }
    }
    let placements: BTreeSet<&str> = evidence
        .golden_runs
        .iter()
        .map(|run| run.placement.trim())
        .filter(|placement| !placement.is_empty())
        .collect();
    if evidence.qualified && (evidence.golden_runs.len() < 2 || placements.len() < 2) {
        missing_evidence(P, "goldenRuns", out);
    }
}

fn check_bounds(bounds: &ResourceBounds, out: &mut Vec<Diagnostic>) {
    for (value, member) in [
        (bounds.max_cpu_millis, "maxCpuMillis"),
        (bounds.max_wall_millis, "maxWallMillis"),
        (bounds.max_memory_bytes, "maxMemoryBytes"),
        (bounds.max_input_bytes, "maxInputBytes"),
        (bounds.max_decoded_bytes, "maxDecodedBytes"),
        (bounds.max_output_bytes, "maxOutputBytes"),
    ] {
        if value.is_none_or(|bound| bound == 0) {
            out.push(error(
                codes::UNBOUNDED_RESOURCE,
                format!("bound {member} must be a positive number"),
                at(["bounds", member]),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------------

/// Fragments that name a credential anywhere in an input name, matched
/// against the name's words joined without separators (so `accessToken`,
/// `access_token`, `accesstoken`, and `api_key_value` all match).
/// Credentials are bound by the `perform` supervisor through capabilities,
/// never passed as semantic inputs. The input schema (by identity) is the
/// authoritative contract; these names are the admission backstop.
const SECRET_FRAGMENTS: &[&str] = &[
    "accesskey",
    "accesstoken",
    "apikey",
    "apitoken",
    "authorization",
    "authtoken",
    "bearer",
    "credential",
    "encryptionkey",
    "idtoken",
    "passphrase",
    "passwd",
    "password",
    "privatekey",
    "refreshtoken",
    "secret",
    "sessiontoken",
    "signingkey",
];

/// Whole words that name a credential (`auth`, but not `author`).
const SECRET_WORDS: &[&str] = &["auth", "cookie", "jwt"];

/// Lower-case value prefixes of well-known credential encodings.
const SECRET_PREFIXES: &[&str] = &[
    "bearer ",
    "basic ",
    "token ",
    "sk-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "-----begin",
];

/// The lower-case words of a name: split at separators and at lower-to-upper
/// case changes (`apiKey`, `api_key`, and `X-Api-Key` all give `api`, `key`).
fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            previous_lower = false;
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        if c.is_uppercase() && previous_lower && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        previous_lower = c.is_lowercase() || c.is_ascii_digit();
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn secret_name(name: &str) -> bool {
    let words = name_words(name);
    let compact = words.concat();
    SECRET_FRAGMENTS
        .iter()
        .any(|fragment| compact.contains(fragment))
        || words
            .iter()
            .any(|word| SECRET_WORDS.contains(&word.as_str()))
        // A final `token` is a credential (`token`, `csrfToken`); a
        // qualifying one is not (`tokenCount`, `maxTokens`).
        || words.last().is_some_and(|word| word == "token")
}

/// A JSON Web Token: three base64url segments, the header starting `eyJ`.
fn looks_like_jwt(value: &str) -> bool {
    let segments: Vec<&str> = value.split('.').collect();
    segments.len() == 3
        && value.starts_with("eyJ")
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '='))
        })
}

/// An AWS access key id (`AKIA` and 16 upper-case letters or digits).
fn looks_like_access_key_id(value: &str) -> bool {
    value.len() == 20
        && value.starts_with("AKIA")
        && value
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn secret_value(value: &str) -> bool {
    let trimmed = value.trim();
    let lower = trimmed.to_lowercase();
    SECRET_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        || looks_like_jwt(trimmed)
        || looks_like_access_key_id(trimmed)
}

/// A string that is, as a whole, a URL or object-store key: a scheme with
/// `://` and no whitespace. Text that merely *mentions* a URL is data.
fn mutable_reference(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.split_once("://").is_some_and(|(scheme, rest)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !rest.is_empty()
            && !trimmed.contains(char::is_whitespace)
    })
}

/// Check an invocation command against its admitted manifest. Empty when it
/// is admissible. See the module docs.
#[must_use]
pub fn check_function_invocation(
    invocation: &FunctionInvocation,
    admitted: &AdmittedFunction,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (value, span) in [
        (invocation.tenant.as_str(), at(["tenant"])),
        (invocation.world.as_str(), at(["world"])),
        (invocation.command_id.as_str(), at(["commandId"])),
        (invocation.run.id.as_str(), at(["run", "id"])),
        (invocation.authority.id.as_str(), at(["authority", "id"])),
        (invocation.fencing.token.as_str(), at(["fencing", "token"])),
    ] {
        if blank(value) {
            out.push(error(
                codes::INVOCATION_BLANK_VALUE,
                "the value must not be blank".to_owned(),
                span,
            ));
        }
    }
    if invocation.world != "main" && !blank(&invocation.world) {
        out.push(error(
            codes::FORK_PERFORM,
            format!(
                "world {:?} is a fork; only main performs, and a fork never dispatches a Function",
                invocation.world
            ),
            at(["world"]),
        ));
    }
    if invocation.manifest != admitted.digest {
        out.push(error(
            codes::MANIFEST_MISMATCH,
            format!(
                "the command names manifest {}, but the admitted manifest is {}",
                invocation.manifest, admitted.digest
            ),
            at(["manifest"]),
        ));
    }
    if !text_is_canonical(canonical_json_of(invocation), invocation, |bytes| {
        serde_json::from_slice::<FunctionInvocation>(&bytes).ok()
    }) {
        out.push(non_canonical(
            codes::INVOCATION_NON_CANONICAL_TEXT,
            "invocation",
        ));
    }
    check_inputs(invocation, &mut out);
    check_input_bound(invocation, &admitted.manifest, &mut out);
    check_freshness(invocation, &mut out);
    check_ceilings(invocation, &admitted.manifest, &mut out);
    out
}

fn check_inputs(invocation: &FunctionInvocation, out: &mut Vec<Diagnostic>) {
    for (name, value) in &invocation.inputs {
        let span = || at(["inputs", name.as_str()]);
        if secret_name(name) {
            out.push(error(
                codes::SECRET_INPUT,
                format!(
                    "input {name:?} names a secret; credentials are capabilities, never inputs"
                ),
                span(),
            ));
        }
        match value {
            RuntimeValue::Array(_) | RuntimeValue::Object(_) => out.push(error(
                codes::NON_SCALAR_INPUT,
                format!("input {name:?} is not a canonical scalar; pass structured data as a blob"),
                span(),
            )),
            RuntimeValue::String(text) if secret_value(text) => out.push(error(
                codes::SECRET_INPUT,
                format!("input {name:?} holds a secret value"),
                span(),
            )),
            RuntimeValue::String(text) if mutable_reference(text) => out.push(error(
                codes::MUTABLE_INPUT,
                format!(
                    "input {name:?} is a mutable URL or object key; stage the bytes and pass their digest as a blob"
                ),
                span(),
            )),
            RuntimeValue::Null | RuntimeValue::Bool(_) | RuntimeValue::Number(_) | RuntimeValue::String(_) => {}
        }
    }
    for (name, blob) in &invocation.blobs {
        if secret_name(name) {
            out.push(error(
                codes::SECRET_INPUT,
                format!("blob {name:?} names a secret; credentials are capabilities, never inputs"),
                at(["blobs", name.as_str()]),
            ));
        }
        require_digest(
            &blob.digest,
            codes::MUTABLE_INPUT,
            "the blob digest",
            at(["blobs", name.as_str(), "digest"]),
            out,
        );
    }
}

fn check_input_bound(
    invocation: &FunctionInvocation,
    manifest: &FunctionManifest,
    out: &mut Vec<Diagnostic>,
) {
    let Some(bound) = manifest.bounds.max_input_bytes else {
        return;
    };
    let scalar_bytes = match norn_ir::canonical::canonical_json_of(&invocation.inputs) {
        Ok(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        Err(failure) => {
            out.push(canonical_failure("invocation inputs", &failure));
            return;
        }
    };
    let staged = invocation.blobs.values().fold(scalar_bytes, |sum, blob| {
        sum.saturating_add(blob.size_bytes)
    });
    if staged > bound {
        out.push(error(
            codes::INPUT_BOUND,
            format!("the staged inputs hold {staged} bytes; the manifest admits {bound}"),
            at(["blobs"]),
        ));
    }
}

fn check_freshness(invocation: &FunctionInvocation, out: &mut Vec<Diagnostic>) {
    match invocation.freshness {
        FreshnessMode::RelevantState if invocation.dependency.is_empty() => out.push(error(
            codes::FRESHNESS_SHAPE,
            "relevant_state freshness needs a dependency token that pins facts, predicates, or collections"
                .to_owned(),
            at(["dependency"]),
        )),
        FreshnessMode::DestinationCas if invocation.destination.is_none() => out.push(error(
            codes::FRESHNESS_SHAPE,
            "destination_cas freshness needs a destination and its expected owner version"
                .to_owned(),
            at(["destination"]),
        )),
        FreshnessMode::RelevantState | FreshnessMode::DestinationCas | FreshnessMode::AppendOnly => {}
    }
    let token = &invocation.dependency;
    let blank_pins = token
        .facts
        .iter()
        .map(|pin| (pin.object_type.as_str(), pin.key.as_str()))
        .chain(
            token
                .predicates
                .iter()
                .map(|pin| (pin.object_type.as_str(), pin.predicate.as_str())),
        )
        .chain(
            token
                .collections
                .iter()
                .map(|pin| (pin.object_type.as_str(), "collection")),
        )
        .any(|(object_type, name)| blank(object_type) || blank(name));
    if blank_pins {
        out.push(error(
            codes::INVOCATION_BLANK_VALUE,
            "every dependency pin needs an object type and a key or predicate".to_owned(),
            at(["dependency"]),
        ));
    }
    if let Some(destination) = &invocation.destination
        && (blank(&destination.object_type) || blank(&destination.key))
    {
        out.push(error(
            codes::INVOCATION_BLANK_VALUE,
            "the destination needs an object type and a key".to_owned(),
            at(["destination"]),
        ));
    }
}

fn check_ceilings(
    invocation: &FunctionInvocation,
    manifest: &FunctionManifest,
    out: &mut Vec<Diagnostic>,
) {
    if invocation.deadline_millis == 0 {
        out.push(error(
            codes::UNBOUNDED_COMMAND,
            "the command needs a deadline".to_owned(),
            at(["deadlineMillis"]),
        ));
    }
    if invocation.retry.max_attempts == 0 {
        out.push(error(
            codes::UNBOUNDED_COMMAND,
            "the attempt ceiling must be at least 1".to_owned(),
            at(["retry", "maxAttempts"]),
        ));
    }
    if invocation.retry.max_cost_units == 0 {
        out.push(error(
            codes::UNBOUNDED_COMMAND,
            "the command needs a positive cost ceiling".to_owned(),
            at(["retry", "maxCostUnits"]),
        ));
    }
    if let DeterminismProfile::Effectful(policy) = &manifest.determinism
        && invocation.retry.max_attempts > policy.max_attempts
    {
        out.push(error(
            codes::UNBOUNDED_COMMAND,
            format!(
                "{} attempts exceed the producer's ceiling of {}",
                invocation.retry.max_attempts, policy.max_attempts
            ),
            at(["retry", "maxAttempts"]),
        ));
    }
}

// ---------------------------------------------------------------------------
// Receipt
// ---------------------------------------------------------------------------

fn mismatch(member: &str, expected: &str, found: &str, out: &mut Vec<Diagnostic>) {
    out.push(error(
        codes::DISPATCH_MISMATCH,
        format!("{member} is {found}, but the dispatch pinned {expected}"),
        at([member]),
    ));
}

/// Check a result receipt against the invocation it claims and the admitted
/// manifest. Empty when it is admissible. See the module docs.
#[must_use]
pub fn check_function_receipt(
    receipt: &FunctionReceipt,
    invocation: &FunctionInvocation,
    admitted: &AdmittedFunction,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if !text_is_canonical(canonical_json_of(receipt), receipt, |bytes| {
        serde_json::from_slice::<FunctionReceipt>(&bytes).ok()
    }) {
        out.push(non_canonical(codes::RECEIPT_NON_CANONICAL_TEXT, "receipt"));
    }
    check_binding(receipt, invocation, admitted, &mut out);
    if receipt.attempt == 0 || receipt.attempt > invocation.retry.max_attempts {
        out.push(error(
            codes::ATTEMPT_OUT_OF_RANGE,
            format!(
                "attempt {} is outside 1..={}",
                receipt.attempt, invocation.retry.max_attempts
            ),
            at(["attempt"]),
        ));
    }
    if blank(&receipt.provenance.executor) || blank(&receipt.provenance.placement) {
        out.push(error(
            codes::RECEIPT_BLANK_VALUE,
            "the provenance needs an executor and a placement".to_owned(),
            at(["provenance"]),
        ));
    }
    check_outcome(receipt, &admitted.manifest, &mut out);
    check_cache_origin(receipt, invocation, admitted, &mut out);
    out
}

fn check_binding(
    receipt: &FunctionReceipt,
    invocation: &FunctionInvocation,
    admitted: &AdmittedFunction,
    out: &mut Vec<Diagnostic>,
) {
    if receipt.command_id != invocation.command_id {
        mismatch(
            "commandId",
            &invocation.command_id,
            &receipt.command_id,
            out,
        );
    }
    if receipt.run != invocation.run {
        mismatch(
            "run",
            &format!("{}@{}", invocation.run.id, invocation.run.generation),
            &format!("{}@{}", receipt.run.id, receipt.run.generation),
            out,
        );
    }
    let pins = invocation.content_digest().and_then(|dispatch| {
        Ok((
            dispatch,
            invocation.input_identity()?,
            invocation.dependency.digest()?,
        ))
    });
    let (dispatch, inputs, dependency) = match pins {
        Ok(pins) => pins,
        Err(failure) => {
            out.push(canonical_failure("invocation", &failure));
            return;
        }
    };
    for (member, expected, found) in [
        ("dispatch", &dispatch, &receipt.dispatch),
        ("manifest", &admitted.digest, &receipt.manifest),
        (
            "executionProfile",
            &admitted.execution_profile,
            &receipt.execution_profile,
        ),
        ("inputIdentity", &inputs, &receipt.input_identity),
        ("dependencyToken", &dependency, &receipt.dependency_token),
    ] {
        if expected != found {
            mismatch(member, expected.as_str(), found.as_str(), out);
        }
    }
    if receipt.provenance.runtime != admitted.manifest.runtime.digest {
        out.push(error(
            codes::DISPATCH_MISMATCH,
            format!(
                "the attempt ran runtime {}, but the manifest pins {}",
                receipt.provenance.runtime, admitted.manifest.runtime.digest
            ),
            at(["provenance", "runtime"]),
        ));
    }
}

fn canonical_size(value: &norn_ir::RuntimeObject) -> Result<u64, CanonicalJsonError> {
    norn_ir::canonical::canonical_json_of(value)
        .map(|bytes| u64::try_from(bytes.len()).unwrap_or(u64::MAX))
}

fn check_summary_size(
    value: &norn_ir::RuntimeObject,
    span: Span,
    what: &str,
    out: &mut Vec<Diagnostic>,
) {
    match canonical_size(value) {
        Ok(size) if size > MAX_RECEIPT_SUMMARY_BYTES => out.push(error(
            codes::SUMMARY_BOUND,
            format!(
                "the {what} holds {size} canonical bytes; at most {MAX_RECEIPT_SUMMARY_BYTES} cross the receipt, the rest lives in output objects"
            ),
            span,
        )),
        Ok(_) => {}
        Err(failure) => out.push(canonical_failure(what, &failure)),
    }
}

fn check_outcome(
    receipt: &FunctionReceipt,
    manifest: &FunctionManifest,
    out: &mut Vec<Diagnostic>,
) {
    let completed = match &receipt.outcome {
        FunctionOutcome::Success(success) => {
            for (index, output) in success.outputs.iter().enumerate() {
                require_digest(
                    &output.digest,
                    codes::OUTPUT_MANIFEST_MISMATCH,
                    "the output digest",
                    at([
                        "outcome",
                        "success",
                        "outputs",
                        &index.to_string(),
                        "digest",
                    ]),
                    out,
                );
            }
            match output_manifest_digest(&success.outputs, &success.summary) {
                Ok(computed) if computed == success.output_manifest => {}
                Ok(computed) => out.push(error(
                    codes::OUTPUT_MANIFEST_MISMATCH,
                    format!(
                        "the output manifest {} does not match the outputs and summary ({computed})",
                        success.output_manifest
                    ),
                    at(["outcome", "success", "outputManifest"]),
                )),
                Err(failure) => out.push(canonical_failure("output manifest", &failure)),
            }
            check_summary_size(
                &success.summary,
                at(["outcome", "success", "summary"]),
                "summary",
                out,
            );
            let written = success
                .outputs
                .iter()
                .fold(0_u64, |sum, output| sum.saturating_add(output.size_bytes));
            if manifest
                .bounds
                .max_output_bytes
                .is_some_and(|bound| written > bound)
            {
                out.push(error(
                    codes::BOUND_EXCEEDED,
                    format!("the outputs hold {written} bytes, beyond the manifest's output bound"),
                    at(["outcome", "success", "outputs"]),
                ));
            }
            true
        }
        FunctionOutcome::Failure(failure) => {
            if blank(&failure.code) {
                out.push(error(
                    codes::RECEIPT_BLANK_VALUE,
                    "a typed failure needs a code".to_owned(),
                    at(["outcome", "failure", "code"]),
                ));
            }
            check_summary_size(
                &failure.detail,
                at(["outcome", "failure", "detail"]),
                "failure detail",
                out,
            );
            true
        }
        FunctionOutcome::ResourceExhausted { .. }
        | FunctionOutcome::DeadlineExceeded {}
        | FunctionOutcome::Cancelled {} => false,
    };
    if completed {
        check_completed_use(receipt, manifest, out);
    }
}

/// A completed outcome stayed within the manifest's bounds, and a qualified
/// WASM attempt reports fuel within its budget.
fn check_completed_use(
    receipt: &FunctionReceipt,
    manifest: &FunctionManifest,
    out: &mut Vec<Diagnostic>,
) {
    let used = &receipt.resources;
    let bounds = &manifest.bounds;
    for (value, bound, member) in [
        (used.cpu_millis, bounds.max_cpu_millis, "cpuMillis"),
        (used.wall_millis, bounds.max_wall_millis, "wallMillis"),
        (
            used.peak_memory_bytes,
            bounds.max_memory_bytes,
            "peakMemoryBytes",
        ),
        (used.output_bytes, bounds.max_output_bytes, "outputBytes"),
    ] {
        if bound.is_some_and(|bound| value > bound) {
            out.push(error(
                codes::BOUND_EXCEEDED,
                format!(
                    "a completed attempt reports {member} {value}, beyond the manifest's bound"
                ),
                at(["resources", member]),
            ));
        }
    }
    if let DeterminismProfile::QualifiedWasm(evidence) = &manifest.determinism {
        let budget = evidence.fuel.unwrap_or(0);
        match used.fuel {
            Some(fuel) if fuel <= budget => {}
            Some(fuel) => out.push(error(
                codes::BOUND_EXCEEDED,
                format!(
                    "a completed attempt reports fuel {fuel}, beyond the qualified budget {budget}"
                ),
                at(["resources", "fuel"]),
            )),
            None => out.push(error(
                codes::BOUND_EXCEEDED,
                "a completed qualified WASM attempt reports the fuel it consumed".to_owned(),
                at(["resources", "fuel"]),
            )),
        }
    }
}

fn check_cache_origin(
    receipt: &FunctionReceipt,
    invocation: &FunctionInvocation,
    admitted: &AdmittedFunction,
    out: &mut Vec<Diagnostic>,
) {
    let CacheOrigin::Hit { action_key } = &receipt.cache else {
        return;
    };
    let manifest = &admitted.manifest;
    if manifest.cache != CachePolicy::ActionCache || !manifest.is_cacheable_profile() {
        out.push(error(
            codes::CACHE_ORIGIN,
            "the manifest does not admit content-based reuse".to_owned(),
            at(["cache"]),
        ));
    }
    if !matches!(receipt.outcome, FunctionOutcome::Success(_)) {
        out.push(error(
            codes::CACHE_ORIGIN,
            "only a success can come from the action cache".to_owned(),
            at(["cache"]),
        ));
    }
    match invocation.action_key(manifest) {
        Ok(expected) if &expected == action_key => {}
        Ok(expected) => out.push(error(
            codes::CACHE_ORIGIN,
            format!("the hit's action key {action_key} is not the command's ({expected})"),
            at(["cache", "hit", "actionKey"]),
        )),
        Err(failure) => out.push(canonical_failure("action key", &failure)),
    }
}
