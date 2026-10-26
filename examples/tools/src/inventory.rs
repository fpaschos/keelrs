//! `check_inventory`: stock levels from a fixed table.

use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::failure::fail;

#[derive(Deserialize, JsonSchema)]
struct CheckInventory {
    /// The stock-keeping unit, e.g. `KB-1` for the keyboard.
    sku: String,
}

pub(crate) fn check_inventory() -> Tool {
    Tool::new(
        "check_inventory",
        "How many units of a product are in stock.",
        async |call: ToolCall<CheckInventory>| match call.data.sku.as_str() {
            "KB-1" => Ok("14 units in stock".to_owned()),
            "MS-2" => Ok("out of stock".to_owned()),
            sku => fail(format!("unknown SKU {sku}")),
        },
    )
}
