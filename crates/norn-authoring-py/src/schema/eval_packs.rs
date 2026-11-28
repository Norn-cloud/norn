//! Pack manifest, composition, secret reference, function handle, and deployment
//! constructor evaluators used by [`crate::schema::eval`].

use std::collections::BTreeMap;

use crate::error::{CODE_SCHEMA, PyAuthoringError, SourceSpan};
use crate::schema::env::{EvalValue, ModuleEnv};
use crate::schema::eval_ctors::{expect_seq, expect_str, expect_str_seq};
use crate::schema::packs::{
    CellHostKind, DeploymentSpec, PackCategory, PackConflictPolicy, PackManifest, SecretRef,
    SettingValue, compose_packs,
};
use crate::schema::types::FnRef;

pub(crate) fn eval_fn_ref<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut id = None;
    let mut version = None;
    let mut input_type = None;
    let mut output_type = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => id = Some(v),
            1 => version = Some(v),
            2 => input_type = Some(v),
            3 => output_type = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "fn_ref() takes at most 4 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "id" => id = Some(v),
            "version" => version = Some(v),
            "input_type" => input_type = Some(v),
            "output_type" => output_type = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in fn_ref()"),
                    span.clone(),
                ));
            }
        }
    }
    Ok(EvalValue::FnRef(Box::new(FnRef {
        id: expect_str(
            id.ok_or_else(|| {
                PyAuthoringError::spanned(CODE_SCHEMA, "fn_ref() missing 'id'", span.clone())
            })?,
            "fn_ref.id",
            span,
        )?,
        version: expect_str(
            version.ok_or_else(|| {
                PyAuthoringError::spanned(CODE_SCHEMA, "fn_ref() missing 'version'", span.clone())
            })?,
            "fn_ref.version",
            span,
        )?,
        input_type: expect_str(
            input_type.ok_or_else(|| {
                PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "fn_ref() missing 'input_type'",
                    span.clone(),
                )
            })?,
            "fn_ref.input_type",
            span,
        )?,
        output_type: expect_str(
            output_type.ok_or_else(|| {
                PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "fn_ref() missing 'output_type'",
                    span.clone(),
                )
            })?,
            "fn_ref.output_type",
            span,
        )?,
    })))
}

pub(crate) fn eval_secret_ref<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut name = None;
    let mut description = None;
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => name = Some(v),
            1 => description = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "SecretRef() takes at most 2 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "name" => name = Some(v),
            "description" => description = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in SecretRef()"),
                    span.clone(),
                ));
            }
        }
    }
    let desc = match description {
        None | Some(EvalValue::None) => None,
        Some(other) => Some(expect_str(other, "SecretRef.description", span)?),
    };
    Ok(EvalValue::SecretRef(Box::new(SecretRef {
        name: expect_str(
            name.ok_or_else(|| {
                PyAuthoringError::spanned(CODE_SCHEMA, "SecretRef() missing 'name'", span.clone())
            })?,
            "SecretRef.name",
            span,
        )?,
        description: desc,
    })))
}

#[derive(Default)]
struct PackArgs<'a> {
    id: Option<EvalValue<'a>>,
    version: Option<EvalValue<'a>>,
    category: Option<EvalValue<'a>>,
    title: Option<EvalValue<'a>>,
    requires: Vec<String>,
    entities: Vec<String>,
    laws: Vec<String>,
    markings: Vec<String>,
    functions: Vec<FnRef>,
    description: Option<String>,
}

pub(crate) fn eval_pack_manifest<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    env: &ModuleEnv<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut args = PackArgs::default();
    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => args.id = Some(v),
            1 => args.version = Some(v),
            2 => args.category = Some(v),
            3 => args.title = Some(v),
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "PackManifest() takes at most 4 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "id" => args.id = Some(v),
            "version" => args.version = Some(v),
            "category" => args.category = Some(v),
            "title" => args.title = Some(v),
            "requires" => args.requires = expect_str_seq(v, "PackManifest.requires", span)?,
            "entities" => args.entities = extract_pack_entities(v, env, span)?,
            "laws" => args.laws = extract_pack_laws(v, span)?,
            "markings" => args.markings = extract_pack_markings(v, span)?,
            "functions" => args.functions = extract_pack_functions(v, span)?,
            "description" => {
                args.description = if matches!(v, EvalValue::None) {
                    None
                } else {
                    Some(expect_str(v, "PackManifest.description", span)?)
                };
            }
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in PackManifest()"),
                    span.clone(),
                ));
            }
        }
    }
    finish_pack_manifest(args, span)
}

fn finish_pack_manifest<'a>(
    args: PackArgs<'a>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let id = expect_pack_str(args.id, "PackManifest", "id", span)?;
    let version = expect_pack_str(args.version, "PackManifest", "version", span)?;
    let cat_raw = expect_pack_str(args.category, "PackManifest", "category", span)?;
    let category = PackCategory::parse(&cat_raw, Some(span))?;
    let title = expect_pack_str(args.title, "PackManifest", "title", span)?;
    Ok(EvalValue::PackManifest(Box::new(PackManifest {
        id,
        version,
        category,
        title,
        requires: args.requires,
        entities: args.entities,
        laws: args.laws,
        markings: args.markings,
        functions: args.functions,
        description: args.description,
    })))
}

fn expect_pack_str(
    val: Option<EvalValue<'_>>,
    ctor: &str,
    field: &str,
    span: &SourceSpan,
) -> Result<String, PyAuthoringError> {
    let raw = val.ok_or_else(|| {
        PyAuthoringError::spanned(
            CODE_SCHEMA,
            format!("{ctor}() missing '{field}'"),
            span.clone(),
        )
    })?;
    expect_str(raw, &format!("{ctor}.{field}"), span)
}

fn extract_pack_entities(
    val: EvalValue<'_>,
    env: &ModuleEnv<'_>,
    span: &SourceSpan,
) -> Result<Vec<String>, PyAuthoringError> {
    let mut out = Vec::new();
    for item in expect_seq(val, "PackManifest.entities", span)? {
        if let EvalValue::ConceptClass(py_name) = item
            && let Some(c) = env.concepts.get(&py_name)
        {
            out.push(c.norn_name.clone());
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "PackManifest.entities entries must be Concept subclasses",
                span.clone(),
            ));
        }
    }
    Ok(out)
}

fn extract_pack_laws(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<Vec<String>, PyAuthoringError> {
    let mut out = Vec::new();
    for item in expect_seq(val, "PackManifest.laws", span)? {
        if let EvalValue::LawDecl(id) | EvalValue::Str(id) = item {
            out.push(id);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "PackManifest.laws entries must be LawDecl instances",
                span.clone(),
            ));
        }
    }
    Ok(out)
}

fn extract_pack_markings(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<Vec<String>, PyAuthoringError> {
    let mut out = Vec::new();
    for item in expect_seq(val, "PackManifest.markings", span)? {
        if let EvalValue::MarkingsCatalog(cat) = item {
            out.push(cat.id);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "PackManifest.markings entries must be MarkingsCatalog instances",
                span.clone(),
            ));
        }
    }
    Ok(out)
}

fn extract_pack_functions(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<Vec<FnRef>, PyAuthoringError> {
    let mut out = Vec::new();
    for item in expect_seq(val, "PackManifest.functions", span)? {
        if let EvalValue::FnRef(f) = item {
            out.push(*f);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "PackManifest.functions entries must be FnRef instances",
                span.clone(),
            ));
        }
    }
    Ok(out)
}

pub(crate) fn eval_compose_packs<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut policy = PackConflictPolicy::FailClosed;
    for (k, v) in kwargs {
        if k == "conflict_policy" {
            let raw = expect_str(v, "compose_packs.conflict_policy", span)?;
            policy = PackConflictPolicy::parse(&raw, Some(span))?;
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("unexpected keyword '{k}' in compose_packs()"),
                span.clone(),
            ));
        }
    }
    let mut packs = Vec::with_capacity(pos.len());
    for item in pos {
        if let EvalValue::PackManifest(pack) = item {
            packs.push(*pack);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "compose_packs() arguments must be PackManifest instances",
                span.clone(),
            ));
        }
    }
    let _composed = compose_packs(&packs, policy, Some(span))?;
    Ok(EvalValue::OpaqueObject)
}

pub(crate) fn eval_deployment_spec<'a>(
    pos: Vec<EvalValue<'a>>,
    kwargs: Vec<(String, EvalValue<'a>)>,
    span: &SourceSpan,
) -> Result<EvalValue<'a>, PyAuthoringError> {
    let mut id = None;
    let mut org_id = None;
    let mut environment_id = None;
    let mut packs = Vec::new();
    let mut cell_host = CellHostKind::Rivet;
    let mut worker_endpoints = BTreeMap::new();
    let mut secret_refs = Vec::new();
    let mut initial_settings = BTreeMap::new();

    for (idx, v) in pos.into_iter().enumerate() {
        match idx {
            0 => id = Some(v),
            1 => org_id = Some(v),
            2 => environment_id = Some(v),
            3 => packs = extract_packs(v, span)?,
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    "DeploymentSpec() takes at most 4 positional arguments",
                    span.clone(),
                ));
            }
        }
    }
    for (k, v) in kwargs {
        match k.as_str() {
            "id" => id = Some(v),
            "org_id" => org_id = Some(v),
            "environment_id" => environment_id = Some(v),
            "packs" => packs = extract_packs(v, span)?,
            "cell_host" => {
                cell_host = CellHostKind::parse(
                    &expect_str(v, "DeploymentSpec.cell_host", span)?,
                    Some(span),
                )?;
            }
            "worker_endpoints" => worker_endpoints = extract_endpoints(v, span)?,
            "secret_refs" => secret_refs = extract_secret_refs(v, span)?,
            "initial_settings" => initial_settings = extract_initial_settings(v, span)?,
            _ => {
                return Err(PyAuthoringError::spanned(
                    CODE_SCHEMA,
                    format!("unexpected keyword '{k}' in DeploymentSpec()"),
                    span.clone(),
                ));
            }
        }
    }

    let spec = DeploymentSpec {
        id: expect_pack_str(id, "DeploymentSpec", "id", span)?,
        org_id: expect_pack_str(org_id, "DeploymentSpec", "org_id", span)?,
        environment_id: expect_pack_str(environment_id, "DeploymentSpec", "environment_id", span)?,
        packs,
        cell_host,
        worker_endpoints,
        secret_refs,
        initial_settings,
    };
    let _validated = spec.validate(Some(span))?;
    Ok(EvalValue::DeploymentSpec)
}

fn extract_packs(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<Vec<PackManifest>, PyAuthoringError> {
    let items = expect_seq(val, "DeploymentSpec.packs", span)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let EvalValue::PackManifest(p) = item {
            out.push(*p);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "DeploymentSpec.packs entries must be PackManifest instances",
                span.clone(),
            ));
        }
    }
    Ok(out)
}

fn extract_endpoints(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<BTreeMap<String, String>, PyAuthoringError> {
    let EvalValue::Dict(map) = val else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "DeploymentSpec.worker_endpoints must be a dict",
            span.clone(),
        ));
    };
    let mut out = BTreeMap::new();
    for (k, v) in map {
        out.insert(
            k,
            expect_str(v, "DeploymentSpec.worker_endpoints value", span)?,
        );
    }
    Ok(out)
}

fn extract_secret_refs(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<Vec<SecretRef>, PyAuthoringError> {
    let items = expect_seq(val, "DeploymentSpec.secret_refs", span)?;
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if let EvalValue::SecretRef(sr) = item {
            out.push(*sr);
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                "DeploymentSpec.secret_refs entries must be SecretRef instances",
                span.clone(),
            ));
        }
    }
    Ok(out)
}

fn extract_initial_settings(
    val: EvalValue<'_>,
    span: &SourceSpan,
) -> Result<BTreeMap<String, SettingValue>, PyAuthoringError> {
    let EvalValue::Dict(map) = val else {
        return Err(PyAuthoringError::spanned(
            CODE_SCHEMA,
            "DeploymentSpec.initial_settings must be a dict",
            span.clone(),
        ));
    };
    let mut out = BTreeMap::new();
    for (k, v) in map {
        let setting = if let EvalValue::Str(s) = v {
            SettingValue::Str(s)
        } else if let EvalValue::Int(n) = v {
            SettingValue::Int(n)
        } else if let EvalValue::Bool(b) = v {
            SettingValue::Bool(b)
        } else if let EvalValue::Decimal(d) = v {
            SettingValue::Decimal(d)
        } else {
            return Err(PyAuthoringError::spanned(
                CODE_SCHEMA,
                format!("DeploymentSpec.initial_settings['{k}'] has unsupported type"),
                span.clone(),
            ));
        };
        out.insert(k, setting);
    }
    Ok(out)
}
