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
use crate::agent::instance::{self, InstanceKey, Instances};
use crate::agent::rig;
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

struct Inner {
    agents: HashMap<String, AnyAgent>,
    /// Each instance asked for so far, so the same id continues the same
    /// conversation; read by the instances' spawner.
    created: Created,
    /// The instances' actors, spawned on their first call.
    instances: Instances,
    /// Parent of every instance token; cancelled when the runtime is dropped.
    shutdown: CancellationToken,
}

/// Each instance asked for, keyed by `(agent name, instance id)`: how its
/// turns are set up, and its data as JSON.
type Created = Arc<Mutex<HashMap<InstanceKey, (TurnSetup, Value)>>>;

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("agents", &self.agents)
            .finish_non_exhaustive()
    }
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
    /// Fails with [`Error::DuplicateAgent`] when two agents share a name,
    /// and with [`Error::NoTokioRuntime`] outside a Tokio runtime.
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
    /// instance runs with that registered agent's settings.
    ///
    /// Takes no lease and starts nothing: the instance's first call does.
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
        let name = agent.name().to_owned();
        let json = serde_json::to_value(&data).map_err(|e| Error::InvalidInstanceData {
            agent: name.clone(),
            id: id.clone(),
            reason: e.to_string(),
        })?;
        let key = (name, id);
        match self.inner.created.lock().entry(key.clone()) {
            Entry::Occupied(entry) => {
                if entry.get().1 != json {
                    let (agent, id) = key;
                    return Err(Error::InstanceDataConflict { agent, id });
                }
            }
            Entry::Vacant(entry) => {
                entry.insert((TurnSetup::new(registered, key.1.clone(), data), json));
            }
        }
        Ok(AgentInstance::new(self.inner.instances.get(key)))
    }
}

/// Settings for a [`Runtime`], from [`Runtime::builder`].
#[derive(Debug, Default)]
pub struct RuntimeBuilder {
    store: Store,
    #[cfg(feature = "testing")]
    model: Option<crate::testing::ScriptedModel>,
}

impl RuntimeBuilder {
    /// Keeps the instances' events in `store` rather than in memory.
    #[must_use]
    pub fn store(mut self, store: Store) -> Self {
        self.store = store;
        self
    }

    /// Answers every instance's turns from `model`, in place of the model
    /// each declares (the `testing` feature).
    #[cfg(feature = "testing")]
    #[must_use]
    pub fn model(mut self, model: crate::testing::ScriptedModel) -> Self {
        self.model = Some(model);
        self
    }

    /// Starts the runtime hosting `agents`, as [`Runtime::start`] does.
    ///
    /// Fails as [`Runtime::start`] does.
    pub fn start(self, agents: impl Agents) -> Result<Runtime> {
        let mut registry = HashMap::new();
        for agent in agents.into_agents() {
            let name = agent.name.clone();
            if registry.insert(name.clone(), agent).is_some() {
                return Err(Error::DuplicateAgent(name));
            }
        }
        // The instances' actors are spawned on this runtime, from any thread.
        let rt = tokio::runtime::Handle::try_current().map_err(|_| Error::NoTokioRuntime)?;
        let connect = rig::connect_catalog();
        #[cfg(feature = "testing")]
        let connect = self.model.map_or(connect, |model| model.connect());
        let (store, shutdown, created) = (self.store, CancellationToken::new(), Created::default());
        let spawn = {
            let (shutdown, created) = (shutdown.clone(), Arc::clone(&created));
            move |key: &InstanceKey| {
                let (setup, data) = created.lock()[key].clone();
                instance::spawn(
                    &rt,
                    &shutdown,
                    key,
                    setup,
                    data,
                    Arc::clone(&connect),
                    store.clone(),
                )
            }
        };
        Ok(Runtime {
            inner: Arc::new(Inner {
                agents: registry,
                created,
                instances: Instances::new(spawn, shutdown.clone()),
                shutdown,
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
