//! The runtime: the agents it knows and the agent instances it hosts.

use std::any::Any;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;

use crate::{Agent, AgentInstance, Error, InstanceId, Result};

/// The running keelrs instance that knows the agents and hosts their instances.
///
/// Dropping it stops every agent instance: runs in flight and queued settle as
/// [`Outcome::Aborted`](crate::Outcome::Aborted).
#[derive(Debug)]
pub struct Runtime {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    agents: HashMap<String, AnyAgent>,
    /// Instances already spawned, keyed by `(agent name, instance id)`, so the
    /// same id continues the same conversation.
    instances: Mutex<HashMap<(String, InstanceId), AgentInstance>>,
    /// Parent of every instance token; cancelled when the runtime is dropped.
    shutdown: CancellationToken,
}

impl Runtime {
    /// Starts a runtime hosting `agents`: a tuple, for agents with different
    /// data types, or an array, `Vec` or slice of one type, each by value or
    /// by reference.
    ///
    /// Fails with [`Error::DuplicateAgent`] when two agents share a name.
    pub fn start(agents: impl Agents) -> Result<Self> {
        let mut registry = HashMap::new();
        for agent in agents.into_agents() {
            match registry.entry(agent.name.clone()) {
                Entry::Occupied(_) => return Err(Error::DuplicateAgent(agent.name)),
                Entry::Vacant(slot) => {
                    slot.insert(agent);
                }
            }
        }
        Ok(Self {
            inner: Arc::new(Inner {
                agents: registry,
                ..Inner::default()
            }),
        })
    }

    /// The instance `id` of an agent without data: created on first use,
    /// continued after.
    ///
    /// Fails as [`instance_with`](Self::instance_with) does.
    pub fn instance(&self, agent: &Agent, id: InstanceId) -> Result<AgentInstance> {
        self.instance_with(agent, id, ())
    }

    /// The instance `id` of `agent`, created with its frozen `data` on first
    /// use. On an existing id the instance is reused: its original data is
    /// kept and `data` dropped, with no error.
    ///
    /// Fails with [`Error::InvalidInstanceId`] on a blank id, and with
    /// [`Error::UnknownAgent`] unless `agent` is the very one given to
    /// [`Runtime::start`] (not just one with its name); the instance runs with
    /// that registered agent's settings. Fails with [`Error::NoTokioRuntime`]
    /// outside a Tokio runtime.
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
        // Checked before the lock: the actor's spawn would panic while holding it.
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(Error::NoTokioRuntime);
        }
        let instance = self
            .inner
            .instances
            .lock()
            .unwrap()
            .entry((agent.name().to_owned(), id.clone()))
            .or_insert_with(|| {
                AgentInstance::spawn(registered.bind(id.clone(), data), id, &self.inner.shutdown)
            })
            .clone();
        Ok(instance)
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
