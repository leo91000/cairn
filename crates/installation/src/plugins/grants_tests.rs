use super::*;
use serde_json::json;

#[test]
fn instruction_grants_require_nonblank_declared_content() {
    let manifest: cairn_plugin_api::Manifest = serde_json::from_value(json!({
        "id": "example",
        "name": "Example",
        "version": "1",
        "apiVersion": 2,
        "capabilities": [],
        "instructions": " \n\t"
    }))
    .unwrap();
    let grant = PluginContributionGrant {
        kind: Kind::Instructions,
        name: String::new(),
        resources: Vec::new(),
    };

    assert!(!declared(&manifest, &grant));
    assert!(declared(
        &cairn_plugin_api::Manifest {
            instructions: "Guide".into(),
            ..manifest
        },
        &grant,
    ));
}

#[tokio::test]
async fn legacy_full_access_is_per_instance_and_supplier_removal_never_falls_back() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(root.path()).unwrap();
    store
        .transaction(|db| {
            let first = "00000000-0000-4000-8000-000000000010";
            let second = "00000000-0000-4000-8000-000000000011";
            let mut agent = crate::agent_fixture::profile(
                json!({"id":"00000000-0000-4000-8000-000000000012"}),
            )?;
            for (id, name) in [(first, "First"), (second, "Second")] {
                let mut instance = super::super::connections::Connection::draft(
                    id,
                    "github",
                    json!({}),
                    Vec::new(),
                );
                instance.name = name.into();
                instance.revision = "r1".into();
                instance.enabled = true;
                db.save_plugin_instance(&instance)?;
            }
            agent.settings.plugins = vec![first.into(), second.into()];
            db.save_agent(&agent)?;
            let id = agent.id.to_string();
            assert_eq!(db.plugin_credential_supplier(&id, "github")?, None);
            db.choose_setup_credential_supplier(&id, first)?;
            assert_eq!(
                db.plugin_credential_supplier(&id, "github")?.as_deref(),
                Some(first)
            );
            db.save_agent(&agent)?;
            assert_eq!(
                db.plugin_credential_supplier(&id, "github")?.as_deref(),
                Some(first)
            );
            assert!(
                db.plugin_contribution(&id, first, Kind::Hook, "future-hook")?
                    .is_some()
            );
            assert!(
                db.plugin_contribution(&id, second, Kind::Tool, "future-tool")?
                    .is_some()
            );
            agent.settings.plugins.retain(|instance| instance != first);
            db.save_agent(&agent)?;
            assert_eq!(db.plugin_credential_supplier(&id, "github")?, None);
            assert!(
                db.plugin_contribution(&id, first, Kind::Tool, "future-tool")?
                    .is_none()
            );
            assert!(
                db.plugin_contribution(&id, second, Kind::Tool, "future-tool")?
                    .is_some()
            );
            db.save_agent(&agent)?;
            assert_eq!(db.plugin_credential_supplier(&id, "github")?, None);
            Ok(())
        })
        .await
        .unwrap();
}

#[test]
fn reductions_compare_contributions_and_every_restricted_type() {
    let grant = |name: &str, resources: Vec<(&str, &str)>| Access::Selected {
        contributions: vec![PluginContributionGrant {
            kind: Kind::Tool,
            name: name.into(),
            resources: resources
                .into_iter()
                .map(|(kind, id)| PluginResourceRef {
                    resource_type: kind.into(),
                    id: id.into(),
                })
                .collect(),
        }],
    };
    let current = grant("post", vec![("channel", "support"), ("channel", "ops")]);
    assert!(access_covers(&Access::Full, &current));
    assert!(!access_covers(&current, &Access::Full));
    assert!(access_covers(
        &current,
        &grant("post", vec![("channel", "support")])
    ));
    assert!(!access_covers(&current, &grant("post", vec![])));
    assert!(!access_covers(
        &current,
        &grant("read", vec![("channel", "support")])
    ));
    assert!(!access_covers(
        &current,
        &grant("post", vec![("channel", "general")])
    ));
}
