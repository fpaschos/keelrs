//! The orders every agent instance shares, and the tools that read and change them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::failure::{Failure, fail};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Order {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) total_eur: f64,
    pub(crate) refunded_eur: f64,
}

/// The orders every agent instance shares.
#[derive(Clone)]
pub(crate) struct Store(Arc<Mutex<HashMap<String, Order>>>);

impl Store {
    pub(crate) fn new(orders: impl IntoIterator<Item = Order>) -> Self {
        let orders = orders.into_iter().map(|o| (o.id.clone(), o)).collect();
        Self(Arc::new(Mutex::new(orders)))
    }

    pub(crate) fn get(&self, id: &str) -> Result<Order, Failure> {
        match self.0.lock().unwrap().get(id) {
            Some(order) => Ok(order.clone()),
            None => fail(format!("no order with id {id}")),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct LookupOrder {
    /// The order id, e.g. `A1001`.
    order_id: String,
}

/// Defined once and declared every turn, as Flue's `defineTool`.
pub(crate) fn lookup_order(store: Store) -> Tool {
    Tool::host(
        "lookup_order",
        "Look up one order by id and return its status, total and refunds.",
        async move |call: ToolCall<LookupOrder>| store.get(&call.data.order_id),
    )
}

#[derive(Deserialize, JsonSchema)]
struct IssueRefund {
    order_id: String,
    /// The amount to refund, in euros.
    amount_eur: f64,
}

pub(crate) fn issue_refund(store: Store) -> Tool {
    Tool::host(
        "issue_refund",
        "Refund part or all of an order. Needs an operator approval first.",
        async move |call: ToolCall<IssueRefund>| {
            let IssueRefund {
                order_id,
                amount_eur,
            } = call.data;
            let mut orders = store.0.lock().unwrap();
            let Some(order) = orders.get_mut(&order_id) else {
                return fail(format!("no order with id {order_id}"));
            };
            if amount_eur <= 0.0 || order.refunded_eur + amount_eur > order.total_eur {
                return fail(format!(
                    "cannot refund {amount_eur} EUR: {} of {} EUR is already refunded",
                    order.refunded_eur, order.total_eur
                ));
            }
            order.refunded_eur += amount_eur;
            Ok(order.clone())
        },
    )
}
