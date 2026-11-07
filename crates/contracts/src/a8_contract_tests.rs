use crate::*;

#[test]
fn questions_waits_and_delegation_share_the_wire_corpus() {
    let corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/questions-waits-contracts.json"
    ))
    .unwrap();
    for value in corpus["asks"].as_array().unwrap() {
        let command: AskQuestionsInput = serde_json::from_value(value.clone()).unwrap();
        validate_question_fields(&command.questions).unwrap();
    }
    for value in corpus["invalidAsks"]
        .as_array()
        .unwrap()
        .iter()
        .chain(corpus["serverInvalidAsks"].as_array().unwrap())
    {
        let valid = serde_json::from_value::<AskQuestionsInput>(value.clone())
            .is_ok_and(|c| validate_question_fields(&c.questions).is_ok());
        assert!(!valid, "{value}");
    }
    for value in corpus["questions"].as_array().unwrap() {
        let question: ChatQuestion = serde_json::from_value(value.clone()).unwrap();
        assert!(question.is_private());
        validate_question_fields(&question.fields).unwrap();
    }
    for value in corpus["answers"].as_array().unwrap() {
        let mut answer: QuestionAnswerInput = serde_json::from_value(value.clone()).unwrap();
        answer.validate().unwrap();
        assert_eq!(serde_json::to_value(answer).unwrap(), *value);
    }
    for value in corpus["wakeups"].as_array().unwrap() {
        let wakeup: ChatWakeup = serde_json::from_value(value.clone()).unwrap();
        let encoded = serde_json::to_value(&wakeup).unwrap();
        assert_eq!(
            serde_json::from_value::<ChatWakeup>(encoded).unwrap(),
            wakeup
        );
        if let Some(result) = &wakeup.progress.observation {
            assert_eq!(
                result.members.as_ref().unwrap()[0]
                    .result
                    .as_ref()
                    .unwrap()
                    .plugin_result
                    .as_ref()
                    .unwrap()
                    .0["runId"],
                40972562294u64
            );
        }
    }
    for value in corpus["waitOutputs"].as_array().unwrap() {
        let output: WaitForOutput = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(output).unwrap(), *value);
    }
    for value in corpus["delegates"].as_array().unwrap() {
        let command: DelegateTaskInput = serde_json::from_value(value.clone()).unwrap();
        command.validate().unwrap();
        assert_eq!(serde_json::to_value(command).unwrap(), *value);
    }
    for value in corpus["invalidDelegates"].as_array().unwrap() {
        assert!(
            !serde_json::from_value::<DelegateTaskInput>(value.clone())
                .is_ok_and(|c| c.validate().is_ok()),
            "{value}"
        );
    }
}
