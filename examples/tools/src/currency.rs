//! `convert_currency`: an enum argument becomes a schema `enum`.

use std::convert::Infallible;

use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
enum Currency {
    Eur,
    Usd,
    Gbp,
}

impl Currency {
    /// Fixed rates, so the example runs the same offline.
    fn in_eur(self) -> f64 {
        match self {
            Self::Eur => 1.0,
            Self::Usd => 0.92,
            Self::Gbp => 1.17,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct ConvertCurrency {
    amount: f64,
    from: Currency,
    to: Currency,
}

pub(crate) fn convert_currency() -> Tool {
    Tool::new(
        "convert_currency",
        "Convert an amount between currencies at fixed rates.",
        async |call: ToolCall<ConvertCurrency>| {
            let ConvertCurrency { amount, from, to } = call.data;
            let converted = amount * from.in_eur() / to.in_eur();
            Ok::<_, Infallible>(format!("{amount} {from:?} is {converted:.2} {to:?}"))
        },
    )
}
