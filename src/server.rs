use axum::{
    extract::{Path, State},
    response::{Html, IntoResponse, Redirect, Response},
    routing::get,
    Form, Router, Server,
};
use clap::Parser;
use fast_qr::{
    convert::{svg::SvgBuilder, Builder, Shape},
    QRBuilder, ECL,
};
use http::{header, uri::InvalidUri, HeaderMap, StatusCode, Uri};
use rand::Rng;
use serde::Deserialize;
use sled::Db;
use std::{iter::repeat_with, path::PathBuf, sync::Arc};

// Alphanumerics excluding ambiguous ones like 0/O or 1/I/l
const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz0123456789";
const WHITELIST: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

const HTML_ROOT: &str = include_str!("./html/index.html");

#[derive(Deserialize)]
struct Submission {
    slug: Option<String>,
    uri: String,
    token: String,
}

#[derive(Clone)]
struct AppState {
    db: Arc<Database>,
    len: usize,
    domain: String,
    token: String,
}

fn is_clean(slug: &str) -> bool {
    slug.is_ascii() && slug.bytes().find(|c| !WHITELIST.contains(c)).is_none()
}

struct Database(Db);

impl Database {
    fn new(path: PathBuf) -> Self {
        Self(sled::open(path).expect("failed to open database"))
    }

    fn put(&self, slug: Option<String>, uri: Uri, len: usize) -> String {
        let slug = match slug {
            Some(slug) if is_clean(&slug) => slug,
            _ => {
                // Generate slugs until we find one that is not occupied.
                // Not the most efficient if occupancy is high but oh well.
                let mut slug = generate_slug(len);
                while self.get(&slug).is_some() {
                    slug = generate_slug(len);
                }
                slug
            }
        };

        println!("PUT {slug}");

        self.0
            .insert(&slug, uri.to_string().as_bytes())
            .expect("failed to write entry");

        slug
    }

    fn get(&self, slug: &str) -> Option<Uri> {
        self.0
            .get(&slug)
            .expect("failed to read database")
            .map(|bytes| String::from_utf8(bytes.to_vec()).expect("failed to deserialize value"))
            .map(|string| {
                string
                    .parse()
                    .expect("failed to deserialize URI from database")
            })
    }
}

#[derive(Parser)]
pub struct Config {
    /// Full domain used in links, excluding protocol and trailing slash
    #[arg(short, long, env)]
    domain: String,

    /// Authorization token used to restrict access
    #[arg(short, long, env)]
    token: String,

    /// Length of the slugs to generate
    #[arg(short, long, env, default_value = "5")]
    length: usize,

    /// Path where the database will be stored
    #[arg(long, default_value = "/var/lib/lnk/links.db", env)]
    db_path: PathBuf,
}

pub async fn run(config: Config) -> Result<(), Box<dyn std::error::Error>> {
    let state = AppState {
        db: Arc::new(Database::new(config.db_path)),
        domain: config.domain,
        token: config.token,
        len: config.length,
    };

    let app = Router::new()
        .route("/", get(root).post(create))
        .route("/:slug", get(redirect))
        .route("/info/:slug", get(get_info))
        .route("/:slug/qr", get(get_info))
        .route("/styles.css", get(stylesheet))
        .with_state(state);

    Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

async fn stylesheet() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/css".parse().unwrap());
    (headers, include_str!("./html/styles.css"))
}

async fn root() -> Html<&'static str> {
    Html(HTML_ROOT)
}

async fn redirect(Path(slug): Path<String>, State(state): State<AppState>) -> Response {
    match state.db.get(&slug) {
        Some(uri) => Redirect::temporary(&uri.to_string()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn create(
    State(state): State<AppState>,
    Form(data): Form<Submission>,
) -> Result<Redirect, String> {
    if data.token != state.token {
        return Err("Unauthorized".into());
    }

    let uri: Uri = data.uri.parse().map_err(|e: InvalidUri| e.to_string())?;
    // TODO Verify URI, make sure it has a TLD, add scheme if necessary

    let slug = state
        .db
        .put(data.slug.filter(|s| !s.is_empty()), uri, state.len);

    Ok(Redirect::to(&format!("/info/{slug}")))
}

async fn get_info(Path(slug): Path<String>, State(state): State<AppState>) -> Response {
    match state.db.get(&slug) {
        Some(uri) => Html(generate_info(slug, uri, &state)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn generate_slug(len: usize) -> String {
    let mut rng = rand::thread_rng();
    let one_char = || CHARSET[rng.gen_range(0..CHARSET.len())] as char;
    repeat_with(one_char).take(len).collect()
}

fn generate_info(slug: String, uri: Uri, state: &AppState) -> String {
    let qr = QRBuilder::new(uri.to_string())
        .ecl(ECL::M)
        .build()
        .expect("failed to build QR code");

    let svg = SvgBuilder::default()
        .margin(0)
        .shape(Shape::RoundedSquare)
        .to_str(&qr);

    include_str!("./html/info.html")
        .replace("{{DOMAIN}}", &state.domain)
        .replace("{{LINK}}", &format!("{}/{slug}", state.domain))
        .replace("{{SVG}}", &svg)
}
