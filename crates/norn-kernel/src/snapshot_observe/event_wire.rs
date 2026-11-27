//! Exact, closed wire decoding for [`ObservationEvent`].
//!
//! `#[serde(tag = "op")]` on a derived `Deserialize` buffers through Serde's
//! internal `Content` enum, which breaks `RawValue`-backed exact `Decimal`
//! decoding inside nested [`FactEvent`] payloads. Decode directly from the
//! object's `MapAccess` (matching [`FactEvent`]'s own wire decoder) while
//! preserving `deny_unknown_fields`, per-variant closed member sets, and
//! non-nullable field validation.

use norn_ir::provenance::InstanceProvenance;
use norn_ir::{DataQualityFinding, PropertyId, TransitionId};
use serde::Deserialize;

use super::{ObservationEvent, ObservationSynthesis};
use crate::cell_host::FactKey;
use crate::decision_commit::FactEvent;

#[derive(Clone, Copy)]
enum ObservationEventOp {
    ObservedFact,
    ObservedTransition,
    DataQualityFinding,
    ObservedDeleted,
    ObservedRecreated,
}

impl ObservationEventOp {
    const VARIANTS: &[&str] = &[
        "observed_fact",
        "observed_transition",
        "data_quality_finding",
        "observed_deleted",
        "observed_recreated",
    ];

    fn parse<E: serde::de::Error>(op: &str) -> Result<Self, E> {
        match op {
            "observed_fact" => Ok(Self::ObservedFact),
            "observed_transition" => Ok(Self::ObservedTransition),
            "data_quality_finding" => Ok(Self::DataQualityFinding),
            "observed_deleted" => Ok(Self::ObservedDeleted),
            "observed_recreated" => Ok(Self::ObservedRecreated),
            other => Err(E::unknown_variant(other, Self::VARIANTS)),
        }
    }

    const fn members(self) -> &'static [&'static str] {
        match self {
            Self::ObservedFact => &["event", "provenance"],
            Self::ObservedTransition => &[
                "key",
                "property",
                "transition",
                "from",
                "to",
                "synthesized",
                "provenance",
            ],
            Self::DataQualityFinding => &["fact"],
            Self::ObservedDeleted => &["key", "tombstoned", "provenance"],
            Self::ObservedRecreated => &["key", "provenance"],
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObservationEventWire {
    op: String,
    #[serde(default, deserialize_with = "present_field")]
    event: Option<FactEvent>,
    #[serde(default, deserialize_with = "present_field")]
    provenance: Option<InstanceProvenance>,
    #[serde(default, deserialize_with = "present_field")]
    key: Option<FactKey>,
    #[serde(default, deserialize_with = "present_field")]
    property: Option<PropertyId>,
    #[serde(default, deserialize_with = "present_field")]
    transition: Option<TransitionId>,
    #[serde(default, deserialize_with = "present_field")]
    from: Option<String>,
    #[serde(default, deserialize_with = "present_field")]
    to: Option<String>,
    #[serde(default, deserialize_with = "present_field")]
    synthesized: Option<ObservationSynthesis>,
    #[serde(default, deserialize_with = "present_field")]
    fact: Option<DataQualityFinding>,
    #[serde(default, deserialize_with = "present_field")]
    tombstoned: Option<bool>,
}

fn present_field<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn required_field<T, E: serde::de::Error>(value: Option<T>, name: &'static str) -> Result<T, E> {
    value.ok_or_else(|| E::missing_field(name))
}

impl ObservationEventWire {
    fn checked_op<E: serde::de::Error>(&self) -> Result<ObservationEventOp, E> {
        let op = ObservationEventOp::parse::<E>(&self.op)?;
        let allowed = op.members();
        let present = [
            ("event", self.event.is_some()),
            ("provenance", self.provenance.is_some()),
            ("key", self.key.is_some()),
            ("property", self.property.is_some()),
            ("transition", self.transition.is_some()),
            ("from", self.from.is_some()),
            ("to", self.to.is_some()),
            ("synthesized", self.synthesized.is_some()),
            ("fact", self.fact.is_some()),
            ("tombstoned", self.tombstoned.is_some()),
        ];
        if let Some((name, _)) = present
            .into_iter()
            .find(|(name, is_present)| *is_present && !allowed.contains(name))
        {
            return Err(E::unknown_field(name, allowed));
        }
        Ok(op)
    }
}

struct ObservationEventVisitor;

impl<'de> serde::de::Visitor<'de> for ObservationEventVisitor {
    type Value = ObservationEventWire;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an observation event object")
    }

    fn visit_map<M: serde::de::MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
        ObservationEventWire::deserialize(serde::de::value::MapAccessDeserializer::new(map))
    }
}

impl<'de> Deserialize<'de> for ObservationEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = deserializer.deserialize_map(ObservationEventVisitor)?;
        Ok(match wire.checked_op::<D::Error>()? {
            ObservationEventOp::ObservedFact => Self::ObservedFact {
                event: required_field::<_, D::Error>(wire.event, "event")?,
                provenance: required_field::<_, D::Error>(wire.provenance, "provenance")?,
            },
            ObservationEventOp::ObservedTransition => Self::ObservedTransition {
                key: required_field::<_, D::Error>(wire.key, "key")?,
                property: required_field::<_, D::Error>(wire.property, "property")?,
                transition: required_field::<_, D::Error>(wire.transition, "transition")?,
                from: required_field::<_, D::Error>(wire.from, "from")?,
                to: required_field::<_, D::Error>(wire.to, "to")?,
                synthesized: required_field::<_, D::Error>(wire.synthesized, "synthesized")?,
                provenance: required_field::<_, D::Error>(wire.provenance, "provenance")?,
            },
            ObservationEventOp::DataQualityFinding => Self::DataQualityFinding {
                fact: required_field::<_, D::Error>(wire.fact, "fact")?,
            },
            ObservationEventOp::ObservedDeleted => Self::ObservedDeleted {
                key: required_field::<_, D::Error>(wire.key, "key")?,
                tombstoned: required_field::<_, D::Error>(wire.tombstoned, "tombstoned")?,
                provenance: required_field::<_, D::Error>(wire.provenance, "provenance")?,
            },
            ObservationEventOp::ObservedRecreated => Self::ObservedRecreated {
                key: required_field::<_, D::Error>(wire.key, "key")?,
                provenance: required_field::<_, D::Error>(wire.provenance, "provenance")?,
            },
        })
    }
}
