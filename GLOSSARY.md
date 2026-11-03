# keelrs

A Flue-style agent framework for Rust: the model reasons, keelrs gives it a model, a sandbox, tools and conversations.
Terms follow Flue's where Flue has one.

## Language

**Agent**:
A named function that, on every turn, declares what the run uses (model, sandbox, skills, MCP servers, tools) and
returns its instructions.
_Avoid_: Agent definition, bot

**Instructions**:
The text an agent gives the model ahead of the conversation on each turn: what the agent function returns, followed by
everything added with `use_instruction`.
_Avoid_: Prompt, preamble, system prompt

**Agent context**:
What an agent function receives to make its declarations for the current turn.
_Avoid_: Hooks, harness

**Runtime**:
The running keelrs instance that knows the agents and hosts their instances.
_Avoid_: Harness, engine, server

**Agent instance**:
One conversation with an agent, addressed by an id; later dispatches to the same id continue it.
_Avoid_: Session, thread, conversation id

**Instance id**:
The name the application gives an agent instance, or asks keelrs to make up; the same id always reaches the same
conversation.
_Avoid_: Conversation id, session id, key

**Instance data**:
What an agent instance is about, given once when it is created and fixed for its whole life; every render reads it.
What changes from one dispatch to the next travels in the message instead.
_Avoid_: Initial data, props, input, state

**Dispatch**:
Submitting a message to an agent instance; it returns a receipt, not the reply.
_Avoid_: Ask, send, prompt

**Receipt**:
The claim ticket for one dispatched message, used to read its reply later.
_Avoid_: Admission, ticket

**Render**:
One run of the agent function, before a turn's model call; what it declares holds for that turn only.
_Avoid_: Config run, call, iteration

**Turn**:
One model call within a dispatch, with the agent function run before it; a dispatch takes one turn, plus one more for
each round of tool calls.
_Avoid_: Step, iteration, round

**Tool**:
A named function, described to the model, that the model may call during a turn; the agent declares it with `use_tool`
on each turn.
_Avoid_: Function, action

**Tool call**:
One request from the model to run a tool with arguments; its result goes back to the model.
_Avoid_: Invocation

**Host tool**:
A tool whose code acts on the machine the agent runs on; it is trusted, and it is where secrets are used, so the model sees only its arguments and result.
_Avoid_: Local tool, unsafe tool

**Sandboxed tool**:
A tool that acts only through the sandbox its turn declares.
_Avoid_: Harness tool, remote tool

**Built-in tool**:
A sandboxed tool that keelrs ships (`read`, `write`, `edit`, `bash`, `grep`, `glob`, `delete`, `move`). Every turn gets all of them unless it narrows them with `only`, and their names stay reserved even when a tool is left out.
_Avoid_: Default tool, core tool, harness tool

**Sandbox**:
Where sandboxed tools read files and run commands: this machine, or a container, microVM or remote host. Every agent declares exactly one on every turn. The application owns it and may share one between many agents; the agent itself always runs outside it.
_Avoid_: Workspace, environment, box

**Sandbox key**:
The name the application gives a sandbox so it can be found again, such as one key for every agent working on one task.
_Avoid_: Run id

**Sandbox id**:
The identity keelrs gives a sandbox, unique within the running process and shared by its clones and narrowed work dirs; it tells two sandboxes apart, and is lost on restart.
_Avoid_: Sandbox key

**Work dir**:
The directory inside a sandbox that one agent works in: relative paths start there and file tools cannot leave it.
_Avoid_: Cwd, working directory, mount

**Skill**:
A folder of instructions (`SKILL.md`, following the Agent Skills spec) and supporting files that the agent mounts with
`use_skill`; the model sees only its name and description until it activates the skill by name, and reads the other
files on demand.
_Avoid_: Plugin, prompt pack

**Skill discovery**:
Finding skills on disk in conventional folders (project before user) before the runtime starts, so agents can mount
them like embedded ones.
_Avoid_: Skill scan, auto-loading

**Read**:
Waiting for a dispatched message to settle and taking its reply.
_Avoid_: Await, poll

**Settled**:
The state of a dispatched message once it has its final outcome (a reply, a failure or an abort); until then it is pending, and once settled it never changes.
_Avoid_: Done, finished, completed

**Reply**:
The settled outcome of one dispatched message.
_Avoid_: Response, answer

**Structured result**:
A typed value a dispatched message settles with, which the model hands over by calling a tool; a caller asks for one per message.
_Avoid_: Report, output, final result
