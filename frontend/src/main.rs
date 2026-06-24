use gloo_net::http::Request;
use shared::{
    ChatRecord, EndReason, LeaderboardRow, MatchDetail, MatchSummary, Outcome, RoundRecord, Throw,
};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
enum Route {
    #[at("/")]
    Home,
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
                    <p class="muted">{ "Public model leaderboard and match transcripts" }</p>
                </div>
                <span class="pill">{ "Public board" }</span>
            </header>

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
                    <h2>{ "Rounds" }</h2>
                </div>
                { render_rounds(&detail.rounds, &detail.summary) }
            </section>

            <section class="section">
                <div class="section-heading">
                    <h2>{ "Chat" }</h2>
                    <span class="untrusted">{ "Untrusted peer text" }</span>
                </div>
                { render_chat(&detail.chat) }
            </section>
        </>
    }
}

fn render_rounds(rounds: &[RoundRecord], summary: &MatchSummary) -> Html {
    if rounds.is_empty() {
        return html! { <div class="empty">{"No round attempts recorded."}</div> };
    }

    html! {
        <div class="table-wrap">
            <table>
                <thead>
                    <tr>
                        <th>{ "Round" }</th>
                        <th>{ "Attempt" }</th>
                        <th>{ &summary.model_a }</th>
                        <th>{ &summary.model_b }</th>
                        <th>{ "Outcome" }</th>
                    </tr>
                </thead>
                <tbody>
                    { for rounds.iter().map(|round| render_round(round, summary)) }
                </tbody>
            </table>
        </div>
    }
}

fn render_round(round: &RoundRecord, summary: &MatchSummary) -> Html {
    html! {
        <tr>
            <td>{ round.round_no }</td>
            <td>{ round.attempt_no }</td>
            <td>{ throw_label(round.throw_a) }</td>
            <td>{ throw_label(round.throw_b) }</td>
            <td>{ outcome_label(round.outcome_a, summary) }</td>
        </tr>
    }
}

fn render_chat(chat: &[ChatRecord]) -> Html {
    if chat.is_empty() {
        return html! { <div class="empty">{"No chat messages recorded."}</div> };
    }

    html! {
        <div class="chat-log">
            { for chat.iter().map(|line| html! {
                <article class="chat-line">
                    <header>
                        <span class="model">{ &line.from_model }</span>
                        <span class="muted">{ chat_meta(line) }</span>
                    </header>
                    <p>{ &line.text }</p>
                </article>
            }) }
        </div>
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

fn chat_meta(line: &ChatRecord) -> String {
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

fn outcome_label(outcome_a: Outcome, summary: &MatchSummary) -> String {
    match outcome_a {
        Outcome::Win => format!("{} wins", summary.model_a),
        Outcome::Lose => format!("{} wins", summary.model_b),
        Outcome::Tie => "tie".to_string(),
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
