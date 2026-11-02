//! Structured results: the reserved `finish_structured` tool, driven through
//! `run_with` on rig-core's scripted model.

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::*;

/// Runs `agent` on one message asking for a `T`, and returns every request the model got.
async fn structured<T: DeserializeOwned + JsonSchema + 'static>(
    agent: &Agent,
    turns: impl IntoIterator<Item = MockTurn>,
) -> (Result<Settled, String>, Vec<CompletionRequest>) {
    drive(
        agent,
        turns,
        Some(Structured::of::<T>()),
        &mut Caches::default(),
    )
    .await
}

/// A plan of work.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Plan {
    steps: Vec<String>,
}

fn finish(id: &str, arguments: Value) -> MockTurn {
    MockTurn::tool_call(id, FINISH_STRUCTURED, arguments)
}

#[tokio::test]
async fn a_valid_finish_structured_settles_with_its_value_without_calling_the_model_again() {
    let (settled, requests) = structured::<Plan>(
        &agent(|_| {}),
        [
            finish("c1", json!({"steps": ["one"]})),
            MockTurn::text("Never reached."),
        ],
    )
    .await;
    assert_eq!(settled.unwrap().structured, Some(json!({"steps": ["one"]})));
    assert_eq!(requests.len(), 1);
}

/// The text of every user message the run accepted, in order, skipping tool results.
fn user_texts(settled: &Settled) -> Vec<String> {
    let mut texts = Vec::new();
    for message in &settled.messages {
        let RigMessage::User { content, .. } = message.0.as_ref() else {
            continue;
        };
        let text: String = content
            .iter()
            .filter_map(|c| match c {
                UserContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        if !text.is_empty() {
            texts.push(text);
        }
    }
    texts
}

#[tokio::test]
async fn the_other_calls_of_the_finishing_reply_still_run() {
    let (settled, requests) = structured::<Plan>(
        &agent(|cx| cx.use_tool(clock())),
        [reply([
            ("c1", FINISH_STRUCTURED, json!({"steps": ["one"]})),
            ("c2", "current_time", json!({})),
        ])],
    )
    .await;
    let settled = settled.unwrap();
    assert_eq!(settled.structured, Some(json!({"steps": ["one"]})));
    assert_eq!(
        results(&settled),
        [("Recorded.".to_owned(), false), ("noon".to_owned(), false)]
    );
    assert_eq!(requests.len(), 1);
}

#[tokio::test]
async fn the_first_of_two_valid_calls_in_one_reply_wins() {
    let (settled, _) = structured::<Plan>(
        &agent(|_| {}),
        [reply([
            ("c1", FINISH_STRUCTURED, json!({"steps": ["first"]})),
            ("c2", FINISH_STRUCTURED, json!({"steps": ["second"]})),
        ])],
    )
    .await;
    let settled = settled.unwrap();
    assert_eq!(settled.structured, Some(json!({"steps": ["first"]})));
    assert_eq!(
        results(&settled),
        [
            ("Recorded.".to_owned(), false),
            ("already finished".to_owned(), false)
        ]
    );
}

#[tokio::test]
async fn invalid_arguments_are_a_tool_error_the_model_retries_without_a_re_ask() {
    let (settled, requests) = structured::<Plan>(
        &agent(|_| {}),
        [
            finish("c1", json!({"steps": "one"})),
            finish("c2", json!({"steps": ["one"]})),
        ],
    )
    .await;
    let settled = settled.unwrap();
    assert_eq!(settled.structured, Some(json!({"steps": ["one"]})));
    assert_eq!(requests.len(), 2);
    let results = results(&settled);
    assert!(results[0].1, "{results:?}");
    assert_eq!(user_texts(&settled).len(), 1, "no re-ask");
}

/// A finding whose note the program reads but never writes back out.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Finding {
    title: String,
    #[serde(skip_serializing)]
    #[expect(dead_code, reason = "only the recorded JSON is checked")]
    note: String,
}

#[tokio::test]
async fn the_recorded_value_is_the_json_the_model_sent() {
    let sent = json!({"title": "Typo", "note": "In the README."});
    let (settled, _) = structured::<Finding>(&agent(|_| {}), [finish("c1", sent.clone())]).await;
    assert_eq!(settled.unwrap().structured, Some(sent));
}

/// A reviewer's verdict.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Approve,
    RequestChanges,
}

#[tokio::test]
async fn a_t_that_is_not_an_object_travels_wrapped_as_result_and_settles_unwrapped() {
    let (settled, requests) = structured::<Verdict>(
        &agent(|_| {}),
        [
            finish("c1", json!({"result": "reject"})),
            finish("c2", json!({"result": "request_changes"})),
        ],
    )
    .await;
    let settled = settled.unwrap();
    assert_eq!(settled.structured, Some(json!("request_changes")));
    assert!(results(&settled)[0].1, "an unknown variant is an error");
    assert_eq!(
        requests[0].tools[0].parameters,
        json!({
            "type": "object",
            "properties": {"result": {
                "type": "string",
                "enum": ["approve", "request_changes"],
                "description": "A reviewer's verdict.",
            }},
            "required": ["result"],
            "additionalProperties": false,
        })
    );
}

const FOOTER: &str = "When the task is complete, call `finish_structured` with your answer as \
    its arguments. The arguments are checked against its schema; if they don't match you get an \
    error and can try again. Don't answer in plain text: only a successful `finish_structured` \
    call counts.";

#[tokio::test]
async fn the_footer_follows_the_message_and_finish_structured_comes_last_with_ts_doc() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let (settled, requests) =
        structured::<Plan>(&agent, [finish("c1", json!({"steps": []}))]).await;
    assert_eq!(user_texts(&settled.unwrap())[0], format!("Go\n\n{FOOTER}"));
    assert_eq!(preamble(&requests[0]), None);
    assert_eq!(
        tool_names(&requests[0]),
        ["current_time", "finish_structured"]
    );
    assert_eq!(
        requests[0].tools[1].description,
        "Call this once, when the task is complete, with your final answer as the arguments. \
         They are validated against the schema; the first successful call wins.\n\n\
         A plan of work."
    );
}

#[tokio::test]
async fn a_plain_run_has_no_footer_and_no_finish_structured() {
    let agent = agent(|cx| cx.use_tool(clock()));
    let (settled, requests) = sent(&agent, [MockTurn::text("Done.")]).await;
    assert_eq!(user_texts(&settled), ["Go"]);
    assert_eq!(tool_names(&requests[0]), ["current_time"]);
}

#[tokio::test]
async fn a_text_reply_is_re_asked_and_a_later_finish_structured_settles() {
    let (settled, requests) = structured::<Plan>(
        &agent(|_| {}),
        [
            MockTurn::text("Here is my plan."),
            finish("c1", json!({"steps": ["one"]})),
        ],
    )
    .await;
    let settled = settled.unwrap();
    assert_eq!(settled.structured, Some(json!({"steps": ["one"]})));
    assert_eq!(requests.len(), 2);
    let reask = "You ended without calling `finish_structured`. Call it now with your answer.";
    assert_eq!(user_texts(&settled)[1], reask);
    // The re-ask follows the rejected reply in the same conversation.
    let [.., said, asked] = requests[1].chat_history.as_slice() else {
        panic!("too short: {:?}", requests[1].chat_history);
    };
    let RigMessage::Assistant(said) = said.as_ref() else {
        panic!("not the model's reply: {said:?}");
    };
    assert!(
        matches!(said.content.first(), Some(AssistantContent::Text(t)) if t.text == "Here is my plan."),
        "{said:?}"
    );
    assert_eq!(**asked, RigMessage::user(reask));
}

#[tokio::test]
async fn four_text_replies_fail_after_three_re_asks() {
    let (settled, requests) =
        structured::<Plan>(&agent(|_| {}), (0..5).map(|_| MockTurn::text("Prose."))).await;
    assert_eq!(
        settled.unwrap_err(),
        "ended without calling `finish_structured` after 3 re-asks"
    );
    assert_eq!(requests.len(), 4);
}

#[tokio::test]
async fn re_asks_count_toward_the_turn_limit() {
    let (settled, requests) = structured::<Plan>(
        &agent(|_| {}).max_turns(2),
        (0..3).map(|_| MockTurn::text("Prose.")),
    )
    .await;
    assert_eq!(
        settled.unwrap_err(),
        "ended without calling `finish_structured`: turn limit (2) reached"
    );
    assert_eq!(requests.len(), 2);
}
