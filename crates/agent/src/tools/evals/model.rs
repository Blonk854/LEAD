use anyhow::{Context as _, Result};
use client::{Client, RefreshLlmTokenListener, UserStore};
use gpui::{AppContext as _, AsyncApp, TestAppContext};
use language_model::{LanguageModel, LanguageModelRegistry, SelectedModel};
use reqwest_client::ReqwestClient;
use settings::SettingsStore;
use std::{str::FromStr, sync::Arc};

/// Load the model named by `ZED_AGENT_MODEL` (`provider/model`).
pub async fn load_eval_model(cx: &mut TestAppContext) -> Arc<dyn LanguageModel> {
    init_eval_app(cx);

    let selected = SelectedModel::from_str(
        &std::env::var("ZED_AGENT_MODEL").unwrap_or_else(|_| "lmstudio/local-model".into()),
    )
    .expect("ZED_AGENT_MODEL must be provider/model");

    let authenticate_tasks = cx.update(|cx| {
        LanguageModelRegistry::global(cx).update(cx, |registry, cx| {
            registry
                .providers()
                .iter()
                .map(|provider| provider.authenticate(cx))
                .collect::<Vec<_>>()
        })
    });

    cx.update(|cx| {
        cx.spawn(async move |cx| {
            futures::future::join_all(authenticate_tasks).await;
            load_model(&selected, cx).await
        })
    })
    .await
    .expect("failed to load eval model")
}

pub fn init_eval_app(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        let settings_store = SettingsStore::test(cx);
        cx.set_global(settings_store);

        gpui_tokio::init(cx);
        let http_client = Arc::new(ReqwestClient::user_agent("agent tool evals").unwrap());
        cx.set_http_client(http_client);
        let client = Client::production(cx);
        let user_store = cx.new(|cx| UserStore::new(client.clone(), cx));
        language_model::init(cx);
        RefreshLlmTokenListener::register(client.clone(), user_store.clone(), cx);
        language_models::init(user_store, client, cx);
    });
}

async fn load_model(
    selected_model: &SelectedModel,
    cx: &mut AsyncApp,
) -> Result<Arc<dyn LanguageModel>> {
    let provider_id = selected_model.provider.clone();
    let auth_task = cx.update(|cx| {
        LanguageModelRegistry::read_global(cx)
            .provider(&provider_id)
            .map(|provider| provider.authenticate(cx))
    });
    let Some(auth_task) = auth_task else {
        anyhow::bail!("provider {:?} not found", provider_id);
    };
    auth_task.await?;

    Ok(cx.update(|cx| {
        let models: Vec<_> = LanguageModelRegistry::read_global(cx)
            .available_models(cx)
            .collect();
        models
            .iter()
            .find(|model| {
                model.provider_id() == selected_model.provider && model.id() == selected_model.model
            })
            .or_else(|| {
                models.iter().find(|model| {
                    model.provider_id() == selected_model.provider
                        && model.id().0.eq_ignore_ascii_case(&selected_model.model.0)
                })
            })
            .cloned()
            .with_context(|| format!("model {} not found", selected_model.model.0))
    })?)
}
