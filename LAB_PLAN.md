# Multi-agent lab plan

Goal: use Claude Code, Codex and opencode as a working team over Reason Network, and use that team to develop Reason
Network itself (the dogfooding loop). Claude Code (`solve/solvelangs-claude`) is the coordinator.

## Starting point (2026-10-29)

- Fork `Solvelangseth/reason-network` (upstream `reason-machines/reason-network`), cloned to `~/projects/reason-network`.
- `feat/agent-launch` (8 commits, not merged, no PR) already adds `start_agent` / `stop_run` and
  `reason-network start` for Claude Code, Codex and opencode. `DEVELOPMENT.md` there lists the next ideas: opencode
  session identity, private workgroups, coordinator workflow, resumable run directory, coordinated restart.
- Installed: claude, codex 0.163.0, opencode 2.1.3, cargo, gh (logged in). The network daemon runs from the Reason CLI
  (systemd user unit `com.reasonmachines.network.service`).
- On the network now: this Claude session, one idle opencode session, and five Claude sessions on the MacBook. No
  Codex session yet.

## Ground rules

- **Never develop against the live daemon.** Dev builds run with `REASON_NETWORK_HOME=~/.reason/network-dev` on their
  own test network, so a broken build can't take down the real network or the sessions on the MacBook. The live
  daemon is only updated after a change is merged.
- **One worker, one worktree, one branch.** Each worker gets `git worktree add ../rn-<task> -b <task>`, so no two
  agents edit the same checkout.
- **PRs go to the fork** (`Solvelangseth/reason-network`), one change per PR as CONTRIBUTING.md asks. The coordinator
  reviews; a different agent from the author does a second review; you merge.
- **Gate every PR:** `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`, plus the relevant
  `tests/e2e/*.py`.
- **Never change the wire identifiers** in `src/net.rs`, `src/team.rs`, `src/invite.rs`.
- Workers message the coordinator only to report done, ask a blocking question, or escalate. No status chatter.

## Phase 0: lab setup

1. Build the branch: `cargo build`; run the unit tests to get a baseline.
2. Start a dev daemon under `REASON_NETWORK_HOME=~/.reason/network-dev` with its own test network, and connect
   test sessions of each agent to it (MCP config pointing at the dev binary).
3. Get a Codex session onto the **live** network (`reason network setup`, then open codex) so all three agents are
   present.
4. Create a small sandbox repo `~/projects/agent-lab` with toy tasks (a little CLI with a few bugs and missing
   features) for experiments that shouldn't touch real code.

## Phase 1: messaging matrix (experiments)

On the live network, in `agent-lab`, check every pair and record the results in `lab/results.md`:

| from \ to | Claude | Codex | opencode |
|---|---|---|---|
| Claude | wake idle, reply | wake idle, mid-turn delivery | wake idle, reply |
| Codex | … | … | … |
| opencode | … | … | … |

For each pair: delivery while idle, while busy, reply routing back to the right session, file attachment, `ask_user`
approval. Then one small 3-agent task: Claude splits it, Codex and opencode each implement a part, they talk to each
other directly about the interface, and Claude reviews. Every failure goes to `lab/results.md` and becomes an issue.

## Phase 2: finish `feat/agent-launch`

The coordinator does the whole setup of a worker, with no steps for you: it creates the folder or worktree, starts the
agent with its task and the coordinator's address, and the session opens in a window you can watch and type into.

1. **Visible launch.** Today `start_agent` runs workers headless. Add a visible mode: the agent runs inside a named
   `tmux` session (`rn-<task>`), so it outlives its window and can be reattached, and a terminal window attached to
   it pops open (Linux: `xdg-terminal-exec` / `uwsm-app`, a Hyprland workspace per worker; macOS: `open -a Terminal`).
   Headless stays available for remote machines and CI.
2. **Workspace setup.** `start_agent` takes a repo plus a task name and creates `git worktree add ../<repo>-<task> -b
   <task>` itself (or an empty folder for scratch work), refusing paths outside the allowed roots.
3. **Pre-wired sessions.** The launched session gets a chosen name (`<repo>-<task>-<agent>`, not generated suffixes)
   and the coordinator's address in its first prompt, and shows up in `who` before `start_agent` returns.
4. Rebase on `main` (0.12.11), fix conflicts, get CI green.
5. Fix the known gap: `opencode run` doesn't register as an addressable session (replies showed up as
   `solve/terminal`).
6. Fix the Claude Code workspace-trust failure on launch, or report it clearly enough to act on.
7. Test `start_agent` for all three agents, local and across machines (laptop ↔ MacBook), on the dev network.
8. Open the PR. This is the base for everything after it: the coordinator can now start workers itself.

## Phase 3: features, built by the agent team

In this order. Each is one PR, done by a worker the coordinator launches and reviewed by another agent:

1. **opencode session identity**: the right cwd/repo in the listing, stale sessions expire when the end event is
   missed. Reproduced 2026-10-29: three opencode sessions opened in different folders all listed as `~`. Small, well scoped, a good first task for a worker.
2. **Run directory**: `list_runs`, `show_run`, `resume_run`, `restart_run`. Records survive daemon restarts and keep
   the task, coordinator, status and last report. No transcripts or credentials.
3. **Workgroups**: a coordinator-scoped group whose members message each other freely and contact the coordinator
   only to escalate or report done. Includes loop and message-volume limits, which matter once agents talk to agents.
4. **Coordinator workflow**: split, assign, see ownership and status, collect results, built on 1 to 3.
5. **Coordinated restart (`!restart`)**: rebuild, restart the daemon, tell each session to reload its tools, report
   who did.
6. From DESIGN.md M4, if there's time: file-overlap warnings (two agents editing the same file), which is useful for
   exactly this setup.

## Phase 4: upstream

Offer the changes that work well upstream to `reason-machines/reason-network`, one at a time, once they've been used
for real on the fork.

## Who does what

| Role | Agent | Why |
|---|---|---|
| Coordinator, reviewer, merge prep | Claude Code (this session) | plans, splits, reviews, runs e2e tests |
| Rust implementation worker | Codex | strong on focused, well-specified changes |
| TypeScript plugin and lab worker | opencode | owns `opencode-plugin.ts` and is the agent being fixed in 3.1 |
| Second reviewer | whichever agent didn't write the PR | an independent look |
| Merge, approvals | you | `ask_user` for anything destructive or outward-facing |
