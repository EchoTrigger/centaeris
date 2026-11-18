use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stage {
    Models,
    ModelEffort(usize),
    CurrentEffort(usize),
}

pub(super) struct Row {
    pub index: usize,
    pub label: String,
    pub description: String,
}

fn description(mode: &str) -> &'static str {
    match mode {
        "none" => "Reasoning disabled",
        "low" => "Faster responses with lighter reasoning",
        "medium" => "Balanced reasoning and response time",
        "high" => "More reasoning for complex tasks",
        "xhigh" => "Extended reasoning for difficult tasks",
        "max" => "Maximum reasoning effort",
        _ => "Model-provided reasoning level",
    }
}

pub(super) fn selected_model(app: &App) -> Option<&TuiModelChoice> {
    let panel = app.model_panel.as_ref()?;
    panel
        .providers
        .get(panel.selected_provider)?
        .models
        .get(panel.selected_model)
}

fn initial_effort(app: &App, provider: &str, model: &TuiModelChoice) -> usize {
    let current = (app.model_provider_id.as_deref() == Some(provider)
        && app.model_display.as_deref() == Some(model.model.as_str()))
    .then_some(app.model_effort.as_deref())
    .flatten();
    [
        current,
        model.preferred_thinking_mode.as_deref(),
        model.default_thinking_mode.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find_map(|mode| {
        model
            .thinking_modes
            .iter()
            .position(|candidate| candidate == mode)
    })
    .unwrap_or(0)
}

fn effort_rows(app: &App, provider: &str, model: &TuiModelChoice) -> Vec<Row> {
    let current_model = app.model_provider_id.as_deref() == Some(provider)
        && app.model_display.as_deref() == Some(model.model.as_str());
    model
        .thinking_modes
        .iter()
        .enumerate()
        .map(|(index, mode)| {
            let current = current_model && app.model_effort.as_ref() == Some(mode);
            let default = model.default_thinking_mode.as_ref() == Some(mode);
            Row {
                index,
                label: format!(
                    "{mode}{}{}",
                    if current { " (current)" } else { "" },
                    if default { " (default)" } else { "" }
                ),
                description: description(mode).into(),
            }
        })
        .collect()
}

pub(super) fn rows(app: &App) -> Vec<Row> {
    let Some(panel) = app.model_panel.as_ref() else {
        return Vec::new();
    };
    let Some(provider) = panel.providers.get(panel.selected_provider) else {
        return Vec::new();
    };
    let rows = match panel.stage {
        Stage::Models if !provider.configured => vec![Row {
            index: 0,
            label: format!("Set up {}", provider.name),
            description: "Enter API key to load this provider's models".into(),
        }],
        Stage::Models => provider
            .models
            .iter()
            .enumerate()
            .map(|(index, model)| {
                let current = panel.active_provider_id.as_deref()
                    == Some(provider.provider_id.as_str())
                    && panel.active_model.as_deref() == Some(model.model.as_str());
                Row {
                    index,
                    label: if current {
                        format!("{} (current)", model.display_name)
                    } else {
                        model.display_name.clone()
                    },
                    description: model.model.clone(),
                }
            })
            .collect(),
        Stage::ModelEffort(_) | Stage::CurrentEffort(_) => selected_model(app)
            .map_or_else(Vec::new, |model| {
                effort_rows(app, &provider.provider_id, model)
            }),
    };
    let query = panel.query.to_lowercase();
    rows.into_iter()
        .filter(|row| {
            row.label.to_lowercase().contains(&query)
                || row.description.to_lowercase().contains(&query)
        })
        .collect()
}

pub(super) fn selection(app: &App) -> usize {
    app.model_panel
        .as_ref()
        .map_or(0, |panel| match panel.stage {
            Stage::Models => panel.selected_model,
            Stage::ModelEffort(index) | Stage::CurrentEffort(index) => index,
        })
}

pub(super) fn select(app: &mut App, index: usize) {
    if app.pending_model_request.is_some() {
        return;
    }
    if let Some(panel) = app.model_panel.as_mut() {
        match &mut panel.stage {
            Stage::Models => panel.selected_model = index,
            Stage::ModelEffort(selected) | Stage::CurrentEffort(selected) => *selected = index,
        }
    }
}

pub(super) fn move_selection(app: &mut App, delta: isize) {
    let items = rows(app);
    if items.is_empty() {
        return;
    }
    let current = items
        .iter()
        .position(|row| row.index == selection(app))
        .unwrap_or(0);
    let next = (current as isize + delta).rem_euclid(items.len() as isize) as usize;
    select(app, items[next].index);
    app.message = None;
}

pub(super) fn filter(app: &mut App, ch: Option<char>) {
    if let Some(panel) = app.model_panel.as_mut() {
        if let Some(ch) = ch {
            panel.query.push(ch);
        } else {
            panel
                .query
                .truncate(editing::previous(&panel.query, panel.query.len()));
        }
    }
    if let Some(row) = rows(app).first() {
        select(app, row.index);
    }
    app.message = None;
}

pub(super) fn activate(app: &mut App) -> Result<(), String> {
    if app.pending_model_request.is_some() {
        return Err("Model configuration request is already in progress".into());
    }
    let panel = app.model_panel.as_ref().ok_or("Model picker is closed")?;
    if !rows(app).iter().any(|row| row.index == selection(app)) {
        return Ok(());
    }
    let provider = selected_model_provider(app)
        .cloned()
        .ok_or("No model providers are available")?;
    if !provider.configured {
        begin_model_credential(app, provider.provider_id, provider.name);
        return Ok(());
    }
    let model = selected_model(app).cloned().ok_or("No selectable model")?;
    match panel.stage {
        Stage::Models if !model.thinking_modes.is_empty() => {
            let index = initial_effort(app, &provider.provider_id, &model);
            let panel = app.model_panel.as_mut().expect("open picker");
            panel.stage = Stage::ModelEffort(index);
            panel.query.clear();
            app.message = None;
            Ok(())
        }
        Stage::Models => request_model_selection(app, provider.provider_id, model.model, None),
        Stage::ModelEffort(index) | Stage::CurrentEffort(index) => {
            let mode = model
                .thinking_modes
                .get(index)
                .cloned()
                .ok_or("No selectable reasoning level")?;
            request_model_selection(app, provider.provider_id, model.model, Some(mode))
        }
    }
}

pub(super) fn back(app: &mut App) {
    if app.pending_model_request.is_some() {
        return;
    }
    if let Some(panel) = app.model_panel.as_mut() {
        if matches!(panel.stage, Stage::ModelEffort(_)) {
            panel.stage = Stage::Models;
            panel.query.clear();
            app.message = None;
            return;
        }
    }
    close_model_panel(app);
}

pub(super) fn open_effort(app: &mut App, config: &Value) -> Result<(), String> {
    open_model_panel(app, config)?;
    let provider = selected_model_provider(app).ok_or("No active model")?;
    let model = selected_model(app).ok_or("No active model")?;
    if app.model_provider_id.as_deref() != Some(&provider.provider_id)
        || app.model_display.as_deref() != Some(&model.model)
    {
        close_model_panel(app);
        return Err("No active model".into());
    }
    if model.thinking_modes.is_empty() {
        let mode = app
            .model_effort
            .as_deref()
            .unwrap_or("provider default")
            .to_owned();
        close_model_panel(app);
        show_message(
            app,
            format!("Reasoning: {mode}; this model has no selectable reasoning levels"),
        );
        return Ok(());
    }
    let index = initial_effort(app, &provider.provider_id, model);
    app.model_panel.as_mut().expect("open picker").stage = Stage::CurrentEffort(index);
    Ok(())
}

fn available_models(app: &App) -> impl Iterator<Item = (&TuiModelProvider, &TuiModelChoice)> {
    app.model_catalog
        .iter()
        .filter(|provider| provider.configured)
        .flat_map(|provider| provider.models.iter().map(move |model| (provider, model)))
}

pub(super) fn resolve_choice(
    app: &App,
    query: &str,
) -> Result<(String, String, Option<String>), String> {
    let exact = |name: &str| -> Result<_, String> {
        let matches = available_models(app)
            .filter(|(provider, model)| {
                name == model.model
                    || name == model.display_name
                    || name == format!("{}/{}", provider.provider_id, model.model)
            })
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [choice] => Ok(*choice),
            [] => Err(format!("Unknown model: {name}")),
            _ => Err(format!(
                "Model query is ambiguous: {name}; use provider/model"
            )),
        }
    };
    if let Ok((provider, model)) = exact(query) {
        return Ok((provider.provider_id.clone(), model.model.clone(), None));
    }
    let (name, mode) = query
        .rsplit_once(char::is_whitespace)
        .ok_or_else(|| format!("Unknown model: {query}"))?;
    let (provider, model) = exact(name.trim_end())?;
    if !model
        .thinking_modes
        .iter()
        .any(|candidate| candidate == mode)
    {
        return Err(format!(
            "Unsupported reasoning level {mode}; available: {}",
            model.thinking_modes.join(", ")
        ));
    }
    Ok((
        provider.provider_id.clone(),
        model.model.clone(),
        Some(mode.to_owned()),
    ))
}
