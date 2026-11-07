//! Complete current attempt setup for neighboring-domain tests.
#![allow(dead_code)]

use cairn_installation::{
    config::{id, now},
    error::Result,
    nodes::attempts::Attempt,
    store::{Db, Store, merge},
};
use serde_json::{Value, json};

pub fn attempt(value: &Value) -> Result<Attempt> {
    let mut complete = json!({"id":id(),"runId":id(),"nodeId":cairn_installation::nodes::LOCAL_NODE_ID,"role":"execution","resources":{"cpu":2,"memoryMiB":4096,"diskMiB":32768},"runtimeId":"fixture","createdAt":now(),"released":false});
    let mut value = value.clone();
    if let Some(resources) = value.get_mut("resources") {
        let mut defaults = complete["resources"].clone();
        merge(&mut defaults, resources);
        *resources = defaults;
    }
    merge(&mut complete, &value);
    Ok(serde_json::from_value(complete)?)
}

pub trait DbAttemptFixture {
    fn put_attempt_fixture(&self, value: &Value) -> Result<Value>;

    fn attempt_fixture(&self, id: &str) -> Result<Option<Value>>;

    fn attempts_fixture(&self) -> Result<Vec<Value>>;
}

impl DbAttemptFixture for Db<'_> {
    fn put_attempt_fixture(&self, value: &Value) -> Result<Value> {
        let attempt = attempt(value)?;
        self.save_attempt(&attempt)?;
        Ok(serde_json::to_value(attempt)?)
    }

    fn attempt_fixture(&self, id: &str) -> Result<Option<Value>> {
        self.attempt(id)?
            .map(|a| Ok(serde_json::to_value(a)?))
            .transpose()
    }

    fn attempts_fixture(&self) -> Result<Vec<Value>> {
        self.attempts()?
            .into_iter()
            .map(|a| Ok(serde_json::to_value(a)?))
            .collect()
    }
}

pub trait StoreAttemptFixture {
    fn put_attempt_fixture(&self, value: Value)
    -> impl std::future::Future<Output = Result<Value>>;

    fn attempt_fixture(&self, id: &str)
    -> impl std::future::Future<Output = Result<Option<Value>>>;

    fn attempts_fixture(&self) -> impl std::future::Future<Output = Result<Vec<Value>>>;
}

impl StoreAttemptFixture for Store {
    async fn put_attempt_fixture(&self, value: Value) -> Result<Value> {
        self.transaction(move |db| db.put_attempt_fixture(&value))
            .await
    }

    async fn attempt_fixture(&self, id: &str) -> Result<Option<Value>> {
        let id = id.to_owned();
        self.read(move |db| db.attempt_fixture(&id)).await
    }

    async fn attempts_fixture(&self) -> Result<Vec<Value>> {
        self.read(|db| db.attempts_fixture()).await
    }
}
