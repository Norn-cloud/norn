//! Unit tests for typed Widget UX checking.

#[cfg(test)]
mod decimal_wire_tests {
    use norn_expr::InferredType;
    use norn_expr::schema::Schema;

    use super::super::types::inferred_matches_schema;

    fn accepts(inferred: &InferredType) -> Result<(), String> {
        let schema = Schema::parse_action(
            &serde_json::from_str(r#"{"type":"number","nornType":{"kind":"decimal","scale":2}}"#)
                .map_err(|error| format!("{error:?}"))?,
            "amount",
        )
        .map_err(|error| format!("{error:?}"))?;
        inferred_matches_schema(inferred, &schema, None, &mut |_, _| false, "amount")
    }

    #[test]
    fn action_mapping_accepts_decimal_text_but_not_numeric_wire_values() {
        assert!(accepts(&InferredType::String).is_ok());
        assert!(accepts(&InferredType::Const("125.50".to_owned())).is_ok());
        assert!(accepts(&InferredType::Enum(vec!["0".into(), "-125.50".into()])).is_ok());
        assert!(accepts(&InferredType::Number).is_err());
        assert!(accepts(&InferredType::Null).is_err());
        for text in ["1.001", "12,50", "NaN"] {
            assert!(accepts(&InferredType::Const(text.to_owned())).is_err());
            assert!(accepts(&InferredType::Enum(vec!["0".into(), text.into()])).is_err());
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn decimal_mapping_matches_the_shared_decoder(coefficient in 0_u32..1_000_000, fraction in 0_u8..100) {
            let text = format!("{coefficient}.{fraction:02}");
            proptest::prop_assert!(accepts(&InferredType::Const(text.clone())).is_ok());
            proptest::prop_assert!(accepts(&InferredType::Enum(vec![text.clone()])).is_ok());
            let invalid = format!("{text}1");
            proptest::prop_assert!(accepts(&InferredType::Const(invalid)).is_err());
        }
    }
}
