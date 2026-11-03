//! CONS-LINEAR-CERT-1: exact, closed wire format bound to a mathematical system.
//! This identity is not a `DefinitionSource`/`Challenge` pin or application evidence.
use super::{Coordinate, Delta, Failure, Rational, Writer, check, validate};
use serde_json::{Value, json};

const PROFILE: &str = "norn.linear-certificate/1";

/// Untrusted wire object; verification always recomputes identity and balance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
    digest: String,
    weights: Vec<(String, Rational)>,
}

pub(super) fn number(q: Rational) -> Value {
    json!([q.numerator.to_string(), q.denominator.to_string()])
}
impl Certificate {
    /// Check the mathematical system and emit a portable certificate.
    /// # Errors
    /// Any independent checker refusal or identity resource limit.
    pub fn issue(
        coordinates: &[Coordinate],
        writers: &[Writer],
        weights: &[Rational],
    ) -> Result<Self, Failure> {
        check(coordinates, writers, weights)?;
        Ok(Self {
            digest: identity(coordinates, writers)?,
            weights: coordinates
                .iter()
                .zip(weights)
                .map(|(c, q)| (c.id.clone(), *q))
                .collect(),
        })
    }

    /// Exact integers are decimal strings, never JSON floating-point numbers.
    #[must_use]
    pub fn json(&self) -> Value {
        json!({"profile":PROFILE,"systemSha256":self.digest,"weights":self.weights.iter().map(|(id, q)| json!([id, number(*q)])).collect::<Vec<_>>()})
    }

    /// Parse a closed, bounded JSON document with canonical rational spelling.
    /// # Errors
    /// Unknown/duplicate members, malformed identity/fraction or byte limit.
    pub fn parse(text: &str) -> Result<Self, Failure> {
        if text.len() > 131_072 {
            return Err(Failure::Malformed);
        }
        norn_canonical::parse_json_verbatim(text.as_bytes()).map_err(|_| Failure::Malformed)?;
        let value: Value = serde_json::from_str(text).map_err(|_| Failure::Malformed)?;
        let object = value.as_object().ok_or(Failure::Malformed)?;
        if object.len() != 3 || value["profile"] != PROFILE {
            return Err(Failure::Malformed);
        }
        let digest = value["systemSha256"].as_str().ok_or(Failure::Malformed)?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Failure::Malformed);
        }
        let entries = value["weights"].as_array().ok_or(Failure::Malformed)?;
        if entries.is_empty() || entries.len() > 256 {
            return Err(Failure::Malformed);
        }
        let mut weights = Vec::new();
        for entry in entries {
            let pair = entry.as_array().ok_or(Failure::Malformed)?;
            if pair.len() != 2 {
                return Err(Failure::Malformed);
            }
            let id = pair
                .first()
                .and_then(Value::as_str)
                .ok_or(Failure::Malformed)?;
            let parts = pair
                .get(1)
                .and_then(Value::as_array)
                .ok_or(Failure::Malformed)?;
            if id.is_empty() || parts.len() != 2 {
                return Err(Failure::Malformed);
            }
            let n = parts
                .first()
                .and_then(Value::as_str)
                .ok_or(Failure::Malformed)?
                .parse::<i64>()
                .map_err(|_| Failure::Malformed)?;
            let d = parts
                .get(1)
                .and_then(Value::as_str)
                .ok_or(Failure::Malformed)?
                .parse::<i64>()
                .map_err(|_| Failure::Malformed)?;
            let q = Rational::new(n, d).ok_or(Failure::Malformed)?;
            if number(q) != *pair.get(1).ok_or(Failure::Malformed)? {
                return Err(Failure::Malformed);
            }
            weights.push((id.to_owned(), q));
        }
        Ok(Self {
            digest: digest.to_owned(),
            weights,
        })
    }

    /// Recheck identity, coordinate order and the declared functional. Stale
    /// mathematical identity is unknown; no stored status field is trusted.
    /// # Errors
    /// Stale identity, substituted coordinates/weights or unbalanced system.
    pub fn verify(&self, coordinates: &[Coordinate], writers: &[Writer]) -> Result<(), Failure> {
        if self.digest != identity(coordinates, writers)? {
            return Err(Failure::Unknown(
                "stale mathematical system certificate".into(),
            ));
        }
        if self.weights.len() != coordinates.len()
            || self
                .weights
                .iter()
                .zip(coordinates)
                .any(|((id, _), c)| id != &c.id)
        {
            return Err(Failure::Malformed);
        }
        check(
            coordinates,
            writers,
            &self.weights.iter().map(|(_, q)| *q).collect::<Vec<_>>(),
        )
    }
}

fn identity(coordinates: &[Coordinate], writers: &[Writer]) -> Result<String, Failure> {
    validate(
        coordinates,
        writers,
        &coordinates.iter().map(|c| c.weight).collect::<Vec<_>>(),
    )?;
    let mut cost = 0_usize;
    let text = |s: &str| s.len().saturating_mul(6);
    for c in coordinates {
        cost = cost
            .saturating_add(text(&c.id))
            .saturating_add(text(&c.unit.code));
        for (key, value) in &c.scope {
            cost = cost.saturating_add(text(key)).saturating_add(text(value));
        }
    }
    for w in writers {
        cost = cost.saturating_add(text(&w.id));
        match &w.delta {
            Delta::Unknown(reason) => cost = cost.saturating_add(text(reason)),
            Delta::Affine(rows) => {
                cost = cost.saturating_add(rows.len().saturating_mul(64));
                for row in rows {
                    cost = cost.saturating_add(row.len().saturating_mul(64));
                }
            }
        }
    }
    if cost > 8_388_608 {
        return Err(Failure::Unknown(
            "certificate identity budget exceeded".into(),
        ));
    }
    let body = json!({"coordinates":coordinates.iter().map(|c| json!([c.id,c.scope,c.unit,number(c.weight)])).collect::<Vec<_>>(),
        "writers":writers.iter().map(|w| json!([w.id,match &w.delta {
            Delta::Unknown(reason) => json!({"unknown":reason}),
            Delta::Affine(rows) => json!({"affine":rows.iter().map(|row| row.iter().copied().map(number).collect::<Vec<_>>()).collect::<Vec<_>>()}),
        }])).collect::<Vec<_>>()});
    let bytes = norn_canonical::serialize_json_verbatim(&body).map_err(|_| Failure::Malformed)?;
    super::super::exact_commitment(b"linear-system/1", &[&bytes])
        .map_err(|_| Failure::Unknown("certificate identity encoding limit".into()))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "exact certificate wire fixtures"
)]
mod tests {
    use super::*;

    fn system() -> (Vec<Coordinate>, Vec<Writer>, Vec<Rational>) {
        let (c, w) = super::super::tests::system();
        let weights = c.iter().map(|c| c.weight).collect();
        (c, w, weights)
    }

    #[test]
    fn cons_certificate_roundtrip_tampering_and_stale_system() {
        let (mut c, mut w, weights) = system();
        let certificate = Certificate::issue(&c, &w, &weights).unwrap();
        let text = certificate.json().to_string();
        assert_eq!(Certificate::parse(&text).unwrap(), certificate);
        assert_eq!(certificate.verify(&c, &w), Ok(()));
        let mut wire = certificate.json();
        wire["weights"][0][1] = json!(["0", "1"]);
        assert_eq!(
            Certificate::parse(&wire.to_string())
                .unwrap()
                .verify(&c, &w),
            Err(Failure::Malformed)
        );
        c[1].unit.code = "mg".into();
        assert!(matches!(
            certificate.verify(&c, &w),
            Err(Failure::Unknown(_))
        ));
        c[1].unit.code = "g".into();
        let Delta::Affine(rows) = &mut w[0].delta else {
            unreachable!()
        };
        rows[1][0] = Rational::new(2, 1).unwrap();
        assert!(matches!(
            certificate.verify(&c, &w),
            Err(Failure::Unknown(_))
        ));
        assert!(Certificate::issue(&c, &w, &weights).is_err());
    }

    #[test]
    fn cons_certificate_refuses_noncanonical_or_open_wire() {
        let (c, w, weights) = system();
        let certificate = Certificate::issue(&c, &w, &weights).unwrap();
        for fraction in [
            json!(["2", "2"]),
            json!(["01", "1"]),
            json!(["1", "0"]),
            json!([1, 1]),
        ] {
            let mut wire = certificate.json();
            wire["weights"][0][1] = fraction;
            assert_eq!(
                Certificate::parse(&wire.to_string()),
                Err(Failure::Malformed)
            );
        }
        let mut wire = certificate.json();
        wire["proved"] = json!(true);
        assert_eq!(
            Certificate::parse(&wire.to_string()),
            Err(Failure::Malformed)
        );
        assert_eq!(
            Certificate::parse("{\"profile\":1,\"profile\":2}"),
            Err(Failure::Malformed)
        );
    }
}
