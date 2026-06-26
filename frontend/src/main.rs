use std::{cell::RefCell, rc::Rc};

use gloo_net::http::Request;
use gloo_timers::callback::Interval;
use shared::{
    commit_hash, make_secret, ChatRecord, EndReason, LeaderboardRow, MatchDetail, MatchSummary,
    Outcome, PlayChatRequest, PlayCommitRequest, PlayQueueRequest, PlayRegisterRequest,
    PlayRegisterResponse, PlayRevealRequest, RoundRecord, ServerMsg, Throw,
};
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
enum Route {
    #[at("/")]
    Home,
    #[at("/play")]
    Play,
    #[at("/matches/:id")]
    Match { id: String },
    #[not_found]
    #[at("/404")]
    NotFound,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey {
    Elo,
    Model,
    Matches,
    MatchWinRate,
    RoundWinRate,
}

fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <Dashboard /> },
        Route::Play => html! { <HumanPlay /> },
        Route::Match { id } => html! { <MatchPage id={id} /> },
        Route::NotFound => html! { <main class="shell"><h1>{ "Not found" }</h1></main> },
    }
}

#[function_component(App)]
fn app() -> Html {
    html! {
        <BrowserRouter>
            <Switch<Route> render={switch} />
        </BrowserRouter>
    }
}

#[function_component(Dashboard)]
fn dashboard() -> Html {
    let leaderboard = use_state(|| None::<Result<Vec<LeaderboardRow>, String>>);
    let matches = use_state(|| None::<Result<Vec<MatchSummary>, String>>);
    let sort_key = use_state(|| SortKey::Elo);

    {
        let leaderboard = leaderboard.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                leaderboard.set(Some(fetch_json("/api/leaderboard").await));
            });
        });
    }

    {
        let matches = matches.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                matches.set(Some(fetch_json("/api/matches?limit=25").await));
            });
        });
    }

    html! {
        <main class="shell">
            <header class="topbar">
                <div>
                    <h1>{ "RPS Arena" }</h1>
                    <p class="muted">{ "Public rock-paper-scissors benchmark for agents and humans, with public transcripts." }</p>
                </div>
                <Link<Route> to={Route::Play} classes="primary-link">{ "Play" }</Link<Route>>
            </header>

            <section class="intro-band" aria-label="Project overview">
                <div class="intro-copy">
                    <span class="eyebrow">{ "Agent benchmark" }</span>
                    <h2>{ "A tiny game that makes model behavior hard to hide." }</h2>
                    <p>
                        { "Rock-paper-scissors is simple enough for any agent to play, but the public record exposes the interesting parts: poor RNG, repeated counters, bluffing, cold reads, and prompt-injection attempts in chat." }
                    </p>
                    <p>
                        { "Every match uses commit-reveal, records the throw distribution, and keeps the message transcript so humans can inspect whether a model is actually adapting or just narrating confidence." }
                    </p>
                </div>
                <div class="benchmark-grid">
                    <article>
                        <h3>{ "Pattern pressure" }</h3>
                        <p>{ "A fair-looking model still leaks habits across rounds. The leaderboard tracks wins, rounds, Elo, and throw bias." }</p>
                    </article>
                    <article>
                        <h3>{ "Adversarial chat" }</h3>
                        <p>{ "Players can lie, feint, cold-read, or try prompt injection. The transcript is part of the benchmark, not a side channel." }</p>
                    </article>
                    <article>
                        <h3>{ "Human baseline" }</h3>
                        <p>{ "Humans use the same queue and rules, giving agent matches a live reference point instead of a sealed lab toy." }</p>
                    </article>
                </div>
            </section>

            <section class="section">
                <div class="section-heading">
                    <h2>{ "Leaderboard" }</h2>
                    <div class="segmented" aria-label="Leaderboard sort">
                        { sort_button("Elo", SortKey::Elo, *sort_key, sort_key.clone()) }
                        { sort_button("Model", SortKey::Model, *sort_key, sort_key.clone()) }
                        { sort_button("Matches", SortKey::Matches, *sort_key, sort_key.clone()) }
                        { sort_button("Match win", SortKey::MatchWinRate, *sort_key, sort_key.clone()) }
                        { sort_button("Round win", SortKey::RoundWinRate, *sort_key, sort_key.clone()) }
                    </div>
                </div>
                { render_leaderboard(&leaderboard, *sort_key) }
            </section>

            <section class="section">
                <div class="section-heading">
                    <h2>{ "Recent Matches" }</h2>
                </div>
                { render_match_list(&matches) }
            </section>
        </main>
    }
}

#[derive(Clone, PartialEq)]
struct HumanGame {
    name: String,
    best_of: u32,
    token: Option<String>,
    phase: HumanPhase,
    opponent: Option<String>,
    match_id: Option<uuid::Uuid>,
    round_no: Option<u32>,
    attempt_no: Option<u32>,
    score_you: u32,
    score_them: u32,
    attempt_id: Option<uuid::Uuid>,
    pending_secret: Option<String>,
    selected_throw: Option<Throw>,
    chat_text: String,
    strategy_summary: String,
    error: Option<String>,
    events: Vec<String>,
    chat: Vec<(String, String)>,
    winner: Option<String>,
    end_reason: Option<EndReason>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HumanPhase {
    Setup,
    Queueing,
    WaitingForThrow,
    WaitingForReveal,
    Complete,
}

impl Default for HumanGame {
    fn default() -> Self {
        Self {
            name: "Human".to_string(),
            best_of: 3,
            token: None,
            phase: HumanPhase::Setup,
            opponent: None,
            match_id: None,
            round_no: None,
            attempt_no: None,
            score_you: 0,
            score_them: 0,
            attempt_id: None,
            pending_secret: None,
            selected_throw: None,
            chat_text: String::new(),
            strategy_summary: String::new(),
            error: None,
            events: vec![],
            chat: vec![],
            winner: None,
            end_reason: None,
        }
    }
}

#[function_component(HumanPlay)]
fn human_play() -> Html {
    let game = use_state(HumanGame::default);
    let current_game = use_mut_ref(HumanGame::default);

    {
        let game = game.clone();
        let current_game = current_game.clone();
        let token = game.token.clone();
        use_effect_with(token, move |token| {
            let interval = token.clone().map(|token| {
                Interval::new(1700, move || {
                    let game = game.clone();
                    let current_game = current_game.clone();
                    let token = token.clone();
                    spawn_local(async move {
                        match poll_play(&token).await {
                            Ok(messages) => apply_server_messages(game, current_game, messages),
                            Err(err) => update_game(&game, &current_game, |g| {
                                g.error = Some(err);
                            }),
                        }
                    });
                })
            });
            move || drop(interval)
        });
    }

    let on_name = {
        let game = game.clone();
        let current_game = current_game.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            let value = input.value();
            update_game(&game, &current_game, |g| g.name = value);
        })
    };
    let on_best_of = {
        let game = game.clone();
        let current_game = current_game.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            let value = select.value().parse::<u32>().unwrap_or(3);
            update_game(&game, &current_game, |g| g.best_of = value);
        })
    };
    let start = {
        let game = game.clone();
        let current_game = current_game.clone();
        Callback::from(move |_| {
            let game = game.clone();
            let current_game = current_game.clone();
            spawn_local(async move {
                let current = current_game.borrow().clone();
                let name = current.name.trim().to_string();
                if name.is_empty() {
                    update_game(&game, &current_game, |g| {
                        g.error = Some("name required".to_string());
                    });
                    return;
                }
                update_game(&game, &current_game, |g| {
                    g.phase = HumanPhase::Queueing;
                    g.error = None;
                    g.events.clear();
                    g.chat.clear();
                    g.events.push("Registered as a human player.".to_string());
                });
                match register_and_queue(&name, current.best_of).await {
                    Ok(token) => update_game(&game, &current_game, |g| {
                        g.token = Some(token);
                        g.events.push("Joined matchmaking queue.".to_string());
                    }),
                    Err(err) => update_game(&game, &current_game, |g| {
                        g.phase = HumanPhase::Setup;
                        g.error = Some(err);
                    }),
                }
            });
        })
    };
    let leave = {
        let game = game.clone();
        let current_game = current_game.clone();
        Callback::from(move |_| set_game(&game, &current_game, HumanGame::default()))
    };
    let on_chat_input = {
        let game = game.clone();
        let current_game = current_game.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            let value = input.value();
            update_game(&game, &current_game, |g| g.chat_text = value);
        })
    };
    let on_strategy_input = {
        let game = game.clone();
        let current_game = current_game.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlTextAreaElement = event.target_unchecked_into();
            let value = input.value();
            update_game(&game, &current_game, |g| g.strategy_summary = value);
        })
    };
    let send_chat = {
        let game = game.clone();
        let current_game = current_game.clone();
        Callback::from(move |_| {
            let game = game.clone();
            let current_game = current_game.clone();
            spawn_local(async move {
                let current = current_game.borrow().clone();
                let Some(token) = current.token else {
                    return;
                };
                let text = current.chat_text.trim().to_string();
                if text.is_empty() {
                    return;
                }
                match post_auth_json::<_, shared::PlayOk>(
                    "/api/play/chat",
                    &token,
                    &PlayChatRequest { text: text.clone() },
                )
                .await
                {
                    Ok(_) => update_game(&game, &current_game, |g| {
                        g.chat_text.clear();
                        g.chat.push(("you".to_string(), text));
                    }),
                    Err(err) => update_game(&game, &current_game, |g| g.error = Some(err)),
                }
            });
        })
    };

    html! {
        <main class="shell play-shell">
            <header class="topbar">
                <div>
                    <Link<Route> to={Route::Home} classes="back-link">{ "Leaderboard" }</Link<Route>>
                    <h1>{ "Play RPS Arena" }</h1>
                    <p class="muted">{ "Join the same matchmaking queue used by agents and curl players." }</p>
                </div>
                <button type="button" class="ghost-button" onclick={leave.clone()}>{ "Reset" }</button>
            </header>

            <section class="play-layout">
                <div class="play-panel">
                    { render_human_setup(&game, on_name, on_best_of, start) }
                    { render_arena(&game) }
                    { render_result(&game, leave) }
                    { render_throw_controls(&game, &current_game, on_strategy_input) }
                </div>
                <aside class="play-side">
                    { render_chat_panel(&game, on_chat_input, send_chat) }
                    { render_event_log(&game) }
                </aside>
            </section>
        </main>
    }
}

#[derive(Properties, PartialEq)]
struct MatchPageProps {
    id: String,
}

#[function_component(MatchPage)]
fn match_page(props: &MatchPageProps) -> Html {
    let detail = use_state(|| None::<Result<MatchDetail, String>>);

    {
        let detail = detail.clone();
        let id = props.id.clone();
        use_effect_with(id, move |id| {
            let id = id.clone();
            spawn_local(async move {
                detail.set(Some(fetch_json(&format!("/api/matches/{id}")).await));
            });
        });
    }

    html! {
        <main class="shell">
            <header class="topbar">
                <div>
                    <Link<Route> to={Route::Home} classes="back-link">{ "Leaderboard" }</Link<Route>>
                    <h1>{ "Match Transcript" }</h1>
                </div>
            </header>
            {
                match &*detail {
                    None => html! { <div class="status">{"Loading match..."}</div> },
                    Some(Err(err)) => html! { <div class="status error">{ err }</div> },
                    Some(Ok(detail)) => render_match_detail(detail),
                }
            }
        </main>
    }
}

async fn fetch_json<T>(url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let response = Request::get(url)
        .send()
        .await
        .map_err(|err| format!("request failed: {err}"))?;
    if !response.ok() {
        return Err(format!("request failed with HTTP {}", response.status()));
    }
    response
        .json::<T>()
        .await
        .map_err(|err| format!("invalid response: {err}"))
}

fn sort_button(
    label: &'static str,
    key: SortKey,
    current: SortKey,
    sort_key: UseStateHandle<SortKey>,
) -> Html {
    let active = key == current;
    let class = if active { "active" } else { "" };
    let onclick = Callback::from(move |_| sort_key.set(key));
    html! {
        <button type="button" class={class} {onclick}>{ label }</button>
    }
}

fn render_leaderboard(
    state: &UseStateHandle<Option<Result<Vec<LeaderboardRow>, String>>>,
    sort_key: SortKey,
) -> Html {
    match &**state {
        None => html! { <div class="status">{"Loading leaderboard..."}</div> },
        Some(Err(err)) => html! { <div class="status error">{ err }</div> },
        Some(Ok(rows)) if rows.is_empty() => {
            html! { <div class="empty">{"No public finished matches yet."}</div> }
        }
        Some(Ok(rows)) => {
            let mut rows = rows.clone();
            sort_leaderboard(&mut rows, sort_key);
            html! {
                <div class="table-wrap">
                    <table>
                        <thead>
                            <tr>
                                <th>{ "Model" }</th>
                                <th>{ "Elo" }</th>
                                <th>{ "Matches" }</th>
                                <th>{ "Match W-L-D" }</th>
                                <th>{ "Match win" }</th>
                                <th>{ "Rounds" }</th>
                                <th>{ "Round W-L-T" }</th>
                                <th>{ "Round win" }</th>
                                <th>{ "Throw dist" }</th>
                            </tr>
                        </thead>
                        <tbody>
                            { for rows.iter().map(render_leaderboard_row) }
                        </tbody>
                    </table>
                </div>
            }
        }
    }
}

fn render_leaderboard_row(row: &LeaderboardRow) -> Html {
    html! {
        <tr>
            <td class="model">{ &row.model }</td>
            <td>{ format!("{:.0}", row.elo) }</td>
            <td>{ row.matches }</td>
            <td>{ format!("{}-{}-{}", row.match_wins, row.match_losses, row.match_draws) }</td>
            <td>{ percent(row.match_win_rate) }</td>
            <td>{ row.rounds }</td>
            <td>{ format!("{}-{}-{}", row.round_wins, row.round_losses, row.round_ties) }</td>
            <td>{ percent(row.round_win_rate) }</td>
            <td>{ render_throw_dist(row.throw_dist) }</td>
        </tr>
    }
}

fn render_match_list(state: &UseStateHandle<Option<Result<Vec<MatchSummary>, String>>>) -> Html {
    match &**state {
        None => html! { <div class="status">{"Loading matches..."}</div> },
        Some(Err(err)) => html! { <div class="status error">{ err }</div> },
        Some(Ok(matches)) if matches.is_empty() => {
            html! { <div class="empty">{"No public finished matches yet."}</div> }
        }
        Some(Ok(matches)) => html! {
            <div class="match-list">
                { for matches.iter().map(render_match_summary) }
            </div>
        },
    }
}

fn render_match_summary(summary: &MatchSummary) -> Html {
    html! {
        <Link<Route> to={Route::Match { id: summary.match_id.to_string() }} classes="match-row">
            <div>
                <span class="model">{ &summary.model_a }</span>
                <span class="score">{ summary.score_a }</span>
                <span class="versus">{ "vs" }</span>
                <span class="score">{ summary.score_b }</span>
                <span class="model">{ &summary.model_b }</span>
            </div>
            <div class="muted">{ format!("best of {} · {} · {}", summary.best_of, winner_label(summary), time_label(summary.ended_at)) }</div>
        </Link<Route>>
    }
}

fn render_match_detail(detail: &MatchDetail) -> Html {
    html! {
        <>
            <section class="summary-band">
                <div>
                    <div class="muted">{ format!("best of {} · {}", detail.summary.best_of, reason_label(detail.summary.reason)) }</div>
                    <h2>
                        <span>{ &detail.summary.model_a }</span>
                        <span class="big-score">{ format!("{}-{}", detail.summary.score_a, detail.summary.score_b) }</span>
                        <span>{ &detail.summary.model_b }</span>
                    </h2>
                    <div class="muted">{ format!("winner: {}", winner_label(&detail.summary)) }</div>
                </div>
            </section>

            <section class="section">
                <div class="section-heading">
                    <h2>{ "Transcript" }</h2>
                    <span class="untrusted">{ "untrusted chat \u{00b7} \u{1f4ad} = private strategy" }</span>
                </div>
                { render_transcript(detail) }
            </section>
        </>
    }
}

/// Render the whole match as one chat thread: real chat = speech bubbles,
/// strategy summaries = interleaved 💭 thought bubbles, throws = move bubbles
/// with a winner emoji. Chat is placed under the round it belongs to.
fn render_transcript(detail: &MatchDetail) -> Html {
    let summary = &detail.summary;
    if detail.rounds.is_empty() && detail.chat.is_empty() {
        return html! { <div class="empty">{ "No transcript recorded." }</div> };
    }

    let mut thread: Vec<Html> = Vec::new();

    // Pre-match chatter (no round attached) opens the thread.
    for line in detail.chat.iter().filter(|c| c.round_no.is_none()) {
        thread.push(speech_bubble(line, summary));
    }

    // Walk attempts in order; when a new round number starts, drop in that
    // round's chat first, then each attempt's moves + thought bubbles.
    let mut last_round: Option<u32> = None;
    for round in &detail.rounds {
        if Some(round.round_no) != last_round {
            for line in detail
                .chat
                .iter()
                .filter(|c| c.round_no == Some(round.round_no))
            {
                thread.push(speech_bubble(line, summary));
            }
            last_round = Some(round.round_no);
        }
        let a_won = round.outcome_a == Outcome::Win;
        let b_won = round.outcome_a == Outcome::Lose;
        let tie = round.outcome_a == Outcome::Tie;
        thread.push(round_divider(round, summary));
        thread.push(move_bubble(
            &summary.model_a,
            round.throw_a,
            "left",
            a_won,
            tie,
        ));
        thread.push(thought_bubble(round.strategy_summary_a.as_deref(), "left"));
        thread.push(move_bubble(
            &summary.model_b,
            round.throw_b,
            "right",
            b_won,
            tie,
        ));
        thread.push(thought_bubble(round.strategy_summary_b.as_deref(), "right"));
    }

    html! { <div class="chat-transcript">{ for thread.into_iter() }</div> }
}

fn round_divider(round: &RoundRecord, summary: &MatchSummary) -> Html {
    let text = match round.outcome_a {
        Outcome::Tie => format!(
            "Round {} \u{00b7} attempt {} \u{2014} \u{1f91d} tie",
            round.round_no, round.attempt_no
        ),
        Outcome::Win => format!(
            "Round {} \u{00b7} attempt {} \u{2014} \u{1f3c6} {}",
            round.round_no, round.attempt_no, summary.model_a
        ),
        Outcome::Lose => format!(
            "Round {} \u{00b7} attempt {} \u{2014} \u{1f3c6} {}",
            round.round_no, round.attempt_no, summary.model_b
        ),
    };
    html! { <div class="round-divider"><span>{ text }</span></div> }
}

/// A player's throw for an attempt, with a winner/tie emoji.
fn move_bubble(model: &str, throw: Throw, side: &'static str, won: bool, tie: bool) -> Html {
    let result = if won {
        "\u{1f3c6}"
    } else if tie {
        "\u{1f91d}"
    } else {
        ""
    };
    let bubble_class = classes!(
        "chat-bubble",
        "move",
        won.then_some("winner"),
        tie.then_some("tie")
    );
    html! {
        <div class={classes!("bubble-row", side)}>
            <div class={bubble_class}>
                <span class="who">{ model }</span>
                <span class="throw-chip">{ throw_emoji(throw) }{ " " }{ throw_label(throw) }</span>
                {
                    if result.is_empty() {
                        html! {}
                    } else {
                        let title = if won { "round winner" } else { "tie" };
                        html! { <span class="result-emoji" title={title} aria-label={title}>{ result }</span> }
                    }
                }
            </div>
        </div>
    }
}

/// The private strategy summary, rendered as an interleaved 💭 thought bubble.
fn thought_bubble(strategy_summary: Option<&str>, side: &'static str) -> Html {
    html! {
        <div class={classes!("bubble-row", side)}>
            <div class={classes!("thought-bubble", side)}>
                <span class="thought-emoji" aria-hidden="true">{ "\u{1f4ad}" }</span>
                {
                    match strategy_summary {
                        Some(text) if !text.trim().is_empty() => html! { <p>{ text }</p> },
                        _ => html! { <p class="muted"><em>{ "no strategy recorded" }</em></p> },
                    }
                }
            </div>
        </div>
    }
}

/// A real chat line as a speech bubble, sided by who sent it.
fn speech_bubble(line: &ChatRecord, summary: &MatchSummary) -> Html {
    let side = if line.from_model == summary.model_a {
        "left"
    } else {
        "right"
    };
    html! {
        <div class={classes!("bubble-row", side)}>
            <div class="chat-bubble speech">
                <header>
                    <span class="who">{ &line.from_model }</span>
                    <span class="muted">{ message_meta(line) }</span>
                </header>
                <p class="bubble-body">{ &line.text }</p>
            </div>
        </div>
    }
}

fn render_human_setup(
    game: &UseStateHandle<HumanGame>,
    on_name: Callback<InputEvent>,
    on_best_of: Callback<Event>,
    start: Callback<MouseEvent>,
) -> Html {
    let disabled = game.phase != HumanPhase::Setup;
    html! {
        <section class="human-setup" aria-label="Human player setup">
            <label>
                <span>{ "Name" }</span>
                <input type="text" value={game.name.clone()} oninput={on_name} disabled={disabled} maxlength="40" />
            </label>
            <label>
                <span>{ "Match" }</span>
                <select onchange={on_best_of} disabled={disabled} value={game.best_of.to_string()}>
                    <option value="1">{ "Best of 1" }</option>
                    <option value="3">{ "Best of 3" }</option>
                    <option value="5">{ "Best of 5" }</option>
                    <option value="7">{ "Best of 7" }</option>
                </select>
            </label>
            <button type="button" class="primary-button" onclick={start} disabled={disabled}>
                { "Join Queue" }
            </button>
        </section>
    }
}

fn render_arena(game: &UseStateHandle<HumanGame>) -> Html {
    // Keep the opponent's identity hidden until the match completes.
    let opponent = match (&game.opponent, game.phase) {
        (None, _) => "waiting for opponent".to_string(),
        (Some(name), HumanPhase::Complete) => name.clone(),
        (Some(_), _) => "opponent (hidden)".to_string(),
    };
    let round = game
        .round_no
        .map(|r| format!("Round {r}"))
        .unwrap_or_else(|| "No round yet".to_string());
    let attempt = game
        .attempt_no
        .map(|a| format!("attempt {a}"))
        .unwrap_or_else(|| phase_label(game.phase).to_string());
    html! {
        <section class="arena">
            <div class="scoreboard">
                <div>
                    <span class="muted">{ "You" }</span>
                    <strong>{ game.score_you }</strong>
                </div>
                <div class="round-state">
                    <span>{ round }</span>
                    <b>{ attempt }</b>
                </div>
                <div>
                    <span class="muted">{ opponent }</span>
                    <strong>{ game.score_them }</strong>
                </div>
            </div>
            <div class="phase-strip">
                <span class={phase_class(game.phase)}>{ phase_label(game.phase) }</span>
                {
                    if let Some(err) = &game.error {
                        html! { <span class="inline-error">{ err }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
        </section>
    }
}

fn render_result(game: &UseStateHandle<HumanGame>, play_again: Callback<MouseEvent>) -> Html {
    if game.phase != HumanPhase::Complete {
        return html! {};
    }
    let (verdict, cls) = match game.score_you.cmp(&game.score_them) {
        std::cmp::Ordering::Greater => ("You won", "win"),
        std::cmp::Ordering::Less => ("You lost", "loss"),
        std::cmp::Ordering::Equal => ("No winner", "draw"),
    };
    let winner = game
        .winner
        .clone()
        .unwrap_or_else(|| "no winner".to_string());
    let opponent = game
        .opponent
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let reason = reason_label(game.end_reason);
    html! {
        <section class={classes!("match-result", cls)} aria-live="polite">
            <h2>{ verdict }</h2>
            <div class="final-score">{ format!("{} \u{2013} {}", game.score_you, game.score_them) }</div>
            <p class="reveal">{ format!("opponent was {opponent}") }</p>
            <p class="muted">{ format!("winner: {winner} \u{00b7} {reason}") }</p>
            <button type="button" class="primary-button" onclick={play_again}>{ "Play again" }</button>
        </section>
    }
}

fn render_throw_controls(
    game: &UseStateHandle<HumanGame>,
    current_game: &Rc<RefCell<HumanGame>>,
    on_strategy_input: Callback<InputEvent>,
) -> Html {
    let live_round = game.phase == HumanPhase::WaitingForThrow && game.attempt_id.is_some();
    let has_comment = !game.chat_text.trim().is_empty();
    let has_strategy = !game.strategy_summary.trim().is_empty();
    let can_throw = live_round && has_comment && has_strategy;
    html! {
        <section class="throw-pad" aria-label="Choose throw">
            <label class="strategy-summary-field">
                <span>{ "Strategy summary" }</span>
                <textarea
                    placeholder="Hidden from your opponent until the match transcript. Keep it short: what are you trying this throw?"
                    value={game.strategy_summary.clone()}
                    oninput={on_strategy_input}
                    disabled={!live_round}
                    maxlength="1000"
                />
            </label>
            <div class="throw-buttons">
                { throw_button(game.clone(), current_game.clone(), Throw::Rock, "Rock", "R", can_throw) }
                { throw_button(game.clone(), current_game.clone(), Throw::Paper, "Paper", "P", can_throw) }
                { throw_button(game.clone(), current_game.clone(), Throw::Scissors, "Scissors", "S", can_throw) }
            </div>
            {
                if live_round && (!has_comment || !has_strategy) {
                    html! { <p class="throw-hint">{ "Add a public comment and a private strategy summary before you throw." }</p> }
                } else {
                    html! {}
                }
            }
        </section>
    }
}

fn throw_button(
    game: UseStateHandle<HumanGame>,
    current_game: Rc<RefCell<HumanGame>>,
    throw: Throw,
    label: &'static str,
    mark: &'static str,
    enabled: bool,
) -> Html {
    let active = game.selected_throw == Some(throw);
    let class = classes!("throw-button", active.then_some("active"));
    let onclick = Callback::from(move |_| {
        let game = game.clone();
        let current_game = current_game.clone();
        spawn_local(async move {
            let current = current_game.borrow().clone();
            let (Some(token), Some(attempt_id)) = (current.token.clone(), current.attempt_id)
            else {
                return;
            };
            // A public comment and private strategy summary are required with every throw.
            let comment = current.chat_text.trim().to_string();
            let strategy_summary = current.strategy_summary.trim().to_string();
            if comment.is_empty() || strategy_summary.is_empty() {
                update_game(&game, &current_game, |g| {
                    g.error =
                        Some("add a comment and strategy summary before you throw".to_string());
                });
                return;
            }
            let nonce = match random_nonce_hex() {
                Ok(nonce) => nonce,
                Err(err) => {
                    update_game(&game, &current_game, |g| g.error = Some(err));
                    return;
                }
            };
            let secret = make_secret(throw, &nonce);
            let hash = commit_hash(&secret);
            // Send the required comment first, then commit the throw. If either
            // call fails we stay in WaitingForThrow so the human can retry.
            if let Err(err) = post_auth_json::<_, shared::PlayOk>(
                "/api/play/chat",
                &token,
                &PlayChatRequest {
                    text: comment.clone(),
                },
            )
            .await
            {
                update_game(&game, &current_game, |g| g.error = Some(err));
                return;
            }
            match post_auth_json::<_, shared::PlayOk>(
                "/api/play/commit",
                &token,
                &PlayCommitRequest {
                    attempt_id,
                    hash,
                    strategy_summary: strategy_summary.clone(),
                },
            )
            .await
            {
                Ok(_) => update_game(&game, &current_game, |g| {
                    g.phase = HumanPhase::WaitingForReveal;
                    g.selected_throw = Some(throw);
                    g.pending_secret = Some(secret.clone());
                    g.error = None;
                    g.chat.push(("you".to_string(), comment.clone()));
                    g.chat_text.clear();
                    g.strategy_summary.clear();
                    g.events
                        .push(format!("Commented and committed {}.", throw_label(throw)));
                }),
                Err(err) => update_game(&game, &current_game, |g| g.error = Some(err)),
            }
        });
    });

    html! {
        <button type="button" class={class} disabled={!enabled} {onclick}>
            <span>{ mark }</span>
            <b>{ label }</b>
        </button>
    }
}

fn render_chat_panel(
    game: &UseStateHandle<HumanGame>,
    on_chat_input: Callback<InputEvent>,
    send_chat: Callback<MouseEvent>,
) -> Html {
    let disabled = game.token.is_none() || game.phase == HumanPhase::Complete;
    html! {
        <section class="side-section">
            <div class="section-heading">
                <h2>{ "Messages" }</h2>
                <span class="untrusted">{ "Untrusted" }</span>
            </div>
            <div class="live-chat">
                {
                    if game.chat.is_empty() {
                        html! { <p class="empty compact">{ "No messages yet." }</p> }
                    } else {
                        let reveal = game.phase == HumanPhase::Complete;
                        html! { for game.chat.iter().map(|(from, text)| {
                            // Hide the opponent's model name (their chat is
                            // tagged with it) until the match completes.
                            let label = if reveal || from == "you" {
                                from.clone()
                            } else {
                                "opponent".to_string()
                            };
                            html! {
                                <article>
                                    <strong>{ label }</strong>
                                    <p>{ text }</p>
                                </article>
                            }
                        }) }
                    }
                }
            </div>
            <div class="chat-compose">
                <input
                    type="text"
                    placeholder="Send a message"
                    value={game.chat_text.clone()}
                    oninput={on_chat_input}
                    disabled={disabled}
                    maxlength="300"
                />
                <button type="button" onclick={send_chat} disabled={disabled}>{ "Send" }</button>
            </div>
        </section>
    }
}

fn render_event_log(game: &UseStateHandle<HumanGame>) -> Html {
    html! {
        <section class="side-section">
            <div class="section-heading">
                <h2>{ "Log" }</h2>
            </div>
            <ol class="event-log">
                { for game.events.iter().rev().take(12).map(|event| html! { <li>{ event }</li> }) }
            </ol>
        </section>
    }
}

async fn register_and_queue(name: &str, best_of: u32) -> Result<String, String> {
    let registered: PlayRegisterResponse = post_json(
        "/api/play/register",
        &PlayRegisterRequest {
            model: "human".to_string(),
            display_name: name.to_string(),
        },
    )
    .await?;
    let token = registered.token.to_string();
    post_auth_json::<_, shared::PlayOk>("/api/play/queue", &token, &PlayQueueRequest { best_of })
        .await?;
    Ok(token)
}

async fn poll_play(token: &str) -> Result<Vec<ServerMsg>, String> {
    let response = Request::get("/api/play/poll?timeout_ms=1500&limit=50")
        .header("Authorization", &format!("Bearer {token}"))
        .send()
        .await
        .map_err(|err| format!("poll failed: {err}"))?;
    if !response.ok() {
        return Err(format!("poll failed with HTTP {}", response.status()));
    }
    response
        .json::<shared::PlayPollResponse>()
        .await
        .map(|body| body.messages)
        .map_err(|err| format!("invalid poll response: {err}"))
}

fn apply_server_messages(
    game: UseStateHandle<HumanGame>,
    current_game: Rc<RefCell<HumanGame>>,
    messages: Vec<ServerMsg>,
) {
    let mut next = current_game.borrow().clone();
    for message in messages {
        match message {
            ServerMsg::Queued { best_of, position } => {
                next.phase = HumanPhase::Queueing;
                next.events.push(format!(
                    "Queued for best-of-{best_of}; position {position}."
                ));
            }
            ServerMsg::MatchStart {
                match_id,
                opponent_model,
                best_of,
                ..
            } => {
                next.match_id = Some(match_id);
                // Stored, but kept hidden in the UI until the match completes.
                next.opponent = Some(opponent_model);
                next.events.push(format!(
                    "Matched for best-of-{best_of}. Opponent revealed when the match ends."
                ));
            }
            ServerMsg::RoundStart {
                attempt_id,
                round_no,
                attempt_no,
                score_you,
                score_them,
                ..
            } => {
                next.phase = HumanPhase::WaitingForThrow;
                next.round_no = Some(round_no);
                next.attempt_no = Some(attempt_no);
                next.score_you = score_you;
                next.score_them = score_them;
                next.attempt_id = Some(attempt_id);
                next.pending_secret = None;
                next.selected_throw = None;
                next.events
                    .push(format!("Round {round_no}, attempt {attempt_no}."));
            }
            ServerMsg::AwaitReveal { attempt_id } => {
                if next.attempt_id == Some(attempt_id) {
                    if let (Some(token), Some(secret)) =
                        (next.token.clone(), next.pending_secret.clone())
                    {
                        let game_for_reveal = game.clone();
                        let current_game_for_reveal = current_game.clone();
                        spawn_local(async move {
                            match post_auth_json::<_, shared::PlayOk>(
                                "/api/play/reveal",
                                &token,
                                &PlayRevealRequest { attempt_id, secret },
                            )
                            .await
                            {
                                Ok(_) => {
                                    update_game(&game_for_reveal, &current_game_for_reveal, |g| {
                                        g.events.push("Revealed committed throw.".to_string());
                                    })
                                }
                                Err(err) => {
                                    update_game(&game_for_reveal, &current_game_for_reveal, |g| {
                                        g.error = Some(err)
                                    });
                                }
                            }
                        });
                    }
                }
            }
            ServerMsg::RoundResult {
                round_no,
                attempt_no,
                your_throw,
                their_throw,
                outcome,
                score_you,
                score_them,
                ..
            } => {
                next.score_you = score_you;
                next.score_them = score_them;
                next.phase = HumanPhase::Queueing;
                next.events.push(format!(
                    "Round {round_no}.{attempt_no}: you played {}, they played {}; {}.",
                    throw_label(your_throw),
                    throw_label(their_throw),
                    human_outcome_label(outcome)
                ));
            }
            ServerMsg::ChatFrom { from_model, text } => {
                next.chat.push((from_model, text));
            }
            ServerMsg::MatchEnd {
                winner_model,
                score_you,
                score_them,
                reason,
            } => {
                next.phase = HumanPhase::Complete;
                next.token = None;
                next.score_you = score_you;
                next.score_them = score_them;
                next.winner = winner_model.clone();
                next.end_reason = Some(reason);
                let winner = winner_model.unwrap_or_else(|| "no winner".to_string());
                next.events.push(format!(
                    "Match ended: {winner}, {}.",
                    reason_label(Some(reason))
                ));
            }
            ServerMsg::Error { message } => next.error = Some(message),
            ServerMsg::Registered { .. }
            | ServerMsg::TurnDeadline { .. }
            | ServerMsg::Heartbeat => {}
        }
    }
    trim_live_match_state(&mut next);
    set_game(&game, &current_game, next);
}

async fn post_json<T, R>(url: &str, body: &T) -> Result<R, String>
where
    T: serde::Serialize,
    R: serde::de::DeserializeOwned,
{
    let request = Request::post(url)
        .json(body)
        .map_err(|err| format!("request failed: {err}"))?;
    parse_json_response(request.send().await).await
}

async fn post_auth_json<T, R>(url: &str, token: &str, body: &T) -> Result<R, String>
where
    T: serde::Serialize,
    R: serde::de::DeserializeOwned,
{
    let request = Request::post(url)
        .header("Authorization", &format!("Bearer {token}"))
        .json(body)
        .map_err(|err| format!("request failed: {err}"))?;
    parse_json_response(request.send().await).await
}

async fn parse_json_response<R>(
    response: Result<gloo_net::http::Response, gloo_net::Error>,
) -> Result<R, String>
where
    R: serde::de::DeserializeOwned,
{
    let response = response.map_err(|err| format!("request failed: {err}"))?;
    if !response.ok() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(format!("request failed with HTTP {status}: {text}"));
    }
    response
        .json::<R>()
        .await
        .map_err(|err| format!("invalid response: {err}"))
}

fn update_game(
    game: &UseStateHandle<HumanGame>,
    current_game: &Rc<RefCell<HumanGame>>,
    f: impl FnOnce(&mut HumanGame),
) {
    let mut next = current_game.borrow().clone();
    f(&mut next);
    trim_live_match_state(&mut next);
    set_game(game, current_game, next);
}

fn set_game(
    game: &UseStateHandle<HumanGame>,
    current_game: &Rc<RefCell<HumanGame>>,
    next: HumanGame,
) {
    *current_game.borrow_mut() = next.clone();
    game.set(next);
}

fn random_nonce_hex() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    let window = web_sys::window().ok_or_else(|| "browser window unavailable".to_string())?;
    let crypto = window
        .crypto()
        .map_err(|_| "browser crypto unavailable".to_string())?;
    crypto
        .get_random_values_with_u8_array(&mut bytes)
        .map_err(|_| "could not generate nonce".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn trim_live_match_state(game: &mut HumanGame) {
    if game.events.len() > 100 {
        game.events.drain(..game.events.len() - 100);
    }
}

fn phase_label(phase: HumanPhase) -> &'static str {
    match phase {
        HumanPhase::Setup => "ready",
        HumanPhase::Queueing => "waiting",
        HumanPhase::WaitingForThrow => "choose throw",
        HumanPhase::WaitingForReveal => "committed",
        HumanPhase::Complete => "complete",
    }
}

fn phase_class(phase: HumanPhase) -> &'static str {
    match phase {
        HumanPhase::Setup => "phase ready",
        HumanPhase::Queueing => "phase waiting",
        HumanPhase::WaitingForThrow => "phase live",
        HumanPhase::WaitingForReveal => "phase waiting",
        HumanPhase::Complete => "phase done",
    }
}

fn human_outcome_label(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Win => "you won",
        Outcome::Lose => "you lost",
        Outcome::Tie => "tie",
    }
}

fn sort_leaderboard(rows: &mut [LeaderboardRow], sort_key: SortKey) {
    rows.sort_by(|a, b| match sort_key {
        SortKey::Elo => cmp_f64_desc(a.elo, b.elo).then_with(|| a.model.cmp(&b.model)),
        SortKey::Model => a.model.cmp(&b.model),
        SortKey::Matches => b
            .matches
            .cmp(&a.matches)
            .then_with(|| a.model.cmp(&b.model)),
        SortKey::MatchWinRate => {
            cmp_f64_desc(a.match_win_rate, b.match_win_rate).then_with(|| a.model.cmp(&b.model))
        }
        SortKey::RoundWinRate => {
            cmp_f64_desc(a.round_win_rate, b.round_win_rate).then_with(|| a.model.cmp(&b.model))
        }
    });
}

fn cmp_f64_desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

fn render_throw_dist(dist: [u32; 3]) -> Html {
    let total = dist.iter().sum::<u32>();
    let items = [
        ("R", dist[0], "#7aa2f7"),
        ("P", dist[1], "#9ece6a"),
        ("S", dist[2], "#f7768e"),
    ];

    html! {
        <div class="throw-dist" title={format!("rock {}, paper {}, scissors {}", dist[0], dist[1], dist[2])}>
            { for items.into_iter().map(|(label, count, color)| {
                let width = if total == 0 { 0.0 } else { count as f64 / total as f64 * 100.0 };
                html! {
                    <span style={format!("--w:{width:.2}%;--c:{color}")}>
                        <span>{ label }</span>
                        <b>{ count }</b>
                    </span>
                }
            }) }
        </div>
    }
}

fn percent(value: f64) -> String {
    format!("{:.1}%", value * 100.0)
}

fn winner_label(summary: &MatchSummary) -> String {
    summary
        .winner_model
        .clone()
        .unwrap_or_else(|| "none".to_string())
}

fn reason_label(reason: Option<EndReason>) -> &'static str {
    match reason {
        Some(EndReason::WinByScore) => "win by score",
        Some(EndReason::Forfeit) => "forfeit",
        Some(EndReason::Disconnect) => "disconnect",
        Some(EndReason::Timeout) => "timeout",
        Some(EndReason::ServerAbort) => "server abort",
        None => "unknown end reason",
    }
}

fn time_label(time: Option<chrono::DateTime<chrono::Utc>>) -> String {
    time.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| "in progress".to_string())
}

fn message_meta(line: &ChatRecord) -> String {
    match line.round_no {
        Some(round) => format!(
            "round {} · {}",
            round,
            line.created_at.format("%H:%M:%S UTC")
        ),
        None => line.created_at.format("%H:%M:%S UTC").to_string(),
    }
}

fn throw_label(throw: Throw) -> &'static str {
    match throw {
        Throw::Rock => "rock",
        Throw::Paper => "paper",
        Throw::Scissors => "scissors",
    }
}

fn throw_emoji(throw: Throw) -> &'static str {
    match throw {
        Throw::Rock => "\u{1faa8}",
        Throw::Paper => "\u{1f4c4}",
        Throw::Scissors => "\u{2702}\u{fe0f}",
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
