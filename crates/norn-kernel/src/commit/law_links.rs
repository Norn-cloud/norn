use super::post_state::{PostRow, PostState};
use crate::cell_host::{CommitError, FactKey};
use crate::commit_check::CommitView;
use crate::governing_laws::{LawEvaluationError, LinkTargets, LinkedTarget};
use norn_ir::ObjectTypeName;
/// LAW-LINK-1 bound: most subjects one written target re-checks in a commit.
pub(crate) const MAX_LINK_LAW_SUBJECTS: usize = 4096;

pub(crate) struct CommitLinks<'p, 'c, 'a, V: CommitView + ?Sized> {
    post: &'p PostState<'c, 'a, V>,
    subject: FactKey,
    host: Option<V::Error>,
}

impl<'p, 'c, 'a, V: CommitView + ?Sized> CommitLinks<'p, 'c, 'a, V> {
    pub(crate) fn new(post: &'p PostState<'c, 'a, V>, subject: &FactKey) -> Self {
        Self {
            post,
            subject: post.model().canonical(subject),
            host: None,
        }
    }
    pub(crate) fn take_host_error(&mut self) -> Option<V::Error> {
        self.host.take()
    }
    fn map_error(&mut self, error: CommitError<V::Error>) -> LawEvaluationError {
        if let CommitError::Law {reason,..} = error {return reason;}
        let diagnostic=LawEvaluationError::Unsupported(format!("LAW-LINK-1: {}",error.code()));
        if let CommitError::Host(cause) = error {self.host=Some(cause);}
        diagnostic
    }
    fn target_row(&mut self, key: &FactKey) -> Result<LinkedTarget, LawEvaluationError> {
        match self.post.row(key).map_err(|e| self.map_error(e))? {
            PostRow::Live {
                concrete,
                version,
                values,
            } => Ok(LinkedTarget {
                key: FactKey::new(concrete.as_str(), &key.key),
                version: version.0,
                values: values.into_owned(),
            }),
            PostRow::Deleted { tombstone } => Err(LawEvaluationError::Unsupported(format!(
                "LAW-LINK-1: linked row was deleted at {tombstone}"
            ))),
            PostRow::Absent => Err(LawEvaluationError::Unsupported(
                "LAW-LINK-1: linked row is absent".into(),
            )),
        }
    }
}

impl<V: CommitView + ?Sized> LinkTargets for CommitLinks<'_, '_, '_, V> {
    fn target(&mut self, link: &str) -> Result<Option<LinkedTarget>, LawEvaluationError> {
        let edges = self
            .post
            .incident(&self.subject)
            .map_err(|e| self.map_error(e))?;
        let mut targets = edges
            .into_iter()
            .filter(|k| k.source == self.subject && k.link == link);
        let Some(key) = targets.next() else {
            return Ok(None);
        };
        if targets.next().is_some() {
            return Err(LawEvaluationError::Unsupported(format!(
                "LAW-LINK-1: multiple targets of {link}"
            )));
        }
        self.target_row(&key.target).map(Some)
    }
    fn sources(
        &mut self,
        role: &str,
        source: &ObjectTypeName,
        link: &str,
    ) -> Result<Vec<LinkedTarget>, LawEvaluationError> {
        let mut found = Vec::new();
        let edges = self
            .post
            .incident(&self.subject)
            .map_err(|e| self.map_error(e))?;
        for key in edges {
            if key.target != self.subject || key.link != link {
                continue;
            }
            let concrete = self.post.concrete(&key.source).map_err(|e|self.map_error(e))?
                .ok_or_else(|| LawEvaluationError::Unsupported(format!("LAW-LINK-MANY-1: a source of {role} is absent")))?;
            if !self.post.model().is_a(&concrete, source) {
                continue;
            }
            if found.len() >= MAX_LINK_LAW_SUBJECTS {
                return Err(LawEvaluationError::LinkFanOutExceeded {
                    target: format!("{}/{}", self.subject.object_type.as_str(), self.subject.key),
                    limit: MAX_LINK_LAW_SUBJECTS,
                });
            }
            found.push(self.target_row(&key.source)?);
        }
        Ok(found)
    }
}
