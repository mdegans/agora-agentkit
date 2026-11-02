use std::{collections::BTreeMap, num::NonZeroUsize};

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::{ids::AgentId, reactor::Agent};

/// A failed bulk save.
#[derive(Debug, thiserror::Error)]
#[error("saved {} before failing: {inner}", saved.len())]
pub struct SaveError<E, V = NoVersion> {
    /// [`AgentId`]s of [`Agent`]s whose [`Agent::State`] was successfully
    /// saved, with each one's new base.
    pub saved: Saved<V>,
    #[source]
    pub inner: E,
}

/// The ids a bulk save committed, each with its new base
pub type Saved<V> = BTreeMap<AgentId, Option<V>>;

/// The [`Storage::Version`] of a store that keeps no versions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NoVersion {}

/// A loaded value and the version it is, to save against
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded<T = serde_json::Value, V = NoVersion> {
    pub value: T,
    pub base: Option<V>,
}

/// An `Inference` backend for an [`Agent`] [`Reactor`].
///
/// [`Agent`]: super::Agent
/// [`Reactor`]: super::Reactor
#[async_trait::async_trait]
pub trait Inference: Send + Sync {
    type Error: super::Error;

    /// Complete a [`misanthropic::response::Message`] for a given `prompt`.
    ///
    /// Like [`misanthropic::Client::message`] this accepts anything Serialize
    /// with an additional `Send` bound because the compiler demanded it.
    /// Typically this should be a [`misanthropic::Prompt`], a
    /// [`misanthropic::CachedPrompt`] wrapper or even a [`serde_json::Value`]
    /// if the situation absolutely requires it.
    async fn infer<P>(
        &self,
        prompt: P,
    ) -> Result<misanthropic::response::Message, Self::Error>
    where
        P: Serialize + Send;

    /// Run `prompts` as one batch, results **aligned to input order**; the outer
    /// `Err` is a whole-submission failure, a per-item `Err` re-batches that
    /// agent. The default fans out through [`infer`](Self::infer) bounded by
    /// [`max_concurrency`](Self::max_concurrency); transports with a cheaper
    /// native batch override it.
    async fn infer_batch<P>(
        &self,
        prompts: &[&P],
    ) -> Result<
        Vec<Result<misanthropic::response::Message, Self::Error>>,
        Self::Error,
    >
    where
        P: Serialize + Send + Sync,
    {
        let limit = self.max_concurrency().get();
        // Materialize the (lazy) futures eagerly so the closure is invoked at the
        // method's concrete lifetimes — a closure left inside `stream`/`buffered`
        // can't satisfy the HRTB an `async_trait` default imposes. `buffered` then
        // drives the ready-made futures in order, bounded by `limit`.
        let futs: Vec<_> = prompts.iter().map(|&p| self.infer(p)).collect();
        let results = futures::stream::iter(futs)
            .buffered(limit)
            .collect::<Vec<_>>()
            .await;
        Ok(results)
    }

    /// The [`Models`] this [`Inference`] can serve, including any model
    /// [`Capabilities`], like batch, structured output, thinking, etc.
    async fn models(&self) -> Result<misanthropic::model::Models, Self::Error>;

    /// The endpoint's behavioral [`Quirks`](super::inference::Quirks). The
    /// default is canonical Anthropic behavior.
    fn quirks(&self) -> super::inference::Quirks {
        super::inference::Quirks::default()
    }

    /// How many agents this transport will run at once. `Some(1)` forces
    /// serial-to-completion execution and is the default. None means unbounded.
    ///
    /// # Note
    ///
    /// In general, especially with the default Anthropic tier and local models,
    /// the default is optimal.
    fn max_concurrency(&self) -> NonZeroUsize {
        NonZeroUsize::new(1).unwrap()
    }
}

/// Error type for when [`Storage`] can't find an agent.
#[derive(Debug, thiserror::Error)]
#[error("Agent was not found in Storage: {0}")]
pub struct AgentNotFound(pub AgentId);

/// An opaque key-value store over agent ids. It should be able to handle many
/// different kinds of Agents so it can be shared between all [`Reactor`]s.
///
/// It is only strictly necessary to implement `save_raw` and `load_raw`,
/// however it's recommended to implement `save_all_raw` and `load_all_raw` if
/// it's optimal for the storage backend (eg. a single database query).
///
/// [`Reactor`]: super::Reactor
#[async_trait::async_trait]
pub trait Storage: Sized + Send + Sync {
    type Error: super::Error + From<serde_json::Error> + From<AgentNotFound>;
    /// The version a value was loaded at, handed back to
    /// [`save_raw`](Storage::save_raw) as its base. [`NoVersion`] for a
    /// store that keeps none
    type Version: Clone
        + std::fmt::Debug
        + PartialEq
        + Serialize
        + serde::de::DeserializeOwned
        + Send
        + Sync
        + 'static;

    /// Persist an opaque JSON value under `id`, overwriting any prior
    /// value. `base` is what [`load_raw`](Storage::load_raw) returned for
    /// it, or the last save's result. Returns the new base
    async fn save_raw(
        &mut self,
        id: AgentId,
        value: serde_json::Value,
        base: Option<&Self::Version>,
    ) -> Result<Option<Self::Version>, Self::Error>;

    /// Load the JSON value stored under `id`
    async fn load_raw(
        &self,
        id: AgentId,
    ) -> Result<Loaded<serde_json::Value, Self::Version>, Self::Error>;

    /// Serialize and store [`Agent::State`]
    ///
    /// [`Agent::State`]: crate::reactor::Agent::State
    async fn save<A: Agent>(
        &mut self,
        id: AgentId,
        state: &A::State,
        base: Option<&Self::Version>,
    ) -> Result<Option<Self::Version>, Self::Error> {
        self.save_raw(id, serde_json::to_value(state)?, base).await
    }

    /// Load and deserialize `Agent::State`
    async fn load<A: Agent>(
        &self,
        id: AgentId,
    ) -> Result<Loaded<A::State, Self::Version>, Self::Error> {
        let Loaded { value, base } = self.load_raw(id).await?;
        Ok(Loaded {
            value: serde_json::from_value(value)?,
            base,
        })
    }

    /// Persist many values at once, reporting exactly which ids committed. On
    /// failure the returned [`SaveError::saved`] holds the ids that *did* land,
    /// so the caller can recover the rest. A transactional store should commit
    /// the whole batch or none (and so return an empty `saved` on rollback); the
    /// default loops [`save_raw`] and is therefore *not* atomic — it reports the
    /// prefix it managed before the first failure. Override it to do one query
    /// (e.g. a multi-row SQL upsert) and return the appropriate `saved` set.
    ///
    /// [`save_raw`]: Storage::save_raw
    async fn save_all_raw<It>(
        &mut self,
        items: It,
    ) -> Result<Saved<Self::Version>, SaveError<Self::Error, Self::Version>>
    where
        It: ExactSizeIterator<
                Item = (AgentId, serde_json::Value, Option<Self::Version>),
            > + Send,
    {
        let mut saved = Saved::<Self::Version>::new();
        for (id, value, base) in items {
            match self.save_raw(id, value, base.as_ref()).await {
                Ok(new) => {
                    saved.insert(id, new);
                }
                Err(inner) => return Err(SaveError { saved, inner }),
            }
        }
        Ok(saved)
    }

    /// Load many raw [`Value`](serde_json::Value)s at once.
    ///
    /// [`load_raw`]: Storage::load_raw
    async fn load_all_raw<It>(
        &self,
        ids: It,
    ) -> Result<
        Vec<(
            AgentId,
            Result<Loaded<serde_json::Value, Self::Version>, Self::Error>,
        )>,
        Self::Error,
    >
    where
        It: ExactSizeIterator<Item = AgentId> + Send,
    {
        let mut raw = Vec::with_capacity(ids.len());
        for id in ids {
            raw.push((id, self.load_raw(id).await))
        }
        Ok(raw)
    }

    /// Batch Serialize and store many [`Agent::State`](super::Agent::State)s.
    /// If any item fails to Serialize the entire save is aborted.
    // FIXME: This is called nowhere because it aborts the entire save. Instead
    // the per-agent failure logic lives in `persist_all`. We *can* fix this
    // here but we need a way to merge `SaveError`s. We'd filter out the agents
    // that can't be serialized first, try to save them all raw, and in the
    // path where we have errors on both, we union saved (empty) and the inner
    // error. So we might need a Vec<E> for inner. Or we change SaveError so
    // that it wraps a `BTreeMap<AgentId, E>` and make the SaveError per-agent.
    // I like that last solution best - mdegans.
    async fn save_all<It, A: Agent>(
        &mut self,
        items: It,
    ) -> Result<Saved<Self::Version>, SaveError<Self::Error, Self::Version>>
    where
        It: ExactSizeIterator<Item = (AgentId, A::State, Option<Self::Version>)>
            + Send,
    {
        let mut raw = Vec::with_capacity(items.len());
        for (id, value, base) in items {
            let value = serde_json::to_value(value).map_err(|e| SaveError {
                saved: Saved::<Self::Version>::new(),
                inner: Self::Error::from(e),
            })?;
            raw.push((id, value, base));
        }
        self.save_all_raw(raw.into_iter()).await
    }

    /// Load and deserialize many payloads, skipping ids with nothing stored.
    /// [`Deserialize`] errors are not fatal and will be converted to
    /// [`Self::Error`]
    #[allow(clippy::type_complexity)]
    async fn load_all<It, A: Agent>(
        &self,
        ids: It,
    ) -> Result<
        Vec<(
            AgentId,
            Result<Loaded<A::State, Self::Version>, Self::Error>,
        )>,
        Self::Error,
    >
    where
        It: ExactSizeIterator<Item = AgentId> + Send,
    {
        let raw = self.load_all_raw(ids).await?;
        let mut out = Vec::with_capacity(raw.len());
        for (id, loaded) in raw {
            let result = loaded.and_then(|Loaded { value, base }| {
                serde_json::from_value(value)
                    .map(|value| Loaded { value, base })
                    .map_err(Into::into)
            });
            out.push((id, result));
        }
        Ok(out)
    }
}
