//! The runtime: the agents it knows and the agent instances it hosts.

use std::any::Any;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent::TurnSetup;
use crate::store::Store;
use crate::{Agent, AgentInstance, Error, InstanceId, Result};

/// The running keelrs instance that knows the agents and hosts their instances.
///
/// Dropping it stops every agent instance and releases their leases: runs in
/// flight stop, and a later runtime on the same [`Store`] settles the one that
/// was running as [`Outcome::Aborted`](crate::Outcome::Aborted) and runs the
/// queued ones.
#[derive(Debug)]
pub struct Runtime {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    agents: HashMap<String, AnyAgent>,
    /// Instances already spawned, keyed by `(agent name, instance id)`, so the
    /// same id continues the same conversation; each with its data as JSON.
    instances: Mutex<HashMap<(String, InstanceId), (AgentInstance, Value)>>,
    /// Where the instances' events are kept.
    store: Store,
    /// Parent of every instance token; cancelled when the runtime is dropped.
    shutdown: CancellationToken,
}

impl Runtime {
    /// A runtime with settings other than the defaults, such as its [`Store`].
    #[must_use]
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::default()
    }

    /// Starts a runtime hosting `agents`, keeping their events in memory:
    /// a tuple, for agents with different data types, or an array, `Vec` or
    /// slice of one type, each by value or by reference.
    ///
    /// Fails with [`Error::DuplicateAgent`] when two agents share a name.
    pub fn start(agents: impl Agents) -> Result<Self> {
        Self::builder().start(agents)
    }

    /// The instance `id` of an agent without data: created on first use,
    /// continued after.
    ///
    /// Fails as [`instance_with`](Self::instance_with) does.
    pub fn instance(&self, agent: &Agent, id: InstanceId) -> Result<AgentInstance> {
        self.instance_with(agent, id, ())
    }

    /// The instance `id` of `agent`, created with its frozen `data` on first
    /// use and continued after when `data` is equal (compared as serialized
    /// JSON).
    ///
    /// Fails with [`Error::InstanceDataConflict`] when the instance already
    /// exists with other data, and with [`Error::InvalidInstanceData`] when
    /// `data` does not serialize. Fails with [`Error::InvalidInstanceId`] on a
    /// blank id, and with [`Error::UnknownAgent`] unless `agent` is the very
    /// one given to [`Runtime::start`] (not just one with its name); the
    /// instance runs with that registered agent's settings. Fails with
    /// [`Error::NoTokioRuntime`] outside a Tokio runtime.
    pub fn instance_with<D>(
        &self,
        agent: &Agent<D>,
        id: InstanceId,
        data: D,
    ) -> Result<AgentInstance>
    where
        D: Serialize + DeserializeOwned + Send + Sync + 'static,
    {
        if id.as_str().trim().is_empty() {
            return Err(Error::InvalidInstanceId(id));
        }
        // The registered agent, not the caller's clone, so its settings apply.
        let Some(registered) = self
            .inner
            .agents
            .get(agent.name())
            .and_then(|registered| registered.agent.downcast_ref::<Agent<D>>())
            .filter(|registered| registered.is(agent))
        else {
            return Err(Error::UnknownAgent(agent.name().to_owned()));
        };
        // Checked first: the actor's spawn panics outside a Tokio runtime.
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(Error::NoTokioRuntime);
        }
        let name = agent.name().to_owned();
        let json = serde_json::to_value(&data).map_err(|e| Error::InvalidInstanceData {
            agent: name.clone(),
            id: id.clone(),
            reason: e.to_string(),
        })?;
        match self
            .inner
            .instances
            .lock()
            .entry((name.clone(), id.clone()))
        {
            Entry::Occupied(entry) => {
                let (instance, kept) = entry.get();
                if *kept != json {
                    return Err(Error::InstanceDataConflict { agent: name, id });
                }
                Ok(instance.clone())
            }
            Entry::Vacant(entry) => {
                let instance = AgentInstance::spawn(
                    name,
                    TurnSetup::new(registered, id.clone(), data),
                    id,
                    json.clone(),
                    self.inner.store.clone(),
                    &self.inner.shutdown,
                );
                entry.insert((instance.clone(), json));
                Ok(instance)
            }
        }
    }
}

/// Settings for a [`Runtime`], from [`Runtime::builder`].
#[derive(Debug, Default)]
pub struct RuntimeBuilder {
    store: Store,
}

impl RuntimeBuilder {
    /// Keeps the instances' events in `store` rather than in memory.
    #[must_use]
    pub fn store(mut self, store: Store) -> Self {
        self.store = store;
        self
    }

    /// Starts the runtime hosting `agents`, as [`Runtime::start`] does.
    ///
    /// Fails with [`Error::DuplicateAgent`] when two agents share a name.
    pub fn start(self, agents: impl Agents) -> Result<Runtime> {
        let mut registry = HashMap::new();
        for agent in agents.into_agents() {
            let name = agent.name.clone();
            if registry.insert(name.clone(), agent).is_some() {
                return Err(Error::DuplicateAgent(name));
            }
        }
        Ok(Runtime {
            inner: Arc::new(Inner {
                agents: registry,
                instances: Mutex::default(),
                store: self.store,
                shutdown: CancellationToken::new(),
            }),
        })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.inner.shutdown.cancel();
    }
}

/// An agent of any data type, so agents with different data can share one
/// runtime; made from an [`Agent`] or a reference to one.
pub struct AnyAgent {
    name: String,
    /// The `Agent<D>` itself, checked against the one an instance is asked for.
    agent: Box<dyn Any + Send + Sync>,
}

impl<D: 'static> From<Agent<D>> for AnyAgent {
    fn from(agent: Agent<D>) -> Self {
        Self {
            name: agent.name().to_owned(),
            agent: Box::new(agent),
        }
    }
}

impl<D: 'static> From<&Agent<D>> for AnyAgent {
    fn from(agent: &Agent<D>) -> Self {
        agent.clone().into()
    }
}

impl fmt::Debug for AnyAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnyAgent")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// The agents [`Runtime::start`] takes: a tuple of up to 12, or an array,
/// `Vec` or slice, of [`Agent`]s or references to them.
///
/// Sealed: only keelrs implements it.
pub trait Agents: sealed::Sealed {
    /// Each agent, in order.
    #[doc(hidden)]
    fn into_agents(self) -> Vec<AnyAgent>;
}

mod sealed {
    /// Keeps [`Agents`](super::Agents) to the impls in this module.
    pub trait Sealed {}
}

impl<A: Into<AnyAgent>> sealed::Sealed for Vec<A> {}
impl<A: Into<AnyAgent>, const N: usize> sealed::Sealed for [A; N] {}
impl<'a, A> sealed::Sealed for &'a [A] where &'a A: Into<AnyAgent> {}
impl<'a, A> sealed::Sealed for &'a Vec<A> where &'a A: Into<AnyAgent> {}
impl<'a, A, const N: usize> sealed::Sealed for &'a [A; N] where &'a A: Into<AnyAgent> {}

impl<A: Into<AnyAgent>> Agents for Vec<A> {
    fn into_agents(self) -> Vec<AnyAgent> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<A: Into<AnyAgent>, const N: usize> Agents for [A; N] {
    fn into_agents(self) -> Vec<AnyAgent> {
        self.into_iter().map(Into::into).collect()
    }
}

impl<'a, A> Agents for &'a [A]
where
    &'a A: Into<AnyAgent>,
{
    fn into_agents(self) -> Vec<AnyAgent> {
        self.iter().map(Into::into).collect()
    }
}

impl<'a, A> Agents for &'a Vec<A>
where
    &'a A: Into<AnyAgent>,
{
    fn into_agents(self) -> Vec<AnyAgent> {
        self.as_slice().into_agents()
    }
}

impl<'a, A, const N: usize> Agents for &'a [A; N]
where
    &'a A: Into<AnyAgent>,
{
    fn into_agents(self) -> Vec<AnyAgent> {
        self.as_slice().into_agents()
    }
}

/// `Agents` for tuples of each length from 1 to 12.
macro_rules! tuples {
    ($($name:ident)+) => {
        impl<$($name: Into<AnyAgent>),+> sealed::Sealed for ($($name,)+) {}

        impl<$($name: Into<AnyAgent>),+> Agents for ($($name,)+) {
            #[allow(non_snake_case)]
            fn into_agents(self) -> Vec<AnyAgent> {
                let ($($name,)+) = self;
                vec![$($name.into()),+]
            }
        }
    };
}

tuples!(A);
tuples!(A B);
tuples!(A B C);
tuples!(A B C D);
tuples!(A B C D E);
tuples!(A B C D E F);
tuples!(A B C D E F G);
tuples!(A B C D E F G H);
tuples!(A B C D E F G H I);
tuples!(A B C D E F G H I J);
tuples!(A B C D E F G H I J K);
tuples!(A B C D E F G H I J K L);
