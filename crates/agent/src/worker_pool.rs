//! Schedules hybrid subagent work across multiple worker model endpoints.

use agent_settings::{NetworkAgentWorkerEndpoint, NetworkAgentWorkers, WorkerScheduling};
use settings::LanguageModelSelection;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

static WORKER_POOL: LazyLock<Mutex<WorkerPoolRuntime>> =
    LazyLock::new(|| Mutex::new(WorkerPoolRuntime::default()));

#[derive(Debug, Default)]
struct WorkerPoolRuntime {
    active_counts: HashMap<String, u32>,
    session_workers: HashMap<String, String>,
    round_robin_index: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerAssignment {
    pub worker_id: String,
    pub model: LanguageModelSelection,
}

impl WorkerAssignment {
    fn from_endpoint(endpoint: &NetworkAgentWorkerEndpoint) -> Self {
        Self {
            worker_id: endpoint.id.clone(),
            model: LanguageModelSelection {
                provider: endpoint.provider.clone(),
                model: endpoint.model.clone(),
                enable_thinking: false,
                effort: None,
                speed: None,
            },
        }
    }
}

/// Acquires a worker for a new or resumed subagent session.
pub fn acquire_worker(
    workers: &NetworkAgentWorkers,
    default_model: &Option<LanguageModelSelection>,
    session_id: Option<&str>,
) -> Option<WorkerAssignment> {
    if !workers.is_active() {
        return None;
    }

    let endpoints = workers.resolved_endpoints(default_model);
    if endpoints.is_empty() {
        return None;
    }

    let mut pool = WORKER_POOL.lock().ok()?;

    if let Some(session_id) = session_id
        && let Some(worker_id) = pool.session_workers.get(session_id).cloned()
        && let Some(endpoint) = endpoints.iter().find(|endpoint| endpoint.id == worker_id)
    {
        return Some(WorkerAssignment::from_endpoint(endpoint));
    }

    let worker_id = select_worker(workers.scheduling, &endpoints, &mut *pool)?;
    if let Some(session_id) = session_id {
        pool.session_workers
            .insert(session_id.to_string(), worker_id.clone());
    }

    endpoints
        .iter()
        .find(|endpoint| endpoint.id == worker_id)
        .map(WorkerAssignment::from_endpoint)
}

/// Marks the start of an active subagent turn on a worker.
pub fn increment_worker(worker_id: &str) {
    if let Ok(mut pool) = WORKER_POOL.lock() {
        *pool.active_counts.entry(worker_id.to_string()).or_insert(0) += 1;
    }
}

/// Records the worker assignment for a newly created subagent session.
pub fn assign_session_worker(session_id: &str, worker_id: &str) {
    if let Ok(mut pool) = WORKER_POOL.lock() {
        pool.session_workers
            .insert(session_id.to_string(), worker_id.to_string());
    }
}

/// Releases a worker after a subagent turn completes.
pub fn release_worker(worker_id: &str) {
    if let Ok(mut pool) = WORKER_POOL.lock()
        && let Some(count) = pool.active_counts.get_mut(worker_id)
    {
        *count = count.saturating_sub(1);
    }
}

fn select_worker(
    scheduling: WorkerScheduling,
    endpoints: &[NetworkAgentWorkerEndpoint],
    pool: &mut WorkerPoolRuntime,
) -> Option<String> {
    let enabled: Vec<&NetworkAgentWorkerEndpoint> = endpoints.iter().collect();
    if enabled.is_empty() {
        return None;
    }

    match scheduling {
        WorkerScheduling::RoundRobin => select_round_robin(&enabled, pool),
        WorkerScheduling::Pool => select_pool(&enabled, pool),
        WorkerScheduling::LeastBusy => select_least_busy(&enabled, pool),
    }
}

fn select_round_robin(
    endpoints: &[&NetworkAgentWorkerEndpoint],
    pool: &mut WorkerPoolRuntime,
) -> Option<String> {
    let len = endpoints.len();
    for offset in 0..len {
        let index = (pool.round_robin_index + offset) % len;
        let endpoint = endpoints[index];
        let active = pool.active_counts.get(&endpoint.id).copied().unwrap_or(0);
        if active < endpoint.max_concurrent {
            pool.round_robin_index = (index + 1) % len;
            return Some(endpoint.id.clone());
        }
    }
    select_least_busy(endpoints, pool)
}

fn select_pool(
    endpoints: &[&NetworkAgentWorkerEndpoint],
    pool: &mut WorkerPoolRuntime,
) -> Option<String> {
    if let Some(id) = endpoints.iter().find_map(|endpoint| {
        let active = pool.active_counts.get(&endpoint.id).copied().unwrap_or(0);
        (active < endpoint.max_concurrent).then(|| endpoint.id.clone())
    }) {
        return Some(id);
    }
    select_least_busy(endpoints, pool)
}

fn select_least_busy(
    endpoints: &[&NetworkAgentWorkerEndpoint],
    pool: &mut WorkerPoolRuntime,
) -> Option<String> {
    let mut best: Option<(&NetworkAgentWorkerEndpoint, f32, usize)> = None;
    for (index, endpoint) in endpoints.iter().enumerate() {
        let active = pool.active_counts.get(&endpoint.id).copied().unwrap_or(0);
        let load = active as f32 / endpoint.max_concurrent.max(1) as f32;
        if best.is_none()
            || load < best.unwrap().1
            || (load == best.unwrap().1 && index < best.unwrap().2)
        {
            best = Some((endpoint, load, index));
        }
    }
    best.map(|(endpoint, _, _)| endpoint.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use settings::LanguageModelProviderSetting;

    fn sample_workers() -> NetworkAgentWorkers {
        NetworkAgentWorkers {
            scheduling: WorkerScheduling::LeastBusy,
            max_workers: 10,
            include_default_model: false,
            endpoints: vec![
                NetworkAgentWorkerEndpoint {
                    id: "a".into(),
                    provider: LanguageModelProviderSetting("worker-a".into()),
                    model: "model-a".into(),
                    max_concurrent: 1,
                    enabled: true,
                },
                NetworkAgentWorkerEndpoint {
                    id: "b".into(),
                    provider: LanguageModelProviderSetting("worker-b".into()),
                    model: "model-b".into(),
                    max_concurrent: 1,
                    enabled: true,
                },
            ],
        }
    }

    fn reset_pool() {
        if let Ok(mut pool) = WORKER_POOL.lock() {
            *pool = WorkerPoolRuntime::default();
        }
    }

    #[test]
    fn least_busy_prefers_idle_worker() {
        reset_pool();
        let workers = sample_workers();
        let first = acquire_worker(&workers, &None, None).unwrap();
        increment_worker(&first.worker_id);
        release_worker(&first.worker_id);

        increment_worker("a");

        let second = acquire_worker(&workers, &None, None).unwrap();
        assert_eq!(second.worker_id, "b");
    }

    #[test]
    fn round_robin_cycles_workers() {
        reset_pool();
        let mut workers = sample_workers();
        workers.scheduling = WorkerScheduling::RoundRobin;

        let first = acquire_worker(&workers, &None, None).unwrap();
        let second = acquire_worker(&workers, &None, None).unwrap();

        assert_ne!(first.worker_id, second.worker_id);
    }

    #[test]
    fn sticky_session_reuses_worker() {
        reset_pool();
        let workers = sample_workers();
        assign_session_worker("session-1", "b");
        let resumed = acquire_worker(&workers, &None, Some("session-1")).unwrap();
        assert_eq!(resumed.worker_id, "b");
    }
}
