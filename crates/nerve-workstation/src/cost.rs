//! Cache/cost telemetry surfaced through the [`nerve_agent::Hook`] seam.
//!
//! This is the first non-`EnvironmentHook` use of the Hook seam (architecture
//! north star: capabilities observe the run through declared seams, never a
//! bespoke side channel). [`CostTelemetryHook`] watches the streamed
//! [`AgentEvent::Usage`] events — including the cache token counts added to the
//! protocol — accumulates totals, and (given an optional [`PriceTable`]) computes
//! a running cost estimate. An opt-in per-run **budget guard** cancels the run's
//! [`CancelToken`] once the estimate crosses a ceiling.
//!
//! Everything here is additive and opt-in: with no price table the hook only
//! tallies tokens; with no budget it never cancels. The deterministic kernel and
//! the provider adapters are untouched — pricing is plain config *data*.

use nerve_agent::{AgentEvent, Hook};
use nerve_core::CancelToken;
use std::collections::HashMap;
use std::sync::Mutex;

/// Per-model token prices in US dollars per **one million** tokens. A model with
/// no entry contributes tokens to the tally but `0` to the cost estimate (so a
/// missing price never silently inflates spend). Plain config data — no I/O.
#[derive(Debug, Clone, Default)]
pub(crate) struct PriceTable {
    models: HashMap<String, ModelPrice>,
}

/// Dollar-per-million-token rates for one model. Cache reads are usually cheaper
/// than fresh input; cache *creation* is usually a surcharge over input.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ModelPrice {
    pub(crate) input_per_mtok: f64,
    pub(crate) output_per_mtok: f64,
    pub(crate) cache_read_per_mtok: f64,
    pub(crate) cache_write_per_mtok: f64,
}

/// A small built-in price table (config *data*, USD per million tokens) covering
/// common models. Approximate published list prices; a future config file (the
/// declared seam) can override or extend it. Unknown models cost `0`.
pub(crate) fn default_price_table() -> PriceTable {
    PriceTable::default()
        .with_model(
            "claude-opus-4-8",
            ModelPrice {
                input_per_mtok: 15.0,
                output_per_mtok: 75.0,
                cache_read_per_mtok: 1.50,
                cache_write_per_mtok: 18.75,
            },
        )
        .with_model(
            "claude-sonnet-4-6",
            ModelPrice {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_read_per_mtok: 0.30,
                cache_write_per_mtok: 3.75,
            },
        )
        .with_model(
            "claude-haiku-4-5",
            ModelPrice {
                input_per_mtok: 1.0,
                output_per_mtok: 5.0,
                cache_read_per_mtok: 0.10,
                cache_write_per_mtok: 1.25,
            },
        )
}

impl PriceTable {
    /// Register a model's prices.
    pub(crate) fn with_model(mut self, model: impl Into<String>, price: ModelPrice) -> Self {
        self.models.insert(model.into(), price);
        self
    }

    /// Estimated dollar cost for one usage delta under `model`. Unknown models
    /// cost `0` (tokens are still tallied elsewhere).
    fn cost_of(&self, model: &str, usage: &UsageDelta) -> f64 {
        let Some(price) = self.models.get(model) else {
            return 0.0;
        };
        per_mtok(usage.input_tokens, price.input_per_mtok)
            + per_mtok(usage.output_tokens, price.output_per_mtok)
            + per_mtok(usage.cache_read_tokens, price.cache_read_per_mtok)
            + per_mtok(usage.cache_creation_tokens, price.cache_write_per_mtok)
    }
}

fn per_mtok(tokens: u64, rate_per_mtok: f64) -> f64 {
    (tokens as f64) / 1_000_000.0 * rate_per_mtok
}

/// One usage delta lifted out of an [`AgentEvent::Usage`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct UsageDelta {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) cache_read_tokens: u64,
    pub(crate) cache_creation_tokens: u64,
}

impl UsageDelta {
    /// Lift a `Usage` agent event into a delta; `None` for other events.
    fn from_event(event: &AgentEvent) -> Option<Self> {
        match event {
            AgentEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            } => Some(Self {
                input_tokens: u64::from(*input_tokens),
                output_tokens: u64::from(*output_tokens),
                cache_read_tokens: u64::from(*cache_read_tokens),
                cache_creation_tokens: u64::from(*cache_creation_tokens),
            }),
            _ => None,
        }
    }

    fn add(&mut self, other: &Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
    }
}

/// Running telemetry snapshot a host can surface after (or during) a run.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct CostSnapshot {
    pub(crate) totals: UsageDelta,
    pub(crate) estimated_usd: f64,
    /// True once the budget guard tripped and cancelled the run.
    pub(crate) budget_exceeded: bool,
}

struct TelemetryState {
    totals: UsageDelta,
    estimated_usd: f64,
    budget_exceeded: bool,
}

/// Observe-only cost/cache telemetry hook with an optional budget guard.
pub(crate) struct CostTelemetryHook {
    model: String,
    prices: PriceTable,
    /// Opt-in per-run ceiling in USD; `None` disables the guard.
    budget_usd: Option<f64>,
    /// Cancelled when the estimate crosses `budget_usd`. Holding a clone lets the
    /// observe-only hook stop the run at the next cancellation check.
    cancel: CancelToken,
    state: Mutex<TelemetryState>,
}

impl CostTelemetryHook {
    /// Build a telemetry hook for `model`. `budget_usd` is opt-in; pass `None` to
    /// only tally. `cancel` is the run's token, cancelled if the budget trips.
    pub(crate) fn new(
        model: impl Into<String>,
        prices: PriceTable,
        budget_usd: Option<f64>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            model: model.into(),
            prices,
            budget_usd,
            cancel,
            state: Mutex::new(TelemetryState {
                totals: UsageDelta::default(),
                estimated_usd: 0.0,
                budget_exceeded: false,
            }),
        }
    }

    /// Current telemetry snapshot.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn snapshot(&self) -> CostSnapshot {
        let state = lock(&self.state);
        CostSnapshot {
            totals: state.totals,
            estimated_usd: state.estimated_usd,
            budget_exceeded: state.budget_exceeded,
        }
    }
}

impl Hook for CostTelemetryHook {
    fn on_event(&self, event: &AgentEvent) {
        let Some(delta) = UsageDelta::from_event(event) else {
            return;
        };
        let mut state = lock(&self.state);
        state.totals.add(&delta);
        state.estimated_usd += self.prices.cost_of(&self.model, &delta);
        // Budget guard: once the estimate crosses the ceiling, cancel the run.
        // Observe-only hooks can't abort directly, but cancelling the shared token
        // stops the loop at the next cancellation check — an honest, cooperative
        // guard rather than a hard kill mid-request.
        if let Some(budget) = self.budget_usd
            && state.estimated_usd > budget
            && !state.budget_exceeded
        {
            state.budget_exceeded = true;
            self.cancel.cancel();
        }
    }
}

fn lock(state: &Mutex<TelemetryState>) -> std::sync::MutexGuard<'_, TelemetryState> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u32, output: u32, cache_read: u32, cache_write: u32) -> AgentEvent {
        AgentEvent::Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_write,
        }
    }

    fn priced() -> PriceTable {
        PriceTable::default().with_model(
            "m1",
            ModelPrice {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_read_per_mtok: 0.30,
                cache_write_per_mtok: 3.75,
            },
        )
    }

    #[test]
    fn tallies_tokens_including_cache_fields() {
        let hook = CostTelemetryHook::new("m1", PriceTable::default(), None, CancelToken::never());
        hook.on_event(&usage(100, 50, 40, 10));
        hook.on_event(&usage(200, 60, 0, 5));
        // Non-usage events are ignored.
        hook.on_event(&AgentEvent::AssistantText("hi".into()));
        let snap = hook.snapshot();
        assert_eq!(snap.totals.input_tokens, 300);
        assert_eq!(snap.totals.output_tokens, 110);
        assert_eq!(snap.totals.cache_read_tokens, 40);
        assert_eq!(snap.totals.cache_creation_tokens, 15);
    }

    #[test]
    fn estimates_cost_from_price_table() {
        let hook = CostTelemetryHook::new("m1", priced(), None, CancelToken::never());
        // 1M input @3, 1M output @15, 1M cache-read @0.30, 1M cache-write @3.75.
        hook.on_event(&usage(1_000_000, 1_000_000, 1_000_000, 1_000_000));
        let snap = hook.snapshot();
        assert!((snap.estimated_usd - (3.0 + 15.0 + 0.30 + 3.75)).abs() < 1e-9);
        assert!(!snap.budget_exceeded);
    }

    #[test]
    fn unknown_model_costs_zero_but_still_tallies() {
        let hook = CostTelemetryHook::new("unknown", priced(), None, CancelToken::never());
        hook.on_event(&usage(1_000_000, 1_000_000, 0, 0));
        let snap = hook.snapshot();
        assert_eq!(snap.estimated_usd, 0.0);
        assert_eq!(snap.totals.input_tokens, 1_000_000);
    }

    #[test]
    fn budget_guard_cancels_once_exceeded() {
        let cancel = CancelToken::new();
        // Budget $0.01; one 1M-input delta costs $3 under m1, tripping the guard.
        let hook = CostTelemetryHook::new("m1", priced(), Some(0.01), cancel.clone());
        assert!(!cancel.is_cancelled());
        hook.on_event(&usage(1_000_000, 0, 0, 0));
        assert!(cancel.is_cancelled(), "budget overage must cancel the run");
        assert!(hook.snapshot().budget_exceeded);
    }

    #[test]
    fn budget_guard_does_not_cancel_within_budget() {
        let cancel = CancelToken::new();
        let hook = CostTelemetryHook::new("m1", priced(), Some(100.0), cancel.clone());
        hook.on_event(&usage(1_000, 1_000, 0, 0)); // a few cents
        assert!(!cancel.is_cancelled());
        assert!(!hook.snapshot().budget_exceeded);
    }

    #[test]
    fn no_budget_never_cancels() {
        let cancel = CancelToken::new();
        let hook = CostTelemetryHook::new("m1", priced(), None, cancel.clone());
        hook.on_event(&usage(10_000_000, 10_000_000, 0, 0));
        assert!(!cancel.is_cancelled());
    }
}
