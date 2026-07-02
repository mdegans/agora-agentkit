//! The `MaxTokens` path of the default [`Agent::handle`]: a clipped response
//! seats nothing, `on_truncate` raises the budget toward the declared model
//! ceiling, and the round stalls — a bounded, non-blind retry.
//!
//! [`Agent::handle`]: crate::reactor::Agent::handle

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use misanthropic::response::StopReason;
use serde::Serialize;

use super::*;

/// A clipped response doubles the budget (the harness agent declares no
/// ceiling — `model_info` leaves `max_tokens` 0), seats nothing, and stalls.
#[tokio::test]
async fn truncation_bumps_budget_and_stalls() {
    let mut a = agent(Behavior::Complete, 1);
    let before = a.prompt.max_tokens.get();
    let seated = a.prompt.messages.len();

    let control = a.handle(message(StopReason::MaxTokens)).await.unwrap();

    assert_eq!(control, Control::Stalled);
    assert_eq!(a.prompt.max_tokens.get(), before * 2, "budget doubled");
    assert_eq!(a.prompt.messages.len(), seated, "nothing was seated");
}

/// The bump clamps to the ceiling the agent declared in its requested
/// [`ModelInfo`], and at the ceiling degenerates to a plain stall — so the
/// stall cap, not the budget, ends a workload that can never fit.
#[tokio::test]
async fn truncation_bump_clamps_to_declared_ceiling() {
    let mut a = agent(Behavior::Complete, 1);
    let ceiling = a.prompt.max_tokens.get() + 1000;
    a.model.max_tokens = ceiling;

    a.handle(message(StopReason::MaxTokens)).await.unwrap();
    assert_eq!(a.prompt.max_tokens.get(), ceiling, "clamped to the ceiling");

    let control = a.handle(message(StopReason::MaxTokens)).await.unwrap();
    assert_eq!(control, Control::Stalled);
    assert_eq!(a.prompt.max_tokens.get(), ceiling, "no raise past it");
}

/// A transport that records each `infer`'s serialized `max_tokens`, proving a
/// raised budget actually reaches the wire on the retry.
struct BudgetRecorder {
    /// Responses handed out one per `infer`, in order (as [`MockInference`]).
    script: Mutex<VecDeque<response::Message>>,
    budgets: Arc<Mutex<Vec<u64>>>,
}

#[async_trait::async_trait]
impl Inference for BudgetRecorder {
    type Error = TestError;

    async fn infer<P>(&self, prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        let wire = serde_json::to_value(&prompt).expect("prompt serializes");
        self.budgets
            .lock()
            .unwrap()
            .push(wire["max_tokens"].as_u64().expect("max_tokens on the wire"));
        Ok(self.script.lock().unwrap().pop_front().expect(
            "mock inference script exhausted: more infer calls than scripted",
        ))
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
    }
}

/// Full drive: a clipped first round is a stall, not a completion (the bug this
/// module pins down — it used to quiesce to `Done(Complete)`); the retry goes
/// out with the doubled budget and the session completes.
#[tokio::test]
async fn clipped_drive_retries_with_raised_budget_and_completes() {
    let budgets = Arc::new(Mutex::new(Vec::new()));
    let transport = BudgetRecorder {
        script: Mutex::new(
            [message(StopReason::MaxTokens), message(StopReason::EndTurn)]
                .into_iter()
                .collect(),
        ),
        budgets: budgets.clone(),
    };
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        transport,
        MemStore::default(),
        [agent(Behavior::Complete, 1)],
    );
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1, "clip then completion, not a false success");
    let budgets = budgets.lock().unwrap();
    assert_eq!(budgets.len(), 2, "the clipped round was retried");
    assert_eq!(budgets[1], budgets[0] * 2, "the retry carried the raise");
}
