//! `get_weather`: online, through Open-Meteo's geocoding then its forecast; no key needed.

use std::time::Duration;

use keelrs::{Tool, ToolCall};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::failure::{Failure, fail};

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

pub(crate) fn get_weather(http: reqwest::Client) -> Tool {
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
