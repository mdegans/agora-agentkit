//! Stopping a run gracefully ([`Stop`]): a drain starts no new session and
//! lets those in flight finish; an end-turn closes each session in flight
//! after its current turn, through [`Agent::on_stop`] (agora-agents#293).

use std::sync::atomic::Ordering;

use misanthropic::response::StopReason;

use super::*;

/// Plain `EndTurn` responses, raising `stop` to `level` as the `at`th
/// inference (1-based, `infer` and `infer_batch` alike) is answered: the
/// signal arrives while that turn is in flight.
struct StoppingInfer {
    stop: Stop,
    level: StopLevel,
    at: usize,
    calls: Arc<AtomicUsize>,
}

impl StoppingInfer {
    fn new(stop: &Stop, level: StopLevel, at: usize) -> Self {
        Self {
            stop: stop.clone(),
            level,
            at,
            calls: Arc::default(),
        }
    }

    fn call(&self) {
        if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
            self.stop.request(self.level);
        }
    }
}

#[async_trait::async_trait]
impl Inference for StoppingInfer {
    type Error = TestError;

    async fn infer<P>(&self, _prompt: P) -> Result<response::Message, TestError>
    where
        P: Serialize + Send,
    {
        self.call();
        Ok(message(StopReason::EndTurn))
    }

    async fn infer_batch<P>(
        &self,
        prompts: &[&P],
    ) -> Result<Vec<Result<response::Message, TestError>>, TestError>
    where
        P: Serialize + Send + Sync,
    {
        self.call();
        Ok(prompts
            .iter()
            .map(|_| Ok(message(StopReason::EndTurn)))
            .collect())
    }

    async fn models(&self) -> Result<misanthropic::model::Models, TestError> {
        Ok(offered_models())
    }
}

/// Draining before the run: no session starts, nothing is saved or torn
/// down, and every agent stays seated in the reactor
#[tokio::test]
async fn a_drain_before_the_run_starts_no_session() {
    let stop = Stop::new();
    stop.request(StopLevel::Drain);
    let agents = vec![
        agent(Behavior::Complete, 1),
        agent(Behavior::Complete, 1),
        batch_agent(Behavior::Complete, 1),
    ];
    let teardowns: Vec<_> =
        agents.iter().map(|a| a.teardowns.clone()).collect();
    let store = MemStore::default();
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(MockInference::end_turns(0), store.clone(), agents)
            .with_stop(stop);
    let report = reactor.run().await.unwrap();

    assert_eq!((report.done, report.failed), (0, 0));
    assert_eq!(report.unstarted, 3);
    assert!(report.errors.is_empty());
    assert!(store.map.lock().unwrap().is_empty(), "nothing saved");
    assert!(teardowns.iter().all(|t| t.load(Ordering::SeqCst) == 0));
    assert_eq!(reactor.agents.len(), 3, "still seated");
}

/// A drain while a session is in flight: that session runs to its own end,
/// every turn of it, and is saved; the next never starts
#[tokio::test]
async fn a_drain_finishes_the_session_in_flight_and_starts_no_other() {
    let stop = Stop::new();
    let inference = StoppingInfer::new(&stop, StopLevel::Drain, 1);
    let calls = inference.calls.clone();
    let first = agent(Behavior::Complete, 3);
    let (first_id, stops) = (first.id, first.stops.clone());
    let agents = vec![
        first,
        agent(Behavior::Complete, 3),
        agent(Behavior::Complete, 3),
    ];
    let store = MemStore::default();
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(inference, store.clone(), agents).with_stop(stop);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(report.unstarted, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "the whole first session");
    assert_eq!(stops.load(Ordering::SeqCst), 0, "a drain doesn't close it");
    let saved: Vec<AgentId> =
        store.map.lock().unwrap().keys().copied().collect();
    assert_eq!(saved, vec![first_id]);
    assert_eq!(reactor.agents.len(), 2);
}

/// An end-turn while a turn is in flight: the turn finishes, then the agent
/// is asked once to close; its closing turn runs, and the session completes
/// and saves like any other
#[tokio::test]
async fn an_end_turn_closes_the_session_after_the_turn_in_flight() {
    let stop = Stop::new();
    let inference = StoppingInfer::new(&stop, StopLevel::EndTurn, 2);
    let calls = inference.calls.clone();
    let mut first = agent(Behavior::Complete, 10);
    first.closes = true;
    let (stops, saves, teardowns) = (
        first.stops.clone(),
        first.saves.clone(),
        first.teardowns.clone(),
    );
    let store = MemStore::default();
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        inference,
        store.clone(),
        vec![first, agent(Behavior::Complete, 1)],
    )
    .with_stop(stop);
    let report = reactor.run().await.unwrap();

    // Two acting turns, then the closing one.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert_eq!(report.done, 1, "{:?}", report.errors);
    assert_eq!(saves.load(Ordering::SeqCst), 1, "complete and saved");
    assert_eq!(teardowns.load(Ordering::SeqCst), 1);
    assert_eq!(report.unstarted, 1, "the next never started");
    assert_eq!(store.map.lock().unwrap().len(), 1);
}

/// An agent that knows no better close (the default `on_stop`) ends at
/// once, failed, torn down and saved as it stands
#[tokio::test]
async fn by_default_a_stopped_session_ends_failed_and_saved() {
    let stop = Stop::new();
    let inference = StoppingInfer::new(&stop, StopLevel::EndTurn, 1);
    let calls = inference.calls.clone();
    let first = agent(Behavior::Complete, 10);
    let (id, saves, teardowns) =
        (first.id, first.saves.clone(), first.teardowns.clone());
    let store = MemStore::default();
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(inference, store.clone(), vec![first]).with_stop(stop);
    let report = reactor.run().await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!((report.done, report.failed), (0, 1));
    assert!(report.errors.is_empty(), "failed, not errored");
    assert_eq!(saves.load(Ordering::SeqCst), 0, "on_saved is for complete");
    assert_eq!(teardowns.load(Ordering::SeqCst), 1);
    assert!(store.map.lock().unwrap().contains_key(&id), "state saved");
}

/// Round-major: every live session is asked to close at the round boundary
/// after the round in flight, once each
#[tokio::test]
async fn an_end_turn_closes_every_live_batch_session_once() {
    let stop = Stop::new();
    let inference = StoppingInfer::new(&stop, StopLevel::EndTurn, 1);
    let calls = inference.calls.clone();
    let agents: Vec<TestAgent> = (0..3)
        .map(|_| {
            let mut a = batch_agent(Behavior::Complete, 10);
            a.closes = true;
            a
        })
        .collect();
    let stops: Vec<_> = agents.iter().map(|a| a.stops.clone()).collect();
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(inference, MemStore::default(), agents).with_stop(stop);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 3, "{:?}", report.errors);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one round, then closing");
    assert!(stops.iter().all(|s| s.load(Ordering::SeqCst) == 1));
}

/// A session held on an unreachable endpoint stops holding once the run
/// is stopping after the turn: it fails on its budget as it would unheld,
/// so the run ends rather than wait without limit
#[tokio::test]
async fn an_end_turn_stops_holding_an_unreachable_endpoint() {
    let stop = Stop::new();
    let inference = FlakyInfer::unreachable(usize::MAX);
    let calls = inference.calls.clone();
    let mut reactor: Reactor<_, _, TestAgent> = Reactor::new(
        inference,
        MemStore::default(),
        vec![agent(Behavior::Complete, 1)],
    )
    .with_unreachable_hold(Duration::from_millis(1))
    .with_stop(stop.clone());
    let raise = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.request(StopLevel::EndTurn);
    });
    let report = tokio::time::timeout(Duration::from_secs(10), reactor.run())
        .await
        .expect("the run ends")
        .unwrap();
    raise.await.unwrap();

    assert_eq!(report.failed, 1);
    assert_eq!(report.errors.len(), 1);
    assert!(calls.load(Ordering::SeqCst) > MAX_INFER_RETRIES as usize);
}

/// A stop that arrives while a session is being set up still lets it act
/// once: it never closes having done nothing
#[tokio::test]
async fn a_stop_during_init_still_lets_the_first_turn_run() {
    let stop = Stop::new();
    let mut first = agent(Behavior::Complete, 10);
    first.closes = true;
    first.stop_on_init = Some(stop.clone());
    let stops = first.stops.clone();
    let inference = MockInference::end_turns(2);
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(inference, MemStore::default(), vec![first])
            .with_stop(stop);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 1, "{:?}", report.errors);
    assert_eq!(
        stops.load(Ordering::SeqCst),
        1,
        "asked after the first turn"
    );
}

/// The batch path, likewise: no session is asked before its first round
#[tokio::test]
async fn a_batch_stop_before_the_first_round_waits_for_it() {
    let stop = Stop::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let agents: Vec<TestAgent> = (0..2)
        .map(|i| {
            let mut a = batch_agent(Behavior::Complete, 10);
            a.closes = true;
            if i == 0 {
                a.stop_on_init = Some(stop.clone());
            }
            a
        })
        .collect();
    let stops: Vec<_> = agents.iter().map(|a| a.stops.clone()).collect();
    let inference = StoppingInfer {
        stop: stop.clone(),
        level: StopLevel::EndTurn,
        at: 0,
        calls: calls.clone(),
    };
    let mut reactor: Reactor<_, _, TestAgent> =
        Reactor::new(inference, MemStore::default(), agents).with_stop(stop);
    let report = reactor.run().await.unwrap();

    assert_eq!(report.done, 2, "{:?}", report.errors);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a round, then the close");
    assert!(stops.iter().all(|s| s.load(Ordering::SeqCst) == 1));
}

/// `unstarted` adds up across reports, as the rest does
#[test]
fn unstarted_adds_up() {
    let a = Report {
        unstarted: 2,
        ..Default::default()
    };
    let b = Report {
        unstarted: 3,
        ..Default::default()
    };
    assert_eq!((a + b).unstarted, 5);
}
