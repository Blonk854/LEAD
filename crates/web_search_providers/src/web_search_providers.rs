mod cloud;
mod local;
mod local_html;

pub use local::{LOCAL_WEB_SEARCH_PROVIDER_ID, LocalHtmlWebSearchProvider};

use client::{Client, UserStore};
use gpui::{App, Context, Entity};
use language_model::LanguageModelRegistry;
use std::sync::Arc;
use std::time::Duration;
use web_search::{WebSearchProviderId, WebSearchRegistry};

pub fn init(client: Arc<Client>, user_store: Entity<UserStore>, cx: &mut App) {
    let registry = WebSearchRegistry::global(cx);
    registry.update(cx, |registry, cx| {
        let (max_results, max_snippet, max_redirects, max_bytes, timeout_ms, delay_ms, preferred) =
            read_web_research_prefs(cx);

        registry.register_provider(
            LocalHtmlWebSearchProvider::new(client.http_client())
                .with_limits(max_results, max_snippet)
                .with_http_policy(
                    max_redirects,
                    max_bytes,
                    Duration::from_millis(timeout_ms),
                    Duration::from_millis(delay_ms),
                ),
            cx,
        );
        register_web_search_providers(registry, client, user_store, preferred, cx);
    });
}

fn read_web_research_prefs(cx: &App) -> (usize, usize, u32, u64, u64, u64, String) {
    use settings::SettingsStore;
    let defaults = (
        5usize,
        300usize,
        5u32,
        1024u64 * 1024,
        15_000u64,
        1_000u64,
        "local".to_string(),
    );
    let Some(store) = cx.try_global::<SettingsStore>() else {
        return defaults;
    };
    let Some(agent) = store.merged_settings().agent.as_ref() else {
        return defaults;
    };
    let Some(research) = agent.web_research.as_ref() else {
        return defaults;
    };
    (
        research
            .max_search_results
            .filter(|value| (1..=20).contains(value))
            .unwrap_or(defaults.0),
        research
            .max_snippet_chars
            .filter(|value| (80..=2_000).contains(value))
            .unwrap_or(defaults.1),
        research
            .max_redirects
            .filter(|value| (0..=20).contains(value))
            .unwrap_or(defaults.2),
        research
            .max_response_bytes
            .filter(|value| (16_384..=8 * 1024 * 1024).contains(value))
            .unwrap_or(defaults.3),
        research
            .request_timeout_ms
            .filter(|value| (1_000..=120_000).contains(value))
            .unwrap_or(defaults.4),
        research
            .per_host_delay_ms
            .filter(|value| (0..=30_000).contains(value))
            .unwrap_or(defaults.5),
        research
            .preferred_provider
            .as_ref()
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "cloud" => "cloud".to_string(),
                "auto" => "auto".to_string(),
                _ => "local".to_string(),
            })
            .unwrap_or(defaults.6),
    )
}

fn register_web_search_providers(
    registry: &mut WebSearchRegistry,
    client: Arc<Client>,
    user_store: Entity<UserStore>,
    preferred_provider: String,
    cx: &mut Context<WebSearchRegistry>,
) {
    register_zed_web_search_provider(
        registry,
        client.clone(),
        user_store.clone(),
        &LanguageModelRegistry::global(cx),
        preferred_provider.clone(),
        cx,
    );

    cx.subscribe(
        &LanguageModelRegistry::global(cx),
        move |this, registry, event, cx| {
            if let language_model::Event::DefaultModelChanged = event {
                let preferred = read_web_research_prefs(cx).6;
                register_zed_web_search_provider(
                    this,
                    client.clone(),
                    user_store.clone(),
                    &registry,
                    preferred,
                    cx,
                )
            }
        },
    )
    .detach();
}

fn register_zed_web_search_provider(
    registry: &mut WebSearchRegistry,
    client: Arc<Client>,
    user_store: Entity<UserStore>,
    language_model_registry: &Entity<LanguageModelRegistry>,
    preferred_provider: String,
    cx: &mut Context<WebSearchRegistry>,
) {
    let using_zed_provider = language_model_registry
        .read(cx)
        .default_model()
        .is_some_and(|default| default.is_provided_by_zed());
    if using_zed_provider {
        registry.register_provider(
            cloud::CloudWebSearchProvider::new(client, user_store, cx),
            cx,
        );
    } else {
        registry.unregister_provider(WebSearchProviderId(
            cloud::ZED_WEB_SEARCH_PROVIDER_ID.into(),
        ));
    }

    apply_preferred_provider(registry, &preferred_provider);
}

fn apply_preferred_provider(registry: &mut WebSearchRegistry, preferred: &str) {
    let preferred = preferred.trim().to_ascii_lowercase();
    let local = registry
        .providers()
        .find(|provider| provider.id().0.as_ref() == LOCAL_WEB_SEARCH_PROVIDER_ID)
        .cloned();
    let cloud = registry
        .providers()
        .find(|provider| provider.id().0.as_ref() == cloud::ZED_WEB_SEARCH_PROVIDER_ID)
        .cloned();

    match preferred.as_str() {
        "cloud" => {
            if let Some(cloud) = cloud {
                registry.set_active_provider(cloud);
            } else if let Some(local) = local {
                // Preferred cloud unavailable — fall back to local with active set.
                registry.set_active_provider(local);
            }
        }
        "auto" => {
            // Prefer cloud when registered (Zed default model), else local.
            if let Some(cloud) = cloud {
                registry.set_active_provider(cloud);
            } else if let Some(local) = local {
                registry.set_active_provider(local);
            }
        }
        _ => {
            // local (default)
            if let Some(local) = local {
                registry.set_active_provider(local);
            }
        }
    }
}
