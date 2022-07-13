mod client;

use crate::schema::transport;
use anyhow::Result;
use onvif::{discovery, schema};
use serde_yaml;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{error, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .init();
    let conf_path = "conf.yaml";
    // let conf = std::fs::read_to_string(conf_path)?.as_str();
    let f = std::fs::File::open(conf_path)?;
    let data: serde_yaml::Value = serde_yaml::from_reader(f)?;

    let mut users = BTreeMap::new();

    data.as_mapping().unwrap().iter().for_each(|(k, v)| {
        let user = users
            .entry(k.as_str().unwrap())
            .or_insert(Box::new(BTreeMap::new()));
        v.as_mapping().unwrap().iter().for_each(|(k, v)| {
            user.insert(k.as_str().unwrap(), v.as_str().unwrap());
        });
    });

    let conf = Arc::new(users);

    use futures_util::stream::StreamExt;
    const MAX_CONCURRENT_JUMPERS: usize = 100;
    // let hosts: Arc<Mutex<Vec<(&str, &str)>>> = Arc::new(Mutex::new(Vec::new()));
    // let last = hosts.clone();

    discovery::discover(std::time::Duration::from_secs(1))
        .await
        .unwrap()
        .for_each_concurrent(MAX_CONCURRENT_JUMPERS, move |addr: discovery::Device| {
            let x = conf.clone();
            async move {
                let uri = format!(
                    "{}://{}",
                    addr.url.scheme(),
                    addr.url.host().unwrap().to_string()
                );

                get_stream(addr.name.unwrap().as_str(), &uri, x)
                    .await
                    .unwrap_or_else(|_error| {
                        // println!("{}", error);
                    });
            }
        })
        .await;

    Ok(())
}

async fn get_stream(
    name: &str,
    uri: &str,
    conf: Arc<BTreeMap<&str, Box<BTreeMap<&str, &str>>>>,
) -> Result<(), transport::Error> {
    let users = conf.get(name);
    // let mut clients: Clients;
    match users {
        Some(users) => {
            warn!("Device found match: {}\t {} {}", name, uri, users.len());
            for (user, pass) in users.iter() {
                warn!("\t{}:{}", user, pass);
                let c = client::Clients::new(uri, Some(*user), Some(*pass)).await;
                if let Ok(c) = c {
                    let clients = c;
                    let r = client::get_stream_uris(&clients).await;
                    if let Ok(_r) = r {
                        return Ok(());
                    }
                };
            }
        }
        None => {
            error!("Device found no match config: {}\t {}", name, uri);
            let c = client::Clients::new(uri, Some("admin"), Some("hx1235698")).await;
            if let Ok(c) = c {
                let clients = c;
                client::get_stream_uris(&clients)
                    .await
                    .unwrap_or_else(|error| {
                        warn!("{}", error);
                    });
            };
        }
    }

    Ok(())
}
