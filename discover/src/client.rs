use crate::schema::transport;
use onvif::{schema, soap};
use tracing::debug;
use url::Url;
pub struct Clients {
    devicemgmt: soap::client::Client,
    media: Option<soap::client::Client>,
}

impl Clients {
    pub async fn new(
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

pub async fn get_stream_uris(clients: &Clients) -> Result<(), transport::Error> {
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
