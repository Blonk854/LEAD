use std::sync::Arc;

use agent_client_protocol::schema as acp;
use gpui::{App, AppContext as _, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, RefreshKind, System, UpdateKind};
use ui::SharedString;

use crate::{AgentTool, ToolCallEventStream, ToolInput, ToolPermissionContext};

/// Inspects running processes or terminates one after explicit approval.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ProcessControlToolInput {
    /// `list`, `info`, `kill`, `system_info`, `service_list`,
    /// `service_start`, or `service_stop`.
    pub action: String,
    /// Process id. Required for `info` and `kill`.
    pub pid: Option<u32>,
    /// Optional case-insensitive process-name filter for `list`.
    pub name_filter: Option<String>,
    /// Service name for `service_start` and `service_stop`.
    pub service_name: Option<String>,
}

pub struct ProcessControlTool;

impl ProcessControlTool {
    fn system() -> System {
        System::new_with_specifics(
            RefreshKind::nothing()
                .with_memory(MemoryRefreshKind::everything())
                .with_cpu(sysinfo::CpuRefreshKind::everything())
                .with_processes(
                    ProcessRefreshKind::nothing()
                        .without_tasks()
                        .with_memory()
                        .with_cpu()
                        .with_cmd(UpdateKind::Always),
                ),
        )
    }
}

impl AgentTool for ProcessControlTool {
    type Input = ProcessControlToolInput;
    type Output = String;

    const NAME: &'static str = "process_control";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Execute
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => match input.pid {
                Some(pid) => format!("Process {} (PID {pid})", input.action).into(),
                None => format!("Process {}", input.action).into(),
            },
            Err(_) => "Control process".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(|error| error.to_string())?;
            if !cx.update(|cx| crate::full_access_enabled(cx)) {
                return Err(
                    "Unleashed is disabled. Enable it in Settings → AI → Unleashed (or set agent.full_access.enabled).".into(),
                );
            }

            let action = input.action.to_ascii_lowercase();
            if action == "kill" {
                let pid = input
                    .pid
                    .ok_or_else(|| "process_control kill requires pid".to_string())?;
                validate_kill_pid(pid)?;
                let authorize = cx.update(|cx| {
                    event_stream.authorize_always_prompt_unless_denied(
                        format!("Terminate process {pid}"),
                        ToolPermissionContext::new(Self::NAME, vec![format!("kill:{pid}")]),
                        cx,
                    )
                });
                authorize.await.map_err(|error| error.to_string())?;
            } else if matches!(action.as_str(), "service_start" | "service_stop") {
                let service = input
                    .service_name
                    .as_deref()
                    .filter(|service| !service.trim().is_empty())
                    .ok_or_else(|| format!("{action} requires service_name"))?;
                let authorize = cx.update(|cx| {
                    event_stream.authorize_always_prompt_unless_denied(
                        format!("{} service {service}", action.replace('_', " ")),
                        ToolPermissionContext::new(
                            Self::NAME,
                            vec![format!("{action}:{service}")],
                        ),
                        cx,
                    )
                });
                authorize.await.map_err(|error| error.to_string())?;
            } else {
                let permission_input = match action.as_str() {
                    "list" => format!(
                        "list:{}",
                        input.name_filter.as_deref().unwrap_or("")
                    ),
                    "info" => format!("info:{}", input.pid.map(|pid| pid.to_string()).unwrap_or_default()),
                    "system_info" => "system_info".to_string(),
                    "service_list" => "service_list".to_string(),
                    other => other.to_string(),
                };
                let authorize = cx.update(|cx| {
                    event_stream.authorize(
                        format!("Process {}", action.replace('_', " ")),
                        ToolPermissionContext::new(Self::NAME, vec![permission_input]),
                        cx,
                    )
                });
                authorize.await.map_err(|error| error.to_string())?;
            }

            cx.background_spawn(async move {
                let system = Self::system();
                match action.as_str() {
                    "list" => {
                        let filter = input.name_filter.as_deref().map(str::to_ascii_lowercase);
                        let mut rows = system
                            .processes()
                            .values()
                            .filter(|process| {
                                filter.as_ref().is_none_or(|filter| {
                                    process
                                        .name()
                                        .to_string_lossy()
                                        .to_ascii_lowercase()
                                        .contains(filter)
                                })
                            })
                            .map(|process| {
                                format!(
                                    "{}\t{}\t{} bytes\t{}",
                                    process.pid().as_u32(),
                                    process.name().to_string_lossy(),
                                    process.memory(),
                                    process
                                        .cmd()
                                        .iter()
                                        .map(|part| part.to_string_lossy())
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                )
                            })
                            .collect::<Vec<_>>();
                        rows.sort();
                        Ok(rows.join("\n"))
                    }
                    "info" => {
                        let pid = input
                            .pid
                            .ok_or_else(|| "process_control info requires pid".to_string())?;
                        let process = system
                            .process(Pid::from_u32(pid))
                            .ok_or_else(|| format!("Process {pid} was not found."))?;
                        Ok(format!(
                            "pid: {pid}\nname: {}\nstatus: {:?}\nmemory_bytes: {}\ncpu_percent: {}\ncommand: {}",
                            process.name().to_string_lossy(),
                            process.status(),
                            process.memory(),
                            process.cpu_usage(),
                            process
                                .cmd()
                                .iter()
                                .map(|part| part.to_string_lossy())
                                .collect::<Vec<_>>()
                                .join(" ")
                        ))
                    }
                    "system_info" => Ok(format!(
                        "host: {}\nos: {}\nos_version: {}\nkernel: {}\narchitecture: {}\nphysical_cores: {}\ntotal_memory_bytes: {}\navailable_memory_bytes: {}",
                        System::host_name().unwrap_or_else(|| "unknown".into()),
                        System::name().unwrap_or_else(|| "unknown".into()),
                        System::os_version().unwrap_or_else(|| "unknown".into()),
                        System::kernel_version().unwrap_or_else(|| "unknown".into()),
                        System::cpu_arch(),
                        System::physical_core_count().unwrap_or(0),
                        system.total_memory(),
                        system.available_memory(),
                    )),
                    "kill" => {
                        let pid = input.pid.expect("validated above");
                        let process = system
                            .process(Pid::from_u32(pid))
                            .ok_or_else(|| format!("Process {pid} was not found."))?;
                        if process.kill() {
                            Ok(format!("Termination signal sent to process {pid}."))
                        } else {
                            Err(format!("Failed to terminate process {pid}."))
                        }
                    }
                    "service_list" => service_command("query", None),
                    "service_start" => {
                        service_command("start", input.service_name.as_deref())
                    }
                    "service_stop" => service_command("stop", input.service_name.as_deref()),
                    _ => Err("Unsupported action. Use list, info, kill, system_info, service_list, service_start, or service_stop.".into()),
                }
            })
            .await
        })
    }
}

#[cfg(windows)]
fn service_command(action: &str, service_name: Option<&str>) -> Result<String, String> {
    let mut command = std::process::Command::new("sc.exe");
    match (action, service_name) {
        ("query", None) => {
            command.args(["query", "type=", "service", "state=", "all"]);
        }
        (action @ ("start" | "stop"), Some(service_name)) => {
            command.args([action, service_name]);
        }
        _ => return Err("A service name is required for this action.".into()),
    }
    let output = command.output().map_err(|error| error.to_string())?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(text)
    } else {
        Err(format!("sc.exe exited with {}:\n{text}", output.status))
    }
}

#[cfg(not(windows))]
fn service_command(_action: &str, _service_name: Option<&str>) -> Result<String, String> {
    Err("Service control is currently implemented only on Windows.".into())
}

fn validate_kill_pid(pid: u32) -> Result<(), String> {
    if pid == 0 || pid == std::process::id() {
        Err("Refusing to terminate PID 0 or LEAD's own process.".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_own_pid_and_pid_zero() {
        assert!(validate_kill_pid(0).is_err());
        assert!(validate_kill_pid(std::process::id()).is_err());
    }
}
