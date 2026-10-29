//! The object model a store is opened with: the object types, their
//! projected tables (`norn-projections` T1-PROJ-2), and the lookups the
//! store needs (supertype chains, column owners, defaults, lifecycles).

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::{
    EffectiveLink, EffectiveObjectType, EntityContext, EntityModel, effective_object_types,
};
use norn_ir::{
    InterfaceDef, Lifecycle, Link, ObjectTypeDef, Property, PropertyKind, PropertyTypeDef,
};
use norn_projections::sqlite_ddl::{
    ID_COLUMN, SCOPE_COLUMN, TYPE_COLUMN, WORLD_COLUMN, quote_identifier,
};
use norn_projections::sqlite_ddl::{LinkTable, ObjectTable, PropertyColumn, SqliteDdlInput};
use norn_projections::{SqliteSchema, project_sqlite_ddl};
use serde::{Deserialize, Serialize};

use crate::codec::hash_json;
use crate::error::CellError;

/// One object type with its supertype chain resolved.
#[derive(Debug, Clone)]
struct TypeEntry {
    def: ObjectTypeDef,
    /// Root first, this type last.
    chain: Vec<String>,
}

fn canonical_binding_indexes(indexes: &[BindingIndexSpec]) -> Vec<BindingIndexSpec> {
    let mut merged = BTreeMap::<(String, String), (bool, bool)>::new();
    for index in indexes {
        let flags = merged
            .entry((index.object_type.clone(), index.property_id.clone()))
            .or_default();
        flags.0 |= index.sort;
        flags.1 |= index.filter;
    }
    merged
        .into_iter()
        .map(
            |((object_type, property_id), (sort, filter))| BindingIndexSpec {
                object_type,
                property_id,
                sort,
                filter,
            },
        )
        .collect()
}

fn binding_property_index_name(table: &str, property: &str, direction: &str) -> String {
    format!("{table}/binding-property/{property}/{direction}")
}

fn binding_property_index_statement(
    table: &str,
    property: &str,
    descending: bool,
    root_owned: bool,
) -> String {
    let direction = if descending { "DESC" } else { "ASC" };
    let mut columns = vec![
        quote_identifier(WORLD_COLUMN),
        format!("{} {direction}", quote_identifier(property)),
        format!("{} ASC", quote_identifier(ID_COLUMN)),
    ];
    if root_owned {
        columns.push(quote_identifier(TYPE_COLUMN));
    }
    columns.push(quote_identifier(SCOPE_COLUMN));
    format!(
        "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
        quote_identifier(&binding_property_index_name(
            table,
            property,
            if descending { "desc" } else { "asc" },
        )),
        quote_identifier(table),
        columns.join(", "),
    )
}

fn append_binding_indexes(
    schema: &mut SqliteSchema,
    types: &BTreeMap<String, TypeEntry>,
    indexes: &[BindingIndexSpec],
) -> Result<(), CellError> {
    let mut created = BTreeSet::new();
    for index in indexes {
        if !index.sort && !index.filter {
            continue;
        }
        let entry = types
            .get(&index.object_type)
            .ok_or_else(|| CellError::UnknownObjectType(index.object_type.clone()))?;
        let root = entry
            .chain
            .first()
            .ok_or_else(|| CellError::UnknownObjectType(index.object_type.clone()))?;
        let mut owner = None;
        for owner_name in entry.chain.iter().rev() {
            if let Some(table) = schema
                .object_tables
                .iter()
                .find(|table| table.object_type.as_str() == owner_name)
                && let Some(column) = table
                    .columns
                    .iter()
                    .find(|column| column.property.as_str() == index.property_id)
            {
                owner = Some((
                    owner_name.as_str(),
                    table.table.clone(),
                    column.column.clone(),
                    column.kind,
                ));
                break;
            }
        }
        let Some((owner_name, table_name, column_name, kind)) = owner else {
            return Err(CellError::UnknownProperty {
                object_type: index.object_type.clone(),
                property: index.property_id.clone(),
            });
        };
        if column_name.is_none() {
            return Err(CellError::OutOfRange(
                "binding property index requires a physical column",
            ));
        }
        if index.sort
            && (owner_name != root
                || !matches!(
                    kind,
                    PropertyKind::Text
                        | PropertyKind::Enum
                        | PropertyKind::Integer
                        | PropertyKind::Boolean
                        | PropertyKind::Date
                ))
        {
            return Err(CellError::OutOfRange("binding sort property index"));
        }

        // Ascending order is shared by filter lookup and ascending sort.
        let owner_is_root = owner_name == root;
        let asc_key = (table_name.clone(), index.property_id.clone(), false);
        if (index.filter || index.sort) && created.insert(asc_key) {
            let statement = binding_property_index_statement(
                &table_name,
                &index.property_id,
                false,
                owner_is_root,
            );
            let table = schema
                .object_tables
                .iter_mut()
                .find(|table| table.table == table_name)
                .ok_or_else(|| CellError::UnknownObjectType(index.object_type.clone()))?;
            table.statements.push(statement);
        }
        if index.sort {
            let desc_key = (table_name.clone(), index.property_id.clone(), true);
            if created.insert(desc_key) {
                let statement = binding_property_index_statement(
                    &table_name,
                    &index.property_id,
                    true,
                    owner_is_root,
                );
                let table = schema
                    .object_tables
                    .iter_mut()
                    .find(|table| table.table == table_name)
                    .ok_or_else(|| CellError::UnknownObjectType(index.object_type.clone()))?;
                table.statements.push(statement);
            }
        }
    }
    Ok(())
}

/// Hash object/property definitions and invariant-bearing interfaces, including
/// semantics absent from projected DDL.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplayDefinitions<'a> {
    object_types: BTreeMap<&'a str, &'a ObjectTypeDef>,
    property_types: BTreeMap<&'a str, &'a PropertyTypeDef>,
    /// Only interfaces with invariants affect the model hash. Omitting this
    /// field when empty preserves legacy object/property-only hashes while all
    /// interfaces remain available to commit admission and snapshots.
    #[serde(skip_serializing_if = "Option::is_none")]
    interfaces: Option<BTreeMap<&'a str, &'a InterfaceDef>>,
    /// Query access paths projected from admitted widget bindings. Omitting
    /// this field preserves hashes for snapshots that predate binding indexes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    binding_indexes: Vec<BindingIndexSpec>,
}

/// A direct property used by an admitted list binding's sort or filter.
///
/// The cell projects indexes for these properties into the owning object
/// table. Sort indexes require a root-owned property; filter indexes follow
/// the property declaration's physical table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingIndexSpec {
    /// Object type named by the binding.
    pub object_type: String,
    /// Direct, stored property id.
    pub property_id: String,
    /// Whether the binding sorts by this property.
    pub sort: bool,
    /// Whether the binding filters by this property.
    pub filter: bool,
}

/// Where a stored property lives.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ColumnRef<'a> {
    /// The table that holds it.
    pub(crate) table: &'a ObjectTable,
    /// Its column (or overflow placement).
    pub(crate) column: &'a PropertyColumn,
}

/// The projected object model of one store.
#[derive(Debug, Clone)]
pub struct Model {
    schema: SqliteSchema,
    types: BTreeMap<String, TypeEntry>,
    property_types: Vec<PropertyTypeDef>,
    interfaces: Vec<InterfaceDef>,
    binding_indexes: Vec<BindingIndexSpec>,
    /// Checker-derived effective property contracts, cached in owned form so
    /// store reads can normalize values without rebuilding or borrowing a
    /// self-referential catalog.
    effective_objects: BTreeMap<String, EffectiveObjectType>,
    effective_links: BTreeMap<String, Vec<EffectiveLink>>,
    ddl_hash: String,
    definitions_hash: String,
}

impl Model {
    /// Project `input` and index it.
    ///
    /// # Errors
    ///
    /// [`CellError::Projection`] when the types do not project.
    pub fn new(input: &SqliteDdlInput<'_>) -> Result<Self, CellError> {
        Self::new_with_interfaces(input, &[])
    }

    fn new_with_interfaces(
        input: &SqliteDdlInput<'_>,
        interfaces: &[InterfaceDef],
    ) -> Result<Self, CellError> {
        Self::new_with_binding_indexes(input, interfaces, &[])
    }

    fn new_with_binding_indexes(
        input: &SqliteDdlInput<'_>,
        interfaces: &[InterfaceDef],
        binding_indexes: &[BindingIndexSpec],
    ) -> Result<Self, CellError> {
        let mut schema = project_sqlite_ddl(input)?;
        let defs: BTreeMap<String, &ObjectTypeDef> = input
            .object_types
            .iter()
            .map(|def| (def.name.as_str().to_owned(), def))
            .collect();
        let mut types = BTreeMap::new();
        for (name, def) in &defs {
            // The projection already validated every chain.
            let mut chain = vec![name.clone()];
            let mut current = *def;
            while let Some(parent) = &current.extends {
                chain.push(parent.as_str().to_owned());
                match defs.get(parent.as_str()) {
                    Some(parent_def) => current = *parent_def,
                    None => break,
                }
            }
            chain.reverse();
            types.insert(
                name.clone(),
                TypeEntry {
                    def: (*def).clone(),
                    chain,
                },
            );
        }
        let binding_indexes = canonical_binding_indexes(binding_indexes);
        append_binding_indexes(&mut schema, &types, &binding_indexes)?;
        let ddl_hash = hash_json(&schema.to_sql())?;
        let definitions_hash = hash_json(&ReplayDefinitions {
            object_types: defs
                .iter()
                .map(|(name, def)| (name.as_str(), *def))
                .collect(),
            property_types: input
                .property_types
                .iter()
                .map(|def| (def.name.as_str(), def))
                .collect(),
            interfaces: {
                let invariant_interfaces: BTreeMap<_, _> = interfaces
                    .iter()
                    .filter(|def| !def.invariants.is_empty())
                    .map(|def| (def.name.as_str(), def))
                    .collect();
                (!invariant_interfaces.is_empty()).then_some(invariant_interfaces)
            },
            binding_indexes: binding_indexes.clone(),
        })?;
        let registry = std::collections::BTreeSet::new();
        let entity_model = EntityModel {
            interfaces,
            object_types: input.object_types,
            extensions: &[],
        };
        let entity_context = EntityContext {
            catalog: input.property_types,
            registry: &registry,
            base: EntityModel::EMPTY,
            tenant: None,
        };
        let (effective_objects, effective_links) =
            effective_object_types(&entity_model, &entity_context)
                .into_values()
                .fold(
                    (BTreeMap::new(), BTreeMap::new()),
                    |(mut objects, mut links), (object_type, object_links)| {
                        let name = object_type.name.as_str().to_owned();
                        objects.insert(name.clone(), object_type);
                        links.insert(name, object_links);
                        (objects, links)
                    },
                );
        Ok(Self {
            schema,
            types,
            property_types: input.property_types.to_vec(),
            interfaces: interfaces.to_vec(),
            binding_indexes,
            effective_objects,
            effective_links,
            ddl_hash,
            definitions_hash,
        })
    }

    /// Project `object_types` with the shared `property_types` their `ref`s
    /// name (see [`Self::new`]).
    ///
    /// # Errors
    ///
    /// [`CellError::Projection`] when the types do not project.
    pub fn from_definitions(
        object_types: &[ObjectTypeDef],
        property_types: &[norn_ir::PropertyTypeDef],
    ) -> Result<Self, CellError> {
        Self::from_definitions_with_interfaces(object_types, property_types, &[])
    }

    /// Project object and shared property definitions while retaining the
    /// admitted interfaces used by kernel commit admission. Interfaces are
    /// semantic model data and do not change the projected SQLite schema.
    ///
    /// # Errors
    ///
    /// [`CellError::Projection`] when the object and property definitions do
    /// not project.
    pub fn from_definitions_with_interfaces(
        object_types: &[ObjectTypeDef],
        property_types: &[PropertyTypeDef],
        interfaces: &[InterfaceDef],
    ) -> Result<Self, CellError> {
        Self::new_with_interfaces(
            &SqliteDdlInput {
                object_types,
                property_types,
            },
            interfaces,
        )
    }

    /// Project entity definitions and their declared binding query access
    /// paths. The binding specs are retained in snapshots so restores rebuild
    /// the same DDL and hashes.
    ///
    /// # Errors
    /// A binding index names an unknown, computed, overflow, or unsupported
    /// property; a sort property is not owned by the hierarchy root; or the
    /// entity definitions do not project.
    pub fn from_definitions_with_binding_indexes(
        object_types: &[ObjectTypeDef],
        property_types: &[PropertyTypeDef],
        interfaces: &[InterfaceDef],
        binding_indexes: &[BindingIndexSpec],
    ) -> Result<Self, CellError> {
        Self::new_with_binding_indexes(
            &SqliteDdlInput {
                object_types,
                property_types,
            },
            interfaces,
            binding_indexes,
        )
    }

    /// The projected schema.
    #[must_use]
    pub const fn schema(&self) -> &SqliteSchema {
        &self.schema
    }

    /// Norn1 hash of the projected DDL; recorded in the file and checked on
    /// reopen.
    #[must_use]
    pub fn ddl_hash(&self) -> &str {
        &self.ddl_hash
    }

    /// Norn1 hash of object, property-type, and invariant-bearing interface
    /// definitions, indexed by name. Interfaces without invariants remain
    /// available through [`Self::interfaces`] and snapshots but are omitted
    /// from the hash to preserve legacy model identity. Includes semantics
    /// absent from projected DDL. Definition-list order is irrelevant; order
    /// within definitions is retained.
    #[must_use]
    pub fn model_hash(&self) -> &str {
        &self.definitions_hash
    }

    /// Whether `object_type` is in the model.
    #[must_use]
    pub fn has_type(&self, object_type: &str) -> bool {
        self.types.contains_key(object_type)
    }

    fn entry(&self, object_type: &str) -> Result<&TypeEntry, CellError> {
        self.types
            .get(object_type)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))
    }

    /// The definition of `object_type`.
    pub(crate) fn def(&self, object_type: &str) -> Result<&ObjectTypeDef, CellError> {
        Ok(&self.entry(object_type)?.def)
    }

    /// Every object type of the model (#3018: the kernel's `TypeCatalog`
    /// over this store is built from them).
    pub fn object_types(&self) -> impl Iterator<Item = &ObjectTypeDef> {
        self.types.values().map(|entry| &entry.def)
    }

    /// The shared property types used to resolve exact runtime defaults.
    #[must_use]
    pub fn property_types(&self) -> &[PropertyTypeDef] {
        &self.property_types
    }

    /// The interface definitions retained with this model for shared kernel
    /// admission.
    pub fn interfaces(&self) -> impl Iterator<Item = &InterfaceDef> {
        self.interfaces.iter()
    }

    /// Effective declared markings for every property of `object_type`.
    ///
    /// Mapping provenance is persisted separately by the writer. This is the
    /// model-owned declaration floor used to classify each new fact version.
    pub(crate) fn object_markings(&self, object_type: &str) -> Result<BTreeSet<String>, CellError> {
        let object = self
            .effective_objects
            .get(object_type)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))?;
        Ok(object
            .markings
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect())
    }

    pub(crate) fn definition_markings(
        &self,
        object_type: &str,
    ) -> Result<BTreeMap<String, Vec<String>>, CellError> {
        let mut markings = self.effective_property_markings(object_type)?;
        markings.insert(
            crate::system::MARKING_OCCURRENCE.to_owned(),
            self.object_markings(object_type)?.into_iter().collect(),
        );
        Ok(markings)
    }

    pub(crate) fn effective_property_markings(
        &self,
        object_type: &str,
    ) -> Result<BTreeMap<String, Vec<String>>, CellError> {
        let object = self
            .effective_objects
            .get(object_type)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))?;
        Ok(object
            .properties
            .iter()
            .map(|property| {
                (
                    property.id.as_str().to_owned(),
                    property
                        .markings
                        .iter()
                        .map(|marking| marking.as_str().to_owned())
                        .collect(),
                )
            })
            .collect())
    }

    /// Direct property dependencies used to close runtime markings. Computed
    /// fields use checker-extracted AST reads; naming-series key components
    /// depend on their declared scope fields. `reads_all` is a conservative
    /// whole-object fallback for dynamic selectors.
    pub(crate) fn computed_property_inputs(
        &self,
        object_type: &str,
    ) -> Result<BTreeMap<String, (Vec<String>, bool)>, CellError> {
        let object = self
            .effective_objects
            .get(object_type)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))?;
        let mut dependencies = object
            .properties
            .iter()
            .filter(|property| property.computed)
            .map(|property| {
                (
                    property.id.as_str().to_owned(),
                    (
                        property.computed_inputs.iter().cloned().collect(),
                        property.computed_reads_all,
                    ),
                )
            })
            .collect::<BTreeMap<String, (BTreeSet<String>, bool)>>();
        for key in &object.keys {
            let Some(series) = &key.naming_series else {
                continue;
            };
            let [generated] = key.components.as_slice() else {
                continue;
            };
            dependencies
                .entry(generated.as_str().to_owned())
                .or_default()
                .0
                .extend(
                    series
                        .scope_fields
                        .iter()
                        .map(|field| field.as_str().to_owned()),
                );
        }
        Ok(dependencies
            .into_iter()
            .map(|(property, (inputs, reads_all))| {
                (property, (inputs.into_iter().collect(), reads_all))
            })
            .collect())
    }

    /// Close a trusted classification snapshot over checker-owned value dependencies.
    pub(crate) fn close_computed_markings(
        &self,
        object_type: &str,
        effective: &mut BTreeMap<String, BTreeSet<String>>,
    ) -> Result<BTreeSet<String>, CellError> {
        let dependencies = self.computed_property_inputs(object_type)?;
        let mut changed_properties = BTreeSet::new();
        let max_rounds = dependencies.len().saturating_add(1);
        for _ in 0..max_rounds {
            let previous = effective.clone();
            let mut changed = false;
            for (property, (inputs, reads_all)) in &dependencies {
                let source_properties = if *reads_all {
                    previous.keys().cloned().collect::<BTreeSet<_>>()
                } else {
                    inputs.iter().cloned().collect()
                };
                let mut inherited = BTreeSet::new();
                for input in source_properties {
                    if let Some(labels) = previous.get(&input) {
                        inherited.extend(labels.iter().cloned());
                    } else {
                        inherited.insert("norn.unknown-lineage".to_owned());
                    }
                }
                if inherited.is_empty() {
                    continue;
                }
                let target = effective.entry(property.clone()).or_default();
                let before = target.len();
                target.extend(inherited);
                if target.len() != before {
                    changed = true;
                    changed_properties.insert(property.clone());
                }
            }
            if !changed {
                break;
            }
        }
        Ok(changed_properties)
    }

    /// Return the static classification floor for a naming counter's exact
    /// generated-key attribution. Descendants that share the declaring key
    /// bucket contribute their effective generated and scope-property labels,
    /// so a counter never understates a more strongly classified subtype.
    pub(crate) fn naming_counter_markings(
        &self,
        object_type: &str,
        key_name: &str,
        generated_property: &str,
        declaration: &norn_ir::NamingSeriesIR,
    ) -> Option<BTreeSet<String>> {
        let mut matched = false;
        let mut markings = BTreeSet::new();
        for object in self.effective_objects.values() {
            for key in &object.keys {
                let [component] = key.components.as_slice() else {
                    continue;
                };
                if key.declared_by.as_str() != object_type
                    || key.name.as_str() != key_name
                    || component.as_str() != generated_property
                    || key.naming_series.as_ref() != Some(declaration)
                {
                    continue;
                }
                matched = true;
                for property_id in std::iter::once(generated_property).chain(
                    declaration
                        .scope_fields
                        .iter()
                        .map(norn_ir::PropertyId::as_str),
                ) {
                    let property = object
                        .properties
                        .iter()
                        .find(|property| property.id.as_str() == property_id)?;
                    markings.extend(
                        property
                            .markings
                            .iter()
                            .map(|marking| marking.as_str().to_owned()),
                    );
                }
            }
        }
        matched.then_some(markings)
    }

    /// Every declared identity-key component that can contribute to a public
    /// object id. Callers hide the resource when any such property's marking
    /// cannot be revealed; ids are not reversible projections.
    pub(crate) fn identity_properties(
        &self,
        object_type: &str,
    ) -> Result<std::collections::BTreeSet<String>, CellError> {
        let object = self
            .effective_objects
            .get(object_type)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))?;
        Ok(object
            .keys
            .iter()
            .flat_map(|key| {
                key.components
                    .iter()
                    .map(|property| property.as_str().to_owned())
            })
            .collect())
    }

    /// The typed entity definitions needed to reconstruct this projection.
    #[must_use]
    pub fn definitions_snapshot(&self) -> ModelDefinitionSnapshot {
        ModelDefinitionSnapshot {
            object_types: self.object_types().cloned().collect(),
            property_types: self.property_types.clone(),
            interfaces: self.interfaces.clone(),
            binding_indexes: self.binding_indexes.clone(),
        }
    }

    /// Rebuild a model from an exported typed snapshot.
    ///
    /// # Errors
    ///
    /// [`CellError::Projection`] when the snapshot does not project.
    pub fn from_snapshot(snapshot: &ModelDefinitionSnapshot) -> Result<Self, CellError> {
        Self::from_definitions_with_binding_indexes(
            &snapshot.object_types,
            &snapshot.property_types,
            &snapshot.interfaces,
            &snapshot.binding_indexes,
        )
    }

    /// The projected sort-index name for a root-owned direct stored property.
    pub(crate) fn binding_sort_index_name(
        &self,
        object_type: &str,
        property_id: &str,
        descending: bool,
    ) -> Result<String, CellError> {
        if !self.binding_indexes.iter().any(|index| {
            index.object_type == object_type && index.property_id == property_id && index.sort
        }) {
            return Err(CellError::OutOfRange(
                "binding sort property is not indexed",
            ));
        }
        let chain = self.chain(object_type)?;
        let root = chain
            .first()
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))?;
        let column = self.column(root, property_id)?;
        if column.table.object_type.as_str() != root
            || column.column.column.is_none()
            || !matches!(
                column.column.kind,
                PropertyKind::Text
                    | PropertyKind::Enum
                    | PropertyKind::Integer
                    | PropertyKind::Boolean
                    | PropertyKind::Date
            )
        {
            return Err(CellError::OutOfRange("binding sort property index"));
        }
        Ok(binding_property_index_name(
            &column.table.table,
            property_id,
            if descending { "desc" } else { "asc" },
        ))
    }

    /// The supertype chain of `object_type`, root first, itself last.
    pub(crate) fn chain(&self, object_type: &str) -> Result<&[String], CellError> {
        Ok(&self.entry(object_type)?.chain)
    }

    /// Whether the admitted `object_type` is `ancestor` or a subtype of it.
    ///
    /// This schema query includes the type itself and transitive object-type
    /// inheritance. It returns `false` if either name is absent from the model,
    /// including when both names are the same unknown type. Read projections
    /// can use it to match declared type ranges against concrete record types;
    /// it does not check record existence, scope, or authorization.
    #[must_use]
    pub fn is_a(&self, object_type: &str, ancestor: &str) -> bool {
        self.types
            .get(object_type)
            .is_some_and(|entry| entry.chain.iter().any(|name| name.as_str() == ancestor))
    }

    /// The table of `object_type`.
    pub(crate) fn table(&self, object_type: &str) -> Result<&ObjectTable, CellError> {
        self.schema
            .object_table(object_type)
            .ok_or_else(|| CellError::UnknownObjectType(object_type.to_owned()))
    }

    /// The tables of `object_type`'s chain, root first.
    pub(crate) fn tables(&self, object_type: &str) -> Result<Vec<&ObjectTable>, CellError> {
        self.chain(object_type)?
            .iter()
            .map(|name| self.table(name))
            .collect()
    }

    /// Every stored property of `object_type` (inherited ones included), in
    /// chain order.
    pub(crate) fn columns(&self, object_type: &str) -> Result<Vec<ColumnRef<'_>>, CellError> {
        Ok(self
            .tables(object_type)?
            .into_iter()
            .flat_map(|table| {
                table
                    .columns
                    .iter()
                    .map(move |column| ColumnRef { table, column })
            })
            .collect())
    }

    /// The stored property `property` of `object_type`.
    pub(crate) fn column(
        &self,
        object_type: &str,
        property: &str,
    ) -> Result<ColumnRef<'_>, CellError> {
        if self
            .declaration(object_type, property)?
            .is_some_and(|p| p.computed.is_some())
        {
            return Err(CellError::ReadOnlyProperty {
                object_type: object_type.to_owned(),
                property: property.to_owned(),
            });
        }
        self.columns(object_type)?
            .into_iter()
            .find(|column| column.column.property.as_str() == property)
            .ok_or_else(|| CellError::UnknownProperty {
                object_type: object_type.to_owned(),
                property: property.to_owned(),
            })
    }

    /// The nearest declaration of `property` (this type first, then its
    /// supertypes): the one whose default and `computed` apply.
    pub(crate) fn declaration(
        &self,
        object_type: &str,
        property: &str,
    ) -> Result<Option<&Property>, CellError> {
        for name in self.chain(object_type)?.iter().rev() {
            if let Some(found) = self
                .def(name)?
                .properties
                .iter()
                .find(|candidate| candidate.id.as_str() == property)
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// The effective lifecycle of `property` (nearest declaration).
    pub(crate) fn lifecycle(
        &self,
        object_type: &str,
        property: &str,
    ) -> Result<Option<&Lifecycle>, CellError> {
        for name in self.chain(object_type)?.iter().rev() {
            if let Some(found) = self
                .def(name)?
                .lifecycles
                .iter()
                .find(|lifecycle| lifecycle.property.as_str() == property)
            {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }

    /// Canonicalize one decoded value using the effective stored-property
    /// contract. Invalid source strings remain intact for observation-quality
    /// handling; admitted writes reject them before storage.
    pub(crate) fn canonicalize_object_value(
        &self,
        object_type: &str,
        property: &str,
        value: &norn_ir::RuntimeValue,
    ) -> norn_ir::RuntimeValue {
        self.effective_objects
            .get(object_type)
            .and_then(|object_type| object_type.property(property))
            .map_or_else(
                || value.clone(),
                |contract| contract.canonical_runtime_value(value),
            )
    }

    /// Canonicalize one decoded link-property value using the effective link
    /// contract for its source type. Inherited links use the source type's
    /// fully resolved contract.
    pub(crate) fn canonicalize_link_value(
        &self,
        object_type: &str,
        link: &str,
        property: &str,
        value: &norn_ir::RuntimeValue,
    ) -> norn_ir::RuntimeValue {
        self.effective_links
            .get(object_type)
            .and_then(|links| links.iter().find(|candidate| candidate.id.as_str() == link))
            .and_then(|link| {
                link.properties
                    .iter()
                    .find(|candidate| candidate.id.as_str() == property)
            })
            .map_or_else(
                || value.clone(),
                |contract| contract.canonical_runtime_value(value),
            )
    }

    /// The link `link` of `object_type` (declared or inherited) and the table
    /// of its declaring type.
    pub(crate) fn link(
        &self,
        object_type: &str,
        link: &str,
    ) -> Result<(&Link, &LinkTable), CellError> {
        // The declaring type is the furthest ancestor that declares the id
        // (redeclarations only narrow and get no table).
        for name in self.chain(object_type)? {
            let def = self.def(name)?;
            if let Some(found) = def
                .links
                .iter()
                .find(|candidate| candidate.id.as_str() == link)
                && let Some(table) = self.schema.link_table(name, link)
            {
                return Ok((found, table));
            }
        }
        Err(CellError::UnknownLink {
            object_type: object_type.to_owned(),
            link: link.to_owned(),
        })
    }
}

/// The entity-only portion of an admitted definition set needed by the cell
/// store to rebuild historic projected models during export restore.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelDefinitionSnapshot {
    /// Object type definitions.
    pub object_types: Vec<ObjectTypeDef>,
    /// Shared property type definitions.
    pub property_types: Vec<PropertyTypeDef>,
    /// Interface definitions used by shared commit admission. Missing values
    /// in older snapshots mean no interface definitions were retained.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<InterfaceDef>,
    /// Widget binding access paths used to rebuild the same object indexes.
    /// Older snapshots omit this because they had no binding property indexes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binding_indexes: Vec<BindingIndexSpec>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;

    use super::Model;
    use norn_ir::ObjectTypeDef;

    fn naming_model() -> Model {
        let object_types: Vec<ObjectTypeDef> = serde_json::from_value(json!([
            {
                "name": "marking.Invoice",
                "markings": ["pii.invoice"],
                "properties": [
                    {"id": "number", "type": {"kind": "text"}},
                    {"id": "tenant", "type": {"kind": "text"}, "markings": ["pii.invoice_scope"]}
                ],
                "identity": [{
                    "name": "business",
                    "properties": ["number"],
                    "namingSeries": {
                        "pattern": "INV-{tenant}-{#####}",
                        "timezone": "Etc/UTC",
                        "scopeFields": ["tenant"],
                        "resetPolicy": "never",
                        "gapPolicy": "gapless_within_commit"
                    }
                }]
            },
            {
                "name": "marking.SpecialInvoice",
                "extends": "marking.Invoice",
                "markings": ["pii.special_invoice"],
                "properties": [{
                    "id": "tenant",
                    "type": {"kind": "text"},
                    "markings": ["pii.special_scope"]
                }]
            },
            {
                "name": "marking.PublicInvoice",
                "properties": [
                    {"id": "number", "type": {"kind": "text"}},
                    {"id": "tenant", "type": {"kind": "text"}}
                ],
                "identity": [{
                    "name": "business",
                    "properties": ["number"],
                    "namingSeries": {
                        "pattern": "PUB-{tenant}-{#####}",
                        "timezone": "Etc/UTC",
                        "scopeFields": ["tenant"],
                        "resetPolicy": "never",
                        "gapPolicy": "gapless_within_commit"
                    }
                }]
            }
        ]))
        .expect("object types decode");
        Model::from_definitions(&object_types, &[]).expect("model projects")
    }

    #[test]
    fn naming_counter_attribution_unions_effective_scope_labels_and_keeps_public_counters_empty() {
        let model = naming_model();
        let invoice = model
            .effective_objects
            .get("marking.Invoice")
            .expect("base invoice is projected");
        let key = invoice
            .keys
            .iter()
            .find(|key| key.name.as_str() == "business")
            .expect("business key exists");
        let declaration = key
            .naming_series
            .as_ref()
            .expect("business key is generated")
            .clone();

        assert_eq!(
            model.naming_counter_markings("marking.Invoice", "business", "number", &declaration,),
            Some(BTreeSet::from([
                "pii.invoice".to_owned(),
                "pii.invoice_scope".to_owned(),
                "pii.special_invoice".to_owned(),
                "pii.special_scope".to_owned(),
            ])),
            "base and descendant scope classifications protect the shared counter bucket"
        );

        let public = model
            .effective_objects
            .get("marking.PublicInvoice")
            .expect("public invoice is projected");
        let public_key = public.keys.first().expect("public key is projected");
        let public_declaration = public_key
            .naming_series
            .as_ref()
            .expect("public key is generated");
        assert_eq!(
            model.naming_counter_markings(
                "marking.PublicInvoice",
                "business",
                "number",
                public_declaration,
            ),
            Some(BTreeSet::new()),
            "an unclassified generated key remains public"
        );
        assert!(
            model
                .naming_counter_markings("marking.Invoice", "other", "number", &declaration,)
                .is_none()
        );
    }
}
