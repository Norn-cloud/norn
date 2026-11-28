//! Manifest parsing and FD-confined path helpers for `norn compile --pack-json`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::AuthoringFailure;
use super::fs_io::display_posix_path;
use super::ts::normalize_lexical_relative;

pub(super) const MAX_MODULES_MANIFEST_BYTES: usize = 4 * 1024 * 1024;

pub(super) struct ModuleSpec {
    pub(super) path: PathBuf,
    pub(super) scope: Option<BTreeMap<String, String>>,
    pub(super) source_path: Option<String>,
    pub(super) registry_package_path: Option<String>,
}

pub(super) fn normalize_abs(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else if let Ok(cwd) = std::env::current_dir() {
        cwd.join(path)
    } else {
        path.to_path_buf()
    };
    let mut out = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

pub(super) fn relative_pathbuf(base: &Path, target: &Path) -> PathBuf {
    let abs_base = normalize_abs(base);
    let abs_target = normalize_abs(target);
    let base_parts: Vec<Component<'_>> = abs_base.components().collect();
    let target_parts: Vec<Component<'_>> = abs_target.components().collect();
    let common = base_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut out = PathBuf::new();
    for _step in common..base_parts.len() {
        out.push("..");
    }
    if let Some(rest) = target_parts.get(common..) {
        for comp in rest {
            out.push(comp.as_os_str());
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

pub(super) fn display_relative(root: Option<&Path>, target: &Path) -> String {
    root.map_or_else(
        || display_posix_path(target),
        |base| display_posix_path(&relative_pathbuf(base, target)),
    )
}

fn parse_optional_string(
    obj: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, AuthoringFailure> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(AuthoringFailure::Io(format!(
            "invalid --modules manifest: `{key}` must be a string"
        ))),
    }
}

pub(super) fn parse_module_specs(raw: &str) -> Result<Vec<ModuleSpec>, AuthoringFailure> {
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|err| AuthoringFailure::Io(format!("invalid --modules manifest: {err}")))?;
    let Some(items) = parsed.as_array() else {
        return Err(AuthoringFailure::Io(
            "invalid --modules manifest: expected a JSON array".to_owned(),
        ));
    };
    let mut specs = Vec::with_capacity(items.len());
    for item in items {
        let Some(obj) = item.as_object() else {
            return Err(AuthoringFailure::Io(
                "invalid --modules manifest: each entry must be an object".to_owned(),
            ));
        };
        for key in obj.keys() {
            if !matches!(
                key.as_str(),
                "path" | "scope" | "sourcePath" | "registryPackagePath"
            ) {
                return Err(AuthoringFailure::Io(format!(
                    "invalid --modules manifest: unknown field `{key}`"
                )));
            }
        }
        let Some(path_str) = obj
            .get("path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return Err(AuthoringFailure::Io(
                "invalid --modules manifest: each entry requires a non-empty `path` string"
                    .to_owned(),
            ));
        };
        let scope = match obj.get("scope") {
            None | Some(Value::Null) => None,
            Some(Value::Object(map)) => {
                let mut out = BTreeMap::new();
                for (k, v) in map {
                    let Some(val) = v.as_str() else {
                        return Err(AuthoringFailure::Io(format!(
                            "invalid --modules manifest: `scope.{k}` must be a string"
                        )));
                    };
                    out.insert(k.clone(), val.to_owned());
                }
                Some(out)
            }
            Some(_) => {
                return Err(AuthoringFailure::Io(
                    "invalid --modules manifest: `scope` must be an object".to_owned(),
                ));
            }
        };
        specs.push(ModuleSpec {
            path: PathBuf::from(path_str),
            scope,
            source_path: parse_optional_string(obj, "sourcePath")?,
            registry_package_path: parse_optional_string(obj, "registryPackagePath")?,
        });
    }
    Ok(specs)
}

pub(super) fn resolve_module_rel_inside_base(
    base_dir: &Path,
    spec_path: &Path,
) -> Result<PathBuf, AuthoringFailure> {
    let abs_base = normalize_abs(base_dir);
    let candidate = if spec_path.is_absolute() {
        spec_path.to_path_buf()
    } else {
        base_dir.join(spec_path)
    };
    let abs_target = normalize_abs(&candidate);
    let Ok(stripped) = abs_target.strip_prefix(&abs_base) else {
        return Err(AuthoringFailure::Io(format!(
            "{}: module path must stay inside target directory {}",
            spec_path.display(),
            base_dir.display()
        )));
    };
    let norm = normalize_lexical_relative(stripped);
    if norm.as_os_str().is_empty()
        || !norm
            .components()
            .all(|comp| matches!(comp, Component::Normal(_)))
    {
        return Err(AuthoringFailure::Io(format!(
            "{}: module path must point to a file inside target directory {}",
            spec_path.display(),
            base_dir.display()
        )));
    }
    Ok(norm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[test]
    fn module_paths_escaping_base_are_refused() {
        let base = Path::new("/tmp/norn-pack-base");
        assert!(resolve_module_rel_inside_base(base, Path::new("/tmp/other/a.ts")).is_err());
        assert!(resolve_module_rel_inside_base(base, Path::new("../outside.ts")).is_err());
        assert!(resolve_module_rel_inside_base(base, Path::new(".")).is_err());
        assert_eq!(
            resolve_module_rel_inside_base(base, Path::new("/tmp/norn-pack-base/sub/../a.ts")).ok(),
            Some(PathBuf::from("a.ts"))
        );
    }

    #[test]
    fn pack_json_refuses_duplicate_normalized_module_roots() {
        let dir = std::env::temp_dir().join(format!(
            "norn-pack-dup-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::create_dir_all(&dir);
        let mod_a = dir.join("a.ts");
        let mod_b = dir.join("b.ts");
        let _ = std::fs::write(&mod_a, "export const a = 1;\n");
        let _ = std::fs::write(&mod_b, "export const b = 2;\n");
        let manifest = dir.join("modules.json");
        let _ = std::fs::write(
            &manifest,
            serde_json::json!([
                { "path": dir.join("a.ts") },
                { "path": dir.join("./a.ts") },
                { "path": dir.join("b.ts") },
            ])
            .to_string(),
        );
        let out = super::super::pack_json::execute_compile_pack_json(&super::super::CompileArgs {
            path: dir.clone(),
            org: None,
            env: None,
            forbid_unsafe_zen: false,
            out: None,
            tool_root: None,
            pack_json: true,
            registry: None,
            root: None,
            source_path: None,
            modules: Some(manifest),
            allow_empty: true,
            max_expansion_work: None,
        });
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(out.exit_code, 2);
        assert!(
            out.stderr.contains("duplicate module root"),
            "expected duplicate module root refusal, got: {}",
            out.stderr
        );
    }

    #[test]
    fn clap_enforces_pack_json_and_scope_constraints() {
        assert!(
            crate::Cli::try_parse_from(["norn", "compile", "foo.ts", "--allow-empty"]).is_err()
        );
        assert!(
            crate::Cli::try_parse_from([
                "norn",
                "compile",
                "foo.ts",
                "--pack-json",
                "--registry",
                "registry/demo",
                "--org",
                "acme",
                "--env",
                "dev",
            ])
            .is_err()
        );
        assert!(
            crate::Cli::try_parse_from([
                "norn", "compile", "foo.ts", "--org", "acme", "--env", "dev"
            ])
            .is_ok()
        );
        assert!(
            crate::Cli::try_parse_from([
                "norn",
                "compile",
                "foo.ts",
                "--pack-json",
                "--registry",
                "registry/demo",
            ])
            .is_ok()
        );
    }
}
