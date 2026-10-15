use crate::{
    Result,
    auth::Capability,
    catalog::Catalog,
    retrieval::{Packet, Query, select},
    store::NamespaceStore,
};
pub fn inspect(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    id: &str,
    history: bool,
    budget: usize,
) -> Result<Packet> {
    inspect_with_options(cat, cap, store, id, history, budget, None, false)
}
#[allow(clippy::too_many_arguments)]
pub fn inspect_with_options(
    cat: &Catalog,
    cap: &Capability,
    store: &mut NamespaceStore,
    id: &str,
    history: bool,
    budget: usize,
    graph: Option<crate::graph::GraphOptions>,
    impact: bool,
) -> Result<Packet> {
    let mut query = Query::new("");
    query.graph = graph;
    query.impact = impact;
    query.max_bytes = budget;
    query.max_tokens = budget.min(8000);
    query.max_items = 128;
    select(cat, cap, store, &query, Some(id), history)
}
