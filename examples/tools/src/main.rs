//! Flow: an order-support agent with eight tools over a shared in-memory store, answered by a
//! real model. `issue_refund` is declared only once `record_approval` has seen the operator's
//! code, so it appears on the turn after the model records it.
//!
//! Needs `ANTHROPIC_API_KEY`, and the network for `get_weather` (Open-Meteo, no key). Run with
//!
//! ```not_rust
//! cargo run -p example-tools
//! ```

use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use keelrs::{Agent, AgentContext, Runtime, Tool, ToolCall};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The operator's approval code that unlocks refunds.
const APPROVAL_CODE: &str = "7731";

/// A tool failure the model reads as its message.
#[derive(Debug)]
struct Failure(String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failure {}

fn fail<T>(message: impl Into<String>) -> Result<T, Failure> {
    Err(Failure(message.into()))
}

#[derive(Debug, Clone, Serialize)]
struct Order {
    id: String,
    status: String,
    total_eur: f64,
    refunded_eur: f64,
}

/// The orders every agent instance shares.
#[derive(Clone, Default)]
struct Store(Arc<Mutex<HashMap<String, Order>>>);

impl Store {
    fn with(orders: impl IntoIterator<Item = Order>) -> Self {
        let orders = orders.into_iter().map(|o| (o.id.clone(), o)).collect();
        Self(Arc::new(Mutex::new(orders)))
    }

    fn get(&self, id: &str) -> Result<Order, Failure> {
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
fn lookup_order(store: Store) -> Tool {
    Tool::new(
        "lookup_order",
        "Look up one order by id and return its status, total and refunds.",
        async move |call: ToolCall<LookupOrder>| store.get(&call.data.order_id),
    )
}

#[derive(Deserialize, JsonSchema)]
struct CheckInventory {
    /// The stock-keeping unit, e.g. `KB-1` for the keyboard.
    sku: String,
}

fn check_inventory() -> Tool {
    Tool::new(
        "check_inventory",
        "How many units of a product are in stock.",
        async |call: ToolCall<CheckInventory>| match call.data.sku.as_str() {
            "KB-1" => Ok(format!("{} units in stock", 14)),
            "MS-2" => Ok("out of stock".to_owned()),
            sku => fail(format!("unknown SKU {sku}")),
        },
    )
}

fn current_time() -> Tool {
    Tool::new(
        "current_time",
        "The current time, as seconds since the Unix epoch (UTC).",
        async |_: ToolCall<()>| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
            Ok::<_, Infallible>(format!("{} seconds since the Unix epoch", now.as_secs()))
        },
    )
}

#[derive(Deserialize, JsonSchema)]
struct IssueRefund {
    order_id: String,
    /// The amount to refund, in euros.
    amount_eur: f64,
}

fn issue_refund(store: Store) -> Tool {
    Tool::new(
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

#[derive(Deserialize, JsonSchema)]
struct ListFiles {
    /// A directory relative to the current one; the current one when absent.
    path: Option<String>,
}

fn list_files() -> Tool {
    Tool::new(
        "list_files",
        "List the files in a directory under the current one, with `ls`.",
        async |call: ToolCall<ListFiles>| {
            let path = call.data.path.unwrap_or_else(|| ".".to_owned());
            if Path::new(&path)
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
            {
                return fail(format!(
                    "{path} is not a relative path under the current directory"
                ));
            }
            let out = tokio::process::Command::new("ls")
                .arg(&path)
                .output()
                .await
                .map_err(|e| Failure(e.to_string()))?;
            if !out.status.success() {
                return fail(String::from_utf8_lossy(&out.stderr).trim().to_owned());
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        },
    )
}

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

fn convert_currency() -> Tool {
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

#[derive(Deserialize, JsonSchema)]
struct GetWeather {
    /// A city name, e.g. `Athens`.
    city: String,
}

#[derive(Deserialize)]
struct Places {
    results: Option<Vec<Place>>,
}

#[derive(Deserialize)]
struct Place {
    name: String,
    country: Option<String>,
    latitude: f64,
    longitude: f64,
}

#[derive(Deserialize)]
struct Forecast {
    current: Current,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    wind_speed_10m: f64,
}

/// Online: Open-Meteo's geocoding, then its forecast; no key needed.
fn get_weather(http: reqwest::Client) -> Tool {
    Tool::new(
        "get_weather",
        "The current temperature and wind in a city.",
        async move |call: ToolCall<GetWeather>| {
            let http_error = |e: reqwest::Error| Failure(e.to_string());
            let places: Places = http
                .get("https://geocoding-api.open-meteo.com/v1/search")
                .query(&[("name", call.data.city.as_str()), ("count", "1")])
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(http_error)?
                .json()
                .await
                .map_err(http_error)?;
            let Some(place) = places.results.into_iter().flatten().next() else {
                return fail(format!("no city called {}", call.data.city));
            };
            let forecast: Forecast = http
                .get("https://api.open-meteo.com/v1/forecast")
                .query(&[
                    ("latitude", place.latitude.to_string()),
                    ("longitude", place.longitude.to_string()),
                    ("current", "temperature_2m,wind_speed_10m".to_owned()),
                ])
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(http_error)?
                .json()
                .await
                .map_err(http_error)?;
            Ok(format!(
                "{} ({}): {} °C, wind {} km/h",
                place.name,
                place.country.unwrap_or_default(),
                forecast.current.temperature_2m,
                forecast.current.wind_speed_10m
            ))
        },
    )
    .timeout(Duration::from_secs(15))
}

#[derive(Deserialize, JsonSchema)]
struct RecordApproval {
    /// The operator's approval code.
    code: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let store = Store::with([Order {
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
            cx.use_tool(lookup_order(store.clone()));
            cx.use_tool(check_inventory());
            cx.use_tool(current_time());
            cx.use_tool(list_files());
            cx.use_tool(convert_currency());
            cx.use_tool(get_weather(http.clone()));
            let flag = approved.clone();
            cx.use_tool(Tool::new(
                "record_approval",
                "Record an operator approval code; refunds become available once it is valid.",
                async move |call: ToolCall<RecordApproval>| {
                    let valid = call.data.code == APPROVAL_CODE;
                    flag.store(valid, Ordering::SeqCst);
                    Ok::<_, Infallible>(if valid {
                        "Approval recorded."
                    } else {
                        "That code is not valid."
                    })
                },
            ));
            if approved.load(Ordering::SeqCst) {
                cx.use_tool(issue_refund(store.clone()));
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
             While you're at it: is the KB-1 keyboard in stock, what time is it, what files are in \
             the current directory, what is 20 EUR in USD, and what's the weather in Athens?",
        )
        .await?;

    println!("{}\n", reply.text());
    println!("Store afterwards: {:?}", store.get("A1001")?);
    Ok(())
}
