//! Credential-scoped admission and pacing, without sleeping inside a plugin handler.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

#[derive(Default)]
pub struct Paces(Mutex<HashMap<String, Arc<Mutex<Gate>>>>);

#[derive(Debug)]
struct Gate {
    next: Instant,
    sending: bool,
    budget: Option<Budget>,
}

#[derive(Debug)]
struct Budget {
    /// Conservative server balance, less completed sends. Reservations are separate
    /// so cancellation cannot add capacity after a newer, lower observation.
    remaining: u64,
    reserved: u64,
    reset: Instant,
}

#[derive(Debug)]
pub struct Ticket {
    gate: Arc<Mutex<Gate>>,
    gap: Duration,
    reserved: Option<Instant>,
}

pub struct Sending(Ticket);

pub enum Ready {
    Send(Sending),
    Wait(Ticket),
    /// The observation expired or its balance was reduced: ask for a fresh budget.
    Expired,
}

impl Paces {
    pub fn reserve(
        &self,
        key: String,
        gap: Duration,
        budget: Option<(u64, Duration)>,
    ) -> Result<Ticket, Duration> {
        let now = Instant::now();
        let gate = {
            let mut paces = self.0.lock().unwrap();
            paces.retain(|_, gate| {
                if Arc::strong_count(gate) > 1 {
                    return true;
                }
                let gate = gate.lock().unwrap();
                gate.next > now
                    || gate
                        .budget
                        .as_ref()
                        .is_some_and(|budget| budget.reset > now)
            });
            paces
                .entry(key)
                .or_insert_with(|| {
                    Arc::new(Mutex::new(Gate {
                        next: now,
                        sending: false,
                        budget: None,
                    }))
                })
                .clone()
        };
        let reserved = {
            let mut state = gate.lock().unwrap();
            if let Some((remaining, reset_after)) = budget {
                match state.budget.as_mut() {
                    Some(budget) if budget.reset > now => {
                        budget.remaining = budget.remaining.min(remaining);
                    }
                    _ => {
                        state.budget = Some(Budget {
                            remaining,
                            reserved: 0,
                            reset: now + reset_after,
                        });
                    }
                }
                let budget = state.budget.as_mut().unwrap();
                if budget.remaining <= budget.reserved {
                    return Err(budget.reset.saturating_duration_since(now));
                }
                budget.reserved += 1;
                Some(budget.reset)
            } else {
                None
            }
        };
        Ok(Ticket {
            gate,
            gap,
            reserved,
        })
    }
}

impl Ticket {
    pub fn at(&self) -> Instant {
        let gate = self.gate.lock().unwrap();
        let at = if gate.sending {
            Instant::now() + Duration::from_millis(10)
        } else {
            gate.next
        };
        self.reserved.map_or(at, |reset| at.min(reset))
    }

    pub fn start(self) -> Ready {
        {
            let mut gate = self.gate.lock().unwrap();
            if let Some(reset) = self.reserved
                && (reset <= Instant::now()
                    || gate
                        .budget
                        .as_ref()
                        .is_none_or(|budget| budget.reset != reset || budget.remaining == 0))
            {
                return Ready::Expired;
            }
            if !gate.sending && gate.next <= Instant::now() {
                gate.sending = true;
                drop(gate);
                return Ready::Send(Sending(self));
            }
        }
        Ready::Wait(self)
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if let Some(reset) = self.reserved {
            let mut gate = self.gate.lock().unwrap();
            if let Some(budget) = gate.budget.as_mut().filter(|budget| budget.reset == reset) {
                budget.reserved = budget.reserved.saturating_sub(1);
            }
        }
    }
}

impl Drop for Sending {
    fn drop(&mut self) {
        let mut gate = self.0.gate.lock().unwrap();
        // Pace from completion of the actual wire write, including a failed write
        // which may have transmitted bytes. Cancellation also releases the gate.
        gate.sending = false;
        gate.next = Instant::now() + self.0.gap;
        if let Some(reset) = self.0.reserved.take()
            && let Some(budget) = gate.budget.as_mut().filter(|budget| budget.reset == reset)
        {
            budget.reserved = budget.reserved.saturating_sub(1);
            budget.remaining = budget.remaining.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn shared_budget_reserves_cancels_and_rechecks_observations() {
        let paces = Paces::default();
        let reset = Duration::from_secs(60);
        let reserve = |remaining| {
            paces.reserve(
                "token".into(),
                Duration::from_secs(5),
                Some((remaining, reset)),
            )
        };
        let first = reserve(1).unwrap();
        assert_eq!(reserve(1).unwrap_err(), reset);
        drop(first);
        let first = reserve(1).unwrap();
        let Ready::Send(sending) = first.start() else {
            panic!("ready")
        };
        drop(sending);
        assert_eq!(reserve(1).unwrap_err(), reset);
        tokio::time::advance(reset).await;
        let pending = reserve(1).unwrap();
        assert!(reserve(0).is_err());
        drop(pending);
        assert!(
            reserve(1).is_err(),
            "a cancelled reservation must not undo a lower observation"
        );
        tokio::time::advance(reset).await;
        assert!(reserve(1).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn queued_sends_recheck_lower_observations_and_expiration() {
        let paces = Paces::default();
        let reserve = |remaining| {
            paces.reserve(
                "token".into(),
                Duration::ZERO,
                Some((remaining, Duration::from_secs(60))),
            )
        };
        let first = reserve(2).unwrap();
        let second = reserve(2).unwrap();
        assert!(reserve(1).is_err());
        let Ready::Send(sending) = first.start() else {
            panic!("first start fits")
        };
        drop(sending);
        assert!(
            matches!(second.start(), Ready::Expired),
            "the lower observation admits only one write"
        );
        assert!(
            reserve(2).is_err(),
            "an old higher observation cannot replenish the balance"
        );
        tokio::time::advance(Duration::from_secs(60)).await;
        let ticket = reserve(2).unwrap();
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(matches!(ticket.start(), Ready::Expired));
    }

    #[tokio::test(start_paused = true)]
    async fn pacing_tracks_actual_writes_and_does_not_reserve_future_time_slots() {
        let paces = Paces::default();
        let reserve = || {
            paces
                .reserve("token".into(), Duration::from_secs(60), None)
                .unwrap()
        };
        let first = reserve();
        let second = reserve();
        let Ready::Send(sending) = first.start() else {
            panic!("ready")
        };
        assert!(second.at() > Instant::now());
        tokio::time::advance(Duration::from_secs(5)).await;
        drop(sending);
        assert_eq!(second.at(), Instant::now() + Duration::from_secs(60));
        drop(second);
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(matches!(reserve().start(), Ready::Send(_)));
    }
}
