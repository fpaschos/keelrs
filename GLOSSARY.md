# keelrs

A Flue-style agent framework for Rust: the model reasons, keelrs gives it a model, a sandbox, tools and conversations.
Terms follow Flue's where Flue has one.

## Language

**Agent**:
A named function that, on every turn, declares what the run uses (model, sandbox, skills, MCP servers, tools) and
returns the prompt.
_Avoid_: Agent definition, bot

**Agent context**:
What an agent function receives to make its declarations for the current turn.
_Avoid_: Hooks, harness

**Runtime**:
The running keelrs instance that knows the agents and hosts their instances.
_Avoid_: Harness, engine, server

**Agent instance**:
One conversation with an agent, addressed by an id; later dispatches to the same id continue it.
_Avoid_: Session, thread, conversation id

**Dispatch**:
Submitting a message to an agent instance; it returns a receipt, not the reply.
_Avoid_: Ask, send, prompt

**Receipt**:
The claim ticket for one dispatched message, used to read its reply later.
_Avoid_: Admission, ticket

**Read**:
Waiting for a dispatched message to settle and taking its reply.
_Avoid_: Await, poll

**Settled**:
The state of a dispatched message once it has its final outcome (a reply, a failure or an abort); until then it is pending, and once settled it never changes.
_Avoid_: Done, finished, completed

**Reply**:
The settled outcome of one dispatched message.
_Avoid_: Response, answer
