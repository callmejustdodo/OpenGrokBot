use super::*;
use crate::ask::{
    fork::PARENT_SESSION_ENV,
    stream::{StreamEvent, parse_line},
};

impl ClaudeSupervisor {
    pub(super) async fn execute_fork(&self, agent: &Agent, fork: &ForkRun) -> Result<ForkResult> {
        let config = &self.inner.config;
        let settings = json!({"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":config.pre_tool_use_command}]}]}}).to_string();
        let options = match fork.tools {
            ForkTools::None => LaunchOptions::default(),
            ForkTools::Gated => LaunchOptions {
                permission_prompt_tool: Some("mcp__formation__permission"),
                mcp_config: Some(&config.mcp_config),
                settings: Some(&settings),
                permission_mode: None,
            },
        };
        let mut args = protocol::launch_args(
            Launch::Resume {
                session_id: &fork.from.0,
            },
            options,
        );
        args.push("--fork-session".into());
        if fork.tools == ForkTools::None {
            args.extend(crate::ask::fork::ISOLATION_ARGS.map(String::from));
            args.extend(["--system-prompt-snapshot".into(), "off".into()]);
        }
        let mut command = Command::new(&config.binary);
        command.args(args).current_dir(&agent.folder_path);
        for key in PARENT_SESSION_ENV {
            command.env_remove(key);
        }
        let mut child = command
            .envs(&config.env)
            .env("CLAUDE_CONFIG_DIR", &config.config_dir)
            .env("FORMATION_RUN_ID", fork.run_id.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| error(ErrorCode::HarnessUnavailable, e))?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        write(
            &mut stdin,
            protocol::encode_user_message(&fork.item.text, None),
        )
        .await?;
        drop(stdin);
        let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
        let mut session = None;
        while let Some(line) = lines.next_line().await.map_err(internal)? {
            match parse_line(&line).map_err(internal)? {
                StreamEvent::Init {
                    session_id, tools, ..
                } => {
                    if fork.tools == ForkTools::None && tools.is_some_and(|t| !t.is_empty()) {
                        return Err(internal("tool-free fork started with tools"));
                    }
                    if let Some(sid) = session_id {
                        let info = self.fork_session(agent, fork, sid)?;
                        self.inner.store.put_fork_session(fork.run_id, &info)?;
                        self.inner
                            .forks
                            .lock()
                            .unwrap()
                            .get_mut(&fork.run_id)
                            .unwrap()
                            .session_id = info.session_id.clone();
                        session = Some(info);
                    }
                }
                StreamEvent::Result(result) => {
                    let mut session = match session {
                        Some(session) => session,
                        None => self.fork_session(
                            agent,
                            fork,
                            result
                                .session_id
                                .ok_or_else(|| internal("fork named no session"))?,
                        )?,
                    };
                    session.last_activity_at = Some(now());
                    self.inner.store.put_fork_session(fork.run_id, &session)?;
                    // Print-mode may linger after its final result; reap it before returning.
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Ok(ForkResult {
                        session,
                        result: TurnResult {
                            outcome: if result.is_error {
                                TurnOutcome::Failed
                            } else {
                                TurnOutcome::Completed
                            },
                            result: Some(result.text),
                        },
                    });
                }
                _ => {}
            }
        }
        let status = child.wait().await.map_err(internal)?;
        Err(error(
            ErrorCode::HarnessUnavailable,
            format!("Claude fork exited without a result: {status}"),
        ))
    }

    fn fork_session(&self, agent: &Agent, fork: &ForkRun, sid: String) -> Result<SessionInfo> {
        if sid == fork.from.0 {
            return Err(internal("Claude did not fork the original session"));
        }
        Ok(SessionInfo {
            agent_id: agent.id,
            transcript_path: transcript::transcript_path(
                &self.inner.config.config_dir,
                &agent.folder_path,
                &sid,
            )
            .map_err(internal)?,
            session_id: SessionId(sid),
            cwd: agent.folder_path.clone(),
            started_at: now(),
            last_activity_at: None,
        })
    }
}
