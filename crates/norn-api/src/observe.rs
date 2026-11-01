//! OBS-EVIDENCE-1: a trusted adapter supplies data, never business mutation authority.
use crate::{host::DevHost, refusal::Refusal, scope::CellScope};
use norn_cell::engine::SqlEngine;
use norn_ir::RuntimeValue;
use norn_kernel::Observed;

/// Connector routing dimensions from trusted deployment configuration.
#[derive(Debug, Clone, Copy)]
pub struct ObservationStream<'a> {
    /// The configured connector instance, not a webhook field.
    pub connector_instance: &'a str,
    /// The configured source partition.
    pub partition: &'a str,
}

fn inbox_id(stream: ObservationStream<'_>, event: &str) -> Result<String, norn_ir::canonical::CanonicalJsonError> {
    norn_ir::canonical::domain_hash_hex(b"norn.observation.inbox.v1\0", &(stream.connector_instance, stream.partition, event))
        .map(|digest| format!("source-observation:{digest}"))
}

impl<E: SqlEngine> DevHost<E> {
    /// Run the trusted adapter's checked submission and enqueue its complete evidence.
    /// A callback keeps transport dependencies outside the cell/API contracts.
    /// Scope comes from trusted connector configuration; this is not a network route.
    /// Mapping and observation consumption happen later through the cell driver.
    ///
    /// # Errors
    /// Scope mismatch, adapter framing/authentication failure, or durable inbox failure.
    pub fn observe_submission(
        &self,
        scope: &CellScope,
        stream: ObservationStream<'_>,
        checked: impl FnOnce() -> Result<Observed<RuntimeValue>, Refusal>,
    ) -> Result<bool, Refusal> {
        self.scope()
            .admit(Some(&scope.org_id), Some(&scope.environment_id))?;
        if stream.connector_instance.trim().is_empty() || stream.partition.trim().is_empty() {
            return Err(Refusal::invalid("connector instance and partition must be nonblank"));
        }
        let _epoch = self.definition_read()?;
        let mut observed = checked()?;
        observed.id.0 = inbox_id(stream, &observed.id.0).map_err(Refusal::host)?;
        let value = norn_ir::runtime::encode(&observed).map_err(Refusal::host)?;
        self.cell()?
            .inject(&observed.id.0, &value)
            .map_err(Refusal::host)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, missing_docs, reason = "source identity correspondence")]
mod tests {
    use super::*;
    use norn_testkit::proptest::prelude::*;
    proptest! {
        #[test]
        fn feed_identity_is_stable_and_separates_trusted_streams(connector in "[a-z%/]{1,12}", partition in "[a-z%/]{1,12}", event in "[a-z%/]{1,12}") {
            let stream = ObservationStream {connector_instance:&connector, partition:&partition};
            let id = inbox_id(stream, &event).expect("identity");
            prop_assert_eq!(&id, &inbox_id(stream, &event).expect("replay"));
            prop_assert_ne!(&id, &inbox_id(ObservationStream {connector_instance:&format!("x{connector}"), ..stream}, &event).expect("other connector"));
            prop_assert_ne!(&id, &inbox_id(ObservationStream {partition:&format!("x{partition}"), ..stream}, &event).expect("other partition"));
            prop_assert_ne!(&id, &inbox_id(stream, &format!("x{event}")).expect("other event"));
        }
    }
}
