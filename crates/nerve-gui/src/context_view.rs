//! The Context view: assemble a token-budgeted context from the workspace
//! selection using a named recipe, then copy/export it — the RepoPrompt-style
//! "build context for an LLM" surface. All deterministic work lives in the
//! engine (`workspace_context` + recipes); this is a thin client over
//! `tool.call` (manage_selection / workspace_context / git).

use crate::data::{fetch_context, fetch_diff, selection_op};
use leptos::prelude::*;

const RECIPES: &[(&str, &str)] = &[
    ("standard", "Standard"),
    ("plan", "Plan"),
    ("review", "Review"),
    ("diff", "Diff Follow-Up"),
    ("manual", "Manual"),
];

/// The token breakdown returned under `workspace_context` `structuredContent.tokens`.
#[derive(Clone, Default, serde::Deserialize)]
struct Budget {
    total_tokens: usize,
    file_map_tokens: usize,
    contents_tokens: usize,
    git_diff_tokens: usize,
    meta_prompts_tokens: usize,
    instructions_tokens: usize,
    #[serde(default)]
    files: Vec<BudgetFile>,
}

#[derive(Clone, serde::Deserialize)]
struct BudgetFile {
    display_path: String,
    token_count: usize,
    mode: String,
}

#[component]
pub fn ContextView(token: StoredValue<Option<String>>) -> impl IntoView {
    let recipe = RwSignal::new("standard".to_string());
    let add_path = RwSignal::new(String::new());
    let context_text = RwSignal::new(String::new());
    let budget = RwSignal::new(Budget::default());

    // Re-assemble: fetch the diff for recipes that need it, then the context.
    let refresh = move || {
        let Some(tok) = token.get_value() else { return };
        let rec = recipe.get_untracked();
        leptos::task::spawn_local(async move {
            let git_diff = if rec == "review" || rec == "diff" {
                fetch_diff(&tok).await
            } else {
                None
            };
            if let Some((text, structured)) = fetch_context(&tok, &rec, git_diff).await {
                context_text.set(text);
                let parsed = structured
                    .get("tokens")
                    .cloned()
                    .and_then(|t| serde_json::from_value::<Budget>(t).ok())
                    .unwrap_or_default();
                budget.set(parsed);
            }
        });
    };

    // Re-assemble whenever the recipe changes (and once on mount).
    Effect::new(move |_| {
        let _ = recipe.get();
        refresh();
    });

    let add = move || {
        let Some(tok) = token.get_value() else { return };
        let path = add_path.get_untracked().trim().to_string();
        if path.is_empty() {
            return;
        }
        add_path.set(String::new());
        leptos::task::spawn_local(async move {
            let _ = selection_op(&tok, "add", vec![path]).await;
            refresh();
        });
    };

    let remove = move |path: String| {
        let Some(tok) = token.get_value() else { return };
        leptos::task::spawn_local(async move {
            let _ = selection_op(&tok, "remove", vec![path]).await;
            refresh();
        });
    };

    let copy = move |_| {
        let text = context_text.get_untracked();
        if let Some(clip) = web_sys::window().map(|w| w.navigator().clipboard()) {
            let _ = clip.write_text(&text);
        }
    };

    view! {
        <aside class="context-view">
            <div class="ctx-head">
                <span class="ctx-title">"Context"</span>
                <select class="ctx-recipe" prop:value=move || recipe.get()
                    on:change=move |ev| recipe.set(event_target_value(&ev))>
                    {RECIPES.iter().map(|(id, label)| view! { <option value=*id>{*label}</option> }).collect_view()}
                </select>
            </div>

            <div class="ctx-add">
                <input class="ctx-add-in" placeholder="add a file path…"
                    prop:value=move || add_path.get()
                    on:input=move |ev| add_path.set(event_target_value(&ev))
                    on:keydown=move |ev| if ev.key() == "Enter" { ev.prevent_default(); add(); } />
                <button class="ctx-add-btn" on:click=move |_| add()>"Add"</button>
            </div>

            <div class="ctx-budget">
                {move || {
                    let b = budget.get();
                    view! {
                        <div class="ctx-total">{move || budget.get().total_tokens}" tokens"</div>
                        <div class="ctx-bars">
                            <Bar label="files" n=b.contents_tokens total=b.total_tokens/>
                            <Bar label="map" n=b.file_map_tokens total=b.total_tokens/>
                            <Bar label="diff" n=b.git_diff_tokens total=b.total_tokens/>
                            <Bar label="meta" n=b.meta_prompts_tokens total=b.total_tokens/>
                            <Bar label="instr" n=b.instructions_tokens total=b.total_tokens/>
                        </div>
                    }
                }}
            </div>

            <div class="ctx-files">
                {move || {
                    let files = budget.get().files;
                    if files.is_empty() {
                        view! { <div class="ctx-empty">"No files selected — add paths above."</div> }.into_any()
                    } else {
                        files.into_iter().map(|f| {
                            let path = f.display_path.clone();
                            view! {
                                <div class="ctx-file">
                                    <span class="ctx-file-path">{f.display_path}</span>
                                    <span class="ctx-file-mode">{f.mode}</span>
                                    <span class="ctx-file-tok">{f.token_count}</span>
                                    <button class="ctx-file-rm" title="Remove" on:click=move |_| remove(path.clone())>"×"</button>
                                </div>
                            }
                        }).collect_view().into_any()
                    }
                }}
            </div>

            <div class="ctx-preview-head">
                <span>"Assembled context"</span>
                <button class="ctx-copy" on:click=copy>"Copy"</button>
            </div>
            <pre class="ctx-preview">{move || context_text.get()}</pre>
        </aside>
    }
}

#[component]
fn Bar(label: &'static str, n: usize, total: usize) -> impl IntoView {
    let pct = if total > 0 { (n * 100) / total } else { 0 };
    view! {
        <div class="ctx-bar-row">
            <span class="ctx-bar-label">{label}</span>
            <span class="ctx-bar-track"><span class="ctx-bar-fill" style=format!("width:{pct}%")></span></span>
            <span class="ctx-bar-n">{n}</span>
        </div>
    }
}
