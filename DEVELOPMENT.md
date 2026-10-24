# Development roadmap

Working notes for improvements found while using Reason Network to coordinate real agent work. This is not a release
promise; each item should become a focused issue or pull request when implementation starts.

## Current

### Launch an agent with a task

Let an agent start a supported coding agent on a selected computer and in a selected repository, with an initial task.
The launched session must have a stable Reason Network address and know the exact coordinator session to reply to.

First slice:

- start one of the locally installed supported agents on the same computer;
- require an explicit working directory and task;
- return the new session address or a clear launch failure;
- deliver replies directly to the requesting session so a supported idle coordinator wakes;
- expose the same operation across computers without adding a separate remote shell mechanism;
- keep launch commands constrained to known agent executables and repository paths.

Progress on `feat/agent-launch`:

- implemented `start_agent` plus the `reason-network start` CLI for Claude Code, Codex and OpenCode;
- added same-person-only remote launch, target feature negotiation, absolute-directory validation and bounded tasks;
- stripped the coordinator harness's session variables from child processes;
- a local OpenCode one-shot completed its assigned task and messaged the coordinator, but OpenCode 1.18.5 did not
  register `opencode run` as an addressable Reason Network session and its CLI reply appeared as `solve/terminal`;
- a Claude Code launch correctly reports its immediate workspace-trust failure instead of claiming it started;
- the first remote test setup was reclaimed by the Linux systemd service before the development daemon got its lock;
  repeat after the OpenCode test session returns and temporarily stop, without disabling, that user service.
- OpenCode launch now uses a local headless server plus an attached task, so the worker server stays alive after its
  first task; `stop_run` terminates the recorded worker process locally or on the owner's other machine.

## Later

### OpenCode session identity and lifecycle

Make it obvious which OpenCode session is which and remove stopped sessions promptly. During the first live test,
`solvelangs-opencode` had no repository while `solvelangs-opencode-2` showed
`public-data-agent@worker/macbook/connectivity-smoke-test`; both remained visible after the user stopped one. A direct
message woke `solvelangs-opencode` and received a reply, but that session reported its actual working directory as
`/home/solvelangs/Work/Projects/public-data-agent`. The directory therefore was not reaching the team listing even
though the OpenCode plugin knew enough to deliver to the session. Conversely, `solvelangs-opencode-2` was listed as
the public-data-agent checkout but reported its actual working directory as `/home/solvelangs/Work/Labs/reason_testing`,
which is not a Git repository. Delivery and direct replies worked for both sessions; their displayed workspace metadata
was missing or assigned to the wrong session.

Investigate:

- whether OpenCode unload or session deletion events are reliably received;
- how stale sessions should be marked or expired when an end event is missed;
- whether names should include a clearer repository, branch, task, or user-selected label;
- how a coordinator can select a session without remembering generated suffixes.

### Private workgroups

Allow a coordinator to create a scoped group of agent sessions. Members can message within the group while the
coordinator is contacted only when a member deliberately escalates, requests a decision, or reports completion.

Define before implementation:

- membership and lifetime;
- whether a group is local to one project or can span repositories and computers;
- routing for replies, escalation, and completion;
- visibility and isolation from team-wide broadcasts;
- loop and message-volume safeguards.

### Coordinator workflow

Build a simple workflow on top of launch, direct wake-up, and groups: split a project into bounded tasks, assign them
to agents, see ownership and status, receive explicit escalation, and review results centrally. Keep the work state
inspectable and avoid relying on agents to emit routine status chatter.

### Resumable agent-run directory

Keep a durable directory of launched and previously used agent runs. A coordinator should be able to discover an old
worker, see where and why it was started, and resume it or start a fresh continuation without reconstructing the
command by hand.

Each run record should include:

- a stable Reason Network run id and the coordinator session that created it;
- person, machine, agent type, session address, native session id when available, and current status;
- repository path, branch, working directory, launch task, and timestamps;
- the last completion, escalation, or failure message and links to changed files when supplied;
- whether the native agent session can be resumed or only restarted from its recorded task.

The first useful operations are `list_runs`, `show_run`, `resume_run`, and `restart_run`. Records should survive daemon
restarts, distinguish a resumed conversation from a fresh attempt, and retain failed runs for diagnosis. The directory
must not store credentials or full transcripts by default.

### Coordinated restart workflow

Use `/restart` in the coordinator conversation as the shorthand for refreshing the Reason Network engine and notifying
connected agent sessions. The workflow should:

1. build or install the selected engine version on this computer;
2. restart the local daemon and verify its version;
3. notify each connected agent that its Reason Network tools need a reload;
4. report which sessions acknowledged the new version and which still need their conversation reopened;
5. avoid interrupting a busy worker unless the user explicitly asks for a forced restart.

Some clients can reload their MCP connection during the current conversation; others only load MCP tools when a new
conversation starts. The coordinator should make that difference visible instead of claiming every session restarted.
