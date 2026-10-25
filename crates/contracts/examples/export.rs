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
    export::<CreateProjectInput>(root, &mut generator, Contract::Deserialize);
    export::<ReplaceProjectInput>(root, &mut generator, Contract::Deserialize);
    export::<ProjectInput>(root, &mut generator, Contract::Deserialize);
    export::<ListProjectsInput>(root, &mut generator, Contract::Deserialize);
    export::<ListProjectsOutput>(root, &mut generator, Contract::Serialize);
    export::<Project>(root, &mut generator, Contract::Serialize);
    export::<DeleteProjectOutput>(root, &mut generator, Contract::Serialize);
    let projects: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/project-contracts.json"
    ))
    .unwrap();
    let creates: Vec<CreateProjectInput> = projects["creates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| serde_json::from_value(value.clone()).unwrap())
        .collect();
    let views: Vec<Project> = creates
        .iter()
        .enumerate()
        .map(|(index, input)| Project {
            id: ProjectId(uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap()),
            settings: input.settings.clone(),
            origin: "forge.example:team/project.git".into(),
            source: if index == 0 {
                None
            } else {
                Some(ProjectSource {
                    plugin_id: "forge".into(),
                    source: "repositories".into(),
                    instance: "00000000-0000-4000-8000-000000000003".into(),
                    id: "team/project".into(),
                })
            },
            revision: "00000000-0000-4000-8000-000000000004".into(),
            created_at: 9007199254740991,
        })
        .collect();
    fs::write(
        root.join("project-wire.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "creates": creates, "views": views }))
            .unwrap()
            + "\n",
    )
    .unwrap();
    let clients = serde_json::json!([
        { "name": "listProjects", "input": "ListProjectsInput", "output": "ListProjectsOutput", "method": "GET", "path": "/projects" },
        { "name": "getProject", "input": "ProjectInput", "output": "Project", "method": "GET", "path": "/projects/{projectId}" },
        { "name": "createProject", "input": "CreateProjectInput", "output": "Project", "method": "POST", "path": "/projects" },
        { "name": "replaceProject", "input": "ReplaceProjectInput", "output": "Project", "method": "PUT", "path": "/projects/{projectId}" },
        { "name": "deleteProject", "input": "ProjectInput", "output": "DeleteProjectOutput", "method": "DELETE", "path": "/projects/{projectId}" }
    ]);
    fs::write(
        root.join("project-clients.json"),
        serde_json::to_string_pretty(&clients).unwrap() + "\n",
    )
    .unwrap();
    export::<ListAccountMethodsOutput>(root, &mut generator, Contract::Serialize);
    fs::write(
        root.join("list-account-methods.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "path": LIST_ACCOUNT_METHODS_PATH }))
            .unwrap()
            + "\n",
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
