//! The closed `actor` operand (#2419).
//!
//! What an expression may read about who is acting is exactly the ratified
//! authorization identity tuple `(actorKind, subjectId, agentSessionId?,
//! scope, capability)` (ruling 2026-08-23 §6), plus derived, read-only facts
//! (`roles`, `authMethod`). It is never a flat user record: a `service` and a
//! `user` with the same subject id stay distinct through `actorKind`, and the
//! agent session binding is kept. Nothing else is ambient; reading any other
//! member (`actor.email`, `actor.scope.region`) is a static type error with a
//! stable code (`expr.member.unknown`).
//!
//! Omitted optional dimensions mean absence (`null`), never a wildcard, as in
//! capability-grant matching. Adding or changing a member is a new
//! [`TYPE_ENV_VERSION`](crate::TYPE_ENV_VERSION).
//!
//! | member | type |
//! | --- | --- |
//! | `actorKind` | enum `agent` \| `service` \| `system` \| `user` |
//! | `subjectId` | text |
//! | `agentSessionId` | text, optional |
//! | `scope` | record `norn:ActorScope` |
//! | `capability` | text: the capability exercised by this request |
//! | `roles` | list of text (derived, read-only) |
//! | `authMethod` | text (derived, read-only) |
//!
//! `norn:ActorScope`: `orgId` (text), and optional `tenantId`, `workspaceId`,
//! `environmentId`, `businessUnitId` (text).

use std::collections::BTreeMap;

use crate::env::{Field, FieldSource, RecordKind, RecordType, Type};

/// Record name of the actor operand (outside the object type name grammar,
/// so it never collides with a model type).
pub const ACTOR_RECORD: &str = "norn:Actor";

/// Record name of the actor's scope.
pub const ACTOR_SCOPE_RECORD: &str = "norn:ActorScope";

/// The admitted `actorKind` values (ruling 2026-08-23 §6).
pub const ACTOR_KINDS: [&str; 4] = ["agent", "service", "system", "user"];

fn builtin(fields: impl IntoIterator<Item = (&'static str, Type)>) -> RecordType {
    RecordType {
        kind: RecordKind::Builtin,
        fields: fields
            .into_iter()
            .map(|(name, ty)| {
                (
                    name.to_owned(),
                    Field {
                        ty,
                        source: FieldSource::Builtin,
                    },
                )
            })
            .collect(),
    }
}

/// The actor records, by name.
pub(crate) fn records() -> BTreeMap<String, RecordType> {
    let actor = builtin([
        ("actorKind", Type::enumeration(ACTOR_KINDS)),
        ("subjectId", Type::text()),
        ("agentSessionId", Type::text().optional()),
        ("scope", Type::Record(ACTOR_SCOPE_RECORD.to_owned())),
        ("capability", Type::text()),
        ("roles", Type::text().list()),
        ("authMethod", Type::text()),
    ]);
    let scope = builtin([
        ("orgId", Type::text()),
        ("tenantId", Type::text().optional()),
        ("workspaceId", Type::text().optional()),
        ("environmentId", Type::text().optional()),
        ("businessUnitId", Type::text().optional()),
    ]);
    BTreeMap::from([
        (ACTOR_RECORD.to_owned(), actor),
        (ACTOR_SCOPE_RECORD.to_owned(), scope),
    ])
}
