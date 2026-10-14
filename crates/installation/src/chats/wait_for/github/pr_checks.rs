//! Required status checks, pinned to a PR head rather than one workflow.
use super::request;
use crate::{
    error::{Error, Result},
    process::Environment,
    service::Service,
    validation::text,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

// Both queries bind the repository and PR explicitly. No branch/current-directory inference.
const IDENTITY: &str = r#"
query($owner:String!,$name:String!,$number:Int!) {
  repository(owner:$owner,name:$name) {
    pullRequest(number:$number) {
      number headRefOid state baseRefName
      baseRef { branchProtectionRule {
        requiresStatusChecks requiredStatusChecks { context app { databaseId } }
      } }
    }
  }
}"#;

const CHECKS: &str = r#"
query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
  repository(owner:$owner,name:$name) {
    pullRequest(number:$number) {
      number headRefOid state baseRefName
      baseRef { branchProtectionRule {
        requiresStatusChecks requiredStatusChecks { context app { databaseId } }
      } }
      commits(last:1) { nodes { commit {
        oid statusCheckRollup { contexts(first:100,after:$cursor) {
          totalCount pageInfo { hasNextPage endCursor }
          nodes {
            __typename
            ...on CheckRun {
              name status conclusion isRequired(pullRequestNumber:$number)
              checkSuite { commit { oid } app { databaseId } }
            }
            ...on StatusContext {
              context state isRequired(pullRequestNumber:$number)
            }
          }
        } }
      } } }
    }
  }
}"#;

const MAX_CHECK_PAGES: usize = 5;
const MAX_RULE_PAGES: usize = 2;

fn invalid() -> Error {
    Error::bad_gateway("Invalid GitHub PR checks response.")
}

async fn query(
    s: &Service,
    env: &Environment,
    repository: &str,
    condition: &Value,
    query: &str,
    cursor: Option<&str>,
) -> Result<Value> {
    let (owner, name) = repository.split_once('/').ok_or_else(invalid)?;
    let mut args = vec![
        "api".into(),
        "--hostname".into(),
        "github.com".into(),
        "graphql".into(),
        "-f".into(),
        format!("query={query}"),
        "-f".into(),
        format!("owner={owner}"),
        "-f".into(),
        format!("name={name}"),
        "-F".into(),
        format!("number={}", condition["prNumber"]),
    ];
    if let Some(cursor) = cursor {
        args.extend(["-f".into(), format!("cursor={cursor}")]);
    }
    let response = request(s, env, &args).await?;
    let pr = &response["data"]["repository"]["pullRequest"];
    if !pr.is_object() {
        return Err(invalid());
    }
    Ok(pr.clone())
}

fn identity(pr: &Value, repository: &str, condition: &Value) -> Result<Option<Value>> {
    let head = text(pr, "headRefOid");
    if pr["number"] != condition["prNumber"]
        || head.len() != 40
        || !head.bytes().all(|c| c.is_ascii_hexdigit())
        || !["OPEN", "CLOSED", "MERGED"].contains(&text(pr, "state"))
        || text(pr, "baseRefName").is_empty()
    {
        return Err(invalid());
    }
    let state = if head.to_ascii_lowercase() != condition["headSha"] {
        "superseded"
    } else if pr["state"] != "OPEN" {
        "closed"
    } else {
        return Ok(None);
    };
    Ok(Some(json!({
        "state": state,
        "type": "github_pr_checks",
        "repository": repository,
        "prNumber": condition["prNumber"],
        "headSha": condition["headSha"],
        "currentHeadSha": head.to_ascii_lowercase(),
        "pullRequestState": pr["state"],
    })))
}

fn requirement(context: &Value, app: &Value) -> Result<(String, Option<i64>)> {
    let context = context
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= 256)
        .ok_or_else(invalid)?;
    let app = if app.is_null() || app == -1 {
        None
    } else {
        Some(app.as_i64().filter(|id| *id > 0).ok_or_else(invalid)?)
    };
    Ok((context.to_owned(), app))
}

fn classic_requirements(pr: &Value) -> Result<BTreeSet<(String, Option<i64>)>> {
    let mut required = BTreeSet::new();
    let rule = &pr["baseRef"]["branchProtectionRule"];
    if rule.is_null() {
        return Ok(required);
    }
    if !rule["requiresStatusChecks"].as_bool().ok_or_else(invalid)? {
        return Ok(required);
    }
    for check in rule["requiredStatusChecks"]
        .as_array()
        .ok_or_else(invalid)?
    {
        required.insert(requirement(&check["context"], &check["app"]["databaseId"])?);
        if required.len() > 500 {
            return Err(Error::conflict(
                "This PR exceeds the supported limit of required checks.",
            ));
        }
    }
    Ok(required)
}

async fn rules(
    s: &Service,
    env: &Environment,
    repository: &str,
    branch: &str,
) -> Result<BTreeSet<(String, Option<i64>)>> {
    let mut url = reqwest::Url::parse(&format!(
        "https://api.github.com/repos/{repository}/rules/branches"
    ))
    .map_err(|_| invalid())?;
    url.path_segments_mut()
        .map_err(|()| invalid())?
        .push(branch);
    let mut required = BTreeSet::new();
    for page in 1..=MAX_RULE_PAGES {
        let endpoint = format!(
            "{}?per_page=100&page={page}",
            url.path().trim_start_matches('/')
        );
        let args = ["api", "--hostname", "github.com", &endpoint].map(str::to_owned);
        let response = request(s, env, &args).await?;
        let rules = response.as_array().ok_or_else(invalid)?;
        if rules.len() > 100 {
            return Err(invalid());
        }
        for rule in rules {
            if rule["type"] == "workflows" {
                return Err(Error::conflict(
                    "Required workflow rules are not supported by this wait condition.",
                ));
            }
            if rule["type"] != "required_status_checks" {
                continue;
            }
            for check in rule["parameters"]["required_status_checks"]
                .as_array()
                .ok_or_else(invalid)?
            {
                required.insert(requirement(&check["context"], &check["integration_id"])?);
                if required.len() > 500 {
                    return Err(Error::conflict(
                        "This PR exceeds the supported limit of required checks.",
                    ));
                }
            }
        }
        if rules.len() < 100 {
            return Ok(required);
        }
    }
    Err(Error::conflict(
        "This PR exceeds the supported limit of branch rules.",
    ))
}

pub(super) async fn observe(
    s: &Service,
    repository: &str,
    env: &Environment,
    condition: &Value,
) -> Result<Option<Value>> {
    let initial = query(s, env, repository, condition, CHECKS, None).await?;
    if let Some(result) = identity(&initial, repository, condition)? {
        return Ok(Some(result));
    }
    let mut required = classic_requirements(&initial)?;
    let active_rules = rules(s, env, repository, text(&initial, "baseRefName")).await?;
    required.extend(active_rules.iter().cloned());
    if required.len() > 500 {
        return Err(Error::conflict(
            "This PR exceeds the supported limit of required checks.",
        ));
    }
    let mut page = initial.clone();
    let mut checks = Vec::new();
    let mut cursors = BTreeSet::new();
    for index in 0..MAX_CHECK_PAGES {
        if let Some(result) = identity(&page, repository, condition)? {
            return Ok(Some(result));
        }
        if page["baseRefName"] != initial["baseRefName"]
            || classic_requirements(&page)? != classic_requirements(&initial)?
        {
            return Ok(None);
        }
        let commit = &page["commits"]["nodes"][0]["commit"];
        if commit["oid"] != condition["headSha"] {
            return Err(invalid());
        }
        let rollup = &commit["statusCheckRollup"];
        if rollup.is_null() {
            if index > 0 {
                return Ok(None);
            }
            break;
        }
        let contexts = &rollup["contexts"];
        let total = contexts["totalCount"].as_u64().ok_or_else(invalid)?;
        if total > (MAX_CHECK_PAGES * 100) as u64 {
            return Err(Error::conflict(
                "This PR exceeds the supported limit of 500 checks.",
            ));
        }
        let nodes = contexts["nodes"].as_array().ok_or_else(invalid)?;
        if nodes.len() > 100 {
            return Err(invalid());
        }
        checks.extend(nodes.iter().cloned());
        let more = contexts["pageInfo"]["hasNextPage"]
            .as_bool()
            .ok_or_else(invalid)?;
        if !more {
            if checks.len() != total as usize {
                return Ok(None);
            }
            break;
        }
        let cursor = text(&contexts["pageInfo"], "endCursor");
        if cursor.is_empty() || cursor.len() > 1024 || !cursors.insert(cursor.to_owned()) {
            return Err(invalid());
        }
        if index + 1 == MAX_CHECK_PAGES {
            return Err(Error::conflict(
                "This PR exceeds the supported limit of check pages.",
            ));
        }
        page = query(s, env, repository, condition, CHECKS, Some(cursor)).await?;
    }
    if rules(s, env, repository, text(&initial, "baseRefName")).await? != active_rules {
        return Ok(None);
    }
    // Fence pushes/closure/base changes after fetching pages and rechecking rules.
    let current = query(s, env, repository, condition, IDENTITY, None).await?;
    if let Some(result) = identity(&current, repository, condition)? {
        return Ok(Some(result));
    }
    if current["baseRefName"] != initial["baseRefName"]
        || classic_requirements(&current)? != classic_requirements(&initial)?
    {
        return Ok(None);
    }
    evaluate(repository, condition, &required, &checks)
}

fn evaluate(
    repository: &str,
    condition: &Value,
    required: &BTreeSet<(String, Option<i64>)>,
    checks: &[Value],
) -> Result<Option<Value>> {
    let mut selected = Vec::new();
    let mut present = BTreeSet::new();
    for check in checks {
        let is_required = check["isRequired"].as_bool().ok_or_else(invalid)?;
        if !is_required {
            continue;
        }
        let is_run = check["__typename"] == "CheckRun";
        if !is_run && check["__typename"] != "StatusContext" {
            return Err(invalid());
        }
        let name = if is_run {
            &check["name"]
        } else {
            &check["context"]
        };
        let app = if is_run {
            &check["checkSuite"]["app"]["databaseId"]
        } else {
            &Value::Null
        };
        let mut binding = requirement(name, app)?;
        if !is_run {
            let bindings: Vec<_> = required
                .iter()
                .filter(|(name, _)| name == &binding.0)
                .collect();
            match bindings.as_slice() {
                [only] => binding.1 = only.1,
                [] => {}
                _ => {
                    return Err(Error::conflict(
                        "A required legacy status has ambiguous application bindings. Inspect the PR checks directly.",
                    ));
                }
            }
        }
        if is_run && check["checkSuite"]["commit"]["oid"] != condition["headSha"] {
            return Err(invalid());
        }
        present.insert(binding.clone());
        let state = if is_run {
            let status = text(check, "status");
            if status == "COMPLETED" {
                text(check, "conclusion")
            } else if ["QUEUED", "IN_PROGRESS", "WAITING", "PENDING", "REQUESTED"].contains(&status)
            {
                "PENDING"
            } else {
                return Err(invalid());
            }
        } else {
            text(check, "state")
        };
        let bucket = match state {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => "success",
            "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" | "STALE" => {
                "failure"
            }
            "CANCELLED" => "cancelled",
            "EXPECTED" | "PENDING" => "pending",
            _ => return Err(invalid()),
        };
        selected.push(json!({
            "name": binding.0,
            "type": if is_run { "check_run" } else { "status_context" },
            "state": bucket,
        }));
    }
    let mut missing: Vec<_> = required
        .iter()
        .filter(|(name, app)| {
            !present
                .iter()
                .any(|(found, source)| found == name && (app.is_none() || source == app))
        })
        .map(|(name, _)| name.clone())
        .collect();
    let failure = selected.iter().any(|check| check["state"] == "failure");
    let cancelled = selected.iter().any(|check| check["state"] == "cancelled");
    let pending = selected
        .iter()
        .filter(|check| check["state"] == "pending")
        .count()
        + missing.len();
    if !failure && !cancelled && pending > 0 {
        return Ok(None);
    }
    let conclusion = if failure {
        "failure"
    } else if cancelled {
        "cancelled"
    } else if selected.is_empty() && required.is_empty() {
        "no_required_checks"
    } else {
        "success"
    };
    let priority = |check: &Value| match text(check, "state") {
        "failure" => 0,
        "cancelled" => 1,
        "pending" => 2,
        _ => 3,
    };
    selected.sort_by(|a, b| {
        priority(a)
            .cmp(&priority(b))
            .then_with(|| text(a, "name").cmp(text(b, "name")))
    });
    let count = selected.len();
    let missing_count = missing.len();
    selected.truncate(100);
    missing.truncate(100 - selected.len());
    Ok(Some(json!({
        "state": "completed",
        "type": "github_pr_checks",
        "repository": repository,
        "prNumber": condition["prNumber"],
        "headSha": condition["headSha"],
        "conclusion": conclusion,
        "requiredCount": count + missing_count,
        "pendingCount": pending,
        "checks": selected,
        "checksTruncated": count + missing_count > 100,
        "missingCheckCount": missing_count,
        "missingChecks": missing,
    })))
}
