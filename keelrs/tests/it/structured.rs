//! Structured results: `ask_structured` and the reserved `finish_structured` tool.

use keelrs::testing::{Request, ScriptedModel};
use keelrs::{Agent, InstanceId};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::time::timeout;

use crate::support::{TIMEOUT, agent, clock, reason, runtime, scripted, tool_names};

const FINISH: &str = "finish_structured";

/// Asks a fresh instance of `agent` for a `T`, answered by `model`; also
/// what each turn sent the model.
async fn structured<T: DeserializeOwned + JsonSchema + 'static>(
    agent: &Agent,
    model: &ScriptedModel,
) -> (keelrs::Result<T>, Vec<Request>) {
    let runtime = runtime(agent, model);
    let instance = runtime.instance(agent, InstanceId::fresh()).unwrap();
    let result = timeout(TIMEOUT, instance.ask_structured::<T>("Go")).await;
    (result.expect("the dispatch settles"), model.requests())
}

/// [`structured`] by an agent with `current_time`, then a plain "Next" on
/// the same instance: the history that next turn sent the model. (With no
/// tool declared, Rig sends earlier tool calls as text.)
async fn then_history<T: DeserializeOwned + JsonSchema + 'static>(
    model: ScriptedModel,
) -> (keelrs::Result<T>, Vec<String>) {
    let agent = &agent(|cx| cx.use_tool(clock())).await;
    let model = model.reply("Ok.");
    let runtime = runtime(agent, &model);
    let instance = runtime.instance(agent, InstanceId::fresh()).unwrap();
    let result = timeout(TIMEOUT, instance.ask_structured::<T>("Go")).await;
    timeout(TIMEOUT, instance.ask("Next"))
        .await
        .unwrap()
        .unwrap();
    let messages = model.requests().pop().unwrap().messages;
    (result.expect("the dispatch settles"), messages)
}

/// The tool results among `messages`: `result:<text>` or `error:<text>`.
fn results(messages: &[String]) -> Vec<&str> {
    messages
        .iter()
        .map(String::as_str)
        .filter(|m| m.starts_with("result:") || m.starts_with("error:"))
        .collect()
}

/// A plan of work.
#[derive(Debug, PartialEq, Deserialize, JsonSchema)]
struct Plan {
    steps: Vec<String>,
}

fn plan(steps: &[&str]) -> Plan {
    Plan {
        steps: steps.iter().map(|s| (*s).to_owned()).collect(),
    }
}

/// A model that calls `finish_structured` once with each of `arguments`, a turn each.
fn finishing(arguments: impl IntoIterator<Item = Value>) -> ScriptedModel {
    arguments
        .into_iter()
        .enumerate()
        .fold(ScriptedModel::new(), |model, (i, arguments)| {
            model.calls([(&*format!("c{i}"), FINISH, arguments)])
        })
}

#[tokio::test]
async fn the_other_calls_of_the_finishing_reply_still_run() {
    let model = ScriptedModel::new().calls([
        ("c1", FINISH, json!({"steps": ["one"]})),
        ("c2", "current_time", json!({})),
    ]);
    let (result, history) = then_history::<Plan>(model).await;
    assert_eq!(result.unwrap(), plan(&["one"]));
    assert_eq!(results(&history), ["result:Recorded.", "result:noon"]);
    assert_eq!(
        history.last().unwrap(),
        "Next",
        "no turn after the finishing reply"
    );
}

#[tokio::test]
async fn the_first_of_two_valid_calls_in_one_reply_wins() {
    let model = ScriptedModel::new().calls([
        ("c1", FINISH, json!({"steps": ["first"]})),
        ("c2", FINISH, json!({"steps": ["second"]})),
    ]);
    let (result, history) = then_history::<Plan>(model).await;
    assert_eq!(result.unwrap(), plan(&["first"]));
    assert_eq!(
        results(&history),
        ["result:Recorded.", "result:already finished"]
    );
}

#[tokio::test]
async fn invalid_arguments_are_a_tool_error_the_model_retries_without_a_re_ask() {
    let model = finishing([json!({"steps": "one"}), json!({"steps": ["one"]})]);
    let (result, requests) = structured::<Plan>(&agent(|_| {}).await, &model).await;
    assert_eq!(result.unwrap(), plan(&["one"]));
    assert_eq!(requests.len(), 2);
    // The retry follows the tool error, with no re-ask after it.
    let retried = requests[1].messages.last().unwrap();
    assert!(retried.starts_with("error:"), "{retried}");
}

/// A finding whose note the program reads but never writes back out.
#[derive(Deserialize, JsonSchema)]
struct Finding {
    title: String,
    #[serde(skip_serializing)]
    note: String,
}

#[tokio::test]
async fn the_recorded_value_is_the_json_the_model_sent() {
    let model = finishing([json!({"title": "Typo", "note": "In the README."})]);
    let (result, _) = structured::<Finding>(&agent(|_| {}).await, &model).await;
    let finding = result.unwrap();
    assert_eq!(
        (finding.title.as_str(), finding.note.as_str()),
        ("Typo", "In the README.")
    );
}

/// A reviewer's verdict.
#[derive(Debug, PartialEq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Approve,
    RequestChanges,
}

#[tokio::test]
async fn a_t_that_is_not_an_object_travels_wrapped_as_result_and_settles_unwrapped() {
    let model = finishing([
        json!({"result": "reject"}),
        json!({"result": "request_changes"}),
    ]);
    let (result, requests) = structured::<Verdict>(&agent(|_| {}).await, &model).await;
    assert_eq!(result.unwrap(), Verdict::RequestChanges);
    let rejected = results(&requests[1].messages)[0];
    assert!(
        rejected.starts_with("error:"),
        "an unknown variant is an error"
    );
    assert_eq!(
        requests[0].tools[0]["parameters"],
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
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let model = finishing([json!({"steps": []})]);
    let (result, requests) = structured::<Plan>(&agent, &model).await;
    result.unwrap();
    assert_eq!(requests[0].messages, [format!("Go\n\n{FOOTER}")]);
    assert_eq!(requests[0].instructions, None);
    assert_eq!(
        tool_names(&requests[0]),
        ["current_time", "finish_structured"]
    );
    assert_eq!(
        requests[0].tools[1]["description"],
        "Call this once, when the task is complete, with your final answer as the arguments. \
         They are validated against the schema; the first successful call wins.\n\n\
         A plan of work."
    );
}

#[tokio::test]
async fn a_plain_run_has_no_footer_and_no_finish_structured() {
    let agent = agent(|cx| cx.use_tool(clock())).await;
    let model = ScriptedModel::new().reply("Done.");
    scripted(&agent, &model, "Go").await.unwrap();
    let request = &model.requests()[0];
    assert_eq!(request.messages, ["Go"]);
    assert_eq!(request.instructions, None);
    assert_eq!(tool_names(request), ["current_time"]);
}

const REASK: &str = "You ended without calling `finish_structured`. Call it now with your answer.";

#[tokio::test]
async fn a_text_reply_is_re_asked_and_a_later_finish_structured_settles() {
    let model = ScriptedModel::new().reply("Here is my plan.").calls([(
        "c1",
        FINISH,
        json!({"steps": ["one"]}),
    )]);
    let (result, requests) = structured::<Plan>(&agent(|_| {}).await, &model).await;
    assert_eq!(result.unwrap(), plan(&["one"]));
    assert_eq!(requests.len(), 2);
    // The re-ask follows the rejected reply in the same conversation.
    assert_eq!(
        requests[1].messages,
        [
            format!("Go\n\n{FOOTER}"),
            "Here is my plan.".to_owned(),
            REASK.to_owned()
        ]
    );
}

/// A model that replies in prose `n` times.
fn prose(n: usize) -> ScriptedModel {
    (0..n).fold(ScriptedModel::new(), |model, _| model.reply("Prose."))
}

#[tokio::test]
async fn four_text_replies_fail_after_three_re_asks() {
    let model = prose(5);
    let (result, requests) = structured::<Plan>(&agent(|_| {}).await, &model).await;
    assert_eq!(
        reason(result),
        "ended without calling `finish_structured` after 3 re-asks"
    );
    assert_eq!(requests.len(), 4);
}

#[tokio::test]
async fn re_asks_count_toward_the_turn_limit() {
    let model = prose(3);
    let agent = agent(|_| {}).await.max_turns(2);
    let (result, requests) = structured::<Plan>(&agent, &model).await;
    assert_eq!(
        reason(result),
        "ended without calling `finish_structured`: turn limit (2) reached"
    );
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn a_nullable_scalar_result_can_settle_as_null() {
    let model = finishing([json!({"result": null})]);
    let (result, requests) = structured::<Option<String>>(&agent(|_| {}).await, &model).await;
    assert_eq!(result.unwrap(), None);
    assert_eq!(
        requests[0].tools[0]["parameters"],
        json!({
            "type": "object",
            "properties": {"result": {"type": ["string", "null"]}},
            "required": ["result"],
            "additionalProperties": false,
        })
    );
}

#[tokio::test]
async fn a_unit_result_keeps_its_empty_object_arguments() {
    let model = finishing([json!({})]);
    let (result, requests) = structured::<()>(&agent(|_| {}).await, &model).await;
    result.unwrap();
    assert_eq!(
        requests[0].tools[0]["parameters"],
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    );
}

#[tokio::test]
async fn a_nullable_object_result_can_be_null_or_an_object() {
    for (value, expected) in [
        (Value::Null, None),
        (json!({"steps": ["one"]}), Some(plan(&["one"]))),
    ] {
        let model = finishing([json!({"result": value})]);
        let (result, requests) = structured::<Option<Plan>>(&agent(|_| {}).await, &model).await;
        assert_eq!(result.unwrap(), expected);
        assert_eq!(requests.len(), 1);
    }
}

#[tokio::test]
async fn nullable_array_elements_keep_their_type_checks() {
    let model = finishing([json!({"result": [123]}), json!({"result": ["one", null]})]);
    let (result, requests) = structured::<Vec<Option<String>>>(&agent(|_| {}).await, &model).await;
    assert_eq!(result.unwrap(), [Some("one".to_owned()), None]);
    let refused = results(&requests[1].messages)[0];
    assert!(
        refused.starts_with("error:"),
        "a number is not a nullable string"
    );
    assert_eq!(requests.len(), 2);
}

#[derive(Deserialize, JsonSchema)]
struct Note {
    text: Option<String>,
}

#[tokio::test]
async fn an_optional_result_field_accepts_null_or_omission() {
    for value in [json!({"text": null}), json!({})] {
        let model = finishing([value]);
        let (result, _) = structured::<Note>(&agent(|_| {}).await, &model).await;
        assert_eq!(result.unwrap().text, None);
    }
}

#[derive(Deserialize, JsonSchema)]
struct Tree {
    #[schemars(length(min = 1))]
    label: String,
    children: Vec<Self>,
}

#[tokio::test]
async fn a_wrapped_recursive_result_checks_its_descendants_schema() {
    let invalid = json!([{"label": "root", "children": [{"label": "", "children": []}]}]);
    let valid = json!([{"label": "root", "children": [{"label": "leaf", "children": []}]}]);
    let model = finishing([json!({"result": invalid}), json!({"result": valid})]);
    let (result, requests) = structured::<Vec<Tree>>(&agent(|_| {}).await, &model).await;
    let refused = results(&requests[1].messages)[0];
    assert!(
        refused.starts_with("error:"),
        "a descendant's empty label is refused"
    );
    let trees = result.unwrap();
    assert_eq!(trees[0].label, "root");
    assert_eq!(trees[0].children[0].label, "leaf");
    assert_eq!(requests.len(), 2);
}
