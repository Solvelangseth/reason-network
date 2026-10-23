//! Starting a coding-agent session on one of the current person's machines.

use super::Daemon;
use crate::{
    agent,
    proto::{LaunchResult, LaunchSpec},
};
use anyhow::{Result, anyhow, bail};
use iroh::EndpointId;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use ulid::Ulid;

pub const FEATURE: &str = "launch";
const MAX_TASK: usize = 64 * 1024;
const REGISTER_WAIT: Duration = Duration::from_secs(12);

impl Daemon {
    pub(super) async fn launch(
        self: &Arc<Self>,
        from: String,
        machine: &str,
        agent_name: &str,
        cwd: &str,
        task: &str,
    ) -> Result<LaunchResult> {
        let coordinator = {
            let g = self.inner.lock().unwrap();
            let session = g
                .live
                .get(&from)
                .or_else(|| g.live.values().find(|l| self.addr(&l.name) == from || l.name == from))
                .ok_or_else(|| anyhow!("the requesting agent session is not registered"))?;
            self.addr(&session.name)
        };
        let launch = LaunchSpec {
            id: Ulid::generate().to_string(),
            coordinator,
            agent: normalized_agent(agent_name)?.into(),
            cwd: cwd.into(),
            task: checked_task(task)?.into(),
        };

        if machine == self.machine {
            return self.launch_here(launch).await;
        }
        let team = self.team_state().ok_or_else(|| anyhow!("this machine is not in a Reason Network team"))?;
        let member = team
            .active()
            .find(|m| m.person == self.person && m.machine == machine)
            .ok_or_else(|| anyhow!("no machine called '{machine}' belongs to {}", self.person))?;
        let net = self.net().ok_or_else(|| anyhow!("Reason Network is not connected"))?;
        let remote =
            net.remotes().get(&member.endpoint).cloned().ok_or_else(|| anyhow!("{machine} has not been seen"))?;
        if !remote.online {
            bail!("{machine} is offline");
        }
        if !remote.presence.has(FEATURE) {
            bail!("{machine} does not support agent launch yet; update Reason Network there first");
        }
        net.send_launch(&member.endpoint, &launch).await
    }

    /// A request arriving from another machine. Launch is deliberately restricted to another machine owned by the
    /// same person; ordinary teammates can message agents but cannot start processes here.
    pub(crate) async fn receive_launch(self: &Arc<Self>, peer: EndpointId, launch: LaunchSpec) -> Result<LaunchResult> {
        let team = self.team_state().ok_or_else(|| anyhow!("this machine is not in a Reason Network team"))?;
        let sender =
            team.member(&peer).filter(|m| m.active).ok_or_else(|| anyhow!("sender is not an active member"))?;
        if sender.person != self.person {
            bail!("only another machine owned by {} may launch an agent here", self.person);
        }
        let launch = LaunchSpec {
            agent: normalized_agent(&launch.agent)?.into(),
            task: checked_task(&launch.task)?.into(),
            ..launch
        };
        self.launch_here(launch).await
    }

    async fn launch_here(self: &Arc<Self>, launch: LaunchSpec) -> Result<LaunchResult> {
        let cwd = checked_cwd(&launch.cwd, &self.cfg)?;
        let before = {
            let g = self.inner.lock().unwrap();
            g.live.keys().cloned().collect::<std::collections::HashSet<_>>()
        };
        let prompt = format!(
            "{}\n\nYou were started by the Reason Network coordinator at {}. When you finish, need a decision, or are blocked, send a direct Reason Network message to that exact session. Include a concise result and any files changed.",
            launch.task.trim(),
            launch.coordinator
        );
        start(&launch.agent, &cwd, &prompt).await?;

        let deadline = tokio::time::Instant::now() + REGISTER_WAIT;
        let mut address = None;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let g = self.inner.lock().unwrap();
            if let Some(live) = g.live.iter().find_map(|(key, live)| {
                (!before.contains(key) && live.reg.agent == launch.agent && same_dir(&live.reg.cwd, &cwd))
                    .then_some(live)
            }) {
                address = Some(self.addr(&live.name));
                break;
            }
        }
        let note = address.as_ref().map_or_else(
            || "agent process started; its session has not registered yet".into(),
            |a| format!("agent started and registered as {a}"),
        );
        Ok(LaunchResult {
            id: launch.id,
            machine: self.machine.clone(),
            agent: launch.agent,
            cwd: cwd.to_string_lossy().into_owned(),
            address,
            note,
        })
    }
}

fn normalized_agent(name: &str) -> Result<&'static str> {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => Ok(agent::CLAUDE_CODE),
        "codex" => Ok("codex"),
        "opencode" => Ok("opencode"),
        _ => bail!("unsupported launch agent '{name}'; choose claude, codex, or opencode"),
    }
}

fn checked_task(task: &str) -> Result<&str> {
    let task = task.trim();
    if task.is_empty() {
        bail!("launch task is empty");
    }
    if task.len() > MAX_TASK {
        bail!("launch task is larger than {} KB", MAX_TASK / 1024);
    }
    Ok(task)
}

fn checked_cwd(cwd: &str, cfg: &crate::config::Config) -> Result<PathBuf> {
    let path = Path::new(cwd);
    if !path.is_absolute() {
        bail!("launch directory must be an absolute path on the target machine");
    }
    let path = path.canonicalize().map_err(|e| anyhow!("cannot open launch directory {}: {e}", path.display()))?;
    if !path.is_dir() {
        bail!("launch directory is not a directory: {}", path.display());
    }
    if cfg.off_in(&path.to_string_lossy()) {
        bail!("Reason Network is turned off for {}", path.display());
    }
    Ok(path)
}

fn same_dir(given: &str, expected: &Path) -> bool {
    Path::new(given).canonicalize().is_ok_and(|p| p == expected)
}

async fn start(agent_name: &str, cwd: &Path, prompt: &str) -> Result<()> {
    let bin_name = if agent_name == agent::CLAUDE_CODE { "claude" } else { agent_name };
    let bin = agent::find_bin(bin_name).ok_or_else(|| anyhow!("{bin_name} is not installed on this machine"))?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.current_dir(cwd).stdin(Stdio::null());
    // The daemon may have been started on demand from inside another agent's shell. None of that caller's session
    // identity belongs to the child; each launched harness establishes its own identity.
    for name in [
        "REASON_NETWORK_AGENT",
        "REASON_NETWORK_SESSION",
        "TINCAN_AGENT",
        "AI_AGENT",
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_PROJECT_DIR",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "CODEX_SANDBOX",
        "GROK_SESSION_ID",
        "GROK_WORKSPACE_ROOT",
        "GEMINI_CLI",
        "GEMINI_SESSION_ID",
        "CURSOR_AGENT",
        "OPENCODE",
        "OPENCODE_SESSION_ID",
        "COPILOT_CLI",
        "COPILOT_AGENT_SESSION_ID",
    ] {
        cmd.env_remove(name);
    }
    match agent_name {
        agent::CLAUDE_CODE => {
            cmd.arg("--bg").arg(prompt).stdout(Stdio::null()).stderr(Stdio::null());
        }
        "codex" => {
            cmd.arg("exec").arg(prompt).stdout(Stdio::null()).stderr(Stdio::null());
        }
        "opencode" => {
            cmd.arg("run").arg(prompt).stdout(Stdio::null()).stderr(Stdio::null());
        }
        _ => unreachable!(),
    }
    cmd.kill_on_drop(false).spawn().map_err(|e| anyhow!("could not start {bin_name}: {e}"))?;
    super::log(&format!("launch {agent_name} in {}", cwd.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_supported_agents_are_named() {
        assert_eq!(normalized_agent("Claude").unwrap(), agent::CLAUDE_CODE);
        assert_eq!(normalized_agent("codex").unwrap(), "codex");
        assert_eq!(normalized_agent(" opencode ").unwrap(), "opencode");
        assert!(normalized_agent("sh").unwrap_err().to_string().contains("unsupported launch agent"));
    }

    #[test]
    fn tasks_are_bounded_and_nonempty() {
        assert_eq!(checked_task("  inspect this  ").unwrap(), "inspect this");
        assert!(checked_task(" \n ").is_err());
        assert!(checked_task(&"x".repeat(MAX_TASK + 1)).is_err());
    }
}
