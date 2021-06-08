mod error;

use error::{ChunkingRequest, InvalidPath, RequestError, RequestIssue, Base64Error, EncodingError, ParseError};
use http::request::Parts;
use hyper::{
    body::Body,
    server::{conn::AddrStream, Server},
    service, Request, Response,
};
use snafu::ResultExt;
use std::{
    convert::TryFrom,
    env,
    error::Error,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    collections::HashMap,
};
use tracing::{debug, error, info, trace};
use tracing_log::LogTracer;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};
use twilight_http::{
    client::Client, request::Request as TwilightRequest, routing::Path, API_VERSION,
};

#[cfg(feature = "expose-metrics")]
use std::{future::Future, pin::Pin, time::Instant};

#[cfg(feature = "expose-metrics")]
use lazy_static::lazy_static;
#[cfg(feature = "expose-metrics")]
use prometheus::{HistogramOpts, HistogramVec, Registry, TextEncoder, Encoder};
use twilight_http::request::Method;
use twilight_model::id::UserId;
use std::sync::Arc;
use parking_lot::Mutex;
use std::str;

#[cfg(feature = "expose-metrics")]
lazy_static! {
    static ref METRIC_KEY: String =
        env::var("METRIC_KEY").unwrap_or_else(|_| "twilight_http_proxy".into());

    static ref REGISTRY: Registry = Registry::new();

    static ref HISTOGRAM: HistogramVec = HistogramVec::new(
        HistogramOpts::new(METRIC_KEY.as_str(), "Response Times"),
        &["method", "route", "status"]
    ).unwrap();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    LogTracer::init()?;

    let log_filter_layer =
        EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;
    let log_fmt_layer = fmt::layer();

    let log_subscriber = tracing_subscriber::registry()
        .with(log_filter_layer)
        .with(log_fmt_layer);

    tracing::subscriber::set_global_default(log_subscriber)?;

    let host_raw = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into());
    let host = IpAddr::from_str(&host_raw)?;
    let port = env::var("PORT").unwrap_or_else(|_| "80".into()).parse()?;

    let clients: Arc<Mutex<HashMap<UserId, Client>>> = Arc::new(Mutex::new(HashMap::new()));

    let address = SocketAddr::from((host, port));

    #[cfg(feature = "expose-metrics")]
        REGISTRY.register(Box::new(HISTOGRAM.clone()))?;

    // The closure inside `make_service_fn` is run for each connection,
    // creating a 'service' to handle requests for that specific connection.
    let service = service::make_service_fn(move |addr: &AddrStream| {
        debug!("Connection from: {:?}", addr);
        let clients = Arc::clone(&clients);

        async move {
            Ok::<_, RequestError>(service::service_fn(move |incoming: Request<Body>| {
                #[cfg(feature = "expose-metrics")]
                    {
                        let uri = incoming.uri();

                        if uri.path() == "/metrics" {
                            handle_metrics()
                        } else {
                            Box::pin(handle_request(Arc::clone(&clients), incoming))
                        }
                    }

                #[cfg(not(feature = "expose-metrics"))]
                    {
                        handle_request(Arc::clone(&clients), incoming)
                    }
            }))
        }
    });

    let server = Server::bind(&address).serve(service);

    info!("Listening on http://{}", address);

    if let Err(why) = server.await {
        error!("Fatal server error: {}", why);
    }

    Ok(())
}

fn path_name(path: &Path) -> &'static str {
    match path {
        Path::ChannelsId(..) => "Channel",
        Path::ChannelsIdInvites(..) => "Channel invite",
        Path::ChannelsIdMessages(..) => "Channel message",
        Path::ChannelsIdMessagesBulkDelete(..) => "Bulk delete message",
        Path::ChannelsIdMessagesId(..) => "Channel message",
        Path::ChannelsIdMessagesIdReactions(..) => "Message reaction",
        Path::ChannelsIdMessagesIdReactionsUserIdType(..) => "Message reaction for user",
        Path::ChannelsIdPermissionsOverwriteId(..) => "Channel permission override",
        Path::ChannelsIdPins(..) => "Channel pins",
        Path::ChannelsIdPinsMessageId(..) => "Specific channel pin",
        Path::ChannelsIdTyping(..) => "Typing indicator",
        Path::ChannelsIdWebhooks(..) => "Webhook",
        Path::Gateway => "Gateway",
        Path::GatewayBot => "Gateway bot info",
        Path::Guilds => "Guilds",
        Path::GuildsId(..) => "Guild",
        Path::GuildsIdBans(..) => "Guild bans",
        Path::GuildsIdAuditLogs(..) => "Guild audit logs",
        Path::GuildsIdBansUserId(..) => "Guild ban for user",
        Path::GuildsIdChannels(..) => "Guild channel",
        Path::GuildsIdWidget(..) => "Guild widget",
        Path::GuildsIdEmojis(..) => "Guild emoji",
        Path::GuildsIdEmojisId(..) => "Specific guild emoji",
        Path::GuildsIdIntegrations(..) => "Guild integrations",
        Path::GuildsIdIntegrationsId(..) => "Specific guild integration",
        Path::GuildsIdIntegrationsIdSync(..) => "Sync guild integration",
        Path::GuildsIdInvites(..) => "Guild invites",
        Path::GuildsIdMembers(..) => "Guild members",
        Path::GuildsIdMembersId(..) => "Specific guild member",
        Path::GuildsIdMembersIdRolesId(..) => "Guild member role",
        Path::GuildsIdMembersMeNick(..) => "Modify own nickname",
        Path::GuildsIdPreview(..) => "Guild preview",
        Path::GuildsIdPrune(..) => "Guild prune",
        Path::GuildsIdRegions(..) => "Guild region",
        Path::GuildsIdRoles(..) => "Guild roles",
        Path::GuildsIdRolesId(..) => "Specific guild role",
        Path::GuildsIdVanityUrl(..) => "Guild vanity invite",
        Path::GuildsIdWebhooks(..) => "Guild webhooks",
        Path::InvitesCode => "Invite info",
        Path::UsersId => "User info",
        Path::UsersIdConnections => "User connections",
        Path::UsersIdChannels => "User channels",
        Path::UsersIdGuilds => "User in guild",
        Path::UsersIdGuildsId => "Guild from user",
        Path::VoiceRegions => "Voice region list",
        Path::WebhooksId(..) => "Webhook",
        Path::OauthApplicationsMe => "Current application info",
        _ => "Unknown path!",
    }
}

async fn handle_request(
    clients: Arc<Mutex<HashMap<UserId, Client>>>,
    request: Request<Body>,
) -> Result<Response<Body>, RequestError> {
    let api_url: String = format!("/api/v{}/", API_VERSION);
    debug!("Incoming request: {:?}", request);

    let (parts, body) = request.into_parts();
    let Parts {
        method,
        uri,
        headers,
        ..
    } = parts;
    let converted_method = convert_method(method.clone())?;

    let trimmed_path = if uri.path().starts_with(&api_url) {
        uri.path().replace(&api_url, "")
    } else {
        uri.path().to_owned()
    };
    let path = Path::try_from((converted_method, trimmed_path.as_ref())).context(InvalidPath)?;

    let bytes = (hyper::body::to_bytes(body).await.context(ChunkingRequest)?).to_vec();

    // Find bot id
    // TODO: Better error handling
    let mut token = match headers.get("Authorization").map(|a| a.to_str().ok()).flatten() {
        Some(v) => v,
        None => return Err(RequestError::MissingAuthorization),
    }.to_owned();

    // Can never be None
    if token.starts_with("Bot ") {
        token = token[4..].to_owned();
    }

    let id_b64 = token.split(".").next().unwrap();
    let id_bytes = base64::decode(id_b64).context(Base64Error)?;
    let bot_id = UserId(str::from_utf8(&id_bytes[..]).context(EncodingError)?.parse().context(ParseError)?);

    let path_and_query = match uri.path_and_query() {
        Some(v) => v.as_str().replace(&api_url, "").into(),
        None => {
            debug!("No path in URI: {:?}", uri);

            return Err(RequestError::NoPath { uri });
        }
    };
    let body = if bytes.is_empty() { None } else { Some(bytes) };
    let p = path_name(&path);
    let m = method.as_str();
    let raw_request = TwilightRequest {
        body,
        form: None,
        headers: Some(headers),
        method: converted_method,
        path,
        path_str: path_and_query,
    };

    #[cfg(feature = "expose-metrics")]
        let start = Instant::now();

    let client: Client;
    {
        let mut clients = clients.lock();
        client = get_client(bot_id, token.as_str(), &mut *clients);
    }

    let resp = client.raw(raw_request).await.context(RequestIssue)?;

    #[cfg(feature = "expose-metrics")]
        let end = Instant::now();

    trace!("Response: {:?}", resp);

    #[cfg(feature = "expose-metrics")]
        HISTOGRAM
        .with_label_values(&[m, p, resp.status().to_string().as_str()])
        .observe((end - start).as_secs_f64());

    debug!("{} {}: {}", m, p, resp.status());

    Ok(resp)
}

fn convert_method(method: http::Method) -> Result<Method, RequestError> {
    match method {
        http::Method::DELETE => Ok(Method::Delete),
        http::Method::GET => Ok(Method::Get),
        http::Method::PATCH => Ok(Method::Patch),
        http::Method::POST => Ok(Method::Post),
        http::Method::PUT => Ok(Method::Put),
        other => Err(RequestError::MethodNotAllowed { method: String::from(other.as_str()) })
    }
}

fn get_client(bot_id: UserId, token: &str, clients: &mut HashMap<UserId, Client>) -> Client {
    match clients.get(&bot_id) {
        Some(v) => v.clone(),
        None => {
            let client = Client::new(token);
            clients.insert(bot_id, client.clone());
            client
        }
    }
}

#[cfg(feature = "expose-metrics")]
fn handle_metrics() -> Pin<Box<dyn Future<Output=Result<Response<Body>, RequestError>> + Send>> {
    Box::pin(async move {
        let mut buffer = Vec::new();

        if let Err(e) = TextEncoder::new().encode(&REGISTRY.gather(), &mut buffer) {
            error!("error while encoding metrics: {:?}", e);

            return Ok(Response::builder()
                .status(500)
                .body(Body::from(format!("{:?}", e)))
                .unwrap());
        }

        match String::from_utf8(buffer) {
            Ok(s) => {
                Ok(Response::builder()
                    .body(Body::from(s))
                    .unwrap())
            }

            Err(e) => {
                Ok(Response::builder()
                    .status(500)
                    .body(Body::from(format!("{:?}", e)))
                    .unwrap())
            }
        }
    })
}
