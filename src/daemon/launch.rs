//! Starting a coding-agent session on one of the current person's machines.

use super::Daemon;
use crate::{
    agent,
    proto::{LaunchResult, LaunchSpec, StopSpec},
};
use anyhow::{Result, anyhow, bail};
use iroh::EndpointId;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use ulid::Ulid;

pub const FEATURE: &str = "launch";
const MAX_TASK: usize = 64 * 1024;
const REGISTER_WAIT: Duration = Duration::from_secs(12);

impl Daemon {
    pub(super) async fn stop_run(&self, from: &str, machine: &str, id: &str) -> Result<String> {
        if machine != self.machine {
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
            return net.send_stop(&member.endpoint, &StopSpec { id: id.into(), coordinator: from.into() }).await;
        }
        let g = self.inner.lock().unwrap();
        if !g.live.contains_key(from) && !g.live.values().any(|l| self.addr(&l.name) == from || l.name == from) {
            bail!("the requesting agent session is not registered");
        }
        drop(g);
        self.stop_pid(id)
    }

    pub(crate) async fn receive_stop(self: &Arc<Self>, peer: EndpointId, stop: StopSpec) -> Result<String> {
        let team = self.team_state().ok_or_else(|| anyhow!("this machine is not in a Reason Network team"))?;
        let sender =
            team.member(&peer).filter(|m| m.active).ok_or_else(|| anyhow!("sender is not an active member"))?;
        if sender.person != self.person {
            bail!("only another machine owned by {} may stop a worker here", self.person);
        }
        self.stop_pid(&stop.id)
    }

    fn stop_pid(&self, id: &str) -> Result<String> {
        let g = &mut *self.inner.lock().unwrap();
        let Some(pid) = g.launches.remove(id) else {
            bail!("no running local worker with run id {id}");
        };
        #[cfg(unix)]
        {
            let result = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            if result != 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::NotFound {
                bail!("could not stop worker process {pid}: {}", std::io::Error::last_os_error());
            }
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).status();
        }
        Ok(format!("stopped worker {id} (process {pid})"))
    }

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
        let pid = start(&launch.agent, &cwd, &prompt).await?;
        if let Some(pid) = pid {
            self.inner.lock().unwrap().launches.insert(launch.id.clone(), pid);
        }

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
            pid,
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

async fn start(agent_name: &str, cwd: &Path, prompt: &str) -> Result<Option<u32>> {
    let bin_name = if agent_name == agent::CLAUDE_CODE { "claude" } else { agent_name };
    let bin = agent::find_bin(bin_name).ok_or_else(|| anyhow!("{bin_name} is not installed on this machine"))?;
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    clean_agent_env(&mut cmd);
    if agent_name == "opencode" {
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
        let mut server = tokio::process::Command::new(&bin);
        server
            .current_dir(cwd)
            .args(["serve", "--hostname", "127.0.0.1", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        clean_agent_env(&mut server);
        let mut server =
            server.kill_on_drop(false).spawn().map_err(|e| anyhow!("could not start opencode server: {e}"))?;
        let pid = server.id().ok_or_else(|| anyhow!("opencode server did not expose a process id"))?;
        let address = format!("http://127.0.0.1:{port}");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while tokio::time::Instant::now() < deadline {
            if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                let mut client = tokio::process::Command::new(&bin);
                client
                    .current_dir(cwd)
                    .args(["run", "--attach", &address, "--dir", &cwd.to_string_lossy(), prompt])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .kill_on_drop(false);
                clean_agent_env(&mut client);
                client.spawn().map_err(|e| anyhow!("could not send the initial opencode task: {e}"))?;
                tokio::spawn(async move {
                    let _ = server.wait().await;
                });
                super::log(&format!("launch persistent opencode server in {}", cwd.display()));
                return Ok(Some(pid));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = server.kill().await;
        bail!("opencode server did not start listening within 10 seconds");
    }
    match agent_name {
        agent::CLAUDE_CODE => {
            cmd.arg("--bg").arg(prompt);
        }
        "codex" => {
            cmd.arg("exec").arg(prompt);
        }
        "opencode" => {
            cmd.arg("run").arg(prompt);
        }
        _ => unreachable!(),
    }
    let mut child = cmd.kill_on_drop(false).spawn().map_err(|e| anyhow!("could not start {bin_name}: {e}"))?;
    let mut stderr = child.stderr.take();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let pid = child.id();
    if let Some(status) = child.try_wait().map_err(|e| anyhow!("could not inspect {bin_name}: {e}"))? {
        let mut detail = String::new();
        if let Some(mut pipe) = stderr.take() {
            let _ = pipe.read_to_string(&mut detail).await;
        }
        if !status.success() {
            let detail = detail.trim();
            bail!(
                "{bin_name} exited before starting{}",
                if detail.is_empty() { String::new() } else { format!(": {detail}") }
            );
        }
    } else {
        // Reap the long-running worker and drain its diagnostics without tying its lifetime to this request.
        tokio::spawn(async move {
            if let Some(mut pipe) = stderr {
                let mut sink = Vec::new();
                let _ = pipe.read_to_end(&mut sink).await;
            }
            let _ = child.wait().await;
        });
    }
    super::log(&format!("launch {agent_name} in {}", cwd.display()));
    Ok(pid)
}

fn clean_agent_env(cmd: &mut tokio::process::Command) {
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
