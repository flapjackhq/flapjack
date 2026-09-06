//! TLS serving support for static certificate/key files.

use std::fmt;
use std::future::Future;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use hyper::body::{Body, Bytes, Incoming};
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use hyper_util::service::TowerToHyperService;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::ServerConfig;
use rustls_pki_types::pem::{Error as PemError, PemObject};
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tower::Service;

use axum::Extension;
use tower::Layer;

use crate::middleware::NativeTlsTransport;
use crate::startup::TlsPaths;
use tls_material_source::{
    configured_pair_is_within_material_dir, configured_pair_resolves_to_generation,
    FULLCHAIN_FILE_NAME, PRIVATE_KEY_FILE_NAME,
};
use tls_plaintext_gate::plaintext_challenge_gate;

#[path = "tls_material_source.rs"]
mod tls_material_source;
#[path = "tls_plaintext_gate.rs"]
mod tls_plaintext_gate;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type TlsStream = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;

const ACCEPT_ERROR_RETRY_DELAY: Duration = Duration::from_secs(1);
pub(crate) const TLS_MATERIAL_OBSERVER_INTERVAL: Duration = Duration::from_secs(5);
const TLS_RECORD_HANDSHAKE: u8 = 0x16;
const CURRENT_LINK_NAME: &str = "current";

pub(crate) trait TcpAccept {
    fn accept(
        &self,
    ) -> impl Future<Output = std::io::Result<(tokio::net::TcpStream, SocketAddr)>> + Send;
}

impl TcpAccept for TcpListener {
    fn accept(
        &self,
    ) -> impl Future<Output = std::io::Result<(tokio::net::TcpStream, SocketAddr)>> + Send {
        TcpListener::accept(self)
    }
}

struct HandshakedConnection {
    stream: TlsStream,
    peer_addr: SocketAddr,
}

struct PlaintextConnection {
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
}

enum ClassifiedConnection {
    Tls(Box<HandshakedConnection>),
    Plaintext(PlaintextConnection),
}

#[derive(Clone)]
pub(crate) struct LoadedTls {
    pub(crate) config: Arc<ServerConfig>,
    pub(crate) resolver: Arc<ReloadableTlsResolver>,
}

impl fmt::Debug for LoadedTls {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("LoadedTls").finish()
    }
}

pub(crate) fn load_tls_config(paths: &TlsPaths) -> Result<LoadedTls, String> {
    let certified_key = load_certified_key(paths)?;
    let resolver = Arc::new(ReloadableTlsResolver::new(certified_key, paths.clone()));
    // One resolver/config pair owns the current certified key.
    let config = Arc::new(tls_config_with_resolver(resolver.clone()));
    Ok(LoadedTls { config, resolver })
}

fn load_certified_key(paths: &TlsPaths) -> Result<Arc<CertifiedKey>, String> {
    flapjack_ssl::install_default_crypto_provider();
    let certs = load_certificates(&paths.cert_path)?;
    let key = load_private_key(&paths.key_path)?;
    CertifiedKey::from_der(
        certs,
        key,
        rustls::crypto::CryptoProvider::get_default().unwrap(),
    )
    .map(Arc::new)
    .map_err(|error| format!("failed to build TLS server config: {error}"))
}

fn tls_config_with_resolver(resolver: Arc<dyn ResolvesServerCert>) -> ServerConfig {
    ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver)
}

pub(crate) struct ReloadableTlsResolver {
    current: RwLock<Arc<CertifiedKey>>,
    startup_paths: TlsPaths,
}

impl ReloadableTlsResolver {
    fn new(initial_key: Arc<CertifiedKey>, startup_paths: TlsPaths) -> Self {
        Self {
            current: RwLock::new(initial_key),
            startup_paths,
        }
    }

    fn current_key(&self) -> Arc<CertifiedKey> {
        Arc::clone(
            &self
                .current
                .read()
                .expect("TLS resolver key lock should not be poisoned"),
        )
    }

    #[cfg(test)]
    pub(crate) fn publish_from_paths(&self, paths: &TlsPaths) -> Result<(), String> {
        let validated_key = load_certified_key(paths)?;
        self.publish_validated_key(validated_key);
        Ok(())
    }

    fn publish_validated_key(&self, validated_key: Arc<CertifiedKey>) {
        // Validation completes before the swap, so failures retain the serving key.
        *self
            .current
            .write()
            .expect("TLS resolver key lock should not be poisoned") = validated_key;
    }

    pub(crate) fn validate_material_observer_source(
        &self,
        material_dir: &Path,
    ) -> Result<(), String> {
        if configured_pair_is_within_material_dir(&self.startup_paths, material_dir)? {
            return Ok(());
        }
        let Some(generation) = resolve_material_generation(material_dir)? else {
            return Err(format!(
                "configured TLS certificate pair is outside managed material directory {}",
                material_dir.display()
            ));
        };
        if configured_pair_resolves_to_generation(&self.startup_paths, &generation)? {
            Ok(())
        } else {
            Err(format!(
                "configured TLS certificate pair does not match managed material generation {}",
                generation.display()
            ))
        }
    }
}

impl fmt::Debug for ReloadableTlsResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ReloadableTlsResolver").finish()
    }
}

impl ResolvesServerCert for ReloadableTlsResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current_key())
    }
}

fn open_pem_file(path: &Path, kind: &str) -> Result<BufReader<std::fs::File>, String> {
    std::fs::File::open(path)
        .map(BufReader::new)
        .map_err(|error| {
            let reason = match error.kind() {
                std::io::ErrorKind::NotFound => "file not found".to_string(),
                std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
                _ => error.to_string(),
            };
            format!(
                "failed to read TLS {kind} file {}: {reason}",
                path.display()
            )
        })
}

fn load_certificates(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let mut reader = open_pem_file(path, "certificate")?;
    let certs = CertificateDer::pem_reader_iter(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| {
            format!(
                "failed to parse TLS certificate file {}: invalid PEM data",
                path.display()
            )
        })?;
    if certs.is_empty() {
        return Err(format!(
            "TLS certificate file {} did not contain any certificates",
            path.display()
        ));
    }
    Ok(certs)
}

fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>, String> {
    let mut reader = open_pem_file(path, "private key")?;
    PrivateKeyDer::from_pem_reader(&mut reader).map_err(|error| match error {
        PemError::NoItemsFound => {
            format!(
                "TLS private key file {} did not contain a private key",
                path.display()
            )
        }
        _ => {
            format!(
                "failed to parse TLS private key file {}: invalid PEM data",
                path.display()
            )
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TlsMaterialObservation {
    Absent,
    Unchanged(PathBuf),
    Published(PathBuf),
    Rejected { generation: PathBuf, error: String },
}

pub(crate) fn observe_tls_material_once(
    material_dir: &Path,
    resolver: &ReloadableTlsResolver,
    last_successful_generation: &mut Option<PathBuf>,
) -> TlsMaterialObservation {
    let generation = match resolve_material_generation(material_dir) {
        Ok(Some(generation)) => generation,
        Ok(None) => return TlsMaterialObservation::Absent,
        Err(error) => {
            return TlsMaterialObservation::Rejected {
                generation: material_dir.to_path_buf(),
                error,
            }
        }
    };
    let paths = generation_tls_paths(&generation);
    if last_successful_generation.as_ref() == Some(&generation) && generation != material_dir {
        return TlsMaterialObservation::Unchanged(generation);
    }

    // The cert and key are both loaded below the same resolved generation, so a
    // symlink flip cannot mix two independently resolved publications.
    let validated_key = match load_certified_key(&paths) {
        Ok(validated_key) => validated_key,
        Err(error) => return TlsMaterialObservation::Rejected { generation, error },
    };
    if last_successful_generation.as_ref() == Some(&generation)
        && validated_key.cert == resolver.current_key().cert
    {
        return TlsMaterialObservation::Unchanged(generation);
    }
    resolver.publish_validated_key(validated_key);
    *last_successful_generation = Some(generation.clone());
    TlsMaterialObservation::Published(generation)
}

pub(crate) async fn run_tls_material_observer<Expiry, ExpiryFuture>(
    resolver: Arc<ReloadableTlsResolver>,
    material_dir: PathBuf,
    mut expiry_days: Expiry,
    mut observation_completed: impl FnMut(&TlsMaterialObservation),
) where
    Expiry: FnMut() -> ExpiryFuture,
    ExpiryFuture: Future<Output = Option<i64>>,
{
    let Some(mut last_successful_generation) =
        initialize_tls_material_observer(Arc::clone(&resolver), material_dir.clone()).await
    else {
        return;
    };
    let mut interval = tokio::time::interval(TLS_MATERIAL_OBSERVER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Some((observation, successful_generation)) = observe_tls_material_in_background(
            Arc::clone(&resolver),
            material_dir.clone(),
            last_successful_generation.clone(),
        )
        .await
        else {
            continue;
        };
        last_successful_generation = successful_generation;
        report_tls_material_observation(&observation, &mut expiry_days).await;
        observation_completed(&observation);
    }
}

async fn initialize_tls_material_observer(
    resolver: Arc<ReloadableTlsResolver>,
    material_dir: PathBuf,
) -> Option<Option<PathBuf>> {
    let initial_generation = tokio::task::spawn_blocking({
        let resolver = Arc::clone(&resolver);
        let material_dir = material_dir.clone();
        move || {
            resolver
                .validate_material_observer_source(&material_dir)
                .map(|()| initial_successful_tls_material_generation(&material_dir, &resolver))
        }
    })
    .await
    .map_err(|error| format!("observer task failed: {error}"))
    .and_then(|result| result);
    match initial_generation {
        Ok(generation) => Some(generation),
        Err(error) => {
            tracing::warn!(
                material_dir = %material_dir.display(),
                "[TLS] Certificate material observer disabled: {error}"
            );
            None
        }
    }
}

async fn observe_tls_material_in_background(
    resolver: Arc<ReloadableTlsResolver>,
    material_dir: PathBuf,
    mut successful_generation: Option<PathBuf>,
) -> Option<(TlsMaterialObservation, Option<PathBuf>)> {
    match tokio::task::spawn_blocking(move || {
        let observation =
            observe_tls_material_once(&material_dir, &resolver, &mut successful_generation);
        (observation, successful_generation)
    })
    .await
    {
        Ok(result) => Some(result),
        Err(error) => {
            tracing::warn!("[TLS] Certificate material observer task failed: {error}");
            None
        }
    }
}

async fn report_tls_material_observation<Expiry, ExpiryFuture>(
    observation: &TlsMaterialObservation,
    expiry_days: &mut Expiry,
) where
    Expiry: FnMut() -> ExpiryFuture,
    ExpiryFuture: Future<Output = Option<i64>>,
{
    match observation {
        TlsMaterialObservation::Published(generation) => {
            let cert_expires_in_days = expiry_days().await;
            tracing::info!(
                generation = %generation.display(),
                cert_expires_in_days = ?cert_expires_in_days,
                "[TLS] Published renewed certificate material"
            );
        }
        TlsMaterialObservation::Rejected { generation, error } => {
            tracing::warn!(
                generation = %generation.display(),
                "[TLS] Certificate material reload rejected; retaining previous key: {error}"
            );
        }
        TlsMaterialObservation::Absent | TlsMaterialObservation::Unchanged(_) => {}
    }
}

fn initial_successful_tls_material_generation(
    material_dir: &Path,
    resolver: &ReloadableTlsResolver,
) -> Option<PathBuf> {
    let generation = match resolve_material_generation(material_dir) {
        Ok(Some(generation)) => generation,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                material_dir = %material_dir.display(),
                "[TLS] Could not resolve startup certificate material generation: {error}"
            );
            return None;
        }
    };
    let generation_key = match load_certified_key(&generation_tls_paths(&generation)) {
        Ok(certified_key) => certified_key,
        Err(error) => {
            tracing::warn!(
                generation = %generation.display(),
                "[TLS] Startup certificate material generation is not currently publishable: {error}"
            );
            return None;
        }
    };
    if generation_key.cert == resolver.current_key().cert {
        Some(generation)
    } else {
        None
    }
}

fn generation_tls_paths(generation: &Path) -> TlsPaths {
    TlsPaths {
        cert_path: generation.join(FULLCHAIN_FILE_NAME),
        key_path: generation.join(PRIVATE_KEY_FILE_NAME),
    }
}

fn resolve_material_generation(material_dir: &Path) -> Result<Option<PathBuf>, String> {
    let metadata = match std::fs::symlink_metadata(material_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to inspect TLS material directory {}: {error}",
                material_dir.display()
            ))
        }
    };
    if metadata.file_type().is_symlink() {
        return resolve_symlink_target(material_dir).map(Some);
    }
    if metadata.is_dir() {
        let current = material_dir.join(CURRENT_LINK_NAME);
        return match std::fs::symlink_metadata(&current) {
            Ok(current_metadata) if current_metadata.file_type().is_symlink() => {
                resolve_symlink_target(&current).map(Some)
            }
            Ok(_) => Err(format!(
                "TLS material current path is not a symlink: {}",
                current.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if direct_material_files_started(material_dir)? {
                    Ok(Some(material_dir.to_path_buf()))
                } else {
                    Ok(None)
                }
            }
            Err(error) => Err(format!(
                "failed to inspect TLS material current symlink {}: {error}",
                current.display()
            )),
        };
    }
    Err(format!(
        "TLS material path is neither a directory nor a symlink: {}",
        material_dir.display()
    ))
}

fn direct_material_files_started(material_dir: &Path) -> Result<bool, String> {
    let cert_path = material_dir.join(FULLCHAIN_FILE_NAME);
    let key_path = material_dir.join(PRIVATE_KEY_FILE_NAME);
    let cert_exists = cert_path.try_exists().map_err(|error| {
        format!(
            "failed to inspect TLS material certificate file {}: {error}",
            cert_path.display()
        )
    })?;
    let key_exists = key_path.try_exists().map_err(|error| {
        format!(
            "failed to inspect TLS material private key file {}: {error}",
            key_path.display()
        )
    })?;
    Ok(cert_exists || key_exists)
}

fn resolve_symlink_target(link_path: &Path) -> Result<PathBuf, String> {
    let target = std::fs::read_link(link_path).map_err(|error| {
        format!(
            "failed to read TLS material symlink {}: {error}",
            link_path.display()
        )
    })?;
    Ok(if target.is_absolute() {
        target
    } else {
        link_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(target)
    })
}

pub(crate) async fn serve_tls<A, M, S, B, E, Shutdown>(
    listener: A,
    make_service: M,
    tls_config: Arc<ServerConfig>,
    shutdown: Shutdown,
) -> Result<(), BoxError>
where
    A: TcpAccept,
    M: Service<SocketAddr, Response = S, Error = E> + Clone + Send + 'static,
    M::Future: Send + 'static,
    E: std::fmt::Display + Send + Sync + 'static,
    S: Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + 'static,
    B: Body<Data = Bytes> + From<&'static str> + Send + 'static,
    B::Error: Into<BoxError>,
    Shutdown: Future<Output = ()> + Send + 'static,
{
    let graceful_connection_drain_timeout =
        Duration::from_secs(crate::startup::shutdown_timeout_secs_from_env());
    let acceptor = TlsAcceptor::from(tls_config);
    let graceful = GracefulShutdown::new();
    let (handshake_shutdown_tx, _) = watch::channel(false);
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    // Owned by the loop, not by the accept future: if a completed handshake
    // drops the in-flight accept branch, the deadline survives and the next
    // iteration still waits out the full resource-pressure backoff.
    let mut next_accept_not_before: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            accepted = accept_tcp(&listener, next_accept_not_before) => {
                match accepted {
                    Ok((stream, peer_addr)) => {
                        next_accept_not_before = None;
                        spawn_classified_connection(
                            &mut connections,
                            acceptor.clone(),
                            stream,
                            peer_addr,
                            handshake_shutdown_tx.subscribe(),
                        );
                    }
                    Err(error) => {
                        next_accept_not_before = accept_error_retry_deadline(&error);
                    }
                }
            }
            classified = connections.join_next(), if !connections.is_empty() => {
                match completed_classified_connection(classified) {
                    Some(ClassifiedConnection::Tls(connection)) => {
                        spawn_tls_http_connection(
                            *connection,
                            make_service.clone(),
                            graceful.watcher(),
                        );
                    }
                    Some(ClassifiedConnection::Plaintext(connection)) => {
                        spawn_plaintext_http_connection(
                            connection,
                            make_service.clone(),
                            graceful.watcher(),
                        );
                    }
                    None => {}
                }
            }
            _ = shutdown.as_mut() => {
                break;
            }
        }
    }

    drop(listener);
    let _ = handshake_shutdown_tx.send(true);
    while let Some(classified) = connections.join_next().await {
        match completed_classified_connection(Some(classified)) {
            Some(ClassifiedConnection::Tls(connection)) => {
                spawn_tls_http_connection(*connection, make_service.clone(), graceful.watcher());
            }
            Some(ClassifiedConnection::Plaintext(connection)) => {
                spawn_plaintext_http_connection(
                    connection,
                    make_service.clone(),
                    graceful.watcher(),
                );
            }
            None => {}
        }
    }
    await_graceful_connection_drain(graceful.shutdown(), graceful_connection_drain_timeout).await?;
    Ok(())
}

async fn await_graceful_connection_drain<Drain>(
    drain: Drain,
    timeout: Duration,
) -> std::io::Result<()>
where
    Drain: Future<Output = ()>,
{
    tokio::time::timeout(timeout, drain).await.map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "TLS graceful connection drain timed out after {}ms",
                timeout.as_millis()
            ),
        )
    })
}

async fn accept_tcp<A: TcpAccept>(
    listener: &A,
    not_before: Option<tokio::time::Instant>,
) -> std::io::Result<(tokio::net::TcpStream, SocketAddr)> {
    if let Some(deadline) = not_before {
        tokio::time::sleep_until(deadline).await;
    }
    listener.accept().await
}

fn accept_error_retry_deadline(error: &std::io::Error) -> Option<tokio::time::Instant> {
    let delay = accept_error_retry_delay(error)?;
    tracing::error!("TCP accept error: {error}");
    Some(tokio::time::Instant::now() + delay)
}

fn accept_error_retry_delay(error: &std::io::Error) -> Option<Duration> {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset => None,
        _ => Some(ACCEPT_ERROR_RETRY_DELAY),
    }
}

fn spawn_classified_connection(
    connections: &mut JoinSet<Option<ClassifiedConnection>>,
    acceptor: TlsAcceptor,
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    mut shutdown: watch::Receiver<bool>,
) {
    connections.spawn(async move {
        tokio::select! {
            classified = classify_connection(acceptor, stream, peer_addr) => classified,
            _ = shutdown.changed() => {
                None
            }
        }
    });
}

async fn classify_connection(
    acceptor: TlsAcceptor,
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
) -> Option<ClassifiedConnection> {
    let mut first_byte = [0_u8; 1];
    match stream.peek(&mut first_byte).await {
        Ok(0) => None,
        Ok(_) if first_byte[0] == TLS_RECORD_HANDSHAKE => match acceptor.accept(stream).await {
            Ok(stream) => Some(ClassifiedConnection::Tls(Box::new(HandshakedConnection {
                stream,
                peer_addr,
            }))),
            Err(error) => {
                tracing::warn!(peer_addr = %peer_addr, "TLS handshake failed: {error}");
                None
            }
        },
        Ok(_) => Some(ClassifiedConnection::Plaintext(PlaintextConnection {
            stream,
            peer_addr,
        })),
        Err(error) => {
            tracing::warn!(peer_addr = %peer_addr, "Connection classification failed: {error}");
            None
        }
    }
}

fn completed_classified_connection(
    classified: Option<Result<Option<ClassifiedConnection>, tokio::task::JoinError>>,
) -> Option<ClassifiedConnection> {
    match classified {
        Some(Ok(connection)) => connection,
        Some(Err(error)) => {
            tracing::warn!("TLS connection classification task failed: {error}");
            None
        }
        None => None,
    }
}

fn spawn_tls_http_connection<M, S, B, E>(
    connection: HandshakedConnection,
    make_service: M,
    watcher: hyper_util::server::graceful::Watcher,
) where
    M: Service<SocketAddr, Response = S, Error = E> + Send + 'static,
    M::Future: Send + 'static,
    E: std::fmt::Display + Send + Sync + 'static,
    S: Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + 'static,
    B: Body<Data = Bytes> + From<&'static str> + Send + 'static,
    B::Error: Into<BoxError>,
{
    tokio::spawn(async move {
        serve_tls_connection(connection, make_service, watcher).await;
    });
}

async fn serve_tls_connection<M, S, B, E>(
    connection: HandshakedConnection,
    mut make_service: M,
    watcher: hyper_util::server::graceful::Watcher,
) where
    M: Service<SocketAddr, Response = S, Error = E> + Send + 'static,
    M::Future: Send + 'static,
    E: std::fmt::Display + Send + Sync + 'static,
    S: Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + 'static,
    B: Body<Data = Bytes> + From<&'static str> + Send + 'static,
    B::Error: Into<BoxError>,
{
    let HandshakedConnection { stream, peer_addr } = connection;
    let service = match make_connection_service(&mut make_service, peer_addr).await {
        Ok(service) => service,
        Err(error) => {
            tracing::warn!(peer_addr = %peer_addr, "Failed to build connection service: {error}");
            return;
        }
    };
    let io = TokioIo::new(stream);
    // The marker is attached at the only native-TLS serving seam so downstream
    // HTTP code never has to infer transport security from unrelated state.
    let service = Extension(NativeTlsTransport).layer(service);
    let service = TowerToHyperService::new(service);
    let builder = Builder::new(TokioExecutor::new());
    let connection = builder.serve_connection_with_upgrades(io, service);
    if let Err(error) = watcher.watch(connection.into_owned()).await {
        tracing::warn!(peer_addr = %peer_addr, "TLS connection error: {error}");
    }
}

fn spawn_plaintext_http_connection<M, S, B, E>(
    connection: PlaintextConnection,
    make_service: M,
    watcher: hyper_util::server::graceful::Watcher,
) where
    M: Service<SocketAddr, Response = S, Error = E> + Send + 'static,
    M::Future: Send + 'static,
    E: std::fmt::Display + Send + Sync + 'static,
    S: Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + 'static,
    B: Body<Data = Bytes> + From<&'static str> + Send + 'static,
    B::Error: Into<BoxError>,
{
    tokio::spawn(async move {
        serve_plaintext_connection(connection, make_service, watcher).await;
    });
}

async fn serve_plaintext_connection<M, S, B, E>(
    connection: PlaintextConnection,
    mut make_service: M,
    watcher: hyper_util::server::graceful::Watcher,
) where
    M: Service<SocketAddr, Response = S, Error = E> + Send + 'static,
    M::Future: Send + 'static,
    E: std::fmt::Display + Send + Sync + 'static,
    S: Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<BoxError> + Send + 'static,
    B: Body<Data = Bytes> + From<&'static str> + Send + 'static,
    B::Error: Into<BoxError>,
{
    let PlaintextConnection { stream, peer_addr } = connection;
    let service = match make_connection_service(&mut make_service, peer_addr).await {
        Ok(service) => service,
        Err(error) => {
            tracing::warn!(peer_addr = %peer_addr, "Failed to build plaintext connection service: {error}");
            return;
        }
    };
    let service = plaintext_challenge_gate(service);
    let service = TowerToHyperService::new(service);
    let builder = Builder::new(TokioExecutor::new());
    let connection = builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
    if let Err(error) = watcher.watch(connection.into_owned()).await {
        tracing::warn!(peer_addr = %peer_addr, "Plaintext challenge connection error: {error}");
    }
}

async fn make_connection_service<M, S, E>(
    make_service: &mut M,
    peer_addr: SocketAddr,
) -> Result<S, E>
where
    M: Service<SocketAddr, Response = S, Error = E>,
{
    std::future::poll_fn(|cx| make_service.poll_ready(cx)).await?;
    make_service.call(peer_addr).await
}

#[cfg(test)]
#[path = "tls_serve_provider_tests.rs"]
mod tls_serve_provider_tests;
#[cfg(test)]
#[path = "tls_serve_rotation_tests.rs"]
mod tls_serve_rotation_tests;
#[cfg(all(test, unix))]
#[path = "tls_serve_source_tests.rs"]
mod tls_serve_source_tests;
#[cfg(test)]
#[path = "tls_serve_tests.rs"]
mod tls_serve_tests;

#[cfg(test)]
mod shutdown_deadline_tests {
    use super::*;

    #[tokio::test]
    async fn graceful_connection_drain_timeout_is_an_explicit_error() {
        let observed = tokio::time::timeout(
            Duration::from_millis(100),
            await_graceful_connection_drain(std::future::pending::<()>(), Duration::from_millis(1)),
        )
        .await;

        let error = observed
            .expect("the configured drain timeout must terminate the wait")
            .expect_err("a timed-out connection drain must fail the server stop");
        assert_eq!(
            error.to_string(),
            "TLS graceful connection drain timed out after 1ms"
        );
    }

    #[tokio::test]
    async fn graceful_connection_drain_preserves_successful_completion() {
        await_graceful_connection_drain(std::future::ready(()), Duration::from_millis(1))
            .await
            .unwrap();
    }
}
