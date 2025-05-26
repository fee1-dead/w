use std::env::args;
use std::sync::Arc;

use futures_util::future::join_all;
use tokio::sync::Semaphore;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let user = args().nth(1).expect("username");

    let client = w::ClientBuilder::new("https://meta.wikimedia.org/w/api.php")
        .user_agent("fee1-dead/mw examples/blockfinder")
        .anonymous()?;

    let v = client
        .get([
            ("action", "query"),
            ("meta", "globaluserinfo"),
            ("guiuser", &user),
            ("guiprop", "merged"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;

    let mut futures = Vec::new();
    let semaphore = Arc::new(Semaphore::new(8));
    for v in v["query"]["globaluserinfo"]["merged"].as_array().unwrap() {
        let base = v["url"].as_str().unwrap().to_owned();
        let api = format!("{}/w/api.php", base);
        let u = format!("User:{user}");
        let client = client.clone();
        let semaphore = semaphore.clone();

        futures.push(tokio::spawn(async move {
            let s = semaphore.acquire().await.unwrap();
            let v = client
                .with_url(&api)
                .get([
                    ("action", "query"),
                    ("list", "logevents"),
                    ("letype", "block"),
                    ("letitle", &u),
                ])
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap();

            drop(s);

            let events = v["query"]["logevents"].as_array().unwrap();

            if events.is_empty() {
                println!("{base} - OK");
            } else {
                println!("Please check {base}/w/index.php?title=Special:Log/block&page={u}");
            }
        }));
    }

    join_all(futures).await;

    Ok(())
}
