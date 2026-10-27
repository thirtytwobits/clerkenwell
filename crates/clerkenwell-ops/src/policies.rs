//! The application's per-document access, as the resource policy each
//! document operation names.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use clapham::{Category, OAuthContext, Refusal, ResourcePolicy};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::documents::{EditInput, EditTextInput, GetInput, ListInput, EDIT, EDIT_TEXT, GET, LIST};

/// What a document operation reaches: one document of an entity, or, for
/// `clerkenwell.document.list`, every document of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentScope {
    pub entity: String,
    /// The document; `None` for every document of the entity.
    pub id: Option<String>,
    /// The parameters that choose the store keeping the entity's documents.
    pub store: BTreeMap<String, String>,
}

impl DocumentScope {
    pub(crate) fn new(entity: &str, id: Option<&str>, store: &BTreeMap<String, String>) -> Self {
        Self {
            entity: entity.to_owned(),
            id: id.map(str::to_owned),
            store: store.clone(),
        }
    }
}

/// Why the application refuses a grant a document, as `forbidden`.
#[derive(Debug, Clone, PartialEq)]
pub struct Forbidden {
    pub message: String,
    pub details: Option<Value>,
}

impl Forbidden {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            details: None,
        }
    }
}

/// The resource policy of every document operation, each under the name its
/// descriptor names, for the application to register on its
/// `clapham::Configuration`. Each refuses a call `check` refuses as
/// `forbidden`, before the operation runs.
pub fn document_policies<F, Fut>(check: F) -> Vec<(String, ResourcePolicy)>
where
    F: Fn(OAuthContext, DocumentScope) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), Forbidden>> + Send + 'static,
{
    let check = Arc::new(check);
    vec![
        policy::<ListInput, _, _>(LIST, check.clone(), ListInput::scope),
        policy::<GetInput, _, _>(GET, check.clone(), GetInput::scope),
        policy::<EditInput, _, _>(EDIT, check.clone(), EditInput::scope),
        policy::<EditTextInput, _, _>(EDIT_TEXT, check, EditTextInput::scope),
    ]
}

/// The policy of the operation `name`, whose input is `I`.
fn policy<I, F, Fut>(
    name: &str,
    check: Arc<F>,
    scope: fn(&I) -> DocumentScope,
) -> (String, ResourcePolicy)
where
    I: JsonSchema + DeserializeOwned + Send + 'static,
    F: Fn(OAuthContext, DocumentScope) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), Forbidden>> + Send + 'static,
{
    let policy = ResourcePolicy::typed(move |auth: OAuthContext, input: I| {
        let checked = check(auth, scope(&input));
        async move { checked.await.map_err(forbidden) }
    });
    (name.to_owned(), policy)
}

fn forbidden(forbidden: Forbidden) -> Refusal {
    let mut refusal = Refusal::new("forbidden", Category::Refused, forbidden.message);
    refusal.details = forbidden.details;
    refusal
}
