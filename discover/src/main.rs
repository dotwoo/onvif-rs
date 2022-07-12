use crate::schema::transport;
use anyhow::Result;
use onvif::{discovery, schema, soap};
use serde_yaml;
use std::collections::BTreeMap;
use std::sync::Arc;
use tracing::{debug, error, warn};
use url::Url;

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
                let c = Clients::new(uri, Some(*user), Some(*pass)).await;
                if let Ok(c) = c {
                    let clients = c;
                    let r = get_stream_uris(&clients).await;
                    if let Ok(_r) = r {
                        return Ok(());
                    }
                };
            }
        }
        None => {
            error!("Device found no match config: {}\t {}", name, uri);
            let c = Clients::new(uri, Some("admin"), Some("hx1235698")).await;
            if let Ok(c) = c {
                let clients = c;
                get_stream_uris(&clients).await.unwrap_or_else(|error| {
                    warn!("{}", error);
                });
            };
        }
    }

    Ok(())
}

struct Clients {
    devicemgmt: soap::client::Client,
    media: Option<soap::client::Client>,
}

impl Clients {
    async fn new(
        uri: &str,
        username: Option<&str>,
        password: Option<&str>,
    ) -> Result<Self, String> {
        let creds = match (username, password) {
            (Some(username), Some(password)) => Some(soap::client::Credentials {
                username: username.to_string(),
                password: password.to_string(),
            }),
            (None, None) => None,
            _ => panic!("username and password must be specified together"),
        };
        let base_uri = uri;
        let devicemgmt_uri = Url::parse(format!("{}/onvif/device_service", base_uri).as_str());
        if let Err(e) = devicemgmt_uri {
            return Err(format!("{}", e));
        }
        let devicemgmt_uri = devicemgmt_uri.unwrap();

        let mut out = Self {
            devicemgmt: soap::client::ClientBuilder::new(&devicemgmt_uri)
                .credentials(creds.clone())
                .build(),
            media: None,
        };
        let services =
            schema::devicemgmt::get_services(&out.devicemgmt, &Default::default()).await?;
        for s in &services.service {
            let url = Url::parse(&s.x_addr).map_err(|e| e.to_string())?;
            if !url.as_str().starts_with(base_uri) {
                return Err(format!(
                    "Service URI {} is not within base URI {}",
                    &s.x_addr, &base_uri
                ));
            }
            let svc = Some(
                soap::client::ClientBuilder::new(&url)
                    .credentials(creds.clone())
                    .build(),
            );
            match s.namespace.as_str() {
                "http://www.onvif.org/ver10/device/wsdl" => {
                    if s.x_addr != devicemgmt_uri.as_str() {
                        return Err(format!(
                            "advertised device mgmt uri {} not expected {}",
                            &s.x_addr, &devicemgmt_uri
                        ));
                    }
                }
                "http://www.onvif.org/ver10/media/wsdl" => out.media = svc,

                _ => debug!("unknown service: {:?}", s),
            }
        }
        Ok(out)
    }
}

async fn get_stream_uris(clients: &Clients) -> Result<(), transport::Error> {
    let media_client = clients
        .media
        .as_ref()
        .ok_or_else(|| transport::Error::Other("Client media is not available".into()))?;
    let profiles = schema::media::get_profiles(media_client, &Default::default()).await?;
    debug!("get_profiles response: {:#?}", &profiles);
    let requests: Vec<_> = profiles
        .profiles
        .iter()
        .map(|p: &schema::onvif::Profile| schema::media::GetStreamUri {
            profile_token: schema::onvif::ReferenceToken(p.token.0.clone()),
            stream_setup: schema::onvif::StreamSetup {
                stream: schema::onvif::StreamType::RtpUnicast,
                transport: schema::onvif::Transport {
                    protocol: schema::onvif::TransportProtocol::Rtsp,
                    tunnel: vec![],
                },
            },
        })
        .collect();

    let responses = futures_util::future::try_join_all(
        requests
            .iter()
            .map(|r| schema::media::get_stream_uri(media_client, r)),
    )
    .await?;
    for (p, resp) in profiles.profiles.iter().zip(responses.iter()) {
        print!("stream name={}", &p.name.0);
        print!("\t{}", &resp.media_uri.uri);
        if let Some(ref v) = p.video_encoder_configuration {
            print!("\t{}x{}", v.resolution.width, v.resolution.height);
            if let Some(ref r) = v.rate_control {
                print!("\t{}", r.frame_rate_limit);
            }
        }
        println!();
    }
    Ok(())
}
