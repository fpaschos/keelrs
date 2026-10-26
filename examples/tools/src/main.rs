//! Flow: an order-support agent with eight tools over a shared in-memory store, answered by a
//! real model. `issue_refund` is declared only once `record_approval` has seen the operator's
//! code, so it appears on the turn after the model records it. Each tool lives in its own
//! module; the store and its tools share one.
//!
//! Needs `ANTHROPIC_API_KEY`, and the network for `get_weather` (Open-Meteo, no key). Run with
//!
//! ```not_rust
//! cargo run -p example-tools
//! ```

mod approval;
mod currency;
mod failure;
mod files;
mod inventory;
mod store;
mod time;
mod weather;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use keelrs::{Agent, AgentContext, Runtime};

use crate::store::{Order, Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store = Store::new([Order {
        id: "A1001".to_owned(),
        status: "delivered".to_owned(),
        total_eur: 120.0,
        refunded_eur: 0.0,
    }]);
    // Shared by every instance of the agent: conditional tools close over it.
    let approved = Arc::new(AtomicBool::new(false));
    let http = reqwest::Client::new();

    let support = Agent::new("support", {
        let (store, approved) = (store.clone(), approved.clone());
        move |cx: &mut AgentContext| {
            cx.use_model("anthropic/claude-sonnet-5-5");
            cx.use_tool(store::lookup_order(store.clone()));
            cx.use_tool(inventory::check_inventory());
            cx.use_tool(time::current_time());
            cx.use_tool(files::list_files());
            cx.use_tool(currency::convert_currency());
            cx.use_tool(weather::get_weather(http.clone()));
            cx.use_tool(approval::record_approval(approved.clone()));
            if approved.load(Ordering::SeqCst) {
                cx.use_tool(store::issue_refund(store.clone()));
            }
            "You help customers with their orders. Use the tools; never guess a fact a tool can \
             give you. Refunds need an operator approval recorded first."
                .into()
        }
    });
    let runtime = Runtime::start([support.clone()]);

    let instance = runtime.init(&support).id("demo")?;
    let reply = instance
        .ask(
            "Order A1001 arrived and I'd like 20 EUR back; the operator's approval code is 7731. \
             While you're at it: is the KB-1 keyboard in stock, what time is it in athens, what files are in \
             the current directory, what is 20 EUR in USD, and what's the weather in Athens?",
        )
        .await?;

    println!("{}\n", reply.text());
    println!("Store afterwards: {:?}", store.get("A1001")?);
    Ok(())
}
