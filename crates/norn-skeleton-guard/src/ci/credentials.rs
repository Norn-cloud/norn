//! Workflows hold no long-lived credential (owner, 2026-11-22).
//!
//! The rules are this module's typed tables and nothing else; there is no
//! per-file exemption. A workflow may read `secrets.GITHUB_TOKEN` (minted per
//! run) and no other secret: a provider with an OIDC path is reached over
//! OIDC, and a credential without one (a Cloudflare API token, an R2 key) is
//! read from Infisical over OIDC inside the job, never stored in GitHub.
//! Acquisition is reviewed code: only [`HELPER`] (or an OIDC action) mints an
//! OIDC token or reads Infisical, and an input named like a credential takes
//! its value only from the run token or from such a step's outputs, never from
//! `vars`, `matrix`, `inputs`, `env` or a literal.
//! Going back to a stored key fails here, in the policy lane and in the
//! base-owned trusted lane alike. `Norn-cloud/norn-ops` holds the same rules
//! in `ci/credential-policy.ts`; a change to one is a change to both.

use super::yaml::Value;

/// The one secret a workflow may read: the run's own short-lived token.
pub const RUN_TOKEN: &str = "GITHUB_TOKEN";

/// Where a banned name counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Any key or value, run scripts included (case-insensitive substring).
    Anywhere,
    /// An `env:` or `with:` key (case-insensitive, exact): where a stored key is wired in.
    Input,
}

/// A long-lived credential and the short-lived path that replaces it.
#[derive(Clone, Copy, Debug)]
pub struct Banned {
    /// The name, header or flag.
    pub needle: &'static str,
    /// Where it counts.
    pub scope: Scope,
    /// What it is.
    pub what: &'static str,
    /// What to use instead.
    pub instead: &'static str,
}

const CF_GLOBAL: &str = "the Cloudflare Global API Key";
const CF_TOKEN: &str = "a scoped Cloudflare API token that expires, read from Infisical over OIDC";
const AWS_KEY: &str = "a static AWS access key";
const AWS_ROLE: &str =
    "aws-actions/configure-aws-credentials with role-to-assume and id-token: write";
const INFISICAL_UA: &str = "an Infisical universal-auth client secret";
const INFISICAL_OIDC: &str = "Infisical OIDC auth (Infisical/secrets-action `method: oidc`, or `infisical login --method=oidc-auth`) with id-token: write";
const GCP_KEY: &str = "a GCP service-account JSON key";
const GCP_WIF: &str =
    "google-github-actions/auth with workload_identity_provider and id-token: write";

/// Every banned long-lived credential.
pub const BANNED: &[Banned] = &[
    Banned {
        needle: "CLOUDFLARE_API_KEY",
        scope: Scope::Anywhere,
        what: CF_GLOBAL,
        instead: CF_TOKEN,
    },
    Banned {
        needle: "CF_API_KEY",
        scope: Scope::Anywhere,
        what: CF_GLOBAL,
        instead: CF_TOKEN,
    },
    Banned {
        needle: "X-Auth-Key",
        scope: Scope::Anywhere,
        what: CF_GLOBAL,
        instead: CF_TOKEN,
    },
    Banned {
        needle: "AWS_ACCESS_KEY_ID",
        scope: Scope::Input,
        what: AWS_KEY,
        instead: AWS_ROLE,
    },
    Banned {
        needle: "AWS_SECRET_ACCESS_KEY",
        scope: Scope::Input,
        what: AWS_KEY,
        instead: AWS_ROLE,
    },
    Banned {
        needle: "aws-access-key-id",
        scope: Scope::Input,
        what: AWS_KEY,
        instead: AWS_ROLE,
    },
    Banned {
        needle: "aws-secret-access-key",
        scope: Scope::Input,
        what: AWS_KEY,
        instead: AWS_ROLE,
    },
    Banned {
        needle: "universal-auth",
        scope: Scope::Anywhere,
        what: INFISICAL_UA,
        instead: INFISICAL_OIDC,
    },
    Banned {
        needle: "INFISICAL_CLIENT_SECRET",
        scope: Scope::Anywhere,
        what: INFISICAL_UA,
        instead: INFISICAL_OIDC,
    },
    Banned {
        needle: "--client-secret",
        scope: Scope::Anywhere,
        what: INFISICAL_UA,
        instead: INFISICAL_OIDC,
    },
    Banned {
        needle: "client-secret",
        scope: Scope::Input,
        what: INFISICAL_UA,
        instead: INFISICAL_OIDC,
    },
    Banned {
        needle: "credentials_json",
        scope: Scope::Input,
        what: GCP_KEY,
        instead: GCP_WIF,
    },
    Banned {
        needle: "GOOGLE_CREDENTIALS",
        scope: Scope::Anywhere,
        what: GCP_KEY,
        instead: GCP_WIF,
    },
    Banned {
        needle: "GCP_SA_KEY",
        scope: Scope::Anywhere,
        what: GCP_KEY,
        instead: GCP_WIF,
    },
    Banned {
        needle: "SERVICE_ACCOUNT_KEY",
        scope: Scope::Anywhere,
        what: GCP_KEY,
        instead: GCP_WIF,
    },
];

/// An action with an OIDC mode, and the input that selects it.
#[derive(Clone, Copy, Debug)]
pub struct OidcAction {
    /// `uses:` prefix, lowercase, before `@`.
    pub action: &'static str,
    /// The input that must be present (`Some(value)`: and equal to it).
    pub input: &'static str,
    /// Its required value, if any.
    pub value: Option<&'static str>,
    /// Inputs that select a non-OIDC mode, rejected even beside the OIDC input.
    pub forbidden: &'static [&'static str],
}

/// Actions that must run in their OIDC mode (their defaults are not).
pub const OIDC_ACTIONS: &[OidcAction] = &[
    OidcAction {
        action: "infisical/secrets-action",
        input: "method",
        value: Some("oidc"),
        forbidden: &["client-id", "client-secret"],
    },
    OidcAction {
        action: "aws-actions/configure-aws-credentials",
        input: "role-to-assume",
        value: None,
        forbidden: &[
            "aws-access-key-id",
            "aws-secret-access-key",
            "aws-session-token",
            "web-identity-token-file",
            "role-chaining",
            "use-existing-credentials",
        ],
    },
    OidcAction {
        action: "google-github-actions/auth",
        input: "workload_identity_provider",
        value: None,
        forbidden: &["credentials_json"],
    },
];

/// The one reviewed acquisition script: it mints the OIDC token, logs in to
/// Infisical and writes the secrets it read as its step's outputs.
pub const HELPER: &str = "scripts/ci/infisical-oidc.sh";

/// What only [`HELPER`] (or an OIDC action) does; a workflow that does it
/// itself is unreviewed acquisition code.
const ACQUISITION_ONLY: &[(&str, &str)] = &[
    ("ACTIONS_ID_TOKEN_REQUEST", "mints a GitHub OIDC token"),
    ("getIDToken", "mints a GitHub OIDC token"),
    ("oidc-auth/login", "logs in to Infisical"),
    ("/secrets/raw", "reads Infisical secrets"),
];

/// The scope name of a composite action's `runs.steps`.
const COMPOSITE: &str = "composite action";

/// The run token, the one credential a workflow reads without acquiring it.
const RUN_TOKEN_EXPRESSIONS: &[&str] = &["github.token", "secrets.github_token"];

/// Every credential-policy error in one workflow document.
#[must_use]
pub fn errors(path: &str, document: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(document, None, &mut |kind, parent, text| {
        for name in secret_reads(parent, text) {
            out.push(format!(
                "{path}: reads secrets.{name}: a workflow reads no secret but {RUN_TOKEN}; reach the provider over OIDC or read the credential from Infisical over OIDC in the job"
            ));
        }
        for banned in BANNED {
            let hit = match banned.scope {
                Scope::Anywhere => contains_ignore_case(text, banned.needle),
                Scope::Input => kind == Input::Key && text.eq_ignore_ascii_case(banned.needle),
            };
            if hit {
                out.push(format!(
                    "{path}: `{}` is {}; use {}",
                    banned.needle, banned.what, banned.instead
                ));
            }
        }
    });
    walk(document, None, &mut |_, _, text| {
        for (needle, does) in ACQUISITION_ONLY {
            if contains_ignore_case(text, needle) {
                out.push(format!(
                    "{path}: `{needle}` {does} outside the reviewed helper; run {HELPER} (or an OIDC action) instead"
                ));
            }
        }
    });
    for (key, value) in document.get("env").map(Value::entries).unwrap_or_default() {
        source_error(&mut out, path, ("env", "env"), key, value, &[]);
    }
    let workflow_permissions = document.get("permissions");
    // A composite action's steps are checked like a job's, under a name no job id can have.
    let composite = document
        .get("runs")
        .filter(|runs| runs.get("steps").is_some())
        .map(|runs| (COMPOSITE.to_owned(), runs.clone()));
    let jobs = document
        .get("jobs")
        .map(Value::entries)
        .unwrap_or_default()
        .iter()
        .cloned()
        .chain(composite);
    for (job_name, job) in jobs {
        let job_name = job_name.as_str();
        let job = &job;
        if job.get("secrets").and_then(Value::as_str) == Some("inherit") {
            out.push(format!(
                "{path}::{job_name}: `secrets: inherit` hands every stored secret to a called workflow"
            ));
        }
        for section in ["env", "with"] {
            for (key, value) in job.get(section).map(Value::entries).unwrap_or_default() {
                source_error(&mut out, path, (job_name, section), key, value, &[]);
            }
        }
        let mut needs_oidc = Vec::new();
        if mentions_infisical(job) || document.get("env").is_some_and(mentions_infisical) {
            needs_oidc.push("talks to Infisical");
        }
        // Steps whose outputs may carry a credential: an admitted OIDC action, or a
        // run of the reviewed helper that writes no output of its own.
        let mut acquired: Vec<String> = Vec::new();
        for step in job.get("steps").map(Value::items).unwrap_or_default() {
            step_errors(
                &mut out,
                (path, job_name),
                step,
                &mut acquired,
                &mut needs_oidc,
            );
        }
        let permissions = job.get("permissions").or(workflow_permissions);
        if job_name != COMPOSITE
            && let Some(reason) = needs_oidc.first()
            && !grants_id_token(permissions)
        {
            out.push(format!(
                "{path}::{job_name}: {reason} without `permissions: id-token: write`; it must authenticate over OIDC"
            ));
        }
    }
    out
}

/// One step's errors; records the step in `acquired` when its outputs may carry
/// a credential, and in `needs_oidc` when it uses an OIDC action.
fn step_errors(
    out: &mut Vec<String>,
    (path, job_name): (&str, &str),
    step: &Value,
    acquired: &mut Vec<String>,
    needs_oidc: &mut Vec<&'static str>,
) {
    for section in ["env", "with"] {
        for (key, value) in step.get(section).map(Value::entries).unwrap_or_default() {
            source_error(out, path, (job_name, section), key, value, acquired);
        }
    }
    let uses = step
        .get("uses")
        .and_then(Value::as_str)
        .map(|uses| uses.split('@').next().unwrap_or(uses).to_ascii_lowercase());
    let mut yields_credential = false;
    if let Some(action) = OIDC_ACTIONS
        .iter()
        .find(|action| uses.as_deref() == Some(action.action))
    {
        needs_oidc.push(action.action);
        yields_credential = oidc_action_admitted(out, (path, job_name), action, step.get("with"));
    }
    let run = step.get("run").and_then(Value::as_str).unwrap_or_default();
    yields_credential |= runs_helper(run) && !contains_ignore_case(run, "GITHUB_OUTPUT");
    if yields_credential && let Some(id) = step.get("id").and_then(Value::as_str) {
        acquired.push(id.to_ascii_lowercase());
    }
    for line in run.lines() {
        let line = line.to_ascii_lowercase();
        if line.contains("infisical login")
            && !line.contains("--method=oidc-auth")
            && !line.contains("--method oidc-auth")
        {
            out.push(format!(
                "{path}::{job_name}: `infisical login` must use --method=oidc-auth"
            ));
        }
    }
}

/// Whether a run script invokes [`HELPER`] as a command: a line that is, after
/// any `NAME=value` assignments, `bash <HELPER>` or `<HELPER>` itself. A
/// mention in an `echo` or a comment does not count.
fn runs_helper(run: &str) -> bool {
    run.lines().any(|line| {
        let mut words = line.split_whitespace().skip_while(|word| {
            word.split_once('=').is_some_and(|(name, _)| {
                !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        });
        let first = words.next();
        let command = if first == Some("bash") {
            words.next()
        } else {
            first
        };
        command == Some(HELPER)
    })
}

/// Whether an OIDC action runs in its OIDC mode and nothing else; pushes why not.
fn oidc_action_admitted(
    out: &mut Vec<String>,
    (path, job_name): (&str, &str),
    action: &OidcAction,
    with: Option<&Value>,
) -> bool {
    let input = with.and_then(|with| with.get(action.input));
    let admitted = match (input.and_then(Value::as_str), action.value) {
        (Some(_), None) => true,
        (Some(got), Some(want)) => got == want,
        (None, _) => false,
    };
    if !admitted {
        out.push(format!(
            "{path}::{job_name}: {} must set `{}{}` (its OIDC mode)",
            action.action,
            action.input,
            action
                .value
                .map(|value| format!(": {value}"))
                .unwrap_or_default()
        ));
    }
    let forbidden: Vec<&str> = with
        .map(Value::keys)
        .into_iter()
        .flatten()
        .filter(|key| {
            action
                .forbidden
                .iter()
                .any(|bad| key.eq_ignore_ascii_case(bad))
        })
        .collect();
    for key in &forbidden {
        out.push(format!(
            "{path}::{job_name}: {} input `{key}` is a non-OIDC credential; set only `{}`",
            action.action, action.input
        ));
    }
    admitted && forbidden.is_empty()
}

/// Whether an `env:` or `with:` key names a credential (`*_TOKEN`, `*_SECRET`,
/// `*_API_KEY`, `*_KEY_ID`, …; `-` and `_` alike, any case). In `env:` a bare
/// `KEY` word counts too; in `with:` it is an action's cache or lookup key.
fn credential_named(section: &str, key: &str) -> bool {
    let upper = key.to_ascii_uppercase().replace('-', "_");
    let parts: Vec<&str> = upper.split('_').filter(|part| !part.is_empty()).collect();
    (section == "env" && parts.contains(&"KEY"))
        || parts.iter().any(|part| {
            matches!(
                *part,
                "TOKEN" | "SECRET" | "PASSWORD" | "PASSWD" | "APIKEY" | "PAT"
            )
        })
        || parts.windows(2).any(|pair| {
            matches!(
                pair,
                [
                    "API" | "ACCESS" | "PRIVATE" | "AUTH" | "SECRET" | "SIGNING" | "DEPLOY" | "SSH",
                    "KEY"
                ] | ["KEY", "ID"]
            )
        })
}

/// A credential-named input must be exactly one expression: the run token, or
/// an output of an acquiring step (`acquired`, lower-case step ids) before it.
fn source_error(
    out: &mut Vec<String>,
    path: &str,
    (scope, section): (&str, &str),
    key: &str,
    value: &Value,
    acquired: &[String],
) {
    if !credential_named(section, key) {
        return;
    }
    let text = value.as_str().unwrap_or_default().trim();
    let whole = expressions(text);
    let reviewed = match whole.as_slice() {
        [only] if text.starts_with("${{") && text.ends_with("}}") => {
            let expression = only.trim().to_ascii_lowercase();
            RUN_TOKEN_EXPRESSIONS.contains(&expression.as_str())
                || expression
                    .strip_prefix("steps.")
                    .and_then(|rest| rest.split_once(".outputs."))
                    .is_some_and(|(id, name)| {
                        acquired.iter().any(|known| known == id)
                            && !name.is_empty()
                            && name
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    })
        }
        _ => false,
    };
    if !reviewed {
        out.push(format!(
            "{path}::{scope}: `{key}` takes a credential from an unreviewed source; a credential input comes only from the run token or the outputs of an earlier step that runs {HELPER} (or an OIDC action), never from vars, matrix, inputs, env or a literal"
        ));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Input {
    /// An `env:` or `with:` key.
    Key,
    /// Any other key, or a value.
    Value,
}

/// Calls `visit(kind, parent, text)` on every scalar: mapping keys and values.
fn walk(value: &Value, parent: Option<&str>, visit: &mut dyn FnMut(Input, Option<&str>, &str)) {
    match value {
        Value::Str(text) => visit(Input::Value, parent, text),
        Value::Seq(items) => {
            for item in items {
                walk(item, None, visit);
            }
        }
        Value::Map(entries) => {
            for (key, item) in entries {
                let kind = if matches!(parent, Some("env" | "with")) {
                    Input::Key
                } else {
                    Input::Value
                };
                visit(kind, None, key);
                walk(item, Some(key), visit);
            }
        }
    }
}

/// The secret names an expression reads (`secrets.X`, `secrets['X']`, or the
/// whole context as `*`), except the run token. Expressions are `${{ }}`
/// spans, and the whole of an `if:` value. Contexts and names are
/// case-insensitive; names are reported in upper case.
fn secret_reads(parent: Option<&str>, text: &str) -> Vec<String> {
    let expressions: Vec<&str> = if parent == Some("if") {
        vec![text]
    } else {
        expressions(text)
    };
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let take = |from: &str| -> String { from.chars().take_while(|c| word(*c)).collect() };
    let mut names = Vec::new();
    for expression in expressions {
        let lower = expression.to_ascii_lowercase();
        let mut pieces = lower.split("secrets");
        let mut before = pieces.next().unwrap_or_default();
        for after in pieces {
            let prefixed = before
                .chars()
                .next_back()
                .is_some_and(|c| word(c) || c == '.');
            before = after;
            if prefixed {
                continue;
            }
            let name = if let Some(dot) = after.strip_prefix('.') {
                take(dot)
            } else if let Some(index) = after.trim_start().strip_prefix('[') {
                take(index.trim_start().trim_start_matches(['\'', '"']))
            } else if after.chars().next().is_some_and(word) {
                continue;
            } else {
                String::new()
            };
            if !name.eq_ignore_ascii_case(RUN_TOKEN) {
                names.push(if name.is_empty() {
                    "*".to_owned()
                } else {
                    name.to_ascii_uppercase()
                });
            }
        }
    }
    names
}

/// The `${{ … }}` expressions in a value, as GitHub's runner reads them
/// (`TemplateReader.cs`): an expression ends at the first `}}` outside a
/// single-quoted string literal (`''` inside one is an escaped quote, which
/// toggling twice handles). An unclosed expression runs to the end of the value.
fn expressions(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some((_, after)) = rest.split_once("${{") {
        let mut in_string = false;
        let mut end = after.len();
        let mut chars = after.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            match c {
                '\'' => in_string = !in_string,
                '}' if !in_string && chars.peek().is_some_and(|&(_, next)| next == '}') => {
                    end = at;
                    break;
                }
                _ => {}
            }
        }
        let (expression, tail) = after.split_at_checked(end).unwrap_or((after, ""));
        found.push(expression);
        rest = tail.strip_prefix("}}").unwrap_or(tail);
    }
    found
}

fn contains_ignore_case(text: &str, needle: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn mentions_infisical(value: &Value) -> bool {
    let mut found = false;
    walk(value, None, &mut |_, _, text| {
        found |= contains_ignore_case(text, "infisical");
    });
    found
}

/// Whether a `permissions:` value grants `id-token: write`.
fn grants_id_token(permissions: Option<&Value>) -> bool {
    match permissions {
        Some(Value::Str(all)) => all == "write-all",
        Some(map @ Value::Map(_)) => map.get("id-token").and_then(Value::as_str) == Some("write"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::yaml;

    fn check(source: &str) -> Vec<String> {
        super::errors("w.yml", &yaml::parse(source).expect("fixture parses"))
    }

    /// Each fixture must fail, naming the expected fragment.
    const MUST_FAIL: &[(&str, &str)] = &[
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: wrangler deploy\n        env:\n          CLOUDFLARE_API_TOKEN: ${{ secrets.CLOUDFLARE_API_TOKEN }}\n",
            "secrets.CLOUDFLARE_API_TOKEN",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    env:\n      CLOUDFLARE_API_KEY: ${{ vars.K }}\n    steps:\n      - run: true\n",
            "Cloudflare Global API Key",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: 'curl -H \"X-Auth-Key: $K\" https://api.cloudflare.com/client/v4/zones'\n",
            "`X-Auth-Key`",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ${{ secrets['CF_API_KEY'] }}\n",
            "secrets.CF_API_KEY",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: aws s3 ls\n        env:\n          AWS_ACCESS_KEY_ID: ${{ vars.ID }}\n",
            "static AWS access key",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n        with:\n          aws-access-key-id: ${{ vars.ID }}\n          aws-region: eu-west-1\n",
            "role-to-assume",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: Infisical/secrets-action@v1\n        with:\n          method: oidc\n          identity-id: x\n",
            "id-token: write",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: Infisical/secrets-action@v1\n        with:\n          client-id: x\n          client-secret: ${{ vars.S }}\n",
            "`method: oidc`",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: infisical login --method=universal-auth --client-id x --client-secret \"$S\"\n",
            "universal-auth",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: infisical login --silent\n",
            "--method=oidc-auth",
        ),
        (
            "permissions:\n  contents: read\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: curl -fsS https://app.infisical.com/api/v1/auth/oidc-auth/login\n",
            "talks to Infisical without",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    permissions:\n      contents: read\n    steps:\n      - run: infisical run --method=oidc-auth -- true\n",
            "talks to Infisical without",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: google-github-actions/auth@v2\n        with:\n          credentials_json: ${{ vars.J }}\n",
            "GCP service-account JSON key",
        ),
        (
            "jobs:\n  a:\n    uses: ./.github/workflows/b.yml\n    secrets: inherit\n",
            "secrets: inherit",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo '${{ toJSON(secrets) }}'\n",
            "secrets.*",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: some/action@v1\n        with:\n          token: ${{ secrets.NORN_SCCACHE_SECRET }}\n",
            "secrets.NORN_SCCACHE_SECRET",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n        env:\n          K: ${{ SECRETS.R2_KEY }}\n",
            "secrets.R2_KEY",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n        if: secrets.DEPLOY_KEY != ''\n",
            "secrets.DEPLOY_KEY",
        ),
        // A `}}` inside a quoted literal does not end the expression (TemplateReader.cs).
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo \"${{ format('{0}}}', 'a') || secrets.DEPLOY_KEY }}\"\n",
            "secrets.DEPLOY_KEY",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo \"${{ format('it''s }}', secrets.R2_KEY) }}\"\n",
            "secrets.R2_KEY",
        ),
        // configure-aws-credentials: no static or file credential beside the role.
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n        with:\n          role-to-assume: arn:aws:iam::1:role/ci\n          aws-session-token: ${{ vars.T }}\n",
            "`aws-session-token` is a non-OIDC credential",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n        with:\n          role-to-assume: arn:aws:iam::1:role/ci\n          web-identity-token-file: /tmp/t\n",
            "`web-identity-token-file` is a non-OIDC credential",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n        with:\n          role-to-assume: arn:aws:iam::1:role/ci\n          role-chaining: true\n",
            "`role-chaining` is a non-OIDC credential",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n        with:\n          role-to-assume: arn:aws:iam::1:role/ci\n          use-existing-credentials: true\n",
            "`use-existing-credentials` is a non-OIDC credential",
        ),
        // A mention of the helper is not a run of it.
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: keys\n        run: echo scripts/ci/infisical-oidc.sh\n      - run: true\n        env:\n          R2_SECRET: ${{ steps.keys.outputs.k }}\n",
            "`R2_SECRET` takes a credential",
        ),
        // The diagnostic names the input, never the rejected value.
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n        env:\n          DEPLOY_TOKEN: literal-value-42\n",
            "`DEPLOY_TOKEN` takes a credential from an unreviewed source",
        ),
        // A credential input from vars, matrix, inputs, env or a literal.
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: ./deploy\n        env:\n          CLOUDFLARE_API_TOKEN: ${{ vars.CF_TOKEN }}\n",
            "`CLOUDFLARE_API_TOKEN` takes a credential",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    strategy:\n      matrix:\n        token: [a]\n    steps:\n      - uses: some/action@v1\n        with:\n          api-token: ${{ matrix.token }}\n",
            "`api-token` takes a credential",
        ),
        (
            "env:\n  DEPLOY_TOKEN: abc123\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n",
            "`DEPLOY_TOKEN` takes a credential",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n        env:\n          KEY: ${{ env.STORED }}\n",
            "`KEY` takes a credential",
        ),
        (
            "runs:\n  using: composite\n  steps:\n    - run: ./deploy\n      shell: bash\n      env:\n        API_KEY: ${{ inputs.api-key }}\n",
            "`API_KEY` takes a credential",
        ),
        // Acquisition is the reviewed helper's: no inline OIDC mint or Infisical read, and a
        // credential comes only from an earlier helper step that writes no output of its own.
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: 'curl -H \"Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN\" \"$ACTIONS_ID_TOKEN_REQUEST_URL\"'\n",
            "mints a GitHub OIDC token outside the reviewed helper",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/github-script@v9\n        with:\n          script: return core.getIDToken()\n",
            "mints a GitHub OIDC token outside the reviewed helper",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: fetch\n        run: curl -fsS https://keys.invalid/k >> \"$GITHUB_OUTPUT\"\n      - run: true\n        env:\n          R2_SECRET: ${{ steps.fetch.outputs.k }}\n",
            "`R2_SECRET` takes a credential",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: keys\n        run: |\n          bash scripts/ci/infisical-oidc.sh prod /x K\n          echo k=stored >> \"$GITHUB_OUTPUT\"\n      - run: true\n        env:\n          R2_SECRET: ${{ steps.keys.outputs.k }}\n",
            "`R2_SECRET` takes a credential",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n        env:\n          R2_SECRET: ${{ steps.keys.outputs.k }}\n      - id: keys\n        run: bash scripts/ci/infisical-oidc.sh prod /x K\n",
            "`R2_SECRET` takes a credential",
        ),
        (
            "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: keys\n        run: bash scripts/ci/infisical-oidc.sh prod /x K\n      - run: true\n        env:\n          R2_SECRET: ${{ steps.keys.outputs.k || vars.FALLBACK }}\n",
            "`R2_SECRET` takes a credential",
        ),
    ];

    /// Each fixture must pass.
    const MUST_PASS: &[&str] = &[
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/github-script@v9\n        with:\n          github-token: ${{ secrets.GITHUB_TOKEN }}\n",
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    permissions:\n      id-token: write\n      contents: read\n    steps:\n      - uses: Infisical/secrets-action@v1\n        with:\n          method: oidc\n          identity-id: ${{ vars.INFISICAL_IDENTITY_ID }}\n",
        "permissions: write-all\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: infisical login --method=oidc-auth --machine-identity-id x --oidc-jwt \"$JWT\"\n",
        "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n        with:\n          role-to-assume: arn:aws:iam::1:role/ci\n          aws-region: eu-west-1\n",
        "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: google-github-actions/auth@v2\n        with:\n          workload_identity_provider: projects/1/locations/global/workloadIdentityPools/p/providers/g\n",
        // An S3-compatible client reads Infisical-held keys, from the reviewed helper's
        // outputs, through AWS_* in its own script.
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    permissions:\n      id-token: write\n    steps:\n      - id: infisical\n        run: bash scripts/ci/infisical-oidc.sh prod /x KEY_ID\n        env:\n          INFISICAL_IDENTITY_ID: ${{ vars.ID }}\n      - run: export AWS_ACCESS_KEY_ID=\"$KEY\"\n        env:\n          KEY: ${{ steps.infisical.outputs.key_id }}\n",
        // The run token under any name; an AWS role's own outputs.
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: gh pr list\n        env:\n          GH_TOKEN: ${{ github.token }}\n          PUSH_TOKEN: ${{ secrets.GITHUB_TOKEN }}\n",
        "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - id: aws\n        uses: aws-actions/configure-aws-credentials@v4\n        with:\n          role-to-assume: arn:aws:iam::1:role/ci\n          aws-region: eu-west-1\n          output-credentials: true\n      - run: true\n        env:\n          SESSION_TOKEN: ${{ steps.aws.outputs.aws-session-token }}\n",
        // A cache key is not a credential; a quoted `}}` does not end an expression early.
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/cache@v4\n        with:\n          key: bun-${{ hashFiles('bun.lock') }}\n      - run: echo \"${{ format('{0}}}', 'it''s }}') }}\"\n",
        // Names containing the word are not reads of the secrets context.
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ${{ steps.my-secrets.outputs.n }} ${{ env.NO_SECRETS }}\n",
    ];

    #[test]
    fn long_lived_credentials_fail() {
        for (source, expected) in MUST_FAIL {
            let errors = check(source);
            assert!(
                errors.iter().any(|error| error.contains(expected)),
                "expected `{expected}` in {errors:?} for\n{source}"
            );
        }
    }

    #[test]
    fn diagnostics_never_echo_a_value() {
        let errors = check(
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n        env:\n          DEPLOY_TOKEN: literal-value-42\n",
        );
        assert!(
            !errors.is_empty()
                && errors
                    .iter()
                    .all(|error| !error.contains("literal-value-42")),
            "{errors:?}"
        );
    }

    #[test]
    fn oidc_and_the_run_token_pass() {
        for source in MUST_PASS {
            assert_eq!(check(source), Vec::<String>::new(), "{source}");
        }
    }
}
