use anyhow::Error;
use anyhow::Result;
use core::pin::Pin;

use futures::prelude::*;
use futures::Future;
use hyper::{http::StatusCode, Body, Method, Request, Response};
use hyper::{service::Service, HeaderMap};
use log::kv::ToValue;
use std::{
  collections::HashMap,
  fmt,
  task::{Context, Poll},
  time::SystemTime,
};
use tower_layer::{Identity, Layer, Stack};

pub struct LogLayer;

impl<S> Layer<S> for LogLayer {
  type Service = LogService<S>;

  fn layer(&self, service: S) -> Self::Service {
    LogService { service }
  }
}

struct HttpHeaders<'a>(&'a HeaderMap);

impl<'a> ToValue for HttpHeaders<'a> {
  fn to_value(&self) -> log::kv::Value {
    log::kv::value::Value::from_debug(self.0)
  }
}

// This service implements the Log behavior
pub struct LogService<S> {
  service: S,
}
impl<S> Service<Request<Body>> for LogService<S>
where
  S: Service<Request<Body>, Response = Response<Body>>,
  S::Future: 'static,
{
  type Response = S::Response;
  type Error = S::Error;
  #[allow(clippy::type_complexity)]
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

  fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    self.service.poll_ready(cx).map_err(Into::into)
  }

  fn call(&mut self, request: Request<Body>) -> Self::Future {
    let start = SystemTime::now();
    debug!("Received request", {
      method: request.method().as_str(),
      path: request.uri().path_and_query().unwrap().as_str(),
      headers: HttpHeaders(request.headers()),
      body: log::kv::value::Value::from_debug(request.body()),
    });

    Box::pin(self.service.call(request).map(move |return_value| {
      let took = SystemTime::now().duration_since(start).unwrap();
      return_value.map(|response| {
        info!(
          "Finished processing request", {
            took: log::kv::value::Value::from_debug(&took),
            status_code: response.status().as_u16(),
            headers: HttpHeaders(response.headers()),
            body: log::kv::value::Value::from_debug(response.body()),
          }
        );
        response
      })
    }))
  }
}

struct Svc;

impl Service<Request<Body>> for Svc {
  type Response = Response<Body>;

  type Error = Error;

  #[allow(clippy::type_complexity)]
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

  fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, _req: Request<Body>) -> Self::Future {
    Box::pin(async {
      Response::builder()
        .status(StatusCode::OK)
        .body(Body::from("Hello world"))
        .map_err(Into::into)
    })
  }
}

type Rtr<T> = HashMap<Method, HashMap<String, T>>;
struct RouterService<T>(Rtr<T>, T);

impl<T> Service<Request<Body>> for RouterService<T>
where
  T: Service<Request<Body>>,
{
  type Response = T::Response;
  type Error = T::Error;

  #[allow(clippy::type_complexity)]
  type Future = T::Future;

  fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, req: Request<Body>) -> Self::Future {
    let router = &mut self.0;
    let handler = match router.get_mut(req.method()) {
      Some(handlers) => handlers.get_mut(req.uri().path()).unwrap_or(&mut self.1),
      None => &mut self.1,
    };
    handler.call(req)
  }
}

pub struct ServiceBuilder<L> {
  layer: L,
}

impl Default for ServiceBuilder<Identity> {
  fn default() -> Self {
    Self::new()
  }
}

impl ServiceBuilder<Identity> {
  /// Create a new [`ServiceBuilder`].
  pub fn new() -> Self {
    ServiceBuilder { layer: Identity::new() }
  }
}

impl<L> ServiceBuilder<L> {
  /// Add a new layer `T` into the [`ServiceBuilder`].
  ///
  /// This wraps the inner service with the service provided by a user-defined
  /// [`Layer`]. The provided layer must implement the [`Layer`] trait.
  ///
  /// [`Layer`]: crate::Layer
  pub fn layer<T>(self, layer: T) -> ServiceBuilder<Stack<T, L>> {
    ServiceBuilder {
      layer: Stack::new(layer, self.layer),
    }
  }

  pub fn log_requests(self) -> ServiceBuilder<Stack<LogLayer, L>> {
    self.layer(LogLayer)
  }

  /// Wrap the service `S` with the middleware provided by this
  /// [`ServiceBuilder`]'s [`Layer`]'s, returning a new [`Service`].
  ///
  /// [`Layer`]: crate::Layer
  /// [`Service`]: crate::Service
  pub fn service<S>(&self, service: S) -> L::Service
  where
    L: Layer<S>,
  {
    self.layer.layer(service)
  }
}

impl<L: fmt::Debug> fmt::Debug for ServiceBuilder<L> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_tuple("ServiceBuilder").field(&self.layer).finish()
  }
}

#[cfg(test)]
mod tests {

  use std::collections::HashMap;

  use super::{RouterService, ServiceBuilder, Svc};
  use hyper::{header::CONTENT_TYPE, service::Service, Body};
  use hyper::{Method, Request};

  #[tokio::test]
  async fn test_service_abstraction() {
    std::env::set_var("RUST_LOG", "debug");
    femme::try_with_level(femme::LevelFilter::Debug).ok();
    // pretty_env_logger::env_logger::builder()
    //   .write_style(WriteStyle::Always)
    //   .is_test(true)
    //   .try_init()
    //   .ok();

    info!("building service");
    let mut svc = ServiceBuilder::new().service(RouterService(HashMap::new(), Svc));
    info!("running service");
    let req = Request::builder().method(Method::GET).body(Body::empty()).unwrap();

    let result = svc.call(req).await.unwrap();
    info!("verifying service: {:?}", result);
  }

  #[tokio::test]
  async fn test_service_with_middleware() {
    std::env::set_var("RUST_LOG", "debug");
    femme::try_with_level(femme::LevelFilter::Debug).ok();
    // pretty_env_logger::env_logger::builder()
    //   .write_style(WriteStyle::Always)
    //   .is_test(true)
    //   .try_init()
    //   .ok();

    info!("building service");
    let mut svc = ServiceBuilder::new()
      .log_requests()
      .service(RouterService(HashMap::new(), Svc));
    info!("running service");
    let req = Request::builder()
      .method(Method::GET)
      .uri("/the-thing?value=first")
      .header(CONTENT_TYPE, "application/json")
      .body(Body::empty())
      .unwrap();

    let result = svc.call(req).await.unwrap();
    info!("verifying service: {:?}", result);

    // assert_eq!(result, "boop hello".to_string());
  }
}
