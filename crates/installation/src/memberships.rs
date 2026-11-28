//! Membership observation and serialization for hook rules, agent access and agent merges.
use crate::{
    error::{Error, Result},
    service::Service,
};
use cairn_contracts::{MergeAgentsInput, MergeAgentsOutput};
use cairn_protocol::TaskAuthorPolicy;

#[derive(Default)]
pub struct Memberships {
    pub(crate) authors: tokio::sync::Mutex<()>,
    /// When the Beacon last confirmed the memberships, reconciled with account access.
    verified: std::sync::Mutex<Option<(std::time::Instant, bool)>>,
    /// Wakes requests waiting for a confirmation made by the holder of `authors`.
    observed: tokio::sync::Notify,
}

#[derive(Clone, Copy)]
enum MembershipFreshness {
    Within(std::time::Duration),
    Since(std::time::Instant),
}

impl Memberships {
    /// The next membership-dependent read confirms the memberships first.
    #[cfg(test)]
    pub(crate) fn expire_membership_check(&self) {
        *self.verified.lock().unwrap() = None;
    }

    pub(crate) fn membership_verified_within(&self, age: std::time::Duration) -> bool {
        self.membership_verified(MembershipFreshness::Within(age))
    }

    fn membership_verified(&self, freshness: MembershipFreshness) -> bool {
        self.verified
            .lock()
            .unwrap()
            .is_some_and(|(verified, _)| match freshness {
                MembershipFreshness::Within(age) => verified.elapsed() < age,
                MembershipFreshness::Since(started) => verified >= started,
            })
    }

    /// A migrated schedule needs a real installation membership observation
    /// before an unverified creator is treated as a member.
    pub(crate) async fn confirm_installation_memberships(&self, s: &Service) -> Result<bool> {
        self.verify_membership(s, std::time::Duration::from_secs(60))
            .await?;
        Ok(self
            .verified
            .lock()
            .unwrap()
            .is_some_and(|(_, claimed)| claimed))
    }

    /// Read-only lists may reuse a recent membership check.
    pub(crate) async fn verify_membership(
        &self,
        s: &Service,
        age: std::time::Duration,
    ) -> Result<()> {
        self.verify_membership_freshness(s, MembershipFreshness::Within(age))
            .await
    }

    /// Directing writes share only a check that began during this request's preflight.
    pub(crate) async fn verify_membership_since(
        &self,
        s: &Service,
        started: std::time::Instant,
    ) -> Result<()> {
        self.verify_membership_freshness(s, MembershipFreshness::Since(started))
            .await
    }

    /// Observations stay under `authors`, consistent with grants and rule saves. A request
    /// can use the holder's check without waiting for its whole work, but an older in-flight
    /// observation cannot authorize a fresh write. Keep its lock queue position and total
    /// wait bound while waiting for a qualifying observation or for the lock itself.
    async fn verify_membership_freshness(
        &self,
        s: &Service,
        freshness: MembershipFreshness,
    ) -> Result<()> {
        const HOLDER_CHECK: std::time::Duration = std::time::Duration::from_secs(10);

        let authors = tokio::time::timeout(HOLDER_CHECK, async {
            let authors = self.authors.lock();
            tokio::pin!(authors);
            loop {
                let observed = self.observed.notified();
                tokio::pin!(observed);
                observed.as_mut().enable();
                if self.membership_verified(freshness) {
                    return None;
                }
                tokio::select! {
                    () = observed.as_mut() => {}
                    authors = authors.as_mut() => return Some(authors),
                }
            }
        })
        .await
        .map_err(|_| Error::unavailable("Memberships could not be confirmed right now."))?;

        if let Some(authors) = authors {
            // Another request may have checked while this one waited.
            if !self.membership_verified(freshness) {
                self.observe(s).await?;
            }
            drop(authors);
        }
        Ok(())
    }

    /// Callers hold `authors`.
    pub(crate) async fn observe(&self, s: &Service) -> Result<Option<TaskAuthorPolicy>> {
        let checked = std::time::Instant::now();
        let policy = crate::relay::task_author_policy(s).await?;
        let observed = policy.clone();
        s.store
            .transaction(move |db| {
                // Rules and account access to agents follow the same memberships.
                if let Some(policy) = &observed {
                    crate::rules::reconcile(db, policy)?;
                }
                crate::agent_access::reconcile(db, observed.as_ref())
            })
            .await?;
        *self.verified.lock().unwrap() = Some((checked, policy.is_some()));
        self.observed.notify_waiters();
        Ok(policy)
    }

    pub(crate) async fn merge_agents_live(
        &self,
        s: &Service,
        input: MergeAgentsInput,
        authorize: crate::authorization::Authorization,
        resource_authority: Option<crate::capabilities::plugin_packages::Authority>,
    ) -> Result<MergeAgentsOutput> {
        authorize.recheck().await?;
        let _authors = self.authors.lock().await;
        if !input.confirm {
            return Err(Error::bad("Confirm the merge."));
        }

        // An active worker can still be settling a terminal run. Keep the same
        // worker/admission ordering as the profile merge while commitments are held.
        let active = s.worker.active.lock().await;
        authorize.recheck().await?;
        let coverage = crate::plugins::grants::check_coverage_authorized(
            s,
            &input.target_id.to_string(),
            &input.source_id.to_string(),
            resource_authority.clone(),
        )
        .await
        .map_err(|error| {
            if error.status == 403 {
                Error::conflict(error.message)
            } else {
                error
            }
        })?;
        authorize.recheck().await?;
        let executing = active.keys().cloned().collect::<Vec<_>>();
        let result = s
            .store
            .transaction(move |db| {
                authorize.authorize(db)?;
                coverage.authorize(db)?;
                if let Some(authority) = &resource_authority {
                    authority.authorize(db)?;
                }
                crate::agent_merge::merge(db, &input, &executing)
            })
            .await?;
        s.worker.notify();
        drop(active);

        if let Err(error) = self.observe(s).await {
            tracing::warn!(
                status = error.status,
                "Rule and access reconciliation awaits the next membership check after a merge"
            );
        }
        Ok(result)
    }
}

impl Service {
    pub async fn synchronize_task_authors(&self) -> Result<()> {
        let _authors = self.memberships.authors.lock().await;
        self.memberships.observe(self).await?;
        Ok(())
    }
}
