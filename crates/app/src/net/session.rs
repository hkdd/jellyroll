//! The Jellyfin session on the network thread: find a server, connect,
//! sign in (Quick Connect), load the home screen (cached copy first, then
//! fresh: stale-while-revalidate, spec 9), refresh it periodically, and
//! fetch images on demand.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jellyfin_api::dto::{AuthenticationResult, BaseItemDto};
use jellyfin_api::{Client, ClientInfo, Error, TlsOptions, discovery, endpoints as ep};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;

use super::store::{self, ImageCache, MetaCache, State};
use super::{Cmd, Event, ImageJob, ImageSource, NetConfig, Status, images};
use crate::home::model::{self, HomeData, Row, Shape};

/// How often the home screen is refreshed while idle.
const REFRESH_EVERY: Duration = Duration::from_secs(5 * 60);
const QUICK_CONNECT_POLL: Duration = Duration::from_secs(3);
const DISCOVERY_WAIT: Duration = Duration::from_secs(3);
/// Concurrent image downloads/decodes.
const IMAGE_CONCURRENCY: usize = 4;

#[derive(Clone)]
pub struct Out {
    pub tx: mpsc::Sender<Event>,
    pub wake: Arc<dyn Fn() + Send + Sync>,
}

impl Out {
    async fn send(&self, e: Event) {
        if self.tx.send(e).await.is_ok() {
            (self.wake)();
        }
    }

    async fn status(&self, s: Status) {
        self.send(Event::Status(s)).await;
    }

    async fn problem(&self, title: &str, detail: impl Into<String>) {
        let detail = detail.into();
        tracing::warn!("{title}: {detail}");
        self.status(Status::Problem { title: title.into(), detail }).await;
    }
}

#[derive(Debug)]
enum Ctl {
    Retry,
}

pub async fn run(cfg: NetConfig, out: Out, mut cmds: mpsc::Receiver<Cmd>) {
    let (ctl_tx, ctl_rx) = mpsc::channel(8);
    let (client_tx, client_rx) = watch::channel::<Option<Client>>(None);
    let queue = Arc::new(ImageQueue::default());

    let cap = cfg.image_cache_bytes;
    let images_dir = cfg.cache_dir.clone();
    let disk = tokio::task::spawn_blocking(move || {
        images_dir.map(|d| Arc::new(Mutex::new(ImageCache::open(d.join("images"), cap))))
    })
    .await
    .ok()
    .flatten();

    let images = tokio::spawn(image_worker(queue.clone(), client_rx, disk, out.clone()));
    let session = tokio::spawn(session(cfg, out, ctl_rx, client_tx));
    while let Some(c) = cmds.recv().await {
        match c {
            Cmd::Retry => {
                let _ = ctl_tx.try_send(Ctl::Retry);
            }
            Cmd::WantImages(jobs) => queue.replace(jobs),
        }
    }
    // The UI is gone.
    images.abort();
    session.abort();
}

/// Waits for a UI command, or `timeout` (None = forever).
async fn wait_ctl(ctl: &mut mpsc::Receiver<Ctl>, timeout: Option<Duration>) -> Option<Ctl> {
    match timeout {
        Some(t) => tokio::time::timeout(t, ctl.recv()).await.ok().flatten(),
        None => ctl.recv().await,
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    tokio::task::spawn_blocking(f).await.ok()
}

/// Retry delays while the server is unreachable.
struct Backoff(u32);

impl Backoff {
    fn next(&mut self) -> Duration {
        let s = [2, 4, 8, 15, 30, 60][(self.0 as usize).min(5)];
        self.0 += 1;
        Duration::from_secs(s)
    }
}

fn home_key(server_id: &str, user_id: &str) -> String {
    format!("home/{server_id}/{user_id}")
}

async fn save_state(state: &State, path: &Option<PathBuf>) {
    let Some(p) = path.clone() else { return };
    let s = state.clone();
    blocking(move || {
        if let Err(e) = s.save(&p) {
            tracing::warn!("{}: cannot save ({e})", p.display());
        }
    })
    .await;
}

async fn session(
    cfg: NetConfig,
    out: Out,
    mut ctl: mpsc::Receiver<Ctl>,
    client_tx: watch::Sender<Option<Client>>,
) {
    let state_path = cfg.data_dir.as_ref().map(|d| d.join("state.json"));
    let sp = state_path.clone();
    let mut state = blocking(move || State::load(sp.as_deref())).await.unwrap_or_default();
    save_state(&state, &state_path).await;
    let meta_dir = cfg.cache_dir.clone();
    let meta = blocking(move || {
        let p = meta_dir?.join("metadata.redb");
        MetaCache::open(&p).map_err(|e| tracing::warn!("metadata cache disabled: {e}")).ok()
    })
    .await
    .flatten()
    .map(Arc::new);

    let info = ClientInfo {
        client: "Jellyroll".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        device: store::device_name(),
        device_id: state.device_id.clone(),
    };
    let mut tls = TlsOptions::default();
    if let Some(ca) = &cfg.ca_file {
        match std::fs::read(ca) {
            Ok(pem) => tls.extra_root_pem = Some(pem),
            Err(e) => {
                tracing::warn!("server.ca_file {}: {e}; using system roots only", ca.display())
            }
        }
    }
    let mut backoff = Backoff(0);

    'outer: loop {
        // ---- 1. which server --------------------------------------------
        let url = if !cfg.server_url.trim().is_empty() {
            cfg.server_url.clone()
        } else if let Some((s, _)) = state.last() {
            s.url.clone()
        } else {
            out.status(Status::Searching).await;
            let found = blocking(|| discovery::discover(DISCOVERY_WAIT)).await;
            match found {
                Some(Ok(list)) if !list.is_empty() => list[0].address.clone(),
                other => {
                    if let Some(Err(e)) = other {
                        tracing::warn!("discovery: {e}");
                    }
                    out.problem(
                        "No Jellyfin server found",
                        "No server answered on this network. Check that it is running, or set its \
                         address in ~/.config/jellyroll/config.toml:  [server]  url = \"http://IP:8096\".",
                    )
                    .await;
                    wait_ctl(&mut ctl, None).await;
                    continue;
                }
            }
        };
        let mut client = match Client::new(&url, info.clone(), &tls) {
            Ok(c) => c,
            Err(e) => {
                out.problem("Can't use the server address", e.to_string()).await;
                wait_ctl(&mut ctl, None).await;
                continue;
            }
        };

        // ---- 2. cached home first (stale-while-revalidate) ---------------
        let saved = state.servers.iter().find(|s| s.url == client.base_url()).cloned();
        let mut showing_home = false;
        let display_name =
            saved.as_ref().map_or_else(|| client.base_url().to_string(), |s| s.name.clone());
        if let Some((srv, Some(user))) = state.last().filter(|(s, _)| s.url == client.base_url()) {
            client.set_token(Some(user.token.clone()));
            client_tx.send_replace(Some(client.clone()));
            let key = home_key(&srv.id, &user.id);
            if let Some(m) = meta.clone()
                && let Some(Some(bytes)) = blocking(move || m.get(&key)).await
                && let Ok(data) = serde_json::from_slice::<HomeData>(&bytes)
            {
                tracing::info!(rows = data.rows.len(), "showing cached home screen");
                out.send(Event::Home { data, fresh: false }).await;
                showing_home = true;
            }
        }
        if !showing_home {
            out.status(Status::Connecting { server: display_name.clone() }).await;
        }

        // ---- 3. connect --------------------------------------------------
        let sys = match client.send(&ep::public_info()).await {
            Ok(s) => s,
            Err(e) => {
                let detail = format!("{}: {e}", client.base_url());
                if showing_home {
                    tracing::warn!("offline: {detail}");
                    out.send(Event::Offline).await;
                } else {
                    out.problem(
                        "Can't reach the server",
                        format!("{detail}. Retrying automatically."),
                    )
                    .await;
                }
                wait_ctl(&mut ctl, Some(backoff.next())).await;
                continue;
            }
        };
        backoff = Backoff(0);
        let version = sys.version.clone().unwrap_or_default();
        let server_name = sys.server_name.clone().filter(|n| !n.is_empty()).unwrap_or(display_name);
        tracing::info!(server = server_name, version, url = client.base_url(), "connected");
        if !jellyfin_api::server_supported(&version) {
            let (a, b, c) = jellyfin_api::MIN_SERVER_VERSION;
            out.problem(
                "Server version not supported",
                format!(
                    "{server_name} runs Jellyfin {version}; jellyroll needs {a}.{b}.{c} or newer."
                ),
            )
            .await;
            wait_ctl(&mut ctl, None).await;
            continue;
        }
        let server_id =
            sys.id.clone().filter(|i| !i.is_empty()).unwrap_or_else(|| client.base_url().into());
        state.remember_server(&server_id, &server_name, client.base_url());
        save_state(&state, &state_path).await;

        // ---- 4. user -----------------------------------------------------
        let (user_id, user_name) = match state.last().and_then(|(_, u)| u.cloned()) {
            Some(u) => {
                client.set_token(Some(u.token.clone()));
                (u.id, u.name)
            }
            None => {
                client_tx.send_replace(None);
                let Some(auth) = quick_connect(&client, &server_name, &out, &mut ctl).await else {
                    continue 'outer;
                };
                let token = auth.access_token.clone().unwrap_or_default();
                let user = auth.user.clone().unwrap_or_default();
                let (id, name) = (user.id.unwrap_or_default(), user.name.unwrap_or_default());
                tracing::info!(user = name, "signed in with Quick Connect");
                state.remember_user(&server_id, &id, &name, &token);
                save_state(&state, &state_path).await;
                client.set_token(Some(token));
                (id, name)
            }
        };
        client_tx.send_replace(Some(client.clone()));

        // ---- 5. home, refreshed until something breaks -------------------
        loop {
            match fetch_home(&client, &user_id).await {
                Ok(rows) => {
                    let data = HomeData {
                        server_name: server_name.clone(),
                        user_name: user_name.clone(),
                        rows,
                    };
                    if let Some(m) = meta.clone()
                        && let Ok(bytes) = serde_json::to_vec(&data)
                    {
                        let key = home_key(&server_id, &user_id);
                        blocking(move || {
                            if let Err(e) = m.put(&key, &bytes) {
                                tracing::warn!("metadata cache: {e}");
                            }
                        })
                        .await;
                    }
                    out.send(Event::Home { data, fresh: true }).await;
                    showing_home = true;
                    wait_ctl(&mut ctl, Some(REFRESH_EVERY)).await;
                }
                Err(Error::Status(401)) => {
                    tracing::warn!("access token rejected; signing in again");
                    state.forget_token(&server_id, &user_id);
                    save_state(&state, &state_path).await;
                    client_tx.send_replace(None);
                    continue 'outer;
                }
                Err(e) => {
                    if showing_home {
                        out.send(Event::Offline).await;
                    } else {
                        out.problem("Can't load the home screen", e.to_string()).await;
                    }
                    if let Some(Ctl::Retry) = wait_ctl(&mut ctl, Some(backoff.next())).await {
                        backoff = Backoff(0);
                    }
                }
            }
        }
    }
}

/// Quick Connect (spec 8): show a code, the user approves it from another
/// signed-in device, then trade the secret for a token. None means "start
/// over" (the server went away or the user asked to retry).
async fn quick_connect(
    client: &Client,
    server: &str,
    out: &Out,
    ctl: &mut mpsc::Receiver<Ctl>,
) -> Option<AuthenticationResult> {
    loop {
        match client.send(&ep::quick_connect_enabled()).await {
            Ok(true) => {}
            Ok(false) => {
                out.problem(
                    "Quick Connect is turned off",
                    format!(
                        "Sign-in on this TV uses Quick Connect. Turn it on in {server}'s Dashboard \
                         (General, then Quick Connect), then press Select."
                    ),
                )
                .await;
                wait_ctl(ctl, None).await;
                continue;
            }
            Err(e) => {
                out.problem("Can't reach the server", e.to_string()).await;
                wait_ctl(ctl, Some(Duration::from_secs(5))).await;
                return None;
            }
        }
        let r = match client.send(&ep::quick_connect_initiate()).await {
            Ok(r) => r,
            Err(e) => {
                out.problem("Quick Connect failed", e.to_string()).await;
                wait_ctl(ctl, None).await;
                return None;
            }
        };
        let (Some(code), Some(secret)) = (r.code, r.secret) else {
            out.problem("Quick Connect failed", "The server's answer had no code.").await;
            wait_ctl(ctl, None).await;
            continue;
        };
        out.status(Status::QuickConnect { server: server.into(), code }).await;
        loop {
            if let Some(Ctl::Retry) = wait_ctl(ctl, Some(QUICK_CONNECT_POLL)).await {
                break; // new code
            }
            match client.send(&ep::quick_connect_state(&secret)).await {
                Ok(s) if s.authenticated == Some(true) => {
                    match client.send(&ep::authenticate_quick_connect(&secret)).await {
                        Ok(a) if a.access_token.as_deref().is_some_and(|t| !t.is_empty()) => {
                            return Some(a);
                        }
                        Ok(_) => {
                            out.problem("Sign-in failed", "The server returned no access token.")
                                .await
                        }
                        Err(e) => out.problem("Sign-in failed", e.to_string()).await,
                    }
                    wait_ctl(ctl, None).await;
                    break;
                }
                Ok(_) => {}
                // The code expired or the server restarted: get a new one.
                Err(Error::Status(401 | 404)) => break,
                Err(e) => tracing::debug!("quick connect poll: {e}"),
            }
        }
    }
}

/// Fetches every home row (spec 8). Rows that fail are left out (logged);
/// the libraries list and auth failures fail the whole screen.
async fn fetch_home(client: &Client, user: &str) -> Result<Vec<Row>, Error> {
    let reqs = (
        ep::user_views(user),
        ep::resume(user, 12),
        ep::next_up(user, 16),
        ep::favorites(user, 24),
    );
    let (views, resume, next_up, favorites) = tokio::join!(
        client.send(&reqs.0),
        client.send(&reqs.1),
        client.send(&reqs.2),
        client.send(&reqs.3),
    );
    let views = views?.items.unwrap_or_default();

    let mut latest = JoinSet::new();
    for (i, v) in views.iter().enumerate() {
        if let Some(id) = v.id.clone().filter(|_| model::has_latest_row(v.collection_type)) {
            let (c, u) = (client.clone(), user.to_string());
            latest.spawn(async move { (i, c.send(&ep::latest(&u, &id, 16)).await) });
        }
    }
    let mut latest_items: Vec<Option<Vec<BaseItemDto>>> = vec![None; views.len()];
    while let Some(Ok((i, r))) = latest.join_next().await {
        latest_items[i] = keep(r)?;
    }

    let mut rows = Vec::new();
    rows.extend(model::row("views", "My Media", Shape::Landscape, &views));
    let items = |r: jellyfin_api::dto::BaseItemDtoQueryResult| r.items.unwrap_or_default();
    if let Some(r) = keep(resume)? {
        rows.extend(model::row("resume", "Continue Watching", Shape::Landscape, &items(r)));
    }
    if let Some(r) = keep(next_up)? {
        rows.extend(model::row("nextup", "Next Up", Shape::Landscape, &items(r)));
    }
    for (v, items) in views.iter().zip(latest_items) {
        if let (Some(id), Some(items)) = (v.id.as_ref(), items) {
            let name = v.name.as_deref().unwrap_or("Library");
            let shape = model::library_shape(v.collection_type);
            rows.extend(model::row(
                format!("latest:{id}"),
                format!("Latest {name}"),
                shape,
                &items,
            ));
        }
    }
    if let Some(r) = keep(favorites)? {
        rows.extend(model::row("favorites", "Favorites", Shape::Poster, &items(r)));
    }
    Ok(rows)
}

/// A row's result: auth failures propagate, other errors drop the row.
fn keep<T>(r: Result<T, Error>) -> Result<Option<T>, Error> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(Error::Status(401)) => Err(Error::Status(401)),
        Err(e) => {
            tracing::warn!("home row skipped: {e}");
            Ok(None)
        }
    }
}

// ---- images -------------------------------------------------------------------

#[derive(Default)]
struct ImageQueue {
    jobs: Mutex<VecDeque<ImageJob>>,
    notify: Notify,
}

impl ImageQueue {
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<ImageJob>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn replace(&self, jobs: Vec<ImageJob>) {
        *self.lock() = jobs.into();
        self.notify.notify_one();
    }

    /// Next job not already in flight that can run now (server images
    /// need a client).
    fn pop(&self, in_flight: &HashSet<u64>, have_client: bool) -> Option<ImageJob> {
        let mut q = self.lock();
        let i = q.iter().position(|j| {
            !in_flight.contains(&j.key)
                && (have_client || matches!(j.source, ImageSource::BlurHash(_)))
        })?;
        q.remove(i)
    }

    fn done(&self, key: u64) {
        self.lock().retain(|j| j.key != key);
    }
}

async fn image_worker(
    queue: Arc<ImageQueue>,
    mut client: watch::Receiver<Option<Client>>,
    disk: Option<Arc<Mutex<ImageCache>>>,
    out: Out,
) {
    let mut running: JoinSet<u64> = JoinSet::new();
    let mut in_flight: HashSet<u64> = HashSet::new();
    loop {
        while running.len() < IMAGE_CONCURRENCY {
            let c = client.borrow().clone();
            let Some(job) = queue.pop(&in_flight, c.is_some()) else { break };
            in_flight.insert(job.key);
            running.spawn(fetch_image(job, c, disk.clone(), out.clone()));
        }
        tokio::select! {
            Some(done) = running.join_next(), if !running.is_empty() => {
                if let Ok(key) = done {
                    in_flight.remove(&key);
                    queue.done(key);
                }
            }
            _ = queue.notify.notified() => {}
            changed = client.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

async fn fetch_image(
    job: ImageJob,
    client: Option<Client>,
    disk: Option<Arc<Mutex<ImageCache>>>,
    out: Out,
) -> u64 {
    let ImageJob { key, source, w, h, finish } = job;
    let result = match source {
        ImageSource::BlurHash(hash) => blocking(move || images::blurhash(&hash, w, h))
            .await
            .unwrap_or_else(|| Err("worker failed".into())),
        ImageSource::Server(path) => match client {
            None => Err("not connected".into()),
            Some(c) => {
                let disk_key = store::fnv1a(&[c.base_url().as_bytes(), path.as_bytes()]);
                let cached = match disk.clone() {
                    Some(d) => blocking(move || d.lock().ok()?.get(disk_key)).await.flatten(),
                    None => None,
                };
                let bytes = match cached {
                    Some(b) => Ok(b),
                    None => match c.get_bytes(&path).await {
                        Ok(b) => {
                            if let Some(d) = disk {
                                let b2 = b.clone();
                                tokio::task::spawn_blocking(move || {
                                    if let Ok(mut d) = d.lock() {
                                        d.put(disk_key, &b2);
                                    }
                                });
                            }
                            Ok(b)
                        }
                        Err(e) => Err(e.to_string()),
                    },
                };
                match bytes {
                    Ok(b) => blocking(move || images::decode(&b, w, h, finish))
                        .await
                        .unwrap_or_else(|| Err("worker failed".into())),
                    Err(e) => Err(e),
                }
            }
        },
    };
    match result {
        Ok(d) => out.send(Event::Image { key, w: d.w, h: d.h, rgba: d.rgba }).await,
        Err(e) => {
            tracing::debug!(key, "image failed: {e}");
            out.send(Event::ImageFailed { key }).await;
        }
    }
    key
}
