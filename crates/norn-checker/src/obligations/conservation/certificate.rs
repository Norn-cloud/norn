//! CONS-LINEAR-CERT-1: exact, closed wire format bound to a mathematical system.
//! This identity is not a `DefinitionSource`/`Challenge` pin or application evidence.
use super::{Coordinate, Delta, Failure, Rational, Writer, check, validate};
use serde_json::{Value, json};

const PROFILE: &str = "norn.linear-certificate/1";
const WIRE_LIMIT: usize = 131_072;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Wire {
    profile: String,
    system_sha256: String,
    weights: Vec<(String, (String, String))>,
}

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
        let result = Self {
            digest: identity(coordinates, writers)?,
            weights: coordinates
                .iter()
                .zip(weights)
                .map(|(c, q)| (c.id.clone(), *q))
                .collect(),
        };
        if result.json().to_string().len() > WIRE_LIMIT {
            return Err(Failure::Unknown("certificate wire limit".into()));
        }
        Ok(result)
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
        if text.len() > WIRE_LIMIT {
            return Err(Failure::Malformed);
        }
        let wire: Wire = serde_json::from_str(text).map_err(|_error| Failure::Malformed)?;
        let digest = wire.system_sha256;
        if wire.profile != PROFILE
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || wire.weights.is_empty()
            || wire.weights.len() > 256
        {
            return Err(Failure::Malformed);
        }
        let weights = wire
            .weights
            .into_iter()
            .map(|(id, (n, d))| {
                let q = Rational::new(
                    n.parse().map_err(|_error| Failure::Malformed)?,
                    d.parse().map_err(|_error| Failure::Malformed)?,
                )
                .ok_or(Failure::Malformed)?;
                if id.is_empty() || q.numerator.to_string() != n || q.denominator.to_string() != d {
                    return Err(Failure::Malformed);
                }
                Ok((id, q))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { digest, weights })
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
    let bytes =
        norn_canonical::serialize_json_verbatim(&body).map_err(|_error| Failure::Malformed)?;
    super::super::exact_commitment(b"linear-system/1", &[&bytes])
        .map_err(|_error| Failure::Unknown("certificate identity encoding limit".into()))
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
    fn cons_certificate_issuer_respects_wire_bound() {
        let (mut c, w, weights) = system();
        for c in &mut c {
            c.id = "x".repeat(50_000) + &c.id;
        }
        assert!(matches!(
            Certificate::issue(&c, &w, &weights),
            Err(Failure::Unknown(_))
        ));
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
        let duplicate = certificate.json().to_string().replacen(
            '{',
            &format!("{{\"profile\":\"{PROFILE}\","),
            1,
        );
        assert_eq!(Certificate::parse(&duplicate), Err(Failure::Malformed));
    }
}
