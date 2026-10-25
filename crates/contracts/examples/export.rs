use cairn_contracts::{beacon::*, *};
use schemars::{JsonSchema, generate::SchemaSettings};
use std::{fs, path::Path};
use ts_rs::TS;

fn export<T: JsonSchema + TS + 'static>(
    root: &Path,
    generator: &mut schemars::SchemaGenerator,
    contract: Contract,
) {
    let schema = cairn_contracts::schema_for::<T>(contract);
    let config = ts_rs::Config::default().with_out_dir(root.join("typescript"));
    fs::write(
        root.join(format!("{}.json", T::ident(&config))),
        serde_json::to_string_pretty(&schema).unwrap() + "\n",
    )
    .unwrap();
    T::export_all(&config).unwrap();
    generator.subschema_for::<T>();
}

fn main() {
    let root = std::env::args().nth(1).expect("output directory");
    let root = Path::new(&root);
    fs::create_dir_all(root).unwrap();
    let mut generator = SchemaSettings::draft07().for_serialize().into_generator();
    export::<ResumeRunInput>(root, &mut generator, Contract::Deserialize);
    export::<ResumeRunOutput>(root, &mut generator, Contract::Serialize);
    export::<QualificationFixture>(root, &mut generator, Contract::Serialize);
    export::<ListAccountMethodsInput>(root, &mut generator, Contract::Deserialize);
    export::<ListAccountMethodsOutput>(root, &mut generator, Contract::Serialize);
    fs::write(
        root.join("list-account-methods.json"),
        serde_json::to_string_pretty(&list_account_methods_descriptor()).unwrap() + "\n",
    )
    .unwrap();
    // Canonical wire values come from the Rust serializer, including omission/null semantics.
    let corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/resume-contracts.json"
    ))
    .unwrap();
    let wire: Vec<_> = corpus["valid"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| {
            let model: QualificationFixture = serde_json::from_value(value.clone()).unwrap();
            serde_json::to_value(model).unwrap()
        })
        .collect();
    fs::write(
        root.join("qualification-wire.json"),
        serde_json::to_string_pretty(&wire).unwrap() + "\n",
    )
    .unwrap();
    let descriptor = serde_json::json!({
        "id": RESUME_RUN_ID,
        "scope": RESUME_RUN_SCOPE,
        "path": RESUME_RUN_PATH,
        "description": RESUME_RUN_DESCRIPTION,
    });
    fs::write(
        root.join("resume-run.json"),
        serde_json::to_string_pretty(&descriptor).unwrap() + "\n",
    )
    .unwrap();
    let schema = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "definitions": generator.take_definitions(true),
    });
    fs::write(
        root.join("models.json"),
        serde_json::to_string_pretty(&schema).unwrap() + "\n",
    )
    .unwrap();
}
