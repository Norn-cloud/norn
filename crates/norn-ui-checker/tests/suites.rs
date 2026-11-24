//! `norn-ui-checker`'s integration tests, built as one test binary.

pub(crate) trait TestMust<T> {
    fn req(self, context: &str) -> T;
}

impl<T, E: std::fmt::Display> TestMust<T> for Result<T, E> {
    fn req(self, context: &str) -> T {
        match self {
            Ok(value) => value,
            Err(error) => unreachable!("{context}: {error}"),
        }
    }
}

impl<T> TestMust<T> for Option<T> {
    fn req(self, context: &str) -> T {
        match self {
            Some(value) => value,
            None => unreachable!("{context}"),
        }
    }
}

pub(crate) trait JsonAt {
    fn at<I: serde_json::value::Index>(&self, index: I) -> &serde_json::Value;
    fn at_mut<I: serde_json::value::Index>(&mut self, index: I) -> &mut serde_json::Value;
    fn set(&mut self, key: &str, value: serde_json::Value);
}

impl JsonAt for serde_json::Value {
    fn at<I: serde_json::value::Index>(&self, index: I) -> &serde_json::Value {
        self.get(index).req("json index")
    }

    fn at_mut<I: serde_json::value::Index>(&mut self, index: I) -> &mut serde_json::Value {
        self.get_mut(index).req("json index mut")
    }

    fn set(&mut self, key: &str, value: serde_json::Value) {
        self.as_object_mut()
            .req("json object for set")
            .insert(key.to_owned(), value);
    }
}

#[path = "ui_interaction.rs"]
mod ui_interaction;

#[path = "ui_laws.rs"]
mod ui_laws;

#[path = "ui_lifecycle.rs"]
mod ui_lifecycle;

#[path = "widget_snapshots.rs"]
mod widget_snapshots;

#[path = "widget_ux.rs"]
mod widget_ux;
