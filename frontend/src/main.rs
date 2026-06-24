use gloo_net::http::Request;
use gloo_timers::future::sleep;
use shared::{AppSocket, ClientMsg, HealthResponse, ServerMsg};
use std::time::Duration;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::*;

#[derive(Clone, Routable, PartialEq)]
enum Route {
    #[at("/")]
    Home,
    #[not_found]
    #[at("/404")]
    NotFound,
}

fn switch(route: Route) -> Html {
    match route {
        Route::Home => html! { <Home /> },
        Route::NotFound => html! { <h1>{ "404 - Not Found" }</h1> },
    }
}

#[function_component(App)]
pub fn app() -> Html {
    html! {
        <BrowserRouter>
            <Switch<Route> render={switch} />
        </BrowserRouter>
    }
}

#[function_component(Home)]
fn home() -> Html {
    let health = use_state(|| None::<String>);
    let ws_status = use_state(|| "Connecting...".to_string());
    let ws_messages = use_state(Vec::<String>::new);

    // Health check via HTTP
    {
        let health = health.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match Request::get("/api/health").send().await {
                    Ok(resp) => {
                        if let Ok(data) = resp.json::<HealthResponse>().await {
                            health.set(Some(data.status));
                        }
                    }
                    Err(e) => health.set(Some(format!("Error: {}", e))),
                }
            });
        });
    }

    // WebSocket connection via ws-bridge
    {
        let ws_status = ws_status.clone();
        let ws_messages = ws_messages.clone();
        use_effect_with((), move |_| {
            match ws_bridge::yew_client::connect::<AppSocket>() {
                Ok(conn) => {
                    ws_status.set("Connected".to_string());
                    let (mut tx, mut rx) = conn.split();

                    // Ping loop — sends a Ping every 5 seconds
                    spawn_local(async move {
                        loop {
                            sleep(Duration::from_secs(5)).await;
                            if tx.send(ClientMsg::Ping).await.is_err() {
                                break;
                            }
                        }
                    });

                    // Receive loop — updates UI state on each message
                    let msgs = ws_messages;
                    let status = ws_status;
                    spawn_local(async move {
                        while let Some(result) = rx.recv().await {
                            match result {
                                Ok(ServerMsg::Heartbeat) => {
                                    let mut current = (*msgs).clone();
                                    current.push("Received: Heartbeat".to_string());
                                    if current.len() > 10 {
                                        current.drain(..current.len() - 10);
                                    }
                                    msgs.set(current);
                                }
                                Ok(ServerMsg::Error { message }) => {
                                    let mut current = (*msgs).clone();
                                    current.push(format!("Received: Error — {}", message));
                                    msgs.set(current);
                                }
                                Ok(ServerMsg::ServerShutdown { reason, .. }) => {
                                    status.set(format!("Server shutting down: {}", reason));
                                    break;
                                }
                                Err(e) => {
                                    status.set(format!("WebSocket error: {}", e));
                                    break;
                                }
                            }
                        }
                    });
                }
                Err(e) => {
                    ws_status.set(format!("Connect failed: {}", e));
                }
            }
        });
    }

    html! {
        <div>
            <h1>{ "App" }</h1>
            <div class="status">
                { match (*health).as_ref() {
                    Some(s) => format!("Backend: {}", s),
                    None => "Checking backend...".to_string(),
                }}
            </div>
            <div class="ws-status">
                { format!("WebSocket: {}", *ws_status) }
            </div>
            <div class="ws-messages">
                <h3>{ "WebSocket messages" }</h3>
                <ul>
                    { for (*ws_messages).iter().map(|m| html! { <li>{ m }</li> }) }
                </ul>
            </div>
        </div>
    }
}

fn main() {
    yew::Renderer::<App>::new().render();
}
