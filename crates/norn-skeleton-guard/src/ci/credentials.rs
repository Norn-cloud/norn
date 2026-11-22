//! Workflows hold no long-lived credential (owner, 2026-11-22).
//!
//! The rules are this module's typed tables and nothing else; there is no
//! per-file exemption. A workflow may read `secrets.GITHUB_TOKEN` (minted per
//! run) and no other secret: a provider with an OIDC path is reached over
//! OIDC, and a credential without one (a Cloudflare API token, an R2 key) is
//! read from Infisical over OIDC inside the job, never stored in GitHub.
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
}

/// Actions that must run in their OIDC mode (their defaults are not).
pub const OIDC_ACTIONS: &[OidcAction] = &[
    OidcAction {
        action: "infisical/secrets-action",
        input: "method",
        value: Some("oidc"),
    },
    OidcAction {
        action: "aws-actions/configure-aws-credentials",
        input: "role-to-assume",
        value: None,
    },
    OidcAction {
        action: "google-github-actions/auth",
        input: "workload_identity_provider",
        value: None,
    },
];

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
    let workflow_permissions = document.get("permissions");
    for (job_name, job) in document.get("jobs").map(Value::entries).unwrap_or_default() {
        if job.get("secrets").and_then(Value::as_str) == Some("inherit") {
            out.push(format!(
                "{path}::{job_name}: `secrets: inherit` hands every stored secret to a called workflow"
            ));
        }
        let mut needs_oidc = Vec::new();
        if mentions_infisical(job) || document.get("env").is_some_and(mentions_infisical) {
            needs_oidc.push("talks to Infisical");
        }
        for step in job.get("steps").map(Value::items).unwrap_or_default() {
            let uses = step
                .get("uses")
                .and_then(Value::as_str)
                .map(|uses| uses.split('@').next().unwrap_or(uses).to_ascii_lowercase());
            if let Some(action) = OIDC_ACTIONS
                .iter()
                .find(|action| uses.as_deref() == Some(action.action))
            {
                needs_oidc.push(action.action);
                let input = step.get("with").and_then(|with| with.get(action.input));
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
            }
            for line in step
                .get("run")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .lines()
            {
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
        let permissions = job.get("permissions").or(workflow_permissions);
        if let Some(reason) = needs_oidc.first()
            && !grants_id_token(permissions)
        {
            out.push(format!(
                "{path}::{job_name}: {reason} without `permissions: id-token: write`; it must authenticate over OIDC"
            ));
        }
    }
    out
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
        text.split("${{")
            .skip(1)
            .map(|part| part.split("}}").next().unwrap_or(part))
            .collect()
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
    ];

    /// Each fixture must pass.
    const MUST_PASS: &[&str] = &[
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/github-script@v9\n        with:\n          github-token: ${{ secrets.GITHUB_TOKEN }}\n",
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    permissions:\n      id-token: write\n      contents: read\n    steps:\n      - uses: Infisical/secrets-action@v1\n        with:\n          method: oidc\n          identity-id: ${{ vars.INFISICAL_IDENTITY_ID }}\n",
        "permissions: write-all\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - run: infisical login --method=oidc-auth --machine-identity-id x --oidc-jwt \"$JWT\"\n",
        "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: aws-actions/configure-aws-credentials@v4\n        with:\n          role-to-assume: arn:aws:iam::1:role/ci\n          aws-region: eu-west-1\n",
        "permissions:\n  id-token: write\njobs:\n  a:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: google-github-actions/auth@v2\n        with:\n          workload_identity_provider: projects/1/locations/global/workloadIdentityPools/p/providers/g\n",
        // An S3-compatible client reads Infisical-held keys through AWS_* in its own script.
        "jobs:\n  a:\n    runs-on: ubuntu-24.04\n    permissions:\n      id-token: write\n    steps:\n      - run: export AWS_ACCESS_KEY_ID=\"$KEY\"\n        env:\n          KEY: ${{ steps.infisical.outputs.key_id }}\n",
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
    fn oidc_and_the_run_token_pass() {
        for source in MUST_PASS {
            assert_eq!(check(source), Vec::<String>::new(), "{source}");
        }
    }
}
