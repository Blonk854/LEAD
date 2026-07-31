mod cloud;
mod local;
mod local_html;

pub use local::{LOCAL_WEB_SEARCH_PROVIDER_ID, LocalHtmlWebSearchProvider};

use client::{Client, UserStore};
use gpui::{App, Context, Entity};
use language_model::LanguageModelRegistry;
use std::sync::Arc;
use web_search::{WebSearchProviderId, WebSearchRegistry};

pub fn init(client: Arc<Client>, user_store: Entity<UserStore>, cx: &mut App) {
    let registry = WebSearchRegistry::global(cx);
    registry.update(cx, |registry, cx| {
        // Local HTML discovery is always available for LEAD (no search API).
        registry.register_provider(
            LocalHtmlWebSearchProvider::new(client.http_client()),
            cx,
        );
        // Prefer local by default for LEAD; cloud may override when Zed models are active.
        register_web_search_providers(registry, client, user_store, cx);
    });
}

fn register_web_search_providers(
    registry: &mut WebSearchRegistry,
    client: Arc<Client>,
    user_store: Entity<UserStore>,
    cx: &mut Context<WebSearchRegistry>,
) {
    register_zed_web_search_provider(
        registry,
        client.clone(),
        user_store.clone(),
        &LanguageModelRegistry::global(cx),
        cx,
    );

    cx.subscribe(
        &LanguageModelRegistry::global(cx),
        move |this, registry, event, cx| {
            if let language_model::Event::DefaultModelChanged = event {
                register_zed_web_search_provider(
                    this,
                    client.clone(),
                    user_store.clone(),
                    &registry,
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
    cx: &mut Context<WebSearchRegistry>,
) {
    let using_zed_provider = language_model_registry
        .read(cx)
        .default_model()
        .is_some_and(|default| default.is_provided_by_zed());
    if using_zed_provider {
        // Cloud can be registered alongside local; keep local as active unless
        // we explicitly prefer cloud. For LEAD local-first, do not steal active
        // away from local_html when it is already set.
        registry.register_provider(
            cloud::CloudWebSearchProvider::new(client, user_store, cx),
            cx,
        );
        if registry.active_provider().is_none() {
            // register_provider already sets active if none; nothing else needed.
        }
    } else {
        registry.unregister_provider(WebSearchProviderId(
            cloud::ZED_WEB_SEARCH_PROVIDER_ID.into(),
        ));
        // Ensure local remains active after cloud removal.
        if registry.active_provider().is_none() {
            // Local should still be registered from init; if missing, leave none.
        }
    }

    // Force local as active when present — LEAD product default.
    let local = registry
        .providers()
        .find(|provider| provider.id().0.as_ref() == LOCAL_WEB_SEARCH_PROVIDER_ID)
        .cloned();
    if let Some(local) = local {
        registry.set_active_provider(local);
    }
}
