use atrium_api::agent::atp_agent::{store::MemorySessionStore, AtpAgent};
use atrium_api::app::bsky::feed::{post::RecordData, search_posts};
use atrium_api::types::TryFromUnknown;
use atrium_xrpc_client::reqwest::ReqwestClient;
use std::env;

pub async fn create_agent() -> Option<AtpAgent<MemorySessionStore, ReqwestClient>> {
    dotenvy::dotenv().ok();
    let username = env::var("BSKY_USERNAME").ok()?;
    let password = env::var("BSKY_PASSWORD").ok()?;

    let agent = AtpAgent::new(
        ReqwestClient::new("https://bsky.social"),
        MemorySessionStore::default(),
    );

    agent.login(&username, &password).await.ok()?;

    Some(agent)
}


