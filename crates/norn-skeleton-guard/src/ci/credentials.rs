//! Workflows hold no long-lived credential (owner, 2026-11-22).
//!
//! The premise: GitHub stores no Actions secret for this repository, its
//! environments or its organization, and the only credential a workflow gets
//! is the run's own `GITHUB_TOKEN`. With nothing stored, no expression can
//! read or leak a stored key, so this module parses no `${{ }}` expression.
//! The zero is enforced outside the workflow text: the daily credential audit
//! in `Norn-cloud/norn-ops` fails on any stored secret (repository,
//! environment, organization or Dependabot), with no allowlist, and fails as
//! UNVERIFIED when it cannot list them. zizmor (pinned, `core.sh` `cheap`)
//! covers `secrets: inherit`, over-provisioned and unredacted secrets,
//! template injection, excessive permissions and unpinned actions. gitleaks
//! covers literals in the tree.
//!
//! What stays here is what neither covers: the long-lived credential *types*
//! (the Cloudflare Global key, static AWS keys, Infisical universal auth, GCP
//! JSON keys) and the actions whose default mode is not OIDC. These are typed
//! tables with no per-file exemption. `Norn-cloud/norn-ops` holds the same
//! rules in `ci/credential-policy.ts`; a change to one is a change to both.

use super::yaml::Value;

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

/// The scope name of a composite action's `runs.steps`.
const COMPOSITE: &str = "composite action";

/// Every credential-policy error in one workflow document.
#[must_use]
pub fn errors(path: &str, document: &Value) -> Vec<String> {
    let mut out = Vec::new();
    walk(document, None, &mut |kind, text| {
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
        let mut needs_oidc = Vec::new();
        if mentions_infisical(&job) || document.get("env").is_some_and(mentions_infisical) {
            needs_oidc.push("talks to Infisical");
        }
        for step in job.get("steps").map(Value::items).unwrap_or_default() {
            step_errors(&mut out, (path, job_name), step, &mut needs_oidc);
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

/// One step's errors; records in `needs_oidc` when it uses an OIDC action.
fn step_errors(
    out: &mut Vec<String>,
    (path, job_name): (&str, &str),
    step: &Value,
    needs_oidc: &mut Vec<&'static str>,
) {
    let uses = step
        .get("uses")
        .and_then(Value::as_str)
        .map(|uses| uses.split('@').next().unwrap_or(uses).to_ascii_lowercase());
    if let Some(action) = OIDC_ACTIONS
        .iter()
        .find(|action| uses.as_deref() == Some(action.action))
    {
        needs_oidc.push(action.action);
        oidc_action_check(out, (path, job_name), action, step.get("with"));
    }
    let run = step.get("run").and_then(Value::as_str).unwrap_or_default();
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

/// Pushes why an OIDC action does not run in its OIDC mode alone.
fn oidc_action_check(
    out: &mut Vec<String>,
    (path, job_name): (&str, &str),
    action: &OidcAction,
    with: Option<&Value>,
) {
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Input {
    /// An `env:` or `with:` key.
    Key,
    /// Any other key, or a value.
    Value,
}

/// Calls `visit(kind, text)` on every scalar: mapping keys and values.
fn walk(value: &Value, parent: Option<&str>, visit: &mut dyn FnMut(Input, &str)) {
    match value {
        Value::Str(text) => visit(Input::Value, text),
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
                visit(kind, key);
                walk(item, Some(key), visit);
            }
        }
    }
}

fn contains_ignore_case(text: &str, needle: &str) -> bool {
    text.to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

fn mentions_infisical(value: &Value) -> bool {
    let mut found = false;
    walk(value, None, &mut |_, text| {
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
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    env:\n      CLOUDFLARE_API_KEY: ${{ vars.K }}\n    steps:\n      - run: true\n",
            "Cloudflare Global API Key",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: 'curl -H \"X-Auth-Key: $K\" https://api.cloudflare.com/client/v4/zones'\n",
            "`X-Auth-Key`",
        ),
        (
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: echo ${{ secrets['CF_API_KEY'] }}\n",
            "`CF_API_KEY` is the Cloudflare Global API Key",
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
        // A composite action is checked like a job.
        (
            "runs:\n  using: composite\n  steps:\n    - run: ./deploy\n      shell: bash\n      env:\n        AWS_SECRET_ACCESS_KEY: ${{ inputs.key }}\n",
            "static AWS access key",
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
            "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: true\n        env:\n          AWS_ACCESS_KEY_ID: literal-value-42\n",
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
