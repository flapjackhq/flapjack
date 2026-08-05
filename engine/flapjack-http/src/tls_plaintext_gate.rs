use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use hyper::body::{Body, Bytes, Incoming};
use hyper::{Method, Request, Response, StatusCode};
use tower::{Service, ServiceExt};

use super::BoxError;

pub(super) fn plaintext_challenge_gate<S>(service: S) -> PlaintextChallengeGate<S> {
    PlaintextChallengeGate { service }
}

#[derive(Clone)]
pub(super) struct PlaintextChallengeGate<S> {
    service: S,
}

impl<S, B> Service<Request<Incoming>> for PlaintextChallengeGate<S>
where
    S: Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    B: Body<Data = Bytes> + From<&'static str> + Send + 'static,
    B::Error: Into<BoxError>,
{
    type Response = Response<B>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(context)
    }

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        let mut service = self.service.clone();
        Box::pin(async move {
            if is_plaintext_acme_request(&request) {
                service.ready().await?.call(request).await
            } else {
                Ok(plaintext_rejection_response())
            }
        })
    }
}

fn is_plaintext_acme_request(request: &Request<Incoming>) -> bool {
    request.method() == Method::GET
        && request
            .uri()
            .path()
            .strip_prefix("/.well-known/acme-challenge/")
            .is_some_and(|token| !token.is_empty() && !token.contains('/'))
}

fn plaintext_rejection_response<B>() -> Response<B>
where
    B: Body<Data = Bytes> + From<&'static str>,
{
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(B::from("Not Found"))
        .expect("plaintext rejection response should be valid")
}
