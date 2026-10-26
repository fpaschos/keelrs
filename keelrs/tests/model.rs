//! `Model` as an agent function builds it.

use keelrs::{Model, Thinking};

#[test]
fn a_model_id_converts_into_a_model_with_no_thinking() {
    let expected = Model {
        id: "anthropic/claude-sonnet-5-5".to_owned(),
        thinking: None,
    };
    assert_eq!(Model::from("anthropic/claude-sonnet-5-5"), expected);
    assert_eq!(
        Model::from("anthropic/claude-sonnet-5-5".to_owned()),
        expected
    );
}

#[test]
fn thinking_sets_the_level() {
    let model = Model::new("anthropic/claude-opus-5-5").thinking(Thinking::High);
    assert_eq!(model.thinking, Some(Thinking::High));
}
